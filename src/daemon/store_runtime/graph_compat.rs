use std::future::Future;
use std::pin::Pin;

use tracedecay_store::{
    CodeShardScopeV1, ConsistencyModeV1, GraphNodeV1, GraphSearchResultV1, GraphSearchScoreV1,
    GraphStatsV1, RuntimeCancellationStageV1, RuntimeInterruptionV1, RuntimeReadCoverageV1,
    RuntimeReadOperationV1, RuntimeReadOutcomeV1, RuntimeReadRequestV1, RuntimeReadResultV1,
    RuntimeRequestProbeV1, RuntimeSubmitOutcomeV1, RuntimeSubmitRequestV1,
    StorageRuntimeContractErrorV1, StorageRuntimeErrorV1, StorageRuntimePortErrorV1,
    StorageRuntimePortFutureV1, StorageRuntimePortResultV1, StorageRuntimeReadPort,
    StorageRuntimeSubmitPort, StoreRuntimeBindingV1, StoreShardScopeV1, UnavailableReasonV1,
};

use crate::db::Database;
use crate::db::runtime_compat::GraphStoreCompat;
use crate::errors::{Result as LegacyResult, TraceDecayError};
use crate::types::{GraphStats, Node, SearchResult};

type LegacyFuture<'a, T> = Pin<Box<dyn Future<Output = LegacyResult<T>> + Send + 'a>>;

/// The narrow existing graph API consumed by the runtime adapter.
///
/// Keeping this private and typed permits deterministic fake tests without
/// exposing a query language or allowing the adapter to open another store.
trait GraphReadBackend: Send + Sync {
    fn load_stats(&self) -> LegacyFuture<'_, GraphStats>;
    fn load_node<'a>(&'a self, node_id: &'a str) -> LegacyFuture<'a, Option<Node>>;
    fn search<'a>(&'a self, query: &'a str, limit: usize) -> LegacyFuture<'a, Vec<SearchResult>>;
    fn quick_check(&self) -> LegacyFuture<'_, bool>;
}

impl GraphReadBackend for GraphStoreCompat<'_> {
    fn load_stats(&self) -> LegacyFuture<'_, GraphStats> {
        Box::pin(self.get_stats())
    }

    fn load_node<'a>(&'a self, node_id: &'a str) -> LegacyFuture<'a, Option<Node>> {
        Box::pin(self.get_node_by_id(node_id))
    }

    fn search<'a>(&'a self, query: &'a str, limit: usize) -> LegacyFuture<'a, Vec<SearchResult>> {
        Box::pin(self.search_nodes(query, limit))
    }

    fn quick_check(&self) -> LegacyFuture<'_, bool> {
        Box::pin(self.quick_check())
    }
}

/// S1 port adapter for an already-open, daemon-authority-owned graph database.
///
/// `binding` is supplied by the owning runtime; this adapter never infers shard
/// identity from a path or mutable label. The legacy graph database exposes no
/// canonical runtime watermark or durable receipt/idempotency ledger. Graph
/// reads therefore serve only honest `LatestAvailable` requests without an
/// asserted commit position, and every submit returns typed unsupported.
pub(crate) struct GraphRuntimeCompat<'db> {
    backend: GraphStoreCompat<'db>,
    binding: StoreRuntimeBindingV1,
}

impl<'db> GraphRuntimeCompat<'db> {
    pub(crate) fn new(database: &'db Database, binding: StoreRuntimeBindingV1) -> Self {
        Self {
            backend: GraphStoreCompat::new(database),
            binding,
        }
    }
}

impl StorageRuntimeReadPort for GraphRuntimeCompat<'_> {
    fn dispatch_read<'a>(
        &'a self,
        request: RuntimeReadRequestV1,
        probe: &'a dyn RuntimeRequestProbeV1,
    ) -> StorageRuntimePortFutureV1<'a, RuntimeReadOutcomeV1> {
        dispatch_graph_read(&self.backend, &self.binding, request, probe)
    }
}

