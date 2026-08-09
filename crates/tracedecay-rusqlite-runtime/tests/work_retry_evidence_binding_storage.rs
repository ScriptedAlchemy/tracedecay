//! Durable source-to-attempt retry evidence bindings.

mod work_registered_store;

use tracedecay_application::{
    CancellationContext, CapabilityGrantSnapshot, Deadline, DisclosureClass, RequestContext,
    RequestId, ResolvedScope, StoredWorkRetryEvidenceV1, WorkAttemptStoragePort,
    WorkProjectionPortError, WorkProjectionReadPort, WorkRetryEvidenceBindingStoragePortV1,
    WorkRetryEvidenceErrorV1, WorkRetryEvidencePortV1, WorkRetryManagedTestJournalPortV1,
    WorkRetryManagedTestJournalV1, WorkRetryServiceV1, WorkRetryTestBindingTokenRequestV1,
    WorkRetryTestBindingTokenServiceV1, WorkRetryTestFailureEvidenceV1,
};
use tracedecay_domain::{
    ActorId, AttemptId, CommitId, ConfigurationRevisionId, ConfigurationSnapshotId, ManifestDigest,
    ProjectId, ProjectionGenerationId, ProposalId, ProviderId, RefId, RepositoryId, RunId, TaskId,
    UtcMicros, WorkApprovalPolicy, WorkAttemptIdentityV1, WorkAttemptProjectionBindingV1,
    WorkAttemptStateV1, WorkAttemptV1, WorkAuthority, WorkCancellationStateV1, WorkEffectStateV1,
    WorkEgressPolicy, WorkExecutableReference, WorkExecutionEnvelopeV1, WorkExecutionLimits,
    WorkExecutionSnapshot, WorkExecutionSnapshotInput, WorkFallbackTopology, WorkFilesystemPolicy,
    WorkLeaseFenceV1, WorkLeaseId, WorkProjection, WorkProjectionCoverageV1, WorkProjectionDeltaV1,
    WorkProjectionResumeCursorV1, WorkProjectionSequenceV1, WorkProjectionSnapshotV1,
    WorkProviderBackendV1, WorkProviderProtocol, WorkProviderRouteId, WorkProviderRouteV1,
    WorkRecoveryStateV1, WorkSandboxPolicy, WorkTerminalEvidenceV1, WorkVersion,
    WorkflowOperationRef, WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use work_registered_store::RegisteredWorkStore;

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn authority() -> WorkAuthority {
    WorkAuthority::new(
        id::<ProjectId>("project.retry-binding.storage"),
        id::<RepositoryId>("repository.retry-binding.storage"),
        id::<WorktreeId>("worktree.retry-binding.storage"),
        id::<ActorId>("actor.retry-binding.storage"),
        digest('a'),
    )
    .unwrap()
}

fn context() -> RequestContext {
    let scope = ResolvedScope::new(
        id::<ProjectId>("project.retry-binding.storage"),
        id::<RepositoryId>("repository.retry-binding.storage"),
        id::<WorktreeId>("worktree.retry-binding.storage"),
        Some(id::<RefId>("refs/heads/retry-binding")),
    )
    .unwrap();
    let grant = CapabilityGrantSnapshot::new(
        id("grant.retry-binding.storage"),
        1,
        digest('a'),
        id::<ActorId>("actor.retry-binding.issuer"),
        UtcMicros(1),
        UtcMicros(100),
        scope.clone(),
        std::collections::BTreeSet::from([CapabilityId::new(
            "capability.work.retry-binding.storage",
        )
        .unwrap()]),
        std::collections::BTreeSet::from([
            UseCaseId::new("use-case.work.retry-binding.storage").unwrap()
        ]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    RequestContext::new(
        id::<ActorId>("actor.retry-binding.storage"),
        scope,
        grant,
        RequestId::new("request.retry-binding.storage").unwrap(),
        Deadline::new(UtcMicros(90)).unwrap(),
        CancellationContext::active("cancellation.retry-binding.storage").unwrap(),
    )
    .unwrap()
}

fn attempt(attempt_id: &str) -> WorkAttemptV1 {
    let identity = WorkAttemptIdentityV1::new(
        id::<TaskId>("task.retry-binding.storage"),
        id::<RunId>("run.retry-binding.storage"),
        id::<AttemptId>(attempt_id),
    )
    .unwrap();
    let binding = WorkAttemptProjectionBindingV1::new(
        id::<ProjectionGenerationId>("generation.retry-binding.storage"),
        tracedecay_domain::WorkProjectionSequenceV1::new(1),
        WorkVersion::new(1).unwrap(),
        id::<ProposalId>("proposal.retry-binding.storage"),
    )
    .unwrap();
    let route = WorkProviderRouteV1::new(
        id::<ProviderId>("provider.work.claude-code-cli"),
        id::<WorkProviderRouteId>("route.retry-binding.storage"),
    )
    .unwrap();
    let snapshot = WorkExecutionSnapshot::new(WorkExecutionSnapshotInput {
        configuration_revision_id: id::<ConfigurationRevisionId>(
            "configuration-revision.retry-binding.storage",
        ),
        configuration_snapshot_id: id::<ConfigurationSnapshotId>(
            "configuration-snapshot.retry-binding.storage",
        ),
        effective_behavior_digest: digest('b'),
        resolution_provenance_digest: digest('c'),
        route: route.clone(),
        backend: WorkProviderBackendV1::ClaudeCodeCli,
        protocol: WorkProviderProtocol::ClaudeStreamJson,
        model: "claude-test".to_owned(),
        executable: WorkExecutableReference::new(
            "executable.retry-binding".to_owned(),
            digest('d'),
        )
        .unwrap(),
        sandbox: WorkSandboxPolicy::Required,
        approval: WorkApprovalPolicy::Never,
        filesystem: WorkFilesystemPolicy::WorkspaceWrite,
        egress: WorkEgressPolicy::Deny,
        environment_allowlist: Default::default(),
        credential_references: Default::default(),
        limits: WorkExecutionLimits::new(1024, 1024, 1024, 1024, 1024, 1).unwrap(),
        deadline: UtcMicros(1_000),
        fallback: WorkFallbackTopology::Disabled,
        topology: tracedecay_domain::safe_work_topology_policy_v1(),
    })
    .unwrap();
    let execution = WorkExecutionEnvelopeV1::new(
        identity.clone(),
        binding.clone(),
        id::<WorkflowOperationRef>("operation.retry-binding.storage"),
        snapshot,
        id::<ProjectId>("project.retry-binding.storage"),
        id::<RepositoryId>("repository.retry-binding.storage"),
        id::<WorktreeId>("worktree.retry-binding.storage"),
        "/tmp/retry-binding-storage".to_owned(),
        Some(id::<RefId>("refs/heads/retry-binding")),
        id::<CommitId>("0123456789abcdef0123456789abcdef01234567"),
        "Execute the admitted provider step.".to_owned(),
        1,
        WorkEffectStateV1::Observational,
    )
    .unwrap();
    let leased = WorkAttemptV1::new(
        identity,
        binding,
        execution,
        WorkLeaseFenceV1::new(
            id::<WorkLeaseId>("lease.retry-binding.storage"),
            tracedecay_domain::WorkFenceEpochV1::new(1).unwrap(),
        )
        .unwrap(),
        WorkAttemptStateV1::Leased,
        None,
        Vec::new(),
        WorkCancellationStateV1::None,
        WorkRecoveryStateV1::Fresh,
        route,
        None,
        None,
    )
    .unwrap();
    let running = leased
        .transition(
            WorkAttemptStateV1::Running,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            Some(leased.requested_route().clone()),
            None,
            leased.lease().clone(),
        )
        .unwrap();
    running
        .transition(
            WorkAttemptStateV1::Failed,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            Some(running.requested_route().clone()),
            Some(WorkTerminalEvidenceV1::failed(digest('f'), UtcMicros(10)).unwrap()),
            running.lease().clone(),
        )
        .unwrap()
}

#[derive(Clone)]
struct RetryProjection {
    snapshot: WorkProjectionSnapshotV1,
}

impl RetryProjection {
    fn for_attempt(authority: &WorkAuthority, attempt: &WorkAttemptV1) -> Self {
        let projection: WorkProjection = serde_json::from_value(serde_json::json!({
            "task_id": attempt.identity().task_id(),
            "version": attempt.projection_binding().work_version(),
            "authority": authority,
            "title": "Retry binding fixture",
            "dependencies": [],
            "accepted_proposal": attempt.projection_binding().accepted_proposal(),
            "execution_admitted": true,
            "runtime_evidence": [],
            "task_accepted": true,
            "history_len": 1,
        }))
        .unwrap();
        Self {
            snapshot: WorkProjectionSnapshotV1::new(
                attempt.projection_binding().generation_id().clone(),
                WorkProjectionSequenceV1::new(1),
                vec![projection],
                WorkProjectionCoverageV1::complete(1, 1).unwrap(),
            )
            .unwrap(),
        }
    }
}

impl WorkProjectionReadPort for RetryProjection {
    fn exact_snapshot(
        &self,
        _authority: &WorkAuthority,
        _task_id: &TaskId,
    ) -> Result<WorkProjectionSnapshotV1, WorkProjectionPortError> {
        Ok(self.snapshot.clone())
    }

    fn snapshot(
        &self,
        _authority: &WorkAuthority,
        _page_size: u32,
    ) -> Result<WorkProjectionSnapshotV1, WorkProjectionPortError> {
        Err(WorkProjectionPortError::Unavailable)
    }

    fn delta(
        &self,
        _authority: &WorkAuthority,
        _cursor: &WorkProjectionResumeCursorV1,
        _page_size: u32,
    ) -> Result<WorkProjectionDeltaV1, WorkProjectionPortError> {
        Err(WorkProjectionPortError::Unavailable)
    }
}

fn test_failure(
    operation_id: &str,
    token: tracedecay_application::WorkRetryTestBindingTokenV1,
) -> WorkRetryTestFailureEvidenceV1 {
    WorkRetryTestFailureEvidenceV1 {
        operation_id: operation_id.to_owned(),
        token,
        terminal: tracedecay_application::OperationReceipt {
            started_at: UtcMicros(12),
            ended_at: UtcMicros(20),
            effective_deadline: Deadline::new(UtcMicros(30)).unwrap(),
            cancellation: None,
            budget: Default::default(),
            // The managed process completed and affirmatively reported a
            // failed test; this is distinct from an operational failure.
            termination: tracedecay_application::OperationTermination::Completed,
        },
        failed_tests: vec!["crate::retry_binding::failing_case".to_owned()],
    }
}

fn terminal_without_observed_test_failure(
    operation_id: &str,
    token: tracedecay_application::WorkRetryTestBindingTokenV1,
    termination: tracedecay_application::OperationTermination,
) -> WorkRetryTestFailureEvidenceV1 {
    let mut evidence = test_failure(operation_id, token);
    evidence.failed_tests.clear();
    evidence.terminal.termination = termination;
    evidence.terminal.cancellation = matches!(
        termination,
        tracedecay_application::OperationTermination::Cancelled
            | tracedecay_application::OperationTermination::TimedOut
    )
    .then_some(tracedecay_application::CancellationObservation {
        stage: tracedecay_application::CancellationStage::DuringRead,
        observed_at: UtcMicros(20),
    });
    evidence
}

#[test]
fn work_minted_test_token_binds_only_its_exact_attempt_once() {
    let mut store = RegisteredWorkStore::start("retry-evidence-source-uniqueness");
    let authority = authority();
    let context = context();
    let first = attempt("attempt.retry-binding.first");
    let second = attempt("attempt.retry-binding.second");
    store.storage().insert(&authority, &first).unwrap();
    store.storage().insert(&authority, &second).unwrap();
    let token = WorkRetryTestBindingTokenServiceV1::new(store.storage().clone())
        .mint_for_attempt(
            &context,
            UtcMicros(11),
            WorkRetryTestBindingTokenRequestV1 {
                original_attempt: first.identity().clone(),
            },
        )
        .unwrap()
        .token;
    let journal = WorkRetryManagedTestJournalV1::new(
        store.storage().clone(),
        authority.clone(),
        token.clone(),
    );
    journal.launch("operation.retry-binding.test.1").unwrap();
    let first_outcome = journal
        .seal(test_failure(
            "operation.retry-binding.test.1",
            token.clone(),
        ))
        .unwrap()
        .expect("failed managed Test must produce retry evidence");
    assert_eq!(first_outcome.original_attempt, *first.identity());
    assert_eq!(store.count("work_retry_evidence_bindings_v1"), 1);

    store = store.restart("retry-evidence-source-uniqueness");
    assert_eq!(
        store
            .storage()
            .resolve_test_retry_binding_authority(&token)
            .unwrap(),
        authority,
    );
    let evidence = StoredWorkRetryEvidenceV1::new(store.storage().clone())
        .resolve_failure(&authority, &first, &first_outcome.selector)
        .unwrap();
    assert_eq!(evidence.evidence_digest, first_outcome.evidence_digest);
    assert_eq!(
        StoredWorkRetryEvidenceV1::new(store.storage().clone()).resolve_failure(
            &authority,
            &second,
            &first_outcome.selector
        ),
        Err(WorkRetryEvidenceErrorV1::Conflict),
    );

    let retried = WorkRetryServiceV1::new(
        store.storage().clone(),
        RetryProjection::for_attempt(&authority, &first),
        StoredWorkRetryEvidenceV1::new(store.storage().clone()),
    )
    .retry(
        &context,
        &tracedecay_domain::safe_work_topology_policy_v1(),
        tracedecay_application::RetryWorkAttemptCommandV1 {
            original_attempt: first.identity().clone(),
            new_attempt_id: id::<AttemptId>("attempt.retry-binding.retried"),
            failure: first_outcome.selector.clone(),
            command_id: id("command.retry-binding.test.1"),
        },
        UtcMicros(21),
    )
    .unwrap();
    assert_eq!(
        retried.attempt().identity().attempt_id().as_str(),
        "attempt.retry-binding.retried"
    );
    assert_eq!(store.count("work_retry_receipts_v1"), 1);

    let restarted_journal =
        WorkRetryManagedTestJournalV1::new(store.storage().clone(), authority, token.clone());
    assert_eq!(
        restarted_journal.launch("operation.retry-binding.test.2"),
        Err(tracedecay_application::WorkRetryManagedTestJournalErrorV1::Conflict),
    );
    assert_eq!(
        restarted_journal
            .seal(test_failure(
                "operation.retry-binding.test.1",
                token.clone()
            ))
            .unwrap(),
        Some(first_outcome),
    );
}

#[test]
fn operational_test_terminals_without_failed_test_observation_retire_tokens() {
    let store = RegisteredWorkStore::start("retry-evidence-no-inference");
    let authority = authority();
    for (failure_class, termination) in [
        (
            "spawn",
            tracedecay_application::OperationTermination::Failed,
        ),
        ("read", tracedecay_application::OperationTermination::Failed),
        (
            "output-limit",
            tracedecay_application::OperationTermination::Failed,
        ),
        (
            "no-match",
            tracedecay_application::OperationTermination::Failed,
        ),
        (
            "cancelled",
            tracedecay_application::OperationTermination::Cancelled,
        ),
        (
            "timeout",
            tracedecay_application::OperationTermination::TimedOut,
        ),
        (
            "zero-failed",
            tracedecay_application::OperationTermination::Completed,
        ),
    ] {
        let attempt = attempt(&format!("attempt.retry-binding.{failure_class}"));
        store.storage().insert(&authority, &attempt).unwrap();
        let token = WorkRetryTestBindingTokenServiceV1::new(store.storage().clone())
            .mint_for_attempt(
                &context(),
                UtcMicros(11),
                WorkRetryTestBindingTokenRequestV1 {
                    original_attempt: attempt.identity().clone(),
                },
            )
            .unwrap()
            .token;
        let operation_id = format!("operation.retry-binding.test.{failure_class}");
        let journal = WorkRetryManagedTestJournalV1::new(
            store.storage().clone(),
            authority.clone(),
            token.clone(),
        );
        journal.launch(&operation_id).unwrap();

        let mut terminal =
            terminal_without_observed_test_failure(&operation_id, token, termination);
        if failure_class != "zero-failed" {
            terminal.failed_tests = vec!["crate::partial::failed_before_terminal".to_owned()];
        }
        assert_eq!(
            journal.seal(terminal).unwrap(),
            None,
            "{failure_class} must not produce TestFailure evidence",
        );
    }
    assert_eq!(store.count("work_retry_evidence_bindings_v1"), 0);

    let retryable_attempt = attempt("attempt.retry-binding.operational-remint");
    store
        .storage()
        .insert(&authority, &retryable_attempt)
        .unwrap();
    let first_token = WorkRetryTestBindingTokenServiceV1::new(store.storage().clone())
        .mint_for_attempt(
            &context(),
            UtcMicros(11),
            WorkRetryTestBindingTokenRequestV1 {
                original_attempt: retryable_attempt.identity().clone(),
            },
        )
        .unwrap()
        .token;
    let first_journal = WorkRetryManagedTestJournalV1::new(
        store.storage().clone(),
        authority.clone(),
        first_token.clone(),
    );
    first_journal
        .launch("operation.retry-binding.test.operational-remint.1")
        .unwrap();
    assert_eq!(
        first_journal
            .seal(terminal_without_observed_test_failure(
                "operation.retry-binding.test.operational-remint.1",
                first_token.clone(),
                tracedecay_application::OperationTermination::TimedOut,
            ))
            .unwrap(),
        None,
    );

    let second_token = WorkRetryTestBindingTokenServiceV1::new(store.storage().clone())
        .mint_for_attempt(
            &context(),
            UtcMicros(11),
            WorkRetryTestBindingTokenRequestV1 {
                original_attempt: retryable_attempt.identity().clone(),
            },
        )
        .unwrap()
        .token;
    assert_ne!(second_token, first_token);
    let second_journal = WorkRetryManagedTestJournalV1::new(
        store.storage().clone(),
        authority,
        second_token.clone(),
    );
    second_journal
        .launch("operation.retry-binding.test.operational-remint.2")
        .unwrap();
    assert!(
        second_journal
            .seal(test_failure(
                "operation.retry-binding.test.operational-remint.2",
                second_token,
            ))
            .unwrap()
            .is_some(),
        "a new exact managed-Test run may prove failure after an operational terminal",
    );
    assert_eq!(store.count("work_retry_evidence_bindings_v1"), 1);
}
