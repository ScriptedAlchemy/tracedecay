use super::capture::*;
#[cfg(windows)]
use super::ingest::snapshot_generation;
use super::ingest::{
    COMPOSER_RETRY_KEY_PREFIX, ComposerIngestContext, complete_composer_retry, composer_retry_key,
    composer_retry_key_prefix, ensure_composer_retry,
};
#[cfg(unix)]
use super::ingest::{directory_entry_is_real_dir, path_is_regular_file_no_follow};
use super::sqlite::*;
use super::store::*;
use super::*;

use serde_json::{Value, json};
use tracedecay_capture::cursor_composer::normalize_cursor_composer_envelope_observation;
use tracedecay_domain::{CanonicalObservationFactV1, CanonicalWorkflowSemanticKindV1};
use tracedecay_domain::{ObservationScopeV1, ObservationSourceGenerationV1, ProjectId};

use crate::admission::HostAdmission;
use crate::admission::test_support::MemoryHostAdmission;
use crate::observation::ObservationCancellation;
use crate::runtime::ingest_byte_budget::IngestByteBudget;
mod projection;

/// A failed durable message-id lookup is store *unavailability*, not proof the
/// bubble is already durable. The sweep must defer it and ingest it on a later
/// pass instead of skipping it — and it must not let a later header in the
/// same composer advance the source cursor past the unverified bubble, which
/// is what made the old `unwrap_or(true)` a permanent silent drop.
#[tokio::test]
async fn store_error_during_message_lookup_defers_instead_of_dropping_the_bubble() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let state_dir = home
        .path()
        .join(".config")
        .join("Cursor")
        .join("User")
        .join("globalStorage");
    std::fs::create_dir_all(&state_dir).unwrap();
    let connection = rusqlite::Connection::open(state_dir.join("state.vscdb")).unwrap();
    connection
        .execute_batch(
            "PRAGMA journal_mode=DELETE;
             CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO cursorDiskKV(key, value) VALUES (?1, ?2)",
            rusqlite::params![
                "composerData:comp-retry",
                json!({
                    "composerId": "comp-retry",
                    "workspaceIdentifier": {
                        "uri": { "fsPath": project.path().to_string_lossy() }
                    },
                    "fullConversationHeadersOnly": [
                        { "bubbleId": "b1" },
                        { "bubbleId": "b2" }
                    ]
                })
                .to_string()
            ],
        )
        .unwrap();
    for (bubble, text) in [("b1", "first bubble"), ("b2", "second bubble")] {
        connection
            .execute(
                "INSERT INTO cursorDiskKV(key, value) VALUES (?1, ?2)",
                rusqlite::params![
                    format!("bubbleId:comp-retry:{bubble}"),
                    json!({ "type": 1, "text": text }).to_string()
                ],
            )
            .unwrap();
    }
    drop(connection);

    let project_id = tracedecay_domain::ProjectId::new("project.cursor-composer-retry").unwrap();
    let admission = MemoryHostAdmission::default();
    let source = CursorComposerSource::with_home(home.path());

    // The very first lookup (bubble `b1`) sees a saturated/unavailable store.
    admission.fail_next_session_message_lookups(1);
    let deferred = source
        .ingest_capped(
            &admission,
            project.path(),
            project_id.clone(),
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
        )
        .await
        .expect("composer sweep after a temporary store failure");

    assert_eq!(
        deferred.messages_upserted, 0,
        "a store error must stop the composer before any later header advances its cursor"
    );
    assert!(
        deferred.deferred_by_byte_cap,
        "the pass must report itself incomplete so catch-up runs again"
    );

    // Catch-up pass with a healthy store.
    let recovered = source
        .ingest_capped(
            &admission,
            project.path(),
            project_id,
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
        )
        .await
        .expect("composer recovery sweep");
    assert!(
        recovered.messages_upserted >= 2,
        "both bubbles must reach the store once it recovers, got {}",
        recovered.messages_upserted
    );

    let payloads = admission
        .observations()
        .iter()
        .map(|stored| stored.observation().payload().to_string())
        .collect::<Vec<_>>();
    for bubble in ["comp-retry:b1", "comp-retry:b2"] {
        assert!(
            payloads.iter().any(|payload| payload.contains(bubble)),
            "{bubble} was dropped instead of retried"
        );
    }
}

/// Replacing the bounded durable-id batch with one
/// `has_session_message` call per header makes this second sweep perform 64
/// scalar store reads.
#[tokio::test]
async fn covered_composer_headers_use_batched_durable_identity_lookup() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let state_dir = home
        .path()
        .join(".config")
        .join("Cursor")
        .join("User")
        .join("globalStorage");
    std::fs::create_dir_all(&state_dir).unwrap();
    let connection = rusqlite::Connection::open(state_dir.join("state.vscdb")).unwrap();
    connection
        .execute_batch(
            "PRAGMA journal_mode=DELETE;
             CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);",
        )
        .unwrap();
    let headers = (0..64)
        .map(|index| json!({ "bubbleId": format!("bubble-{index:03}") }))
        .collect::<Vec<_>>();
    connection
        .execute(
            "INSERT INTO cursorDiskKV(key, value) VALUES (?1, ?2)",
            rusqlite::params![
                "composerData:comp-batch",
                json!({
                    "composerId": "comp-batch",
                    "workspaceIdentifier": {
                        "uri": { "fsPath": project.path().to_string_lossy() }
                    },
                    "fullConversationHeadersOnly": headers
                })
                .to_string()
            ],
        )
        .unwrap();
    for index in 0..64 {
        connection
            .execute(
                "INSERT INTO cursorDiskKV(key, value) VALUES (?1, ?2)",
                rusqlite::params![
                    format!("bubbleId:comp-batch:bubble-{index:03}"),
                    json!({ "type": 1, "text": format!("message {index}") }).to_string()
                ],
            )
            .unwrap();
    }
    drop(connection);

    let admission = MemoryHostAdmission::default();
    let project_id = ProjectId::new("project.cursor-composer-batch").unwrap();
    let source = CursorComposerSource::with_home(home.path());
    source
        .ingest_capped(
            &admission,
            project.path(),
            project_id.clone(),
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
        )
        .await
        .unwrap();
    let scalar_reads_before = admission.session_message_read_count();
    source
        .ingest_capped(
            &admission,
            project.path(),
            project_id,
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
        )
        .await
        .unwrap();
    let scalar_reads_after = admission.session_message_read_count();

    assert_eq!(
        scalar_reads_after, scalar_reads_before,
        "one bounded durable-id batch must replace per-header scalar reads"
    );
}

#[cfg(windows)]
#[test]
fn windows_snapshot_generation_is_stable_across_appends() {
    use std::io::Write as _;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.vscdb");
    std::fs::write(&path, b"before").unwrap();
    let before = snapshot_generation(&path).expect("Windows file identity");
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"after")
        .unwrap();

    assert_eq!(snapshot_generation(&path), Some(before));
}

#[test]
fn composer_capture_request_uses_snapshot_order_and_native_bubble_identity() {
    let bubble = json!({
        "type": 2,
        "text": "redacted fixture",
    });
    let request = build_cursor_composer_capture_request(
        "composer-redacted",
        "bubble-redacted",
        &bubble,
        ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(1).unwrap(),
        7,
        None,
    );
    assert!(request.is_ok());
    assert_eq!(
        cursor_composer_native_record_id("composer-redacted", "bubble-redacted")
            .unwrap()
            .as_str(),
        cursor_composer_native_record_id("composer-redacted", "bubble-redacted")
            .unwrap()
            .as_str()
    );
}

#[test]
fn canonical_composer_bubble_is_snapshot_typed_and_redacted() {
    let native = json!({
        "type": 2,
        "text": "redacted response",
        "createdAt": 1_783_500_600_000_i64,
        "workspaceIdentifier": {"uri": {"fsPath": "/secret/workspace"}},
        "toolFormerData": {
            "name": "Read",
            "toolCallId": "tool-redacted",
            "params": {"path": "/secret/workspace/file.rs", "token": "credential-redacted"},
            "result": {"body": "secret result"},
            "status": "completed"
        },
        "thinking": {"text": "provider-visible summary"},
        "tokenCount": {"inputTokens": 11, "outputTokens": 7},
        "commits": [{"sha": "abc123"}],
        "pullRequests": [{"url": "https://example.invalid/pr/1"}],
        "todos": [{"content": "redacted plan item"}]
    });
    let range = tracedecay_domain::ObservationSourceRangeV1::new(7, 8).unwrap();
    let record_id =
        cursor_composer_native_record_id("composer-redacted", "bubble-redacted").unwrap();
    let envelope = normalize_cursor_composer_observation(
        &native,
        "composer-redacted",
        record_id.clone(),
        range,
        7,
    )
    .unwrap();
    let rendered = format!("{envelope:?}");
    for fact in [
        "Message",
        "ToolInvocation",
        "ToolResult",
        "Reasoning",
        "Usage",
        "Git",
        "Workflow",
    ] {
        assert!(rendered.contains(fact), "missing canonical fact {fact}");
    }
    assert!(!rendered.contains("TodoList") && !rendered.contains("todo_list"));
    assert!(rendered.contains("SnapshotOrder"));
    assert!(rendered.contains(record_id.as_str()));
    assert!(!rendered.contains("/secret/workspace"));
    assert!(!rendered.contains("credential-redacted"));
    assert!(!rendered.contains("secret result"));
    let relations = serde_json::to_value(envelope.relations()).unwrap();
    assert_eq!(relations["thread_id"], "composer-redacted");
    assert_eq!(relations["message_id"], record_id.as_str());
    assert!(relations.get("turn_id").is_none());
    assert!(relations.get("agent_id").is_none());
    assert!(relations.get("parent_agent_id").is_none());
}

