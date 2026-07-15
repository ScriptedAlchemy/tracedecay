use super::*;
use crate::global_db::StoreInstanceUpsert;
use crate::storage::{
    EnrollmentMarker, STORE_MANIFEST_FILENAME, STORE_MANIFEST_SCHEMA_VERSION, StorageMode,
    StoreKind, StoreManifest, profile_sharded_layout, write_enrollment_marker,
    write_repository_identity_marker, write_store_manifest,
};
#[cfg(unix)]
use std::os::unix::fs::symlink;

fn canonical_temp_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        path.to_path_buf()
    }
    #[cfg(not(windows))]
    {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    }
}

#[test]
fn format_bytes_boundaries() {
    assert_eq!(format_bytes(0), "0 B");
    assert_eq!(format_bytes(1023), "1023 B");
    assert_eq!(format_bytes(1024), "1.0 KB");
    assert_eq!(format_bytes(1536), "1.5 KB");
    assert_eq!(format_bytes(1024 * 1024 - 1), "1024.0 KB");
    assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
    assert_eq!(format_bytes(1024 * 1024 * 512), "512.0 MB");
    assert_eq!(format_bytes(1024 * 1024 * 1024), "1.0 GB");
    assert_eq!(format_bytes(1024 * 1024 * 1024 * 2), "2.0 GB");
}

