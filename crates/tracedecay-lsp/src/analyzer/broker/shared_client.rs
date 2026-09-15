//! One analyzer process slot shared by the diagnostics refresh lane and the
//! semantic request lane, together with the supervisor that describes it.
//!
//! Both lanes start, reuse, and retire the same stdio client, so the lifecycle
//! evidence has to live with the slot. A supervisor consulted by only one lane
//! kept reporting `Ready` after the other lane had retired the process, and
//! served the replacement that lane started on the retired incarnation's
//! attempt.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex as SyncMutex};

use tokio::sync::{Mutex, MutexGuard};

use super::super::client::StdioLspClient;
use super::super::error::{AnalyzerResult, AnalyzerRuntimeError};
use crate::{AdmittedRoot, AnalyzerEvent, AnalyzerState, AnalyzerSupervisor};

type ClientReaper = Pin<Box<dyn Future<Output = AnalyzerResult<()>> + Send>>;

pub(crate) struct SharedAnalyzerClientSlot {
    state: SharedAnalyzerClientSlotState,
}

enum SharedAnalyzerClientSlotState {
    Vacant,
    Live(Box<StdioLspClient>),
    Reaping(ClientReaper),
    ReapFailed(AnalyzerRuntimeError),
}

impl SharedAnalyzerClientSlot {
    fn new() -> Self {
        Self {
            state: SharedAnalyzerClientSlotState::Vacant,
        }
    }

    pub(crate) fn as_ref(&self) -> Option<&StdioLspClient> {
        match &self.state {
            SharedAnalyzerClientSlotState::Live(client) => Some(client.as_ref()),
            SharedAnalyzerClientSlotState::Vacant
            | SharedAnalyzerClientSlotState::Reaping(_)
            | SharedAnalyzerClientSlotState::ReapFailed(_) => None,
        }
    }

    pub(crate) fn take(&mut self) -> Option<StdioLspClient> {
        match std::mem::replace(&mut self.state, SharedAnalyzerClientSlotState::Vacant) {
            SharedAnalyzerClientSlotState::Live(client) => Some(*client),
            SharedAnalyzerClientSlotState::Vacant => None,
            SharedAnalyzerClientSlotState::Reaping(reaping) => {
                self.state = SharedAnalyzerClientSlotState::Reaping(reaping);
                None
            }
            SharedAnalyzerClientSlotState::ReapFailed(error) => {
                self.state = SharedAnalyzerClientSlotState::ReapFailed(error);
                None
            }
        }
    }

    pub(crate) fn replace(&mut self, client: StdioLspClient) {
        self.state = SharedAnalyzerClientSlotState::Live(Box::new(client));
    }

    pub(crate) fn retire(&mut self, client: StdioLspClient) {
        self.state = SharedAnalyzerClientSlotState::Reaping(Box::pin(client.reap()));
    }
}

pub(crate) struct SharedAnalyzerClient {
    client: Mutex<SharedAnalyzerClientSlot>,
    supervisor: SyncMutex<AnalyzerSupervisor>,
}

impl SharedAnalyzerClient {
    pub(crate) fn new(root: AdmittedRoot) -> Arc<Self> {
        Arc::new(Self {
            client: Mutex::new(SharedAnalyzerClientSlot::new()),
            supervisor: SyncMutex::new(AnalyzerSupervisor::new(root)),
        })
    }

