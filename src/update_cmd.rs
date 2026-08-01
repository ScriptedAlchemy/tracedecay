//! The `upgrade` / `update` / `post-update` / `update-plugin` flow: binary
//! upgrade via subprocess re-exec, generated-plugin refresh, daemon service
//! refresh, the post-update health pass, and the full tracked-agent
//! reinstall that keeps config-managed integrations in sync.
//!
//! The post-update pass refreshes every already-configured agent integration
//! (re-running `install` + `post_install` for each tracked agent), so a
//! separate `tracedecay reinstall` is not needed after an upgrade. Pass
//! `--no-reinstall` to skip that agent-integration refresh.
//!
//! Hosts that own a canonical first-party component set are refreshed only by
//! that tracked-agent pass, which routes them through the receipt-backed
//! component-set transaction. The generated-plugin refresh deliberately skips
//! them: it is not part of the transaction, so rewriting a receipt-owned
//! artifact there would leave the receipt stale until the next reseal.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::cli::PostUpdateMode;
use tracedecay::upgrade::UpgradeOutcome;
use tracedecay::user_config::UserConfig;

// Exceeds the daemon's sequential 15s client drain, 2s task abort, and 45s
// server-shutdown bounds with margin for service-manager/process-exit latency.
const DAEMON_RESTART_LEASE_TIMEOUT: Duration = Duration::from_secs(90);

pub(crate) async fn refresh_generated_plugins() -> tracedecay::errors::Result<()> {
    let home = tracedecay_home_dir()?;
    let tracedecay_bin = tracedecay_bin_for_generated_artifacts()?;
    refresh_generated_plugins_at(
        tracedecay::agents::all_integrations(),
        &home,
        &tracedecay_bin,
    )
}

/// Whether a host owns a canonical first-party component set.
///
/// For those hosts the receipt-backed component-set transaction is the sole
/// writer of the deployed artifacts: `reinstall_agent_integrations` routes them
/// through `apply_default_canonical_component_set` and never calls
/// `install` / `update_plugin`. A second writer outside that transaction (this
/// generated-artifact refresh) rewrote the very files the receipt claims,
/// before the transaction resealed them, so every version bump left the
/// receipt stale and Doctor reported a component-ownership conflict.
///
/// `integration_id_for_host` is many-to-one (CursorCloud and CursorDesktop both
/// map to `cursor`), so an id counts as canonical when ANY host behind it has a
/// non-empty default component set — the transaction owns that id's artifacts.
fn host_owns_canonical_component_set(agent_id: &str) -> bool {
    tracedecay::agents::host_bundle_v2::stock_host_kinds()
        .into_iter()
        .any(|host| {
            tracedecay::agents::integration_id_for_host(host) == agent_id
                && !tracedecay::agents::host_bundle_registry::default_components(host).is_empty()
        })
}

fn refresh_generated_plugins_at(
    integrations: Vec<Box<dyn tracedecay::agents::AgentIntegration>>,
    home: &Path,
    tracedecay_bin: &str,
) -> tracedecay::errors::Result<()> {
    eprintln!(
        "Refreshing tracedecay-generated plugin artifacts (supported user configs are preserved)"
    );

    // Detection-driven, not `installed_agents`-driven: each integration
    // decides whether generated artifacts exist on this machine, so stale
    // tracking state can neither skip a real install nor install anywhere new.
    let mut refreshed_any = false;
    let mut failures: Vec<String> = Vec::new();
    for ag in integrations {
        if host_owns_canonical_component_set(ag.id()) {
            eprintln!(
                "  \x1b[2m·\x1b[0m {}: owned by the receipt-backed component-set transaction; \
                 skipped here so the receipt is not left stale",
                ag.id()
            );
            continue;
        }
        let hermes_was_installed = ag.id() == "hermes" && ag.has_tracedecay(home);
        // Generated-plugin refresh never rewrites Hermes profile config, so it
        // must not be blocked by an unresolved historical session migration.
        // Migration remains mandatory on install/uninstall paths that can
        // remove a legacy project pin.
        let ctx = tracedecay::agents::InstallContext {
            home: home.to_path_buf(),
            tracedecay_bin: tracedecay_bin.to_string(),
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
            Ok(tracedecay::agents::UpdatePluginOutcome::DeferredUserAction(deferred)) => {
                refreshed_any = true;
                eprintln!(
                    "  \x1b[33mwarning:\x1b[0m {} plugin activation deferred: {}",
                    ag.id(),
                    deferred.remediation
                );
                for path in deferred.staged_paths {
                    eprintln!("    staged: {}", path.display());
                }
            }
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
    if !cfg!(any(target_os = "linux", target_os = "macos", windows)) {
        return Ok(None);
    }
    let tracedecay_bin = tracedecay::agents::which_tracedecay_path().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "tracedecay not found on PATH".to_string(),
        }
    })?;
    let spec = tracedecay::daemon::service_spec(tracedecay_bin, None)?;
    refresh_daemon_service_with_spec(previous_state, &spec)
}