#[tokio::test]
async fn orphan_reporting_uses_complete_registry_rows_not_token_accounting() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let dir = tempfile::Builder::new()
        .prefix("doctor-orphans-")
        .tempdir_in(base)
        .unwrap();
    let db_dir = tempfile::Builder::new()
        .prefix("doctor-orphans-db-")
        .tempdir()
        .unwrap();
    let profile_root = dir.path().join("profile");
    let eligible_root = dir.path().join("eligible-repo");
    let conflicting_root = dir.path().join("conflicting-repo");
    let conflicting_registered_root = dir.path().join("registered-elsewhere");
    let blocked_root = dir.path().join("blocked-repo");
    std::fs::create_dir_all(&eligible_root).unwrap();
    std::fs::create_dir_all(&conflicting_root).unwrap();
    std::fs::create_dir_all(&conflicting_registered_root).unwrap();
    std::fs::create_dir_all(&blocked_root).unwrap();
    for root in [&eligible_root, &conflicting_root, &blocked_root] {
        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(root)
            .status()
            .unwrap();
        assert!(status.success());
    }
    write_enrollment_marker(
        &eligible_root,
        &EnrollmentMarker {
            project_id: "proj_eligible".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    write_repository_identity_marker(&eligible_root, "proj_eligible").unwrap();
    write_enrollment_marker(
        &conflicting_root,
        &EnrollmentMarker {
            project_id: "proj_conflict".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    write_repository_identity_marker(&conflicting_root, "proj_conflict").unwrap();
    for (project_id, project_root) in [
        ("proj_eligible", &eligible_root),
        ("proj_conflict", &conflicting_root),
        ("proj_blocked", &blocked_root),
    ] {
        let data_root = profile_root.join("projects").join(project_id);
        std::fs::create_dir_all(&data_root).unwrap();
        let manifest = StoreManifest {
            schema_version: STORE_MANIFEST_SCHEMA_VERSION,
            project_id: Some(project_id.to_string()),
            store_kind: StoreKind::CodeProject,
            storage_mode: StorageMode::ProfileSharded,
            project_root: project_root.clone(),
            data_root: data_root.clone(),
            graph_db_relpath: "tracedecay.db".into(),
            sessions_db_relpath: "sessions.db".into(),
            branch_meta_relpath: "branch-meta.json".into(),
        };
        std::fs::write(
            data_root.join(STORE_MANIFEST_FILENAME),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    let db = crate::global_db::GlobalDb::open_at(&db_dir.path().join("global.db"))
        .await
        .unwrap();
    db.upsert_code_project(
        "proj_conflict",
        &conflicting_registered_root,
        None,
        None,
        Some("main"),
    )
    .await
    .unwrap();
    let (count, warnings) = orphan_store_manifest_report(&db, &profile_root).await;

    assert_eq!(count, 1, "{warnings:?}");
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("proj_conflict")),
        "{warnings:?}"
    );

    let scan = crate::migrate::registry::scan_profile_store_manifests(&profile_root, 1_800_000_000);
    let eligible = crate::migrate::registry::RegistryReconstructionReport {
        plans: scan
            .plans
            .into_iter()
            .filter(|plan| {
                plan.status == crate::migrate::registry::RegistryReconstructionStatus::Eligible
                    && plan.project.project_id == "proj_eligible"
            })
            .collect(),
        issues: Vec::new(),
    };
    let mut batch_left = eligible.plans[0].clone();
    batch_left.project.aliases = vec![dir.path().join("shared-alias")];
    let mut batch_right = batch_left.clone();
    batch_right.project.project_id = "proj_batch_other".to_string();
    batch_right.project.project_root = conflicting_root.clone();
    batch_right.store.project_id = batch_right.project.project_id.clone();
    batch_right.store.store_id = "store:proj_batch_other:profile_sharded".to_string();
    batch_right.store.store_relpath = "projects/proj_batch_other".to_string();
    batch_right.store.manifest_relpath =
        Some("projects/proj_batch_other/store_manifest.json".to_string());
    batch_right.graph_scopes.clear();
    batch_right.artifacts.clear();
    batch_left.graph_scopes.clear();
    batch_left.artifacts.clear();
    let batch_diff = crate::migrate::registry::diff_registry_reconstruction_report(
        &db,
        &crate::migrate::registry::RegistryReconstructionReport {
            plans: vec![batch_left, batch_right],
            issues: Vec::new(),
        },
    )
    .await;
    assert_eq!(batch_diff.missing_plans, 0);
    assert!(
        batch_diff
            .issues
            .iter()
            .any(|issue| issue.contains("shared-alias")),
        "{:?}",
        batch_diff.issues
    );
    let applied = crate::migrate::registry::apply_registry_reconstruction_report(&db, &eligible)
        .await
        .unwrap();
    assert_eq!(applied.projects, 1);
    assert_eq!(
        orphan_store_manifest_report(&db, &profile_root).await.0,
        0,
        "a complete reconstruction registry is healthy without a legacy projects.path row"
    );
    assert_eq!(
        crate::migrate::registry::apply_registry_reconstruction_report(&db, &eligible)
            .await
            .unwrap(),
        crate::migrate::registry::RegistryReconstructionApplyReport::default()
    );

    db.writer_connection()
        .await
        .unwrap()
        .execute(
            "DELETE FROM store_artifacts WHERE store_id=?1",
            libsql::params![eligible.plans[0].store.store_id.as_str()],
        )
        .await
        .unwrap();
    assert_eq!(orphan_store_manifest_report(&db, &profile_root).await.0, 1);
    crate::migrate::registry::apply_registry_reconstruction_report(&db, &eligible)
        .await
        .unwrap();

    db.writer_connection()
        .await
        .unwrap()
        .execute(
            "DELETE FROM store_instances WHERE store_id=?1",
            libsql::params![eligible.plans[0].store.store_id.as_str()],
        )
        .await
        .unwrap();
    assert_eq!(orphan_store_manifest_report(&db, &profile_root).await.0, 1);
}

#[test]
fn format_bytes_fractional_kb() {
    // 2048 bytes = 2.0 KB
    assert_eq!(format_bytes(2048), "2.0 KB");
    // 1536 = 1.5 KB
    assert_eq!(format_bytes(1536), "1.5 KB");
}

#[test]
fn database_recovery_guidance_names_the_preserved_recovery_set() {
    let db_path = PathBuf::from("/profile/projects/proj_test/tracedecay.db");
    let guidance = database_recovery_guidance(&db_path);

    for path in [
        db_path.clone(),
        db_path.with_extension("db-wal"),
        db_path.with_extension("db-shm"),
        PathBuf::from(format!("{}.dirty", db_path.display())),
        db_path.parent().unwrap().join("dirty"),
    ] {
        assert!(guidance.contains(&path.display().to_string()));
    }
    assert!(guidance.contains("stop all TraceDecay daemon and MCP processes"));
    assert!(
        guidance.contains(
            "Do not run `tracedecay init`, `tracedecay sync --force`, or `tracedecay wipe`"
        )
    );
    assert!(guidance.contains("`sessions.db` is separate and must not be removed"));
    assert!(guidance.contains("Facts are stored in the graph database"));
    assert!(guidance.contains("automatic rebuild is intentionally blocked"));
}

#[tokio::test]
async fn database_check_preserves_corrupt_graph_and_adjacent_stores()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::TempDir::new()?;
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    std::fs::create_dir_all(&project_root)?;
    let open_options = TraceDecayOpenOptions {
        profile_root: Some(profile_root),
        global_db_path: Some(dir.path().join("global.db")),
    };
    let ts = TraceDecay::init_with_options(&project_root, open_options.clone()).await?;
    let layout = ts.store_layout().clone();
    ts.close();

    let corrupt_db = b"not-a-sqlite-database";
    let wal_path = layout.graph_db_path.with_extension("db-wal");
    let shm_path = layout.graph_db_path.with_extension("db-shm");
    std::fs::write(&layout.graph_db_path, corrupt_db)?;
    std::fs::write(&wal_path, b"preserve-wal")?;
    std::fs::write(&shm_path, b"preserve-shm")?;
    std::fs::write(&layout.dirty_path, b"preserve-dirty")?;
    std::fs::write(&layout.sessions_db_path, b"preserve-sessions")?;

    let mut counters = DoctorCounters::new();
    let healthy = check_database(
        &mut counters,
        &serde_json::json!({
            "storage_health": {
                "canonical_db_path": layout.graph_db_path,
                "db_size_bytes": corrupt_db.len(),
                "quick_check_ok": false,
                "authority_audit_ok": true,
                "authority_audit_error": null,
                "dirty_marker": { "exists": true, "state": "dirty" },
            }
        }),
    );

    assert!(!healthy);
    assert_eq!(counters.issues, 1);
    assert_eq!(std::fs::read(&layout.graph_db_path)?, corrupt_db);
    assert_eq!(std::fs::read(&wal_path)?, b"preserve-wal");
    assert_eq!(std::fs::read(&shm_path)?, b"preserve-shm");
    assert_eq!(std::fs::read(&layout.dirty_path)?, b"preserve-dirty");
    assert_eq!(
        std::fs::read(&layout.sessions_db_path)?,
        b"preserve-sessions"
    );
    Ok(())
}

#[tokio::test]
async fn database_check_is_read_only_while_a_writer_is_live()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::TempDir::new()?;
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    std::fs::create_dir_all(&project_root)?;
    let open_options = TraceDecayOpenOptions {
        profile_root: Some(profile_root),
        global_db_path: Some(dir.path().join("global.db")),
    };
    let ts = TraceDecay::init_with_options(&project_root, open_options.clone()).await?;
    let db_path = ts.db_path();
    drop(ts);

    let authority = crate::db::DatabaseAuthority::acquire_test(&db_path, "doctor test")?;
    let (writer, _) = crate::db::Database::open(&db_path, &authority).await?;
    writer
        .execute_write_batch(
            "seed doctor freelist fixture",
            "CREATE TABLE doctor_probe (payload BLOB);\
             WITH RECURSIVE count(x) AS (\
                 VALUES(1) UNION ALL SELECT x + 1 FROM count WHERE x < 256\
             )\
             INSERT INTO doctor_probe SELECT zeroblob(8192) FROM count;\
             DELETE FROM doctor_probe;",
        )
        .await?;
    writer.checkpoint().await?;

    let freelist_before: i64 = {
        let mut rows = writer.conn().query("PRAGMA freelist_count", ()).await?;
        rows.next().await?.expect("freelist row").get(0)?
    };
    assert!(
        freelist_before > 0,
        "fixture must contain reclaimable pages"
    );

    let mut counters = DoctorCounters::new();
    let healthy = check_database(
        &mut counters,
        &serde_json::json!({
            "storage_health": {
                "canonical_db_path": db_path,
                "db_size_bytes": std::fs::metadata(&db_path)?.len(),
                "quick_check_ok": true,
                "authority_audit_ok": true,
                "authority_audit_error": null,
                "dirty_marker": { "exists": false },
                "daemon_owner_pid": std::process::id(),
                "daemon_generation": "test-generation",
            }
        }),
    );
    assert!(healthy);

    let freelist_after: i64 = {
        let mut rows = writer.conn().query("PRAGMA freelist_count", ()).await?;
        rows.next().await?.expect("freelist row").get(0)?
    };
    assert_eq!(
        freelist_after, freelist_before,
        "doctor must not run VACUUM or otherwise compact a live database"
    );
    writer
        .execute_write(
            "verify doctor writer remains usable",
            "INSERT INTO doctor_probe(payload) VALUES (zeroblob(64))",
            (),
        )
        .await?;
    assert!(
        writer.quick_check().await?,
        "live writer must remain healthy"
    );
    Ok(())
}

#[test]
fn database_authority_audit_is_required_and_enforced() {
    let healthy_status = serde_json::json!({
        "storage_health": {
            "quick_check_ok": true,
            "authority_audit_ok": true,
            "authority_audit_error": null,
        }
    });
    let mut healthy_counters = DoctorCounters::new();
    assert!(check_database(&mut healthy_counters, &healthy_status));
    assert_eq!(healthy_counters.issues, 0);

    let failed_status = serde_json::json!({
        "storage_health": {
            "quick_check_ok": true,
            "authority_audit_ok": false,
            "authority_audit_error": "multiple writers detected",
        }
    });
    let mut failed_counters = DoctorCounters::new();
    assert!(!check_database(&mut failed_counters, &failed_status));
    assert_eq!(failed_counters.issues, 1);

    let mut missing_counters = DoctorCounters::new();
    assert!(!check_database(
        &mut missing_counters,
        &serde_json::json!({ "storage_health": { "quick_check_ok": true } }),
    ));
    assert_eq!(missing_counters.issues, 1);
}

#[tokio::test]
async fn current_project_store_resolves_profile_shard_via_registry_alias()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::TempDir::new()?;
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    std::fs::create_dir_all(&project_root)?;
    let project_root = canonical_temp_path(&project_root);
    let shard_root =
        crate::storage::profile_sharded_data_root(&profile_root, "proj_doctor_current");
    std::fs::create_dir_all(&shard_root)?;
    std::fs::write(
        shard_root.join(crate::config::db_filename(&shard_root)),
        b"graph",
    )?;

    let global_db_path = dir.path().join("global.db");
    let db = crate::global_db::GlobalDb::open_at(&global_db_path)
        .await
        .ok_or_else(|| std::io::Error::other("could not open global db"))?;
    db.upsert_code_project(
        "proj_doctor_current",
        &project_root,
        None,
        None,
        Some("main"),
    )
    .await
    .ok_or_else(|| std::io::Error::other("could not upsert project"))?;
    db.upsert_store_instance(StoreInstanceUpsert {
        store_id: "store:proj_doctor_current:profile_sharded".to_string(),
        project_id: "proj_doctor_current".to_string(),
        store_kind: "code_project".to_string(),
        storage_mode: "profile_sharded".to_string(),
        store_relpath: Path::new("projects")
            .join("proj_doctor_current")
            .to_string_lossy()
            .to_string(),
        manifest_relpath: Some(crate::storage::STORE_MANIFEST_FILENAME.to_string()),
        last_verified_at: Some(1_800_000_000),
        last_write_at: Some(1_800_000_000),
    })
    .await
    .ok_or_else(|| std::io::Error::other("could not upsert store"))?;

    let open_options = TraceDecayOpenOptions {
        profile_root: Some(profile_root.clone()),
        global_db_path: Some(global_db_path),
    };

    // No repo-local `.tracedecay/` index exists, yet the project must not
    // be reported as uninitialized: resolution finds the profile shard.
    assert!(!crate::config::has_project_database(&project_root));
    match resolve_current_project_store(&project_root, &open_options).await? {
        CurrentProjectStore::Resolved(layout) => {
            assert_eq!(layout.data_root, shard_root);
            assert_eq!(
                layout.identity.project_id.as_deref(),
                Some("proj_doctor_current")
            );
            assert!(describe_resolved_store(&layout).contains("profile-sharded"));
        }
        other => panic!("expected resolved profile shard, got {other:?}"),
    }

    // A project the registry knows nothing about should still get the
    // `tracedecay init` advice.
    let unregistered = dir.path().join("unregistered");
    std::fs::create_dir_all(&unregistered)?;
    let unregistered = canonical_temp_path(&unregistered);
    assert!(matches!(
        resolve_current_project_store(&unregistered, &open_options).await?,
        CurrentProjectStore::Uninitialized
    ));
    Ok(())
}

