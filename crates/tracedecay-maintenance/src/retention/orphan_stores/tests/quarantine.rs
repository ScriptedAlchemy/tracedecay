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

/// A durable memory table that exists but is empty must not block collection
/// — only an actual row does.
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
/// the recovery boundary even when the writer resets the database mtime.
#[test]
fn quarantine_restores_same_second_sqlite_mutation() {
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

    let result =
        quarantine_store_for_verified_collection(&profile_root, &data_root, &expected).unwrap();
    assert!(matches!(result, QuarantineStoreOutcome::Restored { .. }));
    let connection = rusqlite::Connection::open(&database).unwrap();
    let rows: i64 = connection
        .query_row("SELECT COUNT(*) FROM facts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 1, "mutated SQLite bytes must survive recovery");
}

/// Even an empty replacement is not the inspected directory. Its child list
/// is identical, so the moved root's stable identity must participate in the
/// post-rename comparison before collection can remove anything.
#[test]
fn quarantine_restores_empty_directory_replacement_before_delete() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/empty-rename-race");
    std::fs::create_dir_all(&data_root).unwrap();
    let expected = capture_store_content_fence(&profile_root, &data_root).unwrap();

    let displaced = profile_root.join("stores/displaced-empty");
    std::fs::rename(&data_root, &displaced).unwrap();
    std::fs::create_dir_all(&data_root).unwrap();

    let result =
        quarantine_store_for_verified_collection(&profile_root, &data_root, &expected).unwrap();

    assert!(matches!(result, QuarantineStoreOutcome::Restored { .. }));
    assert!(data_root.is_dir(), "fresh empty replacement must survive");
    assert!(displaced.is_dir(), "inspected empty directory must survive");
}

#[test]
fn stale_retired_marker_occupies_the_whole_quarantine_candidate_namespace() {
    let tmp = tempfile::TempDir::new().unwrap();
    let parent_path = tmp.path().join("stores");
    let data_root = parent_path.join("stale-authority");
    let payload_path = data_root.join("payload.bin");
    let payload = b"new store payload must survive reservation";
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(&payload_path, payload).unwrap();
    let parent =
        cap_std::fs::Dir::open_ambient_dir(&parent_path, cap_std::ambient_authority()).unwrap();
    let candidate = format!(
        ".tracedecay-orphan-quarantine-stale-authority-{}-7",
        std::process::id()
    );
    let retired_marker_path = parent_path.join(format!("{candidate}.receipt-v1.json.retired"));
    let marker_bytes = b"stale commit authority must remain exact";
    std::fs::write(&retired_marker_path, marker_bytes).unwrap();

    assert!(
        !quarantine_candidate_namespace_available(&parent, &candidate).unwrap(),
        "a retired marker reserves its candidate even when the directory and journal are absent"
    );
    let mut sequences = [7, 8].into_iter();
    let reserved =
        reserve_quarantine_name_with_sequence(&parent, &data_root, "stale-authority", None, || {
            sequences
                .next()
                .expect("reservation must use only two candidates")
        })
        .unwrap();
    assert_eq!(
        reserved,
        format!(
            ".tracedecay-orphan-quarantine-stale-authority-{}-8",
            std::process::id()
        ),
        "the stale retired marker must force the next sequence"
    );
    assert_eq!(
        retired_marker_path,
        parent_path.join(format!(
            ".tracedecay-orphan-quarantine-stale-authority-{}-7.receipt-v1.json.retired",
            std::process::id()
        ))
    );
    assert_eq!(std::fs::read(&retired_marker_path).unwrap(), marker_bytes);
    assert_eq!(std::fs::read(&payload_path).unwrap(), payload);
}

/// A Windows directory capability denies same-parent rename because cap-std
/// deliberately omits FILE_SHARE_DELETE. The failed retirement must remain a
/// typed deferral over the exact censused store, then converge once that
/// external owner releases its handle.
#[cfg(windows)]
#[test]
fn quarantine_defers_held_windows_store_and_converges_after_release() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/held-capability");
    let payload_path = data_root.join("payload.bin");
    let payload = b"held store bytes remain authoritative";
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(&payload_path, payload).unwrap();
    let expected = capture_store_content_fence(&profile_root, &data_root).unwrap();
    let StoreContentFence::Present(expected_inventory) = &expected else {
        panic!("fixture must capture an exact present-store fence");
    };
    let expected_root_identity = expected_inventory.root.clone();
    let external_owner =
        cap_std::fs::Dir::open_ambient_dir(&data_root, cap_std::ambient_authority()).unwrap();

    let failure =
        match quarantine_store_for_verified_collection(&profile_root, &data_root, &expected) {
            Err(failure) => failure,
            Ok(_) => panic!("held live leaf must not enter quarantine"),
        };
    let CollectionFailureKind::RemoveFailed(failure) = failure else {
        panic!("held live leaf must report a structured mutation failure");
    };
    assert_eq!(
        failure.operation,
        CollectionMutationOperation::RenameLiveLeafToQuarantine
    );
    assert!(
        matches!(failure.raw_os_error, Some(5 | 32)),
        "Windows held-directory rename must preserve access-denied/sharing violation: {failure:?}"
    );
    assert!(failure.retryable());
    assert_eq!(
        failure.classification,
        CollectionMutationFailureClassification::RetryableDeferred
    );
    assert_eq!(failure.target_path, data_root);
    assert_eq!(failure.expected_root_identity, Some(expected_root_identity));
    assert_eq!(std::fs::read(&payload_path).unwrap(), payload);
    assert!(
        data_root.is_dir(),
        "failed quarantine must not collect the store"
    );
    assert!(
        read_pending_quarantine_receipts(&profile_root)
            .unwrap()
            .is_empty(),
        "the failed rename's prepared journal must still be cleared"
    );

    drop(external_owner);
    let result =
        quarantine_store_for_verified_collection(&profile_root, &data_root, &expected).unwrap();
    let QuarantineStoreOutcome::Verified(quarantine) = result else {
        panic!("retry after releasing the external owner must verify quarantine");
    };
    assert_eq!(
        std::fs::read(quarantine.quarantine_path().join("payload.bin")).unwrap(),
        payload
    );
    quarantine.mark_retirement_committed().unwrap();
    let QuarantineFinalizeOutcome::Removed { journal_failure } =
        quarantine.finalize(unbounded_collection_control())
    else {
        panic!("released quarantine must complete the durable removal sequence");
    };
    assert_eq!(journal_failure, None);
    assert!(!data_root.exists());
    assert!(
        read_pending_quarantine_receipts(&profile_root)
            .unwrap()
            .is_empty()
    );
}

