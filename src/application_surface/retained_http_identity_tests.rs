use std::sync::Mutex;

use axum::body::to_bytes;
use axum::http::StatusCode;
use serde_json::{Value, json};
use tracedecay_application::retained_surfaces::{
    FactStoreAddResultV1, FactStoreRemoveResultV1, RetainedSurfaceOperation,
    RetainedSurfaceResultV1,
};
use tracedecay_application::{
    ApplicationOutcome, ApplicationProblem, AuthorityReceipt, CancellationSignal,
    CapabilityGrantId, Deadline, DisclosureClass, EffectId, EffectReceipt, EffectResult,
    EffectTermination, IdempotencyKey, LegalAction, OperationBudgetUsage, OperationReceipt,
    PolicyDecisionRef, ReconciliationState, RequestId, ResolvedScope, RetryDirective,
    SafeDiagnostic,
};
use tracedecay_domain::{
    ActorId, ComponentVersion, ManifestDigest, ProjectId, RepositoryId, UtcMicros, WorktreeId,
};
use tracedecay_tool_catalog::EffectClass;

use super::super::{RegisteredHttpOperation, WorkOperation, invoke_registered_http};
use super::validated_daemon_outcome;

fn digest(seed: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", seed.to_string().repeat(64))).expect("fixture digest")
}

fn retained_scope(seed: &str) -> ResolvedScope {
    ResolvedScope::new(
        ProjectId::new(format!("project.retained.{seed}")).expect("project"),
        RepositoryId::new(format!("repository.retained.{seed}")).expect("repository"),
        WorktreeId::new(format!("worktree.retained.{seed}")).expect("worktree"),
        None,
    )
    .expect("scope")
}

fn effect_receipt(
    operation: RetainedSurfaceOperation,
    request_id: &RequestId,
    scope: ResolvedScope,
    outcome: EffectTermination,
) -> EffectReceipt {
    EffectReceipt {
        operation: tracedecay_application::retained_surface_application_operation(operation)
            .expect("retained application operation")
            .use_case_id()
            .clone(),
        request_id: request_id.clone(),
        actor: ActorId::new("actor.retained.fixture").expect("actor"),
        scope,
        effect_class: EffectClass::Administrative,
        idempotency_key: IdempotencyKey::new("idempotency.retained.fixture")
            .expect("idempotency key"),
        input_digest: digest('a'),
        expected_state: digest('b'),
        policy_digest: digest('c'),
        configuration_digest: digest('d'),
        catalog_digest: digest('e'),
        privacy_digest: digest('f'),
        outcome,
        committed_state: Some(digest('1')),
        external_proof: None,
    }
}

fn retained_partial_effect_problem(
    operation: RetainedSurfaceOperation,
    request_id: &RequestId,
    scope: ResolvedScope,
) -> ApplicationProblem {
    ApplicationProblem::PartialEffect {
        diagnostic: SafeDiagnostic::new(
            "retained.fixture.partial_effect",
            "This admitted terminal belongs only to the daemon response fixture",
        )
        .expect("safe diagnostic"),
        committed_receipt: Box::new(effect_receipt(
            operation,
            request_id,
            scope,
            EffectTermination::Partial,
        )),
        retry: RetryDirective::Never,
        legal_actions: vec![LegalAction::Reconcile],
    }
}

fn retained_effect_outcome(
    operation: RetainedSurfaceOperation,
    request_id: &RequestId,
    receipt_scope: ResolvedScope,
    payload: RetainedSurfaceResultV1,
) -> ApplicationOutcome<RetainedSurfaceResultV1> {
    let receipt = effect_receipt(
        operation,
        request_id,
        receipt_scope.clone(),
        EffectTermination::Completed,
    );
    let effect = EffectResult::new(
        EffectId::new("effect.retained.fixture").expect("effect id"),
        EffectClass::Administrative,
        receipt.idempotency_key.clone(),
        AuthorityReceipt {
            grant_id: CapabilityGrantId::new("grant.retained.fixture").expect("grant id"),
            grant_revision: 1,
            grant_digest: digest('2'),
            authorized_scope_digest: receipt_scope.scope_digest.clone(),
            disclosure: DisclosureClass::Evidence,
            policy: PolicyDecisionRef::new(
                "policy.retained.fixture",
                1,
                digest('3'),
                ComponentVersion::new("policy.retained.fixture.v1").expect("policy version"),
            )
            .expect("policy decision"),
            revalidated_at: UtcMicros(10),
        },
        receipt.expected_state.clone(),
        OperationReceipt::completed(
            UtcMicros(10),
            UtcMicros(11),
            Deadline::new(UtcMicros(1_000)).expect("deadline"),
            OperationBudgetUsage::default(),
        )
        .expect("execution receipt"),
        ReconciliationState::Reconciled,
        receipt,
        Some(payload),
    )
    .expect("effect result");
    ApplicationOutcome::Effect(effect)
}

