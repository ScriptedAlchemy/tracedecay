use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use crate::errors::{Result, TraceDecayError};

use super::SOCKET_ENV;

pub(crate) mod invocation;
mod multi_root;
mod probe;
pub(crate) mod project_runtime;
mod runner;
mod unit_file;
mod windows_task;

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests;

pub use probe::daemon_reachable;
pub use unit_file::installed_service_socket_path;

use probe::{
    DaemonProtocolState, DaemonSocketState, daemon_protocol_state, daemon_socket_state,
    daemon_transport_display,
};
use runner::ServiceRunner;
use unit_file::{
    launchd_plist_env_value, read_service_unit, remove_service_unit, service_unit_exists,
    service_unit_path, socket_path_from_unit_text, write_service_unit,
};

const LAUNCHD_LABEL: &str = "com.tracedecay.daemon";
const LAUNCHD_PLIST_NAME: &str = "com.tracedecay.daemon.plist";
static SERVICE_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

// Cached project owners retain SQLite families and coordination locks. The
// platform default of 256 descriptors is too small for multi-worktree use.
const DAEMON_OPEN_FILE_LIMIT: u32 = 8_192;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonServiceSpec {
    pub tracedecay_bin: PathBuf,
    pub socket_path: PathBuf,
    pub data_dir_override: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DaemonServiceState {
    Missing,
    RunningEnabled,
    RunningDisabled,
    StoppedEnabled,
    StoppedDisabled,
    Masked,
}

/// Owns the exclusive maintenance lease after stopping the managed daemon and
/// restores the captured daemon state only after releasing that lease.
pub struct QuiescedDaemonLifecycle {
    previous_state: DaemonServiceState,
    lifecycle_lease: Option<crate::lifecycle_lease::LifecycleLease>,
    restored: bool,
}

impl QuiescedDaemonLifecycle {
    pub fn acquire(operation: &str) -> Result<Self> {
        Self::acquire_with(operation, || {
            crate::lifecycle_lease::acquire_exclusive(operation)
        })
    }

    /// Stops the managed daemon, then waits up to `timeout` for its shared
    /// lifecycle lease to release before taking exclusive ownership.
    pub fn acquire_with_timeout(operation: &str, timeout: Duration) -> Result<Self> {
        Self::acquire_with(operation, || {
            crate::lifecycle_lease::acquire_exclusive_with_timeout(operation, timeout)
        })
    }

    fn acquire_with(
        operation: &str,
        acquire: impl FnOnce() -> Result<crate::lifecycle_lease::LifecycleLease>,
    ) -> Result<Self> {
        let previous_state = quiesce_installed_service_before_lease()?;
        match acquire() {
            Ok(lifecycle_lease) => {
                let mut guard = Self {
                    previous_state,
                    lifecycle_lease: Some(lifecycle_lease),
                    restored: false,
                };
                match verify_installed_service_quiesced_under_lease() {
                    Ok(_) => Ok(guard),
                    Err(operation_error) => {
                        let restore_result = guard.restore();
                        combine_operation_and_restore(
                            operation,
                            Err(operation_error),
                            restore_result,
                        )
                    }
                }
            }
            Err(operation_error) => {
                let restore_result = restore_installed_service_after_failed_acquire(previous_state);
                combine_operation_and_restore(operation, Err(operation_error), restore_result)
            }
        }
    }

    pub fn lifecycle_lease(&self) -> Result<&crate::lifecycle_lease::LifecycleLease> {
        self.lifecycle_lease
            .as_ref()
            .ok_or_else(|| TraceDecayError::Config {
                message: "quiesced daemon lifecycle lease already released".to_string(),
            })
    }

    pub fn previous_state(&self) -> DaemonServiceState {
        self.previous_state
    }

    pub fn finish(mut self) -> Result<()> {
        self.restore()
    }

    pub fn finish_with_state(mut self, state: DaemonServiceState) -> Result<()> {
        self.restore_state(state)
    }

    pub fn finish_without_restore(mut self) {
        drop(self.lifecycle_lease.take());
        self.restored = true;
    }

    fn restore(&mut self) -> Result<()> {
        if self.restored {
            return Ok(());
        }
        self.restore_state(self.previous_state)
    }

    /// Windows must release the exclusive lease before the running executable is
    /// replaced; other platforms retain it through the maintenance action.
    fn release_lease_for_executable_replacement(&mut self) {
        if cfg!(windows) {
            drop(self.lifecycle_lease.take());
        }
    }

    fn restore_state(&mut self, state: DaemonServiceState) -> Result<()> {
        if state.is_running() {
            self.downgrade_to_shared()?;
            restore_installed_service_after_update(state)?;
        } else {
            drop(self.lifecycle_lease.take());
        }
        self.restored = true;
        Ok(())
    }

    fn downgrade_to_shared(&mut self) -> Result<()> {
        self.downgrade_to_shared_with(|| {
            crate::lifecycle_lease::acquire_shared_blocking("daemon state restore")
        })
    }

    fn downgrade_to_shared_with(
        &mut self,
        acquire_shared: impl FnOnce() -> Result<crate::lifecycle_lease::LifecycleLease>,
    ) -> Result<()> {
        if self
            .lifecycle_lease
            .as_ref()
            .is_some_and(|lease| !lease.is_exclusive())
        {
            return Ok(());
        }
        if self.lifecycle_lease.is_none() {
            self.lifecycle_lease = Some(acquire_shared()?);
            return Ok(());
        }
        self.lifecycle_lease.as_mut().map_or_else(
            || {
                Err(TraceDecayError::Config {
                    message: "could not reacquire daemon lifecycle lease".to_string(),
                })
            },
            crate::lifecycle_lease::LifecycleLease::downgrade_to_shared,
        )
    }
}

impl Drop for QuiescedDaemonLifecycle {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

pub fn with_quiesced_installed_service<T>(
    operation: &str,
    action: impl FnOnce(&crate::lifecycle_lease::LifecycleLease) -> Result<T>,
) -> Result<T> {
    let mut guard = QuiescedDaemonLifecycle::acquire(operation)?;
    let operation_result = guard.lifecycle_lease().and_then(action);
    let restore_result = guard.restore();
    combine_operation_and_restore(operation, operation_result, restore_result)
}

/// Runs `action` inside an exclusive daemon maintenance window, handing it the
/// lifecycle lease owner token. On Windows the exclusive lease is released
/// before `action` runs so the running executable can be replaced; other
/// platforms retain it for the duration and restore the daemon afterward.
pub fn with_exclusive_maintenance_window<T>(
    operation: &str,
    action: impl FnOnce(&str) -> Result<T>,
) -> Result<T> {
    let mut guard = QuiescedDaemonLifecycle::acquire(operation)?;
    let token = guard
        .lifecycle_lease()?
        .token()
        .ok_or_else(|| TraceDecayError::Config {
            message: format!("{operation} lifecycle lease did not provide an owner token"),
        })?
        .to_string();
    guard.release_lease_for_executable_replacement();
    let operation_result = action(&token);
    let restore_result = guard.restore();
    combine_operation_and_restore(operation, operation_result, restore_result)
}

impl DaemonServiceState {
    fn is_running(self) -> bool {
        matches!(self, Self::RunningEnabled | Self::RunningDisabled)
    }

    fn is_enabled(self) -> bool {
        matches!(self, Self::RunningEnabled | Self::StoppedEnabled)
    }
}

impl DaemonServiceSpec {
    pub fn render_systemd_user_unit(&self) -> String {
        let service_path = daemon_service_path_env(&self.tracedecay_bin);
        format!(
            "[Unit]\n\
             Description=TraceDecay daemon\n\
             After=network.target\n\
             \n\
             [Service]\n\
             Type=simple\n\
             Environment=\"PATH={}\"\n\
             Environment=\"MALLOC_ARENA_MAX=2\"\n\
             ExecStart={} daemon run --socket {}\n\
             Restart=on-failure\n\
             RestartSec=2\n\
             LimitNOFILE={}\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n",
            systemd_escape_env_value(&service_path),
            self.tracedecay_bin.display(),
            self.socket_path.display(),
            DAEMON_OPEN_FILE_LIMIT,
        )
    }

    pub fn render_launchd_plist(&self) -> Result<String> {
        if !self.tracedecay_bin.is_absolute() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "launchd daemon service requires an absolute tracedecay binary path, got '{}'",
                    self.tracedecay_bin.display()
                ),
            });
        }

        let home = home_for_service_env()?;
        let data_dir = match &self.data_dir_override {
            Some(dir) => dir.clone(),
            None => tracedecay_data_dir()?,
        };
        let mut env_entries = vec![
            (
                "PATH".to_string(),
                daemon_service_path_env(&self.tracedecay_bin),
            ),
            ("HOME".to_string(), home.display().to_string()),
        ];
        if let Some(data_dir_override) = &self.data_dir_override {
            env_entries.push((
                crate::config::USER_DATA_DIR_ENV.to_string(),
                data_dir_override.display().to_string(),
            ));
        }

        let mut environment = String::new();
        for (key, value) in env_entries {
            let _ = write!(
                environment,
                "    <key>{}</key>\n    <string>{}</string>\n",
                plist_xml_escape(&key),
                plist_xml_escape(&value)
            );
        }

        Ok(format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\"\n\
             \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n\
             <dict>\n\
               <key>Label</key>\n\
               <string>{label}</string>\n\
             \n\
               <key>ProgramArguments</key>\n\
               <array>\n\
                 <string>{bin}</string>\n\
                 <string>daemon</string>\n\
                 <string>run</string>\n\
                 <string>--socket</string>\n\
                 <string>{socket}</string>\n\
               </array>\n\
             \n\
               <key>EnvironmentVariables</key>\n\
               <dict>\n\
             {environment}\
               </dict>\n\
             \n\
               <key>RunAtLoad</key>\n\
               <true/>\n\
             \n\
               <key>KeepAlive</key>\n\
               <dict>\n\
                 <key>SuccessfulExit</key>\n\
                 <false/>\n\
               </dict>\n\
             \n\
               <key>ThrottleInterval</key>\n\
               <integer>2</integer>\n\
             \n\
               <key>SoftResourceLimits</key>\n\
               <dict>\n\
                 <key>NumberOfFiles</key>\n\
                 <integer>{open_file_limit}</integer>\n\
               </dict>\n\
             \n\
               <key>StandardOutPath</key>\n\
               <string>{stdout}</string>\n\
             \n\
               <key>StandardErrorPath</key>\n\
               <string>{stderr}</string>\n\
             </dict>\n\
             </plist>\n",
            label = plist_xml_escape(LAUNCHD_LABEL),
            bin = plist_xml_escape(&self.tracedecay_bin.display().to_string()),
            socket = plist_xml_escape(&self.socket_path.display().to_string()),
            open_file_limit = DAEMON_OPEN_FILE_LIMIT,
            stdout = plist_xml_escape(&data_dir.join("daemon.out.log").display().to_string()),
            stderr = plist_xml_escape(&data_dir.join("daemon.err.log").display().to_string()),
        ))
    }

    fn render_unit(&self) -> Result<String> {
        match ServiceRunner::current()? {
            ServiceRunner::Systemd => Ok(self.render_systemd_user_unit()),
            ServiceRunner::Launchd => self.render_launchd_plist(),
            ServiceRunner::WindowsTask => windows_task::render_task_xml(self),
        }
    }
}

