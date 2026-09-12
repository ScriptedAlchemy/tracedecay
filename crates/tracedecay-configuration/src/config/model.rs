//! Legacy `config.json` model, defaults, validation, and path policy.
//!
//! Shared runtime pin settings stay in [`crate::config`]; this module owns the
//! serde/migration shape and the include/exclude/gitignore helpers every
//! caller imports directly.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use glob::Pattern;
use serde::{Deserialize, Serialize};
use tracedecay_contracts::storage::compaction::CompactionThresholdConfig;
use tracedecay_domain::configuration::ConfigurationSnapshotV1;
use tracedecay_domain::configuration::{
    SYNC_AUTO_INIT_SETTING_KEY, SYNC_AUTO_WATCH_SETTING_KEY,
    SYNC_BACKSTOP_INTERVAL_MINS_SETTING_KEY, SYNC_BRANCH_GC_DAYS_SETTING_KEY,
    SYNC_FULL_SYNC_ESCALATION_FILES_SETTING_KEY, SYNC_MAX_CONCURRENT_SYNCS_SETTING_KEY,
    SYNC_ORPHAN_DB_GC_DAYS_SETTING_KEY, SYNC_READ_COOLDOWN_SECS_SETTING_KEY,
    SYNC_READ_REFRESH_SETTING_KEY, SYNC_SESSION_START_STALE_THRESHOLD_SECS_SETTING_KEY,
    SYNC_SESSION_START_SYNC_SETTING_KEY, SYNC_WATCH_DEBOUNCE_MS_SETTING_KEY,
    SYNC_WATCH_LINKED_WORKTREES_SETTING_KEY, SYNC_WATCH_MAX_DELAY_MS_SETTING_KEY,
    SYNC_WATCH_MAX_PROJECTS_SETTING_KEY,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
pub use tracedecay_runtime_core::config::brand_env;
use tracedecay_runtime_core::config::{
    GENERATED_DIR_SEGMENTS, active_data_dir_name, discover_project_root, get_tracedecay_dir,
    is_generated_dir_segment,
};
use tracedecay_semantic_contracts::SemanticConfig;

use super::{
    PinnedRuntimeConfiguration, optional_text_setting, required_bool, required_unsigned,
    required_usize,
};

/// Name of the legacy configuration migration input stored inside the data
/// directory. It is not a runtime authority and production code must never
/// rewrite it.
pub const CONFIG_FILENAME: &str = "config.json";

/// Atomic daemon retention/compaction policy tree.
///
/// The value is canonical JSON for [`RetentionConfig`]. Keeping the session
/// (LCM), observation-evidence, orphan-store, debris, and compaction windows
/// under one setting keeps the retention engines threaded as a single
/// versioned unit the daemon backstop reads, mirroring the semantic key. Absent
/// or unset resolves to [`RetentionConfig::default`]'s bounded safe policy.
pub const SYNC_RETENTION_SETTING_KEY: &str = "sync.retention.v1";

/// Returns `true` if any component of `path` is a generated/vendored
/// directory segment, or `path` itself carries a minified-asset suffix
/// (`app.min.js`, `app.min.css`, ...) — mirrors the `**/*.min.*` default
/// exclude pattern built by [`default_exclude_patterns`].
///
/// Path-level (not just directory-level) so callers can filter a flat list
/// of file paths in one pass, e.g. the redundancy scanner's candidate list.
pub fn is_generated_path_segment(path: &str) -> bool {
    has_minified_suffix(path) || path.split('/').any(is_generated_dir_segment)
}

/// `true` for paths like `app.min.js` / `app.min.css.map` — a `.min.`
/// component followed by at least one more character.
fn has_minified_suffix(path: &str) -> bool {
    path.rfind(".min.").is_some_and(|idx| idx + 5 < path.len())
}

/// Default glob-pattern exclude list for [`TraceDecayConfig::default`].
///
/// Built from [`GENERATED_DIR_SEGMENTS`] (both the `segment/**` root form
/// and the `**/segment/**` nested form, since a generated directory can
/// appear at the project root or anywhere below it) plus site-local
/// additions that intentionally are *not* part of the shared segment set:
///
/// - `.git/**`, `.tracedecay/**` — VCS and `TraceDecay`'s own metadata dirs;
///   these are tool/repo bookkeeping, not generated *code*, so they stay
///   local to the config's default patterns rather than joining
///   [`GENERATED_DIR_SEGMENTS`] (which the migrate/scan/redundancy call
///   sites also consult for non-config-driven decisions).
/// - `bin/**` — historically excluded here by default, but not treated as
///   "generated" elsewhere: a `bin/` directory can hold real source in some
///   project layouts, so it isn't added to the shared segment list.
/// - `**/*.min.*` — mirrors [`is_generated_path_segment`]'s suffix check.
fn default_exclude_patterns() -> Vec<String> {
    let mut patterns: Vec<String> = vec![
        ".git/**".to_string(),
        ".tracedecay/**".to_string(),
        "bin/**".to_string(),
        "**/*.min.*".to_string(),
    ];
    for segment in GENERATED_DIR_SEGMENTS {
        patterns.push(format!("{segment}/**"));
        patterns.push(format!("**/{segment}/**"));
    }
    patterns
}

/// Legacy `config.json` representation and the materialized shape used by an
/// already-pinned resolved configuration snapshot.
///
/// `version` and `root_dir` are legacy migration metadata only. Every runtime
/// setting below is sourced from [`ConfigurationSnapshotV1`] before a project
/// opens; serializing this type is retained solely for migration fixtures and
/// backwards-compatible legacy input decoding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "Independent legacy configuration switches retain their serialized migration shape"
)]
pub struct TraceDecayConfig {
    /// Schema version of the configuration.
    pub version: u32,
    /// Root directory of the project being indexed.
    pub root_dir: String,
    /// Glob patterns for files to exclude during indexing.
    pub exclude: Vec<String>,
    /// Glob patterns for paths to include despite the default hidden-directory,
    /// generated-directory, and gitignore filters. For example,
    /// `[".github/**"]` indexes files under `.github/` that would otherwise be
    /// skipped.
    #[serde(default)]
    pub include: Vec<String>,
    /// Maximum file size in bytes; files larger than this are skipped.
    pub max_file_size: u64,
    /// Whether to extract doc comments from source files.
    pub extract_docstrings: bool,
    /// Whether to track call-site locations for edges.
    pub track_call_sites: bool,
    /// Whether to respect `.gitignore` rules when scanning files.
    #[serde(default = "default_git_ignore")]
    pub git_ignore: bool,
    /// Whether a cold `tracedecay_diagnostics` call prewarms in the background
    /// (detached dependency build + immediate `warming` status) instead of
    /// blocking for minutes. Environment precedence is resolved into the
    /// pinned snapshot during legacy migration, never during a tool call.
    #[serde(default)]
    pub diagnostics_prewarm: bool,
    /// Whether the persistent native code graph may activate for this project.
    /// Disabling it leaves exact and lexical retrieval available and reports
    /// graph capability as unavailable.
    #[serde(default = "default_native_graph_activation")]
    pub native_graph_activation: bool,
    /// Optional installed local semantic profile selection. Missing or
    /// unavailable semantics never disables exact, lexical, or graph search.
    #[serde(default)]
    pub semantic: SemanticConfig,
    /// Index-freshness auto-sync settings (git-metadata watcher, serve-stale,
    /// branch lifecycle). Absent in older `config.json` files, so defaulted.
    #[serde(default)]
    pub sync: SyncConfig,
    /// Analytics telemetry settings. Absent in older `config.json` files, so
    /// defaulted.
    #[serde(default)]
    pub telemetry: TelemetryConfig,
}

