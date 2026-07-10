//! Regression tests for issue #16: SQLite FTS corruption during search_nodes.
//!
//! These tests verify:
//! - `quick_check` detects real page-level corruption
//! - `search_nodes` falls back without mutating a corrupt FTS index
//! - `rebuild_fts` restores query capability after FTS damage
//! - `begin_bulk_load` no longer disables fsync (`synchronous = OFF`)
//! - The dirty sentinel lifecycle works correctly
//! - The full crash→detect→repair cycle works end-to-end

use crate::common;
use crate::support;

use std::io::{Seek, Write};
use tempfile::TempDir;
use tracedecay::db::Database;
use tracedecay::tracedecay::{TraceDecay, TraceDecayOpenOptions};
use tracedecay::types::*;

/// Helper: create a temp database and return (Database, TempDir, db_path).
/// Seeded from the cached latest-schema template instead of running
/// `Database::initialize` per test; the initialize path itself is covered by
/// `corrupt_db_detected_and_repaired_on_reopen` and db_test.
async fn setup_db() -> (Database, TempDir, std::path::PathBuf) {
    let dir = TempDir::new().expect("failed to create temp dir");
    let db_path = dir.path().join("test.db");
    support::seed_latest_graph_db(&db_path).await;
    let (db, migrated) = Database::open(&db_path)
        .await
        .expect("failed to open template database");
    assert!(!migrated, "template database should not require migration");
    (db, dir, db_path)
}

#[tokio::test]
async fn writable_open_bootstraps_a_missing_database_path() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("new.db");
    assert!(!db_path.exists());

    let (db, _) = Database::open(&db_path)
        .await
        .expect("writable open should preserve fresh-path bootstrap behavior");
    assert!(db_path.exists());
    assert!(db.quick_check().await.unwrap());
    close_db(db).await;
}

async fn close_db(db: Database) {
    db.checkpoint().await.unwrap();
    db.close();
}

/// Helper: create a sample node.
fn sample_node(id: &str, name: &str) -> Node {
    Node {
        id: id.to_string(),
        kind: NodeKind::Function,
        name: name.to_string(),
        qualified_name: format!("crate::{name}"),
        file_path: "src/lib.rs".to_string(),
        start_line: 1,
        attrs_start_line: 1,
        end_line: 10,
        start_column: 0,
        end_column: 1,
        signature: Some(format!("fn {name}()")),
        docstring: Some(format!("Documentation for {name}")),
        visibility: Visibility::Pub,
        is_async: false,
        branches: 0,
        loops: 0,
        returns: 0,
        max_nesting: 0,
        unsafe_blocks: 0,
        unchecked_calls: 0,
        assertions: 0,
        updated_at: 1000,
        parent_id: None,
    }
}

// ─── quick_check ─────────────────────────────────────────────────────────

#[tokio::test]
async fn quick_check_passes_on_healthy_db() {
    let (db, _dir, _path) = setup_db().await;
    assert!(
        db.quick_check().await.unwrap(),
        "fresh database should pass quick_check"
    );
    close_db(db).await;
}

#[tokio::test]
async fn quick_check_passes_after_inserts() {
    let (db, _dir, _path) = setup_db().await;
    let nodes: Vec<Node> = (0..50)
        .map(|i| sample_node(&format!("n{i}"), &format!("func_{i}")))
        .collect();
    db.insert_nodes(&nodes).await.unwrap();
    assert!(
        db.quick_check().await.unwrap(),
        "database with data should pass quick_check"
    );
    close_db(db).await;
}

