//! Provider launch bound to the daemon's lifetime, supervised off any async
//! runtime.
//!
//! The provider leads its own process group so the cancellation ladder can
//! signal its descendants, which also takes it out of any signal aimed at the
//! daemon's group. A daemon killed outright (SIGKILL, OOM) runs no `Drop`, and
//! the provider would keep running in the worktree after restart recovery has
//! fenced its attempt as `process_lost`. On Linux the kernel closes that gap:
//! `PR_SET_PDEATHSIG` SIGKILLs the provider when the thread that forked it
//! exits.
//!
//! That parent is a *thread*, so the fork runs on a dedicated lifeline thread
//! that also owns the child: it alone waits for it and signals it, so a signal
//! can never reach a reaped (and possibly reused) process id. The pipes are
//! drained by their own threads. The attempt's async side talks to the
//! lifeline only through channels, which wake through ordinary `Waker`s on any
//! executor.
//!
//! A descendant can keep the pipes open after the provider exits. The lifeline
//! therefore keeps the group signalable until the readers finish, and
//! [`ProviderProcess::abandon_output`] releases readers whose pipe never
//! closes, so the attempt's deadline and cancellation bound the whole drain.

use std::io::{self, Write};
use std::process::{Child, ChildStdin, Command, ExitStatus};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{
    Arc,
    mpsc::{self, RecvTimeoutError},
};
use std::time::Duration;

use tokio::sync::{mpsc as async_mpsc, oneshot};

use super::TerminationSignal;
use super::provider_output::{CapturedPipe, read_capped};

/// How long the lifeline waits for a control message before checking whether
/// the provider exited.
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(20);

type Captured = Option<(Vec<u8>, u64)>;

enum Control {
    /// The attempt is durably `Running`: write its instructions to stdin.
    Start(Vec<u8>),
    Signal(TerminationSignal),
}

#[derive(Clone, Copy)]
enum Stream {
    Stdout,
    Stderr,
}

/// A live provider. Dropping it SIGKILLs the provider's process group.
pub(super) struct ProviderProcess {
    control: mpsc::Sender<Control>,
    exited: oneshot::Receiver<io::Result<ExitStatus>>,
    exit: Option<Result<ExitStatus, io::ErrorKind>>,
    captures: async_mpsc::UnboundedReceiver<(Stream, Captured)>,
    undrained: u8,
    stdout: Captured,
    stderr: Captured,
    stop_readers: Arc<AtomicBool>,
}

impl Drop for ProviderProcess {
    fn drop(&mut self) {
        self.abandon_output();
    }
}

impl ProviderProcess {
    /// Spawns `command` on a lifeline thread, capturing at most `stdout_cap`
    /// and `stderr_cap` bytes. Stdin stays open and empty until [`Self::start`].
    pub(super) async fn spawn(
        mut command: Command,
        stdout_cap: u64,
        stderr_cap: u64,
    ) -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        arm_parent_death_signal(&mut command);
        let (spawned_tx, spawned) = oneshot::channel();
        let (control, control_rx) = mpsc::channel();
        let (exited_tx, exited) = oneshot::channel();
        let (captures_tx, captures) = async_mpsc::unbounded_channel();
        let stop_readers = Arc::new(AtomicBool::new(false));
        let live_readers = Arc::new(AtomicUsize::new(2));
        let stop = Arc::clone(&stop_readers);
        std::thread::Builder::new()
            .name("work-provider-lifeline".to_owned())
            .spawn(move || {
                let mut child = match command.spawn() {
                    Ok(child) => child,
                    Err(error) => {
                        let _ = spawned_tx.send(Err(error));
                        return;
                    }
                };
                let handles = ReaderHandles {
                    captured: captures_tx,
                    stop,
                    live: Arc::clone(&live_readers),
                };
                let readers = spawn_reader(
                    Stream::Stdout,
                    child.stdout.take(),
                    stdout_cap,
                    handles.clone(),
                )
                .and_then(|()| {
                    spawn_reader(Stream::Stderr, child.stderr.take(), stderr_cap, handles)
                });
                if let Err(error) = readers {
                    terminate(&mut child, TerminationSignal::Kill);
                    let _ = child.wait();
                    let _ = spawned_tx.send(Err(error));
                    return;
                }
                let _ = spawned_tx.send(Ok(()));
                let group = child.id();
                let _ = exited_tx.send(supervise(&mut child, &control_rx));
                hold_group(group, &control_rx, &live_readers);
            })?;
        spawned
            .await
            .map_err(|_| io::Error::other("provider lifeline thread exited before spawning"))??;
        Ok(Self {
            control,
            exited,
            exit: None,
            captures,
            undrained: 2,
            stdout: None,
            stderr: None,
            stop_readers,
        })
    }

    pub(super) fn start(&self, instructions: Vec<u8>) {
        let _ = self.control.send(Control::Start(instructions));
    }

    pub(super) fn signal(&self, signal: TerminationSignal) {
        let _ = self.control.send(Control::Signal(signal));
    }

    /// Waits for the provider to exit. Cancel-safe and repeatable.
    pub(super) async fn wait(&mut self) -> io::Result<ExitStatus> {
        let exit = match self.exit {
            Some(exit) => exit,
            None => {
                let exit = (&mut self.exited).await.map_err(|_| {
                    io::Error::other("provider lifeline thread exited before the provider")
                })?;
                let exit = exit.map_err(|error| error.kind());
                self.exit = Some(exit);
                exit
            }
        };
        exit.map_err(io::Error::from)
    }

    /// Waits until every descendant holding the provider's pipes has closed
    /// them, or [`Self::abandon_output`] released the readers. Cancel-safe and
    /// repeatable.
    pub(super) async fn drain(&mut self) {
        while self.undrained > 0 {
            let Some((stream, captured)) = self.captures.recv().await else {
                break;
            };
            self.undrained -= 1;
            match stream {
                Stream::Stdout => self.stdout = captured,
                Stream::Stderr => self.stderr = captured,
            }
        }
    }

    /// Waits for the provider to exit and its pipes to close.
    pub(super) async fn finish(&mut self) -> io::Result<ExitStatus> {
        let status = self.wait().await;
        self.drain().await;
        status
    }

    /// Tells readers still parked on an open pipe to stop. [`Self::drain`]
    /// then completes promptly with whatever each stream held so far.
    pub(super) fn abandon_output(&self) {
        self.stop_readers.store(true, Ordering::Release);
    }

    /// The captured streams; a stream still undrained reads as absent.
    pub(super) fn output(&mut self) -> (Captured, Captured) {
        (self.stdout.take(), self.stderr.take())
    }
}

