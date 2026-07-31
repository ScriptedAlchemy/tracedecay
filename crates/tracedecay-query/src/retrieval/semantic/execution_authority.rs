//! Transport-independent semantic composition and optional rerank execution.
//!
//! This authority consumes the calibrated semantic lane decision and the
//! already-authorized query fallback. It never reconstructs the fallback
//! subpayload: every outcome carries the exact same caller-owned [`Arc`].

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use thiserror::Error;
use tracedecay_domain::{
    ComponentRevision, DiversityPolicy, FusionProfile, OptionalStagePublicStatus,
    QueryFallbackSubpayload, RankedCandidate, RerankPolicy, RetrievalRequest, RetrieverKind,
    SanitizedStageFailure, SemanticRetrievalContinuationV1,
};

use super::{
    SemanticAbstentionDispositionV1, SemanticAbstentionV1, SemanticCalibrationEvidenceV1,
    SemanticQueryServiceError, SemanticQueryServiceOutcomeV1,
};
use crate::retrieval::AuthorizedQueryFallbackV1;
use crate::retrieval::fusion::{CompositionKernel, CompositionOutputV1, FusionStageInput};
use crate::retrieval::rerank::BoundedRerankOutcomeV1;

/// Invalid immutable configuration supplied to the composition authority.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SemanticCompositionAuthorityErrorV1 {
    #[error("invalid semantic composition authority: {0}")]
    InvalidAuthority(String),
}

/// Query-layer port for one already-mounted deterministic local reranker.
///
/// Daemon adapters may capture generation-bound view authority and execution
/// control, but the query authority depends only on the bounded rerank
/// contract and transport-independent retrieval values.
pub trait SemanticRerankExecutionPortV1 {
    fn execute_rerank(
        &mut self,
        request: &RetrievalRequest,
        policy: &RerankPolicy,
        pre_rerank: &[RankedCandidate],
    ) -> BoundedRerankOutcomeV1;
}

/// Typed readiness of the configured optional rerank stage.
pub enum SemanticRerankReadinessV1<'a> {
    Ready(&'a mut dyn SemanticRerankExecutionPortV1),
    Unavailable(SanitizedStageFailure),
}

/// Successful semantic composition before paging and hydration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutedSemanticCompositionV1 {
    pub composition: CompositionOutputV1,
    pub calibration: SemanticCalibrationEvidenceV1,
    pub rerank: OptionalStagePublicStatus,
    pub fallback: Arc<QueryFallbackSubpayload>,
}

/// Semantic influence or the exact typed reason that query fallback remained authoritative.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticCompositionExecutionOutcomeV1 {
    Augmented(Box<ExecutedSemanticCompositionV1>),
    Fallback {
        abstention: SemanticAbstentionV1,
        fallback: Arc<QueryFallbackSubpayload>,
    },
}

impl SemanticCompositionExecutionOutcomeV1 {
    pub fn fallback(&self) -> &Arc<QueryFallbackSubpayload> {
        match self {
            Self::Augmented(executed) => &executed.fallback,
            Self::Fallback { fallback, .. } => fallback,
        }
    }
}

/// Immutable authority for semantic recomposition and optional rerank.
///
/// [`CompositionKernel`] remains the sole implementation of fusion, dedupe,
/// and diversity. This owner adds only semantic fallback identity enforcement
/// and the execute-or-restore decision for the optional rerank stage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticCompositionExecutionAuthorityV1 {
    profile: FusionProfile,
    diversity: DiversityPolicy,
    rerank_policy: Option<RerankPolicy>,
    composition: CompositionKernel,
}

impl SemanticCompositionExecutionAuthorityV1 {
    pub fn new(
        profile: FusionProfile,
        diversity: DiversityPolicy,
        rerank_policy: Option<RerankPolicy>,
        ranking_revision: ComponentRevision,
    ) -> Result<Self, SemanticCompositionAuthorityErrorV1> {
        validate_authority(&profile, &diversity, rerank_policy.as_ref())?;
        Ok(Self {
            profile,
            diversity,
            rerank_policy,
            composition: CompositionKernel::new(ranking_revision),
        })
    }

