use super::*;

#[tokio::test]
async fn sweep_collects_orphan_store_and_retires_row() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();

    // Live repository root that still exists on disk.
    let live_root = tmp.path().join("live-repo");
    std::fs::create_dir_all(&live_root).unwrap();
    // Orphan identity: canonical + display roots that no longer exist.
    let dead_root = tmp.path().join("moved-away-repo");

    let (_runtime, db) = open_registered_db(&profile_root).await;

    // Anchor timestamps at a real epoch base so the recorded last-write drives
    // the age (not the freshly-written file mtime, which would be "now").
    let base = 1_700_000_000i64;
    seed_store(
        &db,
        &profile_root,
        "proj_live",
        "store_live",
        &live_root,
        base,
    )
    .await;
    let orphan_data_root = seed_store(
        &db,
        &profile_root,
        "proj_orphan",
        "store_orphan",
        &dead_root,
        base - 100 * DAY,
    )
    .await;
    assert!(orphan_data_root.exists());

    let now = base;
    // Dry run: plan classifies orphan, mutates nothing.
    let report = sweep_orphan_stores(&db, &profile_root, 7 * DAY, now, false)
        .await
        .unwrap();
    assert_eq!(report.plan.collect.len(), 1, "one orphan should be planned");
    assert_eq!(report.plan.collect[0].store_id, "store_orphan");
    assert!(!report.applied);
    assert!(orphan_data_root.exists(), "dry run must not delete");

    // Apply: orphan store removed, row retired, live store untouched.
    let report = sweep_orphan_stores(&db, &profile_root, 7 * DAY, now, true)
        .await
        .unwrap();
    assert!(report.applied);
    assert_eq!(
        report.outcome.collected.len(),
        1,
        "ordinary empty stores must remain collectable: {report:#?}"
    );
    assert_eq!(report.retired_registry_rows, 1);
    assert!(!orphan_data_root.exists(), "orphan store must be collected");
    assert!(live_root.exists());

    let live_data_root = profile_root.join("stores/store_live");
    assert!(live_data_root.exists(), "live store must be untouched");

    let remaining: Vec<_> = db
        .list_code_projects(usize::MAX)
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.project_id)
        .collect();
    assert!(remaining.contains(&"proj_live".to_string()));
    assert!(
        !remaining.contains(&"proj_orphan".to_string()),
        "orphan identity row must be retired"
    );
}

/// The collection plan is only an inspection receipt.  Replacing its directory
/// with byte-identical contents in the same timestamp second must still abort
/// the apply rather than retire a newly-created store identity.
#[tokio::test]
async fn sweep_preserves_immature_sibling_store_identity() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let dead_root = tmp.path().join("moved-away-repo");
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let now = 1_700_000_000i64;
    let old_root = seed_store(
        &db,
        &profile_root,
        "proj_orphan",
        "store_old",
        &dead_root,
        now - 100 * DAY,
    )
    .await;
    let young_root = seed_store(
        &db,
        &profile_root,
        "proj_orphan",
        "store_young",
        &dead_root,
        now - DAY,
    )
    .await;

    let report = sweep_orphan_stores(&db, &profile_root, 7 * DAY, now, true)
        .await
        .unwrap();

    assert_eq!(report.retired_registry_rows, 1);
    assert!(!old_root.exists());
    assert!(young_root.exists());
    let stores = db
        .try_list_store_instances_for_project("proj_orphan")
        .await
        .unwrap();
    assert_eq!(
        stores
            .into_iter()
            .map(|store| store.store_id)
            .collect::<Vec<_>>(),
        vec!["store_young"]
    );
    assert!(
        db.list_code_projects(usize::MAX)
            .await
            .unwrap()
            .into_iter()
            .any(|project| project.project_id == "proj_orphan")
    );
}

