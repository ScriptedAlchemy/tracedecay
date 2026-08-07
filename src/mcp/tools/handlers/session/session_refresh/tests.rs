use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};
use tracedecay_domain::{
    SessionSourceCoverageV1, SessionSourceFrontierV1, SessionSourceIdV1,
    SessionTemporalCoverageRequestV1, TemporalModeV1,
};

use super::{
    SessionRefreshCommand, SessionRefreshCoverageView, SessionRefreshFrontierView,
    SessionRefreshProgressView, SessionRefreshReceiptView, SessionRefreshServiceOutcome,
    SessionRefreshServicePort, SessionRefreshServices, handle_session_refresh,
};
use crate::mcp::tools::get_tool_definitions;

fn refresh_args(scope: &str, action: &str) -> Value {
    let mut args = json!({
        "action": action,
        "scope": scope,
        "profile": { "id": "profile.primary" },
        "session": {
            "id": "session.refresh",
            "store_id": "store.refresh",
            "root_id": "root.refresh"
        },
        "source": { "scope": "cursor" },
        "target": {
            "temporal_mode": { "kind": "current" },
            "grain": "logical_message",
            "frontier": {
                "observed_through": 4,
                "committed_through": 0
            }
        },
        "format": "json"
    });
    if scope == "project" {
        args.as_object_mut().unwrap().insert(
            "project".to_string(),
            json!({
                "id": "project.tracedecay",
                "repository_id": "repository.tracedecay",
                "worktree_id": "worktree.tracedecay",
                "branch_id": "branch.main"
            }),
        );
    }
    if matches!(action, "status" | "cancel") {
        args.as_object_mut()
            .unwrap()
            .insert("handle".to_string(), json!("refresh-handle"));
    }
    args
}

fn response_text(value: &Value) -> &str {
    value["content"][0]["text"].as_str().unwrap()
}

fn stale_source_coverage() -> SessionSourceCoverageV1 {
    SessionSourceCoverageV1::from_frontiers(
        SessionSourceIdV1::new("cursor").unwrap(),
        SessionSourceFrontierV1::new(4),
        SessionSourceFrontierV1::new(2),
        SessionSourceFrontierV1::new(4),
        SessionTemporalCoverageRequestV1::new(TemporalModeV1::Current),
    )
    .unwrap()
}

fn assert_closed_objects(schema: &Value) {
    if schema.get("type").and_then(Value::as_str) == Some("object") {
        assert_eq!(
            schema.get("additionalProperties"),
            Some(&Value::Bool(false)),
            "object schema is not closed: {schema}"
        );
    }
    if let Some(object) = schema.as_object() {
        for value in object.values() {
            assert_closed_objects(value);
        }
    } else if let Some(array) = schema.as_array() {
        for value in array {
            assert_closed_objects(value);
        }
    }
}

#[test]
fn session_refresh_definition_is_mutating_typed_and_closed() {
    let definition = get_tool_definitions()
        .into_iter()
        .find(|definition| definition.name == "tracedecay_session_refresh")
        .expect("session refresh definition");

    assert_eq!(
        definition.annotations.as_ref().unwrap()["readOnlyHint"],
        false
    );
    assert_eq!(
        definition.input_schema["properties"]["action"]["enum"],
        json!(["start", "status", "join", "resume", "cancel", "begin"])
    );
    assert_eq!(
        definition.input_schema["properties"]["format"]["enum"],
        json!(["markdown", "json"])
    );
    assert_eq!(
        definition.input_schema["properties"]["scope"]["enum"],
        json!(["project", "profile"])
    );
    assert_eq!(
        definition.input_schema["required"],
        json!(["action", "scope", "profile", "session", "source", "target"])
    );
    assert_eq!(
        definition.input_schema["allOf"][2]["if"]["properties"]["action"]["enum"],
        json!(["start", "join", "resume", "begin"])
    );
    assert_closed_objects(&definition.input_schema);
}

struct RecordingService {
    calls: AtomicUsize,
    commands: Mutex<Vec<SessionRefreshCommand>>,
    outcome: SessionRefreshServiceOutcome,
}

