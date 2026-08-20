//! Pure configuration-control-plane contracts.
//!
//! These values define typed settings, deterministic resolution inputs,
//! protected-change plans, and opaque credential references. They deliberately
//! contain no secret values, database handles, authorization decisions, or
//! ambient executable lookup rules. Canonical executable paths are permitted
//! only as digest-pinned provider bindings.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::research::{
    AccessPolicyDigest, ActorId, CapabilityId, DomainError, LocatorDigest, ManifestDigest,
    ProjectId, UtcMicros, canonical_sha256,
};

pub mod topology;
mod work_executable_bindings;
mod work_expertise_consent;

pub use topology::*;
pub use work_executable_bindings::*;
pub use work_expertise_consent::*;

const CONFIGURATION_SNAPSHOT_ID_DOMAIN: &str = "tracedecay.configuration.snapshot.v1";
const PROTECTED_CHANGE_DIGEST_DOMAIN: &str = "tracedecay.configuration.protected-change.v1";

/// Canonical setting keys owned by the configuration control plane.
pub const SOURCE_BINDINGS_SETTING_KEY: &str = "scope.source_bindings.v1";
pub const ACCESS_RULES_SETTING_KEY: &str = "scope.access_rules.v1";
pub const ANALYZER_SETTINGS_SETTING_KEY: &str = "analyzer.settings.v1";
pub const WORK_TOPOLOGY_POLICY_SETTING_KEY: &str = "work.topology_policy.v1";
pub const WORK_EXECUTABLE_BINDINGS_SETTING_KEY: &str = "work.executable_bindings.v1";
pub const PROJECT_WORK_EXPERTISE_CONSENT_SETTING_KEY: &str = "work.expertise_consent.v1";
pub const CONTEXT_SCOUT_SETTINGS_SETTING_KEY: &str = "context_scout.settings.v1";
pub const AUTOMATION_SETTINGS_SETTING_KEY: &str = "automation.settings.v1";

/// Canonical user-profile settings.
pub const USER_UPLOAD_ENABLED_SETTING_KEY: &str = "user.upload_enabled.v1";
pub const USER_WATCHER_DEBOUNCE_MS_SETTING_KEY: &str = "user.watcher_debounce_ms.v1";
pub const USER_EXTRACTION_TIMEOUT_SECS_SETTING_KEY: &str = "user.extraction_timeout_secs.v1";
pub const USER_WORK_EXPERTISE_CONSENT_SETTING_KEY: &str = "user.work_expertise_consent.v1";

/// Canonical project-scoped runtime settings.
pub const INDEX_EXCLUDE_SETTING_KEY: &str = "index.exclude.v1";
pub const INDEX_INCLUDE_SETTING_KEY: &str = "index.include.v1";
pub const INDEX_MAX_FILE_SIZE_SETTING_KEY: &str = "index.max_file_size.v1";
pub const INDEX_EXTRACT_DOCSTRINGS_SETTING_KEY: &str = "index.extract_docstrings.v1";
pub const INDEX_TRACK_CALL_SITES_SETTING_KEY: &str = "index.track_call_sites.v1";
pub const INDEX_GIT_IGNORE_SETTING_KEY: &str = "index.git_ignore.v1";
pub const DIAGNOSTICS_PREWARM_SETTING_KEY: &str = "diagnostics.prewarm.v1";
pub const SEMANTIC_RUNTIME_SETTING_KEY: &str = "semantic.runtime.v1";
pub const SYNC_AUTO_WATCH_SETTING_KEY: &str = "sync.auto_watch.v1";
pub const SYNC_WATCH_DEBOUNCE_MS_SETTING_KEY: &str = "sync.watch_debounce_ms.v1";
pub const SYNC_WATCH_MAX_DELAY_MS_SETTING_KEY: &str = "sync.watch_max_delay_ms.v1";
pub const SYNC_WATCH_MAX_PROJECTS_SETTING_KEY: &str = "sync.watch_max_projects.v1";
pub const SYNC_READ_REFRESH_SETTING_KEY: &str = "sync.read_refresh.v1";
pub const SYNC_READ_COOLDOWN_SECS_SETTING_KEY: &str = "sync.read_cooldown_secs.v1";
pub const SYNC_SESSION_START_SYNC_SETTING_KEY: &str = "sync.session_start_sync.v1";
pub const SYNC_SESSION_START_STALE_THRESHOLD_SECS_SETTING_KEY: &str =
    "sync.session_start_stale_threshold_secs.v1";