#[tokio::test]
async fn current_project_store_resolves_moved_repository_identity_read_only()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::TempDir::new()?;
    let profile_root = dir.path().join("profile");
    let original = dir.path().join("repo");
    let moved = dir.path().join("repo-moved");
    std::fs::create_dir_all(&original)?;
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&original)
        .status()?;
    assert!(status.success());

    let project_id = "proj_doctor_moved";
    let shard_root = crate::storage::profile_sharded_data_root(&profile_root, project_id);
    std::fs::create_dir_all(&shard_root)?;
    std::fs::write(
        shard_root.join(crate::config::db_filename(&shard_root)),
        b"graph",
    )?;
    crate::storage::write_repository_identity_marker(&original, project_id)?;
    std::fs::rename(&original, &moved)?;

    let open_options = TraceDecayOpenOptions {
        profile_root: Some(profile_root),
        global_db_path: Some(dir.path().join("global.db")),
    };
    match resolve_current_project_store(&moved, &open_options).await? {
        CurrentProjectStore::Resolved(layout) => {
            assert_eq!(layout.data_root, shard_root);
            assert_eq!(layout.identity.project_id.as_deref(), Some(project_id));
        }
        other => panic!("expected moved repository identity, got {other:?}"),
    }
    Ok(())
}

#[tokio::test]
async fn current_project_store_surfaces_split_identity_conflict()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::TempDir::new()?;
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    std::fs::create_dir_all(&project_root)?;
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&project_root)
        .status()?;
    assert!(status.success());

    for (project_id, node_id) in [
        ("proj_doctor_selected", "selected-node"),
        ("proj_doctor_legacy", "legacy-node"),
    ] {
        let layout = profile_sharded_layout(
            &project_root,
            &profile_root,
            &EnrollmentMarker {
                project_id: project_id.to_string(),
                storage_mode: StorageMode::ProfileSharded,
            },
        )?;
        let authority = crate::db::DatabaseAuthority::acquire_test(
            &layout.graph_db_path,
            "doctor identity test",
        )?;
        let (db, _) = crate::db::Database::initialize(&layout.graph_db_path, &authority).await?;
        db.insert_node(&crate::types::Node {
            id: node_id.to_string(),
            kind: crate::types::NodeKind::Function,
            name: node_id.to_string(),
            qualified_name: node_id.to_string(),
            file_path: "src/lib.rs".to_string(),
            start_line: 1,
            attrs_start_line: 1,
            end_line: 1,
            start_column: 0,
            end_column: 1,
            signature: None,
            docstring: None,
            visibility: crate::types::Visibility::Pub,
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
            updated_at: 1_800_000_000,
            parent_id: None,
        })
        .await?;
        db.checkpoint().await?;
        db.close();
        write_store_manifest(&layout)?;
    }
    write_repository_identity_marker(&project_root, "proj_doctor_selected")?;

    let open_options = TraceDecayOpenOptions {
        profile_root: Some(profile_root.clone()),
        global_db_path: Some(dir.path().join("global.db")),
    };
    let selected_db = profile_root.join("projects/proj_doctor_selected/tracedecay.db");
    let legacy_db = profile_root.join("projects/proj_doctor_legacy/tracedecay.db");
    let selected_before = std::fs::read(&selected_db)?;
    let legacy_before = std::fs::read(&legacy_db)?;

    let resolution = resolve_current_project_store(&project_root, &open_options).await;
    let diagnostic = format!("{resolution:?}");
    assert!(
        diagnostic.contains("identity cutover conflict"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("proj_doctor_selected"), "{diagnostic}");
    assert!(diagnostic.contains("proj_doctor_legacy"), "{diagnostic}");
    assert!(
        diagnostic.contains("tracedecay migrate consolidate"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("--source-project-id proj_doctor_legacy"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("--target-project-id proj_doctor_selected"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("no files changed"), "{diagnostic}");
    assert!(!diagnostic.contains("Uninitialized"), "{diagnostic}");
    assert_eq!(std::fs::read(selected_db)?, selected_before);
    assert_eq!(std::fs::read(legacy_db)?, legacy_before);
    Ok(())
}

#[tokio::test]
async fn registry_backed_profile_shard_is_not_stale_without_marker()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::TempDir::new()?;
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    let shard_relpath = Path::new("projects").join("proj_doctor");
    let shard_root = profile_root.join(&shard_relpath);
    std::fs::create_dir_all(&project_root)?;
    std::fs::create_dir_all(&shard_root)?;
    let project_root = canonical_temp_path(&project_root);
    std::fs::write(shard_root.join("tracedecay.db"), b"graph")?;
    let db = crate::global_db::GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .ok_or_else(|| std::io::Error::other("could not open global db"))?;
    db.upsert(&project_root, 42).await;
    db.upsert_code_project("proj_doctor", &project_root, None, None, Some("main"))
        .await
        .ok_or_else(|| std::io::Error::other("could not upsert project"))?;
    db.upsert_store_instance(StoreInstanceUpsert {
        store_id: "store:proj_doctor:profile_sharded".to_string(),
        project_id: "proj_doctor".to_string(),
        store_kind: "code_project".to_string(),
        storage_mode: "profile_sharded".to_string(),
        store_relpath: shard_relpath.to_string_lossy().to_string(),
        manifest_relpath: Some(crate::storage::STORE_MANIFEST_FILENAME.to_string()),
        last_verified_at: Some(1_800_000_000),
        last_write_at: Some(1_800_000_000),
    })
    .await
    .ok_or_else(|| std::io::Error::other("could not upsert store"))?;

    assert_eq!(
        classify_project_storage(&project_root),
        DoctorStorageStatus::Stale
    );
    assert_eq!(
        classify_project_storage_with_registry(&project_root, &db, Some(&profile_root)).await,
        DoctorStorageStatus::ProfileSharded
    );
    #[cfg(unix)]
    {
        let symlinked_profile_root = dir.path().join("profile-link");
        symlink(&profile_root, &symlinked_profile_root)?;
        assert_eq!(
            classify_project_storage_with_registry(
                &project_root,
                &db,
                Some(&symlinked_profile_root)
            )
            .await,
            DoctorStorageStatus::ProfileSharded
        );
    }
    Ok(())
}

#[tokio::test]
async fn registry_backed_profile_shard_manifest_relpath_uses_profile_root()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::TempDir::new()?;
    let profile_root = canonical_temp_path(&dir.path().join("profile"));
    let project_root = canonical_temp_path(&dir.path().join("repo"));
    let shard_relpath = Path::new("projects").join("proj_doctor_manifest");
    let manifest_relpath = shard_relpath.join(crate::storage::STORE_MANIFEST_FILENAME);
    let shard_root = profile_root.join(&shard_relpath);
    std::fs::create_dir_all(&project_root)?;
    std::fs::create_dir_all(&shard_root)?;
    std::fs::write(profile_root.join(&manifest_relpath), b"manifest")?;
    let db = crate::global_db::GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .ok_or_else(|| std::io::Error::other("could not open global db"))?;
    db.upsert(&project_root, 42).await;
    db.upsert_code_project(
        "proj_doctor_manifest",
        &project_root,
        None,
        None,
        Some("main"),
    )
    .await
    .ok_or_else(|| std::io::Error::other("could not upsert project"))?;
    db.upsert_store_instance(StoreInstanceUpsert {
        store_id: "store:proj_doctor_manifest:profile_sharded".to_string(),
        project_id: "proj_doctor_manifest".to_string(),
        store_kind: "code_project".to_string(),
        storage_mode: "profile_sharded".to_string(),
        store_relpath: shard_relpath.to_string_lossy().to_string(),
        manifest_relpath: Some(manifest_relpath.to_string_lossy().to_string()),
        last_verified_at: Some(1_800_000_000),
        last_write_at: Some(1_800_000_000),
    })
    .await
    .ok_or_else(|| std::io::Error::other("could not upsert store"))?;

    assert_eq!(
        classify_project_storage_with_registry(&project_root, &db, Some(&profile_root)).await,
        DoctorStorageStatus::ManifestReconstructable
    );
    Ok(())
}

#[tokio::test]
async fn registry_backed_profile_shard_rejects_unsafe_store_relpath()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::TempDir::new()?;
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    let outside_root = dir.path().join("outside");
    std::fs::create_dir_all(&project_root)?;
    std::fs::create_dir_all(&outside_root)?;
    let project_root = canonical_temp_path(&project_root);
    std::fs::write(outside_root.join("tracedecay.db"), b"graph")?;
    let db = crate::global_db::GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .ok_or_else(|| std::io::Error::other("could not open global db"))?;
    db.upsert(&project_root, 42).await;
    db.upsert_code_project(
        "proj_doctor_escape",
        &project_root,
        None,
        None,
        Some("main"),
    )
    .await
    .ok_or_else(|| std::io::Error::other("could not upsert project"))?;
    db.upsert_store_instance(StoreInstanceUpsert {
        store_id: "store:proj_doctor_escape:profile_sharded".to_string(),
        project_id: "proj_doctor_escape".to_string(),
        store_kind: "code_project".to_string(),
        storage_mode: "profile_sharded".to_string(),
        store_relpath: "../outside".to_string(),
        manifest_relpath: Some(crate::storage::STORE_MANIFEST_FILENAME.to_string()),
        last_verified_at: Some(1_800_000_000),
        last_write_at: Some(1_800_000_000),
    })
    .await
    .ok_or_else(|| std::io::Error::other("could not upsert store"))?;

    assert_eq!(
        classify_project_storage_with_registry(&project_root, &db, Some(&profile_root)).await,
        DoctorStorageStatus::Stale
    );
    Ok(())
}

