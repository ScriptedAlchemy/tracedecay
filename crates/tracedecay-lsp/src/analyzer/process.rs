//! The analyzer child process, driven from std threads.
//!
//! A supervisor thread spawns the child and owns its `wait`; one thread each
//! owns stdin, stdout, and stderr. They meet the async client over
//! `async_channel`s, whose futures wake through ordinary `Waker`s, so any
//! executor can drive the client and no pipe depends on a reactor. Asupersync
//! child pipes are Unix-only, so std threads are also what Windows runs.

use std::future::poll_fn;
use std::io::{self, Write};
use std::pin::pin;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::task::Poll;
use std::time::Instant;

use tokio_util::bytes::BytesMut;
use tokio_util::codec::Decoder;
use tracedecay_daemon_protocol::{FramePoll, LspFrame};
use tracedecay_runtime_core::child_pipe::{ChildPipeStop, child_pipe_stop};
use tracedecay_runtime_core::native_runtime;

use super::error::{AnalyzerResult as Result, AnalyzerRuntimeError as TraceDecayError};
use crate::{AsyncContentLengthError, ContentLengthCodec};

const STDERR_CAPTURE_BYTES: usize = 8192;

type Pipes = (Option<ChildStdin>, Option<ChildStdout>, Option<ChildStderr>);
type StatusRequest = async_channel::Sender<io::Result<Option<ExitStatus>>>;
type WriteRequest = (Vec<u8>, async_channel::Sender<io::Result<()>>);
type FrameResult = std::result::Result<LspFrame, AsyncContentLengthError>;

pub(super) struct AnalyzerProcess {
    /// Dropping the lifeline makes the supervisor kill and reap the child.
    lifeline: Option<mpsc::Sender<StatusRequest>>,
    exited: async_channel::Receiver<io::Result<ExitStatus>>,
    writes: async_channel::Sender<WriteRequest>,
    frames: async_channel::Receiver<FrameResult>,
    /// Whether the stdout thread holds bytes of a frame it has not completed.
    partial_frame: Arc<AtomicBool>,
    stderr: Arc<Mutex<Vec<u8>>>,
    stderr_closed: async_channel::Receiver<()>,
}

impl AnalyzerProcess {
    /// Spawns `command` with piped stdio. `name` labels the failures.
    pub(super) async fn spawn(mut command: Command, name: &str) -> Result<Self> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (lifeline, requests) = mpsc::channel();
        let (started_sender, started) = async_channel::bounded(1);
        let (exited_sender, exited) = async_channel::bounded(1);
        let stop_pipe = |pipe: &str, error: io::Error| TraceDecayError::Config {
            message: format!(
                "failed to open the {pipe} stop pipe for LSP server '{name}': {error}"
            ),
        };
        let (stdout_stop, stdout_watch) =
            child_pipe_stop().map_err(|error| stop_pipe("stdout", error))?;
        let (stderr_stop, stderr_watch) =
            child_pipe_stop().map_err(|error| stop_pipe("stderr", error))?;
        // The supervisor spawns the child itself, so a thread that fails to
        // start never strands a child nobody kills.
        spawn_thread("tracedecay-lsp-analyzer", name, move || {
            supervise(
                command,
                &started_sender,
                &requests,
                &exited_sender,
                stdout_stop,
                stderr_stop,
            );
        })?;
        let spawned = started.recv().await.map_err(|_| TraceDecayError::Config {
            message: format!("LSP server '{name}' supervisor exited before spawning it"),
        })?;
        let (stdin, stdout, stderr) = spawned.map_err(|error| TraceDecayError::Config {
            message: format!("failed to spawn LSP server '{name}': {error}"),
        })?;
        let missing = |pipe: &str| TraceDecayError::Config {
            message: format!("failed to open {pipe} for LSP server '{name}'"),
        };
        let mut stdin = stdin.ok_or_else(|| missing("stdin"))?;
        let mut stdout = stdout.ok_or_else(|| missing("stdout"))?;
        let mut stderr = stderr.ok_or_else(|| missing("stderr"))?;

