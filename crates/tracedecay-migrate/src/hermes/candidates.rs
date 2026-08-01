//! Discovery of legacy Hermes profile stores eligible for migration.

use std::fs;
use std::path::{Path, PathBuf};

use super::resolution::same_path;
use crate::hermes::{LegacyHermesMigration, LegacyHermesMigrationIssue};

pub(crate) struct LegacyStoreCandidate {
    pub(crate) profile_dir: PathBuf,
    pub(crate) source_db: PathBuf,
    pub(crate) source_sessions_db: Option<PathBuf>,
    pub(crate) source_memory_db: Option<PathBuf>,
    pub(crate) legacy_registry_project_id: Option<String>,
}

impl LegacyStoreCandidate {
    pub(crate) fn primary_path(&self) -> &Path {
        &self.source_db
    }
}

fn same_optional_path(left: Option<&Path>, right: Option<&Path>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => same_path(left, right),
        (None, None) => true,
        _ => false,
    }
}

pub(crate) fn legacy_profile_dirs_for_homes(hermes_homes: &[PathBuf]) -> Vec<PathBuf> {
    let mut profiles = hermes_homes
        .iter()
        .flat_map(|home| legacy_profile_dirs(home))
        .collect::<Vec<_>>();
    profiles.sort();
    profiles.dedup_by(|left, right| same_path(left, right));
    profiles
}

pub(crate) fn legacy_profile_dirs(hermes_home: &Path) -> Vec<PathBuf> {
    let mut profiles = vec![hermes_home.to_path_buf()];
    if !hermes_home.is_dir() {
        return profiles;
    }
    if let Ok(entries) = fs::read_dir(hermes_home.join("profiles")) {
        let mut named = entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                // Do not let a profile symlink turn this bounded scan into an
                // arbitrary filesystem walk.
                entry.file_type().ok()?.is_dir().then(|| entry.path())
            })
            .collect::<Vec<_>>();
        named.sort();
        profiles.extend(named);
    }
    profiles
}

pub(crate) enum CandidateOutcome {
    Migrated(LegacyHermesMigration, Option<LegacyHermesMigrationIssue>),
    AlreadyMigrated(LegacyHermesMigration, Option<LegacyHermesMigrationIssue>),
}

pub(crate) enum CandidateError {
    Unresolved(String),
    Failed(String),
}

pub(crate) fn legacy_store_candidates(
    profiles: &[PathBuf],
    tracedecay_profile_root: &Path,
) -> Vec<LegacyStoreCandidate> {
    let mut candidates = profiles
        .iter()
        .filter_map(|profile_dir| {
            let data_root = profile_dir.join(".tracedecay");
            let sessions_db = data_root.join(crate::root_seam::storage::SESSIONS_DB_FILENAME);
            let memory_db = data_root.join(crate::root_seam::config::db_filename(&data_root));
            (sessions_db.is_file() || memory_db.is_file()).then(|| LegacyStoreCandidate {
                profile_dir: profile_dir.clone(),
                source_db: if sessions_db.is_file() {
                    sessions_db.clone()
                } else {
                    memory_db.clone()
                },
                source_sessions_db: sessions_db.is_file().then_some(sessions_db),
                source_memory_db: memory_db.is_file().then_some(memory_db),
                legacy_registry_project_id: None,
            })
        })
        .collect::<Vec<_>>();

    // A short-lived historical release could create a user-profile shard
    // whose manifest identified a Hermes profile as the code project. Scan
    // only immediate project shards and accept only exact standard-profile
    // identities; unrelated profile stores are never opened.
    if let Ok(entries) = fs::read_dir(tracedecay_profile_root.join("projects")) {
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let shard = entry.path();
            let manifest_path = shard.join(crate::root_seam::storage::STORE_MANIFEST_FILENAME);
            let Ok(manifest) = crate::root_seam::storage::read_store_manifest(&manifest_path)
            else {
                continue;
            };
            let Some(profile_dir) = profiles
                .iter()
                .find(|profile| same_path(profile, &manifest.project_root))
            else {
                continue;
            };
            let sessions_db = shard.join(crate::root_seam::storage::SESSIONS_DB_FILENAME);
            let memory_db = shard.join(crate::root_seam::config::db_filename(&shard));
            if sessions_db.is_file() || memory_db.is_file() {
                candidates.push(LegacyStoreCandidate {
                    profile_dir: profile_dir.clone(),
                    source_db: if sessions_db.is_file() {
                        sessions_db.clone()
                    } else {
                        memory_db.clone()
                    },
                    source_sessions_db: sessions_db.is_file().then_some(sessions_db),
                    source_memory_db: memory_db.is_file().then_some(memory_db),
                    legacy_registry_project_id: manifest.project_id,
                });
            }
        }
    }
    candidates.sort_by(|left, right| left.primary_path().cmp(right.primary_path()));
    candidates.dedup_by(|left, right| {
        same_optional_path(
            left.source_sessions_db.as_deref(),
            right.source_sessions_db.as_deref(),
        ) && same_optional_path(
            left.source_memory_db.as_deref(),
            right.source_memory_db.as_deref(),
        )
    });
    candidates
}
