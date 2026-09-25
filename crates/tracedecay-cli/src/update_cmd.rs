//! The `upgrade` / `update` / `post-update` flow: binary upgrade via
//! subprocess re-exec, daemon service refresh, and the tracked-agent
//! reinstall that keeps every host integration in sync.
//!
//! The post-update pass refreshes every already-configured agent integration
//! through its receipt-backed component-set transaction, the sole writer of
//! host artifacts, so a separate `tracedecay reinstall` is not needed after an
//! upgrade. Pass `--no-reinstall` to skip that agent-integration refresh.

use std::path::{Path, PathBuf};

use crate::upgrade::UpgradeOutcome;
use tracedecay_daemon_control as daemon_control;
use tracedecay_session_memory::user_config::UserConfig;

/// Rewrites the installed daemon service while preserving its captured
/// lifecycle state, returning the service path and socket or `None` when no
/// service is installed.
fn refresh_daemon_service(
    previous_state: daemon_control::DaemonServiceState,
) -> tracedecay_domain::errors::Result<Option<(PathBuf, PathBuf)>> {
    if !cfg!(any(target_os = "linux", target_os = "macos", windows)) {
        return Ok(None);
    }
    let tracedecay_bin =
        tracedecay_agent_hosts::agents::which_tracedecay_path().ok_or_else(|| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: "tracedecay not found on PATH".to_string(),
            }
        })?;
    let spec = daemon_control::service_spec(tracedecay_bin, None)?;
    refresh_daemon_service_with_spec(previous_state, &spec)
}

fn refresh_daemon_service_with_spec(
    previous_state: daemon_control::DaemonServiceState,
    spec: &daemon_control::DaemonServiceSpec,
) -> tracedecay_domain::errors::Result<Option<(PathBuf, PathBuf)>> {
    let socket_path = daemon_control::installed_service_socket_path()?
        .unwrap_or_else(|| spec.socket_path.clone());
    Ok(
        daemon_control::refresh_installed_service_under_lease_with_state(
            spec,
            previous_state,
            crate::product_runtime::PRODUCT_BUILD_VERSION,
        )?
        .map(|service_path| (service_path, socket_path)),
    )
}

fn print_daemon_transport_location(socket_path: &Path) {
    if cfg!(windows) {
        if let Some(profile_root) = socket_path.parent() {
            eprintln!("Daemon profile root: {}", profile_root.display());
        }
        eprintln!("Daemon endpoint: authenticated loopback (authority-discovered)");
    } else {
        eprintln!("Daemon socket: {}", socket_path.display());
    }
}

fn refresh_daemon_service_after_update(
    previous_state: daemon_control::DaemonServiceState,
) -> tracedecay_domain::errors::Result<()> {
    match refresh_daemon_service(previous_state)? {
        Some((service_path, socket_path)) => {
            eprintln!(
                "\x1b[32m✔\x1b[0m Daemon service refreshed at {}",
                service_path.display()
            );
            print_daemon_transport_location(&socket_path);
        }
        None if daemon_control::daemon_reachable() => {
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

#[cfg(test)]
fn restart_daemon_service_with<Lease, Quiesce, Acquire, Refresh, Restore>(
    quiesce: Quiesce,
    acquire: Acquire,
    refresh: Refresh,
    restore: Restore,
) -> tracedecay_domain::errors::Result<Option<(PathBuf, PathBuf)>>
where
    Quiesce: FnOnce() -> tracedecay_domain::errors::Result<daemon_control::DaemonServiceState>,
    Acquire: FnOnce() -> tracedecay_domain::errors::Result<Lease>,
    Refresh: FnOnce(
        daemon_control::DaemonServiceState,
    ) -> tracedecay_domain::errors::Result<Option<(PathBuf, PathBuf)>>,
    Restore: FnOnce(daemon_control::DaemonServiceState) -> tracedecay_domain::errors::Result<()>,
{
    let previous_state = quiesce()?;
    let _lifecycle_lease = match acquire() {
        Ok(lease) => lease,
        Err(acquire_error) => {
            if matches!(
                previous_state,
                daemon_control::DaemonServiceState::RunningEnabled
                    | daemon_control::DaemonServiceState::RunningDisabled
            ) && let Err(restore_error) = restore(previous_state)
            {
                return Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!(
                        "{acquire_error}; additionally failed to restore the managed daemon service: {restore_error}"
                    ),
                });
            }
            return Err(acquire_error);
        }
    };
    refresh(daemon_control::DaemonServiceState::RunningEnabled)
}

pub(crate) fn restart_daemon_service() -> tracedecay_domain::errors::Result<()> {
    let guard = daemon_control::QuiescedDaemonLifecycle::acquire(
        "daemon restart",
        crate::product_runtime::PRODUCT_BUILD_VERSION,
    )?;
    let (stopped_state, desired_state) = match guard.previous_state() {
        daemon_control::DaemonServiceState::RunningEnabled
        | daemon_control::DaemonServiceState::StoppedEnabled => (
            daemon_control::DaemonServiceState::StoppedEnabled,
            daemon_control::DaemonServiceState::RunningEnabled,
        ),
        daemon_control::DaemonServiceState::RunningDisabled
        | daemon_control::DaemonServiceState::StoppedDisabled => (
            daemon_control::DaemonServiceState::StoppedDisabled,
            daemon_control::DaemonServiceState::RunningDisabled,
        ),
        daemon_control::DaemonServiceState::Missing => {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "no TraceDecay daemon service is installed, restart your `tracedecay daemon run` process manually, or run `tracedecay daemon install-service` to manage it as a service".to_string(),
            });
        }
        daemon_control::DaemonServiceState::Masked => {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "TraceDecay daemon service is masked; unmask it before restarting"
                    .to_string(),
            });
        }
    };
    let operation_result = refresh_daemon_service(stopped_state);
    let restore_result = guard.finish_with_state(desired_state);
    match combine_operation_and_restore("daemon restart", operation_result, restore_result)? {
        Some((service_path, socket_path)) => {
            eprintln!(
                "\x1b[32m✔\x1b[0m Daemon service restarted at {}",
                service_path.display()
            );
            print_daemon_transport_location(&socket_path);
            Ok(())
        }
        None => unreachable!("installed service disappeared during daemon restart"),
    }
}

