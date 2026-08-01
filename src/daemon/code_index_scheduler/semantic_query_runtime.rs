//! Exact-scope activation and execution for optional semantic augmentation.
//!
//! This owner is deliberately separate from the query fallback authority. QUERY
//! remains an exact three-lane profile; semantic influence requires a second,
//! independently activated PASS profile carrying exact calibration and vector
//! compatibility pins.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use thiserror::Error;
use tracedecay_application::ResolvedScope;
use tracedecay_domain::{
    EphemeralSanitizedQueryViewV1, OptionalStagePublicStatus, RetrievalRequest, RetrieverKind,
    SemanticRetrievalContinuationV1,
};

use super::CodeIndexSchedulerRegistryV1;
use super::query_runtime::{
    ExecutedQuerySearchV1, QuerySearchExecutionErrorV1, QuerySearchExecutionRequestV1,
};
use crate::application::semantic_runtime::{
    AuthorizedProjectSemanticSearchParametersV1, CommittedRetrievalProfileStateV1,
    ProductionProjectSemanticSearchBridgeV1, ProductionSemanticRetrievalConfigurationStoreV1,
    SemanticConfigurationPinV1, SemanticCurrentLinkedActivationV1,
};
use crate::code_index::production::CodeIndexPublishedGenerationV1;
use crate::config::retrieval::{RerankCompatibilityPinsV1, SemanticCompatibilityPinsV1};
use crate::semantic_code::rerank_adapter::ProductionCodeRerankAuthorityV1;
use tracedecay_query::retrieval::AuthorizedQueryFallbackV1;
use tracedecay_query::retrieval::QueryAuthorityV1;
use tracedecay_query::retrieval::fusion::{CompositionOutputV1, digest_candidate_set};
use tracedecay_query::retrieval::rerank::RerankExecutionControlV1;
use tracedecay_query::retrieval::semantic::{
    SemanticAbstentionDispositionV1, SemanticAbstentionV1, SemanticCalibrationEvidenceV1,
    SemanticCompositionExecutionAuthorityV1, SemanticCompositionExecutionOutcomeV1,
    SemanticExecutionControl, SemanticQueryModeV1, SemanticQueryServiceError,
    SemanticRerankExecutionPortV1, SemanticRerankReadinessV1, SemanticRetrievalRequestV1,
};

#[derive(Clone)]
pub(in crate::daemon) struct SemanticQueryAuthorityV1 {
    activation: SemanticCurrentLinkedActivationV1,
    profile_digest: tracedecay_domain::ManifestDigest,
    execution: SemanticCompositionExecutionAuthorityV1,
    rerank: Option<ConfiguredRerankAuthorityV1>,
}

