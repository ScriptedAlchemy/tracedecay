use super::*;

#[test]
fn execute_batch_execute_and_query_use_owned_dtos() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch(
            "CREATE TABLE migrated (
                    id INTEGER PRIMARY KEY,
                    score REAL,
                    label TEXT,
                    payload BLOB,
                    optional TEXT
                )"
            .to_owned(),
        )
        .unwrap();

    let executed = channel
        .execute(statement(
            "INSERT INTO migrated VALUES (?, ?, ?, ?, ?)",
            vec![
                ExactSqlValue::Integer(7),
                ExactSqlValue::Real(2.5),
                ExactSqlValue::Text("owned".to_owned()),
                ExactSqlValue::Blob(vec![1, 2, 3]),
                ExactSqlValue::Null,
            ],
        ))
        .unwrap();
    let rows = channel
        .query(
            statement(
                "SELECT id, score, label, payload, optional FROM migrated",
                vec![],
            ),
            Duration::from_secs(1),
        )
        .unwrap();

    assert_eq!(executed.changed_rows, 1);
    assert_eq!(
        rows.columns,
        vec!["id", "score", "label", "payload", "optional"]
    );
    assert_eq!(
        rows.rows,
        vec![ExactSqlRow {
            values: vec![
                ExactSqlValue::Integer(7),
                ExactSqlValue::Real(2.5),
                ExactSqlValue::Text("owned".to_owned()),
                ExactSqlValue::Blob(vec![1, 2, 3]),
                ExactSqlValue::Null,
            ],
        }]
    );
}

#[test]
fn statement_admission_limits_accept_boundaries_and_reject_oversize() {
    assert!(ExactSqlStatement::new("x".repeat(MAX_SQL_BYTES), vec![]).is_ok());
    assert!(matches!(
        ExactSqlStatement::new("x".repeat(MAX_SQL_BYTES + 1), vec![]),
        Err(ExactSqlError::RequestLimitExceeded)
    ));
    assert!(
        ExactSqlStatement::new(
            "SELECT 1".to_owned(),
            vec![ExactSqlValue::Null; MAX_SQL_PARAMETERS],
        )
        .is_ok()
    );
    assert!(matches!(
        ExactSqlStatement::new(
            "SELECT 1".to_owned(),
            vec![ExactSqlValue::Null; MAX_SQL_PARAMETERS + 1],
        ),
        Err(ExactSqlError::RequestLimitExceeded)
    ));
}

#[test]
fn batch_admission_rejects_oversize_before_enqueue() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

    let error = channel
        .execute_batch("x".repeat(MAX_SQL_BYTES + 1))
        .unwrap_err();

    assert!(matches!(error, ExactSqlError::RequestLimitExceeded));
}

#[test]
fn validate_checks_syntax_and_schema_on_the_writer_actor() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

    let missing = channel
        .validate(statement("SELECT value FROM missing_table", vec![]))
        .unwrap_err();
    let syntax = channel
        .validate(statement("SELECT FROM", vec![]))
        .unwrap_err();

    assert!(matches!(missing, ExactSqlError::Sqlite { .. }));
    assert!(matches!(syntax, ExactSqlError::Sqlite { .. }));
}

#[test]
fn batch_reports_last_insert_rowid() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch(
            "CREATE TABLE batch_id (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    value TEXT NOT NULL
                )"
            .to_owned(),
        )
        .unwrap();

    let result = channel
        .execute_batch(
            "INSERT INTO batch_id(value) VALUES ('first');
                 INSERT INTO batch_id(value) VALUES ('second');"
                .to_owned(),
        )
        .unwrap();

    assert_eq!(result.changed_rows, 2);
    assert_eq!(result.last_insert_rowid, 2);
    assert_eq!(channel.last_insert_rowid(), 2);
}

