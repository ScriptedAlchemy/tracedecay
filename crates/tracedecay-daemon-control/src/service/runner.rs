#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use tracedecay_domain::errors::{Result, TraceDecayError};

use super::probe::{DaemonSocketState, daemon_socket_state};
use super::unit_file::{launchd_label, launchd_user_service_path};
use super::unit_file::{systemd_unit_name, systemd_user_service_path};
use super::{DaemonServiceState, windows_task};
use tracedecay_runtime_core::config::ProfileRoot;

/// All variants exist on every platform so that dispatch stays exhaustive.
#[derive(Clone, Debug)]
pub(super) enum ServiceRunner {
    /// `systemctl` is `None` only from
    /// [`ServiceRunner::current_for_installed_unit`]: the missing program is
    /// reported when a unit operation first needs it.
    Systemd {
        systemctl: Option<PathBuf>,
        unit: SystemdUnit,
    },
    Launchd {
        launchctl: PathBuf,
        id: PathBuf,
        profile: ProfileRoot,
    },
    WindowsTask {
        profile: ProfileRoot,
    },
}

/// The user unit a profile owns: its name and the file that profile installs.
#[derive(Clone, Debug)]
pub(super) struct SystemdUnit {
    name: String,
    path: PathBuf,
}

impl SystemdUnit {
    fn of(profile: &ProfileRoot) -> Result<Self> {
        Ok(Self {
            name: systemd_unit_name(profile),
            path: systemd_user_service_path(profile)?,
        })
    }

