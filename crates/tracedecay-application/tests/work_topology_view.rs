//! Execution-topology view contract: lanes join real placement rows to the
//! attempt page, the view pins the verified topology generation, absence of
//! Work is a typed state, and an invalid resolved policy is a typed
//! unavailability rather than a fabricated view.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use tracedecay_application::{
    AdmitWorkPlacementCommand, ApplicationProblem, CancellationContext, CapabilityGrantSnapshot,
    Deadline, DisclosureClass, ExecutionTopologyViewV1, GenerateProposalRequest, RequestContext,
    RequestId, ResolvedScope, StartWorkAttemptCommand, WorkAttemptListCoverageV1,
    WorkAttemptService, WorkAttemptTopologyBindingV1, WorkAttemptTopologyStateV1,
    WorkIntelligenceServiceV1, WorkPlacementReadingV1, WorkPlacementService,
    WorkPlacementStorageError, WorkPlacementStoragePort, WorkProductAttemptServiceV1,
    WorkProductSelectionScopeV1, WorkRelationScopeV1, WorkRoutingSnapshotErrorV1,
    WorkRoutingSnapshotPortV1, WorkRoutingSnapshotV1, WorkTopologyViewRequestV1,
    execution_topology_view,
};
use tracedecay_domain::configuration::safe_work_topology_policy_v1;
use tracedecay_domain::{
    ActorId, CommitId, ConfigurationRevisionId, ConfigurationSnapshotId, ManifestDigest, ProjectId,
    ProviderId, RefId, RepositoryId, TaskId, UtcMicros, WorkApprovalPolicy, WorkAuthority,
    WorkEffectStateV1, WorkEgressPolicy, WorkExecutableReference, WorkExecutionLimits,
    WorkExecutionSnapshot, WorkExecutionSnapshotInput, WorkFallbackTopology, WorkFilesystemPolicy,
    WorkPlacementIdentityV1, WorkPlacementKindV1, WorkPlacementObservationV1, WorkPlacementStateV1,
    WorkPlacementTargetV1, WorkPlacementV1, WorkProviderBackendV1, WorkProviderProtocol,
    WorkProviderRouteId, WorkProviderRouteV1, WorkSandboxPolicy, WorkflowOperationRef, WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

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

fn selected_product_scope(context: &RequestContext) -> WorkProductSelectionScopeV1 {
    WorkProductSelectionScopeV1::relations(BTreeSet::from([WorkRelationScopeV1::Repository {
        project_id: context.scope().project_id.clone(),
        repository_id: context.scope().repository_id.clone(),
    }]))
    .unwrap()
}

fn admit_work(
    store: &common::WorkProductAttemptStore,
    proposals: &WorkIntelligenceServiceV1<
        common::WorkProductAttemptStore,
        common::WorkProductAttemptStore,
    >,
    context: &RequestContext,
    task: &str,
) {
    let task_id = id::<TaskId>(task);
    // The shared authority seeds a real immutable Work-product event journal
    // whose accepted proposal and execution admission are required by the
    // public product-attempt service below.
    store.seed_task(context, task_id.clone(), true);
    let proposal = proposals
        .generate_proposal(
            context,
            digest('b'),
            &EMPTY_PROPOSAL_ROUTING,
            GenerateProposalRequest {
                selection: selected_product_scope(context),
                task_id: task_id.clone(),
                proposal_id: id(&format!("proposal.{task}")),
                live_git_evidence: None,
                occurred_at: UtcMicros(15),
            },
        )
        .unwrap();
    assert_eq!(proposal.proposal.task_id(), &task_id);
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
    WorkAttemptService<common::WorkProductAttemptStore>,
    WorkProductAttemptServiceV1<common::WorkProductAttemptStore>,
    WorkIntelligenceServiceV1<common::WorkProductAttemptStore, common::WorkProductAttemptStore>,
    common::WorkProductAttemptStore,
    WorkPlacementService<PlacementStore>,
    RequestContext,
);

fn fixture(project: &str) -> Fixture {
    let store = common::WorkProductAttemptStore::default();
    let attempts = WorkAttemptService::new(store.clone());
    let product_attempts = WorkProductAttemptServiceV1::new(store.clone());
    let proposals = WorkIntelligenceServiceV1::new(
        store.clone(),
        store.clone(),
        common::work_product_binding(),
    );
    (
        attempts,
        product_attempts,
        proposals,
        store,
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
    let (attempts, product_attempts, proposals, store, placements, context) =
        fixture("project.topology.view");
    for task in ["task.topology.a", "task.topology.b"] {
        admit_work(&store, &proposals, &context, task);
        product_attempts
            .start_against_registered_topology(
                &context,
                &common::work_product_binding(),
                &common::work_product_revisions(&context),
                &safe_work_topology_policy_v1(),
                start_command(task, &format!("attempt.{task}.1")),
            )
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
    let (attempts, _product_attempts, _proposals, _store, placements, context) =
        fixture("project.topology.absent");
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
    let (attempts, _product_attempts, _proposals, _store, placements, context) =
        fixture("project.topology.invalid");
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
