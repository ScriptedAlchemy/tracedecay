//! Separately typed progress authority before immutable generation sealing.
//! Real capture, parse, and resolution callbacks alone advance the work clock.

use std::sync::{Arc, PoisonError, RwLock};

use tracedecay_code_index::production::{CodeIndexPresealWorkPhaseV1, CodeIndexPresealWorkV1};
use tracedecay_contracts::code_index_freshness::CodeIndexPresealAttemptProgressV1;
use tracedecay_contracts::now_micros;

pub(crate) type CodeIndexBuildAttemptSlotV1 = Arc<RwLock<CodeIndexBuildAttemptSlotStateV1>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CodeIndexBuildAttemptIdentityV1 {
    daemon_incarnation: u64,
    producer_incarnation: u64,
    sequence: u64,
}

#[derive(Default)]
pub(crate) struct CodeIndexBuildAttemptSlotStateV1 {
    next_sequence: u64,
    epoch: u64,
    snapshot: Option<CodeIndexPresealAttemptProgressV1>,
}

impl CodeIndexBuildAttemptSlotStateV1 {
    fn begin(
        &mut self,
        daemon_incarnation: u64,
        producer_incarnation: u64,
    ) -> CodeIndexBuildAttemptIdentityV1 {
        self.next_sequence = self.next_sequence.saturating_add(1).max(1);
        self.epoch = self.epoch.saturating_add(1).max(1);
        let identity = CodeIndexBuildAttemptIdentityV1 {
            daemon_incarnation,
            producer_incarnation,
            sequence: self.next_sequence,
        };
        self.snapshot = Some(CodeIndexPresealAttemptProgressV1 {
            daemon_incarnation,
            producer_incarnation,
            attempt_sequence: identity.sequence,
            progress_epoch: self.epoch,
            phase: CodeIndexPresealWorkPhaseV1::Capture,
            completed_items: 0,
            total_items: None,
            last_progress_micros: None,
            active: true,
        });
        identity
    }

    fn owns(&self, identity: CodeIndexBuildAttemptIdentityV1) -> bool {
        self.snapshot.as_ref().is_some_and(|it| {
            it.active
                && it.daemon_incarnation == identity.daemon_incarnation
                && it.producer_incarnation == identity.producer_incarnation
                && it.attempt_sequence == identity.sequence
        })
    }

    fn observe(
        &mut self,
        identity: CodeIndexBuildAttemptIdentityV1,
        work: CodeIndexPresealWorkV1,
        observed_at_micros: i64,
    ) -> bool {
        if !self.owns(identity)
            || work.completed_items == 0
            || work
                .total_items
                .is_some_and(|total| work.completed_items > total)
        {
            return false;
        }
        let Some(current) = self.snapshot.as_ref() else {
            return false;
        };
        let new_phase = current.phase != work.phase;
        // Each phase owns different physical units. Admit only its real next
        // stage; an earlier delayed callback must not roll resolution back.
        if (new_phase
            && !matches!(
                (current.phase, work.phase),
                (
                    CodeIndexPresealWorkPhaseV1::Capture,
                    CodeIndexPresealWorkPhaseV1::ParseEvidence
                ) | (
                    CodeIndexPresealWorkPhaseV1::ParseEvidence,
                    CodeIndexPresealWorkPhaseV1::Resolution
                )
            ))
            || (!new_phase && work.completed_items <= current.completed_items)
        {
            return false;
        }
        let mut next = current.clone();
        next.phase = work.phase;
        next.completed_items = work.completed_items;
        next.total_items = work.total_items;
        next.last_progress_micros = Some(
            observed_at_micros.max(current.last_progress_micros.unwrap_or(observed_at_micros)),
        );
        self.epoch = self.epoch.saturating_add(1).max(1);
        next.progress_epoch = self.epoch;
        self.snapshot = Some(next);
        true
    }

    fn finish(&mut self, identity: CodeIndexBuildAttemptIdentityV1) {
        if !self.owns(identity) {
            return;
        }
        if let Some(value) = self.snapshot.as_mut() {
            value.active = false;
            self.epoch = self.epoch.saturating_add(1).max(1);
            value.progress_epoch = self.epoch;
            // Finishing is a state transition, not completed source work.
            // Its timestamp must remain the timestamp of the last real item.
        }
    }

