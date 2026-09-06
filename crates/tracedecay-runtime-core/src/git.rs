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

use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(windows)]
use std::path::{Component, Prefix};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::cancellation::CancellationToken;

const GIT_LITERAL: &str = "git";
const GIT_CAPTURE_AT_TIMEOUT: Duration = Duration::from_secs(2);
const CHILD_WAIT_POLL_INTERVAL: Duration = Duration::from_millis(10);
const DEFAULT_READ_DEADLINE: Duration = Duration::from_secs(10);
const DEFAULT_STDOUT_LIMIT: usize = 16 * 1024 * 1024;
const DEFAULT_STDERR_LIMIT: usize = 64 * 1024;

/// Git cannot be spawned without an exact absolute executable path.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("no absolute git executable could be resolved from GIT or PATH")]
pub struct GitProgramUnavailable;

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

impl From<GitProgramUnavailable> for GitCommandError {
    fn from(error: GitProgramUnavailable) -> Self {
        Self::Unavailable(std::io::Error::new(std::io::ErrorKind::NotFound, error))
    }
}

/// Returns the resolved absolute `git` program to spawn.
///
/// Resolution order (performed once, then cached):
///   1. The `GIT` environment variable, if it names an absolute executable.
///   2. An absolute path found by a which-style walk of `PATH` (+ `PATHEXT` on
///      Windows).
///   3. A typed unavailable result. Production callers must not reintroduce a
///      bare-program fallback because that delegates identity back to ambient
///      `PATH` at spawn time.
pub fn try_git_program() -> Result<&'static OsStr, GitProgramUnavailable> {
    static PROGRAM: OnceLock<Result<OsString, GitProgramUnavailable>> = OnceLock::new();
    PROGRAM
        .get_or_init(resolve_git_program)
        .as_ref()
        .map(OsString::as_os_str)
        .map_err(|error| *error)
}

fn resolve_git_program() -> Result<OsString, GitProgramUnavailable> {
    resolve_git_program_from(
        std::env::var_os("GIT").as_deref(),
        std::env::var_os("PATH").as_deref(),
        #[cfg(windows)]
        std::env::var_os("PATHEXT").as_deref(),
    )
}

fn resolve_git_program_from(
    git_override: Option<&OsStr>,
    path: Option<&OsStr>,
    #[cfg(windows)] pathext: Option<&OsStr>,
) -> Result<OsString, GitProgramUnavailable> {
    if let Some(value) = git_override
        && !value.is_empty()
    {
        let path = Path::new(value);
        return (path.is_absolute() && is_executable_file(path))
            .then(|| value.to_os_string())
            .ok_or(GitProgramUnavailable);
    }

    find_in_path(
        GIT_LITERAL,
        path.ok_or(GitProgramUnavailable)?,
        #[cfg(windows)]
        pathext,
    )
    .map(PathBuf::into_os_string)
    .ok_or(GitProgramUnavailable)
}

/// Minimal `which`-style lookup: find `name` as an executable on `PATH`.
///
/// On Windows, each `PATH` entry is probed with every `PATHEXT` suffix (and the
/// bare name) so `git.exe` resolves from `git`. On Unix, the bare name is probed
/// and the entry must carry at least one execute bit.
fn find_in_path(
    name: &str,
    path: &OsStr,
    #[cfg(windows)] pathext: Option<&OsStr>,
) -> Option<PathBuf> {
    for dir in std::env::split_paths(path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let absolute_dir = if dir.is_absolute() {
            dir
        } else {
            std::path::absolute(dir).ok()?
        };
        if let Some(found) = probe_dir(
            &absolute_dir,
            name,
            #[cfg(windows)]
            pathext,
        ) {
            return Some(found);
        }
    }
    None
}

#[cfg(windows)]
fn probe_dir(dir: &Path, name: &str, pathext: Option<&OsStr>) -> Option<PathBuf> {
    // PATHEXT holds the executable suffixes (";"-separated), e.g.
    // ".COM;.EXE;.BAT;.CMD". Fall back to a sane default when unset.
    let pathext = pathext
        .and_then(OsStr::to_str)
        .unwrap_or(".COM;.EXE;.BAT;.CMD");

    // If the name already carries an extension, try it verbatim first.
    let bare = dir.join(name);
    if is_executable_file(&bare) {
        return Some(bare);
    }
    for ext in pathext.split(';') {
        let ext = ext.trim();
        if ext.is_empty() {
            continue;
        }
        let candidate = dir.join(format!("{name}{ext}"));
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(not(windows))]
fn probe_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    let candidate = dir.join(name);
    is_executable_file(&candidate).then_some(candidate)
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(windows)]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