    pub fn profile(&self) -> &FusionProfile {
        &self.profile
    }

    pub fn diversity(&self) -> &DiversityPolicy {
        &self.diversity
    }

    pub fn rerank_policy(&self) -> Option<&RerankPolicy> {
        self.rerank_policy.as_ref()
    }

    /// Compose the original query lane inputs with one admitted semantic lane.
    ///
    /// Typed semantic abstentions pass through unchanged. Composition failure
    /// becomes a typed lane abstention, while strict mode remains unavailable.
    /// An authenticated continuation restores its frozen order and never
    /// invokes the current reranker.
    pub fn execute(
        &self,
        request: &RetrievalRequest,
        authorized_query: &AuthorizedQueryFallbackV1,
        semantic: SemanticQueryServiceOutcomeV1,
        on_abstention: SemanticAbstentionDispositionV1,
        rerank: Option<SemanticRerankReadinessV1<'_>>,
    ) -> Result<SemanticCompositionExecutionOutcomeV1, SemanticQueryServiceError> {
        if authorized_query.fallback.validate().is_err()
            || !Arc::ptr_eq(semantic.fallback(), &authorized_query.fallback)
        {
            return Err(SemanticQueryServiceError::InvalidFallback);
        }

        let (semantic_lane, calibration, fallback) = match semantic {
            SemanticQueryServiceOutcomeV1::Fallback {
                abstention,
                fallback,
            } => {
                return semantic_abstention(on_abstention, abstention, fallback);
            }
            SemanticQueryServiceOutcomeV1::Augmented {
                semantic_lane,
                calibration,
                fallback,
            } => (semantic_lane, calibration, fallback),
        };

        let mut lanes = authorized_query.fallback_lanes.clone();
        lanes.push(semantic_lane);
        let mut composition = match self.composition.compose(
            &FusionStageInput {
                profile: self.profile.clone(),
                lanes,
            },
            &self.diversity,
        ) {
            Ok(composition) => composition,
            Err(_) => {
                return semantic_abstention(
                    on_abstention,
                    SemanticAbstentionV1::LaneFailure,
                    fallback,
                );
            }
        };

        let rerank = match authorized_query
            .request_cursor
            .as_ref()
            .and_then(|cursor| cursor.semantic.as_ref())
        {
            Some(continuation) => {
                validate_restored_rerank_status(self.rerank_policy.as_ref(), &continuation.rerank)?;
                restore_frozen_semantic_order(continuation, &mut composition)?;
                continuation.rerank.clone()
            }
            None => self.execute_optional_rerank(request, rerank, &mut composition),
        };

        Ok(SemanticCompositionExecutionOutcomeV1::Augmented(Box::new(
            ExecutedSemanticCompositionV1 {
                composition,
                calibration,
                rerank,
                fallback,
            },
        )))
    }

    fn execute_optional_rerank(
        &self,
        request: &RetrievalRequest,
        readiness: Option<SemanticRerankReadinessV1<'_>>,
        composition: &mut CompositionOutputV1,
    ) -> OptionalStagePublicStatus {
        let Some(policy) = self.rerank_policy.as_ref() else {
            return OptionalStagePublicStatus::NotRequested;
        };
        let Some(readiness) = readiness else {
            return OptionalStagePublicStatus::Unavailable(
                SanitizedStageFailure::AuthorityUnavailable,
            );
        };
        let executor = match readiness {
            SemanticRerankReadinessV1::Ready(executor) => executor,
            SemanticRerankReadinessV1::Unavailable(reason) => {
                return OptionalStagePublicStatus::Unavailable(reason);
            }
        };

        let original = composition.ranked_candidates.clone();
        let outcome = executor.execute_rerank(request, policy, &original);
        apply_rerank_outcome(original, outcome, composition)
    }
}

