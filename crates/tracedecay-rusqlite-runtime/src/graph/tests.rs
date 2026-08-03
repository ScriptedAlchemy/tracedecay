use std::{collections::BTreeMap, fs};

use rusqlite::Connection;
use tracedecay_store::{
    AdmissionConfigV1, GraphNodeV1, RuntimeReadOperationV1, RuntimeReadRequestV1,
    RuntimeReadResultV1, StorageRuntimeErrorV1, StoreRuntimeBindingV1,
};

use super::{
    CodeShardAccessV1, CodeShardPhysicalLocator, CodeShardPhysicalLocatorFactory,
    GraphEdgeMutationV1, GraphFileMutationV1, GraphFileReplacementV1, GraphMutationExecutor,
    GraphMutationPayloadV1, GraphPhysicalAttachmentFactory, GraphReaderExecutor,
    fixtures::{
        capture_graph_parity_fixture_v1, exercise_graph_rollback_fixture_v1,
        install_graph_fixture_schema_v1,
    },
};
use crate::reader::ReaderQueryExecutor;

fn worktree_binding(worktree_id: &str) -> StoreRuntimeBindingV1 {
    worktree_binding_for_profile("profile.graph", worktree_id)
}

fn worktree_binding_for_profile(profile_id: &str, worktree_id: &str) -> StoreRuntimeBindingV1 {
    serde_json::from_value(serde_json::json!({
        "shard_id": {
            "brain_id": "brain.graph",
            "profile_id": profile_id,
            "scope": {
                "kind": "code",
                "project_id": "project.graph",
                "repository_id": "repository.graph",
                "scope": {
                    "kind": "worktree",
                    "worktree_id": worktree_id
                }
            }
        },
        "incarnation": 2,
        "authority_epoch": 9
    }))
    .unwrap()
}

fn snapshot_binding(snapshot_id: &str) -> StoreRuntimeBindingV1 {
    serde_json::from_value(serde_json::json!({
        "shard_id": {
            "brain_id": "brain.graph",
            "profile_id": "profile.graph",
            "scope": {
                "kind": "code",
                "project_id": "project.graph",
                "repository_id": "repository.graph",
                "scope": {
                    "kind": "snapshot",
                    "worktree_id": "worktree.graph",
                    "snapshot_id": snapshot_id
                }
            }
        },
        "incarnation": 2,
        "authority_epoch": 9
    }))
    .unwrap()
}

struct PhysicalFixture {
    _temporary: tempfile::TempDir,
    connection: Connection,
    locator: CodeShardPhysicalLocator,
}

impl PhysicalFixture {
    fn new(binding: StoreRuntimeBindingV1) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let factory = CodeShardPhysicalLocatorFactory::new(&root).unwrap();
        let path = factory.prospective_path(&binding.shard_id).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let connection = Connection::open(&path).unwrap();
        install_graph_fixture_schema_v1(&connection).unwrap();
        let locator = factory.resolve_existing(&binding).unwrap();
        Self {
            _temporary: temporary,
            connection,
            locator,
        }
    }
}

fn node(id: &str, name: &str, start_line: u32) -> GraphNodeV1 {
    GraphNodeV1 {
        id: id.to_owned(),
        kind: "function".to_owned(),
        name: name.to_owned(),
        qualified_name: format!("fixture::{name}"),
        file_path: "src/lib.rs".to_owned(),
        start_line,
        attrs_start_line: start_line,
        end_line: start_line + 2,
        start_column: 0,
        end_column: 1,
        signature: Some(format!("fn {name}()")),
        docstring: Some(format!("fixture {name}")),
        visibility: "public".to_owned(),
        is_async: false,
        branches: 0,
        loops: 0,
        returns: 0,
        max_nesting: 0,
        unsafe_blocks: 0,
        unchecked_calls: 0,
        assertions: 0,
        updated_at: 17,
        parent_id: None,
    }
}

fn replacement() -> GraphMutationPayloadV1 {
    GraphMutationPayloadV1::ReplaceFile(GraphFileReplacementV1 {
        file: GraphFileMutationV1 {
            path: "src/lib.rs".to_owned(),
            content_hash: "sha256:fixture".to_owned(),
            size: 128,
            modified_at: 11,
            indexed_at: 12,
            node_count: 2,
        },
        nodes: vec![node("node.alpha", "alpha", 1), node("node.beta", "beta", 5)],
        edges: vec![GraphEdgeMutationV1 {
            source: "node.alpha".to_owned(),
            target: "node.beta".to_owned(),
            kind: "calls".to_owned(),
            line: Some(2),
        }],
    })
}

