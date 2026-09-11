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

pub use tracedecay_domain::configuration::SEMANTIC_RUNTIME_SETTING_KEY;
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
    INDEX_TRACK_CALL_SITES_SETTING_KEY, SYNC_AUTO_TRACK_PR_BRANCHES_SETTING_KEY,
    SYNC_AUTO_TRACK_PR_POLL_SECS_SETTING_KEY, SettingKey, TELEMETRY_TIMINGS_SETTING_KEY,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_global_db::configuration::contracts::ConfigurationCurrentStateV1;
use tracedecay_semantic_contracts::SemanticConfig;

#[derive(Debug, Clone, PartialEq)]
pub struct TraceDecayConfig {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub max_file_size: u64,
    pub extract_docstrings: bool,
    pub track_call_sites: bool,
    pub git_ignore: bool,
    pub diagnostics_prewarm: bool,
    pub native_graph_activation: bool,
    pub semantic: SemanticConfig,
    pub sync: SyncConfig,
    pub telemetry: TelemetryConfig,
}

impl Default for TraceDecayConfig {
    fn default() -> Self {
        Self {
            include: Vec::new(),
            exclude: Vec::new(),
            max_file_size: 1_048_576,
            extract_docstrings: true,
            track_call_sites: true,
            git_ignore: true,
            diagnostics_prewarm: false,
            native_graph_activation: true,
            semantic: SemanticConfig::default(),
            sync: SyncConfig::default(),
            telemetry: TelemetryConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncConfig {
    pub auto_track_pr_branches: bool,
    pub auto_track_pr_poll_secs: u64,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            auto_track_pr_branches: false,
            auto_track_pr_poll_secs: 300,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelemetryConfig {
    pub timings: bool,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self { timings: true }
    }
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
    config: TraceDecayConfig,
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

    pub fn config(&self) -> &TraceDecayConfig {
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
fn runtime_config_from_snapshot(snapshot: &ConfigurationSnapshotV1) -> Result<TraceDecayConfig> {
    snapshot.validate().map_err(|error| {
        config_error(format!("invalid resolved configuration snapshot: {error}"))
    })?;
    Ok(TraceDecayConfig {
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
        semantic: semantic_config_from_snapshot(snapshot)?,
        sync: SyncConfig {
            auto_track_pr_branches: required_bool(
                snapshot,
                SYNC_AUTO_TRACK_PR_BRANCHES_SETTING_KEY,
            )?,
            auto_track_pr_poll_secs: required_unsigned(
                snapshot,
                SYNC_AUTO_TRACK_PR_POLL_SECS_SETTING_KEY,
            )?,
        },
        telemetry: TelemetryConfig {
            timings: required_bool(snapshot, TELEMETRY_TIMINGS_SETTING_KEY)?,
        },
    })
}

fn semantic_config_from_snapshot(snapshot: &ConfigurationSnapshotV1) -> Result<SemanticConfig> {
    let semantic = match optional_text_setting(snapshot, SEMANTIC_RUNTIME_SETTING_KEY)? {
        None => SemanticConfig::default(),
        Some(value) => serde_json::from_str(value).map_err(|error| {
            config_error(format!(
                "resolved semantic runtime setting is invalid: {error}"
            ))
        })?,
    };
    // Structural only: this crate is catalog-free. Membership of
    // `selected_model` is admitted at the configuration write boundary and
    // again by the lifecycle owner on selection, so a persisted id the
    // catalog no longer serves degrades semantics without blocking exact,
    // lexical, or graph retrieval behind an unpublishable configuration.
    semantic.validate()?;
    Ok(semantic)
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

/// An optional text setting (canonical JSON policy trees are stored as text).
/// Absence is `None`; presence with another type is an error.
pub fn optional_text_setting<'a>(
    snapshot: &'a ConfigurationSnapshotV1,
    key_name: &str,
) -> Result<Option<&'a str>> {
    match snapshot.effective_values.get(&setting_key(key_name)?) {
        None => Ok(None),
        Some(ConfigurationValueV1::Text(value)) => Ok(Some(value)),
        Some(value) => Err(config_error(format!(
            "resolved configuration setting '{key_name}' has wrong type: expected text, got {:?}",
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
        ConfigurationValueV1, INDEX_MAX_FILE_SIZE_SETTING_KEY, SettingKey,
    };
    use tracedecay_domain::errors::TraceDecayError;
    use tracedecay_semantic_contracts::SemanticConfig;

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
    fn pin_materializes_the_semantic_selection_from_the_snapshot() {
        let configured = SemanticConfig {
            auto_download: false,
            ..SemanticConfig::default()
        };
        let snapshot = resolved(BTreeMap::from([(
            SettingKey::new(super::SEMANTIC_RUNTIME_SETTING_KEY).unwrap(),
            ConfigurationValueV1::Text(serde_json::to_string(&configured).unwrap()),
        )]));

        let pinned = PinnedRuntimeConfiguration::new(target(), revision(), snapshot).unwrap();

        assert_eq!(pinned.config().semantic, configured);
        assert_ne!(pinned.config().semantic, SemanticConfig::default());
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