pub const SYNC_BACKSTOP_INTERVAL_MINS_SETTING_KEY: &str = "sync.backstop_interval_mins.v1";
pub const SYNC_FULL_SYNC_ESCALATION_FILES_SETTING_KEY: &str = "sync.full_sync_escalation_files.v1";
pub const SYNC_MAX_CONCURRENT_SYNCS_SETTING_KEY: &str = "sync.max_concurrent_syncs.v1";
pub const SYNC_BRANCH_GC_DAYS_SETTING_KEY: &str = "sync.branch_gc_days.v1";
pub const SYNC_ORPHAN_DB_GC_DAYS_SETTING_KEY: &str = "sync.orphan_db_gc_days.v1";
pub const SYNC_AUTO_INIT_SETTING_KEY: &str = "sync.auto_init.v1";
pub const SYNC_AUTO_TRACK_PR_BRANCHES_SETTING_KEY: &str = "sync.auto_track_pr_branches.v1";
pub const SYNC_AUTO_TRACK_PR_POLL_SECS_SETTING_KEY: &str = "sync.auto_track_pr_poll_secs.v1";
pub const TELEMETRY_TIMINGS_SETTING_KEY: &str = "telemetry.timings.v1";

/// Exact Plan 20 registry inventory. Keeping this closed list in the domain
/// contract prevents adapters and migrations from silently inventing keys.
pub const CONFIGURATION_SETTING_KEYS_V1: &[&str] = &[
    SOURCE_BINDINGS_SETTING_KEY,
    ACCESS_RULES_SETTING_KEY,
    ANALYZER_SETTINGS_SETTING_KEY,
    WORK_TOPOLOGY_POLICY_SETTING_KEY,
    WORK_EXECUTABLE_BINDINGS_SETTING_KEY,
    PROJECT_WORK_EXPERTISE_CONSENT_SETTING_KEY,
    CONTEXT_SCOUT_SETTINGS_SETTING_KEY,
    AUTOMATION_SETTINGS_SETTING_KEY,
    crate::feedback::PROXIMITY_RISK_THRESHOLD_SETTING_KEY_V1,
    USER_UPLOAD_ENABLED_SETTING_KEY,
    USER_WATCHER_DEBOUNCE_MS_SETTING_KEY,
    USER_EXTRACTION_TIMEOUT_SECS_SETTING_KEY,
    USER_WORK_EXPERTISE_CONSENT_SETTING_KEY,
    INDEX_EXCLUDE_SETTING_KEY,
    INDEX_INCLUDE_SETTING_KEY,
    INDEX_MAX_FILE_SIZE_SETTING_KEY,
    INDEX_EXTRACT_DOCSTRINGS_SETTING_KEY,
    INDEX_TRACK_CALL_SITES_SETTING_KEY,
    INDEX_GIT_IGNORE_SETTING_KEY,
    DIAGNOSTICS_PREWARM_SETTING_KEY,
    SEMANTIC_RUNTIME_SETTING_KEY,
    SYNC_AUTO_WATCH_SETTING_KEY,
    SYNC_WATCH_DEBOUNCE_MS_SETTING_KEY,
    SYNC_WATCH_MAX_DELAY_MS_SETTING_KEY,
    SYNC_WATCH_MAX_PROJECTS_SETTING_KEY,
    SYNC_READ_REFRESH_SETTING_KEY,
    SYNC_READ_COOLDOWN_SECS_SETTING_KEY,
    SYNC_SESSION_START_SYNC_SETTING_KEY,
    SYNC_SESSION_START_STALE_THRESHOLD_SECS_SETTING_KEY,
    SYNC_BACKSTOP_INTERVAL_MINS_SETTING_KEY,
    SYNC_FULL_SYNC_ESCALATION_FILES_SETTING_KEY,
    SYNC_MAX_CONCURRENT_SYNCS_SETTING_KEY,
    SYNC_BRANCH_GC_DAYS_SETTING_KEY,
    SYNC_ORPHAN_DB_GC_DAYS_SETTING_KEY,
    SYNC_AUTO_INIT_SETTING_KEY,
    SYNC_AUTO_TRACK_PR_BRANCHES_SETTING_KEY,
    SYNC_AUTO_TRACK_PR_POLL_SECS_SETTING_KEY,
    TELEMETRY_TIMINGS_SETTING_KEY,
];

validated_string_newtype!(
    schema,
    DomainError,
    validate_canonical_label;
    UserProfileId => "user profile id",
    SourceBindingId => "source binding id",
    AccessRuleId => "access rule id",
    QueryCollectionId => "query collection id",
    ConfigurationRevisionId => "configuration revision id",
    ConfigurationSnapshotId => "configuration snapshot id",
    ChangePlanId => "configuration change plan id",
    ConfigurationReceiptId => "configuration receipt id",
    ConfigurationAuditEventId => "configuration audit event id",
    ConfigurationIdempotencyKey => "configuration idempotency key",
    ConfigurationGrantReceiptId => "configuration grant receipt id",
    ConfigurationGrantId => "configuration grant id",
    CredentialReferenceId => "credential reference id",
    AnalyzerExecutableId => "analyzer executable id",
    AnalyzerLanguageId => "analyzer language id",
    AnalyzerEnvironmentVariable => "analyzer environment variable",
);

