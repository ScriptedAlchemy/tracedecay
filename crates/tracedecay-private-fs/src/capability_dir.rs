//! Capability-relative durable directory primitives shared by the quarantine
//! and retirement authorities: atomic no-replace rename between already-open
//! parent capabilities, sibling replacement, directory metadata sync, recursive no-follow removal,
//! and create-or-open of one entry that survives the Darwin create race.
//! Every mutation is relative to an open `Dir` handle so a parent path
//! swapped for a symlink cannot redirect the operation.

use std::ffi::OsStr;
use std::io::{self, Read};
use std::path::{Component, Path};

#[cfg(not(windows))]
use cap_fs_ext::OpenOptionsMaybeDirExt;
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, File, OpenOptions};

/// How many times a create-or-open that reports the entry missing is asked
/// again before the answer is believed. xnu bounds its own retry of the same
/// condition at ten; downstream measurements found one extra look always
/// sufficed, so this is generous without letting a genuinely missing parent
/// spin.
const CREATE_RACE_LOOKS: usize = 8;

/// Creates or opens `name` beneath an already-open directory capability.
///
/// `options` must request `create(true)` without `create_new`: this is the
/// create-or-open form, whose two correct answers are "the entry was created"
/// and "the existing entry was opened". On macOS that form has a defect the
/// others do not. `openat(dirfd, name, O_CREAT)` without `O_EXCL` hands most
/// callers that lose the first-creation race of one name a spurious `ENOENT`
/// (xnu's `vn_open_auth` retries a create that failed with `EEXIST` as an
/// open, and the fallback is not atomic), even though the entry is present
/// the moment the error arrives. `O_CREAT|O_EXCL`, opens of an existing entry,
/// and absolute-path `open` are all correct, which is why the std-based
/// sidecar locks never see it and only the capability-relative lock and
/// ledger opens do, and those are exactly the files many writers create at
/// once.
///
/// A `NotFound` answer is therefore looked at again a bounded number of
/// times. A parent that is genuinely gone still reports `NotFound` after the
/// bound, so the caller's error mapping keeps working.
pub fn open_or_create_with(
    directory: &Dir,
    name: &OsStr,
    options: &OpenOptions,
) -> io::Result<File> {
    look_again_on_not_found(|| directory.open_with(name, options))
}

fn look_again_on_not_found<T>(mut attempt: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut looks = 0;
    loop {
        match attempt() {
            Err(error) if error.kind() == io::ErrorKind::NotFound && looks < CREATE_RACE_LOOKS => {
                looks += 1;
                std::thread::yield_now();
            }
            result => return result,
        }
    }
}

/// Atomically renames one directory entry between already-open parent
/// capabilities without allowing an occupied destination to be replaced.
/// Platforms without a suitable primitive fail closed: retaining bytes is
/// always preferable to risking a replacement.
pub fn rename_noreplace(
    from_parent: &Dir,
    from: &OsStr,
    to_parent: &Dir,
    to: &OsStr,
) -> io::Result<()> {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        use std::os::fd::AsRawFd;

        crate::rename_noreplace::rename_noreplace_at(
            from_parent.as_raw_fd(),
            from,
            to_parent.as_raw_fd(),
            to,
        )
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;

        crate::rename_noreplace::rename_noreplace_at(
            from_parent.as_raw_fd(),
            from,
            to_parent.as_raw_fd(),
            to,
        )
    }
    #[cfg(windows)]
    {
        let from = dir_entry_path(from_parent, from)?;
        let to = dir_entry_path(to_parent, to)?;
        crate::rename_noreplace::rename_noreplace_paths(&from, &to)
    }
    #[cfg(not(any(
        all(target_os = "linux", target_env = "gnu"),
        target_os = "macos",
        windows
    )))]
    {
        let _ = (from_parent, from, to_parent, to);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic no-replace rename is unavailable on this platform",
        ))
    }
}

