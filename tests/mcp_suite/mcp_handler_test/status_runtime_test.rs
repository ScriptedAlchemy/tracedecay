use crate::support::*;
#[cfg(feature = "test-transport")]
use serde_json::Value;
use serde_json::json;
#[cfg(feature = "test-transport")]
use std::fs;
#[cfg(feature = "test-transport")]
use std::process::Command;
#[cfg(feature = "test-transport")]
use tempfile::TempDir;
#[cfg(feature = "test-transport")]
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
#[cfg(feature = "test-transport")]
use tracedecay_sessions::runtime::SessionRecord;
#[cfg(feature = "test-transport")]
use tracedecay_usecases::host_admission::HostAdmissionScope;

// ---------------------------------------------------------------------------
// 8. tracedecay_status
// ---------------------------------------------------------------------------

fn assert_sealed_graph_statistics_are_unavailable(text: &str) {
    let status: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(
        status["graph_statistics"],
        json!({
            "status": "unavailable",
            "reason": "sealed_generation_statistics_not_published",
        }),
        "status must not fabricate graph counts without sealed-generation statistics"
    );
    assert!(
        status.get("node_count").is_none(),
        "status must not restore the legacy unsealed node count"
    );
}

#[tokio::test]
async fn test_status() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_status",
        json!({}),
        Some(json!({"uptime": 100})),
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);
    assert_sealed_graph_statistics_are_unavailable(text);
    assert!(
        text.contains("server"),
        "status should include server stats"
    );
    assert!(
        text.contains("branch_diagnostics"),
        "status should include branch diagnostics"
    );
}

