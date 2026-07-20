//! Storage-neutral V1 logical import boundary (Plan 25, "V1 migration").
//!
//! The store-owned reader and sanitizer produce [`V1SanitizedCodeBatchV1`].
//! This module verifies that logical envelope, then delegates deterministic
//! generation construction through [`V1GenerationRebuilder`]. It owns no
//! legacy storage adapter or generation-planning implementation.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CodeGenerationManifestV1, ComponentVersion, FileOccurrenceId, ManifestDigest,
    SanitizedCodeSnapshotV1, ValidatedCodeSnapshotV1, canonical_sha256,
};

use super::capabilities::expected_seal_digest;
use super::chunks::content_digest;
use super::extract::ExtractionCancellation;
use super::intake::INTAKE_DIGEST_SEPARATOR;

/// Domain separator for one complete V1 logical import batch.
pub const V1_CODE_BATCH_DIGEST_SEPARATOR: &str = "tracedecay.v1-code-import-batch.v1";

/// V1 source and adapter identity retained across generation reconstruction.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct V1MigrationProvenanceV1 {
    pub source_generation: String,
    pub source_schema_revision: ComponentVersion,
    pub importer_revision: ComponentVersion,
}

/// Declared logical row and byte counts for a V1 import batch.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct V1CodeBatchCountsV1 {
    pub total_rows: u64,
    pub supported_rows: u64,
    pub unsupported_rows: u64,
    pub sanitized_bytes: u64,
}

/// Sanitized payload carried by one logical V1 row.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "row_kind", rename_all = "snake_case")]
pub enum V1SanitizedCodeRowPayloadV1 {
    /// Source admitted by capture and the sanitizer for deterministic V2
    /// extraction.
    Supported { sanitized_bytes: Vec<u8> },
    /// An explicit non-source disposition. The reason is evidence and the
    /// row remains part of counts and the batch digest.
    Unsupported { reason: String },
}

/// One logical, sanitized row emitted by the store-owned V1 reader.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct V1SanitizedCodeRowV1 {
    pub source_row_id: String,
    pub file_occurrence_id: FileOccurrenceId,
    pub payload: V1SanitizedCodeRowPayloadV1,
}

/// Complete logical input to the V1 reconstruction boundary.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct V1SanitizedCodeBatchV1 {
    pub provenance: V1MigrationProvenanceV1,
    pub snapshot: SanitizedCodeSnapshotV1,
    pub rows: Vec<V1SanitizedCodeRowV1>,
    pub expected_counts: V1CodeBatchCountsV1,
    pub expected_digest: ManifestDigest,
}

/// A batch whose source identity, sanitization evidence, row set, counts, and
/// digests were verified. Only this type crosses into generation rebuilding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedV1SanitizedCodeBatchV1 {
    provenance: V1MigrationProvenanceV1,
    snapshot: ValidatedCodeSnapshotV1,
    rows: Vec<V1SanitizedCodeRowV1>,
    counts: V1CodeBatchCountsV1,
    batch_digest: ManifestDigest,
}

impl VerifiedV1SanitizedCodeBatchV1 {
    pub fn provenance(&self) -> &V1MigrationProvenanceV1 {
        &self.provenance
    }

    pub fn snapshot(&self) -> &ValidatedCodeSnapshotV1 {
        &self.snapshot
    }

    pub fn rows(&self) -> &[V1SanitizedCodeRowV1] {
        &self.rows
    }

    pub const fn counts(&self) -> V1CodeBatchCountsV1 {
        self.counts
    }

    pub fn batch_digest(&self) -> &ManifestDigest {
        &self.batch_digest
    }
}

/// Generation-construction port. The implementation belongs to the
/// generation composition lane; this boundary supplies only verified values.
pub trait V1GenerationRebuilder {
    fn rebuild_generation(
        &self,
        batch: &VerifiedV1SanitizedCodeBatchV1,
    ) -> Result<CodeGenerationManifestV1, V1CodeImportErrorV1>;
}

