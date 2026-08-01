use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::mem::{MaybeUninit, size_of};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr::{addr_of, addr_of_mut, null, null_mut};

use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, GetSecurityInfo, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, SET_ACCESS,
    SetEntriesInAclW, SetSecurityInfo, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation, CopySid,
    DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetAclInformation, GetLengthSid,
    GetSecurityDescriptorControl, GetTokenInformation, InitializeSecurityDescriptor, IsValidAcl,
    IsValidSecurityDescriptor, IsValidSid, NO_INHERITANCE, OWNER_SECURITY_INFORMATION,
    PROTECTED_DACL_SECURITY_INFORMATION, PSID, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES,
    SECURITY_DESCRIPTOR, SUB_CONTAINERS_AND_OBJECTS_INHERIT, SetSecurityDescriptorControl,
    SetSecurityDescriptorDacl, TOKEN_INFORMATION_CLASS, TOKEN_OWNER, TOKEN_QUERY, TOKEN_USER,
    TokenOwner, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ALL_ACCESS, FILE_ATTRIBUTE_DEVICE,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ,
    FILE_GENERIC_WRITE, FILE_ID_INFO, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_WRITE_DATA, FileAttributeTagInfo, FileIdInfo, GetFileInformationByHandleEx, OPEN_ALWAYS,
    OPEN_EXISTING, READ_CONTROL, WRITE_DAC,
};
use windows_sys::Win32::System::SystemServices::{
    ACCESS_ALLOWED_ACE_TYPE, SECURITY_DESCRIPTOR_REVISION,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

const SECURITY_ACCESS: u32 = READ_CONTROL | WRITE_DAC | FILE_READ_ATTRIBUTES;
const SHARE_READ_WRITE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE;
const SECURE_OPEN_FLAGS: u32 = FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT;

#[derive(Clone, Copy, Debug)]
enum PathKind {
    Directory,
    File,
}

impl PathKind {
    const fn inheritance(self) -> u32 {
        match self {
            Self::Directory => SUB_CONTAINERS_AND_OBJECTS_INHERIT,
            Self::File => NO_INHERITANCE,
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::Directory => "directory",
            Self::File => "regular file",
        }
    }
}

#[derive(Debug)]
struct SecuritySnapshot {
    owner_is_token_owner: bool,
    dacl_is_protected: bool,
    ace_count: u32,
    ace_is_allowed: bool,
    ace_mask: u32,
    ace_inheritance: u8,
    trustee_is_current_user: bool,
}

#[derive(PartialEq, Eq)]
struct FileIdentity {
    volume_serial_number: u64,
    file_id: [u8; 16],
}

struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: Windows allocated this pointer for the caller with LocalAlloc.
            let _ = unsafe { LocalFree(self.0) };
        }
    }
}

struct OwnedSid {
    storage: Vec<usize>,
}

struct ProcessSids {
    user: OwnedSid,
    owner: OwnedSid,
}

impl OwnedSid {
    fn as_psid(&self) -> PSID {
        self.storage.as_ptr().cast_mut().cast()
    }
}

/// Create every missing directory with a protected, inheritable current-user DACL.
pub fn create_private_dir_all(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => return restrict_directory(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(wrap_error("inspect private directory", path, error)),
    }

    let mut missing = Vec::<PathBuf>::new();
    let mut current = path;
    loop {
        match std::fs::symlink_metadata(current) {
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing.push(current.to_path_buf());
                current = current.parent().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "private directory path has no existing ancestor: '{}'",
                            path.display()
                        ),
                    )
                })?;
            }
            Err(error) => {
                return Err(wrap_error(
                    "inspect private directory ancestor",
                    current,
                    error,
                ));
            }
        }
    }

    for directory in missing.iter().rev() {
        create_private_directory(directory)?;
        restrict_directory(directory)?;
    }
    Ok(())
}

/// Replace a directory DACL with one protected, inheritable current-user ACE.
pub fn restrict_directory(path: &Path) -> io::Result<()> {
    drop(open_and_secure(
        path,
        PathKind::Directory,
        OPEN_EXISTING,
        SECURITY_ACCESS,
    )?);
    Ok(())
}

