#[cfg(not(windows))]
use std::fs::OpenOptions;
use std::io::{Read, Write};
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use tracedecay_domain::errors::Result;
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW, ReplaceFileW};

use super::{PROCESS_STARTED_EPOCH_MS, TOKEN_NONCE, WriterOwner, access_error, access_io_error};

pub(super) fn publish_record_atomically(
    temporary: &Path,
    destination: &Path,
    payload: &[u8],
    record_name: &str,
) -> Result<()> {
    #[cfg(not(windows))]
    let mut options = OpenOptions::new();
    #[cfg(not(windows))]
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut created = false;
    let publish = (|| {
        #[cfg(windows)]
        let mut file =
            crate::windows_security::create_private_file(temporary).map_err(|error| {
                access_io_error(&format!("create {record_name}"), temporary, &error)
            })?;
        #[cfg(not(windows))]
        let mut file = options.open(temporary).map_err(|error| {
            access_io_error(&format!("create {record_name}"), temporary, &error)
        })?;
        created = true;
        file.write_all(payload)
            .and_then(|()| file.sync_all())
            .map_err(|error| access_io_error(&format!("write {record_name}"), temporary, &error))?;
        drop(file);
        replace_file_atomically(temporary, destination, record_name)?;
        #[cfg(windows)]
        crate::windows_security::validate_private_file(destination).map_err(|error| {
            access_io_error(
                &format!("validate published {record_name}"),
                destination,
                &error,
            )
        })?;
        sync_parent_directory(destination, record_name)
    })();
    if publish.is_err() && created {
        let _ = std::fs::remove_file(temporary);
    }
    publish
}

pub(super) fn read_record_strict(path: &Path, record_name: &str) -> Result<Option<String>> {
    const MAX_RECORD_BYTES: u64 = 4096;

    #[cfg(windows)]
    let file = match crate::windows_security::open_private_file(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(access_io_error(
                &format!("secure {record_name} before read"),
                path,
                &error,
            ));
        }
    };

    #[cfg(not(windows))]
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(access_io_error(
                &format!("inspect {record_name}"),
                path,
                &error,
            ));
        }
    };
    #[cfg(not(windows))]
    if metadata.file_type().is_symlink() {
        return Err(access_error(
            &format!("read {record_name}"),
            path,
            &format!("{record_name} must not be a symlink"),
        ));
    }
    #[cfg(not(windows))]
    if !metadata.is_file() {
        return Err(access_error(
            &format!("read {record_name}"),
            path,
            &format!("{record_name} is not a regular file"),
        ));
    }

    #[cfg(not(windows))]
    let mut options = OpenOptions::new();
    #[cfg(not(windows))]
    options.read(true);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        const O_NOFOLLOW: i32 = 0o40_0000;
        options.custom_flags(O_NOFOLLOW);
    }
    #[cfg(not(windows))]
    let file = options
        .open(path)
        .map_err(|error| access_io_error(&format!("read {record_name}"), path, &error))?;
    let mut bytes = Vec::new();
    file.take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| access_io_error(&format!("read {record_name}"), path, &error))?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(access_error(
            &format!("read {record_name}"),
            path,
            &format!("{record_name} exceeds {MAX_RECORD_BYTES} bytes"),
        ));
    }
    String::from_utf8(bytes).map(Some).map_err(|_| {
        access_error(
            &format!("read {record_name}"),
            path,
            &format!("{record_name} is not valid UTF-8"),
        )
    })
}

#[cfg(not(windows))]
pub(super) fn replace_file_atomically(
    temporary: &Path,
    path: &Path,
    record_name: &str,
) -> Result<()> {
    std::fs::rename(temporary, path)
        .map_err(|error| access_io_error(&format!("publish {record_name}"), path, &error))
}

