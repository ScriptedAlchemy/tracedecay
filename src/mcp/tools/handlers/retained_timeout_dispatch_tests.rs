use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_application::retained_surfaces::{
    AutomationRunRequestV1, AutomationRunResultV1, AutomationRunSummaryV1, AutomationRunTerminalV1,
    AutomationTaskRequestV1, AutomationTaskV1, MemoryCuratorRunInputV1, RetainedSurfaceOperation,
    RetainedSurfaceRequestV1, RetainedSurfaceResultV1,
};
use tracedecay_application::{
    ApplicationOutcome, ApplicationProblem, AuthorityReceipt, CancellationSignal,
    CapabilityGrantId, Deadline, DisclosureClass, EffectId, EffectReceipt, EffectResult,
    EffectTermination, IdempotencyKey, LegalAction, OperationBudgetUsage, OperationReceipt,
    PolicyDecisionRef, ReconciliationState, RequestId, ResolvedScope, RetryDirective,
    SafeDiagnostic,
};
use tracedecay_domain::{
    ActorId, ComponentVersion, ManifestDigest, ProjectId, RepositoryId, RunId, UtcMicros,
    WorktreeId,
};
use tracedecay_tool_catalog::EffectClass;

use super::dispatch_test_support::SelectorEnv;
use super::*;
use crate::config::lock_user_data_dir_test_env;
use crate::tracedecay::TraceDecay;

fn digest(seed: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", seed.to_string().repeat(64))).expect("fixture digest")
}

fn retained_scope() -> ResolvedScope {
    ResolvedScope::new(
        ProjectId::new("project.retained.timeout.fixture").expect("project"),
        RepositoryId::new("repository.retained.timeout.fixture").expect("repository"),
        WorktreeId::new("worktree.retained.timeout.fixture").expect("worktree"),
        None,
    )
    .expect("scope")
}

fn post_commit_partial_effect(
    operation: RetainedSurfaceOperation,
    request_id: &RequestId,
) -> ApplicationProblem {
    ApplicationProblem::PartialEffect {
        diagnostic: SafeDiagnostic::new(
            "retained.fixture.post_commit_partial",
            "The daemon committed the retained effect before its settlement response.",
        )
        .expect("fixture diagnostic"),
        committed_receipt: EffectReceipt {
            operation: tracedecay_application::retained_surface_application_operation(operation)
                .expect("retained application operation")
                .use_case_id()
                .clone(),
            request_id: request_id.clone(),
            actor: ActorId::new("actor.retained.timeout.fixture").expect("actor"),
            scope: retained_scope(),
            effect_class: EffectClass::Administrative,
            idempotency_key: IdempotencyKey::new("idempotency.retained.timeout.fixture")
                .expect("idempotency key"),
            input_digest: digest('a'),
            expected_state: digest('b'),
            policy_digest: digest('c'),
            configuration_digest: digest('d'),
            catalog_digest: digest('e'),
            privacy_digest: digest('f'),
            outcome: EffectTermination::Partial,
            committed_state: Some(digest('1')),
            external_proof: None,
        },
        retry: RetryDirective::Never,
        legal_actions: vec![LegalAction::Reconcile],
    }
}

fn deadline_from_now(offset: Duration) -> Deadline {
    let offset = i64::try_from(offset.as_micros()).expect("fixture deadline fits domain clock");
    Deadline::new(UtcMicros(
        crate::daemon_client::invocation_now_micros()
            .0
            .saturating_add(offset),
    ))
    .expect("fixture deadline")
}

fn mcp_payload(result: ToolResult) -> Value {
    serde_json::from_str(
        result.value["content"][0]["text"]
            .as_str()
            .expect("retained MCP JSON result"),
    )
    .expect("retained MCP JSON envelope")
}

fn fact_store_curate_run(
    run_id: &str,
    fact_review_limit: u32,
    min_confidence_millionths: u32,
) -> AutomationRunResultV1 {
    let request = AutomationRunRequestV1 {
        run_id: RunId::new(run_id).expect("curation run id"),
        task: AutomationTaskRequestV1::MemoryCurator(MemoryCuratorRunInputV1 {
            fact_review_limit,
            min_confidence_millionths,
        }),
    };
    AutomationRunResultV1 {
        run_id: request.run_id.clone(),
        task: AutomationTaskV1::MemoryCurator,
        request_digest: request.input_digest().expect("curation request digest"),
        terminal: AutomationRunTerminalV1::Completed {
            summary: AutomationRunSummaryV1 {
                reviewed_count: 0,
                accepted_count: 0,
                rejected_count: 0,
                skipped_count: 0,
            },
        },
        committed_receipts: Vec::new(),
    }
}

