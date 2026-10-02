//! How long a store's resident owners take to warm, and the signal a read
//! waits on while a released owner warms again.

use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
pub(super) enum WarmOwner {
    Engine,
    Catalog,
}

#[derive(Debug, Default)]
pub(super) struct WarmClock {
    ledger: Mutex<WarmLedger>,
    settled: Condvar,
}

#[derive(Debug, Default)]
struct WarmLedger {
    engine: OwnerWarm,
    catalog: OwnerWarm,
    /// Bumped by every settled warm, so a waiter that read the owners'
    /// state under an earlier epoch never sleeps through the settle.
    epoch: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct OwnerWarm {
    /// Wall time of the owner's last completed warm.
    measured: Option<Duration>,
    /// When the warm in flight began, including any wait for its build gate.
    started: Option<Instant>,
}

impl WarmLedger {
    fn owner(&mut self, owner: WarmOwner) -> &mut OwnerWarm {
        match owner {
            WarmOwner::Engine => &mut self.engine,
            WarmOwner::Catalog => &mut self.catalog,
        }
    }
}

impl WarmClock {
    fn ledger(&self) -> MutexGuard<'_, WarmLedger> {
        self.ledger.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A warm of `owner` began now, unless one is already in flight.
    pub(super) fn begin(&self, owner: WarmOwner) {
        self.ledger()
            .owner(owner)
            .started
            .get_or_insert_with(Instant::now);
    }

    /// The warm in flight ended; `warmed` records its wall time as the
    /// owner's measured warm-up. Wakes every waiter either way.
    pub(super) fn settle(&self, owner: WarmOwner, warmed: bool) {
        let mut ledger = self.ledger();
        let state = ledger.owner(owner);
        if let Some(started) = state.started.take()
            && warmed
        {
            state.measured = Some(started.elapsed());
        }
        ledger.epoch = ledger.epoch.wrapping_add(1);
        drop(ledger);
        self.settled.notify_all();
    }

    /// Whether `owner` has completed a warm before, so a cold owner now is
    /// one that was released.
    pub(super) fn has_warmed(&self, owner: WarmOwner) -> bool {
        self.ledger().owner(owner).measured.is_some()
    }

    pub(super) fn epoch(&self) -> u64 {
        self.ledger().epoch
    }

    /// The measured warm-up `owner` still needs: its last warm's wall time
    /// less what the warm in flight has already run, or the whole of it
    /// when none is in flight yet.
    pub(super) fn remaining(&self, owner: WarmOwner) -> Duration {
        let mut ledger = self.ledger();
        let state = ledger.owner(owner);
        let measured = state.measured.unwrap_or_default();
        state.started.map_or(measured, |started| {
            measured.saturating_sub(started.elapsed())
        })
    }

    /// Sleep until a warm settles after `epoch` or `timeout` passes.
    pub(super) fn wait_past(&self, epoch: u64, timeout: Duration) {
        let ledger = self.ledger();
        drop(
            self.settled
                .wait_timeout_while(ledger, timeout, |ledger| ledger.epoch == epoch)
                .unwrap_or_else(PoisonError::into_inner),
        );
    }
}
