use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock, RwLock};

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

pub mod registry;
pub mod resolver;
pub mod retrieval;
pub mod scope_control;
pub mod topology;

/// Name of the legacy configuration migration input stored inside the data
/// directory. It is not a runtime authority and production code must never
/// rewrite it.
pub const CONFIG_FILENAME: &str = "config.json";

/// Name of the hidden directory used to store `TraceDecay` metadata.
pub const TRACEDECAY_DIR: &str = ".tracedecay";

/// Environment variable that pins the user-level `TraceDecay` data directory.
pub const USER_DATA_DIR_ENV: &str = "TRACEDECAY_DATA_DIR";

/// Project graph database filename inside a `.tracedecay/` data dir.
pub const DB_FILENAME: &str = "tracedecay.db";

/// Atomic project-scoped semantic runtime selection.
///
/// The value is canonical JSON for [`SemanticConfig`]. Keeping the active
/// profile, rollback profile, and local resource ceilings under one setting
/// prevents a configuration revision from exposing a partially updated
/// semantic selection.
pub const SEMANTIC_RUNTIME_SETTING_KEY: &str = "semantic.runtime.v1";

/// Atomic daemon retention/compaction policy tree (Plan 38).
///
/// The value is canonical JSON for [`RetentionConfig`]. Keeping the session
/// (LCM), observation-evidence, orphan-store, debris, and compaction windows
/// under one setting keeps the retention engines threaded as a single
/// versioned unit the daemon backstop reads, mirroring the semantic key. Absent
/// or unset resolves to [`RetentionConfig::default`]'s bounded safe policy.
pub const SYNC_RETENTION_SETTING_KEY: &str = "sync.retention.v1";

/// Default `FastEmbed` catalog model selected on install (offline-safe).
pub const DEFAULT_FASTEMBED_MODEL_ID: &str = "JinaEmbeddingsV2BaseCode";

/// Cataloged `FastEmbed` model ids settings may select. Membership is validated
/// here without depending on the `semantic_code` acquisition module.
const CATALOGED_FASTEMBED_MODEL_IDS: &[&str] = &[DEFAULT_FASTEMBED_MODEL_ID];

const MAX_SEMANTIC_MODEL_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_SEMANTIC_TOKENIZER_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_SEMANTIC_RESIDENT_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_SEMANTIC_THREADS: u32 = 64;
const MAX_SEMANTIC_CONCURRENT_SESSIONS: u32 = 64;
const MAX_SEMANTIC_BATCH_SIZE: u32 = 4096;
const MAX_SEMANTIC_SEQUENCE_LENGTH: u32 = 32768;
const MAX_SEMANTIC_LOAD_DEADLINE_MS: u64 = 10 * 60 * 1000;

/// One explicitly installed local profile. Runtime code receives this path
/// from the pinned configuration snapshot and never searches an ambient model
/// cache or derives a download location from the profile identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticProfileSelection {
    pub profile_id: String,
    pub accepted_profile_digest: tracedecay_domain::ManifestDigest,
    pub artifact_digest: String,
    pub artifact_path: PathBuf,
}

impl SemanticProfileSelection {
    fn validate(&self) -> Result<()> {
        self.accepted_profile_digest
            .validate()
            .map_err(|error| config_error(format!("semantic accepted profile digest: {error}")))?;
        if self.profile_id.trim().is_empty() || self.profile_id.len() > 128 {
            return Err(config_error(
                "semantic profile_id must be non-empty and at most 128 bytes",
            ));
        }
        if self.artifact_digest.len() != 64
            || !self
                .artifact_digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(config_error(
                "semantic artifact_digest must be 64 lowercase hexadecimal characters",
            ));
        }
        if !self.artifact_path.is_absolute()
            || self
                .artifact_path
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(config_error(
                "semantic artifact_path must be an absolute normalized local path",
            ));
        }
        Ok(())
    }
}

/// Process ceilings applied before an installed semantic profile is admitted.
///
/// The selected artifact manifest may impose tighter limits. These local
/// ceilings never authorize a profile to exceed its own declared bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticResourceCeilings {
    pub max_model_bytes: u64,
    pub max_tokenizer_bytes: u64,
    pub max_resident_bytes: u64,
    pub max_threads: u32,
    pub max_concurrent_sessions: u32,
    pub max_batch_size: u32,
    pub max_sequence_length: u32,
    pub load_deadline_ms: u64,
}

