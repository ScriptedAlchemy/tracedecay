use std::collections::{BTreeMap, BTreeSet};

use tracedecay_domain::DomainError;
use tracedecay_domain::configuration::{
    AuthorityRef, ConfigurationLayerIdV1, ConfigurationRevisionId, ConfigurationValueV1,
    SOURCE_BINDINGS_SETTING_KEY, ScopeSourceBinding, SettingKey,
};

use super::resolver::{
    ConfigurationLayerV1, ConfigurationResolutionInputSourceV1, ConfigurationResolutionInputV1,
};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalGenesisConfigurationV1 {
    pub target_layer: ConfigurationLayerIdV1,
    pub target_revision_id: ConfigurationRevisionId,
    pub source_bindings: Vec<ScopeSourceBinding>,
}

impl CanonicalGenesisConfigurationV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.target_layer.validate()?;
        self.target_revision_id.validate()?;
        if self.source_bindings.is_empty() {
            return Err(DomainError::Empty {
                field: "canonical genesis source bindings",
            });
        }
        let mut seen = BTreeSet::new();
        for binding in &self.source_bindings {
            binding.validate()?;
            if !layer_owns_authority(&self.target_layer, &binding.authority) {
                return Err(DomainError::NonCanonical {
                    field: "canonical genesis binding authority",
                });
            }
            if !seen.insert((binding.source_kind, binding.authority.clone())) {
                return Err(DomainError::NonCanonical {
                    field: "canonical genesis binding key",
                });
            }
        }
        Ok(())
    }

    pub(crate) fn resolution_input(&self) -> Result<ConfigurationResolutionInputV1, DomainError> {
        self.validate()?;
        let key = SettingKey::new(SOURCE_BINDINGS_SETTING_KEY)?;
        Ok(ConfigurationResolutionInputV1 {
            source: ConfigurationResolutionInputSourceV1::Canonical,
            layer: ConfigurationLayerV1 {
                layer: self.target_layer.clone(),
                revision_id: self.target_revision_id.clone(),
                entries: BTreeMap::from([(
                    key,
                    ConfigurationValueV1::SourceBindings(self.source_bindings.clone()),
                )]),
            },
        })
    }
}

fn layer_owns_authority(layer: &ConfigurationLayerIdV1, authority: &AuthorityRef) -> bool {
    match (layer, authority) {
        (
            ConfigurationLayerIdV1::Project {
                project_id: layer_project,
            },
            AuthorityRef::Project(binding_project),
        ) => layer_project == binding_project,
        (
            ConfigurationLayerIdV1::UserProfile {
                profile_id: layer_profile,
            },
            AuthorityRef::ProjectlessHermes(binding_profile),
        ) => layer_profile == binding_profile,
        _ => false,
    }
}
