//! The Work product graph authority, end to end over the registered store.
//!
//! This suite drives the REAL composition, the application's
//! `WorkProductMutationServiceV1` and `WorkProductReadServiceV1` over the
//! registered exact-SQL storage, with no port doubles. A suite that
//! substituted its own port would not prove the registered-store path.
//!
//! The assertions are about truthfulness as much as about persistence. The
//! Work views draw effort, concurrency, churn, and a critical path, and every
//! one of those is computed from `WorkItemV1::effort`, which the domain
//! refuses to let be zero. So the test declares effort explicitly and then
//! asserts the projections carry back exactly the declared numbers: if any
//! layer ever starts estimating one, these equalities break.

#[path = "../support/work_product_graph.rs"]
mod work_product_graph;

use std::collections::BTreeSet;

use tracedecay_contracts::{
    AddWorkTaskRequestV1, CreateWorkProductRequestV1, DecideWorkProposalRequestV1, OpaqueCursor,
    PrefetchedWorkGraphReadV1, WorkGraphReadRequestV1, WorkGraphReadV1,
    WorkGraphSelectionCoverageV1, WorkProductApplicationErrorV1,
    WorkProductAuthorizedRelationScopeV1, WorkProductExpectedAuthorityV1,
    WorkProductMutationIdentityV1, WorkProductMutationRequestV1, WorkProductMutationServiceV1,
    WorkProductSelectionScopeV1, WorkStorageFailureV1,
};
use tracedecay_domain::{
    AcceptanceCriterionId, CursorBindingMismatchV1, MAX_WORK_PRODUCT_TEXT_BYTES, ProjectId,
    ProposalId, TaskId, UtcMicros, WorkAcceptanceCriterionV1, WorkGraphVersionV1, WorkItemInputV1,
    WorkItemV1, WorkProductEventPayloadV1, WorkProductEventSequenceV1, WorkProductEventV1,
    WorkProductGraphV1, WorkProposalDispositionV1, WorkProposalV1, WorkRouteDecisionV1,
    WorkRuntimeProjectionCoverageV1, WorkScoreKindV1, WorkShapeAssessmentV1, WorkSizingV1,
    canonical_sha256,
};

use crate::work_registered_store::RegisteredWorkStore;

use tracedecay_domain::test_fixtures::{digest, id};

use work_product_graph::{
    PROJECT, PROJECTED_AT, REPOSITORY, binding, context, create, graph, item, mutation, mutations,
    read_current, reads, repository_selection,
};

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

    let failure = create(
        &store,
        "command.work-product.atomic-rollback",
        UtcMicros(100),
        vec![item("task.only", &[], 2)],
    )
    .expect_err("the verified graph failure must reject the whole atomic append");

    // A trigger abort is a storage failure, not an expected-revision or
    // uniqueness conflict: it must not tell the caller its graph is stale.
    assert_eq!(
        failure,
        WorkProductApplicationErrorV1::EventAuthorityUnavailable
    );
    assert_eq!(store.count("work_product_events_v1"), 0);
    assert_eq!(store.count("work_product_graph_versions_v1"), 0);
}

#[test]
fn a_uniqueness_violation_on_the_journal_insert_is_a_version_conflict() {
    // Another writer claims the sequence between this append's tail read and
    // its insert, which the primary key turns into a constraint violation.
    let store =
        RegisteredWorkStore::start_with_setup("work-product-insert-conflict", |connection| {
            connection
                .execute_batch(
                    "CREATE TRIGGER rival_claims_sequence
                     BEFORE INSERT ON work_product_events_v1
                     BEGIN
                       INSERT INTO work_product_events_v1 (
                         owner_brain_id, owner_profile_id, sequence, event_id, command_id,
                         canonical_input_digest, expected_graph_version, result_graph_version,
                         occurred_at, event_payload
                       ) VALUES (
                         NEW.owner_brain_id, NEW.owner_profile_id, NEW.sequence, 'event.rival',
                         'command.rival', 'digest.rival', NULL, 9999, 0, '{}'
                       );
                     END;",
                )
                .expect("install rival writer trigger");
        });

    let conflict = create(
        &store,
        "command.work-product.insert-conflict",
        UtcMicros(100),
        vec![item("task.only", &[], 2)],
    )
    .expect_err("a lost race for the sequence must reject the append");

    assert_eq!(conflict, WorkProductApplicationErrorV1::VersionConflict);
    assert_eq!(store.count("work_product_events_v1"), 0);
    assert_eq!(store.count("work_product_graph_versions_v1"), 0);
}

