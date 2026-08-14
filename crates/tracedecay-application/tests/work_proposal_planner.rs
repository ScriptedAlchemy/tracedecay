//! End-to-end shape, sizing, decomposition, and route-planning behaviour of the
//! mounted `operation.work.generate_proposal`.
//!
//! Every assertion here runs through `WorkIntelligenceServiceV1::generate_proposal`, not
//! through the policy evaluator directly, so the production path that assembles
//! the authorized snapshot is the thing under test. Routes, budget, content
//! location, prior outcomes, and any human override reach the evaluator only by
//! way of `WorkRoutingSnapshotPortV1`; nothing in this file hands the
//! evaluator a route the authority did not declare.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use tracedecay_application::{
    WorkGraphSelectionCoverageV1,
    AuthorizedWorkProductScopeV1, CancellationContext, CapabilityGrantSnapshot, Deadline,
    DisclosureClass, GenerateProposalRequest, RequestContext, RequestId, ResolvedScope,
    VerifiedWorkGraphVersionV1, WorkGraphReadPortErrorV1, WorkGraphReadPortV1,
    WorkGraphReadRequestV1, WorkGraphReadV1, WorkGraphVersionEntryV1, WorkIntelligenceServiceV1,
    WorkProductBindingV1, WorkProductOwnerAuthorizationErrorV1,
    WorkProductOwnerAuthorizationPortV1, WorkProductPortContextV1, WorkProductSelectionScopeV1,
    WorkRoutingSnapshotErrorV1, WorkRoutingSnapshotPortV1, WorkRoutingSnapshotV1,
};
use tracedecay_domain::{
    ActorId, InitiativeId, ManifestDigest, MilestoneId, ProjectId, ProjectionGenerationId,
    ProposalId, RepositoryId, TaskId, UtcMicros, WorkGraphVersionV1, WorkHierarchyV1,
    WorkInitiativeV1, WorkItemInputV1, WorkItemV1, WorkMilestoneV1, WorkPlanId, WorkPlanV1,
    WorkProductGraphV1, WorkProductProjectionBundleV1, WorkProductSourceWatermarkV1,
    WorkProjectionSequenceV1, WorkRuntimeProjectionCoverageV1, WorkRuntimeProjectionV1, WorktreeId,
};
use tracedecay_policy::{
    WORK_CALIBRATION_SUPPORT_FLOOR, WorkBudgetEnvelopeV1, WorkContentLocationClassV1,
    WorkContentLocationLimitV1, WorkEffortClassV1, WorkOrdinalBandV1, WorkPriorOutcomeV1,
    WorkPriorTerminalV1, WorkProposalReasonV1, WorkRouteCandidateV1, WorkRouteOverrideV1,
    WorkRoutePlanV1,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

/// Task creation time. The local evidence frontier watermark follows the last
/// recorded event, so prior outcomes observed after this are not stale.
const CREATED_AT: UtcMicros = UtcMicros(10);
/// Proposal evaluation time. Prior outcomes observed after this are
/// incomparable and never enter the calibration cohort.
const EVALUATED_AT: UtcMicros = UtcMicros(50);

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
        id::<RepositoryId>("repository.work.fixture"),
        id::<WorktreeId>("worktree.work.fixture"),
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
        id::<ActorId>("actor.work.owner"),
        scope,
        grant,
        RequestId::new(format!("request.{project}")).unwrap(),
        Deadline::new(UtcMicros(9_000)).unwrap(),
        CancellationContext::active(format!("cancel.{project}")).unwrap(),
    )
    .unwrap()
}

/// One exact product graph and the authority's routing state.
#[derive(Clone, Default)]
struct TestStore {
    graph: Arc<Mutex<Option<WorkProductGraphV1>>>,
    routing: Arc<Mutex<WorkRoutingSnapshotV1>>,
}

