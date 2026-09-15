//! Extended primitive port: module API, qualified names, diagnostics, and storage status history.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use sha2::{Digest, Sha256};
use tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1;
use tracedecay_contracts::retrieval::{
    HealthDeltaRequest, HealthDeltaResult, RetrievalPortContext, SymbolPrimitiveRecord,
};
use tracedecay_contracts::{EvidenceDomain, OmissionReason};
use tracedecay_domain::ProjectId;
use tracedecay_domain::canonical_text::encode_lowercase_hex;
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_graph_query::queries::GraphQueryManager;
use tracedecay_graph_query::{
    CodeGraphProjectionReadPort, CodeGraphReadRequest, SourceReadContext,
    request_graph_cancellation,
};
use tracedecay_runtime_core::db::Database;

use super::super::runtime::{
    CallChainPrimitiveRequest, CallChainPrimitiveResult, DiagnosticPrimitiveRecord,
    DiagnosticsPrimitiveRequest, DiagnosticsPrimitiveResult, ExtendedPrimitiveFuture,
    ExtendedPrimitivePort, FileDependentsPrimitiveRequest, FileDependentsPrimitiveResult,
    ModuleApiPrimitiveRequest, ModuleApiPrimitiveResult, QualifiedNamePrimitiveRequest,
    QualifiedNamePrimitiveResult, SourceBodyPrimitiveRequest, SourceBodyPrimitiveResult,
    SourceOutlinePrimitiveRequest, SourceOutlinePrimitiveResult, StorageStatusHistoryPointV1,
    StorageStatusPrimitiveRequest, StorageStatusPrimitiveResult,
};
use super::super::symbol_graph::symbol_record;
use super::{
    AuthenticatedDiagnosticCursorAuthorityV1, DIAGNOSTIC_CURSOR_LANE_WORKSPACE,
    all_code_graph_symbols, completed, diagnostics_result, diagnostics_unavailable,
    evidence_unavailable, failed, graph_read_outcome, now_observed, open_code_graph,
};
use crate::diagnostics_publication::CodeIndexPublicationIdentityPortV1;
use crate::diagnostics_query::{DiagnosticPageRequest, DiagnosticQueryCoverage, DiagnosticsQuery};
use crate::graph_health_delta::compute_verified_health_delta;
use crate::lsp_runtime::LspCodeIndexProjectionIdentityPort;

pub(super) fn public_module_symbols(
    nodes: Vec<CodeGraphSymbolSummaryV1>,
    path: &str,
) -> Result<Vec<SymbolPrimitiveRecord>, ()> {
    let prefix = if path.ends_with('/') {
        path.to_owned()
    } else {
        format!("{path}/")
    };
    let mut pub_nodes: Vec<CodeGraphSymbolSummaryV1> = nodes
        .into_iter()
        .filter(|node| {
            let Some(metadata) = node.metadata.as_ref() else {
                return false;
            };
            let Some(file_path) = node
                .binding
                .as_ref()
                .and_then(|binding| binding.logical_path.as_deref())
            else {
                return false;
            };
            metadata.visibility == "public" && (file_path == path || file_path.starts_with(&prefix))
        })
        .collect();
    pub_nodes.sort_by(|left, right| {
        let left_path = left
            .binding
            .as_ref()
            .and_then(|binding| binding.logical_path.as_deref());
        let right_path = right
            .binding
            .as_ref()
            .and_then(|binding| binding.logical_path.as_deref());
        left_path.cmp(&right_path).then(
            left.metadata
                .as_ref()
                .map(|metadata| metadata.start_line)
                .cmp(&right.metadata.as_ref().map(|metadata| metadata.start_line)),
        )
    });
    pub_nodes
        .into_iter()
        .map(|node| symbol_record(node, None))
        .collect()
}

pub struct TraceDecayExtendedPrimitivePortV1 {
    source_runtime: Arc<SourceReadContext>,
    code_graph: Arc<dyn CodeGraphProjectionReadPort>,
    database: Database,
    observation_database: RegisteredGlobalDbLeaseV1,
    code_index: Arc<dyn LspCodeIndexProjectionIdentityPort>,
    diagnostic_identity: Arc<dyn CodeIndexPublicationIdentityPortV1>,
    diagnostic_cursors: AuthenticatedDiagnosticCursorAuthorityV1,
}