#[test]
fn a_journal_row_that_does_not_decode_is_invalid_stored_data_not_an_absent_graph() {
    let store = RegisteredWorkStore::start("work-product-undecodable-journal");
    let first = create(
        &store,
        "command.work-product.undecodable-journal",
        UtcMicros(100),
        vec![item("task.only", &[], 2)],
    )
    .expect("create the work product");
    assert!(read_current(&store).is_ok());

    store.inspect(|connection| {
        connection
            .execute(
                "UPDATE work_product_events_v1 SET event_payload = 'not json'",
                [],
            )
            .expect("corrupt the stored event payload");
    });

    assert_eq!(
        read_current(&store).expect_err("an undecodable journal is not a readable graph"),
        WorkProductApplicationErrorV1::Storage(WorkStorageFailureV1::StoredDataInvalid)
    );
    // The mutation path reads the same journal and must say the same thing,
    // rather than describing the corrupt store as a stale graph.
    let mut next = mutation(
        "command.work-product.undecodable-journal.add",
        UtcMicros(200),
    );
    next.expected_authority = WorkProductExpectedAuthorityV1::Verified {
        verified_version: first.verified_graph_version().clone(),
    };
    let append = mutations(&store)
        .add_task(
            &context(),
            &binding(),
            AddWorkTaskRequestV1 {
                selection: repository_selection(),
                item: item("task.other", &[], 2),
                mutation: next,
            },
        )
        .expect_err("an undecodable journal cannot be appended to");
    assert_eq!(
        append,
        WorkProductApplicationErrorV1::Storage(WorkStorageFailureV1::StoredDataInvalid)
    );
    assert_eq!(store.count("work_product_events_v1"), 1);
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
fn current_reads_retain_exact_contents_across_published_history() {
    let store = RegisteredWorkStore::start("work-product-current-history");
    let mut latest = create(
        &store,
        "command.work-product.current-history.create",
        UtcMicros(100),
        vec![item("task.history.0", &[], 2)],
    )
    .expect("publish initial graph");
    let first = read_current(&store).expect("read initial graph");
    assert_eq!(
        first.entries()[0].graph().items()[0],
        item("task.history.0", &[], 2)
    );

    for index in 1..32 {
        let mut change = mutation(
            &format!("command.work-product.current-history.add.{index}"),
            UtcMicros(100 + index),
        );
        change.expected_authority = WorkProductExpectedAuthorityV1::Verified {
            verified_version: latest.verified_graph_version().clone(),
        };
        latest = mutations(&store)
            .add_task(
                &context(),
                &binding(),
                AddWorkTaskRequestV1 {
                    selection: repository_selection(),
                    item: item(&format!("task.history.{index}"), &[], 2),
                    mutation: change,
                },
            )
            .expect("publish next graph");
    }

    let current = read_current(&store).expect("read latest graph across history");
    let snapshot = &current.entries()[0];
    assert_eq!(snapshot.verified_version(), latest.verified_graph_version());
    assert_eq!(snapshot.graph().items().len(), 32);
    for index in 0..32 {
        let task = id::<TaskId>(&format!("task.history.{index}"));
        assert_eq!(
            snapshot.graph().item(&task).expect("published task"),
            &item(&format!("task.history.{index}"), &[], 2)
        );
    }
    let earlier = reads(&store)
        .read_graph(
            &context(),
            WorkGraphReadRequestV1::current(repository_selection(), UtcMicros(115)),
        )
        .expect("read only versions observed before the cutoff");
    assert_eq!(earlier.entries()[0].graph().items().len(), 16);
    assert!(
        earlier.entries()[0]
            .graph()
            .item(&id("task.history.16"))
            .is_none()
    );
    store.inspect(|connection| {
        connection
            .execute(
                "UPDATE work_product_events_v1 SET event_payload = json_set(event_payload, '$.sequence', 99) WHERE sequence = 1",
                [],
            )
            .expect("change an earlier event's embedded sequence");
    });
    assert_eq!(
        read_current(&store).expect_err("a mismatched historical event sequence is unavailable"),
        WorkProductApplicationErrorV1::GraphAuthorityUnavailable
    );
    store.inspect(|connection| {
        connection
            .execute(
                "UPDATE work_product_events_v1 SET event_payload = json_set(event_payload, '$.sequence', 1) WHERE sequence = 1",
                [],
            )
            .expect("restore the historical event's sequence");
    });
    assert_eq!(
        read_current(&store)
            .expect("the canonical sequence restores graph availability")
            .entries()[0]
            .verified_version(),
        latest.verified_graph_version()
    );
    store.inspect(|connection| {
        connection
            .execute(
                "UPDATE work_product_graph_versions_v1 SET recovered_graph_digest = 'invalid' WHERE graph_version = 1",
                [],
            )
            .expect("corrupt an earlier published identity");
    });
    assert_eq!(
        read_current(&store).expect_err("invalid historical publication remains unavailable"),
        WorkProductApplicationErrorV1::GraphAuthorityUnavailable
    );
}

#[test]
fn appends_resume_from_a_checkpoint_and_survive_restart_across_checkpoint_boundaries() {
    const EVENTS: i64 = 600;
    let store = RegisteredWorkStore::start("work-product-checkpoints");
    let mut latest = create(
        &store,
        "command.work-product.checkpoints.create",
        UtcMicros(100),
        vec![item("task.checkpoint.0", &[], 1)],
    )
    .expect("publish initial graph");
    for index in 1..EVENTS {
        let mut change = mutation(
            &format!("command.work-product.checkpoints.add.{index}"),
            UtcMicros(100 + index),
        );
        change.expected_authority = WorkProductExpectedAuthorityV1::Verified {
            verified_version: latest.verified_graph_version().clone(),
        };
        latest = mutations(&store)
            .add_task(
                &context(),
                &binding(),
                AddWorkTaskRequestV1 {
                    selection: repository_selection(),
                    item: item(&format!("task.checkpoint.{index}"), &[], 1),
                    mutation: change,
                },
            )
            .expect("publish next graph");
    }
    assert_eq!(store.count("work_product_events_v1"), EVENTS);
    assert_eq!(store.count("work_product_graph_versions_v1"), EVENTS);
    assert_eq!(store.count("work_product_graph_checkpoints_v1"), 2);

    let store = store.restart("work-product-checkpoints");
    let current = reads(&store)
        .read_graph(
            &context(),
            WorkGraphReadRequestV1::current(repository_selection(), UtcMicros(1_000)),
        )
        .expect("read current after restart at an observation after the journal tail");
    assert_eq!(
        current.entries()[0].verified_version(),
        latest.verified_graph_version()
    );
    assert_eq!(current.entries()[0].graph().items().len(), 600);
    for sequence in [255, 256, 257, 512, 513] {
        let historical = reads(&store)
            .read_graph(
                &context(),
                WorkGraphReadRequestV1::as_of(
                    repository_selection(),
                    UtcMicros(99 + sequence),
                    UtcMicros(1_000),
                )
                .unwrap(),
            )
            .expect("historical reads remain exact on either side of a checkpoint");
        assert_eq!(
            historical.entries()[0]
                .verified_version()
                .event_sequence()
                .get(),
            sequence as u64
        );
        assert_eq!(
            historical.entries()[0].graph().items().len(),
            sequence as usize
        );
    }

    let mut change = mutation("command.work-product.checkpoints.after", UtcMicros(2_000));
    change.expected_authority = WorkProductExpectedAuthorityV1::Verified {
        verified_version: latest.verified_graph_version().clone(),
    };
    let next = AddWorkTaskRequestV1 {
        selection: repository_selection(),
        item: item("task.checkpoint.after", &[], 1),
        mutation: change,
    };
    let refuses_checkpoint = || {
        assert_eq!(
            mutations(&store)
                .add_task(&context(), &binding(), next.clone())
                .expect_err("a damaged checkpoint is never a fold base"),
            WorkProductApplicationErrorV1::Storage(WorkStorageFailureV1::StoredDataInvalid)
        );
        assert_eq!(store.count("work_product_events_v1"), EVENTS);
        assert_eq!(store.count("work_product_graph_versions_v1"), EVENTS);
    };

    // Even valid JSON with the expected length must match the graph digest.
    store.inspect(|connection| {
        assert_eq!(
            connection
                .execute(
                    "UPDATE work_product_graph_checkpoint_chunks_v1
                     SET chunk_payload = CAST(replace(CAST(chunk_payload AS TEXT),
                         'Deliver task.checkpoint.0', 'Xeliver task.checkpoint.0') AS BLOB)
                     WHERE event_sequence = 512 AND chunk_index = 0
                       AND instr(CAST(chunk_payload AS TEXT), 'Deliver task.checkpoint.0') > 0",
                    [],
                )
                .unwrap(),
            1
        );
    });
    refuses_checkpoint();
    store.inspect(|connection| {
        connection
            .execute(
                "UPDATE work_product_graph_checkpoint_chunks_v1
                 SET chunk_payload = CAST(replace(CAST(chunk_payload AS TEXT),
                     'Xeliver task.checkpoint.0', 'Deliver task.checkpoint.0') AS BLOB)
                 WHERE event_sequence = 512 AND chunk_index = 0",
                [],
            )
            .unwrap();
    });

    // A missing first chunk, a mismatched count, and a truncated payload all
    // refuse the append without committing its event or publication.
    store.inspect(|connection| {
        connection
            .execute(
                "UPDATE work_product_graph_checkpoint_chunks_v1 SET chunk_index = 999
                 WHERE event_sequence = 512 AND chunk_index = 0",
                [],
            )
            .unwrap();
    });
    refuses_checkpoint();
    store.inspect(|connection| {
        connection
            .execute_batch(
                "UPDATE work_product_graph_checkpoint_chunks_v1 SET chunk_index = 0
                 WHERE event_sequence = 512 AND chunk_index = 999;
                 UPDATE work_product_graph_checkpoints_v1 SET chunk_count = chunk_count + 1
                 WHERE event_sequence = 512;",
            )
            .unwrap();
    });
    refuses_checkpoint();
    store.inspect(|connection| {
        connection
            .execute_batch(
                "UPDATE work_product_graph_checkpoints_v1
                 SET chunk_count = chunk_count - 1, payload_bytes = payload_bytes + 1
                 WHERE event_sequence = 512;",
            )
            .unwrap();
    });
    refuses_checkpoint();
    store.inspect(|connection| {
        connection
            .execute_batch(
                "UPDATE work_product_graph_checkpoints_v1 SET payload_bytes = payload_bytes - 1
                 WHERE event_sequence = 512;
                 UPDATE work_product_graph_checkpoints_v1 SET recovered_graph_digest = 'invalid';",
            )
            .unwrap();
    });
    refuses_checkpoint();

    // A cache can be internally consistent without representing the graph
    // that the journal published. Its header must also match that publication.
    let canonical = current.entries()[0].graph();
    let forged = WorkProductGraphV1::new(
        WorkGraphVersionV1::new(512).unwrap(),
        canonical.initiatives().to_vec(),
        canonical.plans().to_vec(),
        canonical.milestones().to_vec(),
        vec![item("task.forged", &[], 1)],
    )
    .unwrap();
    let forged_digest =
        canonical_sha256(&("tracedecay.rusqlite-runtime.work-product-graph.v1", &forged)).unwrap();
    let payload = serde_json::to_vec(&forged).unwrap();
    store.inspect(|connection| {
        connection
            .execute(
                "DELETE FROM work_product_graph_checkpoint_chunks_v1 WHERE event_sequence = 512",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO work_product_graph_checkpoint_chunks_v1
                 SELECT owner_brain_id, owner_profile_id, event_sequence, 0, ?1
                 FROM work_product_graph_checkpoints_v1 WHERE event_sequence = 512",
                [&payload],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE work_product_graph_checkpoints_v1
                 SET recovered_graph_digest = ?1, payload_bytes = ?2, chunk_count = 1
                 WHERE event_sequence = 512",
                rusqlite::params![forged_digest.as_str(), payload.len() as i64],
            )
            .unwrap();
    });
    refuses_checkpoint();

    // The daemon can prepare a valid current read before entering the writer.
    // Corrupting the prefix after that read must still be caught by the real
    // writer, even though the application receives its valid pre-read.
    let request = WorkProductMutationRequestV1::AddTask(Box::new(next));
    let prefetched = PrefetchedWorkGraphReadV1::for_mutation(store.storage(), &context(), &request);
    store.inspect(|connection| {
        connection
            .execute(
                "UPDATE work_product_events_v1
                 SET event_payload = json_set(event_payload, '$.sequence', 99)
                 WHERE sequence = 1",
                [],
            )
            .unwrap();
    });
    assert_eq!(store.count("work_product_graph_checkpoints_v1"), 0);
    assert_eq!(store.count("work_product_graph_checkpoint_chunks_v1"), 0);
    assert_eq!(
        reads(&store)
            .read_graph(
                &context(),
                WorkGraphReadRequestV1::current(repository_selection(), UtcMicros(2_000)),
            )
            .expect_err("the full reader rejects the changed historical sequence"),
        WorkProductApplicationErrorV1::GraphAuthorityUnavailable
    );
    let revisions = request.mutation_identity().revisions.clone();
    assert_eq!(
        WorkProductMutationServiceV1::new(
            prefetched,
            store.storage().clone(),
            store.storage().clone(),
        )
        .mutate(&context(), &binding(), request.clone(), &revisions)
        .expect_err("the writer must independently reject the corrupted prefix"),
        WorkProductApplicationErrorV1::EventAuthorityUnavailable
    );
    assert_eq!(store.count("work_product_events_v1"), EVENTS);
    assert_eq!(store.count("work_product_graph_versions_v1"), EVENTS);

    store.inspect(|connection| {
        connection
            .execute(
                "UPDATE work_product_events_v1
                 SET event_payload = json_set(event_payload, '$.sequence', 1)
                 WHERE sequence = 1",
                [],
            )
            .unwrap();
    });
    let prefetched = PrefetchedWorkGraphReadV1::for_mutation(store.storage(), &context(), &request);
    store.inspect(|connection| {
        assert_eq!(
            connection
                .execute(
                    "UPDATE work_product_events_v1
                     SET event_payload = json_set(event_payload,
                         '$.payload.graph.items[0].input.title', 'Changed historical title')
                     WHERE sequence = 1
                       AND json_extract(event_payload, '$.payload.graph.items[0].input.title')
                           = 'Deliver task.checkpoint.0'",
                    [],
                )
                .unwrap(),
            1
        );
    });
    assert_eq!(
        reads(&store)
            .read_graph(
                &context(),
                WorkGraphReadRequestV1::current(repository_selection(), UtcMicros(2_000)),
            )
            .expect_err("a valid graph with changed contents does not match its publication"),
        WorkProductApplicationErrorV1::GraphAuthorityUnavailable
    );
    assert_eq!(
        WorkProductMutationServiceV1::new(
            prefetched,
            store.storage().clone(),
            store.storage().clone(),
        )
        .mutate(&context(), &binding(), request, &revisions)
        .expect_err("the writer verifies the recovered head before extending changed history"),
        WorkProductApplicationErrorV1::EventAuthorityUnavailable
    );
    assert_eq!(store.count("work_product_events_v1"), EVENTS);
    assert_eq!(store.count("work_product_graph_versions_v1"), EVENTS);
}

#[test]
fn historical_changes_overwritten_by_later_events_still_block_new_publications() {
    let store = RegisteredWorkStore::start("work-product-overwritten-history");
    let created = create(
        &store,
        "command.work-product.overwritten.create",
        UtcMicros(100),
        vec![item("task.overwritten", &[], 1)],
    )
    .unwrap();
    let mut change = mutation("command.work-product.overwritten.accept", UtcMicros(150));
    change.expected_authority = WorkProductExpectedAuthorityV1::Verified {
        verified_version: created.verified_graph_version().clone(),
    };
    let accepted = mutations(&store)
        .decide_proposal(
            &context(),
            &binding(),
            DecideWorkProposalRequestV1 {
                selection: repository_selection(),
                proposal: WorkProposalV1::new(
                    id::<ProposalId>("proposal.overwritten"),
                    id("task.overwritten"),
                    WorkGraphVersionV1::initial(),
                    WorkShapeAssessmentV1::new(WorkScoreKindV1::Ordinal, 1, 1, 1, 1).unwrap(),
                    WorkSizingV1::new(WorkScoreKindV1::Ordinal, 1, 1, 1, "complete").unwrap(),
                    Vec::new(),
                    WorkRouteDecisionV1::abstain("no execution requested").unwrap(),
                    "accept the task proposal".to_owned(),
                    digest('a'),
                    digest('b'),
                )
                .unwrap(),
                disposition: WorkProposalDispositionV1::Accepted,
                mutation: change,
            },
        )
        .unwrap();
    let original = read_current(&store).unwrap();
    assert_eq!(store.count("work_product_graph_checkpoints_v1"), 0);
    assert_eq!(store.count("work_product_journal_invalidations_v1"), 0);
    let mut change = mutation("command.work-product.overwritten.after", UtcMicros(200));
    change.expected_authority = WorkProductExpectedAuthorityV1::Verified {
        verified_version: accepted.verified_graph_version().clone(),
    };
    let request = WorkProductMutationRequestV1::AddTask(Box::new(AddWorkTaskRequestV1 {
        selection: repository_selection(),
        item: item("task.overwritten.after", &[], 1),
        mutation: change,
    }));
    let prefetched = PrefetchedWorkGraphReadV1::for_mutation(store.storage(), &context(), &request);
    let altered: WorkProductEventV1 = store.inspect(|connection| {
        connection
            .execute(
                "UPDATE work_product_events_v1
                 SET event_payload = json_set(event_payload,
                     '$.payload.graph.items[0].input.updated_at', 11)
                 WHERE sequence = 1",
                [],
            )
            .unwrap();
        let payload: String = connection
            .query_row(
                "SELECT event_payload FROM work_product_events_v1 WHERE sequence = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str(&payload).unwrap()
    });
    let WorkProductEventPayloadV1::Created { graph } = altered.payload() else {
        panic!("the first event creates the graph");
    };
    let WorkProductEventPayloadV1::Changed { change } = accepted.event().payload() else {
        panic!("accepting the proposal changes the graph");
    };
    let recovered_head = graph.clone().apply(change.as_ref().clone()).unwrap();
    assert_eq!(
        &recovered_head,
        original.entries()[0].graph(),
        "the later acceptance overwrites updated_at, leaving the exact current graph unchanged"
    );
    assert_eq!(store.count("work_product_journal_invalidations_v1"), 1);
    assert_eq!(
        read_current(&store).expect_err("a matching head cannot verify the changed first version"),
        WorkProductApplicationErrorV1::GraphAuthorityUnavailable
    );
    let revisions = request.mutation_identity().revisions.clone();
    assert_eq!(
        WorkProductMutationServiceV1::new(
            prefetched,
            store.storage().clone(),
            store.storage().clone(),
        )
        .mutate(&context(), &binding(), request.clone(), &revisions)
        .expect_err("the writer checks every affected publication, even without a checkpoint"),
        WorkProductApplicationErrorV1::EventAuthorityUnavailable
    );
    assert_eq!(store.count("work_product_events_v1"), 2);
    assert_eq!(store.count("work_product_graph_versions_v1"), 2);
    assert_eq!(store.count("work_product_journal_invalidations_v1"), 1);

    store.inspect(|connection| {
        connection
            .execute(
                "UPDATE work_product_events_v1
                 SET event_payload = json_set(event_payload,
                     '$.payload.graph.items[0].input.updated_at', 10)
                 WHERE sequence = 1",
                [],
            )
            .unwrap();
    });
    mutations(&store)
        .mutate(&context(), &binding(), request, &revisions)
        .expect("restored history is verified before its marker is cleared with the next commit");
    assert_eq!(store.count("work_product_events_v1"), 3);
    assert_eq!(store.count("work_product_graph_versions_v1"), 3);
    assert_eq!(store.count("work_product_journal_invalidations_v1"), 0);
    store.inspect(|connection| {
        connection
            .execute(
                "DELETE FROM work_product_graph_versions_v1 WHERE event_sequence = 2",
                [],
            )
            .unwrap();
    });
    assert_eq!(
        read_current(&store).expect_err("a missing intermediate publication is not skipped"),
        WorkProductApplicationErrorV1::GraphAuthorityUnavailable
    );
}

#[test]
fn a_graph_larger_than_the_sql_transport_limit_checkpoints_and_resumes_after_restart() {
    const CRITERIA: usize = 4_096;
    let store = RegisteredWorkStore::start("work-product-large-checkpoint");
    let mut latest = create(
        &store,
        "command.work-product.large-checkpoint.create",
        UtcMicros(100),
        vec![item("task.large-checkpoint.0", &[], 1)],
    )
    .unwrap();
    // Put the large, individually legal events at the end of the interval so
    // this checks the real 64 MiB boundary without replaying a large graph for
    // hundreds of setup mutations.
    for index in 1..256 {
        let task = format!("task.large-checkpoint.{index}");
        let small = item(&task, &[], 1);
        let next_item = if index >= 252 {
            WorkItemV1::new(WorkItemInputV1 {
                task_id: small.task_id().clone(),
                hierarchy: small.hierarchy().clone(),
                title: small.title().to_owned(),
                dependencies: BTreeSet::new(),
                informational_relations: BTreeSet::new(),
                causal_candidates: BTreeSet::new(),
                acceptance_criteria: (0..CRITERIA)
                    .map(|criterion| {
                        WorkAcceptanceCriterionV1::new(
                            id::<AcceptanceCriterionId>(&format!("criterion.{task}.{criterion}")),
                            "x".repeat(MAX_WORK_PRODUCT_TEXT_BYTES),
                            true,
                        )
                        .unwrap()
                    })
                    .collect(),
                effort: 1,
                scheduled_at: None,
                deadline: Some(UtcMicros(1_000)),
                created_at: UtcMicros(10),
                updated_at: UtcMicros(10),
            })
            .unwrap()
        } else {
            small
        };
        let mut change = mutation(
            &format!("command.work-product.large-checkpoint.add.{index}"),
            UtcMicros(100 + index),
        );
        change.expected_authority = WorkProductExpectedAuthorityV1::Verified {
            verified_version: latest.verified_graph_version().clone(),
        };
        latest = mutations(&store)
            .add_task(
                &context(),
                &binding(),
                AddWorkTaskRequestV1 {
                    selection: repository_selection(),
                    item: next_item,
                    mutation: change,
                },
            )
            .expect("an individually legal event can cross a large checkpoint boundary");
    }
    let (payload_bytes, chunk_count, largest_chunk): (i64, i64, i64) =
        store.inspect(|connection| {
            connection
                .query_row(
                    "SELECT checkpoint.payload_bytes, checkpoint.chunk_count,
                        max(length(chunk.chunk_payload))
                 FROM work_product_graph_checkpoints_v1 AS checkpoint
                 JOIN work_product_graph_checkpoint_chunks_v1 AS chunk
                   USING (owner_brain_id, owner_profile_id, event_sequence)
                 WHERE checkpoint.event_sequence = 256",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap()
        });
    assert!(payload_bytes > 64 * 1024 * 1024);
    assert!(chunk_count > 64);
    assert!(largest_chunk <= 1024 * 1024);
    assert_eq!(store.count("work_product_events_v1"), 256);
    assert_eq!(store.count("work_product_graph_versions_v1"), 256);

    let store = store.restart("work-product-large-checkpoint");
    let mut change = mutation(
        "command.work-product.large-checkpoint.after",
        UtcMicros(356),
    );
    change.expected_authority = WorkProductExpectedAuthorityV1::Verified {
        verified_version: latest.verified_graph_version().clone(),
    };
    let appended = mutations(&store)
        .add_task(
            &context(),
            &binding(),
            AddWorkTaskRequestV1 {
                selection: repository_selection(),
                item: item("task.large-checkpoint.after", &[], 1),
                mutation: change,
            },
        )
        .expect("a large checkpoint resumes through bounded SQL results after restart");
    let current = read_current(&store).expect("large journal pages remain readable in full");
    assert_eq!(
        current.entries()[0].verified_version(),
        appended.verified_graph_version()
    );
    assert_eq!(current.entries()[0].graph().items().len(), 257);
    for index in 252..256 {
        assert_eq!(
            current.entries()[0]
                .graph()
                .item(&id(&format!("task.large-checkpoint.{index}")))
                .unwrap()
                .acceptance_criteria()
                .len(),
            CRITERIA
        );
    }
}

#[test]
fn an_owner_with_no_journal_has_an_absent_current_graph_and_an_explicitly_empty_timeline() {
    let store = RegisteredWorkStore::start("work-product-empty");

    // A point read of a version that was never published is an absence, not a
    // zero: a verified version identity requires a real event sequence, so
    // there is no representable empty current graph. The caller is
    // authorized, so the absence is an answer under its scope, not a denial.
    let WorkGraphReadV1::Absent {
        authorized_scope,
        selection_coverage,
    } = read_current(&store).expect("an authorized read of no graph is answered")
    else {
        panic!("an unpublished graph must read as absent");
    };
    assert_eq!(authorized_scope.selection(), &repository_selection());
    assert_eq!(
        selection_coverage,
        WorkGraphSelectionCoverageV1::Complete { covered_events: 0 }
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
        WorkProductAuthorizedRelationScopeV1::Repository {
            project_id: id(PROJECT),
            repository_id: id(REPOSITORY),
        },
        WorkProductAuthorizedRelationScopeV1::Project {
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
    // read, which is the absence an empty journal reports, disclosing the
    // excluded event so it cannot be mistaken for an empty journal.
    let read = reads(&store)
        .read_graph(
            &context(),
            WorkGraphReadRequestV1::current(
                WorkProductSelectionScopeV1::ProfileOwnedNoGit,
                PROJECTED_AT,
            ),
        )
        .expect("a selection covering no event is answered");
    let WorkGraphReadV1::Absent {
        selection_coverage, ..
    } = read
    else {
        panic!("a selection covering no event has no current version: {read:?}");
    };
    assert_eq!(
        selection_coverage,
        WorkGraphSelectionCoverageV1::Partial {
            covered_events: 0,
            excluded_events: 1,
            first_excluded_sequence: WorkProductEventSequenceV1::new(1).unwrap(),
        }
    );
}

/// The no-Git poisoning defect, stated as the contract that replaced it.
///
/// A profile owner creates work with no Git relation, and later an authority
/// that can only act under a repository scope, attempt admission is the real
/// one, appends a repository-scoped event to the same owner journal. The old
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
    // journal reads whole under it, with a `Complete` disclosure. That is the
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
/// not the journal's, so the refusal is kept, but typed by its actual cause
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

#[test]
fn a_timeline_continuation_the_read_did_not_mint_is_refused() {
    let store = RegisteredWorkStore::start("work-product-foreign-continuation");
    create(
        &store,
        "command.work-product.foreign-continuation",
        UtcMicros(100),
        vec![item("task.only", &[], 2)],
    )
    .expect("create the work product");
    let window = WorkGraphReadRequestV1::evolution(
        repository_selection(),
        UtcMicros(0),
        UtcMicros(300),
        PROJECTED_AT,
    )
    .unwrap();
    let WorkGraphReadV1::Evolution { timeline, .. } = reads(&store)
        .read_graph(&context(), window.clone())
        .expect("read evolution")
    else {
        panic!("an evolution read must answer with a timeline");
    };
    assert_eq!(timeline.entries().len(), 1);

    for foreign in ["work-product-graph-version:0", "bc1.7b7d"] {
        assert_eq!(
            reads(&store).read_graph(
                &context(),
                WorkGraphReadRequestV1 {
                    continuation: Some(OpaqueCursor::new(foreign).unwrap()),
                    ..window.clone()
                },
            ),
            Err(WorkProductApplicationErrorV1::CursorRefused(
                CursorBindingMismatchV1::Foreign
            ))
        );
    }
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

    // Not absent either: events exist, so "no graph yet" would be untrue.
    assert_eq!(
        read_current(&store).expect_err("an unverified event is not a readable graph"),
        WorkProductApplicationErrorV1::GraphAuthorityUnavailable
    );
    assert_eq!(store.count("work_product_events_v1"), 1);
}

/// A current read answers from the head, but the head is only as trustworthy
/// as the verified history beneath it, so a corrupted earlier verified row
/// fails the read closed rather than being skipped as history nobody asked for.
#[test]
fn a_corrupted_historical_verified_version_makes_the_current_read_unavailable() {
    let store = RegisteredWorkStore::start("work-product-tampered-history");
    let first = create(
        &store,
        "command.work-product.tampered-history.create",
        UtcMicros(100),
        vec![item("task.first", &[], 2)],
    )
    .expect("create the first graph version");
    let mut second = mutation("command.work-product.tampered-history.add", UtcMicros(200));
    second.expected_authority = WorkProductExpectedAuthorityV1::Verified {
        verified_version: first.verified_graph_version().clone(),
    };
    mutations(&store)
        .add_task(
            &context(),
            &binding(),
            AddWorkTaskRequestV1 {
                selection: repository_selection(),
                item: item("task.second", &[], 3),
                mutation: second,
            },
        )
        .expect("publish the head version");
    assert!(read_current(&store).is_ok());

    // Point the first verified row at an event that does not exist, through
    // the out-of-band inspection connection. No production writer can.
    store.inspect(|connection| {
        connection
            .execute(
                "UPDATE work_product_graph_versions_v1 SET event_sequence = 99
                 WHERE graph_version = 1",
                [],
            )
            .expect("corrupt the historical verified version");
    });

    assert_eq!(
        read_current(&store).expect_err("a broken verified history is not a readable head"),
        WorkProductApplicationErrorV1::GraphAuthorityUnavailable
    );
}
