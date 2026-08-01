use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

#[cfg(unix)]
use std::io::{BufRead, Write};
#[cfg(target_os = "linux")]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::net::UnixListener;
#[cfg(unix)]
use tempfile::TempDir;

#[cfg(target_os = "linux")]
use super::runner::ServiceRunner;
use super::runner::{LaunchctlFailureMode, LaunchdCommand};
use super::{DaemonServiceSpec, DaemonServiceState};
use crate::config::lock_user_data_dir_test_env;

struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let previous = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }

    fn unset(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        unsafe {
            std::env::remove_var(key);
        }
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(previous) = self.previous.take() {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

struct CurrentDirGuard {
    previous: PathBuf,
}

impl CurrentDirGuard {
    fn set(path: impl AsRef<std::path::Path>) -> Self {
        let previous = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(path).expect("set current dir");
        Self { previous }
    }
}

impl Drop for CurrentDirGuard {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.previous).expect("restore current dir");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn service_status_includes_journalctl_debug_command() {
    let status = super::service_status(&PathBuf::from("/tmp/tracedecay.sock"));

    assert!(status.contains("logs: journalctl --user -u tracedecay.service -f"));
}

#[cfg(target_os = "macos")]
#[test]
fn service_status_includes_launchd_debug_commands() {
    let _env_lock = lock_user_data_dir_test_env();
    let profile = tempfile::TempDir::new().expect("profile temp dir");
    let _data_dir_guard = EnvVarGuard::set(crate::config::USER_DATA_DIR_ENV, profile.path());

    let status = super::service_status(&PathBuf::from("/tmp/tracedecay.sock"));
    let expected_log = crate::config::user_data_dir()
        .expect("user data dir")
        .join("daemon.err.log");

    assert!(status.contains("service-detail: launchctl print gui/"));
    assert!(status.contains("/com.tracedecay.daemon"));
    assert!(status.contains(&format!("logs: tail -f \"{}\"", expected_log.display())));
}

#[cfg(unix)]
#[test]
fn service_status_reports_missing_socket() {
    let dir = TempDir::new().expect("temp dir");
    let socket = dir.path().join("missing.sock");

    let status = super::service_status(&socket);

    assert!(
        status.contains(&format!("socket: {} (missing)", socket.display())),
        "status should report missing socket, got:\n{status}"
    );
}

#[cfg(unix)]
#[test]
fn service_status_reports_unconnectable_socket_file() {
    let dir = TempDir::new().expect("temp dir");
    let socket = dir.path().join("unconnectable.sock");
    std::fs::write(&socket, "").expect("unconnectable socket placeholder");

    let status = super::service_status(&socket);

    assert!(
        status.contains(&format!("socket: {} (stale)", socket.display()))
            || status.contains(&format!(
                "socket: {} (present but unreachable)",
                socket.display()
            )),
        "status should report an unconnectable socket, got:\n{status}"
    );
}

#[cfg(unix)]
#[test]
fn service_status_reports_connectable_socket() {
    let dir = TempDir::new().expect("temp dir");
    let socket = dir.path().join("daemon.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind socket");

    let status = super::service_status(&socket);

    assert!(
        status.contains(&format!("socket: {} (connectable)", socket.display())),
        "status should report connectable socket, got:\n{status}"
    );
}

#[test]
fn strict_restoration_requires_readiness_only_for_running_state() {
    assert!(super::restored_service_matches(
        DaemonServiceState::RunningEnabled,
        DaemonServiceState::RunningEnabled,
        super::probe::DaemonSocketState::Connectable,
        &super::probe::DaemonProtocolState::Ready,
    ));
    assert!(!super::restored_service_matches(
        DaemonServiceState::RunningEnabled,
        DaemonServiceState::RunningEnabled,
        super::probe::DaemonSocketState::Missing,
        &super::probe::DaemonProtocolState::NotRequired,
    ));
    assert!(super::restored_service_matches(
        DaemonServiceState::StoppedEnabled,
        DaemonServiceState::StoppedEnabled,
        super::probe::DaemonSocketState::Missing,
        &super::probe::DaemonProtocolState::NotRequired,
    ));
    assert!(!super::restored_service_matches(
        DaemonServiceState::StoppedEnabled,
        DaemonServiceState::RunningEnabled,
        super::probe::DaemonSocketState::Connectable,
        &super::probe::DaemonProtocolState::Ready,
    ));
    assert!(!super::restored_service_matches(
        DaemonServiceState::RunningEnabled,
        DaemonServiceState::RunningEnabled,
        super::probe::DaemonSocketState::Connectable,
        &super::probe::DaemonProtocolState::Unresponsive("not TraceDecay".to_string()),
    ));
}

#[cfg(unix)]
fn serve_probe_response(
    listener: UnixListener,
    name: &'static str,
    version: &'static str,
    expected_auth_token: Option<String>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept readiness probe");
        let mut reader =
            std::io::BufReader::new(stream.try_clone().expect("clone readiness stream"));
        let mut line = String::new();
        if let Some(expected_auth_token) = expected_auth_token {
            reader.read_line(&mut line).expect("read auth preface");
            let preface = super::super::transport::DaemonAuthPreface::from_line(line.trim())
                .expect("parse auth preface");
            assert!(preface.authenticate(&expected_auth_token));
            line.clear();
        }
        reader.read_line(&mut line).expect("read handshake");
        line.clear();
        reader.read_line(&mut line).expect("read initialize");
        let request: serde_json::Value =
            serde_json::from_str(line.trim()).expect("initialize json");
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": request["id"],
            "result": {
                "serverInfo": {"name": name, "version": version}
            }
        });
        writeln!(stream, "{response}").expect("write initialize response");
    })
}

#[cfg(unix)]
#[test]
fn daemon_protocol_probe_requires_current_tracedecay_identity() {
    let _env_lock = lock_user_data_dir_test_env();
    let profile = TempDir::new().expect("profile temp dir");
    let _data_dir_guard = EnvVarGuard::set(crate::config::USER_DATA_DIR_ENV, profile.path());

    let ready_socket = profile.path().join("ready.sock");
    let ready_listener = UnixListener::bind(&ready_socket).expect("bind ready socket");
    let ready_server = serve_probe_response(
        ready_listener,
        "tracedecay",
        crate::version::build_version(),
        None,
    );
    assert_eq!(
        super::probe::daemon_protocol_state(&ready_socket),
        super::probe::DaemonProtocolState::Ready
    );
    ready_server.join().expect("join ready server");

    let stale_socket = profile.path().join("stale.sock");
    let stale_listener = UnixListener::bind(&stale_socket).expect("bind stale socket");
    let stale_server = serve_probe_response(stale_listener, "tracedecay", "0.0.0-stale", None);
    assert_eq!(
        super::probe::daemon_protocol_state(&stale_socket),
        super::probe::DaemonProtocolState::IdentityMismatch {
            name: Some("tracedecay".to_string()),
            version: Some("0.0.0-stale".to_string()),
        }
    );
    stale_server.join().expect("join stale server");
}

