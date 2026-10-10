#[cfg(unix)]
use std::ffi::CString;
use std::{
    fmt,
    fs::{File, OpenOptions},
    io,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    path::{Path, PathBuf},
    time::Duration,
};

use rusqlite::{
    Connection, OpenFlags, Transaction,
    config::DbConfig,
    hooks::{AuthAction, AuthContext, Authorization},
    limits::Limit,
};
use sha2::{Digest, Sha256};
use tracedecay_store::WAL_SOFT_LIMIT_BYTES;

const PROGRESS_INTERVAL_OPS: i32 = 1_000;

/// Writer and reader connections are single-threaded actors, so rusqlite's
/// per-connection statement cache is the reuse path for exact-SQL execute
/// and query. Sixteen (the rusqlite default) is too small for the distinct
/// statements a store issues; 128 covers the repeated catalog without
/// holding an unbounded compile cache.
const PREPARED_STATEMENT_CACHE_CAPACITY: usize = 128;

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
pub(crate) struct OpenedDatabaseFile {
    file: File,
    identity: u64,
}

impl OpenedDatabaseFile {
    pub(crate) fn pin(path: &Path) -> Result<Self, OpenedDatabaseFileError> {
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
        let identity = opened_file_identity(&file)?;
        Ok(Self { file, identity })
    }