#[tokio::test]
async fn sweep_atomically_relinks_moved_store_to_registered_live_project() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let dead_root = tmp.path().join("old-repository-root");
    let live_root = tmp.path().join("renamed-repository-root");
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

    let manifest = StoreManifest {
        schema_version: STORE_MANIFEST_SCHEMA_VERSION,
        project_id: Some("proj_old".to_string()),
        store_kind: StoreKind::CodeProject,
        storage_mode: StorageMode::ProfileSharded,
        project_root: live_root,
        data_root: store_root.clone(),
        graph_db_relpath: PathBuf::from("graph.db"),
        sessions_db_relpath: PathBuf::from("sessions.db"),
        branch_meta_relpath: PathBuf::from(tracedecay_runtime_core::storage::BRANCH_META_FILENAME),
    };
    std::fs::write(
        store_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let report = sweep_orphan_stores(&db, &profile_root, 7 * DAY, 1_700_000_000, true)
        .await
        .unwrap();

    assert_eq!(report.relinked_registry_rows, 1);
    assert!(report.outcome.collected.is_empty());
    assert!(store_root.exists(), "re-link must not delete store payload");
    assert!(
        db.try_list_store_instances_for_project("proj_old")
            .await
            .unwrap()
            .is_empty()
    );
    let target_stores = db
        .try_list_store_instances_for_project("proj_live")
        .await
        .unwrap();
    assert_eq!(target_stores.len(), 1);
    assert_eq!(target_stores[0].store_id, "store_moved");
    assert_eq!(target_stores[0].project_id, "proj_live");
    let relinked_manifest = tracedecay_runtime_core::storage::read_store_manifest(
        &store_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
    )
    .unwrap();
    assert_eq!(relinked_manifest.project_id.as_deref(), Some("proj_live"));
    assert!(
        !db.list_code_projects(usize::MAX)
            .await
            .unwrap()
            .into_iter()
            .any(|project| project.project_id == "proj_old")
    );
}

#[tokio::test]
async fn sweep_resumes_manifest_forward_after_interrupted_relink() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let dead_root = tmp.path().join("old-repository-root");
    let live_root = tmp.path().join("renamed-repository-root");
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
    manifest.project_id = Some("proj_live".to_string());
    manifest.project_root = live_root;
    tracedecay_runtime_core::storage::write_store_manifest_to_path(
        &store_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
        &manifest,
    )
    .unwrap();

    let report = sweep_orphan_stores(&db, &profile_root, 7 * DAY, 1_700_000_000, true)
        .await
        .unwrap();

    assert_eq!(report.relinked_registry_rows, 1);
    assert!(
        db.try_list_store_instances_for_project("proj_old")
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        db.try_list_store_instances_for_project("proj_live")
            .await
            .unwrap()
            .into_iter()
            .map(|store| store.store_id)
            .collect::<Vec<_>>(),
        vec!["store_moved"]
    );
}

#[tokio::test]
async fn sweep_leaves_relinkable_store_unchanged_without_exact_target_registration() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let dead_root = tmp.path().join("old-repository-root");
    let unregistered_live_root = tmp.path().join("unregistered-live-root");
    std::fs::create_dir_all(&unregistered_live_root).unwrap();
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
    let mut manifest = tracedecay_runtime_core::storage::read_store_manifest(
        &store_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
    )
    .unwrap();
    manifest.project_root = unregistered_live_root;
    std::fs::write(
        store_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let report = sweep_orphan_stores(&db, &profile_root, 7 * DAY, 1_700_000_000, true)
        .await
        .unwrap();

    assert_eq!(report.relinked_registry_rows, 0);
    assert!(report.outcome.collected.is_empty());
    assert!(store_root.exists());
    let prior = db
        .try_list_store_instances_for_project("proj_old")
        .await
        .unwrap();
    assert_eq!(prior.len(), 1);
    assert_eq!(prior[0].store_id, "store_moved");
    let unchanged_manifest = tracedecay_runtime_core::storage::read_store_manifest(
        &store_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
    )
    .unwrap();
    assert_eq!(unchanged_manifest.project_id.as_deref(), Some("proj_old"));
}

