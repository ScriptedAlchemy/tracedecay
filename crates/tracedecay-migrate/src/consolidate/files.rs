use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::{config_error, io_error};
use crate::errors::Result;
use crate::storage;

pub(super) fn relative_file_map(root: &Path) -> Result<BTreeMap<PathBuf, PathBuf>> {
    let mut files = BTreeMap::new();
    collect_files(root, root, &mut files)?;
    Ok(files)
}

fn collect_files(
    root: &Path,
    current: &Path,
    files: &mut BTreeMap<PathBuf, PathBuf>,
) -> Result<()> {
    let mut entries = fs::read_dir(current)
        .map_err(io_error)?
        .collect::<io::Result<Vec<_>>>()
        .map_err(io_error)?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let metadata = path.symlink_metadata().map_err(io_error)?;
        if metadata.file_type().is_symlink() {
            return Err(config_error(format!(
                "profile store artifact '{}' is a symlink; refusing unsafe traversal",
                path.display()
            )));
        }
        if metadata.is_dir() {
            collect_files(root, &path, files)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|error| config_error(error.to_string()))?
                .to_path_buf();
            files.insert(relative, path);
        }
    }
    Ok(())
}

pub(super) fn tree_stats(root: &Path) -> Result<(usize, u64)> {
    let files = relative_file_map(root)?;
    let mut bytes = 0_u64;
    for path in files.values() {
        bytes = bytes.saturating_add(fs::metadata(path).map_err(io_error)?.len());
    }
    Ok((files.len(), bytes))
}

pub(super) fn copy_file_exact(source: &Path, target: &Path) -> Result<()> {
    if target.exists() {
        if file_digest(source)? == file_digest(target)? {
            sync_file_and_parent(target)?;
            return Ok(());
        }
        return Err(config_error(format!(
            "existing migration artifact '{}' differs from source '{}'",
            target.display(),
            source.display()
        )));
    }
    copy_file_atomic(source, target)?;
    if file_digest(source)? != file_digest(target)? {
        let _ = fs::remove_file(target);
        return Err(config_error(format!(
            "copied migration artifact '{}' failed checksum verification against '{}'",
            target.display(),
            source.display()
        )));
    }
    Ok(())
}

pub(super) fn copy_file_atomic(source: &Path, target: &Path) -> Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| config_error("artifact target has no parent"))?;
    fs::create_dir_all(parent).map_err(io_error)?;
    let temp = target.with_extension(format!("tmp-{}", std::process::id()));
    match fs::remove_file(&temp) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error(error)),
    }
    let mut input = File::open(source).map_err(|error| {
        config_error(format!(
            "failed to open migration source '{}' for copy to '{}': {error}",
            source.display(),
            target.display()
        ))
    })?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|error| {
            config_error(format!(
                "failed to create migration temp '{}' while copying '{}' to '{}': {error}",
                temp.display(),
                source.display(),
                target.display()
            ))
        })?;
    io::copy(&mut input, &mut output).map_err(|error| {
        config_error(format!(
            "failed to copy migration source '{}' to temp '{}' for '{}': {error}",
            source.display(),
            temp.display(),
            target.display()
        ))
    })?;
    fs::set_permissions(&temp, fs::metadata(source).map_err(io_error)?.permissions())
        .map_err(io_error)?;
    output.sync_all().map_err(io_error)?;
    drop(output);
    fs::rename(&temp, target).map_err(io_error)?;
    sync_parent_directory(parent)?;
    Ok(())
}

fn sync_file_and_parent(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(io_error)?;
    #[cfg(not(unix))]
    let _ = path;
    let parent = path
        .parent()
        .ok_or_else(|| config_error("artifact target has no parent"))?;
    sync_parent_directory(parent)
}

#[cfg(unix)]
pub(super) fn sync_parent_directory(parent: &Path) -> Result<()> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(io_error)
}

#[cfg(not(unix))]
pub(super) fn sync_parent_directory(_parent: &Path) -> Result<()> {
    // Windows directory handles commonly reject sync_all with AccessDenied.
    // File data is still flushed before the atomic rename.
    Ok(())
}

pub(super) fn copy_sqlite_family_exact(source: &Path, target: &Path) -> Result<()> {
    copy_file_exact(source, target)?;
    for suffix in ["-wal", "-shm"] {
        let source_sidecar = sqlite_sidecar(source, suffix);
        if source_sidecar.is_file() {
            copy_file_exact(&source_sidecar, &sqlite_sidecar(target, suffix))?;
        }
    }
    Ok(())
}

pub(super) fn sqlite_sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

pub(super) fn remove_runtime_files(root: &Path) -> Result<()> {
    for relative in ["sync.lock", ".branch-add.lock", ".dirty"] {
        let path = root.join(relative);
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(error)),
        }
    }
    Ok(())
}

pub(super) fn excluded_source_artifact(relative: &Path) -> bool {
    let value = relative.to_string_lossy();
    value == crate::config::DB_FILENAME
        || value == storage::SESSIONS_DB_FILENAME
        || value == storage::BRANCH_META_FILENAME
        || value == storage::STORE_MANIFEST_FILENAME
        || value.starts_with("branches/")
        || value.ends_with("-wal")
        || value.ends_with("-shm")
        || is_runtime_lock(relative)
}

pub(super) fn is_runtime_lock(relative: &Path) -> bool {
    is_coordination_lock(relative)
        || relative.file_name().and_then(|value| value.to_str()) == Some(".dirty")
}

pub(super) fn is_coordination_lock(relative: &Path) -> bool {
    matches!(
        relative.file_name().and_then(|value| value.to_str()),
        Some("sync.lock" | ".branch-add.lock")
    )
}

pub(super) fn is_sqlite_sidecar(relative: &Path) -> bool {
    relative
        .file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.ends_with("-shm") || value.ends_with("-wal"))
}

pub(super) fn is_sqlite_database(relative: &Path) -> bool {
    relative
        .extension()
        .is_some_and(|value| value.eq_ignore_ascii_case("db"))
}

pub(super) fn is_reference_artifact(relative: &Path) -> bool {
    relative.starts_with("lcm-payloads") || relative.starts_with("response-handles")
}

pub(super) fn file_digest(path: &Path) -> Result<[u8; 32]> {
    let mut file = File::open(path).map_err(io_error)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let read = file.read(&mut buffer).map_err(io_error)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(hash.finalize().into())
}