pub(crate) fn tracedecay_bin_on_path() -> tracedecay_domain::errors::Result<String> {
    tracedecay_agent_hosts::agents::which_tracedecay().ok_or_else(|| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: "tracedecay not found on PATH".to_string(),
        }
    })
}

fn current_tracedecay_exe_from(current: Option<&Path>) -> Option<String> {
    let current = current?;
    let stem = current.file_stem()?.to_str()?;
    (stem == "tracedecay")
        .then(|| tracedecay_domain::forward_slash_text(&current.to_string_lossy()))
}

/// How the `post-update` re-exec reacts to the binary-upgrade outcome.
pub(crate) enum RefreshPolicy {
    /// `update`: refresh even when nothing was installed, and a refresh
    /// failure fails the command.
    Always,
    /// `upgrade`: refresh only after a real install, and a refresh failure
    /// only warns, the binary upgrade itself already succeeded (mirroring
    /// how the health pass inside `post-update` is best-effort).
    AfterInstall,
}

/// The shared `update` / `upgrade` flow: install the new binary, then re-exec
/// the NEW binary's `post-update` subcommand, passed the freshly installed
/// binary path, when known, so the plugin refresh, daemon refresh, and
/// health pass run on the new version. `policy` decides whether the refresh
/// runs on a no-op upgrade and whether a refresh failure is fatal.
///
/// Returns the installed binary's version when an install happened and its
/// version is known, so the surrounding maintenance window restores the
/// daemon validating the binary that actually starts. A tolerated
/// ([`RefreshPolicy::AfterInstall`]) refresh failure does not erase that
/// version: the new binary is installed regardless.
pub(crate) fn run_install_then_refresh<U, P>(
    policy: RefreshPolicy,
    upgrade: U,
    post_update: P,
) -> tracedecay_domain::errors::Result<Option<String>>
where
    U: FnOnce() -> tracedecay_domain::errors::Result<UpgradeOutcome>,
    P: FnOnce(Option<&Path>) -> tracedecay_domain::errors::Result<()>,
{
    let outcome = upgrade()?;
    match policy {
        RefreshPolicy::Always => {
            let (binary, installed_version) = match &outcome {
                UpgradeOutcome::Installed { binary, version } => {
                    (binary.as_deref(), version.clone())
                }
                UpgradeOutcome::AlreadyCurrent => (None, None),
            };
            post_update(binary)?;
            Ok(installed_version)
        }
        RefreshPolicy::AfterInstall => match outcome {
            UpgradeOutcome::Installed { binary, version } => {
                if let Err(error) = post_update(binary.as_deref()) {
                    // Point the retry at the installed binary when we know
                    // where it lives, a bare `tracedecay` may not be on PATH.
                    let retry = match &binary {
                        Some(path) => format!("`{} update`", path.display()),
                        None => "`tracedecay update`".to_string(),
                    };
                    eprintln!(
                        "  \x1b[33mwarning:\x1b[0m post-upgrade refresh failed: {error}\n  \
                         The new binary is installed; run {retry} to retry the \
                         plugin and agent-integration refresh."
                    );
                }
                Ok(version)
            }
            UpgradeOutcome::AlreadyCurrent => {
                eprintln!(
                    "Nothing was installed, so plugins were left untouched. \
                     run `tracedecay update` to refresh generated plugins anyway."
                );
                Ok(None)
            }
        },
    }
}