impl TraceDecayExtendedPrimitivePortV1 {
    pub(super) fn new(
        source_runtime: Arc<SourceReadContext>,
        code_graph: Arc<dyn CodeGraphProjectionReadPort>,
        database: Database,
        observation_database: RegisteredGlobalDbLeaseV1,
        code_index: Arc<dyn LspCodeIndexProjectionIdentityPort>,
        diagnostic_identity: Arc<dyn CodeIndexPublicationIdentityPortV1>,
        diagnostic_cursors: AuthenticatedDiagnosticCursorAuthorityV1,
    ) -> Self {
        Self {
            source_runtime,
            code_graph,
            database,
            observation_database,
            code_index,
            diagnostic_identity,
            diagnostic_cursors,
        }
    }
}

pub(super) const STORAGE_STATUS_HISTORY_REVISION_V1: u32 = 1;
pub(super) const MAX_STORAGE_STATUS_HISTORY_POINTS: usize = 128;

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DurableStorageStatusHistoryV1 {
    revision: u32,
    project_id: Option<String>,
    store_path: String,
    samples: Vec<StorageStatusHistoryPointV1>,
}

pub(super) fn storage_status_history_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub(super) fn storage_status_history_path(
    data_root: &Path,
    project_id: Option<&str>,
    store_path: &str,
) -> PathBuf {
    let mut digest = Sha256::new();
    digest.update(project_id.unwrap_or_default().as_bytes());
    digest.update([0]);
    digest.update(store_path.as_bytes());
    data_root
        .join("storage-status-history-v1")
        .join(format!("{}.json", encode_lowercase_hex(&digest.finalize())))
}

pub(super) fn update_storage_status_history(
    history_path: &Path,
    project_id: Option<String>,
    store_path: String,
    database_bytes: u64,
    observed_at: i64,
) -> (Vec<StorageStatusHistoryPointV1>, String) {
    update_storage_status_history_with_lock(
        storage_status_history_lock(),
        history_path,
        project_id,
        store_path,
        database_bytes,
        observed_at,
    )
}

pub(super) fn update_storage_status_history_with_lock(
    history_lock: &Mutex<()>,
    history_path: &Path,
    project_id: Option<String>,
    store_path: String,
    database_bytes: u64,
    observed_at: i64,
) -> (Vec<StorageStatusHistoryPointV1>, String) {
    // The production lock serializes the read-modify-write of one history
    // file, but is process-global: every project's storage-status read funnels
    // through it. A blocking acquire let one stalled write convoy every
    // concurrent status read daemon-wide, so contention degrades to the
    // current sample as a typed bounded state instead of waiting.
    let _guard = match history_lock.try_lock() {
        Ok(guard) => guard,
        Err(std::sync::TryLockError::WouldBlock) => {
            return (
                vec![StorageStatusHistoryPointV1 {
                    observed_at,
                    database_bytes,
                }],
                "current_sample_only_history_lock_contended".to_owned(),
            );
        }
        Err(std::sync::TryLockError::Poisoned(_)) => {
            return (
                vec![StorageStatusHistoryPointV1 {
                    observed_at,
                    database_bytes,
                }],
                "current_sample_only_history_lock_failed".to_owned(),
            );
        }
    };
    let stored = std::fs::read(history_path).ok();
    let restored = stored
        .as_deref()
        .and_then(|bytes| serde_json::from_slice::<DurableStorageStatusHistoryV1>(bytes).ok())
        .filter(|durable| {
            durable.revision == STORAGE_STATUS_HISTORY_REVISION_V1
                && durable.project_id == project_id
                && durable.store_path == store_path
                && durable
                    .samples
                    .windows(2)
                    .all(|pair| pair[0].observed_at <= pair[1].observed_at)
        });
    let invalid_stored_history = stored.is_some() && restored.is_none();
    let mut history = restored.map_or_else(Vec::new, |durable| durable.samples);
    let clock_regressed = history
        .last()
        .is_some_and(|sample| sample.observed_at > observed_at);
    if clock_regressed {
        history.clear();
    }
    // The series records store-size changes, not reads. Re-observing the same
    // size adds no information, and appending per read would both amplify
    // writes and make this evidence operation non-idempotent, so the same
    // authorized status read could never agree across CLI, MCP, and HTTP.
    let recorded = history
        .last()
        .is_none_or(|sample| sample.database_bytes != database_bytes);
    if recorded {
        history.push(StorageStatusHistoryPointV1 {
            observed_at,
            database_bytes,
        });
        if history.len() > MAX_STORAGE_STATUS_HISTORY_POINTS {
            history.drain(..history.len() - MAX_STORAGE_STATUS_HISTORY_POINTS);
        }
    }
    let persisted = !recorded
        || serde_json::to_vec_pretty(&DurableStorageStatusHistoryV1 {
            revision: STORAGE_STATUS_HISTORY_REVISION_V1,
            project_id,
            store_path,
            samples: history.clone(),
        })
        .ok()
        .and_then(|bytes| {
            std::fs::create_dir_all(history_path.parent().unwrap_or_else(|| Path::new(".")))
                .ok()?;
            let temp =
                history_path.with_extension(format!("tmp-{}-{observed_at}", std::process::id()));
            tracedecay_runtime_core::storage::PrivateStoreIo::write_file_atomically(
                history_path,
                &temp,
                &bytes,
            )
            .ok()
        })
        .is_some();
    let coverage = if !persisted {
        "current_sample_only_history_persistence_failed"
    } else if invalid_stored_history {
        "durable_project_store_history_reset_invalid"
    } else if clock_regressed {
        "durable_project_store_history_reset_clock_regression"
    } else {
        "durable_project_store_history"
    };
    (history, coverage.to_owned())
}

