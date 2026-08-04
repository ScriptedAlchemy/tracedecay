use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tracedecay::config::{TraceDecayConfig, USER_DATA_DIR_ENV};
use tracedecay::serve;
use tracedecay::storage::{
    EnrollmentMarker, STORE_MANIFEST_FILENAME, StorageMode, write_enrollment_marker,
};
use tracedecay::tracedecay::{TraceDecay, TraceDecayOpenOptions};

use crate::support::HOME_ENV_LOCK;

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
        "profile storage reset fixture initialization",
    )
    .unwrap();
    let _database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle,
        profile_root,
        "profile storage reset fixture initialization",
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
        "profile storage reset fixture open",
    )
    .unwrap();
    let _database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle,
        profile_root,
        "profile storage reset fixture open",
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
        "profile storage reset fixture branch open",
    )
    .unwrap();
    let _database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle,
        profile_root,
        "profile storage reset fixture branch open",
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

fn schema_version(db_path: &Path) -> u32 {
    let connection =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let version = connection
        .query_row("PRAGMA user_version", (), |row| row.get::<_, i64>(0))
        .unwrap();
    u32::try_from(version).unwrap()
}

#[tokio::test]
async fn fresh_profile_initialization_creates_the_final_v2_store() {
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
    assert_eq!(
        schema_version(&cg.db_path()),
        tracedecay::db::migrations::SCHEMA_VERSION
    );
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
async fn incompatible_profile_store_requires_reset_without_in_place_changes() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let home = root.join("home");
    let profile_root = home.join(".tracedecay");
    let project = root.join("repo");
    fs::create_dir_all(&project).unwrap();
    let _home_guard = HomeEnvGuard::set(&home);

    let initialized =
        init_with_maintenance(&project, &profile_root, TraceDecayOpenOptions::default())
            .await
            .unwrap();
    let db_path = initialized.db_path().to_path_buf();
    drop(initialized);

    let incompatible_version = tracedecay::db::migrations::SCHEMA_VERSION - 1;
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .pragma_update(None, "user_version", incompatible_version)
        .unwrap();
    drop(connection);

    let error = match open_with_maintenance(
        &project,
        &profile_root,
        TraceDecayOpenOptions::default(),
    )
    .await
    {
        Ok(_) => panic!("an incompatible profile store must require a reset"),
        Err(error) => error,
    };
    let tracedecay::errors::TraceDecayError::Database { message, operation } = error else {
        panic!("incompatible profile store returned the wrong error: {error}");
    };
    assert_eq!(operation, "ensure_schema_current");
    assert!(
        message.contains("created by an incompatible binary")
            && message.contains("Remove the store directory"),
        "reset-required error must explain the fresh-profile remedy: {message}"
    );
    assert_eq!(
        schema_version(&db_path),
        incompatible_version,
        "a rejected store must not be migrated or restamped"
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
        "config error: branch snapshot open requires the exact profile's exclusive lifecycle lease"
    );
}

#[tokio::test]
async fn trace_decay_open_branch_uses_shared_profile_store() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let home = root.join("home");
    let profile_root = home.join(".tracedecay");
    let project = root.join("repo");
    let shard_root = profile_root.join("projects/proj_branch");
    fs::create_dir_all(&shard_root).unwrap();
    fs::create_dir_all(project.join(".tracedecay")).unwrap();
    run_git(&project, &["init"]);
    run_git(&project, &["config", "user.email", "test@example.com"]);
    run_git(&project, &["config", "user.name", "TraceDecay Test"]);
    fs::write(project.join("seed.txt"), "seed\n").unwrap();
    run_git(&project, &["add", "seed.txt"]);
    run_git(&project, &["commit", "-m", "seed"]);
    run_git(&project, &["branch", "feature/profile"]);
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

    let cg = open_branch_with_maintenance(
        &project,
        "feature/profile",
        &profile_root,
        TraceDecayOpenOptions::default(),
    )
    .await
    .unwrap();

    assert_path_eq(&cg.store_layout().data_root, &shard_root);
    assert_path_eq(cg.db_path(), shard_root.join("tracedecay.db"));
    assert_eq!(cg.active_branch(), Some("feature/profile"));
}

#[tokio::test]
async fn trace_decay_open_with_options_selects_branch_in_explicit_profile() {
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
    assert_eq!(cg.active_branch(), Some("feature/client-profile"));
    assert_eq!(cg.db_path(), shard_root.join("tracedecay.db"));
    assert!(cg.db_path().is_file());
    assert!(
        !daemon_home.join(".tracedecay").exists(),
        "branch selection with explicit options must not create storage in the daemon/default profile"
    );
}
