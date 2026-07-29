//! Native PR10 semantic/rerank evaluation over production retrieval ports.
//!
//! This module is intentionally input-driven. It never opens an ambient model
//! cache, synthesizes embeddings, supplies a fallback score, or fabricates a
//! resource sample. Callers must pass the production exact-flat semantic lane,
//! an admitted local reranker (when one exists), and raw Linux measurements.
//! Missing optional inputs remain typed `Pending`.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CompactCandidate, DiversityPolicy, ExactClass, FusionProfile, ManifestDigest,
    OptionalStagePublicStatus, Pr9FallbackSubpayload, PublicRetrieverStatus, RankedCandidate,
    RerankPolicy, RetrievalAnchorId, RetrievalFailure, RetrievalRequest, RetrieverBatch,
    RetrieverKind, RetrieverOutcome, SanitizedStageFailure,
};

use super::candidate_output::{CandidateWorkloadV1, ProfileSpecV1, ResourceSampleV1};
use crate::query::retrieval::fusion::{
    CompositionKernel, CompositionLaneInput, FusionStageError, FusionStageInput,
};
use crate::query::retrieval::rerank::{
    BoundedRerankRuntimeV1, DeterministicLocalRerankExecutorV1, EphemeralRerankViewSourceV1,
    LocalRerankFailureV1, LocalRerankInputV1, LocalRerankPermitV1, RerankExecutionControlV1,
    RerankViewOutcomeV1, RerankViewPermitV1,
};
use crate::query::retrieval::semantic::{
    CodeSemanticEvidenceV1, SemanticLaneRetriever, SemanticRetrievalRequestV1, SemanticSearchKindV1,
};

const REQUIRED_RESOURCE_SCALES: [&str; 2] = ["current", "10x"];
const REQUIRED_PROJECTION_CASES: [Pr10ProjectionCaseV1; 7] = [
    Pr10ProjectionCaseV1::Clean,
    Pr10ProjectionCaseV1::OneSymbol,
    Pr10ProjectionCaseV1::Deletion,
    Pr10ProjectionCaseV1::NoOp,
    Pr10ProjectionCaseV1::IdempotencyReplay,
    Pr10ProjectionCaseV1::Cancellation,
    Pr10ProjectionCaseV1::IncompatibleState,
];

/// Why a real optional-stage run could not be recorded.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Pr10NativePendingReasonV1 {
    SemanticArtifactUnavailable,
    SemanticGenerationUnavailable,
    SemanticGenerationIncomplete,
    SemanticCancelled,
    RerankerArtifactUnavailable,
    RerankerUnavailable,
    RerankCancelled,
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
    pub measurement: Pr10NativeStageMeasurementV1,
}

/// Raw work observed at one production retrieval boundary.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Pr10NativeStageMeasurementV1 {
    pub elapsed_micros: u64,
    pub input_candidates: u64,
    pub output_candidates: u64,
}

/// PR9 lane work completed before the PR10 semantic/fusion comparison.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Pr10NativePr9StageMeasurementsV1 {
    pub exact: Pr10NativeStageMeasurementV1,
    pub lexical: Pr10NativeStageMeasurementV1,
    pub graph: Pr10NativeStageMeasurementV1,
}

/// Late source reads performed only after the final rank is fixed.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Pr10NativeHydrationMeasurementV1 {
    pub elapsed_micros: u64,
    pub selected_candidates: u64,
    pub source_fetches: u64,
    pub receipts: u64,
    pub bytes_hydrated: u64,
}

/// Per-query measurements kept beside the exact result they describe.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Pr10NativeQueryMeasurementsV1 {
    pub pr9: Pr10NativePr9StageMeasurementsV1,
    pub semantic: Pr10NativeStageResultV1<Pr10NativeStageMeasurementV1>,
    pub rerank: Pr10NativeStageResultV1<Pr10NativeStageMeasurementV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hydration: Option<Pr10NativeHydrationMeasurementV1>,
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