#[tokio::test]
async fn status_can_omit_verbose_branch_diagnostics() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_status",
        json!({"include_branch_diagnostics": false}),
        Some(json!({"uptime": 100})),
        None,
    )
    .await
    .unwrap();
    let text = extract_text(&result.value);

    assert_sealed_graph_statistics_are_unavailable(text);
    assert!(
        !text.contains("branch_diagnostics"),
        "compact status must omit the unbounded branch payload"
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn status_reports_daemon_owned_partial_history_catch_up() {
    let (cg, _env, dir) = setup_empty_project().await;
    let runtime = open_active_project_session_db(&cg).await;
    let transcript = dir.path().join("claude-backlog.jsonl");
    let file = fs::File::create(&transcript).unwrap();
    file.set_len(tracedecay_sessions::runtime::SESSION_TRANSCRIPT_STALLED_INGEST_WARNING_BYTES + 1)
        .unwrap();
    assert!(
        runtime
            .upsert_session_for_test(
                HostAdmissionScope::Project,
                &SessionRecord {
                    provider: "claude".to_string(),
                    session_id: "claude-backlog".to_string(),
                    project_key: cg.project_root().to_string_lossy().to_string(),
                    project_path: cg.project_root().to_string_lossy().to_string(),
                    title: Some("Claude backlog".to_string()),
                    started_at: Some(1),
                    ended_at: None,
                    transcript_path: Some(transcript.to_string_lossy().to_string()),
                    metadata_json: None,
                    parent_session_id: None,
                    is_subagent: false,
                    agent_id: None,
                    parent_tool_use_id: None,
                }
            )
            .await
            .unwrap()
    );

    let result = tracedecay::mcp::tools::handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_status",
        json!({"format": "json"}),
        None,
        None,
        tracedecay::mcp::tools::ToolCallRegistryOptions::with_session_authorities(
            runtime.mcp_session_authorities(),
        ),
    )
    .await
    .unwrap();
    let status = extract_json(&result.value);
    assert_eq!(status["session_history_catch_up"]["status"], "warming");
    assert_eq!(status["session_history_catch_up"]["coverage"], "partial");
    assert_eq!(status["session_history_catch_up"]["authority"], "daemon");
    let text = status.to_string();
    assert!(text.contains("continues bounded background catch-up"));
    assert!(!text.contains("tracedecay sessions ingest --project-path"));
    assert!(!text.contains("hook catch-up cap"));
    assert!(!text.contains("tracedecay doctor --agent cursor"));
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn runtime_exposes_cursor_ingest_health_for_daemon_owned_doctor_checks() {
    let (cg, _env, dir) = setup_empty_project().await;
    let runtime = open_active_project_session_db(&cg).await;
    let transcript = dir.path().join("cursor-backlog.jsonl");
    fs::write(&transcript, b"pending cursor transcript").unwrap();
    for (session_id, transcript_path) in [
        ("cursor-backlog", transcript.to_string_lossy().into_owned()),
        (
            "cursor-placeholder",
            "${workspaceFolder}/.cursor/sessions/cursor-placeholder.jsonl".to_string(),
        ),
    ] {
        assert!(
            runtime
                .upsert_session_for_test(
                    HostAdmissionScope::Project,
                    &SessionRecord {
                        provider: "cursor".to_string(),
                        session_id: session_id.to_string(),
                        project_key: cg.project_root().to_string_lossy().to_string(),
                        project_path: cg.project_root().to_string_lossy().to_string(),
                        title: None,
                        started_at: Some(1),
                        ended_at: None,
                        transcript_path: Some(transcript_path),
                        metadata_json: None,
                        parent_session_id: None,
                        is_subagent: false,
                        agent_id: None,
                        parent_tool_use_id: None,
                    }
                )
                .await
                .unwrap()
        );
    }

    let result = tracedecay::mcp::tools::handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_runtime",
        json!({ "format": "json", "session_ingest_health": true }),
        None,
        None,
        tracedecay::mcp::tools::ToolCallRegistryOptions::with_session_authorities(
            runtime.mcp_session_authorities(),
        ),
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["cursor_session_ingest"]["tracked_transcripts"], 1);
    assert_eq!(payload["cursor_session_ingest"]["pending_transcripts"], 1);
    assert_eq!(
        payload["cursor_session_placeholder_paths"],
        json!(["${workspaceFolder}/.cursor/sessions/cursor-placeholder.jsonl"])
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn status_without_retained_session_authority_fails_closed() {
    let (cg, _env, _dir) = setup_empty_project().await;
    drop(open_active_project_session_db(&cg).await);

    let result = handle_tool_call(
        &cg,
        "tracedecay_status",
        json!({ "format": "json" }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();

    assert_eq!(payload["session_ingest"]["status"], "unavailable");
    assert_eq!(
        payload["session_ingest"]["message"],
        "daemon project session authority is unavailable"
    );
    assert!(payload.get("cursor_session_ingest").is_none());
}

// ---------------------------------------------------------------------------
// Extra: tracedecay_status without server_stats
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_status_without_server_stats() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(&cg, "tracedecay_status", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert_sealed_graph_statistics_are_unavailable(text);
    // Should NOT contain "server" key when None is passed
    assert!(
        !text.contains("\"server\""),
        "status without server_stats should not include 'server' key"
    );
}

#[tokio::test]
async fn test_status_reports_scope_prefix() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(&cg, "tracedecay_status", json!({}), None, Some("src/mcp"))
        .await
        .unwrap();
    let text = extract_text(&result.value);
    assert!(
        text.contains("scope_prefix"),
        "status should report scope_prefix"
    );
    assert!(
        text.contains("src/mcp"),
        "status should show the actual prefix value"
    );
    close_test_graph(cg).await;
}

#[tokio::test]
async fn test_status_no_scope_prefix() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(&cg, "tracedecay_status", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(
        parsed.get("scope_prefix").is_none() || parsed["scope_prefix"].is_null(),
        "status should not have scope_prefix when None"
    );
}

/// Issue #80: `tracedecay_runtime` must surface process + DB telemetry so
/// users hitting unexpected CPU/RAM can capture a structured snapshot
/// without leaving the chat session.
#[tokio::test]
async fn test_runtime_snapshot_exposes_process_and_db_signals() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let result = handle_tool_call(&cg, "tracedecay_runtime", json!({}), None, None)
        .await
        .unwrap();
    let text = extract_text(&result.value);
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();

    // Top-level envelope.
    assert!(parsed.get("captured_at").is_some());
    assert!(parsed["tracedecay_version"].is_string());
    assert!(parsed["host_os"].is_string());

    // Process block — PID must match our own.
    let proc = &parsed["process"];
    assert_eq!(
        proc["pid"].as_u64().unwrap_or(0),
        u64::from(std::process::id()),
        "snapshot must report this process's PID"
    );
    assert!(
        proc["rss_bytes"].as_u64().unwrap_or(0) > 0,
        "RSS should be non-zero"
    );
    assert!(proc["system_cpu_count"].as_u64().unwrap_or(0) >= 1);
    assert!(proc["system_total_memory_bytes"].as_u64().unwrap_or(0) > 0);

    // Database block — the DB file we just opened must be present and sized.
    let db = &parsed["database"];
    assert!(db["db_path"].is_string());
    assert!(
        db["db_size_bytes"].as_u64().unwrap_or(0) > 0,
        "DB file should have non-zero size"
    );
    assert_eq!(
        db["generation_census"],
        json!({
            "state": "unavailable",
            "reason": "authority_unavailable",
        }),
        "a direct runtime handler has no daemon-owned sealed-generation authority"
    );
    // journal_mode should remain visible through the canonical database status surface.
    assert!(db["journal_mode"].is_string() || db["journal_mode"].is_null());
    assert!(db.get("authority_audit_ok").is_none());
    assert!(db.get("authority_audit_reason").is_none());
    assert!(db.get("authority_audit_error").is_none());
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn test_runtime_snapshot_runs_authority_audit_only_when_requested() {
    let isolation = TempDir::new().unwrap();
    let project = isolation.path().join("project");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "pub fn indexed_fixture() -> bool { true }\n",
    )
    .unwrap();
    for args in [
        &["init", "--quiet"][..],
        &["add", "."][..],
        &[
            "-c",
            "user.name=TraceDecay Tests",
            "-c",
            "user.email=tests@tracedecay.invalid",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ][..],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(&project)
                .status()
                .unwrap()
                .success()
        );
    }
    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .unwrap();
    let response = harness
        .call_tool(
            &project,
            "tracedecay_runtime",
            json!({ "authority_audit": true, "format": "json" }),
        )
        .await
        .expect("production invocation succeeds");
    let result = response.result.unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    let db = &parsed["database"];
    assert_eq!(db["authority_audit_ok"], true);
    // A passing audit publishes the key but leaves both the typed reason and
    // the observed detail empty; a reason is only present when the audit failed
    // or could not run.
    assert!(
        db.as_object()
            .is_some_and(|fields| fields.contains_key("authority_audit_reason")),
        "an audit that ran must publish the reason key: {db}"
    );
    assert!(db["authority_audit_reason"].is_null());
    assert!(db["authority_audit_error"].is_null());

    let census = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let response = harness
                .call_tool(&project, "tracedecay_runtime", json!({ "format": "json" }))
                .await
                .expect("production runtime invocation succeeds");
            let result = response.result.expect("production runtime result");
            let payload: Value =
                serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
            let census = payload["database"]["generation_census"].clone();
            if census["state"] == "observed" {
                break census;
            }
            assert_eq!(
                census["reason"], "exact_scope_generation_not_ready",
                "the composed server may wait only for its exact sealed generation: {census}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("production route publishes its sealed generation census");
    assert!(census["source_total_bytes"].is_u64());
    assert!(census["symbol_count"].is_u64());
    assert!(census["edge_count"].is_u64());
}
