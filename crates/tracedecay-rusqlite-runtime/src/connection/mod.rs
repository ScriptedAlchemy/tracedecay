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

const PROGRESS_INTERVAL_OPS: i32 = 1_000;

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
        let metadata = file
            .metadata()
            .map_err(|_| OpenedDatabaseFileError::Inspect)?;
        if !metadata.is_file() {
            return Err(OpenedDatabaseFileError::NotFile);
        }
        let identity = opened_file_identity(&file)?;
        Ok(Self { file, identity })
    }

    pub(crate) fn create_new(path: &Path) -> Result<Self, OpenedDatabaseFileError> {
        let file = create_pinned_database(path).map_err(|_| OpenedDatabaseFileError::Create)?;
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

    #[cfg(any(unix, windows))]
    pub(crate) fn worker_open_path(
        &self,
        canonical_path: &Path,
    ) -> Result<PathBuf, OpenedDatabaseFileError> {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;

            let descriptor = self.file.as_raw_fd();
            #[cfg(any(target_os = "linux", target_os = "android"))]
            let path = PathBuf::from(format!("/proc/self/fd/{descriptor}"));
            #[cfg(not(any(target_os = "linux", target_os = "android")))]
            let path = PathBuf::from(format!("/dev/fd/{descriptor}"));
            let _ = canonical_path;
            Ok(path)
        }
        #[cfg(windows)]
        {
            Ok(canonical_path.to_path_buf())
        }
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

    #[cfg(not(any(unix, windows)))]
    pub(crate) fn worker_open_path(
        &self,
        _canonical_path: &Path,
    ) -> Result<PathBuf, OpenedDatabaseFileError> {
        Err(OpenedDatabaseFileError::Unsupported)
    }

    #[cfg(not(any(unix, windows)))]
    pub(crate) fn writer_open_path(
        &self,
        _canonical_path: &Path,
    ) -> Result<PathBuf, OpenedDatabaseFileError> {
        Err(OpenedDatabaseFileError::Unsupported)
    }

    pub(crate) fn clone_file(&self) -> Result<File, OpenedDatabaseFileError> {
        self.file
            .try_clone()
            .map_err(|_| OpenedDatabaseFileError::Open)
    }

    pub(crate) fn sync_all(&self) -> Result<(), OpenedDatabaseFileError> {
        self.file
            .sync_all()
            .map_err(|_| OpenedDatabaseFileError::Inspect)
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
        connection: &Connection,
        canonical_path: &Path,
    ) -> Result<(), OpenedDatabaseFileError> {
        // Check the pathname identity first. If it changes during this stat,
        // HAS_MOVED below still observes the SQLite handle's different inode.
        self.verify_current_path(canonical_path)?;
        #[cfg(unix)]
        if sqlite_connection_has_moved(connection)? {
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

fn open_raw(
    path: &Path,
    mode: ConnectionMode,
) -> Result<(Connection, bool), ConnectionPolicyError> {
    let fresh_writer = mode == ConnectionMode::Writer
        && std::fs::metadata(path).is_ok_and(|metadata| metadata.len() == 0);
    let flags = match mode {
        ConnectionMode::Reader => OpenFlags::SQLITE_OPEN_READ_ONLY,
        ConnectionMode::Writer | ConnectionMode::Maintenance => OpenFlags::SQLITE_OPEN_READ_WRITE,
    } | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE;
    let connection =
        Connection::open_with_flags(path, flags).map_err(|source| policy("open", source))?;

    Ok((connection, fresh_writer))
}

fn finish_open(
    connection: Connection,
    mode: ConnectionMode,
    fresh_writer: bool,
) -> Result<Connection, ConnectionPolicyError> {
    apply_pragmas(&connection, mode, fresh_writer)?;
    assert_compile_options(&connection)?;
    apply_limits(&connection, mode)?;
    install_authorizer(&connection, mode)?;
    Ok(connection)
}

/// Opens an immutable, query-only connection for a foreign or health database.
///
/// Uses `file:…?immutable=1&mode=ro` so diagnosis never creates WAL/SHM
/// sidecars or acquires authority locks. The caller owns the source-specific
/// policy for a non-empty WAL: reject it when a complete current snapshot is
/// mandatory, or accept eventual main-file visibility for best-effort foreign
/// ingestion.
pub fn open_immutable_reader(path: &Path) -> Result<Connection, ConnectionPolicyError> {
    let uri = immutable_health_uri(path)?;
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE;
    let connection =
        Connection::open_with_flags(uri, flags).map_err(|source| policy("open", source))?;
    apply_pragmas(&connection, ConnectionMode::Reader, false)?;
    assert_compile_options(&connection)?;
    apply_limits(&connection, ConnectionMode::Reader)?;
    install_authorizer(&connection, ConnectionMode::Reader)?;
    Ok(connection)
}

/// Doctor compatibility name for the canonical immutable reader policy.
///
/// Doctor rejects non-empty WAL families before calling this function.
pub fn open_immutable_health_reader(path: &Path) -> Result<Connection, ConnectionPolicyError> {
    open_immutable_reader(path)
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
        }
        ConnectionMode::Reader => verify_pragma_i64(connection, "query_only", 1)?,
        ConnectionMode::Maintenance => {}
    }
    Ok(())
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
