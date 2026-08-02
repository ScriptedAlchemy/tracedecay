use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, LazyLock, OnceLock, RwLock};

use glob::Pattern;
use serde::{Deserialize, Serialize};
use tracedecay_domain::configuration::{
    ConfigurationLayerIdV1, ConfigurationRevisionId, ConfigurationSnapshotV1, ConfigurationValueV1,
    DIAGNOSTICS_PREWARM_SETTING_KEY, INDEX_EXCLUDE_SETTING_KEY,
    INDEX_EXTRACT_DOCSTRINGS_SETTING_KEY, INDEX_GIT_IGNORE_SETTING_KEY, INDEX_INCLUDE_SETTING_KEY,
    INDEX_MAX_FILE_SIZE_SETTING_KEY, INDEX_TRACK_CALL_SITES_SETTING_KEY,
    SYNC_AUTO_INIT_SETTING_KEY, SYNC_AUTO_TRACK_PR_BRANCHES_SETTING_KEY,
    SYNC_AUTO_TRACK_PR_POLL_SECS_SETTING_KEY, SYNC_AUTO_WATCH_SETTING_KEY,
    SYNC_BACKSTOP_INTERVAL_MINS_SETTING_KEY, SYNC_BRANCH_GC_DAYS_SETTING_KEY,
    SYNC_FULL_SYNC_ESCALATION_FILES_SETTING_KEY, SYNC_MAX_CONCURRENT_SYNCS_SETTING_KEY,
    SYNC_ORPHAN_DB_GC_DAYS_SETTING_KEY, SYNC_READ_COOLDOWN_SECS_SETTING_KEY,
    SYNC_READ_REFRESH_SETTING_KEY, SYNC_SESSION_START_STALE_THRESHOLD_SECS_SETTING_KEY,
    SYNC_SESSION_START_SYNC_SETTING_KEY, SYNC_WATCH_DEBOUNCE_MS_SETTING_KEY,
    SYNC_WATCH_MAX_DELAY_MS_SETTING_KEY, SYNC_WATCH_MAX_PROJECTS_SETTING_KEY, SettingKey,
    TELEMETRY_TIMINGS_SETTING_KEY,
};
use tracedecay_domain::{ProjectId, UtcMicros};

use crate::application::configuration::ConfigurationControlStore;
use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::global_db::configuration::{
    CanonicalGenesisConfigurationV1, GlobalDbConfigurationControlStore,
    migrate_legacy_configuration_inputs_with_genesis,
};

pub use tracedecay_global_db::configuration::{
    LegacyConfigurationDecodeTargetV1, decode_legacy_config_json,
    decode_legacy_configuration_inputs, decode_legacy_environment_overrides, registry,
    resolve_legacy_configuration_inputs, resolver,
};
pub use tracedecay_usecases::config::retrieval;
pub mod scope_control;
pub mod topology;

/// Name of the legacy configuration migration input stored inside the data
/// directory. It is not a runtime authority and production code must never
/// rewrite it.
pub const CONFIG_FILENAME: &str = "config.json";

/// Kernel-owned path primitives. The definitions live in
/// `tracedecay_runtime_core::config` because the storage layout, database,
/// branch-metadata, and store layers depend on them and moved into that crate;
/// re-exporting here keeps every `crate::config::<item>` path intact.
pub use tracedecay_runtime_core::config::{
    DB_FILENAME, TRACEDECAY_DIR, USER_DATA_DIR_ENV, active_data_dir_name, db_filename,
    discover_project_root, get_project_db_path, get_tracedecay_dir, has_project_database,
    user_data_dir,
};

/// Atomic project-scoped semantic runtime selection.
///
/// The value is canonical JSON for [`SemanticConfig`]. Keeping the active
/// profile, rollback profile, and local resource ceilings under one setting
/// prevents a configuration revision from exposing a partially updated
/// semantic selection.
pub use tracedecay_domain::configuration::SEMANTIC_RUNTIME_SETTING_KEY;

/// Atomic daemon retention/compaction policy tree (Plan 38).
///
/// The value is canonical JSON for [`RetentionConfig`]. Keeping the session
/// (LCM), observation-evidence, orphan-store, debris, and compaction windows
/// under one setting keeps the retention engines threaded as a single
/// versioned unit the daemon backstop reads, mirroring the semantic key. Absent
/// or unset resolves to [`RetentionConfig::default`]'s bounded safe policy.
pub const SYNC_RETENTION_SETTING_KEY: &str = "sync.retention.v1";

/// Canonical pinned semantic runtime configuration.
///
/// Re-exported from the global configuration authority so historical
/// `crate::config::<item>` paths retain nominal type identity.
pub use tracedecay_global_db::configuration::semantic::{
    DEFAULT_FASTEMBED_MODEL_ID, SemanticConfig, SemanticProfileSelection, SemanticResourceCeilings,
};

/// The shared generated/vendored segment list and its membership test moved
/// into `tracedecay_runtime_core::config`: the extracted migration inventory
/// scanner consults them and cannot reach back into the root crate.
/// Re-exported so every historical `crate::config::<item>` path keeps
/// resolving.
pub use tracedecay_runtime_core::config::{GENERATED_DIR_SEGMENTS, is_generated_dir_segment};

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

fn default_sync_auto_watch() -> bool {
    true
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

/// Incremental-vacuum compaction trigger for the daemon background lane
/// (Plan 38 §6). Threads [`crate::storage::compaction::CompactionTriggerPolicyV1`]
/// through owner config: the daemon samples a store's free-page ratio and, when
/// this threshold is met, schedules a bounded incremental vacuum off the hot
/// path.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CompactionThresholdConfig {
    /// Free-page ratio at or above which an incremental vacuum is scheduled.
    pub free_page_ratio_threshold: f64,
    /// Minimum reclaimable free bytes below which compaction is not worth it.
    #[serde(default)]
    pub minimum_reclaimable_bytes: u64,
    /// Upper bound on freelist pages reclaimed per tick, keeping each vacuum
    /// bounded and off the hot path.
    #[serde(default = "default_compaction_max_pages_per_tick")]
    pub max_pages_per_tick: u32,
}

fn default_compaction_max_pages_per_tick() -> u32 {
    1024
}

impl Default for CompactionThresholdConfig {
    fn default() -> Self {
        Self {
            free_page_ratio_threshold: 0.25,
            minimum_reclaimable_bytes: 64 * 1024 * 1024,
            max_pages_per_tick: default_compaction_max_pages_per_tick(),
        }
    }
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
    pub session_lcm: crate::sessions::lcm::LcmRetentionConfig,
    /// Observation-evidence generation-scoped retention windows.
    #[serde(default)]
    pub observation: crate::global_db::observation::retention::ObservationRetentionConfig,
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
            session_lcm: crate::sessions::lcm::LcmRetentionConfig::default(),
            observation:
                crate::global_db::observation::retention::ObservationRetentionConfig::default(),
            orphan_store_gc_days: default_orphan_store_gc_days(),
            incident_debris_retention_days: default_incident_debris_retention_days(),
            compaction: default_compaction_threshold(),
            store_soft_budgets_bytes: BTreeMap::new(),
            interval_hours: default_retention_interval_hours(),
        }
    }
}

