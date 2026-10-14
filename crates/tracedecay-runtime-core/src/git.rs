//! Process-wide resolution of the `git` binary.
//!
//! The daemon and CLI spawn `git` from ~13 sites. A bare `Command::new("git")`
//! makes the OS re-walk `PATH` on every spawn, cheap on Linux/macOS but
//! ~100-300ms per spawn on Windows. This module resolves the `git` binary to an
//! absolute path exactly once (cached in a [`OnceLock`]) and hands every product
//! spawn site that cached path, so the long-running daemon never re-walks `PATH`.
//!
//! The read authority in [`crate::git_repository`] uses this program only for
//! the bounded linked-worktree symbolic-HEAD fallback, and the topology reads
//! in [`crate::branch`] and [`crate::worktree`] are fully in-process. Other
//! callers use the bounded CLI fallback here for native Git writes, signing,
//! recovery, and reads where exact porcelain semantics remain the authority.

pub mod churn;
pub mod co_change;

use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use crate::cancellation::CancellationToken;
use crate::path_safety::{plain_git_args, plain_host_path};

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
    #[error("failed to write git stdin: {0}")]
    WriteInput(#[source] std::io::Error),
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
/// Resolution order (cached once it succeeds):
///   1. The `GIT` environment variable, if it names an absolute executable.
///   2. An absolute path found by a which-style walk of `PATH` (+ `PATHEXT` on
///      Windows).
///   3. A typed unavailable result. Production callers must not reintroduce a
///      bare-program fallback because that delegates identity back to ambient
///      `PATH` at spawn time.
pub fn try_git_program() -> Result<&'static OsStr, GitProgramUnavailable> {
    static PROGRAM: OnceLock<OsString> = OnceLock::new();
    cached_program(&PROGRAM, resolve_git_program).map(OsString::as_os_str)
}

/// Memoises only a successful resolution. A failed lookup (for example while
/// `PATH` is momentarily wrong at first call) is returned but not cached, so a
/// later call with a working environment recovers instead of reporting
/// [`GitProgramUnavailable`] for the rest of the process.
fn cached_program(
    cache: &OnceLock<OsString>,
    resolve: impl FnOnce() -> Result<OsString, GitProgramUnavailable>,
) -> Result<&OsString, GitProgramUnavailable> {
    if let Some(program) = cache.get() {
        return Ok(program);
    }
    let program = resolve()?;
    Ok(cache.get_or_init(|| program))
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

/// Locate a runnable executable using PATH and the platform's executable suffixes.
pub fn find_executable_on_path(name: &str) -> Option<PathBuf> {
    find_in_path(
        name,
        &std::env::var_os("PATH")?,
        #[cfg(windows)]
        std::env::var_os("PATHEXT").as_deref(),
    )
}

/// Minimal `which`-style lookup: find `name` as an executable on `PATH`.
///
/// On Windows, each `PATH` entry is probed with every `PATHEXT` suffix (and the
/// bare name when it already has an extension) so `git.exe` resolves from `git`.
/// On Unix, the bare name is probed and the entry must carry at least one
/// execute bit.
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

    // `CreateProcess` cannot launch an extensionless file. npm's global bin
    // directory leaves that shim beside the runnable `.cmd`.
    let bare = dir.join(name);
    if bare.extension().is_some() && is_executable_file(&bare) {
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
        .args(plain_git_args(args))
        .current_dir(plain_host_path(repo_root));
    bounded_command_output(command, None, bounds)
}

/// Runs an already-configured command with bounded output and request controls.
///
/// The caller owns the executable, arguments, working directory, and
/// environment. This function owns only process I/O, cancellation, deadline,
/// and termination. [`bounded_git_output`] is the read-only Git wrapper that
/// applies `TraceDecay`'s ambient-environment sanitization.
pub fn bounded_command_output(
    mut command: Command,
    stdin: Option<&[u8]>,
    bounds: &GitCommandBounds,
) -> Result<Output, GitCommandError> {
    if is_cancelled(bounds) {
        return Err(GitCommandError::Cancelled);
    }
    if Instant::now() >= bounds.deadline {
        return Err(GitCommandError::DeadlineExceeded);
    }

    let mut child = command
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(GitCommandError::Unavailable)?;
    let output = supervise_child(&mut child, stdin, bounds);
    if output.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    output
}

/// Runs in-process Git work under the bounds that govern Git subprocesses.
///
/// `work` must stop once `interrupt` is raised. A watchdog raises it when the
/// request is cancelled or its deadline passes, and the work's output is then
/// discarded because an interrupted walk returns whatever it had so far.
pub(crate) fn run_interruptible<T>(
    bounds: &GitCommandBounds,
    interrupt: &AtomicBool,
    work: impl FnOnce() -> T,
) -> Result<T, GitCommandError> {
    let (finished, settled) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
        let watchdog = scope.spawn(move || {
            loop {
                let wait = bounds
                    .deadline
                    .saturating_duration_since(Instant::now())
                    .min(CHILD_WAIT_POLL_INTERVAL);
                let work_settled =
                    !matches!(settled.recv_timeout(wait), Err(RecvTimeoutError::Timeout));
                let fired = if is_cancelled(bounds) {
                    Some(GitCommandError::Cancelled)
                } else if Instant::now() >= bounds.deadline {
                    Some(GitCommandError::DeadlineExceeded)
                } else {
                    None
                };
                if fired.is_some() {
                    interrupt.store(true, Ordering::Release);
                    return fired;
                }
                if work_settled {
                    return None;
                }
            }
        });
        let output = work();
        drop(finished);
        match watchdog.join() {
            Ok(None) => Ok(output),
            Ok(Some(error)) => Err(error),
            Err(_) => Err(GitCommandError::Wait(std::io::Error::other(
                "in-process Git watchdog panicked",
            ))),
        }
    })
}

