//! Side-effect-free logical inspection of `SQLite` database families.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use fs2::FileExt;
use rusqlite::{Connection, OpenFlags, params_from_iter, types::ValueRef};
use sha2::{Digest, Sha256};

use crate::db::engine::{
    Error as EngineError, Executor, IntoParams, QueryExecutor, Row, Rows, Value,
};

static NEXT_SNAPSHOT: AtomicU64 = AtomicU64::new(0);

pub async fn backup_live_sqlite_database(source: &Path, destination: &Path) -> io::Result<()> {
    let source = source.to_path_buf();
    let destination = destination.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let source = Connection::open_with_flags(&source, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(io::Error::other)?;
        let mut destination = Connection::open_with_flags(
            &destination,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .map_err(io::Error::other)?;
        let backup =
            rusqlite::backup::Backup::new(&source, &mut destination).map_err(io::Error::other)?;
        backup
            .run_to_completion(128, Duration::from_millis(1), None)
            .map_err(io::Error::other)
    })
    .await
    .map_err(|error| io::Error::other(format!("live SQLite backup task failed: {error}")))?
}

pub struct SnapshotConnection {
    connection: Arc<Mutex<Connection>>,
    #[cfg_attr(not(test), allow(dead_code))]
    interrupt: rusqlite::InterruptHandle,
}

impl SnapshotConnection {
    fn open(path: &Path, flags: OpenFlags) -> crate::db::engine::Result<Self> {
        let connection = Connection::open_with_flags(path, flags)
            .map_err(|error| snapshot_sqlite_error("open snapshot", error))?;
        let interrupt = connection.get_interrupt_handle();
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            interrupt,
        })
    }

    pub async fn execute<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<u64>
    where
        P: IntoParams,
    {
        Executor::execute(self, sql, params).await
    }

    pub async fn query<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<Rows>
    where
        P: IntoParams,
    {
        QueryExecutor::query(self, sql, params).await
    }

    pub async fn execute_batch(&self, sql: &str) -> crate::db::engine::Result<()> {
        Executor::execute_batch(self, sql).await
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn interrupt(&self) {
        self.interrupt.interrupt();
    }
}

impl QueryExecutor for SnapshotConnection {
    async fn query<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<Rows>
    where
        P: IntoParams,
    {
        let params = params.into_params()?;
        let connection = Arc::clone(&self.connection);
        let sql = sql.to_owned();
        tokio::task::spawn_blocking(move || {
            let connection = connection
                .lock()
                .map_err(|_| EngineError::Runtime("snapshot connection lock poisoned".into()))?;
            let mut statement = connection
                .prepare(&sql)
                .map_err(|error| snapshot_sqlite_error("prepare snapshot query", error))?;
            let columns = statement.column_count();
            let params = params.into_iter().map(engine_value_to_rusqlite);
            let mut rows = statement
                .query(params_from_iter(params))
                .map_err(|error| snapshot_sqlite_error("query snapshot", error))?;
            let mut collected = Vec::new();
            while let Some(row) = rows
                .next()
                .map_err(|error| snapshot_sqlite_error("read snapshot row", error))?
            {
                let values = (0..columns)
                    .map(|column| {
                        row.get_ref(column)
                            .map_err(|error| snapshot_sqlite_error("read snapshot value", error))
                            .and_then(snapshot_value)
                    })
                    .collect::<crate::db::engine::Result<Vec<_>>>()?;
                collected.push(Row::from_values(values));
            }
            Ok(Rows::from_rows(collected))
        })
        .await
        .map_err(|error| EngineError::Runtime(format!("snapshot query task failed: {error}")))?
    }
}

impl Executor for SnapshotConnection {
    async fn execute<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<u64>
    where
        P: IntoParams,
    {
        let params = params.into_params()?;
        let connection = Arc::clone(&self.connection);
        let sql = sql.to_owned();
        tokio::task::spawn_blocking(move || {
            let connection = connection
                .lock()
                .map_err(|_| EngineError::Runtime("snapshot connection lock poisoned".into()))?;
            let changed = connection
                .execute(
                    &sql,
                    params_from_iter(params.into_iter().map(engine_value_to_rusqlite)),
                )
                .map_err(|error| snapshot_sqlite_error("execute snapshot statement", error))?;
            u64::try_from(changed)
                .map_err(|_| EngineError::Runtime("snapshot row count overflow".into()))
        })
        .await
        .map_err(|error| EngineError::Runtime(format!("snapshot execute task failed: {error}")))?
    }

    async fn execute_batch(&self, sql: &str) -> crate::db::engine::Result<()> {
        let connection = Arc::clone(&self.connection);
        let sql = sql.to_owned();
        tokio::task::spawn_blocking(move || {
            connection
                .lock()
                .map_err(|_| EngineError::Runtime("snapshot connection lock poisoned".into()))?
                .execute_batch(&sql)
                .map_err(|error| snapshot_sqlite_error("execute snapshot batch", error))
        })
        .await
        .map_err(|error| EngineError::Runtime(format!("snapshot batch task failed: {error}")))?
    }
}

fn engine_value_to_rusqlite(value: Value) -> rusqlite::types::Value {
    match value {
        Value::Null => rusqlite::types::Value::Null,
        Value::Integer(value) => rusqlite::types::Value::Integer(value),
        Value::Real(value) => rusqlite::types::Value::Real(value),
        Value::Text(value) => rusqlite::types::Value::Text(value),
        Value::Blob(value) => rusqlite::types::Value::Blob(value),
    }
}

