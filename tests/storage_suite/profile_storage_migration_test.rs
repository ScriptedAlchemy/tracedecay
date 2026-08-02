use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tracedecay::application::host_admission::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
use tracedecay::branch::BranchAddOutcome;
use tracedecay::branch_meta::{self, BranchMeta};
use tracedecay::config::{TraceDecayConfig, USER_DATA_DIR_ENV};
use tracedecay::global_db::{GraphScopeUpsert, StoreArtifactUpsert, StoreInstanceUpsert};
use tracedecay::migrate::inventory::{
    MigrationInventory, RegistryStatus, StoreArtifact, StoreBrand, StoreInventory, StoreRole,
    StoreStatus,
};
use tracedecay::migrate::manifest::{
    MigrationPlanOptions, apply_migration_manifest, build_plan_manifest, finalize_migration_apply,
    verify_migration_manifest,
};
#[cfg(feature = "test-transport")]
use tracedecay::migrate::registry::{
    RegistryReconstructionReport, RegistryReconstructionStatus,
    reconstruct_registry_from_store_manifest, scan_profile_store_manifests,
};
use tracedecay::serve;
use tracedecay::storage::{
    EnrollmentMarker, STORE_MANIFEST_FILENAME, STORE_MANIFEST_SCHEMA_VERSION, StorageMode,
    StoreKind, StoreManifest, read_enrollment_marker, write_enrollment_marker,
    write_repository_identity_marker,
};
use tracedecay::tracedecay::{TraceDecay, TraceDecayOpenOptions};
#[cfg(feature = "test-transport")]
use tracedecay_domain::ProjectId;

use crate::common::EnvVarGuard;
use crate::support::{HOME_ENV_LOCK, ephemeral_safe_fixture_base};

struct HomeEnvGuard {
    previous_home: Option<OsString>,
    previous_userprofile: Option<OsString>,
    previous_data_dir: Option<OsString>,
}

#[cfg(unix)]
fn colliding_non_unicode_project_paths(root: &Path) -> (PathBuf, PathBuf) {
    use std::os::unix::ffi::OsStringExt as _;

    (
        root.join(OsString::from_vec(vec![b'p', 0x80])),
        root.join(OsString::from_vec(vec![b'p', 0x81])),
    )
}

#[cfg(windows)]
fn colliding_non_unicode_project_paths(root: &Path) -> (PathBuf, PathBuf) {
    use std::os::windows::ffi::OsStringExt as _;

    (
        root.join(OsString::from_wide(&[u16::from(b'p'), 0xd800])),
        root.join(OsString::from_wide(&[u16::from(b'p'), 0xd801])),
    )
}

#[tokio::test]
async fn hermes_home_env_cannot_redirect_legacy_migration() {
    let _lock = HOME_ENV_LOCK.lock().await;
    let temp = TempDir::new().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("profile");
    let redirected = temp.path().join("redirected-hermes");
    let redirected_db = redirected.join(".tracedecay/sessions.db");
    fs::create_dir_all(redirected_db.parent().unwrap()).unwrap();
    fs::write(&redirected_db, b"must remain untouched").unwrap();
    let _hermes_home = EnvVarGuard::set("HERMES_HOME", &redirected);

    let report =
        tracedecay::migrate::hermes::migrate_legacy_hermes_stores_to(&user_home, &profile_root)
            .await;

    assert_eq!(report, Default::default());
    assert_eq!(fs::read(&redirected_db).unwrap(), b"must remain untouched");
    assert!(!profile_root.join("global.db").exists());
    assert!(!profile_root.join("projects").exists());
}

impl HomeEnvGuard {
    fn set(home: &Path) -> Self {
        let previous_home = std::env::var_os("HOME");
        let previous_userprofile = std::env::var_os("USERPROFILE");
        let previous_data_dir = std::env::var_os(USER_DATA_DIR_ENV);
        unsafe {
            std::env::set_var("HOME", home);
            std::env::set_var("USERPROFILE", home);
            std::env::set_var(USER_DATA_DIR_ENV, home.join(".tracedecay"));
        }
        Self {
            previous_home,
            previous_userprofile,
            previous_data_dir,
        }
    }
}

impl Drop for HomeEnvGuard {
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

fn prepare_maintenance_profile(profile_root: &Path) {
    fs::create_dir_all(profile_root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(profile_root, fs::Permissions::from_mode(0o700)).unwrap();
    }
}

async fn init_with_maintenance(
    project_root: &Path,
    profile_root: &Path,
    open_options: TraceDecayOpenOptions,
) -> tracedecay::errors::Result<TraceDecay> {
    prepare_maintenance_profile(profile_root);
    let lifecycle = tracedecay::lifecycle_lease::acquire_exclusive_for_profile(
        profile_root,
        "profile storage migration fixture initialization",
    )
    .unwrap();
    let _database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle,
        profile_root,
        "profile storage migration fixture initialization",
    )
    .unwrap();
    TraceDecay::init_with_exclusive_maintenance(project_root, open_options, &lifecycle).await
}

async fn open_with_maintenance(
    project_root: &Path,
    profile_root: &Path,
    open_options: TraceDecayOpenOptions,
) -> tracedecay::errors::Result<TraceDecay> {
    prepare_maintenance_profile(profile_root);
    let lifecycle = tracedecay::lifecycle_lease::acquire_exclusive_for_profile(
        profile_root,
        "profile storage migration fixture open",
    )
    .unwrap();
    let _database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle,
        profile_root,
        "profile storage migration fixture open",
    )
    .unwrap();
    TraceDecay::open_with_exclusive_maintenance(project_root, open_options, &lifecycle).await
}

async fn open_branch_with_maintenance(
    project_root: &Path,
    branch_name: &str,
    profile_root: &Path,
    open_options: TraceDecayOpenOptions,
) -> tracedecay::errors::Result<TraceDecay> {
    prepare_maintenance_profile(profile_root);
    let lifecycle = tracedecay::lifecycle_lease::acquire_exclusive_for_profile(
        profile_root,
        "profile storage migration fixture branch open",
    )
    .unwrap();
    let _database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle,
        profile_root,
        "profile storage migration fixture branch open",
    )
    .unwrap();
    TraceDecay::open_branch_with_exclusive_maintenance(
        project_root,
        branch_name,
        open_options,
        &lifecycle,
    )
    .await
}