fn daemon_service_path_env(tracedecay_bin: &Path) -> String {
    let mut dirs = Vec::new();

    if let Some(parent) = tracedecay_bin
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        push_unique_path(&mut dirs, parent.to_path_buf());
    }

    if let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) {
        let home = PathBuf::from(home);
        push_unique_path(&mut dirs, home.join(".cargo/bin"));
        push_unique_path(&mut dirs, home.join(".local/bin"));
    }

    if let Some(path) = std::env::var_os("PATH").filter(|path| !path.is_empty()) {
        for dir in std::env::split_paths(&path) {
            push_unique_path(&mut dirs, dir);
        }
    }

    for dir in [
        "/usr/local/sbin",
        "/usr/local/bin",
        "/usr/sbin",
        "/usr/bin",
        "/sbin",
        "/bin",
        "/opt/homebrew/bin",
    ] {
        push_unique_path(&mut dirs, PathBuf::from(dir));
    }

    std::env::join_paths(&dirs).map_or_else(
        |_| {
            dirs.iter()
                .map(|path| path.to_string_lossy())
                .collect::<Vec<_>>()
                .join(":")
        },
        |path| path.to_string_lossy().into_owned(),
    )
}

fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if path.as_os_str().is_empty() || paths.iter().any(|existing| existing == &path) {
        return;
    }
    paths.push(path);
}

