use std::collections::HashMap;
use std::fs::File;
#[cfg(not(windows))]
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::errors::Result;

use super::{
    AUTHORITY_NONCE, PROCESS_STARTED_EPOCH_MS, WriterOwner, access_error, access_io_error,
};

pub(super) fn open_lock_file(path: &Path) -> Result<File> {
    #[cfg(windows)]
    {
        crate::windows_security::open_or_create_private_file(path)
            .map_err(|error| access_io_error("open lock", path, &error))
    }

    #[cfg(not(windows))]
    let mut options = OpenOptions::new();
    #[cfg(not(windows))]
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(not(windows))]
    options
        .open(path)
        .map_err(|error| access_io_error("open lock", path, &error))
}

pub(super) fn write_owner(path: &Path, owner: &WriterOwner) -> Result<()> {
    let payload = format!(
        "token={}\tpid={}\tstarted_epoch_ms={}\tversion={}\tintent={}\n",
        owner.token, owner.pid, owner.started_epoch_ms, owner.version, owner.intent
    );
    write_record_atomically(path, payload.as_bytes(), "writer owner")
}

pub(super) fn write_record_atomically(
    path: &Path,
    payload: &[u8],
    record_name: &str,
) -> Result<()> {
    let file_name = path.file_name().ok_or_else(|| {
        access_error(
            &format!("write {record_name}"),
            path,
            &format!("{record_name} path has no file name"),
        )
    })?;
    let nonce = AUTHORITY_NONCE.fetch_add(1, Ordering::Relaxed);
    let temporary = temporary_record_path(path, file_name, nonce);
    publish_record_atomically(&temporary, path, payload, record_name)
}

fn temporary_record_path(
    path: &Path,
    file_name: &std::ffi::OsStr,
    nonce: u64,
) -> std::path::PathBuf {
    path.with_file_name(format!(
        ".{}.{}.{}.{}.tmp",
        file_name.to_string_lossy(),
        std::process::id(),
        crate::runtime_identity::process_run_id(),
        nonce
    ))
}

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
        crate::windows_security::restrict_file(destination).map_err(|error| {
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

pub(super) fn remove_record_durably(path: &Path, record_name: &str) -> Result<()> {
    std::fs::remove_file(path)
        .map_err(|error| access_io_error(&format!("remove {record_name}"), path, &error))?;
    sync_parent_directory(path, record_name)
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

#[cfg(windows)]
pub(super) fn replace_file_atomically(
    temporary: &Path,
    path: &Path,
    record_name: &str,
) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
    }

    let existing = temporary
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let replacement = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    crate::storage::retry_transient_file_op(|| {
        let replaced = unsafe {
            MoveFileExW(
                existing.as_ptr(),
                replacement.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if replaced == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
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
    // `application::host_admission` re-exports these from the domain crate;
    // the kernel imports the canonical definitions directly.
    tracedecay_domain::framed_log::sync_directory(
        parent,
        tracedecay_domain::framed_log::DirectorySyncPolicy::Strict,
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

pub(super) fn read_owner(path: &Path) -> Option<WriterOwner> {
    let mut value = String::new();
    #[cfg(windows)]
    let mut file = crate::windows_security::open_private_file(path).ok()?;
    #[cfg(not(windows))]
    let mut file = File::open(path).ok()?;
    file.read_to_string(&mut value).ok()?;
    let mut fields = HashMap::new();
    for field in value.trim().split('\t') {
        let (key, value) = field.split_once('=')?;
        fields.insert(key, value);
    }
    Some(WriterOwner {
        token: fields.get("token")?.to_string(),
        pid: fields.get("pid")?.parse().ok()?,
        started_epoch_ms: fields.get("started_epoch_ms")?.parse().ok()?,
        version: fields.get("version")?.to_string(),
        intent: fields.get("intent")?.to_string(),
    })
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
    let nonce = AUTHORITY_NONCE.fetch_add(1, Ordering::Relaxed);
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
    value.replace(['\t', '\r', '\n'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_record_names_are_scoped_to_the_process_run() {
        let path = Path::new("writer.owner");
        let temporary = temporary_record_path(path, path.file_name().unwrap(), 17);
        let name = temporary.file_name().unwrap().to_string_lossy();

        assert!(name.contains(crate::runtime_identity::process_run_id()));
        assert!(name.ends_with(".17.tmp"));
    }
}