fn snapshot_value(value: ValueRef<'_>) -> crate::db::engine::Result<Value> {
    Ok(match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(value) => Value::Integer(value),
        ValueRef::Real(value) => Value::Real(value),
        ValueRef::Text(value) => Value::Text(
            std::str::from_utf8(value)
                .map_err(|error| EngineError::Runtime(format!("invalid snapshot UTF-8: {error}")))?
                .to_owned(),
        ),
        ValueRef::Blob(value) => Value::Blob(value.to_vec()),
    })
}

fn snapshot_sqlite_error(operation: &'static str, error: rusqlite::Error) -> EngineError {
    match error {
        rusqlite::Error::SqliteFailure(code, message) => EngineError::Sqlite {
            operation,
            code: Some(code.extended_code & 0xff),
            extended_code: Some(code.extended_code),
            message: message.unwrap_or_else(|| code.to_string()),
        },
        error => EngineError::Sqlite {
            operation,
            code: None,
            extended_code: None,
            message: error.to_string(),
        },
    }
}

pub struct SnapshotDatabase {
    connection: SnapshotConnection,
    source: PathBuf,
    source_state: Vec<FileState>,
    /// The `file:...` URI used to ATTACH this snapshot. Percent-encoded and
    /// carrying `mode=ro`/`immutable=1`, so it is never a valid filesystem
    /// path — use `identity_path` for anything that touches the filesystem.
    path: PathBuf,
    /// The real on-disk file this snapshot reads: the untouched source in
    /// direct-immutable mode, or the scratch copy in copy mode.
    identity_path: PathBuf,
    _scratch: Option<Arc<ScratchDirectory>>,
    _authority: crate::db::DatabaseAuthority,
    #[cfg(any(test, feature = "test-helpers"))]
    copied_bytes: u64,
}

impl SnapshotDatabase {
    pub fn connection(&self) -> &SnapshotConnection {
        &self.connection
    }

    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn attach_token(&self) -> io::Result<SnapshotAttachToken<'_>> {
        let file_identity = crate::db::sqlite_generation_identity(&self.identity_path)
            .map_err(|_| io::Error::other("could not identify immutable SQLite snapshot"))?;
        Ok(SnapshotAttachToken {
            snapshot: self,
            file_identity,
        })
    }

    pub fn validate_source(&self) -> io::Result<()> {
        let current = family_state(&self.source)?;
        if durable_family_state(&self.source, &current)
            == durable_family_state(&self.source, &self.source_state)
        {
            return Ok(());
        }
        Err(io::Error::other(format!(
            "SQLite database family '{}' changed after its read snapshot",
            self.source.display()
        )))
    }

    pub fn source_generation(&self) -> SourceGeneration {
        SourceGeneration {
            source: self.source.clone(),
            states: self.source_state.clone(),
        }
    }

    /// Writes this frozen logical snapshot to one standalone `SQLite` file.
    ///
    /// The backup reads only the already-captured immutable/copy connection;
    /// it never opens the live source authority.
    pub async fn backup_to(&self, destination: &Path) -> io::Result<()> {
        let source = Arc::clone(&self.connection.connection);
        let destination = destination.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let source = source
                .lock()
                .map_err(|_| io::Error::other("snapshot connection lock poisoned"))?;
            let mut destination_connection = Connection::open_with_flags(
                &destination,
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
            )
            .map_err(io::Error::other)?;
            let backup = rusqlite::backup::Backup::new(&source, &mut destination_connection)
                .map_err(io::Error::other)?;
            backup
                .run_to_completion(128, Duration::from_millis(1), None)
                .map_err(io::Error::other)
        })
        .await
        .map_err(|error| io::Error::other(format!("snapshot backup task failed: {error}")))?
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub fn copied_bytes(&self) -> u64 {
        self.copied_bytes
    }
}

pub struct SnapshotAttachToken<'snapshot> {
    snapshot: &'snapshot SnapshotDatabase,
    file_identity: u64,
}

impl SnapshotAttachToken<'_> {
    pub fn verified_path(&self) -> io::Result<&Path> {
        self.snapshot.validate_source()?;
        let current = crate::db::sqlite_generation_identity(&self.snapshot.identity_path)
            .map_err(|_| io::Error::other("could not re-identify immutable SQLite snapshot"))?;
        if current != self.file_identity {
            return Err(io::Error::other(
                "immutable SQLite snapshot path was replaced before ATTACH",
            ));
        }
        for suffix in ["-wal", "-shm"] {
            let mut sidecar = self.snapshot.identity_path.as_os_str().to_os_string();
            sidecar.push(suffix);
            if PathBuf::from(sidecar).exists() {
                return Err(io::Error::other(
                    "immutable SQLite snapshot has live WAL/SHM sidecars",
                ));
            }
        }
        Ok(&self.snapshot.path)
    }
}

#[derive(Debug, Clone)]
pub struct SourceGeneration {
    source: PathBuf,
    states: Vec<FileState>,
}

impl SourceGeneration {
    pub fn validate(&self) -> io::Result<()> {
        let current = family_state(&self.source)?;
        if durable_family_state(&self.source, &current)
            == durable_family_state(&self.source, &self.states)
        {
            return Ok(());
        }
        Err(io::Error::other(format!(
            "SQLite database family '{}' changed after inspection",
            self.source.display()
        )))
    }
}