#[tokio::test]
async fn registry_drift_findings_report_manifest_identity_mismatches()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::TempDir::new()?;
    let profile_root = canonical_temp_path(&dir.path().join("profile"));
    let registry_root = canonical_temp_path(&dir.path().join("registry-repo"));
    let manifest_root = canonical_temp_path(&dir.path().join("manifest-repo"));
    let shard_relpath = Path::new("projects").join("proj_registry");
    let shard_root = profile_root.join(&shard_relpath);
    std::fs::create_dir_all(&registry_root)?;
    std::fs::create_dir_all(&manifest_root)?;
    std::fs::create_dir_all(&shard_root)?;
    std::fs::write(shard_root.join("tracedecay.db"), b"graph")?;
    std::fs::write(shard_root.join("sessions.db"), b"sessions")?;
    std::fs::write(shard_root.join("branch-meta.json"), b"{}")?;
    let manifest = StoreManifest {
        schema_version: STORE_MANIFEST_SCHEMA_VERSION,
        project_id: Some("proj_manifest".to_string()),
        store_kind: StoreKind::CodeProject,
        storage_mode: StorageMode::ProfileSharded,
        project_root: manifest_root.clone(),
        data_root: shard_root.clone(),
        graph_db_relpath: "tracedecay.db".into(),
        sessions_db_relpath: "sessions.db".into(),
        branch_meta_relpath: "branch-meta.json".into(),
    };
    std::fs::write(
        shard_root.join(STORE_MANIFEST_FILENAME),
        serde_json::to_string_pretty(&manifest)?,
    )?;

    let db = crate::global_db::GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .ok_or_else(|| std::io::Error::other("could not open global db"))?;
    db.upsert_code_project("proj_registry", &registry_root, None, None, Some("main"))
        .await
        .ok_or_else(|| std::io::Error::other("could not upsert project"))?;
    db.upsert_store_instance(StoreInstanceUpsert {
        store_id: "store:proj_registry:profile_sharded".to_string(),
        project_id: "proj_registry".to_string(),
        store_kind: "code_project".to_string(),
        storage_mode: "profile_sharded".to_string(),
        store_relpath: shard_relpath.to_string_lossy().to_string(),
        manifest_relpath: Some(
            shard_relpath
                .join(STORE_MANIFEST_FILENAME)
                .to_string_lossy()
                .to_string(),
        ),
        last_verified_at: Some(1_800_000_000),
        last_write_at: Some(1_800_000_000),
    })
    .await
    .ok_or_else(|| std::io::Error::other("could not upsert store"))?;

    let findings = registry_drift::registry_drift_findings(&db, &profile_root).await;
    let fields: Vec<_> = findings.iter().map(|finding| finding.field).collect();
    assert!(
        fields.contains(&"project_id"),
        "expected project_id drift finding, got {findings:#?}"
    );
    assert!(
        fields.contains(&"project_root"),
        "expected project_root drift finding, got {findings:#?}"
    );
    assert!(
        findings
            .iter()
            .any(|finding| finding.registry_value == "proj_registry"
                && finding.manifest_value == "proj_manifest"),
        "project_id finding should include registry and manifest values: {findings:#?}"
    );

    Ok(())
}