impl StorageRuntimeSubmitPort for GraphRuntimeCompat<'_> {
    fn dispatch_submit<'a>(
        &'a self,
        request: RuntimeSubmitRequestV1,
        probe: &'a dyn RuntimeRequestProbeV1,
    ) -> StorageRuntimePortFutureV1<'a, RuntimeSubmitOutcomeV1> {
        Box::pin(async move {
            validate_submit_dispatch(&request, probe)?;
            if let Some(outcome) = submit_binding_outcome(&self.binding, request.binding()) {
                return Ok(outcome);
            }
            if let Some(outcome) =
                submit_interruption(&request, probe, RuntimeCancellationStageV1::BeforeCommit)
            {
                return Ok(outcome);
            }
            Ok(RuntimeSubmitOutcomeV1::Unavailable {
                reason: UnavailableReasonV1::UnsupportedOperation,
            })
        })
    }
}

fn dispatch_graph_read<'a>(
    backend: &'a dyn GraphReadBackend,
    binding: &'a StoreRuntimeBindingV1,
    request: RuntimeReadRequestV1,
    probe: &'a dyn RuntimeRequestProbeV1,
) -> StorageRuntimePortFutureV1<'a, RuntimeReadOutcomeV1> {
    Box::pin(async move {
        validate_read_dispatch(&request, probe)?;
        if let Some(reason) = binding_unavailable_reason(binding, request.binding()) {
            return unavailable_read(reason);
        }
        if !matches!(
            request.operation(),
            RuntimeReadOperationV1::GraphStats
                | RuntimeReadOperationV1::GraphNode { .. }
                | RuntimeReadOperationV1::GraphSearch { .. }
                | RuntimeReadOperationV1::GraphQuickCheck
        ) {
            return unavailable_read(UnavailableReasonV1::UnsupportedOperation);
        }
        match request.consistency() {
            ConsistencyModeV1::LatestAvailable => {}
            ConsistencyModeV1::AtLeast { .. } => {
                return unavailable_read(UnavailableReasonV1::WatermarkNotReached);
            }
            ConsistencyModeV1::ExactSnapshot { .. } => {
                return unavailable_read(UnavailableReasonV1::SnapshotNotRetained);
            }
            ConsistencyModeV1::FrozenWatermarkVector { .. } => {
                return unavailable_read(UnavailableReasonV1::MissingAuthority);
            }
        }
        if let Some(outcome) = read_interruption(probe)? {
            return Ok(outcome);
        }

        let value = match request.operation() {
            RuntimeReadOperationV1::GraphStats => RuntimeReadResultV1::GraphStats {
                stats: graph_stats(backend.load_stats().await.map_err(map_legacy_error)?),
            },
            RuntimeReadOperationV1::GraphNode { node_id } => RuntimeReadResultV1::GraphNode {
                node: backend
                    .load_node(node_id)
                    .await
                    .map_err(map_legacy_error)?
                    .map(graph_node),
            },
            RuntimeReadOperationV1::GraphSearch { query, limit } => {
                let results = backend
                    .search(query, *limit as usize)
                    .await
                    .map_err(map_legacy_error)?;
                let results = results
                    .into_iter()
                    .map(graph_search_result)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(StorageRuntimePortErrorV1::InvalidResponse)?;
                RuntimeReadResultV1::GraphSearch { results }
            }
            RuntimeReadOperationV1::GraphQuickCheck => RuntimeReadResultV1::GraphQuickCheck {
                healthy: backend.quick_check().await.map_err(map_legacy_error)?,
            },
            _ => unreachable!("unsupported graph operations return before dispatch"),
        };

        if let Some(outcome) = read_interruption(probe)? {
            return Ok(outcome);
        }
        RuntimeReadOutcomeV1::new(
            Some(value),
            RuntimeReadCoverageV1::Latest { observed: None },
        )
        .map_err(StorageRuntimePortErrorV1::InvalidResponse)
    })
}

