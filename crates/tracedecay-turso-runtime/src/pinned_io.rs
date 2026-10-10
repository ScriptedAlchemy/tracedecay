//! Adopts an already-open file through the native OS I/O backend. Main-file
//! identity comes from the held descriptor; WAL/log names retain the canonical
//! database pathname. No page, journal, or durability implementation is copied.

use std::{
    fs::File,
    os::unix::{fs::MetadataExt, io::AsRawFd},
    path::{Path, PathBuf},
    sync::Arc,
};
use turso_core::{
    IO, OpenFlags,
    io::{
        Clock, FileId,
        clock::{MonotonicInstant, WallClockInstant},
    },
};

use crate::{Error, Result};

pub(crate) struct PinnedIo {
    delegate: Arc<dyn IO>,
    path: PathBuf,
    file: File,
    identity: FileId,
}

impl PinnedIo {
    pub(crate) fn new(path: &Path, file: File) -> Result<Arc<Self>> {
        let metadata = file.metadata().map_err(|error| {
            Error::InvalidOperation(format!("inspect native held file: {error}"))
        })?;
        if !metadata.is_file() {
            return Err(Error::InvalidOperation(
                "native held object is not a regular file".to_owned(),
            ));
        }
        let path = path.canonicalize().map_err(|error| {
            Error::InvalidOperation(format!("canonicalize native held file: {error}"))
        })?;
        let path_text = path.to_str().ok_or_else(|| {
            Error::InvalidOperation("native database path is not UTF-8".to_owned())
        })?;
        let delegate = turso_core::Database::io_for_path(path_text)?;
        let io = Arc::new(Self {
            delegate,
            path,
            file,
            identity: FileId {
                dev: metadata.dev(),
                ino: metadata.ino(),
            },
        });
        io.verify_path()?;
        Ok(io)
    }

    pub(crate) fn identity(&self) -> FileId {
        self.identity
    }
    pub(crate) fn canonical_path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn verify_path(&self) -> Result<()> {
        let current = File::open(&self.path).map_err(|error| {
            Error::Denied(format!("native held-file path unavailable: {error}"))
        })?;
        let metadata = current
            .metadata()
            .map_err(|error| Error::Denied(format!("inspect native held-file path: {error}")))?;
        if self.identity
            != (FileId {
                dev: metadata.dev(),
                ino: metadata.ino(),
            })
        {
            return Err(Error::Denied(
                "native database pathname no longer identifies the held file".to_owned(),
            ));
        }
        Ok(())
    }

    pub(crate) fn open_main(&self) -> Result<Arc<dyn turso_core::File>> {
        self.verify_path()?;
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let descriptor_path = format!("/proc/self/fd/{}", self.file.as_raw_fd());
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let descriptor_path = format!("/dev/fd/{}", self.file.as_raw_fd());
        // No CREATE: the already-held file is the sole main-file authority.
        let file = self
            .delegate
            .open_file(&descriptor_path, OpenFlags::empty(), false)?;
        self.verify_path()?;
        Ok(file)
    }

    fn engine_verify(&self) -> turso_core::Result<()> {
        self.verify_path()
            .map_err(|error| turso_core::LimboError::InvalidArgument(error.to_string()))
    }
}

impl Clock for PinnedIo {
    fn current_time_monotonic(&self) -> MonotonicInstant {
        self.delegate.current_time_monotonic()
    }
    fn current_time_wall_clock(&self) -> WallClockInstant {
        self.delegate.current_time_wall_clock()
    }
}

impl IO for PinnedIo {
    fn open_file(
        &self,
        path: &str,
        flags: OpenFlags,
        direct: bool,
    ) -> turso_core::Result<Arc<dyn turso_core::File>> {
        self.engine_verify()?;
        if Path::new(path) == self.path {
            return self
                .open_main()
                .map_err(|error| turso_core::LimboError::InvalidArgument(error.to_string()));
        }
        self.delegate.open_file(path, flags, direct)
    }
    fn remove_file(&self, path: &str) -> turso_core::Result<()> {
        self.engine_verify()?;
        self.delegate.remove_file(path)
    }
    fn step(&self) -> turso_core::Result<()> {
        self.engine_verify()?;
        self.delegate.step()
    }
    fn cancel(&self, completions: &[turso_core::io::Completion]) -> turso_core::Result<()> {
        // Preserve the backend's cancellation/drain semantics during cleanup.
        // Cancellation must remain possible after pathname authority is revoked.
        self.delegate.cancel(completions)
    }
    fn register_fixed_buffer(
        &self,
        pointer: std::ptr::NonNull<u8>,
        length: usize,
    ) -> turso_core::Result<u32> {
        self.delegate.register_fixed_buffer(pointer, length)
    }
    fn file_id(&self, path: &str) -> turso_core::Result<FileId> {
        if Path::new(path) == self.path {
            return Ok(self.identity);
        }
        self.delegate.file_id(path)
    }
    fn supports_shared_wal_coordination(&self) -> bool {
        self.delegate.supports_shared_wal_coordination()
    }
    fn open_shared_wal_file(&self, path: &str) -> turso_core::Result<Arc<dyn turso_core::File>> {
        self.engine_verify()?;
        self.delegate.open_shared_wal_file(path)
    }
}
