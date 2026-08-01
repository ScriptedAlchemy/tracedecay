use std::path::{Path, PathBuf};

use crate::errors::{Result, TraceDecayError};

use super::{DaemonServiceSpec, DaemonServiceState};

const TASK_NAME_PREFIX: &str = "TraceDecay Daemon";

#[derive(Clone, Debug, PartialEq, Eq)]
struct TaskIdentity {
    user_sid: String,
    task_name: String,
    task_path: String,
    sddl: String,
}

impl TaskIdentity {
    fn current() -> Result<Self> {
        #[cfg(windows)]
        {
            let user_sid = tracedecay_runtime_core::windows_security::current_user_sid_string()
                .map_err(|error| TraceDecayError::Config {
                    message: format!("could not determine current Windows user SID: {error}"),
                })?;
            Self::for_user_sid(&user_sid)
        }
        #[cfg(not(windows))]
        {
            Err(TraceDecayError::Config {
                message: "Windows Task Scheduler identity is unavailable on this platform"
                    .to_string(),
            })
        }
    }

    fn for_user_sid(user_sid: &str) -> Result<Self> {
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
        let task_name = format!("{TASK_NAME_PREFIX} ({user_sid})");
        Ok(Self {
            user_sid: user_sid.to_string(),
            task_path: format!(r"\{task_name}"),
            task_name,
            sddl: format!("O:{user_sid}D:P(A;;GA;;;SY)(A;;GA;;;{user_sid})"),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TaskSnapshot {
    running: bool,
    enabled: bool,
}

trait TaskSchedulerApi {
    fn snapshot(&mut self) -> Result<Option<TaskSnapshot>>;
    fn registered_xml(&mut self) -> Result<Option<String>>;
    fn register_xml(&mut self, xml: &str) -> Result<()>;
    fn set_enabled(&mut self, enabled: bool) -> Result<()>;
    fn run(&mut self) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
    fn delete(&mut self) -> Result<()>;
}

#[derive(Debug)]
struct ControlObservation {
    satisfied: bool,
    diagnostic: String,
}

trait DaemonControlApi {
    fn request_shutdown(&mut self) -> Result<()>;
    fn readiness(&mut self) -> ControlObservation;
    fn quiescence(&mut self) -> ControlObservation;
    fn wait(&mut self, duration: std::time::Duration);
}

const LIFECYCLE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
const START_READINESS_POLLS: usize = 30;
const GRACEFUL_STOP_POLLS: usize = 40;
const HARD_STOP_POLLS: usize = 20;

#[cfg(windows)]
struct NativeDaemonControl {
    transport_hint: PathBuf,
}

#[cfg(windows)]
impl DaemonControlApi for NativeDaemonControl {
    fn request_shutdown(&mut self) -> Result<()> {
        super::probe::request_daemon_shutdown(&self.transport_hint)
    }

    fn readiness(&mut self) -> ControlObservation {
        let protocol = super::probe::daemon_protocol_state_with_timeout(
            &self.transport_hint,
            std::time::Duration::from_millis(750),
        );
        ControlObservation {
            satisfied: matches!(protocol, super::probe::DaemonProtocolState::Ready),
            diagnostic: format!("protocol {protocol}"),
        }
    }

    fn quiescence(&mut self) -> ControlObservation {
        let socket = super::probe::daemon_socket_state(&self.transport_hint);
        ControlObservation {
            satisfied: socket.is_proven_quiesced(),
            diagnostic: format!("endpoint {socket}"),
        }
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
    #[cfg(windows)]
    let profile_root = if profile_root.is_absolute() {
        profile_root
    } else {
        std::env::current_dir()?.join(profile_root)
    };
    let executable = xml_escape(windows_path_text(
        &spec.tracedecay_bin,
        "daemon executable",
    )?);
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

pub(super) fn preferred_service_executable(executable: &Path) -> PathBuf {
    let Some(file_name) = executable.file_name().and_then(|name| name.to_str()) else {
        return executable.to_path_buf();
    };
    if !file_name.eq_ignore_ascii_case("tracedecay.exe") {
        return executable.to_path_buf();
    }
    if executable
        .parent()
        .is_some_and(|parent| path_file_name_eq(parent, "shims"))
    {
        let Some(scoop_root) = executable.parent().and_then(Path::parent) else {
            return executable.to_path_buf();
        };
        let stable = scoop_root
            .join("apps")
            .join("tracedecay")
            .join("current")
            .join("tracedecay.exe");
        return if stable.is_file() {
            stable
        } else {
            executable.to_path_buf()
        };
    }
    let Some(version_dir) = executable.parent() else {
        return executable.to_path_buf();
    };
    let Some(app_dir) = version_dir.parent() else {
        return executable.to_path_buf();
    };
    let Some(apps_dir) = app_dir.parent() else {
        return executable.to_path_buf();
    };
    if !path_file_name_eq(app_dir, "tracedecay") || !path_file_name_eq(apps_dir, "apps") {
        return executable.to_path_buf();
    }

    let stable = app_dir.join("current").join("tracedecay.exe");
    if stable.is_file() {
        stable
    } else {
        executable.to_path_buf()
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
    if state == DaemonServiceState::Missing {
        return with_platform_api(delete_with);
    }
    with_platform_control_api(|api, control| apply_managed_state_with(api, control, state))
}

pub(super) fn start() -> Result<()> {
    with_platform_control_api(start_managed_with)
}

pub(super) fn stop() -> Result<()> {
    with_platform_control_api(stop_managed_with)
}

pub(super) fn deactivate() -> Result<()> {
    with_platform_control_api(|api, control| {
        apply_managed_state_with(api, control, DaemonServiceState::StoppedDisabled)
    })
}

pub(super) fn delete() -> Result<()> {
    with_platform_api(delete_with)
}

fn register_task_xml_with(api: &mut dyn TaskSchedulerApi, xml: &str) -> Result<()> {
    let previous = api.snapshot()?;
    if api.registered_xml()?.as_deref() == Some(xml) {
        return Ok(());
    }

    api.register_xml(xml)?;
    let Some(previous) = previous else {
        return Ok(());
    };

    if previous.running {
        // Registration publishes an enabled definition. Ensure it is running
        // before restoring disabled state so both properties survive an update.
        api.set_enabled(true)?;
        if !api.snapshot()?.is_some_and(|snapshot| snapshot.running) {
            let run_result = api.run();
            if !previous.enabled {
                let disable_result = api.set_enabled(false);
                return combine_task_operations(
                    "restore running disabled task after registration",
                    run_result,
                    disable_result,
                );
            }
            run_result?;
        }
        if !previous.enabled {
            api.set_enabled(false)?;
        }
        return Ok(());
    }

    apply_state_with(
        api,
        if previous.enabled {
            DaemonServiceState::StoppedEnabled
        } else {
            DaemonServiceState::StoppedDisabled
        },
    )
}

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

fn stop_with(api: &mut dyn TaskSchedulerApi) -> Result<()> {
    let current = api.snapshot()?.ok_or_else(|| missing_task("stop"))?;
    if current.running {
        api.stop()?;
    }
    Ok(())
}

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
                    START_READINESS_POLLS,
                    true,
                    "start",
                )
            })
        }
        DaemonServiceState::StoppedEnabled | DaemonServiceState::StoppedDisabled => {
            stop_managed_with(api, control).and_then(|()| {
                let enabled = desired == DaemonServiceState::StoppedEnabled;
                restore_enablement_with(api, enabled)?;
                wait_for_task_state_with(api, control, desired, HARD_STOP_POLLS, false, "stop")
            })
        }
        DaemonServiceState::Masked => stop_managed_with(api, control).and_then(|()| {
            restore_enablement_with(api, false)?;
            wait_for_task_state_with(
                api,
                control,
                DaemonServiceState::StoppedDisabled,
                HARD_STOP_POLLS,
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
        wait_for_task_state_with(api, control, desired, START_READINESS_POLLS, true, "start")
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
    let initial_quiescence = control.quiescence();
    if !current.running && initial_quiescence.satisfied {
        return Ok(());
    }

    let graceful_result = control.request_shutdown().and_then(|()| {
        wait_for_task_state_with(
            api,
            control,
            desired,
            GRACEFUL_STOP_POLLS,
            false,
            "graceful stop",
        )
    });
    if graceful_result.is_ok() {
        return Ok(());
    }
    let graceful_diagnostic = graceful_result
        .err()
        .map_or_else(|| "ok".to_string(), |error| error.to_string());

    let hard_stop_result = api.stop();
    let hard_stop_diagnostic = hard_stop_result
        .as_ref()
        .err()
        .map_or_else(|| "ok".to_string(), ToString::to_string);
    match wait_for_task_state_with(api, control, desired, HARD_STOP_POLLS, false, "hard stop") {
        Ok(()) => Ok(()),
        Err(postcondition_error) => Err(TraceDecayError::Config {
            message: format!(
                "managed daemon task did not stop: graceful shutdown: {graceful_diagnostic}; hard stop: {hard_stop_diagnostic}; postcondition: {postcondition_error}"
            ),
        }),
    }
}

fn wait_for_task_state_with(
    api: &mut dyn TaskSchedulerApi,
    control: &mut dyn DaemonControlApi,
    desired: DaemonServiceState,
    polls: usize,
    require_ready: bool,
    operation: &str,
) -> Result<()> {
    let mut last_state = DaemonServiceState::Missing;
    let mut last_control = "not observed".to_string();
    for attempt in 0..polls {
        last_state = state_from_snapshot(api.snapshot()?);
        let observation = if require_ready {
            control.readiness()
        } else {
            control.quiescence()
        };
        last_control = observation.diagnostic;
        if last_state == desired && observation.satisfied {
            return Ok(());
        }
        if attempt + 1 < polls {
            control.wait(LIFECYCLE_POLL_INTERVAL);
        }
    }
    Err(TraceDecayError::Config {
        message: format!(
            "TraceDecay daemon task {operation} postcondition failed after {} polls: task {last_state:?}, {last_control}",
            polls
        ),
    })
}

fn restore_snapshot_with(api: &mut dyn TaskSchedulerApi, previous: TaskSnapshot) -> Result<()> {
    let restore_result = (|| -> Result<()> {
        let current = api
            .snapshot()?
            .ok_or_else(|| missing_task("restore state of"))?;
        if current.running && !previous.running {
            api.stop()?;
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

fn path_file_name_eq(path: &Path, expected: &str) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(expected))
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

fn xml_section_text<'a>(xml: &'a str, section: &str) -> Option<&'a str> {
    let opening_prefix = format!("<{section}");
    let opening_start = xml.find(&opening_prefix)?;
    let value_start = xml[opening_start..].find('>')? + opening_start + 1;
    let closing = format!("</{section}>");
    let value_end = xml[value_start..].find(&closing)? + value_start;
    Some(&xml[value_start..value_end])
}

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
    use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };
    use windows::Win32::System::TaskScheduler::{
        IRegisteredTask, ITaskFolder, ITaskService, TASK_CREATE_OR_UPDATE,
        TASK_LOGON_INTERACTIVE_TOKEN, TASK_STATE_QUEUED, TASK_STATE_RUNNING, TaskScheduler,
    };
    use windows::Win32::System::Variant::VARIANT;
    use windows::core::{BSTR, HRESULT};

    use super::*;

    const SCHED_E_TASK_NOT_FOUND: HRESULT = HRESULT(0x8004_130f_u32 as i32);
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
            Ok(Some(TaskSnapshot {
                running: matches!(scheduler_state, TASK_STATE_RUNNING | TASK_STATE_QUEUED),
                enabled,
            }))
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

        fn register_xml(&mut self, xml: &str) -> Result<()> {
            if self.task_unchecked()?.is_some() {
                let _ = self.task()?;
            }
            let user = VARIANT::from(self.identity.user_sid.as_str());
            let password = VARIANT::default();
            let sddl = VARIANT::from(self.identity.sddl.as_str());
            let task = unsafe {
                self.root.RegisterTask(
                    &BSTR::from(self.identity.task_path.as_str()),
                    &BSTR::from(xml),
                    TASK_CREATE_OR_UPDATE.0,
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
        let code = error.code();
        code == SCHED_E_TASK_NOT_FOUND || code == HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0)
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
        fail_next_run: bool,
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
                fail_next_run: false,
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

        fn register_xml(&mut self, xml: &str) -> Result<()> {
            self.operations.push(Operation::Register(xml.to_string()));
            self.task = Some(TaskSnapshot {
                running: false,
                enabled: true,
            });
            self.xml = Some(xml.to_string());
            Ok(())
        }

        fn set_enabled(&mut self, enabled: bool) -> Result<()> {
            let task = self.task.as_mut().ok_or_else(|| missing_task("enable"))?;
            self.operations.push(Operation::Enable(enabled));
            task.enabled = enabled;
            Ok(())
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
            let task = self.task.as_mut().ok_or_else(|| missing_task("stop"))?;
            self.operations.push(Operation::Stop);
            if !self.stop_leaves_running {
                task.running = false;
            }
            Ok(())
        }

        fn delete(&mut self) -> Result<()> {
            self.operations.push(Operation::Delete);
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
    }

    impl DaemonControlApi for FakeDaemonControl {
        fn request_shutdown(&mut self) -> Result<()> {
            self.shutdown_requests += 1;
            if self.shutdown_fails {
                return Err(TraceDecayError::Config {
                    message: "fake graceful shutdown failed".to_string(),
                });
            }
            Ok(())
        }

        fn readiness(&mut self) -> ControlObservation {
            self.readiness_checks += 1;
            ControlObservation {
                satisfied: self
                    .ready_after
                    .is_some_and(|poll| self.readiness_checks >= poll),
                diagnostic: format!("fake readiness check {}", self.readiness_checks),
            }
        }

        fn quiescence(&mut self) -> ControlObservation {
            self.quiescence_checks += 1;
            ControlObservation {
                satisfied: self
                    .quiesced_after
                    .is_some_and(|poll| self.quiescence_checks >= poll),
                diagnostic: format!("fake quiescence check {}", self.quiescence_checks),
            }
        }

        fn wait(&mut self, _duration: std::time::Duration) {
            self.waits += 1;
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
            r#"<Command>C:\Program Files\Trace&lt;&amp;&quot;&apos;Decay\tracedecay.exe</Command>"#
        ));
        assert!(xml.contains(
            r#"<Arguments>daemon run --profile-root &quot;C:\Users\Zack &amp; &lt;Trace&gt;\&quot;&apos;Decay&quot;</Arguments>"#
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
                Operation::Enable(true),
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

        register_task_xml_with(&mut api, "<Task>new</Task>")
            .expect_err("restart must fail");

        assert_eq!(api.state(), DaemonServiceState::StoppedDisabled);
        assert_eq!(
            api.operations,
            vec![
                Operation::Register("<Task>new</Task>".to_string()),
                Operation::Enable(true),
                Operation::Enable(false),
            ]
        );
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
    fn managed_stop_uses_bounded_hard_stop_fallback_and_verifies_quiescence() {
        let mut api = FakeTaskScheduler::with_task(DaemonServiceState::RunningDisabled, "<Task/>");
        let mut control = FakeDaemonControl {
            quiesced_after: Some(GRACEFUL_STOP_POLLS + 2),
            ..FakeDaemonControl::default()
        };

        stop_managed_with(&mut api, &mut control).expect("hard-stop fallback");

        assert_eq!(api.state(), DaemonServiceState::StoppedDisabled);
        assert_eq!(control.shutdown_requests, 1);
        assert_eq!(api.operations, vec![Operation::Stop]);
        assert!(control.waits < GRACEFUL_STOP_POLLS + HARD_STOP_POLLS);
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
        assert_eq!(control.readiness_checks, START_READINESS_POLLS);
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

    #[test]
    fn scoop_install_prefers_existing_current_junction_executable() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let app = temp.path().join("apps/tracedecay");
        let versioned = app.join("4.2.0/tracedecay.exe");
        let current = app.join("current/tracedecay.exe");
        std::fs::create_dir_all(versioned.parent().expect("version parent")).expect("version dir");
        std::fs::create_dir_all(current.parent().expect("current parent")).expect("current dir");
        std::fs::write(&versioned, b"versioned").expect("versioned executable");
        std::fs::write(&current, b"current").expect("current executable");

        assert_eq!(preferred_service_executable(&versioned), current);
        assert_eq!(
            preferred_service_executable(Path::new("/opt/tracedecay/bin/tracedecay")),
            PathBuf::from("/opt/tracedecay/bin/tracedecay")
        );
    }

    #[test]
    fn scoop_shim_prefers_existing_current_junction_executable() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let shim = temp.path().join("shims/tracedecay.exe");
        let current = temp.path().join("apps/tracedecay/current/tracedecay.exe");
        std::fs::create_dir_all(shim.parent().expect("shim parent")).expect("shim dir");
        std::fs::create_dir_all(current.parent().expect("current parent")).expect("current dir");
        std::fs::write(&shim, b"shim").expect("shim executable");
        std::fs::write(&current, b"current").expect("current executable");

        assert_eq!(preferred_service_executable(&shim), current);
    }

    #[test]
    fn scoop_component_matching_is_case_insensitive() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let app = temp.path().join("APPS/TraceDecay");
        let versioned = app.join("4.2.0/tracedecay.EXE");
        let current = app.join("current/tracedecay.exe");
        std::fs::create_dir_all(versioned.parent().expect("version parent")).expect("version dir");
        std::fs::create_dir_all(current.parent().expect("current parent")).expect("current dir");
        std::fs::write(&versioned, b"versioned").expect("versioned executable");
        std::fs::write(&current, b"current").expect("current executable");

        assert_eq!(preferred_service_executable(&versioned), current);
    }
}
