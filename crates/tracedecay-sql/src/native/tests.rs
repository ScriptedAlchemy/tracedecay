use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use super::*;
use crate::Capability;

fn execution_guard() -> ExecutionGuard {
    ExecutionGuard::new(
        Some(Instant::now() + Duration::from_secs(30)),
        Arc::new(AtomicBool::new(false)),
        None,
    )
}

fn database() -> (tempfile::TempDir, Database) {
    let directory = tempfile::tempdir().unwrap();
    let database = Database::open(&directory.path().join("native.tdb")).unwrap();
    (directory, database)
}

fn verdict(sql: &str, capability: Capability) -> std::result::Result<(), String> {
    let (command, _) = policy::dialect().parse(sql).unwrap();
    policy::authorize(&command.unwrap(), capability).map_err(|error| error.to_string())
}

/// Catches a native verdict that diverges from the SQLite arm's authorizers,
/// including the prototype's Writer that allowed CREATE TRIGGER and VIEW.
#[test]
fn capability_verdicts_match_the_sqlite_authorizers() {
    use Capability::{Maintenance, Reader, Writer};
    let cases: &[(&str, &[Capability], &[Capability])] = &[
        ("SELECT 1", &[Reader, Writer, Maintenance], &[]),
        (
            "INSERT INTO t VALUES (1)",
            &[Writer, Maintenance],
            &[Reader],
        ),
        ("EXPLAIN DELETE FROM t", &[Writer, Maintenance], &[Reader]),
        (
            "CREATE TABLE IF NOT EXISTS t (x)",
            &[Writer, Maintenance],
            &[Reader],
        ),
        (
            "CREATE INDEX IF NOT EXISTS i ON t (x)",
            &[Writer, Maintenance],
            &[Reader],
        ),
        ("CREATE TEMP TABLE t (x)", &[Maintenance], &[Reader, Writer]),
        (
            "CREATE TRIGGER g AFTER INSERT ON t BEGIN SELECT 1; END",
            &[Maintenance],
            &[Reader, Writer],
        ),
        (
            "CREATE VIEW v AS SELECT 1",
            &[Maintenance],
            &[Reader, Writer],
        ),
        (
            "CREATE TEMP TRIGGER g AFTER INSERT ON t BEGIN SELECT 1; END",
            &[],
            &[Reader, Writer, Maintenance],
        ),
        (
            "CREATE VIEW temp.v AS SELECT 1",
            &[],
            &[Reader, Writer, Maintenance],
        ),
        ("DROP TABLE t", &[Maintenance], &[Reader, Writer]),
        ("DROP TRIGGER temp.g", &[], &[Reader, Writer, Maintenance]),
        (
            "ALTER TABLE t ADD COLUMN y",
            &[Maintenance],
            &[Reader, Writer],
        ),
        (
            "ATTACH ':memory:' AS other",
            &[],
            &[Reader, Writer, Maintenance],
        ),
        ("VACUUM", &[], &[Reader, Writer, Maintenance]),
        ("BEGIN IMMEDIATE", &[Reader, Writer, Maintenance], &[]),
        ("SAVEPOINT s", &[Reader, Writer, Maintenance], &[]),
        ("PRAGMA journal_mode", &[Reader, Writer, Maintenance], &[]),
        ("PRAGMA table_info(t)", &[Reader, Writer, Maintenance], &[]),
        (
            "PRAGMA wal_checkpoint(TRUNCATE)",
            &[Writer, Maintenance],
            &[Reader],
        ),
        (
            "PRAGMA auto_vacuum = 'incremental'",
            &[Writer, Maintenance],
            &[Reader],
        ),
        ("PRAGMA query_only = OFF", &[Maintenance], &[Reader, Writer]),
        (
            "PRAGMA foreign_keys = OFF",
            &[Maintenance],
            &[Reader, Writer],
        ),
        (
            "PRAGMA writable_schema = ON",
            &[Maintenance],
            &[Reader, Writer],
        ),
    ];
    for (sql, allowed, denied) in cases {
        for capability in *allowed {
            assert_eq!(
                verdict(sql, *capability),
                Ok(()),
                "{sql} under {capability:?}"
            );
        }
        for capability in *denied {
            assert!(
                verdict(sql, *capability).is_err(),
                "{sql} must be denied under {capability:?}"
            );
        }
    }
    assert!(
        verdict("PRAGMA secure_delete = ON", Maintenance)
            .is_err_and(|error| error.contains("secure deletion")),
        "secure delete is refused, never silently accepted"
    );
}