/// This simulates a process death after the registry phase was durably marked
/// but before recursive removal. The journal, quarantine path, and committed
/// phase remain readable without relying on an in-memory collection outcome.
#[test]
fn committed_quarantine_crash_boundary_has_a_readable_recovery_receipt() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/crash-boundary");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(data_root.join("payload.bin"), b"preserve until finalize").unwrap();
    let expected = capture_store_content_fence(&profile_root, &data_root).unwrap();

    let result =
        quarantine_store_for_verified_collection(&profile_root, &data_root, &expected).unwrap();
    let QuarantineStoreOutcome::Verified(quarantine) = result else {
        panic!("fixture must reach verified quarantine");
    };
    quarantine.mark_retirement_committed().unwrap();
    drop(quarantine);

    let receipts = read_pending_quarantine_receipts(&profile_root).unwrap();
    assert_eq!(receipts.len(), 1);
    assert!(receipts[0].retirement_committed);
    assert!(receipts[0].quarantine_path.is_dir());
    assert!(!data_root.exists(), "live name remains private after crash");
}

#[test]
fn prepared_journal_recovery_restores_the_exact_original_store() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/prepared-recovery");
    let payload = b"prepared quarantine bytes are restored exactly";
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(data_root.join("payload.bin"), payload).unwrap();
    let expected = capture_store_content_fence(&profile_root, &data_root).unwrap();

    let result =
        quarantine_store_for_verified_collection(&profile_root, &data_root, &expected).unwrap();
    let QuarantineStoreOutcome::Verified(quarantine) = result else {
        panic!("fixture must reach verified quarantine");
    };
    let quarantine_path = quarantine.quarantine_path().to_path_buf();
    drop(quarantine);

    let outcomes = recover_existing_store_quarantine(
        &profile_root,
        &data_root,
        unbounded_collection_control(),
    )
    .unwrap();

    assert_eq!(
        outcomes,
        vec![QuarantineRecoveryOutcome::Restored {
            restored_path: data_root.clone(),
            failure: None,
        }]
    );
    assert_eq!(
        std::fs::read(data_root.join("payload.bin")).unwrap(),
        payload
    );
    assert_eq!(
        capture_store_content_fence(&profile_root, &data_root).unwrap(),
        expected
    );
    assert!(!quarantine_path.exists());
    assert!(
        read_pending_quarantine_receipts(&profile_root)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn committed_journal_recovery_removes_the_exact_quarantine() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/committed-recovery");
    let payload = b"committed quarantine bytes are removed exactly";
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(data_root.join("payload.bin"), payload).unwrap();
    let expected = capture_store_content_fence(&profile_root, &data_root).unwrap();

    let result =
        quarantine_store_for_verified_collection(&profile_root, &data_root, &expected).unwrap();
    let QuarantineStoreOutcome::Verified(quarantine) = result else {
        panic!("fixture must reach verified quarantine");
    };
    let quarantine_path = quarantine.quarantine_path().to_path_buf();
    assert_eq!(
        std::fs::read(quarantine_path.join("payload.bin")).unwrap(),
        payload
    );
    quarantine.mark_retirement_committed().unwrap();
    drop(quarantine);

    let outcomes = recover_existing_store_quarantine(
        &profile_root,
        &data_root,
        unbounded_collection_control(),
    )
    .unwrap();

    assert_eq!(
        outcomes,
        vec![QuarantineRecoveryOutcome::Removed {
            quarantine_path: quarantine_path.clone(),
            journal_failure: None,
        }]
    );
    assert!(!data_root.exists());
    assert!(!quarantine_path.exists());
    assert!(
        read_pending_quarantine_receipts(&profile_root)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn registered_remove_clears_journal_after_exact_quarantine_is_already_absent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/registered-delete-complete");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(
        data_root.join("payload.bin"),
        b"registered deletion completed before metadata cleanup",
    )
    .unwrap();
    let expected = capture_store_content_fence(&profile_root, &data_root).unwrap();
    let quarantine = quarantine_store_for_verified_collection_controlled(
        &profile_root,
        &data_root,
        &expected,
        QuarantineKindV1::Registered,
        "proj_registered_delete_complete",
        "registered-delete-complete",
        Some(QuarantineRegistryFenceV1 {
            store_relpath: "stores/registered-delete-complete".to_owned(),
            created_at: 1_700_000_000,
            last_write_at: Some(1_700_000_000),
        }),
        unbounded_collection_control(),
    )
    .unwrap();
    let QuarantineStoreOutcome::Verified(quarantine) = quarantine else {
        panic!("fixture must reach verified registered quarantine");
    };
    let quarantine_path = quarantine.quarantine_path().to_path_buf();
    drop(quarantine);
    let intent = match read_registered_quarantine_intents_controlled(
        &profile_root,
        unbounded_collection_control(),
    )
    .unwrap()
    {
        RegisteredQuarantineInventoryV1::Complete(mut intents) => {
            assert_eq!(intents.len(), 1);
            intents.pop().unwrap()
        }
        RegisteredQuarantineInventoryV1::Interrupted => panic!("fixture inventory interrupted"),
    };
    std::fs::remove_dir_all(&quarantine_path).unwrap();

    let recovery = recover_registered_quarantine_intent_controlled(
        &profile_root,
        &intent,
        RegisteredQuarantineDecisionV1::Remove,
        unbounded_collection_control(),
    )
    .unwrap();

    assert_eq!(
        recovery,
        Some(QuarantineRecoveryOutcome::Removed {
            quarantine_path: quarantine_path.clone(),
            journal_failure: None,
        })
    );
    assert!(!data_root.exists());
    assert!(!quarantine_path.exists());
    assert!(
        read_pending_quarantine_receipts(&profile_root)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn unregistered_committed_recovery_clears_journal_after_quarantine_is_already_absent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/unregistered-delete-complete");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(
        data_root.join("payload.bin"),
        b"unregistered deletion completed before metadata cleanup",
    )
    .unwrap();
    let expected = capture_store_content_fence(&profile_root, &data_root).unwrap();
    let quarantine =
        quarantine_store_for_verified_collection(&profile_root, &data_root, &expected).unwrap();
    let QuarantineStoreOutcome::Verified(quarantine) = quarantine else {
        panic!("fixture must reach verified unregistered quarantine");
    };
    let quarantine_path = quarantine.quarantine_path().to_path_buf();
    quarantine.mark_retirement_committed().unwrap();
    drop(quarantine);
    std::fs::remove_dir_all(&quarantine_path).unwrap();
    let quarantine_name = quarantine_path.file_name().unwrap().to_str().unwrap();
    let journal_path = quarantine_path.with_file_name(format!("{quarantine_name}.receipt-v1.json"));
    let renamed_marker =
        quarantine_path.with_file_name(format!("{quarantine_name}.receipt-v1.json.renamed"));
    let retired_marker =
        quarantine_path.with_file_name(format!("{quarantine_name}.receipt-v1.json.retired"));
    std::fs::remove_file(&renamed_marker).unwrap();
    assert!(
        journal_path.is_file() && retired_marker.is_file(),
        "a crash before journal removal retains both recovery authorities"
    );

    let recovery = recover_existing_store_quarantine(
        &profile_root,
        &data_root,
        unbounded_collection_control(),
    )
    .unwrap();

    assert_eq!(
        recovery,
        vec![QuarantineRecoveryOutcome::Removed {
            quarantine_path: quarantine_path.clone(),
            journal_failure: None,
        }]
    );
    assert!(!data_root.exists());
    assert!(!quarantine_path.exists());
    assert!(!journal_path.exists());
    assert!(!retired_marker.exists());
    assert!(
        read_pending_quarantine_receipts(&profile_root)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn unregistered_uncommitted_recovery_retains_journal_when_both_names_are_absent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/unregistered-lost-before-commit");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(data_root.join("payload.bin"), b"uncommitted bytes").unwrap();
    let expected = capture_store_content_fence(&profile_root, &data_root).unwrap();
    let StoreContentFence::Present(expected_inventory) = &expected else {
        panic!("fixture must capture an exact present-store fence");
    };
    let expected_root_identity = expected_inventory.root.clone();
    let quarantine =
        quarantine_store_for_verified_collection(&profile_root, &data_root, &expected).unwrap();
    let QuarantineStoreOutcome::Verified(quarantine) = quarantine else {
        panic!("fixture must reach verified unregistered quarantine");
    };
    let quarantine_path = quarantine.quarantine_path().to_path_buf();
    drop(quarantine);
    std::fs::remove_dir_all(&quarantine_path).unwrap();

    let recovery = recover_existing_store_quarantine(
        &profile_root,
        &data_root,
        unbounded_collection_control(),
    )
    .unwrap();

    assert_eq!(
        recovery,
        vec![QuarantineRecoveryOutcome::Retained {
            quarantine_path: quarantine_path.clone(),
            actual_path: quarantine_path.clone(),
            failure: Some(CollectionMutationFailure {
                operation: CollectionMutationOperation::ValidateRestoredStoreIdentity,
                raw_os_error: None,
                target_path: data_root.clone(),
                expected_root_identity: Some(expected_root_identity),
                classification: CollectionMutationFailureClassification::NonRetryable,
            }),
        }]
    );
    assert!(!data_root.exists());
    assert!(!quarantine_path.exists());
    assert_eq!(
        read_pending_quarantine_receipts(&profile_root).unwrap(),
        vec![PendingQuarantineReceiptV1 {
            quarantine_path: quarantine_path.clone(),
            actual_path: quarantine_path,
            retirement_committed: false,
        }]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn registered_recovery_holds_writer_exclusion_through_exact_restore() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let payload = b"registry deletion waits until exact recovery is durable";
    let (data_root, quarantine_path) = prepare_registered_quarantine(
        &db,
        &profile_root,
        "proj_registered_serialized_restore",
        "store_registered_serialized_restore",
        payload,
    )
    .await;
    let quarantine_name = quarantine_path.file_name().unwrap().to_str().unwrap();
    let journal_path = quarantine_path.with_file_name(format!("{quarantine_name}.receipt-v1.json"));
    let (classified_tx, classified_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let recovery_db = db.clone();
    let recovery_profile_root = profile_root.clone();
    let recovery = tokio::spawn(async move {
        let mut classified_tx = Some(classified_tx);
        let mut outcome = CollectionOutcome::default();
        reconcile_registered_quarantine_inventory_with_classified_hook(
            &recovery_db,
            &recovery_profile_root,
            unbounded_collection_control(),
            &mut outcome,
            move |intent, state| {
                assert_eq!(intent.store_id, "store_registered_serialized_restore");
                assert_eq!(state, RegisteredQuarantineRegistryStateV1::Exact);
                classified_tx.take().unwrap().send(()).unwrap();
                release_rx
                    .recv_timeout(Duration::from_secs(5))
                    .expect("test must release classified recovery");
            },
        )
        .await
        .unwrap();
        outcome
    });

    tokio::time::timeout(Duration::from_secs(5), classified_rx)
        .await
        .expect("recovery must reach exact classification")
        .unwrap();
    assert!(!data_root.exists());
    assert!(quarantine_path.is_dir());
    assert!(journal_path.is_file());

    let competitor_db = db.clone();
    let (attempted_tx, attempted_rx) = tokio::sync::oneshot::channel();
    let mut competitor = tokio::spawn(async move {
        attempted_tx.send(()).unwrap();
        let transaction = competitor_db.begin_write_transaction().await.unwrap();
        let deleted = transaction
            .execute(
                "DELETE FROM store_instances WHERE store_id = ?1",
                tracedecay_runtime_core::db::engine::params!["store_registered_serialized_restore"],
            )
            .await
            .unwrap();
        transaction.commit().await.unwrap();
        deleted
    });
    attempted_rx.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(250), &mut competitor)
            .await
            .is_err(),
        "competing registry deletion committed between classification and restore"
    );
    assert!(!data_root.exists());
    assert!(quarantine_path.is_dir());
    assert!(journal_path.is_file());

    release_tx.send(()).unwrap();
    let outcome = recovery.await.unwrap();
    assert_eq!(competitor.await.unwrap(), 1);
    assert!(outcome.errors.is_empty(), "{outcome:#?}");
    assert_eq!(
        outcome.recovery_receipts,
        vec![CollectionRecoveryReceipt {
            store_id: "store_registered_serialized_restore".to_owned(),
            original_path: data_root.clone(),
            quarantine_path: quarantine_path.clone(),
            actual_path: data_root.clone(),
            action: CollectionRecoveryAction::Restored,
        }]
    );
    assert_eq!(
        std::fs::read(data_root.join("payload.bin")).unwrap(),
        payload
    );
    assert!(!quarantine_path.exists());
    assert!(!journal_path.exists());
    assert!(
        db.try_list_store_instances_for_project("proj_registered_serialized_restore")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        read_pending_quarantine_receipts(&profile_root)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn empty_plan_clears_registered_pre_rename_journal_when_exact_row_remains() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let payload = b"pre-rename registered bytes never moved";
    let (data_root, quarantine_path) = prepare_registered_quarantine(
        &db,
        &profile_root,
        "proj_registered_pre_rename",
        "store_registered_pre_rename",
        payload,
    )
    .await;
    let expected = capture_store_content_fence(&profile_root, &quarantine_path).unwrap();
    let quarantine_name = quarantine_path.file_name().unwrap().to_str().unwrap();
    let journal_path = quarantine_path.with_file_name(format!("{quarantine_name}.receipt-v1.json"));
    let renamed_marker =
        quarantine_path.with_file_name(format!("{quarantine_name}.receipt-v1.json.renamed"));
    let retired_marker =
        quarantine_path.with_file_name(format!("{quarantine_name}.receipt-v1.json.retired"));
    std::fs::rename(&quarantine_path, &data_root).unwrap();
    std::fs::remove_file(&renamed_marker).unwrap();
    assert!(journal_path.is_file());

    let (outcome, retired) =
        execute_registered_collection(&db, &CollectionPlan::default(), &profile_root)
            .await
            .unwrap();

    assert_eq!(retired, 0);
    assert!(outcome.errors.is_empty(), "{outcome:#?}");
    assert!(outcome.collected.is_empty());
    assert_eq!(outcome.reclaimed_bytes, 0);
    assert_eq!(
        outcome.recovery_receipts,
        vec![CollectionRecoveryReceipt {
            store_id: "store_registered_pre_rename".to_owned(),
            original_path: data_root.clone(),
            quarantine_path: quarantine_path.clone(),
            actual_path: data_root.clone(),
            action: CollectionRecoveryAction::Restored,
        }]
    );
    assert_eq!(
        std::fs::read(data_root.join("payload.bin")).unwrap(),
        payload
    );
    assert_eq!(
        capture_store_content_fence(&profile_root, &data_root).unwrap(),
        expected
    );
    assert!(!quarantine_path.exists());
    assert!(!journal_path.exists());
    assert!(!renamed_marker.exists());
    assert!(!retired_marker.exists());
    assert!(
        read_pending_quarantine_receipts(&profile_root)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        db.try_list_store_instances_for_project("proj_registered_pre_rename")
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn stale_registered_retirement_marker_cannot_override_exact_row() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let payload = b"the exact registry row remains the commit authority";
    let (data_root, quarantine_path) = prepare_registered_quarantine(
        &db,
        &profile_root,
        "proj_registered_stale_marker",
        "store_registered_stale_marker",
        payload,
    )
    .await;
    let quarantine_name = quarantine_path.file_name().unwrap().to_str().unwrap();
    std::fs::write(
        quarantine_path.with_file_name(format!("{quarantine_name}.receipt-v1.json.retired")),
        [],
    )
    .unwrap();

    assert_eq!(
        recover_existing_store_quarantine(
            &profile_root,
            &data_root,
            unbounded_collection_control()
        )
        .unwrap(),
        vec![QuarantineRecoveryOutcome::Retained {
            quarantine_path: quarantine_path.clone(),
            actual_path: quarantine_path.clone(),
            failure: None,
        }],
        "registered named recovery has no database authority to consume the marker"
    );
    assert_eq!(
        std::fs::read(quarantine_path.join("payload.bin")).unwrap(),
        payload
    );

    let (outcome, retired) =
        execute_registered_collection(&db, &CollectionPlan::default(), &profile_root)
            .await
            .unwrap();

    assert_eq!(retired, 0);
    assert!(outcome.collected.is_empty());
    assert!(outcome.errors.is_empty(), "{outcome:#?}");
    assert_eq!(
        std::fs::read(data_root.join("payload.bin")).unwrap(),
        payload
    );
    assert!(!quarantine_path.exists());
    assert_eq!(
        db.try_list_store_instances_for_project("proj_registered_stale_marker")
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        read_pending_quarantine_receipts(&profile_root)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn empty_plan_removes_registered_quarantine_when_exact_row_is_absent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let payload = b"exact registered bytes retire after the database commit";
    let (data_root, quarantine_path) = prepare_registered_quarantine(
        &db,
        &profile_root,
        "proj_registered_remove",
        "store_registered_remove",
        payload,
    )
    .await;
    let transaction = db.begin_write_transaction().await.unwrap();
    assert_eq!(
        transaction
            .execute(
                "DELETE FROM store_instances
                 WHERE project_id = ?1 AND store_id = ?2
                   AND store_relpath = ?3 AND created_at = ?4
                   AND last_write_at IS ?5",
                tracedecay_runtime_core::db::engine::params![
                    "proj_registered_remove",
                    "store_registered_remove",
                    "stores/store_registered_remove",
                    1_700_000_000i64,
                    Some(1_700_000_000i64)
                ],
            )
            .await
            .unwrap(),
        1
    );
    transaction.commit().await.unwrap();

    let (outcome, retired) =
        execute_registered_collection(&db, &CollectionPlan::default(), &profile_root)
            .await
            .unwrap();

    assert_eq!(retired, 0);
    assert!(outcome.collected.is_empty());
    assert_eq!(outcome.reclaimed_bytes, 0);
    assert!(outcome.errors.is_empty(), "{outcome:#?}");
    assert!(!data_root.exists());
    assert!(!quarantine_path.exists());
    assert!(
        db.try_list_store_instances_for_project("proj_registered_remove")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        read_pending_quarantine_receipts(&profile_root)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn empty_plan_retains_registered_quarantine_when_row_fence_changed() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let payload = b"changed registry authority cannot consume these bytes";
    let (data_root, quarantine_path) = prepare_registered_quarantine(
        &db,
        &profile_root,
        "proj_registered_changed",
        "store_registered_changed",
        payload,
    )
    .await;
    let transaction = db.begin_write_transaction().await.unwrap();
    assert_eq!(
        transaction
            .execute(
                "UPDATE store_instances SET last_write_at = ?3
                 WHERE project_id = ?1 AND store_id = ?2",
                tracedecay_runtime_core::db::engine::params![
                    "proj_registered_changed",
                    "store_registered_changed",
                    1_700_000_001i64
                ],
            )
            .await
            .unwrap(),
        1
    );
    transaction.commit().await.unwrap();

    let (outcome, retired) =
        execute_registered_collection(&db, &CollectionPlan::default(), &profile_root)
            .await
            .unwrap();

    assert_eq!(retired, 0);
    assert!(outcome.collected.is_empty());
    assert_eq!(outcome.reclaimed_bytes, 0);
    assert_eq!(
        outcome.errors,
        vec![CollectionFailure {
            store_id: "store_registered_changed".to_owned(),
            kind: CollectionFailureKind::RegistryChanged,
        }]
    );
    assert_eq!(
        outcome.recovery_receipts,
        vec![CollectionRecoveryReceipt {
            store_id: "store_registered_changed".to_owned(),
            original_path: data_root.clone(),
            quarantine_path: quarantine_path.clone(),
            actual_path: quarantine_path.clone(),
            action: CollectionRecoveryAction::RetainedForRecovery,
        }]
    );
    assert!(!data_root.exists());
    assert_eq!(
        std::fs::read(quarantine_path.join("payload.bin")).unwrap(),
        payload
    );
    let rows = db
        .try_list_store_instances_for_project("proj_registered_changed")
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].last_write_at, Some(1_700_000_001));
    assert_eq!(
        read_pending_quarantine_receipts(&profile_root)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn registered_recovery_completes_without_fabricating_collection_totals() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/registered-recovery-consumer");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(data_root.join("payload.bin"), b"resume exactly once").unwrap();
    let expected = capture_store_content_fence(&profile_root, &data_root).unwrap();

    let result =
        quarantine_store_for_verified_collection(&profile_root, &data_root, &expected).unwrap();
    let QuarantineStoreOutcome::Verified(quarantine) = result else {
        panic!("fixture must reach verified quarantine");
    };
    quarantine.mark_retirement_committed().unwrap();
    drop(quarantine);
    let mut outcome = CollectionOutcome::default();

    assert!(!reconcile_existing_quarantine(
        &profile_root,
        &data_root,
        "registered-recovery-consumer",
        &mut outcome,
        unbounded_collection_control(),
    ));
    assert_eq!(outcome, CollectionOutcome::default());
    assert!(reconcile_existing_quarantine(
        &profile_root,
        &data_root,
        "registered-recovery-consumer",
        &mut outcome,
        unbounded_collection_control(),
    ));
    assert_eq!(outcome.reclaimed_bytes, 0);
    assert!(outcome.collected.is_empty());
}

#[test]
fn committed_journal_recovery_retains_an_identity_replacement() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/committed-identity-replacement");
    let original_payload = b"exact committed quarantine bytes";
    let replacement_payload = b"replacement must never inherit delete authority";
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(data_root.join("payload.bin"), original_payload).unwrap();
    let expected = capture_store_content_fence(&profile_root, &data_root).unwrap();
    let StoreContentFence::Present(expected_inventory) = &expected else {
        panic!("fixture must capture an exact present-store fence");
    };
    let expected_root_identity = expected_inventory.root.clone();

    let result =
        quarantine_store_for_verified_collection(&profile_root, &data_root, &expected).unwrap();
    let QuarantineStoreOutcome::Verified(quarantine) = result else {
        panic!("fixture must reach verified quarantine");
    };
    let quarantine_path = quarantine.quarantine_path().to_path_buf();
    quarantine.mark_retirement_committed().unwrap();
    drop(quarantine);
    let displaced = profile_root.join("stores/exact-committed-quarantine");
    std::fs::rename(&quarantine_path, &displaced).unwrap();
    std::fs::create_dir_all(&quarantine_path).unwrap();
    std::fs::write(quarantine_path.join("payload.bin"), replacement_payload).unwrap();

    let outcomes = recover_existing_store_quarantine(
        &profile_root,
        &data_root,
        unbounded_collection_control(),
    )
    .unwrap();

    assert_eq!(
        outcomes,
        vec![QuarantineRecoveryOutcome::Retained {
            quarantine_path: quarantine_path.clone(),
            actual_path: quarantine_path.clone(),
            failure: Some(CollectionMutationFailure {
                operation: CollectionMutationOperation::ValidateRestoredStoreIdentity,
                raw_os_error: None,
                target_path: quarantine_path.clone(),
                expected_root_identity: Some(expected_root_identity),
                classification: CollectionMutationFailureClassification::NonRetryable,
            }),
        }]
    );
    assert_eq!(
        std::fs::read(displaced.join("payload.bin")).unwrap(),
        original_payload
    );
    assert_eq!(
        std::fs::read(quarantine_path.join("payload.bin")).unwrap(),
        replacement_payload
    );
    assert!(!data_root.exists());
    assert_eq!(
        read_pending_quarantine_receipts(&profile_root)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn legacy_journal_without_identity_fails_closed_and_preserves_exact_bytes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/legacy-journal");
    let payload = b"legacy journal bytes must remain untouched";
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(data_root.join("payload.bin"), payload).unwrap();
    let expected = capture_store_content_fence(&profile_root, &data_root).unwrap();

    let result =
        quarantine_store_for_verified_collection(&profile_root, &data_root, &expected).unwrap();
    let QuarantineStoreOutcome::Verified(quarantine) = result else {
        panic!("fixture must reach verified quarantine");
    };
    let quarantine_path = quarantine.quarantine_path().to_path_buf();
    drop(quarantine);
    let quarantine_name = quarantine_path.file_name().unwrap().to_str().unwrap();
    let journal_path = quarantine_path.with_file_name(format!("{quarantine_name}.receipt-v1.json"));
    let legacy_journal = br#"{"version":1,"kind":"Unregistered","project_id":"test-project","store_id":"test-store","original_name":"legacy-journal","registry_fence":null}"#;
    std::fs::write(&journal_path, legacy_journal).unwrap();

    let outcomes = recover_existing_store_quarantine(
        &profile_root,
        &data_root,
        unbounded_collection_control(),
    )
    .unwrap();

    assert_eq!(
        outcomes,
        vec![QuarantineRecoveryOutcome::Retained {
            quarantine_path: quarantine_path.clone(),
            actual_path: quarantine_path.clone(),
            failure: Some(CollectionMutationFailure {
                operation: CollectionMutationOperation::ProbeRecoveryJournal,
                raw_os_error: None,
                target_path: journal_path.clone(),
                expected_root_identity: None,
                classification: CollectionMutationFailureClassification::NonRetryable,
            }),
        }]
    );
    assert_eq!(
        std::fs::read(quarantine_path.join("payload.bin")).unwrap(),
        payload
    );
    assert_eq!(std::fs::read(journal_path).unwrap(), legacy_journal);
    assert!(!data_root.exists());
}

#[test]
fn unreadable_pre_rename_journal_reports_the_observed_original_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/unreadable-pre-rename-journal");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(data_root.join("payload.bin"), b"original bytes").unwrap();
    let expected = capture_store_content_fence(&profile_root, &data_root).unwrap();

    let result =
        quarantine_store_for_verified_collection(&profile_root, &data_root, &expected).unwrap();
    let QuarantineStoreOutcome::Verified(quarantine) = result else {
        panic!("fixture must reach verified quarantine");
    };
    let quarantine_path = quarantine.quarantine_path().to_path_buf();
    drop(quarantine);
    let quarantine_name = quarantine_path.file_name().unwrap().to_str().unwrap();
    let journal_path = quarantine_path.with_file_name(format!("{quarantine_name}.receipt-v1.json"));
    std::fs::rename(&quarantine_path, &data_root).unwrap();
    std::fs::remove_file(&journal_path).unwrap();
    std::fs::create_dir(&journal_path).unwrap();

    let outcomes = recover_existing_store_quarantine(
        &profile_root,
        &data_root,
        unbounded_collection_control(),
    )
    .unwrap();

    let [
        QuarantineRecoveryOutcome::Retained {
            actual_path,
            quarantine_path: retained_quarantine_path,
            failure: Some(failure),
        },
    ] = outcomes.as_slice()
    else {
        panic!("unreadable journal must retain the observed store: {outcomes:#?}");
    };
    assert_eq!(actual_path, &data_root);
    assert_eq!(retained_quarantine_path, &quarantine_path);
    assert_eq!(
        failure.operation,
        CollectionMutationOperation::ProbeRecoveryJournal
    );
    assert_eq!(failure.target_path, journal_path);
    assert_eq!(
        std::fs::read(data_root.join("payload.bin")).unwrap(),
        b"original bytes"
    );
}

#[test]
fn unregistered_pre_rename_journal_clears_at_exact_original() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/missing-quarantine");
    let payload = b"restored bytes cannot imply journal completion";
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(data_root.join("payload.bin"), payload).unwrap();
    let expected = capture_store_content_fence(&profile_root, &data_root).unwrap();

    let result =
        quarantine_store_for_verified_collection(&profile_root, &data_root, &expected).unwrap();
    let QuarantineStoreOutcome::Verified(quarantine) = result else {
        panic!("fixture must reach verified quarantine");
    };
    let quarantine_path = quarantine.quarantine_path().to_path_buf();
    drop(quarantine);
    let quarantine_name = quarantine_path.file_name().unwrap().to_str().unwrap();
    let journal_path = quarantine_path.with_file_name(format!("{quarantine_name}.receipt-v1.json"));
    let renamed_marker =
        quarantine_path.with_file_name(format!("{quarantine_name}.receipt-v1.json.renamed"));
    let retired_marker =
        quarantine_path.with_file_name(format!("{quarantine_name}.receipt-v1.json.retired"));
    std::fs::rename(&quarantine_path, &data_root).unwrap();

    let outcomes = recover_existing_store_quarantine(
        &profile_root,
        &data_root,
        unbounded_collection_control(),
    )
    .unwrap();

    assert_eq!(
        outcomes,
        vec![QuarantineRecoveryOutcome::Restored {
            restored_path: data_root.clone(),
            failure: None,
        }]
    );
    assert_eq!(
        std::fs::read(data_root.join("payload.bin")).unwrap(),
        payload
    );
    assert_eq!(
        capture_store_content_fence(&profile_root, &data_root).unwrap(),
        expected
    );
    assert!(!quarantine_path.exists());
    assert!(!journal_path.exists());
    assert!(!renamed_marker.exists());
    assert!(!retired_marker.exists());
    assert!(
        read_pending_quarantine_receipts(&profile_root)
            .unwrap()
            .is_empty()
    );
}

/// A restore rename can complete before the parent-directory sync or journal
/// cleanup. The mounted reader must expose the bytes' original live path,
/// rather than the now-absent quarantine name, while retaining the receipt.
#[test]
fn pending_quarantine_reader_reports_restored_path_when_sync_is_unconfirmed() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/restore-sync-boundary");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(
        data_root.join("payload.bin"),
        b"restore location is authoritative",
    )
    .unwrap();
    let expected = capture_store_content_fence(&profile_root, &data_root).unwrap();

    let result =
        quarantine_store_for_verified_collection(&profile_root, &data_root, &expected).unwrap();
    let QuarantineStoreOutcome::Verified(quarantine) = result else {
        panic!("fixture must reach verified quarantine");
    };
    let quarantine_path = quarantine.quarantine_path().to_path_buf();
    drop(quarantine);
    // This is the persisted shape after the rename succeeds but a later
    // directory sync/journal cleanup cannot be confirmed.
    std::fs::rename(&quarantine_path, &data_root).unwrap();

    let receipts = read_pending_quarantine_receipts(&profile_root).unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].quarantine_path, quarantine_path);
    assert_eq!(receipts[0].actual_path, data_root);
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