fn is_cancelled(bounds: &GitCommandBounds) -> bool {
    bounds
        .cancel
        .as_ref()
        .is_some_and(CancellationToken::is_cancelled)
}

struct BoundedRead {
    bytes: Vec<u8>,
    exceeded: bool,
}

enum PipeEvent {
    Exceeded {
        stream: &'static str,
        bound: usize,
    },
    Read {
        stream: &'static str,
        result: std::io::Result<BoundedRead>,
    },
    Written(std::io::Result<()>),
}

/// What the pipe threads have reported so far. The threads are detached: a
/// descendant that inherited a pipe can keep one blocked past the caller's
/// deadline, and its final send then fails silently.
struct PipeEvents {
    receiver: Receiver<PipeEvent>,
    exceeded: Option<(&'static str, usize)>,
    stdout: Option<std::io::Result<BoundedRead>>,
    stderr: Option<std::io::Result<BoundedRead>>,
    written: Option<std::io::Result<()>>,
}

impl PipeEvents {
    /// Waits at most one poll interval for the next event under the bounds.
    fn pump(&mut self, bounds: &GitCommandBounds) -> Result<(), GitCommandError> {
        if is_cancelled(bounds) {
            return Err(GitCommandError::Cancelled);
        }
        let wait = bounds.deadline.saturating_duration_since(Instant::now());
        if wait.is_zero() {
            return Err(GitCommandError::DeadlineExceeded);
        }
        let wait = wait.min(CHILD_WAIT_POLL_INTERVAL);
        match self.receiver.recv_timeout(wait) {
            Ok(PipeEvent::Exceeded { stream, bound }) => {
                self.exceeded.get_or_insert((stream, bound));
            }
            Ok(PipeEvent::Read { stream, result }) => {
                if stream == "stdout" {
                    self.stdout = Some(result);
                } else {
                    self.stderr = Some(result);
                }
            }
            Ok(PipeEvent::Written(result)) => self.written = Some(result),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                let reader_panicked = || Err(std::io::Error::other("git output reader panicked"));
                self.stdout.get_or_insert_with(reader_panicked);
                self.stderr.get_or_insert_with(reader_panicked);
                self.written
                    .get_or_insert_with(|| Err(std::io::Error::other("git stdin writer panicked")));
                std::thread::sleep(wait);
            }
        }
        Ok(())
    }

