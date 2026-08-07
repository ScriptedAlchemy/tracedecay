//! Admitted-provider attempt authority contract: lease admission and denial,
//! idempotent starts, the cancellation ladder, restart fencing, staleness
//! refusal, and typed provider-availability terminal journeys.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use tracedecay_application::{
    AcceptProposalCommand, AdmitExecutionCommand, ApplicationProblemKind, CancelWorkAttemptCommand,
    CancellationContext, CapabilityGrantSnapshot, CreateWorkCommand, Deadline, DisclosureClass,
    GenerateProposalRequest, MAX_WORK_ATTEMPT_LIST_PAGE_SIZE, RequestContext, RequestId,
    ResolvedScope, ResumeWorkAttemptsCommand, ReviewProposalCommand, StartWorkAttemptCommand,
    WorkAppendOutcome, WorkAppendRequest, WorkAttemptEvidenceRecordV1, WorkAttemptInsertOutcome,
    WorkAttemptListCoverageV1, WorkAttemptListCursorV1, WorkAttemptListPageV1,
    WorkAttemptListRequestV1, WorkAttemptListV1, WorkAttemptProviderOutcomeV1, WorkAttemptService,
    WorkAttemptStatusRequestV1, WorkAttemptStorageError, WorkAttemptStoragePort,
    WorkAttemptTopologyBindingV1, WorkAttemptTopologyStateV1, WorkProjectionPortError,
    WorkProjectionReadPort, WorkService, WorkStorageError, WorkStoragePort,
};
use tracedecay_domain::{
    ActorId, CommitId, ConfigurationRevisionId, ConfigurationSnapshotId, ManifestDigest, ProjectId,
    ProjectionGenerationId, ProviderId, RefId, RepositoryId, TaskId, UtcMicros, WorkApprovalPolicy,
    WorkAttemptIdentityV1, WorkAttemptStateV1, WorkAttemptV1, WorkAuthority, WorkEffectStateV1,
    WorkEgressPolicy, WorkEvent, WorkExecutableReference, WorkExecutionLimits,
    WorkExecutionSnapshot, WorkExecutionSnapshotInput, WorkFallbackTopology, WorkFilesystemPolicy,
    WorkLeaseFenceV1, WorkProjection, WorkProjectionCoverageV1, WorkProjectionSequenceV1,
    WorkProjectionSnapshotV1, WorkProviderBackendV1, WorkProviderProtocol, WorkProviderRouteId,
    WorkProviderRouteV1, WorkSandboxPolicy, WorkVersion, WorkflowOperationRef, WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

type WorkHistoryKey = (WorkAuthority, TaskId);
type WorkHistories = Arc<Mutex<BTreeMap<WorkHistoryKey, Vec<WorkEvent>>>>;

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

fn context(project: &str, actor: &str) -> RequestContext {
    let scope = ResolvedScope::new(
        id::<ProjectId>(project),
        id::<RepositoryId>("repository.attempt.fixture"),
        id::<WorktreeId>("worktree.attempt.fixture"),
        None,
    )
    .unwrap();
    let capability = CapabilityId::new("capability.work.fixture").unwrap();
    let use_case = UseCaseId::new("use-case.work.fixture").unwrap();
    let grant = CapabilityGrantSnapshot::new(
        id("grant.work.fixture"),
        1,
        digest('a'),
        id::<ActorId>("actor.issuer"),
        UtcMicros(1),
        UtcMicros(10_000),
        scope.clone(),
        BTreeSet::from([capability]),
        BTreeSet::from([use_case]),
        DisclosureClass::Sensitive,
    )
    .unwrap();
    RequestContext::new(
        id::<ActorId>(actor),
        scope,
        grant,
        RequestId::new(format!("request.{project}.{actor}")).unwrap(),
        Deadline::new(UtcMicros(9_000)).unwrap(),
        CancellationContext::active(format!("cancel.{project}.{actor}")).unwrap(),
    )
    .unwrap()
}

#[derive(Clone, Default)]
struct TestStore {
    histories: WorkHistories,
}

impl WorkStoragePort for TestStore {
    fn load(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
    ) -> Result<Vec<WorkEvent>, WorkStorageError> {
        self.histories
            .lock()
            .unwrap()
            .get(&(authority.clone(), task_id.clone()))
            .cloned()
            .ok_or(WorkStorageError::NotFoundOrNotAuthorized)
    }

    fn projection(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
    ) -> Result<WorkProjection, WorkStorageError> {
        let history = self.load(authority, task_id)?;
        rebuild(&history)
    }

    fn append(&self, request: &WorkAppendRequest) -> Result<WorkAppendOutcome, WorkStorageError> {
        let mut histories = self.histories.lock().unwrap();
        let key = (
            request.event.authority().clone(),
            request.event.task_id().clone(),
        );
        let existing = histories.get(&key).cloned().unwrap_or_default();
        if let Some(prior) = existing
            .iter()
            .find(|event| event.command_id() == request.event.command_id())
        {
            return if prior.input_digest() == request.event.input_digest() {
                Ok(WorkAppendOutcome::Replayed(rebuild(&existing)?))
            } else {
                Err(WorkStorageError::IdempotencyConflict)
            };
        }
        let current = existing.last().map(WorkEvent::version);
        if current.is_none() && request.expected_version.is_some() {
            return Err(WorkStorageError::NotFoundOrNotAuthorized);
        }
        if current != request.expected_version {
            return Err(WorkStorageError::VersionConflict);
        }
        let history = histories.entry(key).or_default();
        history.push(request.event.clone());
        Ok(WorkAppendOutcome::Appended(rebuild(history)?))
    }
}

fn rebuild(history: &[WorkEvent]) -> Result<WorkProjection, WorkStorageError> {
    WorkProjection::rebuild(history).map_err(|_| WorkStorageError::Unavailable)
}

/// Exact-projection read port over the same in-memory Work history the
/// command authority writes.
#[derive(Clone)]
struct SnapshotPort {
    store: TestStore,
}

impl WorkProjectionReadPort for SnapshotPort {
    fn exact_snapshot(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
    ) -> Result<WorkProjectionSnapshotV1, WorkProjectionPortError> {
        let projection =
            self.store
                .projection(authority, task_id)
                .map_err(|error| match error {
                    WorkStorageError::NotFoundOrNotAuthorized => {
                        WorkProjectionPortError::NotFoundOrNotAuthorized
                    }
                    _ => WorkProjectionPortError::Unavailable,
                })?;
        WorkProjectionSnapshotV1::new(
            id::<ProjectionGenerationId>("generation.attempt.fixture"),
            WorkProjectionSequenceV1::new(1),
            vec![projection],
            WorkProjectionCoverageV1::complete(1, 1)
                .map_err(|_| WorkProjectionPortError::Unavailable)?,
        )
        .map_err(|_| WorkProjectionPortError::Unavailable)
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
        _cursor: &tracedecay_domain::WorkProjectionResumeCursorV1,
        _page_size: u32,
    ) -> Result<tracedecay_domain::WorkProjectionDeltaV1, WorkProjectionPortError> {
        Err(WorkProjectionPortError::Unavailable)
    }
}

type AttemptKey = (WorkAuthority, String);

#[derive(Default)]
struct AttemptRows {
    fences: BTreeMap<WorkAuthority, u64>,
    rows: BTreeMap<AttemptKey, String>,
    evidence: BTreeMap<AttemptKey, String>,
}

/// In-memory attempt rows with the same byte-identity replay and fenced
/// compare-and-swap semantics as the registered SQLite store.
#[derive(Clone, Default)]
struct AttemptStore {
    inner: Arc<Mutex<AttemptRows>>,
}

fn attempt_key(authority: &WorkAuthority, identity: &WorkAttemptIdentityV1) -> AttemptKey {
    (
        authority.clone(),
        format!(
            "{}/{}/{}",
            identity.task_id().as_str(),
            identity.run_id().as_str(),
            identity.attempt_id().as_str()
        ),
    )
}

impl WorkAttemptStoragePort for AttemptStore {
    fn next_fence_epoch(&self, authority: &WorkAuthority) -> Result<u64, WorkAttemptStorageError> {
        let mut inner = self.inner.lock().unwrap();
        let epoch = inner.fences.entry(authority.clone()).or_insert(0);
        *epoch += 1;
        Ok(*epoch)
    }

    fn insert(
        &self,
        authority: &WorkAuthority,
        attempt: &WorkAttemptV1,
    ) -> Result<WorkAttemptInsertOutcome, WorkAttemptStorageError> {
        let payload =
            serde_json::to_string(attempt).map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let mut inner = self.inner.lock().unwrap();
        let key = attempt_key(authority, attempt.identity());
        if let Some(existing) = inner.rows.get(&key) {
            return if *existing == payload {
                serde_json::from_str(existing)
                    .map(WorkAttemptInsertOutcome::Replayed)
                    .map_err(|_| WorkAttemptStorageError::Unavailable)
            } else {
                Err(WorkAttemptStorageError::AttemptConflict)
            };
        }
        inner.rows.insert(key, payload);
        Ok(WorkAttemptInsertOutcome::Inserted)
    }

    fn load(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<WorkAttemptV1, WorkAttemptStorageError> {
        let inner = self.inner.lock().unwrap();
        let payload = inner
            .rows
            .get(&attempt_key(authority, identity))
            .ok_or(WorkAttemptStorageError::NotFoundOrNotAuthorized)?;
        serde_json::from_str(payload).map_err(|_| WorkAttemptStorageError::Unavailable)
    }

    fn update(
        &self,
        authority: &WorkAuthority,
        expected_fence: &WorkLeaseFenceV1,
        expected_state: WorkAttemptStateV1,
        next: &WorkAttemptV1,
        evidence: Option<&WorkAttemptEvidenceRecordV1>,
    ) -> Result<(), WorkAttemptStorageError> {
        let payload =
            serde_json::to_string(next).map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let mut inner = self.inner.lock().unwrap();
        let key = attempt_key(authority, next.identity());
        let Some(existing) = inner.rows.get(&key) else {
            return Err(WorkAttemptStorageError::NotFoundOrNotAuthorized);
        };
        let current: WorkAttemptV1 =
            serde_json::from_str(existing).map_err(|_| WorkAttemptStorageError::Unavailable)?;
        if current.lease() != expected_fence || current.state() != expected_state {
            return Err(WorkAttemptStorageError::FenceConflict);
        }
        if let Some(evidence) = evidence {
            let record = serde_json::to_string(evidence)
                .map_err(|_| WorkAttemptStorageError::Unavailable)?;
            inner.evidence.insert(key.clone(), record);
        }
        inner.rows.insert(key, payload);
        Ok(())
    }

    fn open_attempts(
        &self,
        authority: &WorkAuthority,
    ) -> Result<Vec<WorkAttemptV1>, WorkAttemptStorageError> {
        let inner = self.inner.lock().unwrap();
        inner
            .rows
            .iter()
            .filter(|((row_authority, _), _)| row_authority == authority)
            .map(|(_, payload)| {
                serde_json::from_str::<WorkAttemptV1>(payload)
                    .map_err(|_| WorkAttemptStorageError::Unavailable)
            })
            .filter(|attempt| {
                attempt
                    .as_ref()
                    .map(|attempt| !attempt.is_terminal())
                    .unwrap_or(true)
            })
            .collect()
    }

    fn list(
        &self,
        authority: &WorkAuthority,
        start_after: Option<&WorkAttemptIdentityV1>,
        limit: u32,
    ) -> Result<WorkAttemptListPageV1, WorkAttemptStorageError> {
        let inner = self.inner.lock().unwrap();
        let start_ordinal =
            start_after.map(|identity| attempt_key(authority, identity).1);
        let mut pending = Vec::new();
        for ((row_authority, ordinal), payload) in inner.rows.iter() {
            if row_authority != authority {
                continue;
            }
            if let Some(start) = &start_ordinal {
                if ordinal <= start {
                    continue;
                }
            }
            pending.push(
                serde_json::from_str::<WorkAttemptV1>(payload)
                    .map_err(|_| WorkAttemptStorageError::Unavailable)?,
            );
        }
        let remaining =
            u32::try_from(pending.len()).map_err(|_| WorkAttemptStorageError::Unavailable)?;
        pending.truncate(limit as usize);
        Ok(WorkAttemptListPageV1 {
            attempts: pending,
            remaining,
        })
    }
}

type Fixture = (
    WorkAttemptService<AttemptStore, SnapshotPort, TestStore>,
    WorkService<TestStore>,
    RequestContext,
);

fn fixture(project: &str) -> Fixture {
    let store = TestStore::default();
    let work = WorkService::new(store.clone());
    let attempts = WorkAttemptService::new(
        AttemptStore::default(),
        SnapshotPort {
            store: store.clone(),
        },
        WorkService::new(store),
    );
    (attempts, work, context(project, "actor.attempt.owner"))
}

fn requested_route() -> WorkProviderRouteV1 {
    WorkProviderRouteV1::new(
        id::<ProviderId>("provider.work.claude-code-cli"),
        id::<WorkProviderRouteId>("route.attempt.claude-code.v1"),
    )
    .unwrap()
}

fn execution_snapshot() -> WorkExecutionSnapshot {
    WorkExecutionSnapshot::new(WorkExecutionSnapshotInput {
        configuration_revision_id: id::<ConfigurationRevisionId>("configuration-revision.att.1"),
        configuration_snapshot_id: id::<ConfigurationSnapshotId>("configuration-snapshot.att.1"),
        effective_behavior_digest: digest('c'),
        resolution_provenance_digest: digest('d'),
        route: requested_route(),
        backend: WorkProviderBackendV1::ClaudeCodeCli,
        protocol: WorkProviderProtocol::ClaudeStreamJson,
        model: "claude-test".to_owned(),
        executable: WorkExecutableReference::new(
            "executable.claude.code-cli".to_owned(),
            digest('e'),
        )
        .unwrap(),
        sandbox: WorkSandboxPolicy::Required,
        approval: WorkApprovalPolicy::Never,
        filesystem: WorkFilesystemPolicy::WorkspaceWrite,
        egress: WorkEgressPolicy::Deny,
        environment_allowlist: BTreeSet::new(),
        credential_references: BTreeSet::new(),
        limits: WorkExecutionLimits::new(128_000, 8_192, 16_384, 16_384, 65_536, 1).unwrap(),
        deadline: UtcMicros(1_000_000),
        fallback: WorkFallbackTopology::Disabled,
        topology_policy_digest: digest('f'),
    })
    .unwrap()
}

fn admit_work(work: &WorkService<TestStore>, context: &RequestContext, task: &str) {
    let task_id = id::<TaskId>(task);
    work.create(
        context,
        CreateWorkCommand {
            task_id: task_id.clone(),
            title: format!("Work for {task}"),
            dependencies: BTreeSet::new(),
            command_id: id(&format!("command.{task}.create")),
            occurred_at: UtcMicros(10),
        },
    )
    .unwrap();
    let proposal = work
        .generate_proposal(
            context,
            digest('b'),
            GenerateProposalRequest {
                task_id: task_id.clone(),
                proposal_id: id(&format!("proposal.{task}")),
                live_git_evidence: None,
                occurred_at: UtcMicros(15),
            },
        )
        .unwrap();
    work.accept_proposal(
        context,
        AcceptProposalCommand {
            review: ReviewProposalCommand {
                task_id: task_id.clone(),
                proposal_id: proposal.proposal_id,
                proposal_digest: proposal.proposal_digest,
                expected_version: WorkVersion::initial(),
                command_id: id(&format!("command.{task}.accept")),
                occurred_at: UtcMicros(20),
            },
        },
    )
    .unwrap();
    work.admit_execution(
        context,
        AdmitExecutionCommand {
            task_id,
            expected_version: WorkVersion::new(2).unwrap(),
            command_id: id(&format!("command.{task}.admit")),
            occurred_at: UtcMicros(30),
        },
    )
    .unwrap();
}

fn start_command(task: &str, attempt: &str) -> StartWorkAttemptCommand {
    StartWorkAttemptCommand {
        task_id: id(task),
        run_id: id(&format!("run.{task}")),
        attempt_id: id(attempt),
        operation: id::<WorkflowOperationRef>("operation.attempt.execute-provider"),
        execution_snapshot: execution_snapshot(),
        worktree_root: "/tmp/attempt-fixture".to_owned(),
        reference: Some(id::<RefId>("refs/heads/attempt-fixture")),
        commit: id::<CommitId>("0123456789abcdef0123456789abcdef01234567"),
        instructions: "Execute the admitted provider step.".to_owned(),
        effect_state: WorkEffectStateV1::Observational,
        occurred_at: UtcMicros(40),
    }
}

#[test]
fn start_is_denied_without_admitted_execution() {
    let (attempts, work, context) = fixture("project.attempt.denial");
    // A missing task is indistinguishable from an unauthorized one.
    let missing = attempts
        .start(&context, start_command("task.attempt.missing", "attempt.1"))
        .unwrap_err();
    assert_eq!(
        missing.kind(),
        ApplicationProblemKind::NotFoundOrNotAuthorized
    );
    // A created task without admitted execution is a typed denial, not a
    // queue: the attempt never reaches the lease store.
    work.create(
        &context,
        CreateWorkCommand {
            task_id: id("task.attempt.unadmitted"),
            title: "Unadmitted work".to_owned(),
            dependencies: BTreeSet::new(),
            command_id: id("command.attempt.unadmitted.create"),
            occurred_at: UtcMicros(10),
        },
    )
    .unwrap();
    let denied = attempts
        .start(
            &context,
            start_command("task.attempt.unadmitted", "attempt.1"),
        )
        .unwrap_err();
    assert_eq!(denied.kind(), ApplicationProblemKind::InvalidRequest);
}

#[test]
fn start_leases_once_and_replays_identical_admissions() {
    let (attempts, work, context) = fixture("project.attempt.start");
    admit_work(&work, &context, "task.attempt.start");
    let command = start_command("task.attempt.start", "attempt.1");
    let leased = attempts.start(&context, command.clone()).unwrap();
    assert_eq!(leased.state(), WorkAttemptStateV1::Leased);
    assert_eq!(leased.lease().epoch().get(), 1);
    assert_eq!(
        leased.execution().instructions(),
        "Execute the admitted provider step."
    );
    let replayed = attempts.start(&context, command).unwrap();
    assert_eq!(replayed, leased);
    let status = attempts
        .status(
            &context,
            &WorkAttemptStatusRequestV1 {
                task_id: id("task.attempt.start"),
                run_id: id("run.task.attempt.start"),
                attempt_id: id("attempt.1"),
            },
        )
        .unwrap();
    assert_eq!(status, leased);
}

#[test]
fn cancellation_ladder_reaches_cancelled_and_attaches_evidence() {
    let (attempts, work, context) = fixture("project.attempt.cancel");
    admit_work(&work, &context, "task.attempt.cancel");
    let leased = attempts
        .start(&context, start_command("task.attempt.cancel", "attempt.1"))
        .unwrap();
    let identity = leased.identity().clone();
    // Cancellation before the provider runs is a typed conflict.
    let premature = attempts
        .request_cancellation(
            &context,
            CancelWorkAttemptCommand {
                task_id: identity.task_id().clone(),
                run_id: identity.run_id().clone(),
                attempt_id: identity.attempt_id().clone(),
                request_id: id("cancellation.attempt.premature"),
                occurred_at: UtcMicros(50),
            },
        )
        .unwrap_err();
    assert_eq!(premature.kind(), ApplicationProblemKind::Conflict);

    attempts
        .mark_running(&context, &identity, requested_route())
        .unwrap();
    let requested = attempts
        .request_cancellation(
            &context,
            CancelWorkAttemptCommand {
                task_id: identity.task_id().clone(),
                run_id: identity.run_id().clone(),
                attempt_id: identity.attempt_id().clone(),
                request_id: id("cancellation.attempt.1"),
                occurred_at: UtcMicros(60),
            },
        )
        .unwrap();
    assert_eq!(requested.state(), WorkAttemptStateV1::CancellationRequested);
    // A different concurrent cancellation request is a conflict, not a merge.
    let conflicting = attempts
        .request_cancellation(
            &context,
            CancelWorkAttemptCommand {
                task_id: identity.task_id().clone(),
                run_id: identity.run_id().clone(),
                attempt_id: identity.attempt_id().clone(),
                request_id: id("cancellation.attempt.other"),
                occurred_at: UtcMicros(61),
            },
        )
        .unwrap_err();
    assert_eq!(conflicting.kind(), ApplicationProblemKind::Conflict);

    let acknowledged = attempts
        .acknowledge_cancellation(&context, &identity, UtcMicros(70))
        .unwrap();
    assert_eq!(
        acknowledged.state(),
        WorkAttemptStateV1::CancellationAcknowledged
    );
    let escalated = attempts
        .escalate_cancellation(&context, &identity, UtcMicros(80))
        .unwrap();
    assert_eq!(escalated.state(), WorkAttemptStateV1::CancellationEscalated);
    let evidence = WorkAttemptEvidenceRecordV1 {
        identity: identity.clone(),
        requested_route: escalated.requested_route().clone(),
        actual_route: escalated.actual_route().cloned(),
        outcome: WorkAttemptProviderOutcomeV1::Cancelled,
        stdout: None,
        stderr: None,
        observed_at: UtcMicros(90),
    };
    let cancelled = attempts.settle(&context, &identity, &evidence).unwrap();
    assert_eq!(cancelled.state(), WorkAttemptStateV1::Cancelled);
    assert!(cancelled.is_terminal());
    // The sealed evidence digest is attached to the Work projection through
    // the canonical command authority.
    let projection = work.load(&context, identity.task_id()).unwrap();
    assert_eq!(projection.runtime_evidence().len(), 1);
    assert_eq!(
        *projection.runtime_evidence()[0].evidence_digest(),
        evidence.digest().unwrap()
    );
}

#[test]
fn resume_fences_open_attempts_and_completes_lost_cancellations() {
    let (attempts, work, context) = fixture("project.attempt.resume");
    admit_work(&work, &context, "task.attempt.resume");
    let leased = attempts
        .start(&context, start_command("task.attempt.resume", "attempt.1"))
        .unwrap();
    let running_identity = {
        let command = StartWorkAttemptCommand {
            attempt_id: id("attempt.2"),
            ..start_command("task.attempt.resume", "attempt.2")
        };
        let attempt = attempts.start(&context, command).unwrap();
        attempts
            .mark_running(&context, attempt.identity(), requested_route())
            .unwrap();
        attempt.identity().clone()
    };
    let cancelling_identity = {
        let command = StartWorkAttemptCommand {
            attempt_id: id("attempt.3"),
            ..start_command("task.attempt.resume", "attempt.3")
        };
        let attempt = attempts.start(&context, command).unwrap();
        attempts
            .mark_running(&context, attempt.identity(), requested_route())
            .unwrap();
        attempts
            .request_cancellation(
                &context,
                CancelWorkAttemptCommand {
                    task_id: attempt.identity().task_id().clone(),
                    run_id: attempt.identity().run_id().clone(),
                    attempt_id: attempt.identity().attempt_id().clone(),
                    request_id: id("cancellation.attempt.lost"),
                    occurred_at: UtcMicros(50),
                },
            )
            .unwrap();
        attempt.identity().clone()
    };

    let report = attempts
        .resume(
            &context,
            &ResumeWorkAttemptsCommand {
                occurred_at: UtcMicros(100),
            },
        )
        .unwrap();
    assert_eq!(report.recovery_required.len(), 2);
    assert_eq!(report.cancelled.len(), 1);
    for fenced in &report.recovery_required {
        assert_eq!(fenced.state(), WorkAttemptStateV1::RecoveryRequired);
        assert!(fenced.lease().epoch().get() > leased.lease().epoch().get());
    }
    assert!(
        report
            .recovery_required
            .iter()
            .any(|attempt| attempt.identity() == &running_identity)
    );
    let cancelled = &report.cancelled[0];
    assert_eq!(cancelled.identity(), &cancelling_identity);
    assert_eq!(cancelled.state(), WorkAttemptStateV1::Cancelled);
    assert!(cancelled.is_terminal());

    // The old fence can no longer advance a fenced attempt: settling with
    // evidence prepared under the lost epoch is refused.
    let stale = attempts
        .settle(
            &context,
            &running_identity,
            &WorkAttemptEvidenceRecordV1 {
                identity: running_identity.clone(),
                requested_route: requested_route(),
                actual_route: Some(requested_route()),
                outcome: WorkAttemptProviderOutcomeV1::Exited { code: 0 },
                stdout: None,
                stderr: None,
                observed_at: UtcMicros(110),
            },
        )
        .unwrap_err();
    assert_eq!(stale.kind(), ApplicationProblemKind::InvalidRequest);

    // Recovery execution restarts the fenced attempt under the new fence.
    let restarted = attempts
        .mark_running(&context, &running_identity, requested_route())
        .unwrap();
    assert_eq!(restarted.state(), WorkAttemptStateV1::Running);
}

#[test]
fn provider_unavailability_is_a_typed_terminal_journey() {
    let (attempts, work, context) = fixture("project.attempt.unavailable");
    admit_work(&work, &context, "task.attempt.unavailable");
    let leased = attempts
        .start(
            &context,
            start_command("task.attempt.unavailable", "attempt.1"),
        )
        .unwrap();
    let identity = leased.identity().clone();
    let fenced = attempts
        .mark_provider_unavailable(&context, &identity)
        .unwrap();
    assert_eq!(fenced.state(), WorkAttemptStateV1::RecoveryRequired);
    let evidence = WorkAttemptEvidenceRecordV1 {
        identity: identity.clone(),
        requested_route: fenced.requested_route().clone(),
        actual_route: None,
        outcome: WorkAttemptProviderOutcomeV1::ProviderUnavailable {
            state: tracedecay_application::WorkProviderAvailabilityV1::Absent,
        },
        stdout: None,
        stderr: None,
        observed_at: UtcMicros(120),
    };
    let failed = attempts
        .fail_recovery(&context, &identity, &evidence)
        .unwrap();
    assert_eq!(failed.state(), WorkAttemptStateV1::Failed);
    assert!(failed.is_terminal());
    let projection = work.load(&context, identity.task_id()).unwrap();
    assert_eq!(projection.runtime_evidence().len(), 1);
    // Failing recovery twice replays nothing: the terminal row refuses a
    // second transition.
    let repeated = attempts
        .fail_recovery(&context, &identity, &evidence)
        .unwrap_err();
    assert_eq!(repeated.kind(), ApplicationProblemKind::Conflict);
}

fn verified_topology(generation: &str, task_count: u32) -> WorkAttemptTopologyStateV1 {
    WorkAttemptTopologyStateV1::Verified(WorkAttemptTopologyBindingV1 {
        generation: generation.to_owned(),
        task_count,
    })
}

#[test]
fn list_page_bounds_are_refused_before_any_topology_read() {
    let (attempts, _, context) = fixture("project.attempt.list.bounds");
    for page_size in [0, MAX_WORK_ATTEMPT_LIST_PAGE_SIZE + 1] {
        let refused = attempts
            .list(
                &context,
                &WorkAttemptListRequestV1 {
                    page_size,
                    cursor: None,
                },
                |_| panic!("an out-of-bounds page size must not resolve the topology"),
            )
            .unwrap_err();
        assert_eq!(refused.kind(), ApplicationProblemKind::InvalidRequest);
    }
}

#[test]
fn list_pages_attempts_in_stable_order_and_resumes_from_the_cursor() {
    let (attempts, work, context) = fixture("project.attempt.list.pages");
    admit_work(&work, &context, "task.attempt.list");
    for attempt_id in ["attempt.1", "attempt.2", "attempt.3"] {
        let command = StartWorkAttemptCommand {
            attempt_id: id(attempt_id),
            ..start_command("task.attempt.list", attempt_id)
        };
        attempts.start(&context, command).unwrap();
    }

    let first = attempts
        .list(
            &context,
            &WorkAttemptListRequestV1 {
                page_size: 2,
                cursor: None,
            },
            |_| Ok(verified_topology("generation.work.list.1", 1)),
        )
        .unwrap();
    let WorkAttemptListV1::Listed {
        topology,
        attempts: page,
        coverage,
    } = first
    else {
        panic!("an authorized populated scope must list");
    };
    assert_eq!(topology.generation, "generation.work.list.1");
    assert_eq!(topology.task_count, 1);
    assert_eq!(page.len(), 2);
    assert!(page[0].identity() < page[1].identity());
    assert_eq!(page[0].identity().attempt_id().as_str(), "attempt.1");
    assert_eq!(page[1].identity().attempt_id().as_str(), "attempt.2");
    let WorkAttemptListCoverageV1::Capped {
        returned,
        remaining,
        resume,
    } = coverage
    else {
        panic!("a capped page must carry a resume cursor");
    };
    assert_eq!((returned, remaining), (2, 1));
    assert_eq!(resume.generation, "generation.work.list.1");
    assert_eq!(&resume.start_after, page[1].identity());

    let second = attempts
        .list(
            &context,
            &WorkAttemptListRequestV1 {
                page_size: 2,
                cursor: Some(resume),
            },
            |_| Ok(verified_topology("generation.work.list.1", 1)),
        )
        .unwrap();
    let WorkAttemptListV1::Listed {
        attempts: rest,
        coverage,
        ..
    } = second
    else {
        panic!("the resumed page must list");
    };
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].identity().attempt_id().as_str(), "attempt.3");
    assert_eq!(
        coverage,
        WorkAttemptListCoverageV1::Complete { returned: 1 }
    );
}

