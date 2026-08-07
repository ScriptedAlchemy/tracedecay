//! The Work product graph authority, end to end over the registered store.
//!
//! This suite drives the REAL composition — the application's
//! `WorkProductMutationServiceV1` and `WorkProductReadServiceV1` over the
//! registered exact-SQL storage, with no port doubles anywhere — because the
//! defect this authority was built to close was precisely that every
//! implementation of these ports was a test double. A suite that substituted
//! its own port would reproduce the defect it is meant to prove is gone.
//!
//! The assertions are about truthfulness as much as about persistence. The
//! Work views draw effort, concurrency, churn, and a critical path, and every
//! one of those is computed from `WorkItemV1::effort`, which the domain
//! refuses to let be zero. So the test declares effort explicitly and then
//! asserts the projections carry back exactly the declared numbers: if any
//! layer ever starts estimating one, these equalities break.

mod work_registered_store;

use std::collections::BTreeSet;

use tracedecay_application::{
    CancellationContext, CapabilityGrantSnapshot, CreateWorkProductRequestV1, Deadline,
    DisclosureClass, RequestContext, RequestId, ResolvedScope, WorkGraphReadRequestV1,
    WorkGraphReadV1, WorkProductApplicationErrorV1, WorkProductBindingV1,
    WorkProductExpectedAuthorityV1, WorkProductMutationIdentityV1, WorkProductMutationServiceV1,
    WorkProductReadServiceV1, WorkProductRevisionPinsV1, WorkProductSelectionScopeV1,
    WorkRelationScopeV1,
};
use tracedecay_domain::{
    AcceptanceCriterionId, ActorId, CatalogGenerationId, ConfigurationRevisionId, InitiativeId,
    ManifestDigest, MilestoneId, PolicyRevisionId, ProjectId, RepositoryId, TaskId, UtcMicros,
    WorkAcceptanceCriterionV1, WorkCommandId, WorkGraphVersionV1, WorkHierarchyV1,
    WorkInitiativeV1, WorkItemInputV1, WorkItemV1, WorkMilestoneV1, WorkPlanId, WorkPlanV1,
    WorkProductEventPayloadV1, WorkProductGraphV1, WorkRuntimeProjectionCoverageV1, WorktreeId,
};
use tracedecay_rusqlite_runtime::work::WorkSqliteStorage;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use work_registered_store::RegisteredWorkStore;

const PROJECT: &str = "project.work-product.fixture";
const REPOSITORY: &str = "repository.work-product.fixture";
/// Every read projects at this instant, which is after every event's
/// `occurred_at`, so a projection is never asked to describe its own future.
const PROJECTED_AT: UtcMicros = UtcMicros(400);

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

fn binding() -> WorkProductBindingV1 {
    WorkProductBindingV1::new(
        CapabilityId::new("capability.work.graph.read").unwrap(),
        UseCaseId::new("use-case.work.graph.read").unwrap(),
    )
}

fn repository_selection() -> WorkProductSelectionScopeV1 {
    WorkProductSelectionScopeV1::relations(BTreeSet::from([WorkRelationScopeV1::Repository {
        project_id: id(PROJECT),
        repository_id: id(REPOSITORY),
    }]))
    .unwrap()
}

