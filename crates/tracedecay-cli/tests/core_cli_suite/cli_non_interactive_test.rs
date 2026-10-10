use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::common::{
    MessageRecordBuilder, apply_isolated_profile_env,
    canonical_existing_path as canonical_temp_path, create_runtime, global_session, hermetic_path,
};
use crate::provision_host_cli_fixture;
use serde_json::json;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;
use tracedecay_agent_hosts::PRODUCT_VERSION;
use tracedecay_automation_runtime::automation::run_ledger::{
    AutomationRunArtifactKind, AutomationRunLedgerRecord, append_run_record, write_run_artifact,
};
use tracedecay_domain::ProjectId;
use tracedecay_global_db::StoreInstanceUpsert;
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_runtime_core::branch_meta::BranchMeta;
use tracedecay_runtime_core::storage::{
    STORE_MANIFEST_FILENAME, STORE_MANIFEST_SCHEMA_VERSION, StorageMode, StoreKind, StoreManifest,
    default_profile_project_id, profile_sharded_data_root, write_repository_identity_marker,
};
use tracedecay_runtime_core::test_executable::link_or_copy_executable;
use tracedecay_sessions::admission::HostAdmissionScope;

/// Runs each temp-policy fixture with a private OS temporary directory, so
/// durable fixture paths remain siblings even inside Bazel's writable scratch.
/// The child boundary keeps temp environment changes away from other tests.
fn ephemeral_safe_fixture_base(test_path: &str) -> Option<PathBuf> {
    const ROOT_ENV: &str = "TRACEDECAY_CLI_DURABLE_FIXTURE_ROOT";
    if let Some(root) = std::env::var_os(ROOT_ENV) {
        let root = PathBuf::from(root);
        assert!(!tracedecay_global_db::is_ephemeral_path(&root));
        return Some(root);
    }
    // macOS's per-user temporary directory leaves too little room for the
    // nested child home and its Unix daemon socket. The private child TMPDIR
    // still defines the ephemeral boundary, with durable fixtures beside it.
    let scratch = if cfg!(target_os = "macos") {
        TempDir::new_in("/tmp")
    } else {
        TempDir::new()
    }
    .expect("isolated temp-policy fixture");
    let temporary = scratch.path().join("temporary");
    let durable = scratch.path().join("durable");
    std::fs::create_dir(&temporary).unwrap();
    std::fs::create_dir(&durable).unwrap();
    crate::common::rerun_test_in_child(
        test_path,
        &[
            (ROOT_ENV, Some(durable.as_os_str())),
            ("TMPDIR", Some(temporary.as_os_str())),
            ("TMP", Some(temporary.as_os_str())),
            ("TEMP", Some(temporary.as_os_str())),
        ],
    );
    None
}

fn profile_root(home: &Path) -> PathBuf {
    canonical_temp_path(home).join(".tracedecay")
}

fn profile_shard_root(home: &Path) -> PathBuf {
    profile_root(home).join("projects/proj_cli")
}

fn assert_namespace_absent(path: &Path, context: &str) {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(metadata) => panic!(
            "{context}: namespace entry remains at {} ({:?})",
            path.display(),
            metadata.file_type()
        ),
        Err(error) => panic!(
            "{context}: could not inspect namespace entry {}: {error}",
            path.display()
        ),
    }
}

/// Guarantees a fixture project carries no repo-local `.tracedecay` marker
/// directory. Repository identity moved into the git common dir, so the
/// profile-sharded fixture no longer plants one, a fixture that must model
/// the "registry-backed, no repo marker" shape treats an already-absent
/// directory as exactly that shape rather than a setup failure.
pub(crate) fn remove_repo_local_marker_dir_if_present(project: &Path) {
    match std::fs::remove_dir_all(project.join(".tracedecay")) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!(
            "could not clear the repo-local marker directory in {}: {error}",
            project.display()
        ),
    }
}

fn tracedecay_command_without_daemon(home: &std::path::Path, project: &std::path::Path) -> Command {
    let home = canonical_temp_path(home);
    let mut command = Command::new(crate::tracedecay_exe());
    apply_isolated_profile_env(&mut command, &home, &profile_root(&home));
    command
        .current_dir(project)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn tracedecay_command(home: &std::path::Path, project: &std::path::Path) -> Command {
    crate::common::ensure_tracedecay_daemon(home);
    tracedecay_command_without_daemon(home, project)
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
    link_or_copy_executable(crate::tracedecay_exe(), &shim).unwrap();
    command.env("PATH", hermetic_path(&[bin_dir]));
    shim
}

/// Install a compiled `codex` that emulates `plugin add` / `remove` against
/// the isolated HOME. Real Codex CLI 0.147 does this; CI must not depend on
/// that binary being present, and Windows must not rename a shell script to
/// `.exe`.
fn add_codex_plugin_cli_shim(command: &mut Command, home: &Path) {
    let bin_dir = home.join("bin");
    provision_host_cli_fixture::install_compiled_host_cli_fixture(&bin_dir, "codex");
    command.env("PATH", hermetic_path(&[bin_dir]));
}

/// Kimi Code on the `PATH` [`add_tracedecay_path_shim`] set, which is what
/// makes Kimi Code installed; its lifecycle only resolves `kimi`.
fn add_kimi_cli_shim(home: &Path) {
    provision_host_cli_fixture::install_compiled_host_cli_fixture(&home.join("bin"), "kimi");
}

fn arm_implicit_cursor_reinstall(home: &Path) {
    let profile = profile_root(home);
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(
        profile.join("config.toml"),
        concat!(
            "installed_agents = [\"cursor\"]\n",
            "previous_version = \"0.0.0-beta.1\"\n",
            "last_installed_version = \"0.0.0-beta.1\"\n",
        ),
    )
    .unwrap();
}

fn assert_cursor_plugin_was_not_implicitly_installed(home: &Path) {
    assert!(
        !canonical_temp_path(home)
            .join(".cursor/plugins/local/tracedecay")
            .exists(),
        "ordinary CLI entrypoint repaired the Cursor host bundle before dispatch"
    );
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
            .expect("session message fixture write");
        // The daemon started below opens this database as a separate process.
        // Checkpoint and release the writer here so it sees the fixture rows
        // and can take the single-writer authority, the same discipline the
        // profile-scoped fixtures in this file already follow.
        runtime
            .checkpoint_session_database_for_test(HostAdmissionScope::Project)
            .await
            .expect("session fixture checkpoint");
        drop(runtime);
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
fn sessions_unused_context_reports_used_and_ignored_tool_units() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_root = canonical_temp_path(project.path());
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
                    &global_session("cursor", "unused-context-1", "proj_cli"),
                )
                .await
                .expect("session fixture write")
        );
        let search = json!({
            "results": [
                {"display": {"name": "load_session", "qualified_name": "lcm::load_session", "path": "crates/tracedecay-lcm/src/query/session.rs"}},
                {"display": {"name": "dispatch", "qualified_name": "cli::dispatch", "path": "crates/tracedecay-cli/src/main.rs"}}
            ]
        });
        let grep = json!({
            "results": [
                {"file": "crates/tracedecay-lcm/src/query/session.rs", "line": 9, "text": "pub async fn load_session(conn: &impl QueryExecutor)"},
                {"file": "crates/other/src/lib.rs", "line": 1, "text": "fn never_quoted_helper_name() {}"}
            ],
            "match_count": 2
        });
        runtime
            .upsert_session_message_for_test(
                HostAdmissionScope::Project,
                &MessageRecordBuilder::new(
                    "cursor",
                    "invoke-search",
                    "unused-context-1",
                    "assistant",
                    1,
                    "{}",
                    "tool_call",
                )
                .with_tool_names(Some("tracedecay_search"))
                .build(),
            )
            .await
            .expect("search invocation");
        runtime
            .upsert_session_message_for_test(
                HostAdmissionScope::Project,
                &MessageRecordBuilder::new(
                    "cursor",
                    "result-search",
                    "unused-context-1",
                    "tool",
                    2,
                    &search.to_string(),
                    "tool_result",
                )
                .build(),
            )
            .await
            .expect("search result");
        runtime
            .upsert_session_message_for_test(
                HostAdmissionScope::Project,
                &MessageRecordBuilder::new(
                    "cursor",
                    "invoke-grep",
                    "unused-context-1",
                    "assistant",
                    3,
                    "{}",
                    "tool_call",
                )
                .with_tool_names(Some("tracedecay_grep"))
                .build(),
            )
            .await
            .expect("grep invocation");
        runtime
            .upsert_session_message_for_test(
                HostAdmissionScope::Project,
                &MessageRecordBuilder::new(
                    "cursor",
                    "result-grep",
                    "unused-context-1",
                    "tool",
                    4,
                    &grep.to_string(),
                    "tool_result",
                )
                .build(),
            )
            .await
            .expect("grep result");
        runtime
            .upsert_session_message_for_test(
                HostAdmissionScope::Project,
                &MessageRecordBuilder::new(
                    "cursor",
                    "open-used-search-hit",
                    "unused-context-1",
                    "assistant",
                    5,
                    r#"{"path":"crates/tracedecay-lcm/src/query/session.rs"}"#,
                    "tool_call",
                )
                .with_tool_names(Some("read_file"))
                .build(),
            )
            .await
            .expect("later open");
        runtime
            .upsert_session_message_for_test(
                HostAdmissionScope::Project,
                &MessageRecordBuilder::new(
                    "cursor",
                    "quote-used-grep",
                    "unused-context-1",
                    "assistant",
                    6,
                    "I will reuse pub async fn load_session(conn: &impl QueryExecutor) as the walker.",
                    "message",
                )
                .build(),
            )
            .await
            .expect("later quote");
        runtime
            .checkpoint_session_database_for_test(HostAdmissionScope::Project)
            .await
            .expect("session fixture checkpoint");
        drop(runtime);
    });

    let mut command = tracedecay_command(home.path(), &project_root);
    command.args(["sessions", "unused-context", "--json", "--examples", "3"]);
    let output = run_with_timeout(command, cli_timeout());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "sessions unused-context should succeed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("json report");
    assert_eq!(report["meter"], "chars_div_4");
    assert!(report["sessions_scanned"].as_u64().unwrap() >= 1);
    assert!(report["tool_results_scanned"].as_u64().unwrap() >= 2);
    let tools = report["tools"].as_array().expect("tools");
    let search = tools
        .iter()
        .find(|row| row["tool"] == "search")
        .expect("search row");
    assert!(search["used_tokens"].as_u64().unwrap() > 0);
    assert!(search["unused_tokens"].as_u64().unwrap() > 0);
    let grep = tools
        .iter()
        .find(|row| row["tool"] == "grep")
        .expect("grep row");
    assert!(grep["used_tokens"].as_u64().unwrap() > 0);
}

