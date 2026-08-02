use super::*;

#[test]
fn attach_rejects_different_verified_locators() {
    let fixture = fixture('a', 'b');

    let result = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers);

    assert!(matches!(result, Err(MigrationSqlError::AuthorityMismatch)));
}

#[test]
fn attach_rejects_same_locator_bound_to_different_files() {
    let first = fixture('a', 'a');
    let second = fixture('a', 'a');

    let result = MigrationSqlHandle::attach(&first.writer, &second.readers);

    assert!(matches!(result, Err(MigrationSqlError::AuthorityMismatch)));
}

#[test]
fn read_only_clone_cannot_recover_writer_authority() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    let read_only = channel.read_only_clone();

    let error = read_only
        .execute_batch("CREATE TABLE forbidden (value INTEGER)".to_owned())
        .unwrap_err();

    assert!(matches!(error, MigrationSqlError::WriterUnavailable));
}

#[test]
fn long_lease_transaction_requires_attached_write_authority() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

    let error = match channel.begin_authorized_long_lease_immediate() {
        Ok(_) => panic!("long-lease transaction must require attached authority"),
        Err(error) => error,
    };

    assert!(matches!(error, MigrationSqlError::AuthorityDenied(_)));
}

#[test]
fn writer_actor_allows_only_product_schema_pragmas() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

    for pragma in [
        "PRAGMA auto_vacuum = INCREMENTAL",
        "PRAGMA foreign_keys = ON",
        "PRAGMA defer_foreign_keys = ON",
        "PRAGMA secure_delete = ON",
        "PRAGMA user_version = 24",
    ] {
        channel
            .execute_batch(pragma.to_owned())
            .unwrap_or_else(|error| panic!("{pragma} must remain available: {error}"));
    }
    for pragma in [
        "PRAGMA auto_vacuum = NONE",
        "PRAGMA foreign_keys = OFF",
        "PRAGMA secure_delete = OFF",
        "PRAGMA writable_schema = ON",
    ] {
        let error = channel.execute_batch(pragma.to_owned()).unwrap_err();
        assert!(
            matches!(error, MigrationSqlError::Sqlite { .. }),
            "{pragma}: {error}"
        );
    }
}

/// The projection output-state cache is derived, per-connection scratch
/// rebuilt from `observation_projection_provenance` whenever
/// `PRAGMA data_version` moves. It must be able to exist on this channel;
/// denying it made every projection version migration and rebuild fail.
#[test]
fn writer_actor_allows_temp_tables_and_indexes_but_not_temp_triggers_or_views() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch("CREATE TABLE durable (value INTEGER NOT NULL)".to_owned())
        .expect("durable table remains available");

    // The exact shape the projection output-state cache creates.
    channel
        .execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS observation_projection_output_state (
                    projector_version TEXT NOT NULL,
                    output_provider TEXT NOT NULL,
                    output_message_id TEXT NOT NULL,
                    canonical_observation_id TEXT NOT NULL,
                    latest_observation_id TEXT NOT NULL,
                    latest_sequence INTEGER NOT NULL CHECK(latest_sequence >= 0),
                    projector_owned INTEGER NOT NULL CHECK(projector_owned IN (0, 1)),
                    owner_count INTEGER NOT NULL CHECK(owner_count > 0),
                    PRIMARY KEY(projector_version, output_provider, output_message_id)
                 ) WITHOUT ROWID;"
                .to_owned(),
        )
        .expect("projection output-state cache must be creatable");
    channel
        .execute_batch(
            "CREATE TEMP TABLE scratch (value INTEGER NOT NULL);
                 CREATE INDEX temp.scratch_value ON scratch(value);
                 INSERT INTO temp.scratch(value) VALUES (1);
                 DELETE FROM temp.scratch;
                 DROP INDEX temp.scratch_value;
                 DROP TABLE temp.scratch;"
                .to_owned(),
        )
        .expect("temp scratch must be creatable, writable, and droppable");

    // A temp trigger could mutate durable rows outside the invariant
    // trigger contract, and a temp view has no caller: both stay denied.
    for denied in [
        "CREATE TEMP TRIGGER durable_guard AFTER INSERT ON durable
             BEGIN DELETE FROM durable; END",
        "CREATE TEMP VIEW durable_view AS SELECT value FROM durable",
    ] {
        let error = channel
            .execute_batch(denied.to_owned())
            .expect_err("temp triggers and views must stay denied");
        assert!(
            matches!(error, MigrationSqlError::Sqlite { .. }),
            "{denied}: {error}"
        );
    }
}