struct StaticDaemonResponseExecutor {
    response: Mutex<Option<crate::daemon_contract::DaemonInvocationResponse>>,
}

impl StaticDaemonResponseExecutor {
    fn new(response: crate::daemon_contract::DaemonInvocationResponse) -> Self {
        Self {
            response: Mutex::new(Some(response)),
        }
    }
}

impl tracedecay_application::ApplicationInvocationExecutor for StaticDaemonResponseExecutor {
    fn invoke(
        &self,
        _invocation: tracedecay_application::ApplicationInvocation,
    ) -> tracedecay_application::ApplicationInvocationFuture<
        '_,
        Result<
            tracedecay_application::ApplicationResponse,
            tracedecay_application::InvocationError,
        >,
    > {
        Box::pin(async { Err(tracedecay_application::InvocationError::Unavailable) })
    }
}

impl crate::daemon_client::DaemonInvocationExecutor for StaticDaemonResponseExecutor {
    fn invoke_controlled(
        &self,
        _request: crate::daemon_contract::DaemonInvocationRequest,
        _deadline: Deadline,
        _cancellation: CancellationSignal,
        _policy: crate::daemon_client::InvocationCancellationPolicy,
    ) -> crate::daemon_client::DaemonInvocationExecutorFuture<
        '_,
        Result<
            crate::daemon_contract::DaemonInvocationResponse,
            crate::daemon_client::DaemonInvocationError,
        >,
    > {
        let response = self
            .response
            .lock()
            .expect("static daemon response")
            .take()
            .expect("one daemon invocation");
        Box::pin(async move { Ok(response) })
    }

