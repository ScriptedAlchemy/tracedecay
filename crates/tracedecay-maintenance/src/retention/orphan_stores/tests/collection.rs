use tracedecay_contracts::doctor::{DoctorEvidenceStateV1, DoctorStorageFamilyReadV1};

use super::*;

/// Seed a profile with one live store and one identity-drift orphan store, then
/// prove the async sweep collects only the orphan and retires its registry row.
#[cfg(unix)]
#[tokio::test]
async fn registered_collection_refuses_same_second_directory_replacement() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let dead_root = tmp.path().join("moved-away-repo");
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let data_root = seed_store(
        &db,
        &profile_root,
        "proj_replaced",
        "store_replaced",
        &dead_root,
        1_700_000_000 - 100 * DAY,
    )
    .await;

    let census = build_store_census(&db, &profile_root).await.unwrap();
    let plan = plan_collection(classify_stores(&census, 1_700_000_000), 7 * DAY);
    assert_eq!(
        plan.collect.len(),
        1,
        "fixture must be eligible before replacement"
    );

    let displaced = profile_root.join("displaced-store");
    std::fs::rename(&data_root, &displaced).unwrap();
    std::fs::create_dir_all(&data_root).unwrap();
    for name in [
        "graph.db",
        tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME,
    ] {
        let source = displaced.join(name);
        let target = data_root.join(name);
        std::fs::copy(&source, &target).unwrap();
        let modified =
            filetime::FileTime::from_system_time(source.metadata().unwrap().modified().unwrap());
        filetime::set_file_mtime(&target, modified).unwrap();
    }
    let original_directory_time =
        filetime::FileTime::from_system_time(displaced.metadata().unwrap().modified().unwrap());
    filetime::set_file_mtime(&data_root, original_directory_time).unwrap();

    let (outcome, retired) = execute_registered_collection(&db, &plan, &profile_root)
        .await
        .unwrap();

    assert_eq!(retired, 0);
    assert!(outcome.collected.is_empty());
    assert_eq!(
        outcome.errors,
        vec![CollectionFailure {
            store_id: "store_replaced".to_owned(),
            kind: CollectionFailureKind::PayloadChanged,
        }]
    );
    assert!(data_root.exists(), "replacement directory must survive");
    assert_eq!(
        db.try_list_store_instances_for_project("proj_replaced")
            .await
            .unwrap()
            .len(),
        1,
        "a rejected collection must leave the registry authority intact"
    );
}

/// A profile-contained symlink is still not a store directory authority.  The
/// collector must refuse it instead of deleting the link and retiring the
/// registry row while its target payload survives without an owner.
#[cfg(unix)]
#[tokio::test]
async fn registered_collection_rejects_profile_contained_data_root_symlink() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let dead_root = tmp.path().join("moved-away-repo");
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let data_root = seed_store(
        &db,
        &profile_root,
        "proj_symlinked_root",
        "store_symlinked_root",
        &dead_root,
        1_700_000_000 - 100 * DAY,
    )
    .await;

    let census = build_store_census(&db, &profile_root).await.unwrap();
    let plan = plan_collection(classify_stores(&census, 1_700_000_000), 7 * DAY);
    assert_eq!(
        plan.collect.len(),
        1,
        "fixture must be eligible before replacement"
    );

    let held_payload = profile_root.join("held-payload");
    std::fs::rename(&data_root, &held_payload).unwrap();
    std::os::unix::fs::symlink(&held_payload, &data_root).unwrap();

    let (outcome, retired) = execute_registered_collection(&db, &plan, &profile_root)
        .await
        .unwrap();

    assert_eq!(retired, 0);
    assert!(outcome.collected.is_empty());
    assert_eq!(
        outcome.errors,
        vec![CollectionFailure {
            store_id: "store_symlinked_root".to_owned(),
            kind: CollectionFailureKind::OutsideProfile,
        }]
    );
    assert!(held_payload.exists(), "the payload target must survive");
    assert!(
        data_root
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        db.try_list_store_instances_for_project("proj_symlinked_root")
            .await
            .unwrap()
            .len(),
        1,
        "a rejected collection must leave the registry authority intact"
    );
}