    fn require_owned(
        &self,
        loaded: Option<PathBuf>,
    ) -> std::result::Result<&str, ServiceStateError> {
        if loaded
            .as_deref()
            .is_some_and(|loaded| same_unit_file(loaded, &self.path))
        {
            return Ok(&self.name);
        }
        Err(ServiceStateError::Failed(
            TraceDecayError::ServiceUnitNotOwned {
                unit: self.name.clone(),
                owned: self.path.clone().into_boxed_path(),
                loaded: loaded.map(PathBuf::into_boxed_path),
            },
        ))
    }
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
    pub(super) fn current(profile: &ProfileRoot) -> Result<Self> {
        let path_var = tracedecay_runtime_core::config::host_program_search_path();
        match ServicePlatform::current()? {
            ServicePlatform::Systemd => Self::systemd(
                require_service_program_on_path(
                    "systemctl",
                    "systemd user service management",
                    path_var.as_deref(),
                )?,
                profile,
            ),
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
                profile,
            ),
            ServicePlatform::WindowsTask => Ok(Self::WindowsTask {
                profile: profile.clone(),
            }),
        }
    }

    /// [`Self::current`] for paths that consult the service manager only once
    /// a unit is installed. A Linux host without systemd (a container, say)
    /// has no managed unit, so its maintenance must not require `systemctl`.
    pub(super) fn current_for_installed_unit(profile: &ProfileRoot) -> Result<Self> {
        match Self::current(profile) {
            Err(TraceDecayError::HostCliUnavailable { program, .. }) if program == "systemctl" => {
                Ok(Self::Systemd {
                    systemctl: None,
                    unit: SystemdUnit::of(profile)?,
                })
            }
            runner => runner,
        }
    }

    pub(super) fn systemd(systemctl: impl AsRef<Path>, profile: &ProfileRoot) -> Result<Self> {
        Ok(Self::Systemd {
            systemctl: Some(required_service_program(
                "systemctl",
                "systemd user service management",
                systemctl.as_ref(),
            )?),
            unit: SystemdUnit::of(profile)?,
        })
    }

    pub(super) fn launchd(
        launchctl: impl AsRef<Path>,
        id: impl AsRef<Path>,
        profile: &ProfileRoot,
    ) -> Result<Self> {
        Ok(Self::Launchd {
            launchctl: required_service_program(
                "launchctl",
                "launchd agent management",
                launchctl.as_ref(),
            )?,
            id: required_service_program("id", "launchd user-domain resolution", id.as_ref())?,
            profile: profile.clone(),
        })
    }

    #[tracing::instrument(name = "daemon.service.runner.install", level = "trace", skip_all)]
    pub(super) fn install(
        &self,
        service_path: &Path,
        start: bool,
        socket_path: &Path,
        expected_version: &str,
    ) -> Result<()> {
        match self {
            // The unit file was just (re)written, so systemd must re-read it
            // even under `--no-start`; otherwise a later `start` launches
            // whatever stale definition systemd last loaded.
            Self::Systemd { systemctl, unit } => {
                run_systemctl(systemctl.as_deref(), &["daemon-reload"])?;
                if start {
                    let name = owned_systemd_unit(systemctl.as_deref(), unit)?;
                    run_systemctl(systemctl.as_deref(), &["enable", "--now", name])?;
                }
                Ok(())
            }
            Self::Launchd {
                launchctl,
                id,
                profile,
            } => launchd_install(profile, launchctl, id, service_path, start, socket_path),
            Self::WindowsTask { profile } => windows_task::apply_state(
                profile,
                if start {
                    DaemonServiceState::RunningEnabled
                } else {
                    DaemonServiceState::StoppedDisabled
                },
                expected_version,
            ),
        }
    }

    #[tracing::instrument(name = "daemon.service.runner.refresh", level = "trace", skip_all)]
    pub(super) fn refresh(
        &self,
        service_path: &Path,
        socket_path: &Path,
        previous_state: DaemonServiceState,
        expected_version: &str,
    ) -> Result<()> {
        match self {
            Self::Systemd { systemctl, unit } => {
                run_systemctl(systemctl.as_deref(), &["daemon-reload"])?;
                if previous_state.is_running() {
                    let name = owned_systemd_unit(systemctl.as_deref(), unit)?;
                    run_systemctl(systemctl.as_deref(), &["restart", name])?;
                }
                Ok(())
            }
            Self::Launchd {
                launchctl,
                id,
                profile,
            } if previous_state.is_running() => {
                launchd_refresh(profile, launchctl, id, service_path, socket_path)?;
                if !previous_state.is_enabled() {
                    run_launchctl(
                        launchctl,
                        &["disable", &launchd_service_target(id, profile)?],
                    )?;
                }
                Ok(())
            }
            Self::Launchd { .. } => Ok(()),
            Self::WindowsTask { profile } => {
                windows_task::apply_state(profile, previous_state, expected_version)
            }
        }
    }

    pub(super) fn service_state(&self, socket_path: &Path) -> Result<DaemonServiceState> {
        self.observe_service_state(socket_path)
            .map_err(TraceDecayError::from)
    }

    /// Like [`Self::service_state`], but keeps an unreachable service manager
    /// apart from a failed query so status can report the daemon regardless.
    pub(super) fn observe_service_state(
        &self,
        socket_path: &Path,
    ) -> std::result::Result<DaemonServiceState, ServiceStateError> {
        match self {
            Self::Systemd { systemctl, unit } => {
                // A mask resolves the unit to `/dev/null` whoever placed it;
                // reporting it controls nothing, and every mutation still
                // refuses it.
                let loaded = loaded_systemd_unit_file(systemctl.as_deref(), unit)?;
                if loaded.as_deref() == Some(Path::new("/dev/null")) {
                    return Ok(DaemonServiceState::Masked);
                }
                unit.require_owned(loaded)?;
                let activity = systemctl_unit_query(systemctl.as_deref(), unit, "is-active")?;
                let running = match activity.as_str() {
                    "active" | "reloading" | "refreshing" => true,
                    "inactive" | "failed" | "activating" | "deactivating" | "maintenance" => false,
                    _ => return Err(systemctl_unknown_state(unit, "is-active", &activity).into()),
                };
                let enablement = systemctl_unit_query(systemctl.as_deref(), unit, "is-enabled")?;
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
            Self::Launchd {
                launchctl,
                id,
                profile,
            } => Ok(launchd_service_state(
                launchctl,
                id,
                daemon_socket_state(socket_path),
                profile,
            )?),
            Self::WindowsTask { profile } => Ok(windows_task::service_state(profile)?),
        }
    }

    #[tracing::instrument(
        name = "daemon.service.runner.before_uninstall",
        level = "trace",
        skip_all
    )]
    pub(super) fn before_uninstall(&self, stop: bool, expected_version: &str) -> Result<()> {
        match self {
            // Best effort: the unit file is removed either way, but a unit the
            // profile does not own is never disabled on its behalf.
            Self::Systemd { systemctl, unit } => {
                if stop && let Ok(name) = owned_systemd_unit(systemctl.as_deref(), unit) {
                    let _ = run_systemctl(systemctl.as_deref(), &["disable", "--now", name]);
                }
                Ok(())
            }
            Self::Launchd {
                launchctl,
                id,
                profile,
            } => launchd_before_uninstall(launchctl, id, profile, stop),
            Self::WindowsTask { profile } if stop => {
                windows_task::deactivate(profile, expected_version)
            }
            Self::WindowsTask { .. } => Ok(()),
        }
    }

    #[tracing::instrument(name = "daemon.service.runner.start", level = "trace", skip_all)]
    pub(super) fn start(
        &self,
        service_path: &Path,
        socket_path: &Path,
        expected_version: &str,
    ) -> Result<()> {
        match self {
            // A `start` can follow a unit rewrite that never went through
            // install on this boot, so reload first; the reload is idempotent
            // when the unit on disk is unchanged.
            Self::Systemd { systemctl, unit } => {
                run_systemctl(systemctl.as_deref(), &["daemon-reload"])?;
                let name = owned_systemd_unit(systemctl.as_deref(), unit)?;
                run_systemctl(systemctl.as_deref(), &["start", name])
            }
            Self::Launchd {
                launchctl,
                id,
                profile,
            } => {
                let target = launchd_service_target(id, profile)?;
                launchd_start_preserving_enablement(
                    launchctl,
                    id,
                    profile,
                    &target,
                    service_path,
                    socket_path,
                )
            }
            Self::WindowsTask { profile } => windows_task::start(profile, expected_version),
        }
    }

    #[tracing::instrument(name = "daemon.service.runner.stop", level = "trace", skip_all)]
    pub(super) fn stop(&self, expected_version: &str) -> Result<()> {
        match self {
            Self::Systemd { systemctl, unit } => {
                let name = owned_systemd_unit(systemctl.as_deref(), unit)?;
                run_systemctl(systemctl.as_deref(), &["stop", name])
            }
            Self::Launchd {
                launchctl,
                id,
                profile,
            } => launchd_stop(launchctl, id, profile),
            Self::WindowsTask { profile } => windows_task::stop(profile, expected_version),
        }
    }

    pub(super) fn stop_for_update(&self, expected_version: &str) -> Result<()> {
        self.stop(expected_version)
    }

    pub(super) fn restore_after_update(
        &self,
        profile: &ProfileRoot,
        service_path: &Path,
        socket_path: &Path,
        previous_state: DaemonServiceState,
        expected_version: &str,
    ) -> Result<()> {
        if !previous_state.is_running() {
            return Ok(());
        }
        match self {
            Self::Systemd { systemctl, unit } => {
                run_systemctl(systemctl.as_deref(), &["daemon-reload"])?;
                let name = owned_systemd_unit(systemctl.as_deref(), unit)?;
                if previous_state.is_enabled() {
                    run_systemctl(systemctl.as_deref(), &["enable", name])?;
                } else {
                    run_systemctl(systemctl.as_deref(), &["disable", name])?;
                }
                run_systemctl(systemctl.as_deref(), &["start", name])?;
                // `systemctl start` reports the fork, not a serving daemon.
                // Restore success must mean an authenticated daemon at the
                // expected version answering from the installed unit's socket.
                super::wait_for_installed_service_state_with_runner(
                    profile,
                    self,
                    previous_state,
                    expected_version,
                )
            }
            Self::Launchd {
                launchctl,
                id,
                profile,
            } => {
                launchd_refresh(profile, launchctl, id, service_path, socket_path)?;
                if !previous_state.is_enabled() {
                    run_launchctl(
                        launchctl,
                        &["disable", &launchd_service_target(id, profile)?],
                    )?;
                }
                // `launchd_refresh` proves only that the socket accepts a
                // connection; hold launchd restores to the same authenticated
                // identity bar as systemd.
                super::wait_for_installed_service_state_with_runner(
                    profile,
                    self,
                    previous_state,
                    expected_version,
                )
            }
            // `windows_task::apply_state` already polls authenticated
            // readiness internally; a second wait would double the restore.
            Self::WindowsTask { profile } => {
                windows_task::apply_state(profile, previous_state, expected_version)
            }
        }
    }

    pub(super) fn after_uninstall(&self, stop: bool) {
        match self {
            Self::Systemd { systemctl, .. } => {
                if stop {
                    let _ = run_systemctl(systemctl.as_deref(), &["daemon-reload"]);
                }
            }
            Self::Launchd { .. } | Self::WindowsTask { .. } => {}
        }
    }

    pub(super) fn log_hint(&self, profile: &ProfileRoot) -> String {
        match self {
            Self::Systemd { unit, .. } => format!("journalctl --user -u {} -f", unit.name),
            Self::Launchd { .. } => format!(
                "tail -f \"{}\"",
                profile.data_dir().join("daemon.err.log").display()
            ),
            Self::WindowsTask { .. } => {
                "Event Viewer: Applications and Services Logs/Microsoft/Windows/TaskScheduler/Operational"
                    .to_string()
            }
        }
    }

    pub(super) fn service_detail_hint(&self) -> Option<String> {
        match self {
            Self::Systemd { .. } => None,
            Self::Launchd { id, profile, .. } => launchd_service_target(id, profile)
                .ok()
                .map(|target| format!("launchctl print {target}")),
            Self::WindowsTask { profile } => windows_task::task_name(profile)
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

fn require_systemctl(systemctl: Option<&Path>) -> Result<&Path> {
    systemctl
        .ok_or_else(|| service_program_unavailable("systemctl", "systemd user service management"))
}

fn run_systemctl(systemctl: Option<&Path>, args: &[&str]) -> Result<()> {
    let output = Command::new(require_systemctl(systemctl)?)
        .arg("--user")
        .args(args)
        .output()
        .map_err(|error| {
            service_program_spawn_error("systemctl", "systemd service management", &error)
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

/// The service manager gave no answer to this process. Nothing about the unit
/// is known: the observer lacks access, which is not a misconfiguration.
#[derive(Debug)]
pub(super) struct ServiceManagerUnreachable {
    query: String,
}

impl ServiceManagerUnreachable {
    pub(super) const REMEDY: &'static str = "check XDG_RUNTIME_DIR and DBUS_SESSION_BUS_ADDRESS";
}

impl std::fmt::Display for ServiceManagerUnreachable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.query)
    }
}

#[derive(Debug)]
pub(super) enum ServiceStateError {
    ManagerUnreachable(ServiceManagerUnreachable),
    Failed(TraceDecayError),
}

impl From<TraceDecayError> for ServiceStateError {
    fn from(error: TraceDecayError) -> Self {
        Self::Failed(error)
    }
}

/// Lifecycle operations cannot act without the service manager, so for them
/// an unreachable manager stays a hard error naming its remedy.
impl From<ServiceStateError> for TraceDecayError {
    fn from(error: ServiceStateError) -> Self {
        match error {
            ServiceStateError::ManagerUnreachable(unreachable) => TraceDecayError::Config {
                message: format!(
                    "{unreachable}; the systemd user manager may be unreachable from this environment ({})",
                    ServiceManagerUnreachable::REMEDY
                ),
            },
            ServiceStateError::Failed(error) => error,
        }
    }
}

/// `systemctl --user is-active`/`is-enabled` exit non-zero both for a stopped
/// or disabled unit and when the user manager is unreachable; only the printed
/// state tells them apart, so an empty answer is an error, not "stopped".
fn systemctl_unit_query(
    systemctl: Option<&Path>,
    unit: &SystemdUnit,
    verb: &str,
) -> std::result::Result<String, ServiceStateError> {
    let output = Command::new(require_systemctl(systemctl)?)
        .args(["--user", verb, &unit.name])
        .output()
        .map_err(|error| {
            service_program_spawn_error("systemctl", "systemd service state", &error)
        })?;
    let state = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if state.is_empty() {
        return Err(ServiceStateError::ManagerUnreachable(
            ServiceManagerUnreachable {
                query: format!(
                    "systemctl --user {verb} {} reported no unit state ({}): {}",
                    unit.name,
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            },
        ));
    }
    Ok(state)
}

fn systemctl_unknown_state(unit: &SystemdUnit, verb: &str, state: &str) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "systemctl --user {verb} {} reported unrecognized unit state `{state}`",
            unit.name
        ),
    }
}

