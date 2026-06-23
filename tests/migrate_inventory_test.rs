use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use libsql::Builder;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use tempfile::TempDir;
use tracedecay::global_db::GlobalDb;
use tracedecay::migrate::inventory::{
    build_inventory, MigrationInventory, MigrationInventoryOptions, RegistryStatus, StoreArtifact,
    StoreBrand, StoreInventory, StoreRole, StoreStatus,
};
use tracedecay::migrate::manifest::{
    build_plan_manifest, load_manifest, save_manifest, MigrationManifest, MigrationPlanOptions,
    MigrationProtocol, StoreArtifactPath, StoreArtifactPathValidationError,
};

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

fn with_env_vars<T>(vars: &[(&str, Option<&Path>)], f: impl FnOnce() -> T) -> T {
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
    let previous = vars
        .iter()
        .map(|(name, _)| (*name, std::env::var_os(name)))
        .collect::<Vec<_>>();
    for (name, value) in vars {
        match value {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    for (name, value) in previous {
        if let Some(value) = value {
            std::env::set_var(name, value);
        } else {
            std::env::remove_var(name);
        }
    }
    match result {
        Ok(value) => value,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

fn inventory_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn same_path(left: &Path, right: &Path) -> bool {
    inventory_path(left) == inventory_path(right)
}

fn block_on_inventory(
    options: MigrationInventoryOptions,
) -> tracedecay::errors::Result<tracedecay::migrate::inventory::MigrationInventory> {
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(build_inventory(options))
}

fn make_project_store(root: &Path) {
    let data_dir = root.join(".tracedecay");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(data_dir.join("tracedecay.db"), b"not sqlite").unwrap();
}

async fn make_healthy_project_store(root: &Path) {
    let data_dir = root.join(".tracedecay");
    fs::create_dir_all(&data_dir).unwrap();
    let db = Builder::new_local(data_dir.join("tracedecay.db"))
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    conn.execute("CREATE TABLE health_check (id INTEGER PRIMARY KEY)", ())
        .await
        .unwrap();
}

fn single_ok_inventory(project: &Path, data_dir: &Path, graph_db: &Path) -> MigrationInventory {
    MigrationInventory {
        stores: vec![StoreInventory {
            project_root: project.to_path_buf(),
            data_dir: data_dir.to_path_buf(),
            db_path: graph_db.to_path_buf(),
            brand: StoreBrand::TraceDecay,
            role: StoreRole::CodeProjectStore,
            registry_status: RegistryStatus::Unregistered,
            size_bytes: 128,
            statuses: vec![StoreStatus::Ok],
            artifacts: vec![StoreArtifact {
                kind: "graph_db".to_string(),
                path: graph_db.to_path_buf(),
                size_bytes: 128,
            }],
        }],
        skipped: Vec::new(),
        global_db: None,
    }
}

#[test]
fn manifest_save_generates_token_and_records_protocol_context() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let data_dir = project.join(".tracedecay");
    let graph_db = data_dir.join("tracedecay.db");
    let profile_root = dir.path().join("profile");

    let manifest = build_plan_manifest(
        single_ok_inventory(&project, &data_dir, &graph_db),
        MigrationPlanOptions {
            manifest_path: dir.path().join("manifest.json"),
            migration_id: "mig_123".to_string(),
            tracedecay_version: "0.0.2".to_string(),
            created_at_unix: 1_800_000_000,
            confirmation_token: String::new(),
            target_profile_root: profile_root.clone(),
            project_id: "proj_123".to_string(),
        },
    )
    .unwrap();

    assert!(!manifest.confirmation_token.is_empty());
    assert!(manifest.confirmation_token.contains("mig_123"));
    assert!(manifest.command_args.is_empty());
    assert!(manifest.env_overrides.is_empty());
    assert_eq!(
        manifest.source.project_root.as_deref(),
        Some(project.as_path())
    );
    assert_eq!(
        manifest.source.data_dir.as_deref(),
        Some(data_dir.as_path())
    );
    assert_eq!(
        manifest.destination.profile_root.as_deref(),
        Some(profile_root.as_path())
    );
    assert_eq!(manifest.destination.project_id.as_deref(), Some("proj_123"));
    assert!(manifest.validation_summaries.is_empty());
}

#[test]
fn manifest_atomic_save_roundtrips_and_cleans_protocol_files() {
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let manifest_path = root.join("migration-manifest.json");
    let protocol = MigrationProtocol::for_manifest(&manifest_path, "mig_123");
    let manifest = MigrationManifest::new(
        "mig_123",
        "0.0.2",
        1_800_000_000,
        "confirm-mig_123",
        protocol.clone(),
        MigrationInventory {
            stores: Vec::new(),
            skipped: Vec::new(),
            global_db: None,
        },
    );

    save_manifest(&manifest).unwrap();
    let loaded = load_manifest(&manifest_path).unwrap();

    assert_eq!(loaded.migration_id, "mig_123");
    assert_eq!(loaded.confirmation_token, "confirm-mig_123");
    assert!(protocol.manifest_path.is_file());
    assert!(!protocol.temp_manifest_path.exists());
    assert!(!protocol.lock_path.exists());
}

#[test]
fn manifest_save_requires_confirmation_token() {
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let manifest_path = root.join("manifest.json");
    let protocol = MigrationProtocol::for_manifest(&manifest_path, "mig_123");
    let manifest = MigrationManifest::new(
        "mig_123",
        "0.0.2",
        1_800_000_000,
        "",
        protocol,
        MigrationInventory {
            stores: Vec::new(),
            skipped: Vec::new(),
            global_db: None,
        },
    );

    let err = save_manifest(&manifest).unwrap_err();

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("confirmation_token"));
    assert!(!manifest_path.exists());
}

#[test]
fn store_artifact_path_rejects_path_traversal() {
    let dir = TempDir::new().unwrap();

    let err =
        StoreArtifactPath::from_relative(dir.path(), Path::new("../outside.db"), 1024).unwrap_err();

    assert_eq!(err, StoreArtifactPathValidationError::PathTraversal);
}

#[cfg(unix)]
#[test]
fn store_artifact_path_rejects_symlinks() {
    let dir = TempDir::new().unwrap();
    let outside = dir.path().join("outside.db");
    let link = dir.path().join("link.db");
    fs::write(&outside, b"db").unwrap();
    symlink(&outside, &link).unwrap();

    let err = StoreArtifactPath::from_relative(dir.path(), Path::new("link.db"), 1024).unwrap_err();

    assert_eq!(err, StoreArtifactPathValidationError::Symlink);
}

#[tokio::test]
async fn inventory_does_not_open_or_recover_dirty_project_db() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    make_project_store(&root);
    fs::write(root.join(".tracedecay/dirty"), b"pid=1").unwrap();
    let db_path = root.join(".tracedecay/tracedecay.db");
    let before = fs::read(&db_path).unwrap();

    let report = build_inventory(MigrationInventoryOptions {
        roots: vec![dir.path().to_path_buf()],
        ..MigrationInventoryOptions::default()
    })
    .await
    .unwrap();

    assert_eq!(fs::read(&db_path).unwrap(), before);
    assert!(root.join(".tracedecay/dirty").exists());
    let store = report
        .stores
        .iter()
        .find(|store| store.project_root == root)
        .expect("project store should be inventoried");
    assert!(store.statuses.contains(&StoreStatus::Dirty));
    assert!(store.statuses.contains(&StoreStatus::Corrupt));
}

#[tokio::test]
async fn inventory_records_project_store_sidecar_artifacts() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("repo");
    let data_dir = root.join(".tracedecay");
    fs::create_dir_all(&root).unwrap();
    make_project_store(&root);
    fs::write(data_dir.join("sessions.db"), b"sessions").unwrap();
    fs::write(data_dir.join("branch-meta.json"), b"{}").unwrap();
    fs::write(data_dir.join("config.json"), b"{}").unwrap();
    fs::write(data_dir.join("store_manifest.json"), b"{}").unwrap();

    let report = build_inventory(MigrationInventoryOptions {
        roots: vec![dir.path().to_path_buf()],
        ..MigrationInventoryOptions::default()
    })
    .await
    .unwrap();

    let store = report
        .stores
        .iter()
        .find(|store| store.project_root == root)
        .expect("project store should be inventoried");
    let kinds = store
        .artifacts
        .iter()
        .map(|artifact| artifact.kind.as_str())
        .collect::<Vec<_>>();

    for kind in [
        "graph_db",
        "sessions_db",
        "branch_meta",
        "config",
        "store_manifest",
    ] {
        assert!(kinds.contains(&kind), "{kind} missing from {kinds:?}");
    }
}

