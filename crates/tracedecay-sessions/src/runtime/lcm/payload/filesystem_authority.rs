use std::fmt;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[cfg(windows)]
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;

use super::LcmError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PayloadFileIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(windows)]
    volume_serial_number: u64,
    #[cfg(windows)]
    file_id: [u8; 16],
}

/// Opaque proof that a payload's locator, stable file identity, digest, and
/// byte/character sizes were observed together.
///
/// The fields and constructors are private so callers cannot manufacture a
/// deletion authority from unverified metadata.
///
/// ```compile_fail
/// use tracedecay::sessions::lcm::payload::VerifiedPayloadAuthority;
///
/// let _forged = VerifiedPayloadAuthority {};
/// ```
pub struct VerifiedPayloadAuthority {
    locator: PathBuf,
    identity: PayloadFileIdentity,
    content_hash: String,
    byte_count: u64,
    char_count: u64,
}

impl fmt::Debug for VerifiedPayloadAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedPayloadAuthority")
            .field("identity", &self.identity)
            .field("content_hash", &self.content_hash)
            .field("byte_count", &self.byte_count)
            .field("char_count", &self.char_count)
            .field("locator", &"<redacted>")
            .finish()
    }
}

#[derive(Debug)]
pub(super) struct PayloadFileWrite {
    pub(super) created: bool,
    pub(super) authority: VerifiedPayloadAuthority,
}

#[cfg(target_os = "linux")]
const O_NOFOLLOW: i32 = 0o40_0000;
const MAX_VERIFIED_PAYLOAD_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PAYLOAD_READ_PREALLOC_BYTES: usize = 64 * 1024;

#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
#[cfg(windows)]
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
#[cfg(windows)]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
#[cfg(windows)]
const FILE_SHARE_READ: u32 = 0x0000_0001;
#[cfg(windows)]
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
#[cfg(windows)]
const FILE_SHARE_DELETE: u32 = 0x0000_0004;
#[cfg(windows)]
const GENERIC_READ: u32 = 0x8000_0000;
#[cfg(windows)]
const GENERIC_WRITE: u32 = 0x4000_0000;
#[cfg(windows)]
const DELETE_ACCESS: u32 = 0x0001_0000;
#[cfg(windows)]
const FILE_DISPOSITION_INFO_CLASS: i32 = 4;
#[cfg(windows)]
const FILE_ID_INFO_CLASS: i32 = 18;
#[cfg(windows)]
const ERROR_SHARING_VIOLATION: i32 = 32;
#[cfg(windows)]
const ERROR_LOCK_VIOLATION: i32 = 33;

#[cfg(windows)]
#[derive(Clone, Copy)]
#[repr(C)]
struct FileId128 {
    identifier: [u8; 16],
}

#[cfg(windows)]
#[derive(Clone, Copy)]
#[repr(C)]
struct FileIdInfo {
    volume_serial_number: u64,
    file_id: FileId128,
}

pub(super) fn remove_verified_payload_file(
    authority: &VerifiedPayloadAuthority,
) -> Result<bool, LcmError> {
    remove_verified_payload_file_with(authority, || Ok(()))
}

fn remove_verified_payload_file_with<F>(
    authority: &VerifiedPayloadAuthority,
    before_verify: F,
) -> Result<bool, LcmError>
where
    F: FnOnce() -> Result<(), LcmError>,
{
    let path = &authority.locator;
    #[cfg(windows)]
    let _parent_guard = open_verified_parent(path)?;
    #[cfg(windows)]
    let opened = open_verified_payload_file_for_delete(path)?;
    #[cfg(not(windows))]
    let opened = open_verified_payload_file(path)?;
    let Some((mut file, _opened, _lstat, identity)) = opened else {
        return Ok(false);
    };
    same_payload_file_identity(&identity, &authority.identity)?;
    before_verify()?;
    let content = read_stable_payload_bytes_bounded_with(
        &mut file,
        path,
        &authority.identity,
        authority.byte_count,
        || Ok(()),
    )?;
    verify_authority_content(authority, path, &identity, &content)?;

    #[cfg(windows)]
    {
        delete_open_file_windows(&file)?;
        Ok(true)
    }

    #[cfg(not(windows))]
    {
        fs::remove_file(path).map_err(|err| LcmError::Io(err.to_string()))?;
        Ok(true)
    }
}

pub(super) fn inspect_payload_file_for_delete(path: &Path) -> Result<(bool, u64), LcmError> {
    Ok(match open_verified_payload_file(path)? {
        Some((_file, opened, _lstat, _identity)) => (true, opened.len()),
        None => (false, 0),
    })
}

pub(super) fn read_payload_file_for_verify(
    path: &Path,
) -> Result<Option<(Vec<u8>, VerifiedPayloadAuthority)>, LcmError> {
    read_payload_file_for_verify_bounded(path, MAX_VERIFIED_PAYLOAD_FILE_BYTES)
}

fn read_payload_file_for_verify_bounded(
    path: &Path,
    max_bytes: u64,
) -> Result<Option<(Vec<u8>, VerifiedPayloadAuthority)>, LcmError> {
    let Some((mut file, _opened, _lstat, identity)) = open_verified_payload_file(path)? else {
        return Ok(None);
    };
    let content =
        read_stable_payload_bytes_bounded_with(&mut file, path, &identity, max_bytes, || Ok(()))?;
    let authority = authority_for_content(path, identity, &content)?;
    Ok(Some((content, authority)))
}

pub(super) fn verify_payload_file_authority(
    path: &Path,
    expected_hash: &str,
    expected_bytes: u64,
    expected_chars: u64,
) -> Result<Option<VerifiedPayloadAuthority>, LcmError> {
    Ok(
        read_verified_payload_file(path, expected_hash, expected_bytes, expected_chars)?
            .map(|(_content, authority)| authority),
    )
}

pub(super) fn read_verified_payload_file(
    path: &Path,
    expected_hash: &str,
    expected_bytes: u64,
    expected_chars: u64,
) -> Result<Option<(Vec<u8>, VerifiedPayloadAuthority)>, LcmError> {
    if expected_bytes > MAX_VERIFIED_PAYLOAD_FILE_BYTES {
        return Err(LcmError::PayloadIntegrityMismatch);
    }
    let Some((content, authority)) = read_payload_file_for_verify_bounded(path, expected_bytes)?
    else {
        return Ok(None);
    };
    if authority.content_hash != expected_hash
        || authority.byte_count != expected_bytes
        || authority.char_count != expected_chars
    {
        return Err(LcmError::PayloadIntegrityMismatch);
    }
    debug_assert_eq!(content.len() as u64, authority.byte_count);
    Ok(Some((content, authority)))
}