/// The user manager resolves a unit name through its own search path, never
/// through the profile's config home, so a profile with an isolated home can
/// name a unit the manager loads from somewhere else entirely. Every command
/// that controls or reports the unit first proves the manager's loaded file
/// is the one this profile installed.
fn owned_systemd_unit<'unit>(
    systemctl: Option<&Path>,
    unit: &'unit SystemdUnit,
) -> std::result::Result<&'unit str, ServiceStateError> {
    let loaded = loaded_systemd_unit_file(systemctl, unit)?;
    unit.require_owned(loaded)
}

/// The unit file the user manager loads for `unit`, or `None` when it finds
/// none on its search path.
fn loaded_systemd_unit_file(
    systemctl: Option<&Path>,
    unit: &SystemdUnit,
) -> std::result::Result<Option<PathBuf>, ServiceStateError> {
    let output = Command::new(require_systemctl(systemctl)?)
        .args([
            "--user",
            "show",
            "--property=FragmentPath",
            "--value",
            &unit.name,
        ])
        .output()
        .map_err(|error| {
            service_program_spawn_error("systemctl", "systemd unit ownership", &error)
        })?;
    if !output.status.success() {
        return Err(ServiceStateError::ManagerUnreachable(
            ServiceManagerUnreachable {
                query: format!(
                    "systemctl --user show --property=FragmentPath {} failed ({}): {}",
                    unit.name,
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            },
        ));
    }
    let loaded = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    Ok((!loaded.is_empty()).then(|| PathBuf::from(loaded)))
}

fn same_unit_file(loaded: &Path, owned: &Path) -> bool {
    loaded == owned
        || matches!(
            (std::fs::canonicalize(loaded), std::fs::canonicalize(owned)),
            (Ok(loaded), Ok(owned)) if loaded == owned
        )
}

fn service_program_spawn_error(
    program: &str,
    lifecycle: &str,
    error: &std::io::Error,
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

/// Matches only launchd's EIO bootstrap rejection, the transient window
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
                run_launchctl_allow_not_loaded(launchctl, &args)?;
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
        service_program_spawn_error("id", "launchd user-domain resolution", &error)
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

fn launchd_service_target(id: &Path, profile: &ProfileRoot) -> Result<String> {
    Ok(format!(
        "{}/{}",
        launchd_domain(id)?,
        launchd_label(profile)
    ))
}

/// A shared launchd domain must never act on a label loaded from another
/// profile's plist, even when both profiles retain the default label.
fn launchd_require_owned(launchctl: &Path, target: &str, profile: &ProfileRoot) -> Result<bool> {
    let output = launchctl_spawn(launchctl, &["print", target])?;
    if !output.status.success() {
        if launchctl_stderr_is_not_loaded(&String::from_utf8_lossy(&output.stderr))
            || launchctl_stderr_is_not_loaded(&String::from_utf8_lossy(&output.stdout))
        {
            return Ok(false);
        }
        return Err(launchctl_failure(&["print", target], &output));
    }
    let owned = launchd_user_service_path(profile)?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut paths = text.lines().filter_map(|line| {
        let value = line.trim().strip_prefix("path = ")?.trim();
        Some(PathBuf::from(
            value
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
                .unwrap_or(value),
        ))
    });
    let loaded = paths.next();
    if paths.next().is_none()
        && loaded
            .as_deref()
            .is_some_and(|path| same_unit_file(path, &owned))
    {
        return Ok(true);
    }
    Err(TraceDecayError::ServiceUnitNotOwned {
        unit: target.to_owned(),
        owned: owned.into_boxed_path(),
        loaded: loaded.map(PathBuf::into_boxed_path),
    })
}

/// launchd has no liveness query of its own: the agent is running when its
/// daemon socket accepts a connection.
pub(super) fn launchd_service_state(
    launchctl: &Path,
    id: &Path,
    socket_state: DaemonSocketState,
    profile: &ProfileRoot,
) -> Result<DaemonServiceState> {
    let running = matches!(socket_state, DaemonSocketState::Connectable);
    let enabled = !launchd_service_is_disabled(launchctl, id, profile)?;
    Ok(match (running, enabled) {
        (true, true) => DaemonServiceState::RunningEnabled,
        (true, false) => DaemonServiceState::RunningDisabled,
        (false, true) => DaemonServiceState::StoppedEnabled,
        (false, false) => DaemonServiceState::StoppedDisabled,
    })
}

fn launchd_service_is_disabled(launchctl: &Path, id: &Path, profile: &ProfileRoot) -> Result<bool> {
    let domain = launchd_domain(id)?;
    let output = Command::new(launchctl)
        .args(["print-disabled", &domain])
        .output()
        .map_err(|error| {
            service_program_spawn_error("launchctl", "launchd service state", &error)
        })?;
    Ok(launchd_disabled_output_contains_label(
        &String::from_utf8_lossy(&output.stdout),
        &launchd_label(profile),
    ))
}

pub(super) fn launchd_disabled_output_contains_label(output: &str, label: &str) -> bool {
    output.lines().any(|line| {
        line.split_once("=>").is_some_and(|(name, value)| {
            name.trim().trim_matches('"') == label && value.trim().starts_with("true")
        })
    })
}

fn ensure_launchd_runtime_dirs(profile: &ProfileRoot) -> Result<()> {
    let data_dir = profile.data_dir();
    std::fs::create_dir_all(data_dir).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to create daemon data directory '{}': {e}",
            data_dir.display()
        ),
    })
}

