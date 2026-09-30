use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_domain::ObservationScopeV1;
use tracedecay_lcm::{LcmCompressionRequest, LcmSummarizerMode};
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_sessions::admission::HostAdmissionScope;
use tracedecay_sessions::observation::ObservationCancellation;
use tracedecay_sessions::runtime::hosts::claude::ClaudeSource;
use tracedecay_sessions::runtime::hosts::claude_observation::ingest_source_with_observations_with_admission;

use crate::common::{
    lcm_raw_message as sample_message, lcm_raw_session as sample_session,
    open_lcm_db as open_isolated_db,
};

async fn open_registered_runtime(tmp: &TempDir) -> HostAdmissionTestRuntimeV1 {
    HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
        .await
        .expect("registered profile session runtime")
}

#[tokio::test]
async fn active_replay_metadata_namespaces_original_fields_from_storage_metadata() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    assert!(
        db.upsert_session(&sample_session(
            "cursor",
            "session-active-metadata",
            "project-a"
        ))
        .await
    );

    let active_message = json!({
        "id": "active-collision-metadata",
        "role": "assistant",
        "content": [
            {"type": "text", "text": "namespaced active replay"},
            {"type": "input_json", "value": {"ok": true}},
        ],
        "payload_ref": "original-payload-ref",
        "byte_count": 9876,
        "char_count": 543,
        "sha256": "original-sha256",
        "external_payload": {"source": "original-message"},
        "ingest_protection": {"source": "original-message"},
    });

    // Active-message ingest happens on the compress path now; preflight is a
    // read-only decision surface and never stores host messages.
    let response = db
        .lcm_compress(LcmCompressionRequest {
            provider: "cursor".into(),
            session_id: "session-active-metadata".into(),
            messages: vec![active_message.clone()],
            current_tokens: Some(100),
            focus_topic: None,
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
            ignore_message_patterns: Vec::new(),
            expected_current_frontier_store_id: None,
            threshold_tokens: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: None,
            incremental_max_depth: None,
            fresh_tail_count: None,
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
            summarizer: LcmSummarizerMode::Noop,
        })
        .await
        .unwrap();
    assert_eq!(response.status, "ok");
    assert_eq!(response.summary_nodes_created, 0);

    let raw = db
        .lcm_load_raw_message("cursor", "active-collision-metadata")
        .await
        .expect("raw active message should exist");
    let metadata: Value = serde_json::from_str(raw.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["lcm_active_replay"], true);
    assert_eq!(metadata["active_replay"], active_message);
    assert!(metadata.get("payload_ref").is_none());
    assert!(metadata.get("byte_count").is_none());
    assert!(metadata.get("char_count").is_none());
    assert!(metadata.get("sha256").is_none());
    assert!(metadata.get("external_payload").is_none());
}

#[tokio::test]
async fn transcript_ingest_preserves_lossless_raw_content() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let transcript_dir = home.join(".claude/projects/-lossless");
    std::fs::create_dir_all(&transcript_dir).unwrap();
    let content = format!("{}{}", "a".repeat(300_000), "::lossless-tail");
    let record = json!({
        "type": "user",
        "cwd": project,
        "sessionId": "lossless-session",
        "uuid": "lossless-message",
        "timestamp": "2026-01-01T00:00:00.000Z",
        "message": {"role": "user", "content": content}
    });
    std::fs::write(
        transcript_dir.join("lossless-session.jsonl"),
        format!("{record}\n"),
    )
    .unwrap();

    let db = open_registered_runtime(&tmp).await;
    ingest_source_with_observations_with_admission(
        &ClaudeSource::with_home(&home),
        &project,
        ObservationScopeV1::Profile,
        &db.facade(),
        None,
        ObservationCancellation::default(),
    )
    .await
    .unwrap();

    let stored = db
        .session_message_for_test(HostAdmissionScope::Profile, "claude", "lossless-message")
        .await
        .unwrap()
        .expect("session message should exist");
    assert_eq!(
        stored.text, content,
        "the session row is the one lossless copy"
    );

    let raw = db
        .lcm_load_raw_message_for_test("claude", "lossless-message")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.content, content);
}

#[tokio::test]
async fn search_uses_bounded_projection_but_load_recovers_raw() {
    let tmp = TempDir::new().unwrap();
    let db = open_registered_runtime(&tmp).await;
    let session = sample_session("cursor", "session-1", "project-a");
    assert!(
        db.upsert_session_for_test(HostAdmissionScope::Profile, &session)
            .await
            .unwrap()
    );

    let oversized = format!(
        "unique-search-token\n{}::lossless-tail",
        "x".repeat(tracedecay_lcm::MAX_DERIVED_TEXT_CHARS * 5)
    );
    let message = sample_message("cursor", "message-1", "session-1", &oversized);
    db.upsert_session_message_for_test(HostAdmissionScope::Profile, &message)
        .await
        .unwrap();

    let results = db
        .search_session_messages_for_test(
            HostAdmissionScope::Profile,
            "cursor",
            Some("project-a"),
            "unique-search-token",
            10,
        )
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].message.message_id, "message-1");
    assert!(results[0].message.text.chars().count() <= tracedecay_lcm::MAX_DERIVED_TEXT_CHARS);
    assert!(
        results[0]
            .message
            .text
            .contains(tracedecay_lcm::DERIVED_TRUNCATION_MARKER)
    );

    let raw = db
        .lcm_load_raw_message_for_test("cursor", "message-1")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.content, oversized);
    assert!(raw.content.ends_with("::lossless-tail"));
}
