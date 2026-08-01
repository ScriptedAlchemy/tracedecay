use std::path::{Path, PathBuf};

#[cfg(any(windows, test))]
use serde::{Deserialize, Serialize};

use crate::errors::{Result, TraceDecayError};

use super::{DaemonServiceSpec, DaemonServiceState};

#[cfg(any(windows, test))]
const TASK_NAME_PREFIX: &str = "TraceDecay Daemon";
#[cfg(any(windows, test))]
const BETA_TASK_NAME_PREFIX: &str = "TraceDecay Beta Daemon";
#[cfg(any(windows, test))]
const SCOOP_STATE_SCHEMA: &str = "tracedecay.scoop-service-state.v1";
#[cfg(any(windows, test))]
const SCOOP_STATE_FILE_NAME: &str = "scoop-state.json";

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum WindowsPackageId {
    #[serde(rename = "tracedecay")]
    Stable,
    #[serde(rename = "tracedecay-beta")]
    Beta,
}

#[cfg(any(windows, test))]
impl WindowsPackageId {
    #[cfg(windows)]
    fn parse(value: &str) -> Result<Self> {
        match value {
            "tracedecay" => Ok(Self::Stable),
            "tracedecay-beta" => Ok(Self::Beta),
            _ => Err(TraceDecayError::Config {
                message: format!(
                    "invalid Scoop package id '{value}'; expected tracedecay or tracedecay-beta"
                ),
            }),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "tracedecay",
            Self::Beta => "tracedecay-beta",
        }
    }

    const fn task_name_prefix(self) -> &'static str {
        match self {
            Self::Stable => TASK_NAME_PREFIX,
            Self::Beta => BETA_TASK_NAME_PREFIX,
        }
    }
}

#[cfg(any(windows, test))]
#[derive(Clone, Debug, PartialEq, Eq)]
struct ServiceRuntimeLayout {
    directory: PathBuf,
    executable: PathBuf,
    state_file: PathBuf,
}

#[cfg(any(windows, test))]
impl ServiceRuntimeLayout {
    fn below(local_app_data: &Path, package_id: WindowsPackageId) -> Self {
        let directory = local_app_data
            .join("TraceDecay")
            .join("service")
            .join(package_id.as_str());
        Self {
            executable: directory.join("tracedecay.exe"),
            state_file: directory.join(SCOOP_STATE_FILE_NAME),
            directory,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TaskIdentity {
    #[cfg(any(windows, test))]
    package_id: WindowsPackageId,
    user_sid: String,
    task_name: String,
    task_path: String,
    #[cfg(any(windows, test))]
    sddl: String,
}

impl TaskIdentity {
    fn current() -> Result<Self> {
        #[cfg(windows)]
        {
            let package_id = std::env::current_exe()
                .ok()
                .and_then(|path| package_id_from_executable(&path))
                .unwrap_or(WindowsPackageId::Stable);
            let user_sid = tracedecay_runtime_core::windows_security::current_user_sid_string()
                .map_err(|error| TraceDecayError::Config {
                    message: format!("could not determine current Windows user SID: {error}"),
                })?;
            Self::for_package_user_sid(package_id, &user_sid)
        }
        #[cfg(not(windows))]
        {
            Err(TraceDecayError::Config {
                message: "Windows Task Scheduler identity is unavailable on this platform"
                    .to_string(),
            })
        }
    }

    #[cfg(test)]
    fn for_user_sid(user_sid: &str) -> Result<Self> {
        Self::for_package_user_sid(WindowsPackageId::Stable, user_sid)
    }

    #[cfg(any(windows, test))]
    fn for_package_user_sid(package_id: WindowsPackageId, user_sid: &str) -> Result<Self> {
        let mut components = user_sid.split('-');
        let valid = components.next() == Some("S")
            && components.clone().count() >= 2
            && components.all(|component| {
                !component.is_empty() && component.bytes().all(|byte| byte.is_ascii_digit())
            });
        if !valid {
            return Err(TraceDecayError::Config {
                message: format!("current Windows user SID '{user_sid}' is not canonical"),
            });
        }
        let task_name = format!("{} ({user_sid})", package_id.task_name_prefix());
        Ok(Self {
            package_id,
            user_sid: user_sid.to_string(),
            task_path: format!(r"\{task_name}"),
            task_name,
            sddl: format!("O:{user_sid}D:P(A;;GA;;;SY)(A;;GA;;;{user_sid})"),
        })
    }
}

#[cfg(any(windows, test))]
fn package_id_from_executable(executable: &Path) -> Option<WindowsPackageId> {
    let file_name = executable.file_name()?.to_str()?;
    if !file_name.eq_ignore_ascii_case("tracedecay.exe") {
        return None;
    }
    if executable
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("shims"))
    {
        return Some(WindowsPackageId::Stable);
    }

    let components = executable
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();
    for pair in components.windows(2) {
        if pair[0].eq_ignore_ascii_case("apps") {
            if pair[1].eq_ignore_ascii_case(WindowsPackageId::Stable.as_str()) {
                return Some(WindowsPackageId::Stable);
            }
            if pair[1].eq_ignore_ascii_case(WindowsPackageId::Beta.as_str()) {
                return Some(WindowsPackageId::Beta);
            }
        }
        if pair[0].eq_ignore_ascii_case("service") {
            if pair[1].eq_ignore_ascii_case(WindowsPackageId::Stable.as_str()) {
                return Some(WindowsPackageId::Stable);
            }
            if pair[1].eq_ignore_ascii_case(WindowsPackageId::Beta.as_str()) {
                return Some(WindowsPackageId::Beta);
            }
        }
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TaskSnapshot {
    running: bool,
    enabled: bool,
}

#[cfg(any(windows, test))]
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ScoopTaskAction {
    executable: PathBuf,
    arguments: String,
}

#[cfg(any(windows, test))]
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ScoopServiceState {
    schema: String,
    package_id: WindowsPackageId,
    user_sid: String,
    task_name: String,
    task_path: String,
    task_sddl: String,
    task_xml: String,
    action: ScoopTaskAction,
    profile_root: PathBuf,
    enabled: bool,
    running: bool,
}

#[cfg(any(windows, test))]
impl ScoopServiceState {
    fn capture(
        package_id: WindowsPackageId,
        identity: &TaskIdentity,
        snapshot: TaskSnapshot,
        task_xml: String,
        task_sddl: String,
    ) -> Result<Self> {
        if !task_definition_is_owned(&task_xml, &task_sddl, identity) {
            return Err(foreign_task(identity));
        }
        let action = task_action_from_xml(&task_xml).ok_or_else(|| foreign_task(identity))?;
        if package_id_from_executable(&action.executable) != Some(package_id) {
            return Err(foreign_task(identity));
        }
        let profile_root =
            profile_root_from_task_xml(&task_xml).ok_or_else(|| foreign_task(identity))?;
        Ok(Self {
            schema: SCOOP_STATE_SCHEMA.to_string(),
            package_id,
            user_sid: identity.user_sid.clone(),
            task_name: identity.task_name.clone(),
            task_path: identity.task_path.clone(),
            task_sddl,
            task_xml,
            action,
            profile_root,
            enabled: snapshot.enabled,
            running: snapshot.running,
        })
    }

    fn validate(&self, package_id: WindowsPackageId, identity: &TaskIdentity) -> Result<()> {
        if self.schema != SCOOP_STATE_SCHEMA {
            return Err(invalid_state("schema does not match"));
        }
        if self.package_id != package_id {
            return Err(invalid_state("package id does not match"));
        }
        if self.user_sid != identity.user_sid
            || self.task_name != identity.task_name
            || self.task_path != identity.task_path
        {
            return Err(invalid_state(
                "Windows SID or package-scoped task identity does not match",
            ));
        }
        if !task_definition_is_owned(&self.task_xml, &self.task_sddl, identity) {
            return Err(invalid_state(
                "task XML or security descriptor is not privately owned",
            ));
        }
        let action = task_action_from_xml(&self.task_xml)
            .ok_or_else(|| invalid_state("task XML has no exact executable action"))?;
        if action != self.action
            || package_id_from_executable(&action.executable) != Some(package_id)
        {
            return Err(invalid_state(
                "task action does not match the snapshotted package action",
            ));
        }
        if profile_root_from_task_xml(&self.task_xml).as_ref() != Some(&self.profile_root) {
            return Err(invalid_state(
                "task profile does not match the snapshotted profile",
            ));
        }
        Ok(())
    }

    const fn desired_state(&self) -> DaemonServiceState {
        match (self.running, self.enabled) {
            (true, true) => DaemonServiceState::RunningEnabled,
            (true, false) => DaemonServiceState::RunningDisabled,
            (false, true) => DaemonServiceState::StoppedEnabled,
            (false, false) => DaemonServiceState::StoppedDisabled,
        }
    }
}

trait TaskSchedulerApi {
    fn snapshot(&mut self) -> Result<Option<TaskSnapshot>>;
    fn registered_xml(&mut self) -> Result<Option<String>>;
    #[cfg(windows)]
    fn registered_sddl(&mut self) -> Result<Option<String>>;
    fn register_xml(&mut self, xml: &str) -> Result<()>;
    #[cfg(windows)]
    fn register_xml_with_sddl(&mut self, xml: &str, sddl: &str) -> Result<()> {
        let _ = sddl;
        self.register_xml(xml)
    }
    fn set_enabled(&mut self, enabled: bool) -> Result<()>;
    fn disable_for_rollback(&mut self) -> Result<()>;
    fn run(&mut self) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
    fn delete(&mut self) -> Result<()>;
}

#[cfg(any(windows, test))]
#[derive(Debug)]
struct ControlObservation {
    satisfied: bool,
    diagnostic: String,
}

#[cfg(any(windows, test))]
#[derive(Debug)]
enum ShutdownRequestAttempt {
    Acknowledged,
    SentWithoutAcknowledgement(String),
    NotSent(String),
}

#[cfg(any(windows, test))]
impl ShutdownRequestAttempt {
    fn may_have_been_delivered(&self) -> bool {
        !matches!(self, Self::NotSent(_))
    }

    fn diagnostic(&self) -> String {
        match self {
            Self::Acknowledged => "acknowledged".to_string(),
            Self::SentWithoutAcknowledgement(error) => {
                format!("sent without acknowledgement ({error})")
            }
            Self::NotSent(error) => format!("not sent ({error})"),
        }
    }
}

#[cfg(any(windows, test))]
trait DaemonControlApi {
    fn request_shutdown(&mut self) -> ShutdownRequestAttempt;
    fn readiness(&mut self, timeout: std::time::Duration) -> ControlObservation;
    fn quiescence(&mut self, timeout: std::time::Duration) -> ControlObservation;
    fn elapsed(&self) -> std::time::Duration;
    fn wait(&mut self, duration: std::time::Duration);
}

#[cfg(any(windows, test))]
const LIFECYCLE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
#[cfg(any(windows, test))]
const CONTROL_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(750);
#[cfg(any(windows, test))]
const START_READINESS_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(3);
#[cfg(any(windows, test))]
const GRACEFUL_STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);
#[cfg(any(windows, test))]
const HARD_STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[cfg(windows)]
struct NativeDaemonControl {
    transport_hint: PathBuf,
    clock_origin: std::time::Instant,
}

#[cfg(windows)]
impl DaemonControlApi for NativeDaemonControl {
    fn request_shutdown(&mut self) -> ShutdownRequestAttempt {
        match super::probe::request_daemon_shutdown(&self.transport_hint) {
            Ok(super::probe::DaemonShutdownRequest::Acknowledged) => {
                ShutdownRequestAttempt::Acknowledged
            }
            Ok(super::probe::DaemonShutdownRequest::SentWithoutAcknowledgement(error)) => {
                ShutdownRequestAttempt::SentWithoutAcknowledgement(error)
            }
            Err(error) => ShutdownRequestAttempt::NotSent(error.to_string()),
        }
    }

    fn readiness(&mut self, timeout: std::time::Duration) -> ControlObservation {
        let protocol =
            super::probe::daemon_protocol_state_with_timeout(&self.transport_hint, timeout);
        ControlObservation {
            satisfied: matches!(protocol, super::probe::DaemonProtocolState::Ready),
            diagnostic: format!("protocol {protocol}"),
        }
    }

    fn quiescence(&mut self, timeout: std::time::Duration) -> ControlObservation {
        let socket = super::probe::daemon_socket_state_with_timeout(&self.transport_hint, timeout);
        ControlObservation {
            satisfied: socket.is_proven_quiesced(),
            diagnostic: format!("endpoint {socket}"),
        }
    }

    fn elapsed(&self) -> std::time::Duration {
        self.clock_origin.elapsed()
    }

    fn wait(&mut self, duration: std::time::Duration) {
        std::thread::sleep(duration);
    }
}

pub(super) fn task_name() -> Result<String> {
    Ok(TaskIdentity::current()?.task_name)
}

pub(super) fn task_path() -> Result<PathBuf> {
    Ok(PathBuf::from(TaskIdentity::current()?.task_path))
}

pub(super) fn render_task_xml(spec: &DaemonServiceSpec) -> Result<String> {
    render_task_xml_for(spec, &TaskIdentity::current()?)
}

fn render_task_xml_for(spec: &DaemonServiceSpec, identity: &TaskIdentity) -> Result<String> {
    let profile_root = match &spec.data_dir_override {
        Some(profile_root) => profile_root.clone(),
        None => super::tracedecay_data_dir()?,
    };
    let profile_root = fully_qualified_windows_path(&profile_root, "daemon profile root")?;
    let executable_path = fully_qualified_windows_path(&spec.tracedecay_bin, "daemon executable")?;
    let executable_text = windows_path_text(&executable_path, "daemon executable")?;
    validate_task_command_text(executable_text)?;
    let executable = xml_escape(executable_text);
    let arguments = xml_escape(&format!(
        "daemon run --profile-root {}",
        quote_windows_argument(windows_path_text(&profile_root, "daemon profile root")?)
    ));
    let user_sid = xml_escape(&identity.user_sid);

    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>TraceDecay daemon</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user_sid}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user_sid}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <StartWhenAvailable>true</StartWhenAvailable>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>255</Count>
    </RestartOnFailure>
    <Enabled>true</Enabled>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{executable}</Command>
      <Arguments>{arguments}</Arguments>
    </Exec>
  </Actions>
</Task>
"#
    ))
}

