//! Execution-topology view contract: lanes join real placement rows to the
//! attempt page, the view pins the verified topology generation, absence of
//! Work is a typed state, and an invalid resolved policy is a typed
//! unavailability rather than a fabricated view.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use tracedecay_application::{
    AcceptProposalCommand, AdmitExecutionCommand, AdmitWorkPlacementCommand, ApplicationProblem,
    CancellationContext, CapabilityGrantSnapshot, CreateWorkCommand, Deadline, DisclosureClass,
    ExecutionTopologyViewV1, GenerateProposalRequest, RequestContext, RequestId, ResolvedScope,
    ReviewProposalCommand, StartWorkAttemptCommand, WorkAppendOutcome, WorkAppendRequest,
    WorkAttemptAdmissionKind, WorkAttemptEvidenceRecordV1, WorkAttemptInsertOutcome,
    WorkAttemptListCoverageV1, WorkAttemptListPageV1, WorkAttemptService, WorkAttemptStorageError,
    WorkAttemptStoragePort, WorkAttemptTopologyBindingV1, WorkAttemptTopologyStateV1,
    WorkPlacementReadingV1, WorkPlacementService, WorkPlacementStorageError,
    WorkPlacementStoragePort, WorkProjectionPortError, WorkProjectionReadPort, WorkService,
    WorkStorageError, WorkStoragePort, WorkTopologyViewRequestV1, execution_topology_view,
};
use tracedecay_domain::configuration::safe_work_topology_policy_v1;
use tracedecay_domain::{
    ActorId, CommitId, ConfigurationRevisionId, ConfigurationSnapshotId, ManifestDigest, ProjectId,
    ProjectionGenerationId, ProviderId, RefId, RepositoryId, TaskId, UtcMicros, WorkApprovalPolicy,
    WorkAttemptIdentityV1, WorkAttemptStateV1, WorkAttemptV1, WorkAuthority, WorkEffectStateV1,
    WorkEgressPolicy, WorkEvent, WorkExecutableReference, WorkExecutionLimits,
    WorkExecutionSnapshot, WorkExecutionSnapshotInput, WorkFallbackTopology, WorkFilesystemPolicy,
    WorkLeaseFenceV1, WorkPlacementIdentityV1, WorkPlacementKindV1, WorkPlacementObservationV1,
    WorkPlacementStateV1, WorkPlacementTargetV1, WorkPlacementV1, WorkProjection,
    WorkProjectionCoverageV1, WorkProjectionSequenceV1, WorkProjectionSnapshotV1,
    WorkProviderBackendV1, WorkProviderProtocol, WorkProviderRouteId, WorkProviderRouteV1,
    WorkSandboxPolicy, WorkVersion, WorkflowOperationRef, WorktreeId,
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

fn context(project: &str) -> RequestContext {
    let scope = ResolvedScope::new(
        id::<ProjectId>(project),
        id::<RepositoryId>("repository.topology.fixture"),
        id::<WorktreeId>("worktree.topology.fixture"),
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
        id::<ActorId>("actor.topology.viewer"),
        scope,
        grant,
        RequestId::new(format!("request.{project}.topology")).unwrap(),
        Deadline::new(UtcMicros(9_000)).unwrap(),
        CancellationContext::active(format!("cancel.{project}.topology")).unwrap(),
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
            id::<ProjectionGenerationId>("generation.topology.fixture"),
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
}

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

    fn insert_bounded(
        &self,
        authority: &WorkAuthority,
        attempt: &WorkAttemptV1,
        _maximum_active_per_repository: std::num::NonZeroU16,
        _maximum_parallel_per_task: std::num::NonZeroU16,
    ) -> Result<WorkAttemptInsertOutcome, WorkAttemptStorageError> {
        self.insert(authority, attempt)
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

    fn load_admission_kind(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<WorkAttemptAdmissionKind, WorkAttemptStorageError> {
        self.load(authority, identity)
            .map(|_| WorkAttemptAdmissionKind::Ordinary)
    }

    fn update(
        &self,
        authority: &WorkAuthority,
        expected_fence: &WorkLeaseFenceV1,
        expected_state: WorkAttemptStateV1,
        next: &WorkAttemptV1,
        _evidence: Option<&WorkAttemptEvidenceRecordV1>,
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

type PlacementKey = (WorkAuthority, WorkPlacementIdentityV1);

#[derive(Clone, Default)]
struct PlacementStore {
    placements: Arc<Mutex<BTreeMap<PlacementKey, WorkPlacementV1>>>,
}

impl WorkPlacementStoragePort for PlacementStore {
    fn load_placement(
        &self,
        authority: &WorkAuthority,
        identity: &WorkPlacementIdentityV1,
    ) -> Result<Option<WorkPlacementV1>, WorkPlacementStorageError> {
        Ok(self
            .placements
            .lock()
            .unwrap()
            .get(&(authority.clone(), identity.clone()))
            .cloned())
    }

    fn target_holder(
        &self,
        authority: &WorkAuthority,
        root: &str,
    ) -> Result<Option<WorkPlacementIdentityV1>, WorkPlacementStorageError> {
        Ok(self
            .placements
            .lock()
            .unwrap()
            .iter()
            .find(|((stored_authority, _), placement)| {
                stored_authority == authority
                    && placement.holds_target()
                    && placement.target().root() == Some(root)
            })
            .map(|((_, identity), _)| identity.clone()))
    }

    fn publish_placement(
        &self,
        authority: &WorkAuthority,
        expected: Option<u64>,
        next: &WorkPlacementV1,
    ) -> Result<(), WorkPlacementStorageError> {
        let mut placements = self.placements.lock().unwrap();
        let key = (authority.clone(), next.identity().clone());
        let current = placements.get(&key).map(WorkPlacementV1::authority_version);
        if current != expected {
            return Err(WorkPlacementStorageError::AuthorityConflict);
        }
        placements.insert(key, next.clone());
        Ok(())
    }
}

fn requested_route() -> WorkProviderRouteV1 {
    WorkProviderRouteV1::new(
        id::<ProviderId>("provider.work.claude-code-cli"),
        id::<WorkProviderRouteId>("route.topology.claude-code.v1"),
    )
    .unwrap()
}

fn execution_snapshot() -> WorkExecutionSnapshot {
    WorkExecutionSnapshot::new(WorkExecutionSnapshotInput {
        configuration_revision_id: id::<ConfigurationRevisionId>("configuration-revision.top.1"),
        configuration_snapshot_id: id::<ConfigurationSnapshotId>("configuration-snapshot.top.1"),
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
        worktree_root: "/tmp/topology-fixture".to_owned(),
        reference: Some(id::<RefId>("refs/heads/topology-fixture")),
        commit: id::<CommitId>("0123456789abcdef0123456789abcdef01234567"),
        instructions: "Execute the admitted provider step.".to_owned(),
        effect_state: WorkEffectStateV1::Observational,
        occurred_at: UtcMicros(40),
    }
}

type Fixture = (
    WorkAttemptService<AttemptStore, SnapshotPort, TestStore>,
    WorkService<TestStore>,
    WorkPlacementService<PlacementStore>,
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
    (
        attempts,
        work,
        WorkPlacementService::new(PlacementStore::default()),
        context(project),
    )
}

fn verified_binding()
-> impl FnOnce(&WorkAuthority) -> Result<WorkAttemptTopologyStateV1, ApplicationProblem> {
    |_authority| {
        Ok(WorkAttemptTopologyStateV1::Verified(
            WorkAttemptTopologyBindingV1 {
                generation: "generation.topology.pinned".to_owned(),
                task_count: 2,
            },
        ))
    }
}

#[test]
fn view_joins_placement_lanes_to_the_page_and_carries_the_policy_dimensions() {
    let (attempts, work, placements, context) = fixture("project.topology.view");
    for task in ["task.topology.a", "task.topology.b"] {
        admit_work(&work, &context, task);
        attempts
            .start(&context, start_command(task, &format!("attempt.{task}.1")))
            .unwrap();
    }
    let placed = placements
        .admit_placement(
            &context,
            AdmitWorkPlacementCommand {
                task_id: id::<TaskId>("task.topology.a"),
                run_id: id("run.task.topology.a"),
                target: WorkPlacementTargetV1::new(
                    WorkPlacementKindV1::LinkedWorktree,
                    Some("/workspace/topology-lane-a".to_owned()),
                    false,
                    true,
                )
                .unwrap(),
                retention_eligible_at: None,
                occurred_at: UtcMicros(50),
            },
            |_target| {
                Ok(WorkPlacementObservationV1 {
                    dirty_tracked_paths: 0,
                    untracked_paths: 0,
                    unique_commits: Some(0),
                    readable: true,
                    active_holder: false,
                    network_required: false,
                    observed_at: UtcMicros(50),
                })
            },
        )
        .unwrap();
    assert_eq!(placed.state(), WorkPlacementStateV1::Admitted);

    let policy = safe_work_topology_policy_v1();
    let view = execution_topology_view(
        &attempts,
        &placements,
        &policy,
        &context,
        &WorkTopologyViewRequestV1 {
            page_size: 10,
            cursor: None,
        },
        verified_binding(),
    )
    .unwrap();
    let ExecutionTopologyViewV1::View {
        topology,
        coverage,
        execution_placement,
        branch_topology,
        review_topology,
        integration_strategy,
    } = view
    else {
        panic!("two admitted attempts must produce a topology view");
    };
    assert_eq!(topology.generation, "generation.topology.pinned");
    assert_eq!(
        coverage,
        WorkAttemptListCoverageV1::Complete { returned: 2 }
    );
    assert_eq!(execution_placement.mode, policy.placement);
    assert_eq!(execution_placement.lanes.len(), 2);
    let lane_a = &execution_placement.lanes[0];
    assert_eq!(lane_a.task_id.as_str(), "task.topology.a");
    assert_eq!(lane_a.attempt_count, 1);
    let WorkPlacementReadingV1::Placed { placement } = &lane_a.placement else {
        panic!("the admitted placement must appear on its lane");
    };
    assert_eq!(placement.state(), WorkPlacementStateV1::Admitted);
    let lane_b = &execution_placement.lanes[1];
    assert_eq!(lane_b.task_id.as_str(), "task.topology.b");
    assert_eq!(lane_b.placement, WorkPlacementReadingV1::Absent);
    assert_eq!(branch_topology, policy.branch_topology);
    assert_eq!(review_topology, policy.review_topology);
    assert_eq!(integration_strategy.cross_merge, policy.cross_merge);
    assert_eq!(integration_strategy.gates, policy.gates);
    assert_eq!(integration_strategy.protected_refs, policy.protected_refs);
}

#[test]
fn a_scope_without_any_work_is_the_typed_absent_view() {
    let (attempts, _work, placements, context) = fixture("project.topology.absent");
    let view = execution_topology_view(
        &attempts,
        &placements,
        &safe_work_topology_policy_v1(),
        &context,
        &WorkTopologyViewRequestV1 {
            page_size: 10,
            cursor: None,
        },
        |_authority| Ok(WorkAttemptTopologyStateV1::Absent),
    )
    .unwrap();
    assert_eq!(view, ExecutionTopologyViewV1::Absent);
}

#[test]
fn an_invalid_resolved_policy_is_refused_before_any_read() {
    let (attempts, _work, placements, context) = fixture("project.topology.invalid");
    let mut policy = safe_work_topology_policy_v1();
    policy.schema_version = 99;
    let problem = execution_topology_view(
        &attempts,
        &placements,
        &policy,
        &context,
        &WorkTopologyViewRequestV1 {
            page_size: 10,
            cursor: None,
        },
        |_authority| panic!("an invalid policy must refuse before the topology read"),
    )
    .unwrap_err();
    assert!(matches!(problem, ApplicationProblem::Unavailable { .. }));
}