/// Production bounded-rerank authorities for one evaluation query.
///
/// The evaluator invokes [`BoundedRerankRuntimeV1`] directly. Authorized
/// views remain ephemeral, and the runtime itself enforces every policy cap.
pub struct Pr10NativeRerankInputV1<'a> {
    pub request: &'a RetrievalRequest,
    pub policy: &'a RerankPolicy,
    pub views: &'a mut dyn EphemeralRerankViewSourceV1,
    pub executor: &'a dyn AdmittedNativeRerankExecutorV1,
    pub control: &'a dyn RerankExecutionControlV1,
}

/// Deterministic local executor admitted from one verified artifact.
pub trait AdmittedNativeRerankExecutorV1: DeterministicLocalRerankExecutorV1 {
    fn artifact_manifest_digest(&self) -> &ManifestDigest;
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
    pub measurements: Pr10NativeQueryMeasurementsV1,
}

/// Real production semantic execution input. The request already binds the
/// admitted projection, immutable vector generation, source generation,
/// authorization, and query MAC.
pub struct Pr10NativeSemanticInputV1<'a> {
    pub lane: &'a dyn SemanticLaneRetriever,
    pub request: &'a SemanticRetrievalRequestV1<'a>,
}

/// Inputs for one native query evaluation.
pub struct Pr10NativeQueryInputV1<'fixed, 'runtime> {
    pub profile_spec: &'fixed ProfileSpecV1,
    pub fusion_profile: &'fixed FusionProfile,
    pub diversity_policy: &'fixed DiversityPolicy,
    pub kernel: &'fixed CompositionKernel,
    pub pr9_lanes: &'fixed [CompositionLaneInput],
    pub pr9_measurements: Pr10NativePr9StageMeasurementsV1,
    pub semantic: Option<Pr10NativeSemanticInputV1<'runtime>>,
    pub fallback: &'fixed Pr9FallbackSubpayload,
    pub rerank: Option<Pr10NativeRerankInputV1<'runtime>>,
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
    input: Pr10NativeQueryInputV1<'_, '_>,
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

    let (oracle, semantic_lane, semantic_measurement) = if !requirements.semantic_requested {
        if input.semantic.is_some() {
            return Err(Pr10NativeEvaluationErrorV1::Contract(
                "semantic authority was supplied for a profile that does not request semantics"
                    .to_owned(),
            ));
        }
        (
            Pr10NativeStageResultV1::NotRequested,
            None,
            Pr10NativeStageResultV1::NotRequested,
        )
    } else {
        evaluate_semantic(input.semantic, input.fusion_profile)?
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
        input.fusion_profile,
        input.rerank,
    )?;
    let rerank_measurement = rerank_stage_measurement(&rerank);