#[tokio::test]
async fn inventory_records_branch_graph_db_and_marks_corrupt_when_quick_check_fails() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("repo");
    let branches_dir = root.join(".tracedecay/branches");
    let branch_db = branches_dir.join("feature.db");
    fs::create_dir_all(&root).unwrap();
    make_healthy_project_store(&root).await;
    fs::create_dir_all(&branches_dir).unwrap();
    fs::write(&branch_db, b"not sqlite").unwrap();
    fs::write(branches_dir.join("feature.db-wal"), b"wal").unwrap();
    fs::write(branches_dir.join("feature.db-shm"), b"shm").unwrap();

    let report = build_inventory(MigrationInventoryOptions {
        roots: vec![dir.path().to_path_buf()],
        ..MigrationInventoryOptions::default()
    })
    .await
    .unwrap();

    let store = report
        .stores
        .iter()
        .find(|store| store.project_root == root)
        .expect("project store should be inventoried");
    let artifact = store
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == "branch_graph_db")
        .expect("branch DB artifact should be recorded");

    assert!(same_path(&artifact.path, &branch_db));
    assert_eq!(artifact.size_bytes, fs::metadata(&branch_db).unwrap().len());
    assert!(store.artifacts.iter().any(|artifact| {
        artifact.kind == "branch_graph_db_wal"
            && same_path(&artifact.path, &branches_dir.join("feature.db-wal"))
            && artifact.size_bytes == 3
    }));
    assert!(store.artifacts.iter().any(|artifact| {
        artifact.kind == "branch_graph_db_shm"
            && same_path(&artifact.path, &branches_dir.join("feature.db-shm"))
            && artifact.size_bytes == 3
    }));
    assert_eq!(store.statuses, vec![StoreStatus::Corrupt]);
}