fn latest_request(
    binding: &StoreRuntimeBindingV1,
    operation: RuntimeReadOperationV1,
) -> RuntimeReadRequestV1 {
    let operation = serde_json::to_value(operation).unwrap();
    serde_json::from_value(serde_json::json!({
        "binding": binding,
        "consistency": { "kind": "latest_available" },
        "operation": operation,
        "priority": "foreground",
        "admission_bytes": 64,
        "control": {
            "requested_at": 1,
            "deadline": { "deadline_id": "deadline.graph" },
            "cancellation": {
                "cancellation_id": "cancellation.graph",
                "generation": 1
            }
        }
    }))
    .unwrap()
}

fn commit_mutation(connection: &mut Connection, payload: &GraphMutationPayloadV1) {
    let mut transaction = connection.transaction().unwrap();
    let savepoint = transaction.savepoint().unwrap();
    GraphMutationExecutor.execute(&savepoint, payload).unwrap();
    savepoint.commit().unwrap();
    transaction.commit().unwrap();
}

#[test]
fn canonical_ids_select_distinct_worktree_and_snapshot_paths() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let factory = CodeShardPhysicalLocatorFactory::new(&root).unwrap();
    let first = factory
        .prospective_path(&worktree_binding("/canonical/worktree-a").shard_id)
        .unwrap();
    let second = factory
        .prospective_path(&worktree_binding("/canonical/worktree-b").shard_id)
        .unwrap();
    let snapshot = factory
        .prospective_path(&snapshot_binding("snapshot.graph").shard_id)
        .unwrap();
    let other_profile = factory
        .prospective_path(
            &worktree_binding_for_profile("profile.other", "/canonical/worktree-a").shard_id,
        )
        .unwrap();

    assert_ne!(first, second);
    assert_ne!(first, snapshot);
    assert_ne!(first, other_profile);
    assert!(first.starts_with(&root));
    assert!(!first.to_string_lossy().contains("canonical/worktree-a"));
}

#[test]
fn physical_attachment_factory_keeps_snapshots_read_only() {
    let worktree = PhysicalFixture::new(worktree_binding("worktree.graph"));
    let snapshot = PhysicalFixture::new(snapshot_binding("snapshot.graph"));
    let factory = GraphPhysicalAttachmentFactory;

    assert!(
        factory
            .prepare(&worktree.locator)
            .unwrap()
            .writer_locator()
            .is_some()
    );
    let snapshot_parts = factory.prepare(&snapshot.locator).unwrap();
    assert!(snapshot_parts.writer_locator().is_none());
    assert!(snapshot_parts.mutation_executor().is_none());
}

#[test]
fn gated_physical_attachment_owns_real_workers_and_drains_before_close() {
    let PhysicalFixture {
        _temporary,
        connection,
        locator,
    } = PhysicalFixture::new(worktree_binding("worktree.physical"));
    drop(connection);
    let attachment = GraphPhysicalAttachmentFactory
        .attach(&locator, AdmissionConfigV1::default())
        .unwrap();

    let ready = attachment.snapshot();
    assert!(ready.healthy);
    assert!(ready.writer_present);
    assert_eq!(ready.reader_handles, 3);

    attachment.drain().unwrap();
    let drained = attachment.snapshot();
    assert!(!drained.writer_present);
    assert_eq!(drained.reader_handles, 0);
    assert_eq!(drained.queued_operations, 0);
    attachment.close_and_join().unwrap();

    let reopened = Connection::open(locator.path()).unwrap();
    let runtime_tables: i64 = reopened
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema
             WHERE name LIKE 'td_runtime_%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(runtime_tables, 0, "attachment startup must not migrate");
}

#[cfg(windows)]
#[test]
fn initialized_graph_abort_closes_handles_and_removes_every_sidecar() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("initialized-graph.db");
    let binding = worktree_binding("worktree.windows-abort");
    let locator = tracedecay_store::VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        binding.incarnation,
        tracedecay_domain::LocatorDigest::new(format!("sha256:{}", "f".repeat(64))).unwrap(),
    );
    let attachment = GraphPhysicalAttachmentFactory
        .initialize(binding, locator, path.clone(), AdmissionConfigV1::default())
        .unwrap();
    attachment
        .exact_sql_handle()
        .unwrap()
        .execute_batch(
            "CREATE TABLE initialized_abort(value INTEGER);
             INSERT INTO initialized_abort VALUES (1);"
                .to_owned(),
        )
        .unwrap();
    let family = ["", "-wal", "-shm", "-journal"].map(|suffix| {
        let mut candidate = path.as_os_str().to_os_string();
        candidate.push(suffix);
        std::path::PathBuf::from(candidate)
    });
    assert!(family[1].exists(), "write must create a WAL sidecar");

    attachment.abort_initialization().unwrap();

    assert!(
        family.iter().all(|candidate| !candidate.exists()),
        "aborted initialization left a SQLite family member behind"
    );
}