impl RetentionConfig {
    pub(crate) fn store_soft_budget(
        &self,
        store: &str,
    ) -> Result<Option<tracedecay_application::storage::StoreSizeBudgetV1>> {
        let Some(bytes) = self.store_soft_budgets_bytes.get(store).copied() else {
            return Ok(None);
        };
        let budget = tracedecay_application::storage::StoreSizeBudgetV1 {
            store: tracedecay_application::storage::StoreKeyV1::new(store.to_owned())
                .map_err(|error| config_error(error.to_string()))?,
            soft_limit_bytes: tracedecay_application::storage::StorageByteSizeV1(bytes),
        };
        budget
            .validate()
            .map_err(|error| config_error(error.to_string()))?;
        Ok(Some(budget))
    }

    /// Validate collection windows and the compaction trigger. Immediate
    /// collection and ratios outside the unit interval are rejected.
    fn validate(&self) -> Result<()> {
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
            tracedecay_application::storage::StoreKeyV1::new(store.clone()).map_err(|_| {
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
pub struct SyncConfig {
    /// Enable the daemon git-metadata watcher.
    #[serde(default = "default_sync_auto_watch")]
    pub auto_watch: bool,
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

/// Parses a boolean env value: `1`/`true` => true, `0`/`false` => false
/// (case-insensitive). Any other value is ignored (returns `None`).
fn parse_env_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" => Some(true),
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
            semantic: SemanticConfig::default(),
            sync: SyncConfig::default(),
            telemetry: TelemetryConfig::default(),
        }
    }
}

/// Typed project route for the configuration daemon boundary. The path is
/// display/routing context only; [`ProjectId`] remains the authority key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeConfigurationTarget {
    pub project_id: ProjectId,
    pub project_root: PathBuf,
}

/// A complete resolved configuration pinned to one revision before a runtime
/// component starts. No caller may re-read mutable legacy input after holding
/// this value.
#[derive(Clone, Debug)]
pub struct PinnedRuntimeConfiguration {
    pub target: RuntimeConfigurationTarget,
    pub revision_id: ConfigurationRevisionId,
    pub snapshot: ConfigurationSnapshotV1,
    pub config: TraceDecayConfig,
}

impl PinnedRuntimeConfiguration {
    /// Materializes the legacy runtime shape from a complete typed snapshot.
    /// The conversion rejects missing or wrongly typed settings rather than
    /// adding adapter-local defaults.
    pub fn new(
        target: RuntimeConfigurationTarget,
        revision_id: ConfigurationRevisionId,
        snapshot: ConfigurationSnapshotV1,
    ) -> Result<Self> {
        let config = runtime_config_from_snapshot(&target.project_root, &snapshot)?;
        Ok(Self {
            target,
            revision_id,
            snapshot,
            config,
        })
    }

    fn retarget(&self, target: RuntimeConfigurationTarget) -> Result<Self> {
        Self::new(target, self.revision_id.clone(), self.snapshot.clone())
    }
}

/// Process-local, immutable-after-publication lookup cache. The daemon owns
/// refreshing it when a configuration revision activates; hook paths only
/// perform an in-memory lookup.
#[derive(Default)]
pub struct RuntimeConfigurationCache {
    by_project: RwLock<BTreeMap<String, PinnedRuntimeConfiguration>>,
    project_by_root: RwLock<BTreeMap<PathBuf, String>>,
}

impl RuntimeConfigurationCache {
    pub fn insert(&self, configuration: PinnedRuntimeConfiguration) -> Result<()> {
        let expected = runtime_config_from_snapshot(
            &configuration.target.project_root,
            &configuration.snapshot,
        )?;
        if expected != configuration.config {
            return Err(config_error(
                "pinned runtime configuration does not match its resolved snapshot",
            ));
        }

        let project_id = configuration.target.project_id.as_str().to_owned();
        let project_root = configuration.target.project_root.clone();
        self.by_project
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(project_id.clone(), configuration);
        self.project_by_root
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(project_root, project_id);
        Ok(())
    }

    pub fn for_project(&self, project_id: &ProjectId) -> Result<PinnedRuntimeConfiguration> {
        self.by_project
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(project_id.as_str())
            .cloned()
            .ok_or_else(|| {
                config_error(format!(
                    "configuration authority unavailable: no pinned resolved snapshot for project '{}'",
                    project_id.as_str()
                ))
            })
    }

    pub fn for_root(&self, project_root: &Path) -> Result<PinnedRuntimeConfiguration> {
        let project_id = self
            .project_by_root
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(project_root)
            .cloned()
            .ok_or_else(|| {
                config_error(format!(
                    "configuration authority unavailable: no pinned resolved snapshot for '{}'",
                    project_root.display()
                ))
            })?;
        let configuration = self
            .by_project
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&project_id)
            .cloned()
            .ok_or_else(|| {
                config_error(
                    "configuration authority unavailable: runtime snapshot cache is inconsistent",
                )
            })?;
        configuration.retarget(RuntimeConfigurationTarget {
            project_id: configuration.target.project_id.clone(),
            project_root: project_root.to_path_buf(),
        })
    }
}

impl tracedecay_dashboard_api::config::DashboardConfigurationReadPort
    for RuntimeConfigurationCache
{
    fn cached_runtime_configuration(
        &self,
        project_root: &Path,
    ) -> Result<tracedecay_dashboard_api::config::PinnedRuntimeConfiguration> {
        let configuration = self.for_root(project_root)?;
        tracedecay_dashboard_api::config::PinnedRuntimeConfiguration::new(
            tracedecay_dashboard_api::config::RuntimeConfigurationTarget {
                project_id: configuration.target.project_id,
                project_root: configuration.target.project_root,
            },
            configuration.revision_id,
            configuration.snapshot,
        )
    }

    fn is_in_gitignore(&self, project_root: &Path) -> bool {
        is_in_gitignore(project_root)
    }
}

fn runtime_configuration_cache() -> &'static Arc<RuntimeConfigurationCache> {
    static CACHE: OnceLock<Arc<RuntimeConfigurationCache>> = OnceLock::new();
    CACHE.get_or_init(|| Arc::new(RuntimeConfigurationCache::default()))
}

/// Installs the root-owned configuration cache as the dashboard's read port.
pub fn install_dashboard_configuration_read_port() -> Result<()> {
    tracedecay_dashboard_api::config::install_dashboard_configuration_read_port(
        runtime_configuration_cache().clone(),
    )
}

/// Publishes one daemon-resolved snapshot for runtime and hook consumers.
pub fn install_pinned_runtime_configuration(
    configuration: PinnedRuntimeConfiguration,
) -> Result<()> {
    runtime_configuration_cache().insert(configuration)
}

/// Builds a typed target from a resolved store layout. A missing project ID is
/// never replaced by a path-derived identity.
pub fn runtime_configuration_target_for_layout(
    project_root: &Path,
    layout: &crate::storage::StoreLayout,
) -> Result<RuntimeConfigurationTarget> {
    let project_id = layout.identity.project_id.as_deref().ok_or_else(|| {
        config_error("configuration authority unavailable: store layout has no project id")
    })?;
    runtime_configuration_target_for_project_id(project_root, project_id)
}

/// Builds a typed configuration target from an already-authoritative project
/// ID. The path remains non-authoritative routing context.
pub fn runtime_configuration_target_for_project_id(
    project_root: &Path,
    project_id: &str,
) -> Result<RuntimeConfigurationTarget> {
    Ok(RuntimeConfigurationTarget {
        project_id: ProjectId::new(project_id.to_owned()).map_err(|error| {
            config_error(format!("invalid project id for configuration: {error}"))
        })?,
        project_root: project_root.to_path_buf(),
    })
}

