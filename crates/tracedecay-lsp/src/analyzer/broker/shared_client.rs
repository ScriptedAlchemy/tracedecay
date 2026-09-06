//! One analyzer process slot shared by the diagnostics refresh lane and the
//! semantic request lane, together with the supervisor that describes it.
//!
//! Both lanes start, reuse, and retire the same stdio client, so the lifecycle
//! evidence has to live with the slot. A supervisor consulted by only one lane
//! kept reporting `Ready` after the other lane had retired the process, and
//! served the replacement that lane started on the retired incarnation's
//! attempt.

use std::sync::{Arc, Mutex as SyncMutex};

use tokio::sync::Mutex;

use super::super::client::StdioLspClient;
use crate::{AdmittedRoot, AnalyzerEvent, AnalyzerState, AnalyzerSupervisor};

pub(crate) struct SharedAnalyzerClient {
    client: Mutex<Option<StdioLspClient>>,
    supervisor: SyncMutex<AnalyzerSupervisor>,
}

impl SharedAnalyzerClient {
    pub(crate) fn new(root: AdmittedRoot) -> Arc<Self> {
        Arc::new(Self {
            client: Mutex::new(None),
            supervisor: SyncMutex::new(AnalyzerSupervisor::new(root)),
        })
    }

    /// The process slot. Holding its lock is what makes a start single-owner:
    /// every lane locks it before consulting the supervisor.
    pub(crate) fn client(&self) -> &Mutex<Option<StdioLspClient>> {
        &self.client
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