fn refresh_daemon_service_with_spec(
    previous_state: tracedecay::daemon::DaemonServiceState,
    spec: &tracedecay::daemon::DaemonServiceSpec,
) -> tracedecay::errors::Result<Option<(PathBuf, PathBuf)>> {
    let socket_path = tracedecay::daemon::installed_service_socket_path()?
        .unwrap_or_else(|| spec.socket_path.clone());
    Ok(
        tracedecay::daemon::refresh_installed_service_under_lease_with_state(spec, previous_state)?
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
    previous_state: tracedecay::daemon::DaemonServiceState,
) -> tracedecay::errors::Result<()> {
    match refresh_daemon_service(previous_state)? {
        Some((service_path, socket_path)) => {
            eprintln!(
                "\x1b[32m✔\x1b[0m Daemon service refreshed at {}",
                service_path.display()
            );
            print_daemon_transport_location(&socket_path);
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

#[cfg(test)]
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
    let previous_state = quiesce()?;
    let _lifecycle_lease = match acquire() {
        Ok(lease) => lease,
        Err(acquire_error) => {
            if matches!(
                previous_state,
                tracedecay::daemon::DaemonServiceState::RunningEnabled
                    | tracedecay::daemon::DaemonServiceState::RunningDisabled
            ) && let Err(restore_error) = restore(previous_state)
            {
                return Err(tracedecay::errors::TraceDecayError::Config {
                    message: format!(
                        "{acquire_error}; additionally failed to restore the managed daemon service: {restore_error}"
                    ),
                });
            }
            return Err(acquire_error);
        }
    };
    refresh(tracedecay::daemon::DaemonServiceState::RunningEnabled)
}

fn refresh_forward_only_daemon_service_after_update(
    previous_state: tracedecay::daemon::DaemonServiceState,
    spec: &tracedecay::daemon::DaemonServiceSpec,
) -> tracedecay::errors::Result<()> {
    match refresh_daemon_service_with_spec(previous_state, spec)? {
        Some((service_path, socket_path)) => {
            eprintln!(
                "\x1b[32m✔\x1b[0m Forward-only daemon service refreshed at {}",
                service_path.display()
            );
            print_daemon_transport_location(&socket_path);
            Ok(())
        }
        None if tracedecay::daemon::daemon_reachable() => {
            Err(tracedecay::errors::TraceDecayError::Config {
                message:
                    "forward-only post-update found a reachable unmanaged daemon after maintenance"
                        .to_string(),
            })
        }
        None => {
            eprintln!("TraceDecay daemon service is not installed; skipping daemon restart.");
            Ok(())
        }
    }
}

pub(crate) fn restart_daemon_service() -> tracedecay::errors::Result<()> {
    let guard = tracedecay::daemon::QuiescedDaemonLifecycle::acquire_with_timeout(
        "daemon restart",
        DAEMON_RESTART_LEASE_TIMEOUT,
    )?;
    let (stopped_state, desired_state) = match guard.previous_state() {
        tracedecay::daemon::DaemonServiceState::RunningEnabled
        | tracedecay::daemon::DaemonServiceState::StoppedEnabled => (
            tracedecay::daemon::DaemonServiceState::StoppedEnabled,
            tracedecay::daemon::DaemonServiceState::RunningEnabled,
        ),
        tracedecay::daemon::DaemonServiceState::RunningDisabled
        | tracedecay::daemon::DaemonServiceState::StoppedDisabled => (
            tracedecay::daemon::DaemonServiceState::StoppedDisabled,
            tracedecay::daemon::DaemonServiceState::RunningDisabled,
        ),
        tracedecay::daemon::DaemonServiceState::Missing => {
            return Err(tracedecay::errors::TraceDecayError::Config {
                message: "no TraceDecay daemon service is installed — restart your `tracedecay daemon run` process manually, or run `tracedecay daemon install-service` to manage it as a service".to_string(),
            });
        }
        tracedecay::daemon::DaemonServiceState::Masked => {
            return Err(tracedecay::errors::TraceDecayError::Config {
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
    run_update_flow("update", RefreshPolicy::Always, no_heal, no_reinstall)
}

pub(crate) fn run_upgrade_command(
    no_heal: bool,
    no_reinstall: bool,
) -> tracedecay::errors::Result<()> {
    run_update_flow(
        "upgrade",
        RefreshPolicy::AfterInstall,
        no_heal,
        no_reinstall,
    )
}

fn run_update_flow(
    operation: &str,
    refresh_policy: RefreshPolicy,
    no_heal: bool,
    no_reinstall: bool,
) -> tracedecay::errors::Result<()> {
    tracedecay::daemon::with_exclusive_maintenance_window(operation, |lease_token| {
        run_install_then_refresh(refresh_policy, tracedecay::upgrade::run_upgrade, |binary| {
            run_post_update_subcommand(no_heal, no_reinstall, binary, lease_token)
        })
    })
}

fn combine_operation_and_restore<T>(
    operation: &str,
    operation_result: tracedecay::errors::Result<T>,
    restore_result: tracedecay::errors::Result<()>,
) -> tracedecay::errors::Result<T> {
    match (operation_result, restore_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(operation_error), Err(restore_error)) => {
            Err(tracedecay::errors::TraceDecayError::Config {
                message: format!(
                    "{operation} failed: {operation_error}; daemon state restoration also failed: {restore_error}"
                ),
            })
        }
    }
}

pub(crate) async fn run_post_update_command(
    no_heal: bool,
    no_reinstall: bool,
    lifecycle_lease_token: Option<&str>,
    strict: bool,
    mode: PostUpdateMode,
) -> tracedecay::errors::Result<()> {
    if mode == PostUpdateMode::DogfoodRecoverInactive {
        if lifecycle_lease_token.is_some() {
            return Err(tracedecay::errors::TraceDecayError::Config {
                message: "dogfood inactive recovery cannot inherit an updater lease".to_string(),
            });
        }
        let current_exe = std::env::current_exe().map_err(|error| {
            tracedecay::errors::TraceDecayError::Config {
                message: format!("could not resolve the dogfood recovery executable: {error}"),
            }
        })?;
        let spec = tracedecay::daemon::service_spec(current_exe, None)?;
        let service_path = tracedecay::daemon::enforce_forward_only_service_recovery(&spec)?;
        if let Some(path) = service_path {
            eprintln!(
                "Forward-only recovery retained the new inactive service unit at {}",
                path.display()
            );
        }
        return Ok(());
    }

    if mode == PostUpdateMode::DogfoodForwardOnly {
        if lifecycle_lease_token.is_some() {
            return Err(tracedecay::errors::TraceDecayError::Config {
                message: "dogfood forward-only post-update cannot inherit an updater lease"
                    .to_string(),
            });
        }
        if !strict {
            return Err(tracedecay::errors::TraceDecayError::Config {
                message: "dogfood forward-only post-update requires --strict".to_string(),
            });
        }
        return run_forward_only_post_update_command(no_heal, no_reinstall).await;
    }

    if let Some(token) = lifecycle_lease_token {
        let lifecycle_lease = tracedecay::lifecycle_lease::acquire_exclusive_or_inherited(
            "post-update",
            Some(token),
        )?;
        return run_post_update_tasks(no_heal, no_reinstall, strict, &lifecycle_lease).await;
    }

    let guard = tracedecay::daemon::QuiescedDaemonLifecycle::acquire("post-update")?;
    let previous_daemon_state = guard.previous_state();
    let operation_result = match guard.lifecycle_lease() {
        Ok(lifecycle_lease) => {
            run_post_update_tasks(no_heal, no_reinstall, strict, lifecycle_lease).await
        }
        Err(error) => Err(error),
    };
    let restore_result = guard.finish();
    let readiness_result = if strict {
        tracedecay::daemon::wait_for_installed_service_state(previous_daemon_state)
    } else {
        Ok(())
    };
    let restoration_result = combine_operation_and_restore(
        "post-update daemon restoration",
        restore_result,
        readiness_result,
    );
    combine_operation_and_restore("post-update", operation_result, restoration_result)
}

async fn run_forward_only_post_update_command(
    no_heal: bool,
    no_reinstall: bool,
) -> tracedecay::errors::Result<()> {
    let current_exe =
        std::env::current_exe().map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!("could not resolve the dogfood post-update executable: {error}"),
        })?;
    let spec = tracedecay::daemon::service_spec(current_exe, None)?;
    let guard = tracedecay::daemon::QuiescedDaemonLifecycle::acquire_forward_only_with_timeout(
        "dogfood forward-only post-update",
        &spec,
        DAEMON_RESTART_LEASE_TIMEOUT,
    )
    .map_err(|error| forward_only_failure(&spec, error))?;
    let previous_daemon_state = guard.previous_state();
    let target_daemon_state = dogfood_forward_only_target_state(previous_daemon_state);
    let operation_result = match guard.lifecycle_lease() {
        Ok(lifecycle_lease) => {
            run_forward_only_post_update_tasks(
                no_heal,
                no_reinstall,
                lifecycle_lease,
                target_daemon_state,
                &spec,
            )
            .await
        }
        Err(error) => Err(error),
    };
    guard.finish_without_restore();

    if let Err(error) = operation_result {
        return Err(forward_only_failure(&spec, error));
    }
    let stage_started = Instant::now();
    if let Err(error) = tracedecay::daemon::wait_for_installed_service_state(target_daemon_state) {
        return Err(forward_only_failure(&spec, error));
    }
    report_dogfood_stage("daemon-service-ready", stage_started);
    let stage_started = Instant::now();
    if let Err(error) = verify_forward_only_binary_version(&spec.tracedecay_bin) {
        return Err(forward_only_failure(&spec, error));
    }
    report_dogfood_stage("installed-version-check", stage_started);
    let stage_started = Instant::now();
    if let Err(error) =
        tracedecay::doctor::wait_for_daemon_startup_health(startup_health_timeout()).await
    {
        return Err(forward_only_failure(&spec, error));
    }
    report_dogfood_stage("daemon-convergence", stage_started);
    let stage_started = Instant::now();
    let result = tracedecay::doctor::run_doctor(None)
        .await
        .map_err(|error| forward_only_failure(&spec, error));
    report_dogfood_stage("doctor", stage_started);
    result
}

fn report_dogfood_stage(stage: &str, started: Instant) {
    eprintln!(
        "[dogfood timing] stage={stage} elapsed_ms={}",
        started.elapsed().as_millis()
    );
}

/// How long post-update Doctor validation waits for daemon schema migration and
/// compatibility projections to converge. Large profiles legitimately take more
/// than a few minutes on first start after a schema change (observed: a ~90 GB
/// profile converging well past the previous 3-minute deadline while perfectly
/// healthy), and an expired deadline here strands an otherwise-successful
/// update with the managed daemon disabled. Overridable for constrained
/// environments via TRACEDECAY_STARTUP_HEALTH_TIMEOUT_SECS.
fn startup_health_timeout() -> std::time::Duration {
    std::env::var("TRACEDECAY_STARTUP_HEALTH_TIMEOUT_SECS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map(std::time::Duration::from_secs)
        .unwrap_or(std::time::Duration::from_mins(30))
}

fn dogfood_forward_only_target_state(
    _previous_state: tracedecay::daemon::DaemonServiceState,
) -> tracedecay::daemon::DaemonServiceState {
    tracedecay::daemon::DaemonServiceState::RunningEnabled
}

async fn run_forward_only_post_update_tasks(
    no_heal: bool,
    no_reinstall: bool,
    lifecycle_lease: &tracedecay::lifecycle_lease::LifecycleLease,
    target_daemon_state: tracedecay::daemon::DaemonServiceState,
    spec: &tracedecay::daemon::DaemonServiceSpec,
) -> tracedecay::errors::Result<()> {
    eprintln!("\nPreparing forward-only dogfood maintenance.");
    tracedecay::daemon::verify_installed_service_quiesced_under_lease()?;

    let stage_started = Instant::now();
    run_post_update_mutations(no_heal, no_reinstall, true, lifecycle_lease).await?;
    report_dogfood_stage("post-update-integrations", stage_started);
    let stage_started = Instant::now();
    let result = refresh_forward_only_daemon_service_after_update(target_daemon_state, spec);
    report_dogfood_stage("daemon-service-refresh", stage_started);
    result
}

fn verify_forward_only_binary_version(binary: &Path) -> tracedecay::errors::Result<()> {
    let output = std::process::Command::new(binary)
        .arg("--version")
        .output()
        .map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "could not execute retained dogfood binary '{}': {error}",
                binary.display()
            ),
        })?;
    let version = String::from_utf8_lossy(&output.stdout);
    let expected = format!("tracedecay {}", tracedecay::version::build_version());
    if output.status.success() && version.trim() == expected {
        return Ok(());
    }
    Err(tracedecay::errors::TraceDecayError::Config {
        message: format!(
            "retained dogfood binary version check failed for '{}': status {}, output {:?}, expected {:?}",
            binary.display(),
            output.status,
            version.trim(),
            expected,
        ),
    })
}

fn forward_only_failure(
    spec: &tracedecay::daemon::DaemonServiceSpec,
    operation_error: tracedecay::errors::TraceDecayError,
) -> tracedecay::errors::TraceDecayError {
    match tracedecay::daemon::enforce_forward_only_service_recovery(spec) {
        Ok(service_path) => tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "dogfood forward-only post-update failed: {operation_error}; managed daemon is inactive; retained new binary '{}'{}; recover only with this binary or a newer compatible build",
                spec.tracedecay_bin.display(),
                service_path.map_or_else(
                    || ", no managed service unit was installed".to_string(),
                    |path| format!(" and new service unit '{}'", path.display())
                ),
            ),
        },
        Err(recovery_error) => tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "dogfood forward-only post-update failed: {operation_error}; retained new binary '{}', but managed-daemon inactivity could not be proven: {recovery_error}; do not run an older binary",
                spec.tracedecay_bin.display(),
            ),
        },
    }
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