fn portable_relpath(path: &str) -> String {
    path.replace('\\', "/")
}

fn run_git(project: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(project)
        .output()
        .unwrap_or_else(|err| panic!("failed to run git {args:?}: {err}"));
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn table_exists(db_path: &std::path::Path, table: &str) -> bool {
    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1
         )",
        rusqlite::params![table],
        |row| row.get::<_, bool>(0),
    )
    .unwrap()
}

fn write_profile_store_manifest(profile_root: &Path, project_root: &Path) -> std::path::PathBuf {
    write_profile_store_manifest_for_id(profile_root, project_root, "proj_123")
}

fn write_profile_store_manifest_for_id(
    profile_root: &Path,
    project_root: &Path,
    project_id: &str,
) -> std::path::PathBuf {
    let data_root = profile_root.join("projects").join(project_id);
    fs::create_dir_all(&data_root).unwrap();
    fs::create_dir_all(project_root).unwrap();
    fs::write(data_root.join("tracedecay.db"), b"graph").unwrap();
    fs::write(data_root.join("sessions.db"), b"sessions").unwrap();
    let branch_meta = BranchMeta::new_for_dir(&data_root, "main");
    branch_meta::save_branch_meta(&data_root, &branch_meta).unwrap();
    let manifest = StoreManifest {
        schema_version: STORE_MANIFEST_SCHEMA_VERSION,
        project_id: Some(project_id.to_string()),
        store_kind: StoreKind::CodeProject,
        storage_mode: StorageMode::ProfileSharded,
        project_root: project_root.to_path_buf(),
        data_root: data_root.clone(),
        graph_db_relpath: "tracedecay.db".into(),
        sessions_db_relpath: "sessions.db".into(),
        branch_meta_relpath: "branch-meta.json".into(),
    };
    let manifest_path = data_root.join(STORE_MANIFEST_FILENAME);
    fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    manifest_path
}

#[tokio::test]
async fn global_db_creates_profile_storage_registry_tables() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let runtime = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();
    runtime.checkpoint_profile_database_for_test().await;
    drop(runtime);

    for table in [
        "code_projects",
        "project_aliases",
        "store_instances",
        "graph_scopes",
        "store_artifacts",
    ] {
        assert!(table_exists(&db_path, table), "{table} missing");
    }
}

#[test]
fn reconstructs_registry_records_from_profile_store_manifest() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    let manifest_path = write_profile_store_manifest(&profile_root, &project_root);

    let report =
        reconstruct_registry_from_store_manifest(&manifest_path, &profile_root, 1_800_000_001);

    assert!(report.issues.is_empty(), "{:?}", report.issues);
    assert_eq!(report.plans.len(), 1);
    let plan = &report.plans[0];
    let canonical_project_root = project_root.canonicalize().unwrap();
    assert_eq!(plan.status, RegistryReconstructionStatus::Eligible);
    assert_eq!(plan.project.project_id, "proj_123");
    assert_eq!(plan.project.project_root, canonical_project_root);
    assert_eq!(plan.project.aliases, vec![canonical_project_root]);
    assert_eq!(plan.store.project_id, "proj_123");
    assert_eq!(plan.store.store_kind, "code_project");
    assert_eq!(plan.store.storage_mode, "profile_sharded");
    assert_eq!(plan.store.store_relpath, "projects/proj_123");
    assert_eq!(
        plan.store.manifest_relpath.as_deref().map(portable_relpath),
        Some("projects/proj_123/store_manifest.json".to_string())
    );
    assert_eq!(plan.store.last_verified_at, Some(1_800_000_001));
    assert!(
        plan.artifacts
            .iter()
            .any(|artifact| artifact.artifact_kind == "graph_db"
                && portable_relpath(&artifact.relpath) == "projects/proj_123/tracedecay.db")
    );
    assert!(
        plan.artifacts
            .iter()
            .any(|artifact| artifact.artifact_kind == "store_manifest"
                && portable_relpath(&artifact.relpath) == "projects/proj_123/store_manifest.json")
    );
    assert_eq!(plan.graph_scopes.len(), 1);
    assert_eq!(plan.graph_scopes[0].branch_name, "main");
    assert_eq!(
        portable_relpath(&plan.graph_scopes[0].db_relpath),
        "projects/proj_123/tracedecay.db"
    );
}

#[test]
fn scan_profile_store_manifests_rejects_unsafe_manifest_relpaths() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let data_root = profile_root.join("projects/proj_bad");
    let project_root = dir.path().join("repo");
    fs::create_dir_all(&data_root).unwrap();
    fs::create_dir_all(&project_root).unwrap();
    let manifest = StoreManifest {
        schema_version: STORE_MANIFEST_SCHEMA_VERSION,
        project_id: Some("proj_bad".to_string()),
        store_kind: StoreKind::CodeProject,
        storage_mode: StorageMode::ProfileSharded,
        project_root,
        data_root,
        graph_db_relpath: "../outside.db".into(),
        sessions_db_relpath: "sessions.db".into(),
        branch_meta_relpath: "branch-meta.json".into(),
    };
    fs::write(
        profile_root.join("projects/proj_bad/store_manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let report = scan_profile_store_manifests(&profile_root, 1_800_000_001);

    assert!(report.plans.is_empty());
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.contains("unsafe graph_db_relpath"))
    );
}

