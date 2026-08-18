use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use fs2::FileExt;
use tracedecay_domain::framed_log::DirectorySyncPolicy;

use crate::config;
use crate::errors::{Result, TraceDecayError};

#[cfg(windows)]
use super::DURABLE_REMOVAL_TOMBSTONE_PREFIX;
use super::{
    ActiveProjectContext, BRANCH_META_FILENAME, DurableAtomicWritePhase, EnrollmentMarker,
    GraphScopeId, PrivateStoreIo, ProjectIdentity, ProjectPath, QueryTarget, SESSIONS_DB_FILENAME,
    STORE_MANIFEST_SCHEMA_VERSION, StorageMode, StoreArtifactPath, StoreKind, StoreLayout,
    StoreManifest, inject_durable_atomic_write_fault, inject_durable_namespace_sync_fault,
};

impl StoreManifest {
    pub(crate) fn from_layout(layout: &StoreLayout) -> Self {
        Self {
            schema_version: STORE_MANIFEST_SCHEMA_VERSION,
            project_id: layout.identity.project_id.clone(),
            store_kind: layout.store_kind.clone(),
            storage_mode: layout.storage_mode.clone(),
            project_root: layout.project_root.clone(),
            data_root: layout.data_root.clone(),
            graph_db_relpath: relative_to_data_root(&layout.graph_db_path, &layout.data_root),
            sessions_db_relpath: relative_to_data_root(&layout.sessions_db_path, &layout.data_root),
            branch_meta_relpath: relative_to_data_root(&layout.branch_meta_path, &layout.data_root),
        }
    }
}

impl ActiveProjectContext {
    pub fn new(layout: StoreLayout, scope_id: GraphScopeId) -> Self {
        let query_target = QueryTarget {
            graph_db_path: layout.graph_db_path.clone(),
        };
        Self {
            layout,
            scope_id,
            query_target,
        }
    }
}

impl ProjectPath {
    pub fn resolve(project_root: &Path, path: &Path) -> Result<Self> {
        validate_no_nul(path)?;
        validate_normal_components(path, true)?;
        let root = project_root
            .canonicalize()
            .map_err(|e| TraceDecayError::Config {
                message: format!(
                    "failed to canonicalize project root '{}': {e}",
                    project_root.display()
                ),
            })?;
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            project_root.join(path)
        };
        let absolute_path = candidate
            .canonicalize()
            .map_err(|e| TraceDecayError::Config {
                message: format!(
                    "failed to canonicalize project path '{}': {e}",
                    candidate.display()
                ),
            })?;
        let relative_path = absolute_path
            .strip_prefix(&root)
            .map_err(|_| TraceDecayError::Config {
                message: format!(
                    "path '{}' escapes project root '{}'",
                    path.display(),
                    project_root.display()
                ),
            })?
            .to_path_buf();
        Ok(Self {
            absolute_path,
            relative_path,
        })
    }

    pub fn absolute_path(&self) -> PathBuf {
        self.absolute_path.clone()
    }

    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    pub fn relative_path_string(&self) -> String {
        self.relative_path.to_string_lossy().replace('\\', "/")
    }
}

impl StoreArtifactPath {
    pub fn resolve(store_root: &Path, relpath: &Path) -> Result<Self> {
        validate_no_nul(relpath)?;
        validate_normal_components(relpath, false)?;
        if relpath.is_absolute() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "store artifact path '{}' must be relative",
                    relpath.display()
                ),
            });
        }
        let absolute_path = store_root.join(relpath);
        reject_symlink_components(&absolute_path, "store artifact path").map_err(|e| {
            TraceDecayError::Config {
                message: format!("store artifact path '{}' is unsafe: {e}", relpath.display()),
            }
        })?;
        Ok(Self {
            absolute_path,
            relative_path: relpath.to_path_buf(),
        })
    }

    pub fn absolute_path(&self) -> PathBuf {
        self.absolute_path.clone()
    }

    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }
}