fn launchd_install(
    profile: &ProfileRoot,
    launchctl: &Path,
    id: &Path,
    service_path: &Path,
    start: bool,
    socket_path: &Path,
) -> Result<()> {
    ensure_launchd_runtime_dirs(profile)?;
    let target = launchd_service_target(id, profile)?;
    if !start {
        // launchd bootstraps every plist in ~/Library/LaunchAgents at login,
        // so persist a disabled state to keep --no-start meaning "do not run".
        launchd_require_owned(launchctl, &target, profile)?;
        run_launchctl(launchctl, &["disable", &target])?;
        return Ok(());
    }
    launchd_start(launchctl, id, profile, &target, service_path, socket_path)
}

fn launchd_refresh(
    profile: &ProfileRoot,
    launchctl: &Path,
    id: &Path,
    service_path: &Path,
    socket_path: &Path,
) -> Result<()> {
    ensure_launchd_runtime_dirs(profile)?;
    let target = launchd_service_target(id, profile)?;
    launchd_start(launchctl, id, profile, &target, service_path, socket_path)
}

fn launchd_start_preserving_enablement(
    launchctl: &Path,
    id: &Path,
    profile: &ProfileRoot,
    target: &str,
    service_path: &Path,
    socket_path: &Path,
) -> Result<()> {
    let was_disabled = launchd_service_is_disabled(launchctl, id, profile)?;
    let start_result = launchd_start(launchctl, id, profile, target, service_path, socket_path);
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
    profile: &ProfileRoot,
    target: &str,
    service_path: &Path,
    socket_path: &Path,
) -> Result<()> {
    launchd_require_owned(launchctl, target, profile)?;
    let domain = launchd_domain(id)?;
    run_launchd_commands(
        launchctl,
        &launchd_start_command_plan(&domain, target, service_path),
    )?;
    verify_launchd_started(launchctl, target, socket_path)
}

fn launchd_before_uninstall(
    launchctl: &Path,
    id: &Path,
    profile: &ProfileRoot,
    stop: bool,
) -> Result<()> {
    if !stop {
        return Ok(());
    }
    let target = launchd_service_target(id, profile)?;
    if !launchd_require_owned(launchctl, &target, profile)? {
        return Ok(());
    }
    run_launchd_commands(launchctl, &launchd_uninstall_command_plan(&target))
}

fn launchd_stop(launchctl: &Path, id: &Path, profile: &ProfileRoot) -> Result<()> {
    let target = launchd_service_target(id, profile)?;
    if !launchd_require_owned(launchctl, &target, profile)? {
        return Ok(());
    }
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
                &error,
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
