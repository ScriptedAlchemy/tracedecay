//! Control-plane helpers for the single complete topology-policy setting.
//!
//! The sole resolver combines the typed registry default with explicit layers;
//! these helpers consume its pinned snapshot and return the complete validated
//! policy. They never inspect paths, invoke Git, manufacture capability or
//! repository evidence, or substitute a locally invented default — missing,
//! mistyped, invalid, or unsupported inputs fail closed.

use thiserror::Error;
use tracedecay_domain::DomainError;
use tracedecay_domain::configuration::{
    ConfigurationSnapshotV1, ConfigurationValueV1, SettingKey, WORK_TOPOLOGY_POLICY_SETTING_KEY,
    WorkTopologyPolicyV1, safe_work_topology_policy_v1,
};

#[derive(Debug, Error)]
pub enum TopologyConfigurationError {
    #[error("topology setting key is invalid: {0}")]
    Domain(#[from] DomainError),
    #[error("topology policy is absent from the resolved configuration snapshot")]
    MissingTopologyPolicy,
    #[error("resolved topology setting has an unexpected typed value")]
    WrongTopologyValue,
}

/// Resolve the one complete policy from a pinned configuration snapshot. No
/// adapter defaults are consulted if the setting is missing or malformed.
pub fn resolved_work_topology_policy(
    snapshot: &ConfigurationSnapshotV1,
) -> Result<&WorkTopologyPolicyV1, TopologyConfigurationError> {
    snapshot.validate()?;
    let key = SettingKey::new(WORK_TOPOLOGY_POLICY_SETTING_KEY)?;
    match snapshot.effective_values.get(&key) {
        Some(ConfigurationValueV1::WorkTopologyPolicy(policy)) => {
            policy.validate()?;
            Ok(policy)
        }
        Some(_) => Err(TopologyConfigurationError::WrongTopologyValue),
        None => Err(TopologyConfigurationError::MissingTopologyPolicy),
    }
}

/// Exposes the exact safe policy used by the typed registry before any
/// operator publishes a protected replacement.
pub fn safe_default_work_topology_policy() -> WorkTopologyPolicyV1 {
    safe_work_topology_policy_v1()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tracedecay_domain::configuration::{
        BranchTopologyKindV1, ConfigurationLayerIdV1, ConfigurationSnapshotV1,
        ConfigurationValueKindV1, ConfigurationValueV1, RestartRequirementV1, SettingKey,
        SettingScopeV1, SettingSensitivityV1, WORK_TOPOLOGY_POLICY_SETTING_KEY,
        WorkTopologyPolicyV1, safe_work_topology_policy_v1,
    };
    use tracedecay_domain::{ManifestDigest, ProjectId, UserProfileId};
    use tracedecay_global_db::configuration::registry::ConfigurationRegistry;
    use tracedecay_global_db::configuration::resolver::{
        ConfigurationLayerV1, resolve_configuration,
    };

    use super::*;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("fixture id is canonical")
    }

    fn topology_key() -> SettingKey {
        SettingKey::new(WORK_TOPOLOGY_POLICY_SETTING_KEY).unwrap()
    }

    fn project_layer(policy: WorkTopologyPolicyV1) -> ConfigurationLayerV1 {
        ConfigurationLayerV1 {
            layer: ConfigurationLayerIdV1::Project {
                project_id: id::<ProjectId>("project.fixture"),
            },
            revision_id: id("revision.project.1"),
            entries: BTreeMap::from([(
                topology_key(),
                ConfigurationValueV1::WorkTopologyPolicy(Box::new(policy)),
            )]),
        }
    }

    #[test]
    fn safe_default_never_enables_cross_merge() {
        let policy = safe_default_work_topology_policy();
        policy.validate().unwrap();
        assert!(!policy.cross_merge.allow_cross_repository);
    }

    #[test]
    fn registry_default_is_the_domain_safe_default() {
        let registry = ConfigurationRegistry::core().unwrap();
        let definition = registry.definition(&topology_key()).unwrap();
        assert_eq!(
            definition.value_kind,
            ConfigurationValueKindV1::WorkTopologyPolicy
        );
        assert_eq!(definition.sensitivity, SettingSensitivityV1::Sensitive);
        assert_eq!(definition.scope, SettingScopeV1::Project);
        assert_eq!(
            definition.restart_requirement,
            RestartRequirementV1::DaemonRestart
        );
        let ConfigurationValueV1::WorkTopologyPolicy(default) = &definition.default_value else {
            panic!("registry default must be a typed topology policy");
        };
        let safe = safe_work_topology_policy_v1();
        assert_eq!(**default, safe);
        assert_eq!(
            default.compute_digest().unwrap(),
            safe.compute_digest().unwrap()
        );
    }

    #[test]
    fn resolves_safe_default_when_no_layer_overrides() {
        let registry = ConfigurationRegistry::core().unwrap();
        let snapshot = resolve_configuration(&registry, &[]).unwrap().snapshot;
        let resolved = resolved_work_topology_policy(&snapshot).unwrap();
        let safe = safe_default_work_topology_policy();
        assert_eq!(*resolved, safe);
        assert_eq!(
            resolved.compute_digest().unwrap(),
            safe.compute_digest().unwrap()
        );
    }