fn windows_path_text<'a>(path: &'a Path, description: &str) -> Result<&'a str> {
    path.to_str().ok_or_else(|| TraceDecayError::Config {
        message: format!(
            "Windows Task Scheduler {description} path '{}' is not valid Unicode",
            path.display()
        ),
    })
}

fn fully_qualified_windows_path(path: &Path, description: &str) -> Result<PathBuf> {
    #[cfg(windows)]
    {
        if matches!(
            path.components().next(),
            Some(std::path::Component::Prefix(_))
        ) && !path.has_root()
        {
            return Err(TraceDecayError::Config {
                message: format!(
                    "Windows Task Scheduler {description} path '{}' is drive-relative",
                    path.display()
                ),
            });
        }
        let absolute = std::path::absolute(path).map_err(|error| TraceDecayError::Config {
            message: format!(
                "could not fully qualify Windows Task Scheduler {description} path '{}': {error}",
                path.display()
            ),
        })?;
        if !absolute.is_absolute() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "Windows Task Scheduler {description} path '{}' is not fully qualified",
                    path.display()
                ),
            });
        }
        Ok(absolute)
    }
    #[cfg(not(windows))]
    {
        let _ = description;
        Ok(path.to_path_buf())
    }
}

fn validate_task_command_text(command: &str) -> Result<()> {
    const TASK_COMMAND_UTF16_LIMIT: usize = 260;
    let length = command.encode_utf16().count();
    if length > TASK_COMMAND_UTF16_LIMIT {
        return Err(TraceDecayError::Config {
            message: format!(
                "Windows Task Scheduler daemon executable exceeds the 260 UTF-16 code units allowed by task XML (got {length})"
            ),
        });
    }
    Ok(())
}

pub(super) fn profile_root_from_task_xml(xml: &str) -> Option<PathBuf> {
    let arguments = xml_element_text(xml, "Arguments")?;
    let arguments = xml_unescape(arguments);
    let tokens = windows_argument_tokens(&arguments);
    let mut tokens = tokens.iter();
    while let Some(token) = tokens.next() {
        if token == "--profile-root" {
            return tokens.next().map(PathBuf::from);
        }
        if let Some(value) = token.strip_prefix("--profile-root=") {
            return Some(PathBuf::from(value));
        }
    }
    None
}

pub(super) fn materialize_service_spec_after_quiescence(
    spec: &DaemonServiceSpec,
) -> Result<DaemonServiceSpec> {
    #[cfg(windows)]
    {
        let Some(package_id) = package_id_from_executable(&spec.tracedecay_bin) else {
            return Ok(spec.clone());
        };
        let layout = local_runtime_layout(package_id)?;
        ensure_private_runtime_layout(&layout)?;
        if !windows_paths_equal(&spec.tracedecay_bin, &layout.executable)? {
            let source = scoop_service_source(&spec.tracedecay_bin, package_id)?;
            atomic_copy_private_executable(&source, &layout.executable)?;
        } else {
            tracedecay_runtime_core::windows_security::validate_private_file(&layout.executable)
                .map_err(|error| {
                    secure_path_error("validate service executable", &layout.executable, error)
                })?;
        }
        let mut materialized = spec.clone();
        materialized.tracedecay_bin = layout.executable;
        Ok(materialized)
    }
    #[cfg(not(windows))]
    {
        Ok(spec.clone())
    }
}

#[cfg(windows)]
fn scoop_service_source(executable: &Path, package_id: WindowsPackageId) -> Result<PathBuf> {
    if executable
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("shims"))
    {
        let scoop_root =
            executable
                .parent()
                .and_then(Path::parent)
                .ok_or_else(|| TraceDecayError::Config {
                    message: format!("Scoop shim '{}' has no package root", executable.display()),
                })?;
        let current = scoop_root
            .join("apps")
            .join(package_id.as_str())
            .join("current")
            .join("tracedecay.exe");
        if !current.is_file() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "Scoop package executable '{}' does not exist",
                    current.display()
                ),
            });
        }
        return Ok(current);
    }
    Ok(executable.to_path_buf())
}

#[cfg(windows)]
fn local_runtime_layout(package_id: WindowsPackageId) -> Result<ServiceRuntimeLayout> {
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| TraceDecayError::Config {
            message: "LOCALAPPDATA is required for the Windows daemon service runtime".to_string(),
        })?;
    let local_app_data = fully_qualified_windows_path(&local_app_data, "LOCALAPPDATA")?;
    Ok(ServiceRuntimeLayout::below(&local_app_data, package_id))
}

#[cfg(windows)]
fn ensure_private_runtime_layout(layout: &ServiceRuntimeLayout) -> Result<()> {
    let local_app_data =
        layout
            .directory
            .ancestors()
            .nth(3)
            .ok_or_else(|| TraceDecayError::Config {
                message: format!(
                    "Windows service runtime '{}' has no LOCALAPPDATA ancestor",
                    layout.directory.display()
                ),
            })?;
    tracedecay_runtime_core::windows_security::validate_directory_path(local_app_data)
        .map_err(|error| secure_path_error("validate LOCALAPPDATA", local_app_data, error))?;

    let tracedecay_dir = local_app_data.join("TraceDecay");
    let service_dir = tracedecay_dir.join("service");
    for directory in [&tracedecay_dir, &service_dir, &layout.directory] {
        tracedecay_runtime_core::windows_security::create_private_directory(directory).map_err(
            |error| secure_path_error("create private service directory", directory, error),
        )?;
    }
    Ok(())
}

#[cfg(windows)]
fn atomic_copy_private_executable(source: &Path, destination: &Path) -> Result<()> {
    use std::io::{Read, Write};
    use std::sync::atomic::Ordering;

    let file_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("tracedecay.exe");
    let sequence = super::SERVICE_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = destination.with_file_name(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));
    let operation = (|| {
        let mut input = std::fs::File::open(source).map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to open Scoop service source '{}': {error}",
                source.display()
            ),
        })?;
        let mut output = tracedecay_runtime_core::windows_security::create_private_file(&temporary)
            .map_err(|error| {
                secure_path_error("create service executable temporary", &temporary, error)
            })?;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = input
                .read(&mut buffer)
                .map_err(|error| TraceDecayError::Config {
                    message: format!(
                        "failed to read Scoop service source '{}': {error}",
                        source.display()
                    ),
                })?;
            if read == 0 {
                break;
            }
            output.write_all(&buffer[..read]).map_err(|error| {
                secure_path_error("write service executable temporary", &temporary, error)
            })?;
        }
        output.sync_all().map_err(|error| {
            secure_path_error("sync service executable temporary", &temporary, error)
        })?;
        drop(output);
        tracedecay_runtime_core::db::DatabaseAuthority::replace_file_atomically(
            &temporary,
            destination,
            "Scoop service executable",
        )?;
        tracedecay_runtime_core::windows_security::validate_private_file(destination)
            .map_err(|error| secure_path_error("validate service executable", destination, error))
    })();
    if operation.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    operation
}

#[cfg(windows)]
fn windows_paths_equal(left: &Path, right: &Path) -> Result<bool> {
    let left = windows_path_text(left, "path comparison")?;
    let right = windows_path_text(right, "path comparison")?;
    Ok(left.eq_ignore_ascii_case(right))
}

#[cfg(windows)]
fn secure_path_error(operation: &str, path: &Path, error: std::io::Error) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("{operation} '{}': {error}", path.display()),
    }
}

pub(super) fn task_exists() -> Result<bool> {
    with_platform_api(|api| Ok(api.snapshot()?.is_some()))
}

pub(super) fn service_state() -> Result<DaemonServiceState> {
    with_platform_api(|api| Ok(state_from_snapshot(api.snapshot()?)))
}

pub(super) fn register_task_xml(xml: &str) -> Result<()> {
    with_platform_api(|api| register_task_xml_with(api, xml))
}

pub(super) fn registered_task_xml() -> Result<Option<String>> {
    with_platform_api(|api| api.registered_xml())
}

pub(super) fn apply_state(state: DaemonServiceState) -> Result<()> {
    #[cfg(any(windows, test))]
    {
        if state == DaemonServiceState::Missing {
            return with_platform_api(delete_with);
        }
        with_platform_control_api(|api, control| apply_managed_state_with(api, control, state))
    }
    #[cfg(not(any(windows, test)))]
    {
        let _ = state;
        control_api_unavailable()
    }
}

pub(super) fn start() -> Result<()> {
    #[cfg(any(windows, test))]
    {
        with_platform_control_api(start_managed_with)
    }
    #[cfg(not(any(windows, test)))]
    {
        control_api_unavailable()
    }
}

pub(super) fn stop() -> Result<()> {
    #[cfg(any(windows, test))]
    {
        with_platform_control_api(stop_managed_with)
    }
    #[cfg(not(any(windows, test)))]
    {
        control_api_unavailable()
    }
}

pub(super) fn deactivate() -> Result<()> {
    #[cfg(any(windows, test))]
    {
        with_platform_control_api(|api, control| {
            apply_managed_state_with(api, control, DaemonServiceState::StoppedDisabled)
        })
    }
    #[cfg(not(any(windows, test)))]
    {
        control_api_unavailable()
    }
}

#[cfg(not(any(windows, test)))]
fn control_api_unavailable<T>() -> Result<T> {
    Err(TraceDecayError::Config {
        message: "Windows Task Scheduler is unavailable on this platform".to_string(),
    })
}

pub(super) fn delete() -> Result<()> {
    with_platform_api(delete_with)
}

pub(super) fn rollback_new_registration() -> Result<()> {
    with_platform_api(|api| rollback_registration_with(api, None, None))
}

pub(super) fn prepare_scoop_package_service(package_id: &str, state_file: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        prepare_scoop_package_service_windows(WindowsPackageId::parse(package_id)?, state_file)
    }
    #[cfg(not(windows))]
    {
        let _ = (package_id, state_file);
        Err(TraceDecayError::Config {
            message: "Scoop service hooks are only available on Windows".to_string(),
        })
    }
}

pub(super) fn restore_scoop_package_service(package_id: &str, state_file: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        restore_scoop_package_service_windows(WindowsPackageId::parse(package_id)?, state_file)
    }
    #[cfg(not(windows))]
    {
        let _ = (package_id, state_file);
        Err(TraceDecayError::Config {
            message: "Scoop service hooks are only available on Windows".to_string(),
        })
    }
}

