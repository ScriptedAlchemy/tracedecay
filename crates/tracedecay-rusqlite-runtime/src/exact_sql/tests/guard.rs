use super::*;

#[test]
fn queued_write_rechecks_authority_on_actor_dequeue() {
    let fixture = fixture('a', 'a');
    let holder = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    let allowed = Arc::new(AtomicBool::new(true));
    let transaction = holder.begin_immediate().unwrap();
    let (reply, receive) = async_channel::bounded(1);
    assert!(
        holder
            .writer
            .as_ref()
            .unwrap()
            .try_send(WriterCommand::Dispatch {
                request: SqlRequest::ExecuteBatch(
                    "CREATE TABLE denied_after_queue (value INTEGER)".to_owned(),
                ),
                reply,
                authority: Some(Arc::new(AtomicWriteAuthority(Arc::clone(&allowed)))),
            })
            .is_ok()
    );

    allowed.store(false, Ordering::Release);
    transaction.rollback().unwrap();
    let error = receive.recv_blocking().unwrap().unwrap_err();

    assert!(matches!(error, ExactSqlError::AuthorityDenied(_)));
    let rows = holder
        .query(
            statement(
                "SELECT count(*) FROM sqlite_schema
                     WHERE name = 'denied_after_queue'",
                vec![],
            ),
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(rows.rows[0].values, vec![ExactSqlValue::Integer(0)]);
}

#[test]
fn revoked_commit_rolls_back_pinned_transaction() {
    let fixture = fixture('a', 'a');
    let base = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    base.execute_batch("CREATE TABLE denied_commit (value INTEGER)".to_owned())
        .unwrap();
    let allowed = Arc::new(AtomicBool::new(true));
    let guarded = ExactSqlHandle::attach(&fixture.writer, &fixture.readers)
        .unwrap()
        .with_write_authority(Arc::new(AtomicWriteAuthority(Arc::clone(&allowed))))
        .unwrap();
    let transaction = guarded.begin_immediate().unwrap();
    transaction
        .execute(statement("INSERT INTO denied_commit VALUES (1)", vec![]))
        .unwrap();

    allowed.store(false, Ordering::Release);
    let error = transaction.commit().unwrap_err();

    assert!(matches!(error, ExactSqlError::AuthorityDenied(_)));
    let rows = base
        .query(
            statement("SELECT count(*) FROM denied_commit", vec![]),
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(rows.rows[0].values, vec![ExactSqlValue::Integer(0)]);
}

#[test]
fn revoked_pinned_dispatch_rolls_back_and_releases_writer() {
    let fixture = fixture('a', 'a');
    let base = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    base.execute_batch("CREATE TABLE denied_dispatch (value INTEGER)".to_owned())
        .unwrap();
    let allowed = Arc::new(AtomicBool::new(true));
    let guarded = ExactSqlHandle::attach(&fixture.writer, &fixture.readers)
        .unwrap()
        .with_write_authority(Arc::new(AtomicWriteAuthority(Arc::clone(&allowed))))
        .unwrap();
    let transaction = guarded.begin_immediate().unwrap();
    transaction
        .execute(statement("INSERT INTO denied_dispatch VALUES (1)", vec![]))
        .unwrap();

    allowed.store(false, Ordering::Release);
    let error = transaction
        .execute(statement("INSERT INTO denied_dispatch VALUES (2)", vec![]))
        .unwrap_err();

    assert!(matches!(error, ExactSqlError::AuthorityDenied(_)));
    let rows = base
        .query(
            statement("SELECT count(*) FROM denied_dispatch", vec![]),
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(rows.rows[0].values, vec![ExactSqlValue::Integer(0)]);
}

#[test]
fn pinned_batch_allows_named_savepoint_rollback_and_release() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch("CREATE TABLE savepoint_value (value INTEGER NOT NULL)".to_owned())
        .unwrap();
    let transaction = channel.begin_immediate().unwrap();

    transaction
        .execute_batch(
            "SAVEPOINT projection_collision_guard;
                 INSERT INTO savepoint_value VALUES (1);
                 ROLLBACK TO projection_collision_guard;
                 RELEASE projection_collision_guard;"
                .to_owned(),
        )
        .unwrap();
    transaction.commit().unwrap();

    let rows = channel
        .query(
            statement("SELECT count(*) FROM savepoint_value", vec![]),
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(rows.rows[0].values, vec![ExactSqlValue::Integer(0)]);
}

#[test]
fn pinned_batch_allows_schema_install_ddl() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch("CREATE TABLE old_name (value INTEGER NOT NULL)".to_owned())
        .unwrap();
    let transaction = channel.begin_immediate().unwrap();

    transaction
        .execute_batch(
            "ALTER TABLE old_name RENAME TO new_name;
                 CREATE INDEX new_name_value ON new_name(value);
                 DROP INDEX new_name_value;"
                .to_owned(),
        )
        .unwrap();
    transaction.commit().unwrap();

    let rows = channel
        .query(
            statement(
                "SELECT count(*) FROM sqlite_schema WHERE name = 'new_name'",
                vec![],
            ),
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(rows.rows[0].values, vec![ExactSqlValue::Integer(1)]);
}

#[test]
fn exact_sql_guard_restores_authorizer_after_panic() {
    let clock = FakeClock::frozen();
    clock.install();
    let connection = policy_connection(ConnectionMode::Writer);

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _: Result<(), ExactSqlError> = with_exact_sql_guard(
            &connection,
            exact_writer(false),
            None,
            None,
            true,
            None,
            || panic!("exact SQL operation panic"),
        );
    }));

    assert!(panic.is_err());
    clock.advance(EXACT_SQL_EXECUTION_LIMIT);
    let sum: i64 = connection
        .query_row(
            "WITH RECURSIVE n(value) AS (
                    VALUES(1)
                    UNION ALL
                    SELECT value + 1 FROM n WHERE value < 2000
                 )
                 SELECT sum(value) FROM n",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sum, 2_001_000);
    connection
        .execute_batch("CREATE TABLE protected (value INTEGER)")
        .unwrap();
    assert!(connection.execute_batch("DROP TABLE protected").is_err());
}

