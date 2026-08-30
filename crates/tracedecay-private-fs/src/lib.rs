//! Owner-private filesystem creation, validation, and durable publication,
//! plus the process-wide OS primitives that share this crate's cold,
//! dependency-light position: background CPU admission and Windows
//! file-handle identity.

use std::fs::File;
use std::io;

pub mod background_cpu;
pub mod capability_dir;
pub mod framed_log;
#[cfg(windows)]
pub mod windows_file;

/// A private-file creation failure that distinguishes pre-creation errors from
/// validation errors on an already-created exact file handle.
#[derive(Debug)]
pub struct PrivateFileCreationFailure {
    error: io::Error,
    file: Option<File>,
}

impl PrivateFileCreationFailure {
    pub(crate) fn before_creation(error: io::Error) -> Self {
        Self { error, file: None }
    }

    pub(crate) fn after_creation(error: io::Error, file: File) -> Self {
        Self {
            error,
            file: Some(file),
        }
    }

    /// Returns only the underlying error, releasing any retained file handle.
    pub fn into_error(self) -> io::Error {
        let Self { error, file } = self;
        drop(file);
        error
    }
}

impl std::fmt::Display for PrivateFileCreationFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for PrivateFileCreationFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Receipt from [`make_private_directory`]: the pre-heal state observed on the
/// exact directory handle that was re-permissioned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MadePrivateDirectory {
    /// Unix permission bits observed before re-permissioning; `None` on
    /// platforms without Unix modes.
    pub previous_unix_mode: Option<u32>,
}

#[cfg(windows)]
pub mod windows;

#[cfg(windows)]
pub use windows::{
    available_space, create_private_directory, create_private_file, create_private_file_retained,
    make_private_directory, make_private_file, open_private_directory, open_private_file,
    validate_directory_path, validate_private_directory, validate_private_file,
};

#[cfg(unix)]
mod unix {
    use std::fs;
    use std::io;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::path::Path;

    #[hotpath::measure(label = "private_fs.create_directory")]
    pub fn create_private_directory(path: &Path) -> io::Result<()> {
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(path)?;
        drop(open_private_directory(path)?);
        Ok(())
    }

    pub fn validate_private_directory(path: &Path) -> io::Result<()> {
        drop(open_private_directory(path)?);
        Ok(())
    }

    pub fn validate_private_file(path: &Path) -> io::Result<()> {
        drop(open_private_file(path)?);
        Ok(())
    }

    #[hotpath::measure(label = "private_fs.open_directory")]
    pub fn open_private_directory(path: &Path) -> io::Result<fs::File> {
        let mut options = fs::OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW);
        let file = options.open(path).map_err(normalize_no_follow_error)?;
        validate_handle(&file, true, 0o700)?;
        Ok(file)
    }

    pub fn create_private_file(path: &Path) -> io::Result<fs::File> {
        create_private_file_retained(path).map_err(crate::PrivateFileCreationFailure::into_error)
    }

    /// Creates a new private file and returns its exact handle with any
    /// post-creation validation failure.
    #[hotpath::measure(label = "private_fs.create_file")]
    pub fn create_private_file_retained(
        path: &Path,
    ) -> Result<fs::File, crate::PrivateFileCreationFailure> {
        let mut options = fs::OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        let file = options
            .open(path)
            .map_err(normalize_no_follow_error)
            .map_err(crate::PrivateFileCreationFailure::before_creation)?;
        if let Err(error) = validate_handle(&file, false, 0o600) {
            return Err(crate::PrivateFileCreationFailure::after_creation(
                error, file,
            ));
        }
        Ok(file)
    }

    #[hotpath::measure(label = "private_fs.open_file")]
    pub fn open_private_file(path: &Path) -> io::Result<fs::File> {
        let mut options = fs::OpenOptions::new();
        options
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        let file = options.open(path).map_err(normalize_no_follow_error)?;
        validate_handle(&file, false, 0o600)?;
        Ok(file)
    }

    #[hotpath::measure(label = "private_fs.make_private_file")]
    pub fn make_private_file(path: &Path) -> io::Result<fs::File> {
        let mut options = fs::OpenOptions::new();
        options
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        let file = options.open(path).map_err(normalize_no_follow_error)?;
        let metadata = file.metadata()?;
        validate_kind(&metadata, false)?;
        // SAFETY: `geteuid` takes no pointers and has no preconditions.
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "filesystem handle is not owned by the current user",
            ));
        }
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        validate_handle(&file, false, 0o600)?;
        Ok(file)
    }

    /// Re-permission an existing directory to owner-private through its exact
    /// opened handle, when the current user owns it.
    ///
    /// The directory analogue of [`make_private_file`]: creation-time privacy
    /// belongs to [`create_private_directory`], while this converges a legacy
    /// directory an older binary created under a permissive umask. It never
    /// follows symlinks, refuses a directory another user owns (ownership is
    /// the proof the caller may tighten it), and re-validates the handle after
    /// tightening so a concurrent swap cannot smuggle a non-private object.
    #[hotpath::measure(label = "private_fs.make_private_directory")]
    pub fn make_private_directory(path: &Path) -> io::Result<crate::MadePrivateDirectory> {
        let mut options = fs::OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW);
        let file = options.open(path).map_err(normalize_no_follow_error)?;
        let metadata = file.metadata()?;
        validate_kind(&metadata, true)?;
        // SAFETY: `geteuid` takes no pointers and has no preconditions.
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "filesystem handle is not owned by the current user",
            ));
        }
        let previous_mode = metadata.permissions().mode() & 0o777;
        file.set_permissions(fs::Permissions::from_mode(0o700))?;
        validate_handle(&file, true, 0o700)?;
        Ok(crate::MadePrivateDirectory {
            previous_unix_mode: Some(previous_mode),
        })
    }

    pub fn validate_directory_path(path: &Path) -> io::Result<()> {
        let mut options = fs::OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW);
        let file = options.open(path).map_err(normalize_no_follow_error)?;
        validate_kind(&file.metadata()?, true)?;
        Ok(())
    }

    /// Returns bytes available to the current user at `path` (quota-aware).
    #[hotpath::measure(label = "private_fs.available_space")]
    pub fn available_space(path: &Path) -> io::Result<u64> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "path contains an interior NUL")
        })?;
        let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: `c_path` is a valid NUL-terminated C string and `stat` is
        // writable for a `statvfs` result.
        let rc = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `statvfs` initialized `stat` on success.
        let stat = unsafe { stat.assume_init() };
        // Width of `statvfs` block fields differs by Unix target.
        #[allow(clippy::unnecessary_cast)]
        {
            Ok((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
        }
    }

    fn validate_handle(file: &fs::File, directory: bool, mode: u32) -> io::Result<()> {
        let metadata = file.metadata()?;
        validate_kind(&metadata, directory)?;
        // SAFETY: `geteuid` takes no pointers and has no preconditions.
        let current_user = unsafe { libc::geteuid() };
        if metadata.permissions().mode() & 0o777 != mode || metadata.uid() != current_user {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "filesystem handle is not private to the current owner",
            ));
        }
        Ok(())
    }

    fn validate_kind(metadata: &fs::Metadata, directory: bool) -> io::Result<()> {
        if metadata.is_dir() != directory || (!directory && !metadata.file_type().is_file()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "filesystem handle has the wrong object kind",
            ));
        }
        Ok(())
    }

    fn normalize_no_follow_error(error: io::Error) -> io::Error {
        if error.raw_os_error() == Some(libc::ELOOP) {
            io::Error::new(io::ErrorKind::InvalidInput, "path is a symbolic link")
        } else {
            error
        }
    }
}

