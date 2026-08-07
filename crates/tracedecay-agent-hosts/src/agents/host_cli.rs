// Rust guideline compliant 2026-08-08
//! Bounded driver for a host's own plugin-lifecycle CLI.
//!
//! Some hosts own their plugin registration, cache, and enabled state
//! outright. For those, the canonical way to install or remove TraceDecay is
//! the host's own command — not config surgery on state the host considers
//! private. This module is the single boundary through which TraceDecay
//! invokes such a command.
//!
//! Two properties matter and are enforced here rather than at each call site:
//!
//! * the host binary is a **requirement**, not a preference. When it is
//!   missing the lifecycle fails with a typed error naming it. There is no
//!   fallback that edits host-owned files, because a fallback would be exactly
//!   the emulation the host-capability doctrine forbids.
//! * every invocation is **bounded**. A host CLI may prompt, hang on a lock,
//!   or wait on a network fetch; a lifecycle operation must not inherit that.
//!   Output is drained on separate threads so a chatty command cannot deadlock
//!   against a full pipe buffer while we wait.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::errors::{Result, TraceDecayError};

/// Wall-clock bound for one host CLI invocation.
///
/// Generous enough for a marketplace fetch on a slow link, short enough that a
/// wedged command surfaces as a typed failure instead of hanging a CLI the
/// operator is watching.
pub(crate) const HOST_CLI_TIMEOUT: Duration = Duration::from_secs(120);

/// How often the bounded wait re-checks for child exit.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Typed outcome of exactly one host CLI invocation.
///
/// Captured whether the command succeeded or failed: a failing host command is
/// a normal, reportable lifecycle result, and its own stderr is the most
/// useful thing TraceDecay can show the operator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HostCliOutcomeV1 {
    /// Program as invoked, for diagnostics.
    pub program: String,
    /// Arguments as invoked, for diagnostics.
    pub args: Vec<String>,
    /// Exit code, or `None` when the process was signalled or timed out.
    pub status: Option<i32>,
    /// Whether the bounded wait elapsed before the child exited.
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
}

impl HostCliOutcomeV1 {
    /// A clean exit is the only success. Anything else — non-zero, signalled,
    /// or timed out — leaves host state unproven and must not be reported as a
    /// completed lifecycle step.
    pub(crate) fn succeeded(&self) -> bool {
        !self.timed_out && self.status == Some(0)
    }

    /// Operator-facing rendering of a failed invocation, preferring the host's
    /// own stderr over anything TraceDecay could infer.
    pub(crate) fn failure_message(&self) -> String {
        let invocation = if self.args.is_empty() {
            self.program.clone()
        } else {
            format!("{} {}", self.program, self.args.join(" "))
        };
        if self.timed_out {
            return format!(
                "`{invocation}` did not finish within {} seconds; TraceDecay left host state untouched.",
                HOST_CLI_TIMEOUT.as_secs()
            );
        }
        let detail = {
            let stderr = self.stderr.trim();
            let stdout = self.stdout.trim();
            if !stderr.is_empty() {
                stderr
            } else if !stdout.is_empty() {
                stdout
            } else {
                "the command produced no output"
            }
        };
        match self.status {
            Some(code) => format!("`{invocation}` failed with exit code {code}: {detail}"),
            None => format!("`{invocation}` was terminated by a signal: {detail}"),
        }
    }
}

/// Resolve a host lifecycle binary on `PATH`, or fail with the typed
/// requirement error.
///
/// `lifecycle` names the operation family for the message (e.g. "claude
/// plugin lifecycle"), so the operator learns both what is missing and what it
/// was needed for.
pub(crate) fn require_host_cli(program: &str, lifecycle: &str) -> Result<PathBuf> {
    let path_var = std::env::var_os("PATH");
    require_host_cli_from(program, lifecycle, path_var.as_deref())
}

/// [`require_host_cli`] against an explicit `PATH`.
///
/// Split out for the same reason `which_tracedecay_path_from` is: resolution
/// must be testable without mutating process environment shared by every other
/// test running in parallel.
fn require_host_cli_from(
    program: &str,
    lifecycle: &str,
    path_var: Option<&std::ffi::OsStr>,
) -> Result<PathBuf> {
    resolve_on_path(program, path_var).ok_or_else(|| TraceDecayError::Config {
        message: format!(
            "`{program}` binary required for {lifecycle} but was not found on PATH. \
             TraceDecay drives the host's own plugin commands and never edits host-owned \
             plugin state directly; install {program} (or add it to PATH) and retry."
        ),
    })
}

/// First executable match for `program` across `path_var`.
fn resolve_on_path(program: &str, path_var: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    std::env::split_paths(path_var?).find_map(|dir| {
        for name in candidate_file_names(program) {
            let candidate = dir.join(&name);
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }
        None
    })
}

