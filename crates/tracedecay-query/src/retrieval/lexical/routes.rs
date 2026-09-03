//! Additional ranked lexical routes fused into one lexical lane batch.
//!
//! A hybrid query may carry exact identifiers the caller already knows
//! (`lexical_anchors`) and may ask for a symbol-name route built from the
//! identifier-shaped tokens of its own text (`prefer_symbol`). Each route is
//! ranked through the ordinary lexical lane against the same pinned
//! generation, then the route batches are merged into the single lexical lane
//! input that composition admits. The merge is deterministic: a candidate's
//! lexical raw score is the checked sum of its route scores, the committed
//! prefix is re-sorted under the lane's canonical order, and every surviving
//! candidate keeps a receipt naming the routes that ranked it.
//!
//! Routes are ranked retrieval, not exhaustive grep; they never widen the
//! lane cap and never mint exact-tier admission.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CodeGenerationId, CompactCandidate, FixedPointScore, RetrievalAnchorId, RetrievalBudget,
    RetrievalFailure, RetrieverBatch, RetrieverContinuation, RetrieverCoverage, RetrieverKind,
    RetrieverOutcome, SourceOccurrenceId,
};

use super::{
    LexicalFieldFilterV1, LexicalFieldV1, LexicalLaneEvidence, LexicalQueryPartsV1,
    lexical_checkpoint_digest, lexical_query_parts,
};
use crate::retrieval::ports::{RetrievalPortError, contract_error, lane_candidate_cap};

/// Maximum number of caller-supplied lexical anchors on one request.
pub const MAX_LEXICAL_ANCHORS_V1: usize = 8;
/// Maximum UTF-8 bytes in one lexical anchor.
pub const MAX_LEXICAL_ANCHOR_BYTES_V1: usize = 128;
/// Maximum identifier-shaped tokens the preferred-symbol route ranks; the
/// tokens are taken in query order so the bound is deterministic.
pub const MAX_PREFERRED_SYMBOL_TOKENS_V1: usize = 8;

/// Query words that look like identifiers but name what the caller is asking
/// about rather than a symbol. Compared case-insensitively.
const PREFERRED_SYMBOL_STOPLIST_V1: &[&str] = &[
    "class",
    "struct",
    "enum",
    "interface",
    "function",
    "method",
    "type",
    "const",
    "let",
    "var",
    "namespace",
    "where",
    "find",
    "explain",
    "fn",
    "def",
    "trait",
    "impl",
    "module",
    "the",
    "a",
    "an",
    "of",
    "in",
    "is",
    "to",
    "for",
    "and",
    "or",
    "how",
    "what",
    "does",
    "do",
    "who",
    "calls",
    "this",
    "that",
    "with",
    "from",
    "show",
    "me",
    "all",
    "defined",
    "definition",
    "implements",
    "implementation",
];

/// Typed rejection for caller-supplied lexical routing.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum LexicalRouteErrorV1 {
    #[error("lexical_anchors accepts at most {max} anchors; {actual} were supplied")]
    TooManyAnchors { max: usize, actual: usize },
    #[error("lexical anchor {index} is empty")]
    EmptyAnchor { index: usize },
    #[error("lexical anchor {index} exceeds {max} bytes")]
    AnchorTooLong { index: usize, max: usize },
    #[error(
        "lexical anchor {index} must be one identifier or technical term: no surrounding whitespace, inner whitespace, or control characters"
    )]
    AnchorNotOneTerm { index: usize },
    #[error("lexical anchor {index} repeats an earlier anchor")]
    DuplicateAnchor { index: usize },
}

/// One validated exact identifier or technical term ranked through its own
/// lexical route.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct LexicalAnchorV1(String);

