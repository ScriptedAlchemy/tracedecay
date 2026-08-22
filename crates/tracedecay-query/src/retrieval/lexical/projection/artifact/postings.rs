use std::collections::BTreeSet;

use rusqlite::{Transaction, params};
use tracedecay_code_index::production::CodeIndexExecutionControlV1;

use super::{
    ARTIFACT_DOCUMENT_SCRATCH_LIMIT_BYTES, CodeLexicalArtifactErrorV1, checkpoint, sqlite_error,
};

pub(super) const NGRAM_NORMALIZED: i64 = 0;
pub(super) const NGRAM_RAW_OVERRIDE: i64 = 1;

/// The exact pre-dedup n-gram scratch one document allocates: the window
/// count and the measured scratch bytes reserved for it.
///
/// `try_reserve_exact` may end with a capacity above the request when the
/// allocator returns a larger block, so the scratch bytes are measured on a
/// real reservation instead of assuming `window_count * 4`.
/// `insert_document_ngrams` performs the same reservation, sorts in place,
/// and compacts duplicates in place, so this is the document's n-gram
/// allocation peak; the build memory ledger charges the same number.
pub(super) fn document_ngram_scratch(
    text_len: usize,
) -> Result<(usize, usize), CodeLexicalArtifactErrorV1> {
    let window_count = (1..=text_len.min(3)).try_fold(0usize, |count, width| {
        count
            .checked_add(text_len.saturating_sub(width).saturating_add(1))
            .ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Contract(
                    "lexical artifact n-gram scratch count overflowed".to_owned(),
                )
            })
    })?;
    let scratch = reserve_ngram_scratch(window_count)?;
    let scratch_bytes = scratch
        .capacity()
        .checked_mul(std::mem::size_of::<u32>())
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact n-gram scratch bytes overflowed".to_owned(),
            )
        })?;
    Ok((window_count, scratch_bytes))
}

/// Reserve the exact pre-dedup n-gram scratch vector, surfacing allocation
/// failure as a typed I/O error. Both the ledger measurement and the insert
/// path use this same reservation, so the charge equals the allocation.
fn reserve_ngram_scratch(window_count: usize) -> Result<Vec<u32>, CodeLexicalArtifactErrorV1> {
    let mut scratch = Vec::new();
    scratch.try_reserve_exact(window_count).map_err(|error| {
        CodeLexicalArtifactErrorV1::Io(format!(
            "bounded lexical n-gram scratch allocation failed: {error}"
        ))
    })?;
    Ok(scratch)
}

pub(super) fn insert_document_ngrams(
    transaction: &Transaction<'_>,
    kind: i64,
    document: i64,
    bytes: &[u8],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let (window_count, scratch_bytes) = document_ngram_scratch(bytes.len())?;
    if scratch_bytes > ARTIFACT_DOCUMENT_SCRATCH_LIMIT_BYTES {
        return Err(CodeLexicalArtifactErrorV1::Contract(format!(
            "one lexical document requires {scratch_bytes} bytes of n-gram scratch, exceeding the {}-byte bound",
            ARTIFACT_DOCUMENT_SCRATCH_LIMIT_BYTES
        )));
    }
    let mut ngrams = reserve_ngram_scratch(window_count)?;
    for width in 1..=bytes.len().min(3) {
        ngrams.extend(bytes.windows(width).map(pack_byte_ngram));
    }
    ngrams.sort_unstable();
    ngrams.dedup();
    let mut statement = transaction
        .prepare("INSERT INTO ngram_postings(kind, ngram, document_id) VALUES (?1, ?2, ?3)")
        .map_err(sqlite_error)?;
    for (ordinal, ngram) in ngrams.into_iter().enumerate() {
        if ordinal % 4_096 == 0 {
            checkpoint(control)?;
        }
        statement
            .execute(params![kind, i64::from(ngram), document])
            .map_err(sqlite_error)?;
    }
    Ok(())
}

pub(super) fn query_ngrams(bytes: &[u8]) -> BTreeSet<u32> {
    let width = bytes.len().min(3);
    if width == 0 {
        return BTreeSet::new();
    }
    bytes.windows(width).map(pack_byte_ngram).collect()
}

fn pack_byte_ngram(bytes: &[u8]) -> u32 {
    debug_assert!((1..=3).contains(&bytes.len()));
    bytes
        .iter()
        .enumerate()
        .fold((bytes.len() as u32) << 24, |packed, (index, byte)| {
            packed | (u32::from(*byte) << (index * 8))
        })
}

#[cfg(test)]
mod tests {
    use super::{document_ngram_scratch, reserve_ngram_scratch};

    #[test]
    fn ngram_scratch_charge_equals_a_real_reservation() {
        for text_len in [0usize, 1, 2, 3, 64, 4_096, 100_000] {
            let (window_count, scratch_bytes) =
                document_ngram_scratch(text_len).expect("scratch charge");
            let scratch = reserve_ngram_scratch(window_count).expect("scratch reservation");
            assert_eq!(
                scratch_bytes,
                scratch.capacity() * std::mem::size_of::<u32>(),
                "the ledger must charge the reservation's actual capacity, not the request"
            );
            assert!(scratch.capacity() >= window_count);
        }
    }
}
