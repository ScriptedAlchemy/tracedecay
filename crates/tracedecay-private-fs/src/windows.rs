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
    ConvertSidToStringSidW, EXPLICIT_ACCESS_W, GetSecurityInfo, NO_MULTIPLE_TRUSTEE,
    SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW, SetSecurityInfo, TRUSTEE_IS_SID, TRUSTEE_IS_USER,
    TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation, CopySid,
    DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetAclInformation, GetLengthSid,
    GetSecurityDescriptorControl, GetTokenInformation, InitializeSecurityDescriptor, IsValidAcl,
    IsValidSecurityDescriptor, IsValidSid, NO_INHERITANCE, OWNER_SECURITY_INFORMATION,
    PROTECTED_DACL_SECURITY_INFORMATION, PSID, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES,
    SECURITY_DESCRIPTOR, SUB_CONTAINERS_AND_OBJECTS_INHERIT, SetSecurityDescriptorControl,
    SetSecurityDescriptorDacl, SetSecurityDescriptorOwner, TOKEN_INFORMATION_CLASS, TOKEN_QUERY,
    TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ALL_ACCESS, FILE_ATTRIBUTE_DEVICE,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ,
    FILE_GENERIC_WRITE, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FileAttributeTagInfo, GetDiskFreeSpaceExW, GetFileInformationByHandleEx, OPEN_ALWAYS,
    OPEN_EXISTING, READ_CONTROL, WRITE_DAC, WRITE_OWNER,
};
use windows_sys::Win32::System::SystemServices::{
    ACCESS_ALLOWED_ACE_TYPE, SECURITY_DESCRIPTOR_REVISION,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

const SECURITY_ACCESS: u32 = READ_CONTROL | FILE_READ_ATTRIBUTES;
const SHARE_READ_WRITE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE;
const SHARE_READ_WRITE_DELETE: u32 = SHARE_READ_WRITE | FILE_SHARE_DELETE;
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

#[derive(Debug, PartialEq, Eq)]
struct SecuritySnapshot {
    owner_is_current_user: bool,
    dacl_is_protected: bool,
    ace_count: u32,
    ace_is_allowed: bool,
    ace_mask: u32,
    ace_inheritance: u8,
    trustee_is_current_user: bool,
}

#[derive(Debug)]
struct AclSnapshot {
    ace_count: u32,
    ace_is_allowed: bool,
    ace_mask: u32,
    ace_inheritance: u8,
    trustee_is_current_user: bool,
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

impl OwnedSid {
    fn as_psid(&self) -> PSID {
        self.storage.as_ptr().cast_mut().cast()
    }
}

/// Return the canonical string SID for the current process token user.
pub fn current_user_sid_string() -> io::Result<String> {
    let current_user = current_user_sid()?;
    let mut string_sid = null_mut();
    // SAFETY: the copied token SID remains live and `string_sid` is writable.
    if unsafe { ConvertSidToStringSidW(current_user.as_psid(), &raw mut string_sid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let allocation = LocalAllocation(string_sid.cast());
    if string_sid.is_null() {
        return Err(io::Error::other(
            "ConvertSidToStringSidW returned a null string",
        ));
    }
    let mut length = 0;
    // SAFETY: successful `ConvertSidToStringSidW` returns a NUL-terminated
    // LocalAlloc buffer that stays live through `allocation`.
    while unsafe { *string_sid.add(length) } != 0 {
        length += 1;
    }
    // SAFETY: `length` counts initialized UTF-16 code units before the NUL.
    let units = unsafe { std::slice::from_raw_parts(string_sid, length) };
    let value = String::from_utf16(units).map_err(|error| {
        io::Error::other(format!("current user SID is invalid UTF-16: {error}"))
    })?;
    drop(allocation);
    Ok(value)
}

/// Create one private directory without changing any existing ancestor ACL.
pub fn create_private_directory(path: &Path) -> io::Result<()> {
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
    })?;
    validate_private_directory(path)
}

/// Validate that a directory path and every existing ancestor are not reparse points.
pub fn validate_directory_path(path: &Path) -> io::Result<()> {
    let file = open_handle_with_share(
        path,
        OPEN_EXISTING,
        FILE_READ_ATTRIBUTES,
        null(),
        SHARE_READ_WRITE_DELETE,
    )?;
    validate_file_kind(&file, path, PathKind::Directory)
}

/// Validate an exact protected, inheritable current-user directory ACL.
pub fn validate_private_directory(path: &Path) -> io::Result<()> {
    drop(open_private_directory(path)?);
    Ok(())
}

/// Open an exact protected, inheritable current-user directory.
pub fn open_private_directory(path: &Path) -> io::Result<File> {
    open_and_validate(
        path,
        PathKind::Directory,
        OPEN_EXISTING,
        SECURITY_ACCESS,
        SHARE_READ_WRITE,
    )
}

/// Validate an exact protected current-user regular-file ACL.
pub fn validate_private_file(path: &Path) -> io::Result<()> {
    drop(open_and_validate(
        path,
        PathKind::File,
        OPEN_EXISTING,
        SECURITY_ACCESS,
        SHARE_READ_WRITE_DELETE,
    )?);
    Ok(())
}

/// Open an existing regular file only after validating its exact ACL.
pub fn open_private_file(path: &Path) -> io::Result<File> {
    open_and_validate(
        path,
        PathKind::File,
        OPEN_EXISTING,
        SECURITY_ACCESS | FILE_GENERIC_READ | FILE_GENERIC_WRITE,
        SHARE_READ_WRITE_DELETE,
    )
}

/// Protect an existing regular file through its exact opened handle.
pub fn make_private_file(path: &Path) -> io::Result<File> {
    let file = open_handle_with_share(
        path,
        OPEN_EXISTING,
        SECURITY_ACCESS | FILE_GENERIC_READ | FILE_GENERIC_WRITE | WRITE_DAC | WRITE_OWNER,
        null(),
        SHARE_READ_WRITE_DELETE,
    )?;
    validate_file_kind(&file, path, PathKind::File)?;
    protect_existing_file(&file, path)?;
    validate_private_handle(&file, path, PathKind::File)?;
    Ok(file)
}

/// Open or create a private lock file with concurrent read-write sharing.
pub fn open_or_create_private_lock_file(path: &Path) -> io::Result<File> {
    let file = open_with_private_creation_acl(
        path,
        PathKind::File,
        OPEN_ALWAYS,
        SECURITY_ACCESS | FILE_GENERIC_READ | FILE_GENERIC_WRITE,
        SHARE_READ_WRITE,
    )?;
    validate_private_handle(&file, path, PathKind::File)?;
    Ok(file)
}

/// Create a new empty regular file with an exact private ACL.
pub fn create_private_file(path: &Path) -> io::Result<File> {
    create_private_file_retained(path).map_err(crate::PrivateFileCreationFailure::into_error)
}

/// Create a new empty regular file while retaining its exact handle if
/// post-creation ACL validation fails.
pub fn create_private_file_retained(
    path: &Path,
) -> Result<File, crate::PrivateFileCreationFailure> {
    let file = open_with_private_creation_acl(
        path,
        PathKind::File,
        CREATE_NEW,
        SECURITY_ACCESS | FILE_GENERIC_READ | FILE_GENERIC_WRITE,
        SHARE_READ_WRITE_DELETE,
    )
    .map_err(crate::PrivateFileCreationFailure::before_creation)?;
    if let Err(error) = validate_private_handle(&file, path, PathKind::File) {
        return Err(crate::PrivateFileCreationFailure::after_creation(
            error, file,
        ));
    }
    Ok(file)
}

/// Returns bytes available to the current user at `path` (quota-aware).
pub fn available_space(path: &Path) -> io::Result<u64> {
    let encoded = encode_path(path)?;
    let mut available = 0_u64;
    // SAFETY: `encoded` is a NUL-terminated wide path and `available` is a
    // writable ULARGE_INTEGER. The total/free out-params are unused.
    let succeeded = unsafe {
        GetDiskFreeSpaceExW(
            encoded.as_ptr(),
            addr_of_mut!(available),
            null_mut(),
            null_mut(),
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(available)
}

fn open_and_validate(
    path: &Path,
    kind: PathKind,
    disposition: u32,
    access: u32,
    share_mode: u32,
) -> io::Result<File> {
    let file = open_handle_with_share(path, disposition, access, null(), share_mode)?;
    validate_private_handle(&file, path, kind)?;
    Ok(file)
}

fn open_with_private_creation_acl(
    path: &Path,
    kind: PathKind,
    disposition: u32,
    access: u32,
    share_mode: u32,
) -> io::Result<File> {
    with_private_security_attributes(path, kind, |attributes| {
        open_handle_with_share(path, disposition, access, attributes, share_mode)
    })
}

fn protect_existing_file(file: &File, path: &Path) -> io::Result<()> {
    let current_user = current_user_sid()
        .map_err(|error| wrap_error("resolve current Windows user SID", path, error))?;
    let acl = private_acl(&current_user, PathKind::File.inheritance())
        .map_err(|error| wrap_error("build private Windows file DACL", path, error))?;
    // SAFETY: the file handle, SID, and ACL are valid and remain live for the call.
    let status = unsafe {
        SetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION
                | DACL_SECURITY_INFORMATION
                | PROTECTED_DACL_SECURITY_INFORMATION,
            current_user.as_psid(),
            null_mut(),
            acl.0.cast(),
            null(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(wrap_error(
            "protect existing Windows file",
            path,
            io::Error::from_raw_os_error(status as i32),
        ));
    }
    Ok(())
}

fn with_private_security_attributes<T>(
    path: &Path,
    kind: PathKind,
    operation: impl FnOnce(*const SECURITY_ATTRIBUTES) -> io::Result<T>,
) -> io::Result<T> {
    let current_user = current_user_sid()
        .map_err(|error| wrap_error("resolve current Windows user SID", path, error))?;
    let acl = private_acl(&current_user, kind.inheritance())
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
    // SAFETY: the descriptor is initialized and `current_user` remains valid
    // for both descriptor assembly and the complete creation call.
    if unsafe {
        SetSecurityDescriptorOwner(addr_of_mut!(descriptor).cast(), current_user.as_psid(), 0)
    } == 0
    {
        return Err(contextual_error(
            "attach current Windows user as owner",
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

fn validate_private_handle(file: &File, path: &Path, kind: PathKind) -> io::Result<()> {
    validate_file_kind(file, path, kind)?;
    let current_user = current_user_sid()
        .map_err(|error| wrap_error("resolve current Windows user SID", path, error))?;
    validate_private_security(file, path, kind, &current_user)
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

fn current_user_sid() -> io::Result<OwnedSid> {
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
    // SAFETY: `token_information` verified the returned structure sizes and
    // keeps the aligned buffer live while its SID pointer is copied.
    let user = unsafe { (*token_user.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    copy_sid(user, "user")
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
    let snapshot = inspect_acl(allocation.0.cast(), token_user)?;
    if !acl_is_exact(&snapshot, inheritance) {
        return Err(io::Error::other(format!(
            "SetEntriesInAclW returned a non-private ACL: {snapshot:?}"
        )));
    }
    Ok(allocation)
}

fn acl_is_exact(snapshot: &AclSnapshot, inheritance: u32) -> bool {
    snapshot.ace_count == 1
        && snapshot.ace_is_allowed
        && snapshot.ace_mask == FILE_ALL_ACCESS
        && snapshot.ace_inheritance == inheritance as u8
        && snapshot.trustee_is_current_user
}

fn validate_private_security(
    file: &File,
    path: &Path,
    kind: PathKind,
    current_user: &OwnedSid,
) -> io::Result<()> {
    let snapshot = security_snapshot(file, path, current_user)?;
    let acl = AclSnapshot {
        ace_count: snapshot.ace_count,
        ace_is_allowed: snapshot.ace_is_allowed,
        ace_mask: snapshot.ace_mask,
        ace_inheritance: snapshot.ace_inheritance,
        trustee_is_current_user: snapshot.trustee_is_current_user,
    };
    let valid = snapshot.owner_is_current_user
        && snapshot.dacl_is_protected
        && acl_is_exact(&acl, kind.inheritance());
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
    current_user: &OwnedSid,
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
    let acl = inspect_acl(dacl, current_user)
        .map_err(|error| wrap_error("inspect private Windows DACL", path, error))?;
    let owner_is_current_user = !owner.is_null()
        && unsafe { IsValidSid(owner) } != 0
        && unsafe { EqualSid(owner, current_user.as_psid()) } != 0;
    Ok(SecuritySnapshot {
        owner_is_current_user,
        dacl_is_protected: control & SE_DACL_PROTECTED != 0,
        ace_count: acl.ace_count,
        ace_is_allowed: acl.ace_is_allowed,
        ace_mask: acl.ace_mask,
        ace_inheritance: acl.ace_inheritance,
        trustee_is_current_user: acl.trustee_is_current_user,
    })
}

fn inspect_acl(dacl: *mut ACL, current_user: &OwnedSid) -> io::Result<AclSnapshot> {
    if dacl.is_null() || unsafe { IsValidAcl(dacl) } == 0 {
        return Err(io::Error::other("Windows returned an invalid or null DACL"));
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
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a nonzero result initializes the complete output structure.
    let size_information = unsafe { size_information.assume_init() };
    let mut snapshot = AclSnapshot {
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
        return Err(io::Error::last_os_error());
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
        && unsafe { EqualSid(trustee, current_user.as_psid()) } != 0;
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
    use std::io::{Read, Write};
    use std::process::Command;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MoveFileExW};

    #[test]
    fn current_user_sid_string_is_canonical() {
        let sid = current_user_sid_string().unwrap();

        assert!(sid.starts_with("S-1-"));
        assert!(
            sid.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        );
    }

    fn snapshot(path: &Path, kind: PathKind) -> SecuritySnapshot {
        let file = open_handle_with_share(
            path,
            OPEN_EXISTING,
            SECURITY_ACCESS,
            null(),
            SHARE_READ_WRITE_DELETE,
        )
        .unwrap();
        validate_file_kind(&file, path, kind).unwrap();
        let current_user = current_user_sid().unwrap();
        security_snapshot(&file, path, &current_user).unwrap()
    }

    #[test]
    fn directory_acl_is_protected_current_user_only_and_inheritable() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("private");

        create_private_directory(&path).unwrap();

        let snapshot = snapshot(&path, PathKind::Directory);
        assert!(snapshot.owner_is_current_user);
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
    fn private_directory_is_private_from_creation() {
        let temp = tempfile::tempdir().unwrap();
        let private = temp.path().join("private");

        create_private_directory(&private).unwrap();

        let snapshot = snapshot(&private, PathKind::Directory);
        assert!(snapshot.owner_is_current_user);
        assert!(snapshot.dacl_is_protected);
        assert_eq!(snapshot.ace_count, 1);
        assert_eq!(
            snapshot.ace_inheritance,
            SUB_CONTAINERS_AND_OBJECTS_INHERIT as u8
        );
        assert!(snapshot.trustee_is_current_user);
    }

    #[test]
    fn file_acl_is_protected_current_user_only_without_inheritance() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("secret");

        drop(create_private_file(&path).unwrap());

        let snapshot = snapshot(&path, PathKind::File);
        assert!(snapshot.owner_is_current_user);
        assert!(snapshot.dacl_is_protected);
        assert_eq!(snapshot.ace_count, 1);
        assert!(snapshot.ace_is_allowed);
        assert_eq!(snapshot.ace_mask, FILE_ALL_ACCESS);
        assert_eq!(snapshot.ace_inheritance, NO_INHERITANCE as u8);
        assert!(snapshot.trustee_is_current_user);
    }

    #[test]
    fn ordinary_file_is_hardened_through_its_opened_handle() {
        let temp = tempfile::tempdir().unwrap();
        let private = temp.path().join("private");
        create_private_directory(&private).unwrap();
        let path = private.join("grafeo-created");
        drop(std::fs::File::create(&path).unwrap());

        drop(make_private_file(&path).unwrap());

        let snapshot = snapshot(&path, PathKind::File);
        assert!(snapshot.owner_is_current_user);
        assert!(snapshot.dacl_is_protected);
        assert_eq!(snapshot.ace_count, 1);
        assert!(snapshot.ace_is_allowed);
        assert_eq!(snapshot.ace_mask, FILE_ALL_ACCESS);
        assert_eq!(snapshot.ace_inheritance, NO_INHERITANCE as u8);
        assert!(snapshot.trustee_is_current_user);
    }

    #[test]
    fn permissive_existing_file_is_rejected_without_acl_rewrite() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("legacy-secret");
        std::fs::write(&path, b"secret").unwrap();
        let before = snapshot(&path, PathKind::File);

        let error = open_private_file(&path).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(snapshot(&path, PathKind::File), before);
        assert_eq!(std::fs::read(&path).unwrap(), b"secret");
    }

    #[test]
    fn private_lock_handles_can_open_concurrently_for_read_write() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("lock");

        let first = open_or_create_private_lock_file(&path).unwrap();
        let second = open_or_create_private_lock_file(&path).unwrap();

        drop((first, second));
    }

    #[test]
    fn private_reader_allows_atomic_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("record");
        let replacement = temp.path().join("replacement");
        let mut original = create_private_file(&path).unwrap();
        original.write_all(b"old").unwrap();
        drop(original);
        let mut reader = open_private_file(&path).unwrap();
        let mut replacement_file = create_private_file(&replacement).unwrap();
        replacement_file.write_all(b"new").unwrap();
        drop(replacement_file);
        let encoded_replacement = encode_path(&replacement).unwrap();
        let encoded_path = encode_path(&path).unwrap();

        // SAFETY: both paths are NUL-terminated and remain live for the call.
        let replaced = unsafe {
            MoveFileExW(
                encoded_replacement.as_ptr(),
                encoded_path.as_ptr(),
                MOVEFILE_REPLACE_EXISTING,
            )
        };

        assert_ne!(
            replaced,
            0,
            "replacement failed: {}",
            io::Error::last_os_error()
        );
        let mut old_contents = Vec::new();
        reader.read_to_end(&mut old_contents).unwrap();
        assert_eq!(old_contents, b"old");
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
    }

    #[test]
    fn file_restriction_rejects_a_directory() {
        let temp = tempfile::tempdir().unwrap();

        let error = validate_private_file(temp.path()).unwrap_err();

        assert!(error.to_string().contains("expected a regular file"));
    }

    #[test]
    fn ancestor_reparse_points_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        let nested = target.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let redirect = temp.path().join("redirect");
        let output = Command::new("cmd")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&redirect)
            .arg(&target)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "failed to create test junction: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let error = validate_directory_path(&redirect.join("nested")).unwrap_err();

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