fn default_git_ignore() -> bool {
    true
}

fn default_native_graph_activation() -> bool {
    true
}

fn default_sync_auto_watch() -> bool {
    false
}
fn default_sync_watch_linked_worktrees() -> bool {
    false
}
fn default_sync_watch_debounce_ms() -> u64 {
    2000
}
fn default_sync_watch_max_delay_ms() -> u64 {
    30000
}
fn default_sync_watch_max_projects() -> usize {
    32
}
fn default_sync_read_refresh() -> bool {
    true
}
fn default_sync_read_cooldown_secs() -> u64 {
    30
}
fn default_sync_session_start_sync() -> bool {
    true
}
fn default_sync_session_start_stale_threshold_secs() -> u64 {
    600
}
fn default_sync_backstop_interval_mins() -> u64 {
    15
}
fn default_sync_full_sync_escalation_files() -> usize {
    500
}
fn default_sync_max_concurrent_syncs() -> usize {
    2
}
fn default_sync_branch_gc_days() -> u64 {
    14
}
fn default_sync_orphan_db_gc_days() -> u64 {
    7
}
fn default_sync_auto_init() -> bool {
    true
}
fn default_sync_auto_track_pr_branches() -> bool {
    false
}
fn default_sync_auto_track_pr_poll_secs() -> u64 {
    300
}
fn default_retention_interval_hours() -> u64 {
    24
}