fn systemd_escape_env_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%")
}

fn plist_xml_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn plist_xml_unescape(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn home_for_service_env() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .ok_or_else(|| TraceDecayError::Config {
            message: "could not determine home directory for daemon service".to_string(),
        })
}

fn tracedecay_data_dir() -> Result<PathBuf> {
    crate::config::user_data_dir().ok_or_else(|| TraceDecayError::Config {
        message: "could not determine TraceDecay user data directory".to_string(),
    })
}

pub fn default_socket_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(SOCKET_ENV).filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    Ok(tracedecay_data_dir()?.join("daemon.sock"))
}

pub fn socket_path_or_default(socket: Option<String>) -> Result<PathBuf> {
    socket.map_or_else(default_socket_path, |path| Ok(PathBuf::from(path)))
}

pub fn service_spec(
    tracedecay_bin: impl Into<PathBuf>,
    socket: Option<String>,
) -> Result<DaemonServiceSpec> {
    let tracedecay_bin = tracedecay_bin.into();
    Ok(DaemonServiceSpec {
        tracedecay_bin,
        socket_path: socket_path_or_default(socket)?,
        data_dir_override: std::env::var_os(crate::config::USER_DATA_DIR_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
    })
}

#[doc(hidden)]
pub fn prepare_scoop_package_service(package_id: &str, state_file: &Path) -> Result<()> {
    windows_task::prepare_scoop_package_service(package_id, state_file)
}

#[doc(hidden)]
pub fn restore_scoop_package_service(package_id: &str, state_file: &Path) -> Result<()> {
    windows_task::restore_scoop_package_service(package_id, state_file)
}

pub fn install_service(spec: &DaemonServiceSpec, start: bool) -> Result<PathBuf> {
    let guard = QuiescedDaemonLifecycle::acquire("daemon service install")?;
    let operation_result = install_service_under_lease(spec, false);
    let restore_result = if start {
        guard.finish_with_state(DaemonServiceState::RunningEnabled)
    } else {
        guard.finish()
    };
    combine_operation_and_restore("daemon service install", operation_result, restore_result)
}

/// Install the managed service unit while the caller already holds the
/// quiesced daemon lifecycle lease. The public [`install_service`] wrapper
/// acquires that lease itself and would deadlock if called from a
/// lease-holding context.
pub fn install_service_under_lease(spec: &DaemonServiceSpec, start: bool) -> Result<PathBuf> {
    let runner = ServiceRunner::current()?;
    #[cfg(windows)]
    let new_windows_task = windows_task::service_state()? == DaemonServiceState::Missing;
    #[cfg(not(windows))]
    let new_windows_task = false;
    let operation_result = (|| {
        let materialized_spec = if matches!(runner, ServiceRunner::WindowsTask) {
            windows_task::materialize_service_spec_after_quiescence(spec)?
        } else {
            spec.clone()
        };
        let service_path = write_service_unit(&materialized_spec)?;
        runner.install(&service_path, start, &materialized_spec.socket_path)?;
        Ok(service_path)
    })();
    if operation_result.is_err() && new_windows_task {
        let rollback_result = windows_task::rollback_new_registration();
        return combine_operation_and_restore(
            "install new Windows daemon task",
            operation_result,
            rollback_result,
        );
    }
    operation_result
}

pub fn refresh_service(spec: &DaemonServiceSpec) -> Result<PathBuf> {
    let guard = QuiescedDaemonLifecycle::acquire("daemon service refresh")?;
    let desired_state = match guard.previous_state() {
        DaemonServiceState::Missing => DaemonServiceState::RunningEnabled,
        state => state,
    };
    let stopped_state = match desired_state {
        DaemonServiceState::RunningEnabled => DaemonServiceState::StoppedEnabled,
        DaemonServiceState::RunningDisabled => DaemonServiceState::StoppedDisabled,
        state => state,
    };
    let runner = ServiceRunner::current()?;
    let operation_result = refresh_service_with_runner(&runner, spec, stopped_state);
    let restore_result = guard.finish_with_state(desired_state);
    combine_operation_and_restore("daemon service refresh", operation_result, restore_result)
}

fn refresh_service_with_runner(
    runner: &ServiceRunner,
    spec: &DaemonServiceSpec,
    previous_state: DaemonServiceState,
) -> Result<PathBuf> {
    if matches!(runner, ServiceRunner::Systemd) && previous_state == DaemonServiceState::Masked {
        let service_path = service_unit_path()?;
        if std::fs::read_link(&service_path).is_ok_and(|target| target == Path::new("/dev/null")) {
            return Err(TraceDecayError::Config {
                message: format!(
                    "TraceDecay daemon service '{}' is persistently masked; preserved the /dev/null mask and skipped rewriting it",
                    service_path.display()
                ),
            });
        }
    }
    let materialized_spec = if matches!(runner, ServiceRunner::WindowsTask) {
        windows_task::materialize_service_spec_after_quiescence(spec)?
    } else {
        spec.clone()
    };
    let service_path = write_service_unit(&materialized_spec)?;
    runner.refresh(
        &service_path,
        &materialized_spec.socket_path,
        previous_state,
    )?;
    Ok(service_path)
}

pub fn refresh_installed_service(spec: &DaemonServiceSpec) -> Result<Option<PathBuf>> {
    with_quiesced_installed_service("daemon service refresh", |_| {
        refresh_installed_service_under_lease(spec)
    })
}

#[doc(hidden)]
pub fn refresh_installed_service_under_lease(spec: &DaemonServiceSpec) -> Result<Option<PathBuf>> {
    refresh_installed_service_with_state(spec, None)
}

#[doc(hidden)]
pub fn refresh_installed_service_under_lease_with_state(
    spec: &DaemonServiceSpec,
    previous_state: DaemonServiceState,
) -> Result<Option<PathBuf>> {
    refresh_installed_service_with_state(spec, Some(previous_state))
}

fn refresh_installed_service_with_state(
    spec: &DaemonServiceSpec,
    previous_state: Option<DaemonServiceState>,
) -> Result<Option<PathBuf>> {
    if !cfg!(any(target_os = "linux", target_os = "macos", windows)) {
        return Ok(None);
    }
    let service_path = service_unit_path()?;
    if !service_unit_exists(&service_path)? {
        return Ok(None);
    }
    let unit = read_service_unit(&service_path)?;
    let mut refreshed_spec = spec.clone();
    if let Some(socket_path) = socket_path_from_unit_text(&unit) {
        refreshed_spec.socket_path = socket_path;
    }
    let runner = ServiceRunner::current()?;
    let previous_state = match previous_state {
        Some(state) => state,
        None => runner.service_state(&refreshed_spec.socket_path)?,
    };
    if matches!(runner, ServiceRunner::Launchd) {
        // The installed plist is the source of truth for the daemon's data
        // directory; the refreshing shell may not have the override set.
        refreshed_spec.data_dir_override =
            launchd_plist_env_value(&unit, crate::config::USER_DATA_DIR_ENV).map(PathBuf::from);
    } else if matches!(runner, ServiceRunner::WindowsTask) {
        refreshed_spec.data_dir_override = windows_task::profile_root_from_task_xml(&unit);
    }
    refresh_service_with_runner(&runner, &refreshed_spec, previous_state).map(Some)
}

/// Stops the managed daemon before an exclusive lifecycle lease is acquired.
/// The daemon owns a shared lifecycle lease for its lifetime, so the order is
/// intentionally stop-then-lock.
#[doc(hidden)]
pub fn quiesce_installed_service_before_lease() -> Result<DaemonServiceState> {
    if !cfg!(any(target_os = "linux", target_os = "macos", windows)) {
        return Ok(DaemonServiceState::Missing);
    }
    let service_path = service_unit_path()?;
    if !service_unit_exists(&service_path)? {
        let socket_path = default_socket_path()?;
        let socket_state = daemon_socket_state(&socket_path);
        if !socket_state.is_proven_quiesced() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "refusing post-update mutations: unmanaged daemon socket '{}' is {socket_state}; stop the daemon and retry",
                    socket_path.display()
                ),
            });
        }
        return Ok(DaemonServiceState::Missing);
    }
    let unit = read_service_unit(&service_path)?;
    let socket_path = socket_path_from_unit_text(&unit).unwrap_or(default_socket_path()?);
    let runner = ServiceRunner::current()?;
    let state = runner.service_state(&socket_path)?;
    if !state.is_running() {
        let socket_state = daemon_socket_state(&socket_path);
        if !socket_state.is_proven_quiesced() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "refusing post-update mutations: unmanaged daemon socket '{}' is {socket_state}; stop the daemon and retry",
                    socket_path.display(),
                ),
            });
        }
        return Ok(state);
    }
    runner.stop_for_update()?;
    Ok(state)
}