    /// The process slot. Holding its lock is what makes a start single-owner:
    /// every lane acquires it before consulting the supervisor. A retired
    /// process remains part of the same slot until it has been reaped, so no
    /// successor can be started or served alongside it.
    pub(crate) async fn client(&self) -> AnalyzerResult<MutexGuard<'_, SharedAnalyzerClientSlot>> {
        let mut slot = self.client.lock().await;
        let reap_result = match &mut slot.state {
            SharedAnalyzerClientSlotState::Reaping(reaper) => Some(reaper.as_mut().await),
            SharedAnalyzerClientSlotState::ReapFailed(error) => return Err(error.clone()),
            SharedAnalyzerClientSlotState::Vacant | SharedAnalyzerClientSlotState::Live(_) => None,
        };
        if let Some(result) = reap_result {
            match result {
                Ok(()) => slot.state = SharedAnalyzerClientSlotState::Vacant,
                Err(error) => {
                    // A reap failure is not proof that the child settled. Keep
                    // the slot closed and return the same failure to later
                    // waiters rather than authorizing a replacement process.
                    slot.state = SharedAnalyzerClientSlotState::ReapFailed(error.clone());
                    return Err(error);
                }
            }
        }
        Ok(slot)
    }

    /// Atomic lifecycle snapshot for readiness surfaces.
    pub(crate) fn supervisor(&self) -> AnalyzerSupervisor {
        self.lock_supervisor().clone()
    }

    /// Whether the analyzer may never be started again from this slot.
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(
            self.lock_supervisor().state(),
            AnalyzerState::Exhausted | AnalyzerState::Unavailable
        )
    }

    /// Claims the next start attempt, returning its generation.
    ///
    /// The generation is the fence: a caller that is dropped inside its start
    /// and returns late carries a generation the supervisor has moved past,
    /// so it can neither conclude nor charge the attempt that replaced it.
    pub(crate) fn begin_start(&self) -> Option<u32> {
        let mut supervisor = self.lock_supervisor();
        let root = supervisor.root().clone();
        if supervisor.state() == AnalyzerState::Ready {
            // `Ready` over an empty slot: the client left without either lane
            // recording it, which no lane does any more, so the only remaining
            // explanation is a process that vanished.
            let _ = supervisor.apply(&root, AnalyzerEvent::Crashed);
        }
        // `Starting` belongs here too. Only one caller can be starting at a
        // time — the client lock this runs under is what enforces that — so
        // reaching here in `Starting` means the caller that owned the previous
        // start was dropped mid-flight and released the lock without
        // concluding the transition. This caller takes the start over; it
        // consumes no restart budget, and a failure from `Starting` still
        // charges one.
        if matches!(
            supervisor.state(),
            AnalyzerState::AwaitingStart | AnalyzerState::RestartBackoff | AnalyzerState::Starting
        ) && supervisor
            .apply(&root, AnalyzerEvent::StartRequested)
            .is_ok_and(|state| state == AnalyzerState::Starting)
        {
            return Some(supervisor.attempt());
        }
        None
    }

    /// The generation a caller that found a live client in the slot is serving
    /// on. It holds the client lock, so that generation is its own.
    pub(crate) fn current_attempt(&self) -> u32 {
        self.lock_supervisor().attempt()
    }

    /// Concludes `attempt`'s start, reporting the generation the caller now
    /// owns, or `None` when `attempt` has been superseded.
    ///
    /// A superseded caller is one that was dropped inside its start and
    /// returned late; it must not mark the replacement's attempt ready, and
    /// its client must not be installed over the replacement's.
    pub(crate) fn mark_ready(&self, attempt: u32) -> Option<u32> {
        let mut supervisor = self.lock_supervisor();
        if supervisor.attempt() != attempt {
            return None;
        }
        let root = supervisor.root().clone();
        if supervisor.state() == AnalyzerState::Starting {
            let _ = supervisor.apply(&root, AnalyzerEvent::Ready);
        }
        Some(supervisor.attempt())
    }

    /// Records `event` against `attempt`, ignoring it when that attempt has
    /// been superseded: an abandoned start's late failure is not the
    /// replacement's, and charging it would spend a budget the live process
    /// never earned.
    pub(crate) fn record(&self, attempt: u32, event: AnalyzerEvent) {
        let mut supervisor = self.lock_supervisor();
        if supervisor.attempt() != attempt {
            return;
        }
        let root = supervisor.root().clone();
        let _ = supervisor.apply(&root, event);
    }

    fn lock_supervisor(&self) -> std::sync::MutexGuard<'_, AnalyzerSupervisor> {
        self.supervisor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::future::poll_fn;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::sync::{mpsc, oneshot};

    use super::*;

    #[test]
    fn stale_refresh_retirement_cannot_retire_a_newer_incarnation() {
        let shared = SharedAnalyzerClient::new(AdmittedRoot::new("file:///project"));
        let stale = shared.begin_start().expect("stale refresh attempt");
        let current = shared.begin_start().expect("replacement attempt");
        shared.mark_ready(current).expect("replacement ready");

        shared.record(stale, AnalyzerEvent::Retired);

        let readiness = shared.supervisor();
        assert_eq!(readiness.state(), AnalyzerState::Ready);
        assert_eq!(readiness.attempt(), current);
        assert_eq!(readiness.restart_attempts(), 0);
        assert_eq!(readiness.last_failure(), None);
    }

    #[tokio::test]
    async fn cancelled_waiter_leaves_retired_client_reaper_owned_until_settlement() {
        let shared = SharedAnalyzerClient::new(AdmittedRoot::new("file:///project"));
        let (release, released) = oneshot::channel();
        let (polled, mut reaper_polls) = mpsc::unbounded_channel();
        {
            let mut slot = shared.client.lock().await;
            slot.state = SharedAnalyzerClientSlotState::Reaping(Box::pin(async move {
                let mut released = Box::pin(released);
                poll_fn(move |context| {
                    let _ = polled.send(());
                    released.as_mut().poll(context)
                })
                .await
                .map_err(|_| AnalyzerRuntimeError::new("reaper release was dropped"))?;
                Ok(())
            }));
        }

        let first_waiter = tokio::spawn({
            let shared = Arc::clone(&shared);
            async move {
                let _slot = shared.client().await.unwrap();
            }
        });
        reaper_polls.recv().await.unwrap();
        first_waiter.abort();
        assert!(first_waiter.await.unwrap_err().is_cancelled());

        let (acquired, mut observed) = oneshot::channel();
        let second_waiter = tokio::spawn({
            let shared = Arc::clone(&shared);
            async move {
                let _slot = shared.client().await.unwrap();
                let _ = acquired.send(());
            }
        });
        tokio::select! {
            poll = reaper_polls.recv() => {
                assert!(poll.is_some(), "the retained reaper must be polled by the next waiter");
            }
            acquired = &mut observed => {
                panic!("a successor acquired the slot before the retired process settled: {acquired:?}");
            }
        }

        release.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), &mut observed)
            .await
            .expect("successor should acquire the reaped slot")
            .unwrap();
        second_waiter.await.unwrap();
    }

    #[tokio::test]
    async fn reaper_error_remains_owned_and_blocks_later_clients() {
        let shared = SharedAnalyzerClient::new(AdmittedRoot::new("file:///project"));
        let reap_count = Arc::new(AtomicUsize::new(0));
        {
            let mut slot = shared.client.lock().await;
            let reap_count = Arc::clone(&reap_count);
            slot.state = SharedAnalyzerClientSlotState::Reaping(Box::pin(async move {
                reap_count.fetch_add(1, Ordering::SeqCst);
                Err(AnalyzerRuntimeError::new("retained reaper failure"))
            }));
        }

        let Err(first_error) = shared.client().await else {
            panic!("a failed reaper must not make the slot available");
        };
        let Err(second_error) = shared.client().await else {
            panic!("a later waiter must not bypass the failed reaper");
        };

        assert_eq!(first_error, second_error);
        assert_eq!(reap_count.load(Ordering::SeqCst), 1);
    }
}
