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

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::errors::{Result, TraceDecayError};

pub(crate) use tracedecay_automation_runtime::automation::executable_lookup::resolve_on_path;

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
    resolve_on_path(program, path_var)?.ok_or_else(|| TraceDecayError::HostCliUnavailable {
        program: program.to_string(),
        lifecycle: lifecycle.to_string(),
    })
}

/// Spawn a host command, absorbing the transient `ETXTBSY` window that follows
/// a fresh write of the executable.
///
/// Linux refuses `execve` with `ETXTBSY` while *any* process holds the image
/// open for writing — including a process that merely inherited the descriptor
/// across a `fork` and has not reached its own `exec` yet. A lifecycle that
/// drives a host CLI shortly after something installed or updated that binary
/// can therefore be refused for a reason that has nothing to do with the host,
/// and reporting it would blame the host for a race in its installer.
///
/// The retry is deliberately tiny and bounded: the condition clears as soon as
/// the writer's descriptor closes. Every other spawn failure — including a
/// missing or non-executable file — is returned on the first attempt, so no
/// real refusal is delayed or masked.
fn spawn_admitting_recent_writes(command: &mut Command) -> std::io::Result<std::process::Child> {
    const ATTEMPTS: u32 = 5;
    const BACKOFF: Duration = Duration::from_millis(20);

    let mut attempt = 0;
    loop {
        match command.spawn() {
            Ok(child) => return Ok(child),
            Err(error) => {
                let busy = error.raw_os_error() == Some(TEXT_FILE_BUSY);
                attempt += 1;
                if !busy || attempt >= ATTEMPTS {
                    return Err(error);
                }
                std::thread::sleep(BACKOFF);
            }
        }
    }
}

/// `ETXTBSY`. Named rather than matched through `ErrorKind`, which has no
/// stable variant for it.
#[cfg(unix)]
const TEXT_FILE_BUSY: i32 = 26;

#[cfg(not(unix))]
const TEXT_FILE_BUSY: i32 = i32::MIN;

/// Run one host CLI invocation under [`HOST_CLI_TIMEOUT`], capturing its typed
/// outcome.
///
/// `home` is admitted as both `HOME` and the child working directory, and the
/// rest of the environment is cleared. This lets an isolated-HOME test drive a
/// real lifecycle without touching the operator's own configuration or
/// workspace.
#[hotpath::measure(label = "hosts.agent.host_cli.invoke")]
pub(crate) fn run_host_cli(program: &Path, args: &[&str], home: &Path) -> Result<HostCliOutcomeV1> {
    // Resolve the executable before admitting the child working directory.
    // A relative PATH entry is relative to the operator's current directory;
    // once `current_dir(home)` is applied below, asking the OS to resolve it
    // again could launch a different file (or fail despite a successful
    // preflight resolution).
    let resolved_program =
        std::fs::canonicalize(program).map_err(|error| TraceDecayError::Config {
            message: format!("could not resolve `{}`: {error}", program.display()),
        })?;
    let rendered_program = resolved_program
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("host cli")
        .to_string();
    let rendered_args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();

    let (launch_program, launch_args) = resolve_launch_command(&resolved_program)?;
    let mut command = Command::new(&launch_program);
    command
        .args(&launch_args)
        .args(args)
        // Host lifecycle commands must observe the profile and directory the
        // transaction admitted, not the operator's ambient process state.
        .current_dir(home)
        .env_clear()
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    admit_windows_profile_environment(&mut command, home);

    let mut child =
        spawn_admitting_recent_writes(&mut command).map_err(|error| TraceDecayError::Config {
            message: format!("could not run `{}`: {error}", resolved_program.display()),
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
                    message: format!("could not await `{}`: {error}", resolved_program.display()),
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

/// Convert a finished host CLI invocation into success or the host's own
/// diagnosis. Shared by registry-driven MCP steps and simpler plugin steps
/// that only need the typed failure.
pub(crate) fn require_host_cli_success(outcome: HostCliOutcomeV1) -> Result<()> {
    if outcome.succeeded() {
        Ok(())
    } else {
        Err(TraceDecayError::Config {
            message: outcome.failure_message(),
        })
    }
}

/// Render an invocation the way the operator would have typed it.
pub(crate) fn rendered_host_invocation(program: &Path, args: &[&str]) -> String {
    if args.is_empty() {
        program.display().to_string()
    } else {
        format!("{} {}", program.display(), args.join(" "))
    }
}

/// Exact registry-document bytes (absent when nothing is registered yet) and
/// the operator-owned peer MCP server entries read from that document.
type McpConfigObservation = (Option<Vec<u8>>, serde_json::Map<String, serde_json::Value>);

/// Read a host MCP registry document once: exact bytes for rollback, and peer
/// servers for the preservation guard. An absent file is a legitimate empty
/// registry.
fn read_mcp_config_observation(
    path: &Path,
    own_server_name: &str,
    host_label: &str,
) -> Result<McpConfigObservation> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((None, serde_json::Map::new()));
        }
        Err(error) => {
            return Err(TraceDecayError::Config {
                message: format!(
                    "failed to read {} before {host_label}: {error}",
                    path.display()
                ),
            });
        }
    };
    let config = serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|error| {
        TraceDecayError::Config {
            message: format!("failed to parse {} as JSON: {error}", path.display()),
        }
    })?;
    let Some(servers) = config.get("mcpServers") else {
        return Ok((Some(bytes), serde_json::Map::new()));
    };
    let Some(servers) = servers.as_object() else {
        return Err(TraceDecayError::Config {
            message: format!("{}.mcpServers must be a JSON object", path.display()),
        });
    };
    let peers = servers
        .iter()
        .filter(|(name, _)| name.as_str() != own_server_name)
        .map(|(name, server)| (name.clone(), server.clone()))
        .collect();
    Ok((Some(bytes), peers))
}