#[cfg(unix)]
#[test]
fn reconstruction_accepts_equivalent_profile_symlink_but_rejects_symlink_escape() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let profile_alias = dir.path().join("profile-alias");
    let project_root = dir.path().join("repo");
    let manifest = write_profile_store_manifest(&profile_root, &project_root);
    std::os::unix::fs::symlink(&profile_root, &profile_alias).unwrap();

    let equivalent = reconstruct_registry_from_store_manifest(&manifest, &profile_alias, 1);
    assert!(equivalent.issues.is_empty(), "{:?}", equivalent.issues);
    assert_eq!(equivalent.plans.len(), 1);

    let outside = dir.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let escaped_root = profile_root.join("projects/proj_escape");
    std::os::unix::fs::symlink(&outside, &escaped_root).unwrap();
    let escaped_manifest = StoreManifest {
        schema_version: STORE_MANIFEST_SCHEMA_VERSION,
        project_id: Some("proj_escape".to_string()),
        store_kind: StoreKind::CodeProject,
        storage_mode: StorageMode::ProfileSharded,
        project_root,
        data_root: escaped_root.clone(),
        graph_db_relpath: "tracedecay.db".into(),
        sessions_db_relpath: "sessions.db".into(),
        branch_meta_relpath: "branch-meta.json".into(),
    };
    let escaped_manifest_path = escaped_root.join(STORE_MANIFEST_FILENAME);
    fs::write(
        &escaped_manifest_path,
        serde_json::to_string_pretty(&escaped_manifest).unwrap(),
    )
    .unwrap();

    let escaped =
        reconstruct_registry_from_store_manifest(&escaped_manifest_path, &profile_root, 1);
    assert!(escaped.plans.is_empty());
    assert!(
        escaped
            .issues
            .iter()
            .any(|issue| issue.contains("outside profile root"))
    );
}

#[test]
fn unsafe_branch_database_path_blocks_reconstruction() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    let manifest = write_profile_store_manifest(&profile_root, &project_root);
    let branch_meta_path = manifest.parent().unwrap().join("branch-meta.json");
    let mut branch_meta: serde_json::Value =
        serde_json::from_slice(&fs::read(&branch_meta_path).unwrap()).unwrap();
    branch_meta["branches"]["main"]["db_file"] = serde_json::json!("../escape.db");
    fs::write(
        &branch_meta_path,
        serde_json::to_vec_pretty(&branch_meta).unwrap(),
    )
    .unwrap();

    let report = reconstruct_registry_from_store_manifest(&manifest, &profile_root, 1);

    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.contains("must reference canonical main database")),
        "unexpected reconstruction issues: {:?}",
        report.issues
    );
}

#[tokio::test]
async fn registry_resolves_project_store_by_canonical_alias() {
    let dir = TempDir::new().unwrap();
    let project_root = dir.path().join("repo");
    fs::create_dir_all(&project_root).unwrap();
    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();

    let project = db
        .upsert_code_project(
            "proj_123",
            &project_root,
            None,
            Some("https://example.test/repo.git"),
            Some("main"),
        )
        .await
        .unwrap();
    db.upsert_project_alias(&project_root.join("."), &project.project_id)
        .await
        .unwrap();
    let store = db
        .upsert_store_instance(StoreInstanceUpsert {
            store_id: "store_123".to_string(),
            project_id: project.project_id.clone(),
            store_kind: "code_project".to_string(),
            storage_mode: "profile_sharded".to_string(),
            store_relpath: "projects/proj_123".to_string(),
            manifest_relpath: Some("projects/proj_123/store_manifest.json".to_string()),
            last_verified_at: Some(42),
            last_write_at: Some(43),
        })
        .await
        .unwrap();
    db.upsert_graph_scope(GraphScopeUpsert {
        graph_scope_id: "scope_123".to_string(),
        project_id: project.project_id.clone(),
        store_id: store.store_id.clone(),
        branch_name: "main".to_string(),
        db_relpath: "tracedecay.db".to_string(),
        parent_scope_id: None,
        last_synced_at: Some(44),
        writable: true,
    })
    .await
    .unwrap();
    db.upsert_store_artifact(StoreArtifactUpsert {
        store_id: store.store_id.clone(),
        artifact_kind: "graph_db".to_string(),
        relpath: "tracedecay.db".to_string(),
        size_bytes: Some(128),
        schema_version: Some("1".to_string()),
        updated_at: Some(45),
    })
    .await
    .unwrap();

    let resolved = db
        .resolve_project_store_by_alias(&project_root)
        .await
        .unwrap();

    assert_eq!(resolved.project.project_id, "proj_123");
    assert_eq!(resolved.store.store_id, "store_123");
    assert_eq!(resolved.graph_scopes.len(), 1);
    assert_eq!(resolved.graph_scopes[0].branch_name, "main");
    assert_eq!(resolved.artifacts.len(), 1);
    assert_eq!(resolved.artifacts[0].artifact_kind, "graph_db");
    assert_eq!(
        resolved.project.canonical_root,
        project_root.canonicalize().unwrap().to_string_lossy()
    );
}

#[tokio::test]
async fn delete_project_uses_same_canonical_key_as_upsert() {
    let dir = TempDir::new().unwrap();
    let project_root = dir.path().join("repo");
    fs::create_dir_all(&project_root).unwrap();
    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();

    db.upsert(&project_root, 99).await;
    assert_eq!(db.get_project_tokens(&project_root).await, 99);

    db.delete_project(&project_root.join(".")).await;

    assert_eq!(db.get_project_tokens(&project_root).await, 0);
}

