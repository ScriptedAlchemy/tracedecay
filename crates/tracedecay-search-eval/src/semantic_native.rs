//! Native semantic/rerank evaluation over production retrieval ports.
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
    OptionalStagePublicStatus, PublicRetrieverStatus, QueryFallbackSubpayload, RankedCandidate,
    RerankPolicy, RetrievalAnchorId, RetrievalFailure, RetrievalRequest, RetrieverBatch,
    RetrieverKind, RetrieverOutcome, SanitizedStageFailure, SourceFreshness,
};

use super::candidate_output::{ProfileSpecV1, ResourceSampleV1};
use tracedecay_query::retrieval::fusion::{
    CompositionKernel, CompositionLaneInput, FusionStageError, FusionStageInput,
};
/// Deterministic local executor admitted from one verified artifact. The trait
/// now lives beside its supertrait in the query kernel; this re-export keeps
/// the evaluator's contract surface stable for existing callers.
pub use tracedecay_query::retrieval::rerank::AdmittedNativeRerankExecutorV1;
use tracedecay_query::retrieval::rerank::{
    BoundedRerankRuntimeV1, DeterministicLocalRerankExecutorV1, EphemeralRerankViewSourceV1,
    LocalRerankFailureV1, LocalRerankInputV1, LocalRerankPermitV1, RerankExecutionControlV1,
    RerankViewOutcomeV1, RerankViewPermitV1,
};
use tracedecay_query::retrieval::semantic::{
    CodeSemanticEvidenceV1, SemanticLaneRetriever, SemanticRetrievalRequestV1, SemanticSearchKindV1,
};

mod fallback;
mod profile_requirements;
mod resource_contract;

pub use profile_requirements::{SemanticNativeProfileRequirementsV1, native_profile_requirements};

use profile_requirements::requirements_for_profile;
use resource_contract::{REQUIRED_PROJECTION_CASES, REQUIRED_RESOURCE_SCALES};

/// Why a real optional-stage run could not be recorded.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticNativePendingReasonV1 {
    SemanticArtifactUnavailable,
    SemanticGenerationUnavailable,
    SemanticGenerationIncomplete,
    SemanticCancelled,
    SemanticTimedOut,
    RerankerArtifactUnavailable,
    RerankerUnavailable,
    RerankCancelled,
    ResourceMeasurementUnavailable,
}

/// Truthful state for one optional native evaluation result.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", content = "value", rename_all = "snake_case")]
pub enum SemanticNativeStageResultV1<T> {
    NotRequested,
    Complete(T),
    Pending {
        reason: SemanticNativePendingReasonV1,
    },
}

impl<T> SemanticNativeStageResultV1<T> {
    pub const fn is_pending(&self) -> bool {
        matches!(self, Self::Pending { .. })
    }
}

/// Channel-removal comparisons required by Plans 15 and 31.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SemanticChannelAblationV1 {
    ExactLexical,
    QueryExactLexicalGraph,
    ExactLexicalSemantic,
    HybridExactLexicalGraphSemantic,
}

/// One deterministic compact-candidate ablation result.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticChannelAblationResultV1 {
    pub ablation: SemanticChannelAblationV1,
    pub public_lane_statuses: BTreeMap<RetrieverKind, PublicRetrieverStatus>,
    pub freshness: Vec<SourceFreshness>,
    pub ranked_candidates: Vec<RankedCandidate>,
    pub measurement: SemanticNativeStageMeasurementV1,
}

/// Raw work observed at one production retrieval boundary.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticNativeStageMeasurementV1 {
    pub elapsed_micros: u64,
    pub input_candidates: u64,
    pub output_candidates: u64,
}

/// query lane work completed before the semantic/fusion comparison.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticNativeQueryStageMeasurementsV1 {
    pub exact: SemanticNativeStageMeasurementV1,
    pub lexical: SemanticNativeStageMeasurementV1,
    pub graph: SemanticNativeStageMeasurementV1,
}

/// Late source reads performed only after the final rank is fixed.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticNativeHydrationMeasurementV1 {
    pub elapsed_micros: u64,
    pub selected_candidates: u64,
    pub source_fetches: u64,
    pub receipts: u64,
    pub bytes_hydrated: u64,
}

/// Per-query measurements kept beside the exact result they describe.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticNativeQueryMeasurementsV1 {
    pub query: SemanticNativeQueryStageMeasurementsV1,
    pub semantic: SemanticNativeStageResultV1<SemanticNativeStageMeasurementV1>,
    pub rerank: SemanticNativeStageResultV1<SemanticNativeStageMeasurementV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hydration: Option<SemanticNativeHydrationMeasurementV1>,
}

/// Exact-flat oracle row retaining production semantic provenance.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticExactFlatOracleHitV1 {
    pub candidate: CompactCandidate,
    pub evidence: CodeSemanticEvidenceV1,
}

/// Exact-flat output before generic fusion or reranking.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticExactFlatOracleV1 {
    pub hits: Vec<SemanticExactFlatOracleHitV1>,
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
pub struct SemanticNativeRerankInputV1<'a> {
    pub request: &'a RetrievalRequest,
    pub policy: &'a RerankPolicy,
    pub views: &'a mut dyn EphemeralRerankViewSourceV1,
    pub executor: &'a dyn AdmittedNativeRerankExecutorV1,
    pub control: &'a dyn RerankExecutionControlV1,
}

