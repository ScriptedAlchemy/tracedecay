use crate::config::retrieval::RetrievalProfileAuditOperationV1;
use std::sync::Arc;

use super::ports::{
    RetrievalProfileActivationObserverErrorV1, RetrievalProfileActivationObserverV1,
    SemanticActivationCommandV1, SemanticActivationReceiptV1, SemanticConfigurationBackendErrorV1,
    SemanticConfigurationPinV1, SemanticConfigurationTransitionV1, SemanticFallbackReasonV1,
    SemanticLinkedTransitionV1, SemanticRetrievalConfigurationPortV1, SemanticRollbackCommandV1,
    SemanticRollbackReceiptV1, SemanticRuntimeBackendErrorV1, SemanticRuntimeBackendV1,
    SemanticRuntimeContractErrorV1, SemanticRuntimeFuture, SemanticRuntimeGenerationInspectorV1,
    SemanticRuntimeStateV1,
};

/// Production-semantic lifecycle backend over the PASS-only retrieval
/// configuration owner and the installed-generation verifier.
///
/// The configuration port owns the single atomic CAS that updates the active
/// retrieval profile, rollback profile, semantic pointer, receipt, and audit
/// event. This adapter validates every typed input and refuses to publish a
/// receipt before that linked commit succeeds.
pub struct ConfigurationLinkedSemanticRuntimeBackendV1<C, I> {
    configuration: C,
    generations: I,
    activation_observer: Option<Arc<dyn RetrievalProfileActivationObserverV1>>,
}

impl<C, I> ConfigurationLinkedSemanticRuntimeBackendV1<C, I> {
    pub fn new(configuration: C, generations: I) -> Self {
        Self {
            configuration,
            generations,
            activation_observer: None,
        }
    }

    pub fn new_with_activation_observer(
        configuration: C,
        generations: I,
        activation_observer: Arc<dyn RetrievalProfileActivationObserverV1>,
    ) -> Self {
        Self {
            configuration,
            generations,
            activation_observer: Some(activation_observer),
        }
    }

    pub fn configuration(&self) -> &C {
        &self.configuration
    }

    pub fn generations(&self) -> &I {
        &self.generations
    }
}

impl<C, I> SemanticRuntimeBackendV1 for ConfigurationLinkedSemanticRuntimeBackendV1<C, I>
where
    C: SemanticRetrievalConfigurationPortV1,
    I: SemanticRuntimeGenerationInspectorV1,
{
    fn status<'a>(
        &'a self,
        configuration: &'a SemanticConfigurationPinV1,
    ) -> SemanticRuntimeFuture<'a, Result<SemanticRuntimeStateV1, SemanticRuntimeBackendErrorV1>>
    {
        Box::pin(async move {
            let current = self
                .configuration
                .current_activation(configuration)
                .await
                .map_err(map_configuration_error)?;
            let Some(current) = current else {
                return Ok(SemanticRuntimeStateV1::Unavailable {
                    reason: SemanticFallbackReasonV1::ArtifactUnavailable,
                });
            };
            current
                .receipt
                .validate()
                .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)?;
            if current.receipt.configuration != *configuration {
                return Err(SemanticRuntimeBackendErrorV1::Conflict);
            }
            let evidence = self
                .generations
                .inspect_generation(&current.compatibility)
                .await?;
            evidence
                .validate_for(&current.compatibility, false)
                .map_err(map_contract_error)?;
            Ok(SemanticRuntimeStateV1::Current {
                receipt: current.receipt,
            })
        })
    }

    fn activate<'a>(
        &'a self,
        command: &'a SemanticActivationCommandV1,
    ) -> SemanticRuntimeFuture<'a, Result<SemanticActivationReceiptV1, SemanticRuntimeBackendErrorV1>>
    {
        Box::pin(async move {
            command
                .request
                .validate()
                .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)?;
            let transition = self
                .configuration
                .prepare_activation(command)
                .await
                .map_err(map_configuration_error)?;
            validate_activation_transition(command, &transition).map_err(map_contract_error)?;
            let result_active_semantic = transition
                .result_active_semantic
                .as_ref()
                .ok_or(SemanticRuntimeBackendErrorV1::Rejected)?;
            self.verify_generation(result_active_semantic, false)
                .await?;
            if let Some(rollback) = transition.result_rollback_semantic.as_ref() {
                self.verify_generation(rollback, true).await?;
            }
            let receipt = SemanticActivationReceiptV1::issue_transition(
                command,
                transition.result_configuration.clone(),
                transition.transition_at,
            )
            .map_err(map_contract_error)?;
            let linked = self
                .configuration
                .commit_linked_transition(&transition, Some(&receipt))
                .await
                .map_err(map_configuration_error)?;
            linked
                .validate_for(&transition, Some(&receipt))
                .map_err(map_contract_error)?;
            self.publish_committed_activation(&linked).await?;
            Ok(receipt)
        })
    }

    fn rollback<'a>(
        &'a self,
        command: &'a SemanticRollbackCommandV1,
    ) -> SemanticRuntimeFuture<'a, Result<SemanticRollbackReceiptV1, SemanticRuntimeBackendErrorV1>>
    {
        Box::pin(async move {
            command
                .request
                .validate()
                .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)?;
            let transition = self
                .configuration
                .prepare_rollback(command)
                .await
                .map_err(map_configuration_error)?;
            validate_rollback_transition(command, &transition).map_err(map_contract_error)?;
            if let Some(result_active_semantic) = transition.result_active_semantic.as_ref() {
                self.verify_generation(result_active_semantic, true).await?;
            }
            let receipt = SemanticRollbackReceiptV1::issue_transition(
                command,
                transition.result_configuration.clone(),
                transition.transition_at,
            )
            .map_err(map_contract_error)?;
            let linked = self
                .configuration
                .commit_linked_transition(&transition, receipt.restored_activation.as_ref())
                .await
                .map_err(map_configuration_error)?;
            linked
                .validate_for(&transition, receipt.restored_activation.as_ref())
                .map_err(map_contract_error)?;
            self.publish_committed_activation(&linked).await?;
            Ok(receipt)
        })
    }
}