impl TestStore {
    fn declare(&self, routing: WorkRoutingSnapshotV1) {
        *self.routing.lock().unwrap() = routing;
    }

    fn seed_ready(&self, task_id: TaskId) {
        *self.graph.lock().unwrap() = Some(graph_with_task(task_id));
    }

    fn graph(&self) -> WorkProductGraphV1 {
        self.graph
            .lock()
            .unwrap()
            .clone()
            .expect("fixture graph is seeded")
    }
}

impl WorkProductOwnerAuthorizationPortV1 for TestStore {
    fn authorize_scope(
        &self,
        _context: &RequestContext,
        selection: &WorkProductSelectionScopeV1,
        _observed_at: UtcMicros,
    ) -> Result<AuthorizedWorkProductScopeV1, WorkProductOwnerAuthorizationErrorV1> {
        AuthorizedWorkProductScopeV1::new(
            id("brain.work-proposal.fixture"),
            id("profile.work-proposal.fixture"),
            selection.clone(),
        )
        .map_err(|_| WorkProductOwnerAuthorizationErrorV1::Unavailable)
    }
}

impl WorkGraphReadPortV1 for TestStore {
    fn read_graph(
        &self,
        context: &WorkProductPortContextV1,
        request: &WorkGraphReadRequestV1,
    ) -> Result<WorkGraphReadV1, WorkGraphReadPortErrorV1> {
        let graph = self
            .graph
            .lock()
            .map_err(|_| WorkGraphReadPortErrorV1::Unavailable)?
            .clone()
            .ok_or(WorkGraphReadPortErrorV1::NotFoundOrNotAuthorized)?;
        let source_watermark = WorkProductSourceWatermarkV1::new(BTreeMap::new())
            .map_err(|_| WorkGraphReadPortErrorV1::Unavailable)?;
        let verified = VerifiedWorkGraphVersionV1::new(
            graph.version(),
            tracedecay_domain::WorkProductEventSequenceV1::new(1)
                .map_err(|_| WorkGraphReadPortErrorV1::Unavailable)?,
            source_watermark,
            digest('c'),
        )
        .map_err(|_| WorkGraphReadPortErrorV1::Unavailable)?;
        let runtime = WorkRuntimeProjectionV1::new(
            graph.version(),
            ProjectionGenerationId::new("generation.work-proposal.fixture")
                .map_err(|_| WorkGraphReadPortErrorV1::Unavailable)?,
            WorkProjectionSequenceV1::new(graph.version().get()),
            request.observed_at,
            Vec::new(),
            WorkRuntimeProjectionCoverageV1::Complete,
        )
        .map_err(|_| WorkGraphReadPortErrorV1::Unavailable)?;
        let projections =
            WorkProductProjectionBundleV1::from_graph(&graph, &runtime, request.observed_at)
                .map_err(|_| WorkGraphReadPortErrorV1::Unavailable)?;
        let snapshot = WorkGraphVersionEntryV1::new(
            CREATED_AT,
            request.observed_at,
            request.observed_at,
            verified,
            graph,
            runtime,
            projections,
        )
        .map_err(|_| WorkGraphReadPortErrorV1::Unavailable)?;
        Ok(WorkGraphReadV1::Current {
            authorized_scope: context.authorized_scope().clone(),
            selection_coverage: WorkGraphSelectionCoverageV1::Complete { covered_events: 1 },
            snapshot,
        })
    }
}

impl WorkRoutingSnapshotPortV1 for TestStore {
    fn routing_snapshot(
        &self,
        _context: &RequestContext,
        _task_id: &TaskId,
    ) -> Result<WorkRoutingSnapshotV1, WorkRoutingSnapshotErrorV1> {
        Ok(self.routing.lock().unwrap().clone())
    }
}

/// One ready, dependency-free task, so the planner path is reached without a
/// gate short-circuit standing in front of it.
fn ready_task(store: &TestStore, task: &str) -> TaskId {
    let task_id = id::<TaskId>(task);
    store.seed_ready(task_id.clone());
    task_id
}

