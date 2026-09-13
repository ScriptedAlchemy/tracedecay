//! Store-only configuration control-plane types plus application DTO imports.

use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;
use thiserror::Error;
use tracedecay_domain::configuration::{
    ChangePlanId, ConfigurationAuditEventId, ConfigurationIdempotencyKey, ConfigurationLayerIdV1,
    ConfigurationMutationGrantReceiptV1, ConfigurationRevisionId, ConfigurationValueV1,
    ProtectedChange, RedactedConfigurationChangeV1, RollbackModeV1, SettingKey,
};
use tracedecay_domain::{ActorId, ManifestDigest, canonical_sha256};

pub use tracedecay_contracts::configuration::{
    ActivationDriftV1, ComponentConfigurationState, ConfigurationAuditPage,
    ConfigurationMutationReceipt, ConfigurationSettlementAuthorityV1, ResolvedSetting,
    SettingSummary,
};

pub const CONFIGURATION_AUDIT_PAGE_LIMIT: usize = 1_000;

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

/// Mutation authority is never inferred from an actor identifier. It is a
/// current policy/grant receipt whose complete binding is rechecked by the
/// authorization port immediately before each durable effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigurationMutationAuthority {
    pub receipt: ConfigurationMutationGrantReceiptV1,
}

impl ConfigurationMutationAuthority {
    pub fn actor(&self) -> AuthorizedActor {
        AuthorizedActor {
            actor_id: self.receipt.actor_id.clone(),
        }
    }

    pub fn validate_integrity(&self) -> Result<(), ConfigurationError> {
        self.receipt
            .validate()
            .map_err(|_| ConfigurationError::MutationAuthorityRejected)
    }

    pub fn direct_idempotency_key(
        &self,
    ) -> Result<&ConfigurationIdempotencyKey, ConfigurationError> {
        self.validate_integrity()?;
        if self.receipt.operation
            != tracedecay_domain::configuration::ConfigurationMutationOperationV1::DirectMutation
        {
            return Err(ConfigurationError::MutationAuthorityRejected);
        }
        self.idempotency_key()
    }

    pub fn idempotency_key(&self) -> Result<&ConfigurationIdempotencyKey, ConfigurationError> {
        self.validate_integrity()?;
        self.receipt
            .idempotency_key
            .as_ref()
            .ok_or(ConfigurationError::MutationAuthorityRejected)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum DirectConfigurationMutation {
    Set {
        layer: ConfigurationLayerIdV1,
        key: SettingKey,
        value: Box<ConfigurationValueV1>,
    },
    Unset {
        layer: ConfigurationLayerIdV1,
        key: SettingKey,
    },
    Batch {
        mutations: Vec<DirectConfigurationMutation>,
    },
}

impl DirectConfigurationMutation {
    pub fn touched_keys(&self) -> Result<BTreeSet<SettingKey>, ConfigurationError> {
        match self {
            Self::Set { layer, key, .. } | Self::Unset { layer, key } => {
                layer.validate().map_err(ConfigurationError::validation)?;
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

    pub fn target_layer(&self) -> Result<&ConfigurationLayerIdV1, ConfigurationError> {
        match self {
            Self::Set { layer, .. } | Self::Unset { layer, .. } => Ok(layer),
            Self::Batch { mutations } => {
                let first = mutations.first().ok_or_else(|| {
                    ConfigurationError::validation_message(
                        "direct configuration batch must be non-empty",
                    )
                })?;
                let layer = first.target_layer()?;
                for mutation in &mutations[1..] {
                    if mutation.target_layer()? != layer {
                        return Err(ConfigurationError::validation_message(
                            "direct configuration batch must target one layer",
                        ));
                    }
                }
                Ok(layer)
            }
        }
    }

    pub fn target_scope_digest(&self) -> Result<ManifestDigest, ConfigurationError> {
        configuration_layer_scope_digest(self.target_layer()?)
    }
}

pub fn configuration_layer_scope_digest(
    layer: &ConfigurationLayerIdV1,
) -> Result<ManifestDigest, ConfigurationError> {
    canonical_sha256(&("tracedecay.configuration.direct-target-layer.v1", layer))
        .map_err(ConfigurationError::validation)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigurationAuditQuery {
    pub after_event_id: Option<ConfigurationAuditEventId>,
    pub limit: usize,
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
    #[error("configuration mutation authority is stale, expired, or tampered")]
    MutationAuthorityRejected,
    #[error("configuration validation failed: {0}")]
    Validation(String),
    #[error("configuration reset required: {reason}")]
    ResetRequired { reason: String },
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
