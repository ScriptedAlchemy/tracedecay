//! Independent fielded lexical/BM25 lane contracts (Plan 15: fielded BM25
//! over typed result grains, character-level typo recovery, query/tool/
//! protocol echo penalties, and exact phrase support; Plan 25:
//! `src/query/retrieval/lexical.rs` consumes whole-term and
//! language-profiled subtoken postings independently).
//!
//! The lexical lane is separate from the exact lane; exact and lexical are
//! independently disableable and inspectable.

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    CodeGenerationId, ComponentRevision, RetrievalBudget, RetrievalRequest, RetrieverBatch,
    RetrieverOutcome, ScoreDomainId,
};

use super::ports::{CodeCandidateBindingV1, RetrievalPortError};

/// Typed lexical fields over code-search result grains (Plan 15: typed
/// result grains; Plan 25: whole exact terms and language-profiled subtokens
/// are distinct fields).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LexicalFieldV1 {
    SymbolName,
    QualifiedName,
    Path,
    BodyText,
    PreambleText,
    ExactTerm,
    Subtoken,
}

/// One field filter in a lexical request.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LexicalFieldFilterV1 {
    pub field: LexicalFieldV1,
    pub include: bool,
}

/// Typed lexical-lane request (Plan 05: exact identifier, phrase, token,
/// field, and bounded fuzzy requests).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LexicalLaneRequest {
    pub base: RetrievalRequest,
    pub generation: CodeGenerationId,
    pub whole_terms: Vec<String>,
    pub subtokens: Vec<String>,
    pub phrases: Vec<String>,
    pub field_filters: Vec<LexicalFieldFilterV1>,
    /// Bounded fuzzy-term budget; the profile revision pins tokenizer and
    /// normalization versions (Plan 05: lexical ranking centralizes
    /// tokenizer/profile versions).
    pub fuzzy_budget: u32,
    pub lexical_profile_revision: ComponentRevision,
    pub score_domain: ScoreDomainId,
    pub budget: RetrievalBudget,
}

/// Per-occurrence lexical-lane evidence with its field score breakdown
/// (Plan 05: return each channel's raw score, rank, normalized feature, and
/// fusion contribution; none is a probability unless a valid cohort-bound
/// calibrator says so).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LexicalLaneEvidence {
    pub binding: CodeCandidateBindingV1,
    pub field_scores_micros: Vec<(LexicalFieldV1, u64)>,
    pub matched_whole_terms: Vec<String>,
    pub matched_subtokens: Vec<String>,
    pub typo_recovery_applied: bool,
    pub echo_penalty_applied: bool,
}

/// The lexical-lane retriever contract (Plan 25: independently disableable;
/// missing lexical authority rejects the request as unavailable — Plan 15
/// pipeline step 3).
pub trait LexicalLaneRetriever {
    /// Retrieve the committed lexical candidate prefix for `request`.
    fn retrieve_lexical(
        &self,
        request: &LexicalLaneRequest,
    ) -> Result<RetrieverOutcome<RetrieverBatch<LexicalLaneEvidence>>, RetrievalPortError>;
}
