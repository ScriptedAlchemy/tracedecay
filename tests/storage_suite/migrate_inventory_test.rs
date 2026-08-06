use std::fs;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use fs2::FileExt;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use tempfile::TempDir;
use tracedecay::application::host_admission::HostAdmissionTestRuntimeV1;
// `build_inventory` is deliberately absent: it is reachable only through
// `build_inventory_in`/`block_on_inventory_in`, which require an owned profile.
use tracedecay::migrate::inventory::{
    InventoryIntegrityMode, InventoryStoreAuthority, MigrationInventory, MigrationInventoryOptions,
    RegistryStatus, SqliteIntegrityOutcome, StoreBrand, StoreRole, StoreStatus,
};

use crate::common::EnvVarGuard;
use crate::common::fixture::TestProfile;

/// One throwaway profile per test, taken from the shared fixture authority: it
/// pins `HOME`, the profile data directory, and the global-DB path inside a
/// private temporary directory.
///
/// Inventory takes an *exclusive* lifecycle lease on the profile it is about to
/// inspect, so two tests that resolve the same profile root can never both run:
/// the loser fails with "another lifecycle operation is already active". These
/// tests used to inspect whichever profile the ambient environment named — one
/// shared root per checkout — so they collided as soon as anything stopped
/// handing each test a private directory. Owning a profile per test gives every
/// test its own lease by construction, under any runner.
///
/// Profile resolution still reads process-global environment, which threads
/// cannot hold different values of. Under a process-per-test runner that is
/// moot. Under single-process `cargo test` a sibling module that restores `HOME`
/// mid-test can still redirect resolution, which is why
/// [`prepare_inventory_profile`] refuses to proceed unless the root about to be
/// leased is this test's own.
struct InventoryProfile {
    // Fields drop in declaration order, so `HERMES_HOME` is restored before the
    // profile releases the environment it pinned.
    _hermes_home: EnvVarGuard,
    profile: TestProfile,
}

impl InventoryProfile {
    async fn acquire() -> Self {
        Self::isolate(TestProfile::acquire().await)
    }

    /// Counterpart of [`Self::acquire`] for plain `#[test]` fns; call it outside
    /// any runtime the test later builds, because the fixture lock blocks.
    fn acquire_blocking() -> Self {
        Self::isolate(TestProfile::acquire_blocking())
    }

    /// Removes `HERMES_HOME` so no operator-configured agent host leaks into the
    /// production code these tests drive. Inventory's own Hermes scan resolves
    /// from `HOME`, which the profile already owns; this covers the rest.
    fn isolate(profile: TestProfile) -> Self {
        Self {
            _hermes_home: EnvVarGuard::unset("HERMES_HOME"),
            profile,
        }
    }

    fn home(&self) -> &Path {
        self.profile.home()
    }

    /// This profile's root, which is what inventory leases when a test does not
    /// name a global DB of its own.
    fn root(&self) -> &Path {
        self.profile.root()
    }

    /// Creates `<scratch>/<name>`: a fixture tree beside this profile rather
    /// than inside it, so scanning it never walks the profile's own store.
    fn path(&self, name: impl AsRef<Path>) -> PathBuf {
        self.profile.path(name)
    }
}

/// Inventories `options` after proving the profile it will lease belongs to this
/// test.
///
/// Taking the profile by reference is the point: a test cannot ask for an
/// inventory without first owning an isolated profile, and the assertion turns a
/// silent regression — inventory resolving some shared ambient profile again —
/// into a named failure instead of a lease-contention flake.
async fn build_inventory_in(
    profile: &InventoryProfile,
    options: MigrationInventoryOptions,
) -> tracedecay::errors::Result<MigrationInventory> {
    prepare_inventory_profile(profile, &options);
    tracedecay::migrate::inventory::build_inventory(options).await
}

/// [`build_inventory_in`] for plain `#[test]` fns.
fn block_on_inventory_in(
    profile: &InventoryProfile,
    options: MigrationInventoryOptions,
) -> tracedecay::errors::Result<MigrationInventory> {
    prepare_inventory_profile(profile, &options);
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(tracedecay::migrate::inventory::build_inventory(options))
}

