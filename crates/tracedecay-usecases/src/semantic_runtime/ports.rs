use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_application::ResolvedScope;
use tracedecay_domain::configuration::{ConfigurationRevisionId, ConfigurationSnapshotId};
use tracedecay_domain::{
    FusionProfileId, ManifestDigest, RetrievalAnchorId, UtcMicros, VectorGenerationIdV1,
    canonical_sha256,
};

use crate::config::retrieval::{
    AcceptedRetrievalProfileV1, RetrievalProfileAuditEventV1, RetrievalProfileAuditOperationV1,
    RetrievalProfileCasV1, RetrievalProfileStateV1, RetrievalRuntimeCompatibilityV1,
    SemanticCompatibilityPinsV1, SemanticResourceRequirementV1,
};
use crate::configuration::{
    ConfigurationControlStore, ConfigurationCurrentStateV1, ConfigurationOperationFuture,
};

const SEMANTIC_ACTIVATION_RECEIPT_DIGEST_DOMAIN_V1: &str =
    "tracedecay.semantic-activation-receipt.v1";
const SEMANTIC_ROLLBACK_RECEIPT_DIGEST_DOMAIN_V1: &str = "tracedecay.semantic-rollback-receipt.v1";
const SEMANTIC_CONFIGURATION_TRANSITION_DIGEST_DOMAIN_V1: &str =
    "tracedecay.semantic-configuration-transition.v1";
const SEMANTIC_EXECUTABLE_GENERATION_DIGEST_DOMAIN_V1: &str =
    "tracedecay.semantic-executable-generation.v1";
