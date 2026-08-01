use std::path::Path;
use std::process::Command;

use crate::errors::{Result, TraceDecayError};

use super::probe::{DaemonSocketState, daemon_socket_state};
use super::{DaemonServiceState, LAUNCHD_LABEL, tracedecay_data_dir, windows_task};

/// All variants exist on every platform so that `match` dispatch stays
/// exhaustive everywhere; `current()` is the only constructor and returns an
/// error on platforms without a supported service manager.
pub(super) enum ServiceRunner {
    Systemd,
    Launchd,
    WindowsTask,
}

impl ServiceRunner {
    pub(super) fn current() -> Result<Self> {
        if cfg!(target_os = "linux") {
            Ok(Self::Systemd)
        } else if cfg!(target_os = "macos") {
            Ok(Self::Launchd)
        } else if cfg!(windows) {
            Ok(Self::WindowsTask)
        } else {
            Err(unsupported_service_platform())
        }
    }

    pub(super) fn install(
        &self,
        service_path: &Path,
        start: bool,
        socket_path: &Path,
    ) -> Result<()> {
        match self {
            Self::Systemd => {
                if start {
                    run_systemctl(&["daemon-reload"])?;
                    run_systemctl(&["enable", "--now", super::super::SERVICE_NAME])?;
                }
                Ok(())
            }
            Self::Launchd => launchd_install(service_path, start, socket_path),
            Self::WindowsTask => windows_task::apply_state(if start {
                DaemonServiceState::RunningEnabled
            } else {
                DaemonServiceState::StoppedDisabled
            }),
        }
    }

    pub(super) fn refresh(
        &self,
        service_path: &Path,
        socket_path: &Path,
        previous_state: DaemonServiceState,
    ) -> Result<()> {
        match self {
            Self::Systemd => {
                run_systemctl(&["daemon-reload"])?;
                if previous_state.is_enabled() {
                    run_systemctl(&["enable", super::super::SERVICE_NAME])?;
                }
                if previous_state.is_running() {
                    run_systemctl(&["restart", super::super::SERVICE_NAME])?;
                }
                Ok(())
            }
            Self::Launchd if previous_state.is_running() => {
                launchd_refresh(service_path, socket_path)?;
                if !previous_state.is_enabled() {
                    run_launchctl(&["disable", &launchd_service_target()?])?;
                }
                Ok(())
            }
            Self::Launchd => Ok(()),
            Self::WindowsTask => windows_task::apply_state(previous_state),
        }
    }

    pub(super) fn service_state(&self, socket_path: &Path) -> Result<DaemonServiceState> {
        match self {
            Self::Systemd => {
                let running = Command::new("systemctl")
                    .args(["--user", "is-active", "--quiet", super::super::SERVICE_NAME])
                    .status()
                    .is_ok_and(|status| status.success());
                let enablement = Command::new("systemctl")
                    .args(["--user", "is-enabled", super::super::SERVICE_NAME])
                    .output()
                    .ok()
                    .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
                    .unwrap_or_default();
                if enablement.starts_with("masked") {
                    Ok(DaemonServiceState::Masked)
                } else if running && enablement.starts_with("enabled") {
                    Ok(DaemonServiceState::RunningEnabled)
                } else if running {
                    Ok(DaemonServiceState::RunningDisabled)
                } else if enablement.starts_with("enabled") {
                    Ok(DaemonServiceState::StoppedEnabled)
                } else {
                    Ok(DaemonServiceState::StoppedDisabled)
                }
            }
            Self::Launchd => {
                let running = matches!(
                    daemon_socket_state(socket_path),
                    DaemonSocketState::Connectable
                );
                let enabled = !launchd_service_is_disabled();
                Ok(match (running, enabled) {
                    (true, true) => DaemonServiceState::RunningEnabled,
                    (true, false) => DaemonServiceState::RunningDisabled,
                    (false, true) => DaemonServiceState::StoppedEnabled,
                    (false, false) => DaemonServiceState::StoppedDisabled,
                })
            }
            Self::WindowsTask => windows_task::service_state(),
        }
    }

    pub(super) fn before_uninstall(&self, stop: bool) -> Result<()> {
        match self {
            Self::Systemd => {
                if stop {
                    let _ = run_systemctl(&["disable", "--now", super::super::SERVICE_NAME]);
                }
                Ok(())
            }
            Self::Launchd => launchd_before_uninstall(stop),
            Self::WindowsTask if stop => windows_task::deactivate(),
            Self::WindowsTask => Ok(()),
        }
    }

