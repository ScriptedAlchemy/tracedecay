//! Canonical CLI/MCP wire contract for `tracedecay_search`: exact and lexical
//! code search over the served code generation.
//!
//! Presentation-only transport keys such as `format` are removed before the
//! request body is decoded.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::FusedCandidate;

use crate::ApplicationProblemDetailV1;
use crate::retrieval::{
    ContextLexicalAnchorV1, PrimitiveLaneCompleteV1, PrimitiveRecallV1, PrimitiveSearchFreshnessV1,
    PrimitiveUnavailableEvidenceV1, PrimitiveUnavailableStatusV1,
};

/// Caller-facing lexical routing bounds; they mirror the retrieval kernel's
/// `MAX_LEXICAL_*_V1` limits so the schema states the limits it enforces.
pub const SEARCH_MAX_LEXICAL_ANCHORS: usize = 8;
pub const SEARCH_MAX_LEXICAL_ANCHOR_BYTES: usize = 128;
pub const SEARCH_MAX_LEXICAL_ALIASES: usize = 8;
pub const SEARCH_MAX_LEXICAL_PHRASES: usize = 4;
pub const SEARCH_MAX_LEXICAL_PROXIMITIES: usize = 4;
pub const SEARCH_MAX_LEXICAL_PROXIMITY_TERMS: usize = 8;
pub const SEARCH_MAX_LEXICAL_PROXIMITY_GAP: u32 = 8;
const SEARCH_LEXICAL_FIELDS: usize = 9;

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchSurfaceRequestV1 {
    /// Search query string to match against symbol names
    pub query: String,
    /// Maximum number of results to return (default: 10)
    pub limit: Option<u64>,
    /// Authenticated opaque continuation returned as next_cursor. Repeat the same query and lexical options with it.
    pub cursor: Option<String>,
    /// Exact identifiers or technical terms (e.g. 'reserve_stock', 'Foo::bar', 'E0308') that the answer must be about. Each is ranked through the lexical lane as its own route: a hit carrying an anchor outranks every hit that carries none, exact hits included, every anchor with matches keeps at least its best sites through the lane cap, and `lexical_anchors` in the response reports each anchor's outcome (`matched` rows, `admitted` sites this response returns, `dropped` admitted sites it could not carry with the reason, `unmatched`, or `not_served`). Ranked retrieval, not exhaustive grep (use tracedecay_grep for that). Each result names the routes that ranked it. At most 8 anchors, each one whitespace-free term of at most 128 bytes, no repeats.
    #[schemars(length(max = SEARCH_MAX_LEXICAL_ANCHORS))]
    pub lexical_anchors: Option<Vec<String>>,
    /// Add a lexical route restricted to symbol-name matches for the identifier-shaped words of the query (default: false). Query words such as class/struct/function/find/explain are ignored; 'Foo::bar' and 'Foo.bar' contribute 'bar'.
    pub prefer_symbol: Option<bool>,
    /// Named query-time vocabulary aliases. The strict query always ranks first. Alias-only hits follow it with the strict query, alternative, and configured-vocabulary reason disclosed.
    #[schemars(length(max = SEARCH_MAX_LEXICAL_ALIASES))]
    pub lexical_aliases: Option<Vec<SearchLexicalAliasV1>>,
    /// Exact lexical phrases to rank through n-gram candidate pruning.
    #[schemars(length(max = SEARCH_MAX_LEXICAL_PHRASES))]
    pub lexical_phrases: Option<Vec<String>>,
    /// Ordered lexical terms that must occur within the bounded intervening-token gap.
    #[schemars(length(max = SEARCH_MAX_LEXICAL_PROXIMITIES))]
    pub lexical_proximities: Option<Vec<SearchLexicalProximityV1>>,
    /// Include or exclude lexical symbol_name, qualified_name, path, signature, documentation, body_text, preamble_text, exact_term, or subtoken fields.
    #[schemars(length(max = SEARCH_LEXICAL_FIELDS))]
    pub lexical_field_filters: Option<Vec<SearchLexicalFieldFilterV1>>,
    /// Opt in to bounded indexing of ignored dependency entry files when an import hint matches (default: false).
    pub lazy_index_ignored_dependencies: Option<bool>,
}

/// One named query-time vocabulary alias.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchLexicalAliasV1 {
    #[schemars(length(max = SEARCH_MAX_LEXICAL_ANCHOR_BYTES))]
    pub strict_query: String,
    #[schemars(length(max = SEARCH_MAX_LEXICAL_ANCHOR_BYTES))]
    pub alternative: String,
}

/// Ordered terms that must occur in one field within `maximum_gap`
/// intervening tokens of each other.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchLexicalProximityV1 {
    #[schemars(length(min = 2, max = SEARCH_MAX_LEXICAL_PROXIMITY_TERMS))]
    pub terms: Vec<String>,
    #[schemars(range(max = SEARCH_MAX_LEXICAL_PROXIMITY_GAP))]
    pub maximum_gap: u32,
}

