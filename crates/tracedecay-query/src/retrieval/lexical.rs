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
    RetrievalExecutionControl, RetrievalPortError, candidate_checkpoint_prefix, checkpoint_digest,
    contract_error, lane_bound_evidence, lane_candidate_cap,
};

mod projection;
mod routes;

pub use self::projection::{
    CLONE_FINGERPRINT_CANDIDATE_BODY_BUDGET_V1, CLONE_FINGERPRINT_HOT_POSTING_THRESHOLD_V1,
    CLONE_FINGERPRINT_POSTING_ROW_BUDGET_V1, CLONE_NEAR_MATCH_BODY_COMPARISON_BUDGET_V1,
    CLONE_NEAR_MATCH_MINIMUM_COVERAGE_MILLIONTHS_V1, CLONE_NEAR_MATCH_TOKEN_WORK_BUDGET_V1,
    CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
    CODE_LEXICAL_ARTIFACT_MAXIMUM_PAGE_RETAINED_BYTES_V1,
    CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1, CloneArtifactCursorV1, CloneArtifactPageV1,
    CloneExactArtifactMemberV1, CloneFingerprintArtifactReadV1,
    CloneFingerprintCancellationPointV1, CloneFingerprintPartialReasonV1,
    CloneFingerprintReadAccountingV1, CloneFingerprintStreamDescriptorV1, CloneNearMatchArtifactV1,
    CloneNearMatchExtentV1, CloneSelectedBlockArtifactCandidateV1,
    CloneSelectedBlockArtifactReadV1, CloneSelectedBlockContainmentClassV1, CloneSelectedBlockV1,
    CodeExactLexicalArtifactReaderV1, CodeLexicalArtifactBatchLimitV1,
    CodeLexicalArtifactBuildProgressV1, CodeLexicalArtifactBuilderV1, CodeLexicalArtifactErrorV1,
    CodeLexicalArtifactFinalizationPhaseV1, CodeLexicalArtifactFinalizationStepV1,
    CodeLexicalArtifactOccurrenceV1, CodeLexicalArtifactReaderV1,
    CodeLexicalArtifactSectionDigestV1, CodeLexicalArtifactWriterRevisionV1,
    CodeLexicalCloneSuccessorV1, CodeLexicalImportMembershipWitnessV1,
    CodeLexicalProjectionMetadataV1, MAX_CLONE_EXACT_PAGE_MEMBERS_V1,
    MAX_CLONE_FINGERPRINT_PAGE_BODIES_V1, PreparedCodeLexicalArtifactBatchV1,
    PreparedCodeLexicalArtifactPageV1, VerifiedCodeLexicalArtifactV1,
    code_lexical_artifact_build_memory_budget_for,
};
#[cfg(feature = "search-eval")]
pub use self::projection::{
    CodeExactProjectionAdapterV1, CodeLexicalProjectionAdapterV1, CodeLexicalProjectionBuildStepV1,
    CodeLexicalProjectionBuildV1, LEXICAL_PROJECTION_BUILD_DEADLINE_MICROS_V1,
    lexical_projection_build_deadline_micros,
};
pub use self::routes::{
    LexicalAliasV1, LexicalAlternativeReasonV1, LexicalAnchorV1, LexicalRouteErrorV1,
    LexicalRouteKindV1, LexicalRouteMatchV1, LexicalRouteOutcomeV1, LexicalRoutePlanV1,
    LexicalRouteReceiptV1, LexicalRouteV1, LexicalRoutingV1, MAX_LEXICAL_ALIAS_BYTES_V1,
    MAX_LEXICAL_ALIASES_V1, MAX_LEXICAL_ANCHOR_BYTES_V1, MAX_LEXICAL_ANCHORS_V1,
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
pub const MAX_LEXICAL_PROXIMITIES_V1: usize = 4;
pub const MAX_LEXICAL_PROXIMITY_TERMS_V1: usize = 8;
pub const MAX_LEXICAL_PROXIMITY_GAP_V1: u32 = 8;
pub const MAX_LEXICAL_PHRASES_V1: usize = 4;
pub const MAX_LEXICAL_FIELD_FILTERS_V1: usize = 9;

/// Maximum UTF-8 bytes in one lexical whole term, subtoken, phrase, or
/// proximity term.
pub const MAX_LEXICAL_QUERY_TERM_BYTES_V1: usize = 512;

/// Summed document-frequency budget for lexical term-source admission. Every
/// candidate is decoded from its row and scored, so the union of the request's
/// term sources — not the winner cap — decides the lane's transient allocation
/// and wall time: unbounded, a natural-language task whose terms include
/// common words hydrated ~74k rows of a 472k-chunk corpus per read (~0.95 GB
/// decoded, 5.7 s) and missed the context deadline. Term sources are admitted
/// in ascending document-frequency order until their summed frequencies would
/// exceed this bound; the most selective source is always admitted so a
/// single common-term query still answers. Phrase sources are admitted separately.
/// This is a recall/latency policy, not a hard document or allocation ceiling:
/// documents matching only pruned terms cannot rank. The initial 16,384 value
/// retains the measured policy (~100 MiB decode churn at ~6.6 KiB per row);
/// changing the reader cache must not change candidate eligibility.
pub const MAX_LEXICAL_CANDIDATE_DOCUMENTS_V1: usize = 16_384;

/// Admit `(document_frequency, source)` pairs rarest-first while the summed
/// frequency stays within [`MAX_LEXICAL_CANDIDATE_DOCUMENTS_V1`]; the rarest
/// nonempty source is always admitted. Ties keep request order so admission
/// is deterministic. Sources past the bound still weigh admitted candidates
/// through scoring; a document matching only those sources is never hydrated.
pub(crate) fn admit_candidate_sources<S>(
    mut sources: Vec<(usize, S)>,
    mut on_pruned: impl FnMut(usize, &S),
) -> Vec<S> {
    sources.retain(|(frequency, _)| *frequency > 0);
    sources.sort_by_key(|(frequency, _)| *frequency);
    let total = sources.len();
    let mut admitted_documents = 0usize;
    let mut admitted = Vec::with_capacity(total);
    for (ordinal, (frequency, source)) in sources.into_iter().enumerate() {
        let next = admitted_documents.saturating_add(frequency);
        if ordinal > 0 && next > MAX_LEXICAL_CANDIDATE_DOCUMENTS_V1 {
            on_pruned(frequency, &source);
            continue;
        }
        admitted_documents = next;
        admitted.push(source);
    }
    hotpath::gauge!("query.lane.lexical.candidate_sources_total").inc(total as u64);
    hotpath::gauge!("query.lane.lexical.candidate_sources_pruned")
        .inc((total - admitted.len()) as u64);
    hotpath::gauge!("query.lane.lexical.candidate_documents_admitted")
        .set(admitted_documents as u64);
    admitted
}

fn candidate_admission_outcome<E>(
    batch: RetrieverBatch<E>,
    term_sources: Vec<(String, u64)>,
) -> RetrieverOutcome<RetrieverBatch<E>> {
    if term_sources.is_empty() {
        RetrieverOutcome::Complete(batch)
    } else {
        tracing::debug!(
            pruned_source_count = term_sources.len(),
            source_document_frequencies = ?term_sources.iter().map(|(_, frequency)| *frequency).collect::<Vec<_>>(),
            document_frequency_budget = MAX_LEXICAL_CANDIDATE_DOCUMENTS_V1,
            "lexical candidate term sources pruned by retrieval policy"
        );
        RetrieverOutcome::Partial {
            value: batch,
            reason: RetrievalFailure::CandidateSourcesPruned {
                term_sources,
                document_frequency_budget: MAX_LEXICAL_CANDIDATE_DOCUMENTS_V1 as u64,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LexicalQueryPartsV1 {
    pub whole_terms: Vec<String>,
    pub subtokens: Vec<String>,
    pub phrases: Vec<String>,
}

pub(super) fn normalize_lexical(value: &str) -> String {
    value.to_ascii_lowercase()
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

/// Posting field selected by lexical filters and emitted in per-field score
/// evidence.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LexicalFieldV1 {
    #[serde(alias = "symbol_name")]
    SymbolName,
    #[serde(alias = "qualified_name")]
    QualifiedName,
    #[serde(alias = "path")]
    Path,
    #[serde(alias = "signature")]
    Signature,
    #[serde(alias = "documentation")]
    Documentation,
    #[serde(alias = "body_text")]
    BodyText,
    #[serde(alias = "preamble_text")]
    PreambleText,
    #[serde(alias = "exact_term")]
    ExactTerm,
    #[serde(alias = "subtoken")]
    Subtoken,
}

/// One field filter in a lexical request.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LexicalFieldFilterV1 {
    pub field: LexicalFieldV1,
    pub include: bool,
}

/// Ordered terms that must occur in one field. `maximum_gap` counts the
/// intervening tokens between each adjacent pair.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct LexicalProximityV1 {
    pub terms: Vec<String>,
    pub maximum_gap: u32,
}

impl LexicalProximityV1 {
    fn validate(&self) -> Result<(), RetrievalPortError> {
        if !(2..=MAX_LEXICAL_PROXIMITY_TERMS_V1).contains(&self.terms.len())
            || self.maximum_gap > MAX_LEXICAL_PROXIMITY_GAP_V1
        {
            return Err(RetrievalPortError::Contract(
                "lexical proximity requires two to eight terms and a maximum gap no larger than eight"
                    .to_owned(),
            ));
        }
        for term in &self.terms {
            if term.is_empty()
                || term.trim() != term
                || term.len() > MAX_LEXICAL_QUERY_TERM_BYTES_V1
                || term.chars().any(char::is_control)
                || technical_tokens(term).count() != 1
            {
                return Err(RetrievalPortError::Contract(
                    "lexical proximity terms must be single trimmed control-free terms within the v1 byte bound"
                        .to_owned(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct LexicalSpellingVariantV1 {
    pub query: String,
    pub alternative: String,
}

/// Typed lexical-lane request for identifier, phrase, proximity, token, field,
/// and bounded fuzzy retrieval.
pub struct LexicalLaneRequest<'a> {
    pub base: RetrievalRequest,
    pub query_view: &'a EphemeralSanitizedQueryViewV1,
    pub generation: CodeGenerationId,
    pub whole_terms: Vec<String>,
    pub subtokens: Vec<String>,
    pub phrases: Vec<String>,
    pub proximities: Vec<LexicalProximityV1>,
    pub field_filters: Vec<LexicalFieldFilterV1>,
    /// Bounded fuzzy-term budget; the profile revision pins tokenizer and
    /// normalization versions.
    pub fuzzy_budget: u32,
    pub lexical_profile_revision: ComponentRevision,
    pub score_domain: ScoreDomainId,
    pub budget: RetrievalBudget,
    /// The live request authority the lane consults between bounded units of
    /// row work ([`lexical_checkpoint`]). The candidate-source bound keeps one
    /// request's hydration finite, but a caller that has already settled —
    /// cancelled, past its deadline, or revoked — must not keep the shared
    /// search execution permit occupied while the remaining rows decode and
    /// score. Cancellation unwinds the scan with
    /// [`RetrievalPortError::Cancelled`] instead of an empty or partial batch.
    pub control: &'a dyn RetrievalExecutionControl,
}

/// The lexical lane's cooperative cancellation checkpoint.
///
/// Called before each candidate row is decoded and scored, and between the
/// scan's phases, so cancellation performs at most one further row visit
/// after the signal. An uncancelled request never observes it, which keeps
/// candidate order, evidence, and coverage identical to an unchecked scan.
pub(crate) fn lexical_checkpoint(
    control: &dyn RetrievalExecutionControl,
) -> Result<(), RetrievalPortError> {
    if control.is_cancelled() {
        return Err(RetrievalPortError::Cancelled);
    }
    Ok(())
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
    pub matched_proximities: Vec<LexicalProximityV1>,
    pub spelling_variants: Vec<LexicalSpellingVariantV1>,
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
        if self.whole_terms.is_empty()
            && self.subtokens.is_empty()
            && self.phrases.is_empty()
            && self.proximities.is_empty()
        {
            return Err(RetrievalPortError::Contract(
                "lexical requests require at least one whole term, subtoken, phrase, or proximity"
                    .to_owned(),
            ));
        }
        if self.proximities.len() > MAX_LEXICAL_PROXIMITIES_V1 {
            return Err(RetrievalPortError::Contract(format!(
                "lexical proximity count exceeds the v1 bound of {MAX_LEXICAL_PROXIMITIES_V1}"
            )));
        }
        for proximity in &self.proximities {
            proximity.validate()?;
        }
        if self.phrases.len() > MAX_LEXICAL_PHRASES_V1
            || self.field_filters.len() > MAX_LEXICAL_FIELD_FILTERS_V1
        {
            return Err(RetrievalPortError::Contract(
                "lexical phrases or field filters exceed their v1 request bounds".to_owned(),
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
        for (matched, requested, channel) in [
            (
                &self.matched_whole_terms,
                &request.whole_terms,
                "whole term",
            ),
            (&self.matched_subtokens, &request.subtokens, "subtoken"),
            (&self.matched_phrases, &request.phrases, "phrase"),
        ] {
            if matched.iter().any(|term| !requested.contains(term)) {
                return Err(RetrievalPortError::Contract(format!(
                    "lexical lane evidence matches a {channel} outside the request"
                )));
            }
        }
        if self
            .matched_proximities
            .iter()
            .any(|proximity| !request.proximities.contains(proximity))
        {
            return Err(RetrievalPortError::Contract(
                "lexical lane evidence matches a proximity outside the request".to_owned(),
            ));
        }
        if self.spelling_variants.iter().any(|variant| {
            !request.whole_terms.contains(&variant.query)
                || variant.alternative == variant.query
                || variant.alternative.is_empty()
        }) {
            return Err(RetrievalPortError::Contract(
                "lexical spelling evidence is not bound to the request".to_owned(),
            ));
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
        lexical_checkpoint(request.control)?;
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
