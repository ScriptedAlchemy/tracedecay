//! Bounded retry policy for a background reconcile pass whose blocking task
//! panicked.
//!
//! A reconcile fans per-file work across the indexing pool over arbitrary user
//! source. A panic there aborts the pass and surfaces as an opaque `JoinError`
//! on the worker loop. The loop then restored the pending arrival and waited
//! for the next wake — and because the offending input is still on disk, the
//! next wake reproduced the identical panic. One malformed file therefore
//! blocked a project's entire code index indefinitely, retrying forever with
//! no backoff and no terminal state.
//!
//! This policy makes that failure degrade instead of loop: repeated panics
//! back off exponentially, and after a bounded number of consecutive panics
//! the worker stops re-attempting until the input actually changes (the
//! code-index control epoch advances) or a pass makes progress. The shape
//! mirrors the sealed-generation activation backoff already used by the
//! registry worker; tests shrink the clock, not the shape.

use std::time::Duration;

use tokio::time::Instant;

/// Bounded exponential backoff between reconcile retries after a panicking
/// pass. The floor keeps a transient panic from hot-looping the pool; the
/// ceiling keeps a persistently panicking input from being retried more than a
/// few times an hour.
pub const RECONCILE_PANIC_BACKOFF_FLOOR: Duration = if cfg!(any(test, feature = "test-helpers")) {
    Duration::from_millis(50)
} else {
    Duration::from_secs(30)
};
pub const RECONCILE_PANIC_BACKOFF_CEILING: Duration = if cfg!(any(test, feature = "test-helpers")) {
    Duration::from_millis(400)
} else {
    Duration::from_mins(10)
};

/// Consecutive panics over unchanged input after which retrying is pointless:
/// the same bytes reproduce the same panic. Further attempts are suppressed
/// until the input changes.
pub const MAX_CONSECUTIVE_RECONCILE_PANICS_V1: u32 = 4;

/// What the worker loop should do after a reconcile pass panicked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconcilePanicDecisionV1 {
    /// Re-arm a wake after this delay and try the same input again.
    RetryAfter(Duration),
    /// The bound is exhausted. Stop re-arming; only changed input or a
    /// progressing pass resumes reconciles.
    Quarantine,
}

/// Consecutive-panic accounting for one mounted worktree's worker loop.
#[derive(Debug)]
pub struct ReconcilePanicGuardV1 {
    consecutive_panics: u32,
    backoff: Duration,
    next_attempt_at: Option<Instant>,
    quarantined: bool,
    /// Control epoch observed when the guard last quarantined. An advance
    /// means new input, which is worth one more attempt.
    quarantined_at_epoch: u64,
}

impl Default for ReconcilePanicGuardV1 {
    fn default() -> Self {
        Self::new()
    }
}

impl ReconcilePanicGuardV1 {
    #[hotpath::skip]
    pub const fn new() -> Self {
        Self {
            consecutive_panics: 0,
            backoff: RECONCILE_PANIC_BACKOFF_FLOOR,
            next_attempt_at: None,
            quarantined: false,
            quarantined_at_epoch: 0,
        }
    }

    /// Any pass that completed without panicking clears the accounting: the
    /// next panic starts again at the floor.
    pub fn record_progress(&mut self) {
        self.consecutive_panics = 0;
        self.backoff = RECONCILE_PANIC_BACKOFF_FLOOR;
        self.next_attempt_at = None;
        self.quarantined = false;
    }

    /// Record a panicking pass and decide whether the same input is worth
    /// another attempt.
    pub fn record_panic(&mut self, now: Instant, epoch: u64) -> ReconcilePanicDecisionV1 {
        self.consecutive_panics = self.consecutive_panics.saturating_add(1);
        if self.consecutive_panics >= MAX_CONSECUTIVE_RECONCILE_PANICS_V1 {
            hotpath::gauge!("daemon.code_index.reconcile.panic.quarantined_total").inc(1_u64);
            self.quarantined = true;
            self.quarantined_at_epoch = epoch;
            self.next_attempt_at = None;
            return ReconcilePanicDecisionV1::Quarantine;
        }
        hotpath::gauge!("daemon.code_index.reconcile.panic.retry_total").inc(1_u64);
        let delay = self.backoff;
        self.next_attempt_at = Some(now + delay);
        self.backoff = self
            .backoff
            .saturating_mul(2)
            .min(RECONCILE_PANIC_BACKOFF_CEILING);
        ReconcilePanicDecisionV1::RetryAfter(delay)
    }

