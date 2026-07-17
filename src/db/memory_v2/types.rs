use serde::Serialize;
use tracedecay_domain::{
    FactAssertionId, FactAssertionKindV1, FactEventId, FactId, FactOwnerV1, PayloadAccessState,
    PayloadReferenceV1, ProvenanceId, SourceStoreId, UtcMicros,
};

use crate::errors::Result;

use super::{
    OPERATION, db_message, validate_frontiers, validate_scope, validate_v1_compatibility_source,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct CapturedMemoryV2Frontiers {
    pub(crate) feedback: i64,
    pub(crate) oplog: i64,
    pub(crate) facts: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MemoryV2BackfillBatchOutcome {
    Advanced { processed: usize },
    AwaitingCutover,
}

/// A V22 repair snapshot for legacy feedback rows that had already been
/// imported before history/map projections existed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MemoryV2FeedbackHistoryRepairProgress {
    pub(crate) feedback_frontier: i64,
    pub(crate) feedback_cursor: i64,
    pub(crate) complete: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MemoryV2FeedbackHistoryRepairBatchOutcome {
    Advanced { processed: usize },
    Complete { processed: usize },
    NotRequired,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct MemoryV2CutoverReceipt {
    receipt_id: ProvenanceId,
    pub(super) owner: FactOwnerV1,
    pub(super) source_store_id: SourceStoreId,
    pub(super) frontiers: CapturedMemoryV2Frontiers,
    pub(super) dual_write_activated_at: UtcMicros,
}

impl MemoryV2CutoverReceipt {
    pub(crate) fn new(
        receipt_id: ProvenanceId,
        owner: FactOwnerV1,
        source_store_id: SourceStoreId,
        frontiers: CapturedMemoryV2Frontiers,
        dual_write_activated_at: UtcMicros,
    ) -> Result<Self> {
        receipt_id
            .validate()
            .map_err(|_| db_message(OPERATION, "cutover receipt identity is invalid"))?;
        validate_scope(&owner, &source_store_id)?;
        validate_v1_compatibility_source(&source_store_id)?;
        validate_frontiers(frontiers)?;
        Ok(Self {
            receipt_id,
            owner,
            source_store_id,
            frontiers,
            dual_write_activated_at,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MemoryV2CutoverOutcome {
    TailPending(CapturedMemoryV2Frontiers),
    Complete,
}

#[derive(Clone)]
pub(super) struct OwnerKey {
    pub(super) kind: &'static str,
    pub(super) project_id: String,
    pub(super) json: String,
}

pub(super) struct Progress {
    pub(super) phase: String,
    pub(super) feedback_frontier: i64,
    pub(super) oplog_frontier: i64,
    pub(super) fact_frontier: i64,
    pub(super) feedback_cursor: i64,
    pub(super) oplog_cursor: i64,
    pub(super) fact_cursor: i64,
    pub(super) started_at: i64,
}

pub(super) struct FeedbackHistoryRepairProgress {
    pub(super) feedback_frontier: i64,
    pub(super) feedback_cursor: i64,
    pub(super) phase: String,
    pub(super) started_at: i64,
}

pub(super) struct CurrentFactState {
    pub(super) access: PayloadAccessState,
    pub(super) last_event_id: FactEventId,
    pub(super) active_assertion_id: Option<FactAssertionId>,
    pub(super) active_kind: Option<FactAssertionKindV1>,
    pub(super) active_payload_reference: Option<PayloadReferenceV1>,
}

pub(super) struct LegacyFeedback {
    pub(super) event_id: i64,
    pub(super) fact_id: i64,
    pub(super) action: String,
    pub(super) old_trust: f64,
    pub(super) new_trust: f64,
    pub(super) created_at: i64,
    pub(super) source: Option<String>,
    pub(super) note: Option<String>,
}

pub(super) struct LegacyOplog {
    pub(super) id: i64,
    pub(super) ts: i64,
    pub(super) op: String,
    pub(super) fact_id: Option<i64>,
}

pub(super) struct LegacyFact {
    pub(super) fact_id: i64,
    pub(super) content: String,
    pub(super) category: String,
    pub(super) tags_json: String,
    pub(super) trust_score: f64,
    pub(super) source: String,
    pub(super) metadata_json: String,
    pub(super) updated_at: i64,
    pub(super) telemetry: LegacyFactTelemetry,
}

/// Usage counters carried from `memory_facts` into the canonical projection.
/// Unlike feedback, retrieval history has no legacy event log to replay, so
/// the cutover must preserve these counters or every migrated store silently
/// loses its ranking usage signal.
pub(super) struct LegacyFactTelemetry {
    pub(super) retrieval_count: i64,
    pub(super) access_count: i64,
    pub(super) helpful_count: i64,
    pub(super) unhelpful_count: i64,
}

#[derive(Serialize)]
pub(super) struct StoredAssertionHeaderV1<'a> {
    pub(super) assertion_id: &'a FactAssertionId,
    pub(super) fact_id: &'a FactId,
    pub(super) owner: &'a FactOwnerV1,
    pub(super) kind: &'a FactAssertionKindV1,
    pub(super) payload_reference: &'a PayloadReferenceV1,
    pub(super) evidence: &'a [tracedecay_domain::FactEvidenceRefV1],
    pub(super) asserted_at: UtcMicros,
    pub(super) actor_id: Option<&'a tracedecay_domain::ActorId>,
}
