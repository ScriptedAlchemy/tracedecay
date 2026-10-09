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
use std::path::{Path, PathBuf};
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

/// Variables that relocate a host's config root; inherited, any of them would
/// point a sandboxed child at the operator's real host state.
const HOST_RELOCATION_ENV: &[&str] = &[
    "CODEX_HOME",
    "CLAUDE_CONFIG_DIR",
    "KIMI_CODE_HOME",
    "KIRO_HOME",
    "PI_CODING_AGENT_DIR",
    "DBUS_SESSION_BUS_ADDRESS",
];

/// System directories a hermetic child may search: they carry `sh`, `git`,
/// and coreutils, and no agent-host CLI.
fn system_path_dirs() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        let root = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let mut dirs = vec![root.join("System32"), root];
        // Git for Windows is needed by child processes that spawn `git` (the
        // daemon's Git-correlation reads). Bazel test environments may not
        // carry `ProgramFiles`, so fall back to the default install location;
        // probe the binary so a missing install adds no dead PATH entry.
        let program_files = std::env::var_os("ProgramFiles")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
        let git_cmd = program_files.join("Git").join("cmd");
        if git_cmd.join("git.exe").is_file() {
            dirs.push(git_cmd);
        }
        dirs
    }
    #[cfg(not(windows))]
    {
        vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin")]
    }
}

/// A child `PATH` of exactly `fake_bin_dirs`, then the system directories.
///
/// Never derived from the inherited `PATH`: that is where the operator's real
/// host CLIs live (`~/.local/bin/kimi`, mise/asdf shims, Homebrew), and a
/// test must not be one lookup away from launching them.
pub fn hermetic_path<P: AsRef<Path>>(fake_bin_dirs: &[P]) -> OsString {
    let dirs = fake_bin_dirs
        .iter()
        .map(|dir| dir.as_ref().to_path_buf())
        .chain(system_path_dirs());
    std::env::join_paths(dirs).expect("hermetic PATH entries must be joinable")
}

/// Sandboxes a child's host-facing environment under `home`: `HOME`, every
/// XDG root, a [`hermetic_path`] with no fake hosts, and no host relocation.
///
/// Tests that provide fake host CLIs override `PATH` afterwards with
/// `hermetic_path(&[fake_bin_dir])`.
pub fn apply_hermetic_child_env(command: &mut Command, home: &Path) {
    for key in HOST_RELOCATION_ENV {
        command.env_remove(key);
    }
    // The operator's logging config is not fixture state: hook stderr belongs
    // to its host and stays silent unless `RUST_LOG` overrides it, so a test
    // wanting child logs sets `RUST_LOG` explicitly after this helper.
    command.env_remove("RUST_LOG");
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        // A child resolves its installed unit file under `XDG_CONFIG_HOME` and
        // reaches the user service manager through `XDG_RUNTIME_DIR`; both stay
        // inside the isolated home so it can never stop the real
        // `tracedecay.service`.
        .env("XDG_RUNTIME_DIR", home.join("run"))
        // Host discovery may invoke gh; its telemetry must not write a device
        // identity into the fixture home or send fixture activity upstream.
        .env("GH_TELEMETRY", "0")
        .env("PATH", hermetic_path::<&Path>(&[]));
}

/// Points a child process at a throwaway home and profile.
///
/// This is the command-env subset shared by daemon journeys. It does not
/// detach the process group; callers that need the full hermetic daemon
/// environment still use `apply_tracedecay_home_env`.
pub fn apply_isolated_profile_env(command: &mut Command, home: &Path, profile: &Path) {
    die_with_test_process(command);
    apply_hermetic_child_env(command, home);
    command
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
