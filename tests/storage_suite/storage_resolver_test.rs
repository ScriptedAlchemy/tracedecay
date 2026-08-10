use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use tempfile::TempDir;
use tracedecay::branch_meta::{self, BranchMeta};
use tracedecay::config::{TraceDecayConfig, USER_DATA_DIR_ENV};
use tracedecay::config::{
    discover_project_root, get_config_path, load_config, save_config_to_path,
};
use tracedecay::global_db::GlobalDb;
use tracedecay::mcp::response_handles::{
    ResponseHandleLookup, retrieve_response_handle, store_response_handle,
};
use tracedecay::memory::types::{AddFactRequest, MemoryCategory};
use tracedecay::sessions::SessionRecord;
use tracedecay::sessions::cursor::{project_session_db_path, resolved_project_session_db_path};
use tracedecay::storage::{
    ActiveProjectContext, EnrollmentMarker, GraphScopeId, PrivateStoreIo, ProjectPath,
    STORE_MANIFEST_FILENAME, STORE_MANIFEST_SCHEMA_VERSION, StorageMode, StoreArtifactPath,
    StoreKind, StoreManifest, default_profile_project_id, default_profile_sharded_layout,
    profile_sharded_layout, read_enrollment_marker, read_repository_identity_marker,
    read_store_manifest, repository_identity_path, resolve_layout, resolve_lcm_payload_root,
    resolve_project_session_db_path, resolve_response_handle_root,
    write_repository_identity_marker, write_store_manifest, write_store_manifest_to_path,
};
use tracedecay::tracedecay::{TraceDecay, TraceDecayOpenOptions};

use crate::support::HOME_ENV_LOCK;

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
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::set_var("USERPROFILE", &home);
            std::env::set_var(USER_DATA_DIR_ENV, home.join(".tracedecay"));
        }
        Self {
            previous_home,
            previous_userprofile,
            previous_data_dir,
        }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        unsafe {
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
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn normalize_test_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_start_matches("//?/")
        .to_string()
}

fn assert_path_eq(actual: impl AsRef<Path>, expected: impl AsRef<Path>) {
    assert_eq!(
        normalize_test_path(actual.as_ref()),
        normalize_test_path(expected.as_ref())
    );
}

