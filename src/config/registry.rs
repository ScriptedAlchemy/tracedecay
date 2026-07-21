//! Typed configuration registry for the PR11 control plane.
//!
//! The legacy `config.json` model remains read-only migration input. This
//! registry is the only definition source for settings admitted into the new
//! revisioned control plane; adapters may not add their own defaults.

use std::collections::BTreeMap;

use thiserror::Error;
use tracedecay_domain::DomainError;
use tracedecay_domain::configuration::{
    ACCESS_RULES_SETTING_KEY, ANALYZER_SETTINGS_SETTING_KEY, AnalyzerSettingsV1,
    CONFIGURATION_SETTING_KEYS_V1, ConfigurationValueKindV1, ConfigurationValueV1,
    DEFAULT_COLLECTION_SETTING_KEY, DIAGNOSTICS_PREWARM_SETTING_KEY, DeprecationStateV1,
    INDEX_EXCLUDE_SETTING_KEY, INDEX_EXTRACT_DOCSTRINGS_SETTING_KEY, INDEX_GIT_IGNORE_SETTING_KEY,
    INDEX_INCLUDE_SETTING_KEY, INDEX_MAX_FILE_SIZE_SETTING_KEY, INDEX_TRACK_CALL_SITES_SETTING_KEY,
    RestartRequirementV1, SOURCE_BINDINGS_SETTING_KEY, SYNC_AUTO_INIT_SETTING_KEY,
    SYNC_AUTO_TRACK_PR_BRANCHES_SETTING_KEY, SYNC_AUTO_TRACK_PR_POLL_SECS_SETTING_KEY,
    SYNC_AUTO_WATCH_SETTING_KEY, SYNC_BACKSTOP_INTERVAL_MINS_SETTING_KEY,
    SYNC_BRANCH_GC_DAYS_SETTING_KEY, SYNC_FULL_SYNC_ESCALATION_FILES_SETTING_KEY,
    SYNC_MAX_CONCURRENT_SYNCS_SETTING_KEY, SYNC_ORPHAN_DB_GC_DAYS_SETTING_KEY,
    SYNC_READ_COOLDOWN_SECS_SETTING_KEY, SYNC_READ_REFRESH_SETTING_KEY,
    SYNC_SESSION_START_STALE_THRESHOLD_SECS_SETTING_KEY, SYNC_SESSION_START_SYNC_SETTING_KEY,
    SYNC_WATCH_DEBOUNCE_MS_SETTING_KEY, SYNC_WATCH_MAX_DELAY_MS_SETTING_KEY,
    SYNC_WATCH_MAX_PROJECTS_SETTING_KEY, SettingDefinitionV1, SettingKey, SettingScopeV1,
    SettingSensitivityV1, TELEMETRY_TIMINGS_SETTING_KEY, WORK_TOPOLOGY_POLICY_SETTING_KEY,
    safe_work_topology_policy_v1,
};

/// Read-only cutover contract; production readers remain intentionally
/// unwired until the configuration-control-plane migration boundary lands.
#[allow(dead_code)]
#[path = "legacy_decoder.rs"]
pub(crate) mod legacy_decoder;

/// Registry schema revision. Increment only when setting-definition semantics
/// change, not when a setting value changes.
pub const CONFIGURATION_REGISTRY_SCHEMA_REVISION: u16 = 1;

