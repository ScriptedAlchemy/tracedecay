//! A read-pass lease for one immutable payload and its existing directory route.
//!
//! Each child is opened relative to the retained parent capability. Sharing
//! participates in the Windows data-access checks: directories allow sibling
//! writes but deny deletion, and the payload denies both writes and deletion.
//! A directory already marked as a reparse point is refused. Once its child is
//! pinned, it cannot be emptied to install a new reparse point.

use std::ffi::OsStr;
use std::fs::{File, Metadata};
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Component, Path, Prefix};
use std::ptr::null;

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_OPEN, FILE_OPEN_FOR_BACKUP_INTENT, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
    NtCreateFile,
};
use windows_sys::Win32::Foundation::{
    INVALID_HANDLE_VALUE, OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError, UNICODE_STRING,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_READ_ATTRIBUTES, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FILE_TRAVERSE, OPEN_EXISTING, SYNCHRONIZE,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

use super::{PathKind, open_encoded_handle, rejected, validate_file_kind};
use crate::windows_file::{FileInformation, information};

// FILE_TRAVERSE participates in sharing while preserving routes whose ACL does
// not permit listing. The owner only inspects held metadata and opens children.
const ANCESTOR_READ_ACCESS: u32 = FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE;

/// Owns one read-only payload and the non-reparse directory route used to open it.
///
/// The payload field closes before the ancestor capabilities. The borrowed file
/// must remain inside this owner: a caller must not clone it into a longer-lived
/// reader or publish/rename this payload until the complete lease is dropped.
/// Attribute-only changes are not prevented by Windows sharing. Consumers must
/// keep checking held metadata against their original admission baseline.
pub struct ImmutableReadFile {
    file: File,
    admitted_metadata: Metadata,
    identity: FileInformation,
    ancestors: Vec<Ancestor>,
}

struct Ancestor {
    directory: File,
    identity: FileInformation,
}

impl ImmutableReadFile {
    pub fn file(&self) -> &File {
        &self.file
    }

    pub fn admitted_metadata(&self) -> &Metadata {
        &self.admitted_metadata
    }

    pub fn identity(&self) -> FileInformation {
        self.identity
    }

    /// Heap retained by this owner, excluding its inline size and OS handles.
    pub fn retained_heap_bytes(&self) -> usize {
        self.ancestors.capacity() * size_of::<Ancestor>()
    }
}

/// Allocation-free admission bound for an exact absolute disk or UNC filename.
///
/// Includes the inline owner, retained ancestor slots, borrowed traversal slots,
/// one reused UTF-16 buffer for the root and each literal child name. It excludes
/// the caller's already-owned input path and OS objects.
/// No directory enumeration or file open occurs here.
pub fn immutable_read_workspace_bytes(path: &Path) -> io::Result<usize> {
    let shape = InputShape::inspect(path)?;
    checked_sum(&[
        size_of::<ImmutableReadFile>(),
        checked_product(shape.ancestor_count, size_of::<Ancestor>())?,
        checked_product(shape.ancestor_count, size_of::<&Path>())?,
        checked_product(shape.encoded_units, size_of::<u16>())?,
    ])
}

/// Opens one immutable payload through a retained no-follow directory chain.
///
/// Only this specialized lease changes sharing. General private-fs openers keep
/// their existing policy. Each counter advances immediately before an attempted
/// native open, including a failing open. A successful call makes one payload
/// open and one open for each ancestor; consumer-specific cold named-path proof
/// remains the consumer's separately counted operation.
pub fn open_immutable_read_file(
    path: &Path,
    leaf_attempts: &mut u64,
    ancestor_attempts: &mut u64,
) -> io::Result<ImmutableReadFile> {
    let shape = InputShape::inspect(path)?;
    let mut ancestor_paths = Vec::with_capacity(shape.ancestor_count);
    let mut parent = path.parent();
    while let Some(ancestor) = parent {
        if !ancestor.as_os_str().is_empty() {
            ancestor_paths.push(ancestor);
        }
        parent = ancestor.parent();
    }
    ancestor_paths.reverse();

    // These owners are created before the payload, so error unwinding also
    // closes the payload before releasing any directory route authority.
    let mut ancestors = Vec::<Ancestor>::with_capacity(shape.ancestor_count);
    let mut encoded = Vec::with_capacity(shape.encoded_units);
    let root = ancestor_paths
        .first()
        .copied()
        .ok_or_else(invalid_absolute_path)?;
    encode_absolute(root, shape.spelling, &mut encoded);
    count_attempt(ancestor_attempts)?;
    let root_file = open_encoded_handle(
        root,
        &encoded,
        OPEN_EXISTING,
        ANCESTOR_READ_ACCESS,
        null(),
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        0,
    )?;
    validate_file_kind(&root_file, root, PathKind::Directory)?;
    let root_identity = information(&root_file)?;
    ancestors.push(Ancestor {
        directory: root_file,
        identity: root_identity,
    });

    for ancestor_path in &ancestor_paths[1..] {
        let name = ancestor_path
            .file_name()
            .ok_or_else(invalid_absolute_path)?;
        let parent = ancestors.last().ok_or_else(invalid_absolute_path)?;
        let file = open_relative_read(
            &parent.directory,
            name,
            PathKind::Directory,
            &mut encoded,
            ancestor_attempts,
        )?;
        validate_file_kind(&file, ancestor_path, PathKind::Directory)?;
        let identity = information(&file)?;
        ancestors.push(Ancestor {
            directory: file,
            identity,
        });
    }

    let parent = ancestors.last().ok_or_else(invalid_absolute_path)?;
    let name = path.file_name().ok_or_else(invalid_absolute_path)?;
    let file = open_relative_read(
        &parent.directory,
        name,
        PathKind::File,
        &mut encoded,
        leaf_attempts,
    )?;
    validate_file_kind(&file, path, PathKind::File)?;
    let admitted_metadata = file.metadata()?;
    let identity = information(&file)?;

    // Recheck the original handles after the child chain is complete. Relative
    // opens bind each child to that actual parent even if its pathname is raced;
    // deny-delete sharing then preserves the binding for the complete pass.
    for (ancestor, ancestor_path) in ancestors.iter().zip(&ancestor_paths) {
        validate_file_kind(&ancestor.directory, ancestor_path, PathKind::Directory)?;
        require_same_identity(
            ancestor_path,
            ancestor.identity,
            information(&ancestor.directory)?,
        )?;
    }
    require_admitted_leaf(path, &file, &admitted_metadata, identity)?;

    Ok(ImmutableReadFile {
        file,
        admitted_metadata,
        identity,
        ancestors,
    })
}

/// Opens exactly one literal component under the actual retained parent handle.
/// Fixed read/no-follow/synchronous options are shared by every child admission.
fn open_relative_read(
    parent: &File,
    name: &OsStr,
    kind: PathKind,
    encoded: &mut Vec<u16>,
    attempts: &mut u64,
) -> io::Result<File> {
    encoded.clear();
    encoded.extend(name.encode_wide());
    let byte_length = checked_product(encoded.len(), size_of::<u16>())?;
    let length = u16::try_from(byte_length).map_err(|_| invalid_absolute_path())?;
    let object_name = UNICODE_STRING {
        Length: length,
        MaximumLength: length,
        Buffer: encoded.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle(),
        ObjectName: &object_name,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: null(),
        SecurityQualityOfService: null(),
    };
    let mut io_status = IO_STATUS_BLOCK::default();
    let (access, share) = match kind {
        PathKind::Directory => (ANCESTOR_READ_ACCESS, FILE_SHARE_READ | FILE_SHARE_WRITE),
        PathKind::File => (FILE_GENERIC_READ, FILE_SHARE_READ),
    };
    let mut handle = std::ptr::null_mut();
    count_attempt(attempts)?;
    // SAFETY: the parent is a retained directory; the validated name is one
    // nonempty component with no separators, navigation or NUL. Its UTF-16
    // buffer and both ABI structures remain live through the synchronous open.
    // Both access masks include SYNCHRONIZE, required by NONALERT. FILE_OPEN
    // never creates/truncates anything; OPEN_REPARSE_POINT never follows the leaf.
    let status = unsafe {
        NtCreateFile(
            &raw mut handle,
            access,
            &attributes,
            &raw mut io_status,
            null(),
            FILE_ATTRIBUTE_NORMAL,
            share,
            FILE_OPEN,
            FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_FOR_BACKUP_INTENT,
            null(),
            0,
        )
    };
    if status < 0 {
        // SAFETY: converting a returned NTSTATUS has no pointer/lifetime input.
        return Err(io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(status) } as i32,
        ));
    }
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::other(
            "immutable Windows read returned no file handle",
        ));
    }
    // SAFETY: successful NtCreateFile transferred this one owned handle.
    Ok(unsafe { File::from_raw_handle(handle) })
}