/// Replace a regular-file DACL with one protected current-user ACE.
pub fn restrict_file(path: &Path) -> io::Result<()> {
    drop(open_and_secure(
        path,
        PathKind::File,
        OPEN_EXISTING,
        SECURITY_ACCESS,
    )?);
    Ok(())
}

/// Open an existing regular file only after securing and validating its DACL.
pub fn open_private_file(path: &Path) -> io::Result<File> {
    let security_handle = open_and_secure(path, PathKind::File, OPEN_EXISTING, SECURITY_ACCESS)?;
    reopen_private_file(path, security_handle, FILE_GENERIC_READ)
}

/// Open or create a regular file, securing it before the caller can publish data.
pub fn open_or_create_private_file(path: &Path) -> io::Result<File> {
    let security_handle =
        open_with_private_creation_acl(path, PathKind::File, OPEN_ALWAYS, SECURITY_ACCESS)?;
    secure_handle(&security_handle, path, PathKind::File)?;
    reopen_private_file(
        path,
        security_handle,
        FILE_GENERIC_READ | FILE_GENERIC_WRITE,
    )
}

/// Create a new empty regular file and secure it before returning the handle.
pub fn create_private_file(path: &Path) -> io::Result<File> {
    let file = open_with_private_creation_acl(
        path,
        PathKind::File,
        CREATE_NEW,
        SECURITY_ACCESS | FILE_GENERIC_READ | FILE_GENERIC_WRITE,
    )?;
    if let Err(error) = secure_handle(&file, path, PathKind::File) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok(file)
}

fn open_and_secure(path: &Path, kind: PathKind, disposition: u32, access: u32) -> io::Result<File> {
    let file = open_handle(path, disposition, access, null())?;
    secure_handle(&file, path, kind)?;
    Ok(file)
}

fn open_with_private_creation_acl(
    path: &Path,
    kind: PathKind,
    disposition: u32,
    access: u32,
) -> io::Result<File> {
    with_private_security_attributes(path, kind, |attributes| {
        open_handle(path, disposition, access, attributes)
    })
}

fn create_private_directory(path: &Path) -> io::Result<()> {
    with_private_security_attributes(path, PathKind::Directory, |attributes| {
        let absolute = absolute_security_path(path)?;
        let _ancestors = hold_directory_ancestors(&absolute)?;
        let encoded = encode_path(&absolute)?;
        // SAFETY: `encoded` is NUL-terminated and the security attributes stay
        // valid for the complete creation call.
        if unsafe { CreateDirectoryW(encoded.as_ptr(), attributes) } != 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_ALREADY_EXISTS as i32) {
            return Ok(());
        }
        Err(wrap_error("create private Windows directory", path, error))
    })
}

fn with_private_security_attributes<T>(
    path: &Path,
    kind: PathKind,
    operation: impl FnOnce(*const SECURITY_ATTRIBUTES) -> io::Result<T>,
) -> io::Result<T> {
    let process_sids = current_process_sids()
        .map_err(|error| wrap_error("resolve current Windows token SIDs", path, error))?;
    let acl = private_acl(&process_sids.user, kind.inheritance())
        .map_err(|error| wrap_error("build private Windows creation DACL", path, error))?;
    let mut descriptor = SECURITY_DESCRIPTOR::default();
    // SAFETY: `descriptor` is writable storage for an absolute descriptor.
    if unsafe {
        InitializeSecurityDescriptor(
            addr_of_mut!(descriptor).cast(),
            SECURITY_DESCRIPTOR_REVISION,
        )
    } == 0
    {
        return Err(contextual_error(
            "initialize private Windows security descriptor",
            path,
        ));
    }
    // SAFETY: the descriptor is initialized and `acl` remains valid through creation.
    if unsafe { SetSecurityDescriptorDacl(addr_of_mut!(descriptor).cast(), 1, acl.0.cast(), 0) }
        == 0
    {
        return Err(contextual_error(
            "attach private Windows creation DACL",
            path,
        ));
    }
    // SAFETY: the descriptor is initialized and both control masks are valid.
    if unsafe {
        SetSecurityDescriptorControl(
            addr_of_mut!(descriptor).cast(),
            SE_DACL_PROTECTED,
            SE_DACL_PROTECTED,
        )
    } == 0
    {
        return Err(contextual_error(
            "protect private Windows creation DACL",
            path,
        ));
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: addr_of_mut!(descriptor).cast(),
        bInheritHandle: 0,
    };
    operation(&raw const attributes)
}

