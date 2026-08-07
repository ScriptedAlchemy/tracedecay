//! Process-wide resolution of the `git` binary.
//!
//! The daemon and CLI spawn `git` from ~13 sites. A bare `Command::new("git")`
//! makes the OS re-walk `PATH` on every spawn — cheap on Linux/macOS but
//! ~100-300ms per spawn on Windows. This module resolves the `git` binary to an
//! absolute path exactly once (cached in a [`OnceLock`]) and hands every product
//! spawn site that cached path, so the long-running daemon never re-walks `PATH`.
//!
//! The read authority in [`crate::git_repository`] uses this program only for
//! the bounded linked-worktree symbolic-HEAD fallback, and the topology reads
//! in [`crate::branch`] and [`crate::worktree`] are fully in-process. Other
//! callers use the bounded CLI fallback here for native Git writes, signing,
//! recovery, and reads where exact porcelain semantics remain the authority.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::cancellation::CancellationToken;

/// The literal used when resolution fails, preserving today's behavior (the OS
/// PATH-walks per spawn, but callers keep working).
const GIT_LITERAL: &str = "git";
const GIT_CAPTURE_AT_TIMEOUT: Duration = Duration::from_secs(2);
const CHILD_WAIT_POLL_INTERVAL: Duration = Duration::from_millis(10);
const DEFAULT_READ_DEADLINE: Duration = Duration::from_secs(10);
const DEFAULT_STDOUT_LIMIT: usize = 16 * 1024 * 1024;
const DEFAULT_STDERR_LIMIT: usize = 64 * 1024;

/// Execution bounds for one read-only Git subprocess.
#[derive(Clone, Debug)]
pub struct GitCommandBounds {
    pub deadline: Instant,
    pub cancel: Option<CancellationToken>,
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
}

impl Default for GitCommandBounds {
    fn default() -> Self {
        Self {
            deadline: Instant::now() + DEFAULT_READ_DEADLINE,
            cancel: None,
            max_stdout_bytes: DEFAULT_STDOUT_LIMIT,
            max_stderr_bytes: DEFAULT_STDERR_LIMIT,
        }
    }
}

