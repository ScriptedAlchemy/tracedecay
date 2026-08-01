//! Plan/apply lifecycle, retry-ledger, manifest-retirement, branch-recovery,
//! and sqlite-file-handling consolidation tests.

use super::*;

#[tokio::test]
async fn superseded_chained_ledgers_skip_retirement_validation() {
    let dir = tempfile::TempDir::new().unwrap();
    let profile = dir.path().join("profile");
    let ledger_root = profile.join("migration-inventory");
    std::fs::create_dir_all(&ledger_root).unwrap();
    let git_common_dir = dir.path().join("repo/.git");
    let write = |migration_id: &str, source: &str, target: &str, destination: &str| {
        std::fs::write(
            ledger_root.join(format!("{migration_id}.json")),
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema_version": LEDGER_SCHEMA_VERSION,
                "migration_id": migration_id,
                "confirmation_token": "confirm",
                "input_fingerprint": "fixture",
                "source_project_id": source,
                "target_project_id": target,
                "destination_project_id": destination,
                "project_root": dir.path().join("repo"),
                "git_common_dir": git_common_dir,
                "state": "applied",
                "graph_offsets": [],
                "session_offsets": null,
                "preserved_collisions": []
            }))
            .unwrap(),
        )
        .unwrap();
    };
    // proj_aaa -> consolidated forward into proj_bbb: the older ledger's
    // destination is the newer ledger's target.
    write(
        "consolidate_aaaaaaaaaaaaaaaa",
        "proj_src1",
        "proj_tgt1",
        "proj_aaaaaaaaaaaaaaaa",
    );
    write(
        "consolidate_bbbbbbbbbbbbbbbb",
        "proj_src2",
        "proj_aaaaaaaaaaaaaaaa",
        "proj_bbbbbbbbbbbbbbbb",
    );

    let runtime = HostAdmissionTestRuntimeV1::profile(&profile).await.unwrap();
    let report = retire_applied_input_manifests(&profile, runtime.profile_registry()).await;
    assert!(
        !report
            .warnings
            .iter()
            .any(|warning| warning.contains("proj_aaaaaaaaaaaaaaaa")),
        "superseded ledger must be skipped, got warnings: {:?}",
        report.warnings
    );
    assert!(
        report
            .warnings
            .iter()
            .all(|warning| !warning.contains("consolidation marker mismatch")
                || warning.contains("proj_bbbbbbbbbbbbbbbb")),
        "only the live chain head may fail validation: {:?}",
        report.warnings
    );
}

#[tokio::test]
async fn dry_run_reports_live_split_shape_without_mutation() {
    let fixture = fixture().await;
    let before = migration_surface_snapshot(&fixture);
    let profile_before = full_tree_snapshot(&fixture.profile);
    assert!(!fixture.profile.join("lifecycle.lock").exists());
    let report = plan(&fixture.options()).await.unwrap();
    let after = migration_surface_snapshot(&fixture);
    let profile_after = full_tree_snapshot(&fixture.profile);

    assert!(report.dry_run);
    assert_eq!(report.state, ConsolidationState::Planned);
    assert_eq!(report.source.facts, 1);
    assert_eq!(report.source.feedback_events, 1);
    assert_eq!(report.target.facts, 1);
    assert_eq!(report.source.sessions, 1);
    assert_eq!(report.target.sessions, 1);
    assert_eq!(report.source.lcm_raw_messages, 1);
    assert_eq!(report.target.lcm_raw_messages, 1);
    assert!(!report.destination_data_root.exists());
    assert!(!report.backup_root.exists());
    assert!(!report.ledger_path.exists());
    assert_eq!(after, before, "dry-run changed an input or identity file");
    assert_eq!(
        profile_after, profile_before,
        "dry-run changed the profile tree"
    );
    assert!(
        fs::read_dir(fixture.profile.parent().unwrap())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".tracedecay-migration-scratch-")),
        "dry-run left migration scratch state behind"
    );
    assert!(fixture.profile.join("lifecycle.lock").exists());
    assert_eq!(
        storage::read_repository_identity_marker(&fixture.project)
            .unwrap()
            .unwrap()
            .project_id,
        fixture.target_id
    );
}

#[tokio::test]
async fn legacy_single_db_plan_is_read_only_and_apply_preserves_source_graph() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    fs::remove_file(&source.branch_meta_path).unwrap();
    storage::write_store_manifest(&source).unwrap();
    let source_graph_before = file_digest(&source.graph_db_path).unwrap();
    let profile_before = full_tree_snapshot(&fixture.profile);

    let options = fixture.options();
    let planned = plan(&options).await.unwrap();

    assert_eq!(planned.source.graph_databases, 1);
    assert_eq!(planned.source.branches, 1);
    assert!(!source.branch_meta_path.exists());
    assert_eq!(
        full_tree_snapshot(&fixture.profile),
        profile_before,
        "legacy single-DB planning mutated an input"
    );

    let applied = apply(&options, &planned.confirmation_token).await.unwrap();

    assert!(!source.branch_meta_path.exists());
    assert_eq!(
        file_digest(&source.graph_db_path).unwrap(),
        source_graph_before
    );
    let default_branch = tracedecay_runtime_core::branch::detect_default_branch(&fixture.project)
        .unwrap_or_else(|| "main".to_string());
    let preserved_name = format!("consolidated/{}/{}", fixture.source_id, default_branch);
    let destination_meta = branch_meta::load_branch_meta(&applied.destination_data_root).unwrap();
    let preserved = destination_meta.branches.get(&preserved_name).unwrap();
    assert_eq!(preserved.created_at, "0");
    assert_eq!(preserved.last_synced_at, "0");
    let preserved_path = applied.destination_data_root.join(&preserved.db_file);
    let (db, _) = test_open_read_only(&preserved_path).await;
    let memory = db
        .begin_memory_read_transaction("inspect preserved legacy memory")
        .await
        .unwrap();
    let facts = MemoryStore::new_database_transaction(&memory)
        .list_facts(None, Some(0.0), 100)
        .await
        .unwrap();
    assert!(
        facts
            .iter()
            .any(|fact| fact.content == "legacy durable fact"),
        "the preserved source graph lost its legacy fact"
    );
    drop(memory);
    db.close();
}