pub struct SnapshotSet {
    databases: BTreeMap<PathBuf, SnapshotDatabase>,
    copied_bytes: u64,
    #[allow(dead_code)]
    scratch: Arc<ScratchDirectory>,
}

impl SnapshotSet {
    #[cfg(any(test, feature = "test-helpers"))]
    pub async fn capture(paths: &[PathBuf]) -> io::Result<Self> {
        let root = default_scratch_root(paths)?;
        Self::capture_in(paths, &root).await
    }

    pub async fn capture_in(paths: &[PathBuf], root: &Path) -> io::Result<Self> {
        let scratch = Arc::new(create_scratch_directory(root, expected_owner(paths)?)?);
        let mut unique = paths.to_vec();
        unique.sort();
        unique.dedup();
        let mut prepared = Vec::new();
        let mut copied_bytes = 0_u64;
        for (index, path) in unique.into_iter().enumerate() {
            let snapshot = prepare_one(&path, &scratch, index)?;
            copied_bytes = copied_bytes.saturating_add(snapshot.copy_bytes);
            prepared.push(snapshot);
        }
        let available = fs2::available_space(&scratch.path)?;
        if copied_bytes > available {
            return Err(io::Error::other(format!(
                "insufficient scratch space for SQLite read snapshots: required {copied_bytes} bytes, available {available} bytes at '{}'",
                scratch.path.display()
            )));
        }
        let mut databases = BTreeMap::new();
        for snapshot in prepared {
            let source = snapshot.source.clone();
            let database = finish_one(snapshot, Arc::clone(&scratch)).await?;
            databases.insert(source, database);
        }
        Ok(Self {
            databases,
            copied_bytes,
            scratch,
        })
    }

    pub fn get(&self, path: &Path) -> io::Result<&SnapshotDatabase> {
        self.databases.get(path).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("no frozen SQLite snapshot for '{}'", path.display()),
            )
        })
    }

    pub fn validate_sources_unchanged(&self) -> io::Result<()> {
        for database in self.databases.values() {
            database.validate_source()?;
        }
        Ok(())
    }

    pub fn copied_bytes(&self) -> u64 {
        self.copied_bytes
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub fn database_count(&self) -> usize {
        self.databases.len()
    }
}

struct PreparedSnapshot {
    source: PathBuf,
    source_state: Vec<FileState>,
    target: PathBuf,
    mode: SnapshotMode,
    copy_bytes: u64,
    authority: crate::db::DatabaseAuthority,
}

#[derive(Clone, Copy)]
enum SnapshotMode {
    #[cfg_attr(windows, allow(dead_code))]
    DirectImmutable,
    Reflink,
    Copy,
}

struct ScratchDirectory {
    path: PathBuf,
    owner_lock: Option<File>,
}

impl Drop for ScratchDirectory {
    fn drop(&mut self) {
        drop(self.owner_lock.take());
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileState {
    path: PathBuf,
    bytes: u64,
    modified: SystemTime,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
    #[cfg(unix)]
    links: u64,
}

/// Opens one source family without mutating it. Checkpointed DBs are read
/// directly through `SQLite` immutable mode. WAL-backed DBs are reflinked when
/// supported, then fall back to one full copy with WAL/SHM copied alongside.
#[cfg(any(test, feature = "test-helpers"))]
pub async fn open(path: &Path) -> io::Result<SnapshotDatabase> {
    let mut snapshots = SnapshotSet::capture(&[path.to_path_buf()]).await?;
    snapshots.databases.remove(path).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("no frozen SQLite snapshot for '{}'", path.display()),
        )
    })
}

pub async fn open_in(path: &Path, root: &Path) -> io::Result<SnapshotDatabase> {
    let mut snapshots = SnapshotSet::capture_in(&[path.to_path_buf()], root).await?;
    snapshots.databases.remove(path).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("no frozen SQLite snapshot for '{}'", path.display()),
        )
    })
}

/// Inspects a checkpointed, offline database through the canonical immutable
/// snapshot boundary. This is intentionally purpose-bound: callers cannot
/// obtain a connection or issue arbitrary SQL.
pub fn checkpointed_database_has_any_rows(path: &Path, tables: &[&str]) -> io::Result<bool> {
    let mut has_rows = false;
    for table in tables {
        if table.is_empty()
            || !table
                .bytes()
                .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid SQLite table identifier '{table}'"),
            ));
        }
    }
    for suffix in ["-wal", "-shm"] {
        let sidecar = with_suffix(path, suffix);
        if fs::metadata(&sidecar).is_ok_and(|metadata| metadata.len() > 0) {
            return Err(io::Error::other(format!(
                "checkpointed SQLite inspection refused live sidecar '{}'",
                sidecar.display()
            )));
        }
    }

    let _authority = crate::db::DatabaseAuthority::for_runtime(
        path,
        "inspect checkpointed SQLite family for offline maintenance",
    )
    .map_err(io::Error::other)?;
    let before = family_state(path)?;
    let uri = PathBuf::from(immutable_uri(path)?);
    let snapshot = SnapshotConnection::open(
        &uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(io::Error::other)?;
    let connection = snapshot
        .connection
        .lock()
        .map_err(|_| io::Error::other("snapshot connection lock poisoned"))?;
    for table in tables {
        let exists = connection
            .query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1
                 )",
                [table],
                |row| row.get::<_, bool>(0),
            )
            .map_err(io::Error::other)?;
        if !exists {
            continue;
        }
        let sql = format!("SELECT EXISTS(SELECT 1 FROM \"{table}\" LIMIT 1)");
        if connection
            .query_row(&sql, [], |row| row.get::<_, bool>(0))
            .map_err(io::Error::other)?
        {
            has_rows = true;
            break;
        }
    }
    drop(connection);
    if family_state(path)? != before {
        return Err(changed_during_snapshot(path));
    }
    Ok(has_rows)
}

