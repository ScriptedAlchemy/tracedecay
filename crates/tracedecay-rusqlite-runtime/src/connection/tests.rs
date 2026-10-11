use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;
use tracedecay_sql::{Connection, ConnectionControl, ErrorExt, ErrorKind};
use tracedecay_store::WAL_SOFT_LIMIT_BYTES;

use super::{ConnectionMode, OpenedDatabaseFile, OpenedDatabaseFileError, open, open_writer};

fn database() -> NamedTempFile {
    let file = NamedTempFile::new().expect("temporary database");
    let connection = Connection::open(file.path()).expect("initialize database");
    connection
        .execute_batch("CREATE TABLE items(value INTEGER); INSERT INTO items VALUES (1);")
        .expect("initialize schema");
    drop(connection);
    file
}

fn pragma_i64(connection: &Connection, name: &str) -> i64 {
    connection
        .pragma_query_value(None, name, |row| row.get(0))
        .expect("read pragma")
}

fn sidecar_path(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    sidecar.into()
}

/// Production writer policy: WAL, `synchronous=NORMAL` on SQLite, and no
/// automatic checkpoint. This is the durability contract; it does not consult
/// any process environment variable. Native durability is proven by the crash
/// harness's power-loss modes (`tests/crash`), not by a pragma value.
#[test]
fn writer_mode_applies_wal_integrity_and_write_policy() {
    let file = database();
    let connection = open(file.path(), ConnectionMode::Writer).expect("writer policy");

    let journal: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .expect("journal mode");
    assert_eq!(journal.to_ascii_lowercase(), "wal");
    // Native has none of these pragmas: its connections disable automatic
    // checkpoints when they open and never trust schema-defined functions.
    if tracedecay_sql::ENGINE == tracedecay_sql::Engine::Sqlite {
        assert_eq!(pragma_i64(&connection, "wal_autocheckpoint"), 0);
        assert_eq!(pragma_i64(&connection, "synchronous"), 1);
        assert_eq!(pragma_i64(&connection, "trusted_schema"), 0);
    }
    assert_eq!(pragma_i64(&connection, "foreign_keys"), 1);
    connection
        .execute("INSERT INTO items VALUES (2)", [])
        .expect("ordinary writer DML");
    connection
        .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |_| Ok(()))
        .expect("writer-owned checkpoint observation remains authorized");
    assert!(
        connection
            .pragma_update(None, "cache_size", 1_000_i64)
            .is_err()
    );
    connection
        .execute_batch("CREATE TABLE initialized(value)")
        .expect("non-destructive writer initialization");
    assert!(connection.execute_batch("DROP TABLE initialized").is_err());
}

/// `wal_autocheckpoint = 0` means SQLite never shrinks the WAL on its own, and
/// a checkpoint resets WAL *contents* without returning the file's blocks. The
/// writer must therefore declare the retained WAL span explicitly.
#[test]
fn writer_bounds_the_retained_wal_file_to_the_soft_limit_ceiling() {
    let file = database();
    let connection = open(file.path(), ConnectionMode::Writer).expect("writer policy");

    if tracedecay_sql::ENGINE == tracedecay_sql::Engine::Native {
        // Native has no retained-size bound: a TRUNCATE checkpoint returns
        // every WAL block.
        connection
            .execute("INSERT INTO items VALUES (2)", [])
            .expect("write WAL frame");
        let wal = sidecar_path(file.path(), "-wal");
        assert!(std::fs::metadata(&wal).expect("WAL exists").len() > 0);
        connection
            .checkpoint(tracedecay_sql::CheckpointMode::Truncate)
            .expect("truncate checkpoint");
        assert_eq!(std::fs::metadata(&wal).expect("WAL kept").len(), 0);
        return;
    }
    assert_eq!(
        pragma_i64(&connection, "journal_size_limit"),
        i64::try_from(WAL_SOFT_LIMIT_BYTES).expect("soft limit fits in i64"),
        "writer must cap the retained WAL file at the configurable soft-limit ceiling"
    );
}

