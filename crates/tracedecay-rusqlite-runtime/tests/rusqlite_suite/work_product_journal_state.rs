//! The Work product journal's current-state projection, checkpoints and
//! conversion, end to end over the registered store.
//!
//! Before the projection every append and every read replayed the owner's
//! whole journal through one unpaged query. The exact-SQL materializer refuses
//! a result over 10,000 rows or 64 MiB, so the append that would make a journal
//! 10,001 events long could never commit, and an operation's cost grew with the
//! journal's lifetime. These tests drive the real service composition over the
//! registered exact-SQL channel and check the properties that replaced it:
//! the journal advances past the old ceiling and survives a restart, history
//! larger than a result is never read as one result, a failed append changes
//! nothing and retries exactly once, an operation reads a number of rows that
//! does not grow with the journal, and a version rebuilt from a checkpoint is
//! the version a full replay produces.
//!
//! Journals too long to build one append at a time are seeded in the shape an
//! earlier release stored them, then converted by the same bounded conversion a
//! daemon runs on open, so the seeding exercises the migration too.

use std::collections::BTreeMap;

use rusqlite::{Connection, params};
use tracedecay_contracts::{
    AddWorkTaskRequestV1, CreateWorkProductRequestV1, OpaqueCursor, VerifiedWorkGraphVersionV1,
    WorkGraphReadRequestV1, WorkGraphReadV1, WorkGraphSelectionCoverageV1, WorkHistoryCoverageV1,
    WorkHistoryRequestV1, WorkHistoryServiceV1, WorkProductApplicationErrorV1,
    WorkProductAuthorizedRelationScopeV1, WorkProductExpectedAuthorityV1,
    WorkProductMutationReceiptV1, WorkProductSelectionScopeV1, WorkStorageFailureV1,
};
use tracedecay_domain::test_fixtures::{digest, id};
use tracedecay_domain::{
    ActorId, BrainId, CatalogGenerationId, ConfigurationRevisionId, PolicyRevisionId,
    RetrievalAnchorId, SourceStoreId, UserProfileId, UtcMicros, WorkCommandId, WorkGraphChangeV1,
    WorkGraphVersionV1, WorkProductEventEvidenceV1, WorkProductEventId, WorkProductEventInputV1,
    WorkProductEventPayloadV1, WorkProductEventSequenceV1, WorkProductEventV1, WorkProductGraphV1,
    WorkProductProfileScopeV1, WorkProductSourceWatermarkV1, canonical_sha256,
};
use tracedecay_rusqlite_runtime::work::install_work_schema;
use tracedecay_rusqlite_runtime::work_product::instrument::measure;

use crate::work_registered_store::RegisteredWorkStore;

use crate::work_product_graph_authority::work_product_graph::{
    PROJECT, PROJECTED_AT, REPOSITORY, binding, context, create, graph, item, mutation, mutations,
    read_current, reads, repository_selection,
};

const TASK: &str = "task.only";
const STORE: &str = "store.journal-state";
const GRAPH_DIGEST_DOMAIN: &str = "tracedecay.rusqlite-runtime.work-product-graph.v1";

/// The tables a database written before the projection held its chain in.
const LEGACY_SCHEMA: &str = "
CREATE TABLE work_product_events_v1 (
    owner_brain_id TEXT NOT NULL,
    owner_profile_id TEXT NOT NULL,
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    event_id TEXT NOT NULL,
    command_id TEXT NOT NULL,
    canonical_input_digest TEXT NOT NULL,
    expected_graph_version INTEGER
        CHECK (expected_graph_version IS NULL OR expected_graph_version > 0),
    result_graph_version INTEGER NOT NULL CHECK (result_graph_version > 0),
    occurred_at INTEGER NOT NULL,
    event_payload TEXT NOT NULL,
    PRIMARY KEY (owner_brain_id, owner_profile_id, sequence),
    UNIQUE (owner_brain_id, owner_profile_id, event_id),
    UNIQUE (owner_brain_id, owner_profile_id, command_id),
    UNIQUE (owner_brain_id, owner_profile_id, result_graph_version)
) STRICT;
CREATE TABLE work_product_graph_versions_v1 (
    owner_brain_id TEXT NOT NULL,
    owner_profile_id TEXT NOT NULL,
    graph_version INTEGER NOT NULL CHECK (graph_version > 0),
    event_sequence INTEGER NOT NULL CHECK (event_sequence > 0),
    valid_at INTEGER NOT NULL,
    observed_at INTEGER NOT NULL CHECK (observed_at >= valid_at),
    source_watermark TEXT NOT NULL,
    recovered_graph_digest TEXT NOT NULL,
    PRIMARY KEY (owner_brain_id, owner_profile_id, graph_version),
    UNIQUE (owner_brain_id, owner_profile_id, event_sequence)
) STRICT;
";

