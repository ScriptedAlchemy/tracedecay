//! Branch metadata persistence for multi-branch indexing.
//!
//! Stores tracking information in `branch-meta.json` inside the project data
//! dir.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracedecay_domain::BranchGraphPublicationEpochV1;

use crate::storage::{BRANCH_META_FILENAME, PrivateStoreIo};

/// Metadata for a single tracked branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchEntry {
    /// Relative path to the database serving this branch. Branches tracked on
    /// the single project graph store reference the canonical main database
    /// (`tracedecay.db`) — the same shape the default branch has always used.
    /// Legacy private branch copies reference `branches/<stem>.db`; those
    /// files are retained only for garbage collection and never serve.
    pub db_file: String,
    /// Nearest tracked ancestor at tracking time (None for the default
    /// branch).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// UNIX timestamp (seconds) when this branch DB was created.
    pub created_at: String,
    /// UNIX timestamp (seconds) of last successful sync.
    pub last_synced_at: String,
    /// Whether automatic branch-store GC must retain this entry even when it
    /// has no matching git ref.
    #[serde(default)]
    pub gc_protected: bool,
    /// Exact source identity of the graph published by the last successful
    /// sync. Older metadata omits this evidence and is not branch-query
    /// eligible until the next sync.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_source: Option<BranchGraphSourceV1>,
}

impl BranchEntry {
    /// True when this branch is served by the single project graph store.
    ///
    /// Only legacy entries reference a private `branches/<stem>.db` copy;
    /// physical deletion inventories must be limited to those.
    #[must_use]
    pub fn served_by_project_store(&self) -> bool {
        self.db_file == crate::config::DB_FILENAME
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BranchGraphSourceV1 {
    /// Monotonic graph-local mutation epoch. Every writer advances this before
    /// exposing any row mutation, including semantic no-op rewrites.
    pub publication_epoch: BranchGraphPublicationEpochV1,
    pub project_id: String,
    pub repository_id: String,
    pub worktree_id: String,
    pub worktree_root: String,
    pub reference: String,
    pub source_oid: String,
}

/// Exact graph provenance before the metadata publisher assigns its monotonic
/// publication epoch. Callers must derive this from one Git snapshot rather
/// than composing fields from separate scheduler observations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchGraphSourceDraftV1 {
    pub project_id: String,
    pub repository_id: String,
    pub worktree_id: String,
    pub worktree_root: String,
    pub reference: String,
    pub source_oid: String,
}

impl BranchGraphSourceV1 {
    #[must_use]
    pub fn matches_draft(&self, draft: &BranchGraphSourceDraftV1) -> bool {
        self.project_id == draft.project_id
            && self.repository_id == draft.repository_id
            && self.worktree_id == draft.worktree_id
            && self.worktree_root == draft.worktree_root
            && self.reference == draft.reference
            && self.source_oid == draft.source_oid
    }
}

/// Result of the locked graph-source compare-and-swap publisher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchGraphSourcePublishOutcomeV1 {
    Published(BranchGraphSourcePublicationV1),
    AlreadyPublished(BranchGraphSourceV1),
    CompareAndSwapMiss {
        observed: Option<BranchGraphSourceV1>,
    },
    BranchNotTracked,
}

/// Exact metadata mutation installed by [`publish_graph_source`].
///
/// This is an in-memory rollback authority, not persisted branch metadata.
/// Its full-entry compare-and-swap precondition prevents a failed publisher
/// from erasing a newer sync timestamp or foreign source publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchGraphSourcePublicationV1 {
    branch: String,
    previous_entry: BranchEntry,
    installed_entry: BranchEntry,
}

impl BranchGraphSourcePublicationV1 {
    #[must_use]
    pub fn source(&self) -> Option<&BranchGraphSourceV1> {
        self.installed_entry.graph_source.as_ref()
    }
}

/// Result of an exact graph-source publication rollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchGraphSourceRollbackOutcomeV1 {
    Restored,
    NoMatch,
}

/// Top-level branch metadata for a project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchMeta {
    /// The auto-detected or configured default branch name.
    pub default_branch: String,
    /// Map of branch name → entry.
    #[serde(serialize_with = "serialize_branches")]
    pub branches: HashMap<String, BranchEntry>,
}

