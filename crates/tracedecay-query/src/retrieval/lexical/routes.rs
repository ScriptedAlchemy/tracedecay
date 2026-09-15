//! Additional ranked lexical routes fused into one lexical lane batch.
//!
//! The strict query always runs first. Caller anchors, preferred-symbol lookup,
//! identifier splitting, and configured aliases are visible additive routes.
//! Each route is ranked through the ordinary lexical lane against the same
//! pinned generation, then merged into the single lexical lane input that
//! composition admits. Strict hits retain precedence without alternative-score
//! inflation; other additive route scores combine. The committed prefix is
//! sorted strict-first under stable tie-breakers, and every surviving candidate
//! keeps a receipt naming the routes that ranked it.
//!
//! Routes are ranked retrieval, not exhaustive grep; they never widen the
//! lane cap and never mint exact-tier admission.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CodeGenerationId, CompactCandidate, FixedPointScore, RetrievalAnchorId, RetrievalBudget,
    RetrievalFailure, RetrieverBatch, RetrieverContinuation, RetrieverCoverage, RetrieverKind,
    RetrieverOutcome, SourceOccurrenceId, split_subtokens,
};

use super::{
    LexicalFieldFilterV1, LexicalFieldV1, LexicalLaneEvidence, LexicalProximityV1,
    LexicalQueryPartsV1, lexical_checkpoint_digest, lexical_query_parts, normalize_lexical,
};
use crate::retrieval::ports::{RetrievalPortError, contract_error, lane_candidate_cap};

/// Maximum number of caller-supplied lexical anchors on one request.
pub const MAX_LEXICAL_ANCHORS_V1: usize = 8;
/// Maximum UTF-8 bytes in one lexical anchor.
pub const MAX_LEXICAL_ANCHOR_BYTES_V1: usize = 128;
pub const MAX_LEXICAL_ALIASES_V1: usize = 8;
pub const MAX_LEXICAL_ALIAS_BYTES_V1: usize = 128;
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
    #[error("lexical aliases accept at most {max} entries; {actual} were supplied")]
    TooManyAliases { max: usize, actual: usize },
    #[error("lexical alias {index} has an invalid {side}")]
    InvalidAlias { index: usize, side: &'static str },
    #[error("lexical alias {index} maps a query to itself")]
    IdentityAlias { index: usize },
    #[error("lexical alias {index} repeats an earlier alias")]
    DuplicateAlias { index: usize },
}

/// One validated exact identifier or technical term ranked through its own
/// lexical route.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct LexicalAnchorV1(String);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct LexicalAliasV1 {
    pub strict_query: String,
    pub alternative: String,
}

/// The admitted lexical lane batch plus the per-anchor route matches that
/// produced it.
type MergedLexicalBatch = (
    RetrieverBatch<LexicalLaneEvidence>,
    BTreeMap<RetrievalAnchorId, Vec<LexicalRouteMatchV1>>,
);

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

impl LexicalAliasV1 {
    fn validate(
        &self,
        index: usize,
    ) -> Result<(LexicalQueryPartsV1, LexicalQueryPartsV1), LexicalRouteErrorV1> {
        let strict = validate_alias_text(&self.strict_query, index, "strict query")?;
        let alternative = validate_alias_text(&self.alternative, index, "alternative")?;
        if strict == alternative {
            return Err(LexicalRouteErrorV1::IdentityAlias { index });
        }
        Ok((strict, alternative))
    }
}

fn validate_alias_text(
    value: &str,
    index: usize,
    side: &'static str,
) -> Result<LexicalQueryPartsV1, LexicalRouteErrorV1> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > MAX_LEXICAL_ALIAS_BYTES_V1
        || value.chars().any(char::is_control)
    {
        return Err(LexicalRouteErrorV1::InvalidAlias { index, side });
    }
    lexical_query_parts(value)
        .map(normalized_query_parts)
        .map_err(|_| LexicalRouteErrorV1::InvalidAlias { index, side })
}

fn normalized_query_parts(mut parts: LexicalQueryPartsV1) -> LexicalQueryPartsV1 {
    for term in parts
        .whole_terms
        .iter_mut()
        .chain(&mut parts.subtokens)
        .chain(&mut parts.phrases)
    {
        *term = normalize_lexical(term);
    }
    parts
}