pub(super) fn open_verified_payload_file(
    path: &Path,
) -> Result<Option<(fs::File, fs::Metadata, fs::Metadata, PayloadFileIdentity)>, LcmError> {
    let mut options = private_file_options();
    options.read(true);
    open_verified_payload_file_with(path, &mut options)
}

#[cfg(windows)]
fn open_verified_payload_file_for_delete(
    path: &Path,
) -> Result<Option<(fs::File, fs::Metadata, fs::Metadata, PayloadFileIdentity)>, LcmError> {
    open_verified_payload_file_with(path, &mut delete_file_options())
}

fn open_verified_payload_file_with(
    path: &Path,
    options: &mut fs::OpenOptions,
) -> Result<Option<(fs::File, fs::Metadata, fs::Metadata, PayloadFileIdentity)>, LcmError> {
    #[cfg(windows)]
    let _parent_guard = open_verified_parent(path)?;

    let pre_open = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(LcmError::Io(err.to_string())),
    };
    ensure_regular_non_reparse_file(&pre_open)?;

    let file = match options.open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(classify_payload_open_error(path, err)),
    };
    let (opened, lstat, identity) = verify_opened_payload_file(&file, path)?;
    Ok(Some((file, opened, lstat, identity)))
}

fn read_stable_payload_bytes_with<F>(
    file: &mut fs::File,
    path: &Path,
    expected_identity: &PayloadFileIdentity,
    after_read: F,
) -> Result<Vec<u8>, LcmError>
where
    F: FnOnce() -> Result<(), LcmError>,
{
    read_stable_payload_bytes_bounded_with(
        file,
        path,
        expected_identity,
        MAX_VERIFIED_PAYLOAD_FILE_BYTES,
        after_read,
    )
}

fn read_stable_payload_bytes_bounded_with<F>(
    file: &mut fs::File,
    path: &Path,
    expected_identity: &PayloadFileIdentity,
    max_bytes: u64,
    after_read: F,
) -> Result<Vec<u8>, LcmError>
where
    F: FnOnce() -> Result<(), LcmError>,
{
    let max_bytes = max_bytes.min(MAX_VERIFIED_PAYLOAD_FILE_BYTES);
    let before = file
        .metadata()
        .map_err(|error| LcmError::Io(error.to_string()))?;
    ensure_regular_non_reparse_file(&before)?;
    let before_identity = payload_file_identity(file, &before)?;
    same_payload_file_identity(&before_identity, expected_identity)?;
    if before.len() > max_bytes {
        return Err(LcmError::PayloadIntegrityMismatch);
    }

    let initial_capacity = usize::try_from(before.len())
        .unwrap_or(MAX_PAYLOAD_READ_PREALLOC_BYTES)
        .min(MAX_PAYLOAD_READ_PREALLOC_BYTES);
    let mut content = Vec::with_capacity(initial_capacity);
    file.seek(SeekFrom::Start(0))
        .map_err(|error| LcmError::Io(error.to_string()))?;
    {
        let mut bounded = file.take(max_bytes.saturating_add(1));
        bounded
            .read_to_end(&mut content)
            .map_err(|error| LcmError::Io(error.to_string()))?;
    }
    if u64::try_from(content.len()).map_or(true, |length| length > max_bytes) {
        return Err(LcmError::PayloadIntegrityMismatch);
    }
    after_read()?;

    let (after, _lstat, after_identity) = verify_opened_payload_file(file, path)?;
    same_payload_file_identity(&after_identity, expected_identity)?;
    if before.len() != after.len() || after.len() != content.len() as u64 {
        return Err(LcmError::PayloadIntegrityMismatch);
    }
    Ok(content)
}

fn authority_for_content(
    path: &Path,
    identity: PayloadFileIdentity,
    content: &[u8],
) -> Result<VerifiedPayloadAuthority, LcmError> {
    let text = std::str::from_utf8(content).map_err(|_| LcmError::PayloadIntegrityMismatch)?;
    Ok(VerifiedPayloadAuthority {
        locator: path.to_path_buf(),
        identity,
        content_hash: super::util::sha256_hex(content),
        byte_count: content.len() as u64,
        char_count: text.chars().count() as u64,
    })
}

fn verify_authority_content(
    authority: &VerifiedPayloadAuthority,
    path: &Path,
    identity: &PayloadFileIdentity,
    content: &[u8],
) -> Result<(), LcmError> {
    if authority.locator != path {
        return Err(LcmError::InvalidPayloadRef);
    }
    same_payload_file_identity(identity, &authority.identity)?;
    let actual = authority_for_content(path, *identity, content)?;
    if actual.content_hash != authority.content_hash
        || actual.byte_count != authority.byte_count
        || actual.char_count != authority.char_count
    {
        return Err(LcmError::PayloadIntegrityMismatch);
    }
    Ok(())
}

fn verify_opened_payload_file(
    file: &fs::File,
    path: &Path,
) -> Result<(fs::Metadata, fs::Metadata, PayloadFileIdentity), LcmError> {
    let opened = file
        .metadata()
        .map_err(|err| LcmError::Io(err.to_string()))?;
    ensure_regular_non_reparse_file(&opened)?;
    let lstat = fs::symlink_metadata(path).map_err(|err| LcmError::Io(err.to_string()))?;
    ensure_regular_non_reparse_file(&lstat)?;
    same_file_identity(file, &opened, &lstat, path)?;
    let identity = payload_file_identity(file, &opened)?;
    Ok((opened, lstat, identity))
}

#[cfg(unix)]
fn same_file_identity(
    _file: &fs::File,
    opened: &fs::Metadata,
    lstat: &fs::Metadata,
    _path: &Path,
) -> Result<(), LcmError> {
    use std::os::unix::fs::MetadataExt;

    if opened.dev() == lstat.dev() && opened.ino() == lstat.ino() {
        Ok(())
    } else {
        Err(LcmError::InvalidPayloadRef)
    }
}

#[cfg(unix)]
fn payload_file_identity(
    _file: &fs::File,
    metadata: &fs::Metadata,
) -> Result<PayloadFileIdentity, LcmError> {
    use std::os::unix::fs::MetadataExt;

    Ok(PayloadFileIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
    })
}

