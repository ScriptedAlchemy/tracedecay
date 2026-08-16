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
    SemanticEvaluationLifecycleVerificationV1, SemanticRuntimeFuture,
};
use crate::config::retrieval::{
    AcceptedRetrievalProfileV1, PassingRetrievalEvaluationV1, RetrievalCompatibilityPinsV1,
    RetrievalProfileCasV1, RetrievalRuntimeCompatibilityV1, SemanticResourceRequirementV1,
};
use crate::configuration::{
    ConfigurationCurrentStateV1, ConfigurationMutationAuthority, ConfigurationMutationReceipt,
    DirectConfigurationMutation, ProjectConfigurationRuntime,
};
use tracedecay_search_eval::{
    DirectActivationEvaluationV1, DirectEvaluatedProfileMaterialV1, DirectEvaluationReportV1,
    NativeQualificationExecutionResourceKeyV1, NativeQualificationExpectationsV1,
    NativeQualificationModelKeyV1, NativeQualificationPlatformV1, NativeQualificationRuntimeKeyV1,
    PackagedNativeActivationCandidateV1, PackagedNativeQualificationErrorV1,
    qualified_default_activation_candidate,
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
    pub code_capability_manifest_digest: ManifestDigest,
    pub semantic_source_generation: Option<CodeGenerationId>,
    pub vector_state_revision: Option<i64>,
    pub vector_generation_id: Option<VectorGenerationIdV1>,
    pub semantic_lifecycle_verification: Option<SemanticEvaluationLifecycleVerificationV1>,
    pub runtime: RetrievalRuntimeCompatibilityV1,
}

/// Read-only authority for observing the mounted evaluation state and running
/// the genuine direct evaluator. It deliberately cannot publish, commit, or
/// bootstrap configuration.
pub trait SemanticEvaluationSnapshotPortV1: Send + Sync {
    fn current(
        &self,
    ) -> SemanticRuntimeFuture<
        '_,
        Result<SemanticEvaluationPublicationSnapshotV1, SemanticActivationCoordinationErrorV1>,
    >;

    fn evaluate_default_candidate<'a>(
        &'a self,
        evaluated_profile_id: &'a str,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<DirectActivationEvaluationV1, SemanticActivationCoordinationErrorV1>,
    >;
}

/// Compare-and-swap publication capability layered on the read-only
/// evaluation authority. Qualification never needs this capability.
pub trait SemanticEvaluationPublicationSnapshotPortV1: SemanticEvaluationSnapshotPortV1 {
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

/// Closed evidence retained until the authority commits. Ordinary activation
/// can consume only a package that search-eval has already validated; genuine
/// qualification keeps its non-serializable evaluator capability opaque.
#[derive(Clone)]
enum SemanticActivationPublicationEvidenceV1 {
    Genuine(DirectActivationEvaluationV1),
    Packaged(PackagedNativeActivationCandidateV1),
}

impl SemanticActivationPublicationEvidenceV1 {
    fn report_and_material(&self) -> (DirectEvaluationReportV1, DirectEvaluatedProfileMaterialV1) {
        match self {
            Self::Genuine(evaluation) => evaluation.clone().into_parts(),
            Self::Packaged(candidate) => {
                let (portable_evidence, material) = candidate.clone().into_parts();
                (portable_evidence.report, material)
            }
        }
    }

    fn into_report(self) -> DirectEvaluationReportV1 {
        match self {
            Self::Genuine(evaluation) => evaluation.into_parts().0,
            Self::Packaged(candidate) => candidate.into_parts().0.report,
        }
    }
}

struct PreparedSemanticActivationPublicationV1 {
    report: DirectEvaluationReportV1,
    accepted_profile: AcceptedRetrievalProfileV1,
    accepted_runtime: RetrievalRuntimeCompatibilityV1,
}

/// Non-publishing result of a genuine direct evaluation against one exact
/// mounted snapshot. The opaque evaluation is retained so callers cannot
/// manufacture a qualification from report-shaped data.
pub struct SemanticEvaluatedProfileQualificationV1 {
    evaluation: DirectActivationEvaluationV1,
    snapshot: SemanticEvaluationPublicationSnapshotV1,
    candidate: SemanticEvaluationProfileCandidateV1,
}

impl SemanticEvaluatedProfileQualificationV1 {
    /// Exact snapshot observed both before and after the evaluator ran.
    pub fn snapshot(&self) -> &SemanticEvaluationPublicationSnapshotV1 {
        &self.snapshot
    }

    /// Candidate identity bound to the direct evaluator material.
    pub fn evaluated_profile_id(&self) -> &str {
        &self.candidate.evaluated_profile_id
    }

