mod admission;
pub(super) mod assembly;
mod estimation;
#[cfg(test)]
mod tests;
mod wire;

use thiserror::Error;
use tracedecay_domain::{
    CompactContextBundleV1, CompactContextConflictV1, CompactContextLineageEdgeV1,
    CompactContextOmissionV1, HydrationStateV1, RetrievalAnchorId, TemporalCoverageCountsV1,
};

use super::hydration::{HydratedPayload, UnavailableHydration};
use super::ports::TemporalPortError;
use super::resolution::summary::SummaryOmission;

const CANONICAL_CONTEXT_FORMAT: &str = "tracedecay.compact_context.v1";
const MAX_CONTEXT_RECORDS: usize = 64;
const MAX_CONTEXT_ANCHORS: usize = 256;
const MAX_CONTEXT_FRAME_ITEMS: usize = 256;
const MAX_CONTEXT_OUTPUT_BYTES: u64 = 1024 * 1024;

pub trait VersionedTokenEstimator {
    fn version(&self) -> &str;

    /// Streaming assembly policy. Defaults to whitespace-word counting so
    /// existing `estimate`-only implementors remain source-compatible.
    fn token_policy(&self) -> TokenPolicy {
        TokenPolicy::Whitespace
    }

    /// Compatibility shim retained for pre-`TokenPolicy` whitespace estimators.
    /// Canonical assembly streams via [`Self::token_policy`] and does not call
    /// this method.
    fn estimate(&self, text: &str) -> u64 {
        match self.token_policy() {
            TokenPolicy::Whitespace => text.split_whitespace().count() as u64,
            TokenPolicy::Characters => text.chars().count() as u64,
            TokenPolicy::Substring(pattern) => text.matches(pattern).count() as u64,
            TokenPolicy::JsonDocument => u64::from(!(text.starts_with('{') && text.ends_with('}'))),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenPolicy {
    Whitespace,
    Characters,
    Substring(&'static str),
    JsonDocument,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextBudget {
    pub max_bytes: u64,
    pub max_tokens: u64,
    pub estimator_version: String,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ContextError {
    #[error("token estimator version does not match the requested budget")]
    EstimatorVersionMismatch,
    #[error("compact context metadata exceeded the {resource} budget")]
    BudgetExceeded { resource: &'static str },
    #[error("compact context assembly was interrupted")]
    Interrupted(#[from] TemporalPortError),
    #[error("compact context bundle is invalid: {0}")]
    InvalidBundle(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactContext {
    pub rendered: String,
    pub bundle: CompactContextBundleV1,
    pub accounted_bytes: u64,
    pub estimated_tokens: u64,
    pub estimator_version: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TemporalContextFrames {
    pub coverage: TemporalCoverageCountsV1,
    pub conflicts: Vec<CompactContextConflictV1>,
    pub lineage: Vec<CompactContextLineageEdgeV1>,
    pub omissions: Vec<CompactContextOmissionV1>,
    pub summary_omissions: Vec<SummaryOmission>,
}

pub(crate) trait ContextPayload {
    fn anchor_id(&self) -> &RetrievalAnchorId;
    fn bytes(&self) -> &[u8];
}

impl ContextPayload for HydratedPayload {
    fn anchor_id(&self) -> &RetrievalAnchorId {
        self.anchor_id()
    }

    fn bytes(&self) -> &[u8] {
        self.bytes()
    }
}

pub(crate) trait ContextUnavailable {
    fn anchor_id(&self) -> &RetrievalAnchorId;
    fn state(&self) -> HydrationStateV1;
}

impl ContextUnavailable for UnavailableHydration {
    fn anchor_id(&self) -> &RetrievalAnchorId {
        self.anchor_id()
    }

    fn state(&self) -> HydrationStateV1 {
        self.state()
    }
}