fn proposal_service(store: &TestStore) -> WorkIntelligenceServiceV1<TestStore, TestStore> {
    WorkIntelligenceServiceV1::new(
        store.clone(),
        store.clone(),
        WorkProductBindingV1::new(
            CapabilityId::new("capability.work.fixture").unwrap(),
            UseCaseId::new("use-case.work.fixture").unwrap(),
        ),
    )
}

fn proposal_request(task_id: &TaskId, proposal: &str) -> GenerateProposalRequest {
    GenerateProposalRequest {
        selection: WorkProductSelectionScopeV1::ProfileOwnedNoGit,
        task_id: task_id.clone(),
        proposal_id: id::<ProposalId>(proposal),
        live_git_evidence: None,
        occurred_at: EVALUATED_AT,
    }
}

fn graph_with_task(task_id: TaskId) -> WorkProductGraphV1 {
    let initiative_id = id::<InitiativeId>("initiative.work-proposal.fixture");
    let plan_id = id::<WorkPlanId>("plan.work-proposal.fixture");
    let milestone_id = id::<MilestoneId>("milestone.work-proposal.fixture");
    WorkProductGraphV1::new(
        WorkGraphVersionV1::initial(),
        vec![
            WorkInitiativeV1::new(
                initiative_id.clone(),
                "Proposal fixture initiative".to_owned(),
                UtcMicros(1),
            )
            .unwrap(),
        ],
        vec![
            WorkPlanV1::new(
                plan_id.clone(),
                initiative_id.clone(),
                "Proposal fixture plan".to_owned(),
                UtcMicros(2),
            )
            .unwrap(),
        ],
        vec![
            WorkMilestoneV1::new(
                milestone_id.clone(),
                plan_id.clone(),
                "Proposal fixture milestone".to_owned(),
                UtcMicros(3),
            )
            .unwrap(),
        ],
        vec![
            WorkItemV1::new(WorkItemInputV1 {
                task_id,
                hierarchy: WorkHierarchyV1::new(initiative_id, plan_id, milestone_id),
                title: "Proposal fixture task".to_owned(),
                dependencies: BTreeSet::new(),
                informational_relations: BTreeSet::new(),
                causal_candidates: BTreeSet::new(),
                acceptance_criteria: Vec::new(),
                effort: 1,
                scheduled_at: None,
                deadline: None,
                created_at: CREATED_AT,
                updated_at: CREATED_AT,
            })
            .unwrap(),
        ],
    )
    .unwrap()
}

/// A candidate that clears the declared budget and sits in an allowed content
/// location; only `correctness` varies, so the lexicographic order is readable.
fn route(route_id: &str, correctness: WorkOrdinalBandV1) -> WorkRouteCandidateV1 {
    WorkRouteCandidateV1 {
        route_id: route_id.to_owned(),
        provider_capability_id: format!("capability.provider.{route_id}"),
        model_id: format!("model.{route_id}"),
        effort: WorkEffortClassV1::Standard,
        declared_budget_ceiling: 1_000,
        content_location: WorkContentLocationClassV1::Local,
        correctness,
        sensitive_data_fitness: WorkOrdinalBandV1::Moderate,
        latency: WorkOrdinalBandV1::Moderate,
        cost: WorkOrdinalBandV1::Moderate,
        autonomy: WorkOrdinalBandV1::Moderate,
        evidence_quality: WorkOrdinalBandV1::Moderate,
    }
}

fn budget() -> WorkBudgetEnvelopeV1 {
    WorkBudgetEnvelopeV1 {
        ceiling: 10_000,
        spent: 1_000,
    }
}

fn local_and_tenant() -> WorkContentLocationLimitV1 {
    WorkContentLocationLimitV1 {
        allowed: vec![
            WorkContentLocationClassV1::Local,
            WorkContentLocationClassV1::Tenant,
        ],
    }
}