#[tokio::test]
async fn staged_migration_resumes_cutover_after_registry_and_marker() {
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let manifest_path = root.join("manifest.json");
    let project = root.join("repo");
    let data_dir = project.join(".tracedecay");
    let graph_db = data_dir.join("tracedecay.db");
    let profile_root = root.join("profile");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(&graph_db, b"graph").unwrap();
    fs::write(
        data_dir.join("branch-meta.json"),
        r#"{"default_branch":"main","branches":{}}"#,
    )
    .unwrap();
    let graph_db_path = graph_db.clone();
    let mut manifest = build_plan_manifest(
        MigrationInventory {
            stores: vec![StoreInventory {
                project_root: project.clone(),
                data_dir,
                db_path: graph_db,
                brand: StoreBrand::TraceDecay,
                role: StoreRole::CodeProjectStore,
                registry_status: RegistryStatus::Unregistered,
                size_bytes: 128,
                statuses: vec![StoreStatus::Ok],
                artifacts: vec![StoreArtifact {
                    kind: "graph_db".to_string(),
                    path: graph_db_path,
                    size_bytes: 5,
                }],
            }],
            skipped: Vec::new(),
            global_db: None,
        },
        MigrationPlanOptions {
            manifest_path,
            migration_id: "mig_123".to_string(),
            tracedecay_version: "0.0.2".to_string(),
            created_at_unix: 1_800_000_000,
            confirmation_token: "confirm-mig_123".to_string(),
            target_profile_root: profile_root,
            project_id: "proj_123".to_string(),
        },
    )
    .unwrap();

    apply_migration_manifest(&mut manifest).await.unwrap();
    let staged = verify_migration_manifest(&manifest);
    assert!(staged.cutover_ready);
    assert!(!staged.apply_supported);
    assert!(read_enrollment_marker(&project).unwrap().is_none());

    let db = HostAdmissionTestRuntimeV1::profile(root).await.unwrap();
    db.apply_registry_reconstruction_report(&staged.registry_reconstruction)
        .await
        .unwrap();
    write_enrollment_marker(
        &project,
        &EnrollmentMarker {
            project_id: "proj_123".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    finalize_migration_apply(&mut manifest).unwrap();

    assert!(verify_migration_manifest(&manifest).apply_supported);
}

#[tokio::test]
async fn applies_registry_reconstruction_records_from_manifest() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    let manifest_path = write_profile_store_manifest(&profile_root, &project_root);
    let report =
        reconstruct_registry_from_store_manifest(&manifest_path, &profile_root, 1_800_000_001);
    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();

    let applied = db
        .apply_registry_reconstruction_report(&report)
        .await
        .unwrap();

    assert_eq!(applied.projects, 1);
    assert_eq!(applied.aliases, 1);
    assert_eq!(applied.stores, 1);
    assert_eq!(applied.graph_scopes, 1);
    assert_eq!(applied.artifacts, 4);
    let resolved = db
        .resolve_project_store_by_alias(&project_root.join("."))
        .await
        .unwrap();
    assert_eq!(resolved.project.project_id, "proj_123");
    assert_eq!(resolved.store.storage_mode, "profile_sharded");
    assert_eq!(
        resolved
            .store
            .manifest_relpath
            .as_deref()
            .map(portable_relpath),
        Some("projects/proj_123/store_manifest.json".to_string())
    );
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn registry_reconstruction_preserves_distinct_native_path_aliases() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let first_manifest = write_profile_store_manifest_for_id(
        &profile_root,
        &dir.path().join("first-source"),
        "proj_first_native",
    );
    let second_manifest = write_profile_store_manifest_for_id(
        &profile_root,
        &dir.path().join("second-source"),
        "proj_second_native",
    );
    let mut first = reconstruct_registry_from_store_manifest(&first_manifest, &profile_root, 1);
    let mut second = reconstruct_registry_from_store_manifest(&second_manifest, &profile_root, 1);
    let (first_path, second_path) = colliding_non_unicode_project_paths(dir.path());
    assert_eq!(first_path.to_string_lossy(), second_path.to_string_lossy());
    first.plans[0].project.project_root = first_path.clone();
    first.plans[0].project.aliases = vec![first_path.clone()];
    second.plans[0].project.project_root = second_path.clone();
    second.plans[0].project.aliases = vec![second_path.clone()];
    let report = RegistryReconstructionReport {
        plans: first.plans.into_iter().chain(second.plans).collect(),
        issues: Vec::new(),
    };
    let db = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .unwrap();

    let applied = db
        .apply_registry_reconstruction_report(&report)
        .await
        .unwrap();
    assert_eq!(applied.projects, 2);
    assert_eq!(applied.aliases, 2);
    assert_eq!(
        db.project_registry_context_by_alias(&first_path)
            .await
            .unwrap()
            .unwrap()
            .project
            .project_id,
        "proj_first_native"
    );
    assert_eq!(
        db.project_registry_context_by_alias(&second_path)
            .await
            .unwrap()
            .unwrap()
            .project
            .project_id,
        "proj_second_native"
    );

    let resumed = db
        .apply_registry_reconstruction_report(&report)
        .await
        .unwrap();
    assert_eq!(resumed.projects, 0);
    assert_eq!(resumed.aliases, 0);
}

#[tokio::test]
async fn single_plan_reconstruction_rejects_noneligible_and_accepts_matching_existing_rows() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    let manifest_path = write_profile_store_manifest(&profile_root, &project_root);
    let report =
        reconstruct_registry_from_store_manifest(&manifest_path, &profile_root, 1_800_000_001);
    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();

    let mut retired = report.clone();
    retired.plans[0].status = RegistryReconstructionStatus::Retired;
    retired.plans[0].status_reason = Some("superseded project".to_string());
    let error = db
        .apply_single_registry_reconstruction_report(&retired)
        .await
        .unwrap_err();
    assert!(error.iter().any(|issue| issue.contains("Retired")));
    assert!(db.get_code_project("proj_123").await.is_none());

    let inserted = db
        .apply_registry_reconstruction_report(&report)
        .await
        .unwrap();
    assert_eq!(inserted.projects, 1);
    assert_eq!(inserted.stores, 1);

    let resumed = db
        .apply_single_registry_reconstruction_report(&report)
        .await
        .unwrap();
    assert_eq!(resumed.projects, 0);
    assert_eq!(resumed.stores, 0);
    assert_eq!(
        db.resolve_project_store_by_identity(&project_root, None)
            .await
            .unwrap()
            .unwrap()
            .project
            .project_id,
        "proj_123"
    );
}

