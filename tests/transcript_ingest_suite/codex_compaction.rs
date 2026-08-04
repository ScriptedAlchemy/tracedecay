//! Codex context-compaction ingestion: LCM summary nodes, incremental depth,
//! successor publication, pending-leaf tracking, and writer rollback.

use std::io::Write;

use tempfile::TempDir;
use tracedecay::application::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay::global_db::ParseOffset;
use tracedecay::sessions::codex::CodexSource;
use tracedecay::sessions::lcm::{
    LcmContentSlice, LcmDescribeRequest, LcmDescribeTarget, LcmExpandRequest, LcmExpandTarget,
};
use tracedecay_domain::ProjectId;

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

#[tokio::test]
async fn codex_context_compaction_creates_lcm_summary_node() {
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
    assert_eq!(description.summary_nodes[0].depth, 1);
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
    assert!(
        expansion
            .content
            .contains("Map the release automation state")
    );
    assert!(expansion.content.contains("Release automation is mapped"));
    assert!(!expansion.content.contains("Summary body is encrypted"));
    assert_eq!(expansion.summary_sources.len(), 2);
}
#[tokio::test]
async fn repeated_codex_compactions_only_source_messages_since_previous_boundary() {
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

    let description = runtime
        .lcm_describe_for_test(LcmDescribeRequest {
            provider: "codex".to_string(),
            session_id: "codex-repeat".to_string(),
            target: LcmDescribeTarget::Session,
        })
        .await
        .unwrap();
    assert_eq!(description.summary_nodes.len(), 2);
    let source_counts = description
        .summary_nodes
        .iter()
        .map(|node| (node.depth, node.source_count))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(source_counts.get(&1), Some(&2));
    assert_eq!(source_counts.get(&2), Some(&2));
}

#[tokio::test]
async fn incremental_codex_compaction_depth_continues_from_prior_history() {
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

    let description = runtime
        .lcm_describe_for_test(LcmDescribeRequest {
            provider: "codex".to_string(),
            session_id: "codex-incremental".to_string(),
            target: LcmDescribeTarget::Session,
        })
        .await
        .unwrap();
    let depths = description
        .summary_nodes
        .iter()
        .map(|node| node.depth)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(depths, [1, 2].into_iter().collect());
}

#[tokio::test]
async fn codex_compaction_depth_resets_when_rollout_replays_from_start() {
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

    let description = runtime
        .lcm_describe_for_test(LcmDescribeRequest {
            provider: "codex".to_string(),
            session_id: "codex-replay".to_string(),
            target: LcmDescribeTarget::Session,
        })
        .await
        .unwrap();
    let depths = description
        .summary_nodes
        .iter()
        .map(|node| node.depth)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(depths, [1, 2].into_iter().collect());
}
