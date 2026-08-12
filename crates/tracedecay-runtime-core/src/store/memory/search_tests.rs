use std::collections::BTreeSet;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
use crate::privacy::{MemoryFactSanitizationV1, sanitize_memory_fact_payload};
use crate::store::memory::{DatabaseFactStore, FactWriteControl};
use serde_json::json;
use tempfile::{TempDir, tempdir};
use tracedecay_domain::{
    Confidence, DomainError, FactCategoryV1, FactId, FactOwnerV1, ProvenanceId,
};
use tracedecay_store::{
    FactReadControl, FactStoreError, ProjectMemoryFactAddCommandV1,
    ProjectMemoryFactAddDispositionV1, ProjectMemoryFactAddMaterialV1,
    ProjectMemoryFactContradictionQueryV1, ProjectMemoryFactIdV1, ProjectMemoryFactProjectionV1,
    ProjectMemoryFactRemoveCommandV1, ProjectMemoryFactRetrievalCommandV1,
    ProjectMemoryFactSearchCursorV1, ProjectMemoryFactSearchGraphCoverageV1,
    ProjectMemoryFactSearchGraphDegradationV1, ProjectMemoryFactSearchKindV1,
    ProjectMemoryFactSearchQuery, ProjectMemoryFactStore,
};

use super::candidates::project_memory_probe_candidates_tx;
use super::search::{
    ensure_project_memory_search_not_cancelled, find_project_memory_contradictions_tx,
    probe_project_memory_facts_tx, project_memory_graph_degradation,
};

async fn database() -> (TempDir, Database) {
    let directory = tempdir().expect("create search fixture directory");
    let path = directory.path().join("canonical-search.db");
    let authority = DatabaseAuthority::acquire_test(&path, "canonical search test authority")
        .expect("acquire search fixture authority");
    let (database, _) =
        Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
            .await
            .expect("publish search fixture runtime");
    (directory, database)
}

fn write_control() -> FactWriteControl {
    FactWriteControl::new(Arc::new(|| false), Arc::new(|| true))
}

fn live_read_control(initially_interrupted: bool) -> (Arc<AtomicBool>, FactReadControl) {
    let interrupted = Arc::new(AtomicBool::new(initially_interrupted));
    let observed = Arc::clone(&interrupted);
    let control = FactReadControl::new(Arc::new(move || observed.load(Ordering::Acquire)));
    (interrupted, control)
}

fn read_control_interrupted_on_poll(
    interrupt_on_poll: usize,
) -> (Arc<AtomicBool>, Arc<AtomicUsize>, FactReadControl) {
    let interrupted = Arc::new(AtomicBool::new(false));
    let polls = Arc::new(AtomicUsize::new(0));
    let observed_interrupted = Arc::clone(&interrupted);
    let observed_polls = Arc::clone(&polls);
    let control = FactReadControl::new(Arc::new(move || {
        if observed_polls.fetch_add(1, Ordering::AcqRel) == interrupt_on_poll {
            observed_interrupted.store(true, Ordering::Release);
        }
        observed_interrupted.load(Ordering::Acquire)
    }));
    (interrupted, polls, control)
}

fn add_command(
    operation: &str,
    content: &str,
    mut entities: Vec<String>,
) -> ProjectMemoryFactAddCommandV1 {
    entities.sort_unstable();
    let material = json!({
        "content": content,
        "category": "project",
        "tags": ["search-fixture"],
        "entities": entities,
        "metadata": {"fixture": "canonical-search"},
    });
    let receipt = match sanitize_memory_fact_payload(material.clone())
        .expect("sanitize canonical search fixture")
    {
        MemoryFactSanitizationV1::Durable { payload, receipt } => {
            assert_eq!(payload, material);
            receipt
        }
        MemoryFactSanitizationV1::Quarantined => {
            panic!("canonical search fixture must not be quarantined")
        }
    };
    ProjectMemoryFactAddMaterialV1::new(
        FactOwnerV1::Profile,
        content.to_owned(),
        FactCategoryV1::Project,
        None,
        vec!["search-fixture".to_owned()],
        entities,
        json!({"fixture": "canonical-search"}),
        receipt,
        None,
        Confidence::new(0.8).expect("search fixture trust"),
        None,
    )
    .and_then(|material| {
        material.into_command(
            ProvenanceId::new(operation.to_owned()).expect("search fixture operation id"),
        )
    })
    .expect("canonical search add command")
}