/// Raw measured work returned by the admitted reranker.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticNativeRerankExecutionV1 {
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
pub struct SemanticRerankComparisonV1 {
    pub off: Vec<RankedCandidate>,
    pub on: SemanticNativeStageResultV1<Vec<RankedCandidate>>,
    pub execution: SemanticNativeStageResultV1<SemanticNativeRerankExecutionV1>,
}

/// Per-query native semantic evaluation output.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticNativeQueryOutputV1 {
    pub profile_id: String,
    pub fallback_digest: String,
    pub fallback_bytes_unchanged: bool,
    pub ablations: Vec<SemanticChannelAblationResultV1>,
    pub exact_flat_oracle: SemanticNativeStageResultV1<SemanticExactFlatOracleV1>,
    pub rerank: SemanticRerankComparisonV1,
    pub measurements: SemanticNativeQueryMeasurementsV1,
}

/// Real production semantic execution input. The request already binds the
/// admitted projection, immutable vector generation, source generation,
/// authorization, and query MAC.
pub struct SemanticNativeSemanticInputV1<'a> {
    pub lane: &'a dyn SemanticLaneRetriever,
    pub request: &'a SemanticRetrievalRequestV1<'a>,
}

/// Inputs for one native query evaluation.
pub struct SemanticNativeQueryInputV1<'fixed, 'runtime> {
    pub profile_spec: &'fixed ProfileSpecV1,
    pub fusion_profile: &'fixed FusionProfile,
    pub diversity_policy: &'fixed DiversityPolicy,
    pub kernel: &'fixed CompositionKernel,
    pub fallback_lanes: &'fixed [CompositionLaneInput],
    pub query_measurements: SemanticNativeQueryStageMeasurementsV1,
    pub semantic: Option<SemanticNativeSemanticInputV1<'runtime>>,
    pub fallback: &'fixed QueryFallbackSubpayload,
    pub rerank: Option<SemanticNativeRerankInputV1<'runtime>>,
}

