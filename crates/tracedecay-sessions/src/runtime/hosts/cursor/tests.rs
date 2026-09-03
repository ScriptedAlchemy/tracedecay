use super::*;
use serde_json::json;

#[test]
fn host_event_ordering_is_kept_distinct_from_transcript_ordering() {
    let event = json!({
        "event_id": "evt-redacted",
        "event_sequence": 41,
        "timestamp": 1_783_500_600_i64,
    });
    let mut metadata = serde_json::Map::new();
    append_host_event_ordering(&mut metadata, &event, 128);
    assert_eq!(metadata["cursor_host_event_id"], "evt-redacted");
    assert_eq!(metadata["cursor_host_event_sequence"], 41);
    assert_eq!(metadata["cursor_host_event_timestamp"], 1_783_500_600_i64);
    assert_eq!(metadata["cursor_transcript_offset"], 128);
}

#[test]
fn native_record_identity_is_stable_across_json_formatting() {
    let compact: Value =
        serde_json::from_str(r#"{"role":"assistant","message":{"content":"redacted fixture"}}"#)
            .unwrap();
    let spaced: Value = serde_json::from_str(
        r#"{ "message": { "content": "redacted fixture" }, "role": "assistant" }"#,
    )
    .unwrap();
    assert_eq!(
        observation_native_record_id("cursor", "session-redacted", &compact)
            .unwrap()
            .as_str(),
        observation_native_record_id("cursor", "session-redacted", &spaced)
            .unwrap()
            .as_str()
    );
}

#[test]
fn canonical_record_is_stable_across_hook_sweep_and_mtime_context() {
    let transcript_path = Path::new("/redacted/project/session.fixture.jsonl");
    let hook_context = cursor_observation_context(
        &json!({
            "cwd": "/redacted/project",
            "conversation_id": "route-only-conversation",
            "model": "route-only-model"
        }),
        transcript_path,
        false,
    );
    let sweep_context = cursor_observation_context(
        &cursor_sweep_event("session.fixture", Path::new("/redacted/project"), false),
        transcript_path,
        false,
    );
    let native = json!({
        "role": "assistant",
        "message": {"content": "stable transcript content"}
    });
    let record_id = observation_native_record_id("cursor", "session.fixture", &native).unwrap();
    let range = tracedecay_domain::ObservationSourceRangeV1::new(10, 90).unwrap();

    let hook = normalize_cursor_observation_with_message_id(
        &cursor_native_with_context(native.clone(), &hook_context, None, None),
        "session.fixture",
        record_id.clone(),
        record_id.clone(),
        range,
        None,
        None,
    )
    .unwrap();
    let sweep = normalize_cursor_observation_with_message_id(
        &cursor_native_with_context(native, &sweep_context, None, None),
        "session.fixture",
        record_id.clone(),
        record_id,
        range,
        None,
        None,
    )
    .unwrap();

    assert_eq!(
        serde_json::to_value(hook).unwrap(),
        serde_json::to_value(sweep).unwrap()
    );
}

#[test]
fn canonical_cursor_record_keeps_typed_tools_and_structured_content() {
    let native = json!({
        "role": "assistant",
        "cwd": "/secret/worktree",
        "workspace_roots": ["/secret/worktree"],
        "message": {
            "content": [
                {"type": "text", "text": "redacted answer"},
                {
                    "type": "tool_use",
                    "id": "tool-redacted",
                    "name": "Read",
                    "input": {"path": "/secret/worktree/file.rs", "token": "credential-redacted"}
                },
                {"type": "thinking", "thinking": "provider-visible summary"}
            ]
        }
    });
    let range = tracedecay_domain::ObservationSourceRangeV1::new(10, 90).unwrap();
    let record_id = observation_native_record_id("cursor", "session-redacted", &native).unwrap();
    let envelope = normalize_cursor_observation(
        &native,
        "session-redacted",
        record_id.clone(),
        range,
        None,
        None,
    )
    .unwrap();
    let rendered = format!("{envelope:?}");
    assert!(rendered.contains("Message"));
    assert!(rendered.contains("ToolInvocation"));
    assert!(rendered.contains("Reasoning"));
    assert!(rendered.contains("FileBytes"));
    assert!(rendered.contains(record_id.as_str()));
    assert!(rendered.contains("/secret/worktree/file.rs"));
    assert!(rendered.contains("credential-redacted"));
    let relations = serde_json::to_value(envelope.relations()).unwrap();
    assert!(relations.get("thread_id").is_none());
    assert!(relations.get("turn_id").is_none());
    assert!(relations.get("agent_id").is_none());
    assert!(relations.get("parent_agent_id").is_none());
}

#[test]
fn cursor_conversation_id_sets_thread_relation_without_inventing_turn() {
    let native = json!({
        "role": "user",
        "conversation_id": "conversation-native",
        "message": {"content": [{"type": "text", "text": "hello"}]}
    });
    let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 20).unwrap();
    let record_id = observation_native_record_id("cursor", "conversation-native", &native).unwrap();
    let envelope = normalize_cursor_observation(
        &native,
        "conversation-native",
        record_id.clone(),
        range,
        None,
        None,
    )
    .unwrap();
    let relations = serde_json::to_value(envelope.relations()).unwrap();
    assert_eq!(relations["thread_id"], "conversation-native");
    assert_eq!(relations["message_id"], record_id.as_str());
    assert!(relations.get("turn_id").is_none());
    assert!(relations.get("agent_id").is_none());
    assert!(relations.get("parent_agent_id").is_none());
}