#[hotpath::measure(label = "cli.update.run", future = true)]
pub(crate) async fn run_update_command(
    no_reinstall: bool,
) -> tracedecay_domain::errors::Result<()> {
    run_update_flow("update", RefreshPolicy::Always, no_reinstall).await
}

#[hotpath::measure(label = "cli.upgrade.run", future = true)]
pub(crate) async fn run_upgrade_command(
    no_reinstall: bool,
) -> tracedecay_domain::errors::Result<()> {
    run_update_flow("upgrade", RefreshPolicy::AfterInstall, no_reinstall).await
}

async fn run_update_flow(
    operation: &str,
    refresh_policy: RefreshPolicy,
    no_reinstall: bool,
) -> tracedecay_domain::errors::Result<()> {
    daemon_control::with_exclusive_maintenance_window(
        operation,
        crate::product_runtime::PRODUCT_BUILD_VERSION,
        |lease_token| {
            let installed_version =
                run_install_then_refresh(refresh_policy, crate::upgrade::run_upgrade, |binary| {
                    run_post_update_subcommand(no_reinstall, binary, lease_token)
                })?;
            // Report the installed version so the window's daemon restore
            // validates the binary it actually starts, not the one that was
            // running before the upgrade.
            Ok(daemon_control::MaintenanceWindowOutcome {
                value: (),
                installed_version,
            })
        },
    )?;
    reset_refused_profile_authorities(operation).await
}

/// What the post-window profile probe decided.
#[derive(Debug, PartialEq, Eq)]
enum ProfileResetDecision {
    /// The restored daemon opens the profile; nothing to reset.
    Opens,
    /// The daemon refused the profile with a typed reset state.
    Reset { authority: String, reason: String },
    /// The probe could not decide (no reachable daemon, transport failure);
    /// reported, never acted on.
    Undecided(String),
}

fn profile_reset_decision(
    probe: tracedecay_domain::errors::Result<serde_json::Value>,
) -> ProfileResetDecision {
    match probe {
        Ok(_) => ProfileResetDecision::Opens,
        Err(error) => match error.reset_required_context() {
            Some((authority, reason)) => ProfileResetDecision::Reset {
                authority: authority.to_owned(),
                reason: reason.to_owned(),
            },
            None => ProfileResetDecision::Undecided(error.to_string()),
        },
    }
}

/// The upgrade journey ends with core tools that work, not with a typed
/// refusal on the first tool call. Once the maintenance window has restored
/// the daemon on the new binary, a projectless probe asks it to open the
/// profile registry; a typed reset refusal is acted on here by resetting the
/// complete profile database state, the one reset authority a profile-scoped
/// refusal has. Old shapes are deleted, never migrated or backed up.
async fn reset_refused_profile_authorities(
    operation: &str,
) -> tracedecay_domain::errors::Result<()> {
    if !daemon_control::daemon_reachable() {
        eprintln!(
            "No reachable TraceDecay daemon after {operation}; the profile's persisted shape is \
             checked on the daemon's next open."
        );
        return Ok(());
    }
    let probe = crate::commands::daemon_tool_json(
        None,
        "tracedecay_project_list",
        serde_json::json!({ "limit": 1, "format": "json" }),
    )
    .await;
    match profile_reset_decision(probe) {
        ProfileResetDecision::Opens => Ok(()),
        ProfileResetDecision::Reset { authority, reason } => {
            eprintln!(
                "\n\x1b[33mThe upgraded daemon refuses the persisted {authority} shape: \
                 {reason}\x1b[0m\n\
                 Resetting the complete profile database state so core tools work on this \
                 binary; refused authorities are never migrated or backed up. Re-run \
                 `tracedecay init <project-root>` for each project afterwards."
            );
            crate::commands::handle_wipe(true, true).await
        }
        ProfileResetDecision::Undecided(detail) => {
            eprintln!(
                "  \x1b[33mwarning:\x1b[0m could not verify that the profile opens after \
                 {operation}: {detail}"
            );
            Ok(())
        }
    }
}

fn combine_operation_and_restore<T>(
    operation: &str,
    operation_result: tracedecay_domain::errors::Result<T>,
    restore_result: tracedecay_domain::errors::Result<()>,
) -> tracedecay_domain::errors::Result<T> {
    match (operation_result, restore_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(operation_error), Err(restore_error)) => {
            Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: format!(
                    "{operation} failed: {operation_error}; daemon state restoration also failed: {restore_error}"
                ),
            })
        }
    }
}