/// Verifies that pre-lease quiescence still holds. This never stops or starts
/// a service while the caller owns the exclusive lifecycle lease.
#[doc(hidden)]
pub fn verify_installed_service_quiesced_under_lease() -> Result<DaemonServiceState> {
    if !cfg!(any(target_os = "linux", target_os = "macos", windows)) {
        return Ok(DaemonServiceState::Missing);
    }
    let service_path = service_unit_path()?;
    if !service_unit_exists(&service_path)? {
        let socket_path = default_socket_path()?;
        let socket_state = daemon_socket_state(&socket_path);
        if !socket_state.is_proven_quiesced() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "refusing post-update mutations: unmanaged daemon socket '{}' became {socket_state} after pre-lease quiescence",
                    socket_path.display()
                ),
            });
        }
        return Ok(DaemonServiceState::Missing);
    }
    let unit = read_service_unit(&service_path)?;
    let socket_path = socket_path_from_unit_text(&unit).unwrap_or(default_socket_path()?);
    let runner = ServiceRunner::current()?;
    let state = runner.service_state(&socket_path)?;
    let socket_state = daemon_socket_state(&socket_path);
    if state.is_running() || !socket_state.is_proven_quiesced() {
        return Err(TraceDecayError::Config {
            message: format!(
                "refusing post-update mutations: TraceDecay daemon service is {state:?} and socket '{}' is {socket_state} after pre-lease quiescence",
                socket_path.display()
            ),
        });
    }
    Ok(state)
}