pub fn family_fingerprint(path: &Path) -> io::Result<String> {
    use std::io::Read;

    let _authority = crate::db::DatabaseAuthority::for_runtime(
        path,
        "fingerprint SQLite family for offline maintenance",
    )
    .map_err(io::Error::other)?;
    let before = family_state(path)?;
    let mut hash = Sha256::new();
    for (label, member) in [
        (b"db".as_slice(), path.to_path_buf()),
        (b"wal", with_suffix(path, "-wal")),
    ] {
        if !member.is_file() {
            continue;
        }
        let bytes = fs::metadata(&member)?.len();
        // BEGIN IMMEDIATE may create an empty WAL while acquiring the apply
        // guard. An empty sidecar contains no logical database state.
        if label == b"wal" && bytes == 0 {
            continue;
        }
        hash.update(label);
        hash.update(bytes.to_be_bytes());
        let mut file = fs::File::open(&member)?;
        let mut buffer = vec![0_u8; 1024 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hash.update(&buffer[..read]);
        }
    }
    if family_state(path)? != before {
        return Err(changed_during_snapshot(path));
    }
    Ok(hex::encode(hash.finalize()))
}

/// How long a bounded probe waits on a lock before giving up.
///
/// Short on purpose, and the same bound everywhere: a probe reports its store
/// as unsampled rather than delaying a live daemon writing to it.
pub const BOUNDED_PROBE_BUSY_TIMEOUT: Duration = Duration::from_millis(200);

/// Opens `path` strictly read-only with a bounded busy timeout, for callers
/// that only need to read a pragma or check whether a table exists.
///
/// This is the deliberately cheap counterpart to [`SnapshotSet::capture_in`]:
/// it copies nothing and freezes nothing, so it is only appropriate where a
/// torn read is acceptable and a busy store degrades to "not sampled" instead
/// of being retried. Anything that needs a consistent view of a live family
/// must take a real snapshot.
///
/// `SQLITE_OPEN_NO_MUTEX` is sound here because `rusqlite::Connection` is not
/// `Sync`, so the returned connection stays owned by one thread at a time.
pub fn open_read_only_probe(path: &Path, busy_timeout: Duration) -> rusqlite::Result<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(busy_timeout)?;
    Ok(connection)
}

/// Reads `PRAGMA <pragma>` as a non-negative count, or `None` when the pragma
/// is unavailable or does not answer with an integer.
///
/// A negative answer clamps to zero: every pragma read through this is a page
/// or byte count, for which a negative value is not a smaller number but a
/// missing one.
#[must_use]
pub fn pragma_u64(connection: &Connection, pragma: &str) -> Option<u64> {
    connection
        .query_row(&format!("PRAGMA {pragma}"), [], |row| row.get::<_, i64>(0))
        .ok()
        .map(|value: i64| value.max(0) as u64)
}

fn prepare_one(
    source: &Path,
    scratch: &ScratchDirectory,
    index: usize,
) -> io::Result<PreparedSnapshot> {
    let authority = crate::db::DatabaseAuthority::for_runtime(
        source,
        "capture SQLite family for offline maintenance",
    )
    .map_err(io::Error::other)?;
    let directory = scratch.path.join(index.to_string());
    create_private_directory(&directory)?;
    let target = directory.join("database.db");
    let source_state = family_state(source)?;
    let main = source_state
        .iter()
        .find(|state| state.path == source)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("SQLite database '{}' does not exist", source.display()),
            )
        })?;
    let has_wal = source_state
        .iter()
        .any(|state| state.path == with_suffix(source, "-wal"));
    let mode = if has_wal {
        if reflink_copy::reflink(source, &target).is_ok() {
            SnapshotMode::Reflink
        } else {
            let _ = fs::remove_file(&target);
            SnapshotMode::Copy
        }
    } else {
        checkpointed_snapshot_mode()
    };
    let mut copy_bytes = if matches!(mode, SnapshotMode::Copy) {
        main.bytes
    } else {
        0
    };
    if !matches!(mode, SnapshotMode::DirectImmutable) {
        for suffix in ["-wal", "-shm"] {
            let source_member = with_suffix(source, suffix);
            if let Some(state) = source_state
                .iter()
                .find(|state| state.path == source_member)
            {
                copy_bytes = copy_bytes.saturating_add(state.bytes);
            }
        }
    }
    if family_state(source)? != source_state {
        return Err(changed_during_snapshot(source));
    }
    Ok(PreparedSnapshot {
        source: source.to_path_buf(),
        source_state,
        target,
        mode,
        copy_bytes,
        authority,
    })
}

