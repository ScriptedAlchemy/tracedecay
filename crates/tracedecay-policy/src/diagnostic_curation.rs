//! Pure diagnostic curation for the production LSP projection journey.

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    CodeGenerationId, CommitId, ContentDigest, DiagnosticRecordStateV1, FileOccurrenceId,
    GenerationDiagnosticV1,
};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCurationDecisionV1 {
    Admit,
    TargetFileMismatch,
    GenerationMismatch,
    ContentDigestMismatch,
    RecordNotCurrent,
    SourceRevisionDrift,
}

/// Curates one durable diagnostic against the exact current projection
/// identity. The caller still owns record lookup and LSP publication.
pub fn curate_diagnostic(
    record: &GenerationDiagnosticV1,
    target_file: &FileOccurrenceId,
    code_generation_id: &CodeGenerationId,
    document_content_digest: &ContentDigest,
    head_commit_id: &CommitId,
) -> DiagnosticCurationDecisionV1 {
    if target_file != &record.file_occurrence_id {
        DiagnosticCurationDecisionV1::TargetFileMismatch
    } else if record.generation_id != *code_generation_id {
        DiagnosticCurationDecisionV1::GenerationMismatch
    } else if record.content_digest != *document_content_digest {
        DiagnosticCurationDecisionV1::ContentDigestMismatch
    } else if !matches!(record.state, DiagnosticRecordStateV1::Current) {
        DiagnosticCurationDecisionV1::RecordNotCurrent
    } else if record
        .source_revision
        .as_ref()
        .is_some_and(|revision| revision != head_commit_id)
    {
        DiagnosticCurationDecisionV1::SourceRevisionDrift
    } else {
        DiagnosticCurationDecisionV1::Admit
    }
}