fn validate_read_dispatch(
    request: &RuntimeReadRequestV1,
    probe: &dyn RuntimeRequestProbeV1,
) -> StorageRuntimePortResultV1<()> {
    request
        .validate()
        .and_then(|()| validate_probe(request.control(), probe))
        .map_err(StorageRuntimePortErrorV1::InvalidRequest)
}

fn validate_submit_dispatch(
    request: &RuntimeSubmitRequestV1,
    probe: &dyn RuntimeRequestProbeV1,
) -> StorageRuntimePortResultV1<()> {
    request
        .validate()
        .and_then(|()| validate_probe(request.control(), probe))
        .map_err(StorageRuntimePortErrorV1::InvalidRequest)
}

fn validate_probe(
    control: &tracedecay_store::RuntimeRequestControlV1,
    probe: &dyn RuntimeRequestProbeV1,
) -> Result<(), StorageRuntimeContractErrorV1> {
    if probe.cancellation_identity() != &control.cancellation {
        return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
            field: "runtime cancellation probe identity",
        });
    }
    if probe.deadline_identity() != &control.deadline {
        return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
            field: "runtime deadline probe identity",
        });
    }
    Ok(())
}

fn binding_unavailable_reason(
    authoritative: &StoreRuntimeBindingV1,
    requested: &StoreRuntimeBindingV1,
) -> Option<UnavailableReasonV1> {
    let graph_worktree_shard = matches!(
        &authoritative.shard_id.scope,
        StoreShardScopeV1::Code {
            scope: CodeShardScopeV1::Worktree { .. },
            ..
        }
    );
    if !graph_worktree_shard || authoritative.shard_id != requested.shard_id {
        Some(UnavailableReasonV1::MissingAuthority)
    } else if authoritative.incarnation != requested.incarnation {
        Some(UnavailableReasonV1::WrongIncarnation)
    } else if authoritative.authority_epoch != requested.authority_epoch {
        Some(UnavailableReasonV1::WrongAuthorityEpoch)
    } else {
        None
    }
}

fn submit_binding_outcome(
    authoritative: &StoreRuntimeBindingV1,
    requested: &StoreRuntimeBindingV1,
) -> Option<RuntimeSubmitOutcomeV1> {
    match binding_unavailable_reason(authoritative, requested) {
        Some(UnavailableReasonV1::WrongAuthorityEpoch) => Some(RuntimeSubmitOutcomeV1::Fenced {
            expected: requested.authority_epoch,
            actual: authoritative.authority_epoch,
        }),
        Some(reason) => Some(RuntimeSubmitOutcomeV1::Unavailable { reason }),
        None => None,
    }
}

fn submit_interruption(
    request: &RuntimeSubmitRequestV1,
    probe: &dyn RuntimeRequestProbeV1,
    stage: RuntimeCancellationStageV1,
) -> Option<RuntimeSubmitOutcomeV1> {
    match probe.interruption()? {
        RuntimeInterruptionV1::Cancelled => Some(RuntimeSubmitOutcomeV1::CancelledBeforeCommit {
            cancellation: request.control().cancellation.clone(),
            stage,
        }),
        RuntimeInterruptionV1::DeadlineExceeded => {
            Some(RuntimeSubmitOutcomeV1::DeadlineExceededBeforeCommit {
                deadline: request.control().deadline.clone(),
            })
        }
    }
}

fn read_interruption(
    probe: &dyn RuntimeRequestProbeV1,
) -> StorageRuntimePortResultV1<Option<RuntimeReadOutcomeV1>> {
    let reason = match probe.interruption() {
        Some(RuntimeInterruptionV1::Cancelled) => UnavailableReasonV1::Cancelled,
        Some(RuntimeInterruptionV1::DeadlineExceeded) => UnavailableReasonV1::DeadlineExceeded,
        None => return Ok(None),
    };
    unavailable_read(reason).map(Some)
}

fn unavailable_read(
    reason: UnavailableReasonV1,
) -> StorageRuntimePortResultV1<RuntimeReadOutcomeV1> {
    RuntimeReadOutcomeV1::new(
        None,
        RuntimeReadCoverageV1::Unavailable {
            coverage: None,
            reason,
        },
    )
    .map_err(StorageRuntimePortErrorV1::InvalidResponse)
}