fn default_orphan_store_gc_days() -> Option<u64> {
    Some(30)
}

fn default_incident_debris_retention_days() -> Option<u64> {
    Some(30)
}

fn default_compaction_threshold() -> Option<CompactionThresholdConfig> {
    Some(CompactionThresholdConfig::default())
}

/// The daemon retention/compaction policy tree (Plan 38). Safe, bounded
/// maintenance is active by default for proven orphan stores, quarantined
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
    #[serde(default = "default_orphan_store_gc_days")]
    pub orphan_store_gc_days: Option<u64>,
    /// Retention window for quarantined recovery/corruption artifacts (days).
    /// `None` disables collection while Doctor continues surfacing debris.
    #[serde(default = "default_incident_debris_retention_days")]
    pub incident_debris_retention_days: Option<u64>,
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
            orphan_store_gc_days: default_orphan_store_gc_days(),
            incident_debris_retention_days: default_incident_debris_retention_days(),
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

    /// Validate collection windows and the compaction trigger. Immediate
    /// collection and ratios outside the unit interval are rejected.
    pub(crate) fn validate(&self) -> Result<()> {
        if self.orphan_store_gc_days == Some(0) {
            return Err(config_error(
                "retention orphan_store_gc_days must be greater than zero",
            ));
        }
        if self.incident_debris_retention_days == Some(0) {
            return Err(config_error(
                "retention incident_debris_retention_days must be greater than zero",
            ));
        }
        if let Some(compaction) = &self.compaction
            && (!compaction.free_page_ratio_threshold.is_finite()
                || compaction.free_page_ratio_threshold <= 0.0
                || compaction.free_page_ratio_threshold > 1.0)
        {
            return Err(config_error(
                "retention compaction free_page_ratio_threshold must be within (0.0, 1.0]",
            ));
        }
        for (store, bytes) in &self.store_soft_budgets_bytes {
            tracedecay_contracts::storage::StoreKeyV1::new(store.clone()).map_err(|_| {
                config_error(format!(
                    "retention store soft budget key '{store}' is not a valid StoreKeyV1"
                ))
            })?;
            if *bytes == 0 {
                return Err(config_error(format!(
                    "retention store soft budget for '{store}' must be greater than zero"
                )));
            }
        }
        Ok(())
    }
}

/// Floor for the PR-autotrack poll interval; polls faster than this hammer the
/// GitHub API / `git ls-remote` needlessly, so any smaller configured value is
/// clamped up to this.
pub const MIN_AUTO_TRACK_PR_POLL_SECS: u64 = 60;

fn default_telemetry_timings() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelemetryConfig {
    #[serde(default = "default_telemetry_timings")]
    pub timings: bool,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            timings: default_telemetry_timings(),
        }
    }
}

