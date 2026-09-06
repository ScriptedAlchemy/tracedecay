//! Independent fielded lexical/BM25 lane contracts.
//!
//! The lane supports typed result grains, character-level typo recovery,
//! query/tool/protocol echo penalties, and exact phrases. Whole-term and
//! language-profiled subtoken postings remain independent.
//!
//! The lexical lane is separate from the exact lane; exact and lexical are
//! independently disableable and inspectable.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    CodeGenerationId, CompactCandidate, ComponentRevision, CursorPayloadDigest,
    EphemeralSanitizedQueryViewV1, FixedPointScore, RetrievalBudget, RetrievalFailure,
    RetrievalRequest, RetrieverBatch, RetrieverContinuation, RetrieverCoverage, RetrieverKind,
    RetrieverOutcome, ScoreDomainId, split_subtokens, technical_tokens,
};

use super::ports::{
    CodeCandidateBindingV1, LaneBoundEvidence, LaneEvidenceRejections, LexicalPostingReadPort,
    RetrievalPortError, candidate_checkpoint_prefix, checkpoint_digest, contract_error,
    lane_bound_evidence, lane_candidate_cap,
};

mod projection;
mod routes;

pub use self::projection::{
    CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
    CODE_LEXICAL_ARTIFACT_MAXIMUM_PAGE_RETAINED_BYTES_V1,
    CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1, CodeExactLexicalArtifactReaderV1,
    CodeExactProjectionAdapterV1, CodeLexicalArtifactBatchLimitV1,
    CodeLexicalArtifactBuildProgressV1, CodeLexicalArtifactBuilderV1, CodeLexicalArtifactErrorV1,
    CodeLexicalArtifactFinalizationPhaseV1, CodeLexicalArtifactFinalizationStepV1,
    CodeLexicalArtifactOccurrenceV1, CodeLexicalArtifactReaderV1,
    CodeLexicalArtifactSectionDigestV1, CodeLexicalArtifactWriterRevisionV1,
    CodeLexicalImportMembershipWitnessV1, CodeLexicalProjectionAdapterV1,
    CodeLexicalProjectionBuildStepV1, CodeLexicalProjectionBuildV1,
    CodeLexicalProjectionMetadataV1, LEXICAL_PROJECTION_BUILD_DEADLINE_MICROS_V1,
    PreparedCodeLexicalArtifactBatchV1, PreparedCodeLexicalArtifactPageV1,
    VerifiedCodeLexicalArtifactV1, lexical_projection_build_deadline_micros,
};
pub use self::routes::{
    LexicalAnchorV1, LexicalRouteErrorV1, LexicalRouteKindV1, LexicalRouteMatchV1,
    LexicalRouteOutcomeV1, LexicalRoutePlanV1, LexicalRouteReceiptV1, LexicalRouteV1,
    LexicalRoutingV1, MAX_LEXICAL_ANCHOR_BYTES_V1, MAX_LEXICAL_ANCHORS_V1,
    MAX_PREFERRED_SYMBOL_TOKENS_V1, merge_lexical_routes, preferred_symbol_tokens,
};

/// Wording the lexical lane uses when a port-emitted batch fails the shared
/// candidate/evidence binding checks.
const LEXICAL_REJECTIONS: LaneEvidenceRejections = LaneEvidenceRejections {
    foreign_candidate: "the lexical lane cannot emit exact-tier or other-lane candidates",
    missing_evidence: "lexical lane evidence is missing for a returned occurrence",
    unaddressed_binding: "lexical lane binding does not address its candidate",
};

/// Hard bound on character-level typo expansions selected for one request.
/// The projection sorts all eligible expansions before taking this prefix, so
/// producer order and scheduler timing cannot affect the selected terms.
pub const MAX_FUZZY_TERM_EXPANSIONS_V1: u32 = 64;