#[tokio::test]
async fn legacy_single_db_retry_after_destination_publish_is_deterministic() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    fs::remove_file(&source.branch_meta_path).unwrap();
    storage::write_store_manifest(&source).unwrap();
    let options = fixture.options();
    let planned = plan(&options).await.unwrap();

    let interrupted = apply_with_prepare_stop(
        &options,
        &planned.confirmation_token,
        prepare::PrepareStop::Publish,
    )
    .await
    .unwrap_err();
    assert!(
        interrupted.to_string().contains("synthetic interruption"),
        "{interrupted}"
    );

    std::thread::sleep(std::time::Duration::from_secs(1));
    let applied = apply(&options, &planned.confirmation_token).await.unwrap();

    assert_eq!(applied.state, ConsolidationState::Applied);
    let destination_meta = branch_meta::load_branch_meta(&applied.destination_data_root).unwrap();
    let default_branch =
        tracedecay_runtime_core::branch::detect_default_branch(&fixture.project).unwrap();
    let preserved = &destination_meta.branches
        [&format!("consolidated/{}/{}", fixture.source_id, default_branch)];
    assert_eq!(preserved.created_at, "0");
    assert_eq!(preserved.last_synced_at, "0");
}

#[tokio::test]
async fn target_legacy_single_db_metadata_is_synthesized_without_input_mutation() {
    let fixture = fixture().await;
    let target = layout_for_id(&fixture.project, &fixture.profile, &fixture.target_id).unwrap();
    fs::remove_file(&target.branch_meta_path).unwrap();
    storage::write_store_manifest(&target).unwrap();
    let profile_before = full_tree_snapshot(&fixture.profile);
    let options = fixture.options();

    let planned = plan(&options).await.unwrap();

    assert_eq!(planned.target.branches, 1);
    assert_eq!(full_tree_snapshot(&fixture.profile), profile_before);
    let applied = apply(&options, &planned.confirmation_token).await.unwrap();
    assert!(!target.branch_meta_path.exists());
    let destination_meta = branch_meta::load_branch_meta(&applied.destination_data_root).unwrap();
    let default_branch =
        tracedecay_runtime_core::branch::detect_default_branch(&fixture.project).unwrap();
    assert_eq!(destination_meta.default_branch, default_branch);
    assert_eq!(destination_meta.branches[&default_branch].created_at, "0");
    assert_eq!(
        destination_meta.branches[&default_branch].last_synced_at,
        "0"
    );
}

#[tokio::test]
async fn synthesized_branch_metadata_change_invalidates_confirmation_token() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    fs::remove_file(&source.branch_meta_path).unwrap();
    storage::write_store_manifest(&source).unwrap();
    run_git(&fixture.project, &["branch", "-m", "trunk"]);
    let options = fixture.options();

    let planned = plan(&options).await.unwrap();

    run_git(&fixture.project, &["branch", "-m", "release"]);
    let error = apply(&options, &planned.confirmation_token)
        .await
        .unwrap_err();

    assert!(
        error.to_string().contains("confirmation token mismatch"),
        "{error}"
    );
    assert!(!source.branch_meta_path.exists());
    assert!(!planned.destination_data_root.exists());
    assert!(!planned.backup_root.exists());
    assert!(!planned.ledger_path.exists());
}

#[tokio::test]
async fn corrupt_branch_metadata_fails_closed_without_mutation() {
    for content in [
        b"{\"default_branch\":".as_slice(),
        br#"{"default_branch":"main","branches":{}}"#.as_slice(),
        br#"{"default_branch":"main","branches":{"main":{"db_file":"branches/not-main.db","created_at":"0","last_synced_at":"0"}}}"#.as_slice(),
    ] {
        let fixture = fixture().await;
        let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
        fs::write(&source.branch_meta_path, content).unwrap();
        storage::write_store_manifest(&source).unwrap();
        let profile_before = full_tree_snapshot(&fixture.profile);

        let error = plan(&fixture.options()).await.unwrap_err();

        assert!(
            error.to_string().contains("corrupt branch metadata"),
            "{error}"
        );
        assert_eq!(
            full_tree_snapshot(&fixture.profile),
            profile_before,
            "corrupt metadata planning mutated an input"
        );
    }
}

