//! Typed semantic-activation attachment for the configuration control plane.
//!
//! `tracedecay-configuration` stores these payloads behind type-erased slots
//! so it does not name retrieval or search-eval types. This module is the
//! single typed installer and caller surface.

use std::sync::Arc;

use tracedecay_application::{
    SemanticActivationCoordinationErrorV1, SemanticActivationCoordinationPort,
};
use tracedecay_configuration::{
    ConfigurationCurrentStateV1, ConfigurationError, ConfigurationMutationAuthority,
    DirectConfigurationMutation, ProjectConfigurationRuntime,
};
use tracedecay_domain::configuration::{
    ConfigurationMutationEffectV1, ConfigurationMutationOperationV1, ConfigurationMutationSinkV1,
    ConfigurationRevisionId,
};
use tracedecay_domain::errors::Result;
use tracedecay_domain::{ManifestDigest, UtcMicros};
use tracedecay_global_db::configuration::store::ConfigurationDirectCommitOutcomeV1;

use super::{
    ProductionSemanticActivationCoordinatorV1, ProductionSemanticRetrievalConfigurationStoreV1,
    SemanticActivationReceiptV1, SemanticConfigurationPinV1, SemanticRollbackReceiptV1,
};
use crate::config::retrieval::{
    AcceptedRetrievalProfileV1, RetrievalProfileCasV1, RetrievalProfileMutationCapabilityV1,
    RetrievalProfileStateSnapshotV1, RetrievalRuntimeCompatibilityV1,
};

/// Production binding of [`SemanticActivationCoordinationPort`] for the
/// retained configuration runtime. Associated types stay in this crate so
/// `tracedecay-application` does not name retrieval or store payloads.
#[allow(clippy::type_complexity)]
pub type InstalledSemanticActivationCoordination = dyn SemanticActivationCoordinationPort<
        ConfigurationState = ConfigurationCurrentStateV1,
        AcceptedProfile = AcceptedRetrievalProfileV1,
        RuntimeCompatibility = RetrievalRuntimeCompatibilityV1,
        ConfigurationPin = SemanticConfigurationPinV1,
        MutationCapability = RetrievalProfileMutationCapabilityV1,
        ProfileCas = RetrievalProfileCasV1,
        CentralMutation = DirectConfigurationMutation,
        ActivationReceipt = SemanticActivationReceiptV1,
        RollbackReceipt = SemanticRollbackReceiptV1,
        ProfileState = RetrievalProfileStateSnapshotV1,
        MutationAuthority = ConfigurationMutationAuthority,
        PreviewOutcome = ConfigurationDirectCommitOutcomeV1,
    > + Send
    + Sync;

pub trait ProjectSemanticActivationExt {
    fn install_semantic_runtime(
        &self,
        runtime: Arc<ProductionSemanticActivationCoordinatorV1>,
    ) -> Result<()>;

    fn semantic_configuration_inventory_authority(
        &self,
    ) -> Option<ProductionSemanticRetrievalConfigurationStoreV1>;

    fn semantic_activation_coordinator(
        &self,
    ) -> Option<Arc<InstalledSemanticActivationCoordination>>;

    fn authorize_semantic_configuration_mutation(
        &self,
        authority: ConfigurationMutationAuthority,
        expected_revision: &ConfigurationRevisionId,
        now: UtcMicros,
    ) -> impl std::future::Future<
        Output = std::result::Result<(), SemanticActivationCoordinationErrorV1>,
    > + Send;

    fn bootstrap_query_retrieval_profile(
        &self,
        configuration: ConfigurationCurrentStateV1,
        accepted_query: AcceptedRetrievalProfileV1,
        runtime: &RetrievalRuntimeCompatibilityV1,
    ) -> impl std::future::Future<
        Output = std::result::Result<(), SemanticActivationCoordinationErrorV1>,
    > + Send;

    #[allow(clippy::too_many_arguments)]
    fn stage_and_activate_semantic(
        &self,
        base_configuration: SemanticConfigurationPinV1,
        result_configuration: ConfigurationCurrentStateV1,
        authority: ConfigurationMutationAuthority,
        expected: RetrievalProfileCasV1,
        candidate: AcceptedRetrievalProfileV1,
        current_runtime: &RetrievalRuntimeCompatibilityV1,
        candidate_runtime: &RetrievalRuntimeCompatibilityV1,
        central_mutation: DirectConfigurationMutation,
        freshness_vector_digest: ManifestDigest,
        now: UtcMicros,
    ) -> impl std::future::Future<
        Output = std::result::Result<
            SemanticActivationReceiptV1,
            SemanticActivationCoordinationErrorV1,
        >,
    > + Send;

    #[allow(clippy::too_many_arguments)]
    fn stage_and_rollback_semantic(
        &self,
        base_configuration: SemanticConfigurationPinV1,
        result_configuration: ConfigurationCurrentStateV1,
        authority: ConfigurationMutationAuthority,
        expected: RetrievalProfileCasV1,
        restored_runtime: &RetrievalRuntimeCompatibilityV1,
        central_mutation: DirectConfigurationMutation,
        trigger: String,
        freshness_vector_digest: ManifestDigest,
        now: UtcMicros,
    ) -> impl std::future::Future<
        Output = std::result::Result<
            SemanticRollbackReceiptV1,
            SemanticActivationCoordinationErrorV1,
        >,
    > + Send;
}

