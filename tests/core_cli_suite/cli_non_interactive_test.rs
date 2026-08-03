use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

use crate::common::{MessageRecordBuilder, create_runtime, global_session, sample_node};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;
use tracedecay::application::host_admission::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
use tracedecay::automation::run_ledger::{
    AutomationRunArtifactKind, AutomationRunLedgerRecord, append_run_record, write_run_artifact,
};
use tracedecay::branch_meta::BranchMeta;
use tracedecay::global_db::StoreInstanceUpsert;
use tracedecay::storage::{
    EnrollmentMarker, STORE_MANIFEST_FILENAME, STORE_MANIFEST_SCHEMA_VERSION, StorageMode,
    StoreKind, StoreManifest, default_profile_project_id, profile_sharded_data_root,
    profile_sharded_layout, read_enrollment_marker, write_enrollment_marker,
    write_repository_identity_marker, write_store_manifest,
};
use tracedecay_domain::ProjectId;

fn canonical_temp_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// A directory guaranteed to sit outside `std::env::temp_dir()`, for fixtures
/// that must NOT be classified as "ephemeral" by
/// `migrate::registry::classify_project_root` (which rejects project roots
/// under the OS temp directory). `env!("CARGO_MANIFEST_DIR")).parent()` used
/// to serve this purpose, but that only holds when the checkout itself lives
/// outside the temp directory; a repo cloned under `/tmp` (as some sandboxed
/// CI/dev environments do) breaks that assumption. Deriving the base from the
/// running test binary's own on-disk location is robust regardless of where
/// the checkout lives, because cargo (or any build-cache shim in front of it)
/// never places build output inside the volatile system temp directory.
fn ephemeral_safe_fixture_base() -> PathBuf {
    let exe = std::env::current_exe().expect("test binary has a current_exe path");
    let profile_dir = exe
        .parent() // .../target/<profile>/deps
        .and_then(Path::parent) // .../target/<profile>
        .expect("test binary sits under a cargo target profile directory")
        .to_path_buf();
    let base = profile_dir.join("clone-path-hermetic-fixtures");
    std::fs::create_dir_all(&base).unwrap();
    base
}

fn profile_root(home: &Path) -> PathBuf {
    canonical_temp_path(home).join(".tracedecay")
}

fn profile_shard_root(home: &Path) -> PathBuf {
    profile_root(home).join("projects/proj_cli")
}