#[test]
fn composer_bubble_without_turn_field_leaves_turn_unset() {
    let native = json!({
        "bubbleId": "bubble-1",
        "type": 1,
        "text": "hello from composer"
    });
    let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 1).unwrap();
    let record_id = cursor_composer_native_record_id("composer-native", "bubble-1").unwrap();
    let envelope = normalize_cursor_composer_observation(
        &native,
        "composer-native",
        record_id.clone(),
        range,
        0,
    )
    .unwrap();
    let relations = serde_json::to_value(envelope.relations()).unwrap();
    assert_eq!(relations["session_id"], "composer-native");
    assert_eq!(relations["thread_id"], "composer-native");
    assert_eq!(relations["message_id"], record_id.as_str());
    assert!(relations.get("turn_id").is_none());
    assert!(relations.get("agent_id").is_none());
    assert!(relations.get("parent_agent_id").is_none());
}

/// Exact assistant bubble fields from
/// `tests/transcript_ingest_suite/cursor_composer.rs`
/// (`composer_envelope_and_bubbles_ingest_rows`). Provider-parser evidence is
/// the Cursor composer `bubbleId` payload (`type`/`text`/`toolFormerData`/
/// `thinking`/`tokenCount`); expected output is the canonical envelope with
/// Cursor provider + bubble-id native provenance.
#[test]
fn fixture_backed_composer_assistant_bubble_reaches_canonical_envelope() {
    let native: Value = serde_json::from_str(include_str!(
        "../../../../../../tests/fixtures/provider_normalization/cursor_composer/assistant_bubble.input.json"
    ))
    .expect("Cursor composer golden input");
    let expected: Value = serde_json::from_str(include_str!(
        "../../../../../../tests/fixtures/provider_normalization/cursor_composer/assistant_bubble.expected_envelope.json"
    ))
    .expect("Cursor composer golden expected envelope");
    let range = tracedecay_domain::ObservationSourceRangeV1::new(1, 2).unwrap();
    let record_id = cursor_composer_native_record_id("comp-1", "b-asst").unwrap();
    let envelope =
        normalize_cursor_composer_observation(&native, "comp-1", record_id.clone(), range, 1)
            .unwrap();
    assert_eq!(
        envelope.provider().as_str(),
        expected["provider"].as_str().unwrap()
    );
    assert_eq!(
        envelope.native_record_kind(),
        expected["native_record_kind"].as_str().unwrap()
    );
    assert_eq!(envelope.stable_record_id().as_str(), record_id.as_str());
    let actual = serde_json::to_value(&envelope).unwrap();
    assert_eq!(actual["version"], expected["version"]);
    assert_eq!(actual["evidence"], expected["evidence"]);
    let fact_kinds = actual["facts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|fact| fact["kind"].as_str().unwrap())
        .collect::<Vec<_>>();
    let expected_fact_kinds = expected["fact_kinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|kind| kind.as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(fact_kinds, expected_fact_kinds);
    let relations = actual["relations"].as_object().unwrap();
    assert_eq!(relations["session_id"], expected["relations"]["session_id"]);
    assert_eq!(relations["thread_id"], expected["relations"]["thread_id"]);
    assert_eq!(relations["message_id"], record_id.as_str());
    for absent in expected["relations"]["absent"].as_array().unwrap() {
        assert!(relations.get(absent.as_str().unwrap()).is_none());
    }
    let rendered = actual.to_string();
    for required in expected["encoded_must_contain"].as_array().unwrap() {
        assert!(rendered.contains(required.as_str().unwrap()));
    }
    for rejected in expected["encoded_must_not_contain"].as_array().unwrap() {
        assert!(!rendered.contains(rejected.as_str().unwrap()));
    }
}

/// Checked-in `composerData` envelope `todos[{id,content,status}]` map to
/// `WorkflowLifecycle` `TodoList` + `TodoItem` facts with native order and refs.
#[test]
fn fixture_backed_composer_envelope_todos_reach_workflow_lifecycle() {
    let native: Value = serde_json::from_str(include_str!(
        "../../../../../../tests/fixtures/provider_normalization/cursor_composer/envelope_todos.input.json"
    ))
    .expect("Cursor composer envelope todos golden input");
    let expected: Value = serde_json::from_str(include_str!(
        "../../../../../../tests/fixtures/provider_normalization/cursor_composer/envelope_todos.expected_envelope.json"
    ))
    .expect("Cursor composer envelope todos expected envelope");
    let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 1).unwrap();
    // Fixture lastUpdatedAt is null — checkpoint is the content fingerprint.
    assert!(native.get("lastUpdatedAt").is_some_and(Value::is_null));
    let checkpoint = composer_envelope_todo_checkpoint(&native)
        .expect("fixture todos must yield a content fingerprint checkpoint");
    let record_id = cursor_composer_envelope_native_record_id("comp-1", checkpoint).unwrap();
    let envelope = normalize_cursor_composer_envelope_observation(
        &native,
        "comp-1",
        None,
        record_id.clone(),
        range,
        0,
    )
    .unwrap();
    assert_eq!(
        envelope.provider().as_str(),
        expected["provider"].as_str().unwrap()
    );
    assert_eq!(
        envelope.native_record_kind(),
        expected["native_record_kind"].as_str().unwrap()
    );
    assert_eq!(envelope.stable_record_id().as_str(), record_id.as_str());
    let actual = serde_json::to_value(&envelope).unwrap();
    assert_eq!(actual["version"], expected["version"]);
    assert_eq!(actual["evidence"], expected["evidence"]);
    let fact_kinds = actual["facts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|fact| fact["kind"].as_str().unwrap())
        .collect::<Vec<_>>();
    let expected_fact_kinds = expected["fact_kinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|kind| kind.as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(fact_kinds, expected_fact_kinds);
    let expected_lifecycle = expected["workflow_lifecycle"].as_array().unwrap();
    let actual_facts = actual["facts"].as_array().unwrap();
    assert_eq!(actual_facts.len(), expected_lifecycle.len());
    for (actual_fact, expected_fact) in actual_facts.iter().zip(expected_lifecycle.iter()) {
        assert_eq!(actual_fact["semantic_kind"], expected_fact["semantic_kind"]);
        assert_eq!(
            actual_fact["provider_reference"],
            expected_fact["provider_reference"]
        );
        if let Some(item_id) = expected_fact.get("item_id") {
            assert_eq!(actual_fact["item_id"], *item_id);
        }
        if let Some(list_reference) = expected_fact.get("list_reference") {
            assert_eq!(actual_fact["list_reference"], *list_reference);
        }
        if let Some(status) = expected_fact.get("status") {
            assert_eq!(actual_fact["status"], *status);
        }
        if let Some(item_order) = expected_fact.get("item_order") {
            assert_eq!(actual_fact["item_order"], *item_order);
        }
        if let Some(content) = expected_fact.get("content") {
            assert_eq!(actual_fact["content"], *content);
        }
        for absent in expected_fact["absent"].as_array().unwrap() {
            assert!(actual_fact.get(absent.as_str().unwrap()).is_none());
        }
    }
    let relations = actual["relations"].as_object().unwrap();
    assert_eq!(relations["session_id"], expected["relations"]["session_id"]);
    assert_eq!(relations["thread_id"], expected["relations"]["thread_id"]);
    for absent in expected["relations"]["absent"].as_array().unwrap() {
        assert!(relations.get(absent.as_str().unwrap()).is_none());
    }
    let rendered = actual.to_string();
    for required in expected["encoded_must_contain"].as_array().unwrap() {
        assert!(rendered.contains(required.as_str().unwrap()));
    }
    for rejected in expected["encoded_must_not_contain"].as_array().unwrap() {
        assert!(!rendered.contains(rejected.as_str().unwrap()));
    }
}

#[test]
fn envelope_todo_checkpoint_uses_fixture_backed_content_fingerprint() {
    let native: Value = serde_json::from_str(include_str!(
        "../../../../../../tests/fixtures/provider_normalization/cursor_composer/envelope_todos.input.json"
    ))
    .expect("Cursor composer envelope todos golden input");
    assert!(native.get("lastUpdatedAt").is_some_and(Value::is_null));
    let baseline = composer_envelope_todo_checkpoint(&native).unwrap();
    let mut pending_second = native.clone();
    pending_second["todos"][1]["status"] = Value::String("completed".to_string());
    let updated = composer_envelope_todo_checkpoint(&pending_second).unwrap();
    assert_ne!(
        baseline, updated,
        "pending→completed must change the content fingerprint checkpoint"
    );
    assert_ne!(
        cursor_composer_envelope_native_record_id("comp-1", baseline).unwrap(),
        cursor_composer_envelope_native_record_id("comp-1", updated).unwrap()
    );
    let mut edited = native.clone();
    edited["todos"][1]["content"] = Value::String("Second todo revised".to_string());
    assert_ne!(
        baseline,
        composer_envelope_todo_checkpoint(&edited).unwrap(),
        "content edits must change the checkpoint"
    );
    let mut reordered = native.clone();
    reordered["todos"].as_array_mut().unwrap().swap(0, 1);
    assert_ne!(
        baseline,
        composer_envelope_todo_checkpoint(&reordered).unwrap(),
        "native array-order changes must change the checkpoint"
    );
}

/// Bubble text + todos co-locate `Message` and `WorkflowLifecycle` facts.
#[test]
fn fixture_backed_composer_bubble_colocates_message_and_todo_lifecycle() {
    let native: Value = serde_json::from_str(include_str!(
        "../../../../../../tests/fixtures/provider_normalization/cursor_composer/assistant_bubble_with_todos.input.json"
    ))
    .expect("Cursor composer bubble+todos golden input");
    let expected: Value = serde_json::from_str(include_str!(
        "../../../../../../tests/fixtures/provider_normalization/cursor_composer/assistant_bubble_with_todos.expected_envelope.json"
    ))
    .expect("Cursor composer bubble+todos expected envelope");
    let range = tracedecay_domain::ObservationSourceRangeV1::new(1, 2).unwrap();
    let record_id = cursor_composer_native_record_id("comp-1", "b-todos").unwrap();
    let envelope =
        normalize_cursor_composer_observation(&native, "comp-1", record_id.clone(), range, 1)
            .unwrap();
    let actual = serde_json::to_value(&envelope).unwrap();
    let fact_kinds = actual["facts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|fact| fact["kind"].as_str().unwrap())
        .collect::<Vec<_>>();
    let expected_fact_kinds = expected["fact_kinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|kind| kind.as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(fact_kinds, expected_fact_kinds);
    assert!(
        envelope
            .facts()
            .iter()
            .any(|fact| matches!(fact, CanonicalObservationFactV1::Message { .. })),
        "message fact must remain co-located"
    );
    assert!(
        envelope.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::WorkflowLifecycle {
                semantic_kind: CanonicalWorkflowSemanticKindV1::TodoList,
                ..
            }
        )),
        "todo list fact required"
    );
    let items: Vec<_> = envelope
        .facts()
        .iter()
        .filter_map(|fact| match fact {
            CanonicalObservationFactV1::WorkflowLifecycle {
                semantic_kind: CanonicalWorkflowSemanticKindV1::TodoItem,
                item_id,
                status,
                item_order,
                content,
                list_reference,
                ..
            } => Some((item_id, status, item_order, content, list_reference)),
            _ => None,
        })
        .collect();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].0.as_deref(), Some("t1"));
    assert_eq!(items[0].1.as_deref(), Some("completed"));
    assert_eq!(*items[0].2, Some(0));
    assert_eq!(
        items[0].3.as_ref().and_then(Value::as_str),
        Some("First todo")
    );
    assert_eq!(items[0].4.as_deref(), Some("comp-1"));
    assert_eq!(items[1].0.as_deref(), Some("t2"));
    assert_eq!(items[1].1.as_deref(), Some("pending"));
    assert_eq!(*items[1].2, Some(1));
    assert_eq!(
        items[1].3.as_ref().and_then(Value::as_str),
        Some("Second todo")
    );
    assert_eq!(items[1].4.as_deref(), Some("comp-1"));
    let rendered = actual.to_string();
    for required in expected["encoded_must_contain"].as_array().unwrap() {
        assert!(rendered.contains(required.as_str().unwrap()));
    }
    assert!(!rendered.contains("\"revision\""));
}

#[test]
fn composer_todo_without_native_id_is_not_promoted() {
    let native = json!({
        "type": 2,
        "text": "Working the checklist.",
        "todos": [
            {"content": "No stable identity", "status": "pending"},
            {"id": "t2", "content": "Native identity", "status": "completed"}
        ]
    });
    let range = tracedecay_domain::ObservationSourceRangeV1::new(1, 2).unwrap();
    let record_id = cursor_composer_native_record_id("comp-1", "b-todos").unwrap();
    let envelope =
        normalize_cursor_composer_observation(&native, "comp-1", record_id, range, 1).unwrap();
    let items = envelope
        .facts()
        .iter()
        .filter_map(|fact| match fact {
            CanonicalObservationFactV1::WorkflowLifecycle {
                semantic_kind: CanonicalWorkflowSemanticKindV1::TodoItem,
                item_id,
                item_order,
                ..
            } => Some((item_id.as_deref(), *item_order)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(items, vec![(Some("t2"), Some(1))]);
}

/// Exact provider bool `isCompacted: true` remains the only compaction
/// promotion path; lookalike keys/string forms stay ignored.
#[test]
fn composer_is_compacted_true_promotes_compaction_fact() {
    let native = json!({
        "type": 2,
        "text": "post-compaction bubble",
        "isCompacted": true,
    });
    let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 1).unwrap();
    let record_id =
        cursor_composer_native_record_id("composer-compacted", "bubble-compacted").unwrap();
    let envelope =
        normalize_cursor_composer_observation(&native, "composer-compacted", record_id, range, 0)
            .unwrap();
    assert!(envelope.facts().iter().any(|fact| matches!(
        fact,
        CanonicalObservationFactV1::Compaction {
            summary: Some(Value::String(text)),
            ..
        } if text == "post-compaction bubble"
    )));
}

// Foreign-store fixtures use the same immutable SQLite reader as production.
async fn open_temp_kv_db_with_rows(rows: &[(&str, &str)]) -> (tempfile::TempDir, ReadOnlyDb) {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("state.vscdb");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=DELETE;\n\
             CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);",
        )
        .unwrap();
        for (key, value) in rows {
            conn.execute(
                "INSERT INTO cursorDiskKV(key, value) VALUES (?1, ?2)",
                rusqlite::params![*key, *value],
            )
            .unwrap();
        }
    }
    let ro = open_readonly_immutable(&path).await.expect("open readonly");
    (tmp, ro)
}

