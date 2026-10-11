//! One behavior contract for both engine arms: every case runs, with the same
//! assertions, under the default build and under `--config=native`.

use std::time::Duration;

use tracedecay_sql::{
    Capability, CheckpointMode, Connection, ConnectionControl, EnginePolicy, Error, ErrorExt,
    ErrorKind, OptionalExtension, Result, StatementControl, StatementCounters, TransactionBehavior,
    TransactionControl, params, params_from_iter,
    types::{FromSql, Value, ValueRef},
};

macro_rules! conformance {
    ($($name:ident),+ $(,)?) => {$(
        #[test]
        fn $name() -> Result<()> {
            cases::$name()
        }
    )+};
}

conformance!(
    open_execute_and_query_row,
    reopen_keeps_only_committed_rows,
    optional_maps_missing_row_to_none,
    prepare_and_prepare_cached_rebind,
    params_bind_every_value_type,
    transaction_commit_and_rollback_on_drop,
    savepoints_nest_and_roll_back_on_drop,
    constraint_is_classified,
    busy_is_classified,
    capability_scope_denies_and_restores,
    guard_scope_interrupts_running_statement,
    page_stats_and_checkpoint_report_the_file,
    exact_sql_cannot_end_or_open_the_runtime_transaction,
    exact_sql_scopes_deny_effects_outside_their_capability,
    checkpoints_fold_wal_frames_into_the_main_file,
    statement_work_grows_with_its_target_not_unrelated_rows,
    transaction_guard_scope_interrupts_and_is_removed,
    engine_policy_never_waits_on_a_busy_store,
    damaged_pages_are_classified_corrupt,
    statements_report_columns_and_conversions_carry_into_errors,
);

mod cases {
    use super::*;

    fn file_database() -> (tempfile::TempDir, std::path::PathBuf, Connection) {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("conformance.db");
        let connection = Connection::open(&path).expect("open file database");
        (directory, path, connection)
    }