impl Default for SemanticResourceCeilings {
    fn default() -> Self {
        Self {
            max_model_bytes: 700 * 1024 * 1024,
            max_tokenizer_bytes: 64 * 1024 * 1024,
            max_resident_bytes: 2 * 1024 * 1024 * 1024,
            max_threads: 4,
            max_concurrent_sessions: 1,
            max_batch_size: 32,
            max_sequence_length: 512,
            load_deadline_ms: 30_000,
        }
    }
}

impl SemanticResourceCeilings {
    fn validate(self) -> Result<()> {
        let valid = self.max_model_bytes > 0
            && self.max_model_bytes <= MAX_SEMANTIC_MODEL_BYTES
            && self.max_tokenizer_bytes > 0
            && self.max_tokenizer_bytes <= MAX_SEMANTIC_TOKENIZER_BYTES
            && self.max_resident_bytes > 0
            && self.max_resident_bytes <= MAX_SEMANTIC_RESIDENT_BYTES
            && self.max_model_bytes <= self.max_resident_bytes
            && self.max_tokenizer_bytes <= self.max_resident_bytes
            && (1..=MAX_SEMANTIC_THREADS).contains(&self.max_threads)
            && (1..=MAX_SEMANTIC_CONCURRENT_SESSIONS).contains(&self.max_concurrent_sessions)
            && (1..=MAX_SEMANTIC_BATCH_SIZE).contains(&self.max_batch_size)
            && (1..=MAX_SEMANTIC_SEQUENCE_LENGTH).contains(&self.max_sequence_length)
            && (1..=MAX_SEMANTIC_LOAD_DEADLINE_MS).contains(&self.load_deadline_ms);
        if !valid {
            return Err(config_error(
                "semantic resource ceilings are zero, incoherent, or exceed supported maxima",
            ));
        }
        Ok(())
    }
}

/// Pinned semantic runtime selection.
///
/// A catalog model id (default `JinaEmbeddingsV2BaseCode`) selects the
/// `FastEmbed` package `TraceDecay` will acquire in the background. `None`
/// disables the optional semantic lane while exact, lexical, and graph
/// retrieval remain healthy. Installed local profiles remain explicit and are
/// never inferred by scanning an ambient model cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticConfig {
    /// Cataloged `FastEmbed` model id, or `None` to disable semantics.
    #[serde(default = "default_selected_fastembed_model")]
    pub selected_model: Option<String>,
    /// When true, first daemon startup / selection queues background download.
    #[serde(default = "default_true")]
    pub auto_download: bool,
    #[serde(default)]
    pub active_profile: Option<SemanticProfileSelection>,
    #[serde(default)]
    pub rollback_profile: Option<SemanticProfileSelection>,
    #[serde(default)]
    pub resources: SemanticResourceCeilings,
}

fn default_selected_fastembed_model() -> Option<String> {
    Some(DEFAULT_FASTEMBED_MODEL_ID.to_owned())
}

fn default_true() -> bool {
    true
}

impl Default for SemanticConfig {
    fn default() -> Self {
        Self {
            selected_model: default_selected_fastembed_model(),
            auto_download: true,
            active_profile: None,
            rollback_profile: None,
            resources: SemanticResourceCeilings::default(),
        }
    }
}

impl SemanticConfig {
    pub fn validate(&self) -> Result<()> {
        self.resources.validate()?;
        if let Some(model_id) = self.selected_model.as_ref() {
            if model_id.trim().is_empty() || model_id.len() > 128 {
                return Err(config_error(
                    "semantic selected_model must be a non-empty catalog id at most 128 bytes",
                ));
            }
            if !CATALOGED_FASTEMBED_MODEL_IDS.contains(&model_id.as_str()) {
                return Err(config_error(format!(
                    "semantic selected_model '{model_id}' is not a cataloged FastEmbed model"
                )));
            }
        }
        if let Some(active) = self.active_profile.as_ref() {
            active.validate()?;
        }
        if let Some(rollback) = self.rollback_profile.as_ref() {
            rollback.validate()?;
            if self.active_profile.is_none() {
                return Err(config_error(
                    "semantic rollback profile requires an active profile",
                ));
            }
        }
        if self.active_profile == self.rollback_profile && self.active_profile.is_some() {
            return Err(config_error(
                "semantic active and rollback profiles must be distinct",
            ));
        }
        Ok(())
    }
}

