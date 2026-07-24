use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as SyncMutex};

use serde_json::{Value, json};
use tokio::runtime::Handle;
use tokio::sync::Mutex;
use url::Url;

use crate::application::context::CancellationToken;
use crate::daemon::lsp_gateway::{
    AdmittedRoot, LspAnalyzerCancellationAuthority, LspPosition, LspRequestId, LspRuntimeFuture,
    LspSemanticOperationOutcome, LspSemanticRequestAuthority, Pr12SemanticProviderAdapter,
    SemanticCapability, SemanticProviderOutcome, SemanticProviderPort, SemanticRequest,
    SemanticResponse, utf16_position_to_byte_offset,
};
use crate::db::Database;
use crate::diagnostics::lsp::broker::{DiagnosticBroker, StdioLspSemanticAuthority};
use crate::diagnostics::lsp::client::{LspRefreshTimeouts, LspSemanticRequest};
use crate::errors::{Result, TraceDecayError};
use crate::types::{Edge, EdgeKind, Node, NodeKind};

const MAX_GRAPH_SEMANTIC_ITEMS: usize = 64;

#[derive(Clone)]
pub struct Pr12ProductionSemanticAuthorities {
    pub semantics: Arc<dyn SemanticProviderPort + Send + Sync>,
    pub cancellation: Arc<dyn LspAnalyzerCancellationAuthority>,
    pub analyzer_available: bool,
    pub semantic_capabilities: BTreeSet<SemanticCapability>,
}

/// Semantic methods implemented by the retained graph authority.
pub fn graph_semantic_capabilities() -> BTreeSet<SemanticCapability> {
    [
        SemanticCapability::Declaration,
        SemanticCapability::Definition,
        SemanticCapability::TypeDefinition,
        SemanticCapability::Implementation,
        SemanticCapability::References,
        SemanticCapability::Hover,
        SemanticCapability::DocumentSymbol,
        SemanticCapability::WorkspaceSymbol,
        SemanticCapability::CallHierarchy,
        SemanticCapability::SignatureHelp,
        SemanticCapability::TypeHierarchy,
    ]
    .into_iter()
    .collect()
}

/// Builds the concrete PR12 semantic and cancellation trait objects consumed
/// by `application::lsp_runtime::pr12_lsp_session_factory`.
///
/// The returned semantic provider first uses the retained stdio analyzer when
/// installed and otherwise serves the canonical project graph directly. It
/// also falls back to that graph when an analyzer lacks a standard method.
pub async fn pr12_production_semantic_authorities(
    runtime: Handle,
    diagnostic_broker: Arc<Mutex<DiagnosticBroker>>,
    graph_database: Database,
    language: Option<&str>,
    workspace_root: PathBuf,
    root_uri: impl Into<String>,
    timeouts: LspRefreshTimeouts,
) -> Result<Pr12ProductionSemanticAuthorities> {
    let root_uri = root_uri.into();
    let (upstream_authority, project_root) = {
        let mut broker = diagnostic_broker.lock().await;
        let project_root = broker.project_root().to_path_buf();
        let authority = match language {
            Some(language) => broker.semantic_authority_if_available(
                language,
                workspace_root,
                root_uri.clone(),
                timeouts,
            )?,
            None => None,
        };
        (authority, project_root)
    };
    Ok(pr12_semantic_authorities_from_parts(
        runtime,
        upstream_authority,
        Arc::new(DatabaseGraphSemanticAuthority::new(
            graph_database,
            project_root,
            root_uri,
        )),
    ))
}