// Windows must drop the lease before replacing a running executable, while
// other platforms retain it through the child handoff. Production drives this
// contract through `daemon::with_exclusive_maintenance_window`; this mirror
// keeps the platform contract under test.
#[cfg(test)]
#[allow(clippy::unnecessary_wraps)]
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
    results: Vec<(
        String,
        tracedecay::errors::Result<crate::agent_cmd::AgentReinstallOutcome>,
    )>,
) -> ReinstallOutcome {
    // Carry the reason, not just the name: a swallowed error here left
    // `dogfood` reporting "failed for: claude, cursor, hermes, kimi" with no
    // way to learn why short of reading the installer source. Matches the
    // existing "<environment>: ..." entry format used below.
    let mut failed = Vec::new();
    for (id, result) in results {
        match result {
            Ok(crate::agent_cmd::AgentReinstallOutcome::Installed) => {}
            Ok(crate::agent_cmd::AgentReinstallOutcome::DeferredUserAction(deferred)) => {
                eprintln!(
                    "  \x1b[33mwarning:\x1b[0m {id} reinstall deferred: {}",
                    deferred.remediation
                );
                for path in deferred.staged_paths {
                    eprintln!("    staged: {}", path.display());
                }
            }
            Err(error) => failed.push(format!("{id}: {error}")),
        }
    }
    if failed.is_empty() {
        ReinstallOutcome::AllOk
    } else {
        ReinstallOutcome::PartialFailure { failed }
    }
}

