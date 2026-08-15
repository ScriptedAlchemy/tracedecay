//! Work product evidence and history, end to end over the registered store.
//!
//! Like the graph authority suite next to it, this drives the REAL composition
//! — the application's `WorkProductEvidenceServiceV1` and
//! `WorkHistoryServiceV1` over the registered exact-SQL storage — because the
//! defect being closed here is that both ports had test doubles and nothing
//! else. A suite that supplied its own port would reproduce that defect.
//!
//! Every fact asserted below was WRITTEN by a mutation earlier in the same
//! test: the evidence link ids, anchors, digests, and event sequences are the
//! caller's own declarations read back. Nothing here is derived, defaulted, or
//! backfilled, and the two places where this authority genuinely cannot see
//! something — content behind an anchor, and links a caller's own limit
//! excluded — are asserted as named absences rather than as data.
//!
//! ## Why this suite declares evidence at creation
//!
//! Evidence enters a graph in exactly two ways: as part of the initial graph a
//! create request declares, or through `WorkGraphChangeV1::AcceptedAttemptLinked`.
//! The mutation/publication authority covers the accepted-attempt route through
//! a registered-store restart and idempotent replay in its graph suite. This
//! suite keeps the initial declaration route because its read assertions need
//! evidence present in the first version, not because that mutation is absent.

mod work_registered_store;

use std::collections::{BTreeMap, BTreeSet};

use tracedecay_application::{
    AcceptWorkTaskRequestV1, AddWorkTaskRequestV1, CancellationContext, CapabilityGrantSnapshot,
    CreateWorkProductRequestV1, Deadline, DisclosureClass, OpaqueCursor, RequestContext, RequestId,
    ResolvedScope, SelectedWorkEvidenceV1, VerifiedWorkGraphVersionV1, WorkEvidenceExpandRequestV1,
    WorkEvidenceSelectRequestV1, WorkGraphReadRequestV1, WorkGraphReadV1,
    WorkGraphSelectionCoverageV1, WorkHistoryCoverageV1,
    WorkHistoryRequestV1, WorkHistoryServiceV1, WorkHistoryV1, WorkProductApplicationErrorV1,
    WorkProductBindingV1, WorkProductEvidenceServiceV1, WorkProductExpectedAuthorityV1,
    WorkProductMutationIdentityV1, WorkProductMutationReceiptV1, WorkProductMutationServiceV1,
    WorkProductReadServiceV1, WorkProductRevisionPinsV1, WorkProductSelectionScopeV1,
    WorkRelationScopeV1,
};
use tracedecay_domain::{
    AcceptanceCriterionId, ActorId, CatalogGenerationId, ConfigurationRevisionId, InitiativeId,
    ManifestDigest, MilestoneId, PolicyRevisionId, ProjectId, RepositoryId, RetrievalAnchorId,
    TaskEvidenceLinkId, TaskEvidenceLinkV1, TaskId, UtcMicros, WorkAcceptanceCriterionV1,
    WorkCommandId, WorkGraphVersionV1, WorkHierarchyV1, WorkInitiativeV1, WorkItemInputV1,
    WorkItemV1, WorkMilestoneV1, WorkPlanId, WorkPlanV1, WorkProductEventSequenceV1,
    WorkProductGraphV1, WorkTaskEvidenceCoverageV1, WorktreeId,
};
use tracedecay_rusqlite_runtime::work::WorkSqliteStorage;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use work_registered_store::RegisteredWorkStore;

