//! Branch metadata persistence for multi-branch indexing.
//!
//! Stores tracking information in `branch-meta.json` inside the project data
//! dir.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::storage::{BRANCH_META_FILENAME, PrivateStoreIo};

/// Metadata for a single tracked branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchEntry {
    /// Relative path to the DB file, such as `tracedecay.db` or
    /// `branches/feature_foo.db`.
    pub db_file: String,
    /// Branch this was copied from (None for the default branch).
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
        && (!relative.starts_with("branches")
            || !relative
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("db")))
    {
        return Err(format!(
            "non-default branch '{name}' database path '{}' must be under 'branches/' with a .db extension",
            entry.db_file
        ));
    }
    Ok(())
}

/// Parses `branch-meta.json` content into [`BranchMeta`].
///
/// This is the canonical definition of "corrupt branch metadata": anything
/// this rejects — invalid JSON *or* valid JSON with the wrong schema — makes
/// the runtime fall back to single-DB mode. Every consumer (loading at
/// runtime, quarantining in the post-update health pass) must go through this
/// one predicate so they agree on what corrupt means.
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
    update_synced_timestamp_with_lock(
        tracedecay_dir,
        branch,
        crate::branch::acquire_branch_lock_blocking,
    );
}

/// Updates branch metadata with a caller-supplied shared-lock policy.
///
/// The root compatibility façade supplies its pending branch-admin recovery
/// gate; standalone kernel callers use the raw kernel lock above.
#[doc(hidden)]
pub fn update_synced_timestamp_with_lock(
    tracedecay_dir: &Path,
    branch: &str,
    acquire_branch_lock: fn(&Path) -> crate::errors::Result<std::fs::File>,
) {
    update_synced_timestamp_with_lock_and(tracedecay_dir, branch, acquire_branch_lock, || {});
}

#[cfg(test)]
fn update_synced_timestamp_with(tracedecay_dir: &Path, branch: &str, after_lock: impl FnOnce()) {
    update_synced_timestamp_with_lock_and(
        tracedecay_dir,
        branch,
        crate::branch::acquire_branch_lock_blocking,
        after_lock,
    );
}

fn update_synced_timestamp_with_lock_and(
    tracedecay_dir: &Path,
    branch: &str,
    acquire_branch_lock: fn(&Path) -> crate::errors::Result<std::fs::File>,
    after_lock: impl FnOnce(),
) {
    let Ok(_branch_lock) = acquire_branch_lock(tracedecay_dir) else {
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
            r#"{"default_branch":"main","branches":{"main":{"db_file":"tracedecay.db","created_at":"0","last_synced_at":"0"},"duplicate":{"db_file":"tracedecay.db","created_at":"0","last_synced_at":"0"}}}"#,
        ] {
            assert!(
                parse(content).is_err(),
                "accepted invalid metadata: {content}"
            );
        }
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
    fn roundtrip_json() {
        let mut meta = BranchMeta::new("main");
        meta.add_branch("feature/bar", "branches/feature_bar.db", "main");
        let json = serde_json::to_string(&meta).unwrap();
        let parsed: BranchMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.default_branch, "main");
        assert!(parsed.is_tracked("feature/bar"));
    }
}
