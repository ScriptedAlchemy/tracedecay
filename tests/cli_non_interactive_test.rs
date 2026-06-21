use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod common;

use common::sample_node;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;
use tracedecay::branch_meta::BranchMeta;
use tracedecay::db::Database;
use tracedecay::global_db::{GlobalDb, StoreInstanceUpsert};
use tracedecay::migrate::inventory::MigrationInventory;
use tracedecay::migrate::manifest::{
    load_manifest, save_manifest, verify_migration_manifest, ArtifactState, MigrationArtifact,
    MigrationManifest, MigrationProtocol,
};
use tracedecay::storage::{
    read_enrollment_marker, write_enrollment_marker, EnrollmentMarker, StorageMode, StoreKind,
    StoreManifest, STORE_MANIFEST_FILENAME, STORE_MANIFEST_SCHEMA_VERSION,
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

fn profile_root(home: &Path) -> PathBuf {
    canonical_temp_path(home).join(".tracedecay")
}

fn profile_shard_root(home: &Path) -> PathBuf {
    profile_root(home).join("projects/proj_cli")
}

fn tracedecay_command(home: &std::path::Path, project: &std::path::Path) -> Command {
    let home = canonical_temp_path(home);
    let profile_root = profile_root(&home);
    let mut command = Command::new(env!("CARGO_BIN_EXE_tracedecay"));
    command
        .current_dir(project)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("TRACEDECAY_DATA_DIR", &profile_root)
        .env("TRACEDECAY_GLOBAL_DB", profile_root.join("global.db"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn tracedecay_command_with_stdin(home: &std::path::Path, project: &std::path::Path) -> Command {
    let mut command = tracedecay_command(home, project);
    command.stdin(Stdio::piped());
    command
}

fn write_profile_sharded_fixture(home: &std::path::Path, project: &std::path::Path) {
    let project = canonical_temp_path(project);
    let shard_root = profile_shard_root(home);
    std::fs::create_dir_all(&shard_root).unwrap();
    write_enrollment_marker(
        &project,
        &EnrollmentMarker {
            project_id: "proj_cli".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    std::fs::write(shard_root.join("tracedecay.db"), b"profile graph").unwrap();
    std::fs::write(shard_root.join("sessions.db"), b"sessions").unwrap();
    std::fs::write(
        shard_root.join("branch-meta.json"),
        r#"{"default_branch":"main","branches":{}}"#,
    )
    .unwrap();
    let manifest = StoreManifest {
        schema_version: STORE_MANIFEST_SCHEMA_VERSION,
        project_id: Some("proj_cli".to_string()),
        store_kind: StoreKind::CodeProject,
        storage_mode: StorageMode::ProfileSharded,
        project_root: project,
        data_root: shard_root.clone(),
        graph_db_relpath: "tracedecay.db".into(),
        sessions_db_relpath: "sessions.db".into(),
        branch_meta_relpath: "branch-meta.json".into(),
    };
    std::fs::write(
        shard_root.join(STORE_MANIFEST_FILENAME),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

fn write_sqlite_placeholder(path: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db = libsql::Builder::new_local(path).build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute("CREATE TABLE marker (id INTEGER PRIMARY KEY)", ())
            .await
            .unwrap();
    });
}

async fn register_profile_sharded_store(
    db: &GlobalDb,
    project_root: &std::path::Path,
    project_id: &str,
) {
    db.upsert(project_root, 42).await;
    db.upsert_code_project(project_id, project_root, None, None, Some("main"))
        .await
        .expect("code project should upsert");
    db.upsert_store_instance(StoreInstanceUpsert {
        store_id: format!("store:{project_id}:profile_sharded"),
        project_id: project_id.to_string(),
        store_kind: "code_project".to_string(),
        storage_mode: "profile_sharded".to_string(),
        store_relpath: format!("projects/{project_id}"),
        manifest_relpath: Some(STORE_MANIFEST_FILENAME.to_string()),
        last_verified_at: Some(1_800_000_000),
        last_write_at: Some(1_800_000_000),
    })
    .await
    .expect("store instance should upsert");
}

fn write_branch_meta(
    shard_root: &std::path::Path,
    tracked_branches: &[(&str, &str)],
    create_branch_dbs: bool,
) {
    let mut meta = BranchMeta::new_for_dir(shard_root, "main");
    for (name, rel_db_path) in tracked_branches {
        meta.add_branch(name, rel_db_path, "main");
        if create_branch_dbs {
            let db_path = shard_root.join(rel_db_path);
            if let Some(parent) = db_path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(db_path, format!("branch db for {name}")).unwrap();
        }
    }
    std::fs::write(
        shard_root.join("branch-meta.json"),
        serde_json::to_string_pretty(&meta).unwrap(),
    )
    .unwrap();
}

fn run_with_timeout(mut command: Command, timeout: Duration) -> std::process::Output {
    let mut child = command
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn tracedecay: {e}"));
    let started = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .unwrap_or_else(|e| panic!("failed to poll child: {e}"))
        {
            let stdout = child
                .stdout
                .take()
                .map(|mut out| {
                    let mut buf = Vec::new();
                    std::io::Read::read_to_end(&mut out, &mut buf)
                        .unwrap_or_else(|e| panic!("failed to read stdout: {e}"));
                    buf
                })
                .unwrap_or_default();
            let stderr = child
                .stderr
                .take()
                .map(|mut err| {
                    let mut buf = Vec::new();
                    std::io::Read::read_to_end(&mut err, &mut buf)
                        .unwrap_or_else(|e| panic!("failed to read stderr: {e}"));
                    buf
                })
                .unwrap_or_default();
            return std::process::Output {
                status,
                stdout,
                stderr,
            };
        }
        assert!(
            started.elapsed() < timeout,
            "tracedecay hung with stdin closed after {:?}",
            started.elapsed()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn init_skips_gitignore_prompt_when_stdin_not_a_terminal() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    std::fs::create_dir_all(project.path().join("src")).unwrap();
    std::fs::write(project.path().join("src/lib.rs"), "pub fn marker() {}\n").unwrap();

    let mut command = tracedecay_command(home.path(), project.path());
    command.arg("init");
    let output = run_with_timeout(command, Duration::from_secs(30));

    assert!(
        output.status.success(),
        "init should succeed non-interactively\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        std::fs::read_dir(profile_root(home.path()).join("projects"))
            .unwrap()
            .any(|entry| entry.unwrap().path().join("tracedecay.db").is_file()),
        "init should still create the project index in the profile store"
    );
    let gitignore = project.path().join(".gitignore");
    assert!(
        !gitignore.exists(),
        "non-interactive init must not add .gitignore by default"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Non-interactive: skipped adding .tracedecay to .gitignore"),
        "stderr should explain the non-interactive default\nstderr:\n{stderr}"
    );
}

#[test]
fn bare_invocation_skips_create_prompt_when_stdin_not_a_terminal() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    std::fs::create_dir_all(project.path().join("src")).unwrap();
    std::fs::write(project.path().join("src/lib.rs"), "pub fn marker() {}\n").unwrap();

    let output = run_with_timeout(
        tracedecay_command(home.path(), project.path()),
        Duration::from_secs(30),
    );

    assert!(
        output.status.success(),
        "bare tracedecay should exit cleanly non-interactively\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !project.path().join(".tracedecay").exists(),
        "bare invocation must not create an index non-interactively"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Non-interactive: skipping index creation"),
        "stderr should explain the non-interactive default\nstderr:\n{stderr}"
    );
}

#[test]
fn status_skips_create_prompt_when_stdin_not_a_terminal() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    std::fs::create_dir_all(project.path().join("src")).unwrap();
    std::fs::write(project.path().join("src/lib.rs"), "pub fn marker() {}\n").unwrap();

    let mut command = tracedecay_command(home.path(), project.path());
    command.arg("status");
    let output = run_with_timeout(command, Duration::from_secs(30));

    assert!(
        output.status.success(),
        "status should exit cleanly non-interactively\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !project.path().join(".tracedecay").exists(),
        "status must not create an index non-interactively"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Non-interactive: skipping index creation"),
        "stderr should explain the non-interactive default\nstderr:\n{stderr}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn status_json_reads_readonly_project_database() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_root = canonical_temp_path(project.path());
    write_enrollment_marker(
        &project_root,
        &EnrollmentMarker {
            project_id: "proj_cli".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let db_path = profile_shard_root(home.path()).join("tracedecay.db");
    let (db, _) = Database::initialize(&db_path).await.unwrap();
    db.insert_node(&sample_node("node-1", "process_data", "src/lib.rs"))
        .await
        .unwrap();
    db.checkpoint().await.unwrap();
    db.close();
    let mut permissions = std::fs::metadata(&db_path).unwrap().permissions();
    permissions.set_mode(0o444);
    std::fs::set_permissions(&db_path, permissions).unwrap();

    let mut command = tracedecay_command(home.path(), project.path());
    command.args(["status", "--json"]);
    let output = run_with_timeout(command, Duration::from_secs(30));

    assert!(
        output.status.success(),
        "status --json should read readonly DB\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["node_count"], 1);
}

#[tokio::test]
async fn list_all_reports_profile_sharded_store_without_stale_label() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write_profile_sharded_fixture(home.path(), project.path());
    let db = GlobalDb::open_at(&profile_root(home.path()).join("global.db"))
        .await
        .unwrap();
    db.upsert(project.path(), 42).await;

    let mut command = tracedecay_command(home.path(), project.path());
    command.args(["list", "--all"]);
    let output = run_with_timeout(command, Duration::from_secs(30));

    assert!(
        output.status.success(),
        "list --all should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("profile-sharded"),
        "profile-sharded store should be labelled\nstdout:\n{stdout}"
    );
    assert!(
        !stdout.contains("stale"),
        "live profile shard must not be labelled stale\nstdout:\n{stdout}"
    );
}

#[tokio::test]
async fn wipe_all_removes_profile_sharded_store_and_global_row() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write_profile_sharded_fixture(home.path(), project.path());
    let shard_root = profile_shard_root(home.path());
    let db_path = profile_root(home.path()).join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    db.upsert(project.path(), 42).await;
    drop(db);

    let mut command = tracedecay_command_with_stdin(home.path(), project.path());
    command.args(["wipe", "--all"]);
    let mut child = command.spawn().unwrap();
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().unwrap();
        stdin.write_all(b"go!\n").unwrap();
    }
    let output = child.wait_with_output().unwrap();

    assert!(
        output.status.success(),
        "wipe --all should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !shard_root.exists(),
        "wipe --all should remove the profile shard"
    );
    let reopened = GlobalDb::open_at(&db_path).await.unwrap();
    assert!(
        reopened.list_project_paths().await.is_empty(),
        "global projects table should be empty after wipe --all"
    );
}

#[test]
fn list_all_reports_orphan_manifest_reconstructable_store() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write_profile_sharded_fixture(home.path(), project.path());
    std::fs::create_dir_all(profile_root(home.path())).unwrap();

    let mut command = tracedecay_command(home.path(), project.path());
    command.args(["list", "--all"]);
    let output = run_with_timeout(command, Duration::from_secs(30));

    assert!(
        output.status.success(),
        "list --all should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("orphan manifest-reconstructable"),
        "orphan manifest should be visible and reconstructable\nstdout:\n{stdout}"
    );
}

#[tokio::test]
async fn list_all_uses_registry_profile_shard_when_enrollment_marker_missing() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write_profile_sharded_fixture(home.path(), project.path());
    std::fs::remove_dir_all(project.path().join(".tracedecay")).unwrap();
    let db = GlobalDb::open_at(&profile_root(home.path()).join("global.db"))
        .await
        .unwrap();
    register_profile_sharded_store(&db, project.path(), "proj_cli").await;

    let mut command = tracedecay_command(home.path(), project.path());
    command.args(["list", "--all"]);
    let output = run_with_timeout(command, Duration::from_secs(30));

    assert!(
        output.status.success(),
        "list --all should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("profile-sharded"),
        "registry-backed profile shard should be labelled profile-sharded\nstdout:\n{stdout}"
    );
    assert!(
        !stdout.contains("stale"),
        "registry-backed profile shard must not be labelled stale\nstdout:\n{stdout}"
    );
}

#[tokio::test]
async fn wipe_all_removes_registry_backed_profile_shard_without_enrollment_marker() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write_profile_sharded_fixture(home.path(), project.path());
    std::fs::remove_dir_all(project.path().join(".tracedecay")).unwrap();
    let shard_root = profile_shard_root(home.path());
    let db_path = profile_root(home.path()).join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    register_profile_sharded_store(&db, project.path(), "proj_cli").await;
    drop(db);

    let mut command = tracedecay_command_with_stdin(home.path(), project.path());
    command.args(["wipe", "--all"]);
    let mut child = command.spawn().unwrap();
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().unwrap();
        stdin.write_all(b"go!\n").unwrap();
    }
    let output = child.wait_with_output().unwrap();

    assert!(
        output.status.success(),
        "wipe --all should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !shard_root.exists(),
        "wipe --all should remove registry-backed profile shard"
    );
}

#[test]
fn branch_list_reads_profile_sharded_branch_meta() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write_profile_sharded_fixture(home.path(), project.path());
    let shard_root = profile_shard_root(home.path());
    write_branch_meta(&shard_root, &[], false);

    let mut command = tracedecay_command(home.path(), project.path());
    command.args(["branch", "list"]);
    let output = run_with_timeout(command, Duration::from_secs(30));

    assert!(
        output.status.success(),
        "branch list should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Default branch: main"),
        "branch list should read profile-sharded branch metadata\nstderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("No branch tracking configured"),
        "branch list should not fall back to repo-local metadata\nstderr:\n{stderr}"
    );
}

