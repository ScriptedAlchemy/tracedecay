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
    ProjectSemanticActivationExt, RegisteredSemanticAcceptedProfileAuthorityV1,
    SemanticAcceptedProfileAuthorityErrorV1, SemanticAcceptedProfileAuthorityPortV1,
    SemanticActivationCoordinationErrorV1, SemanticEvaluationLifecycleVerificationV1,
    SemanticRuntimeFuture,
};
use crate::config::retrieval::{
    AcceptedRetrievalProfileV1, PassingRetrievalEvaluationV1, RetrievalCompatibilityPinsV1,
    RetrievalProfileCasV1, RetrievalRuntimeCompatibilityV1, SemanticResourceRequirementV1,
};
use tracedecay_configuration::{
    ConfigurationCurrentStateV1, ConfigurationMutationAuthority, ConfigurationMutationReceipt,
    DirectConfigurationMutation, ProjectConfigurationRuntime,
};
use tracedecay_query::search_quality::{
    DirectActivationEvaluationV1, DirectEvaluatedProfileMaterialV1, DirectEvaluationReportV1,
    NativeQualificationExecutionResourceKeyV1, NativeQualificationExpectationsV1,
    NativeQualificationModelKeyV1, NativeQualificationPlatformV1, NativeQualificationRuntimeKeyV1,
    PackagedNativeActivationCandidateV1, PackagedNativeQualificationErrorV1,
    qualified_default_activation_candidate,
};
use tracedecay_semantic_contracts::SemanticProfileSelection;

use super::accepted_profile_authority::SemanticEvaluationPublicationIdentityV1;

/// Unevaluated fusion material. No evaluation-result anchor is accepted from
/// the caller; production derives it from the genuine direct-evaluator PASS.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticEvaluationFusionCandidateV1 {
    pub profile_id: FusionProfileId,
    pub calibrations: BTreeMap<RetrieverKind, CalibrationProfileId>,
    pub score_domain_calibrations: BTreeMap<ScoreDomainId, ScoreDomainCalibrationV1>,
    pub minimum_calibrated_feature_micros: BTreeMap<RetrieverKind, u32>,
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
    query_fallback: PreparedQueryFallbackPublicationV1,
}

struct PreparedQueryFallbackPublicationV1 {
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
    query_fallback: PreparedQueryFallbackPublicationV1,
}

impl SemanticEvaluationAuthorityPublicationV1 {
    pub fn semantic_compatibility(
        &self,
    ) -> Option<&crate::config::retrieval::SemanticCompatibilityPinsV1> {
        self.accepted_profile.compatibility().semantic.as_ref()
    }

    #[hotpath::measure(label = "usecases.semantic_config.commit_publication", future = true)]
    pub async fn commit(
        self,
        expected: &SemanticEvaluationPublicationSnapshotV1,
    ) -> Result<(), SemanticActivationCoordinationErrorV1> {
        let report = self.evidence.into_report();
        self.accepted_profile
            .executable_under(&self.runtime)
            .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
        self.query_fallback
            .accepted_profile
            .executable_under(&self.query_fallback.accepted_runtime)
            .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
        let publication_identity = SemanticEvaluationPublicationIdentityV1 {
            scope_digest: expected.scope.scope_digest.clone(),
            code_generation: expected.code_generation.clone(),
            code_source_manifest_digest: expected.code_source_manifest_digest.clone(),
            code_snapshot_digest: expected.code_snapshot_digest.clone(),
            semantic_source_generation: expected.semantic_source_generation.clone(),
            vector_state_revision: expected.vector_state_revision,
            vector_generation_id: expected.vector_generation_id.clone(),
        };
        let fallback_digest = self
            .query_fallback
            .accepted_profile
            .profile_digest()
            .clone();
        self.accepted_profiles
            .publish(
                &expected.project_root,
                report.clone(),
                self.query_fallback.accepted_profile,
                self.query_fallback.accepted_runtime,
                publication_identity.clone(),
                expected.code_snapshot_digest.clone(),
            )
            .await
            .map_err(map_authority_error)?;
        self.accepted_profiles
            .publish(
                &expected.project_root,
                report,
                self.accepted_profile,
                self.runtime,
                publication_identity,
                expected.code_snapshot_digest.clone(),
            )
            .await
            .map_err(map_authority_error)?;
        let coordinator = self
            .configuration
            .semantic_activation_coordinator()
            .ok_or(SemanticActivationCoordinationErrorV1::Unavailable)?;
        match coordinator.current_profile_state().await {
            Ok(_) => {}
            Err(SemanticActivationCoordinationErrorV1::Unavailable) => {
                let configuration = current_configuration_state(&self.configuration).await?;
                let fallback = self
                    .accepted_profiles
                    .resolve(&fallback_digest)
                    .await
                    .map_err(map_authority_error)?;
                self.configuration
                    .bootstrap_query_retrieval_profile(
                        configuration,
                        fallback.accepted_profile,
                        &fallback.runtime,
                    )
                    .await?;
            }
            Err(error) => return Err(error),
        }
        Ok(())
    }
}