async fn open_temp_kv_db_with_sql(setup_sql: &str) -> (tempfile::TempDir, ReadOnlyDb) {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("state.vscdb");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=DELETE;\n\
             CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);",
        )
        .unwrap();
        conn.execute_batch(setup_sql).unwrap();
    }
    let ro = open_readonly_immutable(&path).await.expect("open readonly");
    (tmp, ro)
}

#[tokio::test]
async fn rusqlite_foreign_reader_is_immutable_query_only_and_no_create() {
    let tmp = tempfile::TempDir::new().unwrap();
    let missing = tmp.path().join("missing-state.vscdb");
    assert!(
        open_readonly_immutable(&missing).await.is_err(),
        "a missing Cursor database is a typed open miss, not a silent None"
    );
    assert!(
        !missing.exists(),
        "read-only foreign open must not create a missing Cursor database"
    );

    let path = tmp.path().join("state.vscdb");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=DELETE;
             CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);
             INSERT INTO cursorDiskKV(key, value) VALUES ('composerData:one', '{}');",
        )
        .unwrap();
    }
    let before = std::fs::read(&path).unwrap();
    let before_modified = path.metadata().unwrap().modified().unwrap();
    let sidecar = |suffix: &str| {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        std::path::PathBuf::from(sidecar)
    };
    let wal = sidecar("-wal");
    let shm = sidecar("-shm");
    let journal = sidecar("-journal");
    assert!(!wal.exists());
    assert!(!shm.exists());
    assert!(!journal.exists());

    let ro = open_readonly_immutable(&path)
        .await
        .expect("open foreign DB");
    let query_only = ro
        .conn
        .with(|conn| conn.pragma_query_value(None, "query_only", |row| row.get::<_, i64>(0)))
        .await
        .expect("blocking read completed")
        .expect("read query_only");
    assert_eq!(query_only, 1);
    let write = ro
        .conn
        .with(|conn| {
            conn.execute(
                "INSERT INTO cursorDiskKV(key, value) VALUES ('forbidden', 'write')",
                (),
            )
        })
        .await
        .expect("blocking write attempt completed");
    assert!(write.is_err(), "foreign reader must reject every write");
    drop(ro);

    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(
        path.metadata().unwrap().modified().unwrap(),
        before_modified
    );
    assert!(
        !wal.exists(),
        "immutable read must not create a WAL sidecar"
    );
    assert!(
        !shm.exists(),
        "immutable read must not create a SHM sidecar"
    );
    assert!(
        !journal.exists(),
        "immutable read must not create a rollback journal"
    );
}

