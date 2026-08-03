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
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    let started = Instant::now();

    let error = channel
        .query(
            statement(
                "WITH RECURSIVE n(value) AS (
                        VALUES(1)
                        UNION ALL
                        SELECT value + 1 FROM n WHERE value < 100000
                    )
                    SELECT count(*) FROM n AS left_n CROSS JOIN n AS right_n",
                vec![],
            ),
            Duration::from_secs(1),
        )
        .unwrap_err();

    assert!(matches!(error, ExactSqlError::Sqlite { code: Some(9), .. }));
    assert!(started.elapsed() < Duration::from_secs(2));
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