/// Executable spellings to try for a bare program name.
fn candidate_file_names(program: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![
            format!("{program}.exe"),
            format!("{program}.cmd"),
            format!("{program}.bat"),
            program.to_string(),
        ]
    } else {
        vec![program.to_string()]
    }
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

/// Run one host CLI invocation under [`HOST_CLI_TIMEOUT`], capturing its typed
/// outcome.
///
/// `home` is exported as `HOME` so the host CLI operates on the same profile
/// TraceDecay is acting for; this is what lets an isolated-HOME test drive a
/// real lifecycle without touching the operator's own configuration.
pub(crate) fn run_host_cli(program: &Path, args: &[&str], home: &Path) -> Result<HostCliOutcomeV1> {
    let rendered_program = program
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("host cli")
        .to_string();
    let rendered_args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();

    let mut child = Command::new(program)
        .args(args)
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| TraceDecayError::Config {
            message: format!(
                "could not run `{}`: {error}",
                program.display()
            ),
        })?;

    // Drain both pipes concurrently: a command that writes more than one pipe
    // buffer would otherwise block on write while we block on wait.
    let stdout_handle = child.stdout.take().map(spawn_reader);
    let stderr_handle = child.stderr.take().map(spawn_reader);

    let deadline = Instant::now() + HOST_CLI_TIMEOUT;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    timed_out = true;
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(error) => {
                return Err(TraceDecayError::Config {
                    message: format!("could not await `{}`: {error}", program.display()),
                });
            }
        }
    };

    let stdout = stdout_handle.map(join_reader).unwrap_or_default();
    let stderr = stderr_handle.map(join_reader).unwrap_or_default();

    Ok(HostCliOutcomeV1 {
        program: rendered_program,
        args: rendered_args,
        status: status.and_then(|status| status.code()),
        timed_out,
        stdout,
        stderr,
    })
}

fn spawn_reader<R: Read + Send + 'static>(mut source: R) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buffer = String::new();
        // A decode error is not a lifecycle failure; the exit status is the
        // authority and partial output is still useful diagnostics.
        let _ = source.read_to_string(&mut buffer);
        buffer
    })
}

fn join_reader(handle: std::thread::JoinHandle<String>) -> String {
    handle.join().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write an executable shell script usable as a fake host CLI, mirroring
    /// the `install_fake_codex_launcher` pattern used by the root test suite.
    #[cfg(unix)]
    pub(super) fn write_fake_cli(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).expect("write fake host cli");
        let mut permissions = std::fs::metadata(path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("chmod fake host cli");
    }

    #[test]
    fn a_missing_binary_is_a_typed_requirement_naming_the_program() {
        let empty = tempfile::tempdir().unwrap();

        let error = require_host_cli_from(
            "claude",
            "claude plugin lifecycle",
            Some(empty.path().as_os_str()),
        )
        .expect_err("an absent host binary must refuse, never fall back");

        let TraceDecayError::Config { message } = error else {
            panic!("host CLI absence must surface as a config error");
        };
        assert!(
            message.contains("`claude` binary required for claude plugin lifecycle"),
            "the refusal must name the binary and the lifecycle: {message}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_present_binary_resolves_from_the_supplied_path() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude");
        write_fake_cli(&bin, "exit 0");

        let resolved = require_host_cli_from(
            "claude",
            "claude plugin lifecycle",
            Some(dir.path().as_os_str()),
        )
        .expect("an executable on the supplied PATH must resolve");

        assert_eq!(resolved, bin);
    }

    #[cfg(unix)]
    #[test]
    fn a_non_executable_file_does_not_satisfy_the_requirement() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("claude"), b"not executable").unwrap();

        assert!(
            require_host_cli_from(
                "claude",
                "claude plugin lifecycle",
                Some(dir.path().as_os_str()),
            )
            .is_err(),
            "a non-executable file must not be mistaken for the host CLI"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_clean_exit_captures_stdout_and_reports_success() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("faux");
        write_fake_cli(&bin, "echo \"ran: $*\"");

        let outcome = run_host_cli(&bin, &["plugin", "uninstall", "tracedecay"], dir.path())
            .expect("spawning a present binary must not error");

        assert!(outcome.succeeded());
        assert_eq!(outcome.stdout.trim(), "ran: plugin uninstall tracedecay");
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_command_surfaces_the_hosts_own_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("faux");
        write_fake_cli(&bin, "echo 'no such plugin' >&2; exit 3");

        let outcome = run_host_cli(&bin, &["plugin", "uninstall", "tracedecay"], dir.path())
            .expect("a non-zero exit is an outcome, not a spawn error");

        assert!(!outcome.succeeded());
        assert_eq!(outcome.status, Some(3));
        let message = outcome.failure_message();
        assert!(
            message.contains("exit code 3") && message.contains("no such plugin"),
            "the host's own diagnosis must reach the operator: {message}"
        );
    }
}