#[test]
fn branch_add_writes_new_branch_db_into_profile_shard() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write_profile_sharded_fixture(home.path(), project.path());
    let shard_root = profile_shard_root(home.path());
    write_branch_meta(&shard_root, &[], false);

    let mut command = tracedecay_command(home.path(), project.path());
    command.args(["branch", "add", "feature/new"]);
    let output = run_with_timeout(command, Duration::from_secs(30));
    let stderr = String::from_utf8_lossy(&output.stderr);
    let copied_db = shard_root.join("branches/feature_new.db");

    assert!(
        output.status.success() || stderr.contains("file is not a database"),
        "branch add should resolve and copy profile-sharded DB before sync\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        stderr
    );
    assert!(
        copied_db.exists(),
        "branch add should create branch DB under the profile shard"
    );
    assert!(
        !stderr.contains("parent DB not found"),
        "branch add should not look for parent DB in repo-local storage\nstderr:\n{stderr}"
    );
}

#[test]
fn branch_remove_deletes_branch_db_from_profile_shard() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write_profile_sharded_fixture(home.path(), project.path());
    let shard_root = profile_shard_root(home.path());
    write_branch_meta(
        &shard_root,
        &[("feature/ui", "branches/feature_ui.db")],
        true,
    );

    let mut command = tracedecay_command(home.path(), project.path());
    command.args(["branch", "remove", "feature/ui"]);
    let output = run_with_timeout(command, Duration::from_secs(30));

    assert!(
        output.status.success(),
        "branch remove should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !shard_root.join("branches/feature_ui.db").exists(),
        "branch remove should delete branch DB from profile shard"
    );
}