#[tokio::test]
async fn many_branch_plan_retains_constant_database_handles_and_bounded_scratch() {
    const BRANCHES_PER_SHARD: usize = 48;
    let fixture = fixture().await;
    add_branch_links(&fixture, &fixture.source_id, BRANCHES_PER_SHARD);
    add_branch_links(&fixture, &fixture.target_id, BRANCHES_PER_SHARD);

    let resolved = resolve_plan(&fixture.options()).await.unwrap();
    assert_eq!(
        resolved.report.source.graph_databases,
        BRANCHES_PER_SHARD + 1
    );
    assert_eq!(
        resolved.report.target.graph_databases,
        BRANCHES_PER_SHARD + 1
    );
    assert_eq!(
        resolved.evidence.retained_database_count(),
        2,
        "graph snapshots must be processed and dropped one at a time; only the two session snapshots stay open"
    );

    let max_graph_family = input_database_paths(&resolved)
        .unwrap()
        .into_iter()
        .filter(|path| {
            path.file_name().and_then(|name| name.to_str()) != Some(storage::SESSIONS_DB_FILENAME)
        })
        .map(|path| sqlite_family_bytes(&path))
        .max()
        .unwrap();
    assert!(
        resolved.evidence.peak_graph_scratch_bytes() <= max_graph_family,
        "graph scratch must be bounded by one SQLite family, independent of branch count"
    );
    let session_family_bytes = [
        &resolved.source_layout.sessions_db_path,
        &resolved.target_layout.sessions_db_path,
    ]
    .into_iter()
    .map(|path| sqlite_family_bytes(path))
    .sum::<u64>();
    assert!(
        resolved.evidence.sessions.copied_bytes() <= session_family_bytes,
        "retained session scratch must be bounded by the two input families"
    );
}

#[tokio::test]
async fn interrupted_apply_retries_without_duplicates_and_cuts_over_last() {
    let fixture = fixture().await;
    add_fact_relation_to_shard(&fixture, &fixture.source_id).await;
    let options = fixture.options();
    let source_root = fixture.profile.join("projects").join(&fixture.source_id);
    let target_root = fixture.profile.join("projects").join(&fixture.target_id);
    fs::write(source_root.join(".dirty"), b"interrupted source sync").unwrap();
    fs::write(
        source_root.join("tracedecay.db.corrupt-enospc-source"),
        b"source forensic database",
    )
    .unwrap();
    fs::write(
        target_root.join("tracedecay.db.corrupt-enospc-target"),
        b"target forensic database",
    )
    .unwrap();
    let input_database_digests = [
        source_root.join(tracedecay_runtime_core::config::DB_FILENAME),
        source_root.join(storage::SESSIONS_DB_FILENAME),
        target_root.join(tracedecay_runtime_core::config::DB_FILENAME),
        target_root.join(storage::SESSIONS_DB_FILENAME),
    ]
    .map(|path| (path.clone(), file_digest(&path).unwrap()));
    let report = plan(&options).await.unwrap();

    let error = apply_with_stop(
        &options,
        &report.confirmation_token,
        Some(ConsolidationState::DatabasesMerged),
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("synthetic interruption"),
        "{error}"
    );
    assert_eq!(
        storage::read_repository_identity_marker(&fixture.project)
            .unwrap()
            .unwrap()
            .project_id,
        fixture.target_id,
        "marker must not move before all data and registry phases succeed"
    );

    let applied = apply(&options, &report.confirmation_token).await.unwrap();
    assert_eq!(applied.state, ConsolidationState::Applied);
    assert!(!applied.dry_run);
    assert_eq!(
        storage::read_repository_identity_marker(&fixture.project)
            .unwrap()
            .unwrap()
            .project_id,
        applied.destination_project_id
    );
    assert_eq!(
        storage::read_enrollment_marker(&fixture.project)
            .unwrap()
            .unwrap()
            .project_id,
        applied.destination_project_id,
        "successful cutover must suppress legacy shard discovery even when no enrollment marker existed"
    );

    let graph = applied
        .destination_data_root
        .join(tracedecay_runtime_core::config::DB_FILENAME);
    let sessions = applied
        .destination_data_root
        .join(storage::SESSIONS_DB_FILENAME);
    assert_eq!(sqlite::count_rows(&graph, "memory_facts").await.unwrap(), 3);
    assert_eq!(
        sqlite::count_rows(&graph, "memory_fact_relations")
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlite::count_rows(&graph, "memory_feedback_events")
            .await
            .unwrap(),
        1
    );
    assert_eq!(sqlite::count_rows(&sessions, "sessions").await.unwrap(), 2);
    assert_eq!(
        sqlite::count_rows(&sessions, "session_messages")
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        sqlite::count_rows(&sessions, "lcm_raw_messages")
            .await
            .unwrap(),
        2
    );
    assert!(
        applied
            .destination_data_root
            .join("lcm-payloads/source.txt")
            .is_file()
    );
    assert!(
        applied
            .destination_data_root
            .join("lcm-payloads/target.txt")
            .is_file()
    );
    assert!(
        applied
            .backup_root
            .join(&fixture.source_id)
            .join(storage::STORE_MANIFEST_FILENAME)
            .is_file()
    );
    assert_eq!(
        fs::read(applied.backup_root.join(&fixture.source_id).join(".dirty")).unwrap(),
        b"interrupted source sync"
    );
    assert_eq!(
        fs::read(
            applied
                .destination_data_root
                .join("tracedecay.db.corrupt-enospc-source")
        )
        .unwrap(),
        b"source forensic database"
    );
    assert_eq!(
        fs::read(
            applied
                .destination_data_root
                .join("tracedecay.db.corrupt-enospc-target")
        )
        .unwrap(),
        b"target forensic database"
    );
    assert!(
        applied
            .backup_root
            .join(&fixture.target_id)
            .join(storage::SESSIONS_DB_FILENAME)
            .is_file()
    );
    assert!(
        fixture
            .profile
            .join("projects")
            .join(&fixture.source_id)
            .is_dir()
    );
    for project_id in [&fixture.source_id, &fixture.target_id] {
        let (canonical, retired) =
            input_manifest_paths(&fixture, project_id, &applied.destination_project_id);
        assert!(!canonical.exists());
        assert!(retired.is_file());
    }
    for (path, digest) in input_database_digests {
        assert_eq!(
            file_digest(&path).unwrap(),
            digest,
            "{} changed",
            path.display()
        );
    }
    let global = HostAdmissionTestRuntimeV1::profile(&fixture.profile)
        .await
        .unwrap();
    let owners = global
        .profile_registry()
        .list_code_projects(usize::MAX)
        .await
        .unwrap()
        .into_iter()
        .filter(|project| same_path(Path::new(&project.canonical_root), &fixture.project))
        .map(|project| project.project_id)
        .collect::<Vec<_>>();
    assert_eq!(owners, vec![applied.destination_project_id.clone()]);
    drop(global);
    assert!(
        fixture
            .profile
            .join("projects")
            .join(&fixture.target_id)
            .is_dir()
    );

    let meta = branch_meta::load_branch_meta(&applied.destination_data_root).unwrap();
    assert!(meta.branches.contains_key("main"));
    assert!(
        meta.branches
            .contains_key(&format!("consolidated/{}/main", fixture.source_id))
    );

    let retried = apply(&options, &report.confirmation_token).await.unwrap();
    assert_eq!(retried.state, ConsolidationState::Applied);
    assert_eq!(sqlite::count_rows(&graph, "memory_facts").await.unwrap(), 3);
    assert_eq!(
        sqlite::count_rows(&graph, "memory_fact_relations")
            .await
            .unwrap(),
        1
    );
    assert_eq!(sqlite::count_rows(&sessions, "sessions").await.unwrap(), 2);
}

