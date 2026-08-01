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
use std::time::Duration;
use tempfile::TempDir;
use tracedecay::db::Database;
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
async fn writable_initialize_bootstraps_a_missing_database_path() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("new.db");
    assert!(!db_path.exists());

    let (db, _) = crate::common::initialize_test_database(&db_path)
        .await
        .expect("writable initialize should bootstrap a fresh database path");
    assert!(db_path.exists());
    assert!(db.quick_check().await.unwrap());
    close_db(db).await;
}

async fn close_db(db: Database) {
    db.checkpoint().await.unwrap();
    db.close();
}

fn truncate_fixture_wal(db_path: &std::path::Path) {
    let connection = rusqlite::Connection::open(db_path).expect("open offline fixture database");
    let checkpoint = connection
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .expect("truncate offline fixture WAL");
    assert_eq!(checkpoint, (0, 0, 0), "fixture WAL must be fully truncated");
}

fn structured_dirty_marker(pid: u32, epoch: &str) -> String {
    format!(
        r#"{{"schema":2,"owner":{{"pid":{pid}}},"epoch":"{epoch}","state":"dirty","time":0,"version":"test"}}"#
    )
}

/// Returns the PID of a process that has already exited, so a fixture marker
/// describes a crashed owner rather than work still in flight.
fn exited_process_id() -> u32 {
    #[cfg(unix)]
    let (program, args): (&str, &[&str]) = ("sh", &["-c", "exit 0"]);
    #[cfg(windows)]
    let (program, args): (&str, &[&str]) = ("cmd", &["/C", "exit", "0"]);
    let mut child = std::process::Command::new(program)
        .args(args)
        .spawn()
        .expect("spawn fixture owner process");
    let pid = child.id();
    assert!(child.wait().expect("await fixture owner process").success());
    pid
}

/// Spawns a process that outlives the caller's assertions, so a fixture marker
/// describes a foreign owner that is still running.
fn spawn_live_foreign_owner() -> std::process::Child {
    #[cfg(unix)]
    let (program, args): (&str, &[&str]) = ("sleep", &["120"]);
    #[cfg(windows)]
    let (program, args): (&str, &[&str]) = ("cmd", &["/C", "timeout", "/T", "120", "/NOBREAK"]);
    std::process::Command::new(program)
        .args(args)
        .spawn()
        .expect("spawn live fixture owner process")
}

/// Helper: create a sample node. Corruption coverage never varies the file
/// path, so it pins one and defers to the suite-wide fixture for the rest.
fn sample_node(id: &str, name: &str) -> Node {
    support::sample_node(id, name, "src/lib.rs")
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
    let root_page = db
        .query_scalar_i64(
            "read corruption fixture root page",
            "SELECT rootpage FROM sqlite_schema WHERE name = 'edges'",
        )
        .await
        .unwrap() as u64;
    let page_size = db
        .query_scalar_i64("read corruption fixture page size", "PRAGMA page_size")
        .await
        .unwrap() as u64;
    db.checkpoint().await.unwrap();
    db.close();
    truncate_fixture_wal(&db_path);

    // Corrupt the first byte of a known table root page rather than an
    // arbitrary offset that may land in unused space.
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&db_path)
            .unwrap();
        let offset = (root_page - 1) * page_size;
        file.seek(std::io::SeekFrom::Start(offset)).unwrap();
        file.write_all(&[0xff]).unwrap();
        file.sync_all().unwrap();
    }

    // Opening may succeed before the first integrity read, but corruption
    // must never be reported as healthy.
    match crate::common::open_test_database(&db_path).await {
        Err(error) => assert!(
            Database::is_corruption_error(&error),
            "integrity rejection must be classified as corruption: {error}"
        ),
        Ok((db, _)) => {
            let integrity = db.quick_check().await;
            db.close();
            match integrity {
                Ok(false) => {}
                Err(error) => assert!(
                    Database::is_corruption_error(&error),
                    "integrity read must classify corruption: {error}"
                ),
                Ok(true) => panic!("page-level corruption must not be reported as healthy"),
            }
        }
    }
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
    db.execute_write_batch("clear FTS corruption fixture", "DELETE FROM nodes_fts;")
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

