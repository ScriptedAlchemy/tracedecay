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
                .wait(&cx())
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
                .wait(&cx()),
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
            Ok(ticket) => runtime.block_on(ticket.wait(&cx())).unwrap_err(),
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
