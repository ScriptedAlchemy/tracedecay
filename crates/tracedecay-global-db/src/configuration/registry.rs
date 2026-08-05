//! Typed configuration registry for the final control plane.

use std::collections::BTreeMap;

use thiserror::Error;
use tracedecay_domain::DomainError;
use tracedecay_domain::configuration::{
    ACCESS_RULES_SETTING_KEY, ANALYZER_SETTINGS_SETTING_KEY, AnalyzerSettingsV1,
    CONFIGURATION_SETTING_KEYS_V1, CONTEXT_SCOUT_SETTINGS_SETTING_KEY, ConfigurationValueKindV1,
    ConfigurationValueV1, ContextScoutSettingsV1, DEFAULT_COLLECTION_SETTING_KEY,
    DIAGNOSTICS_PREWARM_SETTING_KEY, DeprecationStateV1, INDEX_EXCLUDE_SETTING_KEY,
    INDEX_EXTRACT_DOCSTRINGS_SETTING_KEY, INDEX_GIT_IGNORE_SETTING_KEY, INDEX_INCLUDE_SETTING_KEY,
    INDEX_MAX_FILE_SIZE_SETTING_KEY, INDEX_TRACK_CALL_SITES_SETTING_KEY, RestartRequirementV1,
    SEMANTIC_RUNTIME_SETTING_KEY, SOURCE_BINDINGS_SETTING_KEY, SYNC_AUTO_INIT_SETTING_KEY,
    SYNC_AUTO_TRACK_PR_BRANCHES_SETTING_KEY, SYNC_AUTO_TRACK_PR_POLL_SECS_SETTING_KEY,
    SYNC_AUTO_WATCH_SETTING_KEY, SYNC_BACKSTOP_INTERVAL_MINS_SETTING_KEY,
    SYNC_BRANCH_GC_DAYS_SETTING_KEY, SYNC_FULL_SYNC_ESCALATION_FILES_SETTING_KEY,
    SYNC_MAX_CONCURRENT_SYNCS_SETTING_KEY, SYNC_ORPHAN_DB_GC_DAYS_SETTING_KEY,
    SYNC_READ_COOLDOWN_SECS_SETTING_KEY, SYNC_READ_REFRESH_SETTING_KEY,
    SYNC_SESSION_START_STALE_THRESHOLD_SECS_SETTING_KEY, SYNC_SESSION_START_SYNC_SETTING_KEY,
    SYNC_WATCH_DEBOUNCE_MS_SETTING_KEY, SYNC_WATCH_MAX_DELAY_MS_SETTING_KEY,
    SYNC_WATCH_MAX_PROJECTS_SETTING_KEY, SettingDefinitionV1, SettingKey, SettingScopeV1,
    SettingSensitivityV1, TELEMETRY_TIMINGS_SETTING_KEY, WORK_EXECUTABLE_BINDINGS_SETTING_KEY,
    WORK_TOPOLOGY_POLICY_SETTING_KEY, safe_work_topology_policy_v1,
};
use tracedecay_domain::feedback::PROXIMITY_RISK_THRESHOLD_SETTING_KEY_V1;

use super::semantic::SemanticConfig;

/// Canonical Plan 20 default for configured-tier proximity warnings.
pub const DEFAULT_PROXIMITY_RISK_THRESHOLD_BASIS_POINTS_V1: u64 = 7_000;
pub const MAX_PROXIMITY_RISK_THRESHOLD_BASIS_POINTS_V1: u64 = 10_000;