#[cfg(unix)]
fn write_unreadable_file(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, b"present but denied").unwrap();
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o000);
    std::fs::set_permissions(path, permissions).unwrap();
}

#[cfg(unix)]
fn restore_file_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o644);
    let _ = std::fs::set_permissions(path, permissions);
}

#[cfg(unix)]
#[tokio::test]
async fn present_unreadable_composer_store_is_a_typed_open_failure() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("state.vscdb");
    write_unreadable_file(&path);
    let error = match open_readonly_immutable(&path).await {
        Ok(_) => {
            restore_file_permissions(&path);
            panic!("present unreadable store must not open as success")
        }
        Err(error) => error,
    };
    restore_file_permissions(&path);
    assert!(
        error.contains("read-only"),
        "open failure must stay typed, got {error}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn present_unreadable_state_db_defers_the_composer_sweep() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let state_dir = home
        .path()
        .join(".config")
        .join("Cursor")
        .join("User")
        .join("globalStorage");
    std::fs::create_dir_all(&state_dir).unwrap();
    let state_db = state_dir.join("state.vscdb");
    write_unreadable_file(&state_db);

    let project_id =
        tracedecay_domain::ProjectId::new("project.cursor-composer-unreadable").unwrap();
    let admission = MemoryHostAdmission::default();
    let source = CursorComposerSource::with_home(home.path());
    let outcome = source
        .ingest_capped(
            &admission,
            project.path(),
            project_id,
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
        )
        .await
        .expect("unreadable store must finish as a deferred sweep, not a silent zero-work Ok");
    restore_file_permissions(&state_db);

    assert_eq!(outcome.messages_upserted, 0);
    assert!(
        outcome.deferred_by_byte_cap,
        "a present-but-unreadable state.vscdb must defer so catch-up retries"
    );
}

#[tokio::test]
async fn rusqlite_composer_key_scan_pages_without_gaps_or_duplicates() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("state.vscdb");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "PRAGMA page_size=65536;
             VACUUM;
             PRAGMA journal_mode=DELETE;
             CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);",
        )
        .unwrap();
        let transaction = conn.unchecked_transaction().unwrap();
        {
            let mut insert = transaction
                .prepare("INSERT INTO cursorDiskKV(key, value) VALUES (?1, ?2)")
                .unwrap();
            for index in 0..(COMPOSER_KEY_SCAN_PAGE + 7) {
                insert
                    .execute(rusqlite::params![
                        format!("composerData:{index:06}"),
                        format!("value-{index}")
                    ])
                    .unwrap();
            }
            insert
                .execute(rusqlite::params!["outside-prefix", "ignored"])
                .unwrap();
            insert
                .execute(rusqlite::params![
                    "composerData:000100-null",
                    rusqlite::types::Null
                ])
                .unwrap();
        }
        transaction.commit().unwrap();
    }

    let ro = open_readonly_immutable(&path)
        .await
        .expect("open foreign DB");
    let page_size = ro
        .conn
        .with(|conn| conn.pragma_query_value(None, "page_size", |row| row.get::<_, i64>(0)))
        .await
        .expect("blocking page-size read completed")
        .expect("read page size");
    assert_eq!(page_size, 65_536);
    let first = scan_composer_keys_page(&ro.conn, None, COMPOSER_KEY_SCAN_PAGE)
        .await
        .expect("first key page");
    assert_eq!(first.len(), COMPOSER_KEY_SCAN_PAGE);
    let first_last = first.last().unwrap().0.clone();
    let second = scan_composer_keys_page(&ro.conn, Some(&first_last), COMPOSER_KEY_SCAN_PAGE)
        .await
        .expect("second key page");
    assert_eq!(second.len(), 7);

    let keys = first
        .into_iter()
        .chain(second)
        .map(|(key, _)| key)
        .collect::<Vec<_>>();
    assert_eq!(keys.len(), COMPOSER_KEY_SCAN_PAGE + 7);
    assert!(keys.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(
        keys.iter().collect::<std::collections::BTreeSet<_>>().len(),
        keys.len()
    );
}

