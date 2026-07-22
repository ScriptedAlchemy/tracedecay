use std::fs;

use rusqlite::Connection;
use tracedecay_rusqlite_runtime::graph::{
    CodeShardPhysicalLocatorFactory, GraphFileMutationV1, GraphFileReplacementV1,
    GraphMutationExecutor, GraphMutationPayloadV1, GraphPhysicalAttachmentFactory,
    fixtures::{capture_graph_parity_fixture_v1, install_graph_fixture_schema_v1},
};
use tracedecay_store::GraphNodeV1;

use crate::cutover_support::fixture;

#[test]
fn graph_attachment_preserves_mutable_and_snapshot_parity_without_write_escalation() {
    let fixture = fixture().s7;
    let root = tempfile::tempdir().expect("create S7 graph root");
    let canonical_root = root.path().canonicalize().expect("canonicalize S7 root");
    let locator_factory =
        CodeShardPhysicalLocatorFactory::new(&canonical_root).expect("create locator factory");

    let worktree_path = locator_factory
        .prospective_path(&fixture.worktree_binding.shard_id)
        .expect("resolve worktree graph path");
    let snapshot_path = locator_factory
        .prospective_path(&fixture.snapshot_binding.shard_id)
        .expect("resolve snapshot graph path");
    for path in [&worktree_path, &snapshot_path] {
        fs::create_dir_all(path.parent().expect("graph database parent"))
            .expect("create graph database parent");
        let mut connection = Connection::open(path).expect("create graph authority");
        install_graph_fixture_schema_v1(&connection).expect("install checked graph fixture schema");
        commit_graph_fixture(&mut connection);
    }

    let worktree = locator_factory
        .resolve_existing(&fixture.worktree_binding)
        .expect("resolve mutable graph attachment");
    let snapshot = locator_factory
        .resolve_existing(&fixture.snapshot_binding)
        .expect("resolve immutable graph attachment");
    let attachment = GraphPhysicalAttachmentFactory;
    let mutable_parts = attachment
        .prepare(&worktree)
        .expect("prepare mutable graph attachment");
    let immutable_parts = attachment
        .prepare(&snapshot)
        .expect("prepare immutable graph attachment");

    assert_eq!(mutable_parts.binding(), &fixture.worktree_binding);
    assert!(mutable_parts.writer_locator().is_some());
    assert!(mutable_parts.mutation_executor().is_some());
    assert_eq!(immutable_parts.binding(), &fixture.snapshot_binding);
    assert!(immutable_parts.writer_locator().is_none());
    assert!(immutable_parts.mutation_executor().is_none());

    let worktree_snapshot =
        capture_graph_parity_fixture_v1(&Connection::open(&worktree_path).expect("open worktree"))
            .expect("capture worktree graph parity");
    let immutable_snapshot =
        capture_graph_parity_fixture_v1(&Connection::open(&snapshot_path).expect("open snapshot"))
            .expect("capture immutable graph parity");
    assert_eq!(worktree_snapshot, immutable_snapshot);
    assert_eq!(worktree_snapshot.nodes.len(), 1);
    assert_eq!(worktree_snapshot.files.len(), 1);
}

fn commit_graph_fixture(connection: &mut Connection) {
    let payload = GraphMutationPayloadV1::ReplaceFile(GraphFileReplacementV1 {
        file: GraphFileMutationV1 {
            path: "src/cutover.rs".to_owned(),
            content_hash: "sha256:cutover-graph-v1".to_owned(),
            size: 128,
            modified_at: 17,
            indexed_at: 18,
            node_count: 1,
        },
        nodes: vec![GraphNodeV1 {
            id: "node.cutover".to_owned(),
            kind: "function".to_owned(),
            name: "cutover_graph".to_owned(),
            qualified_name: "fixture::cutover_graph".to_owned(),
            file_path: "src/cutover.rs".to_owned(),
            start_line: 1,
            attrs_start_line: 1,
            end_line: 3,
            start_column: 0,
            end_column: 1,
            signature: Some("fn cutover_graph()".to_owned()),
            docstring: Some("S7 graph attachment parity".to_owned()),
            visibility: "public".to_owned(),
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
            updated_at: 19,
            parent_id: None,
        }],
        edges: Vec::new(),
    });
    let mut transaction = connection.transaction().expect("begin graph fixture write");
    let savepoint = transaction.savepoint().expect("begin graph savepoint");
    GraphMutationExecutor
        .execute(&savepoint, &payload)
        .expect("apply graph fixture through public mutation executor");
    savepoint.commit().expect("commit graph savepoint");
    transaction.commit().expect("commit graph fixture write");
}
