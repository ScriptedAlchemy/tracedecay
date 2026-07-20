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
    ConfigurationValueKindV1, ConfigurationValueV1, DEFAULT_COLLECTION_SETTING_KEY,
    DeprecationStateV1, RestartRequirementV1, SOURCE_BINDINGS_SETTING_KEY, SettingDefinitionV1,
    SettingKey, SettingScopeV1, SettingSensitivityV1, WORK_TOPOLOGY_POLICY_SETTING_KEY,
    safe_work_topology_policy_v1,
};

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
}

/// Immutable mapping of every supported setting to its typed definition.
#[derive(Clone, Debug)]
pub struct ConfigurationRegistry {
    definitions: BTreeMap<SettingKey, SettingDefinitionV1>,
}

impl ConfigurationRegistry {
    /// Build the PR11 core registry. The five definitions cover source
    /// authority, restrictive access policy, the optional convenience
    /// collection selector, analyzer settings, and the safe topology policy.
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
}

fn setting_key(value: &str) -> Result<SettingKey, ConfigurationRegistryError> {
    Ok(SettingKey::new(value)?)
}
