use std::collections::BTreeSet;
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
    // The action text is wire data for Task Scheduler, so it is asserted
    // byte-exactly. On Windows `render_task_xml_for` fully qualifies the
    // profile root, which spells it with the native separator; the Unix
    // arm renders the fixture text verbatim.
    #[cfg(windows)]
    let expected_arguments = r#"daemon run --profile-root "C:\profiles\stable & exact""#;
    #[cfg(not(windows))]
    let expected_arguments = r#"daemon run --profile-root "C:/profiles/stable & exact""#;
    assert_eq!(action.arguments, expected_arguments);
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Operation {
    Register(String),
    Enable(bool),
    Run,
    Stop,
    Delete,
}

#[derive(Clone, Copy, Debug, Ord, PartialOrd, Eq, PartialEq)]
enum FailurePoint {
    Enablement,
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
    fail_next: BTreeSet<FailurePoint>,
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
            fail_next: BTreeSet::new(),
            stop_leaves_running: false,
            snapshots_until_exit: None,
            snapshot_count: 0,
        }
    }

    fn state(&self) -> DaemonServiceState {
        state_from_snapshot(self.task)
    }

    fn fail_next(&mut self, point: FailurePoint) {
        self.fail_next.insert(point);
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
        if self.fail_next.remove(&FailurePoint::Enablement) {
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
        if self.fail_next.remove(&FailurePoint::Run) {
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
        if self.fail_next.remove(&FailurePoint::Stop) {
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
        if self.fail_next.remove(&FailurePoint::Delete) {
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
        remote_tls: None,
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

#[test]
fn task_xml_round_trips_remote_tls_listener_paths() {
    let identity = TaskIdentity::for_user_sid(TEST_SID).expect("task identity");
    let remote_tls = crate::RemoteBrainTlsConfig::from_optional_parts(
        Some("192.0.2.10:7443".parse().expect("listener address")),
        Some(PathBuf::from(r"C:\TraceDecay TLS\server & chain.pem")),
        Some(PathBuf::from(r"C:\TraceDecay TLS\server % key.pem")),
    )
    .expect("valid Remote Brain TLS task configuration")
    .expect("enabled Remote Brain TLS task configuration");
    let mut service_spec = spec(r"C:\TraceDecay\tracedecay.exe", r"C:\TraceDecay\data");
    service_spec.remote_tls = Some(remote_tls.clone());

    let xml = render_task_xml_for(&service_spec, &identity).expect("task XML");

    assert_eq!(
        remote_tls_from_task_xml(&xml).expect("parse task Remote Brain arguments"),
        Some(remote_tls)
    );
    assert!(!xml.contains("PRIVATE KEY"));
}

#[test]
fn task_xml_rejects_ambiguous_remote_tls_argument_quoting() {
    let xml = r"<Task><Arguments>daemon run --remote-listen 192.0.2.10:7443 --remote-tls-cert &quot;C:\TraceDecay TLS\server.pem --remote-tls-key C:\TraceDecay\server-key.pem</Arguments></Task>";

    let error = remote_tls_from_task_xml(xml)
        .expect_err("unterminated Windows argument quoting must fail closed");

    assert!(error.to_string().contains("unterminated quoted argument"));
}

#[test]
fn task_xml_rejects_partial_and_duplicate_remote_tls_arguments() {
    let partial =
        r"<Task><Arguments>daemon run --remote-listen 192.0.2.10:7443</Arguments></Task>";
    let duplicate = r"<Task><Arguments>daemon run --remote-listen 192.0.2.10:7443 --remote-listen 192.0.2.11:7443 --remote-tls-cert C:\TraceDecay\server.pem --remote-tls-key C:\TraceDecay\server-key.pem</Arguments></Task>";

    assert!(remote_tls_from_task_xml(partial).is_err());
    assert!(remote_tls_from_task_xml(duplicate).is_err());
}

#[test]
fn task_xml_rejects_remote_tls_path_control_characters() {
    let identity = TaskIdentity::for_user_sid(TEST_SID).expect("task identity");
    let remote_tls = crate::RemoteBrainTlsConfig::from_optional_parts(
        Some("192.0.2.10:7443".parse().expect("listener address")),
        Some(PathBuf::from("C:\\TraceDecay TLS\\server.pem\nInjected")),
        Some(PathBuf::from(r"C:\TraceDecay TLS\server-key.pem")),
    )
    .expect("complete Remote Brain TLS task configuration")
    .expect("enabled Remote Brain TLS task configuration");
    let mut service_spec = spec(r"C:\TraceDecay\tracedecay.exe", r"C:\TraceDecay\data");
    service_spec.remote_tls = Some(remote_tls);

    let error = render_task_xml_for(&service_spec, &identity)
        .expect_err("control characters must not enter task XML");

    assert!(error.to_string().contains("control character"));
}

#[cfg(windows)]
#[test]
fn task_xml_rejects_relative_remote_tls_paths() {
    let identity = TaskIdentity::for_user_sid(TEST_SID).expect("task identity");
    let remote_tls = crate::RemoteBrainTlsConfig::from_optional_parts(
        Some("192.0.2.10:7443".parse().expect("listener address")),
        Some(PathBuf::from("server.pem")),
        Some(PathBuf::from("server-key.pem")),
    )
    .expect("complete Remote Brain TLS task configuration")
    .expect("enabled Remote Brain TLS task configuration");
    let mut service_spec = spec(r"C:\TraceDecay\tracedecay.exe", r"C:\TraceDecay\data");
    service_spec.remote_tls = Some(remote_tls);

    assert!(render_task_xml_for(&service_spec, &identity).is_err());
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
    api.fail_next(FailurePoint::Run);

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
    api.fail_next(FailurePoint::Enablement);

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
    api.fail_next(FailurePoint::Delete);

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
    api.fail_next(FailurePoint::Enablement);

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
    api.fail_next(FailurePoint::Run);

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
    api.fail_next(FailurePoint::Stop);

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
