//! Application semantic search composition over the calibrated query service.

use std::path::Path;
use std::sync::Arc;

use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_domain::{QueryFallbackSubpayload, RetrieverBatch, RetrieverOutcome};
use tracedecay_policy::retrieval_selection::{
    RetrievalAvailabilityV1, RetrievalRequirementV1, RetrievalSelectionV1, select_retrieval,
};
use tracedecay_query::retrieval::AuthorizedQueryFallbackV1;
use tracedecay_query::retrieval::ports::{RetrievalExecutionControl, RetrievalPortError};
use tracedecay_query::retrieval::semantic::{
    CalibratedSemanticQueryService, CodeSemanticEvidenceV1, CompleteSemanticGenerationV1,
    SemanticAbstentionDispositionV1, SemanticCalibrationProfileV1, SemanticCodeRetriever,
    SemanticIndexStateV1, SemanticLaneReadinessV1, SemanticLaneRetriever, SemanticQueryDecisionV1,
    SemanticQueryModeV1, SemanticQueryServiceError, SemanticQueryServiceOutcomeV1,
    SemanticRetrievalRequestV1, SemanticVectorReadPort,
};
use tracedecay_semantic::DaemonSemanticRuntimeHandleV1;

use super::super::ports::SemanticRuntimeFuture;
use super::daemon_backend::index_state_from_status;
use super::project_registry::project_semantic_production_runtime;
use super::source_coherence::SemanticSourceCoherenceV1;

pub(super) fn execute_calibrated_semantic_query<'a, L>(
    lane: &'a L,
    readiness: SemanticLaneReadinessV1<'a>,
    mode: SemanticQueryModeV1,
    fallback: Arc<QueryFallbackSubpayload>,
) -> Result<SemanticQueryServiceOutcomeV1, SemanticQueryServiceError>
where
    L: SemanticLaneRetriever,
{
    let availability = match &readiness {
        SemanticLaneReadinessV1::Ready { .. } => RetrievalAvailabilityV1::Ready,
        SemanticLaneReadinessV1::Unavailable(state) => match state {
            SemanticIndexStateV1::Unavailable => RetrievalAvailabilityV1::Unavailable,
            SemanticIndexStateV1::Indexing => RetrievalAvailabilityV1::Indexing,
            SemanticIndexStateV1::Degraded => RetrievalAvailabilityV1::Degraded,
            SemanticIndexStateV1::Failed => RetrievalAvailabilityV1::Failed,
            SemanticIndexStateV1::Stale => RetrievalAvailabilityV1::Stale,
            SemanticIndexStateV1::Incompatible => RetrievalAvailabilityV1::Incompatible,
        },
    };
    let requirement = match mode {
        SemanticQueryModeV1::FallbackAllowed => RetrievalRequirementV1::FallbackAllowed,
        SemanticQueryModeV1::StrictSemantic => RetrievalRequirementV1::StrictSemantic,
    };
    let on_abstention = match mode {
        SemanticQueryModeV1::FallbackAllowed => SemanticAbstentionDispositionV1::UseFallback,
        SemanticQueryModeV1::StrictSemantic => SemanticAbstentionDispositionV1::RejectUnavailable,
    };
    let decision = match select_retrieval(availability, requirement) {
        RetrievalSelectionV1::Semantic => {
            SemanticQueryDecisionV1::ExecuteSemantic { on_abstention }
        }
        RetrievalSelectionV1::FrozenFallback => SemanticQueryDecisionV1::UseFallback,
        RetrievalSelectionV1::Unavailable => SemanticQueryDecisionV1::RejectUnavailable,
    };
    CalibratedSemanticQueryService::new(lane).execute(readiness, decision, fallback)
}

/// Complete input set for one application semantic-search composition.
pub struct ApplicationSemanticSearchParametersV1<'a, V, C> {
    pub handle: &'a DaemonSemanticRuntimeHandleV1,
    pub request: &'a SemanticRetrievalRequestV1<'a>,
    pub generation: &'a CompleteSemanticGenerationV1,
    pub calibration: Option<&'a SemanticCalibrationProfileV1>,
    pub vectors: &'a V,
    pub control: &'a C,
    pub mode: SemanticQueryModeV1,
    pub fallback: Arc<QueryFallbackSubpayload>,
    /// How the served vectors' source binding was admitted. With
    /// [`SemanticSourceCoherenceV1::ProvenSourceContent`], query-embedder
    /// admission falls back to model identity (projection key) when the
    /// runtime's exact pointer names a different publication of the same
    /// source truth; the caller's corpus proof is the authority for that.
    pub source_coherence: SemanticSourceCoherenceV1,
}