/// Maximum UTF-8 bytes in one lexical whole term, subtoken, or phrase.
pub const MAX_LEXICAL_QUERY_TERM_BYTES_V1: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LexicalQueryPartsV1 {
    pub whole_terms: Vec<String>,
    pub subtokens: Vec<String>,
    pub phrases: Vec<String>,
}

/// Shared tokenizer for production retrieval and its direct evaluator.
///
/// Whole terms are the same maximal technical tokens the code index extracts
/// (`tracedecay_domain::technical_tokens`), so a term the indexer keeps whole
/// (`foo-bar`, `a::b`, `p/q.rs`) is queried whole, and subtokens decompose
/// through the same shared grammar. Single-token queries intentionally emit
/// no subtokens so one technical term does not fan out to common subtokens.
///
/// Multi-token sanitized input is also retained as a phrase. This gives exact
/// diagnostic/error text and natural-language queries a bounded lexical phrase
/// signal; protected exact admission remains solely authority-controlled.
pub fn lexical_query_parts(query: &str) -> Result<LexicalQueryPartsV1, RetrievalPortError> {
    let query = query.trim();
    if query.is_empty()
        || query.len() > MAX_LEXICAL_QUERY_TERM_BYTES_V1
        || query.chars().any(char::is_control)
    {
        return Err(RetrievalPortError::Contract(
            "lexical query must be non-empty, trimmed, control-free, and within the v1 byte bound"
                .to_owned(),
        ));
    }
    let mut whole_terms = Vec::new();
    let mut subtokens = Vec::new();
    let split_identifiers = query.split_whitespace().nth(1).is_some();
    for (_, token) in technical_tokens(query) {
        whole_terms.push(token.to_owned());
        if split_identifiers {
            let lowercase = token.to_ascii_lowercase();
            subtokens.extend(
                split_subtokens(token)
                    .into_iter()
                    .filter(|part| part != &lowercase),
            );
        }
    }
    whole_terms.sort();
    whole_terms.dedup();
    subtokens.sort();
    subtokens.dedup();
    let phrase = query
        .strip_prefix('"')
        .and_then(|query| query.strip_suffix('"'))
        .unwrap_or(query);
    let phrases: Vec<String> = phrase
        .split_whitespace()
        .nth(1)
        .is_some()
        .then(|| phrase.to_owned())
        .into_iter()
        .collect();
    if whole_terms.is_empty() && phrases.is_empty() {
        return Err(RetrievalPortError::Contract(
            "lexical query has no searchable terms".to_owned(),
        ));
    }
    Ok(LexicalQueryPartsV1 {
        whole_terms,
        subtokens,
        phrases,
    })
}

/// Whole exact terms and language-profiled subtokens are distinct fields.
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

/// Typed lexical-lane request for identifier, phrase, token, field, and
/// bounded fuzzy retrieval.
#[derive(Debug, PartialEq, Eq)]
pub struct LexicalLaneRequest<'a> {
    pub base: RetrievalRequest,
    pub query_view: &'a EphemeralSanitizedQueryViewV1,
    pub generation: CodeGenerationId,
    pub whole_terms: Vec<String>,
    pub subtokens: Vec<String>,
    pub phrases: Vec<String>,
    pub field_filters: Vec<LexicalFieldFilterV1>,
    /// Bounded fuzzy-term budget; the profile revision pins tokenizer and
    /// normalization versions.
    pub fuzzy_budget: u32,
    pub lexical_profile_revision: ComponentRevision,
    pub score_domain: ScoreDomainId,
    pub budget: RetrievalBudget,
}

/// Per-occurrence lexical-lane evidence with its field score breakdown.
///
/// Each channel reports its raw score, rank, normalized feature, and fusion
/// contribution. None is a probability without a valid cohort-bound
/// calibrator.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LexicalLaneEvidence {
    pub binding: CodeCandidateBindingV1,
    pub field_scores_micros: Vec<(LexicalFieldV1, u64)>,
    pub matched_whole_terms: Vec<String>,
    pub matched_subtokens: Vec<String>,
    pub matched_phrases: Vec<String>,
    pub typo_recovery_applied: bool,
    pub echo_penalty_applied: bool,
}

