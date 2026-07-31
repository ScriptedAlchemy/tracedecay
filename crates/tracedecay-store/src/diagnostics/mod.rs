use std::error::Error;

use tracedecay_domain::{CodeGenerationId, DomainError, GenerationDiagnosticV1, RetrievalAnchorId};

pub mod codec;
mod ports;

pub use codec::{
    DIAGNOSTIC_STATE_CLEARED, DIAGNOSTIC_STATE_CURRENT, DIAGNOSTIC_STATE_SUPERSEDED,
    DiagnosticRecordStateKindV1, diagnostic_evidence_class_name, diagnostic_producer_kind_name,
    diagnostic_severity_name, diagnostic_state_columns, parse_diagnostic_evidence_class,
    parse_diagnostic_producer_kind, parse_diagnostic_severity,
};
pub use ports::DiagnosticStore;

/// One admitted request to transition every current record of a prior
/// generation into the superseded state, back-pointing at its successor.
///
/// Supersession is a distinct lane from clean publication: publication clears
/// the records a newer clean generation replaced, while supersession records
/// that a specific prior generation was superseded by a specific successor and
/// keeps the logical finding chain walkable. Validating the pair here means a
/// storage engine cannot be handed a self-supersession.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticGenerationSupersessionV1 {
    prior_generation: CodeGenerationId,
    successor_generation: CodeGenerationId,
}

impl DiagnosticGenerationSupersessionV1 {
    pub fn new(
        prior_generation: CodeGenerationId,
        successor_generation: CodeGenerationId,
    ) -> DiagnosticStoreResult<Self> {
        let request = Self {
            prior_generation,
            successor_generation,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> DiagnosticStoreResult<()> {
        self.prior_generation
            .validate()
            .map_err(DiagnosticStoreError::Contract)?;
        self.successor_generation
            .validate()
            .map_err(DiagnosticStoreError::Contract)?;
        if self.prior_generation == self.successor_generation {
            return Err(DiagnosticStoreError::SelfSupersession {
                generation: self.prior_generation.clone(),
            });
        }
        Ok(())
    }

    pub fn prior_generation(&self) -> &CodeGenerationId {
        &self.prior_generation
    }

    pub fn successor_generation(&self) -> &CodeGenerationId {
        &self.successor_generation
    }
}

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

    pub fn into_parts(self) -> (CodeGenerationId, Vec<GenerationDiagnosticV1>) {
        (self.generation_id, self.records)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticPublicationDispositionV1 {
    Committed,
    ExactReplay,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticPublicationReceiptV1 {
    generation_id: CodeGenerationId,
    inserted_records: u64,
    cleared_records: u64,
    disposition: DiagnosticPublicationDispositionV1,
}

impl DiagnosticPublicationReceiptV1 {
    pub fn new(
        generation_id: CodeGenerationId,
        inserted_records: u64,
        cleared_records: u64,
        disposition: DiagnosticPublicationDispositionV1,
    ) -> Self {
        Self {
            generation_id,
            inserted_records,
            cleared_records,
            disposition,
        }
    }

    pub fn generation_id(&self) -> &CodeGenerationId {
        &self.generation_id
    }

    pub const fn inserted_records(&self) -> u64 {
        self.inserted_records
    }

    pub const fn cleared_records(&self) -> u64 {
        self.cleared_records
    }

    pub const fn disposition(&self) -> DiagnosticPublicationDispositionV1 {
        self.disposition
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
    #[error("diagnostic generation {generation} cannot supersede itself")]
    SelfSupersession { generation: CodeGenerationId },
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
