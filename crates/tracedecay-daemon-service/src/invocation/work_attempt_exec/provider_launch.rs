//! Provider launch bound to the daemon's lifetime.
//!
//! The provider leads its own process group so the cancellation ladder can
//! signal its descendants, which also takes it out of any signal aimed at the
//! daemon's group. A daemon killed outright (SIGKILL, OOM) runs no `Drop`, so
//! `kill_on_drop` never fires, and the provider would keep running in the
//! worktree after restart recovery has fenced its attempt as `process_lost`.
//! On Linux the kernel closes that gap: `PR_SET_PDEATHSIG` SIGKILLs the
//! provider when the thread that forked it exits.
//!
//! That parent is a *thread*. Tokio workers are pool threads that
//! `block_in_place` can retire while the daemon lives, so the fork runs on a
//! dedicated thread that stays parked until the attempt drops its
//! [`ProviderLifeline`].

use std::io;

use tokio::process::{Child, Command};

/// Keeps the provider's parent thread alive. Dropping it while the provider
/// still runs SIGKILLs the provider, matching `kill_on_drop`.
pub(super) struct ProviderLifeline {
    #[cfg(target_os = "linux")]
    _release: std::sync::mpsc::Sender<()>,
}

#[cfg(target_os = "linux")]
pub(super) async fn spawn_provider(mut command: Command) -> io::Result<(Child, ProviderLifeline)> {
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
    let runtime = tokio::runtime::Handle::current();
    let (spawned_tx, spawned_rx) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    std::thread::Builder::new()
        .name("work-provider-lifeline".to_owned())
        .spawn(move || {
            let _runtime = runtime.enter();
            let _ = spawned_tx.send(command.spawn());
            // Returns once the lifeline drops; this thread's exit is the
            // provider's parent-death signal.
            let _ = released.recv();
        })?;
    let child = spawned_rx
        .await
        .map_err(|_| io::Error::other("provider lifeline thread exited before spawning"))??;
    Ok((child, ProviderLifeline { _release: release }))
}

#[cfg(not(target_os = "linux"))]
pub(super) async fn spawn_provider(mut command: Command) -> io::Result<(Child, ProviderLifeline)> {
    command.spawn().map(|child| (child, ProviderLifeline {}))
}