    fn wait_for<T>(
        &mut self,
        bounds: &GitCommandBounds,
        take: impl Fn(&mut Self) -> Option<T>,
    ) -> Result<T, GitCommandError> {
        loop {
            if let Some(value) = take(self) {
                return Ok(value);
            }
            self.pump(bounds)?;
        }
    }
}

fn supervise_child(
    child: &mut Child,
    input: Option<&[u8]>,
    bounds: &GitCommandBounds,
) -> Result<Output, GitCommandError> {
    let missing = |pipe| GitCommandError::Unavailable(std::io::Error::other(pipe));
    let abandoned = AbandonOnReturn(Arc::new(AtomicBool::new(false)));
    let (sender, receiver) = mpsc::channel();
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| missing("missing stdout pipe"))?;
    spawn_reader(
        stdout,
        "stdout",
        bounds.max_stdout_bytes,
        sender.clone(),
        Arc::clone(&abandoned.0),
    )?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| missing("missing stderr pipe"))?;
    spawn_reader(
        stderr,
        "stderr",
        bounds.max_stderr_bytes,
        sender.clone(),
        Arc::clone(&abandoned.0),
    )?;
    if let Some(input) = input {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| missing("missing stdin pipe"))?;
        let input = input.to_vec();
        let abandoned = Arc::clone(&abandoned.0);
        std::thread::Builder::new()
            .name("tracedecay-git-stdin".into())
            .spawn(move || {
                let written = write_input(&mut stdin, &input, &abandoned);
                drop(stdin);
                let _ = sender.send(PipeEvent::Written(written));
            })
            .map_err(GitCommandError::Wait)?;
    } else {
        drop(sender);
    }
    let mut events = PipeEvents {
        receiver,
        exceeded: None,
        stdout: None,
        stderr: None,
        written: None,
    };

    let status = loop {
        if let Some(status) = child.try_wait().map_err(GitCommandError::Wait)? {
            break status;
        }
        events.pump(bounds)?;
        if let Some((stream, bound)) = events.exceeded {
            return Err(GitCommandError::OutputLimitExceeded { stream, bound });
        }
    };
    if input.is_some() {
        let written = events.wait_for(bounds, |events| events.written.take())?;
        if status.success() {
            written.map_err(GitCommandError::WriteInput)?;
        }
    }
    let stdout = events
        .wait_for(bounds, |events| events.stdout.take())?
        .map_err(|source| GitCommandError::ReadOutput {
            stream: "stdout",
            source,
        })?;
    let stderr = events
        .wait_for(bounds, |events| events.stderr.take())?
        .map_err(|source| GitCommandError::ReadOutput {
            stream: "stderr",
            source,
        })?;
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

/// Raised on every return from [`supervise_child`]. A descendant can inherit a
/// pipe and hold it open after the child exits; the pipe threads then stop and
/// close our ends, so the descendant sees EOF or a broken pipe.
struct AbandonOnReturn(Arc<AtomicBool>);