#[derive(Debug, Error)]
pub enum ConfigurationRegistryError {
    #[error("configuration definition is invalid: {0}")]
    InvalidDefinition(#[from] DomainError),
    #[error("setting key already registered: {0}")]
    DuplicateSetting(SettingKey),
    #[error("setting key is not registered: {0}")]
    UnknownSetting(SettingKey),
    #[error("setting value kind does not match {key}: expected {expected:?}, got {actual:?}")]
    ValueKindMismatch {
        key: SettingKey,
        expected: ConfigurationValueKindV1,
        actual: ConfigurationValueKindV1,
    },
    #[error("setting {key} cannot be written in layer {layer:?}")]
    InvalidLayer {
        key: SettingKey,
        layer: tracedecay_domain::configuration::ConfigurationLayerIdV1,
    },
}

/// Immutable mapping of every supported setting to its typed definition.
#[derive(Clone, Debug)]
pub struct ConfigurationRegistry {
    definitions: BTreeMap<SettingKey, SettingDefinitionV1>,
}

impl ConfigurationRegistry {
    /// Build the PR11 core registry. In addition to authority, policy,
    /// collection, analyzer, and topology definitions, this includes every
    /// non-authority scalar currently represented by legacy `config.json`.
    pub fn core() -> Result<Self, ConfigurationRegistryError> {
        let mut registry = Self {
            definitions: BTreeMap::new(),
        };
        registry.register(SettingDefinitionV1 {
            key: setting_key(SOURCE_BINDINGS_SETTING_KEY)?,
            schema_revision: CONFIGURATION_REGISTRY_SCHEMA_REVISION,
            value_kind: ConfigurationValueKindV1::SourceBindings,
            default_value: ConfigurationValueV1::SourceBindings(Vec::new()),
            sensitivity: SettingSensitivityV1::Sensitive,
            scope: SettingScopeV1::Project,
            restart_requirement: RestartRequirementV1::None,
            deprecation: DeprecationStateV1::Active,
        })?;
        registry.register(SettingDefinitionV1 {
            key: setting_key(ACCESS_RULES_SETTING_KEY)?,
            schema_revision: CONFIGURATION_REGISTRY_SCHEMA_REVISION,
            value_kind: ConfigurationValueKindV1::AccessRules,
            default_value: ConfigurationValueV1::AccessRules(Vec::new()),
            sensitivity: SettingSensitivityV1::Sensitive,
            scope: SettingScopeV1::Project,
            restart_requirement: RestartRequirementV1::None,
            deprecation: DeprecationStateV1::Active,
        })?;
        registry.register(SettingDefinitionV1 {
            key: setting_key(DEFAULT_COLLECTION_SETTING_KEY)?,
            schema_revision: CONFIGURATION_REGISTRY_SCHEMA_REVISION,
            value_kind: ConfigurationValueKindV1::DefaultCollection,
            default_value: ConfigurationValueV1::DefaultCollection(None),
            sensitivity: SettingSensitivityV1::Public,
            scope: SettingScopeV1::UserProfile,
            restart_requirement: RestartRequirementV1::None,
            deprecation: DeprecationStateV1::Active,
        })?;
        registry.register(SettingDefinitionV1 {
            key: setting_key(ANALYZER_SETTINGS_SETTING_KEY)?,
            schema_revision: CONFIGURATION_REGISTRY_SCHEMA_REVISION,
            value_kind: ConfigurationValueKindV1::AnalyzerSettings,
            default_value: ConfigurationValueV1::AnalyzerSettings(AnalyzerSettingsV1::empty()),
            sensitivity: SettingSensitivityV1::Sensitive,
            scope: SettingScopeV1::Project,
            restart_requirement: RestartRequirementV1::AnalyzerRestart,
            deprecation: DeprecationStateV1::Active,
        })?;
        registry.register(SettingDefinitionV1 {
            key: setting_key(WORK_TOPOLOGY_POLICY_SETTING_KEY)?,
            schema_revision: CONFIGURATION_REGISTRY_SCHEMA_REVISION,
            value_kind: ConfigurationValueKindV1::WorkTopologyPolicy,
            default_value: ConfigurationValueV1::WorkTopologyPolicy(Box::new(
                safe_work_topology_policy_v1(),
            )),
            sensitivity: SettingSensitivityV1::Sensitive,
            scope: SettingScopeV1::Project,
            restart_requirement: RestartRequirementV1::DaemonRestart,
            deprecation: DeprecationStateV1::Active,
        })?;
        register_legacy_project_settings(&mut registry)?;
        let expected = CONFIGURATION_SETTING_KEYS_V1
            .iter()
            .map(|key| setting_key(key))
            .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
        let actual = registry
            .definitions
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        if actual != expected {
            return Err(ConfigurationRegistryError::InvalidDefinition(
                DomainError::NonCanonical {
                    field: "configuration registry key inventory",
                },
            ));
        }
        Ok(registry)
    }

