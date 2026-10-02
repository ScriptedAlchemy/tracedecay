//! An in-place rewrite of a host transcript replaces what it offered: the
//! edited record's old text and a deleted record leave every read path, for
//! records with and without a native item id.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;

use super::session_search_test::{composed_transcript_home, recovered_owner_payload};
use crate::support::*;
use crate::{common, fixture};

const SESSION_ID: &str = "0199aaaa-bbbb-7ccc-8ddd-eeeeffff2947";
const RECORDS: usize = 16;
const EDITED: usize = 9;
const DELETED: usize = 12;
const EDITED_TEXT: &str = "editedmidomega question about the ledger";

#[derive(Clone, Copy)]
enum UserRecordShape {
    /// `event_msg`/`user_message`: no native id, so identity follows content.
    ContentAddressed,
    /// `event_msg`/`item_completed` carrying a `UserMessage` `item.id`, so the
    /// edit keeps its message id.
    NativeItemId,
}

fn record_token(index: usize) -> String {
    format!("tidepool{index:03}")
}

fn record_text(index: usize) -> String {
    format!("{} question about the ledger", record_token(index))
}

fn rollout_records(project: &Path, shape: UserRecordShape) -> Vec<Value> {
    let mut records = vec![json!({
        "timestamp": "2026-10-02T10:00:00.000Z",
        "type": "session_meta",
        "payload": {"id": SESSION_ID, "cwd": project, "model_provider": "openai"},
    })];
    for index in 1..=RECORDS {
        let timestamp = format!("2026-10-02T10:00:{:02}.{index:03}Z", index % 60);
        let text = record_text(index);
        records.push(if index % 2 == 1 {
            match shape {
                UserRecordShape::ContentAddressed => json!({
                    "timestamp": timestamp,
                    "type": "event_msg",
                    "payload": {"type": "user_message", "message": text},
                }),
                UserRecordShape::NativeItemId => json!({
                    "timestamp": timestamp,
                    "type": "event_msg",
                    "payload": {"type": "item_completed", "item": {
                        "type": "UserMessage",
                        "id": format!("user-item-{index:04}"),
                        "content": [{"type": "text", "text": text}],
                    }},
                }),
            }
        } else {
            json!({
                "timestamp": timestamp,
                "type": "event_msg",
                "payload": {"type": "agent_message", "message": text},
            })
        });
    }
    records
}

/// One tool answer, with a budget-truncated body recovered whole.
async fn call_production_tool(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    tool: &str,
    arguments: Value,
) -> Value {
    let response = harness
        .call_tool(project, tool, arguments)
        .await
        .unwrap_or_else(|error| panic!("{tool} invocation failed: {error}"));
    let result = response
        .result
        .unwrap_or_else(|| panic!("{tool} returned transport error: {:?}", response.error));
    assert_ne!(
        result["isError"], true,
        "{tool} returned an error: {result}"
    );
    let body = recovered_owner_payload(harness, project, result).await;
    body.pointer("/outcome/value/payload")
        .cloned()
        .unwrap_or(body)
}

/// Whether a stored message carries `text`; a native `UserMessage` stores
/// its content parts as JSON.
fn carries(message: &Value, text: &str) -> bool {
    message["content"]
        .as_str()
        .is_some_and(|content| content.contains(text))
}

fn write_rollout(path: &Path, records: &[Value]) {
    let body = records
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(path, format!("{body}\n")).expect("write Codex rollout");
}

async fn ingest(harness: &ProductionProjectCompositionHarnessV1, project: &Path) {
    let ingest = call_production_tool(
        harness,
        project,
        "tracedecay_hook_runtime",
        json!({"action": "ingest_transcript", "provider": "codex", "user_scope": false, "format": "json"}),
    )
    .await;
    assert_eq!(ingest["completed"], true, "{ingest}");
}

