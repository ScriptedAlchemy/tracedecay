use std::ffi::{OsStr, OsString};
#[cfg(target_os = "linux")]
use std::path::Path;
use std::path::PathBuf;

#[cfg(unix)]
use std::io::{BufRead, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::net::UnixListener;
#[cfg(target_os = "linux")]
use std::process::Command;
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::TempDir;

use super::runner::ServiceRunner;
use super::runner::{LaunchctlFailureMode, LaunchdCommand};
use super::{DaemonServiceSpec, DaemonServiceState};
use tracedecay_daemon_protocol::SOCKET_ENV;
use tracedecay_runtime_core::config::{
    USER_DATA_DIR_ENV, lock_user_data_dir_test_env, user_data_dir,
};

const TEST_BUILD_VERSION: &str = "0.1.0-test+service-probe";

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

#[cfg(target_os = "linux")]
fn systemctl_log_contains_sequence(log: &str, expected: &[&str]) -> bool {
    let mut lines = log.lines();
    for command in expected {
        loop {
            match lines.next() {
                Some(line) if line == *command => break,
                Some(_) => {}
                None => return false,
            }
        }
    }
    true
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

#[test]
fn released_windows_replacement_lease_is_reacquired_shared_before_restore() {
    let profile = TempDir::new().expect("profile");
    let mut guard = super::QuiescedDaemonLifecycle {
        previous_state: DaemonServiceState::RunningEnabled,
        lifecycle_lease: None,
        expected_version: TEST_BUILD_VERSION.to_owned(),
        runner: ServiceRunner::WindowsTask,
        restored: false,
    };

    guard
        .downgrade_to_shared_with(|| {
            tracedecay_runtime_core::lifecycle_lease::acquire_shared_for_profile(
                profile.path(),
                "replacement restore regression",
            )
        })
        .expect("reacquire shared restore lease");

    assert!(
        guard
            .lifecycle_lease
            .as_ref()
            .is_some_and(|lease| !lease.is_exclusive())
    );
    guard.restored = true;
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
    let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, profile.path());

    let status = super::service_status(&PathBuf::from("/tmp/tracedecay.sock"));
    let expected_log = user_data_dir()
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
fn after_update_restore_preserves_every_captured_service_state() {
    assert_eq!(
        DaemonServiceState::StoppedEnabled.expected_after_update(),
        DaemonServiceState::StoppedEnabled
    );
    assert_eq!(
        DaemonServiceState::StoppedDisabled.expected_after_update(),
        DaemonServiceState::StoppedDisabled
    );
    assert_eq!(
        DaemonServiceState::RunningEnabled.expected_after_update(),
        DaemonServiceState::RunningEnabled
    );
    assert_eq!(
        DaemonServiceState::RunningDisabled.expected_after_update(),
        DaemonServiceState::RunningDisabled
    );
    assert_eq!(
        DaemonServiceState::Missing.expected_after_update(),
        DaemonServiceState::Missing
    );
    assert_eq!(
        DaemonServiceState::Masked.expected_after_update(),
        DaemonServiceState::Masked
    );
}

#[test]
fn unavailable_socket_advice_keeps_stopped_disabled_lifecycle_operator_owned() {
    let socket = PathBuf::from("/tmp/tracedecay.sock");
    let advice =
        super::unavailable_daemon_socket_advice(&socket, Some(DaemonServiceState::StoppedDisabled));
    assert!(
        advice.contains("stopped and disabled"),
        "advice must distinguish stopped+disabled, got: {advice}"
    );
    assert!(
        advice.contains("may be intentionally held"),
        "advice must preserve operator intent, got: {advice}"
    );
    assert!(
        advice.contains("tracedecay daemon start"),
        "advice may name the explicit lifecycle command, got: {advice}"
    );
    assert!(
        !advice.contains("enable --now") && !advice.contains("install-service"),
        "an installed held service must not be rewritten or enabled by diagnostic advice, got: {advice}"
    );
}

#[cfg(unix)]
#[test]
fn unavailable_socket_advice_without_state_uses_unit_file_presence() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("temp dir");
    let config_home = dir.path().join("config");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home dir");
    let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
    let _home_guard = EnvVarGuard::set("HOME", &home);
    let socket = PathBuf::from("/tmp/tracedecay.sock");

    let missing = super::unavailable_daemon_socket_advice(&socket, None);
    assert!(
        missing.contains("install-service"),
        "absent unit keeps install-service, got: {missing}"
    );
    assert!(
        missing.contains("only if you want a managed daemon"),
        "absent-unit advice must leave installation intentional, got: {missing}"
    );

    let service_path = super::service_unit_path().expect("service unit path");
    std::fs::create_dir_all(service_path.parent().expect("service parent")).expect("service dir");
    std::fs::write(
        &service_path,
        "[Service]\nExecStart=/opt/tracedecay daemon run\n",
    )
    .expect("unit file");

    let installed = super::unavailable_daemon_socket_advice(&socket, None);
    assert!(
        installed.contains("unit is installed"),
        "present unit must be named, got: {installed}"
    );
    assert!(
        installed.contains("may be intentionally held"),
        "present unit must preserve operator intent, got: {installed}"
    );
    assert!(
        installed.contains("tracedecay daemon start")
            && !installed.contains("enable --now")
            && !installed.contains("install-service"),
        "present-unit advice must offer only explicit lifecycle control, got: {installed}"
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
            let preface = tracedecay_daemon_protocol::DaemonAuthPreface::from_line(line.trim())
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
fn serve_counted_authenticated_probe(
    listener: UnixListener,
    expected_auth_token: String,
) -> (std::thread::JoinHandle<()>, Arc<AtomicUsize>) {
    let accepts = Arc::new(AtomicUsize::new(0));
    let server_accepts = Arc::clone(&accepts);
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept readiness probe");
        server_accepts.fetch_add(1, Ordering::SeqCst);
        let mut reader =
            std::io::BufReader::new(stream.try_clone().expect("clone readiness stream"));
        let mut line = String::new();
        reader.read_line(&mut line).expect("read auth preface");
        let preface = tracedecay_daemon_protocol::DaemonAuthPreface::from_line(line.trim())
            .expect("authenticated readiness preface");
        assert!(preface.authenticate(&expected_auth_token));
        line.clear();
        reader.read_line(&mut line).expect("read handshake");
        line.clear();
        reader.read_line(&mut line).expect("read initialize");
        let request: serde_json::Value =
            serde_json::from_str(line.trim()).expect("initialize json");
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": request["id"],
            "result": {
                "serverInfo": {"name": "tracedecay", "version": TEST_BUILD_VERSION}
            }
        });
        writeln!(stream, "{response}").expect("write initialize response");
        listener
            .set_nonblocking(true)
            .expect("nonblocking accept audit");
        let audit_deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
        while std::time::Instant::now() < audit_deadline {
            match listener.accept() {
                Ok((_stream, _)) => {
                    server_accepts.fetch_add(1, Ordering::SeqCst);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(error) => panic!("readiness accept audit failed: {error}"),
            }
        }
    });
    (server, accepts)
}

/// Serves the managed-daemon identity probe for every connection accepted on
/// `listener`, answering `versions[n]` on the n-th completed identity
/// exchange (the last entry repeats once the list is exhausted). Every
/// readiness connection must carry its authenticated initialize request.
/// Returns the count of identity responses served, which lets tests prove the
/// readiness wait actually consulted the daemon.
#[cfg(target_os = "linux")]
fn serve_identity_probes(listener: UnixListener, versions: Vec<&'static str>) -> Arc<AtomicUsize> {
    let served = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&served);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                break;
            };
            let Ok(clone) = stream.try_clone() else {
                continue;
            };
            let mut reader = std::io::BufReader::new(clone);
            let mut line = String::new();
            assert_ne!(
                reader
                    .read_line(&mut line)
                    .expect("read readiness handshake"),
                0,
                "readiness connection closed without a handshake"
            );
            line.clear();
            assert_ne!(
                reader
                    .read_line(&mut line)
                    .expect("read readiness initialize"),
                0,
                "readiness connection closed without initialize"
            );
            let Ok(request) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                continue;
            };
            let index = count.load(Ordering::SeqCst).min(versions.len() - 1);
            let response = serde_json::json!({
                "jsonrpc": "2.0",
                "id": request["id"],
                "result": {
                    "serverInfo": {"name": "tracedecay", "version": versions[index]}
                }
            });
            if writeln!(stream, "{response}").is_ok() {
                count.fetch_add(1, Ordering::SeqCst);
            }
        }
    });
    served
}

