//! Real native graph writer observations for one mounted worktree.
//! A callback is bound to one sealed source and one publication attempt.
//! No poll, phase projection, or replayed receipt can refresh its work clock.

use std::sync::{Arc, PoisonError, RwLock};
use tracedecay_contracts::code_index_freshness::{
    CodeIndexNativeGraphPhaseV1, CodeIndexNativeGraphProgressV1,
};
use tracedecay_store::{
    GraphNativeWorkKindV1, GraphNativeWorkObserverV1, GraphNativeWorkV1,
};

pub(super) type CodeIndexNativeGraphProgressSlotV1 =
    Arc<RwLock<CodeIndexNativeGraphProgressSlotStateV1>>;

#[derive(Clone, Copy)]
struct GraphAttemptOwnerV1 {
    daemon: u64,
    producer: u64,
    sequence: u64,
}

#[derive(Default)]
pub(super) struct CodeIndexNativeGraphProgressSlotStateV1 {
    sequence: u64,
    epoch: u64,
    snapshot: Option<CodeIndexNativeGraphProgressV1>,
    last_stage_ordinal: Option<u64>,
}

impl CodeIndexNativeGraphProgressSlotStateV1 {
    fn begin(
        &mut self,
        daemon: u64,
        producer: u64,
        generation_id: String,
        sealed_graph_digest: String,
    ) -> GraphAttemptOwnerV1 {
        self.sequence = self.sequence.saturating_add(1).max(1);
        self.epoch = self.epoch.saturating_add(1).max(1);
        self.last_stage_ordinal = None;
        let owner = GraphAttemptOwnerV1 {
            daemon,
            producer,
            sequence: self.sequence,
        };
        self.snapshot = Some(CodeIndexNativeGraphProgressV1 {
            generation_id,
            sealed_graph_digest,
            daemon_incarnation: daemon,
            producer_incarnation: producer,
            attempt_sequence: self.sequence,
            progress_epoch: self.epoch,
            phase: None,
            encoded_entities: 0,
            total_entities: None,
            encoded_relations: 0,
            total_relations: None,
            committed_stage_pages: 0,
            committed_stage_mutations: 0,
            committed_stage_bytes: 0,
            committed_compact_segments: 0,
            committed_compact_bytes: 0,
            last_progress_micros: None,
            active: true,
        });
        owner
    }

    fn owns(&self, owner: GraphAttemptOwnerV1) -> bool {
        self.snapshot.as_ref().is_some_and(|state| {
            state.active
                && state.daemon_incarnation == owner.daemon
                && state.producer_incarnation == owner.producer
                && state.attempt_sequence == owner.sequence
        })
    }

    fn observe(&mut self, owner: GraphAttemptOwnerV1, work: GraphNativeWorkV1, now: i64) -> bool {
        if !self.owns(owner) {
            return false;
        }
        let Some(current) = self.snapshot.as_ref() else {
            return false;
        };
        let mut next = current.clone();
        match work {
            GraphNativeWorkV1::CompactRowsEncoded {
                kind,
                completed_rows,
                total_rows,
                total_relations,
            } => {
                if completed_rows == 0 || completed_rows > total_rows
                    || next.committed_compact_segments != 0
                {
                    return false;
                }
                // The manifest already authenticates the relation count,
                // including zero, while entity windows are still being encoded.
                // Never treat an unknown relation bound as completed work.
                if next.total_relations.is_some_and(|known| known != total_relations)
                    || (kind == GraphNativeWorkKindV1::Relations
                        && total_rows != total_relations)
                {
                    return false;
                }
                let (completed, total) = match kind {
                    GraphNativeWorkKindV1::Entities => {
                        if next.encoded_relations != 0 {
                            return false;
                        }
                        (&mut next.encoded_entities, &mut next.total_entities)
                    }
                    GraphNativeWorkKindV1::Relations => {
                        (&mut next.encoded_relations, &mut next.total_relations)
                    }
                };
                if completed_rows <= *completed
                    || total.is_some_and(|prior| prior != total_rows)
                {
                    return false;
                }
                *completed = completed_rows;
                *total = Some(total_rows);
                next.total_relations = Some(total_relations);
                next.phase = Some(CodeIndexNativeGraphPhaseV1::CompactEncoding);
            }
            GraphNativeWorkV1::StagePageApplied {
                ordinal,
                mutations,
                live_bytes,
                ..
            } => {
                if mutations == 0
                    || self.last_stage_ordinal.is_some_and(|last| ordinal <= last)
                {
                    return false;
                }
                self.last_stage_ordinal = Some(ordinal);
                next.committed_stage_pages = next.committed_stage_pages.saturating_add(1);
                next.committed_stage_mutations =
                    next.committed_stage_mutations.saturating_add(mutations);
                next.committed_stage_bytes =
                    next.committed_stage_bytes.saturating_add(live_bytes);
                next.phase = Some(CodeIndexNativeGraphPhaseV1::StagePageCommit);
            }
            GraphNativeWorkV1::CompactSegmentVerified {
                canonical_bytes,
                ..
            } => {
                if canonical_bytes == 0 || next.committed_compact_segments != 0 {
                    return false;
                }
                next.committed_compact_segments = 1;
                next.committed_compact_bytes = canonical_bytes;
                next.phase = Some(CodeIndexNativeGraphPhaseV1::CompactSegmentVerified);
            }
        }
        self.epoch = self.epoch.saturating_add(1).max(1);
        next.progress_epoch = self.epoch;
        next.last_progress_micros = Some(
            current.last_progress_micros.map_or(now, |prior| prior.max(now)),
        );
        self.snapshot = Some(next);
        true
    }