#[cfg(windows)]
fn dir_entry_path(parent: &Dir, name: &OsStr) -> io::Result<std::path::PathBuf> {
    Ok(dir_path(parent)?.join(name))
}

/// Atomically replaces a sibling file beneath a held parent capability.
/// Windows uses the shared namespace-durable rename authority, including its
/// NTFS fallback for replacement while delete-sharing readers remain open.
pub fn replace_file(parent: &Dir, source: &OsStr, destination: &OsStr) -> io::Result<()> {
    for name in [source, destination] {
        let mut components = std::path::Path::new(name).components();
        if !matches!(components.next(), Some(std::path::Component::Normal(_)))
            || components.next().is_some()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "file replacement requires sibling entry names",
            ));
        }
    }
    #[cfg(windows)]
    {
        let source = dir_entry_path(parent, source)?;
        let destination = dir_entry_path(parent, destination)?;
        crate::windows::rename_paths_durable(
            &source,
            &destination,
            crate::windows::RenameMode::Replace,
        )
    }
    #[cfg(not(windows))]
    parent.rename(source, parent, destination)
}

/// Resolves an open directory capability back to its live filesystem path so
/// path-based Win32 primitives can address entries below it.
#[cfg(windows)]
fn dir_path(directory: &Dir) -> io::Result<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW,
    };

    let get_path = |buffer: *mut u16, length: u32| {
        // SAFETY: `buffer`/`length` describe a writable UTF-16 buffer (or a
        // zero-length probe) and the handle is an open directory capability.
        unsafe {
            GetFinalPathNameByHandleW(
                directory.as_raw_handle(),
                buffer,
                length,
                FILE_NAME_NORMALIZED,
            )
        }
    };
    let required = get_path(std::ptr::null_mut(), 0);
    if required == 0 {
        return Err(io::Error::last_os_error());
    }
    let capacity = required.checked_add(1).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "directory capability path length overflowed",
        )
    })?;
    let mut buffer = vec![0_u16; capacity as usize];
    let written = get_path(buffer.as_mut_ptr(), buffer.len() as u32);
    if written == 0 {
        return Err(io::Error::last_os_error());
    }
    if written as usize >= buffer.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "directory capability path changed while resolving it",
        ));
    }
    buffer.truncate(written as usize);
    Ok(std::path::PathBuf::from(std::ffi::OsString::from_wide(
        &buffer,
    )))
}

/// Flushes a directory capability's metadata so a preceding create, rename,
/// or unlink beneath it is durable.
pub fn sync_directory(directory: &Dir) -> io::Result<()> {
    #[cfg(windows)]
    {
        directory.dir_metadata().map(|_| ())
    }
    #[cfg(not(windows))]
    {
        let mut options = OpenOptions::new();
        options.read(true).maybe_dir(true);
        directory
            .open_with(".", &options)
            .and_then(|file| file.sync_all())
    }
}