#[tokio::test]
async fn registered_store_census_resumes_across_bounded_project_pages() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    for suffix in ["a", "b", "c"] {
        seed_store(
            db.as_ref(),
            &profile_root,
            &format!("project-{suffix}"),
            &format!("store-{suffix}"),
            &tmp.path().join(format!("missing-{suffix}")),
            1_700_000_000,
        )
        .await;
    }

    let first = build_store_census_page(db.as_ref(), &profile_root, None, 2)
        .await
        .unwrap();
    assert_eq!(first.entries.len(), 2);
    assert_eq!(first.next_cursor.as_deref(), Some("project-b"));

    let second =
        build_store_census_page(db.as_ref(), &profile_root, first.next_cursor.as_deref(), 2)
            .await
            .unwrap();
    assert_eq!(second.entries.len(), 1);
    assert_eq!(second.entries[0].project_id, "project-c");
    assert!(second.next_cursor.is_none());
}

// === Durable-memory guard ===================================================

/// A store whose graph database carries durable `memory_facts` rows must
/// never be collected, even when every registry/manifest/payload revival
/// check passes and the store is otherwise a textbook orphan.
#[tokio::test]
async fn census_finds_unregistered_project_dir_and_ignores_registered_ones() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;

    let registered_root = tmp.path().join("registered-repo");
    std::fs::create_dir_all(&registered_root).unwrap();
    seed_project(&db, "proj_registered", &registered_root, 1_700_000_000).await;
    let registered_dir = profile_root.join("projects").join("proj_registered");
    std::fs::create_dir_all(&registered_dir).unwrap();

    // A genuinely unregistered directory: no `code_projects` row was ever
    // written for it.
    let unregistered_dir = profile_root.join("projects").join("proj_ghost");
    std::fs::create_dir_all(&unregistered_dir).unwrap();
    std::fs::write(unregistered_dir.join("payload.bin"), vec![0u8; 4096]).unwrap();

    // A stray, unsafely-named entry under `projects/` must never be treated
    // as a candidate.
    std::fs::write(profile_root.join("projects").join("not-a-store.txt"), b"x").unwrap();

    let now = 1_700_100_000i64;
    let findings = census_unregistered_project_dirs(&db, &profile_root, now)
        .await
        .unwrap();

    assert_eq!(findings.len(), 1, "findings: {findings:?}");
    assert_eq!(findings[0].project_dir_name, "proj_ghost");
    assert_eq!(findings[0].data_root, unregistered_dir);
    assert!(findings[0].size_bytes >= 4096);
}

#[tokio::test]
async fn sweep_unregistered_stores_protects_unverifiable_payload_and_retains_young() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;

    let base = 1_700_000_000i64;
    let old_dir = profile_root.join("projects").join("proj_old_ghost");
    std::fs::create_dir_all(&old_dir).unwrap();
    std::fs::write(old_dir.join("payload.bin"), b"old").unwrap();
    filetime::set_file_mtime(
        old_dir.join("payload.bin"),
        filetime::FileTime::from_unix_time(base - 100 * DAY, 0),
    )
    .unwrap();

    let young_dir = profile_root.join("projects").join("proj_young_ghost");
    std::fs::create_dir_all(&young_dir).unwrap();
    std::fs::write(young_dir.join("payload.bin"), b"young").unwrap();
    filetime::set_file_mtime(
        young_dir.join("payload.bin"),
        filetime::FileTime::from_unix_time(base - DAY, 0),
    )
    .unwrap();

    // Dry run: classifies but mutates nothing.
    let report = sweep_unregistered_stores(&db, &profile_root, 7 * DAY, base, false)
        .await
        .unwrap();
    assert_eq!(report.plan.collect.len(), 1);
    assert_eq!(report.plan.collect[0].project_dir_name, "proj_old_ghost");
    assert_eq!(report.plan.retained_immature.len(), 1);
    assert!(!report.applied);
    assert!(old_dir.exists(), "dry run must not delete");

    let report = sweep_unregistered_stores(&db, &profile_root, 7 * DAY, base, true)
        .await
        .unwrap();
    assert!(report.applied);
    assert!(report.outcome.collected.is_empty());
    assert_eq!(report.outcome.errors.len(), 1);
    assert_eq!(
        report.outcome.errors[0].kind,
        CollectionFailureKind::DurableDataProtected
    );
    assert!(
        old_dir.exists(),
        "arbitrary payload without a manifest cannot prove durable-data absence"
    );
    assert!(
        young_dir.exists(),
        "immature unregistered dir must be retained"
    );
}

