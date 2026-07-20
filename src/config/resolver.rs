//! Pure deterministic configuration resolution.
//!
//! This module contains the sole layer-precedence implementation. It has no
//! adapter defaults, file reads, database access, or authorization logic.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;
use tracedecay_domain::DomainError;
use tracedecay_domain::configuration::{
    CandidateDispositionV1, ConfigurationCandidateV1, ConfigurationLayerIdV1,
    ConfigurationLayerKindV1, ConfigurationRevisionId, ConfigurationSnapshotV1,
    ConfigurationValueV1, SettingDefinitionV1, SettingKey, SettingScopeV1,
};

use super::registry::{ConfigurationRegistry, ConfigurationRegistryError};

#[derive(Clone, Debug)]
pub struct ConfigurationLayerV1 {
    pub layer: ConfigurationLayerIdV1,
    pub revision_id: ConfigurationRevisionId,
    pub entries: BTreeMap<SettingKey, ConfigurationValueV1>,
}

impl ConfigurationLayerV1 {
    pub fn validate(
        &self,
        registry: &ConfigurationRegistry,
    ) -> Result<(), ConfigurationResolutionError> {
        self.layer.validate()?;
        self.revision_id.validate()?;
        if self.layer.kind() == ConfigurationLayerKindV1::Default {
            return Err(ConfigurationResolutionError::ReservedDefaultLayer);
        }
        for (key, value) in &self.entries {
            registry.validate_value(key, value)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct ResolvedSettingV1 {
    pub definition: SettingDefinitionV1,
    pub effective_value: ConfigurationValueV1,
    pub candidates: Vec<ConfigurationCandidateV1>,
}

#[derive(Clone, Debug)]
pub struct ConfigurationResolutionV1 {
    pub snapshot: ConfigurationSnapshotV1,
    pub settings: BTreeMap<SettingKey, ResolvedSettingV1>,
}

#[derive(Debug, Error)]
pub enum ConfigurationResolutionError {
    #[error("configuration resolution received an invalid domain value: {0}")]
    Domain(#[from] DomainError),
    #[error("configuration registry rejected a setting: {0}")]
    Registry(#[from] ConfigurationRegistryError),
    #[error("the registry default layer is internal and cannot be supplied")]
    ReservedDefaultLayer,
    #[error("duplicate configuration layer: {0:?}")]
    DuplicateLayer(ConfigurationLayerIdV1),
    #[error("setting {key} cannot be placed in {layer:?}")]
    InvalidLayerForSetting {
        key: SettingKey,
        layer: ConfigurationLayerIdV1,
    },
}

/// Resolve all registered settings against layers from low to high precedence.
/// Default definitions are always present and cannot be replaced by an
/// adapter-provided default layer.
pub fn resolve_configuration(
    registry: &ConfigurationRegistry,
    layers: &[ConfigurationLayerV1],
) -> Result<ConfigurationResolutionV1, ConfigurationResolutionError> {
    let mut seen_layers = BTreeSet::new();
    for layer in layers {
        layer.validate(registry)?;
        if !seen_layers.insert(layer.layer.clone()) {
            return Err(ConfigurationResolutionError::DuplicateLayer(
                layer.layer.clone(),
            ));
        }
    }

    let mut ordered_layers: Vec<_> = layers.iter().collect();
    ordered_layers.sort_by(|left, right| {
        left.layer
            .kind()
            .precedence()
            .cmp(&right.layer.kind().precedence())
            .then_with(|| left.layer.cmp(&right.layer))
    });

    let default_revision = registry_default_revision()?;
    let mut effective_values = BTreeMap::new();
    let mut provenance = BTreeMap::new();
    let mut settings = BTreeMap::new();

    for definition in registry.definitions() {
        let mut effective_value = definition.default_value.clone();
        let mut candidates = vec![ConfigurationCandidateV1 {
            layer: ConfigurationLayerIdV1::Default,
            revision_id: default_revision.clone(),
            disposition: CandidateDispositionV1::Defaulted,
            safe_reason: Some("registry_default".to_owned()),
        }];

        for layer in &ordered_layers {
            let Some(value) = layer.entries.get(&definition.key) else {
                continue;
            };
            if !layer_can_override(definition.scope, &layer.layer) {
                return Err(ConfigurationResolutionError::InvalidLayerForSetting {
                    key: definition.key.clone(),
                    layer: layer.layer.clone(),
                });
            }
            registry.validate_value(&definition.key, value)?;
            if let Some(previous) = candidates.last_mut() {
                previous.disposition = CandidateDispositionV1::Overridden;
                previous.safe_reason = Some("higher_precedence_layer".to_owned());
            }
            effective_value = value.clone();
            candidates.push(ConfigurationCandidateV1 {
                layer: layer.layer.clone(),
                revision_id: layer.revision_id.clone(),
                disposition: CandidateDispositionV1::Winning,
                safe_reason: Some("highest_valid_precedence".to_owned()),
            });
        }

        effective_values.insert(definition.key.clone(), effective_value.clone());
        provenance.insert(definition.key.clone(), candidates.clone());
        settings.insert(
            definition.key.clone(),
            ResolvedSettingV1 {
                definition: definition.clone(),
                effective_value,
                candidates,
            },
        );
    }

    let snapshot = ConfigurationSnapshotV1::new(effective_values, provenance)?;
    Ok(ConfigurationResolutionV1 { snapshot, settings })
}

fn layer_can_override(scope: SettingScopeV1, layer: &ConfigurationLayerIdV1) -> bool {
    matches!(
        (scope, layer.kind()),
        (
            SettingScopeV1::UserProfile,
            ConfigurationLayerKindV1::UserProfile
        ) | (SettingScopeV1::Project, ConfigurationLayerKindV1::Project)
            | (
                SettingScopeV1::Collection,
                ConfigurationLayerKindV1::Collection
            )
    )
}

fn registry_default_revision() -> Result<ConfigurationRevisionId, DomainError> {
    ConfigurationRevisionId::new("configuration.registry.default.v1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::configuration::{
        ANALYZER_SETTINGS_SETTING_KEY, AnalyzerSettingsV1, ConfigurationLayerIdV1,
        ConfigurationValueV1, UserProfileId,
    };

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    #[test]
    fn same_value_in_a_higher_layer_changes_provenance_not_behavior() {
        let registry = ConfigurationRegistry::core().unwrap();
        let baseline = resolve_configuration(&registry, &[]).unwrap();
        let layer = ConfigurationLayerV1 {
            layer: ConfigurationLayerIdV1::Project {
                project_id: id("project.fixture"),
            },
            revision_id: id("revision.project.1"),
            entries: BTreeMap::from([(
                id(ANALYZER_SETTINGS_SETTING_KEY),
                ConfigurationValueV1::AnalyzerSettings(AnalyzerSettingsV1::empty()),
            )]),
        };
        let moved = resolve_configuration(&registry, &[layer]).unwrap();

        assert_eq!(
            baseline.snapshot.effective_behavior_digest,
            moved.snapshot.effective_behavior_digest
        );
        assert_ne!(
            baseline.snapshot.resolution_provenance_digest,
            moved.snapshot.resolution_provenance_digest
        );
    }

    #[test]
    fn user_profile_settings_cannot_be_overridden_from_a_project_layer() {
        let registry = ConfigurationRegistry::core().unwrap();
        let layer = ConfigurationLayerV1 {
            layer: ConfigurationLayerIdV1::Project {
                project_id: id("project.fixture"),
            },
            revision_id: id("revision.project.1"),
            entries: BTreeMap::from([(
                id("query.default_collection.v1"),
                ConfigurationValueV1::DefaultCollection(None),
            )]),
        };
        assert!(resolve_configuration(&registry, &[layer]).is_err());

        let user_layer = ConfigurationLayerV1 {
            layer: ConfigurationLayerIdV1::UserProfile {
                profile_id: id::<UserProfileId>("profile.fixture"),
            },
            revision_id: id("revision.profile.1"),
            entries: BTreeMap::from([(
                id("query.default_collection.v1"),
                ConfigurationValueV1::DefaultCollection(None),
            )]),
        };
        assert!(resolve_configuration(&registry, &[user_layer]).is_ok());
    }
}
