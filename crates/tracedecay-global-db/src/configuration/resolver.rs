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

/// The evidence source supplying a resolution input. Source precedence is
/// explicit so migration adapters cannot hide environment behavior in local
/// defaulting code. Within one canonical layer, legacy host profile values are
/// applied first, followed by `config.json`, then parseable environment
/// overrides; canonical control-plane values remain highest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConfigurationResolutionInputSourceV1 {
    LegacyHostProfile,
    LegacyConfigJson,
    LegacyEnvironment,
    Canonical,
}

impl ConfigurationResolutionInputSourceV1 {
    const fn precedence(self) -> u8 {
        match self {
            Self::LegacyHostProfile => 0,
            Self::LegacyConfigJson => 1,
            Self::LegacyEnvironment => 2,
            Self::Canonical => 3,
        }
    }

    const fn override_reason(self) -> &'static str {
        match self {
            Self::LegacyHostProfile => "higher_precedence_legacy_host_profile",
            Self::LegacyConfigJson => "higher_precedence_legacy_config_json",
            Self::LegacyEnvironment => "higher_precedence_legacy_environment",
            Self::Canonical => "higher_precedence_layer",
        }
    }

    const fn winning_reason(self) -> &'static str {
        match self {
            Self::LegacyHostProfile => "highest_valid_legacy_host_profile",
            Self::LegacyConfigJson => "highest_valid_legacy_config_json",
            Self::LegacyEnvironment => "highest_valid_legacy_environment",
            Self::Canonical => "highest_valid_precedence",
        }
    }
}

/// One explicitly sourced input to deterministic configuration resolution.
/// A source is not itself durable authority: its values still require a
/// canonical layer and revision supplied by the caller.
#[derive(Clone, Debug)]
pub struct ConfigurationResolutionInputV1 {
    pub source: ConfigurationResolutionInputSourceV1,
    pub layer: ConfigurationLayerV1,
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
    #[error("configuration resolution contains competing {0:?} layers")]
    CompetingLayerKind(ConfigurationLayerKindV1),
    #[error("duplicate configuration resolution input: {layer:?} from {input_source:?}")]
    DuplicateResolutionInput {
        layer: ConfigurationLayerIdV1,
        input_source: ConfigurationResolutionInputSourceV1,
    },
    #[error("configuration resolution input changes the revision for layer {0:?}")]
    CompetingLayerRevision(ConfigurationLayerIdV1),
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
    let mut seen_layer_kinds = BTreeSet::new();
    for layer in layers {
        layer.validate(registry)?;
        if !seen_layers.insert(layer.layer.clone()) {
            return Err(ConfigurationResolutionError::DuplicateLayer(
                layer.layer.clone(),
            ));
        }
        if !seen_layer_kinds.insert(layer.layer.kind()) {
            return Err(ConfigurationResolutionError::CompetingLayerKind(
                layer.layer.kind(),
            ));
        }
    }

    let inputs: Vec<_> = layers
        .iter()
        .cloned()
        .map(|layer| ConfigurationResolutionInputV1 {
            source: ConfigurationResolutionInputSourceV1::Canonical,
            layer,
        })
        .collect();
    resolve_configuration_inputs(registry, &inputs)
}

