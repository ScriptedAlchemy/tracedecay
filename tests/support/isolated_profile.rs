#![allow(dead_code)] // each suite uses a subset of this shared harness

//! Isolated profile environment shared by crate integration suites.
//!
//! The shipped binary must not see the operator's `HOME` or profile. Suites
//! used to copy the same environment set, success check, and env restore.

use std::ffi::{OsStr, OsString};
#[cfg(target_os = "linux")]
use std::ffi::{c_int, c_ulong};
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

#[cfg(target_os = "linux")]
unsafe extern "C" {
    fn prctl(option: c_int, ...) -> c_int;
}

/// Makes the kernel SIGKILL the spawned child when the test process dies.
///
/// Fixture guards reap children on return and unwind, but a harness timeout
/// or flake runner that SIGKILLs the test binary runs no `Drop`, and a child
/// in its own process group also escapes the harness's group signal. Such
/// daemons used to survive for days.
///
/// `PR_SET_PDEATHSIG` fires when the spawning *thread* exits (prctl(2)), so a
/// bound child must be spawned from a thread that outlives it, never from a
/// Tokio blocking-pool worker that is reaped after going idle.
pub fn die_with_test_process(command: &mut Command) {
    #[cfg(target_os = "linux")]
    {
        const PR_SET_PDEATHSIG: c_int = 1;
        const SIGKILL: c_ulong = 9;
        let spawner = std::process::id();
        // SAFETY: the hook runs between fork and exec and issues only the
        // async-signal-safe `prctl` and `getppid` syscalls, without allocating.
        unsafe {
            command.pre_exec(move || {
                if prctl(PR_SET_PDEATHSIG, SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // A spawner that died before the signal was armed never
                // delivers it; refuse to start an already-orphaned child.
                if std::os::unix::process::parent_id() != spawner {
                    return Err(std::io::ErrorKind::BrokenPipe.into());
                }
                Ok(())
            });
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = command;
}

/// Restores one process environment variable when the guard drops.
pub struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    pub fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let previous = std::env::var_os(key);
        // SAFETY: callers that pin process-wide env hold that binary's env
        // lock for the guard's whole life.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }

    /// Removes `key` for the guard's lifetime, so tests can exercise the
    /// no-override path.
    pub fn unset(key: &'static str) -> Self {
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

/// Points a child process at a throwaway home and profile.
///
/// This is the command-env subset shared by daemon journeys. It does not
/// detach the process group or pin `XDG_RUNTIME_DIR`; callers that need the
/// full hermetic daemon environment still use `apply_tracedecay_home_env`.
/// Host CLIs launch only through the `lcm.summarizer_executables.v1` setting,
/// which defaults to unconfigured, so no executable pin is needed here.
pub fn apply_isolated_profile_env(command: &mut Command, home: &Path, profile: &Path) {
    die_with_test_process(command);
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("TRACEDECAY_DATA_DIR", profile)
        .env("TRACEDECAY_GLOBAL_DB", profile.join("global.db"))
        .env("TRACEDECAY_TEST_ALLOW_INCOMPLETE_HOLDER_SCAN", "1");
}

/// Runs a command and returns stdout, panicking with both streams on failure.
pub fn run_ok(command: &mut Command, label: &str) -> Vec<u8> {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("{label} could not run: {error}"));
    assert!(
        output.status.success(),
        "{label} failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