fn outcome(route_id: &str, observed_at: i64) -> WorkPriorOutcomeV1 {
    WorkPriorOutcomeV1 {
        route_id: route_id.to_owned(),
        accepted: true,
        rework: false,
        escaped_defect: false,
        terminal: WorkPriorTerminalV1::Succeeded,
        observed_at: UtcMicros(observed_at),
    }
}

fn ranked_ids(plan: &WorkRoutePlanV1) -> Vec<&str> {
    plan.ranked
        .iter()
        .map(|entry| entry.route_id.as_str())
        .collect()
}

fn three_routes() -> Vec<WorkRouteCandidateV1> {
    vec![
        route("route.gamma", WorkOrdinalBandV1::Moderate),
        route("route.alpha", WorkOrdinalBandV1::Highest),
        route("route.beta", WorkOrdinalBandV1::High),
    ]
}

#[test]
fn eligible_routes_from_the_authorized_snapshot_are_ranked_deterministically() {
    let store = TestStore::default();
    let service = proposal_service(&store);
    let context = context("project.work.planner.rank");
    let task_id = ready_task(&store, "task.work.rank");
    store.declare(WorkRoutingSnapshotV1 {
        configuration_revision: None,
        eligible_routes: three_routes(),
        budget: Some(budget()),
        content_location: Some(local_and_tenant()),
        prior_outcomes: Vec::new(),
        human_override: None,
    });

    let request = proposal_request(&task_id, "proposal.work.rank");
    let proposal = service
        .generate_proposal(&context, digest('f'), &store, request.clone())
        .unwrap();
    let plan = proposal
        .decision
        .route_plan
        .as_ref()
        .expect("a snapshot with eligible routes produces a route plan");

    // Correctness descending is the first ordinal dimension, so the best
    // correctness band leads regardless of the order storage returned.
    assert_eq!(
        ranked_ids(plan),
        vec!["route.alpha", "route.beta", "route.gamma"]
    );
    assert!(plan.exclusions.is_empty());
    assert!(!plan.human_override_applied);
    assert!(
        plan.ranked
            .windows(2)
            .all(|pair| pair[0].rank < pair[1].rank),
        "ranks are strictly ascending"
    );
    // Ranking keeps the dimensions separate; the ranked entry repeats the bands
    // the authority declared rather than collapsing them into a score.
    let leader = &plan.ranked[0];
    assert_eq!(leader.correctness, WorkOrdinalBandV1::Highest);
    assert_eq!(leader.sensitive_data_fitness, WorkOrdinalBandV1::Moderate);
    assert_eq!(leader.evidence_quality, WorkOrdinalBandV1::Moderate);

    // Identical authorized inputs produce a byte-identical decision, so the
    // proposal digest that binds acceptance is stable across replay.
    let replayed = service
        .generate_proposal(&context, digest('f'), &store, request)
        .unwrap();
    assert_eq!(proposal, replayed);
}

