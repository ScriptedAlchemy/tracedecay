use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tracedecay_application::SemanticActivationCoordinationPort;
use tracedecay_domain::configuration::ConfigurationRevisionId;
use tracedecay_domain::{ManifestDigest, UtcMicros};

use super::{
    ConfigurationLinkedSemanticRuntimeBackendV1, ProductionSemanticRetrievalConfigurationStoreV1,
    ProductionSemanticRuntimeV1, RetrievalProfileActivationObserverV1, SemanticActivationReceiptV1,
    SemanticActivationRequestV1, SemanticConfigurationBackendErrorV1, SemanticConfigurationPinV1,
    SemanticRollbackReceiptV1, SemanticRollbackRequestV1, SemanticRuntimeControlErrorV1,
    SemanticRuntimeFuture, SemanticRuntimeIntegrationPortV1, SemanticRuntimeOwnerV1,
    SemanticRuntimeStatusV1,
};
use crate::config::retrieval::{
    AcceptedRetrievalProfileV1, RetrievalProfileCasV1, RetrievalProfileMutationCapabilityV1,
    RetrievalProfileStateSnapshotV1, RetrievalProfileStateV1, RetrievalRuntimeCompatibilityV1,
};
use tracedecay_configuration::{
    ConfigurationCurrentStateV1, ConfigurationMutationAuthority, DirectConfigurationMutation,
};
use tracedecay_global_db::configuration::OwnedGlobalDbConfigurationControlStore;
use tracedecay_global_db::configuration::store::ConfigurationDirectCommitOutcomeV1;

pub use tracedecay_application::SemanticActivationCoordinationErrorV1;

type ProductionOwner = SemanticRuntimeOwnerV1<
    OwnedGlobalDbConfigurationControlStore,
    ConfigurationLinkedSemanticRuntimeBackendV1<
        ProductionSemanticRetrievalConfigurationStoreV1,
        ProductionSemanticRuntimeV1,
    >,
>;

impl From<SemanticRuntimeControlErrorV1> for SemanticActivationCoordinationErrorV1 {
    fn from(error: SemanticRuntimeControlErrorV1) -> Self {
        Self::Runtime(error.to_string())
    }
}

