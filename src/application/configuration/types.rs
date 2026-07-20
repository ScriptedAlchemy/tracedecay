//! Transport-neutral configuration control-plane DTOs.

use std::collections::BTreeSet;
use std::fmt;

use thiserror::Error;
use tracedecay_domain::configuration::{
    ChangePlanId, ConfigurationAuditEvent, ConfigurationAuditEventId, ConfigurationCandidateV1,
    ConfigurationReceiptId, ConfigurationRevisionId, ConfigurationSnapshotId, ConfigurationValueV1,
    CredentialKindV1, CredentialReferenceId, ProtectedChange, RedactedConfigurationChangeV1,
    RestartRequirementV1, RollbackModeV1, SettingKey, SettingSensitivityV1,
};
use tracedecay_domain::{ActorId, ManifestDigest, UtcMicros};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizedActor {
    pub actor_id: ActorId,
}

impl AuthorizedActor {
    pub fn validate(&self) -> Result<(), ConfigurationError> {
        self.actor_id
            .validate()
            .map_err(ConfigurationError::validation)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingSummary {
    pub key: SettingKey,
    pub sensitivity: SettingSensitivityV1,
    pub restart_requirement: RestartRequirementV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedSetting {
    pub key: SettingKey,
    pub effective_value: ConfigurationValueV1,
    pub snapshot_id: ConfigurationSnapshotId,
    pub effective_behavior_digest: ManifestDigest,
    pub resolution_provenance_digest: ManifestDigest,
    pub candidates: Vec<ConfigurationCandidateV1>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DirectConfigurationMutation {
    Set {
        key: SettingKey,
        value: ConfigurationValueV1,
    },
    Unset {
        key: SettingKey,
    },
    Batch {
        mutations: Vec<DirectConfigurationMutation>,
    },
}

impl DirectConfigurationMutation {
    pub fn touched_keys(&self) -> Result<BTreeSet<SettingKey>, ConfigurationError> {
        match self {
            Self::Set { key, .. } | Self::Unset { key } => {
                key.validate().map_err(ConfigurationError::validation)?;
                Ok(BTreeSet::from([key.clone()]))
            }
            Self::Batch { mutations } => {
                if mutations.is_empty() {
                    return Err(ConfigurationError::validation_message(
                        "direct configuration batch must be non-empty",
                    ));
                }
                let mut keys = BTreeSet::new();
                for mutation in mutations {
                    for key in mutation.touched_keys()? {
                        if !keys.insert(key) {
                            return Err(ConfigurationError::validation_message(
                                "direct configuration batch contains duplicate keys",
                            ));
                        }
                    }
                }
                Ok(keys)
            }
        }
    }
}

/// Opaque write-handle returned by a secret-safe adapter. The secret material
/// is never present in the application DTO, request logs, receipts, audit, or
/// configuration read path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialWriteHandleV1(String);

impl CredentialWriteHandleV1 {
    pub fn new(value: impl Into<String>) -> Result<Self, ConfigurationError> {
        let value = value.into();
        if value.is_empty()
            || value.trim() != value
            || value.len() > 512
            || value.chars().any(char::is_control)
        {
            return Err(ConfigurationError::validation_message(
                "credential write handle is not canonical",
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Write-only credential operation. The concrete secure sink resolves
/// `write_handle`; no field can carry plaintext credential material.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteOnlyCredentialMutation {
    pub expected_reference_id: Option<CredentialReferenceId>,
    pub kind: CredentialKindV1,
    pub write_handle: CredentialWriteHandleV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComponentConfigurationState {
    pub component: String,
    pub desired_revision_id: ConfigurationRevisionId,
    pub observed_revision_id: Option<ConfigurationRevisionId>,
    pub restart_required: bool,
    pub activation_error_code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigurationMutationReceipt {
    pub receipt_id: ConfigurationReceiptId,
    pub base_revision_id: ConfigurationRevisionId,
    pub result_revision_id: ConfigurationRevisionId,
    pub snapshot_id: ConfigurationSnapshotId,
    pub operation_digest: ManifestDigest,
    pub created_at: UtcMicros,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigurationAuditQuery {
    pub after_event_id: Option<ConfigurationAuditEventId>,
    pub limit: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigurationAuditPage {
    pub events: Vec<ConfigurationAuditEvent>,
    pub next_after_event_id: Option<ConfigurationAuditEventId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigurationRollbackRequest {
    pub target_revision_id: ConfigurationRevisionId,
    pub mode: RollbackModeV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigurationPlanContext {
    pub plan_id: ChangePlanId,
    pub change: ProtectedChange,
    pub redacted_changes: Vec<RedactedConfigurationChangeV1>,
    pub operation_digest: ManifestDigest,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ConfigurationError {
    #[error("target unavailable")]
    TargetUnavailable,
    #[error("authorized target is ambiguous")]
    AuthorizedTargetAmbiguous,
    #[error("configuration revision conflict")]
    RevisionConflict,
    #[error("configuration change plan expired")]
    PlanExpired,
    #[error("configuration change plan is stale")]
    PlanStale,
    #[error("configuration policy widening is forbidden")]
    PolicyWideningForbidden,
    #[error("projectless Hermes requires a user profile authority")]
    ProjectlessProfileRequired,
    #[error("configuration idempotency key conflicts with prior input")]
    IdempotencyConflict,
    #[error("configuration validation failed: {0}")]
    Validation(String),
    #[error("configuration authority is unavailable")]
    Unavailable,
}

impl ConfigurationError {
    pub fn validation(error: impl fmt::Display) -> Self {
        Self::Validation(error.to_string())
    }

    pub fn validation_message(message: impl Into<String>) -> Self {
        Self::Validation(message.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_only_credential_mutation_has_no_plaintext_field() {
        let mutation = WriteOnlyCredentialMutation {
            expected_reference_id: None,
            kind: CredentialKindV1::ApiToken,
            write_handle: CredentialWriteHandleV1::new("credential-write.fixture").unwrap(),
        };
        let debug = format!("{mutation:?}");
        assert!(!debug.contains("plaintext"));
        assert!(!debug.contains("secret_value"));
    }
}