impl LexicalAnchorV1 {
    fn validate(value: &str, index: usize) -> Result<(), LexicalRouteErrorV1> {
        if value.is_empty() {
            return Err(LexicalRouteErrorV1::EmptyAnchor { index });
        }
        if value.len() > MAX_LEXICAL_ANCHOR_BYTES_V1 {
            return Err(LexicalRouteErrorV1::AnchorTooLong {
                index,
                max: MAX_LEXICAL_ANCHOR_BYTES_V1,
            });
        }
        let one_term = value.trim() == value
            && !value
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
            && lexical_query_parts(value).is_ok_and(|parts| !parts.whole_terms.is_empty());
        if !one_term {
            return Err(LexicalRouteErrorV1::AnchorNotOneTerm { index });
        }
        Ok(())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Caller-controlled lexical routing for one hybrid query. The query route
/// always runs; anchors and the preferred-symbol route are additive.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LexicalRoutingV1 {
    pub anchors: Vec<LexicalAnchorV1>,
    pub prefer_symbol: bool,
}

impl LexicalRoutingV1 {
    /// Validate raw caller anchors: bounded count, bounded bytes, one term
    /// each, no repeats. Order is preserved because it is the caller's
    /// evidence order.
    pub fn new(anchors: Vec<String>, prefer_symbol: bool) -> Result<Self, LexicalRouteErrorV1> {
        if anchors.len() > MAX_LEXICAL_ANCHORS_V1 {
            return Err(LexicalRouteErrorV1::TooManyAnchors {
                max: MAX_LEXICAL_ANCHORS_V1,
                actual: anchors.len(),
            });
        }
        let mut validated = Vec::with_capacity(anchors.len());
        let mut seen = BTreeSet::new();
        for (index, anchor) in anchors.into_iter().enumerate() {
            LexicalAnchorV1::validate(&anchor, index)?;
            if !seen.insert(anchor.clone()) {
                return Err(LexicalRouteErrorV1::DuplicateAnchor { index });
            }
            validated.push(LexicalAnchorV1(anchor));
        }
        Ok(Self {
            anchors: validated,
            prefer_symbol,
        })
    }

    /// The routing every request had before anchors existed: the query route
    /// alone.
    pub const fn query_only() -> Self {
        Self {
            anchors: Vec::new(),
            prefer_symbol: false,
        }
    }

    pub fn is_query_only(&self) -> bool {
        self.anchors.is_empty() && !self.prefer_symbol
    }
}

/// Which route ranked a candidate. Serialized into the response so a caller
/// can see why a hit ranked.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "route", rename_all = "snake_case")]
pub enum LexicalRouteKindV1 {
    /// The natural-language query, tokenized exactly as before.
    Query,
    /// One caller-supplied exact identifier or term.
    Anchor { anchor: LexicalAnchorV1 },
    /// Identifier-shaped tokens of the query, restricted to symbol names.
    PreferredSymbol { tokens: Vec<String> },
}

/// One planned lexical route: its identity plus the lane request terms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LexicalRouteV1 {
    pub kind: LexicalRouteKindV1,
    pub parts: LexicalQueryPartsV1,
    pub field_filters: Vec<LexicalFieldFilterV1>,
}

/// The ordered routes one hybrid query runs through the lexical lane. The
/// query route is always first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LexicalRoutePlanV1 {
    routes: Vec<LexicalRouteV1>,
}

impl LexicalRoutePlanV1 {
    /// Plan the query route plus every additive route the caller asked for.
    /// A preferred-symbol request whose query yields no identifier-shaped
    /// token adds no route; the plan's descriptors make that visible.
    pub fn plan(query: &str, routing: &LexicalRoutingV1) -> Result<Self, RetrievalPortError> {
        let mut routes = vec![LexicalRouteV1 {
            kind: LexicalRouteKindV1::Query,
            parts: lexical_query_parts(query)?,
            field_filters: Vec::new(),
        }];
        for anchor in &routing.anchors {
            routes.push(LexicalRouteV1 {
                kind: LexicalRouteKindV1::Anchor {
                    anchor: anchor.clone(),
                },
                parts: lexical_query_parts(anchor.as_str())?,
                field_filters: Vec::new(),
            });
        }
        if routing.prefer_symbol {
            let tokens = preferred_symbol_tokens(query);
            if !tokens.is_empty() {
                let mut whole_terms = tokens.clone();
                whole_terms.sort();
                whole_terms.dedup();
                routes.push(LexicalRouteV1 {
                    kind: LexicalRouteKindV1::PreferredSymbol { tokens },
                    parts: LexicalQueryPartsV1 {
                        whole_terms,
                        subtokens: Vec::new(),
                        phrases: Vec::new(),
                    },
                    field_filters: vec![LexicalFieldFilterV1 {
                        field: LexicalFieldV1::SymbolName,
                        include: true,
                    }],
                });
            }
        }
        Ok(Self { routes })
    }