impl PrivateStoreIo {
    /// Creates or validates one owner-private leaf below an existing store
    /// authority. Unlike `create_dir_all`, this never manufactures ancestors.
    pub fn create_private_directory(path: &Path) -> io::Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| invalid_input("private store directory has no parent"))?;
        reject_symlink_components(parent, "private store directory parent")?;
        if !parent.is_dir() {
            return Err(invalid_input(
                "private store directory parent must already exist",
            ));
        }
        match tracedecay_private_fs::create_private_directory(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                tracedecay_private_fs::validate_private_directory(path)
            }
            Err(error) => Err(error),
        }
    }

    pub fn create_dir_all(path: &Path) -> io::Result<()> {
        reject_symlink_components(path, "private store directory")?;
        // Record the ancestors this call is about to create so every one of
        // them is re-permissioned to owner-private, not just the leaf. Under
        // a permissive umask, `fs::create_dir_all` would otherwise leave
        // intermediate store directories (e.g. the profile root created as a
        // by-product of a deeper store path) group/world accessible, and
        // fail-closed private-store validation later rejects them.
        let mut created_ancestors = Vec::new();
        let mut cursor = path.parent();
        while let Some(current) = cursor {
            if current.as_os_str().is_empty() || current.exists() {
                break;
            }
            created_ancestors.push(current.to_path_buf());
            cursor = current.parent();
        }
        fs::create_dir_all(path)?;
        for ancestor in created_ancestors.iter().rev() {
            set_private_dir_permissions(ancestor)?;
        }
        set_private_dir_permissions(path)
    }

    /// Creates an absolute private directory hierarchy and durably publishes
    /// every new namespace entry before returning.
    pub fn create_dir_all_durable(path: &Path) -> io::Result<()> {
        if !path.is_absolute() {
            return Err(invalid_input(
                "durable private store directory path must be absolute",
            ));
        }
        platform_create_dir_all_durable(path)
    }

    /// Removes one private-store file and establishes the platform namespace
    /// durability barrier before reporting success.
    pub fn remove_file_durable(path: &Path) -> io::Result<bool> {
        reject_symlink_components(path, "private store durable removal")?;
        platform_remove_file_durable(path)
    }

    pub fn write_file(path: &Path, contents: &[u8]) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            Self::create_dir_all(parent)?;
        }
        reject_symlink_components(path, "private store file")?;
        Self::open_private(path, fs::OpenOptions::new().write(true).truncate(true))?
            .write_all(contents)?;
        set_private_file_permissions(path)
    }

    /// Appends one line to the private store `path` while holding the shared
    /// sidecar append lock, so concurrent threads and processes never interleave
    /// partial lines. See [`append_line_locked`] and the sidecar-lock module
    /// note for the read+write-handle rationale.
    pub fn append_line(path: &Path, line: &str) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            Self::create_dir_all(parent)?;
        }
        retry_transient_file_op(|| append_line_locked(path, line, true))
    }

    /// Writes one newline-terminated line to the private store data file with
    /// owner-only permissions. Callers must already hold the sidecar append lock
    /// (see [`append_line_locked`]).
    fn append_line_data(path: &Path, line: &str) -> io::Result<()> {
        reject_symlink_components(path, "private store file")?;
        let mut options = fs::OpenOptions::new();
        options.append(true);
        let mut file = Self::open_private(path, &mut options)?;
        file.write_all(format!("{line}\n").as_bytes())?;
        file.flush()?;
        drop(file);
        set_private_file_permissions(path)
    }

    /// Opens `path` for writing, creating it if missing with owner-only
    /// permissions applied at create time (Unix), so a fresh file never
    /// exists with umask-default permissions before the trailing
    /// `set_private_file_permissions` call. Pre-existing files keep their
    /// mode here and are tightened by that trailing call.
    fn open_private(path: &Path, options: &mut fs::OpenOptions) -> io::Result<fs::File> {
        options.create(true);
        apply_private_create_mode(options);
        options.open(path)
    }

    pub fn write_file_atomically(path: &Path, temp_path: &Path, contents: &[u8]) -> io::Result<()> {
        if path_parent(path) != path_parent(temp_path) {
            return Err(invalid_input(
                "private store atomic write temp path must share the target directory",
            ));
        }
        if path == temp_path {
            return Err(invalid_input(
                "private store atomic write temp path must differ from the target",
            ));
        }
        if let Some(parent) = path.parent() {
            Self::create_dir_all(parent)?;
        }
        reject_symlink_components(path, "private store file")?;
        reject_symlink_components(temp_path, "private store temp file")?;
        fs::write(temp_path, contents)?;
        set_private_file_permissions(temp_path)?;
        crate::db::DatabaseAuthority::replace_file_atomically(
            temp_path,
            path,
            "private store file",
        )
        .map_err(io::Error::other)?;
        set_private_file_permissions(path)
    }

    /// Atomically replaces a private-store file and establishes the durability
    /// barrier required before a destructive operation may trust it.
    ///
    /// A failure after the atomic rename can leave the complete replacement at
    /// `path`. Callers that require rollback must retain and restore their prior
    /// value under their own stable serialization authority; this primitive
    /// never unlinks a destination it cannot prove it still owns.
    pub fn write_file_atomically_durable(
        path: &Path,
        temp_path: &Path,
        contents: &[u8],
    ) -> io::Result<()> {
        if path_parent(path) != path_parent(temp_path) || path == temp_path {
            return Err(invalid_input(
                "durable private-store write requires a distinct sibling temp path",
            ));
        }
        if let Some(parent) = path.parent() {
            Self::create_dir_all_durable(parent)?;
        }
        reject_symlink_components(path, "private store file")?;
        reject_symlink_components(temp_path, "private store temp file")?;
        {
            let mut options = fs::OpenOptions::new();
            options.write(true).truncate(true);
            let mut temp = Self::open_private(temp_path, &mut options)?;
            temp.write_all(contents)?;
            temp.sync_all()?;
        }
        set_private_file_permissions(temp_path)?;
        inject_durable_atomic_write_fault(DurableAtomicWritePhase::AfterTempSync)?;
        crate::db::DatabaseAuthority::replace_file_atomically(
            temp_path,
            path,
            "private store durable file",
        )
        .map_err(io::Error::other)?;
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .and_then(|file| file.sync_all())
            .and_then(|()| inject_durable_atomic_write_fault(DurableAtomicWritePhase::AfterRename))
            .and_then(|()| sync_parent_directory(path))?;
        Ok(())
    }

    /// Synchronizes the durable members of one `SQLite` WAL family. The SHM
    /// coordination file is intentionally excluded because `SQLite` rebuilds it.
    pub fn sync_sqlite_family(path: &Path) -> io::Result<()> {
        reject_symlink_components(path, "private SQLite store")?;
        for member in [
            path.to_path_buf(),
            PathBuf::from(format!("{}-wal", path.display())),
        ] {
            match fs::OpenOptions::new().read(true).write(true).open(&member) {
                Ok(file) => file.sync_all()?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        sync_parent_directory(path)
    }

    pub fn copy_artifact(source: &Path, target: &Path) -> io::Result<u64> {
        let meta = source.symlink_metadata()?;
        if meta.file_type().is_symlink() {
            return Err(invalid_input(
                "private store artifact source must not be a symlink",
            ));
        }
        reject_symlink_components(target, "private store artifact target")?;
        if meta.is_dir() {
            return Self::copy_dir(source, target);
        }
        if let Some(parent) = target.parent() {
            Self::create_dir_all(parent)?;
        }
        let bytes = fs::copy(source, target)?;
        set_private_file_permissions(target)?;
        Ok(bytes)
    }

    fn copy_dir(source: &Path, target: &Path) -> io::Result<u64> {
        Self::create_dir_all(target)?;
        let mut bytes = 0;
        let mut entries = fs::read_dir(source)?.collect::<io::Result<Vec<_>>>()?;
        entries.sort_by_key(std::fs::DirEntry::path);
        for entry in entries {
            let source_path = entry.path();
            let target_path = target.join(entry.file_name());
            let meta = source_path.symlink_metadata()?;
            if meta.file_type().is_symlink() {
                return Err(invalid_input(
                    "private store artifact source must not contain symlinks",
                ));
            }
            if meta.is_dir() {
                bytes += Self::copy_dir(&source_path, &target_path)?;
            } else if meta.is_file() {
                bytes += Self::copy_artifact(&source_path, &target_path)?;
            }
        }
        Ok(bytes)
    }
}

fn sync_parent_directory(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid_input("private store durable file has no parent directory"))?;
    inject_durable_namespace_sync_fault()?;
    tracedecay_domain::framed_log::sync_directory(parent, DirectorySyncPolicy::Strict)
}

