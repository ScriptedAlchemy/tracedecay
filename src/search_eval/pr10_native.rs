//! Native PR10 semantic/rerank evaluation over production retrieval ports.
//!
//! This module is intentionally input-driven. It never opens an ambient model
//! cache, synthesizes embeddings, supplies a fallback score, or fabricates a
//! resource sample. Callers must pass the production exact-flat semantic lane,
//! an admitted local reranker (when one exists), and raw Linux measurements.
//! Missing optional inputs remain typed `Pending`.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    AuthorizedRerankView, CompactCandidate, DiversityPolicy, ExactClass, FreshnessCompatibilityV1,
    FusionProfile, ManifestDigest, Pr9FallbackSubpayload, PublicRetrieverStatus, RankedCandidate,
    RankingDecision, RankingDecisionKind, RerankPolicy, RetrievalAnchorId, RetrieverBatch,
    RetrieverKind, RetrieverOutcome,
};

use super::candidate_output::{CandidateWorkloadV1, ProfileSpecV1, ResourceSampleV1};
use crate::query::retrieval::fusion::{
    CompositionKernel, CompositionLaneInput, FusionStageError, FusionStageInput,
};
use crate::query::retrieval::semantic::{
    CodeSemanticEvidenceV1, SemanticLaneRetriever, SemanticRetrievalRequestV1, SemanticSearchKindV1,
};

const REQUIRED_RESOURCE_SCALES: [&str; 2] = ["current", "10x"];

/// Why a real optional-stage run could not be recorded.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Pr10NativePendingReasonV1 {
    SemanticArtifactUnavailable,
    SemanticGenerationUnavailable,
    SemanticGenerationIncomplete,
    SemanticCancelled,
    SemanticBudgetExceeded,
    RerankerArtifactUnavailable,
    RerankViewsUnavailable,
    RerankerUnavailable,
    ResourceMeasurementUnavailable,
}

/// Truthful state for one optional native evaluation result.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", content = "value", rename_all = "snake_case")]
pub enum Pr10NativeStageResultV1<T> {
    NotRequested,
    Complete(T),
    Pending { reason: Pr10NativePendingReasonV1 },
}

impl<T> Pr10NativeStageResultV1<T> {
    pub const fn is_pending(&self) -> bool {
        matches!(self, Self::Pending { .. })
    }
}

/// Optional stages requested by one checked-in Plan 15 profile.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Pr10NativeProfileRequirementsV1 {
    pub profile_id: String,
    pub semantic_requested: bool,
    pub rerank_requested: bool,
}

/// Derive execution requirements from the checked-in workload profile.
pub fn native_profile_requirements(
    workload: &CandidateWorkloadV1,
    profile_id: &str,
) -> Result<Pr10NativeProfileRequirementsV1, Pr10NativeEvaluationErrorV1> {
    let profile = workload
        .profile_matrix
        .iter()
        .find(|profile| profile.profile_id == profile_id)
        .ok_or_else(|| {
            Pr10NativeEvaluationErrorV1::Contract(format!("unknown profile {profile_id}"))
        })?;
    Ok(requirements_for_profile(profile))
}

fn requirements_for_profile(profile: &ProfileSpecV1) -> Pr10NativeProfileRequirementsV1 {
    Pr10NativeProfileRequirementsV1 {
        profile_id: profile.profile_id.clone(),
        semantic_requested: profile.semantic_weight_ppm != 0,
        rerank_requested: profile.rerank_weight_ppm != 0,
    }
}

/// Channel-removal comparisons required by Plans 15 and 31.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Pr10ChannelAblationV1 {
    ExactLexical,
    Pr9ExactLexicalGraph,
    ExactLexicalSemantic,
    HybridExactLexicalGraphSemantic,
}

/// One deterministic compact-candidate ablation result.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Pr10ChannelAblationResultV1 {
    pub ablation: Pr10ChannelAblationV1,
    pub public_lane_statuses: BTreeMap<RetrieverKind, PublicRetrieverStatus>,
    pub ranked_candidates: Vec<RankedCandidate>,
}

/// Exact-flat oracle row retaining production semantic provenance.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Pr10ExactFlatOracleHitV1 {
    pub candidate: CompactCandidate,
    pub evidence: CodeSemanticEvidenceV1,
}

