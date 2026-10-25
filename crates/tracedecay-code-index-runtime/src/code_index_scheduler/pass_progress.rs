//! Progress ownership before a reconcile pass seals its first generation.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use tracedecay_code_index::production::CodeIndexBuildWorkV1;
use tracedecay_contracts::{
    code_index_freshness::{CodeIndexBuildPhaseV1, CodeIndexBuildProgressV1},
    now_micros,
};

use super::CodeIndexBuildProgressSlotV1;

pub(super) struct UnsealedPassProgressV1 {
    slot: CodeIndexBuildProgressSlotV1,
    owner_epoch: u64,
    daemon_incarnation: u64,
    producer_incarnation: u64,
    started_at: Instant,
    last: Mutex<Option<PassWorkBoundaryV1>>,
}

#[derive(Clone, Copy)]
struct PassWorkBoundaryV1 {
    phase: CodeIndexBuildPhaseV1,
    completed: Option<u64>,
    total: Option<u64>,
}

impl UnsealedPassProgressV1 {
    /// `None` when a generation already owns the slot.
    pub(super) fn claim(
        slot: &CodeIndexBuildProgressSlotV1,
        daemon_incarnation: u64,
        producer_incarnation: u64,
    ) -> Option<UnsealedPassClaimV1> {
        let (owner_epoch, found) = slot
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .claim_unsealed_pass()?;
        Some(UnsealedPassClaimV1 {
            progress: Arc::new(Self {
                slot: Arc::clone(slot),
                owner_epoch,
                daemon_incarnation,
                producer_incarnation,
                started_at: Instant::now(),
                last: Mutex::new(None),
            }),
            found,
            sealed: false,
        })
    }

    /// A retried attempt starts its phases over.
    pub(super) fn begin_attempt(&self) {
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    pub(super) fn observe(&self, work: CodeIndexBuildWorkV1) {
        let (phase, completed, total) = match work {
            CodeIndexBuildWorkV1::Captured { completed, total } => {
                (CodeIndexBuildPhaseV1::Capture, Some(completed), Some(total))
            }
            CodeIndexBuildWorkV1::Parsed { completed, total } => {
                (CodeIndexBuildPhaseV1::Parse, Some(completed), Some(total))
            }
            CodeIndexBuildWorkV1::Linking => (CodeIndexBuildPhaseV1::Link, None, None),
            CodeIndexBuildWorkV1::Linked { completed, total } => {
                (CodeIndexBuildPhaseV1::Link, Some(completed), Some(total))
            }
            CodeIndexBuildWorkV1::Sealed { segments_written } => {
                (CodeIndexBuildPhaseV1::Link, Some(segments_written), None)
            }
        };
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        // Parallel workers finish out of order; a smaller count than the one
        // already published is not new work.
        if let Some(previous) = *last
            && previous.phase == phase
            && previous.total == total
            && previous.completed >= completed
            && completed.is_some()
        {
            return;
        }
        *last = Some(PassWorkBoundaryV1 {
            phase,
            completed,
            total,
        });
        let snapshot = CodeIndexBuildProgressV1 {
            generation_id: None,
            daemon_incarnation: self.daemon_incarnation,
            producer_incarnation: self.producer_incarnation,
            progress_epoch: 0,
            sealed_source_digest: None,
            phase,
            phase_completed_units: completed,
            phase_total_units: total,
            committed_pages: 0,
            committed_chunks: 0,
            committed_imports: 0,
            committed_payload_bytes: 0,
            completed_files: 0,
            total_files: 0,
            completed_lexical_units: 0,
            total_lexical_units: 0,
            current_batch_pages: 0,
            current_batch_payload_bytes: 0,
            elapsed_micros: u64::try_from(self.started_at.elapsed().as_micros())
                .unwrap_or(u64::MAX),
            last_commit_latency_micros: None,
            files_per_second: None,
            lexical_units_per_second: None,
            estimated_remaining_seconds: None,
            last_progress_micros: now_micros().0,
            blocked_reason: None,
        };
        // Publish under `last` so two workers cannot reorder their counts.
        let _ = self
            .slot
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .publish_unsealed_pass(self.owner_epoch, snapshot);
    }
}

/// A pass's claim on the progress slot. Dropping it restores the snapshot
/// the pass found unless the pass sealed a generation, whose text owner
/// replaces the snapshot when it binds.
pub(super) struct UnsealedPassClaimV1 {
    progress: Arc<UnsealedPassProgressV1>,
    found: Option<Arc<CodeIndexBuildProgressV1>>,
    sealed: bool,
}

impl UnsealedPassClaimV1 {
    pub(super) fn progress(&self) -> &Arc<UnsealedPassProgressV1> {
        &self.progress
    }