#[hotpath::measure(label = "cli.update.post", future = true)]
pub(crate) async fn run_post_update_command(
    no_reinstall: bool,
    lifecycle_lease_token: Option<&str>,
) -> tracedecay_domain::errors::Result<()> {
    if let Some(token) = lifecycle_lease_token {
        let lifecycle_lease =
            tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_or_inherited(
                "post-update",
                Some(token),
            )?;
        return run_post_update_tasks(no_reinstall, &lifecycle_lease).await;
    }

    let guard = daemon_control::QuiescedDaemonLifecycle::acquire(
        "post-update",
        crate::product_runtime::PRODUCT_BUILD_VERSION,
    )?;
    let operation_result = match guard.lifecycle_lease() {
        Ok(lifecycle_lease) => run_post_update_tasks(no_reinstall, lifecycle_lease).await,
        Err(error) => Err(error),
    };
    let restore_result = guard.finish_after_update();
    combine_operation_and_restore("post-update", operation_result, restore_result)
}

// Windows must drop the lease before replacing a running executable, while
// other platforms retain it through the child handoff. Production drives this
// contract through `daemon::with_exclusive_maintenance_window`; this mirror
// keeps the platform contract under test.
#[cfg(test)]
#[allow(clippy::unnecessary_wraps)]
fn prepare_post_update_lease(
    lease: tracedecay_runtime_core::lifecycle_lease::LifecycleLease,
) -> Option<tracedecay_runtime_core::lifecycle_lease::LifecycleLease> {
    #[cfg(windows)]
    {
        drop(lease);
        None
    }
    #[cfg(not(windows))]
    Some(lease)
}

/// The binary to re-exec for `post-update`: freshly installed when reported,
/// otherwise the currently running binary.
fn post_update_binary(installed: Option<&Path>) -> tracedecay_domain::errors::Result<String> {
    let current = std::env::current_exe().ok();
    post_update_binary_from(installed, current.as_deref()).map_or_else(tracedecay_bin_on_path, Ok)
}

fn post_update_binary_from(installed: Option<&Path>, current: Option<&Path>) -> Option<String> {
    installed
        .filter(|path| path.exists())
        .map(tracedecay_domain::forward_slash_path)
        .or_else(|| current_tracedecay_exe_from(current))
}

fn run_post_update_subcommand(
    no_reinstall: bool,
    installed: Option<&Path>,
    lifecycle_lease_token: &str,
) -> tracedecay_domain::errors::Result<()> {
    let tracedecay_bin = post_update_binary(installed)?;
    let mut command = std::process::Command::new(&tracedecay_bin);
    command
        .arg("post-update")
        .arg("--lifecycle-lease-token")
        .arg(lifecycle_lease_token);
    if no_reinstall {
        command.arg("--no-reinstall");
    }
    let status =
        command
            .status()
            .map_err(|e| tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("failed to run post-update with '{tracedecay_bin}': {e}"),
            })?;
    if status.success() {
        return Ok(());
    }
    Err(tracedecay_domain::errors::TraceDecayError::Config {
        message: format!("post-update failed with status: {status}"),
    })
}

/// The result of a tracked-agent reinstall pass. Version markers may only
/// advance on [`ReinstallOutcome::AllOk`]; a failure leaves the markers
/// untouched so the startup silent reinstall retries the work.
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
    results: Vec<(
        String,
        tracedecay_domain::errors::Result<crate::agent_cmd::AgentReinstallOutcome>,
    )>,
) -> ReinstallOutcome {
    // Carry the reason, not just the name, so the operator can diagnose a
    // failed integration refresh without reading the installer source.
    let mut failed = Vec::new();
    for (id, result) in results {
        match result {
            Ok(crate::agent_cmd::AgentReinstallOutcome::Installed) => {}
            Err(error) => failed.push(format!("{id}: {error}")),
        }
    }
    if !failed.is_empty() {
        ReinstallOutcome::PartialFailure { failed }
    } else {
        ReinstallOutcome::AllOk
    }
}

/// Records a completed tracked-agent reinstall pass by advancing BOTH version
/// markers, persisting the config only when something actually changed.
///
/// This is the one place any completed pass may record its version, and it is
/// deliberately not open-coded. Both markers describe the last explicit
/// lifecycle pass; ordinary CLI entrypoints never act on them. Callers that
/// treat a failed save as advisory report the error, while `tracedecay
/// reinstall` surfaces it because its explicit lifecycle result was not
/// durably recorded.
pub(crate) fn record_completed_reinstall_pass(
    config: &mut UserConfig,
) -> tracedecay_domain::errors::Result<()> {
    if config.mark_version_installed(env!("CARGO_PKG_VERSION")) {
        config
            .save()
            .map_err(|err| tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("could not save tracedecay config: {err}"),
            })?;
    }
    Ok(())
}