fn canonical_temp_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn inventory_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn same_path(left: &Path, right: &Path) -> bool {
    inventory_path(left) == inventory_path(right)
}

fn prepare_inventory_profile(profile: &InventoryProfile, options: &MigrationInventoryOptions) {
    // The same resolution inventory performs, so the directory it leases exists
    // and is owner-only before it gets there.
    let profile_root = options
        .global_db_path
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| tracedecay::storage::default_profile_root().unwrap());
    if options.global_db_path.is_none() {
        assert_eq!(
            profile_root,
            profile.root(),
            "inventory must lease this test's own profile, not a shared one"
        );
    }
    fs::create_dir_all(&profile_root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(profile_root, fs::Permissions::from_mode(0o700)).unwrap();
    }
}

fn make_project_store(root: &Path) {
    let data_dir = root.join(".tracedecay");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(data_dir.join("tracedecay.db"), b"not sqlite").unwrap();
}

async fn make_healthy_project_store(root: &Path) {
    let data_dir = root.join(".tracedecay");
    fs::create_dir_all(&data_dir).unwrap();
    let conn = rusqlite::Connection::open(data_dir.join("tracedecay.db")).unwrap();
    conn.execute("CREATE TABLE health_check (id INTEGER PRIMARY KEY)", [])
        .unwrap();
}

fn sqlite_table_exists(path: &Path, table: &str) -> bool {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name=?1",
        rusqlite::params![table],
        |row| row.get::<_, i64>(0),
    )
    .unwrap()
        > 0
}

fn make_damaged_sqlite(path: &Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute(
        "CREATE TABLE damaged_facts (id INTEGER PRIMARY KEY, value TEXT NOT NULL)",
        [],
    )
    .unwrap();
    for id in 0..256 {
        conn.execute(
            "INSERT INTO damaged_facts (id, value) VALUES (?1, ?2)",
            rusqlite::params![id, format!("fact-{id:04}-{}", "x".repeat(64))],
        )
        .unwrap();
    }
    let root_page = conn
        .query_row(
            "SELECT rootpage FROM sqlite_schema WHERE name = 'damaged_facts'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap() as usize;
    let page_size = conn
        .query_row("PRAGMA page_size", [], |row| row.get::<_, i64>(0))
        .unwrap() as usize;
    drop(conn);

    let mut bytes = fs::read(path).unwrap();
    bytes[(root_page - 1) * page_size] = 0xff;
    fs::write(path, bytes).unwrap();
}

#[tokio::test]
async fn inventory_does_not_open_or_recover_dirty_project_db() {
    let profile = InventoryProfile::acquire().await;
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    make_project_store(&root);
    fs::write(root.join(".tracedecay/dirty"), b"pid=1").unwrap();
    let db_path = root.join(".tracedecay/tracedecay.db");
    let before = fs::read(&db_path).unwrap();

    let report = build_inventory_in(
        &profile,
        MigrationInventoryOptions {
            roots: vec![dir.path().to_path_buf()],
            ..MigrationInventoryOptions::default()
        },
    )
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
    assert!(store.statuses.iter().any(|status| {
        matches!(
            status,
            StoreStatus::IntegrityIssue {
                path,
                authority: InventoryStoreAuthority::Authoritative,
                outcome: SqliteIntegrityOutcome::Damaged { .. },
            } if same_path(path, &db_path)
        )
    }));
}

#[tokio::test]
async fn inventory_records_project_store_sidecar_artifacts() {
    let profile = InventoryProfile::acquire().await;
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("repo");
    let data_dir = root.join(".tracedecay");
    fs::create_dir_all(&root).unwrap();
    make_project_store(&root);
    fs::write(data_dir.join("sessions.db"), b"sessions").unwrap();
    fs::write(data_dir.join("tracedecay.db-wal"), b"").unwrap();
    fs::write(data_dir.join("tracedecay.db-shm"), b"").unwrap();
    fs::write(data_dir.join("sessions.db-wal"), b"session wal").unwrap();
    fs::write(data_dir.join("sessions.db-shm"), b"session shm").unwrap();
    fs::write(data_dir.join("branch-meta.json"), b"{}").unwrap();
    fs::write(data_dir.join("config.json"), b"{}").unwrap();
    fs::write(data_dir.join("store_manifest.json"), b"{}").unwrap();

    let report = build_inventory_in(
        &profile,
        MigrationInventoryOptions {
            roots: vec![dir.path().to_path_buf()],
            ..MigrationInventoryOptions::default()
        },
    )
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
        "graph_db_wal",
        "graph_db_shm",
        "sessions_db",
        "sessions_db_wal",
        "sessions_db_shm",
        "branch_meta",
        "config",
        "store_manifest",
    ] {
        assert!(kinds.contains(&kind), "{kind} missing from {kinds:?}");
    }
}

