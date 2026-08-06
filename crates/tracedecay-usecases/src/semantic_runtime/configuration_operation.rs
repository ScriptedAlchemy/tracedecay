use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracedecay_application::ResolvedScope;
use tracedecay_domain::{
    CalibrationProfileId, CodeGenerationId, DiversityPolicy, DiversityPolicyId, FusionProfile,
    FusionProfileId, ManifestDigest, RerankPolicy, RerankPolicyId, RetrievalBudget, RetrieverKind,
    ScoreDomainCalibrationV1, ScoreDomainId, UtcMicros, VectorGenerationIdV1,
};

use super::{
    RegisteredSemanticAcceptedProfileAuthorityV1, SemanticAcceptedProfileAuthorityErrorV1,
    SemanticAcceptedProfileAuthorityPortV1, SemanticActivationCoordinationErrorV1,
    SemanticRuntimeFuture,
};
use crate::config::retrieval::{
    AcceptedRetrievalProfileV1, PassingRetrievalEvaluationV1, RetrievalCompatibilityPinsV1,
    RetrievalProfileCasV1, RetrievalRuntimeCompatibilityV1,
};
use crate::configuration::{
    ConfigurationCurrentStateV1, ConfigurationMutationAuthority, ConfigurationMutationReceipt,
    DirectConfigurationMutation, ProjectConfigurationRuntime,
};
use tracedecay_search_eval::{
    DirectActivationEvaluationV1, DirectEvaluatedProfileMaterialV1, DirectEvaluationReportV1,
};

use super::accepted_profile_authority::SemanticEvaluationPublicationIdentityV1;

/// Unevaluated fusion material. No evaluation-result anchor is accepted from
/// the caller; production derives it from the genuine direct-evaluator PASS.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticEvaluationFusionCandidateV1 {
    pub profile_id: FusionProfileId,
    pub calibrations: BTreeMap<RetrieverKind, CalibrationProfileId>,
    pub score_domain_calibrations: BTreeMap<ScoreDomainId, ScoreDomainCalibrationV1>,
    pub weights_micros: BTreeMap<RetrieverKind, u32>,
    pub diversity_policy_id: DiversityPolicyId,
    pub rerank_policy_id: Option<RerankPolicyId>,
    pub retrieval_budget: RetrievalBudget,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticEvaluationDiversityCandidateV1 {
    pub policy_id: DiversityPolicyId,
    pub per_source_namespace: Option<u32>,
    pub per_source_instance: Option<u32>,
    pub per_repository: Option<u32>,
    pub per_file: Option<u32>,
    pub per_session_or_thread: Option<u32>,
    pub per_copy_cluster: Option<u32>,
    pub per_evidence_role: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticEvaluationRerankCandidateV1 {
    pub policy_id: RerankPolicyId,
    pub max_candidates: u32,
    pub max_input_bytes: u64,
    pub max_input_tokens: u64,
    pub max_work_units: u64,
    pub max_model_invocations: u32,
    pub deadline_micros: Option<u64>,
}

/// Unevaluated profile material. A direct-evaluator report or evaluation
/// anchor is deliberately not representable here.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticEvaluationProfileCandidateV1 {
    pub evaluated_profile_id: String,
    pub profile: SemanticEvaluationFusionCandidateV1,
    pub diversity: SemanticEvaluationDiversityCandidateV1,
    pub rerank: Option<SemanticEvaluationRerankCandidateV1>,
    pub compatibility: RetrievalCompatibilityPinsV1,
}

/// Exact mounted authority observed on both sides of a direct evaluation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticEvaluationPublicationSnapshotV1 {
    pub project_root: PathBuf,
    pub scope: ResolvedScope,
    pub code_generation: CodeGenerationId,
    pub code_source_manifest_digest: ManifestDigest,
    pub code_snapshot_digest: ManifestDigest,
    pub semantic_source_generation: Option<CodeGenerationId>,
    pub vector_state_revision: Option<i64>,
    pub vector_generation_id: Option<VectorGenerationIdV1>,
    pub runtime: RetrievalRuntimeCompatibilityV1,
}

pub trait SemanticEvaluationPublicationSnapshotPortV1: Send + Sync {
    fn current(
        &self,
    ) -> SemanticRuntimeFuture<
        '_,
        Result<SemanticEvaluationPublicationSnapshotV1, SemanticActivationCoordinationErrorV1>,
    >;

    fn evaluate_default_candidate<'a>(
        &'a self,
        repo_root: &'a Path,
        evaluated_profile_id: &'a str,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<DirectActivationEvaluationV1, SemanticActivationCoordinationErrorV1>,
    >;

