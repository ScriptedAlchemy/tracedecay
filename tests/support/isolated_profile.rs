#![allow(dead_code)] // each suite uses a subset of this shared harness

//! Isolated profile environment shared by crate integration suites.
//!
//! The shipped binary must not see the operator's `HOME` or profile. Suites
//! used to copy the same environment set, success check, and env restore.

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::Command;

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

/// Environment override the Work/automation Codex launcher still honours.
/// The isolated profile pins it to a path that cannot exist so a child daemon
/// fails with the typed spawn error instead of resolving the operator's real
/// `codex` from `PATH`. LCM summarizers need no pin: they launch only through
/// the `lcm.summarizer_executables.v1` setting, which defaults to unconfigured.
pub const CODEX_BIN_ENV: &str = "TRACEDECAY_CODEX_BIN";

/// Points a child process at a throwaway home and profile.
///
/// This is the command-env subset shared by daemon journeys. It does not
/// detach the process group or pin `XDG_RUNTIME_DIR`; callers that need the
/// full hermetic daemon environment still use `apply_tracedecay_home_env`.
/// A journey that drives a scripted fake host CLI sets [`CODEX_BIN_ENV`] on
/// the command after this call.
pub fn apply_isolated_profile_env(command: &mut Command, home: &Path, profile: &Path) {
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("TRACEDECAY_DATA_DIR", profile)
        .env("TRACEDECAY_GLOBAL_DB", profile.join("global.db"))
        .env(CODEX_BIN_ENV, profile.join("missing-codex-app-server-binary"))
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