/// Restore the authenticated rerank order without rerunning an optional stage.
pub fn restore_frozen_semantic_order(
    continuation: &SemanticRetrievalContinuationV1,
    composition: &mut CompositionOutputV1,
) -> Result<(), SemanticQueryServiceError> {
    continuation
        .validate()
        .map_err(|_| SemanticQueryServiceError::InvalidCursor)?;
    let mut by_anchor = composition
        .ranked_candidates
        .drain(..)
        .map(|candidate| (candidate.candidate.anchor_id.clone(), candidate))
        .collect::<BTreeMap<_, _>>();
    if by_anchor.len() != continuation.ordered_candidate_anchors.len() {
        return Err(SemanticQueryServiceError::InvalidCursor);
    }

    let mut ordered = Vec::with_capacity(by_anchor.len());
    for anchor in &continuation.ordered_candidate_anchors {
        ordered.push(
            by_anchor
                .remove(anchor)
                .ok_or(SemanticQueryServiceError::InvalidCursor)?,
        );
    }
    if !by_anchor.is_empty() {
        return Err(SemanticQueryServiceError::InvalidCursor);
    }
    for (ordinal, candidate) in ordered.iter_mut().enumerate() {
        candidate.final_ordinal =
            u32::try_from(ordinal).map_err(|_| SemanticQueryServiceError::InvalidCursor)?;
    }
    composition.ranked_candidates = ordered;
    Ok(())
}

