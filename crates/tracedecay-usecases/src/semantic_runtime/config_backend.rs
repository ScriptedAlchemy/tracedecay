use crate::config::retrieval::RetrievalProfileAuditOperationV1;
use std::sync::{Arc, Mutex};

use super::ports::{
    CommittedRetrievalProfileStateV1, RetrievalProfileActivationObserverErrorV1,
    RetrievalProfileActivationObserverV1, SemanticActivationCommandV1, SemanticActivationReceiptV1,
    SemanticConfigurationBackendErrorV1, SemanticConfigurationPinV1,
    SemanticConfigurationTransitionV1, SemanticFallbackReasonV1, SemanticLinkedTransitionV1,
    SemanticRetrievalConfigurationPortV1, SemanticRollbackCommandV1, SemanticRollbackReceiptV1,
    SemanticRuntimeBackendErrorV1, SemanticRuntimeBackendV1, SemanticRuntimeContractErrorV1,
    SemanticRuntimeFuture, SemanticRuntimeGenerationInspectorV1, SemanticRuntimeStateV1,
};

/// Production-semantic lifecycle backend over the PASS-only retrieval
/// configuration owner and the installed-generation verifier.
///
/// The configuration port owns the single atomic CAS that updates the active
/// retrieval profile, rollback profile, semantic compatibility pin, receipt,
/// and audit event. This adapter validates every typed input and refuses to publish a
/// receipt before that linked commit succeeds.
pub struct ConfigurationLinkedSemanticRuntimeBackendV1<C, I> {
    configuration: C,
    generations: I,
    activation_observer: Option<Arc<dyn RetrievalProfileActivationObserverV1>>,
    observation_state: Mutex<ActivationObservationStateV1>,
}

#[derive(Clone)]
struct ActivationObservationFailureV1 {
    configuration: SemanticConfigurationPinV1,
    generation: Option<tracedecay_domain::VectorGenerationIdV1>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ActivationObservationTicketV1 {
    epoch: u64,
    sequence: u64,
}

impl ActivationObservationTicketV1 {
    fn next(self) -> Self {
        if self.sequence == u64::MAX {
            Self {
                epoch: self.epoch.wrapping_add(1),
                sequence: 0,
            }
        } else {
            Self {
                epoch: self.epoch,
                sequence: self.sequence + 1,
            }
        }
    }
}

#[derive(Default)]
struct ActivationObservationStateV1 {
    next_ticket: ActivationObservationTicketV1,
    current_transition: Option<ActivationObservationTransitionV1>,
    failure: Option<ActivationObservationFailureV1>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ActivationObservationTransitionV1 {
    epoch: i64,
    result_revision: tracedecay_domain::configuration::ConfigurationRevisionId,
    transition_digest: tracedecay_domain::ManifestDigest,
    ticket: ActivationObservationTicketV1,
}

impl<C, I> ConfigurationLinkedSemanticRuntimeBackendV1<C, I> {
    pub fn new(configuration: C, generations: I) -> Self {
        Self {
            configuration,
            generations,
            activation_observer: None,
            observation_state: Mutex::new(ActivationObservationStateV1::default()),
        }
    }

    fn record_observation(
        &self,
        ticket: ActivationObservationTicketV1,
        configuration: SemanticConfigurationPinV1,
        generation: Option<tracedecay_domain::VectorGenerationIdV1>,
        observed: bool,
    ) {
        let mut state = self
            .observation_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(current) = state.current_transition.as_ref() else {
            return;
        };
        if current.ticket != ticket || current.result_revision != configuration.revision_id {
            return;
        }
        if observed {
            if state
                .failure
                .as_ref()
                .is_some_and(|failure| failure.configuration == configuration)
            {
                state.failure = None;
            }
        } else {
            state.failure = Some(ActivationObservationFailureV1 {
                configuration,
                generation,
            });
        }
    }

    fn reserve_observation(
        &self,
        epoch: i64,
        result_revision: &tracedecay_domain::configuration::ConfigurationRevisionId,
        transition_digest: &tracedecay_domain::ManifestDigest,
    ) -> Option<ActivationObservationTicketV1> {
        if epoch <= 0 {
            return None;
        }
        let mut state = self
            .observation_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(current) = state.current_transition.as_ref() {
            let advances = epoch > current.epoch;
            let exact_retry = epoch == current.epoch
                && &current.result_revision == result_revision
                && &current.transition_digest == transition_digest;
            if !advances && !exact_retry {
                return None;
            }
        }
        state.next_ticket = state.next_ticket.next();
        let ticket = state.next_ticket;
        state.current_transition = Some(ActivationObservationTransitionV1 {
            epoch,
            result_revision: result_revision.clone(),
            transition_digest: transition_digest.clone(),
            ticket,
        });
        Some(ticket)
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
            observation_state: Mutex::new(ActivationObservationStateV1::default()),
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
            let observed_failure = {
                self.observation_state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .failure
                    .as_ref()
                    .filter(|failure| &failure.configuration == configuration)
                    .map(|failure| failure.generation.clone())
            };
            if let Some(generation) = observed_failure {
                return Ok(SemanticRuntimeStateV1::Degraded {
                    active_generation: generation.clone(),
                    reason: SemanticFallbackReasonV1::RuntimeFailure,
                });
            }
            let evidence = self
                .generations
                .inspect_generation(&current.compatibility)
                .await?;
            evidence
                .evidence()
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
            let active_lease = self
                .verify_generation(result_active_semantic, false)
                .await?;
            let rollback_lease = match transition.result_rollback_semantic.as_ref() {
                Some(rollback) => Some(self.verify_generation(rollback, true).await?),
                None => None,
            };
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
            if let Some((observation_ticket, observed)) =
                self.observe_committed_activation(&linked).await
            {
                self.record_observation(
                    observation_ticket,
                    receipt.configuration.clone(),
                    Some(receipt.activated_generation.clone()),
                    observed,
                );
            }
            drop((active_lease, rollback_lease));
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
            let generation_leases = self
                .verify_rollback_generations(
                    transition.result_active_semantic.as_ref(),
                    transition.result_rollback_semantic.as_ref(),
                )
                .await?;
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
            if let Some((observation_ticket, observed)) =
                self.observe_committed_activation(&linked).await
            {
                self.record_observation(
                    observation_ticket,
                    receipt.configuration.clone(),
                    receipt
                        .restored_activation
                        .as_ref()
                        .map(|activation| activation.activated_generation.clone()),
                    observed,
                );
            }
            drop(generation_leases);
            Ok(receipt)
        })
    }
}

