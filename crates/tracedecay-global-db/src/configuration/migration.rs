//! Read-only legacy configuration migration with bounded quarantine.
//!
//! The legacy decoder supplies already-redacted typed candidates. This module
//! never writes a legacy configuration file, derives authority from a path, or
//! guesses source bindings from CWD, host configuration, or registry adjacency.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;

use thiserror::Error;
use tracedecay_domain::canonical_sha256;
use tracedecay_domain::configuration::{
    ACCESS_RULES_SETTING_KEY, AuthorityRef, ConfigurationLayerIdV1, ConfigurationRevisionId,
    ConfigurationSnapshotId, ConfigurationValueV1, SOURCE_BINDINGS_SETTING_KEY, ScopeSourceBinding,
    SettingKey, SettingScopeV1, WORK_TOPOLOGY_POLICY_SETTING_KEY,
};
use tracedecay_domain::{DomainError, ManifestDigest, UtcMicros};

use super::registry::{ConfigurationRegistry, ConfigurationRegistryError};
use super::resolver::{
    ConfigurationLayerV1, ConfigurationResolutionError, ConfigurationResolutionInputSourceV1,
    ConfigurationResolutionInputV1, ConfigurationResolutionV1, resolve_configuration_inputs,
};

pub const CONFIGURATION_CONTROL_PLANE_MIGRATION_RECEIPT_NAME: &str =
    "configuration-control-plane-v1";

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
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

    /// Legacy source precedence is explicit and low-to-high. Environment
    /// values are an input layer, never adapter-local mutation performed after
    /// resolution.
    pub const fn precedence(self) -> u8 {
        match self {
            Self::HostProfile => 0,
            Self::ConfigJson => 1,
            Self::Environment => 2,
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
    /// A decoder can preserve an exact quarantine reason without attempting to
    /// turn a path-derived or malformed legacy value into durable authority.
    #[serde(default)]
    pub quarantine_reason: Option<ConfigurationMigrationQuarantineReasonV1>,
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
        self.redacted_value_digest.validate()?;
        let decoded = self.setting_key.is_some() && self.value.is_some();
        let quarantined =
            self.setting_key.is_none() && self.value.is_none() && self.quarantine_reason.is_some();
        if decoded == quarantined {
            return Err(DomainError::NonCanonical {
                field: "legacy configuration entry state",
            });
        }
        Ok(())
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

/// Ordered low-to-high snapshots from legacy configuration sources. All
/// snapshots target one already-authorized canonical layer and revision; a
/// path or host label never supplies that authority. The only permitted source
/// order is `HostProfile < ConfigJson < Environment`.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadonlyLegacyConfigurationInputsV1 {
    pub inputs: Vec<ReadonlyLegacyConfigurationInputV1>,
}

impl ReadonlyLegacyConfigurationInputsV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        let Some(first) = self.inputs.first() else {
            return Err(DomainError::Empty {
                field: "legacy configuration inputs",
            });
        };
        first.validate()?;
        let target_layer = &first.target_layer;
        let target_revision_id = &first.target_revision_id;
        let mut previous_precedence = first.source_kind.precedence();
        for input in &self.inputs[1..] {
            input.validate()?;
            if &input.target_layer != target_layer {
                return Err(DomainError::NonCanonical {
                    field: "legacy configuration input target layer",
                });
            }
            if &input.target_revision_id != target_revision_id {
                return Err(DomainError::NonCanonical {
                    field: "legacy configuration input target revision",
                });
            }
            if input.source_kind.precedence() <= previous_precedence {
                return Err(DomainError::NonCanonical {
                    field: "legacy configuration input source order",
                });
            }
            previous_precedence = input.source_kind.precedence();
        }
        Ok(())
    }

    pub fn snapshot_digest(&self) -> Result<ManifestDigest, DomainError> {
        self.validate()?;
        canonical_sha256(&("tracedecay.configuration.legacy-inputs.v1", self))
    }
}

