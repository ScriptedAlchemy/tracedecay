use std::{
    fmt,
    fs::{File, OpenOptions},
    io,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};
use tracedecay_sql::{
    Capability, Connection, ConnectionControl, EnginePolicy, OpenFlags, file_identity,
};
use tracedecay_store::WAL_SOFT_LIMIT_BYTES;

/// WAL bytes this runtime keeps allocated across a checkpoint that resets the
/// log.
///
/// A checkpoint returns WAL *contents* to the database; it does not return the
/// WAL file's blocks. `journal_size_limit` is the only control that does, and
/// SQLite's default (`-1`) never shrinks the file, so an isolated write burst
/// pins the WAL at its high-water mark for the life of the database. This
/// runtime is more exposed than most, because it sets `wal_autocheckpoint = 0`
/// and drives every checkpoint from [`crate::checkpoint`]: SQLite will not
/// opportunistically reset the log on our behalf.
///
/// The number is the checkpoint controller's soft-limit ceiling
/// ([`WAL_SOFT_LIMIT_BYTES`], the largest soft limit an operator may configure
/// under `AdmissionConfigV1::validate`), for two reasons that bracket it from
/// both sides:
///
/// - Not lower. Below the soft limit is the band the controller deliberately
///   declines to checkpoint, so it is exactly the WAL span this runtime expects
///   to reuse continuously. Truncating into it would hand back blocks that the
///   next writes immediately re-extend, converting reclaim into per-commit file
///   growth and metadata I/O on the write path.
/// - Not higher. Above the soft limit is the exceptional band the controller
///   only tolerates while readers or snapshot leases block a checkpoint (up to
///   the 256 MiB hard limit). That overage is a symptom of transient blocking,
///   not a working set, and must not become permanent on-disk cost.
///
/// This is a file-size bound only: it takes effect after a checkpoint has
/// already persisted the frames, so it cannot lose a committed write.
const RETAINED_WAL_BYTES: i64 = WAL_SOFT_LIMIT_BYTES as i64;

/// Pins and identifies the exact regular file that an attachment is about to
/// open. The descriptor stays alive until every SQLite worker has reported
/// startup, after which `verify_current_path` proves the pathname still names
/// that same physical file. Attachments retain the identity, never a later
/// pathname stat.
#[derive(Debug)]
pub struct OpenedDatabaseFile {
    file: File,
    identity: u64,
}

impl OpenedDatabaseFile {
    pub fn pin(path: &Path) -> Result<Self, OpenedDatabaseFileError> {
        let file = open_pinned_database(path).map_err(|_| OpenedDatabaseFileError::Open)?;
        Self::adopt(file)
    }

    pub(crate) fn create_new(path: &Path) -> Result<Self, OpenedDatabaseFileError> {
        let file = create_pinned_database(path).map_err(|_| OpenedDatabaseFileError::Create)?;
        Self::adopt(file)
    }

    /// Takes ownership of an already-open handle and records its identity.
    fn adopt(file: File) -> Result<Self, OpenedDatabaseFileError> {
        let metadata = file
            .metadata()
            .map_err(|_| OpenedDatabaseFileError::Inspect)?;
        if !metadata.is_file() {
            return Err(OpenedDatabaseFileError::NotFile);
        }
        let identity = identity_of(&file)?;
        Ok(Self { file, identity })
    }

    pub const fn identity(&self) -> u64 {
        self.identity
    }

    pub(crate) fn try_clone(&self) -> Result<Self, OpenedDatabaseFileError> {
        Ok(Self {
            file: self
                .file
                .try_clone()
                .map_err(|_| OpenedDatabaseFileError::Open)?,
            identity: self.identity,
        })
    }

    #[cfg(all(unix, any(target_os = "linux", target_os = "android")))]
    pub(crate) fn worker_open_path(
        &self,
        _canonical_path: &Path,
    ) -> Result<PathBuf, OpenedDatabaseFileError> {
        use std::os::unix::io::AsRawFd;

        Ok(PathBuf::from(format!(
            "/proc/self/fd/{}",
            self.file.as_raw_fd()
        )))
    }