impl BranchMeta {
    /// Creates a new metadata with a single default branch entry pointing at
    /// the standard `tracedecay.db`.
    pub fn new(default_branch: &str) -> Self {
        Self::with_db_file(default_branch, crate::config::DB_FILENAME)
    }

    /// Creates a new metadata whose default-branch entry references the main
    /// DB filename appropriate for `data_dir`.
    pub fn new_for_dir(data_dir: &Path, default_branch: &str) -> Self {
        Self::with_db_file(default_branch, crate::config::db_filename(data_dir))
    }

    /// Synthesizes metadata for a legacy store that only has the canonical
    /// main database. The timestamps are deliberately unknown (`0`) so the
    /// same input produces byte-identical metadata across interrupted retries.
    pub fn for_legacy_single_db(data_dir: &Path, default_branch: &str) -> Self {
        Self::with_db_file_and_timestamp(default_branch, crate::config::db_filename(data_dir), "0")
    }

    fn with_db_file(default_branch: &str, db_file: &str) -> Self {
        let now = now_unix_str();
        Self::with_db_file_and_timestamp(default_branch, db_file, &now)
    }

    fn with_db_file_and_timestamp(default_branch: &str, db_file: &str, timestamp: &str) -> Self {
        let mut branches = HashMap::new();
        branches.insert(
            default_branch.to_string(),
            BranchEntry {
                db_file: db_file.to_string(),
                parent: None,
                created_at: timestamp.to_string(),
                last_synced_at: timestamp.to_string(),
                gc_protected: false,
                graph_source: None,
            },
        );
        Self {
            default_branch: default_branch.to_string(),
            branches,
        }
    }

    /// Adds a new tracked branch entry.
    pub fn add_branch(&mut self, name: &str, db_file: &str, parent: &str) {
        let now = now_unix_str();
        self.branches.insert(
            name.to_string(),
            BranchEntry {
                db_file: db_file.to_string(),
                parent: Some(parent.to_string()),
                created_at: now.clone(),
                last_synced_at: now,
                gc_protected: false,
                graph_source: None,
            },
        );
    }

    /// Removes a tracked branch entry. Returns the entry if it existed.
    pub fn remove_branch(&mut self, name: &str) -> Option<BranchEntry> {
        if name == self.default_branch {
            return None; // never remove the default branch
        }
        self.branches.remove(name)
    }

    /// Updates the `last_synced_at` timestamp for a branch.
    pub fn touch_synced(&mut self, name: &str) {
        if let Some(entry) = self.branches.get_mut(name) {
            entry.last_synced_at = now_unix_str();
        }
    }

    pub fn publish_graph_source(&mut self, name: &str, source: BranchGraphSourceV1) {
        if let Some(entry) = self.branches.get_mut(name) {
            entry.last_synced_at = now_unix_str();
            entry.graph_source = Some(source);
        }
    }

    /// Removes all tracked branches except the default. Returns removed entries.
    pub fn remove_all_branches(&mut self) -> Vec<(String, BranchEntry)> {
        let default = self.default_branch.clone();
        let removed: Vec<(String, BranchEntry)> = self
            .branches
            .keys()
            .filter(|name| *name != &default)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .filter_map(|name| self.branches.remove(&name).map(|e| (name, e)))
            .collect();
        removed
    }

    /// Returns true if the given branch is tracked.
    pub fn is_tracked(&self, name: &str) -> bool {
        self.branches.contains_key(name)
    }

    fn validate(&self) -> Result<(), String> {
        if self.default_branch.is_empty() {
            return Err("default_branch must not be empty".to_string());
        }
        let default = self.branches.get(&self.default_branch).ok_or_else(|| {
            format!(
                "default_branch '{}' has no matching branch entry",
                self.default_branch
            )
        })?;
        let canonical_main = crate::config::DB_FILENAME;
        if default.db_file != canonical_main {
            return Err(format!(
                "default branch '{}' must reference canonical main database '{canonical_main}', found '{}'",
                self.default_branch, default.db_file
            ));
        }
        if default.parent.is_some() {
            return Err(format!(
                "default branch '{}' must not have a parent",
                self.default_branch
            ));
        }

        let mut db_files = BTreeMap::new();
        for (name, entry) in &self.branches {
            if name.is_empty() {
                return Err("branch names must not be empty".to_string());
            }
            validate_db_file(name, entry, name == &self.default_branch)?;
            if entry.parent.as_deref() == Some(name.as_str()) {
                return Err(format!("branch '{name}' must not be its own parent"));
            }
            // The canonical main database is shared by every branch served
            // from the single project store; only private legacy copies must
            // be uniquely owned.
            if entry.served_by_project_store() {
                continue;
            }
            if let Some(previous) = db_files.insert(entry.db_file.as_str(), name.as_str()) {
                return Err(format!(
                    "branches '{previous}' and '{name}' reference the same database '{}'",
                    entry.db_file
                ));
            }
        }
        Ok(())
    }
}