#[cfg(unix)]
#[test]
fn daemon_protocol_probe_requires_current_tracedecay_identity() {
    let _env_lock = lock_user_data_dir_test_env();
    let profile = TempDir::new().expect("profile temp dir");
    let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, profile.path());

    let ready_socket = profile.path().join("ready.sock");
    let ready_listener = UnixListener::bind(&ready_socket).expect("bind ready socket");
    let ready_server = serve_probe_response(ready_listener, "tracedecay", TEST_BUILD_VERSION, None);
    assert_eq!(
        super::probe::daemon_readiness_probe(
            &ready_socket,
            TEST_BUILD_VERSION,
            std::time::Duration::from_secs(10),
        )
        .1,
        super::probe::DaemonProtocolState::Ready
    );
    ready_server.join().expect("join ready server");

    let stale_socket = profile.path().join("stale.sock");
    let stale_listener = UnixListener::bind(&stale_socket).expect("bind stale socket");
    let stale_server = serve_probe_response(stale_listener, "tracedecay", "0.0.0-stale", None);
    assert_eq!(
        super::probe::daemon_readiness_probe(
            &stale_socket,
            TEST_BUILD_VERSION,
            std::time::Duration::from_secs(10),
        )
        .1,
        super::probe::DaemonProtocolState::IdentityMismatch {
            name: Some("tracedecay".to_string()),
            version: Some("0.0.0-stale".to_string()),
            expected_version: TEST_BUILD_VERSION.to_string(),
        }
    );
    stale_server.join().expect("join stale server");
}

#[cfg(unix)]
#[test]
fn daemon_protocol_probe_authenticates_to_managed_daemon() {
    let _env_lock = lock_user_data_dir_test_env();
    let profile = TempDir::new().expect("profile temp dir");
    let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, profile.path());
    let socket_path = profile.path().join("daemon.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind managed daemon socket");
    let endpoint = tracedecay_daemon_protocol::DaemonEndpoint::Unix(socket_path.clone());
    let authority = tracedecay_daemon_identity::authority::DaemonAuthority::acquire(
        profile.path(),
        &endpoint,
        env!("CARGO_PKG_VERSION"),
    )
    .expect("publish daemon authority");
    let server = serve_probe_response(
        listener,
        "tracedecay",
        TEST_BUILD_VERSION,
        Some(authority.auth_token().to_string()),
    );

    assert_eq!(
        super::probe::daemon_readiness_probe(
            &socket_path,
            TEST_BUILD_VERSION,
            std::time::Duration::from_secs(10),
        )
        .1,
        super::probe::DaemonProtocolState::Ready
    );
    server.join().expect("join authenticated probe server");
}

#[cfg(unix)]
#[test]
fn daemon_readiness_probe_uses_one_authenticated_connection() {
    let _env_lock = lock_user_data_dir_test_env();
    let profile = TempDir::new().expect("profile temp dir");
    let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, profile.path());
    let socket_path = profile.path().join("daemon.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind readiness socket");
    let endpoint = tracedecay_daemon_protocol::DaemonEndpoint::Unix(socket_path.clone());
    let authority = tracedecay_daemon_identity::authority::DaemonAuthority::acquire(
        profile.path(),
        &endpoint,
        TEST_BUILD_VERSION,
    )
    .expect("publish daemon authority");
    let (server, accepts) =
        serve_counted_authenticated_probe(listener, authority.auth_token().to_owned());

    assert_eq!(
        super::probe::daemon_readiness_probe(
            &socket_path,
            TEST_BUILD_VERSION,
            std::time::Duration::from_secs(1),
        ),
        (
            super::probe::DaemonSocketState::Connectable,
            super::probe::DaemonProtocolState::Ready,
        )
    );
    server.join().expect("join readiness server");
    assert_eq!(accepts.load(Ordering::SeqCst), 1);
}

#[cfg(unix)]
#[test]
fn daemon_readiness_probe_classifies_connect_and_protocol_failures() {
    let _env_lock = lock_user_data_dir_test_env();
    let profile = TempDir::new().expect("profile temp dir");
    let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, profile.path());
    let missing_socket = profile.path().join("missing.sock");
    let missing = super::probe::daemon_readiness_probe(
        &missing_socket,
        TEST_BUILD_VERSION,
        std::time::Duration::from_millis(50),
    );
    assert_eq!(missing.0, super::probe::DaemonSocketState::Missing);
    assert!(matches!(
        missing.1,
        super::probe::DaemonProtocolState::Unresponsive(_)
    ));

    let stale_socket = profile.path().join("stale.sock");
    drop(UnixListener::bind(&stale_socket).expect("bind stale socket"));
    let stale = super::probe::daemon_readiness_probe(
        &stale_socket,
        TEST_BUILD_VERSION,
        std::time::Duration::from_millis(50),
    );
    assert_eq!(stale.0, super::probe::DaemonSocketState::Stale);
    assert!(matches!(
        stale.1,
        super::probe::DaemonProtocolState::Unresponsive(_)
    ));

    let connectable_socket = profile.path().join("connectable.sock");
    let _listener = UnixListener::bind(&connectable_socket).expect("bind connectable socket");
    let connectable = super::probe::daemon_readiness_probe(
        &connectable_socket,
        TEST_BUILD_VERSION,
        std::time::Duration::from_millis(20),
    );
    assert!(matches!(
        connectable,
        (
            super::probe::DaemonSocketState::Connectable,
            super::probe::DaemonProtocolState::Unresponsive(_),
        )
    ));
}

#[cfg(unix)]
#[test]
fn daemon_readiness_probe_classifies_authentication_denial() {
    let _env_lock = lock_user_data_dir_test_env();
    let profile = TempDir::new().expect("profile temp dir");
    let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, profile.path());
    let socket_path = profile.path().join("daemon.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind readiness socket");
    let endpoint = tracedecay_daemon_protocol::DaemonEndpoint::Unix(socket_path.clone());
    let authority = tracedecay_daemon_identity::authority::DaemonAuthority::acquire(
        profile.path(),
        &endpoint,
        TEST_BUILD_VERSION,
    )
    .expect("publish daemon authority");
    let expected_auth_token = authority.auth_token().to_owned();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept readiness probe");
        let mut reader =
            std::io::BufReader::new(stream.try_clone().expect("clone readiness stream"));
        let mut line = String::new();
        reader.read_line(&mut line).expect("read auth preface");
        let preface = tracedecay_daemon_protocol::DaemonAuthPreface::from_line(line.trim())
            .expect("authenticated readiness preface");
        assert!(preface.authenticate(&expected_auth_token));
        line.clear();
        reader.read_line(&mut line).expect("read handshake");
        line.clear();
        reader.read_line(&mut line).expect("read initialize");
        let request: serde_json::Value =
            serde_json::from_str(line.trim()).expect("initialize json");
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": request["id"],
            "error": {"code": -32001, "message": "authentication denied"}
        });
        writeln!(stream, "{response}").expect("write denial");
    });

    let readiness = super::probe::daemon_readiness_probe(
        &socket_path,
        TEST_BUILD_VERSION,
        std::time::Duration::from_secs(1),
    );
    server.join().expect("join denial server");
    assert_eq!(readiness.0, super::probe::DaemonSocketState::Connectable);
    let super::probe::DaemonProtocolState::Unresponsive(detail) = readiness.1 else {
        panic!("authentication denial must be unresponsive");
    };
    assert!(detail.contains("authentication denied"), "{detail}");
}