pub fn pr12_semantic_authorities_from_parts(
    runtime: Handle,
    upstream: Option<Arc<StdioLspSemanticAuthority>>,
    graph: Arc<DatabaseGraphSemanticAuthority>,
) -> Pr12ProductionSemanticAuthorities {
    let analyzer_available = upstream.is_some();
    let upstream =
        upstream.map(|upstream| Pr12SemanticProviderAdapter::shared(runtime.clone(), upstream));
    let graph = Pr12SemanticProviderAdapter::shared(runtime, graph);
    let provider = Arc::new(StdioGraphSemanticProvider {
        upstream: upstream.clone(),
        graph: graph.clone(),
        graph_requests: SyncMutex::new(BTreeSet::new()),
    });
    let semantics: Arc<dyn SemanticProviderPort + Send + Sync> = provider.clone();
    let cancellation: Arc<dyn LspAnalyzerCancellationAuthority> =
        Arc::new(SemanticCancellationGroup {
            provider,
            upstream,
            graph,
        });
    Pr12ProductionSemanticAuthorities {
        semantics,
        cancellation,
        analyzer_available,
        semantic_capabilities: graph_semantic_capabilities(),
    }
}

struct SemanticCancellationGroup {
    provider: Arc<StdioGraphSemanticProvider>,
    upstream: Option<Arc<Pr12SemanticProviderAdapter>>,
    graph: Arc<Pr12SemanticProviderAdapter>,
}