/// Exact-flat output before generic fusion or reranking.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Pr10ExactFlatOracleV1 {
    pub hits: Vec<Pr10ExactFlatOracleHitV1>,
    pub examined: u64,
    pub eligible: u64,
    pub excluded: u64,
    pub capped: u64,
    pub unknown: u64,
}

/// Bounded request given to an already-admitted generic reranker.
pub struct Pr10NativeRerankRequestV1<'a> {
    pub policy: &'a RerankPolicy,
    pub candidates: &'a [RankedCandidate],
    pub authorized_views: &'a [AuthorizedRerankView],
}

/// Raw measured work returned by the admitted reranker.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Pr10NativeRerankExecutionV1 {
    pub artifact_manifest_digest: ManifestDigest,
    pub ordered_approximate_anchors: Vec<RetrievalAnchorId>,
    pub input_bytes: u64,
    pub input_tokens: u64,
    pub work_units: u64,
    pub model_invocations: u32,
    pub elapsed_micros: u64,
}

/// A reranker authority already admitted from an explicitly installed local
/// artifact. Implementations must not perform download or model substitution.
pub trait AdmittedNativeRerankerV1 {
    fn artifact_manifest_digest(&self) -> &ManifestDigest;

    fn rerank(
        &self,
        request: Pr10NativeRerankRequestV1<'_>,
    ) -> Result<Pr10NativeRerankExecutionV1, Pr10NativeRerankUnavailableV1>;
}

/// Sanitized unavailability; it cannot carry model output or private bytes.
#[derive(Clone, Copy, Debug, Error, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Pr10NativeRerankUnavailableV1 {
    #[error("reranker artifact is unavailable")]
    ArtifactUnavailable,
    #[error("reranker runtime is unavailable")]
    RuntimeUnavailable,
    #[error("reranker refused the bounded request")]
    Refused,
    #[error("reranker request was cancelled")]
    Cancelled,
    #[error("reranker request exhausted its budget")]
    BudgetExceeded,
}

/// Rerank-off/on comparison. `off` always contains the canonical pre-rerank
/// production order; `on` is pending unless a real admitted reranker ran.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Pr10RerankComparisonV1 {
    pub off: Vec<RankedCandidate>,
    pub on: Pr10NativeStageResultV1<Vec<RankedCandidate>>,
    pub execution: Pr10NativeStageResultV1<Pr10NativeRerankExecutionV1>,
}

/// Per-query native PR10 evaluation output.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Pr10NativeQueryOutputV1 {
    pub profile_id: String,
    pub fallback_digest: String,
    pub fallback_bytes_unchanged: bool,
    pub ablations: Vec<Pr10ChannelAblationResultV1>,
    pub exact_flat_oracle: Pr10NativeStageResultV1<Pr10ExactFlatOracleV1>,
    pub rerank: Pr10RerankComparisonV1,
}

/// Real production semantic execution input. The request already binds the
/// admitted projection, immutable vector generation, source generation,
/// authorization, and query MAC.
pub struct Pr10NativeSemanticInputV1<'a> {
    pub lane: &'a dyn SemanticLaneRetriever,
    pub request: &'a SemanticRetrievalRequestV1<'a>,
}

/// Inputs for one native query evaluation.
pub struct Pr10NativeQueryInputV1<'a> {
    pub profile_spec: &'a ProfileSpecV1,
    pub fusion_profile: &'a FusionProfile,
    pub diversity_policy: &'a DiversityPolicy,
    pub kernel: &'a CompositionKernel,
    pub pr9_lanes: &'a [CompositionLaneInput],
    pub semantic: Option<Pr10NativeSemanticInputV1<'a>>,
    pub fallback: &'a Pr9FallbackSubpayload,
    pub reranker: Option<&'a dyn AdmittedNativeRerankerV1>,
    pub rerank_policy: Option<&'a RerankPolicy>,
    pub authorized_rerank_views: Option<&'a [AuthorizedRerankView]>,
}

