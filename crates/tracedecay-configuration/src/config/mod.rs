//! Runtime pin surfaces and control-plane config helpers.
//!
//! Retrieval-profile evaluation stays in `tracedecay-application::config::retrieval`.
//! The re-export rows below are surfaces `tracedecay-global-db` and
//! `tracedecay-domain` already own, kept under the `crate::config::…`
//! spelling so call sites share one import path.

pub mod analyzer;
pub mod model;
pub mod scope_control;
pub mod topology;
pub mod work_executable_binding;

pub use tracedecay_global_db::configuration::{registry, resolver};
#[cfg(test)]
pub use tracedecay_runtime_core::config::PinnedUserDataDir;

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use tracedecay_domain::ProjectId;
use tracedecay_domain::configuration::{
    ConfigurationRevisionId, ConfigurationSnapshotV1, ConfigurationValueV1,
    DIAGNOSTICS_PREWARM_SETTING_KEY, INDEX_EXCLUDE_SETTING_KEY,
    INDEX_EXTRACT_DOCSTRINGS_SETTING_KEY, INDEX_GIT_IGNORE_SETTING_KEY, INDEX_INCLUDE_SETTING_KEY,
    INDEX_MAX_FILE_SIZE_SETTING_KEY, INDEX_NATIVE_GRAPH_ACTIVATION_SETTING_KEY,
    INDEX_TRACK_CALL_SITES_SETTING_KEY, SYNC_AUTO_INIT_SETTING_KEY,
    SYNC_AUTO_TRACK_PR_BRANCHES_SETTING_KEY, SYNC_AUTO_TRACK_PR_POLL_SECS_SETTING_KEY,
    SYNC_AUTO_WATCH_SETTING_KEY, SYNC_BACKSTOP_INTERVAL_MINS_SETTING_KEY,
    SYNC_BRANCH_GC_DAYS_SETTING_KEY, SYNC_FULL_SYNC_ESCALATION_FILES_SETTING_KEY,
    SYNC_MAX_CONCURRENT_SYNCS_SETTING_KEY, SYNC_READ_COOLDOWN_SECS_SETTING_KEY,
    SYNC_READ_REFRESH_SETTING_KEY,
    SYNC_SESSION_START_STALE_THRESHOLD_SECS_SETTING_KEY, SYNC_SESSION_START_SYNC_SETTING_KEY,
    SYNC_WATCH_DEBOUNCE_MS_SETTING_KEY, SYNC_WATCH_LINKED_WORKTREES_SETTING_KEY,
    SYNC_WATCH_MAX_DELAY_MS_SETTING_KEY, SYNC_WATCH_MAX_PROJECTS_SETTING_KEY, SettingKey,
    TELEMETRY_TIMINGS_SETTING_KEY,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_global_db::configuration::contracts::ConfigurationCurrentStateV1;

use model::{RetentionConfig, SyncConfig, TelemetryConfig};

/// Settings decoded from one resolved configuration snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeTraceDecayConfig {
    /// Glob patterns for paths to index despite the default hidden-directory,
    /// generated-directory, and gitignore filters.
    pub include: Vec<String>,
    /// Glob patterns for files to exclude during indexing.
    pub exclude: Vec<String>,
    /// Maximum file size in bytes; larger files are skipped.
    pub max_file_size: u64,
    pub extract_docstrings: bool,
    pub track_call_sites: bool,
    pub git_ignore: bool,
    /// A cold `tracedecay_diagnostics` call prewarms in the background instead
    /// of blocking on the dependency build.
    pub diagnostics_prewarm: bool,
    /// Whether the persistent native code graph may activate. Disabling it
    /// leaves exact and lexical retrieval available.
    pub native_graph_activation: bool,
    pub sync: SyncConfig,
    pub telemetry: TelemetryConfig,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeConfigurationTarget {
    pub project_id: ProjectId,
    pub project_root: PathBuf,
}

/// One validated binding of a configuration revision to the runtime settings
/// decoded from it.
///
/// The fields are private: [`Self::new`] is the only constructor and it
/// decodes the snapshot exactly once, so a pin can never pair a snapshot with
/// settings it did not produce. The snapshot is shared, because a pin is
/// cloned on every publication and cached read; those clones must not copy
/// the whole snapshot.
#[derive(Clone, Debug)]
pub struct PinnedRuntimeConfiguration {
    target: RuntimeConfigurationTarget,
    revision_id: ConfigurationRevisionId,
    snapshot: Arc<ConfigurationSnapshotV1>,
    config: RuntimeTraceDecayConfig,
}

impl PinnedRuntimeConfiguration {
    /// Decodes `snapshot` into the runtime settings. Missing or wrongly typed
    /// required settings are rejected rather than defaulted.
    pub fn new(
        target: RuntimeConfigurationTarget,
        revision_id: ConfigurationRevisionId,
        snapshot: ConfigurationSnapshotV1,
    ) -> Result<Self> {
        let config = runtime_config_from_snapshot(&snapshot)?;
        Ok(Self {
            target,
            revision_id,
            snapshot: Arc::new(snapshot),
            config,
        })
    }

    pub fn target(&self) -> &RuntimeConfigurationTarget {
        &self.target
    }

    pub fn revision_id(&self) -> &ConfigurationRevisionId {
        &self.revision_id
    }

    pub fn snapshot(&self) -> &ConfigurationSnapshotV1 {
        &self.snapshot
    }

    pub fn config(&self) -> &RuntimeTraceDecayConfig {
        &self.config
    }

    /// The same revision and settings routed under another root of the same
    /// registered project. The root is display/routing context only, so
    /// nothing is decoded again.
    pub fn with_project_root(mut self, project_root: &Path) -> Self {
        self.target.project_root = project_root.to_path_buf();
        self
    }

    /// The revision/snapshot pair as the store-level current state, for
    /// callers that own it by value. The snapshot is copied only while
    /// another pin still shares it.
    pub fn into_current_state(self) -> ConfigurationCurrentStateV1 {
        ConfigurationCurrentStateV1 {
            revision_id: self.revision_id,
            snapshot: Arc::unwrap_or_clone(self.snapshot),
        }
    }
}

pub struct OpenedRuntimeConfiguration {
    pub(crate) configuration: PinnedRuntimeConfiguration,
    pub(crate) registered_database: RegisteredGlobalDbLeaseV1,
}

impl OpenedRuntimeConfiguration {
    pub fn new(
        configuration: PinnedRuntimeConfiguration,
        registered_database: RegisteredGlobalDbLeaseV1,
    ) -> Self {
        Self {
            configuration,
            registered_database,
        }
    }
}

/// Process-wide pin cache used by daemon invocation after project-open
/// publishes a snapshot. Opening durable configuration from a registered
/// store stays with the composition root, which owns that store.
pub trait PinnedRuntimeConfigurationCachePort: Send + Sync {
    fn publish(&self, configuration: PinnedRuntimeConfiguration) -> Result<()>;

    fn cached_for_root(&self, project_root: &Path) -> Result<PinnedRuntimeConfiguration>;
}

static PINNED_RUNTIME_CONFIGURATION_CACHE: OnceLock<Arc<dyn PinnedRuntimeConfigurationCachePort>> =
    OnceLock::new();

pub fn install_pinned_runtime_configuration_cache(
    cache: Arc<dyn PinnedRuntimeConfigurationCachePort>,
) -> Result<()> {
    PINNED_RUNTIME_CONFIGURATION_CACHE
        .set(cache)
        .map_err(|_| config_error("pinned runtime configuration cache is already installed"))
}

fn pinned_runtime_configuration_cache() -> Result<&'static dyn PinnedRuntimeConfigurationCachePort>
{
    PINNED_RUNTIME_CONFIGURATION_CACHE
        .get()
        .map(Arc::as_ref)
        .ok_or_else(|| config_error("pinned runtime configuration cache is not installed"))
}

pub fn publish_pinned_runtime_configuration(
    configuration: PinnedRuntimeConfiguration,
) -> Result<()> {
    pinned_runtime_configuration_cache()?.publish(configuration)
}

pub fn cached_pinned_runtime_configuration(
    project_root: &Path,
) -> Result<PinnedRuntimeConfiguration> {
    pinned_runtime_configuration_cache()?.cached_for_root(project_root)
}

/// Converts a complete typed snapshot into the runtime settings every
/// configuration consumer shares. There are no defaults, file reads, or
/// environment reads: an absent or mistyped required setting is an error.
#[hotpath::measure(label = "configuration.runtime.materialize")]
fn runtime_config_from_snapshot(
    snapshot: &ConfigurationSnapshotV1,
) -> Result<RuntimeTraceDecayConfig> {
    snapshot.validate().map_err(|error| {
        config_error(format!("invalid resolved configuration snapshot: {error}"))
    })?;
    Ok(RuntimeTraceDecayConfig {
        include: required_string_list(snapshot, INDEX_INCLUDE_SETTING_KEY)?,
        exclude: required_string_list(snapshot, INDEX_EXCLUDE_SETTING_KEY)?,
        max_file_size: required_unsigned(snapshot, INDEX_MAX_FILE_SIZE_SETTING_KEY)?,
        extract_docstrings: required_bool(snapshot, INDEX_EXTRACT_DOCSTRINGS_SETTING_KEY)?,
        track_call_sites: required_bool(snapshot, INDEX_TRACK_CALL_SITES_SETTING_KEY)?,
        git_ignore: required_bool(snapshot, INDEX_GIT_IGNORE_SETTING_KEY)?,
        diagnostics_prewarm: required_bool(snapshot, DIAGNOSTICS_PREWARM_SETTING_KEY)?,
        native_graph_activation: required_bool(
            snapshot,
            INDEX_NATIVE_GRAPH_ACTIVATION_SETTING_KEY,
        )?,
        sync: SyncConfig {
            auto_watch: required_bool(snapshot, SYNC_AUTO_WATCH_SETTING_KEY)?,
            watch_linked_worktrees: required_bool(
                snapshot,
                SYNC_WATCH_LINKED_WORKTREES_SETTING_KEY,
            )?,
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
            auto_init: required_bool(snapshot, SYNC_AUTO_INIT_SETTING_KEY)?,
            auto_track_pr_branches: required_bool(
                snapshot,
                SYNC_AUTO_TRACK_PR_BRANCHES_SETTING_KEY,
            )?,
            auto_track_pr_poll_secs: required_unsigned(
                snapshot,
                SYNC_AUTO_TRACK_PR_POLL_SECS_SETTING_KEY,
            )?,
            // Retention is not a registered setting, so a snapshot cannot
            // carry retention policy.
            retention: RetentionConfig::default(),
        },
        telemetry: TelemetryConfig {
            timings: required_bool(snapshot, TELEMETRY_TIMINGS_SETTING_KEY)?,
        },
    })
}

fn setting_key(key_name: &str) -> Result<SettingKey> {
    SettingKey::new(key_name)
        .map_err(|error| config_error(format!("invalid runtime setting key '{key_name}': {error}")))
}

/// Typed readers over a resolved snapshot. Every runtime materializer (this
/// crate's shared settings and the daemon-only policy the composition root
/// layers on top) reads through these, so a missing or mistyped setting is
/// reported identically wherever it is consumed.
pub fn required_setting<'a>(
    snapshot: &'a ConfigurationSnapshotV1,
    key_name: &str,
) -> Result<&'a ConfigurationValueV1> {
    let key = setting_key(key_name)?;
    snapshot.effective_values.get(&key).ok_or_else(|| {
        config_error(format!(
            "resolved configuration snapshot is missing required setting '{key_name}'",
        ))
    })
}

