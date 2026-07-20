//! Pure configuration-control-plane contracts.
//!
//! These values define typed settings, deterministic resolution inputs,
//! protected-change plans, and opaque credential references. They deliberately
//! contain no file paths, secret values, database handles, authorization
//! decisions, or transport-specific payloads.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::research::{
    AccessPolicyDigest, ActorId, CapabilityId, DomainError, LocatorDigest, ManifestDigest,
    ProjectId, UtcMicros, canonical_sha256,
};

pub mod topology;

pub use topology::*;

const CONFIGURATION_SNAPSHOT_ID_DOMAIN: &str = "tracedecay.configuration.snapshot.v1";
const PROTECTED_CHANGE_DIGEST_DOMAIN: &str = "tracedecay.configuration.protected-change.v1";

/// Canonical setting keys owned by the PR11 control plane.
pub const SOURCE_BINDINGS_SETTING_KEY: &str = "scope.source_bindings.v1";
pub const ACCESS_RULES_SETTING_KEY: &str = "scope.access_rules.v1";
pub const DEFAULT_COLLECTION_SETTING_KEY: &str = "query.default_collection.v1";
pub const ANALYZER_SETTINGS_SETTING_KEY: &str = "analyzer.settings.v1";
pub const WORK_TOPOLOGY_POLICY_SETTING_KEY: &str = "work.topology_policy.v1";

macro_rules! configuration_string_id {
    ($($name:ident => $field:literal),+ $(,)?) => {$(
        #[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
                let value = value.into();
                validate_canonical_label(&value, $field)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn validate(&self) -> Result<(), DomainError> {
                validate_canonical_label(&self.0, $field)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
            }
        }

        impl TryFrom<String> for $name {
            type Error = DomainError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    )+};
}

configuration_string_id!(
    UserProfileId => "user profile id",
    SourceBindingId => "source binding id",
    AccessRuleId => "access rule id",
    QueryCollectionId => "query collection id",
    WorkspaceCollectionId => "workspace collection id",
    ConfigurationRevisionId => "configuration revision id",
    ConfigurationSnapshotId => "configuration snapshot id",
    ChangePlanId => "configuration change plan id",
    ConfigurationReceiptId => "configuration receipt id",
    ConfigurationAuditEventId => "configuration audit event id",
    ConfigurationIdempotencyKey => "configuration idempotency key",
    CredentialReferenceId => "credential reference id",
    AnalyzerExecutableId => "analyzer executable id",
    AnalyzerLanguageId => "analyzer language id",
    AnalyzerEnvironmentVariable => "analyzer environment variable",
);

fn validate_canonical_label(value: &str, field: &'static str) -> Result<(), DomainError> {
    if value.is_empty() {
        return Err(DomainError::Empty { field });
    }
    if value.trim() != value || value.len() > 512 || value.chars().any(char::is_control) {
        return Err(DomainError::NonCanonical { field });
    }
    Ok(())
}

fn validate_setting_key(value: &str) -> Result<(), DomainError> {
    validate_canonical_label(value, "configuration setting key")?;
    if !value.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
    }) || !value.contains('.')
    {
        return Err(DomainError::NonCanonical {
            field: "configuration setting key",
        });
    }
    Ok(())
}

/// Typed configuration key. Keys are lowercase, dotted product identifiers;
/// untyped host/adapter keys are not representable.
#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct SettingKey(String);

impl SettingKey {
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        validate_setting_key(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        validate_setting_key(&self.0)
    }
}

impl<'de> Deserialize<'de> for SettingKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<String> for SettingKey {
    type Error = DomainError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl fmt::Display for SettingKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Explicit configuration layer precedence. The resolver is the only place
/// that applies this order; adapters must not add local defaults.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationLayerKindV1 {
    Default,
    UserProfile,
    Project,
    Collection,
}

impl ConfigurationLayerKindV1 {
    pub const fn precedence(self) -> u8 {
        match self {
            Self::Default => 0,
            Self::UserProfile => 1,
            Self::Project => 2,
            Self::Collection => 3,
        }
    }
}