#[cfg(target_os = "linux")]
#[test]
fn running_service_snapshot_uses_one_authenticated_connection() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("temp dir");
    let config_home = dir.path().join("config");
    let home = dir.path().join("home");
    let fake_bin = dir.path().join("bin");
    std::fs::create_dir_all(&home).expect("home dir");
    std::fs::create_dir_all(&fake_bin).expect("fake bin dir");
    let systemctl = fake_bin.join("systemctl");
    std::fs::write(
        &systemctl,
        "#!/bin/sh\n[ \"$2\" = is-active ] && echo active\n[ \"$2\" = is-enabled ] && echo enabled\nexit 0\n",
    )
    .expect("fake systemctl");
    std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755))
        .expect("systemctl permissions");
    let runner = ServiceRunner::systemd(&systemctl).expect("fixture systemd runner");
    let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
    let _home_guard = EnvVarGuard::set("HOME", &home);
    let _data_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, dir.path().join("profile"));
    let service_path = config_home.join("systemd/user").join(crate::SERVICE_NAME);
    std::fs::create_dir_all(service_path.parent().expect("service parent")).expect("service dir");
    let socket_path = dir.path().join("daemon.sock");
    std::fs::write(
        &service_path,
        format!(
            "[Service]\nExecStart=/old/tracedecay daemon run --socket {}\n",
            socket_path.display()
        ),
    )
    .expect("service unit");
    let listener = UnixListener::bind(&socket_path).expect("bind readiness socket");
    let endpoint = tracedecay_daemon_protocol::DaemonEndpoint::Unix(socket_path.clone());
    let authority = tracedecay_daemon_identity::authority::DaemonAuthority::acquire(
        dir.path(),
        &endpoint,
        TEST_BUILD_VERSION,
    )
    .expect("publish daemon authority");
    let (server, accepts) =
        serve_counted_authenticated_probe(listener, authority.auth_token().to_owned());

    let snapshot = super::installed_service_status_snapshot(&runner, TEST_BUILD_VERSION)
        .expect("running service snapshot");
    server.join().expect("join readiness server");

    assert_eq!(snapshot.0, DaemonServiceState::RunningEnabled);
    assert_eq!(snapshot.2, super::probe::DaemonSocketState::Connectable);
    assert_eq!(snapshot.3, super::probe::DaemonProtocolState::Ready);
    assert_eq!(accepts.load(Ordering::SeqCst), 1);
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
        remote_tls: None,
    };

    let unit = spec.render_systemd_user_unit().expect("systemd unit");

    assert!(
        unit.contains(
            "ExecStart=/usr/local/bin/tracedecay daemon run --socket /tmp/tracedecay.sock"
        )
    );
    assert!(unit.contains("Environment=\"PATH="));
    assert!(unit.contains("Environment=\"MALLOC_ARENA_MAX=2\""));
    assert!(unit.contains("Restart=always"));
    assert!(unit.contains("RestartSec=2"));
    assert!(unit.contains("StartLimitIntervalSec=0"));
    assert!(!unit.contains("Restart=on-failure"));
    assert!(unit.contains("LimitNOFILE=8192"));
}

#[test]
fn systemd_unit_quotes_exec_start_paths_that_systemd_would_misparse() {
    let spec = DaemonServiceSpec {
        tracedecay_bin: PathBuf::from("/opt/trace decay/bin/tracedecay"),
        socket_path: PathBuf::from("/run/user/1000/trace decay%50.sock"),
        data_dir_override: None,
        remote_tls: None,
    };

    let unit = spec.render_systemd_user_unit().expect("systemd unit");

    assert!(
        unit.contains(
            "ExecStart=\"/opt/trace decay/bin/tracedecay\" daemon run --socket \"/run/user/1000/trace decay%%50.sock\""
        ),
        "paths with whitespace or specifier characters must be quoted and escaped, got:\n{unit}"
    );
}

#[test]
fn systemd_socket_read_back_round_trips_quoted_and_bare_exec_start_paths() {
    let quoted_socket = PathBuf::from("/run/user/1000/trace decay%50.sock");
    let quoted_spec = DaemonServiceSpec {
        tracedecay_bin: PathBuf::from("/opt/trace decay/bin/tracedecay"),
        socket_path: quoted_socket.clone(),
        data_dir_override: None,
        remote_tls: None,
    };
    let quoted_unit = quoted_spec
        .render_systemd_user_unit()
        .expect("systemd unit");
    assert_eq!(
        super::unit_file::socket_path_from_service_unit(&quoted_unit),
        Some(quoted_socket),
        "socket read-back must round-trip exactly the path the renderer quoted and escaped"
    );

    let bare_socket = PathBuf::from("/run/user/1000/tracedecay.sock");
    let bare_spec = DaemonServiceSpec {
        tracedecay_bin: PathBuf::from("/usr/local/bin/tracedecay"),
        socket_path: bare_socket.clone(),
        data_dir_override: None,
        remote_tls: None,
    };
    let bare_unit = bare_spec.render_systemd_user_unit().expect("systemd unit");
    assert!(
        bare_unit.contains("--socket /run/user/1000/tracedecay.sock"),
        "ordinary paths must stay bare for byte-identity with previously installed units"
    );
    assert_eq!(
        super::unit_file::socket_path_from_service_unit(&bare_unit),
        Some(bare_socket),
        "socket read-back must round-trip bare unquoted paths unchanged"
    );
}

#[test]
fn systemd_socket_read_back_returns_none_for_unterminated_exec_start_quote() {
    let unit = "[Service]\nExecStart=/usr/bin/tracedecay daemon run --socket \"/run/unterminated\n";

    assert_eq!(
        super::unit_file::socket_path_from_service_unit(unit),
        None,
        "an ExecStart line the tokenizer rejects must fall back to None, not garbage"
    );
}

fn remote_tls_config(
    listen: &str,
    certificate_chain: impl Into<PathBuf>,
    private_key: impl Into<PathBuf>,
) -> crate::RemoteBrainTlsConfig {
    crate::RemoteBrainTlsConfig::from_optional_parts(
        Some(listen.parse().expect("listener address")),
        Some(certificate_chain.into()),
        Some(private_key.into()),
    )
    .expect("valid Remote Brain TLS service configuration")
    .expect("enabled Remote Brain TLS service configuration")
}

#[test]
fn systemd_service_round_trips_remote_tls_listener_paths() {
    let fixture = TempDir::new().expect("Remote Brain TLS fixture");
    let tls_root = fixture.path().join("trace decay");
    std::fs::create_dir_all(&tls_root).expect("Remote Brain TLS fixture directory");
    let certificate_chain = tls_root.join("server%$ chain-part.pem");
    let private_key = tls_root.join("server%$ key.pem");
    std::fs::write(&certificate_chain, b"certificate chain").expect("certificate fixture");
    std::fs::write(&private_key, b"private key").expect("private key fixture");
    let remote_tls = remote_tls_config("192.0.2.10:7443", certificate_chain, private_key);
    let spec = DaemonServiceSpec {
        tracedecay_bin: PathBuf::from("/usr/local/bin/tracedecay"),
        socket_path: PathBuf::from("/tmp/tracedecay.sock"),
        data_dir_override: None,
        remote_tls: Some(remote_tls.clone()),
    };

    let unit = spec.render_systemd_user_unit().expect("systemd unit");

    assert_eq!(
        super::unit_file::remote_tls_from_service_unit(&unit)
            .expect("parse systemd Remote Brain arguments"),
        Some(remote_tls)
    );
    assert!(!unit.contains("PRIVATE KEY"));
}

#[test]
fn partial_systemd_remote_tls_arguments_fail_closed() {
    let unit = "ExecStart=/usr/bin/tracedecay daemon run --remote-listen 192.0.2.10:7443";

    assert!(super::unit_file::remote_tls_from_service_unit(unit).is_err());
}

#[test]
fn managed_service_rejects_relative_remote_tls_paths() {
    let remote_tls = remote_tls_config("192.0.2.10:7443", "server.pem", "server-key.pem");

    let error =
        super::service_spec_with_remote_tls("/usr/local/bin/tracedecay", None, Some(remote_tls))
            .expect_err("relative TLS paths must not depend on a service working directory");

    assert!(error.to_string().contains("must be absolute"));
}

#[test]
fn managed_service_rejects_remote_tls_path_control_characters() {
    let fixture = TempDir::new().expect("Remote Brain TLS fixture");
    let certificate_chain = fixture.path().join("server.pem");
    let private_key = fixture.path().join("server-key.pem");
    std::fs::write(&certificate_chain, b"certificate chain").expect("certificate fixture");
    std::fs::write(&private_key, b"private key").expect("private key fixture");
    let mut injected_certificate = certificate_chain.into_os_string();
    injected_certificate.push("\nEnvironment=INJECTED");
    let remote_tls = remote_tls_config(
        "192.0.2.10:7443",
        PathBuf::from(injected_certificate),
        private_key,
    );

    let error = super::service_spec_with_remote_tls(
        fixture.path().join("tracedecay"),
        None,
        Some(remote_tls),
    )
    .expect_err("control characters must not enter a service definition");

    assert!(error.to_string().contains("control character"));
}

#[test]
fn duplicate_systemd_remote_tls_arguments_fail_closed() {
    let unit = "ExecStart=/usr/bin/tracedecay daemon run --remote-listen 192.0.2.10:7443 --remote-listen 192.0.2.11:7443 --remote-tls-cert /etc/server.pem --remote-tls-key /etc/server-key.pem";

    assert!(super::unit_file::remote_tls_from_service_unit(unit).is_err());
}

