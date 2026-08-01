use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::errors::Result;

use super::access_io_error;

pub(super) fn canonical_profile_root(profile_root: &Path) -> Result<PathBuf> {
    let absolute = if profile_root.is_absolute() {
        profile_root.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| access_io_error("resolve profile", profile_root, &error))?
            .join(profile_root)
    };
    Ok(platform_identity_key(
        &absolute.canonicalize().unwrap_or(absolute),
    ))
}

pub(super) fn platform_identity_key(path: &Path) -> PathBuf {
    crate::lifecycle_lease::canonical_or_original(path)
}

#[cfg_attr(
    any(windows, target_os = "macos"),
    allow(
        clippy::unnecessary_wraps,
        reason = "all platforms share the optional case-folded bootstrap-key contract"
    )
)]
pub(super) fn bootstrap_database_key(parent: &Path, file_name: &OsStr) -> Option<PathBuf> {
    #[cfg(any(windows, target_os = "macos"))]
    {
        Some(parent.join(file_name.to_string_lossy().to_lowercase()))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (parent, file_name);
        None
    }
}

pub(super) fn database_lock_root(database_path: &Path, fallback_parent: &Path) -> PathBuf {
    if let Some(profile_root) = profile_project_root(database_path) {
        return profile_root.join(".tracedecay-database-locks");
    }
    database_path
        .parent()
        .unwrap_or(fallback_parent)
        .join(".tracedecay-database-locks")
}

fn profile_project_root(database_path: &Path) -> Option<&Path> {
    let parent = database_path.parent()?;
    // Branch graphs and staged consolidation inputs live one level below
    // their project data root and share its profile authority scope.
    let data_root = if parent
        .file_name()
        .is_some_and(|name| name == "branches" || name == ".consolidation-input")
    {
        parent.parent()?
    } else {
        parent
    };
    let projects_root = data_root.parent()?;
    if projects_root
        .file_name()
        .is_some_and(|name| name == "projects")
    {
        projects_root.parent()
    } else {
        None
    }
}

pub(super) fn is_legacy_repository_database(database_path: &Path) -> bool {
    let Some(parent) = database_path.parent() else {
        return false;
    };
    let is_branch_database = parent.file_name().is_some_and(|name| name == "branches");
    if !is_branch_database
        && database_path.file_name().is_some_and(|name| {
            name == "global.db" || name == "user-memory.db" || name == "user-sessions.db"
        })
    {
        return false;
    }
    let data_root = if is_branch_database {
        let Some(data_root) = parent.parent() else {
            return false;
        };
        data_root
    } else {
        parent
    };
    data_root
        .file_name()
        .is_some_and(|name| name == ".tracedecay")
}

pub(super) fn stable_path_hash(path: &Path) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in native_path_bytes(path) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

pub(super) fn stable_path_set_hash<'a>(paths: impl IntoIterator<Item = &'a Path>) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for path in paths {
        for byte in native_path_bytes(path) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        hash ^= u64::from(b'\0');
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

#[cfg(any(unix, windows))]
fn native_path_bytes(path: &Path) -> Vec<u8> {
    crate::os_str_bytes::native_os_str_bytes(path.as_os_str())
}

#[cfg(not(any(unix, windows)))]
fn native_path_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().into_owned().into_bytes()
}