        let (writes, queue) = async_channel::bounded::<WriteRequest>(1);
        spawn_thread("tracedecay-lsp-analyzer-stdin", name, move || {
            while let Ok((bytes, done)) = queue.recv_blocking() {
                let _ = done.send_blocking(stdin.write_all(&bytes).and_then(|()| stdin.flush()));
            }
        })?;
        let (frame_sender, frames) = async_channel::bounded(1);
        let partial_frame = Arc::new(AtomicBool::new(false));
        let partial = Arc::clone(&partial_frame);
        let mut stdout_watch = stdout_watch;
        spawn_thread("tracedecay-lsp-analyzer-stdout", name, move || {
            read_frames(
                &mut |buf: &mut [u8]| stdout_watch.read(&mut stdout, buf),
                &frame_sender,
                &partial,
            );
        })?;
        let capture = Arc::new(Mutex::new(Vec::new()));
        let (stderr_closed_sender, stderr_closed) = async_channel::bounded::<()>(1);
        let captured = Arc::clone(&capture);
        let mut stderr_watch = stderr_watch;
        spawn_thread("tracedecay-lsp-analyzer-stderr", name, move || {
            drain_stderr(
                &mut |buf: &mut [u8]| stderr_watch.read(&mut stderr, buf),
                &captured,
            );
            drop(stderr_closed_sender);
        })?;

        Ok(Self {
            lifeline: Some(lifeline),
            exited,
            writes,
            frames,
            partial_frame,
            stderr: capture,
            stderr_closed,
        })
    }

    /// Writes and flushes one `Content-Length` frame before `deadline`. A
    /// write still in flight at the deadline keeps its place ahead of later
    /// writes.
    pub(super) async fn write_frame(
        &self,
        frame: &[u8],
        deadline: Instant,
    ) -> std::result::Result<(), AsyncContentLengthError> {
        let encoded = ContentLengthCodec::encode(frame)?;
        let exited = || io::Error::new(io::ErrorKind::BrokenPipe, "LSP stdin writer exited");
        let (done, outcome) = async_channel::bounded(1);
        let written = until(deadline, async {
            self.writes
                .send((encoded, done))
                .await
                .map_err(|_| exited())?;
            outcome.recv().await.map_err(|_| exited())?
        })
        .await?;
        written
            .ok_or(AsyncContentLengthError::DeadlineElapsed)?
            .map_err(Into::into)
    }

    /// Reads one frame before `deadline`.
    ///
    /// An idle deadline is retryable, while a partial frame at the deadline is
    /// a typed failure so callers cannot silently discard an incomplete
    /// envelope.
    pub(super) async fn read_frame_until(
        &self,
        deadline: Instant,
    ) -> std::result::Result<FramePoll, AsyncContentLengthError> {
        match until(deadline, self.frames.recv()).await? {
            Some(Ok(frame)) => frame.map(FramePoll::Frame),
            Some(Err(_closed)) => Ok(FramePoll::Closed),
            None if self.partial_frame.load(Ordering::Acquire) => {
                Err(AsyncContentLengthError::DeadlineElapsed)
            }
            None => Ok(FramePoll::Pending),
        }
    }

    /// The child's exit status if it has exited, without reaping it.
    pub(super) async fn try_wait(&self) -> io::Result<Option<ExitStatus>> {
        let lifeline = self.lifeline.as_ref().ok_or_else(supervisor_gone)?;
        let (reply, status) = async_channel::bounded(1);
        lifeline.send(reply).map_err(|_| supervisor_gone())?;
        status.recv().await.map_err(|_| supervisor_gone())?
    }

    /// Kills the child, then waits until it is reaped and stderr has closed.
    pub(super) async fn terminate(&mut self) -> io::Result<ExitStatus> {
        drop(self.lifeline.take());
        let status = self.exited.recv().await.map_err(|_| supervisor_gone())?;
        let _ = self.stderr_closed.recv().await;
        status
    }

    /// The first stderr bytes the child wrote, as trimmed text.
    pub(super) fn stderr(&self) -> String {
        self.stderr
            .lock()
            .map(|captured| String::from_utf8_lossy(&captured).trim().to_owned())
            .unwrap_or_default()
    }
}

fn supervisor_gone() -> io::Error {
    io::Error::other("LSP server supervisor exited")
}

fn spawn_thread(thread: &str, name: &str, body: impl FnOnce() + Send + 'static) -> Result<()> {
    std::thread::Builder::new()
        .name(thread.to_owned())
        .spawn(body)
        .map(drop)
        .map_err(|error| TraceDecayError::Config {
            message: format!("failed to start {thread} thread for LSP server '{name}': {error}"),
        })
}