#[test]
fn parsed_systemd_remote_tls_paths_are_validated_before_refresh() {
    let relative = "ExecStart=/usr/bin/tracedecay daemon run --remote-listen 192.0.2.10:7443 --remote-tls-cert server.pem --remote-tls-key server-key.pem";
    let control = "ExecStart=/usr/bin/tracedecay daemon run --remote-listen 192.0.2.10:7443 --remote-tls-cert \"/etc/server.pem\tInjected\" --remote-tls-key /etc/server-key.pem";
    let ambiguous_escape = r#"ExecStart=/usr/bin/tracedecay daemon run --remote-listen 192.0.2.10:7443 --remote-tls-cert "/etc/server\tname.pem" --remote-tls-key /etc/server-key.pem"#;

    assert!(super::unit_file::remote_tls_from_service_unit(relative).is_err());
    assert!(super::unit_file::remote_tls_from_service_unit(control).is_err());
    assert!(super::unit_file::remote_tls_from_service_unit(ambiguous_escape).is_err());
}

#[cfg(unix)]
#[test]
fn managed_service_rejects_non_unicode_remote_tls_paths() {
    use std::os::unix::ffi::OsStringExt;

    let certificate = std::ffi::OsString::from_vec(vec![b'/', b'e', b't', b'c', b'/', 0xff]);
    let remote_tls = remote_tls_config(
        "192.0.2.10:7443",
        PathBuf::from(certificate),
        "/etc/server-key.pem",
    );

    let error =
        super::service_spec_with_remote_tls("/usr/local/bin/tracedecay", None, Some(remote_tls))
            .expect_err("non-Unicode TLS paths must not be rendered lossily");

    assert!(error.to_string().contains("valid Unicode"));
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
        remote_tls: None,
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
    assert!(plist.contains("<key>ProcessType</key>"));
    assert!(plist.contains("<string>Interactive</string>"));
    assert!(plist.contains("<key>SoftResourceLimits</key>"));
    assert!(plist.contains("<key>NumberOfFiles</key>"));
    assert!(plist.contains("<integer>8192</integer>"));
}

#[cfg(unix)]
#[test]
fn launchd_service_round_trips_remote_tls_listener_paths() {
    let _env_lock = lock_user_data_dir_test_env();
    let profile = tempfile::TempDir::new().expect("profile temp dir");
    let home = tempfile::TempDir::new().expect("home temp dir");
    let _home_guard = EnvVarGuard::set("HOME", home.path());
    let remote_tls = remote_tls_config(
        "192.0.2.10:7443",
        "/Library/Application Support/TraceDecay/server<&\"'chain.pem",
        "/Library/Application Support/TraceDecay/server>&key.pem",
    );
    let spec = DaemonServiceSpec {
        tracedecay_bin: PathBuf::from("/opt/tracedecay/bin/tracedecay"),
        socket_path: profile.path().join("daemon.sock"),
        data_dir_override: Some(profile.path().to_path_buf()),
        remote_tls: Some(remote_tls.clone()),
    };

    let plist = spec.render_launchd_plist().expect("launchd plist");

    assert_eq!(
        super::unit_file::remote_tls_from_launchd_plist(&plist)
            .expect("parse launchd Remote Brain arguments"),
        Some(remote_tls)
    );
    assert!(!plist.contains("PRIVATE KEY"));
}

#[cfg(unix)]
#[test]
fn parsed_launchd_remote_tls_paths_are_validated_before_refresh() {
    let plist = "<key>ProgramArguments</key><array><string>/usr/bin/tracedecay</string><string>daemon</string><string>run</string><string>--remote-listen</string><string>192.0.2.10:7443</string><string>--remote-tls-cert</string><string>server.pem</string><string>--remote-tls-key</string><string>/etc/server-key.pem</string></array>";

    assert!(super::unit_file::remote_tls_from_launchd_plist(plist).is_err());
}

#[cfg(unix)]
#[test]
fn render_launchd_plist_escapes_xml_and_parser_unescapes_socket_path() {
    let _env_lock = lock_user_data_dir_test_env();
    let profile = tempfile::TempDir::new().expect("profile temp dir");
    let home = tempfile::TempDir::new().expect("home temp dir");
    let _home_guard = EnvVarGuard::set("HOME", home.path());
    let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, profile.path());
    let socket_path = PathBuf::from("/tmp/trace<decay>&\"socket'.sock");
    let spec = DaemonServiceSpec {
        tracedecay_bin: PathBuf::from("/opt/trace&decay/bin/tracedecay"),
        socket_path: socket_path.clone(),
        data_dir_override: None,
        remote_tls: None,
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
        remote_tls: None,
    };

    let plist = spec.render_launchd_plist().expect("launchd plist");

    assert_eq!(
        super::unit_file::launchd_plist_env_value(&plist, USER_DATA_DIR_ENV),
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
    let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, profile.path());
    let spec = DaemonServiceSpec {
        tracedecay_bin: PathBuf::from("/opt/tracedecay/bin/tracedecay"),
        socket_path: profile.path().join("daemon.sock"),
        data_dir_override: None,
        remote_tls: None,
    };

    let plist = spec.render_launchd_plist().expect("launchd plist");

    assert_eq!(
        super::unit_file::launchd_plist_env_value(&plist, USER_DATA_DIR_ENV),
        None
    );
}