impl LspAnalyzerCancellationAuthority for SemanticCancellationGroup {
    fn cancel_request(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool {
        self.provider.cancel_request(root, request_id)
            | self
                .upstream
                .as_ref()
                .is_some_and(|upstream| upstream.cancel_request(root, request_id))
            | self.graph.cancel_request(root, request_id)
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ProviderRequestKey {
    root_uri: String,
    request_id: LspRequestId,
}

struct StdioGraphSemanticProvider {
    upstream: Option<Arc<Pr12SemanticProviderAdapter>>,
    graph: Arc<Pr12SemanticProviderAdapter>,
    graph_requests: SyncMutex<BTreeSet<ProviderRequestKey>>,
}

impl StdioGraphSemanticProvider {
    fn key(root: &AdmittedRoot, request_id: &LspRequestId) -> ProviderRequestKey {
        ProviderRequestKey {
            root_uri: root.uri().to_owned(),
            request_id: request_id.clone(),
        }
    }

    fn cancel_request(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool {
        self.graph_requests
            .try_lock()
            .ok()
            .is_some_and(|mut requests| requests.remove(&Self::key(root, request_id)))
    }
}

impl SemanticProviderPort for StdioGraphSemanticProvider {
    fn request(
        &self,
        root: &AdmittedRoot,
        request_id: &LspRequestId,
        request: &SemanticRequest,
    ) -> SemanticProviderOutcome<SemanticResponse> {
        let key = Self::key(root, request_id);
        let graph_selected = match self.graph_requests.try_lock() {
            Ok(requests) => requests.contains(&key),
            Err(_) => return SemanticProviderOutcome::Pending,
        };
        if graph_selected {
            let outcome = SemanticProviderPort::request(&self.graph, root, request_id, request);
            if !matches!(&outcome, SemanticProviderOutcome::Pending)
                && let Ok(mut requests) = self.graph_requests.try_lock()
            {
                requests.remove(&key);
            }
            return outcome;
        }

        let Some(upstream) = &self.upstream else {
            return SemanticProviderPort::request(&self.graph, root, request_id, request);
        };
        match SemanticProviderPort::request(upstream, root, request_id, request) {
            SemanticProviderOutcome::Unavailable => {
                let Ok(mut requests) = self.graph_requests.try_lock() else {
                    return SemanticProviderOutcome::Pending;
                };
                requests.insert(key);
                drop(requests);
                SemanticProviderPort::request(&self.graph, root, request_id, request)
            }
            outcome => outcome,
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct GraphOperationKey {
    root_uri: String,
    request_id: LspRequestId,
}

pub struct DatabaseGraphSemanticAuthority {
    database: Database,
    project_root: PathBuf,
    root_uri: String,
    operations: Arc<Mutex<BTreeMap<GraphOperationKey, CancellationToken>>>,
}

impl DatabaseGraphSemanticAuthority {
    pub fn new(database: Database, project_root: PathBuf, root_uri: impl Into<String>) -> Self {
        Self {
            database,
            project_root,
            root_uri: root_uri.into(),
            operations: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
}

impl LspSemanticRequestAuthority for DatabaseGraphSemanticAuthority {
    fn start(
        &self,
        root: AdmittedRoot,
        request_id: LspRequestId,
        request: LspSemanticRequest,
    ) -> LspRuntimeFuture<LspSemanticOperationOutcome> {
        if root.uri() != self.root_uri {
            return Box::pin(async {
                LspSemanticOperationOutcome::Partial {
                    value: Value::Null,
                    coverage: "graph-root-mismatch".to_owned(),
                }
            });
        }
        let key = GraphOperationKey {
            root_uri: root.uri().to_owned(),
            request_id,
        };
        let cancellation = CancellationToken::new();
        let inserted = match self.operations.try_lock() {
            Ok(mut operations) => {
                if operations.contains_key(&key) {
                    false
                } else {
                    operations.insert(key.clone(), cancellation.clone());
                    true
                }
            }
            Err(_) => {
                return Box::pin(async {
                    LspSemanticOperationOutcome::Partial {
                        value: Value::Null,
                        coverage: "graph-runtime-busy".to_owned(),
                    }
                });
            }
        };
        if !inserted {
            return Box::pin(async {
                LspSemanticOperationOutcome::Partial {
                    value: Value::Null,
                    coverage: "graph-duplicate-operation".to_owned(),
                }
            });
        }

        let database = self.database.clone();
        let project_root = self.project_root.clone();
        let operations = self.operations.clone();
        Box::pin(async move {
            let outcome = tokio::select! {
                () = cancellation.cancelled() => LspSemanticOperationOutcome::Partial {
                    value: Value::Null,
                    coverage: "graph-cancelled".to_owned(),
                },
                outcome = graph_semantic_request(&database, &project_root, request) => outcome,
            };
            operations.lock().await.remove(&key);
            outcome
        })
    }

    fn cancel_request(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool {
        let key = GraphOperationKey {
            root_uri: root.uri().to_owned(),
            request_id: request_id.clone(),
        };
        self.operations
            .try_lock()
            .ok()
            .and_then(|operations| operations.get(&key).cloned())
            .is_some_and(|cancellation| {
                cancellation.cancel();
                true
            })
    }
}

async fn graph_semantic_request(
    database: &Database,
    project_root: &Path,
    request: LspSemanticRequest,
) -> LspSemanticOperationOutcome {
    let result: Result<GraphProjection> = async {
        match request {
            LspSemanticRequest::Declaration(params) | LspSemanticRequest::Definition(params) => {
                position_targets(
                    database,
                    project_root,
                    params
                        .text_document_position_params
                        .text_document
                        .uri
                        .as_str(),
                    params.text_document_position_params.position.line,
                    params.text_document_position_params.position.character,
                    &[
                        EdgeKind::Uses,
                        EdgeKind::Calls,
                        EdgeKind::Implements,
                        EdgeKind::Extends,
                    ],
                    EdgeDirection::Outgoing,
                )
                .await
            }
            LspSemanticRequest::TypeDefinition(params) => {
                position_targets(
                    database,
                    project_root,
                    params
                        .text_document_position_params
                        .text_document
                        .uri
                        .as_str(),
                    params.text_document_position_params.position.line,
                    params.text_document_position_params.position.character,
                    &[EdgeKind::TypeOf, EdgeKind::Returns, EdgeKind::Receives],
                    EdgeDirection::Outgoing,
                )
                .await
            }
            LspSemanticRequest::Implementation(params) => {
                position_targets(
                    database,
                    project_root,
                    params
                        .text_document_position_params
                        .text_document
                        .uri
                        .as_str(),
                    params.text_document_position_params.position.line,
                    params.text_document_position_params.position.character,
                    &[EdgeKind::Implements, EdgeKind::Extends],
                    EdgeDirection::Incoming,
                )
                .await
            }
            LspSemanticRequest::References(params) => {
                position_targets(
                    database,
                    project_root,
                    params.text_document_position.text_document.uri.as_str(),
                    params.text_document_position.position.line,
                    params.text_document_position.position.character,
                    &[
                        EdgeKind::Uses,
                        EdgeKind::Calls,
                        EdgeKind::TypeOf,
                        EdgeKind::Implements,
                        EdgeKind::Extends,
                        EdgeKind::Receives,
                    ],
                    EdgeDirection::Incoming,
                )
                .await
            }
            LspSemanticRequest::Hover(params) => {
                let node = node_at_position(
                    database,
                    project_root,
                    params
                        .text_document_position_params
                        .text_document
                        .uri
                        .as_str(),
                    params.text_document_position_params.position.line,
                    params.text_document_position_params.position.character,
                )
                .await?;
                Ok(GraphProjection::complete(node.map_or(
                    Value::Null,
                    |node| {
                        json!({
                            "contents": graph_hover(&node),
                            "range": node_range(&node),
                        })
                    },
                )))
            }
            LspSemanticRequest::DocumentSymbols(params) => {
                let path = relative_document_path(project_root, params.text_document.uri.as_str())?;
                let nodes = database.get_nodes_by_file(&path).await?;
                Ok(graph_nodes_projection(
                    nodes
                        .into_iter()
                        .filter(|node| node.kind != NodeKind::File)
                        .map(|node| document_symbol(&node))
                        .collect(),
                ))
            }
            LspSemanticRequest::WorkspaceSymbols(params) => {
                if params.query.trim().is_empty() {
                    return Ok(GraphProjection {
                        value: json!([]),
                        omitted: 1,
                    });
                }
                let nodes = database
                    .search_nodes(&params.query, MAX_GRAPH_SEMANTIC_ITEMS + 1)
                    .await?
                    .into_iter()
                    .map(|result| result.node);
                Ok(graph_nodes_projection(
                    nodes
                        .map(|node| workspace_symbol(project_root, &node))
                        .collect::<Result<Vec<_>>>()?,
                ))
            }
            LspSemanticRequest::PrepareCallHierarchy(params) => {
                hierarchy_prepare(
                    database,
                    project_root,
                    params
                        .text_document_position_params
                        .text_document
                        .uri
                        .as_str(),
                    params.text_document_position_params.position.line,
                    params.text_document_position_params.position.character,
                    call_item,
                )
                .await
            }
            LspSemanticRequest::IncomingCalls(params) => {
                hierarchy_calls(
                    database,
                    project_root,
                    params.item.uri.as_str(),
                    params.item.range.start.line,
                    params.item.range.start.character,
                    EdgeDirection::Incoming,
                )
                .await
            }
            LspSemanticRequest::OutgoingCalls(params) => {
                hierarchy_calls(
                    database,
                    project_root,
                    params.item.uri.as_str(),
                    params.item.range.start.line,
                    params.item.range.start.character,
                    EdgeDirection::Outgoing,
                )
                .await
            }
            LspSemanticRequest::SignatureHelp(params) => {
                signature_help(
                    database,
                    project_root,
                    params
                        .text_document_position_params
                        .text_document
                        .uri
                        .as_str(),
                    params.text_document_position_params.position.line,
                    params.text_document_position_params.position.character,
                )
                .await
            }
            LspSemanticRequest::PrepareTypeHierarchy(params) => {
                hierarchy_prepare(
                    database,
                    project_root,
                    params
                        .text_document_position_params
                        .text_document
                        .uri
                        .as_str(),
                    params.text_document_position_params.position.line,
                    params.text_document_position_params.position.character,
                    type_item,
                )
                .await
            }
            LspSemanticRequest::TypeHierarchySupertypes(params) => {
                hierarchy_types(
                    database,
                    project_root,
                    params.item.uri.as_str(),
                    params.item.range.start.line,
                    params.item.range.start.character,
                    EdgeDirection::Outgoing,
                )
                .await
            }
            LspSemanticRequest::TypeHierarchySubtypes(params) => {
                hierarchy_types(
                    database,
                    project_root,
                    params.item.uri.as_str(),
                    params.item.range.start.line,
                    params.item.range.start.character,
                    EdgeDirection::Incoming,
                )
                .await
            }
        }
    }
    .await;
    match result {
        Ok(projection) if projection.omitted == 0 => {
            LspSemanticOperationOutcome::Complete(projection.value)
        }
        Ok(projection) => LspSemanticOperationOutcome::Partial {
            value: projection.value,
            coverage: format!("graph-results-truncated-{}", projection.omitted),
        },
        Err(error) => LspSemanticOperationOutcome::Partial {
            value: Value::Null,
            coverage: format!("graph-read-{}", bounded_graph_failure(&error.to_string())),
        },
    }
}

struct GraphProjection {
    value: Value,
    omitted: usize,
}

impl GraphProjection {
    fn complete(value: Value) -> Self {
        Self { value, omitted: 0 }
    }
}

#[derive(Clone, Copy)]
enum EdgeDirection {
    Incoming,
    Outgoing,
}

async fn position_targets(
    database: &Database,
    project_root: &Path,
    uri: &str,
    line: u32,
    character: u32,
    kinds: &[EdgeKind],
    direction: EdgeDirection,
) -> Result<GraphProjection> {
    let Some(node) = node_at_position(database, project_root, uri, line, character).await? else {
        return Ok(GraphProjection::complete(json!([])));
    };
    let nodes = related_nodes(database, &node, kinds, direction).await?;
    let mut locations = nodes
        .iter()
        .map(|node| node_location(project_root, node))
        .collect::<Result<Vec<_>>>()?;
    if locations.is_empty() {
        locations.push(node_location(project_root, &node)?);
    }
    Ok(graph_nodes_projection(locations))
}

async fn hierarchy_prepare(
    database: &Database,
    project_root: &Path,
    uri: &str,
    line: u32,
    character: u32,
    project: fn(&Path, &Node) -> Result<Value>,
) -> Result<GraphProjection> {
    let value = match node_at_position(database, project_root, uri, line, character).await? {
        Some(node) => json!([project(project_root, &node)?]),
        None => json!([]),
    };
    Ok(GraphProjection::complete(value))
}

async fn hierarchy_calls(
    database: &Database,
    project_root: &Path,
    uri: &str,
    line: u32,
    character: u32,
    direction: EdgeDirection,
) -> Result<GraphProjection> {
    let Some(node) = node_at_position(database, project_root, uri, line, character).await? else {
        return Ok(GraphProjection::complete(json!([])));
    };
    let edges = related_edges(database, &node, &[EdgeKind::Calls], direction).await?;
    let ids = edges
        .iter()
        .map(|edge| match direction {
            EdgeDirection::Incoming => edge.source.clone(),
            EdgeDirection::Outgoing => edge.target.clone(),
        })
        .collect::<Vec<_>>();
    let nodes = database.get_nodes_by_ids(&ids).await?;
    let mut by_id = nodes
        .into_iter()
        .map(|node| (node.id.clone(), node))
        .collect::<BTreeMap<_, _>>();
    let values = edges
        .into_iter()
        .filter_map(|edge| {
            let id = match direction {
                EdgeDirection::Incoming => &edge.source,
                EdgeDirection::Outgoing => &edge.target,
            };
            let related = by_id.remove(id)?;
            let item = call_item(project_root, &related).ok()?;
            let range = edge_range(&related, edge.line);
            Some(match direction {
                EdgeDirection::Incoming => json!({ "from": item, "fromRanges": [range] }),
                EdgeDirection::Outgoing => json!({ "to": item, "fromRanges": [range] }),
            })
        })
        .collect();
    Ok(graph_nodes_projection(values))
}

async fn signature_help(
    database: &Database,
    project_root: &Path,
    uri: &str,
    line: u32,
    character: u32,
) -> Result<GraphProjection> {
    let Some(node) = node_at_position(database, project_root, uri, line, character).await? else {
        return Ok(GraphProjection::complete(Value::Null));
    };
    let mut nodes =
        related_nodes(database, &node, &[EdgeKind::Calls], EdgeDirection::Outgoing).await?;
    if nodes.is_empty() {
        nodes.push(node);
    }
    let signatures = nodes
        .into_iter()
        .filter_map(|node| node.signature.or(Some(node.qualified_name)))
        .map(|label| json!({ "label": label }))
        .collect::<Vec<_>>();
    let active_signature = if signatures.is_empty() {
        Value::Null
    } else {
        json!(0)
    };
    Ok(GraphProjection::complete(json!({
        "signatures": signatures,
        "activeSignature": active_signature,
        "activeParameter": Value::Null,
    })))
}

async fn hierarchy_types(
    database: &Database,
    project_root: &Path,
    uri: &str,
    line: u32,
    character: u32,
    direction: EdgeDirection,
) -> Result<GraphProjection> {
    let Some(node) = node_at_position(database, project_root, uri, line, character).await? else {
        return Ok(GraphProjection::complete(json!([])));
    };
    let nodes = related_nodes(
        database,
        &node,
        &[EdgeKind::Implements, EdgeKind::Extends],
        direction,
    )
    .await?;
    Ok(graph_nodes_projection(
        nodes
            .iter()
            .map(|node| type_item(project_root, node))
            .collect::<Result<Vec<_>>>()?,
    ))
}

async fn node_at_position(
    database: &Database,
    project_root: &Path,
    uri: &str,
    line: u32,
    character: u32,
) -> Result<Option<Node>> {
    let path = relative_document_path(project_root, uri)?;
    let document_path = scoped_document_path(project_root, &path)?;
    let text =
        crate::sync::read_source_file(&document_path).map_err(|error| TraceDecayError::Config {
            message: format!("failed to read semantic document: {error}"),
        })?;
    select_node_at_lsp_position(
        database.get_nodes_by_file(&path).await?,
        &text,
        LspPosition { line, character },
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("invalid semantic document position: {error:?}"),
    })
}

fn select_node_at_lsp_position(
    nodes: Vec<Node>,
    text: &str,
    position: LspPosition,
) -> std::result::Result<Option<Node>, crate::daemon::lsp_gateway::PositionError> {
    let offset = utf16_position_to_byte_offset(text, position)?;
    let line_start = text[..offset]
        .rfind('\n')
        .map_or(0, |newline| newline.saturating_add(1));
    let byte_column = u32::try_from(offset.saturating_sub(line_start))
        .map_err(|_| crate::daemon::lsp_gateway::PositionError::ByteOutOfBounds)?;
    Ok(nodes
        .into_iter()
        .filter(|node| node.kind != NodeKind::File)
        .filter(|node| node_contains_byte_position(node, position.line, byte_column))
        .min_by(|left, right| {
            node_position_span(left)
                .cmp(&node_position_span(right))
                .then_with(|| left.qualified_name.cmp(&right.qualified_name))
        }))
}

fn node_contains_byte_position(node: &Node, line: u32, byte_column: u32) -> bool {
    let after_start =
        line > node.start_line || (line == node.start_line && byte_column >= node.start_column);
    let before_end =
        line < node.end_line || (line == node.end_line && byte_column < node.end_column);
    after_start && before_end
}

fn node_position_span(node: &Node) -> (u32, u32) {
    (
        node.end_line.saturating_sub(node.start_line),
        node.end_column.saturating_sub(node.start_column),
    )
}

fn scoped_document_path(project_root: &Path, relative_path: &str) -> Result<PathBuf> {
    let canonical_root = project_root
        .canonicalize()
        .map_err(|error| TraceDecayError::Config {
            message: format!("failed to resolve admitted semantic root: {error}"),
        })?;
    let canonical_document =
        canonical_root
            .join(relative_path)
            .canonicalize()
            .map_err(|error| TraceDecayError::Config {
                message: format!("failed to resolve semantic document: {error}"),
            })?;
    if !canonical_document.starts_with(&canonical_root) {
        return Err(TraceDecayError::Config {
            message: "semantic document resolves outside the admitted project root".to_owned(),
        });
    }
    Ok(canonical_document)
}

async fn related_nodes(
    database: &Database,
    node: &Node,
    kinds: &[EdgeKind],
    direction: EdgeDirection,
) -> Result<Vec<Node>> {
    let edges = related_edges(database, node, kinds, direction).await?;
    let ids = edges
        .into_iter()
        .map(|edge| match direction {
            EdgeDirection::Incoming => edge.source,
            EdgeDirection::Outgoing => edge.target,
        })
        .collect::<Vec<_>>();
    database.get_nodes_by_ids(&ids).await
}

async fn related_edges(
    database: &Database,
    node: &Node,
    kinds: &[EdgeKind],
    direction: EdgeDirection,
) -> Result<Vec<Edge>> {
    match direction {
        EdgeDirection::Incoming => database.get_incoming_edges(&node.id, kinds).await,
        EdgeDirection::Outgoing => database.get_outgoing_edges(&node.id, kinds).await,
    }
}

fn graph_nodes_projection(mut values: Vec<Value>) -> GraphProjection {
    let omitted = values.len().saturating_sub(MAX_GRAPH_SEMANTIC_ITEMS);
    values.truncate(MAX_GRAPH_SEMANTIC_ITEMS);
    GraphProjection {
        value: Value::Array(values),
        omitted,
    }
}

fn relative_document_path(project_root: &Path, uri: &str) -> Result<String> {
    let url = Url::parse(uri).map_err(|error| TraceDecayError::Config {
        message: format!("invalid semantic document URI: {error}"),
    })?;
    let path = url.to_file_path().map_err(|()| TraceDecayError::Config {
        message: "semantic document URI is not a file URI".to_owned(),
    })?;
    path.strip_prefix(project_root)
        .ok()
        .and_then(Path::to_str)
        .map(|path| path.replace('\\', "/"))
        .ok_or_else(|| TraceDecayError::Config {
            message: "semantic document is outside the admitted project root".to_owned(),
        })
}

fn node_location(project_root: &Path, node: &Node) -> Result<Value> {
    Ok(json!({
        "uri": node_uri(project_root, node)?,
        "range": node_range(node),
    }))
}

fn document_symbol(node: &Node) -> Value {
    json!({
        "name": node.name,
        "kind": symbol_kind(&node.kind),
        "range": node_range(node),
        "selectionRange": node_range(node),
        "children": [],
    })
}

fn workspace_symbol(project_root: &Path, node: &Node) -> Result<Value> {
    Ok(json!({
        "name": node.qualified_name,
        "kind": symbol_kind(&node.kind),
        "location": node_location(project_root, node)?,
    }))
}

fn call_item(project_root: &Path, node: &Node) -> Result<Value> {
    Ok(json!({
        "name": node.name,
        "kind": symbol_kind(&node.kind),
        "uri": node_uri(project_root, node)?,
        "range": node_range(node),
        "selectionRange": node_range(node),
    }))
}

fn type_item(project_root: &Path, node: &Node) -> Result<Value> {
    call_item(project_root, node)
}

fn node_uri(project_root: &Path, node: &Node) -> Result<String> {
    Url::from_file_path(project_root.join(&node.file_path))
        .map(|url| url.to_string())
        .map_err(|()| TraceDecayError::Config {
            message: "failed to project graph node file URI".to_owned(),
        })
}

fn node_range(node: &Node) -> Value {
    json!({
        "start": { "line": node.start_line, "character": node.start_column },
        "end": { "line": node.end_line, "character": node.end_column },
    })
}

fn edge_range(node: &Node, line: Option<u32>) -> Value {
    let line = line.unwrap_or(node.start_line);
    json!({
        "start": { "line": line, "character": 0 },
        "end": { "line": line, "character": 0 },
    })
}

fn graph_hover(node: &Node) -> String {
    match (&node.signature, &node.docstring) {
        (Some(signature), Some(docstring)) => format!("{signature}\n\n{docstring}"),
        (Some(signature), None) => signature.clone(),
        (None, Some(docstring)) => docstring.clone(),
        (None, None) => node.qualified_name.clone(),
    }
}

fn symbol_kind(kind: &NodeKind) -> u32 {
    match kind {
        NodeKind::File => 1,
        NodeKind::Module => 2,
        NodeKind::Namespace => 3,
        NodeKind::Package
        | NodeKind::GoPackage
        | NodeKind::ScalaPackage
        | NodeKind::KotlinPackage => 4,
        NodeKind::Class
        | NodeKind::InnerClass
        | NodeKind::CaseClass
        | NodeKind::DataClass
        | NodeKind::SealedClass => 5,
        NodeKind::Method
        | NodeKind::StructMethod
        | NodeKind::AbstractMethod
        | NodeKind::Procedure => 6,
        NodeKind::Property | NodeKind::CSharpProperty => 7,
        NodeKind::Field | NodeKind::ValField | NodeKind::VarField => 8,
        NodeKind::Constructor | NodeKind::InitBlock => 9,
        NodeKind::Enum => 10,
        NodeKind::Trait | NodeKind::Interface | NodeKind::InterfaceType => 11,
        NodeKind::Function | NodeKind::ArrowFunction => 12,
        NodeKind::Const | NodeKind::Static => 14,
        NodeKind::EnumVariant => 22,
        NodeKind::Struct | NodeKind::Record | NodeKind::PascalRecord => 23,
        NodeKind::Event => 24,
        NodeKind::TypeAlias | NodeKind::Typedef | NodeKind::GenericParam | NodeKind::Template => 26,
        _ => 13,
    }
}

fn bounded_graph_failure(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        .take(64)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn function_node(name: &str, start_column: u32, end_column: u32) -> Node {
        Node {
            id: format!("node.{name}"),
            kind: NodeKind::Function,
            name: name.to_owned(),
            qualified_name: format!("fixture::{name}"),
            file_path: "src/lib.rs".to_owned(),
            start_line: 0,
            attrs_start_line: 0,
            end_line: 0,
            start_column,
            end_column,
            signature: None,
            docstring: None,
            visibility: crate::types::Visibility::Private,
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
        }
    }

    #[test]
    fn advertised_semantics_derive_from_the_graph_provider_contract() {
        assert_eq!(
            graph_semantic_capabilities(),
            [
                SemanticCapability::Declaration,
                SemanticCapability::Definition,
                SemanticCapability::TypeDefinition,
                SemanticCapability::Implementation,
                SemanticCapability::References,
                SemanticCapability::Hover,
                SemanticCapability::DocumentSymbol,
                SemanticCapability::WorkspaceSymbol,
                SemanticCapability::CallHierarchy,
                SemanticCapability::SignatureHelp,
                SemanticCapability::TypeHierarchy,
            ]
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn exact_utf16_character_selects_the_matching_same_line_symbol() {
        let text = "🙂 first(); second();\n";
        let nodes = vec![
            function_node("first", 5, 12),
            function_node("second", 14, 22),
        ];

        let selected = select_node_at_lsp_position(
            nodes.clone(),
            text,
            LspPosition {
                line: 0,
                character: 12,
            },
        )
        .expect("valid UTF-16 position")
        .expect("second symbol");
        assert_eq!(selected.name, "second");

        assert!(
            select_node_at_lsp_position(
                nodes,
                text,
                LspPosition {
                    line: 0,
                    character: 10,
                },
            )
            .expect("valid UTF-16 position")
            .is_none(),
            "the exclusive end of the first symbol must not resolve it"
        );
    }

    #[test]
    fn semantic_position_rejects_the_middle_of_a_utf16_surrogate_pair() {
        let error = select_node_at_lsp_position(
            vec![function_node("emoji", 0, 4)],
            "🙂",
            LspPosition {
                line: 0,
                character: 1,
            },
        )
        .expect_err("a partial surrogate pair is not a negotiated position");
        assert_eq!(
            error,
            crate::daemon::lsp_gateway::PositionError::InsideSurrogatePair
        );
    }

    #[test]
    fn semantic_document_reads_cannot_escape_the_admitted_root() {
        let fixture = tempfile::tempdir().expect("fixture");
        let root = fixture.path().join("root");
        std::fs::create_dir(&root).expect("root");
        std::fs::write(fixture.path().join("outside.rs"), "fn outside() {}").expect("outside");

        let error =
            scoped_document_path(&root, "../outside.rs").expect_err("scope escape must fail");
        assert!(
            error
                .to_string()
                .contains("outside the admitted project root")
        );
    }
}
