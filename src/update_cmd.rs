//! The `upgrade` / `update` / `post-update` / `update-plugin` flow: binary
//! upgrade via subprocess re-exec, generated-plugin refresh, daemon service
//! refresh, the post-update health pass, and the full tracked-agent
//! reinstall that keeps config-managed integrations in sync.
//!
//! The post-update pass refreshes every already-configured agent integration
//! (re-running `install` + `post_install` for each tracked agent), so a
//! separate `tracedecay reinstall` is not needed after an upgrade. Pass
//! `--no-reinstall` to skip that agent-integration refresh.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tracedecay::upgrade::UpgradeOutcome;
use tracedecay::user_config::UserConfig;

// Exceeds the daemon's sequential 15s client drain, 2s task abort, and 45s
// server-shutdown bounds with margin for service-manager/process-exit latency.
const DAEMON_RESTART_LEASE_TIMEOUT: Duration = Duration::from_secs(90);

pub(crate) async fn refresh_generated_plugins() -> tracedecay::errors::Result<()> {
    let home = tracedecay_home_dir()?;
    let tracedecay_bin = tracedecay_bin_for_generated_artifacts()?;
    eprintln!(
        "Refreshing tracedecay-generated plugin artifacts (supported user configs are preserved)"
    );

    // Detection-driven, not `installed_agents`-driven: each integration
    // decides whether generated artifacts exist on this machine, so stale
    // tracking state can neither skip a real install nor install anywhere new.
    let mut refreshed_any = false;
    let mut failures: Vec<String> = Vec::new();
    for ag in tracedecay::agents::all_integrations() {
        let hermes_was_installed = ag.id() == "hermes" && ag.has_tracedecay(&home);
        // Generated-plugin refresh never rewrites Hermes profile config, so it
        // must not be blocked by an unresolved historical session migration.
        // Migration remains mandatory on install/uninstall paths that can
        // remove a legacy project pin.
        let ctx = tracedecay::agents::InstallContext {
            home: home.clone(),
            tracedecay_bin: tracedecay_bin.clone(),
            tool_permissions: tracedecay::agents::expected_tool_perms(),
            project_root: None,
            dashboard: true,
        };
        let outcome = match ag.update_plugin(&ctx) {
            Ok(tracedecay::agents::UpdatePluginOutcome::NotInstalled) if hermes_was_installed => {
                ag.install(&ctx).map(|()| {
                    tracedecay::agents::UpdatePluginOutcome::Refreshed(vec![
                        home.join(".hermes/plugins/tracedecay"),
                    ])
                })
            }
            outcome => outcome,
        };
        match outcome {
            Ok(tracedecay::agents::UpdatePluginOutcome::Refreshed(paths)) => {
                refreshed_any = true;
                for path in paths {
                    eprintln!(
                        "  \x1b[32m✔\x1b[0m {}: refreshed {}",
                        ag.id(),
                        path.display()
                    );
                }
            }
            Ok(tracedecay::agents::UpdatePluginOutcome::NotInstalled) => {}
            // Config-managed integrations (claude, copilot, …) are refreshed by
            // the tracked-agent reinstall in `run_post_update_tasks`, so there
            // is nothing to do — and nothing to nag about — here.
            Ok(tracedecay::agents::UpdatePluginOutcome::ConfigOnly) => {}
            Err(e) => failures.push(format!("{}: {e}", ag.id())),
        }
    }
    if !refreshed_any {
        eprintln!("No generated plugin installs detected — nothing to update.");
    }
    if !failures.is_empty() {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: format!("update-plugin failed for {}", failures.join("; ")),
        });
    }

    Ok(())
}

/// Rewrites and restarts the installed daemon service, returning the service
/// path and its socket, or `None` when no service is installed.
fn refresh_daemon_service(
    previous_state: tracedecay::daemon::DaemonServiceState,
) -> tracedecay::errors::Result<Option<(PathBuf, PathBuf)>> {
    if !cfg!(any(target_os = "linux", target_os = "macos")) {
        return Ok(None);
    }
    let tracedecay_bin = tracedecay_bin_on_path()?;
    let spec = tracedecay::daemon::service_spec(tracedecay_bin, None)?;
    let socket_path = tracedecay::daemon::installed_service_socket_path()?
        .unwrap_or_else(|| spec.socket_path.clone());
    Ok(
        tracedecay::daemon::refresh_installed_service_under_lease_with_state(
            &spec,
            previous_state,
        )?
        .map(|service_path| (service_path, socket_path)),
    )
}

fn refresh_daemon_service_after_update(
    previous_state: tracedecay::daemon::DaemonServiceState,
) -> tracedecay::errors::Result<()> {
    match refresh_daemon_service(previous_state)? {
        Some((service_path, socket_path)) => {
            eprintln!(
                "\x1b[32m✔\x1b[0m Daemon service refreshed at {}",
                service_path.display()
            );
            eprintln!("Daemon socket: {}", socket_path.display());
        }
        None if tracedecay::daemon::daemon_reachable() => {
            eprintln!(
                "  \x1b[33mwarning:\x1b[0m a TraceDecay daemon is running without an installed service; \
                 it keeps serving the previous version until its `tracedecay daemon run` process is restarted."
            );
        }
        None => {
            eprintln!("TraceDecay daemon service is not installed; skipping daemon restart.");
        }
    }
    Ok(())
}