#[cfg(unix)]
#[test]
fn daemon_protocol_probe_authenticates_to_managed_daemon() {
    let _env_lock = lock_user_data_dir_test_env();
    let profile = TempDir::new().expect("profile temp dir");
    let _data_dir_guard = EnvVarGuard::set(crate::config::USER_DATA_DIR_ENV, profile.path());
    let socket_path = profile.path().join("daemon.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind managed daemon socket");
    let endpoint = super::super::transport::DaemonEndpoint::Unix(socket_path.clone());
    let authority = super::super::authority::DaemonAuthority::acquire(
        profile.path(),
        &endpoint,
        env!("CARGO_PKG_VERSION"),
    )
    .expect("publish daemon authority");
    let server = serve_probe_response(
        listener,
        "tracedecay",
        crate::version::build_version(),
        Some(authority.auth_token().to_string()),
    );

    assert_eq!(
        super::probe::daemon_protocol_state(&socket_path),
        super::probe::DaemonProtocolState::Ready
    );
    server.join().expect("join authenticated probe server");
}

#[test]
fn daemon_shutdown_response_requires_matching_acknowledgement() {
    assert!(super::probe::shutdown_response_accepted(
        r#"{"jsonrpc":"2.0","id":2,"result":{"accepted":true}}"#,
        2
    ));
    assert!(!super::probe::shutdown_response_accepted(
        r#"{"jsonrpc":"2.0","id":3,"result":{"accepted":true}}"#,
        2
    ));
    assert!(!super::probe::shutdown_response_accepted(
        r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32603,"message":"no"}}"#,
        2
    ));
}

#[test]
fn user_service_runs_daemon_with_socket_path() {
    let spec = DaemonServiceSpec {
        tracedecay_bin: PathBuf::from("/usr/local/bin/tracedecay"),
        socket_path: PathBuf::from("/tmp/tracedecay.sock"),
        data_dir_override: None,
    };

    let unit = spec.render_systemd_user_unit();

    assert!(
        unit.contains(
            "ExecStart=/usr/local/bin/tracedecay daemon run --socket /tmp/tracedecay.sock"
        )
    );
    assert!(unit.contains("Environment=\"PATH="));
    assert!(unit.contains("Environment=\"MALLOC_ARENA_MAX=2\""));
    assert!(unit.contains("Restart=on-failure"));
    assert!(unit.contains("LimitNOFILE=8192"));
}

// The launchd render tests use Unix-style absolute binary paths, which
// `Path::is_absolute` rejects on Windows; launchd is Unix-only anyway.
#[cfg(unix)]
#[test]
fn render_launchd_plist_includes_program_arguments_socket_logs_and_label() {
    let _env_lock = lock_user_data_dir_test_env();
    let profile = tempfile::TempDir::new().expect("profile temp dir");
    let home = tempfile::TempDir::new().expect("home temp dir");
    let _home_guard = EnvVarGuard::set("HOME", home.path());
    let spec = DaemonServiceSpec {
        tracedecay_bin: PathBuf::from("/opt/tracedecay/bin/tracedecay"),
        socket_path: profile.path().join("daemon.sock"),
        data_dir_override: Some(profile.path().to_path_buf()),
    };

    let plist = spec.render_launchd_plist().expect("launchd plist");

    assert!(plist.contains("<key>Label</key>"));
    assert!(plist.contains("<string>com.tracedecay.daemon</string>"));
    assert!(plist.contains("<key>ProgramArguments</key>"));
    assert!(plist.contains("<string>/opt/tracedecay/bin/tracedecay</string>"));
    assert!(plist.contains("<string>daemon</string>"));
    assert!(plist.contains("<string>run</string>"));
    assert!(plist.contains("<string>--socket</string>"));
    assert!(plist.contains(&format!(
        "<string>{}</string>",
        profile.path().join("daemon.sock").display()
    )));
    assert!(plist.contains(&format!(
        "<string>{}</string>",
        profile.path().join("daemon.out.log").display()
    )));
    assert!(plist.contains(&format!(
        "<string>{}</string>",
        profile.path().join("daemon.err.log").display()
    )));
    assert!(plist.contains("<key>TRACEDECAY_DATA_DIR</key>"));
    assert!(plist.contains("<key>RunAtLoad</key>"));
    assert!(plist.contains("<key>KeepAlive</key>"));
    assert!(plist.contains("<key>SoftResourceLimits</key>"));
    assert!(plist.contains("<key>NumberOfFiles</key>"));
    assert!(plist.contains("<integer>8192</integer>"));
}

#[cfg(unix)]
#[test]
fn render_launchd_plist_escapes_xml_and_parser_unescapes_socket_path() {
    let _env_lock = lock_user_data_dir_test_env();
    let profile = tempfile::TempDir::new().expect("profile temp dir");
    let home = tempfile::TempDir::new().expect("home temp dir");
    let _home_guard = EnvVarGuard::set("HOME", home.path());
    let _data_dir_guard = EnvVarGuard::set(crate::config::USER_DATA_DIR_ENV, profile.path());
    let socket_path = PathBuf::from("/tmp/trace<decay>&\"socket'.sock");
    let spec = DaemonServiceSpec {
        tracedecay_bin: PathBuf::from("/opt/trace&decay/bin/tracedecay"),
        socket_path: socket_path.clone(),
        data_dir_override: None,
    };

    let plist = spec.render_launchd_plist().expect("launchd plist");

    assert!(plist.contains("/opt/trace&amp;decay/bin/tracedecay"));
    assert!(plist.contains("/tmp/trace&lt;decay&gt;&amp;&quot;socket&apos;.sock"));
    assert_eq!(
        super::unit_file::socket_path_from_launchd_plist(&plist),
        Some(socket_path)
    );
}

#[test]
fn socket_path_from_launchd_plist_returns_none_for_malformed_input() {
    assert_eq!(
        super::unit_file::socket_path_from_launchd_plist("<plist></plist>"),
        None
    );
    assert_eq!(
        super::unit_file::socket_path_from_launchd_plist(
            "<key>ProgramArguments</key><array><string>tracedecay</string></array>"
        ),
        None
    );
}