#[tokio::test]
async fn damaged_stale_branch_is_attributed_without_condemning_authoritative_store() {
    let profile = InventoryProfile::acquire().await;
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("repo");
    let branches_dir = root.join(".tracedecay/branches");
    let branch_db = branches_dir.join("feature.db");
    fs::create_dir_all(&root).unwrap();
    make_healthy_project_store(&root).await;
    fs::create_dir_all(&branches_dir).unwrap();
    make_damaged_sqlite(&branch_db);
    fs::write(branches_dir.join("feature.db-wal"), b"wal").unwrap();
    fs::write(branches_dir.join("feature.db-shm"), b"shm").unwrap();

    let report = build_inventory_in(
        &profile,
        MigrationInventoryOptions {
            roots: vec![dir.path().to_path_buf()],
            ..MigrationInventoryOptions::default()
        },
    )
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
    assert!(!store.statuses.contains(&StoreStatus::Corrupt));
    assert!(
        store.statuses.iter().any(|status| {
            matches!(
                status,
                StoreStatus::IntegrityIssue {
                    path,
                    authority: InventoryStoreAuthority::StaleBranch,
                    outcome: SqliteIntegrityOutcome::Damaged { details },
                } if same_path(path, &branch_db) && !details.is_empty()
            )
        }),
        "{:?}",
        store.statuses
    );
    assert!(!store.statuses.iter().any(|status| {
        matches!(
            status,
            StoreStatus::IntegrityIssue {
                authority: InventoryStoreAuthority::Authoritative,
                ..
            }
        )
    }));
}

#[tokio::test]
async fn metadata_only_inventory_marks_existing_databases_unchecked() {
    let profile = InventoryProfile::acquire().await;
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("repo");
    let branches_dir = root.join(".tracedecay/branches");
    fs::create_dir_all(&branches_dir).unwrap();
    fs::write(root.join(".tracedecay/tracedecay.db"), b"not sqlite").unwrap();
    fs::write(branches_dir.join("feature.db"), b"not sqlite").unwrap();

    let report = build_inventory_in(
        &profile,
        MigrationInventoryOptions {
            roots: vec![dir.path().to_path_buf()],
            integrity: InventoryIntegrityMode::MetadataOnly,
            ..MigrationInventoryOptions::default()
        },
    )
    .await
    .unwrap();
    let store = report
        .stores
        .iter()
        .find(|store| store.project_root == root)
        .unwrap();

    assert_eq!(store.statuses, vec![StoreStatus::IntegrityUnchecked]);
    assert!(
        store
            .artifacts
            .iter()
            .any(|artifact| artifact.kind == "branch_graph_db")
    );
}

#[tokio::test]
async fn inventory_reports_only_actively_held_sync_locks_as_locked() {
    let profile = InventoryProfile::acquire().await;
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    make_healthy_project_store(&root).await;
    let lock_path = root.join(".tracedecay/sync.lock");
    fs::write(&lock_path, b"").unwrap();

    let idle = build_inventory_in(
        &profile,
        MigrationInventoryOptions {
            roots: vec![dir.path().to_path_buf()],
            ..MigrationInventoryOptions::default()
        },
    )
    .await
    .unwrap();
    let idle_store = idle
        .stores
        .iter()
        .find(|store| store.project_root == root)
        .unwrap();
    assert_eq!(idle_store.statuses, vec![StoreStatus::Ok]);
    assert!(
        idle_store
            .artifacts
            .iter()
            .any(|artifact| artifact.kind == "sync_lock")
    );

    let held = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    held.try_lock_exclusive().unwrap();
    let locked = build_inventory_in(
        &profile,
        MigrationInventoryOptions {
            roots: vec![dir.path().to_path_buf()],
            ..MigrationInventoryOptions::default()
        },
    )
    .await
    .unwrap();
    let locked_store = locked
        .stores
        .iter()
        .find(|store| store.project_root == root)
        .unwrap();
    assert_eq!(locked_store.statuses, vec![StoreStatus::Locked]);
}

