use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as SyncMutex};

use lsp_types::PrepareRenameResponse;
use serde_json::{Value, json};
use tokio::runtime::Handle;
use tokio::sync::Mutex;
use url::Url;

use crate::application::context::CancellationToken;
use crate::daemon::lsp_gateway::{
    AdmittedRoot, LspAnalyzerCancellationAuthority, LspPosition, LspRequestId, LspRuntimeFuture,
    LspSemanticOperationOutcome, LspSemanticRequestAuthority, Pr12SemanticProviderAdapter,
    RenameCandidate, RenameCandidateResult, RenameCandidateUnavailableReason, SemanticCapability,
    SemanticProviderOutcome, SemanticProviderPort, SemanticRequest, SemanticResponse,
    byte_offset_to_utf16_position, utf16_position_to_byte_offset,
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
    languages: &[String],
    workspace_root: PathBuf,
    root_uri: impl Into<String>,
    timeouts: LspRefreshTimeouts,
) -> Result<Pr12ProductionSemanticAuthorities> {
    let root_uri = root_uri.into();
    let (upstream_routes, project_root) = {
        let mut broker = diagnostic_broker.lock().await;
        let project_root = broker.project_root().to_path_buf();
        let mut routes = Vec::new();
        for language in languages {
            let adapter = broker
                .adapter_for(language)
                .ok_or_else(|| TraceDecayError::Config {
                    message: format!("no LSP adapter registered for language '{language}'"),
                })?;
            if let Some(authority) = broker.semantic_authority_if_available(
                language,
                workspace_root.clone(),
                root_uri.clone(),
                timeouts,
            )? {
                routes.push((adapter, authority));
            }
        }
        (routes, project_root)
    };
    let graph = Arc::new(DatabaseGraphSemanticAuthority::new(
        graph_database,
        project_root,
        root_uri,
    ));
    let fallback = pr12_semantic_authorities_from_parts(runtime.clone(), None, Arc::clone(&graph));
    if upstream_routes.is_empty() {
        return Ok(fallback);
    }

    let mut routes = Vec::with_capacity(upstream_routes.len());
    let mut cancellation = vec![Arc::clone(&fallback.cancellation)];
    let mut semantic_capabilities = fallback.semantic_capabilities.clone();
    for (adapter, upstream) in upstream_routes {
        let authority = pr12_semantic_authorities_from_parts(
            runtime.clone(),
            Some(upstream),
            Arc::clone(&graph),
        );
        semantic_capabilities.extend(authority.semantic_capabilities.iter().copied());
        cancellation.push(Arc::clone(&authority.cancellation));
        routes.push(LanguageSemanticRoute::new(
            adapter.extensions,
            authority.semantics,
        ));
    }

    Ok(Pr12ProductionSemanticAuthorities {
        semantics: Arc::new(PolyglotSemanticProvider::new(routes, fallback.semantics)),
        cancellation: Arc::new(CompositeSemanticCancellation { cancellation }),
        analyzer_available: true,
        semantic_capabilities,
    })
}

struct CompositeSemanticCancellation {
    cancellation: Vec<Arc<dyn LspAnalyzerCancellationAuthority>>,
}

impl LspAnalyzerCancellationAuthority for CompositeSemanticCancellation {
    fn cancel_request(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool {
        self.cancellation
            .iter()
            .fold(false, |cancelled, authority| {
                authority.cancel_request(root, request_id) | cancelled
            })
    }
}

struct LanguageSemanticRoute {
    extensions: BTreeSet<String>,
    provider: Arc<dyn SemanticProviderPort + Send + Sync>,
}

impl LanguageSemanticRoute {
    fn new(
        extensions: impl IntoIterator<Item = impl Into<String>>,
        provider: Arc<dyn SemanticProviderPort + Send + Sync>,
    ) -> Self {
        Self {
            extensions: extensions
                .into_iter()
                .map(Into::into)
                .map(|extension: String| extension.to_ascii_lowercase())
                .collect(),
            provider,
        }
    }
}

struct PolyglotSemanticProvider {
    routes: Vec<LanguageSemanticRoute>,
    fallback: Arc<dyn SemanticProviderPort + Send + Sync>,
}

impl PolyglotSemanticProvider {
    fn new(
        routes: Vec<LanguageSemanticRoute>,
        fallback: Arc<dyn SemanticProviderPort + Send + Sync>,
    ) -> Self {
        Self { routes, fallback }
    }