/// An unregistered store whose own manifest names a project root that no
/// longer exists is debris the moment the census sees it: the retention
/// window exists for stores whose root might still come back, and a missing
/// or unreadable manifest, or a root that is still present, keeps that window.
#[tokio::test]
async fn unregistered_store_with_a_vanished_manifest_root_is_collected_at_once() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let base = 1_700_000_000i64;

    let manifest_for = |data_root: &Path, project_root: &Path| StoreManifest {
        schema_version: STORE_MANIFEST_SCHEMA_VERSION,
        project_id: Some(data_root.file_name().unwrap().to_str().unwrap().to_owned()),
        store_kind: StoreKind::CodeProject,
        storage_mode: StorageMode::ProfileSharded,
        project_root: project_root.to_path_buf(),
        data_root: data_root.to_path_buf(),
        graph_db_relpath: PathBuf::from("tracedecay.db"),
        sessions_db_relpath: PathBuf::from("sessions.db"),
        branch_meta_relpath: PathBuf::from("branch-meta.json"),
    };
    let seed = |name: &str, project_root: Option<&Path>| {
        let data_root = profile_root.join("projects").join(name);
        std::fs::create_dir_all(&data_root).unwrap();
        std::fs::write(data_root.join("sessions.db"), b"fresh payload").unwrap();
        if let Some(project_root) = project_root {
            tracedecay_runtime_core::storage::write_store_manifest_to_path(
                &data_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
                &manifest_for(&data_root, project_root),
            )
            .unwrap();
        }
        data_root
    };

    let vanished_root = tmp.path().join("checkouts").join("deleted-worktree");
    let present_root = tmp.path().join("checkouts").join("still-here");
    std::fs::create_dir_all(&present_root).unwrap();
    let vanished = seed("proj_vanished_root", Some(&vanished_root));
    let present = seed("proj_present_root", Some(&present_root));
    let unmanifested = seed("proj_no_manifest", None);
    // Every payload was written just now: none of them is past the window.
    let findings = census_unregistered_project_dirs(&db, &profile_root, base + 60)
        .await
        .unwrap();
    assert_eq!(findings.len(), 3);
    let plan = plan_unregistered_collection(findings, 7 * DAY);
    assert_eq!(
        plan.collect
            .iter()
            .map(|finding| finding.project_dir_name.as_str())
            .collect::<Vec<_>>(),
        vec!["proj_vanished_root"],
        "only the store whose root is gone skips the retention window"
    );
    assert!(plan.collect[0].abandoned_root);
    assert_eq!(plan.retained_immature.len(), 2);
    assert!(
        plan.retained_immature
            .iter()
            .all(|finding| !finding.abandoned_root)
    );

    let outcome = execute_unregistered_collection(&db, &plan, &profile_root)
        .await
        .unwrap();
    assert_eq!(outcome.collected.len(), 1);
    assert!(outcome.errors.is_empty());
    assert!(!vanished.exists());
    assert!(present.exists());
    assert!(unmanifested.exists());
}

/// A registered project id must never be treated as an unregistered
/// candidate, and re-registering between census and collection must abort
/// the delete for that finding (closing the revival window).
#[tokio::test]
async fn sweep_unregistered_stores_aborts_when_directory_gets_registered_first() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;

    let base = 1_700_000_000i64;
    let dir = profile_root.join("projects").join("proj_became_live");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("payload.bin"), b"payload").unwrap();
    filetime::set_file_mtime(
        dir.join("payload.bin"),
        filetime::FileTime::from_unix_time(base - 100 * DAY, 0),
    )
    .unwrap();

    let findings = census_unregistered_project_dirs(&db, &profile_root, base)
        .await
        .unwrap();
    assert_eq!(findings.len(), 1);
    let plan = plan_unregistered_collection(findings, 7 * DAY);
    assert_eq!(plan.collect.len(), 1);

    // The directory gets registered for real between census and apply.
    let live_root = tmp.path().join("now-live-repo");
    std::fs::create_dir_all(&live_root).unwrap();
    seed_project(&db, "proj_became_live", &live_root, base).await;

    let outcome = execute_unregistered_collection(&db, &plan, &profile_root)
        .await
        .unwrap();
    assert!(outcome.collected.is_empty());
    assert_eq!(outcome.errors.len(), 1);
    assert_eq!(
        outcome.errors[0].kind,
        CollectionFailureKind::RegistryChanged
    );
    assert!(
        dir.exists(),
        "a directory registered before delete must survive"
    );
}

