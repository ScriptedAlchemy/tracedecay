use std::sync::Arc;

use tracedecay_domain::{ManifestDigest, UtcMicros};

use super::{
    RegisteredSemanticAcceptedProfileAuthorityV1, SemanticAcceptedProfileAuthorityErrorV1,
    SemanticAcceptedProfileAuthorityPortV1, SemanticActivationCoordinationErrorV1,
    SemanticActivationReceiptV1, SemanticRollbackReceiptV1,
};
use crate::application::configuration::{
    ConfigurationCurrentStateV1, ConfigurationMutationAuthority, ConfigurationMutationReceipt,
    DirectConfigurationMutation, ProjectConfigurationRuntime,
};
use crate::config::retrieval::RetrievalProfileCasV1;
use crate::search_eval::DirectEvaluationReportV1;

/// Production application operation for the linked Plan 20 configuration and
/// semantic-profile transition. Profile/evaluation/runtime values are resolved
/// from durable accepted authority by immutable digest; transport callers
/// cannot submit a `pass` label or executable profile directly.
pub(crate) struct ProductionSemanticConfigurationOperationV1 {
    configuration: Arc<ProjectConfigurationRuntime>,
    accepted_profiles: Arc<RegisteredSemanticAcceptedProfileAuthorityV1>,
}

impl ProductionSemanticConfigurationOperationV1 {
    pub(crate) fn new(
        configuration: Arc<ProjectConfigurationRuntime>,
        accepted_profiles: Arc<RegisteredSemanticAcceptedProfileAuthorityV1>,
    ) -> Self {
        Self {
            configuration,
            accepted_profiles,
        }
    }

    pub(crate) async fn publish_evaluated_profile(
        &self,
        report: DirectEvaluationReportV1,
        accepted_profile: crate::config::retrieval::AcceptedRetrievalProfileV1,
        runtime: crate::config::retrieval::RetrievalRuntimeCompatibilityV1,
        freshness_vector_digest: ManifestDigest,
    ) -> Result<(), SemanticActivationCoordinationErrorV1> {
        let bootstrap_pr9 = accepted_profile.is_exact_pr9_fallback();
        let profile_digest = accepted_profile.profile_digest().clone();
        self.accepted_profiles
            .publish(report, accepted_profile, runtime, freshness_vector_digest)
            .await
            .map_err(map_authority_error)?;
        if bootstrap_pr9 {
            let configuration = current_configuration_state(&self.configuration).await?;
            self.bootstrap_pr9(configuration, &profile_digest).await?;
        }
        Ok(())
    }

    pub(crate) async fn bootstrap_pr9(
        &self,
        configuration: ConfigurationCurrentStateV1,
        accepted_profile_digest: &ManifestDigest,
    ) -> Result<(), SemanticActivationCoordinationErrorV1> {
        let accepted = self
            .accepted_profiles
            .resolve(accepted_profile_digest)
            .await
            .map_err(map_authority_error)?;
        self.configuration
            .bootstrap_pr9_retrieval_profile(
                configuration,
                accepted.accepted_profile,
                &accepted.runtime,
            )
            .await
    }

    pub(crate) async fn activate(
        &self,
        request: SemanticProtectedActivationOperationV1,
    ) -> Result<SemanticAppliedActivationV1, SemanticActivationCoordinationErrorV1> {
        let coordinator = self
            .configuration
            .semantic_activation_coordinator()
            .ok_or(SemanticActivationCoordinationErrorV1::Unavailable)?;
        let state = coordinator
            .current_profile_state()
            .await?
            .into_state()
            .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
        let expected = RetrievalProfileCasV1 {
            expected_configuration_revision: state.configuration_revision().clone(),
            expected_active_digest: state.active().profile_digest().clone(),
            expected_rollback_digest: state
                .rollback_profile()
                .map(|profile| profile.profile_digest().clone()),
        };
        self.configuration
            .authorize_semantic_configuration_mutation(
                request.authority.clone(),
                &expected.expected_configuration_revision,
                request.now,
            )
            .await?;
        let candidate = self
            .accepted_profiles
            .resolve(&request.selected_profile.accepted_profile_digest)
            .await
            .map_err(map_authority_error)?;
        if candidate.accepted_profile.profile_digest()
            != &request.selected_profile.accepted_profile_digest
            || candidate
                .accepted_profile
                .compatibility()
                .semantic
                .as_ref()
                .and_then(|pins| {
                    pins.artifact_manifest_digest
                        .as_str()
                        .strip_prefix("sha256:")
                })
                != Some(request.selected_profile.artifact_digest.as_str())
        {
            return Err(SemanticActivationCoordinationErrorV1::Rejected);
        }
        if expected.expected_rollback_digest.as_ref()
            == Some(&request.selected_profile.accepted_profile_digest)
        {
            let applied = self
                .rollback(SemanticProtectedRollbackOperationV1 {
                    authority: request.authority,
                    central_mutation: request.central_mutation,
                    trigger: "configuration_semantic_profile_restored".to_owned(),
                    now: request.now,
                })
                .await?;
            return Ok(SemanticAppliedActivationV1 {
                configuration_receipt: applied.configuration_receipt,
                semantic_receipt: applied
                    .semantic_receipt
                    .and_then(|receipt| receipt.restored_activation),
            });
        }
        if request.selected_profile.accepted_profile_digest == expected.expected_active_digest {
            let receipt = self
                .configuration
                .client()
                .mutate_direct(
                    request.authority,
                    request.central_mutation,
                    expected.expected_configuration_revision,
                )
                .await
                .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
            return Ok(SemanticAppliedActivationV1 {
                configuration_receipt: receipt,
                semantic_receipt: None,
            });
        }
        let current = self
            .accepted_profiles
            .resolve(&expected.expected_active_digest)
            .await
            .map_err(map_authority_error)?;
        let base_configuration = current_configuration_state(&self.configuration).await?;
        let base_pin = super::SemanticConfigurationPinV1::from_current(&base_configuration)
            .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
        let preview = coordinator
            .preview_central_mutation(
                &request.authority,
                &request.central_mutation,
                &expected.expected_configuration_revision,
            )
            .await?;
        let semantic_receipt = self
            .configuration
            .stage_and_activate_semantic(
                base_pin,
                preview.current,
                request.authority,
                expected,
                candidate.accepted_profile,
                &current.runtime,
                &candidate.runtime,
                request.central_mutation,
                candidate.freshness_vector_digest,
                request.now,
            )
            .await?;
        Ok(SemanticAppliedActivationV1 {
            configuration_receipt: preview.receipt,
            semantic_receipt: Some(semantic_receipt),
        })
    }