#[test]
fn socket_path_from_launchd_plist_accepts_socket_equals_form() {
    let plist = "\
            <key>ProgramArguments</key>\
            <array>\
              <string>/opt/tracedecay/bin/tracedecay</string>\
              <string>daemon</string>\
              <string>run</string>\
              <string>--socket=/tmp/tracedecay.sock</string>\
            </array>";

    assert_eq!(
        super::unit_file::socket_path_from_launchd_plist(plist),
        Some(PathBuf::from("/tmp/tracedecay.sock"))
    );
}

#[cfg(unix)]
#[test]
fn launchd_plist_env_value_round_trips_data_dir_override() {
    let _env_lock = lock_user_data_dir_test_env();
    let profile = tempfile::TempDir::new().expect("profile temp dir");
    let home = tempfile::TempDir::new().expect("home temp dir");
    let _home_guard = EnvVarGuard::set("HOME", home.path());
    let spec = DaemonServiceSpec {
        tracedecay_bin: PathBuf::from("/opt/tracedecay/bin/tracedecay"),
        socket_path: profile.path().join("daemon.sock"),
        data_dir_override: Some(profile.path().to_path_buf()),
    };

    let plist = spec.render_launchd_plist().expect("launchd plist");

    assert_eq!(
        super::unit_file::launchd_plist_env_value(&plist, crate::config::USER_DATA_DIR_ENV),
        Some(profile.path().display().to_string())
    );
    assert_eq!(
        super::unit_file::launchd_plist_env_value(&plist, "MISSING_VAR"),
        None
    );
}

#[cfg(unix)]
#[test]
fn launchd_plist_env_value_ignores_plist_without_override() {
    let _env_lock = lock_user_data_dir_test_env();
    let profile = tempfile::TempDir::new().expect("profile temp dir");
    let home = tempfile::TempDir::new().expect("home temp dir");
    let _home_guard = EnvVarGuard::set("HOME", home.path());
    let _data_dir_guard = EnvVarGuard::set(crate::config::USER_DATA_DIR_ENV, profile.path());
    let spec = DaemonServiceSpec {
        tracedecay_bin: PathBuf::from("/opt/tracedecay/bin/tracedecay"),
        socket_path: profile.path().join("daemon.sock"),
        data_dir_override: None,
    };

    let plist = spec.render_launchd_plist().expect("launchd plist");

    assert_eq!(
        super::unit_file::launchd_plist_env_value(&plist, crate::config::USER_DATA_DIR_ENV),
        None
    );
}

#[test]
fn launchd_command_plans_map_start_and_uninstall_sequences() {
    let service_path = PathBuf::from("/Users/me/Library/LaunchAgents/com.tracedecay.daemon.plist");

    assert_eq!(
        super::runner::launchd_start_command_plan(
            "gui/501",
            "gui/501/com.tracedecay.daemon",
            &service_path
        ),
        vec![
            LaunchdCommand::new(
                &["bootout", "gui/501/com.tracedecay.daemon"],
                LaunchctlFailureMode::TolerateNotLoaded
            ),
            LaunchdCommand::new(
                &["enable", "gui/501/com.tracedecay.daemon"],
                LaunchctlFailureMode::Fail
            ),
            LaunchdCommand::new(
                &[
                    "bootstrap",
                    "gui/501",
                    "/Users/me/Library/LaunchAgents/com.tracedecay.daemon.plist"
                ],
                LaunchctlFailureMode::Fail
            ),
            LaunchdCommand::new(
                &["kickstart", "-k", "gui/501/com.tracedecay.daemon"],
                LaunchctlFailureMode::Fail
            ),
        ]
    );
    assert_eq!(
        super::runner::launchd_uninstall_command_plan("gui/501/com.tracedecay.daemon"),
        vec![
            LaunchdCommand::new(
                &["bootout", "gui/501/com.tracedecay.daemon"],
                LaunchctlFailureMode::TolerateNotLoaded
            ),
            LaunchdCommand::new(
                &["disable", "gui/501/com.tracedecay.daemon"],
                LaunchctlFailureMode::Ignore
            ),
        ]
    );
}

#[test]
fn launchctl_stderr_not_loaded_matches_known_messages_only() {
    assert!(super::runner::launchctl_stderr_is_not_loaded(
        "Boot-out failed: 3: No such process"
    ));
    assert!(super::runner::launchctl_stderr_is_not_loaded(
        "Could not find service \"com.tracedecay.daemon\" in domain for user gui: 501"
    ));
    assert!(super::runner::launchctl_stderr_is_not_loaded(
        "service is not loaded"
    ));
    assert!(!super::runner::launchctl_stderr_is_not_loaded(
        "Boot-out failed: 5: Input/output error"
    ));
    assert!(!super::runner::launchctl_stderr_is_not_loaded(""));
}