#[cfg(unix)]
#[tokio::test]
async fn inventory_skips_symlinked_branches_dir_by_default() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("repo");
    let real_branches = dir.path().join("outside_branches");
    let branch_db = real_branches.join("feature.db");
    fs::create_dir_all(&root).unwrap();
    make_healthy_project_store(&root).await;
    fs::create_dir_all(&real_branches).unwrap();
    fs::write(&branch_db, b"not sqlite").unwrap();
    symlink(&real_branches, root.join(".tracedecay/branches")).unwrap();

    let report = build_inventory(MigrationInventoryOptions {
        roots: vec![dir.path().to_path_buf()],
        follow_symlinks: false,
        ..MigrationInventoryOptions::default()
    })
    .await
    .unwrap();

    let store = report
        .stores
        .iter()
        .find(|store| store.project_root == root)
        .expect("project store should be inventoried");

    assert_eq!(store.statuses, vec![StoreStatus::Ok]);
    assert!(!store
        .artifacts
        .iter()
        .any(|artifact| artifact.kind == "branch_graph_db"));
    assert!(report.skipped.iter().any(|skipped| {
        skipped.path == root.join(".tracedecay/branches") && skipped.reason == "symlink"
    }));
}

#[tokio::test]
async fn inventory_reports_global_db_metadata() {
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let db_path = root.join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    let project = root.join("registered");
    fs::create_dir_all(&project).unwrap();
    db.upsert(&project, 42).await;
    assert!(db.ensure_token_count_cache().await);
    drop(db);

    let report = build_inventory(MigrationInventoryOptions {
        roots: Vec::new(),
        global_db_path: Some(db_path.clone()),
        ..MigrationInventoryOptions::default()
    })
    .await
    .unwrap();

    let global = report
        .global_db
        .expect("global DB metadata should be present");
    assert_eq!(global.path, db_path);
    assert_eq!(global.project_count, 1);
    assert_eq!(
        global
            .registered_project_paths
            .iter()
            .map(|path| inventory_path(path))
            .collect::<Vec<_>>(),
        vec![inventory_path(&project)]
    );
    assert!(global.token_cache_present);
    assert!(global.path_overridden);
    assert!(global.warnings.is_empty());
}

#[tokio::test]
async fn inventory_discovers_registered_project_outside_scan_roots() {
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let db_path = root.join("global.db");
    let registered = root.join("registered");
    fs::create_dir_all(&registered).unwrap();
    make_project_store(&registered);
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    db.upsert(&registered, 42).await;
    drop(db);

    let report = build_inventory(MigrationInventoryOptions {
        roots: Vec::new(),
        global_db_path: Some(db_path),
        ..MigrationInventoryOptions::default()
    })
    .await
    .unwrap();

    let store = report
        .stores
        .iter()
        .find(|store| same_path(&store.project_root, &registered))
        .expect("registered project store should be inventoried");
    assert_eq!(store.registry_status, RegistryStatus::Registered);
}