fn restart_daemon_service_with<Lease, Quiesce, Acquire, Refresh, Restore>(
    quiesce: Quiesce,
    acquire: Acquire,
    refresh: Refresh,
    restore: Restore,
) -> tracedecay::errors::Result<Option<(PathBuf, PathBuf)>>
where
    Quiesce: FnOnce() -> tracedecay::errors::Result<tracedecay::daemon::DaemonServiceState>,
    Acquire: FnOnce() -> tracedecay::errors::Result<Lease>,
    Refresh: FnOnce(
        tracedecay::daemon::DaemonServiceState,
    ) -> tracedecay::errors::Result<Option<(PathBuf, PathBuf)>>,
    Restore: FnOnce(tracedecay::daemon::DaemonServiceState) -> tracedecay::errors::Result<()>,
{
    // The managed daemon holds a shared lifecycle lease while serving its
    // databases. Stop it first, then acquire exclusive ownership before the
    // service unit is rewritten and restarted.
    let previous_state = quiesce()?;
    let _lifecycle_lease = match acquire() {
        Ok(lease) => lease,
        Err(acquire_error) => {
            if matches!(
                previous_state,
                tracedecay::daemon::DaemonServiceState::RunningEnabled
                    | tracedecay::daemon::DaemonServiceState::RunningDisabled
            ) {
                if let Err(restore_error) = restore(previous_state) {
                    return Err(tracedecay::errors::TraceDecayError::Config {
                        message: format!(
                            "{acquire_error}; additionally failed to restore the managed daemon service: {restore_error}"
                        ),
                    });
                }
            }
            return Err(acquire_error);
        }
    };
    // `daemon restart` is an explicit request to bring the installed service
    // up, even when it was stopped before restart began.
    refresh(tracedecay::daemon::DaemonServiceState::RunningEnabled)
}

pub(crate) fn restart_daemon_service() -> tracedecay::errors::Result<()> {
    let restarted = restart_daemon_service_with(
        tracedecay::daemon::quiesce_installed_service_for_restart,
        || {
            tracedecay::lifecycle_lease::acquire_exclusive_with_timeout(
                "daemon restart",
                DAEMON_RESTART_LEASE_TIMEOUT,
            )
        },
        refresh_daemon_service,
        tracedecay::daemon::restore_quiesced_installed_service,
    )?;
    match restarted {
        Some((service_path, socket_path)) => {
            eprintln!(
                "\x1b[32m✔\x1b[0m Daemon service restarted at {}",
                service_path.display()
            );
            eprintln!("Daemon socket: {}", socket_path.display());
            Ok(())
        }
        None => Err(tracedecay::errors::TraceDecayError::Config {
            message: "no TraceDecay daemon service is installed — restart your `tracedecay daemon run` \
                      process manually, or run `tracedecay daemon install-service` to manage it as a service"
                .to_string(),
        }),
    }
}

fn tracedecay_home_dir() -> tracedecay::errors::Result<PathBuf> {
    tracedecay::agents::home_dir().ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
        message: "could not determine home directory".to_string(),
    })
}

pub(crate) fn tracedecay_bin_on_path() -> tracedecay::errors::Result<String> {
    tracedecay::agents::which_tracedecay().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "tracedecay not found on PATH".to_string(),
        }
    })
}

fn tracedecay_bin_for_generated_artifacts() -> tracedecay::errors::Result<String> {
    current_tracedecay_exe().map_or_else(tracedecay_bin_on_path, Ok)
}

fn current_tracedecay_exe() -> Option<String> {
    let current = std::env::current_exe().ok()?;
    current_tracedecay_exe_from(Some(&current))
}

fn current_tracedecay_exe_from(current: Option<&Path>) -> Option<String> {
    let current = current?;
    let stem = current.file_stem()?.to_str()?;
    (stem == "tracedecay").then(|| normalize_bin_path(current))
}

fn normalize_bin_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// How the `post-update` re-exec reacts to the binary-upgrade outcome.
pub(crate) enum RefreshPolicy {
    /// `update`: refresh even when nothing was installed, and a refresh
    /// failure fails the command.
    Always,
    /// `upgrade`: refresh only after a real install, and a refresh failure
    /// only warns — the binary upgrade itself already succeeded (mirroring
    /// how the health pass inside `post-update` is best-effort).
    AfterInstall,
}