#[test]
fn cursor_subagent_lineage_sets_native_agent_relations() {
    let native = json!({
        "role": "assistant",
        "conversation_id": "child-agent",
        "message": {"content": [{"type": "text", "text": "subagent reply"}]}
    });
    let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 20).unwrap();
    let record_id = observation_native_record_id("cursor", "child-agent", &native).unwrap();
    let envelope = normalize_cursor_observation(
        &native,
        "child-agent",
        record_id,
        range,
        Some("child-agent"),
        Some("parent-conversation"),
    )
    .unwrap();
    let relations = serde_json::to_value(envelope.relations()).unwrap();
    assert_eq!(relations["thread_id"], "child-agent");
    assert_eq!(relations["agent_id"], "child-agent");
    assert_eq!(relations["parent_agent_id"], "parent-conversation");
    assert!(relations.get("turn_id").is_none());
}

/// Exact assistant+`tool_use` JSONL shape from
/// `tests/transcript_ingest_suite/cursor.rs`
/// (`cursor_tool_use_blocks_populate_tool_event_metadata`). Provider-parser
/// evidence is the native `role`/`message.content[]` Cursor transcript
/// record; the expected output is the canonical envelope projection with
/// explicit Cursor provider provenance — not a generic hand-built record.
#[test]
fn fixture_backed_cursor_jsonl_tool_use_reaches_canonical_envelope() {
    let native: Value = serde_json::from_str(include_str!(
        "../../../../../../tests/fixtures/provider_normalization/cursor/tool_use.input.json"
    ))
    .expect("Cursor golden input");
    let expected: Value = serde_json::from_str(include_str!(
        "../../../../../../tests/fixtures/provider_normalization/cursor/tool_use.expected_envelope.json"
    ))
    .expect("Cursor golden expected envelope");
    let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 64).unwrap();
    let record_id = observation_native_record_id("cursor", "cursor-tool-fixture", &native).unwrap();
    let envelope = normalize_cursor_observation(
        &native,
        "cursor-tool-fixture",
        record_id.clone(),
        range,
        None,
        None,
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
    let relations = actual["relations"].as_object().unwrap();
    assert_eq!(relations["session_id"], expected["relations"]["session_id"]);
    assert_eq!(relations["message_id"], record_id.as_str());
    for absent in expected["relations"]["absent"].as_array().unwrap() {
        assert!(relations.get(absent.as_str().unwrap()).is_none());
    }
    let facts = actual["facts"].as_array().unwrap();
    assert!(facts.iter().any(|fact| fact["kind"] == "session"));
    assert!(facts.iter().any(|fact| {
        fact["kind"] == "message" && fact["content"] == native["message"]["content"]
    }));
    assert!(facts.iter().any(|fact| {
        fact["kind"] == "tool_invocation"
            && fact["arguments"] == native["message"]["content"][1]["input"]
    }));
    assert!(
        envelope
            .facts()
            .iter()
            .all(|fact| { !matches!(fact, CanonicalObservationFactV1::WorkflowLifecycle { .. }) }),
        "Cursor JSONL fixture must not emit WorkflowLifecycle without native lifecycle evidence"
    );
}