#[tokio::test]
async fn replacing_nodes_keeps_fts_index_consistent() {
    let (db, _dir, _path) = setup_db().await;

    // insert_nodes uses INSERT OR REPLACE; without recursive_triggers the
    // conflict-delete skips the FTS delete trigger and orphans index entries.
    db.insert_nodes(&[sample_node("a1", "process_data")])
        .await
        .unwrap();
    for round in 0..3 {
        db.insert_nodes(&[sample_node("a1", &format!("renamed_fn_{round}"))])
            .await
            .unwrap();
    }

    assert_eq!(
        db.quick_check_report().await.unwrap(),
        None,
        "replacing an indexed node must not desync the FTS index"
    );
    let results = db.search_nodes("renamed_fn_2", 10).await.unwrap();
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
    db.execute_write_batch("clear FTS fallback fixture", "DELETE FROM nodes_fts;")
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
async fn bulk_load_preserves_platform_synchronous_mode() {
    let (db, _dir, _path) = setup_db().await;

    let expected_sync = db
        .query_scalar_i64("inspect initial synchronous mode", "PRAGMA synchronous")
        .await
        .unwrap();
    db.begin_bulk_load().await.unwrap();

    let sync_value = db
        .query_scalar_i64("inspect synchronous mode", "PRAGMA synchronous")
        .await
        .unwrap();
    assert_eq!(
        sync_value, expected_sync,
        "bulk load should preserve the connection's configured synchronous mode"
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
    ts.checkpoint().await?;
    ts.close();
    truncate_fixture_wal(&layout.graph_db_path);

    std::fs::write(
        &layout.dirty_path,
        structured_dirty_marker(exited_process_id(), "fixture-epoch"),
    )?;
    let recovered = TraceDecay::open_with_options(&project_root, open_options).await?;
    assert!(
        !layout.dirty_path.exists(),
        "recovery may clear only the epoch it adopted under the lock lease"
    );
    recovered.close();
    Ok(())
}

#[tokio::test]
async fn open_preserves_a_dirty_marker_owned_by_a_live_foreign_writer()
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
    ts.checkpoint().await?;
    ts.close();
    truncate_fixture_wal(&layout.graph_db_path);

    let mut owner = spawn_live_foreign_owner();
    let foreign = structured_dirty_marker(owner.id(), "foreign-writer-epoch");
    if let Err(error) = std::fs::write(&layout.dirty_path, &foreign) {
        let _ = owner.kill();
        let _ = owner.wait();
        return Err(error.into());
    }

    let opened = match TraceDecay::open_with_options(&project_root, open_options).await {
        Ok(opened) => opened,
        Err(error) => {
            let _ = owner.kill();
            let _ = owner.wait();
            return Err(error.into());
        }
    };
    let observed = std::fs::read_to_string(&layout.dirty_path);
    opened.close();
    let _ = owner.kill();
    let _ = owner.wait();

    assert_eq!(
        observed?, foreign,
        "an open must not clear a marker owned by another live writer"
    );
    Ok(())
}

#[tokio::test]
async fn failed_recovery_retains_the_structured_dirty_marker()
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
    ts.checkpoint().await?;
    ts.close();
    truncate_fixture_wal(&layout.graph_db_path);

    let mut corrupted = std::fs::read(&layout.graph_db_path)?;
    corrupted[..16].copy_from_slice(b"not-a-sqlite-db!");
    std::fs::write(&layout.graph_db_path, &corrupted)?;
    let abandoned = structured_dirty_marker(exited_process_id(), "abandoned-epoch");
    std::fs::write(&layout.dirty_path, &abandoned)?;

    assert!(
        TraceDecay::open_with_options(&project_root, open_options)
            .await
            .is_err(),
        "a damaged store must fail recovery instead of opening"
    );
    assert_eq!(
        std::fs::read_to_string(&layout.dirty_path)?,
        abandoned,
        "a failed recovery must leave its adopted epoch as repair evidence"
    );
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
    ts.checkpoint().await?;
    ts.close();

    truncate_fixture_wal(&layout.graph_db_path);
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
async fn dirty_open_self_heals_fts_only_corruption()
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
        .insert_nodes(&[sample_node("a1", "process_data")])
        .await?;
    // Desync the FTS5 inverted index from its content table by gutting the
    // index's shadow storage, as an interrupted bulk load does.
    ts.db()
        .execute_write_batch(
            "clear FTS corruption fixture",
            "DELETE FROM nodes_fts_data WHERE id > 10;",
        )
        .await?;
    let report = ts.db().quick_check_report().await?;
    let problem = report.expect("desynced FTS index must fail quick_check");
    assert!(
        problem.contains("nodes_fts"),
        "expected an FTS-only problem row, got: {problem}"
    );
    ts.checkpoint().await?;
    ts.close();
    truncate_fixture_wal(&layout.graph_db_path);
    std::fs::write(&layout.dirty_path, "pid=99999\nversion=test")?;

    let ts = TraceDecay::open_with_options(&project_root, open_options)
        .await
        .expect("FTS-only damage must self-heal on a writable open");
    assert!(
        !layout.dirty_path.exists(),
        "dirty sentinel must clear after the FTS rebuild"
    );
    assert!(
        ts.db().quick_check_report().await?.is_none(),
        "store must be intact after the FTS rebuild"
    );
    let results = ts.db().search_nodes("process_data", 10).await?;
    assert_eq!(results[0].node.id, "a1");
    ts.close();
    Ok(())
}

