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
use tracedecay_runtime_core::config::ProfileRoot;

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
fn quiesced_guard(profile: &ProfileRoot) -> QuiescedDaemonLifecycle {
    QuiescedDaemonLifecycle {
        profile: profile.clone(),
        previous_state: DaemonServiceState::RunningEnabled,
        lifecycle_lease: None,
        expected_version: QUIESCED_VERSION.to_owned(),
        runner: ServiceRunner::WindowsTask,
        settlement: RestoreSettlement::Complete,
    }
}

/// Version skew must keep failing closed: when the daemon that answers after
/// an upgrade is still the OLD binary (restart raced or was lost), readiness
/// against the installed version reports a typed identity mismatch instead of
/// accepting whatever is running.
#[cfg(unix)]
#[test]
fn restore_readiness_rejects_a_stale_daemon_after_an_upgrade() {
    let profile_dir = TempDir::new().expect("profile temp dir");
    let profile = ProfileRoot::new(profile_dir.path());

    let mut guard = quiesced_guard(&profile);
    guard.adopt_maintenance_outcome(MaintenanceWindowOutcome {
        value: (),
        installed_version: Some(INSTALLED_VERSION.to_owned()),
    });

    let socket_path = profile_dir.path().join("stale.sock");
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
            &profile,
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

/// A managed daemon as the maintenance window sees it: `systemctl stop`
/// returns while the process is still draining, so its shared lifecycle lease
/// outlives the stop. The holder keeps the lease until `release_after_stop`
/// past the observed stop, or forever when `None`, injecting exactly the
/// contention `tracedecay update` met on a healthy launchd daemon.
#[cfg(target_os = "linux")]
struct DrainingDaemonFixture {
    _dir: TempDir,
    runner: ServiceRunner,
    socket_path: std::path::PathBuf,
    log: std::path::PathBuf,
    stopped_marker: std::path::PathBuf,
    profile: ProfileRoot,
}

/// The simulated daemon process; dropping it releases a lease held with
/// `release_after_stop: None`.
#[cfg(target_os = "linux")]
struct LeaseHolder {
    _release: std::sync::mpsc::Sender<()>,
    thread: std::thread::JoinHandle<()>,
}

#[cfg(target_os = "linux")]
impl DrainingDaemonFixture {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().expect("temp dir");
        let config_home = dir.path().join("config");
        let fake_bin = dir.path().join("bin");
        let home = dir.path().join("home");
        let profile_dir = dir.path().join("profile");
        std::fs::create_dir_all(&fake_bin).expect("fake bin dir");
        std::fs::create_dir_all(&home).expect("home dir");
        std::fs::create_dir_all(&profile_dir).expect("profile dir");
        let profile = ProfileRoot::new(&profile_dir)
            .with_home(&home)
            .with_xdg_config_home(&config_home);
        let systemctl = fake_bin.join("systemctl");
        let log = dir.path().join("systemctl.log");
        let stopped_marker = dir.path().join("systemctl.stopped");
        std::fs::write(
            &systemctl,
            super::tests::bake_script_paths(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TRACEDECAY_SYSTEMCTL_LOG\"\n[ \"$2\" = is-enabled ] && echo enabled\n[ \"$2\" = is-active ] && [ -f \"$TRACEDECAY_SYSTEMCTL_STOPPED\" ] && { echo inactive; exit 3; }\n[ \"$2\" = stop ] && touch \"$TRACEDECAY_SYSTEMCTL_STOPPED\"\n[ \"$2\" = start ] && rm -f \"$TRACEDECAY_SYSTEMCTL_STOPPED\"\n[ \"$2\" = is-active ] && echo active\nexit 0\n",
                &[
                    ("TRACEDECAY_SYSTEMCTL_LOG", &log),
                    ("TRACEDECAY_SYSTEMCTL_STOPPED", &stopped_marker),
                ],
            ),
        )
        .expect("fake systemctl");
        std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755))
            .expect("systemctl permissions");
        let runner = ServiceRunner::systemd(&systemctl).expect("fixture systemd runner");
        let service_path = config_home.join("systemd/user").join(crate::SERVICE_NAME);
        std::fs::create_dir_all(service_path.parent().expect("service parent"))
            .expect("service dir");
        let socket_path = dir.path().join("tracedecay.sock");
        std::fs::write(
            &service_path,
            format!(
                "[Service]\nExecStart=/old/tracedecay daemon run --socket {}\n",
                socket_path.display()
            ),
        )
        .expect("existing service unit");
        Self {
            _dir: dir,
            runner,
            socket_path,
            log,
            stopped_marker,
            profile,
        }
    }

    /// Holds the daemon's shared lease until `release_after_stop` past the
    /// moment the fake supervisor records the stop; `None` holds it until the
    /// returned holder is dropped.
    fn hold_daemon_lease(&self, release_after_stop: Option<std::time::Duration>) -> LeaseHolder {
        let profile = self.profile.data_dir().to_path_buf();
        let stopped_marker = self.stopped_marker.clone();
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            let lease = tracedecay_runtime_core::lifecycle_lease::acquire_shared_for_profile(
                &profile,
                "managed daemon database ownership",
            )
            .expect("daemon shared lease");
            held_tx.send(()).expect("report held lease");
            match release_after_stop {
                Some(release_after_stop) => {
                    while !stopped_marker.exists() {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    std::thread::sleep(release_after_stop);
                }
                // `recv` returns once the holder (and its sender) is dropped.
                None => drop(release_rx.recv()),
            }
            drop(lease);
        });
        held_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("daemon lease held before the window opens");
        LeaseHolder {
            _release: release_tx,
            thread,
        }
    }

    /// Serves the restarted daemon's identity on the unit's socket so the
    /// restore's readiness wait can prove `RunningEnabled`.
    fn serve_restarted_daemon(&self) -> tracedecay_daemon_identity::authority::DaemonAuthority {
        let authority = super::tests::seed_socket_authority(&self.socket_path);
        let listener = UnixListener::bind(&self.socket_path).expect("bind restarted daemon");
        super::tests::serve_identity_probes(
            listener,
            vec![super::tests::TEST_BUILD_VERSION],
            authority.auth_token().to_owned(),
        );
        authority
    }

    fn assert_daemon_running_after(&self, phase: &str) {
        assert_eq!(
            self.runner
                .service_state(&self.socket_path)
                .expect("service state"),
            DaemonServiceState::RunningEnabled,
            "the managed daemon must be running after {phase}"
        );
        let commands = std::fs::read_to_string(&self.log).expect("systemctl log");
        assert!(
            super::tests::systemctl_log_contains_sequence(
                &commands,
                &[
                    "--user stop tracedecay.service",
                    "--user start tracedecay.service",
                ]
            ),
            "{phase} must stop, then start the daemon again, got:\n{commands}"
        );
    }
}