#[test]
fn writer_close_never_bypasses_explicit_checkpoint_policy() {
    let file = database();
    let wal = sidecar_path(file.path(), "-wal");
    let connection = open(file.path(), ConnectionMode::Writer).expect("writer policy");
    connection
        .execute("INSERT INTO items VALUES (2)", [])
        .expect("write WAL frame");
    let wal_bytes = std::fs::metadata(&wal)
        .expect("WAL exists before close")
        .len();
    assert!(wal_bytes > 0);

    drop(connection);

    assert_eq!(
        std::fs::metadata(&wal)
            .expect("close must retain uncheckpointed WAL")
            .len(),
        wal_bytes
    );
}

#[test]
fn reader_mode_is_private_query_only_and_denies_writes() {
    let file = database();
    let writer = open(file.path(), ConnectionMode::Writer).expect("prepare WAL database");
    drop(writer);
    let connection = open(file.path(), ConnectionMode::Reader).expect("reader policy");

    assert_eq!(pragma_i64(&connection, "query_only"), 1);
    assert_eq!(pragma_i64(&connection, "foreign_keys"), 1);
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM items", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(
        connection
            .execute("INSERT INTO items VALUES (2)", [])
            .is_err()
    );
}

#[test]
fn reader_authorizer_allows_integrity_diagnostics_and_denies_mutating_pragmas() {
    let file = database();
    let writer = open(file.path(), ConnectionMode::Writer).expect("prepare WAL database");
    drop(writer);
    let connection = open(file.path(), ConnectionMode::Reader).expect("reader policy");

    for pragma in [
        "PRAGMA quick_check",
        "PRAGMA quick_check(1000)",
        "PRAGMA integrity_check",
        "PRAGMA integrity_check(1000)",
    ] {
        let result = connection
            .query_row(pragma, [], |row| row.get::<_, String>(0))
            .unwrap_or_else(|error| panic!("{pragma} must be authorized: {error}"));
        assert_eq!(result, "ok", "{pragma} must report a healthy database");
    }

    for pragma in [
        "PRAGMA application_id = 1",
        "PRAGMA cache_size = 1000",
        "PRAGMA journal_mode = DELETE",
        "PRAGMA user_version = 1",
    ] {
        assert!(
            connection.execute_batch(pragma).is_err(),
            "{pragma} must remain denied"
        );
    }
}

/// Fails if any connection mode lets `ATTACH`/`DETACH` past its capability.
/// Every mode also caps attachments at zero, so only a denial at prepare
/// proves the capability, not the run-time limit, refused it.
#[test]
fn attach_and_detach_are_denied_in_every_connection_mode() {
    let file = database();
    for mode in [ConnectionMode::Writer, ConnectionMode::Reader] {
        let connection = open(file.path(), mode).expect("connection policy");
        for sql in [
            "ATTACH DATABASE ':memory:' AS denied_input",
            // SQLite reports a parameter-bound filename as an unknown action.
            "ATTACH DATABASE ?1 AS denied_input",
            "DETACH DATABASE denied_input",
        ] {
            let error = connection
                .prepare(sql)
                .expect_err("ATTACH/DETACH must be refused");
            assert_eq!(
                error.kind(),
                ErrorKind::Denied,
                "{mode:?} must deny `{sql}` at the capability, got {error:?}"
            );
        }
        let attached: Vec<String> = connection
            .prepare("PRAGMA database_list")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            attached,
            ["main"],
            "{mode:?} must not attach a denied database"
        );
    }
}

#[test]
fn limits_and_authorizer_reject_oversized_or_unsafe_sql() {
    let file = database();
    let connection = open(file.path(), ConnectionMode::Writer).expect("writer policy");

    assert!(
        connection
            .execute_batch("ATTACH DATABASE ':memory:' AS other")
            .is_err()
    );
    let oversized = format!("SELECT 1 /*{}*/", "x".repeat(1024 * 1024));
    assert!(connection.prepare(&oversized).is_err());
}