#[test]
fn sessions_search_omits_absent_optional_filters_and_preserves_provider() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_root = canonical_temp_path(project.path());
    // Commit the fixture before `init`: the daemon's full project open reads
    // an attached git HEAD before it exposes the registered session authority.
    write_git_fixture(&project_root);
    init_project_fixture(home.path(), &project_root);
    let transcript = write_claude_search_transcript(
        home.path(),
        &project_root,
        "session-search-filters",
        "recovery evidence",
    );
    assert_transcript_contains(&transcript, "recovery evidence");

    let _daemon = crate::common::spawn_tracedecay_daemon(home.path());
    import_sessions_until_searchable(home.path(), &project_root, "recovery");
    for extra_args in [vec![], vec!["--provider", "claude"]] {
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

    let payload = sessions_search_json(home.path(), &project_root, "recovery");
    assert_eq!(payload["query"], "recovery", "{payload:#}");
    assert_eq!(payload["status"], "ok", "{payload:#}");
    assert!(
        search_results_contain(&payload, "recovery evidence"),
        "filter-omission search must prove a real hit: {payload:#}"
    );
}

fn host_transcript_files(home: &Path) -> Vec<PathBuf> {
    let roots = [
        home.join(".claude/projects"),
        home.join(".codex/sessions"),
        home.join(".codex/archived_sessions"),
        home.join(".cursor/projects"),
        home.join(".cursor/chats"),
    ];
    let mut files = Vec::new();
    for root in roots {
        if !root.exists() {
            continue;
        }
        let mut pending = vec![root];
        while let Some(dir) = pending.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    files.push(path);
                }
            }
        }
    }
    files.sort();
    files
}

const SEARCH_HIT_PHRASE: &str = "orchid spool tension stays indexed";
const SEARCH_MISS_QUERY: &str = "no such nautilus phrase";

/// Writes a Claude Code transcript whose recorded `cwd` is the registered
/// project. The slug can be dummy; ingest matches on `cwd`, not the folder.
fn write_claude_search_transcript(
    home: &Path,
    project_root: &Path,
    session: &str,
    phrase: &str,
) -> PathBuf {
    let dir = home.join(".claude/projects/-some-slug");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{session}.jsonl"));
    let cwd = project_root.to_string_lossy();
    let contents = format!(
        "{}\n{}\n",
        serde_json::json!({
            "type": "user",
            "cwd": cwd,
            "sessionId": session,
            "uuid": "u1",
            "timestamp": "2026-01-01T00:00:00.000Z",
            "message": {"role": "user", "content": phrase}
        }),
        serde_json::json!({
            "type": "assistant",
            "cwd": cwd,
            "sessionId": session,
            "uuid": "u2",
            "timestamp": "2026-01-01T00:00:05.000Z",
            "message": {
                "id": format!("msg_{session}"),
                "role": "assistant",
                "model": "claude-opus-4-8",
                "content": [{"type": "text", "text": format!("noted {phrase}")}]
            }
        }),
    );
    std::fs::write(&path, contents).unwrap();
    path
}

fn assert_transcript_contains(path: &Path, phrase: &str) {
    let contents = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("could not read transcript {}: {error}", path.display()));
    assert!(
        contents.contains(phrase),
        "transcript {} must contain {phrase:?}: {contents}",
        path.display()
    );
}

fn search_results_contain(payload: &serde_json::Value, phrase: &str) -> bool {
    payload["results"].as_array().is_some_and(|results| {
        results.iter().any(|hit| {
            hit["message"]["text"]
                .as_str()
                .is_some_and(|text| text.contains(phrase))
        })
    })
}

