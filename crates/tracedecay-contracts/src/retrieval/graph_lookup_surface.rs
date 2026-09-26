//! Canonical CLI/MCP wire contracts for the graph lookup and content-search
//! tools the project's graph-tool owner answers: exact and qualified-name
//! symbol lookup, signatures, derives, and text and structural search.
//!
//! Presentation-only transport keys such as `format` and the registered
//! project selector are removed before these request bodies are decoded.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::result::{EvidenceCoverage, Omission};
use crate::retrieval::{NodeExpansionCostV1, PrimitiveSymbolLocationV1};

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FindExactSymbolSurfaceRequestV1 {
    /// Exact bare symbol name (no `::`, no glob).
    pub name: String,
    /// Maximum matches to return (default: 20, max: 200).
    pub limit: Option<u32>,
    /// Opt in to bounded indexing of ignored dependency entry files when an
    /// import hint matches (default: false).
    pub lazy_index_ignored_dependencies: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FindExactSymbolMatchV1 {
    pub id: String,
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    pub signature: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FindExactSymbolResultV1 {
    pub name: String,
    pub count: u64,
    pub matches: Vec<FindExactSymbolMatchV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ByQualifiedNameSurfaceRequestV1 {
    /// The exact qualified name to look up.
    pub qualified_name: String,
}

/// Every indexed symbol sharing one qualified name (overloads, generics,
/// separate impl blocks).
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ByQualifiedNameResultV1(pub Vec<PrimitiveSymbolLocationV1>);

/// Addresses indexed symbols by node id or by qualified name; the node id
/// wins when both are present.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SymbolSelectorSurfaceRequestV1 {
    /// Look up a single node by its ID instead of qualified_name.
    #[serde(alias = "id")]
    pub node_id: Option<String>,
    /// The exact qualified name (or short name) to look up.
    pub qualified_name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SymbolSignatureV1 {
    pub node_id: String,
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub visibility: String,
    pub signature: Option<String>,
    pub docstring: Option<String>,
    pub is_async: bool,
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
    pub cost_to_expand: NodeExpansionCostV1,
    pub unavailable_fields: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct SignatureResultV1(pub Vec<SymbolSignatureV1>);

/// How a derive name is known: read from the declaration's attributes, not
/// from macro expansion.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeriveEvidenceClassV1 {
    SyntaxExact,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeriveAnnotationV1 {
    pub name: String,
    pub evidence_class: DeriveEvidenceClassV1,
    pub unavailable_fields: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DerivesSymbolV1 {
    pub node_id: String,
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    pub derives: Vec<DeriveAnnotationV1>,
}

/// Derive annotations of every addressed symbol; empty when no symbol matched.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct DerivesResultV1(pub Vec<DerivesSymbolV1>);

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GrepSurfaceRequestV1 {
    /// Content to search for. Treated as a regular expression unless
    /// fixed_strings is true.
    pub pattern: String,
    /// Treat pattern as a literal string instead of a regex (default: false).
    pub fixed_strings: Option<bool>,
    /// Match case-sensitively (default: false = case-insensitive).
    pub case_sensitive: Option<bool>,
    /// Optional glob restricting which files are searched, matched against
    /// project-relative paths (e.g. 'src/**/*.rs').
    pub path_glob: Option<String>,
    /// Lines of surrounding context to include per hit (default: 0, max: 3).
    pub context_lines: Option<u32>,
    /// Maximum number of matching lines to return (default: 50, max: 200).
    pub max_results: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GrepMatchV1 {
    pub file: String,
    pub line: u32,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub before: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<String>,
    /// The smallest verified graph symbol enclosing the hit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
}

/// Whether the verified graph resolved each hit's enclosing symbol. Hits are
/// the lexical answer either way.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum GrepGraphEnrichmentV1 {
    Complete {
        enriched: u64,
        returned: u64,
    },
    Unavailable {
        reason_code: String,
        retryable: bool,
        detail: String,
    },
}

/// What the bounded scan skipped, by cause; absent when it skipped nothing.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GrepScanOmissionsV1 {
    /// Files larger than the interactive scan byte limit.
    pub oversized_files: u64,
    /// Lines longer than the scan line byte limit.
    pub oversized_lines: u64,
    /// Source candidates that could not be read during the scan.
    pub unavailable_sources: u64,
}

impl GrepScanOmissionsV1 {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GrepSearchResultV1 {
    pub results: Vec<GrepMatchV1>,
    pub match_count: u64,
    pub files_scanned: u64,
    pub truncated: bool,
    pub coverage: EvidenceCoverage,
    pub omissions: Vec<Omission>,
    #[serde(default, skip_serializing_if = "GrepScanOmissionsV1::is_empty")]
    pub scan_omissions: GrepScanOmissionsV1,
    pub graph_enrichment: GrepGraphEnrichmentV1,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AstGrepSearchSurfaceRequestV1 {
    /// ast-grep structural pattern (SGPattern syntax), e.g.
    /// 'reserve_stock($$$)' or 'Result<$T, $E>'.
    pub pattern: String,
    /// Optional language key to force (e.g. 'rust', 'typescript', 'python').
    /// Omit to auto-detect each file from its extension.
    pub lang: Option<String>,
    /// Optional glob restricting which files are searched, matched against
    /// project-relative paths (e.g. 'src/**/*.rs').
    pub path_glob: Option<String>,
    /// Maximum number of matches to return (default: 50, max: 200).
    pub max_results: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AstGrepSearchMatchV1 {
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub lang: String,
    #[serde(rename = "match")]
    pub matched_text: String,
    pub line_text: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AstGrepSearchResultV1 {
    pub results: Vec<AstGrepSearchMatchV1>,
    pub match_count: u64,
    pub files_scanned: u64,
    pub truncated: bool,
}
