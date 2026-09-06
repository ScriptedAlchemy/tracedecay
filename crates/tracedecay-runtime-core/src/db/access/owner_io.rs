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
enum WindowsPublishNamespace {
    Published(std::fs::File),
    NamesRetained,
    Occupied,
    MoveReady,
    Contradictory,
}

#[cfg(windows)]
#[derive(Clone, Copy)]
enum WindowsPublishStep {
    Replace,
    MoveNoReplace,
    Inspect(bool, Option<i32>),
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
fn inspect_windows_publish_namespace(
    temporary: &Path,
    destination: &Path,
    intended: WindowsFileIdentity,
) -> std::io::Result<WindowsPublishNamespace> {
    let destination = open_windows_identity(destination)?;
    let temporary = open_windows_identity(temporary)?;
    Ok(match (destination, temporary) {
        (Some((file, identity)), None) if identity == intended => {
            WindowsPublishNamespace::Published(file)
        }
        (Some((_, destination)), Some((_, temporary)))
            if destination != intended && temporary == intended =>
        {
            WindowsPublishNamespace::NamesRetained
        }
        (Some((_, identity)), _) if identity != intended => WindowsPublishNamespace::Occupied,
        (None, Some((_, identity))) if identity == intended => WindowsPublishNamespace::MoveReady,
        _ => WindowsPublishNamespace::Contradictory,
    })
}

#[cfg(windows)]
fn windows_error_code() -> i32 {
    std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(i32::MIN)
}

#[cfg(windows)]
fn is_transient_windows_publish_error(code: i32) -> bool {
    matches!(code, 5 | 32 | 33)
}

#[cfg(windows)]
fn is_move_ready_replace_error(code: i32) -> bool {
    matches!(code, 2 | 1176 | 1177)
}

#[cfg(windows)]
fn required_windows_error(code: Option<i32>, message: &str) -> std::io::Result<std::io::Error> {
    code.map(std::io::Error::from_raw_os_error)
        .ok_or_else(|| std::io::Error::other(message.to_string()))
}

#[cfg(windows)]
fn contextual_windows_publish_error(native_error: Option<i32>, message: &str) -> std::io::Error {
    match native_error {
        Some(code) => {
            let error = std::io::Error::from_raw_os_error(code);
            std::io::Error::new(error.kind(), format!("{error}; {message}"))
        }
        None => std::io::Error::other(message.to_string()),
    }
}

#[cfg(windows)]
pub(super) fn replace_file_atomically(
    temporary: &Path,
    path: &Path,
    record_name: &str,
) -> Result<()> {
    let inspection = crate::windows_security::open_private_file(temporary).map_err(|error| {
        access_io_error(
            &format!("inspect {record_name} replacement"),
            temporary,
            &error,
        )
    })?;
    let intended = windows_file_identity(&inspection).map_err(|error| {
        access_io_error(
            &format!("identify {record_name} replacement"),
            temporary,
            &error,
        )
    })?;
    drop(inspection);

    let temporary_wide = temporary
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination_wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut step = WindowsPublishStep::Replace;
    crate::storage::retry_transient_file_op(|| {
        loop {
            match step {
                WindowsPublishStep::Replace => {
                    let replaced = unsafe {
                        ReplaceFileW(
                            destination_wide.as_ptr(),
                            temporary_wide.as_ptr(),
                            std::ptr::null(),
                            0,
                            std::ptr::null(),
                            std::ptr::null(),
                        )
                    };
                    step = WindowsPublishStep::Inspect(
                        false,
                        (replaced == 0).then(windows_error_code),
                    );
                }
                WindowsPublishStep::MoveNoReplace => {
                    let moved = unsafe {
                        MoveFileExW(
                            temporary_wide.as_ptr(),
                            destination_wide.as_ptr(),
                            MOVEFILE_WRITE_THROUGH,
                        )
                    };
                    step = WindowsPublishStep::Inspect(true, (moved == 0).then(windows_error_code));
                }
                WindowsPublishStep::Inspect(move_only, native_error) => {
                    match inspect_windows_publish_namespace(temporary, path, intended)? {
                        WindowsPublishNamespace::Published(file) => {
                            file.sync_all()?;
                            if windows_file_identity(&file)? == intended {
                                return Ok(());
                            }
                            return Err(std::io::Error::other(
                                "published Windows file changed identity while syncing",
                            ));
                        }
                        WindowsPublishNamespace::MoveReady
                            if !move_only
                                && native_error.is_some_and(is_move_ready_replace_error) =>
                        {
                            step = WindowsPublishStep::MoveNoReplace;
                        }
                        WindowsPublishNamespace::NamesRetained => {
                            let error = required_windows_error(
                                native_error,
                                "Windows publish reported success but retained both names",
                            )?;
                            if is_transient_windows_publish_error(
                                error.raw_os_error().unwrap_or_default(),
                            ) {
                                step = if move_only {
                                    WindowsPublishStep::MoveNoReplace
                                } else {
                                    WindowsPublishStep::Replace
                                };
                            }
                            return Err(error);
                        }
                        WindowsPublishNamespace::Occupied => {
                            return Err(required_windows_error(
                                native_error,
                                "Windows destination was occupied after reported success",
                            )?);
                        }
                        WindowsPublishNamespace::MoveReady if move_only => {
                            let error = required_windows_error(
                                native_error,
                                "no-replace Windows move reported success without moving",
                            )?;
                            if is_transient_windows_publish_error(
                                error.raw_os_error().unwrap_or_default(),
                            ) {
                                step = WindowsPublishStep::MoveNoReplace;
                            }
                            return Err(error);
                        }
                        WindowsPublishNamespace::MoveReady => {
                            return Err(contextual_windows_publish_error(
                                native_error,
                                "Windows replacement left only the temporary name",
                            ));
                        }
                        WindowsPublishNamespace::Contradictory => {
                            return Err(contextual_windows_publish_error(
                                native_error,
                                "Windows replacement produced contradictory file identities",
                            ));
                        }
                    }
                }
            }
        }
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