    /// Commit `publication` only while `expected` is still the exact mounted
    /// code/vector/runtime snapshot. The implementation owns the production
    /// snapshot guard or compare-and-swap token and must keep it valid through
    /// `publication.commit(expected)`.
    fn publish_if_current<'a>(
        &'a self,
        expected: &'a SemanticEvaluationPublicationSnapshotV1,
        publication: SemanticEvaluationAuthorityPublicationV1,
    ) -> SemanticRuntimeFuture<'a, Result<(), SemanticActivationCoordinationErrorV1>>;
}

#[derive(Clone, Debug)]
pub struct SemanticEvaluatedProfilePublicationV1 {
    pub report: DirectEvaluationReportV1,
    pub accepted_profile: AcceptedRetrievalProfileV1,
    pub snapshot: SemanticEvaluationPublicationSnapshotV1,
}

/// Closed durable effect supplied to the snapshot authority after the genuine
/// evaluator has produced a PASS. Runtime and freshness bindings are taken
/// only from the snapshot protected by the authority's CAS/guard.
pub struct SemanticEvaluationAuthorityPublicationV1 {
    configuration: Arc<ProjectConfigurationRuntime>,
    accepted_profiles: Arc<RegisteredSemanticAcceptedProfileAuthorityV1>,
    report: DirectEvaluationReportV1,
    accepted_profile: AcceptedRetrievalProfileV1,
}

impl SemanticEvaluationAuthorityPublicationV1 {
    pub async fn commit(
        self,
        expected: &SemanticEvaluationPublicationSnapshotV1,
    ) -> Result<(), SemanticActivationCoordinationErrorV1> {
        self.accepted_profile
            .executable_under(&expected.runtime)
            .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
        let bootstrap_query = self.accepted_profile.is_exact_query_fallback();
        let profile_digest = self.accepted_profile.profile_digest().clone();
        self.accepted_profiles
            .publish(
                &expected.project_root,
                self.report,
                self.accepted_profile,
                expected.runtime.clone(),
                SemanticEvaluationPublicationIdentityV1 {
                    scope_digest: expected.scope.scope_digest.clone(),
                    code_generation: expected.code_generation.clone(),
                    code_source_manifest_digest: expected.code_source_manifest_digest.clone(),
                    code_snapshot_digest: expected.code_snapshot_digest.clone(),
                    semantic_source_generation: expected.semantic_source_generation.clone(),
                    vector_state_revision: expected.vector_state_revision,
                    vector_generation_id: expected.vector_generation_id.clone(),
                },
                expected.code_snapshot_digest.clone(),
            )
            .await
            .map_err(map_authority_error)?;
        if bootstrap_query {
            let configuration = current_configuration_state(&self.configuration).await?;
            let accepted = self
                .accepted_profiles
                .resolve(&profile_digest)
                .await
                .map_err(map_authority_error)?;
            self.configuration
                .bootstrap_query_retrieval_profile(
                    configuration,
                    accepted.accepted_profile,
                    &accepted.runtime,
                )
                .await?;
        }
        Ok(())
    }
}

/// Production application operation for the linked Plan 20 configuration and
/// semantic-profile transition. Profile/evaluation/runtime values are resolved
/// from durable accepted authority by immutable digest; transport callers
/// cannot submit a `pass` label or executable profile directly.
pub struct ProductionSemanticConfigurationOperationV1 {
    configuration: Arc<ProjectConfigurationRuntime>,
    accepted_profiles: Arc<RegisteredSemanticAcceptedProfileAuthorityV1>,
}

impl ProductionSemanticConfigurationOperationV1 {
    pub fn new(
        configuration: Arc<ProjectConfigurationRuntime>,
        accepted_profiles: Arc<RegisteredSemanticAcceptedProfileAuthorityV1>,
    ) -> Self {
        Self {
            configuration,
            accepted_profiles,
        }
    }