fn run<T>(future: impl std::future::Future<Output = T>) -> T {
    tracedecay_rusqlite_runtime::native_runtime::block_on(future).expect("native runtime")
}

fn repository_scope() -> WorkProductAuthorizedRelationScopeV1 {
    WorkProductAuthorizedRelationScopeV1::Repository {
        project_id: id(PROJECT),
        repository_id: id(REPOSITORY),
    }
}

fn graph_digest(graph: &WorkProductGraphV1) -> String {
    canonical_sha256(&(GRAPH_DIGEST_DOMAIN, graph))
        .expect("digest a graph")
        .as_str()
        .to_owned()
}

/// A journal in the form an earlier release stored it, built in memory.
///
/// Every event is a real `WorkProductEventV1` and every graph is the real fold
/// of the events before it, so the converted journal is one the domain accepts.
/// Digests are exact only where `exact_digest` says so: recomputing one digests
/// the whole graph, and the conversion itself verifies only checkpoints and
/// batch ends, so a long journal needs exact digests nowhere else.
struct LegacyChain {
    events: Vec<WorkProductEventV1>,
    versions: Vec<(u64, i64, String)>,
    graphs: BTreeMap<u64, WorkProductGraphV1>,
}

struct ChainShape<'a> {
    length: u64,
    evidence_per_event: usize,
    time_of: &'a dyn Fn(u64) -> i64,
    scopes_of: &'a dyn Fn(u64) -> Vec<WorkProductAuthorizedRelationScopeV1>,
    exact_digest: &'a dyn Fn(u64) -> bool,
    keep_graphs: bool,
}

fn handoff(sequence: u64, at: i64) -> WorkGraphChangeV1 {
    WorkGraphChangeV1::HandoffRecorded {
        handoff: serde_json::from_value(serde_json::json!({
            "handoff_id": format!("handoff.journal-state.{sequence}"),
            "task_id": TASK,
            "from_actor": "actor.journal-state.from",
            "to_actor": "actor.journal-state.to",
            "evidence_frontier": [],
            "unknowns": [],
            "handed_off_at": at,
        }))
        .expect("a canonical handoff"),
    }
}

fn watermark() -> WorkProductSourceWatermarkV1 {
    WorkProductSourceWatermarkV1::new(BTreeMap::from([(id::<SourceStoreId>(STORE), 1)]))
        .expect("a canonical watermark")
}

fn build_chain(shape: &ChainShape<'_>) -> LegacyChain {
    let mut chain = LegacyChain {
        events: Vec::new(),
        versions: Vec::new(),
        graphs: BTreeMap::new(),
    };
    let mut current: Option<WorkProductGraphV1> = None;
    for sequence in 1..=shape.length {
        let at = (shape.time_of)(sequence);
        let payload = match &current {
            None => WorkProductEventPayloadV1::Created {
                graph: graph(vec![item(TASK, &[], 1)]),
            },
            Some(_) => WorkProductEventPayloadV1::Changed {
                change: Box::new(handoff(sequence, at)),
            },
        };
        let folded = match (&current, &payload) {
            (None, WorkProductEventPayloadV1::Created { graph }) => graph.clone(),
            (Some(prior), WorkProductEventPayloadV1::Changed { change }) => prior
                .clone()
                .apply(change.as_ref().clone())
                .expect("the chain folds"),
            _ => unreachable!("the first event creates and every later one changes"),
        };
        let evidence = (0..shape.evidence_per_event)
            .map(|index| WorkProductEventEvidenceV1 {
                source_store_id: id::<SourceStoreId>(STORE),
                anchor_id: id::<RetrievalAnchorId>(&format!("anchor.{sequence}.{index}")),
                evidence_digest: digest('e'),
            })
            .collect();
        let event = WorkProductEventV1::new(WorkProductEventInputV1 {
            event_id: id::<WorkProductEventId>(&format!("event.journal-state.{sequence}")),
            sequence: WorkProductEventSequenceV1::new(sequence).unwrap(),
            actor_id: id::<ActorId>("actor.work-product.requester"),
            owner_scope: WorkProductProfileScopeV1 {
                brain_id: id::<BrainId>("brain.work-storage"),
                profile_id: id::<UserProfileId>("profile.work-storage"),
            },
            authorized_relation_scopes: (shape.scopes_of)(sequence),
            expected_graph_version: (sequence > 1)
                .then(|| WorkGraphVersionV1::new(sequence - 1).unwrap()),
            result_graph_version: WorkGraphVersionV1::new(sequence).unwrap(),
            command_id: id::<WorkCommandId>(&format!("command.journal-state.{sequence}")),
            canonical_input_digest: digest('c'),
            causation_event_id: None,
            evidence,
            source_watermark: watermark(),
            occurred_at: UtcMicros(at),
            policy_revision_id: id::<PolicyRevisionId>("policy.work-product.fixture"),
            configuration_revision_id: id::<ConfigurationRevisionId>("config.work-product.fixture"),
            catalog_generation_id: id::<CatalogGenerationId>("catalog.work-product.fixture"),
            payload,
        })
        .expect("a canonical event");
        let recorded = if (shape.exact_digest)(sequence) {
            graph_digest(&folded)
        } else {
            digest('0').as_str().to_owned()
        };
        chain.versions.push((sequence, at, recorded));
        chain.events.push(event);
        if shape.keep_graphs {
            chain.graphs.insert(sequence, folded.clone());
        }
        current = Some(folded);
    }
    chain
}

