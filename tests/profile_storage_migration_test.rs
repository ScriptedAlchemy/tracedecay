use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tracedecay::branch::BranchAddOutcome;
use tracedecay::branch_meta::{self, BranchMeta};
use tracedecay::config::{TraceDecayConfig, USER_DATA_DIR_ENV};
use tracedecay::db::Database;
use tracedecay::global_db::{GlobalDb, GraphScopeUpsert, StoreArtifactUpsert, StoreInstanceUpsert};
use tracedecay::migrate::inventory::{
    MigrationInventory, RegistryStatus, StoreArtifact, StoreBrand, StoreInventory, StoreRole,
    StoreStatus,
};
use tracedecay::migrate::manifest::{
    apply_migration_manifest, build_plan_manifest, finalize_migration_apply,
    verify_migration_manifest, MigrationPlanOptions,
};
use tracedecay::migrate::registry::{
    apply_registry_reconstruction_report, reconstruct_registry_from_store_manifest,
    scan_profile_store_manifests,
};
use tracedecay::serve;
use tracedecay::sessions::cursor::open_project_session_db;
use tracedecay::storage::{
    read_enrollment_marker, write_enrollment_marker, EnrollmentMarker, StorageMode, StoreKind,
    StoreManifest, STORE_MANIFEST_FILENAME, STORE_MANIFEST_SCHEMA_VERSION,
};
use tracedecay::tracedecay::{TraceDecay, TraceDecayOpenOptions};

static HOME_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct HomeEnvGuard {
    previous_home: Option<OsString>,
    previous_userprofile: Option<OsString>,
    previous_data_dir: Option<OsString>,
}

impl HomeEnvGuard {
    fn set(home: &Path) -> Self {
        let previous_home = std::env::var_os("HOME");
        let previous_userprofile = std::env::var_os("USERPROFILE");
        let previous_data_dir = std::env::var_os(USER_DATA_DIR_ENV);
        std::env::set_var("HOME", home);
        std::env::set_var("USERPROFILE", home);
        std::env::set_var(USER_DATA_DIR_ENV, home.join(".tracedecay"));
        Self {
            previous_home,
            previous_userprofile,
            previous_data_dir,
        }
    }
}