#[tokio::test]
async fn conflicting_alias_is_rejected_without_stealing_or_partial_writes() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    let manifest = write_profile_store_manifest(&profile_root, &project_root);
    let report = reconstruct_registry_from_store_manifest(&manifest, &profile_root, 1);
    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();
    db.upsert_code_project("proj_owner", &project_root, None, None, None)
        .await
        .unwrap();

    let error = db
        .apply_registry_reconstruction_report(&report)
        .await
        .unwrap_err();

    assert!(error.iter().any(|issue| issue.contains("already owned")));
    assert!(db.get_code_project("proj_123").await.is_none());
    assert_eq!(
        db.project_registry_context_by_alias(&project_root)
            .await
            .unwrap()
            .unwrap()
            .project
            .project_id,
        "proj_owner"
    );

    let second_project_root = dir.path().join("repo-2");
    let second_manifest = write_profile_store_manifest(&profile_root, &second_project_root);
    let second_report =
        reconstruct_registry_from_store_manifest(&second_manifest, &profile_root, 2);
    let applied = db
        .apply_registry_reconstruction_report(&second_report)
        .await
        .unwrap();
    assert_eq!(applied.projects, 1);
    assert_eq!(
        db.project_registry_context_by_alias(&second_project_root)
            .await
            .unwrap()
            .unwrap()
            .project
            .project_id,
        "proj_123"
    );
}

#[tokio::test]
async fn conflicting_later_plan_rolls_back_the_entire_reconstruction_batch() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let first_root = dir.path().join("first-repo");
    let second_root = dir.path().join("second-repo");
    let first = write_profile_store_manifest_for_id(&profile_root, &first_root, "proj_first");
    let second = write_profile_store_manifest_for_id(&profile_root, &second_root, "proj_second");
    let first = reconstruct_registry_from_store_manifest(&first, &profile_root, 1);
    let second = reconstruct_registry_from_store_manifest(&second, &profile_root, 1);
    let report = RegistryReconstructionReport {
        plans: first.plans.into_iter().chain(second.plans).collect(),
        issues: Vec::new(),
    };
    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();
    db.upsert_code_project("proj_owner", &second_root, None, None, None)
        .await
        .unwrap();

    db.apply_registry_reconstruction_report(&report)
        .await
        .unwrap_err();

    assert!(db.get_code_project("proj_first").await.is_none());
    assert!(db.get_code_project("proj_second").await.is_none());
    assert!(db.get_code_project("proj_owner").await.is_some());
}

#[tokio::test]
async fn physical_store_path_conflict_rolls_back_without_creating_project() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    let manifest = write_profile_store_manifest(&profile_root, &project_root);
    let report = reconstruct_registry_from_store_manifest(&manifest, &profile_root, 1);
    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();
    let owner_root = dir.path().join("owner");
    fs::create_dir_all(&owner_root).unwrap();
    db.upsert_code_project("proj_owner", &owner_root, None, None, None)
        .await
        .unwrap();
    db.upsert_store_instance(StoreInstanceUpsert {
        store_id: "store:owner".to_string(),
        project_id: "proj_owner".to_string(),
        store_kind: "code_project".to_string(),
        storage_mode: "profile_sharded".to_string(),
        store_relpath: report.plans[0].store.store_relpath.clone(),
        manifest_relpath: None,
        last_verified_at: None,
        last_write_at: None,
    })
    .await
    .unwrap();

    let error = db
        .apply_registry_reconstruction_report(&report)
        .await
        .unwrap_err();

    assert!(
        error
            .iter()
            .any(|issue| issue.contains("physical store path"))
    );
    assert!(db.get_code_project("proj_123").await.is_none());
}

#[tokio::test]
async fn physical_graph_scope_conflict_rolls_back_without_creating_project() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    let manifest = write_profile_store_manifest(&profile_root, &project_root);
    let report = reconstruct_registry_from_store_manifest(&manifest, &profile_root, 1);
    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();
    let owner_root = dir.path().join("owner");
    fs::create_dir_all(&owner_root).unwrap();
    db.upsert_code_project("proj_owner", &owner_root, None, None, None)
        .await
        .unwrap();
    db.upsert_store_instance(StoreInstanceUpsert {
        store_id: "store:owner".to_string(),
        project_id: "proj_owner".to_string(),
        store_kind: "code_project".to_string(),
        storage_mode: "profile_sharded".to_string(),
        store_relpath: "projects/owner".to_string(),
        manifest_relpath: None,
        last_verified_at: None,
        last_write_at: None,
    })
    .await
    .unwrap();
    db.upsert_graph_scope(GraphScopeUpsert {
        graph_scope_id: "scope:owner".to_string(),
        project_id: "proj_owner".to_string(),
        store_id: "store:owner".to_string(),
        branch_name: "owner".to_string(),
        db_relpath: report.plans[0].graph_scopes[0].db_relpath.clone(),
        parent_scope_id: None,
        last_synced_at: None,
        writable: true,
    })
    .await
    .unwrap();

    let error = db
        .apply_registry_reconstruction_report(&report)
        .await
        .unwrap_err();

    assert!(
        error
            .iter()
            .any(|issue| issue.contains("physical graph database path"))
    );
    assert!(db.get_code_project("proj_123").await.is_none());
}

#[test]
fn missing_and_temporary_project_roots_are_classified_stale() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("temporary-repo");
    let manifest = write_profile_store_manifest(&profile_root, &project_root);
    write_enrollment_marker(
        &project_root,
        &EnrollmentMarker {
            project_id: "proj_123".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();

    let scanned = scan_profile_store_manifests(&profile_root, 1);
    assert_eq!(scanned.plans[0].status, RegistryReconstructionStatus::Stale);
    assert!(
        scanned.plans[0]
            .status_reason
            .as_deref()
            .unwrap()
            .contains("temporary directory")
    );

    fs::remove_dir_all(&project_root).unwrap();
    let missing = reconstruct_registry_from_store_manifest(&manifest, &profile_root, 1);
    assert_eq!(missing.plans[0].status, RegistryReconstructionStatus::Stale);
    assert!(
        missing.plans[0]
            .status_reason
            .as_deref()
            .unwrap()
            .contains("unavailable")
    );
}

#[test]
fn strict_scan_classifies_unmarked_temporary_project_root_stale() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("unmarked-repo");
    write_profile_store_manifest(&profile_root, &project_root);

    let scanned = scan_profile_store_manifests(&profile_root, 1);

    assert_eq!(scanned.plans[0].status, RegistryReconstructionStatus::Stale);
    assert!(
        scanned.plans[0]
            .status_reason
            .as_deref()
            .unwrap()
            .contains("temporary directory")
    );
}