/// A typed configuration layer identity. The default layer intentionally has
/// no caller-controlled identifier.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConfigurationLayerIdV1 {
    Default,
    UserProfile { profile_id: UserProfileId },
    Project { project_id: ProjectId },
    Collection { collection_id: QueryCollectionId },
}

impl ConfigurationLayerIdV1 {
    pub const fn kind(&self) -> ConfigurationLayerKindV1 {
        match self {
            Self::Default => ConfigurationLayerKindV1::Default,
            Self::UserProfile { .. } => ConfigurationLayerKindV1::UserProfile,
            Self::Project { .. } => ConfigurationLayerKindV1::Project,
            Self::Collection { .. } => ConfigurationLayerKindV1::Collection,
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Default => Ok(()),
            Self::UserProfile { profile_id } => profile_id.validate(),
            Self::Project { project_id } => project_id.validate(),
            Self::Collection { collection_id } => collection_id.validate(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SettingSensitivityV1 {
    Public,
    Sensitive,
    CredentialReference,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SettingScopeV1 {
    UserProfile,
    Project,
    Collection,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RestartRequirementV1 {
    None,
    AnalyzerRestart,
    DaemonRestart,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum DeprecationStateV1 {
    Active,
    Deprecated { replacement: Option<SettingKey> },
}

impl DeprecationStateV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Active => Ok(()),
            Self::Deprecated { replacement } => {
                replacement.as_ref().map_or(Ok(()), SettingKey::validate)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationValueKindV1 {
    Boolean,
    Unsigned,
    Text,
    StringList,
    SourceBindings,
    AccessRules,
    DefaultCollection,
    AnalyzerSettings,
    WorkTopologyPolicy,
    CredentialReference,
}

/// A reference-only selector. It is convenience input, never collection
/// authority or membership evidence.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum CollectionSelectorV1 {
    Query(QueryCollectionId),
    Workspace(WorkspaceCollectionId),
}

impl CollectionSelectorV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Query(id) => id.validate(),
            Self::Workspace(id) => id.validate(),
        }
    }
}

/// A structured analyzer option value. This deliberately excludes raw
/// environment values, commands, credential material, and transport blobs.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum AnalyzerStructuredValueV1 {
    Boolean(bool),
    Integer(i64),
    Text(String),
    TextList(Vec<String>),
    Object(BTreeMap<String, AnalyzerStructuredValueV1>),
}

impl AnalyzerStructuredValueV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Boolean(_) | Self::Integer(_) => Ok(()),
            Self::Text(value) => validate_canonical_label(value, "analyzer setting text"),
            Self::TextList(values) => {
                for value in values {
                    validate_canonical_label(value, "analyzer setting text")?;
                }
                Ok(())
            }
            Self::Object(values) => {
                for (key, value) in values {
                    validate_canonical_label(key, "analyzer setting key")?;
                    value.validate()?;
                }
                Ok(())
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum AnalyzerExecutableReferenceV1 {
    BuiltIn { executable_id: AnalyzerExecutableId },
    ApprovedExternal { executable_digest: ManifestDigest },
}

impl AnalyzerExecutableReferenceV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::BuiltIn { executable_id } => executable_id.validate(),
            Self::ApprovedExternal { executable_digest } => executable_digest.validate(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AnalyzerPrivacyClassV1 {
    NonSensitive,
    Sensitive,
    Restricted,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AnalyzerResourceLimitsV1 {
    pub maximum_memory_mib: u32,
    pub startup_timeout_millis: u64,
    pub request_timeout_millis: u64,
}

impl AnalyzerResourceLimitsV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.maximum_memory_mib == 0
            || self.startup_timeout_millis == 0
            || self.request_timeout_millis == 0
        {
            return Err(DomainError::NonCanonical {
                field: "analyzer resource limits",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AnalyzerRestartPolicyV1 {
    RestartOnConfigurationChange,
    ManualRestartOnly,
}

/// One language's analyzer selection. Host registration may project only the
/// non-sensitive `language_id`/`enabled` pair; all other fields remain in the
/// configuration authority.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AnalyzerLanguageSelectionV1 {
    pub language_id: AnalyzerLanguageId,
    pub enabled: bool,
    pub executable: AnalyzerExecutableReferenceV1,
    pub arguments: Vec<String>,
    pub initialization_options: BTreeMap<String, AnalyzerStructuredValueV1>,
    pub settings: BTreeMap<String, AnalyzerStructuredValueV1>,
    pub environment_allowlist: BTreeSet<AnalyzerEnvironmentVariable>,
    pub privacy_class: AnalyzerPrivacyClassV1,
    pub resource_limits: AnalyzerResourceLimitsV1,
    pub restart_policy: AnalyzerRestartPolicyV1,
}

impl AnalyzerLanguageSelectionV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.language_id.validate()?;
        self.executable.validate()?;
        for argument in &self.arguments {
            validate_canonical_label(argument, "analyzer argument")?;
        }
        for (key, value) in self
            .initialization_options
            .iter()
            .chain(self.settings.iter())
        {
            validate_canonical_label(key, "analyzer setting key")?;
            value.validate()?;
        }
        for variable in &self.environment_allowlist {
            variable.validate()?;
            if !variable
                .as_str()
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
            {
                return Err(DomainError::NonCanonical {
                    field: "analyzer environment variable",
                });
            }
        }
        self.resource_limits.validate()
    }
}

/// Canonical analyzer settings. A changed selection produces a new
/// configuration revision/digest; cache invalidation remains owned elsewhere.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AnalyzerSettingsV1 {
    pub schema_version: u16,
    pub selections: Vec<AnalyzerLanguageSelectionV1>,
}

impl AnalyzerSettingsV1 {
    pub const SCHEMA_VERSION: u16 = 1;

    pub fn empty() -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            selections: Vec::new(),
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(DomainError::NonCanonical {
                field: "analyzer settings schema version",
            });
        }
        for selection in &self.selections {
            selection.validate()?;
        }
        if self
            .selections
            .windows(2)
            .any(|pair| pair[0].language_id >= pair[1].language_id)
        {
            return Err(DomainError::NonCanonical {
                field: "analyzer language selection order",
            });
        }
        Ok(())
    }

    pub fn compute_digest(&self) -> Result<ManifestDigest, DomainError> {
        self.validate()?;
        canonical_sha256(&("tracedecay.analyzer-settings.v1", self))
    }
}

/// Credential metadata contains only a reference and an integrity digest. No
/// constructor, field, serializer, audit record, or error type accepts a
/// plaintext credential.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKindV1 {
    ApiToken,
    AccessToken,
    SigningKeyReference,
    Other,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CredentialReferenceMetadataV1 {
    pub reference_id: CredentialReferenceId,
    pub kind: CredentialKindV1,
    pub reference_digest: ManifestDigest,
    pub created_at: UtcMicros,
    pub rotation: u64,
}

impl CredentialReferenceMetadataV1 {
    pub fn new(
        reference_id: CredentialReferenceId,
        kind: CredentialKindV1,
        reference_digest: ManifestDigest,
        created_at: UtcMicros,
        rotation: u64,
    ) -> Result<Self, DomainError> {
        let metadata = Self {
            reference_id,
            kind,
            reference_digest,
            created_at,
            rotation,
        };
        metadata.validate()?;
        Ok(metadata)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.reference_id.validate()?;
        self.reference_digest.validate()
    }
}

/// Values that the typed registry can accept. Credentials are references only.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum ConfigurationValueV1 {
    Boolean(bool),
    Unsigned(u64),
    Text(String),
    StringList(Vec<String>),
    SourceBindings(Vec<ScopeSourceBinding>),
    AccessRules(Vec<ScopeAccessRule>),
    DefaultCollection(Option<CollectionSelectorV1>),
    AnalyzerSettings(AnalyzerSettingsV1),
    WorkTopologyPolicy(Box<WorkTopologyPolicyV1>),
    CredentialReference(CredentialReferenceMetadataV1),
}

impl ConfigurationValueV1 {
    pub const fn kind(&self) -> ConfigurationValueKindV1 {
        match self {
            Self::Boolean(_) => ConfigurationValueKindV1::Boolean,
            Self::Unsigned(_) => ConfigurationValueKindV1::Unsigned,
            Self::Text(_) => ConfigurationValueKindV1::Text,
            Self::StringList(_) => ConfigurationValueKindV1::StringList,
            Self::SourceBindings(_) => ConfigurationValueKindV1::SourceBindings,
            Self::AccessRules(_) => ConfigurationValueKindV1::AccessRules,
            Self::DefaultCollection(_) => ConfigurationValueKindV1::DefaultCollection,
            Self::AnalyzerSettings(_) => ConfigurationValueKindV1::AnalyzerSettings,
            Self::WorkTopologyPolicy(_) => ConfigurationValueKindV1::WorkTopologyPolicy,
            Self::CredentialReference(_) => ConfigurationValueKindV1::CredentialReference,
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Boolean(_) | Self::Unsigned(_) => Ok(()),
            Self::Text(value) => validate_canonical_label(value, "configuration text value"),
            Self::StringList(values) => {
                for value in values {
                    validate_canonical_label(value, "configuration text list value")?;
                }
                Ok(())
            }
            Self::SourceBindings(bindings) => {
                ensure_strict_order(
                    bindings.iter().map(|binding| &binding.binding_id),
                    "source binding order",
                )?;
                for binding in bindings {
                    binding.validate()?;
                }
                Ok(())
            }
            Self::AccessRules(rules) => {
                ensure_strict_order(rules.iter().map(|rule| &rule.rule_id), "access rule order")?;
                for rule in rules {
                    rule.validate()?;
                }
                Ok(())
            }
            Self::DefaultCollection(selector) => selector
                .as_ref()
                .map_or(Ok(()), CollectionSelectorV1::validate),
            Self::AnalyzerSettings(settings) => settings.validate(),
            Self::WorkTopologyPolicy(policy) => policy.validate(),
            Self::CredentialReference(metadata) => metadata.validate(),
        }
    }
}

fn ensure_strict_order<'a, T: Ord + 'a>(
    values: impl Iterator<Item = &'a T>,
    field: &'static str,
) -> Result<(), DomainError> {
    let values: Vec<_> = values.collect();
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(DomainError::NonCanonical { field });
    }
    Ok(())
}

/// One registered setting definition. The registry owns the definition;
/// adapters must use it rather than choosing a local default or schema.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SettingDefinitionV1 {
    pub key: SettingKey,
    pub schema_revision: u16,
    pub value_kind: ConfigurationValueKindV1,
    pub default_value: ConfigurationValueV1,
    pub sensitivity: SettingSensitivityV1,
    pub scope: SettingScopeV1,
    pub restart_requirement: RestartRequirementV1,
    pub deprecation: DeprecationStateV1,
}