    /// Selects the pathname used by a writer connection.
    ///
    /// Linux can resolve SQLite's WAL sidecars from `/proc/self/fd/*` while
    /// retaining the pinned-file ABA fence. macOS (and other non-Linux Unix
    /// hosts) cannot reliably create fresh WAL sidecars from `/dev/fd/*`, so
    /// writers use the verified canonical pathname while the pinned descriptor
    /// remains alive for the worker lifetime.
    #[cfg(any(unix, windows))]
    pub(crate) fn writer_open_path(
        &self,
        canonical_path: &Path,
    ) -> Result<PathBuf, OpenedDatabaseFileError> {
        #[cfg(all(unix, any(target_os = "linux", target_os = "android")))]
        {
            self.worker_open_path(canonical_path)
        }
        #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
        {
            Ok(canonical_path.to_path_buf())
        }
        #[cfg(windows)]
        {
            Ok(canonical_path.to_path_buf())
        }
    }

    /// Selects the pathname used by a reader connection.
    ///
    /// A WAL reader must open the `-shm` file, so it needs the same
    /// sidecar-safe pathname a writer does. SQLite derives sidecar names by
    /// appending to the *full pathname* it resolved: Linux `/proc/self/fd/*`
    /// is a symlink, so `unixFullPathname` follows it and derives the real
    /// `<database>-shm`; macOS `/dev/fd/*` is a devfs entry that is not a
    /// symlink, so SQLite keeps that pathname and looks for the impossible
    /// `/dev/fd/<fd>-shm` and fails the first schema read with
    /// `SQLITE_CANTOPEN` ("unable to open database file"). Deferring to the
    /// writer policy keeps Linux on the descriptor pathname byte-for-byte and
    /// gives every other Unix host the verified canonical pathname.
    ///
    /// The pinned-descriptor ABA fence is unaffected: the reader worker still
    /// runs `verify_connection` (pathname inode identity plus
    /// `SQLITE_FCNTL_HAS_MOVED`, rechecked afterwards) and re-pins the file
    /// before it reports startup, the same fence that already makes the
    /// writer's canonical-path open safe on these hosts.
    pub fn reader_open_path(
        &self,
        canonical_path: &Path,
    ) -> Result<PathBuf, OpenedDatabaseFileError> {
        self.writer_open_path(canonical_path)
    }

    #[cfg(not(any(unix, windows)))]
    pub(crate) fn writer_open_path(
        &self,
        _canonical_path: &Path,
    ) -> Result<PathBuf, OpenedDatabaseFileError> {
        Err(OpenedDatabaseFileError::Unsupported)
    }

    pub fn verify_current_path(&self, path: &Path) -> Result<(), OpenedDatabaseFileError> {
        let current = File::open(path).map_err(|_| OpenedDatabaseFileError::Open)?;
        if identity_of(&current)? != self.identity {
            return Err(OpenedDatabaseFileError::Replaced);
        }
        let _ = &self.file;
        Ok(())
    }

    pub(crate) fn verify_connection(
        &self,
        _connection: &Connection,
        canonical_path: &Path,
    ) -> Result<(), OpenedDatabaseFileError> {
        // Check the pathname identity first. If it changes during this stat,
        // the engine's moved-file check below still observes the handle's
        // different inode.
        self.verify_current_path(canonical_path)?;
        #[cfg(unix)]
        if _connection
            .file_has_moved()
            .map_err(|_| OpenedDatabaseFileError::Inspect)?
        {
            return Err(OpenedDatabaseFileError::Replaced);
        }
        #[cfg(unix)]
        {
            // Recheck after the file-control syscall; both checks must agree
            // before any writer policy can create or mutate sidecars.
            self.verify_current_path(canonical_path)?;
        }
        Ok(())
    }

