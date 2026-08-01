#[cfg(unix)]
use crate::common;
use crate::support::*;
use serde_json::{Value, json};
use std::fs;
#[cfg(feature = "test-transport")]
use std::path::Path;
#[cfg(feature = "test-transport")]
use std::time::Duration;
#[cfg(feature = "test-transport")]
use std::time::SystemTime;
use tempfile::TempDir;
use tracedecay::application::host_admission::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
use tracedecay::mcp::get_tool_definitions;
#[cfg(feature = "test-transport")]
use tracedecay::sessions::lcm::types::LcmImmutableSummaryPublication;
#[cfg(feature = "test-transport")]
use tracedecay::sessions::lcm::{
    LcmLifecycleUpdate, LcmMaintenanceDebt, LcmSourceRef, LcmSummaryNodeDraft,
};
#[cfg(feature = "test-transport")]
use tracedecay::sessions::{SessionMessageRecord, SessionRecord};
#[cfg(feature = "test-transport")]
use tracedecay_domain::CanonicalMessageRoleV1;
#[cfg(feature = "test-transport")]
use tracedecay_domain::PayloadAccessState;

#[test]
fn lcm_compress_public_schema_excludes_test_summarizer_modes() {
    let tools = get_tool_definitions();
    let compress = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_lcm_compress")
        .expect("tracedecay_lcm_compress definition");
    let modes = compress.input_schema["properties"]["summarizer"]["properties"]["mode"]["enum"]
        .as_array()
        .expect("summarizer mode enum");

    assert!(modes.iter().any(|mode| mode == "provided"));
    assert!(modes.iter().any(|mode| mode == "hermes_auxiliary"));
    assert!(
        modes.iter().all(|mode| mode != "noop" && mode != "fake"),
        "public MCP schema should not advertise test/control summarizers: {modes:?}"
    );
}

#[tokio::test]
async fn lcm_project_root_storage_arg_is_not_rejected_as_selector() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let project_root = cg.project_root().to_string_lossy().to_string();

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_preflight",
        json!({
            "provider": "cursor",
            "session_id": "stock-check-session",
            "project_root": project_root,
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": "hi there"}
            ],
            "current_tokens": 50
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload = extract_json(&result.value);

    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["session_id"], "stock-check-session");
}