    /// Candidate whose exact material was checked against the opaque direct
    /// evaluator result before this qualification was returned.
    pub fn candidate(&self) -> &SemanticEvaluationProfileCandidateV1 {
        &self.candidate
    }

    /// Consume the opaque genuine evaluator result for a non-publishing
    /// consumer such as daemon-side qualification encoding.
    pub fn into_evaluation(self) -> DirectActivationEvaluationV1 {
        self.evaluation
    }
}

/// Closed durable effect supplied after genuine qualification or package
/// validation has produced a PASS. Runtime and freshness bindings are taken
/// only from the snapshot protected by the authority's CAS/guard.
pub struct SemanticEvaluationAuthorityPublicationV1 {
    configuration: Arc<ProjectConfigurationRuntime>,
    accepted_profiles: Arc<RegisteredSemanticAcceptedProfileAuthorityV1>,
    evidence: SemanticActivationPublicationEvidenceV1,
    accepted_profile: AcceptedRetrievalProfileV1,
    runtime: RetrievalRuntimeCompatibilityV1,
}

impl SemanticEvaluationAuthorityPublicationV1 {
    pub fn semantic_compatibility(
        &self,
    ) -> Option<&crate::config::retrieval::SemanticCompatibilityPinsV1> {
        self.accepted_profile.compatibility().semantic.as_ref()
    }