/// Run one host `mcp …` registry command with the shared peer-preservation
/// guard used by Copilot and Kiro.
///
/// The host owns the registry merge. A buggy or changed command must not
/// silently discard an operator's other MCP servers. The exact post-command
/// bytes are recorded through the active host transaction so rollback can
/// restore the pre-command document when the command fails or a later
/// verification step rejects the effect. Snapshot once after the child exits:
/// reading again after recording would let a foreign writer be absorbed into
/// the transaction's intended state.
#[hotpath::measure(label = "hosts.agent.host_cli.registry_step")]
pub(crate) fn run_mcp_registry_step(
    program: &Path,
    args: &[&str],
    home: &Path,
    mcp_path: &Path,
    own_server_name: &str,
    host_label: &str,
) -> Result<()> {
    run_mcp_registry_step_with_peer_projection(
        program,
        args,
        home,
        mcp_path,
        own_server_name,
        host_label,
        |_| {},
    )
}

/// Run a registry step while comparing peers through a host-specific semantic
/// projection. Most registries use [`run_mcp_registry_step`] for exact values;
/// Copilot uses this boundary because its CLI materializes documented default
/// fields on otherwise unchanged peer registrations.
pub(crate) fn run_mcp_registry_step_with_peer_projection(
    program: &Path,
    args: &[&str],
    home: &Path,
    mcp_path: &Path,
    own_server_name: &str,
    host_label: &str,
    project_peers: impl Fn(&mut serde_json::Map<String, serde_json::Value>),
) -> Result<()> {
    let (_, mut peers_before) = read_mcp_config_observation(mcp_path, own_server_name, host_label)?;
    project_peers(&mut peers_before);
    let outcome = run_host_cli(program, args, home)?;
    let (observed_bytes, mut peers_after) =
        read_mcp_config_observation(mcp_path, own_server_name, host_label)?;
    project_peers(&mut peers_after);
    if peers_before != peers_after {
        return Err(TraceDecayError::Config {
            message: format!(
                "`{}` changed peer MCP servers in {}; TraceDecay left the host state unaccepted",
                rendered_host_invocation(program, args),
                mcp_path.display()
            ),
        });
    }
    crate::agents::record_host_config_observation_bytes(mcp_path, observed_bytes.as_deref())?;
    require_host_cli_success(outcome)
}

