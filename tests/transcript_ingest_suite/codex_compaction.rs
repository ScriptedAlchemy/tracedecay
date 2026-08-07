//! Codex context-compaction ingestion and daemon-owned summary publication.
//! Transcript reads retain exact native evidence; only the PostCompact effect
//! may turn that evidence or an app-server result into an LCM summary node.

use std::io::Write;
#[cfg(unix)]
use std::process::Stdio;

use tempfile::TempDir;
use tracedecay::application::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay::global_db::ParseOffset;
use tracedecay::sessions::codex::CodexSource;
use tracedecay::sessions::lcm::{
    LcmContentSlice, LcmDescribeRequest, LcmDescribeTarget, LcmExpandRequest, LcmExpandTarget,
};
use tracedecay_domain::ProjectId;

#[cfg(unix)]
use crate::common::{
    EnvVarGuard, GLOBAL_DB_ENV, GLOBAL_DB_ENV_LOCK, spawn_tracedecay_daemon,
    tracedecay_command_with_home,
};
#[cfg(unix)]
use crate::restart_atomicity::mark_test_project;
use crate::support::setup;

async fn registered_runtime(
    home: &std::path::Path,
    project: &std::path::Path,
) -> HostAdmissionTestRuntimeV1 {
    HostAdmissionTestRuntimeV1::project(
        home.join(".tracedecay"),
        project,
        ProjectId::new("project.codex-compaction").unwrap(),
    )
    .await
    .unwrap()
}

fn write_codex_rollout_with_compaction(
    home: &std::path::Path,
    project: &std::path::Path,
    session: &str,
) -> std::path::PathBuf {
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("rollout-2026-01-01T00-00-20-{session}.jsonl"));
    let contents = format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n",
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:20.000Z",
            "type": "session_meta",
            "payload": {"id": session, "cwd": project.to_string_lossy(), "model": "gpt-5.5"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:21.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "Map the release automation state"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:22.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "Release automation is mapped."}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:23.000Z",
            "type": "compacted",
            "payload": {
                "message": "",
                "replacement_history": [
                    {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Map the release automation state"}]},
                    {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Release automation is mapped."}]},
                    {"type": "compaction", "encrypted_content": "encrypted-codex-summary"}
                ]
            }
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:23.010Z",
            "type": "event_msg",
            "payload": {"type": "context_compacted"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:24.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "Continue after compaction"}
        }),
    );
    std::fs::write(&path, contents).unwrap();
    path
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn codex_post_compact_hook_commits_app_server_summary_through_daemon_effect() {
    let tmp = TempDir::new().unwrap();
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (home, project) = setup(&tmp);
    let profile = home.join(".tracedecay");
    let _env_guards = [
        EnvVarGuard::set("TRACEDECAY_DATA_DIR", &profile),
        EnvVarGuard::set(GLOBAL_DB_ENV, profile.join("global.db")),
        EnvVarGuard::set("HOME", &home),
        EnvVarGuard::set("USERPROFILE", &home),
    ];
    let project_id = mark_test_project(&project);
    // The hook resolves the project root through the initialized-store gate,
    // exactly like production installs: `init` creates the project store
    // first, then daemon enrollment mounts the already-initialized layout
    // (the canonical enrollment composition itself creates the project graph
    // database, so init must come first — as it does in a real install).
    let init = tracedecay_command_with_home(&home)
        .arg("init")
        .current_dir(&project)
        .output()
        .expect("initialize codex compaction project");
    assert!(
        init.status.success(),
        "fixture init failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&init.stdout),
        String::from_utf8_lossy(&init.stderr)
    );
    let enrollment = HostAdmissionTestRuntimeV1::project(&profile, &project, project_id.clone())
        .await
        .unwrap();
    drop(enrollment);
    write_codex_rollout_with_compaction(&home, &project, "codex-compact");

    let codex_bin = tmp.path().join("codex");
    std::fs::write(
        &codex_bin,
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"id":0'*) printf '%s\n' '{"id":0,"result":{}}' ;;
    *'"id":1'*) printf '%s\n' '{"id":1,"result":{"thread":{"id":"thread-1","model":"codex-hook-test"}}}' ;;
    *'"id":2'*)
      printf '%s\n' '{"method":"item/completed","params":{"model":"codex-hook-test","item":{"content":[{"type":"output_text","text":"Codex authoritative hook summary"}]}}}'
      printf '%s\n' '{"method":"turn/completed"}'
      ;;
  esac