    fn finish(&mut self, owner: GraphAttemptOwnerV1) {
        if !self.owns(owner) {
            return;
        }
        if let Some(current) = self.snapshot.as_mut() {
            current.active = false;
            self.epoch = self.epoch.saturating_add(1).max(1);
            current.progress_epoch = self.epoch;
            // Completion of an attempt is not proof of any additional work.
        }
    }

    pub(super) fn snapshot(&self) -> Option<CodeIndexNativeGraphProgressV1> {
        self.snapshot.clone()
    }
}

/// Always retires the exact attempt when the publication ends or is cancelled.
pub(super) struct CodeIndexNativeGraphProgressGuardV1 {
    slot: CodeIndexNativeGraphProgressSlotV1,
    owner: GraphAttemptOwnerV1,
}

impl CodeIndexNativeGraphProgressGuardV1 {
    pub(super) fn begin(
        slot: CodeIndexNativeGraphProgressSlotV1,
        daemon: u64,
        producer: u64,
        generation_id: String,
        sealed_graph_digest: String,
    ) -> Self {
        let owner = slot.write().unwrap_or_else(PoisonError::into_inner)
            .begin(daemon, producer, generation_id, sealed_graph_digest);
        Self { slot, owner }
    }

    pub(super) fn observer(&self) -> GraphNativeWorkObserverV1 {
        let slot = Arc::clone(&self.slot);
        let owner = self.owner;
        Arc::new(move |work| {
            slot.write().unwrap_or_else(PoisonError::into_inner)
                .observe(owner, work, tracedecay_contracts::now_micros().0);
        })
    }
}

