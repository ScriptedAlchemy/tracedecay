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

pub(super) fn database_profile_root(database_path: &Path, fallback_parent: &Path) -> PathBuf {
    profile_project_root(database_path)
        .unwrap_or_else(|| database_path.parent().unwrap_or(fallback_parent))
        .to_path_buf()
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