#[tokio::test]
async fn open_self_heals_fts_corruption_without_dirty_sentinel()
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
        .insert_nodes(&[sample_node("a1", "process_data")])
        .await?;
    ts.db()
        .execute_write_batch(
            "clear FTS corruption fixture",
            "DELETE FROM nodes_fts_data WHERE id > 10;",
        )
        .await?;
    ts.checkpoint().await?;
    ts.close();
    truncate_fixture_wal(&layout.graph_db_path);
    assert!(
        !layout.dirty_path.exists(),
        "fixture must model live-writer corruption without a crash sentinel"
    );

    // A store corrupted by a live writer carries no dirty sentinel; the open
    // path must still repair derivable FTS damage instead of failing closed.
    let ts = TraceDecay::open_with_options(&project_root, open_options)
        .await
        .expect("FTS-only damage must self-heal without a dirty sentinel");
    assert!(ts.db().quick_check_report().await?.is_none());
    let results = ts.db().search_nodes("process_data", 10).await?;
    assert_eq!(results[0].node.id, "a1");
    ts.close();
    Ok(())
}

#[tokio::test]
async fn open_repairs_post_open_fts_corruption_with_search_parity()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;
    let project_root = dir.path().join("repo");
    std::fs::create_dir_all(&project_root)?;
    let open_options = TraceDecayOpenOptions {
        profile_root: Some(dir.path().join("profile")),
        global_db_path: Some(dir.path().join("global.db")),
    };

    let ts = TraceDecay::init_with_options(&project_root, open_options.clone()).await?;
    ts.db()
        .insert_nodes(&[
            sample_node("a1", "process_data"),
            sample_node("a2", "process_data_helper"),
            sample_node("a3", "unrelated"),
        ])
        .await?;
    let expected = ts
        .db()
        .search_nodes("process_data", 10)
        .await?
        .into_iter()
        .map(|result| (result.node.id, result.score.to_bits()))
        .collect::<Vec<_>>();

    ts.db()
        .execute_write_batch(
            "clear post-open FTS corruption fixture",
            "DELETE FROM nodes_fts_data WHERE id > 10;",
        )
        .await?;
    assert!(
        ts.db().quick_check_report().await?.is_some(),
        "fixture must corrupt the retained connection's FTS index"
    );

    let reopened = TraceDecay::open_with_options(&project_root, open_options)
        .await
        .expect("a reused post-open connection must schedule FTS repair");
    assert!(
        reopened.db().quick_check_report().await?.is_none(),
        "post-open FTS repair must restore quick_check"
    );
    let actual = reopened
        .db()
        .search_nodes("process_data", 10)
        .await?
        .into_iter()
        .map(|result| (result.node.id, result.score.to_bits()))
        .collect::<Vec<_>>();
    assert_eq!(
        actual, expected,
        "post-repair FTS search order and scores must match the healthy baseline"
    );

    reopened.close();
    ts.close();
    Ok(())
}

#[tokio::test]
async fn post_open_fts_repair_waits_for_concurrent_writer()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;
    let project_root = dir.path().join("repo");
    std::fs::create_dir_all(&project_root)?;
    let open_options = TraceDecayOpenOptions {
        profile_root: Some(dir.path().join("profile")),
        global_db_path: Some(dir.path().join("global.db")),
    };

    let ts = TraceDecay::init_with_options(&project_root, open_options.clone()).await?;
    ts.db()
        .insert_nodes(&[sample_node("a1", "process_data")])
        .await?;
    ts.db()
        .execute_write_batch(
            "clear concurrent FTS corruption fixture",
            "DELETE FROM nodes_fts_data WHERE id > 10;",
        )
        .await?;
    let writer = ts.db().memory_writer().await?;

    let repair_project_root = project_root.clone();
    let mut repair = tokio::spawn(async move {
        TraceDecay::open_with_options(&repair_project_root, open_options).await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut repair)
            .await
            .is_err(),
        "FTS repair must wait for the canonical writer lane"
    );

    drop(writer);
    let reopened = tokio::time::timeout(Duration::from_secs(5), repair).await???;
    assert!(reopened.db().quick_check_report().await?.is_none());
    let results = reopened.db().search_nodes("process_data", 10).await?;
    assert_eq!(results[0].node.id, "a1");

    reopened.close();
    ts.close();
    Ok(())
}