/// An unregistered directory uses the same inspect→confirm→apply boundary as
/// a registered orphan. A same-second replacement of an empty directory must
/// not inherit the original collection decision.
#[test]
fn cancelled_content_census_stops_before_hashing_or_mutation() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/cancelled-census");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(data_root.join("large.bin"), vec![7_u8; 512 * 1024]).unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let result = super::fence::capture_store_content_fence_controlled(
        &profile_root,
        &data_root,
        CollectionControl::new(
            &cancellation,
            MonotonicDeadline::at(Instant::now() + Duration::from_secs(1)),
        ),
    );

    assert_eq!(result, Err(CollectionFailureKind::Cancelled));
    assert!(data_root.join("large.bin").is_file());
}

/// Recursive age and size accounting obey the same cancellation authority as
/// hashing, so a deep store cannot consume an unbounded maintenance budget
/// after the pass has already been cancelled.
#[test]
fn cancelled_mtime_and_size_walks_stop_before_descending() {
    let tmp = tempfile::TempDir::new().unwrap();
    let data_root = tmp.path().join("deep");
    std::fs::create_dir_all(data_root.join("a/b/c")).unwrap();
    std::fs::write(data_root.join("a/b/c/payload.bin"), vec![3_u8; 4096]).unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let control = CollectionControl::new(
        &cancellation,
        MonotonicDeadline::at(Instant::now() + Duration::from_secs(1)),
    );

    assert_eq!(
        newest_mtime_secs_controlled(&data_root, control),
        Err(CollectionFailureKind::Cancelled)
    );
    assert_eq!(
        dir_size_bytes_controlled(&data_root, control),
        Err(CollectionFailureKind::Cancelled)
    );
    assert!(data_root.join("a/b/c/payload.bin").is_file());
}

#[tokio::test]
async fn unregistered_store_sweep_applies_one_cursor_page_at_a_time() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let base = 1_700_000_000i64;
    for name in ["proj_page_a", "proj_page_b", "proj_page_c"] {
        std::fs::create_dir_all(profile_root.join("projects").join(name)).unwrap();
    }
    let cancellation = CancellationToken::new();
    let deadline = functional_sweep_deadline();

    let first = sweep_unregistered_store_page(
        &db,
        &profile_root,
        UnregisteredStoreSweepRequestV1 {
            cursor: None,
            limit: 2,
            retention_secs: 0,
            now: base,
            apply: true,
            cancellation: &cancellation,
            deadline,
        },
    )
    .await
    .unwrap();
    assert_eq!(first.completion, UnregisteredSweepCompletionV1::Complete);
    assert_eq!(first.outcome.collected.len(), 2);
    let cursor = first
        .next_cursor
        .clone()
        .expect("a third directory requires a second page");
    let portable_inventory_path = super::unregistered_page::portable_inventory_path(
        &profile_root,
        cursor.split(':').nth(1).unwrap(),
    );
    let portable_inventory =
        std::fs::read(&portable_inventory_path).expect("read first portable inventory state");

    let second = sweep_unregistered_store_page(
        &db,
        &profile_root,
        UnregisteredStoreSweepRequestV1 {
            cursor: Some(cursor),
            limit: 2,
            retention_secs: 0,
            now: base,
            apply: true,
            cancellation: &cancellation,
            deadline,
        },
    )
    .await
    .unwrap();
    assert_eq!(second.completion, UnregisteredSweepCompletionV1::Complete);
    assert_eq!(second.outcome.collected.len(), 1);
    assert!(second.next_cursor.is_none());
    assert_eq!(
        std::fs::read(portable_inventory_path)
            .expect("resumed portable page keeps the prior inventory"),
        portable_inventory,
        "the second apply page must resume the durable inventory rather than re-scan after its own deletion"
    );
    assert!(
        !profile_root.join("projects/proj_page_a").exists()
            && !profile_root.join("projects/proj_page_b").exists()
            && !profile_root.join("projects/proj_page_c").exists(),
        "both bounded pages must eventually reclaim their disjoint directories"
    );
}