    fn provider_for(
        &self,
        request: &SemanticRequest,
    ) -> &Arc<dyn SemanticProviderPort + Send + Sync> {
        let Some(extension) = semantic_request_extension(request) else {
            return &self.fallback;
        };
        let mut matching = self
            .routes
            .iter()
            .filter(|route| route.extensions.contains(&extension));
        let Some(route) = matching.next() else {
            return &self.fallback;
        };
        if matching.next().is_some() {
            return &self.fallback;
        }
        &route.provider
    }
}

impl SemanticProviderPort for PolyglotSemanticProvider {
    fn request(
        &self,
        root: &AdmittedRoot,
        request_id: &LspRequestId,
        request: &SemanticRequest,
    ) -> SemanticProviderOutcome<SemanticResponse> {
        self.provider_for(request)
            .request(root, request_id, request)
    }
}

fn semantic_request_extension(request: &SemanticRequest) -> Option<String> {
    let uri = Url::parse(request.document_uri()?).ok()?;
    if uri.scheme() != "file" {
        return None;
    }
    uri.to_file_path()
        .ok()?
        .extension()?
        .to_str()
        .map(str::to_ascii_lowercase)
}

pub fn pr12_semantic_authorities_from_parts(
    runtime: Handle,
    upstream: Option<Arc<StdioLspSemanticAuthority>>,
    graph: Arc<DatabaseGraphSemanticAuthority>,
) -> Pr12ProductionSemanticAuthorities {
    let analyzer_available = upstream.is_some();
    let rename = upstream.as_ref().map(|upstream| {
        Pr12SemanticProviderAdapter::shared(
            runtime.clone(),
            Arc::new(RenameCandidateMergeAuthority {
                analyzer: upstream.clone(),
                graph: graph.clone(),
            }),
        )
    });
    let upstream =
        upstream.map(|upstream| Pr12SemanticProviderAdapter::shared(runtime.clone(), upstream));
    let graph = Pr12SemanticProviderAdapter::shared(runtime, graph);
    let provider = Arc::new(StdioGraphSemanticProvider {
        upstream: upstream.clone(),
        graph: graph.clone(),
        rename: rename.clone(),
        graph_requests: SyncMutex::new(BTreeSet::new()),
    });
    let semantics: Arc<dyn SemanticProviderPort + Send + Sync> = provider.clone();
    let cancellation: Arc<dyn LspAnalyzerCancellationAuthority> =
        Arc::new(SemanticCancellationGroup {
            provider,
            upstream,
            graph,
            rename,
        });
    let mut semantic_capabilities = graph_semantic_capabilities();
    if analyzer_available {
        semantic_capabilities.insert(SemanticCapability::RenameCandidate);
    }
    Pr12ProductionSemanticAuthorities {
        semantics,
        cancellation,
        analyzer_available,
        semantic_capabilities,
    }
}

struct SemanticCancellationGroup {
    provider: Arc<StdioGraphSemanticProvider>,
    upstream: Option<Arc<Pr12SemanticProviderAdapter>>,
    graph: Arc<Pr12SemanticProviderAdapter>,
    rename: Option<Arc<Pr12SemanticProviderAdapter>>,
}

impl LspAnalyzerCancellationAuthority for SemanticCancellationGroup {
    fn cancel_request(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool {
        self.provider.cancel_request(root, request_id)
            | self
                .upstream
                .as_ref()
                .is_some_and(|upstream| upstream.cancel_request(root, request_id))
            | self.graph.cancel_request(root, request_id)
            | self
                .rename
                .as_ref()
                .is_some_and(|rename| rename.cancel_request(root, request_id))
    }
}

struct RenameCandidateMergeAuthority {
    analyzer: Arc<StdioLspSemanticAuthority>,
    graph: Arc<DatabaseGraphSemanticAuthority>,
}

impl LspSemanticRequestAuthority for RenameCandidateMergeAuthority {
    fn start(
        &self,
        root: AdmittedRoot,
        request_id: LspRequestId,
        request: LspSemanticRequest,
    ) -> LspRuntimeFuture<LspSemanticOperationOutcome> {
        let LspSemanticRequest::PrepareRename(params) = &request else {
            return Box::pin(async { LspSemanticOperationOutcome::Unavailable });
        };
        let document_uri = params.text_document.uri.to_string();
        let analyzer = self
            .analyzer
            .start(root.clone(), request_id.clone(), request.clone());
        let graph = self.graph.start(root, request_id, request);
        Box::pin(async move {
            let (analyzer, graph) = tokio::join!(analyzer, graph);
            LspSemanticOperationOutcome::RenameCandidate(merge_rename_candidate_outcomes(
                &document_uri,
                analyzer,
                graph,
            ))
        })
    }