/// Restores the exact running/disabled state captured before maintenance.
/// Callers hold a shared lifecycle lease, never the exclusive mutation lease.
#[doc(hidden)]
pub fn restore_installed_service_after_update(previous_state: DaemonServiceState) -> Result<()> {
    if !previous_state.is_running() || !cfg!(any(target_os = "linux", target_os = "macos", windows))
    {
        return Ok(());
    }
    let service_path = service_unit_path()?;
    if !service_unit_exists(&service_path)? {
        return Err(TraceDecayError::Config {
            message: format!(
                "cannot restore TraceDecay daemon state: service unit '{}' is missing",
                service_path.display()
            ),
        });
    }
    let unit = read_service_unit(&service_path)?;
    let socket_path = socket_path_from_unit_text(&unit).unwrap_or(default_socket_path()?);
    ServiceRunner::current()?.restore_after_update(&service_path, &socket_path, previous_state)
}

fn restore_installed_service_after_failed_acquire(
    previous_state: DaemonServiceState,
) -> Result<()> {
    if !previous_state.is_running() {
        return Ok(());
    }
    let _lifecycle_lease = crate::lifecycle_lease::acquire_shared_blocking("daemon state restore")?;
    restore_installed_service_after_update(previous_state)
}

pub fn uninstall_service(stop: bool) -> Result<PathBuf> {
    if !stop {
        let state = installed_service_state()?;
        if state.is_running() {
            return Err(TraceDecayError::Config {
                message: "cannot uninstall the daemon service with --no-stop while the managed daemon is running; stop it first or omit --no-stop".to_string(),
            });
        }
        let _lifecycle_lease =
            crate::lifecycle_lease::acquire_exclusive("daemon service uninstall --no-stop")?;
        verify_installed_service_quiesced_under_lease()?;
        return uninstall_service_under_lease(false);
    }
    let guard = QuiescedDaemonLifecycle::acquire("daemon service uninstall")?;
    let operation_result = uninstall_service_under_lease(true);
    guard.finish_without_restore();
    operation_result
}