/// Caller-controlled options for the strict query and additive lexical routes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LexicalRoutingV1 {
    pub anchors: Vec<LexicalAnchorV1>,
    pub prefer_symbol: bool,
    pub aliases: Vec<LexicalAliasV1>,
    pub phrases: Vec<String>,
    pub proximities: Vec<LexicalProximityV1>,
    pub field_filters: Vec<LexicalFieldFilterV1>,
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
            aliases: Vec::new(),
            phrases: Vec::new(),
            proximities: Vec::new(),
            field_filters: Vec::new(),
        })
    }

    pub fn with_aliases(
        mut self,
        mut aliases: Vec<LexicalAliasV1>,
    ) -> Result<Self, LexicalRouteErrorV1> {
        if aliases.len() > MAX_LEXICAL_ALIASES_V1 {
            return Err(LexicalRouteErrorV1::TooManyAliases {
                max: MAX_LEXICAL_ALIASES_V1,
                actual: aliases.len(),
            });
        }
        aliases.sort();
        let mut seen = BTreeSet::new();
        for (index, alias) in aliases.iter().enumerate() {
            if !seen.insert(alias.validate(index)?) {
                return Err(LexicalRouteErrorV1::DuplicateAlias { index });
            }
        }
        self.aliases = aliases;
        Ok(self)
    }

    /// Query route plus the preferred-symbol name route for name-first lexical
    /// lookup.
    pub fn prefer_symbol() -> Self {
        Self {
            prefer_symbol: true,
            ..Self::default()
        }
    }
}

/// Why the lexical lane tried a configured alternative.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum LexicalAlternativeReasonV1 {
    ConfiguredVocabularyAlias,
}

/// Which route ranked a candidate. Serialized into the response so a caller
/// can see why a hit ranked.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "route", rename_all = "snake_case")]
pub enum LexicalRouteKindV1 {
    /// The caller's strict lexical query.
    Query,
    /// One caller-supplied exact identifier or term.
    Anchor { anchor: LexicalAnchorV1 },
    /// Identifier-shaped tokens of the query, restricted to symbol names.
    PreferredSymbol { tokens: Vec<String> },
    /// Identifier and path components recovered from the strict query.
    IdentifierSplit {
        strict_query: String,
        terms: Vec<String>,
    },
    /// A configured query-time vocabulary alternative.
    Alias {
        strict_query: String,
        alternative: String,
        reason: LexicalAlternativeReasonV1,
    },
}

/// One planned lexical route: its identity plus the lane request terms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LexicalRouteV1 {
    pub kind: LexicalRouteKindV1,
    pub parts: LexicalQueryPartsV1,
    pub proximities: Vec<LexicalProximityV1>,
    pub field_filters: Vec<LexicalFieldFilterV1>,
}

/// The ordered routes one hybrid query runs through the lexical lane. The
/// query route is always first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LexicalRoutePlanV1 {
    routes: Vec<LexicalRouteV1>,
}

fn identifier_split_route(
    query: &str,
    query_parts: &LexicalQueryPartsV1,
    field_filters: &[LexicalFieldFilterV1],
) -> Option<LexicalRouteV1> {
    let [whole_term] = query_parts.whole_terms.as_slice() else {
        return None;
    };
    let canonical = whole_term.to_ascii_lowercase();
    let mut seen = BTreeSet::new();
    let terms = split_subtokens(whole_term)
        .into_iter()
        .take(MAX_PREFERRED_SYMBOL_TOKENS_V1)
        .filter(|term| term != &canonical && seen.insert(term.clone()))
        .collect::<Vec<_>>();
    let (whole_terms, proximities) = match terms.as_slice() {
        [] => return None,
        [_] => (terms.clone(), Vec::new()),
        _ => (
            Vec::new(),
            vec![LexicalProximityV1 {
                terms: terms.clone(),
                maximum_gap: 0,
            }],
        ),
    };
    Some(LexicalRouteV1 {
        kind: LexicalRouteKindV1::IdentifierSplit {
            strict_query: query.to_owned(),
            terms,
        },
        parts: LexicalQueryPartsV1 {
            whole_terms,
            subtokens: Vec::new(),
            phrases: Vec::new(),
        },
        proximities,
        field_filters: field_filters.to_vec(),
    })
}

