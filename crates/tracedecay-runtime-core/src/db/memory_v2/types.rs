use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    FactAssertionId, FactAssertionKindV1, FactEventId, FactId, FactOwnerV1, PayloadAccessState,
    PayloadReferenceV1, UtcMicros,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct CapturedMemoryV2Frontiers {
    pub feedback: i64,
    pub oplog: i64,
    pub facts: i64,
}

/// Deserialization exists so a receipt that already carries a verified
/// coverage witness can be read back instead of recomputed: the counts are
/// frozen once the cutover completes, and the join that produces them is not
/// cheap enough to re-run on every daemon tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MemoryV2CutoverCoverage {
    pub(super) source_fact_count: i64,
    pub(super) represented_fact_count: i64,
    pub(super) source_feedback_count: i64,
    pub(super) represented_feedback_count: i64,
    pub(super) source_oplog_count: i64,
    pub(super) represented_oplog_count: i64,
}

impl MemoryV2CutoverCoverage {
    pub(super) fn is_complete(self) -> bool {
        self.source_fact_count == self.represented_fact_count
            && self.source_feedback_count == self.represented_feedback_count
            && self.source_oplog_count == self.represented_oplog_count
    }
}

#[derive(Clone)]
pub(super) struct OwnerKey {
    pub(super) kind: &'static str,
    pub(super) project_id: String,
    pub(super) json: String,
}

#[allow(dead_code)]
pub(super) struct CurrentFactState {
    pub(super) access: PayloadAccessState,
    pub(super) last_event_id: FactEventId,
    pub(super) active_assertion_id: Option<FactAssertionId>,
    pub(super) active_kind: Option<FactAssertionKindV1>,
    pub(super) active_payload_reference: Option<PayloadReferenceV1>,
}

/// Usage counters carried from `memory_facts` into the canonical projection.
/// Unlike feedback, retrieval history has no legacy event log to replay, so
/// the cutover must preserve these counters or every migrated store silently
/// loses its ranking usage signal.
#[derive(Serialize)]
#[allow(dead_code)]
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