#[cfg(unix)]
pub(super) fn same_payload_file_identity(
    actual: &PayloadFileIdentity,
    expected: &PayloadFileIdentity,
) -> Result<(), LcmError> {
    if actual == expected {
        Ok(())
    } else {
        Err(LcmError::InvalidPayloadRef)
    }
}

#[cfg(windows)]
fn same_file_identity(
    file: &fs::File,
    _opened: &fs::Metadata,
    _lstat: &fs::Metadata,
    path: &Path,
) -> Result<(), LcmError> {
    let current = verification_file_options()
        .read(true)
        .open(path)
        .map_err(|err| classify_payload_open_error(path, err))?;
    let current_metadata = current
        .metadata()
        .map_err(|err| LcmError::Io(err.to_string()))?;
    ensure_regular_non_reparse_file(&current_metadata)?;
    same_windows_handle_identity(file, &current)
}

#[cfg(windows)]
fn payload_file_identity(
    file: &fs::File,
    _metadata: &fs::Metadata,
) -> Result<PayloadFileIdentity, LcmError> {
    windows_file_identity(file)
}

#[cfg(windows)]
pub(super) fn same_payload_file_identity(
    actual: &PayloadFileIdentity,
    expected: &PayloadFileIdentity,
) -> Result<(), LcmError> {
    if actual == expected {
        Ok(())
    } else {
        Err(LcmError::InvalidPayloadRef)
    }
}

#[cfg(all(not(unix), not(windows)))]
#[allow(clippy::unnecessary_wraps)] // Keep platform implementations signature-compatible.
fn same_file_identity(
    _file: &fs::File,
    _opened: &fs::Metadata,
    _lstat: &fs::Metadata,
    _path: &Path,
) -> Result<(), LcmError> {
    Ok(())
}

#[cfg(all(not(unix), not(windows)))]
fn payload_file_identity(
    _file: &fs::File,
    _metadata: &fs::Metadata,
) -> Result<PayloadFileIdentity, LcmError> {
    Ok(PayloadFileIdentity {})
}

#[cfg(all(not(unix), not(windows)))]
#[allow(clippy::trivially_copy_pass_by_ref, clippy::unnecessary_wraps)] // Keep the identity API uniform even where the platform identity is opaque.
pub(super) fn same_payload_file_identity(
    _actual: &PayloadFileIdentity,
    _expected: &PayloadFileIdentity,
) -> Result<(), LcmError> {
    Ok(())
}

fn ensure_regular_non_reparse_file(metadata: &fs::Metadata) -> Result<(), LcmError> {
    if metadata.file_type().is_symlink()
        || metadata_is_reparse_point(metadata)
        || !metadata.is_file()
    {
        Err(LcmError::InvalidPayloadRef)
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn metadata_is_reparse_point(metadata: &fs::Metadata) -> bool {
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn metadata_is_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

fn classify_payload_path_error(path: &Path, error: std::io::Error) -> LcmError {
    if fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.file_type().is_symlink()
            || metadata_is_reparse_point(&metadata)
            || !metadata.is_file()
    }) {
        LcmError::InvalidPayloadRef
    } else {
        LcmError::Io(error.to_string())
    }
}

#[cfg(windows)]
fn classify_payload_open_error(path: &Path, error: std::io::Error) -> LcmError {
    if is_windows_sharing_violation(&error) {
        LcmError::Io(error.to_string())
    } else {
        classify_payload_path_error(path, error)
    }
}

#[cfg(not(windows))]
fn classify_payload_open_error(path: &Path, error: std::io::Error) -> LcmError {
    classify_payload_path_error(path, error)
}

#[cfg(windows)]
fn is_windows_sharing_violation(error: &std::io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)
    )
}

#[cfg(windows)]
fn same_windows_handle_identity(left: &fs::File, right: &fs::File) -> Result<(), LcmError> {
    let left = windows_file_identity(left)?;
    let right = windows_file_identity(right)?;
    same_payload_file_identity(&left, &right)
}

#[cfg(windows)]
fn windows_file_identity(file: &fs::File) -> Result<PayloadFileIdentity, LcmError> {
    query_windows_file_identity_with(
        file.as_raw_handle(),
        |handle, information_class, information, buffer_size| {
            // SAFETY: `file` owns `handle` for the duration of this call and
            // `information` points to a writable `FileIdInfo` of `buffer_size`.
            unsafe {
                get_file_information_by_handle_ex(
                    handle,
                    information_class,
                    information,
                    buffer_size,
                )
            }
        },
    )
}

#[cfg(windows)]
fn query_windows_file_identity_with(
    handle: *mut std::ffi::c_void,
    query: impl FnOnce(*mut std::ffi::c_void, i32, *mut std::ffi::c_void, u32) -> i32,
) -> Result<PayloadFileIdentity, LcmError> {
    let mut information = FileIdInfo {
        volume_serial_number: u64::MAX,
        file_id: FileId128 {
            identifier: [u8::MAX; 16],
        },
    };
    let succeeded = query(
        handle,
        FILE_ID_INFO_CLASS,
        (&raw mut information).cast(),
        std::mem::size_of::<FileIdInfo>() as u32,
    );
    if succeeded == 0 {
        return Err(LcmError::Io(std::io::Error::last_os_error().to_string()));
    }
    validate_file_id_info(information)
}

#[cfg(windows)]
fn validate_file_id_info(information: FileIdInfo) -> Result<PayloadFileIdentity, LcmError> {
    let file_id = information.file_id.identifier;
    let invalid_volume =
        information.volume_serial_number == 0 || information.volume_serial_number == u64::MAX;
    let invalid_file_id =
        file_id.iter().all(|byte| *byte == 0) || file_id.iter().all(|byte| *byte == u8::MAX);
    if invalid_volume || invalid_file_id {
        return Err(LcmError::InvalidPayloadRef);
    }
    Ok(PayloadFileIdentity {
        volume_serial_number: information.volume_serial_number,
        file_id,
    })
}

#[cfg(windows)]
fn open_verified_parent(path: &Path) -> Result<fs::File, LcmError> {
    let parent = path.parent().ok_or(LcmError::InvalidPayloadRef)?;
    open_verified_directory(parent)
}

