//! Engine-neutral types and the control surface both engine arms implement.

use std::{fs::File, io};

/// The owned-store engine compiled into this binary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Sqlite,
    Native,
}

/// Engine-neutral error classification. Callers branch on this instead of
/// SQLite result codes, which the native engine does not produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Busy,
    Constraint,
    Interrupted,
    ReadOnly,
    Schema,
    Denied,
    /// A statement inside a [`ConnectionControl::with_capability`] scope tried
    /// to begin, end or (outside its lease) nest a transaction.
    TransactionControlDenied,
    NoRows,
    Unsupported,
    Other,
    /// The file is not a TraceDecay native store (wrong header or
    /// application id). Refused before the engine touches it.
    ForeignFormat,
    /// COMMIT was stepped and then failed: the transaction may or may not be
    /// durable. Callers settle by receipt lookup.
    CommitOutcomeUnknown,
}

pub trait ErrorExt {
    fn kind(&self) -> ErrorKind;

    /// SQLite's `(primary, extended)` result codes. Exact on the SQLite arm;
    /// derived from [`ErrorExt::kind`] elsewhere, until callers that persist
    /// codes match on the kind instead.
    fn sqlite_codes(&self) -> Option<(i32, i32)> {
        let code = match self.kind() {
            ErrorKind::Busy => 5,
            ErrorKind::Interrupted => 9,
            ErrorKind::ReadOnly => 8,
            ErrorKind::Schema => 17,
            ErrorKind::Constraint => 19,
            ErrorKind::Denied | ErrorKind::TransactionControlDenied => 23,
            ErrorKind::ForeignFormat => 26,
            ErrorKind::NoRows
            | ErrorKind::Unsupported
            | ErrorKind::Other
            | ErrorKind::CommitOutcomeUnknown => return None,
        };
        Some((code, code))
    }
}

/// What statements compiled on a connection may do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Capability {
    /// Queries and read-only introspection pragmas.
    Reader,
    /// Caller-supplied SQL on a reader: [`Capability::Reader`] without
    /// transaction control or savepoints, which would end the reader's
    /// snapshot.
    ExactSqlRead,
    /// DML plus idempotent `CREATE TABLE`/`CREATE INDEX` for ledger bootstrap.
    /// Triggers, views, drops, temp objects, ATTACH and protected pragmas are
    /// denied.
    Writer,
    /// Caller-supplied SQL on the writer. Durable DDL, temp tables and temp
    /// indexes are allowed. Temp triggers and views, ATTACH, `load_extension`,
    /// pragmas outside the exact SQL allowlist and transaction control are
    /// denied; savepoints only when the caller holds a transaction lease.
    ExactSql { savepoints: bool },
    /// Schema install, repair and maintenance: everything except ATTACH and
    /// DETACH.
    Maintenance,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckpointMode {
    /// Reports the WAL's frame counts without copying any frame back.
    Noop,
    Passive,
    Full,
    Restart,
    Truncate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckpointOutcome {
    pub busy: bool,
    pub log_frames: i64,
    pub checkpointed_frames: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageStats {
    pub page_size: u64,
    pub page_count: u64,
    pub freelist_count: u64,
}

/// The identity of an open file: (st_dev, st_ino) on Unix, (volume serial,
/// file index) on Windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileIdentity {
    pub volume: u64,
    pub file: u64,
}

pub trait ConnectionControl {
    /// Installs `capability` as this connection's statement policy.
    fn set_capability(&self, capability: Capability) -> crate::Result<()>;

    /// Runs `operation` under `scoped`, then reinstalls `base` on success,
    /// error and unwind. No statement compiled under `scoped` runs under
    /// `base` without being authorized again. A transaction-control statement
    /// denied inside the scope makes the scope fail with
    /// [`ErrorKind::TransactionControlDenied`] whatever `operation` returned.
    fn with_capability<T>(
        &self,
        scoped: Capability,
        base: Capability,
        operation: impl FnOnce(&Self) -> crate::Result<T>,
    ) -> crate::Result<T>;

    /// Runs `operation` while `should_interrupt` is polled every
    /// [`PROGRESS_INTERVAL_OPS`] VM operations; `true` interrupts the running
    /// statement. The poll is removed on every exit path.
    fn guard_scope<T>(
        &self,
        should_interrupt: impl FnMut() -> bool + Send + 'static,
        operation: impl FnOnce(&Self) -> T,
    ) -> crate::Result<T>;

    fn checkpoint(&self, mode: CheckpointMode) -> crate::Result<CheckpointOutcome>;

    fn page_stats(&self) -> crate::Result<PageStats>;

    /// Whether the main database file's pathname no longer names the file the
    /// engine holds open.
    fn file_has_moved(&self) -> crate::Result<bool>;
}

/// [`ConnectionControl::guard_scope`] for a transaction, whose operation needs
/// the transaction itself to open savepoints.
pub trait TransactionControl {
    fn guard_scope_mut<T>(
        &mut self,
        should_interrupt: impl FnMut() -> bool + Send + 'static,
        operation: impl FnOnce(&mut Self) -> T,
    ) -> crate::Result<T>;
}

/// Engine settings every owned-store connection carries: zero busy wait, no
/// checkpoint on close, resource limits, the statement-cache size and the
/// engine build checks.
pub trait EnginePolicy {
    fn apply_engine_policy(&self) -> crate::Result<()>;
}

/// Per-statement work counters, reset when read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatementCounters {
    pub vm_steps: u64,
    pub fullscan_steps: u64,
    pub sort_steps: u64,
    /// Times the statement was compiled again because the schema or the
    /// capability changed under it.
    pub reprepares: u64,
}