    let fallback_after = canonical_fallback_bytes(input.fallback)?;
    Ok(Pr10NativeQueryOutputV1 {
        profile_id: input.profile_spec.profile_id.clone(),
        fallback_digest: input.fallback.digest.as_str().to_owned(),
        fallback_bytes_unchanged: fallback_before == fallback_after,
        ablations,
        exact_flat_oracle: oracle,
        rerank,
        measurements: Pr10NativeQueryMeasurementsV1 {
            pr9: input.pr9_measurements,
            semantic: semantic_measurement,
            rerank: rerank_measurement,
            hydration: None,
        },
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
    let input_candidates = selected.iter().map(composition_lane_candidate_count).sum();
    let started = Instant::now();
    let output = kernel.compose(
        &FusionStageInput {
            profile,
            lanes: selected,
        },
        diversity,
    )?;
    let output_candidates = output.ranked_candidates.len() as u64;
    Ok(Pr10ChannelAblationResultV1 {
        ablation,
        public_lane_statuses: output.public_lane_statuses,
        ranked_candidates: output.ranked_candidates,
        measurement: Pr10NativeStageMeasurementV1 {
            elapsed_micros: elapsed_micros(started),
            input_candidates,
            output_candidates,
        },
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

/// Semantic stage outcome: the retrieval result, the fusion lane input it
/// contributes (absent when the stage stays pending), and the stage measurement.
type SemanticStageOutcome = (
    Pr10NativeStageResultV1<Pr10ExactFlatOracleV1>,
    Option<CompositionLaneInput>,
    Pr10NativeStageResultV1<Pr10NativeStageMeasurementV1>,
);

fn evaluate_semantic(
    semantic: Option<Pr10NativeSemanticInputV1<'_>>,
    fusion_profile: &FusionProfile,
) -> Result<SemanticStageOutcome, Pr10NativeEvaluationErrorV1> {
    let Some(semantic) = semantic else {
        return Ok((
            Pr10NativeStageResultV1::Pending {
                reason: Pr10NativePendingReasonV1::SemanticArtifactUnavailable,
            },
            None,
            Pr10NativeStageResultV1::Pending {
                reason: Pr10NativePendingReasonV1::SemanticArtifactUnavailable,
            },
        ));
    };
    if semantic.request.base.profile_id != fusion_profile.profile_id {
        return Err(Pr10NativeEvaluationErrorV1::Contract(
            "native semantic request and fusion profile do not share one identity".to_owned(),
        ));
    }
    let started = Instant::now();
    let outcome = semantic
        .lane
        .retrieve_semantic(semantic.request)
        .map_err(|error| Pr10NativeEvaluationErrorV1::Contract(error.to_string()))?;
    match outcome {
        RetrieverOutcome::Complete(batch) => {
            let oracle = exact_flat_oracle(&batch)?;
            let measurement = Pr10NativeStageResultV1::Complete(Pr10NativeStageMeasurementV1 {
                elapsed_micros: elapsed_micros(started),
                input_candidates: oracle.eligible,
                output_candidates: oracle.hits.len() as u64,
            });
            let lane = CompositionLaneInput::new(
                RetrieverKind::Semantic,
                RetrieverOutcome::Complete(batch),
            )?;
            Ok((
                Pr10NativeStageResultV1::Complete(oracle),
                Some(lane),
                measurement,
            ))
        }
        RetrieverOutcome::Partial { .. } => Ok((
            Pr10NativeStageResultV1::Pending {
                reason: Pr10NativePendingReasonV1::SemanticGenerationIncomplete,
            },
            None,
            Pr10NativeStageResultV1::Pending {
                reason: Pr10NativePendingReasonV1::SemanticGenerationIncomplete,
            },
        )),
        RetrieverOutcome::Unavailable(
            RetrievalFailure::AuthorityUnavailable { .. }
            | RetrievalFailure::IncompatibleProjection { .. }
            | RetrievalFailure::StaleSource,
        )
        | RetrieverOutcome::Denied
        | RetrieverOutcome::Stale(_) => Ok((
            Pr10NativeStageResultV1::Pending {
                reason: Pr10NativePendingReasonV1::SemanticGenerationUnavailable,
            },
            None,
            Pr10NativeStageResultV1::Pending {
                reason: Pr10NativePendingReasonV1::SemanticGenerationUnavailable,
            },
        )),
        RetrieverOutcome::Unavailable(
            RetrievalFailure::InvalidRequest { .. } | RetrievalFailure::Internal { .. },
        ) => Err(Pr10NativeEvaluationErrorV1::Contract(
            "production exact-flat semantic execution failed".to_owned(),
        )),
        RetrieverOutcome::BudgetExceeded(_) => Err(Pr10NativeEvaluationErrorV1::Contract(
            "production exact-flat semantic execution exceeded its evaluated budget".to_owned(),
        )),
        RetrieverOutcome::Cancelled => Ok((
            Pr10NativeStageResultV1::Pending {
                reason: Pr10NativePendingReasonV1::SemanticCancelled,
            },
            None,
            Pr10NativeStageResultV1::Pending {
                reason: Pr10NativePendingReasonV1::SemanticCancelled,
            },
        )),
    }
}

fn composition_lane_candidate_count(lane: &CompositionLaneInput) -> u64 {
    match &lane.outcome {
        RetrieverOutcome::Complete(batch) | RetrieverOutcome::Partial { value: batch, .. } => {
            batch.candidates.len() as u64
        }
        RetrieverOutcome::Unavailable(_)
        | RetrieverOutcome::Denied
        | RetrieverOutcome::Stale(_)
        | RetrieverOutcome::BudgetExceeded(_)
        | RetrieverOutcome::Cancelled => 0,
    }
}

fn rerank_stage_measurement(
    rerank: &Pr10RerankComparisonV1,
) -> Pr10NativeStageResultV1<Pr10NativeStageMeasurementV1> {
    match (&rerank.on, &rerank.execution) {
        (Pr10NativeStageResultV1::Complete(on), Pr10NativeStageResultV1::Complete(execution)) => {
            Pr10NativeStageResultV1::Complete(Pr10NativeStageMeasurementV1 {
                elapsed_micros: execution.elapsed_micros,
                input_candidates: rerank.off.len() as u64,
                output_candidates: on.len() as u64,
            })
        }
        (Pr10NativeStageResultV1::Pending { reason }, Pr10NativeStageResultV1::Pending { .. }) => {
            Pr10NativeStageResultV1::Pending { reason: *reason }
        }
        (Pr10NativeStageResultV1::NotRequested, Pr10NativeStageResultV1::NotRequested) => {
            Pr10NativeStageResultV1::NotRequested
        }
        _ => Pr10NativeStageResultV1::Pending {
            reason: Pr10NativePendingReasonV1::RerankerUnavailable,
        },
    }
}

fn elapsed_micros(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
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
    fusion_profile: &FusionProfile,
    rerank: Option<Pr10NativeRerankInputV1<'_>>,
) -> Result<Pr10RerankComparisonV1, Pr10NativeEvaluationErrorV1> {
    let off = pre_rerank.to_vec();
    if !requested {
        if rerank.is_some() || fusion_profile.rerank_policy_id.is_some() {
            return Err(Pr10NativeEvaluationErrorV1::Contract(
                "rerank authorities were supplied for a profile that does not request reranking"
                    .to_owned(),
            ));
        }
        return Ok(Pr10RerankComparisonV1 {
            off,
            on: Pr10NativeStageResultV1::NotRequested,
            execution: Pr10NativeStageResultV1::NotRequested,
        });
    }
    let Some(rerank) = rerank else {
        return Ok(pending_rerank(
            off,
            Pr10NativePendingReasonV1::RerankerArtifactUnavailable,
        ));
    };
    if fusion_profile.rerank_policy_id.as_ref() != Some(&rerank.policy.policy_id)
        || rerank.request.profile_id != fusion_profile.profile_id
    {
        return Err(Pr10NativeEvaluationErrorV1::Contract(
            "native rerank request, policy, and fusion profile do not share one identity"
                .to_owned(),
        ));
    }
    rerank
        .executor
        .artifact_manifest_digest()
        .validate()
        .map_err(|error| Pr10NativeEvaluationErrorV1::Contract(error.to_string()))?;

    let mut views = BorrowedRerankViewSourceV1 {
        inner: rerank.views,
    };
    let executor = BorrowedRerankExecutorV1 {
        inner: rerank.executor,
    };
    let outcome = BoundedRerankRuntimeV1::new(&mut views, &executor).rerank(
        rerank.request,
        rerank.policy,
        pre_rerank,
        rerank.control,
    );
    match outcome.public_status {
        OptionalStagePublicStatus::Complete => {
            let ordered_approximate_anchors = outcome
                .ordered_candidates
                .iter()
                .filter(|candidate| candidate.candidate.exact_class == ExactClass::Approximate)
                .take(outcome.usage.candidates as usize)
                .map(|candidate| candidate.candidate.anchor_id.clone())
                .collect();
            let execution = Pr10NativeRerankExecutionV1 {
                artifact_manifest_digest: rerank.executor.artifact_manifest_digest().clone(),
                ordered_approximate_anchors,
                input_bytes: outcome.usage.input_bytes,
                input_tokens: outcome.usage.input_tokens,
                work_units: outcome.usage.work_units,
                model_invocations: outcome.usage.model_invocations,
                elapsed_micros: outcome.usage.elapsed_micros,
            };
            Ok(Pr10RerankComparisonV1 {
                off,
                on: Pr10NativeStageResultV1::Complete(outcome.ordered_candidates),
                execution: Pr10NativeStageResultV1::Complete(execution),
            })
        }
        OptionalStagePublicStatus::Unavailable(
            SanitizedStageFailure::AuthorityUnavailable
            | SanitizedStageFailure::Incompatible
            | SanitizedStageFailure::Stale,
        ) => pending_rerank_outcome(
            off,
            outcome.ordered_candidates,
            Pr10NativePendingReasonV1::RerankerUnavailable,
        ),
        OptionalStagePublicStatus::Unavailable(
            SanitizedStageFailure::Invalid | SanitizedStageFailure::Internal,
        ) => Err(Pr10NativeEvaluationErrorV1::Contract(
            "production rerank runtime failed".to_owned(),
        )),
        OptionalStagePublicStatus::Rejected(_) => Err(Pr10NativeEvaluationErrorV1::Contract(
            "production rerank rejected its evaluated request".to_owned(),
        )),
        OptionalStagePublicStatus::Cancelled => pending_rerank_outcome(
            off,
            outcome.ordered_candidates,
            Pr10NativePendingReasonV1::RerankCancelled,
        ),
        OptionalStagePublicStatus::BudgetExceeded(_) => Err(Pr10NativeEvaluationErrorV1::Contract(
            "production rerank exceeded its evaluated budget".to_owned(),
        )),
        OptionalStagePublicStatus::NotRequested => Err(Pr10NativeEvaluationErrorV1::Contract(
            "requested production rerank returned not_requested".to_owned(),
        )),
    }
}

struct BorrowedRerankViewSourceV1<'a> {
    inner: &'a mut dyn EphemeralRerankViewSourceV1,
}

impl EphemeralRerankViewSourceV1 for BorrowedRerankViewSourceV1<'_> {
    fn authorize_ephemeral_view(
        &mut self,
        request: &RetrievalRequest,
        candidate: &RankedCandidate,
        permit: &RerankViewPermitV1,
    ) -> RerankViewOutcomeV1 {
        self.inner
            .authorize_ephemeral_view(request, candidate, permit)
    }
}

struct BorrowedRerankExecutorV1<'a> {
    inner: &'a dyn AdmittedNativeRerankExecutorV1,
}

impl DeterministicLocalRerankExecutorV1 for BorrowedRerankExecutorV1<'_> {
    fn planned_model_invocations(&self, candidate_count: u32) -> Result<u32, LocalRerankFailureV1> {
        self.inner.planned_model_invocations(candidate_count)
    }

    fn rerank(
        &self,
        policy: &RerankPolicy,
        inputs: &[LocalRerankInputV1<'_>],
        permit: LocalRerankPermitV1,
    ) -> Result<Vec<RetrievalAnchorId>, LocalRerankFailureV1> {
        self.inner.rerank(policy, inputs, permit)
    }
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

fn pending_rerank_outcome(
    off: Vec<RankedCandidate>,
    fallback: Vec<RankedCandidate>,
    reason: Pr10NativePendingReasonV1,
) -> Result<Pr10RerankComparisonV1, Pr10NativeEvaluationErrorV1> {
    if fallback != off {
        return Err(Pr10NativeEvaluationErrorV1::Contract(
            "failed production rerank changed the canonical pre-rerank order".to_owned(),
        ));
    }
    Ok(pending_rerank(off, reason))
}

/// Raw Linux resource sample. Optional values remain absent until the actual
/// process/runtime measurement was captured.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Pr10NativeResourceSampleV1 {
    pub provenance: Pr10NativeResourceProvenanceV1,
    pub eligible_chunks: u64,
    pub latency_samples_us: Vec<u64>,
    pub measured_queries: u64,
    pub cpu_time_us: Option<u64>,
    pub peak_rss_bytes: Option<u64>,
    pub model_bytes: Option<u64>,
    pub vector_bytes: Option<u64>,
    pub index_bytes: Option<u64>,
    pub cache_bytes: Option<u64>,
    pub clean_projection_build_samples_us: Vec<u64>,
    pub incremental_rebuild_samples_us: Vec<u64>,
    pub projection_cases: BTreeMap<Pr10ProjectionCaseV1, Pr10ProjectionCaseSampleV1>,
}

/// Required projection workload cases at each corpus scale.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Pr10ProjectionCaseV1 {
    Clean,
    OneSymbol,
    Deletion,
    NoOp,
    IdempotencyReplay,
    Cancellation,
    IncompatibleState,
}

/// Observable result of a production projection case.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Pr10ProjectionCaseOutcomeV1 {
    Complete,
    CancelledWithoutPublication,
    FullRebuildIncompatible,
}