#[tokio::test]
async fn mixed_page_destination_survives_repeated_read_only_opens() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    let target = layout_for_id(&fixture.project, &fixture.profile, &fixture.target_id).unwrap();
    rewrite_page_size(&source.graph_db_path, 8192).await;
    rewrite_page_size(&target.graph_db_path, 4096).await;
    assert_eq!(database_page_size(&source.graph_db_path).await, 8192);
    assert_eq!(database_page_size(&target.graph_db_path).await, 4096);

    let options = fixture.options();
    let planned = plan(&options).await.unwrap();
    let applied = apply(&options, &planned.confirmation_token).await.unwrap();
    let destination = applied
        .destination_data_root
        .join(tracedecay_runtime_core::config::DB_FILENAME);
    assert_eq!(database_page_size(&destination).await, 4096);
    for _ in 0..2 {
        assert!(storage::has_sqlite_database_header(&destination).unwrap());
        let (verification, _) = test_open_read_only(&destination).await;
        let mut mmap_rows = verification
            .conn()
            .query("PRAGMA mmap_size", ())
            .await
            .unwrap();
        assert_eq!(
            mmap_rows
                .next()
                .await
                .unwrap()
                .unwrap()
                .get::<i64>(0)
                .unwrap(),
            0
        );
        drop(mmap_rows);
        assert!(verification.quick_check().await.unwrap());
        verification.close();
    }
}

