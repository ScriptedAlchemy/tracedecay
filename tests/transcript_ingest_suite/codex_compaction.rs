//! Codex context-compaction ingestion: LCM summary nodes, incremental depth,
//! successor publication, pending-leaf tracking, and writer rollback.

use std::io::Write;

use tempfile::TempDir;
use tracedecay::global_db::ParseOffset;
use tracedecay::sessions::codex::CodexSource;
use tracedecay::sessions::cursor::{open_project_session_db, resolved_project_session_db_path};
use tracedecay::sessions::lcm::{
    LcmContentSlice, LcmDescribeRequest, LcmDescribeTarget, LcmExpandRequest, LcmExpandTarget,
};
use tracedecay::sessions::source::try_ingest_source;

use crate::support::setup;

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

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);

    let stats = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(stats.messages_upserted, 4);

    let status = db.lcm_status("codex", Some("codex-compact")).await.unwrap();
    assert_eq!(status.raw_message_count, 4);
    assert_eq!(status.summary_node_count, 1);
    assert!(status.dag.depths.values().any(|depth| depth.count == 1));

    let description = db
        .lcm_describe(LcmDescribeRequest {
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
    let expanded = db
        .lcm_describe(LcmDescribeRequest {
            provider: "codex".to_string(),
            session_id: "codex-compact".to_string(),
            target: LcmDescribeTarget::SummaryNode { node_id },
        })
        .await
        .unwrap();
    let summary = expanded.summary_node.expect("summary node should expand");
    assert_eq!(summary.source_count, 2);

    let expansion = db
        .lcm_expand(LcmExpandRequest {
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

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);
    let stats = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(stats.messages_upserted, 6);

    let description = db
        .lcm_describe(LcmDescribeRequest {
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

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);
    let stats = try_ingest_source(&db, &source, &project, None)
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

    let stats = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(stats.messages_upserted, 3);

    let description = db
        .lcm_describe(LcmDescribeRequest {
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

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);
    let path_str = path.to_string_lossy().to_string();
    db.set_parse_offset(
        &path_str,
        ParseOffset {
            byte_offset: std::fs::metadata(&path).unwrap().len(),
            mtime: 1,
            file_id: 1,
        },
    )
    .await;

    let stats = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(stats.messages_upserted, 6);

    let description = db
        .lcm_describe(LcmDescribeRequest {
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

#[tokio::test]
async fn codex_compaction_summary_can_publish_immutable_successor_with_auxiliary_summary() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_codex_rollout_with_compaction(&home, &project, "codex-compact");

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);
    try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();

    let pending = db
        .pending_codex_compaction_summary_requests(Some("codex-compact"), 10)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].request.provider, "codex");
    assert_eq!(pending[0].request.session_id, "codex-compact");
    assert_eq!(
        pending[0]
            .request
            .source_messages
            .iter()
            .map(|message| (message.role.as_str(), message.content.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("user", "Map the release automation state"),
            ("assistant", "Release automation is mapped.")
        ]
    );

    let predecessor_node_id = pending[0].node_id.clone();
    let predecessor_before = db
        .lcm_expand_summary_node("codex", "codex-compact", &predecessor_node_id)
        .await
        .unwrap();
    let successor = db
        .publish_codex_compaction_summary_successor(
            &predecessor_node_id,
            "Auxiliary Codex app-server summary",
            "codex_app_server",
            Some("gpt-5.4"),
        )
        .await
        .unwrap();
    assert_eq!(successor.summary_text, "Auxiliary Codex app-server summary");
    assert_ne!(successor.node_id, predecessor_node_id);
    assert_eq!(
        successor.source_refs,
        predecessor_before.summary.source_refs
    );
    let successor_metadata: serde_json::Value =
        serde_json::from_str(successor.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        successor_metadata
            .get("source")
            .and_then(serde_json::Value::as_str),
        Some("codex_context_compacted")
    );
    assert_eq!(
        successor_metadata
            .get("tracedecay_summary_source")
            .and_then(serde_json::Value::as_str),
        Some("codex_app_server")
    );
    assert_eq!(
        successor_metadata
            .get("codex_auxiliary_model")
            .and_then(serde_json::Value::as_str),
        Some("gpt-5.4")
    );

    let replayed_successor = db
        .publish_codex_compaction_summary_successor(
            &predecessor_node_id,
            "Auxiliary Codex app-server summary",
            "codex_app_server",
            Some("gpt-5.4"),
        )
        .await
        .unwrap();
    assert_eq!(replayed_successor, successor);

    let pending_after = db
        .pending_codex_compaction_summary_requests(Some("codex-compact"), 10)
        .await
        .unwrap();
    assert!(pending_after.is_empty());

    let status = db.lcm_status("codex", Some("codex-compact")).await.unwrap();
    assert_eq!(status.summary_node_count, 2);

    let predecessor_after = db
        .lcm_expand_summary_node("codex", "codex-compact", &predecessor_node_id)
        .await
        .expect("predecessor summary remains addressable after successor publish");
    assert_eq!(predecessor_after, predecessor_before);
    let successor_expansion = db
        .lcm_expand_summary_node("codex", "codex-compact", &successor.node_id)
        .await
        .unwrap();
    assert_eq!(successor_expansion.summary.node_id, successor.node_id);
    assert_eq!(
        successor_expansion.summary.summary_text,
        successor.summary_text
    );
    assert_eq!(
        successor_expansion.summary.metadata_json,
        successor.metadata_json
    );
    assert_eq!(
        successor_expansion.summary.source_refs,
        successor.source_refs
    );
    assert_eq!(successor_expansion.sources, predecessor_before.sources);

    let db_path = resolved_project_session_db_path(&project).await.unwrap();
    let lineage_db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let lineage_conn = lineage_db.connect().unwrap();
    let mut lineage_rows = lineage_conn
        .query(
            "SELECT successor_summary_id
             FROM session_summary_successors
             WHERE predecessor_summary_id = ?1
             ORDER BY successor_summary_id",
            libsql::params![predecessor_node_id.as_str()],
        )
        .await
        .unwrap();
    let persisted_successor_id: String = lineage_rows
        .next()
        .await
        .unwrap()
        .expect("successful publication must persist a successor edge")
        .get(0)
        .unwrap();
    assert_eq!(persisted_successor_id, successor.node_id);
    assert!(lineage_rows.next().await.unwrap().is_none());
}

#[tokio::test]
async fn codex_compaction_pending_tracks_only_current_non_app_leaf() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_codex_rollout_with_compaction(&home, &project, "codex-compact-chain");

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);
    try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();

    let pending = db
        .pending_codex_compaction_summary_requests(Some("codex-compact-chain"), 10)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    let predecessor_node_id = pending[0].node_id.clone();
    let predecessor_before = db
        .lcm_expand_summary_node("codex", "codex-compact-chain", &predecessor_node_id)
        .await
        .unwrap();

    let non_app = db
        .publish_codex_compaction_summary_successor(
            &predecessor_node_id,
            "Intermediate non-app summary",
            "codex_local",
            Some("local-model"),
        )
        .await
        .unwrap();
    assert_eq!(non_app.source_refs, predecessor_before.summary.source_refs);
    let non_app_before = db
        .lcm_expand_summary_node("codex", "codex-compact-chain", &non_app.node_id)
        .await
        .unwrap();

    let pending_non_app = db
        .pending_codex_compaction_summary_requests(Some("codex-compact-chain"), 10)
        .await
        .unwrap();
    assert_eq!(pending_non_app.len(), 1);
    assert_eq!(pending_non_app[0].node_id, non_app.node_id);

    let branch_error = db
        .publish_codex_compaction_summary_successor(
            &predecessor_node_id,
            "Invalid sibling summary",
            "codex_local",
            None,
        )
        .await
        .expect_err("a non-current predecessor must not branch");
    assert!(matches!(
        branch_error,
        tracedecay::sessions::lcm::LcmError::InvalidSummarySuccessor {
            predecessor_summary_id,
            ..
        } if predecessor_summary_id == predecessor_node_id
    ));

    let app = db
        .publish_codex_compaction_summary_successor(
            &non_app.node_id,
            "Final Codex app-server summary",
            "codex_app_server",
            Some("gpt-5.4"),
        )
        .await
        .unwrap();
    let replayed_app = db
        .publish_codex_compaction_summary_successor(
            &non_app.node_id,
            "Final Codex app-server summary",
            "codex_app_server",
            Some("gpt-5.4"),
        )
        .await
        .unwrap();
    assert_eq!(replayed_app, app);
    assert_eq!(app.source_refs, non_app.source_refs);
    assert!(
        db.pending_codex_compaction_summary_requests(Some("codex-compact-chain"), 10)
            .await
            .unwrap()
            .is_empty()
    );

    let predecessor_after = db
        .lcm_expand_summary_node("codex", "codex-compact-chain", &predecessor_node_id)
        .await
        .unwrap();
    let non_app_after = db
        .lcm_expand_summary_node("codex", "codex-compact-chain", &non_app.node_id)
        .await
        .unwrap();
    let app_expansion = db
        .lcm_expand_summary_node("codex", "codex-compact-chain", &app.node_id)
        .await
        .unwrap();
    assert_eq!(predecessor_after, predecessor_before);
    assert_eq!(non_app_after, non_app_before);
    assert_eq!(app_expansion.sources, non_app_before.sources);

    let non_app_metadata: serde_json::Value =
        serde_json::from_str(non_app.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        non_app_metadata
            .get("tracedecay_summary_source")
            .and_then(serde_json::Value::as_str),
        Some("codex_local")
    );
    let app_metadata: serde_json::Value =
        serde_json::from_str(app.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        app_metadata
            .get("tracedecay_summary_source")
            .and_then(serde_json::Value::as_str),
        Some("codex_app_server")
    );
    assert_eq!(
        app_metadata
            .get("codex_auxiliary_model")
            .and_then(serde_json::Value::as_str),
        Some("gpt-5.4")
    );

    let db_path = resolved_project_session_db_path(&project).await.unwrap();
    let lineage_db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let lineage_conn = lineage_db.connect().unwrap();
    let mut lineage_rows = lineage_conn
        .query(
            "SELECT predecessor_summary_id, successor_summary_id
             FROM session_summary_successors
             ORDER BY predecessor_summary_id, successor_summary_id",
            (),
        )
        .await
        .unwrap();
    let mut persisted_edges = Vec::new();
    while let Some(row) = lineage_rows.next().await.unwrap() {
        persisted_edges.push((row.get::<String>(0).unwrap(), row.get::<String>(1).unwrap()));
    }
    let mut expected_edges = vec![
        (predecessor_node_id, non_app.node_id),
        (non_app_before.summary.node_id, app.node_id),
    ];
    expected_edges.sort();
    assert_eq!(persisted_edges, expected_edges);
}

#[tokio::test]
async fn codex_compaction_queue_skips_unpublishable_poison_before_limit() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_codex_rollout_with_compaction(&home, &project, "codex-compact-poison");

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);
    try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    let pending = db
        .pending_codex_compaction_summary_requests(Some("codex-compact-poison"), 10)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    let predecessor_node_id = pending[0].node_id.clone();
    let predecessor_before = db
        .lcm_expand_summary_node("codex", "codex-compact-poison", &predecessor_node_id)
        .await
        .unwrap();

    let db_path = resolved_project_session_db_path(&project).await.unwrap();
    let poison_db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let poison_conn = poison_db.connect().unwrap();
    poison_conn
        .execute(
            "INSERT INTO lcm_summary_nodes (
                node_id, provider, conversation_id, session_id, depth,
                summary_text, summary_hash, summary_token_count, source_token_count,
                source_time_start, source_time_end, expand_hint, metadata_json, created_at
             )
             SELECT ?1, provider, conversation_id, session_id, depth + 1000,
                    'unpublishable poison', 'poison-hash', summary_token_count,
                    source_token_count, source_time_start, source_time_end,
                    expand_hint, metadata_json, created_at + 1000000
             FROM lcm_summary_nodes
             WHERE node_id = ?2",
            libsql::params!["poison-summary", predecessor_node_id.as_str()],
        )
        .await
        .unwrap();
    drop(poison_conn);
    drop(poison_db);

    let bounded_pending = db
        .pending_codex_compaction_summary_requests(Some("codex-compact-poison"), 1)
        .await
        .unwrap();
    assert_eq!(bounded_pending.len(), 1);
    assert_eq!(bounded_pending[0].node_id, predecessor_node_id);

    db.publish_codex_compaction_summary_successor(
        &predecessor_node_id,
        "Processed despite poison",
        "codex_app_server",
        Some("gpt-5.4"),
    )
    .await
    .unwrap();
    assert!(
        db.pending_codex_compaction_summary_requests(Some("codex-compact-poison"), 1)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        db.lcm_expand_summary_node("codex", "codex-compact-poison", &predecessor_node_id)
            .await
            .unwrap(),
        predecessor_before
    );
}