/// A `ForeignOrphan` drift line must render as `Info` severity (no warning
/// count) and must never prescribe `tracedecay update` — the remediation the
/// remove path refuses to perform on a foreign package. Mirrors the pure
/// classifier pattern used for database-recovery guidance.
#[test]
fn foreign_orphan_renders_as_info_without_update_remediation() {
    use crate::automation::skill_materialization::SkillDrift;
    let finding = SkillDrift::ForeignOrphan {
        skill_id: "code-slop-cleanup".to_string(),
        path: std::path::PathBuf::from("/repo/.claude/skills/code-slop-cleanup/SKILL.md"),
    };
    let (level, msg) = super::skill_drift_report("claude/project", &finding);
    assert_eq!(level, super::DriftLevel::Info);
    assert!(
        msg.contains("another installation"),
        "message should explain the foreign origin: {msg}"
    );
    assert!(
        !msg.contains("tracedecay update"),
        "foreign orphan must not prescribe `tracedecay update`: {msg}"
    );
}

/// A self-authored `Orphan` still renders as `Warn` and keeps the update
/// remediation — the classifier must not blanket-downgrade every orphan.
#[test]
fn plain_orphan_still_warns_with_update_remediation() {
    use crate::automation::skill_materialization::SkillDrift;
    let finding = SkillDrift::Orphan {
        skill_id: "code-slop-cleanup".to_string(),
        path: std::path::PathBuf::from("/repo/.claude/skills/code-slop-cleanup/SKILL.md"),
    };
    let (level, msg) = super::skill_drift_report("claude/project", &finding);
    assert_eq!(level, super::DriftLevel::Warn);
    assert!(
        msg.contains("tracedecay update"),
        "plain orphan should still prescribe update: {msg}"
    );
}