fn serialize_branches<S>(
    branches: &HashMap<String, BranchEntry>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    branches
        .iter()
        .collect::<BTreeMap<_, _>>()
        .serialize(serializer)
}

fn validate_db_file(name: &str, entry: &BranchEntry, is_default: bool) -> Result<(), String> {
    let relative = Path::new(&entry.db_file);
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(format!(
            "branch '{name}' database path '{}' is not a normalized store-relative path",
            entry.db_file
        ));
    }
    if !is_default
        && !entry.served_by_project_store()
        && (!relative.starts_with("branches")
            || !relative
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("db")))
    {
        return Err(format!(
            "non-default branch '{name}' database path '{}' must be the canonical main database \
             or a legacy store under 'branches/' with a .db extension",
            entry.db_file
        ));
    }
    Ok(())
}

/// Parses `branch-meta.json` content into [`BranchMeta`].
///
/// This is the canonical definition of "corrupt branch metadata": anything
/// this rejects — invalid JSON *or* valid JSON with the wrong schema — makes
/// the runtime fall back to single-DB mode. Every reader must go through this
/// one predicate so invalid metadata is never treated as authority.
pub fn parse(content: &str) -> serde_json::Result<BranchMeta> {
    let meta: BranchMeta = serde_json::from_str(content)?;
    meta.validate()
        .map_err(<serde_json::Error as serde::de::Error>::custom)?;
    Ok(meta)
}

/// Loads branch metadata from `branch-meta.json` in the project data dir.
///
/// Returns `None` if the file doesn't exist (single-DB mode / pre-branch projects).
/// Prints a warning to stderr if the file exists but is malformed.
pub fn load_branch_meta(data_dir: &Path) -> Option<BranchMeta> {
    let path = data_dir.join(BRANCH_META_FILENAME);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            eprintln!(
                "warning: could not inspect branch metadata at '{}': {error} — falling back to single-DB mode",
                path.display()
            );
            return None;
        }
    };
    if !metadata.file_type().is_file() {
        eprintln!(
            "warning: corrupt branch metadata at '{}': path is not a regular file — falling back to single-DB mode",
            path.display()
        );
        return None;
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) => {
            eprintln!(
                "warning: could not read branch metadata at '{}': {error} — falling back to single-DB mode",
                path.display()
            );
            return None;
        }
    };
    match parse(&content) {
        Ok(meta) => Some(meta),
        Err(e) => {
            eprintln!(
                "warning: corrupt branch metadata at '{}': {e} — falling back to single-DB mode",
                path.display()
            );
            None
        }
    }
}

/// Serializes validated branch metadata in the canonical persisted form.
pub fn serialize_branch_meta(meta: &BranchMeta) -> std::io::Result<String> {
    meta.validate()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    serde_json::to_string_pretty(meta).map_err(std::io::Error::other)
}

/// Publishes already-serialized branch metadata after validating that it is the
/// same canonical schema accepted by runtime readers. This is crate-private so
/// the deletion journal can persist and later compare the exact commit bytes.
pub fn save_branch_meta_serialized(data_dir: &Path, serialized: &str) -> std::io::Result<()> {
    parse(serialized)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let path = data_dir.join(BRANCH_META_FILENAME);
    let temp_path = path.with_extension("json.tmp");
    PrivateStoreIo::write_file_atomically(&path, &temp_path, serialized.as_bytes())
}