#[test]
fn branch_removeall_deletes_profile_shard_branch_dbs() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write_profile_sharded_fixture(home.path(), project.path());
    let shard_root = profile_shard_root(home.path());
    write_branch_meta(
        &shard_root,
        &[
            ("feature/one", "branches/feature_one.db"),
            ("feature/two", "branches/feature_two.db"),
        ],
        true,
    );

    let mut command = tracedecay_command(home.path(), project.path());
    command.args(["branch", "removeall"]);
    let output = run_with_timeout(command, Duration::from_secs(30));

    assert!(
        output.status.success(),
        "branch removeall should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !shard_root.join("branches/feature_one.db").exists()
            && !shard_root.join("branches/feature_two.db").exists(),
        "branch removeall should delete all non-default branch DBs from profile shard"
    );
}

#[test]
fn branch_gc_deletes_stale_profile_shard_branch_dbs() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write_profile_sharded_fixture(home.path(), project.path());
    let shard_root = profile_shard_root(home.path());
    write_branch_meta(
        &shard_root,
        &[("feature/stale", "branches/feature_stale.db")],
        true,
    );

    let mut command = tracedecay_command(home.path(), project.path());
    command.args(["branch", "gc"]);
    let output = run_with_timeout(command, Duration::from_secs(30));

    assert!(
        output.status.success(),
        "branch gc should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !shard_root.join("branches/feature_stale.db").exists(),
        "branch gc should delete stale branch DBs from profile shard"
    );
}