#[test]
fn budget_and_content_location_refusals_are_recorded_as_typed_exclusions() {
    let store = TestStore::default();
    let service = proposal_service(&store);
    let context = context("project.work.planner.exclude");
    let task_id = ready_task(&store, "task.work.exclude");

    let mut over_budget = route("route.expensive", WorkOrdinalBandV1::Highest);
    // Remaining budget is ceiling minus spent; this ceiling cannot fit inside it.
    over_budget.declared_budget_ceiling = 50_000;
    let mut offshore = route("route.external", WorkOrdinalBandV1::Highest);
    offshore.content_location = WorkContentLocationClassV1::External;
    store.declare(WorkRoutingSnapshotV1 {
        configuration_revision: None,
        eligible_routes: vec![
            over_budget,
            offshore,
            route("route.allowed", WorkOrdinalBandV1::Low),
        ],
        budget: Some(budget()),
        content_location: Some(local_and_tenant()),
        prior_outcomes: Vec::new(),
        human_override: None,
    });

    let proposal = service
        .generate_proposal(
            &context,
            digest('f'),
            &store,
            proposal_request(&task_id, "proposal.work.exclude"),
        )
        .unwrap();
    let plan = proposal
        .decision
        .route_plan
        .as_ref()
        .expect("a snapshot with eligible routes produces a route plan");

    // An excluded route never ranks, however strong its correctness band is.
    assert_eq!(ranked_ids(plan), vec!["route.allowed"]);
    let refused: BTreeMap<&str, WorkProposalReasonV1> = plan
        .exclusions
        .iter()
        .map(|exclusion| (exclusion.route_id.as_str(), exclusion.reason))
        .collect();
    assert_eq!(
        refused.get("route.expensive"),
        Some(&WorkProposalReasonV1::RouteBudgetExceeded)
    );
    assert_eq!(
        refused.get("route.external"),
        Some(&WorkProposalReasonV1::RouteContentLocationRefused)
    );
}

#[test]
fn a_human_override_promotes_a_surviving_route_and_is_recorded() {
    let store = TestStore::default();
    let service = proposal_service(&store);
    let context = context("project.work.planner.override");
    let task_id = ready_task(&store, "task.work.override");
    store.declare(WorkRoutingSnapshotV1 {
        configuration_revision: None,
        eligible_routes: three_routes(),
        budget: Some(budget()),
        content_location: Some(local_and_tenant()),
        prior_outcomes: Vec::new(),
        human_override: Some(WorkRouteOverrideV1 {
            route_id: "route.gamma".to_owned(),
            recorded_at: UtcMicros(20),
        }),
    });

    let proposal = service
        .generate_proposal(
            &context,
            digest('f'),
            &store,
            proposal_request(&task_id, "proposal.work.override"),
        )
        .unwrap();
    let plan = proposal
        .decision
        .route_plan
        .as_ref()
        .expect("a snapshot with eligible routes produces a route plan");

    // The named route leads and the remaining routes keep their relative order.
    assert_eq!(
        ranked_ids(plan),
        vec!["route.gamma", "route.alpha", "route.beta"]
    );
    assert!(plan.human_override_applied);
    assert!(
        proposal
            .decision
            .ordered_reason_codes
            .contains(&WorkProposalReasonV1::HumanOverrideApplied)
    );
}

#[test]
fn no_eligible_routes_is_a_typed_decision_and_not_a_failure() {
    let store = TestStore::default();
    let service = proposal_service(&store);
    let context = context("project.work.planner.empty");
    let task_id = ready_task(&store, "task.work.empty");
    // The authority holds no routing state at all. Generation must still
    // succeed and answer honestly instead of inventing a default route.
    store.declare(WorkRoutingSnapshotV1::default());

    let proposal = service
        .generate_proposal(
            &context,
            digest('f'),
            &store,
            proposal_request(&task_id, "proposal.work.empty"),
        )
        .expect("an empty route set is a decision, not an error");
    let plan = proposal
        .decision
        .route_plan
        .as_ref()
        .expect("an empty route set still produces an explained route plan");

    assert!(plan.ranked.is_empty());
    assert_eq!(plan.deterministic_baseline, None);
    assert_eq!(plan.uncertainty, WorkOrdinalBandV1::Highest);
    assert!(!plan.human_override_applied);
    assert!(
        proposal
            .decision
            .ordered_reason_codes
            .contains(&WorkProposalReasonV1::NoEligibleRoutes)
    );
}