    pub(crate) const fn identity(&self) -> u64 {
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
    pub(crate) fn reader_open_path(
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

    pub(crate) fn verify_current_path(&self, path: &Path) -> Result<(), OpenedDatabaseFileError> {
        let current = File::open(path).map_err(|_| OpenedDatabaseFileError::Open)?;
        if opened_file_identity(&current)? != self.identity {
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
        // HAS_MOVED below still observes the SQLite handle's different inode.
        self.verify_current_path(canonical_path)?;
        #[cfg(unix)]
        if sqlite_connection_has_moved(_connection)? {
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

#[cfg(unix)]
fn opened_file_identity(file: &File) -> Result<u64, OpenedDatabaseFileError> {
    use std::os::unix::fs::MetadataExt;

    let metadata = file
        .metadata()
        .map_err(|_| OpenedDatabaseFileError::Inspect)?;
    let mut hasher = Sha256::new();
    hasher.update(metadata.dev().to_le_bytes());
    hasher.update(metadata.ino().to_le_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    Ok(u64::from_le_bytes(bytes).max(1))
}

#[cfg(unix)]
fn sqlite_connection_has_moved(connection: &Connection) -> Result<bool, OpenedDatabaseFileError> {
    let database_name = CString::new("main").expect("static SQLite database name");
    let mut moved = 0_i32;
    // SAFETY: `connection` owns a live SQLite handle, `database_name` is a
    // NUL-terminated database name, and `moved` is writable storage for the
    // integer required by SQLITE_FCNTL_HAS_MOVED.
    let result = unsafe {
        rusqlite::ffi::sqlite3_file_control(
            connection.handle(),
            database_name.as_ptr(),
            rusqlite::ffi::SQLITE_FCNTL_HAS_MOVED,
            (&mut moved as *mut i32).cast(),
        )
    };
    match result {
        rusqlite::ffi::SQLITE_OK => Ok(moved != 0),
        // VFS implementations predating HAS_MOVED report NOTFOUND. The
        // pinned dev/inode check remains authoritative in that case.
        rusqlite::ffi::SQLITE_NOTFOUND => Ok(false),
        _ => Err(OpenedDatabaseFileError::Inspect),
    }
}

#[cfg(windows)]
fn opened_file_identity(file: &File) -> Result<u64, OpenedDatabaseFileError> {
    use std::mem::MaybeUninit;
    use std::os::windows::io::AsRawHandle;

    let mut information = MaybeUninit::<ByHandleFileInformation>::uninit();
    // SAFETY: `file` owns a valid Windows file handle and `information` is
    // writable storage for the API's complete output structure.
    let succeeded =
        unsafe { get_file_information_by_handle(file.as_raw_handle(), information.as_mut_ptr()) };
    if succeeded == 0 {
        return Err(OpenedDatabaseFileError::Identify);
    }
    // SAFETY: A nonzero API result initializes every output field.
    let information = unsafe { information.assume_init() };
    let mut hasher = Sha256::new();
    hasher.update(b"windows-file-id");
    hasher.update(information.volume_serial_number.to_le_bytes());
    hasher.update(
        ((u64::from(information.file_index_high) << 32) | u64::from(information.file_index_low))
            .to_le_bytes(),
    );
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    Ok(u64::from_le_bytes(bytes).max(1))
}

#[cfg(windows)]
#[repr(C)]
struct ByHandleFileInformation {
    _file_attributes: u32,
    _creation_time_low_date_time: u32,
    _creation_time_high_date_time: u32,
    _last_access_time_low_date_time: u32,
    _last_access_time_high_date_time: u32,
    _last_write_time_low_date_time: u32,
    _last_write_time_high_date_time: u32,
    volume_serial_number: u32,
    _file_size_high: u32,
    _file_size_low: u32,
    _number_of_links: u32,
    file_index_high: u32,
    file_index_low: u32,
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    #[link_name = "GetFileInformationByHandle"]
    fn get_file_information_by_handle(
        file: *mut std::ffi::c_void,
        information: *mut ByHandleFileInformation,
    ) -> i32;
}

#[cfg(not(any(unix, windows)))]
fn opened_file_identity(_file: &File) -> Result<u64, OpenedDatabaseFileError> {
    Err(OpenedDatabaseFileError::Unsupported)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConnectionMode {
    Writer,
    Reader,
    Maintenance,
}

#[derive(Debug)]
pub(crate) enum WriterOpenError {
    Policy(ConnectionPolicyError),
    Identity(OpenedDatabaseFileError),
}

#[derive(Debug)]
pub struct ConnectionPolicyError {
    stage: &'static str,
    source: rusqlite::Error,
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

#[derive(Debug)]
pub enum VerifiedReaderError {
    Resolve(io::Error),
    Identity(OpenedDatabaseFileError),
    Policy(ConnectionPolicyError),
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

/// Foreign-database reader bound to the physical file actually
/// opened by SQLite rather than a later pathname observation.
pub struct VerifiedReader {
    _opened_file: OpenedDatabaseFile,
    connection: Connection,
    canonical_path: PathBuf,
    file_identity: u64,
}

impl VerifiedReader {
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    pub const fn file_identity(&self) -> u64 {
        self.file_identity
    }

    pub fn into_parts(self) -> (Connection, PathBuf, u64) {
        (self.connection, self.canonical_path, self.file_identity)
    }
}

pub(crate) fn open(path: &Path, mode: ConnectionMode) -> Result<Connection, ConnectionPolicyError> {
    let (connection, fresh_writer) = open_raw(path, mode)?;
    finish_open(connection, mode, fresh_writer)
}

pub(crate) fn open_writer(
    path: &Path,
    opened_database: Option<&OpenedDatabaseFile>,
    canonical_path: &Path,
) -> Result<Connection, WriterOpenError> {
    let (connection, fresh_writer) =
        open_raw(path, ConnectionMode::Writer).map_err(WriterOpenError::Policy)?;
    if let Some(opened_database) = opened_database {
        opened_database
            .verify_connection(&connection, canonical_path)
            .map_err(WriterOpenError::Identity)?;
    }
    let connection = finish_open(connection, ConnectionMode::Writer, fresh_writer)
        .map_err(WriterOpenError::Policy)?;
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
/// Only a verbatim *disk* path is shortened, and only when the conversion
/// cannot change which object the name resolves to. Removing the prefix
/// switches on Win32 name parsing — trailing dots/spaces are stripped, `.`
/// and `..` components are resolved, `/` becomes a separator, and reserved
/// DOS-device components get special treatment — so any component with those
/// semantics keeps the verbatim spelling rather than risk opening a different
/// file. The classic `CreateFileW` limit is checked in UTF-16 code units and
/// leaves room for SQLite's `-wal`/`-shm` sidecars.
fn sqlite_host_path(path: &Path) -> PathBuf {
    // Only Windows gives `\\?\` verbatim-path semantics; elsewhere that byte
    // sequence is a legitimate relative filename and must pass through.
    if !cfg!(windows) {
        return path.to_path_buf();
    }
    let Some(text) = path.to_str() else {
        return path.to_path_buf();
    };
    let Some(rest) = text.strip_prefix(r"\\?\") else {
        return path.to_path_buf();
    };
    let bytes = rest.as_bytes();
    let is_verbatim_disk =
        bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\';
    if is_verbatim_disk
        && rest.encode_utf16().count() + 4 <= 259
        && rest.split('\\').all(win32_name_stable_component)
    {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

/// Whether a single path component reads identically under verbatim and
/// classic Win32 name parsing. Compiled on every host because the reference
/// in `sqlite_host_path` is not cfg-gated, though the `cfg!(windows)` guard
/// short-circuits before it runs.
fn win32_name_stable_component(component: &str) -> bool {
    // Reserved DOS device names are matched on the part before the first
    // '.', per the Win32 namespace rules.
    const DOS_DEVICES: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
        "COM8", "COM9", "COM\u{b9}", "COM\u{b2}", "COM\u{b3}", "LPT0", "LPT1", "LPT2", "LPT3",
        "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "LPT\u{b9}", "LPT\u{b2}", "LPT\u{b3}",
    ];
    if component.is_empty()
        || component == "."
        || component == ".."
        || component.ends_with('.')
        || component.ends_with(' ')
        || component.contains('/')
    {
        return false;
    }
    let stem = component.split('.').next().unwrap_or(component);
    !DOS_DEVICES
        .iter()
        .any(|device| stem.eq_ignore_ascii_case(device))
}

fn open_raw(
    path: &Path,
    mode: ConnectionMode,
) -> Result<(Connection, bool), ConnectionPolicyError> {
    {
        let _span = tracing::trace_span!("rusqlite.connection.open").entered();
        {
            let fresh_writer = mode == ConnectionMode::Writer
                && std::fs::metadata(path).is_ok_and(|metadata| metadata.len() == 0);
            let path = sqlite_host_path(path);
            let flags = match mode {
                ConnectionMode::Reader => OpenFlags::SQLITE_OPEN_READ_ONLY,
                ConnectionMode::Writer | ConnectionMode::Maintenance => {
                    OpenFlags::SQLITE_OPEN_READ_WRITE
                }
            } | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE;
            let connection = Connection::open_with_flags(&path, flags)
                .map_err(|source| policy("open", source))?;

            Ok((connection, fresh_writer))
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
    fresh_writer: bool,
) -> Result<Connection, ConnectionPolicyError> {
    {
        let _span = tracing::trace_span!("rusqlite.connection.configure").entered();
        {
            apply_pragmas(&connection, mode, fresh_writer)?;
            assert_compile_options(&connection)?;
            apply_limits(&connection, mode)?;
            connection.set_prepared_statement_cache_capacity(PREPARED_STATEMENT_CACHE_CAPACITY);
            install_authorizer(&connection, mode)?;
            Ok(connection)
        }
    }
}

/// Opens a verified live database in a retained, query-only SQLite read transaction.
/// The caller bounds its lifetime; dropping the connection releases the snapshot.
pub fn open_verified_read_snapshot(path: &Path) -> Result<VerifiedReader, VerifiedReaderError> {
    open_verified_reader(
        path,
        |path| {
            let connection = open(path, ConnectionMode::Reader)?;
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

/// Opens an immutable, query-only connection for a foreign or health database.
///
/// Uses `file:…?immutable=1&mode=ro` so diagnosis never creates WAL/SHM
/// sidecars or acquires authority locks. The caller owns the source-specific
/// policy for a non-empty WAL: reject it when a complete current snapshot is
/// mandatory, or accept eventual main-file visibility for best-effort foreign
/// ingestion.
pub fn open_immutable_reader(path: &Path) -> Result<Connection, ConnectionPolicyError> {
    {
        let _span = tracing::trace_span!("rusqlite.connection.open_immutable").entered();
        {
            let uri = immutable_health_uri(&sqlite_host_path(path))?;
            let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE;
            let connection =
                Connection::open_with_flags(uri, flags).map_err(|source| policy("open", source))?;
            apply_pragmas(&connection, ConnectionMode::Reader, false)?;
            assert_compile_options(&connection)?;
            apply_limits(&connection, ConnectionMode::Reader)?;
            connection.set_prepared_statement_cache_capacity(PREPARED_STATEMENT_CACHE_CAPACITY);
            install_authorizer(&connection, ConnectionMode::Reader)?;
            Ok(connection)
        }
    }
}

pub fn open_verified_immutable_reader(path: &Path) -> Result<VerifiedReader, VerifiedReaderError> {
    open_verified_reader(path, open_immutable_reader, || {}, || {})
}

fn open_verified_reader(
    path: &Path,
    open_reader: impl FnOnce(&Path) -> Result<Connection, ConnectionPolicyError>,
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
    pinned
        .verify_connection(&connection, &canonical_path)
        .map_err(VerifiedReaderError::Identity)?;
    Ok(VerifiedReader {
        connection,
        canonical_path,
        file_identity: pinned.identity(),
        _opened_file: pinned,
    })
}

fn immutable_health_uri(path: &Path) -> Result<String, ConnectionPolicyError> {
    #[cfg(unix)]
    let raw = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes()
    };
    #[cfg(not(unix))]
    let raw = path
        .to_str()
        .ok_or_else(|| ConnectionPolicyError {
            stage: "immutable uri",
            source: rusqlite::Error::InvalidPath(path.to_path_buf()),
        })?
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

fn apply_pragmas(
    connection: &Connection,
    mode: ConnectionMode,
    fresh_writer: bool,
) -> Result<(), ConnectionPolicyError> {
    // SQLite must never wait past the runtime's own queue/deadline authority.
    connection
        .busy_timeout(Duration::ZERO)
        .map_err(|source| policy("busy timeout", source))?;
    connection
        .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
        .map_err(|source| policy("checkpoint-on-close", source))?;
    connection
        .pragma_update(None, "foreign_keys", true)
        .map_err(|source| policy("foreign keys", source))?;
    connection
        .pragma_update(None, "trusted_schema", false)
        .map_err(|source| policy("trusted schema", source))?;

    if mode == ConnectionMode::Writer {
        if fresh_writer {
            connection
                .pragma_update(None, "auto_vacuum", "INCREMENTAL")
                .map_err(|source| policy("fresh auto-vacuum", source))?;
            verify_pragma_i64(connection, "auto_vacuum", 2)?;
        }
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(|source| policy("WAL journal", source))?;
        connection
            .pragma_update(None, "wal_autocheckpoint", 0_i64)
            .map_err(|source| policy("WAL auto-checkpoint", source))?;
        connection
            .pragma_update(None, "synchronous", "NORMAL")
            .map_err(|source| policy("synchronous mode", source))?;
    }
    // Applied after the journal mode is settled, on every lane that can reset
    // the log: the writer owns scheduled checkpoints, maintenance owns the
    // offline RESTART/TRUNCATE path. Readers never reset a log, so bounding
    // one there would be inert.
    if mode == ConnectionMode::Writer || mode == ConnectionMode::Maintenance {
        connection
            .pragma_update(None, "journal_size_limit", RETAINED_WAL_BYTES)
            .map_err(|source| policy("retained WAL bytes", source))?;
    }
    if mode == ConnectionMode::Reader {
        connection
            .pragma_update(None, "query_only", true)
            .map_err(|source| policy("query-only reader", source))?;
    }

    verify_pragma_i64(connection, "foreign_keys", 1)?;
    verify_pragma_i64(connection, "trusted_schema", 0)?;
    if !connection
        .db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE)
        .map_err(|source| policy("checkpoint-on-close verification", source))?
    {
        return Err(policy(
            "checkpoint-on-close verification",
            rusqlite::Error::InvalidParameterName(
                "SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE=false, expected true".to_owned(),
            ),
        ));
    }
    match mode {
        ConnectionMode::Writer => {
            verify_pragma_text(connection, "journal_mode", "wal")?;
            verify_pragma_i64(connection, "wal_autocheckpoint", 0)?;
            verify_pragma_i64(connection, "synchronous", 1)?;
            verify_pragma_i64(connection, "journal_size_limit", RETAINED_WAL_BYTES)?;
        }
        ConnectionMode::Reader => verify_pragma_i64(connection, "query_only", 1)?,
        ConnectionMode::Maintenance => {
            verify_pragma_i64(connection, "journal_size_limit", RETAINED_WAL_BYTES)?;
        }
    }
    Ok(())
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
            .authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
            .map_err(|source| policy("page cache authorizer", source))?;
        let resized = connection.pragma_update(None, "cache_size", pages);
        connection
            .authorizer(Some(authorize_writer))
            .map_err(|source| policy("restore writer authorizer", source))?;
        resized.map_err(|source| policy("page cache size", source))?;
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
            rusqlite::Error::InvalidParameterName(format!("{name}={actual}, expected {expected}")),
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
            rusqlite::Error::InvalidParameterName(format!("{name}={actual}, expected {expected}")),
        ));
    }
    Ok(())
}

fn assert_compile_options(connection: &Connection) -> Result<(), ConnectionPolicyError> {
    let mut statement = connection
        .prepare("PRAGMA compile_options")
        .map_err(|source| policy("compile options", source))?;
    let options = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|source| policy("compile options", source))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|source| policy("compile options", source))?;
    for required in ["ENABLE_FTS5", "THREADSAFE=1"] {
        if !options.iter().any(|option| option == required) {
            return Err(policy(
                "compile options",
                rusqlite::Error::InvalidParameterName(format!("missing {required}")),
            ));
        }
    }
    if options.iter().any(|option| option == "OMIT_FOREIGN_KEY") {
        return Err(policy(
            "compile options",
            rusqlite::Error::InvalidParameterName("OMIT_FOREIGN_KEY is unsupported".to_owned()),
        ));
    }
    Ok(())
}

fn apply_limits(
    connection: &Connection,
    mode: ConnectionMode,
) -> Result<(), ConnectionPolicyError> {
    let attached = if mode == ConnectionMode::Maintenance {
        4
    } else {
        0
    };
    for (limit, value) in [
        (Limit::SQLITE_LIMIT_LENGTH, 64 * 1024 * 1024),
        (Limit::SQLITE_LIMIT_SQL_LENGTH, 1024 * 1024),
        (Limit::SQLITE_LIMIT_COLUMN, 2_000),
        (Limit::SQLITE_LIMIT_EXPR_DEPTH, 100),
        (Limit::SQLITE_LIMIT_COMPOUND_SELECT, 100),
        (Limit::SQLITE_LIMIT_VDBE_OP, 25_000_000),
        (Limit::SQLITE_LIMIT_FUNCTION_ARG, 100),
        (Limit::SQLITE_LIMIT_ATTACHED, attached),
        (Limit::SQLITE_LIMIT_LIKE_PATTERN_LENGTH, 50_000),
        (Limit::SQLITE_LIMIT_VARIABLE_NUMBER, 32_766),
        (Limit::SQLITE_LIMIT_TRIGGER_DEPTH, 32),
        (Limit::SQLITE_LIMIT_WORKER_THREADS, 0),
    ] {
        connection
            .set_limit(limit, value)
            .map_err(|source| policy("runtime limits", source))?;
    }
    Ok(())
}

fn install_authorizer(
    connection: &Connection,
    mode: ConnectionMode,
) -> Result<(), ConnectionPolicyError> {
    let result = match mode {
        ConnectionMode::Writer => connection.authorizer(Some(authorize_writer)),
        ConnectionMode::Reader => connection.authorizer(Some(authorize_reader)),
        ConnectionMode::Maintenance => connection.authorizer(Some(authorize_maintenance)),
    };
    result.map_err(|source| policy("authorizer", source))
}

pub(crate) fn authorize_writer(context: AuthContext<'_>) -> Authorization {
    authorize(ConnectionMode::Writer, context)
}

pub(crate) fn authorize_reader(context: AuthContext<'_>) -> Authorization {
    authorize(ConnectionMode::Reader, context)
}

fn authorize_maintenance(_: AuthContext<'_>) -> Authorization {
    Authorization::Allow
}

/// Schema-introspection and integrity-diagnostic pragmas that cannot mutate the
/// database, file, or connection configuration. These stay available even to
/// read-only lanes (for example the immutable Doctor health reader) so health
/// and shape audits work without opening a writable connection.
fn is_read_only_introspection_pragma(pragma_name: &str) -> bool {
    const READ_ONLY_INTROSPECTION_PRAGMAS: &[&str] = &[
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
    ];
    READ_ONLY_INTROSPECTION_PRAGMAS
        .iter()
        .any(|candidate| pragma_name.eq_ignore_ascii_case(candidate))
}

fn is_safe_writer_pragma(pragma_name: &str, pragma_value: &str) -> bool {
    pragma_name.eq_ignore_ascii_case("busy_timeout")
        || pragma_name.eq_ignore_ascii_case("incremental_vacuum")
        || pragma_name.eq_ignore_ascii_case("wal_autocheckpoint")
        || pragma_name.eq_ignore_ascii_case("wal_checkpoint")
        || (pragma_name.eq_ignore_ascii_case("auto_vacuum")
            && (pragma_value.eq_ignore_ascii_case("incremental") || pragma_value == "2"))
}

fn authorize(mode: ConnectionMode, context: AuthContext<'_>) -> Authorization {
    if mode == ConnectionMode::Maintenance {
        return Authorization::Allow;
    }
    // Writer-mode CREATE TABLE/INDEX remains available for the closed
    // executor's idempotent ledger bootstrap. Destructive, temporary, virtual,
    // or other schema changes require the explicit Maintenance mode above.
    let denied = matches!(
        context.action,
        AuthAction::Attach { .. }
            | AuthAction::Detach { .. }
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
            | AuthAction::Unknown { .. }
    ) || matches!(context.action, AuthAction::Function { function_name } if function_name.eq_ignore_ascii_case("load_extension"))
        || matches!(
            context.action,
            AuthAction::Pragma {
                pragma_name,
                pragma_value: Some(pragma_value),
            }
            if !is_read_only_introspection_pragma(pragma_name)
                && (mode != ConnectionMode::Writer
                    || !is_safe_writer_pragma(pragma_name, pragma_value))
        )
        || (mode == ConnectionMode::Reader
            && matches!(
                context.action,
                AuthAction::Insert { .. } | AuthAction::Update { .. } | AuthAction::Delete { .. }
            ));
    if denied {
        Authorization::Deny
    } else {
        Authorization::Allow
    }
}

#[cfg(test)]
pub(crate) fn with_progress_cancellation<T, C, F>(
    connection: &mut Connection,
    should_cancel: C,
    operation: F,
) -> rusqlite::Result<T>
where
    C: FnMut() -> bool + Send + 'static,
    F: FnOnce(&mut Connection) -> T,
{
    connection.progress_handler(PROGRESS_INTERVAL_OPS, Some(should_cancel))?;
    let result = catch_unwind(AssertUnwindSafe(|| operation(connection)));
    let clear = connection.progress_handler(PROGRESS_INTERVAL_OPS, None::<fn() -> bool>);
    match result {
        Ok(value) => {
            clear?;
            Ok(value)
        }
        Err(payload) => resume_unwind(payload),
    }
}

pub(crate) fn with_transaction_progress_cancellation<'connection, T, C, F>(
    transaction: &mut Transaction<'connection>,
    should_cancel: C,
    operation: F,
) -> rusqlite::Result<T>
where
    C: FnMut() -> bool + Send + 'static,
    F: FnOnce(&mut Transaction<'connection>) -> T,
{
    transaction.progress_handler(PROGRESS_INTERVAL_OPS, Some(should_cancel))?;
    let result = catch_unwind(AssertUnwindSafe(|| operation(transaction)));
    let clear = transaction.progress_handler(PROGRESS_INTERVAL_OPS, None::<fn() -> bool>);
    match result {
        Ok(value) => {
            clear?;
            Ok(value)
        }
        Err(payload) => resume_unwind(payload),
    }
}

fn policy(stage: &'static str, source: rusqlite::Error) -> ConnectionPolicyError {
    ConnectionPolicyError { stage, source }
}

#[cfg(test)]
mod tests;