fn policy_connection(mode: ConnectionMode) -> rusqlite::Connection {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    connection
        .execute_batch("CREATE TABLE guarded_rows (id INTEGER PRIMARY KEY, payload TEXT NOT NULL); INSERT INTO guarded_rows VALUES (1, 'row');")
        .unwrap();
    crate::connection::install_authorizer(&connection, mode).unwrap();
    connection
}

fn exact_writer(allow_savepoints: bool) -> ExactSqlAuthority {
    ExactSqlAuthority::Writer {
        allow_savepoints,
        database_operation: None,
    }
}

fn prepare_under(
    connection: &rusqlite::Connection,
    authority: ExactSqlAuthority,
    sql: &str,
) -> Result<(), ExactSqlError> {
    with_exact_sql_guard(connection, authority, None, None, false, None, || {
        crate::exact_sql::guard::prepare_cached(connection, sql)
            .map(drop)
            .map_err(|error| sqlite_error("prepare", error))
    })
}

#[test]
fn guarded_statements_stay_compiled_across_operations() {
    let connection = policy_connection(ConnectionMode::Writer);
    let mut reprepares = 0;
    for _ in 0..64 {
        reprepares += with_exact_sql_guard(
            &connection,
            exact_writer(false),
            None,
            None,
            true,
            None,
            || {
                let mut statement = crate::exact_sql::guard::prepare_cached(
                    &connection,
                    "SELECT payload FROM guarded_rows WHERE id = ?1",
                )
                .unwrap();
                let payload: String = statement.query_row([1_i64], |row| row.get(0)).unwrap();
                assert_eq!(payload, "row");
                Ok(statement.reset_status(rusqlite::StatementStatus::RePrepare))
            },
        )
        .unwrap();
        let canonical: i64 = connection
            .prepare_cached("SELECT count(*) FROM guarded_rows")
            .unwrap()
            .query_row([], |row| row.get(0))
            .unwrap();
        assert_eq!(canonical, 1);
    }
    assert_eq!(
        reprepares, 0,
        "an exact SQL operation must not expire the connection's cached statements"
    );
}

#[test]
fn cached_statement_is_never_reused_under_a_stricter_authority() {
    let reader = policy_connection(ConnectionMode::Reader);
    reader.prepare_cached("SAVEPOINT probe").unwrap();
    assert!(matches!(
        prepare_under(
            &reader,
            ExactSqlAuthority::Canonical(ConnectionMode::Reader),
            "SAVEPOINT probe"
        ),
        Err(ExactSqlError::TransactionControlDenied)
    ));

    let writer = policy_connection(ConnectionMode::Writer);
    prepare_under(&writer, exact_writer(true), "SAVEPOINT probe").unwrap();
    assert!(matches!(
        prepare_under(&writer, exact_writer(false), "SAVEPOINT probe"),
        Err(ExactSqlError::TransactionControlDenied)
    ));

    prepare_under(&writer, exact_writer(false), "DROP TABLE guarded_rows").unwrap();
    assert!(writer.prepare_cached("DROP TABLE guarded_rows").is_err());
}

#[test]
fn cache_key_suffixes_fit_the_reserved_length() {
    use AuthorizedDatabaseOperation::{Attach, Detach};
    let authorities = [
        ExactSqlAuthority::Canonical(ConnectionMode::Writer),
        ExactSqlAuthority::Canonical(ConnectionMode::Reader),
        ExactSqlAuthority::Canonical(ConnectionMode::Maintenance),
    ]
    .into_iter()
    .chain([false, true].into_iter().flat_map(|allow_savepoints| {
        [None, Some(Attach), Some(Detach("aux".to_owned()))]
            .into_iter()
            .map(move |database_operation| ExactSqlAuthority::Writer {
                allow_savepoints,
                database_operation,
            })
    }));
    let suffixes: Vec<_> = authorities
        .map(|authority| authority.cache_key_suffix())
        .collect();
    let distinct: std::collections::HashSet<_> = suffixes.iter().collect();
    assert_eq!(distinct.len(), suffixes.len());
    assert!(
        suffixes
            .iter()
            .all(|suffix| suffix.len() <= crate::exact_sql::guard::MAX_CACHE_KEY_SUFFIX_BYTES)
    );
}
