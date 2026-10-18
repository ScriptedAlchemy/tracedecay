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
fn batch_reports_changed_rows() {
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
}

#[test]
fn insert_returning_identifies_each_writer_result() {
    let fixture = fixture('a', 'a');
    let channel_a = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    let channel_b = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel_a
        .execute_batch(
            "CREATE TABLE rowids (id INTEGER PRIMARY KEY, value TEXT NOT NULL UNIQUE)".to_owned(),
        )
        .unwrap();

    let first = channel_a
        .execute_returning(statement(
            "INSERT INTO rowids(value) VALUES (?) RETURNING id",
            vec![ExactSqlValue::Text("a".to_owned())],
        ))
        .unwrap();
    let second = channel_b
        .execute_returning(statement(
            "INSERT INTO rowids(value) VALUES (?) RETURNING id",
            vec![ExactSqlValue::Text("b".to_owned())],
        ))
        .unwrap();
    assert_eq!(first.rows[0].values, vec![ExactSqlValue::Integer(1)]);
    assert_eq!(second.rows[0].values, vec![ExactSqlValue::Integer(2)]);

    let a = channel_a.begin_immediate().unwrap();
    let ignored = a
        .query(statement(
            "INSERT OR IGNORE INTO rowids(value) VALUES ('b') RETURNING id",
            vec![],
        ))
        .unwrap();
    let upsert = a
        .query(statement(
            "INSERT INTO rowids(id, value) VALUES (2, 'updated')
         ON CONFLICT(id) DO UPDATE SET value = excluded.value RETURNING id",
            vec![],
        ))
        .unwrap();
    let explicit = a
        .query(statement(
            "INSERT INTO rowids(id, value) VALUES (?, ?) RETURNING id",
            vec![
                ExactSqlValue::Integer(41),
                ExactSqlValue::Text("explicit".to_owned()),
            ],
        ))
        .unwrap();
    a.commit().unwrap();
    assert!(ignored.rows.is_empty());
    assert_eq!(upsert.rows[0].values, vec![ExactSqlValue::Integer(2)]);
    assert_eq!(explicit.rows[0].values, vec![ExactSqlValue::Integer(41)]);
    let rows = channel_b
        .query(
            statement("SELECT id, value FROM rowids ORDER BY id", vec![]),
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(
        rows.rows,
        vec![
            ExactSqlRow {
                values: vec![
                    ExactSqlValue::Integer(1),
                    ExactSqlValue::Text("a".to_owned())
                ]
            },
            ExactSqlRow {
                values: vec![
                    ExactSqlValue::Integer(2),
                    ExactSqlValue::Text("updated".to_owned())
                ]
            },
            ExactSqlRow {
                values: vec![
                    ExactSqlValue::Integer(41),
                    ExactSqlValue::Text("explicit".to_owned())
                ]
            },
        ]
    );
}

#[test]
fn execute_returning_row_limit_error_rolls_back_the_statement() {
    assert_returning_limit_rolls_back(
        "INSERT INTO returning_target(id) SELECT id FROM returning_source RETURNING id",
    );
}

#[test]
fn execute_returning_byte_limit_error_rolls_back_the_statement() {
    assert_returning_limit_rolls_back(
        "INSERT INTO returning_target(id) SELECT id FROM returning_source LIMIT 3
         RETURNING id, zeroblob(25165824)",
    );
}

#[test]
fn execute_returning_commit_failure_rolls_back_and_releases_writer() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch(
            "CREATE TABLE returning_parent(id INTEGER PRIMARY KEY);
             CREATE TABLE returning_child(id INTEGER PRIMARY KEY, parent_id INTEGER NOT NULL,
                 FOREIGN KEY(parent_id) REFERENCES returning_parent(id)
                 DEFERRABLE INITIALLY DEFERRED)"
                .to_owned(),
        )
        .unwrap();
    let error = channel
        .execute_returning(statement(
            "INSERT INTO returning_child VALUES (1, 9) RETURNING id",
            vec![],
        ))
        .unwrap_err();
    assert!(
        matches!(
            error,
            ExactSqlError::Sqlite {
                code: Some(rusqlite::ffi::SQLITE_CONSTRAINT),
                ..
            }
        ),
        "{error:?}"
    );
    let rows = channel
        .query(
            statement("SELECT count(*) FROM returning_child", vec![]),
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(rows.rows[0].values, vec![ExactSqlValue::Integer(0)]);
    channel
        .execute(statement("INSERT INTO returning_parent VALUES (9)", vec![]))
        .unwrap();
    let rows = channel
        .execute_returning(statement(
            "INSERT INTO returning_child VALUES (1, 9) RETURNING id",
            vec![],
        ))
        .unwrap();
    assert_eq!(rows.rows[0].values, vec![ExactSqlValue::Integer(1)]);
}

fn assert_returning_limit_rolls_back(insert: &str) {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch(
            "CREATE TABLE returning_source(id INTEGER PRIMARY KEY);
             CREATE TABLE returning_target(id INTEGER PRIMARY KEY)"
                .to_owned(),
        )
        .unwrap();
    let source_values = (1..=MAX_QUERY_ROWS + 1)
        .map(|id| format!("({id})"))
        .collect::<Vec<_>>()
        .join(",");
    channel
        .execute(statement(
            &format!("INSERT INTO returning_source(id) VALUES {source_values}"),
            vec![],
        ))
        .unwrap();

    let error = channel
        .execute_returning(statement(insert, vec![]))
        .unwrap_err();
    assert!(
        matches!(error, ExactSqlError::QueryLimitExceeded),
        "{error:?}"
    );
    let rows = channel
        .query(
            statement("SELECT count(*) FROM returning_target", vec![]),
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(rows.rows[0].values, vec![ExactSqlValue::Integer(0)]);
    let reopened =
        rusqlite::Connection::open(fixture._directory.path().join("exact-sql.sqlite3")).unwrap();
    assert_eq!(
        reopened
            .query_row("SELECT count(*) FROM returning_target", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );

    let next = channel
        .execute_returning(statement(
            "INSERT INTO returning_target(id) VALUES (1) RETURNING id",
            vec![],
        ))
        .unwrap();
    assert_eq!(next.rows[0].values, vec![ExactSqlValue::Integer(1)]);
    assert_eq!(
        reopened
            .query_row("SELECT count(*) FROM returning_target", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );
}

#[test]
fn partial_batch_error_preserves_autocommit_and_owned_rollback_state() {
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
    let rows = channel
        .query(
            statement("SELECT value FROM partial_rowid ORDER BY id", vec![]),
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(
        rows.rows[0].values,
        vec![ExactSqlValue::Text("autocommit".to_owned())]
    );

    let transaction = channel.begin_immediate().unwrap();
    let error = transaction
        .execute_batch(
            "INSERT INTO partial_rowid(value) VALUES ('pinned');
                 INSERT INTO missing_table(value) VALUES ('fail');"
                .to_owned(),
        )
        .unwrap_err();
    assert!(matches!(error, ExactSqlError::Sqlite { .. }));
    let rows = transaction
        .query(statement("SELECT count(*) FROM partial_rowid", vec![]))
        .unwrap();
    assert_eq!(rows.rows[0].values, vec![ExactSqlValue::Integer(2)]);
    transaction.rollback().unwrap();
    let rows = channel
        .query(
            statement("SELECT value FROM partial_rowid ORDER BY id", vec![]),
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(
        rows.rows[0].values,
        vec![ExactSqlValue::Text("autocommit".to_owned())]
    );
}

#[test]
fn transaction_insert_returning_preserves_rollback() {
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
    transaction.rollback().unwrap();
    let rows = channel
        .query(
            statement("SELECT count(*) FROM returning_rowid", vec![]),
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(rows.rows[0].values, vec![ExactSqlValue::Integer(0)]);
}
