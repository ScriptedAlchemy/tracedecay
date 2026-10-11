use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

use super::super::command::{EXACT_SQL_WRITE_LOCK_ACQUIRE_LIMIT, retry_busy_begin};

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

fn sqlite_busy(message: String) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
        Some(message),
    )
}

fn is_busy_with(error: &rusqlite::Error, expected: &str) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(error, Some(message))
            if error.code == rusqlite::ErrorCode::DatabaseBusy && message == expected
    )
}

#[test]
fn immediate_begin_retries_sqlite_busy_until_lock_releases() {
    let (_directory, mut locker, contender) = busy_begin_connections();
    let mut lock = Some(
        locker
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap(),
    );
    let shutdown = AtomicBool::new(false);
    let mut attempts = 0;

    // This thread's exact SQL clock is frozen, so it never reaches the acquire
    // deadline and the holder's release alone decides when a retry succeeds.
    let transaction = retry_busy_begin(
        || {
            attempts += 1;
            let attempt =
                rusqlite::Transaction::new_unchecked(&contender, TransactionBehavior::Immediate);
            if let Some(lock) = lock.take() {
                lock.rollback().unwrap();
            }
            attempt
        },
        &shutdown,
        || false,
    )
    .unwrap();

    assert_eq!(
        attempts, 2,
        "the busy attempt must be retried once released"
    );
    transaction.rollback().unwrap();
}

#[test]
fn immediate_begin_busy_retry_is_bounded_and_honors_shutdown() {
    let shutdown = AtomicBool::new(false);
    let clock = FakeClock::frozen();
    clock.install();
    let mut attempts = 0;

    let error = retry_busy_begin(
        || {
            attempts += 1;
            clock.advance(Duration::from_millis(1));
            Err::<(), _>(sqlite_busy(format!("attempt {attempts}")))
        },
        &shutdown,
        || false,
    )
    .unwrap_err();

    assert!(is_busy_with(&error, "attempt 1"), "{error:?}");
    assert_eq!(
        clock.elapsed(),
        EXACT_SQL_WRITE_LOCK_ACQUIRE_LIMIT,
        "busy acquire must stop at the first busy attempt that reaches its bound"
    );

    shutdown.store(true, Ordering::Release);
    let mut attempts = 0;
    let error = retry_busy_begin(
        || {
            attempts += 1;
            clock.advance(Duration::from_millis(1));
            Err::<(), _>(sqlite_busy(format!("attempt {attempts}")))
        },
        &shutdown,
        || false,
    )
    .unwrap_err();

    assert_eq!(attempts, 1, "shutdown must end the retry at the first busy");
    assert!(is_busy_with(&error, "attempt 1"), "{error:?}");
}

#[test]
fn immediate_begin_busy_retry_honors_cancellation() {
    let shutdown = AtomicBool::new(false);
    let cancelled = AtomicBool::new(false);
    let mut attempts = 0;

    let error = retry_busy_begin(
        || {
            attempts += 1;
            if attempts == 1 {
                cancelled.store(true, Ordering::Release);
            }
            Err::<(), _>(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                Some("original database lock".to_owned()),
            ))
        },
        &shutdown,
        || cancelled.load(Ordering::Acquire),
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
fn immediate_begin_shutdown_after_busy_never_publishes_late_success() {
    let shutdown = AtomicBool::new(false);
    let mut attempts = 0;

    let error = retry_busy_begin(
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
        || false,
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