#[cfg(windows)]
fn open_verified_directory(path: &Path) -> Result<fs::File, LcmError> {
    let before = fs::symlink_metadata(path).map_err(|err| LcmError::Io(err.to_string()))?;
    ensure_directory_non_reparse(&before)?;

    let directory = private_directory_options()
        .read(true)
        .open(path)
        .map_err(|err| classify_directory_path_error(path, err))?;
    let opened = directory
        .metadata()
        .map_err(|err| LcmError::Io(err.to_string()))?;
    ensure_directory_non_reparse(&opened)?;

    let after = fs::symlink_metadata(path).map_err(|err| LcmError::Io(err.to_string()))?;
    ensure_directory_non_reparse(&after)?;
    let current = private_directory_options()
        .read(true)
        .open(path)
        .map_err(|err| classify_directory_path_error(path, err))?;
    same_windows_handle_identity(&directory, &current)?;
    Ok(directory)
}

#[cfg(windows)]
fn ensure_directory_non_reparse(metadata: &fs::Metadata) -> Result<(), LcmError> {
    if metadata.file_type().is_symlink()
        || metadata_is_reparse_point(metadata)
        || !metadata.is_dir()
    {
        Err(LcmError::InvalidPayloadRef)
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn classify_directory_path_error(path: &Path, error: std::io::Error) -> LcmError {
    if fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.file_type().is_symlink()
            || metadata_is_reparse_point(&metadata)
            || !metadata.is_dir()
    }) {
        LcmError::InvalidPayloadRef
    } else {
        LcmError::Io(error.to_string())
    }
}

pub(super) fn prepare_payload_dir(storage_root: &Path) -> Result<PathBuf, LcmError> {
    let root = super::canonical_storage_root(storage_root)?;
    #[cfg(windows)]
    let _root_guard = open_verified_directory(&root)?;
    let dir = root.join("lcm-payloads");
    match fs::symlink_metadata(&dir) {
        Ok(metadata) => ensure_actual_private_dir(&dir, &metadata)?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(&dir).map_err(|err| {
                fs::symlink_metadata(&dir).map_or_else(
                    |_| LcmError::Io(err.to_string()),
                    |metadata| {
                        if metadata.file_type().is_symlink()
                            || metadata_is_reparse_point(&metadata)
                            || !metadata.is_dir()
                        {
                            LcmError::InvalidPayloadRef
                        } else {
                            LcmError::Io(err.to_string())
                        }
                    },
                )
            })?;
            set_private_dir_permissions(&dir)?;
        }
        Err(err) => return Err(LcmError::Io(err.to_string())),
    }
    #[cfg(windows)]
    let _dir_guard = open_verified_directory(&dir)?;
    ensure_payload_dir_under_root(&root, &dir)?;
    Ok(dir)
}

pub fn existing_payload_dir(storage_root: &Path) -> Result<PathBuf, LcmError> {
    existing_payload_dir_opt(storage_root)?.ok_or_else(|| {
        LcmError::Io(format!(
            "payload directory missing under {}",
            storage_root.display()
        ))
    })
}

/// Like `existing_payload_dir`, but a payload directory that was never
/// created (it is made lazily on first externalization) or has been removed
/// reports as `None` instead of an I/O error. Invalid configurations —
/// symlinked dir, wrong file type, dir escaping the storage root — still
/// error.
pub fn existing_payload_dir_opt(storage_root: &Path) -> Result<Option<PathBuf>, LcmError> {
    let root = super::canonical_storage_root(storage_root)?;
    #[cfg(windows)]
    let _root_guard = open_verified_directory(&root)?;
    let dir = root.join("lcm-payloads");
    let metadata = match fs::symlink_metadata(&dir) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(LcmError::Io(err.to_string())),
    };
    ensure_actual_private_dir(&dir, &metadata)?;
    #[cfg(windows)]
    let _dir_guard = open_verified_directory(&dir)?;
    ensure_payload_dir_under_root(&root, &dir)?;
    Ok(Some(dir))
}

#[cfg(not(windows))]
pub(super) fn canonical_storage_root(storage_root: &Path) -> Result<PathBuf, LcmError> {
    let metadata =
        fs::symlink_metadata(storage_root).map_err(|err| LcmError::Io(err.to_string()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(LcmError::InvalidPayloadRef);
    }
    storage_root
        .canonicalize()
        .map_err(|err| LcmError::Io(err.to_string()))
}

#[cfg(windows)]
pub fn canonical_storage_root(storage_root: &Path) -> Result<PathBuf, LcmError> {
    let root = open_verified_directory(storage_root)?;
    let canonical = storage_root
        .canonicalize()
        .map_err(|err| LcmError::Io(err.to_string()))?;
    let canonical_root = open_verified_directory(&canonical)?;
    same_windows_handle_identity(&root, &canonical_root)?;
    Ok(canonical)
}

fn ensure_actual_private_dir(dir: &Path, metadata: &fs::Metadata) -> Result<(), LcmError> {
    if metadata.file_type().is_symlink()
        || metadata_is_reparse_point(metadata)
        || !metadata.is_dir()
    {
        return Err(LcmError::InvalidPayloadRef);
    }
    set_private_dir_permissions(dir)?;
    Ok(())
}

#[cfg(not(windows))]
fn ensure_payload_dir_under_root(root: &Path, dir: &Path) -> Result<(), LcmError> {
    let canonical_dir = dir
        .canonicalize()
        .map_err(|err| LcmError::Io(err.to_string()))?;
    if canonical_dir.parent() == Some(root) {
        Ok(())
    } else {
        Err(LcmError::InvalidPayloadRef)
    }
}

#[cfg(windows)]
fn ensure_payload_dir_under_root(root: &Path, dir: &Path) -> Result<(), LcmError> {
    let root_handle = open_verified_directory(root)?;
    let dir_handle = open_verified_directory(dir)?;
    let canonical_dir = dir
        .canonicalize()
        .map_err(|err| LcmError::Io(err.to_string()))?;
    let parent = canonical_dir.parent().ok_or(LcmError::InvalidPayloadRef)?;
    let parent_handle = open_verified_directory(parent)?;
    let canonical_dir_handle = open_verified_directory(&canonical_dir)?;
    same_windows_handle_identity(&root_handle, &parent_handle)?;
    same_windows_handle_identity(&dir_handle, &canonical_dir_handle)
}

#[cfg(not(windows))]
pub fn ensure_contained(root: &Path, path: &Path) -> Result<(), LcmError> {
    let parent = path.parent().ok_or(LcmError::InvalidPayloadRef)?;
    if parent == root {
        Ok(())
    } else {
        Err(LcmError::InvalidPayloadRef)
    }
}

#[cfg(windows)]
pub fn ensure_contained(root: &Path, path: &Path) -> Result<(), LcmError> {
    let parent = path.parent().ok_or(LcmError::InvalidPayloadRef)?;
    if parent != root {
        return Err(LcmError::InvalidPayloadRef);
    }
    let root_handle = open_verified_directory(root)?;
    let parent_handle = open_verified_directory(parent)?;
    same_windows_handle_identity(&root_handle, &parent_handle)
}

pub(super) fn write_private_file(
    path: &Path,
    content: &[u8],
) -> Result<PayloadFileWrite, LcmError> {
    #[cfg(windows)]
    {
        write_private_file_windows(path, content)
    }

    #[cfg(not(windows))]
    {
        write_private_file_non_windows(path, content)
    }
}

#[cfg(not(windows))]
fn write_private_file_non_windows(
    path: &Path,
    content: &[u8],
) -> Result<PayloadFileWrite, LcmError> {
    let file = match private_create_file_options()
        .create_new(true)
        .read(true)
        .write(true)
        .open(path)
    {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            let authority = ensure_existing_payload_matches(path, content)?;
            return Ok(PayloadFileWrite {
                created: false,
                authority,
            });
        }
        Err(err) => return Err(LcmError::Io(err.to_string())),
    };
    let identity = match verify_opened_payload_file(&file, path) {
        Ok((_, _, identity)) => identity,
        Err(error) => {
            remove_failed_payload_create(path, &file);
            drop(file);
            return Err(error);
        }
    };
    finish_private_file_write(path, content, file, identity)
}

