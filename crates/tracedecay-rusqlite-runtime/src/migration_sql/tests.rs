use std::{
    sync::atomic::AtomicUsize,
    time::{Duration, Instant},
};

use rusqlite::Savepoint;
use tempfile::TempDir;
use tracedecay_domain::LocatorDigest;
use tracedecay_store::{
    AdmissionConfigV1, RepositoryWritePayloadV1, RuntimeReadOutcomeV1, RuntimeReadRequestV1,
    StoreIncarnationV1, StoreRuntimeBindingV1, VerifiedStoreLocatorV1,
};

use crate::{
    ExistingWriterLocator, PersistentWriter, StorageOperationExecutor,
    reader::{ExistingReaderLocator, ReaderPool, ReaderQueryExecutor},
};

use super::*;

struct AtomicWriteAuthority(Arc<AtomicBool>);

impl MigrationSqlWriteAuthority for AtomicWriteAuthority {
    fn verify(&self, _intent: MigrationSqlWriteIntent) -> Result<(), MigrationSqlError> {
        if self.0.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(MigrationSqlError::AuthorityDenied("revoked".to_owned()))
        }
    }
}

struct SlowSchemaAuthority {
    execute_batch_checks: AtomicUsize,
}

impl MigrationSqlWriteAuthority for SlowSchemaAuthority {
    fn verify(&self, intent: MigrationSqlWriteIntent) -> Result<(), MigrationSqlError> {
        if intent == MigrationSqlWriteIntent::ExecuteBatch
            && self.execute_batch_checks.fetch_add(1, Ordering::AcqRel) < 3
        {
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    }
}

struct RevokeDuringSchemaStep {
    execute_batch_checks: AtomicUsize,
}

impl MigrationSqlWriteAuthority for RevokeDuringSchemaStep {
    fn verify(&self, intent: MigrationSqlWriteIntent) -> Result<(), MigrationSqlError> {
        if intent == MigrationSqlWriteIntent::ExecuteBatch
            && self.execute_batch_checks.fetch_add(1, Ordering::AcqRel) >= 1
        {
            return Err(MigrationSqlError::AuthorityDenied(
                "revoked during schema step".to_owned(),
            ));
        }
        Ok(())
    }
}

struct NoWrites;

impl StorageOperationExecutor for NoWrites {
    fn execute(
        &mut self,
        _savepoint: &Savepoint<'_>,
        _payload: &RepositoryWritePayloadV1,
    ) -> rusqlite::Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
struct NoReads;

impl ReaderQueryExecutor for NoReads {
    fn execute_read(
        &mut self,
        _snapshot: &rusqlite::Transaction<'_>,
        _request: &RuntimeReadRequestV1,
    ) -> Result<RuntimeReadOutcomeV1, tracedecay_store::StorageRuntimeErrorV1> {
        unreachable!("migration SQL queries bypass the closed product read executor")
    }
}

struct Fixture {
    _directory: TempDir,
    writer: PersistentWriter,
    readers: ReaderPool<NoReads>,
}

fn binding() -> StoreRuntimeBindingV1 {
    serde_json::from_value(serde_json::json!({
        "shard_id": {
            "brain_id": "brain.migration-sql",
            "profile_id": "profile.migration-sql",
            "scope": { "kind": "project", "project_id": "project.migration-sql" }
        },
        "incarnation": 3,
        "authority_epoch": 11
    }))
    .unwrap()
}

fn locator(binding: &StoreRuntimeBindingV1, byte: char) -> VerifiedStoreLocatorV1 {
    VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        StoreIncarnationV1::new(3).unwrap(),
        LocatorDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap(),
    )
}

fn fixture(writer_digest: char, reader_digest: char) -> Fixture {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("migration.sqlite3");
    rusqlite::Connection::open(&path).unwrap();
    let path = path.canonicalize().unwrap();
    let binding = binding();
    let writer = PersistentWriter::start(
        ExistingWriterLocator::new(
            binding.clone(),
            locator(&binding, writer_digest),
            path.clone(),
        )
        .unwrap(),
        AdmissionConfigV1::default(),
        NoWrites,
    )
    .unwrap();
    let readers = ReaderPool::start(
        ExistingReaderLocator::new(binding.clone(), locator(&binding, reader_digest), path)
            .unwrap(),
        AdmissionConfigV1::default().readers,
        NoReads,
    )
    .unwrap();
    Fixture {
        _directory: directory,
        writer,
        readers,
    }
}

fn statement(sql: &str, params: Vec<MigrationSqlValue>) -> MigrationSqlStatement {
    MigrationSqlStatement::new(sql.to_owned(), params).unwrap()
}

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
fn schema_migration_transaction_requires_attached_write_authority() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