    pub(crate) fn discard_created(self, path: &Path) -> Result<(), OpenedDatabaseFileError> {
        self.verify_current_path(path)?;
        let Self { file, .. } = self;
        drop(file);
        for candidate in [
            sidecar_path(path, "-wal"),
            sidecar_path(path, "-shm"),
            sidecar_path(path, "-journal"),
            path.to_path_buf(),
        ] {
            match std::fs::remove_file(candidate) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(_) => return Err(OpenedDatabaseFileError::Remove),
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenedDatabaseFileError {
    Create,
    Open,
    Inspect,
    NotFile,
    #[cfg(windows)]
    Identify,
    Replaced,
    Remove,
    #[cfg(not(any(unix, windows)))]
    Unsupported,
}

impl fmt::Display for OpenedDatabaseFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Create => "could not create the canonical SQLite file",
            Self::Open => "could not open the verified SQLite file",
            Self::Inspect => "could not inspect the verified SQLite file descriptor",
            Self::NotFile => "verified SQLite locator is not a regular file",
            #[cfg(windows)]
            Self::Identify => "could not identify the verified SQLite file descriptor",
            Self::Replaced => "verified SQLite file was replaced while opening workers",
            Self::Remove => "could not remove an uncommitted canonical SQLite file",
            #[cfg(not(any(unix, windows)))]
            Self::Unsupported => "SQLite file identity is unsupported on this platform",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for OpenedDatabaseFileError {}

#[cfg(not(windows))]
fn open_pinned_database(path: &Path) -> io::Result<File> {
    File::open(path)
}

#[cfg(windows)]
fn open_pinned_database(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;

    OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(path)
}

#[cfg(not(windows))]
fn create_pinned_database(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
}

#[cfg(windows)]
fn create_pinned_database(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;

    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(path)
}

#[cfg(windows)]
const FILE_SHARE_READ: u32 = 0x0000_0001;
#[cfg(windows)]
const FILE_SHARE_WRITE: u32 = 0x0000_0002;

fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    value.into()
}

/// The open file's engine identity folded to the nonzero `u64` attachments
/// retain.
fn identity_of(file: &File) -> Result<u64, OpenedDatabaseFileError> {
    #[cfg(windows)]
    let unidentified = OpenedDatabaseFileError::Identify;
    #[cfg(not(windows))]
    let unidentified = OpenedDatabaseFileError::Inspect;
    let identity = file_identity(file).map_err(|_| unidentified)?;
    let mut hasher = Sha256::new();
    hasher.update(identity.volume.to_le_bytes());
    hasher.update(identity.file.to_le_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    Ok(u64::from_le_bytes(bytes).max(1))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConnectionMode {
    Writer,
    Reader,
}

impl ConnectionMode {
    const fn capability(self) -> Capability {
        match self {
            Self::Writer => Capability::Writer,
            Self::Reader => Capability::Reader,
        }
    }
}

#[derive(Debug)]
pub(crate) enum WriterOpenError {
    Policy(ConnectionPolicyError),
    Identity(OpenedDatabaseFileError),
}

#[derive(Debug)]
pub struct ConnectionPolicyError {
    stage: &'static str,
    source: tracedecay_sql::Error,
}

impl ConnectionPolicyError {
    pub fn is_open_failure(&self) -> bool {
        self.stage == "open"
    }
}

impl fmt::Display for ConnectionPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "SQLite connection policy failed at {}: {}",
            self.stage, self.source
        )
    }
}

impl std::error::Error for ConnectionPolicyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

pub(crate) fn open(path: &Path, mode: ConnectionMode) -> Result<Connection, ConnectionPolicyError> {
    finish_open(open_raw(path, mode)?, mode)
}

pub(crate) fn open_writer(
    path: &Path,
    opened_database: Option<&OpenedDatabaseFile>,
    canonical_path: &Path,
) -> Result<Connection, WriterOpenError> {
    let connection = open_raw(path, ConnectionMode::Writer).map_err(WriterOpenError::Policy)?;
    if let Some(opened_database) = opened_database {
        opened_database
            .verify_connection(&connection, canonical_path)
            .map_err(WriterOpenError::Identity)?;
    }
    let connection =
        finish_open(connection, ConnectionMode::Writer).map_err(WriterOpenError::Policy)?;
    if let Some(opened_database) = opened_database {
        opened_database
            .verify_connection(&connection, canonical_path)
            .map_err(WriterOpenError::Identity)?;
    }
    Ok(connection)
}

/// The pathname SQLite is handed, spelled for its Windows VFS.
///
/// A `\\?\`-prefixed (extended-length/UNC-form) path makes the win32 VFS
/// treat the database as UNC (`winIsUNCPath`), which puts every WAL
/// shared-memory lock through the shared-handle emulation: read-lock
/// ownership then lives in per-connection masks rather than real OS byte
/// locks, and a failed `UnlockFile` there leaves a mask bit set forever —
/// an error the VFS cannot see because its own bookkeeping is what lies.
/// The classic per-handle path instead conflicts against real OS locks,
/// so a lock the VFS records is a lock it genuinely holds.
///
/// `dunce::simplified` is the sole conversion gate: it returns `path`
/// unchanged on non-Windows hosts, for non-verbatim spellings, and whenever
/// any component would parse differently without the prefix (trailing
/// dots/spaces, `.`/`..`, reserved DOS-device stems, invalid filename
/// bytes, non-Unicode names, or a path beyond the classic limit). Refused
/// verbatim paths are returned untouched — never re-examined here. Two
/// guards sit on the accepted conversion: a reserved device stem dunce's
/// ASCII list misses (superscript-digit COM¹..COM³, LPT¹..LPT³) must keep
/// the prefix, and SQLite's `-wal`/`-shm` sidecars must fit the classic
/// `CreateFileW` limit measured in UTF-16 code units.
pub fn sqlite_host_path(path: &Path) -> PathBuf {
    let simplified = dunce::simplified(path);
    if simplified == path {
        return path.to_path_buf();
    }
    let Some(text) = simplified.to_str() else {
        return path.to_path_buf();
    };
    if text.split('\\').any(device_stem) {
        return path.to_path_buf();
    }
    if text.encode_utf16().count() + 4 <= 259 {
        PathBuf::from(text)
    } else {
        path.to_path_buf()
    }
}

/// Whether a component's stem (the part Win32 checks before an extension,
/// right-trimmed of `.`/` `) names a reserved DOS device: ASCII COM/LPT/NUL/
/// CON/PRN/AUX plus the superscript-digit variants outside dunce's list.
fn device_stem(component: &str) -> bool {
    const DEVICES: &[&str] = &[
        "CON",
        "PRN",
        "AUX",
        "NUL",
        "COM1",
        "COM2",
        "COM3",
        "COM4",
        "COM5",
        "COM6",
        "COM7",
        "COM8",
        "COM9",
        "LPT1",
        "LPT2",
        "LPT3",
        "LPT4",
        "LPT5",
        "LPT6",
        "LPT7",
        "LPT8",
        "LPT9",
        "COM\u{b9}",
        "COM\u{b2}",
        "COM\u{b3}",
        "LPT\u{b9}",
        "LPT\u{b2}",
        "LPT\u{b3}",
    ];
    let stem = component
        .split('.')
        .next()
        .unwrap_or(component)
        .trim_end_matches([' ', '.']);
    DEVICES
        .iter()
        .any(|device| stem.eq_ignore_ascii_case(device))
}

fn open_raw(path: &Path, mode: ConnectionMode) -> Result<Connection, ConnectionPolicyError> {
    {
        let _span = tracing::trace_span!("rusqlite.connection.open").entered();
        {
            let path = sqlite_host_path(path);
            let flags = match mode {
                ConnectionMode::Reader => OpenFlags::SQLITE_OPEN_READ_ONLY,
                ConnectionMode::Writer => OpenFlags::SQLITE_OPEN_READ_WRITE,
            } | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE;
            Connection::open_with_flags(&path, flags).map_err(|source| policy("open", source))
        }
    }
}

/// Configuration is measured apart from the raw open: `journal_mode = WAL`
/// and the verification pragmas read and can write the database, so under a
/// held write lock this phase, not the file open, is where a slow
/// startup's time goes.
fn finish_open(
    connection: Connection,
    mode: ConnectionMode,
) -> Result<Connection, ConnectionPolicyError> {
    {
        let _span = tracing::trace_span!("rusqlite.connection.configure").entered();
        {
            apply_policy(&connection, mode)?;
            Ok(connection)
        }
    }
}

/// Engine policy first, so no later pragma waits on a lock; then the mode's
/// pragmas, verified; then the mode's capability.
fn apply_policy(
    connection: &Connection,
    mode: ConnectionMode,
) -> Result<(), ConnectionPolicyError> {
    connection
        .apply_engine_policy()
        .map_err(|source| policy("engine policy", source))?;
    connection
        .pragma_update(None, "foreign_keys", true)
        .map_err(|source| policy("foreign keys", source))?;

    if mode == ConnectionMode::Writer {
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(|source| policy("WAL journal", source))?;
        connection
            .pragma_update(None, "wal_autocheckpoint", 0_i64)
            .map_err(|source| policy("WAL auto-checkpoint", source))?;
    }
    // Applied after the journal mode is settled, on the writer: it owns every
    // checkpoint. Readers never reset a log, so bounding one there would be
    // inert.
    match mode {
        ConnectionMode::Writer => connection
            .pragma_update(None, "journal_size_limit", RETAINED_WAL_BYTES)
            .map_err(|source| policy("retained WAL bytes", source))?,
        ConnectionMode::Reader => connection
            .pragma_update(None, "query_only", true)
            .map_err(|source| policy("query-only reader", source))?,
    }

    verify_pragma_i64(connection, "foreign_keys", 1)?;
    match mode {
        ConnectionMode::Writer => {
            verify_pragma_text(connection, "journal_mode", "wal")?;
            verify_pragma_i64(connection, "wal_autocheckpoint", 0)?;
            verify_pragma_i64(connection, "journal_size_limit", RETAINED_WAL_BYTES)?;
        }
        ConnectionMode::Reader => verify_pragma_i64(connection, "query_only", 1)?,
    }
    connection
        .set_capability(mode.capability())
        .map_err(|source| policy("capability", source))
}

/// Levels a point lookup reads in one store B-tree. The narrowest interior
/// fanout these schemas produce is about 45 cells per 4 KiB page (64-hex
/// digest keys), and four levels of it address 45³ ≈ 91k leaf pages, about
/// 370 MB in a single tree.
const BTREE_LEVEL_BOUND: i64 = 4;

/// The writer connection's page cache: one root-to-leaf path for every B-tree
/// the store's admitted schema holds, so the interior pages an ingest looks up
/// on every message stay resident between messages. SQLite's fixed default
/// is smaller than that working set once a session store grows.
pub(crate) struct WriterPageCache {
    floor_pages: i64,
    fitted_schema_version: Option<i64>,
}

impl WriterPageCache {
    /// Captures SQLite's default cache as the floor, then fits the cache to
    /// the schema the store holds at open.
    pub(crate) fn new(connection: &Connection) -> Result<Self, ConnectionPolicyError> {
        let configured: i64 = connection
            .pragma_query_value(None, "cache_size", |row| row.get(0))
            .map_err(|source| policy("page cache default", source))?;
        let page_size: i64 = connection
            .pragma_query_value(None, "page_size", |row| row.get(0))
            .map_err(|source| policy("page size", source))?;
        // A negative cache_size is a budget in KiB, a positive one in pages.
        let floor_pages = if configured < 0 {
            configured.saturating_neg().saturating_mul(1024) / page_size.max(1)
        } else {
            configured
        };
        let mut cache = Self {
            floor_pages,
            fitted_schema_version: None,
        };
        cache.fit(connection)?;
        Ok(cache)
    }