#[test]
fn launchd_disabled_output_matches_only_the_tracedecay_label() {
    assert!(super::runner::launchd_disabled_output_contains_label(
        "disabled services = {\n\t\"com.tracedecay.daemon\" => true\n}",
        "com.tracedecay.daemon"
    ));
    assert!(!super::runner::launchd_disabled_output_contains_label(
        "disabled services = {\n\t\"com.tracedecay.daemon\" => false\n}",
        "com.tracedecay.daemon"
    ));
    assert!(!super::runner::launchd_disabled_output_contains_label(
        "disabled services = {\n\t\"com.example.other\" => true\n}",
        "com.tracedecay.daemon"
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn atomic_service_write_faults_preserve_the_forward_boundary() {
    use super::unit_file::AtomicServiceWriteStep;

    let dir = TempDir::new().expect("temp dir");
    let service_path = dir.path().join("tracedecay.service");
    let old_unit = "[Service]\nExecStart=/old/absolute/tracedecay daemon run\n";
    let new_unit = "[Service]\nExecStart=/new/absolute/tracedecay daemon run\n";
    let steps = [
        AtomicServiceWriteStep::TempWrite,
        AtomicServiceWriteStep::TempFsync,
        AtomicServiceWriteStep::Rename,
        AtomicServiceWriteStep::ParentFsync,
    ];

    for failed_step in steps {
        std::fs::write(&service_path, old_unit).expect("old unit");
        let mut observed = Vec::new();
        let error = super::unit_file::atomic_replace_service_unit_with(
            &service_path,
            new_unit,
            &mut |step| {
                observed.push(step);
                if step == failed_step {
                    Err(crate::errors::TraceDecayError::Config {
                        message: format!("injected {step:?} failure"),
                    })
                } else {
                    Ok(())
                }
            },
        )
        .expect_err("fault must stop the atomic replacement");

        assert!(error.to_string().contains("injected"));
        let selected = std::fs::read_to_string(&service_path).expect("selected unit");
        if failed_step == AtomicServiceWriteStep::ParentFsync {
            assert_eq!(selected, new_unit, "rename is the visibility boundary");
        } else {
            assert_eq!(selected, old_unit, "old unit must remain selected");
        }
        let leftovers = std::fs::read_dir(dir.path())
            .expect("service parent")
            .filter_map(Result::ok)
            .filter(|entry| entry.path() != service_path)
            .count();
        assert_eq!(leftovers, 0, "temporary unit must be cleaned up");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn atomic_service_write_sets_permissions_and_orders_durability_steps() {
    use super::unit_file::AtomicServiceWriteStep;

    let dir = TempDir::new().expect("temp dir");
    let service_path = dir.path().join("tracedecay.service");
    let mut observed = Vec::new();

    super::unit_file::atomic_replace_service_unit_with(&service_path, "new unit\n", &mut |step| {
        observed.push(step);
        Ok(())
    })
    .expect("atomic service write");

    assert_eq!(
        observed,
        vec![
            AtomicServiceWriteStep::TempWrite,
            AtomicServiceWriteStep::TempFsync,
            AtomicServiceWriteStep::Rename,
            AtomicServiceWriteStep::ParentFsync,
        ]
    );
    assert_eq!(
        std::fs::metadata(&service_path)
            .expect("service metadata")
            .permissions()
            .mode()
            & 0o777,
        0o644
    );
}

#[cfg(target_os = "linux")]
#[test]
fn forward_only_commit_reloads_durable_unit_before_stop() {
    let dir = TempDir::new().expect("temp dir");
    let service_path = dir.path().join("tracedecay.service");
    let old_unit = "[Service]\nExecStart=/old/absolute/tracedecay daemon run\n";
    let new_unit = "[Service]\nExecStart=/new/absolute/tracedecay daemon run\n";
    std::fs::write(&service_path, old_unit).expect("old unit");
    let observed = std::cell::RefCell::new(Vec::new());

    let (_, state) = super::commit_forward_only_definition_with(
        || {
            super::unit_file::atomic_replace_service_unit_with(
                &service_path,
                new_unit,
                &mut |step| {
                    observed.borrow_mut().push(format!("{step:?}"));
                    Ok(())
                },
            )?;
            Ok(service_path.clone())
        },
        |_| {
            assert_eq!(
                std::fs::read_to_string(&service_path).expect("reloaded unit"),
                new_unit
            );
            observed.borrow_mut().push("Reload".to_string());
            Ok(())
        },
        || {
            observed.borrow_mut().push("Stop".to_string());
            Ok(DaemonServiceState::RunningEnabled)
        },
    )
    .expect("forward-only commit");

    assert_eq!(state, DaemonServiceState::RunningEnabled);
    assert_eq!(
        observed.into_inner(),
        vec![
            "TempWrite",
            "TempFsync",
            "Rename",
            "ParentFsync",
            "Reload",
            "Stop",
        ]
    );
}

#[cfg(target_os = "linux")]
#[test]
fn forward_only_reload_failure_keeps_old_process_and_new_unit() {
    let dir = TempDir::new().expect("temp dir");
    let service_path = dir.path().join("tracedecay.service");
    let mut stopped = false;

    let error = super::commit_forward_only_definition_with(
        || {
            super::unit_file::atomic_replace_service_unit_with(
                &service_path,
                "ExecStart=/new/absolute/tracedecay\n",
                &mut |_| Ok(()),
            )?;
            Ok(service_path.clone())
        },
        |_| {
            Err(crate::errors::TraceDecayError::Config {
                message: "injected reload failure".to_string(),
            })
        },
        || {
            stopped = true;
            Ok(DaemonServiceState::RunningEnabled)
        },
    )
    .expect_err("reload failure");

    assert!(error.to_string().contains("reload"));
    assert!(!stopped, "old process must not be stopped before reload");
    assert!(
        std::fs::read_to_string(service_path)
            .expect("durable new unit")
            .contains("/new/absolute/tracedecay")
    );
}

#[cfg(target_os = "linux")]
#[test]
fn forward_only_stop_failure_retains_durable_reloaded_unit() {
    let dir = TempDir::new().expect("temp dir");
    let service_path = dir.path().join("tracedecay.service");
    let reloaded = std::cell::Cell::new(false);

    let error = super::commit_forward_only_definition_with(
        || {
            super::unit_file::atomic_replace_service_unit_with(
                &service_path,
                "ExecStart=/new/absolute/tracedecay\n",
                &mut |_| Ok(()),
            )?;
            Ok(service_path.clone())
        },
        |_| {
            reloaded.set(true);
            Ok(())
        },
        || {
            assert!(reloaded.get(), "reload must precede stop");
            Err(crate::errors::TraceDecayError::Config {
                message: "injected stop failure".to_string(),
            })
        },
    )
    .expect_err("stop failure");

    assert!(error.to_string().contains("stop"));
    assert!(
        std::fs::read_to_string(service_path)
            .expect("durable new unit")
            .contains("/new/absolute/tracedecay")
    );
}

#[cfg(target_os = "linux")]
#[test]
fn forward_only_recovery_reloads_before_deactivate() {
    let observed = std::cell::RefCell::new(Vec::new());

    let (_, deactivate_error) = super::recover_forward_only_definition_with(
        || {
            observed.borrow_mut().push("Write".to_string());
            Ok(PathBuf::from("/service/unit"))
        },
        |_| {
            observed.borrow_mut().push("Reload".to_string());
            Ok(())
        },
        || {
            observed.borrow_mut().push("Deactivate".to_string());
            Err(crate::errors::TraceDecayError::Config {
                message: "injected deactivate failure".to_string(),
            })
        },
    )
    .expect("durable write and reload are the recovery boundary");

    assert_eq!(observed.into_inner(), vec!["Write", "Reload", "Deactivate"]);
    assert!(
        deactivate_error
            .expect("deactivate error retained")
            .to_string()
            .contains("deactivate")
    );
}

#[cfg(target_os = "linux")]
#[test]
fn refresh_service_rewrites_unit_and_restarts_daemon() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("temp dir");
    let config_home = dir.path().join("config");
    let fake_bin = dir.path().join("bin");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&fake_bin).expect("fake bin dir");
    std::fs::create_dir_all(&home).expect("home dir");

    let systemctl = fake_bin.join("systemctl");
    let log = dir.path().join("systemctl.log");
    std::fs::write(
        &systemctl,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TRACEDECAY_SYSTEMCTL_LOG\"\n",
    )
    .expect("fake systemctl");
    std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755))
        .expect("systemctl permissions");

    let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
    let _home_guard = EnvVarGuard::set("HOME", &home);
    let _data_guard =
        EnvVarGuard::set(crate::config::USER_DATA_DIR_ENV, dir.path().join("profile"));
    let _path_guard = EnvVarGuard::set("PATH", &fake_bin);
    let _log_guard = EnvVarGuard::set("TRACEDECAY_SYSTEMCTL_LOG", &log);
    let spec = DaemonServiceSpec {
        tracedecay_bin: PathBuf::from("/opt/tracedecay/bin/tracedecay"),
        socket_path: PathBuf::from("/run/user/1000/tracedecay.sock"),
        data_dir_override: None,
    };

    let service_path = super::refresh_service(&spec).expect("refresh service");

    assert_eq!(
        service_path,
        config_home
            .join("systemd/user")
            .join(crate::daemon::SERVICE_NAME)
    );
    let unit = std::fs::read_to_string(&service_path).expect("service unit");
    assert!(unit.contains(
            "ExecStart=/opt/tracedecay/bin/tracedecay daemon run --socket /run/user/1000/tracedecay.sock"
        ));
    assert_eq!(
        std::fs::read_to_string(log).expect("systemctl log"),
        "--user daemon-reload\n--user enable tracedecay.service\n--user daemon-reload\n--user enable tracedecay.service\n--user start tracedecay.service\n"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn forward_only_recovery_rewrites_old_execstart_and_disables_service() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("temp dir");
    let config_home = dir.path().join("config with spaces");
    let fake_bin = dir.path().join("bin");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&fake_bin).expect("fake bin dir");
    std::fs::create_dir_all(&home).expect("home dir");

    let systemctl = fake_bin.join("systemctl");
    let log = dir.path().join("systemctl.log");
    let running = dir.path().join("running");
    let enabled = dir.path().join("enabled");
    std::fs::write(&running, "").expect("running marker");
    std::fs::write(&enabled, "").expect("enabled marker");
    std::fs::write(
        &systemctl,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$TRACEDECAY_SYSTEMCTL_LOG"
case "$2" in
  is-active) test -f "$TRACEDECAY_SYSTEMCTL_RUNNING" ;;
  is-enabled)
    if test -f "$TRACEDECAY_SYSTEMCTL_ENABLED"; then
      printf 'enabled\n'
    else
      printf 'disabled\n'
    fi
    ;;
  disable)
    /usr/bin/rm -f "$TRACEDECAY_SYSTEMCTL_RUNNING" "$TRACEDECAY_SYSTEMCTL_ENABLED"
    ;;
  stop) /usr/bin/rm -f "$TRACEDECAY_SYSTEMCTL_RUNNING" ;;