#[derive(Clone)]
struct ConfiguredRerankAuthorityV1 {
    pins: RerankCompatibilityPinsV1,
    mounted: Option<ProductionCodeRerankAuthorityV1>,
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
        let rerank_policy = accepted.rerank().cloned();
        let rerank_pins = accepted.compatibility().rerank.clone();
        let profile_digest = accepted.profile_digest().clone();
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
            || accepted.profile().rerank_policy_id.as_ref()
                != rerank_policy.as_ref().map(|policy| &policy.policy_id)
            || rerank_policy.is_some() != rerank_pins.is_some()
            || rerank_policy.as_ref().is_some_and(|policy| {
                policy.evaluation_result_anchor != accepted.profile().evaluation_result_anchor
            })
            || accepted.compatibility().semantic.as_ref() != Some(pins)
        {
            return Err(SemanticQueryAuthorityErrorV1::IncompatibleActivation);
        }
        let rerank = rerank_pins.map(|pins| {
            let mounted = crate::semantic_code::shared_lifecycle_owner()
                .and_then(|owner| owner.mount_reranker(pins.clone()).ok());
            ConfiguredRerankAuthorityV1 { pins, mounted }
        });
        let execution = SemanticCompositionExecutionAuthorityV1::new(
            accepted.profile().clone(),
            accepted.diversity().clone(),
            accepted.rerank().cloned(),
            pins.fusion_revision.clone(),
        )
        .map_err(|error| SemanticQueryAuthorityErrorV1::Mount(error.to_string()))?;
        Ok(Self {
            activation,
            profile_digest,
            execution,
            rerank,
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

pub(in crate::daemon) struct ExecutedQuerySemanticSearchV1 {
    pub query: ExecutedQuerySearchV1,
    pub semantic: SemanticAugmentationOutcomeV1,
}

/// Augmented payload carried behind a `Box` so the augmented arm does not
/// dominate the size of every `SemanticAugmentationOutcomeV1`, whose abstaining
/// arm holds only a reason and the shared query fallback.
pub(in crate::daemon) struct SemanticAugmentedCompositionV1 {
    pub composition: CompositionOutputV1,
    pub cursor: Option<tracedecay_domain::RetrievalCursor>,
    pub hydration_budget: tracedecay_domain::RetrievalBudget,
    pub calibration: SemanticCalibrationEvidenceV1,
    pub rerank: OptionalStagePublicStatus,
    pub fallback: Arc<tracedecay_domain::QueryFallbackSubpayload>,
}

pub(in crate::daemon) enum SemanticAugmentationOutcomeV1 {
    Augmented(Box<SemanticAugmentedCompositionV1>),
    Fallback {
        abstention: SemanticAbstentionV1,
        fallback: Arc<tracedecay_domain::QueryFallbackSubpayload>,
    },
}

impl SemanticAugmentationOutcomeV1 {
    fn fallback(&self) -> &Arc<tracedecay_domain::QueryFallbackSubpayload> {
        match self {
            Self::Augmented(augmented) => &augmented.fallback,
            Self::Fallback { fallback, .. } => fallback,
        }
    }
}

#[derive(Debug, Error)]
pub(in crate::daemon) enum QuerySemanticSearchExecutionErrorV1 {
    #[error(transparent)]
    Query(#[from] QuerySearchExecutionErrorV1),
    #[error(
        "strict semantic retrieval is unavailable for code generation {generation}: {abstention:?}"
    )]
    StrictSemanticUnavailable {
        generation: tracedecay_domain::CodeGenerationId,
        abstention: SemanticAbstentionV1,
    },
    #[error(transparent)]
    Semantic(#[from] SemanticQueryServiceError),
}

fn bind_semantic_execution_error(
    generation: &tracedecay_domain::CodeGenerationId,
    error: SemanticQueryServiceError,
) -> QuerySemanticSearchExecutionErrorV1 {
    match error {
        SemanticQueryServiceError::StrictUnavailable(abstention) => {
            QuerySemanticSearchExecutionErrorV1::StrictSemanticUnavailable {
                generation: generation.clone(),
                abstention,
            }
        }
        error => QuerySemanticSearchExecutionErrorV1::Semantic(error),
    }
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
        let mounted = self.mounted.try_lock().ok()?;
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

    /// Run canonical query first, then attempt semantic influence against the
    /// same authenticated query and immutable code generation.
    pub(in crate::daemon) async fn execute_query_with_semantic<C>(
        &self,
        project_root: &Path,
        scope: &ResolvedScope,
        input: QuerySearchExecutionRequestV1,
        control: &C,
        mode: SemanticQueryModeV1,
    ) -> Result<ExecutedQuerySemanticSearchV1, QuerySemanticSearchExecutionErrorV1>
    where
        C: SemanticExecutionControl + Sync,
    {
        let query = self.execute_query_search(scope, input).await?;
        let Some(latest) = self.generation_for(scope, &query.generation).await else {
            let semantic = semantic_abstention(
                mode,
                SemanticAbstentionV1::IndexStale,
                Arc::clone(&query.authorized.fallback),
            )
            .map_err(|error| bind_semantic_execution_error(&query.generation, error))?;
            return Ok(ExecutedQuerySemanticSearchV1 { query, semantic });
        };
        let semantic = self
            .execute_semantic_after_query(
                project_root,
                scope,
                &latest.generation,
                query.sanitized.request(),
                query.sanitized.query_view(),
                &query.authorized,
                control,
                mode,
            )
            .await
            .map_err(|error| bind_semantic_execution_error(&query.generation, error))?;
        Ok(ExecutedQuerySemanticSearchV1 { query, semantic })
    }

    /// Execute optional semantic augmentation against the exact active config,
    /// code generation, vector generation, calibration, and authenticated QUERY
    /// query. Every abstention returns the original canonical query `Arc`.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::daemon) async fn execute_semantic_after_query<C>(
        &self,
        project_root: &Path,
        scope: &ResolvedScope,
        code_generation: &CodeIndexPublishedGenerationV1,
        base: &RetrievalRequest,
        query_view: &EphemeralSanitizedQueryViewV1,
        authorized_query: &AuthorizedQueryFallbackV1,
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
                Arc::clone(&authorized_query.fallback),
            );
        };
        let pins = authority.pins();
        if !semantic_cursor_matches_activation(
            authorized_query.request_cursor.as_ref(),
            &authority.execution.profile().profile_id,
            &authority.profile_digest,
            &code_generation.manifest().generation_id,
            &pins.vector_generation_id,
            pins.projection.projection_key(),
            &pins.search_index_key,
            &pins.fusion_revision,
        ) {
            return semantic_abstention(
                mode,
                SemanticAbstentionV1::Stale,
                Arc::clone(&authorized_query.fallback),
            );
        }
        let request = SemanticRetrievalRequestV1 {
            base: base.clone(),
            query_digest: authorized_query.query_digest.clone(),
            query_view,
            projection: &pins.projection,
            search_index_key: &pins.search_index_key,
            capability_manifest_digest: pins.calibration.capability_manifest_digest.clone(),
            vector_generation: pins.vector_generation_id.clone(),
            code_generation: code_generation.manifest().generation_id.clone(),
            budget: authority.execution.profile().retrieval_budget,
        };
        if request.validate().is_err() {
            return semantic_abstention(
                mode,
                SemanticAbstentionV1::IndexIncompatible,
                Arc::clone(&authorized_query.fallback),
            );
        }
        let outcome = ProductionProjectSemanticSearchBridgeV1
            .execute(AuthorizedProjectSemanticSearchParametersV1 {
                project_root,
                code_generation,
                request: &request,
                calibration: Some(&pins.calibration),
                control,
                mode,
                authorized_query,
            })
            .await?;
        let mut rerank_executor = authority
            .rerank
            .as_ref()
            .and_then(|configured| {
                configured
                    .mounted
                    .as_ref()
                    .filter(|rerank| rerank.compatibility() == &configured.pins)
            })
            .map(|rerank| SemanticRerankExecutorV1 {
                rerank,
                code_generation,
                query_view,
                control,
            });
        let rerank_readiness = if authority.execution.rerank_policy().is_none() {
            None
        } else {
            Some(match rerank_executor.as_mut() {
                Some(executor) => SemanticRerankReadinessV1::Ready(executor),
                None => SemanticRerankReadinessV1::Unavailable(
                    tracedecay_domain::SanitizedStageFailure::AuthorityUnavailable,
                ),
            })
        };
        let outcome = authority.execution.execute(
            base,
            authorized_query,
            outcome,
            semantic_abstention_disposition(mode),
            rerank_readiness,
        )?;
        match outcome {
            SemanticCompositionExecutionOutcomeV1::Fallback {
                abstention,
                fallback,
            } => Ok(SemanticAugmentationOutcomeV1::Fallback {
                abstention,
                fallback,
            }),
            SemanticCompositionExecutionOutcomeV1::Augmented(executed) => {
                let mut composition = executed.composition;
                let Some(query_authority) = self.query_authority_for_scope(scope).await else {
                    return Err(SemanticQueryServiceError::InvalidCursor);
                };
                let cursor = paginate_semantic_composition(
                    query_authority.as_ref(),
                    base,
                    query_view,
                    authorized_query,
                    &authority.profile_digest,
                    &code_generation.manifest().generation_id,
                    &pins.vector_generation_id,
                    pins.projection.projection_key(),
                    &pins.search_index_key,
                    &pins.fusion_revision,
                    &authority.execution.profile().retrieval_budget,
                    &executed.rerank,
                    &mut composition,
                )?;
                Ok(SemanticAugmentationOutcomeV1::Augmented(Box::new(
                    SemanticAugmentedCompositionV1 {
                        composition,
                        cursor,
                        hydration_budget: authority.execution.profile().retrieval_budget,
                        calibration: executed.calibration,
                        rerank: executed.rerank,
                        fallback: executed.fallback,
                    },
                )))
            }
        }
    }
}

