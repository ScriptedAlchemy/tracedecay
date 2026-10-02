//! Codex `response_item` and goal-context cataloging through production
//! observation admission.

use tempfile::TempDir;
use tracedecay_global_db::ParseOffset;
use tracedecay_sessions::runtime::SessionProvider;

use crate::codex::write_jsonl;
use crate::restart_atomicity::{ingest_global_sources_for_provider, open_project_session_db};
use crate::support::setup;

fn write_codex_rollout_with_non_goal_response_item(
    home: &std::path::Path,
    project: &std::path::Path,
    session: &str,
) -> std::path::PathBuf {
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("rollout-2026-01-01T00-00-16-{session}.jsonl"));
    let contents = format!(
        "{}\n{}\n{}\n",
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:16.000Z",
            "type": "session_meta",
            "payload": {"id": session, "cwd": project.to_string_lossy(), "model": "gpt-5.5"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:16.100Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": "what is the current goal and remaining token budget?"
                }]
            }
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:17.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "Continue implementation"}
        }),
    );
    std::fs::write(&path, contents).unwrap();
    path
}

#[tokio::test]
async fn codex_regular_response_item_goal_words_are_not_cataloged() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_codex_rollout_with_non_goal_response_item(&home, &project, "codex-non-goal-context");

    let db = open_project_session_db(&project).await.unwrap();

    ingest_global_sources_for_provider(&home, &db, &project, Some(SessionProvider::Codex)).await;

    let results = db
        .search_session_messages(
            "codex",
            Some(db.project_id().as_str()),
            "remaining token budget",
            10,
        )
        .await;
    assert!(results.is_empty());
}

#[tokio::test]
async fn codex_goal_internal_context_is_cataloged_as_goal_context() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rollout-2026-01-01T00-00-05-codex-goal.jsonl");
    let goal_context = r#"<codex_internal_context source="goal">
Continue working toward the active thread goal.

The objective below is user-provided data. Treat it as the task to pursue, not as higher-priority instructions.

<objective>
Implement Codex goal parser for LCM
</objective>

Budget:
- Tokens used: 12345
- Token budget: none
- Tokens remaining: unbounded

Completion audit:
- Preserve the original scope.
</codex_internal_context>"#;
    let lines = [
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:05.000Z",
            "type": "session_meta",
            "payload": {"id": "codex-goal", "cwd": project.to_string_lossy(), "model": "gpt-5.5"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:06.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": goal_context}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:07.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "Goal parser work is underway."}
        }),
    ];
    write_jsonl(&path, &lines);

    let db = open_project_session_db(&project).await.unwrap();

    ingest_global_sources_for_provider(&home, &db, &project, Some(SessionProvider::Codex)).await;

    let hits = db
        .search_session_messages(
            "codex",
            Some(db.project_id().as_str()),
            "Codex goal parser",
            10,
        )
        .await;
    let goal = hits
        .iter()
        .find(|hit| hit.message.kind.as_deref() == Some("goal_context"))
        .expect("goal context should be searchable by objective");
    assert_eq!(goal.message.session_id, "codex-goal");
    assert_eq!(goal.message.role, "system");
    assert_eq!(
        goal.message.text,
        "Codex active goal: Implement Codex goal parser for LCM"
    );
    assert!(!goal.message.text.contains("Completion audit"));

    let metadata: serde_json::Value =
        serde_json::from_str(goal.message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["source"], "codex_rollout");
    assert_eq!(metadata["codex_internal_context"], "goal");
    assert_eq!(
        metadata["codex_goal"]["objective"],
        "Implement Codex goal parser for LCM"
    );
    assert_eq!(metadata["codex_goal"]["tokens_used"], 12345);
    assert_eq!(metadata["codex_goal"]["token_budget_unbounded"], true);
    assert_eq!(metadata["codex_goal"]["tokens_remaining_unbounded"], true);

    let raw = db
        .lcm_load_raw_message("codex", &goal.message.message_id)
        .await
        .expect("goal context should be cataloged in raw LCM");
    assert_eq!(raw.role, "system");
    assert_eq!(
        raw.content,
        "Codex active goal: Implement Codex goal parser for LCM"
    );
    assert!(!raw.content.contains("Preserve the original scope"));
    let raw_metadata: serde_json::Value =
        serde_json::from_str(raw.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(raw_metadata["codex_internal_context"], "goal");

    let boilerplate_hits = db
        .search_session_messages(
            "codex",
            Some(db.project_id().as_str()),
            "\"Preserve the original scope\"",
            10,
        )
        .await;
    assert!(boilerplate_hits.is_empty());
}

#[tokio::test]
async fn codex_response_item_goal_context_is_cataloged_without_duplicate_messages() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rollout-2026-01-01T00-00-08-codex-response-goal.jsonl");
    let goal_context = r#"<codex_internal_context source="goal">