impl ProjectSemanticActivationExt for ProjectConfigurationRuntime {
    fn install_semantic_runtime(
        &self,
        runtime: Arc<ProductionSemanticActivationCoordinatorV1>,
    ) -> Result<()> {
        let inventory = runtime.configuration_inventory_authority();
        self.install_semantic_inventory(inventory);
        self.install_semantic_activation(runtime);
        Ok(())
    }

    fn semantic_configuration_inventory_authority(
        &self,
    ) -> Option<ProductionSemanticRetrievalConfigurationStoreV1> {
        self.semantic_inventory()
    }

    fn semantic_activation_coordinator(
        &self,
    ) -> Option<Arc<InstalledSemanticActivationCoordination>> {
        self.semantic_activation::<ProductionSemanticActivationCoordinatorV1>()
            .map(|runtime| runtime as Arc<InstalledSemanticActivationCoordination>)
    }

    async fn authorize_semantic_configuration_mutation(
        &self,
        authority: ConfigurationMutationAuthority,
        expected_revision: &ConfigurationRevisionId,
        now: UtcMicros,
    ) -> std::result::Result<(), SemanticActivationCoordinationErrorV1> {
        retrieval_profile_mutation_capability(self, authority, expected_revision, now)
            .await
            .map(|_| ())
    }

    async fn bootstrap_query_retrieval_profile(
        &self,
        configuration: ConfigurationCurrentStateV1,
        accepted_query: AcceptedRetrievalProfileV1,
        runtime: &RetrievalRuntimeCompatibilityV1,
    ) -> std::result::Result<(), SemanticActivationCoordinationErrorV1> {
        SemanticActivationCoordinationPort::bootstrap_query_profile(
            self.semantic_activation_coordinator()
                .ok_or(SemanticActivationCoordinationErrorV1::Unavailable)?
                .as_ref(),
            configuration,
            accepted_query,
            runtime,
        )
        .await
    }

    async fn stage_and_activate_semantic(
        &self,
        base_configuration: SemanticConfigurationPinV1,
        result_configuration: ConfigurationCurrentStateV1,
        authority: ConfigurationMutationAuthority,
        expected: RetrievalProfileCasV1,
        candidate: AcceptedRetrievalProfileV1,
        current_runtime: &RetrievalRuntimeCompatibilityV1,
        candidate_runtime: &RetrievalRuntimeCompatibilityV1,
        central_mutation: DirectConfigurationMutation,
        freshness_vector_digest: ManifestDigest,
        now: UtcMicros,
    ) -> std::result::Result<SemanticActivationReceiptV1, SemanticActivationCoordinationErrorV1>
    {
        let capability = retrieval_profile_mutation_capability(
            self,
            authority,
            &expected.expected_configuration_revision,
            now,
        )
        .await?;
        SemanticActivationCoordinationPort::stage_and_activate(
            self.semantic_activation_coordinator()
                .ok_or(SemanticActivationCoordinationErrorV1::Unavailable)?
                .as_ref(),
            base_configuration,
            result_configuration,
            &capability,
            expected,
            candidate,
            current_runtime,
            candidate_runtime,
            central_mutation,
            freshness_vector_digest,
            now,
        )
        .await
    }

    async fn stage_and_rollback_semantic(
        &self,
        base_configuration: SemanticConfigurationPinV1,
        result_configuration: ConfigurationCurrentStateV1,
        authority: ConfigurationMutationAuthority,
        expected: RetrievalProfileCasV1,
        restored_runtime: &RetrievalRuntimeCompatibilityV1,
        central_mutation: DirectConfigurationMutation,
        trigger: String,
        freshness_vector_digest: ManifestDigest,
        now: UtcMicros,
    ) -> std::result::Result<SemanticRollbackReceiptV1, SemanticActivationCoordinationErrorV1> {
        let capability = retrieval_profile_mutation_capability(
            self,
            authority,
            &expected.expected_configuration_revision,
            now,
        )
        .await?;
        SemanticActivationCoordinationPort::stage_and_rollback(
            self.semantic_activation_coordinator()
                .ok_or(SemanticActivationCoordinationErrorV1::Unavailable)?
                .as_ref(),
            base_configuration,
            result_configuration,
            &capability,
            expected,
            restored_runtime,
            central_mutation,
            trigger,
            freshness_vector_digest,
            now,
        )
        .await
    }
}

async fn retrieval_profile_mutation_capability(
    runtime: &ProjectConfigurationRuntime,
    authority: ConfigurationMutationAuthority,
    expected_revision: &ConfigurationRevisionId,
    now: UtcMicros,
) -> std::result::Result<RetrievalProfileMutationCapabilityV1, SemanticActivationCoordinationErrorV1>
{
    let current = runtime
        .installed_mutation_authorization()
        .map_err(|_| SemanticActivationCoordinationErrorV1::Unavailable)?
        .recheck(
            &authority.receipt,
            ConfigurationMutationOperationV1::DirectMutation,
            expected_revision,
            ConfigurationMutationSinkV1::ConfigurationStore,
            ConfigurationMutationEffectV1::CommitConfigurationRevision,
            now,
        )
        .await
        .map_err(|error| match error {
            ConfigurationError::Unavailable => SemanticActivationCoordinationErrorV1::Unavailable,
            _ => SemanticActivationCoordinationErrorV1::Rejected,
        })?;
    RetrievalProfileMutationCapabilityV1::from_current_authorization(authority, current)
        .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)
}
