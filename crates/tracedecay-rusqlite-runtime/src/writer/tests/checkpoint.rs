use super::*;

#[test]
fn checkpoint_control_surfaces_typed_deadline_and_admission_signal() {
    let database = TestDatabase::new();
    let request = request(metadata("operation.checkpoint", "key.checkpoint", 'p'));
    let writer = start(&database, &request, Arc::new(AtomicU64::new(0)));
    let checkpoint = writer.checkpoint_handle();
    assert_eq!(checkpoint.pressure(), CheckpointPressure::Open);
    let probe = Arc::new(Probe::new(
        &request,
        Some(RuntimeInterruptionV1::DeadlineExceeded),
    ));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();

    let result = runtime
        .block_on(async {
            checkpoint
                .trigger(CheckpointRequest::new(CheckpointBlockers::default(), probe))
                .unwrap()
                .wait()
                .await
        })
        .unwrap();

    assert!(matches!(
        result,
        CheckpointOutcome::Interrupted {
            reason: CheckpointInterruption::DeadlineExceeded,
            wal: None,
            ..
        }
    ));
    assert_eq!(checkpoint.pressure(), CheckpointPressure::Open);
    writer.shutdown_and_join().unwrap();
}

#[test]
fn checkpoint_rechecks_the_same_authority_before_publication() {
    let database = TestDatabase::new();
    let request = request(metadata(
        "operation.checkpoint.authority",
        "key.checkpoint.authority",
        'a',
    ));
    let writer = start(&database, &request, Arc::new(AtomicU64::new(0)));
    let checkpoint = writer.checkpoint_handle();
    let stages = Arc::new(Mutex::new(Vec::new()));
    let authority = Arc::new(RecordingCheckpointAuthority {
        stages: Arc::clone(&stages),
        denied_stage: None,
    });
    let probe = Arc::new(Probe::new(&request, None));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();

    runtime
        .block_on(
            checkpoint
                .trigger_authorized(
                    CheckpointRequest::new(CheckpointBlockers::default(), probe),
                    authority,
                )
                .unwrap()
                .wait(),
        )
        .unwrap();

    assert_eq!(
        *stages.lock().unwrap(),
        [
            RuntimeWriteAuthorityStage::BeforeAdmission,
            RuntimeWriteAuthorityStage::Dequeued,
            RuntimeWriteAuthorityStage::BeforeCommit,
        ]
    );
    assert!(checkpoint.status().latest.is_some());
    writer.shutdown_and_join().unwrap();
}

#[test]
fn checkpoint_authority_loss_is_typed_and_never_published() {
    for denied_stage in [
        RuntimeWriteAuthorityStage::BeforeAdmission,
        RuntimeWriteAuthorityStage::Dequeued,
        RuntimeWriteAuthorityStage::BeforeCommit,
    ] {
        let database = TestDatabase::new();
        let request = request(metadata(
            "operation.checkpoint.revoked",
            "key.checkpoint.revoked",
            'r',
        ));
        let writer = start(&database, &request, Arc::new(AtomicU64::new(0)));
        let checkpoint = writer.checkpoint_handle();
        let authority = Arc::new(RecordingCheckpointAuthority {
            stages: Arc::new(Mutex::new(Vec::new())),
            denied_stage: Some(denied_stage),
        });
        let probe = Arc::new(Probe::new(&request, None));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();

        let result = checkpoint.trigger_authorized(
            CheckpointRequest::new(CheckpointBlockers::default(), probe),
            authority,
        );
        let error = match result {
            Ok(ticket) => runtime.block_on(ticket.wait()).unwrap_err(),
            Err(error) => error,
        };

        assert_eq!(
            error,
            CheckpointControlError::AuthorityDenied {
                stage: denied_stage
            }
        );
        assert_eq!(checkpoint.status(), CheckpointStatus::default());
        writer.shutdown_and_join().unwrap();
    }
}

#[test]
fn hard_checkpoint_pressure_blocks_general_admission() {
    let sample = WalSample {
        frames: 64,
        bytes: 256 * 1024 * 1024,
    };
    let blockers = CheckpointBlockers::default();
    let result = CheckpointResult::Decision {
        sample,
        decision: CheckpointDecision::Pending {
            mode: CheckpointMode::Passive,
            pressure: WalPressure::Hard,
            wal_bytes: sample.bytes,
            report: CheckpointReport {
                busy: false,
                log_frames: sample.frames,
                checkpointed_frames: sample.frames - 1,
            },
            snapshot_blockers: blockers.clone(),
            hard_drain_required: true,
            elapsed: Duration::ZERO,
        },
    };

    assert_eq!(
        worker::checkpoint_pressure_signal(&result),
        Some(CheckpointPressure::BlockGeneral {
            wal: crate::CheckpointWal::from_sample(sample),
            blockers,
        })
    );
}