impl SettingDefinitionV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.key.validate()?;
        if self.schema_revision == 0 || self.default_value.kind() != self.value_kind {
            return Err(DomainError::NonCanonical {
                field: "configuration setting definition",
            });
        }
        self.default_value.validate()?;
        self.deprecation.validate()
    }
}

/// Authoritative scope of a source binding. A mutable path, label, or host
/// profile cannot be represented as authority.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum AuthorityRef {
    Project(ProjectId),
    ProjectlessHermes(UserProfileId),
}

impl AuthorityRef {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Project(project_id) => project_id.validate(),
            Self::ProjectlessHermes(profile_id) => profile_id.validate(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SourceKindV1 {
    Claude,
    Codex,
    Cursor,
    Hermes,
    Kiro,
}

/// A source-to-authority binding. It stores only the source kind, a redacted
/// locator digest, and the pre-resolved authority reference.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScopeSourceBinding {
    pub binding_id: SourceBindingId,
    pub source_kind: SourceKindV1,
    pub source_locator_digest: LocatorDigest,
    pub authority: AuthorityRef,
}

impl ScopeSourceBinding {
    pub fn new(
        binding_id: SourceBindingId,
        source_kind: SourceKindV1,
        source_locator_digest: LocatorDigest,
        authority: AuthorityRef,
    ) -> Result<Self, DomainError> {
        let binding = Self {
            binding_id,
            source_kind,
            source_locator_digest,
            authority,
        };
        binding.validate()?;
        Ok(binding)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.binding_id.validate()?;
        self.source_locator_digest.validate()?;
        self.authority.validate()?;
        if matches!(self.authority, AuthorityRef::ProjectlessHermes(_))
            && self.source_kind != SourceKindV1::Hermes
        {
            return Err(DomainError::NonCanonical {
                field: "projectless source binding",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ScopeControlOperationV1 {
    Read,
    SourceBind,
    SourceRebind,
    SourceUnbind,
    AccessRuleUpsert,
    AccessRuleRemove,
    SetDefaultCollection,
    ReplaceTopologyPolicy,
    Rollback,
}

/// Typed rule selectors. Unset dimensions match all values at that dimension,
/// but at least one dimension must be constrained. Free-form paths, labels,
/// collection names, and branch names are deliberately absent.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScopeAccessSubjectV1 {
    pub actor: Option<ActorId>,
    pub operation: Option<ScopeControlOperationV1>,
    pub source_kind: Option<SourceKindV1>,
}

impl ScopeAccessSubjectV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.actor.is_none() && self.operation.is_none() && self.source_kind.is_none() {
            return Err(DomainError::Empty {
                field: "access rule subject",
            });
        }
        self.actor.as_ref().map_or(Ok(()), ActorId::validate)
    }

    fn applies_to(&self, context: &CapabilityResolutionContextV1) -> bool {
        self.actor
            .as_ref()
            .is_none_or(|actor| actor == &context.actor)
            && self
                .operation
                .is_none_or(|operation| context.operation == Some(operation))
            && self
                .source_kind
                .is_none_or(|source_kind| source_kind == context.source_kind)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RuleEffect {
    Allow,
    Deny,
}

/// Restrictive policy input. An allow never grants capabilities absent from
/// the independently authorized capability set passed to the resolver.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScopeAccessRule {
    pub rule_id: AccessRuleId,
    pub subject: ScopeAccessSubjectV1,
    pub authority: AuthorityRef,
    pub capabilities: BTreeSet<CapabilityId>,
    pub effect: RuleEffect,
    pub expires_at: Option<UtcMicros>,
}

impl ScopeAccessRule {
    pub fn new(
        rule_id: AccessRuleId,
        subject: ScopeAccessSubjectV1,
        authority: AuthorityRef,
        capabilities: BTreeSet<CapabilityId>,
        effect: RuleEffect,
        expires_at: Option<UtcMicros>,
    ) -> Result<Self, DomainError> {
        let rule = Self {
            rule_id,
            subject,
            authority,
            capabilities,
            effect,
            expires_at,
        };
        rule.validate()?;
        Ok(rule)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.rule_id.validate()?;
        self.subject.validate()?;
        self.authority.validate()?;
        if self.capabilities.is_empty() {
            return Err(DomainError::Empty {
                field: "access rule capabilities",
            });
        }
        for capability in &self.capabilities {
            capability.validate()?;
        }
        Ok(())
    }

    fn applies_to(&self, context: &CapabilityResolutionContextV1) -> bool {
        self.authority == context.authority
            && self.subject.applies_to(context)
            && self
                .expires_at
                .is_none_or(|expires_at| context.evaluated_at < expires_at)
    }
}

/// Inputs required to resolve restrictive allow/deny policy. This is not an
/// authorization grant; `base_capabilities` remains independently authorized
/// input from the owning policy layer.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CapabilityResolutionContextV1 {
    pub actor: ActorId,
    pub operation: Option<ScopeControlOperationV1>,
    pub source_kind: SourceKindV1,
    pub authority: AuthorityRef,
    pub evaluated_at: UtcMicros,
}

impl CapabilityResolutionContextV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.actor.validate()?;
        self.authority.validate()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestrictiveCapabilityResolutionV1 {
    pub effective: BTreeSet<CapabilityId>,
    pub denied: BTreeSet<CapabilityId>,
    pub allow_intersection: Option<BTreeSet<CapabilityId>>,
}

/// Resolve the configured restrictive policy: all applicable denies union,
/// all applicable allows intersect, then deny wins. The function is pure and
/// cannot widen the caller's independently authorized capability set.
pub fn resolve_restrictive_capabilities(
    base_capabilities: BTreeSet<CapabilityId>,
    rules: &[ScopeAccessRule],
    context: &CapabilityResolutionContextV1,
) -> Result<RestrictiveCapabilityResolutionV1, DomainError> {
    context.validate()?;
    for capability in &base_capabilities {
        capability.validate()?;
    }

    let mut denied = BTreeSet::new();
    let mut allow_intersection: Option<BTreeSet<CapabilityId>> = None;
    for rule in rules {
        rule.validate()?;
        if !rule.applies_to(context) {
            continue;
        }
        match rule.effect {
            RuleEffect::Deny => denied.extend(rule.capabilities.iter().cloned()),
            RuleEffect::Allow => {
                let allowed = rule.capabilities.clone();
                allow_intersection = Some(match allow_intersection {
                    Some(current) => current.intersection(&allowed).cloned().collect(),
                    None => allowed,
                });
            }
        }
    }

    let mut effective = match &allow_intersection {
        Some(allowed) => base_capabilities.intersection(allowed).cloned().collect(),
        None => base_capabilities,
    };
    effective.retain(|capability| !denied.contains(capability));
    Ok(RestrictiveCapabilityResolutionV1 {
        effective,
        denied,
        allow_intersection,
    })
}

/// The protected configuration operation set. Ordinary scalar mutations are
/// intentionally absent; they activate directly after validation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum ProtectedChange {
    BindSource(ScopeSourceBinding),
    RebindSource(ScopeSourceBinding),
    UnbindSource { binding_id: SourceBindingId },
    UpsertAccessRule(ScopeAccessRule),
    RemoveAccessRule { rule_id: AccessRuleId },
    ReplaceWorkTopologyPolicy(WorkTopologyPolicyV1),
}

impl ProtectedChange {
    pub fn operation_kind(&self) -> ScopeControlOperationV1 {
        match self {
            Self::BindSource(_) => ScopeControlOperationV1::SourceBind,
            Self::RebindSource(_) => ScopeControlOperationV1::SourceRebind,
            Self::UnbindSource { .. } => ScopeControlOperationV1::SourceUnbind,
            Self::UpsertAccessRule(_) => ScopeControlOperationV1::AccessRuleUpsert,
            Self::RemoveAccessRule { .. } => ScopeControlOperationV1::AccessRuleRemove,
            Self::ReplaceWorkTopologyPolicy(_) => ScopeControlOperationV1::ReplaceTopologyPolicy,
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::BindSource(binding) | Self::RebindSource(binding) => binding.validate(),
            Self::UnbindSource { binding_id } => binding_id.validate(),
            Self::UpsertAccessRule(rule) => rule.validate(),
            Self::RemoveAccessRule { rule_id } => rule_id.validate(),
            Self::ReplaceWorkTopologyPolicy(policy) => policy.validate(),
        }
    }