    fn observe_feedback(
        &self,
        _subject_digest: ManifestDigest,
        _observed_at: UtcMicros,
        _event: tracedecay_usecases::feedback::observations::FeedbackSourceEventV1,
    ) -> crate::daemon_client::DaemonInvocationExecutorFuture<'_, crate::errors::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

async fn response_json(response: axum::response::Response) -> Value {
    serde_json::from_slice(
        &to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body"),
    )
    .expect("problem JSON")
}

async fn invoke_retained_http_with_response(
    operation: RetainedSurfaceOperation,
    request_id: RequestId,
    response: crate::daemon_contract::DaemonInvocationResponse,
) -> axum::response::Response {
    let deadline = Deadline::new(UtcMicros(1_000)).expect("deadline");
    let cancellation =
        CancellationSignal::active("cancellation.retained.http").expect("cancellation");
    let retained_request = super::super::retained::decode_request(
        operation,
        json!({"fact_id": "fact.retained.fixture"}),
    )
    .expect("retained request");
    let invocation = crate::daemon_contract::DaemonInvocationRequest::retained_application(
        request_id.as_str(),
        retained_request,
        UtcMicros(10),
        deadline.clone(),
        cancellation.context(),
    );
    let executor = StaticDaemonResponseExecutor::new(response);
    let selected_request_id = request_id.clone();
    invoke_registered_http::<tracedecay_application::retained_surfaces::RetainedSurfaceResultV1, _>(
        &executor,
        operation,
        request_id,
        tracedecay_api::HttpApplicationControls {
            deadline,
            cancellation,
        },
        invocation,
        |outcome| match outcome {
            crate::daemon_contract::DaemonInvocationOutcome::RetainedApplication {
                scope,
                outcome,
            } => tracedecay_application::retained_surface_outcome_matches_terminal(
                operation,
                &selected_request_id,
                &scope,
                &outcome,
            )
            .then_some((scope, outcome)),
            _ => None,
        },
    )
    .await
}

#[test]
fn rejects_each_untrusted_daemon_envelope_field_before_payload_selection() {
    let operation = RetainedSurfaceOperation::FactStoreRemove;
    let caller_request_id =
        RequestId::new("request.retained.http.caller").expect("caller request id");
    let daemon_request_id =
        RequestId::new("request.retained.http.daemon").expect("daemon request id");
    let response = crate::daemon_contract::DaemonInvocationResponse::application_problem(
        caller_request_id.as_str(),
        retained_partial_effect_problem(operation, &caller_request_id, retained_scope("caller")),
    );
    let mut invalid_responses = Vec::new();

    let mut invalid_protocol = response.clone();
    invalid_protocol.protocol = "tracedecay.daemon.invocation.retired".to_owned();
    invalid_responses.push(invalid_protocol);

    let mut invalid_revision = response.clone();
    invalid_revision.revision = crate::daemon_contract::DAEMON_INVOCATION_REVISION + 1;
    invalid_responses.push(invalid_revision);

    let mut invalid_request_id = response;
    invalid_request_id.request_id = daemon_request_id.as_str().to_owned();
    invalid_responses.push(invalid_request_id);

    for response in invalid_responses {
        let problem = validated_daemon_outcome(operation, &caller_request_id, Ok(response))
            .expect_err("invalid daemon identity must be rejected before reading its payload");
        let ApplicationProblem::Unavailable { diagnostic, .. } = problem else {
            panic!("invalid daemon identity must become a pre-admission unavailable problem");
        };
        assert_eq!(diagnostic.code, "retained.invalid_envelope");
        assert_ne!(diagnostic.code, "retained.fixture.partial_effect");
    }
}

#[test]
fn non_retained_registered_operations_reject_retained_scoped_problems() {
    let request_id = RequestId::new("request.retained.http.cross-family").expect("request id");
    let scope = retained_scope("cross-family");

    assert!(
        !WorkOperation::GenerateProposal.application_problem_is_bound(
            &request_id,
            Some(&scope),
            &ApplicationProblem::cancelled_before_admission(),
        )
    );
}

#[tokio::test]
async fn registered_http_rejects_invalid_identity_without_exposing_its_receipt() {
    let operation = RetainedSurfaceOperation::FactStoreRemove;
    let caller_request_id =
        RequestId::new("request.retained.http.outer").expect("caller request id");
    let daemon_request_id =
        RequestId::new("request.retained.http.untrusted").expect("daemon request id");
    let response = crate::daemon_contract::DaemonInvocationResponse::application_problem(
        daemon_request_id.as_str(),
        retained_partial_effect_problem(operation, &daemon_request_id, retained_scope("daemon")),
    );

    let response =
        invoke_retained_http_with_response(operation, caller_request_id.clone(), response).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = response_json(response).await;
    assert_eq!(body["kind"], "problem");
    assert_eq!(body["value"]["request_id"], caller_request_id.as_str());
    assert_eq!(body["value"]["problem"]["kind"], "unavailable");
    assert_eq!(
        body["value"]["problem"]["code"],
        "retained.invalid_envelope"
    );
    assert_eq!(body["value"]["problem"]["committed_receipt"], Value::Null);
}

#[tokio::test]
async fn registered_http_rejects_unbound_partial_effect_receipts_without_exposing_them() {
    let operation = RetainedSurfaceOperation::FactStoreRemove;
    let request_id = RequestId::new("request.retained.http.partial-binding").expect("request id");
    let scope = retained_scope("partial-binding");
    let exact = retained_partial_effect_problem(operation, &request_id, scope.clone());
    let mut wrong_request = exact.clone();
    let mut wrong_operation = exact.clone();
    let mut wrong_scope = exact.clone();
    let mut wrong_effect_class = exact.clone();
    let ApplicationProblem::PartialEffect {
        committed_receipt, ..
    } = &mut wrong_request
    else {
        unreachable!("fixture is a partial effect");
    };
    committed_receipt.request_id =
        RequestId::new("request.retained.http.other").expect("other request id");
    let ApplicationProblem::PartialEffect {
        committed_receipt, ..
    } = &mut wrong_operation
    else {
        unreachable!("fixture is a partial effect");
    };
    committed_receipt.operation = tracedecay_application::retained_surface_application_operation(
        RetainedSurfaceOperation::FactStoreAdd,
    )
    .expect("other application operation")
    .use_case_id()
    .clone();
    let ApplicationProblem::PartialEffect {
        committed_receipt, ..
    } = &mut wrong_scope
    else {
        unreachable!("fixture is a partial effect");
    };
    committed_receipt.scope = retained_scope("partial-other");
    let ApplicationProblem::PartialEffect {
        committed_receipt, ..
    } = &mut wrong_effect_class
    else {
        unreachable!("fixture is a partial effect");
    };
    committed_receipt.effect_class = EffectClass::SourceEdit;

    let responses = [
        crate::daemon_contract::DaemonInvocationResponse::retained_application_problem(
            request_id.as_str(),
            scope.clone(),
            wrong_request,
        ),
        crate::daemon_contract::DaemonInvocationResponse::retained_application_problem(
            request_id.as_str(),
            scope.clone(),
            wrong_operation,
        ),
        crate::daemon_contract::DaemonInvocationResponse::retained_application_problem(
            request_id.as_str(),
            scope.clone(),
            wrong_scope,
        ),
        crate::daemon_contract::DaemonInvocationResponse::retained_application_problem(
            request_id.as_str(),
            scope,
            wrong_effect_class,
        ),
        crate::daemon_contract::DaemonInvocationResponse::application_problem(
            request_id.as_str(),
            exact,
        ),
    ];

    for response in responses {
        let response =
            invoke_retained_http_with_response(operation, request_id.clone(), response).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = response_json(response).await;
        assert_eq!(
            body["value"]["problem"]["code"],
            "retained.invalid_terminal"
        );
        assert_eq!(body["value"]["problem"]["committed_receipt"], Value::Null);
    }
}

#[tokio::test]
async fn registered_http_rejects_successes_with_the_wrong_payload_receipt_or_scope() {
    let operation = RetainedSurfaceOperation::FactStoreRemove;
    let request_id = RequestId::new("request.retained.http.effect-binding").expect("request id");
    let scope = retained_scope("effect-binding");
    let remove_payload = || {
        RetainedSurfaceResultV1::FactStoreRemove(FactStoreRemoveResultV1::NotFound {
            remaining_fact_count: 0,
        })
    };
    let wrong_payload = retained_effect_outcome(
        operation,
        &request_id,
        scope.clone(),
        RetainedSurfaceResultV1::FactStoreAdd(FactStoreAddResultV1::SecretRejected),
    );
    let mut wrong_operation =
        retained_effect_outcome(operation, &request_id, scope.clone(), remove_payload());
    let mut wrong_request =
        retained_effect_outcome(operation, &request_id, scope.clone(), remove_payload());
    let mut wrong_effect_class =
        retained_effect_outcome(operation, &request_id, scope.clone(), remove_payload());
    let wrong_scope = retained_effect_outcome(
        operation,
        &request_id,
        retained_scope("effect-other"),
        remove_payload(),
    );
    let ApplicationOutcome::Effect(effect) = &mut wrong_operation else {
        unreachable!("fixture is an effect");
    };
    effect.receipt.operation = tracedecay_application::retained_surface_application_operation(
        RetainedSurfaceOperation::FactStoreAdd,
    )
    .expect("other application operation")
    .use_case_id()
    .clone();
    let ApplicationOutcome::Effect(effect) = &mut wrong_request else {
        unreachable!("fixture is an effect");
    };
    effect.receipt.request_id =
        RequestId::new("request.retained.http.effect-other").expect("other request id");
    let ApplicationOutcome::Effect(effect) = &mut wrong_effect_class else {
        unreachable!("fixture is an effect");
    };
    effect.effect_class = EffectClass::SourceEdit;
    effect.receipt.effect_class = EffectClass::SourceEdit;

    for outcome in [
        wrong_payload,
        wrong_operation,
        wrong_request,
        wrong_scope,
        wrong_effect_class,
    ] {
        let response = crate::daemon_contract::DaemonInvocationResponse::with_outcome(
            request_id.as_str().to_owned(),
            crate::daemon_contract::DaemonInvocationOutcome::RetainedApplication {
                scope: scope.clone(),
                outcome,
            },
        );
        let response =
            invoke_retained_http_with_response(operation, request_id.clone(), response).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = response_json(response).await;
        assert_eq!(
            body["value"]["problem"]["code"],
            "retained.protocol_unavailable"
        );
    }
}

#[tokio::test]
async fn registered_http_serializes_an_exactly_bound_effect() {
    let operation = RetainedSurfaceOperation::FactStoreRemove;
    let request_id = RequestId::new("request.retained.http.effect-valid").expect("request id");
    let scope = retained_scope("effect-valid");
    let outcome = retained_effect_outcome(
        operation,
        &request_id,
        scope.clone(),
        RetainedSurfaceResultV1::FactStoreRemove(FactStoreRemoveResultV1::NotFound {
            remaining_fact_count: 0,
        }),
    );
    let response = crate::daemon_contract::DaemonInvocationResponse::with_outcome(
        request_id.as_str().to_owned(),
        crate::daemon_contract::DaemonInvocationOutcome::RetainedApplication { scope, outcome },
    );

    let response = invoke_retained_http_with_response(operation, request_id, response).await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn registered_http_serializes_the_exact_valid_partial_effect_receipt() {
    let operation = RetainedSurfaceOperation::FactStoreRemove;
    let request_id = RequestId::new("request.retained.http.partial").expect("request id");
    let scope = retained_scope("partial");
    let problem = retained_partial_effect_problem(operation, &request_id, scope.clone());
    let ApplicationProblem::PartialEffect {
        committed_receipt, ..
    } = &problem
    else {
        unreachable!("fixture is a partial effect");
    };
    let expected_receipt = serde_json::to_value(committed_receipt).expect("receipt JSON");
    let response = crate::daemon_contract::DaemonInvocationResponse::retained_application_problem(
        request_id.as_str(),
        scope,
        problem,
    );

    let response =
        invoke_retained_http_with_response(operation, request_id.clone(), response).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = response_json(response).await;
    assert_eq!(body["kind"], "problem");
    assert_eq!(body["value"]["request_id"], request_id.as_str());
    assert_eq!(body["value"]["problem"]["kind"], "partial_effect");
    assert_eq!(
        body["value"]["problem"]["committed_receipt"],
        expected_receipt
    );
}
