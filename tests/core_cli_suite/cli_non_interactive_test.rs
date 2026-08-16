use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

use crate::common::{MessageRecordBuilder, create_runtime, global_session};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;
use tracedecay::branch_meta::BranchMeta;
use tracedecay::global_db::StoreInstanceUpsert;
use tracedecay::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay::storage::{
    EnrollmentMarker, STORE_MANIFEST_FILENAME, STORE_MANIFEST_SCHEMA_VERSION, StorageMode,
    StoreKind, StoreManifest, default_profile_project_id, profile_sharded_data_root,
    profile_sharded_layout, write_enrollment_marker, write_repository_identity_marker,
    write_store_manifest,
};
use tracedecay_agent_hosts::PRODUCT_VERSION;
use tracedecay_agent_hosts::automation::run_ledger::{
    AutomationRunArtifactKind, AutomationRunLedgerRecord, append_run_record, write_run_artifact,
};
use tracedecay_domain::ProjectId;
use tracedecay_usecases::host_admission::HostAdmissionScope;

fn canonical_temp_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// A directory guaranteed to sit outside `std::env::temp_dir()`, for fixtures
/// that must NOT be classified as "ephemeral" by
/// `global_db::registry_maintenance`'s `classify_project_root` (which rejects project roots
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

/// Install a non-interactive `codex` that emulates `plugin add` / `remove`
/// against the isolated HOME. Real Codex CLI 0.147 does this; CI must not
/// depend on that binary being present.
fn add_codex_plugin_cli_shim(command: &mut Command, home: &Path) {
    let bin_dir = home.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let shim = bin_dir.join(if cfg!(windows) { "codex.exe" } else { "codex" });
    let version = PRODUCT_VERSION;
    let script = format!(
        r#"#!/bin/sh
set -eu
config="$HOME/.codex/config.toml"
source="$HOME/.codex/plugins/tracedecay"
cache="$HOME/.codex/plugins/cache/personal/tracedecay/{version}"
case "${{1:-}} ${{2:-}}" in
  "plugin add")
    mkdir -p "$(dirname "$config")" "$cache"
    if [ -d "$source" ]; then
      cp -a "$source/." "$cache/"
    fi
    printf '%s\n' '[plugins."tracedecay@personal"]' 'enabled = true' > "$config"
    printf '%s\n' '{{"pluginId":"tracedecay@personal","enabled":true}}'
    exit 0
    ;;
  "plugin remove")
    if [ -f "$config" ]; then
      printf '%s\n' > "$config"
    fi
    rm -rf "$HOME/.codex/plugins/cache/personal/tracedecay"
    exit 0
    ;;
esac
echo "unexpected codex invocation: $*" >&2
exit 2
"#,
        version = version
    );
    std::fs::write(&shim, script).unwrap();
    #[cfg(unix)]
    {
        let mut permissions = std::fs::metadata(&shim).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&shim, permissions).unwrap();
    }
    let path = command
        .get_envs()
        .find(|(key, _)| *key == "PATH")
        .and_then(|(_, value)| value.map(|value| value.to_os_string()))
        .unwrap_or_else(|| std::env::var_os("PATH").unwrap_or_default());
    let joined =
        std::env::join_paths(std::iter::once(bin_dir).chain(std::env::split_paths(&path))).unwrap();
    command.env("PATH", joined);
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
    // The daemon's full project open reads an attached git HEAD for the
    // feedback scope; without a committed repository the open degrades and
    // the registered project session authority is never exposed to the CLI.
    write_git_fixture(&project_root);
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
                    "recovery evidence",
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
        command.args(["sessions", "search", "recovery", "--limit", "3"]);
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
    // The sessions store must be fresh (zero tables) so the daemon's own
    // registered-schema admission installs the production shape on first
    // mount; a non-empty wrong-shape file trips the workflow persisted-shape
    // gate and is refused as reset-required.
    write_empty_sqlite_fixture(&shard_root.join("sessions.db"));
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

