//! No-follow filesystem fences for orphan-store collection.
//!
//! A collection finding crosses an inspection-to-apply boundary. These
//! identities keep that boundary fail-closed: a path replacement, symlink, or
//! changed child bytes is never treated as the censused store.

use std::ffi::{OsStr, OsString};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, ambient_authority};
use cap_std::fs::{Dir, OpenOptions};
use sha2::{Digest, Sha256};

use super::{CollectionControl, CollectionFailureKind};

/// Stable identity of an opened directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreRootIdentity {
    pub device: u64,
    pub inode: u64,
}

/// Stable identity and exact bytes of one regular payload file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreFileIdentity {
    pub device: u64,
    pub inode: u64,
    pub size_bytes: u64,
    pub sha256: [u8; 32],
}

/// The kind and identity of one no-follow descendant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreContentEntryKind {
    Directory(StoreRootIdentity),
    File(StoreFileIdentity),
}

/// One descendant in a store's exact no-follow inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreContentEntry {
    pub relative_path: PathBuf,
    pub kind: StoreContentEntryKind,
}

/// The opened root and every regular descendant observed during census.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreContentInventory {
    pub root: StoreRootIdentity,
    pub entries: Vec<StoreContentEntry>,
}

/// Exact content identity carried from census to post-quarantine verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreContentFence {
    Missing,
    Present(StoreContentInventory),
    Unverifiable,
}

/// A cheap root-generation fence used before expensive inspection and apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreDirectoryFence {
    Missing,
    Present { root: StoreRootIdentity },
    Unverifiable,
}

/// Capability handles for one store leaf rooted under its profile.
pub(super) struct StoreDirectoryCapability {
    pub(super) root: Dir,
    pub(super) parent: Dir,
    pub(super) leaf_name: OsString,
}

pub(super) fn profile_relative_store_path(
    profile_root: &Path,
    data_root: &Path,
) -> Result<PathBuf, CollectionFailureKind> {
    let relative = data_root
        .strip_prefix(profile_root)
        .map_err(|_| CollectionFailureKind::OutsideProfile)?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(CollectionFailureKind::OutsideProfile);
    }
    Ok(relative.to_path_buf())
}

/// Opens the profile-relative store parent by component without following a
/// symlink. The leaf is intentionally not opened by this helper so callers can
/// distinguish an absent store from an unsafe or unreadable replacement.
pub(super) fn open_store_parent_nofollow(
    profile_root: &Path,
    data_root: &Path,
) -> Result<StoreDirectoryCapability, CollectionFailureKind> {
    let relative = profile_relative_store_path(profile_root, data_root)?;
    let components = relative.components().collect::<Vec<_>>();
    let Some(Component::Normal(leaf_name)) = components.last() else {
        return Err(CollectionFailureKind::OutsideProfile);
    };
    let metadata = std::fs::symlink_metadata(profile_root)
        .map_err(|_| CollectionFailureKind::InspectFailed)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(CollectionFailureKind::OutsideProfile);
    }
    let root = Dir::open_ambient_dir(profile_root, ambient_authority())
        .map_err(|_| CollectionFailureKind::InspectFailed)?;
    let mut parent = root
        .open_dir_nofollow(".")
        .map_err(|_| CollectionFailureKind::InspectFailed)?;
    for component in &components[..components.len().saturating_sub(1)] {
        let Component::Normal(component) = component else {
            return Err(CollectionFailureKind::OutsideProfile);
        };
        parent = parent
            .open_dir_nofollow(component)
            .map_err(|_| CollectionFailureKind::OutsideProfile)?;
    }
    Ok(StoreDirectoryCapability {
        root,
        parent,
        leaf_name: leaf_name.to_os_string(),
    })
}

