//! Read-only legacy configuration migration with bounded quarantine.
//!
//! The legacy decoder supplies already-redacted typed candidates. This module
//! never writes a legacy configuration file, derives authority from a path, or
//! guesses source bindings from CWD, host configuration, or registry adjacency.

use std::collections::BTreeMap;
use std::future::Future;

use thiserror::Error;
use tracedecay_domain::canonical_sha256;
use tracedecay_domain::configuration::{
    ACCESS_RULES_SETTING_KEY, ConfigurationLayerIdV1, ConfigurationRevisionId,
    ConfigurationSnapshotId, ConfigurationValueV1, SOURCE_BINDINGS_SETTING_KEY, SettingKey,
    SettingScopeV1, WORK_TOPOLOGY_POLICY_SETTING_KEY,
};
use tracedecay_domain::{DomainError, ManifestDigest, UtcMicros};

use crate::config::registry::{ConfigurationRegistry, ConfigurationRegistryError};
use crate::config::resolver::{
    ConfigurationLayerV1, ConfigurationResolutionError, ConfigurationResolutionV1,
    resolve_configuration,
};

pub const CONFIGURATION_CONTROL_PLANE_MIGRATION_RECEIPT_NAME: &str =
    "configuration-control-plane-v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyConfigurationSourceKindV1 {
    ConfigJson,
    Environment,
    HostProfile,
}

impl LegacyConfigurationSourceKindV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ConfigJson => "config_json",
            Self::Environment => "environment",
            Self::HostProfile => "host_profile",
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyConfigurationEntryV1 {
    pub source_key_digest: ManifestDigest,
    pub setting_key: Option<SettingKey>,
    pub value: Option<ConfigurationValueV1>,
    pub redacted_value_digest: ManifestDigest,
}

impl LegacyConfigurationEntryV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.source_key_digest.validate()?;
        self.setting_key
            .as_ref()
            .map_or(Ok(()), SettingKey::validate)?;
        self.value
            .as_ref()
            .map_or(Ok(()), ConfigurationValueV1::validate)?;
        self.redacted_value_digest.validate()
    }
}

/// A read-only input snapshot. Raw paths, secrets, provider labels, and
/// mutable locators must be redacted before constructing this value.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadonlyLegacyConfigurationInputV1 {
    pub source_kind: LegacyConfigurationSourceKindV1,
    pub target_layer: ConfigurationLayerIdV1,
    pub target_revision_id: ConfigurationRevisionId,
    pub entries: Vec<LegacyConfigurationEntryV1>,
}

