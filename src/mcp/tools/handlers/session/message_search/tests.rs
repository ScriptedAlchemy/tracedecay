use std::path::Path;
use std::sync::Mutex;

use serde_json::{Value, json};
use tracedecay_domain::{
    RetrievalAnchorId, SessionSourceCoverageV1, SessionSourceFrontierV1, SessionSourceIdV1,
    SessionTemporalCoverageRequestV1, TemporalCoverageCountsV1, TemporalModeV1,
};

use super::{
    SessionRetrievalCommand, SessionRetrievalExplanationView, SessionRetrievalPageView,
    SessionRetrievalServiceFuture, SessionRetrievalServiceOutcome, SessionRetrievalServicePort,
    SessionRetrievalStoreScope, SessionRetrievalUnavailable, SessionRetrievalUnavailableReason,
    SessionRetrievalWorkerBlocker, SessionRetrievalWorkerRetryClass,
    SessionRetrievalWorkerStatusView, SessionTemporalMetadataView, SessionTemporalWatermarksView,
    handle_message_search_with_service, render_temporal_message_search_md,
};
use crate::application::session::{
    SessionDataFreshness, SessionFreshnessPolicy, SessionRetrievalScope,
};
use crate::errors::TraceDecayError;
use crate::sessions::{SessionMessageRecord, SessionMessageSearchResult, SessionRecord};
use tracedecay_temporal_query::ports::{TemporalMessageTypeFilterV1, TemporalSessionScopeFilterV1};

#[derive(Default)]
struct RecordingService {
    commands: Mutex<Vec<SessionRetrievalCommand>>,
    outcome: Mutex<Option<SessionRetrievalServiceOutcome>>,
}

impl RecordingService {
    fn with_outcome(outcome: SessionRetrievalServiceOutcome) -> Self {
        Self {
            commands: Mutex::new(Vec::new()),
            outcome: Mutex::new(Some(outcome)),
        }
    }

    fn calls(&self) -> usize {
        self.commands.lock().unwrap().len()
    }

    fn command(&self) -> SessionRetrievalCommand {
        self.commands.lock().unwrap()[0].clone()
    }
}

impl SessionRetrievalServicePort for RecordingService {
    fn execute(&self, command: SessionRetrievalCommand) -> SessionRetrievalServiceFuture<'_> {
        self.commands.lock().unwrap().push(command);
        let outcome = self.outcome.lock().unwrap().clone().unwrap_or(
            SessionRetrievalServiceOutcome::CompleteZero {
                temporal: temporal(),
                freshness: SessionDataFreshness::Fresh,
            },
        );
        Box::pin(async move { outcome })
    }
}

fn temporal() -> SessionTemporalMetadataView {
    SessionTemporalMetadataView {
        anchors: vec![RetrievalAnchorId::new("anchor.message.1").unwrap()],
        watermarks: SessionTemporalWatermarksView::default(),
        coverage: TemporalCoverageCountsV1 {
            visible: 1,
            hidden: 2,
            unknown: 3,
            redacted: 4,
        },
        source_coverage: Vec::new(),
        cursor: Some("cursor.next".to_string()),
        explanations: vec![SessionRetrievalExplanationView {
            anchor: RetrievalAnchorId::new("anchor.message.1").unwrap(),
            summary: "exact phrase and current evidence".to_string(),
        }],
        omissions: Vec::new(),
        authorized_root: None,
    }
}

fn temporal_with_stale_source() -> SessionTemporalMetadataView {
    SessionTemporalMetadataView {
        source_coverage: vec![
            SessionSourceCoverageV1::from_frontiers(
                SessionSourceIdV1::new("cursor").unwrap(),
                SessionSourceFrontierV1::new(10),
                SessionSourceFrontierV1::new(5),
                SessionSourceFrontierV1::new(10),
                SessionTemporalCoverageRequestV1::new(TemporalModeV1::Current),
            )
            .unwrap(),
        ],
        ..temporal()
    }
}

fn json_args() -> Value {
    json!({
        "query": "database backup",
        "format": "json"
    })
}