fn import_sessions(home: &Path, project_root: &Path) {
    let mut command = tracedecay_command_without_daemon(home, project_root);
    command.args(["sessions", "import"]);
    let output = run_with_timeout(command, cli_timeout());
    assert!(
        output.status.success(),
        "sessions import should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn import_sessions_until_searchable(home: &Path, project_root: &Path, query: &str) {
    import_sessions(home, project_root);
    let payload = sessions_search_json_until(home, project_root, query, |payload| {
        search_results_contain(payload, query)
    });
    assert_eq!(payload["status"], "ok", "{payload:#}");
}

fn sessions_search_json(home: &Path, project_root: &Path, query: &str) -> serde_json::Value {
    sessions_search_json_until(home, project_root, query, |payload| {
        !matches!(payload["status"].as_str(), Some("stale"))
    })
}

fn sessions_search_json_until(
    home: &Path,
    project_root: &Path,
    query: &str,
    ready: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    crate::common::poll_until(
        Instant::now() + cli_timeout(),
        Duration::from_millis(100),
        || {
            let mut command = tracedecay_command_without_daemon(home, project_root);
            command.args(["sessions", "search", query, "--limit", "3", "--json"]);
            let output = run_with_timeout(command, cli_timeout());
            assert!(
                output.status.success(),
                "sessions search --json should succeed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let payload: serde_json::Value = serde_json::from_slice(&output.stdout)
                .expect("sessions search --json prints one document");
            ready(&payload).then_some(payload)
        },
        || "session search projection did not finish historical convergence".to_owned(),
    )
}

fn sessions_search_text(home: &Path, project_root: &Path, query: &str) -> String {
    let mut command = tracedecay_command_without_daemon(home, project_root);
    command.args(["sessions", "search", query, "--limit", "3"]);
    let output = run_with_timeout(command, cli_timeout());
    assert!(
        output.status.success(),
        "sessions search should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn sessions_search_reports_unavailable_when_no_host_transcripts_exist() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_root = canonical_temp_path(project.path());
    write_git_fixture(&project_root);
    init_project_fixture(home.path(), &project_root);
    let transcripts = host_transcript_files(home.path());
    assert!(
        transcripts.is_empty(),
        "isolated home must have no host transcripts: {transcripts:?}"
    );

    let _daemon = crate::common::spawn_tracedecay_daemon(home.path());
    let payload = sessions_search_json(home.path(), &project_root, "indexLocalPlugins");
    assert_eq!(payload["query"], "indexLocalPlugins", "{payload:#}");
    assert_eq!(payload["status"], "unavailable", "{payload:#}");
    assert_eq!(payload["outcome"], "unavailable", "{payload:#}");
    assert_eq!(payload["count"], 0, "{payload:#}");
    assert_eq!(payload["results"], json!([]), "{payload:#}");
    let message = payload["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("tracedecay sessions import"),
        "empty source must name the import that fills it: {payload:#}"
    );
    assert_eq!(
        payload["next_action"]["tool"], "tracedecay sessions import",
        "{payload:#}"
    );

    let report = sessions_search_text(home.path(), &project_root, "indexLocalPlugins");
    assert!(
        !report.contains("no messages matched"),
        "a missing source is not a query miss: {report}"
    );
    assert!(report.contains("status: unavailable"), "{report}");
    assert!(report.contains("tracedecay sessions import"), "{report}");
}

#[test]
fn sessions_search_reports_complete_zero_when_store_has_no_match() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_root = canonical_temp_path(project.path());
    write_git_fixture(&project_root);
    init_project_fixture(home.path(), &project_root);
    let transcript = write_claude_search_transcript(
        home.path(),
        &project_root,
        "session-search-miss",
        SEARCH_HIT_PHRASE,
    );
    assert_transcript_contains(&transcript, SEARCH_HIT_PHRASE);
    let transcript_text = std::fs::read_to_string(&transcript).unwrap();
    assert!(
        !transcript_text.contains(SEARCH_MISS_QUERY),
        "miss query must be absent from the transcript: {transcript_text}"
    );

    let _daemon = crate::common::spawn_tracedecay_daemon(home.path());
    import_sessions_until_searchable(home.path(), &project_root, SEARCH_HIT_PHRASE);
    let payload = sessions_search_json(home.path(), &project_root, SEARCH_MISS_QUERY);
    assert_eq!(payload["query"], SEARCH_MISS_QUERY, "{payload:#}");
    assert_eq!(payload["status"], "complete_zero", "{payload:#}");
    assert_eq!(payload["outcome"], "complete_zero", "{payload:#}");
    assert_eq!(payload["count"], 0, "{payload:#}");
    assert_eq!(payload["results"], json!([]), "{payload:#}");

    let report = sessions_search_text(home.path(), &project_root, SEARCH_MISS_QUERY);
    assert!(
        report.contains(&format!(
            "no messages matched query \"{SEARCH_MISS_QUERY}\""
        )),
        "{report}"
    );
    assert!(report.contains("status: complete_zero"), "{report}");
}

#[test]
fn sessions_search_returns_a_hit_when_the_store_has_a_match() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_root = canonical_temp_path(project.path());
    write_git_fixture(&project_root);
    init_project_fixture(home.path(), &project_root);
    let transcript = write_claude_search_transcript(
        home.path(),
        &project_root,
        "session-search-hit",
        SEARCH_HIT_PHRASE,
    );
    assert_transcript_contains(&transcript, SEARCH_HIT_PHRASE);

    let _daemon = crate::common::spawn_tracedecay_daemon(home.path());
    import_sessions_until_searchable(home.path(), &project_root, SEARCH_HIT_PHRASE);
    let payload = sessions_search_json(home.path(), &project_root, SEARCH_HIT_PHRASE);
    assert_eq!(payload["query"], SEARCH_HIT_PHRASE, "{payload:#}");
    assert_eq!(payload["status"], "ok", "{payload:#}");
    assert!(
        search_results_contain(&payload, SEARCH_HIT_PHRASE),
        "imported transcript must be searchable: {payload:#}"
    );

    let report = sessions_search_text(home.path(), &project_root, SEARCH_HIT_PHRASE);
    assert!(report.contains(SEARCH_HIT_PHRASE), "{report}");
}

fn poll_git_sync(
    child: &mut std::process::Child,
    stdout: &mut Option<JoinHandle<Vec<u8>>>,
    stderr: &mut Option<JoinHandle<Vec<u8>>>,
) -> Option<Output> {
    child
        .try_wait()
        .expect("git-sync status should be readable")
        .map(|status| child_output(status, stdout.take(), stderr.take()))
}

fn git_sync_failure(message: &str, output: &Output) -> String {
    format!(
        "{message}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_mounted_git_sync(output: &Output, dry_run: bool) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "sessions git-sync should succeed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.starts_with("session git sync completed (session-sync."),
        "{stdout}"
    );
    assert_eq!(
        stdout
            .lines()
            .any(|line| line == "git-sync (dry-run): no rows were written"),
        dry_run,
        "{stdout}"
    );
}

fn wait_for_git_sync(
    child: &mut std::process::Child,
    stdout: &mut Option<JoinHandle<Vec<u8>>>,
    stderr: &mut Option<JoinHandle<Vec<u8>>>,
) -> Output {
    let finished = Instant::now() + cli_timeout();
    loop {
        if let Some(output) = poll_git_sync(child, stdout, stderr) {
            return output;
        }
        if Instant::now() >= finished {
            let _ = child.kill();
            let status = child.wait().expect("git-sync should exit after kill");
            panic!(
                "{}",
                git_sync_failure(
                    "sessions git-sync did not finish after the full server mounted",
                    &child_output(status, stdout.take(), stderr.take())
                )
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A git sync that arrives while the core owner is published, and the full
/// server has not mounted session sync, must keep running. Releasing the
/// hold lets the full server answer; a later sync then writes for real.
#[test]
fn sessions_git_sync_on_a_cold_daemon_waits_for_the_project_mount() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_root = canonical_temp_path(project.path());
    write_git_fixture(&project_root);
    init_project_fixture(home.path(), &project_root);

    let hold = canonical_temp_path(home.path()).join("hold-after-core-publish");
    std::fs::write(&hold, b"hold").unwrap();
    let entered = PathBuf::from(format!("{}.entered", hold.display()));
    let _daemon = crate::common::spawn_tracedecay_daemon_with(home.path(), {
        let hold = hold.clone();
        move |command| {
            command.env("TRACEDECAY_TEST_HOLD_AFTER_CORE_PUBLISH", &hold);
        }
    });

    let mut command = tracedecay_command_without_daemon(home.path(), &project_root);
    command.args(["sessions", "git-sync", "--dry-run"]);
    let mut child = command
        .spawn()
        .expect("tracedecay sessions git-sync should spawn");
    let mut stdout = child.stdout.take().map(drain_pipe);
    let mut stderr = child.stderr.take().map(drain_pipe);

    let core_visible = Instant::now() + Duration::from_secs(30);
    while !entered.is_file() {
        if let Some(output) = poll_git_sync(&mut child, &mut stdout, &mut stderr) {
            panic!(
                "{}",
                git_sync_failure(
                    "sessions git-sync finished before the core owner was held",
                    &output
                )
            );
        }
        assert!(
            Instant::now() < core_visible,
            "core publication was not held"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // A terminal unavailable answer returns immediately. Waiting for the
    // mount keeps the command running for this whole interval.
    let still_mounting = Instant::now() + Duration::from_secs(3);
    while Instant::now() < still_mounting {
        if let Some(output) = poll_git_sync(&mut child, &mut stdout, &mut stderr) {
            panic!(
                "{}",
                git_sync_failure(
                    "sessions git-sync stopped while the project was still mounting",
                    &output
                )
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    std::fs::remove_file(&hold).unwrap();
    assert_mounted_git_sync(
        &wait_for_git_sync(&mut child, &mut stdout, &mut stderr),
        true,
    );

    let mut command = tracedecay_command_without_daemon(home.path(), &project_root);
    command.args(["sessions", "git-sync"]);
    assert_mounted_git_sync(&run_with_timeout(command, cli_timeout()), false);
}

fn dashboard_session_authority(capabilities_url: &str) -> String {
    let capabilities: serde_json::Value = ureq::get(capabilities_url)
        .call()
        .unwrap_or_else(|error| panic!("GET {capabilities_url} failed: {error}"))
        .into_body()
        .read_json()
        .unwrap_or_else(|error| panic!("GET {capabilities_url} returned no JSON: {error}"));
    capabilities["session_authority"]
        .as_str()
        .unwrap_or_else(|| panic!("capabilities carry no session_authority: {capabilities}"))
        .to_owned()
}

/// A dashboard started while the project's open is held after core
/// publication reports its session authority as opening, and mounts the
/// session store when the full server's publication lands.
#[test]
fn dashboard_started_while_the_project_opens_mounts_sessions_on_publication() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_root = canonical_temp_path(project.path());
    write_git_fixture(&project_root);
    init_project_fixture(home.path(), &project_root);

    let hold = canonical_temp_path(home.path()).join("hold-after-core-publish");
    std::fs::write(&hold, b"hold").unwrap();
    let entered = PathBuf::from(format!("{}.entered", hold.display()));
    let _daemon = crate::common::spawn_tracedecay_daemon_with(home.path(), {
        let hold = hold.clone();
        move |command| {
            command.env("TRACEDECAY_TEST_HOLD_AFTER_CORE_PUBLISH", &hold);
        }
    });
    // The dashboard request opens the project; the core server answers it
    // while the open is held before the full server publishes.
    let mut command = tracedecay_command_without_daemon(home.path(), &project_root);
    command.args(["dashboard", "--host", "127.0.0.1", "--port", "0"]);
    let dashboard = run_with_timeout(command, cli_timeout());
    assert!(entered.is_file(), "core publication was not held");
    let stdout = String::from_utf8_lossy(&dashboard.stdout);
    let launch_url = stdout
        .lines()
        .find_map(|line| line.strip_prefix("tracedecay dashboard listening on "))
        .unwrap_or_else(|| {
            panic!(
                "dashboard announced no URL:\n{stdout}\n{}",
                String::from_utf8_lossy(&dashboard.stderr)
            )
        });
    let (origin, token) = launch_url
        .trim()
        .split_once("/?token=")
        .unwrap_or_else(|| panic!("dashboard launch URL carries no token: {launch_url}"));
    let authority = origin
        .strip_prefix("http://")
        .unwrap_or_else(|| panic!("dashboard launch URL is not loopback HTTP: {launch_url}"));
    let capabilities_url = format!("http://tracedecay:{token}@{authority}/api/capabilities");
    for _ in 0..3 {
        assert_eq!(dashboard_session_authority(&capabilities_url), "opening");
    }

    std::fs::remove_file(&hold).unwrap();
    let published = Instant::now() + cli_timeout();
    loop {
        let state = dashboard_session_authority(&capabilities_url);
        if state == "ready" {
            break;
        }
        assert_eq!(state, "opening", "the open must publish its session store");
        assert!(
            Instant::now() < published,
            "the session store never mounted after the open was released"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `tracedecay dashboard` from the user home must name `--path` instead of
/// stopping at the ambient-root diagnosis. The README launch example is this
/// command, and operators cannot guess the flag from the inner config error.
#[test]
fn dashboard_from_home_tells_the_operator_to_pass_path() {
    let home = TempDir::new().unwrap();
    let mut command = tracedecay_command_without_daemon(home.path(), home.path());
    command.args(["dashboard", "--host", "127.0.0.1", "--port", "0"]);
    let output = run_with_timeout(command, cli_timeout());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "dashboard from $HOME must refuse\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("ambient user/filesystem root"),
        "dashboard from $HOME must keep the ambient-root diagnosis\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("--path"),
        "dashboard from $HOME must tell the operator to pass --path\nstderr:\n{stderr}"
    );
    assert!(
        !stdout.contains("tracedecay dashboard listening on"),
        "an ambient-root refusal must not start a listener\nstdout:\n{stdout}"
    );
}

fn refresh_json(output: &Output, step: &str) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "sessions refresh {step} should succeed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    serde_json::from_str(&stdout).unwrap_or_else(|error| {
        panic!("refresh {step} must print the typed result: {error}\n{stdout}")
    })
}

/// A profile-scoped refresh travels CLI → daemon on the projectless route and
/// settles through the profile session authority: begin issues an opaque
/// handle bound to the profile store, status reads it back, cancel returns the
/// durable receipt, and the receipt stays terminal, all with the canonical
/// `scope.kind=profile` request and no project anywhere.
#[cfg(unix)]
#[test]
fn sessions_refresh_profile_scope_begins_reads_and_cancels_through_the_daemon() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let _daemon = crate::common::spawn_tracedecay_daemon(home.path());
    let selectors = [
        "--profile",
        "--session-id",
        "session.cli.profile-refresh",
        "--provider",
        "codex",
        "--source",
        "0",
        "--target",
        "0",
        "--json",
    ];

    let mut begin = tracedecay_command_without_daemon(home.path(), cwd.path());
    begin.args(["sessions", "refresh", "begin"]).args(selectors);
    let begun = refresh_json(&run_with_timeout(begin, cli_timeout()), "begin");
    assert!(
        matches!(begun["outcome"].as_str(), Some("started" | "joined")),
        "{begun}"
    );
    assert_eq!(begun["scope"], "profile", "{begun}");
    assert_eq!(begun["tool"], "tracedecay_session_refresh_begin", "{begun}");
    let handle = begun["handle"]
        .as_str()
        .unwrap_or_else(|| panic!("begin must return an opaque handle: {begun}"))
        .to_owned();
    assert!(handle.starts_with("srh_"), "{handle}");
    let operation_id = begun["operation_id"]
        .as_str()
        .unwrap_or_else(|| panic!("begin must return the durable operation id: {begun}"))
        .to_owned();
    assert_ne!(handle, operation_id);

    let mut status = tracedecay_command_without_daemon(home.path(), cwd.path());
    status
        .args(["sessions", "refresh", "status"])
        .args(selectors)
        .args(["--handle", &handle]);
    let observed = refresh_json(&run_with_timeout(status, cli_timeout()), "status");
    assert!(
        matches!(observed["outcome"].as_str(), Some("running" | "complete")),
        "{observed}"
    );
    assert_eq!(observed["scope"], "profile", "{observed}");
    assert_eq!(
        observed["tool"], "tracedecay_session_refresh_status",
        "{observed}"
    );

    let mut cancel = tracedecay_command_without_daemon(home.path(), cwd.path());
    cancel
        .args(["sessions", "refresh", "cancel"])
        .args(selectors)
        .args(["--handle", &handle]);
    let cancelled = refresh_json(&run_with_timeout(cancel, cli_timeout()), "cancel");
    assert!(
        matches!(
            cancelled["outcome"].as_str(),
            Some("cancelled" | "complete")
        ),
        "{cancelled}"
    );
    assert_eq!(cancelled["scope"], "profile", "{cancelled}");
    assert_eq!(
        cancelled["receipt"]["operation_id"], operation_id,
        "{cancelled}"
    );
    let terminal_state = cancelled["receipt"]["state"]
        .as_str()
        .unwrap_or_else(|| panic!("cancel must return the terminal receipt: {cancelled}"))
        .to_owned();
    assert!(
        matches!(terminal_state.as_str(), "cancelled" | "complete"),
        "{cancelled}"
    );

    let mut settled = tracedecay_command_without_daemon(home.path(), cwd.path());
    settled
        .args(["sessions", "refresh", "status"])
        .args(selectors)
        .args(["--handle", &handle]);
    let settled = refresh_json(&run_with_timeout(settled, cli_timeout()), "settled status");
    assert_eq!(
        settled["receipt"]["operation_id"], operation_id,
        "{settled}"
    );
    assert_eq!(settled["receipt"]["state"], terminal_state, "{settled}");
}

fn write_profile_sharded_fixture(home: &std::path::Path, project: &std::path::Path) {
    let project = canonical_temp_path(project);
    let shard_root = profile_shard_root(home);
    std::fs::create_dir_all(&shard_root).unwrap();
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(&project, "proj_cli")
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
    write_branch_meta(&shard_root, &[]);
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
        sessions_schema_digest: None,
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

fn write_branch_meta(shard_root: &std::path::Path, tracked_branches: &[&str]) {
    let mut meta = BranchMeta::new("main");
    for name in tracked_branches {
        meta.add_branch(name, "main");
    }
    std::fs::write(
        shard_root.join("branch-meta.json"),
        serde_json::to_string_pretty(&meta).unwrap(),
    )
    .unwrap();
}

/// Drains one child pipe on its own thread so the child can never block on a
/// full pipe: `branch list` alone writes hundreds of stderr lines, and a child
/// stalled in `eprintln!` never exits, so polling `try_wait` without readers
/// turned machine-wide pipe pressure into a 90 s "hang" with the daemon's
/// answer already written.
fn drain_pipe<R: std::io::Read + Send + 'static>(mut pipe: R) -> JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        pipe.read_to_end(&mut buf)
            .unwrap_or_else(|e| panic!("failed to drain child pipe: {e}"));
        buf
    })
}

fn child_output(
    status: ExitStatus,
    stdout: Option<JoinHandle<Vec<u8>>>,
    stderr: Option<JoinHandle<Vec<u8>>>,
) -> Output {
    let join = |handle: Option<JoinHandle<Vec<u8>>>| {
        handle
            .map(|handle| handle.join().expect("child pipe drain thread panicked"))
            .unwrap_or_default()
    };
    Output {
        status,
        stdout: join(stdout),
        stderr: join(stderr),
    }
}

fn run_with_timeout(mut command: Command, timeout: Duration) -> Output {
    let mut child = command
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn tracedecay: {e}"));
    let stdout = child.stdout.take().map(drain_pipe);
    let stderr = child.stderr.take().map(drain_pipe);
    let started = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .unwrap_or_else(|e| panic!("failed to poll child: {e}"))
        {
            return child_output(status, stdout, stderr);
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let status = child
                .wait()
                .unwrap_or_else(|e| panic!("failed to wait for timed out child: {e}"));
            let output = child_output(status, stdout, stderr);
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
    // The `.gitignore` offer only exists for a repository, and code indexing
    // is git-backed: without a committed repository `init` can only report
    // that indexing is unavailable, so neither half of this test's subject
    // would be exercised.
    git(project.path(), &["init", "-b", "main"]);
    commit_all(project.path(), "fixture repository");

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
    // `init` is brokered through the daemon now: it admits the project and
    // then asks the daemon-owned code-index scheduler to reconcile. The
    // default confirmation is a typed not-ready receipt, not a finished index.
    assert!(
        stderr.contains("first generation not ready")
            && stderr.contains("code_index_reconciliation_requested"),
        "stderr should confirm enrollment without looking finished\nstderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("initialized "),
        "default init must not look like a finished index\nstderr:\n{stderr}"
    );
}

#[test]
fn init_wait_holds_until_first_generation_is_ready() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    std::fs::create_dir_all(project.path().join("src")).unwrap();
    std::fs::write(project.path().join("src/lib.rs"), "pub fn marker() {}\n").unwrap();
    git(project.path(), &["init", "-b", "main"]);
    commit_all(project.path(), "fixture repository");

    let mut command = tracedecay_command(home.path(), project.path());
    command.args(["init", "--wait"]);
    let output = run_with_timeout(command, cli_timeout());
    assert!(
        output.status.success(),
        "init --wait should succeed once the first generation is ready\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("first generation ready"),
        "init --wait must report a ready receipt\nstderr:\n{stderr}"
    );

    let mut status = tracedecay_command(home.path(), project.path());
    status.args(["status", "--json"]);
    let status_output = run_with_timeout(status, cli_timeout());
    assert!(
        status_output.status.success(),
        "status after init --wait should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&status_output.stdout),
        String::from_utf8_lossy(&status_output.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&status_output.stdout)
        .expect("status --json should print one document");
    assert_eq!(
        payload
            .pointer("/graph_statistics/state")
            .and_then(serde_json::Value::as_str),
        Some("observed"),
        "init --wait must leave a usable graph\n{payload}"
    );
    assert!(
        payload
            .pointer("/graph_statistics/symbol_count")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|count| count > 0),
        "init --wait must serve the fixture's indexed symbol\n{payload}"
    );
}

#[test]
fn explicit_kimi_install_fails_with_interactive_remediation() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let kimi_home = canonical_temp_path(home.path()).join(".kimi-code");
    let mut install = tracedecay_command_without_daemon(home.path(), project.path());
    let _shim = add_tracedecay_path_shim(&mut install, home.path());
    add_kimi_cli_shim(home.path());
    install
        .env(
            tracedecay_agent_hosts::agents::kimi::KIMI_CODE_HOME_ENV,
            &kimi_home,
        )
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

/// Kimi Code activates only through its interactive `/plugins` step, so a
/// detected install reports that host, exits with the operator-action status,
/// and still installs every other detected host.
#[test]
fn detected_install_continues_past_a_host_waiting_on_the_operator() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let home_path = canonical_temp_path(home.path());
    let kimi_home = home_path.join(".kimi-code");
    std::fs::create_dir_all(&kimi_home).unwrap();
    std::fs::create_dir_all(home_path.join(".vibe")).unwrap();
    let mut install = tracedecay_command_without_daemon(home.path(), project.path());
    let _shim = add_tracedecay_path_shim(&mut install, home.path());
    add_kimi_cli_shim(home.path());
    install
        .env(
            tracedecay_agent_hosts::agents::kimi::KIMI_CODE_HOME_ENV,
            &kimi_home,
        )
        .arg("install");

    let output = run_with_timeout(install, cli_timeout());

    let stderr = String::from_utf8_lossy(&output.stderr);
    // 75 is the lifecycle status for "nothing failed, but a host still needs
    // an interactive operator step" (`EX_TEMPFAIL`).
    assert_eq!(
        output.status.code(),
        Some(75),
        "a host waiting on an operator step exits with the operator-action status\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("kimi") && stderr.contains("pending operator action"),
        "the pass must name the host that still needs an operator step\nstderr:\n{stderr}"
    );
    assert!(
        home_path.join(".vibe/config.toml").is_file(),
        "Vibe is detected after Kimi and must still be installed\nstderr:\n{stderr}"
    );
}

#[test]
fn install_without_any_detected_agent_succeeds_with_a_notice() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let mut install = tracedecay_command_without_daemon(home.path(), project.path());
    let _shim = add_tracedecay_path_shim(&mut install, home.path());
    install.arg("install");

    let output = run_with_timeout(install, cli_timeout());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "the first command a user runs, before any agent exists, is not a failure\nstderr:\n{stderr}"
    );
    assert!(stderr.contains("No supported agents detected"), "{stderr}");
    assert!(stderr.contains("Claude Code"), "{stderr}");
    assert!(stderr.contains("Cursor"), "{stderr}");
    assert!(
        stderr.contains("run `tracedecay install` again"),
        "{stderr}"
    );
}

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
fn automation_config_get_does_not_repair_host_bundles_before_dispatch() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_root = canonical_temp_path(project.path());
    std::fs::create_dir_all(project_root.join("src")).unwrap();
    std::fs::write(project_root.join("src/lib.rs"), "pub fn marker() {}\n").unwrap();
    arm_implicit_cursor_reinstall(home.path());

    let mut get = tracedecay_command_without_daemon(home.path(), &project_root);
    get.args(["automation", "config", "get", "--json"]);
    let output = run_with_timeout(get, cli_timeout());

    assert!(
        !output.status.success(),
        "automation config get should report the deliberately absent daemon\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("daemon"),
        "config read should fail at its dispatcher, not during startup maintenance\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_cursor_plugin_was_not_implicitly_installed(home.path());
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
    assert_eq!(config["effective"]["model_id"], "gpt-5.6-sol");
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
    assert_eq!(payload["effective"]["model_id"], "gpt-5.6-sol");
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
    assert_eq!(payload["effective"]["model_id"], "gpt-5.6-sol");
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

    // Pause through the same durable authority as the dashboard before the
    // first mount can admit scheduled work. Manual curation still runs through
    // its ordinary lock and backend gates.
    let dashboard_root = profile_sharded_data_root(
        &profile_root(home.path()),
        &default_profile_project_id(&canonical_temp_path(project.path())),
    )
    .join("dashboard");
    create_runtime()
        .block_on(
            tracedecay_automation_runtime::automation::scheduler::save_scheduler_control(
                &dashboard_root,
                &tracedecay_automation_runtime::automation::scheduler::AutomationSchedulerControl {
                    paused: true,
                },
            ),
        )
        .expect("pause scheduled work before project activation");
    init_project_fixture(home.path(), project.path());

    // A backend-disabled skip is the subject here, and the shipped automation
    // default is now `codex_app_server` with the curation loop scheduled
    // (`AutomationSettingsV1::default`). Arrange the condition instead of
    // inheriting it, or the manual run reports whatever the default backend
    // reached (`nothing_to_review`) and proves nothing about the skip.
    let mut disable = tracedecay_command(home.path(), project.path());
    disable.args(["automation", "config", "set", "--backend", "disabled"]);
    let disable_output = run_with_timeout(disable, cli_timeout());
    assert!(
        disable_output.status.success(),
        "disabling the automation backend should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&disable_output.stdout),
        String::from_utf8_lossy(&disable_output.stderr)
    );

    // `tracedecay tool` answers with the MCP tool-result envelope; the
    // retained document travels in its content block under `format: "json"`.
    let tool_document = |arguments: &str, tool: &str| {
        let mut command = tracedecay_command(home.path(), project.path());
        command.args(["tool", tool, "--json", "--args", arguments]);
        let output = run_with_timeout(command, cli_timeout());
        let envelope: serde_json::Value =
            serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
                panic!(
                    "{tool} should print JSON ({error})\nstdout:\n{}\nstderr:\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                )
            });
        let text = envelope["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("{tool} returned no content text: {envelope}"))
            .to_owned();
        (
            output.status.success() && envelope["isError"] != serde_json::json!(true),
            serde_json::from_str::<serde_json::Value>(&text)
                .unwrap_or(serde_json::Value::String(text)),
        )
    };

    // The project's own automation scheduler runs beside this manual call and
    // takes the same curator lock. While it holds it the run settles with the
    // legal transient `scheduler_lock_active` skip instead of the
    // backend-disabled skip under test, so retry until the lock is free.
    let (run, settled) = {
        let mut attempts = 0;
        loop {
            let (admitted, payload) = tool_document(r#"{"format":"json"}"#, "fact_store_curate");
            assert!(
                admitted,
                "manual automation run should be admitted: {payload}"
            );
            let run = payload["outcome"]["value"]["payload"].clone();
            assert_eq!(run["state"], "started", "{payload}");
            let run_id = run["run_id"]
                .as_str()
                .expect("curate receipt run_id")
                .to_owned();
            let settled_by = std::time::Instant::now() + std::time::Duration::from_secs(60);
            let settled = loop {
                let (found, view) = tool_document(
                    &serde_json::json!({"run_id": run_id, "format": "json"}).to_string(),
                    "automation_run_view",
                );
                if found {
                    break view["run"].clone();
                }
                assert!(
                    std::time::Instant::now() < settled_by,
                    "run {run_id} never settled: {view}"
                );
                std::thread::sleep(std::time::Duration::from_millis(50));
            };
            if settled["error"] != "scheduler_lock_active" {
                break (run, settled);
            }
            attempts += 1;
            assert!(
                attempts < 10,
                "the automation scheduler held the curator lock for every manual attempt: {settled}"
            );
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    };
    assert_eq!(run["task"], "memory_curator");
    assert_eq!(settled["status"], "skipped");
    assert_eq!(settled["error"], "backend_disabled");

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
    let run_id = run["run_id"]
        .as_str()
        .expect("automation run payload should include a run_id");
    // Inspect the exact run identity returned by the manual call.
    let ledger = std::fs::read_to_string(&ledger_paths[0]).unwrap();
    let record = ledger
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .expect("every ledger line should be a JSON record")
        })
        .find(|record| record["run_id"] == run_id)
        .unwrap_or_else(|| panic!("ledger should record run {run_id}:\n{ledger}"));
    assert_eq!(record["status"], "skipped");
    assert_eq!(record["error"], "backend_disabled");
    assert_eq!(record["trigger"], "application");

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
    let listed = list_payload["records"]
        .as_array()
        .expect("runs list should return records")
        .iter()
        .find(|entry| entry["run_id"] == run_id)
        .unwrap_or_else(|| panic!("runs list should surface {run_id}: {list_payload}"));
    assert_eq!(listed["status"], "skipped");

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
        serde_json::from_value(record).expect("ledger should deserialize as run record");
    // The ledger is append-only with an enforced lifecycle: a run that already
    // reached a terminal status cannot be re-appended, so attaching an artifact
    // to the curate run's own `skipped` row is refused with "invalid lifecycle
    // transition". Cover the artifact surface on a run of its own instead.
    // one terminal row that already carries the artifact, which is the only
    // shape the ledger accepts.
    let artifact_run_id = format!("{run_id}-artifact");
    artifact_record.run_id.clone_from(&artifact_run_id);
    let artifact_payload = serde_json::json!({
        "loop_stage": "codex_handoff",
        "run_id": artifact_run_id,
        "status": "ready_for_review",
    });
    let runtime = create_runtime();
    let artifact = runtime
        .block_on(write_run_artifact(
            &dashboard_root,
            &artifact_run_id,
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
        artifact_run_id.as_str(),
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
    assert_eq!(artifact_view_payload["run_id"], artifact_run_id);
    assert_eq!(artifact_view_payload["artifact"]["kind"], "codex_handoff");
    assert_eq!(
        artifact_view_payload["payload"]["status"],
        "ready_for_review"
    );

    let mut original_view = tracedecay_command(home.path(), project.path());
    original_view.args(["automation", "runs", "view", run_id, "--json"]);
    let original_output = run_with_timeout(original_view, cli_timeout());
    assert!(
        original_output.status.success(),
        "original skipped run remains readable: {}",
        String::from_utf8_lossy(&original_output.stderr)
    );
    let original_payload: serde_json::Value =
        serde_json::from_slice(&original_output.stdout).expect("original run JSON");
    assert_eq!(
        original_payload["record"], view_payload["record"],
        "artifact publication and inspection must preserve the skipped terminal"
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
        stderr.contains("Skipping index creation") && stderr.contains("tracedecay init"),
        "stderr should explain how to initialize without waiting on stdin\nstderr:\n{stderr}"
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
async fn list_uses_registry_and_omits_default_disk_size() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let other = TempDir::new().unwrap();
    write_git_fixture(project.path());
    write_profile_sharded_fixture(home.path(), project.path());
    let bulky = profile_shard_root(home.path()).join("bulky");
    std::fs::create_dir_all(&bulky).unwrap();
    std::fs::write(bulky.join("blob"), vec![0u8; 4096]).unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(profile_root(home.path()))
        .await
        .unwrap();
    register_profile_sharded_store(&runtime, project.path(), "proj_cli").await;
    register_profile_sharded_store(&runtime, other.path(), "proj_other").await;
    runtime.checkpoint_profile_database_for_test().await;
    drop(runtime);

    let mut command = tracedecay_command(home.path(), project.path());
    command.arg("list");
    let output = run_with_timeout(command, cli_timeout());

    assert!(
        output.status.success(),
        "list should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let project_path = canonical_temp_path(project.path());
    let other_path = canonical_temp_path(other.path());
    assert!(
        stdout.contains(&project_path.display().to_string()),
        "list should show the registered local project\nstdout:\n{stdout}"
    );
    assert!(
        !stdout.contains(&other_path.display().to_string()),
        "list should not show an unrelated registered project\nstdout:\n{stdout}"
    );
    assert!(
        stdout.contains("—"),
        "default list must not walk the store for a byte size\nstdout:\n{stdout}"
    );
    assert!(
        !stdout.contains("4.0 KB") && !stdout.contains("4.1 kB") && !stdout.contains("4096"),
        "default list must not print the recursive store size\nstdout:\n{stdout}"
    );
    assert!(
        stdout.contains("Total: — on disk"),
        "default list total must stay unmeasured\nstdout:\n{stdout}"
    );
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
async fn projects_list_from_initialized_cwd_stays_projectless_and_marks_active() {
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

    let mut command = tracedecay_command(home.path(), project.path());
    command.args(["projects", "list", "--json"]);
    let output = run_with_timeout(command, cli_timeout());

    assert!(
        output.status.success(),
        "projects list should use the projectless registry route\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["projects"][0]["project_id"], "proj_cli");
    assert_eq!(payload["projects"][0]["is_active"], true);
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

#[test]
fn wipe_all_is_schema_independent_and_removes_every_profile_database_root() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let profile = profile_root(home.path());
    let config_path = profile.join("config.toml");
    let identity_path = profile.join("profile-identity.json");
    std::fs::create_dir_all(&profile).unwrap();
    let config =
        toml::to_string_pretty(&tracedecay_session_memory::user_config::UserConfig::default())
            .unwrap()
            .into_bytes();
    std::fs::write(&config_path, &config).unwrap();
    let identity = br#"{
  "schema_version": 1,
  "brain_id": "brain.wipe-test",
  "profile_id": "profile.wipe-test"
}"#;
    std::fs::write(&identity_path, identity).unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&identity_path, std::fs::Permissions::from_mode(0o600)).unwrap();

    let database_paths = [
        profile.join("global.db"),
        profile.join("user-sessions.db"),
        profile.join("user-memory.db"),
        profile.join("projects/orphan-project/tracedecay.db"),
        profile.join("projects/orphan-project/sessions.db"),
        profile.join("stores/legacy-orphan/tracedecay.db"),
        profile.join(format!("remote/nodes/{}/remote.db", "a".repeat(64))),
    ];
    for database in &database_paths {
        std::fs::create_dir_all(database.parent().unwrap()).unwrap();
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let mut member = database.as_os_str().to_os_string();
            member.push(suffix);
            std::fs::write(PathBuf::from(member), b"not a compatible SQLite schema").unwrap();
        }
    }
    let grafeo_paths = [
        profile.join("user-sessions.grafeo"),
        profile.join("user-memory.grafeo"),
    ];
    for path in &grafeo_paths {
        std::fs::write(path, b"incompatible graph store").unwrap();
        let mut wal = path.as_os_str().to_os_string();
        wal.push(".wal");
        let wal = PathBuf::from(wal);
        std::fs::create_dir(&wal).unwrap();
        std::fs::write(wal.join("segment"), b"graph wal").unwrap();
    }
    let host_admission = profile.join(".user-sessions.db.host-admission");
    std::fs::create_dir(&host_admission).unwrap();
    std::fs::write(host_admission.join("pending"), b"admission spool").unwrap();
    // What the beta.65 operator profile kept after `wipe --all` (#2875).
    let surviving_state = [
        "hook-v2-profile-admissions/claude/admissions.v1.bin",
        "hook-v2-profile-admissions/claude/admission-work-completions.v1.json",
        "hook-v2-profile-admissions/claude/admissions.v1.lock",
        "hook-v2-profile-admissions/codex/admissions.v2.log",
        "lcm-payloads/payload",
        "response-handles/handle",
        "maintenance/unregistered-project-directory-inventory-v2/page",
        "maintenance/retention-cold-store-cursor-v1.json",
        "hook_analytics.jsonl",
        "hook_analytics.jsonl.lock",
    ];
    for relative in surviving_state {
        let path = profile.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"profile state").unwrap();
    }

    let mut command = tracedecay_command_without_daemon(home.path(), project.path());
    command.args(["wipe", "--all", "--yes"]);
    let output = run_with_timeout(command, cli_timeout());

    assert!(
        output.status.success(),
        "wipe --all must not open the databases it destroys\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    for named_state in [
        "user memory and sessions",
        "project, legacy, and remote stores",
        "Grafeo WAL and host-admission state",
    ] {
        assert!(
            stderr.contains(named_state),
            "wipe --all warning omitted {named_state:?}\nstderr:\n{stderr}"
        );
    }
    for database in &database_paths {
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let mut member = database.as_os_str().to_os_string();
            member.push(suffix);
            assert_namespace_absent(
                &PathBuf::from(member),
                "wipe --all left a database family member",
            );
        }
    }
    for removed_root in ["projects", "stores", "remote"] {
        assert_namespace_absent(
            &profile.join(removed_root),
            "wipe --all left a fixed database root",
        );
    }
    for path in grafeo_paths {
        let mut wal = path.as_os_str().to_os_string();
        wal.push(".wal");
        assert_namespace_absent(&path, "wipe --all left a Grafeo store");
        assert_namespace_absent(&PathBuf::from(wal), "wipe --all left a Grafeo WAL");
    }
    assert_namespace_absent(
        &host_admission,
        "wipe --all left the profile host-admission database companion",
    );
    let mut survivors = surviving_state
        .iter()
        .filter_map(|relative| relative.split('/').next())
        .filter(|root| profile.join(root).exists())
        .collect::<Vec<_>>();
    survivors.dedup();
    assert_eq!(
        survivors,
        Vec::<&str>::new(),
        "wipe --all left profile state"
    );
    assert_eq!(std::fs::read(&config_path).unwrap(), config);
    assert_eq!(std::fs::read(&identity_path).unwrap(), identity);
}

#[test]
fn wipe_all_rejects_the_user_home_as_its_profile_root() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let home = canonical_temp_path(home.path());
    let sentinel = home.join("projects/operator-owned/sentinel");
    std::fs::create_dir_all(sentinel.parent().unwrap()).unwrap();
    std::fs::write(&sentinel, b"preserve").unwrap();

    let mut command = tracedecay_command_without_daemon(&home, project.path());
    command.env("TRACEDECAY_DATA_DIR", &home);
    command.args(["wipe", "--all", "--yes"]);
    let output = run_with_timeout(command, cli_timeout());

    assert!(
        !output.status.success(),
        "wipe --all must reject the user home as a database profile root"
    );
    assert_eq!(
        std::fs::read(&sentinel).unwrap(),
        b"preserve",
        "dangerous-root admission must run before any deletion"
    );
}

#[cfg(unix)]
#[test]
fn wipe_local_returns_failure_when_a_selected_store_cannot_be_deleted() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write_profile_sharded_fixture(home.path(), project.path());
    let data_root = profile_shard_root(home.path());
    let blocked = data_root.join("blocked");
    std::fs::create_dir_all(&blocked).unwrap();
    std::fs::write(blocked.join("retry-authority"), b"preserve").unwrap();
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o000)).unwrap();

    let mut command = tracedecay_command_without_daemon(home.path(), project.path());
    command.args(["wipe", "--yes"]);
    let output = run_with_timeout(command, cli_timeout());

    if blocked.exists() {
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "a selected-store deletion failure must be a nonzero CLI result\nstderr:\n{stderr}"
    );
    assert!(
        data_root.exists(),
        "failed local wipe must retain a discoverable target for retry"
    );
    assert!(
        !stderr.contains("Wiped 0 project(s)"),
        "a failed local wipe must not print a green success summary\nstderr:\n{stderr}"
    );
}