    fn items(connection: &Connection) -> Result<()> {
        connection.execute_batch(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, score REAL)",
        )
    }

    fn names(connection: &Connection) -> Result<Vec<String>> {
        let mut statement = connection.prepare("SELECT name FROM items ORDER BY id")?;
        statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<String>>>()
    }

    pub(super) fn open_execute_and_query_row() -> Result<()> {
        let (_directory, _path, connection) = file_database();
        items(&connection)?;
        let changed = connection.execute(
            "INSERT INTO items (name, score) VALUES (?1, ?2)",
            params!["alpha", 1.5],
        )?;
        assert_eq!(changed, 1, "execute reports the inserted row");
        let (name, score): (String, f64) =
            connection.query_row("SELECT name, score FROM items WHERE id = ?1", [1], |row| {
                Ok((row.get("name")?, row.get(1)?))
            })?;
        assert_eq!((name.as_str(), score), ("alpha", 1.5));
        assert!(connection.is_autocommit(), "no transaction is left open");
        Ok(())
    }

    pub(super) fn reopen_keeps_only_committed_rows() -> Result<()> {
        let (_directory, path, mut connection) = file_database();
        items(&connection)?;
        connection.execute("INSERT INTO items (name) VALUES ('kept')", [])?;
        let transaction = connection.transaction()?;
        transaction.execute("INSERT INTO items (name) VALUES ('dropped')", [])?;
        drop(transaction);
        drop(connection);
        let reopened = Connection::open(&path)?;
        assert_eq!(names(&reopened)?, ["kept"]);
        Ok(())
    }

    pub(super) fn optional_maps_missing_row_to_none() -> Result<()> {
        let connection = Connection::open_in_memory()?;
        items(&connection)?;
        let missing =
            connection.query_row("SELECT name FROM items", [], |row| row.get::<_, String>(0));
        let error = missing.expect_err("an empty table has no first row");
        assert_eq!(error.kind(), ErrorKind::NoRows);
        let optional = connection
            .query_row("SELECT name FROM items", [], |row| row.get::<_, String>(0))
            .optional()?;
        assert_eq!(optional, None);
        Ok(())
    }

    pub(super) fn prepare_and_prepare_cached_rebind() -> Result<()> {
        let connection = Connection::open_in_memory()?;
        items(&connection)?;
        let mut insert = connection.prepare("INSERT INTO items (name) VALUES (?1)")?;
        for name in ["a", "b", "c"] {
            assert_eq!(insert.execute([name])?, 1);
        }
        drop(insert);
        for expected in ["a", "c"] {
            let mut select = connection.prepare_cached("SELECT name FROM items WHERE name = ?1")?;
            let found: String = select.query_row([expected], |row| row.get(0))?;
            assert_eq!(found, expected, "a cached statement rebinds its parameters");
            assert!(select.exists([expected])?);
            assert!(!select.exists(["missing"])?);
        }
        let count = connection
            .prepare("SELECT count(*) FROM items")?
            .query_row([], |row| row.get::<_, i64>(0))?;
        assert_eq!(count, 3);
        assert_eq!(
            connection.total_changes(),
            3,
            "three inserts changed three rows"
        );
        Ok(())
    }

    pub(super) fn params_bind_every_value_type() -> Result<()> {
        let connection = Connection::open_in_memory()?;
        connection.execute_batch("CREATE TABLE value_types (v)")?;
        let blob = vec![0_u8, 1, 255];
        connection.execute("INSERT INTO value_types VALUES (?1)", [None::<i64>])?;
        connection.execute("INSERT INTO value_types VALUES (?1)", [i64::MIN])?;
        connection.execute("INSERT INTO value_types VALUES (?1)", [u32::MAX])?;
        connection.execute("INSERT INTO value_types VALUES (?1)", [true])?;
        connection.execute("INSERT INTO value_types VALUES (?1)", [-0.25_f64])?;
        connection.execute("INSERT INTO value_types VALUES (?1)", ["text"])?;
        connection.execute(
            "INSERT INTO value_types VALUES (?1)",
            [String::from("owned")],
        )?;
        connection.execute("INSERT INTO value_types VALUES (?1)", [&blob])?;
        connection.execute("INSERT INTO value_types VALUES (?1)", [&blob[..1]])?;
        connection.execute(
            "INSERT INTO value_types VALUES (?1)",
            [Value::Text("value".to_owned())],
        )?;
        connection.execute("INSERT INTO value_types VALUES (?1)", (Some(7_i32),))?;
        connection.execute(
            "INSERT INTO value_types SELECT ?1 + ?2",
            params_from_iter([20_i64, 22]),
        )?;
        let mut statement = connection.prepare("SELECT v FROM value_types ORDER BY rowid")?;
        let values = statement
            .query_map([], |row| row.get::<_, Value>(0))?
            .collect::<Result<Vec<_>>>()?;
        assert_eq!(
            values,
            [
                Value::Null,
                Value::Integer(i64::MIN),
                Value::Integer(i64::from(u32::MAX)),
                Value::Integer(1),
                Value::Real(-0.25),
                Value::Text("text".to_owned()),
                Value::Text("owned".to_owned()),
                Value::Blob(blob.clone()),
                Value::Blob(vec![0]),
                Value::Text("value".to_owned()),
                Value::Integer(7),
                Value::Integer(42),
            ]
        );
        let typed: (Option<i64>, bool, Vec<u8>) =
            connection.query_row("SELECT NULL, 1, x'0001ff'", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;
        assert_eq!(typed, (None, true, blob));
        let overflow = connection.query_row("SELECT 300", [], |row| row.get::<_, u8>(0));
        assert!(overflow.is_err(), "a value outside u8 does not truncate");
        Ok(())
    }

    pub(super) fn transaction_commit_and_rollback_on_drop() -> Result<()> {
        let mut connection = Connection::open_in_memory()?;
        items(&connection)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute("INSERT INTO items (name) VALUES ('committed')", [])?;
        assert!(!transaction.is_autocommit());
        transaction.commit()?;
        {
            let transaction = connection.transaction()?;
            transaction.execute("INSERT INTO items (name) VALUES ('dropped')", [])?;
        }
        let transaction = connection.transaction()?;
        transaction.execute("INSERT INTO items (name) VALUES ('rolled back')", [])?;
        transaction.rollback()?;
        assert!(connection.is_autocommit());
        assert_eq!(names(&connection)?, ["committed"]);
        Ok(())
    }

    pub(super) fn savepoints_nest_and_roll_back_on_drop() -> Result<()> {
        let mut connection = Connection::open_in_memory()?;
        items(&connection)?;
        {
            let outer = connection.savepoint()?;
            outer.execute("INSERT INTO items (name) VALUES ('outer')", [])?;
            outer.commit()?;
        }
        let mut transaction = connection.transaction()?;
        {
            let mut first = transaction.savepoint()?;
            first.execute("INSERT INTO items (name) VALUES ('first')", [])?;
            {
                let second = first.savepoint()?;
                second.execute("INSERT INTO items (name) VALUES ('second')", [])?;
            }
            {
                let mut third = first.savepoint()?;
                third.execute("INSERT INTO items (name) VALUES ('third')", [])?;
                third.rollback()?;
                third.execute("INSERT INTO items (name) VALUES ('after')", [])?;
                third.commit()?;
            }
            first.commit()?;
        }
        transaction.commit()?;
        assert_eq!(names(&connection)?, ["outer", "first", "after"]);
        Ok(())
    }

    pub(super) fn constraint_is_classified() -> Result<()> {
        let connection = Connection::open_in_memory()?;
        items(&connection)?;
        connection.execute("INSERT INTO items (name) VALUES ('same')", [])?;
        let unique = connection
            .execute("INSERT INTO items (name) VALUES ('same')", [])
            .expect_err("duplicate name violates UNIQUE");
        assert_eq!(unique.kind(), ErrorKind::Constraint, "{unique}");
        let not_null = connection
            .execute("INSERT INTO items (name) VALUES (NULL)", [])
            .expect_err("NULL name violates NOT NULL");
        assert_eq!(not_null.kind(), ErrorKind::Constraint, "{not_null}");
        Ok(())
    }

    pub(super) fn busy_is_classified() -> Result<()> {
        let (_directory, path, mut holder) = file_database();
        items(&holder)?;
        let contender = Connection::open(&path)?;
        contender.busy_timeout(Duration::ZERO)?;
        let transaction = holder.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute("INSERT INTO items (name) VALUES ('held')", [])?;
        let busy = contender
            .execute("INSERT INTO items (name) VALUES ('blocked')", [])
            .expect_err("a second writer waits on the held write lock");
        assert_eq!(busy.kind(), ErrorKind::Busy, "{busy}");
        transaction.commit()?;
        Ok(())
    }

    pub(super) fn capability_scope_denies_and_restores() -> Result<()> {
        let connection = Connection::open_in_memory()?;
        items(&connection)?;
        connection.set_capability(Capability::Writer)?;
        let denied = connection
            .with_capability(Capability::Reader, Capability::Writer, |reader| {
                reader.execute("INSERT INTO items (name) VALUES ('denied')", [])
            })
            .expect_err("a reader scope denies writes");
        assert_eq!(denied.kind(), ErrorKind::Denied, "{denied}");
        let trigger = connection
            .execute_batch("CREATE TRIGGER t AFTER INSERT ON items BEGIN SELECT 1; END")
            .expect_err("the writer capability denies triggers");
        assert_eq!(trigger.kind(), ErrorKind::Denied, "{trigger}");
        connection.execute("INSERT INTO items (name) VALUES ('restored')", [])?;
        assert_eq!(names(&connection)?, ["restored"]);
        Ok(())
    }

    pub(super) fn guard_scope_interrupts_running_statement() -> Result<()> {
        let connection = Connection::open_in_memory()?;
        let interrupted = connection
            .guard_scope(
                || true,
                |connection| {
                    connection.query_row(
                        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n \
                         WHERE i < 10000000) SELECT count(*) FROM n",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                },
            )?
            .expect_err("an always-true guard interrupts the statement");
        assert_eq!(interrupted.kind(), ErrorKind::Interrupted, "{interrupted}");
        let after: i64 = connection.query_row("SELECT 1", [], |row| row.get(0))?;
        assert_eq!(after, 1, "the guard is removed when the scope ends");
        Ok(())
    }

    pub(super) fn page_stats_and_checkpoint_report_the_file() -> Result<()> {
        let (_directory, _path, connection) = file_database();
        items(&connection)?;
        connection.execute("INSERT INTO items (name) VALUES ('page')", [])?;
        let stats = connection.page_stats()?;
        assert!(stats.page_size >= 512, "{stats:?}");
        assert!(
            stats.page_count >= 2,
            "schema and table pages exist: {stats:?}"
        );
        assert!(stats.freelist_count < stats.page_count, "{stats:?}");
        let checkpoint = connection.checkpoint(CheckpointMode::Truncate)?;
        assert!(!checkpoint.busy, "no reader blocks the checkpoint");
        assert!(checkpoint.checkpointed_frames <= checkpoint.log_frames);
        Ok(())
    }

    const LEASED: Capability = Capability::ExactSql { savepoints: true };
    const UNLEASED: Capability = Capability::ExactSql { savepoints: false };

    /// Caller SQL in an exact SQL scope that tries to end or open the
    /// runtime's transaction; it swallows its own error, so only the scope's
    /// report and the store's state show the denial.
    fn swallowing(sql: &'static str) -> impl FnOnce(&Connection) -> Result<()> {
        move |caller| {
            let _ = caller.execute_batch(sql);
            Ok(())
        }
    }

    pub(super) fn exact_sql_cannot_end_or_open_the_runtime_transaction() -> Result<()> {
        let (_directory, _path, mut connection) = file_database();
        items(&connection)?;
        connection.set_capability(Capability::Writer)?;
        {
            let transaction = connection.transaction()?;
            transaction.execute("INSERT INTO items (name) VALUES ('pending')", [])?;
            let denied = transaction
                .with_capability(LEASED, Capability::Writer, swallowing("COMMIT"))
                .expect_err("caller COMMIT is denied");
            assert_eq!(
                denied.kind(),
                ErrorKind::TransactionControlDenied,
                "{denied}"
            );
            transaction.rollback()?;
        }
        assert!(
            names(&connection)?.is_empty(),
            "caller COMMIT committed the runtime's transaction"
        );
        for (capability, sql) in [
            (UNLEASED, "BEGIN IMMEDIATE"),
            (UNLEASED, "SAVEPOINT nested"),
            (Capability::ExactSqlRead, "SAVEPOINT nested"),
        ] {
            let denied = connection
                .with_capability(capability, Capability::Writer, swallowing(sql))
                .expect_err("caller transaction control is denied");
            assert_eq!(
                denied.kind(),
                ErrorKind::TransactionControlDenied,
                "{capability:?} `{sql}`: {denied}"
            );
            assert!(
                connection.is_autocommit(),
                "`{sql}` under {capability:?} opened a transaction"
            );
        }
        {
            let transaction = connection.transaction()?;
            transaction.with_capability(LEASED, Capability::Writer, |caller| {
                caller.execute_batch(
                    "SAVEPOINT nested; INSERT INTO items (name) VALUES ('undone'); \
                     ROLLBACK TO nested; RELEASE nested; \
                     INSERT INTO items (name) VALUES ('kept');",
                )
            })?;
            transaction.commit()?;
        }
        assert_eq!(names(&connection)?, ["kept"], "a leased savepoint works");
        Ok(())
    }

    pub(super) fn exact_sql_scopes_deny_effects_outside_their_capability() -> Result<()> {
        let (_directory, _path, connection) = file_database();
        items(&connection)?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON; \
             CREATE TABLE parents (id INTEGER PRIMARY KEY); \
             CREATE TABLE children (parent INTEGER NOT NULL REFERENCES parents (id));",
        )?;
        connection.set_capability(Capability::Writer)?;

        let read =
            connection.with_capability(Capability::ExactSqlRead, Capability::Writer, |caller| {
                caller.execute("INSERT INTO items (name) VALUES ('read')", [])
            });
        assert!(read.is_err(), "a read scope accepted a write");
        assert!(names(&connection)?.is_empty(), "a read scope wrote a row");

        let _ = connection.with_capability(UNLEASED, Capability::Writer, |caller| {
            caller.execute_batch(
                "CREATE TEMP TRIGGER fire AFTER INSERT ON items WHEN NEW.name = 'base' \
                 BEGIN INSERT INTO items (name) VALUES ('fired'); END",
            )
        });
        connection.execute("INSERT INTO items (name) VALUES ('base')", [])?;
        assert_eq!(names(&connection)?, ["base"], "a denied temp trigger fired");

        let orphan = connection.with_capability(UNLEASED, Capability::Writer, |caller| {
            let _ = caller.execute_batch("PRAGMA foreign_keys = OFF");
            caller.execute("INSERT INTO children (parent) VALUES (9)", [])
        });
        assert!(orphan.is_err(), "foreign keys were switched off");
        let orphans: i64 =
            connection.query_row("SELECT count(*) FROM children", [], |row| row.get(0))?;
        assert_eq!(orphans, 0, "an orphan row was written");

        connection.with_capability(UNLEASED, Capability::Writer, |caller| {
            caller.execute_batch("PRAGMA user_version = 7")
        })?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        assert_eq!(version, 7, "an allowlisted pragma took no effect");
        Ok(())
    }

    pub(super) fn checkpoints_fold_wal_frames_into_the_main_file() -> Result<()> {
        let (_directory, path, connection) = file_database();
        connection.execute_batch("PRAGMA journal_mode = WAL")?;
        items(&connection)?;
        connection.checkpoint(CheckpointMode::Truncate)?;
        let main_bytes = || std::fs::metadata(&path).expect("main file").len();
        let mut wal = path.clone().into_os_string();
        wal.push("-wal");
        let insert = |from: usize| -> Result<()> {
            let mut statement = connection.prepare("INSERT INTO items (name) VALUES (?1)")?;
            for index in from..from + 25 {
                statement.execute([format!("{index}-{}", "x".repeat(4_000))])?;
            }
            Ok(())
        };
        let before = main_bytes();

        insert(0)?;
        let first = connection.checkpoint(CheckpointMode::Noop)?;
        insert(25)?;
        let second = connection.checkpoint(CheckpointMode::Noop)?;
        assert_eq!(
            main_bytes(),
            before,
            "NOOP folded frames into the main file"
        );
        assert!(first.log_frames > 0, "{first:?}");
        assert!(
            second.log_frames > first.log_frames,
            "NOOP missed WAL growth: {first:?} then {second:?}"
        );

        let passive = connection.checkpoint(CheckpointMode::Passive)?;
        assert!(!passive.busy, "{passive:?}");
        assert!(
            main_bytes() >= before + 50 * 4_000,
            "PASSIVE left the rows in the WAL: main file {} bytes, was {before}",
            main_bytes()
        );
        let after = connection.checkpoint(CheckpointMode::Noop)?;
        assert_eq!(
            after.checkpointed_frames, after.log_frames,
            "NOOP disagrees with the PASSIVE that folded every frame"
        );

        connection.checkpoint(CheckpointMode::Truncate)?;
        assert_eq!(std::fs::metadata(&wal).expect("WAL").len(), 0);
        assert_eq!(connection.checkpoint(CheckpointMode::Noop)?.log_frames, 0);
        drop(connection);
        let reopened = Connection::open(&path)?;
        let rows: i64 = reopened.query_row("SELECT count(*) FROM items", [], |row| row.get(0))?;
        assert_eq!(rows, 50);
        Ok(())
    }

    pub(super) fn statement_work_grows_with_its_target_not_unrelated_rows() -> Result<()> {
        let connection = Connection::open_in_memory()?;
        connection
            .execute_batch("CREATE TABLE target (x INTEGER); CREATE TABLE other (x INTEGER)")?;
        let insert = |table: &str, rows: i64| -> Result<()> {
            let mut statement =
                connection.prepare(&format!("INSERT INTO {table} (x) VALUES (?1)"))?;
            for value in 0..rows {
                statement.execute([value])?;
            }
            Ok(())
        };
        let work = || -> Result<StatementCounters> {
            let mut statement =
                connection.prepare_cached("SELECT x FROM target ORDER BY x DESC")?;
            statement.query_map([], |row| row.get::<_, i64>(0))?.count();
            Ok(statement.take_counters())
        };
        insert("target", 10)?;
        let small = work()?;
        assert!(small.vm_steps > 0, "{small:?}");
        assert!(small.fullscan_steps > 0, "{small:?}");
        assert!(small.sort_steps > 0, "{small:?}");
        assert_eq!(
            work()?,
            small,
            "a cached program's earlier use was counted again"
        );
        insert("other", 1_000)?;
        assert_eq!(
            work()?,
            small,
            "work grew with rows the statement never reads"
        );
        insert("target", 1_000)?;
        let large = work()?;
        assert!(
            large.vm_steps >= small.vm_steps + 1_000,
            "{small:?} then {large:?}"
        );
        assert!(
            large.fullscan_steps >= small.fullscan_steps + 1_000,
            "{small:?} then {large:?}"
        );
        Ok(())
    }

    pub(super) fn transaction_guard_scope_interrupts_and_is_removed() -> Result<()> {
        let (_directory, _path, mut connection) = file_database();
        items(&connection)?;
        let count = |connection: &Connection, bound: i64| {
            connection.query_row(
                &format!(
                    "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n \
                     WHERE i < {bound}) SELECT count(*) FROM n"
                ),
                [],
                |row| row.get::<_, i64>(0),
            )
        };
        let mut transaction = connection.transaction()?;
        transaction.execute("INSERT INTO items (name) VALUES ('kept')", [])?;
        let interrupted = transaction
            .guard_scope_mut(|| true, |transaction| count(transaction, 10_000_000))?
            .expect_err("an always-true guard interrupts the statement");
        assert_eq!(interrupted.kind(), ErrorKind::Interrupted, "{interrupted}");
        assert_eq!(
            count(&transaction, 100_000)?,
            100_000,
            "the guard is removed when the scope ends"
        );
        transaction.commit()?;
        assert_eq!(names(&connection)?, ["kept"]);
        Ok(())
    }

    pub(super) fn engine_policy_never_waits_on_a_busy_store() -> Result<()> {
        let (_directory, path, mut holder) = file_database();
        items(&holder)?;
        let held = holder.transaction_with_behavior(TransactionBehavior::Immediate)?;
        held.execute("INSERT INTO items (name) VALUES ('held')", [])?;
        let contender = Connection::open(&path)?;
        contender.apply_engine_policy()?;
        let started = std::time::Instant::now();
        let busy = contender
            .execute("INSERT INTO items (name) VALUES ('blocked')", [])
            .expect_err("the write lock is held");
        let waited = started.elapsed();
        assert_eq!(busy.kind(), ErrorKind::Busy, "{busy}");
        assert!(waited < Duration::from_secs(1), "waited {waited:?}");
        held.commit()?;
        Ok(())
    }

    pub(super) fn damaged_pages_are_classified_corrupt() -> Result<()> {
        let (_directory, path, connection) = file_database();
        items(&connection)?;
        let mut insert = connection.prepare("INSERT INTO items (name) VALUES (?1)")?;
        for index in 0..200 {
            insert.execute([format!("{index}-{}", "x".repeat(500))])?;
        }
        drop(insert);
        connection.checkpoint(CheckpointMode::Truncate)?;
        drop(connection);
        let mut bytes = std::fs::read(&path).expect("read store");
        let page_size = match u16::from_be_bytes([bytes[16], bytes[17]]) {
            1 => 65_536,
            size => usize::from(size),
        };
        // Page 1 keeps the header and schema; every b-tree page after it is
        // overwritten.
        bytes[page_size..].fill(0xA5);
        std::fs::write(&path, bytes).expect("damage store");
        let reopened = Connection::open(&path)?;
        let error = reopened
            .query_row("SELECT count(*) FROM items", [], |row| row.get::<_, i64>(0))
            .expect_err("damaged pages cannot be read");
        assert_eq!(error.kind(), ErrorKind::Corrupt, "{error}");
        Ok(())
    }

    pub(super) fn statements_report_columns_and_conversions_carry_into_errors() -> Result<()> {
        fn narrow(value: ValueRef<'_>) -> Result<u8> {
            Ok(u8::column_result(value)?)
        }
        let connection = Connection::open_in_memory()?;
        let statement = connection.prepare("SELECT 1 AS a, 2 AS b")?;
        assert_eq!(statement.column_count(), 2);
        assert!(matches!(
            narrow(ValueRef::Integer(300)),
            Err(Error::IntegralValueOutOfRange(_, 300))
        ));
        assert!(matches!(
            narrow(ValueRef::Text(b"x")),
            Err(Error::FromSqlConversionFailure(..))
        ));
        Ok(())
    }
}