fn test_home(dir: &TempDir) -> PathBuf {
    let home = dir.path().join("home");
    fs::create_dir_all(&home).unwrap();
    canonical_temp_path(&home)
}

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|err| panic!("failed to run git {args:?}: {err}"));
    assert!(
        output.status.success(),
        "git {args:?} failed in {}\nstdout:\n{}\nstderr:\n{}",
        cwd.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_repo_with_commit(project: &Path) {
    git(project, &["init", "-b", "main"]);
    git(project, &["config", "user.email", "test@example.com"]);
    git(project, &["config", "user.name", "TraceDecay Test"]);
    git(project, &["add", "."]);
    git(project, &["commit", "-m", "initial"]);
}

fn remove_sqlite_family(path: &Path) {
    let _ = fs::remove_file(path);
    let _ = fs::remove_file(path.with_extension("db-wal"));
    let _ = fs::remove_file(path.with_extension("db-shm"));
}

fn fact_request(content: &str) -> AddFactRequest {
    AddFactRequest {
        content: content.to_string(),
        category: MemoryCategory::Project,
        source: Some("legacy-store-adoption-test".to_string()),
        tags: vec!["migration-sentinel".to_string()],
        entities: Vec::new(),
        trust: Some(0.9),
        metadata: serde_json::json!({}),
    }
}

fn relocate_store_as_legacy(
    current_root: &Path,
    legacy_root: &Path,
    project_root: &Path,
    legacy_project_id: &str,
) {
    fs::rename(current_root, legacy_root).unwrap();
    let manifest_path = legacy_root.join(STORE_MANIFEST_FILENAME);
    let mut manifest = read_store_manifest(&manifest_path).unwrap();
    manifest.project_id = Some(legacy_project_id.to_string());
    manifest.project_root = project_root.to_path_buf();
    manifest.data_root = legacy_root.to_path_buf();
    write_store_manifest_to_path(&manifest_path, &manifest).unwrap();
}

async fn initialize_empty_profile_layout(layout: &tracedecay::storage::StoreLayout) {
    save_config_to_path(
        &layout.config_path,
        &TraceDecayConfig {
            root_dir: layout.project_root.to_string_lossy().to_string(),
            ..TraceDecayConfig::default()
        },
    )
    .unwrap();
    let (db, _) = crate::common::initialize_test_database(&layout.graph_db_path)
        .await
        .unwrap();
    db.checkpoint().await.unwrap();
    db.close();
    write_store_manifest(layout).unwrap();
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
fn store_manifest_write_rejects_symlinked_atomic_temp_path() {
    let dir = TempDir::new().unwrap();
    let temp_root = canonical_temp_path(dir.path());
    let project = temp_root.join("repo");
    let profile = temp_root.join("profile");
    let outside = temp_root.join("outside.tmp");
    fs::create_dir_all(&project).unwrap();
    fs::write(&outside, b"outside").unwrap();
    write_enrollment(&project);
    let marker = read_enrollment_marker(&project).unwrap().unwrap();
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
    write_enrollment(&project);
    let marker = read_enrollment_marker(&project).unwrap().unwrap();
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

    assert_path_eq(
        resolve_project_session_db_path(&project).unwrap(),
        profile_root.join("projects/proj_123/sessions.db"),
    );
    assert_path_eq(
        resolve_response_handle_root(&project).unwrap(),
        profile_root.join("projects/proj_123/response-handles"),
    );
    assert_path_eq(
        resolve_lcm_payload_root(&project).unwrap(),
        profile_root.join("projects/proj_123/lcm-payloads"),
    );
    assert_path_eq(
        project_session_db_path(&project),
        profile_root.join("projects/proj_123/sessions.db"),
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

    assert_path_eq(
        resolve_project_session_db_path(&project).unwrap(),
        profile_root.join(format!("projects/{project_id}/sessions.db")),
    );
    assert_path_eq(
        project_session_db_path(&project),
        profile_root.join(format!("projects/{project_id}/sessions.db")),
    );
}

#[tokio::test]
async fn hermes_profile_like_directory_uses_user_profile_shard() {
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
    let project_id = default_profile_project_id(&hermes_home);

    let expected = home
        .join(".tracedecay")
        .join(format!("projects/{project_id}/sessions.db"));
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
    assert_path_eq(&cg.store_layout().data_root, &shard_root);
    assert_path_eq(cg.db_path(), shard_root.join("tracedecay.db"));
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
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn registered() {}\n").unwrap();
    let _home_guard = HomeGuard::set(&home);
    init_repo_with_commit(&project);
    let project_id = default_profile_project_id(&project);

    TraceDecay::init(&project).await.unwrap();
    let db = GlobalDb::open().await.unwrap();
    let resolution = db.resolve_project_store_by_alias(&project).await.unwrap();

    assert_eq!(resolution.project.project_id, project_id);
    let identity_path = tracedecay::worktree::git_common_dir(&project)
        .unwrap()
        .join("tracedecay-project.json");
    let identity: Value = serde_json::from_slice(&fs::read(&identity_path).unwrap()).unwrap();
    assert_eq!(identity["schema_version"], 1);
    assert_eq!(identity["project_id"], project_id);

    fs::remove_file(&identity_path).unwrap();
    TraceDecay::open(&project).await.unwrap();
    assert!(
        identity_path.is_file(),
        "opening a legacy registered checkout must migrate it to durable repository identity"
    );
}

#[tokio::test]
async fn legacy_profile_store_upgrade_preserves_data_across_repo_identity_changes() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let remote = dir.path().join("remote.git");
    let project = dir.path().join("repo");
    let moved = dir.path().join("repo-moved");
    let linked = dir.path().join("repo-linked");
    let clone = dir.path().join("repo-clone");
    let home = test_home(&dir);
    let profile_root = home.join(".tracedecay");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn legacy_sentinel() {}\n").unwrap();
    let _home_guard = HomeGuard::set(&home);
    init_repo_with_commit(&project);
    git(dir.path(), &["init", "--bare", remote.to_str().unwrap()]);
    git(
        &project,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&project, &["push", "-u", "origin", "main"]);
    git(&remote, &["symbolic-ref", "HEAD", "refs/heads/main"]);

    let cg = TraceDecay::init(&project).await.unwrap();
    cg.index_all().await.unwrap();
    let main_fact_id = cg
        .add_fact(fact_request("legacy main fact sentinel"))
        .await
        .unwrap()
        .fact
        .unwrap()
        .fact_id;
    let current_root = cg.store_layout().data_root.clone();
    let current_project_id = cg.store_layout().identity.project_id.clone().unwrap();

    cg.checkpoint().await.unwrap();
    cg.close();
    git(&project, &["checkout", "-b", "feature/legacy-sentinel"]);
    let branch = TraceDecay::open(&project).await.unwrap();
    let branch_fact_id = branch
        .add_fact(fact_request("legacy branch fact sentinel"))
        .await
        .unwrap()
        .fact
        .unwrap()
        .fact_id;
    branch.checkpoint().await.unwrap();
    branch.close();
    git(&project, &["checkout", "main"]);

    let sessions = GlobalDb::open_at(&current_root.join("sessions.db"))
        .await
        .unwrap();
    assert!(
        sessions
            .upsert_session(&SessionRecord {
                provider: "codex".to_string(),
                session_id: "legacy-session-sentinel".to_string(),
                project_key: current_project_id,
                project_path: project.to_string_lossy().to_string(),
                title: Some("legacy session sentinel".to_string()),
                started_at: Some(1_800_000_001),
                ended_at: Some(1_800_000_002),
                transcript_path: None,
                metadata_json: None,
                parent_session_id: None,
                is_subagent: false,
                agent_id: None,
                parent_tool_use_id: None,
            })
            .await
    );
    sessions.checkpoint().await;
    sessions.close();

    let automation_sentinel = current_root.join("automation/migration-sentinel.json");
    fs::create_dir_all(automation_sentinel.parent().unwrap()).unwrap();
    fs::write(&automation_sentinel, br#"{"preserved":true}"#).unwrap();

    fs::remove_file(repository_identity_path(&project).unwrap()).unwrap();
    remove_sqlite_family(&profile_root.join("global.db"));

    let legacy_project_id = "proj_legacy_path_hash";
    let legacy_root = profile_root.join(format!("projects/{legacy_project_id}"));
    relocate_store_as_legacy(&current_root, &legacy_root, &project, legacy_project_id);

    let adopted = TraceDecay::open(&project)
        .await
        .expect("upgrade must adopt the manifest-backed legacy store");
    assert_path_eq(&adopted.store_layout().data_root, &legacy_root);
    assert_eq!(
        adopted
            .get_fact(main_fact_id)
            .await
            .unwrap()
            .unwrap()
            .content,
        "legacy main fact sentinel"
    );
    assert_eq!(
        fs::read_to_string(legacy_root.join("automation/migration-sentinel.json")).unwrap(),
        r#"{"preserved":true}"#
    );

    let sessions = GlobalDb::open_at(&legacy_root.join("sessions.db"))
        .await
        .unwrap();
    assert_eq!(
        sessions
            .get_session("codex", "legacy-session-sentinel")
            .await
            .unwrap()
            .title
            .as_deref(),
        Some("legacy session sentinel")
    );
    sessions.close();

    let branch = TraceDecay::open_branch(&project, "feature/legacy-sentinel")
        .await
        .unwrap();
    assert_eq!(
        branch
            .get_fact(branch_fact_id)
            .await
            .unwrap()
            .unwrap()
            .content,
        "legacy branch fact sentinel"
    );
    branch.close();

    let marker = read_repository_identity_marker(&project)
        .unwrap()
        .expect("successful adoption must persist repository identity");
    assert_eq!(marker.project_id, legacy_project_id);
    adopted.checkpoint().await.unwrap();
    adopted.close();

    fs::rename(&project, &moved).unwrap();
    let reopened = TraceDecay::open(&moved).await.unwrap();
    assert_path_eq(&reopened.store_layout().data_root, &legacy_root);
    reopened.close();

    #[cfg(unix)]
    {
        let alias = dir.path().join("repo-alias");
        symlink(&moved, &alias).unwrap();
        let via_alias = TraceDecay::open(&alias).await.unwrap();
        assert_path_eq(&via_alias.store_layout().data_root, &legacy_root);
        via_alias.close();
    }

    git(
        &moved,
        &[
            "worktree",
            "add",
            "-b",
            "feature/adopted-linked",
            linked.to_str().unwrap(),
        ],
    );
    let linked_graph = TraceDecay::open(&linked).await.unwrap();
    assert_path_eq(&linked_graph.store_layout().data_root, &legacy_root);
    linked_graph.close();

    git(
        dir.path(),
        &["clone", remote.to_str().unwrap(), clone.to_str().unwrap()],
    );
    assert!(
        !TraceDecay::has_initialized_store(&clone).await,
        "same-remote clones must not adopt another checkout's orphan manifest"
    );
    let clone_graph = TraceDecay::init(&clone).await.unwrap();
    assert_ne!(
        normalize_test_path(&clone_graph.store_layout().data_root),
        normalize_test_path(&legacy_root),
        "a separate clone must mint its own store identity"
    );
    clone_graph.close();
}

#[tokio::test]
async fn empty_cutover_store_is_atomically_replaced_by_healthy_legacy_store() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let home = test_home(&dir);
    let profile_root = home.join(".tracedecay");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn cutover() {}\n").unwrap();
    let _home_guard = HomeGuard::set(&home);
    init_repo_with_commit(&project);

    let old = TraceDecay::init(&project).await.unwrap();
    let fact_id = old
        .add_fact(fact_request("healthy legacy cutover fact"))
        .await
        .unwrap()
        .fact
        .unwrap()
        .fact_id;
    let original_root = old.store_layout().data_root.clone();
    old.checkpoint().await.unwrap();
    old.close();
    fs::remove_file(repository_identity_path(&project).unwrap()).unwrap();
    remove_sqlite_family(&profile_root.join("global.db"));

    let legacy_project_id = "proj_healthy_legacy";
    let legacy_root = profile_root.join(format!("projects/{legacy_project_id}"));
    relocate_store_as_legacy(&original_root, &legacy_root, &project, legacy_project_id);

    let cutover = default_profile_sharded_layout(&project, &profile_root).unwrap();
    let cutover_project_id = cutover.identity.project_id.clone().unwrap();
    initialize_empty_profile_layout(&cutover).await;
    write_repository_identity_marker(&project, &cutover_project_id).unwrap();

    let repaired = TraceDecay::open(&project)
        .await
        .expect("an empty cutover shard may safely yield to the healthy legacy shard");
    assert_path_eq(&repaired.store_layout().data_root, &legacy_root);
    assert_eq!(
        repaired.get_fact(fact_id).await.unwrap().unwrap().content,
        "healthy legacy cutover fact"
    );
    assert_eq!(
        read_repository_identity_marker(&project)
            .unwrap()
            .unwrap()
            .project_id,
        legacy_project_id
    );
    assert!(
        cutover.graph_db_path.is_file(),
        "empty shard stays as a backup"
    );
    assert!(
        cutover
            .data_root
            .join("store_manifest.identity-cutover-backup.json")
            .is_file(),
        "the retired empty shard must remain discoverable as an explicit backup"
    );
    assert!(!cutover.data_root.join(STORE_MANIFEST_FILENAME).exists());
    repaired.close();
}

#[tokio::test]
async fn empty_cutover_store_adopts_healthy_legacy_linked_worktree_store() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let linked = dir.path().join("repo-linked");
    let home = test_home(&dir);
    let profile_root = home.join(".tracedecay");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn linked_cutover() {}\n").unwrap();
    let _home_guard = HomeGuard::set(&home);
    init_repo_with_commit(&project);
    git(
        &project,
        &[
            "worktree",
            "add",
            "-b",
            "feature/legacy-linked-cutover",
            linked.to_str().unwrap(),
        ],
    );

    let old = TraceDecay::init(&project).await.unwrap();
    let fact_id = old
        .add_fact(fact_request("healthy linked legacy cutover fact"))
        .await
        .unwrap()
        .fact
        .unwrap()
        .fact_id;
    let original_root = old.store_layout().data_root.clone();
    old.checkpoint().await.unwrap();
    old.close();
    fs::remove_file(repository_identity_path(&project).unwrap()).unwrap();
    remove_sqlite_family(&profile_root.join("global.db"));

    let legacy_project_id = "proj_healthy_linked_legacy";
    let legacy_root = profile_root.join(format!("projects/{legacy_project_id}"));
    relocate_store_as_legacy(&original_root, &legacy_root, &linked, legacy_project_id);

    let cutover = default_profile_sharded_layout(&project, &profile_root).unwrap();
    let cutover_project_id = cutover.identity.project_id.clone().unwrap();
    initialize_empty_profile_layout(&cutover).await;
    write_repository_identity_marker(&project, &cutover_project_id).unwrap();

    let repaired = TraceDecay::open(&project)
        .await
        .expect("a linked worktree manifest with the same git common dir must be adopted");
    assert_path_eq(&repaired.store_layout().data_root, &legacy_root);
    assert_eq!(
        repaired.get_fact(fact_id).await.unwrap().unwrap().content,
        "healthy linked legacy cutover fact"
    );
    repaired.close();
}

#[tokio::test]
async fn corrupt_nonempty_cutover_store_reports_both_shards_without_switching() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let home = test_home(&dir);
    let profile_root = home.join(".tracedecay");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn split() {}\n").unwrap();
    let _home_guard = HomeGuard::set(&home);
    init_repo_with_commit(&project);

    let old = TraceDecay::init(&project).await.unwrap();
    old.add_fact(fact_request("legacy split identity fact"))
        .await
        .unwrap();
    let original_root = old.store_layout().data_root.clone();
    old.checkpoint().await.unwrap();
    old.close();
    fs::remove_file(repository_identity_path(&project).unwrap()).unwrap();
    remove_sqlite_family(&profile_root.join("global.db"));

    let legacy_project_id = "proj_split_legacy";
    let legacy_root = profile_root.join(format!("projects/{legacy_project_id}"));
    relocate_store_as_legacy(&original_root, &legacy_root, &project, legacy_project_id);

    let cutover = default_profile_sharded_layout(&project, &profile_root).unwrap();
    let cutover_project_id = cutover.identity.project_id.clone().unwrap();
    initialize_empty_profile_layout(&cutover).await;
    fs::write(&cutover.graph_db_path, b"not a sqlite database").unwrap();
    let sessions = GlobalDb::open_at(&cutover.sessions_db_path).await.unwrap();
    assert!(
        sessions
            .upsert_session(&SessionRecord {
                provider: "codex".to_string(),
                session_id: "new-cutover-session".to_string(),
                project_key: cutover_project_id.clone(),
                project_path: project.to_string_lossy().to_string(),
                title: Some("new cutover session".to_string()),
                started_at: Some(1_800_000_010),
                ended_at: None,
                transcript_path: None,
                metadata_json: None,
                parent_session_id: None,
                is_subagent: false,
                agent_id: None,
                parent_tool_use_id: None,
            })
            .await
    );
    sessions.checkpoint().await;
    sessions.close();
    write_repository_identity_marker(&project, &cutover_project_id).unwrap();

    let error = TraceDecay::resolve_store_layout_for_identity(&project)
        .await
        .expect_err("nonempty split stores require explicit consolidation");
    let message = error.to_string();
    assert!(message.contains("identity cutover conflict"), "{message}");
    assert!(message.contains(&cutover_project_id), "{message}");
    assert!(message.contains(legacy_project_id), "{message}");
    assert!(message.contains("graph_health=corrupt"), "{message}");
    assert!(message.contains("sessions=1"), "{message}");
    assert!(message.contains("facts=1"), "{message}");
    assert!(message.contains("no files changed"), "{message}");
    assert!(
        message.contains(&format!(
            "tracedecay migrate consolidate --project '{}' --source-project-id {legacy_project_id} --target-project-id {cutover_project_id}",
            project.display()
        )),
        "{message}"
    );
    assert_eq!(
        read_repository_identity_marker(&project)
            .unwrap()
            .unwrap()
            .project_id,
        cutover_project_id
    );
    assert!(cutover.data_root.join(STORE_MANIFEST_FILENAME).is_file());
    assert!(legacy_root.join(STORE_MANIFEST_FILENAME).is_file());
}