/// A deadline already elapsed at entry must not inspect or advance a cursor
/// page. `DeadlineExceeded` is distinct from a successful empty page.
#[tokio::test]
async fn unregistered_store_sweep_elapsed_deadline_does_not_advance_page_state() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    for name in ["proj_page_a", "proj_page_b", "proj_page_c"] {
        std::fs::create_dir_all(profile_root.join("projects").join(name)).unwrap();
    }
    let cancellation = CancellationToken::new();

    let report = sweep_unregistered_store_page(
        &db,
        &profile_root,
        UnregisteredStoreSweepRequestV1 {
            cursor: None,
            limit: 2,
            retention_secs: 0,
            now: 1_700_000_000,
            apply: true,
            cancellation: &cancellation,
            deadline: MonotonicDeadline::at(Instant::now()),
        },
    )
    .await
    .unwrap();

    assert_eq!(
        report.completion,
        UnregisteredSweepCompletionV1::DeadlineExceeded
    );
    assert!(report.plan.collect.is_empty());
    assert!(report.outcome.collected.is_empty());
    assert!(report.next_cursor.is_none());
    assert!(
        profile_root.join("projects/proj_page_a").is_dir()
            && profile_root.join("projects/proj_page_b").is_dir()
            && profile_root.join("projects/proj_page_c").is_dir(),
        "an already-elapsed deadline must not reclaim any page directory"
    );
    assert!(
        !profile_root
            .join("maintenance/unregistered-project-directory-inventory-v2")
            .exists(),
        "an already-elapsed deadline must not create portable inventory state"
    );
}

#[test]
fn portable_inventory_keeps_partial_progress_across_cancelled_pages() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    for index in 0..32 {
        std::fs::create_dir_all(
            profile_root
                .join("projects")
                .join(format!("proj_partial_{index}")),
        )
        .unwrap();
    }
    let cancellation = CancellationToken::new();
    let deadline = MonotonicDeadline::at(Instant::now() + Duration::from_secs(1));
    let interrupted = || cancellation.is_cancelled() || deadline.is_elapsed_at(Instant::now());
    let page =
        super::unregistered_page::read_project_directory_page(&profile_root, None, 1, &interrupted)
            .unwrap()
            .expect("first bounded portable page completes");
    let cursor = page
        .next_cursor
        .expect("a bounded first chunk leaves durable continuation work");
    let inventory_path = super::unregistered_page::portable_inventory_path(
        &profile_root,
        cursor.split(':').nth(1).unwrap(),
    );
    let partial = std::fs::read(&inventory_path).unwrap();

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let interrupted = || cancelled.is_cancelled() || deadline.is_elapsed_at(Instant::now());
    assert!(
        super::unregistered_page::read_project_directory_page(
            &profile_root,
            Some(&cursor),
            1,
            &interrupted,
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(std::fs::read(&inventory_path).unwrap(), partial);

    super::unregistered_page::forget_portable_inventory_builder_for_test(&inventory_path);

    let interrupted = || cancellation.is_cancelled() || deadline.is_elapsed_at(Instant::now());
    let hydration_page = super::unregistered_page::read_project_directory_page(
        &profile_root,
        Some(&cursor),
        1,
        &interrupted,
    )
    .unwrap()
    .expect("restart hydrates the durable portable inventory in a bounded slice");
    let hydration_cursor = hydration_page
        .next_cursor
        .expect("partial inventory remains resumable after restart");
    let replay_page = super::unregistered_page::read_project_directory_page(
        &profile_root,
        Some(&hydration_cursor),
        1,
        &interrupted,
    )
    .unwrap()
    .expect("restart replays only a bounded source slice");
    let replay_cursor = replay_page
        .next_cursor
        .expect("replay keeps a typed continuation cursor");
    let resumed_page = super::unregistered_page::read_project_directory_page(
        &profile_root,
        Some(&replay_cursor),
        1,
        &interrupted,
    )
    .unwrap()
    .expect("later page resumes the portable inventory");
    assert!(resumed_page.next_cursor.is_some());
    assert!(
        std::fs::read(&inventory_path).unwrap().len() > partial.len(),
        "a later bounded page appends rather than replacing durable partial progress"
    );
}

/// Cancellation is a typed page result and must prevent both inspection and
/// collection; it is not an empty successful census.
#[tokio::test]
async fn unregistered_store_sweep_returns_cancelled_without_mutation() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(profile_root.join("projects/proj_cancelled")).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let report = sweep_unregistered_store_page(
        &db,
        &profile_root,
        UnregisteredStoreSweepRequestV1 {
            cursor: None,
            limit: 1,
            retention_secs: 0,
            now: 1_700_000_000,
            apply: true,
            cancellation: &cancellation,
            deadline: MonotonicDeadline::at(Instant::now() + Duration::from_secs(1)),
        },
    )
    .await
    .unwrap();

    assert_eq!(report.completion, UnregisteredSweepCompletionV1::Cancelled);
    assert!(report.plan.collect.is_empty());
    assert!(report.outcome.collected.is_empty());
    assert!(profile_root.join("projects/proj_cancelled").is_dir());
}