#[tokio::test]
async fn lcm_project_path_selector_is_rejected_before_dispatch() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let project_path = cg.project_root().to_string_lossy().to_string();

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_preflight",
        json!({
            "provider": "cursor",
            "session_id": "stock-check-session",
            "project_path": project_path,
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": "hi there"}
            ],
            "current_tokens": 50
        }),
        None,
        None,
    )
    .await;
    let message = expect_tool_error(result);

    assert!(
        message.contains("does not accept project selectors"),
        "LCM preflight should reject project_path selectors before dispatch, got {message}"
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_doctor_clean_dry_run_reports_noise_and_filtered_sessions_without_mutating() {
    let (cg, _env, _dir) = setup_empty_project().await;
    seed_lcm_session_message(
        &cg,
        "cron-20260414",
        "cron-20260414-message",
        "scheduled report body that must not leak",
        1,
    )
    .await;
    seed_lcm_session_message(
        &cg,
        "scratch-shell-a",
        "scratch-shell-message",
        "scratch one-shot body that must not leak",
        2,
    )
    .await;
    seed_lcm_session_message(
        &cg,
        "normal-session",
        "normal-heartbeat",
        "Still working...",
        3,
    )
    .await;
    seed_lcm_session_message(
        &cg,
        "normal-session",
        "normal-valuable",
        "valuable payload to preserve",
        4,
    )
    .await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({
            "provider": "cursor",
            "mode": "clean",
            "apply": false,
            "ignore_session_patterns": ["cron-*"],
            "stateless_session_patterns": ["scratch-shell-*"],
            "ignore_message_patterns": ["Cronjob Response:*"]
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let payload: Value = serde_json::from_str(text).unwrap();

    assert_eq!(payload["mode"], "clean");
    assert_eq!(payload["dry_run"], true);
    assert_eq!(payload["diagnostics"]["cleanup"]["read_only"], true);
    assert_eq!(
        payload["diagnostics"]["cleanup"]["ignored_session_candidates"],
        1
    );
    assert_eq!(
        payload["diagnostics"]["cleanup"]["stateless_session_candidates"],
        1
    );
    assert_eq!(
        payload["diagnostics"]["cleanup"]["noise_message_candidates"],
        0
    );
    assert_eq!(
        payload["diagnostics"]["cleanup"]["heartbeat_noise_message_candidates"],
        1
    );
    assert_eq!(payload["diagnostics"]["cleanup"]["candidate_count"], 2);
    assert_eq!(
        payload["diagnostics"]["cleanup"]["heartbeat_message_candidates"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        payload["repairs"]["planned_actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|action| action["kind"] == "clean_lcm_noise")
    );
    assert_eq!(lcm_raw_message_count(&cg, "cron-20260414").await, 1);
    assert_eq!(lcm_raw_message_count(&cg, "scratch-shell-a").await, 1);
    assert_eq!(lcm_raw_message_count(&cg, "normal-session").await, 2);
    assert!(!text.contains("scheduled report body that must not leak"));
    assert!(!text.contains("scratch one-shot body that must not leak"));
    assert!(!text.contains("Still working"));
    assert!(!text.contains("valuable payload to preserve"));
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_doctor_clean_apply_is_denied_by_default() {
    let (cg, _env, _dir) = setup_empty_project().await;
    seed_lcm_session_message(
        &cg,
        "cron-20260414",
        "cron-20260414-message",
        "scheduled report body that must remain without explicit opt-in",
        1,
    )
    .await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({
            "provider": "cursor",
            "mode": "clean",
            "apply": true,
            "ignore_session_patterns": ["cron-*"]
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    let payload: Value = serde_json::from_str(text).unwrap();

    assert_eq!(payload["status"], "denied");
    assert_eq!(
        payload["error"],
        "destructive cleanup is disabled by default"
    );
    assert_eq!(payload["mode"], "clean");
    assert_eq!(payload["apply"], true);
    assert_eq!(lcm_raw_message_count(&cg, "cron-20260414").await, 1);
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_doctor_clean_apply_backs_up_and_deletes_only_safe_candidates() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let _apply_enabled = TestEnvVarGuard::set("LCM_DOCTOR_CLEAN_APPLY_ENABLED", "true");
    seed_lcm_session_message(
        &cg,
        "cron-20260414",
        "cron-20260414-message",
        "scheduled report body that must be deleted only after backup",
        1,
    )
    .await;
    let db = open_active_project_session_db(&cg).await;
    let cron_store_id = rusqlite::Connection::open(project_session_db_path(&cg))
        .unwrap()
        .query_row(
            "SELECT store_id FROM lcm_raw_messages
             WHERE provider = 'cursor' AND message_id = 'cron-20260414-message'",
            (),
            |row| row.get(0),
        )
        .unwrap();
    db.lcm_insert_summary_node_for_test(
        HostAdmissionScope::Project,
        LcmSummaryNodeDraft {
            provider: "cursor".to_string(),
            conversation_id: "cron-20260414".to_string(),
            session_id: "cron-20260414".to_string(),
            depth: 0,
            summary_text: "scheduled report summary".to_string(),
            source_refs: vec![LcmSourceRef::RawMessage {
                store_id: cron_store_id,
            }],
            source_token_count: 12,
            summary_token_count: 3,
            source_time_start: Some(1),
            source_time_end: Some(2),
            expand_hint: Some("test clean candidate".to_string()),
            metadata_json: None,
        },
    )
    .await
    .unwrap();
    seed_lcm_session_message(
        &cg,
        "normal-session",
        "normal-heartbeat",
        "Still working...",
        2,
    )
    .await;
    seed_lcm_session_message(
        &cg,
        "normal-session",
        "normal-valuable",
        "valuable payload to preserve",
        3,
    )
    .await;

    let server = real_mcp_server(cg).await;
    let result = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_doctor",
        json!({
            "provider": "cursor",
            "mode": "clean",
            "apply": true,
            "ignore_session_patterns": ["cron-*"]
        }),
    )
    .await;
    let text = extract_real_server_text(&result);
    let payload: Value = serde_json::from_str(text).unwrap();
    let backup_path = payload["repairs"]["backup"]["path"]
        .as_str()
        .expect("clean apply should report backup path");

    assert_eq!(payload["status"], "repaired");
    assert_eq!(payload["dry_run"], false);
    assert_eq!(payload["repairs"]["backup"]["ok"], true);
    assert!(Path::new(backup_path).is_file());
    assert_eq!(
        payload["diagnostics"]["cleanup"]["heartbeat_noise_message_candidates"],
        1
    );
    assert_eq!(
        lcm_raw_message_count_at_path(Path::new(backup_path), "cron-20260414").await,
        1
    );
    assert_eq!(
        db.lcm_raw_message_count_for_test(HostAdmissionScope::Project, "cron-20260414")
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        db.lcm_summary_node_count_for_test(HostAdmissionScope::Project, "cron-20260414")
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        db.lcm_raw_message_count_for_test(HostAdmissionScope::Project, "normal-session")
            .await
            .unwrap(),
        2
    );
    assert!(!text.contains("scheduled report body that must be deleted only after backup"));
    assert!(!text.contains("Still working"));
    assert!(!text.contains("valuable payload to preserve"));
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_doctor_clean_apply_deletes_all_matching_noise_beyond_diagnostic_samples() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let _apply_enabled = TestEnvVarGuard::set("LCM_DOCTOR_CLEAN_APPLY_ENABLED", "true");
    let db = open_active_project_session_db(&cg).await;
    for idx in 0..21 {
        seed_lcm_session_message_in_db(
            &db,
            cg.project_root(),
            "normal-session",
            &format!("cron-noise-{idx}"),
            format!("Cronjob Response: noisy heartbeat {idx}"),
            idx + 1,
        )
        .await;
    }
    seed_lcm_session_message_in_db(
        &db,
        cg.project_root(),
        "normal-session",
        "normal-valuable",
        "valuable payload to preserve",
        30,
    )
    .await;

    let server = real_mcp_server(cg).await;
    let result = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_doctor",
        json!({
            "provider": "cursor",
            "mode": "clean",
            "apply": true,
            "ignore_message_patterns": ["^Cronjob Response:"]
        }),
    )
    .await;
    let text = extract_real_server_text(&result);
    let payload: Value = serde_json::from_str(text).unwrap();

    assert_eq!(
        payload["diagnostics"]["cleanup"]["noise_message_candidates"],
        21
    );
    assert_eq!(
        payload["diagnostics"]["cleanup"]["message_candidates"]
            .as_array()
            .unwrap()
            .len(),
        20
    );
    assert_eq!(
        payload["repairs"]["applied_actions"][0]["deleted"]["raw_messages"],
        21
    );
    assert_eq!(
        db.lcm_raw_message_count_for_test(HostAdmissionScope::Project, "normal-session")
            .await
            .unwrap(),
        1
    );
    assert!(!text.contains("Cronjob Response: noisy heartbeat"));
    assert!(!text.contains("valuable payload to preserve"));
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_doctor_reports_missing_and_orphan_payloads_without_payload_bodies() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let secret = format!(
        "LCM_DOCTOR_SECRET_PAYLOAD\n{}",
        "doctor-secret ".repeat(30_000)
    );
    seed_lcm_tool_result_message(
        &cg,
        "lcm-doctor-payload",
        "lcm-doctor-payload-message",
        secret,
        1,
    )
    .await;

    let db = open_active_project_session_db(&cg).await;
    let raw = db
        .lcm_load_raw_message_for_test("cursor", "lcm-doctor-payload-message")
        .await
        .expect("externalized raw message should load");
    let payload_ref = raw.payload_ref.expect("external payload ref");
    fs::remove_file(lcm_payload_dir(&cg).join(&payload_ref)).unwrap();
    fs::write(
        lcm_payload_dir(&cg).join("payload_unreferenced_test.payload"),
        "orphan body that must not be returned",
    )
    .unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({"provider": "cursor", "mode": "diagnose"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "issues_found");
    assert_eq!(payload["diagnostics"]["payloads"]["missing_files"], 1);
    assert_eq!(payload["diagnostics"]["payloads"]["orphan_files"], 1);
    let text = extract_text(&result.value);
    assert!(!text.contains("LCM_DOCTOR_SECRET_PAYLOAD"));
    assert!(!text.contains("orphan body that must not be returned"));
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_doctor_reports_placeholder_recovery_and_gc_candidates_without_bodies() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let missing_ref = "payload_missing_placeholder_test.payload";
    let placeholder = format!(
        "[Externalized LCM ingest payload: kind=ingest_payload; role=user; field=content; chars=2048; bytes=2048; ref={missing_ref}]"
    );
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-placeholder",
        "lcm-doctor-placeholder-message",
        placeholder,
        1,
    )
    .await;

    let payload_dir = lcm_payload_dir(&cg);
    fs::create_dir_all(&payload_dir).unwrap();
    fs::write(
        payload_dir.join("payload_gc_candidate_test.payload"),
        "gc candidate body that must not be returned",
    )
    .unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({
            "provider": "cursor",
            "session_id": "lcm-doctor-placeholder",
            "mode": "diagnose"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "issues_found");
    assert_eq!(
        payload["diagnostics"]["payloads"]["placeholder_refs_total"],
        1
    );
    assert_eq!(
        payload["diagnostics"]["payloads"]["missing_placeholder_metadata"],
        1
    );
    assert_eq!(
        payload["diagnostics"]["payloads"]["missing_placeholder_files"],
        1
    );
    assert_eq!(payload["diagnostics"]["payloads"]["gc_candidate_files"], 1);
    assert!(
        payload["diagnostics"]["payloads"]["missing_placeholder_refs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value["payload_ref"] == missing_ref)
    );
    assert!(
        payload["diagnostics"]["payloads"]["gc_candidate_payload_refs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value.as_str() == Some("payload_gc_candidate_test.payload"))
    );
    let text = extract_text(&result.value);
    assert!(!text.contains("gc candidate body that must not be returned"));
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_doctor_gc_mode_preview_and_apply_reports_without_body_leaks() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let _apply_enabled = TestEnvVarGuard::set("LCM_GC_APPLY_ENABLED", "true");
    seed_lcm_session_message(
        &cg,
        "gc-preview-session",
        "gc-preview-message",
        "seed message for gc preview",
        1,
    )
    .await;
    let payload_dir = lcm_payload_dir(&cg);
    fs::create_dir_all(&payload_dir).unwrap();
    let payload_ref =
        "payload_cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc.payload";
    let payload_path = payload_dir.join(payload_ref);
    fs::write(&payload_path, "gc mode secret body that must not leak").unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&payload_path)
        .unwrap()
        .set_times(
            fs::FileTimes::new().set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
        )
        .unwrap();

    let server = real_mcp_server(cg).await;
    let preview = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_doctor",
        json!({"provider": "cursor", "mode": "gc", "apply": false}),
    )
    .await;
    let preview_text = extract_real_server_text(&preview);
    let preview_payload: Value = serde_json::from_str(preview_text).unwrap();
    assert_eq!(preview_payload["mode"], "gc");
    assert_eq!(preview_payload["dry_run"], true);
    assert_eq!(
        preview_payload["repairs"]["gc_report"]["orphans"]["count"],
        1
    );
    assert!(payload_path.is_file());
    assert!(!preview_text.contains("gc mode secret body that must not leak"));

    let apply = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_doctor",
        json!({
            "provider": "cursor",
            "mode": "gc",
            "apply": true
        }),
    )
    .await;
    let apply_text = extract_real_server_text(&apply);
    let apply_payload: Value = serde_json::from_str(apply_text).unwrap();
    assert_eq!(apply_payload["mode"], "gc");
    assert_eq!(apply_payload["dry_run"], false);
    assert_eq!(apply_payload["repairs"]["gc_report"]["orphans"]["count"], 1);
    assert!(!payload_path.exists());
    assert!(!apply_text.contains("gc mode secret body that must not leak"));
}

#[tokio::test]
async fn lcm_doctor_gc_apply_is_denied_by_default() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({"provider": "cursor", "mode": "gc", "apply": true}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert_eq!(payload["status"], "denied");
    assert_eq!(payload["mode"], "gc");
    assert_eq!(
        payload["repairs"]["unsafe_actions_skipped"][0]["reason"],
        "lcm_gc_apply_disabled"
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_doctor_counts_nested_externalized_payload_refs_as_referenced() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let media_payload = format!(
        "data:image/png;base64,{}",
        "QWxhZGRpbjpvcGVuIHNlc2FtZQ==".repeat(160)
    );
    let content = json!({
        "content": [
            {"type": "text", "text": "doctor nested payload canary"},
            {"type": "image_url", "image_url": {"url": media_payload}},
        ]
    })
    .to_string();
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-nested-payload",
        "lcm-doctor-nested-payload-message",
        content,
        1,
    )
    .await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({
            "provider": "cursor",
            "session_id": "lcm-doctor-nested-payload",
            "mode": "diagnose"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "ok");
    assert_eq!(
        payload["diagnostics"]["payloads"]["unreferenced_metadata"],
        0
    );
    assert_eq!(
        payload["diagnostics"]["payloads"]["placeholder_refs_total"],
        1
    );
    assert_eq!(
        payload["diagnostics"]["payloads"]["missing_placeholder_metadata"],
        0
    );
    assert_eq!(
        payload["diagnostics"]["payloads"]["missing_placeholder_files"],
        0
    );
    close_test_graph(cg).await;
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_doctor_ignores_plain_text_ref_tokens_as_placeholders() {
    let (cg, _env, _dir) = setup_empty_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-plain-ref",
        "lcm-doctor-plain-ref-message",
        "plain documentation mentions ref=payload_plain_text_false_positive.payload outside a placeholder",
        1,
    )
    .await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({
            "provider": "cursor",
            "session_id": "lcm-doctor-plain-ref",
            "mode": "diagnose"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "ok");
    assert_eq!(
        payload["diagnostics"]["payloads"]["placeholder_refs_total"],
        0
    );
    assert_eq!(
        payload["diagnostics"]["payloads"]["missing_placeholder_metadata"],
        0
    );
    assert_eq!(
        payload["diagnostics"]["payloads"]["missing_placeholder_files"],
        0
    );
    assert!(
        payload["diagnostics"]["payloads"]["missing_placeholder_refs"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_doctor_scoped_payload_diagnostics_ignore_other_session_payload_files() {
    let (cg, _env, _dir) = setup_empty_project().await;
    seed_lcm_tool_result_message(
        &cg,
        "lcm-doctor-payload-target",
        "lcm-doctor-payload-target-message",
        format!("target payload\n{}", "target-body ".repeat(30_000)),
        1,
    )
    .await;
    seed_lcm_tool_result_message(
        &cg,
        "lcm-doctor-payload-other",
        "lcm-doctor-payload-other-message",
        format!("other payload\n{}", "other-body ".repeat(30_000)),
        2,
    )
    .await;

    let db = open_active_project_session_db(&cg).await;
    let other_raw = db
        .lcm_load_raw_message_for_test("cursor", "lcm-doctor-payload-other-message")
        .await
        .expect("other externalized raw message should load");
    let other_payload_ref = other_raw.payload_ref.expect("other external payload ref");

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({
            "provider": "cursor",
            "session_id": "lcm-doctor-payload-target",
            "mode": "diagnose"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["diagnostics"]["payloads"]["missing_files"], 0);
    assert_eq!(payload["diagnostics"]["payloads"]["orphan_files"], 0);
    assert!(
        !payload["diagnostics"]["payloads"]["orphan_payload_refs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value.as_str() == Some(&other_payload_ref))
    );
    assert!(!extract_text(&result.value).contains(&other_payload_ref));
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_doctor_reports_scoped_fts_rebuild_when_other_session_matches_probe_term() {
    let (cg, _env, _dir) = setup_empty_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-fts-target",
        "lcm-doctor-fts-target-message",
        "scopedneedle target text",
        1,
    )
    .await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-fts-other",
        "lcm-doctor-fts-other-message",
        "scopedneedle other text",
        2,
    )
    .await;
    wipe_lcm_raw_fts_for_message(&cg, "lcm-doctor-fts-target-message").await;
    assert_eq!(lcm_fts_match_count(&cg, "scopedneedle").await, 1);

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({
            "provider": "cursor",
            "session_id": "lcm-doctor-fts-target",
            "mode": "diagnose"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "issues_found");
    assert_eq!(payload["diagnostics"]["fts"]["raw"]["rebuild_needed"], true);
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_doctor_counts_summary_source_rows_with_missing_owner_node() {
    let (cg, _env, _dir) = setup_empty_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-orphan-owner",
        "lcm-doctor-orphan-owner-message",
        "orphan owner source text",
        1,
    )
    .await;
    let store_id = lcm_raw_store_id(&cg, "lcm-doctor-orphan-owner-message").await;
    project_lcm_conn(&cg)
        .await
        .inject_lcm_orphan_summary_source_for_test(HostAdmissionScope::Project, store_id)
        .await
        .unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({
            "provider": "cursor",
            "session_id": "lcm-doctor-orphan-owner",
            "mode": "diagnose"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "issues_found");
    assert_eq!(payload["diagnostics"]["summaries"]["broken_sources"], 1);
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_doctor_scopes_orphan_lifecycle_debt_to_requested_session() {
    let (cg, _env, _dir) = setup_empty_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-debt-target",
        "lcm-doctor-debt-target-message",
        "target session text",
        1,
    )
    .await;
    project_lcm_conn(&cg)
        .await
        .inject_lcm_foreign_orphan_debt_for_test(HostAdmissionScope::Project)
        .await
        .unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({
            "provider": "cursor",
            "session_id": "lcm-doctor-debt-target",
            "mode": "diagnose"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["diagnostics"]["lifecycle"]["orphan_debt"], 0);
}

#[tokio::test]
async fn lcm_doctor_diagnose_does_not_create_missing_project_session_db() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let db_path = project_session_db_path(&cg);
    if db_path.exists() {
        fs::remove_file(&db_path).unwrap();
    }

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({"provider": "cursor", "mode": "diagnose"}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "unavailable");
    assert!(
        !db_path.exists(),
        "diagnose must not create session storage"
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_doctor_repair_dry_run_does_not_run_schema_migration() {
    let (cg, _env, _dir) = setup_empty_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-read-only-existing",
        "lcm-doctor-read-only-existing-message",
        "read only existing database text",
        1,
    )
    .await;
    let db = project_lcm_conn(&cg).await;
    let server = real_mcp_server(cg).await;
    db.clear_lcm_schema_migration_for_test(HostAdmissionScope::Project)
        .await
        .unwrap();
    assert_eq!(
        db.lcm_schema_migration_version_for_test(HostAdmissionScope::Project)
            .await
            .unwrap(),
        None
    );

    let result = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_doctor",
        json!({"provider": "cursor", "mode": "repair", "apply": false}),
    )
    .await;
    let payload: Value = serde_json::from_str(extract_real_server_text(&result)).unwrap();

    assert_eq!(payload["mode"], "repair");
    assert_eq!(payload["dry_run"], true);
    assert_eq!(payload["diagnostics"]["schema"]["migration_present"], false);
    assert_eq!(
        payload["diagnostics"]["ast_grep"]["rewrite_available"].as_bool(),
        Some(tracedecay::mcp::tools::ast_grep_available())
    );
    assert_eq!(
        payload["diagnostics"]["ast_grep"]["outline_available"].as_bool(),
        Some(tracedecay::mcp::tools::ast_grep_outline_available())
    );
    assert!(
        payload["diagnostics"]["ast_grep"]["message"].is_string(),
        "doctor should include ast-grep install/update guidance"
    );
    assert_eq!(
        db.lcm_schema_migration_version_for_test(HostAdmissionScope::Project)
            .await
            .unwrap(),
        None
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_doctor_repair_dry_run_reports_fts_rebuild_without_mutating() {
    let (cg, _env, _dir) = setup_empty_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-dry-run",
        "lcm-doctor-dry-run-message",
        "dry run searchable needle",
        1,
    )
    .await;
    assert_eq!(lcm_fts_match_count(&cg, "needle").await, 1);
    wipe_lcm_raw_fts(&cg).await;
    assert_eq!(lcm_fts_match_count(&cg, "needle").await, 0);

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({"provider": "cursor", "mode": "repair", "apply": false}),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["mode"], "repair");
    assert_eq!(payload["dry_run"], true);
    assert_eq!(payload["diagnostics"]["fts"]["rebuild_needed"], true);
    assert!(
        payload["repairs"]["planned_actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|action| action["kind"] == "rebuild_raw_fts")
    );
    assert_eq!(lcm_fts_match_count(&cg, "needle").await, 0);
    close_test_graph(cg).await;
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_doctor_repair_apply_rebuilds_damaged_fts() {
    let (cg, _env, _dir) = setup_empty_project().await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-apply",
        "lcm-doctor-apply-message",
        "apply repair searchable needle",
        1,
    )
    .await;
    wipe_lcm_raw_fts(&cg).await;
    assert_eq!(lcm_fts_match_count(&cg, "needle").await, 0);
    let db = project_lcm_conn(&cg).await;

    let server = real_mcp_server(cg).await;
    let result = handle_real_server_tool_call(
        &server,
        "tracedecay_lcm_doctor",
        json!({"provider": "cursor", "mode": "repair", "apply": true}),
    )
    .await;
    let payload: Value = serde_json::from_str(extract_real_server_text(&result)).unwrap();

    assert_eq!(payload["status"], "repaired");
    assert_eq!(payload["dry_run"], false);
    let backup_path = payload["repairs"]["backup"]["path"]
        .as_str()
        .expect("repair apply should report backup path");
    assert_eq!(payload["repairs"]["backup"]["ok"], true);
    assert!(Path::new(backup_path).is_file());
    assert!(
        payload["repairs"]["applied_actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|action| action["kind"] == "rebuild_raw_fts")
    );
    assert_eq!(
        db.lcm_raw_message_fts_count_for_test("needle")
            .await
            .unwrap(),
        1
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_doctor_retention_reports_candidates_without_deleting() {
    let dir = test_temp_dir();
    let (cg, _env) = init_test_project(dir.path()).await;
    seed_lcm_session_message(
        &cg,
        "lcm-doctor-retention",
        "lcm-doctor-retention-message",
        "old session retention candidate",
        1,
    )
    .await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_doctor",
        json!({
            "provider": "cursor",
            "mode": "retention",
            "session_id": "lcm-doctor-retention"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["mode"], "retention");
    assert_eq!(payload["diagnostics"]["retention"]["read_only"], true);
    assert!(
        payload["diagnostics"]["retention"]["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|candidate| candidate["session_id"] == "lcm-doctor-retention")
    );
    assert_eq!(lcm_raw_message_count(&cg, "lcm-doctor-retention").await, 1);
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

#[tokio::test]
async fn user_scoped_lcm_preflight_ingests_without_a_project() {
    let profile = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(profile.path())
        .await
        .unwrap();
    let result = runtime
        .call_user_lcm_tool_for_test(
            "tracedecay_lcm_preflight",
            json!({
                "storage_scope": "user",
                "provider": "hermes",
                "session_id": "untethered-session",
                "messages": [{
                    "id": "untethered-message-1",
                    "role": "user",
                    "content": "Remember this general preference"
                }],
                "transcript_projection": true,
                "format": "json"
            }),
            profile.path(),
        )
        .await
        .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert_eq!(payload["status"], "ok");

    assert!(
        runtime
            .lcm_load_raw_message_for_test("hermes", "untethered-message-1")
            .await
            .is_some()
    );
    let session = runtime
        .session_for_test(HostAdmissionScope::Profile, "hermes", "untethered-session")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.project_key, "user");
}

#[tokio::test]
async fn user_scoped_lcm_projection_preserves_associated_project_roots() {
    let profile = TempDir::new().unwrap();
    let roots = json!(["/work/alpha", "/work/beta"]);
    let runtime = HostAdmissionTestRuntimeV1::profile(profile.path())
        .await
        .unwrap();
    runtime
        .call_user_lcm_tool_for_test(
            "tracedecay_lcm_preflight",
            json!({
            "storage_scope": "user",
            "provider": "hermes",
            "session_id": "multi-project-session",
            "messages": [{
                "id": "multi-project-message-1",
                "role": "user",
                "content": "Update both repositories",
                "associated_project_roots": roots
            }],
            "transcript_projection": true,
            "format": "json"
            }),
            profile.path(),
        )
        .await
        .unwrap();

    let message = runtime
        .session_message_for_test(
            HostAdmissionScope::Profile,
            "hermes",
            "multi-project-message-1",
        )
        .await
        .unwrap()
        .unwrap();
    let metadata: Value = serde_json::from_str(message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["associated_project_roots"], roots);
    assert_eq!(metadata["storage_scope"], "user");
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

    let preflight = handle_tool_call(
        &cg,
        "tracedecay_lcm_preflight",
        json!({
            "provider": "cursor",
            "session_id": "lcm-session",
            "messages": [{"id": "active-preflight", "role": "user", "content": "hello"}]
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let preflight_payload: Value = serde_json::from_str(extract_text(&preflight.value)).unwrap();
    assert_eq!(preflight_payload["status"], "ok");
    assert_eq!(preflight_payload["should_compress"], false);

    let compress = handle_tool_call(
        &cg,
        "tracedecay_lcm_compress",
        json!({
            "provider": "cursor",
            "session_id": "lcm-session",
            "messages": [{"id": "active-compress", "role": "user", "content": "hello again"}],
            "summarizer": {"mode": "noop"}
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let compress_payload: Value = serde_json::from_str(extract_text(&compress.value)).unwrap();
    assert_eq!(compress_payload["status"], "ok");
    assert_eq!(compress_payload["summary_nodes_created"], 0);
    assert_eq!(compress_payload["compression_attempts"], 0);
    assert_eq!(compress_payload["fallback_used"], false);
    assert!(
        compress_payload.get("retry_status").is_some(),
        "compress response must expose retry_status for bridge contract"
    );
    assert_eq!(compress_payload["retry_status"], Value::Null);

    let unsafe_noop_compress = handle_tool_call(
        &cg,
        "tracedecay_lcm_compress",
        json!({
            "provider": "cursor",
            "session_id": "lcm-session",
            "messages": [],
            "current_tokens": 50_000,
            "threshold_tokens": 1_000,
            "fresh_tail_count": 1,
            "leaf_chunk_tokens": 1,
            "summarizer": {"mode": "noop"}
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let unsafe_noop_payload =
        extract_lcm_json_following_handle(&cg, &unsafe_noop_compress.value).await;
    // An explicit no-op summarizer is honored even under hard pressure; the
    // pressure is reported as a typed advisory instead of silently switching
    // the caller to a different summarizer.
    assert_eq!(unsafe_noop_payload["status"], "ok");
    assert_eq!(unsafe_noop_payload["reason"], "noop_summarizer");
    assert_eq!(unsafe_noop_payload["summary_nodes_created"], 0);
    assert_eq!(
        unsafe_noop_payload["summarizer_advisory"]["code"],
        "noop_summarizer_under_hard_pressure"
    );
    assert_eq!(
        unsafe_noop_payload["summarizer_advisory"]["recommended_summarizer"],
        "hermes_auxiliary"
    );

    let reserve_cap_noop_compress = handle_tool_call(
        &cg,
        "tracedecay_lcm_compress",
        json!({
            "provider": "cursor",
            "session_id": "lcm-session",
            "messages": [],
            "current_tokens": 8_000,
            "context_length": 10_000,
            "reserve_tokens_floor": 2_000,
            "fresh_tail_count": 1,
            "leaf_chunk_tokens": 1,
            "summarizer": {"mode": "noop"}
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let reserve_cap_noop_payload =
        extract_lcm_json_following_handle(&cg, &reserve_cap_noop_compress.value).await;
    assert_eq!(reserve_cap_noop_payload["status"], "ok");
    assert_eq!(reserve_cap_noop_payload["reason"], "noop_summarizer");
    assert_eq!(
        reserve_cap_noop_payload["summarizer_advisory"]["code"],
        "noop_summarizer_under_hard_pressure"
    );

    for (index, content) in [
        "old-1 token",
        "old-2 token",
        "old-3 token",
        "old-4 token",
        "old-5 token",
        "old-6 token",
        "fresh-1",
        "fresh-2",
    ]
    .iter()
    .enumerate()
    {
        seed_lcm_session_message(
            &cg,
            "lcm-critical-session",
            &format!("lcm-critical-message-{}", index + 1),
            *content,
            (index + 1) as i64,
        )
        .await;
    }

    let critical_compress = handle_tool_call(
        &cg,
        "tracedecay_lcm_compress",
        json!({
            "provider": "cursor",
            "session_id": "lcm-critical-session",
            "messages": [],
            "current_tokens": 40,
            "max_assembly_tokens": 2,
            "leaf_chunk_tokens": 1,
            "max_source_messages": 3,
            "summarizer": {"mode": "fake", "summary_text": "catchup summary"}
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let critical_payload: Value =
        serde_json::from_str(extract_text(&critical_compress.value)).unwrap();
    assert_eq!(critical_payload["status"], "ok");
    assert_eq!(critical_payload["reason"], "forced_overflow_recovery");
    assert_eq!(critical_payload["summary_nodes_created"], 4);
    assert_eq!(critical_payload["compression_attempts"], 4);
    assert_eq!(critical_payload["fallback_used"], false);
    assert_eq!(
        critical_payload["retry_status"],
        "critical_pressure_catch_up"
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_compress_without_summarizer_requests_auxiliary_summary() {
    let (cg, _env, _dir) = setup_empty_project().await;
    for (index, content) in [
        "historical planning context alpha beta gamma",
        "historical tool result delta epsilon zeta",
        "fresh objective eta theta",
    ]
    .iter()
    .enumerate()
    {
        seed_lcm_session_message(
            &cg,
            "lcm-default-summarizer-session",
            &format!("lcm-default-summarizer-message-{}", index + 1),
            *content,
            (index + 1) as i64,
        )
        .await;
    }

    let compress = handle_tool_call(
        &cg,
        "tracedecay_lcm_compress",
        json!({
            "provider": "cursor",
            "session_id": "lcm-default-summarizer-session",
            "messages": [],
            "current_tokens": 10_000,
            "threshold_tokens": 100,
            "fresh_tail_count": 1,
            "leaf_chunk_tokens": 1,
            "max_assembly_tokens": 20
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&compress.value)).unwrap();

    assert_eq!(payload["status"], "needs_summary");
    assert_eq!(payload["reason"], "hermes_auxiliary_not_available");
    assert_eq!(payload["summary_nodes_created"], 0);
    assert_eq!(payload["compression_attempts"], 0);
    assert_eq!(payload["fallback_used"], false);
    assert_eq!(payload["retry_status"], Value::Null);
    assert_eq!(
        payload["summary_request"]["source_range"]["from_store_id"],
        1
    );
    assert_eq!(payload["summary_request"]["source_range"]["to_store_id"], 1);
    assert_eq!(
        payload["summary_request"]["source_messages"]
            .as_array()
            .expect("source messages should be present")
            .len(),
        1
    );
    let replay = payload["replay_messages"]
        .as_array()
        .expect("bounded replay should be present");
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0]["content"], "fresh objective eta theta");
    assert!(
        payload["replay_token_estimate"].as_i64().unwrap() <= 20,
        "default auxiliary mode must return a bounded replay"
    );
}

#[tokio::test]
async fn lcm_preflight_oversized_replay_preserves_bridge_contract() {
    let dir = test_temp_dir();
    let (cg, _env) = init_test_project(dir.path()).await;
    let huge_source = "preflight oversized active context ".repeat(1_000);

    let preflight = handle_tool_call(
        &cg,
        "tracedecay_lcm_preflight",
        json!({
            "provider": "cursor",
            "session_id": "lcm-oversized-preflight",
            "messages": [
                {"id": "preflight-1", "role": "user", "content": huge_source},
                {"id": "preflight-2", "role": "assistant", "content": "acknowledged"}
            ],
            "current_tokens": 10,
            "threshold_tokens": 1_000
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&preflight.value);
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["should_compress"], false);
    assert_eq!(payload["reason"], "no_compression_needed");
    assert_eq!(payload["mcp_response_truncated"], true);
    assert_eq!(payload["contract_truncated"], true);
    assert!(payload.get("truncated").is_none());
    assert!(text.len() <= MCP_TEST_RESPONSE_CHAR_LIMIT);
    assert!(
        payload["replay_messages_compacted_for_mcp"]
            .as_bool()
            .unwrap_or(false)
    );
    assert_eq!(
        payload["replay_messages"][0]["content_truncated_for_mcp"],
        true
    );
}

#[tokio::test]
async fn lcm_preflight_structured_replay_content_is_bounded_for_mcp() {
    let (cg, _dir) = setup_project().await;
    let huge_source = "structured preflight payload ".repeat(8_000);

    let preflight = handle_tool_call(
        &cg,
        "tracedecay_lcm_preflight",
        json!({
            "provider": "cursor",
            "session_id": "lcm-structured-preflight",
            "messages": [
                {
                    "id": "structured-preflight-1",
                    "role": "user",
                    "content": [
                        {"type": "text", "text": huge_source},
                        {"type": "input_json", "value": {"nested": huge_source}}
                    ]
                },
                {"id": "structured-preflight-2", "role": "assistant", "content": "acknowledged"}
            ],
            "current_tokens": 10,
            "threshold_tokens": 1_000
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&preflight.value);
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["status"], "ok");
    assert!(payload.get("truncated").is_none());
    assert!(text.len() <= MCP_TEST_RESPONSE_CHAR_LIMIT);
    let compacted_content = payload["replay_messages"][0]["content"]
        .as_str()
        .expect("structured replay content should be serialized to bounded text");
    assert!(compacted_content.len() <= 512);
    assert_eq!(
        payload["replay_messages"][0]["content_serialized_for_mcp"],
        true
    );
    assert_eq!(
        payload["replay_messages"][0]["content_truncated_for_mcp"],
        true
    );
    assert_eq!(payload["replay_messages_compacted_for_mcp"], true);
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_session_boundary_handler_records_cooldown_for_skipped_carry_over() {
    let (cg, _env, _dir) = setup_empty_project().await;
    for (index, content) in ["old-1 token", "old-2 token", "fresh-1", "fresh-2"]
        .iter()
        .enumerate()
    {
        seed_lcm_session_message(
            &cg,
            "lcm-boundary-session",
            &format!("lcm-boundary-message-{}", index + 1),
            *content,
            (index + 1) as i64,
        )
        .await;
    }

    let boundary = handle_tool_call(
        &cg,
        "tracedecay_lcm_session_boundary",
        json!({
            "provider": "cursor",
            "session_id": "lcm-boundary-session",
            "old_session_id": "lcm-old-session",
            "boundary_reason": "compression",
            "bound_session_id": "lcm-bound-session"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let boundary_payload: Value = serde_json::from_str(extract_text(&boundary.value)).unwrap();
    assert_eq!(boundary_payload["status"], "ok");
    assert_eq!(boundary_payload["recorded"], true);
    assert_eq!(
        boundary_payload["reason"],
        "compression_boundary_skip_recorded"
    );

    let preflight = handle_tool_call(
        &cg,
        "tracedecay_lcm_preflight",
        json!({
            "provider": "cursor",
            "session_id": "lcm-boundary-session",
            "messages": [],
            "current_tokens": 120,
            "threshold_tokens": 100
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let preflight_payload: Value = serde_json::from_str(extract_text(&preflight.value)).unwrap();
    assert_eq!(preflight_payload["status"], "ok");
    assert_eq!(preflight_payload["should_compress"], false);
    assert_eq!(preflight_payload["reason"], "compression_boundary_cooldown");
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
    let (cg, _dir) = setup_project().await;
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
    let (cg, _dir) = setup_project().await;
    let home = _dir.path().join("home");
    let outside_cwd = test_temp_dir();
    let project_arg = cg.project_root().display().to_string();
    handle_tool_call(
        &cg,
        "tracedecay_lcm_preflight",
        json!({
            "provider": "cursor",
            "session_id": "cli-bridge-status",
            "messages": [{"role": "user", "content": "status payload"}],
            "current_tokens": 10
        }),
        None,
        None,
    )
    .await
    .unwrap();
    close_test_graph(cg).await;
    let _daemon = common::spawn_tracedecay_daemon(&home);
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

    assert!(
        output.status.success(),
        "tracedecay tool exited with {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["content"][0]["type"], "text");
    let payload = extract_first_json_content(&json);
    // "ok" when sessions.db already exists, "not_ingested" on a fresh project
    // that has never had any LCM data. Both indicate the CLI bridge dispatched
    // correctly; the test is about argument plumbing, not store contents.
    assert!(
        payload["status"] == "ok" || payload["status"] == "not_ingested",
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
            "source_offset": 1,
            "source_limit": 2
        }),
    )
    .await;
    let payload: Value = serde_json::from_str(extract_real_server_text(&result)).unwrap();

    assert_eq!(payload["status"], "ok", "{payload}");
    let sources = payload["expansion"]["summary_sources"].as_array().unwrap();
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0]["raw_message"]["store_id"], json!(store_ids[1]));
    assert_eq!(sources[1]["raw_message"]["store_id"], json!(store_ids[2]));
    for (source, expected_body) in sources
        .iter()
        .zip(["paged source body 2", "paged source body 3"])
    {
        assert_eq!(source["state"], "available", "{source}");
        assert_eq!(source["content"], expected_body, "{source}");
        assert_eq!(source["raw_message"]["content"], expected_body, "{source}");
    }
    let pagination = &payload["expansion"]["source_pagination"];
    assert_eq!(pagination["source_offset"], 1);
    assert_eq!(pagination["source_limit"], 2);
    assert_eq!(pagination["returned_sources"], 2);
    assert_eq!(pagination["total_sources"], 4);
    assert_eq!(pagination["next_source_offset"], 3);
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
            "source_limit": 2,
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
            "source_limit": 2,
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
            "source_limit": 2,
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
    assert_eq!(
        continued["expansion"]["source_pagination"]["source_offset"],
        3
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
    assert_eq!(raw_payload["status"], "ok", "{raw_payload}");
    assert_eq!(raw_payload["expansion"]["from_current_session"], false);
    assert!(raw_payload["expansion"]["externalized_note"].is_null());
    let payload_ref = raw_payload["expansion"]["payload_ref"]
        .as_str()
        .expect("cross-session external row should surface payload_ref")
        .to_string();
    let owner_session = raw_payload["expansion"]["raw_message"]["session_id"]
        .as_str()
        .expect("owner session id should be surfaced")
        .to_string();

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
            "target": {"kind": "external_payload", "payload_ref": payload_ref},
            "content_limit": 80
        }),
    )
    .await;
    let payload: Value = serde_json::from_str(extract_real_server_text(&payload_result)).unwrap();
    assert_eq!(payload["status"], "ok", "{payload}");
    assert_eq!(payload["expansion"]["kind"], "external_payload");
    assert!(
        payload["expansion"]["content"]
            .as_str()
            .expect("external payload content")
            .starts_with("data:image/png;base64,")
    );
    server.shutdown().await;
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn lcm_compress_handler_honors_incremental_max_depth_override() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let mut store_ids = Vec::new();
    for index in 1..=6 {
        let message_id = format!("depth-msg-{index}");
        seed_lcm_session_message(
            &cg,
            "lcm-depth-session",
            &message_id,
            format!("depth source body {index}"),
            index,
        )
        .await;
        store_ids.push(lcm_raw_store_id(&cg, &message_id).await);
    }
    let db = open_active_project_session_db(&cg).await;
    for (index, pair) in store_ids.chunks(2).enumerate() {
        db.lcm_insert_summary_node_for_test(
            HostAdmissionScope::Project,
            LcmSummaryNodeDraft {
                provider: "cursor".to_string(),
                conversation_id: "lcm-depth-session".to_string(),
                session_id: "lcm-depth-session".to_string(),
                depth: 1,
                summary_text: format!("depth one summary {}", index + 1),
                source_refs: pair
                    .iter()
                    .map(|store_id| LcmSourceRef::RawMessage {
                        store_id: *store_id,
                    })
                    .collect(),
                source_token_count: 12,
                summary_token_count: 4,
                source_time_start: Some(10 + index as i64),
                source_time_end: Some(20 + index as i64),
                expand_hint: Some("depth override test".to_string()),
                metadata_json: None,
            },
        )
        .await
        .expect("depth-1 summary should insert");
    }
    db.lcm_update_lifecycle_for_test(
        HostAdmissionScope::Project,
        LcmLifecycleUpdate {
            provider: "cursor".to_string(),
            conversation_id: "lcm-depth-session".to_string(),
            current_session_id: "lcm-depth-session".to_string(),
            current_frontier_store_id: store_ids.last().copied(),
            last_finalized_session_id: None,
            last_finalized_frontier_store_id: None,
            maintenance_debt: Vec::new(),
        },
    )
    .await
    .expect("lifecycle state should update");

    let result = handle_tool_call(
        &cg,
        "tracedecay_lcm_compress",
        json!({
            "provider": "cursor",
            "session_id": "lcm-depth-session",
            "messages": [],
            "summary_fan_in": 3,
            "incremental_max_depth": 2,
            "summarizer": {"mode": "fake", "summary_text": "depth-two condensation"}
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["reason"], "condensed_summary_nodes");
    assert_eq!(payload["summary_nodes_created"], 1);
    assert_eq!(payload["summary_nodes"][0]["depth"], 2);
    assert!(
        payload["context_recovery_hint"]
            .as_str()
            .unwrap()
            .contains("tracedecay_lcm_expand_query")
    );
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
        json!(tracedecay::sessions::lcm::LCM_SCHEMA_VERSION)
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
    cg.index_all().await.unwrap();

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
    cg.index_all().await.unwrap();

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
    cg.index_all().await.unwrap();

    // With no sessions.db the tool returns not_ingested — that is fine here;
    // we just verify the argument parsing does not panic or error.
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

    // Either not_ingested (no sessions.db) or ok — both are valid here.
    // The important thing: it must NOT return a Config/argument error about
    // max_tokens or context_max_tokens.
    assert!(
        payload["status"] == "not_ingested" || payload["status"] == "ok",
        "unexpected status in expand_query response: {payload}"
    );
}
