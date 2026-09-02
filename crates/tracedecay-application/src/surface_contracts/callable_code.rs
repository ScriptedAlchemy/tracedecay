//! Transport-neutral callable-code and primitive-code surface request DTOs.

use serde::{Deserialize, Serialize};
use tracedecay_domain::{ExactTechnicalTermKindV1, QueryNormalizationRevision, SanitizerRevision};

use crate::error::ApplicationContractError;
use crate::result::OpaqueCursor;
use crate::retrieval::{
    CodeFacetDimension, CodeFacetRequest, CodeLexicalFieldFilter, CodeNavigationRequest,
    CodeQueryScope, CodeRelationRequest, CodeTimelineRequest, ExactOccurrenceRequest,
    GraphRelationRequest, ImplementationSelector, ImplementationsRequest, PageRequest,
    PhraseSearchRequest, PrimitiveRequest, ResultProjection, RetrievalOrder, RetrievalRequestMeta,
    SignatureSearchRequest, SymbolGraphScope, TypeHierarchyRequest,
};

/// Surface-owned query semantics. Page size remains an invocation control, but
/// continuation is a request field so CLI, MCP, and HTTP callers all have the
/// same channel for spending a `next_cursor`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
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

/// Serializable adapter DTO for the request-local phrase query view.
///
/// The callable application request deliberately keeps its sanitized query
/// non-serializable. The owning runtime supplies the exact sanitizer
/// revisions when converting this transport DTO.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeSymbolSearchSurfaceRequest {
    pub query: String,
    pub scope: SymbolGraphScope,
    pub lazy_index_ignored_dependencies: bool,
    pub meta: CallableCodeSurfaceMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeSignatureSearchSurfaceRequest {
    pub returns: Option<String>,
    pub params: Vec<String>,
    pub is_async: Option<bool>,
    pub scope: SymbolGraphScope,
    pub meta: CallableCodeSurfaceMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeImplementationsSurfaceRequest {
    pub selector: ImplementationSelector,
    pub scope: SymbolGraphScope,
    pub meta: CallableCodeSurfaceMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeTypeHierarchySurfaceRequest {
    pub node_id: String,
    pub maximum_depth: u32,
    pub scope: SymbolGraphScope,
    pub meta: CallableCodeSurfaceMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeCallersSurfaceRequest {
    pub node_id: String,
    pub maximum_depth: u32,
    pub resolve_trait_dispatch: bool,
    pub scope: SymbolGraphScope,
    pub meta: CallableCodeSurfaceMeta,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum PrimitiveCodeSurfaceRequest {
    SymbolSearch(CodeSymbolSearchSurfaceRequest),
    SignatureSearch(CodeSignatureSearchSurfaceRequest),
    Implementations(CodeImplementationsSurfaceRequest),
    TypeHierarchy(CodeTypeHierarchySurfaceRequest),
    Callers(CodeCallersSurfaceRequest),
}

pub fn primitive_code_into_primitive(
    request: PrimitiveCodeSurfaceRequest,
    sanitizer_revision: SanitizerRevision,
    normalization_revision: QueryNormalizationRevision,
    page: PageRequest,
) -> Result<PrimitiveRequest, ApplicationContractError> {
    Ok(match request {
        PrimitiveCodeSurfaceRequest::SymbolSearch(request) => PrimitiveRequest::SymbolSearch(
            request.into_primitive_request(sanitizer_revision, normalization_revision, page)?,
        ),
        PrimitiveCodeSurfaceRequest::SignatureSearch(request) => {
            PrimitiveRequest::SignatureSearch(SignatureSearchRequest {
                returns: request.returns,
                params: request.params,
                is_async: request.is_async,
                scope: request.scope,
                meta: request.meta.into_application(page),
            })
        }
        PrimitiveCodeSurfaceRequest::Implementations(request) => {
            PrimitiveRequest::Implementations(ImplementationsRequest {
                selector: request.selector,
                scope: request.scope,
                meta: request.meta.into_application(page),
            })
        }
        PrimitiveCodeSurfaceRequest::TypeHierarchy(request) => {
            PrimitiveRequest::TypeHierarchy(TypeHierarchyRequest {
                node_id: request.node_id,
                maximum_depth: request.maximum_depth,
                scope: request.scope,
                meta: request.meta.into_application(page),
            })
        }
        PrimitiveCodeSurfaceRequest::Callers(request) => {
            PrimitiveRequest::Callers(GraphRelationRequest {
                node_id: request.node_id,
                maximum_depth: request.maximum_depth,
                resolve_trait_dispatch: request.resolve_trait_dispatch,
                scope: request.scope,
                meta: request.meta.into_application(page),
            })
        }
    })
}

impl CodeSymbolSearchSurfaceRequest {
    pub fn into_primitive_request(
        self,
        sanitizer_revision: SanitizerRevision,
        normalization_revision: QueryNormalizationRevision,
        page: PageRequest,
    ) -> Result<crate::retrieval::SymbolSearchPrimitiveRequest, ApplicationContractError> {
        let query = tracedecay_domain::EphemeralSanitizedQueryViewV1::sanitize(
            self.query,
            sanitizer_revision,
            normalization_revision,
        )?;
        Ok(crate::retrieval::SymbolSearchPrimitiveRequest {
            query,
            scope: self.scope,
            lazy_index_ignored_dependencies: self.lazy_index_ignored_dependencies,
            meta: self.meta.into_application(page),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Debug, Serialize, Deserialize)]
pub enum CallableCodeSurfaceRequest {
    ExactOccurrence(CodeExactOccurrenceSurfaceRequest),
    PhraseSearch(CodePhraseSearchSurfaceRequest),
    Callees(CodeCalleesSurfaceRequest),
    Facets(CodeFacetSurfaceRequest),
    Timeline(CodeTimelineSurfaceRequest),
    Declaration(CodeNavigationSurfaceRequest),
    Definition(CodeNavigationSurfaceRequest),
    TypeDefinition(CodeNavigationSurfaceRequest),
    References(CodeNavigationSurfaceRequest),
}