/// A deadline is distinct from cancellation and must also leave the current
/// page untouched. It is surfaced to the maintenance coordinator so it does
/// not checkpoint partial unregistered work as successful progress.
#[tokio::test]
async fn unregistered_store_sweep_returns_deadline_without_mutation() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(profile_root.join("projects/proj_deadline")).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let cancellation = CancellationToken::new();

    let report = sweep_unregistered_store_page(
        &db,
        &profile_root,
        UnregisteredStoreSweepRequestV1 {
            cursor: None,
            limit: 1,
            retention_secs: 0,
            now: 1_700_000_000,
            apply: true,
            cancellation: &cancellation,
            deadline: MonotonicDeadline::at(Instant::now()),
        },
    )
    .await
    .unwrap();

    assert_eq!(
        report.completion,
        UnregisteredSweepCompletionV1::DeadlineExceeded
    );
    assert!(report.plan.collect.is_empty());
    assert!(report.outcome.collected.is_empty());
    assert!(profile_root.join("projects/proj_deadline").is_dir());
}

/// The mounted unregistered pager recognizes a durable quarantine even though
/// it is not a valid `project_id` leaf. It restores the bytes, emits a typed
/// receipt, and deliberately defers deletion until a later fresh census.
#[tokio::test]
async fn unregistered_store_sweep_reconciles_interrupted_quarantine() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let projects = profile_root.join("projects");
    std::fs::create_dir_all(&projects).unwrap();
    let quarantine = projects.join(".tracedecay-orphan-quarantine-proj_paged_recovery-42-7");
    std::fs::create_dir_all(&quarantine).unwrap();
    std::fs::write(quarantine.join("payload.bin"), b"recover through pager").unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let cancellation = CancellationToken::new();

    let report = sweep_unregistered_store_page(
        &db,
        &profile_root,
        UnregisteredStoreSweepRequestV1 {
            cursor: None,
            limit: 1,
            retention_secs: 0,
            now: 1_700_000_000,
            apply: true,
            cancellation: &cancellation,
            deadline: MonotonicDeadline::at(Instant::now() + Duration::from_secs(1)),
        },
    )
    .await
    .unwrap();

    let restored = projects.join("proj_paged_recovery");
    assert_eq!(report.completion, UnregisteredSweepCompletionV1::Complete);
    assert!(report.outcome.collected.is_empty());
    assert_eq!(report.outcome.recovery_receipts.len(), 1);
    assert_eq!(
        std::fs::read(restored.join("payload.bin")).unwrap(),
        b"recover through pager"
    );
    assert!(!quarantine.exists());
}