#[tokio::test]
async fn quick_check_detects_page_level_corruption() {
    let (db, _dir, db_path) = setup_db().await;

    // Insert enough data to create multiple pages
    let nodes: Vec<Node> = (0..100)
        .map(|i| sample_node(&format!("n{i}"), &format!("function_with_long_name_{i}")))
        .collect();
    db.insert_nodes(&nodes).await.unwrap();
    db.checkpoint().await.unwrap();
    db.close();

    // Corrupt the database by overwriting bytes in the middle of the file.
    // This simulates what happens when a crash leaves partially-written pages.
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&db_path)
            .unwrap();
        let len = file.metadata().unwrap().len();
        // Write garbage in the middle of the file (skip the header page)
        let offset = std::cmp::min(len / 2, 8192);
        file.seek(std::io::SeekFrom::Start(offset)).unwrap();
        file.write_all(&[0xDE, 0xAD, 0xBE, 0xEF].repeat(64))
            .unwrap();
        file.sync_all().unwrap();
    }

    // Reopen — quick_check should detect the corruption
    let (db2, _) = Database::open(&db_path)
        .await
        .expect("open should succeed even with corruption");
    let intact = db2.quick_check().await.unwrap();
    assert!(!intact, "quick_check should detect page-level corruption");
    db2.close();
}

// ─── FTS rebuild ─────────────────────────────────────────────────────────

#[tokio::test]
async fn rebuild_fts_on_fresh_db() {
    let (db, _dir, _path) = setup_db().await;
    // rebuild on empty db should not error
    db.rebuild_fts().await.unwrap();
    close_db(db).await;
}

#[tokio::test]
async fn rebuild_fts_restores_search_after_fts_damage() {
    let (db, _dir, _path) = setup_db().await;

    let nodes = vec![
        sample_node("a1", "process_data"),
        sample_node("a2", "validate_input"),
    ];
    db.insert_nodes(&nodes).await.unwrap();

    // Verify search works before damage
    let results = db.search_nodes("process_data", 10).await.unwrap();
    assert!(!results.is_empty(), "search should find process_data");

    // Damage the FTS index by clearing its internal data tables.
    // This simulates what happens when begin_bulk_load clears FTS but
    // end_bulk_load never runs (crash during indexing).
    db.conn()
        .execute_batch("DELETE FROM nodes_fts;")
        .await
        .unwrap();

    // FTS is wiped but content table intact — search_nodes should still work
    // via LIKE fallback (FTS returns empty, falls through to LIKE).

    // Rebuild FTS from content table
    db.rebuild_fts().await.unwrap();

    // Search should work again
    let results = db.search_nodes("process_data", 10).await.unwrap();
    assert!(!results.is_empty(), "search should work after FTS rebuild");
    assert_eq!(results[0].node.id, "a1");
    close_db(db).await;
}

// ─── search_nodes self-healing ───────────────────────────────────────────

#[tokio::test]
async fn search_nodes_falls_back_to_like_when_fts_empty() {
    let (db, _dir, _path) = setup_db().await;

    let nodes = vec![sample_node("b1", "my_function")];
    db.insert_nodes(&nodes).await.unwrap();

    // Wipe FTS
    db.conn()
        .execute_batch("DELETE FROM nodes_fts;")
        .await
        .unwrap();

    // search_nodes should still find the node via LIKE fallback
    // (after FTS returns empty, it falls back to LIKE)
    let results = db.search_nodes("my_function", 10).await.unwrap();
    assert!(!results.is_empty(), "LIKE fallback should find the node");
    assert_eq!(results[0].node.id, "b1");
    db.rebuild_fts().await.unwrap();
    close_db(db).await;
}

// ─── begin_bulk_load no longer downgrades synchronous ────────────────────

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn bulk_load_preserves_platform_synchronous_mode() {
    // Pin the CI-only unsafe-fast escape hatch off for this test: it asserts
    // the *durable* platform synchronous mode, which
    // TRACEDECAY_SQLITE_UNSAFE_FAST=1 (exported for the whole Windows CI test
    // run) would relax to OFF.
    let _env_lock = common::lock_global_db_env();
    let _unsafe_fast_off = common::EnvVarGuard::unset(tracedecay::db::SQLITE_UNSAFE_FAST_ENV);
    let (db, _dir, _path) = setup_db().await;

    db.begin_bulk_load().await.unwrap();

    // NORMAL = 1, FULL = 2. Windows uses DELETE journaling with FULL sync;
    // other platforms use WAL with NORMAL sync.
    let sync_value: i64 = {
        let mut rows = db.conn().query("PRAGMA synchronous", ()).await.unwrap();
        let row = rows.next().await.unwrap().unwrap();
        row.get(0).unwrap()
    };
    let expected_sync = if cfg!(windows) { 2 } else { 1 };
    assert_eq!(
        sync_value, expected_sync,
        "bulk load should preserve the platform synchronous mode"
    );

    db.end_bulk_load().await.unwrap();
    close_db(db).await;
}

