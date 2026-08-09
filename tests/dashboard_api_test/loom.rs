use crate::dashboard_api_support::*;
use tracedecay_sessions::runtime::git_correlation::{
    DEFAULT_SPAN_MERGE_GAP_SECS, SpanObservation, SpanSource,
};

#[test]
fn loom_temporal_endpoint_reads_recorded_ends_and_causal_authorities() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(true).await;
        let mut session = fixture
            .host_runtime
            .session_for_test(HostAdmissionScope::Project, "cursor", "sess-dashboard-1")
            .await
            .expect("read seeded Loom session")
            .expect("seeded Loom session");
        session.ended_at = Some(1_700_001_090);
        session.metadata_json = Some(
            serde_json::json!({
                "edited_files": [
                    {"path": "src/runtime.rs", "change_type": "edit", "hunks": 2}
                ]
            })
            .to_string(),
        );
        assert!(
            fixture
                .host_runtime
                .upsert_session_for_test(HostAdmissionScope::Project, &session)
                .await
                .expect("update Loom session")
        );
        fixture
            .host_runtime
            .record_project_span_for_test(
                &SpanObservation {
                    provider: "cursor".to_string(),
                    session_id: "sess-dashboard-1".to_string(),
                    thread_id: None,
                    branch: Some("main".to_string()),
                    worktree: fixture.project_root.display().to_string(),
                    ts: 1_700_001_020,
                    source: SpanSource::Ingest,
                },
                DEFAULT_SPAN_MERGE_GAP_SECS,
            )
            .await
            .expect("record Loom branch/worktree span");

        let agent = http_agent();
        let (status, envelope) = get_json(
            &agent,
            &format!("{}/api/loom/temporal?limit=200", fixture.base_url),
        );

        assert_eq!(status, 200, "{envelope}");
        assert_eq!(envelope["schema_revision"], 1);
        assert_eq!(envelope["domain_state"], "partial");
        assert_eq!(envelope["payload"]["available"], true);
        assert_eq!(
            envelope["payload"]["sessions"][0]["ended_at"],
            1_700_001_090
        );
        assert_eq!(
            envelope["payload"]["edited_files"][0]["path"],
            "src/runtime.rs"
        );
        assert_eq!(envelope["payload"]["branch_spans"][0]["branch"], "main");

        let statuses = envelope["payload"]["source_statuses"]
            .as_array()
            .unwrap_or_else(|| panic!("Loom source statuses should be an array: {envelope}"));
        let source = |id: &str| {
            statuses
                .iter()
                .find(|status| status["id"] == id)
                .unwrap_or_else(|| panic!("missing Loom source {id}: {envelope}"))
        };
        assert_eq!(source("session_commit")["state"], "ready");
        assert_eq!(source("session_file")["state"], "partial");
        assert_eq!(source("branch_worktree")["state"], "ready");
        assert_eq!(source("delivery_outcomes")["state"], "unsupported");
        assert!(
            source("delivery_outcomes")["required_authority"]
                .as_str()
                .is_some_and(|authority| {
                    authority.contains("GET /api/delivery/overview")
                        && authority.contains("session-linked")
                }),
            "unsupported source must name the missing authority: {envelope}"
        );
    });
}