/// Writes the chain the way an earlier release did, into the tables it used.
fn seed_legacy(connection: &Connection, chain: &LegacyChain) {
    connection
        .execute_batch(LEGACY_SCHEMA)
        .expect("create the legacy tables");
    let transaction = connection
        .unchecked_transaction()
        .expect("begin the legacy seed");
    {
        let mut insert_event = transaction
            .prepare(
                "INSERT INTO work_product_events_v1 (
                    owner_brain_id, owner_profile_id, sequence, event_id, command_id,
                    canonical_input_digest, expected_graph_version, result_graph_version,
                    occurred_at, event_payload
                 ) VALUES ('brain.work-storage', 'profile.work-storage', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )
            .expect("prepare the legacy event insert");
        let mut insert_version = transaction
            .prepare(
                "INSERT INTO work_product_graph_versions_v1 (
                    owner_brain_id, owner_profile_id, graph_version, event_sequence,
                    valid_at, observed_at, source_watermark, recovered_graph_digest
                 ) VALUES ('brain.work-storage', 'profile.work-storage', ?1, ?1, ?2, ?2, ?3, ?4)",
            )
            .expect("prepare the legacy version insert");
        for (event, (version, at, recorded)) in chain.events.iter().zip(&chain.versions) {
            insert_event
                .execute(params![
                    i64::try_from(event.sequence().get()).unwrap(),
                    event.event_id().as_str(),
                    event.command_id().as_str(),
                    event.canonical_input_digest().as_str(),
                    event
                        .expected_graph_version()
                        .map(|version| i64::try_from(version.get()).unwrap()),
                    i64::try_from(event.result_graph_version().get()).unwrap(),
                    event.occurred_at().0,
                    serde_json::to_string(event).unwrap(),
                ])
                .expect("insert a legacy event");
            insert_version
                .execute(params![
                    i64::try_from(*version).unwrap(),
                    at,
                    serde_json::to_string(&watermark()).unwrap(),
                    recorded,
                ])
                .expect("insert a legacy version");
        }
    }
    transaction.commit().expect("commit the legacy seed");
}

fn repository_chain(length: u64, evidence_per_event: usize) -> LegacyChain {
    build_chain(&ChainShape {
        length,
        evidence_per_event,
        time_of: &|sequence| 100 + i64::try_from(sequence / 4).unwrap(),
        scopes_of: &|_| vec![repository_scope()],
        exact_digest: &|sequence| sequence.is_multiple_of(128) || sequence == length,
        keep_graphs: false,
    })
}

fn converge(
    store: &RegisteredWorkStore,
) -> Result<(), tracedecay_rusqlite_runtime::work_product::WorkProductJournalMigrationError> {
    run(store.storage().converge_product_journal())
}

fn read_at(
    store: &RegisteredWorkStore,
    selection: WorkProductSelectionScopeV1,
    mode: &str,
    at: i64,
    observed_at: i64,
) -> Result<WorkGraphReadV1, WorkProductApplicationErrorV1> {
    let request = match mode {
        "current" => WorkGraphReadRequestV1::current(selection, UtcMicros(observed_at)),
        "as_of" => WorkGraphReadRequestV1::as_of(selection, UtcMicros(at), UtcMicros(observed_at))
            .expect("an as-of request"),
        other => panic!("unknown read mode {other}"),
    };
    reads(store).read_graph(&context(), request)
}

fn current(store: &RegisteredWorkStore, observed_at: i64) -> WorkGraphReadV1 {
    read_at(store, repository_selection(), "current", 0, observed_at).expect("a current read")
}

fn append_task(
    store: &RegisteredWorkStore,
    selection: &WorkProductSelectionScopeV1,
    name: &str,
    occurred_at: i64,
    expected: &VerifiedWorkGraphVersionV1,
) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
    let mut change = mutation(
        &format!("command.journal-state.{name}"),
        UtcMicros(occurred_at),
    );
    change.expected_authority = WorkProductExpectedAuthorityV1::Verified {
        verified_version: expected.clone(),
    };
    mutations(store).add_task(
        &context(),
        &binding(),
        AddWorkTaskRequestV1 {
            selection: selection.clone(),
            item: item(&format!("task.{name}"), &[], 1),
            mutation: change,
        },
    )
}

