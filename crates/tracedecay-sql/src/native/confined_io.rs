//! The engine's entire filesystem: the pinned main file and its `-wal`
//! sidecar. Every other open or remove fails, so no SQL can make the engine
//! touch a path the runtime did not hand it.

use std::{
    fs::{self, File, OpenOptions},
    io,
    path::Path,
    sync::{Arc, Mutex, PoisonError},
};

use turso_core::{
    IO, LimboError, OpenFlags,
    io::{
        Clock, FileId,
        clock::{MonotonicInstant, WallClockInstant},
    },
};

use super::{Error, Result};
use crate::{FileIdentity, file_identity};

pub(crate) struct ConfinedIo {
    delegate: Arc<dyn IO>,
    main: String,
    wal: String,
    identity: FileIdentity,
    /// Descriptors stay open for the engine's lifetime: closing any descriptor
    /// of a file drops every POSIX record lock this process holds on it,
    /// including the engine's exclusive lock. The main file is first.
    held: Mutex<Vec<File>>,
}

impl ConfinedIo {
    pub(crate) fn new(main: String, file: File, identity: FileIdentity) -> Result<Self> {
        Ok(Self {
            delegate: turso_core::Database::io_for_path(&main)?,
            wal: format!("{main}-wal"),
            main,
            identity,
            held: Mutex::new(vec![file]),
        })
    }

    #[cfg_attr(not(td_engine = "native"), allow(dead_code))]
    pub(crate) fn main(&self) -> &str {
        &self.main
    }

    /// Fails when the main pathname no longer names the held file.
    pub(crate) fn verify(&self) -> Result<()> {
        if self.has_moved()? {
            return Err(Error::Denied(
                "native database pathname no longer identifies the held file".to_owned(),
            ));
        }
        Ok(())
    }

    pub(crate) fn has_moved(&self) -> Result<bool> {
        match path_identity(&self.main) {
            Ok(identity) => Ok(identity != self.identity),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
            Err(error) => Err(Error::InvalidOperation(format!(
                "inspect native database path: {error}"
            ))),
        }
    }

    fn held(&self) -> std::sync::MutexGuard<'_, Vec<File>> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The identity a pathname names now. Uses `stat`, never an open-and-close,
/// which would release this process's record locks on the file.
pub(crate) fn path_identity(path: &str) -> io::Result<FileIdentity> {
    let id = turso_core::io::get_file_id(path)?;
    Ok(FileIdentity {
        volume: id.dev,
        file: id.ino,
    })
}

/// Opens a regular file read-write without following a symlink in its final
/// component; with `create`, creates it when absent.
pub(crate) fn open_regular(path: &Path, create: bool) -> io::Result<File> {
    loop {
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                if !metadata.file_type().is_file() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("{} is not a regular file", path.display()),
                    ));
                }
                let file = OpenOptions::new().read(true).write(true).open(path)?;
                if !same_file(&metadata, &file)? {
                    // ponytail: the path was swapped between lstat and open. The
                    // descriptor may name a file this process holds locked, and
                    // closing it would drop those locks, so it is leaked.
                    std::mem::forget(file);
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("{} changed while it was opened", path.display()),
                    ));
                }
                return Ok(file);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound && create => {
                match OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .open(path)
                {
                    Ok(file) => return Ok(file),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(unix)]
fn same_file(metadata: &fs::Metadata, file: &File) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let opened = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()) == (opened.dev(), opened.ino()))
}

// Windows path opens follow SQLite's parity contract instead: the identity of
// the held handle is checked against the path before and after every engine
// open and on each connect.
#[cfg(not(unix))]
fn same_file(_metadata: &fs::Metadata, _file: &File) -> io::Result<bool> {
    Ok(true)
}

fn refused(path: &str) -> LimboError {
    LimboError::InvalidArgument(format!("confined native IO refuses {path}"))
}

fn limbo(error: Error) -> LimboError {
    LimboError::InvalidArgument(error.to_string())
}

impl Clock for ConfinedIo {
    fn current_time_monotonic(&self) -> MonotonicInstant {
        self.delegate.current_time_monotonic()
    }
    fn current_time_wall_clock(&self) -> WallClockInstant {
        self.delegate.current_time_wall_clock()
    }
}

impl IO for ConfinedIo {
    fn open_file(
        &self,
        path: &str,
        flags: OpenFlags,
        direct: bool,
    ) -> turso_core::Result<Arc<dyn turso_core::File>> {
        let mut held = self.held();
        if path == self.main {
            self.verify().map_err(limbo)?;
        } else if path == self.wal {
            let wal = open_regular(Path::new(path), flags.contains(OpenFlags::Create))
                .map_err(|error| LimboError::InvalidArgument(format!("open {path}: {error}")))?;
            held.push(wal);
        } else {
            return Err(refused(path));
        }
        let file = if path == self.main {
            &held[0]
        } else {
            held.last().expect("the WAL descriptor was just pushed")
        };
        #[cfg(unix)]
        let target = descriptor_path(file);
        #[cfg(not(unix))]
        let target = {
            let _ = file;
            path.to_owned()
        };
        let opened = self.delegate.open_file(&target, flags, direct)?;
        if path == self.main {
            self.verify().map_err(limbo)?;
        }
        Ok(opened)
    }

    fn remove_file(&self, path: &str) -> turso_core::Result<()> {
        // The engine discards an orphaned WAL; nothing else may be removed.
        if path != self.wal {
            return Err(refused(path));
        }
        self.delegate.remove_file(path)
    }

    fn open_shared_wal_file(&self, path: &str) -> turso_core::Result<Arc<dyn turso_core::File>> {
        Err(refused(path))
    }

    fn step(&self) -> turso_core::Result<()> {
        self.delegate.step()
    }

    fn cancel(&self, completions: &[turso_core::io::Completion]) -> turso_core::Result<()> {
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
        if path == self.main {
            return Ok(FileId {
                dev: self.identity.volume,
                ino: self.identity.file,
            });
        }
        if path != self.wal {
            return Err(refused(path));
        }
        self.delegate.file_id(path)
    }
}

/// The engine opens the held descriptor rather than the mutable pathname.
#[cfg(unix)]
fn descriptor_path(file: &File) -> String {
    use std::os::unix::io::AsRawFd;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    return format!("/proc/self/fd/{}", file.as_raw_fd());
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    return format!("/dev/fd/{}", file.as_raw_fd());
}

/// The identity of an open descriptor, as [`path_identity`] reports it.
pub(crate) fn held_identity(file: &File) -> Result<FileIdentity> {
    file_identity(file)
        .map_err(|error| Error::InvalidOperation(format!("inspect native database file: {error}")))
}