/// Catches a dialect that resolves extension loading or a catalog object
/// outside the allowlist, in direct SQL or a stored trigger body.
#[test]
fn denied_functions_and_catalog_objects_never_resolve() {
    let (_directory, database) = database();
    let mut connection = database.connect(Access::Writer).unwrap();
    connection.set_capability(Capability::Maintenance);
    let guard = execution_guard();
    connection
        .execute_batch(
            "CREATE TABLE entries(id INTEGER PRIMARY KEY, body TEXT)",
            &guard,
        )
        .unwrap();
    for sql in [
        "SELECT load_extension('missing')",
        "SELECT CASE WHEN 0 THEN load_extension('missing') ELSE 'safe' END",
        "SELECT \"load_extension\"('missing')",
        "SELECT nextval('s')",
    ] {
        assert!(
            matches!(connection.query(sql, &[], &guard), Err(Error::Denied(_))),
            "{sql}"
        );
    }
    if connection
        .execute(
            "CREATE TRIGGER dangerous AFTER INSERT ON entries BEGIN SELECT load_extension('x'); END",
            &[],
            &guard,
        )
        .is_ok()
    {
        assert!(
            connection
                .execute("INSERT INTO entries(body) VALUES ('fires')", &[], &guard)
                .is_err(),
            "a stored trigger body cannot load an extension"
        );
    }
    for table in ["sqlite_dbpage", "btree_dump", "turso_types"] {
        assert!(
            connection
                .query(&format!("SELECT * FROM {table}"), &[], &guard)
                .is_err(),
            "{table} must not resolve"
        );
    }
    for sql in [
        "SELECT count(*) FROM json_each('[1,2]')",
        "SELECT count(*) FROM pragma_table_info('entries')",
    ] {
        assert_eq!(
            connection.query(sql, &[], &guard).unwrap().values,
            vec![vec![Value::Integer(2)]],
            "{sql}"
        );
    }
}

/// Catches a progress guard that loses the interrupt reason, and a commit or
/// rollback that a revoked guard can strand.
#[test]
fn guards_interrupt_with_their_reason_and_never_strand_rollback() {
    let (_directory, database) = database();
    let mut writer = database.connect(Access::Writer).unwrap();
    let guard = execution_guard();
    writer
        .execute(
            "CREATE TABLE entries(id INTEGER PRIMARY KEY, body TEXT)",
            &[],
            &guard,
        )
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let authority_calls = Arc::clone(&calls);
    let revoked = ExecutionGuard::new(
        Some(Instant::now() + Duration::from_secs(30)),
        Arc::new(AtomicBool::new(false)),
        Some(Arc::new(move || {
            if authority_calls.fetch_add(1, Ordering::SeqCst) >= 4 {
                Err("revoked".to_owned())
            } else {
                Ok(())
            }
        })),
    );
    let result = writer.query(
        "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000000) SELECT sum(x) FROM n",
        &[],
        &revoked,
    );
    assert!(
        matches!(&result, Err(Error::Authority(message)) if message == "revoked"),
        "{result:?}"
    );
    assert!(
        calls.load(Ordering::SeqCst) >= 5,
        "the guard ran mid-statement"
    );
    writer
        .begin(TransactionBehavior::Immediate, &guard)
        .unwrap();
    writer
        .execute("INSERT INTO entries(body) VALUES('rollback')", &[], &guard)
        .unwrap();
    let cancelled = guard.clone();
    cancelled.cancelled.store(true, Ordering::Release);
    assert!(matches!(writer.commit(&cancelled), Err(Error::Cancelled)));
    writer.rollback().unwrap();
    assert!(writer.is_autocommit());
    assert_eq!(
        writer
            .query("SELECT count(*) FROM entries", &[], &execution_guard())
            .unwrap()
            .values,
        vec![vec![Value::Integer(0)]]
    );
    let expired = ExecutionGuard::new(Some(Instant::now()), Arc::new(AtomicBool::new(false)), None);
    assert!(matches!(
        writer.query("SELECT 1", &[], &expired),
        Err(Error::DeadlineExceeded)
    ));
}

