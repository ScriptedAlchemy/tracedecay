//! Restore-side version validation across a binary upgrade.
//!
//! A maintenance window quiesces the OLD daemon, the action installs a NEW
//! binary, and the restore starts that new binary. These tests pin the two
//! halves of that contract: the guard adopts the installed version reported
//! by the action (so readiness validates the daemon actually being started),
//! and the readiness probe keeps failing closed with a typed identity
//! mismatch when the daemon that answers is not the expected version.

#[cfg(unix)]
use std::os::unix::net::UnixListener;

#[cfg(unix)]
use tempfile::TempDir;
#[cfg(unix)]
use tracedecay_runtime_core::config::{USER_DATA_DIR_ENV, lock_user_data_dir_test_env};

use super::runner::ServiceRunner;
use super::{
    DaemonServiceState, MaintenanceWindowOutcome, QuiescedDaemonLifecycle, RestoreSettlement,
};

const QUIESCED_VERSION: &str = "0.1.0-test+quiesced";
const INSTALLED_VERSION: &str = "0.2.0-test+installed";

/// A guard as it exists inside the maintenance window right after the action
/// ran: lease released or consumed elsewhere, restore not yet attempted.
/// `RestoreSettlement::Complete` keeps `Drop` from touching the real service
/// manager or lease file.
fn quiesced_guard() -> QuiescedDaemonLifecycle {
    QuiescedDaemonLifecycle {
        previous_state: DaemonServiceState::RunningEnabled,
        lifecycle_lease: None,
        expected_version: QUIESCED_VERSION.to_owned(),
        runner: ServiceRunner::WindowsTask,
        settlement: RestoreSettlement::Complete,
    }
}

#[cfg(unix)]
use super::isolated_profile::EnvVarGuard;

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
    let authority = super::tests::seed_socket_authority(&socket_path);
    let listener = UnixListener::bind(&socket_path).expect("bind stale daemon socket");
    let server = super::tests::serve_probe_response(
        listener,
        "tracedecay",
        QUIESCED_VERSION,
        authority.auth_token().to_owned(),
    );

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
