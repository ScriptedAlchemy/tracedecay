//! One behavior contract for both engine arms: every case runs, with the same
//! assertions, under the default build and under `--config=native`.

use std::time::Duration;

use tracedecay_sql::{
    Capability, CheckpointMode, Connection, ConnectionControl, EnginePolicy, Error, ErrorExt,
    ErrorKind, OptionalExtension, Result, StatementControl, StatementCounters, Transaction,
    TransactionBehavior, TransactionControl, params, params_from_iter,
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
    raised_and_checked_refusals_carry_the_schema_text,
    busy_is_classified,
    capability_scope_denies_and_restores,
    exact_sql_scope_fails_on_swallowed_transaction_control,
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
    unchecked_transactions_commit_or_roll_back_through_a_shared_borrow,
    close_rolls_back_and_path_names_the_store,
    deferred_constraint_at_commit_is_a_refusal_that_rolls_back,
    busy_checkpoint_reports_the_wal_it_could_not_reset,
    a_copied_back_wal_starts_over_on_the_next_write,
    read_only_open_reads_without_creating_a_wal,
    an_interrupted_write_ends_its_transaction,
    bound_limit_and_offset_restart_on_every_subquery_rescan,
    unique_index_entries_stay_findable_under_every_conflict_mode,
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
        // Callers store and compare these messages, so both engines word
        // them alike.
        assert_eq!(error.to_string(), "Query returned no rows");
        assert_eq!(
            Error::InvalidParameterName("stored digest mismatch".to_owned()).to_string(),
            "Invalid parameter name: stored digest mismatch"
        );
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
        assert_eq!(names(&connection)?, ["a", "b", "c"]);
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
        let wide: i64 = connection.query_row("SELECT 300", [], |row| row.get(0))?;
        assert_eq!(wide, 300, "the same value remains readable as i64");
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

    pub(super) fn raised_and_checked_refusals_carry_the_schema_text() -> Result<()> {
        let connection = Connection::open_in_memory()?;
        connection.execute_batch(
            "CREATE TABLE ledger (id INTEGER PRIMARY KEY, amount INTEGER NOT NULL CHECK (amount >= 0));
             CREATE TRIGGER ledger_immutable BEFORE UPDATE ON ledger
             BEGIN SELECT RAISE(ABORT, 'ledger rows are immutable'); END;
             INSERT INTO ledger (amount) VALUES (1);",
        )?;
        let raised = connection
            .execute("UPDATE ledger SET amount = 2", [])
            .expect_err("the trigger refuses updates");
        assert_eq!(raised.kind(), ErrorKind::Constraint, "{raised}");
        assert_eq!(raised.to_string(), "ledger rows are immutable");
        let checked = connection
            .execute("INSERT INTO ledger (amount) VALUES (-1)", [])
            .expect_err("the CHECK refuses negative amounts");
        assert_eq!(checked.kind(), ErrorKind::Constraint, "{checked}");
        assert!(
            checked
                .to_string()
                .starts_with("CHECK constraint failed: amount >= 0"),
            "{checked}"
        );
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

    /// Catches an exact SQL scope that succeeds after its operation swallowed a
    /// transaction-control denial, or a denial that leaks into the next scope.
    pub(super) fn exact_sql_scope_fails_on_swallowed_transaction_control() -> Result<()> {
        let connection = Connection::open_in_memory()?;
        items(&connection)?;
        connection.set_capability(Capability::Writer)?;
        let exact_sql = Capability::ExactSql { savepoints: false };
        let swallowed = connection
            .with_capability(exact_sql, Capability::Writer, |scoped| {
                let _ = scoped.execute_batch("BEGIN");
                scoped.execute("INSERT INTO items (name) VALUES ('scoped')", [])
            })
            .expect_err("a denied BEGIN fails the scope");
        assert_eq!(
            swallowed.kind(),
            ErrorKind::TransactionControlDenied,
            "{swallowed}"
        );
        assert!(connection.is_autocommit(), "the denied BEGIN never ran");
        connection.with_capability(exact_sql, Capability::Writer, |scoped| {
            scoped.execute("INSERT INTO items (name) VALUES ('clean')", [])
        })?;
        assert_eq!(names(&connection)?, ["scoped", "clean"]);
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

    /// An interrupted INSERT inside a transaction takes the transaction with
    /// it, as SQLite's interrupt does; the runtime's writer relies on that.
    pub(super) fn an_interrupted_write_ends_its_transaction() -> Result<()> {
        let (_directory, _path, mut connection) = file_database();
        items(&connection)?;
        connection.execute("INSERT INTO items (name) VALUES ('before')", [])?;
        let transaction = connection.transaction()?;
        transaction.execute("INSERT INTO items (name) VALUES ('inside')", [])?;
        let interrupted = transaction
            .guard_scope(
                || true,
                |connection| {
                    connection.execute_batch(
                        "INSERT INTO items (name)
                         WITH RECURSIVE n(i) AS (
                            SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 10000000
                         )
                         SELECT 'row-' || i FROM n",
                    )
                },
            )?
            .expect_err("an always-true guard interrupts the insert");
        assert_eq!(interrupted.kind(), ErrorKind::Interrupted, "{interrupted}");
        assert!(
            transaction.is_autocommit(),
            "the interrupted write ended its transaction"
        );
        drop(transaction);
        assert_eq!(names(&connection)?, ["before"]);
        Ok(())
    }

    pub(super) fn page_stats_and_checkpoint_report_the_file() -> Result<()> {
        let (_directory, path, connection) = file_database();
        items(&connection)?;
        let before = connection.page_stats()?;
        connection.execute("INSERT INTO items (name) VALUES ('page')", [])?;
        let after = connection.page_stats()?;
        assert!(
            after.page_count >= before.page_count,
            "inserting a row must not shrink the file: before={before:?} after={after:?}"
        );
        let checkpoint = connection.checkpoint(CheckpointMode::Truncate)?;
        assert!(!checkpoint.busy, "no reader blocks the checkpoint");
        drop(connection);
        let reopened = Connection::open(&path)?;
        assert_eq!(names(&reopened)?, ["page"]);
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

    /// Without a restart the WAL only grows between maintenance truncations,
    /// however often its frames are copied back.
    pub(super) fn a_copied_back_wal_starts_over_on_the_next_write() -> Result<()> {
        let (_directory, _path, connection) = file_database();
        connection.apply_engine_policy()?;
        connection.execute_batch("PRAGMA journal_mode = WAL")?;
        items(&connection)?;
        let mut insert = connection.prepare("INSERT INTO items (name) VALUES (?1)")?;
        for index in 0..20 {
            insert.execute([format!("{index}-{}", "x".repeat(4_000))])?;
        }
        drop(insert);
        let grown = connection.checkpoint(CheckpointMode::Noop)?.log_frames;
        let passive = connection.checkpoint(CheckpointMode::Passive)?;
        assert!(!passive.busy, "{passive:?}");
        connection.execute("INSERT INTO items (name) VALUES ('next')", [])?;
        let next = connection.checkpoint(CheckpointMode::Noop)?.log_frames;
        assert!(
            next < grown,
            "the WAL kept growing after every frame was copied back: {grown} then {next}"
        );
        Ok(())
    }

    /// A reader's snapshot blocks a TRUNCATE checkpoint: the outcome is busy
    /// and still counts the WAL it left in place.
    pub(super) fn busy_checkpoint_reports_the_wal_it_could_not_reset() -> Result<()> {
        let (_directory, path, writer) = file_database();
        writer.apply_engine_policy()?;
        writer.execute_batch("PRAGMA journal_mode = WAL")?;
        items(&writer)?;
        writer.execute("INSERT INTO items (name) VALUES ('seen')", [])?;
        let mut reader = Connection::open(&path)?;
        reader.apply_engine_policy()?;
        let snapshot = reader.transaction()?;
        let seen: i64 = snapshot.query_row("SELECT count(*) FROM items", [], |row| row.get(0))?;
        assert_eq!(seen, 1);
        writer.execute("INSERT INTO items (name) VALUES ('unseen')", [])?;

        let outcome = writer.checkpoint(CheckpointMode::Truncate)?;
        assert!(outcome.busy, "the snapshot blocks the reset: {outcome:?}");
        assert!(outcome.log_frames > 0, "{outcome:?}");
        assert!(
            (0..=outcome.log_frames).contains(&outcome.checkpointed_frames),
            "{outcome:?}"
        );
        drop(snapshot);
        let cleared = writer.checkpoint(CheckpointMode::Truncate)?;
        assert!(!cleared.busy, "{cleared:?}");
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
            let _ = statement.query_map([], |row| row.get::<_, i64>(0))?.count();
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
        assert_eq!(statement.column_names(), ["a", "b"]);
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

    pub(super) fn unchecked_transactions_commit_or_roll_back_through_a_shared_borrow() -> Result<()>
    {
        let connection = Connection::open_in_memory()?;
        items(&connection)?;
        let committed = Transaction::new_unchecked(&connection, TransactionBehavior::Immediate)?;
        committed.execute("INSERT INTO items (name) VALUES ('kept')", [])?;
        committed.commit()?;
        let dropped = Transaction::new_unchecked(&connection, TransactionBehavior::Deferred)?;
        dropped.execute("INSERT INTO items (name) VALUES ('dropped')", [])?;
        assert!(
            !connection.is_autocommit(),
            "the shared borrow opened a transaction"
        );
        drop(dropped);
        assert!(connection.is_autocommit());
        assert_eq!(names(&connection)?, ["kept"]);
        Ok(())
    }

    /// A deferred foreign key is checked before COMMIT writes anything: the
    /// failure is a known refusal, not an unknown outcome, and nothing stays.
    pub(super) fn deferred_constraint_at_commit_is_a_refusal_that_rolls_back() -> Result<()> {
        let mut connection = Connection::open_in_memory()?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE parent (id INTEGER PRIMARY KEY);
             CREATE TABLE child (
                id INTEGER PRIMARY KEY,
                parent INTEGER REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED
             );",
        )?;
        let transaction = connection.transaction()?;
        transaction.execute("INSERT INTO child VALUES (1, 9)", [])?;
        let error = transaction
            .commit()
            .expect_err("the dangling child refuses the commit");
        assert_eq!(error.kind(), ErrorKind::Constraint, "{error}");
        assert!(
            connection.is_autocommit(),
            "the refused transaction is gone"
        );
        let children: i64 =
            connection.query_row("SELECT count(*) FROM child", [], |row| row.get(0))?;
        assert_eq!(children, 0);
        Ok(())
    }

    /// Catches a read-only open that leaves a `-wal` beside a sealed file,
    /// which content-addressed inventories refuse as an unknown file.
    pub(super) fn read_only_open_reads_without_creating_a_wal() -> Result<()> {
        let (_directory, path, connection) = file_database();
        items(&connection)?;
        connection.execute("INSERT INTO items (name) VALUES ('sealed')", [])?;
        connection.checkpoint(CheckpointMode::Truncate)?;
        drop(connection);
        let mut wal = path.clone().into_os_string();
        wal.push("-wal");
        let wal = std::path::PathBuf::from(wal);
        if wal.exists() {
            assert_eq!(std::fs::metadata(&wal).expect("wal").len(), 0);
            std::fs::remove_file(&wal).expect("remove the folded WAL");
        }

        let reader = Connection::open_with_flags(
            &path,
            tracedecay_sql::OpenFlags::SQLITE_OPEN_READ_ONLY
                | tracedecay_sql::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        assert_eq!(names(&reader)?, ["sealed"]);
        assert!(!wal.exists(), "an open read-only store has no WAL file");
        drop(reader);
        assert!(!wal.exists(), "a closed read-only store left a WAL file");
        Ok(())
    }

    /// Every row is findable through each unique index, and each index scans
    /// in key order, after inserts that probe, skip, ignore and replace in
    /// scattered key order. An index entry written at a stale cursor position
    /// breaks both.
    pub(super) fn unique_index_entries_stay_findable_under_every_conflict_mode() -> Result<()> {
        use std::collections::BTreeMap;

        fn key(i: u64) -> String {
            format!("k{:05}", i.wrapping_mul(7_919) % 10_007)
        }
        fn number(i: u64) -> i64 {
            (i.wrapping_mul(104_729) % 99_991) as i64
        }
        fn verify(
            connection: &Connection,
            table: &str,
            rows: &BTreeMap<String, i64>,
        ) -> Result<()> {
            for (a, b) in rows {
                let by_a: Option<i64> = connection
                    .query_row(&format!("SELECT b FROM {table} WHERE a = ?1"), [a], |row| {
                        row.get(0)
                    })
                    .optional()?;
                assert_eq!(by_a, Some(*b), "{table}: row {a} lost from the a index");
                let by_b: Option<String> = connection
                    .query_row(&format!("SELECT a FROM {table} WHERE b = ?1"), [b], |row| {
                        row.get(0)
                    })
                    .optional()?;
                assert_eq!(
                    by_b.as_ref(),
                    Some(a),
                    "{table}: row {b} lost from the b index"
                );
            }
            let mut statement = connection.prepare(&format!("SELECT a FROM {table} ORDER BY a"))?;
            let scanned = statement
                .query_map([], |row| row.get(0))?
                .collect::<Result<Vec<String>>>()?;
            assert_eq!(
                scanned,
                rows.keys().cloned().collect::<Vec<_>>(),
                "{table}: the a index scan is not the row set in key order"
            );
            Ok(())
        }

        let connection = Connection::open_in_memory()?;
        connection.execute_batch(
            "CREATE TABLE pairs (id INTEGER PRIMARY KEY, a TEXT NOT NULL UNIQUE, b INTEGER NOT NULL UNIQUE);
             CREATE TABLE mixed (id INTEGER PRIMARY KEY, a TEXT NOT NULL UNIQUE, b INTEGER NOT NULL UNIQUE ON CONFLICT REPLACE);",
        )?;
        let mut pairs = BTreeMap::new();
        let mut plain = connection.prepare("INSERT INTO pairs (a, b) VALUES (?1, ?2)")?;
        for i in 0..600 {
            plain.execute(params![key(i), number(i)])?;
            pairs.insert(key(i), number(i));
        }
        let mut upsert = connection
            .prepare("INSERT INTO pairs (a, b) VALUES (?1, ?2) ON CONFLICT(a) DO NOTHING")?;
        for i in 300..900 {
            let b = 200_000 + i as i64;
            upsert.execute(params![key(i), b])?;
            pairs.entry(key(i)).or_insert(b);
        }
        let mut ignore =
            connection.prepare("INSERT OR IGNORE INTO pairs (a, b) VALUES (?1, ?2)")?;
        for i in 850..1_000 {
            let b = number(i % 700);
            ignore.execute(params![key(i), b])?;
            if !pairs.contains_key(&key(i)) && !pairs.values().any(|existing| *existing == b) {
                pairs.insert(key(i), b);
            }
        }
        verify(&connection, "pairs", &pairs)?;

        let mut mixed = BTreeMap::new();
        let mut replace = connection.prepare("INSERT INTO mixed (a, b) VALUES (?1, ?2)")?;
        for i in 0..500 {
            let b = number(i % 150);
            replace.execute(params![key(i), b])?;
            mixed.retain(|_, existing| *existing != b);
            mixed.insert(key(i), b);
        }
        verify(&connection, "mixed", &mixed)
    }

    pub(super) fn close_rolls_back_and_path_names_the_store() -> Result<()> {
        let (_directory, path, connection) = file_database();
        items(&connection)?;
        let reported = std::fs::canonicalize(connection.path().expect("a file store has a path"))
            .expect("the reported path exists");
        assert_eq!(
            reported,
            std::fs::canonicalize(&path).expect("store exists")
        );
        connection.execute_batch("BEGIN; INSERT INTO items (name) VALUES ('open');")?;
        connection.close().map_err(|(_, error)| error)?;
        let reopened = Connection::open(&path)?;
        assert!(
            names(&reopened)?.is_empty(),
            "close rolled back the open write"
        );
        Ok(())
    }

    /// Each statement runs a nested query again for every outer row. The
    /// CROSS JOIN keeps the CTE on the inner side, and its unindexable join
    /// key keeps Turso from materializing it once into a seek index; the
    /// correlated subquery reruns per row by definition. A bound LIMIT or
    /// OFFSET must count from its bound value on every run, as a literal does
    /// (turso-bugs/cte-limit-parameter-drops-groups.md).
    pub(super) fn bound_limit_and_offset_restart_on_every_subquery_rescan() -> Result<()> {
        let connection = Connection::open_in_memory()?;
        connection.execute_batch(
            "CREATE TABLE facts (obs TEXT NOT NULL, ord INTEGER NOT NULL);
             INSERT INTO facts VALUES ('a', 1), ('a', 2), ('b', 3);",
        )?;
        let joined = |subquery: &str| {
            format!(
                "WITH m AS ({subquery})
                 SELECT f.ord, m.obs FROM facts f CROSS JOIN m
                 WHERE m.obs || '' = f.obs ORDER BY f.ord"
            )
        };
        let grouped = "SELECT obs, MIN(ord) AS first FROM facts GROUP BY obs ORDER BY first";
        let compound = "SELECT obs, ord FROM facts WHERE ord = 1
             UNION ALL SELECT obs, ord FROM facts WHERE ord = 3";
        let cases: [(String, &[i64], &[(i64, &str)]); 5] = [
            (
                joined(&format!("{grouped} LIMIT ?1")),
                &[4],
                &[(1, "a"), (2, "a"), (3, "b")],
            ),
            (
                joined(&format!("{grouped} LIMIT ?1 OFFSET ?2")),
                &[1, 1],
                &[(3, "b")],
            ),
            (
                joined(&format!("{compound} LIMIT ?1 OFFSET ?2")),
                &[1, 1],
                &[(3, "b")],
            ),
            (
                joined(&format!("{compound} ORDER BY 2 LIMIT ?1 OFFSET ?2")),
                &[1, 1],
                &[(3, "b")],
            ),
            (
                "SELECT f.ord, COALESCE((SELECT g.obs FROM facts g WHERE g.ord >= f.ord
                     ORDER BY g.ord LIMIT ?1 OFFSET ?2), '-')
                 FROM facts f ORDER BY f.ord"
                    .to_owned(),
                &[1, 1],
                &[(1, "a"), (2, "b"), (3, "-")],
            ),
        ];
        let mut mismatches = Vec::new();
        for (sql, bound, expected) in cases {
            let mut statement = connection.prepare(&sql)?;
            let rows = statement
                .query_map(params_from_iter(bound.iter()), |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>>>()?;
            let expected = expected
                .iter()
                .map(|&(ord, obs)| (ord, obs.to_owned()))
                .collect::<Vec<_>>();
            if rows != expected {
                mismatches.push(format!(
                    "{sql}\n  bound {bound:?}: got {rows:?}, want {expected:?}"
                ));
            }
        }
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
        Ok(())
    }
}
