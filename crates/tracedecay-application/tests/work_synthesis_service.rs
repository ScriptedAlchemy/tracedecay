//! Admitted synthesis over fan-out sibling evidence: source-set sealing,
//! citation completeness, preservation of failures/unknowns/disagreement,
//! and the unsynthesized-set answer when nothing is citable.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use tracedecay_application::{
    AcceptProposalCommand, AdmitExecutionCommand, AdmitWorkSynthesisCommand,
    ApplicationProblemKind, CancellationContext, CapabilityGrantSnapshot, CreateWorkCommand,
    Deadline, DisclosureClass, GenerateProposalRequest, RequestContext, RequestId, ResolvedScope,
    ReviewProposalCommand, StartWorkAttemptCommand, WorkAppendOutcome, WorkAppendRequest,
    WorkAttemptAdmissionKind, WorkAttemptEvidenceRecordV1, WorkAttemptInsertOutcome,
    WorkAttemptListPageV1, WorkAttemptService, WorkAttemptStatusRequestV1, WorkAttemptStorageError,
    WorkAttemptStoragePort, WorkProjectionPortError, WorkProjectionReadPort,
    WorkRoutingSnapshotErrorV1, WorkRoutingSnapshotPortV1, WorkRoutingSnapshotV1, WorkService,
    WorkStorageError, WorkStoragePort, WorkSynthesisAdmissionRecordV1,
    WorkSynthesisAdmissionStoragePort, WorkSynthesisAttemptV1, WorkSynthesisInsertOutcome,
    WorkSynthesisRefusalV1, WorkSynthesisSourceEnvelopeV1, WorkSynthesisSourceOutcomeV1,
    WorkSynthesisSourceSetV1, admit_work_synthesis,
};
use tracedecay_domain::{
    ActorId, AttemptId, CommitId, ConfigurationRevisionId, ConfigurationSnapshotId, ManifestDigest,
    ProjectId, ProjectionGenerationId, ProposalId, ProviderId, RefId, RepositoryId, RunId, TaskId,
    UtcMicros, WorkApprovalPolicy, WorkArtifactId, WorkArtifactRefV1, WorkAttemptIdentityV1,
    WorkAttemptProjectionBindingV1, WorkAttemptStateV1, WorkAttemptV1, WorkAuthority,
    WorkCancellationStateV1, WorkEffectStateV1, WorkEgressPolicy, WorkEvent,
    WorkExecutableReference, WorkExecutionEnvelopeV1, WorkExecutionLimits, WorkExecutionSnapshot,
    WorkExecutionSnapshotInput, WorkFallbackTopology, WorkFenceEpochV1, WorkFilesystemPolicy,
    WorkLeaseFenceV1, WorkLeaseId, WorkProjection, WorkProjectionCoverageV1,
    WorkProjectionSequenceV1, WorkProjectionSnapshotV1, WorkProviderBackendV1,
    WorkProviderProtocol, WorkProviderRouteId, WorkProviderRouteV1, WorkRecoveryStateV1,
    WorkSandboxPolicy, WorkTerminalEvidenceV1, WorkVersion, WorkflowOperationRef,
    WorkflowOutputName, WorktreeId,
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
        id::<RepositoryId>("repository.synthesis.fixture"),
        id::<WorktreeId>("worktree.synthesis.fixture"),
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

fn authority(project: &str) -> WorkAuthority {
    WorkAuthority::new(
        id::<ProjectId>(project),
        id::<RepositoryId>("repository.synthesis.fixture"),
        id::<WorktreeId>("worktree.synthesis.fixture"),
        id::<ActorId>("actor.synthesis.owner"),
        digest('a'),
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

struct EmptyProposalRouting;

impl WorkRoutingSnapshotPortV1 for EmptyProposalRouting {
    fn routing_snapshot(
        &self,
        _context: &RequestContext,
        _task_id: &TaskId,
    ) -> Result<WorkRoutingSnapshotV1, WorkRoutingSnapshotErrorV1> {
        Ok(WorkRoutingSnapshotV1::default())
    }
}

const EMPTY_PROPOSAL_ROUTING: EmptyProposalRouting = EmptyProposalRouting;

fn rebuild(history: &[WorkEvent]) -> Result<WorkProjection, WorkStorageError> {
    WorkProjection::rebuild(history).map_err(|_| WorkStorageError::Unavailable)
}

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
            id::<ProjectionGenerationId>("generation.synthesis.fixture"),
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

#[derive(serde::Deserialize, serde::Serialize)]
struct StoredAttempt {
    attempt: WorkAttemptV1,
    synthesis: Option<WorkSynthesisAdmissionRecordV1>,
}

#[derive(Clone, Default)]
struct AttemptStore {
    inner: Arc<Mutex<AttemptRows>>,
}

impl AttemptStore {
    fn committed_result_count(
        &self,
        authority: &WorkAuthority,
        run_id: &RunId,
    ) -> Result<usize, WorkAttemptStorageError> {
        let inner = self.inner.lock().unwrap();
        inner
            .rows
            .iter()
            .filter(|((row_authority, _), _)| row_authority == authority)
            .try_fold(0, |count, (_, payload)| {
                let record: StoredAttempt = serde_json::from_str(payload)
                    .map_err(|_| WorkAttemptStorageError::Unavailable)?;
                Ok(count
                    + usize::from(
                        record.synthesis.is_some() && record.attempt.identity().run_id() == run_id,
                    ))
            })
    }
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
        let payload = serde_json::to_string(&StoredAttempt {
            attempt: attempt.clone(),
            synthesis: None,
        })
        .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let mut inner = self.inner.lock().unwrap();
        let key = attempt_key(authority, attempt.identity());
        if let Some(existing) = inner.rows.get(&key) {
            return if *existing == payload {
                serde_json::from_str::<StoredAttempt>(existing)
                    .map(|record| WorkAttemptInsertOutcome::Replayed(Box::new(record.attempt)))
                    .map_err(|_| WorkAttemptStorageError::Unavailable)
            } else {
                Err(WorkAttemptStorageError::AttemptConflict)
            };
        }
        inner.rows.insert(key, payload);
        Ok(WorkAttemptInsertOutcome::Inserted)
    }

    fn insert_bounded(
        &self,
        authority: &WorkAuthority,
        attempt: &WorkAttemptV1,
        _concurrency: &tracedecay_domain::configuration::TopologyConcurrencyPolicyV1,
    ) -> Result<WorkAttemptInsertOutcome, WorkAttemptStorageError> {
        self.insert(authority, attempt)
    }

    fn admission_capacities(
        &self,
        _authority: &WorkAuthority,
        task_ids: &[TaskId],
        concurrency: &tracedecay_domain::configuration::TopologyConcurrencyPolicyV1,
    ) -> Result<
        BTreeMap<TaskId, tracedecay_application::WorkAttemptCapacityV1>,
        WorkAttemptStorageError,
    > {
        Ok(task_ids
            .iter()
            .cloned()
            .map(|task_id| {
                (
                    task_id,
                    tracedecay_application::WorkAttemptCapacityV1::new(
                        0,
                        0,
                        0,
                        concurrency.clone(),
                    ),
                )
            })
            .collect())
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
        serde_json::from_str::<StoredAttempt>(payload)
            .map(|record| record.attempt)
            .map_err(|_| WorkAttemptStorageError::Unavailable)
    }

    fn load_admission_kind(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<WorkAttemptAdmissionKind, WorkAttemptStorageError> {
        let inner = self.inner.lock().unwrap();
        let payload = inner
            .rows
            .get(&attempt_key(authority, identity))
            .ok_or(WorkAttemptStorageError::NotFoundOrNotAuthorized)?;
        let record: StoredAttempt =
            serde_json::from_str(payload).map_err(|_| WorkAttemptStorageError::Unavailable)?;
        Ok(if record.synthesis.is_some() {
            WorkAttemptAdmissionKind::Synthesis
        } else {
            WorkAttemptAdmissionKind::Ordinary
        })
    }

    fn update(
        &self,
        authority: &WorkAuthority,
        expected_fence: &WorkLeaseFenceV1,
        expected_state: WorkAttemptStateV1,
        next: &WorkAttemptV1,
        evidence: Option<&WorkAttemptEvidenceRecordV1>,
    ) -> Result<(), WorkAttemptStorageError> {
        let mut inner = self.inner.lock().unwrap();
        let key = attempt_key(authority, next.identity());
        let Some(existing) = inner.rows.get(&key) else {
            return Err(WorkAttemptStorageError::NotFoundOrNotAuthorized);
        };
        let mut record: StoredAttempt =
            serde_json::from_str(existing).map_err(|_| WorkAttemptStorageError::Unavailable)?;
        if record.attempt.lease() != expected_fence || record.attempt.state() != expected_state {
            return Err(WorkAttemptStorageError::FenceConflict);
        }
        if let Some(evidence) = evidence {
            let record = serde_json::to_string(evidence)
                .map_err(|_| WorkAttemptStorageError::Unavailable)?;
            inner.evidence.insert(key.clone(), record);
        }
        record.attempt = next.clone();
        let payload =
            serde_json::to_string(&record).map_err(|_| WorkAttemptStorageError::Unavailable)?;
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
                serde_json::from_str::<StoredAttempt>(payload)
                    .map(|record| record.attempt)
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
        let start_ordinal = start_after.map(|identity| attempt_key(authority, identity).1);
        let mut pending = Vec::new();
        for ((row_authority, ordinal), payload) in inner.rows.iter() {
            if row_authority != authority {
                continue;
            }
            if let Some(start) = &start_ordinal
                && ordinal <= start
            {
                continue;
            }
            pending.push(
                serde_json::from_str::<StoredAttempt>(payload)
                    .map(|record| record.attempt)
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

impl WorkSynthesisAdmissionStoragePort for AttemptStore {
    fn insert_synthesis(
        &self,
        authority: &WorkAuthority,
        record: &WorkSynthesisAdmissionRecordV1,
    ) -> Result<WorkSynthesisInsertOutcome, WorkAttemptStorageError> {
        let key = attempt_key(authority, record.result.attempt.identity());
        let mut inner = self.inner.lock().unwrap();
        if let Some(existing) = inner.rows.get(&key) {
            let existing: StoredAttempt =
                serde_json::from_str(existing).map_err(|_| WorkAttemptStorageError::Unavailable)?;
            return match existing.synthesis {
                Some(existing) if existing.request_digest == record.request_digest => Ok(
                    WorkSynthesisInsertOutcome::Replayed(Box::new(existing.result)),
                ),
                _ => Err(WorkAttemptStorageError::AttemptConflict),
            };
        }
        let payload = serde_json::to_string(&StoredAttempt {
            attempt: record.result.attempt.clone(),
            synthesis: Some(record.clone()),
        })
        .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        inner.rows.insert(key, payload);
        Ok(WorkSynthesisInsertOutcome::Inserted)
    }

    fn insert_synthesis_bounded(
        &self,
        authority: &WorkAuthority,
        record: &WorkSynthesisAdmissionRecordV1,
        _concurrency: &tracedecay_domain::configuration::TopologyConcurrencyPolicyV1,
    ) -> Result<WorkSynthesisInsertOutcome, WorkAttemptStorageError> {
        self.insert_synthesis(authority, record)
    }

    fn load_synthesis(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<WorkSynthesisAdmissionRecordV1, WorkAttemptStorageError> {
        let inner = self.inner.lock().unwrap();
        let payload = inner
            .rows
            .get(&attempt_key(authority, identity))
            .ok_or(WorkAttemptStorageError::NotFoundOrNotAuthorized)?;
        serde_json::from_str::<StoredAttempt>(payload)
            .map_err(|_| WorkAttemptStorageError::Unavailable)?
            .synthesis
            .ok_or(WorkAttemptStorageError::AttemptConflict)
    }
}

type Fixture = (
    WorkAttemptService<AttemptStore, SnapshotPort, TestStore>,
    AttemptStore,
    WorkService<TestStore>,
    RequestContext,
);

fn fixture(project: &str) -> Fixture {
    let store = TestStore::default();
    let attempt_store = AttemptStore::default();
    let work = WorkService::new(store.clone());
    let attempts = WorkAttemptService::new(
        attempt_store.clone(),
        SnapshotPort {
            store: store.clone(),
        },
        WorkService::new(store),
    );
    (
        attempts,
        attempt_store,
        work,
        context(project, "actor.synthesis.owner"),
    )
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
        configuration_revision_id: id::<ConfigurationRevisionId>("configuration-revision.syn.1"),
        configuration_snapshot_id: id::<ConfigurationSnapshotId>("configuration-snapshot.syn.1"),
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
        topology: tracedecay_domain::safe_work_topology_policy_v1(),
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
            &EMPTY_PROPOSAL_ROUTING,
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
        worktree_root: "/tmp/synthesis-fixture".to_owned(),
        reference: Some(id::<RefId>("refs/heads/synthesis-fixture")),
        commit: id::<CommitId>("0123456789abcdef0123456789abcdef01234567"),
        instructions: "Synthesize the fan-out sibling evidence.".to_owned(),
        effect_state: WorkEffectStateV1::Observational,
        occurred_at: UtcMicros(40),
    }
}

fn source_identity(task: &str, attempt: &str) -> WorkAttemptIdentityV1 {
    WorkAttemptIdentityV1::new(
        id::<TaskId>(task),
        id::<RunId>(&format!("run.{task}")),
        id::<AttemptId>(attempt),
    )
    .unwrap()
}

fn leased_attempt(identity: WorkAttemptIdentityV1) -> WorkAttemptV1 {
    let binding = WorkAttemptProjectionBindingV1::new(
        id::<ProjectionGenerationId>("generation.synthesis.fixture"),
        WorkProjectionSequenceV1::new(7),
        WorkVersion::new(3).unwrap(),
        id::<ProposalId>("proposal.synthesis.fixture"),
    )
    .unwrap();
    let envelope = WorkExecutionEnvelopeV1::new(
        identity.clone(),
        binding.clone(),
        id::<WorkflowOperationRef>("operation.attempt.execute-provider"),
        execution_snapshot(),
        id::<ProjectId>("project.synthesis.sources"),
        id::<RepositoryId>("repository.synthesis.fixture"),
        id::<WorktreeId>("worktree.synthesis.fixture"),
        "/tmp/synthesis-fixture".to_owned(),
        Some(id::<RefId>("refs/heads/synthesis-fixture")),
        id::<CommitId>("0123456789abcdef0123456789abcdef01234567"),
        "Execute the admitted provider step.".to_owned(),
        1,
        WorkEffectStateV1::Observational,
    )
    .unwrap();
    WorkAttemptV1::new(
        identity,
        binding,
        envelope,
        WorkLeaseFenceV1::new(
            id::<WorkLeaseId>("lease.synthesis.fixture"),
            WorkFenceEpochV1::new(1).unwrap(),
        )
        .unwrap(),
        WorkAttemptStateV1::Leased,
        None,
        Vec::new(),
        WorkCancellationStateV1::None,
        WorkRecoveryStateV1::Fresh,
        requested_route(),
        None,
        None,
    )
    .unwrap()
}

/// Drives a fixture attempt to the requested terminal state and inserts it
/// directly into the attempt store, the way the registered store carries
/// settled rows.
fn insert_terminal_source(
    store: &AttemptStore,
    authority: &WorkAuthority,
    identity: WorkAttemptIdentityV1,
    state: WorkAttemptStateV1,
    artifacts: Vec<WorkArtifactRefV1>,
    evidence_digest: ManifestDigest,
) -> WorkAttemptV1 {
    let leased = leased_attempt(identity);
    let running = leased
        .transition(
            WorkAttemptStateV1::Running,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            Some(requested_route()),
            None,
            leased.lease().clone(),
        )
        .unwrap();
    let terminal = match state {
        WorkAttemptStateV1::Succeeded => {
            WorkTerminalEvidenceV1::succeeded(evidence_digest, UtcMicros(500)).unwrap()
        }
        WorkAttemptStateV1::Failed => {
            WorkTerminalEvidenceV1::failed(evidence_digest, UtcMicros(500)).unwrap()
        }
        state => panic!("fixture only settles succeeded or failed sources, got {state:?}"),
    };
    let settled = running
        .transition(
            state,
            None,
            artifacts,
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            Some(requested_route()),
            Some(terminal),
            running.lease().clone(),
        )
        .unwrap();
    store.insert(authority, &settled).unwrap();
    settled
}

fn insert_running_source(
    store: &AttemptStore,
    authority: &WorkAuthority,
    identity: WorkAttemptIdentityV1,
) -> WorkAttemptV1 {
    let leased = leased_attempt(identity);
    let running = leased
        .transition(
            WorkAttemptStateV1::Running,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            Some(requested_route()),
            None,
            leased.lease().clone(),
        )
        .unwrap();
    store.insert(authority, &running).unwrap();
    running
}

fn mark_source_succeeded(
    store: &AttemptStore,
    authority: &WorkAuthority,
    running: &WorkAttemptV1,
    artifacts: Vec<WorkArtifactRefV1>,
) -> WorkAttemptV1 {
    let terminal = WorkTerminalEvidenceV1::succeeded(digest('0'), UtcMicros(600)).unwrap();
    let succeeded = running
        .transition(
            WorkAttemptStateV1::Succeeded,
            None,
            artifacts,
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            Some(requested_route()),
            Some(terminal),
            running.lease().clone(),
        )
        .unwrap();
    store
        .update(
            authority,
            running.lease(),
            WorkAttemptStateV1::Running,
            &succeeded,
            None,
        )
        .unwrap();
    succeeded
}

fn artifact(name: &str, byte: char) -> WorkArtifactRefV1 {
    WorkArtifactRefV1::new(id::<WorkArtifactId>(name), digest(byte), 128).unwrap()
}

fn synthesis_command(sources: Vec<WorkAttemptIdentityV1>) -> AdmitWorkSynthesisCommand {
    AdmitWorkSynthesisCommand {
        start: start_command("task.synthesis", "attempt.synthesis"),
        output_name: id::<WorkflowOutputName>("output.synthesis.fixture"),
        sources,
    }
}

#[test]
fn synthesis_refuses_empty_duplicate_and_self_source_sets() {
    let (attempts, _, _, context) = fixture("project.synthesis.refusals");
    let empty =
        admit_work_synthesis(&attempts, &context, synthesis_command(Vec::new())).unwrap_err();
    assert_eq!(empty.kind(), ApplicationProblemKind::InvalidRequest);

    let source = source_identity("task.source.a", "attempt.1");
    let duplicated = admit_work_synthesis(
        &attempts,
        &context,
        synthesis_command(vec![source.clone(), source.clone()]),
    )
    .unwrap_err();
    assert_eq!(duplicated.kind(), ApplicationProblemKind::InvalidRequest);

    let own_identity = source_identity("task.synthesis", "attempt.synthesis");
    let self_citing =
        admit_work_synthesis(&attempts, &context, synthesis_command(vec![own_identity]))
            .unwrap_err();
    assert_eq!(self_citing.kind(), ApplicationProblemKind::InvalidRequest);
}

#[test]
fn synthesis_refuses_an_unknown_source() {
    let (attempts, _, _, context) = fixture("project.synthesis.unknown-source");
    let missing = admit_work_synthesis(
        &attempts,
        &context,
        synthesis_command(vec![source_identity("task.source.ghost", "attempt.1")]),
    )
    .unwrap_err();
    assert_eq!(
        missing.kind(),
        ApplicationProblemKind::NotFoundOrNotAuthorized
    );
}

#[test]
fn synthesis_returns_the_unsynthesized_set_when_nothing_is_citable() {
    let (attempts, attempt_store, _, context) = fixture("project.synthesis.unsynthesized");
    let mine = authority("project.synthesis.unsynthesized");
    let failed = source_identity("task.source.failed", "attempt.1");
    insert_terminal_source(
        &attempt_store,
        &mine,
        failed.clone(),
        WorkAttemptStateV1::Failed,
        Vec::new(),
        digest('1'),
    );
    let running = source_identity("task.source.running", "attempt.1");
    insert_running_source(&attempt_store, &mine, running.clone());
    let bare = source_identity("task.source.bare", "attempt.1");
    insert_terminal_source(
        &attempt_store,
        &mine,
        bare.clone(),
        WorkAttemptStateV1::Succeeded,
        Vec::new(),
        digest('2'),
    );

    let outcome = admit_work_synthesis(
        &attempts,
        &context,
        synthesis_command(vec![failed.clone(), running.clone(), bare.clone()]),
    )
    .unwrap();
    let WorkSynthesisAttemptV1::Unsynthesized { sources, refusal } = outcome else {
        panic!("expected the unsynthesized set, got an admission");
    };
    assert_eq!(refusal, WorkSynthesisRefusalV1::NoCitableSources);
    assert!(sources.verified());
    // Every source is preserved verbatim, in the requested order: the
    // failure with its sealed evidence digest, the unknown as an unknown,
    // and the artifact-less success with nothing fabricated for it.
    assert_eq!(
        sources.sources,
        vec![
            WorkSynthesisSourceEnvelopeV1 {
                source: failed,
                outcome: WorkSynthesisSourceOutcomeV1::Failed {
                    evidence: digest('1'),
                },
            },
            WorkSynthesisSourceEnvelopeV1 {
                source: running,
                outcome: WorkSynthesisSourceOutcomeV1::Unknown {
                    state: WorkAttemptStateV1::Running,
                },
            },
            WorkSynthesisSourceEnvelopeV1 {
                source: bare,
                outcome: WorkSynthesisSourceOutcomeV1::Succeeded {
                    artifacts: Vec::new(),
                },
            },
        ]
    );
    // No synthesis attempt was admitted.
    let unadmitted = attempts
        .status(
            &context,
            &WorkAttemptStatusRequestV1 {
                task_id: id("task.synthesis"),
                run_id: id("run.task.synthesis"),
                attempt_id: id("attempt.synthesis"),
            },
        )
        .unwrap_err();
    assert_eq!(
        unadmitted.kind(),
        ApplicationProblemKind::NotFoundOrNotAuthorized
    );
}

#[test]
fn synthesis_admits_citing_every_citable_source_and_preserves_the_rest() {
    let (attempts, attempt_store, work, context) = fixture("project.synthesis.admission");
    let mine = authority("project.synthesis.admission");
    admit_work(&work, &context, "task.synthesis");

    // Two sources agree on the same artifact pair, one dissents with a
    // different artifact, and one failed outright.
    let agree_a = source_identity("task.source.agree-a", "attempt.1");
    insert_terminal_source(
        &attempt_store,
        &mine,
        agree_a.clone(),
        WorkAttemptStateV1::Succeeded,
        vec![
            artifact("artifact.log", '3'),
            artifact("artifact.patch", '4'),
        ],
        digest('5'),
    );
    let agree_b = source_identity("task.source.agree-b", "attempt.1");
    insert_terminal_source(
        &attempt_store,
        &mine,
        agree_b.clone(),
        WorkAttemptStateV1::Succeeded,
        vec![
            artifact("artifact.log", '3'),
            artifact("artifact.patch", '4'),
        ],
        digest('6'),
    );
    let dissent = source_identity("task.source.dissent", "attempt.1");
    insert_terminal_source(
        &attempt_store,
        &mine,
        dissent.clone(),
        WorkAttemptStateV1::Succeeded,
        vec![artifact("artifact.patch", '7')],
        digest('8'),
    );
    let failed = source_identity("task.source.failed", "attempt.1");
    insert_terminal_source(
        &attempt_store,
        &mine,
        failed.clone(),
        WorkAttemptStateV1::Failed,
        Vec::new(),
        digest('9'),
    );

    let outcome = admit_work_synthesis(
        &attempts,
        &context,
        synthesis_command(vec![
            agree_a.clone(),
            agree_b.clone(),
            dissent.clone(),
            failed.clone(),
        ]),
    )
    .unwrap();
    let WorkSynthesisAttemptV1::Admitted(admission) = outcome else {
        panic!("expected an admitted synthesis attempt");
    };
    // The synthesis attempt went through the standard admission and holds a
    // lease under the standard fence.
    assert_eq!(admission.attempt.state(), WorkAttemptStateV1::Leased);
    assert_eq!(
        admission.attempt.identity(),
        &source_identity("task.synthesis", "attempt.synthesis")
    );
    // The citation obligation is complete by construction: every citable
    // digest, including the minority evidence, is cited.
    assert_eq!(
        admission.draft.cited_source_digests,
        BTreeSet::from([digest('3'), digest('4'), digest('7')])
    );
    assert_eq!(
        admission.draft.synthesis_attempt,
        source_identity("task.synthesis", "attempt.synthesis")
    );
    // Disagreement is preserved as structure: the concurring pair first,
    // the dissenting minority second, nobody resolved by fiat.
    assert_eq!(admission.groups.len(), 2);
    assert_eq!(admission.groups[0].sources, vec![agree_a, agree_b]);
    assert_eq!(admission.groups[1].sources, vec![dissent]);
    // The failure is preserved uncited rather than dropped.
    assert_eq!(admission.uncited, vec![failed.clone()]);
    assert!(admission.source_set.verified());
    assert_eq!(admission.source_set.sources.len(), 4);
    assert_eq!(
        admission.source_set.sources[3].outcome,
        WorkSynthesisSourceOutcomeV1::Failed {
            evidence: digest('9'),
        }
    );
}

#[test]
fn identical_synthesis_replay_returns_the_byte_stable_admitted_result() {
    let (attempts, attempt_store, work, context) = fixture("project.synthesis.replay");
    let mine = authority("project.synthesis.replay");
    admit_work(&work, &context, "task.synthesis");

    let citable = source_identity("task.source.citable", "attempt.1");
    insert_terminal_source(
        &attempt_store,
        &mine,
        citable.clone(),
        WorkAttemptStateV1::Succeeded,
        vec![artifact("artifact.initial", '1')],
        digest('2'),
    );
    let mutable = source_identity("task.source.mutable", "attempt.1");
    let running = insert_running_source(&attempt_store, &mine, mutable.clone());
    let command = synthesis_command(vec![citable, mutable]);

    let first = admit_work_synthesis(&attempts, &context, command.clone()).unwrap();
    let WorkSynthesisAttemptV1::Admitted(first_admission) = &first else {
        panic!("expected an admitted synthesis attempt");
    };
    assert_eq!(
        first_admission.source_set.sources[1].outcome,
        WorkSynthesisSourceOutcomeV1::Unknown {
            state: WorkAttemptStateV1::Running,
        }
    );
    mark_source_succeeded(
        &attempt_store,
        &mine,
        &running,
        vec![artifact("artifact.late", '3')],
    );
    let replay = admit_work_synthesis(&attempts, &context, command).unwrap();

    assert_eq!(
        serde_json::to_vec(&replay).unwrap(),
        serde_json::to_vec(&first).unwrap()
    );
    assert_eq!(
        attempt_store
            .committed_result_count(&mine, &id("run.task.synthesis"))
            .unwrap(),
        1
    );
}

#[test]
fn changed_synthesis_request_conflicts_without_mutating_the_admitted_result() {
    let (attempts, attempt_store, work, context) = fixture("project.synthesis.conflict");
    let mine = authority("project.synthesis.conflict");
    admit_work(&work, &context, "task.synthesis");

    let source = source_identity("task.source.citable", "attempt.1");
    insert_terminal_source(
        &attempt_store,
        &mine,
        source.clone(),
        WorkAttemptStateV1::Succeeded,
        vec![artifact("artifact.initial", '4')],
        digest('5'),
    );
    let command = synthesis_command(vec![source]);
    let first = admit_work_synthesis(&attempts, &context, command.clone()).unwrap();

    let mut changed = command.clone();
    changed.output_name = id("output.synthesis.changed");
    let conflict = admit_work_synthesis(&attempts, &context, changed).unwrap_err();
    assert_eq!(conflict.kind(), ApplicationProblemKind::Conflict);

    let replay = admit_work_synthesis(&attempts, &context, command).unwrap();
    assert_eq!(
        serde_json::to_vec(&replay).unwrap(),
        serde_json::to_vec(&first).unwrap()
    );
    assert_eq!(
        attempt_store
            .committed_result_count(&mine, &id("run.task.synthesis"))
            .unwrap(),
        1
    );
}

#[test]
fn ordinary_start_conflicts_with_an_existing_synthesis_identity() {
    let (attempts, attempt_store, work, context) = fixture("project.synthesis.cross-mode");
    let mine = authority("project.synthesis.cross-mode");
    admit_work(&work, &context, "task.synthesis");

    let source = source_identity("task.source.citable", "attempt.1");
    insert_terminal_source(
        &attempt_store,
        &mine,
        source.clone(),
        WorkAttemptStateV1::Succeeded,
        vec![artifact("artifact.initial", '6')],
        digest('7'),
    );
    let command = synthesis_command(vec![source]);
    let first = admit_work_synthesis(&attempts, &context, command.clone()).unwrap();

    let conflict = attempts.start(&context, command.start.clone()).unwrap_err();
    assert_eq!(conflict.kind(), ApplicationProblemKind::Conflict);

    let replay = admit_work_synthesis(&attempts, &context, command).unwrap();
    assert_eq!(
        serde_json::to_vec(&replay).unwrap(),
        serde_json::to_vec(&first).unwrap()
    );
    assert_eq!(
        attempt_store
            .committed_result_count(&mine, &id("run.task.synthesis"))
            .unwrap(),
        1
    );
}

#[test]
fn synthesis_source_sets_are_order_and_content_sealed() {
    let first = WorkSynthesisSourceEnvelopeV1 {
        source: source_identity("task.source.a", "attempt.1"),
        outcome: WorkSynthesisSourceOutcomeV1::Succeeded {
            artifacts: vec![digest('3')],
        },
    };
    let second = WorkSynthesisSourceEnvelopeV1 {
        source: source_identity("task.source.b", "attempt.1"),
        outcome: WorkSynthesisSourceOutcomeV1::Failed {
            evidence: digest('4'),
        },
    };
    let forward = WorkSynthesisSourceSetV1::seal(vec![first.clone(), second.clone()]).unwrap();
    let reversed = WorkSynthesisSourceSetV1::seal(vec![second, first]).unwrap();
    // Order is part of the identity of the set.
    assert_ne!(forward.set_digest, reversed.set_digest);
    assert!(forward.verified());
    // Any mutation after sealing is detectable.
    let mut tampered = forward;
    tampered.sources[0].outcome = WorkSynthesisSourceOutcomeV1::Succeeded {
        artifacts: vec![digest('5')],
    };
    assert!(!tampered.verified());
}
