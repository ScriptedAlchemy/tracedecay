#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use tracedecay_domain::errors::{Result, TraceDecayError};

use super::probe::{DaemonSocketState, daemon_socket_state};
use super::{DaemonServiceState, LAUNCHD_LABEL, tracedecay_data_dir, windows_task};

/// All variants exist on every platform so that dispatch stays exhaustive.
#[derive(Clone, Debug)]
pub(super) enum ServiceRunner {
    Systemd { systemctl: PathBuf },
    Launchd { launchctl: PathBuf, id: PathBuf },
    WindowsTask,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ServicePlatform {
    Systemd,
    Launchd,
    WindowsTask,
}

impl ServicePlatform {
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
}

impl ServiceRunner {
    pub(super) fn current() -> Result<Self> {
        let path_var = std::env::var_os("PATH");
        match ServicePlatform::current()? {
            ServicePlatform::Systemd => Self::systemd(require_service_program_on_path(
                "systemctl",
                "systemd user service management",
                path_var.as_deref(),
            )?),
            ServicePlatform::Launchd => Self::launchd(
                require_service_program_on_path(
                    "launchctl",
                    "launchd agent management",
                    path_var.as_deref(),
                )?,
                require_service_program_on_path(
                    "id",
                    "launchd user-domain resolution",
                    path_var.as_deref(),
                )?,
            ),
            ServicePlatform::WindowsTask => Ok(Self::WindowsTask),
        }
    }

    pub(super) fn systemd(systemctl: impl AsRef<Path>) -> Result<Self> {
        Ok(Self::Systemd {
            systemctl: required_service_program(
                "systemctl",
                "systemd user service management",
                systemctl.as_ref(),
            )?,
        })
    }

    pub(super) fn launchd(launchctl: impl AsRef<Path>, id: impl AsRef<Path>) -> Result<Self> {
        Ok(Self::Launchd {
            launchctl: required_service_program(
                "launchctl",
                "launchd agent management",
                launchctl.as_ref(),
            )?,
            id: required_service_program("id", "launchd user-domain resolution", id.as_ref())?,
        })
    }

    #[hotpath::measure(label = "daemon.service.runner.install")]
    pub(super) fn install(
        &self,
        service_path: &Path,
        start: bool,
        socket_path: &Path,
        expected_version: &str,
    ) -> Result<()> {
        match self {
            Self::Systemd { systemctl } => {
                for arguments in systemd_install_command_plan(start) {
                    run_systemctl(systemctl, &arguments)?;
                }
                Ok(())
            }
            Self::Launchd { launchctl, id } => {
                launchd_install(launchctl, id, service_path, start, socket_path)
            }
            Self::WindowsTask => windows_task::apply_state(
                if start {
                    DaemonServiceState::RunningEnabled
                } else {
                    DaemonServiceState::StoppedDisabled
                },
                expected_version,
            ),
        }
    }

    #[hotpath::measure(label = "daemon.service.runner.refresh")]
    pub(super) fn refresh(
        &self,
        service_path: &Path,
        socket_path: &Path,
        previous_state: DaemonServiceState,
        expected_version: &str,
    ) -> Result<()> {
        match self {
            Self::Systemd { systemctl } => {
                run_systemctl(systemctl, &["daemon-reload"])?;
                if previous_state.is_enabled() {
                    run_systemctl(systemctl, &["enable", crate::SERVICE_NAME])?;
                }
                if previous_state.is_running() {
                    run_systemctl(systemctl, &["restart", crate::SERVICE_NAME])?;
                }
                Ok(())
            }
            Self::Launchd { launchctl, id } if previous_state.is_running() => {
                launchd_refresh(launchctl, id, service_path, socket_path)?;
                if !previous_state.is_enabled() {
                    run_launchctl(launchctl, &["disable", &launchd_service_target(id)?])?;
                }
                Ok(())
            }
            Self::Launchd { .. } => Ok(()),
            Self::WindowsTask => windows_task::apply_state(previous_state, expected_version),
        }
    }