/// Resolve explicitly sourced inputs. The source order is part of the pure
/// input contract: for a shared layer, `LegacyEnvironment` wins over
/// `LegacyConfigJson`, rather than a legacy adapter applying hidden mutation
/// after resolution. Canonical layer precedence remains primary across
/// distinct layer identities.
pub fn resolve_configuration_inputs(
    registry: &ConfigurationRegistry,
    inputs: &[ConfigurationResolutionInputV1],
) -> Result<ConfigurationResolutionV1, ConfigurationResolutionError> {
    let mut seen_inputs = BTreeSet::new();
    let mut layers_by_kind = BTreeMap::new();
    let mut revisions_by_layer = BTreeMap::new();
    for input in inputs {
        input.layer.validate(registry)?;
        if !seen_inputs.insert((input.layer.layer.clone(), input.source)) {
            return Err(ConfigurationResolutionError::DuplicateResolutionInput {
                layer: input.layer.layer.clone(),
                input_source: input.source,
            });
        }
        match layers_by_kind.get(&input.layer.layer.kind()) {
            Some(existing) if existing != &input.layer.layer => {
                return Err(ConfigurationResolutionError::CompetingLayerKind(
                    input.layer.layer.kind(),
                ));
            }
            Some(_) => {}
            None => {
                layers_by_kind.insert(input.layer.layer.kind(), input.layer.layer.clone());
            }
        }
        match revisions_by_layer.get(&input.layer.layer) {
            Some(existing) if existing != &input.layer.revision_id => {
                return Err(ConfigurationResolutionError::CompetingLayerRevision(
                    input.layer.layer.clone(),
                ));
            }
            Some(_) => {}
            None => {
                revisions_by_layer
                    .insert(input.layer.layer.clone(), input.layer.revision_id.clone());
            }
        }
    }

    let mut ordered_inputs: Vec<_> = inputs.iter().collect();
    ordered_inputs.sort_by(|left, right| {
        left.layer
            .layer
            .kind()
            .precedence()
            .cmp(&right.layer.layer.kind().precedence())
            .then_with(|| left.layer.layer.cmp(&right.layer.layer))
            .then_with(|| left.source.precedence().cmp(&right.source.precedence()))
    });

    let default_candidate = registry_default_candidate()?;
    let mut effective_values = BTreeMap::new();
    let mut provenance = BTreeMap::new();
    let mut settings = BTreeMap::new();

    for definition in registry.definitions() {
        let mut effective_value = definition.default_value.clone();
        let mut candidates = vec![default_candidate.clone()];

        for input in &ordered_inputs {
            let layer = &input.layer;
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
                previous.safe_reason = Some(input.source.override_reason().to_owned());
            }
            effective_value = value.clone();
            candidates.push(ConfigurationCandidateV1 {
                layer: layer.layer.clone(),
                revision_id: layer.revision_id.clone(),
                disposition: CandidateDispositionV1::Winning,
                safe_reason: Some(input.source.winning_reason().to_owned()),
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

pub(crate) fn registry_default_candidate() -> Result<ConfigurationCandidateV1, DomainError> {
    Ok(ConfigurationCandidateV1 {
        layer: ConfigurationLayerIdV1::Default,
        revision_id: ConfigurationRevisionId::new("configuration.registry.default.v1")?,
        disposition: CandidateDispositionV1::Defaulted,
        safe_reason: Some("registry_default".to_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::configuration::{
        ANALYZER_SETTINGS_SETTING_KEY, AnalyzerSettingsV1, ConfigurationLayerIdV1,
        ConfigurationValueV1, SYNC_AUTO_WATCH_SETTING_KEY, UserProfileId,
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

    #[test]
    fn competing_project_layers_are_rejected_instead_of_sorted_into_authority() {
        let registry = ConfigurationRegistry::core().unwrap();
        let layers = [
            ConfigurationLayerV1 {
                layer: ConfigurationLayerIdV1::Project {
                    project_id: id("project.alpha"),
                },
                revision_id: id("revision.alpha"),
                entries: BTreeMap::new(),
            },
            ConfigurationLayerV1 {
                layer: ConfigurationLayerIdV1::Project {
                    project_id: id("project.beta"),
                },
                revision_id: id("revision.beta"),
                entries: BTreeMap::new(),
            },
        ];

        assert!(matches!(
            resolve_configuration(&registry, &layers),
            Err(ConfigurationResolutionError::CompetingLayerKind(
                ConfigurationLayerKindV1::Project
            ))
        ));
    }

    #[test]
    fn explicit_legacy_environment_input_overrides_config_json_within_one_layer() {
        let registry = ConfigurationRegistry::core().unwrap();
        let layer = ConfigurationLayerIdV1::Project {
            project_id: id("project.fixture"),
        };
        let config_json = ConfigurationResolutionInputV1 {
            source: ConfigurationResolutionInputSourceV1::LegacyConfigJson,
            layer: ConfigurationLayerV1 {
                layer: layer.clone(),
                revision_id: id("revision.legacy"),
                entries: BTreeMap::from([(
                    id(SYNC_AUTO_WATCH_SETTING_KEY),
                    ConfigurationValueV1::Boolean(true),
                )]),
            },
        };
        let environment = ConfigurationResolutionInputV1 {
            source: ConfigurationResolutionInputSourceV1::LegacyEnvironment,
            layer: ConfigurationLayerV1 {
                layer,
                revision_id: id("revision.legacy"),
                entries: BTreeMap::from([(
                    id(SYNC_AUTO_WATCH_SETTING_KEY),
                    ConfigurationValueV1::Boolean(false),
                )]),
            },
        };

        let resolution = resolve_configuration_inputs(&registry, &[environment, config_json])
            .expect("source precedence is explicit, not caller-order dependent");
        let setting = resolution
            .settings
            .get(&id(SYNC_AUTO_WATCH_SETTING_KEY))
            .unwrap();
        assert_eq!(
            setting.effective_value,
            ConfigurationValueV1::Boolean(false)
        );
        assert_eq!(setting.candidates.len(), 3);
        assert_eq!(
            setting.candidates[1].safe_reason.as_deref(),
            Some("higher_precedence_legacy_environment")
        );
        assert_eq!(
            setting
                .candidates
                .last()
                .and_then(|candidate| candidate.safe_reason.as_deref()),
            Some("highest_valid_legacy_environment")
        );
    }

    #[test]
    fn one_layer_cannot_claim_multiple_revisions_across_input_sources() {
        let registry = ConfigurationRegistry::core().unwrap();
        let layer = ConfigurationLayerIdV1::Project {
            project_id: id("project.fixture"),
        };
        let inputs = [
            ConfigurationResolutionInputV1 {
                source: ConfigurationResolutionInputSourceV1::LegacyConfigJson,
                layer: ConfigurationLayerV1 {
                    layer: layer.clone(),
                    revision_id: id("revision.config-json"),
                    entries: BTreeMap::new(),
                },
            },
            ConfigurationResolutionInputV1 {
                source: ConfigurationResolutionInputSourceV1::LegacyEnvironment,
                layer: ConfigurationLayerV1 {
                    layer: layer.clone(),
                    revision_id: id("revision.environment"),
                    entries: BTreeMap::new(),
                },
            },
        ];

        assert!(matches!(
            resolve_configuration_inputs(&registry, &inputs),
            Err(ConfigurationResolutionError::CompetingLayerRevision(
                rejected
            )) if rejected == layer
        ));
    }
}