    /// Consecutive panics observed since the last progressing pass. Reported
    /// on the warn path so an operator sees a bounded counter rather than an
    /// undifferentiated repeating line.
    #[hotpath::skip]
    pub const fn consecutive_panics(&self) -> u32 {
        self.consecutive_panics
    }

    /// True while this wake must be skipped: either the backoff window is
    /// still open, or the guard is quarantined and the input has not changed.
    pub fn suppresses_pass(&mut self, now: Instant, epoch: u64) -> bool {
        if self.quarantined {
            if epoch != self.quarantined_at_epoch {
                // New input is not the input that panicked; allow one attempt
                // and let it re-quarantine if it panics again.
                self.quarantined = false;
                self.consecutive_panics = 0;
                self.backoff = RECONCILE_PANIC_BACKOFF_FLOOR;
                self.next_attempt_at = None;
                return false;
            }
            hotpath::gauge!("daemon.code_index.reconcile.panic.suppressed_wakes_total").inc(1_u64);
            return true;
        }
        let suppressed = self.next_attempt_at.is_some_and(|at| now < at);
        if suppressed {
            hotpath::gauge!("daemon.code_index.reconcile.panic.backoff_wakes_total").inc(1_u64);
        }
        suppressed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin() -> Instant {
        Instant::now()
    }

    #[test]
    fn repeated_panics_back_off_and_then_quarantine() {
        let mut guard = ReconcilePanicGuardV1::new();
        let now = origin();

        let mut delays = Vec::new();
        let mut decisions = Vec::new();
        for _ in 0..MAX_CONSECUTIVE_RECONCILE_PANICS_V1 {
            let decision = guard.record_panic(now, 7);
            decisions.push(decision);
            if let ReconcilePanicDecisionV1::RetryAfter(delay) = decision {
                delays.push(delay);
            }
        }

        assert_eq!(
            decisions.last(),
            Some(&ReconcilePanicDecisionV1::Quarantine),
            "an unchanged panicking input must reach a terminal state, not retry forever"
        );
        assert_eq!(
            delays.len() as u32,
            MAX_CONSECUTIVE_RECONCILE_PANICS_V1 - 1,
            "every attempt before the bound is a delayed retry"
        );
        assert!(
            delays.windows(2).all(|pair| pair[1] > pair[0]),
            "retries must back off, not repeat identically: {delays:?}"
        );
        assert!(
            delays
                .iter()
                .all(|delay| *delay >= RECONCILE_PANIC_BACKOFF_FLOOR
                    && *delay <= RECONCILE_PANIC_BACKOFF_CEILING),
            "delays stay inside the bounded window: {delays:?}"
        );
    }

    #[test]
    fn a_quarantined_guard_suppresses_further_passes_over_unchanged_input() {
        let mut guard = ReconcilePanicGuardV1::new();
        let now = origin();
        for _ in 0..MAX_CONSECUTIVE_RECONCILE_PANICS_V1 {
            guard.record_panic(now, 7);
        }

        let far_future = now + Duration::from_hours(24);
        assert!(
            guard.suppresses_pass(far_future, 7),
            "unchanged input must stay quarantined however long the wait"
        );
    }

    #[test]
    fn changed_input_lifts_the_quarantine() {
        let mut guard = ReconcilePanicGuardV1::new();
        let now = origin();
        for _ in 0..MAX_CONSECUTIVE_RECONCILE_PANICS_V1 {
            guard.record_panic(now, 7);
        }
        assert!(guard.suppresses_pass(now, 7));

        assert!(
            !guard.suppresses_pass(now, 8),
            "an advanced control epoch is new input and earns another attempt"
        );
        assert_eq!(guard.consecutive_panics(), 0, "accounting restarts");
    }

    #[test]
    fn the_backoff_window_suppresses_only_until_it_elapses() {
        let mut guard = ReconcilePanicGuardV1::new();
        let now = origin();
        let ReconcilePanicDecisionV1::RetryAfter(delay) = guard.record_panic(now, 7) else {
            panic!("the first panic must schedule a retry");
        };

        assert!(guard.suppresses_pass(now, 7), "window is open");
        assert!(
            !guard.suppresses_pass(now + delay, 7),
            "the pass runs once the window elapses"
        );
    }

    #[test]
    fn a_progressing_pass_clears_the_accounting() {
        let mut guard = ReconcilePanicGuardV1::new();
        let now = origin();
        for _ in 0..MAX_CONSECUTIVE_RECONCILE_PANICS_V1 {
            guard.record_panic(now, 7);
        }
        assert!(guard.suppresses_pass(now, 7));

        guard.record_progress();

        assert!(!guard.suppresses_pass(now, 7), "progress lifts suppression");
        assert_eq!(guard.consecutive_panics(), 0);
        assert_eq!(
            guard.record_panic(now, 7),
            ReconcilePanicDecisionV1::RetryAfter(RECONCILE_PANIC_BACKOFF_FLOOR),
            "the next panic restarts at the floor"
        );
    }
}

/// Bounded delayed retry for a reconcile pass that failed because shared
/// process capacity was momentarily exhausted.
///
/// A reconcile reserves against one process-wide resident-memory authority. A
/// sibling worktree or artifact build can hold that budget when this pass asks
/// for it, and the request is refused before any indexing work starts. Nothing
/// wakes this worker when the competing holder releases: the failure path only
/// restored the pending arrival, so the worktree stayed stale until an
/// unrelated query or edit happened to wake it.
///
/// The retry is deliberately narrow. A panicking or permanently refused pass
/// reproduces on every attempt, so retrying it forever is the failure this
/// module exists to prevent; only a failure that is *transient by construction*
/// — capacity another holder will release — earns a re-arm, and even that is
/// capped so a genuinely undersized budget degrades to stale instead of
/// spinning.
pub const RECONCILE_CAPACITY_RETRY_FLOOR: Duration = if cfg!(any(test, feature = "test-helpers")) {
    Duration::from_millis(40)
} else {
    Duration::from_secs(2)
};
pub const RECONCILE_CAPACITY_RETRY_CEILING: Duration = if cfg!(any(test, feature = "test-helpers"))
{
    Duration::from_millis(320)
} else {
    Duration::from_mins(1)
};

/// Consecutive capacity refusals after which the shared budget is not
/// momentarily contended but structurally too small for this worktree. Further
/// self-scheduled wakes only burn the pool; the next real hint still retries.
pub const MAX_CONSECUTIVE_CAPACITY_RETRIES_V1: u32 = 5;

/// Consecutive-capacity-refusal accounting for one mounted worktree's worker.
#[derive(Debug)]
pub struct ReconcileCapacityRetryV1 {
    consecutive: u32,
    backoff: Duration,
}

impl Default for ReconcileCapacityRetryV1 {
    fn default() -> Self {
        Self::new()
    }
}

impl ReconcileCapacityRetryV1 {
    #[hotpath::skip]
    pub const fn new() -> Self {
        Self {
            consecutive: 0,
            backoff: RECONCILE_CAPACITY_RETRY_FLOOR,
        }
    }