/// Spawns the child, answers status requests until the lifeline closes, then
/// kills and reaps the child.
fn supervise(
    mut command: Command,
    started: &async_channel::Sender<io::Result<Pipes>>,
    requests: &mpsc::Receiver<StatusRequest>,
    exited: &async_channel::Sender<io::Result<ExitStatus>>,
    mut stdout_stop: ChildPipeStop,
    mut stderr_stop: ChildPipeStop,
) {
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = started.send_blocking(Err(error));
            return;
        }
    };
    let pipes = (child.stdin.take(), child.stdout.take(), child.stderr.take());
    let _ = started.send_blocking(Ok(pipes));
    while let Ok(reply) = requests.recv() {
        let _ = reply.send_blocking(child.try_wait());
    }
    let status = reap_child(&mut child);
    // The child is reaped, so its own writes are done. A descendant can still
    // hold the pipes; stopping the readers lets `terminate` finish.
    stdout_stop.abandon();
    stderr_stop.abandon();
    let _ = exited.send_blocking(status);
}

/// Kills `child` if it is still running, then reaps it.
///
/// `try_wait` reaps a child that has already exited. Windows `kill` returns
/// `InvalidInput` for an exited child, including one that exits between
/// `try_wait` and `kill`, so that error still has to `wait`.
fn reap_child(child: &mut Child) -> io::Result<ExitStatus> {
    match child.try_wait()? {
        Some(status) => Ok(status),
        None => match child.kill() {
            Ok(()) => child.wait(),
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => child.wait(),
            Err(error) => Err(error),
        },
    }
}