const RETRIEVAL_PROFILE_AUDIT_DIGEST_DOMAIN_V1: &str = "tracedecay.retrieval.profile-audit.v1";

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
    pub previous_configuration: SemanticConfigurationPinV1,
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
        Self::issue_transition(command, command.configuration.clone(), activated_at)
    }

    pub fn issue_transition(
        command: &SemanticActivationCommandV1,
        configuration: SemanticConfigurationPinV1,
        activated_at: UtcMicros,
    ) -> Result<Self, SemanticRuntimeContractErrorV1> {
        configuration.validate()?;
        let receipt_digest = activation_receipt_digest(
            &command.configuration,
            &configuration,
            &command.request.target_generation,
            command.request.expected_active_generation.as_ref(),
            command.request.expected_rollback_generation.as_ref(),
            command.request.expected_active_generation.as_ref(),
            activated_at,
        )?;
        let receipt = Self {
            previous_configuration: command.configuration.clone(),
            configuration,
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
        self.previous_configuration.validate()?;
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
        if self.activated_generation != command.request.target_generation
            || self.previous_active_generation != command.request.expected_active_generation
            || self.previous_rollback_generation != command.request.expected_rollback_generation
        {
            return Err(SemanticRuntimeContractErrorV1::ReceiptIdentityMismatch);
        }
        if self.previous_configuration != command.configuration {
            return Err(SemanticRuntimeContractErrorV1::ReceiptIdentityMismatch);
        }
        Ok(())
    }

    fn compute_digest(&self) -> Result<ManifestDigest, SemanticRuntimeContractErrorV1> {
        activation_receipt_digest(
            &self.previous_configuration,
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
    pub target_generation: Option<VectorGenerationIdV1>,
    pub expected_active_generation: VectorGenerationIdV1,
    pub expected_rollback_generation: Option<VectorGenerationIdV1>,
}

impl SemanticRollbackRequestV1 {
    pub fn new(
        target_generation: VectorGenerationIdV1,
        expected_active_generation: VectorGenerationIdV1,
        expected_rollback_generation: VectorGenerationIdV1,
    ) -> Result<Self, SemanticRuntimeContractErrorV1> {
        let request = Self {
            target_generation: Some(target_generation),
            expected_active_generation,
            expected_rollback_generation: Some(expected_rollback_generation),
        };
        request.validate()?;
        Ok(request)
    }

    pub fn disable(
        expected_active_generation: VectorGenerationIdV1,
    ) -> Result<Self, SemanticRuntimeContractErrorV1> {
        let request = Self {
            target_generation: None,
            expected_active_generation,
            expected_rollback_generation: None,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<(), SemanticRuntimeContractErrorV1> {
        validate_generation(&self.expected_active_generation)?;
        validate_optional_generation(self.target_generation.as_ref())?;
        validate_optional_generation(self.expected_rollback_generation.as_ref())?;
        if self.target_generation != self.expected_rollback_generation
            || self.target_generation.as_ref() == Some(&self.expected_active_generation)
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
    pub previous_configuration: SemanticConfigurationPinV1,
    pub configuration: SemanticConfigurationPinV1,
    pub from_generation: VectorGenerationIdV1,
    pub target_generation: Option<VectorGenerationIdV1>,
    pub restored_activation: Option<SemanticActivationReceiptV1>,
    pub rolled_back_at: UtcMicros,
    pub receipt_digest: ManifestDigest,
}

impl SemanticRollbackReceiptV1 {
    pub fn issue(
        command: &SemanticRollbackCommandV1,
        rolled_back_at: UtcMicros,
    ) -> Result<Self, SemanticRuntimeContractErrorV1> {
        Self::issue_transition(command, command.configuration.clone(), rolled_back_at)
    }

    pub fn issue_transition(
        command: &SemanticRollbackCommandV1,
        configuration: SemanticConfigurationPinV1,
        rolled_back_at: UtcMicros,
    ) -> Result<Self, SemanticRuntimeContractErrorV1> {
        configuration.validate()?;
        let restored_activation = match command.request.target_generation.as_ref() {
            Some(target) => {
                let activation = SemanticActivationCommandV1::new(
                    command.configuration.clone(),
                    SemanticActivationRequestV1::new(
                        target.clone(),
                        Some(command.request.expected_active_generation.clone()),
                        command.request.expected_rollback_generation.clone(),
                    )?,
                )?;
                Some(SemanticActivationReceiptV1::issue_transition(
                    &activation,
                    configuration.clone(),
                    rolled_back_at,
                )?)
            }
            None => None,
        };
        let receipt_digest = rollback_receipt_digest(
            &command.configuration,
            &configuration,
            &command.request.expected_active_generation,
            command.request.target_generation.as_ref(),
            restored_activation
                .as_ref()
                .map(|receipt| &receipt.receipt_digest),
            rolled_back_at,
        )?;
        let receipt = Self {
            previous_configuration: command.configuration.clone(),
            configuration,
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
        self.previous_configuration.validate()?;
        self.configuration.validate()?;
        command.configuration.validate()?;
        command.request.validate()?;
        if let Some(restored) = &self.restored_activation {
            restored.validate()?;
        }
        if self.previous_configuration != command.configuration
            || self.from_generation != command.request.expected_active_generation
            || self.target_generation != command.request.target_generation
            || self.compute_digest()? != self.receipt_digest
        {
            return Err(SemanticRuntimeContractErrorV1::ReceiptIdentityMismatch);
        }
        match (&self.target_generation, &self.restored_activation) {
            (Some(target), Some(restored))
                if restored.configuration == self.configuration
                    && restored.previous_configuration == self.previous_configuration
                    && &restored.activated_generation == target
                    && restored.previous_active_generation
                        == Some(self.from_generation.clone())
                    && restored.previous_rollback_generation
                        == command.request.expected_rollback_generation
                    && restored.rollback_generation == Some(self.from_generation.clone()) => {}
            (None, None) => {}
            _ => return Err(SemanticRuntimeContractErrorV1::ReceiptIdentityMismatch),
        }
        Ok(())
    }

    fn compute_digest(&self) -> Result<ManifestDigest, SemanticRuntimeContractErrorV1> {
        rollback_receipt_digest(
            &self.previous_configuration,
            &self.configuration,
            &self.from_generation,
            self.target_generation.as_ref(),
            self.restored_activation
                .as_ref()
                .map(|receipt| &receipt.receipt_digest),
            self.rolled_back_at,
        )
    }
}

/// Why the semantic lane is unavailable or degraded. Defined by the semantic
/// runtime crate, which produces the reasons this projection reports.
pub use tracedecay_semantic::SemanticFallbackReasonV1;

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
    #[error("semantic configuration transition is invalid")]
    InvalidTransition,
    #[error("semantic artifact, projection, generation, or runtime is incompatible")]
    InvalidCompatibility,
    #[error("semantic resource ceiling is below the evaluated requirement")]
    ResourceCeilingExceeded,
    #[error("semantic rollback target is not cold-offline executable")]
    RollbackNotExecutable,
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

/// PASS-gated configuration transition prepared under the retrieval-profile
/// CAS authority. The configuration owner is the only producer: the semantic
/// backend can validate and commit this value but cannot invent profiles,
/// evaluation anchors, calibration, or configuration revisions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SemanticConfigurationTransitionV1 {
    pub operation: RetrievalProfileAuditOperationV1,
    pub base_configuration: SemanticConfigurationPinV1,
    pub result_configuration: SemanticConfigurationPinV1,
    pub prior_active_profile_id: FusionProfileId,
    pub result_active_profile_id: FusionProfileId,
    pub prior_active_profile_digest: ManifestDigest,
    pub result_active_profile_digest: ManifestDigest,
    pub evaluation_anchor: RetrievalAnchorId,
    pub expected_cas: RetrievalProfileCasV1,
    pub prior_active_semantic: Option<SemanticCompatibilityPinsV1>,
    pub prior_rollback_semantic: Option<SemanticCompatibilityPinsV1>,
    pub result_active_semantic: Option<SemanticCompatibilityPinsV1>,
    pub result_rollback_semantic: Option<SemanticCompatibilityPinsV1>,
    pub transition_at: UtcMicros,
    pub transition_digest: ManifestDigest,
}

impl<'de> Deserialize<'de> for SemanticConfigurationTransitionV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            operation: RetrievalProfileAuditOperationV1,
            base_configuration: SemanticConfigurationPinV1,
            result_configuration: SemanticConfigurationPinV1,
            prior_active_profile_id: FusionProfileId,
            result_active_profile_id: FusionProfileId,
            prior_active_profile_digest: ManifestDigest,
            result_active_profile_digest: ManifestDigest,
            evaluation_anchor: RetrievalAnchorId,
            expected_cas: RetrievalProfileCasV1,
            prior_active_semantic: Option<SemanticCompatibilityPinsV1>,
            prior_rollback_semantic: Option<SemanticCompatibilityPinsV1>,
            result_active_semantic: Option<SemanticCompatibilityPinsV1>,
            result_rollback_semantic: Option<SemanticCompatibilityPinsV1>,
            transition_at: UtcMicros,
            transition_digest: ManifestDigest,
        }

        let raw = Raw::deserialize(deserializer)?;
        let transition = Self {
            operation: raw.operation,
            base_configuration: raw.base_configuration,
            result_configuration: raw.result_configuration,
            prior_active_profile_id: raw.prior_active_profile_id,
            result_active_profile_id: raw.result_active_profile_id,
            prior_active_profile_digest: raw.prior_active_profile_digest,
            result_active_profile_digest: raw.result_active_profile_digest,
            evaluation_anchor: raw.evaluation_anchor,
            expected_cas: raw.expected_cas,
            prior_active_semantic: raw.prior_active_semantic,
            prior_rollback_semantic: raw.prior_rollback_semantic,
            result_active_semantic: raw.result_active_semantic,
            result_rollback_semantic: raw.result_rollback_semantic,
            transition_at: raw.transition_at,
            transition_digest: raw.transition_digest,
        };
        transition.validate().map_err(serde::de::Error::custom)?;
        Ok(transition)
    }
}

impl SemanticConfigurationTransitionV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn activation(
        base_configuration: SemanticConfigurationPinV1,
        result_configuration: SemanticConfigurationPinV1,
        prior_active_profile_id: FusionProfileId,
        accepted: &AcceptedRetrievalProfileV1,
        accepted_runtime: &RetrievalRuntimeCompatibilityV1,
        expected_cas: RetrievalProfileCasV1,
        prior_active_semantic: Option<SemanticCompatibilityPinsV1>,
        prior_rollback_semantic: Option<SemanticCompatibilityPinsV1>,
        transition_at: UtcMicros,
    ) -> Result<Self, SemanticRuntimeContractErrorV1> {
        if accepted_runtime.semantic.is_none() {
            return Err(SemanticRuntimeContractErrorV1::InvalidCompatibility);
        }
        Self::new(
            RetrievalProfileAuditOperationV1::Activate,
            base_configuration,
            result_configuration,
            prior_active_profile_id,
            accepted,
            accepted_runtime,
            expected_cas,
            prior_active_semantic,
            prior_rollback_semantic,
            transition_at,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn rollback(
        base_configuration: SemanticConfigurationPinV1,
        result_configuration: SemanticConfigurationPinV1,
        prior_active_profile_id: FusionProfileId,
        restored: &AcceptedRetrievalProfileV1,
        restored_runtime: &RetrievalRuntimeCompatibilityV1,
        expected_cas: RetrievalProfileCasV1,
        prior_active_semantic: SemanticCompatibilityPinsV1,
        prior_rollback_semantic: Option<SemanticCompatibilityPinsV1>,
        trigger: String,
        transition_at: UtcMicros,
    ) -> Result<Self, SemanticRuntimeContractErrorV1> {
        if trigger.trim().is_empty()
            || trigger.trim() != trigger
            || trigger.chars().any(char::is_control)
        {
            return Err(SemanticRuntimeContractErrorV1::InvalidRollback);
        }
        Self::new(
            RetrievalProfileAuditOperationV1::Rollback { trigger },
            base_configuration,
            result_configuration,
            prior_active_profile_id,
            restored,
            restored_runtime,
            expected_cas,
            Some(prior_active_semantic),
            prior_rollback_semantic,
            transition_at,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new(
        operation: RetrievalProfileAuditOperationV1,
        base_configuration: SemanticConfigurationPinV1,
        result_configuration: SemanticConfigurationPinV1,
        prior_active_profile_id: FusionProfileId,
        accepted: &AcceptedRetrievalProfileV1,
        accepted_runtime: &RetrievalRuntimeCompatibilityV1,
        expected_cas: RetrievalProfileCasV1,
        prior_active_semantic: Option<SemanticCompatibilityPinsV1>,
        prior_rollback_semantic: Option<SemanticCompatibilityPinsV1>,
        transition_at: UtcMicros,
    ) -> Result<Self, SemanticRuntimeContractErrorV1> {
        let result_active_semantic = accepted_runtime.semantic.clone();
        if accepted.compatibility().semantic != result_active_semantic
            || !(match (&result_active_semantic, accepted_runtime.semantic_ceiling) {
                (Some(required), Some(ceiling)) => resources_covered(required.resources, ceiling),
                (None, None) => true,
                _ => false,
            })
        {
            return Err(SemanticRuntimeContractErrorV1::InvalidCompatibility);
        }
        let result_active_profile_id = accepted.profile().profile_id.clone();
        let prior_active_profile_digest = expected_cas.expected_active_digest.clone();
        let result_active_profile_digest = accepted.profile_digest().clone();
        let evaluation_anchor = accepted.profile().evaluation_result_anchor.clone();
        let result_rollback_semantic = prior_active_semantic.clone();
        let transition_digest = semantic_configuration_transition_digest(
            &operation,
            &base_configuration,
            &result_configuration,
            &prior_active_profile_id,
            &result_active_profile_id,
            &prior_active_profile_digest,
            &result_active_profile_digest,
            &evaluation_anchor,
            &expected_cas,
            prior_active_semantic.as_ref(),
            prior_rollback_semantic.as_ref(),
            result_active_semantic.as_ref(),
            result_rollback_semantic.as_ref(),
            transition_at,
        )?;
        let transition = Self {
            operation,
            base_configuration,
            result_configuration,
            prior_active_profile_id,
            result_active_profile_id,
            prior_active_profile_digest,
            result_active_profile_digest,
            evaluation_anchor,
            expected_cas,
            prior_active_semantic,
            prior_rollback_semantic,
            result_active_semantic,
            result_rollback_semantic,
            transition_at,
            transition_digest,
        };
        transition.validate_fields()?;
        Ok(transition)
    }

    pub fn validate(&self) -> Result<(), SemanticRuntimeContractErrorV1> {
        self.validate_fields()?;
        if self.compute_digest()? != self.transition_digest {
            return Err(SemanticRuntimeContractErrorV1::InvalidTransition);
        }
        Ok(())
    }

    fn validate_fields(&self) -> Result<(), SemanticRuntimeContractErrorV1> {
        self.base_configuration.validate()?;
        self.result_configuration.validate()?;
        self.prior_active_profile_id
            .validate()
            .map_err(|_| SemanticRuntimeContractErrorV1::InvalidTransition)?;
        self.result_active_profile_id
            .validate()
            .map_err(|_| SemanticRuntimeContractErrorV1::InvalidTransition)?;
        self.prior_active_profile_digest
            .validate()
            .map_err(|_| SemanticRuntimeContractErrorV1::InvalidTransition)?;
        self.result_active_profile_digest
            .validate()
            .map_err(|_| SemanticRuntimeContractErrorV1::InvalidTransition)?;
        self.evaluation_anchor
            .validate()
            .map_err(|_| SemanticRuntimeContractErrorV1::InvalidTransition)?;
        validate_optional_semantic_pins(self.result_active_semantic.as_ref())?;
        validate_optional_semantic_pins(self.prior_active_semantic.as_ref())?;
        validate_optional_semantic_pins(self.prior_rollback_semantic.as_ref())?;
        validate_optional_semantic_pins(self.result_rollback_semantic.as_ref())?;
        if self.base_configuration.revision_id == self.result_configuration.revision_id
            || self.base_configuration.revision_id
                != self.expected_cas.expected_configuration_revision
            || self.prior_active_profile_digest != self.expected_cas.expected_active_digest
            || self.result_rollback_semantic != self.prior_active_semantic
        {
            return Err(SemanticRuntimeContractErrorV1::InvalidTransition);
        }
        if let RetrievalProfileAuditOperationV1::Rollback { trigger } = &self.operation
            && (trigger.trim().is_empty()
                || trigger.trim() != trigger
                || trigger.chars().any(char::is_control))
        {
            return Err(SemanticRuntimeContractErrorV1::InvalidRollback);
        }
        if matches!(self.operation, RetrievalProfileAuditOperationV1::Activate)
            && self.result_active_semantic.is_none()
        {
            return Err(SemanticRuntimeContractErrorV1::InvalidCompatibility);
        }
        if matches!(
            self.operation,
            RetrievalProfileAuditOperationV1::Rollback { .. }
        ) && (self.prior_active_semantic.is_none()
            || self.prior_rollback_semantic != self.result_active_semantic)
        {
            return Err(SemanticRuntimeContractErrorV1::InvalidRollback);
        }
        Ok(())
    }

    fn compute_digest(&self) -> Result<ManifestDigest, SemanticRuntimeContractErrorV1> {
        semantic_configuration_transition_digest(
            &self.operation,
            &self.base_configuration,
            &self.result_configuration,
            &self.prior_active_profile_id,
            &self.result_active_profile_id,
            &self.prior_active_profile_digest,
            &self.result_active_profile_digest,
            &self.evaluation_anchor,
            &self.expected_cas,
            self.prior_active_semantic.as_ref(),
            self.prior_rollback_semantic.as_ref(),
            self.result_active_semantic.as_ref(),
            self.result_rollback_semantic.as_ref(),
            self.transition_at,
        )
    }
}

/// Verified local evidence for one complete immutable semantic generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticExecutableGenerationV1 {
    pub compatibility: SemanticCompatibilityPinsV1,
    pub observed_ceiling: SemanticResourceRequirementV1,
    pub cold_offline_ready: bool,
    pub rollback_executable: bool,
    pub evidence_digest: ManifestDigest,
}

impl SemanticExecutableGenerationV1 {
    pub fn new(
        compatibility: SemanticCompatibilityPinsV1,
        observed_ceiling: SemanticResourceRequirementV1,
        cold_offline_ready: bool,
        rollback_executable: bool,
    ) -> Result<Self, SemanticRuntimeContractErrorV1> {
        let evidence_digest = executable_generation_digest(
            &compatibility,
            observed_ceiling,
            cold_offline_ready,
            rollback_executable,
        )?;
        let evidence = Self {
            compatibility,
            observed_ceiling,
            cold_offline_ready,
            rollback_executable,
            evidence_digest,
        };
        evidence.validate_fields()?;
        Ok(evidence)
    }

    pub fn validate_for(
        &self,
        required: &SemanticCompatibilityPinsV1,
        require_cold_offline_rollback: bool,
    ) -> Result<(), SemanticRuntimeContractErrorV1> {
        self.validate_fields()?;
        if self.compute_digest()? != self.evidence_digest
            || &self.compatibility != required
            || !resources_covered(required.resources, self.observed_ceiling)
        {
            return Err(SemanticRuntimeContractErrorV1::InvalidCompatibility);
        }
        if require_cold_offline_rollback && (!self.cold_offline_ready || !self.rollback_executable)
        {
            return Err(SemanticRuntimeContractErrorV1::RollbackNotExecutable);
        }
        Ok(())
    }

    fn validate_fields(&self) -> Result<(), SemanticRuntimeContractErrorV1> {
        validate_semantic_pins(&self.compatibility)?;
        if !resources_valid(self.observed_ceiling) {
            return Err(SemanticRuntimeContractErrorV1::ResourceCeilingExceeded);
        }
        Ok(())
    }

    fn compute_digest(&self) -> Result<ManifestDigest, SemanticRuntimeContractErrorV1> {
        executable_generation_digest(
            &self.compatibility,
            self.observed_ceiling,
            self.cold_offline_ready,
            self.rollback_executable,
        )
    }
}

/// Durable linkage returned only after the configuration profile CAS, semantic
/// generation pointer, activation receipt, and retrieval audit event commit in
/// one atomic owner transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticLinkedTransitionV1 {
    pub transition_digest: ManifestDigest,
    pub activation_receipt_digest: Option<ManifestDigest>,
    pub audit: RetrievalProfileAuditEventV1,
}

impl SemanticLinkedTransitionV1 {
    pub fn new(
        transition: &SemanticConfigurationTransitionV1,
        receipt: Option<&SemanticActivationReceiptV1>,
        audit: RetrievalProfileAuditEventV1,
    ) -> Result<Self, SemanticRuntimeContractErrorV1> {
        transition.validate()?;
        if let Some(receipt) = receipt {
            receipt.validate()?;
        }
        validate_audit_link(transition, &audit)?;
        match (transition.result_active_semantic.as_ref(), receipt) {
            (Some(semantic), Some(receipt))
                if receipt.previous_configuration == transition.base_configuration
                    && receipt.configuration == transition.result_configuration
                    && receipt.activated_generation == semantic.vector_generation_id => {}
            (None, None) => {}
            _ => return Err(SemanticRuntimeContractErrorV1::InvalidTransition),
        }
        Ok(Self {
            transition_digest: transition.transition_digest.clone(),
            activation_receipt_digest: receipt.map(|receipt| receipt.receipt_digest.clone()),
            audit,
        })
    }

    pub fn validate_for(
        &self,
        transition: &SemanticConfigurationTransitionV1,
        receipt: Option<&SemanticActivationReceiptV1>,
    ) -> Result<(), SemanticRuntimeContractErrorV1> {
        let expected = Self::new(transition, receipt, self.audit.clone())?;
        if &expected != self {
            return Err(SemanticRuntimeContractErrorV1::InvalidTransition);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticCurrentLinkedActivationV1 {
    pub receipt: SemanticActivationReceiptV1,
    pub compatibility: SemanticCompatibilityPinsV1,
}

impl SemanticCurrentLinkedActivationV1 {
    pub fn new(
        receipt: SemanticActivationReceiptV1,
        compatibility: SemanticCompatibilityPinsV1,
    ) -> Result<Self, SemanticRuntimeContractErrorV1> {
        receipt.validate()?;
        validate_semantic_pins(&compatibility)?;
        if receipt.activated_generation != compatibility.vector_generation_id {
            return Err(SemanticRuntimeContractErrorV1::InvalidCompatibility);
        }
        Ok(Self {
            receipt,
            compatibility,
        })
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum SemanticConfigurationBackendErrorV1 {
    #[error("retrieval configuration authority is unavailable")]
    Unavailable,
    #[error("retrieval configuration transition was rejected")]
    Rejected,
    #[error("retrieval configuration compare-and-swap conflicted")]
    Conflict,
}

/// Exact post-commit retrieval state returned by the configuration owner.
///
/// This value may be published only after the linked configuration/profile
/// transaction is durable. It carries the typed scope because paths and
/// mutable labels are never authority.
#[derive(Clone, Debug)]
pub struct CommittedRetrievalProfileStateV1 {
    pub scope: ResolvedScope,
    pub state: RetrievalProfileStateV1,
    pub current_activation: Option<SemanticCurrentLinkedActivationV1>,
}

impl CommittedRetrievalProfileStateV1 {
    pub fn validate_for(
        &self,
        linked: &SemanticLinkedTransitionV1,
    ) -> Result<(), SemanticRuntimeContractErrorV1> {
        self.scope
            .validate()
            .map_err(|_| SemanticRuntimeContractErrorV1::InvalidTransition)?;
        let semantic_binding_valid = match (
            self.state.active().compatibility().semantic.as_ref(),
            self.current_activation.as_ref(),
        ) {
            (Some(required), Some(current)) => {
                &current.compatibility == required
                    && linked.activation_receipt_digest.as_ref()
                        == Some(&current.receipt.receipt_digest)
            }
            (None, None) => linked.activation_receipt_digest.is_none(),
            _ => false,
        };
        if self.state.audit().last() != Some(&linked.audit)
            || self.state.active().profile().profile_id.as_str()
                != linked.audit.resulting_active_profile_id.as_str()
            || self.state.active().profile_digest().as_str()
                != linked.audit.resulting_active_digest.as_str()
            || !semantic_binding_valid
        {
            return Err(SemanticRuntimeContractErrorV1::InvalidTransition);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum RetrievalProfileActivationObserverErrorV1 {
    #[error("retrieval activation observer is unavailable")]
    Unavailable,
    #[error("retrieval activation observer rejected committed state")]
    Rejected,
    #[error("retrieval activation observer detected stale committed state")]
    Conflict,
}

/// Post-durable-commit observer for daemon-owned retrieval authorities.
///
/// The configuration transaction remains authoritative. An observer failure
/// prevents publication to live serving but never rewrites the committed
/// configuration state.
pub trait RetrievalProfileActivationObserverV1: Send + Sync {
    fn activation_committed(
        &self,
        committed: CommittedRetrievalProfileStateV1,
    ) -> SemanticRuntimeFuture<'_, Result<(), RetrievalProfileActivationObserverErrorV1>>;
}

/// Typed adapter over `config::retrieval` PASS-only profile admission and CAS.
/// `commit_linked_transition` owns the atomic configuration/runtime mutation;
/// returning success without both durable links violates this port contract.
pub trait SemanticRetrievalConfigurationPortV1: Sync {
    fn current_activation<'a>(
        &'a self,
        configuration: &'a SemanticConfigurationPinV1,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<Option<SemanticCurrentLinkedActivationV1>, SemanticConfigurationBackendErrorV1>,
    >;

    fn prepare_activation<'a>(
        &'a self,
        command: &'a SemanticActivationCommandV1,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<SemanticConfigurationTransitionV1, SemanticConfigurationBackendErrorV1>,
    >;

    fn prepare_rollback<'a>(
        &'a self,
        command: &'a SemanticRollbackCommandV1,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<SemanticConfigurationTransitionV1, SemanticConfigurationBackendErrorV1>,
    >;

    fn commit_linked_transition<'a>(
        &'a self,
        transition: &'a SemanticConfigurationTransitionV1,
        receipt: Option<&'a SemanticActivationReceiptV1>,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<SemanticLinkedTransitionV1, SemanticConfigurationBackendErrorV1>,
    >;

    /// Re-read the exact current state after `commit_linked_transition`
    /// succeeds. Implementations without a production state owner fail closed.
    fn committed_profile_state<'a>(
        &'a self,
        _linked: &'a SemanticLinkedTransitionV1,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<CommittedRetrievalProfileStateV1, SemanticConfigurationBackendErrorV1>,
    > {
        Box::pin(async { Err(SemanticConfigurationBackendErrorV1::Unavailable) })
    }
}

pub trait SemanticRuntimeGenerationInspectorV1: Sync {
    fn inspect_generation<'a>(
        &'a self,
        required: &'a SemanticCompatibilityPinsV1,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<SemanticExecutableGenerationV1, SemanticRuntimeBackendErrorV1>,
    >;
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

fn validate_semantic_pins(
    pins: &SemanticCompatibilityPinsV1,
) -> Result<(), SemanticRuntimeContractErrorV1> {
    pins.implementation_revision
        .validate()
        .map_err(|_| SemanticRuntimeContractErrorV1::InvalidCompatibility)?;
    pins.fusion_revision
        .validate()
        .map_err(|_| SemanticRuntimeContractErrorV1::InvalidCompatibility)?;
    pins.artifact_manifest_digest
        .validate()
        .map_err(|_| SemanticRuntimeContractErrorV1::InvalidCompatibility)?;
    pins.runtime_compatibility_digest
        .validate()
        .map_err(|_| SemanticRuntimeContractErrorV1::InvalidCompatibility)?;
    pins.projection
        .embedding_key()
        .validate()
        .map_err(|_| SemanticRuntimeContractErrorV1::InvalidCompatibility)?;
    validate_generation(&pins.vector_generation_id)?;
    pins.calibration
        .canonical_digest()
        .map_err(|_| SemanticRuntimeContractErrorV1::InvalidCompatibility)?;
    if pins.calibration.projection_key != *pins.projection.projection_key()
        || pins.calibration.vector_generation != pins.vector_generation_id
    {
        return Err(SemanticRuntimeContractErrorV1::InvalidCompatibility);
    }
    if !resources_valid(pins.resources) {
        return Err(SemanticRuntimeContractErrorV1::InvalidCompatibility);
    }
    Ok(())
}

fn validate_optional_semantic_pins(
    pins: Option<&SemanticCompatibilityPinsV1>,
) -> Result<(), SemanticRuntimeContractErrorV1> {
    pins.map_or(Ok(()), validate_semantic_pins)
}

fn resources_valid(resources: SemanticResourceRequirementV1) -> bool {
    resources.model_bytes > 0
        && resources.tokenizer_bytes > 0
        && resources.resident_bytes >= resources.model_bytes
        && resources.resident_bytes >= resources.tokenizer_bytes
        && resources.threads > 0
        && resources.batch_size > 0
        && resources.sequence_length > 0
        && resources.load_deadline_ms > 0
}

fn resources_covered(
    required: SemanticResourceRequirementV1,
    ceiling: SemanticResourceRequirementV1,
) -> bool {
    resources_valid(required)
        && resources_valid(ceiling)
        && ceiling.model_bytes >= required.model_bytes
        && ceiling.tokenizer_bytes >= required.tokenizer_bytes
        && ceiling.resident_bytes >= required.resident_bytes
        && ceiling.threads >= required.threads
        && ceiling.batch_size >= required.batch_size
        && ceiling.sequence_length >= required.sequence_length
        && ceiling.load_deadline_ms >= required.load_deadline_ms
}

fn validate_audit_link(
    transition: &SemanticConfigurationTransitionV1,
    audit: &RetrievalProfileAuditEventV1,
) -> Result<(), SemanticRuntimeContractErrorV1> {
    audit
        .event_id
        .validate()
        .map_err(|_| SemanticRuntimeContractErrorV1::InvalidTransition)?;
    audit
        .freshness_vector_digest
        .validate()
        .map_err(|_| SemanticRuntimeContractErrorV1::InvalidTransition)?;
    let expected_event_id = canonical_sha256(&(
        RETRIEVAL_PROFILE_AUDIT_DIGEST_DOMAIN_V1,
        &audit.actor_id,
        &audit.operation,
        &audit.prior_active_profile_id,
        &audit.resulting_active_profile_id,
        &audit.prior_active_digest,
        &audit.resulting_active_digest,
        &audit.evaluation_anchor,
        &audit.freshness_vector_digest,
        &audit.base_revision,
        &audit.result_revision,
        audit.occurred_at,
    ))
    .map_err(|_| SemanticRuntimeContractErrorV1::InvalidTransition)?;
    if audit.operation != transition.operation
        || audit.prior_active_profile_id != transition.prior_active_profile_id
        || audit.resulting_active_profile_id != transition.result_active_profile_id
        || audit.prior_active_digest != transition.prior_active_profile_digest
        || audit.resulting_active_digest != transition.result_active_profile_digest
        || audit.evaluation_anchor != transition.evaluation_anchor
        || audit.base_revision != transition.base_configuration.revision_id
        || audit.result_revision != transition.result_configuration.revision_id
        || audit.occurred_at != transition.transition_at
        || audit.event_id != expected_event_id
    {
        return Err(SemanticRuntimeContractErrorV1::InvalidTransition);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn semantic_configuration_transition_digest(
    operation: &RetrievalProfileAuditOperationV1,
    base_configuration: &SemanticConfigurationPinV1,
    result_configuration: &SemanticConfigurationPinV1,
    prior_active_profile_id: &FusionProfileId,
    result_active_profile_id: &FusionProfileId,
    prior_active_profile_digest: &ManifestDigest,
    result_active_profile_digest: &ManifestDigest,
    evaluation_anchor: &RetrievalAnchorId,
    expected_cas: &RetrievalProfileCasV1,
    prior_active_semantic: Option<&SemanticCompatibilityPinsV1>,
    prior_rollback_semantic: Option<&SemanticCompatibilityPinsV1>,
    result_active_semantic: Option<&SemanticCompatibilityPinsV1>,
    result_rollback_semantic: Option<&SemanticCompatibilityPinsV1>,
    transition_at: UtcMicros,
) -> Result<ManifestDigest, SemanticRuntimeContractErrorV1> {
    #[derive(Serialize)]
    struct DigestInput<'a> {
        domain: &'static str,
        operation: &'a RetrievalProfileAuditOperationV1,
        base_configuration: &'a SemanticConfigurationPinV1,
        result_configuration: &'a SemanticConfigurationPinV1,
        prior_active_profile_id: &'a FusionProfileId,
        result_active_profile_id: &'a FusionProfileId,
        prior_active_profile_digest: &'a ManifestDigest,
        result_active_profile_digest: &'a ManifestDigest,
        evaluation_anchor: &'a RetrievalAnchorId,
        expected_configuration_revision: &'a ConfigurationRevisionId,
        expected_active_digest: &'a ManifestDigest,
        expected_rollback_digest: &'a Option<ManifestDigest>,
        prior_active_semantic: Option<&'a SemanticCompatibilityPinsV1>,
        prior_rollback_semantic: Option<&'a SemanticCompatibilityPinsV1>,
        result_active_semantic: Option<&'a SemanticCompatibilityPinsV1>,
        result_rollback_semantic: Option<&'a SemanticCompatibilityPinsV1>,
        transition_at: UtcMicros,
    }

    canonical_sha256(&DigestInput {
        domain: SEMANTIC_CONFIGURATION_TRANSITION_DIGEST_DOMAIN_V1,
        operation,
        base_configuration,
        result_configuration,
        prior_active_profile_id,
        result_active_profile_id,
        prior_active_profile_digest,
        result_active_profile_digest,
        evaluation_anchor,
        expected_configuration_revision: &expected_cas.expected_configuration_revision,
        expected_active_digest: &expected_cas.expected_active_digest,
        expected_rollback_digest: &expected_cas.expected_rollback_digest,
        prior_active_semantic,
        prior_rollback_semantic,
        result_active_semantic,
        result_rollback_semantic,
        transition_at,
    })
    .map_err(|_| SemanticRuntimeContractErrorV1::InvalidTransition)
}

fn executable_generation_digest(
    compatibility: &SemanticCompatibilityPinsV1,
    observed_ceiling: SemanticResourceRequirementV1,
    cold_offline_ready: bool,
    rollback_executable: bool,
) -> Result<ManifestDigest, SemanticRuntimeContractErrorV1> {
    canonical_sha256(&(
        SEMANTIC_EXECUTABLE_GENERATION_DIGEST_DOMAIN_V1,
        compatibility,
        observed_ceiling,
        cold_offline_ready,
        rollback_executable,
    ))
    .map_err(|_| SemanticRuntimeContractErrorV1::InvalidCompatibility)
}

fn activation_receipt_digest(
    previous_configuration: &SemanticConfigurationPinV1,
    configuration: &SemanticConfigurationPinV1,
    activated_generation: &VectorGenerationIdV1,
    previous_active_generation: Option<&VectorGenerationIdV1>,
    previous_rollback_generation: Option<&VectorGenerationIdV1>,
    rollback_generation: Option<&VectorGenerationIdV1>,
    activated_at: UtcMicros,
) -> Result<ManifestDigest, SemanticRuntimeContractErrorV1> {
    canonical_sha256(&(
        SEMANTIC_ACTIVATION_RECEIPT_DIGEST_DOMAIN_V1,
        previous_configuration,
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
    previous_configuration: &SemanticConfigurationPinV1,
    configuration: &SemanticConfigurationPinV1,
    from_generation: &VectorGenerationIdV1,
    target_generation: Option<&VectorGenerationIdV1>,
    restored_activation_receipt_digest: Option<&ManifestDigest>,
    rolled_back_at: UtcMicros,
) -> Result<ManifestDigest, SemanticRuntimeContractErrorV1> {
    canonical_sha256(&(
        SEMANTIC_ROLLBACK_RECEIPT_DIGEST_DOMAIN_V1,
        previous_configuration,
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
    use std::collections::BTreeMap;

    use tracedecay_domain::configuration::{ConfigurationRevisionId, ConfigurationSnapshotV1};
    use tracedecay_domain::{ManifestDigest, VectorGenerationIdV1};

    use super::*;
    use crate::configuration::ConfigurationCurrentStateV1;

    fn pin() -> SemanticConfigurationPinV1 {
        SemanticConfigurationPinV1::from_current(&ConfigurationCurrentStateV1 {
            revision_id: ConfigurationRevisionId::try_from("configuration.revision.1".to_owned())
                .unwrap(),
            snapshot: ConfigurationSnapshotV1::new(BTreeMap::default(), BTreeMap::default())
                .unwrap(),
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
