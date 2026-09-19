//! Helpers forked by a long-lived process must not keep its listen sockets
//! after that process is gone.
//!
//! `fork` duplicates every descriptor. `FD_CLOEXEC` drops them at `exec`, not
//! before, so a child still between those two calls accepts on the parent's
//! socket after the parent has been reaped. Two closures of that window, at
//! the two boundaries that own it:
//!
//! - the child asks the kernel to die with its parent (`PR_SET_PDEATHSIG`),
//!   which covers a supervisor that only knows the leader's pid;
//! - a supervisor that put the leader in its own process group signals that
//!   group, which covers a child that has not reached the death-signal call
//!   and a spawn path (`posix_spawn`) that never runs fork handlers.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

const SIGKILL: i32 = 9;

/// Ask the kernel to kill every later `fork` child if this process dies.
///
/// Linux only. Other Unix targets return success and leave the supervisor's
/// process-group signal as the stop. Idempotent.
///
/// # Errors
///
/// Returns the `pthread_atfork` failure. A daemon that cannot arm this must
/// not start serving: a later force-stop would leave helper children holding
/// the listen socket.
pub fn arm_helper_parent_death() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        arm_linux()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(())
    }
}

/// Signal the process group whose leader is `leader_pid`.
///
/// The leader must have called `setpgid(0, 0)` (or `Command::process_group(0)`).
/// Negating any other pid does not match a group, so this never reaches the
/// caller's own group. `ESRCH` is ignored: the group may already be gone.
pub fn signal_process_group(leader_pid: u32) {
    let Ok(pid) = i32::try_from(leader_pid) else {
        return;
    };
    if pid <= 0 {
        return;
    }
    let Some(group) = pid.checked_neg() else {
        return;
    };
    // SAFETY: `group` is the negation of a pid this process spawned and has
    // not reaped, which is that child's process-group id when it is the
    // leader. A miss returns ESRCH and changes nothing.
    unsafe {
        let _ = kill(group, SIGKILL);
    }
}

#[cfg(target_os = "linux")]
fn arm_linux() -> io::Result<()> {
    static ARMED: AtomicBool = AtomicBool::new(false);
    if ARMED.load(Ordering::Acquire) {
        return Ok(());
    }
    // Record the parent before registering the handler so a child that races
    // the registration still knows which pid should still be its parent.
    EXPECTED_PARENT.store(unsafe { getpid() }, Ordering::Release);
    // SAFETY: the child handler is async-signal-safe (`prctl`, `getppid`,
    // `_exit`, one atomic load). Registering it twice is harmless.
    let rc = unsafe { pthread_atfork(None, None, Some(on_fork_child)) };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc));
    }
    ARMED.store(true, Ordering::Release);
    Ok(())
}

#[cfg(target_os = "linux")]
static EXPECTED_PARENT: AtomicI32 = AtomicI32::new(0);

#[cfg(target_os = "linux")]
unsafe extern "C" fn on_fork_child() {
    let expected = EXPECTED_PARENT.load(Ordering::Acquire);
    // SAFETY: `PR_SET_PDEATHSIG` is process-local. The child calls it before
    // any other work, then exits if the parent already died in the gap.
    unsafe {
        let _ = prctl(PR_SET_PDEATHSIG, SIGKILL as usize, 0, 0, 0);
        if getppid() != expected {
            _exit(1);
        }
    }
}

#[cfg(target_os = "linux")]
const PR_SET_PDEATHSIG: i32 = 1;

unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
    #[cfg(target_os = "linux")]
    fn getpid() -> i32;
    #[cfg(target_os = "linux")]
    fn getppid() -> i32;
    #[cfg(target_os = "linux")]
    fn _exit(status: i32) -> !;
    #[cfg(target_os = "linux")]
    fn prctl(option: i32, arg2: usize, arg3: usize, arg4: usize, arg5: usize) -> i32;
    #[cfg(target_os = "linux")]
    fn pthread_atfork(
        prepare: Option<unsafe extern "C" fn()>,
        parent: Option<unsafe extern "C" fn()>,
        child: Option<unsafe extern "C" fn()>,
    ) -> i32;
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::net::UnixListener;
    use std::os::unix::process::CommandExt;
    use std::path::PathBuf;
    use std::process::{Child, Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{arm_helper_parent_death, signal_process_group};

    const ROLE_ENV: &str = "TRACEDECAY_PROCESS_TREE_ROLE";
    const SOCKET_ENV: &str = "TRACEDECAY_PROCESS_TREE_SOCKET";
    const READY_ENV: &str = "TRACEDECAY_PROCESS_TREE_READY";

    #[test]
    fn helper_between_fork_and_exec_releases_the_listen_socket_when_its_parent_dies() {
        if let Ok(role) = std::env::var(ROLE_ENV) {
            match role.as_str() {
                "leader" => hold_socket_with_paused_child(true),
                "group-leader" => hold_socket_with_paused_child(false),
                other => panic!("unknown process-tree role {other}"),
            }
        }

        let socket = fixture_paths("death");
        let mut leader = spawn_leader("leader", &socket);
        wait_ready(&socket.ready);
        assert!(
            connect_ok(&socket.socket),
            "leader must be accepting before it is killed"
        );
        leader
            .child
            .kill()
            .expect("pid-directed stop of the leader");
        assert!(
            wait_until_refused(&socket.socket, Duration::from_secs(2)),
            "a child still between fork and exec kept the listen socket after the parent died"
        );
    }

    #[test]
    fn signalling_the_leaders_process_group_releases_a_socket_the_child_still_holds() {
        if std::env::var(ROLE_ENV).is_ok() {
            return;
        }
        let socket = fixture_paths("group");
        let mut leader = spawn_leader("group-leader", &socket);
        wait_ready(&socket.ready);
        assert!(connect_ok(&socket.socket), "leader must be accepting");
        let leader_pid = leader.child.id();
        leader
            .child
            .kill()
            .expect("pid-directed stop leaves the helper");
        thread::sleep(Duration::from_millis(150));
        assert!(
            connect_ok(&socket.socket),
            "the paused helper must still hold the listen socket after a pid-only kill"
        );
        signal_process_group(leader_pid);
        assert!(
            wait_until_refused(&socket.socket, Duration::from_secs(2)),
            "signalling the leader's process group must drop the helper's listen socket"
        );
    }

    struct SocketPaths {
        socket: PathBuf,
        ready: PathBuf,
    }

    struct Leader {
        child: Child,
    }

    impl Drop for Leader {
        fn drop(&mut self) {
            signal_process_group(self.child.id());
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    fn fixture_paths(label: &str) -> SocketPaths {
        let dir = std::env::temp_dir().join(format!(
            "tracedecay-process-tree-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("fixture dir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                .expect("fixture dir mode");
        }
        SocketPaths {
            socket: dir.join("daemon.sock"),
            ready: dir.join("ready"),
        }
    }

    fn spawn_leader(role: &str, paths: &SocketPaths) -> Leader {
        let exe = std::env::current_exe().expect("test executable");
        let mut command = Command::new(exe);
        command
            .arg("process_tree::tests::helper_between_fork_and_exec_releases_the_listen_socket_when_its_parent_dies")
            .arg("--exact")
            .arg("--nocapture")
            .env(ROLE_ENV, role)
            .env(SOCKET_ENV, &paths.socket)
            .env(READY_ENV, &paths.ready)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        // The leader has to be its own process-group leader, which is what
        // the daemon harness asks of `daemon run`. Otherwise signalling the
        // negated pid would not name the helper.
        command.process_group(0);
        let child = command.spawn().expect("spawn process-tree leader");
        Leader { child }
    }

    fn wait_ready(ready: &std::path::Path) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !ready.is_file() {
            assert!(
                Instant::now() < deadline,
                "process-tree leader did not become ready"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn connect_ok(socket: &std::path::Path) -> bool {
        std::os::unix::net::UnixStream::connect(socket).is_ok()
    }

    fn wait_until_refused(socket: &std::path::Path, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if !connect_ok(socket) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn hold_socket_with_paused_child(arm: bool) -> ! {
        let socket = std::env::var(SOCKET_ENV).expect("socket path");
        let ready = std::env::var(READY_ENV).expect("ready path");
        if arm {
            arm_helper_parent_death().expect("arm parent death");
        }
        let listener = UnixListener::bind(&socket).expect("bind listen socket");
        let mut pipe_fds = [0i32; 2];
        // SAFETY: `pipe_fds` is a two-int buffer the kernel writes both ends into.
        let pipe_rc = unsafe { pipe(pipe_fds.as_mut_ptr()) };
        assert_eq!(pipe_rc, 0, "pipe");
        let write_fd = pipe_fds[1];
        let read_fd = pipe_fds[0];
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        // SAFETY: `write` and `nanosleep` are async-signal-safe. The closure
        // runs after `fork` and before `exec`, which is the window that still
        // holds `listener`.
        unsafe {
            command.pre_exec(move || {
                let byte = [1u8];
                let _ = write(write_fd, byte.as_ptr(), 1);
                loop {
                    let request = Timespec {
                        tv_sec: 60,
                        tv_nsec: 0,
                    };
                    nanosleep(&raw const request, std::ptr::null_mut());
                }
            });
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        thread::spawn(move || {
            let _ = command.spawn();
        });
        let mut byte = [0u8; 1];
        // SAFETY: `read_fd` is the read end of the pipe just created.
        let n = unsafe { read(read_fd, byte.as_mut_ptr(), 1) };
        assert_eq!(n, 1, "paused child must signal it is between fork and exec");
        std::fs::write(&ready, b"ready").expect("write ready file");
        // Hold the listener for the process lifetime. Dropping it would close
        // the parent's copy and hide a child that still has the other.
        let _listener = listener;
        loop {
            thread::sleep(Duration::from_mins(1));
        }
    }

    #[repr(C)]
    struct Timespec {
        tv_sec: i64,
        tv_nsec: i64,
    }

    unsafe extern "C" {
        fn pipe(fds: *mut i32) -> i32;
        fn read(fd: i32, buf: *mut u8, count: usize) -> isize;
        fn write(fd: i32, buf: *const u8, count: usize) -> isize;
        fn nanosleep(req: *const Timespec, rem: *mut Timespec) -> i32;
    }
}