#[test]
/// Catches a reintroduced incremental-vacuum lane: a fresh writer leaves
/// auto-vacuum off and refuses to enable it or run a bounded vacuum.
fn writer_never_enables_or_runs_incremental_vacuum() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("fresh.sqlite3");
    std::fs::File::create(&path).expect("create empty database file");
    let connection = open(&path, ConnectionMode::Writer).expect("writer policy");

    for pragma in [
        "PRAGMA auto_vacuum = INCREMENTAL",
        "PRAGMA incremental_vacuum(8)",
    ] {
        assert!(
            connection.execute_batch(pragma).is_err(),
            "{pragma} must be denied"
        );
    }
    connection
        .execute_batch("CREATE TABLE items(value INTEGER); INSERT INTO items VALUES (7);")
        .expect("a writer must still accept ordinary schema and rows");
    assert_eq!(
        connection
            .query_row("SELECT value FROM items", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        7
    );
}

#[cfg(any(unix, windows))]
#[test]
fn fresh_writer_uses_a_sidecar_compatible_path() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("fresh-writer.sqlite3");
    let pinned = OpenedDatabaseFile::create_new(&path).expect("pin fresh database");
    let worker_path = pinned.writer_open_path(&path).expect("select writer path");
    let connection = open_writer(&worker_path, Some(&pinned), &path).expect("writer policy");

    connection
        .execute_batch(
            "CREATE TABLE sidecar_probe(value INTEGER);
             INSERT INTO sidecar_probe VALUES (1);",
        )
        .expect("fresh writer schema and WAL write");

    assert!(
        sidecar_path(&path, "-wal").is_file(),
        "fresh writer must create WAL beside the canonical database"
    );
}

#[test]
fn progress_cancellation_interrupts_and_is_removed_after_scope() {
    let file = database();
    let connection = open(file.path(), ConnectionMode::Writer).expect("writer policy");
    let result = connection
        .guard_scope(
            || true,
            |connection| {
                connection.query_row(
                    "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000000) SELECT sum(x) FROM n",
                    [],
                    |row| row.get::<_, i64>(0),
                )
            },
        )
        .expect("progress handler setup");
    assert!(matches!(result, Err(error) if error.kind() == ErrorKind::Interrupted));
    assert_eq!(
        connection
            .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn policy_requires_an_existing_database() {
    let directory = tempfile::tempdir().unwrap();
    let missing = Path::new(directory.path()).join("missing.db");
    assert!(
        open(&missing, ConnectionMode::Writer)
            .unwrap_err()
            .is_open_failure()
    );
}

#[test]
fn create_new_pins_and_discards_the_exact_database() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fresh.db");

    let created = OpenedDatabaseFile::create_new(&path).unwrap();
    assert!(path.is_file());
    assert_ne!(created.identity(), 0);
    created.discard_created(&path).unwrap();

    assert!(!path.exists());
}

#[test]
fn create_new_refuses_to_replace_an_existing_database() {
    let file = NamedTempFile::new().unwrap();

    assert!(matches!(
        OpenedDatabaseFile::create_new(file.path()),
        Err(OpenedDatabaseFileError::Create)
    ));
}

#[cfg(all(unix, any(target_os = "linux", target_os = "android")))]
#[test]
fn worker_open_path_stays_on_the_pinned_file_across_an_a_b_a_swap() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("identity.db");
    let retired = directory.path().join("identity.retired.db");
    let replacement = directory.path().join("identity.replacement.db");
    std::fs::write(&path, b"original").unwrap();
    std::fs::write(&replacement, b"replacement").unwrap();
    let pinned = OpenedDatabaseFile::pin(&path).unwrap();
    let retained = pinned.try_clone().unwrap();
    let worker_path = retained.worker_open_path(&path).unwrap();

    std::fs::rename(&path, &retired).unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    if tracedecay_sql::ENGINE == tracedecay_sql::Engine::Native {
        // No descriptor is pinned: closing one would drop the engine's lock.
        // The canonical path is opened and the swapped file is refused.
        assert_eq!(worker_path, path);
        assert_eq!(
            pinned.verify_current_path(&path),
            Err(OpenedDatabaseFileError::Replaced)
        );
        std::fs::rename(&path, &replacement).unwrap();
        std::fs::rename(&retired, &path).unwrap();
        pinned.verify_current_path(&path).unwrap();
        return;
    }
    assert_eq!(std::fs::read(&worker_path).unwrap(), b"original");

    std::fs::rename(&path, &replacement).unwrap();
    std::fs::rename(&retired, &path).unwrap();
    assert_eq!(std::fs::read(&worker_path).unwrap(), b"original");
    pinned.verify_current_path(&path).unwrap();
}