#[test]
fn composer_discovery_and_durable_id_batch_use_indexed_bounds() {
    fn plan_details(
        connection: &rusqlite::Connection,
        sql: &str,
        parameters: impl rusqlite::Params,
    ) -> Vec<String> {
        let mut statement = connection
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap();
        statement
            .query_map(parameters, |row| row.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    let connection = rusqlite::Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);
             CREATE TABLE session_messages (
                 provider TEXT NOT NULL,
                 message_id TEXT NOT NULL,
                 PRIMARY KEY(provider, message_id)
             );",
        )
        .unwrap();

    let composer_plan = plan_details(
        &connection,
        COMPOSER_KEY_SCAN_AFTER_SQL,
        rusqlite::params!["composerData:000001", 512_i64, 1024_i64],
    );
    assert!(
        composer_plan
            .iter()
            .any(|detail| detail.contains("SEARCH cursorDiskKV") && detail.contains("key>?")),
        "composer discovery must stay an indexed key range: {composer_plan:?}"
    );

    let message_plan = plan_details(
        &connection,
        crate::runtime::store_access::EXISTING_SESSION_MESSAGE_IDS_SQL,
        rusqlite::params!["cursor", r#"["comp:b1","comp:b2"]"#],
    );
    assert!(
        message_plan.iter().any(|detail| {
            detail.contains("SEARCH messages")
                && detail.contains("provider=?")
                && detail.contains("message_id=?")
        }),
        "durable message ids must probe the canonical primary key: {message_plan:?}"
    );
    assert!(
        message_plan
            .iter()
            .all(|detail| !detail.contains("SCAN messages")),
        "durable message-id batches must not scan session_messages: {message_plan:?}"
    );
    connection
        .execute_batch(
            "INSERT INTO session_messages(provider, message_id)
                 VALUES ('cursor', 'comp:b2'), ('codex', 'comp:b1');",
        )
        .unwrap();
    let existing = connection
        .prepare(crate::runtime::store_access::EXISTING_SESSION_MESSAGE_IDS_SQL)
        .unwrap()
        .query_map(
            rusqlite::params!["cursor", r#"["comp:b1","comp:b2","comp:b3"]"#],
            |row| row.get::<_, String>(0),
        )
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    assert_eq!(existing, vec!["comp:b2"]);
}

fn insert_composer_fixture(
    connection: &rusqlite::Connection,
    composer_id: &str,
    project_path: &std::path::Path,
) {
    connection
        .execute(
            "INSERT INTO cursorDiskKV(key, value) VALUES (?1, ?2)",
            rusqlite::params![
                format!("composerData:{composer_id}"),
                json!({
                    "composerId": composer_id,
                    "workspaceIdentifier": {
                        "uri": { "fsPath": project_path.to_string_lossy() }
                    },
                    "fullConversationHeadersOnly": [
                        { "bubbleId": "bubble" }
                    ]
                })
                .to_string()
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO cursorDiskKV(key, value) VALUES (?1, ?2)",
            rusqlite::params![
                format!("bubbleId:{composer_id}:bubble"),
                json!({ "type": 1, "text": format!("message from {composer_id}") }).to_string()
            ],
        )
        .unwrap();
}

/// Removing the durable composer-key frontier makes every fresh source replay
/// the first 4,096 keys, so the tail message and a later pre-frontier insertion
/// never both become durable.
#[tokio::test]
async fn composer_key_frontier_converges_tail_then_wraps_for_pre_frontier_insert() {
    let project = tempfile::tempdir().unwrap();
    let unrelated = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let state_dir = home
        .path()
        .join(".config")
        .join("Cursor")
        .join("User")
        .join("globalStorage");
    std::fs::create_dir_all(&state_dir).unwrap();
    let state_db = state_dir.join("state.vscdb");
    let mut connection = rusqlite::Connection::open(&state_db).unwrap();
    connection
        .execute_batch(
            "PRAGMA journal_mode=DELETE;
             CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);",
        )
        .unwrap();
    let transaction = connection.transaction().unwrap();
    for index in 0..=MAX_COMPOSER_STORE_BLOB_VISITS {
        let composer_id = format!("{index:06}");
        let owner = if index == MAX_COMPOSER_STORE_BLOB_VISITS {
            project.path()
        } else {
            unrelated.path()
        };
        insert_composer_fixture(&transaction, &composer_id, owner);
    }
    transaction.commit().unwrap();

    let project_id = ProjectId::new("project.cursor-composer-frontier").unwrap();
    let admission = MemoryHostAdmission::default();
    let first = CursorComposerSource::with_home(home.path())
        .ingest_capped(&admission, project.path(), project_id.clone(), 1, None)
        .await
        .unwrap();
    assert_eq!(first.messages_upserted, 0);

    insert_composer_fixture(&connection, "000000a", project.path());
    drop(connection);

    for _ in 0..3 {
        CursorComposerSource::with_home(home.path())
            .ingest_capped(&admission, project.path(), project_id.clone(), 1, None)
            .await
            .unwrap();
    }

    let payloads = admission
        .observations()
        .iter()
        .map(|stored| stored.observation().payload().to_string())
        .collect::<Vec<_>>();
    for composer_id in ["004096", "000000a"] {
        let message_id = format!("{composer_id}:bubble");
        assert_eq!(
            payloads
                .iter()
                .filter(|payload| payload.contains(&message_id))
                .count(),
            1,
            "{message_id} must be discovered and admitted exactly once"
        );
    }
}

fn seed_large_frontier_fixture(
    state_db: &std::path::Path,
    project: &std::path::Path,
    unrelated: &std::path::Path,
    owned_indices: &[usize],
) {
    let mut connection = rusqlite::Connection::open(state_db).unwrap();
    connection
        .execute_batch(
            "PRAGMA journal_mode=DELETE;
             CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);",
        )
        .unwrap();
    let transaction = connection.transaction().unwrap();
    for index in 0..=MAX_COMPOSER_STORE_BLOB_VISITS {
        let composer_id = format!("{index:06}");
        let owner = if owned_indices.contains(&index) {
            project
        } else {
            unrelated
        };
        insert_composer_fixture(&transaction, &composer_id, owner);
    }
    transaction.commit().unwrap();
}

fn append_frontier_tail(state_db: &std::path::Path, unrelated: &std::path::Path) {
    let connection = rusqlite::Connection::open(state_db).unwrap();
    insert_composer_fixture(&connection, "999999", unrelated);
}

fn observation_count(admission: &MemoryHostAdmission, message_id: &str) -> usize {
    admission
        .observations()
        .iter()
        .filter(|stored| {
            stored
                .observation()
                .payload()
                .to_string()
                .contains(message_id)
        })
        .count()
}

#[tokio::test]
async fn composer_frontier_retries_early_lookup_failure_before_growing_tail() {
    let project = tempfile::tempdir().unwrap();
    let unrelated = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let state_dir = home
        .path()
        .join(".config")
        .join("Cursor")
        .join("User")
        .join("globalStorage");
    std::fs::create_dir_all(&state_dir).unwrap();
    let state_db = state_dir.join("state.vscdb");
    seed_large_frontier_fixture(&state_db, project.path(), unrelated.path(), &[0]);

    let admission = MemoryHostAdmission::default();
    let project_id = ProjectId::new("project.cursor-composer-lookup-retry").unwrap();
    admission.fail_next_session_message_lookups(1);
    CursorComposerSource::with_home(home.path())
        .ingest_capped(
            &admission,
            project.path(),
            project_id.clone(),
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
        )
        .await
        .unwrap();
    assert_eq!(observation_count(&admission, "000000:bubble"), 0);
    append_frontier_tail(&state_db, unrelated.path());

    CursorComposerSource::with_home(home.path())
        .ingest_capped(
            &admission,
            project.path(),
            project_id,
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
        )
        .await
        .unwrap();
    assert_eq!(observation_count(&admission, "000000:bubble"), 1);
}

#[tokio::test]
async fn composer_frontier_retries_envelope_cap_before_growing_tail() {
    let project = tempfile::tempdir().unwrap();
    let unrelated = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let state_dir = home
        .path()
        .join(".config")
        .join("Cursor")
        .join("User")
        .join("globalStorage");
    std::fs::create_dir_all(&state_dir).unwrap();
    let state_db = state_dir.join("state.vscdb");
    seed_large_frontier_fixture(&state_db, project.path(), unrelated.path(), &[0, 1]);

    let admission = MemoryHostAdmission::default();
    let project_id = ProjectId::new("project.cursor-composer-cap-retry").unwrap();
    CursorComposerSource::with_home(home.path())
        .ingest_capped(&admission, project.path(), project_id.clone(), 1, None)
        .await
        .unwrap();
    assert_eq!(observation_count(&admission, "000000:bubble"), 1);
    assert_eq!(observation_count(&admission, "000001:bubble"), 0);
    append_frontier_tail(&state_db, unrelated.path());

    CursorComposerSource::with_home(home.path())
        .ingest_capped(&admission, project.path(), project_id, 1, None)
        .await
        .unwrap();
    assert_eq!(observation_count(&admission, "000000:bubble"), 1);
    assert_eq!(observation_count(&admission, "000001:bubble"), 1);
}

#[tokio::test]
async fn composer_frontier_retries_capture_error_before_growing_tail() {
    let project = tempfile::tempdir().unwrap();
    let unrelated = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let state_dir = home
        .path()
        .join(".config")
        .join("Cursor")
        .join("User")
        .join("globalStorage");
    std::fs::create_dir_all(&state_dir).unwrap();
    let state_db = state_dir.join("state.vscdb");
    seed_large_frontier_fixture(&state_db, project.path(), unrelated.path(), &[0]);

    let admission = MemoryHostAdmission::default();
    let project_id = ProjectId::new("project.cursor-composer-capture-retry").unwrap();
    admission.fail_next_capture();
    CursorComposerSource::with_home(home.path())
        .ingest_capped(
            &admission,
            project.path(),
            project_id.clone(),
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
        )
        .await
        .unwrap();
    assert_eq!(observation_count(&admission, "000000:bubble"), 0);
    append_frontier_tail(&state_db, unrelated.path());

    CursorComposerSource::with_home(home.path())
        .ingest_capped(
            &admission,
            project.path(),
            project_id,
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
        )
        .await
        .unwrap();
    assert_eq!(observation_count(&admission, "000000:bubble"), 1);
}

#[tokio::test]
async fn malformed_overlength_bubble_reference_does_not_block_valid_tail() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let state_dir = home
        .path()
        .join(".config")
        .join("Cursor")
        .join("User")
        .join("globalStorage");
    std::fs::create_dir_all(&state_dir).unwrap();
    let state_db = state_dir.join("state.vscdb");
    let connection = rusqlite::Connection::open(&state_db).unwrap();
    connection
        .execute_batch(
            "PRAGMA journal_mode=DELETE;
             CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO cursorDiskKV(key, value) VALUES (?1, ?2)",
            rusqlite::params![
                "composerData:000000",
                json!({
                    "composerId": "000000",
                    "workspaceIdentifier": {
                        "uri": { "fsPath": project.path().to_string_lossy() }
                    },
                    "fullConversationHeadersOnly": [{ "bubbleId": "x".repeat(600) }]
                })
                .to_string()
            ],
        )
        .unwrap();
    insert_composer_fixture(&connection, "000001", project.path());
    drop(connection);

    let admission = MemoryHostAdmission::default();
    CursorComposerSource::with_home(home.path())
        .ingest_capped(
            &admission,
            project.path(),
            ProjectId::new("project.cursor-composer-malformed-key").unwrap(),
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
        )
        .await
        .unwrap();

    assert_eq!(observation_count(&admission, "000001:bubble"), 1);
}