#[tokio::test]
async fn relink_database_failure_rolls_back_manifest_and_registry() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let dead_root = tmp.path().join("old-repository-root");
    let live_root = tmp.path().join("registered-live-root");
    std::fs::create_dir_all(&live_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let store_root = seed_store(
        &db,
        &profile_root,
        "proj_old",
        "store_moved",
        &dead_root,
        1_700_000_000,
    )
    .await;
    seed_project(&db, "proj_live", &live_root, 1_700_000_000).await;
    let mut manifest = tracedecay_runtime_core::storage::read_store_manifest(
        &store_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
    )
    .unwrap();
    manifest.project_root = live_root;
    std::fs::write(
        store_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    db.writer_connection()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER reject_test_relink
             BEFORE INSERT ON store_instances
             WHEN NEW.project_id = 'proj_live'
             BEGIN SELECT RAISE(ABORT, 'test relink rejection'); END;",
        )
        .await
        .unwrap();

    assert!(
        sweep_orphan_stores(&db, &profile_root, 7 * DAY, 1_700_000_000, true)
            .await
            .is_err()
    );

    let prior = db
        .try_list_store_instances_for_project("proj_old")
        .await
        .unwrap();
    assert_eq!(prior.len(), 1);
    assert_eq!(prior[0].store_id, "store_moved");
    assert!(
        db.try_list_store_instances_for_project("proj_live")
            .await
            .unwrap()
            .is_empty()
    );
    let restored_manifest = tracedecay_runtime_core::storage::read_store_manifest(
        &store_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
    )
    .unwrap();
    assert_eq!(restored_manifest.project_id.as_deref(), Some("proj_old"));
}

#[tokio::test]
async fn durable_memory_rows_block_orphan_store_collection() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let dead_root = tmp.path().join("moved-away-repo");
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let base = 1_700_000_000i64;
    let data_root = seed_store(
        &db,
        &profile_root,
        "proj_memory",
        "store_memory",
        &dead_root,
        base - 100 * DAY,
    )
    .await;

    {
        let connection = rusqlite::Connection::open(data_root.join("graph.db")).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE memory_facts (fact_id INTEGER PRIMARY KEY, content TEXT NOT NULL);
                 INSERT INTO memory_facts (fact_id, content) VALUES (1, 'durable fact');",
            )
            .unwrap();
    }

    let report = sweep_orphan_stores(&db, &profile_root, 7 * DAY, base, true)
        .await
        .unwrap();

    assert!(
        report.outcome.collected.is_empty(),
        "a store with durable memory rows must never be collected"
    );
    assert_eq!(report.outcome.errors.len(), 1);
    assert_eq!(
        report.outcome.errors[0].kind,
        CollectionFailureKind::DurableDataProtected
    );
    assert!(
        data_root.exists(),
        "durable-memory-protected store must remain on disk"
    );
    assert!(
        db.list_code_projects(usize::MAX)
            .await
            .unwrap()
            .into_iter()
            .any(|project| project.project_id == "proj_memory"),
        "registry row for a protected store must not be retired"
    );
}

/// The guard is schema-discovered, so current and future Memory V2 tables are
/// protected without adding every table name to a second hand-maintained list.
#[tokio::test]
async fn memory_v2_rows_block_orphan_store_collection() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let dead_root = tmp.path().join("moved-away-repo");
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let base = 1_700_000_000i64;
    let data_root = seed_store(
        &db,
        &profile_root,
        "proj_memory_v2",
        "store_memory_v2",
        &dead_root,
        base - 100 * DAY,
    )
    .await;

    {
        let connection = rusqlite::Connection::open(data_root.join("graph.db")).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE memory_v2_assertions (
                    assertion_id TEXT PRIMARY KEY,
                    payload TEXT NOT NULL
                 );
                 INSERT INTO memory_v2_assertions (assertion_id, payload)
                 VALUES ('assertion-1', 'durable v2 fact');",
            )
            .unwrap();
    }

    let report = sweep_orphan_stores(&db, &profile_root, 7 * DAY, base, true)
        .await
        .unwrap();

    assert!(report.outcome.collected.is_empty());
    assert_eq!(
        report.outcome.errors[0].kind,
        CollectionFailureKind::DurableDataProtected
    );
    assert!(data_root.exists());
}