impl LexicalRoutePlanV1 {
    /// Plan the strict query, caller-order anchors, preferred-symbol recovery,
    /// identifier splitting, then byte-sorted matching aliases.
    pub fn plan(query: &str, routing: &LexicalRoutingV1) -> Result<Self, RetrievalPortError> {
        let mut query_parts = lexical_query_parts(query)?;
        let strict_parts = normalized_query_parts(query_parts.clone());
        query_parts.phrases.extend(routing.phrases.iter().cloned());
        query_parts.phrases.sort();
        query_parts.phrases.dedup();
        let mut routes = vec![LexicalRouteV1 {
            kind: LexicalRouteKindV1::Query,
            parts: query_parts.clone(),
            proximities: routing.proximities.clone(),
            field_filters: routing.field_filters.clone(),
        }];
        for anchor in &routing.anchors {
            routes.push(LexicalRouteV1 {
                kind: LexicalRouteKindV1::Anchor {
                    anchor: anchor.clone(),
                },
                parts: lexical_query_parts(anchor.as_str())?,
                proximities: Vec::new(),
                field_filters: routing.field_filters.clone(),
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
                    proximities: Vec::new(),
                    field_filters: vec![LexicalFieldFilterV1 {
                        field: LexicalFieldV1::SymbolName,
                        include: true,
                    }],
                });
            }
        }
        if let Some(route) = identifier_split_route(query, &query_parts, &routing.field_filters) {
            routes.push(route);
        }
        for alias in &routing.aliases {
            if normalized_query_parts(lexical_query_parts(&alias.strict_query)?) != strict_parts {
                continue;
            }
            routes.push(LexicalRouteV1 {
                kind: LexicalRouteKindV1::Alias {
                    strict_query: query.to_owned(),
                    alternative: alias.alternative.clone(),
                    reason: LexicalAlternativeReasonV1::ConfiguredVocabularyAlias,
                },
                parts: lexical_query_parts(&alias.alternative)?,
                proximities: Vec::new(),
                field_filters: routing.field_filters.clone(),
            });
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
        .rsplit([':', '.'])
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
    pub spelling_variants: Vec<super::LexicalSpellingVariantV1>,
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
    pub fn has_disclosure(&self) -> bool {
        self.routes.len() > 1 || !self.matches_by_anchor.is_empty()
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
        let matches_by_anchor = query_batch
            .candidates
            .iter()
            .filter_map(|candidate| {
                let evidence = query_batch
                    .evidence_by_occurrence
                    .get(&candidate.source_occurrence_id)?;
                (!evidence.spelling_variants.is_empty()).then(|| {
                    (
                        candidate.anchor_id.clone(),
                        vec![LexicalRouteMatchV1 {
                            route: LexicalRouteKindV1::Query,
                            score_micros: candidate.raw_score.micros(),
                            matched_terms: matched_terms(evidence),
                            spelling_variants: evidence.spelling_variants.clone(),
                        }],
                    )
                })
            })
            .collect();
        let receipt = LexicalRouteReceiptV1 {
            routes: descriptors,
            matches_by_anchor,
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
        LexicalRouteKindV1::IdentifierSplit { terms, .. } => {
            format!("identifier_split:{}", terms.join(","))
        }
        LexicalRouteKindV1::Alias { alternative, .. } => format!("alias:{alternative}"),
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
    strict: bool,
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
        let is_alternative = matches!(
            kind,
            LexicalRouteKindV1::IdentifierSplit { .. } | LexicalRouteKindV1::Alias { .. }
        );
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
                spelling_variants: evidence.spelling_variants.clone(),
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
                    if is_alternative && existing.strict {
                        existing.matches.push(route_match);
                        continue;
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
                            strict: !is_alternative,
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
    ) -> Result<MergedLexicalBatch, RetrievalPortError> {
        let mut admitted: Vec<MergedCandidate> = self.by_occurrence.into_values().collect();
        admitted.sort_by(|left, right| {
            right
                .strict
                .cmp(&left.strict)
                .then_with(|| right.candidate.raw_score.cmp(&left.candidate.raw_score))
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
    if existing.binding.candidate_anchor != incoming.binding.candidate_anchor
        || existing.binding.occurrence != incoming.binding.occurrence
        || existing.binding.language_descriptor_revision
            != incoming.binding.language_descriptor_revision
        || existing.binding.source_occurrence != incoming.binding.source_occurrence
    {
        return Err(RetrievalPortError::Contract(
            "lexical routes disagree on the binding of one source occurrence".to_owned(),
        ));
    }
    // Match kinds describe each query route, not the identity of its source.
    existing
        .binding
        .matched_term_kinds
        .extend_from_slice(&incoming.binding.matched_term_kinds);
    existing.binding.matched_term_kinds.sort();
    existing.binding.matched_term_kinds.dedup();
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
    existing
        .matched_proximities
        .extend(incoming.matched_proximities.iter().cloned());
    existing.matched_proximities.sort();
    existing.matched_proximities.dedup();
    existing
        .spelling_variants
        .extend(incoming.spelling_variants.iter().cloned());
    existing.spelling_variants.sort();
    existing.spelling_variants.dedup();
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