const PROJECT: &str = "project.work-product-query.fixture";
const REPOSITORY: &str = "repository.work-product-query.fixture";
/// Every read observes at this instant, which is after every event's
/// `occurred_at`, so no read is asked to answer about its own future.
const OBSERVED_AT: UtcMicros = UtcMicros(400);
const TASK: &str = "task.deliver";

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
        id::<WorktreeId>("worktree.work-product-query.fixture"),
        None,
    )
    .unwrap();
    let capability = CapabilityId::new("capability.work.graph.read").unwrap();
    let use_case = UseCaseId::new("use-case.work.graph.read").unwrap();
    let grant = CapabilityGrantSnapshot::new(
        id("grant.work-product-query.fixture"),
        1,
        digest('a'),
        id::<ActorId>("actor.work-product-query.issuer"),
        UtcMicros(-1_000),
        UtcMicros(10_000),
        scope.clone(),
        BTreeSet::from([capability]),
        BTreeSet::from([use_case]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    RequestContext::new(
        id::<ActorId>("actor.work-product-query.requester"),
        scope,
        grant,
        RequestId::new("request.work-product-query.fixture").unwrap(),
        Deadline::new(UtcMicros(9_000)).unwrap(),
        CancellationContext::active("cancel.work-product-query.fixture").unwrap(),
    )
    .unwrap()
}

fn mutation(
    command: &str,
    occurred_at: UtcMicros,
    expected: WorkProductExpectedAuthorityV1,
) -> WorkProductMutationIdentityV1 {
    WorkProductMutationIdentityV1 {
        expected_authority: expected,
        command_id: id::<WorkCommandId>(command),
        causation_event_id: None,
        evidence: Vec::new(),
        occurred_at,
        revisions: WorkProductRevisionPinsV1 {
            policy_revision_id: id::<PolicyRevisionId>("policy.work-product-query.fixture"),
            configuration_revision_id: id::<ConfigurationRevisionId>(
                "config.work-product-query.fixture",
            ),
            catalog_generation_id: id::<CatalogGenerationId>("catalog.work-product-query.fixture"),
        },
    }
}

fn hierarchy() -> WorkHierarchyV1 {
    WorkHierarchyV1::new(
        id::<InitiativeId>("initiative.work-product-query"),
        id::<WorkPlanId>("plan.work-product-query"),
        id::<MilestoneId>("milestone.work-product-query"),
    )
}

fn criterion(task: &str) -> AcceptanceCriterionId {
    id::<AcceptanceCriterionId>(&format!("criterion.{task}"))
}

fn item(task: &str) -> WorkItemV1 {
    WorkItemV1::new(WorkItemInputV1 {
        task_id: id::<TaskId>(task),
        hierarchy: hierarchy(),
        title: format!("Deliver {task}"),
        dependencies: BTreeSet::new(),
        informational_relations: BTreeSet::new(),
        causal_candidates: BTreeSet::new(),
        acceptance_criteria: vec![
            WorkAcceptanceCriterionV1::new(
                criterion(task),
                format!("{task} has reviewed evidence"),
                true,
            )
            .unwrap(),
        ],
        effort: 3,
        scheduled_at: None,
        deadline: Some(UtcMicros(1_000)),
        created_at: UtcMicros(10),
        updated_at: UtcMicros(10),
    })
    .unwrap()
}

fn bare_graph() -> WorkProductGraphV1 {
    WorkProductGraphV1::new(
        WorkGraphVersionV1::initial(),
        vec![
            WorkInitiativeV1::new(
                id("initiative.work-product-query"),
                "Work product query initiative".to_owned(),
                UtcMicros(1),
            )
            .unwrap(),
        ],
        vec![
            WorkPlanV1::new(
                id("plan.work-product-query"),
                id("initiative.work-product-query"),
                "Work product query plan".to_owned(),
                UtcMicros(2),
            )
            .unwrap(),
        ],
        vec![
            WorkMilestoneV1::new(
                id("milestone.work-product-query"),
                id("plan.work-product-query"),
                "Work product query milestone".to_owned(),
                UtcMicros(3),
            )
            .unwrap(),
        ],
        vec![item(TASK), item("task.other")],
    )
    .unwrap()
}

/// The initial graph a create request declares, carrying the caller's evidence
/// links on `TASK`.
///
/// This goes through the graph's own `Deserialize`, which is the same path a
/// `CreateWorkProductRequestV1` arriving as JSON takes, so the domain validates
/// the item/evidence correspondence exactly as it would in production.
fn graph_declaring(links: &[TaskEvidenceLinkV1]) -> WorkProductGraphV1 {
    let mut value = serde_json::to_value(bare_graph()).expect("serialize the bare graph");
    let link_ids = links
        .iter()
        .map(|link| link.link_id().as_str().to_owned())
        .collect::<Vec<_>>();
    for entry in value["items"]
        .as_array_mut()
        .expect("the graph declares items")
    {
        if entry["input"]["task_id"] == serde_json::Value::String(TASK.to_owned()) {
            entry["evidence_links"] = serde_json::json!(link_ids);
        }
    }
    value["evidence"] = serde_json::to_value(links).expect("serialize the declared evidence");
    serde_json::from_value(value).expect("the declared graph is contractually valid")
}

/// One evidence link, entirely declared by the caller. Every field asserted
/// later in this suite comes from here.
fn link(
    link_id: &str,
    anchor: &str,
    digest_byte: char,
    observed_at: UtcMicros,
) -> TaskEvidenceLinkV1 {
    TaskEvidenceLinkV1::new(
        id::<TaskEvidenceLinkId>(link_id),
        1,
        id::<TaskId>(TASK),
        RetrievalAnchorId::new(anchor).unwrap(),
        digest(digest_byte),
        observed_at,
    )
    .unwrap()
}

fn alpha() -> TaskEvidenceLinkV1 {
    link("link.alpha", "retrieval.alpha", 'b', UtcMicros(80))
}

fn beta() -> TaskEvidenceLinkV1 {
    link("link.beta", "retrieval.beta", 'c', UtcMicros(90))
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

fn evidence_service(
    store: &RegisteredWorkStore,
) -> WorkProductEvidenceServiceV1<WorkSqliteStorage, WorkSqliteStorage> {
    WorkProductEvidenceServiceV1::new(store.storage().clone(), store.storage().clone())
}

fn history_service(
    store: &RegisteredWorkStore,
) -> WorkHistoryServiceV1<WorkSqliteStorage, WorkSqliteStorage> {
    WorkHistoryServiceV1::new(store.storage().clone(), store.storage().clone())
}

fn create(
    store: &RegisteredWorkStore,
    links: &[TaskEvidenceLinkV1],
) -> WorkProductMutationReceiptV1 {
    mutations(store)
        .create(
            &context(),
            &binding(),
            CreateWorkProductRequestV1 {
                selection: repository_selection(),
                initial_graph: graph_declaring(links),
                mutation: mutation(
                    "command.work-product-query.create",
                    UtcMicros(100),
                    WorkProductExpectedAuthorityV1::NoPriorGraph,
                ),
            },
        )
        .expect("create the work product")
}

/// Accept the task against the evidence it declared, which is the second event
/// every multi-version test below needs.
fn accept(
    store: &RegisteredWorkStore,
    expected: &VerifiedWorkGraphVersionV1,
) -> WorkProductMutationReceiptV1 {
    mutations(store)
        .accept_task(
            &context(),
            &binding(),
            AcceptWorkTaskRequestV1 {
                selection: repository_selection(),
                task_id: id::<TaskId>(TASK),
                evidence_by_criterion: BTreeMap::from([(
                    criterion(TASK),
                    id::<TaskEvidenceLinkId>("link.alpha"),
                )]),
                mutation: mutation(
                    "command.work-product-query.accept",
                    UtcMicros(150),
                    WorkProductExpectedAuthorityV1::Verified {
                        verified_version: expected.clone(),
                    },
                ),
            },
        )
        .expect("accept the task")
}

fn select(
    store: &RegisteredWorkStore,
    version: &VerifiedWorkGraphVersionV1,
    limit: u32,
) -> Result<SelectedWorkEvidenceV1, WorkProductApplicationErrorV1> {
    evidence_service(store).select(
        &context(),
        &binding(),
        WorkEvidenceSelectRequestV1 {
            selection: repository_selection(),
            task_id: id::<TaskId>(TASK),
            verified_version: version.clone(),
            limit,
            observed_at: OBSERVED_AT,
        },
    )
}

fn read_history(
    store: &RegisteredWorkStore,
    limit: u32,
    continuation: Option<OpaqueCursor>,
) -> Result<WorkHistoryV1, WorkProductApplicationErrorV1> {
    history_service(store).read(
        &context(),
        &binding(),
        WorkHistoryRequestV1 {
            selection: repository_selection(),
            limit,
            continuation,
            observed_at: OBSERVED_AT,
        },
    )
}

#[test]
fn selected_evidence_is_exactly_the_links_the_caller_declared() {
    let store = RegisteredWorkStore::start("work-product-evidence-select");
    let created = create(&store, &[alpha()]);

    let selected = select(&store, created.verified_graph_version(), 16).expect("select evidence");
    assert_eq!(&selected.verified_version, created.verified_graph_version());
    assert_eq!(
        selected.evidence.graph_version(),
        created.verified_graph_version().graph_version()
    );
    let links = selected.evidence.links();
    assert_eq!(links.len(), 1);
    // Each field is the value declared at the mutation, not a value this
    // authority could have recomputed from anything else it stores.
    assert_eq!(links[0].link_id(), &id::<TaskEvidenceLinkId>("link.alpha"));
    assert_eq!(
        links[0].anchor_id(),
        &RetrievalAnchorId::new("retrieval.alpha").unwrap()
    );
    assert_eq!(links[0].evidence_digest(), &digest('b'));
    assert_eq!(links[0].observed_at(), UtcMicros(80));
    assert_eq!(
        selected.evidence.coverage(),
        &WorkTaskEvidenceCoverageV1::Complete {
            returned: 1,
            available: 1,
        }
    );
}

#[test]
fn a_task_with_no_declared_evidence_reads_as_a_true_empty_rather_than_an_absence() {
    let store = RegisteredWorkStore::start("work-product-evidence-empty");
    let created = create(&store, &[]);

    let selected = select(&store, created.verified_graph_version(), 16).expect("select evidence");
    assert!(selected.evidence.links().is_empty());
    // Zero is complete here strictly because the version declares zero links.
    assert_eq!(
        selected.evidence.coverage(),
        &WorkTaskEvidenceCoverageV1::Complete {
            returned: 0,
            available: 0,
        }
    );

    // A task the version never declared is a different answer entirely.
    let missing = evidence_service(&store)
        .select(
            &context(),
            &binding(),
            WorkEvidenceSelectRequestV1 {
                selection: repository_selection(),
                task_id: id::<TaskId>("task.never-declared"),
                verified_version: created.verified_graph_version().clone(),
                limit: 16,
                observed_at: OBSERVED_AT,
            },
        )
        .expect_err("a task outside the graph has no evidence to be empty about");
    assert_eq!(
        missing,
        WorkProductApplicationErrorV1::NotFoundOrNotAuthorized
    );
}

#[test]
fn a_limit_smaller_than_the_declared_evidence_is_reported_as_a_named_absence() {
    let store = RegisteredWorkStore::start("work-product-evidence-limit");
    let created = create(&store, &[alpha(), beta()]);

    let selected = select(&store, created.verified_graph_version(), 1).expect("select evidence");
    // The truncation is the CALLER's limit, and it is named: two links exist,
    // one is returned, and the coverage says which is which rather than
    // presenting one link as the whole truth.
    let WorkTaskEvidenceCoverageV1::Partial {
        returned,
        available,
        unknowns,
    } = selected.evidence.coverage()
    else {
        panic!("a bounded selection over more links must not report complete coverage");
    };
    assert_eq!(*returned, 1);
    assert_eq!(*available, 2);
    assert_eq!(unknowns.len(), 1);
    // Canonical link-id order makes the bounded page a stable prefix.
    assert_eq!(
        selected.evidence.links()[0].link_id(),
        &id::<TaskEvidenceLinkId>("link.alpha")
    );

    let complete = select(&store, created.verified_graph_version(), 16).expect("select evidence");
    assert_eq!(
        complete.evidence.coverage(),
        &WorkTaskEvidenceCoverageV1::Complete {
            returned: 2,
            available: 2,
        }
    );
}

#[test]
fn an_earlier_verified_version_is_answered_as_itself_not_upgraded_to_the_current_one() {
    let store = RegisteredWorkStore::start("work-product-evidence-earlier");
    let created = create(&store, &[alpha()]);
    let accepted = accept(&store, created.verified_graph_version());
    assert_ne!(
        accepted.verified_graph_version().graph_version(),
        created.verified_graph_version().graph_version()
    );

    // Verified versions are retained, so naming an earlier one is a temporal
    // read. The answer carries that version's identity, not the current one's:
    // silently upgrading it would answer a question the caller did not ask.
    let earlier = select(&store, created.verified_graph_version(), 16).expect("select evidence");
    assert_eq!(&earlier.verified_version, created.verified_graph_version());
    assert_eq!(
        earlier.evidence.graph_version(),
        created.verified_graph_version().graph_version()
    );
    let current = select(&store, accepted.verified_graph_version(), 16).expect("select evidence");
    assert_eq!(&current.verified_version, accepted.verified_graph_version());
}

#[test]
fn an_identity_this_authority_never_verified_is_refused_rather_than_reconciled() {
    let store = RegisteredWorkStore::start("work-product-evidence-identity");
    let created = create(&store, &[alpha()]);
    let verified = created.verified_graph_version();

    // Same version number, different recovered digest: two different readings
    // of one version is precisely what this authority exists to make
    // impossible, so it is a conflict rather than a quietly corrected answer.
    let forged = VerifiedWorkGraphVersionV1::new(
        verified.graph_version(),
        verified.event_sequence(),
        verified.source_watermark().clone(),
        digest('f'),
    )
    .unwrap();
    assert_eq!(
        select(&store, &forged, 16).expect_err("a disagreeing identity must not be answered"),
        WorkProductApplicationErrorV1::VersionConflict
    );

    // A version that was never published is an absence, not a conflict.
    let unpublished = VerifiedWorkGraphVersionV1::new(
        WorkGraphVersionV1::new(9).unwrap(),
        verified.event_sequence(),
        verified.source_watermark().clone(),
        verified.recovered_graph_digest().clone(),
    )
    .unwrap();
    assert_eq!(
        select(&store, &unpublished, 16)
            .expect_err("a version this authority never published cannot be answered"),
        WorkProductApplicationErrorV1::NotFoundOrNotAuthorized
    );
}

#[test]
fn an_expansion_returns_the_declared_anchor_and_says_the_content_was_not_disclosed() {
    let store = RegisteredWorkStore::start("work-product-evidence-expand");
    let created = create(&store, &[alpha()]);

    let expanded = evidence_service(&store)
        .expand(
            &context(),
            &binding(),
            WorkEvidenceExpandRequestV1 {
                selection: repository_selection(),
                task_id: id::<TaskId>(TASK),
                link_id: id::<TaskEvidenceLinkId>("link.alpha"),
                verified_version: created.verified_graph_version().clone(),
                observed_at: OBSERVED_AT,
            },
        )
        .expect("expand the evidence link");
    assert_eq!(
        expanded.expansion.link().link_id(),
        &id::<TaskEvidenceLinkId>("link.alpha")
    );
    // The handle is the anchor the caller declared — this authority owns no
    // content store, so it hands back the retrieval handle and marks the
    // content undisclosed rather than claiming a disclosure it never made.
    assert_eq!(expanded.expansion.content_handle(), "retrieval.alpha");
    assert!(expanded.expansion.is_redacted());
    assert_eq!(expanded.expansion.observed_at(), OBSERVED_AT);

    let missing = evidence_service(&store)
        .expand(
            &context(),
            &binding(),
            WorkEvidenceExpandRequestV1 {
                selection: repository_selection(),
                task_id: id::<TaskId>(TASK),
                link_id: id::<TaskEvidenceLinkId>("link.never-declared"),
                verified_version: created.verified_graph_version().clone(),
                observed_at: OBSERVED_AT,
            },
        )
        .expect_err("a link that was never declared cannot be expanded");
    assert_eq!(
        missing,
        WorkProductApplicationErrorV1::NotFoundOrNotAuthorized
    );
}

#[test]
fn history_returns_the_journaled_events_in_durable_sequence_order() {
    let store = RegisteredWorkStore::start("work-product-history");
    let created = create(&store, &[alpha()]);
    let accepted = accept(&store, created.verified_graph_version());

    let history = read_history(&store, 16, None).expect("read history");
    assert_eq!(
        history.coverage,
        WorkHistoryCoverageV1::Complete { returned: 2 }
    );
    // The selection this journal was written under covers all of it, so nothing
    // is withheld and the disclosure says so outright.
    assert_eq!(
        history.selection_coverage,
        WorkGraphSelectionCoverageV1::Complete { covered_events: 2 }
    );
    assert_eq!(history.events.len(), 2);
    // The events are the stored ones, identical to the receipts the mutations
    // returned — not a summary of them.
    assert_eq!(&history.events[0], created.event());
    assert_eq!(&history.events[1], accepted.event());
    assert!(history.events[0].sequence() < history.events[1].sequence());
    assert_eq!(
        history.authorized_scope.selection(),
        &repository_selection()
    );
}

#[test]
fn history_pages_resume_from_the_sequence_the_previous_page_ended_on() {
    let store = RegisteredWorkStore::start("work-product-history-paging");
    let created = create(&store, &[alpha()]);
    let accepted = accept(&store, created.verified_graph_version());

    let first = read_history(&store, 1, None).expect("read the first history page");
    let WorkHistoryCoverageV1::Partial {
        returned,
        continuation,
    } = first.coverage.clone()
    else {
        panic!("a page that does not exhaust the journal must carry a continuation");
    };
    assert_eq!(returned, 1);
    assert_eq!(&first.events[0], created.event());

    let second = read_history(&store, 1, Some(continuation)).expect("read the second page");
    assert_eq!(
        second.coverage,
        WorkHistoryCoverageV1::Complete { returned: 1 }
    );
    assert_eq!(&second.events[0], accepted.event());
}

#[test]
fn an_owner_with_no_journal_has_an_explicitly_empty_history() {
    let store = RegisteredWorkStore::start("work-product-history-empty");
    // A range read's zero state is representable, so it is answered as an
    // explicit complete-and-empty history rather than as a refusal.
    let history = read_history(&store, 16, None).expect("read history");
    assert_eq!(
        history.coverage,
        WorkHistoryCoverageV1::Complete { returned: 0 }
    );
    // An owner with no journal and an owner whose every event lies outside the
    // selection both read empty, and only the selection coverage tells them
    // apart: this one has nothing, rather than nothing *it may see*.
    assert_eq!(
        history.selection_coverage,
        WorkGraphSelectionCoverageV1::Complete { covered_events: 0 }
    );
    assert!(history.events.is_empty());
}

#[test]
fn a_history_cursor_this_authority_did_not_mint_is_refused() {
    let store = RegisteredWorkStore::start("work-product-history-cursor");
    create(&store, &[alpha()]);

    let refused = read_history(
        &store,
        16,
        Some(OpaqueCursor::new("cursor.forged").unwrap()),
    )
    .expect_err("a foreign cursor must not be read as a fresh first page");
    assert_eq!(
        refused,
        WorkProductApplicationErrorV1::NotFoundOrNotAuthorized
    );
}

/// A selection that covers no event at all still has a representable answer:
/// an empty page, qualified by a disclosure that says every event lies outside
/// it. This is the empty-covered-slice case, not the poisoning one below.
#[test]
fn a_selection_covering_no_event_reads_empty_with_a_partial_disclosure() {
    let store = RegisteredWorkStore::start("work-product-history-scope");
    create(&store, &[alpha()]);

    // The journal was written under a repository relation scope from its very
    // first event, so a no-Git selection covers none of it. The empty answer is
    // honest only because the disclosure beside it says so — an unqualified
    // empty history would present a hole as a complete record, which is exactly
    // what the old outright refusal was guarding against.
    let history = history_service(&store)
        .read(
            &context(),
            &binding(),
            WorkHistoryRequestV1 {
                selection: WorkProductSelectionScopeV1::ProfileOwnedNoGit,
                limit: 16,
                continuation: None,
                observed_at: OBSERVED_AT,
            },
        )
        .expect("an empty covered slice is representable, not a refusal");

    assert!(history.events.is_empty());
    assert_eq!(
        history.coverage,
        WorkHistoryCoverageV1::Complete { returned: 0 },
        "the page is exhausted; there is no further page under this selection"
    );
    assert_eq!(
        history.selection_coverage,
        WorkGraphSelectionCoverageV1::Partial {
            covered_events: 0,
            excluded_events: 1,
            first_excluded_sequence: WorkProductEventSequenceV1::new(1).unwrap(),
        },
        "an empty page must never be reported as a complete history"
    );
}

/// The no-Git poisoning defect on the history surface, stated as the contract
/// that replaced it.
///
/// A profile owner creates work with no Git relation, and later an authority
/// that can only act under a repository scope appends a repository-scoped event
/// to the same owner journal. The old rule refused the entire no-Git history
/// from that moment on, permanently, so events the caller was plainly
/// authorized for became unreadable because of an event appended beside them.
///
/// The ruled contract: the covered prefix is served, and the answer says what
/// it left out.
#[test]
fn a_scoped_event_beside_no_git_work_does_not_poison_the_no_git_history() {
    let store = RegisteredWorkStore::start("work-product-history-no-git-prefix");
    let created = mutations(&store)
        .create(
            &context(),
            &binding(),
            CreateWorkProductRequestV1 {
                selection: WorkProductSelectionScopeV1::ProfileOwnedNoGit,
                initial_graph: graph_declaring(&[alpha()]),
                mutation: mutation(
                    "command.work-product-query.no-git",
                    UtcMicros(100),
                    WorkProductExpectedAuthorityV1::NoPriorGraph,
                ),
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
                item: item("task.repository-scoped"),
                mutation: mutation(
                    "command.work-product-query.repository",
                    UtcMicros(200),
                    WorkProductExpectedAuthorityV1::Verified {
                        verified_version: created.verified_graph_version().clone(),
                    },
                ),
            },
        )
        .expect("publish a repository-scoped event beside the no-Git work");

    let history = history_service(&store)
        .read(
            &context(),
            &binding(),
            WorkHistoryRequestV1 {
                selection: WorkProductSelectionScopeV1::ProfileOwnedNoGit,
                limit: 16,
                continuation: None,
                observed_at: OBSERVED_AT,
            },
        )
        .expect("the covered prefix is readable, not poisoned by the event beside it");

    // The disclosure is the whole point: the caller is told where this
    // selection stops covering the journal and how much lies past it.
    assert_eq!(
        history.selection_coverage,
        WorkGraphSelectionCoverageV1::Partial {
            covered_events: 1,
            excluded_events: 1,
            first_excluded_sequence: WorkProductEventSequenceV1::new(2).unwrap(),
        },
        "the read must disclose the scoped event outside this selection"
    );
    // The answer is exactly the covered prefix: the owner's own no-Git event,
    // and nothing at or past the boundary the disclosure names.
    assert_eq!(history.events.len(), 1);
    assert_eq!(&history.events[0], created.event());
    assert_eq!(
        history.coverage,
        WorkHistoryCoverageV1::Complete { returned: 1 },
        "the covered prefix was returned whole; paging is a separate axis"
    );
}

/// Evidence reads the same published versions the graph reads do, so an
/// unpublished event must be invisible to both — while history, which is about
/// the events themselves, still reports them.
#[test]
fn evidence_is_not_served_from_a_version_that_was_never_published() {
    let store = RegisteredWorkStore::start("work-product-evidence-unpublished");
    let created = create(&store, &[alpha()]);
    assert!(select(&store, created.verified_graph_version(), 16).is_ok());

    store.inspect(|connection| {
        connection
            .execute("DELETE FROM work_product_graph_versions_v1", [])
            .expect("drop the published versions");
    });

    assert_eq!(
        select(&store, created.verified_graph_version(), 16)
            .expect_err("an unpublished version has no readable evidence"),
        WorkProductApplicationErrorV1::NotFoundOrNotAuthorized
    );
    assert_eq!(store.count("work_product_events_v1"), 1);
    let history = read_history(&store, 16, None).expect("read history");
    assert_eq!(history.events.len(), 1);
}

#[test]
fn evidence_and_history_are_recovered_from_durable_state_after_a_restart() {
    let store = RegisteredWorkStore::start("work-product-query-restart");
    let created = create(&store, &[alpha()]);
    let accepted = accept(&store, created.verified_graph_version());
    let version = accepted.verified_graph_version().clone();

    let store = store.restart("work-product-query-restart");

    // After the restart the version identity is rebuilt by folding the stored
    // journal, so an equal identity proves the evidence was recovered from
    // durable events rather than from anything the process was holding.
    let selected = select(&store, &version, 16).expect("select evidence after the restart");
    assert_eq!(selected.verified_version, version);
    assert_eq!(
        selected.evidence.links()[0].anchor_id(),
        &RetrievalAnchorId::new("retrieval.alpha").unwrap()
    );
    let history = read_history(&store, 16, None).expect("read history after the restart");
    assert_eq!(
        history.coverage,
        WorkHistoryCoverageV1::Complete { returned: 2 }
    );
    assert_eq!(&history.events[1], accepted.event());
}

/// The read service and the evidence service must agree about which version is
/// current; a disagreement would mean two authorities over one journal.
#[test]
fn the_evidence_version_is_the_version_the_graph_read_serves() {
    let store = RegisteredWorkStore::start("work-product-evidence-agreement");
    let created = create(&store, &[alpha()]);
    let accepted = accept(&store, created.verified_graph_version());

    let reads =
        WorkProductReadServiceV1::new(store.storage().clone(), store.storage().clone(), binding());
    let WorkGraphReadV1::Current { snapshot, .. } = reads
        .read_graph(
            &context(),
            WorkGraphReadRequestV1::current(repository_selection(), OBSERVED_AT),
        )
        .expect("read current")
    else {
        panic!("a current read must answer with a current snapshot");
    };
    assert_eq!(
        snapshot.verified_version(),
        accepted.verified_graph_version()
    );

    let selected = select(&store, snapshot.verified_version(), 16).expect("select evidence");
    assert_eq!(&selected.verified_version, snapshot.verified_version());
    assert_eq!(selected.evidence.links().len(), 1);
}
