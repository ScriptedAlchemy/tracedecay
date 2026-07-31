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

    /// Streaming assembly policy.
    fn token_policy(&self) -> TokenPolicy {
        TokenPolicy::Whitespace
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

/// Canonical ordered text admission for compatibility bindings that must
/// preserve richer transport metadata around each context block.
///
/// The temporal context module owns the budget and UTF-8-safe slicing policy;
/// callers only translate the admitted slice into their legacy response type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderedTextContextAdmission {
    pub content: Option<String>,
    pub limit: u64,
    pub returned_chars: u64,
    pub total_chars: u64,
    pub truncated: bool,
    pub next_content_offset: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderedTextContextAssembler {
    max_chars: usize,
    used_chars: usize,
}

impl OrderedTextContextAssembler {
    pub const fn new(max_chars: usize) -> Self {
        Self {
            max_chars,
            used_chars: 0,
        }
    }

    pub const fn used_chars(&self) -> usize {
        self.used_chars
    }

    pub fn admit(&mut self, content: &str) -> OrderedTextContextAdmission {
        let remaining = self.max_chars.saturating_sub(self.used_chars);
        let total_chars = content.chars().count();
        if remaining == 0 {
            return OrderedTextContextAdmission {
                content: None,
                limit: 0,
                returned_chars: 0,
                total_chars: u64::try_from(total_chars).unwrap_or(u64::MAX),
                truncated: total_chars != 0,
                next_content_offset: (total_chars != 0).then_some(0),
            };
        }

        let admitted = content.chars().take(remaining).collect::<String>();
        let returned_chars = admitted.chars().count();
        self.used_chars = self
            .used_chars
            .saturating_add(returned_chars)
            .min(self.max_chars);
        let truncated = returned_chars < total_chars;
        let returned_chars = u64::try_from(returned_chars).unwrap_or(u64::MAX);
        OrderedTextContextAdmission {
            content: Some(admitted),
            limit: u64::try_from(remaining).unwrap_or(u64::MAX),
            returned_chars,
            total_chars: u64::try_from(total_chars).unwrap_or(u64::MAX),
            truncated,
            next_content_offset: truncated.then_some(returned_chars),
        }
    }
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
