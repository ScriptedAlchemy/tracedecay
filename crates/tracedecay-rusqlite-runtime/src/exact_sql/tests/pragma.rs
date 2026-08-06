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
