//! Registered Work graph setup shared by authority and scaling tests.

use std::collections::BTreeSet;

use tracedecay_contracts::{
    CancellationContext, CapabilityGrantSnapshot, CreateWorkProductRequestV1, Deadline,
    DisclosureClass, RequestContext, RequestId, ResolvedScope, WorkGraphReadRequestV1,
    WorkGraphReadV1, WorkProductApplicationErrorV1, WorkProductAuthorizedRelationScopeV1,
    WorkProductBindingV1, WorkProductExpectedAuthorityV1, WorkProductMutationIdentityV1,
    WorkProductMutationServiceV1, WorkProductReadServiceV1, WorkProductRevisionPinsV1,
    WorkProductSelectionScopeV1,
};
use tracedecay_domain::{
    AcceptanceCriterionId, ActorId, CatalogGenerationId, ConfigurationRevisionId, InitiativeId,
    MilestoneId, PolicyRevisionId, ProjectId, RepositoryId, TaskId, UtcMicros,
    WorkAcceptanceCriterionV1, WorkCommandId, WorkGraphVersionV1, WorkHierarchyV1,
    WorkInitiativeV1, WorkItemInputV1, WorkItemV1, WorkMilestoneV1, WorkPlanId, WorkPlanV1,
    WorkProductGraphV1, WorktreeId,
};
use tracedecay_rusqlite_runtime::work::WorkSqliteStorage;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use crate::work_registered_store::RegisteredWorkStore;

pub const PROJECT: &str = "project.work-product.fixture";
pub const REPOSITORY: &str = "repository.work-product.fixture";
/// Every read projects at this instant, which is after every event's
/// `occurred_at`, so a projection is never asked to describe its own future.
pub const PROJECTED_AT: UtcMicros = UtcMicros(400);

use tracedecay_domain::test_fixtures::id;

use tracedecay_domain::test_fixtures::digest;

pub fn binding() -> WorkProductBindingV1 {
    WorkProductBindingV1::new(
        CapabilityId::new("capability.work.graph.read").unwrap(),
        UseCaseId::new("use-case.work.graph.read").unwrap(),
    )
}

pub fn repository_selection() -> WorkProductSelectionScopeV1 {
    WorkProductSelectionScopeV1::relations(BTreeSet::from([
        WorkProductAuthorizedRelationScopeV1::Repository {
            project_id: id(PROJECT),
            repository_id: id(REPOSITORY),
        },
    ]))
    .unwrap()
}