/// Production application operation for the linked configuration and
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
    #[hotpath::measure(label = "usecases.semantic_config.qualify_profile", future = true)]
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
    #[hotpath::measure(label = "usecases.semantic_config.evaluate_publish", future = true)]
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
            query_fallback: prepared.query_fallback,
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

    #[hotpath::measure(label = "usecases.semantic_config.activate", future = true)]
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
            .await
            .map_err(|error| log_semantic_activation_failure("current_profile_state", error))?
            .into_state()
            .map_err(|_| {
                log_semantic_activation_failure(
                    "decode_profile_state",
                    SemanticActivationCoordinationErrorV1::Rejected,
                )
            })?;
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
            .await
            .map_err(|error| log_semantic_activation_failure("authorize", error))?;
        let candidate = self
            .accepted_profiles
            .resolve(&request.selected_profile.accepted_profile_digest)
            .await
            .map_err(map_authority_error)
            .map_err(|error| log_semantic_activation_failure("resolve_candidate", error))?;
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
            return Err(log_semantic_activation_failure(
                "match_candidate_selection",
                SemanticActivationCoordinationErrorV1::Rejected,
            ));
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
            .map_err(map_authority_error)
            .map_err(|error| log_semantic_activation_failure("resolve_current", error))?;
        let base_configuration = current_configuration_state(&self.configuration)
            .await
            .map_err(|error| log_semantic_activation_failure("current_configuration", error))?;
        let base_pin = super::SemanticConfigurationPinV1::from_current(&base_configuration)
            .map_err(|_| {
                log_semantic_activation_failure(
                    "pin_current_configuration",
                    SemanticActivationCoordinationErrorV1::Rejected,
                )
            })?;
        let preview = coordinator
            .preview_central_mutation(
                &request.authority,
                &request.central_mutation,
                &expected.expected_configuration_revision,
            )
            .await
            .map_err(|error| log_semantic_activation_failure("preview_configuration", error))?;
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
            .await
            .map_err(|error| log_semantic_activation_failure("linked_activation", error))?;
        Ok(SemanticAppliedActivationV1 {
            configuration_receipt: preview.receipt,
        })
    }

    #[hotpath::measure(label = "usecases.semantic_config.rollback", future = true)]
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

fn log_semantic_activation_failure(
    stage: &'static str,
    error: SemanticActivationCoordinationErrorV1,
) -> SemanticActivationCoordinationErrorV1 {
    let outcome = match &error {
        SemanticActivationCoordinationErrorV1::Unavailable => "unavailable",
        SemanticActivationCoordinationErrorV1::Rejected
        | SemanticActivationCoordinationErrorV1::RejectedDetail(_) => "rejected",
        SemanticActivationCoordinationErrorV1::Conflict => "conflict",
        SemanticActivationCoordinationErrorV1::Runtime(_) => "runtime_failure",
    };
    tracing::warn!(
        event = "semantic_activation_failure",
        stage,
        outcome,
        "semantic profile activation did not advance"
    );
    error
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
        _ => Err(SemanticActivationCoordinationErrorV1::RejectedDetail(
            "semantic evaluation candidate runtime does not match the verified snapshot".to_owned(),
        )),
    }
}