/// Directory-name segments treated as generated or vendored content:
/// build output, package-manager caches, and vendored dependencies.
///
/// This is the single source of truth for "what counts as generated" and is
/// shared by four call sites that used to hand-maintain independent lists
/// which had drifted out of sync with each other:
///
/// - [`is_excluded`] / [`default_exclude_patterns`] below (config-driven,
///   glob-pattern based — this list seeds the *default* patterns, but a
///   project's `config.exclude` can still be overridden by the user).
/// - `tracedecay::scan::TraceDecay::is_skipped_dir_hint` (an informational
///   hint only; the authoritative gate there is still [`is_excluded_dir`]).
/// - `migrate::inventory::should_prune_dir` (authoritative directory prune
///   during migration inventory scans).
/// - `mcp::tools::handlers::redundancy::is_generated_path` (candidate
///   filtering for the duplicate-code scanner).
///
/// Each call site may still layer its own local additions on top where
/// something is specific to that tool's purpose (see call-site comments);
/// this list only covers the shared "generated/vendored" core.
pub const GENERATED_DIR_SEGMENTS: &[&str] = &[
    ".cache",
    ".gradle",
    ".next",
    ".turbo",
    ".venv",
    ".worktrees",
    "__pycache__",
    "build",
    "coverage",
    "dist",
    "node_modules",
    "out",
    "target",
    "vendor",
    "venv",
];

/// Returns `true` if `segment` (a single path component, e.g. a directory
/// name) is one of the shared [`GENERATED_DIR_SEGMENTS`].
pub fn is_generated_dir_segment(segment: &str) -> bool {
    GENERATED_DIR_SEGMENTS.contains(&segment)
}

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

/// Reads the sync settings from the already-pinned runtime snapshot.
///
/// This compatibility name intentionally no longer reads `config.json`, opens
/// a store, or applies an environment override at call time.
pub fn load_sync_config(project_root: &Path) -> Result<SyncConfig> {
    cached_sync_config(project_root)
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

/// Reads telemetry settings from the already-pinned runtime snapshot.
///
/// This compatibility name intentionally performs no file, database, or IPC
/// access. A missing authority is an error; hooks handle that by disabling
/// optional telemetry rather than inventing a fallback value.
pub fn load_telemetry_config(project_root: &Path) -> Result<TelemetryConfig> {
    cached_telemetry_config(project_root)
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

/// Narrow daemon/client seam for direct configuration mutations. The daemon
/// implementation must authenticate the caller and invoke the Wave-1
/// `ConfigurationControlPlane`; this adapter deliberately has no store handle
/// and cannot infer authority from a path.
pub trait ConfigurationDaemonClient: Send + Sync {
    fn mutate_direct(
        &self,
        target: RuntimeConfigurationTarget,
        mutation: crate::application::configuration::DirectConfigurationMutation,
        expected_revision: ConfigurationRevisionId,
    ) -> RuntimeConfigurationFuture<'_, PinnedRuntimeConfiguration>;
}

pub type RuntimeConfigurationFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

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

fn runtime_configuration_cache() -> &'static RuntimeConfigurationCache {
    static CACHE: OnceLock<RuntimeConfigurationCache> = OnceLock::new();
    CACHE.get_or_init(RuntimeConfigurationCache::default)
}

#[derive(Default)]
struct ConfigurationDaemonClients {
    by_project: BTreeMap<String, Arc<dyn ConfigurationDaemonClient>>,
    fallback: Option<Arc<dyn ConfigurationDaemonClient>>,
}

fn configuration_daemon_client_slot() -> &'static RwLock<ConfigurationDaemonClients> {
    static CLIENT: OnceLock<RwLock<ConfigurationDaemonClients>> = OnceLock::new();
    CLIENT.get_or_init(|| RwLock::new(ConfigurationDaemonClients::default()))
}

/// Installs a fallback daemon-owned mutation client for compatibility callers
/// that have no authoritative project route.
pub fn install_configuration_daemon_client(client: Arc<dyn ConfigurationDaemonClient>) {
    configuration_daemon_client_slot()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .fallback = Some(client);
}

/// Installs one daemon-owned client for its exact project identity. CLI, MCP,
/// HTTP, and dashboard callers select this mapping from the already-pinned
/// target rather than opening a configuration store or re-resolving a path.
pub fn install_configuration_daemon_client_for_project(
    target: &RuntimeConfigurationTarget,
    client: Arc<dyn ConfigurationDaemonClient>,
) {
    configuration_daemon_client_slot()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .by_project
        .insert(target.project_id.as_str().to_owned(), client);
}