/// Application coordinator for an already-authorized configuration mutation and its
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

    pub(crate) fn configuration_inventory_authority(
        &self,
    ) -> ProductionSemanticRetrievalConfigurationStoreV1 {
        self.configuration.clone()
    }

    /// Re-observe the exact latest durable transition after a verified model
    /// lifecycle recovery. The registrar remains the sole live-route publisher;
    /// this method neither changes configuration nor publishes graph state.
    #[hotpath::measure(label = "usecases.semantic.reobserve", future = true)]
    pub async fn reobserve_current_activation(
        &self,
    ) -> Result<
        Option<(
            i64,
            tracedecay_domain::ConfigurationRevisionId,
            ManifestDigest,
        )>,
        SemanticActivationCoordinationErrorV1,
    > {
        let Some(committed) = self
            .configuration
            .current_committed_state()
            .await
            .map_err(configuration_error_at("current_committed_state"))?
        else {
            return Ok(None);
        };
        let identity = (
            committed.epoch,
            committed.state.configuration_revision().clone(),
            committed.transition_digest.clone(),
        );
        self.owner
            .runtime()
            .reconcile_committed_activation(committed)
            .await
            .map_err(|error| match error {
                super::RetrievalProfileActivationObserverErrorV1::Unavailable => {
                    SemanticActivationCoordinationErrorV1::Unavailable
                }
                super::RetrievalProfileActivationObserverErrorV1::Rejected => {
                    SemanticActivationCoordinationErrorV1::Rejected
                }
                super::RetrievalProfileActivationObserverErrorV1::Conflict => {
                    SemanticActivationCoordinationErrorV1::Conflict
                }
            })?;
        Ok(Some(identity))
    }

    #[hotpath::measure(label = "usecases.semantic.bootstrap", future = true)]
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
            .map_err(configuration_error_at("bootstrap_query_profile.install"))
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
            .map_err(configuration_error_at("current_profile_state"))
    }

    pub async fn preview_central_mutation(
        &self,
        authority: &tracedecay_configuration::ConfigurationMutationAuthority,
        mutation: &DirectConfigurationMutation,
        expected_revision: &tracedecay_domain::ConfigurationRevisionId,
    ) -> Result<
        tracedecay_global_db::configuration::store::ConfigurationDirectCommitOutcomeV1,
        SemanticActivationCoordinationErrorV1,
    > {
        self.configuration
            .preview_central_mutation(authority, mutation, expected_revision)
            .await
            .map_err(configuration_error_at("preview_central_mutation"))
    }

    #[allow(clippy::too_many_arguments)]
    #[hotpath::measure(label = "usecases.semantic.activate", future = true)]
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
            .map_err(configuration_error_at(
                "stage_and_activate.stage_activation",
            ))?;
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
        self.owner
            .activate(request)
            .await
            .map_err(SemanticActivationCoordinationErrorV1::from)
            .inspect_err(crate::hotpath_observe::semantic_coordination_error)
    }

    #[allow(clippy::too_many_arguments)]
    #[hotpath::measure(label = "usecases.semantic.rollback", future = true)]
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
            .map_err(|_| {
                SemanticActivationCoordinationErrorV1::RejectedDetail(
                    "stage_and_rollback: result configuration is not pinnable".to_owned(),
                )
            })?;
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
            .map_err(configuration_error_at("stage_and_rollback.stage_rollback"))?;
        let expected_active = transition
            .prior_active_semantic
            .as_ref()
            .ok_or_else(|| {
                SemanticActivationCoordinationErrorV1::RejectedDetail(
                    "stage_and_rollback: staged transition has no prior active semantic pin"
                        .to_owned(),
                )
            })?
            .vector_generation_id
            .clone();
        let request = match transition.result_active_semantic.as_ref() {
            Some(target) => SemanticRollbackRequestV1::new(
                target.vector_generation_id.clone(),
                expected_active,
                transition
                    .prior_rollback_semantic
                    .as_ref()
                    .ok_or_else(|| {
                        SemanticActivationCoordinationErrorV1::RejectedDetail(
                            "stage_and_rollback: staged transition has no prior rollback \
                             semantic pin"
                                .to_owned(),
                        )
                    })?
                    .vector_generation_id
                    .clone(),
            ),
            None => SemanticRollbackRequestV1::disable(expected_active),
        }
        .map_err(|_| {
            SemanticActivationCoordinationErrorV1::RejectedDetail(
                "stage_and_rollback: rollback request is invalid".to_owned(),
            )
        })?;
        self.owner
            .rollback(request)
            .await
            .map_err(SemanticActivationCoordinationErrorV1::from)
            .inspect_err(crate::hotpath_observe::semantic_coordination_error)
    }
}

impl SemanticActivationCoordinationPort for ProductionSemanticActivationCoordinatorV1 {
    type ConfigurationState = ConfigurationCurrentStateV1;
    type AcceptedProfile = AcceptedRetrievalProfileV1;
    type RuntimeCompatibility = RetrievalRuntimeCompatibilityV1;
    type ConfigurationPin = SemanticConfigurationPinV1;
    type MutationCapability = RetrievalProfileMutationCapabilityV1;
    type ProfileCas = RetrievalProfileCasV1;
    type CentralMutation = DirectConfigurationMutation;
    type ActivationReceipt = SemanticActivationReceiptV1;
    type RollbackReceipt = SemanticRollbackReceiptV1;
    type ProfileState = RetrievalProfileStateSnapshotV1;
    type MutationAuthority = ConfigurationMutationAuthority;
    type PreviewOutcome = ConfigurationDirectCommitOutcomeV1;