/// Once SQL retirement has been marked, cancellation during recursive remove
/// retains the journal-backed quarantine rather than reporting reclaimed
/// bytes. Restart reconciliation owns the remaining irreversible work.
#[test]
fn cancelled_quarantine_finalization_retains_a_readable_recovery_record() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/cancelled-finalize");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(data_root.join("payload.bin"), b"retain while cancelled").unwrap();
    let expected = capture_store_content_fence(&profile_root, &data_root).unwrap();
    let result =
        quarantine_store_for_verified_collection(&profile_root, &data_root, &expected).unwrap();
    let QuarantineStoreOutcome::Verified(quarantine) = result else {
        panic!("fixture must reach verified quarantine");
    };
    quarantine.mark_retirement_committed().unwrap();
    let quarantine_path = quarantine.quarantine_path().to_path_buf();
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    assert!(matches!(
        quarantine.finalize(CollectionControl::new(
            &cancellation,
            MonotonicDeadline::at(Instant::now() + Duration::from_secs(1),),
        )),
        QuarantineFinalizeOutcome::Interrupted { .. }
    ));
    assert!(quarantine_path.is_dir());
    assert_eq!(
        read_pending_quarantine_receipts(&profile_root)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn legacy_recovery_rejects_post_rename_identity_replacement() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let projects = profile_root.join("projects");
    let data_root = projects.join("proj_identity_race");
    let quarantine_name = ".tracedecay-orphan-quarantine-proj_identity_race-42-7";
    let quarantine = projects.join(quarantine_name);
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(data_root.join("payload.bin"), b"exact quarantined bytes").unwrap();
    let expected = capture_store_content_fence(&profile_root, &data_root).unwrap();
    let StoreContentFence::Present(inventory) = expected else {
        panic!("fixture must capture the legacy quarantine identity");
    };
    let expected_root_identity = inventory.root;
    std::fs::rename(&data_root, &quarantine).unwrap();

    let outcome = recover_named_store_quarantine_controlled(
        &profile_root,
        &data_root,
        std::ffi::OsStr::new(quarantine_name),
        &projects,
        || {
            std::fs::rename(&data_root, &quarantine).unwrap();
            std::fs::create_dir_all(&data_root).unwrap();
            std::fs::write(data_root.join("payload.bin"), b"replacement live bytes").unwrap();
        },
    )
    .unwrap();

    assert_eq!(
        outcome,
        Some(QuarantineRecoveryOutcome::Retained {
            quarantine_path: quarantine.clone(),
            actual_path: quarantine.clone(),
            failure: Some(CollectionMutationFailure {
                operation: CollectionMutationOperation::RestoreLiveLeafFromQuarantine,
                raw_os_error: Some(OCCUPIED_RENAME_RAW_OS_ERROR),
                target_path: quarantine.clone(),
                expected_root_identity: Some(expected_root_identity),
                classification: CollectionMutationFailureClassification::NonRetryable,
            }),
        })
    );
    assert_eq!(
        std::fs::read(quarantine.join("payload.bin")).unwrap(),
        b"exact quarantined bytes"
    );
    assert_eq!(
        std::fs::read(data_root.join("payload.bin")).unwrap(),
        b"replacement live bytes"
    );
}

