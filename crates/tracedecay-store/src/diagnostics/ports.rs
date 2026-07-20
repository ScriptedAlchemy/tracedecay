use std::future::Future;

use tracedecay_domain::{
    CodeGenerationId, FileOccurrenceId, GenerationDiagnosticV1, RetrievalAnchorId,
};

use super::{
    DiagnosticPublicationReceiptV1, DiagnosticStoreResult, SanitizedCleanDiagnosticSnapshotV1,
};

/// Authoritative persistence boundary for generation-bound clean diagnostics.
///
/// The write side accepts only [`SanitizedCleanDiagnosticSnapshotV1`], so live
/// analyzer sessions and dirty editor overlays cannot reach durable storage.
pub trait DiagnosticStore: Send + Sync {
    fn publish_clean_diagnostics(
        &self,
        snapshot: SanitizedCleanDiagnosticSnapshotV1,
    ) -> impl Future<Output = DiagnosticStoreResult<DiagnosticPublicationReceiptV1>> + Send;

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

    fn stale_diagnostics(
        &self,
        generation: &CodeGenerationId,
    ) -> impl Future<Output = DiagnosticStoreResult<Vec<GenerationDiagnosticV1>>> + Send;

    fn diagnostic_by_anchor(
        &self,
        anchor: &RetrievalAnchorId,
    ) -> impl Future<Output = DiagnosticStoreResult<Option<GenerationDiagnosticV1>>> + Send;

    fn diagnostic_supersession_chain(
        &self,
        anchor: &RetrievalAnchorId,
    ) -> impl Future<Output = DiagnosticStoreResult<Vec<GenerationDiagnosticV1>>> + Send;

    fn supersede_diagnostic_generation(
        &self,
        prior_generation: &CodeGenerationId,
        successor_generation: &CodeGenerationId,
    ) -> impl Future<Output = DiagnosticStoreResult<u64>> + Send;
}
