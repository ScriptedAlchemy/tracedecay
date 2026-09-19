//! Process-group stop must release a listen socket held by a descendant.
//!
//! `process_group(0)` makes the spawned child its own group leader. Killing
//! only that pid leaves the descendant that inherited the listen descriptor,
//! and the path stays connectable after `wait` returns.

use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::common::{TestChildProcess, poll_until};

/// How long the released descendant has to finish dying.
///
/// The group signal is delivered to a process the harness cannot `wait` on -
/// the descendant is reparented, not a child - so its descriptors close when
/// the kernel finishes tearing it down, not when the leader's `wait` returns.
/// A descendant that was never signaled keeps accepting past this deadline,
/// which is the regression this proof exists to catch.
const DESCENDANT_RELEASE_TIMEOUT: Duration = Duration::from_secs(10);

const HOLDER: &str = r#"
import os, socket, time
path = os.environ["TRACEDECAY_TEST_SOCKET_PATH"]
listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
listener.bind(path)
listener.listen(1)
os.fork()
while True:
    time.sleep(60)
"#;

#[test]
fn group_stop_releases_an_inherited_listen_socket() {
    let scratch = tempfile::tempdir().expect("socket scratch");
    let socket = scratch.path().join("daemon.sock");
    let mut command = Command::new("python3");
    command
        .arg("-c")
        .arg(HOLDER)
        .env("TRACEDECAY_TEST_SOCKET_PATH", &socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.process_group(0);
    let child = command.spawn().expect("spawn socket holder");
    // Do not record a socket path: connect must fail because the group is
    // dead, not because the path was unlinked.
    let mut holder = TestChildProcess::new(child);

    let ready_deadline = Instant::now() + Duration::from_secs(5);
    while UnixStream::connect(&socket).is_err() {
        assert!(
            Instant::now() < ready_deadline,
            "holder did not bind {}",
            socket.display()
        );
        if holder.try_wait().expect("holder status").is_some() {
            panic!("socket holder exited before binding");
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    holder.kill_and_wait().expect("reap socket holder group");
    assert!(
        socket.exists(),
        "this proof must not delete the socket path"
    );
    poll_until(
        Instant::now() + DESCENDANT_RELEASE_TIMEOUT,
        Duration::from_millis(20),
        || UnixStream::connect(&socket).is_err().then_some(()),
        || {
            format!(
                "process-group stop must release the inherited listen socket at {}",
                socket.display()
            )
        },
    );
}

#[test]
fn stop_unlinks_the_socket_path_the_child_published() {
    let scratch = tempfile::tempdir().expect("socket scratch");
    let socket = scratch.path().join("daemon.sock");
    std::os::unix::net::UnixListener::bind(&socket).expect("bind socket");
    let mut command = Command::new("python3");
    command
        .arg("-c")
        .arg("import time; time.sleep(60)")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.process_group(0);
    let child = command.spawn().expect("spawn sleeper");
    let mut sleeper = TestChildProcess::new(child);
    sleeper.release_socket_on_stop(socket.clone());
    drop(sleeper);
    assert!(
        !socket.exists(),
        "stopping the child must unlink the socket path it published"
    );
}