impl Drop for AbandonOnReturn {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[cfg(unix)]
trait PipeEnd: std::os::fd::AsFd + Send + 'static {}
#[cfg(unix)]
impl<T: std::os::fd::AsFd + Send + 'static> PipeEnd for T {}
#[cfg(not(unix))]
trait PipeEnd: Send + 'static {}
#[cfg(not(unix))]
impl<T: Send + 'static> PipeEnd for T {}

#[cfg(unix)]
fn set_nonblocking(pipe: &impl PipeEnd) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let fd = pipe.as_fd().as_raw_fd();
    // SAFETY: `fd` is our open pipe end, borrowed for the call.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: as above; only our end's status flags change.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Waits until `pipe` is ready, failing once the supervisor abandons it.
#[cfg(unix)]
fn await_pipe(pipe: &impl PipeEnd, writable: bool, abandoned: &AtomicBool) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let mut poll_fd = libc::pollfd {
        fd: pipe.as_fd().as_raw_fd(),
        events: if writable {
            libc::POLLOUT
        } else {
            libc::POLLIN
        },
        revents: 0,
    };
    let timeout =
        libc::c_int::try_from(CHILD_WAIT_POLL_INTERVAL.as_millis()).unwrap_or(libc::c_int::MAX);
    while !abandoned.load(Ordering::Acquire) {
        // SAFETY: `poll_fd` is one valid pollfd, exclusively borrowed for the call.
        match unsafe { libc::poll(&raw mut poll_fd, 1, timeout) } {
            0 => {}
            -1 => {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
            _ => return Ok(()),
        }
    }
    Err(std::io::Error::other("git supervisor abandoned the pipe"))
}

/// Windows child pipes cannot be polled; the blocking call that follows waits.
#[cfg(not(unix))]
fn await_pipe(_: &impl PipeEnd, _: bool, _: &AtomicBool) -> std::io::Result<()> {
    Ok(())
}

fn retryable(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
    )
}