#[tokio::test]
async fn ambiguous_legacy_store_adoption_preserves_every_candidate() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let profile_root = dir.path().join("profile");
    let global_db_path = profile_root.join("global.db");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn conflict() {}\n").unwrap();
    init_repo_with_commit(&project);

    for project_id in ["proj_legacy_one", "proj_legacy_two"] {
        let data_root = profile_root.join(format!("projects/{project_id}"));
        fs::create_dir_all(&data_root).unwrap();
        fs::write(data_root.join("tracedecay.db"), project_id).unwrap();
        fs::write(data_root.join("sessions.db"), b"sessions").unwrap();
        branch_meta::save_branch_meta(&data_root, &BranchMeta::new_for_dir(&data_root, "main"))
            .unwrap();
        write_store_manifest_to_path(
            &data_root.join(STORE_MANIFEST_FILENAME),
            &StoreManifest {
                schema_version: STORE_MANIFEST_SCHEMA_VERSION,
                project_id: Some(project_id.to_string()),
                store_kind: StoreKind::CodeProject,
                storage_mode: StorageMode::ProfileSharded,
                project_root: project.clone(),
                data_root,
                graph_db_relpath: "tracedecay.db".into(),
                sessions_db_relpath: "sessions.db".into(),
                branch_meta_relpath: "branch-meta.json".into(),
            },
        )
        .unwrap();
    }

    let error = TraceDecay::resolve_store_layout_for_identity_with_options(
        &project,
        &TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: Some(global_db_path),
        },
    )
    .await
    .expect_err("ambiguous legacy manifests must not be selected implicitly");

    assert!(
        error
            .to_string()
            .contains("ambiguous legacy profile stores")
    );
    for project_id in ["proj_legacy_one", "proj_legacy_two"] {
        assert_eq!(
            fs::read_to_string(profile_root.join(format!("projects/{project_id}/tracedecay.db")))
                .unwrap(),
            project_id,
            "conflict handling must retain every candidate as a recoverable backup"
        );
    }
    assert!(!repository_identity_path(&project).unwrap().exists());
}