#[test]
fn strict_scan_requires_matching_repository_identity_or_enrollment() {
    let dir = tempfile::Builder::new()
        .prefix("reconstruct-identity-")
        .tempdir_in(ephemeral_safe_fixture_base())
        .unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    write_profile_store_manifest(&profile_root, &project_root);
    run_git(&project_root, &["init"]);

    let unowned = scan_profile_store_manifests(&profile_root, 1);
    assert_eq!(
        unowned.plans[0].status,
        RegistryReconstructionStatus::Blocked
    );
    assert!(
        unowned.plans[0]
            .status_reason
            .as_deref()
            .unwrap()
            .contains("no repository identity or enrollment marker")
    );

    write_enrollment_marker(
        &project_root,
        &EnrollmentMarker {
            project_id: "proj_123".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let enrolled = scan_profile_store_manifests(&profile_root, 1);
    assert_eq!(
        enrolled.plans[0].status,
        RegistryReconstructionStatus::Eligible
    );
}

#[tokio::test]
async fn consolidation_source_is_skipped_while_destination_applies() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    fs::create_dir_all(&project_root).unwrap();
    run_git(&project_root, &["init"]);
    write_repository_identity_marker(&project_root, "proj_destination").unwrap();
    let source = write_profile_store_manifest_for_id(
        &profile_root,
        &project_root,
        "proj_consolidated_source",
    );
    fs::write(
        source.parent().unwrap().join("branch-meta.json"),
        r#"{"default_branch":"main","branches":{"main":{"db_file":"../outside.db","created_at":"1","last_synced_at":"1"}}}"#,
    )
    .unwrap();
    let destination =
        write_profile_store_manifest_for_id(&profile_root, &project_root, "proj_destination");

    let source = reconstruct_registry_from_store_manifest(&source, &profile_root, 1);
    let destination = reconstruct_registry_from_store_manifest(&destination, &profile_root, 1);

    assert_eq!(
        source.plans[0].status,
        RegistryReconstructionStatus::Retired
    );
    assert!(source.issues.is_empty(), "{:?}", source.issues);
    assert_eq!(
        destination.plans[0].status,
        RegistryReconstructionStatus::Eligible
    );
    let issues = source
        .issues
        .into_iter()
        .chain(destination.issues)
        .collect();
    let report = RegistryReconstructionReport {
        plans: source.plans.into_iter().chain(destination.plans).collect(),
        issues,
    };
    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();

    let applied = db
        .apply_registry_reconstruction_report(&report)
        .await
        .unwrap();

    assert_eq!(applied.projects, 1);
    assert!(
        db.get_code_project("proj_consolidated_source")
            .await
            .is_none()
    );
    assert!(db.get_code_project("proj_destination").await.is_some());
}

#[test]
fn disagreeing_dual_markers_block_and_matching_retired_markers_retire() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    fs::create_dir_all(&project_root).unwrap();
    run_git(&project_root, &["init"]);
    write_repository_identity_marker(&project_root, "proj_repository").unwrap();
    write_enrollment_marker(
        &project_root,
        &EnrollmentMarker {
            project_id: "proj_enrollment".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let manifest =
        write_profile_store_manifest_for_id(&profile_root, &project_root, "proj_repository");

    let disagreement = reconstruct_registry_from_store_manifest(&manifest, &profile_root, 1);
    assert_eq!(
        disagreement.plans[0].status,
        RegistryReconstructionStatus::Blocked
    );

    write_enrollment_marker(
        &project_root,
        &EnrollmentMarker {
            project_id: "proj_repository".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let retired_manifest =
        write_profile_store_manifest_for_id(&profile_root, &project_root, "proj_retired_manifest");
    let retired = reconstruct_registry_from_store_manifest(&retired_manifest, &profile_root, 1);
    assert_eq!(
        retired.plans[0].status,
        RegistryReconstructionStatus::Retired
    );
}

#[tokio::test]
async fn cursor_session_db_uses_registry_profile_shard_without_marker() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("home");
    let profile_root = home.join(".tracedecay");
    let project_root = dir.path().join("repo");
    let manifest_path = write_profile_store_manifest(&profile_root, &project_root);
    let session_db = profile_root.join("projects/proj_123/sessions.db");
    fs::remove_file(&session_db).unwrap();
    let global = HostAdmissionTestRuntimeV1::project(
        &profile_root,
        &project_root,
        ProjectId::new("proj_123").unwrap(),
    )
    .await
    .unwrap();
    let _home_guard = HomeEnvGuard::set(&home);
    let report =
        reconstruct_registry_from_store_manifest(&manifest_path, &profile_root, 1_800_000_001);
    global
        .apply_registry_reconstruction_report(&report)
        .await
        .unwrap();

    assert_eq!(
        global.database_path(HostAdmissionScope::Project),
        Some(session_db.as_path()),
        "session ingest should retain the registry-backed profile session DB"
    );
    assert!(session_db.is_file());
    assert!(
        !project_root.join(".tracedecay/sessions.db").exists(),
        "session ingest must not create a repo-local sessions DB for registry-backed profile stores"
    );
}

#[tokio::test]
async fn trace_decay_init_uses_profile_shard_when_enrolled() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let home = root.join("home");
    let profile_root = home.join(".tracedecay");
    let project = root.join("repo");
    let shard_root = profile_root.join("projects/proj_init");
    fs::create_dir_all(&project).unwrap();
    let _home_guard = HomeEnvGuard::set(&home);
    write_enrollment_marker(
        &project,
        &EnrollmentMarker {
            project_id: "proj_init".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();

    let cg = init_with_maintenance(&project, &profile_root, TraceDecayOpenOptions::default())
        .await
        .unwrap();

    assert_path_eq(&cg.store_layout().data_root, &shard_root);
    assert_path_eq(cg.db_path(), shard_root.join("tracedecay.db"));
    assert!(cg.db_path().is_file());
    assert!(
        !shard_root.join("config.json").exists(),
        "profile-sharded init persists configuration in the store, not a legacy config.json"
    );
    assert!(shard_root.join(STORE_MANIFEST_FILENAME).is_file());
    assert!(
        !project.join(".tracedecay/tracedecay.db").exists(),
        "profile-sharded init must not create a repo-local graph DB"
    );
}

#[tokio::test]
async fn trace_decay_init_with_options_uses_explicit_profile_identity() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let daemon_home = root.join("daemon-home");
    let client_profile = root.join("client-profile");
    let project = root.join("repo");
    fs::create_dir_all(&project).unwrap();
    let _home_guard = HomeEnvGuard::set(&daemon_home);
    write_enrollment_marker(
        &project,
        &EnrollmentMarker {
            project_id: "proj_explicit".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let open_options = TraceDecayOpenOptions {
        profile_root: Some(client_profile.clone()),
        global_db_path: Some(client_profile.join("global.db")),
    };

    assert!(
        !TraceDecay::is_initialized_with_options(&project, &open_options),
        "a marker alone must not initialize an explicit client profile"
    );

    let cg = init_with_maintenance(&project, &client_profile, open_options.clone())
        .await
        .unwrap();

    assert_eq!(
        cg.store_layout().data_root,
        client_profile.join("projects/proj_explicit")
    );
    assert!(
        !cg.store_layout().config_path.exists(),
        "init persists configuration in the store, not a legacy config.json"
    );
    assert!(cg.db_path().is_file());
    assert!(TraceDecay::is_initialized_with_options(
        &project,
        &open_options
    ));
    assert!(
        !daemon_home.join(".tracedecay").exists(),
        "explicit client profile init must not create a store in the daemon/default profile"
    );
}

#[tokio::test]
async fn trace_decay_options_global_db_path_implies_profile_root() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let daemon_home = root.join("daemon-home");
    let client_profile = root.join("client-profile");
    let project = root.join("repo");
    fs::create_dir_all(&project).unwrap();
    let _home_guard = HomeEnvGuard::set(&daemon_home);
    write_enrollment_marker(
        &project,
        &EnrollmentMarker {
            project_id: "proj_db_only".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let open_options = TraceDecayOpenOptions {
        profile_root: None,
        global_db_path: Some(client_profile.join("global.db")),
    };

    let cg = init_with_maintenance(&project, &client_profile, open_options.clone())
        .await
        .unwrap();

    assert_eq!(
        cg.store_layout().data_root,
        client_profile.join("projects/proj_db_only")
    );
    assert!(
        !cg.store_layout().config_path.exists(),
        "init persists configuration in the store, not a legacy config.json"
    );
    assert!(cg.db_path().is_file());
    assert!(TraceDecay::is_initialized_with_options(
        &project,
        &open_options
    ));
    assert!(
        !daemon_home.join(".tracedecay").exists(),
        "global_db_path-only options must not fall back to the daemon/default profile"
    );
}

#[tokio::test]
async fn trace_decay_add_branch_tracking_returns_not_indexed_for_uninitialized_profile_store() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let home = root.join("home");
    let project = root.join("repo");
    fs::create_dir_all(&project).unwrap();
    let _home_guard = HomeEnvGuard::set(&home);

    let outcome = TraceDecay::add_branch_tracking(&project, "feature/unindexed")
        .await
        .unwrap();

    assert_eq!(outcome, BranchAddOutcome::NotIndexed);
    assert!(
        !home
            .join(".tracedecay/projects")
            .join(tracedecay::storage::default_profile_project_id(&project))
            .exists(),
        "branch add must not create project profile storage before tracedecay init"
    );
}

#[tokio::test]
async fn trace_decay_open_matches_renamed_git_checkout_by_registered_remote() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let home = root.join("home");
    let project = root.join("repo-before-rename");
    let renamed = root.join("repo-after-rename");
    fs::create_dir_all(&project).unwrap();
    run_git(&project, &["init"]);
    run_git(
        &project,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:ScriptedAlchemy/tracedecay.git",
        ],
    );
    let _home_guard = HomeEnvGuard::set(&home);

    let profile_root = home.join(".tracedecay");
    let initialized =
        init_with_maintenance(&project, &profile_root, TraceDecayOpenOptions::default())
            .await
            .unwrap();
    let original_project_id = initialized
        .store_layout()
        .identity
        .project_id
        .clone()
        .unwrap();
    let original_data_root = initialized.store_layout().data_root.clone();
    drop(initialized);
    fs::rename(&project, &renamed).unwrap();

    let reopened = open_with_maintenance(&renamed, &profile_root, TraceDecayOpenOptions::default())
        .await
        .unwrap();

    assert_eq!(
        reopened.store_layout().identity.project_id.as_deref(),
        Some(original_project_id.as_str())
    );
    assert_eq!(reopened.store_layout().data_root, original_data_root);
    assert!(
        !home
            .join(".tracedecay/projects")
            .join(tracedecay::storage::default_profile_project_id(&renamed))
            .join("tracedecay.db")
            .exists(),
        "renamed checkout must not create a second path-hash profile shard"
    );
}

#[tokio::test]
async fn persisted_repository_identity_survives_rename_while_serve_open_fails_closed() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let daemon_home = root.join("daemon-home");
    let client_profile = root.join("client-profile");
    let project = root.join("repo-before-rename");
    let renamed = root.join("repo-after-rename");
    fs::create_dir_all(&project).unwrap();
    run_git(&project, &["init"]);
    run_git(
        &project,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:ScriptedAlchemy/tracedecay.git",
        ],
    );
    let _home_guard = HomeEnvGuard::set(&daemon_home);
    let open_options = TraceDecayOpenOptions {
        profile_root: Some(client_profile.clone()),
        global_db_path: Some(client_profile.join("global.db")),
    };

    let initialized = init_with_maintenance(&project, &client_profile, open_options.clone())
        .await
        .unwrap();
    let original_data_root = initialized.store_layout().data_root.clone();
    drop(initialized);
    fs::rename(&project, &renamed).unwrap();

    assert!(
        TraceDecay::is_initialized_with_options(&renamed, &open_options),
        "the durable git marker should resolve the moved profile store synchronously"
    );
    let serve_error =
        match serve::ensure_initialized_with_options(&renamed, open_options.clone()).await {
            Ok(_) => panic!("serve compatibility API must not open the project database locally"),
            Err(error) => error,
        };
    assert!(
        serve_error
            .to_string()
            .contains("managed TraceDecay daemon"),
        "serve should direct callers to the sole database owner: {serve_error}"
    );

    let reopened = open_with_maintenance(&renamed, &client_profile, open_options)
        .await
        .unwrap();

    assert_eq!(reopened.store_layout().data_root, original_data_root);
    assert!(
        !client_profile
            .join("projects")
            .join(tracedecay::storage::default_profile_project_id(&renamed))
            .join("tracedecay.db")
            .exists(),
        "serve must not create or require a second path-hash profile shard"
    );
}

#[tokio::test]
async fn branch_open_rejects_a_mismatched_maintenance_profile() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let requested_profile = dir.path().join("requested-profile");
    let leased_profile = dir.path().join("leased-profile");
    fs::create_dir_all(&project).unwrap();
    prepare_maintenance_profile(&requested_profile);
    prepare_maintenance_profile(&leased_profile);
    let lifecycle = tracedecay::lifecycle_lease::acquire_exclusive_for_profile(
        &leased_profile,
        "mismatched branch fixture",
    )
    .unwrap();

    let error = match TraceDecay::open_branch_with_exclusive_maintenance(
        &project,
        "main",
        TraceDecayOpenOptions {
            profile_root: Some(requested_profile.clone()),
            global_db_path: Some(requested_profile.join("global.db")),
        },
        &lifecycle,
    )
    .await
    {
        Ok(_) => panic!("mismatched profile lease must be rejected"),
        Err(error) => error,
    };

    assert_eq!(
        error.to_string(),
        "config error: branch open requires the exact profile's exclusive lifecycle lease"
    );
}