    let error = match channel.begin_schema_migration_immediate() {
        Ok(_) => panic!("schema migration must require attached authority"),
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
fn schema_migration_renews_its_lease_after_successful_bounded_steps() {
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
    let transaction = channel.begin_schema_migration_immediate().unwrap();
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
    let transaction = channel.begin_schema_migration_immediate().unwrap();
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
    let transaction = channel.begin_schema_migration_immediate().unwrap();

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

#[test]
fn execute_batch_execute_and_query_use_owned_dtos() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
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
                MigrationSqlValue::Integer(7),
                MigrationSqlValue::Real(2.5),
                MigrationSqlValue::Text("owned".to_owned()),
                MigrationSqlValue::Blob(vec![1, 2, 3]),
                MigrationSqlValue::Null,
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
        vec![MigrationSqlRow {
            values: vec![
                MigrationSqlValue::Integer(7),
                MigrationSqlValue::Real(2.5),
                MigrationSqlValue::Text("owned".to_owned()),
                MigrationSqlValue::Blob(vec![1, 2, 3]),
                MigrationSqlValue::Null,
            ],
        }]
    );
}

#[test]
fn statement_admission_limits_accept_boundaries_and_reject_oversize() {
    assert!(MigrationSqlStatement::new("x".repeat(MAX_SQL_BYTES), vec![]).is_ok());
    assert!(matches!(
        MigrationSqlStatement::new("x".repeat(MAX_SQL_BYTES + 1), vec![]),
        Err(MigrationSqlError::RequestLimitExceeded)
    ));
    assert!(
        MigrationSqlStatement::new(
            "SELECT 1".to_owned(),
            vec![MigrationSqlValue::Null; MAX_SQL_PARAMETERS],
        )
        .is_ok()
    );
    assert!(matches!(
        MigrationSqlStatement::new(
            "SELECT 1".to_owned(),
            vec![MigrationSqlValue::Null; MAX_SQL_PARAMETERS + 1],
        ),
        Err(MigrationSqlError::RequestLimitExceeded)
    ));
}

#[test]
fn batch_admission_rejects_oversize_before_enqueue() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

    let error = channel
        .execute_batch("x".repeat(MAX_SQL_BYTES + 1))
        .unwrap_err();

    assert!(matches!(error, MigrationSqlError::RequestLimitExceeded));
}

#[test]
fn validate_checks_syntax_and_schema_on_the_writer_actor() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

    let missing = channel
        .validate(statement("SELECT value FROM missing_table", vec![]))
        .unwrap_err();
    let syntax = channel
        .validate(statement("SELECT FROM", vec![]))
        .unwrap_err();

    assert!(matches!(missing, MigrationSqlError::Sqlite { .. }));
    assert!(matches!(syntax, MigrationSqlError::Sqlite { .. }));
}