#[test]
fn daemon_runtime_parser_extracts_storage_health_and_owner() {
    let parsed = super::daemon_runtime_status(&serde_json::json!({
        "content": [
            {"type": "text", "text": "daemon notice"},
            {
                "type": "text",
                "text": r#"{"tracedecay_version":"0.0.66","process":{"pid":1234},"database":{"canonical_db_path":"/tmp/project.db","quick_check_ok":true,"authority_audit_ok":true,"authority_audit_error":null,"dirty_marker":{"exists":false}},"cursor_session_ingest":{"tracked_transcripts":1,"pending_transcripts":0,"pending_bytes":0,"max_transcript_pending_bytes":0},"cursor_session_placeholder_paths":["${workspaceFolder}/cursor.jsonl"]}"#
            }
        ]
    }))
    .unwrap();

    assert_eq!(
        parsed.pointer("/storage_health/quick_check_ok"),
        Some(&serde_json::Value::Bool(true))
    );
    assert_eq!(
        parsed.pointer("/storage_health/daemon_owner_pid"),
        Some(&serde_json::json!(1234))
    );
    assert_eq!(
        parsed.pointer("/storage_health/daemon_version"),
        Some(&serde_json::json!("0.0.66"))
    );
    assert_eq!(
        parsed.pointer("/storage_health/authority_audit_ok"),
        Some(&serde_json::Value::Bool(true))
    );
    assert_eq!(
        parsed.pointer("/storage_health/authority_audit_error"),
        Some(&serde_json::Value::Null)
    );
    assert_eq!(
        parsed.pointer("/cursor_session_ingest/tracked_transcripts"),
        Some(&serde_json::json!(1))
    );
    assert_eq!(
        parsed.pointer("/cursor_session_placeholder_paths/0"),
        Some(&serde_json::json!("${workspaceFolder}/cursor.jsonl"))
    );
}

#[test]
fn daemon_runtime_request_enables_authority_audit() {
    assert_eq!(
        super::daemon_runtime_args(),
        serde_json::json!({
            "format": "json",
            "authority_audit": true,
            "session_ingest_health": true,
        })
    );
}

#[test]
fn daemon_runtime_parser_rejects_missing_json_payload() {
    let error = super::daemon_runtime_status(&serde_json::json!({ "content": [] })).unwrap_err();
    assert!(error.to_string().contains("returned no JSON payload"));
}

#[test]
fn daemon_runtime_parser_rejects_missing_database_telemetry() {
    let error = super::daemon_runtime_status(&serde_json::json!({
        "content": [{"type": "text", "text": r#"{"process":{"pid":1234}}"#}]
    }))
    .unwrap_err();
    assert!(error.to_string().contains("omitted database telemetry"));
}