    pub fn routes(&self) -> &[LexicalRouteV1] {
        &self.routes
    }

    pub fn descriptors(&self) -> Vec<LexicalRouteKindV1> {
        self.routes.iter().map(|route| route.kind.clone()).collect()
    }
}

/// Identifier-shaped tokens of a natural-language query, in query order.
///
/// A token is a maximal run of `[A-Za-z0-9_:.]` that starts with a letter or
/// underscore. Qualified spellings (`Foo::bar`, `Foo.bar`) normalize to their
/// trailing segment because the symbol-name field indexes bare names.
/// Stoplisted query words and single characters are dropped; the result is
/// deduplicated and bounded to [`MAX_PREFERRED_SYMBOL_TOKENS_V1`].
pub fn preferred_symbol_tokens(query: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut seen = BTreeSet::new();
    let mut current = String::new();
    let mut flush = |current: &mut String| {
        if current.is_empty() {
            return;
        }
        let token = std::mem::take(current);
        let Some(name) = trailing_symbol_name(&token) else {
            return;
        };
        if name.chars().count() < 2
            || PREFERRED_SYMBOL_STOPLIST_V1
                .iter()
                .any(|stop| stop.eq_ignore_ascii_case(name))
        {
            return;
        }
        if tokens.len() < MAX_PREFERRED_SYMBOL_TOKENS_V1 && seen.insert(name.to_owned()) {
            tokens.push(name.to_owned());
        }
    };
    for character in query.chars() {
        let continues = character.is_ascii_alphanumeric() || matches!(character, '_' | ':' | '.');
        let starts = character.is_ascii_alphabetic() || character == '_';
        if current.is_empty() {
            if starts {
                current.push(character);
            }
        } else if continues {
            current.push(character);
        } else {
            flush(&mut current);
        }
    }
    flush(&mut current);
    tokens
}

/// The bare identifier a qualified token names, or `None` when no segment is
/// identifier-shaped (for example a trailing `.` or a numeric segment).
fn trailing_symbol_name(token: &str) -> Option<&str> {
    token
        .rsplit(|character| character == ':' || character == '.')
        .find(|segment| !segment.is_empty())
        .filter(|segment| {
            segment
                .chars()
                .next()
                .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
                && segment
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
        })
}

/// One executed route: its identity and the lane's typed outcome for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LexicalRouteOutcomeV1 {
    pub kind: LexicalRouteKindV1,
    pub outcome: RetrieverOutcome<RetrieverBatch<LexicalLaneEvidence>>,
}

/// Why one route ranked one candidate.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LexicalRouteMatchV1 {
    pub route: LexicalRouteKindV1,
    pub score_micros: u64,
    pub matched_terms: Vec<String>,
}

/// Route evidence for one composed lexical lane, keyed by candidate anchor so
/// the response can attach it to each ranked result. It is additive
/// presentation metadata: never part of ranking identity, fallback bytes, or
/// cursor state.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LexicalRouteReceiptV1 {
    pub routes: Vec<LexicalRouteKindV1>,
    pub matches_by_anchor: BTreeMap<RetrievalAnchorId, Vec<LexicalRouteMatchV1>>,
}

impl LexicalRouteReceiptV1 {
    /// Whether the caller asked for anything beyond the query route.
    pub fn has_additional_routes(&self) -> bool {
        self.routes.len() > 1
    }
}