#[test]
fn batch_reports_last_insert_rowid() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
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
    let channel_a = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    let channel_b = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
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
            vec![MigrationSqlValue::Text("a".to_owned())],
        ))
        .unwrap();
    let b = channel_b
        .execute(statement(
            "INSERT INTO rowids(value) VALUES (?)",
            vec![MigrationSqlValue::Text("b".to_owned())],
        ))
        .unwrap();
    assert_eq!(a.last_insert_rowid, 1);
    assert_eq!(b.last_insert_rowid, 2);

    let update = channel_a
        .execute(statement(
            "UPDATE rowids SET value = ? WHERE id = 1",
            vec![MigrationSqlValue::Text("updated".to_owned())],
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
            vec![MigrationSqlValue::Text("b".to_owned())],
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
                MigrationSqlValue::Integer(41),
                MigrationSqlValue::Text("explicit".to_owned()),
            ],
        ))
        .unwrap();
    assert_eq!(explicit.last_insert_rowid, 41);
    assert_eq!(channel_a.last_insert_rowid(), 41);
}

#[test]
fn partial_batch_error_still_publishes_applied_insert_rowid() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
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

    assert!(matches!(error, MigrationSqlError::Sqlite { .. }));
    assert_eq!(channel.last_insert_rowid(), 1);

    let transaction = channel.begin_immediate().unwrap();
    let error = transaction
        .execute_batch(
            "INSERT INTO partial_rowid(value) VALUES ('pinned');
                 INSERT INTO missing_table(value) VALUES ('fail');"
                .to_owned(),
        )
        .unwrap_err();
    assert!(matches!(error, MigrationSqlError::Sqlite { .. }));
    assert_eq!(channel.last_insert_rowid(), 2);
    transaction.rollback().unwrap();
    assert_eq!(channel.last_insert_rowid(), 2);
}

#[test]
fn transaction_insert_returning_publishes_rowid() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
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

    assert_eq!(rows.rows[0].values, vec![MigrationSqlValue::Integer(1)]);
    assert_eq!(channel.last_insert_rowid(), 1);
    transaction.rollback().unwrap();
}

#[test]
fn deferred_transaction_is_available_for_default_sqlite_semantics() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch("CREATE TABLE deferred (value INTEGER NOT NULL)".to_owned())
        .unwrap();
    let transaction = channel.begin_deferred().unwrap();
    transaction
        .execute(statement(
            "INSERT INTO deferred VALUES (?)",
            vec![MigrationSqlValue::Integer(1)],
        ))
        .unwrap();

    transaction.commit().unwrap();

    let rows = channel
        .query(
            statement("SELECT count(*) FROM deferred", vec![]),
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(rows.rows[0].values, vec![MigrationSqlValue::Integer(1)]);
}

#[test]
fn immediate_transaction_commit_reports_only_after_commit() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch("CREATE TABLE committed (value INTEGER NOT NULL)".to_owned())
        .unwrap();
    let transaction = channel.begin_immediate().unwrap();
    transaction
        .execute(statement(
            "INSERT INTO committed VALUES (?)",
            vec![MigrationSqlValue::Integer(41)],
        ))
        .unwrap();
    let inside = transaction
        .query(statement("SELECT value FROM committed", vec![]))
        .unwrap();

    assert_eq!(inside.rows[0].values, vec![MigrationSqlValue::Integer(41)]);
    let receipt = transaction.commit().unwrap();
    assert_eq!(receipt.changed_rows, 1);
    let committed = channel
        .query(
            statement("SELECT value FROM committed", vec![]),
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(
        committed.rows[0].values,
        vec![MigrationSqlValue::Integer(41)]
    );
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
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    let attachment =
        || MigrationSqlAttachment::new(source_path.to_string_lossy(), "source_input").unwrap();

    let transaction = channel.begin_immediate().unwrap();
    transaction.attach_database(attachment()).unwrap();
    let rows = transaction
        .query(statement(
            "SELECT value FROM source_input.source_rows",
            vec![],
        ))
        .unwrap();
    assert_eq!(rows.rows[0].values, vec![MigrationSqlValue::Integer(7)]);
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
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

    channel
        .execute(statement(
            "ATTACH DATABASE ?1 AS caller_input",
            vec![MigrationSqlValue::Text(":memory:".to_owned())],
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
            Some(MigrationSqlValue::Text(name)) if name == "caller_input"
        )
    }));
}