pub fn required_bool(snapshot: &ConfigurationSnapshotV1, key_name: &str) -> Result<bool> {
    match required_setting(snapshot, key_name)? {
        ConfigurationValueV1::Boolean(value) => Ok(*value),
        value => Err(config_error(format!(
            "resolved configuration setting '{key_name}' has wrong type: expected boolean, got {:?}",
            value.kind()
        ))),
    }
}

pub fn required_unsigned(snapshot: &ConfigurationSnapshotV1, key_name: &str) -> Result<u64> {
    match required_setting(snapshot, key_name)? {
        ConfigurationValueV1::Unsigned(value) => Ok(*value),
        value => Err(config_error(format!(
            "resolved configuration setting '{key_name}' has wrong type: expected unsigned, got {:?}",
            value.kind()
        ))),
    }
}

pub fn required_usize(snapshot: &ConfigurationSnapshotV1, key_name: &str) -> Result<usize> {
    let value = required_unsigned(snapshot, key_name)?;
    usize::try_from(value).map_err(|_| {
        config_error(format!(
            "resolved configuration setting '{key_name}' does not fit this platform",
        ))
    })
}

pub fn required_string_list(
    snapshot: &ConfigurationSnapshotV1,
    key_name: &str,
) -> Result<Vec<String>> {
    match required_setting(snapshot, key_name)? {
        ConfigurationValueV1::StringList(value) => Ok(value.clone()),
        value => Err(config_error(format!(
            "resolved configuration setting '{key_name}' has wrong type: expected string list, got {:?}",
            value.kind()
        ))),
    }
}

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use tracedecay_domain::ProjectId;
    use tracedecay_domain::configuration::{
        ConfigurationLayerIdV1, ConfigurationRevisionId, ConfigurationSnapshotV1,
        ConfigurationValueV1, INDEX_MAX_FILE_SIZE_SETTING_KEY, SYNC_WATCH_DEBOUNCE_MS_SETTING_KEY,
        SettingKey,
    };
    use tracedecay_domain::errors::TraceDecayError;

    use super::{PinnedRuntimeConfiguration, RuntimeConfigurationTarget, registry, resolver};

    fn target() -> RuntimeConfigurationTarget {
        RuntimeConfigurationTarget {
            project_id: ProjectId::new("project.pinned-runtime".to_owned()).unwrap(),
            project_root: PathBuf::from("/project"),
        }
    }

    fn revision() -> ConfigurationRevisionId {
        ConfigurationRevisionId::new("configuration.revision.pinned-runtime").unwrap()
    }

    fn resolved(entries: BTreeMap<SettingKey, ConfigurationValueV1>) -> ConfigurationSnapshotV1 {
        let layers = if entries.is_empty() {
            Vec::new()
        } else {
            vec![resolver::ConfigurationLayerV1 {
                layer: ConfigurationLayerIdV1::Project {
                    project_id: target().project_id,
                },
                revision_id: revision(),
                entries,
            }]
        };
        resolver::resolve_configuration(&registry::ConfigurationRegistry::core().unwrap(), &layers)
            .unwrap()
            .snapshot
    }

    fn config_message(error: TraceDecayError) -> String {
        match error {
            TraceDecayError::Config { message } => message,
            other => panic!("expected a typed configuration error, got {other:?}"),
        }
    }

    #[test]
    fn pin_rejects_a_snapshot_missing_a_required_setting() {
        let complete = resolved(BTreeMap::new());
        let key = SettingKey::new(INDEX_MAX_FILE_SIZE_SETTING_KEY).unwrap();
        let mut values = complete.effective_values.clone();
        let mut provenance = complete.provenance.clone();
        values.remove(&key);
        provenance.remove(&key);
        let incomplete = ConfigurationSnapshotV1::new(values, provenance).unwrap();

        let message = config_message(
            PinnedRuntimeConfiguration::new(target(), revision(), incomplete).unwrap_err(),
        );
        assert!(
            message.contains(INDEX_MAX_FILE_SIZE_SETTING_KEY) && message.contains("missing"),
            "missing required settings must name the key, not default it: {message}"
        );
    }

    #[test]
    fn pin_decodes_daemon_sync_settings_and_rejects_their_absence() {
        let complete = resolved(BTreeMap::new());
        let pinned =
            PinnedRuntimeConfiguration::new(target(), revision(), complete.clone()).unwrap();
        let key = SettingKey::new(SYNC_WATCH_DEBOUNCE_MS_SETTING_KEY).unwrap();
        assert_eq!(
            complete.effective_values.get(&key),
            Some(&ConfigurationValueV1::Unsigned(
                pinned.config().sync.watch_debounce_ms
            ))
        );

        let mut values = complete.effective_values.clone();
        let mut provenance = complete.provenance.clone();
        values.remove(&key);
        provenance.remove(&key);
        let incomplete = ConfigurationSnapshotV1::new(values, provenance).unwrap();
        let message = config_message(
            PinnedRuntimeConfiguration::new(target(), revision(), incomplete).unwrap_err(),
        );
        assert!(
            message.contains(SYNC_WATCH_DEBOUNCE_MS_SETTING_KEY) && message.contains("missing"),
            "{message}"
        );
    }

    #[test]
    fn pin_rejects_a_required_setting_with_the_wrong_type() {
        let complete = resolved(BTreeMap::new());
        let key = SettingKey::new(INDEX_MAX_FILE_SIZE_SETTING_KEY).unwrap();
        let mut values = complete.effective_values.clone();
        values.insert(key, ConfigurationValueV1::Boolean(true));
        let mistyped = ConfigurationSnapshotV1::new(values, complete.provenance.clone()).unwrap();

        let message = config_message(
            PinnedRuntimeConfiguration::new(target(), revision(), mistyped).unwrap_err(),
        );
        assert!(
            message.contains("expected unsigned"),
            "type mismatches must be reported as such: {message}"
        );
    }

    #[test]
    fn retargeting_shares_the_snapshot_and_keeps_the_revision() {
        let pinned =
            PinnedRuntimeConfiguration::new(target(), revision(), resolved(BTreeMap::new()))
                .unwrap();
        let snapshot_id = pinned.snapshot().snapshot_id.clone();

        let moved = pinned
            .clone()
            .with_project_root(&PathBuf::from("/elsewhere"));

        assert_eq!(moved.target().project_id, target().project_id);
        assert_eq!(moved.target().project_root, PathBuf::from("/elsewhere"));
        assert_eq!(moved.revision_id(), pinned.revision_id());
        assert_eq!(moved.snapshot().snapshot_id, snapshot_id);
        assert!(
            std::ptr::eq(moved.snapshot(), pinned.snapshot()),
            "a retargeted pin must share, not copy, its snapshot"
        );
        assert_eq!(moved.config(), pinned.config());
    }
}