fn validate_authority(
    profile: &FusionProfile,
    diversity: &DiversityPolicy,
    rerank: Option<&RerankPolicy>,
) -> Result<(), SemanticCompositionAuthorityErrorV1> {
    profile.retrieval_budget.validate().map_err(|error| {
        SemanticCompositionAuthorityErrorV1::InvalidAuthority(error.to_string())
    })?;
    let expected_lanes = BTreeSet::from([
        RetrieverKind::ExactLiteral,
        RetrieverKind::Lexical,
        RetrieverKind::Graph,
        RetrieverKind::Semantic,
    ]);
    if profile
        .calibrations
        .keys()
        .copied()
        .collect::<BTreeSet<_>>()
        != expected_lanes
        || profile
            .weights_micros
            .keys()
            .copied()
            .collect::<BTreeSet<_>>()
            != expected_lanes
    {
        return Err(SemanticCompositionAuthorityErrorV1::InvalidAuthority(
            "profile must authorize exact, lexical, graph, and semantic exactly once".to_owned(),
        ));
    }
    if diversity.policy_id != profile.diversity_policy_id
        || diversity.evaluation_result_anchor.as_ref() != Some(&profile.evaluation_result_anchor)
    {
        return Err(SemanticCompositionAuthorityErrorV1::InvalidAuthority(
            "diversity policy is not bound to the accepted semantic evaluation".to_owned(),
        ));
    }
    match (&profile.rerank_policy_id, rerank) {
        (None, None) => {}
        (Some(expected), Some(policy))
            if expected == &policy.policy_id
                && policy.evaluation_result_anchor == profile.evaluation_result_anchor => {}
        _ => {
            return Err(SemanticCompositionAuthorityErrorV1::InvalidAuthority(
                "rerank policy is not bound to the accepted semantic evaluation".to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_restored_rerank_status(
    policy: Option<&RerankPolicy>,
    status: &OptionalStagePublicStatus,
) -> Result<(), SemanticQueryServiceError> {
    if policy.is_none() != matches!(status, OptionalStagePublicStatus::NotRequested) {
        return Err(SemanticQueryServiceError::InvalidCursor);
    }
    Ok(())
}

fn apply_rerank_outcome(
    original: Vec<RankedCandidate>,
    outcome: BoundedRerankOutcomeV1,
    composition: &mut CompositionOutputV1,
) -> OptionalStagePublicStatus {
    if outcome.public_status == OptionalStagePublicStatus::Complete
        && is_candidate_permutation(&original, &outcome.ordered_candidates)
    {
        composition.ranked_candidates = outcome.ordered_candidates;
        OptionalStagePublicStatus::Complete
    } else if outcome.public_status == OptionalStagePublicStatus::Complete {
        composition.ranked_candidates = original;
        OptionalStagePublicStatus::Rejected(SanitizedStageFailure::Invalid)
    } else {
        // Optional-stage failure must restore the exact pre-rerank value,
        // irrespective of what an executor placed in its outcome.
        composition.ranked_candidates = original;
        outcome.public_status
    }
}

fn is_candidate_permutation(original: &[RankedCandidate], reranked: &[RankedCandidate]) -> bool {
    if original.len() != reranked.len()
        || reranked
            .iter()
            .enumerate()
            .any(|(ordinal, candidate)| candidate.final_ordinal != ordinal as u32)
    {
        return false;
    }
    let original_anchors = original
        .iter()
        .map(|candidate| &candidate.candidate.anchor_id)
        .collect::<BTreeSet<_>>();
    let reranked_anchors = reranked
        .iter()
        .map(|candidate| &candidate.candidate.anchor_id)
        .collect::<BTreeSet<_>>();
    original_anchors.len() == original.len() && original_anchors == reranked_anchors
}

fn semantic_abstention(
    disposition: SemanticAbstentionDispositionV1,
    abstention: SemanticAbstentionV1,
    fallback: Arc<QueryFallbackSubpayload>,
) -> Result<SemanticCompositionExecutionOutcomeV1, SemanticQueryServiceError> {
    match disposition {
        SemanticAbstentionDispositionV1::UseFallback => {
            Ok(SemanticCompositionExecutionOutcomeV1::Fallback {
                abstention,
                fallback,
            })
        }
        SemanticAbstentionDispositionV1::RejectUnavailable => {
            Err(SemanticQueryServiceError::StrictUnavailable(abstention))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tracedecay_domain::{
        AuthorizationRevision, CalibrationProfileId, CandidateSetDigest, ExactClass,
        FreshnessVectorDigest, FusedCandidate, LogicalEvidenceId, ManifestDigest, PrincipalId,
        ProjectionKeyV1, ProjectionKindV1, PublicRetrieverStatus, QueryDigest, QueryMac,
        RankingRevision, RetrievalAnchorId, RetrievalBudget, RetrievalScope, RetrievalSnapshot,
        RetrieverBatch, RetrieverCoverage, RetrieverOutcome, SanitizedBudgetUsage,
        SemanticSearchIndexProfileV1, SingleRootScopeV1, SourceFreshness, TemporalModeV1,
        UtcMicros, VectorGenerationIdV1, VectorWatermark,
    };

    use super::*;
    use crate::retrieval::fusion::CompositionLaneInput;
    use crate::retrieval::rerank::RerankUsageV1;

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

    fn budget() -> RetrievalBudget {
        RetrievalBudget {
            max_candidates_per_lane: 8,
            max_fused_candidates: 8,
            max_hydrated_results: 4,
            max_hydration_bytes: 4_096,
            deadline_micros: None,
        }
    }

    fn profile(rerank_policy_id: Option<tracedecay_domain::RerankPolicyId>) -> FusionProfile {
        let lanes = [
            RetrieverKind::ExactLiteral,
            RetrieverKind::Lexical,
            RetrieverKind::Graph,
            RetrieverKind::Semantic,
        ];
        FusionProfile {
            profile_id: id("profile.semantic-execution.v1"),
            evaluation_result_anchor: id("evaluation.semantic-execution.v1"),
            calibrations: lanes
                .into_iter()
                .map(|lane| {
                    (
                        lane,
                        id::<CalibrationProfileId>(&format!(
                            "calibration.{}.semantic-execution.v1",
                            lane.as_str()
                        )),
                    )
                })
                .collect(),
            score_domain_calibrations: BTreeMap::new(),
            weights_micros: lanes.into_iter().map(|lane| (lane, 1_000_000)).collect(),
            diversity_policy_id: id("diversity.semantic-execution.v1"),
            rerank_policy_id,
            retrieval_budget: budget(),
        }
    }

    fn diversity() -> DiversityPolicy {
        DiversityPolicy {
            policy_id: id("diversity.semantic-execution.v1"),
            evaluation_result_anchor: Some(id("evaluation.semantic-execution.v1")),
            per_source_namespace: None,
            per_source_instance: None,
            per_repository: None,
            per_file: None,
            per_session_or_thread: None,
            per_copy_cluster: None,
            per_evidence_role: None,
        }
    }

    fn request() -> RetrievalRequest {
        RetrievalRequest {
            principal: id::<PrincipalId>("principal.semantic-execution"),
            scope: RetrievalScope {
                privacy_domain: id("privacy.semantic-execution"),
                root: SingleRootScopeV1 {
                    repository: id("repository.semantic-execution"),
                    worktree: None,
                    reference: None,
                },
            },
            temporal_mode: TemporalModeV1::Current,
            snapshot: RetrievalSnapshot {
                watermarks: VectorWatermark::default(),
                freshness_digest: digest::<FreshnessVectorDigest>('a'),
                authorization_revision: id::<AuthorizationRevision>(
                    "authorization.semantic-execution.v1",
                ),
                captured_at: UtcMicros(1),
            },
            profile_id: id("profile.semantic-execution.v1"),
            budget: budget(),
        }
    }

    fn fallback() -> Arc<QueryFallbackSubpayload> {
        Arc::new(
            QueryFallbackSubpayload::new(
                id("profile.query.semantic-execution.v1"),
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

    fn empty_lane(lane: RetrieverKind) -> CompositionLaneInput {
        CompositionLaneInput::new(
            lane,
            RetrieverOutcome::Complete(RetrieverBatch::<()> {
                candidates: Vec::new(),
                evidence_by_occurrence: BTreeMap::new(),
                coverage: RetrieverCoverage::default(),
                continuation: None,
            }),
        )
        .expect("empty typed lane")
    }

    fn empty_composition(profile_id: tracedecay_domain::FusionProfileId) -> CompositionOutputV1 {
        CompositionOutputV1 {
            profile_id,
            ranked_candidates: Vec::new(),
            comparator_records: Vec::new(),
            internal_lane_outcomes: BTreeMap::new(),
            public_lane_statuses: BTreeMap::new(),
            freshness: Vec::new(),
            lane_checkpoints: Vec::new(),
            dedupe_decisions: Vec::new(),
            diversity_decisions: Vec::new(),
        }
    }

    fn authorized(fallback: Arc<QueryFallbackSubpayload>) -> AuthorizedQueryFallbackV1 {
        AuthorizedQueryFallbackV1 {
            query_digest: QueryDigest::new(
                id("privacy.semantic-execution"),
                1,
                QueryMac::new(format!("hmac-sha256:{}", "1".repeat(64))).expect("query MAC"),
            ),
            fallback,
            composition: empty_composition(id("profile.query.semantic-execution.v1")),
            fallback_lanes: vec![
                empty_lane(RetrieverKind::ExactLiteral),
                empty_lane(RetrieverKind::Lexical),
                empty_lane(RetrieverKind::Graph),
            ],
            page_size: 4,
            request_cursor: None,
        }
    }

    fn authority() -> SemanticCompositionExecutionAuthorityV1 {
        SemanticCompositionExecutionAuthorityV1::new(
            profile(None),
            diversity(),
            None,
            id("ranking.semantic-execution.v1"),
        )
        .expect("valid semantic composition authority")
    }

    #[test]
    fn typed_semantic_abstentions_preserve_the_exact_fallback_arc() {
        let fallback = fallback();
        let identity = Arc::as_ptr(&fallback);
        let authorized = authorized(Arc::clone(&fallback));
        for abstention in [
            SemanticAbstentionV1::Denied,
            SemanticAbstentionV1::Stale,
            SemanticAbstentionV1::Cancelled,
            SemanticAbstentionV1::BudgetExceeded,
        ] {
            let outcome = authority()
                .execute(
                    &request(),
                    &authorized,
                    SemanticQueryServiceOutcomeV1::Fallback {
                        abstention: abstention.clone(),
                        fallback: Arc::clone(&fallback),
                    },
                    SemanticAbstentionDispositionV1::UseFallback,
                    None,
                )
                .expect("typed fallback");
            assert!(matches!(
                outcome,
                SemanticCompositionExecutionOutcomeV1::Fallback {
                    abstention: ref actual,
                    ..
                } if actual == &abstention
            ));
            assert_eq!(Arc::as_ptr(outcome.fallback()), identity);
        }
    }

    #[test]
    fn admitted_semantic_lane_reuses_fallback_lanes_and_fallback_identity() {
        let fallback = fallback();
        let identity = Arc::as_ptr(&fallback);
        let authorized = authorized(Arc::clone(&fallback));
        let outcome = authority()
            .execute(
                &request(),
                &authorized,
                SemanticQueryServiceOutcomeV1::Augmented {
                    semantic_lane: empty_lane(RetrieverKind::Semantic),
                    calibration: SemanticCalibrationEvidenceV1 {
                        calibration_profile_id: id("calibration.semantic-execution.v1"),
                        cohort_digest: digest::<ManifestDigest>('b'),
                        best_distance: super::super::CanonicalSemanticDistanceV1(0),
                        next_best_margin_micros: u64::MAX,
                    },
                    fallback: Arc::clone(&fallback),
                },
                SemanticAbstentionDispositionV1::UseFallback,
                None,
            )
            .expect("semantic composition");
        let SemanticCompositionExecutionOutcomeV1::Augmented(executed) = outcome else {
            panic!("complete semantic lane must augment");
        };

        assert_eq!(Arc::as_ptr(&executed.fallback), identity);
        assert_eq!(executed.rerank, OptionalStagePublicStatus::NotRequested);
        assert_eq!(
            executed
                .composition
                .public_lane_statuses
                .keys()
                .copied()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                RetrieverKind::ExactLiteral,
                RetrieverKind::Lexical,
                RetrieverKind::Graph,
                RetrieverKind::Semantic,
            ])
        );
    }

    #[test]
    fn byte_equal_but_distinct_fallback_is_rejected() {
        let fallback = fallback();
        let authorized = authorized(Arc::clone(&fallback));
        let duplicate = Arc::new((*fallback).clone());

        assert!(matches!(
            authority().execute(
                &request(),
                &authorized,
                SemanticQueryServiceOutcomeV1::Fallback {
                    abstention: SemanticAbstentionV1::Denied,
                    fallback: duplicate,
                },
                SemanticAbstentionDispositionV1::UseFallback,
                None,
            ),
            Err(SemanticQueryServiceError::InvalidFallback)
        ));
    }

    fn ranked(anchor: &str, ordinal: u32) -> RankedCandidate {
        RankedCandidate {
            candidate: FusedCandidate {
                anchor_id: id::<RetrievalAnchorId>(anchor),
                logical_evidence_id: id::<LogicalEvidenceId>(&format!("logical.{anchor}")),
                occurrences: Vec::new(),
                exact_class: ExactClass::Approximate,
                utility_micros: 1,
                contributions: Vec::new(),
                freshness: Vec::<SourceFreshness>::new(),
                decisions: Vec::new(),
            },
            final_ordinal: ordinal,
        }
    }

    #[test]
    fn failed_rerank_restores_the_exact_post_composition_value() {
        let original = vec![ranked("anchor.one", 0), ranked("anchor.two", 1)];
        let mut composition = empty_composition(id("profile.semantic-execution.v1"));
        composition.ranked_candidates = original.clone();
        let status = apply_rerank_outcome(
            original.clone(),
            BoundedRerankOutcomeV1 {
                ordered_candidates: vec![ranked("anchor.injected", 0)],
                public_status: OptionalStagePublicStatus::BudgetExceeded(SanitizedBudgetUsage {
                    elapsed_micros: 9,
                    truncated: true,
                }),
                usage: RerankUsageV1 {
                    elapsed_micros: 9,
                    ..RerankUsageV1::default()
                },
            },
            &mut composition,
        );

        assert!(matches!(
            status,
            OptionalStagePublicStatus::BudgetExceeded(_)
        ));
        assert_eq!(composition.ranked_candidates, original);
    }

    #[test]
    fn frozen_continuation_restores_order_without_reexecution() {
        let mut composition = empty_composition(id("profile.semantic-execution.v1"));
        composition.ranked_candidates = vec![ranked("anchor.one", 0), ranked("anchor.two", 1)];
        let continuation = SemanticRetrievalContinuationV1 {
            profile_id: id("profile.semantic-execution.v1"),
            profile_digest: digest('c'),
            code_generation: id("generation.semantic-execution.v1"),
            vector_generation: VectorGenerationIdV1::new(digest('d')),
            projection_key: ProjectionKeyV1 {
                kind: ProjectionKindV1::Embedding,
                schema_revision: "projection.semantic-execution.v1".to_owned(),
                profile_digest: digest('e'),
            },
            search_index_key: SemanticSearchIndexProfileV1::exact_flat_v1()
                .and_then(|profile| profile.index_key())
                .expect("search index key"),
            candidate_set_digest: digest::<CandidateSetDigest>('f'),
            public_lane_statuses: BTreeMap::from([(
                RetrieverKind::Semantic,
                PublicRetrieverStatus::Complete,
            )]),
            lane_checkpoints: Vec::new(),
            ranking_revision: id::<RankingRevision>("ranking.semantic-execution.v1"),
            rerank: OptionalStagePublicStatus::NotRequested,
            ordered_candidate_anchors: vec![id("anchor.two"), id("anchor.one")],
            next_ordinal: 1,
        };

        restore_frozen_semantic_order(&continuation, &mut composition)
            .expect("frozen order restores");
        assert_eq!(
            composition
                .ranked_candidates
                .iter()
                .map(|candidate| candidate.candidate.anchor_id.as_str())
                .collect::<Vec<_>>(),
            ["anchor.two", "anchor.one"]
        );
        assert_eq!(
            composition
                .ranked_candidates
                .iter()
                .map(|candidate| candidate.final_ordinal)
                .collect::<Vec<_>>(),
            [0, 1]
        );
    }

    #[test]
    fn malformed_complete_rerank_is_rejected_and_restored() {
        let original = vec![ranked("anchor.one", 0), ranked("anchor.two", 1)];
        let mut composition = empty_composition(id("profile.semantic-execution.v1"));
        composition.ranked_candidates = original.clone();
        let status = apply_rerank_outcome(
            original.clone(),
            BoundedRerankOutcomeV1 {
                ordered_candidates: vec![ranked("anchor.one", 0), ranked("anchor.one", 1)],
                public_status: OptionalStagePublicStatus::Complete,
                usage: RerankUsageV1::default(),
            },
            &mut composition,
        );

        assert_eq!(
            status,
            OptionalStagePublicStatus::Rejected(SanitizedStageFailure::Invalid)
        );
        assert_eq!(composition.ranked_candidates, original);
    }

    #[test]
    fn strict_mode_keeps_composition_failure_typed() {
        let fallback = fallback();
        let mut authorized = authorized(Arc::clone(&fallback));
        authorized.fallback_lanes.clear();
        let result = authority().execute(
            &request(),
            &authorized,
            SemanticQueryServiceOutcomeV1::Augmented {
                semantic_lane: empty_lane(RetrieverKind::Semantic),
                calibration: SemanticCalibrationEvidenceV1 {
                    calibration_profile_id: id("calibration.semantic-execution.v1"),
                    cohort_digest: digest('9'),
                    best_distance: super::super::CanonicalSemanticDistanceV1(0),
                    next_best_margin_micros: u64::MAX,
                },
                fallback,
            },
            SemanticAbstentionDispositionV1::RejectUnavailable,
            None,
        );

        assert!(matches!(
            result,
            Err(SemanticQueryServiceError::StrictUnavailable(
                SemanticAbstentionV1::LaneFailure
            ))
        ));
    }
}