    /// Any pass that did not fail on capacity clears the accounting.
    pub fn record_progress(&mut self) {
        self.consecutive = 0;
        self.backoff = RECONCILE_CAPACITY_RETRY_FLOOR;
    }

    /// `Some(delay)` arms exactly one delayed wake; `None` means the bound is
    /// spent and this worker stops self-scheduling until real input arrives.
    pub fn record_capacity_failure(&mut self) -> Option<Duration> {
        if self.consecutive >= MAX_CONSECUTIVE_CAPACITY_RETRIES_V1 {
            hotpath::gauge!("daemon.code_index.reconcile.capacity.exhausted_total").inc(1_u64);
            return None;
        }
        hotpath::gauge!("daemon.code_index.reconcile.capacity.retry_total").inc(1_u64);
        self.consecutive = self.consecutive.saturating_add(1);
        let delay = self.backoff;
        self.backoff = self
            .backoff
            .saturating_mul(2)
            .min(RECONCILE_CAPACITY_RETRY_CEILING);
        Some(delay)
    }

    /// Consecutive capacity refusals since the last non-capacity pass.
    #[hotpath::skip]
    pub const fn consecutive(&self) -> u32 {
        self.consecutive
    }
}

/// Deterministic reconcile fault installed by the worker-loop isolation tests.
///
/// The guard types above are pure state machines; on their own they prove
/// nothing about whether the background worker consults them. These tests
/// therefore drive the real registry worker over a real mounted worktree and
/// count the passes it actually attempts, which is only observable if the
/// scheduler can be made to fail on demand.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconcileFaultKindV1 {
    /// Unwinds inside the blocking reconcile task, exactly as a malformed
    /// source file did through `generate_node_id`.
    Panic,
    /// Refused before any indexing work because a sibling worktree or artifact
    /// build was holding the shared resident-memory budget. The request fits
    /// the limit, so releasing that budget makes it admissible.
    TransientCapacity,
    /// Refused because the request is larger than the whole process limit. It
    /// is shaped like a capacity refusal and is not one: no release by any
    /// other holder can ever admit it.
    OversizedCapacity,
    /// A refusal the same input reproduces forever.
    Permanent,
}