#[tokio::test]
async fn wipe_all_does_not_repair_host_bundles_before_removing_profile_store() {
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
    arm_implicit_cursor_reinstall(home.path());

    let mut command = tracedecay_command_without_daemon(home.path(), project.path());
    command.args(["wipe", "--all", "--yes"]);
    let output = run_with_timeout(command, cli_timeout());

    assert!(
        output.status.success(),
        "wipe --all should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!shard_root.join("tracedecay.db").exists());
    assert!(!shard_root.join(STORE_MANIFEST_FILENAME).exists());
    assert_cursor_plugin_was_not_implicitly_installed(home.path());
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
    let Some(durable) = ephemeral_safe_fixture_base(
        "cli_non_interactive_test::list_all_reports_orphan_manifest_reconstructable_store",
    ) else {
        return;
    };
    let home = TempDir::new().unwrap();
    let project = tempfile::Builder::new()
        .prefix("list-orphan-project-")
        .tempdir_in(durable)
        .unwrap();
    git(project.path(), &["init"]);
    write_profile_sharded_fixture(home.path(), project.path());
    write_repository_identity_marker(project.path(), "proj_cli").unwrap();
    std::fs::create_dir_all(profile_root(home.path())).unwrap();

    let report = tracedecay_global_db::registry_maintenance::inspect_profile_store_orphans(
        &profile_root(home.path()),
        tracedecay_runtime_core::tracedecay::current_timestamp(),
    );
    assert_eq!(report.plans.len(), 1, "{report:#?}");
    assert_eq!(
        report.plans[0].status,
        tracedecay_global_db::registry_maintenance::RegistryOrphanRelinkStatus::Eligible,
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
    // registered-looking one, the ambient registry fallback this fixture
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
    remove_repo_local_marker_dir_if_present(project.path());
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
    remove_repo_local_marker_dir_if_present(project.path());
    let shard_root = profile_shard_root(home.path());
    let runtime = HostAdmissionTestRuntimeV1::profile(profile_root(home.path()))
        .await
        .unwrap();
    register_profile_sharded_store(&runtime, project.path(), "proj_cli").await;
    runtime.checkpoint_profile_database_for_test().await;
    drop(runtime);

    let mut command = tracedecay_command_without_daemon(home.path(), project.path());
    command.args(["wipe", "--all", "--yes"]);
    let output = run_with_timeout(command, cli_timeout());

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

/// Durable debris in the shape of issue #765's wedge, a sealed code
/// generation that can never seat plus its graph container and WAL. Wipe and
/// forget must treat these as plain bytes: nothing in the escape hatches may
/// open, replay, or await the graph runtime that would wedge on them.
fn write_wedged_generation_debris(shard_root: &Path) {
    let generations = shard_root.join("code-generations-v1");
    std::fs::create_dir_all(generations.join("tracedecay.sealed")).unwrap();
    std::fs::write(
        generations
            .join("tracedecay.sealed")
            .join("278bea7a-sealed"),
        b"sealed generation that conflicts on every seat attempt",
    )
    .unwrap();
    std::fs::write(shard_root.join("tracedecay.grafeo"), b"graph container").unwrap();
    let wal = shard_root.join("tracedecay.grafeo.wal");
    std::fs::create_dir_all(&wal).unwrap();
    std::fs::write(wal.join("segment"), b"graph wal segment").unwrap();
}

/// The #765 operator journey: the managed daemon holds its lifetime shared
/// lease and is wedged in a terminal activation retry loop, so it never
/// exits. Without an installed service to stop, the holder never releases.
/// wipe must refuse typed within its bound instead of advising an operator
/// to wait forever ("retry after it finishes").
#[test]
fn wipe_refuses_within_bound_when_profile_lease_never_releases() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write_profile_sharded_fixture(home.path(), project.path());
    let shard_root = profile_shard_root(home.path());
    write_wedged_generation_debris(&shard_root);
    let profile = profile_root(home.path());
    let hung_holder = tracedecay_runtime_core::lifecycle_lease::acquire_shared_for_profile(
        &profile,
        "daemon run",
    )
    .unwrap();

    let mut command = tracedecay_command_without_daemon(home.path(), project.path());
    command.args(["wipe", "--yes"]);
    let started = Instant::now();
    let output = run_with_timeout(command, cli_timeout());
    let elapsed = started.elapsed();

    drop(hung_holder);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "an unreleasable lease must be a typed refusal\nstderr:\n{stderr}"
    );
    assert!(
        elapsed < Duration::from_secs(60),
        "wipe must refuse within its bound, took {elapsed:?}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("stopping the managed TraceDecay daemon service"),
        "wipe must announce the daemon quiesce as typed progress\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("could not take the profile offline within"),
        "the refusal must name the bound instead of advising an endless retry\nstderr:\n{stderr}"
    );
    assert!(
        shard_root.exists(),
        "a refused wipe must not delete anything"
    );
}

/// Once the wedged holder is stopped (in production the supervisor's bounded
/// service stop, SIGKILL at worst), the same wipe completes inside the lease
/// bound and removes the wedge-shaped store without ever opening it.
#[test]
fn wipe_completes_within_bound_once_the_wedged_holder_stops() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write_profile_sharded_fixture(home.path(), project.path());
    let shard_root = profile_shard_root(home.path());
    write_wedged_generation_debris(&shard_root);
    let profile = profile_root(home.path());
    let holder = tracedecay_runtime_core::lifecycle_lease::acquire_shared_for_profile(
        &profile,
        "daemon run",
    )
    .unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(2));
        drop(holder);
    });

    let mut command = tracedecay_command_without_daemon(home.path(), project.path());
    command.args(["wipe", "--yes"]);
    let started = Instant::now();
    let output = run_with_timeout(command, cli_timeout());
    let elapsed = started.elapsed();
    release.join().unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "wipe must complete once the holder releases within the bound\nstderr:\n{stderr}"
    );
    assert!(
        elapsed < Duration::from_secs(60),
        "wipe must complete within its bound, took {elapsed:?}"
    );
    assert!(
        stderr.contains("Wiped 1 project(s)"),
        "wipe must report the removed project\nstderr:\n{stderr}"
    );
    assert_namespace_absent(&shard_root, "wipe left the wedge-shaped store");
}

