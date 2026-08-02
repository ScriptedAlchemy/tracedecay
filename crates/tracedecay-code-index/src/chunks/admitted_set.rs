use std::sync::Arc;

use tracedecay_domain::{
    CodeSearchChunkV1, ExtractionAdmittedChunkCollectionV1, ExtractionAdmittedChunkV1,
};

use super::{ChunkingFailureV1, ExactExtractionAuthorityV1};

/// One chunk re-admitted through parser-backed extraction authority.
///
/// ```compile_fail
/// use tracedecay_code_index::chunks::ExtractionAdmittedCodeSearchChunkV1;
///
/// let chunk = todo!();
/// let _forged = ExtractionAdmittedCodeSearchChunkV1 { chunk };
/// ```
#[derive(Clone, Debug)]
pub struct ExtractionAdmittedCodeSearchChunkV1 {
    pub(super) chunk: CodeSearchChunkV1,
}

impl ExtractionAdmittedCodeSearchChunkV1 {
    pub fn chunk(&self) -> &CodeSearchChunkV1 {
        &self.chunk
    }

    /// Consume the authority-bearing wrapper and return its admitted chunk.
    pub fn into_chunk(self) -> CodeSearchChunkV1 {
        self.chunk
    }
}

// SAFETY: values are only created by `ExactExtractionAuthorityV1::admit`,
// after the parser-backed chunk digest has been validated.
unsafe impl ExtractionAdmittedChunkV1 for ExtractionAdmittedCodeSearchChunkV1 {
    fn into_admitted_chunk(self) -> CodeSearchChunkV1 {
        self.chunk
    }
}

/// One parser-admitted immutable chunk allocation.
///
/// The set shares the canonical generation allocation instead of copying every
/// content-bearing chunk into another authority-shaped vector.
#[derive(Clone, Debug)]
pub struct ExtractionAdmittedCodeSearchChunkSetV1 {
    chunks: Arc<Vec<CodeSearchChunkV1>>,
}

// SAFETY: construction is private to `ExactExtractionAuthorityV1::admit_shared`,
// which validates the complete collection before retaining its allocation.
unsafe impl ExtractionAdmittedChunkCollectionV1 for ExtractionAdmittedCodeSearchChunkSetV1 {
    fn into_shared_chunks(self) -> Arc<Vec<CodeSearchChunkV1>> {
        self.chunks
    }
}

impl ExactExtractionAuthorityV1 {
    pub fn admit_shared(
        &self,
        chunks: Arc<Vec<CodeSearchChunkV1>>,
    ) -> Result<ExtractionAdmittedCodeSearchChunkSetV1, ChunkingFailureV1> {
        self.validate_all(chunks.as_slice())?;
        Ok(ExtractionAdmittedCodeSearchChunkSetV1 { chunks })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tracedecay_domain::ExtractionAdmittedChunkCollectionV1;

    use super::*;
    use crate::chunks::tests::file_chunks;

    #[test]
    fn admitted_chunk_is_consumed_without_widening_mint_authority() {
        let chunks = file_chunks();
        let expected = chunks.chunks[0].clone();
        let authority = ExactExtractionAuthorityV1::restore(&chunks).expect("sealed authority");
        let admitted = authority.admit(expected.clone()).expect("exact admission");

        assert_eq!(admitted.into_chunk(), expected);
    }

    #[test]
    fn admitted_chunk_set_retains_the_canonical_allocation() {
        let file = file_chunks();
        let authority = ExactExtractionAuthorityV1::restore(&file).expect("sealed authority");
        let canonical = Arc::new(file.chunks);
        let prior_owners = Arc::strong_count(&canonical);

        let admitted = authority
            .admit_shared(Arc::clone(&canonical))
            .expect("shared exact admission");
        let shared = admitted.into_shared_chunks();

        assert_eq!(Arc::strong_count(&canonical), prior_owners + 1);
        assert!(Arc::ptr_eq(&shared, &canonical));
    }

    #[test]
    fn admitted_chunk_vector_converts_to_the_shared_collection_contract() {
        let file = file_chunks();
        let expected = file.chunks.clone();
        let authority = ExactExtractionAuthorityV1::restore(&file).expect("sealed authority");
        let admitted = authority.admit_all(file.chunks).expect("exact admission");

        let shared = admitted.into_shared_chunks();

        assert_eq!(shared.as_slice(), expected);
    }
}
