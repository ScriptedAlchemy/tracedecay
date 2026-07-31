//! Calibrated semantic-lane admission over the exact-flat retriever.
//!
//! This service never edits or reconstructs the query fallback subpayload. It
//! carries the caller's owned `Arc` through unchanged and admits semantic
//! candidates only under an exact projection/generation/cohort calibration.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CalibrationProfileId, CodeGenerationId, ManifestDigest, ProjectionKeyV1,
    QueryFallbackSubpayload, RetrieverBatch, RetrieverKind, RetrieverOutcome,
    SemanticSearchIndexKeyV1, VectorGenerationIdV1, canonical_sha256,
};

use super::{
    CanonicalSemanticDistanceV1, CodeSemanticEvidenceV1, SemanticLaneRetriever,
    SemanticRetrievalRequestV1,
};
use crate::retrieval::fusion::CompositionLaneInput;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticQueryModeV1 {
    FallbackAllowed,
    StrictSemantic,
}

/// Policy-owned decision injected into semantic query execution.
///
/// Policy/application decides whether semantic execution is admitted and what
/// to do if an admitted lane later abstains. The query crate validates and
/// executes this decision but never evaluates retrieval policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticQueryDecisionV1 {
    ExecuteSemantic {
        on_abstention: SemanticAbstentionDispositionV1,
    },
    UseFallback,
    RejectUnavailable,
}

impl SemanticQueryDecisionV1 {
    pub const EXECUTE_WITH_FALLBACK: Self = Self::ExecuteSemantic {
        on_abstention: SemanticAbstentionDispositionV1::UseFallback,
    };
    pub const EXECUTE_STRICT: Self = Self::ExecuteSemantic {
        on_abstention: SemanticAbstentionDispositionV1::RejectUnavailable,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticAbstentionDispositionV1 {
    UseFallback,
    RejectUnavailable,
}

/// Non-ready asynchronous index states. None of these states may start query
/// embedding or wait for vector publication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticIndexStateV1 {
    Unavailable,
    Indexing,
    Degraded,
    Failed,
    Stale,
    Incompatible,
}

/// Atomically published proof that one complete vector generation is current
/// for the request's exact projection, source generation, and manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompleteSemanticGenerationV1 {
    projection_key: ProjectionKeyV1,
    search_index_key: SemanticSearchIndexKeyV1,
    vector_generation: VectorGenerationIdV1,
    source_generation: CodeGenerationId,
    capability_manifest_digest: ManifestDigest,
}

impl CompleteSemanticGenerationV1 {
    pub fn new(
        projection_key: ProjectionKeyV1,
        search_index_key: SemanticSearchIndexKeyV1,
        vector_generation: VectorGenerationIdV1,
        source_generation: CodeGenerationId,
        capability_manifest_digest: ManifestDigest,
    ) -> Result<Self, SemanticAbstentionV1> {
        vector_generation
            .as_digest()
            .validate()
            .map_err(|_| SemanticAbstentionV1::IndexIncompatible)?;
        source_generation
            .validate()
            .map_err(|_| SemanticAbstentionV1::IndexIncompatible)?;
        capability_manifest_digest
            .validate()
            .map_err(|_| SemanticAbstentionV1::IndexIncompatible)?;
        search_index_key
            .validate()
            .map_err(|_| SemanticAbstentionV1::IndexIncompatible)?;
        Ok(Self {
            projection_key,
            search_index_key,
            vector_generation,
            source_generation,
            capability_manifest_digest,
        })
    }

    fn matches(&self, request: &SemanticRetrievalRequestV1<'_>) -> bool {
        self.projection_key == *request.projection.projection_key()
            && self.search_index_key == *request.search_index_key
            && self.vector_generation == request.vector_generation
            && self.source_generation == request.code_generation
            && self.capability_manifest_digest == request.capability_manifest_digest
    }
}