esac
"#,
    )
    .expect("fake systemctl");
    std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755))
        .expect("systemctl permissions");

    let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
    let _home_guard = EnvVarGuard::set("HOME", &home);
    let _data_guard =
        EnvVarGuard::set(crate::config::USER_DATA_DIR_ENV, dir.path().join("profile"));
    let _path_guard = EnvVarGuard::set("PATH", &fake_bin);
    let _log_guard = EnvVarGuard::set("TRACEDECAY_SYSTEMCTL_LOG", &log);
    let _running_guard = EnvVarGuard::set("TRACEDECAY_SYSTEMCTL_RUNNING", &running);
    let _enabled_guard = EnvVarGuard::set("TRACEDECAY_SYSTEMCTL_ENABLED", &enabled);

    let old_spec = DaemonServiceSpec {
        tracedecay_bin: PathBuf::from("/old absolute/tracedecay"),
        socket_path: dir.path().join("daemon.sock"),
        data_dir_override: None,
    };
    let service_path = super::unit_file::write_service_unit(&old_spec).expect("old service unit");
    let new_spec = DaemonServiceSpec {
        tracedecay_bin: dir.path().join("new install/tracedecay"),
        socket_path: old_spec.socket_path.clone(),
        data_dir_override: None,
    };

    let recovered =
        super::enforce_forward_only_service_recovery(&new_spec).expect("forward-only recovery");

    assert_eq!(recovered, Some(service_path.clone()));
    let unit = std::fs::read_to_string(service_path).expect("rewritten service unit");
    assert!(unit.contains(&new_spec.tracedecay_bin.display().to_string()));
    assert!(!unit.contains("/old absolute/tracedecay"));
    assert!(!running.exists(), "managed daemon must be inactive");
    assert!(!enabled.exists(), "managed daemon must be disabled");
    let commands = std::fs::read_to_string(log).expect("systemctl log");
    let reload = commands
        .find("--user daemon-reload")
        .expect("manager reload");
    let deactivate = commands
        .find("--user disable --now tracedecay.service")
        .expect("service deactivation");
    assert!(reload < deactivate, "reload must precede deactivation");
}

#[cfg(target_os = "linux")]
#[test]
fn forward_only_recovery_replaces_service_symlink_without_following_it() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("temp dir");
    let config_home = dir.path().join("config");
    let fake_bin = dir.path().join("bin");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&fake_bin).expect("fake bin dir");
    std::fs::create_dir_all(&home).expect("home dir");
    let systemctl = fake_bin.join("systemctl");
    std::fs::write(
            &systemctl,
            "#!/bin/sh\n[ \"$2\" = is-active ] && exit 3\n[ \"$2\" = is-enabled ] && printf 'disabled\\n'\nexit 0\n",
        )
        .expect("fake systemctl");
    std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755))
        .expect("systemctl permissions");

    let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
    let _home_guard = EnvVarGuard::set("HOME", &home);
    let _data_guard =
        EnvVarGuard::set(crate::config::USER_DATA_DIR_ENV, dir.path().join("profile"));
    let _path_guard = EnvVarGuard::set("PATH", &fake_bin);
    let service_path = config_home
        .join("systemd/user")
        .join(crate::daemon::SERVICE_NAME);
    std::fs::create_dir_all(service_path.parent().expect("service parent"))
        .expect("service parent");
    let external = dir.path().join("external unit");
    std::fs::write(&external, "external-old-unit\n").expect("external unit");
    std::os::unix::fs::symlink(&external, &service_path).expect("service symlink");
    let spec = DaemonServiceSpec {
        tracedecay_bin: dir.path().join("new install/tracedecay"),
        socket_path: dir.path().join("daemon.sock"),
        data_dir_override: None,
    };

    super::enforce_forward_only_service_recovery(&spec).expect("symlink-safe forward recovery");

    assert!(
        !std::fs::symlink_metadata(&service_path)
            .expect("service metadata")
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_to_string(&external).expect("external unit"),
        "external-old-unit\n"
    );
    assert!(
        std::fs::read_to_string(service_path)
            .expect("new service unit")
            .contains(&spec.tracedecay_bin.display().to_string())
    );
}

