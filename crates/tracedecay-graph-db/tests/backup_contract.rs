use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tempfile::TempDir;
use tracedecay_graph_db::{
    GraphCancellation, GraphDb, GraphDbError, GraphEntity, GraphEntityId, GraphMutation,
    GraphNamespace, GraphProjectionId, GraphRelation, GraphRelationId, GraphRelationKind,
    GraphTraversalDirection, GraphWatermark, GraphWriteBatch, NeverCancelled, SourceGeneration,
    TraversalRequest,
};

mod support;

use support::{RegisteredGraph, graph_path};

#[derive(Debug)]
struct Cancelled;

impl GraphCancellation for Cancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
}

#[derive(Debug)]
struct CancelOnPoll {
    polls: AtomicUsize,
    cancel_on: usize,
}

impl CancelOnPoll {
    fn new(cancel_on: usize) -> Self {
        Self {
            polls: AtomicUsize::new(0),
            cancel_on,
        }
    }
}

impl GraphCancellation for CancelOnPoll {
    fn is_cancelled(&self) -> bool {
        self.polls.fetch_add(1, Ordering::SeqCst) + 1 >= self.cancel_on
    }
}

fn live() -> Arc<dyn GraphCancellation> {
    Arc::new(NeverCancelled)
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

fn traversal(start: &str) -> TraversalRequest {
    TraversalRequest {
        namespace: GraphNamespace::new("project").unwrap(),
        start: GraphEntityId::new(start).unwrap(),
        relation_kinds: BTreeSet::new(),
        direction: GraphTraversalDirection::Outgoing,
        max_depth: 8,
        max_visits: 100,
        max_results: 100,
        cancellation: live(),
    }
}

/// Seeds a closed store at `root/graph.grafeo` holding only entity `a`.
fn seeded_store(root: &Path) -> PathBuf {
    fs::create_dir_all(root).unwrap();
    let (registered, db) = RegisteredGraph::open_lease(root).unwrap();
    db.apply_unverified(batch(
        "watermark-1",
        vec![GraphMutation::UpsertEntity(entity("a"))],
    ))
    .unwrap();
    drop(db);
    assert!(registered.close().unwrap());
    graph_path(root)
}

fn full_segment(backup_root: &Path) -> PathBuf {
    fs::read_dir(backup_root.join("native"))
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

fn assert_no_staging_residue(parent: &Path) {
    let residue: Vec<_> = fs::read_dir(parent)
        .unwrap()
        .map(Result::unwrap)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".tracedecay-"))
        .collect();
    assert_eq!(residue, Vec::<String>::new());
}

#[test]
fn full_backup_restores_the_fenced_snapshot_and_excludes_later_writes() {
    let temp = TempDir::new().unwrap();
    let store_root = temp.path().join("store");
    let backup = temp.path().join("backup");
    let restored_root = temp.path().join("restored");
    let source = seeded_store(&store_root);

    let backup_receipt = GraphDb::create_verified_backup(&source, &backup, &live()).unwrap();
    assert!(backup_receipt.artifact_count >= 1);

    let (registered, db) = RegisteredGraph::open_lease(&store_root).unwrap();
    db.apply_unverified(batch(
        "watermark-2",
        vec![
            GraphMutation::UpsertEntity(entity("b")),
            GraphMutation::UpsertRelation(relation("a-b", "a", "b")),
        ],
    ))
    .unwrap();
    drop(db);
    assert!(registered.close().unwrap());

    fs::create_dir(&restored_root).unwrap();
    let destination = graph_path(&restored_root);
    let restore_receipt = GraphDb::restore_verified_backup(&backup, &destination, &live()).unwrap();
    assert_eq!(restore_receipt, backup_receipt);
    assert_no_staging_residue(&restored_root);

    let (registered, restored) = RegisteredGraph::open_lease(&restored_root).unwrap();
    let from_a = restored.traverse(traversal("a")).unwrap();
    assert_eq!(from_a.visits.len(), 1);
    assert_eq!(from_a.visits[0].entity.as_str(), "a");
    // The post-backup entity is absent from the restored fenced snapshot.
    let missing = restored.traverse(traversal("b")).unwrap_err();
    assert!(matches!(missing, GraphDbError::InvalidRequest { .. }));
    drop(restored);
    assert!(registered.close().unwrap());
}

#[test]
fn backup_rejects_a_source_still_held_open_by_its_owner() {
    let temp = TempDir::new().unwrap();
    let store_root = temp.path().join("store");
    let backup = temp.path().join("backup");
    seeded_store(&store_root);
    let (registered, _db) = RegisteredGraph::open_lease(&store_root).unwrap();

    let error =
        GraphDb::create_verified_backup(&graph_path(&store_root), &backup, &live()).unwrap_err();

    assert!(matches!(error, GraphDbError::Unavailable { .. }));
    assert!(!backup.exists());
    drop(_db);
    assert!(registered.close().unwrap());
}

#[test]
fn backup_rejects_a_missing_source_store() {
    let temp = TempDir::new().unwrap();
    let error = GraphDb::create_verified_backup(
        &temp.path().join("absent.grafeo"),
        &temp.path().join("backup"),
        &live(),
    )
    .unwrap_err();
    assert!(matches!(error, GraphDbError::InvalidRequest { .. }));
}

#[test]
fn backup_never_replaces_an_existing_destination() {
    let temp = TempDir::new().unwrap();
    let store_root = temp.path().join("store");
    let backup = temp.path().join("backup");
    let source = seeded_store(&store_root);
    fs::create_dir(&backup).unwrap();

    let error = GraphDb::create_verified_backup(&source, &backup, &live()).unwrap_err();

    assert_eq!(error, GraphDbError::Conflict);
}