#[tokio::test]
async fn bulk_load_round_trip_preserves_data() {
    let (db, _dir, _path) = setup_db().await;

    db.begin_bulk_load().await.unwrap();

    let nodes = vec![sample_node("c1", "alpha"), sample_node("c2", "beta")];
    db.insert_nodes(&nodes).await.unwrap();

    db.end_bulk_load().await.unwrap();

    // After bulk load, FTS should be rebuilt and search should work
    let results = db.search_nodes("alpha", 10).await.unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].node.id, "c1");
    close_db(db).await;
}

// ─── is_corruption_error ─────────────────────────────────────────────────

#[test]
fn is_corruption_error_matches_malformed() {
    let e = tracedecay::errors::TraceDecayError::Database {
        message: "failed to read search result: SQLite failure: `database disk image is malformed`"
            .to_string(),
        operation: "search_nodes".to_string(),
    };
    assert!(Database::is_corruption_error(&e));
}

#[test]
fn is_corruption_error_matches_corrupt() {
    let e = tracedecay::errors::TraceDecayError::Database {
        message: "database is corrupt".to_string(),
        operation: "test".to_string(),
    };
    assert!(Database::is_corruption_error(&e));
}

#[test]
fn is_corruption_error_matches_file_is_not_a_database() {
    let e = tracedecay::errors::TraceDecayError::Database {
        message: "failed to apply pragmas: SQLite failure: `file is not a database`".to_string(),
        operation: "apply_pragmas".to_string(),
    };
    assert!(Database::is_corruption_error(&e));
}

#[test]
fn is_corruption_error_rejects_normal_errors() {
    let e = tracedecay::errors::TraceDecayError::Database {
        message: "no such table: foobar".to_string(),
        operation: "test".to_string(),
    };
    assert!(!Database::is_corruption_error(&e));

    let e2 = tracedecay::errors::TraceDecayError::Config {
        message: "some config error".to_string(),
    };
    assert!(!Database::is_corruption_error(&e2));
}

// ─── Dirty sentinel ──────────────────────────────────────────────────────

#[test]
fn dirty_sentinel_lifecycle() {
    let dir = TempDir::new().unwrap();
    let ts_dir = dir.path().join(".tracedecay");
    std::fs::create_dir_all(&ts_dir).unwrap();

    let dirty_path = ts_dir.join("dirty");

    // No sentinel initially
    assert!(!dirty_path.exists());

    // Write sentinel
    std::fs::write(
        &dirty_path,
        format!("pid={}\nversion=test", std::process::id()),
    )
    .unwrap();
    assert!(dirty_path.exists());

    // Read contents
    let contents = std::fs::read_to_string(&dirty_path).unwrap();
    assert!(contents.contains("pid="));
    assert!(contents.contains("version=test"));

    // Clear sentinel
    std::fs::remove_file(&dirty_path).unwrap();
    assert!(!dirty_path.exists());
}

#[test]
fn dirty_sentinel_survives_drop() {
    // The sentinel is a plain file, not tied to a Drop guard.
    // Simulates: process writes sentinel, then gets killed.
    let dir = TempDir::new().unwrap();
    let ts_dir = dir.path().join(".tracedecay");
    std::fs::create_dir_all(&ts_dir).unwrap();
    let dirty_path = ts_dir.join("dirty");

    {
        // Inner scope — everything is dropped
        std::fs::write(&dirty_path, "pid=99999\nversion=test").unwrap();
    }

    // Sentinel persists after the inner scope exits (simulating process death)
    assert!(dirty_path.exists(), "sentinel must survive scope drop");
}