/// Canonical authority contributed by the daemon when it creates a project's
/// first configuration revision.
///
/// This is deliberately *not* a legacy input. Legacy `config.json`, host
/// profile, and environment entries can never supply a source binding: a
/// binding decoded from a path or host label is quarantined as
/// [`ConfigurationMigrationQuarantineReasonV1::PathDerivedAuthority`]. Genesis
/// bindings are different in kind — they are the project's own already-resolved
/// identity restated by the daemon that registered it, not authority inferred
/// from untrusted input — so they resolve at `Canonical` precedence instead.
///
/// A genesis contribution only ever appears in the initial migration. Once a
/// project has a durable revision, every later binding change goes through the
/// protected [`ProtectedChange::BindSource`] path.
///
/// [`ProtectedChange::BindSource`]: tracedecay_domain::configuration::ProtectedChange::BindSource
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalGenesisConfigurationV1 {
    pub target_layer: ConfigurationLayerIdV1,
    pub target_revision_id: ConfigurationRevisionId,
    /// Every binding must name the same authority as `target_layer`, and no two
    /// bindings may claim the same `(source_kind, authority)` key.
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

    pub fn snapshot_digest(&self) -> Result<ManifestDigest, DomainError> {
        self.validate()?;
        canonical_sha256(&("tracedecay.configuration.canonical-genesis.v1", self))
    }

    fn resolution_input(&self) -> Result<ConfigurationResolutionInputV1, DomainError> {
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

/// A genesis binding may only be issued by the layer that already owns the
/// authority it names. A project layer cannot mint a user-profile binding and
/// vice versa.
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
    migrate_legacy_configuration_inputs_with_digest(
        registry,
        std::slice::from_ref(input),
        None,
        source_snapshot_digest,
        store,
        now,
    )
    .await
}

/// Migrate explicitly ordered legacy snapshots. `Environment` entries are
/// resolved after persisted `config.json` entries in the same canonical layer,
/// preserving the legacy override rule without allowing an adapter-local
/// default to mutate a resolved snapshot.
pub async fn migrate_legacy_configuration_inputs<Store>(
    registry: &ConfigurationRegistry,
    inputs: &ReadonlyLegacyConfigurationInputsV1,
    store: &Store,
    now: UtcMicros,
) -> Result<ConfigurationMigrationOutcomeV1, ConfigurationMigrationError>
where
    Store: ConfigurationMigrationStore,
{
    inputs.validate()?;
    let source_snapshot_digest = inputs.snapshot_digest()?;
    migrate_legacy_configuration_inputs_with_digest(
        registry,
        &inputs.inputs,
        None,
        source_snapshot_digest,
        store,
        now,
    )
    .await
}

/// Migrate explicitly ordered legacy snapshots together with the canonical
/// genesis authority contributed by the daemon that registered the project.
///
/// The genesis contribution is not a legacy input and never widens what legacy
/// input may express: legacy source-binding entries stay quarantined as
/// [`ConfigurationMigrationQuarantineReasonV1::PathDerivedAuthority`]. It only
/// records, at `Canonical` precedence, the binding whose authority the caller
/// already holds, so a project's first durable revision states its own
/// identity instead of leaving it unbound.
pub async fn migrate_legacy_configuration_inputs_with_genesis<Store>(
    registry: &ConfigurationRegistry,
    inputs: &ReadonlyLegacyConfigurationInputsV1,
    genesis: &CanonicalGenesisConfigurationV1,
    store: &Store,
    now: UtcMicros,
) -> Result<ConfigurationMigrationOutcomeV1, ConfigurationMigrationError>
where
    Store: ConfigurationMigrationStore,
{
    inputs.validate()?;
    genesis.validate()?;
    // The receipt key covers the genesis contribution as well, so a project
    // whose first revision was written before genesis existed is not mistaken
    // for one that already recorded its own binding.
    let source_snapshot_digest = canonical_sha256(&(
        "tracedecay.configuration.legacy-inputs-with-genesis.v1",
        inputs.snapshot_digest()?,
        genesis.snapshot_digest()?,
    ))?;
    migrate_legacy_configuration_inputs_with_digest(
        registry,
        &inputs.inputs,
        Some(genesis),
        source_snapshot_digest,
        store,
        now,
    )
    .await
}

async fn migrate_legacy_configuration_inputs_with_digest<Store>(
    registry: &ConfigurationRegistry,
    inputs: &[ReadonlyLegacyConfigurationInputV1],
    genesis: Option<&CanonicalGenesisConfigurationV1>,
    source_snapshot_digest: ManifestDigest,
    store: &Store,
    now: UtcMicros,
) -> Result<ConfigurationMigrationOutcomeV1, ConfigurationMigrationError>
where
    Store: ConfigurationMigrationStore,
{
    if let Some(receipt) = store
        .receipt(
            CONFIGURATION_CONTROL_PLANE_MIGRATION_RECEIPT_NAME,
            &source_snapshot_digest,
        )
        .await?
    {
        return Ok(ConfigurationMigrationOutcomeV1::AlreadyApplied(receipt));
    }
    let initial_revision_id = inputs
        .first()
        .map(|input| input.target_revision_id.clone())
        .or_else(|| genesis.map(|genesis| genesis.target_revision_id.clone()))
        .ok_or(DomainError::Empty {
            field: "legacy configuration inputs",
        })?;
    if let Some(genesis) = genesis {
        // Resolution rejects competing revisions for one layer; refusing here
        // keeps the receipt's `initial_revision_id` truthful for the genesis
        // contribution too.
        if genesis.target_revision_id != initial_revision_id {
            return Err(DomainError::NonCanonical {
                field: "canonical genesis revision",
            }
            .into());
        }
    }

    let mut resolution_inputs = Vec::new();
    let mut imported_keys = BTreeSet::new();
    let mut quarantine = Vec::new();
    for input in inputs {
        input.validate()?;
        let mut entries = BTreeMap::new();
        for entry in &input.entries {
            if let Some(reason) = entry.quarantine_reason {
                quarantine.push(quarantine_entry(input, entry, reason, now));
                continue;
            }

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
            if key.as_str() == SOURCE_BINDINGS_SETTING_KEY
                || key.as_str() == ACCESS_RULES_SETTING_KEY
            {
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
            imported_keys.insert(key);
        }
        resolution_inputs.push(ConfigurationResolutionInputV1 {
            source: legacy_resolution_source(input.source_kind),
            layer: ConfigurationLayerV1 {
                layer: input.target_layer.clone(),
                revision_id: input.target_revision_id.clone(),
                entries,
            },
        });
    }

    if let Some(genesis) = genesis {
        // Appended last so the canonical binding resolves above every legacy
        // layer, whose own binding entries were already quarantined above.
        resolution_inputs.push(genesis.resolution_input()?);
        imported_keys.insert(SettingKey::new(SOURCE_BINDINGS_SETTING_KEY)?);
    }

    let resolution = resolve_configuration_inputs(registry, &resolution_inputs)?;
    let receipt = ConfigurationMigrationReceiptV1 {
        receipt_name: CONFIGURATION_CONTROL_PLANE_MIGRATION_RECEIPT_NAME,
        source_snapshot_digest,
        initial_revision_id,
        initial_snapshot_id: resolution.snapshot.snapshot_id.clone(),
        created_at: now,
    };
    store
        .commit_initial_migration(&receipt, &resolution, &quarantine)
        .await?;
    Ok(ConfigurationMigrationOutcomeV1::Applied {
        receipt,
        imported_keys: imported_keys.into_iter().collect(),
        quarantined: quarantine,
    })
}

fn legacy_resolution_source(
    source_kind: LegacyConfigurationSourceKindV1,
) -> ConfigurationResolutionInputSourceV1 {
    match source_kind {
        LegacyConfigurationSourceKindV1::ConfigJson => {
            ConfigurationResolutionInputSourceV1::LegacyConfigJson
        }
        LegacyConfigurationSourceKindV1::Environment => {
            ConfigurationResolutionInputSourceV1::LegacyEnvironment
        }
        LegacyConfigurationSourceKindV1::HostProfile => {
            ConfigurationResolutionInputSourceV1::LegacyHostProfile
        }
    }
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
        resolution: Mutex<Option<ConfigurationResolutionV1>>,
    }

    impl ConfigurationMigrationStore for Store {
        async fn receipt(
            &self,
            _receipt_name: &'static str,
            _source_snapshot_digest: &ManifestDigest,
        ) -> Result<Option<ConfigurationMigrationReceiptV1>, ConfigurationMigrationError> {
            Ok(self.receipt.lock().unwrap().clone())
        }

        async fn commit_initial_migration(
            &self,
            receipt: &ConfigurationMigrationReceiptV1,
            resolution: &ConfigurationResolutionV1,
            quarantine: &[ConfigurationMigrationQuarantineEntryV1],
        ) -> Result<(), ConfigurationMigrationError> {
            *self.receipt.lock().unwrap() = Some(receipt.clone());
            self.quarantined
                .lock()
                .unwrap()
                .extend_from_slice(quarantine);
            *self.resolution.lock().unwrap() = Some(resolution.clone());
            Ok(())
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
                quarantine_reason: None,
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

    #[tokio::test]
    async fn ordered_inputs_apply_environment_after_config_and_keep_the_digest_idempotent() {
        use tracedecay_domain::configuration::SYNC_AUTO_WATCH_SETTING_KEY;

        let target_layer = ConfigurationLayerIdV1::Project {
            project_id: id("project.fixture"),
        };
        let inputs = ReadonlyLegacyConfigurationInputsV1 {
            inputs: vec![
                ReadonlyLegacyConfigurationInputV1 {
                    source_kind: LegacyConfigurationSourceKindV1::ConfigJson,
                    target_layer: target_layer.clone(),
                    target_revision_id: id("revision.legacy"),
                    entries: vec![
                        LegacyConfigurationEntryV1 {
                            source_key_digest: digest('a'),
                            setting_key: Some(id(SYNC_AUTO_WATCH_SETTING_KEY)),
                            value: Some(ConfigurationValueV1::Boolean(true)),
                            redacted_value_digest: digest('b'),
                            quarantine_reason: None,
                        },
                        LegacyConfigurationEntryV1 {
                            source_key_digest: digest('c'),
                            setting_key: None,
                            value: None,
                            redacted_value_digest: digest('d'),
                            quarantine_reason: Some(
                                ConfigurationMigrationQuarantineReasonV1::PathDerivedAuthority,
                            ),
                        },
                    ],
                },
                ReadonlyLegacyConfigurationInputV1 {
                    source_kind: LegacyConfigurationSourceKindV1::Environment,
                    target_layer,
                    target_revision_id: id("revision.legacy"),
                    entries: vec![LegacyConfigurationEntryV1 {
                        source_key_digest: digest('e'),
                        setting_key: Some(id(SYNC_AUTO_WATCH_SETTING_KEY)),
                        value: Some(ConfigurationValueV1::Boolean(false)),
                        redacted_value_digest: digest('f'),
                        quarantine_reason: None,
                    }],
                },
            ],
        };
        let digest = inputs.snapshot_digest().unwrap();
        let store = Store::default();
        let outcome = migrate_legacy_configuration_inputs(
            &ConfigurationRegistry::core().unwrap(),
            &inputs,
            &store,
            UtcMicros(1),
        )
        .await
        .unwrap();

        let ConfigurationMigrationOutcomeV1::Applied { receipt, .. } = outcome else {
            panic!("first migration must apply")
        };
        assert_eq!(receipt.source_snapshot_digest, digest);
        let resolution = store.resolution.lock().unwrap().clone().unwrap();
        assert_eq!(
            resolution.snapshot.effective_values[&id(SYNC_AUTO_WATCH_SETTING_KEY)],
            ConfigurationValueV1::Boolean(false)
        );
        assert_eq!(
            resolution.settings[&id(SYNC_AUTO_WATCH_SETTING_KEY)]
                .candidates
                .last()
                .and_then(|candidate| candidate.safe_reason.as_deref()),
            Some("highest_valid_legacy_environment")
        );
        assert_eq!(store.quarantined.lock().unwrap().len(), 1);
        assert_eq!(
            store.quarantined.lock().unwrap()[0].reason,
            ConfigurationMigrationQuarantineReasonV1::PathDerivedAuthority
        );

        let replay = migrate_legacy_configuration_inputs(
            &ConfigurationRegistry::core().unwrap(),
            &inputs,
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