async fn add_fact(
    store: &DatabaseFactStore<'_>,
    control: &FactWriteControl,
    operation: &str,
    content: &str,
    entities: Vec<String>,
) -> FactId {
    let outcome = store
        .add_project_memory_fact(add_command(operation, content, entities), control)
        .await
        .expect("add search fixture fact");
    assert_eq!(
        outcome.disposition(),
        ProjectMemoryFactAddDispositionV1::Added
    );
    outcome.fact().fact_id().clone()
}

async fn collect_search_ids(
    store: &DatabaseFactStore<'_>,
    read_control: &FactReadControl,
) -> (Vec<FactId>, Vec<ProjectMemoryFactSearchCursorV1>) {
    let mut ids = Vec::new();
    let mut cursors = Vec::new();
    let mut after = None;
    for _ in 0..64 {
        // A previous page's wall-clock score can exceed the same fact's score
        // on resume. Force that drift deterministically so the continuation
        // must anchor on fact identity rather than re-admitting the cursor.
        let stale_after = after
            .as_ref()
            .map(|cursor: &ProjectMemoryFactSearchCursorV1| {
                ProjectMemoryFactSearchCursorV1::new(
                    cursor
                        .score_millionths()
                        .checked_add(1)
                        .expect("search fixture score has drift headroom"),
                    cursor.updated_at(),
                    cursor.fact_id().clone(),
                )
                .expect("stale search fixture cursor")
            });
        let query = ProjectMemoryFactSearchQuery::new(
            FactOwnerV1::Profile,
            ProjectMemoryFactSearchKindV1::Search,
            Some("recalltoken".to_owned()),
            stale_after,
            1,
        )
        .expect("search fixture query");
        let page = store
            .search_project_memory_facts(query, read_control)
            .await
            .expect("search fixture page");
        assert_eq!(
            page.graph_coverage(),
            ProjectMemoryFactSearchGraphCoverageV1::NotMounted
        );
        if let Some(hit) = page.hits().first() {
            assert!(hit.scores().fts_score_millionths() > 0);
            assert!(hit.scores().jaccard_score_millionths() > 0);
            assert!(hit.scores().holographic_score_millionths() <= 1_000_000);
            ids.push(hit.fact().fact_id().clone());
        }
        let Some(cursor) = page.next_after().cloned() else {
            return (ids, cursors);
        };
        cursors.push(cursor.clone());
        after = Some(cursor);
    }
    panic!("search fixture pagination did not terminate")
}

fn retrieval_count(projection: &ProjectMemoryFactProjectionV1) -> u64 {
    match projection {
        ProjectMemoryFactProjectionV1::Available(fact) => fact.telemetry().retrieval_count(),
        ProjectMemoryFactProjectionV1::Unavailable(_) => {
            panic!("retrieval fixture must remain eligible")
        }
    }
}

