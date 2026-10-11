//! Query-only rusqlite readers for SQLite databases that other agent hosts
//! own. They stay on rusqlite whichever engine owns TraceDecay's stores.

use std::{
    fmt, io,
    path::{Path, PathBuf},
    time::Duration,
};

use rusqlite::{
    Connection, OpenFlags,
    config::DbConfig,
    hooks::{AuthAction, AuthContext, Authorization},
    limits::Limit,
};
use tracedecay_rusqlite_runtime::{OpenedDatabaseFile, OpenedDatabaseFileError, sqlite_host_path};

/// Host scans repeat a small catalog of statements; rusqlite's default of
/// sixteen cached statements is too small for it.
const PREPARED_STATEMENT_CACHE_CAPACITY: usize = 128;

#[derive(Debug)]
pub(crate) struct ReaderPolicyError {
    stage: &'static str,
    source: rusqlite::Error,
}

impl fmt::Display for ReaderPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "SQLite reader policy failed at {}: {}",
            self.stage, self.source
        )
    }
}

impl std::error::Error for ReaderPolicyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[derive(Debug)]
pub(crate) enum VerifiedReaderError {
    Resolve(io::Error),
    Identity(OpenedDatabaseFileError),
    Policy(ReaderPolicyError),
}

impl fmt::Display for VerifiedReaderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Resolve(error) => {
                write!(formatter, "could not resolve SQLite reader path: {error}")
            }
            Self::Identity(error) => write!(formatter, "SQLite reader identity failed: {error}"),
            Self::Policy(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for VerifiedReaderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Resolve(error) => Some(error),
            Self::Identity(error) => Some(error),
            Self::Policy(error) => Some(error),
        }
    }
}

/// A reader bound to the physical file SQLite actually opened, not to a later
/// pathname observation.
pub(crate) struct VerifiedReader {
    _opened_file: OpenedDatabaseFile,
    connection: Connection,
    canonical_path: PathBuf,
    file_identity: u64,
}

impl VerifiedReader {
    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }

    pub(crate) const fn file_identity(&self) -> u64 {
        self.file_identity
    }

    pub(crate) fn into_parts(self) -> (Connection, PathBuf, u64) {
        (self.connection, self.canonical_path, self.file_identity)
    }
}

/// Opens a live database in a retained read transaction. Dropping the reader
/// releases the snapshot.
pub(crate) fn open_verified_read_snapshot(
    path: &Path,
) -> Result<VerifiedReader, VerifiedReaderError> {
    open_verified_reader(
        path,
        |path| {
            let connection = Connection::open_with_flags(
                sqlite_host_path(path),
                OpenFlags::SQLITE_OPEN_READ_ONLY
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX
                    | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE,
            )
            .map_err(|source| policy("open", source))?;
            apply_reader_policy(&connection)?;
            connection
                .execute_batch("BEGIN DEFERRED")
                .map_err(|source| policy("begin read snapshot", source))?;
            connection
                .query_row("SELECT count(*) FROM sqlite_schema", [], |_| Ok(()))
                .map_err(|source| policy("pin read snapshot", source))?;
            Ok(connection)
        },
        || {},
        || {},
    )
}

/// Opens `file:…?immutable=1&mode=ro`, which takes no lock and never creates a
/// WAL or SHM sidecar. The caller owns the policy for a non-empty WAL: reject
/// it when a complete current snapshot is mandatory, or accept eventual
/// main-file visibility for best-effort ingestion.
pub(crate) fn open_immutable_reader(path: &Path) -> Result<Connection, ReaderPolicyError> {
    let connection = Connection::open_with_flags(
        immutable_uri(&sqlite_host_path(path))?,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE,
    )
    .map_err(|source| policy("open", source))?;
    apply_reader_policy(&connection)?;
    Ok(connection)
}

pub(crate) fn open_verified_immutable_reader(
    path: &Path,
) -> Result<VerifiedReader, VerifiedReaderError> {
    open_verified_reader(path, open_immutable_reader, || {}, || {})
}