/// Writable first open publishes the daemon-owned canonical configuration
/// revision. Branch commands then open that store read-only and must not
/// invent a revision or migrate in place.
fn seed_canonical_configuration(home: &Path, project: &Path) {
    crate::common::ensure_tracedecay_daemon(home);
    crate::common::initialize_tracedecay_cli_project(home, project);
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

/// Drives the Codex activation journey non-interactively: install stages the
/// plugin source and marketplace entry, then drives `codex plugin add` through
/// a host-CLI shim that emulates Codex 0.147's non-interactive registry.
fn run_codex_automation_install(home: &TempDir, project_root: &Path) -> Output {
    let home_path = canonical_temp_path(home.path());

    let mut install = tracedecay_command(home.path(), project_root);
    let _shim = add_tracedecay_path_shim(&mut install, home.path());
    add_codex_plugin_cli_shim(&mut install, home.path());
    install.args(["install", "--agent", "codex", "--automation"]);
    let output = run_with_timeout(install, cli_timeout());
    assert!(
        output.status.success(),
        "codex automation install should complete through Codex's own plugin CLI\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let staged_source = home_path.join(".codex/plugins/tracedecay");
    assert!(
        staged_source.join(".codex-plugin/plugin.json").is_file(),
        "install must stage the Codex plugin source package"
    );
    assert!(
        home_path.join(".agents/plugins/marketplace.json").is_file(),
        "install must stage the personal marketplace entry"
    );
    assert!(
        home_path
            .join(".codex/plugins/cache/personal/tracedecay")
            .join(PRODUCT_VERSION)
            .join(".codex-plugin/plugin.json")
            .is_file(),
        "install must drive Codex to materialise the versioned plugin cache"
    );
    output
}

fn read_codex_daemon_automation_config(home: &TempDir, project_root: &Path) -> serde_json::Value {
    let mut get = tracedecay_command(home.path(), project_root);
    get.args(["automation", "config", "get", "--json"]);
    let output = run_with_timeout(get, cli_timeout());
    assert_eq!(
        output.status.code(),
        Some(0),
        "codex automation config read should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout)
        .expect("codex automation config read should return canonical JSON")
}

#[test]
fn install_codex_automation_enables_daemon_owned_project_configuration_noninteractively() {
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

    let output = run_codex_automation_install(&home, &project_root);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("daemon-managed project configuration"),
        "automation install must report the configuration authority it used\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        home.path()
            .join(".codex/plugins/tracedecay/.codex-plugin/plugin.json")
            .is_file(),
        "install --agent codex should still install the Codex plugin bundle"
    );
    // Native lifecycle boundary (289eaa7747): install must not mutate
    // Codex-owned automation state, including the legacy v0.0.10-v0.0.20
    // native scheduled automation the daemon scheduler replaced.
    assert_eq!(
        std::fs::read_to_string(legacy_automation_dir.join("automation.toml")).unwrap(),
        "status = \"ACTIVE\"\n",
        "Codex automation install must leave Codex-native automation state untouched"
    );
    assert!(
        !project_root.join(".codex/automations").exists(),
        "Codex automation install must not create repo-local Codex automation files"
    );
    let config = read_codex_daemon_automation_config(&home, &project_root);
    assert_eq!(config["source"], "daemon_pinned_snapshot");
    assert_eq!(config["effective"]["enabled"], true);
    assert_eq!(config["effective"]["backend"], "codex_app_server");
    assert_eq!(config["effective"]["host_mode"], "standalone");
    assert_eq!(config["effective"]["model_id"], "gpt-5.6-mini");
    assert_eq!(
        config["effective"]["tasks"]["memory_curator"]["enabled"],
        true
    );
    assert_eq!(
        config["effective"]["tasks"]["memory_curator"]["schedule"],
        "interval"
    );
    assert_eq!(
        config["effective"]["tasks"]["memory_curator"]["interval_secs"],
        900
    );
    assert_eq!(
        config["effective"]["tasks"]["session_reflector"]["enabled"],
        true
    );
    assert_eq!(
        config["effective"]["tasks"]["session_reflector"]["interval_secs"],
        900
    );
    assert_eq!(
        config["effective"]["tasks"]["skill_writer"]["enabled"],
        true
    );
    assert_eq!(
        config["effective"]["tasks"]["skill_writer"]["interval_secs"],
        3600
    );
    assert_eq!(
        config["effective"]["tasks"]["skill_writer"]["min_idle_secs"],
        900
    );

    let user_config: toml::Value = toml::from_str(
        &std::fs::read_to_string(profile_root(home.path()).join("config.toml"))
            .expect("install should save host lifecycle settings"),
    )
    .expect("host lifecycle settings should remain valid TOML");
    assert!(
        user_config.get("automation").is_none(),
        "automation install must not persist retired user automation defaults: {user_config:?}"
    );

    let projects_dir = profile_root(home.path()).join("projects");
    let sidecars = std::fs::read_dir(&projects_dir)
        .map(|entries| {
            entries
                .map(|entry| {
                    entry
                        .unwrap()
                        .path()
                        .join("dashboard/automation_config.json")
                })
                .filter(|path| path.is_file())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert!(
        sidecars.is_empty(),
        "automation install must not write retired dashboard sidecars: {sidecars:?}"
    );
}

#[test]
fn automation_config_enable_writes_canonical_project_setting_noninteractively() {
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
    assert_eq!(payload["effective"]["enabled"], true);
    assert_eq!(payload["effective"]["backend"], "codex_app_server");
    assert_eq!(payload["effective"]["model_id"], "gpt-5.6-mini");
    assert_eq!(payload["source"], "daemon_pinned_snapshot");
    assert_eq!(payload["explanation"]["automatic_memory_apply"], true);
    assert_eq!(payload["explanation"]["automatic_skill_activation"], true);

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
    assert_eq!(explain_payload["source"], "daemon_pinned_snapshot");
    assert_eq!(
        explain_payload["explanation"]["trace_decay_backend_calls"],
        true
    );
    assert_eq!(explain_payload["explanation"]["delegated_host"], false);
    assert_eq!(
        explain_payload["backend_availability"]["backend"],
        "codex_app_server"
    );
}

#[test]
fn automation_config_rejects_retired_global_scope() {
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
        !output.status.success(),
        "automation config global set should be rejected\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("automation settings are project-scoped")
    );
}

#[test]
fn automation_config_set_rejects_unimplemented_external_backend() {
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
fn automation_config_set_writes_complete_canonical_project_setting_noninteractively() {
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
    assert_eq!(payload["effective"]["backend"], "codex_app_server");
    assert_eq!(payload["effective"]["model_id"], "gpt-5.6-mini");
    assert_eq!(payload["explanation"]["automatic_memory_apply"], true);
    assert_eq!(payload["explanation"]["automatic_skill_activation"], true);
    assert_eq!(
        payload["effective"]["tasks"]["session_reflector"]["interval_secs"],
        1800
    );
    assert_eq!(
        payload["effective"]["tasks"]["skill_writer"]["stale_lock_secs"],
        7200
    );
    assert_eq!(
        payload["effective"]["tasks"]["memory_curator"]["cooldown_secs"],
        300
    );
    assert_eq!(
        payload["effective"]["tasks"]["session_reflector"]["min_idle_secs"],
        60
    );

    let mut get = tracedecay_command(home.path(), project.path());
    get.args(["automation", "config", "get", "--json"]);
    let get_output = run_with_timeout(get, cli_timeout());
    assert!(
        get_output.status.success(),
        "automation config get should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&get_output.stdout),
        String::from_utf8_lossy(&get_output.stderr)
    );
    let restored: serde_json::Value =
        serde_json::from_slice(&get_output.stdout).expect("project get should print JSON");
    assert_eq!(
        restored["effective"]["tasks"]["skill_writer"]["interval_secs"],
        3600
    );
}

#[test]
fn fact_store_curate_records_backend_disabled_skip_and_preserves_read_only_inspection() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    std::fs::create_dir_all(project.path().join("src")).unwrap();
    std::fs::write(project.path().join("src/lib.rs"), "pub fn marker() {}\n").unwrap();

    init_project_fixture(home.path(), project.path());

    let mut run = tracedecay_command(home.path(), project.path());
    run.args(["tool", "fact_store_curate", "--json", "--args", "{}"]);
    let run_output = run_with_timeout(run, cli_timeout());
    assert!(
        run_output.status.success(),
        "manual automation run should skip cleanly when its backend is disabled\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run_output.stdout),
        String::from_utf8_lossy(&run_output.stderr)
    );
    let payload: serde_json::Value =
        serde_json::from_slice(&run_output.stdout).expect("fact_store_curate should print JSON");
    let run = &payload["outcome"]["value"]["payload"];
    assert_eq!(run["task"], "memory_curator");
    assert_eq!(run["terminal"]["status"], "skipped");
    assert_eq!(run["terminal"]["reason"], "backend_disabled");

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
    assert_eq!(record["run_id"], run["run_id"]);
    assert_eq!(record["status"], "skipped");
    assert_eq!(record["error"], "backend_disabled");
    assert_eq!(record["trigger"], "application");

    let run_id = run["run_id"]
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
    assert_eq!(view_payload["record"]["error"], "backend_disabled");

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

    for project_id in ["proj_status_selected", "proj_status_legacy"] {
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
        reopened
            .project_ledger_paths_for_test()
            .await
            .expect("read exact project ledger paths after wipe")
            .is_empty(),
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

    let report = tracedecay::global_db::registry_maintenance::inspect_profile_store_orphans(
        &profile_root(home.path()),
        tracedecay::tracedecay::current_timestamp(),
    );
    assert_eq!(report.plans.len(), 1, "{report:#?}");
    assert_eq!(
        report.plans[0].status,
        tracedecay::global_db::registry_maintenance::RegistryOrphanRelinkStatus::Eligible,
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
    write_repository_identity_marker(project.path(), "proj_cli").unwrap();
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
    write_repository_identity_marker(&main, "proj_cli").unwrap();

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
async fn automation_facts_list_reports_terminal_receipt_collection() {
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

    let _daemon = crate::common::spawn_tracedecay_daemon(home.path());
    let mut command = tracedecay_command_without_daemon(home.path(), project.path());
    command.args(["automation", "facts", "list"]);
    let output = run_with_timeout(command, cli_timeout());

    assert!(
        output.status.success(),
        "fact list should return terminal automatic receipt evidence\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("fact list json");
    assert_eq!(payload["availability"]["state"], "available");
    assert_eq!(payload["count"], 0);
    assert_eq!(payload["receipts"], serde_json::json!([]));
    assert!(payload["next_after_apply_id"].is_null());
}

#[test]
fn branch_add_tracks_the_branch_on_the_single_project_store() {
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

    assert!(
        output.status.success(),
        "branch add must complete the daemon's exact branch sealing journey\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let meta = tracedecay::branch_meta::load_branch_meta(&shard_root)
        .expect("branch add must publish tracking metadata in the profile shard");
    let entry = meta
        .branches
        .get("feature/new")
        .expect("branch add must track the branch");
    assert!(
        entry.served_by_project_store(),
        "tracked branch must be served by the single project store, found '{}'",
        entry.db_file
    );
    let source = entry
        .graph_source
        .as_ref()
        .expect("branch add must seal exact branch provenance before replying");
    let head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&project_root)
        .output()
        .expect("git rev-parse should run");
    assert!(head.status.success(), "git rev-parse HEAD must succeed");
    assert_eq!(source.project_id, project_id);
    assert!(!source.repository_id.is_empty());
    assert!(!source.worktree_id.is_empty());
    let sealed_worktree = PathBuf::from(&source.worktree_root);
    assert_eq!(
        sealed_worktree.canonicalize().unwrap(),
        sealed_worktree,
        "the sealed worktree path must be canonical provenance"
    );
    let sealed_head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&sealed_worktree)
        .output()
        .expect("git rev-parse should run in the sealed worktree");
    assert!(
        sealed_head.status.success(),
        "sealed worktree must resolve HEAD"
    );
    let sealed_reference = Command::new("git")
        .args(["symbolic-ref", "-q", "HEAD"])
        .current_dir(&sealed_worktree)
        .output()
        .expect("git symbolic-ref should run in the sealed worktree");
    assert!(
        sealed_reference.status.success(),
        "sealed worktree must keep an attached source ref"
    );
    assert_eq!(
        source.reference,
        String::from_utf8_lossy(&sealed_reference.stdout).trim(),
        "the daemon must record the exact ref actually indexed"
    );
    assert_eq!(
        source.source_oid,
        String::from_utf8_lossy(&head.stdout).trim(),
        "the daemon branch-add journey must seal the exact branch head"
    );
    assert_eq!(
        source.source_oid,
        String::from_utf8_lossy(&sealed_head.stdout).trim(),
        "the stored OID must belong to the recorded source worktree"
    );
    assert!(
        !shard_root.join("branches").exists(),
        "branch add must not create a per-branch database"
    );
}

#[tokio::test]
async fn branch_remove_deletes_branch_db_from_profile_shard() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write_git_fixture(project.path());
    write_profile_sharded_fixture(home.path(), project.path());
    write_repository_identity_marker(project.path(), "proj_cli").unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(profile_root(home.path()))
        .await
        .unwrap();
    register_profile_sharded_store(&runtime, project.path(), "proj_cli").await;
    runtime.checkpoint_profile_database_for_test().await;
    drop(runtime);
    seed_canonical_configuration(home.path(), project.path());
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

#[tokio::test]
async fn branch_remove_deletes_branch_local_memory_without_cutover_receipt() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write_git_fixture(project.path());
    write_profile_sharded_fixture(home.path(), project.path());
    write_repository_identity_marker(project.path(), "proj_cli").unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(profile_root(home.path()))
        .await
        .unwrap();
    register_profile_sharded_store(&runtime, project.path(), "proj_cli").await;
    runtime.checkpoint_profile_database_for_test().await;
    drop(runtime);
    seed_canonical_configuration(home.path(), project.path());
    let shard_root = profile_shard_root(home.path());
    write_branch_meta(
        &shard_root,
        &[("feature/legacy-memory", "branches/feature_legacy_memory.db")],
        true,
    );
    let branch_db = shard_root.join("branches/feature_legacy_memory.db");
    rusqlite::Connection::open(&branch_db)
        .unwrap()
        .execute_batch(
            "CREATE TABLE memory_facts (fact_id TEXT PRIMARY KEY);
             INSERT INTO memory_facts (fact_id) VALUES ('branch-local');",
        )
        .unwrap();

    let mut command = tracedecay_command(home.path(), project.path());
    command.args(["branch", "remove", "feature/legacy-memory"]);
    let output = run_with_timeout(command, cli_timeout());

    assert!(
        output.status.success(),
        "branch remove should not require a migration receipt\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !branch_db.exists(),
        "branch remove should delete obsolete branch-local memory with its branch database"
    );
}

#[tokio::test]
async fn branch_removeall_deletes_profile_shard_branch_dbs() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write_git_fixture(project.path());
    write_profile_sharded_fixture(home.path(), project.path());
    write_repository_identity_marker(project.path(), "proj_cli").unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(profile_root(home.path()))
        .await
        .unwrap();
    register_profile_sharded_store(&runtime, project.path(), "proj_cli").await;
    runtime.checkpoint_profile_database_for_test().await;
    drop(runtime);
    seed_canonical_configuration(home.path(), project.path());
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

#[tokio::test]
async fn branch_gc_preserves_profile_shard_without_repository_evidence() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write_profile_sharded_fixture(home.path(), project.path());
    // No git fixture and no repository identity marker: the premise under
    // test is that gc fails closed without repository branch evidence.
    let runtime = HostAdmissionTestRuntimeV1::profile(profile_root(home.path()))
        .await
        .unwrap();
    register_profile_sharded_store(&runtime, project.path(), "proj_cli").await;
    runtime.checkpoint_profile_database_for_test().await;
    drop(runtime);
    seed_canonical_configuration(home.path(), project.path());
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

/// `storage report` is read-only and works against an explicit
/// `--profile-root` without any daemon or registered project, reporting a
/// real registered store's size and an unregistered directory's presence
/// (plan 38 §7 — size observability reachable from a command).
#[test]
fn storage_report_prints_registered_store_size_and_unregistered_backlog() {
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
        "storage",
        "report",
        "--profile-root",
        profile_root.to_str().unwrap(),
        "--json",
    ]);
    let output = run_with_timeout(command, cli_timeout());

    assert!(
        output.status.success(),
        "storage report should succeed\nstdout:\n{}\nstderr:\n{}",
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
async fn storage_report_uses_active_daemon_authority_without_hanging() {
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
    command.args(["storage", "report", "--json"]);
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
