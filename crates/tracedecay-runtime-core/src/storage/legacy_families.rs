//! The native engine's fresh start: SQLite-era store families are deleted,
//! never converted or copied (Zack, 2026-10-10).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tracedecay_store::StoreFile;

use super::{
    PrivateStoreIo, STORE_MANIFEST_FILENAME, profile_shards_root, read_store_manifest,
    write_store_manifest_to_path,
};

/// Deletes every legacy SQLite store family under the profile's canonical
/// roots: the profile root, each project shard and each remote node. A
/// family's paired graph store goes with it, because it indexes rows of the
/// deleted store. Nothing else is touched, so a rerun removes nothing.
///
/// Returns how many entries were removed.
pub(crate) fn delete_legacy_store_families(profile_root: &Path) -> io::Result<usize> {
    let mut removed = 0;
    for store in StoreFile::PROFILE_ROOT {
        removed += delete_legacy_family(profile_root, store)?;
    }
    for shard in child_directories(&profile_shards_root(profile_root))? {
        for store in StoreFile::PROJECT_ROOT {
            removed += delete_legacy_family(&shard, store)?;
        }
        rename_manifest_stores(&shard.join(STORE_MANIFEST_FILENAME))?;
    }
    for node in child_directories(&profile_root.join("remote").join("nodes"))? {
        removed += delete_legacy_family(&node, StoreFile::RemoteNode)?;
    }
    Ok(removed)
}

fn delete_legacy_family(root: &Path, store: StoreFile) -> io::Result<usize> {
    let mut present = false;
    for name in store.legacy_names() {
        present |= exists(&root.join(name))?;
    }
    if !present {
        return Ok(0);
    }
    // The graph goes first: a crash after it and before the legacy files
    // leaves the legacy family present, so the rerun still owns the graph.
    let mut removed = 0;
    for name in store.graph_names().iter().chain(store.legacy_names()) {
        removed += usize::from(remove_member(&root.join(name))?);
    }
    Ok(removed)
}

/// Points a shard manifest that still names the SQLite-era files at the
/// native ones. The admission digest belonged to the deleted sessions store,
/// so it goes too. A manifest that does not parse is left for shard
/// validation to report.
fn rename_manifest_stores(path: &Path) -> io::Result<()> {
    if !exists(path)? {
        return Ok(());
    }
    let Ok(mut manifest) = read_store_manifest(path) else {
        return Ok(());
    };
    let mut changed = false;
    for (relpath, store) in [
        (&mut manifest.graph_db_relpath, StoreFile::Project),
        (
            &mut manifest.sessions_db_relpath,
            StoreFile::ProjectSessions,
        ),
    ] {
        if relpath.as_path() == Path::new(store.legacy_names()[0])
            && store.file_name() != store.legacy_names()[0]
        {
            *relpath = PathBuf::from(store.file_name());
            changed = true;
        }
    }
    if !changed {
        return Ok(());
    }
    manifest.sessions_schema_digest = None;
    write_store_manifest_to_path(path, &manifest).map_err(io::Error::other)
}

fn exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn remove_member(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path).map(|()| true),
        Ok(_) => PrivateStoreIo::remove_file_durable(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn child_directories(root: &Path) -> io::Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut directories = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            directories.push(entry.path());
        }
    }
    Ok(directories)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"x").unwrap();
    }

    fn listing(root: &Path) -> Vec<String> {
        let mut names = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(directory) = stack.pop() {
            for entry in fs::read_dir(&directory).unwrap() {
                let path = entry.unwrap().path();
                names.push(path.strip_prefix(root).unwrap().display().to_string());
                if path.is_dir() {
                    stack.push(path);
                }
            }
        }
        names.sort();
        names
    }

    #[test]
    fn deletes_exactly_the_legacy_families_and_their_graphs_then_is_a_no_op() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let shard = profile_shards_root(root).join("proj_a");
        let node = root.join("remote").join("nodes").join("abc");
        for path in [
            root.join("global.db"),
            root.join("global.db-wal"),
            root.join("global.grafeo"),
            root.join("user-sessions.db-journal"),
            shard.join("sessions.db"),
            shard.join("sessions.db-shm"),
            shard.join("sessions.grafeo.wal"),
            node.join("remote.db"),
        ] {
            write(&path);
        }
        let kept = [
            root.join("notes.db"),
            root.join("profile-identity.json"),
            root.join("user-memory.grafeo"),
            root.join("global.tdb"),
            shard.join("store_manifest.json"),
            shard.join("tracedecay.grafeo"),
            node.join("remote.tdb-wal"),
        ];
        for path in &kept {
            write(path);
        }

        assert_eq!(delete_legacy_store_families(root).unwrap(), 8);

        let mut expected: Vec<String> = kept
            .iter()
            .map(|path| path.strip_prefix(root).unwrap().display().to_string())
            .chain(
                [
                    "projects",
                    "projects/proj_a",
                    "remote",
                    "remote/nodes",
                    "remote/nodes/abc",
                ]
                .map(str::to_owned),
            )
            .collect();
        expected.sort();
        assert_eq!(listing(root), expected);

        assert_eq!(delete_legacy_store_families(root).unwrap(), 0);
        assert_eq!(listing(root), expected);
    }
}