    pub fn compute_digest(&self) -> Result<ManifestDigest, DomainError> {
        self.validate()?;
        canonical_sha256(&(PROTECTED_CHANGE_DIGEST_DOMAIN, self))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RedactedConfigurationChangeV1 {
    pub setting_key: SettingKey,
    pub operation: ScopeControlOperationV1,
    pub before_digest: Option<ManifestDigest>,
    pub after_digest: Option<ManifestDigest>,
}

impl RedactedConfigurationChangeV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.setting_key.validate()?;
        self.before_digest
            .as_ref()
            .map_or(Ok(()), ManifestDigest::validate)?;
        self.after_digest
            .as_ref()
            .map_or(Ok(()), ManifestDigest::validate)?;
        if self.before_digest.is_none() && self.after_digest.is_none() {
            return Err(DomainError::Empty {
                field: "redacted configuration change digest",
            });
        }
        Ok(())
    }
}

/// Immutable dry-run result. It contains no raw locator, secret, target
/// identity, or plaintext configuration value.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProtectedChangePlan {
    pub plan_id: ChangePlanId,
    pub actor_id: ActorId,
    pub base_revision_id: ConfigurationRevisionId,
    pub operation_digest: ManifestDigest,
    pub resolved_scope_digest: ManifestDigest,
    pub membership_digest: Option<ManifestDigest>,
    pub authorization_policy_digest: AccessPolicyDigest,
    pub policy_epoch: u64,
    pub expires_at: UtcMicros,
    pub created_at: UtcMicros,
    pub redacted_changes: Vec<RedactedConfigurationChangeV1>,
}

impl ProtectedChangePlan {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.plan_id.validate()?;
        self.actor_id.validate()?;
        self.base_revision_id.validate()?;
        self.operation_digest.validate()?;
        self.resolved_scope_digest.validate()?;
        self.membership_digest
            .as_ref()
            .map_or(Ok(()), ManifestDigest::validate)?;
        self.authorization_policy_digest.validate()?;
        if self.expires_at <= self.created_at || self.redacted_changes.is_empty() {
            return Err(DomainError::NonCanonical {
                field: "protected configuration change plan",
            });
        }
        for change in &self.redacted_changes {
            change.validate()?;
        }
        Ok(())
    }