/// Open an owned leaf through a held parent, never following a replaced root
/// symlink. Missing roots are normal before the first artifact is sealed.
fn open_owned_directory_nofollow(root: &Path) -> io::Result<Option<Dir>> {
    let name = root
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "root has no leaf name"))?;
    let parent_path = root
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "root has no parent"))?;
    let parent = Dir::open_ambient_dir(parent_path, cap_std::ambient_authority())?;
    match parent.open_dir_nofollow(name) {
        Ok(opened) => Ok(Some(opened)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Read an optional bounded regular receipt from an already-open child.
/// A replaced symlink or unreadable receipt never authorizes reclamation.
fn read_child_receipt(child: &Dir, receipt: &str, limit: usize) -> io::Result<Option<Vec<u8>>> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let mut file = match child.open_with(receipt, &options) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take((limit as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Ok(None);
    }
    Ok(Some(bytes))
}

/// Capability-relative census classes. The caller decides which
/// bounded receipt identities are recognizable, while private-fs owns all
/// directory traversal, metadata and hardlink accounting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectoryCensus<const N: usize> {
    pub counts: [u64; N],
    pub bytes: [u64; N],
    pub unrecognized: u64,
}

/// Classify direct children under one held, no-follow root, then charge live
/// classes in caller-specified priority order. A root swap cannot redirect
/// receipt reads or recursive size walks; an unreadable/oversized receipt
/// grants no classification. Substituted unsafe children fail closed.
pub fn census_child_directories_nofollow<const N: usize>(
    root: &Path,
    receipt: Option<(&str, usize)>,
    mut classify: impl FnMut(&OsStr, Option<&[u8]>) -> Option<usize>,
) -> io::Result<Option<DirectoryCensus<N>>> {
    let Some(directory) = open_owned_directory_nofollow(root)? else {
        return Ok(None);
    };
    let mut classes: [Vec<Dir>; N] = std::array::from_fn(|_| Vec::new());
    let mut result = DirectoryCensus {
        counts: [0; N],
        bytes: [0; N],
        unrecognized: 0,
    };
    for entry in directory.read_dir(".")? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            result.unrecognized = result.unrecognized.saturating_add(1);
            continue;
        }
        let name = entry.file_name();
        let child = directory.open_dir_nofollow(&name)?;
        let receipt_bytes = receipt
            .and_then(|(file, limit)| read_child_receipt(&child, file, limit).ok().flatten());
        match classify(&name, receipt_bytes.as_deref()) {
            Some(index) if index < N => {
                result.counts[index] = result.counts[index].saturating_add(1);
                classes[index].push(child);
            }
            _ => result.unrecognized = result.unrecognized.saturating_add(1),
        }
    }

    // A live layered generation may hard-link files from a superseded base.
    // The first class to charge a multiply linked inode owns those bytes.
    let mut counted = std::collections::HashSet::new();
    for (index, children) in classes.into_iter().enumerate() {
        for child in children {
            result.bytes[index] = result.bytes[index]
                .saturating_add(directory_bytes_nofollow(child, &mut counted)?);
        }
    }
    Ok(Some(result))
}

fn directory_bytes_nofollow(
    root: Dir,
    counted: &mut std::collections::HashSet<(u64, u64)>,
) -> io::Result<u64> {
    let mut total = 0_u64;
    let mut pending = vec![root];
    while let Some(directory) = pending.pop() {
        for entry in directory.read_dir(".")? {
            let entry = entry?;
            let name = entry.file_name();
            if entry.file_type()?.is_dir() {
                pending.push(directory.open_dir_nofollow(&name)?);
                continue;
            }
            let metadata = directory.symlink_metadata(&name)?;
            #[cfg(unix)]
            {
                use cap_fs_ext::MetadataExt;
                if metadata.nlink() > 1 && !counted.insert((metadata.dev(), metadata.ino())) {
                    continue;
                }
            }
            #[cfg(not(unix))]
            let _ = &counted;
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}

/// Enumerate and reclaim only selected real child directories under one held
/// no-follow root. Selection (including optional receipt evidence) and removal
/// refer to the *same opened child*: a root or child swap cannot redirect the
/// deletion into a foreign pathname. An unsafe entry is preserved.
pub fn remove_selected_child_directories_nofollow(
    root: &Path,
    receipt: Option<(&str, usize)>,
    mut eligible: impl FnMut(&OsStr, Option<&[u8]>) -> bool,
) -> io::Result<Option<usize>> {
    let Some(directory) = open_owned_directory_nofollow(root)? else {
        return Ok(None);
    };
    let mut removed = 0usize;
    for entry in directory.read_dir(".")? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let child = match directory.open_dir_nofollow(&name) {
            Ok(child) => child,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                tracing::warn!(
                    event = "private_fs.cleanup_unsafe_child",
                    root = %root.display(),
                    child = ?name,
                    %error,
                    "unverifiable cleanup child preserved"
                );
                continue;
            }
        };
        let receipt_bytes = match receipt {
            Some((file, limit)) => read_child_receipt(&child, file, limit).ok().flatten(),
            None => None,
        };
        if eligible(&name, receipt_bytes.as_deref()) {
            remove_open_dir_all_nofollow(child)?;
            removed += 1;
        }
    }
    Ok(Some(removed))
}

