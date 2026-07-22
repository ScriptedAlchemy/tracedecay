use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::configuration::{ConfigurationRevisionId, ConfigurationSnapshotId};
use tracedecay_domain::{ManifestDigest, UtcMicros, VectorGenerationIdV1, canonical_sha256};

use crate::application::configuration::{
    ConfigurationControlStore, ConfigurationCurrentStateV1, ConfigurationOperationFuture,
};

const SEMANTIC_ACTIVATION_RECEIPT_DIGEST_DOMAIN_V1: &str =
    "tracedecay.semantic-activation-receipt.v1";
const SEMANTIC_ROLLBACK_RECEIPT_DIGEST_DOMAIN_V1: &str = "tracedecay.semantic-rollback-receipt.v1";

pub type SemanticRuntimeFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Read-only configuration view required by the semantic owner. The blanket
/// implementation delegates to the existing central configuration snapshot
/// interface; this seam does not introduce a second configuration authority.
pub trait SemanticConfigurationSnapshotSourceV1: Sync {
    fn current_configuration(
        &self,
    ) -> ConfigurationOperationFuture<'_, ConfigurationCurrentStateV1>;
}

impl<T> SemanticConfigurationSnapshotSourceV1 for T
where
    T: ConfigurationControlStore + ?Sized,
{
    fn current_configuration(
        &self,
    ) -> ConfigurationOperationFuture<'_, ConfigurationCurrentStateV1> {
        self.current()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticConfigurationPinV1 {
    pub revision_id: ConfigurationRevisionId,
    pub snapshot_id: ConfigurationSnapshotId,
    pub effective_behavior_digest: ManifestDigest,
}

impl SemanticConfigurationPinV1 {
    pub fn from_current(
        current: &ConfigurationCurrentStateV1,
    ) -> Result<Self, SemanticRuntimeContractErrorV1> {
        current
            .revision_id
            .validate()
            .map_err(|_| SemanticRuntimeContractErrorV1::InvalidConfiguration)?;
        current
            .snapshot
            .validate()
            .map_err(|_| SemanticRuntimeContractErrorV1::InvalidConfiguration)?;
        Ok(Self {
            revision_id: current.revision_id.clone(),
            snapshot_id: current.snapshot.snapshot_id.clone(),
            effective_behavior_digest: current.snapshot.effective_behavior_digest.clone(),
        })
    }

    pub fn validate(&self) -> Result<(), SemanticRuntimeContractErrorV1> {
        self.revision_id
            .validate()
            .map_err(|_| SemanticRuntimeContractErrorV1::InvalidConfiguration)?;
        self.snapshot_id
            .validate()
            .map_err(|_| SemanticRuntimeContractErrorV1::InvalidConfiguration)?;
        self.effective_behavior_digest
            .validate()
            .map_err(|_| SemanticRuntimeContractErrorV1::InvalidConfiguration)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticActivationRequestV1 {
    pub target_generation: VectorGenerationIdV1,
    pub expected_active_generation: Option<VectorGenerationIdV1>,
    pub expected_rollback_generation: Option<VectorGenerationIdV1>,
}

impl SemanticActivationRequestV1 {
    pub fn new(
        target_generation: VectorGenerationIdV1,
        expected_active_generation: Option<VectorGenerationIdV1>,
        expected_rollback_generation: Option<VectorGenerationIdV1>,
    ) -> Result<Self, SemanticRuntimeContractErrorV1> {
        let request = Self {
            target_generation,
            expected_active_generation,
            expected_rollback_generation,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<(), SemanticRuntimeContractErrorV1> {
        validate_generation(&self.target_generation)?;
        validate_optional_generation(self.expected_active_generation.as_ref())?;
        validate_optional_generation(self.expected_rollback_generation.as_ref())?;
        if self.expected_active_generation.as_ref() == Some(&self.target_generation) {
            return Err(SemanticRuntimeContractErrorV1::InvalidActivation);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticActivationCommandV1 {
    pub configuration: SemanticConfigurationPinV1,
    pub request: SemanticActivationRequestV1,
}

impl SemanticActivationCommandV1 {
    pub fn new(
        configuration: SemanticConfigurationPinV1,
        request: SemanticActivationRequestV1,
    ) -> Result<Self, SemanticRuntimeContractErrorV1> {
        configuration.validate()?;
        request.validate()?;
        Ok(Self {
            configuration,
            request,
        })
    }
}

/// Explicit proof that the semantic active/rollback pointer swap completed.
/// A staged or indexing generation has no receipt and therefore cannot route
/// semantic queries.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticActivationReceiptV1 {
    pub configuration: SemanticConfigurationPinV1,
    pub activated_generation: VectorGenerationIdV1,
    pub previous_active_generation: Option<VectorGenerationIdV1>,
    pub previous_rollback_generation: Option<VectorGenerationIdV1>,
    pub rollback_generation: Option<VectorGenerationIdV1>,
    pub activated_at: UtcMicros,
    pub receipt_digest: ManifestDigest,
}

impl SemanticActivationReceiptV1 {
    pub fn issue(
        command: &SemanticActivationCommandV1,
        activated_at: UtcMicros,
    ) -> Result<Self, SemanticRuntimeContractErrorV1> {
        let receipt_digest = activation_receipt_digest(
            &command.configuration,
            &command.request.target_generation,
            command.request.expected_active_generation.as_ref(),
            command.request.expected_rollback_generation.as_ref(),
            command.request.expected_active_generation.as_ref(),
            activated_at,
        )?;
        let receipt = Self {
            configuration: command.configuration.clone(),
            activated_generation: command.request.target_generation.clone(),
            previous_active_generation: command.request.expected_active_generation.clone(),
            previous_rollback_generation: command.request.expected_rollback_generation.clone(),
            rollback_generation: command.request.expected_active_generation.clone(),
            activated_at,
            receipt_digest,
        };
        receipt.validate_for(command)?;
        Ok(receipt)
    }

    pub fn validate(&self) -> Result<(), SemanticRuntimeContractErrorV1> {
        self.configuration.validate()?;
        validate_generation(&self.activated_generation)?;
        validate_optional_generation(self.previous_active_generation.as_ref())?;
        validate_optional_generation(self.previous_rollback_generation.as_ref())?;
        validate_optional_generation(self.rollback_generation.as_ref())?;
        if self.previous_active_generation.as_ref() == Some(&self.activated_generation) {
            return Err(SemanticRuntimeContractErrorV1::InvalidActivation);
        }
        if self.rollback_generation != self.previous_active_generation {
            return Err(SemanticRuntimeContractErrorV1::ReceiptIdentityMismatch);
        }
        if self.compute_digest()? != self.receipt_digest {
            return Err(SemanticRuntimeContractErrorV1::ReceiptIdentityMismatch);
        }
        Ok(())
    }

    pub fn validate_for(
        &self,
        command: &SemanticActivationCommandV1,
    ) -> Result<(), SemanticRuntimeContractErrorV1> {
        self.validate()?;
        command.configuration.validate()?;
        command.request.validate()?;
        if self.configuration != command.configuration
            || self.activated_generation != command.request.target_generation
            || self.previous_active_generation != command.request.expected_active_generation
            || self.previous_rollback_generation != command.request.expected_rollback_generation
        {
            return Err(SemanticRuntimeContractErrorV1::ReceiptIdentityMismatch);
        }
        Ok(())
    }

    fn compute_digest(&self) -> Result<ManifestDigest, SemanticRuntimeContractErrorV1> {
        activation_receipt_digest(
            &self.configuration,
            &self.activated_generation,
            self.previous_active_generation.as_ref(),
            self.previous_rollback_generation.as_ref(),
            self.rollback_generation.as_ref(),
            self.activated_at,
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticRollbackRequestV1 {
    pub target_generation: VectorGenerationIdV1,
    pub expected_active_generation: VectorGenerationIdV1,
    pub expected_rollback_generation: VectorGenerationIdV1,
}

impl SemanticRollbackRequestV1 {
    pub fn new(
        target_generation: VectorGenerationIdV1,
        expected_active_generation: VectorGenerationIdV1,
        expected_rollback_generation: VectorGenerationIdV1,
    ) -> Result<Self, SemanticRuntimeContractErrorV1> {
        let request = Self {
            target_generation,
            expected_active_generation,
            expected_rollback_generation,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<(), SemanticRuntimeContractErrorV1> {
        validate_generation(&self.target_generation)?;
        validate_generation(&self.expected_active_generation)?;
        validate_generation(&self.expected_rollback_generation)?;
        if self.target_generation != self.expected_rollback_generation
            || self.target_generation == self.expected_active_generation
        {
            return Err(SemanticRuntimeContractErrorV1::InvalidRollback);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticRollbackCommandV1 {
    pub configuration: SemanticConfigurationPinV1,
    pub request: SemanticRollbackRequestV1,
}

impl SemanticRollbackCommandV1 {
    pub fn new(
        configuration: SemanticConfigurationPinV1,
        request: SemanticRollbackRequestV1,
    ) -> Result<Self, SemanticRuntimeContractErrorV1> {
        configuration.validate()?;
        request.validate()?;
        Ok(Self {
            configuration,
            request,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticRollbackReceiptV1 {
    pub configuration: SemanticConfigurationPinV1,
    pub from_generation: VectorGenerationIdV1,
    pub target_generation: VectorGenerationIdV1,
    pub restored_activation: SemanticActivationReceiptV1,
    pub rolled_back_at: UtcMicros,
    pub receipt_digest: ManifestDigest,
}

impl SemanticRollbackReceiptV1 {
    pub fn issue(
        command: &SemanticRollbackCommandV1,
        rolled_back_at: UtcMicros,
    ) -> Result<Self, SemanticRuntimeContractErrorV1> {
        let activation = SemanticActivationCommandV1::new(
            command.configuration.clone(),
            SemanticActivationRequestV1::new(
                command.request.target_generation.clone(),
                Some(command.request.expected_active_generation.clone()),
                Some(command.request.expected_rollback_generation.clone()),
            )?,
        )?;
        let restored_activation = SemanticActivationReceiptV1::issue(&activation, rolled_back_at)?;
        let receipt_digest = rollback_receipt_digest(
            &command.configuration,
            &command.request.expected_active_generation,
            &command.request.target_generation,
            &restored_activation.receipt_digest,
            rolled_back_at,
        )?;
        let receipt = Self {
            configuration: command.configuration.clone(),
            from_generation: command.request.expected_active_generation.clone(),
            target_generation: command.request.target_generation.clone(),
            restored_activation,
            rolled_back_at,
            receipt_digest,
        };
        receipt.validate_for(command)?;
        Ok(receipt)
    }

    pub fn validate_for(
        &self,
        command: &SemanticRollbackCommandV1,
    ) -> Result<(), SemanticRuntimeContractErrorV1> {
        command.configuration.validate()?;
        command.request.validate()?;
        self.restored_activation.validate()?;
        if self.configuration != command.configuration
            || self.from_generation != command.request.expected_active_generation
            || self.target_generation != command.request.target_generation
            || self.restored_activation.configuration != self.configuration
            || self.restored_activation.activated_generation != self.target_generation
            || self.restored_activation.previous_active_generation
                != Some(self.from_generation.clone())
            || self.restored_activation.previous_rollback_generation
                != Some(command.request.expected_rollback_generation.clone())
            || self.restored_activation.rollback_generation != Some(self.from_generation.clone())
            || self.compute_digest()? != self.receipt_digest
        {
            return Err(SemanticRuntimeContractErrorV1::ReceiptIdentityMismatch);
        }
        Ok(())
    }

    fn compute_digest(&self) -> Result<ManifestDigest, SemanticRuntimeContractErrorV1> {
        rollback_receipt_digest(
            &self.configuration,
            &self.from_generation,
            &self.target_generation,
            &self.restored_activation.receipt_digest,
            self.rolled_back_at,
        )
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticFallbackReasonV1 {
    ConfigurationUnavailable,
    RuntimeUnavailable,
    ArtifactUnavailable,
    IncompatibleRuntime,
    ResourceCeilingExceeded,
    CorruptArtifact,
    Indexing,
    RuntimeFailure,
    RollbackInProgress,
    InvalidRuntimeStatus,
    /// Selected catalog model has not been downloaded yet.
    SelectedNotDownloaded,
    /// Daemon-owned model acquisition is in progress.
    Downloading,
    /// Downloaded bytes are being verified against catalog pins.
    Verifying,
    /// Model is installed but not yet loaded into the runtime.
    Loading,
    /// Model acquisition or load failed; exact/lexical/graph remain available.
    ModelFailed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SemanticRuntimeStateV1 {
    Unavailable {
        reason: SemanticFallbackReasonV1,
    },
    /// Catalog model selected but local install bytes are absent.
    SelectedNotDownloaded {
        model_id: String,
        artifact_digest: String,
    },
    /// Background download of catalog members is in progress.
    Downloading {
        model_id: String,
        artifact_digest: String,
        bytes_received: u64,
        bytes_total: u64,
    },
    /// Downloaded members are being length/SHA-256 verified.
    Verifying {
        model_id: String,
        artifact_digest: String,
    },
    /// Verified package is installed locally but not yet loaded.
    Installed {
        model_id: String,
        artifact_digest: String,
    },
    /// Installed model is loading into the embedding runtime.
    Loading {
        model_id: String,
        artifact_digest: String,
    },
    Indexing {
        target_generation: VectorGenerationIdV1,
        completed_units: u64,
        total_units: u64,
    },
    /// Atomically current semantic generation (Doctor/status: Ready).
    #[serde(rename = "ready")]
    Current {
        receipt: SemanticActivationReceiptV1,
    },
    Degraded {
        active_generation: Option<VectorGenerationIdV1>,
        reason: SemanticFallbackReasonV1,
    },
    Rollback {
        from_generation: VectorGenerationIdV1,
        target_generation: VectorGenerationIdV1,
    },
    Failed {
        model_id: String,
        artifact_digest: String,
        detail: String,
        retryable: bool,
    },
}

impl SemanticRuntimeStateV1 {
    fn validate_for(
        &self,
        configuration: Option<&SemanticConfigurationPinV1>,
    ) -> Result<(), SemanticRuntimeContractErrorV1> {
        match self {
            Self::Unavailable { .. } => Ok(()),
            Self::SelectedNotDownloaded {
                model_id,
                artifact_digest,
            }
            | Self::Verifying {
                model_id,
                artifact_digest,
            }
            | Self::Installed {
                model_id,
                artifact_digest,
            }
            | Self::Loading {
                model_id,
                artifact_digest,
            } => {
                // Acquisition states are valid before a configuration pin exists
                // so offline Doctor/status can report SelectedNotDownloaded.
                validate_model_identity(model_id, artifact_digest)?;
                let _ = configuration;
                Ok(())
            }
            Self::Downloading {
                model_id,
                artifact_digest,
                bytes_received,
                bytes_total,
            } => {
                validate_model_identity(model_id, artifact_digest)?;
                if *bytes_total == 0 || bytes_received > bytes_total {
                    return Err(SemanticRuntimeContractErrorV1::InvalidProgress);
                }
                let _ = configuration;
                Ok(())
            }
            Self::Indexing {
                target_generation,
                completed_units,
                total_units,
            } => {
                validate_generation(target_generation)?;
                if *total_units == 0 || completed_units > total_units {
                    return Err(SemanticRuntimeContractErrorV1::InvalidProgress);
                }
                require_configuration(configuration)?;
                Ok(())
            }
            Self::Current { receipt } => {
                receipt.validate()?;
                let configuration = require_configuration(configuration)?;
                if receipt.configuration != *configuration {
                    return Err(SemanticRuntimeContractErrorV1::ReceiptIdentityMismatch);
                }
                Ok(())
            }
            Self::Degraded {
                active_generation, ..
            } => {
                validate_optional_generation(active_generation.as_ref())?;
                require_configuration(configuration)?;
                Ok(())
            }
            Self::Rollback {
                from_generation,
                target_generation,
            } => {
                validate_generation(from_generation)?;
                validate_generation(target_generation)?;
                if from_generation == target_generation {
                    return Err(SemanticRuntimeContractErrorV1::InvalidRollback);
                }
                require_configuration(configuration)?;
                Ok(())
            }
            Self::Failed {
                model_id,
                artifact_digest,
                detail,
                ..
            } => {
                validate_model_identity(model_id, artifact_digest)?;
                if detail.trim().is_empty() {
                    return Err(SemanticRuntimeContractErrorV1::InvalidRuntimeStatus);
                }
                let _ = configuration;
                Ok(())
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "route", rename_all = "snake_case")]
pub enum SemanticRuntimeRouteV1 {
    Semantic {
        generation: VectorGenerationIdV1,
        activation_receipt_digest: ManifestDigest,
    },
    LexicalFallback {
        reason: SemanticFallbackReasonV1,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticRuntimeStatusV1 {
    pub configuration: Option<SemanticConfigurationPinV1>,
    pub state: SemanticRuntimeStateV1,
}

impl SemanticRuntimeStatusV1 {
    pub fn new(
        configuration: Option<SemanticConfigurationPinV1>,
        state: SemanticRuntimeStateV1,
    ) -> Self {
        Self {
            configuration,
            state,
        }
    }

    pub fn validate(&self) -> Result<(), SemanticRuntimeContractErrorV1> {
        self.state.validate_for(self.configuration.as_ref())
    }

    pub fn route(&self) -> SemanticRuntimeRouteV1 {
        if self.validate().is_err() {
            return SemanticRuntimeRouteV1::LexicalFallback {
                reason: SemanticFallbackReasonV1::InvalidRuntimeStatus,
            };
        }
        match &self.state {
            SemanticRuntimeStateV1::Current { receipt } => SemanticRuntimeRouteV1::Semantic {
                generation: receipt.activated_generation.clone(),
                activation_receipt_digest: receipt.receipt_digest.clone(),
            },
            SemanticRuntimeStateV1::Unavailable { reason }
            | SemanticRuntimeStateV1::Degraded { reason, .. } => {
                SemanticRuntimeRouteV1::LexicalFallback { reason: *reason }
            }
            SemanticRuntimeStateV1::SelectedNotDownloaded { .. } => {
                SemanticRuntimeRouteV1::LexicalFallback {
                    reason: SemanticFallbackReasonV1::SelectedNotDownloaded,
                }
            }
            SemanticRuntimeStateV1::Downloading { .. } => SemanticRuntimeRouteV1::LexicalFallback {
                reason: SemanticFallbackReasonV1::Downloading,
            },
            SemanticRuntimeStateV1::Verifying { .. } => SemanticRuntimeRouteV1::LexicalFallback {
                reason: SemanticFallbackReasonV1::Verifying,
            },
            SemanticRuntimeStateV1::Installed { .. } | SemanticRuntimeStateV1::Loading { .. } => {
                SemanticRuntimeRouteV1::LexicalFallback {
                    reason: SemanticFallbackReasonV1::Loading,
                }
            }
            SemanticRuntimeStateV1::Indexing { .. } => SemanticRuntimeRouteV1::LexicalFallback {
                reason: SemanticFallbackReasonV1::Indexing,
            },
            SemanticRuntimeStateV1::Rollback { .. } => SemanticRuntimeRouteV1::LexicalFallback {
                reason: SemanticFallbackReasonV1::RollbackInProgress,
            },
            SemanticRuntimeStateV1::Failed { .. } => SemanticRuntimeRouteV1::LexicalFallback {
                reason: SemanticFallbackReasonV1::ModelFailed,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum SemanticRuntimeBackendErrorV1 {
    #[error("semantic runtime unavailable")]
    Unavailable,
    #[error("semantic runtime rejected the transition")]
    Rejected,
    #[error("semantic runtime compare-and-swap conflict")]
    Conflict,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum SemanticRuntimeContractErrorV1 {
    #[error("invalid configuration snapshot")]
    InvalidConfiguration,
    #[error("invalid semantic generation")]
    InvalidGeneration,
    #[error("invalid semantic indexing progress")]
    InvalidProgress,
    #[error("invalid semantic activation")]
    InvalidActivation,
    #[error("invalid semantic rollback")]
    InvalidRollback,
    #[error("semantic receipt identity mismatch")]
    ReceiptIdentityMismatch,
    #[error("invalid semantic runtime status")]
    InvalidRuntimeStatus,
    #[error("invalid semantic model identity")]
    InvalidModelIdentity,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum SemanticRuntimeControlErrorV1 {
    #[error("configuration snapshot unavailable")]
    ConfigurationUnavailable,
    #[error("semantic runtime unavailable")]
    RuntimeUnavailable,
    #[error("semantic transition request is invalid")]
    InvalidRequest,
    #[error("semantic runtime rejected the transition")]
    Rejected,
    #[error("semantic runtime compare-and-swap conflict")]
    Conflict,
    #[error("semantic runtime returned an invalid receipt")]
    InvalidReceipt,
    #[error("semantic activation receipt was not observed as current")]
    PromotionNotObserved,
}

pub trait SemanticRuntimeBackendV1: Sync {
    fn status<'a>(
        &'a self,
        configuration: &'a SemanticConfigurationPinV1,
    ) -> SemanticRuntimeFuture<'a, Result<SemanticRuntimeStateV1, SemanticRuntimeBackendErrorV1>>;

    fn activate<'a>(
        &'a self,
        command: &'a SemanticActivationCommandV1,
    ) -> SemanticRuntimeFuture<'a, Result<SemanticActivationReceiptV1, SemanticRuntimeBackendErrorV1>>;

    fn rollback<'a>(
        &'a self,
        command: &'a SemanticRollbackCommandV1,
    ) -> SemanticRuntimeFuture<'a, Result<SemanticRollbackReceiptV1, SemanticRuntimeBackendErrorV1>>;
}

/// Mount point for central configuration and Doctor integration. Implementors
/// expose only application semantics; persistence, artifact verification, and
/// pointer CAS remain owned by the semantic runtime backend.
pub trait SemanticRuntimeIntegrationPortV1: Sync {
    fn status(&self) -> SemanticRuntimeFuture<'_, SemanticRuntimeStatusV1>;

    fn activate(
        &self,
        request: SemanticActivationRequestV1,
    ) -> SemanticRuntimeFuture<'_, Result<SemanticActivationReceiptV1, SemanticRuntimeControlErrorV1>>;

    fn rollback(
        &self,
        request: SemanticRollbackRequestV1,
    ) -> SemanticRuntimeFuture<'_, Result<SemanticRollbackReceiptV1, SemanticRuntimeControlErrorV1>>;
}

fn validate_generation(
    generation: &VectorGenerationIdV1,
) -> Result<(), SemanticRuntimeContractErrorV1> {
    generation
        .validate()
        .map_err(|_| SemanticRuntimeContractErrorV1::InvalidGeneration)
}

fn validate_model_identity(
    model_id: &str,
    artifact_digest: &str,
) -> Result<(), SemanticRuntimeContractErrorV1> {
    if model_id.trim().is_empty() || model_id.len() > 128 {
        return Err(SemanticRuntimeContractErrorV1::InvalidModelIdentity);
    }
    if artifact_digest.len() != 64
        || !artifact_digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(SemanticRuntimeContractErrorV1::InvalidModelIdentity);
    }
    Ok(())
}

fn validate_optional_generation(
    generation: Option<&VectorGenerationIdV1>,
) -> Result<(), SemanticRuntimeContractErrorV1> {
    generation.map_or(Ok(()), validate_generation)
}

fn require_configuration(
    configuration: Option<&SemanticConfigurationPinV1>,
) -> Result<&SemanticConfigurationPinV1, SemanticRuntimeContractErrorV1> {
    let configuration =
        configuration.ok_or(SemanticRuntimeContractErrorV1::InvalidConfiguration)?;
    configuration.validate()?;
    Ok(configuration)
}

fn activation_receipt_digest(
    configuration: &SemanticConfigurationPinV1,
    activated_generation: &VectorGenerationIdV1,
    previous_active_generation: Option<&VectorGenerationIdV1>,
    previous_rollback_generation: Option<&VectorGenerationIdV1>,
    rollback_generation: Option<&VectorGenerationIdV1>,
    activated_at: UtcMicros,
) -> Result<ManifestDigest, SemanticRuntimeContractErrorV1> {
    canonical_sha256(&(
        SEMANTIC_ACTIVATION_RECEIPT_DIGEST_DOMAIN_V1,
        configuration,
        activated_generation,
        previous_active_generation,
        previous_rollback_generation,
        rollback_generation,
        activated_at,
    ))
    .map_err(|_| SemanticRuntimeContractErrorV1::ReceiptIdentityMismatch)
}

fn rollback_receipt_digest(
    configuration: &SemanticConfigurationPinV1,
    from_generation: &VectorGenerationIdV1,
    target_generation: &VectorGenerationIdV1,
    restored_activation_receipt_digest: &ManifestDigest,
    rolled_back_at: UtcMicros,
) -> Result<ManifestDigest, SemanticRuntimeContractErrorV1> {
    canonical_sha256(&(
        SEMANTIC_ROLLBACK_RECEIPT_DIGEST_DOMAIN_V1,
        configuration,
        from_generation,
        target_generation,
        restored_activation_receipt_digest,
        rolled_back_at,
    ))
    .map_err(|_| SemanticRuntimeContractErrorV1::ReceiptIdentityMismatch)
}

#[cfg(test)]
mod validate_contract_tests {
    use tracedecay_domain::configuration::{ConfigurationRevisionId, ConfigurationSnapshotV1};
    use tracedecay_domain::{ManifestDigest, VectorGenerationIdV1};

    use super::*;
    use crate::application::configuration::ConfigurationCurrentStateV1;

    fn pin() -> SemanticConfigurationPinV1 {
        SemanticConfigurationPinV1::from_current(&ConfigurationCurrentStateV1 {
            revision_id: ConfigurationRevisionId::try_from("configuration.revision.1".to_owned())
                .unwrap(),
            snapshot: ConfigurationSnapshotV1::new(Default::default(), Default::default()).unwrap(),
        })
        .unwrap()
    }

    fn generation(byte: char) -> VectorGenerationIdV1 {
        VectorGenerationIdV1::new(
            ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap(),
        )
    }

    #[test]
    fn indexing_degraded_and_rollback_require_configuration_pin() {
        for state in [
            SemanticRuntimeStateV1::Indexing {
                target_generation: generation('a'),
                completed_units: 1,
                total_units: 2,
            },
            SemanticRuntimeStateV1::Degraded {
                active_generation: Some(generation('a')),
                reason: SemanticFallbackReasonV1::RuntimeFailure,
            },
            SemanticRuntimeStateV1::Rollback {
                from_generation: generation('a'),
                target_generation: generation('b'),
            },
        ] {
            let missing = SemanticRuntimeStatusV1::new(None, state.clone());
            assert_eq!(
                missing.validate(),
                Err(SemanticRuntimeContractErrorV1::InvalidConfiguration)
            );
            let present = SemanticRuntimeStatusV1::new(Some(pin()), state);
            assert_eq!(present.validate(), Ok(()));
        }
    }

    #[test]
    fn unavailable_allows_missing_configuration() {
        let status = SemanticRuntimeStatusV1::new(
            None,
            SemanticRuntimeStateV1::Unavailable {
                reason: SemanticFallbackReasonV1::RuntimeUnavailable,
            },
        );
        assert_eq!(status.validate(), Ok(()));
    }

    #[test]
    fn indexing_rejects_invalid_progress_before_configuration() {
        let status = SemanticRuntimeStatusV1::new(
            Some(pin()),
            SemanticRuntimeStateV1::Indexing {
                target_generation: generation('a'),
                completed_units: 3,
                total_units: 2,
            },
        );
        assert_eq!(
            status.validate(),
            Err(SemanticRuntimeContractErrorV1::InvalidProgress)
        );
    }
}