/// The shared `update` / `upgrade` flow: install the new binary, then re-exec
/// the NEW binary's `post-update` subcommand — passed the freshly installed
/// binary path, when known — so the plugin refresh, daemon refresh, and
/// health pass run on the new version. `policy` decides whether the refresh
/// runs on a no-op upgrade and whether a refresh failure is fatal.
pub(crate) fn run_install_then_refresh<U, P>(
    policy: RefreshPolicy,
    upgrade: U,
    post_update: P,
) -> tracedecay::errors::Result<()>
where
    U: FnOnce() -> tracedecay::errors::Result<UpgradeOutcome>,
    P: FnOnce(Option<&Path>) -> tracedecay::errors::Result<()>,
{
    let outcome = upgrade()?;
    match policy {
        RefreshPolicy::Always => {
            let binary = match &outcome {
                UpgradeOutcome::Installed { binary } => binary.as_deref(),
                UpgradeOutcome::AlreadyCurrent => None,
            };
            post_update(binary)
        }
        RefreshPolicy::AfterInstall => match outcome {
            UpgradeOutcome::Installed { binary } => {
                if let Err(error) = post_update(binary.as_deref()) {
                    // Point the retry at the installed binary when we know
                    // where it lives — a bare `tracedecay` may not be on PATH.
                    let retry = match &binary {
                        Some(path) => format!("`{} update`", path.display()),
                        None => "`tracedecay update`".to_string(),
                    };
                    eprintln!(
                        "  \x1b[33mwarning:\x1b[0m post-upgrade refresh failed: {error}\n  \
                         The new binary is installed; run {retry} to retry the \
                         plugin refresh and health pass."
                    );
                }
                Ok(())
            }
            UpgradeOutcome::AlreadyCurrent => {
                eprintln!(
                    "Nothing was installed, so plugins were left untouched — \
                     run `tracedecay update` to refresh generated plugins anyway."
                );
                Ok(())
            }
        },
    }
}

pub(crate) fn run_update_command(
    no_heal: bool,
    no_reinstall: bool,
) -> tracedecay::errors::Result<()> {
    let lifecycle_lease = tracedecay::lifecycle_lease::acquire_exclusive("update")?;
    let lease_token = lifecycle_lease
        .token()
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: "update lifecycle lease did not provide an owner token".to_string(),
        })?
        .to_string();
    let mut lifecycle_lease = Some(lifecycle_lease);
    run_install_then_refresh(
        RefreshPolicy::Always,
        tracedecay::upgrade::run_upgrade,
        move |binary| {
            let held_lease =
                prepare_post_update_lease(lifecycle_lease.take().ok_or_else(|| {
                    tracedecay::errors::TraceDecayError::Config {
                        message: "update lifecycle lease was already consumed".to_string(),
                    }
                })?);
            let result = run_post_update_subcommand(no_heal, no_reinstall, binary, &lease_token);
            drop(held_lease);
            result
        },
    )
}

pub(crate) fn run_upgrade_command(
    no_heal: bool,
    no_reinstall: bool,
) -> tracedecay::errors::Result<()> {
    let lifecycle_lease = tracedecay::lifecycle_lease::acquire_exclusive("upgrade")?;
    let lease_token = lifecycle_lease
        .token()
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: "upgrade lifecycle lease did not provide an owner token".to_string(),
        })?
        .to_string();
    let mut lifecycle_lease = Some(lifecycle_lease);
    run_install_then_refresh(
        RefreshPolicy::AfterInstall,
        tracedecay::upgrade::run_upgrade,
        move |binary| {
            let held_lease =
                prepare_post_update_lease(lifecycle_lease.take().ok_or_else(|| {
                    tracedecay::errors::TraceDecayError::Config {
                        message: "upgrade lifecycle lease was already consumed".to_string(),
                    }
                })?);
            let result = run_post_update_subcommand(no_heal, no_reinstall, binary, &lease_token);
            drop(held_lease);
            result
        },
    )
}

pub(crate) fn run_dogfood_command() -> tracedecay::errors::Result<()> {
    let current_exe =
        std::env::current_exe().map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!("could not resolve source-built executable: {error}"),
        })?;
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/dogfood.sh");
    let status = std::process::Command::new("bash")
        .arg(&script)
        .env("TRACEDECAY_DOGFOOD_SOURCE_BINARY", &current_exe)
        .status()
        .map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!("failed to launch {}: {error}", script.display()),
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(tracedecay::errors::TraceDecayError::Config {
            message: format!("dogfood installer failed with status: {status}"),
        })
    }
}

fn prepare_post_update_lease(
    lease: tracedecay::lifecycle_lease::LifecycleLease,
) -> Option<tracedecay::lifecycle_lease::LifecycleLease> {
    #[cfg(windows)]
    {
        drop(lease);
        None
    }
    #[cfg(not(windows))]
    Some(lease)
}

/// The binary to re-exec for `post-update`: the freshly installed one when
/// the upgrade reported where it landed, otherwise the currently running
/// binary. This keeps source-built dogfood on the source-built binary.
fn post_update_binary(installed: Option<&Path>) -> tracedecay::errors::Result<String> {
    let current = std::env::current_exe().ok();
    post_update_binary_from(installed, current.as_deref()).map_or_else(tracedecay_bin_on_path, Ok)
}

fn post_update_binary_from(installed: Option<&Path>, current: Option<&Path>) -> Option<String> {
    installed
        .filter(|path| path.exists())
        .map(normalize_bin_path)
        .or_else(|| current_tracedecay_exe_from(current))
}