/// Canonical storage-status owner used by the application operation and its
/// dashboard projection. History is durable and scope-bound, so growth does
/// not reset when the daemon or dashboard restarts.
#[hotpath::measure(label = "usecases.primitives.storage_status", future = true)]
pub(crate) async fn canonical_storage_status(
    database: &Database,
    source_runtime: &SourceReadContext,
    project_id: &ProjectId,
    include_details: bool,
) -> StorageStatusPrimitiveResult {
    let read_only = source_runtime.is_read_only();
    let database_path = database.canonical_database_path();
    let store_path = database_path.display().to_string();
    let file_bytes = database_path.metadata().ok().map(|metadata| metadata.len());
    let page_counts = hotpath::future!(
        database.storage_page_counts(),
        label = "usecases.primitives.storage_status.page_counts"
    )
    .await
    .ok();
    let page_size_bytes = page_counts.and_then(|(page_size, _, _)| u32::try_from(page_size).ok());
    let page_count = page_counts.map(|(_, page_count, _)| page_count);
    let freelist_pages = page_counts.map(|(_, _, freelist_pages)| freelist_pages);
    let database_bytes = page_size_bytes
        .zip(page_count)
        .map(|(page_size, pages)| u64::from(page_size).saturating_mul(pages))
        .or(file_bytes);
    let project_id = Some(project_id.as_str().to_owned());
    let status = if database_path.is_file() {
        if read_only { "read_only" } else { "ok" }
    } else {
        "missing_graph_db"
    };
    let details = if include_details && page_counts.is_none() {
        vec!["project store page telemetry unavailable".to_owned()]
    } else {
        Vec::new()
    };
    let history_path = storage_status_history_path(
        database_path.parent().unwrap_or_else(|| Path::new(".")),
        project_id.as_deref(),
        &store_path,
    );
    let (history, history_coverage) = match database_bytes {
        None => (Vec::new(), "current_sample_unavailable".to_owned()),
        Some(bytes) => {
            let history_project_id = project_id.clone();
            let history_store_path = store_path.clone();
            let observed_at = now_observed().0;
            // History persistence is file I/O into the store directory; run it
            // on the blocking pool so a stalled filesystem cannot capture an
            // async executor thread for the daemon.
            hotpath::future!(
                tokio::task::spawn_blocking(move || {
                    update_storage_status_history(
                        &history_path,
                        history_project_id,
                        history_store_path,
                        bytes,
                        observed_at,
                    )
                }),
                label = "usecases.primitives.storage_status.history"
            )
            .await
            .unwrap_or_else(|_| {
                (
                    vec![StorageStatusHistoryPointV1 {
                        observed_at,
                        database_bytes: bytes,
                    }],
                    "current_sample_only_history_task_failed".to_owned(),
                )
            })
        }
    };
    StorageStatusPrimitiveResult {
        status: status.to_owned(),
        read_only,
        database_bytes,
        page_size_bytes,
        page_count,
        freelist_pages,
        details,
        project_id,
        store_path: Some(store_path),
        history,
        history_coverage: Some(history_coverage),
    }
}

