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
use tracedecay::db::migrations::{FULL_REINDEX_REQUIRED_KEY, FULL_REINDEX_REQUIRED_VALUE};
use tracedecay::tracedecay::{TraceDecay, TraceDecayOpenOptions, try_acquire_sync_lock_at};
use tracedecay::types::*;

/// Helper: create a temp database and return (Database, TempDir, db_path).
/// Seeded from the cached latest-schema template instead of running
/// `Database::initialize` per test; the initialize path itself is covered by
/// `corrupt_db_detected_and_repaired_on_reopen` and db_test.
async fn setup_db() -> (Database, TempDir, std::path::PathBuf) {
    let dir = TempDir::new().expect("failed to create temp dir");
    let db_path = dir.path().join("test.db");
    support::seed_latest_graph_db(&db_path).await;
    let (db, migrated) = crate::common::open_test_database(&db_path)
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

    let (db, _) = crate::common::open_test_database(&db_path)
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

    // Writable open validates first and must reject the damaged store.
    let error = match crate::common::open_test_database(&db_path).await {
        Ok(_) => panic!("writable open must reject page-level corruption"),
        Err(error) => error,
    };
    assert!(
        Database::is_corruption_error(&error),
        "integrity rejection must be classified as corruption: {error}"
    );
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
fn is_corruption_error_matches_failed_integrity_validation() {
    let e = tracedecay::errors::TraceDecayError::Database {
        message: "database quick_check failed: Tree 2 page 2 returned error code 11".to_string(),
        operation: "validate_integrity".to_string(),
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

#[tokio::test]
async fn persistent_sync_lock_coordinates_processes_and_recovers_after_crash() {
    if let Ok(mode) = std::env::var("TRACEDECAY_TEST_LOCK_CHILD") {
        let ready = std::path::PathBuf::from(
            std::env::var_os("TRACEDECAY_TEST_LOCK_READY").expect("child ready path"),
        );
        let lock_path = std::path::PathBuf::from(
            std::env::var_os("TRACEDECAY_TEST_LOCK_PATH").expect("child lock path"),
        );
        let guard = try_acquire_sync_lock_at(&lock_path).expect("child lock lease");
        std::fs::write(&ready, b"ready").expect("publish child readiness");
        if mode == "crash" {
            std::mem::forget(guard);
            std::process::exit(86);
        }
        let release = std::path::PathBuf::from(
            std::env::var_os("TRACEDECAY_TEST_LOCK_RELEASE").expect("child release path"),
        );
        while !release.exists() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        drop(guard);
        return;
    }

    let dir = TempDir::new().unwrap();
    let lock_path = dir.path().join("sync.lock");

    let run_child = |mode: &str, release: Option<&std::path::Path>| {
        let ready = dir.path().join(format!("{mode}.ready"));
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .arg("persistent_sync_lock_coordinates_processes_and_recovers_after_crash")
            .arg("--nocapture")
            .env("TRACEDECAY_TEST_LOCK_CHILD", mode)
            .env("TRACEDECAY_TEST_LOCK_READY", &ready)
            .env("TRACEDECAY_TEST_LOCK_PATH", &lock_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit());
        if let Some(path) = release {
            command.env("TRACEDECAY_TEST_LOCK_RELEASE", path);
        }
        let mut child = command.spawn().unwrap();
        for _ in 0..1_000 {
            if ready.exists() {
                return child;
            }
            if let Some(status) = child.try_wait().unwrap() {
                panic!("lock child exited before acquiring its lease: {status}");
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let _ = child.kill();
        panic!("lock child did not acquire its lease");
    };

    let release = dir.path().join("release");
    let mut holder = run_child("hold", Some(&release));
    assert!(
        try_acquire_sync_lock_at(&lock_path).is_err(),
        "a second process must not enter while the kernel lease is held"
    );
    std::fs::write(&release, b"release").unwrap();
    assert!(holder.wait().unwrap().success());

    assert!(lock_path.exists(), "the lockfile must persist after Drop");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&lock_path).unwrap().permissions().mode() & 0o777,
            0o600,
            "persistent lock metadata must be owner-only"
        );
    }

    let mut crashed = run_child("crash", None);
    assert_eq!(crashed.wait().unwrap().code(), Some(86));
    assert!(lock_path.exists(), "the lockfile must survive a crash");
    drop(
        try_acquire_sync_lock_at(&lock_path)
            .expect("released and crashed leases must both be reusable"),
    );
}

#[tokio::test]
async fn structured_dirty_marker_is_cleared_after_epoch_owned_recovery()
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

    std::fs::write(
        &layout.dirty_path,
        format!(
            r#"{{"schema":2,"owner":{{"pid":{}}},"epoch":"fixture-epoch","state":"dirty","time":0,"version":"test"}}"#,
            std::process::id()
        ),
    )?;
    let recovered = TraceDecay::open_with_options(&project_root, open_options).await?;
    assert!(
        !layout.dirty_path.exists(),
        "recovery may clear only the epoch it adopted under the lock lease"
    );
    recovered.close();
    Ok(())
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
async fn dirty_open_checks_integrity_before_writable_migration()
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
    ts.db()
        .conn()
        .execute_batch("PRAGMA user_version = 17")
        .await?;
    ts.checkpoint().await?;
    ts.close();

    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&layout.graph_db_path)?;
    let offset = std::cmp::min(file.metadata()?.len() / 2, 8192);
    file.seek(std::io::SeekFrom::Start(offset))?;
    file.write_all(&[0xFF; 256])?;
    file.sync_all()?;
    drop(file);
    std::fs::write(&layout.dirty_path, "pid=99999\nversion=test")?;

    let before = std::fs::read(&layout.graph_db_path)?;
    let result = TraceDecay::open_with_options(&project_root, open_options).await;
    assert!(result.is_err(), "damaged dirty store must require recovery");
    assert_eq!(
        std::fs::read(&layout.graph_db_path)?,
        before,
        "integrity failure must be detected before writable migration"
    );
    assert!(layout.dirty_path.exists());
    Ok(())
}