#[tokio::test]
async fn applied_manifest_retirement_handles_retry_states_and_fails_closed() {
    let fixture = fixture().await;
    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    let applied = apply(&options, &report.confirmation_token).await.unwrap();
    let (source_canonical, source_retired) = input_manifest_paths(
        &fixture,
        &fixture.source_id,
        &applied.destination_project_id,
    );
    let (_, target_retired) = input_manifest_paths(
        &fixture,
        &fixture.target_id,
        &applied.destination_project_id,
    );

    fs::copy(&source_retired, &source_canonical).unwrap();
    let both_identical = apply(&options, &report.confirmation_token).await.unwrap();
    assert_eq!(both_identical.state, ConsolidationState::Applied);
    assert!(!source_canonical.exists());
    assert!(source_retired.is_file());

    fs::write(&source_canonical, b"divergent manifest").unwrap();
    let divergent = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(divergent.to_string().contains("manifests diverge"));
    assert_eq!(fs::read(&source_canonical).unwrap(), b"divergent manifest");
    assert!(source_retired.is_file());
    assert!(target_retired.is_file());

    fs::remove_file(&source_canonical).unwrap();
    storage::write_enrollment_marker(
        &fixture.project,
        &EnrollmentMarker {
            project_id: fixture.target_id.clone(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let marker_mismatch = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(marker_mismatch.to_string().contains("marker mismatch"));
    assert!(source_retired.is_file());
    assert!(target_retired.is_file());

    storage::write_enrollment_marker(
        &fixture.project,
        &EnrollmentMarker {
            project_id: applied.destination_project_id.clone(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    fs::remove_file(&source_retired).unwrap();
    let missing = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(
        missing
            .to_string()
            .contains("neither canonical nor retired")
    );
    assert!(target_retired.is_file());
}

#[tokio::test]
async fn destination_preparation_restarts_after_every_publish_boundary() {
    let fixture = fixture().await;
    let options = fixture.options();
    let report = plan(&options).await.unwrap();

    // Advance the same migration through every restart boundary. Each call
    // proves that the previous interruption is resumable without rebuilding
    // identical source and target shards for the next boundary.
    for stop in [
        prepare::PrepareStop::TargetCopy,
        prepare::PrepareStop::SourceBranch(1),
        prepare::PrepareStop::BranchMetaWrite,
        prepare::PrepareStop::Publish,
    ] {
        let error = apply_with_prepare_stop(&options, &report.confirmation_token, stop)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("synthetic interruption"),
            "{stop:?}: {error}"
        );
        assert_eq!(
            load_ledger(&report.ledger_path).unwrap().unwrap().state,
            ConsolidationState::BackupsReady,
            "{stop:?}: destination preparation advanced the durable ledger"
        );
        assert_eq!(
            storage::read_repository_identity_marker(&fixture.project)
                .unwrap()
                .unwrap()
                .project_id,
            fixture.target_id,
            "{stop:?}: marker moved before destination preparation completed"
        );
    }
    let applied = apply(&options, &report.confirmation_token).await.unwrap();
    assert_eq!(applied.state, ConsolidationState::Applied);
    assert_eq!(
        storage::read_repository_identity_marker(&fixture.project)
            .unwrap()
            .unwrap()
            .project_id,
        applied.destination_project_id,
        "marker did not move after every destination preparation boundary resumed"
    );
}

#[tokio::test]
async fn consolidation_restarts_after_every_durable_state() {
    let fixture = fixture().await;
    let options = fixture.options();
    let report = plan(&options).await.unwrap();

    // Walk one ledger through every durable state so each invocation is a real
    // restart from the preceding state instead of repeating completed phases
    // on an identical fixture.
    for stop in [
        ConsolidationState::BackupsReady,
        ConsolidationState::DestinationReady,
        ConsolidationState::DatabasesMerged,
        ConsolidationState::ArtifactsMerged,
        ConsolidationState::Registered,
    ] {
        let error = apply_with_stop(&options, &report.confirmation_token, Some(stop.clone()))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("synthetic interruption"),
            "{stop:?}: {error}"
        );
        assert_eq!(
            load_ledger(&report.ledger_path).unwrap().unwrap().state,
            stop,
            "restart did not durably reach {stop:?}"
        );
        assert_eq!(
            storage::read_repository_identity_marker(&fixture.project)
                .unwrap()
                .unwrap()
                .project_id,
            fixture.target_id,
            "{stop:?}: marker moved before the final state"
        );
    }
    let applied = apply(&options, &report.confirmation_token).await.unwrap();
    assert_eq!(applied.state, ConsolidationState::Applied);
    assert_eq!(
        storage::read_repository_identity_marker(&fixture.project)
            .unwrap()
            .unwrap()
            .project_id,
        applied.destination_project_id,
        "marker did not move after every durable state resumed"
    );
}

async fn stop_at_destination_ready() -> (
    Fixture,
    ConsolidationOptions,
    ConsolidationReport,
    ConsolidationLedger,
    BTreeMap<PathBuf, TreeSnapshotEntry>,
) {
    let fixture = fixture().await;
    let source_root = fixture.profile.join("projects").join(&fixture.source_id);
    let source_before = full_tree_snapshot(&source_root);
    let options = fixture.options();
    let report = plan(&options).await.unwrap();

    let error = apply_with_stop(
        &options,
        &report.confirmation_token,
        Some(ConsolidationState::DestinationReady),
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("synthetic interruption"),
        "{error}"
    );
    let ledger = load_ledger(&report.ledger_path).unwrap().unwrap();
    assert_eq!(ledger.state, ConsolidationState::DestinationReady);
    assert_eq!(
        full_tree_snapshot(&source_root),
        source_before,
        "publishing the destination changed the source shard"
    );

    (fixture, options, report, ledger, source_before)
}

#[tokio::test]
async fn destination_ready_persists_typed_artifact_authority() {
    let (_fixture, _options, _report, ledger, _source_before) = stop_at_destination_ready().await;

    assert!(
        !ledger.artifact_records.is_empty(),
        "DestinationReady must persist exact artifact authority"
    );
    assert!(ledger.artifact_records.iter().any(|record| {
        record.authority.role == ConsolidationArtifactRoleV1::DestinationCodeGraph
    }));
    assert!(ledger.artifact_records.iter().any(|record| {
        record.authority.role == ConsolidationArtifactRoleV1::DestinationSessions
    }));
    for record in &ledger.artifact_records {
        assert!(
            !record.authority.relative_locator.is_absolute(),
            "artifact locator must remain store-relative: {:?}",
            record.authority
        );
        assert!(
            record
                .authority
                .relative_locator
                .components()
                .all(|component| !matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )),
            "artifact locator escaped the destination store: {:?}",
            record.authority
        );
        assert!(record.authority.incarnation.get() > 0);
    }

    let encoded = serde_json::to_value(&ledger.artifact_records).unwrap();
    let decoded: Vec<ConsolidationArtifactRecordV1> = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, ledger.artifact_records);
}

#[tokio::test]
async fn destination_ready_resume_rejects_wrong_artifact_role_without_source_mutation() {
    let (fixture, options, report, mut ledger, source_before) = stop_at_destination_ready().await;
    let record = ledger
        .artifact_records
        .iter_mut()
        .find(|record| record.authority.role == ConsolidationArtifactRoleV1::DestinationSessions)
        .expect("destination sessions authority");
    record.authority.role = ConsolidationArtifactRoleV1::DestinationCodeGraph;
    save_ledger(&report.ledger_path, &ledger).unwrap();

    let error = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("role does not match"), "{error}");
    assert_eq!(
        full_tree_snapshot(&fixture.profile.join("projects").join(&fixture.source_id)),
        source_before
    );
}

#[tokio::test]
async fn destination_ready_resume_rejects_escaping_artifact_locator_without_source_mutation() {
    let (fixture, options, report, mut ledger, source_before) = stop_at_destination_ready().await;
    ledger.artifact_records[0].authority.relative_locator = PathBuf::from("../outside.db");
    save_ledger(&report.ledger_path, &ledger).unwrap();

    let error = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("normalized relative path"),
        "{error}"
    );
    assert_eq!(
        full_tree_snapshot(&fixture.profile.join("projects").join(&fixture.source_id)),
        source_before
    );
}