#[cfg(unix)]
pub use unix::{
    available_space, create_private_directory, create_private_file, create_private_file_retained,
    make_private_directory, make_private_file, open_private_directory, open_private_file,
    validate_directory_path, validate_private_directory, validate_private_file,
};

#[cfg(not(any(unix, windows)))]
compile_error!("TraceDecay private filesystem authority requires Unix or Windows");

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

    use tempfile::tempdir;

    use super::{
        create_private_directory, create_private_file, open_private_directory, open_private_file,
    };

    #[test]
    fn private_opens_reject_wrong_modes() {
        let temp = tempdir().unwrap();
        let directory = temp.path().join("private");
        create_private_directory(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            open_private_directory(&directory).unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );

        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let file_path = directory.join("store");
        drop(create_private_file(&file_path).unwrap());
        std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            open_private_file(&file_path).unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn private_opens_reject_symlinks() {
        let temp = tempdir().unwrap();
        let directory = temp.path().join("private");
        create_private_directory(&directory).unwrap();
        let directory_link = temp.path().join("directory-link");
        symlink(&directory, &directory_link).unwrap();
        assert!(open_private_directory(&directory_link).is_err());

        let file_path = directory.join("store");
        drop(create_private_file(&file_path).unwrap());
        let file_link = directory.join("store-link");
        symlink(&file_path, &file_link).unwrap();
        assert!(open_private_file(&file_link).is_err());
    }

    #[test]
    fn owned_permissive_directory_is_healed_through_its_handle() {
        let temp = tempdir().unwrap();
        let directory = temp.path().join("legacy");
        create_private_directory(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o775)).unwrap();
        assert!(open_private_directory(&directory).is_err());

        let receipt = super::make_private_directory(&directory).unwrap();

        assert_eq!(receipt.previous_unix_mode, Some(0o775));
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        drop(open_private_directory(&directory).unwrap());
    }

    #[test]
    fn directory_heal_rejects_symlinks_and_non_directories() {
        let temp = tempdir().unwrap();
        let directory = temp.path().join("target");
        create_private_directory(&directory).unwrap();
        let link = temp.path().join("link");
        symlink(&directory, &link).unwrap();
        assert!(super::make_private_directory(&link).is_err());

        let file_path = temp.path().join("regular");
        drop(create_private_file(&file_path).unwrap());
        assert!(super::make_private_directory(&file_path).is_err());
    }

    #[test]
    fn returned_file_handle_keeps_the_created_identity() {
        let temp = tempdir().unwrap();
        let directory = temp.path().join("private");
        create_private_directory(&directory).unwrap();
        let path = directory.join("store");
        let created = create_private_file(&path).unwrap();
        let created_identity = created.metadata().unwrap();

        let moved = directory.join("moved");
        std::fs::rename(&path, &moved).unwrap();
        drop(create_private_file(&path).unwrap());
        let replacement_identity = std::fs::metadata(&path).unwrap();

        assert_eq!(created.metadata().unwrap().dev(), created_identity.dev());
        assert_eq!(created.metadata().unwrap().ino(), created_identity.ino());
        assert_ne!(created_identity.ino(), replacement_identity.ino());
    }
}

#[cfg(test)]
mod available_space_tests {
    use tempfile::tempdir;

    use super::available_space;

    #[test]
    fn available_space_reports_positive_capacity_on_tempdir() {
        let temp = tempdir().unwrap();
        let available = available_space(temp.path()).unwrap();
        assert!(
            available > 0,
            "expected positive free space, got {available}"
        );
    }

    #[test]
    fn available_space_rejects_missing_path() {
        let temp = tempdir().unwrap();
        let missing = temp.path().join("does-not-exist");
        assert!(available_space(&missing).is_err());
    }
}
