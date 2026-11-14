//! Resolve a tracked source alias using the parent Git index and live worktree.

use std::borrow::Cow;
use std::io;
use std::path::{Component, Path};

use gix::bstr::ByteSlice;
use gix::index::entry::{Mode, Stage};
use tracedecay_runtime_core::git_repository::{RepositoryLinkEntry, resolve_repository_link};

use super::{is_source_symlink, nested_repository_marker, submodules::is_source_location};

/// Ordinary files keep their original path. A live parent alias may only read
/// a tracked regular file through tracked links and parent-owned directories.
pub fn resolve_parent_source<'a>(
    root: &Path,
    relative: &'a Path,
) -> io::Result<Option<Cow<'a, Path>>> {
    if !is_source_symlink(root, relative)? {
        return Ok(Some(Cow::Borrowed(relative)));
    }
    if !nested_repository_marker(root)? {
        return Ok(None);
    }
    let repo = tracedecay_runtime_core::git_open::open(root).map_err(io::Error::other)?;
    let index = repo.index_or_empty().map_err(io::Error::other)?;
    let head = repo.head().map_err(io::Error::other)?;
    let tree = if head.id().is_some() {
        Some(repo.head_tree().map_err(io::Error::other)?)
    } else {
        None
    };
    let alias = gix::path::into_bstr(relative).map_err(io::Error::other)?;
    let alias = gix::path::to_unix_separators_on_windows(alias);
    if !index
        .entry_by_path_and_stage(&alias, Stage::Unconflicted)
        .is_some_and(|entry| entry.mode == Mode::SYMLINK)
    {
        return Ok(None);
    }
    resolve_repository_link(b"", &alias, |components| {
        let bytes = components.join(&b'/');
        let path = gix::path::from_byte_slice(&bytes).map_err(io::Error::other)?;
        if !is_source_location(path) {
            return Ok(None);
        }
        let absolute = root.join(path);
        let metadata = match absolute.symlink_metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let indexed = index.entry_by_path_and_stage(bytes.as_bstr(), Stage::Unconflicted);
        if metadata.is_dir() {
            // A gitlink staged for removal still bounds the old checkout even
            // after its marker disappears. Check HEAD before descending.
            let head_entry = tree
                .as_ref()
                .map(|tree| tree.lookup_entry(components.iter().map(Vec::as_slice)))
                .transpose()
                .map_err(io::Error::other)?
                .flatten();
            if index.entry_by_path(bytes.as_bstr()).is_some()
                || head_entry.is_some_and(|entry| entry.mode().is_commit())
                || nested_repository_marker(&absolute)?
            {
                return Ok(None);
            }
            return Ok(Some(RepositoryLinkEntry::Directory));
        }
        let Some(entry) = indexed else {
            return Ok(None);
        };
        if metadata.file_type().is_symlink() && entry.mode == Mode::SYMLINK {
            let target = std::fs::read_link(&absolute)?;
            if target
                .components()
                .any(|component| matches!(component, Component::Prefix(_) | Component::RootDir))
            {
                return Ok(None);
            }
            let target = gix::path::into_bstr(target).map_err(io::Error::other)?;
            return Ok(Some(RepositoryLinkEntry::Link(
                gix::path::to_unix_separators_on_windows(target).to_vec(),
            )));
        }
        Ok((metadata.is_file()
            && entry
                .mode
                .to_tree_entry_mode()
                .is_some_and(|mode| mode.is_blob()))
        .then(|| RepositoryLinkEntry::File(Cow::Owned(path.to_owned()))))
    })
}