#[tokio::test]
async fn missing_bubble_does_not_block_tail_and_retries_when_it_becomes_visible() {
    let project = tempfile::tempdir().unwrap();
    let unrelated = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let state_dir = home
        .path()
        .join(".config")
        .join("Cursor")
        .join("User")
        .join("globalStorage");
    std::fs::create_dir_all(&state_dir).unwrap();
    let state_db = state_dir.join("state.vscdb");
    seed_large_frontier_fixture(&state_db, project.path(), unrelated.path(), &[0, 1]);
    let mut connection = rusqlite::Connection::open(&state_db).unwrap();
    connection
        .execute(
            "DELETE FROM cursorDiskKV WHERE key = 'bubbleId:000000:bubble'",
            [],
        )
        .unwrap();

    let admission = MemoryHostAdmission::default();
    let project_id = ProjectId::new("project.cursor-composer-late-bubble").unwrap();
    CursorComposerSource::with_home(home.path())
        .ingest_capped(
            &admission,
            project.path(),
            project_id.clone(),
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
        )
        .await
        .unwrap();
    assert_eq!(observation_count(&admission, "000000:bubble"), 0);
    assert_eq!(
        observation_count(&admission, "000001:bubble"),
        1,
        "a dangling header must not pin discovery behind its composer"
    );

    // Keep discovery away from EOF/wrap. The next pass must traverse another
    // full key window and then resolve `000000` through the durable retry set.
    let transaction = connection.transaction().unwrap();
    for index in 100_000..100_000 + MAX_COMPOSER_STORE_BLOB_VISITS + 1 {
        insert_composer_fixture(&transaction, &format!("{index:06}"), unrelated.path());
    }
    transaction.commit().unwrap();
    connection
        .execute(
            "INSERT INTO cursorDiskKV(key, value) VALUES (?1, ?2)",
            rusqlite::params![
                "bubbleId:000000:bubble",
                json!({ "type": 1, "text": "visible later" }).to_string()
            ],
        )
        .unwrap();
    drop(connection);

    CursorComposerSource::with_home(home.path())
        .ingest_capped(
            &admission,
            project.path(),
            project_id.clone(),
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
        )
        .await
        .unwrap();
    assert_eq!(observation_count(&admission, "000000:bubble"), 1);
    assert_eq!(observation_count(&admission, "000001:bubble"), 1);

    CursorComposerSource::with_home(home.path())
        .ingest_capped(
            &admission,
            project.path(),
            project_id,
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        observation_count(&admission, "000000:bubble"),
        1,
        "the resolved retry must be admitted exactly once"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn replacement_generation_adopts_retry_and_fences_stale_reader() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let state_dir = home
        .path()
        .join(".config")
        .join("Cursor")
        .join("User")
        .join("globalStorage");
    std::fs::create_dir_all(&state_dir).unwrap();
    let state_db = state_dir.join("state.vscdb");
    let connection = rusqlite::Connection::open(&state_db).unwrap();
    connection
        .execute_batch(
            "PRAGMA journal_mode=DELETE;
             CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);",
        )
        .unwrap();
    insert_composer_fixture(&connection, "replace", project.path());
    connection
        .execute(
            "DELETE FROM cursorDiskKV WHERE key = 'bubbleId:replace:bubble'",
            [],
        )
        .unwrap();
    drop(connection);

    let replacement = state_dir.join("state.next.vscdb");
    let connection = rusqlite::Connection::open(&replacement).unwrap();
    connection
        .execute_batch(
            "PRAGMA journal_mode=DELETE;
             CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);",
        )
        .unwrap();
    insert_composer_fixture(&connection, "replace", project.path());
    drop(connection);

    let admission = MemoryHostAdmission::default();
    let project_id = ProjectId::new("project.cursor-composer-replacement-retry").unwrap();
    CursorComposerSource::with_home(home.path())
        .ingest_capped(
            &admission,
            project.path(),
            project_id.clone(),
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
        )
        .await
        .unwrap();
    assert_eq!(observation_count(&admission, "replace:bubble"), 0);
    assert_eq!(
        admission
            .session_backfill_state_entries(COMPOSER_RETRY_KEY_PREFIX)
            .len(),
        1
    );

    let entered = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let release = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    admission.pause_next_session_backfill_page(entered.clone(), release.clone());
    let stale_admission = admission.clone();
    let stale_home = home.path().to_path_buf();
    let stale_project = project.path().to_path_buf();
    let stale_project_id = project_id.clone();
    let stale = tokio::spawn(async move {
        CursorComposerSource::with_home(&stale_home)
            .ingest_capped(
                &stale_admission,
                &stale_project,
                stale_project_id,
                DEFAULT_COMPOSER_ENVELOPE_CAP,
                None,
            )
            .await
    });
    entered.wait().await;

    std::fs::rename(&replacement, &state_db).unwrap();
    CursorComposerSource::with_home(home.path())
        .ingest_capped(
            &admission,
            project.path(),
            project_id.clone(),
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
        )
        .await
        .unwrap();
    release.wait().await;
    stale.await.unwrap().unwrap();

    assert_eq!(observation_count(&admission, "replace:bubble"), 1);
    assert!(
        admission
            .session_backfill_state_entries(COMPOSER_RETRY_KEY_PREFIX)
            .is_empty(),
        "the replacement generation must reclaim the adopted retry"
    );
    CursorComposerSource::with_home(home.path())
        .ingest_capped(
            &admission,
            project.path(),
            project_id,
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
        )
        .await
        .unwrap();
    assert_eq!(observation_count(&admission, "replace:bubble"), 1);
}

#[tokio::test]
async fn malformed_retry_rows_are_reclaimed_before_a_valid_retry() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let state_dir = home
        .path()
        .join(".config")
        .join("Cursor")
        .join("User")
        .join("globalStorage");
    std::fs::create_dir_all(&state_dir).unwrap();
    let state_db = state_dir.join("state.vscdb");
    let connection = rusqlite::Connection::open(&state_db).unwrap();
    connection
        .execute_batch(
            "PRAGMA journal_mode=DELETE;
             CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);",
        )
        .unwrap();
    let canonical_state_db = state_db.canonicalize().unwrap();
    let retry_prefix = composer_retry_key_prefix(&canonical_state_db);
    let composer_id = (0..1024)
        .map(|index| format!("valid-{index:04}"))
        .find(|composer_id| {
            composer_retry_key(&retry_prefix, &format!("composerData:{composer_id}"))
                .strip_prefix(&retry_prefix)
                .is_some_and(|suffix| suffix.as_bytes()[0] >= b'2')
        })
        .unwrap();
    insert_composer_fixture(&connection, &composer_id, project.path());
    connection
        .execute(
            "DELETE FROM cursorDiskKV WHERE key = ?1",
            [format!("bubbleId:{composer_id}:bubble")],
        )
        .unwrap();
    drop(connection);

    let admission = MemoryHostAdmission::default();
    let project_id = ProjectId::new("project.cursor-composer-malformed-retry").unwrap();
    CursorComposerSource::with_home(home.path())
        .ingest_capped(
            &admission,
            project.path(),
            project_id.clone(),
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
        )
        .await
        .unwrap();

    let scope = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };
    let generation =
        tracedecay_runtime_core::db::sqlite_generation_identity(&canonical_state_db).unwrap();
    for (suffix, value) in [
        ("0".repeat(64), "not-json".to_string()),
        (
            "1".repeat(64),
            json!({
                "composer_key": "composerData:not-the-key-owner",
                "owner_generation": generation,
                "nonce": "0".repeat(32),
            })
            .to_string(),
        ),
    ] {
        assert!(
            admission
                .compare_and_swap_session_backfill_state(
                    &scope,
                    &format!("{retry_prefix}{suffix}"),
                    None,
                    &value,
                )
                .await
                .unwrap()
        );
    }
    let connection = rusqlite::Connection::open(&state_db).unwrap();
    connection
        .execute(
            "INSERT INTO cursorDiskKV(key, value) VALUES (?1, ?2)",
            rusqlite::params![
                format!("bubbleId:{composer_id}:bubble"),
                json!({ "type": 1, "text": "visible" }).to_string()
            ],
        )
        .unwrap();
    drop(connection);

    CursorComposerSource::with_home(home.path())
        .ingest_capped(
            &admission,
            project.path(),
            project_id,
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        observation_count(&admission, &format!("{composer_id}:bubble")),
        1
    );
    assert!(
        admission
            .session_backfill_state_entries(&retry_prefix)
            .is_empty(),
        "malformed, mismatched, and completed retry rows must all be reclaimed"
    );
}

#[tokio::test]
async fn unresolved_retry_and_continuous_discovery_alternate_under_one_byte_budget() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let state_dir = home
        .path()
        .join(".config")
        .join("Cursor")
        .join("User")
        .join("globalStorage");
    std::fs::create_dir_all(&state_dir).unwrap();
    let state_db = state_dir.join("state.vscdb");
    let connection = rusqlite::Connection::open(&state_db).unwrap();
    connection
        .execute_batch(
            "PRAGMA journal_mode=DELETE;
             CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);",
        )
        .unwrap();
    let missing_envelope = json!({
        "composerId": "000000",
        "workspaceIdentifier": {
            "uri": { "fsPath": project.path().to_string_lossy() }
        },
        "padding": "x".repeat(700),
        "fullConversationHeadersOnly": [{ "bubbleId": "late" }]
    })
    .to_string();
    connection
        .execute(
            "INSERT INTO cursorDiskKV(key, value) VALUES (?1, ?2)",
            rusqlite::params!["composerData:000000", &missing_envelope],
        )
        .unwrap();
    insert_composer_fixture(&connection, "000001", project.path());
    drop(connection);

    let late_bubble = json!({ "type": 1, "text": "visible later" }).to_string();
    let budget = u64::try_from(missing_envelope.len() + late_bubble.len() + 8).unwrap();
    let admission = MemoryHostAdmission::default();
    let project_id = ProjectId::new("project.cursor-composer-fair-retry").unwrap();
    for _ in 0..2 {
        CursorComposerSource::with_home(home.path())
            .ingest_capped(
                &admission,
                project.path(),
                project_id.clone(),
                DEFAULT_COMPOSER_ENVELOPE_CAP,
                Some(budget),
            )
            .await
            .unwrap();
    }
    assert_eq!(observation_count(&admission, "000000:late"), 0);
    assert_eq!(observation_count(&admission, "000001:bubble"), 0);

    CursorComposerSource::with_home(home.path())
        .ingest_capped(
            &admission,
            project.path(),
            project_id.clone(),
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            Some(budget),
        )
        .await
        .unwrap();
    assert_eq!(
        observation_count(&admission, "000001:bubble"),
        1,
        "discovery must get the pass after a retry-first pass exhausts the budget"
    );

    let connection = rusqlite::Connection::open(&state_db).unwrap();
    connection
        .execute(
            "INSERT INTO cursorDiskKV(key, value) VALUES (?1, ?2)",
            rusqlite::params!["bubbleId:000000:late", late_bubble],
        )
        .unwrap();
    drop(connection);
    CursorComposerSource::with_home(home.path())
        .ingest_capped(
            &admission,
            project.path(),
            project_id.clone(),
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            Some(budget),
        )
        .await
        .unwrap();
    CursorComposerSource::with_home(home.path())
        .ingest_capped(
            &admission,
            project.path(),
            project_id,
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            Some(budget),
        )
        .await
        .unwrap();
    assert_eq!(
        observation_count(&admission, "000000:late"),
        1,
        "retry must get a bounded pass and remain exactly-once"
    );
}