#[tokio::test]
async fn destination_ready_resume_rejects_stale_artifact_identity_without_source_mutation() {
    let (fixture, options, report, mut ledger, source_before) = stop_at_destination_ready().await;
    let record = &mut ledger.artifact_records[0];
    record.authority.incarnation =
        tracedecay_store::StoreIncarnationV1::new(record.authority.incarnation.get() + 1).unwrap();
    save_ledger(&report.ledger_path, &ledger).unwrap();

    let error = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("ledger authority does not match"),
        "{error}"
    );
    assert_eq!(
        full_tree_snapshot(&fixture.profile.join("projects").join(&fixture.source_id)),
        source_before
    );
}

#[tokio::test]
async fn destination_ready_resume_keeps_exact_artifact_identity() {
    let (fixture, options, report, ledger, _source_before) = stop_at_destination_ready().await;
    let expected = ledger.artifact_records;
    let source_root = fixture.profile.join("projects").join(&fixture.source_id);
    let source_databases_before = relative_file_map(&source_root)
        .unwrap()
        .into_iter()
        .filter(|(relative, _)| is_sqlite_database(relative) || is_sqlite_sidecar(relative))
        .map(|(relative, path)| (relative, file_digest(&path).unwrap()))
        .collect::<BTreeMap<_, _>>();

    let applied = apply(&options, &report.confirmation_token).await.unwrap();
    assert_eq!(applied.state, ConsolidationState::Applied);
    assert_eq!(
        load_ledger(&report.ledger_path)
            .unwrap()
            .unwrap()
            .artifact_records,
        expected
    );
    let source_databases_after = relative_file_map(&source_root)
        .unwrap()
        .into_iter()
        .filter(|(relative, _)| is_sqlite_database(relative) || is_sqlite_sidecar(relative))
        .map(|(relative, path)| (relative, file_digest(&path).unwrap()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        source_databases_after, source_databases_before,
        "exact-identity resume changed a source SQLite family"
    );
}

#[tokio::test]
async fn version_one_destination_ready_without_authority_fails_closed() {
    let fixture = fixture().await;
    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    let error = apply_with_stop(
        &options,
        &report.confirmation_token,
        Some(ConsolidationState::DestinationReady),
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("synthetic interruption"),
        "{error:#}"
    );

    let mut ledger = load_ledger(&report.ledger_path).unwrap().unwrap();
    ledger.schema_version = 1;
    ledger.artifact_records.clear();
    save_ledger(&report.ledger_path, &ledger).unwrap();

    let error = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("cannot be migrated safely"),
        "{error}"
    );
}

#[tokio::test]
async fn version_one_postmerge_ledger_fails_closed() {
    let fixture = fixture().await;
    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    let error = apply_with_stop(
        &options,
        &report.confirmation_token,
        Some(ConsolidationState::DatabasesMerged),
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("synthetic interruption"),
        "{error}"
    );

    let mut ledger = load_ledger(&report.ledger_path).unwrap().unwrap();
    ledger.schema_version = 1;
    save_ledger(&report.ledger_path, &ledger).unwrap();

    let error = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("cannot be migrated safely"));
    assert_eq!(
        storage::read_repository_identity_marker(&fixture.project)
            .unwrap()
            .unwrap()
            .project_id,
        fixture.target_id
    );
}

#[tokio::test]
#[cfg(not(windows))]
async fn identity_survives_symlink_and_repository_move() {
    let fixture = fixture().await;
    let symlink = fixture.project.parent().unwrap().join("repo-symlink");
    std::os::unix::fs::symlink(&fixture.project, &symlink).unwrap();
    let mut options = fixture.options();
    options.project_root = symlink;
    let report = plan(&options).await.unwrap();
    let applied = apply(&options, &report.confirmation_token).await.unwrap();
    assert_eq!(
        storage::read_enrollment_marker(&fixture.project)
            .unwrap()
            .unwrap()
            .project_id,
        applied.destination_project_id
    );
    assert_eq!(
        storage::read_repository_identity_marker(&fixture.project)
            .unwrap()
            .unwrap()
            .project_id,
        applied.destination_project_id
    );

    let moved = fixture.project.parent().unwrap().join("repo-moved");
    fs::rename(&fixture.project, &moved).unwrap();
    let reopened = storage::resolve_layout(&moved, &fixture.profile).unwrap();
    assert!(same_path(
        &reopened.data_root,
        &applied.destination_data_root
    ));
}

