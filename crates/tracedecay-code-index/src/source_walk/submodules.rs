//! A parent gitlink names a commit, not the worktree that may supply its bytes.
//! Bind the registration, object store, and checkout before crossing that boundary.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
#[cfg(unix)]
use std::sync::{Arc, Mutex, PoisonError};

use gix::bstr::ByteSlice;
use ignore::{DirEntry, Error as WalkError, Walk};

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
    parent_root: PathBuf,
    module_path: String,
    module_name: PathBuf,
    staged_modules_id: gix::ObjectId,
    #[cfg(unix)]
    parent_git_dir: PathBuf,
    #[cfg(unix)]
    parent_common_dir: PathBuf,
    #[cfg(unix)]
    verified: Arc<Mutex<Option<BindingWitness>>>,
}

#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
struct FileWitness {
    named: (u64, u64, u32, u64, i64, i64, i64, i64),
    target: Option<(u64, u64, u32, u64, i64, i64, i64, i64)>,
}

#[cfg(unix)]
impl FileWitness {
    fn identity(metadata: &std::fs::Metadata) -> (u64, u64, u32, u64, i64, i64, i64, i64) {
        (
            metadata.dev(),
            metadata.ino(),
            metadata.mode(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        )
    }

    fn at(path: &Path) -> io::Result<Option<Self>> {
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let target = if metadata.file_type().is_symlink() {
            Some(Self::identity(&std::fs::metadata(path)?))
        } else {
            None
        };
        Ok(Some(Self {
            named: Self::identity(&metadata),
            target,
        }))
    }
}

#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
struct BindingWitness {
    paths: BTreeMap<PathBuf, Option<FileWitness>>,
}

#[cfg(unix)]
impl BindingWitness {
    fn unchanged(&self) -> io::Result<bool> {
        for (path, witness) in &self.paths {
            if FileWitness::at(path)? != *witness {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
thread_local! {
    static FULL_BINDING_VALIDATIONS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

pub(super) struct ChildSources {
    pub files: BTreeSet<String>,
    pub gitlinks: BTreeSet<String>,
}

impl AuthenticatedSubmodule {
    #[cfg(unix)]
    fn binding_witness(&self) -> io::Result<Option<BindingWitness>> {
        let objects = self.git_dir.join("objects");
        // Object alternates may change outside this parent-owned checkout.
        // Unobservable external authority cannot be covered by a local stamp.
        match std::fs::symlink_metadata(objects.join("info/alternates")) {
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let pin = self.pin.to_string();
        let mut paths = vec![
            self.parent_root.clone(),
            self.parent_root.join(".git"),
            self.parent_root.join(".gitmodules"),
            self.parent_git_dir.clone(),
            self.parent_git_dir.join("index"),
            self.parent_git_dir.join("config"),
            self.parent_git_dir.join("commondir"),
            self.parent_common_dir.clone(),
            self.parent_common_dir.join("config"),
            self.parent_common_dir.join("modules"),
            self.git_dir.clone(),
            self.git_dir.join("config"),
            self.git_dir.join("config.worktree"),
            objects.clone(),
            objects.join("pack"),
            objects.join("info/alternates"),
            objects.join(&pin[..2]),
            objects.join(&pin[..2]).join(&pin[2..]),
            self.worktree.clone(),
            self.worktree.join(".git"),
        ];
        // Git split indexes carry staged gitlinks in sharedindex.* files.
        // Their contents can change without changing the main index or its
        // parent directory. Witness each file in both administrative roots;
        // the directory stamps above detect newly added or removed files.
        for admin in [&self.parent_git_dir, &self.parent_common_dir] {
            for entry in std::fs::read_dir(admin)? {
                let entry = entry?;
                if entry.file_name().to_string_lossy().starts_with("sharedindex.") {
                    paths.push(entry.path());
                }
            }
        }
        // The pack directory stamp detects additions and deletions. Track
        // existing pack members as well to reject an in-place replacement.
        match std::fs::read_dir(objects.join("pack")) {
            Ok(entries) => {
                for entry in entries {
                    paths.push(entry?.path());
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let mut observed = BTreeMap::new();
        for path in paths {
            observed.insert(path.clone(), FileWitness::at(&path)?);
        }
        Ok(Some(BindingWitness { paths: observed }))
    }

    fn parent_registration_is_live(&self) -> io::Result<bool> {
        // A retained child must preserve the parent's exact staged authority.
        // An unreadable Git authority is not a revoked or absent source.
        let parent = tracedecay_runtime_core::git_open::open(&self.parent_root)
            .map_err(|error| io::Error::other(error.to_string()))?;
        if parent
            .workdir()
            .and_then(|root| root.canonicalize().ok())
            .as_ref()
            != Some(&self.parent_root)
        {
            return Ok(false);
        }
        if !module_stores(&parent, &self.module_name).any(|store| store == self.git_dir) {
            return Ok(false);
        }
        let index = parent
            .index_or_empty()
            .map_err(|error| io::Error::other(error.to_string()))?;
        let stage = gix::index::entry::Stage::Unconflicted;
        let Some(modules) = index.entry_by_path_and_stage(".gitmodules".into(), stage) else {
            return Ok(false);
        };
        if modules.id != self.staged_modules_id
            || !modules
                .mode
                .to_tree_entry_mode()
                .is_some_and(|mode| mode.is_blob())
        {
            return Ok(false);
        }
        let Some(gitlink) =
            index.entry_by_path_and_stage(self.module_path.as_bytes().as_bstr(), stage)
        else {
            return Ok(false);
        };
        if !gitlink.mode.is_submodule() || gitlink.id != self.pin {
            return Ok(false);
        }
        let Some(working) = parent
            .open_modules_file()
            .map_err(|error| io::Error::other(error.to_string()))?
        else {
            return Ok(false);
        };
        let registered_path = working
            .path(self.module_name.as_os_str().as_encoded_bytes().as_bstr())
            .map_err(|error| io::Error::other(error.to_string()))?;
        Ok(registered_path.as_slice() == self.module_path.as_bytes())
    }

    pub fn checkout_is_live(&self) -> io::Result<bool> {
        #[cfg(unix)]
        {
            let cached = self
                .verified
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            if let Some(witness) = cached
                && witness.unchanged()?
            {
                return Ok(true);
            }
            // Invalidate BEFORE touching Git. A read failure must never retain
            // a previous successful authority for a later caller.
            *self.verified.lock().unwrap_or_else(PoisonError::into_inner) = None;
        }
        #[cfg(unix)]
        let before = self.binding_witness().ok().flatten();
        let valid = self.verify_checkout_binding()?;
        #[cfg(unix)]
        if valid {
            let after = self.binding_witness().ok().flatten();
            if before.is_some() && before == after {
                *self.verified.lock().unwrap_or_else(PoisonError::into_inner) = after;
            }
        }
        Ok(valid)
    }

    fn verify_checkout_binding(&self) -> io::Result<bool> {
        #[cfg(test)]
        FULL_BINDING_VALIDATIONS.with(|count| count.set(count.get() + 1));
        let marker = self.worktree.join(".git");
        let checkout_matches = self.worktree.canonicalize().ok().as_ref() == Some(&self.worktree)
            && marker
                .symlink_metadata()
                .is_ok_and(|metadata| metadata.is_file())
            && gix::discover::path::from_gitdir_file(&marker)
                .ok()
                .and_then(|target| target.canonicalize().ok())
                .as_ref()
                == Some(&self.git_dir);
        if !checkout_matches || !self.parent_registration_is_live()? {
            return Ok(false);
        }
        // Rechecking the pinned child also rejects core.worktree redirection.
        Ok(open_pinned_store(&self.git_dir, self.pin)
            .and_then(|child| child.workdir()?.canonicalize().ok())
            .as_ref()
            == Some(&self.worktree))
    }

    pub(super) fn sources(&self) -> Result<ChildSources, SourceWalkError> {
        let repository = tracedecay_runtime_core::git_open::open(&self.git_dir).map_err(failure)?;
        let inventory =
            super::classification::WorktreeChangeClassificationV1::classify(&repository)
                .map_err(failure)?;
        Ok(ChildSources {
            files: inventory
                .regular_child_files(&self.worktree)
                .map_err(failure)?,
            gitlinks: inventory.gitlink_paths().clone(),
        })
    }
}

/// A lazy walk that cannot outlive its authenticated child checkout binding.
/// The ordinary parent walk has no child binding and retains its own policy.
pub struct CheckoutWalk {
    walk: Option<Walk>,
    child: Option<AuthenticatedSubmodule>,
}

impl CheckoutWalk {
    pub(super) fn new(walk: Walk, child: Option<AuthenticatedSubmodule>) -> Self {
        Self {
            walk: Some(walk),
            child,
        }
    }
}

impl Iterator for CheckoutWalk {
    type Item = Result<DirEntry, WalkError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.walk.as_ref()?;
        if let Some(child) = &self.child {
            let refusal = match child.checkout_is_live() {
                Ok(true) => None,
                Ok(false) => Some(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "authenticated submodule checkout binding changed during source walk",
                )),
                Err(error) => Some(error),
            };
            if let Some(error) = refusal {
                // A failed Git read must not become a successful empty walk.
                self.walk = None;
                return Some(Err(WalkError::WithPath {
                    path: child.worktree.clone(),
                    err: Box::new(WalkError::Io(error)),
                }));
            }
        }

        let next = self.walk.as_mut()?.next();
        if next.is_none() {
            self.walk = None;
        }
        next
    }
}

pub(super) struct CheckoutBoundaries {
    pub gitlinks: BTreeSet<String>,
    pub children: BTreeMap<String, AuthenticatedSubmodule>,
}

impl CheckoutBoundaries {
    pub(super) fn open(root: &Path, include_submodules: bool) -> Result<Self, SourceWalkError> {
        Self::open_scoped(root, include_submodules, None)
    }

    pub(super) fn open_scoped(
        root: &Path,
        include_submodules: bool,
        scope: Option<&Path>,
    ) -> Result<Self, SourceWalkError> {
        let gitlinks = super::parent_gitlink_paths_scoped(root, scope)?;
        let children = if include_submodules && !gitlinks.is_empty() {
            let parent = tracedecay_runtime_core::git_open::open(root).map_err(failure)?;
            live_submodules(&parent)?
        } else {
            BTreeMap::new()
        };
        Ok(Self { gitlinks, children })
    }
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
    staged_modules_id: gix::ObjectId,
) -> Option<AuthenticatedSubmodule> {
    let root = parent.workdir()?.canonicalize().ok()?;
    let module_path = path.to_owned();
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
        parent_root: root,
        module_path,
        module_name: name.to_path_buf(),
        staged_modules_id,
        #[cfg(unix)]
        parent_git_dir: parent.git_dir().to_path_buf(),
        #[cfg(unix)]
        parent_common_dir: parent.common_dir().to_path_buf(),
        #[cfg(unix)]
        verified: Arc::new(Mutex::new(None)),
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
        if let Some(child) = bound_checkout(parent, &path, &name, entry.id, modules.id) {
            children.insert(path, child);
        }
    }
    Ok(children)
}

/// Batch the same Git inventory for foreground graph readers. A graph path can
/// outlive its checkout binding, even when a replacement file has equal bytes.
pub struct LiveSourceBoundaries {
    project_root: PathBuf,
    gitlinks: BTreeSet<String>,
    children: BTreeMap<String, (AuthenticatedSubmodule, BTreeSet<String>)>,
}

impl LiveSourceBoundaries {
    pub fn open(root: &Path, include_submodules: bool) -> Result<Self, SourceWalkError> {
        let boundaries = CheckoutBoundaries::open(root, include_submodules)?;
        let mut children = BTreeMap::new();
        for (prefix, child) in boundaries.children {
            let files = child.sources()?.files;
            children.insert(prefix, (child, files));
        }
        Ok(Self {
            project_root: root.to_owned(),
            gitlinks: boundaries.gitlinks,
            children,
        })
    }

    /// The owning checkout and its relative file path, after checking the
    /// current filesystem boundary before reading any bytes.
    pub fn resolve<'a, 'p>(
        &'a self,
        path: &'p Path,
    ) -> std::io::Result<Option<(&'a Path, Cow<'p, Path>)>> {
        if !is_source_location(path) {
            return Ok(None);
        }
        let mut root = self.project_root.as_path();
        let mut relative = path;
        for ancestor in path.ancestors().skip(1).filter_map(Path::to_str) {
            if !self.gitlinks.contains(ancestor) {
                continue;
            }
            let Some((child, files)) = self.children.get(ancestor) else {
                return Ok(None);
            };
            relative = path.strip_prefix(ancestor).map_err(std::io::Error::other)?;
            if !relative.to_str().is_some_and(|file| files.contains(file)) {
                return Ok(None);
            }
            if !child.checkout_is_live()? {
                return Ok(None);
            }
            root = &child.worktree;
            break;
        }
        if has_nested_repository_ancestor(root, relative)? {
            return Ok(None);
        }
        if root == self.project_root {
            Ok(super::resolve_parent_source(root, relative)?.map(|path| (root, path)))
        } else if super::is_source_symlink(root, relative)? {
            Ok(None)
        } else {
            Ok(Some((root, Cow::Borrowed(relative))))
        }
    }
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

#[cfg(test)]
mod tests;