    /// A reconcile's real work may remain visible during its own durable
    /// publication handoff. Once its guard exits, the live status slot must
    /// not reattribute that observation to a later source wake. The attempt
    /// identity fence protects a newer guard from an older guard's drop.
    fn retire(&mut self, identity: CodeIndexBuildAttemptIdentityV1) {
        if self.snapshot.as_ref().is_some_and(|snapshot| {
            snapshot.daemon_incarnation == identity.daemon_incarnation
                && snapshot.producer_incarnation == identity.producer_incarnation
                && snapshot.attempt_sequence == identity.sequence
        }) {
            self.snapshot = None;
        }
    }

    pub(crate) fn snapshot(&self) -> Option<CodeIndexPresealAttemptProgressV1> {
        self.snapshot.clone()
    }
}

/// A stale or unwinding reconcile can never publish into its successor.
/// Dropping this guard on cancellation, retry, panic or return fences it and
/// retires its reading; only a still-owned durable handoff retains the counts.
pub(super) struct CodeIndexPresealAttemptGuardV1 {
    slot: CodeIndexBuildAttemptSlotV1,
    identity: CodeIndexBuildAttemptIdentityV1,
}

impl CodeIndexPresealAttemptGuardV1 {
    pub(super) fn begin(
        slot: CodeIndexBuildAttemptSlotV1,
        daemon_incarnation: u64,
        producer_incarnation: u64,
    ) -> Self {
        let identity = slot
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .begin(daemon_incarnation, producer_incarnation);
        Self { slot, identity }
    }

    /// The real publication handoff fences late or stale work callbacks.
    /// This transition does not claim a completed source item.
    pub(super) fn finisher(&self) -> Arc<dyn Fn() + Send + Sync> {
        let slot = Arc::clone(&self.slot);
        let identity = self.identity;
        Arc::new(move || {
            slot.write()
                .unwrap_or_else(PoisonError::into_inner)
                .finish(identity);
        })
    }

    pub(super) fn observer(&self) -> Arc<dyn Fn(CodeIndexPresealWorkV1) + Send + Sync> {
        let slot = Arc::clone(&self.slot);
        let identity = self.identity;
        Arc::new(move |work| {
            slot.write()
                .unwrap_or_else(PoisonError::into_inner)
                .observe(identity, work, now_micros().0);
        })
    }
}