/// Returns the pinned configuration for an exact authoritative layout.
///
/// This is fail-closed: callers that must not invent authority (hooks,
/// destructive branch administration) use it after the daemon has published a
/// snapshot. Daemon project-open paths that need to cold-start a process use
/// [`open_runtime_configuration_for_registered_database`] instead.
pub fn runtime_configuration_for_layout(
    project_root: &Path,
    layout: &crate::storage::StoreLayout,
) -> Result<PinnedRuntimeConfiguration> {
    let target = runtime_configuration_target_for_layout(project_root, layout)?;
    let configuration = runtime_configuration_cache()
        .for_project(&target.project_id)?
        .retarget(target)?;
    runtime_configuration_cache().insert(configuration.clone())?;
    Ok(configuration)
}

/// Resolves the pinned runtime configuration for a daemon-side operation on a
/// live project, pinning it on demand from the durable configuration store when
/// the process-local snapshot cache is cold.
///
/// Unlike [`runtime_configuration_for_layout`], which is fail-closed for hook
/// paths that must never invent authority, this is the daemon authority path:
/// the daemon owns the durable configuration store, so a registered project that
/// simply has not been opened in this process (a first operation, or the first
/// after a daemon restart) is resolved and pinned rather than rejected. It never
/// consults legacy `config.json` input and never migrates or writes the store; a
/// genuinely uninitialized or unopenable configuration store still yields a
/// typed error rather than a fabricated default authority.
pub(crate) async fn resolve_runtime_configuration_for_registered_database(
    project_root: &Path,
    layout: &crate::storage::StoreLayout,
    database: Arc<RegisteredGlobalDb>,
) -> Result<PinnedRuntimeConfiguration> {
    let target = runtime_configuration_target_for_layout(project_root, layout)?;
    validate_registered_configuration_database(&target, database.as_ref())?;
    if let Ok(configuration) = runtime_configuration_cache().for_project(&target.project_id) {
        // The cache already holds a daemon-published pin (possibly a migrated
        // durable revision). Retarget it to this operation's non-authoritative
        // route and keep the fast path; do not reopen the store.
        let configuration = configuration.retarget(target)?;
        runtime_configuration_cache().insert(configuration.clone())?;
        return Ok(configuration);
    }
    // Cold cache: resolve the durable current revision (read-only, no legacy
    // input, no store mutation) and publish it. A store that was never made
    // writable resolves to registry defaults in memory; an initialized-but-
    // unreadable store surfaces a typed authority error.
    load_runtime_configuration_for_registered_database_read_only(project_root, layout, database)
        .await
}

/// Retained store handle paired with the exact revision resolved at project
/// open. Daemon composition consumes this bundle instead of opening a second
/// configuration database or resolving a second snapshot.
pub(crate) struct OpenedRuntimeConfiguration {
    pub(crate) configuration: PinnedRuntimeConfiguration,
    /// Exact daemon-owned registered session runtime used to resolve this
    /// snapshot. Configuration composition retains this authority directly;
    /// it never reacquires the physical database by path.
    pub(crate) registered_database: Arc<RegisteredGlobalDb>,
}

fn usecase_runtime_configuration(
    configuration: PinnedRuntimeConfiguration,
) -> Result<tracedecay_usecases::config::PinnedRuntimeConfiguration> {
    tracedecay_usecases::config::PinnedRuntimeConfiguration::new(
        tracedecay_usecases::config::RuntimeConfigurationTarget {
            project_id: configuration.target.project_id,
            project_root: configuration.target.project_root,
        },
        configuration.revision_id,
        configuration.snapshot,
    )
}

fn usecase_opened_runtime_configuration(
    opened: OpenedRuntimeConfiguration,
) -> Result<tracedecay_usecases::config::OpenedRuntimeConfiguration> {
    Ok(
        tracedecay_usecases::config::OpenedRuntimeConfiguration::new(
            usecase_runtime_configuration(opened.configuration)?,
            opened.registered_database,
        ),
    )
}

pub(crate) fn root_runtime_configuration(
    configuration: &tracedecay_usecases::config::PinnedRuntimeConfiguration,
) -> Result<PinnedRuntimeConfiguration> {
    PinnedRuntimeConfiguration::new(
        RuntimeConfigurationTarget {
            project_id: configuration.target.project_id.clone(),
            project_root: configuration.target.project_root.clone(),
        },
        configuration.revision_id.clone(),
        configuration.snapshot.clone(),
    )
}

pub(crate) fn materialize_root_runtime_configuration(
    configuration: &tracedecay_usecases::config::PinnedRuntimeConfiguration,
) -> Result<TraceDecayConfig> {
    Ok(root_runtime_configuration(configuration)?.config)
}

struct RootRuntimeConfigurationAuthority;

impl tracedecay_usecases::config::RuntimeConfigurationAuthorityPort
    for RootRuntimeConfigurationAuthority
{
    fn open<'a>(
        &'a self,
        project_root: &'a Path,
        layout: &'a crate::storage::StoreLayout,
        database: Arc<RegisteredGlobalDb>,
    ) -> tracedecay_usecases::config::RuntimeConfigurationFuture<
        'a,
        tracedecay_usecases::config::OpenedRuntimeConfiguration,
    > {
        Box::pin(async move {
            usecase_opened_runtime_configuration(
                open_runtime_configuration_for_registered_database(project_root, layout, database)
                    .await?,
            )
        })
    }

    fn open_read_only<'a>(
        &'a self,
        project_root: &'a Path,
        layout: &'a crate::storage::StoreLayout,
        database: Arc<RegisteredGlobalDb>,
    ) -> tracedecay_usecases::config::RuntimeConfigurationFuture<
        'a,
        tracedecay_usecases::config::OpenedRuntimeConfiguration,
    > {
        Box::pin(async move {
            usecase_opened_runtime_configuration(
                open_runtime_configuration_for_registered_database_read_only(
                    project_root,
                    layout,
                    database,
                )
                .await?,
            )
        })
    }

    fn resolve<'a>(
        &'a self,
        project_root: &'a Path,
        layout: &'a crate::storage::StoreLayout,
        database: Arc<RegisteredGlobalDb>,
    ) -> tracedecay_usecases::config::RuntimeConfigurationFuture<
        'a,
        tracedecay_usecases::config::PinnedRuntimeConfiguration,
    > {
        Box::pin(async move {
            usecase_runtime_configuration(
                resolve_runtime_configuration_for_registered_database(
                    project_root,
                    layout,
                    database,
                )
                .await?,
            )
        })
    }

    fn load_read_only<'a>(
        &'a self,
        project_root: &'a Path,
        layout: &'a crate::storage::StoreLayout,
        database: Arc<RegisteredGlobalDb>,
    ) -> tracedecay_usecases::config::RuntimeConfigurationFuture<
        'a,
        tracedecay_usecases::config::PinnedRuntimeConfiguration,
    > {
        Box::pin(async move {
            usecase_runtime_configuration(
                load_runtime_configuration_for_registered_database_read_only(
                    project_root,
                    layout,
                    database,
                )
                .await?,
            )
        })
    }
}

pub(crate) fn install_usecase_runtime_configuration_authority() -> Result<()> {
    static INSTALLATION: LazyLock<std::result::Result<(), String>> = LazyLock::new(|| {
        tracedecay_usecases::config::install_runtime_configuration_authority(Arc::new(
            RootRuntimeConfigurationAuthority,
        ))
        .map_err(|error| error.to_string())?;
        install_dashboard_configuration_read_port().map_err(|error| error.to_string())
    });
    INSTALLATION
        .as_ref()
        .map_err(|message| config_error(message.clone()))
        .copied()
}