/// Resolve a common `#!/usr/bin/env <interpreter>` launcher before clearing
/// the child environment.  `env` needs `PATH` to find its interpreter, but
/// passing the operator's ambient `PATH` through would let the host command
/// select a different executable after admission.  Resolve the interpreter
/// once, then invoke that absolute path with no `PATH` at all.
fn resolve_launch_command(program: &Path) -> Result<(PathBuf, Vec<OsString>)> {
    #[cfg(not(unix))]
    {
        let _ = program;
        return Ok((program.to_path_buf(), Vec::new()));
    }

    #[cfg(unix)]
    {
        let Ok(bytes) = std::fs::read(program) else {
            return Ok((program.to_path_buf(), Vec::new()));
        };
        let Some(first_line) = bytes.split(|byte| *byte == b'\n').next() else {
            return Ok((program.to_path_buf(), Vec::new()));
        };
        let Ok(first_line) = std::str::from_utf8(first_line) else {
            return Ok((program.to_path_buf(), Vec::new()));
        };
        let Some(shebang) = first_line.strip_prefix("#!") else {
            return Ok((program.to_path_buf(), Vec::new()));
        };
        let mut tokens = shebang.split_whitespace();
        let Some(interpreter_launcher) = tokens.next() else {
            return Ok((program.to_path_buf(), Vec::new()));
        };
        if !matches!(interpreter_launcher, "/usr/bin/env" | "/bin/env") {
            return Ok((program.to_path_buf(), Vec::new()));
        }

        // `/usr/bin/env -S node --experimental` is the other spelling emitted
        // by common JavaScript launchers. The bounded parser accepts that
        // interpreter-plus-arguments form, while unsupported env flags and
        // assignments fall back to the kernel's normal shebang error instead
        // of guessing at their semantics.
        let mut interpreter_tokens = tokens.collect::<Vec<_>>();
        let split_mode = interpreter_tokens.first() == Some(&"-S");
        if split_mode {
            interpreter_tokens.remove(0);
        }
        if !split_mode
            && interpreter_tokens
                .iter()
                .any(|token| token.starts_with('-') || token.contains('='))
        {
            return Ok((program.to_path_buf(), Vec::new()));
        }
        let Some(interpreter) = interpreter_tokens.first().copied() else {
            return Ok((program.to_path_buf(), Vec::new()));
        };
        if interpreter.starts_with('-') || interpreter.contains('=') {
            return Ok((program.to_path_buf(), Vec::new()));
        }
        let interpreter_path = resolve_on_path(interpreter, std::env::var_os("PATH").as_deref())?
            .ok_or_else(|| TraceDecayError::Config {
            message: format!(
                "could not resolve env-shebang interpreter `{interpreter}` for `{}` on PATH",
                program.display()
            ),
        })?;
        // Preserve the admitted executable spelling. Multicall launchers such
        // as Volta dispatch from argv[0] (`node`); canonicalizing the final
        // symlink to `volta-shim` invokes a private target that correctly
        // refuses direct execution. Make relative PATH entries absolute before
        // changing the child's working directory, but do not dereference the
        // final executable name.
        let interpreter_path = if interpreter_path.is_absolute() {
            interpreter_path
        } else {
            std::env::current_dir()
                .map_err(TraceDecayError::Io)?
                .join(interpreter_path)
        };
        let mut launch_args = interpreter_tokens[1..]
            .iter()
            .map(OsString::from)
            .collect::<Vec<_>>();
        launch_args.push(program.as_os_str().to_os_string());
        Ok((interpreter_path, launch_args))
    }
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

/// Windows process launchers and host CLIs use `USERPROFILE` as the profile
/// authority. `SystemRoot`/`ComSpec`/`PATHEXT` are platform launch metadata,
/// not operator configuration; preserve them when present so an absolute host
/// executable can still load the normal Windows runtime under the cleared
/// environment. No `PATH` or host-specific profile variable is inherited.
#[cfg(windows)]
fn admit_windows_profile_environment(command: &mut Command, home: &Path) {
    command.env("USERPROFILE", home);
    for key in ["SystemRoot", "ComSpec", "PATHEXT"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
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

        let TraceDecayError::HostCliUnavailable { program, lifecycle } = error else {
            panic!("host CLI absence must surface as a typed requirement");
        };
        assert_eq!(program, "claude");
        assert_eq!(lifecycle, "claude plugin lifecycle");
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
        let candidate = dir.path().join("claude");
        std::fs::write(&candidate, b"not executable").unwrap();

        let error = require_host_cli_from(
            "claude",
            "claude plugin lifecycle",
            Some(dir.path().as_os_str()),
        )
        .expect_err("a non-executable PATH candidate must refuse");

        let TraceDecayError::Config { message } = error else {
            panic!(
                "a present non-executable candidate must not become HostCliUnavailable: {error}"
            );
        };
        assert!(
            message.contains(&candidate.display().to_string()),
            "the typed failure must identify the unusable PATH candidate: {message}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_path_metadata_failure_is_not_relabelled_as_host_cli_absence() {
        let dir = tempfile::tempdir().unwrap();
        let non_directory = dir.path().join("not-a-directory");
        std::fs::write(&non_directory, b"not a directory").unwrap();

        let error = require_host_cli_from(
            "claude",
            "claude plugin lifecycle",
            Some(non_directory.as_os_str()),
        )
        .expect_err("a broken PATH entry must preserve its filesystem failure");

        assert!(
            matches!(error, TraceDecayError::Io(_)),
            "only an exhausted search may become HostCliUnavailable: {error}"
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

    #[cfg(unix)]
    #[test]
    fn child_receives_only_the_admitted_profile_and_working_directory() {
        struct AmbientKiroHomeGuard {
            previous: Option<std::ffi::OsString>,
            _lock: std::sync::MutexGuard<'static, ()>,
        }

        impl AmbientKiroHomeGuard {
            fn set(value: &Path) -> Self {
                let lock = crate::config::lock_user_data_dir_test_env();
                let previous = std::env::var_os("KIRO_HOME");
                // SAFETY: the shared profile-discovery lock is held for the
                // guard's lifetime, so no sibling profile test observes this
                // temporary ambient value.
                unsafe {
                    std::env::set_var("KIRO_HOME", value);
                }
                Self {
                    previous,
                    _lock: lock,
                }
            }
        }

        impl Drop for AmbientKiroHomeGuard {
            fn drop(&mut self) {
                // SAFETY: see `AmbientKiroHomeGuard::set`.
                unsafe {
                    match self.previous.take() {
                        Some(previous) => std::env::set_var("KIRO_HOME", previous),
                        None => std::env::remove_var("KIRO_HOME"),
                    }
                }
            }
        }

        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join("faux");
        let ambient = tempfile::tempdir().unwrap();
        let _ambient = AmbientKiroHomeGuard::set(ambient.path());
        write_fake_cli(
            &bin,
            r#"
printf '%s' "${KIRO_HOME-<unset>}" > "$HOME/kiro-home"
printf '%s' "${PATH-<unset>}" > "$HOME/path"
pwd > "$HOME/cwd"
printf '%s' "$HOME" > "$HOME/home"
"#,
        );

        let outcome = run_host_cli(&bin, &["mcp", "add"], home.path())
            .expect("the isolated fake host command must launch");
        assert!(outcome.succeeded());
        assert_eq!(
            std::fs::read_to_string(home.path().join("kiro-home")).unwrap(),
            "<unset>"
        );
        // `PATH` cannot be probed for absence the way `KIRO_HOME` can: a POSIX
        // shell assigns itself a default `PATH` when it starts without one, so
        // a `#!/bin/sh` probe reports that synthesized default rather than
        // `<unset>` even though `env_clear` did remove the variable. What the
        // admission actually promises — and what this asserts — is that the
        // *ambient* value did not reach the child.
        let observed = std::fs::read_to_string(home.path().join("path")).unwrap();
        let ambient = std::env::var("PATH").unwrap_or_default();
        assert_ne!(
            observed, ambient,
            "the ambient PATH must not reach the child"
        );
        assert_eq!(
            std::fs::read_to_string(home.path().join("home")).unwrap(),
            home.path().to_string_lossy().to_string()
        );
        assert_eq!(
            std::fs::canonicalize(home.path()).unwrap(),
            std::fs::canonicalize(
                std::fs::read_to_string(home.path().join("cwd"))
                    .unwrap()
                    .trim()
            )
            .unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn env_shebang_interpreter_is_resolved_before_ambient_path_is_cleared() {
        use std::os::unix::fs::PermissionsExt;

        struct PathGuard {
            previous: Option<std::ffi::OsString>,
            _lock: std::sync::MutexGuard<'static, ()>,
        }

        impl PathGuard {
            fn set(path: &std::ffi::OsStr) -> Self {
                let lock = crate::config::lock_user_data_dir_test_env();
                let previous = std::env::var_os("PATH");
                // SAFETY: the shared profile-discovery lock serializes this
                // process-global test environment mutation.
                unsafe { std::env::set_var("PATH", path) };
                Self {
                    previous,
                    _lock: lock,
                }
            }
        }

        impl Drop for PathGuard {
            fn drop(&mut self) {
                // SAFETY: see `PathGuard::set`.
                unsafe {
                    match self.previous.take() {
                        Some(previous) => std::env::set_var("PATH", previous),
                        None => std::env::remove_var("PATH"),
                    }
                }
            }
        }

        let home = tempfile::tempdir().unwrap();
        let node_dir = tempfile::tempdir().unwrap();
        let attacker_dir = tempfile::tempdir().unwrap();
        let node = node_dir.path().join("node");
        std::fs::write(
            &node,
            r#"#!/bin/sh
printf '%s' "$*" > "$HOME/node-args"
printf '%s' "${PATH-<unset>}" > "$HOME/node-path"
exit 0
"#,
        )
        .unwrap();
        let mut node_permissions = std::fs::metadata(&node).unwrap().permissions();
        node_permissions.set_mode(0o755);
        std::fs::set_permissions(&node, node_permissions).unwrap();

        let attacker = attacker_dir.path().join("attacker");
        std::fs::write(
            &attacker,
            "#!/bin/sh\nprintf '%s' invoked > \"$HOME/attacker-ran\"\n",
        )
        .unwrap();
        let mut attacker_permissions = std::fs::metadata(&attacker).unwrap().permissions();
        attacker_permissions.set_mode(0o755);
        std::fs::set_permissions(&attacker, attacker_permissions).unwrap();

        let launcher = home.path().join("kiro-cli");
        std::fs::write(&launcher, "#!/usr/bin/env node\n").unwrap();
        let mut launcher_permissions = std::fs::metadata(&launcher).unwrap().permissions();
        launcher_permissions.set_mode(0o755);
        std::fs::set_permissions(&launcher, launcher_permissions).unwrap();

        let path = std::env::join_paths([node_dir.path(), attacker_dir.path()]).unwrap();
        let _path = PathGuard::set(&path);
        let outcome = run_host_cli(&launcher, &["mcp", "add"], home.path())
            .expect("env-shebang launchers must run after interpreter admission");

        assert!(
            outcome.succeeded(),
            "resolved interpreter must exit cleanly"
        );
        assert_eq!(
            std::fs::read_to_string(home.path().join("node-args")).unwrap(),
            format!(
                "{} mcp add",
                std::fs::canonicalize(&launcher).unwrap().display()
            )
        );
        // As above, `<unset>` is not observable through a `#!/bin/sh` probe:
        // the shell synthesizes a default `PATH` when it inherits none. The
        // guarantee under test is that neither ambient entry survived — not
        // the attacker directory, and not even the directory the interpreter
        // itself was resolved from, because the parent resolves it once and
        // passes an absolute path rather than letting the child re-resolve.
        let observed = std::fs::read_to_string(home.path().join("node-path")).unwrap();
        for ambient_entry in [attacker_dir.path(), node_dir.path()] {
            assert!(
                !observed.contains(&*ambient_entry.to_string_lossy()),
                "ambient PATH entry {} must not reach the child (observed {observed})",
                ambient_entry.display()
            );
        }
        assert!(!home.path().join("attacker-ran").exists());
    }

    #[cfg(unix)]
    #[test]
    fn env_shebang_interpreter_preserves_multicall_symlink_name() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        struct PathGuard {
            previous: Option<std::ffi::OsString>,
            _lock: std::sync::MutexGuard<'static, ()>,
        }

        impl PathGuard {
            fn set(path: &std::ffi::OsStr) -> Self {
                let lock = crate::config::lock_user_data_dir_test_env();
                let previous = std::env::var_os("PATH");
                // SAFETY: the shared profile-discovery lock serializes this
                // process-global test environment mutation.
                unsafe { std::env::set_var("PATH", path) };
                Self {
                    previous,
                    _lock: lock,
                }
            }
        }

        impl Drop for PathGuard {
            fn drop(&mut self) {
                // SAFETY: see `PathGuard::set`.
                unsafe {
                    match self.previous.take() {
                        Some(previous) => std::env::set_var("PATH", previous),
                        None => std::env::remove_var("PATH"),
                    }
                }
            }
        }

        let home = tempfile::tempdir().unwrap();
        let bin_dir = tempfile::tempdir().unwrap();
        let shim = bin_dir.path().join("volta-shim");
        std::fs::write(
            &shim,
            r#"#!/bin/sh
if [ "$(basename "$0")" != node ]; then
  echo "'volta-shim' should not be called directly" >&2
  exit 126
fi
printf '%s' "$*" > "$HOME/node-args"
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&shim).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&shim, permissions).unwrap();
        symlink(&shim, bin_dir.path().join("node")).unwrap();

        let launcher = home.path().join("codex");
        std::fs::write(&launcher, "#!/usr/bin/env node\n").unwrap();
        let mut permissions = std::fs::metadata(&launcher).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&launcher, permissions).unwrap();

        let path = std::env::join_paths([bin_dir.path()]).unwrap();
        let _path = PathGuard::set(&path);
        let outcome = run_host_cli(&launcher, &["plugin", "add"], home.path())
            .expect("the admitted multicall interpreter must launch");

        assert!(
            outcome.succeeded(),
            "the interpreter must observe its admitted symlink name: {}",
            outcome.failure_message()
        );
        assert_eq!(
            std::fs::read_to_string(home.path().join("node-args")).unwrap(),
            format!(
                "{} plugin add",
                std::fs::canonicalize(&launcher).unwrap().display()
            )
        );
    }
}