/// Opens an existing store leaf through its already-verified parent.
pub(super) fn open_store_directory_nofollow(
    profile_root: &Path,
    data_root: &Path,
) -> Result<StoreDirectoryCapability, CollectionFailureKind> {
    let capability = open_store_parent_nofollow(profile_root, data_root)?;
    match capability.parent.open_dir_nofollow(&capability.leaf_name) {
        Ok(root) => Ok(StoreDirectoryCapability {
            root,
            parent: capability.parent,
            leaf_name: capability.leaf_name,
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Err(CollectionFailureKind::PayloadChanged)
        }
        Err(_) => Err(classify_unreadable_store_leaf(
            &capability.parent,
            &capability.leaf_name,
        )),
    }
}

pub(super) fn capture_store_directory_fence(
    profile_root: &Path,
    data_root: &Path,
) -> Result<StoreDirectoryFence, CollectionFailureKind> {
    let capability = open_store_parent_nofollow(profile_root, data_root)?;
    match capability.parent.open_dir_nofollow(&capability.leaf_name) {
        Ok(root) => store_root_identity(&root)
            .map(|root| StoreDirectoryFence::Present { root })
            .map_err(|_| CollectionFailureKind::InspectFailed),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(StoreDirectoryFence::Missing),
        Err(_) => Err(classify_unreadable_store_leaf(
            &capability.parent,
            &capability.leaf_name,
        )),
    }
}

pub(super) fn data_root_fence_matches(
    expected: &StoreDirectoryFence,
    profile_root: &Path,
    data_root: &Path,
) -> Result<bool, CollectionFailureKind> {
    if *expected == StoreDirectoryFence::Unverifiable {
        return Err(CollectionFailureKind::InspectFailed);
    }
    Ok(capture_store_directory_fence(profile_root, data_root)? == *expected)
}

pub(super) fn capture_store_content_fence(
    profile_root: &Path,
    data_root: &Path,
) -> Result<StoreContentFence, CollectionFailureKind> {
    capture_store_content_fence_impl(profile_root, data_root, None)
}

pub(super) fn capture_store_content_fence_controlled(
    profile_root: &Path,
    data_root: &Path,
    control: CollectionControl<'_>,
) -> Result<StoreContentFence, CollectionFailureKind> {
    capture_store_content_fence_impl(profile_root, data_root, Some(control))
}

#[hotpath::measure(label = "maintenance.orphan_stores.fence_content")]
fn capture_store_content_fence_impl(
    profile_root: &Path,
    data_root: &Path,
    control: Option<CollectionControl<'_>>,
) -> Result<StoreContentFence, CollectionFailureKind> {
    check_control(control)?;
    let capability = open_store_parent_nofollow(profile_root, data_root)?;
    let root = match capability.parent.open_dir_nofollow(&capability.leaf_name) {
        Ok(root) => root,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(StoreContentFence::Missing);
        }
        Err(_) => {
            return Err(classify_unreadable_store_leaf(
                &capability.parent,
                &capability.leaf_name,
            ));
        }
    };
    capture_store_content_fence_in_dir_controlled(&root, control)
        .map(StoreContentFence::Present)
        .map_err(|error| failure_from_io(error, control))
}

/// Captures an inventory from an already-open directory; quarantine uses this
/// after its same-parent rename so the proof applies to the moved bytes.
pub(super) fn capture_store_content_fence_in_dir_controlled(
    root: &Dir,
    control: Option<CollectionControl<'_>>,
) -> io::Result<StoreContentInventory> {
    check_control_io(control)?;
    let mut entries = Vec::new();
    capture_directory_entries(root, Path::new(""), &mut entries, control)?;
    entries.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(StoreContentInventory {
        root: store_root_identity(root)?,
        entries,
    })
}

fn capture_directory_entries(
    directory: &Dir,
    relative_parent: &Path,
    entries: &mut Vec<StoreContentEntry>,
    control: Option<CollectionControl<'_>>,
) -> io::Result<()> {
    check_control_io(control)?;
    for entry in directory.read_dir(".")? {
        check_control_io(control)?;
        let entry = entry?;
        let name = entry.file_name();
        let metadata = directory.symlink_metadata(&name)?;
        if metadata.file_type().is_symlink() {
            return Err(unsafe_store_entry_error());
        }
        let relative_path = relative_parent.join(&name);
        if metadata.is_dir() {
            let child = directory.open_dir_nofollow(&name)?;
            entries.push(StoreContentEntry {
                relative_path: relative_path.clone(),
                kind: StoreContentEntryKind::Directory(store_root_identity(&child)?),
            });
            capture_directory_entries(&child, &relative_path, entries, control)?;
        } else if metadata.is_file() {
            entries.push(StoreContentEntry {
                relative_path,
                kind: StoreContentEntryKind::File(capture_file_identity(
                    directory, &name, control,
                )?),
            });
        } else {
            return Err(unsafe_store_entry_error());
        }
    }
    Ok(())
}