#[derive(Debug, Error)]
pub enum Pr10NativeEvaluationErrorV1 {
    #[error("native PR10 evaluation contract violation: {0}")]
    Contract(String),
    #[error(transparent)]
    Fusion(#[from] FusionStageError),
}

/// Execute channel ablations, exact-flat oracle capture, bounded rerank
/// off/on, and the fallback-byte invariant for one checked-in query.
pub fn evaluate_native_query(
    input: Pr10NativeQueryInputV1<'_>,
) -> Result<Pr10NativeQueryOutputV1, Pr10NativeEvaluationErrorV1> {
    validate_profile_binding(input.profile_spec, input.fusion_profile)?;
    input
        .fallback
        .validate()
        .map_err(|error| Pr10NativeEvaluationErrorV1::Contract(error.to_string()))?;
    validate_pr9_lanes(input.pr9_lanes)?;
    let fallback_before = canonical_fallback_bytes(input.fallback)?;
    let requirements = requirements_for_profile(input.profile_spec);

    let mut ablations = vec![
        compose_ablation(
            input.kernel,
            input.fusion_profile,
            input.diversity_policy,
            input.pr9_lanes,
            &[RetrieverKind::ExactLiteral, RetrieverKind::Lexical],
            Pr10ChannelAblationV1::ExactLexical,
        )?,
        compose_ablation(
            input.kernel,
            input.fusion_profile,
            input.diversity_policy,
            input.pr9_lanes,
            &RetrieverKind::PR9_FALLBACK_LANES,
            Pr10ChannelAblationV1::Pr9ExactLexicalGraph,
        )?,
    ];

    let (oracle, semantic_lane) = if !requirements.semantic_requested {
        (Pr10NativeStageResultV1::NotRequested, None)
    } else {
        evaluate_semantic(input.semantic)?
    };
    if let Some(semantic_lane) = semantic_lane {
        let mut lanes = input.pr9_lanes.to_vec();
        lanes.push(semantic_lane);
        ablations.push(compose_ablation(
            input.kernel,
            input.fusion_profile,
            input.diversity_policy,
            &lanes,
            &[
                RetrieverKind::ExactLiteral,
                RetrieverKind::Lexical,
                RetrieverKind::Semantic,
            ],
            Pr10ChannelAblationV1::ExactLexicalSemantic,
        )?);
        ablations.push(compose_ablation(
            input.kernel,
            input.fusion_profile,
            input.diversity_policy,
            &lanes,
            &[
                RetrieverKind::ExactLiteral,
                RetrieverKind::Lexical,
                RetrieverKind::Graph,
                RetrieverKind::Semantic,
            ],
            Pr10ChannelAblationV1::HybridExactLexicalGraphSemantic,
        )?);
    }
    ablations.sort_by_key(|result| result.ablation);

    let rerank_source = ablations
        .iter()
        .find(|result| result.ablation == Pr10ChannelAblationV1::HybridExactLexicalGraphSemantic)
        .or_else(|| {
            ablations
                .iter()
                .find(|result| result.ablation == Pr10ChannelAblationV1::Pr9ExactLexicalGraph)
        })
        .ok_or_else(|| {
            Pr10NativeEvaluationErrorV1::Contract(
                "native evaluation produced no canonical rerank input".to_owned(),
            )
        })?;
    let rerank = evaluate_rerank(
        requirements.rerank_requested,
        &rerank_source.ranked_candidates,
        input.reranker,
        input.rerank_policy,
        input.authorized_rerank_views,
    )?;

    let fallback_after = canonical_fallback_bytes(input.fallback)?;
    Ok(Pr10NativeQueryOutputV1 {
        profile_id: input.profile_spec.profile_id.clone(),
        fallback_digest: input.fallback.digest.as_str().to_owned(),
        fallback_bytes_unchanged: fallback_before == fallback_after,
        ablations,
        exact_flat_oracle: oracle,
        rerank,
    })
}

fn validate_profile_binding(
    profile: &ProfileSpecV1,
    fusion: &FusionProfile,
) -> Result<(), Pr10NativeEvaluationErrorV1> {
    let expected = format!("profile.{}", profile.profile_id);
    if fusion.profile_id.as_str() != expected {
        return Err(Pr10NativeEvaluationErrorV1::Contract(format!(
            "profile {} does not bind fusion profile {}",
            profile.profile_id, fusion.profile_id
        )));
    }
    for (lane, weight) in [
        (RetrieverKind::Lexical, profile.lexical_weight_ppm),
        (RetrieverKind::Graph, profile.graph_weight_ppm),
        (RetrieverKind::Semantic, profile.semantic_weight_ppm),
    ] {
        let observed = fusion.weights_micros.get(&lane).copied().unwrap_or(0);
        if observed != weight {
            return Err(Pr10NativeEvaluationErrorV1::Contract(format!(
                "{} weight does not bind checked-in profile {}",
                lane.as_str(),
                profile.profile_id
            )));
        }
    }
    Ok(())
}

fn validate_pr9_lanes(lanes: &[CompositionLaneInput]) -> Result<(), Pr10NativeEvaluationErrorV1> {
    let observed = lanes.iter().map(|lane| lane.lane).collect::<BTreeSet<_>>();
    let expected = RetrieverKind::PR9_FALLBACK_LANES
        .into_iter()
        .collect::<BTreeSet<_>>();
    if observed != expected || lanes.len() != expected.len() {
        return Err(Pr10NativeEvaluationErrorV1::Contract(
            "native PR10 input must contain exactly the PR9 exact/lexical/graph lanes".to_owned(),
        ));
    }
    Ok(())
}

fn canonical_fallback_bytes(
    fallback: &Pr9FallbackSubpayload,
) -> Result<Vec<u8>, Pr10NativeEvaluationErrorV1> {
    serde_json::to_vec(fallback)
        .map_err(|error| Pr10NativeEvaluationErrorV1::Contract(error.to_string()))
}

fn compose_ablation(
    kernel: &CompositionKernel,
    profile: &FusionProfile,
    diversity: &DiversityPolicy,
    lanes: &[CompositionLaneInput],
    admitted: &[RetrieverKind],
    ablation: Pr10ChannelAblationV1,
) -> Result<Pr10ChannelAblationResultV1, Pr10NativeEvaluationErrorV1> {
    let admitted = admitted.iter().copied().collect::<BTreeSet<_>>();
    let selected = lanes
        .iter()
        .filter(|lane| admitted.contains(&lane.lane))
        .cloned()
        .collect::<Vec<_>>();
    let observed = selected
        .iter()
        .map(|lane| lane.lane)
        .collect::<BTreeSet<_>>();
    if observed != admitted {
        return Err(Pr10NativeEvaluationErrorV1::Contract(format!(
            "ablation {ablation:?} is missing an admitted lane"
        )));
    }
    let profile = ablated_profile(profile, &admitted);
    let output = kernel.compose(
        &FusionStageInput {
            profile,
            lanes: selected,
        },
        diversity,
    )?;
    Ok(Pr10ChannelAblationResultV1 {
        ablation,
        public_lane_statuses: output.public_lane_statuses,
        ranked_candidates: output.ranked_candidates,
    })
}

fn ablated_profile(profile: &FusionProfile, admitted: &BTreeSet<RetrieverKind>) -> FusionProfile {
    let mut profile = profile.clone();
    profile
        .weights_micros
        .retain(|lane, _| admitted.contains(lane));
    profile
        .calibrations
        .retain(|lane, _| admitted.contains(lane));
    profile.rerank_policy_id = None;
    profile
}

fn evaluate_semantic(
    semantic: Option<Pr10NativeSemanticInputV1<'_>>,
) -> Result<
    (
        Pr10NativeStageResultV1<Pr10ExactFlatOracleV1>,
        Option<CompositionLaneInput>,
    ),
    Pr10NativeEvaluationErrorV1,
> {
    let Some(semantic) = semantic else {
        return Ok((
            Pr10NativeStageResultV1::Pending {
                reason: Pr10NativePendingReasonV1::SemanticArtifactUnavailable,
            },
            None,
        ));
    };
    let outcome = semantic
        .lane
        .retrieve_semantic(semantic.request)
        .map_err(|error| Pr10NativeEvaluationErrorV1::Contract(error.to_string()))?;
    match outcome {
        RetrieverOutcome::Complete(batch) => {
            let oracle = exact_flat_oracle(&batch)?;
            let lane = CompositionLaneInput::new(
                RetrieverKind::Semantic,
                RetrieverOutcome::Complete(batch),
            )?;
            Ok((Pr10NativeStageResultV1::Complete(oracle), Some(lane)))
        }
        RetrieverOutcome::Partial { .. } => Ok((
            Pr10NativeStageResultV1::Pending {
                reason: Pr10NativePendingReasonV1::SemanticGenerationIncomplete,
            },
            None,
        )),
        RetrieverOutcome::Unavailable(_)
        | RetrieverOutcome::Denied
        | RetrieverOutcome::Stale(_) => Ok((
            Pr10NativeStageResultV1::Pending {
                reason: Pr10NativePendingReasonV1::SemanticGenerationUnavailable,
            },
            None,
        )),
        RetrieverOutcome::BudgetExceeded(_) => Ok((
            Pr10NativeStageResultV1::Pending {
                reason: Pr10NativePendingReasonV1::SemanticBudgetExceeded,
            },
            None,
        )),
        RetrieverOutcome::Cancelled => Ok((
            Pr10NativeStageResultV1::Pending {
                reason: Pr10NativePendingReasonV1::SemanticCancelled,
            },
            None,
        )),
    }
}

fn exact_flat_oracle(
    batch: &RetrieverBatch<CodeSemanticEvidenceV1>,
) -> Result<Pr10ExactFlatOracleV1, Pr10NativeEvaluationErrorV1> {
    batch
        .validate()
        .map_err(|error| Pr10NativeEvaluationErrorV1::Contract(error.to_string()))?;
    let mut hits = Vec::with_capacity(batch.candidates.len());
    for candidate in &batch.candidates {
        let evidence = batch
            .evidence_by_occurrence
            .get(&candidate.source_occurrence_id)
            .ok_or_else(|| {
                Pr10NativeEvaluationErrorV1::Contract(
                    "semantic oracle candidate is missing occurrence evidence".to_owned(),
                )
            })?;
        if candidate.retriever != RetrieverKind::Semantic
            || evidence.search_kind != SemanticSearchKindV1::ExactFlat
        {
            return Err(Pr10NativeEvaluationErrorV1::Contract(
                "semantic oracle accepts only the production exact-flat lane".to_owned(),
            ));
        }
        hits.push(Pr10ExactFlatOracleHitV1 {
            candidate: candidate.clone(),
            evidence: evidence.clone(),
        });
    }
    Ok(Pr10ExactFlatOracleV1 {
        hits,
        examined: batch.coverage.examined,
        eligible: batch.coverage.eligible,
        excluded: batch.coverage.excluded,
        capped: batch.coverage.capped,
        unknown: batch.coverage.unknown,
    })
}

fn evaluate_rerank(
    requested: bool,
    pre_rerank: &[RankedCandidate],
    reranker: Option<&dyn AdmittedNativeRerankerV1>,
    policy: Option<&RerankPolicy>,
    views: Option<&[AuthorizedRerankView]>,
) -> Result<Pr10RerankComparisonV1, Pr10NativeEvaluationErrorV1> {
    let off = pre_rerank.to_vec();
    if !requested {
        return Ok(Pr10RerankComparisonV1 {
            off,
            on: Pr10NativeStageResultV1::NotRequested,
            execution: Pr10NativeStageResultV1::NotRequested,
        });
    }
    let Some(reranker) = reranker else {
        return Ok(pending_rerank(
            off,
            Pr10NativePendingReasonV1::RerankerArtifactUnavailable,
        ));
    };
    let Some(policy) = policy else {
        return Err(Pr10NativeEvaluationErrorV1::Contract(
            "rerank profile requested execution without a bounded policy".to_owned(),
        ));
    };
    let Some(views) = views else {
        return Ok(pending_rerank(
            off,
            Pr10NativePendingReasonV1::RerankViewsUnavailable,
        ));
    };
    let rerank_prefix = pre_rerank
        .iter()
        .filter(|candidate| candidate.candidate.exact_class == ExactClass::Approximate)
        .take(policy.max_candidates as usize)
        .cloned()
        .collect::<Vec<_>>();
    validate_rerank_views(&rerank_prefix, views)?;
    let execution = match reranker.rerank(Pr10NativeRerankRequestV1 {
        policy,
        candidates: &rerank_prefix,
        authorized_views: views,
    }) {
        Ok(execution) => execution,
        Err(Pr10NativeRerankUnavailableV1::ArtifactUnavailable) => {
            return Ok(pending_rerank(
                off,
                Pr10NativePendingReasonV1::RerankerArtifactUnavailable,
            ));
        }
        Err(
            Pr10NativeRerankUnavailableV1::RuntimeUnavailable
            | Pr10NativeRerankUnavailableV1::Refused
            | Pr10NativeRerankUnavailableV1::Cancelled
            | Pr10NativeRerankUnavailableV1::BudgetExceeded,
        ) => {
            return Ok(pending_rerank(
                off,
                Pr10NativePendingReasonV1::RerankerUnavailable,
            ));
        }
    };
    validate_rerank_execution(reranker, policy, &rerank_prefix, &execution)?;
    let on = apply_rerank_order(
        pre_rerank,
        &rerank_prefix,
        &execution.ordered_approximate_anchors,
        policy,
    )?;
    Ok(Pr10RerankComparisonV1 {
        off,
        on: Pr10NativeStageResultV1::Complete(on),
        execution: Pr10NativeStageResultV1::Complete(execution),
    })
}

fn pending_rerank(
    off: Vec<RankedCandidate>,
    reason: Pr10NativePendingReasonV1,
) -> Pr10RerankComparisonV1 {
    Pr10RerankComparisonV1 {
        off,
        on: Pr10NativeStageResultV1::Pending { reason },
        execution: Pr10NativeStageResultV1::Pending { reason },
    }
}

fn validate_rerank_views(
    candidates: &[RankedCandidate],
    views: &[AuthorizedRerankView],
) -> Result<(), Pr10NativeEvaluationErrorV1> {
    let approximate = candidates
        .iter()
        .filter(|candidate| candidate.candidate.exact_class == ExactClass::Approximate)
        .map(|candidate| candidate.candidate.anchor_id.clone())
        .collect::<BTreeSet<_>>();
    let mut observed = BTreeSet::new();
    let identity = views
        .first()
        .map(|view| (&view.snapshot_digest, &view.privacy_domain));
    for view in views {
        if view.compatibility != FreshnessCompatibilityV1::Current
            || !observed.insert(view.anchor_id.clone())
            || identity.is_some_and(|(snapshot, privacy_domain)| {
                &view.snapshot_digest != snapshot || &view.privacy_domain != privacy_domain
            })
        {
            return Err(Pr10NativeEvaluationErrorV1::Contract(
                "rerank views must be unique, current, and share one snapshot/privacy identity"
                    .to_owned(),
            ));
        }
    }
    if observed != approximate {
        return Err(Pr10NativeEvaluationErrorV1::Contract(
            "rerank views must bind exactly the approximate candidate set".to_owned(),
        ));
    }
    Ok(())
}

fn validate_rerank_execution(
    reranker: &dyn AdmittedNativeRerankerV1,
    policy: &RerankPolicy,
    candidates: &[RankedCandidate],
    execution: &Pr10NativeRerankExecutionV1,
) -> Result<(), Pr10NativeEvaluationErrorV1> {
    if &execution.artifact_manifest_digest != reranker.artifact_manifest_digest() {
        return Err(Pr10NativeEvaluationErrorV1::Contract(
            "rerank result does not bind the admitted artifact".to_owned(),
        ));
    }
    execution
        .artifact_manifest_digest
        .validate()
        .map_err(|error| Pr10NativeEvaluationErrorV1::Contract(error.to_string()))?;
    let approximate_count = candidates
        .iter()
        .filter(|candidate| candidate.candidate.exact_class == ExactClass::Approximate)
        .count();
    if approximate_count > policy.max_candidates as usize
        || execution.input_bytes > policy.max_input_bytes
        || execution.input_tokens > policy.max_input_tokens
        || execution.work_units > policy.max_work_units
        || execution.model_invocations > policy.max_model_invocations
        || policy
            .deadline_micros
            .is_some_and(|deadline| execution.elapsed_micros > deadline)
    {
        return Err(Pr10NativeEvaluationErrorV1::Contract(
            "rerank execution exceeded its evaluated policy".to_owned(),
        ));
    }
    Ok(())
}

fn apply_rerank_order(
    candidates: &[RankedCandidate],
    rerank_prefix: &[RankedCandidate],
    approximate_order: &[RetrievalAnchorId],
    policy: &RerankPolicy,
) -> Result<Vec<RankedCandidate>, Pr10NativeEvaluationErrorV1> {
    let mut exact = Vec::new();
    let mut approximate = Vec::new();
    for candidate in candidates {
        if candidate.candidate.exact_class == ExactClass::Approximate {
            approximate.push(candidate.clone());
        } else {
            exact.push(candidate.clone());
        }
    }
    let mut rerank_candidates = rerank_prefix
        .iter()
        .map(|candidate| (candidate.candidate.anchor_id.clone(), candidate.clone()))
        .collect::<BTreeMap<_, _>>();
    if rerank_candidates.len() != rerank_prefix.len()
        || approximate_order.len() != rerank_candidates.len()
        || approximate_order.iter().collect::<BTreeSet<_>>().len() != rerank_candidates.len()
        || approximate_order
            .iter()
            .any(|anchor| !rerank_candidates.contains_key(anchor))
    {
        return Err(Pr10NativeEvaluationErrorV1::Contract(
            "reranker must return one permutation of the bounded approximate prefix".to_owned(),
        ));
    }
    let mut ordered = exact;
    for anchor in approximate_order {
        let mut candidate = rerank_candidates.remove(anchor).ok_or_else(|| {
            Pr10NativeEvaluationErrorV1::Contract(
                "rerank permutation changed after validation".to_owned(),
            )
        })?;
        candidate.candidate.decisions.push(RankingDecision {
            kind: RankingDecisionKind::RerankAdmission,
            retriever: None,
            policy_anchor: Some(policy.evaluation_result_anchor.clone()),
            evidence_anchor: None,
            detail: format!(
                "admitted by bounded rerank policy {}",
                policy.policy_id.as_str()
            ),
        });
        ordered.push(candidate);
    }
    ordered.extend(approximate.into_iter().skip(rerank_prefix.len()));
    for (ordinal, candidate) in ordered.iter_mut().enumerate() {
        candidate.final_ordinal = ordinal as u32;
    }
    Ok(ordered)
}

/// Raw Linux resource sample. Optional values remain absent until the actual
/// process/runtime measurement was captured.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Pr10NativeResourceSampleV1 {
    pub eligible_chunks: u64,
    pub latency_samples_us: Vec<u64>,
    pub measured_queries: u64,
    pub cpu_time_us: Option<u64>,
    pub peak_rss_bytes: Option<u64>,
    pub model_bytes: Option<u64>,
    pub vector_bytes: Option<u64>,
    pub index_bytes: Option<u64>,
    pub cache_bytes: Option<u64>,
    pub incremental_rebuild_samples_us: Vec<u64>,
}