pub trait StatementControl {
    fn take_counters(&self) -> StatementCounters;
}

pub const PROGRESS_INTERVAL_OPS: i32 = 1_000;

#[cfg(unix)]
pub fn file_identity(file: &File) -> io::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt;

    let metadata = file.metadata()?;
    Ok(FileIdentity {
        volume: metadata.dev(),
        file: metadata.ino(),
    })
}

#[cfg(windows)]
pub fn file_identity(file: &File) -> io::Result<FileIdentity> {
    use std::{mem::MaybeUninit, os::windows::io::AsRawHandle};

    #[repr(C)]
    struct ByHandleFileInformation {
        _file_attributes: u32,
        _creation_time: [u32; 2],
        _last_access_time: [u32; 2],
        _last_write_time: [u32; 2],
        volume_serial_number: u32,
        _file_size: [u32; 2],
        _number_of_links: u32,
        file_index_high: u32,
        file_index_low: u32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[link_name = "GetFileInformationByHandle"]
        fn get_file_information_by_handle(
            file: *mut std::ffi::c_void,
            information: *mut ByHandleFileInformation,
        ) -> i32;
    }

    let mut information = MaybeUninit::<ByHandleFileInformation>::uninit();
    // SAFETY: `file` owns a valid handle and `information` is writable storage
    // for the API's complete output structure.
    if unsafe { get_file_information_by_handle(file.as_raw_handle(), information.as_mut_ptr()) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a nonzero result initializes every output field.
    let information = unsafe { information.assume_init() };
    Ok(FileIdentity {
        volume: u64::from(information.volume_serial_number),
        file: (u64::from(information.file_index_high) << 32)
            | u64::from(information.file_index_low),
    })
}

#[cfg(not(any(unix, windows)))]
pub fn file_identity(_file: &File) -> io::Result<FileIdentity> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "file identity is unsupported on this platform",
    ))
}

/// The pragmas [`Capability::ExactSql`] may run, on either engine.
pub(crate) fn is_allowed_exact_sql_pragma(pragma_name: &str, pragma_value: Option<&str>) -> bool {
    is_exact_sql_read_pragma(pragma_name, pragma_value)
        || (pragma_value.is_none() && pragma_name.eq_ignore_ascii_case("shrink_memory"))
        || pragma_value.is_some_and(|value| {
            (pragma_name.eq_ignore_ascii_case("foreign_keys")
                && (value.eq_ignore_ascii_case("on") || value == "1"))
                || (pragma_name.eq_ignore_ascii_case("defer_foreign_keys")
                    && (value.eq_ignore_ascii_case("on") || value == "1"))
                || (pragma_name.eq_ignore_ascii_case("busy_timeout")
                    && value.parse::<u32>().is_ok())
                || (pragma_name.eq_ignore_ascii_case("secure_delete")
                    && (value.eq_ignore_ascii_case("on") || value == "1"))
                || (pragma_name.eq_ignore_ascii_case("user_version")
                    && value.parse::<u32>().is_ok())
                || (pragma_name.eq_ignore_ascii_case("wal_autocheckpoint")
                    && value.parse::<u32>().is_ok())
        })
}

fn is_exact_sql_read_pragma(pragma_name: &str, pragma_value: Option<&str>) -> bool {
    const ARGUMENT_SAFE: &[&str] = &[
        "foreign_key_check",
        "foreign_key_list",
        "index_info",
        "index_list",
        "index_xinfo",
        "integrity_check",
        "quick_check",
        "table_info",
        "table_list",
        "table_xinfo",
    ];
    const NO_ARGUMENT_ONLY: &[&str] = &[
        "application_id",
        "auto_vacuum",
        "busy_timeout",
        "cache_size",
        "collation_list",
        "compile_options",
        "data_version",
        "database_list",
        "defer_foreign_keys",
        "foreign_keys",
        "freelist_count",
        "function_list",
        "journal_mode",
        "mmap_size",
        "module_list",
        "page_count",
        "page_size",
        "pragma_list",
        "query_only",
        "recursive_triggers",
        "schema_version",
        "secure_delete",
        "synchronous",
        "temp_store",
        "user_version",
        "wal_autocheckpoint",
    ];

    ARGUMENT_SAFE
        .iter()
        .any(|candidate| pragma_name.eq_ignore_ascii_case(candidate))
        || (pragma_value.is_none()
            && NO_ARGUMENT_ONLY
                .iter()
                .any(|candidate| pragma_name.eq_ignore_ascii_case(candidate)))
}