#[test]
fn migrate_verify_text_reports_actual_apply_supported_state() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_root = canonical_temp_path(project.path());
    write_profile_sharded_fixture(home.path(), &project_root);
    let shard_root = profile_shard_root(home.path());
    let manifest_path = canonical_temp_path(home.path()).join("migration-manifest.json");
    let protocol = MigrationProtocol::for_manifest(&manifest_path, "mig_cli_verify");
    let mut manifest = MigrationManifest::new(
        "mig_cli_verify",
        "0.0.2",
        1_800_000_000,
        "confirm-mig_cli_verify",
        protocol,
        MigrationInventory {
            stores: Vec::new(),
            skipped: Vec::new(),
            global_db: None,
        },
    );
    manifest.source.project_root = Some(project_root.clone());
    manifest.source.data_dir = Some(shard_root.clone());
    manifest.destination.profile_root = Some(profile_root(home.path()));
    manifest.destination.project_id = Some("proj_cli".to_string());

    let mut graph_artifact = MigrationArtifact::new(
        "graph_db",
        shard_root.join("tracedecay.db"),
        Some(shard_root.join("tracedecay.db")),
    );
    graph_artifact.state = ArtifactState::Applied;
    manifest.artifacts.push(graph_artifact);

    let mut store_manifest_artifact = MigrationArtifact::new(
        "store_manifest",
        shard_root.join(STORE_MANIFEST_FILENAME),
        Some(shard_root.join(STORE_MANIFEST_FILENAME)),
    );
    store_manifest_artifact.state = ArtifactState::Applied;
    manifest.artifacts.push(store_manifest_artifact);
    save_manifest(&manifest).unwrap();

    let verify_report = verify_migration_manifest(&manifest);
    assert!(
        verify_report.apply_supported,
        "fixture should produce apply_supported=true, got report: {:?}",
        verify_report
    );

    let mut command = tracedecay_command(home.path(), &project_root);
    command.args(["migrate", "verify", "--manifest"]);
    command.arg(manifest_path);
    let output = run_with_timeout(command, Duration::from_secs(30));

    assert!(
        output.status.success(),
        "migrate verify should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("apply supported: yes"),
        "verify text output should match apply_supported=true\nstdout:\n{stdout}"
    );
}