impl<C, I> ConfigurationLinkedSemanticRuntimeBackendV1<C, I>
where
    I: SemanticRuntimeGenerationInspectorV1,
{
    async fn verify_generation(
        &self,
        required: &crate::config::retrieval::SemanticCompatibilityPinsV1,
        require_cold_offline_rollback: bool,
    ) -> Result<super::SemanticExecutableGenerationLeaseV1, SemanticRuntimeBackendErrorV1> {
        let evidence = self.generations.inspect_generation(required).await?;
        evidence
            .evidence()
            .validate_for(required, require_cold_offline_rollback)
            .map_err(map_contract_error)?;
        Ok(evidence)
    }

    async fn verify_rollback_generations(
        &self,
        result_active: Option<&crate::config::retrieval::SemanticCompatibilityPinsV1>,
        result_rollback: Option<&crate::config::retrieval::SemanticCompatibilityPinsV1>,
    ) -> Result<Vec<super::SemanticExecutableGenerationLeaseV1>, SemanticRuntimeBackendErrorV1>
    {
        let requirements = unique_rollback_requirements(result_active, result_rollback);
        let mut leases = Vec::with_capacity(requirements.len());
        for required in requirements {
            leases.push(self.verify_generation(required, true).await?);
        }
        Ok(leases)
    }
}

impl<C, I> ConfigurationLinkedSemanticRuntimeBackendV1<C, I>
where
    C: SemanticRetrievalConfigurationPortV1,
    I: SemanticRuntimeGenerationInspectorV1,
{
    /// Reconcile one exact durable activation through the same observation
    /// ticket and failure state used by the initial linked transition.
    pub async fn reconcile_committed_activation(
        &self,
        committed: CommittedRetrievalProfileStateV1,
    ) -> Result<(), RetrievalProfileActivationObserverErrorV1> {
        let Some(observer) = self.activation_observer.as_ref() else {
            return Ok(());
        };
        let Some(current) = committed.current_activation.as_ref() else {
            return Ok(());
        };
        if current.receipt.configuration.revision_id != *committed.state.configuration_revision() {
            return Err(RetrievalProfileActivationObserverErrorV1::Rejected);
        }
        let configuration = current.receipt.configuration.clone();
        let generation = current.receipt.activated_generation.clone();
        let ticket = self
            .reserve_observation(
                committed.epoch,
                committed.state.configuration_revision(),
                &committed.transition_digest,
            )
            .ok_or(RetrievalProfileActivationObserverErrorV1::Conflict)?;
        let result = observer.activation_committed(committed).await;
        self.record_observation(ticket, configuration, Some(generation), result.is_ok());
        result
    }

    async fn observe_committed_activation(
        &self,
        linked: &SemanticLinkedTransitionV1,
    ) -> Option<(ActivationObservationTicketV1, bool)> {
        let Some(observer) = self.activation_observer.as_ref() else {
            return None;
        };
        let ticket = self.reserve_observation(
            linked.epoch,
            &linked.audit.result_revision,
            &linked.transition_digest,
        )?;
        let committed = match self.configuration.committed_profile_state(linked).await {
            Ok(committed) => committed,
            Err(SemanticConfigurationBackendErrorV1::Conflict) => return None,
            Err(
                SemanticConfigurationBackendErrorV1::Unavailable
                | SemanticConfigurationBackendErrorV1::Rejected,
            ) => return Some((ticket, false)),
        };
        if committed.validate_for(linked).is_err() {
            return Some((ticket, false));
        }
        let observed = observer.activation_committed(committed).await.is_ok();
        Some((ticket, observed))
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

fn unique_rollback_requirements<'a, T: Eq>(
    result_active: Option<&'a T>,
    result_rollback: Option<&'a T>,
) -> Vec<&'a T> {
    let mut required = Vec::with_capacity(2);
    if let Some(active) = result_active {
        required.push(active);
    }
    if let Some(rollback) = result_rollback
        && result_active != Some(rollback)
    {
        required.push(rollback);
    }
    required
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

#[cfg(test)]
#[path = "config_backend_tests.rs"]
mod tests;
