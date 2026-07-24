//! Exact-scope activation and execution for optional semantic augmentation.
//!
//! This owner is deliberately separate from the PR9 fallback authority. PR9
//! remains an exact three-lane profile; semantic influence requires a second,
//! independently activated PASS profile carrying exact calibration and vector
//! compatibility pins.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use thiserror::Error;
use tracedecay_application::ResolvedScope;
use tracedecay_domain::{
    DiversityPolicy, EphemeralSanitizedQueryViewV1, FusionProfile, RetrievalRequest, RetrieverKind,
};

use super::CodeIndexSchedulerRegistryV1;
use super::pr9_runtime::{
    ExecutedPr9SearchV1, Pr9SearchExecutionErrorV1, Pr9SearchExecutionRequestV1,
};
use crate::application::semantic_runtime::{
    CommittedRetrievalProfileStateV1, ProductionProjectSemanticSearchBridgeV1,
    ProductionSemanticRetrievalConfigurationStoreV1, SemanticConfigurationPinV1,
    SemanticCurrentLinkedActivationV1,
};
use crate::code_index::production::CodeIndexPublishedGenerationV1;
use crate::config::retrieval::SemanticCompatibilityPinsV1;
use crate::query::retrieval::AuthorizedPr9FallbackV1;
use crate::query::retrieval::fusion::{CompositionKernel, CompositionOutputV1, FusionStageInput};
use crate::query::retrieval::semantic::{
    SemanticAbstentionV1, SemanticCalibrationEvidenceV1, SemanticExecutionControl,
    SemanticQueryModeV1, SemanticQueryServiceError, SemanticQueryServiceOutcomeV1,
    SemanticRetrievalRequestV1,
};

#[derive(Clone)]
pub(in crate::daemon) struct SemanticQueryAuthorityV1 {
    activation: SemanticCurrentLinkedActivationV1,
    profile: FusionProfile,
    diversity: DiversityPolicy,
    kernel: CompositionKernel,
}

impl SemanticQueryAuthorityV1 {
    fn from_committed(
        committed: CommittedRetrievalProfileStateV1,
    ) -> Result<Self, SemanticQueryAuthorityErrorV1> {
        let activation = committed
            .current_activation
            .ok_or(SemanticQueryAuthorityErrorV1::SemanticNotActivated)?;
        let pins = &activation.compatibility;
        let accepted = committed.state.active();
        let lanes = accepted
            .profile()
            .calibrations
            .keys()
            .copied()
            .collect::<BTreeSet<_>>();
        let expected_lanes = BTreeSet::from([
            RetrieverKind::ExactLiteral,
            RetrieverKind::Lexical,
            RetrieverKind::Graph,
            RetrieverKind::Semantic,
        ]);
        if activation.receipt.activated_generation != pins.vector_generation_id
            || pins.calibration.projection_key != *pins.projection.projection_key()
            || pins.calibration.vector_generation != pins.vector_generation_id
            || pins.calibration.canonical_digest().is_err()
            || lanes != expected_lanes
            || accepted
                .profile()
                .weights_micros
                .keys()
                .copied()
                .collect::<BTreeSet<_>>()
                != expected_lanes
            || accepted.profile().rerank_policy_id.is_some()
            || accepted.compatibility().rerank.is_some()
            || accepted.compatibility().semantic.as_ref() != Some(pins)
        {
            return Err(SemanticQueryAuthorityErrorV1::IncompatibleActivation);
        }
        let profile = accepted.profile().clone();
        let diversity = accepted.diversity().clone();
        let fusion_revision = pins.fusion_revision.clone();
        Ok(Self {
            activation,
            profile,
            diversity,
            kernel: CompositionKernel::new(fusion_revision),
        })
    }