    pub(super) fn sealed(mut self) {
        self.sealed = true;
    }
}

impl Drop for UnsealedPassClaimV1 {
    fn drop(&mut self) {
        if !self.sealed {
            self.progress
                .slot
                .write()
                .unwrap_or_else(PoisonError::into_inner)
                .release_unsealed_pass(self.progress.owner_epoch, self.found.take());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, RwLock};

    use tracedecay_code_index::production::CodeIndexBuildWorkV1;
    use tracedecay_contracts::code_index_freshness::CodeIndexBuildPhaseV1;
    use tracedecay_domain::CodeGenerationId;

    use super::UnsealedPassProgressV1;
    use crate::code_index_scheduler::CodeIndexBuildProgressSlotStateV1;

    #[test]
    fn aborted_pass_removes_its_progress_and_rejects_late_workers() {
        let slot = Arc::new(RwLock::new(CodeIndexBuildProgressSlotStateV1::default()));
        let claim = UnsealedPassProgressV1::claim(&slot, 1, 1).expect("first pass claim");
        let progress = Arc::clone(claim.progress());
        progress.observe(CodeIndexBuildWorkV1::Captured {
            completed: 2,
            total: 3,
        });
        let newer = slot.read().expect("slot").snapshot().expect("progress");
        progress.observe(CodeIndexBuildWorkV1::Captured {
            completed: 1,
            total: 3,
        });
        let current = slot.read().expect("slot").snapshot().expect("progress");
        assert_eq!(current.progress_epoch, newer.progress_epoch);
        assert_eq!(current.phase_completed_units, Some(2));

        drop(claim);
        assert!(slot.read().expect("slot").snapshot().is_none());
        progress.observe(CodeIndexBuildWorkV1::Captured {
            completed: 3,
            total: 3,
        });
        assert!(slot.read().expect("slot").snapshot().is_none());
        let next = UnsealedPassProgressV1::claim(&slot, 1, 1).expect("replacement claim");
        next.progress().observe(CodeIndexBuildWorkV1::Parsed {
            completed: 1,
            total: 3,
        });
        let replacement = slot.read().expect("slot").snapshot().expect("replacement");
        progress.begin_attempt();
        progress.observe(CodeIndexBuildWorkV1::Captured {
            completed: 1,
            total: 3,
        });
        let current = slot.read().expect("slot").snapshot().expect("replacement");
        assert_eq!(current.phase, CodeIndexBuildPhaseV1::Parse);
        assert_eq!(current.progress_epoch, replacement.progress_epoch);
    }

    #[test]
    fn generation_ownership_rejects_an_unsealed_pass() {
        let slot = Arc::new(RwLock::new(CodeIndexBuildProgressSlotStateV1::default()));
        let claim = UnsealedPassProgressV1::claim(&slot, 1, 1).expect("first pass claim");
        slot.write()
            .expect("slot")
            .replace_generation(CodeGenerationId::new("generation.ready").expect("generation"));
        claim.progress().observe(CodeIndexBuildWorkV1::Parsed {
            completed: 1,
            total: 1,
        });
        assert!(slot.read().expect("slot").snapshot().is_none());
        assert!(UnsealedPassProgressV1::claim(&slot, 1, 1).is_none());
        drop(claim);
        assert!(slot.read().expect("slot").snapshot().is_none());
    }
}