fn reopen_private_file(
    path: &Path,
    security_handle: File,
    additional_access: u32,
) -> io::Result<File> {
    let expected = file_identity(&security_handle, path)?;
    let share_mode = if additional_access & FILE_WRITE_DATA != 0 {
        drop(security_handle);
        SHARE_READ_WRITE
    } else {
        FILE_SHARE_READ
    };
    let file = open_handle_with_share(
        path,
        OPEN_EXISTING,
        SECURITY_ACCESS | additional_access,
        null(),
        share_mode,
    )?;
    validate_file_kind(&file, path, PathKind::File)?;
    let actual = file_identity(&file, path)?;
    if expected != actual {
        return Err(rejected(path, "file identity changed while securing it"));
    }
    let process_sids = current_process_sids()
        .map_err(|error| wrap_error("resolve current Windows token SIDs", path, error))?;
    validate_private_security(&file, path, PathKind::File, &process_sids)?;
    Ok(file)
}

fn file_identity(file: &File, path: &Path) -> io::Result<FileIdentity> {
    let mut information = MaybeUninit::<FILE_ID_INFO>::uninit();
    // SAFETY: the output pointer is valid for the exact structure size and the
    // file handle stays live for the duration of the call.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            information.as_mut_ptr().cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    } == 0
    {
        return Err(contextual_error("identify Windows file", path));
    }
    // SAFETY: a nonzero result initializes the complete output structure.
    let information = unsafe { information.assume_init() };
    Ok(FileIdentity {
        volume_serial_number: information.VolumeSerialNumber,
        file_id: information.FileId.Identifier,
    })
}

fn open_handle(
    path: &Path,
    disposition: u32,
    access: u32,
    security_attributes: *const SECURITY_ATTRIBUTES,
) -> io::Result<File> {
    open_handle_with_share(
        path,
        disposition,
        access,
        security_attributes,
        FILE_SHARE_READ,
    )
}

fn open_handle_with_share(
    path: &Path,
    disposition: u32,
    access: u32,
    security_attributes: *const SECURITY_ATTRIBUTES,
    share_mode: u32,
) -> io::Result<File> {
    let absolute = absolute_security_path(path)?;
    let _ancestors = hold_directory_ancestors(&absolute)?;
    open_raw_handle(
        &absolute,
        disposition,
        access,
        security_attributes,
        share_mode,
    )
}

fn open_raw_handle(
    path: &Path,
    disposition: u32,
    access: u32,
    security_attributes: *const SECURITY_ATTRIBUTES,
    share_mode: u32,
) -> io::Result<File> {
    let encoded = encode_path(path)?;

    // SAFETY: `encoded` is NUL-terminated, the optional security attributes
    // remain valid for the call, and a successful handle transfers into `File`.
    let handle = unsafe {
        CreateFileW(
            encoded.as_ptr(),
            access,
            share_mode,
            security_attributes,
            disposition,
            SECURE_OPEN_FLAGS,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(contextual_error("open for Windows security", path));
    }

    // SAFETY: `CreateFileW` returned one owned, valid handle.
    Ok(unsafe { File::from_raw_handle(handle) })
}

fn absolute_security_path(path: &Path) -> io::Result<PathBuf> {
    std::path::absolute(path)
        .map_err(|error| wrap_error("resolve absolute Windows security path", path, error))
}

fn hold_directory_ancestors(path: &Path) -> io::Result<Vec<File>> {
    let mut ancestor_paths = Vec::new();
    let mut current = path.parent();
    while let Some(ancestor) = current {
        if !ancestor.as_os_str().is_empty() {
            ancestor_paths.push(ancestor);
        }
        current = ancestor.parent();
    }
    ancestor_paths.reverse();

    let mut handles = Vec::with_capacity(ancestor_paths.len());
    for ancestor in ancestor_paths {
        let handle = open_raw_handle(
            ancestor,
            OPEN_EXISTING,
            FILE_READ_ATTRIBUTES,
            null(),
            FILE_SHARE_READ,
        )?;
        validate_file_kind(&handle, ancestor, PathKind::Directory)?;
        handles.push(handle);
    }
    Ok(handles)
}

fn encode_path(path: &Path) -> io::Result<Vec<u16>> {
    let encoded = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    if encoded[..encoded.len().saturating_sub(1)].contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Windows security path contains a NUL: '{}'", path.display()),
        ));
    }
    Ok(encoded)
}