    pub fn register(
        &mut self,
        definition: SettingDefinitionV1,
    ) -> Result<(), ConfigurationRegistryError> {
        definition.validate()?;
        if self.definitions.contains_key(&definition.key) {
            return Err(ConfigurationRegistryError::DuplicateSetting(definition.key));
        }
        self.definitions.insert(definition.key.clone(), definition);
        Ok(())
    }

    pub fn definition(
        &self,
        key: &SettingKey,
    ) -> Result<&SettingDefinitionV1, ConfigurationRegistryError> {
        self.definitions
            .get(key)
            .ok_or_else(|| ConfigurationRegistryError::UnknownSetting(key.clone()))
    }

    pub fn definitions(&self) -> impl Iterator<Item = &SettingDefinitionV1> {
        self.definitions.values()
    }

    pub fn validate_value(
        &self,
        key: &SettingKey,
        value: &ConfigurationValueV1,
    ) -> Result<(), ConfigurationRegistryError> {
        let definition = self.definition(key)?;
        let actual = value.kind();
        if actual != definition.value_kind {
            return Err(ConfigurationRegistryError::ValueKindMismatch {
                key: key.clone(),
                expected: definition.value_kind,
                actual,
            });
        }
        value.validate()?;
        Ok(())
    }

    pub fn validate_layer(
        &self,
        key: &SettingKey,
        layer: &tracedecay_domain::configuration::ConfigurationLayerIdV1,
    ) -> Result<(), ConfigurationRegistryError> {
        use tracedecay_domain::configuration::{ConfigurationLayerKindV1, SettingScopeV1};

        let definition = self.definition(key)?;
        let valid = matches!(
            (definition.scope, layer.kind()),
            (
                SettingScopeV1::UserProfile,
                ConfigurationLayerKindV1::UserProfile
            ) | (SettingScopeV1::Project, ConfigurationLayerKindV1::Project)
                | (
                    SettingScopeV1::Collection,
                    ConfigurationLayerKindV1::Collection
                )
        );
        if valid {
            Ok(())
        } else {
            Err(ConfigurationRegistryError::InvalidLayer {
                key: key.clone(),
                layer: layer.clone(),
            })
        }
    }
}

/// Register the legacy scalar surface with the registry rather than allowing
/// a decoder or runtime adapter to invent defaults. The transition bridge
/// intentionally obtains the values from `TraceDecayConfig::default()` so the
/// pre-cutover default behavior remains the one source of truth.
fn register_legacy_project_settings(
    registry: &mut ConfigurationRegistry,
) -> Result<(), ConfigurationRegistryError> {
    let legacy = super::TraceDecayConfig::default();
    let sync = legacy.sync;
    let telemetry = legacy.telemetry;
    let settings = vec![
        (
            INDEX_EXCLUDE_SETTING_KEY,
            ConfigurationValueV1::StringList(legacy.exclude),
            SettingSensitivityV1::Sensitive,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            INDEX_INCLUDE_SETTING_KEY,
            ConfigurationValueV1::StringList(legacy.include),
            SettingSensitivityV1::Sensitive,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            INDEX_MAX_FILE_SIZE_SETTING_KEY,
            ConfigurationValueV1::Unsigned(legacy.max_file_size),
            SettingSensitivityV1::Public,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            INDEX_EXTRACT_DOCSTRINGS_SETTING_KEY,
            ConfigurationValueV1::Boolean(legacy.extract_docstrings),
            SettingSensitivityV1::Public,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            INDEX_TRACK_CALL_SITES_SETTING_KEY,
            ConfigurationValueV1::Boolean(legacy.track_call_sites),
            SettingSensitivityV1::Public,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            INDEX_GIT_IGNORE_SETTING_KEY,
            ConfigurationValueV1::Boolean(legacy.git_ignore),
            SettingSensitivityV1::Public,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            DIAGNOSTICS_PREWARM_SETTING_KEY,
            ConfigurationValueV1::Boolean(legacy.diagnostics_prewarm),
            SettingSensitivityV1::Public,
            RestartRequirementV1::None,
        ),
        (
            SYNC_AUTO_WATCH_SETTING_KEY,
            ConfigurationValueV1::Boolean(sync.auto_watch),
            SettingSensitivityV1::Public,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            SYNC_WATCH_DEBOUNCE_MS_SETTING_KEY,
            ConfigurationValueV1::Unsigned(sync.watch_debounce_ms),
            SettingSensitivityV1::Public,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            SYNC_WATCH_MAX_DELAY_MS_SETTING_KEY,
            ConfigurationValueV1::Unsigned(sync.watch_max_delay_ms),
            SettingSensitivityV1::Public,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            SYNC_WATCH_MAX_PROJECTS_SETTING_KEY,
            ConfigurationValueV1::Unsigned(sync.watch_max_projects as u64),
            SettingSensitivityV1::Public,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            SYNC_READ_REFRESH_SETTING_KEY,
            ConfigurationValueV1::Boolean(sync.read_refresh),
            SettingSensitivityV1::Public,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            SYNC_READ_COOLDOWN_SECS_SETTING_KEY,
            ConfigurationValueV1::Unsigned(sync.read_cooldown_secs),
            SettingSensitivityV1::Public,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            SYNC_SESSION_START_SYNC_SETTING_KEY,
            ConfigurationValueV1::Boolean(sync.session_start_sync),
            SettingSensitivityV1::Public,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            SYNC_SESSION_START_STALE_THRESHOLD_SECS_SETTING_KEY,
            ConfigurationValueV1::Unsigned(sync.session_start_stale_threshold_secs),
            SettingSensitivityV1::Public,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            SYNC_BACKSTOP_INTERVAL_MINS_SETTING_KEY,
            ConfigurationValueV1::Unsigned(sync.backstop_interval_mins),
            SettingSensitivityV1::Public,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            SYNC_FULL_SYNC_ESCALATION_FILES_SETTING_KEY,
            ConfigurationValueV1::Unsigned(sync.full_sync_escalation_files as u64),
            SettingSensitivityV1::Public,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            SYNC_MAX_CONCURRENT_SYNCS_SETTING_KEY,
            ConfigurationValueV1::Unsigned(sync.max_concurrent_syncs as u64),
            SettingSensitivityV1::Public,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            SYNC_BRANCH_GC_DAYS_SETTING_KEY,
            ConfigurationValueV1::Unsigned(sync.branch_gc_days),
            SettingSensitivityV1::Public,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            SYNC_ORPHAN_DB_GC_DAYS_SETTING_KEY,
            ConfigurationValueV1::Unsigned(sync.orphan_db_gc_days),
            SettingSensitivityV1::Public,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            SYNC_AUTO_INIT_SETTING_KEY,
            ConfigurationValueV1::Boolean(sync.auto_init),
            SettingSensitivityV1::Public,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            SYNC_AUTO_TRACK_PR_BRANCHES_SETTING_KEY,
            ConfigurationValueV1::Boolean(sync.auto_track_pr_branches),
            SettingSensitivityV1::Public,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            SYNC_AUTO_TRACK_PR_POLL_SECS_SETTING_KEY,
            ConfigurationValueV1::Unsigned(
                sync.auto_track_pr_poll_secs
                    .max(super::MIN_AUTO_TRACK_PR_POLL_SECS),
            ),
            SettingSensitivityV1::Public,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            TELEMETRY_TIMINGS_SETTING_KEY,
            ConfigurationValueV1::Boolean(telemetry.timings),
            SettingSensitivityV1::Public,
            RestartRequirementV1::None,
        ),
    ];

    for (key, default_value, sensitivity, restart_requirement) in settings {
        registry.register(SettingDefinitionV1 {
            key: setting_key(key)?,
            schema_revision: CONFIGURATION_REGISTRY_SCHEMA_REVISION,
            value_kind: default_value.kind(),
            default_value,
            sensitivity,
            scope: SettingScopeV1::Project,
            restart_requirement,
            deprecation: DeprecationStateV1::Active,
        })?;
    }
    Ok(())
}

fn setting_key(value: &str) -> Result<SettingKey, ConfigurationRegistryError> {
    Ok(SettingKey::new(value)?)
}