#[test]
fn list_of_an_authorized_scope_without_attempts_is_an_explicit_zero_complete_page() {
    let (attempts, work, context) = fixture("project.attempt.list.zero");
    admit_work(&work, &context, "task.attempt.list.zero");
    let listed = attempts
        .list(
            &context,
            &WorkAttemptListRequestV1 {
                page_size: 10,
                cursor: None,
            },
            |_| Ok(verified_topology("generation.work.list.zero", 1)),
        )
        .unwrap();
    let WorkAttemptListV1::Listed {
        attempts: page,
        coverage,
        ..
    } = listed
    else {
        panic!("an authorized empty scope must list, not conceal");
    };
    assert!(page.is_empty());
    assert_eq!(
        coverage,
        WorkAttemptListCoverageV1::Complete { returned: 0 }
    );
}

#[test]
fn list_without_any_work_is_a_typed_absent_state() {
    let (attempts, _, context) = fixture("project.attempt.list.absent");
    let listed = attempts
        .list(
            &context,
            &WorkAttemptListRequestV1 {
                page_size: 10,
                cursor: None,
            },
            |_| Ok(WorkAttemptTopologyStateV1::Absent),
        )
        .unwrap();
    assert_eq!(listed, WorkAttemptListV1::Absent);
}

#[test]
fn list_cursor_from_a_superseded_topology_generation_is_stale() {
    let (attempts, work, context) = fixture("project.attempt.list.stale");
    admit_work(&work, &context, "task.attempt.list.stale");
    attempts
        .start(
            &context,
            start_command("task.attempt.list.stale", "attempt.1"),
        )
        .unwrap();
    let cursor = WorkAttemptListCursorV1 {
        generation: "generation.work.list.old".to_owned(),
        start_after: identity_of("task.attempt.list.stale", "attempt.1"),
    };
    // A newer verified generation refuses the old cursor.
    let stale = attempts
        .list(
            &context,
            &WorkAttemptListRequestV1 {
                page_size: 2,
                cursor: Some(cursor.clone()),
            },
            |_| Ok(verified_topology("generation.work.list.new", 1)),
        )
        .unwrap_err();
    assert_eq!(stale.kind(), ApplicationProblemKind::Stale);
    // A scope whose topology no longer exists refuses the cursor the same way.
    let gone = attempts
        .list(
            &context,
            &WorkAttemptListRequestV1 {
                page_size: 2,
                cursor: Some(cursor),
            },
            |_| Ok(WorkAttemptTopologyStateV1::Absent),
        )
        .unwrap_err();
    assert_eq!(gone.kind(), ApplicationProblemKind::Stale);
}