    /// Re-derives the cache when the committed schema differs from the one it
    /// was last sized for: the schema install that follows a fresh open, and
    /// any later admitted table or index.
    pub(crate) fn fit(&mut self, connection: &Connection) -> Result<(), ConnectionPolicyError> {
        let schema_version: i64 = connection
            .prepare_cached("PRAGMA schema_version")
            .and_then(|mut statement| statement.query_row([], |row| row.get(0)))
            .map_err(|source| policy("schema version", source))?;
        if self.fitted_schema_version == Some(schema_version) {
            return Ok(());
        }
        let btrees: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE rootpage > 0",
                [],
                |row| row.get(0),
            )
            .map_err(|source| policy("admitted B-trees", source))?;
        let pages = btrees
            .saturating_mul(BTREE_LEVEL_BOUND)
            .max(self.floor_pages);
        // Writer SQL may never resize the cache; only this policy may.
        connection
            .with_capability(Capability::Maintenance, Capability::Writer, |connection| {
                connection.pragma_update(None, "cache_size", pages)
            })
            .map_err(|source| policy("page cache size", source))?;
        self.fitted_schema_version = Some(schema_version);
        Ok(())
    }
}

fn verify_pragma_i64(
    connection: &Connection,
    name: &'static str,
    expected: i64,
) -> Result<(), ConnectionPolicyError> {
    let actual: i64 = connection
        .pragma_query_value(None, name, |row| row.get(0))
        .map_err(|source| policy("pragma verification", source))?;
    if actual != expected {
        return Err(policy(
            "pragma verification",
            tracedecay_sql::Error::InvalidParameterName(format!(
                "{name}={actual}, expected {expected}"
            )),
        ));
    }
    Ok(())
}

fn verify_pragma_text(
    connection: &Connection,
    name: &'static str,
    expected: &str,
) -> Result<(), ConnectionPolicyError> {
    let actual: String = connection
        .pragma_query_value(None, name, |row| row.get(0))
        .map_err(|source| policy("pragma verification", source))?;
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(policy(
            "pragma verification",
            tracedecay_sql::Error::InvalidParameterName(format!(
                "{name}={actual}, expected {expected}"
            )),
        ));
    }
    Ok(())
}

fn policy(stage: &'static str, source: tracedecay_sql::Error) -> ConnectionPolicyError {
    ConnectionPolicyError { stage, source }
}

#[cfg(test)]
mod tests;