#[tokio::test]
async fn codex_compaction_summary_successor_rolls_back_and_reuses_writer_after_failure() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_codex_rollout_with_compaction(&home, &project, "codex-compact-rollback");

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);
    try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    let pending = db
        .pending_codex_compaction_summary_requests(Some("codex-compact-rollback"), 10)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    let original_node_id = pending[0].node_id.clone();
    let predecessor_before = db
        .lcm_expand_summary_node("codex", "codex-compact-rollback", &original_node_id)
        .await
        .unwrap();
    let intermediate = db
        .publish_codex_compaction_summary_successor(
            &original_node_id,
            "Intermediate rollback leaf",
            "codex_local",
            None,
        )
        .await
        .unwrap();
    let pending_leaf = db
        .pending_codex_compaction_summary_requests(Some("codex-compact-rollback"), 10)
        .await
        .unwrap();
    assert_eq!(pending_leaf.len(), 1);
    assert_eq!(pending_leaf[0].node_id, intermediate.node_id);
    let leaf_request = pending_leaf[0].request.clone();
    let leaf_before = db
        .lcm_expand_summary_node("codex", "codex-compact-rollback", &intermediate.node_id)
        .await
        .unwrap();

    let db_path = resolved_project_session_db_path(&project).await.unwrap();
    let trigger_db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let trigger_conn = trigger_db.connect().unwrap();
    trigger_conn
        .execute_batch(
            "CREATE TRIGGER fail_codex_summary_successor
             BEFORE INSERT ON lcm_summary_nodes
             BEGIN
                SELECT RAISE(ABORT, 'forced summary successor failure');
             END;",
        )
        .await
        .unwrap();

    let error = db
        .publish_codex_compaction_summary_successor(
            &intermediate.node_id,
            "Failed replacement",
            "codex_app_server",
            None,
        )
        .await
        .expect_err("trigger should abort immutable successor publish");
    assert!(
        format!("{error:?}").contains("forced summary successor failure"),
        "unexpected error: {error:?}"
    );
    let pending_after_failure = db
        .pending_codex_compaction_summary_requests(Some("codex-compact-rollback"), 10)
        .await
        .unwrap();
    assert_eq!(pending_after_failure.len(), 1);
    assert_eq!(pending_after_failure[0].node_id, intermediate.node_id);
    assert_eq!(pending_after_failure[0].request, leaf_request);
    let predecessor_after_failure = db
        .lcm_expand_summary_node("codex", "codex-compact-rollback", &original_node_id)
        .await
        .unwrap();
    let leaf_after_failure = db
        .lcm_expand_summary_node("codex", "codex-compact-rollback", &intermediate.node_id)
        .await
        .unwrap();
    assert_eq!(predecessor_after_failure, predecessor_before);
    assert_eq!(leaf_after_failure, leaf_before);
    assert_eq!(
        db.lcm_status("codex", Some("codex-compact-rollback"))
            .await
            .unwrap()
            .summary_node_count,
        2
    );

    trigger_conn
        .execute_batch("DROP TRIGGER fail_codex_summary_successor;")
        .await
        .unwrap();
    drop(trigger_conn);
    drop(trigger_db);

    let successor = db
        .publish_codex_compaction_summary_successor(
            &intermediate.node_id,
            "Successful replacement",
            "codex_app_server",
            None,
        )
        .await
        .expect("writer should remain reusable after rollback");
    assert_eq!(successor.summary_text, "Successful replacement");
    assert_ne!(successor.node_id, intermediate.node_id);
    assert_eq!(successor.source_refs, leaf_before.summary.source_refs);
    let replayed_successor = db
        .publish_codex_compaction_summary_successor(
            &intermediate.node_id,
            "Successful replacement",
            "codex_app_server",
            None,
        )
        .await
        .unwrap();
    assert_eq!(replayed_successor, successor);
    assert!(
        db.pending_codex_compaction_summary_requests(Some("codex-compact-rollback"), 10)
            .await
            .unwrap()
            .is_empty()
    );
    let predecessor_after_success = db
        .lcm_expand_summary_node("codex", "codex-compact-rollback", &original_node_id)
        .await
        .unwrap();
    let leaf_after_success = db
        .lcm_expand_summary_node("codex", "codex-compact-rollback", &intermediate.node_id)
        .await
        .unwrap();
    assert_eq!(predecessor_after_success, predecessor_before);
    assert_eq!(leaf_after_success, leaf_before);
}
