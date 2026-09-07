use super::*;

#[test]
fn mutating_no_argument_pragmas_are_denied() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

    for pragma in [
        "PRAGMA cache_flush",
        "PRAGMA incremental_vacuum",
        "PRAGMA optimize",
        "PRAGMA wal_checkpoint",
    ] {
        let error = channel.execute_batch(pragma.to_owned()).unwrap_err();
        assert!(
            matches!(
                error,
                ExactSqlError::Sqlite {
                    code: Some(23),
                    extended_code: Some(23),
                    ..
                }
            ),
            "{pragma} must be denied, got {error:?}"
        );
    }
}

#[test]
fn connection_local_memory_release_pragma_is_allowed() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

    channel
        .execute_batch("PRAGMA shrink_memory".to_owned())
        .expect("connection-local cache release must be authorized");
}

#[test]
fn read_only_handle_releases_reader_memory_without_writer() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach_read_only(&fixture.readers);

    let outcome = channel
        .release_connection_memory()
        .expect("read-only memory release must not require a writer");
    assert!(
        matches!(
            outcome,
            MemoryReleaseOutcome::Released {
                reader_connections,
                writer: false,
            } if reader_connections > 0
        ),
        "read-only handle must shrink its reader pool, got {outcome:?}"
    );
    assert!(
        matches!(
            channel.execute_batch("PRAGMA shrink_memory".to_owned()),
            Err(ExactSqlError::WriterUnavailable)
        ),
        "read-only execute_batch must still refuse the writer lane"
    );
}

#[test]
fn writable_handle_releases_readers_and_writer() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

    let outcome = channel
        .release_connection_memory()
        .expect("writable memory release");
    assert!(
        matches!(
            outcome,
            MemoryReleaseOutcome::Released {
                reader_connections,
                writer: true,
            } if reader_connections > 0
        ),
        "writable handle must shrink readers and writer, got {outcome:?}"
    );
}

/// A worker leased into a retained snapshot answers the release on its
/// snapshot channel, interleaved between reads — a live snapshot must not
/// degrade the release into a no-op or a spurious "worker closed".
#[test]
fn memory_release_reaches_workers_inside_retained_snapshots() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach_read_only(&fixture.readers);
    let snapshot = channel
        .begin_read_snapshot(Duration::from_secs(5))
        .expect("retained read snapshot");

    let outcome = channel
        .release_connection_memory()
        .expect("release must interleave with leased snapshot workers");
    assert!(
        matches!(
            outcome,
            MemoryReleaseOutcome::Released {
                reader_connections,
                writer: false,
            } if reader_connections > 0
        ),
        "leased snapshot workers must still release, got {outcome:?}"
    );
    drop(snapshot);
}

#[test]
fn closed_reader_pool_reports_typed_memory_release_noop() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("closed-pool.sqlite3");
    rusqlite::Connection::open(&path).unwrap();
    let path = path.canonicalize().unwrap();
    let binding = binding();
    let readers = ReaderPool::start(
        ExistingReaderLocator::new(binding.clone(), locator(&binding, 'c'), path).unwrap(),
        AdmissionConfigV1::default().readers,
        NoReads,
    )
    .unwrap();
    let channel = ExactSqlHandle::attach_read_only(&readers);
    drop(readers);

    let outcome = channel
        .release_connection_memory()
        .expect("closed-pool release is a typed no-op, not a writer fault");
    assert_eq!(
        outcome,
        MemoryReleaseOutcome::NoOp {
            reason: MemoryReleaseNoOpReason::ReaderPoolClosed,
        }
    );
}

#[test]
fn exact_sql_read_policy_allows_integrity_diagnostic_arguments() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch("CREATE TABLE pragma_probe (value INTEGER)".to_owned())
        .unwrap();
    let transaction = channel.begin_deferred().unwrap();
    for pragma in [
        "PRAGMA quick_check",
        "PRAGMA quick_check(1000)",
        "PRAGMA integrity_check",
        "PRAGMA integrity_check(1000)",
    ] {
        let rows = transaction.query(statement(pragma, vec![])).unwrap();
        assert_eq!(
            rows.rows[0].values,
            vec![ExactSqlValue::Text("ok".to_owned())],
            "{pragma} must remain classified as a read-only diagnostic"
        );
    }
    let table_info = transaction
        .query(statement("PRAGMA table_info(pragma_probe)", vec![]))
        .unwrap();
    assert_eq!(
        table_info.rows[0].values[1],
        ExactSqlValue::Text("value".to_owned())
    );
    transaction.rollback().unwrap();
}