/// A lexical posting field a search may include or exclude.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchLexicalFieldV1 {
    SymbolName,
    QualifiedName,
    Path,
    Signature,
    Documentation,
    BodyText,
    PreambleText,
    ExactTerm,
    Subtoken,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchLexicalFieldFilterV1 {
    pub field: SearchLexicalFieldV1,
    pub include: bool,
}

/// What a search served: a ranked page, or the typed reason no generation
/// could answer.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(untagged)]
pub enum SearchResultV1 {
    Complete(SearchCompleteV1),
    Unavailable(SearchUnavailableV1),
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchCompleteV1 {
    /// Freshness of the served code generation, derived from the typed lane
    /// coverage and the daemon scheduler's worktree state.
    pub freshness: PrimitiveSearchFreshnessV1,
    pub code_generation: String,
    pub query_fallback_digest: String,
    /// Authenticated continuation for the next page; null on the last page.
    pub next_cursor: Option<String>,
    pub coverage: SearchCoverageV1,
    pub results: Vec<SearchResultRowV1>,
    /// Every lexical route fused into this page; present only when a route
    /// beyond the strict query ran or ranked a result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lexical_routes: Option<Vec<SearchLexicalRouteV1>>,
    /// Outcome of every caller `lexical_anchors` entry, beside the routes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lexical_anchors: Option<Vec<ContextLexicalAnchorV1>>,
    /// The session's scope prefix. Ranked candidates carry no file path, so
    /// the scope is reported and not applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_prefix_applied: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_graph_evidence: Option<PrimitiveUnavailableEvidenceV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_import_hint: Option<SearchExternalImportHintV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchUnavailableV1 {
    pub freshness: PrimitiveSearchFreshnessV1,
    /// Always empty: no generation answered.
    pub results: Vec<SearchResultRowV1>,
    pub code_generation: Option<String>,
    /// Always null: no fallback subpayload was produced.
    pub query_fallback_digest: Option<String>,
    pub status: PrimitiveUnavailableStatusV1,
    pub reason: String,
    pub coverage: SearchCoverageV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_graph_evidence: Option<PrimitiveUnavailableEvidenceV1>,
    /// Present when indexing is parked: its cause, remedy, and wake retry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<ApplicationProblemDetailV1>,
}

/// Per-lane recall of one search, so a full-recall answer is told apart from
/// one produced while a lane was down.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchCoverageV1 {
    pub exact: SearchLaneStatusV1,
    pub lexical: SearchLaneStatusV1,
    pub graph: SearchLaneStatusV1,
    pub recall: PrimitiveRecallV1,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum SearchLaneStatusV1 {
    Complete(PrimitiveLaneCompleteV1),
    State(SearchLaneStateV1),
}

/// A lane that did not serve the current complete generation. A partial
/// lane names its generation and reason when it has them, null otherwise.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum SearchLaneStateV1 {
    Stale {
        generation: String,
    },
    Partial {
        generation: Option<String>,
        reason: Option<String>,
    },
    Unavailable {
        reason: String,
    },
}

/// One ranked candidate with its generation-bound display metadata.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchResultRowV1 {
    pub candidate: FusedCandidate,
    pub final_ordinal: u32,
    /// The graph symbol to read with `tracedecay_source_body`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<SearchResultDisplayV1>,
    /// The lexical routes that ranked this candidate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lexical_routes: Option<Vec<SearchRouteMatchV1>>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchResultDisplayV1 {
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub path: String,
}

/// One route that ranked a candidate, named by its route label.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchRouteMatchV1 {
    pub route: String,
    pub score_micros: u64,
    pub matched_terms: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spelling_variants: Vec<SearchSpellingVariantV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchSpellingVariantV1 {
    pub query: String,
    pub alternative: String,
}

/// Why the lexical lane tried a configured alternative.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchLexicalAlternativeReasonV1 {
    ConfiguredVocabularyAlias,
}

/// One lexical route fused into the page, with its display label.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(tag = "route", rename_all = "snake_case", deny_unknown_fields)]
pub enum SearchLexicalRouteV1 {
    /// The caller's strict lexical query.
    Query { label: String },
    /// One caller-supplied exact identifier or term.
    Anchor { anchor: String, label: String },
    /// Identifier-shaped tokens of the query, restricted to symbol names.
    PreferredSymbol { tokens: Vec<String>, label: String },
    /// Identifier and path components recovered from the strict query.
    IdentifierSplit {
        strict_query: String,
        terms: Vec<String>,
        label: String,
    },
    /// A configured query-time vocabulary alternative.
    Alias {
        strict_query: String,
        alternative: String,
        reason: SearchLexicalAlternativeReasonV1,
        label: String,
    },
}

/// Advisory evidence that a sparse search matched external-module imports,
/// or why that evidence could not be read.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum SearchExternalImportHintV1 {
    Candidates(SearchExternalImportCandidatesV1),
    Unavailable(PrimitiveUnavailableEvidenceV1),
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchExternalImportCandidatesV1 {
    pub message: String,
    pub evidence: String,
    pub resolution_status: String,
    pub candidates: Vec<SearchExternalImportV1>,
    pub suggested_action: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchExternalImportV1 {
    pub module: String,
    /// The imported name; null for a whole-module import.
    pub symbol: Option<String>,
    pub import_file: String,
    pub line: u32,
}