/// A WAL reader opens the `-shm` sidecar, so it needs the same sidecar-safe
/// pathname a writer does.
///
/// SQLite derives sidecar names by appending to the full pathname it resolved.
/// Linux `/proc/self/fd/*` is a symlink, so the real `<database>-shm` is
/// derived and the descriptor pathname is safe; every other Unix host exposes
/// `/dev/fd/*` as a non-symlink devfs entry, so SQLite would look for
/// `/dev/fd/<fd>-shm` and fail the first schema read with `SQLITE_CANTOPEN`.
/// Readers once used the raw descriptor policy and macOS CI failed 2284 tests
/// on that single divergence; pinning the two policies together is what keeps
/// them from drifting apart again on a host this suite cannot run on.
#[cfg(any(unix, windows))]
#[test]
fn reader_and_writer_open_paths_share_one_sidecar_policy() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reader-path.db");
    std::fs::File::create(&path).unwrap();
    let pinned = OpenedDatabaseFile::pin(&path).unwrap();

    let reader_path = pinned.reader_open_path(&path).unwrap();
    let writer_path = pinned.writer_open_path(&path).unwrap();
    assert_eq!(reader_path, writer_path);

    // The native engine refuses a symlinked pathname and fences the file's
    // identity itself, so it always opens the canonical path.
    #[cfg(unix)]
    if cfg!(any(target_os = "linux", target_os = "android"))
        && tracedecay_sql::ENGINE == tracedecay_sql::Engine::Sqlite
    {
        assert!(reader_path.starts_with("/proc/self/fd/"));
    } else {
        assert_eq!(reader_path, path);
    }
    #[cfg(windows)]
    assert_eq!(reader_path, path);
}

#[cfg(unix)]
#[test]
fn writer_identity_fence_rejects_a_replacement_hidden_by_path_restore() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("identity.db");
    let replacement = directory.path().join("identity.replacement.db");
    let retired = directory.path().join("identity.retired.db");
    for candidate in [&path, &replacement] {
        let connection = Connection::open(candidate).unwrap();
        connection
            .execute_batch("CREATE TABLE identity(value INTEGER);")
            .unwrap();
    }

    let pinned = OpenedDatabaseFile::pin(&path).unwrap();
    std::fs::rename(&path, &retired).unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    let connection = Connection::open(&path).unwrap();
    std::fs::rename(&path, &replacement).unwrap();
    std::fs::rename(&retired, &path).unwrap();

    assert_eq!(
        pinned.verify_connection(&connection, &path),
        Err(OpenedDatabaseFileError::Replaced)
    );
}

#[cfg(windows)]
#[test]
fn windows_pinned_file_blocks_replacement_until_authority_closes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("identity.db");
    let retired = directory.path().join("identity.retired.db");
    std::fs::write(&path, b"original").unwrap();
    let pinned = OpenedDatabaseFile::pin(&path).unwrap();
    let retained = pinned.try_clone().unwrap();

    assert_eq!(retained.writer_open_path(&path).unwrap(), path);
    // The block surfaces as ERROR_SHARING_VIOLATION (32) from the pin's
    // share mode, or ERROR_ACCESS_DENIED (5) on hosts that deny through the
    // handle instead; std's ErrorKind mapping for 32 varies by release, so
    // assert the raw contract.
    let blocked = std::fs::rename(&path, &retired).unwrap_err();
    assert!(
        matches!(blocked.raw_os_error(), Some(5 | 32)),
        "pinned replacement must be blocked while the authority is open: {blocked}"
    );
    pinned.verify_current_path(&path).unwrap();

    drop(retained);
    drop(pinned);
    std::fs::rename(&path, &retired)
        .expect("replacement must become possible after retained handles close");
}