impl Pr10NativeResourceSampleV1 {
    fn is_complete(&self) -> bool {
        self.eligible_chunks != 0
            && self.measured_queries != 0
            && self.measured_queries == self.latency_samples_us.len() as u64
            && self.cpu_time_us.is_some()
            && self.peak_rss_bytes.is_some()
            && self.model_bytes.is_some()
            && self.vector_bytes.is_some()
            && self.index_bytes.is_some()
            && self.cache_bytes.is_some()
            && !self.incremental_rebuild_samples_us.is_empty()
    }

    /// Lossless projection into the resource fields understood by the current
    /// direct evaluator. This is available only after a complete real sample.
    pub fn as_existing_evaluator_sample(&self) -> Option<ResourceSampleV1> {
        self.is_complete().then(|| ResourceSampleV1 {
            status: super::candidate_output::ResourceMeasurementStatusV1::Measured,
            eligible_chunks: self.eligible_chunks,
            peak_rss_bytes: self.peak_rss_bytes,
            latency_samples_us: self.latency_samples_us.clone(),
            measured_queries: self.measured_queries,
            pending_reason: None,
        })
    }
}

/// Required current and exact-10x samples.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Pr10NativeResourceEvidenceV1 {
    pub samples: BTreeMap<String, Pr10NativeStageResultV1<Pr10NativeResourceSampleV1>>,
}