#[tokio::test]
async fn worktree_profile_stores_prefer_the_exact_manifest_root() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let worktree = dir.path().join("repo-wt");
    let profile_root = dir.path().join("profile");
    let global_db_path = profile_root.join("global.db");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn main_root() {}\n").unwrap();
    init_repo_with_commit(&project);
    git(
        &project,
        &[
            "worktree",
            "add",
            "-b",
            "feature/exact-manifest-root",
            worktree.to_str().unwrap(),
        ],
    );

    for (project_id, manifest_root) in [
        ("proj_main_worktree", project.as_path()),
        ("proj_linked_worktree", worktree.as_path()),
    ] {
        let data_root = profile_root.join(format!("projects/{project_id}"));
        fs::create_dir_all(&data_root).unwrap();
        fs::write(data_root.join("tracedecay.db"), project_id).unwrap();
        fs::write(data_root.join("sessions.db"), b"sessions").unwrap();
        branch_meta::save_branch_meta(&data_root, &BranchMeta::new_for_dir(&data_root, "main"))
            .unwrap();
        write_store_manifest_to_path(
            &data_root.join(STORE_MANIFEST_FILENAME),
            &StoreManifest {
                schema_version: STORE_MANIFEST_SCHEMA_VERSION,
                project_id: Some(project_id.to_string()),
                store_kind: StoreKind::CodeProject,
                storage_mode: StorageMode::ProfileSharded,
                project_root: manifest_root.to_path_buf(),
                data_root,
                graph_db_relpath: "tracedecay.db".into(),
                sessions_db_relpath: "sessions.db".into(),
                branch_meta_relpath: "branch-meta.json".into(),
            },
        )
        .unwrap();
    }

    for (root, expected_project_id) in [
        (project.as_path(), "proj_main_worktree"),
        (worktree.as_path(), "proj_linked_worktree"),
    ] {
        let layout = TraceDecay::resolve_store_layout_for_identity_with_options(
            root,
            &TraceDecayOpenOptions {
                profile_root: Some(profile_root.clone()),
                global_db_path: Some(global_db_path.clone()),
            },
        )
        .await
        .expect("the manifest whose project_root exactly matches must win");

        assert_eq!(
            layout.identity.project_id.as_deref(),
            Some(expected_project_id)
        );
    }
}