#[tokio::test]
async fn more_than_one_scan_window_of_missing_bubbles_does_not_block_tail() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let state_dir = home
        .path()
        .join(".config")
        .join("Cursor")
        .join("User")
        .join("globalStorage");
    std::fs::create_dir_all(&state_dir).unwrap();
    let state_db = state_dir.join("state.vscdb");
    seed_large_frontier_fixture(&state_db, project.path(), project.path(), &[]);
    let connection = rusqlite::Connection::open(&state_db).unwrap();
    connection
        .execute("DELETE FROM cursorDiskKV WHERE key LIKE 'bubbleId:%'", [])
        .unwrap();
    insert_composer_fixture(&connection, "999999", project.path());
    drop(connection);

    let admission = MemoryHostAdmission::default();
    let project_id = ProjectId::new("project.cursor-composer-retry-saturation").unwrap();
    CursorComposerSource::with_home(home.path())
        .ingest_capped(
            &admission,
            project.path(),
            project_id.clone(),
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
        )
        .await
        .unwrap();
    assert_eq!(observation_count(&admission, "999999:bubble"), 0);

    CursorComposerSource::with_home(home.path())
        .ingest_capped(
            &admission,
            project.path(),
            project_id,
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        observation_count(&admission, "999999:bubble"),
        1,
        "the 4,097th unresolved composer must not backpressure later discovery"
    );
}

#[tokio::test]
async fn refreshed_missing_intent_rejects_stale_completion_and_reclaims_exact_row() {
    let project = tempfile::tempdir().unwrap();
    let state_db = project.path().join("state.vscdb");
    drop(rusqlite::Connection::open(&state_db).unwrap());
    let canonical_state_db = state_db.canonicalize().unwrap();
    let generation = ObservationSourceGenerationV1::new(
        tracedecay_runtime_core::db::sqlite_generation_identity(&canonical_state_db).unwrap(),
    )
    .unwrap();
    let admission = MemoryHostAdmission::default();
    let cancellation = ObservationCancellation::default();
    let matchers = crate::runtime::shared::ProjectRootMatcherCache::default();
    let context = ComposerIngestContext {
        facade: &admission,
        scope: ObservationScopeV1::Project {
            project_id: ProjectId::new("project.cursor-composer-retry-cas").unwrap(),
        },
        project_root: Some(project.path()),
        registered_roots: &[],
        cancellation: &cancellation,
        matchers: &matchers,
    };
    let retry_prefix = "cursor-composer.scan.fixture.retry.";
    let composer_key = "composerData:000000";

    ensure_composer_retry(
        &context,
        retry_prefix,
        composer_key,
        &canonical_state_db,
        generation,
    )
    .await
    .unwrap();
    let first = admission.session_backfill_state_entries(retry_prefix);
    assert_eq!(first.len(), 1);
    let (retry_key, stale_value) = (first[0].1.clone(), first[0].2.clone());

    // A newer immutable snapshot observes the composer missing again before
    // the old snapshot finishes. Refreshing the exact row must invalidate the
    // old sweep's completion authority.
    ensure_composer_retry(
        &context,
        retry_prefix,
        composer_key,
        &canonical_state_db,
        generation,
    )
    .await
    .unwrap();
    let refreshed = admission.session_backfill_state_entries(retry_prefix);
    assert_eq!(refreshed.len(), 1);
    assert_ne!(refreshed[0].2, stale_value);
    assert!(
        complete_composer_retry(
            &context,
            &retry_key,
            &stale_value,
            &canonical_state_db,
            generation,
        )
        .await
        .is_err(),
        "the stale snapshot must not delete the refreshed missing intent"
    );
    assert_eq!(
        admission.session_backfill_state_entries(retry_prefix),
        refreshed
    );

    complete_composer_retry(
        &context,
        &retry_key,
        &refreshed[0].2,
        &canonical_state_db,
        generation,
    )
    .await
    .unwrap();
    assert!(
        admission
            .session_backfill_state_entries(retry_prefix)
            .is_empty(),
        "terminal retry completion must reclaim the exact durable row"
    );

    ensure_composer_retry(
        &context,
        retry_prefix,
        composer_key,
        &canonical_state_db,
        generation,
    )
    .await
    .unwrap();
    let reinserted = admission.session_backfill_state_entries(retry_prefix);
    assert_ne!(reinserted[0].2, refreshed[0].2);
    assert!(
        complete_composer_retry(
            &context,
            &retry_key,
            &refreshed[0].2,
            &canonical_state_db,
            generation,
        )
        .await
        .is_err(),
        "a delete-and-reinsert cycle must not recreate the stale CAS value"
    );
}

#[tokio::test]
async fn retry_cycle_high_water_wraps_despite_continuous_higher_insertions() {
    let project = tempfile::tempdir().unwrap();
    let state_db = project.path().join("state.vscdb");
    drop(rusqlite::Connection::open(&state_db).unwrap());
    let canonical_state_db = state_db.canonicalize().unwrap();
    let generation = ObservationSourceGenerationV1::new(
        tracedecay_runtime_core::db::sqlite_generation_identity(&canonical_state_db).unwrap(),
    )
    .unwrap();
    let admission = MemoryHostAdmission::default();
    let cancellation = ObservationCancellation::default();
    let matchers = crate::runtime::shared::ProjectRootMatcherCache::default();
    let scope = ObservationScopeV1::Project {
        project_id: ProjectId::new("project.cursor-composer-retry-high-water").unwrap(),
    };
    let context = ComposerIngestContext {
        facade: &admission,
        scope: scope.clone(),
        project_root: Some(project.path()),
        registered_roots: &[],
        cancellation: &cancellation,
        matchers: &matchers,
    };
    let retry_prefix = "cursor-composer.scan.fixture.retry.";
    let mut candidates = (0..256)
        .map(|index| {
            let composer_key = format!("composerData:{index:06}");
            (
                composer_retry_key(retry_prefix, &composer_key),
                composer_key,
            )
        })
        .collect::<Vec<_>>();
    candidates.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    let target = candidates[0].clone();
    let initial_cursor = candidates[8].0.clone();
    let initial_cycle = &candidates[9..26];
    for (retry_key, composer_key) in std::iter::once(&target).chain(initial_cycle.iter()) {
        ensure_composer_retry(
            &context,
            retry_prefix,
            composer_key,
            &canonical_state_db,
            generation,
        )
        .await
        .unwrap();
        assert_eq!(retry_key, &composer_retry_key(retry_prefix, composer_key));
    }
    let captured_high_water = admission
        .session_backfill_state_high_water(&scope, retry_prefix)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(captured_high_water, initial_cycle.last().unwrap().0);

    let mut after = Some(initial_cursor);
    let mut inserted = 26usize;
    let mut pages = 0usize;
    loop {
        let page = admission
            .list_session_backfill_state_page(
                &scope,
                retry_prefix,
                after.as_deref(),
                &captured_high_water,
            )
            .await
            .unwrap();
        if page.is_empty() {
            break;
        }
        after = page.last().map(|(key, _)| key.clone());
        pages += 1;
        for (retry_key, composer_key) in &candidates[inserted..inserted + 16] {
            assert!(retry_key.as_str() > captured_high_water.as_str());
            ensure_composer_retry(
                &context,
                retry_prefix,
                composer_key,
                &canonical_state_db,
                generation,
            )
            .await
            .unwrap();
        }
        inserted += 16;
    }
    assert_eq!(pages, 3, "the captured cycle must terminate independently");

    let next_high_water = admission
        .session_backfill_state_high_water(&scope, retry_prefix)
        .await
        .unwrap()
        .unwrap();
    let wrapped = admission
        .list_session_backfill_state_page(&scope, retry_prefix, None, &next_high_water)
        .await
        .unwrap();
    assert_eq!(
        wrapped.first().map(|(key, _)| key),
        Some(&target.0),
        "wrapping at the captured high water must expose a behind-cursor retry"
    );
}