/// Records a completed tracked-agent reinstall pass by advancing BOTH version
/// markers, persisting the config only when something actually changed.
///
/// This is the one place any completed pass may record its version, and it is
/// deliberately not open-coded. `previous_version` — not
/// `last_installed_version` — is what arms the startup silent reinstall
/// (`silent_reinstall_action`), so a pass that advanced only
/// `last_installed_version` left the arming intact and repeated the whole
/// reinstall on the very next ordinary command. Callers that treat a failed
/// save as advisory report the error; `tracedecay reinstall` surfaces it,
/// because an unsaved marker means the arming it just cleared is still on disk.
pub(crate) fn record_completed_reinstall_pass(
    config: &mut UserConfig,
) -> tracedecay::errors::Result<()> {
    if config.mark_version_installed(env!("CARGO_PKG_VERSION")) {
        config
            .save()
            .map_err(|err| tracedecay::errors::TraceDecayError::Config {
                message: format!("could not save tracedecay config: {err}"),
            })?;
    }
    Ok(())
}

/// Whether an explicit `tracedecay install` pass amounted to a full
/// tracked-agent refresh and may therefore call
/// [`record_completed_reinstall_pass`].
///
/// The install flow only (re)installs its selection delta — agents that were
/// already tracked are left untouched. After an upgrade those untouched
/// agents still carry the previous binary's integration, so the pass may
/// only disarm the startup silent reinstall when every agent that remains
/// tracked was actually installed by this very pass. An empty tracked set is
/// trivially covered: there is nothing left to refresh.
pub(crate) fn install_pass_covers_tracked_agents(
    tracked: &[String],
    refreshed: &std::collections::BTreeSet<String>,
) -> bool {
    tracked.iter().all(|id| refreshed.contains(id))
}

