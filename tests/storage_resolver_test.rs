use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::symlink;
use tempfile::TempDir;
use tokio::sync::Mutex;
use tracedecay::branch_meta::{self, BranchMeta};
use tracedecay::config::{discover_project_root, get_config_path, load_config};
use tracedecay::config::{TraceDecayConfig, USER_DATA_DIR_ENV};
use tracedecay::db::Database;
use tracedecay::global_db::GlobalDb;
use tracedecay::mcp::response_handles::{
    retrieve_response_handle, store_response_handle, ResponseHandleLookup,
};
use tracedecay::sessions::cursor::project_session_db_path;
use tracedecay::storage::{
    default_profile_project_id, default_profile_sharded_layout, profile_sharded_layout,
    read_enrollment_marker, read_store_manifest, resolve_layout, resolve_lcm_payload_root,
    resolve_project_session_db_path, resolve_response_handle_root, write_store_manifest,
    ActiveProjectContext, EnrollmentMarker, GraphScopeId, PrivateStoreIo, ProjectPath, StorageMode,
    StoreArtifactPath, STORE_MANIFEST_FILENAME,
};
use tracedecay::tracedecay::TraceDecay;

static HOME_ENV_LOCK: Mutex<()> = Mutex::const_new(());

struct HomeGuard {
    previous_home: Option<OsString>,
    previous_userprofile: Option<OsString>,
    previous_data_dir: Option<OsString>,
}

impl HomeGuard {
    fn set(home: &Path) -> Self {
        let previous_home = std::env::var_os("HOME");
        let previous_userprofile = std::env::var_os("USERPROFILE");
        let previous_data_dir = std::env::var_os(USER_DATA_DIR_ENV);
        fs::create_dir_all(home).unwrap();
        let home = canonical_temp_path(home);
        std::env::set_var("HOME", &home);
        std::env::set_var("USERPROFILE", &home);
        std::env::set_var(USER_DATA_DIR_ENV, home.join(".tracedecay"));
        Self {
            previous_home,
            previous_userprofile,
            previous_data_dir,
        }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match self.previous_home.take() {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match self.previous_userprofile.take() {
            Some(value) => std::env::set_var("USERPROFILE", value),
            None => std::env::remove_var("USERPROFILE"),
        }
        match self.previous_data_dir.take() {
            Some(value) => std::env::set_var(USER_DATA_DIR_ENV, value),
            None => std::env::remove_var(USER_DATA_DIR_ENV),
        }
    }
}

fn write_enrollment(root: &Path) {
    fs::create_dir_all(root.join(".tracedecay")).unwrap();
    fs::write(
        root.join(".tracedecay/enrollment.json"),
        r#"{"project_id":"proj_123","storage_mode":"profile_sharded"}"#,
    )
    .unwrap();
}

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

fn test_home(dir: &TempDir) -> PathBuf {
    let home = dir.path().join("home");
    fs::create_dir_all(&home).unwrap();
    canonical_temp_path(&home)
}

#[test]
fn enrollment_marker_is_discovered_without_graph_db() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let child = root.join("src/storage");
    fs::create_dir_all(&child).unwrap();
    write_enrollment(root);

    assert_eq!(discover_project_root(&child), Some(root.to_path_buf()));
    assert!(TraceDecay::is_initialized(root));
}

#[test]
fn enrollment_marker_preserves_profile_identity() {
    let dir = TempDir::new().unwrap();
    write_enrollment(dir.path());

    let marker = read_enrollment_marker(dir.path())
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
fn invalid_enrollment_marker_is_not_treated_as_initialized() {
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
    assert!(read_enrollment_marker(root).is_err());
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
    write_enrollment(&project);
    let marker = read_enrollment_marker(&project).unwrap().unwrap();

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

#[test]
fn store_manifest_roundtrips_from_profile_sharded_layout() {
    let dir = TempDir::new().unwrap();
    let temp_root = canonical_temp_path(dir.path());
    let project = temp_root.join("repo");
    let profile = temp_root.join("profile");
    fs::create_dir_all(&project).unwrap();
    write_enrollment(&project);
    let marker = read_enrollment_marker(&project).unwrap().unwrap();
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
    write_enrollment(&project);
    let marker = read_enrollment_marker(&project).unwrap().unwrap();
    let layout = profile_sharded_layout(&project, &profile, &marker).unwrap();

    let err = write_store_manifest(&layout).unwrap_err();

    assert!(err.to_string().contains("symlink"));
    assert!(
        !outside.join("proj_123").exists(),
        "manifest writer must not create directories through a symlinked parent"
    );
    assert!(!outside
        .join(format!("proj_123/{STORE_MANIFEST_FILENAME}"))
        .exists());
}

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
    write_enrollment(&project);

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

    assert_eq!(get_config_path(&project), shard_root.join("config.json"));
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

    assert_eq!(
        get_config_path(&project),
        profile_root.join(format!("projects/{project_id}/config.json"))
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

#[tokio::test]
async fn resolved_project_store_helpers_route_profile_sharded_session_artifacts() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let home = test_home(&dir);
    let profile_root = home.join(".tracedecay");
    fs::create_dir_all(&project).unwrap();
    let _home_guard = HomeGuard::set(&home);
    write_enrollment(&project);

    assert_eq!(
        resolve_project_session_db_path(&project).unwrap(),
        profile_root.join("projects/proj_123/sessions.db")
    );
    assert_eq!(
        resolve_response_handle_root(&project).unwrap(),
        profile_root.join("projects/proj_123/response-handles")
    );
    assert_eq!(
        resolve_lcm_payload_root(&project).unwrap(),
        profile_root.join("projects/proj_123/lcm-payloads")
    );
    assert_eq!(
        project_session_db_path(&project),
        profile_root.join("projects/proj_123/sessions.db")
    );
}

#[tokio::test]
async fn resolved_project_store_helpers_default_to_profile_sharded_artifact_paths() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let home = test_home(&dir);
    let profile_root = home.join(".tracedecay");
    fs::create_dir_all(&project).unwrap();
    let _home_guard = HomeGuard::set(&home);
    let project_id = default_profile_project_id(&project);

    assert_eq!(
        resolve_project_session_db_path(&project).unwrap(),
        profile_root.join(format!("projects/{project_id}/sessions.db"))
    );
    assert_eq!(
        project_session_db_path(&project),
        profile_root.join(format!("projects/{project_id}/sessions.db"))
    );
}

#[tokio::test]
async fn hermes_profile_home_session_path_wins_over_default_profile_shard() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let hermes_home = dir.path().join(".hermes");
    let home = test_home(&dir);
    fs::create_dir_all(&hermes_home).unwrap();
    fs::write(
        hermes_home.join("config.yaml"),
        "memory:\n  provider: tracedecay\n",
    )
    .unwrap();
    let _home_guard = HomeGuard::set(&home);

    let expected = hermes_home.join(".tracedecay/sessions.db");
    assert_eq!(project_session_db_path(&hermes_home), expected);
    assert_eq!(
        tracedecay::sessions::cursor::resolved_project_session_db_path(&hermes_home)
            .await
            .unwrap(),
        expected
    );
}