/// Auto-sync / index-freshness knobs in the legacy migration shape.
///
/// Runtime consumers receive these values only from a pinned resolved
/// configuration snapshot. `TRACEDECAY_SYNC_*` values are decoded as an
/// explicit legacy environment layer during migration, rather than being read
/// independently by each adapter.
///
/// Every field carries a `#[serde(default = ...)]` so that a partial JSON
/// object (only some keys present) still deserializes, and a missing `sync`
/// key entirely falls back to [`SyncConfig::default`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "Independent sync admission switches are configuration choices, not mutually exclusive states"
)]
pub struct SyncConfig {
    /// Enable the daemon git-metadata watcher.
    #[serde(default = "default_sync_auto_watch")]
    pub auto_watch: bool,
    /// Admit linked worktrees into the daemon watcher without an explicit
    /// branch-indexing request.
    #[serde(default = "default_sync_watch_linked_worktrees")]
    pub watch_linked_worktrees: bool,
    /// Per-project quiet-period debounce before a watcher-triggered sync (ms).
    #[serde(default = "default_sync_watch_debounce_ms")]
    pub watch_debounce_ms: u64,
    /// Maximum time a watcher-triggered sync can be deferred by debounce (ms).
    #[serde(default = "default_sync_watch_max_delay_ms")]
    pub watch_max_delay_ms: u64,
    /// Maximum number of recently-seen projects the watcher registers.
    #[serde(default = "default_sync_watch_max_projects")]
    pub watch_max_projects: usize,
    /// Enable non-blocking sync-on-read for query tools.
    #[serde(default = "default_sync_read_refresh")]
    pub read_refresh: bool,
    /// Cooldown between read-triggered background refreshes (seconds).
    #[serde(default = "default_sync_read_cooldown_secs")]
    pub read_cooldown_secs: u64,
    /// Fire a catch-up sync on session start.
    #[serde(default = "default_sync_session_start_sync")]
    pub session_start_sync: bool,
    /// Staleness threshold above which session-start sync runs (seconds).
    #[serde(default = "default_sync_session_start_stale_threshold_secs")]
    pub session_start_stale_threshold_secs: u64,
    /// Daemon backstop scheduler interval (minutes); 0 disables it.
    #[serde(default = "default_sync_backstop_interval_mins")]
    pub backstop_interval_mins: u64,
    /// Diff-scoped syncs above this many changed files escalate to a full sync.
    #[serde(default = "default_sync_full_sync_escalation_files")]
    pub full_sync_escalation_files: usize,
    /// Daemon-wide cap on concurrent syncs.
    #[serde(default = "default_sync_max_concurrent_syncs")]
    pub max_concurrent_syncs: usize,
    /// Grace period before a dead tracked-branch store is GC'd (days).
    #[serde(default = "default_sync_branch_gc_days")]
    pub branch_gc_days: u64,
    /// Grace period before an orphan branch DB is GC'd (days).
    #[serde(default = "default_sync_orphan_db_gc_days")]
    pub orphan_db_gc_days: u64,
    /// Auto-initialise never-indexed repos on first contact.
    #[serde(default = "default_sync_auto_init")]
    pub auto_init: bool,
    /// Enable the daemon PR-branch auto-tracking mode: when on, the daemon polls
    /// the repo's GitHub remote for open PRs and tracks/untracks each PR head
    /// branch through the normal branch-tracking machinery. Off by default for
    /// back-compat.
    #[serde(default = "default_sync_auto_track_pr_branches")]
    pub auto_track_pr_branches: bool,
    /// Poll cadence (seconds) for PR-branch auto-tracking discovery. Clamped up
    /// to [`MIN_AUTO_TRACK_PR_POLL_SECS`] at read time.
    #[serde(default = "default_sync_auto_track_pr_poll_secs")]
    pub auto_track_pr_poll_secs: u64,
    /// Daemon retention/compaction policy tree (Plan 38).
    #[serde(default)]
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
            auto_watch: default_sync_auto_watch(),
            watch_linked_worktrees: default_sync_watch_linked_worktrees(),
            watch_debounce_ms: default_sync_watch_debounce_ms(),
            watch_max_delay_ms: default_sync_watch_max_delay_ms(),
            watch_max_projects: default_sync_watch_max_projects(),
            read_refresh: default_sync_read_refresh(),
            read_cooldown_secs: default_sync_read_cooldown_secs(),
            session_start_sync: default_sync_session_start_sync(),
            session_start_stale_threshold_secs: default_sync_session_start_stale_threshold_secs(),
            backstop_interval_mins: default_sync_backstop_interval_mins(),
            full_sync_escalation_files: default_sync_full_sync_escalation_files(),
            max_concurrent_syncs: default_sync_max_concurrent_syncs(),
            branch_gc_days: default_sync_branch_gc_days(),
            orphan_db_gc_days: default_sync_orphan_db_gc_days(),
            auto_init: default_sync_auto_init(),
            auto_track_pr_branches: default_sync_auto_track_pr_branches(),
            auto_track_pr_poll_secs: default_sync_auto_track_pr_poll_secs(),
            retention: RetentionConfig::default(),
        }
    }
}

