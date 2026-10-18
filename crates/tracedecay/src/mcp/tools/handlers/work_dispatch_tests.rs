//! Journey: MCP Work adapter preserves the typed HTTP envelope.

use std::sync::Mutex;

use axum::body::to_bytes;
use axum::response::IntoResponse;
use serde_json::{Value, json};
use tracedecay_api::{HttpApplicationControls, WorkHttpRequest};
use tracedecay_contracts::{
    ApplicationInvocation, ApplicationInvocationExecutor, ApplicationInvocationFuture,
    ApplicationOutcome, ApplicationProblem, ApplicationResponse, CancellationSignal, Deadline,
    EffectReceipt, EffectTermination, IdempotencyKey, InvocationError, LegalAction, RequestId,
    ResolvedScope, RetryDirective, SafeDiagnostic, WorkAttemptRecoveryReportV1,
};
use tracedecay_daemon_protocol::{DaemonInvocationOutcome, WorkApplicationOutcomeV1};
use tracedecay_domain::test_fixtures::digest;
use tracedecay_domain::{ActorId, ProjectId, RepositoryId, UtcMicros, WorktreeId};
use tracedecay_mcp::handle_work;
use tracedecay_mcp::handlers::work::work_operation_for_tool;
use tracedecay_tool_catalog::{EffectClass, UseCaseId};

use super::dispatch_test_support::tool_json;
use super::invoke_admitted_work_operation;
use crate::daemon::retained_test_support::ignore_feedback_observations;

/// Refuses the request after recording its closed route. Both transports
/// must preserve the identical typed Work unavailable envelope.
#[derive(Default)]
pub(super) struct RecordingApplicationExecutor {
    operations: Mutex<Vec<tracedecay_daemon_protocol::DaemonInvocationOperation>>,
    settlement: Option<DaemonInvocationOutcome>,
}

impl RecordingApplicationExecutor {
    pub(super) fn with_settlement(settlement: DaemonInvocationOutcome) -> Self {
        Self {
            settlement: Some(settlement),
            ..Self::default()
        }
    }

    pub(super) fn invocation_count(&self) -> usize {
        self.operations.lock().expect("recorded operations").len()
    }
}

impl ApplicationInvocationExecutor for RecordingApplicationExecutor {
    fn invoke(
        &self,
        _invocation: ApplicationInvocation,
    ) -> ApplicationInvocationFuture<'_, std::result::Result<ApplicationResponse, InvocationError>>
    {
        Box::pin(async { Err(InvocationError::Unavailable) })
    }
}

impl tracedecay_daemon_protocol::DaemonInvocationExecutor for RecordingApplicationExecutor {
    fn invoke_controlled(
        &self,
        request: tracedecay_daemon_protocol::DaemonInvocationRequest,
        _deadline: Deadline,
        _cancellation: CancellationSignal,
        _policy: tracedecay_daemon_protocol::InvocationCancellationPolicy,
    ) -> tracedecay_daemon_protocol::DaemonInvocationExecutorFuture<
        '_,
        std::result::Result<
            tracedecay_daemon_protocol::DaemonInvocationResponse,
            tracedecay_daemon_protocol::DaemonInvocationError,
        >,
    > {
        self.operations
            .lock()
            .expect("recorded Work daemon operations")
            .push(request.operation());
        let settlement = self.settlement.clone();
        Box::pin(async move {
            match settlement {
                Some(outcome) => Ok(tracedecay_daemon_protocol::DaemonInvocationResponse {
                    protocol: tracedecay_daemon_protocol::DAEMON_INVOCATION_PROTOCOL.to_owned(),
                    revision: tracedecay_daemon_protocol::DAEMON_INVOCATION_REVISION,
                    request_id: request.request_id,
                    outcome,
                }),
                None => Err(tracedecay_daemon_protocol::DaemonInvocationError::Unavailable),
            }
        })
    }

    ignore_feedback_observations!();
}