#[cfg(not(any(unix, windows)))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

/// Spells a native path the way the invoked Git accepts it as a command-line
/// argument.
///
/// `fs::canonicalize` returns the `\\?\` verbatim form on Windows. The Win32
/// file APIs take that form, so `std::fs` and [`Command::current_dir`] are
/// fine with it, but Git for Windows rewrites the separators of a path it
/// receives as an *argument* and then fails on the resulting `//?/C:/...`
/// (`could not create leading directories of '//?/D:/...': Invalid argument`
/// from `git worktree add`). This drops only the verbatim prefix —
/// `\\?\C:\x` becomes `C:\x`, `\\?\UNC\server\share\x` becomes
/// `\\server\share\x` — and passes every other component through byte for
/// byte, so long, spaced, and non-ASCII paths are untouched. Elsewhere the
/// path is returned as is.
///
/// This is a spelling for one Git argument, not a new identity: callers keep
/// the native path they verified as the root they key on and compare against.
#[cfg(windows)]
pub fn git_path_argument(path: &Path) -> Cow<'_, OsStr> {
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return Cow::Borrowed(path.as_os_str());
    };
    let mut spelled = match prefix.kind() {
        Prefix::VerbatimDisk(letter) => PathBuf::from(format!(r"{}:\", char::from(letter))),
        Prefix::VerbatimUNC(server, share) => {
            let mut root = OsString::from(r"\\");
            root.push(server);
            root.push(r"\");
            root.push(share);
            root.push(r"\");
            PathBuf::from(root)
        }
        // `\\?\Volume{...}`-style prefixes have no non-verbatim spelling, and
        // the remaining prefixes are already what Git expects.
        Prefix::Verbatim(_) | Prefix::DeviceNS(_) | Prefix::UNC(..) | Prefix::Disk(_) => {
            return Cow::Borrowed(path.as_os_str());
        }
    };
    for component in components {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => spelled.push(name),
            // A verbatim path is not normalized by Win32, so `.`/`..` are
            // literal names in it; the plain spelling would collapse them and
            // name a different directory. Refuse to change the meaning.
            Component::Prefix(_) | Component::CurDir | Component::ParentDir => {
                return Cow::Borrowed(path.as_os_str());
            }
        }
    }
    Cow::Owned(spelled.into_os_string())
}

/// Spells a native path the way the invoked Git accepts it as a command-line
/// argument. Only Windows has a verbatim spelling to translate; every other
/// host passes the path through unchanged.
#[cfg(not(windows))]
pub fn git_path_argument(path: &Path) -> Cow<'_, OsStr> {
    Cow::Borrowed(path.as_os_str())
}

/// Runs `git <args>` in `repo_root` with the resolved [`try_git_program`], returning
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
    let program = try_git_program().map_err(GitCommandError::from)?;
    let mut command = Command::new(program);
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
/// applies `TraceDecay`'s ambient-environment sanitization.
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
    let Ok(mut command) = git_command_at(repo_root, args) else {
        return GitCaptureAtResult::Failed;
    };
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

fn git_command_at(repo_root: &Path, args: &[&str]) -> Result<Command, GitProgramUnavailable> {
    let mut command = Command::new(try_git_program()?);
    // Repository selection must come from `-C <repo_root>`, never from
    // overrides inherited from the daemon's own environment: an inherited
    // GIT_DIR would silently resolve every probed path to the same repo.
    command.env_remove("GIT_DIR");
    command.env_remove("GIT_WORK_TREE");
    command.env_remove("GIT_COMMON_DIR");
    let repo_root: &OsStr = &git_path_argument(repo_root);
    command.arg("-C").arg(repo_root).args(args);
    Ok(command)
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
                    .map_or(ChildCaptureResult::Failed, ChildCaptureResult::Completed);
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
    fn git_program_is_stable_and_absolute() {
        let first = try_git_program().expect("git executable should resolve");
        let second = try_git_program().expect("cached git executable should resolve");
        assert_eq!(first, second);
        assert!(Path::new(first).is_absolute());
    }

    #[test]
    fn resolver_preserves_exact_absolute_override() {
        let temporary = tempfile::tempdir().expect("temporary executable directory");
        let executable = temporary
            .path()
            .join(if cfg!(windows) { "git.exe" } else { "git" });
        std::fs::write(&executable, b"test executable").expect("write fake git executable");
        #[cfg(unix)]
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
            .expect("make fake git executable");

        let resolved = resolve_git_program_from(
            Some(executable.as_os_str()),
            None,
            #[cfg(windows)]
            None,
        )
        .expect("absolute override should resolve");

        assert_eq!(resolved, executable.into_os_string());
    }

    #[test]
    fn resolver_returns_absolute_path_candidate() {
        let temporary = tempfile::tempdir().expect("temporary executable directory");
        let executable = temporary
            .path()
            .join(if cfg!(windows) { "git.exe" } else { "git" });
        std::fs::write(&executable, b"test executable").expect("write fake git executable");
        #[cfg(unix)]
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
            .expect("make fake git executable");
        let path = std::env::join_paths([temporary.path()]).expect("fixture PATH");

        let resolved = resolve_git_program_from(
            None,
            Some(path.as_os_str()),
            #[cfg(windows)]
            None,
        )
        .expect("PATH candidate should resolve");

        assert_eq!(resolved, executable.into_os_string());
    }

    #[test]
    fn resolver_reports_unavailable_without_an_absolute_executable() {
        let temporary = tempfile::tempdir().expect("empty PATH directory");
        let path = std::env::join_paths([temporary.path()]).expect("fixture PATH");

        let error = resolve_git_program_from(
            Some(OsStr::new(GIT_LITERAL)),
            Some(path.as_os_str()),
            #[cfg(windows)]
            None,
        )
        .expect_err("a relative override must not become an ambient PATH lookup");

        assert_eq!(error, GitProgramUnavailable);
    }

    #[test]
    fn git_at_command_uses_dash_c_without_target_current_dir() {
        let repo_root = Path::new("/problematic/project/root");
        let command = git_command_at(
            repo_root,
            &["rev-parse", "--show-toplevel", "--git-common-dir"],
        )
        .expect("git executable should resolve");

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
    fn git_path_argument_passes_a_plain_path_through_unchanged() {
        let plain = Path::new("/problematic/project/root");
        assert!(matches!(
            git_path_argument(plain),
            Cow::Borrowed(spelled) if spelled == plain.as_os_str()
        ));

        let spaced = tempfile::tempdir()
            .unwrap()
            .path()
            .join("with space/ünïcode-ß");
        assert_eq!(&*git_path_argument(&spaced), spaced.as_os_str());
    }

    #[cfg(windows)]
    #[test]
    fn git_path_argument_drops_only_the_verbatim_prefix() {
        assert_eq!(
            &*git_path_argument(Path::new(r"\\?\D:\a\_temp\tmp\.tmpF1zlYs-admission-wt")),
            OsStr::new(r"D:\a\_temp\tmp\.tmpF1zlYs-admission-wt")
        );
        assert_eq!(
            &*git_path_argument(Path::new(r"\\?\C:\Users\Zack\TraceDecay Data\ünïcode")),
            OsStr::new(r"C:\Users\Zack\TraceDecay Data\ünïcode")
        );
        assert_eq!(
            &*git_path_argument(Path::new(r"\\?\UNC\server\share\repo\src")),
            OsStr::new(r"\\server\share\repo\src")
        );
        for unchanged in [
            r"C:\already\plain",
            r"\\server\share\plain",
            r"\\?\Volume{1234}\no-plain-spelling",
            r"\\?\C:\literal\..\dot-dot-is-a-name-here",
        ] {
            assert_eq!(
                &*git_path_argument(Path::new(unchanged)),
                OsStr::new(unchanged),
                "{unchanged} must pass through unchanged"
            );
        }
    }

    /// The spelling exists so Git sees a directory it can open; the directory
    /// itself must be the one the caller canonicalized.
    #[test]
    fn git_path_argument_names_the_canonical_directory() {
        let temporary = tempfile::tempdir().unwrap();
        let canonical = temporary.path().canonicalize().unwrap();
        let spelled = PathBuf::from(git_path_argument(&canonical).into_owned());
        assert_eq!(spelled.canonicalize().unwrap(), canonical);
        if cfg!(windows) {
            assert!(
                !spelled.to_string_lossy().starts_with(r"\\?\"),
                "Windows canonical paths are verbatim and must be respelled: {spelled:?}"
            );
        } else {
            assert_eq!(spelled, canonical);
        }
    }

    #[test]
    fn git_at_command_spells_the_repository_root_for_git() {
        let temporary = tempfile::tempdir().unwrap();
        let canonical = temporary.path().canonicalize().unwrap();
        let command = git_command_at(&canonical, &["rev-parse", "--show-toplevel"])
            .expect("git executable should resolve");
        let root_argument = command
            .get_args()
            .nth(1)
            .expect("-C carries the repository root")
            .to_os_string();
        assert_eq!(root_argument, git_path_argument(&canonical).into_owned());
    }

    #[test]
    fn git_at_command_clears_repository_selection_overrides() {
        let command = git_command_at(Path::new("/problematic/project/root"), &["status"])
            .expect("git executable should resolve");

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