fn secure_handle(file: &File, path: &Path, kind: PathKind) -> io::Result<()> {
    validate_file_kind(file, path, kind)?;
    let process_sids = current_process_sids()
        .map_err(|error| wrap_error("resolve current Windows token SIDs", path, error))?;
    validate_token_owner(file, path, &process_sids.owner)?;
    let acl = private_acl(&process_sids.user, kind.inheritance())
        .map_err(|error| wrap_error("build private Windows DACL", path, error))?;

    // SAFETY: `file` owns a live file-system handle, `acl` remains allocated
    // for the call, and null owner/group/SACL pointers match the information mask.
    let status = unsafe {
        SetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            acl.0.cast(),
            null(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(wrap_error(
            "set private Windows DACL",
            path,
            io::Error::from_raw_os_error(status as i32),
        ));
    }

    validate_private_security(file, path, kind, &process_sids)
}

fn validate_file_kind(file: &File, path: &Path, kind: PathKind) -> io::Result<()> {
    let mut information = MaybeUninit::<FILE_ATTRIBUTE_TAG_INFO>::uninit();
    // SAFETY: the output pointer is valid for the exact structure size and the
    // file handle stays live for the duration of the call.
    let succeeded = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileAttributeTagInfo,
            information.as_mut_ptr().cast(),
            size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    };
    if succeeded == 0 {
        return Err(contextual_error("inspect Windows file attributes", path));
    }
    // SAFETY: a nonzero result initializes the complete output structure.
    let information = unsafe { information.assume_init() };
    if information.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(rejected(path, "reparse points are not allowed"));
    }
    let is_directory = information.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    let is_device = information.FileAttributes & FILE_ATTRIBUTE_DEVICE != 0;
    let expected_kind = matches!(
        (kind, is_directory, is_device),
        (PathKind::Directory, true, false) | (PathKind::File, false, false)
    );
    if !expected_kind {
        return Err(rejected(
            path,
            &format!("expected a {}", kind.description()),
        ));
    }
    Ok(())
}

fn current_process_sids() -> io::Result<ProcessSids> {
    let mut token = null_mut();
    // SAFETY: the process pseudo-handle is always valid and `token` is writable.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if token.is_null() {
        return Err(io::Error::other(
            "OpenProcessToken returned a null token handle",
        ));
    }
    // SAFETY: `OpenProcessToken` returned one owned token handle.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };

    let token_user = token_information(&token, TokenUser, size_of::<TOKEN_USER>())?;
    let token_owner = token_information(&token, TokenOwner, size_of::<TOKEN_OWNER>())?;
    // SAFETY: `token_information` verified the returned structure sizes and
    // keeps the aligned buffers live while their SID pointers are copied.
    let user = unsafe { (*token_user.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    // SAFETY: see the preceding comment.
    let owner = unsafe { (*token_owner.as_ptr().cast::<TOKEN_OWNER>()).Owner };
    Ok(ProcessSids {
        user: copy_sid(user, "user")?,
        owner: copy_sid(owner, "owner")?,
    })
}

fn token_information(
    token: &OwnedHandle,
    information_class: TOKEN_INFORMATION_CLASS,
    minimum_size: usize,
) -> io::Result<Vec<usize>> {
    let mut required = 0_u32;
    // SAFETY: a null buffer with zero length is the documented sizing call.
    let sized = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            information_class,
            null_mut(),
            0,
            &raw mut required,
        )
    };
    if sized != 0 || required < minimum_size as u32 {
        return Err(io::Error::other(
            "GetTokenInformation returned an invalid token information size",
        ));
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) {
        return Err(error);
    }

    let word_count = (required as usize).div_ceil(size_of::<usize>());
    let mut information = vec![0_usize; word_count];
    let mut returned = required;
    // SAFETY: `information` is aligned storage of at least `required` bytes.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            information_class,
            information.as_mut_ptr().cast(),
            required,
            &raw mut returned,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if returned < minimum_size as u32 || returned > required {
        return Err(io::Error::other(
            "GetTokenInformation returned an invalid token information length",
        ));
    }
    Ok(information)
}

