//! Restore-side version validation across a binary upgrade.
//!
//! A maintenance window quiesces the OLD daemon, the action installs a NEW
//! binary, and the restore starts that new binary. These tests pin the two
//! halves of that contract: the guard adopts the installed version reported
//! by the action (so readiness validates the daemon actually being started),
//! and the readiness probe keeps failing closed with a typed identity
//! mismatch when the daemon that answers is not the expected version.

#[cfg(unix)]
use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::io::{BufRead, Write};
#[cfg(unix)]
use std::os::unix::net::UnixListener;

#[cfg(unix)]
use tempfile::TempDir;
#[cfg(unix)]
use tracedecay_runtime_core::config::{USER_DATA_DIR_ENV, lock_user_data_dir_test_env};

use super::runner::ServiceRunner;
use super::{DaemonServiceState, MaintenanceWindowOutcome, QuiescedDaemonLifecycle};

const QUIESCED_VERSION: &str = "0.1.0-test+quiesced";
const INSTALLED_VERSION: &str = "0.2.0-test+installed";

/// A guard as it exists inside the maintenance window right after the action
/// ran: lease released or consumed elsewhere, restore not yet attempted.
/// `restored: true` keeps `Drop` from touching the real service manager or
/// lease file.
fn quiesced_guard() -> QuiescedDaemonLifecycle {
    QuiescedDaemonLifecycle {
        previous_state: DaemonServiceState::RunningEnabled,
        lifecycle_lease: None,
        expected_version: QUIESCED_VERSION.to_owned(),
        runner: ServiceRunner::WindowsTask,
        restored: true,
    }
}

#[test]
fn maintenance_outcome_carries_installed_version_to_restore_validation() {
    let mut guard = quiesced_guard();

    let value = guard.adopt_maintenance_outcome(MaintenanceWindowOutcome {
        value: 7_u32,
        installed_version: Some(INSTALLED_VERSION.to_owned()),
    });

    assert_eq!(value, 7);
    assert_eq!(
        guard.expected_version, INSTALLED_VERSION,
        "restore must validate the freshly installed binary, not the quiesced one"
    );
}

#[test]
fn maintenance_outcome_without_install_keeps_acquire_time_version() {
    let mut guard = quiesced_guard();

    guard.adopt_maintenance_outcome(MaintenanceWindowOutcome {
        value: (),
        installed_version: None,
    });

    assert_eq!(
        guard.expected_version, QUIESCED_VERSION,
        "no install restarts the same binary, so the acquire-time version stays authoritative"
    );
}

#[cfg(unix)]
struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

#[cfg(unix)]
impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let previous = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

#[cfg(unix)]
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

#[cfg(unix)]
fn serve_initialize_identity(
    listener: UnixListener,
    name: &'static str,
    version: &'static str,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept readiness probe");
        let mut reader =
            std::io::BufReader::new(stream.try_clone().expect("clone readiness stream"));
        let mut line = String::new();
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

/// The upgrade success shape: old and new versions differ, the restarted
/// daemon reports the NEW (installed) version, and readiness passes because
/// the guard validates the version it adopted from the action's outcome.
/// Before the outcome existed, restore kept the quiesced version and this
/// exact situation timed out as an identity mismatch.
#[cfg(unix)]
#[test]
fn restore_readiness_accepts_the_upgraded_daemon_reporting_the_installed_version() {
    let _env_lock = lock_user_data_dir_test_env();
    let profile = TempDir::new().expect("profile temp dir");
    let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, profile.path());

    let mut guard = quiesced_guard();
    guard.adopt_maintenance_outcome(MaintenanceWindowOutcome {
        value: (),
        installed_version: Some(INSTALLED_VERSION.to_owned()),
    });

    let socket_path = profile.path().join("upgraded.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind upgraded daemon socket");
    let server = serve_initialize_identity(listener, "tracedecay", INSTALLED_VERSION);

    assert_eq!(
        super::probe::daemon_protocol_state_with_timeout(
            &socket_path,
            &guard.expected_version,
            std::time::Duration::from_secs(5),
        ),
        super::probe::DaemonProtocolState::Ready
    );
    server.join().expect("join upgraded daemon");
}

/// Version skew must keep failing closed: when the daemon that answers after
/// an upgrade is still the OLD binary (restart raced or was lost), readiness
/// against the installed version reports a typed identity mismatch instead of
/// accepting whatever is running.
#[cfg(unix)]
#[test]
fn restore_readiness_rejects_a_stale_daemon_after_an_upgrade() {
    let _env_lock = lock_user_data_dir_test_env();
    let profile = TempDir::new().expect("profile temp dir");
    let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, profile.path());

    let mut guard = quiesced_guard();
    guard.adopt_maintenance_outcome(MaintenanceWindowOutcome {
        value: (),
        installed_version: Some(INSTALLED_VERSION.to_owned()),
    });

    let socket_path = profile.path().join("stale.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind stale daemon socket");
    let server = serve_initialize_identity(listener, "tracedecay", QUIESCED_VERSION);

    assert_eq!(
        super::probe::daemon_protocol_state_with_timeout(
            &socket_path,
            &guard.expected_version,
            std::time::Duration::from_secs(5),
        ),
        super::probe::DaemonProtocolState::IdentityMismatch {
            name: Some("tracedecay".to_string()),
            version: Some(QUIESCED_VERSION.to_string()),
            expected_version: INSTALLED_VERSION.to_string(),
        }
    );
    server.join().expect("join stale daemon");
}