fn missing_directories(path: &Path) -> io::Result<Vec<PathBuf>> {
    if path.as_os_str().is_empty() {
        return Err(invalid_input(
            "durable private store directory path must not be empty",
        ));
    }
    let mut missing = Vec::new();
    let mut cursor = Some(path);
    while let Some(current) = cursor {
        if current.as_os_str().is_empty() {
            break;
        }
        match current.try_exists() {
            Ok(true) => break,
            Ok(false) => missing.push(current.to_path_buf()),
            Err(error) => return Err(error),
        }
        cursor = current.parent();
    }
    Ok(missing)
}

#[cfg(unix)]
fn platform_create_dir_all_durable(path: &Path) -> io::Result<()> {
    let missing = missing_directories(path)?;
    let Some(highest_missing) = missing.last() else {
        PrivateStoreIo::create_dir_all(path)?;
        return sync_parent_directory(path);
    };
    let existing_parent = highest_missing
        .parent()
        .ok_or_else(|| invalid_input("durable private store directory has no parent directory"))?;
    if existing_parent.parent().is_some() {
        sync_parent_directory(existing_parent)?;
    }
    for destination in missing.iter().rev() {
        PrivateStoreIo::create_private_directory(destination)?;
        sync_parent_directory(destination)?;
    }
    Ok(())
}