impl<C, I> ConfigurationLinkedSemanticRuntimeBackendV1<C, I>
where
    C: SemanticRetrievalConfigurationPortV1,
    I: SemanticRuntimeGenerationInspectorV1,
{
    async fn verify_generation(
        &self,
        required: &crate::config::retrieval::SemanticCompatibilityPinsV1,
        require_cold_offline_rollback: bool,
    ) -> Result<(), SemanticRuntimeBackendErrorV1> {
        let evidence = self.generations.inspect_generation(required).await?;
        evidence
            .validate_for(required, require_cold_offline_rollback)
            .map_err(map_contract_error)
    }

    async fn publish_committed_activation(
        &self,
        linked: &SemanticLinkedTransitionV1,
    ) -> Result<(), SemanticRuntimeBackendErrorV1> {
        let Some(observer) = self.activation_observer.as_ref() else {
            return Ok(());
        };
        let committed = self
            .configuration
            .committed_profile_state(linked)
            .await
            .map_err(map_configuration_error)?;
        committed.validate_for(linked).map_err(map_contract_error)?;
        observer
            .activation_committed(committed)
            .await
            .map_err(map_activation_observer_error)
    }
}

fn validate_activation_transition(
    command: &SemanticActivationCommandV1,
    transition: &SemanticConfigurationTransitionV1,
) -> Result<(), SemanticRuntimeContractErrorV1> {
    transition.validate()?;
    if !matches!(
        transition.operation,
        RetrievalProfileAuditOperationV1::Activate
    ) || transition.base_configuration != command.configuration
        || generation_of(transition.result_active_semantic.as_ref())
            != Some(&command.request.target_generation)
        || generation_of(transition.prior_active_semantic.as_ref())
            != command.request.expected_active_generation.as_ref()
        || generation_of(transition.prior_rollback_semantic.as_ref())
            != command.request.expected_rollback_generation.as_ref()
    {
        return Err(SemanticRuntimeContractErrorV1::InvalidTransition);
    }
    Ok(())
}

fn validate_rollback_transition(
    command: &SemanticRollbackCommandV1,
    transition: &SemanticConfigurationTransitionV1,
) -> Result<(), SemanticRuntimeContractErrorV1> {
    transition.validate()?;
    if !matches!(
        transition.operation,
        RetrievalProfileAuditOperationV1::Rollback { .. }
    ) || transition.base_configuration != command.configuration
        || generation_of(transition.result_active_semantic.as_ref())
            != command.request.target_generation.as_ref()
        || generation_of(transition.prior_active_semantic.as_ref())
            != Some(&command.request.expected_active_generation)
        || generation_of(transition.prior_rollback_semantic.as_ref())
            != command.request.expected_rollback_generation.as_ref()
    {
        return Err(SemanticRuntimeContractErrorV1::InvalidTransition);
    }
    Ok(())
}

fn generation_of(
    pins: Option<&crate::config::retrieval::SemanticCompatibilityPinsV1>,
) -> Option<&tracedecay_domain::VectorGenerationIdV1> {
    pins.map(|pins| &pins.vector_generation_id)
}

fn map_configuration_error(
    error: SemanticConfigurationBackendErrorV1,
) -> SemanticRuntimeBackendErrorV1 {
    match error {
        SemanticConfigurationBackendErrorV1::Unavailable => {
            SemanticRuntimeBackendErrorV1::Unavailable
        }
        SemanticConfigurationBackendErrorV1::Rejected => SemanticRuntimeBackendErrorV1::Rejected,
        SemanticConfigurationBackendErrorV1::Conflict => SemanticRuntimeBackendErrorV1::Conflict,
    }
}

fn map_contract_error(error: SemanticRuntimeContractErrorV1) -> SemanticRuntimeBackendErrorV1 {
    match error {
        SemanticRuntimeContractErrorV1::ResourceCeilingExceeded
        | SemanticRuntimeContractErrorV1::RollbackNotExecutable => {
            SemanticRuntimeBackendErrorV1::Unavailable
        }
        SemanticRuntimeContractErrorV1::InvalidConfiguration
        | SemanticRuntimeContractErrorV1::InvalidTransition => {
            SemanticRuntimeBackendErrorV1::Conflict
        }
        _ => SemanticRuntimeBackendErrorV1::Rejected,
    }
}

fn map_activation_observer_error(
    error: RetrievalProfileActivationObserverErrorV1,
) -> SemanticRuntimeBackendErrorV1 {
    match error {
        RetrievalProfileActivationObserverErrorV1::Unavailable => {
            SemanticRuntimeBackendErrorV1::Unavailable
        }
        RetrievalProfileActivationObserverErrorV1::Rejected => {
            SemanticRuntimeBackendErrorV1::Rejected
        }
        RetrievalProfileActivationObserverErrorV1::Conflict => {
            SemanticRuntimeBackendErrorV1::Conflict
        }
    }
}