const CONFIGURATION_GRANT_RECEIPT_DIGEST_DOMAIN: &str = "tracedecay.configuration.grant-receipt.v1";

/// Closed mutation operations that a policy/grant receipt may authorize.
/// Read operations deliberately use separate discovery/read authorization.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationMutationOperationV1 {
    DirectMutation,
    CredentialWrite,
    ProtectedDryRun,
    ProtectedApply,
    RollbackDryRun,
    RollbackApply,
}

/// Sink at which the configuration effect will be admitted. A receipt for one
/// sink cannot be replayed at another.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationMutationSinkV1 {
    ConfigurationStore,
    CredentialStore,
    ConfigurationAudit,
}

/// Exact effect class admitted by policy. This prevents a read or preview
/// receipt from authorizing a durable configuration or credential write.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationMutationEffectV1 {
    AppendAuditOnly,
    CreateProtectedChangePlan,
    CommitConfigurationRevision,
    WriteCredentialReference,
}

/// Immutable current-policy/grant receipt minted by the policy/application
/// authorization boundary. Configuration operations verify its canonical
/// digest locally and ask the policy port to recheck current grant, policy,
/// scope, revision, sink, and effect state immediately before mutation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationMutationGrantReceiptV1 {
    pub receipt_id: ConfigurationGrantReceiptId,
    pub grant_id: ConfigurationGrantId,
    pub actor_id: ActorId,
    pub operation: ConfigurationMutationOperationV1,
    pub scope_digest: ManifestDigest,
    pub expected_configuration_revision: ConfigurationRevisionId,
    pub policy_epoch: u64,
    pub policy_digest: AccessPolicyDigest,
    pub sink: ConfigurationMutationSinkV1,
    pub effect: ConfigurationMutationEffectV1,
    pub idempotency_key: Option<ConfigurationIdempotencyKey>,
    pub issued_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub receipt_digest: ManifestDigest,
}

