use std::fmt;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::{
    GraphCancellation, GraphCommit, GraphDbError, GraphIdempotencyKey, GraphNamespace,
    GraphWatermark, GraphWriteBatch, SourceGeneration,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphPublicationDigest(String);

impl GraphPublicationDigest {
    pub(crate) fn from_persisted(value: String) -> Result<Self, GraphDbError> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(GraphDbError::Corrupt {
                message: "publication digest is not a canonical SHA-256 digest".to_owned(),
            });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphPublicationInputDigest(String);

impl GraphPublicationInputDigest {
    pub fn new(value: impl Into<String>) -> Result<Self, GraphDbError> {
        let value = value.into();
        let Some(hex) = value.strip_prefix("sha256:") else {
            return Err(GraphDbError::invalid(
                "publication input digest must use the sha256 scheme",
            ));
        };
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(GraphDbError::invalid(
                "publication input digest must contain 64 lowercase hexadecimal characters",
            ));
        }
        Ok(Self(value))
    }

    pub(crate) fn from_persisted(value: String) -> Result<Self, GraphDbError> {
        Self::new(value).map_err(|error| GraphDbError::Corrupt {
            message: format!("persisted publication input digest is invalid: {error}"),
        })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// Idempotency metadata retained inside the disposable graph index.
///
/// A receipt may speed a derived-projection replay, but it does not attest to
/// an external publication or replace the canonical projection authority.
pub struct GraphPublicationReceipt {
    pub digest: GraphPublicationDigest,
    pub input_digest: GraphPublicationInputDigest,
    pub commit: GraphCommit,
}

#[derive(Clone)]
/// A derived-index publication request.
///
/// The publication record is local replay metadata only. Its batch must
/// remain reproducible from canonical source data.
pub struct GraphPublication {
    pub namespace: GraphNamespace,
    pub idempotency_key: GraphIdempotencyKey,
    pub input_digest: GraphPublicationInputDigest,
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
            .field("input_digest", &self.input_digest)
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
            &self.input_digest.as_str(),
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
