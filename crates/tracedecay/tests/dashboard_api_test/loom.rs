use std::collections::BTreeSet;
use std::time::Duration;

use crate::dashboard_api_support::*;
use serde_json::json;
use tracedecay_dashboard_api::{
    DashboardGitCorrelationReadFutureV1, DashboardGitCorrelationReadPortV1,
};
use tracedecay_global_db::ParseOffset;
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
            json!({
                "edited_files": [
                    {"path": "src/lib.rs", "change_type": "edit", "hunks": 1,
                     "edited_at_micros": 1_700_001_050_000_000i64},
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
        let delegated = SessionRecord {
            session_id: "sess-dashboard-child".to_string(),
            title: Some("Delegated explorer".to_string()),
            started_at: Some(1_700_001_030),
            ended_at: Some(1_700_001_060),
            metadata_json: None,
            parent_session_id: Some("sess-dashboard-1".to_string()),
            is_subagent: true,
            agent_id: Some("explorer".to_string()),
            parent_tool_use_id: Some("toolu_fork_01".to_string()),
            ..session.clone()
        };
        assert!(
            fixture
                .host_runtime
                .upsert_session_for_test(HostAdmissionScope::Project, &delegated)
                .await
                .expect("seed delegated Loom session")
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
        // Newest start first: the delegated child leads, then its parent.
        let sessions = &envelope["payload"]["sessions"];
        assert_eq!(sessions[0]["session_id"], "sess-dashboard-child");
        assert_eq!(sessions[0]["parent_session_id"], "sess-dashboard-1");
        assert_eq!(sessions[0]["parent_tool_use_id"], "toolu_fork_01");
        assert_eq!(sessions[0]["is_subagent"], true);
        assert_eq!(sessions[1]["session_id"], "sess-dashboard-1");
        assert_eq!(sessions[1]["ended_at"], 1_700_001_090);
        assert!(
            sessions[1].get("parent_session_id").is_none()
                && sessions[1].get("parent_tool_use_id").is_none(),
            "an unrecorded parent is absent, never null-as-root: {}",
            sessions[1]
        );

        // Files are served in path order; only the recorded timestamp is carried.
        let edited_files = &envelope["payload"]["edited_files"];
        assert_eq!(edited_files[0]["path"], "src/lib.rs");
        assert_eq!(
            edited_files[0]["edited_at_micros"],
            1_700_001_050_000_000i64
        );
        assert_eq!(edited_files[1]["path"], "src/runtime.rs");
        assert!(
            edited_files[1].get("edited_at_micros").is_none(),
            "a rollup without a recorded time must not fabricate one: {}",
            edited_files[1]
        );
        assert_eq!(edited_files.as_array().map(Vec::len), Some(2));
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
        assert_eq!(statuses.len(), 3);
    });
}

const LARGE_HISTORY_SESSIONS: usize = 2_000;
const LARGE_HISTORY_MESSAGES: usize = 12;

fn large_history_session(
    project: &DashboardTestRuntimeV1,
    root: &Path,
    index: usize,
) -> SessionRecord {
    SessionRecord {
        provider: "cursor".to_string(),
        session_id: format!("large-{index:04}"),
        project_key: project.project_id().as_str().to_string(),
        project_path: root.display().to_string(),
        title: Some(format!("Large history session {index}")),
        started_at: Some(1_700_000_000 + i64::try_from(index * 600).expect("start offset")),
        ended_at: None,
        transcript_path: None,
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    }
}

fn large_history_messages(session: &SessionRecord) -> Vec<SessionMessageRecord> {
    let started_at = session.started_at.expect("large history start");
    (0..LARGE_HISTORY_MESSAGES)
        .map(|ordinal| {
            let offset = i64::try_from(ordinal).expect("message ordinal");
            message(
                &format!("{}-msg-{ordinal:02}", session.session_id),
                &session.session_id,
                if ordinal % 2 == 0 {
                    "user"
                } else {
                    "assistant"
                },
                offset + 1,
                "synthetic Loom history turn",
                MessageDetails {
                    timestamp: started_at + offset * 10,
                    model: (ordinal % 2 == 1).then_some("gpt-large"),
                    metadata_json: None,
                },
            )
        })
        .collect()
}

#[test]
fn loom_temporal_serves_one_bounded_page_of_a_large_history() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        for index in 0..LARGE_HISTORY_SESSIONS {
            let session =
                large_history_session(&fixture.host_runtime, &fixture.project_root, index);
            fixture
                .host_runtime
                .upsert_transcript_batch_for_test(
                    HostAdmissionScope::Project,
                    &session,
                    &large_history_messages(&session),
                    &format!("large-history:{}", session.session_id),
                    ParseOffset::default(),
                )
                .await
                .unwrap_or_else(|error| panic!("seed {}: {error}", session.session_id));
        }

        // The suite's standard HTTP client gives up after four seconds; the
        // first page must arrive inside that budget regardless of how much
        // history precedes it.
        let agent = http_agent();
        let (status, first) = get_json(
            &agent,
            &format!("{}/api/loom/temporal?limit=25&offset=0", fixture.base_url),
        );
        assert_eq!(status, 200, "{first}");
        assert_eq!(first["payload"]["total"], 2_000);
        let sessions = first["payload"]["sessions"]
            .as_array()
            .unwrap_or_else(|| panic!("Loom sessions should be an array: {first}"));
        assert_eq!(sessions.len(), 25);
        assert_eq!(sessions[0]["session_id"], "large-1999");
        assert_eq!(sessions[0]["messages"], 12);
        assert_eq!(sessions[0]["started_at"], 1_701_199_400);
        assert_eq!(sessions[0]["last_message_at"], 1_701_199_510);
        assert_eq!(sessions[0]["models"], json!([{ "model": "gpt-large" }]));
        assert_eq!(sessions[24]["session_id"], "large-1975");
        assert_eq!(first["coverage"]["examined"], 25);
        assert_eq!(first["coverage"]["denominator"], 2_000);

        let (status, last) = get_json(
            &agent,
            &format!(
                "{}/api/loom/temporal?limit=25&offset=1990",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200, "{last}");
        let sessions = last["payload"]["sessions"]
            .as_array()
            .unwrap_or_else(|| panic!("Loom sessions should be an array: {last}"));
        assert_eq!(sessions.len(), 10);
        assert_eq!(sessions[0]["session_id"], "large-0009");
        assert_eq!(sessions[9]["session_id"], "large-0000");
        assert_eq!(sessions[9]["messages"], 12);
        assert_eq!(sessions[9]["last_message_at"], 1_700_000_110);
    });
}