#[test]
fn explicit_roots_do_not_inventory_unrelated_registered_projects_by_default() {
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let db_path = root.join("global.db");
    let scan_root = root.join("scan-root");
    let discovered = scan_root.join("discovered");
    let unrelated = root.join("unrelated-registered");
    fs::create_dir_all(&discovered).unwrap();
    fs::create_dir_all(&unrelated).unwrap();
    make_project_store(&discovered);
    make_project_store(&unrelated);
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db = GlobalDb::open_at(&db_path).await.unwrap();
        db.upsert(&discovered, 42).await;
        db.upsert(&unrelated, 99).await;
    });

    let report = with_env_vars(&[("HERMES_HOME", None), ("HOME", Some(&root))], || {
        block_on_inventory(MigrationInventoryOptions {
            roots: vec![scan_root],
            global_db_path: Some(db_path),
            ..MigrationInventoryOptions::default()
        })
        .unwrap()
    });

    assert_eq!(
        report.stores.len(),
        1,
        "unexpected stores: {:?}",
        report
            .stores
            .iter()
            .map(|store| (&store.project_root, &store.role, &store.registry_status))
            .collect::<Vec<_>>()
    );
    let store = report
        .stores
        .iter()
        .find(|store| same_path(&store.project_root, &discovered))
        .expect("discovered store should be inventoried");
    assert_eq!(store.registry_status, RegistryStatus::Registered);
    assert!(!report
        .stores
        .iter()
        .any(|store| same_path(&store.project_root, &unrelated)));
}

#[test]
fn explicit_roots_can_include_all_registered_projects_when_requested() {
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let db_path = root.join("global.db");
    let scan_root = root.join("scan-root");
    let discovered = scan_root.join("discovered");
    let unrelated = root.join("unrelated-registered");
    fs::create_dir_all(&discovered).unwrap();
    fs::create_dir_all(&unrelated).unwrap();
    make_project_store(&discovered);
    make_project_store(&unrelated);
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db = GlobalDb::open_at(&db_path).await.unwrap();
        db.upsert(&discovered, 42).await;
        db.upsert(&unrelated, 99).await;
    });

    let report = with_env_vars(&[("HERMES_HOME", None), ("HOME", Some(&root))], || {
        block_on_inventory(MigrationInventoryOptions {
            roots: vec![scan_root],
            global_db_path: Some(db_path),
            include_all_registered: true,
            ..MigrationInventoryOptions::default()
        })
        .unwrap()
    });

    assert!(report
        .stores
        .iter()
        .any(|store| same_path(&store.project_root, &discovered)
            && store.registry_status == RegistryStatus::Registered));
    assert!(report
        .stores
        .iter()
        .any(|store| same_path(&store.project_root, &unrelated)
            && store.registry_status == RegistryStatus::Registered));
}

#[tokio::test]
async fn inventory_reports_registered_project_with_missing_local_store() {
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let db_path = root.join("global.db");
    let registered = root.join("registered_missing");
    fs::create_dir_all(&registered).unwrap();
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    db.upsert(&registered, 42).await;
    drop(db);

    let report = build_inventory(MigrationInventoryOptions {
        roots: Vec::new(),
        global_db_path: Some(db_path),
        ..MigrationInventoryOptions::default()
    })
    .await
    .unwrap();

    let store = report
        .stores
        .iter()
        .find(|store| same_path(&store.project_root, &registered))
        .expect("registered missing project should still be inventoried");
    assert_eq!(store.registry_status, RegistryStatus::Registered);
    assert!(store.statuses.contains(&StoreStatus::MissingDb));
}

#[tokio::test]
async fn inventory_warns_instead_of_failing_on_unreadable_global_db() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    fs::write(&db_path, b"not sqlite").unwrap();

    let report = build_inventory(MigrationInventoryOptions {
        roots: Vec::new(),
        global_db_path: Some(db_path),
        ..MigrationInventoryOptions::default()
    })
    .await
    .unwrap();

    let global = report.global_db.expect("global DB metadata should exist");
    assert!(global.exists);
    assert_eq!(global.project_count, 0);
    assert!(!global.warnings.is_empty());
}

#[tokio::test]
async fn inventory_flags_leftover_config_tmp_for_manual_review() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    make_project_store(&root);
    fs::write(root.join(".tracedecay/config.json.tmp"), b"partial config").unwrap();

    let report = build_inventory(MigrationInventoryOptions {
        roots: vec![dir.path().to_path_buf()],
        ..MigrationInventoryOptions::default()
    })
    .await
    .unwrap();

    let store = report
        .stores
        .iter()
        .find(|store| store.project_root == root)
        .expect("store should be inventoried");
    assert!(store.statuses.contains(&StoreStatus::NeedsManualReview));
    assert!(store
        .artifacts
        .iter()
        .any(|artifact| artifact.kind == "config_tmp"));
}