fn write_input(
    stdin: &mut (impl Write + PipeEnd),
    mut input: &[u8],
    abandoned: &AtomicBool,
) -> std::io::Result<()> {
    #[cfg(unix)]
    set_nonblocking(stdin)?;
    while !input.is_empty() {
        await_pipe(stdin, true, abandoned)?;
        match stdin.write(input) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(written) => input = &input[written..],
            Err(error) if retryable(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn spawn_reader(
    mut pipe: impl Read + PipeEnd,
    stream: &'static str,
    limit: usize,
    sender: Sender<PipeEvent>,
    abandoned: Arc<AtomicBool>,
) -> Result<(), GitCommandError> {
    std::thread::Builder::new()
        .name(format!("tracedecay-git-{stream}"))
        .spawn(move || {
            let result = read_bounded_output(&mut pipe, stream, limit, &sender, &abandoned);
            let _ = sender.send(PipeEvent::Read { stream, result });
        })
        .map(drop)
        .map_err(GitCommandError::Wait)
}

/// Keeps draining past `limit` so an over-limit child never blocks on a full
/// pipe before the supervisor kills it.
fn read_bounded_output(
    pipe: &mut (impl Read + PipeEnd),
    stream: &'static str,
    limit: usize,
    sender: &Sender<PipeEvent>,
    abandoned: &AtomicBool,
) -> std::io::Result<BoundedRead> {
    #[cfg(unix)]
    set_nonblocking(pipe)?;
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut buffer = [0u8; 8 * 1024];
    let mut exceeded = false;
    loop {
        await_pipe(pipe, false, abandoned)?;
        let count = match pipe.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if retryable(&error) => continue,
            Err(error) => return Err(error),
        };
        let remaining = limit.saturating_sub(bytes.len());
        bytes.extend_from_slice(&buffer[..count.min(remaining)]);
        if count > remaining && !exceeded {
            exceeded = true;
            let _ = sender.send(PipeEvent::Exceeded {
                stream,
                bound: limit,
            });
        }
    }
    Ok(BoundedRead { bytes, exceeded })
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

/// Reads `remote.origin.url` from the repository at `project_root`.
///
/// Prefers an in-process gix config snapshot (repo-local + global) and
/// falls back to a bounded `git config --get` when gix cannot discover
/// the repository but git still may.
pub fn git_remote_url(project_root: &Path) -> Option<String> {
    if let Ok(repo) = crate::git_open::discover(project_root) {
        let url = repo
            .config_snapshot()
            .string("remote.origin.url")?
            .to_string();
        let url = url.trim();
        return (!url.is_empty()).then(|| url.to_string());
    }
    if !crate::worktree::git_may_resolve_repo(project_root) {
        return None;
    }
    git_capture(project_root, &["config", "--get", "remote.origin.url"])
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
    command
        .arg("-C")
        .arg(plain_host_path(repo_root))
        .args(plain_git_args(args));
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
    use crate::test_executable::write_executable_script;

    #[test]
    fn resolver_preserves_exact_absolute_override() {
        let temporary = tempfile::tempdir().expect("temporary executable directory");
        let executable = temporary
            .path()
            .join(if cfg!(windows) { "git.exe" } else { "git" });
        write_executable_script(&executable, b"test executable")
            .expect("write fake git executable");

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
        write_executable_script(&executable, b"test executable")
            .expect("write fake git executable");
        let path = std::env::join_paths([temporary.path()]).expect("fixture PATH");

        let resolved = resolve_git_program_from(
            None,
            Some(path.as_os_str()),
            #[cfg(windows)]
            None,
        )
        .expect("PATH candidate should resolve");

        // Windows spells the candidate with the `PATHEXT` suffix it probed
        // (`.EXE`), which the case-insensitive filesystem accepts for the
        // on-disk `git.exe`; the two are one executable, so compare the file
        // they name rather than the bytes. Unix has no such aliasing.
        #[cfg(windows)]
        assert!(
            crate::path_safety::same_canonical_path(Path::new(&resolved), &executable),
            "resolved {resolved:?} must name the fixture executable {}",
            executable.display()
        );
        #[cfg(not(windows))]
        assert_eq!(resolved, executable.into_os_string());
    }

    #[cfg(windows)]
    #[test]
    fn windows_lookup_skips_an_extensionless_shell_shim() {
        let temporary = tempfile::tempdir().expect("npm-style bin directory");
        std::fs::write(temporary.path().join("ast-grep"), b"#!/bin/sh\n").expect("shell shim");
        let launcher = temporary.path().join("ast-grep.cmd");
        std::fs::write(&launcher, b"@echo off\r\n").expect("cmd launcher");
        let path = std::env::join_paths([temporary.path()]).expect("fixture PATH");

        let resolved = find_in_path("ast-grep", &path, Some(OsStr::new(".EXE;.CMD")))
            .expect("the runnable launcher must resolve");

        assert!(
            crate::path_safety::same_canonical_path(&resolved, &launcher),
            "resolved {} instead of the runnable launcher",
            resolved.display()
        );
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
    fn failed_lookup_is_not_memoised_and_later_success_is() {
        let cache = OnceLock::new();

        assert_eq!(
            cached_program(&cache, || Err(GitProgramUnavailable)),
            Err(GitProgramUnavailable)
        );
        let resolved = cached_program(&cache, || Ok(OsString::from("/fixture/git")))
            .expect("a working lookup after a failed one must resolve");
        assert_eq!(resolved, "/fixture/git");
        let cached = cached_program(&cache, || panic!("a cached success must not re-resolve"))
            .expect("cached program");
        assert_eq!(cached, resolved);
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

    fn spin_until_interrupted(interrupt: &AtomicBool) {
        let spinning = Instant::now();
        while !interrupt.load(Ordering::Acquire) {
            assert!(
                spinning.elapsed() < Duration::from_secs(5),
                "the watchdog never raised the interrupt"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn in_process_work_is_interrupted_by_live_cancellation() {
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        let bounds = GitCommandBounds {
            cancel: Some(cancel),
            ..GitCommandBounds::default()
        };
        let interrupt = AtomicBool::new(false);
        let notifier = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            trigger.cancel();
        });

        let result = run_interruptible(&bounds, &interrupt, || spin_until_interrupted(&interrupt));
        notifier.join().expect("cancellation notifier");

        assert!(matches!(result, Err(GitCommandError::Cancelled)));
    }

    #[test]
    fn in_process_work_is_interrupted_at_its_deadline() {
        let bounds = GitCommandBounds {
            deadline: Instant::now() + Duration::from_millis(30),
            ..GitCommandBounds::default()
        };
        let interrupt = AtomicBool::new(false);

        let result = run_interruptible(&bounds, &interrupt, || spin_until_interrupted(&interrupt));

        assert!(matches!(result, Err(GitCommandError::DeadlineExceeded)));
    }
}