#[test]
fn immediate_transaction_rollback_reports_after_rollback_and_discards_rows() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch("CREATE TABLE rolled_back (value INTEGER NOT NULL)".to_owned())
        .unwrap();
    let transaction = channel.begin_immediate().unwrap();
    transaction
        .execute(statement(
            "INSERT INTO rolled_back VALUES (?)",
            vec![MigrationSqlValue::Integer(99)],
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
    assert_eq!(rows.rows[0].values, vec![MigrationSqlValue::Integer(0)]);
}

#[test]
fn pinned_batch_rejects_transaction_control() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    let transaction = channel.begin_immediate().unwrap();

    let error = transaction
        .execute_batch("COMMIT; BEGIN IMMEDIATE".to_owned())
        .unwrap_err();

    assert!(matches!(error, MigrationSqlError::TransactionControlDenied));
    transaction.rollback().unwrap();
}

#[test]
fn pinned_execute_rejects_transaction_control_before_commit_receipt() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    let transaction = channel.begin_immediate().unwrap();

    let error = transaction
        .execute(statement("COMMIT", vec![]))
        .unwrap_err();

    assert!(matches!(error, MigrationSqlError::TransactionControlDenied));
    transaction.rollback().unwrap();
}

#[test]
fn unpinned_batch_rejects_transaction_control_and_releases_writer() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

    let error = channel
        .execute_batch("BEGIN IMMEDIATE".to_owned())
        .unwrap_err();

    assert!(matches!(error, MigrationSqlError::TransactionControlDenied));
    channel
        .execute_batch("CREATE TABLE after_denied_begin (value INTEGER)".to_owned())
        .unwrap();
}

#[test]
fn mutating_no_argument_pragmas_are_denied() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

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
                MigrationSqlError::Sqlite {
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
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

    channel
        .execute_batch("PRAGMA shrink_memory".to_owned())
        .expect("connection-local cache release must be authorized");
}

#[test]
fn migration_read_policy_allows_integrity_diagnostic_arguments() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
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
            vec![MigrationSqlValue::Text("ok".to_owned())],
            "{pragma} must remain classified as a read-only diagnostic"
        );
    }
    let table_info = transaction
        .query(statement("PRAGMA table_info(pragma_probe)", vec![]))
        .unwrap();
    assert_eq!(
        table_info.rows[0].values[1],
        MigrationSqlValue::Text("value".to_owned())
    );
    transaction.rollback().unwrap();
}

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
fn pinned_batch_allows_schema_migration_ddl() {
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
fn unpinned_batch_allows_schema_migration_ddl() {
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

#[test]
fn dropping_pinned_transaction_rolls_back() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch("CREATE TABLE dropped (value INTEGER NOT NULL)".to_owned())
        .unwrap();
    {
        let transaction = channel.begin_immediate().unwrap();
        transaction
            .execute(statement(
                "INSERT INTO dropped VALUES (?)",
                vec![MigrationSqlValue::Integer(8)],
            ))
            .unwrap();
    }

    let rows = channel
        .query(
            statement("SELECT count(*) FROM dropped", vec![]),
            Duration::from_secs(1),
        )
        .unwrap();

    assert_eq!(rows.rows[0].values, vec![MigrationSqlValue::Integer(0)]);
}

#[test]
fn writer_shutdown_rolls_back_and_closes_a_leaked_transaction() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    let transaction = channel.begin_immediate().unwrap();
    let Fixture {
        _directory,
        writer,
        readers,
    } = fixture;
    let (finished, receive) = std::sync::mpsc::sync_channel(1);

    std::thread::spawn(move || {
        drop(writer);
        let _ = finished.send(());
    });

    receive
        .recv_timeout(Duration::from_secs(1))
        .expect("writer shutdown must not wait forever on leaked migration transaction");
    assert!(matches!(
        transaction.commit(),
        Err(MigrationSqlError::TransactionClosed)
    ));
    drop(readers);
    drop(_directory);
}