#[cfg(test)]
#[derive(Debug)]
pub struct ReconcileFaultInjectionV1 {
    kind: ReconcileFaultKindV1,
    /// Passes to fault before behaving normally; `usize::MAX` never recovers.
    faulting_passes: usize,
    attempts: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl ReconcileFaultInjectionV1 {
    pub const fn new(kind: ReconcileFaultKindV1, faulting_passes: usize) -> Self {
        Self {
            kind,
            faulting_passes,
            attempts: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Reconcile passes the worker actually dispatched, faulting or not.
    pub fn attempts(&self) -> usize {
        self.attempts.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Called at the top of every real reconcile pass.
    pub fn arrive(&self) -> Result<(), super::CodeIndexSchedulerErrorV1> {
        let seen = self
            .attempts
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        if seen >= self.faulting_passes {
            return Ok(());
        }
        match self.kind {
            ReconcileFaultKindV1::Panic => {
                panic!("injected reconcile panic (pass {})", seen + 1)
            }
            // Exactly the shape `ProcessResidentMemoryV1::reserve` returns when
            // the process budget is already spoken for by another holder.
            ReconcileFaultKindV1::TransientCapacity => {
                Err(super::CodeIndexSchedulerErrorV1::WorkerMemoryAdmission(
                    tracedecay_runtime_core::resident_memory::ResidentMemoryAdmissionFailureV1::ReservationCeiling {
                        used_bytes: 900,
                        requested_bytes: 200,
                        limit_bytes: 1_000,
                    },
                ))
            }
            // Same variant, but the request alone exceeds the whole limit.
            ReconcileFaultKindV1::OversizedCapacity => {
                Err(super::CodeIndexSchedulerErrorV1::WorkerMemoryAdmission(
                    tracedecay_runtime_core::resident_memory::ResidentMemoryAdmissionFailureV1::ReservationCeiling {
                        used_bytes: 0,
                        requested_bytes: 4_000,
                        limit_bytes: 1_000,
                    },
                ))
            }
            ReconcileFaultKindV1::Permanent => Err(super::CodeIndexSchedulerErrorV1::Identity(
                "injected permanent reconcile refusal".to_owned(),
            )),
        }
    }
}
