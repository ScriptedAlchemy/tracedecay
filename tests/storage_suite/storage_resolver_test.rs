use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use tempfile::TempDir;
use tracedecay::application::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay::branch_meta::{self, BranchMeta};
use tracedecay::config::{TraceDecayConfig, USER_DATA_DIR_ENV};
use tracedecay::config::{
    discover_project_root, get_config_path, load_config, save_config_to_path,
};
use tracedecay::global_db::{ProjectObservationStoreError, StoreInstanceUpsert};
use tracedecay::mcp::response_handles::{
    ResponseHandleLookup, retrieve_response_handle, store_response_handle,
};
use tracedecay::memory::types::{AddFactRequest, MemoryCategory};
use tracedecay::storage::{
    ActiveProjectContext, EnrollmentMarker, GraphScopeId, PrivateStoreIo, ProjectPath,
    STORE_MANIFEST_FILENAME, STORE_MANIFEST_SCHEMA_VERSION, StorageMode, StoreArtifactPath,
    StoreKind, StoreManifest, default_profile_project_id, default_profile_sharded_layout,
    enrollment_marker_path, profile_sharded_layout, read_enrollment_marker,
    read_repository_identity_marker, read_store_manifest, repository_identity_path, resolve_layout,
    resolve_lcm_payload_root, resolve_project_session_db_path, resolve_response_handle_root,
    write_enrollment_marker, write_repository_identity_marker, write_store_manifest,
    write_store_manifest_to_path,
};
use tracedecay::tracedecay::{TraceDecay, TraceDecayOpenOptions};
use tracedecay_domain::ProjectId;
use tracedecay_sessions::runtime::SessionRecord;

use crate::support::HOME_ENV_LOCK;

mod artifact_routing;
mod identity_resolution;
mod init_and_open;
mod layout_and_paths;
mod manifest;
mod marker_relocation;
mod markers;
mod worktrees_clones;

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

fn maintenance_profile_root() -> PathBuf {
    tracedecay::config::user_data_dir().expect("test profile root")
}

fn prepare_maintenance_profile(profile_root: &Path) {
    fs::create_dir_all(profile_root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(profile_root, fs::Permissions::from_mode(0o700)).unwrap();
    }
}

fn acquire_fixture_maintenance() -> tracedecay::lifecycle_lease::LifecycleLease {
    let profile_root = maintenance_profile_root();
    prepare_maintenance_profile(&profile_root);
    tracedecay::lifecycle_lease::acquire_exclusive_for_profile(
        &profile_root,
        "storage resolver fixture mutation",
    )
    .unwrap()
}

async fn init_with_maintenance(project_root: &Path) -> tracedecay::errors::Result<TraceDecay> {
    let profile_root = maintenance_profile_root();
    prepare_maintenance_profile(&profile_root);
    let lifecycle = tracedecay::lifecycle_lease::acquire_exclusive_for_profile(
        &profile_root,
        "storage resolver fixture initialization",
    )
    .unwrap();
    let _database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle,
        &profile_root,
        "storage resolver fixture initialization",
    )
    .unwrap();
    TraceDecay::init_with_exclusive_maintenance(
        project_root,
        TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: Some(profile_root.join("global.db")),
        },
        &lifecycle,
    )
    .await
}

async fn open_with_maintenance(project_root: &Path) -> tracedecay::errors::Result<TraceDecay> {
    let profile_root = maintenance_profile_root();
    prepare_maintenance_profile(&profile_root);
    let lifecycle = tracedecay::lifecycle_lease::acquire_exclusive_for_profile(
        &profile_root,
        "storage resolver fixture open",
    )
    .unwrap();
    let _database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle,
        &profile_root,
        "storage resolver fixture open",
    )
    .unwrap();
    TraceDecay::open_with_exclusive_maintenance(
        project_root,
        TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: Some(profile_root.join("global.db")),
        },
        &lifecycle,
    )
    .await
}

async fn open_branch_with_maintenance(
    project_root: &Path,
    branch_name: &str,
) -> tracedecay::errors::Result<TraceDecay> {
    let profile_root = maintenance_profile_root();
    prepare_maintenance_profile(&profile_root);
    let lifecycle = tracedecay::lifecycle_lease::acquire_exclusive_for_profile(
        &profile_root,
        "storage resolver fixture branch open",
    )
    .unwrap();
    let _database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle,
        &profile_root,
        "storage resolver fixture branch open",
    )
    .unwrap();
    TraceDecay::open_branch_with_exclusive_maintenance(
        project_root,
        branch_name,
        TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: Some(profile_root.join("global.db")),
        },
        &lifecycle,
    )
    .await
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