#[tokio::test]
async fn linked_worktree_exact_manifest_overrides_healthy_shared_identity_store() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let worktree = dir.path().join("repo-wt");
    let candidate_source = dir.path().join("candidate-source");
    let home = test_home(&dir);
    let profile_root = home.join(".tracedecay");
    let _home_guard = HomeGuard::set(&home);

    for root in [&project, &candidate_source] {
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn indexed() {}\n").unwrap();
        init_repo_with_commit(root);
    }

    let main = TraceDecay::init(&project).await.unwrap();
    main.index_all().await.unwrap();
    let main_project_id = main.store_layout().identity.project_id.clone().unwrap();
    main.close();

    git(
        &project,
        &[
            "worktree",
            "add",
            "-b",
            "feature/exact-over-shared",
            worktree.to_str().unwrap(),
        ],
    );

    let candidate = TraceDecay::init(&candidate_source).await.unwrap();
    candidate.index_all().await.unwrap();
    let candidate_root = candidate.store_layout().data_root.clone();
    candidate.close();

    let exact_project_id = "proj_linked_exact_over_shared";
    let exact_root = profile_root.join(format!("projects/{exact_project_id}"));
    relocate_store_as_legacy(&candidate_root, &exact_root, &worktree, exact_project_id);
    assert_eq!(
        read_repository_identity_marker(&worktree)
            .unwrap()
            .unwrap()
            .project_id,
        main_project_id
    );

    let layout = TraceDecay::resolve_store_layout_for_identity_with_options(
        &worktree,
        &TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: Some(profile_root.join("global.db")),
        },
    )
    .await
    .expect("the healthy exact-root worktree shard must override the healthy shared shard");

    assert_ne!(
        layout.identity.project_id.as_deref(),
        Some(main_project_id.as_str())
    );
    assert_eq!(
        layout.identity.project_id.as_deref(),
        Some(exact_project_id)
    );
    assert_path_eq(&layout.data_root, &exact_root);
}