/// Loads and publishes the durable current configuration for a resolved store
/// layout.
///
/// A fresh project receives one migration-backed registry-default revision.
/// Once any revision exists, open always reads that durable current revision;
/// a corrupt or ambiguous history is never replaced with local defaults.
pub(crate) async fn open_runtime_configuration_for_registered_database(
    project_root: &Path,
    layout: &crate::storage::StoreLayout,
    database: Arc<RegisteredGlobalDb>,
) -> Result<OpenedRuntimeConfiguration> {
    let target = runtime_configuration_target_for_layout(project_root, layout)?;
    validate_registered_configuration_database(&target, database.as_ref())?;
    let store = GlobalDbConfigurationControlStore::new_registered(database.as_ref());
    let configuration = open_runtime_configuration_from_store(target, layout, &store).await?;
    Ok(OpenedRuntimeConfiguration {
        configuration,
        registered_database: database,
    })
}

async fn open_runtime_configuration_from_store(
    target: RuntimeConfigurationTarget,
    layout: &crate::storage::StoreLayout,
    store: &GlobalDbConfigurationControlStore<'_>,
) -> Result<PinnedRuntimeConfiguration> {
    if let Err(error) = store.current().await {
        if !store
            .is_uninitialized()
            .await
            .map_err(map_configuration_error)?
        {
            return Err(map_configuration_error(error));
        }
        let registry = registry::ConfigurationRegistry::core().map_err(|error| {
            config_error(format!("configuration registry unavailable: {error}"))
        })?;
        let target_layer = ConfigurationLayerIdV1::Project {
            project_id: target.project_id.clone(),
        };
        let initial_revision_id =
            ConfigurationRevisionId::new("configuration.initial.migration.v1").map_err(
                |error| config_error(format!("invalid initial configuration revision: {error}")),
            )?;
        let legacy_target = LegacyConfigurationDecodeTargetV1 {
            target_layer: target_layer.clone(),
            target_revision_id: initial_revision_id.clone(),
        };
        let environment = std::env::vars().collect::<BTreeMap<_, _>>();
        let legacy =
            read_legacy_configuration_inputs(&layout.config_path, &environment, &legacy_target)?;
        // The project's first durable revision states the one binding the
        // daemon already owns for the project it just registered. Both
        // components restate resolved identity the caller holds — the
        // project's own id and its project-open locator digest — so this
        // grants no authority that a later protected `BindSource` would be
        // required to grant. Without it a fresh project has no binding at
        // all and every source-authorized surface is unreachable.
        let genesis = CanonicalGenesisConfigurationV1 {
            target_layer,
            target_revision_id: initial_revision_id,
            source_bindings: vec![
                scope_control::daemon_owned_project_source_binding(
                    &target.project_id,
                    &target.project_root,
                )
                .map_err(|error| {
                    config_error(format!(
                        "daemon project source binding could not be derived: {error}"
                    ))
                })?,
            ],
        };
        migrate_legacy_configuration_inputs_with_genesis(
            &registry,
            &legacy,
            &genesis,
            store,
            current_utc_micros(),
        )
        .await
        .map_err(|error| {
            config_error(format!(
                "configuration initial migration could not commit: {error}"
            ))
        })?;
    }
    let daemon_binding = scope_control::daemon_owned_project_source_binding(
        &target.project_id,
        &target.project_root,
    )
    .map_err(|error| {
        config_error(format!(
            "daemon project source binding could not be derived: {error}"
        ))
    })?;
    let current = store
        .ensure_daemon_source_binding(daemon_binding, current_utc_micros())
        .await
        .map_err(|error| {
            config_error(format!(
                "daemon project source binding forward repair failed: {error}"
            ))
        })?;
    let configuration =
        PinnedRuntimeConfiguration::new(target, current.revision_id, current.snapshot)?;
    install_pinned_runtime_configuration(configuration.clone())?;
    Ok(configuration)
}

/// Test-only convenience wrapper over
/// [`open_runtime_configuration_for_registered_database`] that returns just the
/// pinned snapshot. Production open paths keep the full
/// [`OpenedRuntimeConfiguration`] bundle (snapshot + registered database).
#[cfg(test)]
pub(crate) async fn ensure_runtime_configuration_for_registered_database(
    project_root: &Path,
    layout: &crate::storage::StoreLayout,
    database: Arc<RegisteredGlobalDb>,
) -> Result<PinnedRuntimeConfiguration> {
    Ok(
        open_runtime_configuration_for_registered_database(project_root, layout, database)
            .await?
            .configuration,
    )
}

/// Loads an already-persisted current configuration without creating a store,
/// applying a migration, or publishing a fallback revision.
pub(crate) async fn open_runtime_configuration_for_registered_database_read_only(
    project_root: &Path,
    layout: &crate::storage::StoreLayout,
    database: Arc<RegisteredGlobalDb>,
) -> Result<OpenedRuntimeConfiguration> {
    let target = runtime_configuration_target_for_layout(project_root, layout)?;
    validate_registered_configuration_database(&target, database.as_ref())?;
    let store = GlobalDbConfigurationControlStore::new_registered(database.as_ref());
    let configuration = open_runtime_configuration_read_only_from_store(target, &store).await?;
    Ok(OpenedRuntimeConfiguration {
        configuration,
        registered_database: database,
    })
}

async fn open_runtime_configuration_read_only_from_store(
    target: RuntimeConfigurationTarget,
    store: &GlobalDbConfigurationControlStore<'_>,
) -> Result<PinnedRuntimeConfiguration> {
    if store
        .is_uninitialized()
        .await
        .map_err(map_configuration_error)?
    {
        // The durable store exists but holds no configuration revision or
        // migration receipt yet — for example a consolidated destination whose
        // configuration authority was never migrated in, reopened read-only
        // after a repository move. Read-only inspection degrades to the
        // registry-default snapshot exactly as it does for a never-writable
        // store, rather than hard-erroring on the absent current revision. A
        // non-empty store with an unreadable current revision is not
        // uninitialized, so it still surfaces a typed authority error below and
        // durable authority is never silently replaced.
        let configuration = read_only_default_runtime_configuration(target)?;
        install_pinned_runtime_configuration(configuration.clone())?;
        return Ok(configuration);
    }
    let current = store.current().await.map_err(map_configuration_error)?;
    let configuration =
        PinnedRuntimeConfiguration::new(target, current.revision_id, current.snapshot)?;
    install_pinned_runtime_configuration(configuration.clone())?;
    Ok(configuration)
}

fn validate_registered_configuration_database(
    target: &RuntimeConfigurationTarget,
    database: &RegisteredGlobalDb,
) -> Result<()> {
    match &database.binding().shard_id.scope {
        tracedecay_store::StoreShardScopeV1::ProjectSessions { project_id }
            if project_id == &target.project_id =>
        {
            Ok(())
        }
        _ => Err(config_error(
            "configuration authority unavailable: registered database is not the exact project session shard",
        )),
    }
}

/// Builds the registry-default runtime configuration for a read-only open of a
/// store that has no durable configuration history yet. This mirrors the
/// snapshot a fresh writable open would migrate in, but stays entirely
/// in-memory so inspecting a never-opened store never mutates it.
fn read_only_default_runtime_configuration(
    target: RuntimeConfigurationTarget,
) -> Result<PinnedRuntimeConfiguration> {
    let registry = registry::ConfigurationRegistry::core()
        .map_err(|error| config_error(format!("configuration registry unavailable: {error}")))?;
    let resolution = resolver::resolve_configuration(&registry, &[]).map_err(|error| {
        config_error(format!(
            "configuration authority unavailable: could not resolve default snapshot: {error}"
        ))
    })?;
    let revision_id =
        ConfigurationRevisionId::new("configuration.read_only.default.v1").map_err(|error| {
            config_error(format!("invalid default configuration revision: {error}"))
        })?;
    PinnedRuntimeConfiguration::new(target, revision_id, resolution.snapshot)
}

