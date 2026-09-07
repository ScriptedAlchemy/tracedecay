use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

fn busy_begin_connections() -> (TempDir, Connection, Connection) {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("busy-begin.sqlite3");
    let locker = Connection::open(&path).unwrap();
    locker.pragma_update(None, "journal_mode", "WAL").unwrap();
    locker
        .execute_batch("CREATE TABLE busy_begin(value INTEGER NOT NULL)")
        .unwrap();
    let contender = Connection::open(&path).unwrap();
    contender.busy_timeout(Duration::ZERO).unwrap();
    (directory, locker, contender)
}

#[test]
fn immediate_begin_retries_sqlite_busy_until_lock_releases() {
    let (_directory, mut locker, contender) = busy_begin_connections();
    let lock = locker
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let started = Arc::new(AtomicBool::new(false));
    let shutdown = Arc::new(AtomicBool::new(false));

    std::thread::scope(|scope| {
        let admission_started = Arc::clone(&started);
        let shutdown = Arc::clone(&shutdown);
        let admission = scope.spawn(move || {
            admission_started.store(true, Ordering::Release);
            let transaction = super::super::command::begin_transaction_with_busy_retry(
                &contender,
                TransactionBehavior::Immediate,
                &shutdown,
            )
            .unwrap();
            transaction.rollback().unwrap();
        });
        while !started.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        std::thread::yield_now();
        lock.rollback().unwrap();
        admission.join().unwrap();
    });
}

#[test]
fn immediate_begin_busy_retry_is_bounded_and_honors_shutdown() {
    let (_directory, mut locker, contender) = busy_begin_connections();
    let _lock = locker
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let shutdown = AtomicBool::new(false);

    let error = super::super::command::begin_transaction_with_busy_retry(
        &contender,
        TransactionBehavior::Immediate,
        &shutdown,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        rusqlite::Error::SqliteFailure(error, _)
            if matches!(
                error.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    ));

    shutdown.store(true, Ordering::Release);
    let error = super::super::command::begin_transaction_with_busy_retry(
        &contender,
        TransactionBehavior::Immediate,
        &shutdown,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        rusqlite::Error::SqliteFailure(error, _)
            if matches!(
                error.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    ));
}

#[test]
fn immediate_begin_shutdown_after_busy_never_publishes_late_success() {
    let shutdown = AtomicBool::new(false);
    let mut attempts = 0;

    let error = super::super::command::retry_busy_begin(
        || {
            attempts += 1;
            if attempts == 1 {
                shutdown.store(true, Ordering::Release);
                Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                    Some("original database lock".to_owned()),
                ))
            } else {
                Ok(())
            }
        },
        &shutdown,
    )
    .unwrap_err();

    assert_eq!(attempts, 1);
    assert!(matches!(
        error,
        rusqlite::Error::SqliteFailure(error, Some(message))
            if error.code == rusqlite::ErrorCode::DatabaseBusy
                && message == "original database lock"
    ));
}

#[test]
fn deferred_begin_keeps_one_shot_sqlite_semantics() {
    let (_directory, mut locker, contender) = busy_begin_connections();
    let _lock = locker
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let shutdown = AtomicBool::new(true);

    let transaction = super::super::command::begin_transaction_with_busy_retry(
        &contender,
        TransactionBehavior::Deferred,
        &shutdown,
    )
    .unwrap();

    transaction.rollback().unwrap();
}

#[test]
fn deferred_transaction_is_available_for_default_sqlite_semantics() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch("CREATE TABLE deferred (value INTEGER NOT NULL)".to_owned())
        .unwrap();
    let transaction = channel.begin_deferred().unwrap();
    transaction
        .execute(statement(
            "INSERT INTO deferred VALUES (?)",
            vec![ExactSqlValue::Integer(1)],
        ))
        .unwrap();

    transaction.commit().unwrap();

    let rows = channel
        .query(
            statement("SELECT count(*) FROM deferred", vec![]),
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(rows.rows[0].values, vec![ExactSqlValue::Integer(1)]);
}