#[test]
fn systemd_install_and_start_plans_reload_written_units() {
    assert_eq!(
        super::runner::systemd_install_command_plan(false),
        vec![vec!["daemon-reload"]],
        "install --no-start must still make systemd re-read the freshly written unit"
    );
    assert_eq!(
        super::runner::systemd_install_command_plan(true),
        vec![
            vec!["daemon-reload"],
            vec!["enable", "--now", crate::SERVICE_NAME],
        ]
    );
    assert_eq!(
        super::runner::systemd_start_command_plan(),
        vec![vec!["daemon-reload"], vec!["start", crate::SERVICE_NAME]],
        "start must reload first so a unit rewritten since the last reload starts fresh"
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
                LaunchctlFailureMode::RetryTransientBootstrap
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
fn transient_bootstrap_matcher_covers_only_the_eio_race() {
    assert!(
        super::runner::launchctl_output_is_transient_bootstrap_failure(
            "Bootstrap failed: 5: Input/output error"
        )
    );
    // Non-transient bootstrap failures (bad plist, permissions, unknown
    // domain) must fail immediately instead of being retried.
    assert!(
        !super::runner::launchctl_output_is_transient_bootstrap_failure(
            "Bootstrap failed: 125: Domain does not support specified action"
        )
    );
    assert!(
        !super::runner::launchctl_output_is_transient_bootstrap_failure(
            "Boot-out failed: 5: Input/output error"
        )
    );
    assert!(!super::runner::launchctl_output_is_transient_bootstrap_failure(""));
}

#[cfg(unix)]
mod transient_bootstrap_retry {
    use std::cell::RefCell;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{ExitStatus, Output};
    use std::time::Duration;

    fn output(code: i32, stderr: &str) -> Output {
        Output {
            status: ExitStatus::from_raw(code << 8),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    fn eio() -> Output {
        output(5, "Bootstrap failed: 5: Input/output error")
    }

    /// The re-bootstrap race resolves once the booted-out job drains, so a
    /// transient EIO retries with backoff and the eventual success wins.
    #[test]
    fn retries_the_eio_race_until_bootstrap_succeeds() {
        let outputs = RefCell::new(vec![output(0, ""), eio(), eio()]);
        let sleeps = RefCell::new(Vec::new());

        super::super::runner::retry_transient_bootstrap(
            &["bootstrap", "gui/501", "plist"],
            || Ok(outputs.borrow_mut().pop().expect("scripted output")),
            |backoff| sleeps.borrow_mut().push(backoff),
        )
        .expect("transient bootstrap race must recover");

        assert!(
            outputs.borrow().is_empty(),
            "all scripted attempts consumed"
        );
        assert_eq!(
            sleeps.borrow().as_slice(),
            [Duration::from_millis(200), Duration::from_millis(400)],
            "backoff must grow between transient retries"
        );
    }

    /// Any other bootstrap failure is not the drain race and must propagate
    /// on the first attempt without sleeping.
    #[test]
    fn non_transient_bootstrap_failure_fails_immediately() {
        let mut attempts = 0;

        let error = super::super::runner::retry_transient_bootstrap(
            &["bootstrap", "gui/501", "plist"],
            || {
                attempts += 1;
                Ok(output(
                    125,
                    "Bootstrap failed: 125: Domain does not support specified action",
                ))
            },
            |_| panic!("non-transient failures must not back off"),
        )
        .expect_err("non-transient bootstrap failure must propagate");

        assert_eq!(attempts, 1);
        assert!(error.to_string().contains("Bootstrap failed: 125"));
    }

    /// The retry is bounded: a persistent EIO surfaces the real failure
    /// after the attempt budget instead of spinning forever.
    #[test]
    fn persistent_eio_fails_after_the_bounded_attempts() {
        let mut attempts = 0;
        let sleeps = RefCell::new(Vec::new());

        let error = super::super::runner::retry_transient_bootstrap(
            &["bootstrap", "gui/501", "plist"],
            || {
                attempts += 1;
                Ok(eio())
            },
            |backoff| sleeps.borrow_mut().push(backoff),
        )
        .expect_err("persistent EIO must eventually propagate");

        assert_eq!(attempts, 5);
        assert_eq!(
            sleeps.borrow().as_slice(),
            [
                Duration::from_millis(200),
                Duration::from_millis(400),
                Duration::from_millis(800),
                Duration::from_millis(1600),
            ],
            "backoff doubles and caps below the final failed attempt"
        );
        assert!(error.to_string().contains("Bootstrap failed: 5"));
    }
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

#[cfg(unix)]
fn assert_path_lookup_skips_non_executable_shadow(program: &str, lifecycle: &str) {
    let dir = TempDir::new().expect("temp dir");
    let shadow_dir = dir.path().join("shadow");
    let executable_dir = dir.path().join("executable");
    std::fs::create_dir_all(&shadow_dir).expect("shadow dir");
    std::fs::create_dir_all(&executable_dir).expect("executable dir");

    let shadow = shadow_dir.join(program);
    std::fs::write(&shadow, "#!/bin/sh\nexit 1\n").expect("shadow program");
    std::fs::set_permissions(&shadow, std::fs::Permissions::from_mode(0o644))
        .expect("shadow permissions");

    let executable = executable_dir.join(program);
    std::fs::write(&executable, "#!/bin/sh\nexit 0\n").expect("executable program");
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
        .expect("executable permissions");

    let path_var = std::env::join_paths([shadow_dir, executable_dir]).expect("fixture PATH");
    let resolved = super::runner::require_service_program_on_path(
        program,
        lifecycle,
        Some(path_var.as_os_str()),
    )
    .expect("later executable PATH candidate");

    assert_eq!(
        resolved,
        executable.canonicalize().expect("canonical executable")
    );
}

#[cfg(unix)]
#[test]
fn launchctl_path_lookup_skips_non_executable_shadow() {
    assert_path_lookup_skips_non_executable_shadow("launchctl", "launchd agent management");
}

#[cfg(unix)]
#[test]
fn id_path_lookup_skips_non_executable_shadow() {
    assert_path_lookup_skips_non_executable_shadow("id", "launchd user-domain resolution");
}

#[cfg(unix)]
#[test]
fn explicitly_injected_launchd_programs_reject_non_executable_paths() {
    let dir = TempDir::new().expect("temp dir");
    let launchctl = dir.path().join("launchctl");
    let id = dir.path().join("id");
    std::fs::write(&launchctl, "#!/bin/sh\nexit 0\n").expect("launchctl program");
    std::fs::write(&id, "#!/bin/sh\nexit 0\n").expect("id program");

    std::fs::set_permissions(&launchctl, std::fs::Permissions::from_mode(0o644))
        .expect("launchctl permissions");
    std::fs::set_permissions(&id, std::fs::Permissions::from_mode(0o755)).expect("id permissions");
    let launchctl_error = ServiceRunner::launchd(&launchctl, &id)
        .expect_err("explicit launchctl path must remain strict");
    assert!(launchctl_error.to_string().contains("launchctl candidate"));
    assert!(launchctl_error.to_string().contains("not executable"));

    std::fs::set_permissions(&launchctl, std::fs::Permissions::from_mode(0o755))
        .expect("launchctl permissions");
    std::fs::set_permissions(&id, std::fs::Permissions::from_mode(0o644)).expect("id permissions");
    let id_error =
        ServiceRunner::launchd(&launchctl, &id).expect_err("explicit id path must remain strict");
    assert!(id_error.to_string().contains("id candidate"));
    assert!(id_error.to_string().contains("not executable"));
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
                    Err(tracedecay_domain::errors::TraceDecayError::Config {
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
fn refresh_installed_service_skips_missing_unit() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("temp dir");
    let config_home = dir.path().join("config");
    let fake_bin = dir.path().join("bin");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&fake_bin).expect("fake bin dir");
    std::fs::create_dir_all(&home).expect("home dir");
    let systemctl = fake_bin.join("systemctl");
    std::fs::write(&systemctl, "#!/bin/sh\nexit 0\n").expect("fake systemctl");
    std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755))
        .expect("systemctl permissions");
    let runner = ServiceRunner::systemd(&systemctl).expect("fixture systemd runner");

    let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
    let _home_guard = EnvVarGuard::set("HOME", &home);
    let _data_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, dir.path().join("profile"));
    let spec = DaemonServiceSpec {
        tracedecay_bin: PathBuf::from("/opt/tracedecay/bin/tracedecay"),
        socket_path: PathBuf::from("/run/user/1000/tracedecay.sock"),
        data_dir_override: None,
        remote_tls: None,
    };

    let service_path = config_home.join("systemd/user").join(crate::SERVICE_NAME);
    let outcome = super::with_quiesced_installed_service_with_runner(
        runner,
        "daemon service refresh",
        TEST_BUILD_VERSION,
        |_, runner| {
            super::refresh_installed_service_with_state_and_runner(
                runner,
                &spec,
                None,
                TEST_BUILD_VERSION,
            )
        },
    )
    .expect("refresh service");

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
    let _data_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &data_dir);
    let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
    let _socket_guard = EnvVarGuard::unset(SOCKET_ENV);
    let socket_path = super::default_socket_path().expect("default socket");
    let _listener = std::os::unix::net::UnixListener::bind(&socket_path).expect("bind socket");

    let error = super::quiesce_installed_service_before_lease(TEST_BUILD_VERSION)
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
    let runner = ServiceRunner::systemd(&systemctl).expect("fixture systemd runner");

    let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
    let _home_guard = EnvVarGuard::set("HOME", &home);
    let _data_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, dir.path().join("profile"));
    let _log_guard = EnvVarGuard::set("TRACEDECAY_SYSTEMCTL_LOG", &log);
    let _stopped_guard = EnvVarGuard::set("TRACEDECAY_SYSTEMCTL_STOPPED", &stopped);

    let service_path = config_home.join("systemd/user").join(crate::SERVICE_NAME);
    std::fs::create_dir_all(service_path.parent().expect("service parent")).expect("service dir");
    let custom_socket = dir.path().join("custom-tracedecay.sock");
    std::fs::write(
        &service_path,
        format!(
            "[Unit]\n\
             Description=TraceDecay daemon\n\
             \n\
             [Service]\n\
             ExecStart=/old/tracedecay daemon run --socket {} --remote-listen 192.0.2.10:7443 --remote-tls-cert \"/etc/trace decay/server.pem\" --remote-tls-key \"/etc/trace decay/server-key.pem\"\n",
            custom_socket.display()
        ),
    )
    .expect("existing service unit");

    let spec = DaemonServiceSpec {
        tracedecay_bin: PathBuf::from("/opt/tracedecay/bin/tracedecay"),
        socket_path: PathBuf::from("/run/user/1000/tracedecay.sock"),
        data_dir_override: None,
        remote_tls: None,
    };

    let previous_state =
        super::quiesce_installed_service_before_lease_with_runner(&runner, TEST_BUILD_VERSION)
            .expect("quiesce installed service");
    let outcome = super::refresh_installed_service_with_state_and_runner(
        &runner,
        &spec,
        Some(DaemonServiceState::StoppedEnabled),
        TEST_BUILD_VERSION,
    )
    .expect("refresh service");
    let listener = UnixListener::bind(&custom_socket).expect("bind managed daemon socket");
    let _served = serve_identity_probes(listener, vec![TEST_BUILD_VERSION]);
    super::restore_installed_service_after_update_with_runner(
        &runner,
        previous_state,
        TEST_BUILD_VERSION,
    )
    .expect("restore service state");

    assert_eq!(outcome, Some(service_path.clone()));
    let unit = std::fs::read_to_string(service_path).expect("service unit");
    assert!(unit.contains(&format!(
        "ExecStart=/opt/tracedecay/bin/tracedecay daemon run --socket {}",
        custom_socket.display()
    )));
    assert!(!unit.contains("/run/user/1000/tracedecay.sock"));
    assert!(unit.contains("--remote-listen \"192.0.2.10:7443\""));
    assert!(unit.contains("--remote-tls-cert \"/etc/trace decay/server.pem\""));
    assert!(unit.contains("--remote-tls-key \"/etc/trace decay/server-key.pem\""));
    let commands = std::fs::read_to_string(log).expect("systemctl log");
    assert!(
        systemctl_log_contains_sequence(
            &commands,
            &[
                "--user is-active --quiet tracedecay.service",
                "--user is-enabled tracedecay.service",
                "--user stop tracedecay.service",
                "--user daemon-reload",
                "--user daemon-reload",
                "--user enable tracedecay.service",
                "--user start tracedecay.service",
            ]
        ),
        "refresh must only reload; restore of the previously running-enabled state must reload, enable, and start, got:\n{commands}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn refresh_installed_service_migrates_overlong_generated_socket_path() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("temp dir");
    let config_home = dir.path().join("config");
    let fake_bin = dir.path().join("bin");
    let home = dir.path().join("home");
    let profile = dir.path().join("p".repeat(120)).join(".tracedecay");
    std::fs::create_dir_all(&fake_bin).expect("fake bin dir");
    std::fs::create_dir_all(&home).expect("home dir");

    let systemctl = fake_bin.join("systemctl");
    std::fs::write(
        &systemctl,
        "#!/bin/sh\n[ \"$2\" = is-enabled ] && echo enabled\nexit 0\n",
    )
    .expect("fake systemctl");
    std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755))
        .expect("systemctl permissions");
    let runner = ServiceRunner::systemd(&systemctl).expect("fixture systemd runner");

    let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
    let _home_guard = EnvVarGuard::set("HOME", &home);
    let _data_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile);

    let legacy_socket = profile.join("daemon.sock");
    let expected_socket = super::default_socket_path().expect("short default socket");
    assert_ne!(legacy_socket, expected_socket);

    let service_path = config_home.join("systemd/user").join(crate::SERVICE_NAME);
    std::fs::create_dir_all(service_path.parent().expect("service parent")).expect("service dir");
    std::fs::write(
        &service_path,
        format!(
            "[Unit]\nDescription=TraceDecay daemon\n\n[Service]\nExecStart=/old/tracedecay daemon run --socket {}\n",
            legacy_socket.display()
        ),
    )
    .expect("existing service unit");

    let spec = DaemonServiceSpec {
        tracedecay_bin: PathBuf::from("/opt/tracedecay/bin/tracedecay"),
        socket_path: expected_socket.clone(),
        data_dir_override: Some(profile),
        remote_tls: None,
    };
    let outcome = super::refresh_installed_service_with_state_and_runner(
        &runner,
        &spec,
        Some(DaemonServiceState::StoppedEnabled),
        TEST_BUILD_VERSION,
    )
    .expect("refresh service");

    assert_eq!(outcome, Some(service_path.clone()));
    let unit = std::fs::read_to_string(service_path).expect("service unit");
    assert!(unit.contains(&format!("--socket {}", expected_socket.display())));
    assert!(!unit.contains(&legacy_socket.display().to_string()));
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
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TRACEDECAY_SYSTEMCTL_LOG\"\n[ \"$2\" = is-enabled ] && echo enabled\nexit 0\n",
    )
    .expect("fake systemctl");
    std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755))
        .expect("systemctl permissions");
    let runner = ServiceRunner::systemd(&systemctl).expect("fixture systemd runner");

    let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
    let _home_guard = EnvVarGuard::set("HOME", &home);
    let _data_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, dir.path().join("profile"));
    let _log_guard = EnvVarGuard::set("TRACEDECAY_SYSTEMCTL_LOG", &log);
    let service_path = config_home.join("systemd/user").join(crate::SERVICE_NAME);
    std::fs::create_dir_all(service_path.parent().expect("service parent")).expect("service dir");
    let custom_socket = dir.path().join("custom-tracedecay.sock");
    let original_unit = format!(
        "[Service]\nExecStart=/old/tracedecay daemon run --socket {}\n",
        custom_socket.display()
    );
    std::fs::write(&service_path, &original_unit).expect("existing service unit");
    let listener = UnixListener::bind(&custom_socket).expect("bind managed daemon socket");
    let served = serve_identity_probes(listener, vec![TEST_BUILD_VERSION]);

    super::restore_installed_service_after_update_with_runner(
        &runner,
        DaemonServiceState::RunningEnabled,
        TEST_BUILD_VERSION,
    )
    .expect("restore service");

    assert_eq!(
        std::fs::read_to_string(service_path).expect("service unit"),
        original_unit
    );
    assert!(
        served.load(Ordering::SeqCst) >= 1,
        "restore success must be backed by an authenticated identity answer"
    );
    let commands = std::fs::read_to_string(log).expect("systemctl log");
    assert!(
        systemctl_log_contains_sequence(
            &commands,
            &[
                "--user daemon-reload",
                "--user enable tracedecay.service",
                "--user start tracedecay.service",
            ]
        ),
        "restore of a previously-running unit must enable and start, got:\n{commands}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn restore_after_update_does_not_activate_a_held_stopped_unit() {
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
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TRACEDECAY_SYSTEMCTL_LOG\"\n[ \"$2\" = is-enabled ] && echo enabled\nexit 0\n",
    )
    .expect("fake systemctl");
    std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755))
        .expect("systemctl permissions");
    let runner = ServiceRunner::systemd(&systemctl).expect("fixture systemd runner");

    let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
    let _home_guard = EnvVarGuard::set("HOME", &home);
    let _data_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, dir.path().join("profile"));
    let _log_guard = EnvVarGuard::set("TRACEDECAY_SYSTEMCTL_LOG", &log);
    let service_path = config_home.join("systemd/user").join(crate::SERVICE_NAME);
    std::fs::create_dir_all(service_path.parent().expect("service parent")).expect("service dir");
    let custom_socket = dir.path().join("custom-tracedecay.sock");
    let original_unit = format!(
        "[Service]\nExecStart=/old/tracedecay daemon run --socket {}\n",
        custom_socket.display()
    );
    std::fs::write(&service_path, &original_unit).expect("existing service unit");
    super::restore_installed_service_after_update_with_runner(
        &runner,
        DaemonServiceState::StoppedDisabled,
        TEST_BUILD_VERSION,
    )
    .expect("preserve held stopped service");

    assert_eq!(
        std::fs::read_to_string(service_path).expect("service unit"),
        original_unit,
        "restore must not rewrite a held stopped unit"
    );
    assert!(
        !log.exists()
            || std::fs::read_to_string(&log)
                .expect("systemctl log")
                .is_empty(),
        "restore of a held stopped unit must not invoke systemctl"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn no_start_install_then_refresh_and_restore_has_no_activation_commands() {
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
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TRACEDECAY_SYSTEMCTL_LOG\"\ncase \"$2\" in start|restart|enable) exit 99;; esac\nexit 0\n",
    )
    .expect("fake systemctl");
    std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755))
        .expect("systemctl permissions");
    let runner = ServiceRunner::systemd(&systemctl).expect("fixture systemd runner");

    let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
    let _home_guard = EnvVarGuard::set("HOME", &home);
    let _data_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, dir.path().join("profile"));
    let _log_guard = EnvVarGuard::set("TRACEDECAY_SYSTEMCTL_LOG", &log);
    let service_path = config_home.join("systemd/user").join(crate::SERVICE_NAME);
    std::fs::create_dir_all(service_path.parent().expect("service parent")).expect("service dir");
    std::fs::write(
        &service_path,
        "[Service]\nExecStart=/old/tracedecay daemon run --socket /custom/tracedecay.sock\n",
    )
    .expect("existing service unit");
    let spec = DaemonServiceSpec {
        tracedecay_bin: PathBuf::from("/opt/tracedecay/bin/tracedecay"),
        socket_path: PathBuf::from("/custom/tracedecay.sock"),
        data_dir_override: None,
        remote_tls: None,
    };

    runner
        .install(&service_path, false, &spec.socket_path, TEST_BUILD_VERSION)
        .expect("install service without starting it");
    super::refresh_installed_service_with_state_and_runner(
        &runner,
        &spec,
        Some(DaemonServiceState::StoppedDisabled),
        TEST_BUILD_VERSION,
    )
    .expect("refresh held service");
    super::restore_installed_service_after_update_with_runner(
        &runner,
        DaemonServiceState::StoppedDisabled,
        TEST_BUILD_VERSION,
    )
    .expect("preserve no-start install");

    let commands = std::fs::read_to_string(log).expect("systemctl log");
    assert_eq!(
        commands.lines().collect::<Vec<_>>(),
        vec!["--user daemon-reload", "--user daemon-reload"],
        "install --no-start plus update/post-update may reload units but must not start, restart, enable, or enable --now"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn restore_after_update_waits_for_authenticated_daemon_identity() {
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
        "#!/bin/sh\n[ \"$2\" = is-enabled ] && echo enabled\nexit 0\n",
    )
    .expect("fake systemctl");
    std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755))
        .expect("systemctl permissions");
    let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
    let _home_guard = EnvVarGuard::set("HOME", &home);
    let _data_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, dir.path().join("profile"));
    let _path_guard = EnvVarGuard::set("PATH", &fake_bin);
    let service_path = config_home.join("systemd/user").join(crate::SERVICE_NAME);
    std::fs::create_dir_all(service_path.parent().expect("service parent")).expect("service dir");
    let socket_path = dir.path().join("daemon.sock");
    std::fs::write(
        &service_path,
        format!(
            "[Service]\nExecStart=/old/tracedecay daemon run --socket {}\n",
            socket_path.display()
        ),
    )
    .expect("existing service unit");
    let listener = UnixListener::bind(&socket_path).expect("bind managed daemon socket");
    // The first identity answer is a stale daemon; restore must keep polling
    // until the expected version answers instead of trusting the systemctl
    // exit status.
    let served = serve_identity_probes(listener, vec!["0.0.0-stale", TEST_BUILD_VERSION]);

    super::restore_installed_service_after_update(
        DaemonServiceState::RunningEnabled,
        TEST_BUILD_VERSION,
    )
    .expect("restore must succeed once the daemon answers the expected identity");

    assert_eq!(
        served.load(Ordering::SeqCst),
        2,
        "restore must consume the stale identity answer and re-poll until the expected version"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn start_service_reloads_units_and_requires_authenticated_identity() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("temp dir");
    let config_home = dir.path().join("config");
    let fake_bin = dir.path().join("bin");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&fake_bin).expect("fake bin dir");
    std::fs::create_dir_all(&home).expect("home dir");
    let systemctl = fake_bin.join("systemctl");
    let log = dir.path().join("systemctl.log");
    // The started marker begins absent (unit stopped) and `start` creates it
    // with shell redirection: PATH holds only the fake bin dir, so external
    // commands like `rm`/`touch` are unavailable inside the script.
    let started = dir.path().join("systemctl.started");
    std::fs::write(
            &systemctl,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TRACEDECAY_SYSTEMCTL_LOG\"\n[ \"$2\" = is-enabled ] && echo enabled\n[ \"$2\" = is-active ] && [ ! -f \"$TRACEDECAY_SYSTEMCTL_STARTED\" ] && exit 3\n[ \"$2\" = start ] && : > \"$TRACEDECAY_SYSTEMCTL_STARTED\"\nexit 0\n",
        )
        .expect("fake systemctl");
    std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755))
        .expect("systemctl permissions");
    let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
    let _home_guard = EnvVarGuard::set("HOME", &home);
    let _data_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, dir.path().join("profile"));
    let _path_guard = EnvVarGuard::set("PATH", &fake_bin);
    let _log_guard = EnvVarGuard::set("TRACEDECAY_SYSTEMCTL_LOG", &log);
    let _started_guard = EnvVarGuard::set("TRACEDECAY_SYSTEMCTL_STARTED", &started);
    let service_path = config_home.join("systemd/user").join(crate::SERVICE_NAME);
    std::fs::create_dir_all(service_path.parent().expect("service parent")).expect("service dir");
    let socket_path = dir.path().join("daemon.sock");
    std::fs::write(
        &service_path,
        format!(
            "[Service]\nExecStart=/old/tracedecay daemon run --socket {}\n",
            socket_path.display()
        ),
    )
    .expect("existing service unit");
    let listener = UnixListener::bind(&socket_path).expect("bind managed daemon socket");
    let served = serve_identity_probes(listener, vec![TEST_BUILD_VERSION]);

    super::start_service(TEST_BUILD_VERSION).expect("start service");

    let commands = std::fs::read_to_string(log).expect("systemctl log");
    assert!(
        systemctl_log_contains_sequence(
            &commands,
            &["--user daemon-reload", "--user start tracedecay.service"]
        ),
        "start must reload the unit definition before starting it, got:\n{commands}"
    );
    assert!(
        served.load(Ordering::SeqCst) >= 1,
        "start success must be backed by an authenticated identity answer"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn wait_for_installed_service_state_rejects_identity_mismatch_at_the_deadline() {
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
        "#!/bin/sh\n[ \"$2\" = is-enabled ] && echo enabled\nexit 0\n",
    )
    .expect("fake systemctl");
    std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755))
        .expect("systemctl permissions");
    let runner = ServiceRunner::systemd(&systemctl).expect("fixture systemd runner");
    let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
    let _home_guard = EnvVarGuard::set("HOME", &home);
    let _data_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, dir.path().join("profile"));
    let service_path = config_home.join("systemd/user").join(crate::SERVICE_NAME);
    std::fs::create_dir_all(service_path.parent().expect("service parent")).expect("service dir");
    let socket_path = dir.path().join("daemon.sock");
    std::fs::write(
        &service_path,
        format!(
            "[Service]\nExecStart=/old/tracedecay daemon run --socket {}\n",
            socket_path.display()
        ),
    )
    .expect("existing service unit");
    let listener = UnixListener::bind(&socket_path).expect("bind managed daemon socket");
    let _served = serve_identity_probes(listener, vec!["0.0.0-stale"]);

    let error = super::wait_for_installed_service_state_with(
        &runner,
        DaemonServiceState::RunningEnabled,
        TEST_BUILD_VERSION,
        std::time::Duration::from_millis(700),
    )
    .expect_err("a daemon that never answers the expected version must fail the wait");

    assert!(
        error.to_string().contains("did not return to"),
        "failure must name the state the daemon never reached, got: {error}"
    );
    assert!(
        error.to_string().contains("identity mismatch"),
        "failure must surface the mismatched identity, got: {error}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn wait_for_installed_service_state_rejects_unresponsive_socket_at_the_deadline() {
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
        "#!/bin/sh\n[ \"$2\" = is-enabled ] && echo enabled\nexit 0\n",
    )
    .expect("fake systemctl");
    std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755))
        .expect("systemctl permissions");
    let runner = ServiceRunner::systemd(&systemctl).expect("fixture systemd runner");
    let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
    let _home_guard = EnvVarGuard::set("HOME", &home);
    let _data_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, dir.path().join("profile"));
    let service_path = config_home.join("systemd/user").join(crate::SERVICE_NAME);
    std::fs::create_dir_all(service_path.parent().expect("service parent")).expect("service dir");
    let socket_path = dir.path().join("daemon.sock");
    std::fs::write(
        &service_path,
        format!(
            "[Service]\nExecStart=/old/tracedecay daemon run --socket {}\n",
            socket_path.display()
        ),
    )
    .expect("existing service unit");
    // Leave a socket file behind with nothing listening: the reported-running
    // unit never serves it, so the wait must fail instead of trusting the
    // service manager's state alone.
    drop(UnixListener::bind(&socket_path).expect("bind managed daemon socket"));

    let error = super::wait_for_installed_service_state_with(
        &runner,
        DaemonServiceState::RunningEnabled,
        TEST_BUILD_VERSION,
        std::time::Duration::from_millis(600),
    )
    .expect_err("a daemon that never serves its socket must fail the wait");

    assert!(
        error.to_string().contains("did not return to"),
        "failure must name the state the daemon never reached, got: {error}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn restore_after_update_leaves_masked_and_missing_units_untouched() {
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
    let runner = ServiceRunner::systemd(&systemctl).expect("fixture systemd runner");

    let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
    let _home_guard = EnvVarGuard::set("HOME", &home);
    let _data_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, dir.path().join("profile"));
    let _log_guard = EnvVarGuard::set("TRACEDECAY_SYSTEMCTL_LOG", &log);

    super::restore_installed_service_after_update_with_runner(
        &runner,
        DaemonServiceState::Missing,
        TEST_BUILD_VERSION,
    )
    .expect("missing stays missing");
    assert!(
        !log.exists()
            || std::fs::read_to_string(&log)
                .expect("systemctl log")
                .is_empty(),
        "missing must not invoke systemctl"
    );

    let service_path = config_home.join("systemd/user").join(crate::SERVICE_NAME);
    std::fs::create_dir_all(service_path.parent().expect("service parent")).expect("service dir");
    std::fs::write(
        &service_path,
        "[Service]\nExecStart=/old/tracedecay daemon run\n",
    )
    .expect("masked unit");

    super::restore_installed_service_after_update_with_runner(
        &runner,
        DaemonServiceState::Masked,
        TEST_BUILD_VERSION,
    )
    .expect("masked stays masked");
    assert!(
        !log.exists()
            || std::fs::read_to_string(&log)
                .expect("systemctl log")
                .is_empty(),
        "masked must not invoke systemctl"
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
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TRACEDECAY_SYSTEMCTL_LOG\"\n[ \"$2\" = is-active ] && exit 3\n[ \"$2\" = is-enabled ] && echo enabled\nexit 0\n",
        )
        .expect("fake systemctl");
    std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755))
        .expect("systemctl permissions");
    let runner = ServiceRunner::systemd(&systemctl).expect("fixture systemd runner");
    let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
    let _home_guard = EnvVarGuard::set("HOME", &home);
    let _data_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, dir.path().join("profile"));
    let _log_guard = EnvVarGuard::set("TRACEDECAY_SYSTEMCTL_LOG", &log);
    let service_path = config_home.join("systemd/user").join(crate::SERVICE_NAME);
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
        remote_tls: None,
    };

    super::with_quiesced_installed_service_with_runner(
        runner,
        "daemon service refresh",
        TEST_BUILD_VERSION,
        |_, runner| {
            super::refresh_installed_service_with_state_and_runner(
                runner,
                &spec,
                None,
                TEST_BUILD_VERSION,
            )
        },
    )
    .expect("refresh service");

    let commands = std::fs::read_to_string(log).expect("systemctl log");
    assert!(commands.contains("--user is-active --quiet tracedecay.service"));
    assert!(
        !commands.contains("enable tracedecay.service"),
        "a stopped-enabled service is already enabled; refresh must not mutate its lifecycle"
    );
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
    let runner = ServiceRunner::systemd(&systemctl).expect("fixture systemd runner");

    assert_eq!(
        runner
            .service_state(&dir.path().join("daemon.sock"))
            .expect("systemd service state"),
        DaemonServiceState::Masked
    );
}