fn semantic_cursor_matches_activation(
    cursor: Option<&tracedecay_domain::RetrievalCursor>,
    profile_id: &tracedecay_domain::FusionProfileId,
    profile_digest: &tracedecay_domain::ManifestDigest,
    code_generation: &tracedecay_domain::CodeGenerationId,
    vector_generation: &tracedecay_domain::VectorGenerationIdV1,
    projection_key: &tracedecay_domain::ProjectionKeyV1,
    search_index_key: &tracedecay_domain::SemanticSearchIndexKeyV1,
    semantic_ranking_revision: &tracedecay_domain::ComponentRevision,
) -> bool {
    let Some(cursor) = cursor else {
        return true;
    };
    let Some(semantic) = cursor.semantic.as_ref() else {
        return cursor.next_ordinal == 0;
    };
    semantic.profile_id == *profile_id
        && semantic.profile_digest == *profile_digest
        && semantic.code_generation == *code_generation
        && semantic.vector_generation == *vector_generation
        && semantic.projection_key == *projection_key
        && semantic.search_index_key == *search_index_key
        && semantic.ranking_revision.as_str() == semantic_ranking_revision.as_str()
}

struct SemanticRerankControlV1<'a, C: ?Sized>(&'a C);

impl<C> RerankExecutionControlV1 for SemanticRerankControlV1<'_, C>
where
    C: SemanticExecutionControl + ?Sized,
{
    fn elapsed_micros(&self) -> u64 {
        self.0.elapsed_micros()
    }

    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

struct SemanticRerankExecutorV1<'a, C: ?Sized> {
    rerank: &'a ProductionCodeRerankAuthorityV1,
    code_generation: &'a CodeIndexPublishedGenerationV1,
    query_view: &'a EphemeralSanitizedQueryViewV1,
    control: &'a C,
}

impl<C> SemanticRerankExecutionPortV1 for SemanticRerankExecutorV1<'_, C>
where
    C: SemanticExecutionControl + ?Sized,
{
    fn execute_rerank(
        &mut self,
        request: &RetrievalRequest,
        policy: &tracedecay_domain::RerankPolicy,
        pre_rerank: &[tracedecay_domain::RankedCandidate],
    ) -> tracedecay_query::retrieval::rerank::BoundedRerankOutcomeV1 {
        let rerank_control = SemanticRerankControlV1(self.control);
        self.rerank.execute(
            self.code_generation,
            self.query_view,
            request,
            policy,
            pre_rerank,
            &rerank_control,
        )
    }
}

