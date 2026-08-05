use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{ExactTechnicalTermKindV1, QueryNormalizationRevision, SanitizerRevision};

use crate::error::ApplicationContractError;
use crate::result::OpaqueCursor;

use super::callable_code::{
    CodeFacetDimension, CodeFacetRequest, CodeLexicalFieldFilter, CodeNavigationRequest,
    CodeQueryScope, CodeRelationRequest, CodeTimelineRequest, ExactOccurrenceRequest,
    PhraseSearchRequest,
};
use super::requests::{PageRequest, ResultProjection, RetrievalOrder, RetrievalRequestMeta};
use super::symbol_graph::{
    GraphRelationRequest, ImplementationSelector, ImplementationsRequest, SignatureSearchRequest,
    SymbolGraphScope, SymbolSearchPrimitiveRequest, TypeHierarchyRequest,
};

/// Surface-owned query semantics. Page size remains an invocation control, but
/// continuation is a request field so CLI, MCP, and HTTP callers all have the
/// same channel for spending a `next_cursor`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CallableCodeSurfaceMeta {
    pub projection: ResultProjection,
    pub order: RetrievalOrder,
    #[serde(default)]
    pub cursor: Option<OpaqueCursor>,
}

impl CallableCodeSurfaceMeta {
    pub fn into_application(self, page: PageRequest) -> RetrievalRequestMeta {
        let Self {
            projection,
            order,
            cursor,
        } = self;
        let page = match cursor {
            Some(cursor) => PageRequest {
                page_size: page.page_size,
                cursor: Some(cursor),
            },
            None => page,
        };
        RetrievalRequestMeta::current(page, projection, order)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeExactOccurrenceSurfaceRequest {
    pub literal: String,
    pub kind: Option<ExactTechnicalTermKindV1>,
    pub scope: CodeQueryScope,
    pub meta: CallableCodeSurfaceMeta,
}

impl CodeExactOccurrenceSurfaceRequest {
    pub fn into_application_request(
        self,
        page: PageRequest,
    ) -> Result<ExactOccurrenceRequest, ApplicationContractError> {
        ExactOccurrenceRequest::new(
            self.literal,
            self.kind,
            self.scope,
            self.meta.into_application(page),
        )
    }
}

/// Serializable ingress for the request-local phrase query view.
///
/// The callable application request deliberately keeps its sanitized query
/// non-serializable. The owning runtime supplies the exact sanitizer
/// revisions when converting this wire value.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodePhraseSearchSurfaceRequest {
    pub query: String,
    pub phrases: Vec<String>,
    #[serde(default)]
    pub field_filters: Vec<CodeLexicalFieldFilter>,
    #[serde(default)]
    pub fuzzy_budget: u32,
    pub scope: CodeQueryScope,
    pub meta: CallableCodeSurfaceMeta,
}

impl CodePhraseSearchSurfaceRequest {
    pub fn into_application_request(
        self,
        sanitizer_revision: SanitizerRevision,
        normalization_revision: QueryNormalizationRevision,
        page: PageRequest,
    ) -> Result<PhraseSearchRequest, ApplicationContractError> {
        let query = tracedecay_domain::EphemeralSanitizedQueryViewV1::sanitize(
            self.query,
            sanitizer_revision,
            normalization_revision,
        )?;
        PhraseSearchRequest::new(
            query,
            self.phrases,
            self.field_filters,
            self.fuzzy_budget,
            self.scope,
            self.meta.into_application(page),
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeSymbolSearchSurfaceRequest {
    pub query: String,
    pub scope: SymbolGraphScope,
    pub lazy_index_ignored_dependencies: bool,
    pub meta: CallableCodeSurfaceMeta,
}

impl CodeSymbolSearchSurfaceRequest {
    pub fn into_primitive_request(
        self,
        sanitizer_revision: SanitizerRevision,
        normalization_revision: QueryNormalizationRevision,
        page: PageRequest,
    ) -> Result<SymbolSearchPrimitiveRequest, ApplicationContractError> {
        let query = tracedecay_domain::EphemeralSanitizedQueryViewV1::sanitize(
            self.query,
            sanitizer_revision,
            normalization_revision,
        )?;
        Ok(SymbolSearchPrimitiveRequest {
            query,
            scope: self.scope,
            lazy_index_ignored_dependencies: self.lazy_index_ignored_dependencies,
            meta: self.meta.into_application(page),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeSignatureSearchSurfaceRequest {
    pub returns: Option<String>,
    pub params: Vec<String>,
    pub is_async: Option<bool>,
    pub scope: SymbolGraphScope,
    pub meta: CallableCodeSurfaceMeta,
}

impl CodeSignatureSearchSurfaceRequest {
    pub fn into_application_request(self, page: PageRequest) -> SignatureSearchRequest {
        SignatureSearchRequest {
            returns: self.returns,
            params: self.params,
            is_async: self.is_async,
            scope: self.scope,
            meta: self.meta.into_application(page),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeImplementationsSurfaceRequest {
    pub selector: ImplementationSelector,
    pub scope: SymbolGraphScope,
    pub meta: CallableCodeSurfaceMeta,
}

impl CodeImplementationsSurfaceRequest {
    pub fn into_application_request(self, page: PageRequest) -> ImplementationsRequest {
        ImplementationsRequest {
            selector: self.selector,
            scope: self.scope,
            meta: self.meta.into_application(page),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeTypeHierarchySurfaceRequest {
    pub node_id: String,
    pub maximum_depth: u32,
    pub scope: SymbolGraphScope,
    pub meta: CallableCodeSurfaceMeta,
}

impl CodeTypeHierarchySurfaceRequest {
    pub fn into_application_request(self, page: PageRequest) -> TypeHierarchyRequest {
        TypeHierarchyRequest {
            node_id: self.node_id,
            maximum_depth: self.maximum_depth,
            scope: self.scope,
            meta: self.meta.into_application(page),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeCallersSurfaceRequest {
    pub node_id: String,
    pub maximum_depth: u32,
    pub resolve_trait_dispatch: bool,
    pub scope: SymbolGraphScope,
    pub meta: CallableCodeSurfaceMeta,
}

impl CodeCallersSurfaceRequest {
    pub fn into_application_request(self, page: PageRequest) -> GraphRelationRequest {
        GraphRelationRequest {
            node_id: self.node_id,
            maximum_depth: self.maximum_depth,
            resolve_trait_dispatch: self.resolve_trait_dispatch,
            scope: self.scope,
            meta: self.meta.into_application(page),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeCalleesSurfaceRequest {
    pub node_id: String,
    pub maximum_depth: u32,
    pub resolve_trait_dispatch: bool,
    pub scope: CodeQueryScope,
    pub meta: CallableCodeSurfaceMeta,
}

impl CodeCalleesSurfaceRequest {
    pub fn into_application_request(self, page: PageRequest) -> CodeRelationRequest {
        CodeRelationRequest {
            node_id: self.node_id,
            maximum_depth: self.maximum_depth,
            resolve_trait_dispatch: self.resolve_trait_dispatch,
            scope: self.scope,
            meta: self.meta.into_application(page),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeFacetSurfaceRequest {
    pub dimension: CodeFacetDimension,
    pub scope: CodeQueryScope,
    pub meta: CallableCodeSurfaceMeta,
}

impl CodeFacetSurfaceRequest {
    pub fn into_application_request(self, page: PageRequest) -> CodeFacetRequest {
        CodeFacetRequest {
            dimension: self.dimension,
            scope: self.scope,
            meta: self.meta.into_application(page),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeTimelineSurfaceRequest {
    pub scope: CodeQueryScope,
    pub meta: CallableCodeSurfaceMeta,
}

impl CodeTimelineSurfaceRequest {
    pub fn into_application_request(self, page: PageRequest) -> CodeTimelineRequest {
        CodeTimelineRequest {
            scope: self.scope,
            meta: self.meta.into_application(page),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeNavigationSurfaceRequest {
    pub node_id: String,
    pub scope: CodeQueryScope,
    pub meta: CallableCodeSurfaceMeta,
}

impl CodeNavigationSurfaceRequest {
    pub fn into_application_request(self, page: PageRequest) -> CodeNavigationRequest {
        CodeNavigationRequest {
            node_id: self.node_id,
            scope: self.scope,
            meta: self.meta.into_application(page),
        }
    }
}