#[test]
fn cancelled_backup_publishes_nothing_and_cleans_its_staging() {
    let temp = TempDir::new().unwrap();
    let store_root = temp.path().join("store");
    let backup = temp.path().join("backups").join("graph");
    let source = seeded_store(&store_root);
    fs::create_dir(temp.path().join("backups")).unwrap();

    let pre_cancelled: Arc<dyn GraphCancellation> = Arc::new(Cancelled);
    assert_eq!(
        GraphDb::create_verified_backup(&source, &backup, &pre_cancelled).unwrap_err(),
        GraphDbError::Cancelled
    );

    // Cancel after the store opened and the native segments were written so
    // cancellation must abandon and remove real staged artifacts.
    let mid_flight: Arc<dyn GraphCancellation> = Arc::new(CancelOnPoll::new(3));
    assert_eq!(
        GraphDb::create_verified_backup(&source, &backup, &mid_flight).unwrap_err(),
        GraphDbError::Cancelled
    );

    assert!(!backup.exists());
    assert_no_staging_residue(&temp.path().join("backups"));
}

#[test]
fn cancelled_restore_publishes_nothing_and_cleans_its_staging() {
    let temp = TempDir::new().unwrap();
    let store_root = temp.path().join("store");
    let backup = temp.path().join("backup");
    let restored_root = temp.path().join("restored");
    let source = seeded_store(&store_root);
    GraphDb::create_verified_backup(&source, &backup, &live()).unwrap();
    fs::create_dir(&restored_root).unwrap();
    let destination = graph_path(&restored_root);

    // Cancel after the fenced epoch was rebuilt into staging so cancellation
    // must roll the staged restore back instead of publishing it.
    let mid_flight: Arc<dyn GraphCancellation> = Arc::new(CancelOnPoll::new(3));
    assert_eq!(
        GraphDb::restore_verified_backup(&backup, &destination, &mid_flight).unwrap_err(),
        GraphDbError::Cancelled
    );

    assert!(!destination.exists());
    assert_no_staging_residue(&restored_root);
}

#[test]
fn restore_rejects_corrupted_full_segment_without_publishing_a_database() {
    let temp = TempDir::new().unwrap();
    let store_root = temp.path().join("store");
    let backup = temp.path().join("backup");
    let restored_root = temp.path().join("restored");
    let source = seeded_store(&store_root);
    GraphDb::create_verified_backup(&source, &backup, &live()).unwrap();
    fs::write(full_segment(&backup), b"corrupt full segment").unwrap();
    fs::create_dir(&restored_root).unwrap();
    let destination = graph_path(&restored_root);

    let error = GraphDb::restore_verified_backup(&backup, &destination, &live()).unwrap_err();

    assert!(matches!(error, GraphDbError::Corrupt { .. }));
    assert!(!destination.exists());
    assert_no_staging_residue(&restored_root);
}

#[test]
fn restore_rejects_an_artifact_inventory_that_outgrew_its_manifest() {
    let temp = TempDir::new().unwrap();
    let store_root = temp.path().join("store");
    let backup = temp.path().join("backup");
    let source = seeded_store(&store_root);
    GraphDb::create_verified_backup(&source, &backup, &live()).unwrap();
    fs::write(backup.join("native").join("unlisted-artifact"), b"foreign").unwrap();

    let error =
        GraphDb::restore_verified_backup(&backup, &temp.path().join("restored.grafeo"), &live())
            .unwrap_err();

    assert!(matches!(error, GraphDbError::Corrupt { .. }));
}

#[test]
fn restore_rejects_a_stale_format_backup_with_reset_required() {
    let temp = TempDir::new().unwrap();
    let store_root = temp.path().join("store");
    let backup = temp.path().join("backup");
    let source = seeded_store(&store_root);
    GraphDb::create_verified_backup(&source, &backup, &live()).unwrap();
    let manifest_path = backup.join("tracedecay-graph-backup.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["graph_format_version"] = serde_json::Value::from(1);
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let destination = temp.path().join("restored.grafeo");

    let error = GraphDb::restore_verified_backup(&backup, &destination, &live()).unwrap_err();

    assert!(matches!(error, GraphDbError::ResetRequired { .. }));
    assert!(!destination.exists());
}

#[test]
fn restore_never_replaces_an_existing_destination() {
    let temp = TempDir::new().unwrap();
    let store_root = temp.path().join("store");
    let backup = temp.path().join("backup");
    let source = seeded_store(&store_root);
    GraphDb::create_verified_backup(&source, &backup, &live()).unwrap();
    let destination = temp.path().join("restored.grafeo");
    fs::write(&destination, b"operator-owned destination").unwrap();

    let error = GraphDb::restore_verified_backup(&backup, &destination, &live()).unwrap_err();

    assert_eq!(error, GraphDbError::Conflict);
    assert_eq!(
        fs::read(&destination).unwrap(),
        b"operator-owned destination"
    );
}

#[test]
fn verify_closed_store_accepts_a_healthy_store_and_rejects_garbage() {
    let temp = TempDir::new().unwrap();
    let store_root = temp.path().join("store");
    let source = seeded_store(&store_root);
    GraphDb::verify_closed_store(&source, &live()).unwrap();

    let garbage = temp.path().join("garbage.grafeo");
    fs::write(&garbage, b"not a graph database").unwrap();
    assert!(GraphDb::verify_closed_store(&garbage, &live()).is_err());
}