async fn grep_count(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    query: &str,
) -> u64 {
    let grep = call_production_tool(
        harness,
        project,
        "tracedecay_lcm_grep",
        json!({"query": query, "provider": "codex", "limit": 5, "format": "json"}),
    )
    .await;
    grep["count"]
        .as_u64()
        .unwrap_or_else(|| panic!("lcm_grep count for {query}: {grep}"))
}

/// Message search over the session-temporal store, once it is converged.
async fn search_texts(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    query: &str,
) -> Vec<String> {
    loop {
        let search = call_production_tool(
            harness,
            project,
            "tracedecay_message_search",
            json!({"query": query, "provider": "codex", "limit": 5, "format": "json"}),
        )
        .await;
        if matches!(search["outcome"].as_str(), Some("partial" | "stale")) {
            tokio::task::yield_now().await;
            continue;
        }
        return search["results"]
            .as_array()
            .unwrap_or_else(|| panic!("message_search results for {query}: {search}"))
            .iter()
            .filter_map(|result| result["message"]["text"].as_str().map(str::to_owned))
            .filter(|text| text.contains(query))
            .collect();
    }
}

async fn session_messages(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
) -> Vec<Value> {
    let loaded = call_production_tool(
        harness,
        project,
        "tracedecay_lcm_load_session",
        json!({"provider": "codex", "session_id": SESSION_ID, "limit": 100, "format": "json"}),
    )
    .await;
    loaded["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("lcm_load_session messages: {loaded}"))
        .clone()
}

fn current_branch(project: &Path) -> String {
    let output = Command::new(common::git_program())
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(project)
        .output()
        .expect("git rev-parse");
    assert!(output.status.success(), "git rev-parse must succeed");
    String::from_utf8(output.stdout)
        .expect("branch name is UTF-8")
        .trim()
        .to_owned()
}

async fn session_event_count(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    branch: &str,
) -> Option<u64> {
    let correlated = call_production_tool(
        harness,
        project,
        "tracedecay_sessions_for",
        json!({"git_ref": "branch", "value": branch, "format": "json"}),
    )
    .await;
    let hits = correlated["results"]
        .as_array()
        .unwrap_or_else(|| panic!("sessions_for answer: {correlated}"))
        .iter()
        .filter(|hit| hit["session_id"] == SESSION_ID)
        .collect::<Vec<_>>();
    assert!(
        hits.len() <= 1,
        "sessions_for repeated the session: {correlated}"
    );
    hits.first().map(|hit| {
        hit["event_count"]
            .as_u64()
            .unwrap_or_else(|| panic!("sessions_for event_count: {hit}"))
    })
}

async fn in_place_edit_supersedes_every_read_path(shape: UserRecordShape) {
    let root = test_temp_dir();
    let isolation = root.path().join("composition");
    let transcripts = composed_transcript_home(&isolation);
    let project = isolation.join("project");
    std::fs::create_dir_all(&project).expect("production composition project");
    fixture::write_indexed_fixture_sources(&project);
    commit_worktree(&project, "production Codex transcript fixture");
    let sessions = transcripts.join(".codex/sessions/2026/10/02");
    std::fs::create_dir_all(&sessions).expect("Codex sessions directory");
    let rollout = sessions.join(format!("rollout-2026-10-02T10-00-00-{SESSION_ID}.jsonl"));
    let mut records = rollout_records(&project, shape);
    write_rollout(&rollout, &records);

    let harness = ProductionProjectCompositionHarnessV1::open_for_session_retrieval(
        &isolation,
        [project.clone()],
    )
    .await
    .expect("production composition harness");
    ingest(&harness, &project).await;
    let last = record_token(RECORDS);
    let cold = tokio::time::timeout(Duration::from_secs(60), async {
        while grep_count(&harness, &project, &last).await == 0
            || session_messages(&harness, &project).await.len() != RECORDS
        {
            tokio::task::yield_now().await;
        }
    })
    .await;
    if cold.is_err() {
        let messages = session_messages(&harness, &project).await;
        panic!(
            "cold rollout never converged: grep {last}={} messages={:?}",
            grep_count(&harness, &project, &last).await,
            messages
                .iter()
                .map(|message| (message["role"].clone(), message["content"].clone()))
                .collect::<Vec<_>>()
        );
    }
    let branch = current_branch(&project);
    let cold_events = session_event_count(&harness, &project, &branch).await;

    let edited = &mut records[EDITED];
    let replaced = edited
        .to_string()
        .replace(&record_text(EDITED), EDITED_TEXT);
    *edited = serde_json::from_str(&replaced).expect("edited record");
    records.remove(DELETED);
    write_rollout(&rollout, &records);
    ingest(&harness, &project).await;

    let expected = RECORDS - 1;
    let messages = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let messages = session_messages(&harness, &project).await;
            let edited_current = messages.iter().any(|message| carries(message, EDITED_TEXT));
            if edited_current && messages.len() == expected {
                break messages;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the rewrite became the session's current content");
    for retired in [record_token(EDITED), record_token(DELETED)] {
        assert!(
            !messages.iter().any(|message| carries(message, &retired)),
            "lcm_load_session still serves {retired}"
        );
        assert_eq!(
            grep_count(&harness, &project, &retired).await,
            0,
            "lcm_grep still finds {retired}"
        );
        let found = search_texts(&harness, &project, &retired).await;
        assert!(
            found.is_empty(),
            "message_search still finds {retired}: {found:?}"
        );
    }
    assert_eq!(grep_count(&harness, &project, "editedmidomega").await, 1);
    let found = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let found = search_texts(&harness, &project, "editedmidomega").await;
            if !found.is_empty() {
                break found;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("message_search found the edit");
    assert_eq!(
        found.len(),
        1,
        "message_search duplicated the edit: {found:?}"
    );

    let message_id = messages
        .iter()
        .find(|message| carries(message, EDITED_TEXT))
        .and_then(|message| message["message_id"].as_str())
        .expect("edited message identity");
    let expanded = call_production_tool(
        &harness,
        &project,
        "tracedecay_lcm_expand",
        json!({
            "provider": "codex",
            "session_id": SESSION_ID,
            "target": {"kind": "canonical_occurrence", "message_id": message_id},
            "format": "json"
        }),
    )
    .await;
    assert!(
        carries(&expanded["expansion"], EDITED_TEXT),
        "lcm_expand did not serve the edit: {expanded}"
    );

    // The rewrite replaced records, not the session's git activity.
    assert!(
        cold_events.is_some(),
        "sessions_for never correlated the session"
    );
    assert_eq!(
        session_event_count(&harness, &project, &branch).await,
        cold_events,
        "the rewrite changed the session's branch correlation"
    );

    // Dropping the final record adds no observation, so the rebuild it forces
    // has the frontiers of the edit's rebuild; it must still run, not join
    // the one the new reset discarded.
    records.pop();
    write_rollout(&rollout, &records);
    ingest(&harness, &project).await;
    let truncated = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let messages = session_messages(&harness, &project).await;
            if messages.len() == expected - 1 {
                break messages;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the truncated rollout became the session's current content");
    assert!(
        !truncated.iter().any(|message| carries(message, &last)),
        "lcm_load_session still serves the dropped final record"
    );
    assert_eq!(grep_count(&harness, &project, &last).await, 0);
    assert_eq!(grep_count(&harness, &project, &record_token(1)).await, 1);
    assert_eq!(grep_count(&harness, &project, "editedmidomega").await, 1);
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_place_edit_of_a_content_addressed_record_supersedes_every_read_path() {
    in_place_edit_supersedes_every_read_path(UserRecordShape::ContentAddressed).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_place_edit_of_a_native_id_record_supersedes_every_read_path() {
    in_place_edit_supersedes_every_read_path(UserRecordShape::NativeItemId).await;
}