/// Saves branch metadata to `branch-meta.json` in the project data dir.
///
/// Writes via a sibling temp file and renames it into place (the same
/// atomic-write helper used for `store-manifest.json`), so a concurrent
/// reader never observes a torn or truncated file.
pub fn save_branch_meta(data_dir: &Path, meta: &BranchMeta) -> std::io::Result<()> {
    let serialized = serialize_branch_meta(meta)?;
    save_branch_meta_serialized(data_dir, &serialized)
}

/// Advances the `last_synced_at` timestamp for `branch` in the project's
/// branch metadata, best-effort.
///
/// This is the entry point every successful sync path calls so `branch_list`
/// reflects real sync activity (previously `last_synced_at` only moved at
/// branch-add finalize, making the list misleading). It silently no-ops when
/// there is no branch metadata (single-DB mode / pre-branch projects) or when
/// `branch` is untracked — a sync of an untracked branch has no entry to touch.
/// The shared branch lock serializes this load-modify-save sequence with branch
/// add, removal, GC, and pending deletion recovery.
pub fn update_synced_timestamp(tracedecay_dir: &Path, branch: &str) {
    update_synced_timestamp_with(tracedecay_dir, branch, || {});
}

/// Atomically allocates and publishes the exact worktree/ref/OID identity
/// that produced a tracked branch graph.
///
/// The expected source is a compare-and-swap precondition: `None` means the
/// tracked entry must still be unpublished. Epoch allocation happens only
/// after that precondition succeeds while holding the shared branch lock, so
/// concurrent branch publications cannot reuse an epoch or overwrite newer
/// provenance.
pub fn publish_graph_source(
    tracedecay_dir: &Path,
    branch: &str,
    expected: Option<&BranchGraphSourceV1>,
    draft: BranchGraphSourceDraftV1,
) -> std::io::Result<BranchGraphSourcePublishOutcomeV1> {
    let _branch_lock = crate::branch::acquire_branch_lock_blocking(tracedecay_dir)
        .map_err(std::io::Error::other)?;
    let Some(mut meta) = load_branch_meta(tracedecay_dir) else {
        return Ok(BranchGraphSourcePublishOutcomeV1::BranchNotTracked);
    };
    let Some(entry) = meta.branches.get(branch) else {
        return Ok(BranchGraphSourcePublishOutcomeV1::BranchNotTracked);
    };
    let observed = entry.graph_source.clone();
    if observed.as_ref() != expected {
        return Ok(BranchGraphSourcePublishOutcomeV1::CompareAndSwapMiss { observed });
    }
    if let Some(source) = observed.filter(|source| source.matches_draft(&draft)) {
        return Ok(BranchGraphSourcePublishOutcomeV1::AlreadyPublished(source));
    }
    let last_epoch = meta
        .branches
        .values()
        .filter_map(|entry| entry.graph_source.as_ref())
        .map(|source| source.publication_epoch.get())
        .max()
        .unwrap_or(0);
    let epoch = last_epoch.checked_add(1).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "branch graph publication epoch is exhausted",
        )
    })?;
    let publication_epoch = BranchGraphPublicationEpochV1::new(epoch).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("branch graph publication epoch is invalid: {error}"),
        )
    })?;
    let source = BranchGraphSourceV1 {
        publication_epoch,
        project_id: draft.project_id,
        repository_id: draft.repository_id,
        worktree_id: draft.worktree_id,
        worktree_root: draft.worktree_root,
        reference: draft.reference,
        source_oid: draft.source_oid,
    };
    let previous_entry = entry.clone();
    meta.publish_graph_source(branch, source);
    let installed_entry = meta.branches.get(branch).cloned().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "branch graph source publication disappeared before commit",
        )
    })?;
    save_branch_meta(tracedecay_dir, &meta)?;
    Ok(BranchGraphSourcePublishOutcomeV1::Published(
        BranchGraphSourcePublicationV1 {
            branch: branch.to_owned(),
            previous_entry,
            installed_entry,
        },
    ))
}