/// Runs on the lifeline thread until the provider is reaped. A dropped
/// [`ProviderProcess`] kills it.
fn supervise(child: &mut Child, control: &mpsc::Receiver<Control>) -> io::Result<ExitStatus> {
    let mut stdin = child.stdin.take();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        match control.recv_timeout(EXIT_POLL_INTERVAL) {
            Ok(Control::Start(instructions)) => {
                if let Some(stdin) = stdin.take() {
                    spawn_writer(stdin, instructions);
                }
            }
            Ok(Control::Signal(signal)) => terminate(child, signal),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                terminate(child, TerminationSignal::Kill);
                return child.wait();
            }
        }
    }
}

/// What a reader thread shares with the attempt and the lifeline.
#[derive(Clone)]
struct ReaderHandles {
    captured: async_mpsc::UnboundedSender<(Stream, Captured)>,
    stop: Arc<AtomicBool>,
    live: Arc<AtomicUsize>,
}

fn spawn_reader(
    which: Stream,
    pipe: Option<impl CapturedPipe + Send + 'static>,
    cap: u64,
    handles: ReaderHandles,
) -> io::Result<()> {
    std::thread::Builder::new()
        .name("work-provider-output".to_owned())
        .spawn(move || {
            let captured = read_capped(pipe, cap, &handles.stop);
            // The pipe is closed by now; only then may the lifeline give up
            // the group it kept signalable for this reader.
            handles.live.fetch_sub(1, Ordering::AcqRel);
            let _ = handles.captured.send((which, captured));
        })
        .map(drop)
}

/// Keeps the provider's process group signalable after the leader is reaped,
/// until every reader has finished. The group id cannot name an unrelated
/// process while a member survives, because the kernel does not recycle a pid
/// that is still a process group id. The first `ESRCH` means no member
/// remains, and the lifeline stops signaling.
#[cfg(unix)]
fn hold_group(group: u32, control: &mpsc::Receiver<Control>, live_readers: &AtomicUsize) {
    while live_readers.load(Ordering::Acquire) > 0 {
        let (signal, last) = match control.recv_timeout(EXIT_POLL_INTERVAL) {
            Ok(Control::Signal(signal)) => (signal, false),
            Ok(Control::Start(_)) | Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => (TerminationSignal::Kill, true),
        };
        if !signal_group(group, signal) || last {
            return;
        }
    }
}

#[cfg(not(unix))]
fn hold_group(_group: u32, _control: &mpsc::Receiver<Control>, _live_readers: &AtomicUsize) {}

/// Writes off the lifeline so a provider that never reads stdin cannot stall
/// the cancellation ladder.
fn spawn_writer(mut stdin: ChildStdin, instructions: Vec<u8>) {
    let written = std::thread::Builder::new()
        .name("work-provider-stdin".to_owned())
        .spawn(move || {
            if let Err(error) = stdin.write_all(&instructions) {
                tracing::debug!(?error, "work attempt provider closed stdin early");
            }
        });
    if let Err(error) = written {
        tracing::warn!(?error, "work attempt provider stdin could not be written");
    }
}

#[cfg(target_os = "linux")]
fn arm_parent_death_signal(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    let daemon = std::process::id();
    // SAFETY: the hook runs between fork and exec and issues only the
    // async-signal-safe `prctl` and `getppid` syscalls, without allocating.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(io::Error::last_os_error());
            }
            // A daemon that died before the signal was armed never delivers it.
            if std::os::unix::process::parent_id() != daemon {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            Ok(())
        });
    }
}

#[cfg(unix)]
fn terminate(child: &mut Child, signal: TerminationSignal) {
    // The child leads its own process group; signal the whole group so
    // provider-spawned descendants observe the ladder too. The child is not
    // yet reaped, so its group id cannot have been reused.
    if !signal_group(child.id(), signal) {
        let _ = child.kill();
    }
}

/// Signals a process group this thread still owns. False when the group is
/// gone or the id is out of range.
#[cfg(unix)]
fn signal_group(group: u32, signal: TerminationSignal) -> bool {
    let signal = match signal {
        TerminationSignal::Interrupt => libc::SIGINT,
        TerminationSignal::Kill => libc::SIGKILL,
    };
    let Ok(group) = libc::pid_t::try_from(group) else {
        return false;
    };
    // SAFETY: killpg with a constant signal number has no memory-safety
    // obligations.
    unsafe { libc::killpg(group, signal) == 0 }
}

#[cfg(not(unix))]
fn terminate(child: &mut Child, _signal: TerminationSignal) {
    let _ = child.kill();
}