impl Drop for HomeEnvGuard {
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

async fn table_exists(db_path: &std::path::Path, table: &str) -> bool {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            libsql::params![table],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().is_some()
}

fn write_profile_store_manifest(profile_root: &Path, project_root: &Path) -> std::path::PathBuf {
    let data_root = profile_root.join("projects/proj_123");
    fs::create_dir_all(&data_root).unwrap();
    fs::create_dir_all(project_root).unwrap();
    fs::write(data_root.join("tracedecay.db"), b"graph").unwrap();
    fs::write(data_root.join("sessions.db"), b"sessions").unwrap();
    let branch_meta = BranchMeta::new_for_dir(&data_root, "main");
    branch_meta::save_branch_meta(&data_root, &branch_meta).unwrap();
    let manifest = StoreManifest {
        schema_version: STORE_MANIFEST_SCHEMA_VERSION,
        project_id: Some("proj_123".to_string()),
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
    GlobalDb::open_at(&db_path).await.unwrap();

    for table in [
        "code_projects",
        "project_aliases",
        "store_instances",
        "graph_scopes",
        "store_artifacts",
    ] {
        assert!(table_exists(&db_path, table).await, "{table} missing");
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
    assert_eq!(plan.project.project_id, "proj_123");
    assert_eq!(plan.project.project_root, project_root);
    assert_eq!(plan.project.aliases, vec![project_root]);
    assert_eq!(plan.store.project_id, "proj_123");
    assert_eq!(plan.store.store_kind, "code_project");
    assert_eq!(plan.store.storage_mode, "profile_sharded");
    assert_eq!(plan.store.store_relpath, "projects/proj_123");
    assert_eq!(
        plan.store.manifest_relpath.as_deref().map(portable_relpath),
        Some("projects/proj_123/store_manifest.json".to_string())
    );
    assert_eq!(plan.store.last_verified_at, Some(1_800_000_001));
    assert!(plan
        .artifacts
        .iter()
        .any(|artifact| artifact.artifact_kind == "graph_db"
            && portable_relpath(&artifact.relpath) == "projects/proj_123/tracedecay.db"));
    assert!(plan
        .artifacts
        .iter()
        .any(|artifact| artifact.artifact_kind == "store_manifest"
            && portable_relpath(&artifact.relpath) == "projects/proj_123/store_manifest.json"));
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
    assert!(report
        .issues
        .iter()
        .any(|issue| issue.contains("unsafe graph_db_relpath")));
}

#[tokio::test]
async fn registry_resolves_project_store_by_canonical_alias() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let project_root = dir.path().join("repo");
    fs::create_dir_all(&project_root).unwrap();
    let db = GlobalDb::open_at(&db_path).await.unwrap();

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
    let db_path = dir.path().join("global.db");
    let project_root = dir.path().join("repo");
    fs::create_dir_all(&project_root).unwrap();
    let db = GlobalDb::open_at(&db_path).await.unwrap();

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

    apply_migration_manifest(&mut manifest).unwrap();
    let staged = verify_migration_manifest(&manifest);
    assert!(staged.cutover_ready);
    assert!(!staged.apply_supported);
    assert!(read_enrollment_marker(&project).unwrap().is_none());

    let db = GlobalDb::open_at(&root.join("global.db")).await.unwrap();
    apply_registry_reconstruction_report(&db, &staged.registry_reconstruction)
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
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .unwrap();

    let applied = apply_registry_reconstruction_report(&db, &report)
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
    GlobalDb::open_at(&session_db).await.unwrap();
    let _home_guard = HomeEnvGuard::set(&home);
    let report =
        reconstruct_registry_from_store_manifest(&manifest_path, &profile_root, 1_800_000_001);
    let global = GlobalDb::open_at(&profile_root.join("global.db"))
        .await
        .unwrap();
    apply_registry_reconstruction_report(&global, &report)
        .await
        .unwrap();

    let db = open_project_session_db(&project_root).await;

    assert!(
        db.is_some(),
        "session ingest should open the registry-backed profile session DB"
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

    let cg = TraceDecay::init(&project).await.unwrap();

    assert_eq!(cg.store_layout().data_root, shard_root);
    assert_eq!(cg.db_path(), shard_root.join("tracedecay.db"));
    assert!(shard_root.join("config.json").is_file());
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

    let cg = TraceDecay::init_with_options(&project, open_options.clone())
        .await
        .unwrap();

    assert_eq!(
        cg.store_layout().data_root,
        client_profile.join("projects/proj_explicit")
    );
    assert!(cg.store_layout().config_path.is_file());
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

    let cg = TraceDecay::init_with_options(&project, open_options.clone())
        .await
        .unwrap();

    assert_eq!(
        cg.store_layout().data_root,
        client_profile.join("projects/proj_db_only")
    );
    assert!(cg.store_layout().config_path.is_file());
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

    let initialized = TraceDecay::init(&project).await.unwrap();
    let original_project_id = initialized
        .store_layout()
        .identity
        .project_id
        .clone()
        .unwrap();
    let original_data_root = initialized.store_layout().data_root.clone();
    drop(initialized);
    fs::rename(&project, &renamed).unwrap();

    let reopened = TraceDecay::open(&renamed).await.unwrap();

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
async fn ensure_initialized_with_options_uses_registered_remote_store() {
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

    let initialized = TraceDecay::init_with_options(&project, open_options.clone())
        .await
        .unwrap();
    let original_data_root = initialized.store_layout().data_root.clone();
    drop(initialized);
    fs::rename(&project, &renamed).unwrap();

    assert!(
        !TraceDecay::is_initialized_with_options(&renamed, &open_options),
        "the synchronous marker check cannot see renamed registered stores"
    );
    let reopened = serve::ensure_initialized_with_options(&renamed, open_options)
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
    Database::initialize(&shard_root.join("tracedecay.db"))
        .await
        .unwrap();
    Database::initialize(&branch_db).await.unwrap();
    let mut meta = BranchMeta::new_for_dir(&shard_root, "main");
    meta.add_branch("feature/profile", "branches/feature_profile.db", "main");
    branch_meta::save_branch_meta(&shard_root, &meta).unwrap();

    let cg = TraceDecay::open_branch(&project, "feature/profile")
        .await
        .unwrap();

    assert_eq!(cg.store_layout().data_root, shard_root);
    assert_eq!(cg.db_path(), branch_db);
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
    let main = TraceDecay::init_with_options(&project, open_options.clone())
        .await
        .unwrap();
    let shard_root = main.store_layout().data_root.clone();
    assert_eq!(shard_root, client_profile.join("projects/proj_auto_branch"));
    drop(main);

    run_git(&project, &["checkout", "feature/client-profile"]);
    let cg = TraceDecay::open_with_options(&project, open_options)
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