fn fact_store_curate_effect(
    request_id: &RequestId,
    scope: ResolvedScope,
    fact_review_limit: u32,
    min_confidence_millionths: u32,
) -> ApplicationOutcome<RetainedSurfaceResultV1> {
    let idempotency_key =
        IdempotencyKey::new("idempotency.retained.fact-store-curate").expect("idempotency key");
    let receipt = EffectReceipt {
        operation: tracedecay_application::retained_surface_application_operation(
            RetainedSurfaceOperation::FactStoreCurate,
        )
        .expect("fact-store curation operation")
        .use_case_id()
        .clone(),
        request_id: request_id.clone(),
        actor: ActorId::new("actor.retained.fact-store-curate").expect("actor"),
        scope: scope.clone(),
        effect_class: EffectClass::Administrative,
        idempotency_key: idempotency_key.clone(),
        input_digest: digest('a'),
        expected_state: digest('b'),
        policy_digest: digest('c'),
        configuration_digest: digest('d'),
        catalog_digest: digest('e'),
        privacy_digest: digest('f'),
        outcome: EffectTermination::Completed,
        committed_state: Some(digest('1')),
        external_proof: None,
    };
    let authority = AuthorityReceipt {
        grant_id: CapabilityGrantId::new("grant.retained.fact-store-curate").expect("grant id"),
        grant_revision: 1,
        grant_digest: digest('2'),
        authorized_scope_digest: scope.scope_digest.clone(),
        disclosure: DisclosureClass::Evidence,
        policy: PolicyDecisionRef::new(
            "policy.retained.fact-store-curate",
            1,
            digest('3'),
            ComponentVersion::new("policy.retained.fact-store-curate.v1").expect("policy version"),
        )
        .expect("policy decision"),
        revalidated_at: UtcMicros(10),
    };
    let effect = EffectResult::new(
        EffectId::new("effect.retained.fact-store-curate").expect("effect id"),
        EffectClass::Administrative,
        idempotency_key,
        authority,
        receipt.expected_state.clone(),
        OperationReceipt::completed(
            UtcMicros(10),
            UtcMicros(11),
            Deadline::new(UtcMicros(1_000)).expect("execution deadline"),
            OperationBudgetUsage::default(),
        )
        .expect("execution receipt"),
        ReconciliationState::Reconciled,
        receipt,
        Some(RetainedSurfaceResultV1::FactStoreCurate(
            fact_store_curate_run(
                "run.mcp.fact-store-curate",
                fact_review_limit,
                min_confidence_millionths,
            ),
        )),
    )
    .expect("fact-store curation effect");
    ApplicationOutcome::Effect(effect)
}

struct FactStoreCurateSuccessExecutor {
    calls: AtomicUsize,
    scope: ResolvedScope,
    expected_bounds: (u32, u32),
}

impl tracedecay_application::ApplicationInvocationExecutor for FactStoreCurateSuccessExecutor {
    fn invoke(
        &self,
        _invocation: tracedecay_application::ApplicationInvocation,
    ) -> tracedecay_application::ApplicationInvocationFuture<
        '_,
        std::result::Result<
            tracedecay_application::ApplicationResponse,
            tracedecay_application::InvocationError,
        >,
    > {
        Box::pin(async { Err(tracedecay_application::InvocationError::Unavailable) })
    }
}

