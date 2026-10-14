use super::*;

#[test]
fn query_materialization_is_bounded() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

    let error = channel
        .query(
            statement(
                "WITH RECURSIVE n(value) AS (
                        VALUES(1)
                        UNION ALL
                        SELECT value + 1 FROM n WHERE value <= 10000
                    )
                    SELECT value FROM n",
                vec![],
            ),
            Duration::from_secs(1),
        )
        .unwrap_err();

    assert!(matches!(error, ExactSqlError::QueryLimitExceeded));
}

#[test]
fn query_execution_time_is_bounded() {
    // `execute_query` is the reader worker's statement path. Each read of this
    // thread's clock moves it a quarter of the limit, so the statement's own
    // progress callbacks carry it past its deadline.
    let clock = FakeClock::ticking(EXACT_SQL_EXECUTION_LIMIT / 4);
    clock.install();
    let connection = rusqlite::Connection::open_in_memory().unwrap();

    let error = execute_query(
        &connection,
        statement(
            "WITH RECURSIVE n(value) AS (
                    VALUES(1)
                    UNION ALL
                    SELECT value + 1 FROM n WHERE value < 10000
                )
                SELECT count(*) FROM n AS left_n CROSS JOIN n AS right_n",
            vec![],
        ),
        ExactSqlStatementBudget::Request,
    )
    .unwrap_err();

    assert!(matches!(error, ExactSqlError::Sqlite { code: Some(9), .. }));
    // The interrupt must land near the execution limit, not multiples beyond.
    assert!(clock.elapsed() < EXACT_SQL_EXECUTION_LIMIT * 2);
}

#[test]
fn convergence_query_runs_past_the_request_deadline() {
    // A store-sized audit scan on a large store outlives any request
    // deadline; interrupting it reproduces #3760's "SQLite advance query
    // failed: interrupted" on every open.
    let clock = FakeClock::ticking(EXACT_SQL_EXECUTION_LIMIT / 4);
    clock.install();
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    let scan = || {
        statement(
            "WITH RECURSIVE n(value) AS (
                    VALUES(1)
                    UNION ALL
                    SELECT value + 1 FROM n WHERE value < 100000
                )
                SELECT count(*) FROM n",
            vec![],
        )
    };

    let error = execute_query(&connection, scan(), ExactSqlStatementBudget::Request).unwrap_err();
    assert!(
        matches!(
            error,
            ExactSqlError::Sqlite {
                operation: "advance query",
                code: Some(9),
                ..
            }
        ),
        "a request-budget scan past the deadline is interrupted: {error}"
    );

    let rows = execute_query(&connection, scan(), ExactSqlStatementBudget::Convergence)
        .expect("a convergence-budget scan has no wall-clock deadline");
    assert_eq!(rows.rows[0].values, vec![ExactSqlValue::Integer(100_000)]);
}

#[test]
fn until_budget_interrupts_at_its_deadline_not_the_request_limit() {
    // A doctor audit classifies an interrupt past its deadline as an
    // exhausted budget, so only that deadline may interrupt its statements.
    let clock = FakeClock::ticking(EXACT_SQL_EXECUTION_LIMIT / 4);
    clock.install();
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    let deadline = clock::now() + EXACT_SQL_EXECUTION_LIMIT * 3;

    let error = execute_query(
        &connection,
        statement(
            "WITH RECURSIVE n(value) AS (
                    VALUES(1)
                    UNION ALL
                    SELECT value + 1 FROM n WHERE value < 10000
                )
                SELECT count(*) FROM n AS left_n CROSS JOIN n AS right_n",
            vec![],
        ),
        ExactSqlStatementBudget::Until(deadline),
    )
    .unwrap_err();

    assert!(matches!(error, ExactSqlError::Sqlite { code: Some(9), .. }));
    assert!(
        clock.elapsed() >= EXACT_SQL_EXECUTION_LIMIT * 3,
        "interrupted at {:?}, before its own deadline",
        clock.elapsed()
    );
    assert!(clock.elapsed() < EXACT_SQL_EXECUTION_LIMIT * 4);
}

#[test]
fn invalid_sqlite_text_is_rejected_without_lossy_conversion() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch(
            "CREATE TABLE invalid_text (value TEXT NOT NULL);
                 INSERT INTO invalid_text(value) VALUES (CAST(x'80' AS TEXT));"
                .to_owned(),
        )
        .unwrap();

    let error = channel
        .query(
            statement("SELECT value FROM invalid_text", vec![]),
            Duration::from_secs(1),
        )
        .unwrap_err();

    assert!(matches!(
        error,
        ExactSqlError::Sqlite {
            operation: "decode query text",
            ..
        }
    ));
}

#[test]
fn sqlite_errors_preserve_primary_and_extended_codes() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch("CREATE TABLE unique_value (value INTEGER UNIQUE)".to_owned())
        .unwrap();
    channel
        .execute(statement(
            "INSERT INTO unique_value VALUES (?)",
            vec![ExactSqlValue::Integer(1)],
        ))
        .unwrap();

    let error = channel
        .execute(statement(
            "INSERT INTO unique_value VALUES (?)",
            vec![ExactSqlValue::Integer(1)],
        ))
        .unwrap_err();

    assert!(matches!(
        error,
        ExactSqlError::Sqlite {
            code: Some(19),
            extended_code: Some(2067),
            ..
        }
    ));
}

#[test]
fn read_snapshot_stays_frozen_across_queries() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch("CREATE TABLE frozen (value INTEGER NOT NULL)".to_owned())
        .unwrap();
    channel
        .execute(statement(
            "INSERT INTO frozen VALUES (?)",
            vec![ExactSqlValue::Integer(1)],
        ))
        .unwrap();
    let snapshot = channel.begin_read_snapshot(Duration::from_secs(1)).unwrap();
    let first = snapshot
        .query(statement("SELECT count(*) FROM frozen", vec![]))
        .unwrap();
    channel
        .execute(statement(
            "INSERT INTO frozen VALUES (?)",
            vec![ExactSqlValue::Integer(2)],
        ))
        .unwrap();

    let frozen = snapshot
        .query(statement("SELECT count(*) FROM frozen", vec![]))
        .unwrap();

    assert_eq!(first.rows[0].values, vec![ExactSqlValue::Integer(1)]);
    assert_eq!(frozen.rows[0].values, vec![ExactSqlValue::Integer(1)]);
    drop(snapshot);
    let current = channel
        .query(
            statement("SELECT count(*) FROM frozen", vec![]),
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(current.rows[0].values, vec![ExactSqlValue::Integer(2)]);
}

#[test]
fn health_snapshot_retires_the_reserved_reader() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

    let snapshot = channel
        .begin_health_read_snapshot(Duration::from_secs(1))
        .unwrap();
    assert_eq!(fixture.readers.snapshot().leased_health, 1);
    drop(snapshot);

    let pool = fixture.readers.snapshot();
    assert_eq!(pool.leased_health, 0);
    assert_eq!(pool.health_workers, 0);
}