/// Builds a journal the service way, one append at a time: a created task and
/// `appends` added tasks, one per instant starting at 100.
fn serviced_journal(
    name: &str,
    appends: u64,
) -> (RegisteredWorkStore, WorkProductMutationReceiptV1) {
    let store = RegisteredWorkStore::start(name);
    let selection = repository_selection();
    let mut latest = create(
        &store,
        "command.journal-state.create",
        UtcMicros(100),
        vec![item(TASK, &[], 1)],
    )
    .expect("create the journal");
    for index in 1..=appends {
        latest = append_task(
            &store,
            &selection,
            &format!("added.{index}"),
            100 + i64::try_from(index).unwrap(),
            latest.verified_graph_version(),
        )
        .expect("append the next version");
    }
    (store, latest)
}

/// Every stored event, decoded, oldest first, read straight from the table.
fn stored_events(store: &RegisteredWorkStore) -> Vec<WorkProductEventV1> {
    store.inspect(|connection| {
        let mut statement = connection
            .prepare("SELECT event_payload FROM work_product_journal_events_v1 ORDER BY sequence")
            .expect("prepare the event scan");
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .expect("scan events")
            .map(|payload| serde_json::from_str(&payload.expect("a row")).expect("a stored event"))
            .collect()
    })
}

/// The graph at every version, by folding the stored events one at a time:
/// the full replay a reconstruction from a checkpoint must agree with.
fn full_replay(events: &[WorkProductEventV1]) -> BTreeMap<u64, WorkProductGraphV1> {
    let mut graphs = BTreeMap::new();
    let mut current: Option<WorkProductGraphV1> = None;
    for event in events {
        let next = match (current.take(), event.payload()) {
            (None, WorkProductEventPayloadV1::Created { graph }) => graph.clone(),
            (Some(prior), WorkProductEventPayloadV1::Changed { change }) => {
                prior.apply(change.as_ref().clone()).expect("replay folds")
            }
            _ => panic!("the stored chain is not one canonical progression"),
        };
        graphs.insert(event.sequence().get(), next.clone());
        current = Some(next);
    }
    graphs
}

fn only_entry(read: &WorkGraphReadV1) -> &tracedecay_contracts::WorkGraphVersionEntryV1 {
    let entries = read.entries();
    assert_eq!(entries.len(), 1, "a point read answers one version");
    &entries[0]
}

fn history_pages(
    store: &RegisteredWorkStore,
    limit: u32,
    observed_at: i64,
) -> Vec<Vec<WorkProductEventV1>> {
    let service = WorkHistoryServiceV1::new(store.storage().clone(), store.storage().clone());
    let mut pages = Vec::new();
    let mut continuation: Option<OpaqueCursor> = None;
    loop {
        let page = service
            .read(
                &context(),
                &binding(),
                WorkHistoryRequestV1 {
                    selection: repository_selection(),
                    limit,
                    continuation: continuation.take(),
                    observed_at: UtcMicros(observed_at),
                },
            )
            .expect("a history page");
        pages.push(page.events);
        match page.coverage {
            WorkHistoryCoverageV1::Complete { .. } => return pages,
            WorkHistoryCoverageV1::Partial {
                continuation: next, ..
            } => continuation = Some(next),
        }
    }
}