fn map_legacy_error(_error: TraceDecayError) -> StorageRuntimePortErrorV1 {
    // The legacy public error collapses driver details and does not expose a
    // reliable corruption discriminator. Preserve that uncertainty as an
    // infrastructure error instead of classifying by message text.
    StorageRuntimeErrorV1::Infrastructure {
        operation: "legacy graph read".to_owned(),
    }
    .into()
}

fn graph_stats(stats: GraphStats) -> GraphStatsV1 {
    GraphStatsV1 {
        node_count: stats.node_count,
        edge_count: stats.edge_count,
        file_count: stats.file_count,
        nodes_by_kind: stats.nodes_by_kind.into_iter().collect(),
        edges_by_kind: stats.edges_by_kind.into_iter().collect(),
        db_size_bytes: stats.db_size_bytes,
        last_updated: stats.last_updated,
        total_source_bytes: stats.total_source_bytes,
        files_by_language: stats.files_by_language.into_iter().collect(),
        last_sync_at: stats.last_sync_at,
        last_full_sync_at: stats.last_full_sync_at,
        last_sync_duration_ms: stats.last_sync_duration_ms,
    }
}

fn graph_node(node: Node) -> GraphNodeV1 {
    GraphNodeV1 {
        id: node.id,
        kind: node.kind.as_str().to_owned(),
        name: node.name,
        qualified_name: node.qualified_name,
        file_path: node.file_path,
        start_line: node.start_line,
        attrs_start_line: node.attrs_start_line,
        end_line: node.end_line,
        start_column: node.start_column,
        end_column: node.end_column,
        signature: node.signature,
        docstring: node.docstring,
        visibility: node.visibility.as_str().to_owned(),
        is_async: node.is_async,
        branches: node.branches,
        loops: node.loops,
        returns: node.returns,
        max_nesting: node.max_nesting,
        unsafe_blocks: node.unsafe_blocks,
        unchecked_calls: node.unchecked_calls,
        assertions: node.assertions,
        updated_at: node.updated_at,
        parent_id: node.parent_id,
    }
}