/// Merge the executed routes into the one lexical lane input composition
/// admits.
///
/// The query route is authoritative for lane availability: when it did not
/// serve, its typed outcome is returned unchanged. A servable query route
/// merged with a non-servable additive route yields a `Partial` outcome whose
/// reason names the failed route, so recall is never overstated.
pub fn merge_lexical_routes(
    generation: &CodeGenerationId,
    lane_budget: &RetrievalBudget,
    base_budget: &RetrievalBudget,
    routes: Vec<LexicalRouteOutcomeV1>,
) -> Result<
    (
        RetrieverOutcome<RetrieverBatch<LexicalLaneEvidence>>,
        LexicalRouteReceiptV1,
    ),
    RetrievalPortError,
> {
    let descriptors: Vec<LexicalRouteKindV1> =
        routes.iter().map(|route| route.kind.clone()).collect();
    let mut routes = routes.into_iter();
    let query = routes.next().ok_or_else(|| {
        RetrievalPortError::Contract("lexical routing requires the query route".to_owned())
    })?;
    if query.kind != LexicalRouteKindV1::Query {
        return Err(RetrievalPortError::Contract(
            "the first lexical route must be the query route".to_owned(),
        ));
    }
    let (query_batch, mut partial_reason) = match query.outcome {
        RetrieverOutcome::Complete(batch) => (batch, None),
        RetrieverOutcome::Partial { value, reason } => (value, Some(reason)),
        other => {
            return Ok((
                other,
                LexicalRouteReceiptV1 {
                    routes: descriptors,
                    matches_by_anchor: BTreeMap::new(),
                },
            ));
        }
    };
    let additional: Vec<LexicalRouteOutcomeV1> = routes.collect();
    if additional.is_empty() {
        // The query route alone is the pre-existing lexical lane: its batch
        // passes through untouched and no per-candidate route evidence is
        // recorded, so a plain query costs exactly what it always did.
        let receipt = LexicalRouteReceiptV1 {
            routes: descriptors,
            matches_by_anchor: BTreeMap::new(),
        };
        let outcome = match partial_reason {
            Some(reason) => RetrieverOutcome::Partial {
                value: query_batch,
                reason,
            },
            None => RetrieverOutcome::Complete(query_batch),
        };
        return Ok((outcome, receipt));
    }

    let mut merged = MergedRoutes::default();
    merged.absorb(&LexicalRouteKindV1::Query, &query_batch)?;
    for route in &additional {
        match &route.outcome {
            RetrieverOutcome::Complete(batch) => merged.absorb(&route.kind, batch)?,
            RetrieverOutcome::Partial { value, reason } => {
                merged.absorb(&route.kind, value)?;
                partial_reason.get_or_insert_with(|| reason.clone());
            }
            other => {
                merged.exhausted = false;
                partial_reason.get_or_insert_with(|| additive_route_failure(&route.kind, other));
            }
        }
    }
    let (batch, matches_by_anchor) =
        merged.into_batch(generation, lane_candidate_cap(lane_budget, base_budget))?;
    let receipt = LexicalRouteReceiptV1 {
        routes: descriptors,
        matches_by_anchor,
    };
    let outcome = match partial_reason {
        Some(reason) => RetrieverOutcome::Partial {
            value: batch,
            reason,
        },
        None => RetrieverOutcome::Complete(batch),
    };
    Ok((outcome, receipt))
}

fn additive_route_failure(
    kind: &LexicalRouteKindV1,
    outcome: &RetrieverOutcome<RetrieverBatch<LexicalLaneEvidence>>,
) -> RetrievalFailure {
    let route = route_label(kind);
    match outcome {
        RetrieverOutcome::Unavailable(failure) => failure.clone(),
        RetrieverOutcome::Stale(_) => RetrievalFailure::StaleSource,
        RetrieverOutcome::Denied => RetrievalFailure::AuthorityUnavailable {
            detail: format!("lexical route {route} was not served"),
        },
        RetrieverOutcome::BudgetExceeded(_) => RetrievalFailure::Internal {
            detail: format!("lexical route {route} exceeded its budget"),
        },
        RetrieverOutcome::TimedOut(_) => RetrievalFailure::Internal {
            detail: format!("lexical route {route} timed out"),
        },
        RetrieverOutcome::Cancelled => RetrievalFailure::Internal {
            detail: format!("lexical route {route} was cancelled"),
        },
        RetrieverOutcome::Complete(_) | RetrieverOutcome::Partial { .. } => {
            RetrievalFailure::Internal {
                detail: format!("lexical route {route} reported a servable outcome as a failure"),
            }
        }
    }
}