#[tokio::test]
async fn composer_key_scan_reports_corrupt_or_incompatible_schema() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("state #?%.vscdb");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=DELETE;
             CREATE TABLE incompatible (key TEXT PRIMARY KEY, value TEXT);",
        )
        .unwrap();
    }

    let ro = open_readonly_immutable(&path)
        .await
        .expect("open URI-metacharacter path");
    let error = scan_composer_keys_page(&ro.conn, None, COMPOSER_KEY_SCAN_PAGE)
        .await
        .expect_err("missing required schema must not look like EOF");
    assert!(error.contains("cursorDiskKV"), "{error}");
}

#[test]
fn protobuf_child_refs_rejects_overflowing_or_truncated_lengths() {
    let overflowing_length = [0x0a, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f];
    assert!(protobuf_child_refs(&overflowing_length).is_none());

    let truncated_length = [0x0a, 0xff, 0xff, 0xff, 0xff, 0x0f];
    assert!(protobuf_child_refs(&truncated_length).is_none());

    let mut valid_then_truncated = vec![0x0a, 32];
    valid_then_truncated.extend([0x42; 32]);
    valid_then_truncated.extend([0x09, 0x01]);
    assert!(
        protobuf_child_refs(&valid_then_truncated).is_none(),
        "a corrupt protobuf must not expose refs parsed before the corruption"
    );

    let overflowing_varint = [0x08, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02];
    assert!(protobuf_child_refs(&overflowing_varint).is_none());
}

#[cfg(unix)]
#[test]
fn composer_chat_discovery_does_not_follow_symlinks() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::TempDir::new().unwrap();
    let real_dir = tmp.path().join("real");
    std::fs::create_dir(&real_dir).unwrap();
    let linked_dir = tmp.path().join("linked");
    symlink(&real_dir, &linked_dir).unwrap();
    let entry = std::fs::read_dir(tmp.path())
        .unwrap()
        .flatten()
        .find(|entry| entry.file_name() == "linked")
        .unwrap();
    assert!(!directory_entry_is_real_dir(&entry));

    let real_file = real_dir.join("store.db");
    std::fs::write(&real_file, b"not opened").unwrap();
    let linked_file = tmp.path().join("store.db");
    symlink(&real_file, &linked_file).unwrap();
    assert!(!path_is_regular_file_no_follow(&linked_file));
    assert!(path_is_regular_file_no_follow(&real_file));
}

#[tokio::test]
async fn sql_length_gate_rejects_oversized_bubble_built_in_sql() {
    // Hostile TEXT is constructed entirely in SQL (hex(zeroblob)) so the
    // product fetch never receives a pre-built Rust String of that value.
    let setup = "INSERT INTO cursorDiskKV(key, value) \
         SELECT 'bubbleId:comp:hostile', hex(zeroblob(33));";
    let (tmp, ro) = open_temp_kv_db_with_sql(setup).await;
    let _keep = tmp;

    match fetch_kv_text_bounded(&ro.conn, "bubbleId:comp:hostile", 64, None).await {
        BoundedSqliteValue::Oversized { byte_len } => {
            assert_eq!(byte_len, 66);
        }
        other => panic!("expected Oversized, got {other:?}"),
    }
    match fetch_bubble_bounded(&ro.conn, "comp", "hostile", None).await {
        // 66 bytes is under the real 1 MiB record ceiling; complete non-JSON
        // text receives typed malformed coverage rather than disappearing.
        BoundedSqliteValue::Malformed { byte_len } => assert_eq!(byte_len, 66),
        other => panic!("unexpected bubble outcome {other:?}"),
    }
}

#[tokio::test]
async fn sql_length_gate_counts_utf8_bytes_not_characters() {
    // SQLite length(TEXT) would report 40 characters and incorrectly admit
    // this 80-byte value under a 64-byte ceiling. Construct it in SQL so no
    // product Rust code pre-materializes the hostile text.
    let setup = "INSERT INTO cursorDiskKV(key, value) \
         SELECT 'bubbleId:comp:multibyte', \
                replace(hex(zeroblob(40)), '00', 'é');";
    let (tmp, ro) = open_temp_kv_db_with_sql(setup).await;
    let _keep = tmp;

    match fetch_kv_text_bounded(&ro.conn, "bubbleId:comp:multibyte", 64, None).await {
        BoundedSqliteValue::Oversized { byte_len } => assert_eq!(byte_len, 80),
        other => panic!("expected UTF-8 byte Oversized, got {other:?}"),
    }
}

/// Replacing `octet_length(value)` with `length(CAST(value AS BLOB))`
/// materializes the retained value and trips this deliberately lowered limit.
#[tokio::test]
async fn composer_value_length_stays_readable_below_sqlite_materialization_limit() {
    let setup = "INSERT INTO cursorDiskKV(key, value) \
         SELECT 'composerData:oversized', hex(zeroblob(4096));";
    let (tmp, ro) = open_temp_kv_db_with_sql(setup).await;
    let _keep = tmp;
    ro.conn
        .with(|conn| {
            conn.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, 1024)
                .map(|_| ())
        })
        .await
        .expect("blocking SQLite limit update completed")
        .expect("lower SQLite length limit");

    let rows = scan_composer_keys_page(&ro.conn, None, 1)
        .await
        .expect("length-only composer scan must not materialize the oversized value");
    assert_eq!(rows, vec![("composerData:oversized".to_string(), 8192)]);
    match fetch_kv_text_bounded(&ro.conn, "composerData:oversized", 64, None).await {
        BoundedSqliteValue::Oversized { byte_len } => assert_eq!(byte_len, 8192),
        other => panic!("expected length-only Oversized classification, got {other:?}"),
    }
}

#[tokio::test]
async fn sql_budget_gate_defers_before_materializing_bubble_text() {
    let (tmp, ro) =
        open_temp_kv_db_with_rows(&[("bubbleId:comp:b1", r#"{"type":1,"text":"hello"}"#)]).await;
    let _keep = tmp;

    match fetch_bubble_bounded(&ro.conn, "comp", "b1", Some(4)).await {
        BoundedSqliteValue::BudgetExceeded { byte_len } => {
            assert!(byte_len > 4);
        }
        other => panic!("expected BudgetExceeded, got {other:?}"),
    }
}

#[tokio::test]
async fn store_blob_zeroblob_is_skipped_without_full_table_select() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("store.db");
    let root = "aa".repeat(32);
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=DELETE;\n\
             CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);\n\
             CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB);",
        )
        .unwrap();
        let leaf = "bb".repeat(32);
        let meta = serde_json::json!({
            "agentId": "agent-adv",
            "latestRootBlobId": root,
            "createdAt": 1_700_000_000_000i64,
        });
        conn.execute(
            "INSERT INTO meta(key, value) VALUES ('0', ?1)",
            rusqlite::params![encode_hex(meta.to_string().as_bytes())],
        )
        .unwrap();
        let hostile = (max_composer_record_bytes() as i64).saturating_add(64);
        conn.execute(
            "INSERT INTO blobs(id, data) VALUES (?1, zeroblob(?2))",
            rusqlite::params![root.clone(), hostile],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO blobs(id, data) VALUES (?1, ?2)",
            rusqlite::params![
                leaf,
                serde_json::json!({"role":"user","content":"reachable"})
                    .to_string()
                    .into_bytes()
            ],
        )
        .unwrap();
    }

    let ro = open_readonly_immutable(&path).await.unwrap();
    let mut budget = IngestByteBudget::bounded(DEFAULT_COMPOSER_SWEEP_BYTES);
    let outcome = order_store_messages_bounded(&ro.conn, Some(&root), &mut budget).await;
    // Hostile root is skipped (oversized); fallback id-sort still finds the leaf.
    match outcome {
        StoreWalkOutcome::Messages(messages) => {
            assert!(
                messages.iter().any(|(role, _, _)| role == "user"),
                "bounded fallback should still reach the valid leaf"
            );
        }
        StoreWalkOutcome::DeferredEmpty => panic!("default sweep budget should reach leaf"),
    }
}

#[test]
fn configured_composer_sqlite_bounds_match_shared_host_ceilings() {
    assert_eq!(max_composer_record_bytes(), 1_048_576);
    assert_eq!(MAX_COMPOSER_ENVELOPE_BYTES, 16 * 1024 * 1024);
    assert_eq!(DEFAULT_COMPOSER_SWEEP_BYTES, 16 * 1024 * 1024 + 1);
    assert_eq!(MAX_COMPOSER_STORE_META_BYTES, 256 * 1024);
    assert_eq!(MAX_COMPOSER_STORE_META_HEX_BYTES, 512 * 1024);
    assert_eq!(MAX_COMPOSER_STORE_BLOB_VISITS, 4096);
    assert_eq!(MAX_COMPOSER_SQLITE_KEY_BYTES, 512);
}