#[cfg(windows)]
fn write_private_file_windows(path: &Path, content: &[u8]) -> Result<PayloadFileWrite, LcmError> {
    let _parent_guard = open_verified_parent(path)?;
    let mut file = match private_create_file_options()
        .create_new(true)
        .write(true)
        .open(path)
    {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            let authority = ensure_existing_payload_matches(path, content)?;
            return Ok(PayloadFileWrite {
                created: false,
                authority,
            });
        }
        Err(err) => return Err(classify_payload_open_error(path, err)),
    };
    let identity = match file
        .metadata()
        .map_err(|error| LcmError::Io(error.to_string()))
        .and_then(|metadata| {
            ensure_regular_non_reparse_file(&metadata)?;
            payload_file_identity(&file, &metadata)
        }) {
        Ok(identity) => identity,
        Err(error) => {
            remove_failed_payload_create(path, &file);
            drop(file);
            return Err(error);
        }
    };
    if let Err(error) = file.write_all(content).and_then(|()| file.sync_all()) {
        remove_failed_payload_create(path, &file);
        return Err(LcmError::Io(error.to_string()));
    }
    drop(file);

    let Some((mut verification, _opened, _lstat, actual_identity)) =
        open_verified_payload_file(path)?
    else {
        return Err(LcmError::Io(format!(
            "payload file disappeared while verifying {}",
            path.display()
        )));
    };
    same_payload_file_identity(&actual_identity, &identity)?;
    let actual = read_stable_payload_bytes_with(&mut verification, path, &identity, || Ok(()))?;
    let authority = authority_for_content(path, identity, &actual)?;
    if actual != content {
        let _ = remove_verified_payload_file(&authority);
        return Err(LcmError::PayloadIntegrityMismatch);
    }
    Ok(PayloadFileWrite {
        created: true,
        authority,
    })
}

#[cfg(not(windows))]
fn finish_private_file_write(
    path: &Path,
    content: &[u8],
    mut file: fs::File,
    identity: PayloadFileIdentity,
) -> Result<PayloadFileWrite, LcmError> {
    if let Err(error) = file.write_all(content).and_then(|()| file.sync_all()) {
        remove_failed_payload_create(path, &file);
        return Err(LcmError::Io(error.to_string()));
    }
    let verification = (|| {
        let actual = read_stable_payload_bytes_with(&mut file, path, &identity, || Ok(()))?;
        if actual != content {
            return Err(LcmError::PayloadIntegrityMismatch);
        }
        authority_for_content(path, identity, &actual)
    })();
    match verification {
        Ok(authority) => Ok(PayloadFileWrite {
            created: true,
            authority,
        }),
        Err(error) => {
            remove_failed_payload_create(path, &file);
            Err(error)
        }
    }
}

#[cfg(windows)]
fn remove_failed_payload_create(_path: &Path, file: &fs::File) {
    let _ = delete_open_file_windows(file);
}

#[cfg(not(windows))]
fn remove_failed_payload_create(path: &Path, _file: &fs::File) {
    let _ = fs::remove_file(path);
}

fn ensure_existing_payload_matches(
    path: &Path,
    content: &[u8],
) -> Result<VerifiedPayloadAuthority, LcmError> {
    let Some((existing, authority)) = read_payload_file_for_verify(path)? else {
        return Err(LcmError::Io(format!(
            "payload file disappeared while opening {}",
            path.display()
        )));
    };
    if existing == content {
        Ok(authority)
    } else {
        Err(LcmError::PayloadIntegrityMismatch)
    }
}

#[cfg(unix)]
fn private_file_options() -> fs::OpenOptions {
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = fs::OpenOptions::new();
    options.mode(0o600);
    #[cfg(target_os = "linux")]
    options.custom_flags(O_NOFOLLOW);
    options
}

#[cfg(all(not(unix), not(windows)))]
fn private_file_options() -> fs::OpenOptions {
    fs::OpenOptions::new()
}

#[cfg(not(windows))]
fn private_create_file_options() -> fs::OpenOptions {
    private_file_options()
}

#[cfg(windows)]
fn private_file_options() -> fs::OpenOptions {
    let mut options = fs::OpenOptions::new();
    options
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE);
    options
}

#[cfg(windows)]
fn private_create_file_options() -> fs::OpenOptions {
    let mut options = private_file_options();
    options.access_mode(GENERIC_READ | GENERIC_WRITE | DELETE_ACCESS);
    options
}

#[cfg(windows)]
fn verification_file_options() -> fs::OpenOptions {
    let mut options = fs::OpenOptions::new();
    options
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE);
    options
}

#[cfg(windows)]
fn private_directory_options() -> fs::OpenOptions {
    let mut options = fs::OpenOptions::new();
    options
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
    options
}

#[cfg(windows)]
fn delete_file_options() -> fs::OpenOptions {
    let mut options = fs::OpenOptions::new();
    options
        .access_mode(GENERIC_READ | DELETE_ACCESS)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .share_mode(FILE_SHARE_READ);
    options
}