/// Decodes strict frames from `stdout` until EOF, the first failure, or the
/// client hanging up. The channel holds one frame, so an unread stdout
/// backpressures the analyzer as an unpolled reader did.
fn read_frames(
    mut read: impl FnMut(&mut [u8]) -> io::Result<usize>,
    frames: &async_channel::Sender<FrameResult>,
    partial: &AtomicBool,
) {
    let mut codec = ContentLengthCodec::new();
    let mut buffered = BytesMut::new();
    let mut chunk = vec![0; 8 * 1024];
    loop {
        let decoded = match codec.decode(&mut buffered) {
            Ok(None) => {
                partial.store(!buffered.is_empty(), Ordering::Release);
                match read(&mut chunk) {
                    Ok(0) => match codec.decode_eof(&mut buffered) {
                        Ok(None) => return,
                        decoded => decoded,
                    },
                    Ok(read) => {
                        buffered.extend_from_slice(&chunk[..read]);
                        continue;
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => Err(error.into()),
                }
            }
            decoded => decoded,
        };
        // Publish the buffer the reader will see before it observes this
        // outcome. Otherwise a finished envelope still looks partial, and the
        // next deadline reports `DeadlineElapsed` for an idle pipe.
        partial.store(!buffered.is_empty(), Ordering::Release);
        let failed = decoded.is_err();
        if let Some(frame) = decoded.transpose()
            && frames.send_blocking(frame).is_err()
        {
            return;
        }
        if failed {
            return;
        }
    }
}

/// Keeps the first `STDERR_CAPTURE_BYTES` and drains the rest, so a chatty
/// analyzer never blocks on a full stderr pipe.
fn drain_stderr(mut read: impl FnMut(&mut [u8]) -> io::Result<usize>, capture: &Mutex<Vec<u8>>) {
    let mut buffer = [0_u8; 1024];
    loop {
        let read = match read(&mut buffer) {
            Ok(0) => return,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        };
        if let Ok(mut captured) = capture.lock() {
            let remaining = STDERR_CAPTURE_BYTES.saturating_sub(captured.len());
            captured.extend_from_slice(&buffer[..read.min(remaining)]);
        }
    }
}

/// `work`'s output, or `None` once `deadline` passes first. The deadline is a
/// native timer, which wakes any executor through its join's `Waker`.
async fn until<F: Future>(deadline: Instant, work: F) -> io::Result<Option<F::Output>> {
    let mut work = pin!(work);
    let mut due = pin!(native_runtime::sleep_until(deadline));
    poll_fn(|task| {
        if let Poll::Ready(output) = work.as_mut().poll(task) {
            return Poll::Ready(Ok(Some(output)));
        }
        due.as_mut()
            .poll(task)
            .map(|fired| fired.map(|()| None).map_err(io::Error::other))
    })
    .await
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::time::Duration;

    use super::*;

    /// A pipe whose bytes arrive when the test sends them.
    struct Feed(mpsc::Receiver<Vec<u8>>);

    impl Read for Feed {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let Ok(bytes) = self.0.recv() else {
                return Ok(0);
            };
            buffer[..bytes.len()].copy_from_slice(&bytes);
            Ok(bytes.len())
        }
    }

    fn process_reading(feed: Feed) -> AnalyzerProcess {
        let (frame_sender, frames) = async_channel::bounded(1);
        let partial_frame = Arc::new(AtomicBool::new(false));
        let partial = Arc::clone(&partial_frame);
        std::thread::spawn(move || {
            let mut feed = feed;
            read_frames(
                &mut |buf: &mut [u8]| feed.read(buf),
                &frame_sender,
                &partial,
            );
        });
        let (_, exited) = async_channel::bounded(1);
        let (writes, _) = async_channel::bounded(1);
        let (_, stderr_closed) = async_channel::bounded(1);
        AnalyzerProcess {
            lifeline: None,
            exited,
            writes,
            frames,
            partial_frame,
            stderr: Arc::default(),
            stderr_closed,
        }
    }

    #[test]
    fn read_deadline_distinguishes_idle_partial_and_closed() {
        let (send, receive) = mpsc::channel();
        let process = process_reading(Feed(receive));
        let read = |wait| {
            native_runtime::block_on(process.read_frame_until(Instant::now() + wait)).unwrap()
        };
        let idle = Duration::from_millis(20);
        let settled = Duration::from_secs(10);

        assert_eq!(read(idle).unwrap(), FramePoll::Pending);

        send.send(b"Content-Length: 2\r\n\r\n{".to_vec()).unwrap();
        while !process.partial_frame.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        let partial = read(idle);
        assert!(
            matches!(partial, Err(AsyncContentLengthError::DeadlineElapsed)),
            "a partial frame at the deadline is a typed failure: {partial:?}"
        );

        send.send(b"}".to_vec()).unwrap();
        assert_eq!(read(settled).unwrap(), FramePoll::Frame(b"{}".to_vec()));
        assert!(
            !process.partial_frame.load(Ordering::Acquire),
            "a completed frame must clear the partial-frame flag before the reader observes it"
        );
        assert_eq!(
            read(idle).unwrap(),
            FramePoll::Pending,
            "an idle pipe after a completed frame is retryable"
        );

        drop(send);
        assert_eq!(read(settled).unwrap(), FramePoll::Closed);
    }

    #[cfg(unix)]
    #[test]
    fn reap_collects_a_child_that_already_exited() {
        let mut child = Command::new("true").spawn().unwrap();
        while child.try_wait().unwrap().is_none() {
            std::thread::yield_now();
        }
        assert!(reap_child(&mut child).unwrap().success());
    }

    #[cfg(unix)]
    #[test]
    fn reap_kills_a_running_child() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let status = reap_child(&mut child).unwrap();
        assert!(!status.success());
    }

    /// A background grandchild inherits the shell's pipes. After the shell
    /// exits, `terminate` has to finish while that grandchild is still alive.
    #[cfg(unix)]
    #[test]
    fn terminate_returns_while_a_grandchild_holds_the_pipes() {
        let pid_path = std::env::temp_dir().join(format!(
            "tracedecay-analyzer-grandchild-{}.pid",
            std::process::id()
        ));
        let script = format!("sleep 30 & echo $! > '{}'; exec true", pid_path.display());
        let mut command = Command::new("/bin/sh");
        command.args(["-c", &script]);
        let mut process = native_runtime::block_on(AnalyzerProcess::spawn(command, "grandchild"))
            .unwrap()
            .unwrap();
        let spawned = Instant::now();
        loop {
            assert!(
                spawned.elapsed() < Duration::from_secs(5),
                "the shell did not exit"
            );
            if native_runtime::block_on(process.try_wait())
                .unwrap()
                .unwrap()
                .is_some()
            {
                break;
            }
            std::thread::yield_now();
        }
        let pid: u32 = std::fs::read_to_string(&pid_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let _cleanup = KillGrandchild {
            pid,
            pid_path: &pid_path,
        };
        assert!(
            Command::new("kill")
                .args(["-0", &pid.to_string()])
                .status()
                .unwrap()
                .success(),
            "the grandchild must still be alive, or the pipes may already be closed"
        );

        let outcome = native_runtime::block_on(until(
            Instant::now() + Duration::from_secs(2),
            process.terminate(),
        ))
        .unwrap()
        .unwrap();
        let Some(Ok(status)) = outcome else {
            panic!("terminate waited on a descendant-held pipe: {outcome:?}");
        };
        assert!(status.success(), "{status}");
    }

    #[cfg(unix)]
    struct KillGrandchild<'a> {
        pid: u32,
        pid_path: &'a std::path::Path,
    }

    #[cfg(unix)]
    impl Drop for KillGrandchild<'_> {
        fn drop(&mut self) {
            let _ = Command::new("kill")
                .args(["-KILL", &self.pid.to_string()])
                .status();
            let _ = std::fs::remove_file(self.pid_path);
        }
    }
}