impl crate::daemon_client::DaemonInvocationExecutor for FactStoreCurateSuccessExecutor {
    fn invoke_controlled(
        &self,
        request: crate::daemon_contract::DaemonInvocationRequest,
        deadline: Deadline,
        cancellation: CancellationSignal,
        policy: crate::daemon_client::InvocationCancellationPolicy,
    ) -> crate::daemon_client::DaemonInvocationExecutorFuture<
        '_,
        std::result::Result<
            crate::daemon_contract::DaemonInvocationResponse,
            crate::daemon_client::DaemonInvocationError,
        >,
    > {
        assert_eq!(
            policy,
            crate::daemon_client::InvocationCancellationPolicy::AuthoritativeEffect,
        );
        assert!(!cancellation.is_cancelled());
        let crate::daemon_contract::DaemonInvocationPayload::RetainedApplication {
            request: RetainedSurfaceRequestV1::FactStoreCurate(bounds),
            deadline: embedded_deadline,
            cancellation: embedded_cancellation,
            ..
        } = request.payload
        else {
            panic!("fact-store curation must use the retained application route")
        };
        assert_eq!(
            (bounds.fact_review_limit, bounds.min_confidence_millionths),
            self.expected_bounds,
            "the MCP caller may control only the two public curation bounds",
        );
        assert_eq!(deadline, embedded_deadline);
        assert_eq!(cancellation.context(), embedded_cancellation);
        self.calls.fetch_add(1, Ordering::SeqCst);
        let request_id = RequestId::new(request.request_id).expect("request id");
        let outcome = fact_store_curate_effect(
            &request_id,
            self.scope.clone(),
            bounds.fact_review_limit,
            bounds.min_confidence_millionths,
        );
        let response = crate::daemon_contract::DaemonInvocationResponse::with_outcome(
            request_id.as_str().to_owned(),
            crate::daemon_contract::DaemonInvocationOutcome::RetainedApplication {
                scope: self.scope.clone(),
                outcome,
            },
        );
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

struct ExpiredDeadlineExecutor {
    calls: AtomicUsize,
    mutations: AtomicUsize,
}

impl tracedecay_application::ApplicationInvocationExecutor for ExpiredDeadlineExecutor {
    fn invoke(
        &self,
        _invocation: tracedecay_application::ApplicationInvocation,
    ) -> tracedecay_application::ApplicationInvocationFuture<
        '_,
        std::result::Result<
            tracedecay_application::ApplicationResponse,
            tracedecay_application::InvocationError,
        >,
    > {
        Box::pin(async { Err(tracedecay_application::InvocationError::Unavailable) })
    }
}

impl crate::daemon_client::DaemonInvocationExecutor for ExpiredDeadlineExecutor {
    fn invoke_controlled(
        &self,
        request: crate::daemon_contract::DaemonInvocationRequest,
        deadline: Deadline,
        _cancellation: CancellationSignal,
        policy: crate::daemon_client::InvocationCancellationPolicy,
    ) -> crate::daemon_client::DaemonInvocationExecutorFuture<
        '_,
        std::result::Result<
            crate::daemon_contract::DaemonInvocationResponse,
            crate::daemon_client::DaemonInvocationError,
        >,
    > {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            policy,
            crate::daemon_client::InvocationCancellationPolicy::AuthoritativeEffect,
        );
        assert!(deadline.is_elapsed_at(crate::daemon_client::invocation_now_micros()));
        let response = if deadline.is_elapsed_at(crate::daemon_client::invocation_now_micros()) {
            crate::daemon_contract::DaemonInvocationResponse::application_problem(
                &request.request_id,
                ApplicationProblem::timed_out_before_admission(),
            )
        } else {
            self.mutations.fetch_add(1, Ordering::SeqCst);
            crate::daemon_contract::DaemonInvocationResponse::application_problem(
                &request.request_id,
                ApplicationProblem::unavailable(
                    SafeDiagnostic::new(
                        "retained.fixture.unexpected-deadline-mutation",
                        "The expired fixture would have attempted a mutation.",
                    )
                    .expect("fixture diagnostic"),
                ),
            )
        };
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

#[tokio::test]
async fn fact_store_curate_forwards_only_bounds_and_preserves_canonical_success() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("fixture directory");
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("fact-store-curate-success");
    std::fs::create_dir_all(project.join("src")).expect("fixture source directory");
    std::fs::write(project.join("src/lib.rs"), "pub fn curate_success() {}\n")
        .expect("fixture source");
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-fact-store-curate-success",
    )
    .await
    .expect("registered retained fixture");
    let executor = FactStoreCurateSuccessExecutor {
        calls: AtomicUsize::new(0),
        scope: retained_scope(),
        expected_bounds: (17, 810_000),
    };
    let request_id = RequestId::new("request.retained.mcp.fact-store-curate").expect("request id");

    let result = handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_fact_store_curate",
        json!({
            "fact_review_limit": 17,
            "min_confidence_millionths": 810_000,
            "format": "json",
        }),
        None,
        None,
        ToolCallRegistryOptions {
            application_invocation_executor: Some(&executor),
            application_request_id: Some(request_id.clone()),
            application_deadline: Some(deadline_from_now(Duration::from_secs(5))),
            application_cancellation: Some(
                CancellationSignal::active("cancel.retained.mcp.fact-store-curate")
                    .expect("cancellation"),
            ),
            ..Default::default()
        },
    )
    .await
    .expect("canonical fact-store curation result");
    let payload = mcp_payload(result);

    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    assert_eq!(payload["request_id"], request_id.as_str());
    assert_eq!(payload["outcome"]["outcome"], "effect");
    assert_eq!(
        payload["outcome"]["value"]["payload"]["run_id"],
        "run.mcp.fact-store-curate"
    );
    assert_eq!(
        payload["outcome"]["value"]["payload"]["task"],
        "memory_curator"
    );
    assert_eq!(
        payload["outcome"]["value"]["payload"]["terminal"]["summary"]["reviewed_count"],
        0
    );
    cg.close();
}