fn run_post_update_subcommand(
    no_heal: bool,
    no_reinstall: bool,
    installed: Option<&Path>,
    lifecycle_lease_token: &str,
) -> tracedecay::errors::Result<()> {
    let tracedecay_bin = post_update_binary(installed)?;
    let mut command = std::process::Command::new(&tracedecay_bin);
    command
        .arg("post-update")
        .arg("--lifecycle-lease-token")
        .arg(lifecycle_lease_token);
    if no_heal {
        command.arg("--no-heal");
    }
    if no_reinstall {
        command.arg("--no-reinstall");
    }
    let status = command
        .status()
        .map_err(|e| tracedecay::errors::TraceDecayError::Config {
            message: format!("failed to run post-update with '{tracedecay_bin}': {e}"),
        })?;
    if status.success() {
        return Ok(());
    }
    Err(tracedecay::errors::TraceDecayError::Config {
        message: format!("post-update failed with status: {status}"),
    })
}

/// The result of a tracked-agent reinstall pass. Version markers may only
/// advance on [`ReinstallOutcome::AllOk`]; a partial failure leaves the
/// markers untouched so the startup silent reinstall retries the work.
pub(crate) enum ReinstallOutcome {
    /// Every tracked agent reinstalled successfully (an empty tracked list is
    /// also `AllOk`).
    AllOk,
    /// One or more tracked agents failed to reinstall; `failed` lists ids (or
    /// a descriptive pseudo-id when the environment could not be resolved).
    PartialFailure { failed: Vec<String> },
}

/// Partitions per-agent reinstall results into a [`ReinstallOutcome`]. A pure
/// helper so the outcome logic is unit-testable without touching the real
/// filesystem or agent registry.
pub(crate) fn partition_reinstall_results(
    results: Vec<(String, tracedecay::errors::Result<()>)>,
) -> ReinstallOutcome {
    let failed: Vec<String> = results
        .into_iter()
        .filter_map(|(id, result)| result.err().map(|_| id))
        .collect();
    if failed.is_empty() {
        ReinstallOutcome::AllOk
    } else {
        ReinstallOutcome::PartialFailure { failed }
    }
}

/// Re-runs full `install()` + `post_install()` for every tracked agent so tool
/// permissions, hooks, and MCP config stay in sync with the running binary — a
/// superset of `refresh_generated_plugins`, which rewrites generated artifacts
/// only. Mirrors the canonical `handle_reinstall_command` (global scope:
/// `project_root: None`). Continues past a failing agent; returns
/// [`ReinstallOutcome::PartialFailure`] listing every failure (an empty tracked
/// list is [`ReinstallOutcome::AllOk`]). If the home or binary cannot be
/// resolved, no install runs and a descriptive failure is reported so the
/// version markers stay put.
pub(crate) async fn reinstall_tracked_agents(user_config: &UserConfig) -> ReinstallOutcome {
    reinstall_tracked_agents_with_lease(user_config, None).await
}

async fn reinstall_tracked_agents_under_lease(
    user_config: &UserConfig,
    lifecycle_lease: &tracedecay::lifecycle_lease::LifecycleLease,
) -> ReinstallOutcome {
    reinstall_tracked_agents_with_lease(user_config, Some(lifecycle_lease)).await
}

async fn reinstall_tracked_agents_with_lease(
    user_config: &UserConfig,
    lifecycle_lease: Option<&tracedecay::lifecycle_lease::LifecycleLease>,
) -> ReinstallOutcome {
    let (Some(home), Some(bin)) = (
        tracedecay::agents::home_dir(),
        tracedecay::agents::which_tracedecay(),
    ) else {
        return ReinstallOutcome::PartialFailure {
            failed: vec![
                "<environment>: could not resolve home directory or tracedecay binary on PATH"
                    .to_string(),
            ],
        };
    };
    let results = match lifecycle_lease {
        Some(lifecycle_lease) => {
            crate::agent_cmd::reinstall_agent_integrations_under_lease(
                &user_config.installed_agents,
                &home,
                &bin,
                lifecycle_lease,
            )
            .await
        }
        None => {
            crate::agent_cmd::reinstall_agent_integrations(
                &user_config.installed_agents,
                &home,
                &bin,
            )
            .await
        }
    };
    partition_reinstall_results(results)
}

pub(crate) async fn run_post_update_tasks(
    no_heal: bool,
    no_reinstall: bool,
    lifecycle_lease: &tracedecay::lifecycle_lease::LifecycleLease,
) -> tracedecay::errors::Result<()> {
    eprintln!("\nPreparing safe post-update maintenance.");
    eprintln!("  Waiting for TraceDecay writers to shut down cleanly — do not interrupt.");
    let previous_daemon_state = tracedecay::daemon::quiesce_installed_service_under_lease()?;
    eprintln!("\x1b[32m✔\x1b[0m TraceDecay writers stopped; exclusive maintenance window active.");
    let mutation_result = run_post_update_mutations(no_heal, no_reinstall, lifecycle_lease).await;
    let restart_result = refresh_daemon_service_after_update(previous_daemon_state);
    mutation_result?;
    restart_result
}

