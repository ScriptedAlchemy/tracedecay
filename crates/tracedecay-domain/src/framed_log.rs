//! Deterministic framing values shared by hook and host-admission spools.

use sha2::{Digest, Sha256};

/// Trailing SHA-256 over exact framed bytes (excluding the checksum suffix).
pub const CHECKSUM_BYTES: usize = 32;

/// SHA-256 over the exact bytes that precede a frame checksum suffix.
pub fn checksum(input: &[u8]) -> [u8; 32] {
    Sha256::digest(input).into()
}
