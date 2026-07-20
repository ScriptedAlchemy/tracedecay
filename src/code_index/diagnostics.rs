//! Generation/content-exact attachment of Plan 35 diagnostic records.
//!
//! Plan 35 owns diagnostic identity, producer provenance, persistence, and
//! clearing/supersession semantics. This module neither stores nor translates
//! diagnostics. It validates one clean-generation watermark and emits only
//! exact current attachments; stale, cleared, superseded, incomplete, and
//! unsupported evidence remains explicitly typed.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CodeGenerationId, CodeGenerationManifestV1, ContentDigest, DiagnosticEvidenceClassV1,
    DiagnosticRecordStateV1, FileOccurrenceId, GenerationDiagnosticAttachmentV1,
    GenerationDiagnosticV1, ManifestDigest, RetrievalAnchorId, UtcMicros, ValidatedCodeSnapshotV1,
};

use super::capabilities::expected_seal_digest;

/// Completeness reported by the Plan-35 producer/read boundary.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "coverage", rename_all = "snake_case")]
pub enum DiagnosticJoinInputCoverageV1 {
    Complete,
    Partial { reason: String },
}

/// Plan-35 diagnostic observation watermark, kept distinct from the code
/// generation watermark.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticEvidenceWatermarkV1 {
    pub generation_id: CodeGenerationId,
    pub snapshot_digest: ManifestDigest,
    pub content_identity: ContentDigest,
    pub observed_through: UtcMicros,
    pub coverage: DiagnosticJoinInputCoverageV1,
}

/// Why a diagnostic join cannot claim a complete current set.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GenerationDiagnosticPartialReasonV1 {
    InputPartial { reason: String },
    StaleGeneration { anchor: RetrievalAnchorId },
    StaleScope { anchor: RetrievalAnchorId },
    StaleContent { anchor: RetrievalAnchorId },
    MissingFile { anchor: RetrievalAnchorId },
    BeyondWatermark { anchor: RetrievalAnchorId },
    UnknownUnsupported { anchor: RetrievalAnchorId },
}

/// Overall diagnostic join coverage.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "coverage", rename_all = "snake_case")]
pub enum GenerationDiagnosticJoinCoverageV1 {
    Complete,
    Partial {
        reasons: Vec<GenerationDiagnosticPartialReasonV1>,
    },
}

/// Typed disposition of one Plan-35 diagnostic relative to the requested
/// clean code generation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub enum GenerationDiagnosticDispositionV1 {
    Current {
        attachment: GenerationDiagnosticAttachmentV1,
    },
    Superseded {
        successor_generation: CodeGenerationId,
    },
    Cleared {
        cleared_in_generation: CodeGenerationId,
    },
    StaleGeneration {
        record_generation: CodeGenerationId,
    },
    StaleScope,
    StaleContent {
        expected: ContentDigest,
        observed: ContentDigest,
    },
    MissingFile {
        file_occurrence_id: FileOccurrenceId,
    },
    BeyondWatermark {
        collected_at: UtcMicros,
        observed_through: UtcMicros,
    },
    UnknownUnsupported,
}

/// One diagnostic record plus its exact join disposition. The owning
/// Plan-35 record is preserved instead of copied into a parallel schema.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationDiagnosticJoinRecordV1 {
    pub record: GenerationDiagnosticV1,
    pub disposition: GenerationDiagnosticDispositionV1,
}

/// Deterministic generation-aware diagnostic join.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationDiagnosticJoinV1 {
    pub generation_id: CodeGenerationId,
    pub code_snapshot_digest: ManifestDigest,
    pub code_content_identity: ContentDigest,
    pub diagnostic_watermark: DiagnosticEvidenceWatermarkV1,
    pub records: Vec<GenerationDiagnosticJoinRecordV1>,
    pub coverage: GenerationDiagnosticJoinCoverageV1,
}

/// Failures of the join contract itself. Individual stale/historical records
/// are successful typed evidence and do not use this error.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum GenerationDiagnosticJoinErrorV1 {
    #[error("the code generation does not seal the supplied sanitized snapshot")]
    StaleGenerationWatermark,
    #[error("the diagnostic observation watermark is stale")]
    StaleDiagnosticWatermark,
    #[error("duplicate diagnostic anchor {0}")]
    DuplicateDiagnostic(RetrievalAnchorId),
    #[error("invalid generation or diagnostic evidence: {0}")]
    Contract(String),
}

