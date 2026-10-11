//! The single owner of owned-store file names.
//!
//! Every owned store family lives under one stem. The engine compiled into
//! the binary picks the live names: `<stem>.tdb`, `<stem>.tdb-wal` and the
//! `<stem>.tdb-tmp/` directory its sorts spill into under the native engine,
//! `<stem>.db` plus its `-wal`/`-shm`/`-journal` sidecars under SQLite. The
//! SQLite names are the legacy family the native runtime deletes on its first
//! boot.

use std::ops::Deref;
use std::path::{Path, PathBuf};

const NATIVE: bool = matches!(tracedecay_sql::ENGINE, tracedecay_sql::Engine::Native);

struct Names {
    stem: &'static str,
    native: [&'static str; 3],
    legacy: [&'static str; 4],
    graph: [&'static str; 2],
}

macro_rules! names {
    ($stem:literal) => {
        Names {
            stem: $stem,
            native: [
                concat!($stem, ".tdb"),
                concat!($stem, ".tdb-wal"),
                concat!($stem, ".tdb-tmp"),
            ],
            legacy: [
                concat!($stem, ".db"),
                concat!($stem, ".db-wal"),
                concat!($stem, ".db-shm"),
                concat!($stem, ".db-journal"),
            ],
            graph: [concat!($stem, ".grafeo"), concat!($stem, ".grafeo.wal")],
        }
    };
}

/// One owned store family, named by where it lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StoreFile {
    /// The profile authority, in the profile root.
    Profile,
    /// Profile memory, in the profile root.
    ProfileMemory,
    /// Profile sessions, in the profile root.
    ProfileSessions,
    /// One remote node's store, in `remote/nodes/<digest>/`.
    RemoteNode,
    /// A project's code and memory store, in its project shard.
    Project,
    /// A project's sessions store, in its project shard.
    ProjectSessions,
}

const TABLE: [Names; 6] = [
    names!("global"),
    names!("user-memory"),
    names!("user-sessions"),
    names!("remote"),
    names!("tracedecay"),
    names!("sessions"),
];

impl StoreFile {
    /// Families stored directly in the profile root.
    pub const PROFILE_ROOT: [Self; 3] = [Self::Profile, Self::ProfileMemory, Self::ProfileSessions];
    /// Families stored in each project shard.
    pub const PROJECT_ROOT: [Self; 2] = [Self::Project, Self::ProjectSessions];
    /// What the compiled engine appends to a main file name for its sidecar
    /// files.
    pub const SIDECAR_SUFFIXES: &'static [&'static str] = if NATIVE {
        &["-wal"]
    } else {
        &["-wal", "-shm", "-journal"]
    };
    /// What the compiled engine appends to a main file name for the private
    /// directory its sorts and ephemeral tables spill into. Closing the store
    /// removes it, so one that lingers is a dead process's scratch and never
    /// durable data.
    pub const TEMP_DIR_SUFFIXES: &'static [&'static str] = if NATIVE { &["-tmp"] } else { &[] };

    const fn names(self) -> &'static Names {
        &TABLE[self as usize]
    }

    /// The stem every member of the family, graph store included, starts with.
    pub const fn stem(self) -> &'static str {
        self.names().stem
    }

    /// The main database file name under the compiled engine.
    pub const fn file_name(self) -> &'static str {
        if NATIVE {
            self.names().native[0]
        } else {
            self.names().legacy[0]
        }
    }

    /// The main file, every sidecar file and the temporary directory the
    /// compiled engine writes beside it.
    pub const fn family_names(self) -> &'static [&'static str] {
        if NATIVE {
            &self.names().native
        } else {
            &self.names().legacy
        }
    }

    /// The SQLite-era main file and its `-wal`, `-shm` and `-journal` sidecars.
    pub const fn legacy_names(self) -> &'static [&'static str; 4] {
        &self.names().legacy
    }

    /// The Grafeo graph store paired with this family and its WAL.
    pub const fn graph_names(self) -> &'static [&'static str; 2] {
        &self.names().graph
    }

    /// The family's main database file under `root`.
    pub fn path(self, root: &Path) -> OwnedStorePath {
        OwnedStorePath(root.join(self.file_name()))
    }
}

/// A path whose file name is an owned store's main database file under the
/// compiled engine. Only [`StoreFile::path`] constructs one.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OwnedStorePath(PathBuf);

impl OwnedStorePath {
    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }
}

impl Deref for OwnedStorePath {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for OwnedStorePath {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl From<OwnedStorePath> for PathBuf {
    fn from(path: OwnedStorePath) -> Self {
        path.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_family_has_distinct_names_and_engine_matching_main_file() {
        let all = [
            StoreFile::Profile,
            StoreFile::ProfileMemory,
            StoreFile::ProfileSessions,
            StoreFile::RemoteNode,
            StoreFile::Project,
            StoreFile::ProjectSessions,
        ];
        let mut seen = std::collections::BTreeSet::new();
        for file in all {
            for name in file
                .legacy_names()
                .iter()
                .chain(file.names().native.iter())
                .chain(file.graph_names())
            {
                assert!(seen.insert(*name), "{name} is spelled by two families");
            }
            let members = std::iter::once("")
                .chain(StoreFile::SIDECAR_SUFFIXES.iter().copied())
                .chain(StoreFile::TEMP_DIR_SUFFIXES.iter().copied())
                .map(|suffix| format!("{}{suffix}", file.file_name()))
                .collect::<Vec<_>>();
            assert_eq!(file.family_names(), members.as_slice());
            let expected = if NATIVE { ".tdb" } else { ".db" };
            assert!(file.file_name().ends_with(expected), "{}", file.file_name());
        }
        assert_eq!(StoreFile::Profile.legacy_names()[0], "global.db");
        assert_eq!(
            StoreFile::ProjectSessions.path(Path::new("/p")).as_ref(),
            Path::new("/p").join(StoreFile::ProjectSessions.file_name())
        );
    }
}