/// `projects forget` is scoped-destructive, so it refuses without the global
/// `--yes` confirmation and names both the preview and the keep-store escape.
#[test]
fn projects_forget_requires_the_yes_confirmation() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();

    let mut command = tracedecay_command_without_daemon(home.path(), project.path());
    command.args(["projects", "forget", "proj_anything"]);
    let output = run_with_timeout(command, cli_timeout());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "forget without --yes must refuse\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("re-run with --yes"),
        "the refusal must name the confirmation flag\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("--dry-run") && stderr.contains("--keep-store"),
        "the refusal must name the preview and keep-store escapes\nstderr:\n{stderr}"
    );
}

/// End-to-end #730 journey: two registered projects, forget one by id with no
/// daemon running, and exactly that project's rows and store bytes are gone.
#[tokio::test]
async fn projects_forget_cli_removes_only_the_selected_project() {
    let home = TempDir::new().unwrap();
    let project_a = TempDir::new().unwrap();
    let project_b = TempDir::new().unwrap();
    let profile = profile_root(home.path());
    let runtime = HostAdmissionTestRuntimeV1::profile(&profile).await.unwrap();
    register_profile_sharded_store(&runtime, project_a.path(), "proj_forget_a").await;
    register_profile_sharded_store(&runtime, project_b.path(), "proj_forget_b").await;
    runtime.checkpoint_profile_database_for_test().await;
    drop(runtime);
    for project_id in ["proj_forget_a", "proj_forget_b"] {
        let store = profile.join("projects").join(project_id);
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("tracedecay.db"), b"store bytes").unwrap();
    }
    write_wedged_generation_debris(&profile.join("projects").join("proj_forget_a"));

    let mut command = tracedecay_command_without_daemon(home.path(), project_a.path());
    command.args(["projects", "forget", "proj_forget_a", "--yes"]);
    let output = run_with_timeout(command, cli_timeout());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "projects forget should succeed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("Forgot project proj_forget_a"),
        "forget must report the retired identity\nstdout:\n{stdout}"
    );
    assert_namespace_absent(
        &profile.join("projects").join("proj_forget_a"),
        "forget left the selected project's store",
    );
    assert!(
        profile
            .join("projects")
            .join("proj_forget_b")
            .join("tracedecay.db")
            .exists(),
        "forget must not touch the sibling project's store"
    );
    let runtime = HostAdmissionTestRuntimeV1::profile(&profile).await.unwrap();
    assert!(
        runtime
            .get_code_project("proj_forget_a")
            .await
            .unwrap()
            .is_none(),
        "the forgotten registry identity must be retired"
    );
    assert!(
        runtime
            .get_code_project("proj_forget_b")
            .await
            .unwrap()
            .is_some(),
        "the sibling registry identity must survive"
    );
    drop(runtime);
}