pub(super) fn replace_sqlite_with_rollback_atomically(
    staging: &Path,
    destination: &Path,
    rollback: &Path,
    expected_destination_identity: u64,
    expected_staging_identity: u64,
) -> Result<()> {
    let same_parent =
        staging.parent() == destination.parent() && destination.parent() == rollback.parent();
    if !same_parent || staging == destination || staging == rollback || destination == rollback {
        return Err(access_error(
            "publish restored SQLite database",
            destination,
            "staging, destination, and rollback must be distinct siblings",
        ));
    }
    if rollback.exists() {
        return Err(access_error(
            "publish restored SQLite database",
            rollback,
            "rollback destination already exists",
        ));
    }
    let destination_identity =
        crate::db::sqlite_generation_identity(destination).map_err(|error| {
            access_error(
                "verify restore destination identity",
                destination,
                &format!("{error:?}"),
            )
        })?;
    let staging_identity = crate::db::sqlite_generation_identity(staging).map_err(|error| {
        access_error(
            "verify restore staging identity",
            staging,
            &format!("{error:?}"),
        )
    })?;
    if destination_identity != expected_destination_identity
        || staging_identity != expected_staging_identity
    {
        return Err(access_error(
            "publish restored SQLite database",
            destination,
            "pre-publication SQLite identity changed",
        ));
    }
    platform_replace_with_rollback(staging, destination, rollback)?;
    let published_identity =
        crate::db::sqlite_generation_identity(destination).map_err(|error| {
            access_error(
                "verify published restore identity",
                destination,
                &format!("{error:?}"),
            )
        })?;
    let rollback_identity = crate::db::sqlite_generation_identity(rollback).map_err(|error| {
        access_error(
            "verify rollback SQLite identity",
            rollback,
            &format!("{error:?}"),
        )
    })?;
    if published_identity == expected_staging_identity
        && rollback_identity == expected_destination_identity
    {
        sync_parent_directory(destination, "restored SQLite database")?;
        return Ok(());
    }
    platform_replace_with_rollback(rollback, destination, staging)?;
    Err(access_error(
        "publish restored SQLite database",
        destination,
        "atomic publication identity verification failed and was rolled back",
    ))
}

#[cfg(target_os = "linux")]
fn platform_replace_with_rollback(
    replacement: &Path,
    destination: &Path,
    rollback: &Path,
) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let replacement_c = CString::new(replacement.as_os_str().as_bytes()).map_err(|_| {
        access_error(
            "publish restored SQLite database",
            replacement,
            "replacement path contains NUL",
        )
    })?;
    let destination_c = CString::new(destination.as_os_str().as_bytes()).map_err(|_| {
        access_error(
            "publish restored SQLite database",
            destination,
            "destination path contains NUL",
        )
    })?;
    let exchanged = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            replacement_c.as_ptr(),
            libc::AT_FDCWD,
            destination_c.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if exchanged != 0 {
        return Err(access_io_error(
            "atomically exchange restored SQLite database",
            destination,
            &std::io::Error::last_os_error(),
        ));
    }
    if let Err(error) = std::fs::rename(replacement, rollback) {
        let rollback_exchange = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                replacement_c.as_ptr(),
                libc::AT_FDCWD,
                destination_c.as_ptr(),
                libc::RENAME_EXCHANGE,
            )
        };
        return if rollback_exchange == 0 {
            Err(access_io_error(
                "retain replaced SQLite rollback",
                rollback,
                &error,
            ))
        } else {
            Err(access_error(
                "retain replaced SQLite rollback",
                rollback,
                "rollback retention failed after exchange and requires forward recovery",
            ))
        };
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn platform_replace_with_rollback(
    replacement: &Path,
    destination: &Path,
    rollback: &Path,
) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    const RENAME_SWAP: u32 = 0x0000_0002;
    unsafe extern "C" {
        fn renamex_np(
            from: *const libc::c_char,
            to: *const libc::c_char,
            flags: u32,
        ) -> libc::c_int;
    }
    let replacement_c = CString::new(replacement.as_os_str().as_bytes()).map_err(|_| {
        access_error(
            "publish restored SQLite database",
            replacement,
            "replacement path contains NUL",
        )
    })?;
    let destination_c = CString::new(destination.as_os_str().as_bytes()).map_err(|_| {
        access_error(
            "publish restored SQLite database",
            destination,
            "destination path contains NUL",
        )
    })?;
    let exchanged =
        unsafe { renamex_np(replacement_c.as_ptr(), destination_c.as_ptr(), RENAME_SWAP) };
    if exchanged != 0 {
        return Err(access_io_error(
            "atomically exchange restored SQLite database",
            destination,
            &std::io::Error::last_os_error(),
        ));
    }
    if let Err(error) = std::fs::rename(replacement, rollback) {
        let rollback_exchange =
            unsafe { renamex_np(replacement_c.as_ptr(), destination_c.as_ptr(), RENAME_SWAP) };
        return if rollback_exchange == 0 {
            Err(access_io_error(
                "retain replaced SQLite rollback",
                rollback,
                &error,
            ))
        } else {
            Err(access_error(
                "retain replaced SQLite rollback",
                rollback,
                "rollback retention failed after exchange and requires forward recovery",
            ))
        };
    }
    Ok(())
}