fn native_qualification_expectations(
    snapshot: &SemanticEvaluationPublicationSnapshotV1,
    candidate: &SemanticEvaluationProfileCandidateV1,
) -> Result<NativeQualificationExpectationsV1, SemanticActivationCoordinationErrorV1> {
    let semantic = snapshot.runtime.semantic.as_ref().ok_or_else(|| {
        SemanticActivationCoordinationErrorV1::RejectedDetail(format!(
            "semantic evaluation profile {} requires semantic runtime pins, but the verified \
             snapshot carries none",
            candidate.evaluated_profile_id
        ))
    })?;
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
        // Every remaining variant is a distinct failed invariant. Collapsing
        // them into a bare rejection erases the only evidence an operator has
        // about which package identity actually disagreed.
        rejected => SemanticActivationCoordinationErrorV1::RejectedDetail(format!(
            "packaged native qualification rejected: {rejected}"
        )),
    }
}

#[hotpath::measure(label = "usecases.semantic_config.prepare_activation")]
fn prepare_semantic_activation_publication(
    snapshot: &SemanticEvaluationPublicationSnapshotV1,
    candidate: &SemanticEvaluationProfileCandidateV1,
    evidence: &SemanticActivationPublicationEvidenceV1,
) -> Result<PreparedSemanticActivationPublicationV1, SemanticActivationCoordinationErrorV1> {
    let (report, evaluated_material) = evidence.report_and_material();
    if let Err(mismatch) = candidate_matches_evaluated_material(candidate, &evaluated_material) {
        return Err(SemanticActivationCoordinationErrorV1::RejectedDetail(
            format!(
                "semantic evaluation candidate material does not match what the evaluator ran: \
             {mismatch}"
            ),
        ));
    }
    let mut compatibility = candidate.compatibility.clone();
    if let Some(semantic) = compatibility.semantic.as_mut() {
        semantic.resources =
            semantic_resource_requirement_from_report(&report, &candidate.evaluated_profile_id)?;
    }
    let accepted_runtime = runtime_with_accepted_resources(&snapshot.runtime, &compatibility)?;
    let passing_evaluation =
        PassingRetrievalEvaluationV1::from_report(&report, &candidate.evaluated_profile_id)
            .map_err(|error| {
                SemanticActivationCoordinationErrorV1::RejectedDetail(format!(
                    "semantic evaluation report cannot certify profile {}: {error}",
                    candidate.evaluated_profile_id
                ))
            })?;
    let evaluation_anchor = passing_evaluation.evaluation_anchor().clone();
    let evaluated_profile = evaluated_material.profile;
    let profile = FusionProfile {
        profile_id: evaluated_profile.profile_id,
        evaluation_result_anchor: evaluation_anchor.clone(),
        calibrations: evaluated_profile.calibrations,
        score_domain_calibrations: evaluated_profile.score_domain_calibrations,
        minimum_calibrated_feature_micros: evaluated_profile.minimum_calibrated_feature_micros,
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
    .map_err(|error| {
        SemanticActivationCoordinationErrorV1::RejectedDetail(format!(
            "semantic evaluation profile {} cannot be accepted: {error}",
            candidate.evaluated_profile_id
        ))
    })?;
    accepted_profile
        .executable_under(&accepted_runtime)
        .map_err(|error| {
            SemanticActivationCoordinationErrorV1::RejectedDetail(format!(
                "semantic evaluation profile {} is not executable under the verified runtime: \
                 {error}; measured resources {:?}; runtime ceiling {:?}",
                candidate.evaluated_profile_id,
                accepted_profile
                    .compatibility()
                    .semantic
                    .as_ref()
                    .map(|pins| pins.resources),
                accepted_runtime.semantic_ceiling,
            ))
        })?;
    let query_fallback = prepare_query_fallback_publication(&report, &snapshot.runtime)?;
    Ok(PreparedSemanticActivationPublicationV1 {
        report,
        accepted_profile,
        accepted_runtime,
        query_fallback,
    })
}

fn prepare_query_fallback_publication(
    report: &DirectEvaluationReportV1,
    observed_runtime: &RetrievalRuntimeCompatibilityV1,
) -> Result<PreparedQueryFallbackPublicationV1, SemanticActivationCoordinationErrorV1> {
    let material =
        tracedecay_query::search_quality::load_default_evaluated_profile_material("query-fallback")
            .map_err(|error| {
                SemanticActivationCoordinationErrorV1::RejectedDetail(format!(
                    "query fallback material is unavailable: {error}"
                ))
            })?;
    let evaluation =
        PassingRetrievalEvaluationV1::from_report(report, "query-fallback").map_err(|error| {
            SemanticActivationCoordinationErrorV1::RejectedDetail(format!(
                "semantic evaluation report does not certify its query fallback: {error}"
            ))
        })?;
    let evaluation_anchor = evaluation.evaluation_anchor().clone();
    let profile = FusionProfile {
        evaluation_result_anchor: evaluation_anchor.clone(),
        ..material.profile
    };
    let diversity = DiversityPolicy {
        evaluation_result_anchor: Some(evaluation_anchor),
        ..material.diversity
    };
    let accepted_profile = AcceptedRetrievalProfileV1::new(
        profile,
        diversity,
        None,
        RetrievalCompatibilityPinsV1::default(),
        evaluation,
    )
    .map_err(|error| {
        SemanticActivationCoordinationErrorV1::RejectedDetail(format!(
            "query fallback cannot be accepted: {error}"
        ))
    })?;
    let accepted_runtime = RetrievalRuntimeCompatibilityV1 {
        retrieval_ceiling: observed_runtime.retrieval_ceiling,
        semantic: None,
        semantic_ceiling: None,
        rerank: None,
        rerank_ceiling: None,
    };
    accepted_profile
        .executable_under(&accepted_runtime)
        .map_err(|error| {
            SemanticActivationCoordinationErrorV1::RejectedDetail(format!(
                "query fallback is not executable under the verified runtime: {error}"
            ))
        })?;
    Ok(PreparedQueryFallbackPublicationV1 {
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
        .map_err(|error| {
            SemanticActivationCoordinationErrorV1::RejectedDetail(format!(
                "semantic evaluation report carries no usable resource measurement for profile \
                 {evaluated_profile_id}: {error}"
            ))
        })?;
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
            if let Some(mismatch) = semantic_pin_mismatch(observed, accepted) {
                return Err(SemanticActivationCoordinationErrorV1::RejectedDetail(
                    format!("semantic runtime pins do not match the accepted profile: {mismatch}"),
                ));
            }
            // Keep the runtime-observed configured ceiling. The accepted
            // profile's canonical `executable_under` validation below proves
            // measured report resources fit within this actual ceiling.
        }
        (None, None) => {}
        (Some(_), None) => {
            return Err(SemanticActivationCoordinationErrorV1::RejectedDetail(
                "the verified runtime carries semantic pins but the accepted profile does not"
                    .to_owned(),
            ));
        }
        (None, Some(_)) => {
            return Err(SemanticActivationCoordinationErrorV1::RejectedDetail(
                "the accepted profile carries semantic pins but the verified runtime does not"
                    .to_owned(),
            ));
        }
    }
    runtime.semantic = accepted.semantic.clone();
    Ok(runtime)
}

/// Name the first semantic pin that differs, with both observed values.
///
/// The pins are an exact-equality contract, so a bare inequality is useless to
/// an operator: every field is an opaque digest or revision and only the
/// differing one identifies which side drifted.
fn semantic_pin_mismatch(
    observed: &crate::config::retrieval::SemanticCompatibilityPinsV1,
    accepted: &crate::config::retrieval::SemanticCompatibilityPinsV1,
) -> Option<String> {
    fn differing<T: PartialEq + std::fmt::Debug>(
        field: &str,
        observed: &T,
        accepted: &T,
    ) -> Option<String> {
        (observed != accepted)
            .then(|| format!("{field} observed {observed:?}, accepted {accepted:?}"))
    }

    differing(
        "implementation_revision",
        &observed.implementation_revision,
        &accepted.implementation_revision,
    )
    .or_else(|| {
        differing(
            "fusion_revision",
            &observed.fusion_revision,
            &accepted.fusion_revision,
        )
    })
    .or_else(|| {
        differing(
            "artifact_manifest_digest",
            &observed.artifact_manifest_digest,
            &accepted.artifact_manifest_digest,
        )
    })
    .or_else(|| {
        differing(
            "runtime_compatibility_digest",
            &observed.runtime_compatibility_digest,
            &accepted.runtime_compatibility_digest,
        )
    })
    .or_else(|| differing("projection", &observed.projection, &accepted.projection))
    .or_else(|| {
        differing(
            "search_index_key",
            &observed.search_index_key,
            &accepted.search_index_key,
        )
    })
    .or_else(|| {
        differing(
            "vector_generation_id",
            &observed.vector_generation_id,
            &accepted.vector_generation_id,
        )
    })
    .or_else(|| differing("calibration", &observed.calibration, &accepted.calibration))
    .or_else(|| differing("resources", &observed.resources, &accepted.resources))
}

/// Name the first candidate field that differs from what the evaluator ran,
/// with both values.
///
/// This is an exact-equality binding between the profile material the daemon
/// proposed and the material the evaluator actually executed. A boolean answer
/// tells an operator only that *something* drifted, which is precisely the
/// signal that invites tuning a checked-in fixture value until the comparison
/// happens to agree. Naming the field and printing both sides identifies the
/// drifting authority instead.
fn candidate_matches_evaluated_material(
    candidate: &SemanticEvaluationProfileCandidateV1,
    evaluated: &DirectEvaluatedProfileMaterialV1,
) -> Result<(), String> {
    fn same<T: PartialEq + std::fmt::Debug>(
        field: &str,
        candidate: &T,
        evaluated: &T,
    ) -> Result<(), String> {
        if candidate == evaluated {
            Ok(())
        } else {
            Err(format!(
                "{field} candidate {candidate:?}, evaluator {evaluated:?}"
            ))
        }
    }

    same(
        "profile.profile_id",
        &candidate.profile.profile_id,
        &evaluated.profile.profile_id,
    )?;
    same(
        "profile.calibrations",
        &candidate.profile.calibrations,
        &evaluated.profile.calibrations,
    )?;
    same(
        "profile.score_domain_calibrations",
        &candidate.profile.score_domain_calibrations,
        &evaluated.profile.score_domain_calibrations,
    )?;
    same(
        "profile.minimum_calibrated_feature_micros",
        &candidate.profile.minimum_calibrated_feature_micros,
        &evaluated.profile.minimum_calibrated_feature_micros,
    )?;
    same(
        "profile.weights_micros",
        &candidate.profile.weights_micros,
        &evaluated.profile.weights_micros,
    )?;
    same(
        "profile.diversity_policy_id",
        &candidate.profile.diversity_policy_id,
        &evaluated.profile.diversity_policy_id,
    )?;
    same(
        "profile.rerank_policy_id",
        &candidate.profile.rerank_policy_id,
        &evaluated.profile.rerank_policy_id,
    )?;
    same(
        "profile.retrieval_budget",
        &candidate.profile.retrieval_budget,
        &evaluated.profile.retrieval_budget,
    )?;
    same(
        "diversity.policy_id",
        &candidate.diversity.policy_id,
        &evaluated.diversity.policy_id,
    )?;
    same(
        "diversity.per_source_namespace",
        &candidate.diversity.per_source_namespace,
        &evaluated.diversity.per_source_namespace,
    )?;
    same(
        "diversity.per_source_instance",
        &candidate.diversity.per_source_instance,
        &evaluated.diversity.per_source_instance,
    )?;
    same(
        "diversity.per_repository",
        &candidate.diversity.per_repository,
        &evaluated.diversity.per_repository,
    )?;
    same(
        "diversity.per_file",
        &candidate.diversity.per_file,
        &evaluated.diversity.per_file,
    )?;
    same(
        "diversity.per_session_or_thread",
        &candidate.diversity.per_session_or_thread,
        &evaluated.diversity.per_session_or_thread,
    )?;
    same(
        "diversity.per_copy_cluster",
        &candidate.diversity.per_copy_cluster,
        &evaluated.diversity.per_copy_cluster,
    )?;
    same(
        "diversity.per_evidence_role",
        &candidate.diversity.per_evidence_role,
        &evaluated.diversity.per_evidence_role,
    )?;
    match (&candidate.rerank, &evaluated.rerank) {
        (None, None) => Ok(()),
        (Some(candidate), Some(evaluated)) => {
            same(
                "rerank.policy_id",
                &candidate.policy_id,
                &evaluated.policy_id,
            )?;
            same(
                "rerank.max_candidates",
                &candidate.max_candidates,
                &evaluated.max_candidates,
            )?;
            same(
                "rerank.max_input_bytes",
                &candidate.max_input_bytes,
                &evaluated.max_input_bytes,
            )?;
            same(
                "rerank.max_input_tokens",
                &candidate.max_input_tokens,
                &evaluated.max_input_tokens,
            )?;
            same(
                "rerank.max_work_units",
                &candidate.max_work_units,
                &evaluated.max_work_units,
            )?;
            same(
                "rerank.max_model_invocations",
                &candidate.max_model_invocations,
                &evaluated.max_model_invocations,
            )?;
            same(
                "rerank.deadline_micros",
                &candidate.deadline_micros,
                &evaluated.deadline_micros,
            )
        }
        (Some(_), None) => Err("rerank candidate declares a policy, evaluator ran none".to_owned()),
        (None, Some(_)) => Err("rerank candidate declares no policy, evaluator ran one".to_owned()),
    }
}

#[hotpath::measure(label = "usecases.semantic_config.validate_snapshot")]
fn validate_evaluation_snapshot(
    repo_root: &Path,
    snapshot: &SemanticEvaluationPublicationSnapshotV1,
    candidate: &SemanticEvaluationProfileCandidateV1,
) -> Result<(), SemanticActivationCoordinationErrorV1> {
    snapshot.scope.validate().map_err(|_| {
        SemanticActivationCoordinationErrorV1::RejectedDetail(
            "semantic evaluation scope is invalid".to_owned(),
        )
    })?;
    snapshot.code_generation.validate().map_err(|_| {
        SemanticActivationCoordinationErrorV1::RejectedDetail(
            "semantic evaluation code generation is invalid".to_owned(),
        )
    })?;
    snapshot
        .code_source_manifest_digest
        .validate()
        .map_err(|_| {
            SemanticActivationCoordinationErrorV1::RejectedDetail(
                "semantic evaluation source manifest digest is invalid".to_owned(),
            )
        })?;
    snapshot.code_snapshot_digest.validate().map_err(|_| {
        SemanticActivationCoordinationErrorV1::RejectedDetail(
            "semantic evaluation code snapshot digest is invalid".to_owned(),
        )
    })?;
    snapshot
        .code_capability_manifest_digest
        .validate()
        .map_err(|_| {
            SemanticActivationCoordinationErrorV1::RejectedDetail(
                "semantic evaluation capability manifest digest is invalid".to_owned(),
            )
        })?;
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
        return Err(SemanticActivationCoordinationErrorV1::RejectedDetail(
            "semantic evaluation project or profile selection does not match the mounted authority"
                .to_owned(),
        ));
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
        _ => {
            return Err(SemanticActivationCoordinationErrorV1::RejectedDetail(
                "semantic evaluation vector, lifecycle, or runtime pins do not match the verified snapshot"
                    .to_owned(),
            ));
        }
    }
    Ok(())
}