/// Parses a boolean env value. Truthy spellings (`1`/`true`/`yes`/`on`) share
/// [`tracedecay_global_db::env_value_truthy`]; `0`/`false` are false. Any
/// other value is ignored (returns `None`) so an override is not applied.
pub(crate) fn parse_env_bool(raw: &str) -> Option<bool> {
    if tracedecay_global_db::env_value_truthy(raw) {
        return Some(true);
    }
    match raw.trim().to_ascii_lowercase().as_str() {
        "0" | "false" => Some(false),
        _ => None,
    }
}

/// Reads a `TRACEDECAY_<suffix>` env var and parses it as a bool.
pub(crate) fn env_bool(suffix: &str) -> Option<bool> {
    brand_env(suffix).as_deref().and_then(parse_env_bool)
}

/// Reads a `TRACEDECAY_<suffix>` env var and parses it as an integer of the
/// caller's choosing.
fn env_parse<T: std::str::FromStr>(suffix: &str) -> Option<T> {
    brand_env(suffix)
        .as_deref()
        .and_then(|raw| raw.trim().parse::<T>().ok())
}

impl SyncConfig {
    /// Applies legacy `TRACEDECAY_SYNC_*` environment overrides on top of
    /// `self`. This remains for pre-store/bootstrap compatibility only; live
    /// runtime adapters must consume [`PinnedRuntimeConfiguration`] instead.
    #[must_use]
    pub fn with_env_overrides(mut self) -> Self {
        if let Some(value) = env_bool("SYNC_AUTO_WATCH") {
            self.auto_watch = value;
        }
        if let Some(value) = env_bool("SYNC_WATCH_LINKED_WORKTREES") {
            self.watch_linked_worktrees = value;
        }
        if let Some(value) = env_parse("SYNC_WATCH_DEBOUNCE_MS") {
            self.watch_debounce_ms = value;
        }
        if let Some(value) = env_parse("SYNC_WATCH_MAX_DELAY_MS") {
            self.watch_max_delay_ms = value;
        }
        if let Some(value) = env_parse("SYNC_WATCH_MAX_PROJECTS") {
            self.watch_max_projects = value;
        }
        if let Some(value) = env_bool("SYNC_READ_REFRESH") {
            self.read_refresh = value;
        }
        if let Some(value) = env_parse("SYNC_READ_COOLDOWN_SECS") {
            self.read_cooldown_secs = value;
        }
        if let Some(value) = env_bool("SYNC_SESSION_START_SYNC") {
            self.session_start_sync = value;
        }
        if let Some(value) = env_parse("SYNC_SESSION_START_STALE_THRESHOLD_SECS") {
            self.session_start_stale_threshold_secs = value;
        }
        if let Some(value) = env_parse("SYNC_BACKSTOP_INTERVAL_MINS") {
            self.backstop_interval_mins = value;
        }
        if let Some(value) = env_parse("SYNC_FULL_SYNC_ESCALATION_FILES") {
            self.full_sync_escalation_files = value;
        }
        if let Some(value) = env_parse("SYNC_MAX_CONCURRENT_SYNCS") {
            self.max_concurrent_syncs = value;
        }
        if let Some(value) = env_parse("SYNC_BRANCH_GC_DAYS") {
            self.branch_gc_days = value;
        }
        if let Some(value) = env_parse("SYNC_ORPHAN_DB_GC_DAYS") {
            self.orphan_db_gc_days = value;
        }
        if let Some(value) = env_bool("SYNC_AUTO_INIT") {
            self.auto_init = value;
        }
        if let Some(value) = env_bool("SYNC_AUTO_TRACK_PR_BRANCHES") {
            self.auto_track_pr_branches = value;
        }
        if let Some(value) = env_parse("SYNC_AUTO_TRACK_PR_POLL_SECS") {
            self.auto_track_pr_poll_secs = value;
        }
        self
    }
}

impl Default for TraceDecayConfig {
    fn default() -> Self {
        Self {
            version: 1,
            root_dir: String::new(),
            exclude: default_exclude_patterns(),
            include: Vec::new(),
            max_file_size: 1_048_576,
            extract_docstrings: true,
            track_call_sites: true,
            git_ignore: default_git_ignore(),
            diagnostics_prewarm: false,
            native_graph_activation: default_native_graph_activation(),
            semantic: SemanticConfig::default(),
            sync: SyncConfig::default(),
            telemetry: TelemetryConfig::default(),
        }
    }
}