/// Catches a batch that applies its first statements before a later one is
/// refused, and an unbounded materialized result.
#[test]
fn whole_batch_is_authorized_before_first_write_and_results_are_bounded() {
    let (_directory, database) = database();
    let mut writer = database.connect(Access::Writer).unwrap();
    let guard = execution_guard();
    writer
        .execute(
            "CREATE TABLE entries(id INTEGER PRIMARY KEY, body TEXT)",
            &[],
            &guard,
        )
        .unwrap();
    assert!(matches!(
        writer.execute_batch(
            "INSERT INTO entries(body) VALUES('must not apply'); ATTACH ':memory:' AS other",
            &guard
        ),
        Err(Error::Denied(_))
    ));
    assert_eq!(
        writer
            .query("SELECT count(*) FROM entries", &[], &guard)
            .unwrap()
            .values,
        vec![vec![Value::Integer(0)]]
    );
    assert!(matches!(
        writer.query(
            "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10001) SELECT x FROM n",
            &[],
            &guard
        ),
        Err(Error::QueryLimitExceeded)
    ));
}

/// Catches lost committed rows or a leaked uncommitted transaction across a
/// drop and reopen.
#[test]
fn drop_rolls_back_and_reopen_retains_only_committed_values() {
    let (directory, database) = database();
    let guard = execution_guard();
    {
        let mut writer = database.connect(Access::Writer).unwrap();
        writer
            .execute(
                "CREATE TABLE entries(id INTEGER PRIMARY KEY, body TEXT)",
                &[],
                &guard,
            )
            .unwrap();
        writer
            .execute("INSERT INTO entries(body) VALUES('committed')", &[], &guard)
            .unwrap();
        writer
            .begin(TransactionBehavior::Immediate, &guard)
            .unwrap();
        writer
            .execute("INSERT INTO entries(body) VALUES('dropped')", &[], &guard)
            .unwrap();
    }
    drop(database);
    let reopened = Database::open(&directory.path().join("native.tdb")).unwrap();
    let mut reader = reopened.connect(Access::Reader).unwrap();
    assert_eq!(
        reader
            .query("SELECT body FROM entries", &[], &guard)
            .unwrap()
            .values,
        vec![vec![Value::Text("committed".to_owned())]]
    );
    assert_eq!(
        reader
            .query("PRAGMA integrity_check", &[], &guard)
            .unwrap()
            .values,
        vec![vec![Value::Text("ok".to_owned())]]
    );
}

#[test]
fn foreign_keys_and_constraint_refusals_remain_typed() {
    let (_directory, database) = database();
    let mut writer = database.connect(Access::Writer).unwrap();
    let guard = execution_guard();
    writer.execute_batch("CREATE TABLE parents(id INTEGER PRIMARY KEY); CREATE TABLE children(id INTEGER PRIMARY KEY, parent_id INTEGER NOT NULL REFERENCES parents(id))", &guard).unwrap();
    let error = writer
        .execute(
            "INSERT INTO children(id,parent_id) VALUES(1,9)",
            &[],
            &guard,
        )
        .unwrap_err();
    assert!(error.is_deterministic_refusal(), "{error:?}");
    assert!(!error.is_transient_read_failure());
    assert!(!error.requires_transaction_retry());
    assert!(Error::Engine(turso_core::LimboError::WriteWriteConflict).requires_transaction_retry());
}

/// Catches a second engine instance on one file: the second open must see the
/// first open's write lock, also when it names the file by another path.
#[test]
fn in_process_opens_share_one_engine_by_file_identity() {
    let (directory, first) = database();
    let guard = execution_guard();
    let mut holder = first.connect(Access::Writer).unwrap();
    holder.execute("CREATE TABLE t(x)", &[], &guard).unwrap();
    holder
        .begin(TransactionBehavior::Immediate, &guard)
        .unwrap();
    holder
        .execute("INSERT INTO t VALUES (1)", &[], &guard)
        .unwrap();
    let alias = directory.path().join("alias.tdb");
    std::fs::hard_link(directory.path().join("native.tdb"), &alias).unwrap();
    for path in [directory.path().join("native.tdb"), alias] {
        let second = Database::open(&path).unwrap();
        let mut contender = second.connect(Access::Writer).unwrap();
        contender.set_busy_timeout(Duration::ZERO);
        assert!(
            matches!(
                contender.execute("INSERT INTO t VALUES (2)", &[], &guard),
                Err(Error::Busy)
            ),
            "{} opened a second engine instance",
            path.display()
        );
    }
    holder.commit(&guard).unwrap();
}