impl ExtendedPrimitivePort for TraceDecayExtendedPrimitivePortV1 {
    fn qualified_name<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a QualifiedNamePrimitiveRequest,
    ) -> ExtendedPrimitiveFuture<'a, QualifiedNamePrimitiveResult> {
        Box::pin(hotpath::future!(
            async move {
                let cancellation = request_graph_cancellation(context.request);
                let reader = match open_code_graph(
                    self.code_graph.as_ref(),
                    context.request,
                    now_observed(),
                    Arc::clone(&cancellation),
                )
                .await
                {
                    Ok(reader) => reader,
                    Err(error) => {
                        return graph_read_outcome(&error, EvidenceDomain::Symbol, now_observed());
                    }
                };
                let Ok(nodes) = reader.resolve_qualified_name(
                    &request.qualified_name,
                    None,
                    10_000,
                    cancellation,
                ) else {
                    return failed(EvidenceDomain::Symbol, now_observed());
                };
                let symbols = nodes
                    .into_iter()
                    .map(|node| symbol_record(node, None))
                    .collect::<Result<Vec<_>, _>>();
                let Ok(symbols) = symbols else {
                    return failed(EvidenceDomain::Symbol, now_observed());
                };
                let total = symbols.len() as u64;
                completed(
                    QualifiedNamePrimitiveResult {
                        symbols,
                        total: Some(total),
                        next_cursor: None,
                    },
                    EvidenceDomain::Symbol,
                    now_observed(),
                )
            },
            label = "usecases.primitives.qualified_name"
        ))
    }

    fn call_chain<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CallChainPrimitiveRequest,
    ) -> ExtendedPrimitiveFuture<'a, CallChainPrimitiveResult> {
        Box::pin(hotpath::future!(
            async move {
                let cancellation = request_graph_cancellation(context.request);
                let Ok(reader) = open_code_graph(
                    self.code_graph.as_ref(),
                    context.request,
                    now_observed(),
                    Arc::clone(&cancellation),
                )
                .await
                else {
                    return failed(EvidenceDomain::Graph, now_observed());
                };
                let Ok(from) =
                    tracedecay_domain::SymbolOccurrenceId::new(request.from_node_id.clone())
                else {
                    return failed(EvidenceDomain::Graph, now_observed());
                };
                let Ok(to) = tracedecay_domain::SymbolOccurrenceId::new(request.to_node_id.clone())
                else {
                    return failed(EvidenceDomain::Graph, now_observed());
                };
                let Ok(path) = reader.shortest_path(
                    &from,
                    &to,
                    &[tracedecay_domain::RelationEdgeKindV1::Calls],
                    request.maximum_depth,
                    100_000,
                    cancellation,
                ) else {
                    return failed(EvidenceDomain::Graph, now_observed());
                };
                if !path.complete {
                    return evidence_unavailable(
                        EvidenceDomain::Graph,
                        now_observed(),
                        OmissionReason::Unavailable,
                        0,
                    );
                }
                let edges = path.path.unwrap_or_default();
                let mut node_ids = vec![from.as_str().to_owned()];
                node_ids.extend(
                    edges
                        .iter()
                        .map(|edge| edge.to_occurrence.as_str().to_owned()),
                );
                let edge_kinds = edges
                    .into_iter()
                    .map(|edge| edge.kind.as_str().to_owned())
                    .collect();
                completed(
                    CallChainPrimitiveResult {
                        node_ids,
                        edge_kinds,
                    },
                    EvidenceDomain::Graph,
                    now_observed(),
                )
            },
            label = "usecases.primitives.call_chain"
        ))
    }

    fn file_dependents<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a FileDependentsPrimitiveRequest,
    ) -> ExtendedPrimitiveFuture<'a, FileDependentsPrimitiveResult> {
        Box::pin(hotpath::future!(
            async move {
                let observed_at = now_observed();
                let cancellation = request_graph_cancellation(context.request);
                let verified = match self
                    .code_graph
                    .open(CodeGraphReadRequest::new(
                        context.request,
                        observed_at,
                        Arc::clone(&cancellation),
                    ))
                    .await
                {
                    Ok(verified) => verified,
                    Err(error) => {
                        return graph_read_outcome(&error, EvidenceDomain::Graph, observed_at);
                    }
                };
                let reader = match verified.reader_with_cancellation(
                    context.request,
                    observed_at,
                    Arc::clone(&cancellation),
                ) {
                    Ok(reader) => reader,
                    Err(error) => {
                        return graph_read_outcome(&error, EvidenceDomain::Graph, observed_at);
                    }
                };
                let query = GraphQueryManager::new(&reader, cancellation);
                let Ok(dependent_files) = query.get_file_dependents(&request.file).await else {
                    return evidence_unavailable(
                        EvidenceDomain::Graph,
                        now_observed(),
                        OmissionReason::Unavailable,
                        0,
                    );
                };
                completed(
                    FileDependentsPrimitiveResult {
                        file: request.file.clone(),
                        dependent_files,
                    },
                    EvidenceDomain::Graph,
                    now_observed(),
                )
            },
            label = "usecases.primitives.file_dependents"
        ))
    }

    fn source_body<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a SourceBodyPrimitiveRequest,
    ) -> ExtendedPrimitiveFuture<'a, SourceBodyPrimitiveResult> {
        Box::pin(hotpath::future!(
            async move {
                let cancellation = request_graph_cancellation(context.request);
                let Ok(reader) = open_code_graph(
                    self.code_graph.as_ref(),
                    context.request,
                    now_observed(),
                    Arc::clone(&cancellation),
                )
                .await
                else {
                    return failed(EvidenceDomain::Source, now_observed());
                };
                let Ok(occurrence) =
                    tracedecay_domain::SymbolOccurrenceId::new(request.node_id.clone())
                else {
                    return failed(EvidenceDomain::Source, now_observed());
                };
                let Ok(Some(node)) = reader.symbol_summary(&occurrence, cancellation) else {
                    return failed(EvidenceDomain::Source, now_observed());
                };
                let Some(metadata) = node.metadata else {
                    return failed(EvidenceDomain::Source, now_observed());
                };
                let Some(file) = node.binding.and_then(|binding| binding.logical_path) else {
                    return failed(EvidenceDomain::Source, now_observed());
                };
                let Some(line_span) = metadata.line_span.checked_sub(1) else {
                    return failed(EvidenceDomain::Source, now_observed());
                };
                let Some(end_line) = metadata.start_line.checked_add(line_span) else {
                    return failed(EvidenceDomain::Source, now_observed());
                };
                let path = self.source_runtime.project_root().join(&file);
                let Ok(content) = tokio::fs::read_to_string(&path).await else {
                    return failed(EvidenceDomain::Source, now_observed());
                };
                let start = metadata.start_line as usize;
                let end = end_line as usize;
                let body = content
                    .lines()
                    .skip(start)
                    .take(end.saturating_sub(start).saturating_add(1))
                    .collect::<Vec<_>>()
                    .join("\n");
                completed(
                    SourceBodyPrimitiveResult {
                        node_id: occurrence.as_str().to_owned(),
                        file,
                        start_line: metadata.start_line.saturating_add(1),
                        end_line: end_line.saturating_add(1),
                        body,
                    },
                    EvidenceDomain::Source,
                    now_observed(),
                )
            },
            label = "usecases.primitives.source_body"
        ))
    }

    fn source_outline<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a SourceOutlinePrimitiveRequest,
    ) -> ExtendedPrimitiveFuture<'a, SourceOutlinePrimitiveResult> {
        Box::pin(hotpath::future!(
            async move {
                let cancellation = request_graph_cancellation(context.request);
                let Ok(reader) = open_code_graph(
                    self.code_graph.as_ref(),
                    context.request,
                    now_observed(),
                    Arc::clone(&cancellation),
                )
                .await
                else {
                    return failed(EvidenceDomain::Source, now_observed());
                };
                let Ok(nodes) =
                    reader.symbols_in_logical_file(&request.file, 100_000, cancellation)
                else {
                    return failed(EvidenceDomain::Source, now_observed());
                };
                let symbols = nodes
                    .into_iter()
                    .map(|node| symbol_record(node, None))
                    .collect::<Result<Vec<_>, _>>();
                let Ok(symbols) = symbols else {
                    return failed(EvidenceDomain::Source, now_observed());
                };
                completed(
                    SourceOutlinePrimitiveResult {
                        file: request.file.clone(),
                        symbols,
                    },
                    EvidenceDomain::Source,
                    now_observed(),
                )
            },
            label = "usecases.primitives.source_outline"
        ))
    }

    fn module_api<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a ModuleApiPrimitiveRequest,
    ) -> ExtendedPrimitiveFuture<'a, ModuleApiPrimitiveResult> {
        Box::pin(hotpath::future!(
            async move {
                let cancellation = request_graph_cancellation(context.request);
                let Ok(reader) = open_code_graph(
                    self.code_graph.as_ref(),
                    context.request,
                    now_observed(),
                    Arc::clone(&cancellation),
                )
                .await
                else {
                    return failed(EvidenceDomain::Symbol, now_observed());
                };
                let Ok(nodes) = all_code_graph_symbols(&reader, cancellation) else {
                    return failed(EvidenceDomain::Symbol, now_observed());
                };
                let Ok(symbols) = public_module_symbols(nodes, &request.path) else {
                    return failed(EvidenceDomain::Symbol, now_observed());
                };
                completed(
                    ModuleApiPrimitiveResult {
                        path: request.path.clone(),
                        symbols,
                    },
                    EvidenceDomain::Symbol,
                    now_observed(),
                )
            },
            label = "usecases.primitives.module_api"
        ))
    }

    fn health_delta<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a HealthDeltaRequest,
    ) -> ExtendedPrimitiveFuture<'a, HealthDeltaResult> {
        Box::pin(hotpath::future!(
            async move {
                let observed_at = now_observed();
                let cancellation = request_graph_cancellation(context.request);
                let verified = match self
                    .code_graph
                    .open(CodeGraphReadRequest::new(
                        context.request,
                        observed_at,
                        Arc::clone(&cancellation),
                    ))
                    .await
                {
                    Ok(verified) => verified,
                    Err(error) => {
                        return graph_read_outcome(
                            &error,
                            EvidenceDomain::Operational,
                            observed_at,
                        );
                    }
                };
                let reader = match verified.reader_with_cancellation(
                    context.request,
                    observed_at,
                    Arc::clone(&cancellation),
                ) {
                    Ok(reader) => reader,
                    Err(error) => {
                        return graph_read_outcome(
                            &error,
                            EvidenceDomain::Operational,
                            observed_at,
                        );
                    }
                };
                let query = GraphQueryManager::new(&reader, cancellation);
                match compute_verified_health_delta(
                    Some(context.request.scope().project_id.as_str().to_owned()),
                    &query,
                    self.observation_database.as_ref(),
                    request.before_cursor.as_deref(),
                    request.path_prefix.as_deref(),
                )
                .await
                {
                    Ok(result) => completed(result, EvidenceDomain::Operational, now_observed()),
                    Err(_) => evidence_unavailable(
                        EvidenceDomain::Operational,
                        observed_at,
                        OmissionReason::Unavailable,
                        0,
                    ),
                }
            },
            label = "usecases.primitives.health_delta"
        ))
    }

    fn storage_status<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a StorageStatusPrimitiveRequest,
    ) -> ExtendedPrimitiveFuture<'a, StorageStatusPrimitiveResult> {
        Box::pin(hotpath::future!(
            async move {
                completed(
                    canonical_storage_status(
                        &self.database,
                        self.source_runtime.as_ref(),
                        &context.request.scope().project_id,
                        request.include_details,
                    )
                    .await,
                    EvidenceDomain::Operational,
                    now_observed(),
                )
            },
            label = "usecases.primitives.storage_status"
        ))
    }

    fn diagnostics<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a DiagnosticsPrimitiveRequest,
    ) -> ExtendedPrimitiveFuture<'a, DiagnosticsPrimitiveResult> {
        Box::pin(hotpath::future!(
            async move {
                let finished_at = now_observed();
                if !(1..=1_000).contains(&request.maximum_diagnostics) {
                    return diagnostics_unavailable(finished_at, OmissionReason::Unsupported);
                }
                let query = DiagnosticsQuery::new(self.database.clone());
                let current = query.current_generation().await;
                let Some(current_generation) = current.generation else {
                    // No diagnostic publication means there is no retained
                    // source identity to validate yet.
                    return diagnostics_unavailable(finished_at, OmissionReason::Unsupported);
                };
                if !matches!(current.coverage, DiagnosticQueryCoverage::Complete) {
                    return diagnostics_unavailable(finished_at, OmissionReason::Unavailable);
                }
                let Some(identity) = self
                    .diagnostic_identity
                    .resolve(self.source_runtime.project_root().to_path_buf())
                    .await
                else {
                    return diagnostics_unavailable(finished_at, OmissionReason::Unavailable);
                };
                let scope = context.request.scope();
                if identity.repository() != &scope.repository_id
                    || identity.worktree() != Some(&scope.worktree_id)
                    || identity.reference() != scope.reference.as_ref()
                {
                    return diagnostics_unavailable(finished_at, OmissionReason::Stale);
                }
                let document_path = match &request.scope {
                    super::super::runtime::DiagnosticsPrimitiveScope::Workspace => None,
                    super::super::runtime::DiagnosticsPrimitiveScope::File(path) => {
                        let Some(path) = crate::diagnostics_publication::code_index_logical_path(
                            self.source_runtime.project_root(),
                            path,
                        ) else {
                            return diagnostics_unavailable(
                                finished_at,
                                OmissionReason::Unavailable,
                            );
                        };
                        if identity.file(&path).is_none() {
                            return diagnostics_unavailable(
                                finished_at,
                                OmissionReason::Unavailable,
                            );
                        }
                        Some(path)
                    }
                    super::super::runtime::DiagnosticsPrimitiveScope::Package(_) => {
                        return diagnostics_unavailable(finished_at, OmissionReason::Unsupported);
                    }
                };
                let current_index = match self
                    .code_index
                    .current_identity(
                        self.source_runtime.project_root().to_path_buf(),
                        document_path.clone(),
                    )
                    .await
                {
                    Ok(identity) => identity,
                    Err(_) => {
                        return diagnostics_unavailable(finished_at, OmissionReason::Unavailable);
                    }
                };
                if current_index.code_generation_id != *identity.generation_id() {
                    return diagnostics_unavailable(finished_at, OmissionReason::Stale);
                }
                if current_generation != *identity.generation_id() {
                    return diagnostics_unavailable(finished_at, OmissionReason::Stale);
                }
                let selected_file = document_path
                    .as_deref()
                    .and_then(|path| identity.file(path).map(|(file, _)| file));
                let cursor_lane = selected_file.map_or(
                    DIAGNOSTIC_CURSOR_LANE_WORKSPACE,
                    tracedecay_domain::FileOccurrenceId::as_str,
                );
                let cursor = match request.cursor.as_deref() {
                    Some(cursor) => match self.diagnostic_cursors.decode(
                        cursor,
                        context.request,
                        &current_generation,
                        cursor_lane,
                    ) {
                        Ok(cursor) => Some(cursor),
                        Err(()) => {
                            return diagnostics_unavailable(
                                finished_at,
                                OmissionReason::Unsupported,
                            );
                        }
                    },
                    None => None,
                };
                let page_request =
                    DiagnosticPageRequest::new(request.maximum_diagnostics as usize, cursor);
                let page = match selected_file {
                    Some(file) => {
                        query
                            .current_by_file(&current_generation, file, &page_request)
                            .await
                    }
                    None => {
                        query
                            .current_by_generation(&current_generation, &page_request)
                            .await
                    }
                };
                let Ok(page) = page else {
                    return diagnostics_unavailable(finished_at, OmissionReason::Unavailable);
                };
                match page.coverage {
                    DiagnosticQueryCoverage::Complete | DiagnosticQueryCoverage::Truncated => {}
                    DiagnosticQueryCoverage::StoreUnavailable { .. } => {
                        return diagnostics_unavailable(finished_at, OmissionReason::Unavailable);
                    }
                }
                let next_cursor = page
                    .next_cursor
                    .as_ref()
                    .map(|cursor| {
                        self.diagnostic_cursors.encode(
                            cursor,
                            context.request,
                            &current_generation,
                            cursor_lane,
                        )
                    })
                    .transpose();
                let Ok(next_cursor) = next_cursor else {
                    return diagnostics_unavailable(finished_at, OmissionReason::Unavailable);
                };
                let mut diagnostics = Vec::new();
                for diagnostic in page.records {
                    if diagnostic.repository != *identity.repository()
                        || diagnostic.worktree.as_ref() != identity.worktree()
                        || diagnostic.reference.as_ref() != identity.reference()
                        || diagnostic.source_revision.as_ref() != identity.source_revision()
                        || diagnostic.generation_id != *identity.generation_id()
                        || !diagnostic.is_current()
                    {
                        return diagnostics_unavailable(finished_at, OmissionReason::Stale);
                    }
                    let Some((logical_path, expected_digest)) = identity
                        .logical_path(&diagnostic.file_occurrence_id)
                        .and_then(|path| identity.file(path).map(|(_, digest)| (path, digest)))
                    else {
                        return diagnostics_unavailable(finished_at, OmissionReason::Stale);
                    };
                    if expected_digest != &diagnostic.content_digest {
                        return diagnostics_unavailable(finished_at, OmissionReason::Stale);
                    }
                    if selected_file.is_none_or(|file| file == &diagnostic.file_occurrence_id) {
                        diagnostics.push(DiagnosticPrimitiveRecord {
                            logical_path: logical_path.to_owned(),
                            diagnostic,
                        });
                    }
                }
                diagnostics_result(
                    identity.generation_id().clone(),
                    current_index.snapshot_digest,
                    diagnostics,
                    page.total as u64,
                    next_cursor,
                    finished_at,
                )
            },
            label = "usecases.primitives.diagnostics"
        ))
    }
}