#[test]
fn writer_checkpoint_returns_status() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

    let rows = channel.checkpoint_wal_truncate().unwrap();

    assert_eq!(rows.columns.len(), 3);
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(rows.rows[0].values.len(), 3);
}

#[test]
fn ordinary_transaction_cannot_request_an_unbounded_schema_step() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers)
        .unwrap()
        .with_write_authority(Arc::new(AtomicWriteAuthority(Arc::new(AtomicBool::new(
            true,
        )))))
        .unwrap();
    let transaction = channel.begin_immediate().unwrap();

    let error = transaction
        .execute_schema_batch_step("CREATE TABLE forbidden_schema_mode (id INTEGER)".to_owned())
        .unwrap_err();

    assert!(matches!(error, MigrationSqlError::AuthorityDenied(_)));
    transaction.rollback().unwrap();
}

#[test]
fn long_lease_transaction_renews_its_lease_after_successful_bounded_steps() {
    let fixture = fixture('a', 'a');
    let base = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    base.execute_batch("CREATE TABLE lease_probe (value INTEGER)".to_owned())
        .unwrap();
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers)
        .unwrap()
        .with_write_authority(Arc::new(AtomicWriteAuthority(Arc::new(AtomicBool::new(
            true,
        )))))
        .unwrap();
    let transaction = channel.begin_authorized_long_lease_immediate().unwrap();
    let started = Instant::now();

    for value in 0..5 {
        std::thread::sleep(Duration::from_millis(125));
        transaction
            .execute(statement(
                "INSERT INTO lease_probe VALUES (?)",
                vec![MigrationSqlValue::Integer(value)],
            ))
            .unwrap();
    }
    assert!(started.elapsed() > MIGRATION_SQL_TRANSACTION_LIMIT);
    transaction.commit().unwrap();

    let rows = base
        .query(
            statement("SELECT count(*) FROM lease_probe", vec![]),
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(rows.rows[0].values, vec![MigrationSqlValue::Integer(5)]);
}

#[test]
fn explicit_schema_step_has_no_guessed_deadline_and_rechecks_authority() {
    let fixture = fixture('a', 'a');
    let authority = Arc::new(SlowSchemaAuthority {
        execute_batch_checks: AtomicUsize::new(0),
    });
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers)
        .unwrap()
        .with_write_authority(authority.clone())
        .unwrap();
    let transaction = channel.begin_authorized_long_lease_immediate().unwrap();
    let started = Instant::now();

    transaction
        .execute_schema_batch_step(
            "CREATE TABLE long_schema_step (value INTEGER);
                 WITH RECURSIVE n(value) AS (
                     VALUES(1)
                     UNION ALL
                     SELECT value + 1 FROM n WHERE value < 10000
                 )
                 INSERT INTO long_schema_step SELECT value FROM n;"
                .to_owned(),
        )
        .unwrap();

    assert!(started.elapsed() > MIGRATION_SQL_EXECUTION_LIMIT);
    assert!(authority.execute_batch_checks.load(Ordering::Acquire) > 3);
    transaction.rollback().unwrap();
}

#[test]
fn authority_loss_during_schema_step_rolls_back_transaction() {
    let fixture = fixture('a', 'a');
    let base = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers)
        .unwrap()
        .with_write_authority(Arc::new(RevokeDuringSchemaStep {
            execute_batch_checks: AtomicUsize::new(0),
        }))
        .unwrap();
    let transaction = channel.begin_authorized_long_lease_immediate().unwrap();

    let error = transaction
        .execute_schema_batch_step(
            "CREATE TABLE revoked_schema_step (value INTEGER);
                 WITH RECURSIVE n(value) AS (
                     VALUES(1)
                     UNION ALL
                     SELECT value + 1 FROM n WHERE value < 10000
                 )
                 INSERT INTO revoked_schema_step SELECT value FROM n;"
                .to_owned(),
        )
        .unwrap_err();

    assert!(matches!(error, MigrationSqlError::AuthorityDenied(_)));
    let rows = base
        .query(
            statement(
                "SELECT count(*) FROM sqlite_schema WHERE name = 'revoked_schema_step'",
                vec![],
            ),
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(rows.rows[0].values, vec![MigrationSqlValue::Integer(0)]);
}
