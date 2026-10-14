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

use std::io::{self, Read, Write};
use std::process::{Child, ChildStdin, Command, ExitStatus};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use tokio::sync::oneshot;

use super::TerminationSignal;
use super::provider_output::read_capped;

/// How long the lifeline waits for a control message before checking whether
/// the provider exited.
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(20);

type Captured = Option<(Vec<u8>, u64)>;

enum Control {
    /// The attempt is durably `Running`: write its instructions to stdin.
    Start(Vec<u8>),
    Signal(TerminationSignal),
}

/// A live provider. Dropping it SIGKILLs the provider's process group.
pub(super) struct ProviderProcess {
    control: mpsc::Sender<Control>,
    exited: oneshot::Receiver<io::Result<ExitStatus>>,
    stdout: oneshot::Receiver<Captured>,
    stderr: oneshot::Receiver<Captured>,
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
        let (stdout_tx, stdout) = oneshot::channel();
        let (stderr_tx, stderr) = oneshot::channel();
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
                let readers = spawn_reader(child.stdout.take(), stdout_cap, stdout_tx)
                    .and_then(|()| spawn_reader(child.stderr.take(), stderr_cap, stderr_tx));
                if let Err(error) = readers {
                    terminate(&mut child, TerminationSignal::Kill);
                    let _ = child.wait();
                    let _ = spawned_tx.send(Err(error));
                    return;
                }
                let _ = spawned_tx.send(Ok(()));
                let _ = exited_tx.send(supervise(&mut child, &control_rx));
            })?;
        spawned
            .await
            .map_err(|_| io::Error::other("provider lifeline thread exited before spawning"))??;
        Ok(Self {
            control,
            exited,
            stdout,
            stderr,
        })
    }

    pub(super) fn start(&self, instructions: Vec<u8>) {
        let _ = self.control.send(Control::Start(instructions));
    }

    pub(super) fn signal(&self, signal: TerminationSignal) {
        let _ = self.control.send(Control::Signal(signal));
    }

    /// Waits for the provider to exit. Cancel-safe; never call it again once
    /// it has returned.
    pub(super) async fn wait(&mut self) -> io::Result<ExitStatus> {
        (&mut self.exited)
            .await
            .map_err(|_| io::Error::other("provider lifeline thread exited before the provider"))?
    }

    /// The captured streams, once the provider and every descendant holding
    /// its pipes have closed them.
    pub(super) async fn output(self) -> (Captured, Captured) {
        let Self { stdout, stderr, .. } = self;
        (stdout.await.ok().flatten(), stderr.await.ok().flatten())
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

fn spawn_reader(
    stream: Option<impl Read + Send + 'static>,
    cap: u64,
    captured: oneshot::Sender<Captured>,
) -> io::Result<()> {
    std::thread::Builder::new()
        .name("work-provider-output".to_owned())
        .spawn(move || {
            let _ = captured.send(read_capped(stream, cap));
        })
        .map(drop)
}

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
    let signal = match signal {
        TerminationSignal::Interrupt => libc::SIGINT,
        TerminationSignal::Kill => libc::SIGKILL,
    };
    let Ok(pid) = libc::pid_t::try_from(child.id()) else {
        let _ = child.kill();
        return;
    };
    // The child leads its own process group; signal the whole group so
    // provider-spawned descendants observe the ladder too. The child is not
    // yet reaped, so its group id cannot have been reused.
    // SAFETY: killpg with a pid owned by this thread and a constant signal
    // number has no memory-safety obligations.
    if unsafe { libc::killpg(pid, signal) } != 0 {
        let _ = child.kill();
    }
}

#[cfg(not(unix))]
fn terminate(child: &mut Child, _signal: TerminationSignal) {
    let _ = child.kill();
}