/// A durable memory table that exists but is empty must not block collection.
/// Only an actual row does.
#[tokio::test]
async fn empty_memory_table_does_not_block_collection() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let dead_root = tmp.path().join("moved-away-repo");
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let base = 1_700_000_000i64;
    let data_root = seed_store(
        &db,
        &profile_root,
        "proj_empty_memory",
        "store_empty_memory",
        &dead_root,
        base - 100 * DAY,
    )
    .await;
    {
        let connection = rusqlite::Connection::open(data_root.join("graph.db")).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE memory_facts (
                    fact_id INTEGER PRIMARY KEY,
                    content TEXT NOT NULL
                 );
                 CREATE VIRTUAL TABLE memory_facts_fts USING fts5(content);",
            )
            .unwrap();
    }

    let report = sweep_orphan_stores(&db, &profile_root, 7 * DAY, base, true)
        .await
        .unwrap();

    assert_eq!(
        report.outcome.collected.len(),
        1,
        "empty durable-memory tables must not block collection: {report:#?}"
    );
    assert!(!data_root.exists());
}

// === Unregistered store directories =========================================

#[cfg(unix)]
#[tokio::test]
async fn unregistered_collection_refuses_same_second_directory_replacement() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let data_root = profile_root.join("projects/proj_replaced_unregistered");
    std::fs::create_dir_all(&data_root).unwrap();

    let now = walk_store_stats(&data_root)
        .newest_mtime_secs
        .saturating_add(100 * DAY);
    let findings = census_unregistered_project_dirs(&db, &profile_root, now)
        .await
        .unwrap();
    let plan = plan_unregistered_collection(findings, 7 * DAY);
    assert_eq!(
        plan.collect.len(),
        1,
        "fixture must be eligible before replacement"
    );

    let displaced = profile_root.join("displaced-unregistered-store");
    let original_time =
        filetime::FileTime::from_system_time(data_root.metadata().unwrap().modified().unwrap());
    std::fs::rename(&data_root, &displaced).unwrap();
    std::fs::create_dir_all(&data_root).unwrap();
    filetime::set_file_mtime(&data_root, original_time).unwrap();

    let outcome = execute_unregistered_collection(&db, &plan, &profile_root)
        .await
        .unwrap();

    assert!(outcome.collected.is_empty());
    assert_eq!(
        outcome.errors,
        vec![CollectionFailure {
            store_id: "proj_replaced_unregistered".to_owned(),
            kind: CollectionFailureKind::PayloadChanged,
        }]
    );
    assert!(data_root.exists(), "replacement directory must survive");
}

/// An unregistered leaf must not become a deletion target merely because its
/// symlink resolves back inside the profile. The physical `<profile>/projects`
/// path, rather than canonicalized containment, is the destructive authority.
#[cfg(unix)]
#[tokio::test]
async fn unregistered_collection_rejects_profile_contained_data_root_symlink() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let data_root = profile_root.join("projects/proj_symlinked_unregistered");
    std::fs::create_dir_all(&data_root).unwrap();

    let now = walk_store_stats(&data_root)
        .newest_mtime_secs
        .saturating_add(100 * DAY);
    let findings = census_unregistered_project_dirs(&db, &profile_root, now)
        .await
        .unwrap();
    let plan = plan_unregistered_collection(findings, 7 * DAY);
    assert_eq!(
        plan.collect.len(),
        1,
        "fixture must be eligible before the symlink swap"
    );

    let held_payload = profile_root.join("held-unregistered-payload");
    std::fs::rename(&data_root, &held_payload).unwrap();
    std::os::unix::fs::symlink(&held_payload, &data_root).unwrap();

    let outcome = execute_unregistered_collection(&db, &plan, &profile_root)
        .await
        .unwrap();

    assert!(outcome.collected.is_empty());
    assert_eq!(
        outcome.errors,
        vec![CollectionFailure {
            store_id: "proj_symlinked_unregistered".to_owned(),
            kind: CollectionFailureKind::OutsideProfile,
        }]
    );
    assert!(
        held_payload.is_dir(),
        "the in-profile symlink target must survive"
    );
}