fn checkpointed_snapshot_mode() -> SnapshotMode {
    // SQLite's immutable connection still holds a byte-range lock on Windows.
    // Consolidation retains read snapshots while copying the frozen inputs, so
    // opening a private copy keeps those handles off the source database.
    #[cfg(windows)]
    {
        SnapshotMode::Copy
    }
    #[cfg(not(windows))]
    {
        SnapshotMode::DirectImmutable
    }
}

async fn finish_one(
    prepared: PreparedSnapshot,
    scratch: Arc<ScratchDirectory>,
) -> io::Result<SnapshotDatabase> {
    if matches!(prepared.mode, SnapshotMode::Copy) {
        fs::copy(&prepared.source, &prepared.target)?;
    }
    if !matches!(prepared.mode, SnapshotMode::DirectImmutable) {
        for suffix in ["-wal", "-shm"] {
            let source_member = with_suffix(&prepared.source, suffix);
            let Some(_) = prepared
                .source_state
                .iter()
                .find(|state| state.path == source_member)
            else {
                continue;
            };
            fs::copy(&source_member, with_suffix(&prepared.target, suffix))?;
        }
    }
    if family_state(&prepared.source)? != prepared.source_state {
        return Err(changed_during_snapshot(&prepared.source));
    }
    if !matches!(prepared.mode, SnapshotMode::DirectImmutable) {
        materialize_standalone_snapshot(&prepared.target).await?;
    }
    // `identity_path` is the real file on disk; `attach_path` is the URI used
    // to ATTACH it. They are never interchangeable — the URI is percent-encoded
    // and carries query parameters, so passing it to the filesystem fails.
    let (open_path, attach_path, identity_path, flags, scratch) =
        if matches!(prepared.mode, SnapshotMode::DirectImmutable) {
            let uri = PathBuf::from(immutable_uri(&prepared.source)?);
            (
                uri.clone(),
                uri,
                prepared.source.clone(),
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
                None,
            )
        } else {
            let uri = PathBuf::from(immutable_uri(&prepared.target)?);
            (
                uri.clone(),
                uri,
                prepared.target.clone(),
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
                Some(scratch),
            )
        };
    let connection = SnapshotConnection::open(&open_path, flags).map_err(io::Error::other)?;
    connection
        .execute_batch("PRAGMA query_only = ON; PRAGMA busy_timeout = 5000;")
        .await
        .map_err(io::Error::other)?;
    let snapshot = SnapshotDatabase {
        connection,
        source: prepared.source,
        source_state: prepared.source_state,
        path: attach_path,
        identity_path,
        _scratch: scratch,
        _authority: prepared.authority,
        #[cfg(any(test, feature = "test-helpers"))]
        copied_bytes: prepared.copy_bytes,
    };
    snapshot.validate_source()?;
    Ok(snapshot)
}

async fn materialize_standalone_snapshot(path: &Path) -> io::Result<()> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let standalone = with_suffix(&path, ".standalone");
        match fs::remove_file(&standalone) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let source = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(io::Error::other)?;
        let mut destination = Connection::open_with_flags(
            &standalone,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .map_err(io::Error::other)?;
        {
            let backup = rusqlite::backup::Backup::new(&source, &mut destination)
                .map_err(io::Error::other)?;
            backup
                .run_to_completion(128, Duration::from_millis(1), None)
                .map_err(io::Error::other)?;
        }
        drop(destination);
        drop(source);
        for member in family_paths(&path) {
            match fs::remove_file(member) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        fs::rename(standalone, path)
    })
    .await
    .map_err(|error| io::Error::other(format!("snapshot materialization task failed: {error}")))?
}

fn changed_during_snapshot(source: &Path) -> io::Error {
    io::Error::other(format!(
        "SQLite database family '{}' changed while taking a read snapshot",
        source.display()
    ))
}

fn create_scratch_directory(
    root: &Path,
    expected_uid: Option<u32>,
) -> io::Result<ScratchDirectory> {
    ensure_private_root(root, expected_uid)?;
    let cleanup_lock = open_private_lock(&root.join(".cleanup.lock"), true)?;
    cleanup_lock.lock_exclusive()?;
    cleanup_stale_directories(root)?;
    for _ in 0..100 {
        let id = NEXT_SNAPSHOT.fetch_add(1, Ordering::Relaxed);
        let path = root.join(format!("read-{}-{id}", std::process::id()));
        match create_private_directory(&path) {
            Ok(()) => {
                let owner_lock = open_private_lock(&path.join(".owner.lock"), true)?;
                owner_lock.lock_exclusive()?;
                FileExt::unlock(&cleanup_lock)?;
                return Ok(ScratchDirectory {
                    path,
                    owner_lock: Some(owner_lock),
                });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique SQLite read snapshot directory",
    ))
}

#[cfg(any(test, feature = "test-helpers"))]
#[cfg_attr(not(unix), allow(clippy::unnecessary_wraps))] // Preserve the fallible Unix contract.
fn default_scratch_root(paths: &[PathBuf]) -> io::Result<PathBuf> {
    #[cfg(unix)]
    {
        let uid = expected_owner(paths)?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "no SQLite input path was supplied")
        })?;
        Ok(std::env::temp_dir().join(format!("tracedecay-sqlite-read-{uid}")))
    }
    #[cfg(not(unix))]
    {
        let _ = paths;
        Ok(std::env::temp_dir().join("tracedecay-sqlite-read"))
    }
}