#[cfg(windows)]
#[test]
fn windows_discard_created_removes_the_complete_sqlite_family() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fresh.db");
    let created = OpenedDatabaseFile::create_new(&path).unwrap();
    let sidecars = [
        sidecar_path(&path, "-wal"),
        sidecar_path(&path, "-shm"),
        sidecar_path(&path, "-journal"),
    ];
    for sidecar in &sidecars {
        std::fs::write(sidecar, b"sidecar").unwrap();
    }

    created.discard_created(&path).unwrap();

    assert!(!path.exists());
    assert!(sidecars.iter().all(|sidecar| !sidecar.exists()));
}

/// `sqlite_host_path` shortens a verbatim *disk* path so SQLite's win32 VFS
/// uses per-handle byte locking instead of the UNC shared-handle emulation;
/// it must keep the prefix for genuine UNC and over-long paths, which need
/// the extended-length spelling to open at all.
#[cfg(windows)]
#[test]
fn sqlite_host_path_shortens_verbatim_disk_paths_only() {
    use super::sqlite_host_path;

    let verbatim = Path::new(r"\\?\C:\data\store.db");
    assert_eq!(sqlite_host_path(verbatim), Path::new(r"C:\data\store.db"));

    let unc = Path::new(r"\\?\UNC\server\share\store.db");
    assert_eq!(sqlite_host_path(unc), unc);

    let long = PathBuf::from(format!(r"\\?\C:\{}", "d".repeat(300)));
    assert_eq!(sqlite_host_path(&long), long);

    // 255 plain chars strip (sidecars still fit); 256 keep the prefix
    // because `-wal`/`-shm` would push past the classic 259-char limit.
    let fits_with_sidecars = PathBuf::from(format!(r"\\?\C:\{}", "d".repeat(252)));
    assert_eq!(
        sqlite_host_path(&fits_with_sidecars),
        PathBuf::from(format!(r"C:\{}", "d".repeat(252)))
    );
    let sidecars_too_long = PathBuf::from(format!(r"\\?\C:\{}", "d".repeat(253)));
    assert_eq!(sqlite_host_path(&sidecars_too_long), sidecars_too_long);

    // An empty `\\` component collapses to a plain separator either way,
    // so dunce accepts the conversion; the literal spelling is preserved.
    let empty_component = Path::new(r"\\?\C:\data\\store.db");
    assert_eq!(
        sqlite_host_path(empty_component),
        Path::new(r"C:\data\\store.db")
    );

    // The classic limit is measured in UTF-16 code units, not UTF-8 bytes:
    // 150 multibyte chars are 300 bytes but 150 units, so they may strip.
    let wide_fits = PathBuf::from(format!("\\\\?\\C:\\{}", "é".repeat(150)));
    assert_eq!(
        sqlite_host_path(&wide_fits),
        PathBuf::from(format!("C:\\{}", "é".repeat(150)))
    );

    // Components whose identity would change under Win32 name parsing keep
    // the prefix: trailing dots/spaces are stripped, dot components resolve,
    // `/` is a separator, and reserved DOS-device stems get special
    // treatment. `dunce::simplified` refuses all of these and the refusal
    // is honored — refused verbatim paths stay verbatim. (An empty `\\`
    // component collapses to a plain separator either way, so dunce accepts
    // that one — covered above.)
    for raw in [
        r"\\?\C:\data\store.",
        r"\\?\C:\data\store ",
        r"\\?\C:\data\.\store.db",
        r"\\?\C:\data\..\store.db",
        r"\\?\C:\data/store.db",
        r"\\?\C:\data\CON.db",
        r"\\?\C:\data\con.db",
        r"\\?\C:\data\NUL",
        r"\\?\C:\data\com1.db",
        r"\\?\C:\data\lpt9.log",
        "\\\\?\\C:\\data\\COM\u{b9}.db",
        "\\\\?\\C:\\data\\lpt\u{b3}",
    ] {
        let path = Path::new(raw);
        assert_eq!(sqlite_host_path(path), path, "{raw}");
    }

    let plain = Path::new(r"C:\data\store.db");
    assert_eq!(sqlite_host_path(plain), plain);

    let relative = Path::new("data\\store.db");
    assert_eq!(sqlite_host_path(relative), relative);
}