fn require_admitted_leaf(
    path: &Path,
    file: &File,
    admitted: &Metadata,
    identity: FileInformation,
) -> io::Result<()> {
    validate_file_kind(file, path, PathKind::File)?;
    let current = file.metadata()?;
    if current.file_attributes() != admitted.file_attributes()
        || current.len() != admitted.len()
        || current.creation_time() != admitted.creation_time()
        || current.last_write_time() != admitted.last_write_time()
    {
        return Err(rejected(
            path,
            "immutable read metadata changed during admission",
        ));
    }
    require_same_identity(path, identity, information(file)?)
}

fn require_same_identity(
    path: &Path,
    admitted: FileInformation,
    current: FileInformation,
) -> io::Result<()> {
    if current.volume_serial_number != admitted.volume_serial_number
        || current.file_index != admitted.file_index
    {
        return Err(rejected(
            path,
            "immutable read identity changed during admission",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum AbsoluteSpelling {
    Disk,
    Unc,
    Verbatim,
}

struct InputShape {
    ancestor_count: usize,
    encoded_units: usize,
    spelling: AbsoluteSpelling,
}

impl InputShape {
    fn inspect(path: &Path) -> io::Result<Self> {
        let mut components = path.components();
        let Some(Component::Prefix(prefix)) = components.next() else {
            return Err(invalid_absolute_path());
        };
        let (spelling, added_units) = match prefix.kind() {
            Prefix::Disk(_) => (AbsoluteSpelling::Disk, 4),
            Prefix::UNC(_, _) => (AbsoluteSpelling::Unc, 6),
            Prefix::VerbatimDisk(_) | Prefix::VerbatimUNC(_, _) => (AbsoluteSpelling::Verbatim, 0),
            _ => return Err(invalid_absolute_path()),
        };
        let bytes = path.as_os_str().as_encoded_bytes();
        let suffix = &bytes[prefix.as_os_str().as_encoded_bytes().len()..];
        if !path.is_absolute()
            || bytes.contains(&0)
            || bytes.contains(&b'/')
            || !suffix.starts_with(b"\\")
            || suffix[1..]
                .split(|byte| *byte == b'\\')
                .any(|name| ambiguous_component(name, spelling))
        {
            return Err(invalid_absolute_path());
        }
        if !matches!(components.next(), Some(Component::RootDir)) {
            return Err(invalid_absolute_path());
        }
        for component in components {
            let Component::Normal(name) = component else {
                return Err(invalid_absolute_path());
            };
            // UNICODE_STRING counts bytes in a u16. Refuse an oversized name
            // before opening any of its ancestors, without allocating a buffer.
            if name.encode_wide().count() > usize::from(u16::MAX) / size_of::<u16>() {
                return Err(invalid_absolute_path());
            }
        }
        let mut ancestor_count = 0usize;
        let mut parent = path.parent();
        while let Some(ancestor) = parent {
            if !ancestor.as_os_str().is_empty() {
                ancestor_count = ancestor_count.checked_add(1).ok_or_else(size_overflow)?;
            }
            parent = ancestor.parent();
        }
        let encoded_units = checked_sum(&[path.as_os_str().encode_wide().count(), added_units, 1])?;
        Ok(Self {
            ancestor_count,
            encoded_units,
            spelling,
        })
    }
}

fn ambiguous_component(name: &[u8], spelling: AbsoluteSpelling) -> bool {
    if name.is_empty()
        || matches!(name, b"." | b"..")
        || name.contains(&b':')
        || (!matches!(spelling, AbsoluteSpelling::Verbatim)
            && name.last().is_some_and(|last| matches!(last, b'.' | b' ')))
    {
        return true;
    }
    if matches!(spelling, AbsoluteSpelling::Verbatim) {
        return false;
    }
    // Plain Win32 names must survive conversion to an exact NT component. The
    // verbatim form already denotes literal names, including reserved stems.
    let stem = name.split(|byte| *byte == b'.').next().unwrap_or_default();
    let Ok(stem) = std::str::from_utf8(stem) else {
        return false;
    };
    let stem = stem.trim_end_matches(' ');
    ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"]
        .iter()
        .any(|reserved| stem.eq_ignore_ascii_case(reserved))
        || ["COM", "LPT"].iter().any(|prefix| {
            stem.get(..3)
                .is_some_and(|value| value.eq_ignore_ascii_case(prefix))
                && matches!(
                    &stem[3..],
                    "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
        })
}

fn encode_absolute(path: &Path, spelling: AbsoluteSpelling, encoded: &mut Vec<u16>) {
    encoded.clear();
    match spelling {
        AbsoluteSpelling::Disk => encoded.extend("\\\\?\\".encode_utf16()),
        AbsoluteSpelling::Unc => encoded.extend("\\\\?\\UNC\\".encode_utf16()),
        AbsoluteSpelling::Verbatim => {}
    }
    let skip = if matches!(spelling, AbsoluteSpelling::Unc) {
        2
    } else {
        0
    };
    encoded.extend(path.as_os_str().encode_wide().skip(skip));
    encoded.push(0);
}

fn count_attempt(counter: &mut u64) -> io::Result<()> {
    *counter = counter
        .checked_add(1)
        .ok_or_else(|| io::Error::other("immutable Windows read open-attempt counter overflow"))?;
    Ok(())
}

fn checked_sum(values: &[usize]) -> io::Result<usize> {
    values
        .iter()
        .try_fold(0usize, |sum, value| sum.checked_add(*value))
        .ok_or_else(size_overflow)
}

fn checked_product(left: usize, right: usize) -> io::Result<usize> {
    left.checked_mul(right).ok_or_else(size_overflow)
}

fn size_overflow() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "immutable Windows read size overflow",
    )
}

fn invalid_absolute_path() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "immutable Windows read requires an exact absolute disk or UNC filename",
    )
}

#[cfg(test)]
mod tests {
    use std::fs::{FileTimes, OpenOptions as StdOpenOptions};
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::windows::fs::{FileTimesExt, OpenOptionsExt as StdOpenOptionsExt};
    use std::os::windows::io::AsRawHandle;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION, ERROR_SUCCESS,
    };
    use windows_sys::Win32::Security::Authorization::{SE_FILE_OBJECT, SetSecurityInfo};
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, DACL_SECURITY_INFORMATION, GetAce, NO_INHERITANCE,
        PROTECTED_DACL_SECURITY_INFORMATION,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_SHARE_DELETE, FILE_WRITE_ATTRIBUTES, READ_CONTROL, WRITE_DAC,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{FSCTL_GET_REPARSE_POINT, FSCTL_SET_REPARSE_POINT};

    use super::*;
    use crate::windows::{LocalAllocation, SECURE_OPEN_FLAGS, current_user_sid, private_acl};

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("route").join("parent");
        std::fs::create_dir_all(&parent).unwrap();
        let path = parent.join("payload");
        std::fs::write(&path, b"immutable payload").unwrap();
        let path = std::fs::canonicalize(path).unwrap();
        (temp, path)
    }

    fn lease(path: &Path) -> ImmutableReadFile {
        let mut leaf_attempts = 0;
        let mut ancestor_attempts = 0;
        let opened =
            open_immutable_read_file(path, &mut leaf_attempts, &mut ancestor_attempts).unwrap();
        assert_eq!(leaf_attempts, 1);
        assert_eq!(
            ancestor_attempts as usize,
            InputShape::inspect(path).unwrap().ancestor_count
        );
        assert!(opened.retained_heap_bytes() > 0);
        assert!(
            size_of::<ImmutableReadFile>() + opened.retained_heap_bytes()
                <= immutable_read_workspace_bytes(path).unwrap()
        );
        opened
    }

    #[test]
    fn immutable_read_holds_payload_and_ancestors_but_allows_sibling_publication() {
        let (_temp, path) = fixture();
        let parent = path.parent().unwrap();
        let parent_renamed = parent.with_file_name("parent-renamed");
        let route = parent.parent().unwrap();
        let route_renamed = route.with_file_name("route-renamed");
        let payload_renamed = parent.join("payload-renamed");
        let opened = lease(&path);

        assert_eq!(std::fs::read(&path).unwrap(), b"immutable payload");
        assert!(StdOpenOptions::new().write(true).open(&path).is_err());
        assert!(std::fs::write(&path, b"replacement").is_err());
        assert!(std::fs::remove_file(&path).is_err());
        assert!(std::fs::rename(&path, &payload_renamed).is_err());
        assert!(std::fs::rename(parent, &parent_renamed).is_err());
        assert!(std::fs::rename(route, &route_renamed).is_err());

        let sibling = parent.join("sibling");
        let sibling_renamed = parent.join("sibling-published");
        std::fs::write(&sibling, b"sibling publication").unwrap();
        std::fs::rename(&sibling, &sibling_renamed).unwrap();
        assert_eq!(
            std::fs::read(&sibling_renamed).unwrap(),
            b"sibling publication"
        );
        std::fs::remove_file(&sibling_renamed).unwrap();

        let mut reader = opened.file();
        for _ in 0..8 {
            reader.seek(SeekFrom::Start(0)).unwrap();
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, b"immutable payload");
            require_admitted_leaf(
                &path,
                opened.file(),
                opened.admitted_metadata(),
                opened.identity(),
            )
            .unwrap();
        }
        drop(opened);

        std::fs::rename(&path, &payload_renamed).unwrap();
        std::fs::write(&payload_renamed, b"replacement after lease").unwrap();
        std::fs::remove_file(&payload_renamed).unwrap();
        std::fs::rename(parent, &parent_renamed).unwrap();
        std::fs::rename(route, &route_renamed).unwrap();
    }

    struct RestoreDirectoryAcl {
        directory: File,
        original_acl: LocalAllocation,
        pending: bool,
    }

    impl RestoreDirectoryAcl {
        fn restrict_to_traverse(path: &Path) -> io::Result<Self> {
            // The fixture starts with the canonical protected private DACL.
            // This metadata-only handle remains authorized to restore it even
            // after restriction and does not conflict with the read lease.
            crate::validate_private_directory(path)?;
            let directory = StdOpenOptions::new()
                .access_mode(READ_CONTROL | WRITE_DAC)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                .custom_flags(SECURE_OPEN_FLAGS)
                .open(path)?;
            let user = current_user_sid()?;
            let original_acl = private_acl(&user, PathKind::Directory.inheritance())?;
            let restricted_acl = private_acl(&user, NO_INHERITANCE)?;
            let mut ace = std::ptr::null_mut();
            // SAFETY: private_acl produced exactly one current-user allow ACE;
            // its LocalAlloc storage stays live until SetSecurityInfo returns.
            if unsafe { GetAce(restricted_acl.0.cast(), 0, &raw mut ace) } == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: the checked first ACE has ACCESS_ALLOWED_ACE layout.
            unsafe { (*ace.cast::<ACCESS_ALLOWED_ACE>()).Mask = ANCESTOR_READ_ACCESS };
            let restore = Self {
                directory,
                original_acl,
                pending: true,
            };
            restore.set_acl(&restricted_acl)?;
            Ok(restore)
        }

        fn set_acl(&self, acl: &LocalAllocation) -> io::Result<()> {
            // SAFETY: the retained WRITE_DAC handle and canonical ACL allocation
            // remain valid through this synchronous descriptor update.
            let status = unsafe {
                SetSecurityInfo(
                    self.directory.as_raw_handle(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    acl.0.cast(),
                    std::ptr::null(),
                )
            };
            if status == ERROR_SUCCESS {
                Ok(())
            } else {
                Err(io::Error::from_raw_os_error(status as i32))
            }
        }

        fn restore(&mut self) -> io::Result<()> {
            self.set_acl(&self.original_acl)?;
            self.pending = false;
            Ok(())
        }
    }

    impl Drop for RestoreDirectoryAcl {
        fn drop(&mut self) {
            if self.pending {
                let _ = self.restore();
            }
        }
    }

    #[test]
    fn immutable_read_accepts_traverse_only_ancestry_without_list_access() {
        let temp = tempfile::tempdir().unwrap();
        let parent = std::fs::canonicalize(temp.path())
            .unwrap()
            .join("traverse-only");
        crate::create_private_directory(&parent).unwrap();
        let path = parent.join("payload");
        let mut payload = crate::create_private_file(&path).unwrap();
        payload.write_all(b"traverse-only payload").unwrap();
        drop(payload);
        let mut restore = RestoreDirectoryAcl::restrict_to_traverse(&parent).unwrap();

        let error = StdOpenOptions::new()
            .access_mode(FILE_GENERIC_READ)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(SECURE_OPEN_FLAGS)
            .open(&parent)
            .err()
            .expect("the ancestor DACL must refuse generic read/list access");
        assert_eq!(error.raw_os_error(), Some(ERROR_ACCESS_DENIED as i32));
        assert_eq!(
            std::fs::read_dir(&parent).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );

        let opened = lease(&path);
        let mut bytes = Vec::new();
        opened.file().read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"traverse-only payload");
        drop(opened);
        restore.restore().unwrap();
        crate::validate_private_directory(&parent).unwrap();
        assert_eq!(std::fs::read_dir(&parent).unwrap().count(), 1);
    }

    #[test]
    fn immutable_read_refuses_preexisting_writer_and_releases_partial_ancestors() {
        let (_temp, path) = fixture();
        let parent = path.parent().unwrap();
        let parent_renamed = parent.with_file_name("retry-parent");
        let writer = StdOpenOptions::new().write(true).open(&path).unwrap();
        let mut leaves = 0;
        let mut ancestors = 0;
        let error = open_immutable_read_file(&path, &mut leaves, &mut ancestors)
            .err()
            .expect("a preexisting data writer must prevent immutable admission");
        assert_eq!(error.raw_os_error(), Some(ERROR_SHARING_VIOLATION as i32));
        assert_eq!(leaves, 1);
        assert_eq!(
            ancestors as usize,
            InputShape::inspect(&path).unwrap().ancestor_count
        );
        drop(writer);

        // This succeeds only if the failed admission released every ancestor.
        std::fs::rename(parent, &parent_renamed).unwrap();
        let retry_path = parent_renamed.join("payload");
        let opened = lease(&retry_path);
        assert_eq!(
            opened.file().metadata().unwrap().len(),
            b"immutable payload".len() as u64
        );
        drop(opened);
        std::fs::remove_file(retry_path).unwrap();
    }

    #[test]
    fn immutable_read_counts_missing_leaf_and_releases_partial_route() {
        let (_temp, path) = fixture();
        let missing = path.with_file_name("missing");
        let mut leaves = 0;
        let mut ancestors = 0;
        let error = open_immutable_read_file(&missing, &mut leaves, &mut ancestors)
            .err()
            .expect("missing payload must be refused");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(leaves, 1);
        assert_eq!(
            ancestors as usize,
            InputShape::inspect(&missing).unwrap().ancestor_count
        );
        let parent = path.parent().unwrap();
        std::fs::rename(parent, parent.with_file_name("released")).unwrap();
    }

    #[test]
    fn immutable_read_preserves_original_metadata_and_native_identity() {
        let (_temp, path) = fixture();
        let opened = lease(&path);
        let admitted_write = opened.admitted_metadata().modified().unwrap();
        let attributes = StdOpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(SECURE_OPEN_FLAGS)
            .open(&path)
            .unwrap();
        attributes
            .set_times(FileTimes::new().set_modified(admitted_write + Duration::from_secs(2)))
            .unwrap();
        let error = require_admitted_leaf(
            &path,
            opened.file(),
            opened.admitted_metadata(),
            opened.identity(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("metadata changed"));
        assert_eq!(
            opened.admitted_metadata().modified().unwrap(),
            admitted_write
        );

        let foreign_path = path.with_file_name("foreign-payload");
        std::fs::write(&foreign_path, b"immutable payload").unwrap();
        let foreign = StdOpenOptions::new()
            .read(true)
            .write(true)
            .open(&foreign_path)
            .unwrap();
        foreign
            .set_times(
                FileTimes::new()
                    .set_created(opened.admitted_metadata().created().unwrap())
                    .set_modified(admitted_write),
            )
            .unwrap();
        let foreign_metadata = foreign.metadata().unwrap();
        assert_eq!(foreign_metadata.len(), opened.admitted_metadata().len());
        assert_eq!(
            foreign_metadata.creation_time(),
            opened.admitted_metadata().creation_time()
        );
        assert_eq!(
            foreign_metadata.last_write_time(),
            opened.admitted_metadata().last_write_time()
        );
        assert_eq!(
            foreign_metadata.file_attributes(),
            opened.admitted_metadata().file_attributes()
        );
        let error = require_admitted_leaf(
            &foreign_path,
            &foreign,
            opened.admitted_metadata(),
            opened.identity(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("identity changed"));
    }

    fn junction(path: &Path, target: &Path) {
        let output = Command::new("cmd")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(path)
            .arg(target)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "failed to create test junction: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn reparse_data(path: &Path) -> Vec<u8> {
        let file = StdOpenOptions::new()
            .read(true)
            .custom_flags(SECURE_OPEN_FLAGS)
            .open(path)
            .unwrap();
        // Windows' maximum reparse buffer is 16 KiB; this fixture reads a real
        // junction's native buffer rather than constructing a synthetic tag.
        let mut data = vec![0u8; 16 * 1024];
        let mut returned = 0;
        // SAFETY: the held junction and initialized output buffer remain live;
        // the synchronous call writes at most the supplied buffer length.
        let ok = unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                FSCTL_GET_REPARSE_POINT,
                std::ptr::null(),
                0,
                data.as_mut_ptr().cast(),
                data.len() as u32,
                &raw mut returned,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(ok, 0, "read junction data: {}", io::Error::last_os_error());
        data.truncate(returned as usize);
        data
    }

    fn set_reparse_data(path: &Path, data: &[u8]) -> io::Result<()> {
        let file = StdOpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(SECURE_OPEN_FLAGS)
            .open(path)?;
        let mut returned = 0;
        // SAFETY: the input is the exact native buffer returned for the fixture
        // junction, and both buffer and directory handle remain live.
        let ok = unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                FSCTL_SET_REPARSE_POINT,
                data.as_ptr().cast(),
                data.len() as u32,
                std::ptr::null_mut(),
                0,
                &raw mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    #[test]
    fn immutable_read_refuses_reparse_ancestors_and_blocks_held_route_retargeting() {
        let (temp, path) = fixture();
        let foreign = temp.path().join("foreign");
        std::fs::create_dir(&foreign).unwrap();
        std::fs::write(foreign.join("payload"), b"foreign payload").unwrap();
        let redirect = temp.path().join("redirect");
        junction(&redirect, &foreign);
        let data = reparse_data(&redirect);
        let through_redirect = std::fs::canonicalize(temp.path())
            .unwrap()
            .join("redirect")
            .join("payload");
        let mut leaves = 0;
        let mut ancestors = 0;
        let error = open_immutable_read_file(&through_redirect, &mut leaves, &mut ancestors)
            .err()
            .expect("an existing junction ancestor must not be followed");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("reparse points are not allowed"));
        assert_eq!(leaves, 0);
        assert_eq!(
            ancestors as usize,
            InputShape::inspect(&through_redirect)
                .unwrap()
                .ancestor_count
        );

        let parent = path.parent().unwrap();
        let opened = lease(&path);
        assert!(set_reparse_data(parent, &data).is_err());
        assert!(std::fs::remove_file(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"immutable payload");
        require_admitted_leaf(
            &path,
            opened.file(),
            opened.admitted_metadata(),
            opened.identity(),
        )
        .unwrap();
        drop(opened);

        // The same real FSCTL succeeds after the lease is gone and the now
        // removable child is removed. Refusal above is not an invalid fixture.
        std::fs::remove_file(&path).unwrap();
        set_reparse_data(parent, &data).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"foreign payload");
        std::fs::remove_dir(parent).unwrap();
        std::fs::remove_dir(&redirect).unwrap();
        assert_eq!(
            std::fs::read(foreign.join("payload")).unwrap(),
            b"foreign payload"
        );
    }

    fn assert_preflight_refusal(path: &Path) {
        assert_eq!(
            immutable_read_workspace_bytes(path).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        let mut leaves = 0;
        let mut ancestors = 0;
        let error = open_immutable_read_file(path, &mut leaves, &mut ancestors)
            .err()
            .expect("invalid path must not open a handle");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!((leaves, ancestors), (0, 0));
    }

    #[test]
    fn immutable_read_rejects_ambiguous_or_oversized_names_before_any_open() {
        for path in [
            r"relative\payload",
            r"C:payload",
            r"\payload",
            r"C:\",
            r"C:\route\.\payload",
            r"C:\route\..\payload",
            r"C:\route\\payload",
            r"C:/route/payload",
            r"C:\route\payload\",
            r"C:\route\payload.",
            r"C:\route\payload ",
            r"C:\route\payload:stream",
            r"C:\route\NUL",
            r"C:\route\com¹.txt",
            r"\\.\pipe\payload",
            "C:\\route\\nul\0payload",
            r"\\?\C:\route\.\payload",
            r"\\?\C:\route\..\payload",
        ] {
            assert_preflight_refusal(Path::new(path));
        }
        let too_many_units = format!("C:\\route\\{}", "x".repeat(usize::from(u16::MAX) / 2 + 1));
        assert_preflight_refusal(Path::new(&too_many_units));
        for path in [
            r"C:\route\payload",
            r"\\?\C:\route\payload",
            r"\\server\share\route\payload",
            r"\\?\UNC\server\share\route\payload",
            r"\\?\C:\route.\payload",
            r"\\?\C:\route \payload",
            r"\\?\C:\NUL\payload",
            r"\\?\C:\COM1\payload",
        ] {
            assert!(immutable_read_workspace_bytes(Path::new(path)).unwrap() > 0);
        }
    }

    #[test]
    fn immutable_read_preserves_literal_verbatim_ancestors_and_long_paths() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        for name in [
            "literal.",
            "literal ",
            "NUL",
            "COM1",
            "LPT1.txt",
            "COM¹",
            "NUL\u{2003}",
        ] {
            let parent = root.join(name);
            crate::create_private_directory(&parent).unwrap();
            let path = parent.join("payload");
            std::fs::write(&path, b"literal ancestor").unwrap();
            let opened = lease(&path);
            let mut reader = opened.file();
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, b"literal ancestor");
            drop(opened);
            std::fs::remove_file(&path).unwrap();
            std::fs::remove_dir(parent).unwrap();
        }

        let mut parent = root;
        while parent.as_os_str().encode_wide().count() < 280 {
            parent.push("long-canonical-component");
        }
        std::fs::create_dir_all(&parent).unwrap();
        let path = parent.join("payload");
        std::fs::write(&path, b"long path").unwrap();
        let opened = lease(&path);
        assert_eq!(opened.file().metadata().unwrap().len(), 9);
        drop(opened);
        std::fs::remove_file(path).unwrap();
    }
}