impl TraceDecayConfig {
    /// Layers the daemon-only policy over the shared runtime settings of an
    /// already validated pin. The shared settings are copied from the pin, so
    /// they agree with every other consumer by construction; only the
    /// daemon-only sync, retention, and legacy metadata fields are decoded
    /// here, from the same snapshot, without defaults, file reads, or
    /// environment reads.
    #[hotpath::measure(label = "daemon.config.parse")]
    pub fn from_runtime(runtime: &PinnedRuntimeConfiguration) -> Result<Self> {
        let shared = runtime.config();
        let snapshot = runtime.snapshot();
        Ok(Self {
            version: 1,
            root_dir: runtime.target().project_root.to_string_lossy().to_string(),
            exclude: shared.exclude.clone(),
            include: shared.include.clone(),
            max_file_size: shared.max_file_size,
            extract_docstrings: shared.extract_docstrings,
            track_call_sites: shared.track_call_sites,
            git_ignore: shared.git_ignore,
            diagnostics_prewarm: shared.diagnostics_prewarm,
            native_graph_activation: shared.native_graph_activation,
            semantic: shared.semantic.clone(),
            sync: SyncConfig {
                auto_watch: required_bool(snapshot, SYNC_AUTO_WATCH_SETTING_KEY)?,
                watch_linked_worktrees: required_bool(
                    snapshot,
                    SYNC_WATCH_LINKED_WORKTREES_SETTING_KEY,
                )?,
                watch_debounce_ms: required_unsigned(snapshot, SYNC_WATCH_DEBOUNCE_MS_SETTING_KEY)?,
                watch_max_delay_ms: required_unsigned(
                    snapshot,
                    SYNC_WATCH_MAX_DELAY_MS_SETTING_KEY,
                )?,
                watch_max_projects: required_usize(snapshot, SYNC_WATCH_MAX_PROJECTS_SETTING_KEY)?,
                read_refresh: required_bool(snapshot, SYNC_READ_REFRESH_SETTING_KEY)?,
                read_cooldown_secs: required_unsigned(
                    snapshot,
                    SYNC_READ_COOLDOWN_SECS_SETTING_KEY,
                )?,
                session_start_sync: required_bool(snapshot, SYNC_SESSION_START_SYNC_SETTING_KEY)?,
                session_start_stale_threshold_secs: required_unsigned(
                    snapshot,
                    SYNC_SESSION_START_STALE_THRESHOLD_SECS_SETTING_KEY,
                )?,
                backstop_interval_mins: required_unsigned(
                    snapshot,
                    SYNC_BACKSTOP_INTERVAL_MINS_SETTING_KEY,
                )?,
                full_sync_escalation_files: required_usize(
                    snapshot,
                    SYNC_FULL_SYNC_ESCALATION_FILES_SETTING_KEY,
                )?,
                max_concurrent_syncs: required_usize(
                    snapshot,
                    SYNC_MAX_CONCURRENT_SYNCS_SETTING_KEY,
                )?,
                branch_gc_days: required_unsigned(snapshot, SYNC_BRANCH_GC_DAYS_SETTING_KEY)?,
                orphan_db_gc_days: required_unsigned(snapshot, SYNC_ORPHAN_DB_GC_DAYS_SETTING_KEY)?,
                auto_init: required_bool(snapshot, SYNC_AUTO_INIT_SETTING_KEY)?,
                auto_track_pr_branches: shared.sync.auto_track_pr_branches,
                auto_track_pr_poll_secs: shared.sync.auto_track_pr_poll_secs,
                retention: retention_config_from_snapshot(snapshot)?,
            },
            telemetry: TelemetryConfig {
                timings: shared.telemetry.timings,
            },
        })
    }
}

fn retention_config_from_snapshot(snapshot: &ConfigurationSnapshotV1) -> Result<RetentionConfig> {
    let retention = match optional_text_setting(snapshot, SYNC_RETENTION_SETTING_KEY)? {
        None => RetentionConfig::default(),
        Some(value) => serde_json::from_str(value).map_err(|error| {
            config_error(format!("resolved retention setting is invalid: {error}"))
        })?,
    };
    retention.validate()?;
    Ok(retention)
}

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