impl RecordingService {
    fn new(outcome: SessionRefreshServiceOutcome) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            commands: Mutex::new(Vec::new()),
            outcome,
        }
    }

    fn commands(&self) -> Vec<SessionRefreshCommand> {
        self.commands.lock().unwrap().clone()
    }

    fn actions(&self) -> Vec<super::SessionRefreshAction> {
        self.commands()
            .into_iter()
            .map(|command| command.action)
            .collect()
    }
}

impl SessionRefreshServicePort for RecordingService {
    fn execute<'a>(
        &'a self,
        command: SessionRefreshCommand,
    ) -> Pin<Box<dyn Future<Output = SessionRefreshServiceOutcome> + Send + 'a>> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        self.commands.lock().unwrap().push(command);
        let outcome = self.outcome.clone();
        Box::pin(async move { outcome })
    }
}

#[tokio::test]
async fn project_begin_dispatches_to_project_service_and_renders_stable_json() {
    let project = RecordingService::new(SessionRefreshServiceOutcome::Started {
        operation_id: "refresh-operation".to_string(),
        handle: "refresh-handle".to_string(),
        accepted_at: 123,
    });
    let profile = RecordingService::new(SessionRefreshServiceOutcome::Unavailable);

    let result = handle_session_refresh(
        refresh_args("project", "begin"),
        SessionRefreshServices::new(Some(&project), Some(&profile)),
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(response_text(&result.value)).unwrap();

    assert_eq!(payload["tool"], "tracedecay_session_refresh");
    assert_eq!(payload["action"], "begin");
    assert_eq!(payload["scope"], "project");
    assert_eq!(payload["outcome"], "started");
    assert_eq!(payload["operation_id"], "refresh-operation");
    assert_eq!(payload["handle"], "refresh-handle");
    assert_eq!(project.calls.load(Ordering::Acquire), 1);
    assert_eq!(profile.calls.load(Ordering::Acquire), 0);
    let commands = project.commands();
    let command = &commands[0];
    command
        .binding
        .validate_context(&command.context)
        .expect("refresh command binding matches application context");
    let identity = command.binding.identity();
    let git_route = identity.git_route().expect("project git route");
    assert_eq!(
        identity.project_id().unwrap().as_str(),
        "project.tracedecay"
    );
    assert_eq!(identity.store_id().as_str(), "store.refresh");
    assert_eq!(identity.root_id().as_str(), "root.refresh");
    assert_eq!(git_route.repository_id().as_str(), "repository.tracedecay");
    assert_eq!(git_route.worktree_id().as_str(), "worktree.tracedecay");
    assert_eq!(git_route.branch_id().as_str(), "branch.main");
    assert_eq!(command.target.session_id().as_str(), "session.refresh");
    assert_eq!(command.target.source_scope(), Some("cursor"));
    assert_eq!(command.target.frozen_frontier().observed_through(), 4);
    assert_eq!(command.target.frozen_frontier().committed_through(), 0);
    assert!(command.handle.is_none());
    assert_eq!(result.semantic_error(), Some(false));
}

#[tokio::test]
async fn start_join_resume_and_begin_share_begin_or_join_authority() {
    let profile = RecordingService::new(SessionRefreshServiceOutcome::Joined {
        operation_id: "refresh-operation".to_string(),
        handle: "refresh-handle".to_string(),
        accepted_at: 123,
    });

    for action in ["start", "join", "resume", "begin"] {
        let result = handle_session_refresh(
            refresh_args("profile", action),
            SessionRefreshServices::new(None, Some(&profile)),
        )
        .await
        .unwrap();
        let payload: Value = serde_json::from_str(response_text(&result.value)).unwrap();

        assert_eq!(payload["action"], action);
        assert_eq!(payload["outcome"], "joined");
        assert_eq!(payload["handle"], "refresh-handle");
        assert_eq!(result.semantic_error(), Some(false));
    }
    assert_eq!(
        profile.actions(),
        vec![super::SessionRefreshAction::Begin; 4]
    );
}

#[tokio::test]
async fn transport_injected_request_id_is_stripped_before_typed_parsing() {
    let profile = RecordingService::new(SessionRefreshServiceOutcome::Started {
        operation_id: "refresh-operation".to_string(),
        handle: "refresh-handle".to_string(),
        accepted_at: 123,
    });
    let mut args = refresh_args("profile", "start");
    args.as_object_mut()
        .unwrap()
        .insert("__mcp_request_id".to_string(), json!("request.mcp.fixture"));

    let result = handle_session_refresh(args, SessionRefreshServices::new(None, Some(&profile)))
        .await
        .unwrap();
    let payload: Value = serde_json::from_str(response_text(&result.value)).unwrap();

    assert_eq!(payload["outcome"], "started");
    assert_eq!(profile.calls.load(Ordering::Acquire), 1);
    assert_eq!(result.semantic_error(), Some(false));
}

#[tokio::test]
async fn missing_daemon_authority_returns_unavailable_without_fallback() {
    let result = handle_session_refresh(
        refresh_args("profile", "status"),
        SessionRefreshServices::default(),
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(response_text(&result.value)).unwrap();

    assert_eq!(payload["outcome"], "unavailable");
    assert_eq!(payload["error"]["code"], "refresh_service_unavailable");
    assert_eq!(result.semantic_error(), Some(true));
}

#[tokio::test]
async fn wrong_scope_is_a_stable_semantic_error() {
    let profile = RecordingService::new(SessionRefreshServiceOutcome::WrongScope);
    let result = handle_session_refresh(
        refresh_args("profile", "status"),
        SessionRefreshServices::new(None, Some(&profile)),
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(response_text(&result.value)).unwrap();

    assert_eq!(payload["outcome"], "wrong_scope");
    assert_eq!(payload["error"]["code"], "refresh_wrong_scope");
    assert_eq!(result.semantic_error(), Some(true));
}

#[tokio::test]
async fn stale_not_found_aborted_and_deadline_are_non_leaking_semantic_errors() {
    for (outcome, name, code) in [
        (
            SessionRefreshServiceOutcome::Stale,
            "stale",
            "refresh_handle_stale",
        ),
        (
            SessionRefreshServiceOutcome::NotFound,
            "not_found",
            "refresh_handle_not_found",
        ),
        (
            SessionRefreshServiceOutcome::Aborted,
            "aborted",
            "refresh_aborted",
        ),
        (
            SessionRefreshServiceOutcome::DeadlineExceeded,
            "deadline_exceeded",
            "refresh_deadline_exceeded",
        ),
    ] {
        let profile = RecordingService::new(outcome);
        let result = handle_session_refresh(
            refresh_args("profile", "status"),
            SessionRefreshServices::new(None, Some(&profile)),
        )
        .await
        .unwrap();
        let payload: Value = serde_json::from_str(response_text(&result.value)).unwrap();

        assert_eq!(payload["outcome"], name);
        assert_eq!(payload["error"]["code"], code);
        assert!(payload.get("receipt").is_none());
        assert!(payload.get("operation_id").is_none());
        assert_eq!(result.semantic_error(), Some(true));
    }
}

#[tokio::test]
async fn profile_authorization_never_falls_back_to_project_service() {
    let project = RecordingService::new(SessionRefreshServiceOutcome::Started {
        operation_id: "wrong-authority".to_string(),
        handle: "wrong-authority".to_string(),
        accepted_at: 1,
    });

    let result = handle_session_refresh(
        refresh_args("profile", "begin"),
        SessionRefreshServices::new(Some(&project), None),
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(response_text(&result.value)).unwrap();

    assert_eq!(payload["outcome"], "unavailable");
    assert_eq!(project.calls.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn authorization_denial_is_stable_and_markdown_is_human_readable() {
    let profile = RecordingService::new(SessionRefreshServiceOutcome::Denied);
    let mut args = refresh_args("profile", "cancel");
    args.as_object_mut().unwrap().remove("format");

    let result = handle_session_refresh(args, SessionRefreshServices::new(None, Some(&profile)))
        .await
        .unwrap();
    let markdown = response_text(&result.value);

    assert!(markdown.starts_with("# Session Refresh\n"));
    assert!(markdown.contains("- Action: `cancel`"));
    assert!(markdown.contains("- Scope: `profile`"));
    assert!(markdown.contains("- Outcome: `denied`"));
    assert_eq!(result.semantic_error(), Some(true));
}

#[tokio::test]
async fn status_progress_is_stable_in_json_and_markdown() {
    let profile = RecordingService::new(SessionRefreshServiceOutcome::Running(Some(
        SessionRefreshProgressView {
            operation_id: "refresh-operation".to_string(),
            session_id: "session.refresh".to_string(),
            frontier: SessionRefreshFrontierView {
                observed_through: 4,
                committed_through: 2,
            },
            coverage: SessionRefreshCoverageView {
                visible: 3,
                hidden: 1,
                unknown: 0,
                redacted: 0,
            },
            source_coverage: vec![stale_source_coverage()],
            committed_batches: 2,
            committed_records: 4,
            updated_at: 123,
        },
    )));
    let args = refresh_args("profile", "status");
    let result = handle_session_refresh(
        args.clone(),
        SessionRefreshServices::new(None, Some(&profile)),
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(response_text(&result.value)).unwrap();
    assert_eq!(payload["outcome"], "running");
    assert_eq!(payload["progress"]["frontier"]["committed_through"], 2);
    assert_eq!(payload["progress"]["coverage"]["hidden"], 1);
    assert_eq!(
        payload["progress"]["source_coverage"][0]["source_id"],
        "cursor"
    );
    assert_eq!(
        payload["progress"]["source_coverage"][0]["reason"]["kind"],
        "projection_behind_source"
    );

    let mut markdown_args = args;
    markdown_args.as_object_mut().unwrap().remove("format");
    let result = handle_session_refresh(
        markdown_args,
        SessionRefreshServices::new(None, Some(&profile)),
    )
    .await
    .unwrap();
    let markdown = response_text(&result.value);
    assert!(markdown.contains("- Frontier: 2/4 committed"));
    assert!(markdown.contains("- Coverage: visible 3, hidden 1, unknown 0, redacted 0"));
    assert!(markdown.contains("- Committed records: 4"));
    let commands = profile.commands();
    assert_eq!(commands.len(), 2);
    assert!(commands.iter().all(|command| {
        command.action == super::SessionRefreshAction::Status
            && command.handle.as_deref() == Some("refresh-handle")
            && command.target.session_id().as_str() == "session.refresh"
    }));
}

#[tokio::test]
async fn terminal_receipt_preserves_failure_and_coverage_details() {
    let profile = RecordingService::new(SessionRefreshServiceOutcome::Failed(
        SessionRefreshReceiptView {
            operation_id: "refresh-operation".to_string(),
            session_id: "session.refresh".to_string(),
            frontier: SessionRefreshFrontierView {
                observed_through: 4,
                committed_through: 3,
            },
            coverage: SessionRefreshCoverageView {
                visible: 2,
                hidden: 1,
                unknown: 1,
                redacted: 0,
            },
            source_coverage: vec![stale_source_coverage()],
            state: "failed".to_string(),
            failure_code: Some("projector_failed".to_string()),
            terminal_at: 456,
        },
    ));
    let result = handle_session_refresh(
        refresh_args("profile", "status"),
        SessionRefreshServices::new(None, Some(&profile)),
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(response_text(&result.value)).unwrap();

    assert_eq!(payload["outcome"], "failed");
    assert_eq!(payload["receipt"]["state"], "failed");
    assert_eq!(payload["receipt"]["failure_code"], "projector_failed");
    assert_eq!(payload["receipt"]["coverage"]["unknown"], 1);
    assert_eq!(
        payload["receipt"]["source_coverage"][0]["committed_frontier"],
        2
    );
    assert_eq!(payload["error"]["code"], "refresh_failed");
    assert_eq!(result.semantic_error(), Some(true));
}

#[tokio::test]
async fn busy_complete_and_cancelled_keep_typed_semantics() {
    let busy = RecordingService::new(SessionRefreshServiceOutcome::Busy);
    let busy_result = handle_session_refresh(
        refresh_args("profile", "start"),
        SessionRefreshServices::new(None, Some(&busy)),
    )
    .await
    .unwrap();
    let busy_payload: Value = serde_json::from_str(response_text(&busy_result.value)).unwrap();
    assert_eq!(busy_payload["outcome"], "busy");
    assert_eq!(busy_payload["error"]["code"], "refresh_busy");
    assert_eq!(busy_result.semantic_error(), Some(true));

    let receipt = SessionRefreshReceiptView {
        operation_id: "refresh-operation".to_string(),
        session_id: "session.refresh".to_string(),
        frontier: SessionRefreshFrontierView {
            observed_through: 4,
            committed_through: 4,
        },
        coverage: SessionRefreshCoverageView {
            visible: 4,
            hidden: 0,
            unknown: 0,
            redacted: 0,
        },
        source_coverage: Vec::new(),
        state: "complete".to_string(),
        failure_code: None,
        terminal_at: 456,
    };
    let complete = RecordingService::new(SessionRefreshServiceOutcome::Complete(receipt.clone()));
    let complete_result = handle_session_refresh(
        refresh_args("profile", "status"),
        SessionRefreshServices::new(None, Some(&complete)),
    )
    .await
    .unwrap();
    let complete_payload: Value =
        serde_json::from_str(response_text(&complete_result.value)).unwrap();
    assert_eq!(complete_payload["outcome"], "complete");
    assert!(complete_payload.get("error").is_none());
    assert_eq!(complete_result.semantic_error(), Some(false));

    let cancelled = RecordingService::new(SessionRefreshServiceOutcome::Cancelled(receipt));
    let cancelled_result = handle_session_refresh(
        refresh_args("profile", "cancel"),
        SessionRefreshServices::new(None, Some(&cancelled)),
    )
    .await
    .unwrap();
    let cancelled_payload: Value =
        serde_json::from_str(response_text(&cancelled_result.value)).unwrap();
    assert_eq!(cancelled_payload["outcome"], "cancelled");
    assert_eq!(cancelled_result.semantic_error(), Some(false));
}

#[tokio::test]
async fn start_like_actions_reject_handles_and_status_requires_one() {
    for action in ["start", "join", "resume", "begin"] {
        let mut args = refresh_args("profile", action);
        args.as_object_mut()
            .unwrap()
            .insert("handle".to_string(), json!("unexpected"));
        let result = handle_session_refresh(args, SessionRefreshServices::default())
            .await
            .unwrap();
        let payload: Value = serde_json::from_str(response_text(&result.value)).unwrap();
        assert_eq!(payload["outcome"], "error");
        assert_eq!(payload["error"]["code"], "invalid_request");
        assert_eq!(payload["action"], action);
    }

    for action in ["status", "cancel"] {
        let mut args = refresh_args("profile", action);
        args.as_object_mut().unwrap().remove("handle");
        let result = handle_session_refresh(args, SessionRefreshServices::default())
            .await
            .unwrap();
        let payload: Value = serde_json::from_str(response_text(&result.value)).unwrap();
        assert_eq!(payload["outcome"], "error");
        assert_eq!(payload["error"]["code"], "invalid_request");
        assert_eq!(payload["action"], action);
    }

    let mut args = refresh_args("profile", "start");
    args.as_object_mut()
        .unwrap()
        .insert("format".to_string(), json!("yaml"));
    let result = handle_session_refresh(args, SessionRefreshServices::default())
        .await
        .unwrap();
    let payload: Value = serde_json::from_str(response_text(&result.value)).unwrap();
    assert_eq!(payload["outcome"], "error");
    assert_eq!(payload["error"]["code"], "invalid_request");
    assert!(
        payload["error"]["message"]
            .as_str()
            .unwrap()
            .contains("markdown, json")
    );
}

#[tokio::test]
async fn invalid_closed_request_returns_stable_argument_error() {
    let mut args = refresh_args("project", "begin");
    args.as_object_mut()
        .unwrap()
        .insert("unexpected".to_string(), Value::Bool(true));

    let result = handle_session_refresh(args, SessionRefreshServices::default())
        .await
        .unwrap();
    let payload: Value = serde_json::from_str(response_text(&result.value)).unwrap();

    assert_eq!(payload["outcome"], "error");
    assert_eq!(payload["error"]["code"], "invalid_request");
    assert!(
        payload["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unexpected")
    );
    assert_eq!(result.semantic_error(), Some(true));
}