pub(crate) async fn load_runtime_configuration_for_registered_database_read_only(
    project_root: &Path,
    layout: &crate::storage::StoreLayout,
    database: Arc<RegisteredGlobalDb>,
) -> Result<PinnedRuntimeConfiguration> {
    Ok(
        open_runtime_configuration_for_registered_database_read_only(
            project_root,
            layout,
            database,
        )
        .await?
        .configuration,
    )
}

#[cfg(not(test))]
pub async fn ensure_runtime_configuration_for_layout(
    _project_root: &Path,
    _layout: &crate::storage::StoreLayout,
) -> Result<PinnedRuntimeConfiguration> {
    Err(registered_configuration_database_required())
}

#[cfg(not(test))]
pub async fn resolve_runtime_configuration_for_layout(
    _project_root: &Path,
    _layout: &crate::storage::StoreLayout,
) -> Result<PinnedRuntimeConfiguration> {
    Err(registered_configuration_database_required())
}

#[cfg(not(test))]
pub async fn load_runtime_configuration_for_layout_read_only(
    _project_root: &Path,
    _layout: &crate::storage::StoreLayout,
) -> Result<PinnedRuntimeConfiguration> {
    Err(registered_configuration_database_required())
}

#[cfg(not(test))]
fn registered_configuration_database_required() -> TraceDecayError {
    config_error(
        "configuration authority unavailable: a registered project session runtime is required",
    )
}

fn current_utc_micros() -> UtcMicros {
    UtcMicros(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| {
                duration.as_micros().min(i64::MAX as u128) as i64
            }),
    )
}

fn map_configuration_error(
    error: crate::application::configuration::ConfigurationError,
) -> TraceDecayError {
    config_error(format!("configuration authority unavailable: {error}"))
}

/// Returns a cached configuration without resolving a layout, opening a
/// database, performing IPC, or reading a file. This is the hook-safe lookup.
pub fn cached_runtime_configuration(project_root: &Path) -> Result<PinnedRuntimeConfiguration> {
    runtime_configuration_cache().for_root(project_root)
}

/// Looks up a daemon-published snapshot by an already-authoritative project
/// ID. The supplied root is only used to materialize legacy display metadata;
/// it never participates in authority resolution.
pub fn cached_runtime_configuration_for_project_id(
    project_root: &Path,
    project_id: &str,
) -> Result<PinnedRuntimeConfiguration> {
    let target = runtime_configuration_target_for_project_id(project_root, project_id)?;
    runtime_configuration_cache()
        .for_project(&target.project_id)?
        .retarget(target)
}

pub fn cached_sync_config(project_root: &Path) -> Result<SyncConfig> {
    Ok(cached_runtime_configuration(project_root)?.config.sync)
}

pub fn cached_telemetry_config(project_root: &Path) -> Result<TelemetryConfig> {
    Ok(cached_runtime_configuration(project_root)?.config.telemetry)
}

/// Creates the only permitted pre-store runtime snapshot: registry defaults
/// with a synthetic bootstrap revision. It does not read or write
/// `config.json`, and a daemon must replace it with its migrated canonical
/// snapshot before a subsequent process can serve the project.
pub fn bootstrap_runtime_configuration(
    project_root: &Path,
    layout: &crate::storage::StoreLayout,
) -> Result<PinnedRuntimeConfiguration> {
    let target = runtime_configuration_target_for_layout(project_root, layout)?;
    if let Ok(existing) = runtime_configuration_cache().for_project(&target.project_id) {
        // One authoritative project can be opened through more than one
        // non-authoritative root spelling (for example, a linked worktree).
        // Publish the retargeted view too so its hook paths remain cache-only.
        let configuration = existing.retarget(target)?;
        runtime_configuration_cache().insert(configuration.clone())?;
        return Ok(configuration);
    }

    let registry = registry::ConfigurationRegistry::core()
        .map_err(|error| config_error(format!("configuration registry unavailable: {error}")))?;
    let snapshot = resolver::resolve_configuration(&registry, &[])
        .map_err(|error| {
            config_error(format!(
                "configuration bootstrap resolution failed: {error}"
            ))
        })?
        .snapshot;
    let revision_id =
        ConfigurationRevisionId::new("configuration.bootstrap.default.v1").map_err(|error| {
            config_error(format!("invalid bootstrap configuration revision: {error}"))
        })?;
    let configuration = PinnedRuntimeConfiguration::new(target, revision_id, snapshot)?;
    runtime_configuration_cache().insert(configuration.clone())?;
    Ok(configuration)
}

/// Applies the typed diff between two runtime materializations through the
/// installed daemon client. Missing client authority fails closed and cannot
/// fall back to a legacy file write.
pub async fn mutate_pinned_runtime_configuration(
    current: &PinnedRuntimeConfiguration,
    updated: TraceDecayConfig,
) -> Result<PinnedRuntimeConfiguration> {
    let Some(mutation) = direct_mutation_for_runtime_config_diff(
        &current.target.project_id,
        &current.config,
        &updated,
    )?
    else {
        return Ok(current.clone());
    };
    let client = tracedecay_usecases::config::configuration_daemon_client_for_project(
        &current.target.project_id,
    )
    .ok_or_else(|| {
        config_error(
            "configuration authority unavailable: daemon control-plane client is not installed",
        )
    })?;
    commit_runtime_configuration_mutation(client.as_ref(), current, mutation).await
}

/// Commits a precomputed typed mutation through the daemon control plane.
/// Keeping this seam independent from the process-global client slot makes the
/// response validation and publication rules testable without a local store.
async fn commit_runtime_configuration_mutation(
    client: &dyn tracedecay_usecases::config::ConfigurationDaemonClient,
    current: &PinnedRuntimeConfiguration,
    mutation: crate::application::configuration::DirectConfigurationMutation,
) -> Result<PinnedRuntimeConfiguration> {
    let next = client
        .mutate_direct(
            tracedecay_usecases::config::RuntimeConfigurationTarget {
                project_id: current.target.project_id.clone(),
                project_root: current.target.project_root.clone(),
            },
            mutation,
            current.revision_id.clone(),
        )
        .await?;
    if next.target.project_id != current.target.project_id {
        return Err(config_error(
            "configuration daemon returned a snapshot for a different project",
        ));
    }
    if next.revision_id == current.revision_id {
        return Err(config_error(
            "configuration daemon returned the expected revision after a mutation",
        ));
    }
    // The daemon's target path is non-authoritative routing metadata. Retarget
    // the returned snapshot to the caller's already-authorized route before
    // publishing it, which also re-materializes the legacy display fields from
    // the validated snapshot rather than trusting an adapter-provided shape.
    let next =
        PinnedRuntimeConfiguration::new(current.target.clone(), next.revision_id, next.snapshot)?;
    runtime_configuration_cache().insert(next.clone())?;
    Ok(next)
}

/// Decodes a legacy file only as migration input. This function never writes
/// the file and callers must pass the already-authorized target layer/revision
/// supplied by the control-plane migration.
pub fn read_legacy_configuration_inputs(
    config_path: &Path,
    environment: &BTreeMap<String, String>,
    target: &LegacyConfigurationDecodeTargetV1,
) -> Result<crate::global_db::configuration::migration::ReadonlyLegacyConfigurationInputsV1> {
    let config_json = if config_path.exists() {
        fs::read_to_string(config_path).map_err(|error| {
            config_error(format!(
                "failed to read legacy config input '{}': {error}",
                config_path.display()
            ))
        })?
    } else {
        "{}".to_owned()
    };
    decode_legacy_configuration_inputs(&config_json, environment, target)
        .map_err(|error| config_error(format!("legacy configuration input is invalid: {error}")))
}