done
"#,
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&codex_bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let _summary_env = [
        EnvVarGuard::set("TRACEDECAY_CODEX_BIN", &codex_bin),
        EnvVarGuard::set("TRACEDECAY_CODEX_SUMMARY_TIMEOUT_SECS", "5"),
    ];
    let daemon = spawn_tracedecay_daemon(&home);
    let event = serde_json::json!({
        "hook_event_name": "PostCompact",
        "session_id": "codex-compact",
        "cwd": project,
        "context_tokens": 124_000,
        "context_window_size": 128_000
    });
    let mut hook = tracedecay_command_with_home(&home);
    let mut child = hook
        .arg("hook-codex-post-compact")
        .current_dir(&project)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(child.stdin.take().unwrap(), "{event}").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "Codex PostCompact hook failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("PostCompact daemon call failed"),
        "the PostCompact hook must not fail open around the daemon effect: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    drop(daemon);

    let runtime = HostAdmissionTestRuntimeV1::project(&profile, &project, project_id)
        .await
        .unwrap();

    let status = runtime
        .lcm_status_for_test("codex", Some("codex-compact"))
        .await
        .unwrap();
    assert_eq!(status.raw_message_count, 4);
    assert_eq!(status.summary_node_count, 1);
    assert!(status.dag.depths.values().any(|depth| depth.count == 1));

    let description = runtime
        .lcm_describe_for_test(LcmDescribeRequest {
            provider: "codex".to_string(),
            session_id: "codex-compact".to_string(),
            target: LcmDescribeTarget::Session,
        })
        .await
        .unwrap();
    assert_eq!(description.summary_nodes.len(), 1);
    // The daemon compression publishes a leaf summary over the session's raw
    // backlog (depth 0); the retired host side channel used to publish its
    // replacement-history summary at depth 1.
    assert_eq!(description.summary_nodes[0].depth, 0);
    assert_eq!(description.summary_nodes[0].source_count, 2);

    let node_id = description.summary_nodes[0].node_id.clone();
    let expanded = runtime
        .lcm_describe_for_test(LcmDescribeRequest {
            provider: "codex".to_string(),
            session_id: "codex-compact".to_string(),
            target: LcmDescribeTarget::SummaryNode { node_id },
        })
        .await
        .unwrap();
    let summary = expanded.summary_node.expect("summary node should expand");
    assert_eq!(summary.source_count, 2);
    // Provenance metadata rides the node-level describe; the session-level
    // summary listing is a lightweight overview without metadata_json.
    assert!(
        summary
            .metadata_json
            .as_deref()
            .unwrap_or_default()
            .contains("codex_app_server:codex-hook-test")
    );

    let expansion = runtime
        .lcm_expand_for_test(LcmExpandRequest {
            provider: "codex".to_string(),
            session_id: "codex-compact".to_string(),
            target: LcmExpandTarget::SummaryNode {
                node_id: summary.node_id.clone(),
            },
            content_slice: Some(LcmContentSlice {
                offset: 0,
                limit: 1024,
            }),
            source_offset: 0,
            source_limit: Some(10),
        })
        .await
        .unwrap();
    assert_eq!(
        expansion
            .summary_node
            .as_ref()
            .expect("expanded summary node should be present")
            .summary_text,
        "Codex authoritative hook summary"
    );
    // The summary expansion returns the summary text as its content and the
    // exact raw sources alongside; the encrypted native payload never lands.
    assert!(!expansion.content.contains("encrypted-codex-summary"));
    assert_eq!(expansion.summary_sources.len(), 2);
    let source_contents = expansion
        .summary_sources
        .iter()
        .map(|source| source.content.as_str())
        .collect::<Vec<_>>();
    assert!(source_contents.contains(&"Map the release automation state"));
    assert!(source_contents.contains(&"Release automation is mapped."));
    assert!(
        !source_contents
            .iter()
            .any(|content| content.contains("encrypted-codex-summary"))
    );
}