    #[test]
    fn project_layer_override_wins_with_its_own_digest() {
        let registry = ConfigurationRegistry::core().unwrap();
        let mut replacement = safe_work_topology_policy_v1();
        replacement
            .branch_topology
            .allowed
            .insert(BranchTopologyKindV1::LocalStack);
        replacement.validate().unwrap();

        let resolution =
            resolve_configuration(&registry, &[project_layer(replacement.clone())]).unwrap();
        let resolved = resolved_work_topology_policy(&resolution.snapshot).unwrap();
        assert_eq!(*resolved, replacement);
        assert_ne!(*resolved, safe_work_topology_policy_v1());
        assert_eq!(
            resolved.compute_digest().unwrap(),
            replacement.compute_digest().unwrap()
        );

        // The behavior digest changes with the override even though the
        // resolution path is identical.
        let baseline = resolve_configuration(&registry, &[]).unwrap();
        let moved = resolve_configuration(&registry, &[project_layer(replacement)]).unwrap();
        assert_ne!(
            baseline.snapshot.effective_behavior_digest,
            moved.snapshot.effective_behavior_digest
        );
    }

    #[test]
    fn user_profile_layer_cannot_override_project_scoped_topology() {
        let registry = ConfigurationRegistry::core().unwrap();
        let layer = ConfigurationLayerV1 {
            layer: ConfigurationLayerIdV1::UserProfile {
                profile_id: id::<UserProfileId>("profile.fixture"),
            },
            revision_id: id("revision.profile.1"),
            entries: BTreeMap::from([(
                topology_key(),
                ConfigurationValueV1::WorkTopologyPolicy(Box::new(safe_work_topology_policy_v1())),
            )]),
        };
        assert!(resolve_configuration(&registry, &[layer]).is_err());
    }

    #[test]
    fn reserved_default_layer_injection_is_rejected() {
        let registry = ConfigurationRegistry::core().unwrap();
        let layer = ConfigurationLayerV1 {
            layer: ConfigurationLayerIdV1::Default,
            revision_id: id("revision.adapter.default"),
            entries: BTreeMap::from([(
                topology_key(),
                ConfigurationValueV1::WorkTopologyPolicy(Box::new(safe_work_topology_policy_v1())),
            )]),
        };
        assert!(resolve_configuration(&registry, &[layer]).is_err());
    }

    #[test]
    fn wrong_value_kind_fails_closed() {
        let registry = ConfigurationRegistry::core().unwrap();
        let layer = ConfigurationLayerV1 {
            layer: ConfigurationLayerIdV1::Project {
                project_id: id::<ProjectId>("project.fixture"),
            },
            revision_id: id("revision.project.1"),
            entries: BTreeMap::from([(
                topology_key(),
                ConfigurationValueV1::Text("permissive".to_owned()),
            )]),
        };
        assert!(resolve_configuration(&registry, &[layer]).is_err());
    }

    #[test]
    fn invalid_or_unsupported_policy_in_layer_fails_closed() {
        let registry = ConfigurationRegistry::core().unwrap();

        // No protected-ref rules at all.
        let mut unprotected = safe_work_topology_policy_v1();
        unprotected.protected_refs.clear();
        assert!(resolve_configuration(&registry, &[project_layer(unprotected)]).is_err());

        // Unsupported schema version.
        let mut future = safe_work_topology_policy_v1();
        future.schema_version = 2;
        assert!(resolve_configuration(&registry, &[project_layer(future)]).is_err());
    }

    #[test]
    fn snapshot_resolution_requires_the_typed_policy_value() {
        // Missing key fails closed rather than inventing a default.
        let empty = ConfigurationSnapshotV1::new(BTreeMap::new(), BTreeMap::new()).unwrap();
        assert!(matches!(
            resolved_work_topology_policy(&empty),
            Err(TopologyConfigurationError::MissingTopologyPolicy)
        ));

        // A mistyped value at the topology key fails closed.
        let registry = ConfigurationRegistry::core().unwrap();
        let default_candidate = resolve_configuration(&registry, &[])
            .unwrap()
            .settings
            .get(&topology_key())
            .unwrap()
            .candidates
            .first()
            .unwrap()
            .clone();
        let mistyped = ConfigurationSnapshotV1::new(
            BTreeMap::from([(
                topology_key(),
                ConfigurationValueV1::Text("permissive".to_owned()),
            )]),
            BTreeMap::from([(topology_key(), vec![default_candidate])]),
        )
        .unwrap();
        assert!(matches!(
            resolved_work_topology_policy(&mistyped),
            Err(TopologyConfigurationError::WrongTopologyValue)
        ));

        // A tampered snapshot identity fails closed before the value is read.
        let mut snapshot = resolve_configuration(&registry, &[]).unwrap().snapshot;
        snapshot.effective_behavior_digest =
            ManifestDigest::new(format!("sha256:{}", "0".repeat(64))).unwrap();
        assert!(matches!(
            resolved_work_topology_policy(&snapshot),
            Err(TopologyConfigurationError::Domain(_))
        ));
    }
}
