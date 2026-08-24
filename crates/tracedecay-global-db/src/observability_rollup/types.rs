use serde::Serialize;
use tracedecay_domain::CoverageStateV1;

pub(super) const OBSERVABILITY_ROLLUP_RETENTION_DAYS_V1: i64 = 395;
pub(super) const SECONDS_PER_DAY: i64 = 86_400;
pub(super) const MAX_IDENTIFIER_BYTES: usize = 512;
pub(super) const MAX_FRAGMENT_JSON_BYTES: usize = 4 * 1_048_576;
pub(super) const MAX_FRAGMENT_QUERY_BYTES: usize = 32 * 1_048_576;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ObservabilityRollupRebuildV1 {
    pub authorized_scope_ref: String,
    pub day_start_seconds: i64,
    pub projector_revision: String,
    pub source_watermark: i64,
    /// Coverage of the complete source day. Non-known generations are
    /// terminal typed evidence at this watermark and contain no cells.
    pub coverage: CoverageStateV1,
    pub idempotency_key: String,
    /// Present only for a dirty-day rebuild. Publication verifies this lease
    /// and its exact adopted source watermark in the same transaction that
    /// replaces the generation and clears the dirty marker.
    pub dirty_claim: Option<ObservabilityRollupDirtyDayClaimV1>,
    /// Present only when closing a proved quiet completed UTC day. The event
    /// trigger revokes this lease if a topology source arrives before commit.
    pub empty_day_claim: Option<ObservabilityRollupEmptyDayClaimV1>,
    /// Mergeable projector-owned sufficient statistics and cross-day carry
    /// state. It is an internal backend artifact, never a wire/share cell.
    pub fragment_json: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ObservabilityRollupDirtyDayClaimV1 {
    pub authorized_scope_ref: String,
    pub day_start_seconds: i64,
    pub source_watermark: i64,
    pub claimant_id: String,
    pub lease_until_seconds: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ObservabilityRollupEmptyDayClaimV1 {
    pub authorized_scope_ref: String,
    pub day_start_seconds: i64,
    pub claimant_id: String,
    pub lease_until_seconds: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObservabilityRollupEmptyDayClaimOutcomeV1 {
    Claimed(ObservabilityRollupEmptyDayClaimV1),
    AdvancedExisting {
        day_start_seconds: i64,
    },
    DirtyDay {
        day_start_seconds: i64,
    },
    Leased {
        day_start_seconds: i64,
    },
    NotReady {
        coverage_start_day_seconds: i64,
        next_day_start_seconds: i64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObservabilityRollupFrontierV1 {
    pub coverage_start_day_seconds: i64,
    pub next_day_start_seconds: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservabilityRollupCompactionCandidateV1 {
    pub authorized_scope_ref: String,
    pub day_start_seconds: i64,
    pub generation: u64,
    pub projector_revision: String,
    pub source_watermark: i64,
    pub content_digest: String,
    pub fragment_json: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservabilityRollupCompactionV1 {
    pub candidate: ObservabilityRollupCompactionCandidateV1,
    pub fragment_json: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObservabilityRollupCompactionReceiptV1 {
    pub day_start_seconds: i64,
    pub previous_generation: u64,
    pub generation: u64,
    pub changed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservabilityRollupRebuildReceiptV1 {
    pub authorized_scope_ref: String,
    pub day_start_seconds: i64,
    pub generation: u64,
    pub projector_revision: String,
    pub source_watermark: i64,
    pub coverage: CoverageStateV1,
    pub content_digest: String,
    pub late_correction: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservabilityRollupFragmentQueryV1 {
    pub authorized_scope_ref: String,
    pub since_day_start_seconds: i64,
    pub until_day_start_seconds: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservabilityRollupFragmentRecordV1 {
    pub authorized_scope_ref: String,
    pub day_start_seconds: i64,
    pub generation: u64,
    pub projector_revision: String,
    pub source_watermark: i64,
    pub content_digest: String,
    pub fragment_json: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservabilityRollupFragmentPageV1 {
    pub fragments: Vec<ObservabilityRollupFragmentRecordV1>,
    pub coverage: CoverageStateV1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObservabilityRollupRetentionReceiptV1 {
    pub expired_generations: u64,
    pub expired_journal_entries: u64,
    pub expired_dirty_days: u64,
}

#[derive(Clone)]
pub(super) struct PublishedGeneration {
    pub(super) generation: u64,
    pub(super) projector_revision: String,
    pub(super) source_watermark: i64,
    pub(super) coverage: CoverageStateV1,
    pub(super) content_digest: String,
}

pub(super) fn validate_identifier(field: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > MAX_IDENTIFIER_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(format!("invalid observability rollup {field}"));
    }
    Ok(())
}

pub(super) fn validate_day(day_start_seconds: i64) -> Result<(), String> {
    if day_start_seconds < 0 || day_start_seconds % SECONDS_PER_DAY != 0 {
        return Err("observability rollup day must be a UTC day boundary".to_owned());
    }
    Ok(())
}

pub(super) fn merge_coverage(left: CoverageStateV1, right: CoverageStateV1) -> CoverageStateV1 {
    const fn rank(state: CoverageStateV1) -> u8 {
        match state {
            CoverageStateV1::Known => 0,
            CoverageStateV1::Capped => 1,
            CoverageStateV1::Sampled => 2,
            CoverageStateV1::Partial => 3,
            CoverageStateV1::Stale => 4,
            CoverageStateV1::Unknown => 5,
        }
    }
    if rank(left) >= rank(right) {
        left
    } else {
        right
    }
}