#[test]
fn immediate_transaction_commit_reports_only_after_commit() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch("CREATE TABLE committed (value INTEGER NOT NULL)".to_owned())
        .unwrap();
    let transaction = channel.begin_immediate().unwrap();
    transaction
        .execute(statement(
            "INSERT INTO committed VALUES (?)",
            vec![ExactSqlValue::Integer(41)],
        ))
        .unwrap();
    let inside = transaction
        .query(statement("SELECT value FROM committed", vec![]))
        .unwrap();

    assert_eq!(inside.rows[0].values, vec![ExactSqlValue::Integer(41)]);
    let receipt = transaction.commit().unwrap();
    assert_eq!(receipt.changed_rows, 1);
    let committed = channel
        .query(
            statement("SELECT value FROM committed", vec![]),
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(committed.rows[0].values, vec![ExactSqlValue::Integer(41)]);
}

#[test]
fn transaction_attachment_is_exact_and_auto_detached() {
    let fixture = fixture('a', 'a');
    let source_path = fixture._directory.path().join("source.sqlite3");
    rusqlite::Connection::open(&source_path)
        .unwrap()
        .execute_batch(
            "CREATE TABLE source_rows(value INTEGER NOT NULL);
                 INSERT INTO source_rows VALUES (7);",
        )
        .unwrap();
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    let attachment =
        || ExactSqlAttachment::new(source_path.to_string_lossy(), "source_input").unwrap();

    let transaction = channel.begin_immediate().unwrap();
    transaction.attach_database(attachment()).unwrap();
    let rows = transaction
        .query(statement(
            "SELECT value FROM source_input.source_rows",
            vec![],
        ))
        .unwrap();
    assert_eq!(rows.rows[0].values, vec![ExactSqlValue::Integer(7)]);
    transaction.commit().unwrap();

    let transaction = channel.begin_immediate().unwrap();
    transaction
        .attach_database(attachment())
        .expect("commit must detach the prior exact input");
    transaction.rollback().unwrap();

    let transaction = channel.begin_immediate().unwrap();
    transaction
        .attach_database(attachment())
        .expect("rollback must detach the prior exact input");
    drop(transaction);

    let transaction = channel.begin_immediate().unwrap();
    transaction
        .attach_database(attachment())
        .expect("dropping a transaction must detach the prior exact input");
    transaction.rollback().unwrap();
}

#[test]
fn caller_sql_cannot_attach_database() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

    channel
        .execute(statement(
            "ATTACH DATABASE ?1 AS caller_input",
            vec![ExactSqlValue::Text(":memory:".to_owned())],
        ))
        .unwrap_err();
    let databases = channel
        .query(
            statement("PRAGMA database_list", vec![]),
            Duration::from_secs(1),
        )
        .unwrap();
    assert!(databases.rows.iter().all(|row| {
        !matches!(
            row.values.get(1),
            Some(ExactSqlValue::Text(name)) if name == "caller_input"
        )
    }));
}

#[test]
fn immediate_transaction_rollback_reports_after_rollback_and_discards_rows() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch("CREATE TABLE rolled_back (value INTEGER NOT NULL)".to_owned())
        .unwrap();
    let transaction = channel.begin_immediate().unwrap();
    transaction
        .execute(statement(
            "INSERT INTO rolled_back VALUES (?)",
            vec![ExactSqlValue::Integer(99)],
        ))
        .unwrap();

    let receipt = transaction.rollback().unwrap();

    assert_eq!(receipt.discarded_changed_rows, 1);
    let rows = channel
        .query(
            statement("SELECT count(*) FROM rolled_back", vec![]),
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(rows.rows[0].values, vec![ExactSqlValue::Integer(0)]);
}

#[test]
fn pinned_batch_rejects_transaction_control() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    let transaction = channel.begin_immediate().unwrap();

    let error = transaction
        .execute_batch("COMMIT; BEGIN IMMEDIATE".to_owned())
        .unwrap_err();

    assert!(matches!(error, ExactSqlError::TransactionControlDenied));
    transaction.rollback().unwrap();
}

#[test]
fn pinned_execute_rejects_transaction_control_before_commit_receipt() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    let transaction = channel.begin_immediate().unwrap();

    let error = transaction
        .execute(statement("COMMIT", vec![]))
        .unwrap_err();

    assert!(matches!(error, ExactSqlError::TransactionControlDenied));
    transaction.rollback().unwrap();
}

#[test]
fn unpinned_batch_rejects_transaction_control_and_releases_writer() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

    let error = channel
        .execute_batch("BEGIN IMMEDIATE".to_owned())
        .unwrap_err();

    assert!(matches!(error, ExactSqlError::TransactionControlDenied));
    channel
        .execute_batch("CREATE TABLE after_denied_begin (value INTEGER)".to_owned())
        .unwrap();
}