fn route_label(kind: &LexicalRouteKindV1) -> String {
    match kind {
        LexicalRouteKindV1::Query => "query".to_owned(),
        LexicalRouteKindV1::Anchor { anchor } => format!("anchor:{}", anchor.as_str()),
        LexicalRouteKindV1::PreferredSymbol { tokens } => {
            format!("preferred_symbol:{}", tokens.join(","))
        }
    }
}

fn matched_terms(evidence: &LexicalLaneEvidence) -> Vec<String> {
    let mut terms: Vec<String> = evidence
        .matched_whole_terms
        .iter()
        .chain(&evidence.matched_subtokens)
        .chain(&evidence.matched_phrases)
        .cloned()
        .collect();
    terms.sort();
    terms.dedup();
    terms
}

struct MergedCandidate {
    candidate: CompactCandidate,
    evidence: LexicalLaneEvidence,
    matches: Vec<LexicalRouteMatchV1>,
}

#[derive(Default)]
struct MergedRoutes {
    by_occurrence: BTreeMap<SourceOccurrenceId, MergedCandidate>,
    coverage: RetrieverCoverage,
    exhausted: bool,
    absorbed_routes: usize,
}

impl MergedRoutes {
    fn absorb(
        &mut self,
        kind: &LexicalRouteKindV1,
        batch: &RetrieverBatch<LexicalLaneEvidence>,
    ) -> Result<(), RetrievalPortError> {
        batch.validate().map_err(contract_error)?;
        let route_exhausted = batch
            .continuation
            .as_ref()
            .is_some_and(|continuation| continuation.exhausted);
        self.exhausted = if self.absorbed_routes == 0 {
            route_exhausted
        } else {
            self.exhausted && route_exhausted
        };
        self.absorbed_routes += 1;
        self.coverage.examined = self
            .coverage
            .examined
            .saturating_add(batch.coverage.examined);
        self.coverage.excluded = self
            .coverage
            .excluded
            .saturating_add(batch.coverage.excluded);
        self.coverage.capped = self.coverage.capped.saturating_add(batch.coverage.capped);
        self.coverage.unknown = self.coverage.unknown.saturating_add(batch.coverage.unknown);
        for candidate in &batch.candidates {
            let evidence = batch
                .evidence_by_occurrence
                .get(&candidate.source_occurrence_id)
                .ok_or_else(|| {
                    RetrievalPortError::Contract(
                        "lexical route evidence is missing for a returned occurrence".to_owned(),
                    )
                })?;
            let route_match = LexicalRouteMatchV1 {
                route: kind.clone(),
                score_micros: candidate.raw_score.micros(),
                matched_terms: matched_terms(evidence),
            };
            match self.by_occurrence.get_mut(&candidate.source_occurrence_id) {
                Some(existing) => {
                    if existing.candidate.anchor_id != candidate.anchor_id
                        || existing.candidate.logical_evidence_id != candidate.logical_evidence_id
                        || existing.candidate.retriever_evidence_anchor
                            != candidate.retriever_evidence_anchor
                    {
                        return Err(RetrievalPortError::Contract(
                            "lexical routes disagree on the identity of one source occurrence"
                                .to_owned(),
                        ));
                    }
                    existing.candidate.raw_score = existing
                        .candidate
                        .raw_score
                        .checked_add(candidate.raw_score)
                        .map_err(contract_error)?;
                    merge_evidence(&mut existing.evidence, evidence)?;
                    existing.matches.push(route_match);
                }
                None => {
                    self.by_occurrence.insert(
                        candidate.source_occurrence_id.clone(),
                        MergedCandidate {
                            candidate: candidate.clone(),
                            evidence: evidence.clone(),
                            matches: vec![route_match],
                        },
                    );
                }
            }
        }
        Ok(())
    }