#[test]
fn migrate_plan_save_writes_manifest_and_prints_confirmation_token_noninteractively() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_root = canonical_temp_path(project.path());
    let profile_root = profile_root(home.path());
    let graph_db = project_root.join(".tracedecay/tracedecay.db");
    write_sqlite_placeholder(&graph_db);

    let mut command = tracedecay_command(home.path(), &project_root);
    command.args([
        "migrate",
        "plan",
        "--root",
        project_root.to_str().unwrap(),
        "--save",
        "--profile-root",
        profile_root.to_str().unwrap(),
        "--project-id",
        "proj_cli",
    ]);
    let output = run_with_timeout(command, Duration::from_secs(30));

    assert!(
        output.status.success(),
        "migrate plan --save should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("confirmation token: confirm-mig_"),
        "stdout should include the generated confirmation token\nstdout:\n{stdout}"
    );
    let manifest_dir = profile_root.join("migration-inventory");
    let manifests = std::fs::read_dir(&manifest_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect::<Vec<_>>();
    assert_eq!(manifests.len(), 1, "unexpected manifests: {manifests:?}");
    let manifest = load_manifest(&manifests[0]).unwrap();
    assert_eq!(manifest.destination.project_id.as_deref(), Some("proj_cli"));
    assert!(!manifest.confirmation_token.is_empty());
}

