use super::*;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

/// The first busy error a retry saw; the retry must surface this one.
#[derive(Debug, PartialEq)]
struct OriginalLock;

impl ErrorExt for OriginalLock {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Busy
    }
}

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
                || false,
            )
            .unwrap();
            transaction.rollback().unwrap();
        });
        while !started.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        // Hold past a yield-only attempt budget (~50µs for 64 yields) so the
        // contender must wait on the write-lock acquire bound, not burn out.
        std::thread::sleep(Duration::from_millis(5));
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

    let started = Instant::now();
    let error = super::super::command::begin_transaction_with_busy_retry(
        &contender,
        TransactionBehavior::Immediate,
        &shutdown,
        || false,
    )
    .unwrap_err();
    let elapsed = started.elapsed();

    assert_eq!(error.kind(), ErrorKind::Busy, "{error}");
    // Must not borrow the transaction idle lease (2s in tests / 30s in prod).
    assert!(
        elapsed < EXACT_SQL_TRANSACTION_IDLE_LIMIT / 2,
        "busy acquire waited {elapsed:?}, which reaches toward the idle lease"
    );
    assert!(
        elapsed >= Duration::from_millis(32),
        "busy acquire returned in {elapsed:?}, below the measured 64ms write-lock bound"
    );

    shutdown.store(true, Ordering::Release);
    let started = Instant::now();
    let error = super::super::command::begin_transaction_with_busy_retry(
        &contender,
        TransactionBehavior::Immediate,
        &shutdown,
        || false,
    )
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Busy, "{error}");
    assert!(
        started.elapsed() < Duration::from_millis(50),
        "shutdown-aware busy acquire stalled for {:?}",
        started.elapsed()
    );
}

#[test]
fn immediate_begin_busy_retry_honors_cancellation() {
    let shutdown = AtomicBool::new(false);
    let cancelled = AtomicBool::new(false);
    let mut attempts = 0;
    let started = Instant::now();

    let error = super::super::command::retry_busy_begin(
        || {
            attempts += 1;
            if attempts == 1 {
                cancelled.store(true, Ordering::Release);
            }
            Err::<(), _>(OriginalLock)
        },
        &shutdown,
        || cancelled.load(Ordering::Acquire),
    )
    .unwrap_err();

    assert_eq!(attempts, 1);
    assert!(started.elapsed() < Duration::from_millis(50));
    assert_eq!(error, OriginalLock);
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
                Err(OriginalLock)
            } else {
                Ok(())
            }
        },
        &shutdown,
        || false,
    )
    .unwrap_err();

    assert_eq!(attempts, 1);
    assert_eq!(error, OriginalLock);
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

/// Fails if any exact SQL lane lets `ATTACH`/`DETACH` past its authorizer.
/// The writer connection also caps attachments at zero, so an authorizer
/// regression would still fail at run time; only `SQLITE_AUTH` proves the
/// statement was refused at prepare.
#[test]
fn attach_and_detach_are_denied_on_every_exact_sql_lane() {
    fn assert_not_authorized<T: std::fmt::Debug>(
        lane: &str,
        sql: &str,
        result: Result<T, ExactSqlError>,
    ) {
        let error = result.expect_err("ATTACH/DETACH must be refused");
        assert!(
            matches!(
                error,
                ExactSqlError::Sqlite {
                    kind: ErrorKind::Denied,
                    ..
                }
            ),
            "{lane} must deny `{sql}` at the authorizer, got {error:?}"
        );
    }

    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    let denied = [
        ("ATTACH DATABASE ':memory:' AS denied_input", vec![]),
        // SQLite reports a parameter-bound filename as an unknown action.
        (
            "ATTACH DATABASE ?1 AS denied_input",
            vec![ExactSqlValue::Text(":memory:".to_owned())],
        ),
        ("DETACH DATABASE denied_input", vec![]),
    ];
    for (sql, params) in denied.clone() {
        assert_not_authorized(
            "one-shot writer",
            sql,
            channel.execute(statement(sql, params.clone())),
        );
        if params.is_empty() {
            assert_not_authorized("writer batch", sql, channel.execute_batch(sql.to_owned()));
        }
        assert_not_authorized(
            "reader",
            sql,
            channel.query(statement(sql, params), Duration::from_secs(1)),
        );
    }
    // The transaction holds the writer lease, so one-shot writes must not run
    // while it is open.
    let transaction = channel.begin_immediate().unwrap();
    for (sql, params) in denied {
        assert_not_authorized(
            "writer transaction",
            sql,
            transaction.execute(statement(sql, params)),
        );
    }
    let databases = transaction
        .query(statement("PRAGMA database_list", vec![]))
        .unwrap();
    assert!(
        databases.rows.iter().all(|row| {
            !matches!(row.values.get(1), Some(ExactSqlValue::Text(name)) if name == "denied_input")
        }),
        "no denied database may be attached: {databases:?}"
    );
    transaction.rollback().unwrap();
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

/// Fails if a leased commit that SQLite refuses leaves its writes behind or
/// keeps the writer pinned: the refused COMMIT must roll the lease back.
#[test]
fn leased_commit_failure_rolls_back_and_releases_writer() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch(
            "CREATE TABLE leased_parent(id INTEGER PRIMARY KEY);
             CREATE TABLE leased_child(id INTEGER PRIMARY KEY, parent_id INTEGER NOT NULL,
                 FOREIGN KEY(parent_id) REFERENCES leased_parent(id)
                 DEFERRABLE INITIALLY DEFERRED)"
                .to_owned(),
        )
        .unwrap();
    let transaction = channel.begin_immediate().unwrap();
    transaction
        .execute(statement("INSERT INTO leased_child VALUES (1, 9)", vec![]))
        .unwrap();

    let error = transaction.commit().unwrap_err();

    assert!(
        matches!(
            error,
            ExactSqlError::Sqlite {
                kind: ErrorKind::Constraint,
                ..
            }
        ),
        "{error:?}"
    );
    let rows = channel
        .query(
            statement("SELECT count(*) FROM leased_child", vec![]),
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(rows.rows[0].values, vec![ExactSqlValue::Integer(0)]);
    channel
        .execute(statement("INSERT INTO leased_parent VALUES (9)", vec![]))
        .unwrap();
}

/// Fails if a failed rollback after a failed RETURNING write drops the write
/// error that caused the rollback.
#[test]
fn rollback_failure_keeps_the_write_error() {
    let write = ExactSqlError::Sqlite {
        operation: "commit returning write transaction",
        kind: ErrorKind::Constraint,
        message: "FOREIGN KEY constraint failed".to_owned(),
    };
    let rollback = ExactSqlError::Sqlite {
        operation: "rollback write transaction",
        kind: ErrorKind::IoErr,
        message: "disk I/O error".to_owned(),
    };

    let error = super::super::command::rollback_failed_after(write, rollback);

    let (operation, kind, message) = match error {
        ExactSqlError::Sqlite {
            operation,
            kind,
            message,
        } => (operation, kind, message),
        other => panic!("expected the rollback failure, got {other:?}"),
    };
    assert_eq!(operation, "rollback write transaction");
    assert_eq!(kind, ErrorKind::IoErr);
    assert!(message.contains("disk I/O error"), "{message}");
    assert!(
        message.contains("FOREIGN KEY constraint failed"),
        "{message}"
    );
}