fn tracedecay_command_without_daemon(home: &std::path::Path, project: &std::path::Path) -> Command {
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

fn tracedecay_command(home: &std::path::Path, project: &std::path::Path) -> Command {
    crate::common::ensure_tracedecay_daemon(home);
    tracedecay_command_without_daemon(home, project)
}

fn tracedecay_command_with_stdin_without_daemon(
    home: &std::path::Path,
    project: &std::path::Path,
) -> Command {
    let mut command = tracedecay_command_without_daemon(home, project);
    command.stdin(Stdio::piped());
    command
}

fn cli_timeout() -> Duration {
    Duration::from_secs(90)
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

/// Initializes the profile-sharded project store through the daemon-owned
/// runtime. Only for tests where init is setup, not the behaviour under test.
fn init_project_fixture(home: &Path, project: &Path) {
    let project = canonical_temp_path(project);
    let daemon = crate::common::spawn_tracedecay_daemon(home);
    let mut command = tracedecay_command_without_daemon(home, &project);
    command.args(["init", "."]);
    let output = run_with_timeout(command, cli_timeout());
    assert!(
        output.status.success(),
        "fixture init should run\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    drop(daemon);
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

fn write_git_fixture(project: &Path) {
    git(project, &["init", "-b", "main"]);
    std::fs::write(project.join("lib.rs"), "pub fn indexed() {}\n").unwrap();
    commit_all(project, "fixture repository");
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

#[test]
fn sessions_unfinished_lists_workflow_state_evidence() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_root = canonical_temp_path(project.path());
    std::fs::write(project_root.join("lib.rs"), "pub fn indexed() {}\n").unwrap();
    init_project_fixture(home.path(), &project_root);
    let project_id = default_profile_project_id(&project_root);

    create_runtime().block_on(async {
        let runtime = HostAdmissionTestRuntimeV1::project(
            profile_root(home.path()),
            &project_root,
            ProjectId::new(project_id).expect("valid fixture project id"),
        )
        .await
        .expect("registered project runtime");
        assert!(
            runtime
                .upsert_session_for_test(
                    HostAdmissionScope::Project,
                    &global_session("claude", "session-1", "proj_cli"),
                )
                .await
                .expect("session fixture write")
        );
        assert!(
            runtime
                .upsert_session_message_for_test(
                    HostAdmissionScope::Project,
                    &MessageRecordBuilder::new(
                        "claude",
                        "message-1",
                        "session-1",
                        "assistant",
                        1,
                        "Blocked: waiting on missing deploy credentials",
                        "message",
                    )
                    .with_source(Some("/tmp/project/transcript.jsonl"), Some(1))
                    .with_metadata(Some(r#"{"task_id":"task-7"}"#))
                    .build(),
                )
                .await
                .expect("session message fixture write")
        );
    });

    let mut command = tracedecay_command(home.path(), &project_root);
    command.args(["sessions", "unfinished", "--json"]);
    let output = run_with_timeout(command, cli_timeout());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "sessions unfinished should succeed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains(r#""status": "blocked""#), "{stdout}");
    assert!(stdout.contains(r#""session_id": "session-1""#), "{stdout}");
    assert!(stdout.contains(r#""task_id": "task-7""#), "{stdout}");
    assert!(stdout.contains("missing deploy credentials"), "{stdout}");
}

#[test]
fn sessions_search_omits_absent_optional_filters_and_preserves_provider() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_root = canonical_temp_path(project.path());
    std::fs::write(project_root.join("lib.rs"), "pub fn indexed() {}\n").unwrap();
    init_project_fixture(home.path(), &project_root);
    let project_id = default_profile_project_id(&project_root);

    create_runtime().block_on(async {
        let runtime = HostAdmissionTestRuntimeV1::project(
            profile_root(home.path()),
            &project_root,
            ProjectId::new(project_id).expect("valid fixture project id"),
        )
        .await
        .expect("registered project runtime");
        runtime
            .upsert_session_for_test(
                HostAdmissionScope::Project,
                &global_session("cursor", "session-search", "proj_cli"),
            )
            .await
            .expect("session fixture write");
        runtime
            .upsert_session_message_for_test(
                HostAdmissionScope::Project,
                &MessageRecordBuilder::new(
                    "cursor",
                    "message-search",
                    "session-search",
                    "assistant",
                    1,
                    "dogfood recovery evidence",
                    "message",
                )
                .build(),
            )
            .await
            .expect("session message fixture write");
    });

    let _daemon = crate::common::spawn_tracedecay_daemon(home.path());
    for extra_args in [vec![], vec!["--provider", "cursor"]] {
        let mut command = tracedecay_command_without_daemon(home.path(), &project_root);
        command.args(["sessions", "search", "dogfood", "--limit", "3"]);
        command.args(extra_args);
        let output = run_with_timeout(command, cli_timeout());
        assert!(
            output.status.success(),
            "sessions search should accept omitted optional filters\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
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
    let graph_db_path = shard_root.join("tracedecay.db");
    std::thread::spawn(move || {
        create_runtime()
            .block_on(crate::common::initialize_test_database(&graph_db_path))
            .unwrap();
    })
    .join()
    .unwrap();
    write_sqlite_placeholder(&shard_root.join("sessions.db"));
    write_branch_meta(&shard_root, &[], false);
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
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        create_runtime().block_on(async {
            let (db, _) = crate::common::initialize_test_database(&path)
                .await
                .unwrap();
            db.close();
        });
    })
    .join()
    .unwrap();
}

fn write_empty_sqlite_fixture(path: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    drop(rusqlite::Connection::open(path).expect("empty SQLite fixture"));
}

async fn register_profile_sharded_store(
    runtime: &HostAdmissionTestRuntimeV1,
    project_root: &std::path::Path,
    project_id: &str,
) {
    runtime.upsert(project_root, 42).await;
    runtime
        .upsert_code_project(project_id, project_root, None, None, Some("main"))
        .await
        .expect("code project should upsert");
    runtime
        .upsert_store_instance(StoreInstanceUpsert {
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
            write_empty_sqlite_fixture(&db_path);
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
        stderr.contains("initialized and indexed"),
        "stderr should confirm non-interactive initialization\nstderr:\n{stderr}"
    );
}

#[test]
fn explicit_kimi_install_fails_with_interactive_remediation() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let kimi_home = canonical_temp_path(home.path()).join(".kimi-code");
    let mut install = tracedecay_command_without_daemon(home.path(), project.path());
    let _shim = add_tracedecay_path_shim(&mut install, home.path());
    install
        .env(tracedecay::agents::kimi::KIMI_CODE_HOME_ENV, &kimi_home)
        .args(["install", "--agent", "kimi"]);

    let output = run_with_timeout(install, cli_timeout());

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("interactive `/plugins` host API"));
    assert!(stderr.contains("/plugins install"));
    assert!(stderr.contains("made no current plugin registration changes"));
    assert!(
        canonical_temp_path(home.path())
            .join(".tracedecay/host-bundle-stage/kimi/tracedecay/.kimi-plugin/plugin.json")
            .is_file()
    );
    assert!(!kimi_home.join("plugins/installed.json").exists());
}

fn run_codex_automation_install(
    home: &TempDir,
    project_root: &Path,
    extra_args: &[&str],
) -> Output {
    let mut install = tracedecay_command(home.path(), project_root);
    let _shim = add_tracedecay_path_shim(&mut install, home.path());
    install.args(["install", "--agent", "codex", "--automation"]);
    install.args(extra_args);
    let output = run_with_timeout(install, cli_timeout());
    assert!(
        output.status.success(),
        "codex automation install should succeed non-interactively\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn read_codex_automation_sidecar(home: &TempDir) -> serde_json::Value {
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
        "codex automation install should write one TraceDecay project scheduler sidecar"
    );
    serde_json::from_slice(
        &std::fs::read(&sidecars[0]).expect("automation sidecar should be readable"),
    )
    .expect("automation sidecar should be valid JSON")
}

#[test]
fn install_codex_automation_enables_tracedecay_daemon_loop_noninteractively() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_root = canonical_temp_path(project.path());
    std::fs::create_dir_all(project_root.join("src")).unwrap();
    std::fs::write(project_root.join("src/lib.rs"), "pub fn marker() {}\n").unwrap();

    let legacy_automation_dir = home
        .path()
        .join(".codex/automations/watch-tracedecay-memory");
    std::fs::create_dir_all(&legacy_automation_dir).unwrap();
    std::fs::write(
        legacy_automation_dir.join("automation.toml"),
        "status = \"ACTIVE\"\n",
    )
    .unwrap();

    run_codex_automation_install(&home, &project_root, &[]);

    assert!(
        home.path()
            .join("plugins/tracedecay/.codex-plugin/plugin.json")
            .is_file(),
        "install --agent codex should still install the Codex plugin bundle"
    );
    assert!(
        !legacy_automation_dir.exists(),
        "Codex automation install must remove the legacy native scheduled automation"
    );
    assert!(
        !project_root.join(".codex/automations").exists(),
        "Codex automation install must not create repo-local Codex automation files"
    );
    let sidecar = read_codex_automation_sidecar(&home);
    assert_eq!(sidecar["enabled"], true);
    assert_eq!(sidecar["backend"], "codex_app_server");
    assert_eq!(sidecar["host_mode"], "standalone");
    assert!(sidecar.get("model").is_none());
    assert!(sidecar.get("require_dashboard_approval").is_none());
    assert!(
        sidecar.get("auto_apply_memory_ops").is_none(),
        "install must not enable unattended memory ops by default: {sidecar}"
    );
    assert!(
        sidecar.get("auto_enable_skills").is_none(),
        "install must leave skill auto-enablement at its default: {sidecar}"
    );
    assert_eq!(sidecar["memory_curator"]["enabled"], true);
    assert_eq!(sidecar["memory_curator"]["schedule"], "interval");
    assert_eq!(sidecar["memory_curator"]["interval_secs"], 900);
    assert_eq!(sidecar["session_reflector"]["enabled"], true);
    assert_eq!(sidecar["session_reflector"]["interval_secs"], 900);
    assert_eq!(sidecar["skill_writer"]["enabled"], true);
    assert_eq!(sidecar["skill_writer"]["interval_secs"], 3600);
    assert_eq!(sidecar["skill_writer"]["min_idle_secs"], 900);
}

#[test]
fn install_codex_automation_auto_apply_flag_opts_into_unattended_memory_ops() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_root = canonical_temp_path(project.path());
    std::fs::create_dir_all(project_root.join("src")).unwrap();
    std::fs::write(project_root.join("src/lib.rs"), "pub fn marker() {}\n").unwrap();

    let output = run_codex_automation_install(&home, &project_root, &["--auto-apply"]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("without dashboard approval"),
        "opting into --auto-apply should print an explicit warning\nstderr:\n{stderr}"
    );

    let sidecar = read_codex_automation_sidecar(&home);
    assert_eq!(sidecar["enabled"], true);
    assert!(sidecar.get("require_dashboard_approval").is_none());
    assert_eq!(sidecar["auto_apply_memory_ops"], true);
    assert!(
        sidecar.get("auto_enable_skills").is_none(),
        "--auto-apply must not touch skill auto-enablement: {sidecar}"
    );
}

#[test]
fn automation_config_enable_writes_project_sidecar_noninteractively() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    std::fs::create_dir_all(project.path().join("src")).unwrap();
    std::fs::write(project.path().join("src/lib.rs"), "pub fn marker() {}\n").unwrap();

    init_project_fixture(home.path(), project.path());
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
        "--timeout-secs",
        "75",
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
    assert!(payload["effective"].get("model").is_none());
    assert!(payload["effective"].get("max_tokens").is_none());
    assert!(payload["effective"].get("temperature").is_none());
    assert_eq!(
        payload["effective"]["tasks"]["session_reflector"]["interval_secs"],
        1800
    );

    let config_toml = std::fs::read_to_string(profile_root(home.path()).join("config.toml"))
        .expect("global config should be saved");
    assert!(config_toml.contains("[automation]"));
    assert!(!config_toml.contains("model"));

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
    assert!(get_payload["effective"].get("model").is_none());
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

    init_project_fixture(home.path(), project.path());

    let mut set = tracedecay_command(home.path(), project.path());
    set.args([
        "automation",
        "config",
        "set",
        "--backend",
        "codex-app-server",
        "--host-mode",
        "standalone",
        "--timeout-secs",
        "90",
        "--auto-apply-memory-ops",
        "true",
        "--auto-enable-skills",
        "true",
        "--export-memory-digest",
        "false",
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
    assert!(payload["project"].get("model").is_none());
    assert!(payload["project"].get("max_tokens").is_none());
    assert!(payload["project"].get("temperature").is_none());
    assert!(
        payload["project"]
            .get("require_dashboard_approval")
            .is_none()
    );
    assert_eq!(payload["project"]["auto_apply_memory_ops"], true);
    assert_eq!(payload["project"]["auto_enable_skills"], true);
    assert_eq!(payload["project"]["export_memory_digest"], false);
    assert_eq!(payload["effective"]["export_memory_digest"], false);
    assert_eq!(
        payload["project"]["session_reflector"]["interval_secs"],
        1800
    );
    assert_eq!(payload["project"]["skill_writer"]["stale_lock_secs"], 7200);
    assert!(payload["effective"].get("model").is_none());
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

    init_project_fixture(home.path(), project.path());

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

    init_project_fixture(home.path(), project.path());

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

    init_project_fixture(home.path(), project.path());

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
fn automation_run_rejects_removed_hermes_storage_flags() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();

    for (task, flag, value) in [
        ("session-reflection", "--storage-scope", "hermes_profile"),
        ("session-reflection", "--hermes-home", "/tmp/hermes"),
        ("skill-writing", "--storage-scope", "hermes_profile"),
        ("skill-writing", "--hermes-home", "/tmp/hermes"),
    ] {
        let mut run = tracedecay_command(home.path(), project.path());
        run.args(["automation", "run", task, flag, value]);
        let output = run_with_timeout(run, cli_timeout());
        assert!(
            !output.status.success(),
            "{task} must reject removed {flag}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("unexpected argument") && stderr.contains(flag),
            "{task} should report {flag} as unknown:\n{stderr}"
        );
    }
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
fn status_reports_uninitialized_project_without_creating_it() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    std::fs::create_dir_all(project.path().join("src")).unwrap();
    std::fs::write(project.path().join("src/lib.rs"), "pub fn marker() {}\n").unwrap();

    let mut command = tracedecay_command(home.path(), project.path());
    command.arg("status");
    let output = run_with_timeout(command, cli_timeout());

    assert!(!output.status.success(), "uninitialized status must fail");
    assert!(
        !project.path().join(".tracedecay").exists(),
        "status must not create an index non-interactively"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no TraceDecay index found") && stderr.contains("tracedecay init"),
        "stderr should explain how to initialize the project\nstderr:\n{stderr}"
    );
}

#[tokio::test]
async fn status_surfaces_split_identity_conflict_without_suggesting_init() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_root = canonical_temp_path(project.path());
    git(&project_root, &["init", "-b", "main"]);

    for (project_id, node_id) in [
        ("proj_status_selected", "status-selected-node"),
        ("proj_status_legacy", "status-legacy-node"),
    ] {
        let layout = profile_sharded_layout(
            &project_root,
            &profile_root(home.path()),
            &EnrollmentMarker {
                project_id: project_id.to_string(),
                storage_mode: StorageMode::ProfileSharded,
            },
        )
        .unwrap();
        let (db, _) = crate::common::initialize_test_database(&layout.graph_db_path)
            .await
            .unwrap();
        db.insert_node(&sample_node(node_id, node_id, "src/lib.rs"))
            .await
            .unwrap();
        db.checkpoint().await.unwrap();
        db.close();
        write_store_manifest(&layout).unwrap();
    }
    // Only the repository identity marker, deliberately: an enrollment marker
    // is a current-generation authority that resolves this checkout outright,
    // short-circuiting the legacy-candidate scan that detects the split. A
    // cutover conflict can only exist on a pre-enrollment checkout, so writing
    // one here would model a state in which the conflict cannot arise.
    write_repository_identity_marker(&project_root, "proj_status_selected").unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(profile_root(home.path()))
        .await
        .unwrap();
    register_profile_sharded_store(&runtime, &project_root, "proj_status_selected").await;
    runtime.checkpoint_profile_database_for_test().await;
    drop(runtime);

    let selected_db = profile_root(home.path()).join("projects/proj_status_selected/tracedecay.db");
    let legacy_db = profile_root(home.path()).join("projects/proj_status_legacy/tracedecay.db");
    let selected_before = std::fs::read(&selected_db).unwrap();
    let legacy_before = std::fs::read(&legacy_db).unwrap();

    let mut command = tracedecay_command(home.path(), &project_root);
    command.arg("status");
    let output = run_with_timeout(command, cli_timeout());
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "status should fail safely\n{stderr}"
    );
    assert!(stderr.contains("identity cutover conflict"), "{stderr}");
    assert!(stderr.contains("proj_status_selected"), "{stderr}");
    assert!(stderr.contains("proj_status_legacy"), "{stderr}");
    assert!(
        stderr.contains("choose one shard and retire the other"),
        "{stderr}"
    );
    assert!(!stderr.contains("run `tracedecay init`"), "{stderr}");
    assert_eq!(std::fs::read(selected_db).unwrap(), selected_before);
    assert_eq!(std::fs::read(legacy_db).unwrap(), legacy_before);
}

#[cfg(unix)]
#[tokio::test]
async fn status_json_reads_readonly_project_database() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_root = canonical_temp_path(project.path());
    std::fs::create_dir_all(project_root.join("src")).unwrap();
    std::fs::write(
        project_root.join("src/lib.rs"),
        "pub fn process_data() {}\n",
    )
    .unwrap();
    let home_path = home.path().to_path_buf();
    let init_root = project_root.clone();
    std::thread::spawn(move || init_project_fixture(&home_path, &init_root))
        .join()
        .unwrap();
    let marker = read_enrollment_marker(&project_root)
        .unwrap()
        .expect("fixture init writes enrollment");
    let db_path = profile_sharded_layout(&project_root, &profile_root(home.path()), &marker)
        .unwrap()
        .graph_db_path;
    let (db, _) = crate::common::open_test_database(&db_path).await.unwrap();
    db.insert_node(&sample_node("node-1", "process_data", "src/lib.rs"))
        .await
        .unwrap();
    let expected_node_count = db.get_stats().await.unwrap().node_count;
    assert_eq!(expected_node_count, 3);
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
    assert_eq!(payload["node_count"], expected_node_count);
}

#[tokio::test]
async fn list_all_reports_profile_sharded_store_without_stale_label() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write_git_fixture(project.path());
    write_profile_sharded_fixture(home.path(), project.path());
    let runtime = HostAdmissionTestRuntimeV1::profile(profile_root(home.path()))
        .await
        .unwrap();
    register_profile_sharded_store(&runtime, project.path(), "proj_cli").await;
    runtime.checkpoint_profile_database_for_test().await;
    drop(runtime);

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
    let runtime = HostAdmissionTestRuntimeV1::profile(profile_root(home.path()))
        .await
        .unwrap();
    register_profile_sharded_store(&runtime, project.path(), "proj_cli").await;
    runtime.checkpoint_profile_database_for_test().await;
    drop(runtime);

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
    assert_eq!(payload["summary"]["project_count"], 1);
    assert_eq!(
        payload["project_tree"][0]["projects"][0]["project_id"],
        "proj_cli"
    );
    assert_eq!(
        payload["project_tree"][0]["projects"][0]["branches"][0],
        "main"
    );
}

#[tokio::test]
async fn projects_search_text_matches_registered_alias() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(profile_root(home.path()))
        .await
        .unwrap();
    register_profile_sharded_store(&runtime, project.path(), "proj_cli").await;
    runtime.checkpoint_profile_database_for_test().await;
    drop(runtime);

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
    assert!(
        stdout.contains("Repositories") && stdout.contains("branches: main"),
        "search output should render compact project tree\nstdout:\n{stdout}"
    );
}

#[tokio::test]
async fn projects_context_resolves_project_id_and_path() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(profile_root(home.path()))
        .await
        .unwrap();
    register_profile_sharded_store(&runtime, project.path(), "proj_cli").await;
    runtime.checkpoint_profile_database_for_test().await;
    drop(runtime);

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
async fn projects_context_resolves_linked_worktree_path_by_git_common_dir() {
    let home = TempDir::new().unwrap();
    let dir = TempDir::new().unwrap();
    let main = canonical_temp_path(&dir.path().join("main"));
    let linked = canonical_temp_path(&dir.path().join("linked"));
    std::fs::create_dir_all(&main).unwrap();
    git(&main, &["init", "-b", "main"]);
    std::fs::write(main.join("README.md"), "linked worktree fixture\n").unwrap();
    commit_all(&main, "initial commit");
    git(
        &main,
        &[
            "worktree",
            "add",
            "-b",
            "feature/worktree-context",
            linked.to_str().unwrap(),
            "HEAD",
        ],
    );

    let runtime = HostAdmissionTestRuntimeV1::profile(profile_root(home.path()))
        .await
        .unwrap();
    runtime
        .upsert_code_project(
            "proj_cli_worktree",
            &main,
            Some(&main.join(".git")),
            None,
            Some("main"),
        )
        .await
        .expect("code project should upsert with git common-dir alias");
    runtime
        .upsert_store_instance(StoreInstanceUpsert {
            store_id: "store:proj_cli_worktree:profile_sharded".to_string(),
            project_id: "proj_cli_worktree".to_string(),
            store_kind: "code_project".to_string(),
            storage_mode: "profile_sharded".to_string(),
            store_relpath: "projects/proj_cli_worktree".to_string(),
            manifest_relpath: Some(STORE_MANIFEST_FILENAME.to_string()),
            last_verified_at: Some(1_800_000_000),
            last_write_at: Some(1_800_000_000),
        })
        .await
        .expect("store instance should upsert");
    runtime.checkpoint_profile_database_for_test().await;
    drop(runtime);

    let mut command = tracedecay_command(home.path(), &linked);
    command.args(["projects", "context", linked.to_str().unwrap(), "--json"]);
    let output = run_with_timeout(command, cli_timeout());

    assert!(
        output.status.success(),
        "projects context should resolve linked worktree path through git common dir\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["project"]["project_id"], "proj_cli_worktree");
    assert_eq!(
        payload["stores"][0]["store"]["storage_mode"],
        "profile_sharded"
    );
}

#[tokio::test]
async fn wipe_all_removes_profile_sharded_store_and_global_row() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write_profile_sharded_fixture(home.path(), project.path());
    let shard_root = profile_shard_root(home.path());
    let profile_root = profile_root(home.path());
    let runtime = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .unwrap();
    register_profile_sharded_store(&runtime, project.path(), "proj_cli").await;
    runtime.checkpoint_profile_database_for_test().await;
    drop(runtime);

    let mut command = tracedecay_command_with_stdin_without_daemon(home.path(), project.path());
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
    assert!(!shard_root.join("tracedecay.db").exists());
    assert!(!shard_root.join(STORE_MANIFEST_FILENAME).exists());
    let reopened = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .unwrap();
    assert!(
        reopened.list_project_paths_compat().await.is_empty(),
        "global projects table should be empty after wipe --all"
    );
}

#[test]
fn list_all_reports_orphan_manifest_reconstructable_store() {
    let home = TempDir::new().unwrap();
    let project = tempfile::Builder::new()
        .prefix("list-orphan-project-")
        .tempdir_in(ephemeral_safe_fixture_base())
        .unwrap();
    git(project.path(), &["init"]);
    write_profile_sharded_fixture(home.path(), project.path());
    write_repository_identity_marker(project.path(), "proj_cli").unwrap();
    write_enrollment_marker(
        project.path(),
        &EnrollmentMarker {
            project_id: "proj_cli".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    std::fs::create_dir_all(profile_root(home.path())).unwrap();

    let report = tracedecay::migrate::registry::scan_profile_store_manifests(
        &profile_root(home.path()),
        tracedecay::tracedecay::current_timestamp(),
    );
    assert_eq!(report.plans.len(), 1, "{report:#?}");
    assert_eq!(
        report.plans[0].status,
        tracedecay::migrate::registry::RegistryReconstructionStatus::Eligible,
        "{report:#?}"
    );

    let mut command = tracedecay_command(home.path(), project.path());
    command.args(["list", "--all"]);
    let output = run_with_timeout(command, cli_timeout());

    assert!(
        output.status.success(),
        "list --all should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // The shard exists on disk with a reconstructable manifest but was never
    // registered, so `list --all` must say exactly that. Reporting it as a
    // plain `profile-sharded` row would promote an unregistered store to a
    // registered-looking one — the ambient registry fallback this fixture
    // exists to forbid. `list_all_uses_registry_profile_shard_when_enrollment_marker_missing`
    // covers the registered spelling.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[orphan manifest-reconstructable]"),
        "unregistered reconstructable shard must be reported as an orphan\nstdout:\n{stdout}"
    );
    assert!(
        stdout.contains(&canonical_temp_path(project.path()).display().to_string()),
        "orphan row must name the project root\nstdout:\n{stdout}"
    );
    assert!(
        !stdout.contains("stale"),
        "a reconstructable shard is not stale\nstdout:\n{stdout}"
    );
    assert!(
        !stdout.contains("[profile-sharded]"),
        "unregistered shard must not be labelled as a registered profile shard\nstdout:\n{stdout}"
    );
}

#[tokio::test]
async fn list_all_uses_registry_profile_shard_when_enrollment_marker_missing() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write_profile_sharded_fixture(home.path(), project.path());
    std::fs::remove_dir_all(project.path().join(".tracedecay")).unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(profile_root(home.path()))
        .await
        .unwrap();
    register_profile_sharded_store(&runtime, project.path(), "proj_cli").await;
    runtime.checkpoint_profile_database_for_test().await;
    drop(runtime);

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
    let runtime = HostAdmissionTestRuntimeV1::profile(profile_root(home.path()))
        .await
        .unwrap();
    register_profile_sharded_store(&runtime, project.path(), "proj_cli").await;
    runtime.checkpoint_profile_database_for_test().await;
    drop(runtime);

    let mut command = tracedecay_command_with_stdin_without_daemon(home.path(), project.path());
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

#[tokio::test]
async fn branch_list_reads_profile_sharded_branch_meta() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write_git_fixture(project.path());
    write_profile_sharded_fixture(home.path(), project.path());
    let shard_root = profile_shard_root(home.path());
    let runtime = HostAdmissionTestRuntimeV1::profile(profile_root(home.path()))
        .await
        .unwrap();
    register_profile_sharded_store(&runtime, project.path(), "proj_cli").await;
    runtime.checkpoint_profile_database_for_test().await;
    drop(runtime);

    let _daemon = crate::common::spawn_tracedecay_daemon(home.path());
    let mut warm = tracedecay_command_without_daemon(home.path(), project.path());
    warm.args(["status", "--json"]);
    let warm_output = run_with_timeout(warm, cli_timeout());
    assert!(
        warm_output.status.success(),
        "fixture project should mount before branch metadata expands"
    );

    let tracked_branches = (0..300)
        .map(|index| {
            (
                format!("feature/branch-{index:03}-with-enough-detail-to-exercise-status-bounds"),
                format!("branches/feature_branch_{index:03}.db"),
            )
        })
        .collect::<Vec<_>>();
    for (name, _) in &tracked_branches {
        git(project.path(), &["branch", name]);
    }
    let tracked_branch_refs = tracked_branches
        .iter()
        .map(|(name, path)| (name.as_str(), path.as_str()))
        .collect::<Vec<_>>();
    write_branch_meta(&shard_root, &tracked_branch_refs, false);
    for (_, path) in &tracked_branches {
        let db_path = shard_root.join(path);
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        std::fs::write(db_path, b"branch fixture").unwrap();
    }

    let mut command = tracedecay_command_without_daemon(home.path(), project.path());
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
        stderr.contains("feature/branch-299-with-enough-detail-to-exercise-status-bounds"),
        "branch list should receive the complete explicitly requested branch diagnostics\nstderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("No branch tracking configured"),
        "branch list should not fall back to repo-local metadata\nstderr:\n{stderr}"
    );
}

#[tokio::test]
async fn gitignore_reads_effective_config_for_primary_and_linked_worktrees() {
    let home = TempDir::new().unwrap();
    let dir = TempDir::new().unwrap();
    let main = canonical_temp_path(&dir.path().join("main"));
    let linked = canonical_temp_path(&dir.path().join("linked"));
    std::fs::create_dir_all(&main).unwrap();
    git(&main, &["init", "-b", "main"]);
    std::fs::write(main.join("README.md"), "gitignore fixture\n").unwrap();
    commit_all(&main, "initial commit");
    git(
        &main,
        &[
            "worktree",
            "add",
            "-b",
            "feature/gitignore",
            linked.to_str().unwrap(),
            "HEAD",
        ],
    );
    write_profile_sharded_fixture(home.path(), &main);

    let runtime = HostAdmissionTestRuntimeV1::profile(profile_root(home.path()))
        .await
        .unwrap();
    runtime
        .upsert_code_project(
            "proj_cli",
            &main,
            Some(&main.join(".git")),
            None,
            Some("main"),
        )
        .await
        .expect("code project should upsert with git common-dir alias");
    runtime
        .upsert_store_instance(StoreInstanceUpsert {
            store_id: "store:proj_cli:profile_sharded".to_string(),
            project_id: "proj_cli".to_string(),
            store_kind: "code_project".to_string(),
            storage_mode: "profile_sharded".to_string(),
            store_relpath: "projects/proj_cli".to_string(),
            manifest_relpath: Some(STORE_MANIFEST_FILENAME.to_string()),
            last_verified_at: Some(1_800_000_000),
            last_write_at: Some(1_800_000_000),
        })
        .await
        .expect("store instance should upsert");
    runtime.checkpoint_profile_database_for_test().await;
    drop(runtime);

    let _daemon = crate::common::spawn_tracedecay_daemon(home.path());
    for project_root in [&main, &linked] {
        let mut command = tracedecay_command_without_daemon(home.path(), project_root);
        command.arg("gitignore");
        let output = run_with_timeout(command, cli_timeout());
        assert!(
            output.status.success(),
            "gitignore should resolve effective configuration for {}\nstdout:\n{}\nstderr:\n{}",
            project_root.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("gitignore: on"),
            "gitignore should report the daemon-authoritative default for {}",
            project_root.display()
        );
    }
}

#[tokio::test]
async fn automation_facts_list_reports_incompatible_proposal_bank_as_unavailable() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write_git_fixture(project.path());
    write_profile_sharded_fixture(home.path(), project.path());
    let runtime = HostAdmissionTestRuntimeV1::profile(profile_root(home.path()))
        .await
        .unwrap();
    register_profile_sharded_store(&runtime, project.path(), "proj_cli").await;
    runtime.checkpoint_profile_database_for_test().await;
    drop(runtime);

    let graph_db =
        rusqlite::Connection::open(profile_shard_root(home.path()).join("tracedecay.db"))
            .expect("fixture graph database");
    graph_db
        .execute_batch(
            "DROP TABLE IF EXISTS memory_v2_proposal_current;
             DROP TABLE IF EXISTS memory_v2_proposals;
             CREATE TABLE memory_v2_proposal_current (proposal_id TEXT PRIMARY KEY);
             CREATE TABLE memory_v2_proposals (proposal_id TEXT PRIMARY KEY);",
        )
        .expect("install incompatible compatibility proposal bank fixture");
    drop(graph_db);

    let _daemon = crate::common::spawn_tracedecay_daemon(home.path());
    let mut command = tracedecay_command_without_daemon(home.path(), project.path());
    command.args(["automation", "facts", "list"]);
    let output = run_with_timeout(command, cli_timeout());

    assert!(
        output.status.success(),
        "fact list should return typed unavailable evidence\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("fact list json");
    assert_eq!(payload["availability"]["state"], "unavailable");
    assert_eq!(
        payload["availability"]["reason"],
        "compatibility_proposal_authority_incompatible"
    );
    assert_eq!(payload["count"], 0);
    assert_eq!(payload["proposals"], serde_json::json!([]));
}

#[test]
fn branch_add_writes_new_branch_db_into_profile_shard() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_root = canonical_temp_path(project.path());
    git(&project_root, &["init", "-b", "main"]);
    std::fs::write(project_root.join("lib.rs"), "pub fn indexed() {}\n").unwrap();
    commit_all(&project_root, "initial commit");
    init_project_fixture(home.path(), &project_root);
    git(&project_root, &["checkout", "-b", "feature/new"]);
    let project_id = default_profile_project_id(&project_root);
    let shard_root = profile_sharded_data_root(&profile_root(home.path()), &project_id);
    let _daemon = crate::common::spawn_tracedecay_daemon(home.path());
    let mut command = tracedecay_command_without_daemon(home.path(), &project_root);
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
    write_git_fixture(project.path());
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
    write_git_fixture(project.path());
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
fn branch_gc_preserves_profile_shard_without_repository_evidence() {
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
        shard_root.join("branches/feature_stale.db").exists(),
        "branch gc must fail closed without repository branch evidence"
    );
}

#[test]
fn init_refuses_ephemeral_project_in_persistent_profile() {
    let home = TempDir::new().expect("home tempdir");
    let project = TempDir::new().expect("ephemeral project");
    std::fs::write(project.path().join("lib.rs"), "pub fn transient() {}\n")
        .expect("ephemeral source");
    let profile = tempfile::Builder::new()
        .prefix("persistent-profile-")
        .tempdir_in(ephemeral_safe_fixture_base())
        .expect("persistent profile");
    #[cfg(unix)]
    std::fs::set_permissions(profile.path(), std::fs::Permissions::from_mode(0o700))
        .expect("secure persistent profile permissions");

    let mut command = tracedecay_command_without_daemon(home.path(), project.path());
    let output = command
        .env("TRACEDECAY_DATA_DIR", profile.path())
        .env("TRACEDECAY_GLOBAL_DB", profile.path().join("global.db"))
        .args(["init", "."])
        .output()
        .expect("init ephemeral project");

    assert!(
        !output.status.success(),
        "an ephemeral project must not be enrolled in a persistent profile"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("temporary directory"),
        "stderr should explain the ephemeral-root guard\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !profile.path().join("projects").exists(),
        "rejected ephemeral project must not mint a profile store"
    );

    let projects = create_runtime().block_on(async {
        HostAdmissionTestRuntimeV1::profile(profile.path())
            .await
            .expect("open persistent profile registry")
            .list_code_projects(usize::MAX)
            .await
    });
    assert!(
        projects.is_empty(),
        "rejected ephemeral project must not enter the persistent registry"
    );
}

/// `migrate storage-report` is read-only and works against an explicit
/// `--profile-root` without any daemon or registered project, reporting a
/// real registered store's size and an unregistered directory's presence
/// (plan 38 §7 — size observability reachable from a command).
#[test]
fn migrate_storage_report_prints_registered_store_size_and_unregistered_backlog() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let profile_root = profile_root(home.path());
    std::fs::create_dir_all(&profile_root).unwrap();

    // One registered store with a real graph database file. It needs actual
    // content: `Connection::open` alone leaves a zero-length file on disk,
    // which is not a store any profile would ever hold.
    let registered_root = profile_root.join("projects/proj_cli");
    std::fs::create_dir_all(&registered_root).unwrap();
    let store_db = rusqlite::Connection::open(registered_root.join("tracedecay.db")).unwrap();
    store_db
        .execute_batch("CREATE TABLE fixture (id INTEGER PRIMARY KEY);")
        .unwrap();
    drop(store_db);
    let global_db = rusqlite::Connection::open(profile_root.join("global.db")).unwrap();
    global_db
        .execute_batch(
            "CREATE TABLE code_projects (project_id TEXT PRIMARY KEY, canonical_root TEXT NOT NULL);",
        )
        .unwrap();
    global_db
        .execute(
            "INSERT INTO code_projects (project_id, canonical_root) VALUES ('proj_cli', ?1)",
            rusqlite::params![project.path().display().to_string()],
        )
        .unwrap();
    drop(global_db);

    // An unregistered leaf directory under `projects/`.
    let unregistered = profile_root.join("projects/proj_ghost");
    std::fs::create_dir_all(&unregistered).unwrap();
    std::fs::write(unregistered.join("payload.bin"), vec![0u8; 4096]).unwrap();

    let mut command = tracedecay_command_without_daemon(home.path(), project.path());
    command.args([
        "migrate",
        "storage-report",
        "--profile-root",
        profile_root.to_str().unwrap(),
        "--json",
    ]);
    let output = run_with_timeout(command, cli_timeout());

    assert!(
        output.status.success(),
        "storage-report should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("report json");
    assert_eq!(report["stores"].as_array().unwrap().len(), 1);
    assert_eq!(report["stores"][0]["project_id"], "proj_cli");
    assert!(report["stores"][0]["total_bytes"].as_u64().unwrap() > 0);
    assert_eq!(report["unregistered_dir_count"], 1);
    assert!(report["unregistered_bytes"].as_u64().unwrap() >= 4096);
}

#[tokio::test]
async fn migrate_storage_report_uses_active_daemon_authority_without_hanging() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write_git_fixture(project.path());
    write_profile_sharded_fixture(home.path(), project.path());
    let profile_root = profile_root(home.path());
    let runtime = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .unwrap();
    register_profile_sharded_store(&runtime, project.path(), "proj_cli").await;
    for index in 0..4 {
        let project_id = format!("proj_storage_page_{index}");
        let project_root = home.path().join(format!("storage-project-{index}"));
        std::fs::create_dir_all(&project_root).unwrap();
        runtime
            .upsert_code_project(&project_id, &project_root, None, None, Some("main"))
            .await
            .expect("paged storage project should register");
        let data_root = profile_root.join("projects").join(&project_id);
        std::fs::create_dir_all(&data_root).unwrap();
        let connection = rusqlite::Connection::open(data_root.join("tracedecay.db")).unwrap();
        connection
            .execute_batch("CREATE TABLE fixture (id INTEGER PRIMARY KEY);")
            .unwrap();
    }
    runtime.checkpoint_profile_database_for_test().await;
    drop(runtime);

    let _daemon = crate::common::spawn_tracedecay_daemon(home.path());
    let mut command = tracedecay_command_without_daemon(home.path(), project.path());
    command.args(["migrate", "storage-report", "--json"]);
    let started = Instant::now();
    let output = run_with_timeout(command, Duration::from_secs(15));

    assert!(
        started.elapsed() < Duration::from_secs(15),
        "active-daemon storage report must complete within its bounded timeout"
    );
    assert!(
        output.status.success(),
        "active-daemon storage report should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("report json");
    assert_eq!(report["stores"].as_array().unwrap().len(), 5);
    assert_eq!(report["coverage"]["state"], "complete");
    assert_eq!(report["coverage"]["next_cursor"], serde_json::Value::Null);
}