    pub(super) fn service_state(&self, socket_path: &Path) -> Result<DaemonServiceState> {
        match self {
            Self::Systemd { systemctl } => {
                let running = Command::new(systemctl)
                    .args(["--user", "is-active", "--quiet", crate::SERVICE_NAME])
                    .status()
                    .map_err(|error| {
                        service_program_spawn_error("systemctl", "systemd service state", error)
                    })?
                    .success();
                let enablement = Command::new(systemctl)
                    .args(["--user", "is-enabled", crate::SERVICE_NAME])
                    .output()
                    .map_err(|error| {
                        service_program_spawn_error("systemctl", "systemd service state", error)
                    })?;
                let enablement = String::from_utf8_lossy(&enablement.stdout)
                    .trim()
                    .to_string();
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
            Self::Launchd { launchctl, id } => {
                let running = matches!(
                    daemon_socket_state(socket_path),
                    DaemonSocketState::Connectable
                );
                let enabled = !launchd_service_is_disabled(launchctl, id)?;
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

    #[hotpath::measure(label = "daemon.service.runner.before_uninstall")]
    pub(super) fn before_uninstall(&self, stop: bool, expected_version: &str) -> Result<()> {
        match self {
            Self::Systemd { systemctl } => {
                if stop {
                    let _ = run_systemctl(systemctl, &["disable", "--now", crate::SERVICE_NAME]);
                }
                Ok(())
            }
            Self::Launchd { launchctl, id } => launchd_before_uninstall(launchctl, id, stop),
            Self::WindowsTask if stop => windows_task::deactivate(expected_version),
            Self::WindowsTask => Ok(()),
        }
    }

    #[hotpath::measure(label = "daemon.service.runner.start")]
    pub(super) fn start(
        &self,
        service_path: &Path,
        socket_path: &Path,
        expected_version: &str,
    ) -> Result<()> {
        match self {
            Self::Systemd { systemctl } => {
                for arguments in systemd_start_command_plan() {
                    run_systemctl(systemctl, &arguments)?;
                }
                Ok(())
            }
            Self::Launchd { launchctl, id } => {
                let target = launchd_service_target(id)?;
                launchd_start_preserving_enablement(
                    launchctl,
                    id,
                    &target,
                    service_path,
                    socket_path,
                )
            }
            Self::WindowsTask => windows_task::start(expected_version),
        }
    }

    #[hotpath::measure(label = "daemon.service.runner.stop")]
    pub(super) fn stop(&self, expected_version: &str) -> Result<()> {
        match self {
            Self::Systemd { systemctl } => run_systemctl(systemctl, &["stop", crate::SERVICE_NAME]),
            Self::Launchd { launchctl, id } => launchd_stop(launchctl, id),
            Self::WindowsTask => windows_task::stop(expected_version),
        }
    }

    pub(super) fn stop_for_update(&self, expected_version: &str) -> Result<()> {
        self.stop(expected_version)
    }

    pub(super) fn restore_after_update(
        &self,
        service_path: &Path,
        socket_path: &Path,
        previous_state: DaemonServiceState,
        expected_version: &str,
    ) -> Result<()> {
        if !previous_state.is_running() {
            return Ok(());
        }
        match self {
            Self::Systemd { systemctl } => {
                run_systemctl(systemctl, &["daemon-reload"])?;
                if previous_state.is_enabled() {
                    run_systemctl(systemctl, &["enable", crate::SERVICE_NAME])?;
                } else {
                    run_systemctl(systemctl, &["disable", crate::SERVICE_NAME])?;
                }
                run_systemctl(systemctl, &["start", crate::SERVICE_NAME])?;
                // `systemctl start` reports the fork, not a serving daemon.
                // Restore success must mean an authenticated daemon at the
                // expected version answering from the installed unit's socket.
                super::wait_for_installed_service_state_with_runner(
                    self,
                    previous_state,
                    expected_version,
                )
            }
            Self::Launchd { launchctl, id } => {
                launchd_refresh(launchctl, id, service_path, socket_path)?;
                if !previous_state.is_enabled() {
                    run_launchctl(launchctl, &["disable", &launchd_service_target(id)?])?;
                }
                // `launchd_refresh` proves only that the socket accepts a
                // connection; hold launchd restores to the same authenticated
                // identity bar as systemd.
                super::wait_for_installed_service_state_with_runner(
                    self,
                    previous_state,
                    expected_version,
                )
            }
            // `windows_task::apply_state` already polls authenticated
            // readiness internally; a second wait would double the restore.
            Self::WindowsTask => windows_task::apply_state(previous_state, expected_version),
        }
    }

    pub(super) fn after_uninstall(&self, stop: bool) {
        match self {
            Self::Systemd { systemctl } => {
                if stop {
                    let _ = run_systemctl(systemctl, &["daemon-reload"]);
                }
            }
            Self::Launchd { .. } => {}
            Self::WindowsTask => {}
        }
    }

    pub(super) fn log_hint(&self) -> String {
        match self {
            Self::Systemd { .. } => {
                format!("journalctl --user -u {} -f", crate::SERVICE_NAME)
            }
            Self::Launchd { .. } => tracedecay_runtime_core::config::user_data_dir().map_or_else(
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
            Self::Systemd { .. } => None,
            Self::Launchd { id, .. } => launchd_service_target(id)
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

fn run_launchctl(launchctl: &Path, args: &[&str]) -> Result<std::process::Output> {
    let output = launchctl_spawn(launchctl, args)?;
    if output.status.success() {
        return Ok(output);
    }
    Err(launchctl_failure(args, &output))
}

fn run_systemctl(systemctl: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new(systemctl)
        .arg("--user")
        .args(args)
        .output()
        .map_err(|error| {
            service_program_spawn_error("systemctl", "systemd service management", error)
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

fn required_service_program(program: &str, lifecycle: &str, candidate: &Path) -> Result<PathBuf> {
    if !candidate.is_absolute() {
        return Err(TraceDecayError::Config {
            message: format!(
                "{program} program path '{}' must be absolute for {lifecycle}",
                candidate.display()
            ),
        });
    }
    canonical_service_program_candidate(
        program,
        lifecycle,
        candidate,
        NonExecutableCandidate::Reject,
    )?
    .ok_or_else(|| service_program_unavailable(program, lifecycle))
}

pub(super) fn require_service_program_on_path(
    program: &str,
    lifecycle: &str,
    path_var: Option<&std::ffi::OsStr>,
) -> Result<PathBuf> {
    let Some(path_var) = path_var else {
        return Err(service_program_unavailable(program, lifecycle));
    };
    for directory in std::env::split_paths(path_var) {
        let candidate = directory.join(program);
        if let Some(canonical) = canonical_service_program_candidate(
            program,
            lifecycle,
            &candidate,
            NonExecutableCandidate::Skip,
        )? {
            return Ok(canonical);
        }
    }
    Err(service_program_unavailable(program, lifecycle))
}

#[derive(Clone, Copy)]
enum NonExecutableCandidate {
    Reject,
    Skip,
}

fn canonical_service_program_candidate(
    program: &str,
    lifecycle: &str,
    candidate: &Path,
    non_executable: NonExecutableCandidate,
) -> Result<Option<PathBuf>> {
    let metadata = match std::fs::metadata(candidate) {
        Ok(metadata) if !metadata.is_file() => return Ok(None),
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(TraceDecayError::Io(error)),
    };
    if !service_program_is_executable(&metadata) {
        return match non_executable {
            NonExecutableCandidate::Reject => Err(TraceDecayError::Config {
                message: format!(
                    "{program} candidate '{}' exists but is not executable for {lifecycle}",
                    candidate.display()
                ),
            }),
            NonExecutableCandidate::Skip => Ok(None),
        };
    }
    match std::fs::canonicalize(candidate) {
        Ok(canonical) => Ok(Some(canonical)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(TraceDecayError::Io(error)),
    }
}

#[cfg(unix)]
fn service_program_is_executable(metadata: &std::fs::Metadata) -> bool {
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn service_program_is_executable(_metadata: &std::fs::Metadata) -> bool {
    true
}

fn service_program_spawn_error(
    program: &str,
    lifecycle: &str,
    error: std::io::Error,
) -> TraceDecayError {
    if error.kind() == std::io::ErrorKind::NotFound {
        service_program_unavailable(program, lifecycle)
    } else {
        TraceDecayError::Config {
            message: format!("failed to run {program} for {lifecycle}: {error}"),
        }
    }
}

fn service_program_unavailable(program: &str, lifecycle: &str) -> TraceDecayError {
    TraceDecayError::HostCliUnavailable {
        program: program.to_owned(),
        lifecycle: lifecycle.to_owned(),
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
    /// `bootstrap` right after `bootout` can race the old job's teardown:
    /// launchd rejects the re-bootstrap with `Bootstrap failed: 5:
    /// Input/output error` (EIO) until the previous instance drains, which
    /// left `daemon restart` with a stopped service. Retry exactly that
    /// failure a bounded number of times with a short backoff; every other
    /// failure propagates immediately.
    RetryTransientBootstrap,
}

const TRANSIENT_BOOTSTRAP_ATTEMPTS: u32 = 5;
const TRANSIENT_BOOTSTRAP_INITIAL_BACKOFF: std::time::Duration =
    std::time::Duration::from_millis(200);
const TRANSIENT_BOOTSTRAP_MAX_BACKOFF: std::time::Duration = std::time::Duration::from_millis(1600);

/// Matches only launchd's EIO bootstrap rejection — the transient window
/// while the booted-out job is still draining. Other bootstrap failures
/// (bad plist, permission, unknown domain) are not transient and must fail.
pub(super) fn launchctl_output_is_transient_bootstrap_failure(output: &str) -> bool {
    output.contains("Bootstrap failed: 5:")
}

fn run_launchctl_retrying_transient_bootstrap(launchctl: &Path, args: &[&str]) -> Result<()> {
    retry_transient_bootstrap(
        args,
        || launchctl_spawn(launchctl, args),
        std::thread::sleep,
    )
}

pub(super) fn retry_transient_bootstrap(
    args: &[&str],
    mut spawn: impl FnMut() -> Result<std::process::Output>,
    mut sleep: impl FnMut(std::time::Duration),
) -> Result<()> {
    let mut backoff = TRANSIENT_BOOTSTRAP_INITIAL_BACKOFF;
    let mut attempt = 0;
    loop {
        attempt += 1;
        let output = spawn()?;
        if output.status.success() {
            return Ok(());
        }
        let transient = launchctl_output_is_transient_bootstrap_failure(&String::from_utf8_lossy(
            &output.stderr,
        )) || launchctl_output_is_transient_bootstrap_failure(
            &String::from_utf8_lossy(&output.stdout),
        );
        if !transient || attempt >= TRANSIENT_BOOTSTRAP_ATTEMPTS {
            return Err(launchctl_failure(args, &output));
        }
        sleep(backoff);
        backoff = (backoff * 2).min(TRANSIENT_BOOTSTRAP_MAX_BACKOFF);
    }
}

/// Commands that register a freshly written systemd unit. The caller has just
/// (re)written the unit file, so systemd must re-read it even when the unit is
/// not started now (`--no-start`); without the reload a later `start` launches
/// whatever stale definition systemd last loaded.
pub(super) fn systemd_install_command_plan(start: bool) -> Vec<Vec<&'static str>> {
    let mut plan = vec![vec!["daemon-reload"]];
    if start {
        plan.push(vec!["enable", "--now", crate::SERVICE_NAME]);
    }
    plan
}

/// Commands that start the installed unit. A `start` can follow a unit
/// rewrite that never went through install on this boot, so reload first;
/// the reload is idempotent when the unit on disk is unchanged.
pub(super) fn systemd_start_command_plan() -> Vec<Vec<&'static str>> {
    vec![vec!["daemon-reload"], vec!["start", crate::SERVICE_NAME]]
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
            LaunchctlFailureMode::RetryTransientBootstrap,
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

fn run_launchd_commands(launchctl: &Path, commands: &[LaunchdCommand]) -> Result<()> {
    for command in commands {
        let args: Vec<&str> = command.args.iter().map(String::as_str).collect();
        match command.failure_mode {
            LaunchctlFailureMode::Fail => {
                run_launchctl(launchctl, &args)?;
            }
            LaunchctlFailureMode::TolerateNotLoaded => {
                run_launchctl_allow_not_loaded(launchctl, &args)?
            }
            LaunchctlFailureMode::Ignore => {
                let _ = run_launchctl(launchctl, &args);
            }
            LaunchctlFailureMode::RetryTransientBootstrap => {
                run_launchctl_retrying_transient_bootstrap(launchctl, &args)?;
            }
        }
    }
    Ok(())
}

fn launchd_domain(id: &Path) -> Result<String> {
    let output = Command::new(id).arg("-u").output().map_err(|error| {
        service_program_spawn_error("id", "launchd user-domain resolution", error)
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

fn launchd_service_target(id: &Path) -> Result<String> {
    Ok(format!("{}/{}", launchd_domain(id)?, LAUNCHD_LABEL))
}

fn launchd_service_is_disabled(launchctl: &Path, id: &Path) -> Result<bool> {
    let domain = launchd_domain(id)?;
    let output = Command::new(launchctl)
        .args(["print-disabled", &domain])
        .output()
        .map_err(|error| {
            service_program_spawn_error("launchctl", "launchd service state", error)
        })?;
    Ok(launchd_disabled_output_contains_label(
        &String::from_utf8_lossy(&output.stdout),
        LAUNCHD_LABEL,
    ))
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

fn launchd_install(
    launchctl: &Path,
    id: &Path,
    service_path: &Path,
    start: bool,
    socket_path: &Path,
) -> Result<()> {
    ensure_launchd_runtime_dirs()?;
    let target = launchd_service_target(id)?;
    if !start {
        // launchd bootstraps every plist in ~/Library/LaunchAgents at login,
        // so persist a disabled state to keep --no-start meaning "do not run".
        run_launchctl(launchctl, &["disable", &target])?;
        return Ok(());
    }
    launchd_start(launchctl, id, &target, service_path, socket_path)
}

fn launchd_refresh(
    launchctl: &Path,
    id: &Path,
    service_path: &Path,
    socket_path: &Path,
) -> Result<()> {
    ensure_launchd_runtime_dirs()?;
    let target = launchd_service_target(id)?;
    launchd_start(launchctl, id, &target, service_path, socket_path)
}

fn launchd_start_preserving_enablement(
    launchctl: &Path,
    id: &Path,
    target: &str,
    service_path: &Path,
    socket_path: &Path,
) -> Result<()> {
    let was_disabled = launchd_service_is_disabled(launchctl, id)?;
    let start_result = launchd_start(launchctl, id, target, service_path, socket_path);
    if !was_disabled {
        return start_result;
    }
    let restore_result = run_launchctl(launchctl, &["disable", target]).map(|_| ());
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

fn launchd_start(
    launchctl: &Path,
    id: &Path,
    target: &str,
    service_path: &Path,
    socket_path: &Path,
) -> Result<()> {
    let domain = launchd_domain(id)?;
    run_launchd_commands(
        launchctl,
        &launchd_start_command_plan(&domain, target, service_path),
    )?;
    verify_launchd_started(launchctl, target, socket_path)
}

fn launchd_before_uninstall(launchctl: &Path, id: &Path, stop: bool) -> Result<()> {
    if !stop {
        return Ok(());
    }
    let target = launchd_service_target(id)?;
    run_launchd_commands(launchctl, &launchd_uninstall_command_plan(&target))
}

fn launchd_stop(launchctl: &Path, id: &Path) -> Result<()> {
    let target = launchd_service_target(id)?;
    run_launchctl_allow_not_loaded(launchctl, &["bootout", &target])
}

fn verify_launchd_started(launchctl: &Path, target: &str, socket_path: &Path) -> Result<()> {
    if daemon_socket_state(socket_path) == DaemonSocketState::Connectable {
        return Ok(());
    }
    run_launchctl(launchctl, &["print", target]).map(|_| ())
}

fn launchctl_spawn(launchctl: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new(launchctl)
        .args(args)
        .output()
        .map_err(|error| {
            service_program_spawn_error(
                "launchctl",
                &format!("launchd command `{}`", args.join(" ")),
                error,
            )
        })
}

fn run_launchctl_allow_not_loaded(launchctl: &Path, args: &[&str]) -> Result<()> {
    let output = launchctl_spawn(launchctl, args)?;
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
