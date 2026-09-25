use std::collections::BTreeSet;

use tracedecay_code_index::production::CodeIndexExecutionControlV1;

use super::{ARTIFACT_DOCUMENT_SCRATCH_LIMIT_BYTES, CodeLexicalArtifactErrorV1, checkpoint};

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

/// Canonical bounded value projection for out-of-transaction preparation.
pub(super) fn document_ngrams(
    bytes: &[u8],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<Vec<u32>, CodeLexicalArtifactErrorV1> {
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
    let mut observed = 0usize;
    for width in 1..=bytes.len().min(3) {
        for window in bytes.windows(width) {
            if observed.is_multiple_of(4_096) {
                checkpoint(control)?;
            }
            ngrams.push(super::super::pack_byte_ngram(window));
            observed = observed.checked_add(1).ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Contract(
                    "lexical artifact n-gram work count overflowed".to_owned(),
                )
            })?;
        }
    }
    ngrams.sort_unstable();
    ngrams.dedup();
    Ok(ngrams)
}

pub(super) fn query_ngrams(bytes: &[u8]) -> BTreeSet<u32> {
    super::super::packed_query_ngrams(bytes)
}

/// Whether a packed n-gram needs the case-preserving kind. Normalized text is
/// the ASCII-lowercased raw text at the same byte offsets, so a raw window
/// without an ASCII uppercase byte is already the normalized window there.
pub(super) fn ngram_is_case_sensitive(ngram: u32) -> bool {
    let width = ngram >> 24;
    (0..width).any(|index| ((ngram >> (index * 8)) as u8).is_ascii_uppercase())
}

/// The kind that holds each query n-gram for a case-sensitive raw match, or
/// `None` when every window is case-insensitive and the normalized query
/// already admits every raw match.
pub(super) fn raw_override_query_ngrams(bytes: &[u8]) -> Option<Vec<(i64, u32)>> {
    let keys = query_ngrams(bytes)
        .into_iter()
        .map(|ngram| {
            let kind = if ngram_is_case_sensitive(ngram) {
                NGRAM_RAW_OVERRIDE
            } else {
                NGRAM_NORMALIZED
            };
            (kind, ngram)
        })
        .collect::<Vec<_>>();
    keys.iter()
        .any(|(kind, _)| *kind == NGRAM_RAW_OVERRIDE)
        .then_some(keys)
}

#[cfg(test)]
mod tests {
    use super::{document_ngram_scratch, reserve_ngram_scratch};

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