fn copy_sid(source: PSID, description: &str) -> io::Result<OwnedSid> {
    // SAFETY: `source` came from successfully populated token information.
    if source.is_null() || unsafe { IsValidSid(source) } == 0 {
        return Err(io::Error::other(format!(
            "GetTokenInformation returned an invalid {description} SID"
        )));
    }
    // SAFETY: `source` is a valid SID.
    let sid_length = unsafe { GetLengthSid(source) };
    if sid_length == 0 {
        return Err(io::Error::last_os_error());
    }
    let sid_words = (sid_length as usize).div_ceil(size_of::<usize>());
    let mut storage = vec![0_usize; sid_words];
    // SAFETY: the destination has `sid_length` writable bytes and `source` is valid.
    if unsafe { CopySid(sid_length, storage.as_mut_ptr().cast(), source) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(OwnedSid { storage })
}

fn validate_token_owner(file: &File, path: &Path, token_owner: &OwnedSid) -> io::Result<()> {
    let mut owner = null_mut();
    let mut descriptor = null_mut();
    // SAFETY: output pointers are writable and the handle remains live.
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &raw mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &raw mut descriptor,
        )
    };
    let descriptor = LocalAllocation(descriptor);
    if status != ERROR_SUCCESS {
        return Err(wrap_error(
            "read Windows owner",
            path,
            io::Error::from_raw_os_error(status as i32),
        ));
    }
    // SAFETY: successful `GetSecurityInfo` returned an owner inside `descriptor`.
    if descriptor.0.is_null()
        || owner.is_null()
        || unsafe { IsValidSid(owner) } == 0
        || unsafe { EqualSid(owner, token_owner.as_psid()) } == 0
    {
        return Err(rejected(path, "owner is not the process token owner"));
    }
    Ok(())
}

fn private_acl(token_user: &OwnedSid, inheritance: u32) -> io::Result<LocalAllocation> {
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_ALL_ACCESS,
        grfAccessMode: SET_ACCESS,
        grfInheritance: inheritance,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: token_user.as_psid().cast(),
        },
    };
    let mut acl: *mut ACL = null_mut();
    // SAFETY: `entry` and its SID remain valid for the call; a null old ACL
    // requests an exact new ACL allocated with LocalAlloc.
    let status = unsafe { SetEntriesInAclW(1, &raw const entry, null(), &raw mut acl) };
    let allocation = LocalAllocation(acl.cast());
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    if allocation.0.is_null() {
        return Err(io::Error::other("SetEntriesInAclW returned a null ACL"));
    }
    Ok(allocation)
}

fn validate_private_security(
    file: &File,
    path: &Path,
    kind: PathKind,
    process_sids: &ProcessSids,
) -> io::Result<()> {
    let snapshot = security_snapshot(file, path, process_sids)?;
    let valid = snapshot.owner_is_token_owner
        && snapshot.dacl_is_protected
        && snapshot.ace_count == 1
        && snapshot.ace_is_allowed
        && snapshot.ace_mask == FILE_ALL_ACCESS
        && snapshot.ace_inheritance == kind.inheritance() as u8
        && snapshot.trustee_is_current_user;
    if !valid {
        return Err(rejected(
            path,
            &format!("private Windows DACL validation failed: {snapshot:?}"),
        ));
    }
    Ok(())
}