impl Drop for CodeIndexPresealAttemptGuardV1 {
    fn drop(&mut self) {
        self.slot
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .retire(self.identity);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_application::code_index::DaemonCodeIndexControlV1;
    use tracedecay_code_index::production::CodeIndexExecutionControlV1;

    #[test]
    fn durable_publication_handoff_retires_attempt_without_forging_work() {
        let slot = Arc::new(RwLock::new(CodeIndexBuildAttemptSlotStateV1::default()));
        let attempt = CodeIndexPresealAttemptGuardV1::begin(Arc::clone(&slot), 7, 11);
        let control = DaemonCodeIndexControlV1::default()
            .with_preseal_observer(attempt.observer())
            .with_preseal_finisher(attempt.finisher());
        control.observed_preseal_work(work(CodeIndexPresealWorkPhaseV1::Capture, 3, None));
        let before = slot.read().unwrap().snapshot().unwrap();
        assert!(before.active);
        assert_eq!(before.completed_items, 3);
        assert!(before.last_progress_micros.is_some());

        // The guard remains alive during durable publication. Retiring it
        // must not renew the work clock, and late callbacks are fenced.
        control.finish_preseal_observation();
        let after = slot.read().unwrap().snapshot().unwrap();
        assert!(!after.active);
        assert_eq!(after.completed_items, 3);
        assert_eq!(after.last_progress_micros, before.last_progress_micros);
        assert!(after.progress_epoch > before.progress_epoch);
        control.observed_preseal_work(work(CodeIndexPresealWorkPhaseV1::ParseEvidence, 1, None));
        control.finish_preseal_observation();
        assert_eq!(slot.read().unwrap().snapshot().unwrap(), after);
        // The completed reconcile no longer owns this progress. A later
        // source hint must not make its old counts appear to be new work.
        drop(attempt);
        assert!(slot.read().unwrap().snapshot().is_none());
    }

    #[test]
    fn completed_handoff_does_not_reappear_during_a_later_source_wake() {
        let slot = Arc::new(RwLock::new(CodeIndexBuildAttemptSlotStateV1::default()));
        let first = CodeIndexPresealAttemptGuardV1::begin(Arc::clone(&slot), 7, 11);
        first.observer()(work(CodeIndexPresealWorkPhaseV1::Resolution, 7, None));
        // A real preseal transition requires capture then parse before
        // resolution. The first observed work must be an actual capture.
        let initial = slot.read().unwrap().snapshot().unwrap();
        assert_eq!(initial.completed_items, 0);
        assert!(initial.last_progress_micros.is_none());

        first.observer()(work(CodeIndexPresealWorkPhaseV1::Capture, 3, Some(3)));
        first.observer()(work(CodeIndexPresealWorkPhaseV1::ParseEvidence, 3, Some(3)));
        first.observer()(work(CodeIndexPresealWorkPhaseV1::Resolution, 7, None));
        first.finisher()();
        let during_handoff = slot.read().unwrap().snapshot().unwrap();
        assert!(!during_handoff.active);
        assert_eq!(during_handoff.completed_items, 7);
        assert!(during_handoff.last_progress_micros.is_some());

        // Source admission may begin before the next producer guard. This
        // slot is the authoritative value copied into a freshness response.
        drop(first);
        assert!(slot.read().unwrap().snapshot().is_none());

        let next = CodeIndexPresealAttemptGuardV1::begin(Arc::clone(&slot), 7, 11);
        let admitted = slot.read().unwrap().snapshot().unwrap();
        assert!(admitted.active);
        assert_eq!(admitted.completed_items, 0);
        assert!(admitted.last_progress_micros.is_none());
        next.observer()(work(CodeIndexPresealWorkPhaseV1::Capture, 2, Some(4)));
        let actual = slot.read().unwrap().snapshot().unwrap();
        assert_eq!(actual.completed_items, 2);
        assert!(actual.last_progress_micros.is_some());
        assert!(actual.attempt_sequence > during_handoff.attempt_sequence);
    }

    #[test]
    fn out_of_order_guard_drop_never_erases_a_newer_attempt() {
        let slot = Arc::new(RwLock::new(CodeIndexBuildAttemptSlotStateV1::default()));
        let first = CodeIndexPresealAttemptGuardV1::begin(Arc::clone(&slot), 3, 5);
        let old_observer = first.observer();
        old_observer(work(CodeIndexPresealWorkPhaseV1::Capture, 4, Some(4)));
        first.finisher()();

        let second = CodeIndexPresealAttemptGuardV1::begin(Arc::clone(&slot), 3, 5);
        second.observer()(work(CodeIndexPresealWorkPhaseV1::Capture, 2, Some(6)));
        let current = slot.read().unwrap().snapshot().unwrap();
        assert!(current.active);
        drop(first);
        old_observer(work(CodeIndexPresealWorkPhaseV1::ParseEvidence, 2, Some(4)));
        assert_eq!(slot.read().unwrap().snapshot().unwrap(), current);

        drop(second);
        assert!(slot.read().unwrap().snapshot().is_none());
    }

    fn work(
        phase: CodeIndexPresealWorkPhaseV1,
        n: u64,
        total: Option<u64>,
    ) -> CodeIndexPresealWorkV1 {
        CodeIndexPresealWorkV1 {
            phase,
            completed_items: n,
            total_items: total,
        }
    }

    #[test]
    fn real_progress_never_renews_on_poll_or_regression() {
        let mut slot = CodeIndexBuildAttemptSlotStateV1::default();
        let id = slot.begin(7, 9);
        assert_eq!(slot.snapshot().unwrap().last_progress_micros, None);
        assert!(slot.observe(
            id,
            work(CodeIndexPresealWorkPhaseV1::Capture, 2, Some(5)),
            100
        ));
        let first = slot.snapshot().unwrap();
        assert_eq!(first.completed_items, 2);
        assert_eq!(first.last_progress_micros, Some(100));
        assert!(!slot.observe(
            id,
            work(CodeIndexPresealWorkPhaseV1::Capture, 2, Some(5)),
            900
        ));
        assert!(!slot.observe(
            id,
            work(CodeIndexPresealWorkPhaseV1::Capture, 1, Some(5)),
            950
        ));
        assert!(!slot.observe(
            id,
            work(CodeIndexPresealWorkPhaseV1::Capture, 6, Some(5)),
            990
        ));
        assert_eq!(slot.snapshot().unwrap(), first);
        assert!(slot.observe(
            id,
            work(CodeIndexPresealWorkPhaseV1::ParseEvidence, 1, Some(3)),
            95
        ));
        let parsed = slot.snapshot().unwrap();
        assert_eq!(parsed.last_progress_micros, Some(100));
        assert!(parsed.progress_epoch > first.progress_epoch);
        assert!(!slot.observe(
            id,
            work(CodeIndexPresealWorkPhaseV1::Capture, 3, Some(5)),
            150
        ));
        slot.finish(id);
        let ended = slot.snapshot().unwrap();
        assert!(!ended.active);
        assert_eq!(ended.last_progress_micros, Some(100));
        assert!(!slot.observe(
            id,
            work(CodeIndexPresealWorkPhaseV1::ParseEvidence, 2, Some(3)),
            200
        ));
        assert_eq!(slot.snapshot().unwrap(), ended);
    }

    #[test]
    fn resolution_carries_real_work_without_accepting_stale_parser_reports() {
        let mut slot = CodeIndexBuildAttemptSlotStateV1::default();
        let attempt = slot.begin(4, 7);
        assert!(slot.observe(
            attempt,
            work(CodeIndexPresealWorkPhaseV1::Capture, 2, Some(2)),
            100,
        ));
        assert!(slot.observe(
            attempt,
            work(CodeIndexPresealWorkPhaseV1::ParseEvidence, 2, Some(2)),
            200,
        ));
        let parsed = slot.snapshot().unwrap();
        assert!(slot.observe(
            attempt,
            work(CodeIndexPresealWorkPhaseV1::Resolution, 1, None),
            300,
        ));
        let resolved = slot.snapshot().unwrap();
        assert_eq!(resolved.phase, CodeIndexPresealWorkPhaseV1::Resolution);
        assert_eq!(resolved.completed_items, 1);
        assert_eq!(resolved.total_items, None);
        assert!(resolved.progress_epoch > parsed.progress_epoch);
        assert_eq!(resolved.last_progress_micros, Some(300));
        assert!(!slot.observe(
            attempt,
            work(CodeIndexPresealWorkPhaseV1::ParseEvidence, 3, Some(3)),
            900,
        ));
        assert!(!slot.observe(
            attempt,
            work(CodeIndexPresealWorkPhaseV1::Resolution, 1, None),
            901,
        ));
        assert_eq!(slot.snapshot().unwrap(), resolved);
        assert!(slot.observe(
            attempt,
            work(CodeIndexPresealWorkPhaseV1::Resolution, 2, None),
            400,
        ));
        assert_eq!(slot.snapshot().unwrap().last_progress_micros, Some(400));
        slot.finish(attempt);
        assert!(!slot.observe(
            attempt,
            work(CodeIndexPresealWorkPhaseV1::Resolution, 3, None),
            999,
        ));
    }

    #[test]
    fn retry_and_remount_fence_old_producers_even_when_counters_match() {
        let mut slot = CodeIndexBuildAttemptSlotStateV1::default();
        let first = slot.begin(1, 11);
        assert!(slot.observe(
            first,
            work(CodeIndexPresealWorkPhaseV1::Capture, 3, None),
            100
        ));
        let second = slot.begin(1, 11);
        assert_ne!(first.sequence, second.sequence);
        assert!(!slot.observe(
            first,
            work(CodeIndexPresealWorkPhaseV1::Capture, 40, None),
            800
        ));
        slot.finish(first);
        assert!(slot.observe(
            second,
            work(CodeIndexPresealWorkPhaseV1::Capture, 1, Some(2)),
            200
        ));
        let third = slot.begin(2, 1);
        assert!(!slot.observe(
            second,
            work(CodeIndexPresealWorkPhaseV1::ParseEvidence, 2, Some(2)),
            700
        ));
        assert!(slot.observe(
            third,
            work(CodeIndexPresealWorkPhaseV1::Capture, 1, Some(3)),
            400
        ));
        let visible = slot.snapshot().unwrap();
        assert_eq!(visible.daemon_incarnation, 2);
        assert_eq!(visible.producer_incarnation, 1);
        assert_eq!(visible.completed_items, 1);
        assert_eq!(visible.last_progress_micros, Some(400));
    }
}
