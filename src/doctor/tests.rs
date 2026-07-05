use super::*;
use crate::global_db::StoreInstanceUpsert;
use crate::storage::{
    StorageMode, StoreKind, StoreManifest, STORE_MANIFEST_FILENAME, STORE_MANIFEST_SCHEMA_VERSION,
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

#[test]
fn format_bytes_fractional_kb() {
    // 2048 bytes = 2.0 KB
    assert_eq!(format_bytes(2048), "2.0 KB");
    // 1536 = 1.5 KB
    assert_eq!(format_bytes(1536), "1.5 KB");
}

#[tokio::test]
async fn current_project_store_resolves_profile_shard_via_registry_alias(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
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
    match resolve_current_project_store(&project_root, &open_options).await {
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
        resolve_current_project_store(&unregistered, &open_options).await,
        CurrentProjectStore::Uninitialized
    ));
    Ok(())
}

#[tokio::test]
async fn registry_backed_profile_shard_is_not_stale_without_marker(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
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
async fn registry_backed_profile_shard_manifest_relpath_uses_profile_root(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
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
async fn registry_backed_profile_shard_rejects_unsafe_store_relpath(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
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
async fn registry_drift_findings_report_manifest_identity_mismatches(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
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