pub struct SemanticProtectedActivationOperationV1 {
    pub authority: ConfigurationMutationAuthority,
    pub selected_profile: SemanticProfileSelection,
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

#[hotpath::measure(label = "usecases.semantic_config.read_state", future = true)]
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
                minimum_calibrated_feature_micros: BTreeMap::new(),
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
            inference_batch_size: 8,
            inference_batch_bytes: 1024,
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
            Err(SemanticActivationCoordinationErrorV1::RejectedDetail(
                "semantic evaluation vector, lifecycle, or runtime pins do not match the verified snapshot"
                    .to_owned(),
            ))
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
            Err(SemanticActivationCoordinationErrorV1::RejectedDetail(detail))
                if detail == "semantic evaluation candidate runtime does not match the verified snapshot"
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

    /// Thirteen distinct package-identity failures used to collapse into one
    /// bare rejection, so "the model does not match" and "the corpus is stale"
    /// were indistinguishable from the caller's side.
    #[test]
    fn every_packaged_qualification_failure_keeps_its_own_reason() {
        let mut reasons = std::collections::BTreeSet::new();
        for error in [
            PackagedNativeQualificationErrorV1::CorruptBytes,
            PackagedNativeQualificationErrorV1::UnsupportedSchema,
            PackagedNativeQualificationErrorV1::InvalidQualificationKey,
            PackagedNativeQualificationErrorV1::StaleWorkload,
            PackagedNativeQualificationErrorV1::StaleCorpus,
            PackagedNativeQualificationErrorV1::StaleExecutionRevision,
            PackagedNativeQualificationErrorV1::ModelMismatch,
            PackagedNativeQualificationErrorV1::BuildMismatch,
            PackagedNativeQualificationErrorV1::SearchIndexMismatch,
            PackagedNativeQualificationErrorV1::RuntimeMismatch,
            PackagedNativeQualificationErrorV1::PlatformMismatch,
            PackagedNativeQualificationErrorV1::InvalidRawOutputEvidence,
            PackagedNativeQualificationErrorV1::IncompleteNativeEvidence,
            PackagedNativeQualificationErrorV1::FailedQualification,
        ] {
            let expected = error.to_string();
            match map_packaged_qualification_error(error) {
                SemanticActivationCoordinationErrorV1::RejectedDetail(detail) => {
                    assert!(detail.contains(&expected), "{detail} omits {expected}");
                    reasons.insert(detail);
                }
                other => panic!("expected a detailed rejection, got {other:?}"),
            }
        }
        assert_eq!(
            reasons.len(),
            14,
            "package failures must stay distinguishable"
        );
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

    #[test]
    fn packaged_semantic_pass_prepares_the_exact_query_fallback() {
        let qualification: tracedecay_query::search_quality::PackagedNativeQualificationV1 =
            serde_json::from_slice(
                tracedecay_query::search_quality::packaged_native_qualification_bytes(),
            )
            .expect("reviewed packaged qualification");
        let material = tracedecay_query::search_quality::load_default_evaluated_profile_material(
            EVALUATED_PROFILE_ID,
        )
        .expect("checked-in query fallback material");
        let observed_runtime = RetrievalRuntimeCompatibilityV1 {
            retrieval_ceiling: material.profile.retrieval_budget,
            semantic: Some(semantic_compatibility(semantic_resources(10))),
            semantic_ceiling: Some(semantic_resources(20)),
            rerank: None,
            rerank_ceiling: None,
        };

        let prepared = prepare_query_fallback_publication(
            &qualification.portable_evidence.report,
            &observed_runtime,
        )
        .expect("the reviewed semantic pass also certifies its query baseline");

        assert!(prepared.accepted_profile.is_exact_query_fallback());
        assert_eq!(prepared.accepted_runtime.semantic, None);
        assert_eq!(prepared.accepted_runtime.semantic_ceiling, None);
        assert_eq!(prepared.accepted_runtime.rerank, None);
        assert_eq!(prepared.accepted_runtime.rerank_ceiling, None);
        prepared
            .accepted_profile
            .executable_under(&prepared.accepted_runtime)
            .expect("the baseline is executable under its retained runtime");
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
        let configuration = tracedecay_configuration::config::PinnedRuntimeConfiguration {
            target: tracedecay_configuration::config::RuntimeConfigurationTarget {
                project_id,
                project_root,
            },
            revision_id: ConfigurationRevisionId::try_from(
                "configuration.native-qualification-operation".to_owned(),
            )
            .expect("configuration revision"),
            snapshot: ConfigurationSnapshotV1::new(BTreeMap::new(), BTreeMap::new())
                .expect("empty configuration snapshot"),
            config: tracedecay_configuration::config::TraceDecayConfig::default(),
        };
        let (configuration, _) = ProjectConfigurationRuntime::open(
            tracedecay_configuration::config::OpenedRuntimeConfiguration::new(
                configuration,
                database.clone(),
            ),
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

        assert_rejection_names_its_invariant(&result, "profile selection");
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

        assert_rejection_names_its_invariant(&result, "vector, lifecycle, or runtime pins");
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

        assert_rejection_names_its_invariant(&result, "candidate runtime does not match");
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

        assert_rejection_names_its_invariant(&result, "candidate runtime does not match");
        assert_eq!(authority.calls(), (1, 0, 0));
        assert_eq!(authority.published_snapshot(), None);
    }

    /// Every rejection reachable from qualification or publication must name
    /// the invariant it failed. A bare `Rejected` renders as "semantic
    /// activation input was rejected", which tells an operator nothing about
    /// which of a dozen exact-equality bindings actually drifted.
    #[track_caller]
    fn assert_rejection_names_its_invariant<T>(
        result: &Result<T, SemanticActivationCoordinationErrorV1>,
        expected_invariant: &str,
    ) {
        match result {
            Err(SemanticActivationCoordinationErrorV1::RejectedDetail(detail)) => assert!(
                detail.contains(expected_invariant),
                "rejection detail {detail:?} does not name {expected_invariant:?}"
            ),
            Err(other) => panic!("expected a detailed rejection, got {other:?}"),
            Ok(_) => panic!("expected a detailed rejection, got a success"),
        }
    }

    #[test]
    fn caller_profile_material_must_match_what_direct_evaluator_runs() {
        // The evaluator fixtures are workspace-relative, not crate-relative.
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root above crates/tracedecay-usecases");
        let material = tracedecay_query::search_quality::load_direct_evaluated_profile_material(
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
                minimum_calibrated_feature_micros: material
                    .profile
                    .minimum_calibrated_feature_micros
                    .clone(),
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

        assert_eq!(
            candidate_matches_evaluated_material(&candidate, &material),
            Ok(())
        );
        *candidate
            .profile
            .weights_micros
            .get_mut(&RetrieverKind::Lexical)
            .expect("lexical weight") += 1;
        // The drift report must name the field and print both sides: a bare
        // boolean is what makes an operator reach for a fixture knob.
        let mismatch = candidate_matches_evaluated_material(&candidate, &material)
            .expect_err("a drifted weight must be reported");
        assert!(
            mismatch.starts_with("profile.weights_micros candidate ")
                && mismatch.contains("evaluator "),
            "unhelpful weight mismatch: {mismatch}"
        );
        *candidate
            .profile
            .weights_micros
            .get_mut(&RetrieverKind::Lexical)
            .expect("lexical weight") -= 1;
        candidate
            .profile
            .minimum_calibrated_feature_micros
            .insert(RetrieverKind::Lexical, 1);
        let mismatch = candidate_matches_evaluated_material(&candidate, &material)
            .expect_err("a drifted acceptance cut must be reported");
        assert!(
            mismatch.starts_with("profile.minimum_calibrated_feature_micros candidate "),
            "unhelpful acceptance-cut mismatch: {mismatch}"
        );
        let serialized = serde_json::to_string(&candidate).expect("serialize candidate");
        assert!(!serialized.contains("evaluation_result_anchor"));
    }
}