/// How many tables the storage detail lines name before summarising the rest.
#[cfg(test)]
pub(super) const STORAGE_TABLE_DETAIL_LIMIT: usize = 10;

/// Renders per-table byte attribution for the graph store.
///
/// Without this, a store total is one opaque number and no claim about which
/// table holds the bytes can be reproduced through the product. A read the
/// runtime cannot serve reports that it could not be sampled, never an absent
/// or zero line that would read as "no table holds any bytes".
#[cfg(test)]
pub(super) fn largest_table_details(
    tables: tracedecay_domain::errors::Result<Vec<(String, u64)>>,
) -> Vec<String> {
    let mut tables = match tables {
        Ok(tables) => tables,
        Err(error) => return vec![format!("table sizes could not be sampled: {error}")],
    };
    if tables.is_empty() {
        return vec!["table sizes reported no tables".to_owned()];
    }
    tables.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let total: u64 = tables.iter().map(|(_, bytes)| bytes).sum();
    let remainder = tables.len().saturating_sub(STORAGE_TABLE_DETAIL_LIMIT);
    let mut details = vec![format!(
        "table bytes total {total} across {} tables",
        tables.len()
    )];
    details.extend(
        tables
            .iter()
            .take(STORAGE_TABLE_DETAIL_LIMIT)
            .map(|(table, bytes)| format!("table {table} holds {bytes} bytes")),
    );
    if remainder > 0 {
        details.push(format!("{remainder} smaller tables not listed"));
    }
    details
}
