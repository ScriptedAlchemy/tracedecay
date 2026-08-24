//! Deterministic framing values shared by hook and host-admission spools.

use sha2::{Digest, Sha256};

/// Trailing SHA-256 over exact framed bytes (excluding the checksum suffix).
pub const CHECKSUM_BYTES: usize = 32;

/// SHA-256 over the exact bytes that precede a frame checksum suffix.
pub fn checksum(input: &[u8]) -> [u8; 32] {
    Sha256::digest(input).into()
}

/// Returns true when `tail` is a strict prefix of the unpublished frame bytes
/// recorded in an append intent.
pub fn partial_tail_matches_prefix(tail: &[u8], expected: &[u8], framed_len: usize) -> bool {
    !tail.is_empty() && tail.len() < framed_len && expected.starts_with(tail)
}