    pub(crate) async fn rollback(
        &self,
        request: SemanticProtectedRollbackOperationV1,
    ) -> Result<SemanticAppliedRollbackV1, SemanticActivationCoordinationErrorV1> {
        let coordinator = self
            .configuration
            .semantic_activation_coordinator()
            .ok_or(SemanticActivationCoordinationErrorV1::Unavailable)?;
        let state = coordinator
            .current_profile_state()
            .await?
            .into_state()
            .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
        let expected = RetrievalProfileCasV1 {
            expected_configuration_revision: state.configuration_revision().clone(),
            expected_active_digest: state.active().profile_digest().clone(),
            expected_rollback_digest: state
                .rollback_profile()
                .map(|profile| profile.profile_digest().clone()),
        };
        self.configuration
            .authorize_semantic_configuration_mutation(
                request.authority.clone(),
                &expected.expected_configuration_revision,
                request.now,
            )
            .await?;
        if state.active().compatibility().semantic.is_none() {
            let receipt = self
                .configuration
                .client()
                .mutate_direct(
                    request.authority,
                    request.central_mutation,
                    expected.expected_configuration_revision,
                )
                .await
                .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
            return Ok(SemanticAppliedRollbackV1 {
                configuration_receipt: receipt,
                semantic_receipt: None,
            });
        }
        let restored_digest = expected
            .expected_rollback_digest
            .as_ref()
            .ok_or(SemanticActivationCoordinationErrorV1::Rejected)?;
        let restored = self
            .accepted_profiles
            .resolve(restored_digest)
            .await
            .map_err(map_authority_error)?;
        let base_configuration = current_configuration_state(&self.configuration).await?;
        let base_pin = super::SemanticConfigurationPinV1::from_current(&base_configuration)
            .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
        let preview = coordinator
            .preview_central_mutation(
                &request.authority,
                &request.central_mutation,
                &expected.expected_configuration_revision,
            )
            .await?;
        let semantic_receipt = self
            .configuration
            .stage_and_rollback_semantic(
                base_pin,
                preview.current,
                request.authority,
                expected,
                &restored.runtime,
                request.central_mutation,
                request.trigger,
                restored.freshness_vector_digest,
                request.now,
            )
            .await?;
        Ok(SemanticAppliedRollbackV1 {
            configuration_receipt: preview.receipt,
            semantic_receipt: Some(semantic_receipt),
        })
    }
}

pub(crate) struct SemanticProtectedActivationOperationV1 {
    pub authority: ConfigurationMutationAuthority,
    pub selected_profile: crate::config::SemanticProfileSelection,
    pub central_mutation: DirectConfigurationMutation,
    pub now: UtcMicros,
}

pub(crate) struct SemanticProtectedRollbackOperationV1 {
    pub authority: ConfigurationMutationAuthority,
    pub central_mutation: DirectConfigurationMutation,
    pub trigger: String,
    pub now: UtcMicros,
}

pub(crate) struct SemanticAppliedActivationV1 {
    pub configuration_receipt: ConfigurationMutationReceipt,
    pub semantic_receipt: Option<SemanticActivationReceiptV1>,
}

pub(crate) struct SemanticAppliedRollbackV1 {
    pub configuration_receipt: ConfigurationMutationReceipt,
    pub semantic_receipt: Option<SemanticRollbackReceiptV1>,
}

async fn current_configuration_state(
    runtime: &ProjectConfigurationRuntime,
) -> Result<ConfigurationCurrentStateV1, SemanticActivationCoordinationErrorV1> {
    let current = runtime
        .client()
        .current()
        .await
        .map_err(|_| SemanticActivationCoordinationErrorV1::Unavailable)?;
    Ok(ConfigurationCurrentStateV1 {
        revision_id: current.revision_id,
        snapshot: current.snapshot,
    })
}

fn map_authority_error(
    error: SemanticAcceptedProfileAuthorityErrorV1,
) -> SemanticActivationCoordinationErrorV1 {
    match error {
        SemanticAcceptedProfileAuthorityErrorV1::Unavailable => {
            SemanticActivationCoordinationErrorV1::Unavailable
        }
        SemanticAcceptedProfileAuthorityErrorV1::Rejected => {
            SemanticActivationCoordinationErrorV1::Rejected
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_requires_durable_authority_and_plan20_runtime() {
        let _constructor = ProductionSemanticConfigurationOperationV1::new;
    }
}
