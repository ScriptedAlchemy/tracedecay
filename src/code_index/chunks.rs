//! Deterministic chunker port (Plan 25, "Code-search chunk and projection
//! contract"): build the five-grain chunks and their parent/child hierarchy
//! from one extraction batch.
//!
//! Every eligible sanitized byte is covered by a declared chunk or an
//! explicit unsupported/excluded range. Oversized bodies split on
//! deterministic structural boundaries; if none are available, fixed byte
//! windows with pinned size/overlap are used. Extractor enumeration order
//! and mutable line numbers cannot affect `CodeSearchChunkId`.

use thiserror::Error;
use tracedecay_domain::{
    CodeSearchChunkV1, CodeSearchDocumentV1, ExtractionBatchV1, LanguageDescriptorV1,
    ValidatedCodeFileV1,
};

use super::extract::ExtractionCancellation;

/// Chunker failures. Partial coverage is evidence, not an error; errors are
/// reserved for contract violations.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ChunkingFailureV1 {
    #[error("the descriptor does not match the extraction batch language")]
    DescriptorMismatch,
    #[error("the extraction batch is not generation-consistent with the document")]
    GenerationMismatch,
    #[error("chunking was cancelled")]
    Cancelled,
    #[error("chunk identity inputs are not canonical: {0}")]
    NonCanonicalIdentity(String),
}

/// The deterministic chunker contract (Plan 25: `src/code_index/chunks.rs`
/// builds chunks and their parent/child hierarchy).
pub trait CodeChunker {
    /// Build every chunk for one validated file plus its extraction batch,
    /// covering every eligible sanitized byte with a declared chunk or an
    /// explicit unsupported/excluded range.
    fn chunk_file(
        &self,
        file: &ValidatedCodeFileV1,
        batch: &ExtractionBatchV1,
        descriptor: &LanguageDescriptorV1,
        cancellation: &dyn ExtractionCancellation,
    ) -> Result<CodeFileChunksV1, ChunkingFailureV1>;
}

/// The chunks produced for one file: the generation-bound document manifest
/// plus its chunks in deterministic order (Plan 25).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeFileChunksV1 {
    pub document: CodeSearchDocumentV1,
    pub chunks: Vec<CodeSearchChunkV1>,
}

impl CodeFileChunksV1 {
    /// Validate the generation/file binding and canonical document membership
    /// of one chunker result before it can cross the publication boundary.
    pub fn validate(&self) -> Result<(), ChunkingFailureV1> {
        self.document
            .generation_id
            .validate()
            .map_err(|error| ChunkingFailureV1::NonCanonicalIdentity(error.to_string()))?;
        self.document
            .file_occurrence_id
            .validate()
            .map_err(|error| ChunkingFailureV1::NonCanonicalIdentity(error.to_string()))?;
        self.document
            .content_digest
            .validate()
            .map_err(|error| ChunkingFailureV1::NonCanonicalIdentity(error.to_string()))?;

        if self.document.chunk_ids.len() != self.chunks.len()
            || self
                .document
                .chunk_ids
                .iter()
                .zip(&self.chunks)
                .any(|(document_id, chunk)| document_id != &chunk.id)
        {
            return Err(ChunkingFailureV1::NonCanonicalIdentity(
                "document chunk membership does not match canonical chunk order".to_owned(),
            ));
        }
        for chunk in &self.chunks {
            if chunk.anchor.generation_id != self.document.generation_id
                || chunk.anchor.file_occurrence_id != self.document.file_occurrence_id
            {
                return Err(ChunkingFailureV1::GenerationMismatch);
            }
            chunk
                .validate()
                .map_err(|error| ChunkingFailureV1::NonCanonicalIdentity(error.to_string()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{
        BoundedSanitizedText, ChunkerRevision, CodeGenerationId, CodeSearchChunkAnchorV1,
        CodeSearchChunkGrainV1, CodeSearchChunkId, CodeSearchEligibilityV1, ContentDigest,
        FileOccurrenceId, LanguageDescriptorRevision, PolicyRevisionId, SanitizerRevision,
        SensitivityDecision, SensitivityLevelV1, SourceSpan,
    };

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("valid fixture identity")
    }

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn file_chunks() -> CodeFileChunksV1 {
        let generation_id: CodeGenerationId = id("generation.fixture");
        let file_occurrence_id: FileOccurrenceId = id("file.fixture");
        let chunk_id: CodeSearchChunkId = id("chunk.fixture");
        CodeFileChunksV1 {
            document: CodeSearchDocumentV1 {
                generation_id: generation_id.clone(),
                file_occurrence_id: file_occurrence_id.clone(),
                content_digest: id::<ContentDigest>(&digest('a')),
                eligibility: CodeSearchEligibilityV1::Eligible,
                chunk_ids: vec![chunk_id.clone()],
            },
            chunks: vec![CodeSearchChunkV1 {
                id: chunk_id,
                anchor: CodeSearchChunkAnchorV1 {
                    generation_id,
                    file_occurrence_id,
                    symbol_occurrence_id: None,
                    parent_chunk_id: None,
                    source_span: SourceSpan {
                        start_byte: 0,
                        end_byte: 4,
                    },
                    grain: CodeSearchChunkGrainV1::FileWindow,
                    ordinal: 0,
                },
                content_digest: id::<ContentDigest>(&digest('b')),
                language_descriptor_revision: id::<LanguageDescriptorRevision>("descriptor.v1"),
                chunker_revision: id::<ChunkerRevision>("chunker.v1"),
                sanitizer_revision: id::<SanitizerRevision>("sanitizer.v1"),
                sensitivity: SensitivityDecision {
                    level: SensitivityLevelV1::Internal,
                    policy_revision: id::<PolicyRevisionId>("policy.v1"),
                },
                exact_terms: vec![],
                subtokens: vec!["text".to_owned()],
                sanitized_text: BoundedSanitizedText::new("text").unwrap(),
            }],
        }
    }

    #[test]
    fn file_chunks_reject_mixed_generation_or_document_membership() {
        file_chunks().validate().expect("consistent file chunks");

        let mut mixed_generation = file_chunks();
        mixed_generation.chunks[0].anchor.generation_id = id("generation.other");
        assert_eq!(
            mixed_generation.validate(),
            Err(ChunkingFailureV1::GenerationMismatch)
        );

        let mut wrong_membership = file_chunks();
        wrong_membership.document.chunk_ids[0] = id("chunk.other");
        assert!(wrong_membership.validate().is_err());
    }
}
