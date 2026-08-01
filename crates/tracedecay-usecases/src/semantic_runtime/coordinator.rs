use std::sync::Arc;

use thiserror::Error;
use tracedecay_domain::{ManifestDigest, UtcMicros};

use super::{
    ConfigurationLinkedSemanticRuntimeBackendV1, ProductionSemanticRetrievalConfigurationStoreV1,
    ProductionSemanticRuntimeV1, RetrievalProfileActivationObserverV1, SemanticActivationReceiptV1,
    SemanticActivationRequestV1, SemanticConfigurationBackendErrorV1, SemanticConfigurationPinV1,
    SemanticRollbackReceiptV1, SemanticRollbackRequestV1, SemanticRuntimeControlErrorV1,
    SemanticRuntimeFuture, SemanticRuntimeIntegrationPortV1, SemanticRuntimeOwnerV1,
    SemanticRuntimeStatusV1,
};
use crate::configuration::{ConfigurationCurrentStateV1, DirectConfigurationMutation};
use crate::config::retrieval::{
    AcceptedRetrievalProfileV1, RetrievalProfileCasV1, RetrievalProfileMutationCapabilityV1,
    RetrievalProfileStateV1, RetrievalRuntimeCompatibilityV1,
};
use tracedecay_global_db::configuration::OwnedGlobalDbConfigurationControlStore;

type ProductionOwner = SemanticRuntimeOwnerV1<
    OwnedGlobalDbConfigurationControlStore,
    ConfigurationLinkedSemanticRuntimeBackendV1<
        ProductionSemanticRetrievalConfigurationStoreV1,
        ProductionSemanticRuntimeV1,
    >,