impl GenerationDiagnosticJoinV1 {
    /// Join Plan-35 records to one exact clean generation.
    pub fn join(
        generation: &CodeGenerationManifestV1,
        snapshot: &ValidatedCodeSnapshotV1,
        diagnostics: &[GenerationDiagnosticV1],
        watermark: &DiagnosticEvidenceWatermarkV1,
    ) -> Result<Self, GenerationDiagnosticJoinErrorV1> {
        validate_generation_snapshot(generation, snapshot)?;
        validate_watermark(generation, snapshot, watermark)?;

        let files: BTreeMap<&FileOccurrenceId, &ContentDigest> = snapshot
            .snapshot
            .files
            .iter()
            .map(|file| (&file.file_occurrence_id, &file.content_digest))
            .collect();
        let mut diagnostics = diagnostics.to_vec();
        diagnostics.sort_by(|left, right| left.diagnostic_anchor.cmp(&right.diagnostic_anchor));
        if let Some(duplicate) = diagnostics
            .windows(2)
            .find(|pair| pair[0].diagnostic_anchor == pair[1].diagnostic_anchor)
        {
            return Err(GenerationDiagnosticJoinErrorV1::DuplicateDiagnostic(
                duplicate[0].diagnostic_anchor.clone(),
            ));
        }

        let mut partial_reasons = match &watermark.coverage {
            DiagnosticJoinInputCoverageV1::Complete => Vec::new(),
            DiagnosticJoinInputCoverageV1::Partial { reason } => {
                vec![GenerationDiagnosticPartialReasonV1::InputPartial {
                    reason: reason.clone(),
                }]
            }
        };
        let mut records = Vec::with_capacity(diagnostics.len());
        for record in diagnostics {
            record
                .validate()
                .map_err(|error| GenerationDiagnosticJoinErrorV1::Contract(error.to_string()))?;
            let disposition = disposition_for(
                generation,
                snapshot,
                watermark,
                &files,
                &record,
                &mut partial_reasons,
            );
            records.push(GenerationDiagnosticJoinRecordV1 {
                record,
                disposition,
            });
        }

        partial_reasons.sort();
        partial_reasons.dedup();
        let coverage = if partial_reasons.is_empty() {
            GenerationDiagnosticJoinCoverageV1::Complete
        } else {
            GenerationDiagnosticJoinCoverageV1::Partial {
                reasons: partial_reasons,
            }
        };
        Ok(Self {
            generation_id: generation.generation_id.clone(),
            code_snapshot_digest: generation.snapshot_digest.clone(),
            code_content_identity: snapshot.snapshot.content_identity.clone(),
            diagnostic_watermark: watermark.clone(),
            records,
            coverage,
        })
    }
}