#[cfg(windows)]
fn platform_replace_with_rollback(
    replacement: &Path,
    destination: &Path,
    rollback: &Path,
) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn ReplaceFileW(
            replaced: *const u16,
            replacement: *const u16,
            backup: *const u16,
            flags: u32,
            exclude: *mut core::ffi::c_void,
            reserved: *mut core::ffi::c_void,
        ) -> i32;
    }
    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>()
    };
    let replaced = wide(destination);
    let replacement = wide(replacement);
    let backup = wide(rollback);
    let result = unsafe {
        ReplaceFileW(
            replaced.as_ptr(),
            replacement.as_ptr(),
            backup.as_ptr(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if result == 0 {
        Err(access_io_error(
            "atomically replace restored SQLite database",
            destination,
            &std::io::Error::last_os_error(),
        ))
    } else {
        Ok(())
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn platform_replace_with_rollback(
    _replacement: &Path,
    destination: &Path,
    _rollback: &Path,
) -> Result<()> {
    Err(access_error(
        "publish restored SQLite database",
        destination,
        "atomic exchange with retained rollback is unsupported on this platform",
    ))
}

#[cfg(windows)]
type WindowsFileIdentity = (u32, u64);

#[cfg(windows)]
struct WindowsReplacementState {
    temporary: Option<(std::fs::File, WindowsFileIdentity)>,
    destination: Option<(std::fs::File, WindowsFileIdentity)>,
    backup: Option<(std::fs::File, WindowsFileIdentity)>,
}

#[cfg(windows)]
fn windows_file_identity(file: &std::fs::File) -> std::io::Result<WindowsFileIdentity> {
    tracedecay_private_fs::windows_file::information(file)
        .map(|information| (information.volume_serial_number, information.file_index))
}

#[cfg(windows)]
fn open_windows_identity(
    path: &Path,
) -> std::io::Result<Option<(std::fs::File, WindowsFileIdentity)>> {
    match crate::windows_security::open_private_file(path) {
        Ok(file) => {
            let identity = windows_file_identity(&file)?;
            Ok(Some((file, identity)))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn has_windows_identity(
    file: &Option<(std::fs::File, WindowsFileIdentity)>,
    identity: WindowsFileIdentity,
) -> bool {
    file.as_ref().is_some_and(|(_, found)| *found == identity)
}

#[cfg(windows)]
fn inspect_windows_identity_bounded(path: &Path) -> std::io::Result<Option<WindowsFileIdentity>> {
    let mut identity = None;
    crate::storage::retry_transient_file_op(|| {
        identity = Some(open_windows_identity(path)?.map(|(_, identity)| identity));
        Ok(())
    })?;
    Ok(identity.flatten())
}

#[cfg(windows)]
fn inspect_windows_replacement(
    temporary: &Path,
    destination: &Path,
    backup: &Path,
) -> std::io::Result<WindowsReplacementState> {
    Ok(WindowsReplacementState {
        temporary: open_windows_identity(temporary)?,
        destination: open_windows_identity(destination)?,
        backup: open_windows_identity(backup)?,
    })
}

#[cfg(windows)]
fn encode_windows_path(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(windows)]
fn sync_exact_windows_file(
    file: &std::fs::File,
    identity: WindowsFileIdentity,
) -> std::io::Result<()> {
    let actual = crate::storage::retry_transient_file_op(|| file.sync_all())
        .and_then(|()| windows_file_identity(file));
    match actual {
        Ok(actual) if actual == identity => Ok(()),
        Ok(_) => Err(std::io::Error::other("synced Windows identity changed")),
        Err(error) => Err(std::io::Error::new(
            error.kind(),
            format!("exact Windows sync failed: {error}"),
        )),
    }
}

#[cfg(windows)]
fn move_windows_file_no_replace(
    source: &Path,
    destination: &Path,
    identity: WindowsFileIdentity,
) -> std::io::Result<()> {
    let source_wide = encode_windows_path(source);
    let destination_wide = encode_windows_path(destination);
    crate::storage::retry_transient_file_op(|| {
        let moved = unsafe {
            MoveFileExW(
                source_wide.as_ptr(),
                destination_wide.as_ptr(),
                MOVEFILE_WRITE_THROUGH,
            )
        };
        let native_error = (moved == 0).then(std::io::Error::last_os_error);
        let source_after = open_windows_identity(source)?;
        let destination_after = open_windows_identity(destination)?;
        if source_after.is_none()
            && let Some((file, destination_identity)) = destination_after
            && destination_identity == identity
        {
            return sync_exact_windows_file(&file, identity);
        }
        match native_error {
            Some(error) => Err(error),
            None => Err(std::io::Error::other("contradictory no-replace move")),
        }
    })
}

#[cfg(windows)]
fn contextual_windows_error(
    error: &std::io::Error,
    message: impl std::fmt::Display,
) -> std::io::Error {
    std::io::Error::new(error.kind(), format!("{error}; {message}"))
}

#[cfg(windows)]
fn finish_windows_replacement(
    state: &WindowsReplacementState,
    temporary_identity: WindowsFileIdentity,
    destination_identity: WindowsFileIdentity,
    backup: &Path,
) -> std::io::Result<bool> {
    if state.temporary.is_some()
        || !has_windows_identity(&state.destination, temporary_identity)
        || !has_windows_identity(&state.backup, destination_identity)
    {
        return Ok(false);
    }
    let (destination, _) = state.destination.as_ref().expect("verified destination");
    sync_exact_windows_file(&destination, temporary_identity)?;
    crate::storage::retry_transient_file_op(|| std::fs::remove_file(backup))
        .map_err(|error| contextual_windows_error(&error, "could not remove verified backup"))?;
    Ok(true)
}

#[cfg(windows)]
pub(super) fn replace_file_atomically(
    temporary: &Path,
    path: &Path,
    record_name: &str,
) -> Result<()> {
    let temporary_identity = inspect_windows_identity_bounded(temporary)
        .and_then(|identity| {
            identity.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "temporary"))
        })
        .map_err(|error| {
            access_io_error(
                &format!("inspect {record_name} replacement"),
                temporary,
                &error,
            )
        })?;
    let mut backup = path.as_os_str().to_owned();
    backup.push(".tracedecay-replace-backup");
    let backup = std::path::PathBuf::from(backup);
    let mut destination_identity = inspect_windows_identity_bounded(path).map_err(|error| {
        access_io_error(&format!("inspect {record_name} destination"), path, &error)
    })?;
    let backup_identity = inspect_windows_identity_bounded(&backup).map_err(|error| {
        access_io_error(&format!("inspect {record_name} backup"), &backup, &error)
    })?;

    match (destination_identity, backup_identity) {
        (None, Some(identity)) => {
            move_windows_file_no_replace(&backup, path, identity).map_err(|error| {
                access_io_error(&format!("restore {record_name} backup"), &backup, &error)
            })?;
            destination_identity = Some(identity);
        }
        (Some(_), Some(_)) => {
            return Err(access_error(
                &format!("publish {record_name}"),
                path,
                &format!(
                    "destination and reserved backup both exist at '{}'",
                    backup.display()
                ),
            ));
        }
        _ => {}
    }

    let Some(destination_identity) = destination_identity else {
        return move_windows_file_no_replace(temporary, path, temporary_identity)
            .map_err(|error| access_io_error(&format!("publish {record_name}"), path, &error));
    };
    let temporary_wide = encode_windows_path(temporary);
    let destination_wide = encode_windows_path(path);
    let backup_wide = encode_windows_path(&backup);
    crate::storage::retry_transient_file_op(|| {
        let replaced = unsafe {
            ReplaceFileW(
                destination_wide.as_ptr(),
                temporary_wide.as_ptr(),
                backup_wide.as_ptr(),
                0,
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        let native_error = (replaced == 0).then(std::io::Error::last_os_error);
        let state = inspect_windows_replacement(temporary, path, &backup).map_err(|error| {
            contextual_windows_error(
                &error,
                format!("could not reconcile reserved backup '{}'", backup.display()),
            )
        })?;
        if finish_windows_replacement(&state, temporary_identity, destination_identity, &backup)? {
            return Ok(());
        }

        let error = native_error.unwrap_or_else(|| {
            std::io::Error::other("ReplaceFileW reported success with contradictory identities")
        });
        let code = error.raw_os_error();
        let names_retained = has_windows_identity(&state.destination, destination_identity)
            && has_windows_identity(&state.temporary, temporary_identity);
        if matches!(code, Some(1175 | 1176)) && names_retained {
            return Err(error);
        }
        let recoverable_1177 = code == Some(1177)
            && state.destination.is_none()
            && has_windows_identity(&state.temporary, temporary_identity)
            && has_windows_identity(&state.backup, destination_identity);
        if recoverable_1177 {
            drop(state);
            return match move_windows_file_no_replace(&backup, path, destination_identity) {
                Ok(())
                    if inspect_windows_identity_bounded(temporary)? == Some(temporary_identity) =>
                {
                    Err(error)
                }
                Ok(()) => Err(contextual_windows_error(
                    &error,
                    format!(
                        "1177 recovery changed replacement identity; backup: '{}'",
                        backup.display()
                    ),
                )),
                Err(recovery_error) => Err(contextual_windows_error(
                    &error,
                    format!(
                        "1177 recovery failed: {recovery_error}; backup: '{}'",
                        backup.display()
                    ),
                )),
            };
        }
        if names_retained && state.backup.is_none() {
            return Err(error);
        }
        Err(contextual_windows_error(
            &error,
            format!(
                "reserved backup state is contradictory at '{}'",
                backup.display()
            ),
        ))
    })
    .map_err(|error| access_io_error(&format!("publish {record_name}"), path, &error))
}

pub(super) fn sync_parent_directory(path: &Path, record_name: &str) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        access_error(
            &format!("sync {record_name} directory"),
            path,
            &format!("{record_name} path has no parent directory"),
        )
    })?;
    tracedecay_private_fs::framed_log::sync_directory(
        parent,
        tracedecay_private_fs::framed_log::DirectorySyncPolicy::Strict,
    )
    .map_err(|error| access_io_error(&format!("sync {record_name} directory"), parent, &error))
}

pub(super) fn writer_owner(token: &str, intent: &str) -> WriterOwner {
    WriterOwner {
        token: token.to_string(),
        pid: std::process::id(),
        started_epoch_ms: *PROCESS_STARTED_EPOCH_MS,
        version: env!("CARGO_PKG_VERSION").to_string(),
        intent: sanitize_metadata(intent),
    }
}

#[cfg(windows)]
pub fn is_lock_contended(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock || error.raw_os_error() == Some(33)
}

#[cfg(not(windows))]
pub fn is_lock_contended(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock
}

pub(super) fn authority_token() -> String {
    let nonce = TOKEN_NONCE.fetch_add(1, Ordering::Relaxed);
    format!(
        "{}:{}:{}:{nonce}",
        crate::runtime_identity::process_run_id(),
        std::process::id(),
        epoch_ms()
    )
}

pub(super) fn epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn sanitize_metadata(value: &str) -> String {
    const MAX_METADATA_BYTES: usize = 256;

    let mut sanitized = value.replace(['\t', '\r', '\n'], " ");
    if sanitized.len() > MAX_METADATA_BYTES {
        let mut boundary = MAX_METADATA_BYTES;
        while !sanitized.is_char_boundary(boundary) {
            boundary -= 1;
        }
        sanitized.truncate(boundary);
    }
    sanitized
}