/// Typed failure from a bounded Git subprocess read.
#[derive(Debug, thiserror::Error)]
pub enum GitCommandError {
    #[error("git executable unavailable: {0}")]
    Unavailable(#[source] std::io::Error),
    #[error("git read cancelled")]
    Cancelled,
    #[error("git read deadline exceeded")]
    DeadlineExceeded,
    #[error("git {stream} output exceeded {bound} bytes")]
    OutputLimitExceeded { stream: &'static str, bound: usize },
    #[error("failed to read git {stream}: {source}")]
    ReadOutput {
        stream: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to wait for git: {0}")]
    Wait(#[source] std::io::Error),
}

/// Returns the resolved `git` program to spawn, as a cached `&'static OsStr`.
///
/// Resolution order (performed once, then cached):
///   1. The `GIT` environment variable, if set and non-empty (explicit override,
///      matching git's own habit of honoring a program override).
///   2. An absolute path found by a which-style walk of `PATH` (+ `PATHEXT` on
///      Windows).
///   3. The literal `"git"` fallback, so behavior is never worse than a bare
///      `Command::new("git")`.
///
/// Callers pass the result straight to `Command::new(..)` (both `std` and
/// `tokio` accept `impl AsRef<OsStr>`).
pub fn git_program() -> &'static OsStr {
    static PROGRAM: OnceLock<OsString> = OnceLock::new();
    PROGRAM.get_or_init(resolve_git_program).as_os_str()
}

fn resolve_git_program() -> OsString {
    // 1. Explicit override wins. Empty values are ignored so an accidental
    //    `GIT=` does not break spawns.
    if let Some(value) = std::env::var_os("GIT")
        && !value.is_empty()
    {
        return value;
    }

    // 2. which-style lookup over PATH (+ PATHEXT on Windows).
    if let Some(path) = find_in_path(GIT_LITERAL) {
        return path.into_os_string();
    }

    // 3. Fallback: let the OS resolve it per-spawn, as before.
    OsString::from(GIT_LITERAL)
}

/// Minimal `which`-style lookup: find `name` as an executable on `PATH`.
///
/// On Windows, each `PATH` entry is probed with every `PATHEXT` suffix (and the
/// bare name) so `git.exe` resolves from `git`. On Unix, the bare name is probed
/// and the entry must be a file (execute-permission is not separately checked —
/// git's own PATH lookup does not either, and a false positive simply degrades to
/// today's per-spawn PATH walk on exec failure).
fn find_in_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        if let Some(found) = probe_dir(&dir, name) {
            return Some(found);
        }
    }
    None
}

#[cfg(windows)]
fn probe_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    // PATHEXT holds the executable suffixes (";"-separated), e.g.
    // ".COM;.EXE;.BAT;.CMD". Fall back to a sane default when unset.
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());

    // If the name already carries an extension, try it verbatim first.
    let bare = dir.join(name);
    if bare.is_file() {
        return Some(bare);
    }
    for ext in pathext.split(';') {
        let ext = ext.trim();
        if ext.is_empty() {
            continue;
        }
        let candidate = dir.join(format!("{name}{ext}"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(not(windows))]
fn probe_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    let candidate = dir.join(name);
    candidate.is_file().then_some(candidate)
}

/// Runs `git <args>` in `repo_root` with the resolved [`git_program`], returning
/// the command [`Output`] on a zero exit status, or `None` on spawn failure or a
/// non-zero exit. Use this when the raw, untrimmed stdout matters (multi-line
/// output such as `git reflog` or `git log`).
pub fn git_output(repo_root: &Path, args: &[&str]) -> Option<Output> {
    let output = bounded_git_output(repo_root, args, &GitCommandBounds::default()).ok()?;
    output.status.success().then_some(output)
}

/// Runs `git <args>` with bounded output, cooperative cancellation, and an
/// in-flight deadline. Pipes are drained concurrently so stderr cannot
/// deadlock a stdout-heavy read, while retained bytes remain bounded.
pub fn bounded_git_output(
    repo_root: &Path,
    args: &[&str],
    bounds: &GitCommandBounds,
) -> Result<Output, GitCommandError> {
    let mut command = Command::new(git_program());
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(&key);
        }
    }
    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .args(args)
        .current_dir(repo_root);
    bounded_command_output(command, None, bounds)
}

/// Runs an already-configured command with bounded output and request controls.
///
/// The caller owns the executable, arguments, working directory, and
/// environment. This function owns only process I/O, cancellation, deadline,
/// and termination. [`bounded_git_output`] is the read-only Git wrapper that
/// applies TraceDecay's ambient-environment sanitization.
pub fn bounded_command_output(
    command: Command,
    stdin: Option<&[u8]>,
    bounds: &GitCommandBounds,
) -> Result<Output, GitCommandError> {
    if bounds
        .cancel
        .as_ref()
        .is_some_and(CancellationToken::is_cancelled)
    {
        return Err(GitCommandError::Cancelled);
    }
    if Instant::now() >= bounds.deadline {
        return Err(GitCommandError::DeadlineExceeded);
    }

    let input = stdin.map(<[u8]>::to_vec);
    let bounds = bounds.clone();
    std::thread::scope(|scope| {
        scope
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_io()
                    .enable_time()
                    .build()
                    .map_err(GitCommandError::Wait)?;
                runtime.block_on(run_bounded_command(command, input, bounds))
            })
            .join()
            .map_err(|_| {
                GitCommandError::Wait(std::io::Error::other("bounded command supervisor panicked"))
            })?
    })
}

struct BoundedRead {
    bytes: Vec<u8>,
    exceeded: bool,
}