#[derive(Debug, Error)]
pub enum SemanticNativeEvaluationErrorV1 {
    #[error("native semantic evaluation contract violation: {0}")]
    Contract(String),
    #[error(transparent)]
    Fusion(#[from] FusionStageError),
}

/// Execute channel ablations, exact-flat oracle capture, bounded rerank
/// off/on, and the fallback-byte invariant for one checked-in query.
pub fn evaluate_native_query(
    input: SemanticNativeQueryInputV1<'_, '_>,
) -> Result<SemanticNativeQueryOutputV1, SemanticNativeEvaluationErrorV1> {
    validate_profile_binding(input.profile_spec, input.fusion_profile)?;
    input
        .fallback
        .validate()
        .map_err(|error| SemanticNativeEvaluationErrorV1::Contract(error.to_string()))?;
    validate_fallback_lanes(input.fallback_lanes)?;
    let fallback_before = canonical_fallback_bytes(input.fallback)?;
    let requirements = requirements_for_profile(input.profile_spec);

    let mut ablations = vec![
        compose_ablation(
            input.kernel,
            input.fusion_profile,
            input.diversity_policy,
            input.fallback_lanes,
            &[RetrieverKind::ExactLiteral, RetrieverKind::Lexical],
            SemanticChannelAblationV1::ExactLexical,
        )?,
        compose_ablation(
            input.kernel,
            input.fusion_profile,
            input.diversity_policy,
            input.fallback_lanes,
            &RetrieverKind::QUERY_FALLBACK_LANES,
            SemanticChannelAblationV1::QueryExactLexicalGraph,
        )?,
    ];

    let (oracle, semantic_lane, semantic_measurement) = if !requirements.semantic_requested {
        if input.semantic.is_some() {
            return Err(SemanticNativeEvaluationErrorV1::Contract(
                "semantic authority was supplied for a profile that does not request semantics"
                    .to_owned(),
            ));
        }
        (
            SemanticNativeStageResultV1::NotRequested,
            None,
            SemanticNativeStageResultV1::NotRequested,
        )
    } else {
        evaluate_semantic(input.semantic, input.fusion_profile)?
    };
    if let Some(semantic_lane) = semantic_lane {
        let mut lanes = input.fallback_lanes.to_vec();
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
            SemanticChannelAblationV1::ExactLexicalSemantic,
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
            SemanticChannelAblationV1::HybridExactLexicalGraphSemantic,
        )?);
    }
    ablations.sort_by_key(|result| result.ablation);
    let query_fallback_baseline = ablations
        .iter()
        .find(|result| result.ablation == SemanticChannelAblationV1::QueryExactLexicalGraph)
        .ok_or_else(|| {
            SemanticNativeEvaluationErrorV1::Contract(
                "native evaluation produced no query fallback baseline".to_owned(),
            )
        })?;
    fallback::validate_query_fallback_baseline(input.fallback, query_fallback_baseline)?;

    let rerank_source = ablations
        .iter()
        .find(|result| {
            result.ablation == SemanticChannelAblationV1::HybridExactLexicalGraphSemantic
        })
        .or_else(|| {
            ablations
                .iter()
                .find(|result| result.ablation == SemanticChannelAblationV1::QueryExactLexicalGraph)
        })
        .ok_or_else(|| {
            SemanticNativeEvaluationErrorV1::Contract(
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
    Ok(SemanticNativeQueryOutputV1 {
        profile_id: input.profile_spec.profile_id.clone(),
        fallback_digest: input.fallback.digest.as_str().to_owned(),
        fallback_bytes_unchanged: fallback_before == fallback_after,
        ablations,
        exact_flat_oracle: oracle,
        rerank,
        measurements: SemanticNativeQueryMeasurementsV1 {
            query: input.query_measurements,
            semantic: semantic_measurement,
            rerank: rerank_measurement,
            hydration: None,
        },
    })
}

fn validate_profile_binding(
    profile: &ProfileSpecV1,
    fusion: &FusionProfile,
) -> Result<(), SemanticNativeEvaluationErrorV1> {
    let expected = format!("profile.{}", profile.profile_id);
    if fusion.profile_id.as_str() != expected {
        return Err(SemanticNativeEvaluationErrorV1::Contract(format!(
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
            return Err(SemanticNativeEvaluationErrorV1::Contract(format!(
                "{} weight does not bind checked-in profile {}",
                lane.as_str(),
                profile.profile_id
            )));
        }
    }
    Ok(())
}

fn validate_fallback_lanes(
    lanes: &[CompositionLaneInput],
) -> Result<(), SemanticNativeEvaluationErrorV1> {
    let observed = lanes.iter().map(|lane| lane.lane).collect::<BTreeSet<_>>();
    let expected = RetrieverKind::QUERY_FALLBACK_LANES
        .into_iter()
        .collect::<BTreeSet<_>>();
    if observed != expected || lanes.len() != expected.len() {
        return Err(SemanticNativeEvaluationErrorV1::Contract(
            "native semantic input must contain exactly the query exact/lexical/graph lanes"
                .to_owned(),
        ));
    }
    Ok(())
}

fn canonical_fallback_bytes(
    fallback: &QueryFallbackSubpayload,
) -> Result<Vec<u8>, SemanticNativeEvaluationErrorV1> {
    serde_json::to_vec(fallback)
        .map_err(|error| SemanticNativeEvaluationErrorV1::Contract(error.to_string()))
}

fn compose_ablation(
    kernel: &CompositionKernel,
    profile: &FusionProfile,
    diversity: &DiversityPolicy,
    lanes: &[CompositionLaneInput],
    admitted: &[RetrieverKind],
    ablation: SemanticChannelAblationV1,
) -> Result<SemanticChannelAblationResultV1, SemanticNativeEvaluationErrorV1> {
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
        return Err(SemanticNativeEvaluationErrorV1::Contract(format!(
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
    Ok(SemanticChannelAblationResultV1 {
        ablation,
        public_lane_statuses: output.public_lane_statuses,
        freshness: output.freshness,
        ranked_candidates: output.ranked_candidates,
        measurement: SemanticNativeStageMeasurementV1 {
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
    SemanticNativeStageResultV1<SemanticExactFlatOracleV1>,
    Option<CompositionLaneInput>,
    SemanticNativeStageResultV1<SemanticNativeStageMeasurementV1>,
);

fn evaluate_semantic(
    semantic: Option<SemanticNativeSemanticInputV1<'_>>,
    fusion_profile: &FusionProfile,
) -> Result<SemanticStageOutcome, SemanticNativeEvaluationErrorV1> {
    let Some(semantic) = semantic else {
        return Ok((
            SemanticNativeStageResultV1::Pending {
                reason: SemanticNativePendingReasonV1::SemanticArtifactUnavailable,
            },
            None,
            SemanticNativeStageResultV1::Pending {
                reason: SemanticNativePendingReasonV1::SemanticArtifactUnavailable,
            },
        ));
    };
    if semantic.request.base.profile_id != fusion_profile.profile_id {
        return Err(SemanticNativeEvaluationErrorV1::Contract(
            "native semantic request and fusion profile do not share one identity".to_owned(),
        ));
    }
    let started = Instant::now();
    let outcome = semantic
        .lane
        .retrieve_semantic(semantic.request)
        .map_err(|error| SemanticNativeEvaluationErrorV1::Contract(error.to_string()))?;
    match outcome {
        RetrieverOutcome::Complete(batch) => {
            let oracle = exact_flat_oracle(&batch)?;
            let measurement =
                SemanticNativeStageResultV1::Complete(SemanticNativeStageMeasurementV1 {
                    elapsed_micros: elapsed_micros(started),
                    input_candidates: oracle.eligible,
                    output_candidates: oracle.hits.len() as u64,
                });
            let lane = CompositionLaneInput::new(
                RetrieverKind::Semantic,
                RetrieverOutcome::Complete(batch),
            )?;
            Ok((
                SemanticNativeStageResultV1::Complete(oracle),
                Some(lane),
                measurement,
            ))
        }
        RetrieverOutcome::Partial { .. } => Ok((
            SemanticNativeStageResultV1::Pending {
                reason: SemanticNativePendingReasonV1::SemanticGenerationIncomplete,
            },
            None,
            SemanticNativeStageResultV1::Pending {
                reason: SemanticNativePendingReasonV1::SemanticGenerationIncomplete,
            },
        )),
        RetrieverOutcome::Unavailable(
            RetrievalFailure::AuthorityUnavailable { .. }
            | RetrievalFailure::IncompatibleProjection { .. }
            | RetrievalFailure::StaleSource,
        )
        | RetrieverOutcome::Denied
        | RetrieverOutcome::Stale(_) => Ok((
            SemanticNativeStageResultV1::Pending {
                reason: SemanticNativePendingReasonV1::SemanticGenerationUnavailable,
            },
            None,
            SemanticNativeStageResultV1::Pending {
                reason: SemanticNativePendingReasonV1::SemanticGenerationUnavailable,
            },
        )),
        RetrieverOutcome::Unavailable(
            RetrievalFailure::InvalidRequest { .. } | RetrievalFailure::Internal { .. },
        ) => Err(SemanticNativeEvaluationErrorV1::Contract(
            "production exact-flat semantic execution failed".to_owned(),
        )),
        RetrieverOutcome::BudgetExceeded(_) => Err(SemanticNativeEvaluationErrorV1::Contract(
            "production exact-flat semantic execution exceeded its evaluated budget".to_owned(),
        )),
        RetrieverOutcome::TimedOut(_) => Ok((
            SemanticNativeStageResultV1::Pending {
                reason: SemanticNativePendingReasonV1::SemanticTimedOut,
            },
            None,
            SemanticNativeStageResultV1::Pending {
                reason: SemanticNativePendingReasonV1::SemanticTimedOut,
            },
        )),
        RetrieverOutcome::Cancelled => Ok((
            SemanticNativeStageResultV1::Pending {
                reason: SemanticNativePendingReasonV1::SemanticCancelled,
            },
            None,
            SemanticNativeStageResultV1::Pending {
                reason: SemanticNativePendingReasonV1::SemanticCancelled,
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
        | RetrieverOutcome::TimedOut(_)
        | RetrieverOutcome::Cancelled => 0,
    }
}

fn rerank_stage_measurement(
    rerank: &SemanticRerankComparisonV1,
) -> SemanticNativeStageResultV1<SemanticNativeStageMeasurementV1> {
    match (&rerank.on, &rerank.execution) {
        (
            SemanticNativeStageResultV1::Complete(on),
            SemanticNativeStageResultV1::Complete(execution),
        ) => SemanticNativeStageResultV1::Complete(SemanticNativeStageMeasurementV1 {
            elapsed_micros: execution.elapsed_micros,
            input_candidates: rerank.off.len() as u64,
            output_candidates: on.len() as u64,
        }),
        (
            SemanticNativeStageResultV1::Pending { reason },
            SemanticNativeStageResultV1::Pending { .. },
        ) => SemanticNativeStageResultV1::Pending { reason: *reason },
        (SemanticNativeStageResultV1::NotRequested, SemanticNativeStageResultV1::NotRequested) => {
            SemanticNativeStageResultV1::NotRequested
        }
        _ => SemanticNativeStageResultV1::Pending {
            reason: SemanticNativePendingReasonV1::RerankerUnavailable,
        },
    }
}

fn elapsed_micros(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

fn exact_flat_oracle(
    batch: &RetrieverBatch<CodeSemanticEvidenceV1>,
) -> Result<SemanticExactFlatOracleV1, SemanticNativeEvaluationErrorV1> {
    batch
        .validate()
        .map_err(|error| SemanticNativeEvaluationErrorV1::Contract(error.to_string()))?;
    let mut hits = Vec::with_capacity(batch.candidates.len());
    for candidate in &batch.candidates {
        let evidence = batch
            .evidence_by_occurrence
            .get(&candidate.source_occurrence_id)
            .ok_or_else(|| {
                SemanticNativeEvaluationErrorV1::Contract(
                    "semantic oracle candidate is missing occurrence evidence".to_owned(),
                )
            })?;
        if candidate.retriever != RetrieverKind::Semantic
            || evidence.search_kind != SemanticSearchKindV1::ExactFlat
        {
            return Err(SemanticNativeEvaluationErrorV1::Contract(
                "semantic oracle accepts only the production exact-flat lane".to_owned(),
            ));
        }
        hits.push(SemanticExactFlatOracleHitV1 {
            candidate: candidate.clone(),
            evidence: evidence.clone(),
        });
    }
    Ok(SemanticExactFlatOracleV1 {
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
    rerank: Option<SemanticNativeRerankInputV1<'_>>,
) -> Result<SemanticRerankComparisonV1, SemanticNativeEvaluationErrorV1> {
    let off = pre_rerank.to_vec();
    if !requested {
        if rerank.is_some() || fusion_profile.rerank_policy_id.is_some() {
            return Err(SemanticNativeEvaluationErrorV1::Contract(
                "rerank authorities were supplied for a profile that does not request reranking"
                    .to_owned(),
            ));
        }
        return Ok(SemanticRerankComparisonV1 {
            off,
            on: SemanticNativeStageResultV1::NotRequested,
            execution: SemanticNativeStageResultV1::NotRequested,
        });
    }
    let Some(rerank) = rerank else {
        return Ok(pending_rerank(
            off,
            SemanticNativePendingReasonV1::RerankerArtifactUnavailable,
        ));
    };
    if fusion_profile.rerank_policy_id.as_ref() != Some(&rerank.policy.policy_id)
        || rerank.request.profile_id != fusion_profile.profile_id
    {
        return Err(SemanticNativeEvaluationErrorV1::Contract(
            "native rerank request, policy, and fusion profile do not share one identity"
                .to_owned(),
        ));
    }
    rerank
        .executor
        .artifact_manifest_digest()
        .validate()
        .map_err(|error| SemanticNativeEvaluationErrorV1::Contract(error.to_string()))?;

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
            let execution = SemanticNativeRerankExecutionV1 {
                artifact_manifest_digest: rerank.executor.artifact_manifest_digest().clone(),
                ordered_approximate_anchors,
                input_bytes: outcome.usage.input_bytes,
                input_tokens: outcome.usage.input_tokens,
                work_units: outcome.usage.work_units,
                model_invocations: outcome.usage.model_invocations,
                elapsed_micros: outcome.usage.elapsed_micros,
            };
            Ok(SemanticRerankComparisonV1 {
                off,
                on: SemanticNativeStageResultV1::Complete(outcome.ordered_candidates),
                execution: SemanticNativeStageResultV1::Complete(execution),
            })
        }
        OptionalStagePublicStatus::Unavailable(
            SanitizedStageFailure::AuthorityUnavailable
            | SanitizedStageFailure::Incompatible
            | SanitizedStageFailure::Stale,
        ) => pending_rerank_outcome(
            off,
            outcome.ordered_candidates,
            SemanticNativePendingReasonV1::RerankerUnavailable,
        ),
        OptionalStagePublicStatus::Unavailable(
            SanitizedStageFailure::Invalid | SanitizedStageFailure::Internal,
        ) => Err(SemanticNativeEvaluationErrorV1::Contract(
            "production rerank runtime failed".to_owned(),
        )),
        OptionalStagePublicStatus::Rejected(_) => Err(SemanticNativeEvaluationErrorV1::Contract(
            "production rerank rejected its evaluated request".to_owned(),
        )),
        OptionalStagePublicStatus::Cancelled => pending_rerank_outcome(
            off,
            outcome.ordered_candidates,
            SemanticNativePendingReasonV1::RerankCancelled,
        ),
        OptionalStagePublicStatus::BudgetExceeded(_) => {
            Err(SemanticNativeEvaluationErrorV1::Contract(
                "production rerank exceeded its evaluated budget".to_owned(),
            ))
        }
        OptionalStagePublicStatus::NotRequested => Err(SemanticNativeEvaluationErrorV1::Contract(
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
    reason: SemanticNativePendingReasonV1,
) -> SemanticRerankComparisonV1 {
    SemanticRerankComparisonV1 {
        off,
        on: SemanticNativeStageResultV1::Pending { reason },
        execution: SemanticNativeStageResultV1::Pending { reason },
    }
}

fn pending_rerank_outcome(
    off: Vec<RankedCandidate>,
    fallback: Vec<RankedCandidate>,
    reason: SemanticNativePendingReasonV1,
) -> Result<SemanticRerankComparisonV1, SemanticNativeEvaluationErrorV1> {
    if fallback != off {
        return Err(SemanticNativeEvaluationErrorV1::Contract(
            "failed production rerank changed the canonical pre-rerank order".to_owned(),
        ));
    }
    Ok(pending_rerank(off, reason))
}

/// Raw Linux resource sample. Optional values remain absent until the actual
/// process/runtime measurement was captured.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticNativeResourceSampleV1 {
    pub provenance: SemanticNativeResourceProvenanceV1,
    pub eligible_chunks: u64,
    pub latency_samples_us: Vec<u64>,
    pub measured_queries: u64,
    pub cpu_time_us: Option<u64>,
    pub peak_rss_bytes: Option<u64>,
    pub model_bytes: Option<u64>,
    pub tokenizer_bytes: Option<u64>,
    pub vector_bytes: Option<u64>,
    pub index_bytes: Option<u64>,
    pub cache_bytes: Option<u64>,
    /// Cold model/session-open observations from the exact evaluated runtime.
    pub cold_model_load_samples_us: Vec<u64>,
    pub clean_projection_build_samples_us: Vec<u64>,
    pub incremental_rebuild_samples_us: Vec<u64>,
    pub projection_cases: BTreeMap<SemanticProjectionCaseV1, SemanticProjectionCaseSampleV1>,
}

/// Exact resource pins derived only from complete current/10x native
/// observations for one activation-selected profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SemanticActivationResourcePinsV1 {
    pub model_bytes: u64,
    pub tokenizer_bytes: u64,
    pub resident_bytes: u64,
    pub threads: u32,
    pub max_concurrent_sessions: u32,
    pub batch_size: u32,
    pub sequence_length: u32,
    pub load_deadline_ms: u64,
}

/// Required projection workload cases at each corpus scale.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SemanticProjectionCaseV1 {
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
pub enum SemanticProjectionCaseOutcomeV1 {
    Complete,
    CancelledWithoutPublication,
    FullRebuildIncompatible,
}

/// Raw bounded work for one real projection execution.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticProjectionCaseSampleV1 {
    pub outcome: SemanticProjectionCaseOutcomeV1,
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
pub struct SemanticNativeResourceProvenanceV1 {
    pub workload_digest: String,
    pub corpus_digest: String,
    pub scale: String,
    pub code_generation_id: String,
    pub code_source_manifest_digest: String,
    pub incremental_code_generation_id: String,
    pub incremental_code_source_manifest_digest: String,
    pub incremental_before_content_digest: String,
    pub incremental_after_content_digest: String,
    pub threads: u32,
    pub max_concurrent_sessions: u32,
    pub batch_size: u32,
    pub sequence_length: u32,
    pub load_deadline_ms: u64,
    pub vector_generation_id: Option<String>,
    pub artifact_digest: Option<String>,
    pub measurement_method: String,
}

impl SemanticNativeResourceSampleV1 {
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
            && self.provenance.threads != 0
            && self.provenance.max_concurrent_sessions == 1
            && self.provenance.batch_size != 0
            && self.provenance.sequence_length != 0
            && self.provenance.load_deadline_ms != 0
            && !self.provenance.measurement_method.is_empty()
            && self.eligible_chunks != 0
            && self.measured_queries != 0
            && self.measured_queries == self.latency_samples_us.len() as u64
            && self.cpu_time_us.is_some()
            && self.peak_rss_bytes.is_some()
            && self.model_bytes.is_some_and(|bytes| bytes != 0)
            && self.tokenizer_bytes.is_some_and(|bytes| bytes != 0)
            && self.vector_bytes.is_some()
            && self.index_bytes.is_some()
            && self.cache_bytes.is_some()
            && self.cold_model_loads_within_deadline()
            && !self.clean_projection_build_samples_us.is_empty()
            && !self.incremental_rebuild_samples_us.is_empty()
            && self.projection_case_matrix_is_complete()
    }

    fn cold_model_loads_within_deadline(&self) -> bool {
        let Some(deadline_micros) = self.provenance.load_deadline_ms.checked_mul(1_000) else {
            return false;
        };
        !self.cold_model_load_samples_us.is_empty()
            && self
                .cold_model_load_samples_us
                .iter()
                .all(|sample| *sample != 0 && *sample <= deadline_micros)
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
        let Some(clean) = self.projection_cases.get(&SemanticProjectionCaseV1::Clean) else {
            return false;
        };
        let Some(one_symbol) = self
            .projection_cases
            .get(&SemanticProjectionCaseV1::OneSymbol)
        else {
            return false;
        };
        let Some(deletion) = self
            .projection_cases
            .get(&SemanticProjectionCaseV1::Deletion)
        else {
            return false;
        };
        let Some(no_op) = self.projection_cases.get(&SemanticProjectionCaseV1::NoOp) else {
            return false;
        };
        let Some(idempotency_replay) = self
            .projection_cases
            .get(&SemanticProjectionCaseV1::IdempotencyReplay)
        else {
            return false;
        };
        let Some(cancellation) = self
            .projection_cases
            .get(&SemanticProjectionCaseV1::Cancellation)
        else {
            return false;
        };
        let Some(incompatible) = self
            .projection_cases
            .get(&SemanticProjectionCaseV1::IncompatibleState)
        else {
            return false;
        };
        clean.outcome == SemanticProjectionCaseOutcomeV1::Complete
            && clean.projection_calls == clean.chunks_added_or_changed
            && clean.projection_calls != 0
            && one_symbol.outcome == SemanticProjectionCaseOutcomeV1::Complete
            && one_symbol.projection_calls == one_symbol.chunks_added_or_changed
            && one_symbol.projection_calls != 0
            && deletion.outcome == SemanticProjectionCaseOutcomeV1::Complete
            && deletion.chunks_deleted != 0
            && no_op.outcome == SemanticProjectionCaseOutcomeV1::Complete
            && no_op.projection_calls == 0
            && no_op.chunks_added_or_changed == 0
            && no_op.chunks_deleted == 0
            && idempotency_replay.outcome == SemanticProjectionCaseOutcomeV1::Complete
            && idempotency_replay.projection_calls == 0
            && idempotency_replay.chunks_added_or_changed == 0
            && idempotency_replay.chunks_deleted == 0
            && idempotency_replay.chunks_reused == 0
            && cancellation.outcome == SemanticProjectionCaseOutcomeV1::CancelledWithoutPublication
            && cancellation.projection_calls != 0
            && cancellation.chunks_added_or_changed != 0
            && cancellation.projection_calls < cancellation.chunks_added_or_changed
            && incompatible.outcome == SemanticProjectionCaseOutcomeV1::FullRebuildIncompatible
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
pub struct SemanticNativeResourceEvidenceV1 {
    pub samples: BTreeMap<String, SemanticNativeStageResultV1<SemanticNativeResourceSampleV1>>,
}

impl SemanticNativeResourceEvidenceV1 {
    /// Validate exact scale and preserve pending measurements without filling
    /// any absent observation.
    pub fn validate(&self) -> Result<(), SemanticNativeEvaluationErrorV1> {
        let observed = self
            .samples
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let expected = REQUIRED_RESOURCE_SCALES
            .into_iter()
            .collect::<BTreeSet<_>>();
        if observed != expected {
            return Err(SemanticNativeEvaluationErrorV1::Contract(
                "resource evidence must contain exactly current and 10x".to_owned(),
            ));
        }
        let current = self.samples.get("current").ok_or_else(|| {
            SemanticNativeEvaluationErrorV1::Contract(
                "resource evidence is missing current".to_owned(),
            )
        })?;
        let ten_x = self.samples.get("10x").ok_or_else(|| {
            SemanticNativeEvaluationErrorV1::Contract("resource evidence is missing 10x".to_owned())
        })?;
        for (scale, sample) in [("current", current), ("10x", ten_x)] {
            match sample {
                SemanticNativeStageResultV1::Complete(sample) if !sample.is_complete() => {
                    return Err(SemanticNativeEvaluationErrorV1::Contract(
                        "complete resource evidence is missing a raw observation".to_owned(),
                    ));
                }
                SemanticNativeStageResultV1::Complete(sample)
                    if sample.provenance.scale != scale =>
                {
                    return Err(SemanticNativeEvaluationErrorV1::Contract(
                        "resource evidence provenance names the wrong scale".to_owned(),
                    ));
                }
                SemanticNativeStageResultV1::NotRequested => {
                    return Err(SemanticNativeEvaluationErrorV1::Contract(
                        "current and 10x resource measurements are required".to_owned(),
                    ));
                }
                SemanticNativeStageResultV1::Complete(_)
                | SemanticNativeStageResultV1::Pending { .. } => {}
            }
        }
        if let (
            SemanticNativeStageResultV1::Complete(current),
            SemanticNativeStageResultV1::Complete(ten_x),
        ) = (current, ten_x)
            && current
                .eligible_chunks
                .checked_mul(10)
                .is_none_or(|expected| ten_x.eligible_chunks != expected)
        {
            return Err(SemanticNativeEvaluationErrorV1::Contract(
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
                SemanticNativeStageResultV1::Complete(sample) => sample
                    .as_existing_evaluator_sample()
                    .map(|sample| (scale.clone(), sample)),
                SemanticNativeStageResultV1::NotRequested
                | SemanticNativeStageResultV1::Pending { .. } => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CandidateWorkloadV1, load_candidate_workload};
    use tracedecay_domain::canonical_sha256;

    fn checked_in_workload() -> CandidateWorkloadV1 {
        load_candidate_workload(
            &crate::checked_in_fixture_root()
                .join("tests/fixtures/search_quality/query-semantic-candidate-workload-v1.json"),
        )
        .expect("checked-in Plan 15 workload")
    }

    #[test]
    fn checked_in_profiles_request_only_declared_native_stages() {
        let workload = checked_in_workload();

        assert_eq!(
            native_profile_requirements(&workload, "query-fallback").expect("profile"),
            SemanticNativeProfileRequirementsV1 {
                profile_id: "query-fallback".to_owned(),
                semantic_requested: false,
                rerank_requested: false,
            }
        );
        assert_eq!(
            native_profile_requirements(&workload, "hybrid-conservative").expect("profile"),
            SemanticNativeProfileRequirementsV1 {
                profile_id: "hybrid-conservative".to_owned(),
                semantic_requested: true,
                rerank_requested: false,
            }
        );
        assert_eq!(
            native_profile_requirements(&workload, "hybrid-reranked").expect("profile"),
            SemanticNativeProfileRequirementsV1 {
                profile_id: "hybrid-reranked".to_owned(),
                semantic_requested: true,
                rerank_requested: true,
            }
        );
    }

    #[test]
    fn unavailable_native_inputs_remain_pending_without_measurements() {
        let evidence = SemanticNativeResourceEvidenceV1 {
            samples: BTreeMap::from([
                (
                    "10x".to_owned(),
                    SemanticNativeStageResultV1::Pending {
                        reason: SemanticNativePendingReasonV1::ResourceMeasurementUnavailable,
                    },
                ),
                (
                    "current".to_owned(),
                    SemanticNativeStageResultV1::Pending {
                        reason: SemanticNativePendingReasonV1::ResourceMeasurementUnavailable,
                    },
                ),
            ]),
        };

        evidence.validate().expect("truthful pending evidence");
        assert!(evidence.existing_evaluator_samples().is_empty());
    }

    fn projection_case_sample(
        outcome: SemanticProjectionCaseOutcomeV1,
        chunks_added_or_changed: u64,
        chunks_deleted: u64,
        chunks_reused: u64,
        projection_calls: u64,
    ) -> SemanticProjectionCaseSampleV1 {
        SemanticProjectionCaseSampleV1 {
            outcome,
            elapsed_micros: 1,
            input_bytes: 1,
            chunks_added_or_changed,
            chunks_deleted,
            chunks_reused,
            projection_calls,
        }
    }

    fn complete_resource_sample() -> SemanticNativeResourceSampleV1 {
        SemanticNativeResourceSampleV1 {
            provenance: SemanticNativeResourceProvenanceV1 {
                workload_digest: "sha256:workload".to_owned(),
                corpus_digest: "sha256:corpus".to_owned(),
                scale: "current".to_owned(),
                code_generation_id: "code-generation".to_owned(),
                code_source_manifest_digest: "sha256:code-source".to_owned(),
                incremental_code_generation_id: "incremental-generation".to_owned(),
                incremental_code_source_manifest_digest: "sha256:incremental-source".to_owned(),
                incremental_before_content_digest: "sha256:before".to_owned(),
                incremental_after_content_digest: "sha256:after".to_owned(),
                threads: 4,
                max_concurrent_sessions: 1,
                batch_size: 32,
                sequence_length: 512,
                load_deadline_ms: 30_000,
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
            tokenizer_bytes: Some(1),
            vector_bytes: Some(1),
            index_bytes: Some(1),
            cache_bytes: Some(1),
            cold_model_load_samples_us: vec![1],
            clean_projection_build_samples_us: vec![1],
            incremental_rebuild_samples_us: vec![1],
            projection_cases: BTreeMap::from([
                (
                    SemanticProjectionCaseV1::Clean,
                    projection_case_sample(SemanticProjectionCaseOutcomeV1::Complete, 2, 0, 0, 2),
                ),
                (
                    SemanticProjectionCaseV1::OneSymbol,
                    projection_case_sample(SemanticProjectionCaseOutcomeV1::Complete, 1, 0, 1, 1),
                ),
                (
                    SemanticProjectionCaseV1::Deletion,
                    projection_case_sample(SemanticProjectionCaseOutcomeV1::Complete, 0, 1, 1, 0),
                ),
                (
                    SemanticProjectionCaseV1::NoOp,
                    projection_case_sample(SemanticProjectionCaseOutcomeV1::Complete, 0, 0, 2, 0),
                ),
                (
                    SemanticProjectionCaseV1::IdempotencyReplay,
                    projection_case_sample(SemanticProjectionCaseOutcomeV1::Complete, 0, 0, 0, 0),
                ),
                (
                    SemanticProjectionCaseV1::Cancellation,
                    projection_case_sample(
                        SemanticProjectionCaseOutcomeV1::CancelledWithoutPublication,
                        2,
                        0,
                        0,
                        1,
                    ),
                ),
                (
                    SemanticProjectionCaseV1::IncompatibleState,
                    projection_case_sample(
                        SemanticProjectionCaseOutcomeV1::FullRebuildIncompatible,
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
            serde_json::from_str::<SemanticProjectionCaseV1>("\"model_key_change\"").is_err(),
            "the retired lookalike case must not deserialize"
        );
    }

    #[test]
    fn projection_case_matrix_accepts_zero_work_idempotency_replay() {
        assert!(complete_resource_sample().is_complete());
    }

    #[test]
    fn native_resource_sample_requires_tokenizer_and_bounded_cold_load_evidence() {
        let mut sample = complete_resource_sample();
        sample.tokenizer_bytes = None;
        assert!(!sample.is_complete());

        let mut sample = complete_resource_sample();
        sample.cold_model_load_samples_us = vec![30_000_001];
        assert!(!sample.is_complete());

        let mut sample = complete_resource_sample();
        sample.provenance.threads = 0;
        assert!(!sample.is_complete());

        let mut sample = complete_resource_sample();
        sample.provenance.max_concurrent_sessions = 2;
        assert!(!sample.is_complete());
    }

    #[test]
    fn projection_case_matrix_rejects_idempotency_replay_that_reprojects() {
        let mut sample = complete_resource_sample();
        sample
            .projection_cases
            .get_mut(&SemanticProjectionCaseV1::IdempotencyReplay)
            .expect("idempotency replay")
            .projection_calls = 1;

        assert!(!sample.is_complete());
    }

    #[test]
    fn projection_case_matrix_rejects_cancellation_before_any_projection_work() {
        let mut sample = complete_resource_sample();
        let cancellation = sample
            .projection_cases
            .get_mut(&SemanticProjectionCaseV1::Cancellation)
            .expect("cancellation case");
        cancellation.chunks_added_or_changed = 0;
        cancellation.projection_calls = 0;

        assert!(
            !sample.is_complete(),
            "a pre-start cancellation cannot stand in for interrupted semantic projection"
        );
    }

    #[test]
    fn projection_case_matrix_rejects_cancellation_after_all_projection_work() {
        let mut sample = complete_resource_sample();
        let cancellation = sample
            .projection_cases
            .get_mut(&SemanticProjectionCaseV1::Cancellation)
            .expect("cancellation case");
        cancellation.projection_calls = cancellation.chunks_added_or_changed;

        assert!(
            !sample.is_complete(),
            "a completed projection cannot stand in for mid-batch cancellation"
        );
    }
}