#[test]
fn list_conceals_foreign_scopes_behind_their_own_typed_states() {
    let (attempts, work, owner) = fixture("project.attempt.list.conceal");
    admit_work(&work, &owner, "task.attempt.list.conceal");
    attempts
        .start(
            &owner,
            start_command("task.attempt.list.conceal", "attempt.1"),
        )
        .unwrap();

    // A foreign actor's authority resolves its own topology: absent, exactly
    // like a scope that never had Work.
    let foreign = context("project.attempt.list.conceal", "actor.attempt.foreign");
    let absent = attempts
        .list(
            &foreign,
            &WorkAttemptListRequestV1 {
                page_size: 10,
                cursor: None,
            },
            |_| Ok(WorkAttemptTopologyStateV1::Absent),
        )
        .unwrap();
    assert_eq!(absent, WorkAttemptListV1::Absent);

    // Even against a verified topology, the foreign authority scope holds no
    // rows: nothing owned by another actor ever leaks into the page.
    let empty = attempts
        .list(
            &foreign,
            &WorkAttemptListRequestV1 {
                page_size: 10,
                cursor: None,
            },
            |_| Ok(verified_topology("generation.work.list.conceal", 1)),
        )
        .unwrap();
    let WorkAttemptListV1::Listed {
        attempts: page,
        coverage,
        ..
    } = empty
    else {
        panic!("a foreign authorized scope lists its own (empty) attempt set");
    };
    assert!(page.is_empty());
    assert_eq!(
        coverage,
        WorkAttemptListCoverageV1::Complete { returned: 0 }
    );
}

fn identity_of(task: &str, attempt: &str) -> WorkAttemptIdentityV1 {
    WorkAttemptIdentityV1::new(id(task), id(&format!("run.{task}")), id(attempt)).unwrap()
}