#[tokio::test]
async fn dirty_open_reuses_recovery_lock_for_migration_reindex()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;
    let project_root = dir.path().join("repo");
    std::fs::create_dir_all(project_root.join("src"))?;
    std::fs::write(project_root.join("src/lib.rs"), "pub fn migrated() {}\n")?;
    let open_options = TraceDecayOpenOptions {
        profile_root: Some(dir.path().join("profile")),
        global_db_path: Some(dir.path().join("global.db")),
    };

    let ts = TraceDecay::init_with_options(&project_root, open_options.clone()).await?;
    let layout = ts.store_layout().clone();
    ts.db()
        .conn()
        .execute_batch("PRAGMA user_version = 17")
        .await?;
    ts.checkpoint().await?;
    ts.close();
    std::fs::write(&layout.dirty_path, "pid=99999\nversion=test")?;

    let reopened = TraceDecay::open_with_options(&project_root, open_options).await?;
    assert!(
        reopened.get_nodes_by_name("migrated").await?.len() == 1,
        "migration re-index must complete after dirty recovery"
    );
    assert!(!layout.dirty_path.exists());
    reopened.close();
    Ok(())
}

#[tokio::test]
async fn pending_migration_reindex_retries_after_migration_already_committed()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;
    let project_root = dir.path().join("repo");
    std::fs::create_dir_all(project_root.join("src"))?;
    std::fs::write(project_root.join("src/lib.rs"), "pub fn retried() {}\n")?;
    let open_options = TraceDecayOpenOptions {
        profile_root: Some(dir.path().join("profile")),
        global_db_path: Some(dir.path().join("global.db")),
    };

    let ts = TraceDecay::init_with_options(&project_root, open_options.clone()).await?;
    ts.db()
        .set_metadata(FULL_REINDEX_REQUIRED_KEY, FULL_REINDEX_REQUIRED_VALUE)
        .await?;
    ts.checkpoint().await?;
    ts.close();

    let reopened = TraceDecay::open_with_options(&project_root, open_options).await?;
    assert_eq!(reopened.get_nodes_by_name("retried").await?.len(), 1);
    assert_eq!(
        reopened
            .db()
            .get_metadata(FULL_REINDEX_REQUIRED_KEY)
            .await?
            .as_deref(),
        Some("0"),
        "reindex intent must clear only after the retry commits"
    );
    reopened.close();
    Ok(())
}

#[tokio::test]
async fn dirty_open_does_not_race_an_active_sync_lock()
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
    let active_lock = layout.graph_db_path.with_file_name(format!(
        "{}.sync.lock",
        layout.graph_db_path.file_name().unwrap().to_string_lossy()
    ));
    let active_lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&active_lock)?;
    fs2::FileExt::try_lock_exclusive(&active_lock_file)?;
    std::fs::write(&layout.dirty_path, "pid=99999\nversion=test")?;
    let before = std::fs::read(&layout.graph_db_path)?;

    let error = match TraceDecay::open_with_options(&project_root, open_options).await {
        Ok(_) => panic!("active writer lock must block recovery"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("another sync is already in progress")
    );
    assert_eq!(std::fs::read(&layout.graph_db_path)?, before);
    assert!(layout.dirty_path.exists());
    drop(active_lock_file);
    Ok(())
}

#[tokio::test]
async fn dirty_open_recovers_committed_rows_before_clearing_sentinel()
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
    ts.db()
        .conn()
        .execute_batch("PRAGMA wal_autocheckpoint = 0")
        .await?;
    let mut journal_rows = ts.db().conn().query("PRAGMA journal_mode", ()).await?;
    let journal_mode = journal_rows
        .next()
        .await?
        .expect("journal mode row")
        .get::<String>(0)?;
    drop(journal_rows);
    let node = sample_node("wal-recovery-node", "wal_recovery_node");
    ts.db().insert_nodes(std::slice::from_ref(&node)).await?;
    if journal_mode.eq_ignore_ascii_case("wal") {
        let mut wal_path = layout.graph_db_path.as_os_str().to_os_string();
        wal_path.push("-wal");
        assert!(
            std::fs::metadata(std::path::PathBuf::from(wal_path))?.len() > 0,
            "disabled autocheckpoint must retain committed WAL frames"
        );
    } else {
        assert!(
            matches!(
                journal_mode.to_ascii_lowercase().as_str(),
                "delete" | "memory"
            ),
            "production recovery fixture must use a platform-safe non-WAL journal"
        );
    }
    std::fs::write(&layout.dirty_path, "pid=99999\nversion=test")?;

    let recovered = TraceDecay::open_with_options(&project_root, open_options).await?;
    assert!(recovered.get_node(&node.id).await?.is_some());
    assert!(
        !layout.dirty_path.exists(),
        "sentinel clears only after WAL-aware quick_check succeeds"
    );
    recovered.close();
    ts.close();
    Ok(())
}

#[tokio::test]
async fn corrupt_db_detected_and_repaired_on_reopen() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");

    // Create and populate a database
    let (db, _) = crate::common::initialize_test_database(&db_path)
        .await
        .unwrap();
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
    let open_result = crate::common::open_test_database(&db_path).await;
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

    let (db3, _) = crate::common::initialize_test_database(&db_path)
        .await
        .unwrap();
    assert!(
        db3.quick_check().await.unwrap(),
        "fresh db after recovery should be healthy"
    );
    close_db(db3).await;
}

mod fallback;
