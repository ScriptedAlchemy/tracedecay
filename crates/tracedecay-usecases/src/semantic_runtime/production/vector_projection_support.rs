use std::sync::Arc;

use tracedecay_domain::CodeSearchChunkV1;
use tracedecay_graph_db::GraphCancellation;
use tracedecay_semantic::SemanticRuntimeScheduleFailureV1;

use crate::semantic_runtime::RetainedSemanticVectorGraphV1;
use crate::store::vector_generations::{
    GraphVectorGenerationStoreV1, VectorGenerationBuildIdV1, VectorGenerationPublicationV1,
    VectorProjectionCheckpointV1,
};

#[derive(Default)]
pub(super) struct BatchCommitStateV1 {
    pub(super) build: Option<VectorGenerationBuildIdV1>,
    pub(super) store: Option<Arc<GraphVectorGenerationStoreV1>>,
    pub(super) checkpoint: Option<VectorProjectionCheckpointV1>,
    pub(super) published: Option<VectorGenerationPublicationV1>,
}

pub(super) fn graph_vector_store(
    retained: &RetainedSemanticVectorGraphV1,
) -> Result<
    (GraphVectorGenerationStoreV1, Arc<dyn GraphCancellation>),
    SemanticRuntimeScheduleFailureV1,
> {
    Ok((
        GraphVectorGenerationStoreV1::read_only(retained)
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?,
        Arc::clone(retained.cancellation()),
    ))
}

pub(super) fn projection_input_bytes(
    chunks: &[CodeSearchChunkV1],
) -> Result<u64, SemanticRuntimeScheduleFailureV1> {
    chunks.iter().try_fold(0_u64, |total, chunk| {
        let bytes = u64::try_from(chunk.sanitized_text.as_str().len())
            .map_err(|_| SemanticRuntimeScheduleFailureV1::Projection)?;
        total
            .checked_add(bytes)
            .ok_or(SemanticRuntimeScheduleFailureV1::Projection)
    })
}