#[test]
fn rowid_is_handle_local_and_changes_only_after_applied_insert() {
    let fixture = fixture('a', 'a');
    let channel_a = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    let channel_b = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel_a
        .execute_batch(
            "CREATE TABLE rowids (
                    id INTEGER PRIMARY KEY,
                    value TEXT NOT NULL UNIQUE
                )"
            .to_owned(),
        )
        .unwrap();

    let a = channel_a
        .execute(statement(
            "INSERT INTO rowids(value) VALUES (?)",
            vec![ExactSqlValue::Text("a".to_owned())],
        ))
        .unwrap();
    let b = channel_b
        .execute(statement(
            "INSERT INTO rowids(value) VALUES (?)",
            vec![ExactSqlValue::Text("b".to_owned())],
        ))
        .unwrap();
    assert_eq!(a.last_insert_rowid, 1);
    assert_eq!(b.last_insert_rowid, 2);

    let update = channel_a
        .execute(statement(
            "UPDATE rowids SET value = ? WHERE id = 1",
            vec![ExactSqlValue::Text("updated".to_owned())],
        ))
        .unwrap();
    channel_a
        .validate(statement("SELECT value FROM rowids", vec![]))
        .unwrap();
    channel_a
        .query(
            statement("SELECT value FROM rowids WHERE id = 1", vec![]),
            Duration::from_secs(1),
        )
        .unwrap();
    let ignored = channel_a
        .execute(statement(
            "INSERT OR IGNORE INTO rowids(value) VALUES (?)",
            vec![ExactSqlValue::Text("b".to_owned())],
        ))
        .unwrap();
    let upsert_update = channel_a
        .execute(statement(
            "INSERT INTO rowids(id, value) VALUES (2, 'b')
                 ON CONFLICT(id) DO UPDATE SET value = excluded.value",
            vec![],
        ))
        .unwrap();

    assert_eq!(update.last_insert_rowid, 1);
    assert_eq!(ignored.last_insert_rowid, 1);
    assert_eq!(upsert_update.last_insert_rowid, 1);
    assert_eq!(channel_a.last_insert_rowid(), 1);
    assert_eq!(channel_b.last_insert_rowid(), 2);

    let explicit = channel_a
        .execute(statement(
            "INSERT INTO rowids(id, value) VALUES (?, ?)",
            vec![
                ExactSqlValue::Integer(41),
                ExactSqlValue::Text("explicit".to_owned()),
            ],
        ))
        .unwrap();
    assert_eq!(explicit.last_insert_rowid, 41);
    assert_eq!(channel_a.last_insert_rowid(), 41);
}

#[test]
fn partial_batch_error_still_publishes_applied_insert_rowid() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch(
            "CREATE TABLE partial_rowid (
                    id INTEGER PRIMARY KEY,
                    value TEXT NOT NULL
                )"
            .to_owned(),
        )
        .unwrap();

    let error = channel
        .execute_batch(
            "INSERT INTO partial_rowid(value) VALUES ('autocommit');
                 INSERT INTO missing_table(value) VALUES ('fail');"
                .to_owned(),
        )
        .unwrap_err();

    assert!(matches!(error, ExactSqlError::Sqlite { .. }));
    assert_eq!(channel.last_insert_rowid(), 1);

    let transaction = channel.begin_immediate().unwrap();
    let error = transaction
        .execute_batch(
            "INSERT INTO partial_rowid(value) VALUES ('pinned');
                 INSERT INTO missing_table(value) VALUES ('fail');"
                .to_owned(),
        )
        .unwrap_err();
    assert!(matches!(error, ExactSqlError::Sqlite { .. }));
    assert_eq!(channel.last_insert_rowid(), 2);
    transaction.rollback().unwrap();
    assert_eq!(channel.last_insert_rowid(), 2);
}

#[test]
fn transaction_insert_returning_publishes_rowid() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch(
            "CREATE TABLE returning_rowid (
                    id INTEGER PRIMARY KEY,
                    value TEXT NOT NULL
                )"
            .to_owned(),
        )
        .unwrap();
    let transaction = channel.begin_immediate().unwrap();

    let rows = transaction
        .query(statement(
            "INSERT INTO returning_rowid(value) VALUES ('value') RETURNING id",
            vec![],
        ))
        .unwrap();

    assert_eq!(rows.rows[0].values, vec![ExactSqlValue::Integer(1)]);
    assert_eq!(channel.last_insert_rowid(), 1);
    transaction.rollback().unwrap();
}