#[tokio::test]
async fn registered_healthy_exact_branch_ignores_duplicate_exact_manifests() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let home = test_home(&dir);
    let profile_root = home.join(".tracedecay");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn registered_root() {}\n").unwrap();
    let _home_guard = HomeGuard::set(&home);
    init_repo_with_commit(&project);

    let selected = TraceDecay::init(&project).await.unwrap();
    selected.index_all().await.unwrap();
    let selected_project_id = selected.store_layout().identity.project_id.clone().unwrap();
    let selected_data_root = selected.store_layout().data_root.clone();
    selected.close();

    git(
        &project,
        &["checkout", "-b", "feature/selected-exact-branch"],
    );
    let selected_branch = TraceDecay::open(&project)
        .await
        .expect("the selected store must create a healthy current-branch graph");
    selected_branch.close();
    remove_sqlite_family(&selected_data_root.join("tracedecay.db"));
    fs::write(
        selected_data_root.join("tracedecay.db"),
        b"root graph is unavailable; current branch graph remains healthy",
    )
    .unwrap();

    for project_id in ["proj_duplicate_exact_one", "proj_duplicate_exact_two"] {
        let data_root = profile_root.join(format!("projects/{project_id}"));
        fs::create_dir_all(&data_root).unwrap();
        fs::write(data_root.join("tracedecay.db"), project_id).unwrap();
        fs::write(data_root.join("sessions.db"), b"sessions").unwrap();
        branch_meta::save_branch_meta(&data_root, &BranchMeta::new_for_dir(&data_root, "main"))
            .unwrap();
        write_store_manifest_to_path(
            &data_root.join(STORE_MANIFEST_FILENAME),
            &StoreManifest {
                schema_version: STORE_MANIFEST_SCHEMA_VERSION,
                project_id: Some(project_id.to_string()),
                store_kind: StoreKind::CodeProject,
                storage_mode: StorageMode::ProfileSharded,
                project_root: project.clone(),
                data_root,
                graph_db_relpath: "tracedecay.db".into(),
                sessions_db_relpath: "sessions.db".into(),
                branch_meta_relpath: "branch-meta.json".into(),
            },
        )
        .unwrap();
    }

    let layout = TraceDecay::resolve_store_layout_for_identity(&project)
        .await
        .expect("a healthy selected exact-branch shard must outrank duplicate legacy manifests");

    assert_eq!(
        layout.identity.project_id.as_deref(),
        Some(selected_project_id.as_str())
    );
    assert_path_eq(&layout.data_root, &selected_data_root);
    for project_id in ["proj_duplicate_exact_one", "proj_duplicate_exact_two"] {
        assert_eq!(
            fs::read_to_string(profile_root.join(format!("projects/{project_id}/tracedecay.db")))
                .unwrap(),
            project_id,
            "duplicate stores must remain untouched as recoverable history"
        );
    }
}

#[tokio::test]
async fn registered_exact_root_ignores_sibling_worktree_manifests() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let first_worktree = dir.path().join("repo-wt-one");
    let second_worktree = dir.path().join("repo-wt-two");
    let home = test_home(&dir);
    let profile_root = home.join(".tracedecay");
    let global_db_path = profile_root.join("global.db");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn registered_root() {}\n").unwrap();
    let _home_guard = HomeGuard::set(&home);
    init_repo_with_commit(&project);

    let main = TraceDecay::init(&project).await.unwrap();
    main.index_all().await.unwrap();
    let main_project_id = main.store_layout().identity.project_id.clone().unwrap();
    let main_data_root = main.store_layout().data_root.clone();
    main.close();

    for (branch, worktree) in [
        ("feature/registered-sibling-one", first_worktree.as_path()),
        ("feature/registered-sibling-two", second_worktree.as_path()),
    ] {
        git(
            &project,
            &["worktree", "add", "-b", branch, worktree.to_str().unwrap()],
        );
    }

    for (project_id, manifest_root) in [
        ("proj_registered_sibling_one", first_worktree.as_path()),
        ("proj_registered_sibling_two", second_worktree.as_path()),
    ] {
        let data_root = profile_root.join(format!("projects/{project_id}"));
        fs::create_dir_all(&data_root).unwrap();
        fs::write(data_root.join("tracedecay.db"), project_id).unwrap();
        fs::write(data_root.join("sessions.db"), b"sessions").unwrap();
        branch_meta::save_branch_meta(&data_root, &BranchMeta::new_for_dir(&data_root, "main"))
            .unwrap();
        write_store_manifest_to_path(
            &data_root.join(STORE_MANIFEST_FILENAME),
            &StoreManifest {
                schema_version: STORE_MANIFEST_SCHEMA_VERSION,
                project_id: Some(project_id.to_string()),
                store_kind: StoreKind::CodeProject,
                storage_mode: StorageMode::ProfileSharded,
                project_root: manifest_root.to_path_buf(),
                data_root,
                graph_db_relpath: "tracedecay.db".into(),
                sessions_db_relpath: "sessions.db".into(),
                branch_meta_relpath: "branch-meta.json".into(),
            },
        )
        .unwrap();
    }

    let git_common_dir = tracedecay::worktree::git_common_dir(&project).unwrap();
    let global_db = GlobalDb::open().await.unwrap();
    let registered = global_db
        .resolve_project_store_by_identity(&project, Some(&git_common_dir))
        .await
        .expect("the exact main checkout must resolve through GlobalDb");
    assert_eq!(registered.project.project_id, main_project_id);

    fs::remove_file(repository_identity_path(&project).unwrap()).unwrap();
    let layout = TraceDecay::resolve_store_layout_for_identity_with_options(
        &project,
        &TraceDecayOpenOptions {
            profile_root: Some(profile_root),
            global_db_path: Some(global_db_path),
        },
    )
    .await
    .expect("a registered exact-root shard must ignore sibling worktree manifests");

    assert_eq!(
        layout.identity.project_id.as_deref(),
        Some(main_project_id.as_str())
    );
    assert_path_eq(&layout.data_root, &main_data_root);
}

