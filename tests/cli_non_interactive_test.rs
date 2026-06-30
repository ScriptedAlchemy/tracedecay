use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

mod common;

use common::{create_runtime, sample_node};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;
use tracedecay::automation::run_ledger::{
    append_run_record, write_run_artifact, AutomationRunArtifactKind, AutomationRunLedgerRecord,
};
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

fn comparable_existing_path(path: &Path) -> String {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    #[cfg(windows)]
    {
        let path = path.to_string_lossy().into_owned();
        path.strip_prefix(r"\\?\")
            .unwrap_or(&path)
            .to_ascii_lowercase()
    }
    #[cfg(not(windows))]
    {
        path.to_string_lossy().into_owned()
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

fn cli_timeout() -> Duration {
    if cfg!(windows) {
        Duration::from_secs(90)
    } else {
        Duration::from_secs(30)
    }
}

fn add_tracedecay_path_shim(command: &mut Command, home: &Path) -> PathBuf {
    let bin_dir = home.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let shim = bin_dir.join(if cfg!(windows) {
        "tracedecay.exe"
    } else {
        "tracedecay"
    });
    if std::fs::hard_link(env!("CARGO_BIN_EXE_tracedecay"), &shim).is_err() {
        std::fs::copy(env!("CARGO_BIN_EXE_tracedecay"), &shim).unwrap();
    }
    #[cfg(unix)]
    {
        let mut permissions = std::fs::metadata(&shim).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&shim, permissions).unwrap();
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let joined =
        std::env::join_paths(std::iter::once(bin_dir).chain(std::env::split_paths(&path))).unwrap();
    command.env("PATH", joined);
    shim
}

fn git(project: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(["-c", "core.hooksPath=.git/no-hooks"])
        .args(args)
        .current_dir(project)
        .output()
        .unwrap_or_else(|e| panic!("failed to run git {args:?}: {e}"));
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn commit_all(project: &Path, message: &str) {
    git(project, &["add", "."]);
    git(
        project,
        &[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay-test@example.com",
            "commit",
            "-m",
            message,
        ],
    );
}

#[test]
fn init_accepts_relative_current_directory() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_root = canonical_temp_path(project.path());
    std::fs::write(project_root.join("lib.rs"), "pub fn indexed() {}\n").unwrap();

    let mut command = tracedecay_command(home.path(), &project_root);
    command.args(["init", "."]);
    let output = run_with_timeout(command, cli_timeout());

    assert!(
        output.status.success(),
        "init . should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !project_root.join(".tracedecay/tracedecay.db").exists(),
        "default init must use the profile-sharded store, not a repo-local graph DB"
    );
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

fn write_profile_sharded_branch_fixture(home: &std::path::Path, project: &std::path::Path) {
    write_profile_sharded_fixture(home, project);
    let project = canonical_temp_path(project);
    let shard_root = profile_shard_root(home);
    git(&project, &["init", "-b", "main"]);
    std::fs::write(project.join("lib.rs"), "pub fn indexed() {}\n").unwrap();
    commit_all(&project, "initial commit");
    git(&project, &["checkout", "-b", "feature/new"]);
    std::fs::remove_file(shard_root.join("tracedecay.db")).unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(Database::initialize(&shard_root.join("tracedecay.db")))
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

fn child_output(mut child: Child, status: ExitStatus) -> Output {
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
    Output {
        status,
        stdout,
        stderr,
    }
}

fn run_with_timeout(mut command: Command, timeout: Duration) -> Output {
    let mut child = command
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn tracedecay: {e}"));
    let started = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .unwrap_or_else(|e| panic!("failed to poll child: {e}"))
        {
            return child_output(child, status);
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let status = child
                .wait()
                .unwrap_or_else(|e| panic!("failed to wait for timed out child: {e}"));
            let output = child_output(child, status);
            panic!(
                "tracedecay hung with stdin closed after {:?}\nstdout:\n{}\nstderr:\n{}",
                started.elapsed(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
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
    let output = run_with_timeout(command, cli_timeout());

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
fn install_codex_automation_writes_global_project_record_noninteractively() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_root = canonical_temp_path(project.path());

    let mut install = tracedecay_command(home.path(), &project_root);
    let _shim = add_tracedecay_path_shim(&mut install, home.path());
    install.args(["install", "--agent", "codex", "--automation"]);
    let output = run_with_timeout(install, cli_timeout());
    assert!(
        output.status.success(),
        "codex automation install should succeed non-interactively\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        home.path()
            .join("plugins/tracedecay/.codex-plugin/plugin.json")
            .is_file(),
        "install --agent codex should still install the Codex plugin bundle"
    );
    let automation_path = home
        .path()
        .join(".codex/automations/watch-tracedecay-memory/automation.toml");
    let automation = std::fs::read_to_string(&automation_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", automation_path.display()));
    let parsed = automation
        .parse::<toml::Table>()
        .expect("automation.toml should be valid TOML");
    let configured_cwd = parsed
        .get("cwds")
        .and_then(|value| value.as_array())
        .and_then(|values| values.first())
        .and_then(|value| value.as_str())
        .expect("automation cwd should be written");
    assert_eq!(
        comparable_existing_path(Path::new(configured_cwd)),
        comparable_existing_path(&project_root)
    );
    assert!(
        !project_root.join(".codex/automations").exists(),
        "codex automation install should write the automation under the user profile"
    );
}

#[test]
fn automation_config_enable_writes_project_sidecar_noninteractively() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    std::fs::create_dir_all(project.path().join("src")).unwrap();
    std::fs::write(project.path().join("src/lib.rs"), "pub fn marker() {}\n").unwrap();

    let mut init = tracedecay_command(home.path(), project.path());
    init.arg("init");
    let init_output = run_with_timeout(init, cli_timeout());
    assert!(
        init_output.status.success(),
        "init should succeed before automation config\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&init_output.stdout),
        String::from_utf8_lossy(&init_output.stderr)
    );

    let mut enable = tracedecay_command(home.path(), project.path());
    enable.args(["automation", "config", "enable"]);
    let enable_output = run_with_timeout(enable, cli_timeout());
    assert!(
        enable_output.status.success(),
        "automation config enable should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&enable_output.stdout),
        String::from_utf8_lossy(&enable_output.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&enable_output.stdout)
        .expect("automation config enable should print JSON");
    assert_eq!(payload["project"]["enabled"], true);
    assert_eq!(payload["project"]["backend"], "codex_app_server");
    assert_eq!(payload["effective"]["enabled"], true);
    assert_eq!(payload["effective"]["backend"], "codex_app_server");

    let mut explain = tracedecay_command(home.path(), project.path());
    explain.args(["automation", "config", "explain", "--json"]);
    let explain_output = run_with_timeout(explain, cli_timeout());
    assert!(
        explain_output.status.success(),
        "automation config explain should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&explain_output.stdout),
        String::from_utf8_lossy(&explain_output.stderr)
    );
    let explain_payload: serde_json::Value = serde_json::from_slice(&explain_output.stdout)
        .expect("automation config explain should print JSON");
    assert_eq!(explain_payload["explanation"]["source"], "project");
    assert_eq!(
        explain_payload["explanation"]["trace_decay_backend_calls"],
        true
    );
    assert_eq!(explain_payload["explanation"]["delegated_host"], false);
    assert_eq!(
        explain_payload["backend_availability"]["backend"],
        "codex_app_server"
    );

    let projects_dir = profile_root(home.path()).join("projects");
    let sidecars = std::fs::read_dir(&projects_dir)
        .unwrap()
        .map(|entry| {
            entry
                .unwrap()
                .path()
                .join("dashboard/automation_config.json")
        })
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    assert_eq!(
        sidecars.len(),
        1,
        "automation config should write one project sidecar under {projects_dir:?}, got {sidecars:?}"
    );
}

#[test]
fn automation_config_set_global_defaults_noninteractively() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    std::fs::create_dir_all(project.path()).unwrap();

    let mut set = tracedecay_command(home.path(), project.path());
    set.args([
        "automation",
        "config",
        "set",
        "--scope",
        "global",
        "--backend",
        "codex-app-server",
        "--model",
        "global-model",
        "--timeout-secs",
        "75",
        "--max-tokens",
        "4096",
        "--temperature",
        "0.2",
        "--session-reflector",
        "true",
        "--session-reflector-schedule",
        "interval",
        "--session-reflector-interval-secs",
        "1800",
    ]);
    let output = run_with_timeout(set, cli_timeout());
    assert!(
        output.status.success(),
        "automation config global set should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("global set should print JSON");
    assert_eq!(payload["project"], serde_json::Value::Null);
    assert_eq!(payload["global"]["backend"], "codex_app_server");
    assert_eq!(payload["effective"]["model"], "global-model");
    assert_eq!(payload["effective"]["max_tokens"], 4096);
    assert!(
        (payload["effective"]["temperature"].as_f64().unwrap() - 0.2).abs() < 0.0001,
        "temperature should round-trip near 0.2: {}",
        payload["effective"]["temperature"]
    );
    assert_eq!(
        payload["effective"]["tasks"]["session_reflector"]["interval_secs"],
        1800
    );

    let config_toml = std::fs::read_to_string(profile_root(home.path()).join("config.toml"))
        .expect("global config should be saved");
    assert!(config_toml.contains("[automation]"));
    assert!(config_toml.contains("global-model"));

    let projects_dir = profile_root(home.path()).join("projects");
    assert!(
        !projects_dir.exists(),
        "global automation config must not create a project sidecar"
    );

    let mut get = tracedecay_command(home.path(), project.path());
    get.args(["automation", "config", "get", "--scope", "global", "--json"]);
    let get_output = run_with_timeout(get, cli_timeout());
    assert!(
        get_output.status.success(),
        "automation config global get should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&get_output.stdout),
        String::from_utf8_lossy(&get_output.stderr)
    );
    let get_payload: serde_json::Value =
        serde_json::from_slice(&get_output.stdout).expect("global get should print JSON");
    assert_eq!(get_payload["effective"]["backend"], "codex_app_server");
    assert_eq!(get_payload["effective"]["model"], "global-model");
}

#[test]
fn automation_config_set_rejects_unimplemented_external_backend() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    std::fs::create_dir_all(project.path()).unwrap();

    let mut set = tracedecay_command(home.path(), project.path());
    set.args([
        "automation",
        "config",
        "set",
        "--scope",
        "global",
        "--backend",
        "external-command",
    ]);
    let output = run_with_timeout(set, cli_timeout());
    assert!(
        !output.status.success(),
        "external backend should be rejected\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown automation backend"));
    assert!(stderr.contains("disabled, codex-app-server"));
}

#[test]
fn automation_config_set_writes_complete_project_sidecar_noninteractively() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    std::fs::create_dir_all(project.path().join("src")).unwrap();
    std::fs::write(project.path().join("src/lib.rs"), "pub fn marker() {}\n").unwrap();

    let mut init = tracedecay_command(home.path(), project.path());
    init.arg("init");
    let init_output = run_with_timeout(init, cli_timeout());
    assert!(
        init_output.status.success(),
        "init should succeed before automation config set\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&init_output.stdout),
        String::from_utf8_lossy(&init_output.stderr)
    );

    let mut set = tracedecay_command(home.path(), project.path());
    set.args([
        "automation",
        "config",
        "set",
        "--backend",
        "codex-app-server",
        "--host-mode",
        "standalone",
        "--model",
        "project-model",
        "--timeout-secs",
        "90",
        "--max-tokens",
        "2048",
        "--temperature",
        "0.3",
        "--require-dashboard-approval",
        "true",
        "--auto-apply-memory-ops",
        "true",
        "--auto-enable-skills",
        "true",
        "--memory-curator",
        "true",
        "--memory-curator-schedule",
        "manual",
        "--memory-curator-cooldown-secs",
        "300",
        "--session-reflector",
        "true",
        "--session-reflector-schedule",
        "interval",
        "--session-reflector-interval-secs",
        "1800",
        "--session-reflector-min-idle-secs",
        "60",
        "--skill-writer",
        "true",
        "--skill-writer-schedule",
        "interval",
        "--skill-writer-interval-secs",
        "3600",
        "--skill-writer-stale-lock-secs",
        "7200",
    ]);
    let output = run_with_timeout(set, cli_timeout());
    assert!(
        output.status.success(),
        "automation config set should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("project set should print JSON");
    assert_eq!(payload["project"]["backend"], "codex_app_server");
    assert_eq!(payload["project"]["max_tokens"], 2048);
    assert!(
        (payload["project"]["temperature"].as_f64().unwrap() - 0.3).abs() < 0.0001,
        "temperature should round-trip near 0.3: {}",
        payload["project"]["temperature"]
    );
    assert_eq!(payload["project"]["auto_apply_memory_ops"], true);
    assert_eq!(payload["project"]["auto_enable_skills"], true);
    assert_eq!(
        payload["project"]["session_reflector"]["interval_secs"],
        1800
    );
    assert_eq!(payload["project"]["skill_writer"]["stale_lock_secs"], 7200);
    assert_eq!(payload["effective"]["model"], "project-model");
    assert_eq!(
        payload["effective"]["tasks"]["memory_curator"]["cooldown_secs"],
        300
    );
    assert_eq!(
        payload["effective"]["tasks"]["session_reflector"]["min_idle_secs"],
        60
    );

    let projects_dir = profile_root(home.path()).join("projects");
    let sidecars = std::fs::read_dir(&projects_dir)
        .unwrap()
        .map(|entry| {
            entry
                .unwrap()
                .path()
                .join("dashboard/automation_config.json")
        })
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    assert_eq!(
        sidecars.len(),
        1,
        "automation config set should write one project sidecar under {projects_dir:?}, got {sidecars:?}"
    );
    let sidecar: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&sidecars[0]).unwrap()).unwrap();
    assert_eq!(sidecar["skill_writer"]["interval_secs"], 3600);
}

#[test]
fn automation_run_memory_curation_skips_without_backend_when_disabled() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    std::fs::create_dir_all(project.path().join("src")).unwrap();
    std::fs::write(project.path().join("src/lib.rs"), "pub fn marker() {}\n").unwrap();

    let mut init = tracedecay_command(home.path(), project.path());
    init.arg("init");
    let init_output = run_with_timeout(init, cli_timeout());
    assert!(
        init_output.status.success(),
        "init should succeed before automation run\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&init_output.stdout),
        String::from_utf8_lossy(&init_output.stderr)
    );

    let mut run = tracedecay_command(home.path(), project.path());
    run.args(["automation", "run", "memory-curation"]);
    let run_output = run_with_timeout(run, cli_timeout());
    assert!(
        run_output.status.success(),
        "disabled automation run should skip cleanly\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run_output.stdout),
        String::from_utf8_lossy(&run_output.stderr)
    );
    let payload: serde_json::Value =
        serde_json::from_slice(&run_output.stdout).expect("automation run should print JSON");
    assert_eq!(payload["ledger_record"]["status"], "skipped");
    assert_eq!(payload["ledger_record"]["trigger"], "manual_cli");
    assert_eq!(payload["ledger_record"]["error"], "automation_disabled");
    assert_eq!(payload["report"]["reason"], "automation_disabled");
    assert!(payload.get("backend_response").is_none());

    let ledger_paths = std::fs::read_dir(profile_root(home.path()).join("projects"))
        .unwrap()
        .map(|entry| {
            entry
                .unwrap()
                .path()
                .join("dashboard/automation_runs.jsonl")
        })
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    assert_eq!(
        ledger_paths.len(),
        1,
        "automation run should write one run ledger, got {ledger_paths:?}"
    );
    let ledger = std::fs::read_to_string(&ledger_paths[0]).unwrap();
    let record: serde_json::Value =
        serde_json::from_str(ledger.trim()).expect("ledger should contain one JSON record");
    assert_eq!(record["run_id"], payload["run_id"]);
    assert_eq!(record["status"], "skipped");
    assert_eq!(record["error"], "automation_disabled");

    let run_id = payload["run_id"]
        .as_str()
        .expect("automation run payload should include a run_id");
    let mut list = tracedecay_command(home.path(), project.path());
    list.args(["automation", "runs", "list", "--json", "--limit", "5"]);
    let list_output = run_with_timeout(list, cli_timeout());
    assert!(
        list_output.status.success(),
        "automation runs list should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&list_output.stdout),
        String::from_utf8_lossy(&list_output.stderr)
    );
    let list_payload: serde_json::Value =
        serde_json::from_slice(&list_output.stdout).expect("runs list should print JSON");
    assert_eq!(list_payload["count"], 1);
    assert_eq!(list_payload["records"][0]["run_id"], run_id);
    assert_eq!(list_payload["records"][0]["status"], "skipped");

    let mut view = tracedecay_command(home.path(), project.path());
    view.args(["automation", "runs", "view", run_id, "--json"]);
    let view_output = run_with_timeout(view, cli_timeout());
    assert!(
        view_output.status.success(),
        "automation runs view should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&view_output.stdout),
        String::from_utf8_lossy(&view_output.stderr)
    );
    let view_payload: serde_json::Value =
        serde_json::from_slice(&view_output.stdout).expect("runs view should print JSON");
    assert_eq!(view_payload["record"]["run_id"], run_id);
    assert_eq!(view_payload["record"]["error"], "automation_disabled");

    let dashboard_root = ledger_paths[0]
        .parent()
        .expect("ledger should live under dashboard root")
        .to_path_buf();
    let mut artifact_record: AutomationRunLedgerRecord =
        serde_json::from_str(ledger.trim()).expect("ledger should deserialize as run record");
    let artifact_payload = serde_json::json!({
        "loop_stage": "codex_handoff",
        "run_id": run_id,
        "status": "ready_for_review",
    });
    let runtime = create_runtime();
    let artifact = runtime
        .block_on(write_run_artifact(
            &dashboard_root,
            run_id,
            AutomationRunArtifactKind::CodexHandoff,
            &artifact_payload,
            Some("CLI handoff artifact".to_string()),
            "2026-06-24T05:00:02Z",
        ))
        .expect("artifact write should succeed");
    artifact_record.artifacts = vec![artifact];
    runtime
        .block_on(append_run_record(&dashboard_root, &artifact_record))
        .expect("artifact ledger append should succeed");

    let mut artifact_view = tracedecay_command(home.path(), project.path());
    artifact_view.args([
        "automation",
        "runs",
        "artifact",
        run_id,
        "codex_handoff",
        "--json",
    ]);
    let artifact_output = run_with_timeout(artifact_view, cli_timeout());
    assert!(
        artifact_output.status.success(),
        "automation runs artifact should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&artifact_output.stdout),
        String::from_utf8_lossy(&artifact_output.stderr)
    );
    let artifact_view_payload: serde_json::Value =
        serde_json::from_slice(&artifact_output.stdout).expect("artifact view should print JSON");
    assert_eq!(artifact_view_payload["run_id"], run_id);
    assert_eq!(artifact_view_payload["artifact"]["kind"], "codex_handoff");
    assert_eq!(
        artifact_view_payload["payload"]["status"],
        "ready_for_review"
    );
}

#[test]
fn automation_run_session_reflection_skips_without_backend_when_disabled() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    std::fs::create_dir_all(project.path().join("src")).unwrap();
    std::fs::write(project.path().join("src/lib.rs"), "pub fn marker() {}\n").unwrap();

    let mut init = tracedecay_command(home.path(), project.path());
    init.arg("init");
    let init_output = run_with_timeout(init, cli_timeout());
    assert!(
        init_output.status.success(),
        "init should succeed before automation run\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&init_output.stdout),
        String::from_utf8_lossy(&init_output.stderr)
    );

    let mut run = tracedecay_command(home.path(), project.path());
    run.args(["automation", "run", "session-reflection"]);
    let run_output = run_with_timeout(run, cli_timeout());
    assert!(
        run_output.status.success(),
        "disabled session reflection run should skip cleanly\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run_output.stdout),
        String::from_utf8_lossy(&run_output.stderr)
    );
    let payload: serde_json::Value =
        serde_json::from_slice(&run_output.stdout).expect("automation run should print JSON");
    assert_eq!(payload["ledger_record"]["task"], "session_reflector");
    assert_eq!(payload["ledger_record"]["status"], "skipped");
    assert_eq!(payload["ledger_record"]["trigger"], "manual_cli");
    assert_eq!(payload["ledger_record"]["error"], "automation_disabled");
    assert_eq!(payload["report"]["reason"], "automation_disabled");
    assert!(payload.get("backend_response").is_none());
}

#[test]
fn automation_run_skill_writing_skips_without_backend_when_disabled() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    std::fs::create_dir_all(project.path().join("src")).unwrap();
    std::fs::write(project.path().join("src/lib.rs"), "pub fn marker() {}\n").unwrap();

    let mut init = tracedecay_command(home.path(), project.path());
    init.arg("init");
    let init_output = run_with_timeout(init, cli_timeout());
    assert!(
        init_output.status.success(),
        "init should succeed before automation run\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&init_output.stdout),
        String::from_utf8_lossy(&init_output.stderr)
    );

    let mut run = tracedecay_command(home.path(), project.path());
    run.args(["automation", "run", "skill-writing"]);
    let run_output = run_with_timeout(run, cli_timeout());
    assert!(
        run_output.status.success(),
        "disabled skill writing run should skip cleanly\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run_output.stdout),
        String::from_utf8_lossy(&run_output.stderr)
    );
    let payload: serde_json::Value =
        serde_json::from_slice(&run_output.stdout).expect("automation run should print JSON");
    assert_eq!(payload["ledger_record"]["task"], "skill_writer");
    assert_eq!(payload["ledger_record"]["status"], "skipped");
    assert_eq!(payload["ledger_record"]["trigger"], "manual_cli");
    assert_eq!(payload["ledger_record"]["error"], "automation_disabled");
    assert_eq!(payload["report"]["reason"], "automation_disabled");
    assert!(payload.get("backend_response").is_none());
}

#[test]
fn bare_invocation_skips_create_prompt_when_stdin_not_a_terminal() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    std::fs::create_dir_all(project.path().join("src")).unwrap();
    std::fs::write(project.path().join("src/lib.rs"), "pub fn marker() {}\n").unwrap();

    let output = run_with_timeout(
        tracedecay_command(home.path(), project.path()),
        cli_timeout(),
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
    let output = run_with_timeout(command, cli_timeout());

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
    let output = run_with_timeout(command, cli_timeout());

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
    let output = run_with_timeout(command, cli_timeout());

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
async fn projects_list_json_reads_global_registry() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let db = GlobalDb::open_at(&profile_root(home.path()).join("global.db"))
        .await
        .unwrap();
    register_profile_sharded_store(&db, project.path(), "proj_cli").await;
    drop(db);

    let mut command = tracedecay_command(home.path(), project.path());
    command.args(["projects", "list", "--json"]);
    let output = run_with_timeout(command, cli_timeout());

    assert!(
        output.status.success(),
        "projects list --json should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["projects"][0]["project_id"], "proj_cli");
    assert_eq!(payload["projects"][0]["default_branch"], "main");
}

#[tokio::test]
async fn projects_search_text_matches_registered_alias() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let db = GlobalDb::open_at(&profile_root(home.path()).join("global.db"))
        .await
        .unwrap();
    register_profile_sharded_store(&db, project.path(), "proj_cli").await;
    drop(db);

    let mut command = tracedecay_command(home.path(), project.path());
    command.args(["projects", "search", "proj_cli"]);
    let output = run_with_timeout(command, cli_timeout());

    assert!(
        output.status.success(),
        "projects search should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("proj_cli") && stdout.contains("main"),
        "search output should include project id and branch\nstdout:\n{stdout}"
    );
}

#[tokio::test]
async fn projects_context_resolves_project_id_and_path() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let db = GlobalDb::open_at(&profile_root(home.path()).join("global.db"))
        .await
        .unwrap();
    register_profile_sharded_store(&db, project.path(), "proj_cli").await;
    drop(db);

    let mut by_id = tracedecay_command(home.path(), project.path());
    by_id.args(["projects", "context", "proj_cli", "--json"]);
    let by_id_output = run_with_timeout(by_id, cli_timeout());
    assert!(
        by_id_output.status.success(),
        "projects context by id should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&by_id_output.stdout),
        String::from_utf8_lossy(&by_id_output.stderr)
    );
    let by_id_payload: serde_json::Value = serde_json::from_slice(&by_id_output.stdout).unwrap();
    assert_eq!(by_id_payload["project"]["project_id"], "proj_cli");
    assert_eq!(
        by_id_payload["stores"][0]["store"]["storage_mode"],
        "profile_sharded"
    );

    let mut by_path = tracedecay_command(home.path(), project.path());
    by_path.args(["projects", "context", project.path().to_str().unwrap()]);
    let by_path_output = run_with_timeout(by_path, cli_timeout());
    assert!(
        by_path_output.status.success(),
        "projects context by path should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&by_path_output.stdout),
        String::from_utf8_lossy(&by_path_output.stderr)
    );
    let stdout = String::from_utf8_lossy(&by_path_output.stdout);
    assert!(
        stdout.contains("Project: proj_cli") && stdout.contains("profile_sharded"),
        "path context output should include project and store\nstdout:\n{stdout}"
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
    let output = run_with_timeout(command, cli_timeout());

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
    let output = run_with_timeout(command, cli_timeout());

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
    let output = run_with_timeout(command, cli_timeout());

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
    write_profile_sharded_branch_fixture(home.path(), project.path());
    let shard_root = profile_shard_root(home.path());
    write_branch_meta(&shard_root, &[], false);

    let mut command = tracedecay_command(home.path(), project.path());
    command.args(["branch", "add", "feature/new"]);
    let output = run_with_timeout(command, cli_timeout());
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
    let output = run_with_timeout(command, cli_timeout());

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
    let output = run_with_timeout(command, cli_timeout());

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
    let output = run_with_timeout(command, cli_timeout());

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
    let output = run_with_timeout(command, cli_timeout());

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
    let output = run_with_timeout(command, cli_timeout());

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
    let output = run_with_timeout(command, cli_timeout());

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
    let output = run_with_timeout(command, cli_timeout());

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
