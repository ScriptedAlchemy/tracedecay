use std::future::Future;

use tracedecay_domain::{
    CodeGenerationId, FileOccurrenceId, GenerationDiagnosticV1, RetrievalAnchorId,
};

use super::DiagnosticStoreResult;

/// Authoritative read boundary for generation-bound clean diagnostics.
pub trait DiagnosticStore: Send + Sync {
    fn current_diagnostic_generation(
        &self,
    ) -> impl Future<Output = DiagnosticStoreResult<Option<CodeGenerationId>>> + Send;

    fn diagnostics_for_generation(
        &self,
        generation: &CodeGenerationId,
    ) -> impl Future<Output = DiagnosticStoreResult<Vec<GenerationDiagnosticV1>>> + Send;

    fn current_diagnostics(
        &self,
        generation: &CodeGenerationId,
    ) -> impl Future<Output = DiagnosticStoreResult<Vec<GenerationDiagnosticV1>>> + Send;

    fn current_diagnostics_for_file(
        &self,
        generation: &CodeGenerationId,
        file_occurrence_id: &FileOccurrenceId,
    ) -> impl Future<Output = DiagnosticStoreResult<Vec<GenerationDiagnosticV1>>> + Send;

    fn diagnostic_by_anchor(
        &self,
        anchor: &RetrievalAnchorId,
    ) -> impl Future<Output = DiagnosticStoreResult<Option<GenerationDiagnosticV1>>> + Send;
}