/// Logical import and reconstruction failures.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum V1CodeImportErrorV1 {
    #[error("V1 code import was cancelled")]
    Cancelled,
    #[error("V1 import provenance is invalid: {0}")]
    InvalidProvenance(String),
    #[error("V1 sanitized snapshot is invalid: {0}")]
    InvalidSnapshot(String),
    #[error("V1 logical row counts do not match: expected {expected:?}, actual {actual:?}")]
    CountMismatch {
        expected: V1CodeBatchCountsV1,
        actual: V1CodeBatchCountsV1,
    },
    #[error("V1 logical batch digest does not match")]
    DigestMismatch,
    #[error("V1 source row {0} appears more than once")]
    DuplicateSourceRow(String),
    #[error("V1 file occurrence {0} appears more than once")]
    DuplicateFileOccurrence(FileOccurrenceId),
    #[error("V1 row names file occurrence {0} outside the sanitized snapshot")]
    UnknownFileOccurrence(FileOccurrenceId),
    #[error("sanitized snapshot file occurrence {0} has no V1 logical row")]
    MissingFileOccurrence(FileOccurrenceId),
    #[error("V1 row {source_row_id} is invalid: {detail}")]
    InvalidRow {
        source_row_id: String,
        detail: String,
    },
    #[error("generation reconstruction failed: {0}")]
    RebuildFailed(String),
    #[error("generation reconstruction returned an invalid manifest: {0}")]
    InvalidGeneration(String),
}

/// Validate logical V1 input and delegate deterministic generation
/// reconstruction. Cancellation is checked before, during, and after
/// validation and around the pure reconstruction call.
pub struct V1CodeBatchConsumer<R, C> {
    rebuilder: R,
    cancellation: C,
}

impl<R, C> V1CodeBatchConsumer<R, C>
where
    R: V1GenerationRebuilder,
    C: ExtractionCancellation,
{
    pub const fn new(rebuilder: R, cancellation: C) -> Self {
        Self {
            rebuilder,
            cancellation,
        }
    }

    pub fn rebuild(
        &self,
        batch: V1SanitizedCodeBatchV1,
    ) -> Result<CodeGenerationManifestV1, V1CodeImportErrorV1> {
        check_cancelled(&self.cancellation)?;
        let verified = verify_batch(batch, &self.cancellation)?;
        check_cancelled(&self.cancellation)?;

        let generation = self.rebuilder.rebuild_generation(&verified)?;
        check_cancelled(&self.cancellation)?;
        verify_generation(&generation, &verified)?;
        Ok(generation)
    }
}

/// Recompute the canonical digest of a V1 logical batch. Input row order is
/// not authoritative: rows are sorted by source-row and file-occurrence
/// identity before hashing.
pub fn expected_v1_batch_digest(
    batch: &V1SanitizedCodeBatchV1,
) -> Result<ManifestDigest, V1CodeImportErrorV1> {
    let mut rows = batch.rows.clone();
    rows.sort_by(|left, right| {
        (&left.source_row_id, &left.file_occurrence_id)
            .cmp(&(&right.source_row_id, &right.file_occurrence_id))
    });
    canonical_sha256(&(
        V1_CODE_BATCH_DIGEST_SEPARATOR,
        &batch.provenance,
        &batch.snapshot,
        &rows,
        batch.expected_counts,
    ))
    .map_err(|error| V1CodeImportErrorV1::InvalidSnapshot(error.to_string()))
}