#[tokio::test]
async fn success_denial_and_partial_effect_reach_http_and_mcp_unchanged() {
    let request_id = RequestId::new("request.work-settlement").expect("request id");
    let scope = ResolvedScope::new(
        ProjectId::new("project.work-settlement").expect("project"),
        RepositoryId::new("repository.work-settlement").expect("repository"),
        WorktreeId::new("worktree.work-settlement").expect("worktree"),
        None,
    )
    .expect("scope");
    let receipt = EffectReceipt {
        operation: UseCaseId::new("use-case.work.resume_attempts").expect("operation"),
        request_id: request_id.clone(),
        actor: ActorId::new("actor.work-settlement").expect("actor"),
        scope: scope.clone(),
        effect_class: EffectClass::Administrative,
        idempotency_key: IdempotencyKey::new("idempotency.work-settlement").expect("key"),
        input_digest: digest('a'),
        expected_state: digest('b'),
        policy_digest: digest('c'),
        configuration_digest: digest('d'),
        catalog_digest: digest('e'),
        privacy_digest: digest('f'),
        outcome: EffectTermination::Partial,
        committed_state: Some(digest('1')),
        external_proof: None,
    };
    let settlements = [
        DaemonInvocationOutcome::WorkApplication {
            scope: scope.clone(),
            outcome: WorkApplicationOutcomeV1::ResumeAttempts(ApplicationOutcome::Result(
                WorkAttemptRecoveryReportV1 {
                    recovery_required: Vec::new(),
                    cancelled: Vec::new(),
                },
            )),
        },
        DaemonInvocationOutcome::ApplicationProblem {
            problem: ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
        },
        DaemonInvocationOutcome::ApplicationProblem {
            problem: ApplicationProblem::PartialEffect {
                diagnostic: SafeDiagnostic::new(
                    "work.partial_effect",
                    "The admitted operation committed part of its changes",
                )
                .expect("diagnostic"),
                committed_receipt: Box::new(receipt.clone()),
                retry: RetryDirective::Never,
                legal_actions: vec![LegalAction::Reconcile],
            },
        },
    ];
    for (index, settlement) in settlements.into_iter().enumerate() {
        let executor = RecordingApplicationExecutor {
            settlement: Some(settlement),
            ..Default::default()
        };
        let controls = HttpApplicationControls {
            deadline: Deadline::new(UtcMicros(tracedecay_contracts::now_micros().0 + 30_000_000))
                .expect("deadline"),
            cancellation: CancellationSignal::active("cancellation.work-settlement")
                .expect("cancellation"),
        };
        let body = json!({"occurred_at": 1});
        let http_response = tracedecay_daemon_service::application_surface::invoke_work_operation(
            &executor,
            WorkHttpRequest {
                operation: tracedecay_api::WorkOperation::ResumeAttempts,
                request_id: request_id.clone(),
                controls: controls.clone(),
                body: body.clone(),
            },
        )
        .await
        .expect("typed Work settlement")
        .into_response();
        let http_status = http_response.status();
        let http = response_json(http_response).await;
        let mcp = handle_work(
            "tracedecay_work_resume_attempts",
            body,
            Some(|request| invoke_admitted_work_operation(&executor, request)),
            Some(request_id.clone()),
            Some(controls.deadline),
            Some(controls.cancellation),
        )
        .await
        .expect("MCP Work settlement");
        assert_eq!(tool_json(&mcp), http);
        assert_eq!(http["value"]["request_id"], request_id.as_str());
        assert_eq!(mcp.semantic_error(), Some(index != 0));
        if index == 0 {
            assert_eq!(http_status, axum::http::StatusCode::OK);
            assert_eq!(
                http["value"]["scope"],
                serde_json::to_value(&scope).unwrap()
            );
            assert_eq!(
                http["value"]["outcome"],
                json!({"outcome": "result", "value": {"recovery_required": [], "cancelled": []}})
            );
        } else {
            assert_eq!(
                mcp.value["structuredContent"]["problem"],
                http["value"]["problem"]
            );
            if index == 1 {
                assert!(http["value"].get("binding_id").is_none());
                assert_eq!(http_status, axum::http::StatusCode::NOT_FOUND);
            } else {
                assert_eq!(
                    http["value"]["problem"]["committed_receipt"],
                    serde_json::to_value(&receipt).unwrap()
                );
                assert_eq!(http["value"]["problem"]["kind"], "partial_effect");
            }
        }
    }
}