#[cfg_attr(not(unix), allow(clippy::unnecessary_wraps))] // Preserve the fallible Unix contract.
fn expected_owner(paths: &[PathBuf]) -> io::Result<Option<u32>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let path = paths.first().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "no SQLite input path was supplied",
            )
        })?;
        Ok(Some(fs::metadata(path)?.uid()))
    }
    #[cfg(not(unix))]
    {
        let _ = paths;
        Ok(None)
    }
}

fn ensure_private_root(root: &Path, expected_uid: Option<u32>) -> io::Result<()> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(io::Error::other(format!(
                "SQLite scratch root '{}' is not a directory",
                root.display()
            )));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_private_directory_all(root)?;
        }
        Err(error) => return Err(error),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let metadata = fs::symlink_metadata(root)?;
        if expected_uid.is_some_and(|uid| metadata.uid() != uid) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "SQLite scratch root '{}' has the wrong owner",
                    root.display()
                ),
            ));
        }
        if metadata.permissions().mode() & 0o077 != 0 {
            fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        }
    }
    #[cfg(not(unix))]
    let _ = expected_uid;
    Ok(())
}

fn create_private_directory(path: &Path) -> io::Result<()> {
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let mut builder = builder;
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

fn create_private_directory_all(path: &Path) -> io::Result<()> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    // Walk up only as far as the deepest component that already exists. The
    // components below it are the ones this call creates, and the loop below
    // re-checks each of them. Ancestors above it belong to the operating system
    // and are routinely symlinks -- macOS reaches the default temporary
    // directory through `/var` -> `/private/var` -- so requiring the whole
    // chain to be symlink-free rejected every scratch path on that platform.
    // The scratch root's own owner and mode are verified by
    // `ensure_private_root`, which is what actually keeps it private.
    let mut missing = Vec::new();
    let mut current = path.as_path();
    loop {
        let is_ancestor = current != path.as_path();
        match fs::symlink_metadata(current) {
            // The target itself must never be a symlink; an ancestor may be, so
            // long as it leads to a directory.
            Ok(metadata) if is_ancestor && metadata.file_type().is_symlink() => {
                if !fs::metadata(current)?.is_dir() {
                    return Err(io::Error::other(format!(
                        "SQLite scratch path component '{}' is not a regular directory",
                        current.display()
                    )));
                }
                break;
            }
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(io::Error::other(format!(
                    "SQLite scratch path component '{}' is not a regular directory",
                    current.display()
                )));
            }
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing.push(current.to_path_buf());
            }
            Err(error) => return Err(error),
        }
        let Some(parent) = current.parent() else {
            break;
        };
        current = parent;
    }

    for directory in missing.into_iter().rev() {
        match create_private_directory(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let metadata = fs::symlink_metadata(&directory)?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(io::Error::other(format!(
                        "SQLite scratch path component '{}' is not a regular directory",
                        directory.display()
                    )));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    if metadata.permissions().mode() & 0o077 != 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            format!(
                                "concurrently created SQLite scratch directory '{}' is not private",
                                directory.display()
                            ),
                        ));
                    }
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn open_private_lock(path: &Path, create: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(create)
        .truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn cleanup_stale_directories(root: &Path) -> io::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("read-") {
            continue;
        }
        let path = entry.path();
        if !fs::symlink_metadata(&path)?.is_dir() {
            continue;
        }
        let removable = match open_private_lock(&path.join(".owner.lock"), false) {
            Ok(lock) => lock.try_lock_exclusive().is_ok(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => true,
            Err(error) => return Err(error),
        };
        if removable {
            fs::remove_dir_all(path)?;
        }
    }
    Ok(())
}

