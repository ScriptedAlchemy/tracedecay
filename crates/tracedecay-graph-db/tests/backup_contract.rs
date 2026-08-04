use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_graph_db::{
    GraphCancellation, GraphDb, GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDurability,
    GraphEntity, GraphEntityId, GraphFormatVersion, GraphMutation, GraphNamespace,
    GraphProjectionId, GraphRelation, GraphRelationId, GraphRelationKind, GraphWatermark,
    GraphWriteBatch, NeverCancelled, SourceGeneration, TraversalRequest,
};

fn live() -> Arc<dyn GraphCancellation> {
    Arc::new(NeverCancelled)
}

fn format() -> GraphFormatVersion {
    GraphFormatVersion::new(2).unwrap()
}

fn options(path: std::path::PathBuf) -> GraphDbOpenOptions {
    GraphDbOpenOptions {
        location: GraphDbLocation::Persistent(path),
        expected_format: format(),
        durability: GraphDurability::Sync,
        cancellation: live(),
    }
}

fn entity(value: &str) -> GraphEntity {
    GraphEntity::new(
        GraphEntityId::new(value).unwrap(),
        BTreeSet::new(),
        BTreeMap::new(),
    )
    .unwrap()
}

fn relation(value: &str, from: &str, to: &str) -> GraphRelation {
    GraphRelation::new(
        GraphRelationId::new(value).unwrap(),
        GraphEntityId::new(from).unwrap(),
        GraphEntityId::new(to).unwrap(),
        GraphRelationKind::new("calls").unwrap(),
        BTreeMap::new(),
    )
    .unwrap()
}

fn batch(watermark: &str, mutations: Vec<GraphMutation>) -> GraphWriteBatch {
    GraphWriteBatch::new(
        GraphNamespace::new("project").unwrap(),
        GraphProjectionId::new("code").unwrap(),
        SourceGeneration::new("generation-1").unwrap(),
        GraphWatermark::new(watermark).unwrap(),
        mutations,
        live(),
    )
    .unwrap()
}

fn traversal() -> TraversalRequest {
    TraversalRequest {
        namespace: GraphNamespace::new("project").unwrap(),
        start: GraphEntityId::new("a").unwrap(),
        relation_kinds: BTreeSet::new(),
        max_depth: 8,
        max_visits: 100,
        max_results: 100,
        cancellation: live(),
    }
}

fn full_segment(backup_root: &std::path::Path) -> std::path::PathBuf {
    std::fs::read_dir(backup_root.join("native"))
        .unwrap()
        .map(Result::unwrap)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("backup_full_"))
        })
        .unwrap()
}

#[test]
fn full_backup_restores_the_fenced_graph_snapshot() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source.grafeo");
    let backup = temp.path().join("backup");
    let restored = temp.path().join("restored.grafeo");
    let db = GraphDb::open(options(source)).unwrap();
    db.apply(batch(
        "watermark-1",
        vec![GraphMutation::UpsertEntity(entity("a"))],
    ))
    .unwrap();

    let backup_receipt = db.backup_full(&backup).unwrap();

    db.apply(batch(
        "watermark-2",
        vec![
            GraphMutation::UpsertEntity(entity("b")),
            GraphMutation::UpsertRelation(relation("a-b", "a", "b")),
        ],
    ))
    .unwrap();
    let restore_receipt = GraphDb::restore_full_backup(&backup, &restored, format()).unwrap();
    assert_eq!(restore_receipt, backup_receipt);

    let restored_db = GraphDb::open(options(restored)).unwrap();
    let result = restored_db.traverse(traversal()).unwrap();
    assert_eq!(result.visits.len(), 1);
    assert_eq!(result.visits[0].entity.as_str(), "a");
}

#[test]
fn restore_rejects_corrupted_full_segment_without_publishing_a_database() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source.grafeo");
    let backup = temp.path().join("backup");
    let restored = temp.path().join("restored.grafeo");
    let db = GraphDb::open(options(source)).unwrap();
    db.apply(batch(
        "watermark-1",
        vec![GraphMutation::UpsertEntity(entity("a"))],
    ))
    .unwrap();
    db.backup_full(&backup).unwrap();
    std::fs::write(full_segment(&backup), b"corrupt full segment").unwrap();

    let error = GraphDb::restore_full_backup(&backup, &restored, format()).unwrap_err();

    assert!(matches!(error, GraphDbError::Corrupt { .. }));
    assert!(!restored.exists());
}

#[test]
fn restore_rejects_the_wrong_final_format_without_publishing_a_database() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source.grafeo");
    let backup = temp.path().join("backup");
    let restored = temp.path().join("restored.grafeo");
    let db = GraphDb::open(options(source)).unwrap();
    db.backup_full(&backup).unwrap();

    let error =
        GraphDb::restore_full_backup(&backup, &restored, GraphFormatVersion::new(3).unwrap())
            .unwrap_err();

    assert!(matches!(error, GraphDbError::ResetRequired { .. }));
    assert!(!restored.exists());
}

#[test]
fn restore_never_replaces_an_existing_destination() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source.grafeo");
    let backup = temp.path().join("backup");
    let restored = temp.path().join("restored.grafeo");
    let db = GraphDb::open(options(source)).unwrap();
    db.backup_full(&backup).unwrap();
    std::fs::write(&restored, b"operator-owned destination").unwrap();

    let error = GraphDb::restore_full_backup(&backup, &restored, format()).unwrap_err();

    assert!(matches!(error, GraphDbError::Conflict));
    assert_eq!(
        std::fs::read(&restored).unwrap(),
        b"operator-owned destination"
    );
}
