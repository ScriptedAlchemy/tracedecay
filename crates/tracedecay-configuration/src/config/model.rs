//! Sync, telemetry, and retention policy shapes plus project path helpers.
//!
//! Runtime values are decoded from a pinned snapshot in [`crate::config`].

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracedecay_contracts::storage::compaction::CompactionThresholdConfig;
use tracedecay_domain::errors::{Result, TraceDecayError};
pub use tracedecay_runtime_core::config::brand_env;
use tracedecay_runtime_core::config::{discover_project_root, is_generated_dir_segment};

/// Returns `true` if any component of `path` is a generated/vendored
/// directory segment, or `path` itself carries a minified-asset suffix
/// (`app.min.js`, `app.min.css`, ...).
///
/// Path-level, including individual file paths, so callers can filter a flat
/// list of file paths in one pass.
pub fn is_generated_path_segment(path: &str) -> bool {
    has_minified_suffix(path) || path.split('/').any(is_generated_dir_segment)
}

/// `true` for paths like `app.min.js` / `app.min.css.map`: a `.min.`
/// component followed by at least one more character.
fn has_minified_suffix(path: &str) -> bool {
    path.rfind(".min.").is_some_and(|idx| idx + 5 < path.len())
}

fn default_thirty_day_retention() -> Option<u64> {
    Some(30)
}

fn default_retention_interval_hours() -> u64 {
    24
}

fn default_compaction_threshold() -> Option<CompactionThresholdConfig> {
    Some(CompactionThresholdConfig::default())
}

/// The daemon retention/compaction policy tree (Plan 38). Safe, bounded
/// maintenance is active by default for proven orphan stores, incident
/// debris, redundant projection-durable session copies, and free-page bloat.
/// Lossy session/evidence deletion remains disabled and soft budgets remain
/// owner-configured findings only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetentionConfig {
    /// Session-store (LCM raw/projected) retention windows.
    #[serde(default)]
    pub session_lcm: tracedecay_lcm::LcmRetentionConfig,
    /// Observation-evidence generation-scoped retention windows.
    #[serde(default)]
    pub observation: tracedecay_global_db::observation::retention::ObservationRetentionConfig,
    /// Orphan profile-sharded store collection window (days). `None` disables
    /// the sweep; the Doctor surface still reports findings read-only.
    #[serde(default = "default_thirty_day_retention")]
    pub orphan_store_gc_days: Option<u64>,
    /// Incremental-vacuum compaction trigger. `None` disables compaction.
    #[serde(default = "default_compaction_threshold")]
    pub compaction: Option<CompactionThresholdConfig>,
    /// Owner-configured soft byte budgets keyed by exact logical store key.
    /// Missing entries mean no budget was configured for that store.
    #[serde(default)]
    pub store_soft_budgets_bytes: BTreeMap<String, u64>,
    /// Cadence between daemon retention passes (hours).
    #[serde(default = "default_retention_interval_hours")]
    pub interval_hours: u64,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            session_lcm: tracedecay_lcm::LcmRetentionConfig::default(),
            observation:
                tracedecay_global_db::observation::retention::ObservationRetentionConfig::default(),
            orphan_store_gc_days: default_thirty_day_retention(),
            compaction: default_compaction_threshold(),
            store_soft_budgets_bytes: BTreeMap::new(),
            interval_hours: default_retention_interval_hours(),
        }
    }
}

impl RetentionConfig {
    pub fn store_soft_budget(
        &self,
        store: &str,
    ) -> Result<Option<tracedecay_contracts::storage::StoreSizeBudgetV1>> {
        let Some(bytes) = self.store_soft_budgets_bytes.get(store).copied() else {
            return Ok(None);
        };
        let budget = tracedecay_contracts::storage::StoreSizeBudgetV1 {
            store: tracedecay_contracts::storage::StoreKeyV1::new(store.to_owned())
                .map_err(|error| config_error(error.to_string()))?,
            soft_limit_bytes: tracedecay_contracts::storage::StorageByteSizeV1(bytes),
        };
        budget
            .validate()
            .map_err(|error| config_error(error.to_string()))?;
        Ok(Some(budget))
    }
}

/// Floor for the PR-autotrack poll interval; polls faster than this hammer the
/// GitHub API / `git ls-remote` needlessly, so any smaller configured value is
/// clamped up to this.
pub const MIN_AUTO_TRACK_PR_POLL_SECS: u64 = 60;