    /// Run the genuine checked-in direct evaluator and publish only when the
    /// exact mounted scope, source generation, snapshot, and runtime remain
    /// unchanged through evaluation.
    pub async fn evaluate_and_publish_profile(
        &self,
        snapshot_authority: &dyn SemanticEvaluationPublicationSnapshotPortV1,
        repo_root: &Path,
        candidate: SemanticEvaluationProfileCandidateV1,
    ) -> Result<SemanticEvaluatedProfilePublicationV1, SemanticActivationCoordinationErrorV1> {
        let before = snapshot_authority.current().await?;
        validate_evaluation_snapshot(repo_root, &before, &candidate)?;

        let (report, evaluated_material) = snapshot_authority
            .evaluate_default_candidate(repo_root, &candidate.evaluated_profile_id)
            .await?
            .into_parts();
        if !candidate_matches_evaluated_material(&candidate, &evaluated_material) {
            return Err(SemanticActivationCoordinationErrorV1::Rejected);
        }
        let evaluation =
            PassingRetrievalEvaluationV1::from_report(&report, &candidate.evaluated_profile_id)
                .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
        let evaluation_anchor = evaluation.evaluation_anchor().clone();
        let evaluated_profile = evaluated_material.profile;
        let profile = FusionProfile {
            profile_id: evaluated_profile.profile_id,
            evaluation_result_anchor: evaluation_anchor.clone(),
            calibrations: evaluated_profile.calibrations,
            score_domain_calibrations: evaluated_profile.score_domain_calibrations,
            weights_micros: evaluated_profile.weights_micros,
            diversity_policy_id: evaluated_profile.diversity_policy_id,
            rerank_policy_id: evaluated_profile.rerank_policy_id,
            retrieval_budget: evaluated_profile.retrieval_budget,
        };
        let evaluated_diversity = evaluated_material.diversity;
        let diversity = DiversityPolicy {
            policy_id: evaluated_diversity.policy_id,
            evaluation_result_anchor: Some(evaluation_anchor.clone()),
            per_source_namespace: evaluated_diversity.per_source_namespace,
            per_source_instance: evaluated_diversity.per_source_instance,
            per_repository: evaluated_diversity.per_repository,
            per_file: evaluated_diversity.per_file,
            per_session_or_thread: evaluated_diversity.per_session_or_thread,
            per_copy_cluster: evaluated_diversity.per_copy_cluster,
            per_evidence_role: evaluated_diversity.per_evidence_role,
        };
        let rerank = evaluated_material.rerank.map(|rerank| RerankPolicy {
            policy_id: rerank.policy_id,
            evaluation_result_anchor: evaluation_anchor,
            max_candidates: rerank.max_candidates,
            max_input_bytes: rerank.max_input_bytes,
            max_input_tokens: rerank.max_input_tokens,
            max_work_units: rerank.max_work_units,
            max_model_invocations: rerank.max_model_invocations,
            deadline_micros: rerank.deadline_micros,
        });
        let accepted_profile = AcceptedRetrievalProfileV1::new(
            profile,
            diversity,
            rerank,
            candidate.compatibility,
            evaluation,
        )
        .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
        accepted_profile
            .executable_under(&before.runtime)
            .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;

        snapshot_authority
            .publish_if_current(
                &before,
                SemanticEvaluationAuthorityPublicationV1 {
                    configuration: Arc::clone(&self.configuration),
                    accepted_profiles: Arc::clone(&self.accepted_profiles),
                    report: report.clone(),
                    accepted_profile: accepted_profile.clone(),
                },
            )
            .await?;
        Ok(SemanticEvaluatedProfilePublicationV1 {
            report,
            accepted_profile,
            snapshot: before,
        })
    }

    pub async fn activate(
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
        self.configuration
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
        })
    }

    pub async fn rollback(
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
        self.configuration
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
        })
    }
}

fn candidate_matches_evaluated_material(
    candidate: &SemanticEvaluationProfileCandidateV1,
    evaluated: &DirectEvaluatedProfileMaterialV1,
) -> bool {
    candidate.profile.profile_id == evaluated.profile.profile_id
        && candidate.profile.calibrations == evaluated.profile.calibrations
        && candidate.profile.score_domain_calibrations
            == evaluated.profile.score_domain_calibrations
        && candidate.profile.weights_micros == evaluated.profile.weights_micros
        && candidate.profile.diversity_policy_id == evaluated.profile.diversity_policy_id
        && candidate.profile.rerank_policy_id == evaluated.profile.rerank_policy_id
        && candidate.profile.retrieval_budget == evaluated.profile.retrieval_budget
        && candidate.diversity.policy_id == evaluated.diversity.policy_id
        && candidate.diversity.per_source_namespace == evaluated.diversity.per_source_namespace
        && candidate.diversity.per_source_instance == evaluated.diversity.per_source_instance
        && candidate.diversity.per_repository == evaluated.diversity.per_repository
        && candidate.diversity.per_file == evaluated.diversity.per_file
        && candidate.diversity.per_session_or_thread == evaluated.diversity.per_session_or_thread
        && candidate.diversity.per_copy_cluster == evaluated.diversity.per_copy_cluster
        && candidate.diversity.per_evidence_role == evaluated.diversity.per_evidence_role
        && match (&candidate.rerank, &evaluated.rerank) {
            (None, None) => true,
            (Some(candidate), Some(evaluated)) => {
                candidate.policy_id == evaluated.policy_id
                    && candidate.max_candidates == evaluated.max_candidates
                    && candidate.max_input_bytes == evaluated.max_input_bytes
                    && candidate.max_input_tokens == evaluated.max_input_tokens
                    && candidate.max_work_units == evaluated.max_work_units
                    && candidate.max_model_invocations == evaluated.max_model_invocations
                    && candidate.deadline_micros == evaluated.deadline_micros
            }
            _ => false,
        }
}