#[cfg(target_os = "linux")]
#[test]
fn forward_only_acquire_failure_never_restores_old_execstart() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("temp dir");
    let config_home = dir.path().join("config");
    let fake_bin = dir.path().join("bin");
    let home = dir.path().join("home");
    let profile = dir.path().join("profile");
    std::fs::create_dir_all(&fake_bin).expect("fake bin dir");
    std::fs::create_dir_all(&home).expect("home dir");
    std::fs::create_dir_all(&profile).expect("profile dir");

    let systemctl = fake_bin.join("systemctl");
    let log = dir.path().join("systemctl.log");
    let running = dir.path().join("running");
    let enabled = dir.path().join("enabled");
    std::fs::write(&running, "").expect("running marker");
    std::fs::write(&enabled, "").expect("enabled marker");
    std::fs::write(
        &systemctl,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$TRACEDECAY_SYSTEMCTL_LOG"
case "$2" in
  is-active) test -f "$TRACEDECAY_SYSTEMCTL_RUNNING" ;;
  is-enabled)
    test -f "$TRACEDECAY_SYSTEMCTL_ENABLED" && printf 'enabled\n' || printf 'disabled\n'
    ;;
  stop) /usr/bin/rm -f "$TRACEDECAY_SYSTEMCTL_RUNNING" ;;
  disable) /usr/bin/rm -f "$TRACEDECAY_SYSTEMCTL_RUNNING" "$TRACEDECAY_SYSTEMCTL_ENABLED" ;;
esac
"#,
    )
    .expect("fake systemctl");
    std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755))
        .expect("systemctl permissions");

    let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
    let _home_guard = EnvVarGuard::set("HOME", &home);
    let _data_guard = EnvVarGuard::set(crate::config::USER_DATA_DIR_ENV, &profile);
    let _path_guard = EnvVarGuard::set("PATH", &fake_bin);
    let _log_guard = EnvVarGuard::set("TRACEDECAY_SYSTEMCTL_LOG", &log);
    let _running_guard = EnvVarGuard::set("TRACEDECAY_SYSTEMCTL_RUNNING", &running);
    let _enabled_guard = EnvVarGuard::set("TRACEDECAY_SYSTEMCTL_ENABLED", &enabled);

    let old_spec = DaemonServiceSpec {
        tracedecay_bin: PathBuf::from("/old absolute/tracedecay"),
        socket_path: dir.path().join("daemon.sock"),
        data_dir_override: None,
    };
    let service_path = super::unit_file::write_service_unit(&old_spec).expect("old service unit");
    let new_spec = DaemonServiceSpec {
        tracedecay_bin: dir.path().join("new install/tracedecay"),
        socket_path: old_spec.socket_path.clone(),
        data_dir_override: None,
    };
    let _shared_lease = crate::lifecycle_lease::acquire_shared_blocking("test lease holder")
        .expect("shared lifecycle lease");

    let error = match super::QuiescedDaemonLifecycle::acquire_forward_only(
        "forward acquire test",
        &new_spec,
    ) {
        Ok(_) => panic!("exclusive acquisition must fail while shared lease is held"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("forward acquire test"));
    let unit = std::fs::read_to_string(service_path).expect("rewritten service unit");
    assert!(unit.contains(&new_spec.tracedecay_bin.display().to_string()));
    assert!(!unit.contains("/old absolute/tracedecay"));
    assert!(!running.exists(), "managed daemon must remain inactive");
    let commands = std::fs::read_to_string(log).expect("systemctl log");
    assert!(!commands.contains(" start "));
    assert!(!commands.contains(" restart "));
    assert!(
        commands
            .find("--user daemon-reload")
            .expect("manager reload")
            < commands.find("--user stop").expect("old service stop"),
        "new unit must be reloaded before the old process is stopped"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn forward_only_start_failure_is_recovered_inactive() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("temp dir");
    let config_home = dir.path().join("config");
    let fake_bin = dir.path().join("bin");
    let home = dir.path().join("home");
    let profile = dir.path().join("profile");
    std::fs::create_dir_all(&fake_bin).expect("fake bin dir");
    std::fs::create_dir_all(&home).expect("home dir");
    std::fs::create_dir_all(&profile).expect("profile dir");
    let systemctl = fake_bin.join("systemctl");
    let log = dir.path().join("systemctl.log");
    let running = dir.path().join("running");
    let enabled = dir.path().join("enabled");
    std::fs::write(&running, "").expect("running marker");
    std::fs::write(&enabled, "").expect("enabled marker");
    std::fs::write(
        &systemctl,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$TRACEDECAY_SYSTEMCTL_LOG"
case "$2" in
  is-active) test -f "$TRACEDECAY_SYSTEMCTL_RUNNING" ;;
  is-enabled)
    test -f "$TRACEDECAY_SYSTEMCTL_ENABLED" && printf 'enabled\n' || printf 'disabled\n'
    ;;
  stop) /usr/bin/rm -f "$TRACEDECAY_SYSTEMCTL_RUNNING" ;;
  disable) /usr/bin/rm -f "$TRACEDECAY_SYSTEMCTL_RUNNING" "$TRACEDECAY_SYSTEMCTL_ENABLED" ;;
  enable) /usr/bin/touch "$TRACEDECAY_SYSTEMCTL_ENABLED" ;;
  restart)
    /usr/bin/touch "$TRACEDECAY_SYSTEMCTL_RUNNING"
    exit 42
    ;;
esac
"#,
    )
    .expect("fake systemctl");
    std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755))
        .expect("systemctl permissions");
    let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
    let _home_guard = EnvVarGuard::set("HOME", &home);
    let _data_guard = EnvVarGuard::set(crate::config::USER_DATA_DIR_ENV, &profile);
    let _path_guard = EnvVarGuard::set("PATH", &fake_bin);
    let _log_guard = EnvVarGuard::set("TRACEDECAY_SYSTEMCTL_LOG", &log);
    let _running_guard = EnvVarGuard::set("TRACEDECAY_SYSTEMCTL_RUNNING", &running);
    let _enabled_guard = EnvVarGuard::set("TRACEDECAY_SYSTEMCTL_ENABLED", &enabled);
    let old_spec = DaemonServiceSpec {
        tracedecay_bin: PathBuf::from("/old absolute/tracedecay"),
        socket_path: dir.path().join("daemon.sock"),
        data_dir_override: None,
    };
    super::unit_file::write_service_unit(&old_spec).expect("old service unit");
    let new_spec = DaemonServiceSpec {
        tracedecay_bin: dir.path().join("new install/tracedecay"),
        socket_path: old_spec.socket_path,
        data_dir_override: None,
    };
    let guard =
        super::QuiescedDaemonLifecycle::acquire_forward_only("forward start test", &new_spec)
            .expect("forward-only acquisition");
    let previous_state = guard.previous_state();

    let start_error =
        super::refresh_installed_service_under_lease_with_state(&new_spec, previous_state)
            .expect_err("restart must fail");
    guard.finish_without_restore();
    super::enforce_forward_only_service_recovery(&new_spec).expect("start failure recovery");

    assert!(start_error.to_string().contains("restart"));
    assert!(!running.exists(), "failed start must be stopped");
    assert!(!enabled.exists(), "failed start must be disabled");
    let commands = std::fs::read_to_string(log).expect("systemctl log");
    assert!(commands.contains("--user restart tracedecay.service"));
    assert!(
        commands
            .rfind("--user daemon-reload")
            .expect("recovery reload")
            < commands
                .rfind("--user disable --now tracedecay.service")
                .expect("recovery deactivation"),
        "recovery must reload the durable unit before deactivation"
    );
    assert_eq!(
        commands
            .rmatch_indices("--user disable --now tracedecay.service")
            .count(),
        1
    );
}

