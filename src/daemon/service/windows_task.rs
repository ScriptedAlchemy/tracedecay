use std::path::{Path, PathBuf};

use crate::errors::{Result, TraceDecayError};

use super::{DaemonServiceSpec, DaemonServiceState};

const TASK_NAME: &str = "TraceDecay Daemon";
const TASK_PATH: &str = r"\TraceDecay Daemon";

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

pub(super) fn task_name() -> &'static str {
    TASK_NAME
}

pub(super) fn task_path() -> PathBuf {
    PathBuf::from(TASK_PATH)
}

pub(super) fn render_task_xml(spec: &DaemonServiceSpec) -> Result<String> {
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
    let executable = xml_escape(&spec.tracedecay_bin.to_string_lossy());
    let arguments = xml_escape(&format!(
        "daemon run --profile-root {}",
        quote_windows_argument(&profile_root.to_string_lossy())
    ));

    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>TraceDecay daemon</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
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
      <Interval>PT2S</Interval>
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
    with_platform_api(|api| apply_state_with(api, state))
}

pub(super) fn start() -> Result<()> {
    with_platform_api(start_with)
}

pub(super) fn stop() -> Result<()> {
    with_platform_api(stop_with)
}

pub(super) fn deactivate() -> Result<()> {
    with_platform_api(deactivate_with)
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
            api.run()?;
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

    pub(super) struct NativeTaskScheduler {
        root: ITaskFolder,
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
                _apartment: apartment,
            })
        }

        fn task(&self) -> Result<Option<IRegisteredTask>> {
            match unsafe { self.root.GetTask(&BSTR::from(TASK_PATH)) } {
                Ok(task) => Ok(Some(task)),
                Err(error) if is_task_not_found(&error) => Ok(None),
                Err(error) => Err(com_error("get TraceDecay daemon task", error)),
            }
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
            let empty = VARIANT::default();
            unsafe {
                self.root.RegisterTask(
                    &BSTR::from(TASK_PATH),
                    &BSTR::from(xml),
                    TASK_CREATE_OR_UPDATE.0,
                    &empty,
                    &empty,
                    TASK_LOGON_INTERACTIVE_TOKEN,
                    &empty,
                )
            }
            .map(drop)
            .map_err(|error| com_error("register TraceDecay daemon task", error))
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
            match unsafe { self.root.DeleteTask(&BSTR::from(TASK_NAME), 0) } {
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
            }
        }

        fn state(&self) -> DaemonServiceState {
            state_from_snapshot(self.task)
        }
    }

    impl TaskSchedulerApi for FakeTaskScheduler {
        fn snapshot(&mut self) -> Result<Option<TaskSnapshot>> {
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
            task.running = false;
            Ok(())
        }

        fn delete(&mut self) -> Result<()> {
            self.operations.push(Operation::Delete);
            self.task = None;
            self.xml = None;
            Ok(())
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
    fn task_xml_escapes_action_paths_and_declares_daemon_settings() {
        let xml = render_task_xml(&spec(
            r#"C:\Program Files\Trace<&"'Decay\tracedecay.exe"#,
            r#"C:\Users\Zack & <Trace>"'Decay"#,
        ))
        .expect("render task XML");

        assert!(xml.contains(
            r#"<Command>C:\Program Files\Trace&lt;&amp;&quot;&apos;Decay\tracedecay.exe</Command>"#
        ));
        assert!(xml.contains(
            r#"<Arguments>daemon run --profile-root &quot;C:\Users\Zack &amp; &lt;Trace&gt;\&quot;&apos;Decay&quot;</Arguments>"#
        ));
        assert!(xml.contains("<LogonTrigger>"));
        assert!(xml.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(xml.contains("<RunLevel>LeastPrivilege</RunLevel>"));
        assert!(!xml.contains("<UserId>"));
        assert!(xml.contains("<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>"));
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
        assert!(xml.contains("<Interval>PT2S</Interval>"));
        assert!(xml.contains("<Count>255</Count>"));
        assert!(xml.contains("<Enabled>true</Enabled>"));
    }

    #[test]
    fn task_xml_profile_root_round_trips_escaped_text() {
        let profile_root = PathBuf::from("C:\\Users\\Z & <Trace>\"'Decay\\");
        let xml = render_task_xml(&spec(
            r"C:\Users\Z\scoop\apps\tracedecay\current\tracedecay.exe",
            &profile_root,
        ))
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
}