/// Converts a complete typed snapshot into the legacy runtime shape without
/// defaults, file reads, or environment reads. This is intentionally public so
/// daemon composition can validate snapshot-to-runtime parity before publish.
pub fn runtime_config_from_snapshot(
    project_root: &Path,
    snapshot: &ConfigurationSnapshotV1,
) -> Result<TraceDecayConfig> {
    snapshot.validate().map_err(|error| {
        config_error(format!("invalid resolved configuration snapshot: {error}"))
    })?;

    Ok(TraceDecayConfig {
        version: 1,
        root_dir: project_root.to_string_lossy().to_string(),
        exclude: required_string_list(snapshot, INDEX_EXCLUDE_SETTING_KEY)?,
        include: required_string_list(snapshot, INDEX_INCLUDE_SETTING_KEY)?,
        max_file_size: required_unsigned(snapshot, INDEX_MAX_FILE_SIZE_SETTING_KEY)?,
        extract_docstrings: required_bool(snapshot, INDEX_EXTRACT_DOCSTRINGS_SETTING_KEY)?,
        track_call_sites: required_bool(snapshot, INDEX_TRACK_CALL_SITES_SETTING_KEY)?,
        git_ignore: required_bool(snapshot, INDEX_GIT_IGNORE_SETTING_KEY)?,
        diagnostics_prewarm: required_bool(snapshot, DIAGNOSTICS_PREWARM_SETTING_KEY)?,
        semantic: semantic_config_from_snapshot(snapshot)?,
        sync: SyncConfig {
            auto_watch: required_bool(snapshot, SYNC_AUTO_WATCH_SETTING_KEY)?,
            watch_debounce_ms: required_unsigned(snapshot, SYNC_WATCH_DEBOUNCE_MS_SETTING_KEY)?,
            watch_max_delay_ms: required_unsigned(snapshot, SYNC_WATCH_MAX_DELAY_MS_SETTING_KEY)?,
            watch_max_projects: required_usize(snapshot, SYNC_WATCH_MAX_PROJECTS_SETTING_KEY)?,
            read_refresh: required_bool(snapshot, SYNC_READ_REFRESH_SETTING_KEY)?,
            read_cooldown_secs: required_unsigned(snapshot, SYNC_READ_COOLDOWN_SECS_SETTING_KEY)?,
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
            max_concurrent_syncs: required_usize(snapshot, SYNC_MAX_CONCURRENT_SYNCS_SETTING_KEY)?,
            branch_gc_days: required_unsigned(snapshot, SYNC_BRANCH_GC_DAYS_SETTING_KEY)?,
            orphan_db_gc_days: required_unsigned(snapshot, SYNC_ORPHAN_DB_GC_DAYS_SETTING_KEY)?,
            auto_init: required_bool(snapshot, SYNC_AUTO_INIT_SETTING_KEY)?,
            auto_track_pr_branches: required_bool(
                snapshot,
                SYNC_AUTO_TRACK_PR_BRANCHES_SETTING_KEY,
            )?,
            auto_track_pr_poll_secs: required_unsigned(
                snapshot,
                SYNC_AUTO_TRACK_PR_POLL_SECS_SETTING_KEY,
            )?,
            retention: retention_config_from_snapshot(snapshot)?,
        },
        telemetry: TelemetryConfig {
            timings: required_bool(snapshot, TELEMETRY_TIMINGS_SETTING_KEY)?,
        },
    })
}

fn semantic_config_from_snapshot(snapshot: &ConfigurationSnapshotV1) -> Result<SemanticConfig> {
    let key = SettingKey::new(SEMANTIC_RUNTIME_SETTING_KEY).map_err(|error| {
        config_error(format!(
            "invalid runtime setting key '{SEMANTIC_RUNTIME_SETTING_KEY}': {error}"
        ))
    })?;
    let semantic = match snapshot.effective_values.get(&key) {
        None => SemanticConfig::default(),
        Some(ConfigurationValueV1::Text(value)) => {
            serde_json::from_str(value).map_err(|error| {
                config_error(format!(
                    "resolved semantic runtime setting is invalid: {error}"
                ))
            })?
        }
        Some(value) => {
            return Err(config_error(format!(
                "resolved configuration setting '{SEMANTIC_RUNTIME_SETTING_KEY}' has wrong type: expected text, got {:?}",
                value.kind()
            )));
        }
    };
    semantic.validate()?;
    Ok(semantic)
}

fn retention_config_from_snapshot(snapshot: &ConfigurationSnapshotV1) -> Result<RetentionConfig> {
    let key = SettingKey::new(SYNC_RETENTION_SETTING_KEY).map_err(|error| {
        config_error(format!(
            "invalid runtime setting key '{SYNC_RETENTION_SETTING_KEY}': {error}"
        ))
    })?;
    let retention = match snapshot.effective_values.get(&key) {
        None => RetentionConfig::default(),
        Some(ConfigurationValueV1::Text(value)) => {
            serde_json::from_str(value).map_err(|error| {
                config_error(format!("resolved retention setting is invalid: {error}"))
            })?
        }
        Some(value) => {
            return Err(config_error(format!(
                "resolved configuration setting '{SYNC_RETENTION_SETTING_KEY}' has wrong type: expected text, got {:?}",
                value.kind()
            )));
        }
    };
    retention.validate()?;
    Ok(retention)
}

fn required_setting<'a>(
    snapshot: &'a ConfigurationSnapshotV1,
    key_name: &str,
) -> Result<&'a ConfigurationValueV1> {
    let key = SettingKey::new(key_name).map_err(|error| {
        config_error(format!("invalid runtime setting key '{key_name}': {error}"))
    })?;
    snapshot.effective_values.get(&key).ok_or_else(|| {
        config_error(format!(
            "resolved configuration snapshot is missing required setting '{key_name}'",
        ))
    })
}

fn required_bool(snapshot: &ConfigurationSnapshotV1, key_name: &str) -> Result<bool> {
    match required_setting(snapshot, key_name)? {
        ConfigurationValueV1::Boolean(value) => Ok(*value),
        value => Err(config_error(format!(
            "resolved configuration setting '{key_name}' has wrong type: expected boolean, got {:?}",
            value.kind()
        ))),
    }
}

fn required_unsigned(snapshot: &ConfigurationSnapshotV1, key_name: &str) -> Result<u64> {
    match required_setting(snapshot, key_name)? {
        ConfigurationValueV1::Unsigned(value) => Ok(*value),
        value => Err(config_error(format!(
            "resolved configuration setting '{key_name}' has wrong type: expected unsigned, got {:?}",
            value.kind()
        ))),
    }
}

fn required_usize(snapshot: &ConfigurationSnapshotV1, key_name: &str) -> Result<usize> {
    let value = required_unsigned(snapshot, key_name)?;
    usize::try_from(value).map_err(|_| {
        config_error(format!(
            "resolved configuration setting '{key_name}' does not fit this platform",
        ))
    })
}

fn required_string_list(snapshot: &ConfigurationSnapshotV1, key_name: &str) -> Result<Vec<String>> {
    match required_setting(snapshot, key_name)? {
        ConfigurationValueV1::StringList(value) => Ok(value.clone()),
        value => Err(config_error(format!(
            "resolved configuration setting '{key_name}' has wrong type: expected string list, got {:?}",
            value.kind()
        ))),
    }
}