fn security_snapshot(
    file: &File,
    path: &Path,
    process_sids: &ProcessSids,
) -> io::Result<SecuritySnapshot> {
    let mut owner = null_mut();
    let mut dacl: *mut ACL = null_mut();
    let mut descriptor = null_mut();
    // SAFETY: all requested output pointers are writable and the handle remains live.
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &raw mut owner,
            null_mut(),
            &raw mut dacl,
            null_mut(),
            &raw mut descriptor,
        )
    };
    let descriptor = LocalAllocation(descriptor);
    if status != ERROR_SUCCESS {
        return Err(wrap_error(
            "validate private Windows DACL",
            path,
            io::Error::from_raw_os_error(status as i32),
        ));
    }
    // SAFETY: successful `GetSecurityInfo` initializes a security descriptor.
    if descriptor.0.is_null() || unsafe { IsValidSecurityDescriptor(descriptor.0) } == 0 {
        return Err(rejected(
            path,
            "Windows returned an invalid security descriptor",
        ));
    }

    let mut control = 0_u16;
    let mut revision = 0_u32;
    // SAFETY: the descriptor is valid and both output pointers are writable.
    if unsafe { GetSecurityDescriptorControl(descriptor.0, &raw mut control, &raw mut revision) }
        == 0
    {
        return Err(contextual_error(
            "read Windows security descriptor control",
            path,
        ));
    }
    if dacl.is_null() || unsafe { IsValidAcl(dacl) } == 0 {
        return Err(rejected(path, "Windows returned an invalid or null DACL"));
    }

    let mut size_information = MaybeUninit::<ACL_SIZE_INFORMATION>::uninit();
    // SAFETY: `dacl` is valid and the output buffer has the documented size.
    if unsafe {
        GetAclInformation(
            dacl,
            size_information.as_mut_ptr().cast(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    } == 0
    {
        return Err(contextual_error("inspect private Windows DACL", path));
    }
    // SAFETY: a nonzero result initializes the complete output structure.
    let size_information = unsafe { size_information.assume_init() };

    let owner_is_token_owner = !owner.is_null()
        && unsafe { IsValidSid(owner) } != 0
        && unsafe { EqualSid(owner, process_sids.owner.as_psid()) } != 0;
    let mut snapshot = SecuritySnapshot {
        owner_is_token_owner,
        dacl_is_protected: control & SE_DACL_PROTECTED != 0,
        ace_count: size_information.AceCount,
        ace_is_allowed: false,
        ace_mask: 0,
        ace_inheritance: 0,
        trustee_is_current_user: false,
    };
    if size_information.AceCount != 1 {
        return Ok(snapshot);
    }

    let mut raw_ace = null_mut();
    // SAFETY: `dacl` is valid, has one ACE, and `raw_ace` is writable.
    if unsafe { GetAce(dacl, 0, &raw mut raw_ace) } == 0 || raw_ace.is_null() {
        return Err(contextual_error("read private Windows DACL ACE", path));
    }
    // SAFETY: `GetAce` returned a pointer to an ACE with at least an ACE header.
    let header = unsafe { &*raw_ace.cast::<windows_sys::Win32::Security::ACE_HEADER>() };
    snapshot.ace_is_allowed = u32::from(header.AceType) == ACCESS_ALLOWED_ACE_TYPE;
    snapshot.ace_inheritance = header.AceFlags;
    if !snapshot.ace_is_allowed || usize::from(header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>() {
        return Ok(snapshot);
    }

    // SAFETY: the ACE type and size establish the `ACCESS_ALLOWED_ACE` prefix.
    let ace = unsafe { &*raw_ace.cast::<ACCESS_ALLOWED_ACE>() };
    snapshot.ace_mask = ace.Mask;
    let trustee = addr_of!(ace.SidStart).cast_mut().cast();
    // SAFETY: an access-allowed ACE stores its SID starting at `SidStart`.
    snapshot.trustee_is_current_user = unsafe { IsValidSid(trustee) } != 0
        && unsafe { EqualSid(trustee, process_sids.user.as_psid()) } != 0;
    Ok(snapshot)
}

fn contextual_error(operation: &str, path: &Path) -> io::Error {
    wrap_error(operation, path, io::Error::last_os_error())
}

fn wrap_error(operation: &str, path: &Path, source: io::Error) -> io::Error {
    if source.raw_os_error().is_some() {
        return source;
    }
    io::Error::new(
        source.kind(),
        format!("{operation} failed for '{}': {source}", path.display()),
    )
}

fn rejected(path: &Path, reason: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "refused Windows security path '{}': {reason}",
            path.display()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::windows::fs::symlink_dir;
    use windows_sys::Win32::Foundation::ERROR_PRIVILEGE_NOT_HELD;

    fn snapshot(path: &Path, kind: PathKind) -> SecuritySnapshot {
        let file = open_handle(path, OPEN_EXISTING, SECURITY_ACCESS, null()).unwrap();
        validate_file_kind(&file, path, kind).unwrap();
        let process_sids = current_process_sids().unwrap();
        security_snapshot(&file, path, &process_sids).unwrap()
    }

    #[test]
    fn directory_acl_is_protected_current_user_only_and_inheritable() {
        let temp = tempfile::tempdir().unwrap();

        restrict_directory(temp.path()).unwrap();

        let snapshot = snapshot(temp.path(), PathKind::Directory);
        assert!(snapshot.owner_is_token_owner);
        assert!(snapshot.dacl_is_protected);
        assert_eq!(snapshot.ace_count, 1);
        assert!(snapshot.ace_is_allowed);
        assert_eq!(snapshot.ace_mask, FILE_ALL_ACCESS);
        assert_eq!(
            snapshot.ace_inheritance,
            SUB_CONTAINERS_AND_OBJECTS_INHERIT as u8
        );
        assert!(snapshot.trustee_is_current_user);
    }

    #[test]
    fn missing_directory_chain_is_private_from_creation() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("private");
        let nested = first.join("nested");

        create_private_dir_all(&nested).unwrap();

        for path in [&first, &nested] {
            let snapshot = snapshot(path, PathKind::Directory);
            assert!(snapshot.owner_is_token_owner);
            assert!(snapshot.dacl_is_protected);
            assert_eq!(snapshot.ace_count, 1);
            assert_eq!(
                snapshot.ace_inheritance,
                SUB_CONTAINERS_AND_OBJECTS_INHERIT as u8
            );
            assert!(snapshot.trustee_is_current_user);
        }
    }

    #[test]
    fn file_acl_is_protected_current_user_only_without_inheritance() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("secret");

        drop(create_private_file(&path).unwrap());

        let snapshot = snapshot(&path, PathKind::File);
        assert!(snapshot.owner_is_token_owner);
        assert!(snapshot.dacl_is_protected);
        assert_eq!(snapshot.ace_count, 1);
        assert!(snapshot.ace_is_allowed);
        assert_eq!(snapshot.ace_mask, FILE_ALL_ACCESS);
        assert_eq!(snapshot.ace_inheritance, NO_INHERITANCE as u8);
        assert!(snapshot.trustee_is_current_user);
    }

    #[test]
    fn existing_file_is_secured_before_contents_are_read() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("legacy-secret");
        std::fs::write(&path, b"secret").unwrap();

        let mut file = open_private_file(&path).unwrap();
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).unwrap();

        assert_eq!(contents, b"secret");
        let snapshot = snapshot(&path, PathKind::File);
        assert!(snapshot.dacl_is_protected);
        assert_eq!(snapshot.ace_count, 1);
        assert!(snapshot.trustee_is_current_user);
    }

    #[test]
    fn file_restriction_rejects_a_directory() {
        let temp = tempfile::tempdir().unwrap();

        let error = restrict_file(temp.path()).unwrap_err();

        assert!(error.to_string().contains("expected a regular file"));
    }

    #[test]
    fn ancestor_reparse_points_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        let nested = target.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let redirect = temp.path().join("redirect");
        match symlink_dir(&target, &redirect) {
            Ok(()) => {}
            Err(error)
                if error.kind() == io::ErrorKind::PermissionDenied
                    || error.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD as i32) =>
            {
                return;
            }
            Err(error) => panic!("failed to create test directory symlink: {error}"),
        }

        let error = restrict_directory(&redirect.join("nested")).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("reparse points are not allowed"));
    }

    #[test]
    fn native_error_codes_are_preserved() {
        let error = wrap_error(
            "test Windows operation",
            Path::new("test"),
            io::Error::from_raw_os_error(5),
        );

        assert_eq!(error.raw_os_error(), Some(5));
    }
}