fn disposition_for(
    generation: &CodeGenerationManifestV1,
    snapshot: &ValidatedCodeSnapshotV1,
    watermark: &DiagnosticEvidenceWatermarkV1,
    files: &BTreeMap<&FileOccurrenceId, &ContentDigest>,
    record: &GenerationDiagnosticV1,
    partial_reasons: &mut Vec<GenerationDiagnosticPartialReasonV1>,
) -> GenerationDiagnosticDispositionV1 {
    if record.repository != snapshot.snapshot.repository
        || record.worktree != snapshot.snapshot.worktree
        || record.reference != snapshot.snapshot.reference
        || record.source_revision != snapshot.snapshot.source_revision
    {
        partial_reasons.push(GenerationDiagnosticPartialReasonV1::StaleScope {
            anchor: record.diagnostic_anchor.clone(),
        });
        return GenerationDiagnosticDispositionV1::StaleScope;
    }
    match &record.state {
        DiagnosticRecordStateV1::Superseded {
            successor_generation,
        } => {
            return GenerationDiagnosticDispositionV1::Superseded {
                successor_generation: successor_generation.clone(),
            };
        }
        DiagnosticRecordStateV1::Cleared {
            cleared_in_generation,
        } => {
            return GenerationDiagnosticDispositionV1::Cleared {
                cleared_in_generation: cleared_in_generation.clone(),
            };
        }
        DiagnosticRecordStateV1::Current => {}
    }
    if record.evidence_class == DiagnosticEvidenceClassV1::UnknownUnsupported {
        partial_reasons.push(GenerationDiagnosticPartialReasonV1::UnknownUnsupported {
            anchor: record.diagnostic_anchor.clone(),
        });
        return GenerationDiagnosticDispositionV1::UnknownUnsupported;
    }
    if record.collected_at.0 > watermark.observed_through.0 {
        partial_reasons.push(GenerationDiagnosticPartialReasonV1::BeyondWatermark {
            anchor: record.diagnostic_anchor.clone(),
        });
        return GenerationDiagnosticDispositionV1::BeyondWatermark {
            collected_at: record.collected_at,
            observed_through: watermark.observed_through,
        };
    }
    if record.generation_id != generation.generation_id {
        partial_reasons.push(GenerationDiagnosticPartialReasonV1::StaleGeneration {
            anchor: record.diagnostic_anchor.clone(),
        });
        return GenerationDiagnosticDispositionV1::StaleGeneration {
            record_generation: record.generation_id.clone(),
        };
    }
    let Some(expected_content) = files.get(&record.file_occurrence_id).copied() else {
        partial_reasons.push(GenerationDiagnosticPartialReasonV1::MissingFile {
            anchor: record.diagnostic_anchor.clone(),
        });
        return GenerationDiagnosticDispositionV1::MissingFile {
            file_occurrence_id: record.file_occurrence_id.clone(),
        };
    };
    if expected_content != &record.content_digest {
        partial_reasons.push(GenerationDiagnosticPartialReasonV1::StaleContent {
            anchor: record.diagnostic_anchor.clone(),
        });
        return GenerationDiagnosticDispositionV1::StaleContent {
            expected: expected_content.clone(),
            observed: record.content_digest.clone(),
        };
    }

    GenerationDiagnosticDispositionV1::Current {
        attachment: GenerationDiagnosticAttachmentV1 {
            generation_id: generation.generation_id.clone(),
            file_occurrence_id: record.file_occurrence_id.clone(),
            symbol_occurrence_id: record.symbol_occurrence_id.clone(),
            diagnostic_anchor: record.diagnostic_anchor.clone(),
            content_digest: record.content_digest.clone(),
        },
    }
}

fn validate_generation_snapshot(
    generation: &CodeGenerationManifestV1,
    snapshot: &ValidatedCodeSnapshotV1,
) -> Result<(), GenerationDiagnosticJoinErrorV1> {
    snapshot
        .snapshot
        .validate()
        .map_err(|error| GenerationDiagnosticJoinErrorV1::Contract(error.to_string()))?;
    if generation.snapshot_digest != snapshot.intake_digest {
        return Err(GenerationDiagnosticJoinErrorV1::StaleGenerationWatermark);
    }
    generation
        .validate()
        .map_err(|error| GenerationDiagnosticJoinErrorV1::Contract(error.to_string()))?;
    let seal = expected_seal_digest(generation)
        .map_err(|error| GenerationDiagnosticJoinErrorV1::Contract(error.to_string()))?;
    if seal != generation.seal.expected_digest {
        return Err(GenerationDiagnosticJoinErrorV1::StaleGenerationWatermark);
    }
    Ok(())
}

fn validate_watermark(
    generation: &CodeGenerationManifestV1,
    snapshot: &ValidatedCodeSnapshotV1,
    watermark: &DiagnosticEvidenceWatermarkV1,
) -> Result<(), GenerationDiagnosticJoinErrorV1> {
    watermark
        .generation_id
        .validate()
        .map_err(|error| GenerationDiagnosticJoinErrorV1::Contract(error.to_string()))?;
    watermark
        .snapshot_digest
        .validate()
        .map_err(|error| GenerationDiagnosticJoinErrorV1::Contract(error.to_string()))?;
    watermark
        .content_identity
        .validate()
        .map_err(|error| GenerationDiagnosticJoinErrorV1::Contract(error.to_string()))?;
    if watermark.generation_id != generation.generation_id
        || watermark.snapshot_digest != generation.snapshot_digest
        || watermark.content_identity != snapshot.snapshot.content_identity
    {
        return Err(GenerationDiagnosticJoinErrorV1::StaleDiagnosticWatermark);
    }
    Ok(())
}