#[tokio::test]
async fn fact_store_curate_expired_deadline_does_not_mutate() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("fixture directory");
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("fact-store-curate-deadline");
    std::fs::create_dir_all(project.join("src")).expect("fixture source directory");
    std::fs::write(project.join("src/lib.rs"), "pub fn curate_deadline() {}\n")
        .expect("fixture source");
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-fact-store-curate-deadline",
    )
    .await
    .expect("registered retained fixture");
    let executor = ExpiredDeadlineExecutor {
        calls: AtomicUsize::new(0),
        mutations: AtomicUsize::new(0),
    };

    let error = handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_fact_store_curate",
        json!({"fact_review_limit": 5, "min_confidence_millionths": 700_000}),
        None,
        None,
        ToolCallRegistryOptions {
            application_invocation_executor: Some(&executor),
            application_request_id: Some(
                RequestId::new("request.retained.mcp.fact-store-curate-deadline")
                    .expect("request id"),
            ),
            application_deadline: Some(Deadline::new(UtcMicros(1)).expect("expired deadline")),
            application_cancellation: Some(
                CancellationSignal::active("cancel.retained.mcp.fact-store-curate-deadline")
                    .expect("cancellation"),
            ),
            ..Default::default()
        },
    )
    .await
    .expect_err("an elapsed MCP deadline must reject curation before dispatch");

    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
    assert_eq!(executor.mutations.load(Ordering::SeqCst), 0);
    assert_eq!(
        error
            .project_route_context()
            .map(|(code, retryable, _)| (code, retryable)),
        Some(("tool_dispatch_deadline_exceeded", true)),
    );
    cg.close();
}

struct PostCommitPartialEffectExecutor {
    response: Mutex<Option<crate::daemon_contract::DaemonInvocationResponse>>,
}

impl PostCommitPartialEffectExecutor {
    fn with_scope(
        operation: RetainedSurfaceOperation,
        request_id: &RequestId,
        authority_scope: ResolvedScope,
    ) -> Self {
        Self {
            response: Mutex::new(Some(
                crate::daemon_contract::DaemonInvocationResponse::retained_application_problem(
                    request_id.as_str(),
                    authority_scope,
                    post_commit_partial_effect(operation, request_id),
                ),
            )),
        }
    }
}

impl tracedecay_application::ApplicationInvocationExecutor for PostCommitPartialEffectExecutor {
    fn invoke(
        &self,
        _invocation: tracedecay_application::ApplicationInvocation,
    ) -> tracedecay_application::ApplicationInvocationFuture<
        '_,
        std::result::Result<
            tracedecay_application::ApplicationResponse,
            tracedecay_application::InvocationError,
        >,
    > {
        Box::pin(async { Err(tracedecay_application::InvocationError::Unavailable) })
    }
}