#[test]
fn graph_mutation_fixture_proves_exact_rollback() {
    let mut connection = Connection::open_in_memory().unwrap();
    install_graph_fixture_schema_v1(&connection).unwrap();
    let mut executor = GraphMutationExecutor;
    let evidence =
        exercise_graph_rollback_fixture_v1(&mut connection, &mut executor, &replacement()).unwrap();

    assert!(evidence.before.nodes.is_empty());
    assert_eq!(evidence.applied.nodes.len(), 2);
    assert_eq!(evidence.applied.edges.len(), 1);
    assert!(evidence.restored_exactly());
}

#[test]
fn graph_reader_executes_closed_node_search_stats_and_health_queries() {
    let binding = worktree_binding("worktree.reader");
    let mut fixture = PhysicalFixture::new(binding.clone());
    commit_mutation(&mut fixture.connection, &replacement());
    commit_mutation(
        &mut fixture.connection,
        &GraphMutationPayloadV1::SetMetadata {
            entries: BTreeMap::from([
                ("last_sync_at".to_owned(), "21".to_owned()),
                ("last_full_sync_at".to_owned(), "20".to_owned()),
                ("last_sync_duration_ms".to_owned(), "8".to_owned()),
            ]),
        },
    );

    let transaction = fixture.connection.transaction().unwrap();
    let mut executor = GraphReaderExecutor::new(CodeShardAccessV1::MutableWorktree);

    let stats = executor
        .execute_read(
            &transaction,
            &latest_request(&binding, RuntimeReadOperationV1::GraphStats),
        )
        .unwrap();
    assert!(matches!(
        stats.value(),
        Some(RuntimeReadResultV1::GraphStats { stats })
            if stats.node_count == 2 && stats.edge_count == 1 && stats.last_sync_at == 21
    ));

    let point = executor
        .execute_read(
            &transaction,
            &latest_request(
                &binding,
                RuntimeReadOperationV1::GraphNode {
                    node_id: "node.alpha".to_owned(),
                },
            ),
        )
        .unwrap();
    assert!(matches!(
        point.value(),
        Some(RuntimeReadResultV1::GraphNode { node: Some(node) })
            if node.id == "node.alpha"
    ));

    let search = executor
        .execute_read(
            &transaction,
            &latest_request(
                &binding,
                RuntimeReadOperationV1::GraphSearch {
                    query: "alpha".to_owned(),
                    limit: 10,
                },
            ),
        )
        .unwrap();
    assert!(matches!(
        search.value(),
        Some(RuntimeReadResultV1::GraphSearch { results })
            if results.first().is_some_and(|result| result.node.id == "node.alpha")
    ));

    let health = executor
        .execute_read(
            &transaction,
            &latest_request(&binding, RuntimeReadOperationV1::GraphQuickCheck),
        )
        .unwrap();
    assert!(matches!(
        health.value(),
        Some(RuntimeReadResultV1::GraphQuickCheck { healthy: true })
    ));

    let parity = capture_graph_parity_fixture_v1(&transaction).unwrap();
    assert_eq!(parity.files[0].path, "src/lib.rs");
}

#[test]
fn graph_reader_rejects_negative_unsigned_node_fields() {
    let binding = worktree_binding("worktree.negative-node-field");
    let mut fixture = PhysicalFixture::new(binding.clone());
    commit_mutation(&mut fixture.connection, &replacement());
    fixture
        .connection
        .execute(
            "UPDATE nodes SET updated_at = -1 WHERE id = 'node.alpha'",
            [],
        )
        .unwrap();

    let transaction = fixture.connection.transaction().unwrap();
    let mut executor = GraphReaderExecutor::new(CodeShardAccessV1::MutableWorktree);
    let error = executor
        .execute_read(
            &transaction,
            &latest_request(
                &binding,
                RuntimeReadOperationV1::GraphNode {
                    node_id: "node.alpha".to_owned(),
                },
            ),
        )
        .unwrap_err();

    assert!(matches!(
        error,
        StorageRuntimeErrorV1::Infrastructure { operation }
            if operation.contains("map graph node")
    ));
}