/// Registry schema revision. Increment only when setting-definition semantics
/// change, not when a setting value changes.
pub const CONFIGURATION_REGISTRY_SCHEMA_REVISION: u16 = 2;

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
    #[error("setting {key} value {actual} is outside [{minimum}, {maximum}]")]
    UnsignedValueOutOfRange {
        key: SettingKey,
        minimum: u64,
        maximum: u64,
        actual: u64,
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
    /// project-scoped runtime scalar.
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
        registry.register(SettingDefinitionV1 {
            key: setting_key(WORK_EXECUTABLE_BINDINGS_SETTING_KEY)?,
            schema_revision: CONFIGURATION_REGISTRY_SCHEMA_REVISION,
            value_kind: ConfigurationValueKindV1::WorkExecutableBindings,
            default_value: ConfigurationValueV1::WorkExecutableBindings(Vec::new()),
            sensitivity: SettingSensitivityV1::Sensitive,
            scope: SettingScopeV1::Project,
            restart_requirement: RestartRequirementV1::DaemonRestart,
            deprecation: DeprecationStateV1::Active,
        })?;
        registry.register(SettingDefinitionV1 {
            key: setting_key(CONTEXT_SCOUT_SETTINGS_SETTING_KEY)?,
            schema_revision: CONFIGURATION_REGISTRY_SCHEMA_REVISION,
            value_kind: ConfigurationValueKindV1::ContextScoutSettings,
            default_value: ConfigurationValueV1::ContextScoutSettings(
                ContextScoutSettingsV1::disabled(),
            ),
            sensitivity: SettingSensitivityV1::Sensitive,
            scope: SettingScopeV1::Project,
            restart_requirement: RestartRequirementV1::None,
            deprecation: DeprecationStateV1::Active,
        })?;
        registry.register(SettingDefinitionV1 {
            key: setting_key(PROXIMITY_RISK_THRESHOLD_SETTING_KEY_V1)?,
            schema_revision: CONFIGURATION_REGISTRY_SCHEMA_REVISION,
            value_kind: ConfigurationValueKindV1::Unsigned,
            default_value: ConfigurationValueV1::Unsigned(
                DEFAULT_PROXIMITY_RISK_THRESHOLD_BASIS_POINTS_V1,
            ),
            sensitivity: SettingSensitivityV1::Public,
            scope: SettingScopeV1::Project,
            restart_requirement: RestartRequirementV1::None,
            deprecation: DeprecationStateV1::Active,
        })?;
        let semantic_default = SemanticConfig::default();
        semantic_default.validate().map_err(|_| {
            ConfigurationRegistryError::InvalidDefinition(DomainError::NonCanonical {
                field: "semantic runtime default",
            })
        })?;
        let semantic_default = serde_json::to_string(&semantic_default).map_err(|_| {
            ConfigurationRegistryError::InvalidDefinition(DomainError::NonCanonical {
                field: "semantic runtime default encoding",
            })
        })?;
        registry.register(SettingDefinitionV1 {
            key: setting_key(SEMANTIC_RUNTIME_SETTING_KEY)?,
            schema_revision: CONFIGURATION_REGISTRY_SCHEMA_REVISION,
            value_kind: ConfigurationValueKindV1::Text,
            default_value: ConfigurationValueV1::Text(semantic_default),
            sensitivity: SettingSensitivityV1::Sensitive,
            scope: SettingScopeV1::Project,
            restart_requirement: RestartRequirementV1::AnalyzerRestart,
            deprecation: DeprecationStateV1::Active,
        })?;
        register_project_settings(&mut registry)?;
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
        if key.as_str() == PROXIMITY_RISK_THRESHOLD_SETTING_KEY_V1 {
            let ConfigurationValueV1::Unsigned(actual) = value else {
                return Err(ConfigurationRegistryError::ValueKindMismatch {
                    key: key.clone(),
                    expected: ConfigurationValueKindV1::Unsigned,
                    actual: value.kind(),
                });
            };
            if *actual > MAX_PROXIMITY_RISK_THRESHOLD_BASIS_POINTS_V1 {
                return Err(ConfigurationRegistryError::UnsignedValueOutOfRange {
                    key: key.clone(),
                    minimum: 0,
                    maximum: MAX_PROXIMITY_RISK_THRESHOLD_BASIS_POINTS_V1,
                    actual: *actual,
                });
            }
        }
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

/// Lower bound the daemon clamps PR-branch auto-tracking polling up to.
///
/// Mirrors root `config::MIN_AUTO_TRACK_PR_POLL_SECS`.
pub const MIN_AUTO_TRACK_PR_POLL_SECS: u64 = 60;

/// Canonical defaults for the project-scoped runtime settings.
struct ProjectDefaults {
    exclude: Vec<String>,
    include: Vec<String>,
    max_file_size: u64,
    extract_docstrings: bool,
    track_call_sites: bool,
    git_ignore: bool,
    diagnostics_prewarm: bool,
    telemetry_timings: bool,
    sync: SyncDefaults,
}

#[derive(Clone, Copy)]
struct SyncDefaults {
    auto_watch: bool,
    watch_debounce_ms: u64,
    watch_max_delay_ms: u64,
    watch_max_projects: usize,
    read_refresh: bool,
    read_cooldown_secs: u64,
    session_start_sync: bool,
    session_start_stale_threshold_secs: u64,
    backstop_interval_mins: u64,
    full_sync_escalation_files: usize,
    max_concurrent_syncs: usize,
    branch_gc_days: u64,
    orphan_db_gc_days: u64,
    auto_init: bool,
    auto_track_pr_branches: bool,
    auto_track_pr_poll_secs: u64,
}

impl Default for SyncDefaults {
    fn default() -> Self {
        Self {
            auto_watch: true,
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
            orphan_db_gc_days: 7,
            auto_init: true,
            auto_track_pr_branches: false,
            auto_track_pr_poll_secs: 300,
        }
    }
}

impl Default for ProjectDefaults {
    fn default() -> Self {
        let mut exclude: Vec<String> = vec![
            ".git/**".to_string(),
            ".tracedecay/**".to_string(),
            "bin/**".to_string(),
            "**/*.min.*".to_string(),
        ];
        for segment in tracedecay_runtime_core::config::GENERATED_DIR_SEGMENTS {
            exclude.push(format!("{segment}/**"));
            exclude.push(format!("**/{segment}/**"));
        }
        Self {
            exclude,
            include: Vec::new(),
            max_file_size: 1_048_576,
            extract_docstrings: true,
            track_call_sites: true,
            git_ignore: true,
            diagnostics_prewarm: false,
            telemetry_timings: true,
            sync: SyncDefaults::default(),
        }
    }
}

/// Register every project scalar in the sole typed registry.
fn register_project_settings(
    registry: &mut ConfigurationRegistry,
) -> Result<(), ConfigurationRegistryError> {
    let defaults = ProjectDefaults::default();
    let sync = defaults.sync;
    let settings = vec![
        (
            INDEX_EXCLUDE_SETTING_KEY,
            ConfigurationValueV1::StringList(defaults.exclude),
            SettingSensitivityV1::Sensitive,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            INDEX_INCLUDE_SETTING_KEY,
            ConfigurationValueV1::StringList(defaults.include),
            SettingSensitivityV1::Sensitive,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            INDEX_MAX_FILE_SIZE_SETTING_KEY,
            ConfigurationValueV1::Unsigned(defaults.max_file_size),
            SettingSensitivityV1::Public,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            INDEX_EXTRACT_DOCSTRINGS_SETTING_KEY,
            ConfigurationValueV1::Boolean(defaults.extract_docstrings),
            SettingSensitivityV1::Public,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            INDEX_TRACK_CALL_SITES_SETTING_KEY,
            ConfigurationValueV1::Boolean(defaults.track_call_sites),
            SettingSensitivityV1::Public,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            INDEX_GIT_IGNORE_SETTING_KEY,
            ConfigurationValueV1::Boolean(defaults.git_ignore),
            SettingSensitivityV1::Public,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            DIAGNOSTICS_PREWARM_SETTING_KEY,
            ConfigurationValueV1::Boolean(defaults.diagnostics_prewarm),
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
                    .max(MIN_AUTO_TRACK_PR_POLL_SECS),
            ),
            SettingSensitivityV1::Public,
            RestartRequirementV1::DaemonRestart,
        ),
        (
            TELEMETRY_TIMINGS_SETTING_KEY,
            ConfigurationValueV1::Boolean(defaults.telemetry_timings),
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

#[cfg(test)]
mod proximity_threshold_tests {
    use super::*;

    #[test]
    fn proximity_threshold_has_one_bounded_project_default() {
        let registry = ConfigurationRegistry::core().expect("registry");
        let key = SettingKey::new(PROXIMITY_RISK_THRESHOLD_SETTING_KEY_V1).expect("key");
        let definition = registry.definition(&key).expect("definition");

        assert_eq!(definition.value_kind, ConfigurationValueKindV1::Unsigned);
        assert_eq!(
            definition.default_value,
            ConfigurationValueV1::Unsigned(DEFAULT_PROXIMITY_RISK_THRESHOLD_BASIS_POINTS_V1)
        );
        assert_eq!(definition.scope, SettingScopeV1::Project);
        assert_eq!(definition.sensitivity, SettingSensitivityV1::Public);
        assert_eq!(definition.restart_requirement, RestartRequirementV1::None);
        assert!(
            registry
                .validate_value(&key, &ConfigurationValueV1::Unsigned(0))
                .is_ok()
        );
        assert!(
            registry
                .validate_value(
                    &key,
                    &ConfigurationValueV1::Unsigned(MAX_PROXIMITY_RISK_THRESHOLD_BASIS_POINTS_V1),
                )
                .is_ok()
        );
        assert!(matches!(
            registry.validate_value(
                &key,
                &ConfigurationValueV1::Unsigned(MAX_PROXIMITY_RISK_THRESHOLD_BASIS_POINTS_V1 + 1),
            ),
            Err(ConfigurationRegistryError::UnsignedValueOutOfRange { .. })
        ));
    }
}

#[cfg(test)]
mod context_scout_tests {
    use super::*;
    use tracedecay_domain::configuration::{
        ContextScoutConfigurationModeV1, ContextScoutConfigurationStateV1,
    };

    #[test]
    fn stock_context_scout_configuration_is_explicitly_disabled() {
        let registry = ConfigurationRegistry::core().unwrap();
        let definition = registry
            .definition(&SettingKey::new(CONTEXT_SCOUT_SETTINGS_SETTING_KEY).unwrap())
            .unwrap();
        let ConfigurationValueV1::ContextScoutSettings(settings) = &definition.default_value else {
            panic!("Context Scout registry entry must retain its typed value");
        };
        assert_eq!(settings.state, ContextScoutConfigurationStateV1::Disabled);
        assert_eq!(
            settings.mode,
            ContextScoutConfigurationModeV1::Deterministic
        );
        assert_eq!(settings.model_path, None);
        settings.validate().unwrap();
    }
}