#[cfg(target_os = "linux")]
#[test]
fn refresh_installed_service_skips_missing_unit() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("temp dir");
    let config_home = dir.path().join("config");
    let fake_bin = dir.path().join("bin");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&fake_bin).expect("fake bin dir");
    std::fs::create_dir_all(&home).expect("home dir");

    let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
    let _home_guard = EnvVarGuard::set("HOME", &home);
    let _data_guard =
        EnvVarGuard::set(crate::config::USER_DATA_DIR_ENV, dir.path().join("profile"));
    let _path_guard = EnvVarGuard::set("PATH", &fake_bin);
    let spec = DaemonServiceSpec {
        tracedecay_bin: PathBuf::from("/opt/tracedecay/bin/tracedecay"),
        socket_path: PathBuf::from("/run/user/1000/tracedecay.sock"),
        data_dir_override: None,
    };

    let service_path = config_home
        .join("systemd/user")
        .join(crate::daemon::SERVICE_NAME);
    let outcome = super::refresh_installed_service(&spec).expect("refresh service");

    assert_eq!(outcome, None);
    assert!(!service_path.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn post_update_rejects_reachable_unmanaged_daemon() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("temp dir");
    let data_dir = dir.path().join("profile");
    let config_home = dir.path().join("config");
    std::fs::create_dir_all(&data_dir).expect("data dir");
    let _data_guard = EnvVarGuard::set(crate::config::USER_DATA_DIR_ENV, &data_dir);
    let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
    let _socket_guard = EnvVarGuard::unset(crate::daemon::SOCKET_ENV);
    let socket_path = super::default_socket_path().expect("default socket");
    let _listener = std::os::unix::net::UnixListener::bind(&socket_path).expect("bind socket");

    let error = super::quiesce_installed_service_before_lease()
        .expect_err("unmanaged daemon must block post-update mutations");

    assert!(error.to_string().contains("unmanaged daemon"));
    assert!(error.to_string().contains("stop"));
}

#[cfg(target_os = "linux")]
#[test]
fn refresh_installed_service_preserves_existing_socket_path() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("temp dir");
    let config_home = dir.path().join("config");
    let fake_bin = dir.path().join("bin");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&fake_bin).expect("fake bin dir");
    std::fs::create_dir_all(&home).expect("home dir");

    let systemctl = fake_bin.join("systemctl");
    let log = dir.path().join("systemctl.log");
    let stopped = dir.path().join("systemctl.stopped");
    std::fs::write(
            &systemctl,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TRACEDECAY_SYSTEMCTL_LOG\"\n[ \"$2\" = is-enabled ] && echo enabled\n[ \"$2\" = is-active ] && [ -f \"$TRACEDECAY_SYSTEMCTL_STOPPED\" ] && exit 3\n[ \"$2\" = stop ] && touch \"$TRACEDECAY_SYSTEMCTL_STOPPED\"\n[ \"$2\" = start ] && rm -f \"$TRACEDECAY_SYSTEMCTL_STOPPED\"\nexit 0\n",
        )
        .expect("fake systemctl");
    std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755))
        .expect("systemctl permissions");

    let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
    let _home_guard = EnvVarGuard::set("HOME", &home);
    let _data_guard =
        EnvVarGuard::set(crate::config::USER_DATA_DIR_ENV, dir.path().join("profile"));
    let _path_guard = EnvVarGuard::set("PATH", &fake_bin);
    let _log_guard = EnvVarGuard::set("TRACEDECAY_SYSTEMCTL_LOG", &log);
    let _stopped_guard = EnvVarGuard::set("TRACEDECAY_SYSTEMCTL_STOPPED", &stopped);

    let service_path = config_home
        .join("systemd/user")
        .join(crate::daemon::SERVICE_NAME);
    std::fs::create_dir_all(service_path.parent().expect("service parent")).expect("service dir");
    std::fs::write(
        &service_path,
        "[Unit]\n\
             Description=TraceDecay daemon\n\
             \n\
             [Service]\n\
             ExecStart=/old/tracedecay daemon run --socket /custom/tracedecay.sock\n",
    )
    .expect("existing service unit");

    let spec = DaemonServiceSpec {
        tracedecay_bin: PathBuf::from("/opt/tracedecay/bin/tracedecay"),
        socket_path: PathBuf::from("/run/user/1000/tracedecay.sock"),
        data_dir_override: None,
    };

    let previous_state =
        super::quiesce_installed_service_before_lease().expect("quiesce installed service");
    let outcome = super::refresh_installed_service_under_lease_with_state(
        &spec,
        DaemonServiceState::StoppedEnabled,
    )
    .expect("refresh service");
    super::restore_installed_service_after_update(previous_state).expect("restore service state");

    assert_eq!(outcome, Some(service_path.clone()));
    let unit = std::fs::read_to_string(service_path).expect("service unit");
    assert!(unit.contains(
        "ExecStart=/opt/tracedecay/bin/tracedecay daemon run --socket /custom/tracedecay.sock"
    ));
    assert!(!unit.contains("/run/user/1000/tracedecay.sock"));
    assert_eq!(
        std::fs::read_to_string(log).expect("systemctl log"),
        "--user is-active --quiet tracedecay.service\n--user is-enabled tracedecay.service\n--user stop tracedecay.service\n--user daemon-reload\n--user enable tracedecay.service\n--user daemon-reload\n--user enable tracedecay.service\n--user start tracedecay.service\n"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn restore_quiesced_service_starts_existing_unit_without_rewriting_it() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("temp dir");
    let config_home = dir.path().join("config");
    let fake_bin = dir.path().join("bin");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&fake_bin).expect("fake bin dir");
    std::fs::create_dir_all(&home).expect("home dir");

    let systemctl = fake_bin.join("systemctl");
    let log = dir.path().join("systemctl.log");
    std::fs::write(
        &systemctl,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TRACEDECAY_SYSTEMCTL_LOG\"\nexit 0\n",
    )
    .expect("fake systemctl");
    std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755))
        .expect("systemctl permissions");

    let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
    let _home_guard = EnvVarGuard::set("HOME", &home);
    let _data_guard =
        EnvVarGuard::set(crate::config::USER_DATA_DIR_ENV, dir.path().join("profile"));
    let _path_guard = EnvVarGuard::set("PATH", &fake_bin);
    let _log_guard = EnvVarGuard::set("TRACEDECAY_SYSTEMCTL_LOG", &log);
    let service_path = config_home
        .join("systemd/user")
        .join(crate::daemon::SERVICE_NAME);
    std::fs::create_dir_all(service_path.parent().expect("service parent")).expect("service dir");
    let original_unit =
        "[Service]\nExecStart=/old/tracedecay daemon run --socket /custom/tracedecay.sock\n";
    std::fs::write(&service_path, original_unit).expect("existing service unit");

    super::restore_installed_service_after_update(DaemonServiceState::RunningEnabled)
        .expect("restore service");

    assert_eq!(
        std::fs::read_to_string(service_path).expect("service unit"),
        original_unit
    );
    assert_eq!(
        std::fs::read_to_string(log).expect("systemctl log"),
        "--user daemon-reload\n--user enable tracedecay.service\n--user start tracedecay.service\n"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn refresh_installed_service_preserves_stopped_state() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("temp dir");
    let config_home = dir.path().join("config");
    let fake_bin = dir.path().join("bin");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&fake_bin).expect("fake bin dir");
    std::fs::create_dir_all(&home).expect("home dir");
    let systemctl = fake_bin.join("systemctl");
    let log = dir.path().join("systemctl.log");
    std::fs::write(
            &systemctl,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TRACEDECAY_SYSTEMCTL_LOG\"\n[ \"$2\" = is-active ] && exit 3\nexit 0\n",
        )
        .expect("fake systemctl");
    std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755))
        .expect("systemctl permissions");
    let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
    let _home_guard = EnvVarGuard::set("HOME", &home);
    let _data_guard =
        EnvVarGuard::set(crate::config::USER_DATA_DIR_ENV, dir.path().join("profile"));
    let _path_guard = EnvVarGuard::set("PATH", &fake_bin);
    let _log_guard = EnvVarGuard::set("TRACEDECAY_SYSTEMCTL_LOG", &log);
    let service_path = config_home
        .join("systemd/user")
        .join(crate::daemon::SERVICE_NAME);
    std::fs::create_dir_all(service_path.parent().expect("service parent")).expect("service dir");
    std::fs::write(
        &service_path,
        "[Service]\nExecStart=/old/tracedecay daemon run --socket /custom/tracedecay.sock\n",
    )
    .expect("existing service unit");
    let spec = DaemonServiceSpec {
        tracedecay_bin: PathBuf::from("/opt/tracedecay/bin/tracedecay"),
        socket_path: PathBuf::from("/run/user/1000/tracedecay.sock"),
        data_dir_override: None,
    };

    super::refresh_installed_service(&spec).expect("refresh service");

    let commands = std::fs::read_to_string(log).expect("systemctl log");
    assert!(commands.contains("--user is-active --quiet tracedecay.service"));
    assert!(!commands.contains("enable tracedecay.service"));
    assert!(!commands.contains("restart tracedecay.service"));
}