async fn read_bounded_output(
    mut reader: impl tokio::io::AsyncRead + Unpin,
    stream: &'static str,
    limit: usize,
    limit_sender: tokio::sync::mpsc::UnboundedSender<(&'static str, usize)>,
) -> std::io::Result<BoundedRead> {
    use tokio::io::AsyncReadExt;

    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut buffer = [0u8; 8 * 1024];
    let mut over_limit = false;
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_sub(bytes.len());
        bytes.extend_from_slice(&buffer[..count.min(remaining)]);
        if count > remaining && !over_limit {
            over_limit = true;
            let _ = limit_sender.send((stream, limit));
        }
    }
    Ok(BoundedRead {
        bytes,
        exceeded: over_limit,
    })
}

async fn join_reader(
    reader: tokio::task::JoinHandle<std::io::Result<BoundedRead>>,
    stream: &'static str,
) -> Result<BoundedRead, GitCommandError> {
    reader
        .await
        .map_err(|_| GitCommandError::ReadOutput {
            stream,
            source: std::io::Error::other("git output reader panicked"),
        })?
        .map_err(|source| GitCommandError::ReadOutput { stream, source })
}

async fn run_bounded_command(
    command: Command,
    input: Option<Vec<u8>>,
    bounds: GitCommandBounds,
) -> Result<Output, GitCommandError> {
    use tokio::io::AsyncWriteExt;

    let has_input = input.is_some();
    let mut command = tokio::process::Command::from(command);
    let mut child = command
        .stdin(if has_input {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(GitCommandError::Unavailable)?;
    let stdout = child.stdout.take().ok_or_else(|| {
        GitCommandError::Unavailable(std::io::Error::other("missing stdout pipe"))
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        GitCommandError::Unavailable(std::io::Error::other("missing stderr pipe"))
    })?;
    let (limit_sender, mut limit_receiver) = tokio::sync::mpsc::unbounded_channel();
    let _limit_sender_guard = limit_sender.clone();
    let stdout_reader = tokio::spawn(read_bounded_output(
        stdout,
        "stdout",
        bounds.max_stdout_bytes,
        limit_sender.clone(),
    ));
    let stderr_reader = tokio::spawn(read_bounded_output(
        stderr,
        "stderr",
        bounds.max_stderr_bytes,
        limit_sender,
    ));
    let input_writer = match input {
        Some(input) => {
            let mut stdin = child.stdin.take().ok_or_else(|| {
                GitCommandError::Unavailable(std::io::Error::other("missing stdin pipe"))
            })?;
            Some(tokio::spawn(async move {
                stdin.write_all(&input).await?;
                stdin.shutdown().await
            }))
        }
        None => None,
    };

    let cancellation = bounds.cancel.clone();
    let deadline = tokio::time::Instant::from_std(bounds.deadline);
    let process_outcome = tokio::select! {
        status = child.wait() => status.map_err(GitCommandError::Wait),
        () = wait_for_cancellation(cancellation) => Err(GitCommandError::Cancelled),
        () = tokio::time::sleep_until(deadline) => Err(GitCommandError::DeadlineExceeded),
        exceeded = limit_receiver.recv() => {
            let (stream, bound) = exceeded.unwrap_or((
                "output",
                bounds.max_stdout_bytes.max(bounds.max_stderr_bytes),
            ));
            Err(GitCommandError::OutputLimitExceeded { stream, bound })
        }
    };

    if process_outcome.is_err() {
        terminate_child(&mut child).await;
    }
    if let Some(writer) = input_writer {
        let _ = writer.await;
    }
    let stdout = join_reader(stdout_reader, "stdout").await?;
    let stderr = join_reader(stderr_reader, "stderr").await?;
    let status = process_outcome?;
    if stdout.exceeded {
        return Err(GitCommandError::OutputLimitExceeded {
            stream: "stdout",
            bound: bounds.max_stdout_bytes,
        });
    }
    if stderr.exceeded {
        return Err(GitCommandError::OutputLimitExceeded {
            stream: "stderr",
            bound: bounds.max_stderr_bytes,
        });
    }
    Ok(Output {
        status,
        stdout: stdout.bytes,
        stderr: stderr.bytes,
    })
}

async fn wait_for_cancellation(cancel: Option<CancellationToken>) {
    match cancel {
        Some(cancel) => cancel.cancelled().await,
        None => std::future::pending().await,
    }
}

async fn terminate_child(child: &mut tokio::process::Child) {
    let _ = child.start_kill();
    let _ = child.wait().await;
}

/// Runs `git <args>` in `repo_root` and returns the trimmed stdout as a
/// `String`, or `None` on spawn failure, non-zero exit, non-UTF-8 output, or
/// empty (after trimming) output. Convenience wrapper over [`git_output`] for
/// the common single-value reads (`rev-parse`, `config --get`, ...).
pub fn git_capture(repo_root: &Path, args: &[&str]) -> Option<String> {
    let output = git_output(repo_root, args)?;
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Outcome of the bounded `git -C` capture used by repository identity lookup.
#[derive(Debug)]
pub enum GitCaptureAtResult {
    Captured(String),
    Failed,
    TimedOut,
}

/// Runs `git -C <repo_root> <args>` without setting the child process working
/// directory to `repo_root`.
///
/// Some network-backed or otherwise unhealthy project roots can block inside
/// the child's initial `getcwd` when passed through [`Command::current_dir`].
/// Git's `-C` resolves the repository after process startup and avoids that
/// pre-argument cwd lookup. The child is killed and reaped at the hard deadline.
pub fn git_capture_at(repo_root: &Path, args: &[&str]) -> GitCaptureAtResult {
    let mut command = git_command_at(repo_root, args);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let Ok(child) = command.spawn() else {
        return GitCaptureAtResult::Failed;
    };
    match capture_child_with_deadline(child, GIT_CAPTURE_AT_TIMEOUT) {
        ChildCaptureResult::Completed(output) if output.status.success() => {
            let Ok(text) = String::from_utf8(output.stdout) else {
                return GitCaptureAtResult::Failed;
            };
            let trimmed = text.trim();
            if trimmed.is_empty() {
                GitCaptureAtResult::Failed
            } else {
                GitCaptureAtResult::Captured(trimmed.to_string())
            }
        }
        ChildCaptureResult::TimedOut => GitCaptureAtResult::TimedOut,
        ChildCaptureResult::Completed(_) | ChildCaptureResult::Failed => GitCaptureAtResult::Failed,
    }
}

fn git_command_at(repo_root: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(git_program());
    // Repository selection must come from `-C <repo_root>`, never from
    // overrides inherited from the daemon's own environment: an inherited
    // GIT_DIR would silently resolve every probed path to the same repo.
    command.env_remove("GIT_DIR");
    command.env_remove("GIT_WORK_TREE");
    command.env_remove("GIT_COMMON_DIR");
    command.arg("-C").arg(repo_root).args(args);
    command
}

#[derive(Debug)]
enum ChildCaptureResult {
    Completed(Output),
    Failed,
    TimedOut,
}

fn capture_child_with_deadline(mut child: Child, timeout: Duration) -> ChildCaptureResult {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .map(ChildCaptureResult::Completed)
                    .unwrap_or(ChildCaptureResult::Failed);
            }
            Ok(None) => {}
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return ChildCaptureResult::Failed;
            }
        }

        let now = Instant::now();
        if now >= deadline {
            let _ = child.kill();
            return if child.wait().is_ok() {
                ChildCaptureResult::TimedOut
            } else {
                ChildCaptureResult::Failed
            };
        }
        std::thread::sleep(
            deadline
                .saturating_duration_since(now)
                .min(CHILD_WAIT_POLL_INTERVAL),
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn git_program_is_stable_and_resolves() {
        // Cached: two calls return the identical pointer/value.
        let first = git_program();
        let second = git_program();
        assert_eq!(first, second);

        // Either an existing absolute path was found, or we fell back to the
        // literal "git" — never worse than a bare Command::new("git").
        let resolved = Path::new(first);
        assert!(
            resolved == Path::new(GIT_LITERAL) || resolved.is_file(),
            "git_program() should be the \"git\" fallback or an existing file, got {}",
            resolved.display()
        );
    }

    #[test]
    fn git_at_command_uses_dash_c_without_target_current_dir() {
        let repo_root = Path::new("/problematic/project/root");
        let command = git_command_at(
            repo_root,
            &["rev-parse", "--show-toplevel", "--git-common-dir"],
        );

        assert!(
            command.get_current_dir().is_none(),
            "git -C must inherit the safe daemon cwd instead of entering the target root"
        );
        assert_eq!(
            command
                .get_args()
                .map(std::ffi::OsStr::to_os_string)
                .collect::<Vec<_>>(),
            vec![
                OsString::from("-C"),
                repo_root.as_os_str().to_os_string(),
                OsString::from("rev-parse"),
                OsString::from("--show-toplevel"),
                OsString::from("--git-common-dir"),
            ]
        );
    }

    #[test]
    fn git_at_command_clears_repository_selection_overrides() {
        let command = git_command_at(Path::new("/problematic/project/root"), &["status"]);

        for key in ["GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR"] {
            assert_eq!(
                command
                    .get_envs()
                    .find(|(candidate, _)| *candidate == OsStr::new(key))
                    .map(|(_, value)| value),
                Some(None),
                "git -C must resolve the supplied root rather than inherited {key}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn git_capture_deadline_kills_and_reaps_child() {
        let child = Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleeping child");
        let started = std::time::Instant::now();

        let result = capture_child_with_deadline(child, std::time::Duration::from_millis(25));

        let ChildCaptureResult::TimedOut = result else {
            panic!("sleeping child should time out, got {result:?}");
        };
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "deadline must stop and reap the child promptly"
        );
    }

    #[test]
    fn git_env_override_is_honored() {
        // resolve_git_program() reads GIT directly; test it in isolation so the
        // process-wide OnceLock cache in git_program() is untouched.
        let sentinel = "/nonexistent/tracedecay-test-git-override";
        unsafe {
            std::env::set_var("GIT", sentinel);
        }
        let resolved = resolve_git_program();
        unsafe {
            std::env::remove_var("GIT");
        }
        assert_eq!(resolved, OsString::from(sentinel));

        // An empty GIT is ignored (falls through to PATH lookup / literal).
        unsafe {
            std::env::set_var("GIT", "");
        }
        let resolved_empty = resolve_git_program();
        unsafe {
            std::env::remove_var("GIT");
        }
        assert_ne!(resolved_empty, OsString::from(""));
    }

    #[test]
    fn bounded_output_reports_deadline_and_output_limit() {
        let root = tempfile::tempdir().unwrap();
        let expired = GitCommandBounds {
            deadline: Instant::now(),
            ..GitCommandBounds::default()
        };
        assert!(matches!(
            bounded_git_output(root.path(), &["--version"], &expired),
            Err(GitCommandError::DeadlineExceeded)
        ));

        let limited = GitCommandBounds {
            max_stdout_bytes: 1,
            ..GitCommandBounds::default()
        };
        assert!(matches!(
            bounded_git_output(root.path(), &["--version"], &limited),
            Err(GitCommandError::OutputLimitExceeded {
                stream: "stdout",
                bound: 1
            })
        ));
    }

    #[test]
    fn bounded_output_observes_pre_spawn_cancellation() {
        let root = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let bounds = GitCommandBounds {
            cancel: Some(cancel),
            ..GitCommandBounds::default()
        };
        assert!(matches!(
            bounded_git_output(root.path(), &["--version"], &bounds),
            Err(GitCommandError::Cancelled)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn bounded_command_interrupts_an_in_flight_process() {
        let cancellation = CancellationToken::new();
        let trigger = cancellation.clone();
        let notifier = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            trigger.cancel();
        });
        let mut command = Command::new("sh");
        command.args(["-c", "exec sleep 30"]);
        let bounds = GitCommandBounds {
            cancel: Some(cancellation),
            ..GitCommandBounds::default()
        };
        let started = Instant::now();
        let result = bounded_command_output(command, None, &bounds);
        notifier.join().unwrap();

        assert!(matches!(result, Err(GitCommandError::Cancelled)));
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