    pub async fn commit(
        self,
        expected: &SemanticEvaluationPublicationSnapshotV1,
    ) -> Result<(), SemanticActivationCoordinationErrorV1> {
        let report = self.evidence.into_report();
        self.accepted_profile
            .executable_under(&self.runtime)
            .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
        let bootstrap_query = self.accepted_profile.is_exact_query_fallback();
        let profile_digest = self.accepted_profile.profile_digest().clone();
        self.accepted_profiles
            .publish(
                &expected.project_root,
                report,
                self.accepted_profile,
                self.runtime,
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

    /// Validate and run the genuine checked-in direct evaluator without a
    /// publication capability. The returned qualification binds the opaque
    /// evaluator output to an unchanged mounted snapshot.
    pub async fn qualify_profile(
        snapshot_authority: &dyn SemanticEvaluationSnapshotPortV1,
        repo_root: &Path,
        candidate: SemanticEvaluationProfileCandidateV1,
    ) -> Result<SemanticEvaluatedProfileQualificationV1, SemanticActivationCoordinationErrorV1>
    {
        Self::qualify_profile_with(snapshot_authority, repo_root, candidate).await
    }

    async fn qualify_profile_with<SnapshotAuthority>(
        snapshot_authority: &SnapshotAuthority,
        repo_root: &Path,
        candidate: SemanticEvaluationProfileCandidateV1,
    ) -> Result<SemanticEvaluatedProfileQualificationV1, SemanticActivationCoordinationErrorV1>
    where
        SnapshotAuthority: SemanticEvaluationSnapshotPortV1 + ?Sized,
    {
        let before = snapshot_authority.current().await?;
        validate_evaluation_snapshot(repo_root, &before, &candidate)?;

        let qualification_candidate = candidate.clone();
        let evaluation = snapshot_authority
            .evaluate_default_candidate(&candidate.evaluated_profile_id)
            .await?;
        prepare_semantic_activation_publication(
            &before,
            &candidate,
            &SemanticActivationPublicationEvidenceV1::Genuine(evaluation.clone()),
        )?;

        if snapshot_authority.current().await? != before {
            return Err(SemanticActivationCoordinationErrorV1::Conflict);
        }

        Ok(SemanticEvaluatedProfileQualificationV1 {
            evaluation,
            snapshot: before,
            candidate: qualification_candidate,
        })
    }

    /// Publish only evidence from the reviewed native-qualification package.
    /// Genuine evaluation is intentionally exclusive to [`Self::qualify_profile`].
    pub async fn evaluate_and_publish_profile(
        &self,
        snapshot_authority: &dyn SemanticEvaluationPublicationSnapshotPortV1,
        repo_root: &Path,
        candidate: SemanticEvaluationProfileCandidateV1,
    ) -> Result<SemanticEvaluatedProfilePublicationV1, SemanticActivationCoordinationErrorV1> {
        let before = snapshot_authority.current().await?;
        validate_evaluation_snapshot(repo_root, &before, &candidate)?;
        let candidate = candidate_rebound_to_snapshot_runtime(candidate, &before)?;
        let expectations = native_qualification_expectations(&before, &candidate)?;
        let evidence = SemanticActivationPublicationEvidenceV1::Packaged(
            qualified_default_activation_candidate(&expectations)
                .map_err(map_packaged_qualification_error)?,
        );
        let prepared = prepare_semantic_activation_publication(&before, &candidate, &evidence)?;

        if snapshot_authority.current().await? != before {
            return Err(SemanticActivationCoordinationErrorV1::Conflict);
        }

        let publication = SemanticEvaluationAuthorityPublicationV1 {
            configuration: Arc::clone(&self.configuration),
            accepted_profiles: Arc::clone(&self.accepted_profiles),
            evidence,
            accepted_profile: prepared.accepted_profile.clone(),
            runtime: prepared.accepted_runtime,
        };
        snapshot_authority
            .publish_if_current(&before, publication)
            .await?;
        Ok(SemanticEvaluatedProfilePublicationV1 {
            report: prepared.report,
            accepted_profile: prepared.accepted_profile,
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

fn candidate_rebound_to_snapshot_runtime(
    mut candidate: SemanticEvaluationProfileCandidateV1,
    snapshot: &SemanticEvaluationPublicationSnapshotV1,
) -> Result<SemanticEvaluationProfileCandidateV1, SemanticActivationCoordinationErrorV1> {
    match (
        candidate.compatibility.semantic.as_ref(),
        snapshot.runtime.semantic.as_ref(),
    ) {
        (Some(candidate_semantic), Some(snapshot_semantic))
            if candidate_semantic == snapshot_semantic =>
        {
            // Portable packaged evidence deliberately has no project-local
            // vector identity. Bind publication to the current snapshot,
            // never to package material.
            candidate.compatibility.semantic = Some(snapshot_semantic.clone());
            Ok(candidate)
        }
        _ => Err(SemanticActivationCoordinationErrorV1::Rejected),
    }
}

fn native_qualification_expectations(
    snapshot: &SemanticEvaluationPublicationSnapshotV1,
    candidate: &SemanticEvaluationProfileCandidateV1,
) -> Result<NativeQualificationExpectationsV1, SemanticActivationCoordinationErrorV1> {
    let semantic = snapshot
        .runtime
        .semantic
        .as_ref()
        .ok_or(SemanticActivationCoordinationErrorV1::Rejected)?;
    let runtime = NativeQualificationRuntimeKeyV1 {
        implementation_revision: semantic.implementation_revision.clone(),
        fusion_revision: semantic.fusion_revision.clone(),
        runtime_compatibility_digest: semantic.runtime_compatibility_digest.clone(),
        model: NativeQualificationModelKeyV1::from_admitted_projection(&semantic.projection),
        search_index_key: semantic.search_index_key.clone(),
        execution_resources: NativeQualificationExecutionResourceKeyV1 {
            model_bytes: semantic.resources.model_bytes,
            tokenizer_bytes: semantic.resources.tokenizer_bytes,
            threads: semantic.resources.threads,
            max_concurrent_sessions: semantic.resources.max_concurrent_sessions,
            batch_size: semantic.resources.batch_size,
            sequence_length: semantic.resources.sequence_length,
            load_deadline_ms: semantic.resources.load_deadline_ms,
        },
    };
    NativeQualificationExpectationsV1::packaged_default(
        candidate.evaluated_profile_id.clone(),
        runtime,
        NativeQualificationPlatformV1::current(),
    )
    .map_err(map_packaged_qualification_error)
}

fn map_packaged_qualification_error(
    error: PackagedNativeQualificationErrorV1,
) -> SemanticActivationCoordinationErrorV1 {
    match error {
        PackagedNativeQualificationErrorV1::EmbeddedAssetUnavailable => {
            SemanticActivationCoordinationErrorV1::Unavailable
        }
        PackagedNativeQualificationErrorV1::CorruptBytes
        | PackagedNativeQualificationErrorV1::UnsupportedSchema
        | PackagedNativeQualificationErrorV1::InvalidQualificationKey
        | PackagedNativeQualificationErrorV1::StaleWorkload
        | PackagedNativeQualificationErrorV1::StaleCorpus
        | PackagedNativeQualificationErrorV1::StaleExecutionRevision
        | PackagedNativeQualificationErrorV1::ModelMismatch
        | PackagedNativeQualificationErrorV1::BuildMismatch
        | PackagedNativeQualificationErrorV1::SearchIndexMismatch
        | PackagedNativeQualificationErrorV1::RuntimeMismatch
        | PackagedNativeQualificationErrorV1::PlatformMismatch
        | PackagedNativeQualificationErrorV1::InvalidRawOutputEvidence
        | PackagedNativeQualificationErrorV1::IncompleteNativeEvidence
        | PackagedNativeQualificationErrorV1::FailedQualification => {
            SemanticActivationCoordinationErrorV1::Rejected
        }
    }
}

fn prepare_semantic_activation_publication(
    snapshot: &SemanticEvaluationPublicationSnapshotV1,
    candidate: &SemanticEvaluationProfileCandidateV1,
    evidence: &SemanticActivationPublicationEvidenceV1,
) -> Result<PreparedSemanticActivationPublicationV1, SemanticActivationCoordinationErrorV1> {
    let (report, evaluated_material) = evidence.report_and_material();
    if !candidate_matches_evaluated_material(candidate, &evaluated_material) {
        return Err(SemanticActivationCoordinationErrorV1::Rejected);
    }
    let mut compatibility = candidate.compatibility.clone();
    if let Some(semantic) = compatibility.semantic.as_mut() {
        semantic.resources =
            semantic_resource_requirement_from_report(&report, &candidate.evaluated_profile_id)?;
    }
    let accepted_runtime = runtime_with_accepted_resources(&snapshot.runtime, &compatibility)?;
    let passing_evaluation =
        PassingRetrievalEvaluationV1::from_report(&report, &candidate.evaluated_profile_id)
            .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
    let evaluation_anchor = passing_evaluation.evaluation_anchor().clone();
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
        compatibility,
        passing_evaluation,
    )
    .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
    accepted_profile
        .executable_under(&accepted_runtime)
        .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
    Ok(PreparedSemanticActivationPublicationV1 {
        report,
        accepted_profile,
        accepted_runtime,
    })
}

fn semantic_resource_requirement_from_report(
    report: &DirectEvaluationReportV1,
    evaluated_profile_id: &str,
) -> Result<SemanticResourceRequirementV1, SemanticActivationCoordinationErrorV1> {
    let measured = report
        .semantic_activation_resource_pins(evaluated_profile_id)
        .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
    Ok(SemanticResourceRequirementV1 {
        model_bytes: measured.model_bytes,
        tokenizer_bytes: measured.tokenizer_bytes,
        resident_bytes: measured.resident_bytes,
        threads: measured.threads,
        max_concurrent_sessions: measured.max_concurrent_sessions,
        batch_size: measured.batch_size,
        sequence_length: measured.sequence_length,
        load_deadline_ms: measured.load_deadline_ms,
    })
}

fn runtime_with_accepted_resources(
    observed: &RetrievalRuntimeCompatibilityV1,
    accepted: &RetrievalCompatibilityPinsV1,
) -> Result<RetrievalRuntimeCompatibilityV1, SemanticActivationCoordinationErrorV1> {
    let mut runtime = observed.clone();
    match (runtime.semantic.as_mut(), accepted.semantic.as_ref()) {
        (Some(observed), Some(accepted)) => {
            observed.resources = accepted.resources;
            if observed != accepted {
                return Err(SemanticActivationCoordinationErrorV1::Rejected);
            }
            // Keep the runtime-observed configured ceiling. The accepted
            // profile's canonical `executable_under` validation below proves
            // measured report resources fit within this actual ceiling.
        }
        (None, None) => {}
        _ => return Err(SemanticActivationCoordinationErrorV1::Rejected),
    }
    runtime.semantic = accepted.semantic.clone();
    Ok(runtime)
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
    snapshot
        .code_capability_manifest_digest
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
        snapshot.runtime.semantic.as_ref(),
        snapshot.semantic_lifecycle_verification.as_ref(),
    ) {
        (
            Some(required),
            Some(source),
            Some(revision),
            Some(generation),
            Some(observed),
            Some(_),
        ) if source == &snapshot.code_generation
            && revision >= 0
            && generation == &required.vector_generation_id
            && observed == required => {}
        (None, None, None, None, None, None) => {}
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
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;
    use tracedecay_domain::configuration::{ConfigurationRevisionId, ConfigurationSnapshotV1};
    use tracedecay_domain::{
        ChunkerRevision, ComponentRevision, EmbeddingDeviceClassV1, EmbeddingMetricV1,
        EmbeddingNormalizationV1, EmbeddingPoolingV1, EmbeddingPrecisionV1,
        EmbeddingProjectionKeyV1, EmbeddingTruncationSideV1, FusionProfileId, PrivacyDomainId,
        ProjectId, RepositoryId, RetrievalBudget, WorktreeId,
    };
    use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;
    use tracedecay_query::retrieval::semantic::SemanticCalibrationProfileV1;

    use crate::config::retrieval::SemanticCompatibilityPinsV1;

    const EVALUATED_PROFILE_ID: &str = "query-fallback";

    struct RecordingSnapshotAuthority {
        snapshots: Mutex<VecDeque<SemanticEvaluationPublicationSnapshotV1>>,
        evaluated_profile_id: String,
        publish_result: Result<(), SemanticActivationCoordinationErrorV1>,
        current_calls: AtomicUsize,
        evaluation_calls: AtomicUsize,
        publish_calls: AtomicUsize,
        published_snapshot: Mutex<Option<SemanticEvaluationPublicationSnapshotV1>>,
    }

    impl RecordingSnapshotAuthority {
        // Successful qualification requires an independently validated opaque
        // evaluator result. This authority models only genuine evaluator
        // denial, so these tests cannot accidentally mint one from report data.
        fn rejecting(
            snapshots: impl IntoIterator<Item = SemanticEvaluationPublicationSnapshotV1>,
        ) -> Self {
            Self {
                snapshots: Mutex::new(snapshots.into_iter().collect()),
                evaluated_profile_id: EVALUATED_PROFILE_ID.to_owned(),
                publish_result: Ok(()),
                current_calls: AtomicUsize::new(0),
                evaluation_calls: AtomicUsize::new(0),
                publish_calls: AtomicUsize::new(0),
                published_snapshot: Mutex::new(None),
            }
        }

        fn with_publish_result(mut self, result: SemanticActivationCoordinationErrorV1) -> Self {
            self.publish_result = Err(result);
            self
        }

        fn calls(&self) -> (usize, usize, usize) {
            (
                self.current_calls.load(Ordering::SeqCst),
                self.evaluation_calls.load(Ordering::SeqCst),
                self.publish_calls.load(Ordering::SeqCst),
            )
        }

        fn published_snapshot(&self) -> Option<SemanticEvaluationPublicationSnapshotV1> {
            self.published_snapshot
                .lock()
                .expect("published snapshot lock")
                .clone()
        }
    }

    impl SemanticEvaluationSnapshotPortV1 for RecordingSnapshotAuthority {
        fn current(
            &self,
        ) -> SemanticRuntimeFuture<
            '_,
            Result<SemanticEvaluationPublicationSnapshotV1, SemanticActivationCoordinationErrorV1>,
        > {
            Box::pin(async move {
                self.current_calls.fetch_add(1, Ordering::SeqCst);
                let mut snapshots = self.snapshots.lock().expect("snapshots lock");
                if snapshots.len() > 1 {
                    snapshots.pop_front()
                } else {
                    snapshots.front().cloned()
                }
                .ok_or(SemanticActivationCoordinationErrorV1::Unavailable)
            })
        }

        fn evaluate_default_candidate<'a>(
            &'a self,
            evaluated_profile_id: &'a str,
        ) -> SemanticRuntimeFuture<
            'a,
            Result<DirectActivationEvaluationV1, SemanticActivationCoordinationErrorV1>,
        > {
            Box::pin(async move {
                self.evaluation_calls.fetch_add(1, Ordering::SeqCst);
                if evaluated_profile_id != self.evaluated_profile_id {
                    return Err(SemanticActivationCoordinationErrorV1::Rejected);
                }
                Err(SemanticActivationCoordinationErrorV1::Rejected)
            })
        }
    }

    impl SemanticEvaluationPublicationSnapshotPortV1 for RecordingSnapshotAuthority {
        fn publish_if_current<'a>(
            &'a self,
            expected: &'a SemanticEvaluationPublicationSnapshotV1,
            _publication: SemanticEvaluationAuthorityPublicationV1,
        ) -> SemanticRuntimeFuture<'a, Result<(), SemanticActivationCoordinationErrorV1>> {
            Box::pin(async move {
                self.publish_calls.fetch_add(1, Ordering::SeqCst);
                *self
                    .published_snapshot
                    .lock()
                    .expect("published snapshot lock") = Some(expected.clone());
                self.publish_result.clone()
            })
        }
    }

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64)))
            .expect("test manifest digest")
    }

    fn workspace_root() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root above crates/tracedecay-usecases")
    }

    fn typed<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("test identity")
    }

    fn query_candidate() -> SemanticEvaluationProfileCandidateV1 {
        SemanticEvaluationProfileCandidateV1 {
            evaluated_profile_id: EVALUATED_PROFILE_ID.to_owned(),
            profile: SemanticEvaluationFusionCandidateV1 {
                profile_id: typed::<FusionProfileId>("profile.qualification-rejection-test"),
                calibrations: BTreeMap::new(),
                score_domain_calibrations: BTreeMap::new(),
                weights_micros: BTreeMap::new(),
                diversity_policy_id: typed("diversity.qualification-rejection-test"),
                rerank_policy_id: None,
                retrieval_budget: RetrievalBudget {
                    max_candidates_per_lane: 1,
                    max_fused_candidates: 1,
                    max_hydrated_results: 1,
                    max_hydration_bytes: 1,
                    deadline_micros: None,
                },
            },
            diversity: SemanticEvaluationDiversityCandidateV1 {
                policy_id: typed("diversity.qualification-rejection-test"),
                per_source_namespace: None,
                per_source_instance: None,
                per_repository: None,
                per_file: None,
                per_session_or_thread: None,
                per_copy_cluster: None,
                per_evidence_role: None,
            },
            rerank: None,
            compatibility: RetrievalCompatibilityPinsV1::default(),
        }
    }

    fn semantic_resources(model_bytes: u64) -> SemanticResourceRequirementV1 {
        SemanticResourceRequirementV1 {
            model_bytes,
            tokenizer_bytes: model_bytes / 2,
            resident_bytes: model_bytes * 2,
            threads: 1,
            max_concurrent_sessions: 1,
            batch_size: 1,
            sequence_length: 32,
            load_deadline_ms: 1_000,
        }
    }

    fn semantic_compatibility(
        resources: SemanticResourceRequirementV1,
    ) -> SemanticCompatibilityPinsV1 {
        let artifact = digest('a');
        let projection = EmbeddingProjectionKeyV1 {
            model_artifact_digest: artifact.clone(),
            tokenizer_digest: digest('b'),
            config_digest: digest('c'),
            query_instruction_digest: None,
            document_instruction_digest: None,
            pooling: EmbeddingPoolingV1::Mean,
            truncation_side: EmbeddingTruncationSideV1::Right,
            truncation_length: 32,
            runtime_backend: "fastembed-ort".to_owned(),
            runtime_build_revision: "runtime.qualification-test.v1".to_owned(),
            device_class: EmbeddingDeviceClassV1::Cpu,
            dimensions: 4,
            metric: EmbeddingMetricV1::Cosine,
            normalization: EmbeddingNormalizationV1::L2,
            precision: EmbeddingPrecisionV1::Fp32,
            chunk_schema_revision: "code-search-chunk.v1".to_owned(),
            chunker_revision: typed::<ChunkerRevision>("chunker.qualification-test.v1"),
            privacy_domain: typed::<PrivacyDomainId>("privacy.qualification-test.v1"),
            privacy_key_epoch: 1,
        }
        .admit()
        .expect("admitted projection");
        let vector_generation_id = VectorGenerationIdV1::new(digest('d'));
        SemanticCompatibilityPinsV1 {
            implementation_revision: ComponentRevision::new("semantic.qualification-test.v1")
                .expect("implementation revision"),
            fusion_revision: ComponentRevision::new("fusion.qualification-test.v1")
                .expect("fusion revision"),
            artifact_manifest_digest: artifact,
            runtime_compatibility_digest: digest('e'),
            projection: projection.clone(),
            search_index_key: tracedecay_domain::SemanticSearchIndexProfileV1::exact_flat_v1()
                .and_then(|profile| profile.index_key())
                .expect("exact-flat search index"),
            vector_generation_id: vector_generation_id.clone(),
            calibration: SemanticCalibrationProfileV1 {
                calibration_profile_id: typed("calibration.qualification-test.v1"),
                cohort_digest: digest('f'),
                projection_key: projection.projection_key().clone(),
                vector_generation: vector_generation_id,
                capability_manifest_digest: digest('1'),
                maximum_distance_micros: 2_000_000,
                minimum_margin_micros: 0,
            },
            resources,
        }
    }

    fn query_snapshot(
        candidate: &SemanticEvaluationProfileCandidateV1,
    ) -> SemanticEvaluationPublicationSnapshotV1 {
        SemanticEvaluationPublicationSnapshotV1 {
            project_root: workspace_root().to_path_buf(),
            scope: ResolvedScope::new(
                ProjectId::new("project.qualification-rejection-test").expect("project id"),
                RepositoryId::new("repository.qualification-rejection-test")
                    .expect("repository id"),
                WorktreeId::new("worktree.qualification-rejection-test").expect("worktree id"),
                None,
            )
            .expect("resolved scope"),
            code_generation: CodeGenerationId::new("generation.qualification-rejection-test")
                .expect("code generation"),
            code_source_manifest_digest: digest('2'),
            code_snapshot_digest: digest('3'),
            code_capability_manifest_digest: digest('4'),
            semantic_source_generation: None,
            vector_state_revision: None,
            vector_generation_id: None,
            semantic_lifecycle_verification: None,
            runtime: RetrievalRuntimeCompatibilityV1 {
                retrieval_ceiling: candidate.profile.retrieval_budget,
                semantic: None,
                semantic_ceiling: None,
                rerank: None,
                rerank_ceiling: None,
            },
        }
    }

    #[test]
    fn semantic_qualification_requires_a_runtime_minted_lifecycle_receipt() {
        let mut candidate = query_candidate();
        let semantic = semantic_compatibility(semantic_resources(10));
        candidate.compatibility.semantic = Some(semantic.clone());
        let mut snapshot = query_snapshot(&candidate);
        snapshot.semantic_source_generation = Some(snapshot.code_generation.clone());
        snapshot.vector_state_revision = Some(0);
        snapshot.vector_generation_id = Some(semantic.vector_generation_id.clone());
        snapshot.runtime.semantic = Some(semantic);
        snapshot.runtime.semantic_ceiling = Some(semantic_resources(20));

        assert_eq!(
            validate_evaluation_snapshot(workspace_root(), &snapshot, &candidate),
            Err(SemanticActivationCoordinationErrorV1::Rejected)
        );
    }

    #[test]
    fn ordinary_packaged_publication_rejects_a_foreign_project_vector_generation() {
        let mut candidate = query_candidate();
        candidate.compatibility.semantic = Some(semantic_compatibility(semantic_resources(10)));
        let mut snapshot = query_snapshot(&candidate);
        snapshot.runtime.semantic = candidate.compatibility.semantic.clone();
        let foreign = VectorGenerationIdV1::new(digest('9'));
        candidate
            .compatibility
            .semantic
            .as_mut()
            .expect("semantic candidate")
            .vector_generation_id = foreign;

        assert!(matches!(
            candidate_rebound_to_snapshot_runtime(candidate, &snapshot),
            Err(SemanticActivationCoordinationErrorV1::Rejected)
        ));
    }

    #[test]
    fn packaged_qualification_unavailability_remains_typed() {
        assert_eq!(
            map_packaged_qualification_error(
                PackagedNativeQualificationErrorV1::EmbeddedAssetUnavailable,
            ),
            SemanticActivationCoordinationErrorV1::Unavailable
        );
    }

    #[test]
    fn ordinary_packaged_publication_denies_missing_genuine_package_without_evaluator() {
        let mut candidate = query_candidate();
        candidate.compatibility.semantic = Some(semantic_compatibility(semantic_resources(10)));
        let mut snapshot = query_snapshot(&candidate);
        snapshot.runtime.semantic = candidate.compatibility.semantic.clone();
        let candidate = candidate_rebound_to_snapshot_runtime(candidate, &snapshot)
            .expect("current semantic snapshot rebinds the candidate");
        let expectations = native_qualification_expectations(&snapshot, &candidate)
            .expect("runtime pins produce native qualification expectations");

        assert!(matches!(
            qualified_default_activation_candidate(&expectations),
            Err(PackagedNativeQualificationErrorV1::EmbeddedAssetUnavailable)
        ));
    }

    #[test]
    fn measured_report_resources_replace_semantic_pins_but_retain_configured_ceiling() {
        let measured = semantic_resources(10);
        let configured_ceiling = semantic_resources(20);
        let observed_semantic = semantic_compatibility(semantic_resources(8));
        let mut accepted_semantic = observed_semantic.clone();
        accepted_semantic.resources = measured;
        let observed = RetrievalRuntimeCompatibilityV1 {
            retrieval_ceiling: RetrievalBudget {
                max_candidates_per_lane: 1,
                max_fused_candidates: 1,
                max_hydrated_results: 1,
                max_hydration_bytes: 1,
                deadline_micros: None,
            },
            semantic: Some(observed_semantic),
            semantic_ceiling: Some(configured_ceiling),
            rerank: None,
            rerank_ceiling: None,
        };
        let accepted = RetrievalCompatibilityPinsV1 {
            semantic: Some(accepted_semantic.clone()),
            rerank: None,
        };

        let runtime = runtime_with_accepted_resources(&observed, &accepted)
            .expect("measured semantic resources remain within the configured ceiling");

        assert_eq!(runtime.semantic, Some(accepted_semantic));
        assert_eq!(runtime.semantic_ceiling, Some(configured_ceiling));
    }

    async fn operation_for_publish_test() -> ProductionSemanticConfigurationOperationV1 {
        let directory = tempfile::tempdir().expect("test profile directory");
        let project_root = directory.path().join("project");
        std::fs::create_dir_all(&project_root).expect("test project directory");
        let project_id =
            ProjectId::new("project.native-qualification-operation").expect("project id");
        let database_runtime = RegisteredGlobalDbTestRuntime::project(
            directory.path().join("profile"),
            &project_root,
            project_id.clone(),
        )
        .await
        .expect("registered project database");
        let database = database_runtime
            .project_database_arc()
            .expect("project database");
        let configuration = crate::config::PinnedRuntimeConfiguration {
            target: crate::config::RuntimeConfigurationTarget {
                project_id,
                project_root,
            },
            revision_id: ConfigurationRevisionId::try_from(
                "configuration.native-qualification-operation".to_owned(),
            )
            .expect("configuration revision"),
            snapshot: ConfigurationSnapshotV1::new(BTreeMap::new(), BTreeMap::new())
                .expect("empty configuration snapshot"),
            config: crate::config::TraceDecayConfig::default(),
        };
        let (configuration, _) = ProjectConfigurationRuntime::open(
            crate::config::OpenedRuntimeConfiguration::new(configuration, Arc::clone(&database)),
        )
        .expect("configuration runtime");
        ProductionSemanticConfigurationOperationV1::new(
            Arc::new(configuration),
            Arc::new(RegisteredSemanticAcceptedProfileAuthorityV1::new(database)),
        )
    }

    #[tokio::test]
    async fn qualification_rejects_a_controlled_evaluator_without_publishing() {
        let candidate = query_candidate();
        let authority = RecordingSnapshotAuthority::rejecting([query_snapshot(&candidate)]);

        let result = ProductionSemanticConfigurationOperationV1::qualify_profile(
            &authority,
            workspace_root(),
            candidate,
        )
        .await;

        assert!(matches!(
            result,
            Err(SemanticActivationCoordinationErrorV1::Rejected)
        ));
        assert_eq!(authority.calls(), (1, 1, 0));
    }

    #[tokio::test]
    async fn qualification_rejects_malformed_candidate_without_evaluation_or_publication() {
        let mut candidate = query_candidate();
        candidate.evaluated_profile_id = " query-fallback".to_owned();
        let authority = RecordingSnapshotAuthority::rejecting([query_snapshot(&candidate)]);

        let result = ProductionSemanticConfigurationOperationV1::qualify_profile(
            &authority,
            workspace_root(),
            candidate,
        )
        .await;

        assert!(matches!(
            result,
            Err(SemanticActivationCoordinationErrorV1::Rejected)
        ));
        assert_eq!(authority.calls(), (1, 0, 0));
    }

    #[tokio::test]
    async fn qualification_rejects_a_stale_mounted_snapshot_without_evaluation_or_publication() {
        let candidate = query_candidate();
        let mut snapshot = query_snapshot(&candidate);
        snapshot.semantic_source_generation = Some(
            CodeGenerationId::new("generation.stale-qualification-test")
                .expect("stale code generation"),
        );
        let authority = RecordingSnapshotAuthority::rejecting([snapshot]);

        let result = ProductionSemanticConfigurationOperationV1::qualify_profile(
            &authority,
            workspace_root(),
            candidate,
        )
        .await;

        assert!(matches!(
            result,
            Err(SemanticActivationCoordinationErrorV1::Rejected)
        ));
        assert_eq!(authority.calls(), (1, 0, 0));
    }

    #[tokio::test]
    async fn ordinary_publish_does_not_run_the_native_evaluator_without_package_evidence() {
        let candidate = query_candidate();
        let authority = RecordingSnapshotAuthority::rejecting([query_snapshot(&candidate)]);
        let operation = operation_for_publish_test().await;

        let result = operation
            .evaluate_and_publish_profile(&authority, workspace_root(), candidate)
            .await;

        assert!(matches!(
            result,
            Err(SemanticActivationCoordinationErrorV1::Rejected)
        ));
        assert_eq!(authority.calls(), (1, 0, 0));
        assert_eq!(authority.published_snapshot(), None);
    }

    #[tokio::test]
    async fn ordinary_publish_never_reaches_compare_and_swap_without_package_evidence() {
        let candidate = query_candidate();
        let authority = RecordingSnapshotAuthority::rejecting([query_snapshot(&candidate)])
            .with_publish_result(SemanticActivationCoordinationErrorV1::Conflict);
        let operation = operation_for_publish_test().await;

        let result = operation
            .evaluate_and_publish_profile(&authority, workspace_root(), candidate)
            .await;

        assert!(matches!(
            result,
            Err(SemanticActivationCoordinationErrorV1::Rejected)
        ));
        assert_eq!(authority.calls(), (1, 0, 0));
        assert_eq!(authority.published_snapshot(), None);
    }

    #[test]
    fn operation_requires_durable_authority_and_configuration_runtime() {
        std::hint::black_box(ProductionSemanticConfigurationOperationV1::new);
    }

    #[test]
    fn caller_profile_material_must_match_what_direct_evaluator_runs() {
        // The evaluator fixtures are workspace-relative, not crate-relative.
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root above crates/tracedecay-usecases");
        let material = tracedecay_search_eval::load_direct_evaluated_profile_material(
            workspace_root,
            None,
            "query-fallback",
        )
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