fn context() -> RequestContext {
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

fn mutation(command: &str, occurred_at: UtcMicros) -> WorkProductMutationIdentityV1 {
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
fn item(task: &str, dependencies: &[&str], effort: u32) -> WorkItemV1 {
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

fn graph(items: Vec<WorkItemV1>) -> WorkProductGraphV1 {
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

type Mutations = WorkProductMutationServiceV1<
    WorkSqliteStorage,
    WorkSqliteStorage,
    WorkSqliteStorage,
    WorkSqliteStorage,
>;

fn mutations(store: &RegisteredWorkStore) -> Mutations {
    WorkProductMutationServiceV1::new(
        store.storage().clone(),
        store.storage().clone(),
        store.storage().clone(),
        store.storage().clone(),
    )
}

fn reads(
    store: &RegisteredWorkStore,
) -> WorkProductReadServiceV1<WorkSqliteStorage, WorkSqliteStorage> {
    WorkProductReadServiceV1::new(store.storage().clone(), store.storage().clone(), binding())
}

fn create(
    store: &RegisteredWorkStore,
    command: &str,
    occurred_at: UtcMicros,
    items: Vec<WorkItemV1>,
) -> Result<tracedecay_application::WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
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

fn read_current(
    store: &RegisteredWorkStore,
) -> Result<WorkGraphReadV1, WorkProductApplicationErrorV1> {
    reads(store).read_graph(
        &context(),
        WorkGraphReadRequestV1::current(repository_selection(), PROJECTED_AT),
    )
}

#[test]
fn a_created_work_product_is_journaled_published_and_read_back_with_declared_effort() {
    let store = RegisteredWorkStore::start("work-product-create");
    let receipt = create(
        &store,
        "command.work-product.create",
        UtcMicros(100),
        vec![
            item("task.design", &[], 3),
            item("task.build", &["task.design"], 5),
        ],
    )
    .expect("create the work product");

    assert!(!receipt.replayed());
    assert!(matches!(
        receipt.event().payload(),
        WorkProductEventPayloadV1::Created { .. }
    ));
    assert_eq!(
        receipt.verified_graph_version().graph_version(),
        WorkGraphVersionV1::initial()
    );
    // The event, its outbox entry, and the verified version are all durable
    // and settled in the same commit: an event can never exist unpublished
    // once its publication succeeded.
    assert_eq!(store.count("work_product_events_v1"), 1);
    assert_eq!(store.count("work_product_graph_versions_v1"), 1);
    assert_eq!(
        store.inspect(|connection| connection
            .query_row(
                "SELECT COUNT(*) FROM work_product_event_outbox_v1 WHERE published_at IS NOT NULL",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()),
        1
    );

    let WorkGraphReadV1::Current { snapshot, .. } = read_current(&store).expect("read current")
    else {
        panic!("a current read must answer with a current snapshot");
    };
    assert_eq!(snapshot.graph().items().len(), 2);
    assert_eq!(snapshot.graph().version(), WorkGraphVersionV1::initial());

    // The channels the Work views cannot draw today, proven present and equal
    // to the DECLARED effort rather than to anything derived. 3 + 5 = 8, and
    // the critical path is design -> build, so its total is also 8.
    let projections = snapshot.projections();
    assert_eq!(projections.workload().total_effort(), 8);
    assert_eq!(projections.critical_path().total_effort(), 8);
    assert_eq!(
        projections.critical_path().task_ids(),
        vec![id::<TaskId>("task.design"), id::<TaskId>("task.build")]
    );
    // The gating edge is the one the item declared as a dependency.
    assert_eq!(projections.dag().gating_edges().len(), 1);
    // No causal candidate was declared, so none is invented from execution
    // order. This absence is the point, not an oversight.
    assert!(projections.causal().candidate_edges().is_empty());
}

#[test]
fn the_runtime_reading_is_complete_only_because_no_attempt_was_ever_accepted() {
    let store = RegisteredWorkStore::start("work-product-runtime");
    create(
        &store,
        "command.work-product.runtime",
        UtcMicros(100),
        vec![item("task.only", &[], 2)],
    )
    .expect("create the work product");

    let WorkGraphReadV1::Current { snapshot, .. } = read_current(&store).expect("read current")
    else {
        panic!("a current read must answer with a current snapshot");
    };
    // Zero observed attempts is COMPLETE here strictly because the graph
    // declares zero accepted attempts. It is a true empty reading, not a
    // stand-in for an unobserved runtime.
    assert_eq!(
        snapshot.runtime().coverage(),
        &WorkRuntimeProjectionCoverageV1::Complete
    );
    assert!(snapshot.runtime().attempts().is_empty());
    assert_eq!(snapshot.runtime().observed_at(), PROJECTED_AT);
}

#[test]
fn replaying_one_command_returns_the_same_event_without_a_second_journal_row() {
    let store = RegisteredWorkStore::start("work-product-replay");
    let first = create(
        &store,
        "command.work-product.replay",
        UtcMicros(100),
        vec![item("task.only", &[], 2)],
    )
    .expect("create the work product");
    let second = create(
        &store,
        "command.work-product.replay",
        UtcMicros(100),
        vec![item("task.only", &[], 2)],
    )
    .expect("replay the identical command");

    assert!(!first.replayed());
    assert!(second.replayed());
    assert_eq!(first.event(), second.event());
    assert_eq!(store.count("work_product_events_v1"), 1);
    assert_eq!(store.count("work_product_graph_versions_v1"), 1);
}

#[test]
fn the_same_command_with_different_input_is_an_idempotency_conflict() {
    let store = RegisteredWorkStore::start("work-product-idempotency");
    create(
        &store,
        "command.work-product.conflict",
        UtcMicros(100),
        vec![item("task.only", &[], 2)],
    )
    .expect("create the work product");

    // Only the declared effort differs, which is exactly the class of silent
    // divergence a reused idempotency key would otherwise hide.
    let conflict = create(
        &store,
        "command.work-product.conflict",
        UtcMicros(100),
        vec![item("task.only", &[], 7)],
    )
    .expect_err("a reused command id with different input must not be accepted");
    assert_eq!(conflict, WorkProductApplicationErrorV1::IdempotencyConflict);
    assert_eq!(store.count("work_product_events_v1"), 1);
}

#[test]
fn a_second_creation_cannot_claim_there_is_no_prior_graph() {
    let store = RegisteredWorkStore::start("work-product-version");
    create(
        &store,
        "command.work-product.first",
        UtcMicros(100),
        vec![item("task.only", &[], 2)],
    )
    .expect("create the work product");

    let conflict = create(
        &store,
        "command.work-product.second",
        UtcMicros(120),
        vec![item("task.other", &[], 4)],
    )
    .expect_err("a second create must lose the compare-and-swap");
    assert_eq!(conflict, WorkProductApplicationErrorV1::VersionConflict);
    assert_eq!(store.count("work_product_events_v1"), 1);
}

#[test]
fn an_owner_with_no_journal_has_no_current_graph_but_an_explicitly_empty_timeline() {
    let store = RegisteredWorkStore::start("work-product-empty");

    // A point read of a version that was never published is an absence, not a
    // zero: a verified version identity requires a real event sequence, so
    // there is no representable empty current graph to answer with.
    assert_eq!(
        read_current(&store).expect_err("an unpublished graph has no current version"),
        WorkProductApplicationErrorV1::NotFoundOrNotAuthorized
    );

    // A range read's zero state IS representable, so it is answered as an
    // explicit complete-and-empty timeline rather than as a refusal.
    let request = WorkGraphReadRequestV1::evolution(
        repository_selection(),
        UtcMicros(0),
        UtcMicros(300),
        PROJECTED_AT,
    )
    .unwrap();
    let WorkGraphReadV1::Evolution { timeline, .. } = reads(&store)
        .read_graph(&context(), request)
        .expect("read evolution")
    else {
        panic!("an evolution read must answer with a timeline");
    };
    assert!(timeline.entries().is_empty());
    assert!(timeline.continuation().is_none());
}

#[test]
fn a_selection_naming_another_project_is_refused_rather_than_narrowed() {
    let store = RegisteredWorkStore::start("work-product-scope");
    create(
        &store,
        "command.work-product.scope",
        UtcMicros(100),
        vec![item("task.only", &[], 2)],
    )
    .expect("create the work product");

    let foreign = WorkProductSelectionScopeV1::relations(BTreeSet::from([
        WorkRelationScopeV1::Repository {
            project_id: id(PROJECT),
            repository_id: id(REPOSITORY),
        },
        WorkRelationScopeV1::Project {
            project_id: id::<ProjectId>("project.someone-else"),
        },
    ]))
    .unwrap();
    let refused = reads(&store)
        .read_graph(
            &context(),
            WorkGraphReadRequestV1::current(foreign, PROJECTED_AT),
        )
        .expect_err("a selection outside the resolved scope must be refused");
    // Refused whole. Silently dropping the unauthorized scope would answer a
    // question the caller did not ask, with data they did not request.
    assert_eq!(refused, WorkProductApplicationErrorV1::NotAuthorized);
}

#[test]
fn a_narrower_selection_than_the_journal_was_written_under_is_not_answered_partially() {
    let store = RegisteredWorkStore::start("work-product-narrow");
    create(
        &store,
        "command.work-product.narrow",
        UtcMicros(100),
        vec![item("task.only", &[], 2)],
    )
    .expect("create the work product");

    // The journal was written under a repository relation scope. A no-Git
    // selection covers none of it, so folding what it does cover would produce
    // a graph that never existed.
    let refused = reads(&store)
        .read_graph(
            &context(),
            WorkGraphReadRequestV1::current(
                WorkProductSelectionScopeV1::ProfileOwnedNoGit,
                PROJECTED_AT,
            ),
        )
        .expect_err("a selection that does not cover the journal must be refused");
    assert_eq!(
        refused,
        WorkProductApplicationErrorV1::NotFoundOrNotAuthorized
    );
}

#[test]
fn the_published_graph_survives_a_registered_store_restart() {
    let store = RegisteredWorkStore::start("work-product-restart");
    let receipt = create(
        &store,
        "command.work-product.restart",
        UtcMicros(100),
        vec![
            item("task.design", &[], 3),
            item("task.build", &["task.design"], 5),
        ],
    )
    .expect("create the work product");
    let digest_before = receipt
        .verified_graph_version()
        .recovered_graph_digest()
        .clone();

    let store = store.restart("work-product-restart");

    let WorkGraphReadV1::Current { snapshot, .. } = read_current(&store).expect("read current")
    else {
        panic!("a current read must answer with a current snapshot");
    };
    // The digest is recomputed by folding the journal after the restart, so an
    // equal digest proves the graph was recovered from durable events rather
    // than from anything the process was holding.
    assert_eq!(
        snapshot.verified_version().recovered_graph_digest(),
        &digest_before
    );
    assert_eq!(snapshot.projections().workload().total_effort(), 8);
}

#[test]
fn a_forensic_read_is_placed_by_observation_time_not_by_the_change_instant() {
    let store = RegisteredWorkStore::start("work-product-forensic");
    create(
        &store,
        "command.work-product.forensic",
        UtcMicros(100),
        vec![item("task.only", &[], 2)],
    )
    .expect("create the work product");

    // The event occurred at 100 and was observed at 100 (the mutation's own
    // instant is the port context's observation), so a forensic window that
    // excludes 100 must return nothing even though an as-of read at 100 finds
    // the version. The two clocks are not interchangeable.
    let request = WorkGraphReadRequestV1::forensic(
        repository_selection(),
        UtcMicros(200),
        UtcMicros(300),
        PROJECTED_AT,
    )
    .unwrap();
    let WorkGraphReadV1::Forensic { timeline, .. } = reads(&store)
        .read_graph(&context(), request)
        .expect("read forensic")
    else {
        panic!("a forensic read must answer with a timeline");
    };
    assert!(timeline.entries().is_empty());

    let request =
        WorkGraphReadRequestV1::as_of(repository_selection(), UtcMicros(100), PROJECTED_AT)
            .unwrap();
    let WorkGraphReadV1::AsOf { snapshot, .. } = reads(&store)
        .read_graph(&context(), request)
        .expect("read as-of")
    else {
        panic!("an as-of read must answer with a snapshot");
    };
    assert_eq!(snapshot.valid_at(), UtcMicros(100));
}

/// A read must never be answered from an event that was appended but whose
/// publication never landed: the unverified fold is exactly the falsified
/// reading this authority exists to prevent.
#[test]
fn an_appended_event_without_a_published_version_is_not_readable() {
    let store = RegisteredWorkStore::start("work-product-unpublished");
    create(
        &store,
        "command.work-product.unpublished",
        UtcMicros(100),
        vec![item("task.only", &[], 2)],
    )
    .expect("create the work product");
    assert!(read_current(&store).is_ok());

    // Remove only the verified publication, leaving the journal intact — the
    // exact durable shape a crash between append and publish would leave if
    // they were not one transaction.
    store.inspect(|connection| {
        connection
            .execute("DELETE FROM work_product_graph_versions_v1", [])
            .expect("drop the published version");
    });

    assert_eq!(
        read_current(&store).expect_err("an unpublished event is not a readable graph"),
        WorkProductApplicationErrorV1::NotFoundOrNotAuthorized
    );
    assert_eq!(store.count("work_product_events_v1"), 1);
}