/// Raw bounded work for one real projection execution.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Pr10ProjectionCaseSampleV1 {
    pub outcome: Pr10ProjectionCaseOutcomeV1,
    pub elapsed_micros: u64,
    pub input_bytes: u64,
    pub chunks_added_or_changed: u64,
    pub chunks_deleted: u64,
    pub chunks_reused: u64,
    /// Added/changed chunks requiring projection work; reuse decisions do not
    /// invoke the projector.
    pub projection_calls: u64,
}

/// Immutable inputs and production identities behind one Linux observation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Pr10NativeResourceProvenanceV1 {
    pub workload_digest: String,
    pub corpus_digest: String,
    pub scale: String,
    pub code_generation_id: String,
    pub code_source_manifest_digest: String,
    pub incremental_code_generation_id: String,
    pub incremental_code_source_manifest_digest: String,
    pub incremental_before_content_digest: String,
    pub incremental_after_content_digest: String,
    pub vector_generation_id: Option<String>,
    pub artifact_digest: Option<String>,
    pub measurement_method: String,
}

impl Pr10NativeResourceSampleV1 {
    fn is_complete(&self) -> bool {
        !self.provenance.workload_digest.is_empty()
            && !self.provenance.corpus_digest.is_empty()
            && REQUIRED_RESOURCE_SCALES.contains(&self.provenance.scale.as_str())
            && !self.provenance.code_generation_id.is_empty()
            && !self.provenance.code_source_manifest_digest.is_empty()
            && !self.provenance.incremental_code_generation_id.is_empty()
            && !self
                .provenance
                .incremental_code_source_manifest_digest
                .is_empty()
            && !self.provenance.incremental_before_content_digest.is_empty()
            && !self.provenance.incremental_after_content_digest.is_empty()
            && self.provenance.incremental_before_content_digest
                != self.provenance.incremental_after_content_digest
            && !self.provenance.measurement_method.is_empty()
            && self.eligible_chunks != 0
            && self.measured_queries != 0
            && self.measured_queries == self.latency_samples_us.len() as u64
            && self.cpu_time_us.is_some()
            && self.peak_rss_bytes.is_some()
            && self.model_bytes.is_some()
            && self.vector_bytes.is_some()
            && self.index_bytes.is_some()
            && self.cache_bytes.is_some()
            && !self.clean_projection_build_samples_us.is_empty()
            && !self.incremental_rebuild_samples_us.is_empty()
            && self.projection_case_matrix_is_complete()
    }