/// The 10,000-row ceiling used to stop the journal: the next append read its
/// own new row, exceeded the result cap and rolled back, and no retry could
/// advance it. This crosses it with real appends, restarts, and checks that
/// the current and historical results are right on both sides.
#[test]
fn a_journal_advances_past_ten_thousand_events_and_survives_restart() {
    let chain = repository_chain(9_995, 0);
    let store = RegisteredWorkStore::start_seeded("journal-ten-thousand", |connection| {
        seed_legacy(connection, &chain);
    });
    converge(&store).expect("convert the seeded journal");
    assert_eq!(store.count("work_product_journal_events_v1"), 9_995);
    assert_eq!(
        store.count("work_product_journal_checkpoints_v1"),
        9_995 / 128
    );

    let selection = repository_selection();
    let mut latest = only_entry(&current(&store, 8_000))
        .verified_version()
        .clone();
    assert_eq!(latest.graph_version().get(), 9_995);
    let mut receipts = Vec::new();
    for index in 0..15 {
        let receipt = append_task(
            &store,
            &selection,
            &format!("beyond.{index}"),
            7_000 + index,
            &latest,
        )
        .unwrap_or_else(|error| panic!("append {} must commit: {error:?}", 9_996 + index));
        latest = receipt.verified_graph_version().clone();
        receipts.push(receipt);
    }
    assert_eq!(latest.graph_version().get(), 10_010);
    assert_eq!(store.count("work_product_journal_events_v1"), 10_010);

    let store = store.restart("journal-ten-thousand");

    let head = current(&store, 8_000);
    let head = only_entry(&head);
    assert_eq!(head.verified_version(), &latest);
    assert_eq!(head.graph().items().len(), 16);
    assert_eq!(
        head.graph().item(&id(TASK)).unwrap().handoffs().len(),
        9_994
    );
    assert_eq!(
        head.verified_version().recovered_graph_digest().as_str(),
        graph_digest(head.graph())
    );

    // Version 10,000 is the fifth append past the old ceiling, rebuilt from the
    // checkpoint at 9,984 and the sixteen events after it.
    let at_ceiling = read_at(&store, selection.clone(), "as_of", 7_004, 8_000).unwrap();
    let at_ceiling = only_entry(&at_ceiling);
    assert_eq!(at_ceiling.verified_version().graph_version().get(), 10_000);
    assert_eq!(at_ceiling.graph().items().len(), 5 + 1);
    assert_eq!(
        at_ceiling
            .verified_version()
            .recovered_graph_digest()
            .as_str(),
        graph_digest(at_ceiling.graph())
    );
    // The chain's own last version, from the same checkpoint.
    let chain_end = read_at(&store, selection, "as_of", 2_598, 8_000).unwrap();
    let chain_end = only_entry(&chain_end);
    assert_eq!(chain_end.verified_version().graph_version().get(), 9_995);
    assert_eq!(chain_end.graph().items().len(), 1);

    let pages = history_pages(&store, 1_024, 8_000);
    let events = pages.into_iter().flatten().collect::<Vec<_>>();
    assert_eq!(events.len(), 10_010);
    assert!(
        events
            .windows(2)
            .all(|pair| pair[0].sequence().get() + 1 == pair[1].sequence().get())
    );
}

/// Eighty megabytes of event payload is more than one result may hold, and the
/// graph stays small. Appending, reading the current state, rebuilding an old
/// version and paging history must never need the payload as one result.
#[test]
fn cumulative_payload_over_the_result_cap_never_needs_one_oversized_result() {
    // Its graph is a few hundred small handoffs, so every digest is cheap, and
    // the conversion's batches end where the payload budget ends them, not at a
    // fixed sequence, so every digest must be exact.
    let chain = build_chain(&ChainShape {
        length: 520,
        evidence_per_event: 1_024,
        time_of: &|sequence| 100 + i64::try_from(sequence / 4).unwrap(),
        scopes_of: &|_| vec![repository_scope()],
        exact_digest: &|_| true,
        keep_graphs: false,
    });
    let store = RegisteredWorkStore::start_seeded("journal-sixty-four-mib", |connection| {
        seed_legacy(connection, &chain);
    });
    converge(&store).expect("convert the seeded journal");
    let stored_bytes: i64 = store.inspect(|connection| {
        connection
            .query_row(
                "SELECT SUM(length(event_payload)) FROM work_product_journal_events_v1",
                [],
                |row| row.get(0),
            )
            .expect("sum the stored payload")
    });
    assert!(
        stored_bytes > 64 * 1024 * 1024,
        "the journal must hold more than one result may: {stored_bytes} bytes"
    );

    let head = current(&store, 8_000);
    assert_eq!(
        only_entry(&head).verified_version().graph_version().get(),
        520
    );

    let appended = append_task(
        &store,
        &repository_selection(),
        "over-the-cap",
        7_000,
        only_entry(&head).verified_version(),
    )
    .expect("an append commits beside more than 64 MiB of history");
    assert_eq!(appended.verified_graph_version().graph_version().get(), 521);

    // Version 303 shares its instant with its neighbours, so the latest of them
    // answers; it is rebuilt from the checkpoint at 256.
    let old = read_at(&store, repository_selection(), "as_of", 175, 8_000).unwrap();
    assert_eq!(
        only_entry(&old).verified_version().graph_version().get(),
        303
    );

    let pages = history_pages(&store, 1_024, 8_000);
    let total = pages.iter().map(Vec::len).sum::<usize>();
    assert_eq!(total, 521);
    assert!(
        pages.len() >= 8,
        "history of {stored_bytes} bytes must page, not arrive as one result: {} pages",
        pages.len()
    );
}