/// Remove an exact child of the held root. Unlike a path-based recursive
/// removal this never follows a replacement of the root or child symlink.
pub fn remove_named_child_directory_nofollow(root: &Path, name: &OsStr) -> io::Result<()> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "removal requires one child directory name",
        ));
    }
    let Some(directory) = open_owned_directory_nofollow(root)? else {
        return Ok(());
    };
    let child = match directory.open_dir_nofollow(name) {
        Ok(child) => child,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    remove_open_dir_all_nofollow(child)
}

/// Removes the held directory without following child symlinks.
pub fn remove_open_dir_all_nofollow(directory: Dir) -> io::Result<()> {
    #[cfg(not(windows))]
    {
        directory.remove_open_dir_all()
    }
    #[cfg(windows)]
    {
        // cap-std's Windows recursive removal drops its capability before
        // descending. Keep ours open until its children have been removed.
        for entry in directory.read_dir(".")? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let child = directory.open_dir_nofollow(entry.file_name())?;
                remove_open_dir_all_nofollow(child)?;
            } else {
                entry.remove_file()?;
            }
        }
        directory.remove_open_dir()
    }
}

#[cfg(test)]
mod replacement_tests {
    use std::io::Read;

    use super::*;

    #[cfg(unix)]
    #[test]
    fn census_uses_held_root_after_foreign_symlink_swap() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let owned = temp.path().join("owned");
        let moved = temp.path().join("moved");
        let foreign = temp.path().join("foreign");
        std::fs::create_dir_all(owned.join("generation")).unwrap();
        std::fs::create_dir_all(foreign.join("generation")).unwrap();
        std::fs::write(owned.join("generation").join("payload"), vec![1_u8; 41]).unwrap();
        std::fs::write(foreign.join("generation").join("payload"), vec![2_u8; 100_041])
            .unwrap();