/// Catches an engine that opens, rewrites or stamps a file that is not a
/// TraceDecay native store.
#[test]
fn foreign_files_are_refused_before_the_engine_touches_them() {
    let directory = tempfile::tempdir().unwrap();
    let sqlite = directory.path().join("foreign.db");
    rusqlite::Connection::open(&sqlite)
        .unwrap()
        .execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (1);")
        .unwrap();
    let short = directory.path().join("short.tdb");
    std::fs::write(&short, [0_u8; 50]).unwrap();
    let garbage = directory.path().join("garbage.tdb");
    std::fs::write(&garbage, vec![7_u8; 4096]).unwrap();
    for path in [sqlite, short, garbage] {
        let before = std::fs::read(&path).unwrap();
        assert!(
            matches!(Database::open(&path), Err(Error::ForeignFormat)),
            "{} must be refused",
            path.display()
        );
        assert_eq!(std::fs::read(&path).unwrap(), before, "{}", path.display());
        let mut wal = path.clone().into_os_string();
        wal.push("-wal");
        assert!(!std::path::Path::new(&wal).exists(), "{}", path.display());
    }

    let (directory, fresh) = database();
    let guard = execution_guard();
    let mut connection = fresh.connect(Access::Writer).unwrap();
    assert_eq!(
        connection
            .query("PRAGMA application_id", &[], &guard)
            .unwrap()
            .values,
        vec![vec![Value::Integer(0x5444_4E31)]]
    );
    connection
        .query("PRAGMA wal_checkpoint(TRUNCATE)", &[], &guard)
        .unwrap();
    drop(connection);
    drop(fresh);
    let header = std::fs::read(directory.path().join("native.tdb")).unwrap();
    assert_eq!(&header[..16], b"SQLite format 3\0");
    assert_eq!(&header[68..72], b"TDN1");

    // A creation that crashed before its stamp reached the main file leaves
    // only the engine's unstamped first page; reopening finishes the stamp.
    let path = directory.path().join("native.tdb");
    let unstamp = |path: &std::path::Path| {
        let mut bytes = std::fs::read(path).unwrap();
        bytes[68..72].fill(0);
        std::fs::write(path, bytes).unwrap();
    };
    unstamp(&path);
    let resumed = Database::open(&path).unwrap();
    let mut connection = resumed.connect(Access::Writer).unwrap();
    connection
        .execute("CREATE TABLE t(x)", &[], &guard)
        .unwrap();
    connection
        .query("PRAGMA wal_checkpoint(TRUNCATE)", &[], &guard)
        .unwrap();
    drop(connection);
    drop(resumed);
    assert_eq!(&std::fs::read(&path).unwrap()[68..72], b"TDN1");
    unstamp(&path);
    assert!(
        matches!(Database::open(&path), Err(Error::ForeignFormat)),
        "an unstamped store with content is not adopted"
    );
}

/// Catches the engine reaching a path other than its main file and WAL.
#[test]
fn confined_io_admits_only_the_main_file_and_its_wal() {
    use turso_core::{IO, OpenFlags};
    let directory = tempfile::tempdir().unwrap();
    let main = directory.path().join("store.tdb");
    let file = confined_io::open_regular(&main, true).unwrap();
    let identity = confined_io::held_identity(&file).unwrap();
    let main = main.to_str().unwrap().to_owned();
    let io = confined_io::ConfinedIo::new(main.clone(), file, identity).unwrap();
    let elsewhere = directory.path().join("elsewhere");
    for path in [
        elsewhere.to_str().unwrap().to_owned(),
        format!("{main}-journal"),
        format!("{main}-shm"),
    ] {
        assert!(
            io.open_file(&path, OpenFlags::Create, false).is_err(),
            "{path}"
        );
        assert!(!std::path::Path::new(&path).exists(), "{path}");
    }
    assert!(io.remove_file(&main).is_err());
    assert!(std::path::Path::new(&main).exists());
    io.open_file(&main, OpenFlags::Create, false).unwrap();
    #[cfg(unix)]
    {
        let target = directory.path().join("target");
        std::fs::write(&target, b"").unwrap();
        std::os::unix::fs::symlink(&target, format!("{main}-wal")).unwrap();
        assert!(
            io.open_file(&format!("{main}-wal"), OpenFlags::Create, false)
                .is_err(),
            "a symlinked WAL is refused"
        );
        std::fs::remove_file(format!("{main}-wal")).unwrap();
    }
    io.open_file(&format!("{main}-wal"), OpenFlags::Create, false)
        .unwrap();
    io.remove_file(&format!("{main}-wal")).unwrap();
}