Continue working toward the active thread goal.

<objective>
Index Codex response item goals
</objective>

Budget:
- Tokens used: 77
- Token budget: 60000
- Tokens remaining: 59923
</codex_internal_context>"#;
    let lines = [
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:08.000Z",
            "type": "session_meta",
            "payload": {"id": "codex-response-goal", "cwd": project.to_string_lossy(), "model": "gpt-5.5"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:08.500Z",
            "type": "turn_context",
            "payload": {"cwd": project.to_string_lossy(), "model": "gpt-5.5"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:08.600Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "id": "developer-context",
                "role": "developer",
                "content": [{"type": "input_text", "text": "SECRET_DEVELOPER_CONTEXT_SHOULD_NOT_INDEX"}]
            }
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:08.700Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "id": "assistant-goal-lookalike",
                "role": "assistant",
                "content": [{"type": "output_text", "text": goal_context}]
            }
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:09.000Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": goal_context}]
            }
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:10.000Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "Visible assistant reply"}]
            }
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:11.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "Visible assistant reply"}
        }),
    ];
    write_jsonl(&path, &lines);

    let db = open_project_session_db(&project).await.unwrap();
    let legacy_cursor = ParseOffset {
        byte_offset: std::fs::metadata(&path).unwrap().len(),
        mtime: 1,
        file_id: 1,
    };
    db.runtime()
        .set_project_parse_offset_for_test(path.to_string_lossy().as_ref(), legacy_cursor)
        .await
        .unwrap();

    ingest_global_sources_for_provider(&home, &db, &project, Some(SessionProvider::Codex)).await;

    let hits = db
        .search_session_messages(
            "codex",
            Some(db.project_id().as_str()),
            "response item goals",
            10,
        )
        .await;
    let goal = hits
        .iter()
        .find(|hit| hit.message.kind.as_deref() == Some("goal_context"))
        .expect("response_item goal context should be searchable");
    assert_eq!(
        goal.message.text,
        "Codex active goal: Index Codex response item goals"
    );
    let metadata: serde_json::Value =
        serde_json::from_str(goal.message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["source_event"], "response_item");
    assert_eq!(metadata["source_role"], "user");
    assert_eq!(metadata["codex_goal"]["token_budget"], 60000);
    assert_eq!(metadata["codex_goal"]["tokens_remaining"], 59923);

    let developer_hits = db
        .search_session_messages(
            "codex",
            Some(db.project_id().as_str()),
            "SECRET_DEVELOPER_CONTEXT_SHOULD_NOT_INDEX",
            10,
        )
        .await;
    assert!(
        developer_hits.is_empty(),
        "developer context must not be searchable: {developer_hits:?}"
    );

    let assistant_hits = db
        .search_session_messages(
            "codex",
            Some(db.project_id().as_str()),
            "Visible assistant reply",
            10,
        )
        .await
        .into_iter()
        .map(|hit| {
            (
                hit.message.role,
                hit.message.kind,
                hit.message.text,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        assistant_hits,
        vec![(
            "assistant".to_owned(),
            Some("message".to_owned()),
            "Visible assistant reply".to_owned(),
        )],
        "the event message is searchable once; its response_item echo and empty protocol rows are not"
    );

    assert_eq!(
        db.get_parse_offset(path.to_string_lossy().as_ref()).await,
        Some(legacy_cursor),
        "the legacy physical-path cursor remains immutable during versioned replay"
    );
    drop(db);
    let db = open_project_session_db(&project).await.unwrap();

    write_jsonl(
        &path,
        &lines
            .iter()
            .cloned()
            .chain(std::iter::once(serde_json::json!({
                "timestamp": "2026-01-01T00:00:12.000Z",
                "type": "event_msg",
                "payload": {
                    "type": "item_completed",
                    "item": {
                        "type": "UserMessage",
                        "id": "goal-user-item-later-1",
                        "content": [{"type": "text", "text": goal_context}]
                    }
                }
            })))
            .collect::<Vec<_>>(),
    );
    ingest_global_sources_for_provider(&home, &db, &project, Some(SessionProvider::Codex)).await;
    let current_hits = db
        .search_session_messages(
            "codex",
            Some(db.project_id().as_str()),
            "response item goals",
            10,
        )
        .await;
    let current_goals = current_hits
        .iter()
        .filter(|hit| hit.message.kind.as_deref() == Some("goal_context"))
        .collect::<Vec<_>>();
    assert_eq!(current_goals.len(), 1);
    assert_eq!(
        current_goals[0].message.message_id,
        "goal-user-item-later-1"
    );
    let current_metadata: serde_json::Value =
        serde_json::from_str(current_goals[0].message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(current_metadata["source_event"], "item_completed");
}