fn response_payload(result: &crate::mcp::tools::ToolResult) -> Value {
    serde_json::from_str(result.value["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn existing_filters_translate_to_one_root_wide_temporal_query() {
    let service = RecordingService::default();
    let args = json!({
        "query": " database backup ",
        "provider": "claude",
        "project_key": "project-key",
        "parent_session_id": "parent-session",
        "include_subagents": false,
        "message_type": "direct_user",
        "since": 10,
        "until": 20,
        "branch": "feature/message-search",
        "workflow_run": "wf_123",
        "workflow_agent": "researcher",
        "limit": 7,
        "format": "json"
    });

    handle_message_search_with_service(
        Some(Path::new("/repo")),
        SessionRetrievalStoreScope::Project,
        args,
        Some(&service),
    )
    .await
    .unwrap();

    let command = service.command();
    assert_eq!(command.query().query(), "database backup");
    assert_eq!(command.query().provider(), Some("claude"));
    assert_eq!(command.query().limit(), 7);
    assert_eq!(
        command.query().retrieval_scope(),
        &SessionRetrievalScope::AllSessionsInAuthorizedRoot
    );
    assert_eq!(
        command.query().freshness_policy(),
        SessionFreshnessPolicy::AllowStored
    );
    assert_eq!(
        command.filters().project_key.as_deref(),
        Some("project-key")
    );
    assert_eq!(
        command.filters().parent_session_id.as_deref(),
        Some("parent-session")
    );
    assert_eq!(command.filters().scope.as_str(), "parents_only");
    assert_eq!(command.filters().message_type.as_str(), "direct_user");
    assert_eq!(command.filters().time_range.start_time, Some(10));
    assert_eq!(command.filters().time_range.end_time, Some(20));
    assert_eq!(
        command.filters().git_filter.branch.as_deref(),
        Some("feature/message-search")
    );
    assert_eq!(
        command
            .filters()
            .workflow_scope
            .as_ref()
            .map(|scope| scope.run_id.as_str()),
        Some("wf_123")
    );
    let semantic = command.query().semantic_filter();
    assert_eq!(semantic.project_key.as_deref(), Some("project-key"));
    assert_eq!(
        semantic.parent_session_id.as_deref(),
        Some("parent-session")
    );
    assert_eq!(
        semantic.session_scope,
        TemporalSessionScopeFilterV1::ParentsOnly
    );
    assert_eq!(
        semantic.message_type,
        TemporalMessageTypeFilterV1::DirectUser
    );
    assert_eq!(
        semantic.git_branch.as_deref(),
        Some("feature/message-search")
    );
    assert_eq!(semantic.workflow_run.as_deref(), Some("wf_123"));
    assert_eq!(semantic.workflow_agent.as_deref(), Some("researcher"));
    assert_eq!(
        (semantic.start_time, semantic.end_time),
        (Some(10), Some(20))
    );
    assert!(!command.goals());
}

#[tokio::test]
async fn compatibility_filters_bind_the_temporal_cursor_request() {
    let first = RecordingService::default();
    handle_message_search_with_service(
        Some(Path::new("/repo")),
        SessionRetrievalStoreScope::Project,
        json!({
            "query": "database backup",
            "scope": "parents_only",
            "message_type": "direct_user",
            "since": 10,
            "until": 20,
            "format": "json"
        }),
        Some(&first),
    )
    .await
    .unwrap();

    let changed = RecordingService::default();
    handle_message_search_with_service(
        Some(Path::new("/repo")),
        SessionRetrievalStoreScope::Project,
        json!({
            "query": "database backup",
            "scope": "subagents_only",
            "message_type": "tool_result",
            "since": 11,
            "until": 21,
            "format": "json"
        }),
        Some(&changed),
    )
    .await
    .unwrap();

    let first = first.command();
    let changed = changed.command();
    assert_ne!(
        first.query().compatibility_filter_digest(),
        changed.query().compatibility_filter_digest()
    );
    assert!(
        first
            .query()
            .compatibility_filter_digest()
            .is_some_and(|digest| digest.starts_with("sha256:"))
    );
}

#[tokio::test]
async fn goals_mode_keeps_query_optional_but_normal_search_requires_it() {
    let service = RecordingService::default();
    handle_message_search_with_service(
        Some(Path::new("/repo")),
        SessionRetrievalStoreScope::Project,
        json!({"goals": true, "format": "json"}),
        Some(&service),
    )
    .await
    .unwrap();
    let command = service.command();
    assert_eq!(command.query().query(), "");
    assert!(command.goals());
    assert!(command.query().semantic_filter().goals);

    let missing = RecordingService::default();
    let error = handle_message_search_with_service(
        Some(Path::new("/repo")),
        SessionRetrievalStoreScope::Project,
        json!({"format": "json"}),
        Some(&missing),
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("missing required parameter: query")
    );
    assert_eq!(missing.calls(), 0);
}

#[tokio::test]
async fn malformed_optional_arguments_are_rejected_without_broadening() {
    for (field, value) in [
        ("provider", json!(7)),
        ("project_key", json!(false)),
        ("parent_session_id", json!([])),
        ("include_subagents", json!("yes")),
        ("catch_up", json!(1)),
        ("limit", json!("ten")),
        ("project_scope", json!(true)),
    ] {
        let service = RecordingService::default();
        let mut args = json_args();
        args[field] = value;
        let error = handle_message_search_with_service(
            Some(Path::new("/repo")),
            SessionRetrievalStoreScope::Project,
            args,
            Some(&service),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains(field), "{error}");
        assert_eq!(service.calls(), 0);
    }

    let service = RecordingService::default();
    let mut args = json_args();
    args["workflow_agent"] = json!("researcher");
    let error = handle_message_search_with_service(
        Some(Path::new("/repo")),
        SessionRetrievalStoreScope::Project,
        args,
        Some(&service),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("workflow_run"), "{error}");
    assert_eq!(service.calls(), 0);

    let service = RecordingService::default();
    let mut args = json_args();
    args["include_subagents"] = json!(false);
    args["scope"] = json!("subagents_only");
    let error = handle_message_search_with_service(
        Some(Path::new("/repo")),
        SessionRetrievalStoreScope::Project,
        args,
        Some(&service),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("include_subagents"), "{error}");
    assert_eq!(service.calls(), 0);
}

#[tokio::test]
async fn catch_up_defaults_false_and_true_is_only_a_freshness_precondition() {
    let stored = RecordingService::default();
    let stored_result = handle_message_search_with_service(
        Some(Path::new("/repo")),
        SessionRetrievalStoreScope::Project,
        json_args(),
        Some(&stored),
    )
    .await
    .unwrap();
    let stored_payload = response_payload(&stored_result);
    assert_eq!(
        stored.command().query().freshness_policy(),
        SessionFreshnessPolicy::AllowStored
    );
    assert_eq!(stored_payload["catch_up"], false);
    assert_eq!(stored_payload["catch_up_performed"], false);
    assert_eq!(stored_payload["catch_up_failures"], json!([]));

    let fresh = RecordingService::default();
    let mut args = json_args();
    args["catch_up"] = json!(true);
    let fresh_result = handle_message_search_with_service(
        Some(Path::new("/repo")),
        SessionRetrievalStoreScope::Project,
        args,
        Some(&fresh),
    )
    .await
    .unwrap();
    let fresh_payload = response_payload(&fresh_result);
    assert_eq!(
        fresh.command().query().freshness_policy(),
        SessionFreshnessPolicy::RequireFresh
    );
    assert_eq!(fresh.calls(), 1);
    assert_eq!(fresh_payload["catch_up"], true);
    assert_eq!(fresh_payload["catch_up_performed"], false);
    assert_eq!(fresh_payload["catch_up_failures"], json!([]));
    assert_eq!(fresh_payload["refresh_required"], false);
}

#[tokio::test]
async fn stale_freshness_precondition_returns_coverage_and_typed_refresh_action() {
    let service = RecordingService::with_outcome(SessionRetrievalServiceOutcome::Stale {
        temporal: temporal_with_stale_source(),
        freshness: SessionDataFreshness::Stored { generation_lag: 5 },
    });
    let result = handle_message_search_with_service(
        Some(Path::new("/repo")),
        SessionRetrievalStoreScope::Project,
        json!({
            "query": "database backup",
            "catch_up": true,
            "format": "json"
        }),
        Some(&service),
    )
    .await
    .unwrap();
    let payload = response_payload(&result);

    assert_eq!(payload["status"], "stale");
    assert_eq!(payload["outcome"], "stale");
    assert_eq!(payload["refresh_required"], true);
    assert_eq!(payload["next_action"]["kind"], "session_refresh");
    assert_eq!(payload["next_action"]["tool"], "tracedecay_session_refresh");
    assert_eq!(payload["temporal"]["coverage"]["visible"], 1);
    assert_eq!(payload["temporal"]["coverage"]["hidden"], 2);
    assert_eq!(payload["temporal"]["freshness"]["generation_lag"], 5);
    assert_eq!(
        payload["temporal"]["source_coverage"][0]["source_id"],
        "cursor"
    );
    assert_eq!(
        payload["temporal"]["source_coverage"][0]["observed_frontier"],
        10
    );
    assert_eq!(
        payload["temporal"]["source_coverage"][0]["committed_frontier"],
        5
    );
    assert_eq!(
        payload["temporal"]["source_coverage"][0]["reason"]["kind"],
        "projection_behind_source"
    );
    assert_eq!(payload["catch_up_performed"], false);
    assert_eq!(payload["catch_up_failures"], json!([]));
}

#[tokio::test]
async fn partial_outcome_preserves_results_temporal_metadata_and_omissions() {
    let service = RecordingService::with_outcome(SessionRetrievalServiceOutcome::Partial {
        page: SessionRetrievalPageView {
            results: Vec::new(),
            temporal: temporal(),
        },
        freshness: SessionDataFreshness::Stored { generation_lag: 2 },
        omitted: 9,
    });
    let result = handle_message_search_with_service(
        Some(Path::new("/repo")),
        SessionRetrievalStoreScope::Project,
        json!({
            "query": "database backup",
            "catch_up": true,
            "format": "json"
        }),
        Some(&service),
    )
    .await
    .unwrap();
    let payload = response_payload(&result);

    assert_eq!(payload["status"], "partial");
    assert_eq!(payload["outcome"], "partial");
    assert_eq!(payload["omitted"], 9);
    assert_eq!(payload["refresh_required"], true);
    assert_eq!(payload["temporal"]["anchors"][0], "anchor.message.1");
    assert_eq!(payload["temporal"]["cursor"], "cursor.next");
    assert_eq!(
        payload["temporal"]["explanations"][0]["summary"],
        "exact phrase and current evidence"
    );
}

#[tokio::test]
async fn non_finite_result_score_is_rejected_instead_of_rendered_as_null() {
    let service = RecordingService::with_outcome(SessionRetrievalServiceOutcome::Complete {
        page: SessionRetrievalPageView {
            results: vec![SessionMessageSearchResult {
                session: SessionRecord {
                    provider: "claude".to_string(),
                    session_id: "session-non-finite".to_string(),
                    project_key: "project".to_string(),
                    project_path: "/project".to_string(),
                    title: None,
                    started_at: Some(10),
                    ended_at: None,
                    transcript_path: None,
                    metadata_json: None,
                    parent_session_id: None,
                    is_subagent: false,
                    agent_id: None,
                    parent_tool_use_id: None,
                },
                message: SessionMessageRecord {
                    provider: "claude".to_string(),
                    message_id: "message-non-finite".to_string(),
                    session_id: "session-non-finite".to_string(),
                    role: "assistant".to_string(),
                    timestamp: Some(20),
                    ordinal: 1,
                    text: "result must not disappear".to_string(),
                    kind: None,
                    model: None,
                    tool_names: None,
                    source_path: None,
                    source_offset: None,
                    metadata_json: None,
                },
                score: f64::NAN,
            }],
            temporal: temporal(),
        },
        freshness: SessionDataFreshness::Fresh,
    });

    let error = handle_message_search_with_service(
        Some(Path::new("/repo")),
        SessionRetrievalStoreScope::Project,
        json_args(),
        Some(&service),
    )
    .await
    .unwrap_err();

    assert!(matches!(error, TraceDecayError::Config { .. }), "{error}");
    assert!(
        error.to_string().contains("score must be finite"),
        "{error}"
    );
}

#[tokio::test]
async fn fresh_partial_outcome_uses_cursor_without_requesting_refresh() {
    let service = RecordingService::with_outcome(SessionRetrievalServiceOutcome::Partial {
        page: SessionRetrievalPageView {
            results: Vec::new(),
            temporal: temporal(),
        },
        freshness: SessionDataFreshness::Fresh,
        omitted: 3,
    });
    let result = handle_message_search_with_service(
        Some(Path::new("/repo")),
        SessionRetrievalStoreScope::Project,
        json!({
            "query": "database backup",
            "catch_up": true,
            "format": "json"
        }),
        Some(&service),
    )
    .await
    .unwrap();
    let payload = response_payload(&result);

    assert_eq!(payload["outcome"], "partial");
    assert_eq!(payload["omitted"], 3);
    assert_eq!(payload["temporal"]["freshness"]["state"], "fresh");
    assert_eq!(payload["temporal"]["cursor"], "cursor.next");
    assert_eq!(payload["refresh_required"], false);
    assert!(payload["next_action"].is_null());
}

#[tokio::test]
async fn all_registered_defers_without_invoking_retrieval() {
    let service = RecordingService::default();
    let result = handle_message_search_with_service(
        Some(Path::new("/repo")),
        SessionRetrievalStoreScope::Project,
        json!({
            "query": "database backup",
            "project_scope": "all_registered",
            "format": "json"
        }),
        Some(&service),
    )
    .await
    .unwrap();
    let payload = response_payload(&result);

    assert_eq!(service.calls(), 0);
    assert_eq!(payload["status"], "deferred");
    assert_eq!(payload["outcome"], "deferred");
    assert_eq!(payload["project_scope"], "all_registered");
    assert_eq!(
        payload["error"]["code"],
        "session_retrieval_multi_root_deferred"
    );
    assert_eq!(payload["error"]["retryable"], false);
}

#[tokio::test]
async fn explicit_project_selector_and_profile_dispatch_remain_single_root() {
    let project = RecordingService::default();
    handle_message_search_with_service(
        Some(Path::new("/repo")),
        SessionRetrievalStoreScope::Project,
        json!({
            "query": "database backup",
            "project_selector": {
                "project_id": "project.target",
                "path": "/target"
            },
            "format": "json"
        }),
        Some(&project),
    )
    .await
    .unwrap();
    let project_command = project.command();
    assert_eq!(
        project_command.store_scope(),
        SessionRetrievalStoreScope::Project
    );
    assert_eq!(
        project_command
            .project_selector()
            .and_then(|selector| selector.project_id.as_deref()),
        Some("project.target")
    );
    assert_eq!(
        project_command
            .project_selector()
            .and_then(|selector| selector.project_path.as_deref()),
        Some("/target")
    );

    let profile = RecordingService::default();
    handle_message_search_with_service(
        None,
        SessionRetrievalStoreScope::Profile,
        json_args(),
        Some(&profile),
    )
    .await
    .unwrap();
    assert_eq!(
        profile.command().store_scope(),
        SessionRetrievalStoreScope::Profile
    );
    assert!(profile.command().project_selector().is_none());
}

#[tokio::test]
async fn complete_zero_and_terminal_error_outcomes_are_typed() {
    let complete_zero =
        RecordingService::with_outcome(SessionRetrievalServiceOutcome::CompleteZero {
            temporal: temporal(),
            freshness: SessionDataFreshness::Fresh,
        });
    let complete_zero_result = handle_message_search_with_service(
        Some(Path::new("/repo")),
        SessionRetrievalStoreScope::Project,
        json_args(),
        Some(&complete_zero),
    )
    .await
    .unwrap();
    let complete_zero_payload = response_payload(&complete_zero_result);
    assert_eq!(complete_zero_payload["status"], "ok");
    assert_eq!(complete_zero_payload["outcome"], "complete_zero");
    assert_eq!(
        complete_zero_payload["temporal"]["freshness"]["state"],
        "fresh"
    );
    assert_eq!(
        complete_zero_payload["temporal"]["anchors"][0],
        "anchor.message.1"
    );
    assert_eq!(complete_zero_payload["temporal"]["cursor"], "cursor.next");
    assert_eq!(
        complete_zero_payload["temporal"]["explanations"][0]["summary"],
        "exact phrase and current evidence"
    );

    let complete = RecordingService::with_outcome(SessionRetrievalServiceOutcome::Complete {
        page: SessionRetrievalPageView {
            results: Vec::new(),
            temporal: temporal(),
        },
        freshness: SessionDataFreshness::Fresh,
    });
    let complete_result = handle_message_search_with_service(
        Some(Path::new("/repo")),
        SessionRetrievalStoreScope::Project,
        json_args(),
        Some(&complete),
    )
    .await
    .unwrap();
    let complete_payload = response_payload(&complete_result);
    assert_eq!(complete_payload["status"], "ok");
    assert_eq!(complete_payload["outcome"], "complete");
    assert_eq!(complete_payload["temporal"]["freshness"]["state"], "fresh");

    let terminal = [
        (
            SessionRetrievalServiceOutcome::WrongScope,
            "wrong_scope",
            "session_retrieval_wrong_scope",
        ),
        (
            SessionRetrievalServiceOutcome::Locked,
            "locked",
            "session_retrieval_locked",
        ),
        (
            SessionRetrievalServiceOutcome::Redacted,
            "redacted",
            "session_retrieval_redacted",
        ),
        (
            SessionRetrievalServiceOutcome::Deleted,
            "deleted",
            "session_retrieval_deleted",
        ),
        (
            SessionRetrievalServiceOutcome::Denied,
            "denied",
            "session_retrieval_denied",
        ),
        (
            SessionRetrievalServiceOutcome::Unavailable(
                SessionRetrievalUnavailable::service_not_configured(),
            ),
            "unavailable",
            "session_retrieval_service_unavailable",
        ),
        (
            SessionRetrievalServiceOutcome::BudgetExhausted,
            "budget_exhausted",
            "session_retrieval_budget_exhausted",
        ),
        (
            SessionRetrievalServiceOutcome::Cancelled,
            "cancelled",
            "session_retrieval_cancelled",
        ),
    ];
    for (outcome, status, code) in terminal {
        let service = RecordingService::with_outcome(outcome);
        let result = handle_message_search_with_service(
            Some(Path::new("/repo")),
            SessionRetrievalStoreScope::Project,
            json_args(),
            Some(&service),
        )
        .await
        .unwrap();
        let payload = response_payload(&result);
        assert_eq!(payload["status"], status);
        assert_eq!(payload["outcome"], status);
        assert_eq!(payload["error"]["code"], code);
    }
}

#[tokio::test]
async fn unavailable_outcome_exposes_typed_worker_status() {
    let service = RecordingService::with_outcome(SessionRetrievalServiceOutcome::Unavailable(
        SessionRetrievalUnavailable {
            reason: SessionRetrievalUnavailableReason::RefreshWorkerRecovering,
            worker: Some(SessionRetrievalWorkerStatusView {
                last_progress_at_unix_micros: Some(42),
                backlog: 7,
                blocker: Some(SessionRetrievalWorkerBlocker::WorkerPanicked),
                retry_class: Some(SessionRetrievalWorkerRetryClass::Projector),
            }),
        },
    ));

    let result = handle_message_search_with_service(
        Some(Path::new("/repo")),
        SessionRetrievalStoreScope::Project,
        json_args(),
        Some(&service),
    )
    .await
    .unwrap();
    let payload = response_payload(&result);

    assert_eq!(payload["error"]["reason"], "refresh_worker_recovering");
    assert_eq!(payload["error"]["retryable"], true);
    assert_eq!(
        payload["service_status"]["last_progress_at_unix_micros"],
        42
    );
    assert_eq!(payload["service_status"]["backlog"], 7);
    assert_eq!(payload["service_status"]["blocker"], "worker_panicked");
    assert_eq!(payload["service_status"]["retry_class"], "projector");
    let markdown = render_temporal_message_search_md(&payload).unwrap();
    assert!(markdown.contains("Unavailable reason: `refresh_worker_recovering`"));
    assert!(markdown.contains(
            "Refresh worker: last progress 42, backlog 7, blocker `worker_panicked`, retry class `projector`"
        ));
}

#[tokio::test]
async fn unavailable_outcome_reports_no_progress_backlog_as_stalled() {
    let service = RecordingService::with_outcome(SessionRetrievalServiceOutcome::Unavailable(
        SessionRetrievalUnavailable {
            reason: SessionRetrievalUnavailableReason::RefreshWorkerStalled,
            worker: Some(SessionRetrievalWorkerStatusView {
                last_progress_at_unix_micros: None,
                backlog: 14,
                blocker: Some(SessionRetrievalWorkerBlocker::Storage),
                retry_class: Some(SessionRetrievalWorkerRetryClass::Storage),
            }),
        },
    ));

    let result = handle_message_search_with_service(
        Some(Path::new("/repo")),
        SessionRetrievalStoreScope::Project,
        json_args(),
        Some(&service),
    )
    .await
    .unwrap();
    let payload = response_payload(&result);

    assert_eq!(payload["error"]["reason"], "refresh_worker_stalled");
    assert_eq!(payload["error"]["retryable"], true);
    assert_eq!(
        payload["service_status"]["last_progress_at_unix_micros"],
        Value::Null
    );
    assert_eq!(payload["service_status"]["backlog"], 14);
    assert_eq!(payload["service_status"]["blocker"], "storage");
    assert_eq!(payload["service_status"]["retry_class"], "storage");
}

#[test]
fn markdown_rejects_malformed_temporal_coverage() {
    let payload = json!({
        "query": "database backup",
        "provider": "all",
        "scope": "all",
        "count": 0,
        "results": [],
        "goals": false,
        "refresh_required": false,
        "temporal": {
            "coverage": {
                "visible": "one",
                "hidden": 2,
                "unknown": 3,
                "redacted": 4
            }
        }
    });

    let error = render_temporal_message_search_md(&payload).unwrap_err();

    assert!(matches!(error, TraceDecayError::Config { .. }), "{error}");
    assert!(
        error.to_string().contains("temporal.coverage.visible"),
        "{error}"
    );
}