#[cfg(target_os = "linux")]
#[test]
fn systemd_service_state_detects_runtime_mask() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("temp dir");
    let fake_bin = dir.path().join("bin");
    std::fs::create_dir_all(&fake_bin).expect("fake bin dir");
    let systemctl = fake_bin.join("systemctl");
    std::fs::write(
            &systemctl,
            "#!/bin/sh\n[ \"$2\" = is-active ] && exit 3\n[ \"$2\" = is-enabled ] && { echo masked-runtime; exit 1; }\nexit 0\n",
        )
        .expect("fake systemctl");
    std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755))
        .expect("systemctl permissions");
    let _path_guard = EnvVarGuard::set("PATH", &fake_bin);

    assert_eq!(
        ServiceRunner::Systemd
            .service_state(&dir.path().join("daemon.sock"))
            .expect("systemd service state"),
        DaemonServiceState::Masked
    );
}

#[cfg(target_os = "linux")]
#[test]
fn refresh_preserves_persistent_systemd_mask_symlink() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("temp dir");
    let config_home = dir.path().join("config");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home dir");
    let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
    let _home_guard = EnvVarGuard::set("HOME", &home);
    let service_path = config_home
        .join("systemd/user")
        .join(crate::daemon::SERVICE_NAME);
    std::fs::create_dir_all(service_path.parent().expect("service parent")).expect("service dir");
    std::os::unix::fs::symlink("/dev/null", &service_path).expect("mask service");
    let spec = DaemonServiceSpec {
        tracedecay_bin: PathBuf::from("/opt/tracedecay/bin/tracedecay"),
        socket_path: PathBuf::from("/run/user/1000/tracedecay.sock"),
        data_dir_override: None,
    };

    let error =
        super::refresh_installed_service_under_lease_with_state(&spec, DaemonServiceState::Masked)
            .expect_err("persistent mask must not be overwritten");

    assert!(error.to_string().contains("persistently masked"));
    assert_eq!(
        std::fs::read_link(service_path).unwrap(),
        PathBuf::from("/dev/null")
    );
}

#[test]
fn default_socket_path_is_profile_scoped_not_project_scoped() {
    let _env_lock = lock_user_data_dir_test_env();
    let profile = tempfile::TempDir::new().expect("profile temp dir");
    let project_a = tempfile::TempDir::new().expect("project a temp dir");
    let project_b = tempfile::TempDir::new().expect("project b temp dir");
    let override_socket = profile.path().join("override.sock");
    let _socket_guard = EnvVarGuard::unset(crate::daemon::SOCKET_ENV);
    let _data_dir_guard = EnvVarGuard::set(
        crate::config::USER_DATA_DIR_ENV,
        profile.path().join(".tracedecay"),
    );
    let expected_socket = crate::config::user_data_dir()
        .expect("user data dir")
        .join("daemon.sock");

    {
        let _cwd_guard = CurrentDirGuard::set(project_a.path());
        assert_eq!(
            super::default_socket_path().expect("default socket path"),
            expected_socket
        );
    }
    {
        let _cwd_guard = CurrentDirGuard::set(project_b.path());
        assert_eq!(
            super::default_socket_path().expect("default socket path"),
            expected_socket
        );
    }

    let _override_guard = EnvVarGuard::set(crate::daemon::SOCKET_ENV, &override_socket);
    assert_eq!(
        super::default_socket_path().expect("override socket path"),
        override_socket
    );
}
