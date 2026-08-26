//! Store manifest round-trip and symlink-safety tests.

use super::*;

#[test]
fn store_manifest_roundtrips_from_profile_sharded_layout() {
    let dir = TempDir::new().unwrap();
    let temp_root = canonical_temp_path(dir.path());
    let project = temp_root.join("repo");
    let profile = temp_root.join("profile");
    fs::create_dir_all(&project).unwrap();
    let marker = EnrollmentMarker {
        project_id: "proj_123".to_string(),
        storage_mode: StorageMode::ProfileSharded,
    };
    let layout = profile_sharded_layout(&project, &profile, &marker).unwrap();
    fs::create_dir_all(&layout.data_root).unwrap();

    let written = write_store_manifest(&layout).unwrap();
    let manifest = read_store_manifest(layout.manifest_path.as_ref().unwrap()).unwrap();

    assert_eq!(manifest, written);
    assert_eq!(manifest.project_id.as_deref(), Some("proj_123"));
    assert_eq!(manifest.storage_mode, StorageMode::ProfileSharded);
    assert_eq!(manifest.data_root, layout.data_root);
    assert_eq!(manifest.graph_db_relpath, Path::new("tracedecay.db"));
    assert_eq!(manifest.sessions_db_relpath, Path::new("sessions.db"));
    assert_eq!(manifest.branch_meta_relpath, Path::new("branch-meta.json"));
}

#[cfg(unix)]
#[test]
fn store_manifest_write_rejects_symlinked_atomic_temp_path() {
    let dir = TempDir::new().unwrap();
    let temp_root = canonical_temp_path(dir.path());
    let project = temp_root.join("repo");
    let profile = temp_root.join("profile");
    let outside = temp_root.join("outside.tmp");
    fs::create_dir_all(&project).unwrap();
    fs::write(&outside, b"outside").unwrap();
    let marker = EnrollmentMarker {
        project_id: "proj_123".to_string(),
        storage_mode: StorageMode::ProfileSharded,
    };
    let layout = profile_sharded_layout(&project, &profile, &marker).unwrap();
    let manifest_path = layout.manifest_path.as_ref().unwrap();
    PrivateStoreIo::create_dir_all(manifest_path.parent().unwrap()).unwrap();
    symlink(&outside, manifest_path.with_extension("json.tmp")).unwrap();

    let err = write_store_manifest(&layout).unwrap_err();

    assert!(err.to_string().contains("symlink"));
    assert!(!manifest_path.exists());
    assert_eq!(fs::read(&outside).unwrap(), b"outside");
}

#[cfg(unix)]
#[test]
fn store_manifest_write_rejects_symlinked_parent_components() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let outside = dir.path().join("outside");
    let profile = dir.path().join("profile");
    let projects_link = profile.join("projects");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::create_dir_all(&profile).unwrap();
    symlink(&outside, &projects_link).unwrap();
    let marker = EnrollmentMarker {
        project_id: "proj_123".to_string(),
        storage_mode: StorageMode::ProfileSharded,
    };
    let layout = profile_sharded_layout(&project, &profile, &marker).unwrap();

    let err = write_store_manifest(&layout).unwrap_err();

    assert!(err.to_string().contains("symlink"));
    assert!(
        !outside.join("proj_123").exists(),
        "manifest writer must not create directories through a symlinked parent"
    );
    assert!(
        !outside
            .join(format!("proj_123/{STORE_MANIFEST_FILENAME}"))
            .exists()
    );
}