/// The preview is read-only and daemon-brokered like the other `projects`
/// reads: it prints the exact removal plan and mutates nothing.
#[tokio::test]
async fn projects_forget_dry_run_previews_without_mutation() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write_profile_sharded_fixture(home.path(), project.path());
    let profile = profile_root(home.path());
    let runtime = HostAdmissionTestRuntimeV1::profile(&profile).await.unwrap();
    register_profile_sharded_store(&runtime, project.path(), "proj_cli").await;
    runtime.checkpoint_profile_database_for_test().await;
    drop(runtime);
    let shard_root = profile_shard_root(home.path());

    let mut command = tracedecay_command(home.path(), project.path());
    command.args(["projects", "forget", "proj_cli", "--dry-run"]);
    let output = run_with_timeout(command, cli_timeout());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "forget --dry-run should succeed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("Would forget project proj_cli"),
        "the preview must name the resolved identity\nstdout:\n{stdout}"
    );
    assert!(
        stdout.contains("would delete store"),
        "the preview must name the store directories\nstdout:\n{stdout}"
    );
    assert!(
        shard_root.join("tracedecay.db").exists(),
        "a dry run must not delete store bytes"
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
            format!("feature/branch-{index:03}-with-enough-detail-to-exercise-status-bounds")
        })
        .collect::<Vec<_>>();
    for name in &tracked_branches {
        git(project.path(), &["branch", name]);
    }
    let tracked_branch_refs = tracked_branches
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    write_branch_meta(&shard_root, &tracked_branch_refs);

    let mut command = tracedecay_command_without_daemon(home.path(), project.path());
    command.args(["branch", "list"]);
    let output = run_with_timeout(command, cli_timeout());

    assert!(
        output.status.success(),
        "branch list should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Default branch: main"),
        "branch list should print its listing on stdout\nstdout:\n{stdout}"
    );
    assert!(
        stdout.contains("feature/branch-299-with-enough-detail-to-exercise-status-bounds"),
        "branch list should receive the complete explicitly requested branch diagnostics\nstdout:\n{stdout}"
    );
    assert!(
        !stdout.contains("No branch tracking configured"),
        "branch list should not fall back to repo-local metadata\nstdout:\n{stdout}"
    );

    let mut command = tracedecay_command_without_daemon(home.path(), project.path());
    command.args(["branch", "list", "--json"]);
    let output = run_with_timeout(command, cli_timeout());
    assert!(
        output.status.success(),
        "branch list --json should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("branch list --json prints one document");
    assert_eq!(diagnostics["default_branch"], "main");
    assert!(
        diagnostics["branches"]
            .as_array()
            .expect("branch diagnostics list branches")
            .iter()
            .any(|branch| branch["name"]
                == "feature/branch-299-with-enough-detail-to-exercise-status-bounds"),
        "{diagnostics:#}"
    );
}

