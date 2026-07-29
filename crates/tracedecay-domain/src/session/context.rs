//! Compact context hydration, omission, conflict, and lineage contracts.

use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};

use crate::research::{RetrievalAnchorId, UtcMicros};

use super::coverage::TemporalCoverageCountsV1;
use super::occurrence::{
    RetrievalGrainV1, SessionAuthorityClassV1, SessionContractError, TemporalAssertionKindV1,
};

/// Current hydration eligibility after authorization and retention rechecks.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum HydrationStateV1 {
    Available,
    RetainedButUnavailable,
    Redacted,
    Deleted,
    RetentionExpired,
    Unauthorized,
    Locked,
    UnverifiableLegacy,
}

impl HydrationStateV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::RetainedButUnavailable => "retained_but_unavailable",
            Self::Redacted => "redacted",
            Self::Deleted => "deleted",
            Self::RetentionExpired => "retention_expired",
            Self::Unauthorized => "unauthorized",
            Self::Locked => "locked",
            Self::UnverifiableLegacy => "unverifiable_legacy",
        }
    }
}

/// Why an otherwise relevant item was omitted from compact context.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ContextOmissionReasonV1 {
    ByteBudget,
    TokenBudget,
    Unauthorized,
    Redacted,
    Deleted,
    RetentionExpired,
    Locked,
    Unavailable,
    SummaryHorizonMismatch,
    DuplicateRepresentative,
}

impl ContextOmissionReasonV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ByteBudget => "byte_budget",
            Self::TokenBudget => "token_budget",
            Self::Unauthorized => "unauthorized",
            Self::Redacted => "redacted",
            Self::Deleted => "deleted",
            Self::RetentionExpired => "retention_expired",
            Self::Locked => "locked",
            Self::Unavailable => "unavailable",
            Self::SummaryHorizonMismatch => "summary_horizon_mismatch",
            Self::DuplicateRepresentative => "duplicate_representative",
        }
    }
}

/// One selected context item. Payload text remains behind the exact anchor.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompactContextRecordV1 {
    pub anchor_id: RetrievalAnchorId,
    pub grain: RetrievalGrainV1,
    pub hydration: HydrationStateV1,
    pub encoded_bytes: u64,
}

impl CompactContextRecordV1 {
    pub fn validate(&self) -> Result<(), SessionContractError> {
        self.anchor_id
            .validate()
            .map_err(|_| SessionContractError::InvalidIdentity {
                field: "compact context record anchor",
            })
    }
}

/// One explicit compact-context omission.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompactContextOmissionV1 {
    pub anchor_id: Option<RetrievalAnchorId>,
    pub reason: ContextOmissionReasonV1,
}

impl CompactContextOmissionV1 {
    pub fn validate(&self) -> Result<(), SessionContractError> {
        if let Some(anchor_id) = &self.anchor_id {
            anchor_id
                .validate()
                .map_err(|_| SessionContractError::InvalidIdentity {
                    field: "compact context omission anchor",
                })?;
        }
        Ok(())
    }
}

/// One conflict retained in compact context instead of silently selecting a side.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompactContextConflictV1 {
    pub anchor_id: RetrievalAnchorId,
    pub supporting_anchor_ids: BTreeSet<RetrievalAnchorId>,
}

impl CompactContextConflictV1 {
    pub fn validate(&self) -> Result<(), SessionContractError> {
        self.anchor_id
            .validate()
            .map_err(|_| SessionContractError::InvalidIdentity {
                field: "compact context conflict anchor",
            })?;
        for anchor_id in &self.supporting_anchor_ids {
            anchor_id
                .validate()
                .map_err(|_| SessionContractError::InvalidIdentity {
                    field: "compact context conflict supporting anchor",
                })?;
        }
        Ok(())
    }
}

