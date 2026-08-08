use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    CodeGenerationId, CodeSearchChunkId, EphemeralSanitizedQueryViewV1, ExactTechnicalTermKindV1,
    FileOccurrenceId, QueryFallbackSubpayload, SourceSpan, SymbolOccurrenceId, UtcMicros,
};

use crate::error::ApplicationContractError;
use crate::handlers::ApplicationOperation;
use crate::result::OpaqueCursor;

use super::{ImplementationSelector, RetrievalRequestMeta};

pub const CALLABLE_CODE_OPERATION_COUNT: usize = 18;
pub const MAX_CALLABLE_CODE_QUERY_BYTES: usize = 4_096;
pub const MAX_CALLABLE_CODE_FILTERS: usize = 32;
pub const MAX_CALLABLE_CODE_DEPTH: u32 = 10;
pub const MAX_CALLABLE_CODE_FUZZY_EXPANSIONS: u32 = 64;
pub const MAX_SOURCE_METADATA_FILES: usize = 256;

/// One immutable code-index generation inside the authorized single root.
/// The path prefix narrows a query but never establishes project identity.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeQueryScope {
    pub generation: CodeGenerationId,
    pub path_prefix: Option<String>,
}

impl CodeQueryScope {
    pub fn new(
        generation: CodeGenerationId,
        path_prefix: Option<String>,
    ) -> Result<Self, ApplicationContractError> {
        let scope = Self {
            generation,
            path_prefix,
        };
        scope.validate()?;
        Ok(scope)
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.generation.validate()?;
        if let Some(path_prefix) = &self.path_prefix {
            validate_query(path_prefix, "code query path prefix")?;
            if path_prefix.starts_with('/') || path_prefix.split('/').any(|part| part == "..") {
                return Err(ApplicationContractError::Inconsistent {
                    field: "code query path prefix",
                });
            }
        }
        Ok(())
    }
}

/// Generation-bound page returned by every callable code query. Coverage,
/// omissions, scoring, and terminal state remain in the enclosing
/// [`crate::result::RetrievalEvidence`].
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CodeQueryPage<T> {
    pub generation: CodeGenerationId,
    pub items: Vec<T>,
    pub total: Option<u64>,
    /// Opaque resume token; its bounded string is the public wire form.
    #[schemars(with = "Option<String>")]
    pub next_cursor: Option<OpaqueCursor>,
    /// Independently hashed exact/lexical/graph subpayload. Callers preserve it
    /// byte-for-byte rather than interpreting it, so the public schema admits
    /// the canonical JSON it carries without re-declaring its internals.
    #[schemars(with = "Option<serde_json::Value>")]
    pub query_fallback: Option<QueryFallbackSubpayload>,
}

