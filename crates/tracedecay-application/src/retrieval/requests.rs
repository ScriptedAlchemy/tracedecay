use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    CodeGenerationId, EphemeralSanitizedQueryViewV1, FileOccurrenceId, ManifestDigest,
    QueryFallbackSubpayload, RetrievalAnchorId, SessionId, SourceSpan, SymbolOccurrenceId,
    TemporalModeV1, TestAttributionEvidenceClassV1, UtcMicros,
};

use crate::error::ApplicationContractError;
use crate::result::OpaqueCursor;

pub const MAX_APPLICATION_PAGE_SIZE: u32 = 1_000;

/// Bounded opaque page request. Resume authorization occurs before an adapter
/// decodes or hydrates the cursor.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PageRequest {
    pub page_size: u32,
    /// Opaque resume token. The identifier type is deliberately absent from the
    /// generated schema surface, so the public wire form is its bounded string.
    #[schemars(with = "Option<String>")]
    pub cursor: Option<OpaqueCursor>,
}

impl PageRequest {
    pub fn first(page_size: u32) -> Result<Self, ApplicationContractError> {
        Self::new(page_size, None)
    }

    pub fn new(
        page_size: u32,
        cursor: Option<OpaqueCursor>,
    ) -> Result<Self, ApplicationContractError> {
        if page_size == 0 || page_size > MAX_APPLICATION_PAGE_SIZE {
            return Err(ApplicationContractError::InvalidRange {
                field: "retrieval page size",
            });
        }
        Ok(Self { page_size, cursor })
    }
}

/// Bounded output projection chosen by a concrete use case.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum ResultProjection {
    Summary,
    Evidence,
    ReferencesOnly,
}

/// Stable semantic ordering; adapters may not replace it with transport order.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalOrder {
    Relevance,
    SourcePosition,
    TemporalDescending,
    StableIdentity,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrievalRequestMeta {
    pub temporal: TemporalModeV1,
    pub page: PageRequest,
    pub projection: ResultProjection,
    pub order: RetrievalOrder,
}

impl RetrievalRequestMeta {
    pub fn current(page: PageRequest, projection: ResultProjection, order: RetrievalOrder) -> Self {
        Self {
            temporal: TemporalModeV1::Current,
            page,
            projection,
            order,
        }
    }
}

/// Concrete QUERY-backed symbol retrieval request. Its query view is
/// receipt/sanitization-bound and intentionally non-serializable.
#[derive(Debug)]
pub struct SymbolSearchRequest {
    pub query: EphemeralSanitizedQueryViewV1,
    pub meta: RetrievalRequestMeta,
}

impl SymbolSearchRequest {
    pub fn new(
        query: EphemeralSanitizedQueryViewV1,
        page: PageRequest,
        projection: ResultProjection,
        order: RetrievalOrder,
    ) -> Result<Self, ApplicationContractError> {
        Ok(Self {
            query,
            meta: RetrievalRequestMeta::current(page, projection, order),
        })
    }
}

/// The application-facing query fallback boundary. The exact/lexical/graph
/// subpayload is preserved byte-for-byte by the owning query lane.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SymbolSearchResult {
    pub query_fallback: QueryFallbackSubpayload,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceLinesRequest {
    pub file: FileOccurrenceId,
    pub span: SourceSpan,
    pub meta: RetrievalRequestMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceReference {
    pub anchor: RetrievalAnchorId,
    pub span: SourceSpan,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceLinesResult {
    pub references: Vec<SourceReference>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphCallersRequest {
    pub symbol: SymbolOccurrenceId,
    pub maximum_depth: u32,
    pub meta: RetrievalRequestMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphCallersResult {
    pub callers: Vec<SymbolOccurrenceId>,
}

/// Plan-05 graph-kernel input for one exact feedback target. The feedback
/// layer only translates its typed address; graph traversal remains owned by
/// the retrieval implementation behind this request.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphImpactRequest {
    pub file: FileOccurrenceId,
    pub symbol: SymbolOccurrenceId,
    pub generation: CodeGenerationId,
    pub meta: RetrievalRequestMeta,
}

/// Reference-only graph impact returned by the Plan-05 query kernel. The
/// kernel supplies canonical occurrence and anchor identities; adapters never
/// reconstruct the graph from source text or edge tables.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphImpactResult {
    pub affected_files: Vec<FileOccurrenceId>,
    pub affected_callers: Vec<SymbolOccurrenceId>,
    pub evidence_anchors: Vec<RetrievalAnchorId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AffectedTestsRequest {
    pub symbol: SymbolOccurrenceId,
    pub generation: CodeGenerationId,
    pub meta: RetrievalRequestMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AffectedTestAttributionV1 {
    pub test: SymbolOccurrenceId,
    pub evidence_class: TestAttributionEvidenceClassV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AffectedTestsResult {
    pub tests: Vec<SymbolOccurrenceId>,
    /// Exact class reported by the generation-bound attribution authority.
    /// `tests` remains the compatibility projection of current candidates.
    #[serde(default)]
    pub attributions: Vec<AffectedTestAttributionV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionLookupRequest {
    pub session_id: SessionId,
    pub meta: RetrievalRequestMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionLookupResult {
    pub anchors: Vec<RetrievalAnchorId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AnchorExpandRequest {
    pub anchor: RetrievalAnchorId,
    pub meta: RetrievalRequestMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AnchorExpandResult {
    pub anchors: Vec<RetrievalAnchorId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HealthReadRequest {
    pub meta: RetrievalRequestMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HealthReadResult {
    pub status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HealthDeltaRequest {
    pub before_cursor: Option<String>,
    pub path_prefix: Option<String>,
    pub meta: RetrievalRequestMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HealthDeltaScopeV1 {
    pub project_id: Option<String>,
    pub scope_digest: ManifestDigest,
    pub path_prefix: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HealthDimensionPointV1 {
    pub score_ppm: u64,
    pub denominator: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HealthDeltaPointV1 {
    pub watermark: ManifestDigest,
    pub observed_at: UtcMicros,
    pub quality_signal: u32,
    pub files_analyzed: u64,
    pub function_denominator: u64,
    pub dimensions: BTreeMap<String, HealthDimensionPointV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HealthDimensionDeltaV1 {
    pub before_ppm: u64,
    pub after_ppm: u64,
    pub delta_ppm: i64,
    pub before_denominator: Option<u64>,
    pub after_denominator: Option<u64>,
    pub status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HealthDeltaCoverageV1 {
    pub eligible: Option<u64>,
    pub visited: Option<u64>,
    pub denominator: Option<u64>,
    pub completeness: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HealthDeltaCurrentnessV1 {
    pub state: String,
    pub observed_at: UtcMicros,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HealthDeltaResult {
    pub schema_version: u32,
    pub scope: HealthDeltaScopeV1,
    pub before: HealthDeltaPointV1,
    pub after: HealthDeltaPointV1,
    pub before_cursor: String,
    pub after_cursor: String,
    pub pass: bool,
    pub delta: i64,
    pub dimensions: BTreeMap<String, HealthDimensionDeltaV1>,
    pub coverage: HealthDeltaCoverageV1,
    pub currentness: HealthDeltaCurrentnessV1,
}