impl crate::daemon_client::DaemonInvocationExecutor for PostCommitPartialEffectExecutor {
    fn invoke_controlled(
        &self,
        _request: crate::daemon_contract::DaemonInvocationRequest,
        _deadline: Deadline,
        _cancellation: CancellationSignal,
        policy: crate::daemon_client::InvocationCancellationPolicy,
    ) -> crate::daemon_client::DaemonInvocationExecutorFuture<
        '_,
        std::result::Result<
            crate::daemon_contract::DaemonInvocationResponse,
            crate::daemon_client::DaemonInvocationError,
        >,
    > {
        assert_eq!(
            policy,
            crate::daemon_client::InvocationCancellationPolicy::AuthoritativeEffect,
            "retained mutations must retain the authoritative-effect policy",
        );
        let response = self
            .response
            .lock()
            .expect("post-commit response")
            .take()
            .expect("one post-commit response");
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

struct PreCommitInterruptionExecutor {
    calls: AtomicUsize,
    mutations: AtomicUsize,
}

impl tracedecay_application::ApplicationInvocationExecutor for PreCommitInterruptionExecutor {
    fn invoke(
        &self,
        _invocation: tracedecay_application::ApplicationInvocation,
    ) -> tracedecay_application::ApplicationInvocationFuture<
        '_,
        std::result::Result<
            tracedecay_application::ApplicationResponse,
            tracedecay_application::InvocationError,
        >,
    > {
        Box::pin(async { Err(tracedecay_application::InvocationError::Unavailable) })
    }
}

impl crate::daemon_client::DaemonInvocationExecutor for PreCommitInterruptionExecutor {
    fn invoke_controlled(
        &self,
        request: crate::daemon_contract::DaemonInvocationRequest,
        _deadline: Deadline,
        cancellation: CancellationSignal,
        policy: crate::daemon_client::InvocationCancellationPolicy,
    ) -> crate::daemon_client::DaemonInvocationExecutorFuture<
        '_,
        std::result::Result<
            crate::daemon_contract::DaemonInvocationResponse,
            crate::daemon_client::DaemonInvocationError,
        >,
    > {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            policy,
            crate::daemon_client::InvocationCancellationPolicy::AuthoritativeEffect,
            "retained mutations must retain the authoritative-effect policy",
        );
        let response = if cancellation.is_cancelled() {
            crate::daemon_contract::DaemonInvocationResponse::application_problem(
                &request.request_id,
                ApplicationProblem::cancelled_before_admission(),
            )
        } else {
            self.mutations.fetch_add(1, Ordering::SeqCst);
            crate::daemon_contract::DaemonInvocationResponse::application_problem(
                &request.request_id,
                ApplicationProblem::unavailable(
                    SafeDiagnostic::new(
                        "retained.fixture.unexpected_mutation",
                        "The cancelled fixture would have attempted a mutation.",
                    )
                    .expect("fixture diagnostic"),
                ),
            )
        };
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

#[tokio::test]
async fn fact_store_curate_rejects_a_partial_receipt_from_another_scope() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("fixture directory");
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("fact-store-curate-scope-mismatch");
    std::fs::create_dir_all(project.join("src")).expect("fixture source directory");
    std::fs::write(project.join("src/lib.rs"), "pub fn curate_scope() {}\n")
        .expect("fixture source");
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-fact-store-curate-scope-mismatch",
    )
    .await
    .expect("registered retained fixture");
    let request_id = RequestId::new("request.retained.mcp.fact-store-curate-scope-mismatch")
        .expect("request id");
    let authority_scope = ResolvedScope::new(
        ProjectId::new("project.retained.fact-store-curate.other").expect("project"),
        RepositoryId::new("repository.retained.fact-store-curate.other").expect("repository"),
        WorktreeId::new("worktree.retained.fact-store-curate.other").expect("worktree"),
        None,
    )
    .expect("authority scope");
    let executor = PostCommitPartialEffectExecutor::with_scope(
        RetainedSurfaceOperation::FactStoreCurate,
        &request_id,
        authority_scope,
    );

    let result = handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_fact_store_curate",
        json!({"fact_review_limit": 8, "min_confidence_millionths": 730_000}),
        None,
        None,
        ToolCallRegistryOptions {
            application_invocation_executor: Some(&executor),
            application_request_id: Some(request_id),
            application_deadline: Some(deadline_from_now(Duration::from_secs(5))),
            application_cancellation: Some(
                CancellationSignal::active("cancel.retained.mcp.fact-store-curate-scope")
                    .expect("cancellation"),
            ),
            ..Default::default()
        },
    )
    .await
    .expect("scope mismatch must render an unavailable result");

    assert_eq!(result.value["problem"]["kind"], "unavailable");
    cg.close();
}

#[tokio::test]
async fn fact_store_curate_pre_commit_cancellation_does_not_mutate() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("fixture directory");
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("fact-store-curate-cancelled");
    std::fs::create_dir_all(project.join("src")).expect("fixture source directory");
    std::fs::write(project.join("src/lib.rs"), "pub fn retained_cancel() {}\n")
        .expect("fixture source");
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-fact-store-curate-cancelled",
    )
    .await
    .expect("registered retained fixture");
    let executor = PreCommitInterruptionExecutor {
        calls: AtomicUsize::new(0),
        mutations: AtomicUsize::new(0),
    };
    let cancellation =
        CancellationSignal::active("cancel.retained.mcp.fact-store-curate").expect("cancellation");
    assert!(cancellation.cancel(UtcMicros(1)));

    let result = handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_fact_store_curate",
        json!({"fact_review_limit": 5, "min_confidence_millionths": 700_000}),
        None,
        None,
        ToolCallRegistryOptions {
            application_invocation_executor: Some(&executor),
            application_request_id: Some(
                RequestId::new("request.retained.mcp.fact-store-curate-cancelled")
                    .expect("request identity"),
            ),
            application_deadline: Some(deadline_from_now(Duration::from_secs(5))),
            application_cancellation: Some(cancellation),
            ..Default::default()
        },
    )
    .await
    .expect("pre-commit cancellation must render a canonical retained problem");

    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    assert_eq!(executor.mutations.load(Ordering::SeqCst), 0);
    assert_eq!(result.value["problem"]["kind"], "cancelled");
    cg.close();
}