/// A refused append leaves the journal exactly as it was, and the same command
/// retried commits once and then replays.
#[test]
fn a_failed_append_leaves_event_version_and_projection_unchanged() {
    let (store, latest) = serviced_journal("journal-failed-append", 2);
    let selection = repository_selection();
    let fingerprint = |store: &RegisteredWorkStore| {
        store.inspect(|connection| {
            let count = |table: &str| -> i64 {
                connection
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })
                    .unwrap()
            };
            let head: (i64, String) = connection
                .query_row(
                    "SELECT head_version, head_graph FROM work_product_journal_head_v1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            (
                count("work_product_journal_events_v1"),
                count("work_product_journal_versions_v1"),
                count("work_product_journal_scopes_v1"),
                count("work_product_journal_checkpoints_v1"),
                head,
            )
        })
    };
    let before = fingerprint(&store);
    assert_eq!(before.0, 3);
    assert_eq!(before.4.0, 3);

    store.inspect(|connection| {
        connection
            .execute_batch(
                "CREATE TRIGGER refuse_the_version
                 BEFORE INSERT ON work_product_journal_versions_v1
                 BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
            )
            .expect("install the failure");
    });
    let failed = append_task(
        &store,
        &selection,
        "retried",
        500,
        latest.verified_graph_version(),
    )
    .expect_err("the version row cannot be written");
    assert_eq!(
        failed,
        WorkProductApplicationErrorV1::EventAuthorityUnavailable
    );
    assert_eq!(
        fingerprint(&store),
        before,
        "a refused append changes nothing"
    );
    assert_eq!(
        only_entry(&current(&store, 8_000)).verified_version(),
        latest.verified_graph_version()
    );

    store.inspect(|connection| {
        connection
            .execute_batch("DROP TRIGGER refuse_the_version")
            .expect("remove the failure");
    });
    let committed = append_task(
        &store,
        &selection,
        "retried",
        500,
        latest.verified_graph_version(),
    )
    .expect("the same command commits once the failure is gone");
    assert!(!committed.replayed());
    let after = fingerprint(&store);
    assert_eq!((after.0, after.1, after.4.0), (4, 4, 4));

    let replayed = append_task(
        &store,
        &selection,
        "retried",
        500,
        latest.verified_graph_version(),
    )
    .expect("the same command again");
    assert!(replayed.replayed());
    assert_eq!(
        replayed.verified_graph_version(),
        committed.verified_graph_version()
    );
    assert_eq!(fingerprint(&store), after, "a replay writes nothing");

    let mut conflicting = mutation("command.journal-state.retried", UtcMicros(500));
    conflicting.expected_authority = WorkProductExpectedAuthorityV1::Verified {
        verified_version: latest.verified_graph_version().clone(),
    };
    let conflict = mutations(&store)
        .add_task(
            &context(),
            &binding(),
            AddWorkTaskRequestV1 {
                selection,
                item: item("task.a-different-input", &[], 1),
                mutation: conflicting,
            },
        )
        .expect_err("a reused command id with different input is refused");
    assert_eq!(conflict, WorkProductApplicationErrorV1::IdempotencyConflict);
    assert_eq!(fingerprint(&store), after);
}

/// What an append and a current read read must not depend on how long the
/// journal is. The same operations on a journal ten times as long read the same
/// queries and rows, and a historical version reads at most the events since
/// its checkpoint.
#[test]
fn appends_and_current_reads_read_a_constant_number_of_rows() {
    let (short_store, short_head) = serviced_journal("journal-rows-short", 30);
    let (long_store, long_head) = serviced_journal("journal-rows-long", 300);
    let selection = repository_selection();

    let cost = |store: &RegisteredWorkStore, head: &WorkProductMutationReceiptV1| {
        let (_, read) = measure(|| current(store, 8_000));
        let (appended, append) = measure(|| {
            append_task(
                store,
                &selection,
                "measured",
                900,
                head.verified_graph_version(),
            )
            .expect("a measured append")
        });
        drop(appended);
        (read, append)
    };
    let (short_read, short_append) = cost(&short_store, &short_head);
    let (long_read, long_append) = cost(&long_store, &long_head);

    // Rows and queries are the cost of the journal's length; bytes are the cost
    // of the current graph, which a longer journal here also has more of.
    for (name, short, long) in [
        ("a current read", short_read, long_read),
        ("an append", short_append, long_append),
    ] {
        assert_eq!(
            short.queries, long.queries,
            "{name} must not grow with history"
        );
        assert_eq!(short.rows, long.rows, "{name} must not grow with history");
    }
    assert!(
        long_read.rows <= 12 && long_append.rows <= 24,
        "an operation reads a handful of rows, not the journal: read {long_read:?}, append {long_append:?}"
    );
    let head_bytes: i64 = long_store.inspect(|connection| {
        connection
            .query_row(
                "SELECT length(head_graph) FROM work_product_journal_head_v1",
                [],
                |row| row.get(0),
            )
            .unwrap()
    });
    let head_bytes = u64::try_from(head_bytes).unwrap();
    assert!(
        long_read.bytes <= 2 * head_bytes && long_append.bytes <= 4 * head_bytes,
        "an operation reads the current graph and not the events behind it: \
         read {long_read:?}, append {long_append:?}, head {head_bytes} bytes"
    );

    // Version 200 of the long journal is rebuilt from the checkpoint at 128.
    let (old, rebuild) = measure(|| {
        read_at(&long_store, repository_selection(), "as_of", 299, 8_000)
            .expect("a rebuilt version")
    });
    assert_eq!(
        only_entry(&old).verified_version().graph_version().get(),
        200
    );
    assert!(
        rebuild.rows < 128 + 12,
        "a rebuild reads the events since its checkpoint, not the journal: {rebuild:?}"
    );
}