    fn pins(&self) -> &SemanticCompatibilityPinsV1 {
        &self.activation.compatibility
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(in crate::daemon) enum SemanticQueryAuthorityErrorV1 {
    #[error("semantic configuration authority is unavailable")]
    Unavailable,
    #[error("no active semantic PASS profile exists for the exact scope")]
    SemanticNotActivated,
    #[error("semantic configuration scope does not match the admitted scope")]
    ScopeMismatch,
    #[error("semantic activation pins are incompatible")]
    IncompatibleActivation,
    #[error("semantic authority mount failed: {0}")]
    Mount(String),
}

pub(in crate::daemon) struct ExecutedPr9SemanticSearchV1 {
    pub pr9: ExecutedPr9SearchV1,
    pub semantic: SemanticAugmentationOutcomeV1,
}

pub(in crate::daemon) enum SemanticAugmentationOutcomeV1 {
    Augmented {
        composition: CompositionOutputV1,
        calibration: SemanticCalibrationEvidenceV1,
        fallback: Arc<tracedecay_domain::Pr9FallbackSubpayload>,
    },
    Fallback {
        abstention: SemanticAbstentionV1,
        fallback: Arc<tracedecay_domain::Pr9FallbackSubpayload>,
    },
}

impl SemanticAugmentationOutcomeV1 {
    fn fallback(&self) -> &Arc<tracedecay_domain::Pr9FallbackSubpayload> {
        match self {
            Self::Augmented { fallback, .. } | Self::Fallback { fallback, .. } => fallback,
        }
    }
}

#[derive(Debug, Error)]
pub(in crate::daemon) enum Pr9SemanticSearchExecutionErrorV1 {
    #[error(transparent)]
    Pr9(#[from] Pr9SearchExecutionErrorV1),
    #[error(transparent)]
    Semantic(#[from] SemanticQueryServiceError),
}

pub(crate) const fn semantic_abstention_reason(abstention: &SemanticAbstentionV1) -> &'static str {
    match abstention {
        SemanticAbstentionV1::IndexUnavailable => "semantic_index_unavailable",
        SemanticAbstentionV1::Indexing => "semantic_indexing",
        SemanticAbstentionV1::IndexDegraded => "semantic_degraded",
        SemanticAbstentionV1::IndexFailed => "semantic_failed",
        SemanticAbstentionV1::IndexStale => "semantic_generation_stale",
        SemanticAbstentionV1::IndexIncompatible => "semantic_generation_incompatible",
        SemanticAbstentionV1::CalibrationUnavailable => "calibration_unavailable",
        SemanticAbstentionV1::CalibrationInvalid => "calibration_invalid",
        SemanticAbstentionV1::CalibrationShifted => "calibration_shifted",
        SemanticAbstentionV1::NoCandidates => "semantic_no_candidates",
        SemanticAbstentionV1::BelowAcceptanceThreshold => "semantic_below_threshold",
        SemanticAbstentionV1::AmbiguousTopCandidates => "semantic_ambiguous",
        SemanticAbstentionV1::PartialCoverage => "semantic_partial",
        SemanticAbstentionV1::SemanticUnavailable => "semantic_unavailable",
        SemanticAbstentionV1::Cancelled => "semantic_cancelled",
        SemanticAbstentionV1::BudgetExceeded => "semantic_budget_exceeded",
        SemanticAbstentionV1::Denied => "semantic_denied",
        SemanticAbstentionV1::Stale => "semantic_stale",
        SemanticAbstentionV1::LaneFailure => "semantic_lane_failed",
    }
}

pub(in crate::daemon) async fn mount_current_semantic_query_authority_on_project_open(
    registry: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    scope: &ResolvedScope,
    configuration: &ProductionSemanticRetrievalConfigurationStoreV1,
    configuration_pin: &SemanticConfigurationPinV1,
) -> Result<(), SemanticQueryAuthorityErrorV1> {
    scope
        .validate()
        .map_err(|_| SemanticQueryAuthorityErrorV1::ScopeMismatch)?;
    let committed = configuration
        .current_committed_profile_state(configuration_pin)
        .await
        .map_err(|_| SemanticQueryAuthorityErrorV1::Unavailable)?;
    if committed.scope != *scope {
        return Err(SemanticQueryAuthorityErrorV1::ScopeMismatch);
    }
    registry
        .mount_semantic_query_authority_from_committed(project_root, scope, committed)
        .await
}

impl CodeIndexSchedulerRegistryV1 {
    pub(in crate::daemon) async fn mount_semantic_query_authority_from_committed(
        &self,
        project_root: &Path,
        scope: &ResolvedScope,
        committed: CommittedRetrievalProfileStateV1,
    ) -> Result<(), SemanticQueryAuthorityErrorV1> {
        if committed.scope != *scope {
            return Err(SemanticQueryAuthorityErrorV1::ScopeMismatch);
        }
        let authority = Arc::new(SemanticQueryAuthorityV1::from_committed(committed)?);
        self.mount_semantic_query_authority(project_root, scope, authority)
            .await
    }

    pub(in crate::daemon) async fn mount_semantic_query_authority(
        &self,
        project_root: &Path,
        scope: &ResolvedScope,
        authority: Arc<SemanticQueryAuthorityV1>,
    ) -> Result<(), SemanticQueryAuthorityErrorV1> {
        scope
            .validate()
            .map_err(|_| SemanticQueryAuthorityErrorV1::ScopeMismatch)?;
        let project_root = project_root
            .canonicalize()
            .map_err(|error| SemanticQueryAuthorityErrorV1::Mount(error.to_string()))?;
        let mut mounted = self.mounted.lock().await;
        let worktree = mounted
            .get_mut(&project_root)
            .ok_or(SemanticQueryAuthorityErrorV1::Unavailable)?;
        if worktree.repository_id != scope.repository_id
            || worktree.worktree_id != scope.worktree_id
        {
            return Err(SemanticQueryAuthorityErrorV1::ScopeMismatch);
        }
        worktree.semantic_query_authority = Some((scope.scope_digest.clone(), authority));
        Ok(())
    }

    pub(in crate::daemon) async fn clear_semantic_query_authority(
        &self,
        scope: &ResolvedScope,
    ) -> Result<(), SemanticQueryAuthorityErrorV1> {
        let mut mounted = self.mounted.lock().await;
        for worktree in mounted.values_mut() {
            if worktree.repository_id == scope.repository_id
                && worktree.worktree_id == scope.worktree_id
            {
                worktree.semantic_query_authority = None;
            }
        }
        Ok(())
    }

    async fn semantic_query_authority_for_scope(
        &self,
        scope: &ResolvedScope,
    ) -> Option<Arc<SemanticQueryAuthorityV1>> {
        let mounted = self.mounted.lock().await;
        let mut matched = None;
        for worktree in mounted.values() {
            if worktree.repository_id != scope.repository_id
                || worktree.worktree_id != scope.worktree_id
            {
                continue;
            }
            let (scope_digest, authority) = worktree.semantic_query_authority.as_ref()?;
            if scope_digest != &scope.scope_digest || matched.is_some() {
                return None;
            }
            matched = Some(Arc::clone(authority));
        }
        matched
    }

    /// Run canonical PR9 first, then attempt semantic influence against the
    /// same authenticated query and immutable code generation.
    pub(in crate::daemon) async fn execute_pr9_with_semantic<C>(
        &self,
        project_root: &Path,
        scope: &ResolvedScope,
        input: Pr9SearchExecutionRequestV1,
        control: &C,
        mode: SemanticQueryModeV1,
    ) -> Result<ExecutedPr9SemanticSearchV1, Pr9SemanticSearchExecutionErrorV1>
    where
        C: SemanticExecutionControl + Sync,
    {
        let pr9 = self.execute_pr9_search(scope, input).await?;
        let Some(latest) = self.generation_for(scope, &pr9.generation).await else {
            let semantic = semantic_abstention(
                mode,
                SemanticAbstentionV1::IndexStale,
                Arc::clone(&pr9.authorized.fallback),
            )?;
            return Ok(ExecutedPr9SemanticSearchV1 { pr9, semantic });
        };
        let semantic = self
            .execute_semantic_after_pr9(
                project_root,
                scope,
                &latest.generation,
                pr9.sanitized.request(),
                pr9.sanitized.query_view(),
                &pr9.authorized,
                control,
                mode,
            )
            .await?;
        Ok(ExecutedPr9SemanticSearchV1 { pr9, semantic })
    }

    /// Execute optional semantic augmentation against the exact active config,
    /// code generation, vector generation, calibration, and authenticated PR9
    /// query. Every abstention returns the original canonical PR9 `Arc`.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::daemon) async fn execute_semantic_after_pr9<C>(
        &self,
        project_root: &Path,
        scope: &ResolvedScope,
        code_generation: &CodeIndexPublishedGenerationV1,
        base: &RetrievalRequest,
        query_view: &EphemeralSanitizedQueryViewV1,
        authorized_pr9: &AuthorizedPr9FallbackV1,
        control: &C,
        mode: SemanticQueryModeV1,
    ) -> Result<SemanticAugmentationOutcomeV1, SemanticQueryServiceError>
    where
        C: SemanticExecutionControl + Sync,
    {
        let Some(authority) = self.semantic_query_authority_for_scope(scope).await else {
            return semantic_abstention(
                mode,
                SemanticAbstentionV1::CalibrationUnavailable,
                Arc::clone(&authorized_pr9.fallback),
            );
        };
        let pins = authority.pins();
        let request = SemanticRetrievalRequestV1 {
            base: base.clone(),
            query_digest: authorized_pr9.query_digest.clone(),
            query_view,
            projection: &pins.projection,
            capability_manifest_digest: pins.calibration.capability_manifest_digest.clone(),
            vector_generation: pins.vector_generation_id.clone(),
            code_generation: code_generation.manifest().generation_id.clone(),
            budget: authority.profile.retrieval_budget,
        };
        if request.validate().is_err() {
            return semantic_abstention(
                mode,
                SemanticAbstentionV1::IndexIncompatible,
                Arc::clone(&authorized_pr9.fallback),
            );
        }
        let outcome = ProductionProjectSemanticSearchBridgeV1
            .execute(
                project_root,
                code_generation,
                &request,
                Some(&pins.calibration),
                control,
                mode,
                authorized_pr9,
            )
            .await?;
        if !Arc::ptr_eq(outcome.fallback(), &authorized_pr9.fallback) {
            return Err(SemanticQueryServiceError::InvalidFallback);
        }
        match outcome {
            SemanticQueryServiceOutcomeV1::Fallback {
                abstention,
                fallback,
            } => Ok(SemanticAugmentationOutcomeV1::Fallback {
                abstention,
                fallback,
            }),
            SemanticQueryServiceOutcomeV1::Augmented {
                semantic_lane,
                calibration,
                fallback,
            } => {
                let mut lanes = authorized_pr9.pr9_lanes.clone();
                lanes.push(semantic_lane);
                let mut composition = match authority.kernel.compose(
                    &FusionStageInput {
                        profile: authority.profile.clone(),
                        lanes,
                    },
                    &authority.diversity,
                ) {
                    Ok(composition) => composition,
                    Err(_) => {
                        return semantic_abstention(
                            mode,
                            SemanticAbstentionV1::LaneFailure,
                            fallback,
                        );
                    }
                };
                composition
                    .ranked_candidates
                    .truncate(authorized_pr9.page_size);
                Ok(SemanticAugmentationOutcomeV1::Augmented {
                    composition,
                    calibration,
                    fallback,
                })
            }
        }
    }
}

fn semantic_abstention(
    mode: SemanticQueryModeV1,
    abstention: SemanticAbstentionV1,
    fallback: Arc<tracedecay_domain::Pr9FallbackSubpayload>,
) -> Result<SemanticAugmentationOutcomeV1, SemanticQueryServiceError> {
    match mode {
        SemanticQueryModeV1::FallbackAllowed => Ok(SemanticAugmentationOutcomeV1::Fallback {
            abstention,
            fallback,
        }),
        SemanticQueryModeV1::StrictSemantic => {
            Err(SemanticQueryServiceError::StrictUnavailable(abstention))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tracedecay_domain::{Pr9FallbackSubpayload, PublicRetrieverStatus, RetrieverKind};

    use super::*;

    fn fallback() -> Arc<Pr9FallbackSubpayload> {
        Arc::new(
            Pr9FallbackSubpayload::new(
                "profile.pr9.semantic-bridge.v1"
                    .to_owned()
                    .try_into()
                    .expect("profile id"),
                Vec::new(),
                BTreeMap::from([
                    (RetrieverKind::ExactLiteral, PublicRetrieverStatus::Complete),
                    (RetrieverKind::Lexical, PublicRetrieverStatus::Complete),
                    (RetrieverKind::Graph, PublicRetrieverStatus::Complete),
                ]),
                Vec::new(),
                None,
            )
            .expect("canonical PR9 fallback"),
        )
    }

    #[test]
    fn absent_activation_preserves_the_exact_fallback_arc() {
        let fallback = fallback();
        let identity = Arc::as_ptr(&fallback);
        let outcome = semantic_abstention(
            SemanticQueryModeV1::FallbackAllowed,
            SemanticAbstentionV1::CalibrationUnavailable,
            fallback,
        )
        .expect("fallback allowed");

        assert_eq!(Arc::as_ptr(outcome.fallback()), identity);
        outcome
            .fallback()
            .validate()
            .expect("canonical fallback remains valid");
    }

    #[test]
    fn strict_semantic_reports_typed_unavailable_without_a_fallback_result() {
        assert!(matches!(
            semantic_abstention(
                SemanticQueryModeV1::StrictSemantic,
                SemanticAbstentionV1::CalibrationUnavailable,
                fallback(),
            ),
            Err(SemanticQueryServiceError::StrictUnavailable(
                SemanticAbstentionV1::CalibrationUnavailable
            ))
        ));
    }
}