impl Pr10NativeResourceEvidenceV1 {
    /// Validate exact scale and preserve pending measurements without filling
    /// any absent observation.
    pub fn validate(&self) -> Result<(), Pr10NativeEvaluationErrorV1> {
        let observed = self
            .samples
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let expected = REQUIRED_RESOURCE_SCALES
            .into_iter()
            .collect::<BTreeSet<_>>();
        if observed != expected {
            return Err(Pr10NativeEvaluationErrorV1::Contract(
                "resource evidence must contain exactly current and 10x".to_owned(),
            ));
        }
        let current = self.samples.get("current").ok_or_else(|| {
            Pr10NativeEvaluationErrorV1::Contract("resource evidence is missing current".to_owned())
        })?;
        let ten_x = self.samples.get("10x").ok_or_else(|| {
            Pr10NativeEvaluationErrorV1::Contract("resource evidence is missing 10x".to_owned())
        })?;
        for sample in [current, ten_x] {
            match sample {
                Pr10NativeStageResultV1::Complete(sample) if !sample.is_complete() => {
                    return Err(Pr10NativeEvaluationErrorV1::Contract(
                        "complete resource evidence is missing a raw observation".to_owned(),
                    ));
                }
                Pr10NativeStageResultV1::NotRequested => {
                    return Err(Pr10NativeEvaluationErrorV1::Contract(
                        "current and 10x resource measurements are required".to_owned(),
                    ));
                }
                Pr10NativeStageResultV1::Complete(_) | Pr10NativeStageResultV1::Pending { .. } => {}
            }
        }
        if let (
            Pr10NativeStageResultV1::Complete(current),
            Pr10NativeStageResultV1::Complete(ten_x),
        ) = (current, ten_x)
        {
            if current
                .eligible_chunks
                .checked_mul(10)
                .is_none_or(|expected| ten_x.eligible_chunks != expected)
            {
                return Err(Pr10NativeEvaluationErrorV1::Contract(
                    "10x resource evidence must contain exactly ten times the eligible chunks"
                        .to_owned(),
                ));
            }
        }
        Ok(())
    }