        let mut swapped = false;
        let measured = census_child_directories_nofollow::<1>(&owned, None, |name, _| {
            assert_eq!(name, OsStr::new("generation"));
            assert!(!swapped);
            std::fs::rename(&owned, &moved).unwrap();
            symlink(&foreign, &owned).unwrap();
            swapped = true;
            Some(0)
        })
        .unwrap()
        .unwrap();
        assert!(swapped);
        assert_eq!(measured.counts, [1]);
        assert_eq!(measured.bytes, [41]);
        assert_eq!(
            std::fs::metadata(foreign.join("generation").join("payload"))
                .unwrap()
                .len(),
            100_041
        );
        assert_eq!(
            std::fs::metadata(moved.join("generation").join("payload"))
                .unwrap()
                .len(),
            41
        );
    }

    #[cfg(unix)]
    #[test]
    fn census_never_descends_foreign_symlink_children() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let owned = temp.path().join("owned");
        let foreign = temp.path().join("foreign");
        std::fs::create_dir_all(owned.join("generation")).unwrap();
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::write(foreign.join("payload"), vec![0_u8; 100_041]).unwrap();
        std::fs::write(owned.join("generation").join("local"), vec![0_u8; 41]).unwrap();
        symlink(&foreign, owned.join("generation").join("linked")).unwrap();
        let measured = census_child_directories_nofollow::<1>(&owned, None, |_, _| Some(0))
            .unwrap()
            .unwrap();
        assert_eq!(measured.counts, [1]);
        assert!(measured.bytes[0] >= 41);
        assert!(measured.bytes[0] < 100_041);
    }

    #[test]
    fn replacement_publishes_complete_bytes_and_refuses_parent_traversal() {
        let root = tempfile::tempdir().unwrap();
        let parent = Dir::open_ambient_dir(root.path(), cap_std::ambient_authority()).unwrap();
        std::fs::write(root.path().join("published"), b"before").unwrap();
        std::fs::write(root.path().join("staged"), b"after").unwrap();
        let mut reader = std::fs::File::open(root.path().join("published")).unwrap();
        let error =
            replace_file(&parent, OsStr::new("staged"), OsStr::new("../escape")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            std::fs::read(root.path().join("published")).unwrap(),
            b"before"
        );
        assert_eq!(std::fs::read(root.path().join("staged")).unwrap(), b"after");
        replace_file(&parent, OsStr::new("staged"), OsStr::new("published")).unwrap();
        assert_eq!(
            std::fs::read(root.path().join("published")).unwrap(),
            b"after"
        );
        let mut displaced = Vec::new();
        reader.read_to_end(&mut displaced).unwrap();
        assert_eq!(displaced, b"before");
        assert!(!root.path().join("staged").exists());
    }

    #[cfg(unix)]
    #[test]
    fn held_cleanup_does_not_follow_a_root_swap_into_foreign_data() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("graph.sealed");
        let former = temp.path().join("former-sealed");
        let foreign = temp.path().join("foreign");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&foreign).unwrap();
        for parent in [&root, &foreign] {
            std::fs::create_dir(parent.join(".staging-owned")).unwrap();
            std::fs::write(parent.join(".staging-owned/keep"), b"sentinel").unwrap();
        }
        let mut swapped = false;
        let removed = remove_selected_child_directories_nofollow(&root, None, |name, _| {
            if !swapped {
                std::fs::rename(&root, &former).unwrap();
                symlink(&foreign, &root).unwrap();
                swapped = true;
            }
            name == OsStr::new(".staging-owned")
        })
        .unwrap();
        assert!(swapped);
        assert_eq!(removed, Some(1));
        assert!(!former.join(".staging-owned").exists());
        assert_eq!(
            std::fs::read(foreign.join(".staging-owned/keep")).unwrap(),
            b"sentinel"
        );
        assert!(
            remove_named_child_directory_nofollow(&root, OsStr::new(".staging-owned")).is_err()
        );
        assert_eq!(
            std::fs::read(foreign.join(".staging-owned/keep")).unwrap(),
            b"sentinel"
        );
    }

    #[cfg(windows)]
    #[test]
    fn no_replace_publication_preserves_an_occupied_entry() {
        let root = tempfile::tempdir().unwrap();
        let parent = Dir::open_ambient_dir(root.path(), cap_std::ambient_authority()).unwrap();
        std::fs::write(root.path().join("staged"), b"owned").unwrap();
        std::fs::write(root.path().join("published"), b"foreign").unwrap();
        let error = rename_noreplace(
            &parent,
            OsStr::new("staged"),
            &parent,
            OsStr::new("published"),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(root.path().join("staged")).unwrap(), b"owned");
        assert_eq!(
            std::fs::read(root.path().join("published")).unwrap(),
            b"foreign"
        );
        std::fs::remove_file(root.path().join("published")).unwrap();
        rename_noreplace(
            &parent,
            OsStr::new("staged"),
            &parent,
            OsStr::new("published"),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(root.path().join("published")).unwrap(),
            b"owned"
        );
        assert!(!root.path().join("staged").exists());
    }
}

#[cfg(all(
    test,
    any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))
))]
mod tests {
    use cap_std::ambient_authority;

    use super::*;

    fn open(path: &std::path::Path) -> Dir {
        Dir::open_ambient_dir(path, ambient_authority()).expect("open test directory")
    }