fn open_verified_reader(
    path: &Path,
    open_reader: impl FnOnce(&Path) -> Result<Connection, ReaderPolicyError>,
    after_pin: impl FnOnce(),
    after_open: impl FnOnce(),
) -> Result<VerifiedReader, VerifiedReaderError> {
    let canonical_path = path.canonicalize().map_err(VerifiedReaderError::Resolve)?;
    let pinned = OpenedDatabaseFile::pin(&canonical_path).map_err(VerifiedReaderError::Identity)?;
    let open_path = pinned
        .reader_open_path(&canonical_path)
        .map_err(VerifiedReaderError::Identity)?;
    after_pin();
    let connection = open_reader(&open_path).map_err(VerifiedReaderError::Policy)?;
    after_open();
    verify_connection(&pinned, &connection, &canonical_path)
        .map_err(VerifiedReaderError::Identity)?;
    Ok(VerifiedReader {
        connection,
        canonical_path,
        file_identity: pinned.identity(),
        _opened_file: pinned,
    })
}

/// The pathname must still name the pinned file, and SQLite's own handle must
/// not report that it moved; the pathname is checked on both sides of the
/// file-control call so a swap during it is still seen.
fn verify_connection(
    pinned: &OpenedDatabaseFile,
    _connection: &Connection,
    canonical_path: &Path,
) -> Result<(), OpenedDatabaseFileError> {
    pinned.verify_current_path(canonical_path)?;
    #[cfg(unix)]
    {
        if has_moved(_connection)? {
            return Err(OpenedDatabaseFileError::Replaced);
        }
        pinned.verify_current_path(canonical_path)?;
    }
    Ok(())
}

#[cfg(unix)]
fn has_moved(connection: &Connection) -> Result<bool, OpenedDatabaseFileError> {
    let main = std::ffi::CString::new("main").expect("static database name has no NUL");
    let mut moved = 0_i32;
    // SAFETY: `connection` owns a live SQLite handle, `main` is NUL-terminated,
    // and `moved` is the writable int SQLITE_FCNTL_HAS_MOVED requires.
    let code = unsafe {
        rusqlite::ffi::sqlite3_file_control(
            connection.handle(),
            main.as_ptr(),
            rusqlite::ffi::SQLITE_FCNTL_HAS_MOVED,
            (&raw mut moved).cast(),
        )
    };
    match code {
        rusqlite::ffi::SQLITE_OK => Ok(moved != 0),
        // A VFS that predates HAS_MOVED cannot answer; the pinned identity
        // check stays authoritative.
        rusqlite::ffi::SQLITE_NOTFOUND => Ok(false),
        _ => Err(OpenedDatabaseFileError::Inspect),
    }
}

fn immutable_uri(path: &Path) -> Result<String, ReaderPolicyError> {
    #[cfg(unix)]
    let raw = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes()
    };
    #[cfg(not(unix))]
    let raw = path
        .to_str()
        .ok_or_else(|| policy("immutable uri", rusqlite::Error::InvalidPath(path.into())))?
        .as_bytes();

    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(raw.len().saturating_mul(3).saturating_add(24));
    for byte in raw {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                encoded.push(*byte as char)
            }
            _ => {
                encoded.push('%');
                encoded.push(HEX[(byte >> 4) as usize] as char);
                encoded.push(HEX[(byte & 0x0f) as usize] as char);
            }
        }
    }
    Ok(format!("file:{encoded}?immutable=1&mode=ro"))
}

/// Query-only, untrusted-schema, no-wait, bounded, and closed to writes,
/// attachments and configuration pragmas. Closing never checkpoints, so a
/// reader cannot rewrite the host's WAL.
fn apply_reader_policy(connection: &Connection) -> Result<(), ReaderPolicyError> {
    connection
        .busy_timeout(Duration::ZERO)
        .map_err(|source| policy("busy timeout", source))?;
    connection
        .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
        .map_err(|source| policy("checkpoint-on-close", source))?;
    connection
        .pragma_update(None, "trusted_schema", false)
        .map_err(|source| policy("trusted schema", source))?;
    connection
        .pragma_update(None, "query_only", true)
        .map_err(|source| policy("query-only reader", source))?;
    for (limit, value) in [
        (Limit::SQLITE_LIMIT_LENGTH, 64 * 1024 * 1024),
        (Limit::SQLITE_LIMIT_SQL_LENGTH, 1024 * 1024),
        (Limit::SQLITE_LIMIT_COLUMN, 2_000),
        (Limit::SQLITE_LIMIT_EXPR_DEPTH, 100),
        (Limit::SQLITE_LIMIT_COMPOUND_SELECT, 100),
        (Limit::SQLITE_LIMIT_VDBE_OP, 25_000_000),
        (Limit::SQLITE_LIMIT_FUNCTION_ARG, 100),
        (Limit::SQLITE_LIMIT_ATTACHED, 0),
        (Limit::SQLITE_LIMIT_LIKE_PATTERN_LENGTH, 50_000),
        (Limit::SQLITE_LIMIT_VARIABLE_NUMBER, 32_766),
        (Limit::SQLITE_LIMIT_TRIGGER_DEPTH, 32),
        (Limit::SQLITE_LIMIT_WORKER_THREADS, 0),
    ] {
        connection
            .set_limit(limit, value)
            .map_err(|source| policy("reader limits", source))?;
    }
    connection.set_prepared_statement_cache_capacity(PREPARED_STATEMENT_CACHE_CAPACITY);
    connection
        .authorizer(Some(authorize_reader))
        .map_err(|source| policy("authorizer", source))
}

