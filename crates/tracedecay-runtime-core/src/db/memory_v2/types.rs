use serde::{Deserialize, Serialize};
use tracedecay_domain::{FactEventId, PayloadAccessState};

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

/// The lineage state the live purge path compares against: the current payload
/// access and the CAS identity of the last recorded event.
pub(super) struct CurrentFactState {
    pub(super) access: PayloadAccessState,
    pub(super) last_event_id: FactEventId,
}