/// A version rebuilt from a checkpoint plus the events after it is the version
/// a replay of the whole journal produces, on both sides of every checkpoint
/// boundary and under both the narrow and the wide selection.
#[test]
fn historical_reconstruction_agrees_with_full_replay_across_checkpoints() {
    let (store, _) = serviced_journal("journal-replay-agreement", 299);
    let replay = full_replay(&stored_events(&store));
    assert_eq!(replay.len(), 300);
    assert_eq!(store.count("work_product_journal_checkpoints_v1"), 2);
    for version in [1u64, 2, 127, 128, 129, 130, 255, 256, 257, 299, 300] {
        // The service journal places version `n` at instant `99 + n`.
        let read = read_at(
            &store,
            repository_selection(),
            "as_of",
            99 + i64::try_from(version).unwrap(),
            PROJECTED_AT.0,
        )
        .expect("a historical read");
        let entry = only_entry(&read);
        assert_eq!(entry.verified_version().graph_version().get(), version);
        assert_eq!(entry.graph(), &replay[&version], "version {version}");
        assert_eq!(
            entry.verified_version().recovered_graph_digest().as_str(),
            graph_digest(&replay[&version])
        );
    }

    // The whole timeline in one page agrees with the replay entry by entry.
    let evolution = reads(&store)
        .read_graph(
            &context(),
            WorkGraphReadRequestV1::evolution(
                repository_selection(),
                UtcMicros(0),
                PROJECTED_AT,
                PROJECTED_AT,
            )
            .unwrap(),
        )
        .expect("the evolution");
    assert_eq!(evolution.entries().len(), 300);
    for entry in evolution.entries() {
        assert_eq!(
            entry.graph(),
            &replay[&entry.verified_version().graph_version().get()]
        );
    }
}

/// The narrow selection reads a prefix of a journal that also holds repository
/// scoped events; its rebuilt versions must agree with the replay of that
/// prefix, and the wide selection with the replay of the whole.
#[test]
fn a_narrow_selection_rebuilds_its_prefix_across_a_checkpoint_boundary() {
    let store = RegisteredWorkStore::start("journal-narrow-prefix");
    let no_git = WorkProductSelectionScopeV1::ProfileOwnedNoGit;
    let repository = repository_selection();
    let mut latest = mutations(&store)
        .create(
            &context(),
            &binding(),
            CreateWorkProductRequestV1 {
                selection: no_git.clone(),
                initial_graph: graph(vec![item(TASK, &[], 1)]),
                mutation: mutation("command.journal-state.create", UtcMicros(100)),
            },
        )
        .expect("create the no-Git work");
    for index in 1..150 {
        latest = append_task(
            &store,
            &no_git,
            &format!("no-git.{index}"),
            100 + index,
            latest.verified_graph_version(),
        )
        .expect("append no-Git work");
    }
    for index in 150..170 {
        latest = append_task(
            &store,
            &repository,
            &format!("scoped.{index}"),
            100 + index,
            latest.verified_graph_version(),
        )
        .expect("append repository-scoped work beside it");
    }
    let replay = full_replay(&stored_events(&store));
    assert_eq!(replay.len(), 170);

    // Wide: every version of the whole journal.
    for version in [127u64, 128, 129, 150, 151, 170] {
        let read = read_at(
            &store,
            repository.clone(),
            "as_of",
            99 + i64::try_from(version).unwrap(),
            PROJECTED_AT.0,
        )
        .unwrap();
        assert_eq!(
            only_entry(&read).graph(),
            &replay[&version],
            "wide {version}"
        );
    }
    // Narrow: only the no-Git prefix is readable, and it says so.
    for version in [127u64, 128, 129, 150] {
        let read = read_at(
            &store,
            no_git.clone(),
            "as_of",
            99 + i64::try_from(version).unwrap(),
            PROJECTED_AT.0,
        )
        .unwrap();
        assert_eq!(
            only_entry(&read).graph(),
            &replay[&version],
            "narrow {version}"
        );
    }
    let narrow = read_at(&store, no_git, "current", 0, PROJECTED_AT.0).unwrap();
    assert_eq!(
        only_entry(&narrow).verified_version().graph_version().get(),
        150
    );
    assert!(matches!(
        narrow.selection_coverage(),
        WorkGraphSelectionCoverageV1::Partial {
            covered_events: 150,
            excluded_events: 20,
            ..
        }
    ));
}

