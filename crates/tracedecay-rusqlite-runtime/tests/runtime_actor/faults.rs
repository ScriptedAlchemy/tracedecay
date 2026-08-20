use tracedecay_rusqlite_runtime::{WriterActorError, WriterState};
use tracedecay_store::{
    AdmissionConfigV1, CorruptionClassV1, OperationPriorityV1, RuntimeSubmitOutcomeV1,
    StorageRuntimeErrorV1, UnavailableReasonV1,
};

use crate::support::{
    ExecutorControl, TestBinding, TestDatabase, TestProbe, marker_count, request, runtime, writer,
};

#[test]
fn binding_mismatches_are_typed_and_corrupt_replay_faults_closed() {
    let database = TestDatabase::new();
    let binding = TestBinding::project("project.fence");
    let base = request(
        binding,
        "operation.fence.base",
        "key.fence.base",
        'a',
        OperationPriorityV1::Foreground,
    );
    let first_writer = writer(
        &database,
        &base,
        AdmissionConfigV1::default(),
        ExecutorControl::default(),
    );
    let wrong_incarnation = request(
        TestBinding {
            incarnation: 2,
            ..binding
        },
        "operation.fence.incarnation",
        "key.fence.incarnation",
        'b',
        OperationPriorityV1::Foreground,
    );
    assert_eq!(
        runtime()
            .block_on(first_writer.submit(
                wrong_incarnation.clone(),
                TestProbe::fixed(&wrong_incarnation),
            ))
            .unwrap(),
        RuntimeSubmitOutcomeV1::Unavailable {
            reason: UnavailableReasonV1::WrongIncarnation
        }
    );
    let wrong_epoch = request(
        TestBinding {
            authority_epoch: 8,
            ..binding
        },
        "operation.fence.epoch",
        "key.fence.epoch",
        'c',
        OperationPriorityV1::Foreground,
    );
    assert!(matches!(
        runtime()
            .block_on(first_writer.submit(wrong_epoch.clone(), TestProbe::fixed(&wrong_epoch)))
            .unwrap(),
        RuntimeSubmitOutcomeV1::Fenced { .. }
    ));
    assert!(matches!(
        runtime()
            .block_on(first_writer.submit(base.clone(), TestProbe::fixed(&base)))
            .unwrap(),
        RuntimeSubmitOutcomeV1::Committed { .. }
    ));
    first_writer.shutdown_and_join().unwrap();

    database
        .connect()
        .execute(
            "UPDATE td_runtime_writer_idempotency_v1 SET original_receipt_json = '{}'",
            [],
        )
        .unwrap();
    let replay = request(
        binding,
        "operation.fence.replay",
        "key.fence.base",
        'a',
        OperationPriorityV1::Foreground,
    );
    let restarted = writer(
        &database,
        &replay,
        AdmissionConfigV1::default(),
        ExecutorControl::default(),
    );
    assert!(matches!(
        runtime().block_on(restarted.submit(replay.clone(), TestProbe::fixed(&replay))),
        Err(WriterActorError::StorageFailure(
            StorageRuntimeErrorV1::Corrupt {
                class: CorruptionClassV1::Authoritative
            }
        ))
    ));
    assert_eq!(restarted.state(), WriterState::Faulted);
    restarted.shutdown_and_join().unwrap();
    assert_eq!(marker_count(&database), 1);
}

#[test]
fn executor_panic_faults_the_actor_and_releases_the_pending_request() {
    let database = TestDatabase::new();
    let request = request(
        TestBinding::project("project.panic"),
        "operation.panic",
        "key.panic",
        'a',
        OperationPriorityV1::Foreground,
    );
    let writer = writer(
        &database,
        &request,
        AdmissionConfigV1::default(),
        ExecutorControl {
            panic_after_mutation: true,
            ..ExecutorControl::default()
        },
    );
    assert!(matches!(
        runtime().block_on(writer.submit(request.clone(), TestProbe::fixed(&request))),
        Err(WriterActorError::ReplyDropped)
    ));

    // Joining the worker is the fault-settlement event: once the actor
    // thread has exited, the queue has been released and the final state and
    // telemetry are visible, so nothing here needs to poll.
    let (state, telemetry) = writer.shutdown_and_join_snapshot().unwrap();
    assert_eq!(state, WriterState::Faulted);
    assert_eq!(telemetry.queue.queued_operations, 0);
    assert_eq!(
        telemetry.operations.admitted_operations,
        telemetry.operations.completed_operations
    );
    assert_eq!(telemetry.error_events, 1);
    assert_eq!(marker_count(&database), 0);
}