#[cfg(not(unix))]
#[tokio::test]
async fn codex_transcript_compaction_evidence_does_not_publish_outside_daemon_effect() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_codex_rollout_with_compaction(&home, &project, "codex-compact");
    let runtime = registered_runtime(&home, &project).await;
    let source = CodexSource::with_home(&home);
    let stats = runtime
        .ingest_project_transcript_source_for_test(&source, &project, None)
        .await
        .unwrap();
    assert_eq!(stats.messages_upserted, 4);
    let status = runtime
        .lcm_status_for_test("codex", Some("codex-compact"))
        .await
        .unwrap();
    assert_eq!(status.summary_node_count, 0);
}
#[tokio::test]
async fn repeated_codex_compactions_remain_native_evidence_until_daemon_effect() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rollout-2026-01-01T00-00-30-codex-repeat.jsonl");
    let cwd = project.to_string_lossy();
    let compact = |at: &str| {
        serde_json::json!({
            "timestamp": at,
            "type": "compacted",
            "payload": {
                "message": "",
                "replacement_history": [
                    {"type": "compaction", "encrypted_content": "encrypted"}
                ]
            }
        })
    };
    let lines = [
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:30.000Z",
            "type": "session_meta",
            "payload": {"id": "codex-repeat", "cwd": cwd, "model": "gpt-5.5"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:31.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "First compacted prompt"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:32.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "First compacted reply"}
        }),
        compact("2026-01-01T00:00:33.000Z"),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:34.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "Second compacted prompt"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:35.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "Second compacted reply"}
        }),
        compact("2026-01-01T00:00:36.000Z"),
    ];
    std::fs::write(
        &path,
        lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();

    let runtime = registered_runtime(&home, &project).await;
    let source = CodexSource::with_home(&home);
    let stats = runtime
        .ingest_project_transcript_source_for_test(&source, &project, None)
        .await
        .unwrap();
    assert_eq!(stats.messages_upserted, 6);

    let status = runtime
        .lcm_status_for_test("codex", Some("codex-repeat"))
        .await
        .unwrap();
    assert_eq!(status.raw_message_count, 6);
    assert_eq!(status.summary_node_count, 0);
}

#[tokio::test]
async fn incremental_codex_compaction_ingest_never_bypasses_daemon_effect() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rollout-2026-01-01T00-00-40-codex-incremental.jsonl");
    let cwd = project.to_string_lossy();
    let compact = |at: &str| {
        serde_json::json!({
            "timestamp": at,
            "type": "compacted",
            "payload": {
                "message": "",
                "replacement_history": [
                    {"type": "compaction", "encrypted_content": "encrypted"}
                ]
            }
        })
    };
    let first = [
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:40.000Z",
            "type": "session_meta",
            "payload": {"id": "codex-incremental", "cwd": cwd, "model": "gpt-5.5"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:41.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "First incremental prompt"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:42.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "First incremental reply"}
        }),
        compact("2026-01-01T00:00:43.000Z"),
    ];
    std::fs::write(
        &path,
        first
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();

    let runtime = registered_runtime(&home, &project).await;
    let source = CodexSource::with_home(&home);
    let stats = runtime
        .ingest_project_transcript_source_for_test(&source, &project, None)
        .await
        .unwrap();
    assert_eq!(stats.messages_upserted, 3);

    let second = [
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:44.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "Second incremental prompt"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:45.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "Second incremental reply"}
        }),
        compact("2026-01-01T00:00:46.000Z"),
    ];
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    for line in second {
        writeln!(file, "{line}").unwrap();
    }

    let stats = runtime
        .ingest_project_transcript_source_for_test(&source, &project, None)
        .await
        .unwrap();
    assert_eq!(stats.messages_upserted, 3);

    let status = runtime
        .lcm_status_for_test("codex", Some("codex-incremental"))
        .await
        .unwrap();
    assert_eq!(status.raw_message_count, 6);
    assert_eq!(status.summary_node_count, 0);
}

#[tokio::test]
async fn replayed_codex_compaction_ingest_never_bypasses_daemon_effect() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rollout-2026-01-01T00-00-45-codex-replay.jsonl");
    let cwd = project.to_string_lossy();
    let compact = |at: &str| {
        serde_json::json!({
            "timestamp": at,
            "type": "compacted",
            "payload": {
                "message": "",
                "replacement_history": [
                    {"type": "compaction", "encrypted_content": "encrypted"}
                ]
            }
        })
    };
    let lines = [
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:45.000Z",
            "type": "session_meta",
            "payload": {"id": "codex-replay", "cwd": cwd, "model": "gpt-5.5"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:46.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "First replay prompt"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:47.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "First replay reply"}
        }),
        compact("2026-01-01T00:00:48.000Z"),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:49.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "Second replay prompt"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:50.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "Second replay reply"}
        }),
        compact("2026-01-01T00:00:51.000Z"),
    ];
    let contents = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&path, contents).unwrap();

    let runtime = registered_runtime(&home, &project).await;
    let source = CodexSource::with_home(&home);
    let path_str = path.to_string_lossy().to_string();
    runtime
        .set_project_parse_offset_for_test(
            &path_str,
            ParseOffset {
                byte_offset: std::fs::metadata(&path).unwrap().len(),
                mtime: 1,
                file_id: 1,
            },
        )
        .await
        .unwrap();

    let stats = runtime
        .ingest_project_transcript_source_for_test(&source, &project, None)
        .await
        .unwrap();
    assert_eq!(stats.messages_upserted, 6);

    let status = runtime
        .lcm_status_for_test("codex", Some("codex-replay"))
        .await
        .unwrap();
    assert_eq!(status.raw_message_count, 6);
    assert_eq!(status.summary_node_count, 0);
}