fn installed_service_state() -> Result<DaemonServiceState> {
    let service_path = service_unit_path()?;
    if !service_unit_exists(&service_path)? {
        return Ok(DaemonServiceState::Missing);
    }
    let unit = read_service_unit(&service_path)?;
    let socket_path = socket_path_from_unit_text(&unit).unwrap_or(default_socket_path()?);
    ServiceRunner::current()?.service_state(&socket_path)
}

pub fn start_service() -> Result<()> {
    let service_path = service_unit_path()?;
    if !service_unit_exists(&service_path)? {
        return Err(TraceDecayError::Config {
            message: "no TraceDecay daemon service is installed".to_string(),
        });
    }
    let unit = read_service_unit(&service_path)?;
    let socket_path = socket_path_from_unit_text(&unit).unwrap_or(default_socket_path()?);
    ServiceRunner::current()?.start(&service_path, &socket_path)
}

pub fn stop_service() -> Result<()> {
    if matches!(installed_service_state()?, DaemonServiceState::Missing) {
        return Err(TraceDecayError::Config {
            message: "no TraceDecay daemon service is installed".to_string(),
        });
    }
    ServiceRunner::current()?.stop()
}

/// Waits for a strict maintenance command to observe the exact managed-service
/// state it captured before quiescence. Running services must also accept a
/// `TraceDecay` protocol request from the socket configured in their installed
/// unit and identify as the current installed version; stopped or missing
/// services must remain quiescent.
pub fn wait_for_installed_service_state(expected: DaemonServiceState) -> Result<()> {
    // A freshly restored daemon may legitimately spend a while on startup
    // recovery (schema migrations, projection rebuilds, transcript catch-up)
    // before it answers its first initialize, so the restoration window is
    // generous — bounded, with progress visibility — rather than a snap
    // judgement that fails a healthy, still-converging service.
    //
    // The bound is a wall-clock deadline, not an attempt count: each attempt
    // calls into `query_daemon_identity`, which itself has a per-probe
    // timeout for a connect-but-never-answer daemon. An attempt-count bound
    // multiplies that per-probe timeout by the attempt count in the worst
    // case, which can stretch total wait time (and the progress-message
    // cadence) far past what the comment above promises. Bounding by
    // elapsed wall-clock time keeps the overall wait — and how often we
    // report progress — independent of per-probe cost.
    const TOTAL_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(3);
    const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);
    const PROGRESS_INTERVAL: std::time::Duration = std::time::Duration::from_secs(20);

    let deadline = std::time::Instant::now() + TOTAL_TIMEOUT;
    let mut last = installed_service_status_snapshot()?;
    let mut last_progress = std::time::Instant::now();
    loop {
        let (actual, _, socket_state, protocol_state) = &last;
        if restored_service_matches(expected, *actual, *socket_state, protocol_state) {
            return Ok(());
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            break;
        }
        if now.duration_since(last_progress) >= PROGRESS_INTERVAL {
            eprintln!(
                "Waiting for the restored daemon to reach {expected:?} (service {actual:?}, socket {socket_state}, protocol {protocol_state})…"
            );
            last_progress = now;
        }
        std::thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(now)));
        last = installed_service_status_snapshot()?;
    }

    let (actual, socket_path, socket_state, protocol_state) = last;
    Err(TraceDecayError::Config {
        message: format!(
            "TraceDecay daemon did not return to {expected:?}: service is {actual:?}, socket '{}' is {socket_state}, and protocol readiness is {protocol_state}",
            socket_path.display()
        ),
    })
}