async fn response_json(response: axum::response::Response) -> Value {
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("bounded Work HTTP response");
    serde_json::from_slice(&body).expect("JSON Work HTTP response")
}

#[tokio::test]
async fn read_mutation_and_evidence_preserve_the_typed_http_work_envelope() {
    let executor = RecordingApplicationExecutor::default();
    let requests = [
        (
            "tracedecay_work_views",
            json!({
                "selection": {"selection": "profile_owned_no_git"},
                "mode": {"mode": "current"},
                "continuation": null,
                "observed_at": 1
            }),
        ),
        ("tracedecay_work_resume_attempts", json!({"occurred_at": 1})),
        (
            "tracedecay_work_retrieve_evidence",
            json!({
                "selection": {"selection": "profile_owned_no_git"},
                "task_id": "task.work-mcp-parity",
                "verified_version": {
                    "graph_version": 1,
                    "event_sequence": 1,
                    "source_watermark": {},
                    "recovered_graph_digest": concat!(
                        "sha256:",
                        "11111111111111111111111111111111",
                        "11111111111111111111111111111111"
                    )
                },
                "temporal": {"kind": "forensic"},
                "page_size": 10,
                "expansion": null,
                "continuation": null,
                "observed_at": 1
            }),
        ),
    ];

    for (index, (tool_name, body)) in requests.into_iter().enumerate() {
        let operation = work_operation_for_tool(tool_name).expect("canonical Work name");
        let request_id =
            RequestId::new(format!("request.work-mcp-parity-{index}")).expect("valid request id");
        let deadline = Deadline::new(UtcMicros(tracedecay_contracts::now_micros().0 + 30_000_000))
            .expect("valid deadline");
        let cancellation =
            CancellationSignal::active(format!("cancellation.work-mcp-parity-{index}"))
                .expect("valid cancellation signal");
        let http = response_json(
            tracedecay_daemon_service::application_surface::invoke_work_operation(
                &executor,
                WorkHttpRequest {
                    operation,
                    request_id: request_id.clone(),
                    controls: HttpApplicationControls {
                        deadline: deadline.clone(),
                        cancellation: cancellation.clone(),
                    },
                    body: body.clone(),
                },
            )
            .await
            .expect("typed Work owner response")
            .into_response(),
        )
        .await;
        let mcp = handle_work(
            tool_name,
            body,
            Some(|request| invoke_admitted_work_operation(&executor, request)),
            Some(request_id),
            Some(deadline),
            Some(cancellation),
        )
        .await
        .expect("MCP Work adapter response");

        assert_eq!(mcp.semantic_error(), Some(true));
        assert_eq!(
            tool_json(&mcp),
            http,
            "{tool_name} MCP envelope drifted from HTTP"
        );
    }

    assert_eq!(
        *executor
            .operations
            .lock()
            .expect("recorded Work daemon operations"),
        vec![
            tracedecay_daemon_protocol::DaemonInvocationOperation::WorkApplication,
            tracedecay_daemon_protocol::DaemonInvocationOperation::WorkApplication,
            tracedecay_daemon_protocol::DaemonInvocationOperation::WorkApplication,
            tracedecay_daemon_protocol::DaemonInvocationOperation::WorkApplication,
            tracedecay_daemon_protocol::DaemonInvocationOperation::WorkApplication,
            tracedecay_daemon_protocol::DaemonInvocationOperation::WorkApplication,
        ]
    );
}