#[tokio::test]
async fn linked_worktree_uses_initialized_git_common_dir_store_without_init() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let worktree = dir.path().join("repo-wt");
    let home = test_home(&dir);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn main_only() {}\n").unwrap();
    let _home_guard = HomeGuard::set(&home);

    init_repo_with_commit(&project);

    let main = TraceDecay::init(&project).await.unwrap();
    main.index_all().await.unwrap();
    let main_store = main.store_layout().data_root.clone();
    drop(main);

    git(
        &project,
        &[
            "worktree",
            "add",
            "-b",
            "feature/worktree-auto",
            worktree.to_str().unwrap(),
        ],
    );
    fs::write(
        worktree.join("src/lib.rs"),
        "pub fn main_only() {}\npub fn worktree_only() {}\n",
    )
    .unwrap();

    assert_eq!(
        discover_project_root(&worktree.join("src")),
        None,
        "discovery must not walk from a linked worktree into the main checkout"
    );
    assert!(
        TraceDecay::has_initialized_store(&worktree).await,
        "linked worktree should resolve the already-initialized shared git store"
    );

    let worktree_cg = TraceDecay::open(&worktree).await.unwrap();
    assert_eq!(worktree_cg.project_root(), worktree.as_path());
    assert_eq!(worktree_cg.store_layout().data_root, main_store);
    assert_eq!(
        resolved_project_session_db_path(&worktree).await.unwrap(),
        worktree_cg.store_layout().sessions_db_path,
        "session storage should follow the shared git-common-dir store too"
    );
    assert!(
        !worktree_cg
            .search("worktree_only", 10)
            .await
            .unwrap()
            .is_empty(),
        "opening a linked worktree should auto-track and sync its branch DB"
    );
    assert!(
        !worktree.join(".tracedecay").exists(),
        "automatic worktree support must not require or create a per-worktree marker"
    );

    let meta = branch_meta::load_branch_meta(&main_store).unwrap();
    assert!(
        meta.is_tracked("feature/worktree-auto"),
        "linked worktree branch should be tracked in the shared store"
    );
}

#[tokio::test]
async fn same_remote_clone_is_not_considered_initialized_without_local_identity() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let remote = dir.path().join("remote.git");
    let project = dir.path().join("repo");
    let clone = dir.path().join("repo-clone");
    let home = test_home(&dir);
    let _home_guard = HomeGuard::set(&home);

    git(dir.path(), &["init", "--bare", remote.to_str().unwrap()]);
    git(
        dir.path(),
        &["clone", remote.to_str().unwrap(), project.to_str().unwrap()],
    );
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn main_only() {}\n").unwrap();
    git(&project, &["config", "user.email", "test@example.com"]);
    git(&project, &["config", "user.name", "TraceDecay Test"]);
    git(&project, &["add", "."]);
    git(&project, &["commit", "-m", "initial"]);
    git(&project, &["push", "origin", "HEAD:master"]);
    git(
        dir.path(),
        &["clone", remote.to_str().unwrap(), clone.to_str().unwrap()],
    );

    TraceDecay::init(&project).await.unwrap();

    assert!(
        !TraceDecay::has_initialized_store(&clone).await,
        "a separate clone with the same origin is not a linked worktree and must not borrow the initialized store"
    );
    assert_eq!(
        resolved_project_session_db_path(&clone).await.unwrap(),
        project_session_db_path(&clone),
        "session storage must not use a same-remote clone as repository identity"
    );

    let original_identity = tracedecay::worktree::git_common_dir(&project)
        .unwrap()
        .join("tracedecay-project.json");
    let copied_identity = tracedecay::worktree::git_common_dir(&clone)
        .unwrap()
        .join("tracedecay-project.json");
    fs::copy(original_identity, copied_identity).unwrap();
    let error = match TraceDecay::open(&clone).await {
        Ok(_) => panic!("a copied repository marker must not bind a second live clone"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("repository identity conflict"),
        "unexpected copied-marker error: {error}"
    );
}

#[tokio::test]
async fn renamed_checkout_session_db_follows_registered_store() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let remote = dir.path().join("remote.git");
    let original = dir.path().join("repo");
    let renamed = dir.path().join("repo-renamed");
    let home = test_home(&dir);
    let _home_guard = HomeGuard::set(&home);

    git(dir.path(), &["init", "--bare", remote.to_str().unwrap()]);
    git(
        dir.path(),
        &[
            "clone",
            remote.to_str().unwrap(),
            original.to_str().unwrap(),
        ],
    );
    fs::create_dir_all(original.join("src")).unwrap();
    fs::write(original.join("src/lib.rs"), "pub fn main_only() {}\n").unwrap();
    git(&original, &["config", "user.email", "test@example.com"]);
    git(&original, &["config", "user.name", "TraceDecay Test"]);
    git(&original, &["add", "."]);
    git(&original, &["commit", "-m", "initial"]);
    git(&original, &["push", "origin", "HEAD:master"]);

    let cg = TraceDecay::init(&original).await.unwrap();
    let registered_session_db = cg.store_layout().sessions_db_path.clone();
    drop(cg);

    // Move the whole checkout on disk; both its canonical root and git common
    // dir change, so registry identity resolution can no longer match by path.
    fs::rename(&original, &renamed).unwrap();
    git(&renamed, &["remote", "remove", "origin"]);

    let resolved = resolved_project_session_db_path(&renamed)
        .await
        .expect("renamed checkout should resolve a session DB path");
    assert_path_eq(&resolved, &registered_session_db);
    assert_ne!(
        normalize_test_path(&resolved),
        normalize_test_path(&project_session_db_path(&renamed)),
        "renamed checkout must not fork a fresh default-path session DB",
    );

    #[cfg(unix)]
    {
        let alias = dir.path().join("repo-alias");
        symlink(&renamed, &alias).unwrap();
        let via_alias = resolved_project_session_db_path(&alias)
            .await
            .expect("symlink alias should retain repository identity");
        assert_path_eq(via_alias, registered_session_db);
    }
}

