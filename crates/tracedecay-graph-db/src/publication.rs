use std::fmt;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::{
    GraphCancellation, GraphDbError, GraphIdempotencyKey, GraphNamespace, GraphWatermark,
    GraphWriteBatch, SourceGeneration,
};

#[derive(Clone)]
pub struct GraphPublication {
    pub namespace: GraphNamespace,
    pub idempotency_key: GraphIdempotencyKey,
    pub source_generation: SourceGeneration,
    pub expected_watermark: Option<GraphWatermark>,
    pub next_watermark: GraphWatermark,
    pub batch: GraphWriteBatch,
    pub cancellation: Arc<dyn GraphCancellation>,
}

impl fmt::Debug for GraphPublication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GraphPublication")
            .field("namespace", &self.namespace)
            .field("idempotency_key", &self.idempotency_key)
            .field("source_generation", &self.source_generation)
            .field("expected_watermark", &self.expected_watermark)
            .field("next_watermark", &self.next_watermark)
            .field("batch", &self.batch)
            .finish_non_exhaustive()
    }
}

impl GraphPublication {
    pub(crate) fn validate_and_digest(&mut self) -> Result<String, GraphDbError> {
        if self.cancellation.is_cancelled() || self.batch.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        if self.namespace != self.batch.namespace
            || self.source_generation != self.batch.source_generation
            || self.next_watermark != self.batch.next_watermark
        {
            return Err(GraphDbError::invalid(
                "publication identity must match its graph batch",
            ));
        }
        let batch_digest = self.batch.validate_and_digest()?;
        let canonical = serde_json::to_vec(&(
            &self.namespace,
            &self.idempotency_key,
            &self.source_generation,
            &self.expected_watermark,
            &self.next_watermark,
            batch_digest,
        ))
        .map_err(|error| {
            GraphDbError::invalid(format!("failed to canonicalize publication: {error}"))
        })?;
        Ok(hex::encode(Sha256::digest(canonical)))
    }
}