#[cfg(windows)]
fn delete_open_file_windows(file: &fs::File) -> Result<(), LcmError> {
    let disposition = FileDispositionInfo { delete_file: 1 };
    // SAFETY: `file` owns a valid handle opened with DELETE access, and
    // `disposition` is the complete input structure required by this class.
    let succeeded = unsafe {
        set_file_information_by_handle(
            file.as_raw_handle(),
            FILE_DISPOSITION_INFO_CLASS,
            (&raw const disposition).cast_mut().cast(),
            std::mem::size_of::<FileDispositionInfo>() as u32,
        )
    };
    if succeeded == 0 {
        return Err(LcmError::Io(std::io::Error::last_os_error().to_string()));
    }
    Ok(())
}

#[cfg(windows)]
#[repr(C)]
struct FileDispositionInfo {
    delete_file: u8,
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    #[link_name = "GetFileInformationByHandleEx"]
    fn get_file_information_by_handle_ex(
        file: *mut std::ffi::c_void,
        information_class: i32,
        information: *mut std::ffi::c_void,
        buffer_size: u32,
    ) -> i32;

    #[link_name = "SetFileInformationByHandle"]
    fn set_file_information_by_handle(
        file: *mut std::ffi::c_void,
        information_class: i32,
        information: *mut std::ffi::c_void,
        buffer_size: u32,
    ) -> i32;
}

fn set_private_dir_permissions(path: &Path) -> Result<(), LcmError> {
    tracedecay_runtime_core::storage::set_private_dir_permissions(path)
        .map_err(|err| LcmError::Io(err.to_string()))
}

#[cfg(all(test, windows))]
mod windows_tests {
    use std::cell::Cell;
    use std::mem::{align_of, offset_of, size_of};
    use std::os::windows::fs::{symlink_dir, symlink_file};
    use std::process::Command;

    use super::*;

    fn raw_file_id_info(volume_serial_number: u64, file_id: [u8; 16]) -> FileIdInfo {
        FileIdInfo {
            volume_serial_number,
            file_id: FileId128 {
                identifier: file_id,
            },
        }
    }

    #[test]
    fn file_id_info_layout_matches_windows_abi() {
        assert_eq!(size_of::<FileId128>(), 16);
        assert_eq!(align_of::<FileId128>(), 1);
        assert_eq!(size_of::<FileIdInfo>(), 24);
        assert_eq!(align_of::<FileIdInfo>(), 8);
        assert_eq!(offset_of!(FileIdInfo, volume_serial_number), 0);
        assert_eq!(offset_of!(FileIdInfo, file_id), 8);
        assert_eq!(FILE_ID_INFO_CLASS, 18);
    }

    #[test]
    fn exact_volume_and_128_bit_file_id_are_required() {
        let refs_id = [
            0x71, 0x3a, 0x99, 0x08, 0x4c, 0xde, 0x17, 0x51, 0x82, 0x6b, 0xb4, 0x20, 0xea, 0x77,
            0x05, 0xc9,
        ];
        let identity = validate_file_id_info(raw_file_id_info(0x1020_3040_5060_7080, refs_id))
            .expect("ReFS-shaped identity should be accepted");
        assert_eq!(identity.volume_serial_number, 0x1020_3040_5060_7080);
        assert_eq!(same_payload_file_identity(&identity, &identity), Ok(()));

        let mut different_high_bits = refs_id;
        different_high_bits[15] ^= 0x80;
        let different_id =
            validate_file_id_info(raw_file_id_info(0x1020_3040_5060_7080, different_high_bits))
                .unwrap();
        assert_eq!(
            same_payload_file_identity(&identity, &different_id),
            Err(LcmError::InvalidPayloadRef)
        );

        let different_volume =
            validate_file_id_info(raw_file_id_info(0x8877_6655_5060_7080, refs_id)).unwrap();
        assert_eq!(
            same_payload_file_identity(&identity, &different_volume),
            Err(LcmError::InvalidPayloadRef)
        );
    }

    #[test]
    fn ntfs_and_refs_shaped_file_ids_are_valid() {
        let ntfs = validate_file_id_info(raw_file_id_info(
            0x55aa_0123_89ab_cdef,
            [
                0x32, 0x10, 0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
        ))
        .unwrap();
        assert_eq!(
            ntfs.file_id,
            [
                0x32, 0x10, 0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54, 0, 0, 0, 0, 0, 0, 0, 0,
            ]
        );

        let refs_id = [
            0x90, 0x8f, 0x7e, 0x6d, 0x5c, 0x4b, 0x3a, 0x29, 0x18, 0x07, 0xf6, 0xe5, 0xd4, 0xc3,
            0xb2, 0xa1,
        ];
        let refs = validate_file_id_info(raw_file_id_info(0xa5a5_5a5a_1234_5678, refs_id)).unwrap();
        assert_eq!(refs.file_id, refs_id);
    }

    #[test]
    fn successful_file_id_query_reads_the_complete_identity() {
        let expected = raw_file_id_info(
            0x1020_3040_5060_7080,
            [
                0x90, 0x8f, 0x7e, 0x6d, 0x5c, 0x4b, 0x3a, 0x29, 0x18, 0x07, 0xf6, 0xe5, 0xd4, 0xc3,
                0xb2, 0xa1,
            ],
        );
        let identity = query_windows_file_identity_with(
            std::ptr::null_mut(),
            |_, information_class, information, buffer_size| {
                assert_eq!(information_class, FILE_ID_INFO_CLASS);
                assert_eq!(buffer_size as usize, size_of::<FileIdInfo>());
                // SAFETY: The seam supplies a writable `FileIdInfo` buffer.
                unsafe {
                    information.cast::<FileIdInfo>().write(expected);
                }
                1
            },
        )
        .unwrap();

        assert_eq!(identity.volume_serial_number, expected.volume_serial_number);
        assert_eq!(identity.file_id, expected.file_id.identifier);
    }

    #[test]
    fn zero_sentinel_and_partial_file_id_info_are_rejected() {
        let valid_id = [0x42; 16];
        for invalid in [
            raw_file_id_info(0, valid_id),
            raw_file_id_info(u64::MAX, valid_id),
            raw_file_id_info(7, [0; 16]),
            raw_file_id_info(7, [u8::MAX; 16]),
        ] {
            assert_eq!(
                validate_file_id_info(invalid),
                Err(LcmError::InvalidPayloadRef)
            );
        }

        let volume_only = query_windows_file_identity_with(
            std::ptr::null_mut(),
            |_, information_class, information, buffer_size| {
                assert_eq!(information_class, FILE_ID_INFO_CLASS);
                assert_eq!(buffer_size as usize, size_of::<FileIdInfo>());
                // SAFETY: The seam supplies a writable `FileIdInfo` buffer.
                unsafe {
                    (*information.cast::<FileIdInfo>()).volume_serial_number = 7;
                }
                1
            },
        );
        assert_eq!(volume_only, Err(LcmError::InvalidPayloadRef));
    }

    #[test]
    fn unsupported_file_id_query_fails_closed_without_fallback() {
        let calls = Cell::new(0);
        let result = query_windows_file_identity_with(
            std::ptr::null_mut(),
            |_, information_class, _, buffer_size| {
                calls.set(calls.get() + 1);
                assert_eq!(information_class, FILE_ID_INFO_CLASS);
                assert_eq!(buffer_size as usize, size_of::<FileIdInfo>());
                0
            },
        );

        assert!(matches!(result, Err(LcmError::Io(_))));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn ordinary_unicode_payload_round_trips_with_stable_identity() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("存储-root");
        fs::create_dir(&root).unwrap();
        let dir = prepare_payload_dir(&root).unwrap();
        let path = dir.join("payload_雪.payload");

        assert!(
            write_private_file(&path, "héllo 雪".as_bytes())
                .unwrap()
                .created
        );
        assert_eq!(
            read_payload_file_for_verify(&path).unwrap().unwrap().0,
            "héllo 雪".as_bytes()
        );

        let (_, _, _, first_identity) = open_verified_payload_file(&path).unwrap().unwrap();
        let (_, _, _, second_identity) = open_verified_payload_file(&path).unwrap().unwrap();
        same_payload_file_identity(&first_identity, &second_identity).unwrap();
    }

    #[test]
    fn file_symlink_is_rejected_for_open_expand_and_create() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir(&root).unwrap();
        let dir = prepare_payload_dir(&root).unwrap();
        let target = temp.path().join("outside.payload");
        fs::write(&target, b"outside").unwrap();
        let link = dir.join("payload_link.payload");
        if let Err(error) = symlink_file(&target, &link) {
            if error.raw_os_error() == Some(1314) {
                return;
            }
            panic!("failed to create Windows file symlink: {error}");
        }

        assert_eq!(
            open_verified_payload_file(&link).unwrap_err(),
            LcmError::InvalidPayloadRef
        );
        assert_eq!(
            read_payload_file_for_verify(&link).unwrap_err(),
            LcmError::InvalidPayloadRef
        );
        assert_eq!(
            write_private_file(&link, b"outside").unwrap_err(),
            LcmError::InvalidPayloadRef
        );
    }

