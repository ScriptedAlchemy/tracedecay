use std::future::Future;
use std::pin::Pin;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{EphemeralSanitizedQueryViewV1, UtcMicros};

use crate::context::RequestContext;
use crate::error::ApplicationContractError;
use crate::handlers::ApplicationOperation;
use crate::result::{OpaqueCursor, OperationBudgetUsage};

use super::RetrievalRequestMeta;

pub const MAX_SYMBOL_GRAPH_DEPTH: u32 = 10;
pub const MAX_SYMBOL_GRAPH_QUERY_BYTES: usize = 4_096;
pub const MAX_SYMBOL_GRAPH_FILTERS: usize = 32;

/// Optional narrowing inside the immutable project/repository/worktree scope
/// carried by [`RequestContext`]. A path prefix never establishes identity or
/// authorization.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SymbolGraphScope {
    pub path_prefix: Option<String>,
}

impl SymbolGraphScope {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if let Some(path_prefix) = &self.path_prefix {
            validate_text(path_prefix, "symbol graph path prefix")?;
            if path_prefix.starts_with('/') || path_prefix.split('/').any(|part| part == "..") {
                return Err(ApplicationContractError::Inconsistent {
                    field: "symbol graph path prefix",
                });
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SymbolPrimitiveRecord {
    pub node_id: String,
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub file: String,
    /// Canonical tree-sitter row retained for compatibility adapters.
    pub start_line_zero_based: u32,
    pub end_line_zero_based: u32,
    /// One-based user-facing line.
    pub line: u32,
    pub end_line: u32,
    pub signature: Option<String>,
    pub is_async: bool,
    pub score: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SymbolRelationRecord {
    pub symbol: SymbolPrimitiveRecord,
    pub edge_kind: String,
    pub dispatch_via_trait: bool,
    pub dispatch_from: Option<String>,
    pub depth: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TypeHierarchyRecord {
    pub symbol: SymbolPrimitiveRecord,
    pub parent_node_id: String,
    pub edge_kind: String,
    pub depth: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PrimitiveSupportGap {
    pub provider: Option<String>,
    pub language: Option<String>,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PrimitiveFailureKind {
    InvalidRequest,
    NotFoundOrNotAuthorized,
    Stale,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PrimitiveFailure {
    pub kind: PrimitiveFailureKind,
    pub code: String,
    pub message: String,
}

impl PrimitiveFailure {
    pub fn new(
        kind: PrimitiveFailureKind,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<Self, ApplicationContractError> {
        let code = code.into();
        let message = message.into();
        validate_query(&code, "symbol graph failure code")?;
        validate_query(&message, "symbol graph failure message")?;
        Ok(Self {
            kind,
            code,
            message,
        })
    }
}

impl PrimitiveSupportGap {
    pub fn unsupported(
        provider: Option<String>,
        language: Option<String>,
        reason: impl Into<String>,
    ) -> Result<Self, ApplicationContractError> {
        let reason = reason.into();
        validate_text(&reason, "symbol graph support reason")?;
        if let Some(provider) = &provider {
            validate_query(provider, "symbol graph provider")?;
        }
        if let Some(language) = &language {
            validate_query(language, "symbol graph language")?;
        }
        Ok(Self {
            provider,
            language,
            reason,
        })
    }
}

/// Bounded semantic result shared by the compatibility surfaces. Rendering,
/// transport envelopes, and MCP content blocks remain outside this contract.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SymbolGraphPage<T> {
    pub items: Vec<T>,
    pub total: Option<u64>,
    pub next_cursor: Option<OpaqueCursor>,
    pub truncated: bool,
    pub related_edge_count: Option<u64>,
    pub support_gaps: Vec<PrimitiveSupportGap>,
}

impl<T> SymbolGraphPage<T> {
    pub fn complete(items: Vec<T>, total: Option<u64>, next_cursor: Option<OpaqueCursor>) -> Self {
        let truncated = next_cursor.is_some();
        Self {
            items,
            total,
            next_cursor,
            truncated,
            related_edge_count: None,
            support_gaps: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub struct SymbolSearchPrimitiveRequest {
    pub query: EphemeralSanitizedQueryViewV1,
    pub scope: SymbolGraphScope,
    pub lazy_index_ignored_dependencies: bool,
    pub meta: RetrievalRequestMeta,
}

impl SymbolSearchPrimitiveRequest {
    /// Validate this request before dispatching it to a primitive port.
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        <Self as ValidatedPrimitiveRequest>::validate(self)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExactSymbolRequest {
    pub name: String,
    pub scope: SymbolGraphScope,
    pub lazy_index_ignored_dependencies: bool,
    pub meta: RetrievalRequestMeta,
}

impl ExactSymbolRequest {
    /// Validate this request before dispatching it to a primitive port.
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        <Self as ValidatedPrimitiveRequest>::validate(self)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SignatureSearchRequest {
    pub returns: Option<String>,
    pub params: Vec<String>,
    pub is_async: Option<bool>,
    pub scope: SymbolGraphScope,
    pub meta: RetrievalRequestMeta,
}

impl SignatureSearchRequest {
    /// Validate this request before dispatching it to a primitive port.
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        <Self as ValidatedPrimitiveRequest>::validate(self)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "selector")]
pub enum ImplementationSelector {
    Trait { name: String },
    Method { name: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ImplementationsRequest {
    pub selector: ImplementationSelector,
    pub scope: SymbolGraphScope,
    pub meta: RetrievalRequestMeta,
}

impl ImplementationsRequest {
    /// Validate this request before dispatching it to a primitive port.
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        <Self as ValidatedPrimitiveRequest>::validate(self)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TypeHierarchyRequest {
    pub node_id: String,
    pub maximum_depth: u32,
    pub scope: SymbolGraphScope,
    pub meta: RetrievalRequestMeta,
}

impl TypeHierarchyRequest {
    /// Validate this request before dispatching it to a primitive port.
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        <Self as ValidatedPrimitiveRequest>::validate(self)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphRelationRequest {
    pub node_id: String,
    pub maximum_depth: u32,
    pub resolve_trait_dispatch: bool,
    pub scope: SymbolGraphScope,
    pub meta: RetrievalRequestMeta,
}

impl GraphRelationRequest {
    /// Validate this request before dispatching it to a primitive port.
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        <Self as ValidatedPrimitiveRequest>::validate(self)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphImpactPrimitiveRequest {
    pub node_id: String,
    pub maximum_depth: u32,
    pub scope: SymbolGraphScope,
    pub meta: RetrievalRequestMeta,
}

impl GraphImpactPrimitiveRequest {
    /// Validate this request before dispatching it to a primitive port.
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        <Self as ValidatedPrimitiveRequest>::validate(self)
    }
}

trait ValidatedPrimitiveRequest {
    fn validate(&self) -> Result<(), ApplicationContractError>;
}

impl ValidatedPrimitiveRequest for SymbolSearchPrimitiveRequest {
    fn validate(&self) -> Result<(), ApplicationContractError> {
        validate_query(self.query.as_str(), "symbol search query")?;
        self.scope.validate()?;
        validate_meta(&self.meta)
    }
}

impl ValidatedPrimitiveRequest for ExactSymbolRequest {
    fn validate(&self) -> Result<(), ApplicationContractError> {
        validate_query(&self.name, "exact symbol name")?;
        self.scope.validate()?;
        validate_meta(&self.meta)
    }
}

impl ValidatedPrimitiveRequest for SignatureSearchRequest {
    fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.returns.is_none() && self.params.is_empty() && self.is_async.is_none() {
            return Err(ApplicationContractError::Inconsistent {
                field: "signature search filters",
            });
        }
        if self.params.len() > MAX_SYMBOL_GRAPH_FILTERS {
            return Err(ApplicationContractError::InvalidRange {
                field: "signature search parameter filters",
            });
        }
        if let Some(returns) = &self.returns {
            validate_query(returns, "signature return filter")?;
        }
        for param in &self.params {
            validate_query(param, "signature parameter filter")?;
        }
        self.scope.validate()?;
        validate_meta(&self.meta)
    }
}

impl ValidatedPrimitiveRequest for ImplementationsRequest {
    fn validate(&self) -> Result<(), ApplicationContractError> {
        match &self.selector {
            ImplementationSelector::Trait { name } => {
                validate_query(name, "implementation trait name")?
            }
            ImplementationSelector::Method { name } => {
                validate_query(name, "implementation method name")?
            }
        }
        self.scope.validate()?;
        validate_meta(&self.meta)
    }
}

impl ValidatedPrimitiveRequest for TypeHierarchyRequest {
    fn validate(&self) -> Result<(), ApplicationContractError> {
        validate_node_depth(&self.node_id, self.maximum_depth)?;
        self.scope.validate()?;
        validate_meta(&self.meta)
    }
}

impl ValidatedPrimitiveRequest for GraphRelationRequest {
    fn validate(&self) -> Result<(), ApplicationContractError> {
        validate_node_depth(&self.node_id, self.maximum_depth)?;
        self.scope.validate()?;
        validate_meta(&self.meta)
    }
}

impl ValidatedPrimitiveRequest for GraphImpactPrimitiveRequest {
    fn validate(&self) -> Result<(), ApplicationContractError> {
        validate_node_depth(&self.node_id, self.maximum_depth)?;
        self.scope.validate()?;
        validate_meta(&self.meta)
    }
}

fn validate_meta(meta: &RetrievalRequestMeta) -> Result<(), ApplicationContractError> {
    if meta.temporal != tracedecay_domain::TemporalModeV1::Current {
        return Err(ApplicationContractError::Inconsistent {
            field: "symbol graph temporal mode",
        });
    }
    super::PageRequest::new(meta.page.page_size, meta.page.cursor.clone()).map(|_| ())
}

fn validate_node_depth(node_id: &str, maximum_depth: u32) -> Result<(), ApplicationContractError> {
    validate_text(node_id, "symbol graph node id")?;
    if maximum_depth == 0 || maximum_depth > MAX_SYMBOL_GRAPH_DEPTH {
        return Err(ApplicationContractError::InvalidRange {
            field: "symbol graph maximum depth",
        });
    }
    Ok(())
}

fn validate_query(value: &str, field: &'static str) -> Result<(), ApplicationContractError> {
    validate_text(value, field)?;
    if value.len() > MAX_SYMBOL_GRAPH_QUERY_BYTES {
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

#[derive(Clone, Debug, PartialEq)]
pub enum SymbolGraphPortOutcome<T> {
    Completed {
        page: SymbolGraphPage<T>,
        finished_at: UtcMicros,
        budget: OperationBudgetUsage,
    },
    Partial {
        page: SymbolGraphPage<T>,
        finished_at: UtcMicros,
        budget: OperationBudgetUsage,
    },
    Failed {
        failure: PrimitiveFailure,
        finished_at: UtcMicros,
        budget: OperationBudgetUsage,
    },
}

pub type SymbolGraphPortFuture<'a, T> =
    Pin<Box<dyn Future<Output = SymbolGraphPortOutcome<T>> + Send + 'a>>;

#[derive(Clone, Copy, Debug)]
pub struct SymbolGraphPortContext<'a> {
    pub request: &'a RequestContext,
    pub operation: &'a ApplicationOperation,
    pub observed_at: UtcMicros,
}

/// Production async port for the symbol and graph primitive family.
/// Implementations delegate to the owning query/graph kernels.
pub trait SymbolGraphPrimitivePort {
    fn symbol_search<'a>(
        &'a self,
        context: SymbolGraphPortContext<'a>,
        request: &'a SymbolSearchPrimitiveRequest,
    ) -> SymbolGraphPortFuture<'a, SymbolPrimitiveRecord>;

    fn exact_symbol<'a>(
        &'a self,
        context: SymbolGraphPortContext<'a>,
        request: &'a ExactSymbolRequest,
    ) -> SymbolGraphPortFuture<'a, SymbolPrimitiveRecord>;

    fn signature_search<'a>(
        &'a self,
        context: SymbolGraphPortContext<'a>,
        request: &'a SignatureSearchRequest,
    ) -> SymbolGraphPortFuture<'a, SymbolPrimitiveRecord>;

    fn implementations<'a>(
        &'a self,
        context: SymbolGraphPortContext<'a>,
        request: &'a ImplementationsRequest,
    ) -> SymbolGraphPortFuture<'a, SymbolRelationRecord>;

    fn type_hierarchy<'a>(
        &'a self,
        context: SymbolGraphPortContext<'a>,
        request: &'a TypeHierarchyRequest,
    ) -> SymbolGraphPortFuture<'a, TypeHierarchyRecord>;

    fn callers<'a>(
        &'a self,
        context: SymbolGraphPortContext<'a>,
        request: &'a GraphRelationRequest,
    ) -> SymbolGraphPortFuture<'a, SymbolRelationRecord>;

    fn callees<'a>(
        &'a self,
        context: SymbolGraphPortContext<'a>,
        request: &'a GraphRelationRequest,
    ) -> SymbolGraphPortFuture<'a, SymbolRelationRecord>;

    fn impact<'a>(
        &'a self,
        context: SymbolGraphPortContext<'a>,
        request: &'a GraphImpactPrimitiveRequest,
    ) -> SymbolGraphPortFuture<'a, SymbolPrimitiveRecord>;
}