#[tokio::test]
async fn open_self_heals_bundled_sqlite_fts_blob_corruption()
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
        .insert_nodes(&[sample_node("blob-corruption", "blob_corruption_probe")])
        .await?;
    let segment = ts
        .db()
        .query_scalar_blob(
            "read FTS segment fixture",
            "SELECT block FROM nodes_fts_data WHERE id > 10 ORDER BY id DESC LIMIT 1",
        )
        .await?;
    ts.checkpoint().await?;
    ts.close();
    truncate_fixture_wal(&layout.graph_db_path);

    let mut bytes = std::fs::read(&layout.graph_db_path)?;
    let segment_offset = bytes
        .windows(segment.len())
        .position(|candidate| candidate == segment)
        .expect("FTS segment must be present in the checkpointed database");
    bytes[segment_offset..segment_offset + 8].fill(0xff);
    std::fs::write(&layout.graph_db_path, bytes)?;

    let reopened = TraceDecay::open_with_options(&project_root, open_options)
        .await
        .expect("bundled SQLite nodes_fts blob corruption must self-heal on open");
    assert!(reopened.db().quick_check_report().await?.is_none());
    let results = reopened
        .db()
        .search_nodes("blob_corruption_probe", 10)
        .await?;
    assert_eq!(results[0].node.id, "blob-corruption");
    reopened.close();
    Ok(())
}

#[tokio::test]
async fn open_never_repairs_or_replaces_whole_database_corruption()
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
        .insert_nodes(&[sample_node("whole-db", "whole_db_probe")])
        .await?;

    ts.db()
        .execute_write_batch(
            "clear whole-database corruption FTS fixture",
            "DELETE FROM nodes_fts_data WHERE id > 10;",
        )
        .await?;
    let root_page = ts
        .db()
        .query_scalar_i64(
            "read edges root page",
            "SELECT rootpage FROM sqlite_schema WHERE name = 'edges'",
        )
        .await? as u64;
    let page_size = ts
        .db()
        .query_scalar_i64("read SQLite page size", "PRAGMA page_size")
        .await? as u64;
    ts.checkpoint().await?;
    ts.close();
    truncate_fixture_wal(&layout.graph_db_path);

    let mut bytes = std::fs::read(&layout.graph_db_path)?;
    bytes[((root_page - 1) * page_size) as usize] = 0xff;
    std::fs::write(&layout.graph_db_path, &bytes)?;
    let corrupted = std::fs::read(&layout.graph_db_path)?;

    let result = TraceDecay::open_with_options(&project_root, open_options).await;
    assert!(
        result.is_err(),
        "whole-database corruption must require offline recovery"
    );
    assert_eq!(
        std::fs::read(&layout.graph_db_path)?,
        corrupted,
        "ordinary open must not rebuild or replace a whole-database corruption fixture"
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
        .execute_write_batch(
            "set corrupt schema version fixture",
            "PRAGMA user_version = 17",
        )
        .await?;
    let root_page = ts
        .db()
        .query_scalar_i64(
            "read dirty corruption fixture root page",
            "SELECT rootpage FROM sqlite_schema WHERE name = 'edges'",
        )
        .await? as u64;
    let page_size = ts
        .db()
        .query_scalar_i64(
            "read dirty corruption fixture page size",
            "PRAGMA page_size",
        )
        .await? as u64;
    ts.checkpoint().await?;
    ts.close();
    truncate_fixture_wal(&layout.graph_db_path);

    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&layout.graph_db_path)?;
    let offset = (root_page - 1) * page_size;
    file.seek(std::io::SeekFrom::Start(offset))?;
    file.write_all(&[0xff])?;
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
    let journal_mode = ts
        .db()
        .query_scalar_text("read SQLite journal mode", "PRAGMA journal_mode")
        .await?;
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
    let root_page = db
        .query_scalar_i64(
            "read reopen corruption fixture root page",
            "SELECT rootpage FROM sqlite_schema WHERE name = 'edges'",
        )
        .await
        .unwrap() as u64;
    let page_size = db
        .query_scalar_i64(
            "read reopen corruption fixture page size",
            "PRAGMA page_size",
        )
        .await
        .unwrap() as u64;
    db.checkpoint().await.unwrap();
    db.close();
    truncate_fixture_wal(&db_path);

    // Corrupt a known table root page.
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&db_path)
            .unwrap();
        let offset = (root_page - 1) * page_size;
        file.seek(std::io::SeekFrom::Start(offset)).unwrap();
        file.write_all(&[0xff]).unwrap();
        file.sync_all().unwrap();
    }

    // Reopen — should be able to open but quick_check fails
    let open_result = crate::common::open_test_database(&db_path).await;
    match open_result {
        Ok((db2, _)) => {
            let integrity = db2.quick_check().await;
            db2.close();
            match integrity {
                Ok(false) => {}
                Err(error) => assert!(
                    Database::is_corruption_error(&error),
                    "quick_check error must be classified as corruption: {error}"
                ),
                Ok(true) => panic!("corrupted db should fail quick_check"),
            }
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

#[path = "corruption_test/fallback.rs"]
mod fallback;