fn graph_search_result(
    result: SearchResult,
) -> Result<GraphSearchResultV1, StorageRuntimeContractErrorV1> {
    Ok(GraphSearchResultV1 {
        node: graph_node(result.node),
        score: GraphSearchScoreV1::new(result.score)?,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

    use tracedecay_domain::{
        BrainId, ProjectId, RepositoryId, UserProfileId, UtcMicros, WorktreeId,
    };
    use tracedecay_store::{
        CommitSequenceV1, FrozenWatermarkVectorV1, OperationPriorityV1, RuntimeCancellationIdV1,
        RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1, RuntimeDeadlineV1,
        RuntimeRequestControlV1, ShardWatermarkV1, SnapshotLeaseIdV1, SnapshotLeaseV1,
        StoreAuthorityEpochV1, StoreIncarnationV1, StoreShardIdV1, StoreSnapshotIdV1,
    };

    use crate::db::DatabaseAuthority;
    use crate::types::{Edge, FileRecord, NodeKind, Visibility};

    use super::*;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn watermark(sequence: u64) -> ShardWatermarkV1 {
        ShardWatermarkV1 {
            shard_id: StoreShardIdV1::code(
                id::<BrainId>("brain.fixture"),
                id::<UserProfileId>("profile.fixture"),
                id::<ProjectId>("project.fixture"),
                id::<RepositoryId>("repository.fixture"),
                CodeShardScopeV1::Worktree {
                    worktree_id: id::<WorktreeId>("worktree.fixture"),
                },
            ),
            incarnation: StoreIncarnationV1::new(1).unwrap(),
            authority_epoch: StoreAuthorityEpochV1::new(7).unwrap(),
            commit_sequence: CommitSequenceV1(sequence),
        }
    }

    fn control() -> RuntimeRequestControlV1 {
        RuntimeRequestControlV1 {
            requested_at: UtcMicros(1),
            deadline: RuntimeDeadlineV1 {
                deadline_id: RuntimeDeadlineIdV1::new("deadline.graph.fixture").unwrap(),
            },
            cancellation: RuntimeCancellationIdentityV1 {
                cancellation_id: RuntimeCancellationIdV1::new("cancel.graph.fixture").unwrap(),
                generation: 1,
            },
        }
    }

    fn request(operation: RuntimeReadOperationV1) -> RuntimeReadRequestV1 {
        request_with_consistency(ConsistencyModeV1::LatestAvailable, operation)
    }

    fn request_with_consistency(
        consistency: ConsistencyModeV1,
        operation: RuntimeReadOperationV1,
    ) -> RuntimeReadRequestV1 {
        let watermark = watermark(4);
        RuntimeReadRequestV1::new(
            StoreRuntimeBindingV1::new(
                watermark.shard_id,
                watermark.incarnation,
                watermark.authority_epoch,
            ),
            consistency,
            operation,
            OperationPriorityV1::Foreground,
            64,
            control(),
        )
        .unwrap()
    }

    struct Probe {
        identity: RuntimeCancellationIdentityV1,
        deadline: RuntimeDeadlineV1,
        interruption: Arc<AtomicU8>,
    }

    impl Probe {
        fn active() -> Self {
            Self {
                identity: control().cancellation,
                deadline: control().deadline,
                interruption: Arc::new(AtomicU8::new(0)),
            }
        }
    }

    impl RuntimeRequestProbeV1 for Probe {
        fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
            &self.identity
        }

        fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
            &self.deadline
        }

        fn interruption(&self) -> Option<RuntimeInterruptionV1> {
            match self.interruption.load(Ordering::SeqCst) {
                0 => None,
                1 => Some(RuntimeInterruptionV1::Cancelled),
                2 => Some(RuntimeInterruptionV1::DeadlineExceeded),
                _ => unreachable!("test probe has a closed interruption state"),
            }
        }
    }

    fn node(id: &str, name: &str) -> Node {
        Node {
            id: id.to_owned(),
            kind: NodeKind::Function,
            name: name.to_owned(),
            qualified_name: format!("fixture::{name}"),
            file_path: "src/fixture.rs".to_owned(),
            start_line: 1,
            attrs_start_line: 1,
            end_line: 2,
            start_column: 0,
            end_column: 1,
            signature: Some(format!("fn {name}()")),
            docstring: None,
            visibility: Visibility::Pub,
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
            updated_at: 1,
            parent_id: None,
        }
    }

    struct FakeBackend {
        searches: AtomicUsize,
        interrupt_after_search: Option<Arc<AtomicU8>>,
    }

    impl GraphReadBackend for FakeBackend {
        fn load_stats(&self) -> LegacyFuture<'_, GraphStats> {
            Box::pin(async {
                Ok(GraphStats {
                    node_count: 0,
                    edge_count: 0,
                    file_count: 0,
                    nodes_by_kind: HashMap::new(),
                    edges_by_kind: HashMap::new(),
                    db_size_bytes: 0,
                    last_updated: 0,
                    total_source_bytes: 0,
                    files_by_language: HashMap::new(),
                    last_sync_at: 0,
                    last_full_sync_at: 0,
                    last_sync_duration_ms: 0,
                })
            })
        }

        fn load_node<'a>(&'a self, _node_id: &'a str) -> LegacyFuture<'a, Option<Node>> {
            Box::pin(async { Ok(None) })
        }

        fn search<'a>(
            &'a self,
            _query: &'a str,
            _limit: usize,
        ) -> LegacyFuture<'a, Vec<SearchResult>> {
            self.searches.fetch_add(1, Ordering::SeqCst);
            if let Some(interruption) = &self.interrupt_after_search {
                interruption.store(2, Ordering::SeqCst);
            }
            Box::pin(async {
                Ok(vec![
                    SearchResult {
                        node: node("node.b", "beta"),
                        score: 1.0,
                    },
                    SearchResult {
                        node: node("node.a", "alpha"),
                        score: 2.0,
                    },
                ])
            })
        }

        fn quick_check(&self) -> LegacyFuture<'_, bool> {
            Box::pin(async { Ok(true) })
        }
    }

    #[tokio::test]
    async fn fake_backend_search_preserves_legacy_order() {
        let backend = FakeBackend {
            searches: AtomicUsize::new(0),
            interrupt_after_search: None,
        };
        let request = request(RuntimeReadOperationV1::GraphSearch {
            query: "fixture".to_owned(),
            limit: 2,
        });
        let watermark = watermark(4);
        let binding = StoreRuntimeBindingV1::new(
            watermark.shard_id,
            watermark.incarnation,
            watermark.authority_epoch,
        );
        let outcome = dispatch_graph_read(&backend, &binding, request, &Probe::active())
            .await
            .unwrap();
        let Some(RuntimeReadResultV1::GraphSearch { results }) = outcome.value() else {
            panic!("expected graph search result");
        };
        assert_eq!(results[0].node.id, "node.b");
        assert_eq!(results[1].node.id, "node.a");
        assert!(matches!(
            outcome.coverage(),
            RuntimeReadCoverageV1::Latest { observed: None }
        ));
        assert_eq!(backend.searches.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn interruption_after_legacy_call_discards_the_read_result() {
        let probe = Probe::active();
        let backend = FakeBackend {
            searches: AtomicUsize::new(0),
            interrupt_after_search: Some(Arc::clone(&probe.interruption)),
        };
        let request = request(RuntimeReadOperationV1::GraphSearch {
            query: "fixture".to_owned(),
            limit: 2,
        });
        let binding = request.binding().clone();

        let outcome = dispatch_graph_read(&backend, &binding, request, &probe)
            .await
            .unwrap();

        assert_eq!(backend.searches.load(Ordering::SeqCst), 1);
        assert!(outcome.value().is_none());
        assert!(matches!(
            outcome.coverage(),
            RuntimeReadCoverageV1::Unavailable {
                reason: UnavailableReasonV1::DeadlineExceeded,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn bounded_graph_reads_abstain_without_canonical_runtime_watermarks() {
        let backend = FakeBackend {
            searches: AtomicUsize::new(0),
            interrupt_after_search: None,
        };
        let operation = || RuntimeReadOperationV1::GraphSearch {
            query: "fixture".to_owned(),
            limit: 2,
        };
        let exact_watermark = watermark(4);
        let cases = [
            (
                ConsistencyModeV1::AtLeast {
                    commit_sequence: CommitSequenceV1(1),
                },
                UnavailableReasonV1::WatermarkNotReached,
            ),
            (
                ConsistencyModeV1::ExactSnapshot {
                    lease: Box::new(SnapshotLeaseV1 {
                        lease_id: SnapshotLeaseIdV1::new("snapshot.graph.fixture").unwrap(),
                        snapshot_id: StoreSnapshotIdV1::new("snapshot.graph.fixture").unwrap(),
                        watermark: exact_watermark.clone(),
                        acquired_at: UtcMicros(1),
                        expires_at: UtcMicros(2),
                    }),
                },
                UnavailableReasonV1::SnapshotNotRetained,
            ),
            (
                ConsistencyModeV1::FrozenWatermarkVector {
                    vector: FrozenWatermarkVectorV1::new([exact_watermark]).unwrap(),
                },
                UnavailableReasonV1::MissingAuthority,
            ),
        ];

        for (consistency, expected) in cases {
            let request = request_with_consistency(consistency, operation());
            let binding = request.binding().clone();
            let outcome = dispatch_graph_read(&backend, &binding, request, &Probe::active())
                .await
                .unwrap();
            assert!(matches!(
                outcome.coverage(),
                RuntimeReadCoverageV1::Unavailable { reason, .. } if *reason == expected
            ));
            assert!(outcome.value().is_none());
        }
        assert_eq!(backend.searches.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn adapter_rejects_wrong_identity_and_interruption_before_backend_access() {
        let backend = FakeBackend {
            searches: AtomicUsize::new(0),
            interrupt_after_search: None,
        };
        let search_request = || {
            request(RuntimeReadOperationV1::GraphSearch {
                query: "fixture".to_owned(),
                limit: 2,
            })
        };

        let mut wrong_epoch = request(RuntimeReadOperationV1::GraphStats)
            .binding()
            .clone();
        wrong_epoch.authority_epoch = StoreAuthorityEpochV1::new(8).unwrap();
        let outcome =
            dispatch_graph_read(&backend, &wrong_epoch, search_request(), &Probe::active())
                .await
                .unwrap();
        assert!(matches!(
            outcome.coverage(),
            RuntimeReadCoverageV1::Unavailable {
                reason: UnavailableReasonV1::WrongAuthorityEpoch,
                ..
            }
        ));

        let cancelled = Probe::active();
        cancelled.interruption.store(1, Ordering::SeqCst);
        let binding = search_request().binding().clone();
        let outcome = dispatch_graph_read(&backend, &binding, search_request(), &cancelled)
            .await
            .unwrap();
        assert!(matches!(
            outcome.coverage(),
            RuntimeReadCoverageV1::Unavailable {
                reason: UnavailableReasonV1::Cancelled,
                ..
            }
        ));

        let expired = Probe::active();
        expired.interruption.store(2, Ordering::SeqCst);
        let outcome = dispatch_graph_read(&backend, &binding, search_request(), &expired)
            .await
            .unwrap();
        assert!(matches!(
            outcome.coverage(),
            RuntimeReadCoverageV1::Unavailable {
                reason: UnavailableReasonV1::DeadlineExceeded,
                ..
            }
        ));
        assert_eq!(backend.searches.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn real_database_serves_graph_reads_without_opening_another_store() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("graph-runtime.db");
        let authority = DatabaseAuthority::acquire_test(&db_path, "graph runtime compat").unwrap();
        let (database, _) = Database::initialize(&db_path, &authority).await.unwrap();
        let fixture = node("node.fixture", "fixture");
        database
            .insert_all(
                std::slice::from_ref(&fixture),
                &[] as &[Edge],
                &[FileRecord {
                    path: "src/fixture.rs".to_owned(),
                    content_hash: "sha256:fixture".to_owned(),
                    size: 10,
                    modified_at: 1,
                    indexed_at: 2,
                    node_count: 1,
                }],
            )
            .await
            .unwrap();

        let adapter = GraphRuntimeCompat::new(
            &database,
            request(RuntimeReadOperationV1::GraphStats)
                .binding()
                .clone(),
        );
        let probe = Probe::active();
        for (operation, expected) in [
            (RuntimeReadOperationV1::GraphStats, "stats"),
            (
                RuntimeReadOperationV1::GraphNode {
                    node_id: fixture.id.clone(),
                },
                "node",
            ),
            (
                RuntimeReadOperationV1::GraphSearch {
                    query: "fixture".to_owned(),
                    limit: 5,
                },
                "search",
            ),
            (RuntimeReadOperationV1::GraphQuickCheck, "health"),
        ] {
            let outcome = adapter.read(request(operation), &probe).await.unwrap();
            match (expected, outcome.value()) {
                ("stats", Some(RuntimeReadResultV1::GraphStats { stats })) => {
                    assert_eq!(stats.node_count, 1);
                }
                ("node", Some(RuntimeReadResultV1::GraphNode { node })) => {
                    assert_eq!(
                        node.as_ref().map(|node| node.id.as_str()),
                        Some("node.fixture")
                    );
                }
                ("search", Some(RuntimeReadResultV1::GraphSearch { results })) => {
                    assert_eq!(results.len(), 1);
                    assert_eq!(results[0].node.id, "node.fixture");
                }
                ("health", Some(RuntimeReadResultV1::GraphQuickCheck { healthy })) => {
                    assert!(*healthy);
                }
                _ => panic!("unexpected {expected} response: {outcome:?}"),
            }
        }
    }
}