    #[test]
    fn directory_junction_or_reparse_point_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let outside = temp.path().join("outside");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        let link = root.join("lcm-payloads");

        let junction_created = Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(&link)
            .arg(&outside)
            .status()
            .is_ok_and(|status| status.success());
        if !junction_created
            && let Err(error) = symlink_dir(&outside, &link)
            && error.raw_os_error() != Some(1314)
        {
            panic!("failed to create Windows directory reparse point: {error}");
        }

        assert_eq!(
            prepare_payload_dir(&root).unwrap_err(),
            LcmError::InvalidPayloadRef
        );
        assert_eq!(
            existing_payload_dir_opt(&root).unwrap_err(),
            LcmError::InvalidPayloadRef
        );
    }

    fn writer_file_options() -> fs::OpenOptions {
        let mut options = fs::OpenOptions::new();
        options
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
        options
    }

    #[test]
    fn pre_existing_writer_blocks_authority_open() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir(&root).unwrap();
        let dir = prepare_payload_dir(&root).unwrap();
        let payload_ref = "payload_writer.payload";
        let path = dir.join(payload_ref);
        fs::write(&path, b"first").unwrap();

        let writer = writer_file_options().open(&path).unwrap();
        assert!(matches!(
            open_verified_payload_file(&path),
            Err(LcmError::Io(_))
        ));
        assert!(matches!(
            read_payload_file_for_verify(&path),
            Err(LcmError::Io(_))
        ));
        assert!(path.exists());

        drop(writer);
        let (_, authority) = read_payload_file_for_verify(&path).unwrap().unwrap();
        assert!(remove_verified_payload_file(&authority).unwrap());
        assert!(!path.exists());
    }

    #[test]
    fn authority_open_blocks_later_writer_and_in_place_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir(&root).unwrap();
        let dir = prepare_payload_dir(&root).unwrap();
        let path = dir.join("payload_mutation.payload");
        fs::write(&path, b"stable").unwrap();

        let (mut authority, _, _, identity) = open_verified_payload_file(&path).unwrap().unwrap();
        let content = read_stable_payload_bytes_with(&mut authority, &path, &identity, || {
            let error = writer_file_options().open(&path).unwrap_err();
            assert!(is_windows_sharing_violation(&error));
            Ok(())
        })
        .unwrap();

        assert_eq!(content, b"stable");
        assert_eq!(fs::read(&path).unwrap(), b"stable");
    }

    #[test]
    fn replacement_race_is_rejected_after_stable_handle_read() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir(&root).unwrap();
        let dir = prepare_payload_dir(&root).unwrap();
        let path = dir.join("payload_race.payload");
        let displaced = dir.join("payload_displaced.payload");
        fs::write(&path, b"first").unwrap();

        let (mut authority, _, _, identity) = open_verified_payload_file(&path).unwrap().unwrap();
        assert_eq!(
            read_stable_payload_bytes_with(&mut authority, &path, &identity, || {
                fs::rename(&path, &displaced).map_err(|error| LcmError::Io(error.to_string()))?;
                fs::write(&path, b"replacement")
                    .map_err(|error| LcmError::Io(error.to_string()))?;
                Ok(())
            })
            .unwrap_err(),
            LcmError::InvalidPayloadRef
        );
        assert_eq!(fs::read(&path).unwrap(), b"replacement");
        assert_eq!(fs::read(&displaced).unwrap(), b"first");
    }

    #[test]
    fn stable_read_bytes_match_reported_size_and_hash() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir(&root).unwrap();
        let dir = prepare_payload_dir(&root).unwrap();
        let path = dir.join("payload_hash.payload");
        let expected = b"stable hash input";
        fs::write(&path, expected).unwrap();

        let (content, _) = read_payload_file_for_verify(&path).unwrap().unwrap();
        assert_eq!(content.len() as u64, expected.len() as u64);
        assert_eq!(
            super::super::util::sha256_hex(&content),
            super::super::util::sha256_hex(expected)
        );
    }

    #[test]
    fn handle_delete_blocks_writer_and_removes_opened_file() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir(&root).unwrap();
        let dir = prepare_payload_dir(&root).unwrap();
        let path = dir.join("payload_delete.payload");
        fs::write(&path, b"pending").unwrap();

        let (_, authority) = read_payload_file_for_verify(&path).unwrap().unwrap();
        remove_verified_payload_file_with(&authority, || {
            let error = writer_file_options().open(&path).unwrap_err();
            assert!(is_windows_sharing_violation(&error));
            assert!(fs::rename(&path, dir.join("payload_replacement.payload")).is_err());
            Ok(())
        })
        .unwrap();

        assert!(!path.exists());
    }

    #[test]
    fn only_windows_sharing_and_lock_violations_use_retryable_mapping() {
        assert!(is_windows_sharing_violation(
            &std::io::Error::from_raw_os_error(ERROR_SHARING_VIOLATION)
        ));
        assert!(is_windows_sharing_violation(
            &std::io::Error::from_raw_os_error(ERROR_LOCK_VIOLATION)
        ));
        assert!(!is_windows_sharing_violation(
            &std::io::Error::from_raw_os_error(5)
        ));
        assert!(!is_windows_sharing_violation(
            &std::io::Error::from_raw_os_error(50)
        ));
    }

    #[test]
    fn ordinary_payload_lifecycle_creates_reads_and_deletes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir(&root).unwrap();
        let dir = prepare_payload_dir(&root).unwrap();
        let payload_ref = "payload_lifecycle.payload";
        let path = dir.join(payload_ref);

        let write = write_private_file(&path, b"lifecycle").unwrap();
        assert!(write.created);
        assert_eq!(
            read_payload_file_for_verify(&path).unwrap().unwrap().0,
            b"lifecycle"
        );
        assert!(remove_verified_payload_file(&write.authority).unwrap());
        assert!(!path.exists());
    }
}

