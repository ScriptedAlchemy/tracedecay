use super::*;
use std::fs;
use tempfile::TempDir;

/// A retired legacy enrollment file is never an identity authority for the
/// synchronous walk: discovery and initialization answer through the `.git/`
/// marker, the profile store, or the registry — never a working-tree file.
#[test]
fn legacy_enrollment_marker_alone_is_not_discovered() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let child = root.join("src/storage");
    fs::create_dir_all(&child).unwrap();
    write_enrollment(root);

    assert_eq!(discover_project_root(&child), None);
    assert!(!TraceDecay::is_initialized(root));
}

#[test]
fn repository_identity_marker_is_discovered_without_graph_db() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("repo");
    let child = root.join("src/storage");
    fs::create_dir_all(&child).unwrap();
    fs::write(root.join("lib.rs"), "pub fn discovered() {}\n").unwrap();
    init_repo_with_commit(&root);
    assert!(write_repository_identity_marker(&root, "proj_123").unwrap());

    assert_eq!(discover_project_root(&child), Some(root.clone()));
    assert!(TraceDecay::is_initialized(&root));
}

#[test]
fn legacy_enrollment_marker_preserves_profile_identity() {
    let dir = TempDir::new().unwrap();
    write_enrollment(dir.path());

    let marker = read_legacy_enrollment_marker(dir.path())
        .unwrap()
        .expect("marker should be present");

    assert_eq!(
        marker,
        EnrollmentMarker {
            project_id: "proj_123".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        }
    );
}

#[test]
fn invalid_legacy_enrollment_marker_is_not_treated_as_initialized() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".tracedecay")).unwrap();
    fs::write(
        root.join(".tracedecay/enrollment.json"),
        r#"{"project_id":"../bad","storage_mode":"profile_sharded"}"#,
    )
    .unwrap();

    assert_eq!(discover_project_root(root), None);
    assert!(!TraceDecay::is_initialized(root));
    assert!(read_legacy_enrollment_marker(root).is_err());
}

#[test]
fn repository_identity_marker_rejects_unknown_schema() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn marker_test() {}\n").unwrap();
    init_repo_with_commit(&project);
    let marker_path = tracedecay::worktree::git_common_dir(&project)
        .unwrap()
        .join("tracedecay-project.json");
    fs::write(
        marker_path,
        r#"{"schema_version":99,"project_id":"proj_future"}"#,
    )
    .unwrap();

    let error = read_repository_identity_marker(&project).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unsupported repository identity schema_version=99"),
        "unexpected error: {error}"
    );
}

#[test]
fn profile_sharded_layout_rejects_dot_and_hidden_project_ids() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let profile = dir.path().join("profile");
    fs::create_dir_all(&project).unwrap();

    for project_id in [".", ".hidden"] {
        let marker = EnrollmentMarker {
            project_id: project_id.to_string(),
            storage_mode: StorageMode::ProfileSharded,
        };

        let err = profile_sharded_layout(&project, &profile, &marker).unwrap_err();

        assert!(
            err.to_string().contains("single safe path segment"),
            "project_id {project_id:?} should be rejected, got {err}"
        );
    }
}

#[test]
fn project_local_marker_without_graph_db_is_not_initialized() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".tracedecay")).unwrap();
    fs::write(
        root.join(".tracedecay/enrollment.json"),
        r#"{"project_id":"proj_local","storage_mode":"project_local"}"#,
    )
    .unwrap();

    assert_eq!(discover_project_root(root), None);
    assert!(!TraceDecay::is_initialized(root));
}

#[test]
fn profile_sharded_layout_maps_marker_to_profile_store_paths() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let profile = dir.path().join("profile");
    fs::create_dir_all(&project).unwrap();
    let marker = EnrollmentMarker {
        project_id: "proj_123".to_string(),
        storage_mode: StorageMode::ProfileSharded,
    };

    let layout = profile_sharded_layout(&project, &profile, &marker).unwrap();

    let data_root = profile.join("projects/proj_123");
    assert_eq!(layout.project_root, project);
    assert_eq!(layout.storage_mode, StorageMode::ProfileSharded);
    assert_eq!(layout.identity.project_id.as_deref(), Some("proj_123"));
    assert_eq!(layout.data_root, data_root);
    assert_eq!(
        layout.graph_db_path,
        profile.join("projects/proj_123/tracedecay.db")
    );
    assert_eq!(
        layout.config_path,
        profile.join("projects/proj_123/config.json")
    );
    assert_eq!(
        layout.branch_meta_path,
        profile.join("projects/proj_123/branch-meta.json")
    );
    assert_eq!(
        layout.sessions_db_path,
        profile.join("projects/proj_123/sessions.db")
    );
    assert_eq!(
        layout.response_handle_root,
        profile.join("projects/proj_123/response-handles")
    );
    assert_eq!(
        layout.lcm_payload_root,
        profile.join("projects/proj_123/lcm-payloads")
    );
    assert_eq!(
        layout.dashboard_root,
        profile.join("projects/proj_123/dashboard")
    );
    assert_eq!(
        layout.manifest_path,
        Some(profile.join(format!("projects/proj_123/{STORE_MANIFEST_FILENAME}")))
    );
    assert_eq!(layout.dirty_path, profile.join("projects/proj_123/dirty"));
    assert_eq!(
        layout.sync_lock_path,
        profile.join("projects/proj_123/sync.lock")
    );
    assert_eq!(
        layout.branch_add_lock_path,
        profile.join("projects/proj_123/.branch-add.lock")
    );
}