/// `SQLite` pages can change in place without changing the parent directory.
/// Hashing the opened child handles makes that post-census mutation visible at
/// the delete boundary even when the writer resets the database mtime.
#[test]
fn delete_boundary_refuses_same_second_sqlite_mutation() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/sqlite-race");
    std::fs::create_dir_all(&data_root).unwrap();
    let database = data_root.join("graph.db");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch("CREATE TABLE facts (value TEXT NOT NULL);")
        .unwrap();
    drop(connection);
    let expected = capture_store_content_fence(&profile_root, &data_root).unwrap();
    let original_time =
        filetime::FileTime::from_system_time(database.metadata().unwrap().modified().unwrap());

    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute("INSERT INTO facts (value) VALUES ('post-census')", [])
        .unwrap();
    drop(connection);
    filetime::set_file_mtime(&database, original_time).unwrap();

    let result = open_verified_store(
        &profile_root,
        &data_root,
        &expected,
        unbounded_collection_control(),
    );
    assert!(matches!(result, Err(CollectionFailureKind::PayloadChanged)));
    let connection = rusqlite::Connection::open(&database).unwrap();
    let rows: i64 = connection
        .query_row("SELECT COUNT(*) FROM facts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 1, "mutated SQLite bytes must survive");
}

/// Even an empty replacement is not the inspected directory. Its child list
/// is identical, so the opened root's stable identity must participate in the
/// comparison before collection can remove anything.
#[test]
fn delete_boundary_refuses_empty_directory_replacement() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/empty-rename-race");
    std::fs::create_dir_all(&data_root).unwrap();
    let expected = capture_store_content_fence(&profile_root, &data_root).unwrap();

    let displaced = profile_root.join("stores/displaced-empty");
    std::fs::rename(&data_root, &displaced).unwrap();
    std::fs::create_dir_all(&data_root).unwrap();

    let result = open_verified_store(
        &profile_root,
        &data_root,
        &expected,
        unbounded_collection_control(),
    );

    assert!(matches!(result, Err(CollectionFailureKind::PayloadChanged)));
    assert!(data_root.is_dir(), "fresh empty replacement must survive");
    assert!(displaced.is_dir(), "inspected empty directory must survive");
}

/// Cancellation is checked before any recursive SHA-256 read. A cancelled
/// maintenance admission cannot turn a deep inventory into a partial plan or
/// an implicit deletion permit.
#[tokio::test]
async fn registered_collection_payload_fence_cancellation_is_terminal() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/payload-fence-cancelled");
    seed_payload_fence_work(&data_root);
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let finding = payload_fence_finding(data_root.clone(), "stores/payload-fence-cancelled");
    let plan = CollectionPlan {
        collect: vec![finding],
        ..CollectionPlan::default()
    };
    let cancellation = CancellationToken::new();
    let started = std::sync::Arc::new(AtomicBool::new(false));
    let started_thread = std::sync::Arc::clone(&started);
    let cancellation_thread = cancellation.clone();
    let signal = std::thread::spawn(move || {
        while !started_thread.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        std::thread::sleep(Duration::from_millis(20));
        cancellation_thread.cancel();
    });
    started.store(true, Ordering::Release);

    let (outcome, retired) = execute_registered_collection_controlled(
        &db,
        &plan,
        &profile_root,
        CollectionControl::new(
            &cancellation,
            MonotonicDeadline::at(Instant::now() + Duration::from_secs(5)),
        ),
    )
    .await
    .unwrap();
    signal.join().unwrap();

    assert_eq!(retired, 0);
    assert_eq!(outcome.completion, CollectionCompletionV1::Cancelled);
    assert!(outcome.errors.is_empty());
    assert!(outcome.collected.is_empty());
    assert!(data_root.exists());
}