    pub(super) fn start(&self, service_path: &Path, socket_path: &Path) -> Result<()> {
        match self {
            Self::Systemd => run_systemctl(&["start", super::super::SERVICE_NAME]),
            Self::Launchd => {
                let target = launchd_service_target()?;
                launchd_start_preserving_enablement(&target, service_path, socket_path)
            }
            Self::WindowsTask => windows_task::start(),
        }
    }

    pub(super) fn stop(&self) -> Result<()> {
        match self {
            Self::Systemd => run_systemctl(&["stop", super::super::SERVICE_NAME]),
            Self::Launchd => launchd_stop(),
            Self::WindowsTask => windows_task::stop(),
        }
    }

    pub(super) fn stop_for_update(&self) -> Result<()> {
        self.stop()
    }

    pub(super) fn deactivate_for_forward_recovery(&self) -> Result<()> {
        match self {
            Self::Systemd => match run_systemctl(&["disable", "--now", super::super::SERVICE_NAME])
            {
                Ok(()) => Ok(()),
                Err(primary_error) => {
                    let stop_result = run_systemctl(&["stop", super::super::SERVICE_NAME]);
                    let disable_result = run_systemctl(&["disable", super::super::SERVICE_NAME]);
                    match (stop_result, disable_result) {
                        (Ok(()), Ok(())) => Ok(()),
                        (stop, disable) => Err(TraceDecayError::Config {
                            message: format!(
                                "could not deactivate TraceDecay daemon for forward-only recovery: {primary_error}; fallback stop: {}; fallback disable: {}",
                                stop.err()
                                    .map_or_else(|| "ok".to_string(), |error| error.to_string()),
                                disable
                                    .err()
                                    .map_or_else(|| "ok".to_string(), |error| error.to_string()),
                            ),
                        }),
                    }
                }
            },
            Self::Launchd => launchd_before_uninstall(true),
            Self::WindowsTask => windows_task::deactivate(),
        }
    }

    pub(super) fn reload_forward_recovery_unit(&self, service_path: &Path) -> Result<()> {
        match self {
            Self::Systemd => run_systemctl(&["daemon-reload"]),
            Self::Launchd => {
                // launchd has no in-place equivalent to daemon-reload. The
                // durable plist becomes login/reboot authority before bootout;
                // bootout is the manager boundary that makes the old loaded
                // definition inactive.
                let _ = service_path;
                Ok(())
            }
            Self::WindowsTask => Ok(()),
        }
    }

    pub(super) fn restore_after_update(
        &self,
        service_path: &Path,
        socket_path: &Path,
        previous_state: DaemonServiceState,
    ) -> Result<()> {
        if !previous_state.is_running() {
            return Ok(());
        }
        match self {
            Self::Systemd => {
                run_systemctl(&["daemon-reload"])?;
                if previous_state.is_enabled() {
                    run_systemctl(&["enable", super::super::SERVICE_NAME])?;
                } else {
                    run_systemctl(&["disable", super::super::SERVICE_NAME])?;
                }
                run_systemctl(&["start", super::super::SERVICE_NAME])
            }
            Self::Launchd => {
                launchd_refresh(service_path, socket_path)?;
                if !previous_state.is_enabled() {
                    run_launchctl(&["disable", &launchd_service_target()?])?;
                }
                Ok(())
            }
            Self::WindowsTask => windows_task::apply_state(previous_state),
        }
    }

    pub(super) fn after_uninstall(&self, stop: bool) {
        match self {
            Self::Systemd => {
                if stop {
                    let _ = run_systemctl(&["daemon-reload"]);
                }
            }
            Self::Launchd => {}
            Self::WindowsTask => {}
        }
    }

    pub(super) fn log_hint(&self) -> String {
        match self {
            Self::Systemd => format!("journalctl --user -u {} -f", super::super::SERVICE_NAME),
            Self::Launchd => crate::config::user_data_dir().map_or_else(
                || "tail -f <tracedecay-data-dir>/daemon.err.log".to_string(),
                |dir| format!("tail -f \"{}\"", dir.join("daemon.err.log").display()),
            ),
            Self::WindowsTask => {
                "Event Viewer: Applications and Services Logs/Microsoft/Windows/TaskScheduler/Operational"
                    .to_string()
            }
        }
    }