impl ConfigurationMutationGrantReceiptV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn issue(
        receipt_id: ConfigurationGrantReceiptId,
        grant_id: ConfigurationGrantId,
        actor_id: ActorId,
        operation: ConfigurationMutationOperationV1,
        scope_digest: ManifestDigest,
        expected_configuration_revision: ConfigurationRevisionId,
        policy_epoch: u64,
        policy_digest: AccessPolicyDigest,
        sink: ConfigurationMutationSinkV1,
        effect: ConfigurationMutationEffectV1,
        idempotency_key: Option<ConfigurationIdempotencyKey>,
        issued_at: UtcMicros,
        expires_at: UtcMicros,
    ) -> Result<Self, DomainError> {
        let mut receipt = Self {
            receipt_id,
            grant_id,
            actor_id,
            operation,
            scope_digest,
            expected_configuration_revision,
            policy_epoch,
            policy_digest,
            sink,
            effect,
            idempotency_key,
            issued_at,
            expires_at,
            receipt_digest: canonical_sha256(&("pending",))?,
        };
        receipt.validate_fields()?;
        receipt.receipt_digest = receipt.compute_digest()?;
        Ok(receipt)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.validate_fields()?;
        if self.receipt_digest != self.compute_digest()? {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn validate_for(
        &self,
        actor_id: &ActorId,
        operation: ConfigurationMutationOperationV1,
        scope_digest: &ManifestDigest,
        expected_revision: &ConfigurationRevisionId,
        sink: ConfigurationMutationSinkV1,
        effect: ConfigurationMutationEffectV1,
        now: UtcMicros,
    ) -> Result<(), DomainError> {
        self.validate()?;
        if &self.actor_id != actor_id
            || self.operation != operation
            || &self.scope_digest != scope_digest
            || &self.expected_configuration_revision != expected_revision
            || self.sink != sink
            || self.effect != effect
            || now < self.issued_at
            || now >= self.expires_at
        {
            return Err(DomainError::SnapshotMismatch {
                field: "configuration mutation grant receipt",
            });
        }
        Ok(())
    }

    fn validate_fields(&self) -> Result<(), DomainError> {
        self.receipt_id.validate()?;
        self.grant_id.validate()?;
        self.actor_id.validate()?;
        self.scope_digest.validate()?;
        self.expected_configuration_revision.validate()?;
        self.policy_digest.validate()?;
        match (self.operation, self.idempotency_key.as_ref()) {
            (
                ConfigurationMutationOperationV1::DirectMutation
                | ConfigurationMutationOperationV1::CredentialWrite
                | ConfigurationMutationOperationV1::ProtectedApply
                | ConfigurationMutationOperationV1::RollbackApply,
                Some(key),
            ) => key.validate()?,
            (
                ConfigurationMutationOperationV1::DirectMutation
                | ConfigurationMutationOperationV1::CredentialWrite
                | ConfigurationMutationOperationV1::ProtectedApply
                | ConfigurationMutationOperationV1::RollbackApply,
                None,
            ) => {
                return Err(DomainError::NonCanonical {
                    field: "configuration mutation grant receipt idempotency",
                });
            }
            (
                ConfigurationMutationOperationV1::ProtectedDryRun
                | ConfigurationMutationOperationV1::RollbackDryRun,
                None,
            ) => {}
            (
                ConfigurationMutationOperationV1::ProtectedDryRun
                | ConfigurationMutationOperationV1::RollbackDryRun,
                Some(_),
            ) => {
                return Err(DomainError::NonCanonical {
                    field: "configuration mutation preview idempotency",
                });
            }
        }
        if self.policy_epoch == 0 || self.expires_at <= self.issued_at {
            return Err(DomainError::NonCanonical {
                field: "configuration mutation grant receipt lifetime",
            });
        }
        Ok(())
    }

    fn compute_digest(&self) -> Result<ManifestDigest, DomainError> {
        canonical_sha256(&(
            CONFIGURATION_GRANT_RECEIPT_DIGEST_DOMAIN,
            &self.receipt_id,
            &self.grant_id,
            &self.actor_id,
            self.operation,
            &self.scope_digest,
            &self.expected_configuration_revision,
            self.policy_epoch,
            &self.policy_digest,
            self.sink,
            self.effect,
            &self.idempotency_key,
            self.issued_at,
            self.expires_at,
        ))
    }
}

use crate::canonical_text::validate_canonical_string as validate_canonical_label;
use crate::canonical_text::validated_string_newtype;

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
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
#[derive(
    Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
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

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
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

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
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

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationValueKindV1 {
    Boolean,
    Unsigned,
    Text,
    StringList,
    SourceBindings,
    AccessRules,
    AnalyzerSettings,
    WorkTopologyPolicy,
    WorkExecutableBindings,
    WorkExpertiseConsent,
    ContextScoutSettings,
    AutomationSettings,
    CredentialReference,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutConfigurationStateV1 {
    Active,
    Paused,
    Disabled,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutConfigurationModeV1 {
    Deterministic,
    ConfiguredModel,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutConfiguredModelPathV1 {
    CodexAppServer,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AutomationBackendV1 {
    #[default]
    Disabled,
    CodexAppServer,
}

impl AutomationBackendV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::CodexAppServer => "codex_app_server",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AutomationHostModeV1 {
    #[default]
    Standalone,
    DelegatedHost,
}

impl AutomationHostModeV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Standalone => "standalone",
            Self::DelegatedHost => "delegated_host",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct AutomationTaskSettingsV1 {
    pub enabled: bool,
    pub schedule: Option<String>,
    pub interval_secs: Option<u64>,
    pub cooldown_secs: Option<u64>,
    pub min_idle_secs: Option<u64>,
    pub stale_lock_secs: Option<u64>,
    /// Suppression window between session-evidence retrieval attempts while
    /// the evidence budget stays exhausted. Unset uses the automation crate's
    /// one-hour default. Distinct from `cooldown_secs`, which paces retries
    /// after failed runs; budget exhaustion is a skip, not a failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_evidence_budget_backoff_secs: Option<u64>,
}

impl AutomationTaskSettingsV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        for value in [
            self.interval_secs,
            self.cooldown_secs,
            self.min_idle_secs,
            self.stale_lock_secs,
            self.session_evidence_budget_backoff_secs,
        ] {
            if matches!(value, Some(0)) {
                return Err(DomainError::NonCanonical {
                    field: "automation task duration",
                });
            }
        }
        if let Some(schedule) = self.schedule.as_deref() {
            validate_canonical_label(schedule, "automation task schedule")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct AutomationTaskSetV1 {
    pub memory_curator: AutomationTaskSettingsV1,
    pub session_reflector: AutomationTaskSettingsV1,
    pub skill_writer: AutomationTaskSettingsV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AutomationSettingsV1 {
    pub schema_version: u16,
    pub enabled: bool,
    pub backend: AutomationBackendV1,
    pub host_mode: AutomationHostModeV1,
    pub model_id: Option<String>,
    pub timeout_secs: u64,
    pub scheduler_tick_secs: u64,
    pub combine_due_tasks: bool,
    pub allow_job_commands: bool,
    pub tasks: AutomationTaskSetV1,
}

impl AutomationSettingsV1 {
    pub const SCHEMA_VERSION: u16 = 1;

    pub fn validate(&self) -> Result<(), DomainError> {
        if self.schema_version != Self::SCHEMA_VERSION
            || self.timeout_secs == 0
            || self.scheduler_tick_secs == 0
            || matches!(self.backend, AutomationBackendV1::CodexAppServer)
                && self
                    .model_id
                    .as_deref()
                    .is_none_or(|model| model.trim().is_empty())
            || !matches!(self.backend, AutomationBackendV1::CodexAppServer)
                && self.model_id.is_some()
        {
            return Err(DomainError::NonCanonical {
                field: "automation settings",
            });
        }
        if let Some(model_id) = self.model_id.as_deref() {
            validate_canonical_label(model_id, "automation model id")?;
        }
        self.tasks.memory_curator.validate()?;
        self.tasks.session_reflector.validate()?;
        self.tasks.skill_writer.validate()
    }

    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

impl Default for AutomationSettingsV1 {
    fn default() -> Self {
        let scheduled_task = |interval_secs, min_idle_secs| AutomationTaskSettingsV1 {
            enabled: true,
            schedule: Some("interval".to_owned()),
            interval_secs: Some(interval_secs),
            cooldown_secs: Some(300),
            min_idle_secs,
            stale_lock_secs: Some(3_600),
            session_evidence_budget_backoff_secs: None,
        };
        Self {
            schema_version: Self::SCHEMA_VERSION,
            enabled: true,
            backend: AutomationBackendV1::CodexAppServer,
            host_mode: AutomationHostModeV1::Standalone,
            model_id: Some("gpt-5.6-mini".to_owned()),
            timeout_secs: 60,
            scheduler_tick_secs: 60,
            combine_due_tasks: true,
            allow_job_commands: false,
            tasks: AutomationTaskSetV1 {
                memory_curator: scheduled_task(900, None),
                session_reflector: scheduled_task(900, None),
                skill_writer: scheduled_task(3_600, Some(900)),
            },
        }
    }
}

#[cfg(test)]
mod automation_settings_tests {
    use super::{AutomationBackendV1, AutomationHostModeV1, AutomationSettingsV1};

    #[test]
    fn fresh_v2_settings_schedule_the_required_curation_loop() {
        let settings = AutomationSettingsV1::default();

        assert!(settings.enabled);
        assert_eq!(settings.backend, AutomationBackendV1::CodexAppServer);
        assert_eq!(settings.host_mode, AutomationHostModeV1::Standalone);
        assert_eq!(settings.model_id.as_deref(), Some("gpt-5.6-mini"));
        assert_eq!(settings.scheduler_tick_secs, 60);
        assert!(settings.combine_due_tasks);

        assert_eq!(
            (
                settings.tasks.memory_curator.enabled,
                settings.tasks.memory_curator.schedule.as_deref(),
                settings.tasks.memory_curator.interval_secs,
                settings.tasks.memory_curator.cooldown_secs,
                settings.tasks.memory_curator.min_idle_secs,
            ),
            (true, Some("interval"), Some(900), Some(300), None)
        );
        assert_eq!(
            (
                settings.tasks.session_reflector.enabled,
                settings.tasks.session_reflector.schedule.as_deref(),
                settings.tasks.session_reflector.interval_secs,
                settings.tasks.session_reflector.cooldown_secs,
                settings.tasks.session_reflector.min_idle_secs,
            ),
            (true, Some("interval"), Some(900), Some(300), None)
        );
        assert_eq!(
            (
                settings.tasks.skill_writer.enabled,
                settings.tasks.skill_writer.schedule.as_deref(),
                settings.tasks.skill_writer.interval_secs,
                settings.tasks.skill_writer.cooldown_secs,
                settings.tasks.skill_writer.min_idle_secs,
            ),
            (true, Some("interval"), Some(3_600), Some(300), Some(900))
        );
        settings.validate().expect("fresh V2 automation settings");
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutConfigurationLimitsV1 {
    pub max_candidates: u32,
    pub max_evidence: u32,
    pub max_text_bytes: u32,
    pub max_model_input_tokens: u32,
    pub max_model_output_tokens: u32,
}

impl ContextScoutConfigurationLimitsV1 {
    pub const fn bounded_defaults() -> Self {
        Self {
            max_candidates: 32,
            max_evidence: 16,
            max_text_bytes: 4 * 1024,
            max_model_input_tokens: 2_048,
            max_model_output_tokens: 256,
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        let maximum = Self::bounded_defaults();
        if self.max_candidates == 0
            || self.max_candidates > maximum.max_candidates
            || self.max_evidence == 0
            || self.max_evidence > maximum.max_evidence
            || self.max_text_bytes == 0
            || self.max_text_bytes > maximum.max_text_bytes
            || self.max_model_input_tokens == 0
            || self.max_model_input_tokens > maximum.max_model_input_tokens
            || self.max_model_output_tokens == 0
            || self.max_model_output_tokens > maximum.max_model_output_tokens
        {
            return Err(DomainError::NonCanonical {
                field: "context scout configuration limits",
            });
        }
        Ok(())
    }
}

/// Canonical Context Scout control-plane value. Disabled is the only stock
/// state; deterministic or configured-model execution requires an explicit
/// configuration revision.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutSettingsV1 {
    pub schema_version: u16,
    pub state: ContextScoutConfigurationStateV1,
    pub mode: ContextScoutConfigurationModeV1,
    pub limits: ContextScoutConfigurationLimitsV1,
    pub model_path: Option<ContextScoutConfiguredModelPathV1>,
    pub model_id: Option<String>,
    pub model_timeout_secs: Option<u64>,
}

impl ContextScoutSettingsV1 {
    pub const SCHEMA_VERSION: u16 = 1;

    pub const fn disabled() -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            state: ContextScoutConfigurationStateV1::Disabled,
            mode: ContextScoutConfigurationModeV1::Deterministic,
            limits: ContextScoutConfigurationLimitsV1::bounded_defaults(),
            model_path: None,
            model_id: None,
            model_timeout_secs: None,
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        let model_configuration_is_valid = match self.mode {
            ContextScoutConfigurationModeV1::Deterministic => {
                self.model_path.is_none()
                    && self.model_id.is_none()
                    && self.model_timeout_secs.is_none()
            }
            ContextScoutConfigurationModeV1::ConfiguredModel => {
                self.model_path.is_some()
                    && self
                        .model_id
                        .as_deref()
                        .is_some_and(|model| !model.trim().is_empty())
                    && self
                        .model_timeout_secs
                        .is_some_and(|timeout| (5..=300).contains(&timeout))
            }
        };
        if self.schema_version != Self::SCHEMA_VERSION || !model_configuration_is_valid {
            return Err(DomainError::NonCanonical {
                field: "context scout settings",
            });
        }
        if let Some(model_id) = self.model_id.as_deref() {
            validate_canonical_label(model_id, "context scout model id")?;
        }
        self.limits.validate()
    }
}

/// A structured analyzer option value. This deliberately excludes raw
/// environment values, commands, credential material, and transport blobs.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
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

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
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

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum AnalyzerPrivacyClassV1 {
    NonSensitive,
    Sensitive,
    Restricted,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
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

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum AnalyzerRestartPolicyV1 {
    RestartOnConfigurationChange,
    ManualRestartOnly,
}

/// One language's analyzer selection. Host registration may project only the
/// non-sensitive `language_id`/`enabled` pair; all other fields remain in the
/// configuration authority.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
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
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
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
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKindV1 {
    ApiToken,
    AccessToken,
    SigningKeyReference,
    Other,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CredentialReferenceMetadataV1 {
    pub reference_id: CredentialReferenceId,
    pub kind: CredentialKindV1,
    pub reference_digest: ManifestDigest,
    pub operation_digest: ManifestDigest,
    pub settlement_authority: ConfigurationSettlementAuthorityV1,
    pub created_at: UtcMicros,
    pub effective_deadline_at: UtcMicros,
    pub rotation: u64,
}

impl CredentialReferenceMetadataV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.reference_id.validate()?;
        self.reference_digest.validate()?;
        self.operation_digest.validate()?;
        self.settlement_authority.validate()?;
        if self.settlement_authority.revalidated_at > self.created_at
            || self.effective_deadline_at <= self.created_at
        {
            return Err(DomainError::NonCanonical {
                field: "credential write receipt deadline",
            });
        }
        Ok(())
    }
}

/// Original authorization evidence pinned to a durable configuration effect.
/// A retry reauthorizes access separately without replacing these fields.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationSettlementAuthorityV1 {
    pub policy_epoch: u64,
    pub policy_digest: AccessPolicyDigest,
    pub revalidated_at: UtcMicros,
}

impl ConfigurationSettlementAuthorityV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.policy_digest.validate()?;
        if self.policy_epoch == 0 {
            return Err(DomainError::NonCanonical {
                field: "configuration settlement policy epoch",
            });
        }
        Ok(())
    }
}

/// Values that the typed registry can accept. Credentials are references only.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum ConfigurationValueV1 {
    Boolean(bool),
    Unsigned(u64),
    Text(String),
    StringList(Vec<String>),
    SourceBindings(Vec<ScopeSourceBinding>),
    AccessRules(Vec<ScopeAccessRule>),
    AnalyzerSettings(AnalyzerSettingsV1),
    WorkTopologyPolicy(Box<WorkTopologyPolicyV1>),
    WorkExecutableBindings(Vec<WorkExecutableBindingV1>),
    WorkExpertiseConsent(WorkExpertiseConsentV1),
    ContextScoutSettings(ContextScoutSettingsV1),
    AutomationSettings(AutomationSettingsV1),
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
            Self::AnalyzerSettings(_) => ConfigurationValueKindV1::AnalyzerSettings,
            Self::WorkTopologyPolicy(_) => ConfigurationValueKindV1::WorkTopologyPolicy,
            Self::WorkExecutableBindings(_) => ConfigurationValueKindV1::WorkExecutableBindings,
            Self::WorkExpertiseConsent(_) => ConfigurationValueKindV1::WorkExpertiseConsent,
            Self::ContextScoutSettings(_) => ConfigurationValueKindV1::ContextScoutSettings,
            Self::AutomationSettings(_) => ConfigurationValueKindV1::AutomationSettings,
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
            Self::AnalyzerSettings(settings) => settings.validate(),
            Self::WorkTopologyPolicy(policy) => policy.validate(),
            Self::WorkExecutableBindings(bindings) => validate_work_executable_bindings(bindings),
            Self::WorkExpertiseConsent(consent) => consent.validate(),
            Self::ContextScoutSettings(settings) => settings.validate(),
            Self::AutomationSettings(settings) => settings.validate(),
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
#[derive(
    Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
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

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum SourceKindV1 {
    Claude,
    Codex,
    Cursor,
    GitHub,
    Hermes,
    Kiro,
}

/// A source-to-authority binding. It stores only the source kind, a redacted
/// locator digest, and the pre-resolved authority reference.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
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

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum ScopeControlOperationV1 {
    Read,
    SourceBind,
    SourceRebind,
    SourceUnbind,
    AccessRuleUpsert,
    AccessRuleRemove,
    ReplaceTopologyPolicy,
    Rollback,
}

/// Typed rule selectors. Unset dimensions match all values at that dimension,
/// but at least one dimension must be constrained. Free-form paths, labels,
/// collection names, and branch names are deliberately absent.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
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

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum RuleEffect {
    Allow,
    Deny,
}

/// Restrictive policy input. An allow never grants capabilities absent from
/// the independently authorized capability set passed to the resolver.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
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
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
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

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ProtectedChangeSnapshotError {
    #[error("protected change does not apply to the current snapshot")]
    Stale,
    #[error("protected change contains an invalid domain value: {0}")]
    Domain(#[from] DomainError),
    #[error("{0}")]
    IncompatibleValue(&'static str),
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
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
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
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

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum RollbackModeV1 {
    AllOrNothing,
    Partial,
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
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
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
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

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
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
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
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
        if !effective_values.keys().eq(provenance.keys()) {
            return Err(DomainError::SnapshotMismatch {
                field: "configuration value/provenance key set",
            });
        }
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

    /// Apply one protected scope/topology change to a snapshot copy.
    ///
    /// This is pure snapshot transition logic: source bindings, access rules,
    /// topology policy, provenance, and staleness checks. Persistence and CAS
    /// belong to the store adapter.
    pub fn apply_protected_change(
        &self,
        change: &ProtectedChange,
        revision_id: &ConfigurationRevisionId,
    ) -> Result<Self, ProtectedChangeSnapshotError> {
        change.validate()?;
        let mut effective_values = self.effective_values.clone();
        let mut provenance = self.provenance.clone();
        match change {
            ProtectedChange::BindSource(binding) => {
                let key = SettingKey::new(SOURCE_BINDINGS_SETTING_KEY)?;
                let mut bindings = match effective_values.get(&key) {
                    Some(ConfigurationValueV1::SourceBindings(bindings)) => bindings.clone(),
                    Some(_) => {
                        return Err(ProtectedChangeSnapshotError::IncompatibleValue(
                            "source bindings setting has an incompatible typed value",
                        ));
                    }
                    None => Vec::new(),
                };
                if bindings.iter().any(|candidate| {
                    candidate.binding_id == binding.binding_id
                        || (candidate.source_kind == binding.source_kind
                            && candidate.source_locator_digest == binding.source_locator_digest)
                }) {
                    return Err(ProtectedChangeSnapshotError::Stale);
                }
                bindings.push(binding.clone());
                replace_protected_effective_value(
                    &mut effective_values,
                    &mut provenance,
                    key,
                    ConfigurationValueV1::SourceBindings(bindings),
                    revision_id,
                );
            }
            ProtectedChange::RebindSource(binding) => {
                let key = SettingKey::new(SOURCE_BINDINGS_SETTING_KEY)?;
                let mut bindings = match effective_values.get(&key) {
                    Some(ConfigurationValueV1::SourceBindings(bindings)) => bindings.clone(),
                    _ => return Err(ProtectedChangeSnapshotError::Stale),
                };
                let Some(index) = bindings
                    .iter()
                    .position(|candidate| candidate.binding_id == binding.binding_id)
                else {
                    return Err(ProtectedChangeSnapshotError::Stale);
                };
                if bindings
                    .iter()
                    .enumerate()
                    .any(|(candidate_index, candidate)| {
                        candidate_index != index
                            && candidate.source_kind == binding.source_kind
                            && candidate.source_locator_digest == binding.source_locator_digest
                    })
                {
                    return Err(ProtectedChangeSnapshotError::Stale);
                }
                bindings[index] = binding.clone();
                replace_protected_effective_value(
                    &mut effective_values,
                    &mut provenance,
                    key,
                    ConfigurationValueV1::SourceBindings(bindings),
                    revision_id,
                );
            }
            ProtectedChange::UnbindSource { binding_id } => {
                let key = SettingKey::new(SOURCE_BINDINGS_SETTING_KEY)?;
                let mut bindings = match effective_values.get(&key) {
                    Some(ConfigurationValueV1::SourceBindings(bindings)) => bindings.clone(),
                    _ => return Err(ProtectedChangeSnapshotError::Stale),
                };
                let before = bindings.len();
                bindings.retain(|binding| &binding.binding_id != binding_id);
                if bindings.len() == before {
                    return Err(ProtectedChangeSnapshotError::Stale);
                }
                replace_protected_effective_value(
                    &mut effective_values,
                    &mut provenance,
                    key,
                    ConfigurationValueV1::SourceBindings(bindings),
                    revision_id,
                );
            }
            ProtectedChange::UpsertAccessRule(rule) => {
                let key = SettingKey::new(ACCESS_RULES_SETTING_KEY)?;
                let mut rules = match effective_values.get(&key) {
                    Some(ConfigurationValueV1::AccessRules(rules)) => rules.clone(),
                    Some(_) => {
                        return Err(ProtectedChangeSnapshotError::IncompatibleValue(
                            "access rules setting has an incompatible typed value",
                        ));
                    }
                    None => Vec::new(),
                };
                if let Some(index) = rules
                    .iter()
                    .position(|candidate| candidate.rule_id == rule.rule_id)
                {
                    rules[index] = rule.clone();
                } else {
                    rules.push(rule.clone());
                }
                replace_protected_effective_value(
                    &mut effective_values,
                    &mut provenance,
                    key,
                    ConfigurationValueV1::AccessRules(rules),
                    revision_id,
                );
            }
            ProtectedChange::RemoveAccessRule { rule_id } => {
                let key = SettingKey::new(ACCESS_RULES_SETTING_KEY)?;
                let mut rules = match effective_values.get(&key) {
                    Some(ConfigurationValueV1::AccessRules(rules)) => rules.clone(),
                    _ => return Err(ProtectedChangeSnapshotError::Stale),
                };
                let before = rules.len();
                rules.retain(|rule| &rule.rule_id != rule_id);
                if rules.len() == before {
                    return Err(ProtectedChangeSnapshotError::Stale);
                }
                replace_protected_effective_value(
                    &mut effective_values,
                    &mut provenance,
                    key,
                    ConfigurationValueV1::AccessRules(rules),
                    revision_id,
                );
            }
            ProtectedChange::ReplaceWorkTopologyPolicy(policy) => {
                let key = SettingKey::new(WORK_TOPOLOGY_POLICY_SETTING_KEY)?;
                replace_protected_effective_value(
                    &mut effective_values,
                    &mut provenance,
                    key,
                    ConfigurationValueV1::WorkTopologyPolicy(Box::new(policy.clone())),
                    revision_id,
                );
            }
        }
        Self::new(effective_values, provenance).map_err(ProtectedChangeSnapshotError::Domain)
    }
}

fn protected_mutation_provenance(
    revision_id: &ConfigurationRevisionId,
) -> Vec<ConfigurationCandidateV1> {
    vec![ConfigurationCandidateV1 {
        layer: ConfigurationLayerIdV1::Default,
        revision_id: revision_id.clone(),
        disposition: CandidateDispositionV1::Winning,
        safe_reason: None,
    }]
}

fn replace_protected_effective_value(
    effective_values: &mut BTreeMap<SettingKey, ConfigurationValueV1>,
    provenance: &mut BTreeMap<SettingKey, Vec<ConfigurationCandidateV1>>,
    key: SettingKey,
    value: ConfigurationValueV1,
    revision_id: &ConfigurationRevisionId,
) {
    effective_values.insert(key.clone(), value);
    provenance.insert(key, protected_mutation_provenance(revision_id));
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
    let encoded =
        crate::canonical_text::sha256_hex_body(digest.as_str(), "configuration snapshot digest")?;
    ConfigurationSnapshotId::new(format!("{CONFIGURATION_SNAPSHOT_ID_DOMAIN}.{encoded}"))
}