/// Application search composition: admit `SemanticCodeRetriever` only through
/// [`DaemonSemanticRuntimeHandleV1::query_factory`].
///
/// Non-ready / indexing / degraded states never construct the retriever and
/// return the frozen query fallback without waiting on `FastEmbed` download or
/// projection. Exact/lexical/graph owners stay independently callable.
#[hotpath::measure(label = "usecases.semantic.search")]
pub fn compose_application_semantic_search<'a, V, C>(
    parameters: ApplicationSemanticSearchParametersV1<'a, V, C>,
) -> Result<SemanticQueryServiceOutcomeV1, SemanticQueryServiceError>
where
    V: SemanticVectorReadPort,
    C: RetrievalExecutionControl + Sync,
{
    let ApplicationSemanticSearchParametersV1 {
        handle,
        request,
        generation,
        calibration,
        vectors,
        control,
        mode,
        fallback,
        source_coherence,
    } = parameters;
    let factory = handle
        .query_factory(
            &request.code_generation,
            &request.vector_generation,
            request.projection.projection_key(),
        )
        .or_else(|| match source_coherence {
            SemanticSourceCoherenceV1::ExactGeneration => None,
            // The caller proved the served vectors carry the current source
            // content; the runtime pointer may still name the prior
            // publication (or a sibling projection of the same corpus). The
            // embedder's physical identity is the projection key alone.
            SemanticSourceCoherenceV1::ProvenSourceContent => {
                handle.query_factory_for_projection(request.projection.projection_key())
            }
        });
    match factory {
        Some(factory) => {
            let embedder = factory.create(control, request.budget.deadline_micros);
            let lane = SemanticCodeRetriever::new(&embedder, vectors, control);
            execute_calibrated_semantic_query(
                &lane,
                SemanticLaneReadinessV1::Ready {
                    request,
                    generation,
                    calibration,
                },
                mode,
                fallback,
            )
        }
        None => execute_calibrated_semantic_query(
            &NeverCalledSemanticLane,
            SemanticLaneReadinessV1::Unavailable(index_state_from_status(handle.status())),
            mode,
            fallback,
        ),
    }
}

/// Project-scoped application search consumer over the retained production
/// runtime and committed configuration-selected vector generation.
#[hotpath::measure(label = "usecases.semantic.project_search", future = true)]
pub async fn compose_project_application_semantic_search<C>(
    project_root: &Path,
    code_generation: &CodeIndexPublishedGenerationV1,
    request: &SemanticRetrievalRequestV1<'_>,
    calibration: Option<&SemanticCalibrationProfileV1>,
    control: &C,
    mode: SemanticQueryModeV1,
    fallback: Arc<QueryFallbackSubpayload>,
) -> Result<SemanticQueryServiceOutcomeV1, SemanticQueryServiceError>
where
    C: RetrievalExecutionControl + Sync,
{
    let Some(runtime) = project_semantic_production_runtime(project_root) else {
        return execute_calibrated_semantic_query(
            &NeverCalledSemanticLane,
            SemanticLaneReadinessV1::Unavailable(SemanticIndexStateV1::Unavailable),
            mode,
            fallback,
        );
    };
    runtime
        .execute_search(
            code_generation,
            request,
            calibration,
            control,
            mode,
            fallback,
        )
        .await
}

/// Production execution bridge for callers that already own authenticated
/// semantic request material and an authenticated frozen query composition.
///
/// MCP cannot use this bridge until its query-MAC and query composition
/// authorities are mounted; accepting the typed request here prevents that
/// boundary from inventing either input.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProductionProjectSemanticSearchBridgeV1;

/// Authenticated project-scoped inputs for production semantic search.
pub struct AuthorizedProjectSemanticSearchParametersV1<'a, C> {
    pub project_root: &'a Path,
    pub code_generation: &'a CodeIndexPublishedGenerationV1,
    pub request: &'a SemanticRetrievalRequestV1<'a>,
    pub calibration: Option<&'a SemanticCalibrationProfileV1>,
    pub control: &'a C,
    pub mode: SemanticQueryModeV1,
    pub authorized_query: &'a AuthorizedQueryFallbackV1,
}

impl ProductionProjectSemanticSearchBridgeV1 {
    pub fn execute<'a, C>(
        &'a self,
        parameters: AuthorizedProjectSemanticSearchParametersV1<'a, C>,
    ) -> SemanticRuntimeFuture<'a, Result<SemanticQueryServiceOutcomeV1, SemanticQueryServiceError>>
    where
        C: RetrievalExecutionControl + Sync + 'a,
    {
        let AuthorizedProjectSemanticSearchParametersV1 {
            project_root,
            code_generation,
            request,
            calibration,
            control,
            mode,
            authorized_query,
        } = parameters;
        if request.query_digest != authorized_query.query_digest {
            return Box::pin(async { Err(SemanticQueryServiceError::InvalidFallback) });
        }
        Box::pin(compose_project_application_semantic_search(
            project_root,
            code_generation,
            request,
            calibration,
            control,
            mode,
            Arc::clone(&authorized_query.fallback),
        ))
    }
}

pub(super) struct NeverCalledSemanticLane;

impl SemanticLaneRetriever for NeverCalledSemanticLane {
    fn retrieve_semantic(
        &self,
        _request: &SemanticRetrievalRequestV1<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<CodeSemanticEvidenceV1>>, RetrievalPortError> {
        Err(RetrievalPortError::Contract(
            "non-ready semantic lane must never be invoked".to_owned(),
        ))
    }
}