fn reinstall_failure_result(failed: &[String], strict: bool) -> tracedecay::errors::Result<()> {
    if strict {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "dogfood agent integration refresh failed for: {}",
                failed.join(", ")
            ),
        });
    }
    Ok(())
}

/// Only warnings whose [`StoreDurabilityClass`](tracedecay::migrate::durability::StoreDurabilityClass)
/// proves the underlying data `Durable` can fail a `--strict` post-update.
/// This is the fix for the diagnosed dogfood failure: mounting/migrating a
/// 15GB `sessions.db` got interrupted, and because every health-pass warning
/// used to be treated as fatal under `--strict`, that single advisory
/// warning about recoverable session data failed the whole upgrade and
/// disabled the daemon. See `tracedecay::doctor::heal` and
/// `tracedecay::migrate::durability` for the classification this consults.
fn health_pass_failure_result(
    report: &tracedecay::doctor::heal::HealthPassReport,
    strict: bool,
) -> tracedecay::errors::Result<()> {
    let blocking: Vec<&str> = report
        .warnings
        .iter()
        .filter(|warning| warning.blocks_strict_upgrade())
        .map(|warning| warning.message.as_str())
        .collect();
    if strict && !blocking.is_empty() {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "dogfood post-update health pass failed: {}",
                blocking.join("; ")
            ),
        });
    }
    Ok(())
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
    strict: bool,
    lifecycle_lease: &tracedecay::lifecycle_lease::LifecycleLease,
) -> tracedecay::errors::Result<()> {
    eprintln!("\nPreparing safe post-update maintenance.");
    eprintln!("  Waiting for TraceDecay writers to shut down cleanly — do not interrupt.");
    let previous_daemon_state =
        tracedecay::daemon::verify_installed_service_quiesced_under_lease()?;
    eprintln!("\x1b[32m✔\x1b[0m TraceDecay writers stopped; exclusive maintenance window active.");
    let mutation_result =
        run_post_update_mutations(no_heal, no_reinstall, strict, lifecycle_lease).await;
    let restart_result = refresh_daemon_service_after_update(previous_daemon_state);
    combine_operation_and_restore("post-update maintenance", mutation_result, restart_result)
}