#[cfg(windows)]
fn platform_create_dir_all_durable(path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};

    reject_symlink_components(path, "durable private store directory")?;
    let missing = missing_directories(path)?;
    if missing.is_empty() {
        return PrivateStoreIo::create_dir_all(path);
    }
    for destination in missing.iter().rev() {
        let parent = destination.parent().ok_or_else(|| {
            invalid_input("durable private store directory has no parent directory")
        })?;
        let destination_name = destination
            .file_name()
            .ok_or_else(|| invalid_input("durable private store directory has no file name"))?;
        let mut lock_name = OsString::from(".");
        lock_name.push(destination_name);
        lock_name.push(".durable-directory.lock");
        let lock_path = parent.join(lock_name);
        reject_symlink_components(&lock_path, "durable private store directory lock")?;
        let _lock = acquire_lock_file_blocking(&lock_path, true)?;
        if destination.try_exists()? {
            tracedecay_private_fs::validate_private_directory(destination)?;
            continue;
        }
        let staging = unique_private_staging_directory(parent)?;
        let encode = |value: &Path| {
            value
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect::<Vec<_>>()
        };
        let existing = encode(&staging);
        let replacement = encode(destination);
        let moved = unsafe {
            MoveFileExW(
                existing.as_ptr(),
                replacement.as_ptr(),
                MOVEFILE_WRITE_THROUGH,
            )
        };
        if moved != 0 {
            continue;
        }
        let error = io::Error::last_os_error();
        let cleanup = fs::remove_dir(&staging);
        if error.kind() == io::ErrorKind::AlreadyExists && cleanup.is_ok() {
            tracedecay_private_fs::validate_private_directory(destination)?;
            continue;
        }
        return match cleanup {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(io::Error::new(
                error.kind(),
                format!(
                    "{error}; additionally failed to remove private directory staging: {cleanup_error}"
                ),
            )),
        };
    }
    PrivateStoreIo::create_dir_all(path)
}

#[cfg(not(any(unix, windows)))]
fn platform_create_dir_all_durable(path: &Path) -> io::Result<()> {
    let missing = missing_directories(path)?;
    if missing.is_empty() {
        return PrivateStoreIo::create_dir_all(path);
    }
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "durable private-directory publication is unsupported on this platform",
    ))
}

#[cfg(windows)]
fn unique_private_staging_directory(parent: &Path) -> io::Result<PathBuf> {
    for _ in 0..16 {
        let mut entropy = [0_u8; 16];
        getrandom::getrandom(&mut entropy).map_err(|error| io::Error::other(error.to_string()))?;
        let path = parent.join(format!(
            ".tracedecay-directory-staging-{}",
            hex::encode(entropy)
        ));
        match tracedecay_private_fs::create_private_directory(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not reserve a unique private directory staging path",
    ))
}