    /// Complete samples for the existing evaluator. Pending scales are
    /// omitted instead of receiving synthetic latency/RSS values.
    pub fn existing_evaluator_samples(&self) -> BTreeMap<String, ResourceSampleV1> {
        self.samples
            .iter()
            .filter_map(|(scale, sample)| match sample {
                Pr10NativeStageResultV1::Complete(sample) => sample
                    .as_existing_evaluator_sample()
                    .map(|sample| (scale.clone(), sample)),
                Pr10NativeStageResultV1::NotRequested | Pr10NativeStageResultV1::Pending { .. } => {
                    None
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::search_eval::load_candidate_workload;

    fn checked_in_workload() -> CandidateWorkloadV1 {
        load_candidate_workload(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/search_quality/pr9-pr10-candidate-workload-v1.json"),
        )
        .expect("checked-in Plan 15 workload")
    }

    #[test]
    fn checked_in_profiles_request_only_declared_native_stages() {
        let workload = checked_in_workload();

        assert_eq!(
            native_profile_requirements(&workload, "pr9-fallback").expect("profile"),
            Pr10NativeProfileRequirementsV1 {
                profile_id: "pr9-fallback".to_owned(),
                semantic_requested: false,
                rerank_requested: false,
            }
        );
        assert_eq!(
            native_profile_requirements(&workload, "hybrid-conservative").expect("profile"),
            Pr10NativeProfileRequirementsV1 {
                profile_id: "hybrid-conservative".to_owned(),
                semantic_requested: true,
                rerank_requested: false,
            }
        );
        assert_eq!(
            native_profile_requirements(&workload, "hybrid-reranked").expect("profile"),
            Pr10NativeProfileRequirementsV1 {
                profile_id: "hybrid-reranked".to_owned(),
                semantic_requested: true,
                rerank_requested: true,
            }
        );
    }

    #[test]
    fn unavailable_native_inputs_remain_pending_without_measurements() {
        let evidence = Pr10NativeResourceEvidenceV1 {
            samples: BTreeMap::from([
                (
                    "10x".to_owned(),
                    Pr10NativeStageResultV1::Pending {
                        reason: Pr10NativePendingReasonV1::ResourceMeasurementUnavailable,
                    },
                ),
                (
                    "current".to_owned(),
                    Pr10NativeStageResultV1::Pending {
                        reason: Pr10NativePendingReasonV1::ResourceMeasurementUnavailable,
                    },
                ),
            ]),
        };

        evidence.validate().expect("truthful pending evidence");
        assert!(evidence.existing_evaluator_samples().is_empty());
    }
}