#[tokio::test]
async fn unregistered_collection_payload_fence_deadline_is_distinct() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("projects/proj_payload_fence_deadline");
    seed_payload_fence_work(&data_root);
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let finding = payload_fence_finding(data_root.clone(), "projects/proj_payload_fence_deadline");
    let plan = UnregisteredCollectionPlan {
        collect: vec![UnregisteredStoreFinding {
            project_dir_name: "proj_payload_fence_deadline".to_owned(),
            data_root: finding.data_root,
            age_secs: finding.age_secs,
            size_bytes: finding.size_bytes,
            expected_payload_mtime_secs: finding.expected_payload_mtime_secs,
            expected_data_root_fence: finding.expected_data_root_fence,
            expected_content_fence: finding.expected_content_fence,
            abandoned_root: false,
        }],
        ..UnregisteredCollectionPlan::default()
    };

    let cancellation = CancellationToken::new();
    let (outcome, deadline) = {
        let deadline = MonotonicDeadline::at(Instant::now() + Duration::from_millis(20));
        let outcome = execute_unregistered_collection_controlled(
            &db,
            &plan,
            &profile_root,
            CollectionControl::new(&cancellation, deadline),
        )
        .await
        .unwrap();
        (outcome, deadline)
    };

    assert!(deadline.is_elapsed_at(Instant::now()));
    assert_eq!(outcome.completion, CollectionCompletionV1::DeadlineExceeded);
    assert!(outcome.errors.is_empty());
    assert!(outcome.collected.is_empty());
    assert!(data_root.exists());
}

/// A durable-memory guard applies to unregistered directories exactly as it
/// does to registered orphan stores.
#[tokio::test]
async fn sweep_unregistered_stores_never_deletes_durable_memory_rows() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;

    let base = 1_700_000_000i64;
    let dir = profile_root.join("projects").join("proj_ghost_with_memory");
    std::fs::create_dir_all(&dir).unwrap();
    {
        let connection =
            rusqlite::Connection::open(dir.join(tracedecay_runtime_core::config::DB_FILENAME))
                .unwrap();
        connection
            .execute_batch(
                "CREATE TABLE memory_facts (fact_id INTEGER PRIMARY KEY, content TEXT NOT NULL);
                 INSERT INTO memory_facts (fact_id, content) VALUES (1, 'durable fact');",
            )
            .unwrap();
    }
    filetime::set_file_mtime(
        dir.join(tracedecay_runtime_core::config::DB_FILENAME),
        filetime::FileTime::from_unix_time(base - 100 * DAY, 0),
    )
    .unwrap();

    let report = sweep_unregistered_stores(&db, &profile_root, 7 * DAY, base, true)
        .await
        .unwrap();
    assert!(report.outcome.collected.is_empty());
    assert_eq!(
        report.outcome.errors[0].kind,
        CollectionFailureKind::DurableDataProtected
    );
    assert!(dir.exists());
}

/// Seeds `branches/feature.db` (carrying a durable memory row) and its WAL the
/// way the retired per-branch store layout left them. Returns the directory
/// and the seeded bytes.
fn seed_retired_branch_store(data_root: &Path, mtime_secs: i64) -> (PathBuf, u64) {
    let branches = data_root.join("branches");
    std::fs::create_dir_all(&branches).unwrap();
    let database = branches.join("feature.db");
    rusqlite::Connection::open(&database)
        .unwrap()
        .execute_batch(
            "CREATE TABLE memory_facts (fact_id INTEGER PRIMARY KEY, content TEXT NOT NULL);
             INSERT INTO memory_facts (fact_id, content) VALUES (1, 'old-layout fact');",
        )
        .unwrap();
    let wal = branches.join("feature.db-wal");
    std::fs::write(&wal, b"old-layout wal").unwrap();
    let mut bytes = 0;
    for path in [&database, &wal] {
        bytes += std::fs::metadata(path).unwrap().len();
    }
    for path in [&database, &wal, &branches] {
        filetime::set_file_mtime(path, filetime::FileTime::from_unix_time(mtime_secs, 0)).unwrap();
    }
    (branches, bytes)
}