#[cfg(unix)]
fn platform_remove_file_durable(path: &Path) -> io::Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => {
            sync_parent_directory(path)?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            sync_parent_directory(path)?;
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn platform_remove_file_durable(path: &Path) -> io::Result<bool> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let parent = path
        .parent()
        .ok_or_else(|| invalid_input("private store durable removal has no parent directory"))?;
    let tombstone = tempfile::Builder::new()
        .prefix(DURABLE_REMOVAL_TOMBSTONE_PREFIX)
        .tempfile_in(parent)?;
    let (tombstone_file, tombstone_path) = tombstone.keep()?;
    drop(tombstone_file);
    let encode = |value: &Path| {
        value
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>()
    };
    let existing = encode(path);
    let replacement = encode(&tombstone_path);
    let retired = retry_transient_file_op(|| {
        let moved = unsafe {
            MoveFileExW(
                existing.as_ptr(),
                replacement.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if moved == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    });
    if let Err(error) = retired {
        let cleanup = fs::remove_file(&tombstone_path);
        if error.kind() == io::ErrorKind::NotFound && cleanup.is_ok() {
            return Ok(false);
        }
        return match cleanup {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(io::Error::new(
                error.kind(),
                format!(
                    "{error}; additionally failed to remove durable-removal tombstone: {cleanup_error}"
                ),
            )),
        };
    }
    fs::remove_file(&tombstone_path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "target was durably retired but its deletion tombstone could not be removed: {error}"
            ),
        )
    })?;
    Ok(true)
}

#[cfg(not(any(unix, windows)))]
fn platform_remove_file_durable(path: &Path) -> io::Result<bool> {
    if !path.try_exists()? {
        return Ok(false);
    }
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "durable private-file removal is unsupported on this platform",
    ))
}

pub fn reject_symlink_components(path: &Path, subject: &str) -> io::Result<()> {
    let is_absolute = path.is_absolute();
    let mut current = PathBuf::new();
    let mut normal_components = 0usize;
    for component in path.components() {
        match component {
            Component::Normal(_) => {
                current.push(component.as_os_str());
                normal_components += 1;
            }
            Component::RootDir | Component::Prefix(_) => {
                current.push(component.as_os_str());
            }
            Component::CurDir | Component::ParentDir => {
                return Err(invalid_input(format!("{subject} path must be normalized")));
            }
        }
        if normal_components == 0 || (is_absolute && normal_components == 1) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(invalid_input(format!(
                    "{subject} path must not contain symlinks"
                )));
            }
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => break,
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn path_parent(path: &Path) -> &Path {
    path.parent().unwrap_or_else(|| Path::new(""))
}

/// Sibling `<file>.lock` path used to serialize appends without locking the
/// data file's own handle. Shared with the automation run ledger writer.
pub fn append_lock_path(path: &Path) -> PathBuf {
    let mut lock_name = path
        .file_name()
        .map_or_else(|| OsString::from("append"), std::ffi::OsStr::to_os_string);
    lock_name.push(".lock");
    path.with_file_name(lock_name)
}

// ── Cross-process sidecar lock utility ──────────────────────────────
//
// TraceDecay sanctions two cross-process file-coordination strategies; new code
// should reuse one rather than hand-rolling a third:
//
//   1. Sidecar advisory lock (this utility). Open a dedicated `<file>.lock`
//      handle for read+write and hold an `fs2` `flock` on it while mutating the
//      real file. Use it to serialize writers to an append-only log or an
//      mmap/config file where readers must never see a torn write and a crashed
//      holder must not leave a stale marker (the OS drops the lock on process
//      death). Callers: private-store appends, the automation run ledger, the
//      monitor ring buffer and single-instance guard, the structured-backfill
//      sweep, and the user-config save.
//   2. Atomic rename + hash ownership (see `write_file_atomically` and the
//      dashboard curation writers). Write a sibling temp file and `rename` it
//      over the target so readers always observe a whole file, using a content
//      hash to decide the final owner. Use it for whole-file replaces where
//      last-writer-wins is acceptable.
//
// The lock is always taken on a *separate* r/w `<file>.lock` handle, never on
// the data handle. Rust opens append-only handles with
// `FILE_GENERIC_WRITE & !FILE_WRITE_DATA` (no read-data, no write-data), and
// Windows `LockFileEx` requires the handle to carry `FILE_READ_DATA` or
// `FILE_WRITE_DATA`, so locking such a handle fails with `ERROR_ACCESS_DENIED`
// (os error 5). Locking the r/w sidecar sidesteps that and avoids locking the
// data region being written. This rationale lives here once; call sites point
// back to it rather than restating it.

pub(super) fn open_lock_file(lock_path: &Path, private: bool) -> io::Result<fs::File> {
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }

    #[cfg(windows)]
    if private {
        return crate::windows_security::open_or_create_private_lock_file(lock_path);
    }

    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).truncate(false);
    let file = if private {
        PrivateStoreIo::open_private(lock_path, &mut options)?
    } else {
        options.create(true).open(lock_path)?
    };
    if private {
        set_private_file_permissions(lock_path)?;
    }
    Ok(file)
}