fn installed_service_status_snapshot() -> Result<(
    DaemonServiceState,
    PathBuf,
    DaemonSocketState,
    DaemonProtocolState,
)> {
    let service_path = service_unit_path()?;
    if !service_unit_exists(&service_path)? {
        let socket_path = default_socket_path()?;
        let socket_state = daemon_socket_state(&socket_path);
        return Ok((
            DaemonServiceState::Missing,
            socket_path,
            socket_state,
            DaemonProtocolState::NotRequired,
        ));
    }
    let unit = read_service_unit(&service_path)?;
    let socket_path = socket_path_from_unit_text(&unit).unwrap_or(default_socket_path()?);
    let actual = ServiceRunner::current()?.service_state(&socket_path)?;
    let socket_state = daemon_socket_state(&socket_path);
    let protocol_state = if actual.is_running() {
        daemon_protocol_state(&socket_path)
    } else {
        DaemonProtocolState::NotRequired
    };
    Ok((actual, socket_path, socket_state, protocol_state))
}

fn restored_service_matches(
    expected: DaemonServiceState,
    actual: DaemonServiceState,
    socket_state: DaemonSocketState,
    protocol_state: &DaemonProtocolState,
) -> bool {
    if actual != expected {
        return false;
    }
    if expected.is_running() {
        matches!(socket_state, DaemonSocketState::Connectable)
            && matches!(protocol_state, DaemonProtocolState::Ready)
    } else {
        socket_state.is_proven_quiesced()
    }
}