impl<T> CodeQueryPage<T> {
    pub fn new(
        generation: CodeGenerationId,
        items: Vec<T>,
        total: Option<u64>,
        next_cursor: Option<OpaqueCursor>,
        query_fallback: Option<QueryFallbackSubpayload>,
    ) -> Result<Self, ApplicationContractError> {
        let page = Self {
            generation,
            items,
            total,
            next_cursor,
            query_fallback,
        };
        page.validate()?;
        Ok(page)
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.generation.validate()?;
        if self
            .total
            .is_some_and(|total| total < self.items.len() as u64)
        {
            return Err(ApplicationContractError::InvalidRange {
                field: "code query page total",
            });
        }
        if let Some(fallback) = &self.query_fallback {
            fallback
                .validate()
                .map_err(|_| ApplicationContractError::Inconsistent {
                    field: "query fallback subpayload",
                })?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeOccurrenceRecord {
    pub file: FileOccurrenceId,
    pub symbol: Option<SymbolOccurrenceId>,
    pub chunk: Option<CodeSearchChunkId>,
    pub path: String,
    pub span: SourceSpan,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExactOccurrenceRecord {
    pub occurrence: CodeOccurrenceRecord,
    pub matched_kind: ExactTechnicalTermKindV1,
    pub matched_literal: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LexicalOccurrenceRecord {
    pub occurrence: CodeOccurrenceRecord,
    pub score_micros: u64,
    pub matched_phrases: Vec<String>,
    pub matched_terms: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceMetadataRecord {
    pub file: FileOccurrenceId,
    pub path: String,
    pub language: Option<String>,
    pub indexed_at: Option<UtcMicros>,
    pub byte_size: Option<u64>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodeFacetDimension {
    Kind,
    Language,
    Path,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeFacetRecord {
    pub dimension: CodeFacetDimension,
    pub value: String,
    pub count: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeTimelineRecord {
    pub generation: CodeGenerationId,
    pub indexed_at: UtcMicros,
    pub file_count: u64,
    pub symbol_count: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExactOccurrenceRequest {
    pub literal: String,
    pub kind: Option<ExactTechnicalTermKindV1>,
    pub scope: CodeQueryScope,
    pub meta: RetrievalRequestMeta,
}

impl ExactOccurrenceRequest {
    pub fn new(
        literal: impl Into<String>,
        kind: Option<ExactTechnicalTermKindV1>,
        scope: CodeQueryScope,
        meta: RetrievalRequestMeta,
    ) -> Result<Self, ApplicationContractError> {
        let request = Self {
            literal: literal.into(),
            kind,
            scope,
            meta,
        };
        request.validate()?;
        Ok(request)
    }
}

#[derive(Debug)]
pub struct PhraseSearchRequest {
    pub query: EphemeralSanitizedQueryViewV1,
    pub phrases: Vec<String>,
    pub field_filters: Vec<CodeLexicalFieldFilter>,
    pub fuzzy_budget: u32,
    pub scope: CodeQueryScope,
    pub meta: RetrievalRequestMeta,
}

impl PhraseSearchRequest {
    pub fn new(
        query: EphemeralSanitizedQueryViewV1,
        phrases: Vec<String>,
        field_filters: Vec<CodeLexicalFieldFilter>,
        fuzzy_budget: u32,
        scope: CodeQueryScope,
        meta: RetrievalRequestMeta,
    ) -> Result<Self, ApplicationContractError> {
        let request = Self {
            query,
            phrases,
            field_filters,
            fuzzy_budget,
            scope,
            meta,
        };
        request.validate()?;
        Ok(request)
    }
}

/// Public wire form of [`PhraseSearchRequest`].
///
/// [`PhraseSearchRequest::query`] holds a receipt-bound
/// [`EphemeralSanitizedQueryViewV1`], which is deliberately non-serializable so
/// a sanitized view can never be reconstructed from a transport payload. The
/// admitted wire request therefore carries the raw query text and the daemon
/// sanitizes it; every other field is the same bounded value the service
/// validates.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PhraseSearchSurfaceRequest {
    pub query: String,
    pub phrases: Vec<String>,
    pub field_filters: Vec<CodeLexicalFieldFilter>,
    pub fuzzy_budget: u32,
    pub scope: CodeQueryScope,
    pub meta: RetrievalRequestMeta,
}

/// Typed code fields accepted by the generation-owned lexical authority.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum CodeLexicalField {
    SymbolName,
    QualifiedName,
    Path,
    BodyText,
    PreambleText,
    ExactTerm,
    Subtoken,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeLexicalFieldFilter {
    pub field: CodeLexicalField,
    pub include: bool,
}

#[derive(Debug)]
pub struct CodeSymbolSearchRequest {
    pub query: EphemeralSanitizedQueryViewV1,
    pub scope: CodeQueryScope,
    pub meta: RetrievalRequestMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QualifiedNameRequest {
    pub qualified_name: String,
    pub scope: CodeQueryScope,
    pub meta: RetrievalRequestMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeSignatureRequest {
    pub returns: Option<String>,
    pub params: Vec<String>,
    pub is_async: Option<bool>,
    pub scope: CodeQueryScope,
    pub meta: RetrievalRequestMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeImplementationsRequest {
    pub selector: ImplementationSelector,
    pub scope: CodeQueryScope,
    pub meta: RetrievalRequestMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeHierarchyRequest {
    pub node_id: String,
    pub maximum_depth: u32,
    pub scope: CodeQueryScope,
    pub meta: RetrievalRequestMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeRelationRequest {
    pub node_id: String,
    pub maximum_depth: u32,
    pub resolve_trait_dispatch: bool,
    pub scope: CodeQueryScope,
    pub meta: RetrievalRequestMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeImpactRequest {
    pub node_id: String,
    pub maximum_depth: u32,
    pub scope: CodeQueryScope,
    pub meta: RetrievalRequestMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ModuleApiRequest {
    pub path: String,
    pub scope: CodeQueryScope,
    pub meta: RetrievalRequestMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceMetadataRequest {
    pub files: Vec<FileOccurrenceId>,
    pub scope: CodeQueryScope,
    pub meta: RetrievalRequestMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeFacetRequest {
    pub dimension: CodeFacetDimension,
    pub scope: CodeQueryScope,
    pub meta: RetrievalRequestMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeTimelineRequest {
    pub scope: CodeQueryScope,
    pub meta: RetrievalRequestMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeNavigationRequest {
    pub node_id: String,
    pub scope: CodeQueryScope,
    pub meta: RetrievalRequestMeta,
}

impl SourceMetadataRequest {
    pub fn new(
        files: Vec<FileOccurrenceId>,
        scope: CodeQueryScope,
        meta: RetrievalRequestMeta,
    ) -> Result<Self, ApplicationContractError> {
        let request = Self { files, scope, meta };
        request.validate()?;
        Ok(request)
    }
}

pub(super) trait ValidatedCodeQueryRequest {
    fn validate(&self) -> Result<(), ApplicationContractError>;
}

impl ValidatedCodeQueryRequest for ExactOccurrenceRequest {
    fn validate(&self) -> Result<(), ApplicationContractError> {
        validate_query(&self.literal, "exact occurrence literal")?;
        validate_scope_meta(&self.scope, &self.meta)
    }
}

impl ValidatedCodeQueryRequest for PhraseSearchRequest {
    fn validate(&self) -> Result<(), ApplicationContractError> {
        validate_query(self.query.as_str(), "phrase search query")?;
        validate_filters(&self.phrases, "phrase search phrases")?;
        if self.field_filters.len() > MAX_CALLABLE_CODE_FILTERS {
            return Err(ApplicationContractError::InvalidRange {
                field: "phrase search field filters",
            });
        }
        let mut fields = std::collections::BTreeSet::new();
        if self
            .field_filters
            .iter()
            .any(|filter| !fields.insert(filter.field))
        {
            return Err(ApplicationContractError::Duplicate {
                field: "phrase search field filter",
            });
        }
        if self.fuzzy_budget > MAX_CALLABLE_CODE_FUZZY_EXPANSIONS {
            return Err(ApplicationContractError::InvalidRange {
                field: "phrase search fuzzy budget",
            });
        }
        validate_scope_meta(&self.scope, &self.meta)
    }
}

impl ValidatedCodeQueryRequest for CodeSymbolSearchRequest {
    fn validate(&self) -> Result<(), ApplicationContractError> {
        validate_query(self.query.as_str(), "code symbol query")?;
        validate_scope_meta(&self.scope, &self.meta)
    }
}

impl ValidatedCodeQueryRequest for QualifiedNameRequest {
    fn validate(&self) -> Result<(), ApplicationContractError> {
        validate_query(&self.qualified_name, "qualified name")?;
        validate_scope_meta(&self.scope, &self.meta)
    }
}

impl ValidatedCodeQueryRequest for CodeSignatureRequest {
    fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.returns.is_none() && self.params.is_empty() && self.is_async.is_none() {
            return Err(ApplicationContractError::Inconsistent {
                field: "code signature filters",
            });
        }
        if let Some(returns) = &self.returns {
            validate_query(returns, "code signature return filter")?;
        }
        if !self.params.is_empty() {
            validate_filters(&self.params, "code signature parameter filters")?;
        }
        validate_scope_meta(&self.scope, &self.meta)
    }
}

impl ValidatedCodeQueryRequest for CodeImplementationsRequest {
    fn validate(&self) -> Result<(), ApplicationContractError> {
        match &self.selector {
            ImplementationSelector::Trait { name } => {
                validate_query(name, "implementation trait name")?
            }
            ImplementationSelector::Method { name } => {
                validate_query(name, "implementation method name")?
            }
        }
        validate_scope_meta(&self.scope, &self.meta)
    }
}

impl ValidatedCodeQueryRequest for CodeHierarchyRequest {
    fn validate(&self) -> Result<(), ApplicationContractError> {
        validate_node_depth(&self.node_id, self.maximum_depth)?;
        validate_scope_meta(&self.scope, &self.meta)
    }
}

impl ValidatedCodeQueryRequest for CodeRelationRequest {
    fn validate(&self) -> Result<(), ApplicationContractError> {
        validate_node_depth(&self.node_id, self.maximum_depth)?;
        validate_scope_meta(&self.scope, &self.meta)
    }
}

impl ValidatedCodeQueryRequest for CodeImpactRequest {
    fn validate(&self) -> Result<(), ApplicationContractError> {
        validate_node_depth(&self.node_id, self.maximum_depth)?;
        validate_scope_meta(&self.scope, &self.meta)
    }
}

impl ValidatedCodeQueryRequest for ModuleApiRequest {
    fn validate(&self) -> Result<(), ApplicationContractError> {
        validate_query(&self.path, "module API path")?;
        if self.path.starts_with('/') || self.path.split('/').any(|part| part == "..") {
            return Err(ApplicationContractError::Inconsistent {
                field: "module API path",
            });
        }
        validate_scope_meta(&self.scope, &self.meta)
    }
}

impl ValidatedCodeQueryRequest for SourceMetadataRequest {
    fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.files.is_empty() || self.files.len() > MAX_SOURCE_METADATA_FILES {
            return Err(ApplicationContractError::InvalidRange {
                field: "source metadata files",
            });
        }
        for file in &self.files {
            file.validate()?;
        }
        validate_scope_meta(&self.scope, &self.meta)
    }
}

impl ValidatedCodeQueryRequest for CodeFacetRequest {
    fn validate(&self) -> Result<(), ApplicationContractError> {
        validate_scope_meta(&self.scope, &self.meta)
    }
}

impl ValidatedCodeQueryRequest for CodeTimelineRequest {
    fn validate(&self) -> Result<(), ApplicationContractError> {
        validate_scope_meta(&self.scope, &self.meta)
    }
}

impl ValidatedCodeQueryRequest for CodeNavigationRequest {
    fn validate(&self) -> Result<(), ApplicationContractError> {
        validate_query(&self.node_id, "code navigation node id")?;
        validate_scope_meta(&self.scope, &self.meta)
    }
}

fn validate_scope_meta(
    scope: &CodeQueryScope,
    meta: &RetrievalRequestMeta,
) -> Result<(), ApplicationContractError> {
    scope.validate()?;
    if meta.temporal != tracedecay_domain::TemporalModeV1::Current {
        return Err(ApplicationContractError::Inconsistent {
            field: "code query temporal mode",
        });
    }
    super::PageRequest::new(meta.page.page_size, meta.page.cursor.clone()).map(|_| ())
}

fn validate_filters(
    filters: &[String],
    field: &'static str,
) -> Result<(), ApplicationContractError> {
    if filters.is_empty() || filters.len() > MAX_CALLABLE_CODE_FILTERS {
        return Err(ApplicationContractError::InvalidRange { field });
    }
    for filter in filters {
        validate_query(filter, field)?;
    }
    Ok(())
}

fn validate_node_depth(node_id: &str, maximum_depth: u32) -> Result<(), ApplicationContractError> {
    validate_query(node_id, "code graph node id")?;
    if maximum_depth == 0 || maximum_depth > MAX_CALLABLE_CODE_DEPTH {
        return Err(ApplicationContractError::InvalidRange {
            field: "code graph maximum depth",
        });
    }
    Ok(())
}

fn validate_query(value: &str, field: &'static str) -> Result<(), ApplicationContractError> {
    validate_text(value, field)?;
    if value.len() > MAX_CALLABLE_CODE_QUERY_BYTES {
        return Err(ApplicationContractError::InvalidRange { field });
    }
    Ok(())
}

fn validate_text(value: &str, field: &'static str) -> Result<(), ApplicationContractError> {
    if value.is_empty() || value.trim() != value || value.chars().any(char::is_control) {
        return Err(ApplicationContractError::InvalidIdentifier { field });
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CallableCodeOperationKind {
    ExactOccurrence,
    PhraseSearch,
    SymbolSearch,
    QualifiedName,
    SignatureSearch,
    Implementations,
    TypeHierarchy,
    Callers,
    Callees,
    Impact,
    ModuleApi,
    SourceMetadata,
    Facets,
    Timeline,
    Declaration,
    Definition,
    TypeDefinition,
    References,
}

impl CallableCodeOperationKind {
    pub const ALL: [Self; CALLABLE_CODE_OPERATION_COUNT] = [
        Self::ExactOccurrence,
        Self::PhraseSearch,
        Self::SymbolSearch,
        Self::QualifiedName,
        Self::SignatureSearch,
        Self::Implementations,
        Self::TypeHierarchy,
        Self::Callers,
        Self::Callees,
        Self::Impact,
        Self::ModuleApi,
        Self::SourceMetadata,
        Self::Facets,
        Self::Timeline,
        Self::Declaration,
        Self::Definition,
        Self::TypeDefinition,
        Self::References,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExactOccurrence => "exact_occurrence",
            Self::PhraseSearch => "phrase_search",
            Self::SymbolSearch => "symbol_search",
            Self::QualifiedName => "qualified_name",
            Self::SignatureSearch => "signature_search",
            Self::Implementations => "implementations",
            Self::TypeHierarchy => "type_hierarchy",
            Self::Callers => "callers",
            Self::Callees => "callees",
            Self::Impact => "impact",
            Self::ModuleApi => "module_api",
            Self::SourceMetadata => "source_metadata",
            Self::Facets => "facets",
            Self::Timeline => "timeline",
            Self::Declaration => "declaration",
            Self::Definition => "definition",
            Self::TypeDefinition => "type_definition",
            Self::References => "references",
        }
    }
}

#[derive(Clone, Debug)]
pub struct CallableCodeOperations {
    operations: BTreeMap<CallableCodeOperationKind, ApplicationOperation>,
}

impl CallableCodeOperations {
    pub fn new(
        operations: impl IntoIterator<Item = (CallableCodeOperationKind, ApplicationOperation)>,
    ) -> Result<Self, ApplicationContractError> {
        let mut indexed = BTreeMap::new();
        for (kind, operation) in operations {
            if indexed.insert(kind, operation).is_some() {
                return Err(ApplicationContractError::Duplicate {
                    field: "callable code operation",
                });
            }
        }
        if CallableCodeOperationKind::ALL
            .iter()
            .any(|kind| !indexed.contains_key(kind))
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "callable code operation set",
            });
        }
        Ok(Self {
            operations: indexed,
        })
    }

    pub fn get(&self, kind: CallableCodeOperationKind) -> &ApplicationOperation {
        self.operations
            .get(&kind)
            .expect("validated callable code operation set is complete")
    }

    pub fn iter(&self) -> impl Iterator<Item = (CallableCodeOperationKind, &ApplicationOperation)> {
        CallableCodeOperationKind::ALL
            .into_iter()
            .map(|kind| (kind, self.get(kind)))
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;

    use serde::Serialize;
    use serde::de::DeserializeOwned;

    use super::*;
    use crate::retrieval::{SymbolPrimitiveRecord, SymbolRelationRecord};

    fn assert_typed_json_roundtrip<T>(json: &str)
    where
        T: DeserializeOwned + Serialize + PartialEq + Debug,
    {
        let decoded: T = serde_json::from_str(json).expect("fixture deserializes into its DTO");
        let rendered = serde_json::to_string(&decoded).expect("DTO serializes to JSON");
        let reparsed: T =
            serde_json::from_str(&rendered).expect("serialized DTO deserializes without a Value");

        assert_eq!(reparsed, decoded);
    }

    #[test]
    fn code_query_result_dtos_round_trip_through_typed_json() {
        assert_typed_json_roundtrip::<SymbolPrimitiveRecord>(
            r#"{
                "node_id": "node.fixture",
                "name": "work",
                "qualified_name": "crate::worker::work",
                "kind": "function",
                "file": "src/worker.rs",
                "start_line_zero_based": 4,
                "end_line_zero_based": 8,
                "line": 5,
                "end_line": 9,
                "signature": "fn work()",
                "is_async": false,
                "score": 875000
            }"#,
        );
        assert_typed_json_roundtrip::<SymbolRelationRecord>(
            r#"{
                "symbol": {
                    "node_id": "node.fixture",
                    "name": "work",
                    "qualified_name": "crate::worker::work",
                    "kind": "function",
                    "file": "src/worker.rs",
                    "start_line_zero_based": 4,
                    "end_line_zero_based": 8,
                    "line": 5,
                    "end_line": 9,
                    "signature": null,
                    "is_async": false,
                    "score": null
                },
                "edge_kind": "calls",
                "dispatch_via_trait": false,
                "dispatch_from": null,
                "depth": 1
            }"#,
        );
        assert_typed_json_roundtrip::<ExactOccurrenceRecord>(
            r#"{
                "occurrence": {
                    "file": "file.fixture",
                    "symbol": "symbol.fixture",
                    "chunk": "chunk.fixture",
                    "path": "src/worker.rs",
                    "span": { "start_byte": 12, "end_byte": 16 }
                },
                "matched_kind": "whole_symbol",
                "matched_literal": "work"
            }"#,
        );
        assert_typed_json_roundtrip::<CodeFacetRecord>(
            r#"{ "dimension": "language", "value": "rust", "count": 3 }"#,
        );
        assert_typed_json_roundtrip::<LexicalOccurrenceRecord>(
            r#"{
                "occurrence": {
                    "file": "file.fixture",
                    "symbol": null,
                    "chunk": "chunk.fixture",
                    "path": "src/worker.rs",
                    "span": { "start_byte": 12, "end_byte": 16 }
                },
                "score_micros": 875000,
                "matched_phrases": ["worker"],
                "matched_terms": ["work"]
            }"#,
        );
        assert_typed_json_roundtrip::<CodeTimelineRecord>(
            r#"{
                "generation": "generation.fixture",
                "indexed_at": 1720000000000000,
                "file_count": 3,
                "symbol_count": 7
            }"#,
        );
        assert_typed_json_roundtrip::<CodeQueryPage<SymbolRelationRecord>>(
            r#"{
                "generation": "generation.fixture",
                "items": [{
                    "symbol": {
                        "node_id": "node.fixture",
                        "name": "work",
                        "qualified_name": "crate::worker::work",
                        "kind": "function",
                        "file": "src/worker.rs",
                        "start_line_zero_based": 4,
                        "end_line_zero_based": 8,
                        "line": 5,
                        "end_line": 9,
                        "signature": null,
                        "is_async": false,
                        "score": null
                    },
                    "edge_kind": "calls",
                    "dispatch_via_trait": false,
                    "dispatch_from": null,
                    "depth": 1
                }],
                "total": 1,
                "next_cursor": "cursor.fixture.page-2",
                "query_fallback": null
            }"#,
        );
    }

    #[test]
    fn code_query_result_dtos_reject_unknown_json_fields() {
        assert!(
            serde_json::from_str::<CodeQueryPage<SymbolRelationRecord>>(
                r#"{
                "generation": "generation.fixture",
                "items": [],
                "total": 0,
                "next_cursor": null,
                "query_fallback": null,
                "unexpected": true
            }"#,
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<ExactOccurrenceRecord>(
                r#"{
                "occurrence": {
                    "file": "file.fixture",
                    "symbol": null,
                    "chunk": null,
                    "path": "src/worker.rs",
                    "span": { "start_byte": 12, "end_byte": 16 }
                },
                "matched_kind": "whole_symbol",
                "matched_literal": "work",
                "unexpected": true
            }"#,
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<SymbolPrimitiveRecord>(
                r#"{
                "node_id": "node.fixture",
                "name": "work",
                "qualified_name": "crate::worker::work",
                "kind": "function",
                "file": "src/worker.rs",
                "start_line_zero_based": 4,
                "end_line_zero_based": 8,
                "line": 5,
                "end_line": 9,
                "signature": null,
                "is_async": false,
                "score": null,
                "unexpected": true
            }"#,
            )
            .is_err()
        );
    }
}
