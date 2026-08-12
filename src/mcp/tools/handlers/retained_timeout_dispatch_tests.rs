use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::oneshot;
use tracedecay_application::{
    ApplicationProblem, CancellationSignal, Deadline, EffectReceipt, EffectTermination,
    IdempotencyKey, LegalAction, RequestId, ResolvedScope, RetryDirective, SafeDiagnostic,
};
use tracedecay_domain::{ActorId, ManifestDigest, ProjectId, RepositoryId, UtcMicros, WorktreeId};
use tracedecay_tool_catalog::{EffectClass, UseCaseId};

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

fn post_commit_partial_effect(request_id: &RequestId) -> ApplicationProblem {
    ApplicationProblem::PartialEffect {
        diagnostic: SafeDiagnostic::new(
            "retained.fixture.post_commit_partial",
            "The daemon committed the retained effect before its settlement response.",
        )
        .expect("fixture diagnostic"),
        committed_receipt: EffectReceipt {
            operation: UseCaseId::new("use-case.application.retained.fact-store-add")
                .expect("use case"),
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

struct PostCommitPartialEffectExecutor {
    started: Mutex<Option<oneshot::Sender<()>>>,
    settle: Mutex<Option<oneshot::Receiver<()>>>,
    response: Mutex<Option<crate::daemon_contract::DaemonInvocationResponse>>,
}

impl PostCommitPartialEffectExecutor {
    fn new(
        started: oneshot::Sender<()>,
        settle: oneshot::Receiver<()>,
        request_id: &RequestId,
    ) -> Self {
        Self {
            started: Mutex::new(Some(started)),
            settle: Mutex::new(Some(settle)),
            response: Mutex::new(Some(
                crate::daemon_contract::DaemonInvocationResponse::retained_application_problem(
                    request_id.as_str(),
                    retained_scope(),
                    post_commit_partial_effect(request_id),
                ),
            )),
        }
    }

    fn with_scope(
        started: oneshot::Sender<()>,
        settle: oneshot::Receiver<()>,
        request_id: &RequestId,
        authority_scope: ResolvedScope,
    ) -> Self {
        Self {
            started: Mutex::new(Some(started)),
            settle: Mutex::new(Some(settle)),
            response: Mutex::new(Some(
                crate::daemon_contract::DaemonInvocationResponse::retained_application_problem(
                    request_id.as_str(),
                    authority_scope,
                    post_commit_partial_effect(request_id),
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
        let started = self
            .started
            .lock()
            .expect("post-commit start gate")
            .take()
            .expect("one post-commit invocation");
        let settle = self
            .settle
            .lock()
            .expect("post-commit settlement gate")
            .take()
            .expect("one post-commit settlement");
        let response = self
            .response
            .lock()
            .expect("post-commit response")
            .take()
            .expect("one post-commit response");
        started.send(()).expect("post-commit invocation observed");
        Box::pin(async move {
            settle
                .await
                .map_err(|_| crate::daemon_client::DaemonInvocationError::Unavailable)?;
            Ok(response)
        })
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

#[tokio::test(flavor = "current_thread")]
async fn retained_post_commit_settlement_preserves_its_canonical_partial_effect() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("fixture directory");
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("retained-post-commit-settlement");
    std::fs::create_dir_all(project.join("src")).expect("fixture source directory");
    std::fs::write(project.join("src/lib.rs"), "pub fn retained_timeout() {}\n")
        .expect("fixture source");
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-retained-post-commit-timeout",
    )
    .await
    .expect("registered retained fixture");
    let request_id =
        RequestId::new("request.retained.mcp.post-commit").expect("retained request identity");
    tokio::time::pause();
    let (started_tx, started_rx) = oneshot::channel();
    let (settle_tx, settle_rx) = oneshot::channel();
    let executor = PostCommitPartialEffectExecutor::new(started_tx, settle_rx, &request_id);
    let payload = {
        let call = handle_tool_call_with_registry_options(
            &cg,
            "tracedecay_fact_store_add",
            json!({"content": "the effect committed before its settlement response", "format": "json"}),
            None,
            None,
            ToolCallRegistryOptions {
                application_invocation_executor: Some(&executor),
                application_request_id: Some(request_id.clone()),
                application_deadline: Some(deadline_from_now(Duration::from_secs(1))),
                application_cancellation: Some(
                    CancellationSignal::active("cancel.retained.mcp.post-commit")
                        .expect("retained cancellation"),
                ),
                ..Default::default()
            },
        );
        tokio::pin!(call);
        tokio::pin!(started_rx);
        tokio::select! {
            started = &mut started_rx => started.expect("retained invocation reached settlement"),
            result = &mut call => panic!("retained dispatch ended before settlement: {result:?}"),
        }
        tokio::time::advance(Duration::from_millis(1_100)).await;
        tokio::select! {
            result = &mut call => panic!("generic dispatch timeout replaced the canonical terminal: {result:?}"),
            _ = tokio::task::yield_now() => {}
        }
        settle_tx.send(()).expect("release committed settlement");

        let result = call
            .await
            .expect("post-commit retained terminal must not become a generic dispatch timeout");
        mcp_payload(result)
    };

    assert_eq!(payload["problem"]["kind"], "partial_effect");
    assert_eq!(
        payload["problem"]["committed_receipt"]["request_id"],
        request_id.as_str(),
        "the canonical committed receipt must survive post-commit settlement",
    );
    cg.close();
}

#[tokio::test(flavor = "current_thread")]
async fn retained_mcp_rejects_a_partial_effect_with_the_wrong_authority_scope() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("fixture directory");
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("retained-partial-scope-mismatch");
    std::fs::create_dir_all(project.join("src")).expect("fixture source directory");
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn retained_scope_mismatch() {}\n",
    )
    .expect("fixture source");
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-retained-partial-scope-mismatch",
    )
    .await
    .expect("registered retained fixture");
    let request_id =
        RequestId::new("request.retained.mcp.scope-mismatch").expect("request identity");
    let authority_scope = ResolvedScope::new(
        ProjectId::new("project.retained.timeout.other").expect("project"),
        RepositoryId::new("repository.retained.timeout.other").expect("repository"),
        WorktreeId::new("worktree.retained.timeout.other").expect("worktree"),
        None,
    )
    .expect("authority scope");
    let (started_tx, _started_rx) = oneshot::channel();
    let (settle_tx, settle_rx) = oneshot::channel();
    let executor = PostCommitPartialEffectExecutor::with_scope(
        started_tx,
        settle_rx,
        &request_id,
        authority_scope,
    );
    settle_tx.send(()).expect("release settlement");

    let result = handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_fact_store_add",
        json!({"content": "the receipt scope must match the authority scope", "format": "json"}),
        None,
        None,
        ToolCallRegistryOptions {
            application_invocation_executor: Some(&executor),
            application_request_id: Some(request_id),
            application_deadline: Some(deadline_from_now(Duration::from_secs(5))),
            application_cancellation: Some(
                CancellationSignal::active("cancel.retained.mcp.scope-mismatch")
                    .expect("cancellation"),
            ),
            ..Default::default()
        },
    )
    .await
    .expect("scope mismatch must render a canonical unavailable problem");
    let payload = mcp_payload(result);

    assert_eq!(payload["problem"]["kind"], "unavailable");
    assert_eq!(payload["problem"]["committed_receipt"], Value::Null);
    cg.close();
}

#[tokio::test]
async fn retained_pre_commit_interruption_reaches_the_canonical_cancellation_without_mutation() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("fixture directory");
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("retained-pre-commit-interruption");
    std::fs::create_dir_all(project.join("src")).expect("fixture source directory");
    std::fs::write(project.join("src/lib.rs"), "pub fn retained_cancel() {}\n")
        .expect("fixture source");
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-retained-pre-commit-interruption",
    )
    .await
    .expect("registered retained fixture");
    let executor = PreCommitInterruptionExecutor {
        mutations: AtomicUsize::new(0),
    };
    let cancellation =
        CancellationSignal::active("cancel.retained.mcp.pre-commit").expect("cancellation");
    assert!(cancellation.cancel(UtcMicros(1)));

    let result = handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_fact_store_add",
        json!({"content": "this cancelled request must not mutate", "format": "json"}),
        None,
        None,
        ToolCallRegistryOptions {
            application_invocation_executor: Some(&executor),
            application_request_id: Some(
                RequestId::new("request.retained.mcp.pre-commit").expect("request identity"),
            ),
            application_deadline: Some(deadline_from_now(Duration::from_secs(5))),
            application_cancellation: Some(cancellation),
            ..Default::default()
        },
    )
    .await
    .expect("pre-commit cancellation must render a canonical retained problem");
    let payload = mcp_payload(result);

    assert_eq!(executor.mutations.load(Ordering::SeqCst), 0);
    assert_eq!(payload["problem"]["kind"], "cancelled");
    cg.close();
}