/// A Git evidence read that never answers, the shape of a projection read
/// stuck behind store convergence.
struct StalledGitCorrelationRead;

impl DashboardGitCorrelationReadPortV1 for StalledGitCorrelationRead {
    fn read(&self, _session_ids: BTreeSet<String>) -> DashboardGitCorrelationReadFutureV1<'_> {
        Box::pin(std::future::pending())
    }
}

#[test]
fn loom_temporal_read_past_its_deadline_answers_the_typed_timeout() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture_with_git_correlation_authority(Arc::new(
            StalledGitCorrelationRead,
        ))
        .await;

        // Longer than the route's admitted request deadline, so the answer
        // comes from the route rather than from the client giving up.
        let agent = http_agent_with_timeout(Duration::from_secs(90));
        let url = format!("{}/api/loom/temporal?limit=25&offset=0", fixture.base_url);
        let (status, body) = response_to_json(
            agent
                .get(&url)
                .call()
                .unwrap_or_else(|error| panic!("GET {url} got no answer: {error}")),
        );

        assert_eq!(status, 504, "{body}");
        assert_eq!(body["domain_state"], "timed_out");
        assert_eq!(body["payload"], Value::Null);
        assert_eq!(
            body["coverage"]["omission_reasons"][0],
            "loom_temporal_read_timed_out"
        );
        assert_eq!(
            body["legal_actions"],
            json!([{
                "kind": "refresh",
                "operation": "use-case.dashboard.loom.temporal.refresh"
            }])
        );
    });
}