    fn projection_case_matrix_is_complete(&self) -> bool {
        let observed = self
            .projection_cases
            .keys()
            .copied()
            .collect::<BTreeSet<_>>();
        let expected = REQUIRED_PROJECTION_CASES
            .into_iter()
            .collect::<BTreeSet<_>>();
        if observed != expected {
            return false;
        }
        let Some(clean) = self.projection_cases.get(&Pr10ProjectionCaseV1::Clean) else {
            return false;
        };
        let Some(one_symbol) = self.projection_cases.get(&Pr10ProjectionCaseV1::OneSymbol) else {
            return false;
        };
        let Some(deletion) = self.projection_cases.get(&Pr10ProjectionCaseV1::Deletion) else {
            return false;
        };
        let Some(no_op) = self.projection_cases.get(&Pr10ProjectionCaseV1::NoOp) else {
            return false;
        };
        let Some(idempotency_replay) = self
            .projection_cases
            .get(&Pr10ProjectionCaseV1::IdempotencyReplay)
        else {
            return false;
        };
        let Some(cancellation) = self
            .projection_cases
            .get(&Pr10ProjectionCaseV1::Cancellation)
        else {
            return false;
        };
        let Some(incompatible) = self
            .projection_cases
            .get(&Pr10ProjectionCaseV1::IncompatibleState)
        else {
            return false;
        };
        clean.outcome == Pr10ProjectionCaseOutcomeV1::Complete
            && clean.projection_calls == clean.chunks_added_or_changed
            && clean.projection_calls != 0
            && one_symbol.outcome == Pr10ProjectionCaseOutcomeV1::Complete
            && one_symbol.projection_calls == one_symbol.chunks_added_or_changed
            && one_symbol.projection_calls != 0
            && deletion.outcome == Pr10ProjectionCaseOutcomeV1::Complete
            && deletion.chunks_deleted != 0
            && no_op.outcome == Pr10ProjectionCaseOutcomeV1::Complete
            && no_op.projection_calls == 0
            && no_op.chunks_added_or_changed == 0
            && no_op.chunks_deleted == 0
            && idempotency_replay.outcome == Pr10ProjectionCaseOutcomeV1::Complete
            && idempotency_replay.projection_calls == 0
            && idempotency_replay.chunks_added_or_changed == 0
            && idempotency_replay.chunks_deleted == 0
            && idempotency_replay.chunks_reused == 0
            && cancellation.outcome == Pr10ProjectionCaseOutcomeV1::CancelledWithoutPublication
            && cancellation.projection_calls == 0
            && incompatible.outcome == Pr10ProjectionCaseOutcomeV1::FullRebuildIncompatible
            && incompatible.projection_calls != 0
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
        for (scale, sample) in [("current", current), ("10x", ten_x)] {
            match sample {
                Pr10NativeStageResultV1::Complete(sample) if !sample.is_complete() => {
                    return Err(Pr10NativeEvaluationErrorV1::Contract(
                        "complete resource evidence is missing a raw observation".to_owned(),
                    ));
                }
                Pr10NativeStageResultV1::Complete(sample) if sample.provenance.scale != scale => {
                    return Err(Pr10NativeEvaluationErrorV1::Contract(
                        "resource evidence provenance names the wrong scale".to_owned(),
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
            && current
                .eligible_chunks
                .checked_mul(10)
                .is_none_or(|expected| ten_x.eligible_chunks != expected)
        {
            return Err(Pr10NativeEvaluationErrorV1::Contract(
                "10x resource evidence must contain exactly ten times the eligible chunks"
                    .to_owned(),
            ));
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
    use tracedecay_domain::canonical_sha256;

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

    fn projection_case_sample(
        outcome: Pr10ProjectionCaseOutcomeV1,
        chunks_added_or_changed: u64,
        chunks_deleted: u64,
        chunks_reused: u64,
        projection_calls: u64,
    ) -> Pr10ProjectionCaseSampleV1 {
        Pr10ProjectionCaseSampleV1 {
            outcome,
            elapsed_micros: 1,
            input_bytes: 1,
            chunks_added_or_changed,
            chunks_deleted,
            chunks_reused,
            projection_calls,
        }
    }

    fn complete_resource_sample() -> Pr10NativeResourceSampleV1 {
        Pr10NativeResourceSampleV1 {
            provenance: Pr10NativeResourceProvenanceV1 {
                workload_digest: "sha256:workload".to_owned(),
                corpus_digest: "sha256:corpus".to_owned(),
                scale: "current".to_owned(),
                code_generation_id: "code-generation".to_owned(),
                code_source_manifest_digest: "sha256:code-source".to_owned(),
                incremental_code_generation_id: "incremental-generation".to_owned(),
                incremental_code_source_manifest_digest: "sha256:incremental-source".to_owned(),
                incremental_before_content_digest: "sha256:before".to_owned(),
                incremental_after_content_digest: "sha256:after".to_owned(),
                vector_generation_id: Some("vector-generation".to_owned()),
                artifact_digest: Some("sha256:artifact".to_owned()),
                measurement_method: "linux-test".to_owned(),
            },
            eligible_chunks: 2,
            latency_samples_us: vec![1],
            measured_queries: 1,
            cpu_time_us: Some(1),
            peak_rss_bytes: Some(1),
            model_bytes: Some(1),
            vector_bytes: Some(1),
            index_bytes: Some(1),
            cache_bytes: Some(1),
            clean_projection_build_samples_us: vec![1],
            incremental_rebuild_samples_us: vec![1],
            projection_cases: BTreeMap::from([
                (
                    Pr10ProjectionCaseV1::Clean,
                    projection_case_sample(Pr10ProjectionCaseOutcomeV1::Complete, 2, 0, 0, 2),
                ),
                (
                    Pr10ProjectionCaseV1::OneSymbol,
                    projection_case_sample(Pr10ProjectionCaseOutcomeV1::Complete, 1, 0, 1, 1),
                ),
                (
                    Pr10ProjectionCaseV1::Deletion,
                    projection_case_sample(Pr10ProjectionCaseOutcomeV1::Complete, 0, 1, 1, 0),
                ),
                (
                    Pr10ProjectionCaseV1::NoOp,
                    projection_case_sample(Pr10ProjectionCaseOutcomeV1::Complete, 0, 0, 2, 0),
                ),
                (
                    Pr10ProjectionCaseV1::IdempotencyReplay,
                    projection_case_sample(Pr10ProjectionCaseOutcomeV1::Complete, 0, 0, 0, 0),
                ),
                (
                    Pr10ProjectionCaseV1::Cancellation,
                    projection_case_sample(
                        Pr10ProjectionCaseOutcomeV1::CancelledWithoutPublication,
                        0,
                        0,
                        0,
                        0,
                    ),
                ),
                (
                    Pr10ProjectionCaseV1::IncompatibleState,
                    projection_case_sample(
                        Pr10ProjectionCaseOutcomeV1::FullRebuildIncompatible,
                        2,
                        0,
                        0,
                        2,
                    ),
                ),
            ]),
        }
    }

    #[test]
    fn projection_case_catalog_serialization_and_digest_are_stable() {
        assert_eq!(
            serde_json::to_value(REQUIRED_PROJECTION_CASES).expect("serialize projection cases"),
            serde_json::json!([
                "clean",
                "one_symbol",
                "deletion",
                "no_op",
                "idempotency_replay",
                "cancellation",
                "incompatible_state"
            ])
        );
        assert_eq!(
            canonical_sha256(&REQUIRED_PROJECTION_CASES)
                .expect("projection case digest")
                .as_str(),
            "sha256:41e92b630c7ca8093cc3a5944fee08020bcf4b05c269cf111d1e3b4f7c2dd4d4"
        );
        assert!(
            serde_json::from_str::<Pr10ProjectionCaseV1>("\"model_key_change\"").is_err(),
            "the retired lookalike case must not deserialize"
        );
    }

    #[test]
    fn projection_case_matrix_accepts_zero_work_idempotency_replay() {
        assert!(complete_resource_sample().is_complete());
    }

    #[test]
    fn projection_case_matrix_rejects_idempotency_replay_that_reprojects() {
        let mut sample = complete_resource_sample();
        sample
            .projection_cases
            .get_mut(&Pr10ProjectionCaseV1::IdempotencyReplay)
            .expect("idempotency replay")
            .projection_calls = 1;

        assert!(!sample.is_complete());
    }
}