    #[test]
    fn rename_noreplace_moves_between_parents_and_refuses_occupied_destinations() {
        let root = tempfile::tempdir().expect("create rename fixture");
        std::fs::create_dir(root.path().join("source")).expect("create source parent");
        std::fs::create_dir(root.path().join("target")).expect("create target parent");
        std::fs::write(root.path().join("source/payload"), b"owned").expect("write payload");
        let source = open(&root.path().join("source"));
        let target = open(&root.path().join("target"));

        rename_noreplace(
            &source,
            OsStr::new("payload"),
            &target,
            OsStr::new("payload"),
        )
        .expect("cross-parent rename succeeds into a free destination");
        assert_eq!(
            std::fs::read(root.path().join("target/payload")).expect("moved payload"),
            b"owned"
        );

        std::fs::write(root.path().join("source/payload"), b"replacement")
            .expect("write replacement");
        let error = rename_noreplace(
            &source,
            OsStr::new("payload"),
            &target,
            OsStr::new("payload"),
        )
        .expect_err("an occupied destination must never be replaced");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(root.path().join("target/payload")).expect("retained payload"),
            b"owned"
        );
    }

    #[test]
    fn recursive_removal_unlinks_symlinks_without_following_them() {
        let root = tempfile::tempdir().expect("create removal fixture");
        let external = tempfile::tempdir().expect("create external target");
        std::fs::write(external.path().join("sentinel"), b"preserve").expect("write sentinel");
        let tree = root.path().join("tree");
        std::fs::create_dir_all(tree.join("nested")).expect("create nested tree");
        std::fs::write(tree.join("nested/file"), b"bytes").expect("write nested file");
        std::os::unix::fs::symlink(external.path(), tree.join("escape"))
            .expect("plant symlink escape");

        remove_open_dir_all_nofollow(open(&tree)).expect("recursive removal consumes the tree");

        assert!(!tree.exists());
        assert_eq!(
            std::fs::read(external.path().join("sentinel")).expect("sentinel survives"),
            b"preserve"
        );
    }

    #[test]
    fn a_transient_missing_answer_is_looked_at_again() {
        let mut attempts = 0;
        let value = look_again_on_not_found(|| {
            attempts += 1;
            if attempts <= 2 {
                Err(io::Error::from(io::ErrorKind::NotFound))
            } else {
                Ok(attempts)
            }
        })
        .expect("the entry that appears within the bound is returned");
        assert_eq!(value, 3);
    }

    #[test]
    fn a_persistent_missing_answer_is_still_reported_after_the_bound() {
        let mut attempts = 0;
        let error = look_again_on_not_found(|| -> io::Result<()> {
            attempts += 1;
            Err(io::Error::from(io::ErrorKind::NotFound))
        })
        .expect_err("a parent that is genuinely gone must not spin");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(attempts, CREATE_RACE_LOOKS + 1);
    }

    #[test]
    fn other_errors_are_not_looked_at_again() {
        let mut attempts = 0;
        let error = look_again_on_not_found(|| -> io::Result<()> {
            attempts += 1;
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        })
        .expect_err("only the create race is retried");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(attempts, 1);
    }

    /// Many threads racing the first creation of one name must all come back
    /// holding the same entry. On macOS this is the `openat(O_CREAT)` race
    /// the helper exists for; elsewhere it is a plain concurrency smoke test.
    #[test]
    fn concurrent_creators_of_one_entry_all_open_it() {
        use std::os::unix::fs::MetadataExt;

        let root = tempfile::tempdir().expect("create race fixture");
        for round in 0..16 {
            let name = format!("racy-{round}.lock");
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
            let handles = (0..8)
                .map(|_| {
                    let path = root.path().to_path_buf();
                    let name = name.clone();
                    let barrier = std::sync::Arc::clone(&barrier);
                    std::thread::spawn(move || {
                        let directory = open(&path);
                        let mut options = OpenOptions::new();
                        options.read(true).write(true).create(true);
                        barrier.wait();
                        open_or_create_with(&directory, OsStr::new(&name), &options)
                            .expect("a create-or-open loser must still be handed the entry")
                            .into_std()
                            .metadata()
                            .expect("metadata of the opened entry")
                            .ino()
                    })
                })
                .collect::<Vec<_>>();
            let inodes = handles
                .into_iter()
                .map(|handle| handle.join().expect("creator thread"))
                .collect::<Vec<_>>();
            assert!(
                inodes.iter().all(|inode| *inode == inodes[0]),
                "every racer must open the one created entry"
            );
        }
    }
}