/// The conversion runs in bounded batches, each its own transaction, so an
/// interruption resumes where it stopped; until it finishes every Work product
/// operation answers busy instead of reading an owner as empty.
#[test]
fn a_legacy_journal_converts_in_batches_and_resumes_after_an_interruption() {
    let chain = repository_chain(600, 0);
    let store = RegisteredWorkStore::start_seeded("journal-conversion-resume", |connection| {
        seed_legacy(connection, &chain);
        install_work_schema(connection).expect("install the current schema");
        // The second batch of 256 ends at 512, and its head write is refused.
        connection
            .execute_batch(
                "CREATE TRIGGER interrupt_the_second_batch
                 BEFORE UPDATE ON work_product_journal_head_v1
                 WHEN NEW.head_version = 512
                 BEGIN SELECT RAISE(ABORT, 'interrupted'); END;",
            )
            .expect("install the interruption");
    });

    let interrupted = converge(&store).expect_err("the second batch is interrupted");
    assert!(
        interrupted.to_string().contains("conversion"),
        "{interrupted}"
    );
    // One whole batch committed; nothing past it did.
    let remaining: i64 = store.count("work_product_events_v1");
    let converted: i64 = store.count("work_product_journal_events_v1");
    assert_eq!((remaining, converted), (344, 256));
    let head_version: i64 = store.inspect(|connection| {
        connection
            .query_row(
                "SELECT head_version FROM work_product_journal_head_v1",
                [],
                |row| row.get(0),
            )
            .unwrap()
    });
    assert_eq!(head_version, 256);

    // Mid-conversion the journal is closed, not empty.
    assert_eq!(
        read_at(&store, repository_selection(), "current", 0, PROJECTED_AT.0).unwrap_err(),
        WorkProductApplicationErrorV1::Storage(WorkStorageFailureV1::Busy)
    );
    let busy = append_task(
        &store,
        &repository_selection(),
        "too-early",
        300,
        &tracedecay_contracts::VerifiedWorkGraphVersionV1::new(
            WorkGraphVersionV1::new(1).unwrap(),
            WorkProductEventSequenceV1::new(1).unwrap(),
            watermark(),
            digest('1'),
        )
        .unwrap(),
    )
    .unwrap_err();
    assert_eq!(
        busy,
        WorkProductApplicationErrorV1::Storage(WorkStorageFailureV1::Busy)
    );

    store.inspect(|connection| {
        connection
            .execute_batch("DROP TRIGGER interrupt_the_second_batch")
            .expect("remove the interruption");
    });
    converge(&store).expect("the conversion resumes and finishes");
    converge(&store).expect("converting a converted journal is a no-op");

    let tables: i64 = store.inspect(|connection| {
        connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE name IN ('work_product_events_v1', 'work_product_graph_versions_v1')",
                [],
                |row| row.get(0),
            )
            .unwrap()
    });
    assert_eq!(
        tables, 0,
        "the legacy tables are gone once nothing is left in them"
    );
    assert_eq!(store.count("work_product_journal_events_v1"), 600);

    let store = store.restart("journal-conversion-resume");
    let head = read_current(&store).expect("the converted journal reads");
    let head = only_entry(&head);
    assert_eq!(head.verified_version().graph_version().get(), 600);
    assert_eq!(head.graph().item(&id(TASK)).unwrap().handoffs().len(), 599);
    let replay = full_replay(&stored_events(&store));
    assert_eq!(head.graph(), &replay[&600]);
}

/// A chain that does not verify stops the conversion and leaves every legacy
/// row where it was, with the journal still closed, instead of converting a
/// graph nobody can prove.
#[test]
fn a_legacy_chain_that_does_not_verify_is_not_converted() {
    let length = 300;
    let mut chain = repository_chain(length, 0);
    // The checkpoint version's recorded digest no longer matches its graph.
    chain.versions[127].2 = digest('9').as_str().to_owned();
    let store = RegisteredWorkStore::start_seeded("journal-conversion-corrupt", |connection| {
        seed_legacy(connection, &chain);
    });

    let error = converge(&store).expect_err("a digest that does not match is not converted");
    assert!(error.to_string().contains("did not verify"), "{error}");
    assert_eq!(store.count("work_product_events_v1"), 300);
    assert_eq!(store.count("work_product_journal_events_v1"), 0);
    assert_eq!(
        read_at(&store, repository_selection(), "current", 0, PROJECTED_AT.0).unwrap_err(),
        WorkProductApplicationErrorV1::Storage(WorkStorageFailureV1::Busy)
    );
}
