use super::*;

#[test]
fn cancelled_before_admission_never_enters_the_queue() {
    let database = TestDatabase::new();
    let request = request(metadata("operation.cancel", "key.cancel", 'c'));
    let applied = Arc::new(AtomicU64::new(0));
    let writer = start(&database, &request, Arc::clone(&applied));
    let probe = Arc::new(Probe::new(&request, Some(RuntimeInterruptionV1::Cancelled)));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let outcome = runtime.block_on(writer.submit(request, probe)).unwrap();
    assert!(matches!(
        outcome,
        RuntimeSubmitOutcomeV1::CancelledBeforeCommit {
            stage: RuntimeCancellationStageV1::BeforeAdmission,
            ..
        }
    ));
    assert_eq!(applied.load(Ordering::SeqCst), 0);
    writer.shutdown_and_join().unwrap();
}

#[test]
fn cancelled_request_does_not_interrupt_an_unrelated_request_in_the_same_batch() {
    let database = TestDatabase::new();
    let first = request(metadata(
        "operation.cancel.batch.first",
        "key.cancel.batch.first",
        'c',
    ));
    let second = request(metadata(
        "operation.cancel.batch.second",
        "key.cancel.batch.second",
        'd',
    ));
    let binding = binding(&first.envelope().metadata);
    let first_probe = Arc::new(Probe::new(&first, None));
    let second_probe = Arc::new(Probe::new(&second, None));
    let admission = Admission::new(
        Limits::new(
            Capacity {
                operations: 2,
                bytes: u64::MAX,
            },
            Capacity {
                operations: 1,
                bytes: u64::MAX,
            },
            u64::MAX,
            u64::MAX,
        )
        .unwrap(),
    );
    let (first_reply, mut first_result) = tokio::sync::oneshot::channel();
    let (second_reply, mut second_result) = tokio::sync::oneshot::channel();
    let first = Arc::new(first);
    let second = Arc::new(second);
    let batch = request::ExecutionBatch {
        bytes: first.envelope().metadata.admission_bytes
            + second.envelope().metadata.admission_bytes,
        items: vec![
            AcceptedRequest::new(
                Arc::clone(&first),
                first_probe.clone(),
                Arc::new(UnrestrictedRuntimeWriteAuthority),
                first_reply,
                admission.reserve(&first.envelope().metadata).unwrap(),
            ),
            AcceptedRequest::new(
                Arc::clone(&second),
                second_probe,
                Arc::new(UnrestrictedRuntimeWriteAuthority),
                second_reply,
                admission.reserve(&second.envelope().metadata).unwrap(),
            ),
        ],
    };
    let mut connection = Connection::open(&database.0).unwrap();
    let telemetry = WriterTelemetry::default();
    let state = AtomicU8::new(WriterState::Ready as u8);
    let watermark = CommittedWatermarkPublisher::new(binding.clone());
    let mut persistence = CancellingFirstRequestPersistence {
        first_probe,
        sequence: 0,
    };

    worker::process_execution_batch(
        &mut connection,
        &binding,
        batch,
        &mut persistence,
        &telemetry,
        &state,
        &watermark,
    );

    assert!(matches!(
        first_result.try_recv().unwrap(),
        Ok(RuntimeSubmitOutcomeV1::CancelledBeforeCommit { .. })
    ));
    assert!(matches!(
        second_result.try_recv().unwrap(),
        Ok(RuntimeSubmitOutcomeV1::Committed { .. })
    ));
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM cancellation_batch", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn active_long_running_request_remains_interruptible() {
    let database = TestDatabase::new();
    let request = request(metadata("operation.cancel.long", "key.cancel.long", 'e'));
    let binding = binding(&request.envelope().metadata);
    let probe = Arc::new(DelayedInterruptionProbe {
        inner: Probe::new(&request, None),
        checks_before_interruption: AtomicU64::new(1),
        interruption: RuntimeInterruptionV1::Cancelled,
    });
    let admission = Admission::new(
        Limits::new(
            Capacity {
                operations: 1,
                bytes: u64::MAX,
            },
            Capacity {
                operations: 1,
                bytes: u64::MAX,
            },
            u64::MAX,
            u64::MAX,
        )
        .unwrap(),
    );
    let (reply, mut result) = tokio::sync::oneshot::channel();
    let request = Arc::new(request);
    let batch = request::ExecutionBatch {
        bytes: request.envelope().metadata.admission_bytes,
        items: vec![AcceptedRequest::new(
            Arc::clone(&request),
            probe,
            Arc::new(UnrestrictedRuntimeWriteAuthority),
            reply,
            admission.reserve(&request.envelope().metadata).unwrap(),
        )],
    };
    let mut connection = Connection::open(&database.0).unwrap();
    let telemetry = WriterTelemetry::default();
    let state = AtomicU8::new(WriterState::Ready as u8);
    let watermark = CommittedWatermarkPublisher::new(binding.clone());

    worker::process_execution_batch(
        &mut connection,
        &binding,
        batch,
        &mut LongRunningPersistence,
        &telemetry,
        &state,
        &watermark,
    );

    assert!(matches!(
        result.try_recv().unwrap(),
        Ok(RuntimeSubmitOutcomeV1::CancelledBeforeCommit {
            stage: RuntimeCancellationStageV1::BeforeCommit,
            ..
        })
    ));
    assert_eq!(state.load(Ordering::SeqCst), WriterState::Ready as u8);
}