async fn run_post_update_mutations(
    no_heal: bool,
    no_reinstall: bool,
    strict: bool,
    lifecycle_lease: &tracedecay::lifecycle_lease::LifecycleLease,
) -> tracedecay::errors::Result<()> {
    refresh_generated_plugins().await?;
    if no_heal {
        eprintln!("Skipping post-update health pass (--no-heal).");
    } else {
        let report =
            tracedecay::doctor::heal::run_post_update_health_pass_under_lease(lifecycle_lease)
                .await;
        health_pass_failure_result(&report, strict)?;
    }

    if no_reinstall {
        eprintln!("Skipping agent integration refresh (--no-reinstall).");
        // `--no-reinstall` is a durable opt-out for THIS version, not a
        // one-command deferral: advance the version markers so the startup
        // silent reinstall (`silent_reinstall_action`) does not immediately
        // undo the skip on the next ordinary command and reinstall everything
        // anyway. The next real upgrade re-arms the reinstall as usual.
        let mut config = UserConfig::load();
        if let Err(err) = record_completed_reinstall_pass(&mut config) {
            eprintln!("warning: {err}");
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
    if config.installed_agents.len() != before
        && let Err(err) = config.save()
    {
        eprintln!("warning: could not save tracedecay config: {err}");
    }
    if config.installed_agents.is_empty() {
        eprintln!("Refreshing agent integrations: nothing to refresh");
    } else {
        eprintln!(
            "Refreshing agent integrations: {}",
            config.installed_agents.join(", ")
        );
    }
    let reinstall_result =
        match reinstall_tracked_agents_under_lease(&config, lifecycle_lease).await {
            ReinstallOutcome::AllOk => {
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
                reinstall_failure_result(&failed, strict)
            }
        };
    reconcile_materialized_managed_skills_after_update();
    reinstall_result
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
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    #[cfg(unix)]
    use super::verify_forward_only_binary_version;
    use super::{
        RefreshPolicy, ReinstallOutcome, current_tracedecay_exe_from,
        dogfood_forward_only_target_state, health_pass_failure_result,
        host_owns_canonical_component_set, normalize_bin_path, partition_reinstall_results,
        post_update_binary, post_update_binary_from, prepare_post_update_lease,
        refresh_generated_plugins_at, reinstall_failure_result, restart_daemon_service_with,
        run_install_then_refresh,
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

    #[test]
    fn dogfood_forward_only_always_validates_a_running_enabled_daemon() {
        for previous in [
            tracedecay::daemon::DaemonServiceState::Missing,
            tracedecay::daemon::DaemonServiceState::StoppedDisabled,
            tracedecay::daemon::DaemonServiceState::StoppedEnabled,
            tracedecay::daemon::DaemonServiceState::RunningDisabled,
            tracedecay::daemon::DaemonServiceState::RunningEnabled,
        ] {
            assert_eq!(
                dogfood_forward_only_target_state(previous),
                tracedecay::daemon::DaemonServiceState::RunningEnabled
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn forward_only_version_probe_rejects_a_retained_wrong_version() {
        let dir = TempDir::new().expect("temp dir");
        let binary = dir.path().join("new binary with spaces");
        std::fs::write(&binary, "#!/bin/sh\nprintf 'tracedecay 0.0.0-wrong\\n'\n")
            .expect("fake binary");
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))
            .expect("binary permissions");

        let error = verify_forward_only_binary_version(&binary)
            .expect_err("wrong retained version must fail");

        assert!(error.to_string().contains("version check failed"));
        assert!(error.to_string().contains("0.0.0-wrong"));
    }
    use tracedecay::user_config::UserConfig;

    fn config_err(message: &str) -> tracedecay::errors::TraceDecayError {
        tracedecay::errors::TraceDecayError::Config {
            message: message.to_string(),
        }
    }

    fn ok(
        id: &str,
    ) -> (
        String,
        tracedecay::errors::Result<crate::agent_cmd::AgentReinstallOutcome>,
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
        tracedecay::errors::Result<crate::agent_cmd::AgentReinstallOutcome>,
    ) {
        (id.to_string(), Err(config_err("install failed")))
    }

    fn deferred(
        id: &str,
    ) -> (
        String,
        tracedecay::errors::Result<crate::agent_cmd::AgentReinstallOutcome>,
    ) {
        (
            id.to_string(),
            Ok(crate::agent_cmd::AgentReinstallOutcome::DeferredUserAction(
                tracedecay::agents::DeferredUserAction {
                    remediation: "run /plugins install staged-kimi".to_string(),
                    staged_paths: vec![PathBuf::from("staged-kimi")],
                },
            )),
        )
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
    fn partition_deferred_user_action_is_non_blocking() {
        assert!(matches!(
            partition_reinstall_results(vec![ok("claude"), deferred("kimi")]),
            ReinstallOutcome::AllOk
        ));
    }

    /// Kimi owns a canonical component set, so the receipt-backed transaction
    /// (`reinstall_agent_integrations` → `apply_default_canonical_component_set`)
    /// is its sole writer. The generated-artifact refresh must leave it alone —
    /// including its staging directory — and must still succeed rather than
    /// treating the skip as a failure that blocks maintenance.
    #[test]
    fn deferred_kimi_refresh_does_not_block_maintenance() {
        let home = TempDir::new().unwrap();
        let installed_path = home.path().join(".kimi-code/plugins/installed.json");
        std::fs::create_dir_all(installed_path.parent().unwrap()).unwrap();
        let original = br#"{"version":1,"plugins":[{"id":"tracedecay","enabled":false}]}
"#;
        std::fs::write(&installed_path, original).unwrap();

        let result = refresh_generated_plugins_at(
            vec![Box::new(tracedecay::agents::kimi::KimiIntegration)],
            home.path(),
            "new-tracedecay",
        );

        assert!(result.is_ok());
        assert_eq!(std::fs::read(installed_path).unwrap(), original);
        assert!(
            !home
                .path()
                .join(".tracedecay/host-bundle-stage/kimi/tracedecay/.kimi-plugin/plugin.json")
                .exists(),
            "the component-set transaction owns the Kimi staging bundle"
        );
    }

    /// Post-update writer ordering. Every host with a canonical component set
    /// is written exclusively by the receipt-backed transaction; a second
    /// writer running before the transaction reseals the receipt is exactly
    /// what left Cursor Core's receipt stale on every version bump and made
    /// Doctor report a component-ownership conflict.
    #[test]
    fn canonical_component_set_hosts_are_not_refreshed_by_a_second_writer() {
        for agent_id in [
            "claude", "codex", "cursor", "hermes", "kimi", "kiro", "opencode",
        ] {
            assert!(
                host_owns_canonical_component_set(agent_id),
                "{agent_id} owns a canonical component set"
            );
        }
        for agent_id in ["gemini", "copilot", "zed", "cline", "roo-code", "kilo"] {
            assert!(
                !host_owns_canonical_component_set(agent_id),
                "{agent_id} has no canonical component set and keeps the generated refresh"
            );
        }
    }

    /// Cursor's receipt-owned plugin bundle must not be rewritten outside the
    /// component-set transaction: `.cursor-plugin/plugin.json` carries the
    /// stamped manifest version and `hooks/hooks.json` bakes the resolved
    /// binary path, so a refresh here guarantees byte drift from the receipt.
    #[test]
    fn cursor_plugin_bundle_is_left_to_the_component_set_transaction() {
        let home = TempDir::new().unwrap();
        let manifest_path = home
            .path()
            .join(".cursor/plugins/local/tracedecay/.cursor-plugin/plugin.json");
        std::fs::create_dir_all(manifest_path.parent().unwrap()).unwrap();
        let receipt_owned = br#"{"name":"tracedecay","version":"0.0.0-receipt"}"#;
        std::fs::write(&manifest_path, receipt_owned).unwrap();

        let result = refresh_generated_plugins_at(
            vec![Box::new(tracedecay::agents::CursorIntegration)],
            home.path(),
            "new-tracedecay",
        );

        assert!(result.is_ok());
        assert_eq!(std::fs::read(&manifest_path).unwrap(), receipt_owned);
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
                assert_eq!(
                    failed,
                    vec!["cursor: config error: install failed".to_string()]
                );
            }
            ReinstallOutcome::AllOk => panic!("a real install() failure must gate markers"),
        }
    }

    #[test]
    fn only_strict_post_update_propagates_integration_failures() {
        let failed = vec!["claude".to_string(), "hermes".to_string()];

        assert!(reinstall_failure_result(&failed, false).is_ok());
        let error = reinstall_failure_result(&failed, true)
            .expect_err("strict dogfood must fail")
            .to_string();
        assert!(error.contains("claude, hermes"));
    }

    #[test]
    fn strict_post_update_propagates_health_pass_failures_only() {
        let failed = tracedecay::doctor::heal::HealthPassReport {
            warnings: vec![tracedecay::doctor::heal::HealthPassWarning::durable(
                "could not open the global DB",
            )],
            ..Default::default()
        };
        let advisory = tracedecay::doctor::heal::HealthPassReport {
            remaining_findings: vec!["stale non-temp registry row".to_string()],
            ..Default::default()
        };

        assert!(health_pass_failure_result(&failed, false).is_ok());
        let error = health_pass_failure_result(&failed, true)
            .expect_err("strict dogfood must fail on health-pass errors")
            .to_string();
        assert!(error.contains("could not open the global DB"));
        assert!(health_pass_failure_result(&advisory, true).is_ok());
    }

    /// The diagnosed bug: a warning about a store whose durability class is
    /// not `Durable` (e.g. a `sessions.db` mount/repair failure -- dominated
    /// by recoverable transcript/evidence data) must never fail `--strict`,
    /// even though it is still surfaced to the operator.
    #[test]
    fn strict_post_update_never_fails_on_non_durable_store_warnings() {
        let recoverable_only = tracedecay::doctor::heal::HealthPassReport {
            warnings: vec![tracedecay::doctor::heal::HealthPassWarning::about_store(
                "could not mount the current project session store for repair: interrupted",
                tracedecay::migrate::durability::StoreShardKind::ProjectSessions,
            )],
            ..Default::default()
        };

        assert!(health_pass_failure_result(&recoverable_only, false).is_ok());
        assert!(
            health_pass_failure_result(&recoverable_only, true).is_ok(),
            "a recoverable-store warning must never fail a strict post-update"
        );
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

    /// Defect: `tracedecay reinstall` recorded only `last_installed_version`,
    /// but `silent_reinstall_action` arms on `previous_version`. The explicit
    /// pass therefore left the startup silent reinstall armed and repeated the
    /// entire tracked-agent install on the very next ordinary command.
    #[test]
    fn an_explicit_reinstall_pass_disarms_the_startup_silent_reinstall() {
        let running = "9.9.9";
        let armed = || UserConfig {
            installed_agents: vec!["claude".to_string()],
            previous_version: "9.0.0".to_string(),
            ..UserConfig::default()
        };
        assert_eq!(
            crate::silent_reinstall_action(&armed(), running),
            crate::SilentReinstallAction::Reinstall,
            "an unrecorded minor upgrade arms the startup pass"
        );

        // The old reinstall tail: only `last_installed_version` advanced.
        let mut last_marker_only = armed();
        last_marker_only.last_installed_version = running.to_string();
        assert_eq!(
            crate::silent_reinstall_action(&last_marker_only, running),
            crate::SilentReinstallAction::Reinstall,
            "advancing only last_installed_version leaves the pass armed"
        );

        // The shared completion protocol advances both markers.
        let mut completed = armed();
        assert!(completed.mark_version_installed(running));
        assert_eq!(completed.previous_version, running);
        assert_eq!(completed.last_installed_version, running);
        assert_eq!(
            crate::silent_reinstall_action(&completed, running),
            crate::SilentReinstallAction::Nothing,
            "a completed explicit reinstall disarms the startup pass"
        );
    }

    /// Defect (sibling of the reinstall one): `tracedecay install` recorded
    /// only `last_installed_version`, leaving the startup pass armed even
    /// when the install had just refreshed every tracked agent. The converse
    /// matters too: an install that only touched its selection delta must
    /// NOT disarm the pass, because after an upgrade the untouched agents
    /// still carry the previous binary's integration.
    #[test]
    fn an_install_pass_disarms_the_silent_reinstall_only_on_full_coverage() {
        let running = "9.9.9";
        let armed = |agents: &[&str]| UserConfig {
            installed_agents: agents.iter().map(ToString::to_string).collect(),
            previous_version: "9.0.0".to_string(),
            ..UserConfig::default()
        };
        let refreshed = |ids: &[&str]| -> std::collections::BTreeSet<String> {
            ids.iter().map(ToString::to_string).collect()
        };

        // Full coverage: every tracked agent was installed by this pass, so
        // completing the shared protocol disarms the startup reinstall.
        let mut config = armed(&["claude"]);
        assert!(install_pass_covers_tracked_agents(
            &config.installed_agents,
            &refreshed(&["claude"]),
        ));
        assert!(config.mark_version_installed(running));
        assert_eq!(
            crate::silent_reinstall_action(&config, running),
            crate::SilentReinstallAction::Nothing,
            "a full-coverage install pass disarms the startup pass"
        );

        // Partial coverage: `cursor` stayed tracked but untouched, so the
        // old tail (last_installed_version only) must be kept and the
        // startup pass must stay armed to refresh it.
        let mut config = armed(&["claude", "cursor"]);
        assert!(!install_pass_covers_tracked_agents(
            &config.installed_agents,
            &refreshed(&["claude"]),
        ));
        config.last_installed_version = running.to_string();
        assert_eq!(
            crate::silent_reinstall_action(&config, running),
            crate::SilentReinstallAction::Reinstall,
            "a delta-only install pass leaves the startup pass armed"
        );

        // Nothing tracked: trivially covered, nothing left to refresh.
        assert!(install_pass_covers_tracked_agents(&[], &refreshed(&[])));
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