/// Returns the path to the configuration file (`config.json`) within the
/// resolved data directory.
pub fn get_config_path(project_root: &Path) -> PathBuf {
    if let Ok(layout) =
        tracedecay_runtime_core::storage::resolve_layout_for_current_profile(project_root)
    {
        return layout.config_path;
    }
    get_tracedecay_dir(project_root).join(CONFIG_FILENAME)
}

/// Loads a legacy configuration input from disk.
///
/// This compatibility reader is for migration and read-only diagnostics only;
/// runtime consumers must use a pinned resolved snapshot. If the file does
/// not exist, it returns the legacy defaults with `root_dir` set to the given
/// project root.
pub fn load_config(project_root: &Path) -> Result<TraceDecayConfig> {
    let config_path = get_config_path(project_root);
    load_config_from_path(project_root, &config_path)
}

/// Loads configuration from an explicit config path while preserving the
/// project root used for default config values.
pub fn load_config_from_path(project_root: &Path, config_path: &Path) -> Result<TraceDecayConfig> {
    if !config_path.exists() {
        return Ok(TraceDecayConfig {
            root_dir: project_root.to_string_lossy().to_string(),
            ..TraceDecayConfig::default()
        });
    }

    let contents = fs::read_to_string(config_path).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to read config file '{}': {}",
            config_path.display(),
            e
        ),
    })?;

    let config: TraceDecayConfig =
        serde_json::from_str(&contents).map_err(|e| TraceDecayError::Config {
            message: format!(
                "failed to parse config file '{}': {}",
                config_path.display(),
                e
            ),
        })?;

    Ok(config)
}

/// Writes a legacy configuration fixture to an explicit path using an atomic
/// write.
///
/// Production runtime code must use the daemon control plane instead of this
/// compatibility helper. It remains for fixtures and legacy-input tests while
/// callers complete their migration.
pub fn save_config_to_path(config_path: &Path, config: &TraceDecayConfig) -> Result<()> {
    let data_dir = config_path
        .parent()
        .ok_or_else(|| TraceDecayError::Config {
            message: format!(
                "configuration path '{}' has no parent directory",
                config_path.display()
            ),
        })?;
    tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all(data_dir).map_err(|e| {
        TraceDecayError::Config {
            message: format!(
                "failed to create tracedecay directory '{}': {}",
                data_dir.display(),
                e
            ),
        }
    })?;

    let tmp_path = config_path.with_extension("tmp");

    let json = serde_json::to_string_pretty(config).map_err(|e| TraceDecayError::Config {
        message: format!("failed to serialize config: {e}"),
    })?;

    fs::write(&tmp_path, &json).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to write temporary config file '{}': {}",
            tmp_path.display(),
            e
        ),
    })?;

    fs::rename(&tmp_path, config_path).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to rename temporary config file '{}' to '{}': {}",
            tmp_path.display(),
            config_path.display(),
            e
        ),
    })?;

    Ok(())
}

/// Returns `true` if the project marker dir (`.tracedecay`) is ignored by Git
/// for this project.
///
/// This respects the repository `.gitignore`, `.git/info/exclude`, and the
/// user's global excludes file via `git check-ignore`. If Git cannot answer
/// (for example outside a Git repository), falls back to checking the local
/// `.gitignore` file only.
pub fn is_in_gitignore(project_path: &Path) -> bool {
    if let Some(is_ignored) = is_ignored_by_git(project_path, None) {
        return is_ignored;
    }

    is_in_local_gitignore(project_path)
}

pub(crate) fn is_ignored_by_git(
    project_path: &Path,
    git_config_global: Option<&Path>,
) -> Option<bool> {
    let fallback_global_excludes = || {
        git_config_global
            .and_then(|path| is_ignored_by_explicit_global_excludes(project_path, path))
    };
    let dir_name = active_data_dir_name(project_path);
    let Ok(git) = tracedecay_runtime_core::git::try_git_program() else {
        return fallback_global_excludes();
    };
    let mut command = Command::new(git);
    command
        .arg("-C")
        .arg(project_path)
        .arg("check-ignore")
        .arg("-q")
        .arg(format!("{dir_name}/"))
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    if let Some(path) = git_config_global {
        command.env_clear();
        command.env("PATH", git_subprocess_path());
        command.env("GIT_CONFIG_GLOBAL", path);
        command.env("GIT_CONFIG_NOSYSTEM", "1");
    }

    let Ok(status) = command.status() else {
        return fallback_global_excludes();
    };

    match status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => fallback_global_excludes(),
    }
}