fn verify_batch(
    batch: V1SanitizedCodeBatchV1,
    cancellation: &dyn ExtractionCancellation,
) -> Result<VerifiedV1SanitizedCodeBatchV1, V1CodeImportErrorV1> {
    validate_provenance(&batch.provenance)?;
    batch
        .snapshot
        .validate()
        .map_err(|error| V1CodeImportErrorV1::InvalidSnapshot(error.to_string()))?;

    let actual_counts = actual_counts(&batch.rows)?;
    if batch.expected_counts != actual_counts {
        return Err(V1CodeImportErrorV1::CountMismatch {
            expected: batch.expected_counts,
            actual: actual_counts,
        });
    }
    if batch.expected_digest != expected_v1_batch_digest(&batch)? {
        return Err(V1CodeImportErrorV1::DigestMismatch);
    }

    let mut source_rows = BTreeSet::new();
    let mut file_occurrences = BTreeSet::new();
    for row in &batch.rows {
        check_cancelled(cancellation)?;
        validate_text(&row.source_row_id, 512).map_err(|detail| {
            V1CodeImportErrorV1::InvalidRow {
                source_row_id: row.source_row_id.clone(),
                detail,
            }
        })?;
        if !source_rows.insert(row.source_row_id.clone()) {
            return Err(V1CodeImportErrorV1::DuplicateSourceRow(
                row.source_row_id.clone(),
            ));
        }
        if !file_occurrences.insert(row.file_occurrence_id.clone()) {
            return Err(V1CodeImportErrorV1::DuplicateFileOccurrence(
                row.file_occurrence_id.clone(),
            ));
        }
        let file = batch
            .snapshot
            .files
            .iter()
            .find(|file| file.file_occurrence_id == row.file_occurrence_id)
            .ok_or_else(|| {
                V1CodeImportErrorV1::UnknownFileOccurrence(row.file_occurrence_id.clone())
            })?;
        validate_row(row, file)?;
    }
    if let Some(missing) = batch
        .snapshot
        .files
        .iter()
        .find(|file| !file_occurrences.contains(&file.file_occurrence_id))
    {
        return Err(V1CodeImportErrorV1::MissingFileOccurrence(
            missing.file_occurrence_id.clone(),
        ));
    }

    let mut rows = batch.rows;
    rows.sort_by(|left, right| {
        (&left.source_row_id, &left.file_occurrence_id)
            .cmp(&(&right.source_row_id, &right.file_occurrence_id))
    });
    let intake_digest = canonical_sha256(&(INTAKE_DIGEST_SEPARATOR, &batch.snapshot))
        .map_err(|error| V1CodeImportErrorV1::InvalidSnapshot(error.to_string()))?;
    Ok(VerifiedV1SanitizedCodeBatchV1 {
        provenance: batch.provenance,
        snapshot: ValidatedCodeSnapshotV1 {
            validated_at: batch.snapshot.captured_at,
            snapshot: batch.snapshot,
            intake_digest,
        },
        rows,
        counts: actual_counts,
        batch_digest: batch.expected_digest,
    })
}

fn validate_provenance(provenance: &V1MigrationProvenanceV1) -> Result<(), V1CodeImportErrorV1> {
    validate_text(&provenance.source_generation, 512)
        .map_err(V1CodeImportErrorV1::InvalidProvenance)?;
    provenance
        .source_schema_revision
        .validate()
        .map_err(|error| V1CodeImportErrorV1::InvalidProvenance(error.to_string()))?;
    provenance
        .importer_revision
        .validate()
        .map_err(|error| V1CodeImportErrorV1::InvalidProvenance(error.to_string()))
}