impl LaneBoundEvidence for LexicalLaneEvidence {
    fn binding(&self) -> &CodeCandidateBindingV1 {
        &self.binding
    }
}

/// The independently disableable lexical-lane retriever contract.
///
/// Missing lexical authority rejects the request as unavailable.
pub trait LexicalLaneRetriever {
    /// Retrieve the committed lexical candidate prefix for `request`.
    fn retrieve_lexical(
        &self,
        request: &LexicalLaneRequest<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<LexicalLaneEvidence>>, RetrievalPortError>;
}

impl LexicalLaneRequest<'_> {
    pub fn validate(&self) -> Result<(), RetrievalPortError> {
        self.base.budget.validate().map_err(contract_error)?;
        self.budget.validate().map_err(contract_error)?;
        self.generation.validate().map_err(contract_error)?;
        self.lexical_profile_revision
            .validate()
            .map_err(contract_error)?;
        self.score_domain.validate().map_err(contract_error)?;
        if self.fuzzy_budget > MAX_FUZZY_TERM_EXPANSIONS_V1 {
            return Err(RetrievalPortError::Contract(format!(
                "lexical fuzzy budget exceeds the v1 bound of {MAX_FUZZY_TERM_EXPANSIONS_V1}"
            )));
        }
        if self.whole_terms.is_empty() && self.subtokens.is_empty() && self.phrases.is_empty() {
            return Err(RetrievalPortError::Contract(
                "lexical requests require at least one whole term, subtoken, or phrase".to_owned(),
            ));
        }
        for term in self
            .whole_terms
            .iter()
            .chain(self.subtokens.iter())
            .chain(self.phrases.iter())
        {
            if term.is_empty()
                || term.trim() != term
                || term.len() > MAX_LEXICAL_QUERY_TERM_BYTES_V1
                || term.chars().any(char::is_control)
            {
                return Err(RetrievalPortError::Contract(
                    "lexical terms must be non-empty, trimmed, control-free, and within the v1 byte bound"
                        .to_owned(),
                ));
            }
        }
        let mut filtered_fields = BTreeSet::new();
        for filter in &self.field_filters {
            if !filtered_fields.insert(filter.field) {
                return Err(RetrievalPortError::Contract(
                    "lexical field filters must name each field at most once".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

impl LexicalLaneEvidence {
    pub fn validate(&self, request: &LexicalLaneRequest<'_>) -> Result<(), RetrievalPortError> {
        request.validate()?;
        self.validate_against_validated_request(request)
    }

    /// Same rejection set as [`Self::validate`], minus the request
    /// revalidation the caller has already performed.
    ///
    /// The lane validates the request once per retrieval; re-running it for
    /// every candidate in the batch is pure hot-path cost.
    fn validate_against_validated_request(
        &self,
        request: &LexicalLaneRequest<'_>,
    ) -> Result<(), RetrievalPortError> {
        if self.binding.occurrence.generation != request.generation {
            return Err(RetrievalPortError::GenerationMismatch);
        }
        if self.field_scores_micros.is_empty() {
            return Err(RetrievalPortError::Contract(
                "lexical lane evidence requires at least one field score".to_owned(),
            ));
        }
        let mut scored_fields = BTreeSet::new();
        for (field, _) in &self.field_scores_micros {
            if !scored_fields.insert(*field) {
                return Err(RetrievalPortError::Contract(
                    "lexical lane evidence scores one field more than once".to_owned(),
                ));
            }
        }
        for term in &self.matched_whole_terms {
            if !request.whole_terms.contains(term) {
                return Err(RetrievalPortError::Contract(
                    "lexical lane evidence matches a whole term outside the request".to_owned(),
                ));
            }
        }
        for subtoken in &self.matched_subtokens {
            if !request.subtokens.contains(subtoken) {
                return Err(RetrievalPortError::Contract(
                    "lexical lane evidence matches a subtoken outside the request".to_owned(),
                ));
            }
        }
        for phrase in &self.matched_phrases {
            if !request.phrases.contains(phrase) {
                return Err(RetrievalPortError::Contract(
                    "lexical lane evidence matches a phrase outside the request".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

/// Whether `field` survives the request's field filters: include filters
/// form an explicit whitelist when present, and exclude filters always
/// remove their field.
fn field_admitted(filters: &[LexicalFieldFilterV1], field: LexicalFieldV1) -> bool {
    let whitelisted = !filters.iter().any(|filter| filter.include)
        || filters
            .iter()
            .any(|filter| filter.include && filter.field == field);
    let excluded = filters
        .iter()
        .any(|filter| !filter.include && filter.field == field);
    whitelisted && !excluded
}

/// The independent fielded lexical/BM25 lane over typed result grains.
///
/// Whole-term and language-profiled subtoken postings remain independent of
/// the exact lane.
///
/// The lane composes the store-side [`LexicalPostingReadPort`]. It enforces
/// field filters, recomputes every candidate's raw score as the checked
/// fixed-point sum of its admitted per-field micros (no float ever crosses
/// the candidate identity; per-field weighting belongs to the locked fusion
/// profile, not the lane), canonicalizes the committed prefix, applies the
/// budget cutoff, and reports typed coverage with a deterministic checkpoint
/// digest. Exact-tier candidates and admission proofs can never enter this
/// lane.
#[derive(Clone, Debug)]
pub struct LexicalLane<P> {
    postings: P,
}

impl<P> LexicalLane<P> {
    pub fn new(postings: P) -> Self {
        Self { postings }
    }
}

impl<P> LexicalLane<P>
where
    P: LexicalPostingReadPort,
{
    /// Validate one port-emitted batch against the request, apply field
    /// filters, then rebuild the committed deterministic prefix: canonical
    /// score order, sequential ordinals, typed coverage, budget cutoff, and
    /// a checkpoint digest.
    fn enforce_batch(
        &self,
        request: &LexicalLaneRequest<'_>,
        batch: &RetrieverBatch<LexicalLaneEvidence>,
    ) -> Result<RetrieverBatch<LexicalLaneEvidence>, RetrievalPortError> {
        batch.validate().map_err(contract_error)?;
        let mut admitted: Vec<(CompactCandidate, LexicalLaneEvidence, FixedPointScore)> =
            Vec::with_capacity(batch.candidates.len());
        let mut excluded = 0_u64;
        for candidate in &batch.candidates {
            let evidence = lane_bound_evidence(
                batch,
                candidate,
                RetrieverKind::Lexical,
                &LEXICAL_REJECTIONS,
            )?;
            evidence.validate_against_validated_request(request)?;
            let mut filtered = evidence.clone();
            filtered
                .field_scores_micros
                .retain(|(field, _)| field_admitted(&request.field_filters, *field));
            if filtered.field_scores_micros.is_empty() {
                // A candidate scored only on filtered-out fields is excluded
                // by the typed field filters; it is accounted, never silent.
                excluded += 1;
                continue;
            }
            let mut raw_score = FixedPointScore::ZERO;
            for (_, field_score) in &filtered.field_scores_micros {
                raw_score = raw_score
                    .checked_add(FixedPointScore(*field_score))
                    .map_err(contract_error)?;
            }
            admitted.push((candidate.clone(), filtered, raw_score));
        }
        // Canonical deterministic order: recomputed fixed-point score
        // (descending), then stable occurrence identity, then the evidence
        // anchor. Port emission order can never select a different prefix.
        admitted.sort_by(|left, right| {
            right
                .2
                .cmp(&left.2)
                .then_with(|| {
                    left.0
                        .source_occurrence_id
                        .cmp(&right.0.source_occurrence_id)
                })
                .then_with(|| {
                    left.0
                        .retriever_evidence_anchor
                        .cmp(&right.0.retriever_evidence_anchor)
                })
        });
        let cap = lane_candidate_cap(&request.budget, &request.base.budget);
        let examined = batch.coverage.examined.max(batch.candidates.len() as u64);
        let truncated = admitted.len().saturating_sub(cap);
        admitted.truncate(cap);
        let mut candidates = Vec::with_capacity(admitted.len());
        let mut evidence_by_occurrence = BTreeMap::new();
        for (ordinal, (mut candidate, evidence, raw_score)) in admitted.into_iter().enumerate() {
            candidate.ordinal_rank = ordinal as u32;
            candidate.raw_score = raw_score;
            evidence_by_occurrence.insert(candidate.source_occurrence_id.clone(), evidence);
            candidates.push(candidate);
        }
        // Preserve the port's own truncation accounting: a port that already
        // capped its batch reported every eligible row and its surplus, so a
        // pre-capped search must stay capped and non-exhausted here instead
        // of being reported complete.
        let seen = (candidates.len() + truncated) as u64 + excluded;
        let eligible = batch.coverage.eligible.max(seen).saturating_sub(excluded);
        let capped = batch.coverage.capped.saturating_add(truncated as u64);
        let exhausted = truncated == 0 && batch.coverage.capped == 0;
        let checkpoint_digest = lexical_checkpoint_digest(&request.generation, &candidates)?;
        let rebuilt = RetrieverBatch {
            candidates,
            evidence_by_occurrence,
            coverage: RetrieverCoverage {
                examined,
                eligible,
                excluded: batch.coverage.excluded.saturating_add(excluded),
                capped,
                unknown: batch.coverage.unknown,
            },
            continuation: Some(RetrieverContinuation {
                lane: RetrieverKind::Lexical,
                checkpoint_digest,
                exhausted,
            }),
        };
        rebuilt.validate().map_err(contract_error)?;
        Ok(rebuilt)
    }
}

impl<P> LexicalLaneRetriever for LexicalLane<P>
where
    P: LexicalPostingReadPort,
{
    #[hotpath::measure(label = "query.lane.lexical")]
    fn retrieve_lexical(
        &self,
        request: &LexicalLaneRequest<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<LexicalLaneEvidence>>, RetrievalPortError> {
        request.validate()?;
        let outcome = match self.postings.read_lexical_postings(request) {
            Ok(outcome) => outcome,
            // A missing lexical authority rejects the request as a typed
            // unavailable outcome, never a substitution.
            Err(RetrievalPortError::AuthorityUnavailable(detail)) => {
                return Ok(RetrieverOutcome::Unavailable(
                    RetrievalFailure::AuthorityUnavailable { detail },
                ));
            }
            Err(error) => return Err(error),
        };
        let outcome = match outcome {
            RetrieverOutcome::Complete(batch) => {
                RetrieverOutcome::Complete(self.enforce_batch(request, &batch)?)
            }
            RetrieverOutcome::Partial { value, reason } => RetrieverOutcome::Partial {
                value: self.enforce_batch(request, &value)?,
                reason,
            },
            outcome => outcome,
        };
        crate::hotpath_metrics::record_lane(
            "query.lane.lexical.candidates",
            "query.lane.lexical.examined",
            "query.lane.lexical.results",
            "query.lane.lexical.residency",
            &outcome,
        );
        Ok(outcome)
    }
}

/// Deterministic digest of the lexical lane's committed prefix.
///
/// A lane contributes its admitted prefix with a committed checkpoint; cursor
/// replay binds the completed set and never recomputes it.
fn lexical_checkpoint_digest(
    generation: &CodeGenerationId,
    candidates: &[CompactCandidate],
) -> Result<CursorPayloadDigest, RetrievalPortError> {
    checkpoint_digest(&(
        "tracedecay.retrieval-lane-checkpoint.v1",
        RetrieverKind::Lexical.as_str(),
        generation.as_str(),
        candidate_checkpoint_prefix(candidates),
    ))
}

#[cfg(test)]
mod tests;