#[tokio::test]
async fn untracked_branch_databases_with_mixed_case_extensions_are_recovered() {
    const SOURCE_ORPHAN_FACT: &str = "fact unique to the source orphan branch";
    const TARGET_ORPHAN_FACT: &str = "fact unique to the target orphan branch";
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    let target = layout_for_id(&fixture.project, &fixture.profile, &fixture.target_id).unwrap();
    add_untracked_branch(&source, "orphan-source", SOURCE_ORPHAN_FACT).await;
    add_untracked_branch(&target, "orphan-target", TARGET_ORPHAN_FACT).await;
    let lowercase = target.data_root.join("branches/orphan-target.db");
    let uppercase = target.data_root.join("branches/orphan-target.DB");
    fs::rename(&lowercase, &uppercase).unwrap();
    for suffix in ["-wal", "-shm"] {
        let source = sqlite_sidecar(&lowercase, suffix);
        if source.is_file() {
            fs::rename(source, sqlite_sidecar(&uppercase, suffix)).unwrap();
        }
    }

    let options = fixture.options();
    let planned = plan(&options).await.unwrap();
    assert_eq!(planned.source.graph_databases, 2);
    assert_eq!(planned.target.graph_databases, 2);
    assert_eq!(planned.source.branches, 2);
    assert_eq!(planned.target.branches, 2);

    let applied = apply(&options, &planned.confirmation_token).await.unwrap();
    let meta = branch_meta::load_branch_meta(&applied.destination_data_root).unwrap();
    let recovered = meta
        .branches
        .iter()
        .filter(|(name, _)| name.contains("orphan-source-") || name.contains("orphan-target-"))
        .map(|(name, entry)| {
            assert!(entry.gc_protected);
            (name.clone(), entry.db_file.clone())
        })
        .collect::<Vec<_>>();
    assert_eq!(recovered.len(), 2);
    assert!(recovered.iter().any(|(name, db_file)| {
        name.starts_with("recovered/orphan-target-") && db_file == "branches/orphan-target.DB"
    }));
    assert!(recovered.iter().any(|(name, db_file)| {
        name.starts_with(&format!(
            "consolidated/{}/recovered/orphan-source-",
            fixture.source_id
        )) && applied.destination_data_root.join(db_file).is_file()
    }));

    for (name, db_file) in recovered {
        assert!(meta.branches[&name].gc_protected);
        let path = applied.destination_data_root.join(db_file);
        assert!(path.is_file());
        let expected = if name.contains("orphan-source-") {
            SOURCE_ORPHAN_FACT
        } else {
            TARGET_ORPHAN_FACT
        };
        let (db, _) = test_open_read_only(&path).await;
        let memory = db
            .begin_memory_read_transaction("inspect recovered branch memory")
            .await
            .unwrap();
        let facts = MemoryStore::new_database_transaction(&memory)
            .list_facts(None, Some(0.0), 100)
            .await
            .unwrap();
        assert!(
            facts.iter().any(|fact| fact.content == expected),
            "recovered branch '{name}' lost its unique fact"
        );
        drop(memory);
        db.close();
    }
}

#[tokio::test]
async fn corrupt_untracked_branch_database_is_rejected_before_mutation() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    let branches = source.data_root.join("branches");
    fs::create_dir_all(&branches).unwrap();
    let corrupt = branches.join("corrupt-orphan.db");
    let connection = rusqlite::Connection::open(&corrupt).unwrap();
    connection
        .execute_batch(
            "PRAGMA journal_mode = DELETE;
             CREATE TABLE damaged_fixture(payload BLOB NOT NULL);
             INSERT INTO damaged_fixture(payload) VALUES (zeroblob(65536));",
        )
        .unwrap();
    drop(connection);
    let file = fs::OpenOptions::new().write(true).open(&corrupt).unwrap();
    let length = fs::metadata(&corrupt).unwrap().len();
    file.set_len(length.saturating_sub(4096)).unwrap();
    drop(file);
    let before = full_tree_snapshot(&fixture.profile);

    let error = plan(&fixture.options()).await.unwrap_err();

    assert!(error.to_string().contains("quick_check"), "{error}");
    assert_eq!(
        full_tree_snapshot(&fixture.profile),
        before,
        "corrupt-input planning mutated the profile"
    );
}

#[tokio::test]
async fn branch_metadata_paths_cannot_escape_the_profile_shard() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    let outside = fixture.profile.join("outside.db");
    fs::copy(&source.graph_db_path, &outside).unwrap();
    let meta = branch_meta::load_branch_meta(&source.data_root).unwrap();

    for db_file in [
        "../outside.db".to_string(),
        outside.to_string_lossy().to_string(),
    ] {
        let mut invalid = meta.clone();
        invalid.branches.insert(
            "unsafe".to_string(),
            BranchEntry {
                db_file,
                parent: Some(meta.default_branch.clone()),
                created_at: "0".to_string(),
                last_synced_at: "0".to_string(),
                gc_protected: false,
            },
        );
        fs::write(
            &source.branch_meta_path,
            serde_json::to_vec_pretty(&invalid).unwrap(),
        )
        .unwrap();
        let error = plan(&fixture.options()).await.unwrap_err();
        assert!(error.to_string().contains("store-relative path"), "{error}");
    }
}