async fn run_post_update_mutations(
    no_heal: bool,
    no_reinstall: bool,
    lifecycle_lease: &tracedecay::lifecycle_lease::LifecycleLease,
) -> tracedecay::errors::Result<()> {
    refresh_generated_plugins().await?;
    if no_heal {
        eprintln!("Skipping post-update health pass (--no-heal).");
    } else {
        tracedecay::doctor::heal::run_post_update_health_pass_under_lease(lifecycle_lease).await;
    }

    if no_reinstall {
        eprintln!("Skipping agent integration refresh (--no-reinstall).");
        // `--no-reinstall` is a durable opt-out for THIS version, not a
        // one-command deferral: advance the version markers so the startup
        // silent reinstall (`silent_reinstall_action`) does not immediately
        // undo the skip on the next ordinary command and reinstall everything
        // anyway. The next real upgrade re-arms the reinstall as usual.
        let mut config = UserConfig::load();
        if config.mark_version_installed(env!("CARGO_PKG_VERSION")) {
            if let Err(err) = config.save() {
                eprintln!("warning: could not save tracedecay config: {err}");
            }
        }
        return Ok(());
    }

    // The generated-artifact refresh above skips config-managed integrations
    // (claude, copilot, …), but a version bump can change their tool
    // permissions, hooks, or MCP config too. Run the same full tracked-agent
    // install pass the startup silent reinstall would, then advance the
    // version markers so that startup pass does not repeat it. On failure the
    // markers stay put, so the next ordinary command retries via the silent
    // reinstall.
    //
    // Migrate first so a configured-but-untracked agent (has_tracedecay true,
    // absent from `installed_agents`) is picked up and refreshed too — exactly
    // what the canonical `handle_reinstall_command` does.
    let mut config = UserConfig::load();
    if let Some(home) = tracedecay::agents::home_dir() {
        tracedecay::agents::migrate_installed_agents(&home, &mut config);
    }
    // Prune tracked ids that no longer resolve to an integration (a release
    // renamed/removed one, or a typo landed in `installed_agents`).
    // `migrate_installed_agents` only ADDS ids, so without this the stale id
    // would be retried on every command forever. The reinstall pass already
    // skips such ids, but dropping them here stops the pointless retry churn.
    let before = config.installed_agents.len();
    config
        .installed_agents
        .retain(|id| tracedecay::agents::get_integration(id).is_ok());
    if config.installed_agents.len() != before {
        if let Err(err) = config.save() {
            eprintln!("warning: could not save tracedecay config: {err}");
        }
    }
    if config.installed_agents.is_empty() {
        eprintln!("Refreshing agent integrations: nothing to refresh");
    } else {
        eprintln!(
            "Refreshing agent integrations: {}",
            config.installed_agents.join(", ")
        );
    }
    match reinstall_tracked_agents_under_lease(&config, lifecycle_lease).await {
        ReinstallOutcome::AllOk => {
            if config.mark_version_installed(env!("CARGO_PKG_VERSION")) {
                if let Err(err) = config.save() {
                    eprintln!("warning: could not save tracedecay config: {err}");
                }
            }
        }
        ReinstallOutcome::PartialFailure { failed } => {
            eprintln!(
                "  \x1b[33mwarning:\x1b[0m agent install failed for: {}; \
                 it will be retried on the next tracedecay command.",
                failed.join(", ")
            );
        }
    }
    reconcile_materialized_managed_skills_after_update();
    Ok(())
}