#[cfg(unix)]
#[tokio::test]
async fn inventory_skips_symlinked_branches_dir_by_default() {
    let profile = InventoryProfile::acquire().await;
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("repo");
    let real_branches = dir.path().join("outside_branches");
    let branch_db = real_branches.join("feature.db");
    fs::create_dir_all(&root).unwrap();
    make_healthy_project_store(&root).await;
    fs::create_dir_all(&real_branches).unwrap();
    fs::write(&branch_db, b"not sqlite").unwrap();
    symlink(&real_branches, root.join(".tracedecay/branches")).unwrap();

    let report = build_inventory_in(
        &profile,
        MigrationInventoryOptions {
            roots: vec![dir.path().to_path_buf()],
            follow_symlinks: false,
            ..MigrationInventoryOptions::default()
        },
    )
    .await
    .unwrap();

    let store = report
        .stores
        .iter()
        .find(|store| store.project_root == root)
        .expect("project store should be inventoried");

    assert_eq!(store.statuses, vec![StoreStatus::Ok]);
    assert!(
        !store
            .artifacts
            .iter()
            .any(|artifact| artifact.kind == "branch_graph_db")
    );
    assert!(report.skipped.iter().any(|skipped| {
        skipped.path == root.join(".tracedecay/branches") && skipped.reason == "symlink"
    }));
}

#[tokio::test]
async fn inventory_reports_global_db_metadata() {
    let profile = InventoryProfile::acquire().await;
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let db_path = root.join("global.db");
    let db = HostAdmissionTestRuntimeV1::profile(&root).await.unwrap();
    let project = root.join("registered");
    fs::create_dir_all(&project).unwrap();
    db.upsert(&project, 42).await;
    drop(db);
    assert!(!sqlite_table_exists(&db_path, "dashboard_token_counts"));

    let report = build_inventory_in(
        &profile,
        MigrationInventoryOptions {
            roots: Vec::new(),
            global_db_path: Some(db_path.clone()),
            integrity: InventoryIntegrityMode::MetadataOnly,
            ..MigrationInventoryOptions::default()
        },
    )
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
    assert!(global.path_overridden);
    assert!(global.warnings.is_empty());
}

#[tokio::test]
async fn inventory_discovers_registered_project_outside_scan_roots() {
    let profile = InventoryProfile::acquire().await;
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let db_path = root.join("global.db");
    let registered = root.join("registered");
    fs::create_dir_all(&registered).unwrap();
    make_project_store(&registered);
    let db = HostAdmissionTestRuntimeV1::profile(&root).await.unwrap();
    db.upsert(&registered, 42).await;
    drop(db);

    let report = build_inventory_in(
        &profile,
        MigrationInventoryOptions {
            roots: Vec::new(),
            global_db_path: Some(db_path),
            ..MigrationInventoryOptions::default()
        },
    )
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
    let profile = InventoryProfile::acquire_blocking();
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
        let db = HostAdmissionTestRuntimeV1::profile(&root).await.unwrap();
        db.upsert(&discovered, 42).await;
        db.upsert(&unrelated, 99).await;
    });

    let report = block_on_inventory_in(
        &profile,
        MigrationInventoryOptions {
            roots: vec![scan_root],
            global_db_path: Some(db_path),
            ..MigrationInventoryOptions::default()
        },
    )
    .unwrap();

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
    assert!(
        !report
            .stores
            .iter()
            .any(|store| same_path(&store.project_root, &unrelated))
    );
}

