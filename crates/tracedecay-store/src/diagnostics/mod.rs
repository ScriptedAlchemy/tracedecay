use std::error::Error;

use tracedecay_domain::{CodeGenerationId, DomainError, GenerationDiagnosticV1, RetrievalAnchorId};

pub mod codec;
mod ports;

pub use codec::{
    DIAGNOSTIC_STATE_CLEARED, DIAGNOSTIC_STATE_CURRENT, DiagnosticRecordStateKindV1,
    diagnostic_evidence_class_name, diagnostic_producer_kind_name, diagnostic_severity_name,
    diagnostic_state_columns, parse_diagnostic_evidence_class, parse_diagnostic_producer_kind,
    parse_diagnostic_severity,
};
pub use ports::DiagnosticStore;

/// A complete durable diagnostic snapshot admitted from the normal sanitized
/// clean-generation pipeline.
///
/// Dirty editor overlays are intentionally unrepresentable at this boundary:
/// callers can persist only validated current records for one exact immutable
/// generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SanitizedCleanDiagnosticSnapshotV1 {
    generation_id: CodeGenerationId,
    records: Vec<GenerationDiagnosticV1>,
}

impl SanitizedCleanDiagnosticSnapshotV1 {
    pub fn new(
        generation_id: CodeGenerationId,
        mut records: Vec<GenerationDiagnosticV1>,
    ) -> DiagnosticStoreResult<Self> {
        generation_id
            .validate()
            .map_err(DiagnosticStoreError::Contract)?;
        for record in &records {
            record.validate().map_err(DiagnosticStoreError::Contract)?;
            if record.generation_id != generation_id {
                return Err(DiagnosticStoreError::GenerationMismatch {
                    expected: generation_id,
                    actual: record.generation_id.clone(),
                    anchor: record.diagnostic_anchor.clone(),
                });
            }
            if !record.is_current() {
                return Err(DiagnosticStoreError::NonCurrentRecord {
                    anchor: record.diagnostic_anchor.clone(),
                });
            }
        }
        records.sort_by(|left, right| {
            left.diagnostic_anchor
                .as_str()
                .cmp(right.diagnostic_anchor.as_str())
        });
        if let Some(duplicate) = records
            .windows(2)
            .find(|pair| pair[0].diagnostic_anchor == pair[1].diagnostic_anchor)
        {
            return Err(DiagnosticStoreError::DuplicateAnchor {
                anchor: duplicate[0].diagnostic_anchor.clone(),
            });
        }
        Ok(Self {
            generation_id,
            records,
        })
    }

    pub fn generation_id(&self) -> &CodeGenerationId {
        &self.generation_id
    }

    pub fn records(&self) -> &[GenerationDiagnosticV1] {
        &self.records
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DiagnosticStoreError {
    #[error(
        "diagnostic {anchor} names generation {actual}, but the clean snapshot targets {expected}"
    )]
    GenerationMismatch {
        expected: CodeGenerationId,
        actual: CodeGenerationId,
        anchor: RetrievalAnchorId,
    },
    #[error("diagnostic {anchor} is stale and cannot enter a clean snapshot")]
    NonCurrentRecord { anchor: RetrievalAnchorId },
    #[error("diagnostic anchor {anchor} occurs more than once in a clean snapshot")]
    DuplicateAnchor { anchor: RetrievalAnchorId },
    #[error("diagnostic contract validation failed")]
    Contract(#[source] DomainError),
    #[error("diagnostic storage operation {operation} failed")]
    Storage {
        operation: &'static str,
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
}

pub type DiagnosticStoreResult<T> = Result<T, DiagnosticStoreError>;

/// Exact diagnostic observation equality for an ordered snapshot, excluding
/// only the server ingestion clock carried by each record.
pub fn diagnostic_snapshot_observation_eq(
    stored: &[GenerationDiagnosticV1],
    incoming: &[GenerationDiagnosticV1],
) -> bool {
    stored.len() == incoming.len()
        && stored
            .iter()
            .zip(incoming)
            .all(|(left, right)| left.same_observation_as(right))
}