/// The window's own stop is not contention: the exclusive acquisition waits
/// for the quiesced daemon to release its lease, then the window runs and
/// restores a running daemon.
#[cfg(target_os = "linux")]
#[test]
fn maintenance_window_waits_out_the_lease_of_the_daemon_it_just_stopped() {
    let fixture = DrainingDaemonFixture::new();
    let holder = fixture.hold_daemon_lease(Some(std::time::Duration::from_millis(400)));

    let guard = QuiescedDaemonLifecycle::acquire_with_runner_and_timeout(
        &fixture.profile,
        "update",
        super::tests::TEST_BUILD_VERSION,
        fixture.runner.clone(),
        super::QUIESCED_LEASE_RELEASE_TIMEOUT,
    )
    .expect("the draining daemon's lease must be waited out, not reported as contention");
    holder.thread.join().expect("daemon holder");
    assert_eq!(guard.previous_state(), DaemonServiceState::RunningEnabled);
    assert!(
        guard
            .lifecycle_lease()
            .expect("window lease")
            .is_exclusive(),
        "the window owns the exclusive lease once the daemon released its share"
    );

    let _authority = fixture.serve_restarted_daemon();
    guard
        .finish_after_update()
        .expect("restore the previously running daemon");

    fixture.assert_daemon_running_after("a completed maintenance window");
}

/// A holder that outlives the bound is contention, reported typed, and the
/// failure path still restores the daemon it stopped: contention must never
/// leave a previously healthy daemon stopped.
#[cfg(target_os = "linux")]
#[test]
fn failed_lease_acquisition_restores_the_stopped_daemon_before_reporting() {
    let fixture = DrainingDaemonFixture::new();
    let _holder = fixture.hold_daemon_lease(None);
    let _authority = fixture.serve_restarted_daemon();

    let error = QuiescedDaemonLifecycle::acquire_with_runner_and_timeout(
        &fixture.profile,
        "update",
        super::tests::TEST_BUILD_VERSION,
        fixture.runner.clone(),
        std::time::Duration::from_millis(300),
    )
    .err()
    .expect("a lease held past the bound is contention");

    let message = error.to_string();
    assert!(
        message.contains("cannot start update")
            && message.contains("another lifecycle operation is already active"),
        "contention must stay a typed refusal naming the operation: {message}"
    );
    assert!(
        !message.contains("restoration also failed"),
        "the restore after a lost acquisition must succeed: {message}"
    );
    fixture.assert_daemon_running_after("a lost lease acquisition");
}