/// One typed temporal edge needed to interpret compact-context evolution.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompactContextLineageEdgeV1 {
    pub kind: TemporalAssertionKindV1,
    pub subject_anchor_id: RetrievalAnchorId,
    pub object_anchor_id: RetrievalAnchorId,
    pub knowledge_at: UtcMicros,
    pub authority: SessionAuthorityClassV1,
    pub authorized: bool,
    pub supporting_anchor_ids: BTreeSet<RetrievalAnchorId>,
}

impl CompactContextLineageEdgeV1 {
    pub fn validate(&self) -> Result<(), SessionContractError> {
        if self.subject_anchor_id == self.object_anchor_id {
            return Err(SessionContractError::AssertionSelfReference);
        }
        for (field, anchor_id) in [
            ("compact context lineage subject", &self.subject_anchor_id),
            ("compact context lineage object", &self.object_anchor_id),
        ] {
            anchor_id
                .validate()
                .map_err(|_| SessionContractError::InvalidIdentity { field })?;
        }
        for anchor_id in &self.supporting_anchor_ids {
            anchor_id
                .validate()
                .map_err(|_| SessionContractError::InvalidIdentity {
                    field: "compact context lineage supporting anchor",
                })?;
        }
        Ok(())
    }
}

/// Anchor-only compact-context assembly result.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompactContextBundleV1 {
    pub records: Vec<CompactContextRecordV1>,
    pub omissions: Vec<CompactContextOmissionV1>,
    pub continuation_anchors: Vec<RetrievalAnchorId>,
    pub coverage: TemporalCoverageCountsV1,
    pub conflicts: Vec<CompactContextConflictV1>,
    pub lineage: Vec<CompactContextLineageEdgeV1>,
    pub encoded_bytes: u64,
}

impl CompactContextBundleV1 {
    pub fn validate(&self) -> Result<(), SessionContractError> {
        let mut anchors = BTreeSet::new();
        let mut encoded_bytes = 0_u64;
        for record in &self.records {
            record.validate()?;
            if !anchors.insert(record.anchor_id.clone()) {
                return Err(SessionContractError::DuplicateContextAnchor);
            }
            encoded_bytes = encoded_bytes
                .checked_add(record.encoded_bytes)
                .ok_or(SessionContractError::CompactContextEncodedBytesOverflow)?;
        }
        for anchor in &self.continuation_anchors {
            anchor
                .validate()
                .map_err(|_| SessionContractError::InvalidIdentity {
                    field: "compact context continuation anchor",
                })?;
            if !anchors.insert(anchor.clone()) {
                return Err(SessionContractError::DuplicateContextAnchor);
            }
        }
        for omission in &self.omissions {
            omission.validate()?;
            if let Some(anchor_id) = &omission.anchor_id
                && !anchors.insert(anchor_id.clone())
            {
                return Err(SessionContractError::DuplicateContextAnchor);
            }
        }
        for conflict in &self.conflicts {
            conflict.validate()?;
        }
        for edge in &self.lineage {
            edge.validate()?;
        }
        if self.encoded_bytes != encoded_bytes {
            return Err(SessionContractError::CompactContextEncodedBytesMismatch);
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for CompactContextBundleV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            records: Vec<CompactContextRecordV1>,
            omissions: Vec<CompactContextOmissionV1>,
            continuation_anchors: Vec<RetrievalAnchorId>,
            #[serde(default)]
            coverage: TemporalCoverageCountsV1,
            #[serde(default)]
            conflicts: Vec<CompactContextConflictV1>,
            #[serde(default)]
            lineage: Vec<CompactContextLineageEdgeV1>,
            encoded_bytes: u64,
        }

        let wire = Wire::deserialize(deserializer)?;
        let bundle = Self {
            records: wire.records,
            omissions: wire.omissions,
            continuation_anchors: wire.continuation_anchors,
            coverage: wire.coverage,
            conflicts: wire.conflicts,
            lineage: wire.lineage,
            encoded_bytes: wire.encoded_bytes,
        };
        bundle.validate().map_err(serde::de::Error::custom)?;
        Ok(bundle)
    }
}