impl Drop for CodeIndexNativeGraphProgressGuardV1 {
    fn drop(&mut self) {
        self.slot.write().unwrap_or_else(PoisonError::into_inner)
            .finish(self.owner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_native_work_distinguishes_encoding_receipted_pages_and_verified_segment() {
        let slot = Arc::new(RwLock::new(CodeIndexNativeGraphProgressSlotStateV1::default()));
        let attempt = CodeIndexNativeGraphProgressGuardV1::begin(
            Arc::clone(&slot), 4, 6, "generation.a".into(), "digest.a".into(),
        );
        let observer = attempt.observer();
        let initial = slot.read().unwrap().snapshot().unwrap();
        assert_eq!(initial.last_progress_micros, None);
        observer(GraphNativeWorkV1::CompactRowsEncoded {
            kind: GraphNativeWorkKindV1::Entities,
            completed_rows: 512,
            total_rows: 1101,
            total_relations: 1143,
        });
        let encoded = slot.read().unwrap().snapshot().unwrap();
        assert_eq!(encoded.encoded_entities, 512);
        assert_eq!(encoded.total_relations, Some(1143));
        assert_eq!(encoded.encoded_relations, 0);
        assert_eq!(encoded.committed_stage_pages, 0);
        assert_eq!(encoded.committed_compact_segments, 0);
        assert!(encoded.last_progress_micros.is_some());
        observer(GraphNativeWorkV1::CompactRowsEncoded {
            kind: GraphNativeWorkKindV1::Entities,
            completed_rows: 512,
            total_rows: 1101,
            total_relations: 1143,
        });
        assert_eq!(slot.read().unwrap().snapshot().unwrap(), encoded);
        observer(GraphNativeWorkV1::CompactRowsEncoded {
            kind: GraphNativeWorkKindV1::Relations,
            completed_rows: 143,
            total_rows: 1143,
            total_relations: 1143,
        });
        let parsed = slot.read().unwrap().snapshot().unwrap();
        assert!(parsed.progress_epoch > encoded.progress_epoch);
        observer(GraphNativeWorkV1::CompactRowsEncoded {
            kind: GraphNativeWorkKindV1::Relations,
            completed_rows: 200,
            total_rows: 1143,
            total_relations: 9000,
        });
        assert_eq!(
            slot.read().unwrap().snapshot().unwrap(),
            parsed,
            "changing the sealed relation bound cannot renew progress"
        );
        observer(GraphNativeWorkV1::StagePageApplied {
            ordinal: 0, kind: GraphNativeWorkKindV1::Entities, mutations: 36, live_bytes: 4096,
        });
        let durable = slot.read().unwrap().snapshot().unwrap();
        assert_eq!(durable.committed_stage_pages, 1);
        assert_eq!(durable.committed_stage_mutations, 36);
        assert_eq!(durable.committed_stage_bytes, 4096);
        observer(GraphNativeWorkV1::StagePageApplied {
            ordinal: 0, kind: GraphNativeWorkKindV1::Entities, mutations: 36, live_bytes: 4096,
        });
        assert_eq!(slot.read().unwrap().snapshot().unwrap(), durable);
        observer(GraphNativeWorkV1::CompactSegmentVerified {
            entities: 1101, relations: 1143, canonical_bytes: 67890,
        });
        let installed = slot.read().unwrap().snapshot().unwrap();
        assert_eq!(installed.committed_compact_segments, 1);
        assert_eq!(installed.committed_compact_bytes, 67890);
        assert!(installed.progress_epoch > durable.progress_epoch);
        drop(attempt);
        let finished = slot.read().unwrap().snapshot().unwrap();
        assert!(!finished.active);
        assert_eq!(finished.last_progress_micros, installed.last_progress_micros);
        observer(GraphNativeWorkV1::CompactSegmentVerified {
            entities: 1101, relations: 1143, canonical_bytes: 67890,
        });
        assert_eq!(slot.read().unwrap().snapshot().unwrap(), finished);
    }

    #[test]
    fn restarted_native_publication_fences_old_callbacks_and_resets_attempt_counters() {
        let slot = Arc::new(RwLock::new(CodeIndexNativeGraphProgressSlotStateV1::default()));
        let stale = CodeIndexNativeGraphProgressGuardV1::begin(
            Arc::clone(&slot), 1, 9, "generation.a".into(), "digest.a".into(),
        );
        let old_observer = stale.observer();
        old_observer(GraphNativeWorkV1::StagePageApplied {
            ordinal: 0, kind: GraphNativeWorkKindV1::Entities, mutations: 2, live_bytes: 18,
        });
        let old_epoch = slot.read().unwrap().snapshot().unwrap().progress_epoch;
        let current = CodeIndexNativeGraphProgressGuardV1::begin(
            Arc::clone(&slot), 2, 1, "generation.a".into(), "digest.a".into(),
        );
        let current_observer = current.observer();
        old_observer(GraphNativeWorkV1::StagePageApplied {
            ordinal: 1, kind: GraphNativeWorkKindV1::Entities, mutations: 100, live_bytes: 1000,
        });
        drop(stale);
        let reopened = slot.read().unwrap().snapshot().unwrap();
        assert_eq!(reopened.committed_stage_pages, 0);
        assert_eq!(reopened.daemon_incarnation, 2);
        assert!(reopened.progress_epoch > old_epoch);
        assert_eq!(reopened.last_progress_micros, None);
        current_observer(GraphNativeWorkV1::StagePageApplied {
            ordinal: 0, kind: GraphNativeWorkKindV1::Relations, mutations: 3, live_bytes: 120,
        });
        let resumed = slot.read().unwrap().snapshot().unwrap();
        assert_eq!(resumed.committed_stage_pages, 1);
        assert_eq!(resumed.committed_stage_mutations, 3);
        assert!(resumed.active);
    }
}