#[cfg(windows)]
fn prepare_scoop_package_service_windows(
    package_id: WindowsPackageId,
    state_file: &Path,
) -> Result<()> {
    let identity = current_package_identity(package_id)?;
    let layout = local_runtime_layout(package_id)?;
    validate_state_file_path(state_file, &layout)?;

    with_platform_api_for_package(package_id, |api| {
        let snapshot = match api.snapshot() {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => return Ok(()),
            Err(error) if is_foreign_task_error(&error) => return Ok(()),
            Err(error) => return Err(error),
        };
        let task_xml = api
            .registered_xml()?
            .ok_or_else(|| missing_task("snapshot Scoop service state for"))?;
        let task_sddl = api
            .registered_sddl()?
            .ok_or_else(|| missing_task("snapshot Scoop service ACL for"))?;
        let state = match ScoopServiceState::capture(
            package_id, &identity, snapshot, task_xml, task_sddl,
        ) {
            Ok(state) => state,
            Err(error) if is_foreign_task_error(&error) => return Ok(()),
            Err(error) => return Err(error),
        };

        ensure_private_runtime_layout(&layout)?;
        write_scoop_state(state_file, &state)?;

        let mut control = NativeDaemonControl {
            transport_hint: state.profile_root.join("daemon.sock"),
            clock_origin: std::time::Instant::now(),
        };
        stop_managed_with(api, &mut control)?;
        let _lifecycle_lease =
            tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_for_profile(
                &state.profile_root,
                "Scoop service prepare",
            )?;
        delete_with(api)?;
        if api.snapshot()?.is_some() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "Scoop service prepare could not prove task '{}' absent",
                    identity.task_path
                ),
            });
        }
        let quiescence = control.quiescence(CONTROL_PROBE_TIMEOUT);
        if !quiescence.satisfied {
            return Err(TraceDecayError::Config {
                message: format!(
                    "Scoop service prepare could not prove profile '{}' quiescent: {}",
                    state.profile_root.display(),
                    quiescence.diagnostic
                ),
            });
        }
        remove_private_runtime_executable(&layout.executable)?;
        Ok(())
    })
}

#[cfg(windows)]
fn restore_scoop_package_service_windows(
    package_id: WindowsPackageId,
    state_file: &Path,
) -> Result<()> {
    let identity = current_package_identity(package_id)?;
    let layout = local_runtime_layout(package_id)?;
    validate_state_file_path(state_file, &layout)?;
    let Some(state) = read_scoop_state(state_file)? else {
        return Ok(());
    };
    state.validate(package_id, &identity)?;

    let current_exe = std::env::current_exe().map_err(|error| TraceDecayError::Config {
        message: format!("could not resolve the new Scoop executable: {error}"),
    })?;
    if package_id_from_executable(&current_exe) != Some(package_id) {
        return Err(TraceDecayError::Config {
            message: format!(
                "Scoop restore package {} cannot use executable '{}'",
                package_id.as_str(),
                current_exe.display()
            ),
        });
    }
    let source = scoop_service_source(&current_exe, package_id)?;
    let restored_xml = replace_task_action_executable(&state.task_xml, &layout.executable)?;
    let restored_action = task_action_from_xml(&restored_xml)
        .ok_or_else(|| invalid_state("restored task XML has no executable action"))?;
    if restored_action.arguments != state.action.arguments
        || profile_root_from_task_xml(&restored_xml).as_ref() != Some(&state.profile_root)
    {
        return Err(invalid_state(
            "restored task action or profile differs from the snapshot",
        ));
    }

    with_platform_api_for_package(package_id, |api| {
        let mut control = NativeDaemonControl {
            transport_hint: state.profile_root.join("daemon.sock"),
            clock_origin: std::time::Instant::now(),
        };
        match api.snapshot() {
            Ok(Some(snapshot)) => {
                let existing_xml = api
                    .registered_xml()?
                    .ok_or_else(|| missing_task("validate Scoop restore target for"))?;
                let existing_sddl = api
                    .registered_sddl()?
                    .ok_or_else(|| missing_task("validate Scoop restore target ACL for"))?;
                let existing = ScoopServiceState::capture(
                    package_id,
                    &identity,
                    snapshot,
                    existing_xml,
                    existing_sddl,
                )?;
                if existing.profile_root != state.profile_root
                    || existing.action.arguments != state.action.arguments
                {
                    return Err(foreign_task(&identity));
                }
                stop_managed_with(api, &mut control)?;
            }
            Ok(None) => {
                let quiescence = control.quiescence(CONTROL_PROBE_TIMEOUT);
                if !quiescence.satisfied {
                    return Err(TraceDecayError::Config {
                        message: format!(
                            "refusing Scoop restore while profile '{}' is not quiescent: {}",
                            state.profile_root.display(),
                            quiescence.diagnostic
                        ),
                    });
                }
            }
            Err(error) => return Err(error),
        }

        let lifecycle_lease =
            tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_for_profile(
                &state.profile_root,
                "Scoop service restore",
            )?;
        ensure_private_runtime_layout(&layout)?;
        atomic_copy_private_executable(&source, &layout.executable)?;
        api.register_xml_with_sddl(&restored_xml, &state.task_sddl)?;
        let stopped_state = if state.enabled {
            DaemonServiceState::StoppedEnabled
        } else {
            DaemonServiceState::StoppedDisabled
        };
        apply_state_with(api, stopped_state)?;
        verify_restored_task(api, &state, &layout, stopped_state)?;
        drop(lifecycle_lease);

        if state.running {
            start_managed_with(api, &mut control)?;
        } else {
            let quiescence = control.quiescence(CONTROL_PROBE_TIMEOUT);
            if !quiescence.satisfied {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "restored Scoop service profile '{}' is not quiescent: {}",
                        state.profile_root.display(),
                        quiescence.diagnostic
                    ),
                });
            }
        }
        verify_restored_task(api, &state, &layout, state.desired_state())
    })?;

    remove_scoop_state(state_file)
}

#[cfg(windows)]
fn verify_restored_task(
    api: &mut dyn TaskSchedulerApi,
    state: &ScoopServiceState,
    layout: &ServiceRuntimeLayout,
    expected_state: DaemonServiceState,
) -> Result<()> {
    let actual_state = state_from_snapshot(api.snapshot()?);
    if actual_state != expected_state {
        return Err(TraceDecayError::Config {
            message: format!(
                "restored Scoop service state is {actual_state:?}, expected {expected_state:?}"
            ),
        });
    }
    let xml = api
        .registered_xml()?
        .ok_or_else(|| missing_task("verify Scoop restore for"))?;
    let sddl = api
        .registered_sddl()?
        .ok_or_else(|| missing_task("verify Scoop restore ACL for"))?;
    let action = task_action_from_xml(&xml)
        .ok_or_else(|| invalid_state("restored task XML has no executable action"))?;
    if !windows_paths_equal(&action.executable, &layout.executable)?
        || action.arguments != state.action.arguments
        || profile_root_from_task_xml(&xml).as_ref() != Some(&state.profile_root)
        || sddl != state.task_sddl
    {
        return Err(TraceDecayError::Config {
            message: "restored Scoop service did not preserve its exact action, profile, or SDDL"
                .to_string(),
        });
    }
    Ok(())
}

#[cfg(windows)]
fn current_package_identity(package_id: WindowsPackageId) -> Result<TaskIdentity> {
    let user_sid =
        tracedecay_runtime_core::windows_security::current_user_sid_string().map_err(|error| {
            TraceDecayError::Config {
                message: format!("could not determine current Windows user SID: {error}"),
            }
        })?;
    TaskIdentity::for_package_user_sid(package_id, &user_sid)
}

#[cfg(windows)]
fn validate_state_file_path(state_file: &Path, layout: &ServiceRuntimeLayout) -> Result<()> {
    let state_file = fully_qualified_windows_path(state_file, "Scoop state file")?;
    if windows_paths_equal(&state_file, &layout.state_file)? {
        return Ok(());
    }
    Err(TraceDecayError::Config {
        message: format!(
            "Scoop state file must be '{}', got '{}'",
            layout.state_file.display(),
            state_file.display()
        ),
    })
}

#[cfg(windows)]
fn write_scoop_state(state_file: &Path, state: &ScoopServiceState) -> Result<()> {
    use std::sync::atomic::Ordering;

    let mut payload =
        serde_json::to_vec_pretty(state).map_err(|error| TraceDecayError::Config {
            message: format!("could not serialize Scoop service state: {error}"),
        })?;
    payload.push(b'\n');
    let sequence = super::SERVICE_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = state_file.with_file_name(format!(
        ".{SCOOP_STATE_FILE_NAME}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));
    tracedecay_runtime_core::db::DatabaseAuthority::publish_record_atomically(
        &temporary,
        state_file,
        &payload,
        "Scoop service state",
    )
}

#[cfg(windows)]
fn read_scoop_state(state_file: &Path) -> Result<Option<ScoopServiceState>> {
    let file = match tracedecay_runtime_core::windows_security::open_private_file(state_file) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(secure_path_error(
                "open Scoop service state",
                state_file,
                error,
            ));
        }
    };
    serde_json::from_reader(file)
        .map(Some)
        .map_err(|error| TraceDecayError::Config {
            message: format!(
                "invalid Scoop service state marker '{}': {error}",
                state_file.display()
            ),
        })
}

#[cfg(windows)]
fn remove_scoop_state(state_file: &Path) -> Result<()> {
    tracedecay_runtime_core::windows_security::validate_private_file(state_file).map_err(
        |error| {
            secure_path_error(
                "validate Scoop service state before removal",
                state_file,
                error,
            )
        },
    )?;
    std::fs::remove_file(state_file).map_err(|error| {
        secure_path_error("remove restored Scoop service state", state_file, error)
    })
}