/// Catches a connection that keeps using a store whose pathname now names a
/// different file.
#[test]
fn identity_fence_rejects_replacement_and_unlink_recreate() {
    let (directory, database) = database();
    let path = directory.path().join("native.tdb");
    let guard = execution_guard();
    let mut writer = database.connect(Access::Writer).unwrap();
    writer
        .execute("CREATE TABLE entries(body TEXT)", &[], &guard)
        .unwrap();
    writer
        .execute("INSERT INTO entries VALUES('original')", &[], &guard)
        .unwrap();
    let staged = directory.path().join("renamed.tdb");
    std::fs::rename(&path, &staged).unwrap();
    std::fs::write(&path, b"replacement").unwrap();
    assert!(matches!(
        database.connect(Access::Reader),
        Err(Error::Denied(_))
    ));
    assert!(matches!(
        writer.query("SELECT * FROM entries", &[], &guard),
        Err(Error::Denied(_))
    ));
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(&staged, &path).unwrap();
    assert_eq!(
        writer
            .query("SELECT body FROM entries", &[], &guard)
            .unwrap()
            .values,
        vec![vec![Value::Text("original".to_owned())]]
    );
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, b"new inode").unwrap();
    assert!(matches!(
        writer.execute("INSERT INTO entries VALUES('must not apply')", &[], &guard),
        Err(Error::Denied(_))
    ));
}

