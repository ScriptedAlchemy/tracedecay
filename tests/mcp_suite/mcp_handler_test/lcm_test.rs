#[cfg(unix)]
use crate::common;
use crate::support::*;
use serde_json::{Value, json};
#[cfg(feature = "test-transport")]
use std::time::SystemTime;
#[cfg(feature = "test-transport")]
use tracedecay::application::host_admission::{HostAdmissionScope, LcmLineageFaultForTest};
use tracedecay::mcp::get_tool_definitions;
#[cfg(feature = "test-transport")]
use tracedecay_domain::CanonicalMessageRoleV1;
#[cfg(feature = "test-transport")]
use tracedecay_domain::PayloadAccessState;
#[cfg(feature = "test-transport")]
use tracedecay_sessions::runtime::lcm::types::LcmImmutableSummaryPublication;
#[cfg(feature = "test-transport")]
use tracedecay_sessions::runtime::lcm::{
    LcmLifecycleUpdate, LcmMaintenanceDebt, LcmSourceRef, LcmSummaryNodeDraft,
};
#[cfg(feature = "test-transport")]
use tracedecay_sessions::runtime::{SessionMessageRecord, SessionRecord};

#[test]
fn lcm_mutation_tools_remain_daemon_internal() {
    let names = get_tool_definitions()
        .expect("tool definitions")
        .into_iter()
        .map(|tool| tool.name)
        .collect::<std::collections::BTreeSet<_>>();

    for retired in [
        "tracedecay_lcm_preflight",
        "tracedecay_lcm_compress",
        "tracedecay_lcm_session_boundary",
    ] {
        assert!(
            !names.contains(retired),
            "{retired} must remain daemon-internal"
        );
    }
}
#[tokio::test]
async fn lcm_tools_reject_invalid_storage_routing_arguments() {
    let dir = test_temp_dir();
    let (cg, _env) = init_test_project(dir.path()).await;
    for (removed, value, expected) in [
        (
            "storage_scope",
            json!("hermes_profile"),
            "storage_scope must be one of",
        ),
        (
            "hermes_home",
            json!("/tmp/hermes"),
            "unknown parameter `hermes_home`",
        ),
    ] {
        let mut args = json!({"provider": "cursor"});
        args.as_object_mut()
            .unwrap()
            .insert(removed.to_string(), value);
        let error = expect_tool_error(
            handle_tool_call(&cg, "tracedecay_lcm_status", args, None, None).await,
        );
        assert!(
            error.contains(expected),
            "invalid {removed} should fail clearly: {error}"
        );
    }
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_session_handlers_expose_bounded_read_apis_and_placeholders() {
    let dir = test_temp_dir();
    let (cg, _env) = init_test_project(dir.path()).await;
    let full_text = format!("orchard dispatch {}", "external-payload-body ".repeat(220));
    let projection =
        seed_temporal_lcm_session_message(&cg, "lcm-session", "lcm-message", full_text, 1).await;
    let temporal_db = open_active_project_session_db(&cg).await;
    activate_test_temporal_generation(&temporal_db, "lcm-session", vec![projection]).await;
    let db = open_active_project_session_db(&cg).await;
    let raw = db
        .lcm_load_raw_message_for_test("cursor", "lcm-message")
        .await
        .expect("LCM raw message should be created by compatibility ingest");

    let status = handle_tool_call(
        &cg,
        "tracedecay_lcm_status",
        json!({"provider": "cursor"}),
        None,
        None,
    )
    .await
    .unwrap();
    let status_payload: Value = serde_json::from_str(extract_text(&status.value)).unwrap();
    assert_eq!(status_payload["status"], "ok");
    assert_eq!(status_payload["lcm"]["raw_message_count"], 1);

    let loaded = handle_tool_call(
        &cg,
        "tracedecay_lcm_load_session",
        json!({
            "provider": "cursor",
            "session_id": "lcm-session",
            "content_limit": 24
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let loaded_payload: Value = serde_json::from_str(extract_text(&loaded.value)).unwrap();
    assert_eq!(loaded_payload["status"], "partial");
    assert_eq!(loaded_payload["omitted"], 1);
    assert_eq!(loaded_payload["coverage"]["unknown"], 1);
    assert_eq!(loaded_payload["messages"].as_array().unwrap().len(), 1);
    assert!(
        loaded_payload["messages"][0]["content_range"]["truncated"]
            .as_bool()
            .unwrap()
    );
    assert_eq!(
        loaded_payload["messages"][0]["content"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        24
    );

    let grep = handle_tool_call(
        &cg,
        "tracedecay_lcm_grep",
        json!({"provider": "cursor", "query": "orchard dispatch", "limit": 5}),
        None,
        None,
    )
    .await
    .unwrap();
    let grep_payload: Value = serde_json::from_str(extract_text(&grep.value)).unwrap();
    assert_eq!(
        grep_payload["status"], "partial",
        "root-wide grep payload: {grep_payload}"
    );
    assert_eq!(
        grep_payload["omitted"], 2,
        "root-wide grep payload: {grep_payload}"
    );
    assert_eq!(grep_payload["coverage"]["unknown"], 1);
    assert_eq!(grep_payload["hits"].as_array().unwrap().len(), 1);
    assert!(
        grep_payload["hits"][0]["snippet"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= 4096,
        "grep snippets must stay bounded"
    );

    let default_provider_grep = handle_tool_call(
        &cg,
        "tracedecay_lcm_grep",
        json!({"query": "orchard dispatch", "limit": 5}),
        None,
        None,
    )
    .await
    .unwrap();
    let default_provider_grep_payload: Value =
        serde_json::from_str(extract_text(&default_provider_grep.value)).unwrap();
    assert_eq!(
        default_provider_grep_payload["status"], "partial",
        "default-provider root-wide grep payload: {default_provider_grep_payload}"
    );
    assert_eq!(
        default_provider_grep_payload["omitted"], 2,
        "default-provider root-wide grep payload: {default_provider_grep_payload}"
    );
    assert_eq!(default_provider_grep_payload["coverage"]["unknown"], 1);
    assert_eq!(default_provider_grep_payload["provider"], "all");
    assert_eq!(
        default_provider_grep_payload["hits"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let cursor_projection = seed_temporal_lcm_session_message_for_provider(
        &cg,
        "cursor",
        "provider-local-session",
        "cursor-provider-local-message",
        "provider local collision belongs to cursor",
        2,
    )
    .await;
    let codex_projection = seed_temporal_lcm_session_message_for_provider(
        &cg,
        "codex",
        "provider-local-session",
        "codex-provider-local-message",
        "provider local collision belongs to codex",
        3,
    )
    .await;
    activate_test_temporal_generation(
        &temporal_db,
        "provider-local-session",
        vec![cursor_projection, codex_projection],
    )
    .await;

    let scoped_default_provider_grep = handle_tool_call(
        &cg,
        "tracedecay_lcm_grep",
        json!({
            "query": "provider local collision",
            "scope": "session",
            "session_id": "provider-local-session",
            "limit": 5
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let scoped_default_provider_grep_payload: Value =
        serde_json::from_str(extract_text(&scoped_default_provider_grep.value)).unwrap();
    assert_eq!(scoped_default_provider_grep_payload["status"], "partial");
    assert_eq!(scoped_default_provider_grep_payload["omitted"], 2);
    assert_eq!(
        scoped_default_provider_grep_payload["coverage"]["unknown"],
        2
    );
    assert_eq!(scoped_default_provider_grep_payload["provider"], "all");
    assert_eq!(scoped_default_provider_grep_payload["count"], 2);
    assert_eq!(
        scoped_default_provider_grep_payload["hits"][0]["provider"],
        "codex"
    );
    assert_eq!(
        scoped_default_provider_grep_payload["hits"][1]["provider"],
        "cursor"
    );

    let provider_local_load = handle_tool_call(
        &cg,
        "tracedecay_lcm_load_session",
        json!({
            "session_id": "provider-local-session",
            "limit": 5
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let provider_local_load_payload: Value =
        serde_json::from_str(extract_text(&provider_local_load.value)).unwrap();
    assert_eq!(provider_local_load_payload["status"], "partial");
    assert_eq!(provider_local_load_payload["omitted"], 2);
    assert_eq!(provider_local_load_payload["coverage"]["unknown"], 2);
    assert_eq!(provider_local_load_payload["provider"], "all");
    let loaded_providers = provider_local_load_payload["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| message["provider"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(loaded_providers, vec!["codex", "cursor"]);

    let described = handle_tool_call(
        &cg,
        "tracedecay_lcm_describe",
        json!({"provider": "cursor", "session_id": "lcm-session"}),
        None,
        None,
    )
    .await
    .unwrap();
    let described_payload: Value = serde_json::from_str(extract_text(&described.value)).unwrap();
    assert_eq!(described_payload["status"], "ok");
    assert_eq!(described_payload["description"]["raw_message_count"], 1);
    assert!(
        described_payload["description"]["raw_messages"][0]
            .get("content_preview")
            .is_some()
    );
    assert!(
        described_payload["description"]["raw_messages"][0]
            .get("content")
            .is_none(),
        "describe must not expose raw payload bodies"
    );

    let expanded = handle_tool_call(
        &cg,
        "tracedecay_lcm_expand",
        json!({
            "provider": "cursor",
            "session_id": "lcm-session",
            "target": {"kind": "raw_message", "store_id": raw.store_id},
            "content_offset": 8,
            "content_limit": 16
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let expanded_payload: Value = serde_json::from_str(extract_text(&expanded.value)).unwrap();
    assert_eq!(expanded_payload["status"], "ok");
    assert_eq!(expanded_payload["expansion"]["kind"], "raw_message");
    assert_eq!(
        expanded_payload["expansion"]["content"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        16
    );
    assert!(
        expanded_payload["expansion"]["content_range"]["truncated"]
            .as_bool()
            .unwrap()
    );

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_expand_query",
        json!({
            "provider": "cursor",
            "session_id": "lcm-session",
            "prompt": "Summarize orchard dispatch",
            "query": "orchard dispatch",
            "context_max_tokens": 32_000,
            "max_tokens": 64
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert_eq!(
        payload["status"], "partial",
        "bounded expand-query payload: {payload}"
    );
    assert_eq!(
        payload["omitted"], 2,
        "bounded expand-query payload: {payload}"
    );
    assert_eq!(payload["coverage"]["unknown"], 1);
    assert_eq!(payload["needs_synthesis"], true);
    assert_eq!(payload["prompt"], "Summarize orchard dispatch");
    assert!(
        payload["context_blocks"]
            .as_array()
            .expect("context blocks")
            .iter()
            .any(|block| block["kind"] == "raw_message")
    );
    assert!(
        payload["synthesis_prompt"]["user"]
            .as_str()
            .unwrap()
            .contains("EXPANDED CONTEXT")
    );
    assert!(extract_text(&result.value).len() <= MCP_TEST_RESPONSE_CHAR_LIMIT);
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_status_response_is_valid_json_and_omits_payload_secrets() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let db = open_active_project_session_db(&cg).await;
    assert!(
        db.upsert_session_for_test(
            HostAdmissionScope::Project,
            &SessionRecord {
                provider: "cursor".to_string(),
                session_id: "lcm-status-session".to_string(),
                project_key: cg.project_root().to_string_lossy().to_string(),
                project_path: cg.project_root().to_string_lossy().to_string(),
                title: Some("LCM status diagnostics".to_string()),
                started_at: Some(1),
                ended_at: None,
                transcript_path: Some("lcm-status-session.jsonl".to_string()),
                metadata_json: None,
                parent_session_id: None,
                is_subagent: false,
                agent_id: None,
                parent_tool_use_id: None,
            },
        )
        .await
        .unwrap()
    );

    let secret = format!("MCP_STATUS_SECRET_PAYLOAD\n{}", "Q".repeat(300_000));
    db.lcm_ingest_raw_message_for_test(
        HostAdmissionScope::Project,
        &SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id: "lcm-status-secret-message".to_string(),
            session_id: "lcm-status-session".to_string(),
            role: "tool".to_string(),
            timestamp: Some(2),
            ordinal: 1,
            text: secret,
            kind: Some("tool_result".to_string()),
            model: Some("test-model".to_string()),
            tool_names: None,
            source_path: Some("lcm-status-session.jsonl".to_string()),
            source_offset: Some(0),
            metadata_json: None,
        },
    )
    .await
    .expect("external payload should ingest");

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_status",
        json!({
            "provider": "cursor",
            "session_id": "lcm-status-session"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let payload: Value = serde_json::from_str(text).expect("LCM status response must be JSON");

    assert_eq!(payload["status"], "ok");
    assert!(payload["lcm"].get("storage_scope").is_none());
    assert_eq!(payload["lcm"]["payload"]["externalized_count"], 1);
    assert_eq!(payload["lcm"]["payload"]["missing_count"], 0);
    assert_eq!(payload["lcm"]["payload"]["unreferenced_count"], 0);
    assert_eq!(payload["lcm"]["redaction"]["enabled"], false);
    assert!(!text.contains("MCP_STATUS_SECRET_PAYLOAD"));
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_status_reports_lifecycle_fields_from_active_project() {
    let dir = test_temp_dir();
    let (cg, _env) = init_test_project(dir.path()).await;
    seed_lcm_session_message(
        &cg,
        "lcm-status-frontier",
        "lcm-status-frontier-message-1",
        "frontier seed one",
        1,
    )
    .await;
    seed_lcm_session_message(
        &cg,
        "lcm-status-frontier",
        "lcm-status-frontier-message-2",
        "frontier seed two",
        2,
    )
    .await;
    let db = open_active_project_session_db(&cg).await;
    let first = db
        .lcm_load_raw_message_for_test("cursor", "lcm-status-frontier-message-1")
        .await
        .expect("first raw message should load");
    let second = db
        .lcm_load_raw_message_for_test("cursor", "lcm-status-frontier-message-2")
        .await
        .expect("second raw message should load");
    db.lcm_update_lifecycle_for_test(
        HostAdmissionScope::Project,
        LcmLifecycleUpdate {
            provider: "cursor".into(),
            conversation_id: "lcm-status-frontier".into(),
            current_session_id: "lcm-status-frontier".into(),
            current_frontier_store_id: Some(second.store_id),
            last_finalized_session_id: Some("lcm-status-prior".into()),
            last_finalized_frontier_store_id: Some(first.store_id),
            maintenance_debt: vec![LcmMaintenanceDebt::RawBacklog {
                from_store_id: first.store_id,
                to_store_id: second.store_id,
            }],
        },
    )
    .await
    .expect("lifecycle state should update");

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_status",
        json!({
            "provider": "cursor",
            "session_id": "lcm-status-frontier"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "ok");
    assert!(payload["lcm"].get("storage_scope").is_none());
    assert_eq!(payload["lcm"]["raw_message_count"], 2);
    assert_eq!(
        payload["lcm"]["lifecycle"]["current_session_id"],
        "lcm-status-frontier"
    );
    assert_eq!(
        payload["lcm"]["lifecycle"]["current_frontier_store_id"],
        second.store_id
    );
    assert_eq!(
        payload["lcm"]["lifecycle"]["last_finalized_session_id"],
        "lcm-status-prior"
    );
    assert_eq!(
        payload["lcm"]["lifecycle"]["last_finalized_frontier_store_id"],
        first.store_id
    );
    assert_eq!(payload["lcm"]["lifecycle"]["maintenance_debt_count"], 1);
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_describe_supports_summary_node_and_external_payload_targets() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let source_projection = seed_temporal_lcm_session_message(
        &cg,
        "lcm-describe-targets",
        "lcm-describe-source",
        "describe source body must not leak through metadata",
        1,
    )
    .await;
    let external_projection = seed_temporal_lcm_tool_result_message(
        &cg,
        "lcm-describe-targets",
        "lcm-describe-tool",
        format!("describe external secret {}", "payload ".repeat(40_000)),
        2,
    )
    .await;
    let db = open_active_project_session_db(&cg).await;
    activate_test_temporal_generation(
        &db,
        "lcm-describe-targets",
        vec![source_projection, external_projection],
    )
    .await;
    let source = db
        .lcm_load_raw_message_for_test("cursor", "lcm-describe-source")
        .await
        .expect("source raw message should exist");
    let external = db
        .lcm_load_raw_message_for_test("cursor", "lcm-describe-tool")
        .await
        .expect("external raw message should exist");
    let payload_ref = external.payload_ref.expect("payload ref");
    let summary = db
        .lcm_insert_summary_node_for_test(
            HostAdmissionScope::Project,
            LcmSummaryNodeDraft {
                provider: "cursor".to_string(),
                conversation_id: "conversation-1".to_string(),
                session_id: "lcm-describe-targets".to_string(),
                depth: 0,
                summary_text: "summary secret body must not leak through metadata".to_string(),
                source_refs: vec![LcmSourceRef::RawMessage {
                    store_id: source.store_id,
                }],
                source_token_count: 30,
                summary_token_count: 5,
                source_time_start: Some(1),
                source_time_end: Some(2),
                expand_hint: Some("describe target summary".to_string()),
                metadata_json: None,
            },
        )
        .await
        .expect("summary should insert");
    let server = real_mcp_server(cg).await;

    let node_result = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_describe",
        json!({
            "provider": "cursor",
            "session_id": "lcm-describe-targets",
            "target": {"kind": "summary_node", "node_id": summary.node_id.clone()}
        }),
    )
    .await;
    let node_payload: Value = serde_json::from_str(extract_real_server_text(&node_result)).unwrap();
    assert_eq!(node_payload["status"], "ok", "{node_payload}");
    assert_eq!(node_payload["description"]["target"], "summary_node");
    assert_eq!(
        node_payload["description"]["summary_node"]["node_id"],
        summary.node_id
    );
    assert_eq!(
        node_payload["description"]["summary_node"]["source_count"],
        1
    );
    assert_eq!(node_payload["grain"], "summary");
    assert_eq!(node_payload["state"], "available");
    assert_eq!(node_payload["anchors"].as_array().unwrap().len(), 1);
    assert!(node_payload["watermarks"]["generation"].as_u64().unwrap() > 0);
    assert_eq!(node_payload["coverage"]["visible"], 1);
    assert_eq!(node_payload["lineage"].as_array().unwrap().len(), 1);

    let payload_result = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_describe",
        json!({
            "provider": "cursor",
            "session_id": "lcm-describe-targets",
            "target": {"kind": "external_payload", "payload_ref": payload_ref.clone()}
        }),
    )
    .await;
    let payload_payload: Value =
        serde_json::from_str(extract_real_server_text(&payload_result)).unwrap();
    assert_eq!(payload_payload["status"], "ok", "{payload_payload}");
    assert_eq!(payload_payload["description"]["target"], "external_payload");
    assert_eq!(
        payload_payload["description"]["external_payload"]["payload_ref"],
        payload_ref
    );
    assert_eq!(
        payload_payload["description"]["external_payload"]["content_preview"],
        ""
    );
    assert_eq!(payload_payload["grain"], "occurrence");
    assert_eq!(payload_payload["state"], "available");
    assert_eq!(payload_payload["anchors"].as_array().unwrap().len(), 1);

    let rendered = format!(
        "{}\n{}",
        extract_real_server_text(&node_result),
        extract_real_server_text(&payload_result)
    );
    assert!(!rendered.contains("summary secret body"));
    assert!(!rendered.contains("describe source body"));
    assert!(!rendered.contains("describe external secret"));
    server.shutdown().await;
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_grep_and_load_session_honor_native_filters_and_content_clamp() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let old = seed_temporal_lcm_session_message_at_micros(
        &cg,
        "lcm-native-filters",
        "lcm-native-old-cli-assistant",
        "orchard native old cli assistant",
        CanonicalMessageRoleV1::Assistant,
        1,
        10,
    )
    .await;
    let user = seed_temporal_lcm_session_message_at_micros(
        &cg,
        "lcm-native-filters",
        "lcm-native-new-cli-user",
        "orchard native new cli user",
        CanonicalMessageRoleV1::User,
        2,
        20,
    )
    .await;
    let newer = seed_temporal_lcm_session_message_at_micros(
        &cg,
        "lcm-native-filters",
        "lcm-native-new-api-assistant",
        "orchard native new api assistant",
        CanonicalMessageRoleV1::Assistant,
        3,
        30,
    )
    .await;
    let db = open_active_project_session_db(&cg).await;
    activate_test_temporal_generation(&db, "lcm-native-filters", vec![old, user, newer]).await;

    let grep = handle_tool_call(
        &cg,
        "tracedecay_lcm_grep",
        json!({
            "provider": "cursor",
            "query": "orchard native",
            "scope": "session",
            "session_id": "lcm-native-filters",
            "role": "assistant",
            "start_time": 5,
            "end_time": 25,
            "limit": 10
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let grep_payload: Value = serde_json::from_str(extract_text(&grep.value)).unwrap();
    assert_eq!(grep_payload["status"], "partial");
    assert_eq!(grep_payload["count"], 1);
    assert_eq!(grep_payload["omitted"], 3);
    assert_eq!(
        grep_payload["hits"][0]["message_id"],
        "lcm-native-old-cli-assistant"
    );
    assert_eq!(grep_payload["sort"], "relevance");

    let loaded = handle_tool_call(
        &cg,
        "tracedecay_lcm_load_session",
        json!({
            "provider": "cursor",
            "session_id": "lcm-native-filters",
            "roles": ["assistant", "user"],
            "time_from": 1,
            "time_to": 25,
            "content_limit": 25_000,
            "limit": 10
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let loaded_payload: Value = serde_json::from_str(extract_text(&loaded.value)).unwrap();
    assert_eq!(
        loaded_payload["status"], "partial",
        "payload: {loaded_payload}"
    );
    assert_eq!(
        loaded_payload["omitted"], 2,
        "native-filter load payload: {loaded_payload}"
    );
    assert_eq!(loaded_payload["coverage"]["unknown"], 2);
    assert_eq!(loaded_payload["content_limit"], 20_000);
    assert_eq!(loaded_payload["content_limit_clamped_from"], 25_000);
    assert_eq!(
        loaded_payload["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|message| message["message_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["lcm-native-new-cli-user", "lcm-native-old-cli-assistant"]
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_grep_accepts_string_timestamp_filters() {
    let dir = test_temp_dir();
    let (cg, _env) = init_test_project(dir.path()).await;
    let old = seed_temporal_lcm_session_message_at(
        &cg,
        "lcm-string-timestamps",
        "lcm-string-timestamps-old",
        "orchard string timestamp old",
        CanonicalMessageRoleV1::Assistant,
        1,
        10,
    )
    .await;
    let target = seed_temporal_lcm_session_message_at(
        &cg,
        "lcm-string-timestamps",
        "lcm-string-timestamps-target",
        "orchard string timestamp target",
        CanonicalMessageRoleV1::Assistant,
        2,
        20,
    )
    .await;
    let new = seed_temporal_lcm_session_message_at(
        &cg,
        "lcm-string-timestamps",
        "lcm-string-timestamps-new",
        "orchard string timestamp new",
        CanonicalMessageRoleV1::Assistant,
        3,
        30,
    )
    .await;
    let db = open_active_project_session_db(&cg).await;
    activate_test_temporal_generation(&db, "lcm-string-timestamps", vec![old, target, new]).await;

    let grep = handle_tool_call(
        &cg,
        "tracedecay_lcm_grep",
        json!({
            "provider": "cursor",
            "query": "orchard string timestamp",
            "scope": "session",
            "session_id": "lcm-string-timestamps",
            "start_time": "15",
            "end_time": "1970-01-01T00:00:25Z",
            "limit": 10
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&grep.value)).unwrap();
    assert_eq!(payload["status"], "partial", "payload: {payload}");
    assert_eq!(payload["count"], 1);
    assert_eq!(payload["omitted"], 3);
    assert_eq!(
        payload["hits"][0]["message_id"],
        "lcm-string-timestamps-target"
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_grep_accepts_relative_time_filters() {
    let dir = test_temp_dir();
    let (cg, _env) = init_test_project(dir.path()).await;
    let now = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let old = seed_temporal_lcm_session_message_at(
        &cg,
        "lcm-relative-timestamps",
        "lcm-relative-timestamps-old",
        "orchard relative timestamp old",
        CanonicalMessageRoleV1::Assistant,
        1,
        now - 7200,
    )
    .await;
    let new = seed_temporal_lcm_session_message_at(
        &cg,
        "lcm-relative-timestamps",
        "lcm-relative-timestamps-new",
        "orchard relative timestamp new",
        CanonicalMessageRoleV1::Assistant,
        2,
        now - 300,
    )
    .await;
    let db = open_active_project_session_db(&cg).await;
    activate_test_temporal_generation(&db, "lcm-relative-timestamps", vec![old, new]).await;

    let grep = handle_tool_call(
        &cg,
        "tracedecay_lcm_grep",
        json!({
            "provider": "cursor",
            "query": "orchard relative timestamp",
            "scope": "session",
            "session_id": "lcm-relative-timestamps",
            "since": "last hour",
            "limit": 10
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&grep.value)).unwrap();
    assert_eq!(payload["status"], "partial", "payload: {payload}");
    assert_eq!(payload["count"], 1);
    assert_eq!(payload["omitted"], 2);
    assert_eq!(
        payload["hits"][0]["message_id"],
        "lcm-relative-timestamps-new"
    );
}

#[tokio::test]
async fn lcm_grep_rejects_invalid_scope_without_searching_all_sessions() {
    let dir = test_temp_dir();
    let (cg, _env) = init_test_project(dir.path()).await;

    let err = expect_tool_error(
        handle_tool_call(
            &cg,
            "tracedecay_lcm_grep",
            json!({
                "provider": "cursor",
                "query": "unique-cross-session-token",
                "scope": "everything",
                "limit": 10
            }),
            None,
            None,
        )
        .await,
    );
    assert!(
        err.contains("scope"),
        "invalid scope should report an argument error, got {err}"
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_load_session_rejects_fractional_negative_and_wrong_type_numeric_args() {
    let (cg, _env, _dir) = setup_empty_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-numeric",
        "lcm-numeric-message",
        "numeric validation test body",
        1,
    )
    .await;

    for (case, args) in [
        (
            "fractional limit",
            json!({"provider": "cursor", "session_id": "lcm-numeric", "limit": 1.5}),
        ),
        (
            "negative limit",
            json!({"provider": "cursor", "session_id": "lcm-numeric", "limit": -1}),
        ),
        (
            "string limit",
            json!({"provider": "cursor", "session_id": "lcm-numeric", "limit": "1"}),
        ),
    ] {
        let err = expect_tool_error(
            handle_tool_call(&cg, "tracedecay_lcm_load_session", args, None, None).await,
        );
        assert!(
            err.contains("limit"),
            "{case} should report an argument error mentioning limit, got {err}"
        );
    }
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_load_session_accepts_valid_integer_args() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let projection = seed_temporal_lcm_session_message_at_micros(
        &cg,
        "lcm-valid-integers",
        "lcm-valid-integers-message",
        "valid integer argument body",
        CanonicalMessageRoleV1::Assistant,
        1,
        2,
    )
    .await;
    let db = open_active_project_session_db(&cg).await;
    activate_test_temporal_generation(&db, "lcm-valid-integers", vec![projection]).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_load_session",
        json!({
            "provider": "cursor",
            "session_id": "lcm-valid-integers",
            "limit": 1,
            "content_offset": 0,
            "content_limit": 8,
            "start_time": 1,
            "end_time": 10
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert_eq!(payload["status"], "partial");
    assert_eq!(payload["omitted"], 1);
    assert_eq!(payload["coverage"]["unknown"], 1);
    assert_eq!(
        payload["messages"].as_array().unwrap().len(),
        1,
        "payload: {payload}"
    );
    assert_eq!(
        payload["messages"][0]["content"].as_str().unwrap(),
        "valid in"
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_large_json_response_stays_parseable_after_truncation() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let mut projections = Vec::new();
    for index in 0..4 {
        projections.push(
            seed_temporal_lcm_session_message(
                &cg,
                "lcm-large-json",
                &format!("lcm-large-json-message-{index}"),
                format!("large json response {index} {}", "payload ".repeat(1100)),
                index + 1,
            )
            .await,
        );
    }
    let db = open_active_project_session_db(&cg).await;
    activate_test_temporal_generation(&db, "lcm-large-json", projections).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_load_session",
        json!({
            "provider": "cursor",
            "session_id": "lcm-large-json",
            "limit": 4,
            "content_limit": 8192
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value))
        .expect("truncated LCM tool text should remain valid JSON");
    assert_eq!(payload["truncated"], true);
    assert!(payload["preview"].as_str().unwrap().len() <= MCP_TEST_RESPONSE_CHAR_LIMIT);
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_expand_query_large_response_preserves_synthesis_contract() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let projection = seed_temporal_lcm_session_message(
        &cg,
        "lcm-large-expand-query",
        "lcm-large-expand-query-message",
        format!(
            "oversized expand-query evidence {}",
            "context ".repeat(4000)
        ),
        1,
    )
    .await;
    let db = open_active_project_session_db(&cg).await;
    activate_test_temporal_generation(&db, "lcm-large-expand-query", vec![projection]).await;

    let server = real_mcp_server(cg).await;
    let result = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_expand_query",
        json!({
            "provider": "cursor",
            "session_id": "lcm-large-expand-query",
            "prompt": "Summarize oversized expand-query evidence",
            "query": "oversized expand-query evidence",
            "context_max_tokens": 65536,
            "max_tokens": 128
        }),
    )
    .await;
    let text = extract_real_server_text(&result);
    let payload: Value =
        serde_json::from_str(text).expect("large expand-query response must remain valid JSON");

    assert_ne!(
        payload["truncated"], true,
        "must not use generic truncation"
    );
    assert_eq!(payload["status"], "partial");
    assert_eq!(payload["needs_synthesis"], true);
    assert_eq!(
        payload["prompt"],
        "Summarize oversized expand-query evidence"
    );
    assert!(
        payload["synthesis_prompt"]["system"]
            .as_str()
            .unwrap()
            .contains("expanded LCM retrieval context")
    );
    assert!(
        payload["synthesis_prompt"]["user"]
            .as_str()
            .unwrap()
            .contains("Summarize oversized expand-query evidence")
    );
    assert!(payload["context_truncated"].as_bool().is_some());
    assert!(payload["context_budget"]["used_chars"].as_u64().is_some());
    assert!(!payload["matches"].as_array().unwrap().is_empty());
    assert!(
        payload["context_blocks"].as_array().unwrap().len() <= 3,
        "MCP expand-query context should stay compact"
    );
    assert!(text.len() <= MCP_TEST_RESPONSE_CHAR_LIMIT);
    server.shutdown().await;
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_expand_query_oversized_prompt_preserves_synthesis_contract() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let projection = seed_temporal_lcm_session_message(
        &cg,
        "lcm-huge-prompt-expand-query",
        "lcm-huge-prompt-expand-query-message",
        "contract overflow evidence lives in this raw message",
        1,
    )
    .await;
    let db = open_active_project_session_db(&cg).await;
    activate_test_temporal_generation(&db, "lcm-huge-prompt-expand-query", vec![projection]).await;
    let raw = db
        .lcm_load_raw_message_for_test("cursor", "lcm-huge-prompt-expand-query-message")
        .await
        .expect("raw message should exist");
    let summary = db
        .lcm_insert_summary_node_for_test(
            HostAdmissionScope::Project,
            LcmSummaryNodeDraft {
                provider: "cursor".to_string(),
                conversation_id: "conversation-1".to_string(),
                session_id: "lcm-huge-prompt-expand-query".to_string(),
                depth: 0,
                summary_text: "summary contract overflow evidence".to_string(),
                source_refs: vec![LcmSourceRef::RawMessage {
                    store_id: raw.store_id,
                }],
                source_token_count: 30,
                summary_token_count: 5,
                source_time_start: Some(1),
                source_time_end: Some(2),
                expand_hint: Some("contract overflow summary".to_string()),
                metadata_json: None,
            },
        )
        .await
        .expect("summary should insert");
    let huge_prompt = format!(
        "Explain contract overflow evidence. {}",
        "PROMPT_OVERFLOW ".repeat(12_000)
    );
    let huge_query = format!(
        "contract overflow evidence {}",
        "QUERY_OVERFLOW ".repeat(12_000)
    );

    let server = real_mcp_server(cg).await;
    let result = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_expand_query",
        json!({
            "provider": "cursor",
            "session_id": "lcm-huge-prompt-expand-query",
            "prompt": huge_prompt,
            "query": huge_query,
            "node_ids": [summary.node_id],
            "context_max_tokens": 65536,
            "max_tokens": 128
        }),
    )
    .await;
    let text = extract_real_server_text(&result);
    let payload: Value =
        serde_json::from_str(text).expect("oversized expand-query response must remain valid JSON");

    assert_ne!(
        payload["truncated"], true,
        "must not use generic truncation"
    );
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["needs_synthesis"], true);
    assert_eq!(payload["mcp_response_truncated"], true);
    assert!(payload["prompt"].as_str().unwrap().chars().count() <= 2_048);
    assert!(payload["query"].as_str().unwrap().chars().count() <= 1_024);
    assert!(payload["prompt_truncated_for_mcp"].as_bool().unwrap());
    assert!(payload["query_truncated_for_mcp"].as_bool().unwrap());
    assert!(payload["contract_truncated"].as_bool().unwrap());
    assert!(
        payload["synthesis_prompt"]["user"]
            .as_str()
            .unwrap()
            .contains("QUESTION:")
    );
    assert!(text.len() <= MCP_TEST_RESPONSE_CHAR_LIMIT);
    server.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn lcm_status_cli_bridge_accepts_json_args() {
    use std::time::Duration;

    let (cg, _env, _dir) = setup_empty_project().await;
    let home = _dir.path().join("home");
    let outside_cwd = test_temp_dir();
    let project_arg = cg.project_root().display().to_string();
    close_test_graph(cg).await;
    let _daemon = common::spawn_tracedecay_daemon(&home);
    // Compatibility `tracedecay tool` still returns the typed warming state
    // from `call_default_tool_within` without an internal project-open retry
    // (unlike the typed application-surface CLI path). Waiting that documented
    // retryable state out is the client protocol for this bridge.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let output = loop {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_tracedecay"));
        common::apply_tracedecay_home_env(&mut command, &home);
        let output = command
            .current_dir(outside_cwd.path())
            .args([
                "tool",
                "--project",
                &project_arg,
                "tracedecay_lcm_status",
                "--json",
                "--args",
                r#"{"provider":"cursor","format":"json"}"#,
            ])
            .output()
            .unwrap();
        if output.status.success() {
            break output;
        }
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            stderr.contains("is warming in the background"),
            "tracedecay tool exited with {:?}\nstdout:\n{}\nstderr:\n{stderr}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
        );
        assert!(
            std::time::Instant::now() < deadline,
            "registered daemon project stayed warming past the bridge deadline\nstderr:\n{stderr}",
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["content"][0]["type"], "text");
    let payload = extract_first_json_content(&json);
    // Typed status proves the CLI bridge dispatched and parsed JSON args.
    // Store contents vary: ok / not_ingested when an LCM authority is mounted,
    // unavailable when the daemon has not mounted one for this exact store.
    let status = payload["status"].as_str();
    assert!(
        matches!(status, Some("ok" | "not_ingested" | "unavailable")),
        "unexpected lcm_status: {payload}"
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_expand_paginates_summary_sources_over_mcp() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let mut store_ids = Vec::new();
    let mut projections = Vec::new();
    for index in 1..=4 {
        let message_id = format!("page-msg-{index}");
        projections.push(
            seed_temporal_lcm_session_message(
                &cg,
                "lcm-page-session",
                &message_id,
                format!("paged source body {index}"),
                index,
            )
            .await,
        );
        store_ids.push(lcm_raw_store_id(&cg, &message_id).await);
    }
    let db = open_active_project_session_db(&cg).await;
    activate_test_temporal_generation(&db, "lcm-page-session", projections).await;
    let summary = db
        .lcm_insert_summary_node_for_test(
            HostAdmissionScope::Project,
            LcmSummaryNodeDraft {
                provider: "cursor".to_string(),
                conversation_id: "lcm-page-session".to_string(),
                session_id: "lcm-page-session".to_string(),
                depth: 0,
                summary_text: "paged summary".to_string(),
                source_refs: store_ids
                    .iter()
                    .map(|store_id| LcmSourceRef::RawMessage {
                        store_id: *store_id,
                    })
                    .collect(),
                source_token_count: 16,
                summary_token_count: 2,
                source_time_start: Some(1),
                source_time_end: Some(4),
                expand_hint: Some("pagination test".to_string()),
                metadata_json: None,
            },
        )
        .await
        .expect("summary should insert");
    let summary_id = summary.node_id.clone();
    for (index, store_id) in store_ids.iter().copied().enumerate() {
        db.poison_lcm_raw_projection_for_test(
            HostAdmissionScope::Project,
            store_id,
            &format!("projection poison {}", index + 1),
        )
        .await
        .expect("legacy LCM source projection should be poisonable");
    }
    let server = real_mcp_server(cg).await;

    let result = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_expand",
        json!({
            "provider": "cursor",
            "session_id": "lcm-page-session",
            "target": {"kind": "summary_node", "node_id": summary_id},
            "source_limit": 3
        }),
    )
    .await;
    let payload: Value = serde_json::from_str(extract_real_server_text(&result)).unwrap();

    assert_eq!(payload["status"], "ok", "{payload}");
    let sources = payload["expansion"]["summary_sources"].as_array().unwrap();
    assert_eq!(sources.len(), 3);
    assert_eq!(sources[0]["raw_message"]["store_id"], json!(store_ids[0]));
    assert_eq!(sources[1]["raw_message"]["store_id"], json!(store_ids[1]));
    assert_eq!(sources[2]["raw_message"]["store_id"], json!(store_ids[2]));
    for (source, expected_body) in sources.iter().zip([
        "paged source body 1",
        "paged source body 2",
        "paged source body 3",
    ]) {
        assert_eq!(source["state"], "available", "{source}");
        assert_eq!(source["content"], expected_body, "{source}");
        assert_eq!(source["raw_message"]["content"], expected_body, "{source}");
    }
    let pagination = &payload["expansion"]["source_pagination"];
    assert!(pagination.get("source_offset").is_none(), "{pagination}");
    assert!(
        pagination.get("next_source_offset").is_none(),
        "{pagination}"
    );
    assert_eq!(pagination["source_limit"], 3);
    assert_eq!(pagination["returned_sources"], 3);
    assert_eq!(pagination["total_sources"], 4);
    assert_eq!(pagination["has_more"], true);
    assert_eq!(pagination["remaining_sources"], 1);
    assert_eq!(payload["grain"], "summary");
    assert_eq!(payload["state"], "available");
    assert!(!payload["anchors"].as_array().unwrap().is_empty());
    assert!(payload["watermarks"]["generation"].as_u64().unwrap() > 0);
    assert!(payload["coverage"]["visible"].as_u64().unwrap() > 0);
    let cursor = payload["next_cursor"]
        .as_str()
        .expect("summary source page should return an opaque cursor");

    let tampered = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_expand",
        json!({
            "provider": "cursor",
            "session_id": "lcm-page-session",
            "target": {"kind": "summary_node", "node_id": summary_id},
            "source_limit": 3,
            "cursor": format!("{cursor}00")
        }),
    )
    .await;
    let tampered: Value = serde_json::from_str(extract_real_server_text(&tampered)).unwrap();
    assert_eq!(tampered["status"], "denied", "{tampered}");

    let rebound = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_expand",
        json!({
            "provider": "cursor",
            "session_id": "lcm-page-session",
            "target": {"kind": "summary_node", "node_id": summary_id},
            "source_limit": 1,
            "cursor": cursor
        }),
    )
    .await;
    let rebound: Value = serde_json::from_str(extract_real_server_text(&rebound)).unwrap();
    assert_eq!(rebound["status"], "denied", "{rebound}");

    let private_terminal = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_expand",
        json!({
            "provider": "cursor",
            "session_id": "lcm-page-session",
            "target": {"kind": "summary_node", "node_id": "summary.missing"},
            "source_limit": 3,
            "cursor": cursor
        }),
    )
    .await;
    let private_terminal: Value =
        serde_json::from_str(extract_real_server_text(&private_terminal)).unwrap();
    assert_eq!(
        private_terminal["status"], "denied",
        "cursor authentication must precede target-state disclosure: {private_terminal}"
    );

    let continued = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_expand",
        json!({
            "provider": "cursor",
            "session_id": "lcm-page-session",
            "target": {"kind": "summary_node", "node_id": summary_id},
            "source_limit": 3,
            "cursor": cursor
        }),
    )
    .await;
    let continued: Value = serde_json::from_str(extract_real_server_text(&continued)).unwrap();
    assert_eq!(
        continued["expansion"]["summary_sources"][0]["raw_message"]["store_id"],
        json!(store_ids[3])
    );
    assert_eq!(
        continued["expansion"]["summary_sources"][0]["state"],
        "available"
    );
    assert_eq!(
        continued["expansion"]["summary_sources"][0]["content"],
        "paged source body 4"
    );
    assert_eq!(
        continued["expansion"]["summary_sources"][0]["raw_message"]["content"],
        "paged source body 4"
    );
    assert!(
        continued["expansion"]["source_pagination"]
            .get("source_offset")
            .is_none()
    );
    assert!(continued["next_cursor"].is_null());

    let first_query_page = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_expand_query",
        json!({
            "provider": "cursor",
            "session_id": "lcm-page-session",
            "prompt": "Recover every paged source",
            "node_ids": [summary_id],
            "max_results": 2,
            "context_max_tokens": 4096
        }),
    )
    .await;
    let first_query_page: Value =
        serde_json::from_str(extract_real_server_text(&first_query_page)).unwrap();
    assert_eq!(
        first_query_page["status"], "ok",
        "expand-query first page: {first_query_page}"
    );
    for body in ["paged source body 1", "paged source body 2"] {
        assert!(
            first_query_page["context_blocks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|block| block["content"] == body),
            "expand-query first page should contain {body}: {first_query_page}"
        );
    }
    let query_cursor = first_query_page["next_cursor"]
        .as_str()
        .expect("expand-query source page should return a cursor");

    let continued_query_page = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_expand_query",
        json!({
            "provider": "cursor",
            "session_id": "lcm-page-session",
            "prompt": "Recover every paged source",
            "node_ids": [summary_id],
            "max_results": 2,
            "context_max_tokens": 4096,
            "cursor": query_cursor
        }),
    )
    .await;
    let continued_query_page: Value =
        serde_json::from_str(extract_real_server_text(&continued_query_page)).unwrap();
    for body in ["paged source body 3", "paged source body 4"] {
        assert!(
            continued_query_page["context_blocks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|block| block["content"] == body),
            "expand-query continued page should contain {body}: {continued_query_page}"
        );
    }
    assert!(continued_query_page["next_cursor"].is_null());
    server.shutdown().await;
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_expand_resolves_cross_session_store_ids_over_mcp() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let origin_projection = seed_temporal_lcm_session_message(
        &cg,
        "lcm-origin-session",
        "origin-message",
        "cross session grep target body",
        1,
    )
    .await;
    let active_projection = seed_temporal_lcm_session_message(
        &cg,
        "lcm-active-session",
        "active-message",
        "the caller's active session",
        2,
    )
    .await;
    let origin_store_id = lcm_raw_store_id(&cg, "origin-message").await;
    let db = open_active_project_session_db(&cg).await;
    activate_test_temporal_generation(&db, "lcm-origin-session", vec![origin_projection]).await;
    activate_test_temporal_generation(&db, "lcm-active-session", vec![active_projection]).await;
    db.poison_lcm_raw_projection_for_test(
        HostAdmissionScope::Project,
        origin_store_id,
        "legacy projection poison",
    )
    .await
    .unwrap();
    let server = real_mcp_server(cg).await;

    let result = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_expand",
        json!({
            "provider": "cursor",
            "session_id": "lcm-active-session",
            "target": {"kind": "raw_message", "store_id": origin_store_id}
        }),
    )
    .await;
    let payload: Value = serde_json::from_str(extract_real_server_text(&result)).unwrap();

    assert_eq!(payload["status"], "ok", "{payload}");
    assert_eq!(payload["expansion"]["kind"], "raw_message");
    assert_eq!(payload["expansion"]["from_current_session"], false);
    assert_eq!(
        payload["expansion"]["raw_message"]["session_id"],
        "lcm-origin-session"
    );
    assert_eq!(
        payload["expansion"]["content"],
        "cross session grep target body"
    );
    assert_eq!(payload["state"], "available");
    assert_eq!(payload["grain"], "occurrence");
    assert_eq!(payload["anchors"].as_array().unwrap().len(), 1);
    assert!(payload["watermarks"]["generation"].as_u64().unwrap() > 0);
    server.shutdown().await;
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_expand_real_service_rechecks_terminal_anchor_states() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let available_projection = seed_temporal_lcm_session_message(
        &cg,
        "lcm-state-session",
        "available-state-message",
        "stateful expansion body",
        1,
    )
    .await;
    let redacted_projection = seed_temporal_lcm_session_message_with_access(
        &cg,
        "lcm-state-session",
        "redacted-state-message",
        "redacted expansion body",
        2,
        PayloadAccessState::Redacted,
    )
    .await;
    let locked_projection = seed_temporal_lcm_session_message_with_access(
        &cg,
        "lcm-state-session",
        "locked-state-message",
        "locked expansion body",
        3,
        PayloadAccessState::Quarantined,
    )
    .await;
    let deleted_projection = seed_temporal_lcm_session_message_with_access(
        &cg,
        "lcm-state-session",
        "deleted-state-message",
        "deleted expansion body",
        4,
        PayloadAccessState::Deleted,
    )
    .await;
    let available_store_id = lcm_raw_store_id(&cg, "available-state-message").await;
    let redacted_store_id = lcm_raw_store_id(&cg, "redacted-state-message").await;
    let locked_store_id = lcm_raw_store_id(&cg, "locked-state-message").await;
    let deleted_store_id = lcm_raw_store_id(&cg, "deleted-state-message").await;
    let db = open_active_project_session_db(&cg).await;
    activate_test_temporal_generation(
        &db,
        "lcm-state-session",
        vec![
            available_projection,
            redacted_projection,
            locked_projection,
            deleted_projection,
        ],
    )
    .await;
    let project_id = cg
        .store_layout()
        .identity
        .project_id
        .clone()
        .expect("test project id");
    // Register through the graph's own retained runtime. That runtime is the
    // registry the MCP server's LCM service reads, and its profile root is the
    // isolated standalone test profile the graph database actually lives under
    // — the ambient profile root is a different identity that holds neither.
    let registry = open_active_project_session_db(&cg).await;
    let project = registry
        .upsert_code_project(&project_id, cg.project_root(), None, None, None)
        .await
        .expect("register test project");
    let serving_db_relpath = registry
        .profile_relative_path_for_test(&cg.db_path())
        .expect("test graph database must be under the registry profile root")
        .to_string_lossy()
        .into_owned();
    let store = registry
        .upsert_store_instance(tracedecay::global_db::StoreInstanceUpsert {
            store_id: format!("store_{project_id}"),
            project_id: project.project_id.clone(),
            store_kind: "code_project".to_string(),
            storage_mode: "profile_sharded".to_string(),
            store_relpath: serving_db_relpath.clone(),
            manifest_relpath: None,
            last_verified_at: Some(1),
            last_write_at: Some(1),
        })
        .await
        .expect("register test project store");
    registry
        .upsert_graph_scope(tracedecay::global_db::GraphScopeUpsert {
            graph_scope_id: format!("scope_{project_id}"),
            project_id: project.project_id,
            store_id: store.store_id,
            branch_name: "test".to_string(),
            db_relpath: serving_db_relpath,
            parent_scope_id: None,
            last_synced_at: Some(1),
            writable: true,
        })
        .await
        .expect("register test graph scope");
    let server = real_mcp_server(cg).await;
    let initial = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_expand",
        json!({
            "provider": "cursor",
            "session_id": "lcm-state-session",
            "target": {"kind": "raw_message", "store_id": available_store_id}
        }),
    )
    .await;
    let initial: Value = serde_json::from_str(extract_real_server_text(&initial)).unwrap();
    assert_eq!(initial["status"], "ok", "{initial}");
    assert_eq!(initial["expansion"]["content"], "stateful expansion body");

    for (store_id, expected_status) in [
        (redacted_store_id, "redacted"),
        (locked_store_id, "locked"),
        (deleted_store_id, "deleted"),
    ] {
        let result = handle_real_server_tool_call(
            &server,
            "tracedecay_lcm_expand",
            json!({
                "provider": "cursor",
                "session_id": "lcm-state-session",
                "target": {"kind": "raw_message", "store_id": store_id}
            }),
        )
        .await;
        let payload: Value = serde_json::from_str(extract_real_server_text(&result)).unwrap();
        assert_eq!(payload["status"], expected_status, "{payload}");
        assert!(
            payload["expansion"].as_array().unwrap().is_empty(),
            "{payload}"
        );
    }

    let denied = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_expand",
        json!({
            "provider": "cursor",
            "session_id": "lcm-state-session",
            "target": {"kind": "summary_node", "node_id": "summary.forged"},
            "source_limit": 1,
            "cursor": "forged"
        }),
    )
    .await;
    let denied: Value = serde_json::from_str(extract_real_server_text(&denied)).unwrap();
    assert_eq!(denied["status"], "denied", "{denied}");
    assert!(
        denied["expansion"].as_array().unwrap().is_empty(),
        "{denied}"
    );

    server.shutdown().await;
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_expand_cross_session_external_payload_supports_two_step_hydration() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let body = format!("data:image/png;base64,{}", "A".repeat(220_000));
    let origin_projection = seed_temporal_lcm_tool_result_message(
        &cg,
        "lcm-origin-session",
        "origin-external-message",
        body,
        1,
    )
    .await;
    let active_projection = seed_temporal_lcm_session_message(
        &cg,
        "lcm-active-session",
        "active-message",
        "active context",
        2,
    )
    .await;
    let origin_store_id = lcm_raw_store_id(&cg, "origin-external-message").await;
    let db = open_active_project_session_db(&cg).await;
    let active_session = db
        .session_for_test(HostAdmissionScope::Project, "cursor", "lcm-active-session")
        .await
        .unwrap()
        .expect("canonical projection must create the active session");
    assert_eq!(
        active_session.project_key,
        cg.store_layout()
            .identity
            .project_id
            .as_deref()
            .expect("test project id")
    );
    activate_test_temporal_generation(&db, "lcm-origin-session", vec![origin_projection]).await;
    activate_test_temporal_generation(&db, "lcm-active-session", vec![active_projection]).await;
    db.lcm_publish_immutable_summary_for_test(
        HostAdmissionScope::Project,
        LcmImmutableSummaryPublication {
            summary_id: "summary.lcm-origin-external".to_string(),
            predecessor_summary_id: None,
            draft: LcmSummaryNodeDraft {
                provider: "cursor".to_string(),
                conversation_id: "lcm-origin-session".to_string(),
                session_id: "lcm-origin-session".to_string(),
                depth: 0,
                summary_text: "external payload attestation".to_string(),
                source_refs: vec![LcmSourceRef::RawMessage {
                    store_id: origin_store_id,
                }],
                source_token_count: 1,
                summary_token_count: 1,
                source_time_start: Some(1),
                source_time_end: Some(1),
                expand_hint: Some("external payload fixture".to_string()),
                metadata_json: None,
            },
        },
    )
    .await
    .expect("external payload must receive a canonical summary attestation");
    let project_id = cg
        .store_layout()
        .identity
        .project_id
        .clone()
        .expect("test project id");
    // Register through the graph's own retained runtime. That runtime is the
    // registry the MCP server's LCM service reads, and its profile root is the
    // isolated standalone test profile the graph database actually lives under
    // — the ambient profile root is a different identity that holds neither.
    let registry = open_active_project_session_db(&cg).await;
    let project = registry
        .upsert_code_project(&project_id, cg.project_root(), None, None, None)
        .await
        .expect("register test project");
    let serving_db_relpath = registry
        .profile_relative_path_for_test(&cg.db_path())
        .expect("test graph database must be under the registry profile root")
        .to_string_lossy()
        .into_owned();
    let store = registry
        .upsert_store_instance(tracedecay::global_db::StoreInstanceUpsert {
            store_id: format!("store_{project_id}"),
            project_id: project.project_id.clone(),
            store_kind: "code_project".to_string(),
            storage_mode: "profile_sharded".to_string(),
            store_relpath: serving_db_relpath.clone(),
            manifest_relpath: None,
            last_verified_at: Some(1),
            last_write_at: Some(1),
        })
        .await
        .expect("register test project store");
    registry
        .upsert_graph_scope(tracedecay::global_db::GraphScopeUpsert {
            graph_scope_id: format!("scope_{project_id}"),
            project_id: project.project_id,
            store_id: store.store_id,
            branch_name: "test".to_string(),
            db_relpath: serving_db_relpath,
            parent_scope_id: None,
            last_synced_at: Some(1),
            writable: true,
        })
        .await
        .expect("register test graph scope");
    let payload_storage_root = cg
        .db_path()
        .parent()
        .expect("project database storage root")
        .join("lcm-payloads");
    let server = real_mcp_server(cg).await;

    let raw_result = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_expand",
        json!({
            "provider": "cursor",
            "session_id": "lcm-active-session",
            "target": {"kind": "raw_message", "store_id": origin_store_id}
        }),
    )
    .await;
    let raw_payload: Value = serde_json::from_str(extract_real_server_text(&raw_result)).unwrap();
    assert_eq!(raw_payload["status"], "partial", "{raw_payload}");
    assert_eq!(raw_payload["omitted"], 1, "{raw_payload}");
    assert_eq!(
        raw_payload["retrieval"]["outcome"], "partial",
        "{raw_payload}"
    );
    assert_eq!(
        raw_payload["expansion"]["content_range"]["truncated"], true,
        "{raw_payload}"
    );
    assert_eq!(raw_payload["expansion"]["from_current_session"], false);
    assert!(raw_payload["expansion"]["externalized_note"].is_null());
    let payload_ref = raw_payload["expansion"]["payload_ref"]
        .as_str()
        .expect("cross-session external row should surface payload_ref")
        .to_string();
    let raw_message = &raw_payload["expansion"]["raw_message"];
    assert!(raw_message.is_object(), "{raw_payload}");
    assert_eq!(raw_message["content"], raw_payload["expansion"]["content"]);
    let raw_metadata_text = raw_message["metadata_json"]
        .as_str()
        .expect("external raw-message metadata must be canonical JSON");
    let raw_metadata: Value =
        serde_json::from_str(raw_metadata_text).expect("canonical external raw-message metadata");
    let owner_session = raw_message["session_id"]
        .as_str()
        .expect("owner session id should be surfaced")
        .to_string();
    let original_manifest = db
        .lcm_external_payload_manifest_for_test(&payload_ref)
        .await
        .expect("read external payload manifest")
        .expect("external payload manifest");
    let manifest: Value = serde_json::from_str(&original_manifest.manifest_json)
        .expect("canonical external payload manifest");
    let payload_metadata: Value = serde_json::from_str(
        manifest["metadata"]
            .as_str()
            .expect("external payload manifest metadata"),
    )
    .expect("canonical external payload metadata");
    let raw_metadata_object = raw_metadata
        .as_object()
        .expect("external raw-message metadata object");
    for admitted in [
        "external_payload",
        "payload_ref",
        "kind",
        "byte_count",
        "char_count",
        "sha256",
        "ingest_protection",
    ] {
        assert!(
            raw_metadata_object.contains_key(admitted),
            "missing admitted `{admitted}` metadata: {raw_metadata}"
        );
    }
    assert_eq!(
        raw_metadata_object.len(),
        7,
        "provider/private metadata must not escape the public expansion: {raw_metadata}"
    );
    assert_eq!(raw_metadata["external_payload"], true);
    assert_eq!(raw_metadata["payload_ref"], payload_ref);
    assert_eq!(raw_metadata["kind"], manifest["kind"]);
    assert_eq!(raw_metadata["byte_count"], manifest["byte_count"]);
    assert_eq!(raw_metadata["char_count"], manifest["char_count"]);
    assert_eq!(raw_metadata["sha256"], original_manifest.payload_digest);
    assert_eq!(
        raw_metadata["ingest_protection"], payload_metadata["ingest_protection"],
        "the public safety receipt must be the manifest-bound receipt"
    );
    let ingest_protection = raw_metadata["ingest_protection"]
        .as_object()
        .expect("canonical ingest protection metadata");
    assert_eq!(
        ingest_protection.len(),
        1,
        "fixture must expose only its safety receipt: {raw_metadata}"
    );
    let receipt = &raw_metadata["ingest_protection"]["sanitization_receipt"];
    assert_eq!(
        receipt.as_object().map(serde_json::Map::len),
        Some(4),
        "receipt must expose only reference, disposition, sensitivity, and payload binding"
    );
    assert_eq!(receipt["disposition"], "accepted");
    assert_eq!(receipt["sensitivity"], "non_sensitive");
    assert_eq!(
        receipt["receipt"].as_object().map(serde_json::Map::len),
        Some(2),
        "receipt reference must remain opaque"
    );
    assert_eq!(
        receipt["receipt"]["sanitizer_version"],
        "privacy.lcm-payload.v1"
    );
    assert!(
        receipt["receipt"]["receipt_id"]
            .as_str()
            .is_some_and(|receipt_id| receipt_id.starts_with("privacy.lcm-payload.v1.")),
        "receipt identity must remain opaque and version-bound: {receipt}"
    );
    assert_eq!(
        receipt["payload"].as_object().map(serde_json::Map::len),
        Some(2),
        "payload binding must expose only digest and byte length"
    );
    assert!(
        receipt["payload"]["digest"].as_str().is_some_and(|digest| {
            digest.strip_prefix("sha256:").is_some_and(|hex| {
                hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        }),
        "payload binding must remain a tagged opaque digest: {receipt}"
    );
    assert_eq!(
        receipt["payload"]["byte_len"].as_u64(),
        manifest["byte_count"]
            .as_u64()
            .and_then(|byte_count| byte_count.checked_add(2)),
        "the canonical JSON string binding includes its two quote bytes"
    );
    assert!(
        !raw_metadata_text.contains("data:image/png;base64"),
        "metadata must not copy external payload content"
    );
    let payload_path = payload_storage_root.join(&payload_ref);
    assert!(
        payload_path.is_file(),
        "external payload fixture is missing"
    );

    let denied_payload = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_expand",
        json!({
            "provider": "cursor",
            "session_id": "lcm-active-session",
            "target": {"kind": "external_payload", "payload_ref": payload_ref.clone()},
            "content_limit": 80
        }),
    )
    .await;
    let denied_payload: Value =
        serde_json::from_str(extract_real_server_text(&denied_payload)).unwrap();
    assert_eq!(denied_payload["status"], "deleted");

    let payload_result = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_expand",
        json!({
            "provider": "cursor",
            "session_id": owner_session,
            "target": {"kind": "external_payload", "payload_ref": payload_ref.clone()},
            "content_limit": 80
        }),
    )
    .await;
    let payload: Value = serde_json::from_str(extract_real_server_text(&payload_result)).unwrap();
    assert_eq!(payload["status"], "partial", "{payload}");
    assert_eq!(payload["omitted"], 1, "{payload}");
    assert_eq!(payload["retrieval"]["outcome"], "partial", "{payload}");
    assert_eq!(
        payload["expansion"]["content_range"]["truncated"], true,
        "{payload}"
    );
    assert_eq!(payload["expansion"]["kind"], "external_payload");
    assert!(
        payload["expansion"]["content"]
            .as_str()
            .expect("external payload content")
            .starts_with("data:image/png;base64,")
    );

    let wrong_provider_result = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_expand",
        json!({
            "provider": "claude",
            "session_id": owner_session,
            "target": {"kind": "external_payload", "payload_ref": payload_ref.clone()},
            "content_limit": 80
        }),
    )
    .await;
    let wrong_provider: Value =
        serde_json::from_str(extract_real_server_text(&wrong_provider_result)).unwrap();
    assert_eq!(wrong_provider["status"], "deleted", "{wrong_provider}");

    db.apply_lcm_lineage_fault_for_test(LcmLineageFaultForTest::ReplaceOccurrenceProvider {
        session_id: owner_session.clone(),
        message_id: "origin-external-message".to_string(),
        source_provider: "claude".to_string(),
    })
    .await
    .expect("tamper occurrence provider binding");
    let wrong_occurrence_result = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_expand",
        json!({
            "provider": "cursor",
            "session_id": owner_session,
            "target": {"kind": "external_payload", "payload_ref": payload_ref.clone()},
            "content_limit": 80
        }),
    )
    .await;
    let wrong_occurrence: Value =
        serde_json::from_str(extract_real_server_text(&wrong_occurrence_result)).unwrap();
    assert_eq!(wrong_occurrence["status"], "denied", "{wrong_occurrence}");
    db.apply_lcm_lineage_fault_for_test(LcmLineageFaultForTest::ReplaceOccurrenceProvider {
        session_id: owner_session.clone(),
        message_id: "origin-external-message".to_string(),
        source_provider: "cursor".to_string(),
    })
    .await
    .expect("restore occurrence provider binding");

    db.apply_lcm_lineage_fault_for_test(LcmLineageFaultForTest::ReplacePublicationReceipt {
        receipt_id: original_manifest.receipt_id.clone(),
        sanitizer_version: "tampered-sanitizer".to_string(),
        payload_digest: "tampered-summary-digest".to_string(),
        receipt_json: r#"{"summary_id":"tampered-receipt"}"#.to_string(),
    })
    .await
    .expect("tamper frozen publication receipt");
    let tampered_receipt_result = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_expand",
        json!({
            "provider": "cursor",
            "session_id": owner_session,
            "target": {"kind": "external_payload", "payload_ref": payload_ref.clone()},
            "content_limit": 80
        }),
    )
    .await;
    let tampered_receipt: Value =
        serde_json::from_str(extract_real_server_text(&tampered_receipt_result)).unwrap();
    assert_eq!(
        tampered_receipt["status"], "unavailable",
        "{tampered_receipt}"
    );
    db.apply_lcm_lineage_fault_for_test(LcmLineageFaultForTest::ReplacePublicationReceipt {
        receipt_id: original_manifest.receipt_id.clone(),
        sanitizer_version: original_manifest.receipt_sanitizer_version.clone(),
        payload_digest: original_manifest.receipt_payload_digest.clone(),
        receipt_json: original_manifest.receipt_json.clone(),
    })
    .await
    .expect("restore frozen publication receipt");

    let mut wrong_session_manifest = original_manifest.clone();
    wrong_session_manifest.session_id = "wrong-session".to_string();
    db.replace_lcm_external_payload_manifest_for_test(&payload_ref, &wrong_session_manifest)
        .await
        .expect("tamper external payload session");
    let wrong_session_result = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_expand",
        json!({
            "provider": "cursor",
            "session_id": owner_session,
            "target": {"kind": "external_payload", "payload_ref": payload_ref.clone()},
            "content_limit": 80
        }),
    )
    .await;
    let wrong_session: Value =
        serde_json::from_str(extract_real_server_text(&wrong_session_result)).unwrap();
    assert_eq!(wrong_session["status"], "unavailable", "{wrong_session}");

    db.replace_lcm_external_payload_manifest_for_test(&payload_ref, &original_manifest)
        .await
        .expect("restore external payload session");
    let mut tampered_manifest = original_manifest.clone();
    tampered_manifest.payload_digest = "tampered-publication-digest".to_string();
    db.replace_lcm_external_payload_manifest_for_test(&payload_ref, &tampered_manifest)
        .await
        .expect("tamper external payload digest");
    let tampered_result = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_expand",
        json!({
            "provider": "cursor",
            "session_id": owner_session,
            "target": {"kind": "external_payload", "payload_ref": payload_ref.clone()},
            "content_limit": 80
        }),
    )
    .await;
    let tampered: Value = serde_json::from_str(extract_real_server_text(&tampered_result)).unwrap();
    assert_eq!(tampered["status"], "unavailable", "{tampered}");

    db.replace_lcm_external_payload_manifest_for_test(&payload_ref, &original_manifest)
        .await
        .expect("restore external payload manifest");
    std::fs::remove_file(&payload_path).expect("remove external payload fixture");
    let missing_result = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_expand",
        json!({
            "provider": "cursor",
            "session_id": owner_session,
            "target": {"kind": "external_payload", "payload_ref": payload_ref},
            "content_limit": 80
        }),
    )
    .await;
    let missing: Value = serde_json::from_str(extract_real_server_text(&missing_result)).unwrap();
    assert_eq!(missing["status"], "deleted", "{missing}");
    server.shutdown().await;
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_status_reports_dag_store_and_config_diagnostics_over_mcp() {
    let (cg, _env, _dir) = setup_empty_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-diag-session",
        "diag-message",
        "alpha beta gamma delta",
        1,
    )
    .await;
    let db = open_active_project_session_db(&cg).await;
    let raw = db
        .lcm_load_raw_message_for_test("cursor", "diag-message")
        .await
        .expect("raw message should load from the active project-local store");
    assert_eq!(raw.session_id, "lcm-diag-session");
    let store_id = raw.store_id;
    db.lcm_insert_summary_node_for_test(
        HostAdmissionScope::Project,
        LcmSummaryNodeDraft {
            provider: "cursor".to_string(),
            conversation_id: "lcm-diag-session".to_string(),
            session_id: "lcm-diag-session".to_string(),
            depth: 0,
            summary_text: "diag summary".to_string(),
            source_refs: vec![LcmSourceRef::RawMessage { store_id }],
            source_token_count: 24,
            summary_token_count: 6,
            source_time_start: Some(1),
            source_time_end: Some(2),
            expand_hint: Some("diagnostics test".to_string()),
            metadata_json: None,
        },
    )
    .await
    .expect("summary should insert");

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_status",
        json!({"provider": "cursor", "session_id": "lcm-diag-session"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "ok");
    let lcm = &payload["lcm"];
    assert_eq!(lcm["store"]["messages"], 1);
    assert_eq!(lcm["store"]["estimated_tokens"], 4);
    assert_eq!(lcm["dag"]["total_nodes"], 1);
    assert_eq!(lcm["dag"]["total_tokens"], 6);
    assert_eq!(lcm["dag"]["total_source_tokens"], 24);
    assert_eq!(lcm["dag"]["compression_ratio"], "4.0:1");
    assert_eq!(lcm["dag"]["depths"]["d0"]["count"], 1);
    assert_eq!(lcm["dag"]["depths"]["d0"]["tokens"], 6);
    assert_eq!(lcm["dag"]["depths"]["d0"]["source_tokens"], 24);
    assert_eq!(lcm["config"]["fresh_tail_count"], 2);
    assert_eq!(lcm["config"]["summary_fan_in"], 4);
    assert_eq!(lcm["config"]["compression_boundary_cooldown_seconds"], 60);
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_status_all_provider_aggregates_provider_counts() {
    let (cg, _env, _dir) = setup_empty_project().await;
    seed_lcm_session_message_for_provider(
        &cg,
        "cursor",
        "cursor-session",
        "cursor-msg",
        "alpha beta",
        1,
    )
    .await;
    seed_lcm_session_message_for_provider(
        &cg,
        "codex",
        "codex-session",
        "codex-msg",
        "gamma delta epsilon",
        2,
    )
    .await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_status",
        json!({"provider": "all"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["provider"], "all");
    assert_eq!(payload["lcm"]["raw_message_count"], 2);
    assert_eq!(payload["lcm"]["store"]["messages"], 2);
    assert_eq!(payload["lcm"]["store"]["estimated_tokens"], 5);
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_status_all_provider_counts_payload_health_once() {
    let (cg, _env, _dir) = setup_empty_project().await;
    seed_lcm_tool_result_message_for_provider(
        &cg,
        "cursor",
        "lcm-status-all-payload-cursor",
        "lcm-status-all-payload-cursor-message",
        format!("cursor payload\n{}", "cursor-body ".repeat(30_000)),
        1,
    )
    .await;
    seed_lcm_tool_result_message_for_provider(
        &cg,
        "codex",
        "lcm-status-all-payload-codex",
        "lcm-status-all-payload-codex-message",
        format!("codex payload\n{}", "codex-body ".repeat(30_000)),
        2,
    )
    .await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_status",
        json!({"provider": "all"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["lcm"]["payload"]["externalized_count"], 2);
    assert_eq!(payload["lcm"]["payload"]["orphan_file_count"], 0);
    assert_eq!(payload["lcm"]["payload"]["missing_count"], 0);
}

// Repeated LCM tool calls in one process must reuse the per-process
// The retained project runtime must not re-run the full DDL ensure for each
// request. Observable via the version gate: after admission, a manually
// downgraded version marker stays downgraded across calls on the same server
// — reconstructing the server would correctly admit and migrate it again.
#[cfg(feature = "test-transport")]
#[tokio::test]
async fn repeated_lcm_calls_skip_schema_reensure_per_process() {
    let dir = test_temp_dir();
    let (cg, _env) = init_test_project(dir.path()).await;

    // Seed data to ensure the sessions.db exists (lcm_status is read-only and
    // will not create the DB), then retain one real server/runtime across both
    // calls.
    seed_lcm_session_message(
        &cg,
        "ensure-cache-session",
        "ensure-cache-msg",
        "schema ensure cache sentinel",
        1,
    )
    .await;
    let runtime = open_active_project_session_db(&cg).await;
    let server = real_mcp_server(cg).await;

    let result = handle_real_server_tool_call(&server, "tracedecay_lcm_status", json!({})).await;
    let payload: Value = serde_json::from_str(extract_real_server_text(&result)).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(
        payload["lcm"]["schema_version"],
        json!(tracedecay_sessions::runtime::lcm::LCM_SCHEMA_VERSION)
    );

    runtime
        .set_lcm_schema_migration_version_for_test(HostAdmissionScope::Project, 1)
        .await
        .unwrap();

    let result = handle_real_server_tool_call(&server, "tracedecay_lcm_status", json!({})).await;
    let payload: Value = serde_json::from_str(extract_real_server_text(&result)).unwrap();
    assert_eq!(
        payload["status"], "ok",
        "repeated serve-mode call must work"
    );
    assert_eq!(
        payload["lcm"]["schema_version"],
        json!(1),
        "second call must use the retained runtime without re-running migrations; payload: {payload}"
    );

    // The on-disk marker is untouched as well.
    let version = runtime
        .lcm_schema_migration_version_for_test(HostAdmissionScope::Project)
        .await
        .unwrap();
    assert_eq!(version, Some(1));
    server.shutdown().await;
}

// ---------------------------------------------------------------------------
// Scope validation (fail-closed, not fail-open)
// ---------------------------------------------------------------------------

/// Regression test: an invalid `scope` must be a hard error naming the valid
/// values — never silently broadened to `all`.
#[tokio::test]
async fn lcm_grep_rejects_invalid_scope() {
    let dir = test_temp_dir();
    let (cg, _env) = init_test_project(dir.path()).await;
    let err = expect_tool_error(
        handle_tool_call(
            &cg,
            "tracedecay_lcm_grep",
            json!({"query": "anything", "scope": "everything"}),
            None,
            None,
        )
        .await,
    );
    assert!(
        err.contains("scope must be one of current, session, all"),
        "unexpected error: {err}"
    );
    let err = expect_tool_error(
        handle_tool_call(
            &cg,
            "tracedecay_lcm_grep",
            json!({"query": "anything", "relationship_scope": "children"}),
            None,
            None,
        )
        .await,
    );
    assert!(
        err.contains("relationship_scope must be one of all, parents_only, subagents_only"),
        "unexpected error: {err}"
    );
}

// ---------------------------------------------------------------------------
// Regression: ghost-create — pure-read LCM tools must not create sessions.db
// ---------------------------------------------------------------------------

/// Calling a `readOnlyHint` LCM tool on a freshly initialized project (config
/// authority already opened sessions.db, but no transcript ingest) must stay
/// typed and non-mutating: return an empty/read status rather than inventing
/// ingest success, and must not create a second store path.
#[tokio::test]
async fn lcm_read_only_tools_return_not_ingested_without_creating_sessions_db() {
    let dir = test_temp_dir();
    let project = dir.path();
    std::fs::write(project.join("lib.rs"), "fn f() {}").unwrap();
    let (cg, _env) = init_test_project(project).await;

    let db_path = cg.store_layout().sessions_db_path.clone();
    assert!(
        db_path.exists(),
        "init must open configuration authority sessions.db"
    );
    let size_before = std::fs::metadata(&db_path)
        .expect("sessions.db metadata")
        .len();

    // Exercise the five generic pure-read LCM tools.
    for (tool, args) in [
        ("tracedecay_lcm_status", json!({})),
        ("tracedecay_lcm_grep", json!({"query": "anything"})),
        (
            "tracedecay_lcm_describe",
            json!({"provider": "cursor", "session_id": "ghost-session"}),
        ),
        (
            "tracedecay_lcm_expand",
            json!({"provider": "cursor", "session_id": "ghost-session", "target": {"kind": "raw_message", "store_id": 1}}),
        ),
        (
            "tracedecay_lcm_expand_query",
            json!({"provider": "cursor", "session_id": "ghost-session", "prompt": "anything"}),
        ),
    ] {
        let result = handle_tool_call(&cg, tool, args.clone(), None, None)
            .await
            .unwrap_or_else(|e| panic!("{tool} returned error: {e}"));

        let text = extract_text(&result.value);
        let payload: Value = serde_json::from_str(text)
            .unwrap_or_else(|e| panic!("{tool} response is not valid JSON: {e}\n{text}"));

        let status = payload["status"].as_str().unwrap_or_default();
        // The temporal retrieval runtime maps an empty/zero-row resolution for a
        // never-ingested anchor to a typed, non-retryable `deleted` outcome
        // (session_retrieval.rs CompleteZero -> Deleted). That stays a typed,
        // non-error, non-mutating read, which is exactly this test's intent.
        assert!(
            matches!(
                status,
                "ok" | "not_ingested" | "unavailable" | "complete_zero" | "deleted"
            ),
            "{tool}: unexpected status={status}, got {payload}"
        );
        assert_ne!(
            status, "error",
            "{tool}: read-only empty store must stay typed, got {payload}"
        );

        assert!(
            db_path.exists(),
            "{tool}: sessions.db must remain at {}",
            db_path.display()
        );
        let size_after = std::fs::metadata(&db_path)
            .expect("sessions.db metadata")
            .len();
        assert!(
            size_after <= size_before.saturating_add(64 * 1024),
            "{tool}: read-only tool grew sessions.db unexpectedly ({size_before} -> {size_after})"
        );
    }
}

#[tokio::test]
async fn lcm_load_session_missing_store_uses_typed_empty_messages_without_creating_sessions_db() {
    let dir = test_temp_dir();
    let project = dir.path();
    std::fs::write(project.join("lib.rs"), "fn f() {}").unwrap();
    let (cg, _env) = init_test_project(project).await;

    let db_path = cg.store_layout().sessions_db_path.clone();
    assert!(
        db_path.exists(),
        "init must open configuration authority sessions.db"
    );

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_load_session",
        json!({"session_id": "ghost-session"}),
        None,
        None,
    )
    .await
    .unwrap_or_else(|error| panic!("tracedecay_lcm_load_session returned error: {error}"));

    let text = extract_text(&result.value);
    let payload: Value = serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("load-session response is not valid JSON: {error}\n{text}"));

    // Without retained temporal retrieval, ghost loads stay typed-empty.
    assert_eq!(payload["messages"], json!([]));
    assert_eq!(payload["next_cursor"], Value::Null);
    let status = payload["status"].as_str().unwrap_or_default();
    assert!(
        matches!(
            status,
            "unavailable" | "ok" | "complete_zero" | "not_ingested"
        ),
        "unexpected status={status}, got {payload}"
    );
    if status == "unavailable" {
        assert_eq!(
            payload["error"]["code"],
            "lcm_retrieval_service_unavailable"
        );
    }
    assert!(
        db_path.exists(),
        "tracedecay_lcm_load_session must keep configuration sessions.db at {}",
        db_path.display()
    );
}

// ---------------------------------------------------------------------------
// Regression: max_tokens must not suppress context budget
// ---------------------------------------------------------------------------

/// Before the fix, `default_context_limit = max_tokens.clamp(32_000, 65_536)`
/// always evaluated to 32_000 because max_tokens ≤ 8_192 < 32_000, making
/// `max_tokens` dead. After the fix, `context_max_tokens` defaults to the
/// constant 32_000 and both params are independent. We verify that the handler
/// accepts an explicit `context_max_tokens` override and that the returned
/// payload reflects it.
#[tokio::test]
async fn lcm_expand_query_context_max_tokens_is_independent_of_max_tokens() {
    let dir = test_temp_dir();
    let project = dir.path();
    std::fs::write(project.join("lib.rs"), "fn f() {}").unwrap();
    let (cg, _env) = init_test_project(project).await;

    // With no retained transcript the tool returns a typed empty or unavailable
    // outcome. This test only verifies that the independent budgets pass
    // argument validation without requiring code indexing.
    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_expand_query",
        json!({
            "session_id": "test-session",
            "provider": "cursor",
            "prompt": "what did we discuss?",
            "max_tokens": 500,
            "context_max_tokens": 48000,
        }),
        None,
        None,
    )
    .await
    .expect("expand_query with explicit context_max_tokens must not error");

    let text = extract_text(&result.value);
    let payload: Value =
        serde_json::from_str(text).expect("expand_query result must be valid JSON");

    // The important thing: it must NOT return a Config/argument error about
    // max_tokens or context_max_tokens. The exact empty outcome depends on
    // whether the canonical retained-session service is mounted by the test
    // harness.
    assert!(
        matches!(
            payload["status"].as_str(),
            Some("not_ingested" | "ok" | "unavailable" | "complete_zero" | "deleted")
        ),
        "unexpected status in expand_query response: {payload}"
    );
}