/// Non-blocking sidecar lock acquisition. Returns the held lock file on
/// success, or `None` when another process/thread already holds it (the caller
/// then skips its critical section). See the sidecar-lock module note above for
/// the read+write-handle rationale.
pub fn try_acquire_sidecar_lock(lock_path: &Path) -> io::Result<Option<fs::File>> {
    let file = open_lock_file(lock_path, false)?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(file)),
        Err(err) if err.kind() == io::ErrorKind::WouldBlock => Ok(None),
        Err(err) => Err(err),
    }
}

/// Blocking sidecar lock acquisition. Returns the held lock file once the
/// exclusive lock is granted. See the sidecar-lock module note above for the
/// read+write-handle rationale.
pub fn acquire_sidecar_lock_blocking(lock_path: &Path) -> io::Result<fs::File> {
    acquire_lock_file_blocking(lock_path, false)
}

fn acquire_lock_file_blocking(lock_path: &Path, private: bool) -> io::Result<fs::File> {
    let file = open_lock_file(lock_path, private)?;
    file.lock_exclusive()?;
    Ok(file)
}

/// Appends `line` (newline-terminated) to `path` under the shared sidecar
/// append lock. When `private`, the data file is created owner-only and both
/// the data and lock paths are symlink-checked (the private-store contract);
/// otherwise a plain create+append handle is used (the automation run ledger).
pub(crate) fn append_line_locked(path: &Path, line: &str, private: bool) -> io::Result<()> {
    let lock_path = append_lock_path(path);
    if private {
        reject_symlink_components(&lock_path, "private store lock file")?;
    }
    let lock_file = acquire_lock_file_blocking(&lock_path, private)?;
    let write_result = if private {
        PrivateStoreIo::append_line_data(path, line)
    } else {
        append_line_plain(path, line)
    };
    let unlock_result = lock_file.unlock();
    write_result?;
    unlock_result?;
    if private {
        set_private_file_permissions(&lock_path)?;
    }
    Ok(())
}

fn append_line_plain(path: &Path, line: &str) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(format!("{line}\n").as_bytes())?;
    file.flush()
}

/// Runs `op`, retrying a bounded number of times on Windows for the transient
/// file-access error codes that antivirus scanners and delete-pending handle
/// states briefly produce: `ERROR_ACCESS_DENIED` (5), `ERROR_SHARING_VIOLATION`
/// (32), and `ERROR_LOCK_VIOLATION` (33). The retries total well under ~250ms
/// and the final error is always propagated. On non-Windows platforms `op`
/// runs exactly once.
pub fn retry_transient_file_op<F>(mut op: F) -> io::Result<()>
where
    F: FnMut() -> io::Result<()>,
{
    #[cfg(windows)]
    {
        const MAX_ATTEMPTS: u32 = 5;
        let mut attempt: u32 = 1;
        loop {
            match op() {
                Ok(()) => return Ok(()),
                Err(err) if attempt < MAX_ATTEMPTS && is_transient_windows_file_error(&err) => {
                    std::thread::sleep(transient_file_backoff(attempt));
                    attempt += 1;
                }
                Err(err) => return Err(err),
            }
        }
    }
    #[cfg(not(windows))]
    {
        op()
    }
}

#[cfg(windows)]
fn is_transient_windows_file_error(err: &io::Error) -> bool {
    matches!(err.raw_os_error(), Some(5 | 32 | 33))
}

