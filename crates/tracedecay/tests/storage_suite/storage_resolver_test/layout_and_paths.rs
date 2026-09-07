//! Store layout resolution, config path, and path-safety guard tests.

use super::*;

#[test]
fn resolve_layout_defaults_to_profile_shard_without_marker_or_local_db() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("repo");
    let profile = dir.path().join("profile");
    fs::create_dir_all(&root).unwrap();

    let layout = resolve_layout(&root, &profile).unwrap();
    let project_id = default_profile_project_id(&root);

    assert_eq!(layout.storage_mode, StorageMode::ProfileSharded);
    assert_eq!(
        layout.identity.project_id.as_deref(),
        Some(project_id.as_str())
    );
    assert_eq!(
        layout.data_root,
        profile.join(format!("projects/{project_id}"))
    );
    assert_eq!(
        layout.graph_db_path,
        profile.join(format!("projects/{project_id}/tracedecay.db"))
    );
}

#[tokio::test]
async fn config_path_uses_profile_shard_when_enrolled() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let home = test_home(&dir);
    let shard_root = home.join(".tracedecay/projects/proj_123");
    fs::create_dir_all(project.join(".tracedecay")).unwrap();
    fs::create_dir_all(&shard_root).unwrap();
    let _home_guard = HomeGuard::set(&home);
    fs::write(project.join("lib.rs"), "pub fn enrolled() {}\n").unwrap();
    init_repo_with_commit(&project);
    assert!(write_repository_identity_marker(&project, "proj_123").unwrap());

    // A retired legacy config left in the working tree stays ignored.
    let repo_local_config = TraceDecayConfig {
        root_dir: "repo-local-config".to_string(),
        ..TraceDecayConfig::default()
    };
    fs::write(
        project.join(".tracedecay/config.json"),
        serde_json::to_string_pretty(&repo_local_config).unwrap(),
    )
    .unwrap();
    let shard_config = TraceDecayConfig {
        root_dir: "profile-shard-config".to_string(),
        ..TraceDecayConfig::default()
    };
    fs::write(
        shard_root.join("config.json"),
        serde_json::to_string_pretty(&shard_config).unwrap(),
    )
    .unwrap();

    assert_path_eq(get_config_path(&project), shard_root.join("config.json"));
    assert_eq!(
        load_config(&project).unwrap().root_dir,
        "profile-shard-config"
    );
}

#[tokio::test]
async fn config_path_defaults_to_profile_shard_without_enrollment() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let home = test_home(&dir);
    let profile_root = home.join(".tracedecay");
    fs::create_dir_all(&project).unwrap();
    let _home_guard = HomeGuard::set(&home);
    let project_id = default_profile_project_id(&project);

    assert_path_eq(
        get_config_path(&project),
        profile_root.join(format!("projects/{project_id}/config.json")),
    );
}

#[test]
fn active_project_context_keeps_layout_and_scope_identity() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    let profile = dir.path().join("profile");
    let layout = default_profile_sharded_layout(&root, &profile).unwrap();

    let context = ActiveProjectContext::new(layout.clone(), GraphScopeId::Project);

    assert_eq!(context.layout, layout);
    assert_eq!(context.scope_id, GraphScopeId::Project);
    assert_eq!(
        context.query_target.graph_db_path,
        profile.join(format!(
            "projects/{}/tracedecay.db",
            default_profile_project_id(&root)
        ))
    );
}

#[test]
fn project_path_accepts_contained_relative_and_absolute_paths() {
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path()).join("repo");
    let file = root.join("src/lib.rs");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "pub fn lib() {}").unwrap();
    let expected_file = file.canonicalize().unwrap_or_else(|_| file.clone());

    let relative = ProjectPath::resolve(&root, Path::new("src/lib.rs")).unwrap();
    assert_eq!(relative.relative_path(), Path::new("src/lib.rs"));
    assert_eq!(relative.absolute_path(), expected_file);

    let absolute = ProjectPath::resolve(&root, &file).unwrap();
    assert_eq!(absolute.relative_path(), Path::new("src/lib.rs"));
    assert_eq!(absolute.absolute_path(), expected_file);
}