#[test]
fn maintenance_checkpoint_uses_linear_permit_through_the_handle() {
    let database = TestDatabase::new();
    let request = request(metadata(
        "operation.maintenance-checkpoint",
        "key.maintenance-checkpoint",
        'm',
    ));
    let writer = start(&database, &request, Arc::new(AtomicU64::new(0)));
    let checkpoint = writer.checkpoint_handle();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime
        .block_on(writer.submit(request.clone(), Arc::new(Probe::new(&request, None))))
        .unwrap();
    let permit = ExclusiveMaintenancePermit::issue(
        MaintenanceOwnerId::new(1).unwrap(),
        writer.binding().clone(),
    );
    writer.begin_drain();

    let result = runtime
        .block_on(async {
            checkpoint
                .trigger_maintenance(MaintenanceCheckpointRequest::new(
                    MaintenanceCheckpointMode::Restart,
                    permit,
                    CheckpointBlockers::default(),
                ))
                .unwrap()
                .wait()
                .await
        })
        .unwrap();

    assert!(matches!(
        result,
        CheckpointOutcome::Complete {
            kind: CheckpointKind::Restart,
            ..
        }
    ));
    writer.shutdown_and_join().unwrap();
}

#[test]
fn maintenance_checkpoint_surfaces_blockers_without_faulting_writer() {
    let database = TestDatabase::new();
    let request = request(metadata(
        "operation.maintenance-blocked",
        "key.maintenance-blocked",
        'b',
    ));
    let writer = start(&database, &request, Arc::new(AtomicU64::new(0)));
    let checkpoint = writer.checkpoint_handle();
    let permit = ExclusiveMaintenancePermit::issue(
        MaintenanceOwnerId::new(1).unwrap(),
        writer.binding().clone(),
    );
    writer.begin_drain();
    let blockers = CheckpointBlockers {
        blockers: Vec::new(),
        omitted: 1,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();

    let error = runtime
        .block_on(async {
            checkpoint
                .trigger_maintenance(MaintenanceCheckpointRequest::new(
                    MaintenanceCheckpointMode::Restart,
                    permit,
                    blockers.clone(),
                ))
                .unwrap()
                .wait()
                .await
        })
        .unwrap_err();

    assert_eq!(error, CheckpointControlError::Blocked(blockers));
    assert_eq!(writer.state(), WriterState::Draining);
    writer.shutdown_and_join().unwrap();
}

#[test]
fn maintenance_checkpoint_waits_for_product_writes_admitted_before_drain() {
    let database = TestDatabase::new();
    let first_request = fact_request(
        "operation.maintenance-order.first",
        "key.maintenance-order.first",
        'f',
    );
    let second_request = fact_request(
        "operation.maintenance-order.second",
        "key.maintenance-order.second",
        's',
    );
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let writer = Arc::new(start_with_persistence(
        &database,
        &first_request,
        Box::new(BlockingPersistence {
            entered: entered_tx,
            release: release_rx,
            sequence: 0,
        }),
    ));
    let checkpoint = writer.checkpoint_handle();

    std::thread::scope(|scope| {
        let first_writer = Arc::clone(&writer);
        let first = scope.spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            runtime.block_on(first_writer.submit(
                first_request.clone(),
                Arc::new(Probe::new(&first_request, None)),
            ))
        });
        assert_eq!(
            entered_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            1,
            "the first admitted write must hold the worker"
        );

        let second_writer = Arc::clone(&writer);
        let second = scope.spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            runtime.block_on(second_writer.submit(
                second_request.clone(),
                Arc::new(Probe::new(&second_request, None)),
            ))
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while {
            let sender = writer
                .sender
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let sender = sender.as_ref().expect("writer admission stays open");
            sender.capacity() == sender.max_capacity()
        } {
            assert!(
                std::time::Instant::now() < deadline,
                "the second write was not queued while the worker was occupied"
            );
            std::thread::yield_now();
        }

        writer.begin_drain();
        let permit = ExclusiveMaintenancePermit::issue(
            MaintenanceOwnerId::new(1).unwrap(),
            writer.binding().clone(),
        );
        let mut ticket = checkpoint
            .trigger_maintenance(MaintenanceCheckpointRequest::new(
                MaintenanceCheckpointMode::Truncate,
                permit,
                CheckpointBlockers::default(),
            ))
            .unwrap();

        release_tx.send(()).unwrap();
        assert_eq!(
            entered_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            2,
            "the already-admitted second write must reach persistence"
        );
        let maintenance_waited = matches!(
            ticket.response.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        );
        release_tx.send(()).unwrap();
        assert!(matches!(
            first.join().unwrap().unwrap(),
            RuntimeSubmitOutcomeV1::Committed { .. }
        ));
        assert!(matches!(
            second.join().unwrap().unwrap(),
            RuntimeSubmitOutcomeV1::Committed { .. }
        ));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        if maintenance_waited {
            assert!(matches!(
                runtime.block_on(ticket.wait()).unwrap(),
                CheckpointOutcome::Complete {
                    kind: CheckpointKind::Truncate,
                    ..
                }
            ));
        }
        assert!(
            maintenance_waited,
            "maintenance must remain queued until the admitted write commits"
        );
    });

    let writer = match Arc::try_unwrap(writer) {
        Ok(writer) => writer,
        Err(_) => panic!("test submit handles must be joined"),
    };
    writer.shutdown_and_join().unwrap();
}
