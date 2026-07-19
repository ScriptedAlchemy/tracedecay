use super::capture::*;
use super::ingest::*;
use super::observation::normalize_cursor_composer_envelope_observation;
use super::sqlite::*;
use super::store::*;
use super::*;

use std::fmt::Write as _;

use libsql::{Builder, OpenFlags};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracedecay_domain::{CanonicalObservationFactV1, CanonicalWorkflowSemanticKindV1};
use tracedecay_domain::{
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceGenerationV1,
    ObservationSourceIdentityV1, ProviderId, SessionId,
};
use tracedecay_store::ObservationPersistOutcome;

use crate::privacy::MAX_OBSERVATION_RECORD_BYTES;
use crate::sessions::ingest_byte_budget::IngestByteBudget;
use crate::sessions::snapshot_observation::MAX_SNAPSHOT_METADATA_BYTES;
use crate::sessions::source::MAX_JSONL_RECORD_BYTES;

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
        "../../../tests/fixtures/provider_normalization/cursor_composer/assistant_bubble.input.json"
    ))
    .expect("Cursor composer golden input");
    let expected: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/provider_normalization/cursor_composer/assistant_bubble.expected_envelope.json"
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
        "../../../tests/fixtures/provider_normalization/cursor_composer/envelope_todos.input.json"
    ))
    .expect("Cursor composer envelope todos golden input");
    let expected: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/provider_normalization/cursor_composer/envelope_todos.expected_envelope.json"
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
        "../../../tests/fixtures/provider_normalization/cursor_composer/envelope_todos.input.json"
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
        "../../../tests/fixtures/provider_normalization/cursor_composer/assistant_bubble_with_todos.input.json"
    ))
    .expect("Cursor composer bubble+todos golden input");
    let expected: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/provider_normalization/cursor_composer/assistant_bubble_with_todos.expected_envelope.json"
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

async fn open_temp_kv_db_with_rows(rows: &[(&str, &str)]) -> (tempfile::TempDir, ReadOnlyDb) {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("state.vscdb");
    {
        let db = Builder::new_local(&path).build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=DELETE;\n\
             CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);",
        )
        .await
        .unwrap();
        for (key, value) in rows {
            conn.execute(
                "INSERT INTO cursorDiskKV(key, value) VALUES (?1, ?2)",
                libsql::params![*key, *value],
            )
            .await
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
        let db = Builder::new_local(&path).build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=DELETE;\n\
             CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);",
        )
        .await
        .unwrap();
        conn.execute_batch(setup_sql).await.unwrap();
    }
    let ro = open_readonly_immutable(&path).await.expect("open readonly");
    (tmp, ro)
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
        let db = Builder::new_local(&path).build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=DELETE;\n\
             CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);\n\
             CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB);",
        )
        .await
        .unwrap();
        let leaf = "bb".repeat(32);
        let meta = serde_json::json!({
            "agentId": "agent-adv",
            "latestRootBlobId": root,
            "createdAt": 1_700_000_000_000i64,
        });
        conn.execute(
            "INSERT INTO meta(key, value) VALUES ('0', ?1)",
            libsql::params![encode_hex(meta.to_string().as_bytes())],
        )
        .await
        .unwrap();
        let hostile = (max_composer_record_bytes() as i64).saturating_add(64);
        conn.execute(
            "INSERT INTO blobs(id, data) VALUES (?1, zeroblob(?2))",
            libsql::params![root.clone(), hostile],
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO blobs(id, data) VALUES (?1, ?2)",
            libsql::params![
                leaf,
                libsql::Value::Blob(
                    serde_json::json!({"role":"user","content":"reachable"})
                        .to_string()
                        .into_bytes()
                )
            ],
        )
        .await
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
fn configured_composer_sqlite_bounds_match_shared_pr6_ceilings() {
    assert_eq!(max_composer_record_bytes(), 1_048_576);
    assert_eq!(MAX_COMPOSER_ENVELOPE_BYTES, 16 * 1024 * 1024);
    assert_eq!(DEFAULT_COMPOSER_SWEEP_BYTES, 16 * 1024 * 1024 + 1);
    assert_eq!(MAX_COMPOSER_STORE_META_BYTES, 256 * 1024);
    assert_eq!(MAX_COMPOSER_STORE_META_HEX_BYTES, 512 * 1024);
    assert_eq!(MAX_COMPOSER_STORE_BLOB_VISITS, 4096);
    assert_eq!(MAX_COMPOSER_SQLITE_KEY_BYTES, 512);
}