fn profile_file_names(root: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                pending.push(entry.path());
            }
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names
}

/// A manifestless unregistered directory holding only the retired branch-store
/// layout is old data, not durable authority: its memory rows and sidecars do
/// not protect it, and collection leaves no copy behind.
#[tokio::test]
async fn sweep_unregistered_stores_collects_retired_branch_store_layout() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let base = 1_700_000_000i64;
    let dir = profile_root.join("projects").join("proj_ghost_branches");
    seed_retired_branch_store(&dir, base - 100 * DAY);

    let report = sweep_unregistered_stores(&db, &profile_root, 7 * DAY, base, true)
        .await
        .unwrap();

    assert!(
        report.outcome.errors.is_empty(),
        "{:?}",
        report.outcome.errors
    );
    assert_eq!(report.outcome.collected.len(), 1);
    assert!(!dir.exists());
    assert!(
        !profile_file_names(&profile_root)
            .iter()
            .any(|name| name.starts_with("feature.db")),
        "no copy of the retired layout may remain"
    );
}

/// Production journey over one profile: a live registered store carrying a
/// stray `branches/*.db` is reported by Doctor, the maintenance cold-store
/// orphan-collection page deletes it without a copy, Doctor is then clean, and
/// the live store's own files are byte-identical.
#[tokio::test]
async fn cold_store_page_deletes_retired_branch_store_and_doctor_is_clean() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(profile_root.join("projects")).unwrap();
    let live_root = tmp.path().join("live-repo");
    std::fs::create_dir_all(&live_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .cast_signed();
    let data_root = seed_store(
        &db,
        &profile_root,
        "proj_live",
        "store_live",
        &live_root,
        now,
    )
    .await;
    let (branches, seeded_bytes) = seed_retired_branch_store(&data_root, now);
    let live_files = [
        data_root.join("graph.db"),
        data_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
    ];
    let live_bytes = live_files
        .iter()
        .map(|path| std::fs::read(path).unwrap())
        .collect::<Vec<_>>();

    let before = crate::retention::diagnostics::collect_profile_storage_findings(
        &db,
        &profile_root,
        7 * DAY,
        now,
    )
    .await;
    let DoctorStorageFamilyReadV1::Observed { findings } = &before.incident_debris else {
        panic!(
            "incident debris must be observed: {:?}",
            before.incident_debris
        );
    };
    assert_eq!(
        findings
            .iter()
            .map(|finding| finding.finding().state())
            .collect::<Vec<_>>(),
        [DoctorEvidenceStateV1::Degraded],
        "the stray retired store must surface before collection"
    );

    let page = crate::retention::cold_store::run_cold_store_page(
        &profile_root,
        &db,
        Some(7),
        &CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(
        page.outcome,
        crate::retention::cold_store::ColdStorePageOutcomeV1::Processed
    );
    assert_eq!(page.reclaimed_bytes, seeded_bytes);
    assert!(
        !branches.exists(),
        "the retired directory itself is removed"
    );
    assert!(
        !profile_file_names(&profile_root)
            .iter()
            .any(|name| name.starts_with("feature.db")),
        "no copy of the retired layout may remain"
    );
    for (path, bytes) in live_files.iter().zip(&live_bytes) {
        assert_eq!(&std::fs::read(path).unwrap(), bytes, "{}", path.display());
    }
    assert_eq!(
        db.try_list_store_instances_for_project("proj_live")
            .await
            .unwrap()
            .len(),
        1,
        "the live store registration is untouched"
    );

    let after = crate::retention::diagnostics::collect_profile_storage_findings(
        &db,
        &profile_root,
        7 * DAY,
        now,
    )
    .await;
    let DoctorStorageFamilyReadV1::Observed { findings } = &after.incident_debris else {
        panic!(
            "incident debris must be observed: {:?}",
            after.incident_debris
        );
    };
    assert_eq!(
        findings
            .iter()
            .map(|finding| finding.finding().state())
            .collect::<Vec<_>>(),
        [DoctorEvidenceStateV1::HealthyCompleteCoverage]
    );
    assert_eq!(after.orphan_stores, DoctorStorageFamilyReadV1::Absent);
    assert_eq!(after.unregistered_stores, DoctorStorageFamilyReadV1::Absent);
}

/// The durable-data check covers the manifest-selected project graph and every
/// registered project graph scope, and refuses to answer when the manifest
/// that names them cannot be read.
mod durable_inventory {
    use super::*;

    fn manifest_bytes(graph_db_relpath: &str) -> Vec<u8> {
        let project_root = PathBuf::from("/definitely/not/here/gone");
        let manifest = StoreManifest {
            schema_version: STORE_MANIFEST_SCHEMA_VERSION,
            project_id: Some("proj_inventory".to_string()),
            store_kind: StoreKind::CodeProject,
            storage_mode: StorageMode::ProfileSharded,
            project_root: project_root.clone(),
            data_root: project_root,
            graph_db_relpath: PathBuf::from(graph_db_relpath),
            sessions_db_relpath: PathBuf::from("sessions.db"),
            branch_meta_relpath: PathBuf::from(
                tracedecay_runtime_core::storage::BRANCH_META_FILENAME,
            ),
        };
        serde_json::to_vec(&manifest).unwrap()
    }

    #[test]
    fn registered_graph_scopes_at_custom_paths_are_covered() {
        let custom = PathBuf::from("scopes/custom-scope.db");

        let DurableDatabaseInventoryV1::Resolved(inventory) = durable_database_inventory(
            Some(&manifest_bytes("custom-main.db")),
            std::slice::from_ref(&custom),
            unbounded_collection_control(),
        ) else {
            panic!("a readable manifest must resolve an inventory");
        };

        assert_eq!(
            inventory,
            [PathBuf::from("custom-main.db"), custom],
            "the manifest's custom main graph path must be honoured, not the default filename"
        );
    }

    #[test]
    fn a_missing_manifest_fails_closed() {
        assert_eq!(
            durable_database_inventory(None, &[], unbounded_collection_control()),
            DurableDatabaseInventoryV1::Unverifiable,
            "without a manifest the store's graph path is a guess, not a fact"
        );
    }

    #[test]
    fn manifest_graph_path_must_be_normalized_relative() {
        for graph_path in [PathBuf::from(""), PathBuf::from("../graph.db")] {
            let bytes = manifest_bytes(graph_path.to_string_lossy().as_ref());
            assert_eq!(
                durable_database_inventory(Some(&bytes), &[], unbounded_collection_control()),
                DurableDatabaseInventoryV1::Unverifiable,
                "graph path {graph_path:?} must not escape the store"
            );
        }

        assert_eq!(
            durable_database_inventory(
                Some(&manifest_bytes("/tmp/graph.db")),
                &[],
                unbounded_collection_control(),
            ),
            DurableDatabaseInventoryV1::Unverifiable,
            "an absolute graph path must not replace the store root"
        );
    }

    #[test]
    fn registered_graph_scope_path_must_be_normalized_relative() {
        assert_eq!(
            durable_database_inventory(
                Some(&manifest_bytes("graph.db")),
                &[PathBuf::from("scopes/../../escape.db")],
                unbounded_collection_control(),
            ),
            DurableDatabaseInventoryV1::Unverifiable
        );
        assert_eq!(
            durable_database_inventory(
                Some(&manifest_bytes("graph.db")),
                &[PathBuf::from("/tmp/escape.db")],
                unbounded_collection_control(),
            ),
            DurableDatabaseInventoryV1::Unverifiable
        );
    }

    #[test]
    fn cancelled_control_interrupts_the_inventory() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        assert_eq!(
            durable_database_inventory(
                Some(&manifest_bytes("graph.db")),
                &[],
                CollectionControl::new(
                    &cancellation,
                    MonotonicDeadline::at(Instant::now() + Duration::from_secs(1)),
                ),
            ),
            DurableDatabaseInventoryV1::Interrupted,
            "a cancelled admission must not resolve an inventory"
        );
    }

    #[tokio::test]
    async fn a_store_whose_manifest_is_unreadable_is_never_reported_empty() {
        let profile = tempfile::tempdir().unwrap();
        let data_root = profile.path().join("stores/unreadable");
        std::fs::create_dir_all(&data_root).unwrap();

        let check = check_store_durable_memory(
            &data_root,
            Some(b"{ not json"),
            &[],
            &durable_check_scratch_root(profile.path()),
            unbounded_collection_control(),
        )
        .await;

        assert_eq!(
            check,
            DurableMemoryCheck::Unverifiable,
            "an unverifiable inventory must protect the store, never clear it for deletion"
        );
    }

    #[tokio::test]
    async fn a_pre_cancelled_durable_snapshot_reports_interrupted() {
        let profile = tempfile::tempdir().unwrap();
        let data_root = profile.path().join("stores/cancelled");
        std::fs::create_dir_all(&data_root).unwrap();
        rusqlite::Connection::open(data_root.join("graph.db")).unwrap();

        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let check = check_store_durable_memory(
            &data_root,
            Some(&manifest_bytes("graph.db")),
            &[],
            &durable_check_scratch_root(profile.path()),
            CollectionControl::new(
                &cancellation,
                MonotonicDeadline::at(Instant::now() + Duration::from_secs(1)),
            ),
        )
        .await;

        assert_eq!(
            check,
            DurableMemoryCheck::Interrupted,
            "a pre-cancelled durable snapshot must not report an empty database"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_manifest_is_unverifiable_and_never_collected() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let dead_root = tmp.path().join("moved-away-repo");
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let data_root = seed_store(
        &db,
        &profile_root,
        "proj_symlink_manifest",
        "store_symlink_manifest",
        &dead_root,
        1_700_000_000 - 100 * DAY,
    )
    .await;
    let manifest_path = data_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME);
    let target = tmp.path().join("manifest-target.json");
    std::fs::copy(&manifest_path, &target).unwrap();
    std::fs::remove_file(&manifest_path).unwrap();
    std::os::unix::fs::symlink(&target, &manifest_path).unwrap();

    let report = sweep_orphan_stores(&db, &profile_root, 7 * DAY, 1_700_000_000, true)
        .await
        .unwrap();

    assert!(report.plan.collect.is_empty());
    assert_eq!(report.plan.unverifiable.len(), 1);
    assert!(report.outcome.collected.is_empty());
    assert!(data_root.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_graph_database_is_durable_data_protected() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let dead_root = tmp.path().join("moved-away-repo");
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let data_root = seed_store(
        &db,
        &profile_root,
        "proj_symlink_graph",
        "store_symlink_graph",
        &dead_root,
        1_700_000_000 - 100 * DAY,
    )
    .await;
    let graph_path = data_root.join("graph.db");
    let target = tmp.path().join("graph-target.db");
    rusqlite::Connection::open(&target).unwrap();
    std::fs::remove_file(&graph_path).unwrap();
    std::os::unix::fs::symlink(&target, &graph_path).unwrap();

    let report = sweep_orphan_stores(&db, &profile_root, 7 * DAY, 1_700_000_000, true)
        .await
        .unwrap();

    assert_eq!(report.plan.collect.len(), 1);
    assert!(report.outcome.collected.is_empty());
    assert_eq!(report.outcome.errors.len(), 1);
    assert_eq!(
        report.outcome.errors[0].kind,
        CollectionFailureKind::DurableDataProtected
    );
    assert!(data_root.exists());
}