#[cfg(target_os = "linux")]
#[test]
fn service_fixture_does_not_hide_git_during_concurrent_spawn() {
    let git_program = tracedecay_runtime_core::git::try_git_program()
        .expect("canonical Git executable")
        .to_os_string();
    assert!(
        Path::new(&git_program).is_absolute(),
        "the canonical Git authority must resolve before fixture overlap"
    );
    let original_path = std::env::var_os("PATH");
    let dir = TempDir::new().expect("temp dir");
    let fake_bin = dir.path().join("bin");
    std::fs::create_dir_all(&fake_bin).expect("fake bin dir");
    let systemctl = fake_bin.join("systemctl");
    let observed = dir.path().join("systemctl.observed");
    let ready = dir.path().join("systemctl.ready");
    let release = dir.path().join("systemctl.release");
    let mkfifo = Command::new("/usr/bin/mkfifo")
        .args([&ready, &release])
        .status()
        .expect("spawn mkfifo");
    assert!(mkfifo.success(), "create synchronization FIFOs");
    std::fs::write(
        &systemctl,
        format!(
            "#!/bin/sh\n\
             if [ ! -e '{observed}' ]; then\n\
               : > '{observed}'\n\
               printf 'ready\\n' > '{ready}'\n\
               IFS= read -r release < '{release}'\n\
             fi\n\
             [ \"$2\" = is-active ] && exit 3\n\
             [ \"$2\" = is-enabled ] && echo enabled\n\
             exit 0\n",
            observed = observed.display(),
            ready = ready.display(),
            release = release.display(),
        ),
    )
    .expect("fake systemctl");
    std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755))
        .expect("systemctl permissions");
    let runner = ServiceRunner::systemd(&systemctl).expect("fixture systemd runner");

    let socket_path = dir.path().join("daemon.sock");
    let fixture = std::thread::spawn(move || {
        runner
            .service_state(&socket_path)
            .expect("fixture systemd state")
    });

    let ready_event = std::fs::read_to_string(&ready).expect("wait for systemctl ready event");
    let path_during_overlap = std::env::var_os("PATH");
    let git_output = Command::new(&git_program).arg("--version").output();
    std::fs::write(&release, "continue\n").expect("release systemctl fixture");
    assert_eq!(
        fixture.join().expect("join service fixture"),
        DaemonServiceState::StoppedEnabled
    );
    assert_eq!(
        ready_event, "ready\n",
        "systemctl fixture must reach barrier"
    );

    if let Err(error) = &git_output {
        assert_ne!(
            error.kind(),
            std::io::ErrorKind::NotFound,
            "concurrent Git spawn must never fail with ENOENT"
        );
    }
    assert!(
        git_output.expect("spawn canonical Git").status.success(),
        "canonical Git invocation must succeed"
    );
    assert_eq!(
        path_during_overlap, original_path,
        "service fixtures must not mutate process-global PATH"
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
    let service_path = config_home.join("systemd/user").join(crate::SERVICE_NAME);
    std::fs::create_dir_all(service_path.parent().expect("service parent")).expect("service dir");
    std::os::unix::fs::symlink("/dev/null", &service_path).expect("mask service");
    let spec = DaemonServiceSpec {
        tracedecay_bin: PathBuf::from("/opt/tracedecay/bin/tracedecay"),
        socket_path: PathBuf::from("/run/user/1000/tracedecay.sock"),
        data_dir_override: None,
        remote_tls: None,
    };

    let error = super::refresh_installed_service_under_lease_with_state(
        &spec,
        DaemonServiceState::Masked,
        TEST_BUILD_VERSION,
    )
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
    let _socket_guard = EnvVarGuard::unset(SOCKET_ENV);
    let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, profile.path().join(".tracedecay"));
    let expected_socket = user_data_dir().expect("user data dir").join("daemon.sock");

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

    let _override_guard = EnvVarGuard::set(SOCKET_ENV, &override_socket);
    assert_eq!(
        super::default_socket_path().expect("override socket path"),
        override_socket
    );
}