>;

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum SemanticActivationCoordinationErrorV1 {
    #[error("semantic activation configuration authority is unavailable")]
    Unavailable,
    #[error("semantic activation input was rejected")]
    Rejected,
    #[error("semantic activation compare-and-swap conflicted")]
    Conflict,
    #[error("semantic runtime activation failed: {0}")]
    Runtime(#[from] SemanticRuntimeControlErrorV1),
}

/// Application coordinator for an already-authorized Plan 20 mutation and its
/// linked semantic profile transition. It never selects profiles, fabricates
/// grants, or exposes a transport endpoint.
pub struct ProductionSemanticActivationCoordinatorV1 {
    configuration: ProductionSemanticRetrievalConfigurationStoreV1,
    owner: Arc<ProductionOwner>,
}

impl ProductionSemanticActivationCoordinatorV1 {
    pub fn new(
        configuration: ProductionSemanticRetrievalConfigurationStoreV1,
        central_configuration: OwnedGlobalDbConfigurationControlStore,
        runtime: ProductionSemanticRuntimeV1,
        observer: Arc<dyn RetrievalProfileActivationObserverV1>,
    ) -> Self {
        let backend = ConfigurationLinkedSemanticRuntimeBackendV1::new_with_activation_observer(
            configuration.clone(),
            runtime,
            observer,
        );
        Self {
            configuration,
            owner: Arc::new(SemanticRuntimeOwnerV1::new(central_configuration, backend)),
        }
    }

    pub async fn bootstrap_query_profile(
        &self,
        configuration: ConfigurationCurrentStateV1,
        accepted_query: AcceptedRetrievalProfileV1,
        runtime: &RetrievalRuntimeCompatibilityV1,
    ) -> Result<(), SemanticActivationCoordinationErrorV1> {
        if !accepted_query.is_exact_query_fallback() {
            return Err(SemanticActivationCoordinationErrorV1::Rejected);
        }
        let pin = SemanticConfigurationPinV1::from_current(&configuration)
            .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
        let state =
            RetrievalProfileStateV1::new(configuration.revision_id, accepted_query, runtime)
                .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
        self.configuration
            .install_initial_state(&pin, &state)
            .await
            .map_err(map_configuration_error)
    }

    pub async fn current_profile_state(
        &self,
    ) -> Result<
        crate::config::retrieval::RetrievalProfileStateSnapshotV1,
        SemanticActivationCoordinationErrorV1,
    > {
        self.configuration
            .current_profile_state()
            .await
            .map_err(map_configuration_error)
    }

    pub async fn preview_central_mutation(
        &self,
        authority: &crate::configuration::ConfigurationMutationAuthority,
        mutation: &DirectConfigurationMutation,
        expected_revision: &tracedecay_domain::ConfigurationRevisionId,
    ) -> Result<
        tracedecay_global_db::configuration::store::ConfigurationDirectCommitOutcomeV1,
        SemanticActivationCoordinationErrorV1,
    > {
        self.configuration
            .preview_central_mutation(authority, mutation, expected_revision)
            .await
            .map_err(map_configuration_error)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn stage_and_activate(
        &self,
        base_configuration: SemanticConfigurationPinV1,
        result_configuration: ConfigurationCurrentStateV1,
        capability: &RetrievalProfileMutationCapabilityV1,
        expected: RetrievalProfileCasV1,
        candidate: AcceptedRetrievalProfileV1,
        current_runtime: &RetrievalRuntimeCompatibilityV1,
        candidate_runtime: &RetrievalRuntimeCompatibilityV1,
        central_mutation: DirectConfigurationMutation,
        freshness_vector_digest: ManifestDigest,
        now: UtcMicros,
    ) -> Result<SemanticActivationReceiptV1, SemanticActivationCoordinationErrorV1> {
        let result_configuration = SemanticConfigurationPinV1::from_current(&result_configuration)
            .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
        let transition = self
            .configuration
            .stage_activation(
                base_configuration,
                result_configuration,
                capability,
                expected,
                candidate,
                current_runtime,
                candidate_runtime,
                central_mutation,
                freshness_vector_digest,
                now,
            )
            .await
            .map_err(map_configuration_error)?;
        let target = transition
            .result_active_semantic
            .as_ref()
            .ok_or(SemanticActivationCoordinationErrorV1::Rejected)?
            .vector_generation_id
            .clone();
        let request = SemanticActivationRequestV1::new(
            target,
            transition
                .prior_active_semantic
                .as_ref()
                .map(|pins| pins.vector_generation_id.clone()),
            transition
                .prior_rollback_semantic
                .as_ref()
                .map(|pins| pins.vector_generation_id.clone()),
        )
        .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
        self.owner.activate(request).await.map_err(Into::into)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn stage_and_rollback(
        &self,
        base_configuration: SemanticConfigurationPinV1,
        result_configuration: ConfigurationCurrentStateV1,
        capability: &RetrievalProfileMutationCapabilityV1,
        expected: RetrievalProfileCasV1,
        restored_runtime: &RetrievalRuntimeCompatibilityV1,
        central_mutation: DirectConfigurationMutation,
        trigger: String,
        freshness_vector_digest: ManifestDigest,
        now: UtcMicros,
    ) -> Result<SemanticRollbackReceiptV1, SemanticActivationCoordinationErrorV1> {
        let result_configuration = SemanticConfigurationPinV1::from_current(&result_configuration)
            .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
        let transition = self
            .configuration
            .stage_rollback(
                base_configuration,
                result_configuration,
                capability,
                expected,
                restored_runtime,
                central_mutation,
                trigger,
                freshness_vector_digest,
                now,
            )
            .await
            .map_err(map_configuration_error)?;
        let expected_active = transition
            .prior_active_semantic
            .as_ref()
            .ok_or(SemanticActivationCoordinationErrorV1::Rejected)?
            .vector_generation_id
            .clone();
        let request = match transition.result_active_semantic.as_ref() {
            Some(target) => SemanticRollbackRequestV1::new(
                target.vector_generation_id.clone(),
                expected_active,
                transition
                    .prior_rollback_semantic
                    .as_ref()
                    .ok_or(SemanticActivationCoordinationErrorV1::Rejected)?
                    .vector_generation_id
                    .clone(),
            ),
            None => SemanticRollbackRequestV1::disable(expected_active),
        }
        .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
        self.owner.rollback(request).await.map_err(Into::into)
    }
}

impl SemanticRuntimeIntegrationPortV1 for ProductionSemanticActivationCoordinatorV1 {
    fn status(&self) -> SemanticRuntimeFuture<'_, SemanticRuntimeStatusV1> {
        SemanticRuntimeIntegrationPortV1::status(self.owner.as_ref())
    }

    fn activate(
        &self,
        request: SemanticActivationRequestV1,
    ) -> SemanticRuntimeFuture<'_, Result<SemanticActivationReceiptV1, SemanticRuntimeControlErrorV1>>
    {
        SemanticRuntimeIntegrationPortV1::activate(self.owner.as_ref(), request)
    }

    fn rollback(
        &self,
        request: SemanticRollbackRequestV1,
    ) -> SemanticRuntimeFuture<'_, Result<SemanticRollbackReceiptV1, SemanticRuntimeControlErrorV1>>
    {
        SemanticRuntimeIntegrationPortV1::rollback(self.owner.as_ref(), request)
    }
}

fn map_configuration_error(
    error: SemanticConfigurationBackendErrorV1,
) -> SemanticActivationCoordinationErrorV1 {
    match error {
        SemanticConfigurationBackendErrorV1::Unavailable => {
            SemanticActivationCoordinationErrorV1::Unavailable
        }
        SemanticConfigurationBackendErrorV1::Rejected => {
            SemanticActivationCoordinationErrorV1::Rejected
        }
        SemanticConfigurationBackendErrorV1::Conflict => {
            SemanticActivationCoordinationErrorV1::Conflict
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_production_mount<T: SemanticRuntimeIntegrationPortV1 + Send + Sync>() {}

    #[test]
    fn production_coordinator_is_a_concrete_project_runtime_mount() {
        assert_production_mount::<ProductionSemanticActivationCoordinatorV1>();
        std::hint::black_box(ProductionSemanticActivationCoordinatorV1::new);
    }
}