/// Accepted, versioned calibration bound to one immutable semantic cohort.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticCalibrationProfileV1 {
    pub calibration_profile_id: CalibrationProfileId,
    pub cohort_digest: ManifestDigest,
    pub projection_key: ProjectionKeyV1,
    pub vector_generation: VectorGenerationIdV1,
    pub capability_manifest_digest: ManifestDigest,
    pub maximum_distance_micros: i64,
    pub minimum_margin_micros: u64,
}

impl SemanticCalibrationProfileV1 {
    pub fn validate(&self) -> Result<(), SemanticAbstentionV1> {
        self.cohort_digest
            .validate()
            .map_err(|_| SemanticAbstentionV1::CalibrationInvalid)?;
        self.vector_generation
            .as_digest()
            .validate()
            .map_err(|_| SemanticAbstentionV1::CalibrationInvalid)?;
        self.capability_manifest_digest
            .validate()
            .map_err(|_| SemanticAbstentionV1::CalibrationInvalid)?;
        Ok(())
    }

    pub fn canonical_digest(&self) -> Result<ManifestDigest, SemanticAbstentionV1> {
        self.validate()?;
        canonical_sha256(&("tracedecay.semantic-calibration-profile.v1", self))
            .map_err(|_| SemanticAbstentionV1::CalibrationInvalid)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticAbstentionV1 {
    IndexUnavailable,
    Indexing,
    IndexDegraded,
    IndexFailed,
    IndexStale,
    IndexIncompatible,
    CalibrationUnavailable,
    CalibrationInvalid,
    CalibrationShifted,
    NoCandidates,
    BelowAcceptanceThreshold,
    AmbiguousTopCandidates,
    PartialCoverage,
    SemanticUnavailable,
    Cancelled,
    BudgetExceeded,
    Denied,
    Stale,
    LaneFailure,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticCalibrationEvidenceV1 {
    pub calibration_profile_id: CalibrationProfileId,
    pub cohort_digest: ManifestDigest,
    pub best_distance: CanonicalSemanticDistanceV1,
    pub next_best_margin_micros: u64,
}

/// Request admission sampled before the semantic lane is invoked. A non-ready
/// state deliberately carries no request, calibration, or future to await.
pub enum SemanticLaneReadinessV1<'a> {
    Ready {
        request: &'a SemanticRetrievalRequestV1<'a>,
        generation: &'a CompleteSemanticGenerationV1,
        calibration: Option<&'a SemanticCalibrationProfileV1>,
    },
    Unavailable(SemanticIndexStateV1),
}

// Boxing the large variant would ripple through in-flight construction/match
// sites; the size gap is accepted here.
#[allow(clippy::large_enum_variant)]
pub enum SemanticQueryServiceOutcomeV1 {
    Augmented {
        semantic_lane: CompositionLaneInput,
        calibration: SemanticCalibrationEvidenceV1,
        fallback: Arc<QueryFallbackSubpayload>,
    },
    Fallback {
        abstention: SemanticAbstentionV1,
        fallback: Arc<QueryFallbackSubpayload>,
    },
}

impl SemanticQueryServiceOutcomeV1 {
    pub fn fallback(&self) -> &Arc<QueryFallbackSubpayload> {
        match self {
            Self::Augmented { fallback, .. } | Self::Fallback { fallback, .. } => fallback,
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SemanticQueryServiceError {
    #[error("strict semantic retrieval is unavailable: {0:?}")]
    StrictUnavailable(SemanticAbstentionV1),
    #[error("the authorized query fallback binding is invalid")]
    InvalidFallback,
    #[error("the authenticated semantic continuation is invalid or stale")]
    InvalidCursor,
    #[error("the injected semantic policy decision contradicts the immutable readiness facts")]
    InvalidPolicyDecision,
}

pub struct CalibratedSemanticQueryService<'a, L> {
    lane: &'a L,
}

impl<'a, L> CalibratedSemanticQueryService<'a, L>
where
    L: SemanticLaneRetriever,
{
    pub const fn new(lane: &'a L) -> Self {
        Self { lane }
    }

    pub fn execute(
        &self,
        readiness: SemanticLaneReadinessV1<'_>,
        decision: SemanticQueryDecisionV1,
        fallback: Arc<QueryFallbackSubpayload>,
    ) -> Result<SemanticQueryServiceOutcomeV1, SemanticQueryServiceError> {
        if fallback.validate().is_err() {
            return Err(SemanticQueryServiceError::InvalidFallback);
        }
        let (request, generation, calibration, on_abstention) = match (decision, readiness) {
            (
                SemanticQueryDecisionV1::ExecuteSemantic { on_abstention },
                SemanticLaneReadinessV1::Ready {
                    request,
                    generation,
                    calibration,
                },
            ) => (request, generation, calibration, on_abstention),
            (SemanticQueryDecisionV1::UseFallback, SemanticLaneReadinessV1::Unavailable(state)) => {
                return Ok(SemanticQueryServiceOutcomeV1::Fallback {
                    abstention: index_abstention(state),
                    fallback,
                });
            }
            (
                SemanticQueryDecisionV1::RejectUnavailable,
                SemanticLaneReadinessV1::Unavailable(state),
            ) => {
                return Err(SemanticQueryServiceError::StrictUnavailable(
                    index_abstention(state),
                ));
            }
            (
                SemanticQueryDecisionV1::ExecuteSemantic { .. },
                SemanticLaneReadinessV1::Unavailable(_),
            )
            | (
                SemanticQueryDecisionV1::UseFallback | SemanticQueryDecisionV1::RejectUnavailable,
                SemanticLaneReadinessV1::Ready { .. },
            ) => return Err(SemanticQueryServiceError::InvalidPolicyDecision),
        };
        if !generation.matches(request) {
            return self.abstain(
                on_abstention,
                SemanticAbstentionV1::IndexIncompatible,
                fallback,
            );
        }
        let Some(calibration) = calibration else {
            return self.abstain(
                on_abstention,
                SemanticAbstentionV1::CalibrationUnavailable,
                fallback,
            );
        };
        if let Err(abstention) = preflight_calibration(request, calibration) {
            return self.abstain(on_abstention, abstention, fallback);
        }
        let Ok(outcome) = self.lane.retrieve_semantic(request) else {
            return self.abstain(on_abstention, SemanticAbstentionV1::LaneFailure, fallback);
        };
        let batch = match outcome {
            RetrieverOutcome::Complete(batch) => batch,
            RetrieverOutcome::Partial { .. } => {
                return self.abstain(
                    on_abstention,
                    SemanticAbstentionV1::PartialCoverage,
                    fallback,
                );
            }
            RetrieverOutcome::Unavailable(_) => {
                return self.abstain(
                    on_abstention,
                    SemanticAbstentionV1::SemanticUnavailable,
                    fallback,
                );
            }
            RetrieverOutcome::Denied => {
                return self.abstain(on_abstention, SemanticAbstentionV1::Denied, fallback);
            }
            RetrieverOutcome::Stale(_) => {
                return self.abstain(on_abstention, SemanticAbstentionV1::Stale, fallback);
            }
            RetrieverOutcome::BudgetExceeded(_) => {
                return self.abstain(
                    on_abstention,
                    SemanticAbstentionV1::BudgetExceeded,
                    fallback,
                );
            }
            RetrieverOutcome::Cancelled => {
                return self.abstain(on_abstention, SemanticAbstentionV1::Cancelled, fallback);
            }
        };
        let evidence = match evaluate_calibration(request, &batch, calibration) {
            Ok(evidence) => evidence,
            Err(abstention) => return self.abstain(on_abstention, abstention, fallback),
        };
        let Ok(semantic_lane) =
            CompositionLaneInput::new(RetrieverKind::Semantic, RetrieverOutcome::Complete(batch))
        else {
            return self.abstain(on_abstention, SemanticAbstentionV1::LaneFailure, fallback);
        };
        Ok(SemanticQueryServiceOutcomeV1::Augmented {
            semantic_lane,
            calibration: evidence,
            fallback,
        })
    }

    fn abstain(
        &self,
        disposition: SemanticAbstentionDispositionV1,
        abstention: SemanticAbstentionV1,
        fallback: Arc<QueryFallbackSubpayload>,
    ) -> Result<SemanticQueryServiceOutcomeV1, SemanticQueryServiceError> {
        match disposition {
            SemanticAbstentionDispositionV1::UseFallback => {
                Ok(SemanticQueryServiceOutcomeV1::Fallback {
                    abstention,
                    fallback,
                })
            }
            SemanticAbstentionDispositionV1::RejectUnavailable => {
                Err(SemanticQueryServiceError::StrictUnavailable(abstention))
            }
        }
    }
}

fn index_abstention(state: SemanticIndexStateV1) -> SemanticAbstentionV1 {
    match state {
        SemanticIndexStateV1::Unavailable => SemanticAbstentionV1::IndexUnavailable,
        SemanticIndexStateV1::Indexing => SemanticAbstentionV1::Indexing,
        SemanticIndexStateV1::Degraded => SemanticAbstentionV1::IndexDegraded,
        SemanticIndexStateV1::Failed => SemanticAbstentionV1::IndexFailed,
        SemanticIndexStateV1::Stale => SemanticAbstentionV1::IndexStale,
        SemanticIndexStateV1::Incompatible => SemanticAbstentionV1::IndexIncompatible,
    }
}

fn preflight_calibration(
    request: &SemanticRetrievalRequestV1<'_>,
    calibration: &SemanticCalibrationProfileV1,
) -> Result<(), SemanticAbstentionV1> {
    calibration.validate()?;
    if calibration.projection_key != *request.projection.projection_key()
        || calibration.vector_generation != request.vector_generation
        || calibration.capability_manifest_digest != request.capability_manifest_digest
    {
        return Err(SemanticAbstentionV1::CalibrationShifted);
    }
    Ok(())
}

fn evaluate_calibration(
    request: &SemanticRetrievalRequestV1<'_>,
    batch: &RetrieverBatch<CodeSemanticEvidenceV1>,
    calibration: &SemanticCalibrationProfileV1,
) -> Result<SemanticCalibrationEvidenceV1, SemanticAbstentionV1> {
    preflight_calibration(request, calibration)?;
    let Some(best_candidate) = batch.candidates.first() else {
        return Err(SemanticAbstentionV1::NoCandidates);
    };
    let best = batch
        .evidence_by_occurrence
        .get(&best_candidate.source_occurrence_id)
        .ok_or(SemanticAbstentionV1::CalibrationInvalid)?;
    if best.distance.micros() > calibration.maximum_distance_micros {
        return Err(SemanticAbstentionV1::BelowAcceptanceThreshold);
    }
    let next_best_margin_micros = match batch.candidates.get(1) {
        Some(candidate) => {
            let next = batch
                .evidence_by_occurrence
                .get(&candidate.source_occurrence_id)
                .ok_or(SemanticAbstentionV1::CalibrationInvalid)?;
            u64::try_from(i128::from(next.distance.micros()) - i128::from(best.distance.micros()))
                .unwrap_or(0)
        }
        None => u64::MAX,
    };
    if next_best_margin_micros < calibration.minimum_margin_micros {
        return Err(SemanticAbstentionV1::AmbiguousTopCandidates);
    }
    Ok(SemanticCalibrationEvidenceV1 {
        calibration_profile_id: calibration.calibration_profile_id.clone(),
        cohort_digest: calibration.cohort_digest.clone(),
        best_distance: best.distance,
        next_best_margin_micros,
    })
}