fn paginate_semantic_composition(
    query_authority: &QueryAuthorityV1,
    request: &RetrievalRequest,
    query_view: &EphemeralSanitizedQueryViewV1,
    authorized_query: &AuthorizedQueryFallbackV1,
    profile_digest: &tracedecay_domain::ManifestDigest,
    code_generation: &tracedecay_domain::CodeGenerationId,
    vector_generation: &tracedecay_domain::VectorGenerationIdV1,
    projection_key: &tracedecay_domain::ProjectionKeyV1,
    search_index_key: &tracedecay_domain::SemanticSearchIndexKeyV1,
    semantic_ranking_revision: &tracedecay_domain::ComponentRevision,
    semantic_budget: &tracedecay_domain::RetrievalBudget,
    rerank: &OptionalStagePublicStatus,
    composition: &mut CompositionOutputV1,
) -> Result<Option<tracedecay_domain::RetrievalCursor>, SemanticQueryServiceError> {
    let candidate_set_digest = digest_candidate_set(&composition.ranked_candidates)
        .map_err(|_| SemanticQueryServiceError::InvalidCursor)?;
    let ranking_revision =
        tracedecay_domain::RankingRevision::new(semantic_ranking_revision.as_str().to_owned())
            .map_err(|_| SemanticQueryServiceError::InvalidCursor)?;
    let supplied_semantic = authorized_query
        .request_cursor
        .as_ref()
        .and_then(|cursor| cursor.semantic.as_ref());
    if supplied_semantic.is_some_and(|cursor| {
        cursor.profile_id != composition.profile_id || cursor.code_generation != *code_generation
    }) {
        return Err(SemanticQueryServiceError::InvalidCursor);
    }
    let semantic_start = match supplied_semantic {
        Some(cursor)
            if cursor.profile_id == composition.profile_id
                && cursor.profile_digest == *profile_digest
                && cursor.code_generation == *code_generation
                && cursor.vector_generation == *vector_generation
                && cursor.projection_key == *projection_key
                && cursor.search_index_key == *search_index_key
                && cursor.candidate_set_digest == candidate_set_digest
                && cursor.public_lane_statuses == composition.public_lane_statuses
                && cursor.lane_checkpoints == composition.lane_checkpoints
                && cursor.ranking_revision == ranking_revision =>
        {
            cursor.next_ordinal as usize
        }
        Some(_) => return Err(SemanticQueryServiceError::InvalidCursor),
        None => 0,
    };
    if semantic_start >= composition.ranked_candidates.len() {
        return Err(SemanticQueryServiceError::InvalidCursor);
    }
    let semantic_page_size = usize::try_from(semantic_budget.max_hydrated_results)
        .map_err(|_| SemanticQueryServiceError::InvalidCursor)?;
    let semantic_page_size = semantic_page_size.min(authorized_query.page_size);
    if semantic_page_size == 0 {
        return Err(SemanticQueryServiceError::InvalidCursor);
    }
    let semantic_end = semantic_start
        .saturating_add(semantic_page_size)
        .min(composition.ranked_candidates.len());
    let page = composition.ranked_candidates[semantic_start..semantic_end].to_vec();

    let query_start = authorized_query
        .request_cursor
        .as_ref()
        .map_or(0, |cursor| cursor.next_ordinal as usize);
    if query_start > authorized_query.composition.ranked_candidates.len() {
        return Err(SemanticQueryServiceError::InvalidCursor);
    }
    let query_end = query_start
        .saturating_add(page.len())
        .min(authorized_query.composition.ranked_candidates.len());
    let has_more = semantic_end < composition.ranked_candidates.len();
    let cursor = if has_more {
        let mut cursor = query_authority
            .continuation_cursor_at(
                request,
                query_view,
                &authorized_query.composition,
                query_end,
            )
            .map_err(|_| SemanticQueryServiceError::InvalidCursor)?;
        query_authority
            .bind_semantic_continuation(
                &mut cursor,
                SemanticRetrievalContinuationV1 {
                    profile_id: composition.profile_id.clone(),
                    profile_digest: profile_digest.clone(),
                    code_generation: code_generation.clone(),
                    vector_generation: vector_generation.clone(),
                    projection_key: projection_key.clone(),
                    search_index_key: search_index_key.clone(),
                    candidate_set_digest,
                    public_lane_statuses: composition.public_lane_statuses.clone(),
                    lane_checkpoints: composition.lane_checkpoints.clone(),
                    ranking_revision,
                    rerank: rerank.clone(),
                    ordered_candidate_anchors: composition
                        .ranked_candidates
                        .iter()
                        .map(|candidate| candidate.candidate.anchor_id.clone())
                        .collect(),
                    next_ordinal: u32::try_from(semantic_end)
                        .map_err(|_| SemanticQueryServiceError::InvalidCursor)?,
                },
            )
            .map_err(|_| SemanticQueryServiceError::InvalidCursor)?;
        Some(cursor)
    } else {
        None
    };
    composition.ranked_candidates = page;
    Ok(cursor)
}

