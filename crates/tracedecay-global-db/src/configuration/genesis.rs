//! Fresh-profile configuration genesis.
//!
//! Genesis materializes registry defaults plus the exact project authority
//! already held by the daemon. It has no file, environment, host-profile, or
//! prior-store input.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;
use tracedecay_domain::DomainError;
use tracedecay_domain::configuration::{
    AuthorityRef, ConfigurationLayerIdV1, ConfigurationRevisionId, ConfigurationValueV1,
    SOURCE_BINDINGS_SETTING_KEY, ScopeSourceBinding, SettingKey,
};

use super::registry::ConfigurationRegistry;
use super::resolver::{
    ConfigurationLayerV1, ConfigurationResolutionError, ConfigurationResolutionV1,
    resolve_configuration,
};

#[derive(Debug, Error)]
pub enum ConfigurationGenesisError {
    #[error("configuration genesis contains an invalid domain value: {0}")]
    Domain(#[from] DomainError),
    #[error("configuration resolver rejected genesis: {0}")]
    Resolution(#[from] ConfigurationResolutionError),
}

/// Resolves the one configuration snapshot a fresh project may persist.
pub fn resolve_project_genesis(
    registry: &ConfigurationRegistry,
    project_layer: ConfigurationLayerIdV1,
    revision_id: ConfigurationRevisionId,
    source_bindings: Vec<ScopeSourceBinding>,
) -> Result<ConfigurationResolutionV1, ConfigurationGenesisError> {
    project_layer.validate()?;
    revision_id.validate()?;
    let ConfigurationLayerIdV1::Project { project_id } = &project_layer else {
        return Err(DomainError::NonCanonical {
            field: "configuration genesis project layer",
        }
        .into());
    };
    if source_bindings.is_empty() {
        return Err(DomainError::Empty {
            field: "configuration genesis source bindings",
        }
        .into());
    }

    let mut claimed_sources = BTreeSet::new();
    for binding in &source_bindings {
        binding.validate()?;
        if binding.authority != AuthorityRef::Project(project_id.clone()) {
            return Err(DomainError::NonCanonical {
                field: "configuration genesis source binding authority",
            }
            .into());
        }
        if !claimed_sources.insert((binding.source_kind, binding.source_locator_digest.clone())) {
            return Err(DomainError::NonCanonical {
                field: "configuration genesis source binding",
            }
            .into());
        }
    }

    let source_bindings_key = SettingKey::new(SOURCE_BINDINGS_SETTING_KEY)?;
    let layer = ConfigurationLayerV1 {
        layer: project_layer,
        revision_id,
        entries: BTreeMap::from([(
            source_bindings_key,
            ConfigurationValueV1::SourceBindings(source_bindings),
        )]),
    };
    resolve_configuration(registry, &[layer]).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::configuration::{SourceBindingId, SourceKindV1};
    use tracedecay_domain::{LocatorDigest, ProjectId};

    fn binding(project_id: &ProjectId) -> ScopeSourceBinding {
        ScopeSourceBinding::new(
            SourceBindingId::new("binding.genesis.cursor".to_owned()).unwrap(),
            SourceKindV1::Cursor,
            LocatorDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
            AuthorityRef::Project(project_id.clone()),
        )
        .unwrap()
    }

    #[test]
    fn genesis_contains_every_registered_setting_and_exact_project_binding() {
        let registry = ConfigurationRegistry::core().unwrap();
        let project_id = ProjectId::new("project.genesis".to_owned()).unwrap();
        let resolution = resolve_project_genesis(
            &registry,
            ConfigurationLayerIdV1::Project {
                project_id: project_id.clone(),
            },
            ConfigurationRevisionId::new("configuration.genesis".to_owned()).unwrap(),
            vec![binding(&project_id)],
        )
        .unwrap();

        assert_eq!(
            resolution.snapshot.effective_values.len(),
            registry.definitions().count()
        );
        assert_eq!(
            resolution
                .snapshot
                .effective_values
                .get(&SettingKey::new(SOURCE_BINDINGS_SETTING_KEY).unwrap()),
            Some(&ConfigurationValueV1::SourceBindings(vec![binding(
                &project_id
            )]))
        );
    }

    #[test]
    fn genesis_rejects_binding_for_another_project() {
        let registry = ConfigurationRegistry::core().unwrap();
        let project_id = ProjectId::new("project.genesis".to_owned()).unwrap();
        let other_project = ProjectId::new("project.other".to_owned()).unwrap();

        assert!(
            resolve_project_genesis(
                &registry,
                ConfigurationLayerIdV1::Project { project_id },
                ConfigurationRevisionId::new("configuration.genesis".to_owned()).unwrap(),
                vec![binding(&other_project)],
            )
            .is_err()
        );
    }
}