// ─── Full crash→detect→repair cycle ──────────────────────────────────────

#[tokio::test]
async fn open_preserves_corrupt_store_and_dirty_sentinel_for_offline_repair()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;
    let project_root = dir.path().join("repo");
    std::fs::create_dir_all(&project_root)?;
    let open_options = TraceDecayOpenOptions {
        profile_root: Some(dir.path().join("profile")),
        global_db_path: Some(dir.path().join("global.db")),
    };

    let ts = TraceDecay::init_with_options(&project_root, open_options.clone()).await?;
    let layout = ts.store_layout().clone();
    ts.close();

    let mut corrupted = std::fs::read(&layout.graph_db_path)?;
    corrupted[..16].copy_from_slice(b"not-a-sqlite-db!");
    std::fs::write(&layout.graph_db_path, &corrupted)?;
    std::fs::write(&layout.dirty_path, "pid=99999\nversion=test")?;

    let result = TraceDecay::open_with_options(&project_root, open_options).await;
    assert!(
        result.is_err(),
        "ordinary open must not silently replace a damaged store"
    );
    assert_eq!(
        std::fs::read(&layout.graph_db_path)?,
        corrupted,
        "damaged database must remain available for explicit offline recovery"
    );
    assert!(
        layout.dirty_path.exists(),
        "dirty sentinel must remain until recovery succeeds"
    );
    Ok(())
}

#[tokio::test]
async fn corrupt_db_detected_and_repaired_on_reopen() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");

    // Create and populate a database
    let (db, _) = Database::initialize(&db_path).await.unwrap();
    let nodes: Vec<Node> = (0..50)
        .map(|i| sample_node(&format!("d{i}"), &format!("func_{i}")))
        .collect();
    db.insert_nodes(&nodes).await.unwrap();
    db.checkpoint().await.unwrap();
    db.close();

    // Corrupt the database file
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&db_path)
            .unwrap();
        let len = file.metadata().unwrap().len();
        let offset = std::cmp::min(len / 2, 8192);
        file.seek(std::io::SeekFrom::Start(offset)).unwrap();
        file.write_all(&[0xFF; 256]).unwrap();
        file.sync_all().unwrap();
    }

    // Reopen — should be able to open but quick_check fails
    let open_result = Database::open(&db_path).await;
    match open_result {
        Ok((db2, _)) => {
            let intact = db2.quick_check().await.unwrap();
            assert!(!intact, "corrupted db should fail quick_check");
            db2.close();
        }
        Err(e) => {
            // Some corruption is severe enough to prevent open — that's also
            // valid. The important thing is it doesn't silently succeed.
            assert!(
                Database::is_corruption_error(&e)
                    || format!("{e}").contains("malformed")
                    || format!("{e}").contains("not a database"),
                "unexpected error: {e}"
            );
        }
    }

    // Simulate the recovery path: delete and re-initialize
    std::fs::remove_file(&db_path).ok();
    let mut wal = db_path.clone();
    wal.set_extension("db-wal");
    std::fs::remove_file(&wal).ok();
    wal.set_extension("db-shm");
    std::fs::remove_file(&wal).ok();

    let (db3, _) = Database::initialize(&db_path).await.unwrap();
    assert!(
        db3.quick_check().await.unwrap(),
        "fresh db after recovery should be healthy"
    );
    close_db(db3).await;
}