#[cfg(unix)]
#[tokio::test]
async fn inventory_reports_skipped_symlink_directories() {
    let dir = TempDir::new().unwrap();
    let real = dir.path().join("real_project");
    fs::create_dir_all(&real).unwrap();
    make_project_store(&real);
    let alias = dir.path().join("alias_project");
    std::os::unix::fs::symlink(&real, &alias).unwrap();

    let report = build_inventory(MigrationInventoryOptions {
        roots: vec![dir.path().to_path_buf()],
        follow_symlinks: false,
        ..MigrationInventoryOptions::default()
    })
    .await
    .unwrap();

    assert!(report
        .skipped
        .iter()
        .any(|skipped| skipped.path == alias && skipped.reason == "symlink"));
}

#[cfg(unix)]
#[tokio::test]
async fn inventory_skips_symlinked_data_dir_by_default() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("repo");
    let real_data = dir.path().join("real_data");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&real_data).unwrap();
    fs::write(real_data.join("tracedecay.db"), b"not sqlite").unwrap();
    std::os::unix::fs::symlink(&real_data, root.join(".tracedecay")).unwrap();

    let report = build_inventory(MigrationInventoryOptions {
        roots: vec![dir.path().to_path_buf()],
        follow_symlinks: false,
        ..MigrationInventoryOptions::default()
    })
    .await
    .unwrap();

    assert!(!report.stores.iter().any(|store| store.project_root == root));
    assert!(report.skipped.iter().any(|skipped| {
        skipped.path == root.join(".tracedecay") && skipped.reason == "symlink"
    }));
}

#[test]
fn inventory_discovers_hermes_home_profiles_and_state_dbs() {
    let dir = TempDir::new().unwrap();
    let hermes_home = dir.path().join("custom-hermes");
    let default_store = hermes_home.join(".tracedecay");
    let work_profile = hermes_home.join("profiles/work");
    let work_store = work_profile.join(".tracedecay");
    fs::create_dir_all(&default_store).unwrap();
    fs::create_dir_all(&work_store).unwrap();
    fs::write(default_store.join("tracedecay.db"), b"not sqlite").unwrap();
    fs::write(hermes_home.join("state.db"), b"not sqlite").unwrap();
    fs::write(work_store.join("tracedecay.db"), b"not sqlite").unwrap();
    fs::write(work_profile.join("state.db"), b"not sqlite").unwrap();

    let report = with_env_vars(&[("HERMES_HOME", Some(&hermes_home))], || {
        block_on_inventory(MigrationInventoryOptions {
            roots: Vec::new(),
            ..MigrationInventoryOptions::default()
        })
        .unwrap()
    });

    let default = report
        .stores
        .iter()
        .find(|store| store.data_dir == default_store)
        .expect("default Hermes profile store should be inventoried");
    assert_eq!(default.role, StoreRole::HermesProfileStore);
    assert_eq!(default.brand, StoreBrand::TraceDecay);

    let work = report
        .stores
        .iter()
        .find(|store| store.data_dir == work_store)
        .expect("named Hermes profile store should be inventoried");
    assert_eq!(work.role, StoreRole::HermesProfileStore);
    assert_eq!(work.brand, StoreBrand::TraceDecay);

    assert!(report.stores.iter().any(|store| {
        store.role == StoreRole::HermesStateDbSource
            && store.db_path == hermes_home.join("state.db")
    }));
    assert!(report.stores.iter().any(|store| {
        store.role == StoreRole::HermesStateDbSource
            && store.db_path == work_profile.join("state.db")
    }));
}

#[test]
fn inventory_discovers_default_home_hermes_project_pin() {
    let dir = TempDir::new().unwrap();
    let hermes_home = dir.path().join(".hermes");
    let pinned_project = dir.path().join("pinned-project");
    fs::create_dir_all(&hermes_home).unwrap();
    fs::create_dir_all(&pinned_project).unwrap();
    fs::write(
        hermes_home.join("config.yaml"),
        format!(
            "plugins:\n  tracedecay:\n    project_root: '{}'\n",
            pinned_project.display()
        ),
    )
    .unwrap();
    make_project_store(&pinned_project);

    let report = with_env_vars(&[("HERMES_HOME", None), ("HOME", Some(dir.path()))], || {
        block_on_inventory(MigrationInventoryOptions {
            roots: Vec::new(),
            ..MigrationInventoryOptions::default()
        })
        .unwrap()
    });

    let store = report
        .stores
        .iter()
        .find(|store| store.project_root == pinned_project)
        .expect("Hermes config project_root pin should be inventoried");
    assert_eq!(store.role, StoreRole::CodeProjectStore);
    assert_eq!(store.brand, StoreBrand::TraceDecay);
}