/// Reconciles already-Active managed skills into every detected host skills
/// directory on `tracedecay update`, so a skill approved before this binary
/// shipped (or a body update applied since the last activation) still lands as
/// a real, host-loadable `SKILL.md`. Fork-protected and best-effort: a failure
/// here never fails the update.
fn reconcile_materialized_managed_skills_after_update() {
    let Ok(profile_root) = tracedecay::storage::default_profile_root() else {
        return;
    };
    let start = std::env::current_dir()
        .ok()
        .or_else(tracedecay::agents::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    let project_root = tracedecay::automation::skill_materialization::resolve_project_root(&start);
    tracedecay::automation::skill_materialization::reconcile_after_activation(
        &profile_root,
        &project_root,
    );
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    use super::{
        RefreshPolicy, ReinstallOutcome, current_tracedecay_exe_from, normalize_bin_path,
        partition_reinstall_results, post_update_binary, post_update_binary_from,
        prepare_post_update_lease, restart_daemon_service_with, run_install_then_refresh,
    };
    use tempfile::TempDir;
    use tracedecay::upgrade::UpgradeOutcome;

    #[test]
    fn daemon_restart_quiesces_service_before_acquiring_exclusive_lease() {
        let order = RefCell::new(Vec::new());
        let result = restart_daemon_service_with(
            || {
                order.borrow_mut().push("quiesce");
                Ok(tracedecay::daemon::DaemonServiceState::RunningEnabled)
            },
            || {
                assert_eq!(order.borrow().as_slice(), ["quiesce"]);
                order.borrow_mut().push("acquire");
                Ok(())
            },
            |state| {
                assert_eq!(
                    state,
                    tracedecay::daemon::DaemonServiceState::RunningEnabled
                );
                order.borrow_mut().push("refresh");
                Ok(Some((PathBuf::from("service"), PathBuf::from("socket"))))
            },
            |_| panic!("successful lease acquisition must not restore the old service"),
        )
        .expect("restart orchestration");

        assert_eq!(
            result,
            Some((PathBuf::from("service"), PathBuf::from("socket")))
        );
        assert_eq!(order.into_inner(), ["quiesce", "acquire", "refresh"]);
    }

    #[test]
    fn daemon_restart_forces_stopped_service_running() {
        let result = restart_daemon_service_with(
            || Ok(tracedecay::daemon::DaemonServiceState::StoppedEnabled),
            || Ok(()),
            |state| {
                assert_eq!(
                    state,
                    tracedecay::daemon::DaemonServiceState::RunningEnabled
                );
                Ok(Some((PathBuf::from("service"), PathBuf::from("socket"))))
            },
            |_| panic!("successful lease acquisition must not restore the old service"),
        )
        .expect("restart orchestration");

        assert!(result.is_some());
    }

    #[test]
    fn daemon_restart_restores_running_service_when_exclusive_lease_acquisition_fails() {
        let order = RefCell::new(Vec::new());
        let result = restart_daemon_service_with(
            || {
                order.borrow_mut().push("quiesce");
                Ok(tracedecay::daemon::DaemonServiceState::RunningEnabled)
            },
            || -> tracedecay::errors::Result<()> {
                order.borrow_mut().push("acquire");
                Err(config_err("lifecycle lease busy"))
            },
            |_| {
                order.borrow_mut().push("refresh");
                Ok(None)
            },
            |state| {
                assert_eq!(
                    state,
                    tracedecay::daemon::DaemonServiceState::RunningEnabled
                );
                order.borrow_mut().push("restore");
                Ok(())
            },
        );

        assert!(
            result
                .expect_err("lease acquisition should fail")
                .to_string()
                .contains("lifecycle lease busy")
        );
        assert_eq!(order.into_inner(), ["quiesce", "acquire", "restore"]);
    }

    #[test]
    fn post_update_lease_handoff_matches_platform_contract() {
        let profile = TempDir::new().unwrap();
        let lease =
            tracedecay::lifecycle_lease::acquire_exclusive_for_profile(profile.path(), "update")
                .unwrap();

        let held = prepare_post_update_lease(lease);
        let reacquired = tracedecay::lifecycle_lease::acquire_exclusive_for_profile(
            profile.path(),
            "post-update",
        );

        #[cfg(windows)]
        assert!(reacquired.is_ok());
        #[cfg(not(windows))]
        assert!(reacquired.is_err());
        drop(held);
    }
    use tracedecay::user_config::UserConfig;

    fn config_err(message: &str) -> tracedecay::errors::TraceDecayError {
        tracedecay::errors::TraceDecayError::Config {
            message: message.to_string(),
        }
    }

    fn ok(id: &str) -> (String, tracedecay::errors::Result<()>) {
        (id.to_string(), Ok(()))
    }

    fn err(id: &str) -> (String, tracedecay::errors::Result<()>) {
        (id.to_string(), Err(config_err("install failed")))
    }

    #[test]
    fn generated_artifact_bin_accepts_cargo_target_tracedecay_exe() {
        let current = Path::new("/repo/target/debug/tracedecay");

        assert_eq!(
            current_tracedecay_exe_from(Some(current)).as_deref(),
            Some("/repo/target/debug/tracedecay")
        );
    }

    #[test]
    fn generated_artifact_bin_ignores_non_tracedecay_test_exe() {
        let current = Path::new("/repo/target/debug/deps/agent_suite-abc123");

        assert_eq!(current_tracedecay_exe_from(Some(current)), None);
    }

    #[test]
    fn partition_empty_is_all_ok() {
        assert!(matches!(
            partition_reinstall_results(Vec::new()),
            ReinstallOutcome::AllOk
        ));
    }

    #[test]
    fn partition_all_success_is_all_ok() {
        assert!(matches!(
            partition_reinstall_results(vec![ok("claude"), ok("cursor")]),
            ReinstallOutcome::AllOk
        ));
    }

    #[test]
    fn partition_collects_only_failed_ids_in_order() {
        match partition_reinstall_results(vec![ok("claude"), err("cursor"), err("copilot")]) {
            ReinstallOutcome::PartialFailure { failed } => {
                assert_eq!(failed, vec!["cursor".to_string(), "copilot".to_string()]);
            }
            ReinstallOutcome::AllOk => panic!("expected a partial failure"),
        }
    }

    /// An unresolvable tracked id (renamed/removed by a later release, or a
    /// typo in `installed_agents`) must be SKIPPED, not treated as a failure —
    /// otherwise it gates marker advancement forever and wedges the startup
    /// silent reinstall into an infinite reinstall loop. The reinstall pass
    /// drops it from the results entirely, so an otherwise-empty pass is AllOk
    /// and the markers advance.
    #[tokio::test]
    async fn reinstall_agent_integrations_skips_unknown_ids()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let home = TempDir::new()?;
        let results = crate::agent_cmd::reinstall_agent_integrations(
            &["unknown-agent".to_string()],
            home.path(),
            "tracedecay",
        )
        .await;
        // Skipped, not failed: the unknown id is absent from the results.
        assert!(
            results.is_empty(),
            "unknown tracked agent id must be skipped, not reported: {:?}",
            results.iter().map(|(id, _)| id).collect::<Vec<_>>()
        );
        assert!(
            matches!(
                partition_reinstall_results(results),
                ReinstallOutcome::AllOk
            ),
            "an unknown id must not prevent AllOk / marker advancement"
        );
        Ok(())
    }

    /// A genuine `install()` failure (as opposed to an unresolvable id) is a
    /// real failure: it stays in the results and yields PartialFailure so the
    /// version markers do NOT advance and the work is retried.
    #[test]
    fn real_install_failure_yields_partial_failure() {
        match partition_reinstall_results(vec![ok("claude"), err("cursor")]) {
            ReinstallOutcome::PartialFailure { failed } => {
                assert_eq!(failed, vec!["cursor".to_string()]);
            }
            ReinstallOutcome::AllOk => panic!("a real install() failure must gate markers"),
        }
    }

    /// Markers advance only when every tracked agent reinstalled (AllOk).
    #[test]
    fn markers_advance_only_on_all_ok() {
        let running = "9.9.9";

        // AllOk: markers advance.
        let mut config = UserConfig {
            installed_agents: vec!["claude".to_string()],
            previous_version: "9.0.0".to_string(),
            ..UserConfig::default()
        };
        if let ReinstallOutcome::AllOk = partition_reinstall_results(vec![ok("claude")]) {
            assert!(config.mark_version_installed(running));
        } else {
            panic!("expected AllOk");
        }
        assert_eq!(config.previous_version, running);
        assert_eq!(config.last_installed_version, running);

        // PartialFailure: markers must NOT advance, so a later
        // mark_version_installed still reports work to do.
        let mut config = UserConfig {
            installed_agents: vec!["claude".to_string()],
            previous_version: "9.0.0".to_string(),
            ..UserConfig::default()
        };
        match partition_reinstall_results(vec![err("claude")]) {
            ReinstallOutcome::PartialFailure { .. } => {
                // Intentionally do not advance markers.
            }
            ReinstallOutcome::AllOk => panic!("expected PartialFailure"),
        }
        assert_eq!(config.previous_version, "9.0.0");
        assert!(config.last_installed_version.is_empty());
        // A subsequent full install pass would still have work to record.
        assert!(config.mark_version_installed(running));
    }

    /// Closure factory for the upgrade step: records `label`, returns `result`.
    fn record_upgrade<'a>(
        calls: &'a RefCell<Vec<&'static str>>,
        label: &'static str,
        result: tracedecay::errors::Result<UpgradeOutcome>,
    ) -> impl FnOnce() -> tracedecay::errors::Result<UpgradeOutcome> + 'a {
        move || {
            calls.borrow_mut().push(label);
            result
        }
    }

    /// Closure factory for the post-update step: records `label` and the
    /// binary path it was handed, returns `result`.
    fn record_post_update<'a>(
        calls: &'a RefCell<Vec<&'static str>>,
        label: &'static str,
        seen_binary: &'a RefCell<Option<Option<PathBuf>>>,
        result: tracedecay::errors::Result<()>,
    ) -> impl FnOnce(Option<&Path>) -> tracedecay::errors::Result<()> + 'a {
        move |binary| {
            calls.borrow_mut().push(label);
            *seen_binary.borrow_mut() = Some(binary.map(Path::to_path_buf));
            result
        }
    }

    #[test]
    fn update_policy_runs_post_update_after_upgrade() {
        let calls = RefCell::new(Vec::new());
        let seen_binary = RefCell::new(None);

        run_install_then_refresh(
            RefreshPolicy::Always,
            record_upgrade(&calls, "upgrade", Ok(UpgradeOutcome::AlreadyCurrent)),
            record_post_update(&calls, "post-update", &seen_binary, Ok(())),
        )
        .expect("update steps should succeed");

        assert_eq!(calls.into_inner(), vec!["upgrade", "post-update"]);
        // Nothing was installed, so no installed-binary path to prefer.
        assert_eq!(seen_binary.into_inner(), Some(None));
    }

    #[test]
    fn update_policy_stops_after_upgrade_failure() {
        let calls = RefCell::new(Vec::new());
        let seen_binary = RefCell::new(None);

        let result = run_install_then_refresh(
            RefreshPolicy::Always,
            record_upgrade(&calls, "upgrade", Err(config_err("upgrade failed"))),
            record_post_update(&calls, "post-update", &seen_binary, Ok(())),
        );

        assert!(result.is_err());
        assert_eq!(calls.into_inner(), vec!["upgrade"]);
    }

    #[test]
    fn update_policy_treats_post_update_failure_as_fatal() {
        let calls = RefCell::new(Vec::new());
        let seen_binary = RefCell::new(None);

        let result = run_install_then_refresh(
            RefreshPolicy::Always,
            record_upgrade(&calls, "upgrade", Ok(UpgradeOutcome::AlreadyCurrent)),
            record_post_update(
                &calls,
                "post-update",
                &seen_binary,
                Err(config_err("plugin refresh failed")),
            ),
        );

        assert!(result.is_err());
        assert_eq!(calls.into_inner(), vec!["upgrade", "post-update"]);
    }

    #[test]
    fn upgrade_policy_forwards_installed_binary_to_post_update() {
        let calls = RefCell::new(Vec::new());
        let seen_binary = RefCell::new(None);
        let installed = PathBuf::from("/opt/homebrew/bin/tracedecay");

        run_install_then_refresh(
            RefreshPolicy::AfterInstall,
            record_upgrade(
                &calls,
                "upgrade",
                Ok(UpgradeOutcome::Installed {
                    binary: Some(installed.clone()),
                }),
            ),
            record_post_update(&calls, "post-update", &seen_binary, Ok(())),
        )
        .expect("upgrade steps should succeed");

        assert_eq!(calls.into_inner(), vec!["upgrade", "post-update"]);
        // The refresh must re-exec the binary the upgrade just installed,
        // never a re-resolved (possibly stale) one.
        assert_eq!(seen_binary.into_inner(), Some(Some(installed)));
    }

    #[test]
    fn upgrade_policy_skips_post_update_when_already_current() {
        let calls = RefCell::new(Vec::new());
        let seen_binary = RefCell::new(None);

        run_install_then_refresh(
            RefreshPolicy::AfterInstall,
            record_upgrade(&calls, "upgrade", Ok(UpgradeOutcome::AlreadyCurrent)),
            record_post_update(&calls, "post-update", &seen_binary, Ok(())),
        )
        .expect("an up-to-date upgrade should stay a successful no-op");

        assert_eq!(calls.into_inner(), vec!["upgrade"]);
        assert_eq!(seen_binary.into_inner(), None);
    }

    #[test]
    fn upgrade_policy_tolerates_post_update_failure() {
        let calls = RefCell::new(Vec::new());
        let seen_binary = RefCell::new(None);

        let result = run_install_then_refresh(
            RefreshPolicy::AfterInstall,
            record_upgrade(
                &calls,
                "upgrade",
                Ok(UpgradeOutcome::Installed { binary: None }),
            ),
            record_post_update(
                &calls,
                "post-update",
                &seen_binary,
                Err(config_err("plugin refresh failed")),
            ),
        );

        // The binary upgrade itself succeeded — a refresh failure only warns.
        assert!(result.is_ok());
        assert_eq!(calls.into_inner(), vec!["upgrade", "post-update"]);
    }

    #[test]
    fn upgrade_policy_stops_after_upgrade_failure() {
        let calls = RefCell::new(Vec::new());
        let seen_binary = RefCell::new(None);

        let result = run_install_then_refresh(
            RefreshPolicy::AfterInstall,
            record_upgrade(&calls, "upgrade", Err(config_err("upgrade failed"))),
            record_post_update(&calls, "post-update", &seen_binary, Ok(())),
        );

        assert!(result.is_err());
        assert_eq!(calls.into_inner(), vec!["upgrade"]);
    }

    #[test]
    fn post_update_binary_prefers_the_freshly_installed_path() {
        let temp = tempfile::tempdir().expect("tempdir should exist");
        let installed = temp.path().join("tracedecay");
        std::fs::write(&installed, b"new-binary").expect("binary should be writable");

        let resolved = post_update_binary(Some(&installed)).expect("installed path should resolve");

        assert_eq!(resolved, normalize_bin_path(&installed));
    }

    #[test]
    fn post_update_binary_keeps_source_built_current_executable() {
        let temp = tempfile::tempdir().expect("tempdir should exist");
        let current = temp.path().join("tracedecay");
        std::fs::write(&current, b"source-built").expect("binary should be writable");
        let expected = current.to_string_lossy().replace('\\', "/");

        assert_eq!(
            post_update_binary_from(None, Some(&current)).as_deref(),
            Some(expected.as_str())
        );
    }

    #[test]
    fn post_update_binary_ignores_a_missing_installed_path() {
        let temp = tempfile::tempdir().expect("tempdir should exist");
        let missing = temp.path().join("does-not-exist/tracedecay");

        // A dangling path (e.g. brew cleaned the keg) must fall back to the
        // normal resolution instead of re-execing a nonexistent file. Either
        // branch proves the dangling path was rejected — which one runs
        // depends on whether the test environment has tracedecay on PATH.
        match post_update_binary(Some(&missing)) {
            Ok(resolved) => assert_ne!(resolved, missing.to_string_lossy()),
            Err(error) => assert!(
                error.to_string().contains("not found on PATH"),
                "unexpected fallback error: {error}"
            ),
        }
    }
}