#[test]
fn sizing_is_withheld_below_the_declared_calibration_support_floor() {
    let store = TestStore::default();
    let service = proposal_service(&store);
    let context = context("project.work.planner.sparse");
    let task_id = ready_task(&store, "task.work.sparse");
    let in_cohort: Vec<WorkPriorOutcomeV1> = (11..14)
        .map(|observed_at| outcome("route.alpha", observed_at))
        .collect();
    assert!(u32::try_from(in_cohort.len()).unwrap() < WORK_CALIBRATION_SUPPORT_FLOOR);
    store.declare(WorkRoutingSnapshotV1 {
        configuration_revision: None,
        eligible_routes: three_routes(),
        budget: Some(budget()),
        content_location: Some(local_and_tenant()),
        prior_outcomes: in_cohort,
        human_override: None,
    });

    let proposal = service
        .generate_proposal(
            &context,
            digest('f'),
            &store,
            proposal_request(&task_id, "proposal.work.sparse"),
        )
        .unwrap();

    // Thin evidence widens uncertainty; it never produces a point estimate.
    assert_eq!(proposal.decision.sizing, None);
    assert!(
        proposal
            .decision
            .ordered_reason_codes
            .contains(&WorkProposalReasonV1::InsufficientCalibrationSupport)
    );
}

#[test]
fn sizing_at_the_support_floor_carries_the_floor_that_governed_it() {
    let store = TestStore::default();
    let service = proposal_service(&store);
    let context = context("project.work.planner.calibrated");
    let task_id = ready_task(&store, "task.work.calibrated");
    let support = WORK_CALIBRATION_SUPPORT_FLOOR;
    let first_observed = CREATED_AT.0 + 1;
    let in_cohort: Vec<WorkPriorOutcomeV1> = (0..i64::from(support))
        .map(|offset| outcome("route.alpha", first_observed + offset))
        .chain(std::iter::once(outcome("route.beta", first_observed)))
        .collect();
    store.declare(WorkRoutingSnapshotV1 {
        configuration_revision: None,
        eligible_routes: three_routes(),
        budget: Some(budget()),
        content_location: Some(local_and_tenant()),
        prior_outcomes: in_cohort,
        human_override: None,
    });

    let proposal = service
        .generate_proposal(
            &context,
            digest('f'),
            &store,
            proposal_request(&task_id, "proposal.work.calibrated"),
        )
        .unwrap();
    let sizing = proposal
        .decision
        .sizing
        .as_ref()
        .expect("support at the declared floor admits calibrated sizing");

    // The cohort is the top-ranked route only; the out-of-cohort outcome for
    // route.beta never inflates the denominator.
    assert_eq!(sizing.cohort, "route.alpha");
    assert_eq!(sizing.support, support);
    // The governing floor travels in the record so replay shows which floor
    // admitted this sizing.
    assert_eq!(sizing.support_floor, WORK_CALIBRATION_SUPPORT_FLOOR);
    assert_eq!(
        sizing.horizon,
        UtcMicros(first_observed + i64::from(support) - 1)
    );
    assert!(
        !proposal
            .decision
            .ordered_reason_codes
            .contains(&WorkProposalReasonV1::InsufficientCalibrationSupport)
    );
}

#[test]
fn planning_a_proposal_mutates_no_work_state() {
    let store = TestStore::default();
    let service = proposal_service(&store);
    let context = context("project.work.planner.readonly");
    let task_id = ready_task(&store, "task.work.readonly");
    store.declare(WorkRoutingSnapshotV1 {
        configuration_revision: None,
        eligible_routes: three_routes(),
        budget: Some(budget()),
        content_location: Some(local_and_tenant()),
        prior_outcomes: vec![outcome("route.alpha", 11)],
        human_override: None,
    });
    let before = store.graph();

    let proposal = service
        .generate_proposal(
            &context,
            digest('f'),
            &store,
            proposal_request(&task_id, "proposal.work.readonly"),
        )
        .unwrap();
    assert!(proposal.decision.route_plan.is_some());

    let after = store.graph();
    assert_eq!(after.version(), WorkGraphVersionV1::initial());
    assert_eq!(after, before);
    assert_eq!(proposal.proposal.based_on_version(), before.version());
}