#[cfg(windows)]
fn remove_private_runtime_executable(executable: &Path) -> Result<()> {
    match tracedecay_runtime_core::windows_security::validate_private_file(executable) {
        Ok(()) => std::fs::remove_file(executable).map_err(|error| {
            secure_path_error(
                "remove quiesced Scoop service executable",
                executable,
                error,
            )
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(secure_path_error(
            "validate Scoop service executable before removal",
            executable,
            error,
        )),
    }
}

#[cfg(windows)]
fn is_foreign_task_error(error: &TraceDecayError) -> bool {
    error
        .to_string()
        .contains("refusing to manage scheduled task")
}

fn register_task_xml_with(api: &mut dyn TaskSchedulerApi, xml: &str) -> Result<()> {
    let previous = api.snapshot()?;
    let previous_xml = api.registered_xml()?;
    if previous_xml.as_deref() == Some(xml) {
        return Ok(());
    }

    let operation_result = api.register_xml(xml).and_then(|()| match previous {
        Some(snapshot) => restore_registered_snapshot_with(api, snapshot),
        None => Ok(()),
    });
    if let Err(operation_error) = operation_result {
        let rollback_result = rollback_registration_with(api, previous, previous_xml.as_deref());
        return combine_task_operations(
            "register daemon task",
            Err(operation_error),
            rollback_result,
        );
    }
    Ok(())
}

fn rollback_registration_with(
    api: &mut dyn TaskSchedulerApi,
    previous: Option<TaskSnapshot>,
    previous_xml: Option<&str>,
) -> Result<()> {
    let disable_result = api.disable_for_rollback();
    let definition_result = match (previous, previous_xml) {
        (Some(snapshot), Some(xml)) => api
            .register_xml(xml)
            .and_then(|()| restore_snapshot_with(api, snapshot)),
        (None, None) => api.delete(),
        _ => Err(TraceDecayError::Config {
            message: "daemon task registration rollback snapshot was inconsistent".to_string(),
        }),
    };
    let rollback_result = combine_task_operations(
        "disable and restore daemon task registration",
        definition_result,
        disable_result,
    );
    if rollback_result.is_err() || previous.is_some_and(|snapshot| !snapshot.enabled) {
        let final_disable_result = api.disable_for_rollback();
        return combine_task_operations(
            "restore disabled daemon task registration",
            rollback_result,
            final_disable_result,
        );
    }
    rollback_result
}

fn restore_registered_snapshot_with(
    api: &mut dyn TaskSchedulerApi,
    previous: TaskSnapshot,
) -> Result<()> {
    let restore_result = restore_snapshot_with(api, previous);
    if previous.enabled {
        return restore_result;
    }
    let disable_result = restore_enablement_with(api, false);
    combine_task_operations(
        "restore disabled daemon task after registration",
        restore_result,
        disable_result,
    )
}

#[cfg(any(windows, test))]
fn apply_state_with(api: &mut dyn TaskSchedulerApi, desired: DaemonServiceState) -> Result<()> {
    if desired == DaemonServiceState::Missing {
        return delete_with(api);
    }
    if desired == DaemonServiceState::Masked {
        return deactivate_with(api);
    }

    let current = api
        .snapshot()?
        .ok_or_else(|| missing_task("apply state to"))?;
    let transition_result = (|| -> Result<()> {
        match desired {
            DaemonServiceState::RunningEnabled => {
                if !current.enabled {
                    api.set_enabled(true)?;
                }
                if !current.running {
                    api.run()?;
                }
            }
            DaemonServiceState::RunningDisabled if current.running => {
                if current.enabled {
                    api.set_enabled(false)?;
                }
            }
            DaemonServiceState::RunningDisabled => {
                if !current.enabled {
                    api.set_enabled(true)?;
                }
                api.run()?;
                api.set_enabled(false)?;
            }
            DaemonServiceState::StoppedEnabled => {
                if current.running {
                    api.stop()?;
                }
                if !current.enabled {
                    api.set_enabled(true)?;
                }
            }
            DaemonServiceState::StoppedDisabled => {
                if current.running {
                    api.stop()?;
                }
                if current.enabled {
                    api.set_enabled(false)?;
                }
            }
            DaemonServiceState::Missing | DaemonServiceState::Masked => {}
        }
        Ok(())
    })();
    if let Err(error) = transition_result {
        let restore_result = restore_enablement_with(api, current.enabled);
        return combine_task_operations("apply daemon task state", Err(error), restore_result);
    }
    Ok(())
}

fn restore_enablement_with(api: &mut dyn TaskSchedulerApi, enabled: bool) -> Result<()> {
    if api
        .snapshot()?
        .is_some_and(|snapshot| snapshot.enabled != enabled)
    {
        api.set_enabled(enabled)?;
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn start_with(api: &mut dyn TaskSchedulerApi) -> Result<()> {
    let current = api.snapshot()?.ok_or_else(|| missing_task("start"))?;
    if current.running {
        return Ok(());
    }
    if current.enabled {
        return api.run();
    }

    api.set_enabled(true)?;
    let run_result = api.run();
    let disable_result = api.set_enabled(false);
    combine_task_operations("start disabled task", run_result, disable_result)
}

#[cfg(test)]
fn stop_with(api: &mut dyn TaskSchedulerApi) -> Result<()> {
    let current = api.snapshot()?.ok_or_else(|| missing_task("stop"))?;
    if current.running {
        api.stop()?;
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn apply_managed_state_with(
    api: &mut dyn TaskSchedulerApi,
    control: &mut dyn DaemonControlApi,
    desired: DaemonServiceState,
) -> Result<()> {
    let previous = api
        .snapshot()?
        .ok_or_else(|| missing_task("apply managed state to"))?;
    let operation_result = match desired {
        DaemonServiceState::RunningEnabled | DaemonServiceState::RunningDisabled => {
            apply_state_with(api, desired).and_then(|()| {
                wait_for_task_state_with(
                    api,
                    control,
                    desired,
                    START_READINESS_TIMEOUT,
                    true,
                    "start",
                )
            })
        }
        DaemonServiceState::StoppedEnabled | DaemonServiceState::StoppedDisabled => {
            stop_managed_with(api, control).and_then(|()| {
                let enabled = desired == DaemonServiceState::StoppedEnabled;
                restore_enablement_with(api, enabled)?;
                wait_for_task_state_with(api, control, desired, HARD_STOP_TIMEOUT, false, "stop")
            })
        }
        DaemonServiceState::Masked => stop_managed_with(api, control).and_then(|()| {
            restore_enablement_with(api, false)?;
            wait_for_task_state_with(
                api,
                control,
                DaemonServiceState::StoppedDisabled,
                HARD_STOP_TIMEOUT,
                false,
                "deactivate",
            )
        }),
        DaemonServiceState::Missing => return delete_with(api),
    };
    if let Err(operation_error) = operation_result {
        let restore_result = restore_snapshot_with(api, previous);
        return combine_task_operations(
            "apply managed daemon task state",
            Err(operation_error),
            restore_result,
        );
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn start_managed_with(
    api: &mut dyn TaskSchedulerApi,
    control: &mut dyn DaemonControlApi,
) -> Result<()> {
    let previous = api.snapshot()?.ok_or_else(|| missing_task("start"))?;
    let desired = if previous.enabled {
        DaemonServiceState::RunningEnabled
    } else {
        DaemonServiceState::RunningDisabled
    };
    let operation_result = start_with(api).and_then(|()| {
        wait_for_task_state_with(
            api,
            control,
            desired,
            START_READINESS_TIMEOUT,
            true,
            "start",
        )
    });
    if let Err(operation_error) = operation_result {
        let restore_result = restore_snapshot_with(api, previous);
        return combine_task_operations(
            "start managed daemon task",
            Err(operation_error),
            restore_result,
        );
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn stop_managed_with(
    api: &mut dyn TaskSchedulerApi,
    control: &mut dyn DaemonControlApi,
) -> Result<()> {
    let current = api.snapshot()?.ok_or_else(|| missing_task("stop"))?;
    let desired = if current.enabled {
        DaemonServiceState::StoppedEnabled
    } else {
        DaemonServiceState::StoppedDisabled
    };
    let initial_quiescence = control.quiescence(CONTROL_PROBE_TIMEOUT);
    if !current.running {
        if initial_quiescence.satisfied {
            return Ok(());
        }
        return Err(TraceDecayError::Config {
            message: format!(
                "refusing to stop an unmanaged TraceDecay daemon: scheduled task is {desired:?}, but {}",
                initial_quiescence.diagnostic
            ),
        });
    }

    let shutdown_attempt = control.request_shutdown();
    let graceful_result = shutdown_attempt.may_have_been_delivered().then(|| {
        wait_for_task_state_with(
            api,
            control,
            desired,
            GRACEFUL_STOP_TIMEOUT,
            false,
            "graceful stop",
        )
    });
    if matches!(graceful_result, Some(Ok(()))) {
        return Ok(());
    }
    let graceful_diagnostic = graceful_result.map_or_else(
        || shutdown_attempt.diagnostic(),
        |result| match result {
            Ok(()) => shutdown_attempt.diagnostic(),
            Err(error) => format!("{}; {error}", shutdown_attempt.diagnostic()),
        },
    );

    let hard_stop_result = api.stop();
    let hard_stop_diagnostic = hard_stop_result
        .as_ref()
        .err()
        .map_or_else(|| "ok".to_string(), ToString::to_string);
    match wait_for_task_state_with(api, control, desired, HARD_STOP_TIMEOUT, false, "hard stop") {
        Ok(()) => Ok(()),
        Err(postcondition_error) => Err(TraceDecayError::Config {
            message: format!(
                "managed daemon task did not stop: graceful shutdown: {graceful_diagnostic}; hard stop: {hard_stop_diagnostic}; postcondition: {postcondition_error}"
            ),
        }),
    }
}

#[cfg(any(windows, test))]
fn wait_for_task_state_with(
    api: &mut dyn TaskSchedulerApi,
    control: &mut dyn DaemonControlApi,
    desired: DaemonServiceState,
    timeout: std::time::Duration,
    require_ready: bool,
    operation: &str,
) -> Result<()> {
    let deadline = control.elapsed().saturating_add(timeout);
    let mut observations = 0_usize;
    let mut last_state = DaemonServiceState::Missing;
    let mut last_control = "not observed".to_string();
    while control.elapsed() < deadline {
        last_state = state_from_snapshot(api.snapshot()?);
        let remaining = deadline.saturating_sub(control.elapsed());
        if remaining.is_zero() {
            break;
        }
        let probe_timeout = CONTROL_PROBE_TIMEOUT.min(remaining);
        let observation = if require_ready {
            control.readiness(probe_timeout)
        } else {
            control.quiescence(probe_timeout)
        };
        observations += 1;
        last_control = observation.diagnostic;
        if last_state == desired && observation.satisfied {
            return Ok(());
        }
        let remaining = deadline.saturating_sub(control.elapsed());
        if remaining.is_zero() {
            break;
        }
        control.wait(LIFECYCLE_POLL_INTERVAL.min(remaining));
    }
    Err(TraceDecayError::Config {
        message: format!(
            "TraceDecay daemon task {operation} postcondition failed at its {:.3}s deadline after {observations} observations: task {last_state:?}, {last_control}",
            timeout.as_secs_f64()
        ),
    })
}

fn restore_snapshot_with(api: &mut dyn TaskSchedulerApi, previous: TaskSnapshot) -> Result<()> {
    let restore_result = (|| -> Result<()> {
        let current = api
            .snapshot()?
            .ok_or_else(|| missing_task("restore state of"))?;
        if current.running && !previous.running {
            let stop_result = api.stop();
            let enablement_result = restore_enablement_with(api, previous.enabled);
            combine_task_operations(
                "restore daemon task stopped state",
                stop_result,
                enablement_result,
            )?;
        } else if !current.running && previous.running {
            if !current.enabled {
                api.set_enabled(true)?;
            }
            let run_result = api.run();
            let enablement_result = restore_enablement_with(api, previous.enabled);
            combine_task_operations(
                "restore daemon task running state",
                run_result,
                enablement_result,
            )?;
        }
        restore_enablement_with(api, previous.enabled)?;
        Ok(())
    })();
    restore_result?;
    let actual = api
        .snapshot()?
        .ok_or_else(|| missing_task("verify restored state of"))?;
    if actual != previous {
        return Err(TraceDecayError::Config {
            message: format!(
                "daemon task state restoration postcondition failed: expected {previous:?}, got {actual:?}"
            ),
        });
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn deactivate_with(api: &mut dyn TaskSchedulerApi) -> Result<()> {
    let Some(current) = api.snapshot()? else {
        return Ok(());
    };
    if current.running {
        api.stop()?;
    }
    if current.enabled {
        api.set_enabled(false)?;
    }
    Ok(())
}

fn delete_with(api: &mut dyn TaskSchedulerApi) -> Result<()> {
    if api.snapshot()?.is_some() {
        api.delete()?;
    }
    Ok(())
}

fn state_from_snapshot(snapshot: Option<TaskSnapshot>) -> DaemonServiceState {
    match snapshot {
        None => DaemonServiceState::Missing,
        Some(TaskSnapshot {
            running: true,
            enabled: true,
        }) => DaemonServiceState::RunningEnabled,
        Some(TaskSnapshot {
            running: true,
            enabled: false,
        }) => DaemonServiceState::RunningDisabled,
        Some(TaskSnapshot {
            running: false,
            enabled: true,
        }) => DaemonServiceState::StoppedEnabled,
        Some(TaskSnapshot {
            running: false,
            enabled: false,
        }) => DaemonServiceState::StoppedDisabled,
    }
}

#[cfg(any(windows, test))]
fn task_snapshot_from_scheduler_state(scheduler_state: i32, enabled: bool) -> Result<TaskSnapshot> {
    let running = match (scheduler_state, enabled) {
        (2 | 4, _) => true,
        (1, false) | (3, true) => false,
        (0, _) => {
            return Err(TraceDecayError::Config {
                message: "Windows Task Scheduler returned unknown daemon task state".to_string(),
            });
        }
        (1, true) | (3, false) => {
            return Err(TraceDecayError::Config {
                message: format!(
                    "Windows Task Scheduler returned inconsistent daemon task state {scheduler_state} with enabled={enabled}"
                ),
            });
        }
        _ => {
            return Err(TraceDecayError::Config {
                message: format!(
                    "Windows Task Scheduler returned unsupported daemon task state {scheduler_state}"
                ),
            });
        }
    };
    Ok(TaskSnapshot { running, enabled })
}

fn combine_task_operations(
    operation: &str,
    primary: Result<()>,
    restore: Result<()>,
) -> Result<()> {
    match (primary, restore) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(restore)) => Err(TraceDecayError::Config {
            message: format!(
                "{operation} failed: {primary}; state restoration also failed: {restore}"
            ),
        }),
    }
}

fn missing_task(operation: &str) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("cannot {operation} TraceDecay daemon task: task is not registered"),
    }
}

fn xml_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn xml_unescape(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn xml_element_text<'a>(xml: &'a str, element: &str) -> Option<&'a str> {
    let opening = format!("<{element}>");
    let closing = format!("</{element}>");
    let value_start = xml.find(&opening)? + opening.len();
    let after_start = &xml[value_start..];
    let value_end = after_start.find(&closing)?;
    Some(&after_start[..value_end])
}

#[cfg(any(windows, test))]
fn xml_section_text<'a>(xml: &'a str, section: &str) -> Option<&'a str> {
    let opening_prefix = format!("<{section}");
    let opening_start = xml.find(&opening_prefix)?;
    let value_start = xml[opening_start..].find('>')? + opening_start + 1;
    let closing = format!("</{section}>");
    let value_end = xml[value_start..].find(&closing)? + value_start;
    Some(&xml[value_start..value_end])
}

#[cfg(any(windows, test))]
fn task_action_from_xml(xml: &str) -> Option<ScoopTaskAction> {
    let action = xml_section_text(xml, "Exec")?;
    let executable = PathBuf::from(xml_unescape(xml_element_text(action, "Command")?));
    let arguments = xml_unescape(xml_element_text(action, "Arguments")?);
    Some(ScoopTaskAction {
        executable,
        arguments,
    })
}

#[cfg(any(windows, test))]
fn replace_task_action_executable(xml: &str, executable: &Path) -> Result<String> {
    let action =
        xml_section_text(xml, "Exec").ok_or_else(|| invalid_state("task XML has no Exec"))?;
    let command = xml_element_text(action, "Command")
        .ok_or_else(|| invalid_state("task XML has no Command"))?;
    let action_start = xml
        .find(action)
        .ok_or_else(|| invalid_state("task XML is malformed"))?;
    let command_start_in_action = action
        .find(command)
        .ok_or_else(|| invalid_state("task XML Command is malformed"))?;
    let command_start = action_start + command_start_in_action;
    let command_end = command_start + command.len();
    let executable = windows_path_text(executable, "Scoop service executable")?;
    let mut restored = String::with_capacity(xml.len() + executable.len());
    restored.push_str(&xml[..command_start]);
    restored.push_str(&xml_escape(executable));
    restored.push_str(&xml[command_end..]);
    Ok(restored)
}

#[cfg(any(windows, test))]
fn task_definition_is_owned(xml: &str, sddl: &str, identity: &TaskIdentity) -> bool {
    if xml.matches("<LogonTrigger").count() != 1 || xml.matches("<Principal ").count() != 1 {
        return false;
    }
    let trigger_user = xml_section_text(xml, "LogonTrigger")
        .and_then(|section| xml_element_text(section, "UserId"))
        .map(xml_unescape);
    let principal_user = xml_section_text(xml, r#"Principal id="Author""#)
        .and_then(|section| xml_element_text(section, "UserId"))
        .map(xml_unescape);
    trigger_user.as_deref() == Some(identity.user_sid.as_str())
        && principal_user.as_deref() == Some(identity.user_sid.as_str())
        && task_sddl_is_private(sddl, &identity.user_sid)
}

#[cfg(any(windows, test))]
fn task_sddl_is_private(sddl: &str, user_sid: &str) -> bool {
    let Some(dacl_start) = sddl.find("D:P") else {
        return false;
    };
    let owner_and_group = &sddl[..dacl_start];
    if owner_and_group != format!("O:{user_sid}")
        && !owner_and_group.starts_with(&format!("O:{user_sid}G:"))
    {
        return false;
    }
    let mut aces = &sddl[dacl_start + 3..];
    let user_ace = format!("(A;;GA;;;{user_sid})");
    let mut saw_user = false;
    let mut saw_system = false;
    while !aces.is_empty() {
        let Some(end) = aces.find(')') else {
            return false;
        };
        let (ace, remaining) = aces.split_at(end + 1);
        match ace {
            "(A;;GA;;;SY)" | "(A;;GA;;;S-1-5-18)" if !saw_system => saw_system = true,
            value if value == user_ace.as_str() && !saw_user => saw_user = true,
            _ => return false,
        }
        aces = remaining;
    }
    saw_user && saw_system
}

#[cfg(any(windows, test))]
fn foreign_task(identity: &TaskIdentity) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "refusing to manage scheduled task '{}': it is not owned by Scoop package {} for current SID {}",
            identity.task_path,
            identity.package_id.as_str(),
            identity.user_sid
        ),
    }
}

#[cfg(any(windows, test))]
fn invalid_state(reason: &str) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("invalid Scoop service state marker: {reason}"),
    }
}

fn quote_windows_argument(argument: &str) -> String {
    let mut quoted = String::from("\"");
    let mut backslashes = 0;
    for character in argument.chars() {
        match character {
            '\\' => backslashes += 1,
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push(character);
                backslashes = 0;
            }
        }
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

fn windows_argument_tokens(arguments: &str) -> Vec<String> {
    let characters: Vec<char> = arguments.chars().collect();
    let mut tokens = Vec::new();
    let mut position = 0;
    while position < characters.len() {
        while characters[position].is_whitespace() {
            position += 1;
            if position == characters.len() {
                return tokens;
            }
        }

        let mut token = String::new();
        let mut quoted = false;
        while position < characters.len() {
            if characters[position].is_whitespace() && !quoted {
                break;
            }
            if characters[position] == '"' {
                quoted = !quoted;
                position += 1;
                continue;
            }
            if characters[position] == '\\' {
                let start = position;
                while position < characters.len() && characters[position] == '\\' {
                    position += 1;
                }
                let backslashes = position - start;
                if position < characters.len() && characters[position] == '"' {
                    token.extend(std::iter::repeat_n('\\', backslashes / 2));
                    if backslashes % 2 == 0 {
                        quoted = !quoted;
                    } else {
                        token.push('"');
                    }
                    position += 1;
                    continue;
                }
                token.extend(std::iter::repeat_n('\\', backslashes));
                continue;
            }
            token.push(characters[position]);
            position += 1;
        }
        tokens.push(token);
        while position < characters.len() && characters[position].is_whitespace() {
            position += 1;
        }
    }
    tokens
}

fn with_platform_api<T>(
    operation: impl FnOnce(&mut dyn TaskSchedulerApi) -> Result<T>,
) -> Result<T> {
    #[cfg(windows)]
    {
        let mut api = native::NativeTaskScheduler::connect()?;
        operation(&mut api)
    }
    #[cfg(not(windows))]
    {
        let _ = operation;
        Err(TraceDecayError::Config {
            message: "Windows Task Scheduler is unavailable on this platform".to_string(),
        })
    }
}

#[cfg(windows)]
fn with_platform_api_for_package<T>(
    package_id: WindowsPackageId,
    operation: impl FnOnce(&mut dyn TaskSchedulerApi) -> Result<T>,
) -> Result<T> {
    let user_sid =
        tracedecay_runtime_core::windows_security::current_user_sid_string().map_err(|error| {
            TraceDecayError::Config {
                message: format!("could not determine current Windows user SID: {error}"),
            }
        })?;
    let identity = TaskIdentity::for_package_user_sid(package_id, &user_sid)?;
    let mut api = native::NativeTaskScheduler::connect_for(identity)?;
    operation(&mut api)
}

#[cfg(any(windows, test))]
fn with_platform_control_api<T>(
    operation: impl FnOnce(&mut dyn TaskSchedulerApi, &mut dyn DaemonControlApi) -> Result<T>,
) -> Result<T> {
    #[cfg(windows)]
    {
        with_platform_api(|api| {
            let xml = api
                .registered_xml()?
                .ok_or_else(|| missing_task("resolve profile for"))?;
            let profile_root =
                profile_root_from_task_xml(&xml).ok_or_else(|| TraceDecayError::Config {
                    message:
                        "cannot manage TraceDecay daemon task: registered task has no profile root"
                            .to_string(),
                })?;
            let mut control = NativeDaemonControl {
                transport_hint: profile_root.join("daemon.sock"),
                clock_origin: std::time::Instant::now(),
            };
            operation(api, &mut control)
        })
    }
    #[cfg(not(windows))]
    {
        let _ = operation;
        Err(TraceDecayError::Config {
            message: "Windows Task Scheduler is unavailable on this platform".to_string(),
        })
    }
}

#[cfg(windows)]
mod native {
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND};
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };
    use windows::Win32::System::TaskScheduler::{
        IRegisteredTask, ITaskFolder, ITaskService, TASK_CREATE_OR_UPDATE,
        TASK_DONT_ADD_PRINCIPAL_ACE, TASK_LOGON_INTERACTIVE_TOKEN, TaskScheduler,
    };
    use windows::Win32::System::Variant::VARIANT;
    use windows::core::{BSTR, HRESULT};

    use super::*;

    const OWNER_AND_DACL_SECURITY_INFORMATION: i32 = 0x0000_0001 | 0x0000_0004;

    pub(super) struct NativeTaskScheduler {
        root: ITaskFolder,
        identity: TaskIdentity,
        _apartment: ComApartment,
    }

    struct ComApartment;

    impl ComApartment {
        fn initialize() -> Result<Self> {
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
                .ok()
                .map_err(|error| com_error("initialize COM", error))?;
            Ok(Self)
        }
    }

    impl Drop for ComApartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }

    impl NativeTaskScheduler {
        pub(super) fn connect() -> Result<Self> {
            let identity = TaskIdentity::current()?;
            Self::connect_for(identity)
        }

        pub(super) fn connect_for(identity: TaskIdentity) -> Result<Self> {
            let apartment = ComApartment::initialize()?;
            let service: ITaskService =
                unsafe { CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER) }
                    .map_err(|error| com_error("create Task Scheduler service", error))?;
            let empty = VARIANT::default();
            unsafe { service.Connect(&empty, &empty, &empty, &empty) }
                .map_err(|error| com_error("connect to Task Scheduler as current user", error))?;
            let root = unsafe { service.GetFolder(&BSTR::from(r"\")) }
                .map_err(|error| com_error("open Task Scheduler root folder", error))?;
            Ok(Self {
                root,
                identity,
                _apartment: apartment,
            })
        }

        fn task_unchecked(&self) -> Result<Option<IRegisteredTask>> {
            match unsafe {
                self.root
                    .GetTask(&BSTR::from(self.identity.task_path.as_str()))
            } {
                Ok(task) => Ok(Some(task)),
                Err(error) if is_task_not_found(&error) => Ok(None),
                Err(error) => Err(com_error("get TraceDecay daemon task", error)),
            }
        }

        fn task(&self) -> Result<Option<IRegisteredTask>> {
            let Some(task) = self.task_unchecked()? else {
                return Ok(None);
            };
            self.verify_ownership(&task)?;
            Ok(Some(task))
        }

        fn verify_ownership(&self, task: &IRegisteredTask) -> Result<()> {
            let xml = unsafe { task.Xml() }
                .map_err(|error| com_error("read daemon task XML for ownership", error))?;
            let xml = String::try_from(xml).map_err(|error| TraceDecayError::Config {
                message: format!("daemon task XML is not valid UTF-16: {error}"),
            })?;
            let sddl = unsafe { task.GetSecurityDescriptor(OWNER_AND_DACL_SECURITY_INFORMATION) }
                .map_err(|error| com_error("read daemon task security descriptor", error))?;
            let sddl = String::try_from(sddl).map_err(|error| TraceDecayError::Config {
                message: format!("daemon task security descriptor is not valid UTF-16: {error}"),
            })?;
            if task_definition_is_owned(&xml, &sddl, &self.identity) {
                return Ok(());
            }
            Err(TraceDecayError::Config {
                message: format!(
                    "refusing to manage scheduled task '{}': its user identity or protected ACL is not owned by current SID {}",
                    self.identity.task_path, self.identity.user_sid
                ),
            })
        }

        fn required_task(&self, operation: &str) -> Result<IRegisteredTask> {
            self.task()?.ok_or_else(|| missing_task(operation))
        }
    }

    impl TaskSchedulerApi for NativeTaskScheduler {
        fn snapshot(&mut self) -> Result<Option<TaskSnapshot>> {
            let Some(task) = self.task()? else {
                return Ok(None);
            };
            let enabled = unsafe { task.Enabled() }
                .map_err(|error| com_error("read daemon task enablement", error))?
                .as_bool();
            let scheduler_state = unsafe { task.State() }
                .map_err(|error| com_error("read daemon task state", error))?;
            task_snapshot_from_scheduler_state(scheduler_state.0, enabled).map(Some)
        }

        fn registered_xml(&mut self) -> Result<Option<String>> {
            let Some(task) = self.task()? else {
                return Ok(None);
            };
            let xml =
                unsafe { task.Xml() }.map_err(|error| com_error("read daemon task XML", error))?;
            String::try_from(xml)
                .map(Some)
                .map_err(|error| TraceDecayError::Config {
                    message: format!("daemon task XML is not valid UTF-16: {error}"),
                })
        }

        fn registered_sddl(&mut self) -> Result<Option<String>> {
            let Some(task) = self.task()? else {
                return Ok(None);
            };
            let sddl = unsafe { task.GetSecurityDescriptor(OWNER_AND_DACL_SECURITY_INFORMATION) }
                .map_err(|error| com_error("read daemon task security descriptor", error))?;
            String::try_from(sddl)
                .map(Some)
                .map_err(|error| TraceDecayError::Config {
                    message: format!(
                        "daemon task security descriptor is not valid UTF-16: {error}"
                    ),
                })
        }

        fn register_xml(&mut self, xml: &str) -> Result<()> {
            let sddl = self.identity.sddl.clone();
            self.register_xml_with_sddl(xml, &sddl)
        }

        fn register_xml_with_sddl(&mut self, xml: &str, sddl: &str) -> Result<()> {
            if self.task_unchecked()?.is_some() {
                let _ = self.task()?;
            }
            let user = VARIANT::from(self.identity.user_sid.as_str());
            let password = VARIANT::default();
            let sddl = VARIANT::from(sddl);
            let task = unsafe {
                self.root.RegisterTask(
                    &BSTR::from(self.identity.task_path.as_str()),
                    &BSTR::from(xml),
                    TASK_CREATE_OR_UPDATE.0 | TASK_DONT_ADD_PRINCIPAL_ACE.0,
                    &user,
                    &password,
                    TASK_LOGON_INTERACTIVE_TOKEN,
                    &sddl,
                )
            }
            .map_err(|error| com_error("register TraceDecay daemon task", error))?;
            self.verify_ownership(&task)
        }

        fn set_enabled(&mut self, enabled: bool) -> Result<()> {
            let task = self.required_task("change enablement of")?;
            unsafe { task.SetEnabled(enabled.into()) }
                .map_err(|error| com_error("change daemon task enablement", error))
        }

        fn disable_for_rollback(&mut self) -> Result<()> {
            let Some(task) = self.task_unchecked()? else {
                return Ok(());
            };
            unsafe { task.SetEnabled(false.into()) }
                .map_err(|error| com_error("disable daemon task during rollback", error))
        }

        fn run(&mut self) -> Result<()> {
            let task = self.required_task("start")?;
            unsafe { task.Run(&VARIANT::default()) }
                .map(drop)
                .map_err(|error| com_error("start daemon task", error))
        }

        fn stop(&mut self) -> Result<()> {
            let task = self.required_task("stop")?;
            unsafe { task.Stop(0) }.map_err(|error| com_error("stop daemon task", error))
        }

        fn delete(&mut self) -> Result<()> {
            match unsafe {
                self.root
                    .DeleteTask(&BSTR::from(self.identity.task_name.as_str()), 0)
            } {
                Ok(()) => Ok(()),
                Err(error) if is_task_not_found(&error) => Ok(()),
                Err(error) => Err(com_error("delete TraceDecay daemon task", error)),
            }
        }
    }

    fn is_task_not_found(error: &windows::core::Error) -> bool {
        is_task_not_found_code(error.code())
    }

    pub(super) fn is_task_not_found_code(code: HRESULT) -> bool {
        code == HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0)
            || code == HRESULT::from_win32(ERROR_PATH_NOT_FOUND.0)
    }

    fn com_error(operation: &str, error: windows::core::Error) -> TraceDecayError {
        TraceDecayError::Config {
            message: format!("{operation} failed: {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    const TEST_SID: &str = "S-1-5-21-111-222-333-1001";

    #[test]
    fn scoop_packages_have_isolated_runtime_and_task_identities() {
        let local_app_data = Path::new(r"C:\Users\alice\AppData\Local");
        let stable = ServiceRuntimeLayout::below(local_app_data, WindowsPackageId::Stable);
        let beta = ServiceRuntimeLayout::below(local_app_data, WindowsPackageId::Beta);
        assert_eq!(
            stable.executable,
            local_app_data
                .join("TraceDecay")
                .join("service")
                .join("tracedecay")
                .join("tracedecay.exe")
        );
        assert_eq!(
            beta.executable,
            local_app_data
                .join("TraceDecay")
                .join("service")
                .join("tracedecay-beta")
                .join("tracedecay.exe")
        );
        assert_ne!(stable.state_file, beta.state_file);

        let stable_identity =
            TaskIdentity::for_package_user_sid(WindowsPackageId::Stable, TEST_SID)
                .expect("stable task identity");
        let beta_identity = TaskIdentity::for_package_user_sid(WindowsPackageId::Beta, TEST_SID)
            .expect("beta task identity");
        assert_eq!(
            stable_identity.task_name,
            format!("TraceDecay Daemon ({TEST_SID})")
        );
        assert_eq!(
            beta_identity.task_name,
            format!("TraceDecay Beta Daemon ({TEST_SID})")
        );
        assert_ne!(stable_identity.task_path, beta_identity.task_path);
    }

    #[test]
    fn scoop_package_detection_is_case_insensitive_and_strict() {
        assert_eq!(
            package_id_from_executable(Path::new(
                "C:/Users/alice/scoop/apps/TraceDecay/5.0.0/tracedecay.exe"
            )),
            Some(WindowsPackageId::Stable)
        );
        assert_eq!(
            package_id_from_executable(Path::new(
                "C:/Users/alice/scoop/apps/TRACEDECAY-BETA/5.1.0-beta.1/tracedecay.exe"
            )),
            Some(WindowsPackageId::Beta)
        );
        assert_eq!(
            package_id_from_executable(Path::new(
                "C:/Users/alice/AppData/Local/TraceDecay/service/tracedecay-beta/tracedecay.exe"
            )),
            Some(WindowsPackageId::Beta)
        );
        assert_eq!(
            package_id_from_executable(Path::new("C:/tools/tracedecay.exe")),
            None
        );
    }

    #[test]
    fn scoop_state_marker_authenticates_exact_package_task_state() {
        let identity = TaskIdentity::for_package_user_sid(WindowsPackageId::Beta, TEST_SID)
            .expect("beta identity");
        let task_xml = render_task_xml_for(
            &spec(
                "C:/scoop/apps/tracedecay-beta/5.1.0-beta.1/tracedecay.exe",
                "C:/profiles/beta",
            ),
            &identity,
        )
        .expect("task XML");
        let marker = ScoopServiceState::capture(
            WindowsPackageId::Beta,
            &identity,
            TaskSnapshot {
                running: true,
                enabled: false,
            },
            task_xml,
            identity.sddl.clone(),
        )
        .expect("owned marker");
        marker
            .validate(WindowsPackageId::Beta, &identity)
            .expect("valid marker");
        assert_eq!(marker.desired_state(), DaemonServiceState::RunningDisabled);
        assert!(
            marker
                .validate(WindowsPackageId::Stable, &identity)
                .is_err()
        );
    }

    #[test]
    fn scoop_restore_rewrites_only_the_service_executable() {
        let identity = TaskIdentity::for_package_user_sid(WindowsPackageId::Stable, TEST_SID)
            .expect("stable identity");
        let original = render_task_xml_for(
            &spec(
                "C:/scoop/apps/tracedecay/5.0.0/tracedecay.exe",
                "C:/profiles/stable & exact",
            ),
            &identity,
        )
        .expect("task XML");
        let replacement =
            Path::new("C:/Users/alice/AppData/Local/TraceDecay/service/tracedecay/tracedecay.exe");
        let restored =
            replace_task_action_executable(&original, replacement).expect("rewritten action");
        let action = task_action_from_xml(&restored).expect("restored action");
        assert_eq!(action.executable, replacement);
        assert_eq!(
            profile_root_from_task_xml(&restored),
            Some(PathBuf::from("C:/profiles/stable & exact"))
        );
        assert_eq!(
            action.arguments,
            r#"daemon run --profile-root "C:/profiles/stable & exact""#
        );
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Operation {
        Register(String),
        Enable(bool),
        Run,
        Stop,
        Delete,
    }

    #[derive(Default)]
    struct FakeTaskScheduler {
        task: Option<TaskSnapshot>,
        xml: Option<String>,
        operations: Vec<Operation>,
        registration_failures_remaining: usize,
        fail_next_enablement: bool,
        fail_next_run: bool,
        fail_next_stop: bool,
        fail_next_delete: bool,
        stop_leaves_running: bool,
        snapshots_until_exit: Option<usize>,
        snapshot_count: usize,
    }

    impl FakeTaskScheduler {
        fn with_task(state: DaemonServiceState, xml: &str) -> Self {
            let (running, enabled) = match state {
                DaemonServiceState::RunningEnabled => (true, true),
                DaemonServiceState::RunningDisabled => (true, false),
                DaemonServiceState::StoppedEnabled => (false, true),
                DaemonServiceState::StoppedDisabled | DaemonServiceState::Masked => (false, false),
                DaemonServiceState::Missing => return Self::default(),
            };
            Self {
                task: Some(TaskSnapshot { running, enabled }),
                xml: Some(xml.to_string()),
                operations: Vec::new(),
                registration_failures_remaining: 0,
                fail_next_enablement: false,
                fail_next_run: false,
                fail_next_stop: false,
                fail_next_delete: false,
                stop_leaves_running: false,
                snapshots_until_exit: None,
                snapshot_count: 0,
            }
        }

        fn state(&self) -> DaemonServiceState {
            state_from_snapshot(self.task)
        }
    }

    impl TaskSchedulerApi for FakeTaskScheduler {
        fn snapshot(&mut self) -> Result<Option<TaskSnapshot>> {
            self.snapshot_count += 1;
            if self
                .snapshots_until_exit
                .is_some_and(|count| self.snapshot_count >= count)
                && let Some(task) = self.task.as_mut()
            {
                task.running = false;
            }
            Ok(self.task)
        }

        fn registered_xml(&mut self) -> Result<Option<String>> {
            Ok(self.xml.clone())
        }

        #[cfg(windows)]
        fn registered_sddl(&mut self) -> Result<Option<String>> {
            Ok(self
                .task
                .map(|_| TaskIdentity::for_user_sid(TEST_SID).expect("identity").sddl))
        }

        fn register_xml(&mut self, xml: &str) -> Result<()> {
            self.operations.push(Operation::Register(xml.to_string()));
            self.task = Some(TaskSnapshot {
                running: false,
                enabled: true,
            });
            self.xml = Some(xml.to_string());
            if self.registration_failures_remaining > 0 {
                self.registration_failures_remaining -= 1;
                return Err(TraceDecayError::Config {
                    message: "fake scheduler registration failed after mutation".to_string(),
                });
            }
            Ok(())
        }

        fn set_enabled(&mut self, enabled: bool) -> Result<()> {
            if std::mem::take(&mut self.fail_next_enablement) {
                return Err(TraceDecayError::Config {
                    message: "fake scheduler enablement change failed".to_string(),
                });
            }
            let task = self.task.as_mut().ok_or_else(|| missing_task("enable"))?;
            self.operations.push(Operation::Enable(enabled));
            task.enabled = enabled;
            Ok(())
        }

        fn disable_for_rollback(&mut self) -> Result<()> {
            if self.task.is_none() {
                return Ok(());
            }
            self.set_enabled(false)
        }

        fn run(&mut self) -> Result<()> {
            if std::mem::take(&mut self.fail_next_run) {
                return Err(TraceDecayError::Config {
                    message: "fake scheduler run failed".to_string(),
                });
            }
            let task = self.task.as_mut().ok_or_else(|| missing_task("run"))?;
            if !task.enabled {
                return Err(TraceDecayError::Config {
                    message: "fake scheduler refuses to run a disabled task".to_string(),
                });
            }
            self.operations.push(Operation::Run);
            task.running = true;
            Ok(())
        }

        fn stop(&mut self) -> Result<()> {
            if std::mem::take(&mut self.fail_next_stop) {
                return Err(TraceDecayError::Config {
                    message: "fake scheduler stop failed".to_string(),
                });
            }
            let task = self.task.as_mut().ok_or_else(|| missing_task("stop"))?;
            self.operations.push(Operation::Stop);
            if !self.stop_leaves_running {
                task.running = false;
            }
            Ok(())
        }

        fn delete(&mut self) -> Result<()> {
            self.operations.push(Operation::Delete);
            if std::mem::take(&mut self.fail_next_delete) {
                return Err(TraceDecayError::Config {
                    message: "fake scheduler delete failed".to_string(),
                });
            }
            self.task = None;
            self.xml = None;
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeDaemonControl {
        shutdown_requests: usize,
        readiness_checks: usize,
        quiescence_checks: usize,
        waits: usize,
        ready_after: Option<usize>,
        quiesced_after: Option<usize>,
        shutdown_fails: bool,
        shutdown_loses_acknowledgement: bool,
        probe_latency: std::time::Duration,
        probe_timeouts: Vec<std::time::Duration>,
        elapsed: std::time::Duration,
    }

    impl DaemonControlApi for FakeDaemonControl {
        fn request_shutdown(&mut self) -> ShutdownRequestAttempt {
            self.shutdown_requests += 1;
            if self.shutdown_fails {
                return ShutdownRequestAttempt::NotSent(
                    "fake graceful shutdown failed".to_string(),
                );
            }
            if self.shutdown_loses_acknowledgement {
                return ShutdownRequestAttempt::SentWithoutAcknowledgement(
                    "fake acknowledgement lost".to_string(),
                );
            }
            ShutdownRequestAttempt::Acknowledged
        }

        fn readiness(&mut self, timeout: std::time::Duration) -> ControlObservation {
            self.readiness_checks += 1;
            self.probe_timeouts.push(timeout);
            self.elapsed = self.elapsed.saturating_add(self.probe_latency.min(timeout));
            ControlObservation {
                satisfied: self
                    .ready_after
                    .is_some_and(|poll| self.readiness_checks >= poll),
                diagnostic: format!("fake readiness check {}", self.readiness_checks),
            }
        }

        fn quiescence(&mut self, timeout: std::time::Duration) -> ControlObservation {
            self.quiescence_checks += 1;
            self.probe_timeouts.push(timeout);
            self.elapsed = self.elapsed.saturating_add(self.probe_latency.min(timeout));
            ControlObservation {
                satisfied: self
                    .quiesced_after
                    .is_some_and(|poll| self.quiescence_checks >= poll),
                diagnostic: format!("fake quiescence check {}", self.quiescence_checks),
            }
        }

        fn elapsed(&self) -> std::time::Duration {
            self.elapsed
        }

        fn wait(&mut self, duration: std::time::Duration) {
            self.waits += 1;
            self.elapsed = self.elapsed.saturating_add(duration);
        }
    }

    fn spec(executable: impl Into<PathBuf>, profile_root: impl Into<PathBuf>) -> DaemonServiceSpec {
        DaemonServiceSpec {
            tracedecay_bin: executable.into(),
            socket_path: PathBuf::from("ignored-by-windows-task"),
            data_dir_override: Some(profile_root.into()),
        }
    }

    #[test]
    fn task_identity_scopes_name_path_and_acl_to_user_sid() {
        let identity = TaskIdentity::for_user_sid(TEST_SID).expect("task identity");

        assert_eq!(
            identity.task_name,
            "TraceDecay Daemon (S-1-5-21-111-222-333-1001)"
        );
        assert_eq!(
            identity.task_path,
            r"\TraceDecay Daemon (S-1-5-21-111-222-333-1001)"
        );
        assert_eq!(
            identity.sddl,
            "O:S-1-5-21-111-222-333-1001D:P(A;;GA;;;SY)(A;;GA;;;S-1-5-21-111-222-333-1001)"
        );
        assert!(TaskIdentity::for_user_sid("S-1-5-21-(foreign)").is_err());
    }

    #[test]
    fn ownership_requires_matching_trigger_principal_and_private_acl() {
        let identity = TaskIdentity::for_user_sid(TEST_SID).expect("task identity");
        let xml = render_task_xml_for(
            &spec(r"C:\TraceDecay\tracedecay.exe", r"C:\TraceDecay\data"),
            &identity,
        )
        .expect("task XML");

        assert!(task_definition_is_owned(&xml, &identity.sddl, &identity));
        assert!(!task_definition_is_owned(
            &xml.replace(TEST_SID, "S-1-5-21-111-222-333-1002"),
            &identity.sddl,
            &identity
        ));
        assert!(!task_definition_is_owned(
            &xml,
            &format!("{}(A;;GA;;;BA)", identity.sddl),
            &identity
        ));
    }

    #[cfg(unix)]
    #[test]
    fn task_xml_rejects_non_unicode_paths_instead_of_corrupting_them() {
        use std::os::unix::ffi::OsStringExt;

        let identity = TaskIdentity::for_user_sid(TEST_SID).expect("task identity");
        let executable = std::ffi::OsString::from_vec(vec![b'C', b':', b'\\', 0xff]);
        let error = render_task_xml_for(
            &spec(PathBuf::from(executable), r"C:\TraceDecay\data"),
            &identity,
        )
        .expect_err("invalid Unicode must fail");

        assert!(error.to_string().contains("is not valid Unicode"));
    }

    #[test]
    fn task_xml_round_trips_unc_and_extended_profile_roots() {
        let identity = TaskIdentity::for_user_sid(TEST_SID).expect("task identity");
        for profile_root in [
            PathBuf::from(r"\\server\share\TraceDecay Data\"),
            PathBuf::from(r"\\?\C:\Users\Zack\TraceDecay Data\"),
        ] {
            let xml = render_task_xml_for(
                &spec(r"\\?\C:\TraceDecay\tracedecay.exe", &profile_root),
                &identity,
            )
            .expect("render task XML");

            assert_eq!(profile_root_from_task_xml(&xml), Some(profile_root));
        }
    }

    #[test]
    fn task_command_enforces_scheduler_utf16_limit() {
        let at_limit = format!("C:\\{}", "x".repeat(257));
        let over_limit = format!("C:\\{}", "x".repeat(258));

        assert_eq!(at_limit.encode_utf16().count(), 260);
        validate_task_command_text(&at_limit).expect("260 UTF-16 units");
        assert!(
            validate_task_command_text(&over_limit)
                .expect_err("261 UTF-16 units")
                .to_string()
                .contains("260 UTF-16 code units")
        );
    }

    #[cfg(windows)]
    #[test]
    fn relative_profile_root_is_made_absolute() {
        let identity = TaskIdentity::for_user_sid(TEST_SID).expect("task identity");
        let xml = render_task_xml_for(
            &spec(r"C:\TraceDecay\tracedecay.exe", "relative-profile"),
            &identity,
        )
        .expect("render task XML");

        assert_eq!(
            profile_root_from_task_xml(&xml),
            Some(
                std::env::current_dir()
                    .expect("current directory")
                    .join("relative-profile")
            )
        );
    }

    #[cfg(windows)]
    #[test]
    fn relative_executable_is_made_absolute_and_drive_relative_profile_is_rejected() {
        let identity = TaskIdentity::for_user_sid(TEST_SID).expect("task identity");
        let xml = render_task_xml_for(&spec("tracedecay.exe", "relative-profile"), &identity)
            .expect("render task XML");
        let command = xml_element_text(&xml, "Command").expect("command");
        assert!(Path::new(command).is_absolute());

        let error = render_task_xml_for(
            &spec(r"C:\TraceDecay\tracedecay.exe", r"C:relative-profile"),
            &identity,
        )
        .expect_err("drive-relative profile");
        assert!(error.to_string().contains("drive-relative"));
    }

    #[test]
    fn task_xml_escapes_action_paths_and_declares_daemon_settings() {
        let identity = TaskIdentity::for_user_sid(TEST_SID).expect("task identity");
        let xml = render_task_xml_for(
            &spec(
                r#"C:\Program Files\Trace<&"'Decay\tracedecay.exe"#,
                r#"C:\Users\Zack & <Trace>"'Decay"#,
            ),
            &identity,
        )
        .expect("render task XML");

        assert!(xml.contains(
            r"<Command>C:\Program Files\Trace&lt;&amp;&quot;&apos;Decay\tracedecay.exe</Command>"
        ));
        assert!(xml.contains(
            r"<Arguments>daemon run --profile-root &quot;C:\Users\Zack &amp; &lt;Trace&gt;\&quot;&apos;Decay&quot;</Arguments>"
        ));
        assert!(xml.contains("<LogonTrigger>"));
        assert_eq!(
            xml.matches(&format!("<UserId>{TEST_SID}</UserId>")).count(),
            2
        );
        assert!(xml.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(xml.contains("<RunLevel>LeastPrivilege</RunLevel>"));
        assert!(xml.contains("<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>"));
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
        assert!(xml.contains("<Interval>PT1M</Interval>"));
        assert!(xml.contains("<Count>255</Count>"));
        assert!(xml.contains("<Enabled>true</Enabled>"));
    }

    #[test]
    fn task_xml_profile_root_round_trips_escaped_text() {
        let profile_root = PathBuf::from("C:\\Users\\Z & <Trace>\"'Decay\\");
        let identity = TaskIdentity::for_user_sid(TEST_SID).expect("task identity");
        let xml = render_task_xml_for(
            &spec(
                r"C:\Users\Z\scoop\apps\tracedecay\current\tracedecay.exe",
                &profile_root,
            ),
            &identity,
        )
        .expect("render task XML");

        assert_eq!(profile_root_from_task_xml(&xml), Some(profile_root));
    }

    #[test]
    fn snapshot_maps_all_running_and_enablement_combinations() {
        assert_eq!(state_from_snapshot(None), DaemonServiceState::Missing);
        assert_eq!(
            state_from_snapshot(Some(TaskSnapshot {
                running: true,
                enabled: true,
            })),
            DaemonServiceState::RunningEnabled
        );
        assert_eq!(
            state_from_snapshot(Some(TaskSnapshot {
                running: true,
                enabled: false,
            })),
            DaemonServiceState::RunningDisabled
        );
        assert_eq!(
            state_from_snapshot(Some(TaskSnapshot {
                running: false,
                enabled: true,
            })),
            DaemonServiceState::StoppedEnabled
        );
        assert_eq!(
            state_from_snapshot(Some(TaskSnapshot {
                running: false,
                enabled: false,
            })),
            DaemonServiceState::StoppedDisabled
        );
    }

    #[test]
    fn native_scheduler_state_mapping_fails_closed() {
        assert_eq!(
            task_snapshot_from_scheduler_state(1, false).expect("disabled"),
            TaskSnapshot {
                running: false,
                enabled: false,
            }
        );
        assert_eq!(
            task_snapshot_from_scheduler_state(2, false).expect("queued disabled"),
            TaskSnapshot {
                running: true,
                enabled: false,
            }
        );
        assert_eq!(
            task_snapshot_from_scheduler_state(3, true).expect("ready"),
            TaskSnapshot {
                running: false,
                enabled: true,
            }
        );
        assert!(task_snapshot_from_scheduler_state(0, false).is_err());
        assert!(task_snapshot_from_scheduler_state(1, true).is_err());
        assert!(task_snapshot_from_scheduler_state(3, false).is_err());
        assert!(task_snapshot_from_scheduler_state(5, true).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn account_information_error_is_not_task_not_found() {
        use windows::core::HRESULT;

        assert!(!native::is_task_not_found_code(HRESULT(
            0x8004_130f_u32 as i32
        )));
        assert!(native::is_task_not_found_code(HRESULT::from_win32(
            windows::Win32::Foundation::ERROR_FILE_NOT_FOUND.0
        )));
    }

    #[test]
    fn registration_updates_definition_and_restores_running_disabled_state() {
        let mut api =
            FakeTaskScheduler::with_task(DaemonServiceState::RunningDisabled, "<Task>old</Task>");

        register_task_xml_with(&mut api, "<Task>new</Task>").expect("update task");

        assert_eq!(api.state(), DaemonServiceState::RunningDisabled);
        assert_eq!(api.xml.as_deref(), Some("<Task>new</Task>"));
        assert_eq!(
            api.operations,
            vec![
                Operation::Register("<Task>new</Task>".to_string()),
                Operation::Run,
                Operation::Enable(false),
            ]
        );
    }

    #[test]
    fn registration_preserves_every_existing_running_and_enablement_state() {
        for state in [
            DaemonServiceState::RunningEnabled,
            DaemonServiceState::RunningDisabled,
            DaemonServiceState::StoppedEnabled,
            DaemonServiceState::StoppedDisabled,
        ] {
            let mut api = FakeTaskScheduler::with_task(state, "<Task>old</Task>");

            register_task_xml_with(&mut api, "<Task>new</Task>").expect("update task");

            assert_eq!(api.state(), state, "state changed while updating {state:?}");
        }
    }

    #[test]
    fn registration_restores_disabled_state_when_restart_fails() {
        let mut api =
            FakeTaskScheduler::with_task(DaemonServiceState::RunningDisabled, "<Task>old</Task>");
        api.fail_next_run = true;

        register_task_xml_with(&mut api, "<Task>new</Task>").expect_err("restart must fail");

        assert_eq!(api.state(), DaemonServiceState::RunningDisabled);
        assert_eq!(api.xml.as_deref(), Some("<Task>old</Task>"));
        assert!(
            api.operations
                .contains(&Operation::Register("<Task>old</Task>".to_string()))
        );
    }

    #[test]
    fn registration_failure_still_restores_disabled_state() {
        let mut api =
            FakeTaskScheduler::with_task(DaemonServiceState::StoppedDisabled, "<Task>old</Task>");
        api.fail_next_enablement = true;

        let error = register_task_xml_with(&mut api, "<Task>new</Task>")
            .expect_err("restoration must fail");

        assert!(error.to_string().contains("enablement change failed"));
        assert_eq!(api.state(), DaemonServiceState::StoppedDisabled);
        assert_eq!(api.xml.as_deref(), Some("<Task>old</Task>"));
        assert!(
            api.operations
                .contains(&Operation::Register("<Task>old</Task>".to_string()))
        );
    }

    #[test]
    fn registration_api_failure_after_mutation_restores_disabled_state() {
        let mut api =
            FakeTaskScheduler::with_task(DaemonServiceState::StoppedDisabled, "<Task>old</Task>");
        api.registration_failures_remaining = 1;

        let error = register_task_xml_with(&mut api, "<Task>new</Task>")
            .expect_err("registration must fail");

        assert!(
            error
                .to_string()
                .contains("registration failed after mutation")
        );
        assert_eq!(api.state(), DaemonServiceState::StoppedDisabled);
        assert_eq!(api.xml.as_deref(), Some("<Task>old</Task>"));
        assert!(
            api.operations
                .contains(&Operation::Register("<Task>old</Task>".to_string()))
        );
    }

    #[test]
    fn failed_enabled_task_definition_rollback_disables_residual_task() {
        let mut api =
            FakeTaskScheduler::with_task(DaemonServiceState::StoppedEnabled, "<Task>old</Task>");
        api.registration_failures_remaining = 2;

        let error = register_task_xml_with(&mut api, "<Task>new</Task>")
            .expect_err("registration and rollback must fail");

        assert!(error.to_string().contains("state restoration also failed"));
        assert_eq!(api.state(), DaemonServiceState::StoppedDisabled);
        assert_eq!(api.operations.last(), Some(&Operation::Enable(false)));
    }

    #[test]
    fn failed_new_registration_cleanup_leaves_residual_task_disabled() {
        let mut api =
            FakeTaskScheduler::with_task(DaemonServiceState::StoppedEnabled, "<Task>new</Task>");
        api.fail_next_delete = true;

        let error = rollback_registration_with(&mut api, None, None)
            .expect_err("delete failure must surface");

        assert!(error.to_string().contains("fake scheduler delete failed"));
        assert_eq!(api.state(), DaemonServiceState::StoppedDisabled);
        assert_eq!(
            api.operations,
            vec![Operation::Enable(false), Operation::Delete]
        );
    }

    #[test]
    fn failed_no_start_transition_removes_new_registration() {
        let mut api = FakeTaskScheduler::default();
        register_task_xml_with(&mut api, "<Task>new</Task>").expect("register task");
        api.fail_next_enablement = true;

        apply_state_with(&mut api, DaemonServiceState::StoppedDisabled)
            .expect_err("disable transition must fail");
        rollback_registration_with(&mut api, None, None).expect("remove new task");

        assert_eq!(api.state(), DaemonServiceState::Missing);
        assert_eq!(api.operations.last(), Some(&Operation::Delete));
    }

    #[test]
    fn registration_is_idempotent_and_create_or_update_does_not_duplicate() {
        let xml = "<Task>same</Task>";
        let mut api = FakeTaskScheduler::default();

        register_task_xml_with(&mut api, xml).expect("create task");
        register_task_xml_with(&mut api, xml).expect("repeat registration");

        assert_eq!(api.state(), DaemonServiceState::StoppedEnabled);
        assert_eq!(api.xml.as_deref(), Some(xml));
        assert_eq!(api.operations, vec![Operation::Register(xml.to_string())]);
    }

    #[test]
    fn apply_state_enables_runs_and_redisables_for_running_disabled() {
        let mut api = FakeTaskScheduler::with_task(DaemonServiceState::StoppedDisabled, "<Task/>");

        apply_state_with(&mut api, DaemonServiceState::RunningDisabled).expect("apply state");

        assert_eq!(api.state(), DaemonServiceState::RunningDisabled);
        assert_eq!(
            api.operations,
            vec![
                Operation::Enable(true),
                Operation::Run,
                Operation::Enable(false)
            ]
        );
    }

    #[test]
    fn apply_state_restores_disabled_state_when_run_fails() {
        let mut api = FakeTaskScheduler::with_task(DaemonServiceState::StoppedDisabled, "<Task/>");
        api.fail_next_run = true;

        apply_state_with(&mut api, DaemonServiceState::RunningDisabled).expect_err("run must fail");

        assert_eq!(api.state(), DaemonServiceState::StoppedDisabled);
        assert_eq!(
            api.operations,
            vec![Operation::Enable(true), Operation::Enable(false)]
        );
    }

    #[test]
    fn start_and_stop_preserve_enablement() {
        let mut api = FakeTaskScheduler::with_task(DaemonServiceState::StoppedDisabled, "<Task/>");

        start_with(&mut api).expect("start disabled task");
        assert_eq!(api.state(), DaemonServiceState::RunningDisabled);
        assert_eq!(
            api.operations,
            vec![
                Operation::Enable(true),
                Operation::Run,
                Operation::Enable(false)
            ]
        );

        api.operations.clear();
        stop_with(&mut api).expect("stop task");
        assert_eq!(api.state(), DaemonServiceState::StoppedDisabled);
        assert_eq!(api.operations, vec![Operation::Stop]);
    }

    #[test]
    fn rollback_restores_disabled_state_even_when_stop_fails() {
        let mut api = FakeTaskScheduler::with_task(DaemonServiceState::RunningEnabled, "<Task/>");
        api.fail_next_stop = true;

        let error = restore_snapshot_with(
            &mut api,
            TaskSnapshot {
                running: false,
                enabled: false,
            },
        )
        .expect_err("stop must fail");

        assert!(error.to_string().contains("fake scheduler stop failed"));
        assert_eq!(api.state(), DaemonServiceState::RunningDisabled);
        assert_eq!(api.operations, vec![Operation::Enable(false)]);
    }

    #[test]
    fn managed_stop_uses_authenticated_graceful_shutdown_without_hard_stop() {
        let mut api = FakeTaskScheduler::with_task(DaemonServiceState::RunningEnabled, "<Task/>");
        api.snapshots_until_exit = Some(3);
        let mut control = FakeDaemonControl {
            quiesced_after: Some(2),
            ..FakeDaemonControl::default()
        };

        stop_managed_with(&mut api, &mut control).expect("graceful stop");

        assert_eq!(api.state(), DaemonServiceState::StoppedEnabled);
        assert_eq!(control.shutdown_requests, 1);
        assert!(!api.operations.contains(&Operation::Stop));
    }

    #[test]
    fn managed_stop_refuses_live_endpoint_when_task_is_already_stopped() {
        let mut api = FakeTaskScheduler::with_task(DaemonServiceState::StoppedEnabled, "<Task/>");
        let mut control = FakeDaemonControl::default();

        let error =
            stop_managed_with(&mut api, &mut control).expect_err("unmanaged daemon must fail");

        assert!(error.to_string().contains("unmanaged"));
        assert_eq!(control.shutdown_requests, 0);
        assert!(api.operations.is_empty());
    }

    #[test]
    fn managed_stop_observes_graceful_exit_when_acknowledgement_is_lost() {
        let mut api = FakeTaskScheduler::with_task(DaemonServiceState::RunningEnabled, "<Task/>");
        api.snapshots_until_exit = Some(3);
        let mut control = FakeDaemonControl {
            quiesced_after: Some(2),
            shutdown_loses_acknowledgement: true,
            ..FakeDaemonControl::default()
        };

        stop_managed_with(&mut api, &mut control).expect("unacknowledged graceful stop");

        assert_eq!(api.state(), DaemonServiceState::StoppedEnabled);
        assert!(!api.operations.contains(&Operation::Stop));
    }

    #[test]
    fn managed_stop_hard_stops_immediately_when_shutdown_was_not_sent() {
        let mut api = FakeTaskScheduler::with_task(DaemonServiceState::RunningEnabled, "<Task/>");
        let mut control = FakeDaemonControl {
            quiesced_after: Some(2),
            shutdown_fails: true,
            ..FakeDaemonControl::default()
        };

        stop_managed_with(&mut api, &mut control).expect("unsent hard-stop fallback");

        assert_eq!(api.operations, vec![Operation::Stop]);
        assert_eq!(control.waits, 0);
    }

    #[test]
    fn managed_stop_uses_bounded_hard_stop_fallback_and_verifies_quiescence() {
        let mut api = FakeTaskScheduler::with_task(DaemonServiceState::RunningDisabled, "<Task/>");
        let graceful_observations =
            (GRACEFUL_STOP_TIMEOUT.as_millis() / LIFECYCLE_POLL_INTERVAL.as_millis()) as usize;
        let mut control = FakeDaemonControl {
            quiesced_after: Some(graceful_observations + 2),
            ..FakeDaemonControl::default()
        };

        stop_managed_with(&mut api, &mut control).expect("hard-stop fallback");

        assert_eq!(api.state(), DaemonServiceState::StoppedDisabled);
        assert_eq!(control.shutdown_requests, 1);
        assert_eq!(api.operations, vec![Operation::Stop]);
        assert!(
            control.waits
                < ((GRACEFUL_STOP_TIMEOUT + HARD_STOP_TIMEOUT).as_millis()
                    / LIFECYCLE_POLL_INTERVAL.as_millis()) as usize
        );
    }

    #[test]
    fn managed_stop_rejects_scheduler_success_without_stop_postcondition() {
        let mut api = FakeTaskScheduler::with_task(DaemonServiceState::RunningEnabled, "<Task/>");
        api.stop_leaves_running = true;
        let mut control = FakeDaemonControl::default();

        let error = stop_managed_with(&mut api, &mut control).expect_err("postcondition must fail");

        assert!(error.to_string().contains("task RunningEnabled"));
        assert_eq!(api.state(), DaemonServiceState::RunningEnabled);
        assert_eq!(api.operations, vec![Operation::Stop]);
    }

    #[test]
    fn managed_start_rolls_back_task_state_when_readiness_never_arrives() {
        let mut api = FakeTaskScheduler::with_task(DaemonServiceState::StoppedDisabled, "<Task/>");
        let mut control = FakeDaemonControl::default();

        let error = start_managed_with(&mut api, &mut control).expect_err("readiness must fail");

        assert!(error.to_string().contains("start postcondition failed"));
        assert_eq!(api.state(), DaemonServiceState::StoppedDisabled);
        assert_eq!(
            api.operations,
            vec![
                Operation::Enable(true),
                Operation::Run,
                Operation::Enable(false),
                Operation::Stop,
            ]
        );
        assert_eq!(
            control.readiness_checks,
            (START_READINESS_TIMEOUT.as_millis() / LIFECYCLE_POLL_INTERVAL.as_millis()) as usize
        );
    }

    #[test]
    fn managed_start_returns_only_after_protocol_readiness() {
        let mut api = FakeTaskScheduler::with_task(DaemonServiceState::StoppedEnabled, "<Task/>");
        let mut control = FakeDaemonControl {
            ready_after: Some(2),
            ..FakeDaemonControl::default()
        };

        start_managed_with(&mut api, &mut control).expect("ready start");

        assert_eq!(api.state(), DaemonServiceState::RunningEnabled);
        assert_eq!(api.operations, vec![Operation::Run]);
        assert_eq!(control.readiness_checks, 2);
    }

    #[test]
    fn readiness_uses_remaining_absolute_deadline_for_each_probe() {
        let mut api = FakeTaskScheduler::with_task(DaemonServiceState::RunningEnabled, "<Task/>");
        let timeout = std::time::Duration::from_millis(2_600);
        let mut control = FakeDaemonControl {
            probe_latency: std::time::Duration::from_secs(2),
            ..FakeDaemonControl::default()
        };

        wait_for_task_state_with(
            &mut api,
            &mut control,
            DaemonServiceState::RunningEnabled,
            timeout,
            true,
            "start",
        )
        .expect_err("readiness must reach deadline");

        assert_eq!(control.elapsed, timeout);
        assert_eq!(
            control.probe_timeouts,
            vec![
                CONTROL_PROBE_TIMEOUT,
                CONTROL_PROBE_TIMEOUT,
                std::time::Duration::from_millis(600),
            ]
        );
    }

    #[test]
    fn deactivate_and_delete_are_idempotent() {
        let mut api = FakeTaskScheduler::with_task(DaemonServiceState::RunningEnabled, "<Task/>");

        deactivate_with(&mut api).expect("deactivate task");
        delete_with(&mut api).expect("delete task");
        delete_with(&mut api).expect("repeat delete");

        assert_eq!(api.state(), DaemonServiceState::Missing);
        assert_eq!(
            api.operations,
            vec![Operation::Stop, Operation::Enable(false), Operation::Delete]
        );
    }
}