#[tokio::test]
async fn parent_index_excludes_nested_linked_worktree_sources() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let nested_worktree = project.join(".worktrees/feature");
    let home = test_home(&dir);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn parent_only() {}\n").unwrap();
    let _home_guard = HomeGuard::set(&home);
    init_repo_with_commit(&project);

    git(
        &project,
        &[
            "worktree",
            "add",
            "-b",
            "feature/nested-index",
            nested_worktree.to_str().unwrap(),
        ],
    );
    fs::write(
        nested_worktree.join("src/lib.rs"),
        "pub fn parent_only() {}\npub fn nested_worktree_only() {}\n",
    )
    .unwrap();

    let mut parent = TraceDecay::init(&project).await.unwrap();
    parent.add_include_folders(&[".worktrees".to_string()]);
    parent.index_all().await.unwrap();

    assert!(
        !parent.search("parent_only", 10).await.unwrap().is_empty(),
        "the parent checkout must remain indexed"
    );
    assert!(
        parent
            .search("nested_worktree_only", 10)
            .await
            .unwrap()
            .is_empty(),
        "a nested linked worktree must be a separate project view, not duplicate parent source"
    );
}

#[tokio::test]
async fn same_remote_clone_session_db_does_not_borrow_registered_store() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let remote = dir.path().join("remote.git");
    let project = dir.path().join("repo");
    let clone = dir.path().join("repo-clone");
    let home = test_home(&dir);
    let _home_guard = HomeGuard::set(&home);

    git(dir.path(), &["init", "--bare", remote.to_str().unwrap()]);
    git(
        dir.path(),
        &["clone", remote.to_str().unwrap(), project.to_str().unwrap()],
    );
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn main_only() {}\n").unwrap();
    git(&project, &["config", "user.email", "test@example.com"]);
    git(&project, &["config", "user.name", "TraceDecay Test"]);
    git(&project, &["add", "."]);
    git(&project, &["commit", "-m", "initial"]);
    git(&project, &["push", "origin", "HEAD:master"]);
    git(
        dir.path(),
        &["clone", remote.to_str().unwrap(), clone.to_str().unwrap()],
    );

    let cg = TraceDecay::init(&project).await.unwrap();
    let registered_session_db = cg.store_layout().sessions_db_path.clone();
    drop(cg);

    // The original checkout still exists on disk, so the same-remote clone must
    // not inherit its registered session store even though the remote is unique
    // in the registry.
    let resolved = resolved_project_session_db_path(&clone)
        .await
        .expect("clone should still resolve a default session DB path");
    assert_ne!(
        normalize_test_path(&resolved),
        normalize_test_path(&registered_session_db),
        "a separate same-remote clone must not borrow another checkout's session store",
    );
    assert_path_eq(&resolved, project_session_db_path(&clone));
}

#[tokio::test]
async fn same_remote_repositories_keep_distinct_persistent_identities() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let remote = dir.path().join("remote.git");
    let one = dir.path().join("repo-one");
    let two = dir.path().join("repo-two");
    let renamed_one = dir.path().join("repo-one-renamed");
    let home = test_home(&dir);
    let _home_guard = HomeGuard::set(&home);

    git(dir.path(), &["init", "--bare", remote.to_str().unwrap()]);
    git(
        dir.path(),
        &["clone", remote.to_str().unwrap(), one.to_str().unwrap()],
    );
    fs::create_dir_all(one.join("src")).unwrap();
    fs::write(one.join("src/lib.rs"), "pub fn main_only() {}\n").unwrap();
    git(&one, &["config", "user.email", "test@example.com"]);
    git(&one, &["config", "user.name", "TraceDecay Test"]);
    git(&one, &["add", "."]);
    git(&one, &["commit", "-m", "initial"]);
    git(&one, &["push", "origin", "HEAD:master"]);
    git(
        dir.path(),
        &["clone", remote.to_str().unwrap(), two.to_str().unwrap()],
    );

    let one_session_db = TraceDecay::init(&one)
        .await
        .unwrap()
        .store_layout()
        .sessions_db_path
        .clone();
    TraceDecay::init(&two).await.unwrap();

    fs::rename(&one, &renamed_one).unwrap();

    let resolved = resolved_project_session_db_path(&renamed_one)
        .await
        .expect("moved checkout should resolve its persistent repository identity");
    assert_path_eq(&resolved, one_session_db);
    assert_ne!(
        normalize_test_path(&resolved),
        normalize_test_path(&project_session_db_path(&renamed_one)),
        "remote ambiguity must not fork the moved repository into a new path-hash store"
    );
}

#[tokio::test]
async fn nested_linked_worktree_does_not_discover_parent_checkout_marker() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let worktree = project.join(".worktrees/feature-nested");
    let home = test_home(&dir);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn main_only() {}\n").unwrap();
    let _home_guard = HomeGuard::set(&home);

    init_repo_with_commit(&project);
    TraceDecay::init(&project).await.unwrap();

    git(
        &project,
        &[
            "worktree",
            "add",
            "-b",
            "feature/nested",
            worktree.to_str().unwrap(),
        ],
    );

    assert_eq!(
        discover_project_root(&worktree.join("src")),
        None,
        "a linked worktree inside the main checkout must not inherit the parent marker"
    );
    assert!(
        TraceDecay::has_initialized_store(&worktree).await,
        "nested linked worktree should still find the shared git store"
    );
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
    crate::common::initialize_test_database(&shard_root.join("tracedecay.db"))
        .await
        .unwrap();
    let meta = BranchMeta::new_for_dir(&shard_root, "main");
    branch_meta::save_branch_meta(&shard_root, &meta).unwrap();

    let opened = TraceDecay::open(&project).await.unwrap();

    assert_path_eq(opened.db_path(), shard_root.join("tracedecay.db"));
    assert_eq!(opened.get_config().root_dir, project.to_string_lossy());
    assert_eq!(opened.serving_branch(), Some("main"));
}