#[tokio::test]
async fn eligible_low_limit_search_reaches_older_facts_across_stable_pages() {
    let (_directory, database) = database().await;
    let store = DatabaseFactStore::new(&database);
    let control = write_control();
    let mut expected = BTreeSet::new();
    for index in 0..40 {
        let content = format!(
            "recalltoken archive item unique{index} payload{:02x}",
            index * 17
        );
        expected.insert(
            add_fact(
                &store,
                &control,
                &format!("fixture.search.add.{index}"),
                &content,
                vec![format!("Entity{index}")],
            )
            .await,
        );
    }
    let removed = add_fact(
        &store,
        &control,
        "fixture.search.add.removed",
        "recalltoken deleted payload must remain ineligible",
        vec!["DeletedEntity".to_owned()],
    )
    .await;
    store
        .remove_project_memory_fact(
            ProjectMemoryFactRemoveCommandV1::new(
                ProjectMemoryFactIdV1::new(FactOwnerV1::Profile, removed.clone())
                    .expect("removed fixture target"),
                ProvenanceId::new("fixture.search.remove".to_owned())
                    .expect("removed fixture operation"),
                None,
                None,
            )
            .expect("remove search fixture command"),
            &control,
        )
        .await
        .expect("remove search fixture fact");

    let (_interrupted, read_control) = live_read_control(false);
    let (first_ids, first_cursors) = collect_search_ids(&store, &read_control).await;
    let (second_ids, second_cursors) = collect_search_ids(&store, &read_control).await;
    assert_eq!(first_ids, second_ids);
    assert_eq!(first_ids.len(), expected.len());
    assert_eq!(second_ids.len(), expected.len());
    assert_eq!(first_cursors.len(), expected.len() - 1);
    assert_eq!(second_cursors.len(), expected.len() - 1);
    let cursor_positions = |cursors: &[ProjectMemoryFactSearchCursorV1]| {
        cursors
            .iter()
            .map(|cursor| (cursor.updated_at(), cursor.fact_id().clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        cursor_positions(&first_cursors),
        cursor_positions(&second_cursors)
    );
    assert_eq!(first_ids.iter().cloned().collect::<BTreeSet<_>>(), expected);
    assert_eq!(
        second_ids.iter().cloned().collect::<BTreeSet<_>>(),
        expected
    );
    assert!(!first_ids.contains(&removed));

    let tracked_id = expected.iter().next().expect("tracked fixture id").clone();
    let tracked_target = ProjectMemoryFactIdV1::new(FactOwnerV1::Profile, tracked_id.clone())
        .expect("tracked fixture target");
    let before = store
        .get_project_memory_fact(tracked_target.clone(), &read_control)
        .await
        .expect("read untracked search fixture")
        .expect("untracked search fixture exists");
    assert_eq!(retrieval_count(&before), 0);
    let retrieval_operation = ProvenanceId::new("fixture.search.retrieval".to_owned())
        .expect("retrieval fixture operation");
    let retrieval_command = ProjectMemoryFactRetrievalCommandV1::new(
        FactOwnerV1::Profile,
        retrieval_operation.clone(),
        vec![tracked_target.clone()],
        true,
    )
    .expect("retrieval fixture command");
    let retrieval_input_digest = retrieval_command
        .input_digest()
        .expect("canonical retrieval input digest");
    let tracked = store
        .record_project_memory_fact_retrieval(retrieval_command.clone(), &control)
        .await
        .expect("record tracked search fixture");
    assert_eq!(tracked.receipt().owner(), &FactOwnerV1::Profile);
    assert_eq!(tracked.receipt().operation_id(), &retrieval_operation);
    assert_eq!(tracked.receipt().input_digest(), retrieval_input_digest);
    assert_eq!(tracked.receipt().fact_ids(), &[tracked_target.clone()]);
    assert!(tracked.receipt().recall());
    assert!(!tracked.receipt().replayed());
    assert_eq!(tracked.projections().len(), 1);
    assert_eq!(retrieval_count(&tracked.projections()[0]), 1);

    let replayed = store
        .record_project_memory_fact_retrieval(retrieval_command, &control)
        .await
        .expect("replay tracked search fixture");
    assert_eq!(replayed.receipt().owner(), tracked.receipt().owner());
    assert_eq!(
        replayed.receipt().operation_id(),
        tracked.receipt().operation_id()
    );
    assert_eq!(
        replayed.receipt().input_digest(),
        tracked.receipt().input_digest()
    );
    assert_eq!(replayed.receipt().fact_ids(), tracked.receipt().fact_ids());
    assert_eq!(replayed.receipt().recall(), tracked.receipt().recall());
    assert!(replayed.receipt().replayed());
    assert_eq!(retrieval_count(&replayed.projections()[0]), 1);

    let changed_input = ProjectMemoryFactRetrievalCommandV1::new(
        FactOwnerV1::Profile,
        retrieval_operation,
        vec![tracked_target.clone()],
        false,
    )
    .expect("changed retrieval fixture command");
    let conflict = store
        .record_project_memory_fact_retrieval(changed_input, &control)
        .await
        .expect_err("changed input cannot reuse a retrieval operation id");
    assert!(matches!(conflict, FactStoreError::OperationConflict));
    let after_conflict = store
        .get_project_memory_fact(tracked_target.clone(), &read_control)
        .await
        .expect("read tracked search fixture after conflict")
        .expect("tracked search fixture remains present");
    assert_eq!(retrieval_count(&after_conflict), 1);

    let duplicate_targets = ProjectMemoryFactRetrievalCommandV1::new(
        FactOwnerV1::Profile,
        ProvenanceId::new("fixture.search.retrieval.duplicate".to_owned())
            .expect("duplicate retrieval fixture operation"),
        vec![tracked_target.clone(), tracked_target],
        true,
    )
    .expect_err("duplicate retrieval targets are not canonical");
    assert!(matches!(
        duplicate_targets,
        FactStoreError::Contract(DomainError::DuplicateId {
            field: "project memory fact retrieval targets"
        })
    ));

    let removed_error = store
        .record_project_memory_fact_retrieval(
            ProjectMemoryFactRetrievalCommandV1::new(
                FactOwnerV1::Profile,
                ProvenanceId::new("fixture.search.retrieval.removed".to_owned())
                    .expect("removed retrieval fixture operation"),
                vec![
                    ProjectMemoryFactIdV1::new(FactOwnerV1::Profile, removed.clone())
                        .expect("removed retrieval fixture target"),
                ],
                true,
            )
            .expect("removed retrieval fixture command"),
            &control,
        )
        .await
        .expect_err("removed facts cannot be recorded as retrieved");
    assert!(matches!(
        removed_error,
        FactStoreError::FactDeleted { fact_id } if fact_id == removed
    ));
}

#[tokio::test]
async fn related_search_restores_entity_cooccurrence_without_direct_only_filtering() {
    let (_directory, database) = database().await;
    let store = DatabaseFactStore::new(&database);
    let control = write_control();
    let source = add_fact(
        &store,
        &control,
        "fixture.related.source",
        "TraceDecay stores canonical facts in SQLite",
        vec!["TraceDecay".to_owned(), "SQLite".to_owned()],
    )
    .await;
    let cooccurring = add_fact(
        &store,
        &control,
        "fixture.related.cooccurring",
        "Grafeo projects relations from SQLite facts",
        vec!["SQLite".to_owned(), "Grafeo".to_owned()],
    )
    .await;
    let unrelated = add_fact(
        &store,
        &control,
        "fixture.related.unrelated",
        "A separate weather record remains isolated",
        vec!["Weather".to_owned()],
    )
    .await;
    let query = ProjectMemoryFactSearchQuery::new(
        FactOwnerV1::Profile,
        ProjectMemoryFactSearchKindV1::Related {
            entity: "TraceDecay".to_owned(),
        },
        None,
        None,
        10,
    )
    .expect("related fixture query");
    let (_interrupted, read_control) = live_read_control(false);
    let page = store
        .related_project_memory_facts(query, &read_control)
        .await
        .expect("related fixture page");
    let ids = page
        .hits()
        .iter()
        .map(|hit| hit.fact().fact_id().clone())
        .collect::<BTreeSet<_>>();
    assert!(ids.contains(&source));
    assert!(ids.contains(&cooccurring));
    assert!(!ids.contains(&unrelated));
}

#[tokio::test]
async fn probe_filters_before_the_bound_instead_of_scanning_a_fact_id_prefix() {
    let (_directory, database) = database().await;
    let store = DatabaseFactStore::new(&database);
    let control = write_control();
    let mut facts = Vec::with_capacity(1_001);
    for index in 0..1_001 {
        let entity = format!("ScopedEntity{index}");
        let fact_id = add_fact(
            &store,
            &control,
            &format!("fixture.probe.scope.{index}"),
            &format!("scope{index} checksum{:016x}", index as u64 * 1_000_003),
            vec![entity.clone()],
        )
        .await;
        facts.push((fact_id, entity));
    }
    facts.sort_by(|(left, _), (right, _)| left.cmp(right));
    let (target_id, target_entity) = facts
        .last()
        .expect("oversized probe fixture has a target")
        .clone();
    let query = ProjectMemoryFactSearchQuery::new(
        FactOwnerV1::Profile,
        ProjectMemoryFactSearchKindV1::Probe,
        Some(target_entity),
        None,
        1,
    )
    .expect("oversized probe query");
    let (_interrupted, read_control) = live_read_control(false);
    let page = store
        .probe_project_memory_facts(query, &read_control)
        .await
        .expect("oversized probe result");
    assert_eq!(
        page.graph_coverage(),
        ProjectMemoryFactSearchGraphCoverageV1::NotApplicable
    );
    assert_eq!(page.hits().len(), 1);
    assert_eq!(page.hits()[0].fact().fact_id(), &target_id);
}

#[tokio::test]
async fn candidate_rows_and_hydration_observe_live_read_control_toggles() {
    let (_directory, database) = database().await;
    let store = DatabaseFactStore::new(&database);
    let control = write_control();
    add_fact(
        &store,
        &control,
        "fixture.candidate.cancel",
        "SQLite candidate hydration remains interruptible",
        vec!["CancellableEntity".to_owned()],
    )
    .await;
    let query = ProjectMemoryFactSearchQuery::new(
        FactOwnerV1::Profile,
        ProjectMemoryFactSearchKindV1::Probe,
        Some("CancellableEntity".to_owned()),
        None,
        1,
    )
    .expect("candidate cancellation query");
    let min_trust = Confidence::new(0.3).expect("candidate cancellation trust");

    let (row_interrupted, row_polls, row_control) = read_control_interrupted_on_poll(2);
    let row_transaction = database
        .begin_memory_read_transaction("candidate row cancellation test")
        .await
        .expect("candidate row cancellation transaction");
    let row_error =
        project_memory_probe_candidates_tx(&row_transaction, &query, min_trust, &row_control)
            .await
            .expect_err("live interruption must stop inside the candidate row loop");
    assert!(matches!(row_error, FactStoreError::ReadCancelled));
    assert!(row_interrupted.load(Ordering::Acquire));
    assert_eq!(row_polls.load(Ordering::Acquire), 3);
    drop(row_transaction);

    let (hydration_interrupted, hydration_polls, hydration_control) =
        read_control_interrupted_on_poll(6);
    let hydration_transaction = database
        .begin_memory_read_transaction("candidate hydration cancellation test")
        .await
        .expect("candidate hydration cancellation transaction");
    let hydration_error =
        probe_project_memory_facts_tx(&hydration_transaction, &query, &hydration_control)
            .await
            .expect_err("live interruption must stop after candidate hydration");
    assert!(matches!(hydration_error, FactStoreError::ReadCancelled));
    assert!(hydration_interrupted.load(Ordering::Acquire));
    assert_eq!(hydration_polls.load(Ordering::Acquire), 7);
}

#[tokio::test]
async fn contradiction_pair_scan_observes_a_live_read_control_toggle() {
    let (_directory, database) = database().await;
    let store = DatabaseFactStore::new(&database);
    let control = write_control();
    add_fact(
        &store,
        &control,
        "fixture.contradiction.cancel.first",
        "Compiler symbols remain available in the durable index",
        vec!["SharedEntity".to_owned()],
    )
    .await;
    add_fact(
        &store,
        &control,
        "fixture.contradiction.cancel.second",
        "Garden irrigation follows the measured sunrise schedule",
        vec!["SharedEntity".to_owned()],
    )
    .await;

    let interrupted = Arc::new(AtomicBool::new(false));
    let polls = Arc::new(AtomicUsize::new(0));
    let observed_interrupted = Arc::clone(&interrupted);
    let observed_polls = Arc::clone(&polls);
    let read_control = FactReadControl::new(Arc::new(move || {
        if observed_polls.fetch_add(1, Ordering::AcqRel) == 2 {
            observed_interrupted.store(true, Ordering::Release);
        }
        observed_interrupted.load(Ordering::Acquire)
    }));
    let query = ProjectMemoryFactContradictionQueryV1::new(FactOwnerV1::Profile, None, 0, 10)
        .expect("contradiction cancellation query");
    let transaction = database
        .begin_memory_read_transaction("contradiction cancellation test")
        .await
        .expect("contradiction cancellation transaction");
    let error = find_project_memory_contradictions_tx(&transaction, &query, &read_control)
        .await
        .expect_err("live interruption must stop the pair scan");
    assert!(matches!(error, FactStoreError::ReadCancelled));
    assert!(interrupted.load(Ordering::Acquire));
    assert_eq!(polls.load(Ordering::Acquire), 3);
}

#[test]
fn graph_degradation_and_cancellation_remain_typed() {
    assert_eq!(
        project_memory_graph_degradation(&FactStoreError::GraphConflict),
        Some(ProjectMemoryFactSearchGraphDegradationV1::Conflict)
    );
    assert_eq!(
        project_memory_graph_degradation(&FactStoreError::GraphUnavailable),
        Some(ProjectMemoryFactSearchGraphDegradationV1::Unavailable)
    );
    assert_eq!(
        project_memory_graph_degradation(&FactStoreError::GraphBudgetExhausted),
        Some(ProjectMemoryFactSearchGraphDegradationV1::BudgetExhausted)
    );
    assert_eq!(
        project_memory_graph_degradation(&FactStoreError::GraphDeadlineExceeded),
        Some(ProjectMemoryFactSearchGraphDegradationV1::DeadlineExceeded)
    );
    assert_eq!(
        project_memory_graph_degradation(&FactStoreError::GraphCancelled),
        None
    );
    assert_eq!(
        project_memory_graph_degradation(&FactStoreError::GraphResetRequired {
            owner: FactOwnerV1::Profile,
            reason: "verified graph reset".to_owned(),
        }),
        None,
        "reset-required must reach the application boundary instead of degrading"
    );
    let (interrupted, read_control) = live_read_control(true);
    assert!(matches!(
        ensure_project_memory_search_not_cancelled(&read_control),
        Err(FactStoreError::ReadCancelled)
    ));
    interrupted.store(false, Ordering::Release);
    assert!(ensure_project_memory_search_not_cancelled(&read_control).is_ok());
}