impl ReadonlyLegacyConfigurationInputV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.target_layer.validate()?;
        self.target_revision_id.validate()?;
        for entry in &self.entries {
            entry.validate()?;
        }
        Ok(())
    }

    pub fn snapshot_digest(&self) -> Result<ManifestDigest, DomainError> {
        self.validate()?;
        canonical_sha256(&("tracedecay.configuration.legacy-input.v1", self))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationMigrationQuarantineReasonV1 {
    UnknownKey,
    DeprecatedInvalid,
    Undecodable,
    PathDerivedAuthority,
    AmbiguousBinding,
    UnauthorizedBinding,
    InvalidLayer,
    InvalidTopologyFloor,
    ProtectedLegacyValue,
    DuplicateKey,
}

impl ConfigurationMigrationQuarantineReasonV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnknownKey => "unknown_key",
            Self::DeprecatedInvalid => "deprecated_invalid",
            Self::Undecodable => "undecodable",
            Self::PathDerivedAuthority => "path_derived_authority",
            Self::AmbiguousBinding => "ambiguous_binding",
            Self::UnauthorizedBinding => "unauthorized_binding",
            Self::InvalidLayer => "invalid_layer",
            Self::InvalidTopologyFloor => "invalid_topology_floor",
            Self::ProtectedLegacyValue => "protected_legacy_value",
            Self::DuplicateKey => "duplicate_key",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigurationMigrationQuarantineEntryV1 {
    pub source_kind: LegacyConfigurationSourceKindV1,
    pub source_key_digest: ManifestDigest,
    pub reason: ConfigurationMigrationQuarantineReasonV1,
    pub redacted_value_digest: ManifestDigest,
    pub quarantined_at: UtcMicros,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigurationMigrationReceiptV1 {
    pub receipt_name: &'static str,
    pub source_snapshot_digest: ManifestDigest,
    pub initial_revision_id: ConfigurationRevisionId,
    pub initial_snapshot_id: ConfigurationSnapshotId,
    pub created_at: UtcMicros,
}

/// The concrete adapter must make this commit atomic: initial revision,
/// quarantine rows, and receipt either all appear or none do.
pub trait ConfigurationMigrationStore {
    fn receipt(
        &self,
        receipt_name: &'static str,
        source_snapshot_digest: &ManifestDigest,
    ) -> impl Future<
        Output = Result<Option<ConfigurationMigrationReceiptV1>, ConfigurationMigrationError>,
    > + Send;

    fn commit_initial_migration(
        &self,
        receipt: &ConfigurationMigrationReceiptV1,
        resolution: &ConfigurationResolutionV1,
        quarantine: &[ConfigurationMigrationQuarantineEntryV1],
    ) -> impl Future<Output = Result<(), ConfigurationMigrationError>> + Send;
}

#[derive(Clone, Debug)]
pub enum ConfigurationMigrationOutcomeV1 {
    AlreadyApplied(ConfigurationMigrationReceiptV1),
    Applied {
        receipt: ConfigurationMigrationReceiptV1,
        imported_keys: Vec<SettingKey>,
        quarantined: Vec<ConfigurationMigrationQuarantineEntryV1>,
    },
}

#[derive(Debug, Error)]
pub enum ConfigurationMigrationError {
    #[error("legacy configuration input is invalid: {0}")]
    Domain(#[from] DomainError),
    #[error("configuration registry rejected legacy input: {0}")]
    Registry(#[from] ConfigurationRegistryError),
    #[error("configuration resolver rejected legacy input: {0}")]
    Resolution(#[from] ConfigurationResolutionError),
    #[error("configuration migration store failed: {0}")]
    Store(String),
}

/// Migrate only typed values that retain their existing authority semantics.
/// Source bindings and access rules are quarantined rather than inferred; their
/// registry defaults remain empty. A topology import must be complete and meet
/// the protected-ref/history-rewrite floor, otherwise the safe default remains.
pub async fn migrate_legacy_configuration<Store>(
    registry: &ConfigurationRegistry,
    input: &ReadonlyLegacyConfigurationInputV1,
    store: &Store,
    now: UtcMicros,
) -> Result<ConfigurationMigrationOutcomeV1, ConfigurationMigrationError>
where
    Store: ConfigurationMigrationStore,
{
    input.validate()?;
    let source_snapshot_digest = input.snapshot_digest()?;
    if let Some(receipt) = store
        .receipt(
            CONFIGURATION_CONTROL_PLANE_MIGRATION_RECEIPT_NAME,
            &source_snapshot_digest,
        )
        .await?
    {
        return Ok(ConfigurationMigrationOutcomeV1::AlreadyApplied(receipt));
    }

    let mut entries = BTreeMap::new();
    let mut imported_keys = Vec::new();
    let mut quarantine = Vec::new();
    for entry in &input.entries {
        let Some(key) = entry.setting_key.clone() else {
            quarantine.push(quarantine_entry(
                input,
                entry,
                ConfigurationMigrationQuarantineReasonV1::Undecodable,
                now,
            ));
            continue;
        };
        let Some(value) = entry.value.clone() else {
            quarantine.push(quarantine_entry(
                input,
                entry,
                ConfigurationMigrationQuarantineReasonV1::Undecodable,
                now,
            ));
            continue;
        };

        let definition = match registry.definition(&key) {
            Ok(definition) => definition,
            Err(ConfigurationRegistryError::UnknownSetting(_)) => {
                quarantine.push(quarantine_entry(
                    input,
                    entry,
                    ConfigurationMigrationQuarantineReasonV1::UnknownKey,
                    now,
                ));
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if !layer_can_override(definition.scope, &input.target_layer) {
            quarantine.push(quarantine_entry(
                input,
                entry,
                ConfigurationMigrationQuarantineReasonV1::InvalidLayer,
                now,
            ));
            continue;
        }
        if key.as_str() == SOURCE_BINDINGS_SETTING_KEY || key.as_str() == ACCESS_RULES_SETTING_KEY {
            quarantine.push(quarantine_entry(
                input,
                entry,
                ConfigurationMigrationQuarantineReasonV1::PathDerivedAuthority,
                now,
            ));
            continue;
        }
        if key.as_str() == WORK_TOPOLOGY_POLICY_SETTING_KEY {
            let ConfigurationValueV1::WorkTopologyPolicy(policy) = &value else {
                quarantine.push(quarantine_entry(
                    input,
                    entry,
                    ConfigurationMigrationQuarantineReasonV1::Undecodable,
                    now,
                ));
                continue;
            };
            if policy.validate().is_err() || !policy.meets_protected_ref_floor() {
                quarantine.push(quarantine_entry(
                    input,
                    entry,
                    ConfigurationMigrationQuarantineReasonV1::InvalidTopologyFloor,
                    now,
                ));
                continue;
            }
        }
        if registry.validate_value(&key, &value).is_err() {
            quarantine.push(quarantine_entry(
                input,
                entry,
                ConfigurationMigrationQuarantineReasonV1::DeprecatedInvalid,
                now,
            ));
            continue;
        }
        if entries.contains_key(&key) {
            quarantine.push(quarantine_entry(
                input,
                entry,
                ConfigurationMigrationQuarantineReasonV1::DuplicateKey,
                now,
            ));
            continue;
        }
        entries.insert(key.clone(), value);
        imported_keys.push(key);
    }

    let resolution = resolve_configuration(
        registry,
        &[ConfigurationLayerV1 {
            layer: input.target_layer.clone(),
            revision_id: input.target_revision_id.clone(),
            entries,
        }],
    )?;
    let receipt = ConfigurationMigrationReceiptV1 {
        receipt_name: CONFIGURATION_CONTROL_PLANE_MIGRATION_RECEIPT_NAME,
        source_snapshot_digest,
        initial_revision_id: input.target_revision_id.clone(),
        initial_snapshot_id: resolution.snapshot.snapshot_id.clone(),
        created_at: now,
    };
    store
        .commit_initial_migration(&receipt, &resolution, &quarantine)
        .await?;
    Ok(ConfigurationMigrationOutcomeV1::Applied {
        receipt,
        imported_keys,
        quarantined: quarantine,
    })
}

fn layer_can_override(scope: SettingScopeV1, layer: &ConfigurationLayerIdV1) -> bool {
    use tracedecay_domain::configuration::ConfigurationLayerKindV1;

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

fn quarantine_entry(
    input: &ReadonlyLegacyConfigurationInputV1,
    entry: &LegacyConfigurationEntryV1,
    reason: ConfigurationMigrationQuarantineReasonV1,
    now: UtcMicros,
) -> ConfigurationMigrationQuarantineEntryV1 {
    ConfigurationMigrationQuarantineEntryV1 {
        source_kind: input.source_kind,
        source_key_digest: entry.source_key_digest.clone(),
        reason,
        redacted_value_digest: entry.redacted_value_digest.clone(),
        quarantined_at: now,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use tracedecay_domain::configuration::{
        AuthorityRef, ConfigurationLayerIdV1, ConfigurationValueV1, ScopeSourceBinding,
        SourceBindingId, SourceKindV1,
    };
    use tracedecay_domain::{LocatorDigest, ProjectId};

    #[derive(Default)]
    struct Store {
        receipt: Mutex<Option<ConfigurationMigrationReceiptV1>>,
        quarantined: Mutex<Vec<ConfigurationMigrationQuarantineEntryV1>>,
    }

    impl ConfigurationMigrationStore for Store {
        fn receipt(
            &self,
            _receipt_name: &'static str,
            _source_snapshot_digest: &ManifestDigest,
        ) -> impl Future<
            Output = Result<Option<ConfigurationMigrationReceiptV1>, ConfigurationMigrationError>,
        > + Send {
            async move { Ok(self.receipt.lock().unwrap().clone()) }
        }

        fn commit_initial_migration(
            &self,
            receipt: &ConfigurationMigrationReceiptV1,
            _resolution: &ConfigurationResolutionV1,
            quarantine: &[ConfigurationMigrationQuarantineEntryV1],
        ) -> impl Future<Output = Result<(), ConfigurationMigrationError>> + Send {
            async move {
                *self.receipt.lock().unwrap() = Some(receipt.clone());
                self.quarantined
                    .lock()
                    .unwrap()
                    .extend_from_slice(quarantine);
                Ok(())
            }
        }
    }

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    #[tokio::test]
    async fn legacy_source_bindings_are_quarantined_instead_of_becoming_authority() {
        let input = ReadonlyLegacyConfigurationInputV1 {
            source_kind: LegacyConfigurationSourceKindV1::ConfigJson,
            target_layer: ConfigurationLayerIdV1::Project {
                project_id: id("project.fixture"),
            },
            target_revision_id: id("revision.legacy"),
            entries: vec![LegacyConfigurationEntryV1 {
                source_key_digest: digest('a'),
                setting_key: Some(id(SOURCE_BINDINGS_SETTING_KEY)),
                value: Some(ConfigurationValueV1::SourceBindings(vec![
                    ScopeSourceBinding::new(
                        id::<SourceBindingId>("binding.legacy"),
                        SourceKindV1::Cursor,
                        LocatorDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
                        AuthorityRef::Project(id::<ProjectId>("project.fixture")),
                    )
                    .unwrap(),
                ])),
                redacted_value_digest: digest('c'),
            }],
        };
        let store = Store::default();
        let outcome = migrate_legacy_configuration(
            &ConfigurationRegistry::core().unwrap(),
            &input,
            &store,
            UtcMicros(1),
        )
        .await
        .unwrap();

        assert!(matches!(
            outcome,
            ConfigurationMigrationOutcomeV1::Applied { .. }
        ));
        assert_eq!(store.quarantined.lock().unwrap().len(), 1);
        assert_eq!(
            store.quarantined.lock().unwrap()[0].reason,
            ConfigurationMigrationQuarantineReasonV1::PathDerivedAuthority
        );

        let replay = migrate_legacy_configuration(
            &ConfigurationRegistry::core().unwrap(),
            &input,
            &store,
            UtcMicros(2),
        )
        .await
        .unwrap();
        assert!(matches!(
            replay,
            ConfigurationMigrationOutcomeV1::AlreadyApplied(_)
        ));
        assert_eq!(store.quarantined.lock().unwrap().len(), 1);
    }
}