    pub(super) fn service_detail_hint(&self) -> Option<String> {
        match self {
            Self::Systemd => None,
            Self::Launchd => launchd_service_target()
                .ok()
                .map(|target| format!("launchctl print {target}")),
            Self::WindowsTask => windows_task::task_name()
                .ok()
                .map(|name| format!("Get-ScheduledTask -TaskName '{name}'")),
        }
    }
}

fn launchctl_failure(args: &[&str], output: &std::process::Output) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "launchctl {} failed with status {}\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    }
}

fn run_launchctl(args: &[&str]) -> Result<std::process::Output> {
    let output = launchctl_spawn(args)?;
    if output.status.success() {
        return Ok(output);
    }
    Err(launchctl_failure(args, &output))
}

fn run_systemctl(args: &[&str]) -> Result<()> {
    let output = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|e| TraceDecayError::Config {
            message: format!("failed to run systemctl --user {}: {e}", args.join(" ")),
        })?;
    if output.status.success() {
        return Ok(());
    }
    Err(TraceDecayError::Config {
        message: format!(
            "systemctl --user {} failed with status {}\n{}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ),
    })
}

fn unsupported_service_platform() -> TraceDecayError {
    TraceDecayError::Config {
        message: "daemon service install is currently supported on Linux systemd user services, macOS launchd agents, and per-user Windows scheduled tasks"
            .to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LaunchctlFailureMode {
    /// Propagate any failure.
    Fail,
    /// Tolerate "service is not loaded" failures (e.g. `bootout` before the
    /// agent was ever bootstrapped); propagate everything else.
    TolerateNotLoaded,
    /// Best effort: ignore any failure.
    Ignore,
}

/// Commands that (re)start the launchd agent. Booting the service out first
/// (tolerating "not loaded") makes the sequence idempotent, and enabling
/// before bootstrap clears any persisted disabled state so the bootstrap
/// cannot be rejected.
pub(super) fn launchd_start_command_plan(
    domain: &str,
    target: &str,
    service_path: &Path,
) -> Vec<LaunchdCommand> {
    vec![
        LaunchdCommand::new(
            &["bootout", target],
            LaunchctlFailureMode::TolerateNotLoaded,
        ),
        LaunchdCommand::new(&["enable", target], LaunchctlFailureMode::Fail),
        LaunchdCommand::new(
            &["bootstrap", domain, &service_path.display().to_string()],
            LaunchctlFailureMode::Fail,
        ),
        LaunchdCommand::new(&["kickstart", "-k", target], LaunchctlFailureMode::Fail),
    ]
}

pub(super) fn launchd_uninstall_command_plan(target: &str) -> Vec<LaunchdCommand> {
    vec![
        LaunchdCommand::new(
            &["bootout", target],
            LaunchctlFailureMode::TolerateNotLoaded,
        ),
        // Persist the stopped state so launchd does not revive the agent at
        // the next login; best effort because the plist is removed anyway.
        LaunchdCommand::new(&["disable", target], LaunchctlFailureMode::Ignore),
    ]
}

fn run_launchd_commands(commands: &[LaunchdCommand]) -> Result<()> {
    for command in commands {
        let args: Vec<&str> = command.args.iter().map(String::as_str).collect();
        match command.failure_mode {
            LaunchctlFailureMode::Fail => {
                run_launchctl(&args)?;
            }
            LaunchctlFailureMode::TolerateNotLoaded => run_launchctl_allow_not_loaded(&args)?,
            LaunchctlFailureMode::Ignore => {
                let _ = run_launchctl(&args);
            }
        }
    }
    Ok(())
}

fn launchd_domain() -> Result<String> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .map_err(|e| TraceDecayError::Config {
            message: format!("failed to determine user id for launchd domain: {e}"),
        })?;
    if !output.status.success() {
        return Err(TraceDecayError::Config {
            message: format!(
                "id -u failed with status {}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ),
        });
    }
    let uid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if uid.is_empty() {
        return Err(TraceDecayError::Config {
            message: "id -u returned an empty user id".to_string(),
        });
    }
    Ok(format!("gui/{uid}"))
}

fn launchd_service_target() -> Result<String> {
    Ok(format!("{}/{}", launchd_domain()?, LAUNCHD_LABEL))
}

fn launchd_service_is_disabled() -> bool {
    let Ok(domain) = launchd_domain() else {
        return false;
    };
    let Ok(output) = Command::new("launchctl")
        .args(["print-disabled", &domain])
        .output()
    else {
        return false;
    };
    launchd_disabled_output_contains_label(&String::from_utf8_lossy(&output.stdout), LAUNCHD_LABEL)
}

pub(super) fn launchd_disabled_output_contains_label(output: &str, label: &str) -> bool {
    output.lines().any(|line| {
        line.contains(label)
            && line
                .split_once("=>")
                .is_some_and(|(_, value)| value.trim().starts_with("true"))
    })
}

fn ensure_launchd_runtime_dirs() -> Result<()> {
    let data_dir = tracedecay_data_dir()?;
    std::fs::create_dir_all(&data_dir).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to create daemon data directory '{}': {e}",
            data_dir.display()
        ),
    })
}