    fn cancel_request(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool {
        self.analyzer.cancel_request(root, request_id) | self.graph.cancel_request(root, request_id)
    }
}

#[derive(Debug, Eq, PartialEq)]
struct RawRenameCandidate {
    range: crate::daemon::lsp_gateway::LspRange,
    placeholder: Option<String>,
}

fn merge_rename_candidate_outcomes(
    document_uri: &str,
    analyzer: LspSemanticOperationOutcome,
    graph: LspSemanticOperationOutcome,
) -> RenameCandidateResult {
    let analyzer = match rename_candidate_from_outcome(analyzer, true) {
        Ok(candidate) => candidate,
        Err(reason) => return RenameCandidateResult::Unavailable { reason },
    };
    let graph = match rename_candidate_from_outcome(graph, false) {
        Ok(candidate) => candidate,
        Err(reason) => return RenameCandidateResult::Unavailable { reason },
    };
    let (Some(analyzer), Some(graph)) = (analyzer, graph) else {
        return RenameCandidateResult::Unavailable {
            reason: RenameCandidateUnavailableReason::EvidenceAbsent,
        };
    };
    if analyzer.range != graph.range
        || analyzer
            .placeholder
            .as_ref()
            .is_some_and(|placeholder| Some(placeholder) != graph.placeholder.as_ref())
    {
        return RenameCandidateResult::Unavailable {
            reason: RenameCandidateUnavailableReason::AmbiguousEvidence,
        };
    }
    let Some(placeholder) = graph.placeholder.or(analyzer.placeholder) else {
        return RenameCandidateResult::Unavailable {
            reason: RenameCandidateUnavailableReason::AmbiguousEvidence,
        };
    };
    RenameCandidateResult::Available(RenameCandidate {
        document_uri: document_uri.to_owned(),
        range: graph.range,
        placeholder,
    })
}

fn rename_candidate_from_outcome(
    outcome: LspSemanticOperationOutcome,
    analyzer: bool,
) -> std::result::Result<Option<RawRenameCandidate>, RenameCandidateUnavailableReason> {
    match outcome {
        LspSemanticOperationOutcome::Complete(value) => parse_raw_rename_candidate(value),
        LspSemanticOperationOutcome::Partial { coverage, .. } => Err(
            if coverage.contains("stale") || coverage.contains("superseded") {
                RenameCandidateUnavailableReason::StaleEvidence
            } else if analyzer {
                RenameCandidateUnavailableReason::AnalyzerUnavailable
            } else {
                RenameCandidateUnavailableReason::GraphUnavailable
            },
        ),
        LspSemanticOperationOutcome::Unavailable => Err(if analyzer {
            RenameCandidateUnavailableReason::AnalyzerUnavailable
        } else {
            RenameCandidateUnavailableReason::GraphUnavailable
        }),
        LspSemanticOperationOutcome::RenameCandidate(_) => {
            Err(RenameCandidateUnavailableReason::AmbiguousEvidence)
        }
    }
}

fn parse_raw_rename_candidate(
    value: Value,
) -> std::result::Result<Option<RawRenameCandidate>, RenameCandidateUnavailableReason> {
    if value.is_null() {
        return Ok(None);
    }
    let (range, placeholder) = match serde_json::from_value::<PrepareRenameResponse>(value)
        .map_err(|_| RenameCandidateUnavailableReason::AmbiguousEvidence)?
    {
        PrepareRenameResponse::Range(range) => (range, None),
        PrepareRenameResponse::RangeWithPlaceholder { range, placeholder }
            if !placeholder.is_empty() =>
        {
            (range, Some(placeholder))
        }
        PrepareRenameResponse::RangeWithPlaceholder { .. }
        | PrepareRenameResponse::DefaultBehavior { .. } => {
            return Err(RenameCandidateUnavailableReason::AmbiguousEvidence);
        }
    };
    let range = crate::daemon::lsp_gateway::LspRange {
        start: LspPosition {
            line: range.start.line,
            character: range.start.character,
        },
        end: LspPosition {
            line: range.end.line,
            character: range.end.character,
        },
    };
    Ok(Some(RawRenameCandidate { range, placeholder }))
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ProviderRequestKey {
    root_uri: String,
    request_id: LspRequestId,
}

struct StdioGraphSemanticProvider {
    upstream: Option<Arc<Pr12SemanticProviderAdapter>>,
    graph: Arc<Pr12SemanticProviderAdapter>,
    rename: Option<Arc<Pr12SemanticProviderAdapter>>,
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
        if matches!(request, SemanticRequest::RenameCandidate { .. }) {
            return self.rename.as_ref().map_or_else(
                || {
                    SemanticProviderOutcome::Complete(SemanticResponse::RenameCandidate(
                        RenameCandidateResult::Unavailable {
                            reason: RenameCandidateUnavailableReason::AnalyzerUnavailable,
                        },
                    ))
                },
                |rename| SemanticProviderPort::request(rename, root, request_id, request),
            );
        }
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
                    detail: None,
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
                        detail: None,
                    }
                });
            }
        };
        if !inserted {
            return Box::pin(async {
                LspSemanticOperationOutcome::Partial {
                    value: Value::Null,
                    coverage: "graph-duplicate-operation".to_owned(),
                    detail: None,
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
                    detail: None,
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
            LspSemanticRequest::PrepareRename(params) => {
                let node = node_at_position(
                    database,
                    project_root,
                    params.text_document.uri.as_str(),
                    params.position.line,
                    params.position.character,
                )
                .await?;
                Ok(GraphProjection::complete(match node {
                    Some(node) => match node_identifier_range(project_root, &node)? {
                        Some(range) => {
                            json!({
                                    "range": {
                                        "start": {
                                            "line": range.start.line,
                                            "character": range.start.character,
                                        },
                                        "end": {
                                            "line": range.end.line,
                                            "character": range.end.character,
                                        },
                                    },
                                "placeholder": node.name,
                            })
                        }
                        None => Value::Null,
                    },
                    None => Value::Null,
                }))
            }
        }
    }
    .await;
    graph_projection_outcome(result)
}

fn graph_projection_outcome(result: Result<GraphProjection>) -> LspSemanticOperationOutcome {
    match result {
        Ok(projection) if projection.omitted == 0 => {
            LspSemanticOperationOutcome::Complete(projection.value)
        }
        Ok(projection) => LspSemanticOperationOutcome::Partial {
            value: projection.value,
            coverage: "graph-results-truncated".to_owned(),
            detail: None,
        },
        Err(error) => {
            eprintln!("[tracedecay] event=graph_semantic_read_failed error={error}");
            LspSemanticOperationOutcome::Partial {
                value: Value::Null,
                coverage: "graph-read-failed".to_owned(),
                detail: Some(LspSemanticOperationOutcome::GRAPH_READ_FAILED_DETAIL),
            }
        }
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

fn node_identifier_range(
    project_root: &Path,
    node: &Node,
) -> Result<Option<crate::daemon::lsp_gateway::LspRange>> {
    let document_path = scoped_document_path(project_root, &node.file_path)?;
    let text =
        crate::sync::read_source_file(&document_path).map_err(|error| TraceDecayError::Config {
            message: format!("failed to read semantic document: {error}"),
        })?;
    let line_start = if node.start_line == 0 {
        Some(0)
    } else {
        text.match_indices('\n')
            .nth(node.start_line.saturating_sub(1) as usize)
            .map(|(offset, _)| offset.saturating_add(1))
    };
    let Some(line_start) = line_start else {
        return Ok(None);
    };
    let line_end = text[line_start..]
        .find('\n')
        .map_or(text.len(), |offset| line_start.saturating_add(offset));
    let search_start = line_start.saturating_add(node.start_column as usize);
    if search_start > line_end || !text.is_char_boundary(search_start) {
        return Ok(None);
    }
    let declaration_line = &text[search_start..line_end];
    let identifier = declaration_line
        .match_indices(&node.name)
        .find(|(offset, matched)| {
            let before = declaration_line[..*offset].chars().next_back();
            let after = declaration_line[offset.saturating_add(matched.len())..]
                .chars()
                .next();
            !before.is_some_and(identifier_character) && !after.is_some_and(identifier_character)
        })
        .map(|(offset, matched)| {
            let start = search_start.saturating_add(offset);
            (start, start.saturating_add(matched.len()))
        });
    let Some((start, end)) = identifier else {
        return Ok(None);
    };
    Ok(Some(crate::daemon::lsp_gateway::LspRange {
        start: byte_offset_to_utf16_position(&text, start).map_err(|error| {
            TraceDecayError::Config {
                message: format!("invalid graph rename start position: {error:?}"),
            }
        })?,
        end: byte_offset_to_utf16_position(&text, end).map_err(|error| {
            TraceDecayError::Config {
                message: format!("invalid graph rename end position: {error:?}"),
            }
        })?,
    }))
}

fn identifier_character(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn partial_coverage(outcome: LspSemanticOperationOutcome) -> String {
        match outcome {
            LspSemanticOperationOutcome::Partial { coverage, .. } => coverage,
            outcome => panic!("expected partial semantic outcome, got {outcome:?}"),
        }
    }

    #[test]
    fn graph_coverage_ignores_errors_and_omission_counts() {
        for omitted in [1, 37, usize::MAX] {
            assert_eq!(
                partial_coverage(graph_projection_outcome(Ok(GraphProjection {
                    value: json!([]),
                    omitted,
                }))),
                "graph-results-truncated"
            );
        }

        for message in [
            "stale graph: Bearer super-secret-token!",
            "file:///home/alice/private.rs?credential=hunter2",
            r"C:\Users\alice\private.rs: failed?!",
        ] {
            assert_eq!(
                partial_coverage(graph_projection_outcome(Err(TraceDecayError::Config {
                    message: message.to_owned(),
                }))),
                "graph-read-failed"
            );
        }
    }

    #[test]
    fn graph_failure_detail_never_copies_raw_errors() {
        let sensitive = concat!(
            "graph stderr\n",
            "Authorization: Bearer bearer-secret\n",
            "file://alice:hunter2@localhost/home/alice/private.rs\n",
            r"C:\Users\alice\private.rs",
            "\nUTF-8: 密碼 🔐"
        );
        let LspSemanticOperationOutcome::Partial {
            coverage, detail, ..
        } = graph_projection_outcome(Err(TraceDecayError::Config {
            message: sensitive.to_owned(),
        }))
        else {
            panic!("expected graph read failure");
        };

        assert_eq!(coverage, "graph-read-failed");
        assert_eq!(
            detail,
            Some(LspSemanticOperationOutcome::GRAPH_READ_FAILED_DETAIL)
        );
        for forbidden in [
            "bearer-secret",
            "alice:hunter2",
            "/home/alice",
            r"C:\Users\alice",
            "密碼",
            "🔐",
        ] {
            assert!(
                !detail
                    .expect("typed graph failure detail")
                    .contains(forbidden),
                "caller detail leaked {forbidden}"
            );
        }
    }

    struct RecordingSemanticProvider {
        requests: AtomicUsize,
    }

    impl RecordingSemanticProvider {
        fn new() -> Self {
            Self {
                requests: AtomicUsize::new(0),
            }
        }

        fn requests(&self) -> usize {
            self.requests.load(Ordering::SeqCst)
        }
    }

    impl SemanticProviderPort for RecordingSemanticProvider {
        fn request(
            &self,
            _root: &AdmittedRoot,
            _request_id: &LspRequestId,
            _request: &SemanticRequest,
        ) -> SemanticProviderOutcome<SemanticResponse> {
            self.requests.fetch_add(1, Ordering::SeqCst);
            SemanticProviderOutcome::Complete(SemanticResponse::Hover(None))
        }
    }

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
            updated_at: 0,
            parent_id: None,
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
    fn polyglot_semantics_route_each_document_to_its_language_provider() {
        let rust = Arc::new(RecordingSemanticProvider::new());
        let typescript = Arc::new(RecordingSemanticProvider::new());
        let fallback = Arc::new(RecordingSemanticProvider::new());
        let provider = PolyglotSemanticProvider::new(
            vec![
                LanguageSemanticRoute::new(["rs"], rust.clone()),
                LanguageSemanticRoute::new(["ts", "tsx"], typescript.clone()),
            ],
            fallback.clone(),
        );
        let root = AdmittedRoot::new("file:///workspace");

        for document_uri in [
            "file:///workspace/src/lib.rs",
            "file:///workspace/dashboard/src/app.tsx",
        ] {
            assert!(matches!(
                provider.request(
                    &root,
                    &LspRequestId::Number(1),
                    &SemanticRequest::Hover {
                        document_uri: document_uri.to_owned(),
                        position: LspPosition {
                            line: 0,
                            character: 0,
                        },
                    },
                ),
                SemanticProviderOutcome::Complete(SemanticResponse::Hover(None))
            ));
        }

        assert_eq!(rust.requests(), 1);
        assert_eq!(typescript.requests(), 1);
        assert_eq!(fallback.requests(), 0);
    }

    #[test]
    fn polyglot_semantics_fail_closed_to_graph_for_ambiguous_extensions() {
        let first = Arc::new(RecordingSemanticProvider::new());
        let second = Arc::new(RecordingSemanticProvider::new());
        let fallback = Arc::new(RecordingSemanticProvider::new());
        let provider = PolyglotSemanticProvider::new(
            vec![
                LanguageSemanticRoute::new(["h"], first.clone()),
                LanguageSemanticRoute::new(["h"], second.clone()),
            ],
            fallback.clone(),
        );
        let root = AdmittedRoot::new("file:///workspace");

        let _ = provider.request(
            &root,
            &LspRequestId::Number(1),
            &SemanticRequest::DocumentSymbols {
                document_uri: "file:///workspace/include/shared.h".to_owned(),
            },
        );

        assert_eq!(first.requests(), 0);
        assert_eq!(second.requests(), 0);
        assert_eq!(fallback.requests(), 1);
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

    #[test]
    fn graph_rename_evidence_uses_the_identifier_not_the_full_node_span() {
        let fixture = tempfile::tempdir().expect("fixture");
        let source = fixture.path().join("src");
        std::fs::create_dir(&source).expect("source");
        std::fs::write(source.join("lib.rs"), "fn 名称(名称: usize) {}\n").expect("document");
        let mut node = function_node("名称", 0, 27);
        node.end_column = 27;

        let range = node_identifier_range(fixture.path(), &node)
            .expect("range projection")
            .expect("identifier");
        assert_eq!(
            range,
            crate::daemon::lsp_gateway::LspRange {
                start: LspPosition {
                    line: 0,
                    character: 3,
                },
                end: LspPosition {
                    line: 0,
                    character: 5,
                },
            }
        );
    }

    #[test]
    fn rename_candidate_requires_exact_analyzer_graph_agreement() {
        let candidate = json!({
            "range": {
                "start": { "line": 0, "character": 3 },
                "end": { "line": 0, "character": 7 }
            },
            "placeholder": "name"
        });
        assert!(matches!(
            merge_rename_candidate_outcomes(
                "file:///root/src/lib.rs",
                LspSemanticOperationOutcome::Complete(candidate.clone()),
                LspSemanticOperationOutcome::Complete(candidate),
            ),
            RenameCandidateResult::Available(RenameCandidate {
                placeholder,
                ..
            }) if placeholder == "name"
        ));

        let disagreement = merge_rename_candidate_outcomes(
            "file:///root/src/lib.rs",
            LspSemanticOperationOutcome::Complete(json!({
                "range": {
                    "start": { "line": 0, "character": 3 },
                    "end": { "line": 0, "character": 7 }
                },
                "placeholder": "analyzer_name"
            })),
            LspSemanticOperationOutcome::Complete(json!({
                "range": {
                    "start": { "line": 0, "character": 3 },
                    "end": { "line": 0, "character": 7 }
                },
                "placeholder": "graph_name"
            })),
        );
        assert_eq!(
            disagreement,
            RenameCandidateResult::Unavailable {
                reason: RenameCandidateUnavailableReason::AmbiguousEvidence,
            }
        );
    }

    #[test]
    fn stale_or_absent_analyzer_rename_evidence_is_typed_unavailable() {
        assert_eq!(
            merge_rename_candidate_outcomes(
                "file:///root/src/lib.rs",
                LspSemanticOperationOutcome::Partial {
                    value: Value::Null,
                    coverage: "analyzer-stale-result".to_owned(),
                    detail: None,
                },
                LspSemanticOperationOutcome::Complete(Value::Null),
            ),
            RenameCandidateResult::Unavailable {
                reason: RenameCandidateUnavailableReason::StaleEvidence,
            }
        );
        assert_eq!(
            merge_rename_candidate_outcomes(
                "file:///root/src/lib.rs",
                LspSemanticOperationOutcome::Unavailable,
                LspSemanticOperationOutcome::Complete(Value::Null),
            ),
            RenameCandidateResult::Unavailable {
                reason: RenameCandidateUnavailableReason::AnalyzerUnavailable,
            }
        );
        assert_eq!(
            merge_rename_candidate_outcomes(
                "file:///root/src/lib.rs",
                LspSemanticOperationOutcome::Complete(Value::Null),
                LspSemanticOperationOutcome::Complete(json!({
                    "range": {
                        "start": { "line": 0, "character": 3 },
                        "end": { "line": 0, "character": 7 }
                    },
                    "placeholder": "name"
                })),
            ),
            RenameCandidateResult::Unavailable {
                reason: RenameCandidateUnavailableReason::EvidenceAbsent,
            }
        );
    }
}
