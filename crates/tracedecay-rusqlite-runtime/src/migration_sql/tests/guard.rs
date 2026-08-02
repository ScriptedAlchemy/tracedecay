use super::*;

#[test]
fn queued_write_rechecks_authority_on_actor_dequeue() {
    let fixture = fixture('a', 'a');
    let holder = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    let allowed = Arc::new(AtomicBool::new(true));
    let transaction = holder.begin_immediate().unwrap();
    let (reply, receive) = std::sync::mpsc::sync_channel(1);
    assert!(
        holder
            .writer
            .as_ref()
            .unwrap()
            .try_send(WriterCommand::Dispatch {
                request: MigrationSqlRequest::ExecuteBatch(
                    "CREATE TABLE denied_after_queue (value INTEGER)".to_owned(),
                ),
                reply,
                last_insert_rowid: Arc::new(AtomicI64::new(0)),
                authority: Some(Arc::new(AtomicWriteAuthority(Arc::clone(&allowed)))),
            })
            .is_ok()
    );

    allowed.store(false, Ordering::Release);
    transaction.rollback().unwrap();
    let error = receive
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap_err();

    assert!(matches!(error, MigrationSqlError::AuthorityDenied(_)));
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
    assert_eq!(rows.rows[0].values, vec![MigrationSqlValue::Integer(0)]);
}

#[test]
fn revoked_commit_rolls_back_pinned_transaction() {
    let fixture = fixture('a', 'a');
    let base = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    base.execute_batch("CREATE TABLE denied_commit (value INTEGER)".to_owned())
        .unwrap();
    let allowed = Arc::new(AtomicBool::new(true));
    let guarded = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers)
        .unwrap()
        .with_write_authority(Arc::new(AtomicWriteAuthority(Arc::clone(&allowed))))
        .unwrap();
    let transaction = guarded.begin_immediate().unwrap();
    transaction
        .execute(statement("INSERT INTO denied_commit VALUES (1)", vec![]))
        .unwrap();

    allowed.store(false, Ordering::Release);
    let error = transaction.commit().unwrap_err();

    assert!(matches!(error, MigrationSqlError::AuthorityDenied(_)));
    let rows = base
        .query(
            statement("SELECT count(*) FROM denied_commit", vec![]),
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(rows.rows[0].values, vec![MigrationSqlValue::Integer(0)]);
}

#[test]
fn revoked_pinned_dispatch_rolls_back_and_releases_writer() {
    let fixture = fixture('a', 'a');
    let base = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    base.execute_batch("CREATE TABLE denied_dispatch (value INTEGER)".to_owned())
        .unwrap();
    let allowed = Arc::new(AtomicBool::new(true));
    let guarded = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers)
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

    assert!(matches!(error, MigrationSqlError::AuthorityDenied(_)));
    let rows = base
        .query(
            statement("SELECT count(*) FROM denied_dispatch", vec![]),
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(rows.rows[0].values, vec![MigrationSqlValue::Integer(0)]);
}

#[test]
fn pinned_batch_allows_named_savepoint_rollback_and_release() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
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
    assert_eq!(rows.rows[0].values, vec![MigrationSqlValue::Integer(0)]);
}

#[test]
fn pinned_batch_allows_schema_install_ddl() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
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
    assert_eq!(rows.rows[0].values, vec![MigrationSqlValue::Integer(1)]);
}

#[test]
fn unpinned_batch_allows_schema_install_ddl() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch("CREATE TABLE protected (value INTEGER NOT NULL)".to_owned())
        .unwrap();
    let transaction = channel.begin_immediate().unwrap();
    transaction
        .execute_batch("INSERT INTO protected VALUES (1)".to_owned())
        .unwrap();
    transaction.commit().unwrap();

    channel
        .execute_batch("DROP TABLE protected".to_owned())
        .unwrap();
}

#[test]
fn migration_guard_restores_authorizer_after_success() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    connection
        .authorizer(Some(crate::connection::authorize_writer))
        .unwrap();
    connection
        .execute_batch("CREATE TABLE protected (value INTEGER)")
        .unwrap();

    with_migration_guard(
        &connection,
        false,
        false,
        None,
        None,
        true,
        None,
        crate::connection::authorize_writer,
        true,
        None,
        None,
        || {
            connection
                .execute_batch("DROP TABLE protected")
                .map_err(|error| sqlite_error("test migration DDL", error))
        },
    )
    .unwrap();

    connection
        .execute_batch("CREATE TABLE protected (value INTEGER)")
        .unwrap();
    assert!(connection.execute_batch("DROP TABLE protected").is_err());
}

#[test]
fn migration_guard_restores_authorizer_after_panic() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    connection
        .authorizer(Some(crate::connection::authorize_writer))
        .unwrap();

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _: Result<(), MigrationSqlError> = with_migration_guard(
            &connection,
            false,
            false,
            None,
            None,
            true,
            None,
            crate::connection::authorize_writer,
            true,
            None,
            None,
            || panic!("migration operation panic"),
        );
    }));

    assert!(panic.is_err());
    std::thread::sleep(MIGRATION_SQL_EXECUTION_LIMIT + Duration::from_millis(50));
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