fn launchd_install(service_path: &Path, start: bool, socket_path: &Path) -> Result<()> {
    ensure_launchd_runtime_dirs()?;
    let target = launchd_service_target()?;
    if !start {
        // launchd bootstraps every plist in ~/Library/LaunchAgents at login,
        // so persist a disabled state to keep --no-start meaning "do not run".
        run_launchctl(&["disable", &target])?;
        return Ok(());
    }
    launchd_start(&target, service_path, socket_path)
}

fn launchd_refresh(service_path: &Path, socket_path: &Path) -> Result<()> {
    ensure_launchd_runtime_dirs()?;
    let target = launchd_service_target()?;
    launchd_start(&target, service_path, socket_path)
}

fn launchd_start_preserving_enablement(
    target: &str,
    service_path: &Path,
    socket_path: &Path,
) -> Result<()> {
    let was_disabled = launchd_service_is_disabled();
    let start_result = launchd_start(target, service_path, socket_path);
    if !was_disabled {
        return start_result;
    }
    let restore_result = run_launchctl(&["disable", target]).map(|_| ());
    match (start_result, restore_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(start), Err(restore)) => Err(TraceDecayError::Config {
            message: format!(
                "failed to start disabled launchd service: {start}; restoring disabled state also failed: {restore}"
            ),
        }),
    }
}

fn launchd_start(target: &str, service_path: &Path, socket_path: &Path) -> Result<()> {
    let domain = launchd_domain()?;
    run_launchd_commands(&launchd_start_command_plan(&domain, target, service_path))?;
    verify_launchd_started(target, socket_path)
}

fn launchd_before_uninstall(stop: bool) -> Result<()> {
    if !stop {
        return Ok(());
    }
    let target = launchd_service_target()?;
    run_launchd_commands(&launchd_uninstall_command_plan(&target))
}

fn launchd_stop() -> Result<()> {
    let target = launchd_service_target()?;
    run_launchctl_allow_not_loaded(&["bootout", &target])
}

fn verify_launchd_started(target: &str, socket_path: &Path) -> Result<()> {
    if daemon_socket_state(socket_path) == DaemonSocketState::Connectable {
        return Ok(());
    }
    run_launchctl(&["print", target]).map(|_| ())
}

fn launchctl_spawn(args: &[&str]) -> Result<std::process::Output> {
    Command::new("launchctl")
        .args(args)
        .output()
        .map_err(|e| TraceDecayError::Config {
            message: format!("failed to run launchctl {}: {e}", args.join(" ")),
        })
}

fn run_launchctl_allow_not_loaded(args: &[&str]) -> Result<()> {
    let output = launchctl_spawn(args)?;
    if output.status.success()
        || launchctl_stderr_is_not_loaded(&String::from_utf8_lossy(&output.stderr))
    {
        return Ok(());
    }
    Err(launchctl_failure(args, &output))
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct LaunchdCommand {
    args: Vec<String>,
    failure_mode: LaunchctlFailureMode,
}

impl LaunchdCommand {
    pub(super) fn new(args: &[&str], failure_mode: LaunchctlFailureMode) -> Self {
        Self {
            args: args.iter().map(|arg| String::from(*arg)).collect(),
            failure_mode,
        }
    }
}

pub(super) fn launchctl_stderr_is_not_loaded(stderr: &str) -> bool {
    [
        "No such process",
        "No such file or directory",
        "Could not find service",
        "Could not find specified service",
        "service is not loaded",
    ]
    .iter()
    .any(|needle| stderr.contains(needle))
}