#[cfg(test)]
mod authority_tests {
    use super::*;

    fn expectation(content: &str) -> (String, u64, u64) {
        (
            super::super::util::sha256_hex(content.as_bytes()),
            content.len() as u64,
            content.chars().count() as u64,
        )
    }

    #[test]
    fn verified_authority_rejects_same_identity_mutation_between_calls() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("payload.payload");
        fs::write(&path, b"original").unwrap();
        let (_, authority) = read_payload_file_for_verify(&path).unwrap().unwrap();

        fs::write(&path, b"mutated!").unwrap();
        let (_, mutated) = read_payload_file_for_verify(&path).unwrap().unwrap();
        assert_eq!(mutated.identity, authority.identity);
        assert_eq!(
            remove_verified_payload_file(&authority),
            Err(LcmError::PayloadIntegrityMismatch)
        );
        assert_eq!(fs::read(&path).unwrap(), b"mutated!");
    }

    #[test]
    fn expected_hash_byte_and_char_sizes_must_all_match() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("payload.payload");
        let content = "héllo 雪";
        fs::write(&path, content).unwrap();
        let (hash, bytes, chars) = expectation(content);

        assert!(matches!(
            verify_payload_file_authority(&path, "wrong", bytes, chars),
            Err(LcmError::PayloadIntegrityMismatch)
        ));
        assert!(matches!(
            verify_payload_file_authority(&path, &hash, bytes + 1, chars),
            Err(LcmError::PayloadIntegrityMismatch)
        ));
        assert!(matches!(
            verify_payload_file_authority(&path, &hash, bytes, chars + 1),
            Err(LcmError::PayloadIntegrityMismatch)
        ));
        assert!(path.exists());
    }

    #[test]
    fn oversized_payload_is_rejected_before_allocation_or_read() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("oversized.payload");
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_VERIFIED_PAYLOAD_FILE_BYTES + 1).unwrap();

        assert!(matches!(
            read_payload_file_for_verify(&path),
            Err(LcmError::PayloadIntegrityMismatch)
        ));
    }

    #[test]
    fn verified_authority_rejects_replacement_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("payload.payload");
        fs::write(&path, b"original").unwrap();
        let (_, authority) = read_payload_file_for_verify(&path).unwrap().unwrap();
        #[cfg(unix)]
        let _original_guard = fs::File::open(&path).unwrap();

        fs::remove_file(&path).unwrap();
        fs::write(&path, b"original").unwrap();
        assert_eq!(
            remove_verified_payload_file(&authority),
            Err(LcmError::InvalidPayloadRef)
        );
        assert!(path.exists());
    }

    #[test]
    fn verified_authority_can_retry_after_original_content_is_restored() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("payload.payload");
        fs::write(&path, b"original").unwrap();
        let (_, authority) = read_payload_file_for_verify(&path).unwrap().unwrap();

        fs::write(&path, b"mutated!").unwrap();
        assert_eq!(
            remove_verified_payload_file(&authority),
            Err(LcmError::PayloadIntegrityMismatch)
        );
        fs::write(&path, b"original").unwrap();
        assert!(remove_verified_payload_file(&authority).unwrap());
        assert!(!path.exists());
    }

    #[test]
    fn unicode_authority_is_non_sensitive_and_deletes_verified_content() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("秘密-payload.payload");
        let content = "héllo 雪";
        fs::write(&path, content).unwrap();
        let (hash, bytes, chars) = expectation(content);

        let authority = verify_payload_file_authority(&path, &hash, bytes, chars)
            .unwrap()
            .unwrap();
        let debug = format!("{authority:?}");
        assert!(!debug.contains(content));
        assert!(!debug.contains("秘密-payload.payload"));
        assert!(debug.contains(&hash));
        assert!(remove_verified_payload_file(&authority).unwrap());
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn authority_creation_rejects_symlink_and_directory_payloads() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let outside = temp.path().join("outside");
        fs::write(&outside, b"outside").unwrap();
        let link = temp.path().join("link.payload");
        symlink(&outside, &link).unwrap();
        assert!(matches!(
            read_payload_file_for_verify(&link),
            Err(LcmError::InvalidPayloadRef)
        ));
        assert_eq!(fs::read(&outside).unwrap(), b"outside");

        let directory = temp.path().join("directory.payload");
        fs::create_dir(&directory).unwrap();
        assert!(matches!(
            read_payload_file_for_verify(&directory),
            Err(LcmError::InvalidPayloadRef)
        ));
        assert!(directory.is_dir());
    }
}