#[cfg(windows)]
fn transient_file_backoff(attempt: u32) -> std::time::Duration {
    // Base 10, 20, 40, 80 ms (sum 150 ms across the 4 retries) plus a small
    // jitter derived from the wall clock to de-correlate contending writers.
    let base = 10u64 << (attempt - 1);
    let jitter = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::from(d.subsec_nanos()) % u64::from(attempt + 1));
    std::time::Duration::from_millis(base + jitter)
}

pub(super) fn relative_to_data_root(path: &Path, data_root: &Path) -> PathBuf {
    path.strip_prefix(data_root).unwrap_or(path).to_path_buf()
}

impl StoreLayout {
    pub(super) fn new(
        identity: ProjectIdentity,
        store_kind: StoreKind,
        storage_mode: StorageMode,
        project_root: PathBuf,
        data_root: PathBuf,
        manifest_filename: Option<&str>,
    ) -> Self {
        let graph_db_path = data_root.join(config::db_filename(&data_root));
        let config_path = data_root.join("config.json");
        let branch_meta_path = data_root.join(BRANCH_META_FILENAME);
        let sessions_db_path = data_root.join(SESSIONS_DB_FILENAME);
        let response_handle_root = data_root.join("response-handles");
        let lcm_payload_root = data_root.join("lcm-payloads");
        let dashboard_root = data_root.join("dashboard");
        let manifest_path = manifest_filename.map(|filename| data_root.join(filename));
        let dirty_path = data_root.join("dirty");
        let sync_lock_path = data_root.join("sync.lock");
        let branch_add_lock_path = data_root.join(".branch-add.lock");
        Self {
            identity,
            store_kind,
            storage_mode,
            project_root,
            data_root,
            graph_db_path,
            config_path,
            branch_meta_path,
            sessions_db_path,
            response_handle_root,
            lcm_payload_root,
            dashboard_root,
            manifest_path,
            dirty_path,
            sync_lock_path,
            branch_add_lock_path,
        }
    }
}

pub(super) fn validate_enrollment_marker(marker: &EnrollmentMarker, path: &Path) -> Result<()> {
    validate_project_id(&marker.project_id).map_err(|message| TraceDecayError::Config {
        message: format!("invalid enrollment marker '{}': {message}", path.display()),
    })
}

pub fn validate_project_id(project_id: &str) -> std::result::Result<(), &'static str> {
    if project_id.is_empty() {
        return Err("project_id must not be empty");
    }
    if project_id.starts_with('.')
        || project_id.contains('/')
        || project_id.contains('\\')
        || project_id.contains("..")
    {
        return Err("project_id must be a single safe path segment");
    }
    if !project_id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
    {
        return Err("project_id contains unsupported characters");
    }
    Ok(())
}

fn validate_no_nul(path: &Path) -> Result<()> {
    if path.to_string_lossy().contains('\0') {
        return Err(TraceDecayError::Config {
            message: format!("path '{}' contains a NUL byte", path.display()),
        });
    }
    Ok(())
}

fn validate_normal_components(path: &Path, allow_absolute: bool) -> Result<()> {
    if path.as_os_str().is_empty() || has_current_dir_segment(path) {
        return Err(TraceDecayError::Config {
            message: format!("path '{}' is not normalized", path.display()),
        });
    }
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            Component::RootDir | Component::Prefix(_) if allow_absolute => {}
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(TraceDecayError::Config {
                    message: format!("path '{}' is not normalized", path.display()),
                });
            }
        }
    }
    Ok(())
}

fn has_current_dir_segment(path: &Path) -> bool {
    let text = path.to_string_lossy();
    text == "."
        || text.starts_with("./")
        || text.starts_with(".\\")
        || text.ends_with("/.")
        || text.ends_with("\\.")
        || text.contains("/./")
        || text.contains("\\.\\")
}

#[cfg(unix)]
pub fn set_private_dir_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)] // Keep platform implementations signature-compatible.
pub fn set_private_dir_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(unix)]
fn apply_private_create_mode(options: &mut fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.mode(0o600);
}

#[cfg(not(unix))]
fn apply_private_create_mode(_options: &mut fs::OpenOptions) {}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)] // Keep platform implementations signature-compatible.
fn set_private_file_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}