fn direct_mutation_for_runtime_config_diff(
    project_id: &ProjectId,
    before: &TraceDecayConfig,
    after: &TraceDecayConfig,
) -> Result<Option<crate::application::configuration::DirectConfigurationMutation>> {
    if before.version != after.version || before.root_dir != after.root_dir {
        return Err(config_error(
            "legacy configuration metadata cannot be mutated through the runtime control plane",
        ));
    }

    let mut mutations = Vec::new();
    push_runtime_change(
        &mut mutations,
        project_id,
        INDEX_EXCLUDE_SETTING_KEY,
        ConfigurationValueV1::StringList(before.exclude.clone()),
        ConfigurationValueV1::StringList(after.exclude.clone()),
    )?;
    push_runtime_change(
        &mut mutations,
        project_id,
        INDEX_INCLUDE_SETTING_KEY,
        ConfigurationValueV1::StringList(before.include.clone()),
        ConfigurationValueV1::StringList(after.include.clone()),
    )?;
    push_runtime_change(
        &mut mutations,
        project_id,
        INDEX_MAX_FILE_SIZE_SETTING_KEY,
        ConfigurationValueV1::Unsigned(before.max_file_size),
        ConfigurationValueV1::Unsigned(after.max_file_size),
    )?;
    push_runtime_change(
        &mut mutations,
        project_id,
        INDEX_EXTRACT_DOCSTRINGS_SETTING_KEY,
        ConfigurationValueV1::Boolean(before.extract_docstrings),
        ConfigurationValueV1::Boolean(after.extract_docstrings),
    )?;
    push_runtime_change(
        &mut mutations,
        project_id,
        INDEX_TRACK_CALL_SITES_SETTING_KEY,
        ConfigurationValueV1::Boolean(before.track_call_sites),
        ConfigurationValueV1::Boolean(after.track_call_sites),
    )?;
    push_runtime_change(
        &mut mutations,
        project_id,
        INDEX_GIT_IGNORE_SETTING_KEY,
        ConfigurationValueV1::Boolean(before.git_ignore),
        ConfigurationValueV1::Boolean(after.git_ignore),
    )?;
    push_runtime_change(
        &mut mutations,
        project_id,
        DIAGNOSTICS_PREWARM_SETTING_KEY,
        ConfigurationValueV1::Boolean(before.diagnostics_prewarm),
        ConfigurationValueV1::Boolean(after.diagnostics_prewarm),
    )?;
    push_runtime_change(
        &mut mutations,
        project_id,
        SEMANTIC_RUNTIME_SETTING_KEY,
        ConfigurationValueV1::Text(semantic_config_json(&before.semantic)?),
        ConfigurationValueV1::Text(semantic_config_json(&after.semantic)?),
    )?;
    push_runtime_change(
        &mut mutations,
        project_id,
        SYNC_AUTO_WATCH_SETTING_KEY,
        ConfigurationValueV1::Boolean(before.sync.auto_watch),
        ConfigurationValueV1::Boolean(after.sync.auto_watch),
    )?;
    push_runtime_change(
        &mut mutations,
        project_id,
        SYNC_WATCH_DEBOUNCE_MS_SETTING_KEY,
        ConfigurationValueV1::Unsigned(before.sync.watch_debounce_ms),
        ConfigurationValueV1::Unsigned(after.sync.watch_debounce_ms),
    )?;
    push_runtime_change(
        &mut mutations,
        project_id,
        SYNC_WATCH_MAX_DELAY_MS_SETTING_KEY,
        ConfigurationValueV1::Unsigned(before.sync.watch_max_delay_ms),
        ConfigurationValueV1::Unsigned(after.sync.watch_max_delay_ms),
    )?;
    push_runtime_change(
        &mut mutations,
        project_id,
        SYNC_WATCH_MAX_PROJECTS_SETTING_KEY,
        ConfigurationValueV1::Unsigned(before.sync.watch_max_projects as u64),
        ConfigurationValueV1::Unsigned(after.sync.watch_max_projects as u64),
    )?;
    push_runtime_change(
        &mut mutations,
        project_id,
        SYNC_READ_REFRESH_SETTING_KEY,
        ConfigurationValueV1::Boolean(before.sync.read_refresh),
        ConfigurationValueV1::Boolean(after.sync.read_refresh),
    )?;
    push_runtime_change(
        &mut mutations,
        project_id,
        SYNC_READ_COOLDOWN_SECS_SETTING_KEY,
        ConfigurationValueV1::Unsigned(before.sync.read_cooldown_secs),
        ConfigurationValueV1::Unsigned(after.sync.read_cooldown_secs),
    )?;
    push_runtime_change(
        &mut mutations,
        project_id,
        SYNC_SESSION_START_SYNC_SETTING_KEY,
        ConfigurationValueV1::Boolean(before.sync.session_start_sync),
        ConfigurationValueV1::Boolean(after.sync.session_start_sync),
    )?;
    push_runtime_change(
        &mut mutations,
        project_id,
        SYNC_SESSION_START_STALE_THRESHOLD_SECS_SETTING_KEY,
        ConfigurationValueV1::Unsigned(before.sync.session_start_stale_threshold_secs),
        ConfigurationValueV1::Unsigned(after.sync.session_start_stale_threshold_secs),
    )?;
    push_runtime_change(
        &mut mutations,
        project_id,
        SYNC_BACKSTOP_INTERVAL_MINS_SETTING_KEY,
        ConfigurationValueV1::Unsigned(before.sync.backstop_interval_mins),
        ConfigurationValueV1::Unsigned(after.sync.backstop_interval_mins),
    )?;
    push_runtime_change(
        &mut mutations,
        project_id,
        SYNC_FULL_SYNC_ESCALATION_FILES_SETTING_KEY,
        ConfigurationValueV1::Unsigned(before.sync.full_sync_escalation_files as u64),
        ConfigurationValueV1::Unsigned(after.sync.full_sync_escalation_files as u64),
    )?;
    push_runtime_change(
        &mut mutations,
        project_id,
        SYNC_MAX_CONCURRENT_SYNCS_SETTING_KEY,
        ConfigurationValueV1::Unsigned(before.sync.max_concurrent_syncs as u64),
        ConfigurationValueV1::Unsigned(after.sync.max_concurrent_syncs as u64),
    )?;
    push_runtime_change(
        &mut mutations,
        project_id,
        SYNC_BRANCH_GC_DAYS_SETTING_KEY,
        ConfigurationValueV1::Unsigned(before.sync.branch_gc_days),
        ConfigurationValueV1::Unsigned(after.sync.branch_gc_days),
    )?;
    push_runtime_change(
        &mut mutations,
        project_id,
        SYNC_ORPHAN_DB_GC_DAYS_SETTING_KEY,
        ConfigurationValueV1::Unsigned(before.sync.orphan_db_gc_days),
        ConfigurationValueV1::Unsigned(after.sync.orphan_db_gc_days),
    )?;
    push_runtime_change(
        &mut mutations,
        project_id,
        SYNC_AUTO_INIT_SETTING_KEY,
        ConfigurationValueV1::Boolean(before.sync.auto_init),
        ConfigurationValueV1::Boolean(after.sync.auto_init),
    )?;
    push_runtime_change(
        &mut mutations,
        project_id,
        SYNC_AUTO_TRACK_PR_BRANCHES_SETTING_KEY,
        ConfigurationValueV1::Boolean(before.sync.auto_track_pr_branches),
        ConfigurationValueV1::Boolean(after.sync.auto_track_pr_branches),
    )?;
    push_runtime_change(
        &mut mutations,
        project_id,
        SYNC_AUTO_TRACK_PR_POLL_SECS_SETTING_KEY,
        ConfigurationValueV1::Unsigned(before.sync.auto_track_pr_poll_secs),
        ConfigurationValueV1::Unsigned(after.sync.auto_track_pr_poll_secs),
    )?;
    push_runtime_change(
        &mut mutations,
        project_id,
        SYNC_RETENTION_SETTING_KEY,
        ConfigurationValueV1::Text(retention_config_json(&before.sync.retention)?),
        ConfigurationValueV1::Text(retention_config_json(&after.sync.retention)?),
    )?;
    push_runtime_change(
        &mut mutations,
        project_id,
        TELEMETRY_TIMINGS_SETTING_KEY,
        ConfigurationValueV1::Boolean(before.telemetry.timings),
        ConfigurationValueV1::Boolean(after.telemetry.timings),
    )?;

    Ok((!mutations.is_empty()).then_some(
        crate::application::configuration::DirectConfigurationMutation::Batch { mutations },
    ))
}