#[derive(Debug, Clone, PartialEq)]
pub struct TelemetryConfig {
    pub timings: bool,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self { timings: true }
    }
}

/// Auto-sync / index-freshness knobs.
///
/// Runtime consumers receive these values only from a pinned resolved
/// configuration snapshot; no environment layer overrides them.
#[derive(Debug, Clone, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "Independent sync admission switches are configuration choices, not mutually exclusive states"
)]
pub struct SyncConfig {
    /// Enable the daemon git-metadata watcher.
    pub auto_watch: bool,
    /// Admit linked worktrees into the daemon watcher without an explicit
    /// branch-indexing request.
    pub watch_linked_worktrees: bool,
    /// Per-project quiet-period debounce before a watcher-triggered sync (ms).
    pub watch_debounce_ms: u64,
    /// Maximum time a watcher-triggered sync can be deferred by debounce (ms).
    pub watch_max_delay_ms: u64,
    /// Maximum number of recently-seen projects the watcher registers.
    pub watch_max_projects: usize,
    /// Enable non-blocking sync-on-read for query tools.
    pub read_refresh: bool,
    /// Cooldown between read-triggered background refreshes (seconds).
    pub read_cooldown_secs: u64,
    /// Fire a catch-up sync on session start.
    pub session_start_sync: bool,
    /// Staleness threshold above which session-start sync runs (seconds).
    pub session_start_stale_threshold_secs: u64,
    /// Daemon backstop scheduler interval (minutes); 0 disables it.
    pub backstop_interval_mins: u64,
    /// Diff-scoped syncs above this many changed files escalate to a full sync.
    pub full_sync_escalation_files: usize,
    /// Daemon-wide cap on concurrent syncs.
    pub max_concurrent_syncs: usize,
    /// Grace period before a dead tracked-branch store is GC'd (days).
    pub branch_gc_days: u64,
    /// Auto-initialise never-indexed repos on first contact.
    pub auto_init: bool,
    /// Enable the daemon PR-branch auto-tracking mode: when on, the daemon polls
    /// the repo's GitHub remote for open PRs and tracks/untracks each PR head
    /// branch through the normal branch-tracking machinery.
    pub auto_track_pr_branches: bool,
    /// Poll cadence (seconds) for PR-branch auto-tracking discovery. Clamped up
    /// to [`MIN_AUTO_TRACK_PR_POLL_SECS`] at read time.
    pub auto_track_pr_poll_secs: u64,
    /// Daemon retention/compaction policy tree (Plan 38).
    pub retention: RetentionConfig,
}

impl SyncConfig {
    /// The effective PR-autotrack poll interval, never below the safety floor.
    #[must_use]
    pub fn effective_auto_track_pr_poll_secs(&self) -> u64 {
        self.auto_track_pr_poll_secs
            .max(MIN_AUTO_TRACK_PR_POLL_SECS)
    }
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            auto_watch: false,
            watch_linked_worktrees: false,
            watch_debounce_ms: 2000,
            watch_max_delay_ms: 30000,
            watch_max_projects: 32,
            read_refresh: true,
            read_cooldown_secs: 30,
            session_start_sync: true,
            session_start_stale_threshold_secs: 600,
            backstop_interval_mins: 15,
            full_sync_escalation_files: 500,
            max_concurrent_syncs: 2,
            branch_gc_days: 14,
            auto_init: true,
            auto_track_pr_branches: false,
            auto_track_pr_poll_secs: 300,
            retention: RetentionConfig::default(),
        }
    }
}

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

/// Resolves a CLI path argument to an absolute `PathBuf`.
///
/// If `path` is `Some`, uses that value; otherwise falls back to the current
/// working directory.
pub fn resolve_path(path: Option<String>) -> PathBuf {
    let path = match path {
        Some(p) => PathBuf::from(p),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    absolutize_path(path)
}

fn absolutize_path(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

/// Like [`resolve_path`], but when `path` is `None` it walks up from `cwd`
/// to find the nearest initialised `TraceDecay` project before falling back to
/// `cwd` itself.
///
/// Used by `serve`, `sync`, and `status`. NOT used by `init` (which must
/// create a fresh project at the target directory).
pub fn resolve_path_with_discovery(path: Option<String>) -> PathBuf {
    if let Some(p) = path {
        PathBuf::from(p)
    } else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        discover_project_root(&cwd)
            .or_else(|| tracedecay_runtime_core::worktree::git_worktree_root(&cwd))
            .unwrap_or(cwd)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
