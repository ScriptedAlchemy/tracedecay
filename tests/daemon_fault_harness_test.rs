mod common;

use std::sync::atomic::{AtomicBool, Ordering};

use common::{spawn_tracedecay_daemon_with, tempdir_or_panic};

#[test]
fn configured_daemon_can_be_killed_and_reaped() {
    let home = tempdir_or_panic();
    let configured = AtomicBool::new(false);
    let mut daemon = spawn_tracedecay_daemon_with(home.path(), |command| {
        configured.store(true, Ordering::Relaxed);
        command.env("TRACEDECAY_FAULT_HARNESS_TEST", "1");
    });

    assert!(configured.load(Ordering::Relaxed));
    let status = daemon
        .kill_and_wait()
        .expect("configured daemon should be killed and reaped");
    assert!(!status.success());

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;

        assert_eq!(status.signal(), Some(9));
    }
}