fn semantic_config_json(config: &SemanticConfig) -> Result<String> {
    config.validate()?;
    serde_json::to_string(config)
        .map_err(|error| config_error(format!("semantic runtime setting is invalid: {error}")))
}

fn retention_config_json(config: &RetentionConfig) -> Result<String> {
    config.validate()?;
    serde_json::to_string(config)
        .map_err(|error| config_error(format!("retention setting is invalid: {error}")))
}

fn push_runtime_change(
    mutations: &mut Vec<crate::application::configuration::DirectConfigurationMutation>,
    project_id: &ProjectId,
    key_name: &str,
    before: ConfigurationValueV1,
    after: ConfigurationValueV1,
) -> Result<()> {
    if before == after {
        return Ok(());
    }
    let key = SettingKey::new(key_name).map_err(|error| {
        config_error(format!("invalid runtime setting key '{key_name}': {error}"))
    })?;
    mutations.push(
        crate::application::configuration::DirectConfigurationMutation::Set {
            layer: ConfigurationLayerIdV1::Project {
                project_id: project_id.clone(),
            },
            key,
            value: after,
        },
    );
    Ok(())
}

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

/// Reads the `TRACEDECAY_<suffix>` environment variable.
pub fn brand_env(suffix: &str) -> Option<String> {
    std::env::var(format!("TRACEDECAY_{suffix}")).ok()
}

/// Returns the path to the configuration file (`config.json`) within the
/// resolved data directory.
pub fn get_config_path(project_root: &Path) -> PathBuf {
    if let Ok(layout) = crate::storage::resolve_layout_for_current_profile(project_root) {
        return layout.config_path;
    }
    get_tracedecay_dir(project_root).join(CONFIG_FILENAME)
}

pub async fn get_config_path_with_identity(project_root: &Path) -> PathBuf {
    if let Ok(layout) =
        crate::tracedecay::TraceDecay::resolve_store_layout_for_identity(project_root).await
    {
        return layout.config_path;
    }
    get_config_path(project_root)
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

pub async fn load_config_with_identity(project_root: &Path) -> Result<TraceDecayConfig> {
    let config_path = get_config_path_with_identity(project_root).await;
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

/// Writes a legacy configuration fixture using an atomic write.
///
/// Production runtime code must use the daemon control plane instead of this
/// compatibility helper. It remains for fixtures and legacy-input tests while
/// callers complete their migration.
pub fn save_config(project_root: &Path, config: &TraceDecayConfig) -> Result<()> {
    let config_path = get_config_path(project_root);
    save_config_to_path(&config_path, config)
}

pub async fn save_config_with_identity(
    project_root: &Path,
    config: &TraceDecayConfig,
) -> Result<()> {
    let config_path = get_config_path_with_identity(project_root).await;
    save_config_to_path(&config_path, config)
}

pub fn save_config_to_path(config_path: &Path, config: &TraceDecayConfig) -> Result<()> {
    let data_dir = config_path
        .parent()
        .ok_or_else(|| TraceDecayError::Config {
            message: format!(
                "configuration path '{}' has no parent directory",
                config_path.display()
            ),
        })?;
    crate::storage::PrivateStoreIo::create_dir_all(data_dir).map_err(|e| {
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

fn is_ignored_by_git(project_path: &Path, git_config_global: Option<&Path>) -> Option<bool> {
    let fallback_global_excludes = || {
        git_config_global
            .and_then(|path| is_ignored_by_explicit_global_excludes(project_path, path))
    };
    let dir_name = active_data_dir_name(project_path);
    let mut command = Command::new(crate::git::git_program());
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

fn is_ignored_by_explicit_global_excludes(
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

/// Appends the project marker dir name (`.tracedecay`) to the project's
/// `.gitignore`, creating the file if needed. Ensures the entry starts on its
/// own line (adds a trailing newline to existing content if missing).
pub fn add_to_gitignore(project_path: &Path) {
    let dir_name = active_data_dir_name(project_path);
    let gitignore = project_path.join(".gitignore");
    let mut content = fs::read_to_string(&gitignore).unwrap_or_default();
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(dir_name);
    content.push('\n');
    if let Err(e) = fs::write(&gitignore, content) {
        eprintln!("warning: failed to update .gitignore: {e}");
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

/// Like [`discover_project_root`], but on a sync miss checks the git worktree
/// root with [`crate::tracedecay::TraceDecay::has_initialized_store`] so renamed
/// or global-only repos still resolve without probing unrelated ancestors.
pub async fn discover_project_root_with_identity(start: &Path) -> Option<PathBuf> {
    if let Some(root) = discover_project_root(start) {
        return Some(root);
    }
    let candidate =
        crate::worktree::git_worktree_root(start).unwrap_or_else(|| start.to_path_buf());
    if crate::tracedecay::TraceDecay::has_initialized_store(&candidate).await {
        Some(candidate)
    } else {
        None
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
            .or_else(|| crate::worktree::git_worktree_root(&cwd))
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

/// Returns `true` if a directory should be entered because it or one of its
/// descendants matches an explicit include glob.
pub fn is_included_dir(dir_path: &str, config: &TraceDecayConfig) -> bool {
    let descendant_probe = format!("{dir_path}/_");
    any_pattern_matches(&config.include, &[dir_path, &descendant_probe])
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

/// Serializes test and benchmark code that mutates process-wide storage env
/// vars (`TRACEDECAY_DATA_DIR` and related HOME/profile pins).
///
/// Single source of truth: [`tracedecay_runtime_core::config`] owns the lock,
/// [`lock_user_data_dir_test_env`], and `PinnedUserDataDir`; this module only
/// re-exports them so every historical `crate::config::…` call site keeps
/// resolving. The lock and its accessor are unconditional there (not gated
/// behind `cfg(test)` / `feature = "test-helpers"`) because non-test code —
/// `src/sessions/session_temporal_benchmark.rs`, which is always compiled and
/// backs `cargo bench` — takes this lock outside a test build.
pub(crate) use tracedecay_runtime_core::config::lock_user_data_dir_test_env;

/// Pins [`USER_DATA_DIR_ENV`] and agent home discovery to an isolated temp
/// profile while holding the shared user-data-dir test lock, so parallel lib
/// tests cannot race profile resolution or scan live host transcripts during
/// `TraceDecay::init` / indexing.
#[cfg(test)]
pub(crate) use tracedecay_runtime_core::config::PinnedUserDataDir;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
