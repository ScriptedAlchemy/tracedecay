// Rust guideline compliant 2025-10-17
//! Central orchestrator for the code graph.
//!
//! This module root holds the [`TraceDecay`] struct and its shared result
//! types; the behavior is implemented in focused submodules:
//! [`lifecycle`] (init/open/branch tracking), [`indexing`] (index/sync),
//! [`scan`] (file walking), [`edits`] (anchored source edits), [`queries`]
//! (read-side graph queries), [`diagnostics`] (branch state), [`facts`]
//! (session memory), and [`locking`] (dirty sentinel + sync lock).
use std::path::PathBuf;

use crate::config::TraceDecayConfig;
use crate::db::Database;
use crate::errors::Result;
use crate::extraction::LanguageRegistry;
use crate::global_db::GlobalDb;
use crate::storage::{self, StoreLayout};

mod diagnostics;
mod edits;
mod facts;
mod indexing;
mod lifecycle;
mod locking;
mod queries;
mod scan;

pub use diagnostics::{BranchDiagnostics, TrackedBranchDiagnostic};

#[doc(hidden)]
pub use locking::{try_acquire_sync_lock, SyncLockGuard};

/// Central orchestrator that coordinates all subsystems of the code graph.
///
/// Provides a high-level API for initializing, indexing, querying, and
/// syncing a Rust codebase's semantic knowledge graph.
pub struct TraceDecay {
    db: Database,
    config: TraceDecayConfig,
    project_root: PathBuf,
    store_layout: StoreLayout,
    open_options: TraceDecayOpenOptions,
    registry: LanguageRegistry,
    /// The active git branch (None if detached HEAD or not a git repo).
    active_branch: Option<String>,
    /// The branch whose DB is actually being served (may differ from `active_branch` on fallback).
    serving_branch: Option<String>,
    /// Set when serving from a fallback (ancestor) DB instead of the exact branch.
    fallback_warning: Option<String>,
    read_only: bool,
}

#[derive(Debug, Clone, Default)]
pub struct TraceDecayOpenOptions {
    pub profile_root: Option<PathBuf>,
    pub global_db_path: Option<PathBuf>,
}

impl TraceDecayOpenOptions {
    fn resolved_profile_root(&self) -> Result<PathBuf> {
        if let Some(profile_root) = &self.profile_root {
            return Ok(profile_root.clone());
        }
        if let Some(parent) = self
            .global_db_path
            .as_deref()
            .and_then(std::path::Path::parent)
        {
            return Ok(parent.to_path_buf());
        }
        storage::default_profile_root()
    }

    async fn open_global_db(&self) -> Option<GlobalDb> {
        match self.global_db_path.as_deref() {
            Some(path) => GlobalDb::open_at(path).await,
            None => GlobalDb::open().await,
        }
    }
}

/// Result of a full indexing operation.
pub struct IndexResult {
    /// Number of files scanned and indexed.
    pub file_count: usize,
    /// Total number of nodes extracted.
    pub node_count: usize,
    /// Total number of edges (extracted + resolved).
    pub edge_count: usize,
    /// Time taken in milliseconds.
    pub duration_ms: u64,
}

/// Result of an incremental sync operation.
#[derive(Debug)]
pub struct SyncResult {
    /// Number of newly added files.
    pub files_added: usize,
    /// Number of modified (re-indexed) files.
    pub files_modified: usize,
    /// Number of removed files.
    pub files_removed: usize,
    /// Time taken in milliseconds.
    pub duration_ms: u64,
    /// Paths of added files (populated only when doctor mode is requested).
    pub added_paths: Vec<String>,
    /// Paths of modified files (populated only when doctor mode is requested).
    pub modified_paths: Vec<String>,
    /// Paths of removed files (populated only when doctor mode is requested).
    pub removed_paths: Vec<String>,
    /// Files that were found on disk but could not be read (path, error message).
    pub skipped_paths: Vec<(String, String)>,
}

/// Returns the current UNIX timestamp in seconds.
pub fn current_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Returns `true` if the file path looks like a test file.
pub fn is_test_file(path: &str) -> bool {
    let test_segments = [
        "test/",
        "tests/",
        "__tests__/",
        "spec/",
        "e2e/",
        ".test.",
        ".spec.",
        "_test.",
        "_spec.",
    ];
    let lower = path.to_ascii_lowercase();
    test_segments.iter().any(|s| lower.contains(s))
}
