use std::collections::BTreeSet;

use rusqlite::{Transaction, params};
use tracedecay_code_index::production::CodeIndexExecutionControlV1;

use super::{
    ARTIFACT_DOCUMENT_SCRATCH_LIMIT_BYTES, CodeLexicalArtifactErrorV1, checkpoint, sqlite_error,
};

pub(super) const NGRAM_NORMALIZED: i64 = 0;
pub(super) const NGRAM_RAW_OVERRIDE: i64 = 1;

/// A conservative pre-dedup n-gram scratch reservation for one document. The
/// calculation is arithmetic-only so a page can be refused before any n-gram
/// scratch allocation is attempted.
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
    let reservation_capacity = if window_count == 0 {
        0
    } else {
        window_count.checked_next_power_of_two().ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact n-gram scratch capacity overflowed".to_owned(),
            )
        })?
    };
    let scratch_bytes = reservation_capacity
        .checked_mul(std::mem::size_of::<u32>())
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact n-gram scratch bytes overflowed".to_owned(),
            )
        })?;
    Ok((window_count, scratch_bytes))
}

/// Reserve the authorized pre-dedup n-gram scratch capacity, surfacing
/// allocation failure as a typed I/O error.
fn reserve_ngram_scratch(
    reservation_capacity: usize,
) -> Result<Vec<u32>, CodeLexicalArtifactErrorV1> {
    let mut scratch = Vec::new();
    scratch
        .try_reserve_exact(reservation_capacity)
        .map_err(|error| {
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
    let (_, scratch_bytes) = document_ngram_scratch(bytes.len())?;
    if scratch_bytes > ARTIFACT_DOCUMENT_SCRATCH_LIMIT_BYTES {
        return Err(CodeLexicalArtifactErrorV1::Contract(format!(
            "one lexical document requires {scratch_bytes} bytes of n-gram scratch, exceeding the {}-byte bound",
            ARTIFACT_DOCUMENT_SCRATCH_LIMIT_BYTES
        )));
    }
    let reservation_capacity = scratch_bytes / std::mem::size_of::<u32>();
    let mut ngrams = reserve_ngram_scratch(reservation_capacity)?;
    let allocated_bytes = ngrams
        .capacity()
        .checked_mul(std::mem::size_of::<u32>())
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "allocated lexical n-gram scratch bytes overflowed".to_owned(),
            )
        })?;
    if allocated_bytes > scratch_bytes {
        return Err(CodeLexicalArtifactErrorV1::Unreserved(format!(
            "lexical n-gram allocator retained {allocated_bytes} bytes beyond the {scratch_bytes}-byte scratch authority"
        )));
    }
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
    fn ngram_scratch_charge_covers_the_real_boundary_reservation() {
        let (window_count, scratch_bytes) = document_ngram_scratch(2).expect("scratch charge");
        assert_eq!(window_count, 3, "two bytes produce three n-gram windows");

        let scratch = reserve_ngram_scratch(4).expect("boundary reservation");
        let allocated_bytes = scratch
            .capacity()
            .checked_mul(std::mem::size_of::<u32>())
            .expect("allocated scratch bytes");
        assert!(
            scratch_bytes >= allocated_bytes,
            "the preflight charged {scratch_bytes} bytes for {window_count} windows, but the boundary reservation retained {allocated_bytes} bytes at capacity {}",
            scratch.capacity()
        );
    }

    #[test]
    fn ngram_scratch_charge_covers_real_reservations() {
        for (text_len, expected_windows, expected_capacity) in [
            (0usize, 0usize, 0usize),
            (1, 1, 1),
            (3, 6, 8),
            (64, 189, 256),
            (4_096, 12_285, 16_384),
            (100_000, 299_997, 524_288),
        ] {
            let (window_count, scratch_bytes) =
                document_ngram_scratch(text_len).expect("scratch charge");
            assert_eq!(
                window_count, expected_windows,
                "the window count must remain exact"
            );
            assert_eq!(
                scratch_bytes,
                expected_capacity * std::mem::size_of::<u32>(),
                "the allocation-free preflight must charge the conservative reservation"
            );
            let scratch = reserve_ngram_scratch(expected_capacity).expect("scratch reservation");
            let allocated_bytes = scratch.capacity() * std::mem::size_of::<u32>();
            assert!(
                scratch_bytes >= allocated_bytes,
                "the preflight charge must cover the real reservation capacity"
            );
            assert!(scratch.capacity() >= window_count);
        }
    }
}