/// Whether an explicit `tracedecay install` pass amounted to a full
/// tracked-agent refresh and may therefore call
/// [`record_completed_reinstall_pass`].
///
/// The install flow only (re)installs its selection delta, agents that were
/// already tracked are left untouched. After an upgrade those untouched
/// agents still carry the previous binary's integration, so the pass may
/// only record a completed full refresh when every agent that remains tracked
/// was actually installed by this very pass. An empty tracked set is trivially
/// covered: there is nothing left to refresh.
pub(crate) fn install_pass_covers_tracked_agents(
    tracked: &[String],
    refreshed: &std::collections::BTreeSet<String>,
) -> bool {
    tracked.iter().all(|id| refreshed.contains(id))
}

/// Re-runs the canonical component lifecycle for every tracked agent so
/// artifacts, tool permissions, hooks, and MCP config stay in sync with the
/// running binary, exactly as `tracedecay reinstall` does. Continues past a failing agent; returns
/// [`ReinstallOutcome::PartialFailure`] listing every failure (an empty tracked
/// list is [`ReinstallOutcome::AllOk`]). If the home or binary cannot be
/// resolved, no install runs and a descriptive failure is reported so the
/// version markers stay put.
async fn reinstall_tracked_agents_under_lease(
    agent_ids: &[String],
    home: &Path,
    lifecycle_lease: &tracedecay_runtime_core::lifecycle_lease::LifecycleLease,
) -> ReinstallOutcome {
    let Some(bin) = tracedecay_agent_hosts::agents::which_tracedecay() else {
        return ReinstallOutcome::PartialFailure {
            failed: vec!["<environment>: could not resolve tracedecay binary on PATH".to_string()],
        };
    };
    let results = crate::agent_cmd::reinstall_agent_integrations_under_lease(
        agent_ids,
        home,
        &bin,
        lifecycle_lease,
    )
    .await;
    partition_reinstall_results(results)
}

pub(crate) async fn run_post_update_tasks(
    no_reinstall: bool,
    lifecycle_lease: &tracedecay_runtime_core::lifecycle_lease::LifecycleLease,
) -> tracedecay_domain::errors::Result<()> {
    eprintln!("\nPreparing safe post-update maintenance.");
    eprintln!("  Waiting for TraceDecay writers to shut down cleanly, do not interrupt.");
    let previous_daemon_state = daemon_control::verify_installed_service_quiesced_under_lease()?;
    eprintln!("\x1b[32m✔\x1b[0m TraceDecay writers stopped; exclusive maintenance window active.");
    let mutation_result = run_post_update_mutations(no_reinstall, lifecycle_lease).await;
    let restart_result = refresh_daemon_service_after_update(previous_daemon_state);
    combine_operation_and_restore("post-update maintenance", mutation_result, restart_result)
}

async fn run_post_update_mutations(
    no_reinstall: bool,
    lifecycle_lease: &tracedecay_runtime_core::lifecycle_lease::LifecycleLease,
) -> tracedecay_domain::errors::Result<()> {
    if no_reinstall {
        eprintln!("Skipping agent integration refresh (--no-reinstall).");
        // `--no-reinstall` is a durable opt-out for THIS version, not a
        // one-command deferral: advance the version markers so the explicit
        // lifecycle decision remains durable for this version.
        let mut config = UserConfig::load();
        if let Err(err) = record_completed_reinstall_pass(&mut config) {
            eprintln!("warning: {err}");
        }
        return Ok(());
    }

    // A version bump can change any host's artifacts, permissions, hooks, or
    // MCP config. Run the full tracked-agent pass, then advance the version
    // markers. On failure the markers stay put so the incomplete explicit
    // lifecycle remains observable.
    let mut config = UserConfig::load();
    // Prune tracked ids that no longer resolve to an integration (a release
    // renamed/removed one, or a typo landed in `installed_agents`).
    // The reinstall pass skips such ids, but dropping them here stops the
    // pointless retry churn.
    let before = config.installed_agents.len();
    config
        .installed_agents
        .retain(|id| tracedecay_agent_hosts::agents::get_integration(id).is_ok());
    if config.installed_agents.len() != before
        && let Err(err) = config.save()
    {
        eprintln!("warning: could not save tracedecay config: {err}");
    }
    let Some(home) = tracedecay_agent_hosts::agents::home_dir() else {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        });
    };
    // Detected integrations the config never tracked are refreshed too, so an
    // upgrade over an older install does not leave doctor reporting stale
    // rendered versions that no maintenance pass will touch.
    let agent_ids = crate::agent_cmd::maintenance_sweep_agents(&config.installed_agents, &home);
    if agent_ids.is_empty() {
        eprintln!("Refreshing agent integrations: nothing to refresh");
    } else {
        eprintln!("Refreshing agent integrations: {}", agent_ids.join(", "));
    }
    let reinstall_result =
        match reinstall_tracked_agents_under_lease(&agent_ids, &home, lifecycle_lease).await {
            ReinstallOutcome::AllOk => {
                if config.installed_agents != agent_ids {
                    config.installed_agents = agent_ids;
                    if let Err(err) = config.save() {
                        eprintln!("warning: could not save tracedecay config: {err}");
                    }
                }
                if let Err(err) = record_completed_reinstall_pass(&mut config) {
                    eprintln!("warning: {err}");
                }
                Ok(())
            }
            ReinstallOutcome::PartialFailure { failed } => {
                eprintln!(
                    "  \x1b[33mwarning:\x1b[0m agent install failed for: {}; \
                 it will be retried on the next tracedecay command.",
                    failed.join(", ")
                );
                Ok(())
            }
        };
    deploy_managed_skills_after_lifecycle();
    reinstall_result
}