/// (voluntary context switches, CPU clock ticks) of the calling thread.
#[cfg(target_os = "linux")]
fn thread_scheduling() -> (u64, u64) {
    let status = std::fs::read_to_string("/proc/thread-self/status").unwrap();
    let voluntary = status
        .lines()
        .find_map(|line| line.strip_prefix("voluntary_ctxt_switches:"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let stat = std::fs::read_to_string("/proc/thread-self/stat").unwrap();
    let fields: Vec<&str> = stat.rsplit_once(") ").unwrap().1.split(' ').collect();
    let ticks = fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap();
    (voluntary, ticks)
}

/// Catches a busy wait that pumps the no-op IO loop instead of sleeping, and
/// a sleep that outlives the statement deadline.
#[cfg(target_os = "linux")]
#[test]
fn busy_waits_sleep_within_the_deadline_instead_of_spinning() {
    let (_directory, database) = database();
    let guard = execution_guard();
    let mut holder = database.connect(Access::Writer).unwrap();
    holder.execute("CREATE TABLE t(x)", &[], &guard).unwrap();
    holder
        .begin(TransactionBehavior::Immediate, &guard)
        .unwrap();
    holder
        .execute("INSERT INTO t VALUES (1)", &[], &guard)
        .unwrap();
    let mut contender = database.connect(Access::Writer).unwrap();
    contender.set_busy_timeout(Duration::from_millis(400));
    let (switches, ticks) = thread_scheduling();
    let started = Instant::now();
    let result = contender.execute("INSERT INTO t VALUES (2)", &[], &guard);
    let elapsed = started.elapsed();
    let (switches_after, ticks_after) = thread_scheduling();
    assert!(matches!(result, Err(Error::Busy)), "{result:?}");
    assert!(elapsed >= Duration::from_millis(300), "{elapsed:?}");
    assert!(
        switches_after - switches >= 5,
        "the wait slept {} times",
        switches_after - switches
    );
    let cpu = Duration::from_millis((ticks_after - ticks) * 10);
    assert!(cpu * 2 <= elapsed, "{cpu:?} of CPU over {elapsed:?}");

    contender.set_busy_timeout(Duration::from_secs(30));
    let bounded = ExecutionGuard::new(
        Some(Instant::now() + Duration::from_millis(150)),
        Arc::new(AtomicBool::new(false)),
        None,
    );
    let started = Instant::now();
    let result = contender.execute("INSERT INTO t VALUES (3)", &[], &bounded);
    assert!(matches!(result, Err(Error::DeadlineExceeded)), "{result:?}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    holder.commit(&guard).unwrap();
}

/// Catches authority baked into a compiled or cached statement.
#[test]
fn compiled_statements_are_readmitted_under_the_current_capability() {
    let (_directory, database) = database();
    let connection = database.connect(Access::Writer).unwrap();
    connection.run_batch("CREATE TABLE t(x)").unwrap();
    let insert = "INSERT INTO t VALUES (1)";
    let cached = connection.prepare(insert, true).unwrap();
    connection.recycle(cached);
    connection.set_capability(Capability::Reader);
    assert!(matches!(
        connection.prepare(insert, true),
        Err(Error::Denied(_))
    ));
    connection.set_capability(Capability::Writer);
    let mut held = connection.prepare(insert, false).unwrap();
    connection.set_capability(Capability::Reader);
    assert!(matches!(connection.admit(&mut held), Err(Error::Denied(_))));
    connection.set_capability(Capability::Writer);
    connection.admit(&mut held).unwrap();
    while connection.step(&mut held).unwrap() {}

    let revoked = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&revoked);
    connection.push_guard(ExecutionGuard::new(
        None,
        Arc::new(AtomicBool::new(false)),
        Some(Arc::new(move || {
            if flag.load(Ordering::SeqCst) {
                Err("revoked".to_owned())
            } else {
                Ok(())
            }
        })),
    ));
    let mut select = connection.prepare("SELECT count(*) FROM t", true).unwrap();
    connection.admit(&mut select).unwrap();
    assert!(connection.step(&mut select).unwrap());
    connection.recycle(select);
    revoked.store(true, Ordering::SeqCst);
    let mut select = connection.prepare("SELECT count(*) FROM t", true).unwrap();
    assert!(matches!(
        connection.admit(&mut select),
        Err(Error::Authority(message)) if message == "revoked"
    ));
    connection.pop_guard();
}

#[cfg(td_engine = "native")]
mod driver {
    use std::time::Duration;

    use crate::{
        Capability, CheckpointMode, Connection, ConnectionControl, EnginePolicy, ErrorExt,
        ErrorKind,
    };

    fn open() -> (tempfile::TempDir, std::path::PathBuf, Connection) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("driver.tdb");
        let connection = Connection::open(&path).unwrap();
        (directory, path, connection)
    }

    /// Catches a COMMIT failure reported as an ordinary error, which a writer
    /// would settle as rolled back.
    #[test]
    fn commit_failure_after_commit_starts_is_outcome_unknown() {
        let (_directory, _path, mut connection) = open();
        connection
            .execute_batch(
                "CREATE TABLE parents(id INTEGER PRIMARY KEY); \
                 CREATE TABLE children(id INTEGER PRIMARY KEY, parent_id INTEGER \
                 REFERENCES parents(id) DEFERRABLE INITIALLY DEFERRED)",
            )
            .unwrap();
        let transaction = connection.transaction().unwrap();
        transaction
            .execute("INSERT INTO children VALUES (1, 9)", [])
            .unwrap();
        let error = transaction.commit().unwrap_err();
        assert_eq!(error.kind(), ErrorKind::CommitOutcomeUnknown, "{error}");
    }

    /// Catches a statement cache that serves a program compiled under one
    /// capability to another, and a stale program after DDL.
    #[test]
    fn cached_statements_follow_capability_and_schema() {
        let (_directory, _path, connection) = open();
        connection.execute_batch("CREATE TABLE t(a)").unwrap();
        connection.set_capability(Capability::Writer).unwrap();
        connection.execute("INSERT INTO t VALUES (1)", []).unwrap();
        let denied = connection
            .with_capability(Capability::Reader, Capability::Writer, |reader| {
                reader.execute("INSERT INTO t VALUES (2)", [])
            })
            .unwrap_err();
        assert_eq!(denied.kind(), ErrorKind::Denied, "{denied}");
        let mut held = connection.prepare("INSERT INTO t VALUES (3)").unwrap();
        let denied = connection
            .with_capability(Capability::Reader, Capability::Writer, |_| held.execute([]))
            .unwrap_err();
        assert_eq!(denied.kind(), ErrorKind::Denied, "{denied}");
        held.execute([]).unwrap();
        drop(held);

        connection.set_capability(Capability::Maintenance).unwrap();
        let columns = |connection: &Connection| {
            connection
                .prepare_cached("SELECT * FROM t")
                .unwrap()
                .query_row([], |row| Ok(row.get_ref(1).is_ok()))
                .unwrap()
        };
        assert!(!columns(&connection));
        connection
            .execute_batch("DROP TABLE t; CREATE TABLE t(a, b); INSERT INTO t VALUES (1, 2)")
            .unwrap();
        assert!(
            columns(&connection),
            "the cached program saw the old schema"
        );
    }

    /// Catches materialized results: a driver query streams past the facade's
    /// row bound and releases its read when dropped mid-way.
    #[test]
    fn rows_stream_and_release_their_read() {
        let (_directory, path, connection) = open();
        connection.execute_batch("CREATE TABLE t(x)").unwrap();
        let mut statement = connection
            .prepare(
                "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<20000) \
                 SELECT x FROM n",
            )
            .unwrap();
        let count = statement
            .query_map([], |row| row.get::<_, i64>(0))
            .unwrap()
            .count();
        assert_eq!(count, 20_000);
        connection
            .execute("INSERT INTO t VALUES (1), (2), (3)", [])
            .unwrap();
        let mut select = connection.prepare("SELECT x FROM t").unwrap();
        let mut rows = select.query([]).unwrap();
        assert!(rows.next().unwrap().is_some());
        drop(rows);
        let checkpointer = Connection::open(&path).unwrap();
        checkpointer.busy_timeout(Duration::ZERO).unwrap();
        let outcome = checkpointer.checkpoint(CheckpointMode::Truncate).unwrap();
        assert!(!outcome.busy, "a dropped Rows still holds its read");
    }

    /// Catches an interrupt handle that cannot stop a running statement.
    #[test]
    fn interrupt_handle_stops_a_running_statement() {
        let (_directory, _path, connection) = open();
        let handle = connection.get_interrupt_handle();
        let interrupter = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            handle.interrupt();
        });
        let error = connection
            .query_row(
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) \
                 SELECT count(*) FROM n",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_err();
        interrupter.join().unwrap();
        assert_eq!(error.kind(), ErrorKind::Interrupted, "{error}");
        let after: i64 = connection
            .query_row("SELECT 1", [], |row| row.get(0))
            .unwrap();
        assert_eq!(after, 1);
    }

    /// Catches a control surface that is missing or answers wrongly on native:
    /// every checkpoint mode, WAL mode with no automatic checkpoint, and file
    /// identity.
    #[test]
    fn control_surface_reports_the_store() {
        let (directory, path, connection) = open();
        let mode: String = connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(mode.to_ascii_lowercase(), "wal");
        connection
            .execute_batch("CREATE TABLE t(payload BLOB)")
            .unwrap();
        let mut insert = connection.prepare("INSERT INTO t VALUES (?1)").unwrap();
        for _ in 0..1_200 {
            insert.execute([vec![0_u8; 4_000]]).unwrap();
        }
        drop(insert);
        let passive = connection.checkpoint(CheckpointMode::Passive).unwrap();
        assert!(
            passive.log_frames >= 1_200,
            "an automatic checkpoint ran: {passive:?}"
        );
        for mode in [
            CheckpointMode::Full,
            CheckpointMode::Restart,
            CheckpointMode::Truncate,
        ] {
            let outcome = connection.checkpoint(mode).unwrap();
            assert!(!outcome.busy, "{mode:?}: {outcome:?}");
        }
        let mut wal = path.clone().into_os_string();
        wal.push("-wal");
        assert_eq!(std::fs::metadata(&wal).unwrap().len(), 0);
        assert!(!connection.file_has_moved().unwrap());
        let moved = directory.path().join("moved.tdb");
        std::fs::rename(&path, &moved).unwrap();
        assert!(connection.file_has_moved().unwrap());
        std::fs::rename(&moved, &path).unwrap();
        assert!(!connection.file_has_moved().unwrap());
    }

    /// Catches a driver open that accepts a SQLite file another engine owns.
    #[test]
    fn opening_a_foreign_sqlite_file_is_foreign_format() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("foreign.db");
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE t(x)")
            .unwrap();
        let error = Connection::open(&path).err().unwrap();
        assert_eq!(error.kind(), ErrorKind::ForeignFormat, "{error}");
    }

    /// Catches a runaway statement the engine policy does not bound, and a
    /// budget that leaks from one statement into the next.
    #[test]
    fn engine_policy_bounds_vm_operations_per_statement() {
        let (_directory, _path, connection) = open();
        connection.apply_engine_policy().unwrap();
        let error = connection
            .query_row(
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) \
                 SELECT count(*) FROM n",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded, "{error}");
        for _ in 0..3 {
            let count: i64 = connection
                .query_row(
                    "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n \
                     WHERE i < 100000) SELECT count(*) FROM n",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 100_000);
        }
    }

    #[cfg(unix)]
    const POWER_LOSS_CHILD: &str = "TD_SQL_POWER_LOSS_CHILD";

    /// The re-executed child of `power_loss_keeps_acknowledged_commits`: it
    /// commits rows until the armed power cut aborts it, reporting each
    /// acknowledged commit on stdout.
    #[cfg(unix)]
    #[test]
    fn power_loss_child() {
        use std::io::Write;

        use crate::native::crash_io::{CrashEffect, CrashOp, CrashPlan, CrashTarget, arm};

        let Ok(spec) = std::env::var(POWER_LOSS_CHILD) else {
            return;
        };
        let (nth, path) = spec.split_once(' ').unwrap();
        arm(CrashPlan {
            target: CrashTarget::Wal,
            op: CrashOp::Write,
            nth: nth.parse().unwrap(),
            effect: CrashEffect::PowerLoss,
        });
        let mut connection = Connection::open(path).unwrap();
        // A caller trying to trade durability for speed.
        let _ = connection.execute_batch("PRAGMA synchronous = OFF");
        for index in 0_i64.. {
            let transaction = connection.transaction().unwrap();
            transaction
                .execute(
                    "INSERT INTO rows (id, payload) VALUES (?1, ?2)",
                    (index, "x".repeat(8_000)),
                )
                .unwrap();
            transaction.commit().unwrap();
            let mut out = std::io::stdout().lock();
            writeln!(out, "TD|ack {index}").unwrap();
            out.flush().unwrap();
        }
    }

    /// Catches commits acknowledged before they are durable: a power cut
    /// after a commit returned must not lose it, even when the caller asked
    /// for less durability.
    #[cfg(unix)]
    #[test]
    fn power_loss_keeps_acknowledged_commits() {
        use std::os::unix::process::ExitStatusExt;

        for nth in [4, 9, 15, 24] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("power.tdb");
            Connection::open(&path)
                .unwrap()
                .execute_batch("CREATE TABLE rows (id INTEGER PRIMARY KEY, payload TEXT NOT NULL)")
                .unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "native::tests::driver::power_loss_child",
                    "--exact",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(POWER_LOSS_CHILD, format!("{nth} {}", path.display()))
                .output()
                .unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.signal() == Some(6) && stderr.contains("TD_CRASH "),
                "the child did not crash at WAL write {nth}: {}: {stderr}",
                output.status
            );
            let acknowledged = String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| {
                    line.split_once("TD|ack ")
                        .map(|(_, index)| index.to_owned())
                })
                .map(|index| index.parse::<i64>().unwrap())
                .collect::<Vec<_>>();
            assert!(
                !acknowledged.is_empty(),
                "WAL write {nth}: no commit acknowledged"
            );
            let reopened = Connection::open(&path).unwrap();
            let mut statement = reopened.prepare("SELECT id FROM rows ORDER BY id").unwrap();
            let durable = statement
                .query_map([], |row| row.get::<_, i64>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert!(
                acknowledged.iter().all(|index| durable.contains(index)),
                "a power cut at WAL write {nth} lost acknowledged commits: \
                 acknowledged {acknowledged:?}, durable {durable:?}"
            );
        }
    }
}