fn semantic_abstention(
    mode: SemanticQueryModeV1,
    abstention: SemanticAbstentionV1,
    fallback: Arc<tracedecay_domain::QueryFallbackSubpayload>,
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

fn semantic_abstention_disposition(mode: SemanticQueryModeV1) -> SemanticAbstentionDispositionV1 {
    match mode {
        SemanticQueryModeV1::FallbackAllowed => SemanticAbstentionDispositionV1::UseFallback,
        SemanticQueryModeV1::StrictSemantic => SemanticAbstentionDispositionV1::RejectUnavailable,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tracedecay_domain::{
        AuthorizationRevision, CalibrationProfileId, CodeGenerationId, ComponentRevision,
        DiversityPolicy, ExactClass, FreshnessVectorDigest, FusedCandidate, FusionProfile,
        LogicalEvidenceId, ManifestDigest, PrincipalId, ProjectionKeyV1, ProjectionKindV1,
        PublicRetrieverStatus, QueryFallbackSubpayload, QueryNormalizationRevision,
        RankedCandidate, RetrievalAnchorId, RetrievalBudget, RetrievalCursorKeyId,
        RetrievalRequest, RetrievalScope, RetrievalSnapshot, SanitizerRevision,
        SemanticSearchIndexKeyV1, SemanticSearchIndexProfileV1, SingleRootScopeV1, TemporalModeV1,
        UtcMicros, VectorGenerationIdV1, VectorWatermark,
    };

    use super::*;
    use tracedecay_query::retrieval::fusion::RetrievalCursorKeyringV1;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("fixture identity")
    }

    fn digest<T>(byte: char) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        id(&format!("sha256:{}", byte.to_string().repeat(64)))
    }

    fn search_index_key() -> SemanticSearchIndexKeyV1 {
        SemanticSearchIndexProfileV1::exact_flat_v1()
            .and_then(|profile| profile.index_key())
            .expect("exact-flat search index key")
    }

    fn fallback() -> Arc<QueryFallbackSubpayload> {
        Arc::new(
            QueryFallbackSubpayload::new(
                "profile.query.semantic-bridge.v1"
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
            .expect("canonical query fallback"),
        )
    }

    fn budget() -> RetrievalBudget {
        RetrievalBudget {
            max_candidates_per_lane: 16,
            max_fused_candidates: 16,
            max_hydrated_results: 8,
            max_hydration_bytes: 65_536,
            deadline_micros: None,
        }
    }

    fn semantic_budget(page_size: u32) -> RetrievalBudget {
        RetrievalBudget {
            max_hydrated_results: page_size,
            ..budget()
        }
    }

    fn query_profile() -> FusionProfile {
        let lanes = RetrieverKind::QUERY_FALLBACK_LANES;
        FusionProfile {
            profile_id: id("profile.query.pagination.v1"),
            evaluation_result_anchor: id("evaluation.query.pagination.v1"),
            calibrations: lanes
                .into_iter()
                .map(|lane| {
                    (
                        lane,
                        id::<CalibrationProfileId>(&format!(
                            "calibration.{}.pagination.v1",
                            lane.as_str()
                        )),
                    )
                })
                .collect(),
            score_domain_calibrations: BTreeMap::new(),
            weights_micros: [
                (RetrieverKind::ExactLiteral, 1_000_000),
                (RetrieverKind::Lexical, 500_000),
                (RetrieverKind::Graph, 250_000),
            ]
            .into_iter()
            .collect(),
            diversity_policy_id: id("diversity.query.pagination.v1"),
            rerank_policy_id: None,
            retrieval_budget: budget(),
        }
    }

    fn query_authority(request: &RetrievalRequest) -> QueryAuthorityV1 {
        let profile = query_profile();
        QueryAuthorityV1::new(
            profile.clone(),
            DiversityPolicy {
                policy_id: profile.diversity_policy_id,
                evaluation_result_anchor: Some(profile.evaluation_result_anchor),
                per_source_namespace: None,
                per_source_instance: None,
                per_repository: None,
                per_file: None,
                per_session_or_thread: None,
                per_copy_cluster: None,
                per_evidence_role: None,
            },
            id("ranking.query.pagination.v1"),
            RetrievalCursorKeyringV1::new(
                request.scope.privacy_domain.clone(),
                id::<RetrievalCursorKeyId>("cursor-key.query.pagination.v1"),
                7,
                vec![7_u8; 32],
                1_000_000,
            )
            .expect("cursor keyring"),
        )
        .expect("query authority")
    }

    fn request() -> RetrievalRequest {
        RetrievalRequest {
            principal: id::<PrincipalId>("principal.pagination"),
            scope: RetrievalScope {
                privacy_domain: id("privacy.pagination"),
                root: SingleRootScopeV1 {
                    repository: id("repository.pagination"),
                    worktree: None,
                    reference: None,
                },
            },
            temporal_mode: TemporalModeV1::Current,
            snapshot: RetrievalSnapshot {
                watermarks: VectorWatermark::default(),
                freshness_digest: digest::<FreshnessVectorDigest>('f'),
                authorization_revision: id::<AuthorizationRevision>("authorization.pagination.v1"),
                captured_at: UtcMicros(7),
            },
            profile_id: query_profile().profile_id,
            budget: budget(),
        }
    }

    fn ranked(ordinal: u32) -> RankedCandidate {
        RankedCandidate {
            candidate: FusedCandidate {
                anchor_id: id::<RetrievalAnchorId>(&format!("anchor.pagination.{ordinal}")),
                logical_evidence_id: id::<LogicalEvidenceId>(&format!(
                    "logical.pagination.{ordinal}"
                )),
                occurrences: Vec::new(),
                exact_class: ExactClass::Approximate,
                utility_micros: u64::from(100 - ordinal),
                contributions: Vec::new(),
                freshness: Vec::new(),
                decisions: Vec::new(),
            },
            final_ordinal: ordinal,
        }
    }

    fn composition(
        profile_id: tracedecay_domain::FusionProfileId,
        lanes: &[RetrieverKind],
    ) -> CompositionOutputV1 {
        CompositionOutputV1 {
            profile_id,
            ranked_candidates: (0..6).map(ranked).collect(),
            comparator_records: Vec::new(),
            internal_lane_outcomes: BTreeMap::new(),
            public_lane_statuses: lanes
                .iter()
                .copied()
                .map(|lane| (lane, PublicRetrieverStatus::Complete))
                .collect(),
            freshness: Vec::new(),
            lane_checkpoints: Vec::new(),
            dedupe_decisions: Vec::new(),
            diversity_decisions: Vec::new(),
        }
    }

    fn fallback_page(ordinals: &[u32]) -> Arc<QueryFallbackSubpayload> {
        let candidates = ordinals
            .iter()
            .copied()
            .enumerate()
            .map(|(page_ordinal, source_ordinal)| {
                let mut candidate = ranked(source_ordinal);
                candidate.final_ordinal = page_ordinal as u32;
                candidate
            })
            .collect();
        Arc::new(
            QueryFallbackSubpayload::new(
                query_profile().profile_id,
                candidates,
                BTreeMap::from([
                    (RetrieverKind::ExactLiteral, PublicRetrieverStatus::Complete),
                    (RetrieverKind::Lexical, PublicRetrieverStatus::Complete),
                    (RetrieverKind::Graph, PublicRetrieverStatus::Complete),
                ]),
                Vec::new(),
                None,
            )
            .expect("canonical query fallback page"),
        )
    }

    #[test]
    fn semantic_composition_resumes_across_three_authenticated_pages() {
        let request = request();
        let query_view = EphemeralSanitizedQueryViewV1::sanitize(
            "pagination",
            id::<SanitizerRevision>("sanitizer.pagination.v1"),
            id::<QueryNormalizationRevision>("normalization.pagination.v1"),
        )
        .expect("query view");
        let authority = query_authority(&request);
        let query_composition = composition(
            query_profile().profile_id,
            &RetrieverKind::QUERY_FALLBACK_LANES,
        );
        let semantic_profile =
            id::<tracedecay_domain::FusionProfileId>("profile.semantic.pagination.v1");
        let code_generation = id::<CodeGenerationId>("code-generation.pagination.v1");
        let vector_generation = VectorGenerationIdV1::new(digest::<ManifestDigest>('a'));
        let projection = ProjectionKeyV1 {
            kind: ProjectionKindV1::Embedding,
            schema_revision: "projection.pagination.v1".to_owned(),
            profile_digest: digest('b'),
        };
        let ranking_revision = id::<ComponentRevision>("ranking.semantic.pagination.v1");
        let fallback = fallback();
        let fallback_identity = Arc::as_ptr(&fallback);
        let mut request_cursor = None;
        let mut seen = Vec::new();

        for page_number in 0..3 {
            let mut semantic_composition = composition(
                semantic_profile.clone(),
                &[
                    RetrieverKind::ExactLiteral,
                    RetrieverKind::Lexical,
                    RetrieverKind::Graph,
                    RetrieverKind::Semantic,
                ],
            );
            let authorized = AuthorizedQueryFallbackV1 {
                query_digest: authority
                    .authenticate_query(&request, &query_view)
                    .expect("query digest"),
                fallback: Arc::clone(&fallback),
                composition: query_composition.clone(),
                fallback_lanes: Vec::new(),
                page_size: 5,
                request_cursor,
            };
            request_cursor = paginate_semantic_composition(
                &authority,
                &request,
                &query_view,
                &authorized,
                &digest::<ManifestDigest>('c'),
                &code_generation,
                &vector_generation,
                &projection,
                &search_index_key(),
                &ranking_revision,
                &semantic_budget(2),
                &OptionalStagePublicStatus::NotRequested,
                &mut semantic_composition,
            )
            .expect("authenticated semantic page");
            seen.extend(
                semantic_composition
                    .ranked_candidates
                    .iter()
                    .map(|candidate| candidate.final_ordinal),
            );
            assert_eq!(
                request_cursor.is_some(),
                page_number < 2,
                "only nonterminal pages continue"
            );
            if let Some(cursor) = request_cursor.as_ref() {
                assert_eq!(cursor.next_ordinal, (page_number + 1) * 2);
            }
            assert_eq!(Arc::as_ptr(&authorized.fallback), fallback_identity);
        }

        assert_eq!(seen, vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn requested_limit_caps_the_active_profile_hydration_page() {
        let request = request();
        let query_view = EphemeralSanitizedQueryViewV1::sanitize(
            "pagination",
            id::<SanitizerRevision>("sanitizer.pagination.v1"),
            id::<QueryNormalizationRevision>("normalization.pagination.v1"),
        )
        .expect("query view");
        let authority = query_authority(&request);
        let mut semantic_composition = composition(
            id("profile.semantic.pagination.v1"),
            &[
                RetrieverKind::ExactLiteral,
                RetrieverKind::Lexical,
                RetrieverKind::Graph,
                RetrieverKind::Semantic,
            ],
        );
        let authorized = AuthorizedQueryFallbackV1 {
            query_digest: authority
                .authenticate_query(&request, &query_view)
                .expect("query digest"),
            fallback: fallback(),
            composition: composition(
                query_profile().profile_id,
                &RetrieverKind::QUERY_FALLBACK_LANES,
            ),
            fallback_lanes: Vec::new(),
            page_size: 1,
            request_cursor: None,
        };

        let cursor = paginate_semantic_composition(
            &authority,
            &request,
            &query_view,
            &authorized,
            &digest::<ManifestDigest>('c'),
            &id::<CodeGenerationId>("code-generation.pagination.v1"),
            &VectorGenerationIdV1::new(digest::<ManifestDigest>('a')),
            &ProjectionKeyV1 {
                kind: ProjectionKindV1::Embedding,
                schema_revision: "projection.pagination.v1".to_owned(),
                profile_digest: digest('b'),
            },
            &search_index_key(),
            &id::<ComponentRevision>("ranking.semantic.pagination.v1"),
            &semantic_budget(2),
            &OptionalStagePublicStatus::NotRequested,
            &mut semantic_composition,
        )
        .expect("bounded semantic page")
        .expect("continuation");

        assert_eq!(semantic_composition.ranked_candidates.len(), 1);
        assert_eq!(cursor.next_ordinal, 1);
        assert_eq!(cursor.semantic.expect("semantic cursor").next_ordinal, 1);
    }

    #[test]
    fn frozen_rerank_order_is_restored_without_reexecuting_the_optional_stage() {
        let request = request();
        let query_view = EphemeralSanitizedQueryViewV1::sanitize(
            "pagination",
            id::<SanitizerRevision>("sanitizer.pagination.v1"),
            id::<QueryNormalizationRevision>("normalization.pagination.v1"),
        )
        .expect("query view");
        let authority = query_authority(&request);
        let query_composition = composition(
            query_profile().profile_id,
            &RetrieverKind::QUERY_FALLBACK_LANES,
        );
        let profile_digest = digest::<ManifestDigest>('c');
        let code_generation = id::<CodeGenerationId>("code-generation.pagination.v1");
        let vector_generation = VectorGenerationIdV1::new(digest::<ManifestDigest>('a'));
        let projection = ProjectionKeyV1 {
            kind: ProjectionKindV1::Embedding,
            schema_revision: "projection.pagination.v1".to_owned(),
            profile_digest: digest('b'),
        };
        let ranking_revision = id::<ComponentRevision>("ranking.semantic.pagination.v1");
        let mut first = composition(
            id("profile.semantic.pagination.v1"),
            &[
                RetrieverKind::ExactLiteral,
                RetrieverKind::Lexical,
                RetrieverKind::Graph,
                RetrieverKind::Semantic,
            ],
        );
        first.ranked_candidates.reverse();
        for (ordinal, candidate) in first.ranked_candidates.iter_mut().enumerate() {
            candidate.final_ordinal = ordinal as u32;
        }
        let authorized = AuthorizedQueryFallbackV1 {
            query_digest: authority
                .authenticate_query(&request, &query_view)
                .expect("query digest"),
            fallback: fallback(),
            composition: query_composition.clone(),
            fallback_lanes: Vec::new(),
            page_size: 2,
            request_cursor: None,
        };
        let cursor = paginate_semantic_composition(
            &authority,
            &request,
            &query_view,
            &authorized,
            &profile_digest,
            &code_generation,
            &vector_generation,
            &projection,
            &search_index_key(),
            &ranking_revision,
            &semantic_budget(2),
            &OptionalStagePublicStatus::Complete,
            &mut first,
        )
        .expect("first reranked page")
        .expect("continuation");
        let continuation = cursor.semantic.as_ref().expect("semantic continuation");
        let continuation_rerank = continuation.rerank.clone();

        let mut resumed = composition(
            id("profile.semantic.pagination.v1"),
            &[
                RetrieverKind::ExactLiteral,
                RetrieverKind::Lexical,
                RetrieverKind::Graph,
                RetrieverKind::Semantic,
            ],
        );
        tracedecay_query::retrieval::semantic::restore_frozen_semantic_order(
            continuation,
            &mut resumed,
        )
        .expect("authenticated order restores");
        assert_eq!(
            resumed
                .ranked_candidates
                .iter()
                .map(|candidate| candidate.candidate.anchor_id.as_str())
                .collect::<Vec<_>>(),
            [
                "anchor.pagination.5",
                "anchor.pagination.4",
                "anchor.pagination.3",
                "anchor.pagination.2",
                "anchor.pagination.1",
                "anchor.pagination.0",
            ]
        );
        let authorized = AuthorizedQueryFallbackV1 {
            query_digest: authority
                .authenticate_query(&request, &query_view)
                .expect("query digest"),
            fallback: fallback(),
            composition: query_composition,
            fallback_lanes: Vec::new(),
            page_size: 2,
            request_cursor: Some(cursor),
        };
        let next = paginate_semantic_composition(
            &authority,
            &request,
            &query_view,
            &authorized,
            &profile_digest,
            &code_generation,
            &vector_generation,
            &projection,
            &search_index_key(),
            &ranking_revision,
            &semantic_budget(2),
            &continuation_rerank,
            &mut resumed,
        )
        .expect("second frozen page");

        assert!(next.is_some());
        assert_eq!(
            resumed
                .ranked_candidates
                .iter()
                .map(|candidate| candidate.candidate.anchor_id.as_str())
                .collect::<Vec<_>>(),
            ["anchor.pagination.3", "anchor.pagination.2"]
        );
    }

    #[test]
    fn augmented_pagination_stops_when_semantic_candidates_end_before_query() {
        let request = request();
        let query_view = EphemeralSanitizedQueryViewV1::sanitize(
            "pagination",
            id::<SanitizerRevision>("sanitizer.pagination.v1"),
            id::<QueryNormalizationRevision>("normalization.pagination.v1"),
        )
        .expect("query view");
        let authority = query_authority(&request);
        let query_composition = composition(
            query_profile().profile_id,
            &RetrieverKind::QUERY_FALLBACK_LANES,
        );
        let mut semantic_composition = composition(
            id("profile.semantic.pagination.v1"),
            &[
                RetrieverKind::ExactLiteral,
                RetrieverKind::Lexical,
                RetrieverKind::Graph,
                RetrieverKind::Semantic,
            ],
        );
        semantic_composition.ranked_candidates.truncate(2);
        let authorized = AuthorizedQueryFallbackV1 {
            query_digest: authority
                .authenticate_query(&request, &query_view)
                .expect("query digest"),
            fallback: fallback(),
            composition: query_composition,
            fallback_lanes: Vec::new(),
            page_size: 5,
            request_cursor: None,
        };

        let cursor = paginate_semantic_composition(
            &authority,
            &request,
            &query_view,
            &authorized,
            &digest::<ManifestDigest>('c'),
            &id::<CodeGenerationId>("code-generation.pagination.v1"),
            &VectorGenerationIdV1::new(digest::<ManifestDigest>('a')),
            &ProjectionKeyV1 {
                kind: ProjectionKindV1::Embedding,
                schema_revision: "projection.pagination.v1".to_owned(),
                profile_digest: digest('b'),
            },
            &search_index_key(),
            &id::<ComponentRevision>("ranking.semantic.pagination.v1"),
            &semantic_budget(2),
            &OptionalStagePublicStatus::NotRequested,
            &mut semantic_composition,
        )
        .expect("terminal semantic page");

        assert!(
            cursor.is_none(),
            "query remainder cannot extend semantic paging"
        );
        assert_eq!(
            semantic_composition
                .ranked_candidates
                .iter()
                .map(|candidate| candidate.final_ordinal)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    #[test]
    fn semantic_activation_mid_query_pagination_preserves_the_existing_page() {
        let request = request();
        let query_view = EphemeralSanitizedQueryViewV1::sanitize(
            "pagination",
            id::<SanitizerRevision>("sanitizer.pagination.v1"),
            id::<QueryNormalizationRevision>("normalization.pagination.v1"),
        )
        .expect("query view");
        let authority = query_authority(&request);
        let query_composition = composition(
            query_profile().profile_id,
            &RetrieverKind::QUERY_FALLBACK_LANES,
        );
        let legacy_cursor = authority
            .continuation_cursor_at(&request, &query_view, &query_composition, 2)
            .expect("authenticated query continuation");
        let semantic_profile =
            id::<tracedecay_domain::FusionProfileId>("profile.semantic.pagination.v1");
        let code_generation = id::<CodeGenerationId>("code-generation.pagination.v1");
        let vector_generation = VectorGenerationIdV1::new(digest::<ManifestDigest>('a'));
        let projection = ProjectionKeyV1 {
            kind: ProjectionKindV1::Embedding,
            schema_revision: "projection.pagination.v1".to_owned(),
            profile_digest: digest('b'),
        };
        let ranking_revision = id::<ComponentRevision>("ranking.semantic.pagination.v1");

        assert!(!semantic_cursor_matches_activation(
            Some(&legacy_cursor),
            &semantic_profile,
            &digest::<ManifestDigest>('c'),
            &code_generation,
            &vector_generation,
            &projection,
            &search_index_key(),
            &ranking_revision,
        ));
        let fallback = fallback_page(&[2, 3]);
        let identity = Arc::as_ptr(&fallback);
        let outcome = semantic_abstention(
            SemanticQueryModeV1::FallbackAllowed,
            SemanticAbstentionV1::Stale,
            fallback,
        )
        .expect("legacy continuation falls back");

        assert_eq!(Arc::as_ptr(outcome.fallback()), identity);
        let mut seen = vec![
            "anchor.pagination.0".to_owned(),
            "anchor.pagination.1".to_owned(),
        ];
        seen.extend(
            outcome
                .fallback()
                .ordered_candidates
                .iter()
                .map(|candidate| candidate.candidate.anchor_id.as_str().to_owned()),
        );
        assert_eq!(
            seen,
            [
                "anchor.pagination.0",
                "anchor.pagination.1",
                "anchor.pagination.2",
                "anchor.pagination.3",
            ]
        );
    }

    #[test]
    fn semantic_profile_change_mid_pagination_preserves_the_query_page() {
        let request = request();
        let query_view = EphemeralSanitizedQueryViewV1::sanitize(
            "pagination",
            id::<SanitizerRevision>("sanitizer.pagination.v1"),
            id::<QueryNormalizationRevision>("normalization.pagination.v1"),
        )
        .expect("query view");
        let authority = query_authority(&request);
        let query_composition = composition(
            query_profile().profile_id,
            &RetrieverKind::QUERY_FALLBACK_LANES,
        );
        let original_profile =
            id::<tracedecay_domain::FusionProfileId>("profile.semantic.pagination.v1");
        let changed_profile =
            id::<tracedecay_domain::FusionProfileId>("profile.semantic.pagination.v2");
        let code_generation = id::<CodeGenerationId>("code-generation.pagination.v1");
        let vector_generation = VectorGenerationIdV1::new(digest::<ManifestDigest>('a'));
        let projection = ProjectionKeyV1 {
            kind: ProjectionKindV1::Embedding,
            schema_revision: "projection.pagination.v1".to_owned(),
            profile_digest: digest('b'),
        };
        let ranking_revision = id::<ComponentRevision>("ranking.semantic.pagination.v1");
        let mut semantic_composition = composition(
            original_profile.clone(),
            &[
                RetrieverKind::ExactLiteral,
                RetrieverKind::Lexical,
                RetrieverKind::Graph,
                RetrieverKind::Semantic,
            ],
        );
        let authorized = AuthorizedQueryFallbackV1 {
            query_digest: authority
                .authenticate_query(&request, &query_view)
                .expect("query digest"),
            fallback: fallback_page(&[0, 1]),
            composition: query_composition,
            fallback_lanes: Vec::new(),
            page_size: 2,
            request_cursor: None,
        };
        let cursor = paginate_semantic_composition(
            &authority,
            &request,
            &query_view,
            &authorized,
            &digest::<ManifestDigest>('c'),
            &code_generation,
            &vector_generation,
            &projection,
            &search_index_key(),
            &ranking_revision,
            &semantic_budget(2),
            &OptionalStagePublicStatus::NotRequested,
            &mut semantic_composition,
        )
        .expect("first semantic page")
        .expect("semantic continuation");

        assert!(semantic_cursor_matches_activation(
            Some(&cursor),
            &original_profile,
            &digest::<ManifestDigest>('c'),
            &code_generation,
            &vector_generation,
            &projection,
            &search_index_key(),
            &ranking_revision,
        ));
        assert!(!semantic_cursor_matches_activation(
            Some(&cursor),
            &original_profile,
            &digest::<ManifestDigest>('d'),
            &code_generation,
            &vector_generation,
            &projection,
            &search_index_key(),
            &ranking_revision,
        ));
        assert!(!semantic_cursor_matches_activation(
            Some(&cursor),
            &changed_profile,
            &digest::<ManifestDigest>('c'),
            &code_generation,
            &vector_generation,
            &projection,
            &search_index_key(),
            &ranking_revision,
        ));
        let mut changed_search_index = search_index_key();
        changed_search_index.profile_digest = digest('e');
        assert!(!semantic_cursor_matches_activation(
            Some(&cursor),
            &original_profile,
            &digest::<ManifestDigest>('c'),
            &code_generation,
            &vector_generation,
            &projection,
            &changed_search_index,
            &ranking_revision,
        ));
        let fallback = fallback_page(&[2, 3]);
        let identity = Arc::as_ptr(&fallback);
        let outcome = semantic_abstention(
            SemanticQueryModeV1::FallbackAllowed,
            SemanticAbstentionV1::Stale,
            fallback,
        )
        .expect("profile drift falls back");
        assert_eq!(Arc::as_ptr(outcome.fallback()), identity);
        let mut seen = vec![
            "anchor.pagination.0".to_owned(),
            "anchor.pagination.1".to_owned(),
        ];
        seen.extend(
            outcome
                .fallback()
                .ordered_candidates
                .iter()
                .map(|candidate| candidate.candidate.anchor_id.as_str().to_owned()),
        );
        assert_eq!(
            seen,
            [
                "anchor.pagination.0",
                "anchor.pagination.1",
                "anchor.pagination.2",
                "anchor.pagination.3",
            ]
        );
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

    #[test]
    fn strict_semantic_execution_error_preserves_the_query_generation() {
        let generation = id::<CodeGenerationId>("code-generation.strict-semantic-selected.v1");
        let error = bind_semantic_execution_error(
            &generation,
            SemanticQueryServiceError::StrictUnavailable(
                SemanticAbstentionV1::CalibrationUnavailable,
            ),
        );

        assert!(matches!(
            error,
            QuerySemanticSearchExecutionErrorV1::StrictSemanticUnavailable {
                generation: selected,
                abstention: SemanticAbstentionV1::CalibrationUnavailable,
            } if selected == generation
        ));
    }
}