    pub fn is_expired_at(&self, now: UtcMicros) -> bool {
        now >= self.expires_at
    }
}

/// Confirmation required to apply a protected change or forward rollback.
/// The actor and operation digest must match the immutable dry-run plan.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProtectedApplyRequest {
    pub plan_id: ChangePlanId,
    pub actor_id: ActorId,
    pub expected_base_revision_id: ConfigurationRevisionId,
    pub operation_digest: ManifestDigest,
    pub idempotency_key: ConfigurationIdempotencyKey,
}

impl ProtectedApplyRequest {
    pub fn validate_against(
        &self,
        plan: &ProtectedChangePlan,
        now: UtcMicros,
    ) -> Result<(), DomainError> {
        self.plan_id.validate()?;
        self.actor_id.validate()?;
        self.expected_base_revision_id.validate()?;
        self.operation_digest.validate()?;
        self.idempotency_key.validate()?;
        plan.validate()?;
        if plan.is_expired_at(now)
            || self.plan_id != plan.plan_id
            || self.actor_id != plan.actor_id
            || self.expected_base_revision_id != plan.base_revision_id
            || self.operation_digest != plan.operation_digest
        {
            return Err(DomainError::SnapshotMismatch {
                field: "protected configuration apply request",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RollbackModeV1 {
    AllOrNothing,
    Partial,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationAuditEventKindV1 {
    DryRunCreated,
    Applied,
    Rejected,
    Expired,
    ActivationFailed,
    RollbackDryRunCreated,
    RollbackApplied,
    Recovered,
}

/// Append-only audit record. `target_commitment` is event-scoped and cannot be
/// joined across audit events; a caller must be separately authorized before
/// any canonical target is resolved by the store/application layer.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationAuditEvent {
    pub event_id: ConfigurationAuditEventId,
    pub event_kind: ConfigurationAuditEventKindV1,
    pub actor_id: ActorId,
    pub idempotency_key: Option<ConfigurationIdempotencyKey>,
    pub base_revision_id: ConfigurationRevisionId,
    pub result_revision_id: Option<ConfigurationRevisionId>,
    pub operation_digest: ManifestDigest,
    pub target_commitment: ManifestDigest,
    pub receipt_id: Option<ConfigurationReceiptId>,
    pub safe_reason_code: Option<String>,
    pub occurred_at: UtcMicros,
}

impl ConfigurationAuditEvent {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.event_id.validate()?;
        self.actor_id.validate()?;
        self.idempotency_key
            .as_ref()
            .map_or(Ok(()), ConfigurationIdempotencyKey::validate)?;
        self.base_revision_id.validate()?;
        self.result_revision_id
            .as_ref()
            .map_or(Ok(()), ConfigurationRevisionId::validate)?;
        self.operation_digest.validate()?;
        self.target_commitment.validate()?;
        self.receipt_id
            .as_ref()
            .map_or(Ok(()), ConfigurationReceiptId::validate)?;
        self.safe_reason_code.as_ref().map_or(Ok(()), |reason| {
            validate_canonical_label(reason, "audit reason code")
        })
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CandidateDispositionV1 {
    Winning,
    Overridden,
    Rejected,
    Defaulted,
}

/// Resolution provenance is intentionally distinct from behavior. Moving the
/// same winner between layers can change this material without changing the
/// effective behavior digest.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationCandidateV1 {
    pub layer: ConfigurationLayerIdV1,
    pub revision_id: ConfigurationRevisionId,
    pub disposition: CandidateDispositionV1,
    pub safe_reason: Option<String>,
}

impl ConfigurationCandidateV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.layer.validate()?;
        self.revision_id.validate()?;
        self.safe_reason.as_ref().map_or(Ok(()), |reason| {
            validate_canonical_label(reason, "configuration candidate reason")
        })
    }
}

/// Effective configuration snapshot with separate behavior and provenance
/// digests. It is pure data: loading/activating it is a daemon concern.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationSnapshotV1 {
    pub snapshot_id: ConfigurationSnapshotId,
    pub effective_behavior_digest: ManifestDigest,
    pub resolution_provenance_digest: ManifestDigest,
    pub effective_values: BTreeMap<SettingKey, ConfigurationValueV1>,
    pub provenance: BTreeMap<SettingKey, Vec<ConfigurationCandidateV1>>,
}

impl ConfigurationSnapshotV1 {
    pub fn new(
        effective_values: BTreeMap<SettingKey, ConfigurationValueV1>,
        provenance: BTreeMap<SettingKey, Vec<ConfigurationCandidateV1>>,
    ) -> Result<Self, DomainError> {
        for (key, value) in &effective_values {
            key.validate()?;
            value.validate()?;
        }
        for (key, candidates) in &provenance {
            key.validate()?;
            if candidates.is_empty() {
                return Err(DomainError::Empty {
                    field: "configuration provenance candidates",
                });
            }
            for candidate in candidates {
                candidate.validate()?;
            }
        }
        let effective_behavior_digest =
            canonical_sha256(&("tracedecay.configuration.behavior.v1", &effective_values))?;
        let resolution_provenance_digest =
            canonical_sha256(&("tracedecay.configuration.provenance.v1", &provenance))?;
        let snapshot_id = derive_configuration_snapshot_id(
            &effective_behavior_digest,
            &resolution_provenance_digest,
        )?;
        Ok(Self {
            snapshot_id,
            effective_behavior_digest,
            resolution_provenance_digest,
            effective_values,
            provenance,
        })
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        let expected = Self::new(self.effective_values.clone(), self.provenance.clone())?;
        if self.snapshot_id != expected.snapshot_id
            || self.effective_behavior_digest != expected.effective_behavior_digest
            || self.resolution_provenance_digest != expected.resolution_provenance_digest
        {
            return Err(DomainError::SnapshotMismatch {
                field: "configuration snapshot identity",
            });
        }
        Ok(())
    }
}

fn derive_configuration_snapshot_id(
    effective_behavior_digest: &ManifestDigest,
    resolution_provenance_digest: &ManifestDigest,
) -> Result<ConfigurationSnapshotId, DomainError> {
    let digest = canonical_sha256(&(
        CONFIGURATION_SNAPSHOT_ID_DOMAIN,
        effective_behavior_digest,
        resolution_provenance_digest,
    ))?;
    let encoded = digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or(DomainError::NonCanonical {
            field: "configuration snapshot digest",
        })?;
    ConfigurationSnapshotId::new(format!("{CONFIGURATION_SNAPSHOT_ID_DOMAIN}.{encoded}"))
}