fn actual_counts(
    rows: &[V1SanitizedCodeRowV1],
) -> Result<V1CodeBatchCountsV1, V1CodeImportErrorV1> {
    let mut counts = V1CodeBatchCountsV1 {
        total_rows: u64::try_from(rows.len()).map_err(|error| {
            V1CodeImportErrorV1::InvalidSnapshot(format!("row count overflow: {error}"))
        })?,
        supported_rows: 0,
        unsupported_rows: 0,
        sanitized_bytes: 0,
    };
    for row in rows {
        match &row.payload {
            V1SanitizedCodeRowPayloadV1::Supported { sanitized_bytes } => {
                counts.supported_rows = counts.supported_rows.checked_add(1).ok_or_else(|| {
                    V1CodeImportErrorV1::InvalidSnapshot("supported row count overflow".to_owned())
                })?;
                let bytes = u64::try_from(sanitized_bytes.len()).map_err(|error| {
                    V1CodeImportErrorV1::InvalidSnapshot(format!(
                        "sanitized byte count overflow: {error}"
                    ))
                })?;
                counts.sanitized_bytes =
                    counts.sanitized_bytes.checked_add(bytes).ok_or_else(|| {
                        V1CodeImportErrorV1::InvalidSnapshot(
                            "sanitized byte count overflow".to_owned(),
                        )
                    })?;
            }
            V1SanitizedCodeRowPayloadV1::Unsupported { .. } => {
                counts.unsupported_rows =
                    counts.unsupported_rows.checked_add(1).ok_or_else(|| {
                        V1CodeImportErrorV1::InvalidSnapshot(
                            "unsupported row count overflow".to_owned(),
                        )
                    })?;
            }
        }
    }
    Ok(counts)
}

fn validate_row(
    row: &V1SanitizedCodeRowV1,
    file: &tracedecay_domain::SanitizedCodeFileV1,
) -> Result<(), V1CodeImportErrorV1> {
    let invalid = |detail: &str| V1CodeImportErrorV1::InvalidRow {
        source_row_id: row.source_row_id.clone(),
        detail: detail.to_owned(),
    };
    match &row.payload {
        V1SanitizedCodeRowPayloadV1::Supported { sanitized_bytes } => {
            if file.disposition != tracedecay_domain::SnapshotFileDispositionV1::Present {
                return Err(invalid(
                    "supported payload requires a present sanitized snapshot file",
                ));
            }
            std::str::from_utf8(sanitized_bytes)
                .map_err(|_| invalid("supported payload is not sanitized UTF-8"))?;
            if content_digest(sanitized_bytes) != file.content_digest {
                return Err(invalid(
                    "supported payload digest does not match the sanitized snapshot",
                ));
            }
        }
        V1SanitizedCodeRowPayloadV1::Unsupported { reason } => {
            if file.disposition == tracedecay_domain::SnapshotFileDispositionV1::Present {
                return Err(invalid(
                    "unsupported payload cannot name a present sanitized snapshot file",
                ));
            }
            validate_text(reason, 4_096).map_err(|detail| invalid(&detail))?;
        }
    }
    Ok(())
}

fn verify_generation(
    generation: &CodeGenerationManifestV1,
    batch: &VerifiedV1SanitizedCodeBatchV1,
) -> Result<(), V1CodeImportErrorV1> {
    generation
        .validate()
        .map_err(|error| V1CodeImportErrorV1::InvalidGeneration(error.to_string()))?;
    if generation.snapshot_digest != batch.snapshot.intake_digest
        || generation.sanitizer_revision != batch.snapshot.snapshot.sanitizer_revision
    {
        return Err(V1CodeImportErrorV1::InvalidGeneration(
            "manifest does not bind the verified sanitized snapshot".to_owned(),
        ));
    }
    let expected = expected_seal_digest(generation)
        .map_err(|error| V1CodeImportErrorV1::InvalidGeneration(error.to_string()))?;
    if generation.seal.expected_digest != expected {
        return Err(V1CodeImportErrorV1::InvalidGeneration(
            "manifest seal does not recompute".to_owned(),
        ));
    }
    Ok(())
}

fn check_cancelled(cancellation: &dyn ExtractionCancellation) -> Result<(), V1CodeImportErrorV1> {
    if cancellation.is_cancelled() {
        Err(V1CodeImportErrorV1::Cancelled)
    } else {
        Ok(())
    }
}

fn validate_text(value: &str, max_len: usize) -> Result<(), String> {
    if value.is_empty() {
        return Err("value is empty".to_owned());
    }
    if value.len() > max_len {
        return Err(format!("value exceeds {max_len} bytes"));
    }
    if value.trim() != value || value.chars().any(char::is_control) {
        return Err("value is not canonical text".to_owned());
    }
    Ok(())
}
