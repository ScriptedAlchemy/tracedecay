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
    AddWorkTaskRequestV1, CancellationContext, CapabilityGrantSnapshot, CreateWorkProductRequestV1,
    Deadline, DisclosureClass, RequestContext, RequestId, ResolvedScope, WorkGraphReadRequestV1,
    WorkGraphReadV1, WorkGraphSelectionCoverageV1, WorkProductApplicationErrorV1,
    WorkProductBindingV1, WorkProductExpectedAuthorityV1, WorkProductMutationIdentityV1,
    WorkProductMutationServiceV1, WorkProductReadServiceV1, WorkProductRevisionPinsV1,
    WorkProductSelectionScopeV1, WorkRelationScopeV1,
};
use tracedecay_domain::{
    AcceptanceCriterionId, ActorId, CatalogGenerationId, ConfigurationRevisionId, InitiativeId,
    ManifestDigest, MilestoneId, PolicyRevisionId, ProjectId, RepositoryId, TaskId, UtcMicros,
    WorkAcceptanceCriterionV1, WorkCommandId, WorkGraphVersionV1, WorkHierarchyV1,
    WorkInitiativeV1, WorkItemInputV1, WorkItemV1, WorkMilestoneV1, WorkPlanId, WorkPlanV1,
    WorkProductEventPayloadV1, WorkProductEventSequenceV1, WorkProductGraphV1,
    WorkRuntimeProjectionCoverageV1, WorktreeId,
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

type Mutations =
    WorkProductMutationServiceV1<WorkSqliteStorage, WorkSqliteStorage, WorkSqliteStorage>;

fn mutations(store: &RegisteredWorkStore) -> Mutations {
    WorkProductMutationServiceV1::new(
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
fn a_created_work_product_commits_one_verified_journal_version_with_declared_effort() {
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
    // The event and verified version are the two rows of one atomic commit.
    // Their exact sequence/version identity must agree with the returned
    // receipt; separate row counts alone would not prove that relationship.
    assert_eq!(store.count("work_product_events_v1"), 1);
    assert_eq!(store.count("work_product_graph_versions_v1"), 1);
    let (event_id, event_sequence, verified_sequence, verified_version): (String, i64, i64, i64) =
        store.inspect(|connection| {
            connection
                .query_row(
                    "SELECT event.event_id, event.sequence,
                            verified.event_sequence, verified.graph_version
                     FROM work_product_events_v1 AS event
                     JOIN work_product_graph_versions_v1 AS verified
                       ON verified.owner_brain_id = event.owner_brain_id
                      AND verified.owner_profile_id = event.owner_profile_id
                      AND verified.event_sequence = event.sequence
                      AND verified.graph_version = event.result_graph_version",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .expect("inspect the atomic event and verified graph pair")
        });
    assert_eq!(
        event_id,
        receipt.event().event_id().as_str(),
        "the committed journal row must be the returned event"
    );
    assert_eq!(event_sequence, verified_sequence);
    assert_eq!(
        event_sequence,
        i64::try_from(receipt.event().sequence().get()).expect("event sequence fits SQLite")
    );
    assert_eq!(
        verified_version,
        i64::try_from(receipt.verified_graph_version().graph_version().get())
            .expect("graph version fits SQLite")
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
    assert_eq!(
        first.verified_graph_version(),
        second.verified_graph_version()
    );
    assert_eq!(store.count("work_product_events_v1"), 1);
    assert_eq!(store.count("work_product_graph_versions_v1"), 1);
}

#[test]
fn a_verified_graph_insert_failure_rolls_back_the_journal_event() {
    let store =
        RegisteredWorkStore::start_with_setup("work-product-atomic-rollback", |connection| {
            connection
                .execute_batch(
                    "CREATE TRIGGER reject_work_product_verified_graph
                     BEFORE INSERT ON work_product_graph_versions_v1
                     BEGIN
                       SELECT RAISE(ABORT, 'injected verified graph failure');
                     END;",
                )
                .expect("install verified graph failure trigger");
        });

    create(
        &store,
        "command.work-product.atomic-rollback",
        UtcMicros(100),
        vec![item("task.only", &[], 2)],
    )
    .expect_err("the verified graph failure must reject the whole atomic append");

    assert_eq!(store.count("work_product_events_v1"), 0);
    assert_eq!(store.count("work_product_graph_versions_v1"), 0);
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
    assert_eq!(store.count("work_product_graph_versions_v1"), 1);
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
    assert_eq!(store.count("work_product_graph_versions_v1"), 1);
}

#[test]
fn current_read_at_an_earlier_observation_excludes_later_published_versions() {
    let store = RegisteredWorkStore::start("work-product-observation-cutoff");
    let first = create(
        &store,
        "command.work-product.observation-cutoff.create",
        UtcMicros(100),
        vec![item("task.first", &[], 2)],
    )
    .expect("create the first graph version");
    let mut second_mutation = mutation(
        "command.work-product.observation-cutoff.add",
        UtcMicros(200),
    );
    second_mutation.expected_authority = WorkProductExpectedAuthorityV1::Verified {
        verified_version: first.verified_graph_version().clone(),
    };
    mutations(&store)
        .add_task(
            &context(),
            &binding(),
            AddWorkTaskRequestV1 {
                selection: repository_selection(),
                item: item("task.second", &[], 3),
                mutation: second_mutation,
            },
        )
        .expect("publish the later graph version");

    let WorkGraphReadV1::Current { snapshot, .. } = reads(&store)
        .read_graph(
            &context(),
            WorkGraphReadRequestV1::current(repository_selection(), UtcMicros(150)),
        )
        .expect("read the head visible at the earlier observation")
    else {
        panic!("a current read must answer with a current snapshot");
    };
    assert_eq!(snapshot.graph().version(), WorkGraphVersionV1::initial());
    assert_eq!(snapshot.graph().items().len(), 1);
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

/// A selection that covers no event at all has no version to point at, so a
/// `Current` read is the same typed absence an owner with no journal gets.
/// This is the empty-covered-slice case, not the poisoning one below.
#[test]
fn a_selection_that_covers_no_event_has_no_current_version() {
    let store = RegisteredWorkStore::start("work-product-narrow");
    create(
        &store,
        "command.work-product.narrow",
        UtcMicros(100),
        vec![item("task.only", &[], 2)],
    )
    .expect("create the work product");

    // The journal was written under a repository relation scope from its very
    // first event, so a no-Git selection covers none of it. `Current` is a
    // point read of a version and there is no version inside this selection to
    // read, which is exactly the absence an empty journal reports.
    let refused = reads(&store)
        .read_graph(
            &context(),
            WorkGraphReadRequestV1::current(
                WorkProductSelectionScopeV1::ProfileOwnedNoGit,
                PROJECTED_AT,
            ),
        )
        .expect_err("a selection covering no event has no current version");
    assert_eq!(
        refused,
        WorkProductApplicationErrorV1::NotFoundOrNotAuthorized
    );
}

/// The no-Git poisoning defect, stated as the contract that replaced it.
///
/// A profile owner creates work with no Git relation, and later an authority
/// that can only act under a repository scope — attempt admission is the real
/// one — appends a repository-scoped event to the same owner journal. The old
/// rule refused the entire no-Git read from that moment on, permanently, so
/// work the caller was plainly authorized for became unreadable because of an
/// event admitted beside it.
///
/// The ruled contract: the covered prefix is answered, and the answer says what
/// it left out.
#[test]
fn a_scoped_event_beside_no_git_work_does_not_poison_the_no_git_selection() {
    let store = RegisteredWorkStore::start("work-product-no-git-prefix");
    let created = mutations(&store)
        .create(
            &context(),
            &binding(),
            CreateWorkProductRequestV1 {
                selection: WorkProductSelectionScopeV1::ProfileOwnedNoGit,
                initial_graph: graph(vec![item("task.no-git", &[], 2)]),
                mutation: mutation("command.work-product.no-git", UtcMicros(100)),
            },
        )
        .expect("create profile-owned work with no Git relation");

    // The event beside it: admitted under a repository relation scope, on the
    // same owner journal. This is what a settled provider attempt publishes.
    mutations(&store)
        .add_task(
            &context(),
            &binding(),
            AddWorkTaskRequestV1 {
                selection: repository_selection(),
                item: item("task.repository-scoped", &[], 3),
                mutation: WorkProductMutationIdentityV1 {
                    expected_authority: WorkProductExpectedAuthorityV1::Verified {
                        verified_version: created.verified_graph_version().clone(),
                    },
                    ..mutation("command.work-product.repository", UtcMicros(200))
                },
            },
        )
        .expect("publish a repository-scoped event beside the no-Git work");

    let read = reads(&store)
        .read_graph(
            &context(),
            WorkGraphReadRequestV1::current(
                WorkProductSelectionScopeV1::ProfileOwnedNoGit,
                PROJECTED_AT,
            ),
        )
        .expect("the covered prefix is readable, not poisoned by the event beside it");

    // The disclosure is the whole point: the caller is told, in the read model's
    // own coverage vocabulary, that one event lies outside this selection and
    // where the boundary is. Answering the prefix silently would be the real
    // falsification.
    assert_eq!(
        read.selection_coverage(),
        &WorkGraphSelectionCoverageV1::Partial {
            covered_events: 1,
            excluded_events: 1,
            first_excluded_sequence: WorkProductEventSequenceV1::new(2).unwrap(),
        },
        "the read must disclose the scoped event outside this selection"
    );
    let WorkGraphReadV1::Current { snapshot, .. } = read else {
        panic!("a current read must answer with a current snapshot");
    };
    // The answer is the covered slice folded on its own: the no-Git task, at
    // the version its own event published, and nothing from the event outside
    // the selection.
    assert_eq!(snapshot.verified_version().graph_version().get(), 1);
    assert_eq!(
        snapshot
            .graph()
            .items()
            .iter()
            .map(|item| item.task_id().as_str().to_owned())
            .collect::<Vec<_>>(),
        vec!["task.no-git".to_owned()],
        "the covered slice must not carry the scoped task folded beside it"
    );
    assert_eq!(snapshot.projections().workload().total_effort(), 2);

    // A repository selection covers the scope-free events too, so the same
    // journal reads whole under it — with a `Complete` disclosure. That is the
    // remedy the mutation refusal names, proven to actually work.
    let whole = reads(&store)
        .read_graph(
            &context(),
            WorkGraphReadRequestV1::current(repository_selection(), PROJECTED_AT),
        )
        .expect("the widened selection covers the whole journal");
    assert_eq!(
        whole.selection_coverage(),
        &WorkGraphSelectionCoverageV1::Complete { covered_events: 2 }
    );
}

/// Reads answer over a covered slice; mutations do not. A prepared change pins
/// the head it read, and under partial coverage that head is the slice's head,
/// not the journal's — so the refusal is kept, but typed by its actual cause
/// with the selection remedy in it, instead of the concealed
/// `not_found_or_not_authorized` the old rule produced.
#[test]
fn a_mutation_over_a_partially_covered_selection_is_refused_by_name() {
    let store = RegisteredWorkStore::start("work-product-no-git-mutation");
    let created = mutations(&store)
        .create(
            &context(),
            &binding(),
            CreateWorkProductRequestV1 {
                selection: WorkProductSelectionScopeV1::ProfileOwnedNoGit,
                initial_graph: graph(vec![item("task.no-git", &[], 2)]),
                mutation: mutation("command.work-product.no-git-mutation", UtcMicros(100)),
            },
        )
        .expect("create profile-owned work with no Git relation");
    mutations(&store)
        .add_task(
            &context(),
            &binding(),
            AddWorkTaskRequestV1 {
                selection: repository_selection(),
                item: item("task.repository-scoped", &[], 3),
                mutation: WorkProductMutationIdentityV1 {
                    expected_authority: WorkProductExpectedAuthorityV1::Verified {
                        verified_version: created.verified_graph_version().clone(),
                    },
                    ..mutation("command.work-product.repository-mutation", UtcMicros(200))
                },
            },
        )
        .expect("publish a repository-scoped event beside the no-Git work");

    let refused = mutations(&store)
        .add_task(
            &context(),
            &binding(),
            AddWorkTaskRequestV1 {
                selection: WorkProductSelectionScopeV1::ProfileOwnedNoGit,
                item: item("task.third", &[], 1),
                mutation: WorkProductMutationIdentityV1 {
                    expected_authority: WorkProductExpectedAuthorityV1::Verified {
                        verified_version: created.verified_graph_version().clone(),
                    },
                    ..mutation("command.work-product.no-git-second", UtcMicros(300))
                },
            },
        )
        .expect_err("a mutation cannot be submitted over a covered slice");
    assert_eq!(
        refused,
        WorkProductApplicationErrorV1::SelectionCoverageIncomplete,
        "the refusal must name the coverage cause, not conceal it as an absence"
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

/// A read must never answer from a journal whose verified row was corrupted
/// out of band. Production cannot commit this shape because both rows share one
/// transaction, but the read authority still fails closed against tampering.
#[test]
fn a_tampered_journal_without_its_verified_version_is_not_readable() {
    let store = RegisteredWorkStore::start("work-product-tampered-verification");
    create(
        &store,
        "command.work-product.tampered-verification",
        UtcMicros(100),
        vec![item("task.only", &[], 2)],
    )
    .expect("create the work product");
    assert!(read_current(&store).is_ok());

    // Remove only the verified row through the out-of-band inspection
    // connection. No production writer exposes this partial mutation.
    store.inspect(|connection| {
        connection
            .execute("DELETE FROM work_product_graph_versions_v1", [])
            .expect("drop the published version");
    });

    assert_eq!(
        read_current(&store).expect_err("an unverified event is not a readable graph"),
        WorkProductApplicationErrorV1::NotFoundOrNotAuthorized
    );
    assert_eq!(store.count("work_product_events_v1"), 1);
}