#[tokio::test]
async fn unregistered_store_sweep_reports_failed_legacy_restore() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let projects = profile_root.join("projects");
    let data_root = projects.join("proj_paged_retained");
    let quarantine = projects.join(".tracedecay-orphan-quarantine-proj_paged_retained-42-7");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(data_root.join("payload.bin"), b"new live bytes").unwrap();
    std::fs::create_dir_all(&quarantine).unwrap();
    std::fs::write(quarantine.join("payload.bin"), b"legacy quarantine bytes").unwrap();
    let expected = capture_store_content_fence(&profile_root, &quarantine).unwrap();
    let StoreContentFence::Present(inventory) = expected else {
        panic!("fixture must capture the legacy quarantine identity");
    };
    let expected_root_identity = inventory.root;
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let cancellation = CancellationToken::new();

    let report = sweep_unregistered_store_page(
        &db,
        &profile_root,
        UnregisteredStoreSweepRequestV1 {
            cursor: None,
            limit: 2,
            retention_secs: 0,
            now: 1_700_000_000,
            apply: true,
            cancellation: &cancellation,
            deadline: functional_sweep_deadline(),
        },
    )
    .await
    .unwrap();

    let failure = CollectionMutationFailure {
        operation: CollectionMutationOperation::RestoreLiveLeafFromQuarantine,
        raw_os_error: Some(OCCUPIED_RENAME_RAW_OS_ERROR),
        target_path: data_root.clone(),
        expected_root_identity: Some(expected_root_identity),
        classification: CollectionMutationFailureClassification::NonRetryable,
    };
    assert_eq!(
        report.outcome.errors,
        vec![
            CollectionFailure {
                store_id: "proj_paged_retained".to_owned(),
                kind: CollectionFailureKind::RemoveFailed(failure),
            },
            CollectionFailure {
                store_id: "proj_paged_retained".to_owned(),
                kind: CollectionFailureKind::PayloadChanged,
            },
        ]
    );
    assert_eq!(
        report.outcome.recovery_receipts,
        vec![CollectionRecoveryReceipt {
            store_id: "proj_paged_retained".to_owned(),
            original_path: data_root.clone(),
            quarantine_path: quarantine.clone(),
            actual_path: quarantine.clone(),
            action: CollectionRecoveryAction::RetainedForRecovery,
        }]
    );
    assert_eq!(
        std::fs::read(data_root.join("payload.bin")).unwrap(),
        b"new live bytes"
    );
    assert_eq!(
        std::fs::read(quarantine.join("payload.bin")).unwrap(),
        b"legacy quarantine bytes"
    );
}

/// Expiry before the quarantine entry is processed is not a fabricated
/// recovery failure: `DeadlineExceeded` carries the empty accumulated list.
#[tokio::test]
async fn unregistered_store_sweep_elapsed_deadline_reports_empty_legacy_restore_failures() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let projects = profile_root.join("projects");
    let data_root = projects.join("proj_paged_retained");
    let quarantine = projects.join(".tracedecay-orphan-quarantine-proj_paged_retained-42-7");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(data_root.join("payload.bin"), b"new live bytes").unwrap();
    std::fs::create_dir_all(&quarantine).unwrap();
    std::fs::write(quarantine.join("payload.bin"), b"legacy quarantine bytes").unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let cancellation = CancellationToken::new();

    let report = sweep_unregistered_store_page(
        &db,
        &profile_root,
        UnregisteredStoreSweepRequestV1 {
            cursor: None,
            limit: 2,
            retention_secs: 0,
            now: 1_700_000_000,
            apply: true,
            cancellation: &cancellation,
            deadline: MonotonicDeadline::at(Instant::now()),
        },
    )
    .await
    .unwrap();

    assert_eq!(
        report.completion,
        UnregisteredSweepCompletionV1::DeadlineExceeded
    );
    assert!(
        report.outcome.errors.is_empty(),
        "interruption before the quarantine entry must not fabricate restore failures"
    );
    assert!(report.outcome.recovery_receipts.is_empty());
    assert_eq!(
        std::fs::read(data_root.join("payload.bin")).unwrap(),
        b"new live bytes"
    );
    assert_eq!(
        std::fs::read(quarantine.join("payload.bin")).unwrap(),
        b"legacy quarantine bytes"
    );
}
