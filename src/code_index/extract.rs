//! Language extractor port (Plan 25 phase 3): `languages.rs` owns the
//! descriptors; this module owns the extractor contract
//! `LanguageExtractor::extract(&ValidatedCodeFileV1, &LanguageDescriptorV1,
//! &CancellationToken) -> Result<ExtractionBatchV1, ExtractionFailureV1>`.
//!
//! Extraction acquires one tree-sitter parser from the descriptor's pinned
//! grammar; the in-process `ast-grep-core` structural kernel shares that
//! pinned grammar and source generation. Parse errors and unsupported
//! constructs are preserved as evidence; extraction never invents successful
//! structure.

use tracedecay_domain::{
    ExtractionBatchV1, ExtractionFailureV1, LanguageDescriptorV1, ValidatedCodeFileV1,
};

/// Cancellation checkpoint for extraction (the code-index-local spelling of
/// the Plan 25 `CancellationToken`). Application adapts its cancellation
/// token to this port; extraction checks it at deterministic boundaries and
/// never publishes partial extraction or mutation state.
pub trait ExtractionCancellation {
    /// Whether cancellation was requested.
    fn is_cancelled(&self) -> bool;
}

/// The language extractor contract (Plan 25). Language-specific logic stays
/// behind this small interface while identity, lineage, and output contracts
/// are shared.
pub trait LanguageExtractor {
    /// Extract one canonical batch from one validated file under one
    /// descriptor. Identical input, registry, and extractor revisions produce
    /// stable canonical rows and digests on every supported host.
    fn extract(
        &self,
        file: &ValidatedCodeFileV1,
        descriptor: &LanguageDescriptorV1,
        cancellation: &dyn ExtractionCancellation,
    ) -> Result<ExtractionBatchV1, ExtractionFailureV1>;
}