/// Schema-introspection and integrity pragmas that cannot mutate the
/// database, file, or connection configuration.
fn is_read_only_introspection_pragma(name: &str) -> bool {
    [
        "collation_list",
        "database_list",
        "foreign_key_check",
        "foreign_key_list",
        "function_list",
        "index_info",
        "index_list",
        "index_xinfo",
        "integrity_check",
        "module_list",
        "pragma_list",
        "quick_check",
        "table_info",
        "table_list",
        "table_xinfo",
    ]
    .iter()
    .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

fn authorize_reader(context: AuthContext<'_>) -> Authorization {
    let denied = matches!(
        context.action,
        AuthAction::Attach { .. }
            | AuthAction::Detach { .. }
            | AuthAction::CreateIndex { .. }
            | AuthAction::CreateTable { .. }
            | AuthAction::CreateTempIndex { .. }
            | AuthAction::CreateTempTable { .. }
            | AuthAction::CreateTempTrigger { .. }
            | AuthAction::CreateTempView { .. }
            | AuthAction::CreateTrigger { .. }
            | AuthAction::CreateView { .. }
            | AuthAction::DropIndex { .. }
            | AuthAction::DropTable { .. }
            | AuthAction::DropTempIndex { .. }
            | AuthAction::DropTempTable { .. }
            | AuthAction::DropTempTrigger { .. }
            | AuthAction::DropTempView { .. }
            | AuthAction::DropTrigger { .. }
            | AuthAction::DropView { .. }
            | AuthAction::AlterTable { .. }
            | AuthAction::Analyze { .. }
            | AuthAction::CreateVtable { .. }
            | AuthAction::DropVtable { .. }
            | AuthAction::Insert { .. }
            | AuthAction::Update { .. }
            | AuthAction::Delete { .. }
            | AuthAction::Unknown { .. }
    ) || matches!(context.action, AuthAction::Function { function_name } if function_name.eq_ignore_ascii_case("load_extension"))
        || matches!(
            context.action,
            AuthAction::Pragma { pragma_name, pragma_value: Some(_) }
            if !is_read_only_introspection_pragma(pragma_name)
        );
    if denied {
        Authorization::Deny
    } else {
        Authorization::Allow
    }
}

fn policy(stage: &'static str, source: rusqlite::Error) -> ReaderPolicyError {
    ReaderPolicyError { stage, source }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn database(directory: &Path) -> PathBuf {
        let path = directory.join("host.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE items(value INTEGER); INSERT INTO items VALUES (1);")
            .unwrap();
        path
    }

    fn pragma_i64(connection: &Connection, name: &str) -> i64 {
        connection
            .pragma_query_value(None, name, |row| row.get(0))
            .unwrap()
    }

    fn listing(directory: &Path) -> Vec<(String, Vec<u8>)> {
        let mut entries = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    std::fs::read(entry.path()).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        entries.sort();
        entries
    }

    /// Catches a reader that writes beside a host's database or leaves it
    /// writable, attachable or reconfigurable.
    #[test]
    fn immutable_reader_applies_reader_policy_without_touching_the_host_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = database(directory.path());
        let before = listing(directory.path());

        let connection = open_immutable_reader(&path).expect("immutable reader policy");
        assert_eq!(pragma_i64(&connection, "query_only"), 1);
        assert_eq!(pragma_i64(&connection, "trusted_schema"), 0);
        assert_eq!(pragma_i64(&connection, "busy_timeout"), 0);
        assert_eq!(connection.limit(Limit::SQLITE_LIMIT_ATTACHED).unwrap(), 0);
        for denied in [
            "INSERT INTO items VALUES (2)",
            "CREATE TEMP TABLE scratch(value)",
            "ATTACH DATABASE ':memory:' AS other",
            "PRAGMA query_only = 0",
        ] {
            assert!(
                connection.execute_batch(denied).is_err(),
                "{denied} must be denied"
            );
        }
        let value: i64 = connection
            .query_row("SELECT value FROM items", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, 1);
        drop(connection);

        assert_eq!(listing(directory.path()), before);
    }

    #[cfg(unix)]
    #[test]
    fn verified_immutable_reader_never_binds_transient_b_to_restored_a_identity() {
        use std::sync::{Arc, Barrier};

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("identity.db");
        let replacement = directory.path().join("identity.replacement.db");
        let retired = directory.path().join("identity.retired.db");
        for (candidate, marker) in [(&path, "a"), (&replacement, "b")] {
            Connection::open(candidate)
                .unwrap()
                .execute_batch(&format!(
                    "CREATE TABLE identity(marker TEXT);\
                     INSERT INTO identity VALUES ('{marker}');"
                ))
                .unwrap();
        }
        let expected_a = OpenedDatabaseFile::pin(&path).unwrap().identity();

        let [pinned, swapped, opened, restored] =
            std::array::from_fn(|_| Arc::new(Barrier::new(2)));
        let replacer = {
            let (path, replacement, retired) = (path.clone(), replacement.clone(), retired);
            let (pinned, swapped, opened, restored) = (
                Arc::clone(&pinned),
                Arc::clone(&swapped),
                Arc::clone(&opened),
                Arc::clone(&restored),
            );
            std::thread::spawn(move || {
                pinned.wait();
                std::fs::rename(&path, &retired).unwrap();
                std::fs::rename(&replacement, &path).unwrap();
                swapped.wait();
                opened.wait();
                std::fs::rename(&path, &replacement).unwrap();
                std::fs::rename(&retired, &path).unwrap();
                restored.wait();
            })
        };

        let result = open_verified_reader(
            &path,
            open_immutable_reader,
            || {
                pinned.wait();
                swapped.wait();
            },
            || {
                opened.wait();
                restored.wait();
            },
        );
        replacer.join().unwrap();

        match result {
            Ok(reader) => {
                assert_eq!(reader.file_identity(), expected_a);
                let marker: String = reader
                    .connection()
                    .query_row("SELECT marker FROM identity", [], |row| row.get(0))
                    .unwrap();
                assert_eq!(marker, "a");
            }
            Err(VerifiedReaderError::Identity(OpenedDatabaseFileError::Replaced)) => {}
            Err(error) => panic!("unexpected verified immutable open failure: {error}"),
        }
    }

    /// Catches a snapshot that follows later commits, admits writes, or keeps
    /// the host's checkpoint blocked after it is dropped.
    #[test]
    fn verified_live_snapshot_retains_committed_wal_view_and_denies_writes() {
        let directory = tempfile::tempdir().unwrap();
        let path = database(directory.path());
        let writer = Connection::open(&path).unwrap();
        writer.pragma_update(None, "journal_mode", "WAL").unwrap();
        let read = |reader: &VerifiedReader| -> i64 {
            reader
                .connection()
                .query_row("SELECT value FROM items", [], |row| row.get(0))
                .unwrap()
        };

        let reader = open_verified_read_snapshot(&path).unwrap();
        writer.execute("UPDATE items SET value = 2", []).unwrap();
        assert_eq!(read(&reader), 1);
        assert!(
            reader
                .connection()
                .execute("UPDATE items SET value = 3", [])
                .is_err()
        );
        drop(reader);
        let reader = open_verified_read_snapshot(&path).unwrap();
        assert_eq!(read(&reader), 2);
        drop(reader);

        let busy: i64 = writer
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            busy, 0,
            "dropping the reader must release its checkpoint lock"
        );
    }
}