fn family_state(path: &Path) -> io::Result<Vec<FileState>> {
    let mut states = Vec::new();
    for member in family_paths(path) {
        match fs::metadata(&member) {
            Ok(metadata) if metadata.is_file() => {
                #[cfg(unix)]
                use std::os::unix::fs::MetadataExt;
                states.push(FileState {
                    path: member,
                    bytes: metadata.len(),
                    modified: metadata.modified()?,
                    #[cfg(unix)]
                    device: metadata.dev(),
                    #[cfg(unix)]
                    inode: metadata.ino(),
                    #[cfg(unix)]
                    changed_seconds: metadata.ctime(),
                    #[cfg(unix)]
                    changed_nanoseconds: metadata.ctime_nsec(),
                    #[cfg(unix)]
                    links: metadata.nlink(),
                });
            }
            Ok(_) => {
                return Err(io::Error::other(format!(
                    "SQLite family member '{}' is not a file",
                    member.display()
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(states)
}

fn durable_family_state(path: &Path, states: &[FileState]) -> Vec<FileState> {
    let wal = with_suffix(path, "-wal");
    states
        .iter()
        .filter(|state| state.path == path || (state.path == wal && state.bytes > 0))
        .cloned()
        .collect()
}

fn family_paths(path: &Path) -> [PathBuf; 3] {
    [
        path.to_path_buf(),
        with_suffix(path, "-wal"),
        with_suffix(path, "-shm"),
    ]
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

pub(crate) fn immutable_uri(path: &Path) -> io::Result<String> {
    Ok(format!("{}&immutable=1", read_only_uri(path)?))
}

fn read_only_uri(path: &Path) -> io::Result<String> {
    let raw = path.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("SQLite path '{}' is not UTF-8", path.display()),
        )
    })?;
    let mut encoded = String::with_capacity(raw.len() + 24);
    for ch in raw.chars() {
        match ch {
            '?' => encoded.push_str("%3f"),
            '#' => encoded.push_str("%23"),
            '%' => encoded.push_str("%25"),
            other => encoded.push(other),
        }
    }
    Ok(format!("file:{encoded}?mode=ro"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn checkpointed_inspection_is_purpose_bound_and_refuses_live_wal() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("source.db");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE durable(value TEXT NOT NULL);
                 CREATE TABLE empty(value TEXT NOT NULL);
                 INSERT INTO durable(value) VALUES ('retained');",
            )
            .unwrap();
        drop(connection);

        assert!(checkpointed_database_has_any_rows(&path, &["empty", "durable"]).unwrap());
        assert!(!checkpointed_database_has_any_rows(&path, &["empty"]).unwrap());
        assert!(checkpointed_database_has_any_rows(&path, &["bad-name"]).is_err());

        fs::write(with_suffix(&path, "-wal"), b"live").unwrap();
        assert!(checkpointed_database_has_any_rows(&path, &["durable"]).is_err());
    }

    #[tokio::test]
    async fn snapshot_reads_wal_rows_without_touching_source_bytes_or_mtime() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("source.db");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 CREATE TABLE durable(value TEXT NOT NULL);
                 INSERT INTO durable(value) VALUES ('wal-resident');",
            )
            .unwrap();
        assert!(with_suffix(&path, "-wal").metadata().unwrap().len() > 0);
        let before = family_state(&path).unwrap();

        let snapshot = open(&path).await.unwrap();
        let mut rows = snapshot
            .connection()
            .query("SELECT value FROM durable", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap(),
            "wal-resident"
        );
        assert_eq!(
            snapshot.attach_token().unwrap().verified_path().unwrap(),
            snapshot.path()
        );
        assert!(
            ["-wal", "-shm"].into_iter().all(|suffix| !with_suffix(
                &snapshot.identity_path,
                suffix
            )
            .exists())
        );
        assert_eq!(family_state(&path).unwrap(), before);
    }

    #[tokio::test]
    async fn copied_snapshot_survives_empty_writer_sidecar_cleanup() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("source.db");
        let writer = Connection::open(&path).unwrap();
        writer
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 CREATE TABLE durable(value TEXT NOT NULL);
                 PRAGMA wal_checkpoint(TRUNCATE);
                 BEGIN IMMEDIATE;",
            )
            .unwrap();
        let wal = with_suffix(&path, "-wal");
        let shm = with_suffix(&path, "-shm");
        assert_eq!(fs::metadata(&wal).unwrap().len(), 0);
        assert!(shm.is_file());

        let snapshot = open(&path).await.unwrap();
        assert_ne!(snapshot.identity_path, path);

        writer.execute_batch("ROLLBACK;").unwrap();
        drop(writer);
        for sidecar in [wal, shm] {
            match fs::remove_file(sidecar) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => panic!("could not remove transient sidecar: {error}"),
            }
        }

        assert_eq!(
            snapshot.attach_token().unwrap().verified_path().unwrap(),
            snapshot.path()
        );
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn checkpointed_database_reads_directly_without_copy_or_metadata_change() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("source.db");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE durable(value TEXT NOT NULL);
                 INSERT INTO durable(value) VALUES ('checkpointed');",
            )
            .unwrap();
        drop(connection);
        let before = family_state(&path).unwrap();
        let snapshots = SnapshotSet::capture(std::slice::from_ref(&path))
            .await
            .unwrap();
        assert_eq!(snapshots.copied_bytes(), 0);
        let mut rows = snapshots
            .get(&path)
            .unwrap()
            .connection()
            .query("SELECT value FROM durable", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap(),
            "checkpointed"
        );
        assert_eq!(family_state(&path).unwrap(), before);
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn direct_immutable_attach_token_verifies_the_filesystem_identity() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("source.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE durable(value TEXT NOT NULL);")
            .unwrap();

        let snapshot = open(&path).await.unwrap();
        assert_ne!(snapshot.path(), snapshot.identity_path);
        assert_eq!(
            snapshot.attach_token().unwrap().verified_path().unwrap(),
            snapshot.path()
        );
    }

    #[tokio::test]
    async fn snapshot_executor_cannot_mutate_main_or_attached_inputs() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source.db");
        let other = temp.path().join("other.db");
        Connection::open(&source)
            .unwrap()
            .execute_batch(
                "CREATE TABLE durable(value TEXT NOT NULL);
                 INSERT INTO durable(value) VALUES ('original');",
            )
            .unwrap();
        let other_writer = Connection::open(&other).unwrap();
        other_writer
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 CREATE TABLE durable(value TEXT NOT NULL);
                 INSERT INTO durable(value) VALUES ('original');",
            )
            .unwrap();
        assert!(with_suffix(&other, "-wal").is_file());
        let source_before = family_state(&source).unwrap();
        let other_before = family_state(&other).unwrap();
        let snapshots = SnapshotSet::capture(&[source.clone(), other.clone()])
            .await
            .unwrap();
        let source_snapshot = snapshots.get(&source).unwrap();
        let other_snapshot = snapshots.get(&other).unwrap();
        source_snapshot
            .connection()
            .execute(
                "ATTACH DATABASE ?1 AS other",
                crate::db::engine::params![other_snapshot.path().to_string_lossy().to_string()],
            )
            .await
            .unwrap();
        source_snapshot
            .connection()
            .execute_batch("PRAGMA query_only = OFF;")
            .await
            .unwrap();

        assert!(
            source_snapshot
                .connection()
                .execute("INSERT INTO main.durable(value) VALUES ('changed')", ())
                .await
                .is_err()
        );
        assert!(
            source_snapshot
                .connection()
                .execute("INSERT INTO other.durable(value) VALUES ('changed')", ())
                .await
                .is_err()
        );
        source_snapshot
            .connection()
            .execute("DETACH DATABASE other", ())
            .await
            .unwrap();
        assert_eq!(family_state(&source).unwrap(), source_before);
        assert_eq!(family_state(&other).unwrap(), other_before);
        drop(other_writer);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn checkpointed_snapshot_does_not_lock_source_against_copying() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("source.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE durable(value TEXT NOT NULL);")
            .unwrap();

        let snapshots = SnapshotSet::capture(std::slice::from_ref(&path))
            .await
            .unwrap();
        assert_eq!(snapshots.copied_bytes(), fs::metadata(&path).unwrap().len());
        fs::copy(&path, temp.path().join("backup.db")).unwrap();
    }

    #[test]
    fn empty_wal_does_not_change_the_content_fingerprint() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("source.db");
        fs::write(&path, b"database bytes").unwrap();
        let before = family_fingerprint(&path).unwrap();
        fs::write(with_suffix(&path, "-wal"), b"").unwrap();
        assert_eq!(family_fingerprint(&path).unwrap(), before);
        fs::write(with_suffix(&path, "-wal"), b"logical frame").unwrap();
        assert_ne!(family_fingerprint(&path).unwrap(), before);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn nested_missing_scratch_root_is_created_private() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = TempDir::new().unwrap();
        let path = temp.path().join("source.db");
        let first = temp.path().join("missing");
        let second = first.join("nested");
        let scratch_root = second.join("sqlite-read");
        Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE durable(value TEXT NOT NULL);")
            .unwrap();

        let snapshots = SnapshotSet::capture_in(&[path], &scratch_root)
            .await
            .unwrap();

        for directory in [&first, &second, &scratch_root, &snapshots.scratch.path] {
            assert_eq!(
                fs::metadata(directory).unwrap().permissions().mode() & 0o777,
                0o700,
                "{} must be owner-only",
                directory.display()
            );
        }
    }

    /// macOS reaches its default temporary directory through the system
    /// `/var` -> `/private/var` symlink, so a symlinked ancestor that leads to a
    /// directory has to be usable. The scratch root's own owner and mode are
    /// what keep it private.
    #[cfg(unix)]
    #[tokio::test]
    async fn nested_scratch_root_accepts_a_symlinked_ancestor() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let path = temp.path().join("source.db");
        let real = temp.path().join("real");
        let linked = temp.path().join("linked");
        fs::create_dir(&real).unwrap();
        fs::create_dir(real.join("existing")).unwrap();
        symlink(&real, &linked).unwrap();
        Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE durable(value TEXT NOT NULL);")
            .unwrap();

        SnapshotSet::capture_in(&[path], &linked.join("existing/sqlite-read"))
            .await
            .expect("a symlinked ancestor that leads to a directory is usable");
        assert!(real.join("existing/sqlite-read").is_dir());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn scratch_root_rejects_a_symlinked_target() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let path = temp.path().join("source.db");
        let real = temp.path().join("real");
        let linked = temp.path().join("linked");
        fs::create_dir(&real).unwrap();
        symlink(&real, &linked).unwrap();
        Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE durable(value TEXT NOT NULL);")
            .unwrap();

        let error = match SnapshotSet::capture_in(&[path], &linked).await {
            Ok(_) => panic!("a symlinked scratch root must be rejected"),
            Err(error) => error,
        };

        assert!(
            error.to_string().contains("not a directory"),
            "unexpected error: {error}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn scratch_is_private_and_next_capture_cleans_crash_debris() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new().unwrap();
        let path = temp.path().join("source.db");
        let scratch_root = temp.path().join("private-scratch");
        Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE durable(value TEXT NOT NULL);")
            .unwrap();

        ensure_private_root(
            &scratch_root,
            expected_owner(std::slice::from_ref(&path)).unwrap(),
        )
        .unwrap();
        let stale = scratch_root.join("read-999999-0");
        create_private_directory(&stale).unwrap();
        fs::write(stale.join("database.db"), b"private session data").unwrap();
        fs::write(stale.join(".owner.lock"), b"").unwrap();

        let snapshots = SnapshotSet::capture_in(&[path], &scratch_root)
            .await
            .unwrap();
        assert!(
            !stale.exists(),
            "an unlocked crashed snapshot must be cleaned"
        );
        assert_eq!(
            fs::metadata(&scratch_root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&snapshots.scratch.path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let live = snapshots.scratch.path.clone();
        drop(snapshots);
        assert!(
            !live.exists(),
            "normal drop must remove copied database data"
        );
        assert!(
            fs::read_dir(&scratch_root)
                .unwrap()
                .all(|entry| entry.unwrap().file_name() == ".cleanup.lock")
        );
    }
}