/// Removes one project's daemon client if (and only if) the installed client
/// is the exact instance being released. The `Arc::ptr_eq` guard keeps a
/// dropping runtime from clobbering a newer client installed by a live
/// handle for the same project (e.g. the daemon's close-and-reopen
/// handshake). Without this release, any non-daemon process that opened a
/// project retained the store `Arc` — and its exclusive sessions.db writer
/// lease — in this process-global slot for its entire lifetime, starving the
/// managed daemon of the single-writer lock.
pub fn uninstall_configuration_daemon_client_for_project(
    target: &RuntimeConfigurationTarget,
    client: &Arc<dyn ConfigurationDaemonClient>,
) {
    let mut clients = configuration_daemon_client_slot()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let project_id = target.project_id.as_str();
    if clients
        .by_project
        .get(project_id)
        .is_some_and(|existing| Arc::ptr_eq(existing, client))
    {
        clients.by_project.remove(project_id);
    }
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
        let legacy_target = registry::legacy_decoder::LegacyConfigurationDecodeTargetV1 {
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
    let client = {
        let clients = configuration_daemon_client_slot()
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clients.fallback.clone().or_else(|| {
            clients
                .by_project
                .get(current.target.project_id.as_str())
                .cloned()
        })
    }
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
    client: &dyn ConfigurationDaemonClient,
    current: &PinnedRuntimeConfiguration,
    mutation: crate::application::configuration::DirectConfigurationMutation,
) -> Result<PinnedRuntimeConfiguration> {
    let next = client
        .mutate_direct(
            current.target.clone(),
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
    let next = next.retarget(current.target.clone())?;
    runtime_configuration_cache().insert(next.clone())?;
    Ok(next)
}

/// Decodes a legacy file only as migration input. This function never writes
/// the file and callers must pass the already-authorized target layer/revision
/// supplied by the control-plane migration.
pub fn read_legacy_configuration_inputs(
    config_path: &Path,
    environment: &BTreeMap<String, String>,
    target: &registry::legacy_decoder::LegacyConfigurationDecodeTargetV1,
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
    registry::legacy_decoder::decode_legacy_configuration_inputs(&config_json, environment, target)
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

/// Returns the project marker directory for the given project root.
///
/// New runtime storage lives in the user-level profile shard. The project root
/// only carries lightweight marker/config files under `.tracedecay/`.
pub fn get_tracedecay_dir(project_root: &Path) -> PathBuf {
    project_root.join(TRACEDECAY_DIR)
}

/// Name of the project marker directory for this project root.
pub fn active_data_dir_name(project_root: &Path) -> &'static str {
    let _ = project_root;
    TRACEDECAY_DIR
}

/// Database filename appropriate for the given data directory.
pub fn db_filename(data_dir: &Path) -> &'static str {
    let _ = data_dir;
    DB_FILENAME
}

/// Full path to the repo-local graph database marker path.
///
/// Normal runtime graph storage resolves through `crate::storage::StoreLayout`
/// into the user profile shard; this helper is only for explicit marker checks
/// and migration cleanup.
pub fn get_project_db_path(project_root: &Path) -> PathBuf {
    get_tracedecay_dir(project_root).join(DB_FILENAME)
}

/// Returns true when the old repo-local `TraceDecay` graph DB exists at this root.
pub fn has_project_database(project_root: &Path) -> bool {
    project_root.join(TRACEDECAY_DIR).join(DB_FILENAME).exists()
}

/// User-level data directory. Runtime storage is always rooted at
/// `~/.tracedecay` unless `TRACEDECAY_DATA_DIR` explicitly overrides it.
pub fn user_data_dir() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(USER_DATA_DIR_ENV).filter(|path| !path.is_empty()) {
        return Some(nextest_isolated_user_data_dir(canonicalize_data_dir(
            PathBuf::from(path),
        )));
    }
    let home = dirs::home_dir()?;
    Some(canonicalize_data_dir(home.join(TRACEDECAY_DIR)))
}