/// Redeploys managed skills after a lifecycle pass (`update`, `reinstall`,
/// `install`), so both halves of a managed skill converge on the store: the
/// host-loadable `SKILL.md` files and the per-host prompt index blocks.
///
/// Reconciling only the materialized files left the prompt index converging
/// solely as a side effect of a successful store mutation, so a store that
/// emptied without one kept advertising skills that no longer exist. Deploying
/// here makes the lifecycle converge the index whatever the store did.
/// Best-effort: a failure never fails the lifecycle pass.
pub(crate) fn deploy_managed_skills_after_lifecycle() {
    let Ok(profile_root) = tracedecay_runtime_core::storage::default_profile_root() else {
        return;
    };
    let Some(home) = tracedecay_agent_hosts::agents::home_dir() else {
        return;
    };
    let start = std::env::current_dir().ok().unwrap_or_else(|| home.clone());
    let project_root =
        tracedecay_automation_runtime::automation::skill_materialization::resolve_project_root(
            &start,
        );
    let receipt = tracedecay_automation_runtime::automation::skill_writer::deploy_managed_skills_at(
        &tracedecay_agent_hosts::host_io(),
        &home,
        &profile_root,
        &project_root,
    );
    for error in &receipt.errors {
        tracing::warn!(%error, "managed skill deployment failed");
    }
    for report in receipt.exports.iter() {
        if let Some(error) = &report.error {
            tracing::warn!(agent = %report.agent, %error, "managed skill export failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    use super::{
        ProfileResetDecision, RefreshPolicy, ReinstallOutcome, current_tracedecay_exe_from,
        install_pass_covers_tracked_agents, partition_reinstall_results, post_update_binary,
        post_update_binary_from, prepare_post_update_lease, profile_reset_decision,
        restart_daemon_service_with, run_install_then_refresh,
    };
    use crate::upgrade::UpgradeOutcome;
    use tempfile::TempDir;
    use tracedecay_daemon_control as daemon_control;

    /// Only the typed reset state authorizes deleting profile data after an
    /// update; an opening profile is left alone and every other failure is
    /// reported without acting.
    #[test]
    fn post_update_profile_reset_acts_only_on_the_typed_reset_state() {
        assert_eq!(
            profile_reset_decision(Ok(serde_json::json!({ "projects": [] }))),
            ProfileResetDecision::Opens
        );
        assert_eq!(
            profile_reset_decision(Err(
                tracedecay_domain::errors::TraceDecayError::reset_required(
                    "session temporal",
                    "published v3 shape",
                )
            )),
            ProfileResetDecision::Reset {
                authority: "session temporal".to_owned(),
                reason: "published v3 shape".to_owned(),
            }
        );
        assert_eq!(
            profile_reset_decision(Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "daemon tool call failed: storage unavailable".to_owned(),
            })),
            ProfileResetDecision::Undecided(
                "config error: daemon tool call failed: storage unavailable".to_owned()
            )
        );
    }

    #[test]
    fn daemon_restart_quiesces_service_before_acquiring_exclusive_lease() {
        let order = RefCell::new(Vec::new());
        let result = restart_daemon_service_with(
            || {
                order.borrow_mut().push("quiesce");
                Ok(daemon_control::DaemonServiceState::RunningEnabled)
            },
            || {
                assert_eq!(order.borrow().as_slice(), ["quiesce"]);
                order.borrow_mut().push("acquire");
                Ok(())
            },
            |state| {
                assert_eq!(state, daemon_control::DaemonServiceState::RunningEnabled);
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
        let order = RefCell::new(Vec::new());
        let result = restart_daemon_service_with(
            || {
                order.borrow_mut().push("quiesce");
                Ok(daemon_control::DaemonServiceState::StoppedEnabled)
            },
            || {
                order.borrow_mut().push("acquire");
                Ok(())
            },
            |state| {
                order.borrow_mut().push("refresh");
                assert_eq!(state, daemon_control::DaemonServiceState::RunningEnabled);
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
    fn daemon_restart_restores_running_service_when_exclusive_lease_acquisition_fails() {
        let order = RefCell::new(Vec::new());
        let result = restart_daemon_service_with(
            || {
                order.borrow_mut().push("quiesce");
                Ok(daemon_control::DaemonServiceState::RunningEnabled)
            },
            || -> tracedecay_domain::errors::Result<()> {
                order.borrow_mut().push("acquire");
                Err(config_err("lifecycle lease busy"))
            },
            |_| {
                order.borrow_mut().push("refresh");
                Ok(None)
            },
            |state| {
                assert_eq!(state, daemon_control::DaemonServiceState::RunningEnabled);
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
        let lease = tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_for_profile(
            profile.path(),
            "update",
        )
        .unwrap();

        let held = prepare_post_update_lease(lease);
        let reacquired = tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_for_profile(
            profile.path(),
            "post-update",
        );

        #[cfg(windows)]
        assert!(reacquired.is_ok());
        #[cfg(not(windows))]
        assert!(reacquired.is_err());
        drop(held);
    }

    use tracedecay_session_memory::user_config::UserConfig;

    fn config_err(message: &str) -> tracedecay_domain::errors::TraceDecayError {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: message.to_string(),
        }
    }

    fn ok(
        id: &str,
    ) -> (
        String,
        tracedecay_domain::errors::Result<crate::agent_cmd::AgentReinstallOutcome>,
    ) {
        (
            id.to_string(),
            Ok(crate::agent_cmd::AgentReinstallOutcome::Installed),
        )
    }

    fn err(
        id: &str,
    ) -> (
        String,
        tracedecay_domain::errors::Result<crate::agent_cmd::AgentReinstallOutcome>,
    ) {
        (id.to_string(), Err(config_err("install failed")))
    }

    #[test]
    fn generated_artifact_bin_ignores_non_tracedecay_test_exe() {
        let current = Path::new("/repo/target/debug/deps/agent_suite-abc123");

        assert_eq!(current_tracedecay_exe_from(Some(current)), None);
    }

    #[test]
    fn partition_collects_only_failed_ids_in_order() {
        match partition_reinstall_results(vec![ok("claude"), err("cursor"), err("copilot")]) {
            ReinstallOutcome::PartialFailure { failed } => {
                assert_eq!(
                    failed,
                    vec![
                        "cursor: config error: install failed".to_string(),
                        "copilot: config error: install failed".to_string(),
                    ],
                );
            }
            ReinstallOutcome::AllOk => panic!("expected a partial failure"),
        }
    }

    /// An unresolvable tracked id (renamed/removed by a later release, or a
    /// typo in `installed_agents`) must be SKIPPED, not treated as a failure.
    /// otherwise it gates marker advancement forever and wedges explicit
    /// post-update maintenance into an infinite reinstall loop. The reinstall
    /// pass drops it from the results entirely, so an otherwise-empty pass is
    /// AllOk and the markers advance.
    #[tokio::test]
    async fn reinstall_agent_integrations_skips_unknown_ids()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let home = TempDir::new()?;
        let lease_root = TempDir::new()?;
        let lifecycle_lease =
            tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_for_profile(
                lease_root.path(),
                "post-update-test",
            )?;
        let results = crate::agent_cmd::reinstall_agent_integrations_under_lease(
            &["unknown-agent".to_string()],
            home.path(),
            "tracedecay",
            &lifecycle_lease,
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

    /// Markers advance only when every tracked agent reinstalled (AllOk).
    #[test]
    fn markers_advance_only_on_all_ok() {
        let running = "9.9.9";

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

        let mut config = UserConfig {
            installed_agents: vec!["claude".to_string()],
            previous_version: "9.0.0".to_string(),
            ..UserConfig::default()
        };
        match partition_reinstall_results(vec![err("claude")]) {
            ReinstallOutcome::PartialFailure { .. } => {}
            ReinstallOutcome::AllOk => panic!("expected PartialFailure"),
        }
        assert_eq!(config.previous_version, "9.0.0");
        assert!(config.last_installed_version.is_empty());
        assert!(config.mark_version_installed(running));
    }

    /// An install that only touched its selection delta must not record a full
    /// refresh, because untouched tracked agents still carry the prior
    /// lifecycle state.
    #[test]
    fn an_install_pass_records_completion_only_on_full_coverage() {
        let running = "9.9.9";
        let armed = |agents: &[&str]| UserConfig {
            installed_agents: agents.iter().map(ToString::to_string).collect(),
            previous_version: "9.0.0".to_string(),
            ..UserConfig::default()
        };
        let refreshed = |ids: &[&str]| -> std::collections::BTreeSet<String> {
            ids.iter().map(ToString::to_string).collect()
        };

        let mut config = armed(&["claude"]);
        assert!(install_pass_covers_tracked_agents(
            &config.installed_agents,
            &refreshed(&["claude"]),
        ));
        assert!(config.mark_version_installed(running));
        assert_eq!(config.previous_version, running);
        assert_eq!(config.last_installed_version, running);

        let mut config = armed(&["claude", "cursor"]);
        assert!(!install_pass_covers_tracked_agents(
            &config.installed_agents,
            &refreshed(&["claude"]),
        ));
        config.last_installed_version = running.to_string();
        assert_eq!(config.previous_version, "9.0.0");
        assert_eq!(config.last_installed_version, running);

        assert!(install_pass_covers_tracked_agents(&[], &refreshed(&[])));
    }

    fn record_upgrade<'a>(
        calls: &'a RefCell<Vec<&'static str>>,
        label: &'static str,
        result: tracedecay_domain::errors::Result<UpgradeOutcome>,
    ) -> impl FnOnce() -> tracedecay_domain::errors::Result<UpgradeOutcome> + 'a {
        move || {
            calls.borrow_mut().push(label);
            result
        }
    }

    fn record_post_update<'a>(
        calls: &'a RefCell<Vec<&'static str>>,
        label: &'static str,
        seen_binary: &'a RefCell<Option<Option<PathBuf>>>,
        result: tracedecay_domain::errors::Result<()>,
    ) -> impl FnOnce(Option<&Path>) -> tracedecay_domain::errors::Result<()> + 'a {
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

        let installed_version = run_install_then_refresh(
            RefreshPolicy::Always,
            record_upgrade(&calls, "upgrade", Ok(UpgradeOutcome::AlreadyCurrent)),
            record_post_update(&calls, "post-update", &seen_binary, Ok(())),
        )
        .expect("update steps should succeed");

        assert_eq!(calls.into_inner(), vec!["upgrade", "post-update"]);
        assert_eq!(seen_binary.into_inner(), Some(None));
        assert_eq!(
            installed_version, None,
            "no install must leave daemon restore validating the running version"
        );
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
                    version: None,
                }),
            ),
            record_post_update(&calls, "post-update", &seen_binary, Ok(())),
        )
        .expect("upgrade steps should succeed");

        assert_eq!(calls.into_inner(), vec!["upgrade", "post-update"]);
        assert_eq!(seen_binary.into_inner(), Some(Some(installed)));
    }

    /// An upgrade that replaced the binary (old ≠ new) must hand the NEW
    /// version to the surrounding maintenance window, so the daemon restore
    /// validates the binary it actually starts rather than the pre-upgrade
    /// one it quiesced.
    #[test]
    fn update_policy_reports_installed_version_for_restore_validation() {
        let calls = RefCell::new(Vec::new());
        let seen_binary = RefCell::new(None);

        let installed_version = run_install_then_refresh(
            RefreshPolicy::Always,
            record_upgrade(
                &calls,
                "upgrade",
                Ok(UpgradeOutcome::Installed {
                    binary: None,
                    version: Some("9.9.9".to_string()),
                }),
            ),
            record_post_update(&calls, "post-update", &seen_binary, Ok(())),
        )
        .expect("update steps should succeed");

        assert_eq!(installed_version.as_deref(), Some("9.9.9"));
    }

    #[test]
    fn upgrade_policy_skips_post_update_when_already_current() {
        let calls = RefCell::new(Vec::new());
        let seen_binary = RefCell::new(None);

        let installed_version = run_install_then_refresh(
            RefreshPolicy::AfterInstall,
            record_upgrade(&calls, "upgrade", Ok(UpgradeOutcome::AlreadyCurrent)),
            record_post_update(&calls, "post-update", &seen_binary, Ok(())),
        )
        .expect("an up-to-date upgrade should stay a successful no-op");

        assert_eq!(calls.into_inner(), vec!["upgrade"]);
        assert_eq!(seen_binary.into_inner(), None);
        assert_eq!(
            installed_version, None,
            "no install must leave daemon restore validating the running version"
        );
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
                Ok(UpgradeOutcome::Installed {
                    binary: None,
                    version: Some("9.9.9".to_string()),
                }),
            ),
            record_post_update(
                &calls,
                "post-update",
                &seen_binary,
                Err(config_err("plugin refresh failed")),
            ),
        );

        // The warn-only refresh failure neither fails the upgrade nor erases
        // the installed version: the new binary is on disk, so the daemon
        // restore must still validate it.
        assert_eq!(
            result.expect("tolerated refresh failure").as_deref(),
            Some("9.9.9")
        );
        assert_eq!(calls.into_inner(), vec!["upgrade", "post-update"]);
    }

    #[test]
    fn post_update_binary_prefers_the_freshly_installed_path() {
        let temp = tempfile::tempdir().expect("tempdir should exist");
        let installed = temp.path().join("tracedecay");
        std::fs::write(&installed, b"new-binary").expect("binary should be writable");

        let resolved = post_update_binary(Some(&installed)).expect("installed path should resolve");

        assert_eq!(resolved, tracedecay_domain::forward_slash_path(&installed));
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
        // branch proves the dangling path was rejected, which one runs
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