fn combine_operation_and_restore<T>(
    operation: &str,
    operation_result: Result<T>,
    restore_result: Result<()>,
) -> Result<T> {
    match (operation_result, restore_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(operation_error), Err(restore_error)) => Err(TraceDecayError::Config {
            message: format!(
                "{operation} failed: {operation_error}; daemon state restoration also failed: {restore_error}"
            ),
        }),
    }
}

fn uninstall_service_under_lease(stop: bool) -> Result<PathBuf> {
    let runner = ServiceRunner::current()?;
    let service_path = service_unit_path()?;
    runner.before_uninstall(stop)?;
    remove_service_unit(&service_path)?;
    runner.after_uninstall(stop);
    Ok(service_path)
}

pub fn service_status(socket_path: &Path) -> String {
    let transport_path = if cfg!(unix) {
        socket_path.to_path_buf()
    } else {
        installed_service_socket_path()
            .ok()
            .flatten()
            .unwrap_or_else(|| socket_path.to_path_buf())
    };
    let socket_state = daemon_socket_state(&transport_path);
    let service = service_unit_path().map_or_else(
        |e| format!("unavailable: {e}"),
        |path| path.display().to_string(),
    );
    let runner = ServiceRunner::current();
    let state = runner.as_ref().map_or_else(
        |error| format!("unavailable: {error}"),
        |runner| {
            runner.service_state(&transport_path).map_or_else(
                |error| format!("unavailable: {error}"),
                |state| format!("{state:?}"),
            )
        },
    );
    let detail = runner
        .as_ref()
        .ok()
        .and_then(ServiceRunner::service_detail_hint)
        .map(|hint| format!("service-detail: {hint}\n"))
        .unwrap_or_default();
    let logs = runner.map_or_else(|e| format!("unavailable: {e}"), |runner| runner.log_hint());
    let transport_kind = if cfg!(unix) { "socket" } else { "endpoint" };
    let transport = daemon_transport_display(&transport_path);
    format!(
        "service: {service}\nstate: {state}\n{transport_kind}: {transport} ({socket_state})\n{detail}logs: {logs}\n",
    )
}