#[test]
fn idle_transaction_expires_and_releases_writer() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    let transaction = channel.begin_immediate().unwrap();

    std::thread::sleep(MIGRATION_SQL_TRANSACTION_IDLE_LIMIT + Duration::from_millis(100));

    assert!(matches!(
        transaction.commit(),
        Err(MigrationSqlError::TransactionExpired)
    ));
    channel
        .execute_batch("CREATE TABLE after_idle_expiry (value INTEGER)".to_owned())
        .unwrap();
}

#[test]
fn active_transaction_hits_absolute_lease_and_releases_writer() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    let transaction = channel.begin_immediate().unwrap();
    let started = Instant::now();

    let error = loop {
        match transaction.query(statement("SELECT 1", vec![])) {
            Ok(_) => std::thread::sleep(Duration::from_millis(20)),
            Err(error) => break error,
        }
    };

    assert!(matches!(error, MigrationSqlError::TransactionExpired));
    assert!(started.elapsed() < Duration::from_secs(2));
    channel
        .execute_batch("CREATE TABLE after_absolute_expiry (value INTEGER)".to_owned())
        .unwrap();
}

#[test]
fn query_materialization_is_bounded() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

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

    assert!(matches!(error, MigrationSqlError::QueryLimitExceeded));
}

#[test]
fn query_execution_time_is_bounded() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
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

    assert!(matches!(
        error,
        MigrationSqlError::Sqlite { code: Some(9), .. }
    ));
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn invalid_sqlite_text_is_rejected_without_lossy_conversion() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
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
        MigrationSqlError::Sqlite {
            operation: "decode query text",
            ..
        }
    ));
}

#[test]
fn sqlite_errors_preserve_primary_and_extended_codes() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch("CREATE TABLE unique_value (value INTEGER UNIQUE)".to_owned())
        .unwrap();
    channel
        .execute(statement(
            "INSERT INTO unique_value VALUES (?)",
            vec![MigrationSqlValue::Integer(1)],
        ))
        .unwrap();

    let error = channel
        .execute(statement(
            "INSERT INTO unique_value VALUES (?)",
            vec![MigrationSqlValue::Integer(1)],
        ))
        .unwrap_err();

    assert!(matches!(
        error,
        MigrationSqlError::Sqlite {
            code: Some(19),
            extended_code: Some(2067),
            ..
        }
    ));
}

#[test]
fn read_snapshot_stays_frozen_across_queries() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch("CREATE TABLE frozen (value INTEGER NOT NULL)".to_owned())
        .unwrap();
    channel
        .execute(statement(
            "INSERT INTO frozen VALUES (?)",
            vec![MigrationSqlValue::Integer(1)],
        ))
        .unwrap();
    let snapshot = channel.begin_read_snapshot(Duration::from_secs(1)).unwrap();
    let first = snapshot
        .query(statement("SELECT count(*) FROM frozen", vec![]))
        .unwrap();
    channel
        .execute(statement(
            "INSERT INTO frozen VALUES (?)",
            vec![MigrationSqlValue::Integer(2)],
        ))
        .unwrap();

    let frozen = snapshot
        .query(statement("SELECT count(*) FROM frozen", vec![]))
        .unwrap();

    assert_eq!(first.rows[0].values, vec![MigrationSqlValue::Integer(1)]);
    assert_eq!(frozen.rows[0].values, vec![MigrationSqlValue::Integer(1)]);
    drop(snapshot);
    let current = channel
        .query(
            statement("SELECT count(*) FROM frozen", vec![]),
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(current.rows[0].values, vec![MigrationSqlValue::Integer(2)]);
}

#[test]
fn health_snapshot_retires_the_reserved_reader() {
    let fixture = fixture('a', 'a');
    let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

    let snapshot = channel
        .begin_health_read_snapshot(Duration::from_secs(1))
        .unwrap();
    assert_eq!(fixture.readers.snapshot().leased_health, 1);
    drop(snapshot);

    let pool = fixture.readers.snapshot();
    assert_eq!(pool.leased_health, 0);
    assert_eq!(pool.health_workers, 0);
}