    fn bootstrap_query_profile<'a>(
        &'a self,
        configuration: Self::ConfigurationState,
        accepted_query: Self::AcceptedProfile,
        runtime: &'a Self::RuntimeCompatibility,
    ) -> Pin<Box<dyn Future<Output = Result<(), SemanticActivationCoordinationErrorV1>> + Send + 'a>>
    {
        Box::pin(async move {
            ProductionSemanticActivationCoordinatorV1::bootstrap_query_profile(
                self,
                configuration,
                accepted_query,
                runtime,
            )
            .await
        })
    }

    fn current_profile_state<'a>(
        &'a self,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Self::ProfileState, SemanticActivationCoordinationErrorV1>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            ProductionSemanticActivationCoordinatorV1::current_profile_state(self).await
        })
    }

    fn preview_central_mutation<'a>(
        &'a self,
        authority: &'a Self::MutationAuthority,
        mutation: &'a Self::CentralMutation,
        expected_revision: &'a ConfigurationRevisionId,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Self::PreviewOutcome, SemanticActivationCoordinationErrorV1>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            ProductionSemanticActivationCoordinatorV1::preview_central_mutation(
                self,
                authority,
                mutation,
                expected_revision,
            )
            .await
        })
    }

    fn stage_and_activate<'a>(
        &'a self,
        base_configuration: Self::ConfigurationPin,
        result_configuration: Self::ConfigurationState,
        capability: &'a Self::MutationCapability,
        expected: Self::ProfileCas,
        candidate: Self::AcceptedProfile,
        current_runtime: &'a Self::RuntimeCompatibility,
        candidate_runtime: &'a Self::RuntimeCompatibility,
        central_mutation: Self::CentralMutation,
        freshness_vector_digest: ManifestDigest,
        now: UtcMicros,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<Self::ActivationReceipt, SemanticActivationCoordinationErrorV1>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            ProductionSemanticActivationCoordinatorV1::stage_and_activate(
                self,
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
        })
    }

    fn stage_and_rollback<'a>(
        &'a self,
        base_configuration: Self::ConfigurationPin,
        result_configuration: Self::ConfigurationState,
        capability: &'a Self::MutationCapability,
        expected: Self::ProfileCas,
        restored_runtime: &'a Self::RuntimeCompatibility,
        central_mutation: Self::CentralMutation,
        trigger: String,
        freshness_vector_digest: ManifestDigest,
        now: UtcMicros,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<Self::RollbackReceipt, SemanticActivationCoordinationErrorV1>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            ProductionSemanticActivationCoordinatorV1::stage_and_rollback(
                self,
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
        })
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

/// Every stage of a linked transition answers a configuration refusal with the
/// same `Rejected` category, so the refusal has to carry the stage that
/// produced it or an operator cannot reach it from the public problem.
fn configuration_error_at(
    stage: &'static str,
) -> impl Fn(SemanticConfigurationBackendErrorV1) -> SemanticActivationCoordinationErrorV1 {
    move |error| {
        let mapped = match error {
            SemanticConfigurationBackendErrorV1::Unavailable => {
                SemanticActivationCoordinationErrorV1::Unavailable
            }
            SemanticConfigurationBackendErrorV1::Rejected => {
                SemanticActivationCoordinationErrorV1::RejectedDetail(format!(
                    "{stage}: retrieval configuration transition was rejected"
                ))
            }
            SemanticConfigurationBackendErrorV1::RejectedAt(inner) => {
                SemanticActivationCoordinationErrorV1::RejectedDetail(format!("{stage}: {inner}"))
            }
            SemanticConfigurationBackendErrorV1::Conflict => {
                SemanticActivationCoordinationErrorV1::Conflict
            }
        };
        crate::hotpath_observe::semantic_coordination_error(&mapped);
        mapped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_production_mount<T: SemanticRuntimeIntegrationPortV1 + Send + Sync>() {}

    fn assert_activation_port<T: SemanticActivationCoordinationPort + Send + Sync>() {}

    #[test]
    fn production_coordinator_is_a_concrete_project_runtime_mount() {
        assert_production_mount::<ProductionSemanticActivationCoordinatorV1>();
        assert_activation_port::<ProductionSemanticActivationCoordinatorV1>();
        std::hint::black_box(ProductionSemanticActivationCoordinatorV1::new);
    }
}
