//! A profile deleted under a running daemon ends it as `ProfileLost`.
//!
//! In #4099 the daemon kept writing 112 GB into the unlinked files of a deleted
//! profile. It now notices its profile identity file is gone, closes its
//! stores through the ordinary graceful shutdown and exits, so its supervisor
//! restarts it on whatever profile is at the path.

use std::fs;
use std::time::{Duration, Instant};

use crate::common::{IsolatedHome, poll_until, spawn_tracedecay_daemon_logged};

const PROFILE_LOST: &str = "event=daemon_shutdown outcome=profile_lost";

#[test]
fn deleting_the_profile_under_a_running_daemon_ends_it_as_profile_lost() {
    let (environment, _project) = IsolatedHome::new();
    let log_path = environment.scratch().join("profile-lost.log");
    let mut daemon = spawn_tracedecay_daemon_logged(environment.home(), &log_path, |_| {});
    let log = || fs::read_to_string(&log_path).unwrap_or_default();
    poll_until(
        Instant::now() + Duration::from_secs(120),
        Duration::from_millis(25),
        || log().contains("event=daemon_ready").then_some(()),
        || format!("the daemon must report ready; log={}", log()),
    );

    let profile_root = environment.profile_root().to_path_buf();
    poll_until(
        Instant::now() + Duration::from_secs(30),
        Duration::from_millis(25),
        || match fs::remove_dir_all(&profile_root) {
            Ok(()) => Some(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(()),
            Err(_) => None,
        },
        || format!("delete the profile at {}", profile_root.display()),
    );
    let deleted = Instant::now();

    // One watch tick (1 s) plus scheduling slack on a loaded host.
    poll_until(
        deleted + Duration::from_secs(5),
        Duration::from_millis(25),
        || log().contains(PROFILE_LOST).then_some(()),
        || {
            format!(
                "the daemon must report its lost profile within one tick; log={}",
                log()
            )
        },
    );
    let exit = daemon
        .wait_for_exit(Duration::from_secs(120))
        .expect("wait for the daemon to exit")
        .unwrap_or_else(|| panic!("a daemon whose profile is lost must exit; log={}", log()));
    let log = log();
    assert_eq!(
        exit.code(),
        Some(tracedecay::daemon::PROFILE_LOST_EXIT_CODE),
        "launchd restarts only a non-zero exit; log={log}"
    );
    assert!(
        !log.contains("still held at shutdown"),
        "every store runtime must close before the exit; exit={exit} log={log}"
    );
}