/// On non-Windows hosts the `\\?\` byte sequence is an ordinary relative
/// filename — a database path spelled that way must reach SQLite unchanged.
#[cfg(not(windows))]
#[test]
fn sqlite_host_path_keeps_literal_windows_prefix_names() {
    use super::sqlite_host_path;

    let name = PathBuf::from(r"\\?\C:\data\store.db");
    assert_eq!(sqlite_host_path(&name), name);
}

/// Two real directories can coexist whose names differ only by a trailing
/// dot, and stripping `\\?\` from the dotted spelling would change which
/// name the OS sees — so `dunce::simplified` refuses and the verbatim
/// path is passed through untouched. SQLite itself still canonicalizes
/// the name and aliases `store.` to `store`: a separate SQLite
/// limitation, not a reason to normalize further here. What the
/// production boundary must guarantee is that a refused verbatim
/// spelling keeps its prefix and still opens.
#[cfg(windows)]
#[test]
fn dot_suffixed_directory_retains_verbatim_and_opens() {
    use super::sqlite_host_path;

    let temp = tempfile::tempdir().unwrap();
    let canonical = std::fs::canonicalize(temp.path()).unwrap();
    assert!(
        canonical.to_str().unwrap().starts_with(r"\\?\"),
        "test requires a verbatim tempdir path"
    );

    // Both creations must go through verbatim spelling: a plain `store.`
    // would collapse onto `store`.
    let dotted = canonical.join("store.");
    let plain = canonical.join("store");
    std::fs::create_dir(&dotted).unwrap();
    std::fs::create_dir(&plain).unwrap();
    assert!(dotted.is_dir() && plain.is_dir());

    // The refused verbatim spelling is passed through untouched.
    let dotted_db = dotted.join("db.sqlite");
    assert_eq!(sqlite_host_path(&dotted_db), dotted_db);

    // Seed the `store` database (production open is READ_WRITE-only).
    let db = Connection::open(plain.join("db.sqlite")).expect("create database");
    db.execute_batch(
        "CREATE TABLE sentinel(v TEXT); INSERT INTO sentinel VALUES('canonical-store');",
    )
    .unwrap();
    drop(db);

    // The verbatim open succeeds through the production boundary. SQLite
    // resolves the name to `store`'s database (its own canonicalization
    // aliases the dotted directory — a SQLite limitation, not this
    // conversion's identity claim) — proven by the sentinel row; a
    // separate `store.` database is unreachable through SQLite either way.
    let reader = open(&dotted_db, ConnectionMode::Writer).expect("open via dotted verbatim path");
    let value: String = reader
        .query_row("SELECT v FROM sentinel", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, "canonical-store");
}

/// A database opened through a verbatim `\\?\` path is the same file, the
/// same WAL journal, and the same lock domain as its plain spelling — the
/// normalization only changes the pathname the VFS sees.
#[cfg(windows)]
#[test]
fn verbatim_disk_path_opens_the_same_wal_database() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("verbatim.db");
    std::fs::write(&path, b"").unwrap();
    // `canonicalize` yields the `\\?\` extended-length spelling that store
    // paths carry into the engine on Windows.
    let verbatim = path.canonicalize().unwrap();
    assert!(verbatim.to_str().unwrap().starts_with(r"\\?\"));

    let writer = open(&verbatim, ConnectionMode::Writer).expect("verbatim writer open");
    writer
        .execute_batch("CREATE TABLE items(value INTEGER); INSERT INTO items VALUES (1);")
        .unwrap();
    let journal_mode: String = writer
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .expect("read pragma");
    assert_eq!(journal_mode, "wal");

    let reader = open(&path, ConnectionMode::Reader).expect("plain reader open");
    assert_eq!(
        reader.query_row("SELECT value FROM items", [], |row| row.get::<_, i64>(0)),
        Ok(1)
    );
    drop(reader);
    drop(writer);
}
