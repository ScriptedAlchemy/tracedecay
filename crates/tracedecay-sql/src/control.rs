//! Engine-neutral types and the control surface both engine arms implement.

use std::{fs::File, io};

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
    NoRows,
    Unsupported,
    Other,
}

pub trait ErrorExt {
    fn kind(&self) -> ErrorKind;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Capability {
    /// Queries and read-only introspection pragmas.
    Reader,
    /// DML plus idempotent `CREATE TABLE`/`CREATE INDEX` for ledger bootstrap.
    /// Triggers, views, drops, temp objects, ATTACH and protected pragmas are
    /// denied.
    Writer,
    /// Schema install, repair and maintenance: everything.
    Maintenance,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckpointMode {
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
    fn set_capability(&self, capability: Capability) -> crate::Result<()>;

    /// Runs `operation` under `scoped`, then reinstalls `base` and drops cached
    /// statements compiled under `scoped`, on success, error and unwind.
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
