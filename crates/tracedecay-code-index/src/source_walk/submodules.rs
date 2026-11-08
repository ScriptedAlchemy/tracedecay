//! A parent gitlink names a commit, not the worktree that may supply its bytes.
//! Bind the registration, object store, and checkout before crossing that boundary.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use gix::bstr::ByteSlice;

use super::{SourceWalkError, has_nested_repository_ancestor};

fn failure(error: impl std::fmt::Display) -> SourceWalkError {
    SourceWalkError {
        glob: String::new(),
        message: format!("cannot inspect registered submodule: {error}"),
    }
}

#[derive(Clone, Debug)]
pub struct AuthenticatedSubmodule {
    pub worktree: PathBuf,
    pub git_dir: PathBuf,
    pub pin: gix::ObjectId,
}

fn normal_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

/// Administrative data is outside every parent source surface, even with an explicit glob.
pub fn is_source_location(relative: &Path) -> bool {
    normal_relative(relative)
        && !relative.components().any(|component| {
            component.as_os_str() == ".git" || component.as_os_str() == ".tracedecay"
        })
}

fn registrations(bytes: &[u8]) -> Result<BTreeMap<String, PathBuf>, SourceWalkError> {
    let modules = gix::submodule::File::from_bytes(bytes, None, &gix::config::File::default())
        .map_err(failure)?;
    let mut registrations = BTreeMap::new();
    for name in modules.names() {
        let name = gix::validate::submodule::name(name).map_err(failure)?;
        let path = modules.path(name).map_err(failure)?;
        let path = path.to_str().map_err(failure)?;
        if !is_source_location(Path::new(path)) {
            continue;
        }
        let name = PathBuf::from(name.to_str().map_err(failure)?);
        if !normal_relative(&name)
            || !normal_relative(Path::new(path))
            || registrations.insert(path.to_owned(), name).is_some()
        {
            return Err(failure("ambiguous or invalid .gitmodules path"));
        }
    }
    Ok(registrations)
}

fn module_stores<'a>(
    parent: &'a gix::Repository,
    name: &'a Path,
) -> impl Iterator<Item = PathBuf> + 'a {
    [parent.git_dir(), parent.common_dir()]
        .into_iter()
        .filter_map(move |directory| {
            let expected = directory.canonicalize().ok()?.join("modules").join(name);
            (expected.canonicalize().ok()? == expected).then_some(expected)
        })
}

fn open_pinned_store(expected: &Path, pin: gix::ObjectId) -> Option<gix::Repository> {
    let child = tracedecay_runtime_core::git_open::open(expected).ok()?;
    if child.git_dir().canonicalize().ok()? != expected
        || child.common_dir().canonicalize().ok()? != expected
    {
        return None;
    }
    child.find_commit(pin).ok()?;
    Some(child)
}

fn bound_checkout(
    parent: &gix::Repository,
    path: &str,
    name: &Path,
    pin: gix::ObjectId,
) -> Option<AuthenticatedSubmodule> {
    let root = parent.workdir()?.canonicalize().ok()?;
    let path = Path::new(path);
    if has_nested_repository_ancestor(&root, path).ok()? {
        return None;
    }
    let worktree = root.join(path);
    let marker = worktree.join(".git");
    if worktree.canonicalize().ok()? != worktree || !marker.symlink_metadata().ok()?.is_file() {
        return None;
    }
    let git_dir = gix::discover::path::from_gitdir_file(&marker)
        .ok()?
        .canonicalize()
        .ok()?;
    // Git initializes children of a linked parent under that worktree's
    // administrative directory. Check the name-derived, canonical location
    // before opening any repository named by the checkout's marker.
    let expected = module_stores(parent, name).find(|candidate| *candidate == git_dir)?;
    let child = open_pinned_store(&expected, pin)?;
    if child.workdir()?.canonicalize().ok()? != worktree {
        return None;
    }
    Some(AuthenticatedSubmodule {
        worktree,
        git_dir: expected,
        pin,
    })
}

pub fn live_submodules(
    parent: &gix::Repository,
) -> Result<BTreeMap<String, AuthenticatedSubmodule>, SourceWalkError> {
    let mut children = BTreeMap::new();
    let index = parent.index_or_empty().map_err(failure)?;
    let stage = gix::index::entry::Stage::Unconflicted;
    let Some(modules) = index.entry_by_path_and_stage(".gitmodules".into(), stage) else {
        return Ok(children);
    };
    if !modules
        .mode
        .to_tree_entry_mode()
        .is_some_and(|mode| mode.is_blob())
    {
        return Ok(children);
    }
    let blob = parent.find_blob(modules.id).map_err(failure)?;
    let registered = registrations(&blob.data)?;
    // An unstaged deletion or replacement of the registration cannot authorize a checkout.
    let Some(live_registration) = parent.open_modules_file().map_err(failure)? else {
        return Ok(children);
    };
    for (path, name) in registered {
        let Some(entry) = index.entry_by_path_and_stage(path.as_bytes().as_bstr(), stage) else {
            continue;
        };
        if !entry.mode.is_submodule() {
            continue;
        }
        if !live_registration
            .path(name.as_os_str().as_encoded_bytes().as_bstr())
            .is_ok_and(|current| current.as_slice() == path.as_bytes())
        {
            continue;
        }
        if let Some(child) = bound_checkout(parent, &path, &name, entry.id) {
            children.insert(path, child);
        }
    }
    Ok(children)
}

/// Only the selected parent tree may supply historical registrations and pins.
pub fn pinned_submodules(
    parent: &gix::Repository,
    pins: &tracedecay_runtime_core::git_repository::GitTreeSubmodulePins,
) -> Result<BTreeMap<String, (gix::Repository, gix::ObjectId)>, SourceWalkError> {
    let mut children = BTreeMap::new();
    if pins.gitlinks.is_empty() {
        return Ok(children);
    }
    let registrations = registrations(&pins.registration)?;
    for (path, pin) in &pins.gitlinks {
        let Some(name) = registrations.get(path) else {
            continue;
        };
        // The checkout can move or be deinitialized after the parent revision.
        // Its current bytes and .git marker are never historical authorities.
        let repo = module_stores(parent, name)
            .find_map(|directory| open_pinned_store(&directory, *pin))
            .ok_or_else(|| {
                failure(format!(
                    "registered object store or pinned commit unavailable for {path}"
                ))
            })?;
        children.insert(path.clone(), (repo, *pin));
    }
    Ok(children)
}