/// Restores the entry immediately preceding one exact graph-source
/// publication. A no-match means another writer owns the metadata now and
/// must be preserved.
pub fn rollback_graph_source_publication(
    tracedecay_dir: &Path,
    publication: &BranchGraphSourcePublicationV1,
) -> std::io::Result<BranchGraphSourceRollbackOutcomeV1> {
    let _branch_lock = crate::branch::acquire_branch_lock_blocking(tracedecay_dir)
        .map_err(std::io::Error::other)?;
    let Some(mut meta) = load_branch_meta(tracedecay_dir) else {
        return Ok(BranchGraphSourceRollbackOutcomeV1::NoMatch);
    };
    let Some(entry) = meta.branches.get_mut(&publication.branch) else {
        return Ok(BranchGraphSourceRollbackOutcomeV1::NoMatch);
    };
    if entry != &publication.installed_entry {
        return Ok(BranchGraphSourceRollbackOutcomeV1::NoMatch);
    }
    *entry = publication.previous_entry.clone();
    save_branch_meta(tracedecay_dir, &meta)?;
    Ok(BranchGraphSourceRollbackOutcomeV1::Restored)
}

fn update_synced_timestamp_with(tracedecay_dir: &Path, branch: &str, after_lock: impl FnOnce()) {
    let Ok(_branch_lock) = crate::branch::acquire_branch_lock_blocking(tracedecay_dir) else {
        return;
    };
    after_lock();
    let Some(mut meta) = load_branch_meta(tracedecay_dir) else {
        return;
    };
    if !meta.is_tracked(branch) {
        return;
    }
    meta.touch_synced(branch);
    let _ = save_branch_meta(tracedecay_dir, &meta);
}

/// Returns the path to the `branches/` subdirectory, creating it if needed.
pub fn ensure_branches_dir(data_dir: &Path) -> std::io::Result<PathBuf> {
    let dir = data_dir.join("branches");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn now_unix_str() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs}")
}