pub(crate) fn is_ignored_by_explicit_global_excludes(
    project_path: &Path,
    git_config_global: &Path,
) -> Option<bool> {
    let config = fs::read_to_string(git_config_global).ok()?;
    let excludes_file = config.lines().find_map(|line| {
        let trimmed = line.trim();
        let (key, value) = trimmed.split_once('=')?;
        (key.trim() == "excludesFile").then(|| PathBuf::from(value.trim()))
    })?;
    let excludes = fs::read_to_string(excludes_file).ok()?;
    let dir_name = active_data_dir_name(project_path);
    let dir_pattern = format!("{dir_name}/");
    Some(excludes.lines().any(|line| {
        let trimmed = line.trim();
        !trimmed.is_empty()
            && !trimmed.starts_with('#')
            && (trimmed == dir_name || trimmed == dir_pattern)
    }))
}

#[cfg(test)]
fn git_subprocess_path() -> OsString {
    std::env::var_os("PATH").unwrap_or_else(|| {
        #[cfg(windows)]
        {
            OsString::new()
        }
        #[cfg(not(windows))]
        {
            OsString::from("/usr/bin:/bin")
        }
    })
}

#[cfg(not(test))]
fn git_subprocess_path() -> OsString {
    std::env::var_os("PATH").unwrap_or_default()
}

fn is_in_local_gitignore(project_path: &Path) -> bool {
    let dir_name = active_data_dir_name(project_path);
    let gitignore = project_path.join(".gitignore");
    match fs::read_to_string(&gitignore) {
        Ok(content) => content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed == dir_name
                || trimmed == format!("{dir_name}/")
                || trimmed == format!("/{dir_name}")
        }),
        Err(_) => false,
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

/// Returns `true` if the path matches any of the configured `include` patterns.
///
/// This is used to allow hidden (dot-prefixed) directories that would
/// otherwise be skipped by the file walker.
pub fn is_included(path: &str, config: &TraceDecayConfig) -> bool {
    any_pattern_matches(&config.include, &[path])
}

/// Returns `true` if a directory should be pruned during scanning.
///
/// Matches `dir/_` against exclude patterns (for `dir/**`-style globs) and
/// also matches `dir` itself (for bare `**/dirname`-style globs).  This
/// ensures that patterns like `**/node_modules` and `**/node_modules/**`
/// both trigger directory pruning in `scan_files_walkdir`.
pub fn is_excluded_dir(dir_path: &str, config: &TraceDecayConfig) -> bool {
    // Try both the dummy-file probe (catches `dir/**`) and the bare directory
    // path (catches `**/dirname`).
    let descendant_probe = format!("{dir_path}/_");
    any_pattern_matches(&config.exclude, &[&descendant_probe, dir_path])
}

/// Returns `true` if the file matches any of the configured exclude patterns.
pub fn is_excluded(file_path: &str, config: &TraceDecayConfig) -> bool {
    any_pattern_matches(&config.exclude, &[file_path])
}

/// Glob semantics shared by every include/exclude test. Kept in one place so
/// the four entry points cannot drift apart on case or separator handling.
const PATTERN_MATCH_OPTIONS: glob::MatchOptions = glob::MatchOptions {
    case_sensitive: true,
    require_literal_separator: false,
    require_literal_leading_dot: false,
};

/// True when any of `patterns` matches any of `candidates`. Unparseable
/// patterns are skipped rather than failing the whole test, matching the
/// long-standing behaviour of the include/exclude entry points.
///
/// Callers pass every candidate string they want probed, built once per call:
/// the directory variants used to format their `dir/_` probe once per pattern.
fn any_pattern_matches(patterns: &[String], candidates: &[&str]) -> bool {
    patterns.iter().any(|pattern_str| {
        Pattern::new(pattern_str).is_ok_and(|pattern| {
            candidates
                .iter()
                .any(|candidate| pattern.matches_with(candidate, PATTERN_MATCH_OPTIONS))
        })
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