#[test]
fn fixture_backed_cursor_workflow_lookalike_emits_no_workflow_lifecycle() {
    let native: Value = serde_json::from_str(include_str!(
        "../../../../../../tests/fixtures/provider_normalization/cursor/workflow_lookalike.input.json"
    ))
    .expect("Cursor workflow lookalike input");
    let expected: Value = serde_json::from_str(include_str!(
        "../../../../../../tests/fixtures/provider_normalization/cursor/workflow_lookalike.expected_envelope.json"
    ))
    .expect("Cursor workflow lookalike expected");
    let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 64).unwrap();
    let record_id =
        observation_native_record_id("cursor", "cursor-workflow-lookalike", &native).unwrap();
    let envelope = normalize_cursor_observation(
        &native,
        "cursor-workflow-lookalike",
        record_id,
        range,
        None,
        None,
    )
    .unwrap();
    let actual = serde_json::to_value(&envelope).unwrap();
    let facts = actual["facts"].as_array().unwrap();
    assert!(facts.iter().any(|fact| {
        fact["kind"] == "message"
            && fact["content"].as_str() == expected["expected_message"].as_str()
    }));
    for forbidden in expected["forbidden_fact_kinds"].as_array().unwrap() {
        assert!(
            facts.iter().all(|fact| fact["kind"] != *forbidden),
            "forbidden fact kind {forbidden} must remain absent"
        );
    }
    assert!(
        envelope
            .facts()
            .iter()
            .all(|fact| { !matches!(fact, CanonicalObservationFactV1::WorkflowLifecycle { .. }) }),
        "Cursor JSONL workflow lookalikes must not become WorkflowLifecycle"
    );
    let rendered = actual.to_string();
    for rejected in expected["encoded_must_not_contain"].as_array().unwrap() {
        assert!(
            !rendered.contains(rejected.as_str().unwrap()),
            "{rejected} must not survive Cursor JSONL normalization"
        );
    }
}

#[test]
fn every_batch_of_a_rewritten_transcript_keeps_one_replacement_namespace() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session-redacted.jsonl");
    let event = json!({"session_id": "session-redacted"});
    std::fs::write(
        &path,
        "{\"role\":\"user\",\"content\":\"first generation\"}\n",
    )
    .unwrap();

    let first = parse_cursor_jsonl(
        &event,
        "session-redacted",
        &path,
        StoredCursor::default(),
        None,
        false,
    )
    .unwrap();
    assert_eq!(first.messages.len(), 1);
    assert!(!first.messages[0].message_id.contains(":generation:"));

    // Truncate-and-rewrite, then read the replacement one record per batch
    // so the second batch no longer starts at the file head.
    std::fs::write(
        &path,
        "{\"role\":\"user\",\"content\":\"replacement head\"}\n\
         {\"role\":\"user\",\"content\":\"replacement tail\"}\n",
    )
    .unwrap();

    let head = parse_cursor_jsonl(
        &event,
        "session-redacted",
        &path,
        first.new_cursor,
        Some(1),
        false,
    )
    .unwrap();
    assert_eq!(head.messages.len(), 1);
    let suffix = format!(":generation:{}", head.new_cursor.file_id);
    assert!(head.messages[0].message_id.ends_with(&suffix));

    let tail = parse_cursor_jsonl(
        &event,
        "session-redacted",
        &path,
        head.new_cursor,
        Some(1),
        false,
    )
    .unwrap();
    assert_eq!(tail.messages.len(), 1);
    // Without the stored generation the tail would re-mint the bare
    // `<session>:<offset>` id and overwrite retained pre-rewrite history.
    assert!(tail.messages[0].message_id.ends_with(&suffix));
    assert_ne!(tail.messages[0].message_id, first.messages[0].message_id);
}