fn nextest_isolated_user_data_dir(path: PathBuf) -> PathBuf {
    use std::hash::{Hash, Hasher};

    let Some(test_name) = std::env::var_os("NEXTEST_TEST_NAME").filter(|name| !name.is_empty())
    else {
        return path;
    };
    let Some(profile_dir) = path.parent() else {
        return path;
    };
    if path.file_name() != Some(std::ffi::OsStr::new(TRACEDECAY_DIR)) {
        return path;
    }

    let profile_name = profile_dir.file_name().and_then(std::ffi::OsStr::to_str);
    let target_profile = profile_name == Some("test-profile")
        && profile_dir
            .parent()
            .is_some_and(|target| target.join("debug").is_dir());
    let ci_profile =
        profile_name == Some("tracedecay-test-profile") && std::env::var_os("CI").is_some();
    if !target_profile && !ci_profile {
        return path;
    }

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::env::var_os("NEXTEST_RUN_ID")
        .unwrap_or_default()
        .to_string_lossy()
        .hash(&mut hasher);
    std::env::var_os("NEXTEST_ATTEMPT_ID")
        .unwrap_or_default()
        .to_string_lossy()
        .hash(&mut hasher);
    std::env::var_os("NEXTEST_BINARY_ID")
        .unwrap_or_default()
        .to_string_lossy()
        .hash(&mut hasher);
    test_name.to_string_lossy().hash(&mut hasher);
    path.join("nextest")
        .join(format!("{:016x}", hasher.finish()))
}

fn canonicalize_data_dir(path: PathBuf) -> PathBuf {
    if !path.is_absolute() {
        return path;
    }
    canonicalize_path_or_existing_parent(&path)
}

fn canonicalize_path_or_existing_parent(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }

    let mut current = path;
    let mut missing_suffix = PathBuf::new();
    while let Some(name) = current.file_name() {
        missing_suffix = Path::new(name).join(missing_suffix);
        let Some(parent) = current.parent() else {
            break;
        };
        current = parent;
        if let Ok(canonical_parent) = current.canonicalize() {
            return canonical_parent.join(missing_suffix);
        }
    }

    path.to_path_buf()
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

/// Walks from `start` upward looking for an initialised repo marker
/// (`.tracedecay/tracedecay.db`) or a profile-storage enrollment marker
/// (`.tracedecay/enrollment.json`).
///
/// Returns the first ancestor directory (inclusive) that contains an
/// initialised `TraceDecay` project, or `None` if the filesystem root is
/// reached without finding one.
///
/// # Canonical local project-root resolution order
///
/// This walk-up is the heart of project-root resolution. Every entry point
/// that needs a project root should resolve it in this order — new code must
/// converge on this chain instead of inventing its own:
///
/// 0. **Template pre-filter** (`serve` only,
///    [`crate::serve::sanitize_serve_path_arg`]): an explicit path that is a literal
///    unexpanded `${...}` host template variable (e.g. `${workspaceFolder}`
///    from a host that failed to expand it) is discarded with a warning and
///    resolution continues as if no path was given.
/// 1. **Explicit path** (`--path`/`-p`, tool `path` argument): used verbatim,
///    no discovery, and failure to open is fatal — never silently fall back.
/// 2. **CWD walk-up** (this function via [`resolve_path_with_discovery`]):
///    nearest ancestor of the working directory containing an initialised
///    project database (see [`get_project_db_path`]).
///
/// `serve` forwards this routing metadata to the managed daemon. MCP
/// `initialize` roots and registry aliases are resolved there; the proxy never
/// opens a project or global database and has no in-process fallback.
pub fn discover_project_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    let worktree_root = crate::worktree::git_worktree_root(start);
    loop {
        if has_project_database(&dir)
            || crate::storage::has_enrollment_marker(&dir)
            || crate::storage::has_path_local_profile_store(&dir)
        {
            return Some(dir);
        }
        if worktree_root
            .as_ref()
            .is_some_and(|root| paths_same(&dir, root))
        {
            return None;
        }
        if !dir.pop() {
            return None;
        }
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

fn paths_same(left: &Path, right: &Path) -> bool {
    let left = std::fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = std::fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left == right
}

/// Returns `true` if the path matches any of the configured `include` patterns.
///
/// This is used to allow hidden (dot-prefixed) directories that would
/// otherwise be skipped by the file walker.
pub fn is_included(path: &str, config: &TraceDecayConfig) -> bool {
    let match_opts = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: false,
        require_literal_leading_dot: false,
    };

    for pattern_str in &config.include {
        if let Ok(pattern) = Pattern::new(pattern_str)
            && pattern.matches_with(path, match_opts)
        {
            return true;
        }
    }

    false
}