/// Formats a UNIX timestamp string as a human-readable relative time.
pub fn format_timestamp(ts: &str) -> String {
    let Ok(secs) = ts.parse::<u64>() else {
        return ts.to_string();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let age = now.saturating_sub(secs);
    if age < 60 {
        "just now".to_string()
    } else if age < 3600 {
        format!("{}m ago", age / 60)
    } else if age < 86400 {
        format!("{}h {}m ago", age / 3600, (age % 3600) / 60)
    } else {
        format!("{}d ago", age / 86400)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn new_meta_has_default_branch() {
        let meta = BranchMeta::new("main");
        assert_eq!(meta.default_branch, "main");
        assert!(meta.is_tracked("main"));
        assert_eq!(meta.branches["main"].db_file, "tracedecay.db");
        assert!(meta.branches["main"].parent.is_none());
    }

    #[test]
    fn new_for_dir_tracks_current_db_file() {
        let meta = BranchMeta::new_for_dir(Path::new("/p/.tracedecay"), "main");
        assert_eq!(meta.branches["main"].db_file, "tracedecay.db");
    }

    #[test]
    fn add_and_remove_branch() {
        let mut meta = BranchMeta::new("main");
        meta.add_branch("feature/foo", "branches/feature_foo.db", "main");
        assert!(meta.is_tracked("feature/foo"));
        assert_eq!(meta.branches["feature/foo"].parent.as_deref(), Some("main"));

        let removed = meta.remove_branch("feature/foo");
        assert!(removed.is_some());
        assert!(!meta.is_tracked("feature/foo"));
    }

    #[test]
    fn cannot_remove_default_branch() {
        let mut meta = BranchMeta::new("main");
        assert!(meta.remove_branch("main").is_none());
    }

    #[test]
    fn parse_rejects_schema_mismatch_as_corrupt() {
        assert!(parse(r#"{"default_branch":"main","branches":{}}"#).is_err());
        assert!(parse("{not valid json").is_err());
        assert!(parse(r#"{"default_branch": 5}"#).is_err());
        assert!(parse("[]").is_err());
    }

    #[test]
    fn parse_rejects_semantically_invalid_branch_metadata() {
        for content in [
            r#"{"default_branch":"main","branches":{"main":{"db_file":"branches/main.db","created_at":"0","last_synced_at":"0"}}}"#,
            r#"{"default_branch":"main","branches":{"main":{"db_file":"tracedecay.db","parent":"main","created_at":"0","last_synced_at":"0"}}}"#,
            r#"{"default_branch":"main","branches":{"main":{"db_file":"tracedecay.db","created_at":"0","last_synced_at":"0"},"escape":{"db_file":"../escape.db","created_at":"0","last_synced_at":"0"}}}"#,
            r#"{"default_branch":"main","branches":{"main":{"db_file":"tracedecay.db","created_at":"0","last_synced_at":"0"},"left":{"db_file":"branches/shared.db","created_at":"0","last_synced_at":"0"},"right":{"db_file":"branches/shared.db","created_at":"0","last_synced_at":"0"}}}"#,
        ] {
            assert!(
                parse(content).is_err(),
                "accepted invalid metadata: {content}"
            );
        }
    }

    #[test]
    fn parse_accepts_branches_served_by_the_project_store() {
        // The single-store tracking shape: every branch references the
        // canonical main database while keeping its own lineage and
        // graph-source provenance.
        let content = r#"{"default_branch":"main","branches":{"main":{"db_file":"tracedecay.db","created_at":"0","last_synced_at":"0"},"feature/one":{"db_file":"tracedecay.db","parent":"main","created_at":"0","last_synced_at":"0"},"feature/two":{"db_file":"tracedecay.db","parent":"main","created_at":"0","last_synced_at":"0"}}}"#;

        let meta = parse(content).expect("single-store tracking metadata must parse");

        assert!(meta.branches["feature/one"].served_by_project_store());
        assert!(meta.branches["feature/two"].served_by_project_store());
        assert!(!meta.is_tracked("feature/three"));
        let legacy = parse(
            r#"{"default_branch":"main","branches":{"main":{"db_file":"tracedecay.db","created_at":"0","last_synced_at":"0"},"legacy":{"db_file":"branches/legacy.db","created_at":"0","last_synced_at":"0"}}}"#,
        )
        .expect("legacy private stores must keep parsing for collection");
        assert!(!legacy.branches["legacy"].served_by_project_store());
    }

    #[test]
    fn parse_accepts_case_insensitive_branch_database_extensions() {
        let mut meta = BranchMeta::new("main");
        meta.add_branch("legacy", "branches/legacy.DB", "main");

        let content = serde_json::to_string(&meta).unwrap();

        assert!(parse(&content).is_ok());
    }

    #[test]
    fn legacy_single_db_metadata_is_byte_stable() {
        let first = BranchMeta::for_legacy_single_db(Path::new("/profile/project"), "trunk");
        let second = BranchMeta::for_legacy_single_db(Path::new("/profile/project"), "trunk");

        assert_eq!(first.branches["trunk"].created_at, "0");
        assert_eq!(first.branches["trunk"].last_synced_at, "0");
        assert_eq!(
            serde_json::to_vec_pretty(&first).unwrap(),
            serde_json::to_vec_pretty(&second).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn load_rejects_symlinked_branch_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside.json");
        let data_dir = dir.path().join("data");
        std::fs::create_dir(&data_dir).unwrap();
        std::fs::write(
            &outside,
            serde_json::to_vec_pretty(&BranchMeta::new("main")).unwrap(),
        )
        .unwrap();
        std::os::unix::fs::symlink(&outside, data_dir.join(BRANCH_META_FILENAME)).unwrap();

        assert!(load_branch_meta(&data_dir).is_none());
    }

    #[test]
    fn parse_old_entry_defaults_gc_protected_to_false() {
        let meta = parse(
            r#"{"default_branch":"main","branches":{"main":{"db_file":"tracedecay.db","created_at":"1","last_synced_at":"1"}}}"#,
        )
        .unwrap();
        assert!(!meta.branches["main"].gc_protected);
    }

    #[test]
    fn update_synced_timestamp_advances_tracked_branch() {
        let dir = tempfile::tempdir().unwrap();
        let mut meta = BranchMeta::new("main");
        meta.add_branch("feature/foo", "branches/feature_foo.db", "main");
        // Backdate so the advance is observable regardless of same-second timing.
        meta.branches.get_mut("feature/foo").unwrap().last_synced_at = "1000".to_string();
        save_branch_meta(dir.path(), &meta).unwrap();

        update_synced_timestamp(dir.path(), "feature/foo");

        let reloaded = load_branch_meta(dir.path()).unwrap();
        let synced: u64 = reloaded.branches["feature/foo"]
            .last_synced_at
            .parse()
            .unwrap();
        assert!(synced > 1000, "last_synced_at should advance, got {synced}");
    }

    #[test]
    fn update_synced_timestamp_holds_shared_branch_lock_during_load_modify_save() {
        let dir = tempfile::tempdir().unwrap();
        let mut meta = BranchMeta::new("main");
        meta.add_branch("feature/foo", "branches/feature_foo.db", "main");
        save_branch_meta(dir.path(), &meta).unwrap();
        let mut observed_contention = false;

        update_synced_timestamp_with(dir.path(), "feature/foo", || {
            let error = crate::branch::try_acquire_branch_add_lock(dir.path())
                .expect_err("timestamp update must already own the shared branch lock");
            observed_contention = matches!(error, crate::errors::TraceDecayError::SyncLock { .. });
        });

        assert!(observed_contention);
        assert!(
            load_branch_meta(dir.path())
                .unwrap()
                .is_tracked("feature/foo")
        );
    }

    #[test]
    fn update_synced_timestamp_noops_for_unknown_branch() {
        let dir = tempfile::tempdir().unwrap();
        let meta = BranchMeta::new("main");
        save_branch_meta(dir.path(), &meta).unwrap();

        // Untracked branch: must not create an entry or error.
        update_synced_timestamp(dir.path(), "does/not/exist");

        let reloaded = load_branch_meta(dir.path()).unwrap();
        assert!(!reloaded.is_tracked("does/not/exist"));
    }

    #[test]
    fn update_synced_timestamp_noops_without_meta() {
        let dir = tempfile::tempdir().unwrap();
        // No branch-meta.json present; must silently no-op.
        update_synced_timestamp(dir.path(), "main");
        assert!(load_branch_meta(dir.path()).is_none());
    }

    #[test]
    fn graph_source_publication_round_trips_exact_worktree_identity() {
        let dir = tempfile::tempdir().unwrap();
        let meta = BranchMeta::new_for_dir(dir.path(), "main");
        save_branch_meta(dir.path(), &meta).unwrap();
        let source = BranchGraphSourceV1 {
            publication_epoch: BranchGraphPublicationEpochV1::new(1).unwrap(),
            project_id: "project.fixture".to_owned(),
            repository_id: "repository.fixture".to_owned(),
            worktree_id: "worktree.linked".to_owned(),
            worktree_root: "/fixture/linked".to_owned(),
            reference: "refs/heads/main".to_owned(),
            source_oid: "a".repeat(40),
        };
        let draft = BranchGraphSourceDraftV1 {
            project_id: source.project_id.clone(),
            repository_id: source.repository_id.clone(),
            worktree_id: source.worktree_id.clone(),
            worktree_root: source.worktree_root.clone(),
            reference: source.reference.clone(),
            source_oid: source.source_oid.clone(),
        };
        let BranchGraphSourcePublishOutcomeV1::Published(publication) =
            publish_graph_source(dir.path(), "main", None, draft.clone()).unwrap()
        else {
            panic!("first exact source publication must install metadata")
        };
        assert_eq!(publication.source(), Some(&source));
        assert_eq!(
            publish_graph_source(dir.path(), "main", Some(&source), draft).unwrap(),
            BranchGraphSourcePublishOutcomeV1::AlreadyPublished(source.clone())
        );
        assert_eq!(
            load_branch_meta(dir.path())
                .unwrap()
                .branches
                .get("main")
                .unwrap()
                .graph_source,
            Some(source)
        );
    }

    #[test]
    fn graph_source_rollback_requires_the_exact_installed_entry() {
        let dir = tempfile::tempdir().unwrap();
        let meta = BranchMeta::new_for_dir(dir.path(), "main");
        save_branch_meta(dir.path(), &meta).unwrap();
        let draft = BranchGraphSourceDraftV1 {
            project_id: "project.fixture".to_owned(),
            repository_id: "repository.fixture".to_owned(),
            worktree_id: "worktree.fixture".to_owned(),
            worktree_root: "/fixture".to_owned(),
            reference: "refs/heads/main".to_owned(),
            source_oid: "a".repeat(40),
        };
        let BranchGraphSourcePublishOutcomeV1::Published(publication) =
            publish_graph_source(dir.path(), "main", None, draft.clone()).unwrap()
        else {
            panic!("publication must install a rollback authority")
        };
        assert_eq!(
            rollback_graph_source_publication(dir.path(), &publication).unwrap(),
            BranchGraphSourceRollbackOutcomeV1::Restored
        );
        assert!(
            load_branch_meta(dir.path()).unwrap().branches["main"]
                .graph_source
                .is_none(),
            "the exact rollback restores the unpublished entry"
        );

        let BranchGraphSourcePublishOutcomeV1::Published(publication) =
            publish_graph_source(dir.path(), "main", None, draft).unwrap()
        else {
            panic!("republication must install a new rollback authority")
        };
        let mut foreign = load_branch_meta(dir.path()).unwrap();
        foreign.branches.get_mut("main").unwrap().last_synced_at = "foreign".to_owned();
        save_branch_meta(dir.path(), &foreign).unwrap();
        assert_eq!(
            rollback_graph_source_publication(dir.path(), &publication).unwrap(),
            BranchGraphSourceRollbackOutcomeV1::NoMatch,
            "a foreign metadata write must survive a stale rollback"
        );
        assert_eq!(
            load_branch_meta(dir.path()).unwrap().branches["main"].last_synced_at,
            "foreign"
        );
    }

    #[test]
    fn concurrent_graph_source_publications_allocate_distinct_epochs() {
        let dir = tempfile::tempdir().unwrap();
        let mut meta = BranchMeta::new_for_dir(dir.path(), "main");
        meta.add_branch("feature/one", crate::config::DB_FILENAME, "main");
        meta.add_branch("feature/two", crate::config::DB_FILENAME, "main");
        save_branch_meta(dir.path(), &meta).unwrap();

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let first_dir = dir.path().to_path_buf();
        let first_barrier = std::sync::Arc::clone(&barrier);
        let second_dir = dir.path().to_path_buf();
        let second_barrier = std::sync::Arc::clone(&barrier);
        let first = std::thread::spawn(move || {
            first_barrier.wait();
            publish_graph_source(
                &first_dir,
                "feature/one",
                None,
                BranchGraphSourceDraftV1 {
                    project_id: "project.fixture".to_owned(),
                    repository_id: "repository.fixture".to_owned(),
                    worktree_id: "worktree.one".to_owned(),
                    worktree_root: "/fixture/one".to_owned(),
                    reference: "refs/heads/feature/one".to_owned(),
                    source_oid: "a".repeat(40),
                },
            )
        });
        let second = std::thread::spawn(move || {
            second_barrier.wait();
            publish_graph_source(
                &second_dir,
                "feature/two",
                None,
                BranchGraphSourceDraftV1 {
                    project_id: "project.fixture".to_owned(),
                    repository_id: "repository.fixture".to_owned(),
                    worktree_id: "worktree.two".to_owned(),
                    worktree_root: "/fixture/two".to_owned(),
                    reference: "refs/heads/feature/two".to_owned(),
                    source_oid: "b".repeat(40),
                },
            )
        });

        assert!(matches!(
            first.join().unwrap().unwrap(),
            BranchGraphSourcePublishOutcomeV1::Published(_)
        ));
        assert!(matches!(
            second.join().unwrap().unwrap(),
            BranchGraphSourcePublishOutcomeV1::Published(_)
        ));

        let meta = load_branch_meta(dir.path()).unwrap();
        let first_epoch = meta.branches["feature/one"]
            .graph_source
            .as_ref()
            .unwrap()
            .publication_epoch
            .get();
        let second_epoch = meta.branches["feature/two"]
            .graph_source
            .as_ref()
            .unwrap()
            .publication_epoch
            .get();
        assert_ne!(first_epoch, second_epoch);
        assert!(
            [first_epoch, second_epoch]
                .into_iter()
                .all(|epoch| epoch <= 2),
            "the locked publisher must allocate the first two epochs, got {first_epoch} and {second_epoch}"
        );
    }

    #[test]
    fn roundtrip_json() {
        let mut meta = BranchMeta::new("main");
        meta.add_branch("feature/bar", "branches/feature_bar.db", "main");
        let json = serde_json::to_string(&meta).unwrap();
        let parsed: BranchMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.default_branch, "main");
        assert!(parsed.is_tracked("feature/bar"));
    }
}