#[cfg(windows)]
#[test]
fn unreadable_recovery_journal_is_a_retryable_typed_failure() {
    let journal_path = PathBuf::from(
        r"C:\profile\projects\.tracedecay-orphan-quarantine-proj_journal-42-7.receipt-v1.json",
    );
    let expected_root_identity = StoreRootIdentity {
        device: 17,
        inode: 23,
    };

    assert_eq!(
        classify_recovery_journal_probe(
            Err(std::io::Error::from_raw_os_error(32)),
            journal_path.clone(),
            &expected_root_identity,
        ),
        Err(CollectionFailureKind::RemoveFailed(
            CollectionMutationFailure {
                operation: CollectionMutationOperation::ProbeRecoveryJournal,
                raw_os_error: Some(32),
                target_path: journal_path,
                expected_root_identity: Some(expected_root_identity),
                classification: CollectionMutationFailureClassification::RetryableDeferred,
            }
        ))
    );
}

/// Unregistered projects are an on-disk-only class, but their retention work
/// still advances through a bounded, resumable page rather than recursing the
/// entire profile under a single writer admission.
#[test]
fn committed_unregistered_recovery_preserves_interrupted_control_and_resumes() {
    for expected_completion in [
        CollectionCompletionV1::Cancelled,
        CollectionCompletionV1::DeadlineExceeded,
    ] {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        let projects = profile_root.join("projects");
        let data_root = projects.join("proj_controlled_recovery");
        std::fs::create_dir_all(data_root.join("nested")).unwrap();
        std::fs::write(
            data_root.join("nested/payload.bin"),
            b"retained exact bytes",
        )
        .unwrap();
        let expected = capture_store_content_fence(&profile_root, &data_root).unwrap();
        let QuarantineStoreOutcome::Verified(quarantine) =
            quarantine_store_for_verified_collection(&profile_root, &data_root, &expected).unwrap()
        else {
            panic!("fixture must reach verified quarantine");
        };
        let quarantine_path = quarantine.quarantine_path().to_path_buf();
        quarantine.mark_retirement_committed().unwrap();
        drop(quarantine);
        let pending = read_pending_quarantine_receipts(&profile_root).unwrap();
        assert_eq!(pending.len(), 1);
        assert!(pending[0].retirement_committed);
        let cancellation = CancellationToken::new();
        let deadline = if expected_completion == CollectionCompletionV1::DeadlineExceeded {
            MonotonicDeadline::at(Instant::now())
        } else {
            cancellation.cancel();
            MonotonicDeadline::at(Instant::now() + Duration::from_secs(1))
        };
        let control = CollectionControl::new(&cancellation, deadline);
        let outcome = recover_named_store_quarantine(
            &profile_root,
            &data_root,
            quarantine_path.file_name().unwrap(),
            &projects,
            control,
        )
        .unwrap();
        assert_eq!(
            outcome,
            Some(QuarantineRecoveryOutcome::Retained {
                quarantine_path: quarantine_path.clone(),
                actual_path: quarantine_path.clone(),
                failure: None,
            })
        );
        assert_eq!(control.completion(), Some(expected_completion));
        assert_eq!(
            std::fs::read(quarantine_path.join("nested/payload.bin")).unwrap(),
            b"retained exact bytes"
        );
        assert_eq!(
            read_pending_quarantine_receipts(&profile_root).unwrap(),
            pending
        );
        assert!(!data_root.exists());

        let mut collection = CollectionOutcome::default();
        assert!(!reconcile_existing_quarantine(
            &profile_root,
            &data_root,
            "proj_controlled_recovery",
            &mut collection,
            control,
        ));
        assert_eq!(collection.completion, expected_completion);
        assert_eq!(collection.reclaimed_bytes, 0);
        assert!(collection.collected.is_empty());
        assert_eq!(
            read_pending_quarantine_receipts(&profile_root).unwrap(),
            pending
        );

        let fresh = CancellationToken::new();
        let resumed = recover_named_store_quarantine(
            &profile_root,
            &data_root,
            quarantine_path.file_name().unwrap(),
            &projects,
            CollectionControl::new(
                &fresh,
                MonotonicDeadline::at(Instant::now() + Duration::from_secs(1)),
            ),
        )
        .unwrap();
        assert_eq!(
            resumed,
            Some(QuarantineRecoveryOutcome::Removed {
                quarantine_path: quarantine_path.clone(),
                journal_failure: None,
            })
        );
        assert!(!quarantine_path.exists());
        assert!(
            read_pending_quarantine_receipts(&profile_root)
                .unwrap()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn unregistered_recovery_removal_has_no_fabricated_bytes_or_receipt() {
    for already_removed in [false, true] {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        let data_root = profile_root.join("projects/proj_committed_recovery");
        std::fs::create_dir_all(&data_root).unwrap();
        std::fs::write(data_root.join("payload.bin"), b"remove through pager").unwrap();
        let expected = capture_store_content_fence(&profile_root, &data_root).unwrap();
        let result =
            quarantine_store_for_verified_collection(&profile_root, &data_root, &expected).unwrap();
        let QuarantineStoreOutcome::Verified(quarantine) = result else {
            panic!("fixture must reach verified quarantine");
        };
        let quarantine_path = quarantine.quarantine_path().to_path_buf();
        quarantine.mark_retirement_committed().unwrap();
        drop(quarantine);
        if already_removed {
            // Model a crash after deleting the committed leaf but before clearing
            // its journal. The pager must discover the journal without a directory.
            std::fs::remove_dir_all(&quarantine_path).unwrap();
        }
        let (_runtime, db) = open_registered_db(&profile_root).await;
        let cancellation = CancellationToken::new();

        let report = sweep_unregistered_store_page(
            &db,
            &profile_root,
            UnregisteredStoreSweepRequestV1 {
                cursor: None,
                limit: 4,
                retention_secs: 0,
                now: 1_700_000_000,
                apply: true,
                cancellation: &cancellation,
                deadline: MonotonicDeadline::at(Instant::now() + Duration::from_secs(1)),
            },
        )
        .await
        .unwrap();

        assert_eq!(report.completion, UnregisteredSweepCompletionV1::Complete);
        assert_eq!(report.outcome.reclaimed_bytes, 0);
        assert!(report.outcome.collected.is_empty());
        assert!(report.outcome.recovery_receipts.is_empty());
        assert!(report.outcome.errors.is_empty());
        assert!(!quarantine_path.exists());
        assert!(
            read_pending_quarantine_receipts(&profile_root)
                .unwrap()
                .is_empty()
        );
    }
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
        let store = tempfile::tempdir().unwrap();
        let custom = PathBuf::from("scopes/custom-scope.db");

        let DurableDatabaseInventoryV1::Resolved(inventory) = durable_database_inventory(
            store.path(),
            Some(&manifest_bytes("custom-main.db")),
            std::slice::from_ref(&custom),
            unbounded_collection_control(),
        ) else {
            panic!("a readable manifest must resolve an inventory");
        };

        assert!(
            inventory.contains(&PathBuf::from("custom-main.db")),
            "the manifest's custom main graph path must be honoured, not the default filename"
        );
        assert!(inventory.contains(&custom));
    }

    #[test]
    fn branch_databases_are_part_of_the_inventory() {
        let store = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(store.path().join("branches")).unwrap();
        std::fs::write(store.path().join("branches/feature-x.db"), b"").unwrap();
        std::fs::write(store.path().join("branches/main.db"), b"").unwrap();
        std::fs::write(store.path().join("branches/notes.txt"), b"").unwrap();

        let DurableDatabaseInventoryV1::Resolved(inventory) = durable_database_inventory(
            store.path(),
            Some(&manifest_bytes("code.db")),
            &[],
            unbounded_collection_control(),
        ) else {
            panic!("a readable manifest must resolve an inventory");
        };

        assert!(
            inventory.contains(&PathBuf::from("branches/feature-x.db")),
            "a branch database can hold the only surviving durable rows"
        );
        assert!(inventory.contains(&PathBuf::from("branches/main.db")));
        assert!(!inventory.contains(&PathBuf::from("branches/notes.txt")));
    }

    #[test]
    fn a_missing_manifest_fails_closed() {
        let store = tempfile::tempdir().unwrap();
        assert_eq!(
            durable_database_inventory(store.path(), None, &[], unbounded_collection_control()),
            DurableDatabaseInventoryV1::Unverifiable,
            "without a manifest the store's graph path is a guess, not a fact"
        );
    }

    #[test]
    fn manifest_graph_path_must_be_normalized_relative() {
        for graph_path in [PathBuf::from(""), PathBuf::from("../graph.db")] {
            let bytes = manifest_bytes(graph_path.to_string_lossy().as_ref());
            assert_eq!(
                durable_database_inventory(
                    Path::new("/tmp/store"),
                    Some(&bytes),
                    &[],
                    unbounded_collection_control(),
                ),
                DurableDatabaseInventoryV1::Unverifiable,
                "graph path {graph_path:?} must not escape the store"
            );
        }

        assert_eq!(
            durable_database_inventory(
                Path::new("/tmp/store"),
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
                Path::new("/tmp/store"),
                Some(&manifest_bytes("graph.db")),
                &[PathBuf::from("scopes/../../escape.db")],
                unbounded_collection_control(),
            ),
            DurableDatabaseInventoryV1::Unverifiable
        );
        assert_eq!(
            durable_database_inventory(
                Path::new("/tmp/store"),
                Some(&manifest_bytes("graph.db")),
                &[PathBuf::from("/tmp/escape.db")],
                unbounded_collection_control(),
            ),
            DurableDatabaseInventoryV1::Unverifiable
        );
    }

    #[test]
    fn cancelled_control_interrupts_branch_database_inventory() {
        let store = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(store.path().join("branches")).unwrap();
        std::fs::write(store.path().join("branches/only-memory.db"), b"").unwrap();
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        assert_eq!(
            durable_database_inventory(
                store.path(),
                Some(&manifest_bytes("graph.db")),
                &[],
                CollectionControl::new(
                    &cancellation,
                    MonotonicDeadline::at(Instant::now() + Duration::from_secs(1)),
                ),
            ),
            DurableDatabaseInventoryV1::Interrupted,
            "a cancelled admission must not finish the lazy branch scan"
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