    fn into_batch(
        self,
        generation: &CodeGenerationId,
        cap: usize,
    ) -> Result<
        (
            RetrieverBatch<LexicalLaneEvidence>,
            BTreeMap<RetrievalAnchorId, Vec<LexicalRouteMatchV1>>,
        ),
        RetrievalPortError,
    > {
        let mut admitted: Vec<MergedCandidate> = self.by_occurrence.into_values().collect();
        // The lane's canonical order: recomputed score descending, then
        // occurrence identity, then evidence anchor. Route execution order
        // can never select a different prefix.
        admitted.sort_by(|left, right| {
            right
                .candidate
                .raw_score
                .cmp(&left.candidate.raw_score)
                .then_with(|| {
                    left.candidate
                        .source_occurrence_id
                        .cmp(&right.candidate.source_occurrence_id)
                })
                .then_with(|| {
                    left.candidate
                        .retriever_evidence_anchor
                        .cmp(&right.candidate.retriever_evidence_anchor)
                })
        });
        let eligible = admitted.len() as u64;
        let truncated = admitted.len().saturating_sub(cap);
        admitted.truncate(cap);
        let mut candidates = Vec::with_capacity(admitted.len());
        let mut evidence_by_occurrence = BTreeMap::new();
        let mut matches_by_anchor: BTreeMap<RetrievalAnchorId, Vec<LexicalRouteMatchV1>> =
            BTreeMap::new();
        for (ordinal, merged) in admitted.into_iter().enumerate() {
            let mut candidate = merged.candidate;
            candidate.ordinal_rank = ordinal as u32;
            evidence_by_occurrence.insert(candidate.source_occurrence_id.clone(), merged.evidence);
            matches_by_anchor
                .entry(candidate.anchor_id.clone())
                .or_default()
                .extend(merged.matches);
            candidates.push(candidate);
        }
        let checkpoint_digest = lexical_checkpoint_digest(generation, &candidates)?;
        let batch = RetrieverBatch {
            candidates,
            evidence_by_occurrence,
            coverage: RetrieverCoverage {
                examined: self.coverage.examined,
                eligible,
                excluded: self.coverage.excluded,
                capped: self.coverage.capped.saturating_add(truncated as u64),
                unknown: self.coverage.unknown,
            },
            continuation: Some(RetrieverContinuation {
                lane: RetrieverKind::Lexical,
                checkpoint_digest,
                exhausted: self.exhausted && truncated == 0,
            }),
        };
        batch.validate().map_err(contract_error)?;
        Ok((batch, matches_by_anchor))
    }
}

/// Fold one route's evidence for an occurrence into the evidence already
/// merged for it: per-field scores add, matched terms union, flags OR.
fn merge_evidence(
    existing: &mut LexicalLaneEvidence,
    incoming: &LexicalLaneEvidence,
) -> Result<(), RetrievalPortError> {
    if existing.binding != incoming.binding {
        return Err(RetrievalPortError::Contract(
            "lexical routes disagree on the binding of one source occurrence".to_owned(),
        ));
    }
    for (field, score) in &incoming.field_scores_micros {
        match existing
            .field_scores_micros
            .iter_mut()
            .find(|(existing_field, _)| existing_field == field)
        {
            Some((_, existing_score)) => {
                *existing_score = FixedPointScore(*existing_score)
                    .checked_add(FixedPointScore(*score))
                    .map_err(contract_error)?
                    .micros();
            }
            None => existing.field_scores_micros.push((*field, *score)),
        }
    }
    union_terms(
        &mut existing.matched_whole_terms,
        &incoming.matched_whole_terms,
    );
    union_terms(&mut existing.matched_subtokens, &incoming.matched_subtokens);
    union_terms(&mut existing.matched_phrases, &incoming.matched_phrases);
    existing.typo_recovery_applied |= incoming.typo_recovery_applied;
    existing.echo_penalty_applied |= incoming.echo_penalty_applied;
    Ok(())
}

fn union_terms(existing: &mut Vec<String>, incoming: &[String]) {
    existing.extend(incoming.iter().cloned());
    existing.sort();
    existing.dedup();
}

#[cfg(test)]
mod tests;
