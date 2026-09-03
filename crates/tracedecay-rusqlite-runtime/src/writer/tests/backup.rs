use super::*;

#[test]
fn online_backup_is_verified_and_leaves_the_source_writer_usable() {
    let database = TestDatabase::new();
    let first = request(metadata("operation.backup.first", "key.backup.first", 'b'));
    let writer = start(&database, &first, Arc::new(AtomicU64::new(0)));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime
        .block_on(writer.submit(first.clone(), Arc::new(Probe::new(&first, None))))
        .unwrap();
    let destination = database.0.with_extension("backup.sqlite3");
    let allowed = Arc::new(AtomicBool::new(true));

    let receipt = runtime
        .block_on(writer.snapshot_to(
            destination.clone(),
            Arc::new(ToggleAuthority {
                allowed: Arc::clone(&allowed),
            }),
        ))
        .unwrap();

    assert_eq!(
        receipt.source_watermark.commit_sequence,
        CommitSequenceV1(1)
    );
    assert!(receipt.destination_bytes > 0);
    assert_ne!(receipt.destination_sha256.0, [0; 32]);
    let backup_rows: i64 = Connection::open(&destination)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM writer_test", [], |row| row.get(0))
        .unwrap();
    assert_eq!(backup_rows, 1);

    let second = request(metadata(
        "operation.backup.second",
        "key.backup.second",
        'c',
    ));
    runtime
        .block_on(writer.submit(second.clone(), Arc::new(Probe::new(&second, None))))
        .unwrap();
    let source_rows: i64 = Connection::open(&database.0)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM writer_test", [], |row| row.get(0))
        .unwrap();
    assert_eq!(source_rows, 2);
    writer.shutdown_and_join().unwrap();
    std::fs::remove_file(destination).unwrap();
}

#[test]
fn online_backup_rejects_revoked_authority_and_existing_destinations() {
    let database = TestDatabase::new();
    let request = request(metadata(
        "operation.backup.reject",
        "key.backup.reject",
        'r',
    ));
    let writer = start(&database, &request, Arc::new(AtomicU64::new(0)));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let destination = database.0.with_extension("backup-reject.sqlite3");

    let error = runtime
        .block_on(writer.snapshot_to(
            destination.clone(),
            Arc::new(RevokeAfterAdmissionAuthority {
                admitted: AtomicBool::new(false),
            }),
        ))
        .unwrap_err();
    assert!(matches!(
        error,
        WriterActorError::AuthorityDenied {
            stage: RuntimeWriteAuthorityStage::Dequeued
        }
    ));
    assert!(!destination.exists());

    std::fs::write(&destination, b"existing").unwrap();
    let error = runtime
        .block_on(writer.snapshot_to(
            destination.clone(),
            Arc::new(ToggleAuthority {
                allowed: Arc::new(AtomicBool::new(true)),
            }),
        ))
        .unwrap_err();
    assert!(matches!(
        error,
        WriterActorError::OnlineBackupFailed(WriterOnlineBackupError::DestinationExists)
    ));
    assert_eq!(std::fs::read(&destination).unwrap(), b"existing");
    writer.shutdown_and_join().unwrap();
    std::fs::remove_file(destination).unwrap();
}

#[test]
fn online_backup_authority_loss_before_publication_removes_staging() {
    let database = TestDatabase::new();
    let request = request(metadata(
        "operation.backup.prepublish",
        "key.backup.prepublish",
        'p',
    ));
    let writer = start(&database, &request, Arc::new(AtomicU64::new(0)));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let destination = database.0.with_extension("backup-prepublish.sqlite3");

    let error = runtime
        .block_on(writer.snapshot_to(
            destination.clone(),
            Arc::new(DenyThirdBeforeCommitAuthority {
                before_commit_checks: AtomicU64::new(0),
            }),
        ))
        .unwrap_err();

    assert!(matches!(
        error,
        WriterActorError::AuthorityDenied {
            stage: RuntimeWriteAuthorityStage::BeforeCommit
        }
    ));
    assert!(!destination.exists());
    let staging_prefix = format!(
        ".{}.tracedecay-backup-",
        destination.file_name().unwrap().to_string_lossy()
    );
    let leaked_staging = std::fs::read_dir(destination.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().starts_with(&staging_prefix))
        .collect::<Vec<_>>();
    assert!(leaked_staging.is_empty(), "{leaked_staging:?}");
    writer.shutdown_and_join().unwrap();
}

#[test]
fn online_backup_cancellation_and_deadline_remove_private_staging() {
    for interruption in [
        RuntimeInterruptionV1::Cancelled,
        RuntimeInterruptionV1::DeadlineExceeded,
    ] {
        let database = TestDatabase::new();
        let request = request(metadata(
            "operation.backup.interrupt",
            "key.backup.interrupt",
            'i',
        ));
        let writer = start(&database, &request, Arc::new(AtomicU64::new(0)));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let destination = database.0.with_extension("backup-interrupted.sqlite3");
        let probe = Arc::new(DelayedInterruptionProbe {
            inner: Probe::new(&request, None),
            checks_before_interruption: AtomicU64::new(3),
            interruption,
        });

        let error = runtime
            .block_on(writer.snapshot_to_interruptible(
                destination.clone(),
                probe,
                Arc::new(ToggleAuthority {
                    allowed: Arc::new(AtomicBool::new(true)),
                }),
            ))
            .unwrap_err();

        assert!(matches!(
            (interruption, error),
            (
                RuntimeInterruptionV1::Cancelled,
                WriterActorError::OnlineBackupFailed(WriterOnlineBackupError::Cancelled)
            ) | (
                RuntimeInterruptionV1::DeadlineExceeded,
                WriterActorError::OnlineBackupFailed(WriterOnlineBackupError::DeadlineExceeded)
            )
        ));
        assert!(!destination.exists());
        let staging_prefix = format!(
            ".{}.tracedecay-backup-",
            destination.file_name().unwrap().to_string_lossy()
        );
        assert!(
            std::fs::read_dir(destination.parent().unwrap())
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&staging_prefix))
        );
        writer.shutdown_and_join().unwrap();
    }
}