#[test]
fn explicit_roots_can_include_all_registered_projects_when_requested() {
    let profile = InventoryProfile::acquire_blocking();
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
        let db = HostAdmissionTestRuntimeV1::profile(&root).await.unwrap();
        db.upsert(&discovered, 42).await;
        db.upsert(&unrelated, 99).await;
    });

    let report = block_on_inventory_in(
        &profile,
        MigrationInventoryOptions {
            roots: vec![scan_root],
            global_db_path: Some(db_path),
            include_all_registered: true,
            ..MigrationInventoryOptions::default()
        },
    )
    .unwrap();

    assert!(
        report
            .stores
            .iter()
            .any(|store| same_path(&store.project_root, &discovered)
                && store.registry_status == RegistryStatus::Registered)
    );
    assert!(
        report
            .stores
            .iter()
            .any(|store| same_path(&store.project_root, &unrelated)
                && store.registry_status == RegistryStatus::Registered)
    );
}

#[tokio::test]
async fn inventory_reports_registered_project_with_missing_local_store() {
    let profile = InventoryProfile::acquire().await;
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let db_path = root.join("global.db");
    let registered = root.join("registered_missing");
    fs::create_dir_all(&registered).unwrap();
    let db = HostAdmissionTestRuntimeV1::profile(&root).await.unwrap();
    db.upsert(&registered, 42).await;
    drop(db);

    let report = build_inventory_in(
        &profile,
        MigrationInventoryOptions {
            roots: Vec::new(),
            global_db_path: Some(db_path),
            ..MigrationInventoryOptions::default()
        },
    )
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
async fn inventory_reports_non_sqlite_global_db_as_damaged() {
    let profile = InventoryProfile::acquire().await;
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    fs::write(&db_path, b"not sqlite").unwrap();

    let report = build_inventory_in(
        &profile,
        MigrationInventoryOptions {
            roots: Vec::new(),
            global_db_path: Some(db_path),
            ..MigrationInventoryOptions::default()
        },
    )
    .await
    .unwrap();

    let global = report.global_db.expect("global DB metadata should exist");
    assert!(global.exists);
    assert_eq!(global.project_count, 0);
    assert!(matches!(
        global.integrity,
        SqliteIntegrityOutcome::Damaged { ref details }
            if !details.is_empty()
    ));
    assert!(!global.warnings.is_empty());
}

#[tokio::test]
async fn inventory_flags_leftover_config_tmp_for_manual_review() {
    let profile = InventoryProfile::acquire().await;
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    make_project_store(&root);
    fs::write(root.join(".tracedecay/config.json.tmp"), b"partial config").unwrap();

    let report = build_inventory_in(
        &profile,
        MigrationInventoryOptions {
            roots: vec![dir.path().to_path_buf()],
            ..MigrationInventoryOptions::default()
        },
    )
    .await
    .unwrap();

    let store = report
        .stores
        .iter()
        .find(|store| store.project_root == root)
        .expect("store should be inventoried");
    assert!(store.statuses.contains(&StoreStatus::NeedsManualReview));
    assert!(
        store
            .artifacts
            .iter()
            .any(|artifact| artifact.kind == "config_tmp")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn inventory_reports_skipped_symlink_directories() {
    let profile = InventoryProfile::acquire().await;
    let dir = TempDir::new().unwrap();
    let real = dir.path().join("real_project");
    fs::create_dir_all(&real).unwrap();
    make_project_store(&real);
    let alias = dir.path().join("alias_project");
    std::os::unix::fs::symlink(&real, &alias).unwrap();

    let report = build_inventory_in(
        &profile,
        MigrationInventoryOptions {
            roots: vec![dir.path().to_path_buf()],
            follow_symlinks: false,
            ..MigrationInventoryOptions::default()
        },
    )
    .await
    .unwrap();

    assert!(
        report
            .skipped
            .iter()
            .any(|skipped| skipped.path == alias && skipped.reason == "symlink")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn inventory_skips_symlinked_data_dir_by_default() {
    let profile = InventoryProfile::acquire().await;
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("repo");
    let real_data = dir.path().join("real_data");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&real_data).unwrap();
    fs::write(real_data.join("tracedecay.db"), b"not sqlite").unwrap();
    std::os::unix::fs::symlink(&real_data, root.join(".tracedecay")).unwrap();

    let report = build_inventory_in(
        &profile,
        MigrationInventoryOptions {
            roots: vec![dir.path().to_path_buf()],
            follow_symlinks: false,
            ..MigrationInventoryOptions::default()
        },
    )
    .await
    .unwrap();

    assert!(!report.stores.iter().any(|store| store.project_root == root));
    assert!(report.skipped.iter().any(|skipped| {
        skipped.path == root.join(".tracedecay") && skipped.reason == "symlink"
    }));
}

#[test]
fn inventory_discovers_hermes_home_profiles_and_state_dbs() {
    // A Hermes profile home is discovered from `HOME`, which this fixture owns.
    let profile = InventoryProfile::acquire_blocking();
    let hermes_home = profile.home().join(".hermes");
    let default_store = hermes_home.join(".tracedecay");
    let work_profile = hermes_home.join("profiles/work");
    let work_store = work_profile.join(".tracedecay");
    fs::create_dir_all(&default_store).unwrap();
    fs::create_dir_all(&work_store).unwrap();
    fs::write(default_store.join("tracedecay.db"), b"not sqlite").unwrap();
    fs::write(hermes_home.join("state.db"), b"not sqlite").unwrap();
    fs::write(work_store.join("tracedecay.db"), b"not sqlite").unwrap();
    fs::write(work_profile.join("state.db"), b"not sqlite").unwrap();

    let report = block_on_inventory_in(
        &profile,
        MigrationInventoryOptions {
            roots: Vec::new(),
            ..MigrationInventoryOptions::default()
        },
    )
    .unwrap();

    let default = report
        .stores
        .iter()
        .find(|store| store.data_dir == default_store)
        .expect("default Hermes profile store should be inventoried");
    assert_eq!(default.role, StoreRole::HermesProfileStore);
    assert_eq!(default.brand, StoreBrand::TraceDecay);
    assert!(default.statuses.contains(&StoreStatus::NeedsManualReview));

    let work = report
        .stores
        .iter()
        .find(|store| store.data_dir == work_store)
        .expect("named Hermes profile store should be inventoried");
    assert_eq!(work.role, StoreRole::HermesProfileStore);
    assert_eq!(work.brand, StoreBrand::TraceDecay);
    assert!(work.statuses.contains(&StoreStatus::NeedsManualReview));

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
fn inventory_does_not_treat_scan_root_dot_hermes_as_a_profile_home() {
    // The scan root deliberately sits beside this fixture's `HOME`, so a
    // `.hermes` directory inside it is not the user's Hermes profile home.
    let profile = InventoryProfile::acquire_blocking();
    let scan_root = profile.path("project");
    let redirected = scan_root.join(".hermes");
    fs::create_dir_all(&redirected).unwrap();
    fs::write(redirected.join("state.db"), b"not sqlite").unwrap();

    let report = block_on_inventory_in(
        &profile,
        MigrationInventoryOptions {
            roots: vec![scan_root],
            ..MigrationInventoryOptions::default()
        },
    )
    .unwrap();

    assert!(
        report
            .stores
            .iter()
            .all(|store| !store.db_path.starts_with(&redirected)),
        "a scan root must not become an alternate Hermes profile home: {report:?}"
    );
}

#[test]
fn inventory_discovers_default_home_hermes_project_pin() {
    let profile = InventoryProfile::acquire_blocking();
    let hermes_home = profile.home().join(".hermes");
    let pinned_project = profile.path("pinned-project");
    fs::create_dir_all(&hermes_home).unwrap();
    fs::write(
        hermes_home.join("config.yaml"),
        format!(
            "plugins:\n  tracedecay:\n    project_root: '{}'\n",
            pinned_project.display()
        ),
    )
    .unwrap();
    make_project_store(&pinned_project);

    let report = block_on_inventory_in(
        &profile,
        MigrationInventoryOptions {
            roots: Vec::new(),
            ..MigrationInventoryOptions::default()
        },
    )
    .unwrap();

    let store = report
        .stores
        .iter()
        .find(|store| store.project_root == pinned_project)
        .expect("Hermes config project_root pin should be inventoried");
    assert_eq!(store.role, StoreRole::CodeProjectStore);
    assert_eq!(store.brand, StoreBrand::TraceDecay);
}