/// Returns `true` if a directory should be entered because it or one of its
/// descendants matches an explicit include glob.
pub fn is_included_dir(dir_path: &str, config: &TraceDecayConfig) -> bool {
    let match_opts = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: false,
        require_literal_leading_dot: false,
    };

    for pattern_str in &config.include {
        if let Ok(pattern) = Pattern::new(pattern_str)
            && (pattern.matches_with(dir_path, match_opts)
                || pattern.matches_with(&format!("{dir_path}/_"), match_opts))
        {
            return true;
        }
    }

    false
}

/// Returns `true` if a directory should be pruned during scanning.
///
/// Matches `dir/_` against exclude patterns (for `dir/**`-style globs) and
/// also matches `dir` itself (for bare `**/dirname`-style globs).  This
/// ensures that patterns like `**/node_modules` and `**/node_modules/**`
/// both trigger directory pruning in `scan_files_walkdir`.
pub fn is_excluded_dir(dir_path: &str, config: &TraceDecayConfig) -> bool {
    let match_opts = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: false,
        require_literal_leading_dot: false,
    };

    for pattern_str in &config.exclude {
        if let Ok(pattern) = Pattern::new(pattern_str) {
            // Try both the dummy-file probe (catches dir/**) and the bare
            // directory path (catches **/dirname).
            if pattern.matches_with(&format!("{dir_path}/_"), match_opts)
                || pattern.matches_with(dir_path, match_opts)
            {
                return true;
            }
        }
    }

    false
}

/// Returns `true` if the file matches any of the configured exclude patterns.
pub fn is_excluded(file_path: &str, config: &TraceDecayConfig) -> bool {
    let match_opts = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: false,
        require_literal_leading_dot: false,
    };

    for pattern_str in &config.exclude {
        if let Ok(pattern) = Pattern::new(pattern_str)
            && pattern.matches_with(file_path, match_opts)
        {
            return true;
        }
    }

    false
}

/// Serializes test and benchmark code that mutates process-wide storage env
/// vars (`TRACEDECAY_DATA_DIR` and related HOME/profile pins).
static USER_DATA_DIR_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Acquires [`USER_DATA_DIR_TEST_LOCK`], recovering even when poisoned.
pub(crate) fn lock_user_data_dir_test_env() -> std::sync::MutexGuard<'static, ()> {
    USER_DATA_DIR_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Pins [`USER_DATA_DIR_ENV`] and agent home discovery to an isolated temp
/// profile while holding [`USER_DATA_DIR_TEST_LOCK`], so parallel lib tests
/// cannot race profile resolution or scan live host transcripts during
/// `TraceDecay::init` / indexing.
#[cfg(test)]
pub struct PinnedUserDataDir {
    _lock: std::sync::MutexGuard<'static, ()>,
    _root: tempfile::TempDir,
    previous: Option<OsString>,
    previous_home: Option<OsString>,
    previous_userprofile: Option<OsString>,
}

#[cfg(test)]
impl PinnedUserDataDir {
    pub fn new() -> Self {
        let lock = lock_user_data_dir_test_env();
        let root = tempfile::TempDir::new()
            .unwrap_or_else(|err| panic!("failed to create temp profile dir: {err}"));
        let profile = root.path().join(TRACEDECAY_DIR);
        crate::storage::PrivateStoreIo::create_dir_all(&profile)
            .unwrap_or_else(|err| panic!("failed to create isolated profile root: {err}"));
        let previous = std::env::var_os(USER_DATA_DIR_ENV);
        let previous_home = std::env::var_os("HOME");
        let previous_userprofile = std::env::var_os("USERPROFILE");
        unsafe {
            std::env::set_var(USER_DATA_DIR_ENV, &profile);
            std::env::set_var("HOME", root.path());
            std::env::set_var("USERPROFILE", root.path());
        }
        Self {
            _lock: lock,
            _root: root,
            previous,
            previous_home,
            previous_userprofile,
        }
    }
}

#[cfg(test)]
impl Default for PinnedUserDataDir {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl Drop for PinnedUserDataDir {
    fn drop(&mut self) {
        unsafe {
            match self.previous.take() {
                Some(previous) => std::env::set_var(USER_DATA_DIR_ENV, previous),
                None => std::env::remove_var(USER_DATA_DIR_ENV),
            }
            match self.previous_home.take() {
                Some(previous) => std::env::set_var("HOME", previous),
                None => std::env::remove_var("HOME"),
            }
            match self.previous_userprofile.take() {
                Some(previous) => std::env::set_var("USERPROFILE", previous),
                None => std::env::remove_var("USERPROFILE"),
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