/// `index.git_ignore.v1` is retired: the index is Git's view of the worktree,
/// so there is no toggle left for a `gitignore` command to report or flip.
#[test]
fn gitignore_command_is_refused_as_unknown() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write_git_fixture(project.path());
    for arguments in [&["gitignore"][..], &["gitignore", "off"][..]] {
        let mut command = tracedecay_command_without_daemon(home.path(), project.path());
        command.args(arguments);
        let output = run_with_timeout(command, cli_timeout());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{arguments:?} must be a usage error\nstderr:\n{stderr}"
        );
        assert!(
            stderr.contains("unrecognized subcommand 'gitignore'"),
            "{arguments:?} must be refused as an unknown command\nstderr:\n{stderr}"
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

    // A state outside the receipt contract is refused, not normalized.
    let mut command = tracedecay_command_without_daemon(home.path(), project.path());
    command.args(["automation", "facts", "list", "--state", " applied "]);
    let output = run_with_timeout(command, cli_timeout());
    assert!(!output.status.success(), "a padded state must be refused");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("invalid automatic fact state ` applied `; expected applied or quarantined"),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The daemon compiles the manual branch-activation journey only on Unix
/// (`branch_add_response` answers `code_index_scheduler_unavailable` under
/// `#[cfg(not(unix))]`), so this end-to-end journey is scoped the same way
/// rather than asserting a success the product does not offer there.
#[cfg(unix)]
#[test]
fn branch_add_admits_background_publication_and_remove_retires_its_exact_artifacts() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let caller = TempDir::new().unwrap();
    let project_root = canonical_temp_path(project.path());
    let caller_root = canonical_temp_path(caller.path());
    git(&project_root, &["init", "-b", "main"]);
    std::fs::write(project_root.join("lib.rs"), "pub fn indexed() {}\n").unwrap();
    commit_all(&project_root, "initial commit");
    init_project_fixture(home.path(), &project_root);
    git(&project_root, &["checkout", "-b", "feature/new"]);
    let project_id = default_profile_project_id(&project_root);
    let shard_root = profile_sharded_data_root(&profile_root(home.path()), &project_id);
    let daemon = crate::common::spawn_tracedecay_daemon(home.path());
    let mut command = tracedecay_command_without_daemon(home.path(), &project_root);
    command.args(["branch", "add", "feature/new"]);
    let output = run_with_timeout(command, cli_timeout());

    assert!(
        output.status.success(),
        "branch add must admit exact branch publication\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("indexing continues in the background"),
        "branch add must report truthful pending state"
    );
    let mut pending = tracedecay_command_without_daemon(home.path(), &project_root);
    pending.args(["branch", "list"]);
    let pending = run_with_timeout(pending, cli_timeout());
    let pending_stdout = String::from_utf8_lossy(&pending.stdout);
    assert!(
        pending.status.success(),
        "branch list must read durable admission\nstdout:\n{pending_stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&pending.stderr)
    );
    // The one-file publication may seal before this listing runs, so the
    // admitted branch reads either as pending or as synced, and a synced
    // branch serves only once the daemon switches to it. A synced listing
    // must already rest on sealed provenance; sealing never reverts.
    let admitted = pending_stdout
        .lines()
        .find(|line| line.starts_with("  feature/new "))
        .unwrap_or_else(|| panic!("admitted branch must be durably listed: {pending_stdout}"));
    let listed_pending = admitted.contains("indexing");
    if listed_pending {
        assert!(
            admitted.ends_with(", exact index pending"),
            "a pending branch must name its pending exact index: {admitted}"
        );
    } else {
        assert!(
            admitted.starts_with("  feature/new [current] (from main), synced ")
                || admitted.starts_with("  feature/new [current, serving] (from main), synced "),
            "admitted branch must read as pending or synced: {admitted}"
        );
        assert!(
            tracedecay_runtime_core::branch_meta::load_branch_meta(&shard_root)
                .and_then(|meta| meta.branches.get("feature/new").cloned())
                .is_some_and(|entry| entry.graph_source.is_some()),
            "a synced listing must rest on sealed provenance: {admitted}"
        );
    }
    let started = Instant::now();
    let meta = loop {
        if let Some(meta) = tracedecay_runtime_core::branch_meta::load_branch_meta(&shard_root)
            && meta
                .branches
                .get("feature/new")
                .is_some_and(|entry| entry.graph_source.is_some())
        {
            break meta;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "background branch publication did not seal exact provenance"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    let mut sealed = tracedecay_command_without_daemon(home.path(), &project_root);
    sealed.args(["branch", "list"]);
    let sealed = run_with_timeout(sealed, cli_timeout());
    let sealed_stdout = String::from_utf8_lossy(&sealed.stdout);
    assert!(
        sealed_stdout
            .lines()
            .any(|line| line.starts_with("  feature/new [current")
                && !line.contains("indexing")
                && line.contains(" (from main), synced ")),
        "a sealed branch must list as synced: {sealed_stdout}"
    );
    let entry = meta
        .branches
        .get("feature/new")
        .expect("branch add must track the branch");
    let source = entry
        .graph_source
        .as_ref()
        .expect("background branch publication must seal exact provenance");
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

    git(
        &project_root,
        &[
            "worktree",
            "add",
            "-b",
            "caller/linked",
            caller_root.to_str().unwrap(),
            "main",
        ],
    );
    let mut search = tracedecay_command_without_daemon(home.path(), &caller_root);
    search.args([
        "tool",
        "branch_search",
        "--args",
        r#"{"branch":"feature/new","query":"indexed","limit":5,"format":"json"}"#,
        "--json",
    ]);
    let search = run_with_timeout(search, cli_timeout());
    assert!(
        search.status.success(),
        "a linked worktree must consume an explicitly published branch generation\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&search.stdout),
        String::from_utf8_lossy(&search.stderr)
    );
    let envelope: serde_json::Value =
        serde_json::from_slice(&search.stdout).expect("branch search MCP envelope");
    let payload: serde_json::Value = serde_json::from_str(
        envelope["content"][0]["text"]
            .as_str()
            .expect("branch search JSON content"),
    )
    .expect("branch search payload");
    assert_eq!(payload["status"], "complete", "{payload:#}");
    assert!(
        payload["results"]
            .as_array()
            .is_some_and(|results| !results.is_empty()),
        "published branch search must return the indexed symbol: {payload:#}"
    );

    drop(daemon);
    let _restarted_daemon = crate::common::spawn_tracedecay_daemon(home.path());
    let mut list = tracedecay_command_without_daemon(home.path(), &project_root);
    list.args(["branch", "list"]);
    let listed = run_with_timeout(list, cli_timeout());
    let stdout = String::from_utf8_lossy(&listed.stdout);
    assert!(
        listed.status.success(),
        "branch list must reopen persisted branch tracking\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let branch = stdout
        .lines()
        .find(|line| line.contains("feature/new"))
        .expect("reopened branch list must retain feature/new");
    assert!(
        !branch.contains("indexing") && !branch.contains("missing-db"),
        "reopened branch must remain exact and ready: {branch}"
    );

    let mut remove = tracedecay_command_without_daemon(home.path(), &project_root);
    remove.args(["branch", "remove", "feature/new"]);
    let remove_output = run_with_timeout(remove, cli_timeout());
    assert!(
        remove_output.status.success(),
        "branch remove must retire the exact manually activated branch\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&remove_output.stdout),
        String::from_utf8_lossy(&remove_output.stderr)
    );
    assert!(
        !sealed_worktree.exists(),
        "branch remove must delete the sealed linked worktree"
    );
    let tracking_ref = Command::new("git")
        .args([
            "rev-parse",
            "--verify",
            "--end-of-options",
            "refs/tracedecay/branch/feature/new",
        ])
        .current_dir(&project_root)
        .output()
        .expect("git should verify the tracking ref");
    assert!(
        !tracking_ref.status.success(),
        "branch remove must retire the exact raw branch tracking ref"
    );
    assert!(
        !tracedecay_runtime_core::branch_meta::load_branch_meta(&shard_root)
            .expect("branch metadata after removal")
            .is_tracked("feature/new"),
        "branch remove must retire its metadata only after exact provenance cleanup is selected"
    );
}

#[cfg(unix)]
#[test]
fn branch_search_serves_a_committed_generation_behind_dirty_worktree_state() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_root = canonical_temp_path(project.path());
    git(&project_root, &["init", "-b", "main"]);
    std::fs::write(
        project_root.join("lib.rs"),
        "pub fn committed_anchor() -> usize { 1 }\n",
    )
    .unwrap();
    commit_all(&project_root, "initial commit");
    std::fs::write(
        project_root.join("lib.rs"),
        concat!(
            "pub fn committed_anchor() -> usize { 1 }\n",
            "pub fn dirty_anchor() -> usize { 2 }\n",
        ),
    )
    .unwrap();
    init_project_fixture(home.path(), &project_root);

    let _daemon = crate::common::spawn_tracedecay_daemon(home.path());
    let ready_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let mut ready = tracedecay_command_without_daemon(home.path(), &project_root);
        ready.args([
            "tool",
            "search",
            "--args",
            r#"{"query":"dirty_anchor","limit":5,"format":"json"}"#,
            "--json",
        ]);
        let ready = run_with_timeout(ready, cli_timeout());
        let dirty_generation_ready = ready.status.success()
            && serde_json::from_slice::<serde_json::Value>(&ready.stdout)
                .ok()
                .and_then(|envelope| envelope["content"][0]["text"].as_str().map(str::to_owned))
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                .and_then(|payload| payload["results"].as_array().cloned())
                .is_some_and(|results| !results.is_empty());
        if dirty_generation_ready {
            break;
        }
        assert!(
            Instant::now() < ready_deadline,
            "dirty worktree generation did not become queryable\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&ready.stdout),
            String::from_utf8_lossy(&ready.stderr)
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    let mut search = tracedecay_command_without_daemon(home.path(), &project_root);
    search.args([
        "tool",
        "branch_search",
        "--args",
        r#"{"branch":"main","query":"committed_anchor","limit":5,"format":"json"}"#,
        "--json",
    ]);
    let search = run_with_timeout(search, cli_timeout());
    assert!(
        search.status.success(),
        "branch search must derive a queryable text owner for the exact committed generation\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&search.stdout),
        String::from_utf8_lossy(&search.stderr)
    );
    let envelope: serde_json::Value =
        serde_json::from_slice(&search.stdout).expect("branch search MCP envelope");
    let payload: serde_json::Value = serde_json::from_str(
        envelope["content"][0]["text"]
            .as_str()
            .expect("branch search JSON content"),
    )
    .expect("branch search payload");
    assert_eq!(payload["status"], "complete", "{payload:#}");
    assert!(
        payload["results"]
            .as_array()
            .is_some_and(|results| !results.is_empty()),
        "branch search must return the committed symbol: {payload:#}"
    );
}

#[tokio::test]
async fn branch_removeall_retires_every_tracked_branch() {
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
    write_branch_meta(&shard_root, &["feature/one", "feature/two"]);

    let mut command = tracedecay_command(home.path(), project.path());
    command.args(["branch", "removeall"]);
    let output = run_with_timeout(command, cli_timeout());

    assert!(
        output.status.success(),
        "branch removeall should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let meta = tracedecay_runtime_core::branch_meta::load_branch_meta(&shard_root).unwrap();
    assert_eq!(
        meta.branches.keys().collect::<Vec<_>>(),
        vec!["main"],
        "branch removeall should retire every non-default tracked branch"
    );
    assert!(shard_root.join("tracedecay.db").exists());
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
    write_branch_meta(&shard_root, &["feature/stale"]);

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
        tracedecay_runtime_core::branch_meta::load_branch_meta(&shard_root)
            .unwrap()
            .is_tracked("feature/stale"),
        "branch gc must fail closed without repository branch evidence"
    );
}

#[test]
fn init_refuses_ephemeral_project_in_persistent_profile() {
    let Some(durable) = ephemeral_safe_fixture_base(
        "cli_non_interactive_test::init_refuses_ephemeral_project_in_persistent_profile",
    ) else {
        return;
    };
    let home = TempDir::new().expect("home tempdir");
    let project = TempDir::new().expect("ephemeral project");
    std::fs::write(project.path().join("lib.rs"), "pub fn transient() {}\n")
        .expect("ephemeral source");
    let profile = tempfile::Builder::new()
        .prefix("persistent-profile-")
        .tempdir_in(durable)
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
/// (plan 38 §7, size observability reachable from a command).
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
    std::fs::write(profile_root.join("user-sessions.db"), vec![1u8; 2048]).unwrap();

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
    assert_eq!(report["user_sessions_db_bytes"], 2048);
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