#[tokio::test]
async fn fts_corruption_falls_back_without_rebuild_or_write() {
    let (db, _dir, db_path) = setup_db().await;

    // Insert data so FTS has content
    let nodes = vec![
        sample_node("e1", "important_handler"),
        sample_node("e2", "other_helper"),
    ];
    db.insert_nodes(&nodes).await.unwrap();

    // Verify search works
    let results = db.search_nodes("important_handler", 10).await.unwrap();
    assert_eq!(results[0].node.id, "e1");

    // Capture an FTS segment, then corrupt only its payload on disk. The nodes
    // table and primary database B-trees remain healthy.
    let mut rows = db
        .conn()
        .query(
            "SELECT block FROM nodes_fts_data WHERE id > 10 ORDER BY id DESC LIMIT 1",
            (),
        )
        .await
        .unwrap();
    let segment = rows
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<Vec<u8>>(0)
        .unwrap();
    drop(rows);
    db.checkpoint().await.unwrap();
    db.close();

    // Corrupt both FTS and an unrelated table. Checking only `nodes` would
    // incorrectly permit the LIKE fallback because its B-tree is still sound.
    let mut bytes = std::fs::read(&db_path).unwrap();
    let offset = bytes
        .windows(segment.len())
        .position(|candidate| candidate == segment)
        .expect("FTS segment must be present in the checkpointed database");
    bytes[offset..offset + 8].fill(0xff);
    std::fs::write(&db_path, bytes).unwrap();

    let (db, _) = Database::open(&db_path).await.unwrap();
    assert!(
        !db.quick_check().await.unwrap(),
        "fixture must trigger SQLite's FTS integrity failure"
    );
    let changes_before = db.conn().total_changes();

    let results = db.search_nodes("important_handler", 10).await.unwrap();
    assert_eq!(results[0].node.id, "e1", "LIKE fallback must still match");
    assert_eq!(
        db.conn().total_changes(),
        changes_before,
        "search must not rebuild or otherwise write"
    );

    let mut rows = db
        .conn()
        .query(
            "SELECT rowid FROM nodes_fts WHERE nodes_fts MATCH '\"important_handler\"*'",
            (),
        )
        .await
        .unwrap();
    assert!(
        rows.next().await.is_err(),
        "the corrupt FTS index must remain untouched for offline repair"
    );
    drop(rows);
    close_db(db).await;
}

#[tokio::test]
async fn whole_database_corruption_propagates_without_write() {
    let (db, _dir, db_path) = setup_db().await;
    db.insert_nodes(&[sample_node("whole-db", "whole_db_probe")])
        .await
        .unwrap();

    let mut rows = db
        .conn()
        .query(
            "SELECT block FROM nodes_fts_data WHERE id > 10 ORDER BY id DESC LIMIT 1",
            (),
        )
        .await
        .unwrap();
    let segment = rows
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<Vec<u8>>(0)
        .unwrap();
    drop(rows);
    let mut rows = db
        .conn()
        .query(
            "SELECT rootpage FROM sqlite_schema WHERE name = 'edges'",
            (),
        )
        .await
        .unwrap();
    let root_page = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap() as u64;
    drop(rows);
    let mut rows = db.conn().query("PRAGMA page_size", ()).await.unwrap();
    let page_size = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap() as u64;
    drop(rows);
    db.checkpoint().await.unwrap();
    db.close();

    let mut bytes = std::fs::read(&db_path).unwrap();
    let fts_offset = bytes
        .windows(segment.len())
        .position(|candidate| candidate == segment)
        .expect("FTS segment must be present in the checkpointed database");
    bytes[fts_offset..fts_offset + 8].fill(0xff);
    bytes[((root_page - 1) * page_size) as usize] = 0xff;
    std::fs::write(&db_path, bytes).unwrap();

    let (db, _) = Database::open(&db_path).await.unwrap();
    let changes_before = db.conn().total_changes();
    let error = db.search_nodes("whole_db_probe", 10).await.unwrap_err();
    assert!(
        Database::is_corruption_error(&error),
        "unexpected error: {error}"
    );
    assert_eq!(
        db.conn().total_changes(),
        changes_before,
        "search must not write while reporting whole-database corruption"
    );
    db.close();
}