#[test]
fn project_path_rejects_parent_absolute_nul_non_normal_and_symlink_escapes() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("repo");
    let outside = dir.path().join("outside");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.txt"), "secret").unwrap();

    assert!(ProjectPath::resolve(&root, Path::new("../secret.txt")).is_err());
    assert!(ProjectPath::resolve(&root, &outside.join("secret.txt")).is_err());
    assert!(ProjectPath::resolve(&root, Path::new("src/../lib.rs")).is_err());
    assert!(ProjectPath::resolve(&root, Path::new("src/./lib.rs")).is_err());
    assert!(ProjectPath::resolve(&root, Path::new("src/bad\0name.rs")).is_err());

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
        assert!(ProjectPath::resolve(&root, Path::new("escape/secret.txt")).is_err());
    }
}

#[test]
fn store_artifact_path_accepts_only_normalized_relative_paths() {
    let dir = TempDir::new().unwrap();
    let store_root = canonical_temp_path(dir.path()).join("store");
    fs::create_dir_all(&store_root).unwrap();

    let artifact =
        StoreArtifactPath::resolve(&store_root, Path::new("response-handles/abc.json")).unwrap();

    assert_eq!(
        artifact.relative_path(),
        Path::new("response-handles/abc.json")
    );
    assert_eq!(
        artifact.absolute_path(),
        store_root.join("response-handles/abc.json")
    );
    assert!(StoreArtifactPath::resolve(&store_root, Path::new("../abc.json")).is_err());
    assert!(StoreArtifactPath::resolve(&store_root, &store_root.join("abc.json")).is_err());
    assert!(
        StoreArtifactPath::resolve(&store_root, Path::new("response-handles/./abc.json")).is_err()
    );
    assert!(StoreArtifactPath::resolve(&store_root, Path::new("bad\0name")).is_err());
}

#[cfg(unix)]
#[test]
fn store_artifact_path_rejects_symlinked_relative_components() {
    let dir = TempDir::new().unwrap();
    let store_root = dir.path().join("store");
    let outside = dir.path().join("outside");
    fs::create_dir_all(&store_root).unwrap();
    fs::create_dir_all(&outside).unwrap();
    symlink(&outside, store_root.join("escape")).unwrap();

    let err = StoreArtifactPath::resolve(&store_root, Path::new("escape/abc.json")).unwrap_err();

    assert!(
        err.to_string().contains("symlink") || err.to_string().contains("escapes"),
        "symlinked store artifact relpath should be rejected, got {err}"
    );
}

#[test]
fn private_store_io_creates_private_dirs_and_files() {
    let dir = TempDir::new().unwrap();
    let private_dir = canonical_temp_path(dir.path()).join("private");
    let private_file = private_dir.join("config.json");

    PrivateStoreIo::create_dir_all(&private_dir).unwrap();
    PrivateStoreIo::write_file(&private_file, b"{}").unwrap();

    assert_eq!(fs::read_to_string(&private_file).unwrap(), "{}");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(
            fs::metadata(&private_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&private_file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[cfg(windows)]
#[test]
fn private_store_io_allows_verbatim_absolute_paths() {
    let dir = TempDir::new().unwrap();
    let private_file = fs::canonicalize(dir.path())
        .unwrap()
        .join("private")
        .join("enrollment.json");

    PrivateStoreIo::write_file(&private_file, b"{}").unwrap();

    assert_eq!(fs::read_to_string(&private_file).unwrap(), "{}");
}

#[cfg(unix)]
#[test]
fn private_store_io_rejects_symlinked_parent_components() {
    let dir = TempDir::new().unwrap();
    let outside = dir.path().join("outside");
    let private_root = dir.path().join("private");
    let link = private_root.join("link");
    fs::create_dir_all(&outside).unwrap();
    fs::create_dir_all(&private_root).unwrap();
    symlink(&outside, &link).unwrap();

    let err = PrivateStoreIo::write_file(&link.join("nested/config.json"), b"{}").unwrap_err();

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(!outside.join("nested/config.json").exists());
}