#[tokio::test]
async fn a_third_matching_shard_is_left_for_its_own_pass() {
    // A repository can be split across more than two legacy shards. The named
    // source and target only need to be a *subset* of this identity's
    // claimants, so a third matching shard no longer blocks the pairwise
    // source->target pass; it is simply left untouched for its own explicit
    // consolidation later.
    let fixture = fixture().await;
    let current_enrollment = storage::read_enrollment_marker(&fixture.project)
        .unwrap()
        .expect("fixture must retain the current project enrollment");
    storage::write_enrollment_marker(
        &fixture.project,
        &EnrollmentMarker {
            project_id: "proj_third".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    create_shard(
        &fixture.profile,
        &fixture.project,
        "proj_third",
        "third fact",
        "third-session",
        false,
    )
    .await;
    storage::write_enrollment_marker(&fixture.project, &current_enrollment).unwrap();
    let third_layout = layout_for_id(&fixture.project, &fixture.profile, "proj_third").unwrap();
    let third_manifest_path = third_layout.manifest_path.clone().unwrap();
    let third_graph_before = file_digest(&third_layout.graph_db_path).unwrap();
    let third_sessions_before = file_digest(&third_layout.sessions_db_path).unwrap();
    let third_manifest_before = fs::read(&third_manifest_path).unwrap();

    let options = fixture.options();
    let planned = plan(&options).await.unwrap();
    let applied = apply(&options, &planned.confirmation_token).await.unwrap();
    assert_eq!(applied.state, ConsolidationState::Applied);

    // The extra shard's store is completely untouched by this pass.
    assert_eq!(
        file_digest(&third_layout.graph_db_path).unwrap(),
        third_graph_before,
        "the untouched third shard's graph database changed"
    );
    assert_eq!(
        file_digest(&third_layout.sessions_db_path).unwrap(),
        third_sessions_before,
        "the untouched third shard's sessions database changed"
    );
    assert_eq!(
        fs::read(&third_manifest_path).unwrap(),
        third_manifest_before,
        "the untouched third shard's store manifest changed"
    );

    // ...and it still fails resolution closed on its own: unlike the source
    // and target manifests, which this applied consolidation retires, the
    // third shard's manifest is neither retired nor absorbed into the
    // destination. Store resolution still reports it as an unresolved legacy
    // claimant of this exact repository identity, which is what keeps its
    // opens failing closed until an explicit pass names it.
    let (third_canonical, third_retired) =
        input_manifest_paths(&fixture, "proj_third", &applied.destination_project_id);
    assert!(
        third_canonical.is_file(),
        "the untouched third shard's manifest must not be retired by this pass"
    );
    assert!(
        !third_retired.exists(),
        "the untouched third shard must not be folded into this pass's destination"
    );
    let (claimants, _) = storage::matching_legacy_profile_layouts(
        &fixture.project,
        &fixture.profile,
        Some(applied.destination_project_id.as_str()),
    )
    .unwrap();
    assert_eq!(
        claimants
            .iter()
            .map(|layout| layout.identity.project_id.as_deref().unwrap())
            .collect::<Vec<_>>(),
        vec!["proj_third"],
        "the untouched third shard must remain the sole unresolved claimant \
         of this repository identity"
    );

    // The third shard is consumable by its own explicit pairwise pass: a
    // follow-up consolidation naming it as the source and the just-published
    // destination as the target plans cleanly.
    let second_pass = ConsolidationOptions {
        project_root: fixture.project.clone(),
        profile_root: fixture.profile.clone(),
        source_project_id: "proj_third".to_string(),
        target_project_id: applied.destination_project_id.clone(),
    };
    let second_plan = plan(&second_pass).await.unwrap();
    assert_eq!(second_plan.state, ConsolidationState::Planned);
    assert_eq!(second_plan.source.facts, 1);
}

#[tokio::test]
async fn preexisting_destination_without_ledger_is_never_reused() {
    let fixture = fixture().await;
    let options = fixture.options();
    let planned = plan(&options).await.unwrap();
    fs::create_dir_all(&planned.destination_data_root).unwrap();
    fs::write(planned.destination_data_root.join("foreign"), b"foreign").unwrap();

    let error = apply(&options, &planned.confirmation_token)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("already exists without this migration ledger")
    );
    assert!(!planned.ledger_path.exists());
    assert_eq!(
        storage::read_repository_identity_marker(&fixture.project)
            .unwrap()
            .unwrap()
            .project_id,
        fixture.target_id
    );
}

#[tokio::test]
async fn corrupt_retry_ledger_is_never_overwritten() {
    let fixture = fixture().await;
    let options = fixture.options();
    let planned = plan(&options).await.unwrap();
    fs::create_dir_all(planned.ledger_path.parent().unwrap()).unwrap();
    fs::write(&planned.ledger_path, b"{not-json").unwrap();

    let error = apply(&options, &planned.confirmation_token)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("ledger"));
    assert!(error.to_string().contains("corrupt"));
    assert_eq!(fs::read(&planned.ledger_path).unwrap(), b"{not-json");
    assert!(!planned.destination_data_root.exists());
}

#[test]
fn sqlite_family_backup_includes_wal_and_shm() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source.db");
    let target = temp.path().join("backup/target.db");
    fs::write(&source, b"db").unwrap();
    fs::write(sqlite_sidecar(&source, "-wal"), b"wal").unwrap();
    fs::write(sqlite_sidecar(&source, "-shm"), b"shm").unwrap();

    copy_sqlite_family_exact(&source, &target).unwrap();

    assert_eq!(fs::read(&target).unwrap(), b"db");
    assert_eq!(fs::read(sqlite_sidecar(&target, "-wal")).unwrap(), b"wal");
    assert_eq!(fs::read(sqlite_sidecar(&target, "-shm")).unwrap(), b"shm");
}

#[test]
fn atomic_copy_recovers_an_interrupted_temp_and_reopens_cleanly() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source.bin");
    let target = temp.path().join("backup/target.bin");
    let interrupted = target.with_extension(format!("tmp-{}", std::process::id()));
    fs::create_dir_all(interrupted.parent().unwrap()).unwrap();
    fs::write(&source, b"durable source bytes").unwrap();
    fs::write(&interrupted, b"partial").unwrap();

    copy_file_atomic(&source, &target).unwrap();

    assert_eq!(fs::read(&target).unwrap(), b"durable source bytes");
    assert!(!interrupted.exists());
}

#[test]
fn atomic_copy_preserves_read_only_files_without_losing_durability() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source.bin");
    let target = temp.path().join("backup/target.bin");
    fs::write(&source, b"read-only source bytes").unwrap();
    let original_permissions = fs::metadata(&source).unwrap().permissions();
    let mut permissions = original_permissions.clone();
    permissions.set_readonly(true);
    fs::set_permissions(&source, permissions).unwrap();

    copy_file_atomic(&source, &target).unwrap();
    files::copy_file_exact(&source, &target).unwrap();

    assert_eq!(fs::read(&target).unwrap(), b"read-only source bytes");
    assert!(fs::metadata(&target).unwrap().permissions().readonly());
    for path in [&source, &target] {
        fs::set_permissions(path, original_permissions.clone()).unwrap();
    }
}