#[tokio::test]
async fn trace_decay_open_branch_uses_profile_shard_branch_db() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let home = root.join("home");
    let profile_root = home.join(".tracedecay");
    let project = root.join("repo");
    let shard_root = profile_root.join("projects/proj_branch");
    let branch_db = shard_root.join("branches/feature_profile.db");
    fs::create_dir_all(branch_db.parent().unwrap()).unwrap();
    fs::create_dir_all(project.join(".tracedecay")).unwrap();
    run_git(&project, &["init"]);
    let _home_guard = HomeEnvGuard::set(&home);
    write_enrollment_marker(
        &project,
        &EnrollmentMarker {
            project_id: "proj_branch".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let config = TraceDecayConfig {
        root_dir: project.to_string_lossy().to_string(),
        ..TraceDecayConfig::default()
    };
    fs::write(
        shard_root.join("config.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();
    crate::common::initialize_test_database(&shard_root.join("tracedecay.db"))
        .await
        .unwrap();
    crate::common::initialize_test_database(&branch_db)
        .await
        .unwrap();
    let mut meta = BranchMeta::new_for_dir(&shard_root, "main");
    meta.add_branch("feature/profile", "branches/feature_profile.db", "main");
    branch_meta::save_branch_meta(&shard_root, &meta).unwrap();

    let cg = open_branch_with_maintenance(
        &project,
        "feature/profile",
        &profile_root,
        TraceDecayOpenOptions::default(),
    )
    .await
    .unwrap();

    assert_path_eq(&cg.store_layout().data_root, &shard_root);
    assert_path_eq(cg.db_path(), &branch_db);
    assert_eq!(cg.serving_branch(), Some("feature/profile"));
}

#[tokio::test]
async fn trace_decay_open_with_options_auto_tracks_branch_in_explicit_profile() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let daemon_home = root.join("daemon-home");
    let client_profile = root.join("client-profile");
    let project = root.join("repo");
    fs::create_dir_all(&project).unwrap();
    run_git(&project, &["init"]);
    run_git(&project, &["config", "user.email", "test@example.com"]);
    run_git(&project, &["config", "user.name", "TraceDecay Test"]);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/main.rs"), "fn main() {}\n").unwrap();
    run_git(&project, &["add", "."]);
    run_git(&project, &["commit", "-m", "initial"]);
    run_git(&project, &["checkout", "-b", "feature/client-profile"]);
    fs::write(
        project.join("src/main.rs"),
        "fn main() { println!(\"feature\"); }\n",
    )
    .unwrap();
    run_git(&project, &["add", "."]);
    run_git(&project, &["commit", "-m", "feature"]);
    run_git(&project, &["checkout", "-"]);

    let _home_guard = HomeEnvGuard::set(&daemon_home);
    write_enrollment_marker(
        &project,
        &EnrollmentMarker {
            project_id: "proj_auto_branch".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let open_options = TraceDecayOpenOptions {
        profile_root: Some(client_profile.clone()),
        global_db_path: Some(client_profile.join("global.db")),
    };
    let main = init_with_maintenance(&project, &client_profile, open_options.clone())
        .await
        .unwrap();
    let shard_root = main.store_layout().data_root.clone();
    assert_eq!(shard_root, client_profile.join("projects/proj_auto_branch"));
    drop(main);

    run_git(&project, &["checkout", "feature/client-profile"]);
    let cg = open_with_maintenance(&project, &client_profile, open_options)
        .await
        .unwrap();

    assert_eq!(cg.store_layout().data_root, shard_root);
    assert_eq!(cg.serving_branch(), Some("feature/client-profile"));
    assert!(cg.db_path().starts_with(shard_root.join("branches")));
    assert!(cg.db_path().is_file());
    assert!(
        !daemon_home.join(".tracedecay").exists(),
        "auto-tracking with explicit options must not create branch storage in the daemon/default profile"
    );
}
