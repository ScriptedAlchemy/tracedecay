//! A fixture daemon must not outlive a test process killed before `Drop` runs.
//!
//! Harness timeouts and flake runners SIGKILL the test binary. No guard runs
//! then, and the daemon sits in its own process group, so without a
//! parent-death signal it keeps its memory for days.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::common::{self, TestChildProcess, canonical_existing_path, poll_until};

const PROBE_HOME_ENV: &str = "TRACEDECAY_TEST_KILLED_PROBE_HOME";
const DAEMON_PID_FILE: &str = "fixture-daemon.pid";
const PROBE_READY_TIMEOUT: Duration = Duration::from_secs(60);
const ORPHAN_EXIT_TIMEOUT: Duration = Duration::from_secs(2);

fn process_is_running(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rsplit_once(')')
            .and_then(|(_, fields)| fields.split_whitespace().next())
            .is_some_and(|state| state != "Z")
    })
}

/// Runs inside the re-executed test binary: own a fixture daemon, publish its
/// pid, and wait to be SIGKILLed.
fn hold_fixture_daemon(home: &Path) -> ! {
    let daemon = common::spawn_tracedecay_daemon(home);
    let staged = home.join(format!("{DAEMON_PID_FILE}.tmp"));
    std::fs::write(&staged, daemon.id().to_string()).expect("stage daemon pid");
    std::fs::rename(&staged, home.join(DAEMON_PID_FILE)).expect("publish daemon pid");
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

#[test]
fn fixture_daemon_dies_with_a_sigkilled_test_process() {
    if let Some(home) = std::env::var_os(PROBE_HOME_ENV) {
        hold_fixture_daemon(&PathBuf::from(home));
    }
    let scratch = tempfile::tempdir().expect("probe home");
    let home = canonical_existing_path(scratch.path());
    let filter = format!(
        "{}::fixture_daemon_dies_with_a_sigkilled_test_process",
        module_path!()
            .strip_prefix("daemon_suite::")
            .unwrap_or(module_path!())
    );
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .args([
            filter.as_str(),
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PROBE_HOME_ENV, &home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .process_group(0);
    let mut probe = TestChildProcess::new(command.spawn().expect("spawn killed-test probe"));

    let pid_file = home.join(DAEMON_PID_FILE);
    let daemon_pid: u32 = poll_until(
        Instant::now() + PROBE_READY_TIMEOUT,
        Duration::from_millis(50),
        || {
            if let Some(status) = probe.try_wait().expect("probe status") {
                panic!("probe exited before publishing its daemon: {status}");
            }
            std::fs::read_to_string(&pid_file)
                .ok()
                .map(|pid| pid.trim().parse().expect("daemon pid"))
        },
        || format!("probe did not publish {}", pid_file.display()),
    );
    assert!(process_is_running(daemon_pid), "fixture daemon never ran");

    probe
        .kill_and_wait()
        .expect("SIGKILL the probe test process");
    let deadline = Instant::now() + ORPHAN_EXIT_TIMEOUT;
    while process_is_running(daemon_pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    if process_is_running(daemon_pid) {
        // SAFETY: `daemon_pid` is the orphan this proof must not leak; it
        // leads its own process group.
        unsafe {
            libc::kill(-(daemon_pid as libc::pid_t), libc::SIGKILL);
            libc::kill(daemon_pid as libc::pid_t, libc::SIGKILL);
        }
        panic!(
            "fixture daemon {daemon_pid} outlived its SIGKILLed test process by {ORPHAN_EXIT_TIMEOUT:?}"
        );
    }
}
