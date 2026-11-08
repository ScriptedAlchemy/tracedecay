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

    pub(crate) fn snapshot(&self) -> Option<CodeIndexPresealAttemptProgressV1> {
        self.snapshot.clone()
    }
}

/// A stale or unwinding reconcile can never publish into its successor.
/// Dropping this guard on every cancellation, retry, panic or return fences it.
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
            .finish(self.identity);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