#[tokio::test]
async fn trace_decay_init_defaults_to_profile_shard_without_repo_marker() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let child = project.join("src");
    let home = test_home(&dir);
    let profile_root = home.join(".tracedecay");
    fs::create_dir_all(&child).unwrap();
    let _home_guard = HomeGuard::set(&home);
    let project_id = default_profile_project_id(&project);
    let shard_root = profile_root.join(format!("projects/{project_id}"));

    assert!(!TraceDecay::is_initialized(&project));

    let cg = TraceDecay::init(&project).await.unwrap();

    assert_eq!(cg.store_layout().storage_mode, StorageMode::ProfileSharded);
    assert_eq!(cg.store_layout().data_root, shard_root);
    assert_eq!(cg.db_path(), shard_root.join("tracedecay.db"));
    assert_eq!(discover_project_root(&child), Some(project.clone()));
    assert!(!project.join(".tracedecay").exists());
    assert!(shard_root.join("config.json").exists());
    assert!(shard_root.join(STORE_MANIFEST_FILENAME).exists());
}

#[tokio::test]
async fn trace_decay_init_registers_default_profile_shard_globally() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let home = test_home(&dir);
    fs::create_dir_all(&project).unwrap();
    let _home_guard = HomeGuard::set(&home);
    let project_id = default_profile_project_id(&project);

    TraceDecay::init(&project).await.unwrap();
    let db = GlobalDb::open().await.unwrap();
    let resolution = db.resolve_project_store_by_alias(&project).await.unwrap();

    assert_eq!(resolution.project.project_id, project_id);
}

#[tokio::test]
async fn response_handles_route_to_profile_shard_when_enrolled() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let home = test_home(&dir);
    let shard_root = home.join(".tracedecay/projects/proj_123");
    fs::create_dir_all(&project).unwrap();
    let _home_guard = HomeGuard::set(&home);
    write_enrollment(&project);

    let stored = store_response_handle(&project, r#"{"items":[1]}"#, 1_720_000_000).unwrap();
    let shard_path = shard_root
        .join("response-handles")
        .join(format!("{}.json", stored.handle));

    assert!(shard_path.exists());
    assert!(!project.join(".tracedecay/response-handles").exists());
    assert!(matches!(
        retrieve_response_handle(&project, &stored.handle, 1_720_000_001).unwrap(),
        ResponseHandleLookup::Found(record) if record.content == r#"{"items":[1]}"#
    ));
}

#[tokio::test]
async fn trace_decay_open_uses_profile_shard_paths_from_enrollment_marker() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let home = test_home(&dir);
    let profile_root = home.join(".tracedecay");
    let shard_root = profile_root.join("projects/proj_123");
    fs::create_dir_all(project.join(".tracedecay")).unwrap();
    fs::create_dir_all(&shard_root).unwrap();
    let _home_guard = HomeGuard::set(&home);

    write_enrollment(&project);
    let repo_local_config = TraceDecayConfig {
        root_dir: "repo-local-marker-config".to_string(),
        ..TraceDecayConfig::default()
    };
    fs::write(
        project.join(".tracedecay/config.json"),
        serde_json::to_string_pretty(&repo_local_config).unwrap(),
    )
    .unwrap();
    let shard_config = TraceDecayConfig {
        root_dir: project.to_string_lossy().to_string(),
        ..TraceDecayConfig::default()
    };
    fs::write(
        shard_root.join("config.json"),
        serde_json::to_string_pretty(&shard_config).unwrap(),
    )
    .unwrap();
    Database::initialize(&shard_root.join("tracedecay.db"))
        .await
        .unwrap();
    let meta = BranchMeta::new_for_dir(&shard_root, "main");
    branch_meta::save_branch_meta(&shard_root, &meta).unwrap();

    let opened = TraceDecay::open(&project).await.unwrap();

    assert_eq!(opened.db_path(), shard_root.join("tracedecay.db"));
    assert_eq!(opened.get_config().root_dir, project.to_string_lossy());
    assert_eq!(opened.serving_branch(), Some("main"));
}