pub fn context() -> RequestContext {
    let scope = ResolvedScope::new(
        id::<ProjectId>(PROJECT),
        id::<RepositoryId>(REPOSITORY),
        id::<WorktreeId>("worktree.work-product.fixture"),
        None,
    )
    .unwrap();
    let capability = CapabilityId::new("capability.work.graph.read").unwrap();
    let use_case = UseCaseId::new("use-case.work.graph.read").unwrap();
    let grant = CapabilityGrantSnapshot::new(
        id("grant.work-product.fixture"),
        1,
        digest('a'),
        id::<ActorId>("actor.work-product.issuer"),
        UtcMicros(-1_000),
        UtcMicros(10_000),
        scope.clone(),
        BTreeSet::from([capability]),
        BTreeSet::from([use_case]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    RequestContext::new(
        id::<ActorId>("actor.work-product.requester"),
        scope,
        grant,
        RequestId::new("request.work-product.fixture").unwrap(),
        Deadline::new(UtcMicros(9_000)).unwrap(),
        CancellationContext::active("cancel.work-product.fixture").unwrap(),
    )
    .unwrap()
}

pub fn mutation(command: &str, occurred_at: UtcMicros) -> WorkProductMutationIdentityV1 {
    WorkProductMutationIdentityV1 {
        expected_authority: WorkProductExpectedAuthorityV1::NoPriorGraph,
        command_id: id::<WorkCommandId>(command),
        causation_event_id: None,
        evidence: Vec::new(),
        occurred_at,
        revisions: WorkProductRevisionPinsV1 {
            policy_revision_id: id::<PolicyRevisionId>("policy.work-product.fixture"),
            configuration_revision_id: id::<ConfigurationRevisionId>("config.work-product.fixture"),
            catalog_generation_id: id::<CatalogGenerationId>("catalog.work-product.fixture"),
        },
    }
}

fn hierarchy() -> WorkHierarchyV1 {
    WorkHierarchyV1::new(
        id::<InitiativeId>("initiative.work-product"),
        id::<WorkPlanId>("plan.work-product"),
        id::<MilestoneId>("milestone.work-product"),
    )
}

/// One declared work item. `effort` is a number the CALLER states; nothing in
/// the authority may compute, default, or infer it.
pub fn item(task: &str, dependencies: &[&str], effort: u32) -> WorkItemV1 {
    WorkItemV1::new(WorkItemInputV1 {
        task_id: id::<TaskId>(task),
        hierarchy: hierarchy(),
        title: format!("Deliver {task}"),
        dependencies: dependencies
            .iter()
            .map(|value| id::<TaskId>(value))
            .collect(),
        informational_relations: BTreeSet::new(),
        causal_candidates: BTreeSet::new(),
        acceptance_criteria: vec![
            WorkAcceptanceCriterionV1::new(
                id::<AcceptanceCriterionId>(&format!("criterion.{task}")),
                format!("{task} has reviewed evidence"),
                true,
            )
            .unwrap(),
        ],
        effort,
        scheduled_at: None,
        deadline: Some(UtcMicros(1_000)),
        created_at: UtcMicros(10),
        updated_at: UtcMicros(10),
    })
    .unwrap()
}

pub fn graph(items: Vec<WorkItemV1>) -> WorkProductGraphV1 {
    WorkProductGraphV1::new(
        WorkGraphVersionV1::initial(),
        vec![
            WorkInitiativeV1::new(
                id("initiative.work-product"),
                "Work product initiative".to_owned(),
                UtcMicros(1),
            )
            .unwrap(),
        ],
        vec![
            WorkPlanV1::new(
                id("plan.work-product"),
                id("initiative.work-product"),
                "Work product plan".to_owned(),
                UtcMicros(2),
            )
            .unwrap(),
        ],
        vec![
            WorkMilestoneV1::new(
                id("milestone.work-product"),
                id("plan.work-product"),
                "Work product milestone".to_owned(),
                UtcMicros(3),
            )
            .unwrap(),
        ],
        items,
    )
    .unwrap()
}

pub type Mutations =
    WorkProductMutationServiceV1<WorkSqliteStorage, WorkSqliteStorage, WorkSqliteStorage>;

pub fn mutations(store: &RegisteredWorkStore) -> Mutations {
    WorkProductMutationServiceV1::new(
        store.storage().clone(),
        store.storage().clone(),
        store.storage().clone(),
    )
}

pub fn reads(
    store: &RegisteredWorkStore,
) -> WorkProductReadServiceV1<WorkSqliteStorage, WorkSqliteStorage> {
    WorkProductReadServiceV1::new(store.storage().clone(), store.storage().clone(), binding())
}

pub fn create(
    store: &RegisteredWorkStore,
    command: &str,
    occurred_at: UtcMicros,
    items: Vec<WorkItemV1>,
) -> Result<tracedecay_contracts::WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
    mutations(store).create(
        &context(),
        &binding(),
        CreateWorkProductRequestV1 {
            selection: repository_selection(),
            initial_graph: graph(items),
            mutation: mutation(command, occurred_at),
        },
    )
}

pub fn read_current(
    store: &RegisteredWorkStore,
) -> Result<WorkGraphReadV1, WorkProductApplicationErrorV1> {
    reads(store).read_graph(
        &context(),
        WorkGraphReadRequestV1::current(repository_selection(), PROJECTED_AT),
    )
}
