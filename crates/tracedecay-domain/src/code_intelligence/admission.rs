use std::sync::Arc;

use super::CodeSearchChunkV1;

/// Type-state boundary for one chunk re-admitted by parser-backed extraction.
///
/// Consumers may accept this contract without depending on the concrete
/// extraction engine. Implementations remain owned by that engine and return
/// the native domain chunk after their authority checks have succeeded.
///
/// # Safety
///
/// Implementors must only wrap chunks whose authority-sensitive exact terms
/// were produced or revalidated by parser-backed extraction.
pub unsafe trait ExtractionAdmittedChunkV1 {
    fn into_admitted_chunk(self) -> CodeSearchChunkV1;
}

/// Type-state boundary for a shared immutable collection of admitted chunks.
///
/// # Safety
///
/// Every returned chunk must have passed the same parser-backed extraction
/// admission required by [`ExtractionAdmittedChunkV1`].
pub unsafe trait ExtractionAdmittedChunkCollectionV1 {
    fn into_shared_chunks(self) -> Arc<Vec<CodeSearchChunkV1>>;
}

// SAFETY: the element contract guarantees every consumed chunk was admitted.
unsafe impl<C> ExtractionAdmittedChunkCollectionV1 for Vec<C>
where
    C: ExtractionAdmittedChunkV1,
{
    fn into_shared_chunks(self) -> Arc<Vec<CodeSearchChunkV1>> {
        Arc::new(
            self.into_iter()
                .map(ExtractionAdmittedChunkV1::into_admitted_chunk)
                .collect(),
        )
    }
}