fn capture_file_identity(
    parent: &Dir,
    name: &OsStr,
    control: Option<CollectionControl<'_>>,
) -> io::Result<StoreFileIdentity> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let mut file = parent.open_with(name, &options)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(unsafe_store_entry_error());
    }
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        check_control_io(control)?;
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let sha256: [u8; 32] = digest.finalize().into();
    let identity = capability_identity(&file, &metadata)?;
    Ok(StoreFileIdentity {
        device: identity.device,
        inode: identity.inode,
        size_bytes: metadata.len(),
        sha256,
    })
}

pub(crate) fn store_root_identity(directory: &Dir) -> io::Result<StoreRootIdentity> {
    let metadata = directory.metadata(".")?;
    capability_identity(directory, &metadata)
}

/// Filesystem identity of an already-open capability: the pair a rename
/// preserves and a replacement changes.
///
/// Unix reads `st_dev`/`st_ino` from the capability's metadata. Windows must
/// read the volume serial number and file index *by handle* — `cap_std`
/// exposes them on `Metadata` only under the nightly `windows_by_handle`
/// feature, so the stable build saw `None` and every fence capture failed with
/// `InspectFailed`, which left every unregistered store unverifiable and every
/// orphan-store sweep planning nothing.
#[cfg(unix)]
fn capability_identity<H>(
    _handle: &H,
    metadata: &cap_std::fs::Metadata,
) -> io::Result<StoreRootIdentity> {
    use cap_std::fs::MetadataExt;

    Ok(StoreRootIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(windows)]
fn capability_identity<H: std::os::windows::io::AsRawHandle>(
    handle: &H,
    _metadata: &cap_std::fs::Metadata,
) -> io::Result<StoreRootIdentity> {
    let information = tracedecay_private_fs::windows_file::information(handle)?;
    Ok(StoreRootIdentity {
        device: u64::from(information.volume_serial_number),
        inode: information.file_index,
    })
}

#[cfg(not(any(unix, windows)))]
fn capability_identity<H>(
    _handle: &H,
    _metadata: &cap_std::fs::Metadata,
) -> io::Result<StoreRootIdentity> {
    Err(io::Error::other(
        "orphan-store filesystem identity is unsupported",
    ))
}

/// A leaf that is not a real directory at the physical profile-relative path
/// is outside the store authority, even when a symlink target resolves back
/// inside the profile. Intermediate `open_dir_nofollow` failures already use
/// this classification; the leaf must match.
fn classify_unreadable_store_leaf(parent: &Dir, leaf_name: &OsStr) -> CollectionFailureKind {
    match parent.symlink_metadata(leaf_name) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            CollectionFailureKind::OutsideProfile
        }
        _ => CollectionFailureKind::InspectFailed,
    }
}

fn check_control(control: Option<CollectionControl<'_>>) -> Result<(), CollectionFailureKind> {
    if control.is_some_and(|control| control.completion().is_some()) {
        return Err(CollectionFailureKind::Cancelled);
    }
    Ok(())
}

fn check_control_io(control: Option<CollectionControl<'_>>) -> io::Result<()> {
    check_control(control).map_err(|_| {
        io::Error::new(
            io::ErrorKind::Interrupted,
            "orphan-store content census interrupted",
        )
    })
}

fn failure_from_io(
    error: io::Error,
    control: Option<CollectionControl<'_>>,
) -> CollectionFailureKind {
    if error.kind() == io::ErrorKind::Interrupted
        || control.is_some_and(|control| control.completion().is_some())
    {
        CollectionFailureKind::Cancelled
    } else {
        CollectionFailureKind::InspectFailed
    }
}

fn unsafe_store_entry_error() -> io::Error {
    io::Error::other("orphan-store content inventory encountered an unsafe entry")
}