/// A profile rooted deep enough to overflow `sockaddr_un` (macOS `SUN_LEN`)
/// must not surface as a daemon that cannot bind its socket. The default
/// endpoint re-derives to a short deterministic per-profile path that daemon
/// and clients converge on independently.
#[cfg(unix)]
#[test]
fn over_long_profile_socket_path_falls_back_to_a_short_deterministic_path() {
    let _env_lock = lock_user_data_dir_test_env();
    let _socket_guard = EnvVarGuard::unset(SOCKET_ENV);
    let root = tempfile::TempDir::new().expect("profile temp dir");
    let deep_profile = root.path().join("p".repeat(120)).join(".tracedecay");
    let sibling_profile = root.path().join("q".repeat(120)).join(".tracedecay");

    let first = {
        let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &deep_profile);
        let first = super::default_socket_path().expect("fallback socket path");
        assert_eq!(
            first,
            super::default_socket_path().expect("repeat lookup"),
            "clients and daemon must derive the same endpoint independently"
        );
        first
    };
    assert!(
        tracedecay_daemon_protocol::unix_socket_path_within_limit(&first),
        "fallback endpoint must satisfy the platform socket path limit: {}",
        first.display()
    );
    assert!(
        first.starts_with("/tmp"),
        "fallback endpoint must live under the fixed short base: {}",
        first.display()
    );

    let sibling = {
        let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &sibling_profile);
        super::default_socket_path().expect("sibling fallback socket path")
    };
    assert_ne!(
        first, sibling,
        "distinct profiles must keep distinct daemon endpoints"
    );
}

#[cfg(unix)]
#[test]
fn short_socket_derivation_uses_the_installed_profile_not_the_shell_profile() {
    let _env_lock = lock_user_data_dir_test_env();
    let _socket_guard = EnvVarGuard::unset(SOCKET_ENV);
    let root = tempfile::TempDir::new().expect("profile temp dir");
    let installed_profile = root.path().join("i".repeat(120)).join(".tracedecay");
    let shell_profile = root.path().join("shell-profile");
    let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &shell_profile);

    let installed_socket = super::default_socket_path_for_profile(&installed_profile);
    let shell_socket = super::default_socket_path().expect("shell socket path");

    assert!(
        tracedecay_daemon_protocol::unix_socket_path_within_limit(&installed_socket),
        "installed profile endpoint must satisfy the platform limit"
    );
    assert_ne!(installed_socket, shell_socket);
    assert_ne!(installed_socket, installed_profile.join("daemon.sock"));
}