/// Initializes a profile shard enrolled under `legacy_project_id` so every fact
/// written through it is owned by that id. Canonical fact ids embed the owner
/// digest (`FactId::derive`), so a legacy store must be born under its own
/// identity — relabeling a store manifest afterwards cannot re-own its memory.
async fn init_enrolled_legacy_shard(project: &Path, legacy_project_id: &str) -> TraceDecay {
    write_enrollment_marker(
        project,
        &EnrollmentMarker {
            project_id: legacy_project_id.to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let store = init_with_maintenance(project).await.unwrap();
    assert_eq!(
        store.store_layout().identity.project_id.as_deref(),
        Some(legacy_project_id),
        "enrolled shard must own its memory under the legacy identity"
    );
    store
}

/// Points a seeded shard's manifest at a different checkout in the same
/// repository (e.g. a linked worktree that shares the git common dir) without
/// disturbing its project id, so adoption is exercised by repository identity
/// rather than an exact project-root match.
fn rebind_manifest_project_root(data_root: &Path, project_root: &Path) {
    let manifest_path = data_root.join(STORE_MANIFEST_FILENAME);
    let mut manifest = read_store_manifest(&manifest_path).unwrap();
    manifest.project_root = project_root.to_path_buf();
    write_store_manifest_to_path(&manifest_path, &manifest).unwrap();
}

/// Strips the enrollment and repository-identity markers plus the global
/// registry so a seeded shard looks like an unadopted legacy store the resolver
/// must rediscover by scanning profile manifests.
fn demote_shard_to_unadopted_legacy(project: &Path, profile_root: &Path) {
    let _ = fs::remove_file(enrollment_marker_path(project));
    if let Some(marker) = repository_identity_path(project) {
        let _ = fs::remove_file(marker);
    }
    remove_sqlite_family(&profile_root.join("global.db"));
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

async fn register_observation_store(
    db: &HostAdmissionTestRuntimeV1,
    profile_root: &Path,
    project_id: &str,
    project_root: &Path,
    git_common_dir: Option<&Path>,
) -> (PathBuf, PathBuf) {
    db.upsert_code_project(project_id, project_root, git_common_dir, None, Some("main"))
        .await
        .unwrap();
    db.upsert_store_instance(StoreInstanceUpsert {
        store_id: format!("store:{project_id}:profile_sharded"),
        project_id: project_id.to_string(),
        store_kind: "code_project".to_string(),
        storage_mode: "profile_sharded".to_string(),
        store_relpath: format!("projects/{project_id}"),
        manifest_relpath: Some(format!("projects/{project_id}/store_manifest.json")),
        last_verified_at: Some(100),
        last_write_at: Some(101),
    })
    .await
    .unwrap();
    let store_root = profile_root.join(format!("projects/{project_id}"));
    let database_path = store_root.join("sessions.db");
    fs::create_dir_all(&store_root).unwrap();
    write_store_manifest_to_path(
        &store_root.join(STORE_MANIFEST_FILENAME),
        &StoreManifest {
            schema_version: STORE_MANIFEST_SCHEMA_VERSION,
            project_id: Some(project_id.to_string()),
            store_kind: StoreKind::CodeProject,
            storage_mode: StorageMode::ProfileSharded,
            project_root: project_root.to_path_buf(),
            data_root: store_root.clone(),
            graph_db_relpath: PathBuf::from("tracedecay.db"),
            sessions_db_relpath: PathBuf::from("sessions.db"),
            branch_meta_relpath: PathBuf::from("branch-meta.json"),
        },
    )
    .unwrap();
    let (database, _) = crate::common::initialize_test_database(&database_path)
        .await
        .unwrap();
    drop(database);
    (store_root, database_path)
}

// --- Repository identity across symlinks / renames / moves (regression) -----
//
// These pin the resolution semantics for `read_repository_identity_marker` and
// registered project-store resolution: canonicalized aliases and moved
// checkouts keep one project identity, a moved repo whose old path is later
// reused by an unrelated repo self-heals, and a genuine copy still fails closed.

async fn register_identity_store(
    db: &HostAdmissionTestRuntimeV1,
    project_id: &str,
    project_root: &Path,
    git_common_dir: &Path,
) {
    db.upsert_code_project(
        project_id,
        project_root,
        Some(git_common_dir),
        None,
        Some("main"),
    )
    .await
    .unwrap();
    db.upsert_store_instance(StoreInstanceUpsert {
        store_id: format!("store_{project_id}"),
        project_id: project_id.to_string(),
        store_kind: "code_project".to_string(),
        storage_mode: "profile_sharded".to_string(),
        store_relpath: format!("projects/{project_id}"),
        manifest_relpath: Some(format!("projects/{project_id}/store_manifest.json")),
        last_verified_at: Some(100),
        last_write_at: Some(101),
    })
    .await
    .unwrap();
}

/// Recursively copy a directory tree (including a repo's `.git` and its identity
/// marker) so a genuine `cp -a` style duplicate can be staged portably.
fn copy_dir_all(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let file_type = entry.file_type().unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&from, &to);
        } else if file_type.is_symlink() {
            #[cfg(unix)]
            {
                let target = fs::read_link(&from).unwrap();
                symlink(target, &to).unwrap();
            }
        } else {
            fs::copy(&from, &to).unwrap();
        }
    }
}