fn validate_evaluation_snapshot(
    repo_root: &Path,
    snapshot: &SemanticEvaluationPublicationSnapshotV1,
    candidate: &SemanticEvaluationProfileCandidateV1,
) -> Result<(), SemanticActivationCoordinationErrorV1> {
    snapshot
        .scope
        .validate()
        .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
    snapshot
        .code_generation
        .validate()
        .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
    snapshot
        .code_source_manifest_digest
        .validate()
        .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
    snapshot
        .code_snapshot_digest
        .validate()
        .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
    let expected_root = repo_root
        .canonicalize()
        .map_err(|_| SemanticActivationCoordinationErrorV1::Unavailable)?;
    let mounted_root = snapshot
        .project_root
        .canonicalize()
        .map_err(|_| SemanticActivationCoordinationErrorV1::Unavailable)?;
    if expected_root != mounted_root
        || candidate.evaluated_profile_id.trim() != candidate.evaluated_profile_id
        || candidate.evaluated_profile_id.is_empty()
    {
        return Err(SemanticActivationCoordinationErrorV1::Rejected);
    }
    match (
        candidate.compatibility.semantic.as_ref(),
        snapshot.semantic_source_generation.as_ref(),
        snapshot.vector_state_revision,
        snapshot.vector_generation_id.as_ref(),
    ) {
        (Some(required), Some(source), Some(revision), Some(generation))
            if source == &snapshot.code_generation
                && revision >= 0
                && generation == &required.vector_generation_id => {}
        (None, None, None, None) => {}
        _ => return Err(SemanticActivationCoordinationErrorV1::Rejected),
    }
    Ok(())
}

pub struct SemanticProtectedActivationOperationV1 {
    pub authority: ConfigurationMutationAuthority,
    pub selected_profile: crate::config::SemanticProfileSelection,
    pub central_mutation: DirectConfigurationMutation,
    pub now: UtcMicros,
}

pub struct SemanticProtectedRollbackOperationV1 {
    pub authority: ConfigurationMutationAuthority,
    pub central_mutation: DirectConfigurationMutation,
    pub trigger: String,
    pub now: UtcMicros,
}

pub struct SemanticAppliedActivationV1 {
    pub configuration_receipt: ConfigurationMutationReceipt,
}

pub struct SemanticAppliedRollbackV1 {
    pub configuration_receipt: ConfigurationMutationReceipt,
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
    use tracedecay_search_eval::load_direct_evaluated_profile_material;

    #[test]
    fn operation_requires_durable_authority_and_plan20_runtime() {
        std::hint::black_box(ProductionSemanticConfigurationOperationV1::new);
    }

    #[test]
    fn caller_profile_material_must_match_what_direct_evaluator_runs() {
        // The evaluator fixtures are workspace-relative, not crate-relative.
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root above crates/tracedecay-usecases");
        let material =
            load_direct_evaluated_profile_material(workspace_root, None, "query-fallback")
                .expect("checked-in evaluated profile");
        let mut candidate = SemanticEvaluationProfileCandidateV1 {
            evaluated_profile_id: "query-fallback".to_owned(),
            profile: SemanticEvaluationFusionCandidateV1 {
                profile_id: material.profile.profile_id.clone(),
                calibrations: material.profile.calibrations.clone(),
                score_domain_calibrations: material.profile.score_domain_calibrations.clone(),
                weights_micros: material.profile.weights_micros.clone(),
                diversity_policy_id: material.profile.diversity_policy_id.clone(),
                rerank_policy_id: material.profile.rerank_policy_id.clone(),
                retrieval_budget: material.profile.retrieval_budget,
            },
            diversity: SemanticEvaluationDiversityCandidateV1 {
                policy_id: material.diversity.policy_id.clone(),
                per_source_namespace: material.diversity.per_source_namespace,
                per_source_instance: material.diversity.per_source_instance,
                per_repository: material.diversity.per_repository,
                per_file: material.diversity.per_file,
                per_session_or_thread: material.diversity.per_session_or_thread,
                per_copy_cluster: material.diversity.per_copy_cluster,
                per_evidence_role: material.diversity.per_evidence_role,
            },
            rerank: None,
            compatibility: RetrievalCompatibilityPinsV1::default(),
        };

        assert!(candidate_matches_evaluated_material(&candidate, &material));
        *candidate
            .profile
            .weights_micros
            .get_mut(&RetrieverKind::Lexical)
            .expect("lexical weight") += 1;
        assert!(!candidate_matches_evaluated_material(&candidate, &material));
        let serialized = serde_json::to_string(&candidate).expect("serialize candidate");
        assert!(!serialized.contains("evaluation_result_anchor"));
    }
}