#[test]
fn migrate_export_from_profile_copies_profile_store_to_target() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_root = canonical_temp_path(project.path());
    write_profile_sharded_fixture(home.path(), &project_root);
    let export_dir = canonical_temp_path(home.path()).join("exported-store");

    let mut command = tracedecay_command(home.path(), &project_root);
    command.args([
        "migrate",
        "export",
        "--from-profile",
        "--project",
        project_root.to_str().unwrap(),
        "--to",
        export_dir.to_str().unwrap(),
    ]);
    let output = run_with_timeout(command, Duration::from_secs(30));

    assert!(
        output.status.success(),
        "migrate export should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read(export_dir.join("tracedecay.db")).unwrap(),
        b"profile graph"
    );
    let exported_manifest =
        tracedecay::storage::read_store_manifest(&export_dir.join(STORE_MANIFEST_FILENAME))
            .unwrap();
    assert_eq!(exported_manifest.project_id.as_deref(), Some("proj_cli"));
    assert_eq!(exported_manifest.data_root, export_dir);
}

#[test]
fn migrate_cleanup_sources_removes_source_artifacts_but_preserves_enrollment_marker() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_root = canonical_temp_path(project.path());
    let data_dir = project_root.join(".tracedecay");
    let source_graph = data_dir.join("tracedecay.db");
    let profile_root = profile_root(home.path());
    let target_root = profile_root.join("projects/proj_cli");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&target_root).unwrap();
    std::fs::write(&source_graph, b"graph").unwrap();
    std::fs::write(target_root.join("tracedecay.db"), b"graph").unwrap();
    std::fs::write(
        target_root.join("branch-meta.json"),
        r#"{"default_branch":"main","branches":{}}"#,
    )
    .unwrap();
    let store_manifest = StoreManifest {
        schema_version: STORE_MANIFEST_SCHEMA_VERSION,
        project_id: Some("proj_cli".to_string()),
        store_kind: StoreKind::CodeProject,
        storage_mode: StorageMode::ProfileSharded,
        project_root: project_root.clone(),
        data_root: target_root.clone(),
        graph_db_relpath: "tracedecay.db".into(),
        sessions_db_relpath: "sessions.db".into(),
        branch_meta_relpath: "branch-meta.json".into(),
    };
    std::fs::write(
        target_root.join(STORE_MANIFEST_FILENAME),
        serde_json::to_string_pretty(&store_manifest).unwrap(),
    )
    .unwrap();
    write_enrollment_marker(
        &project_root,
        &EnrollmentMarker {
            project_id: "proj_cli".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();

    let manifest_path = canonical_temp_path(home.path()).join("migration-manifest.json");
    let protocol = MigrationProtocol::for_manifest(&manifest_path, "mig_cli_cleanup");
    let mut manifest = MigrationManifest::new(
        "mig_cli_cleanup",
        "0.0.2",
        1_800_000_000,
        "confirm-mig_cli_cleanup",
        protocol,
        MigrationInventory {
            stores: Vec::new(),
            skipped: Vec::new(),
            global_db: None,
        },
    );
    manifest.source.project_root = Some(project_root.clone());
    manifest.source.data_dir = Some(data_dir.clone());
    manifest.destination.profile_root = Some(profile_root);
    manifest.destination.project_id = Some("proj_cli".to_string());
    let mut graph_artifact = MigrationArtifact::new(
        "graph_db",
        source_graph.clone(),
        Some(target_root.join("tracedecay.db")),
    );
    graph_artifact.state = ArtifactState::Applied;
    manifest.artifacts.push(graph_artifact);
    let mut store_manifest_artifact = MigrationArtifact::new(
        "store_manifest",
        target_root.join(STORE_MANIFEST_FILENAME),
        Some(target_root.join(STORE_MANIFEST_FILENAME)),
    );
    store_manifest_artifact.state = ArtifactState::Applied;
    manifest.artifacts.push(store_manifest_artifact);
    save_manifest(&manifest).unwrap();

    let mut command = tracedecay_command(home.path(), &project_root);
    command.args([
        "migrate",
        "cleanup-sources",
        "--manifest",
        manifest_path.to_str().unwrap(),
        "--confirm-token",
        "confirm-mig_cli_cleanup",
    ]);
    let output = run_with_timeout(command, Duration::from_secs(30));

    assert!(
        output.status.success(),
        "cleanup-sources should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !source_graph.exists(),
        "source graph artifact should be removed"
    );
    assert!(
        read_enrollment_marker(&project_root).unwrap().is_some(),
        "cleanup must preserve profile-sharded enrollment marker"
    );
}
