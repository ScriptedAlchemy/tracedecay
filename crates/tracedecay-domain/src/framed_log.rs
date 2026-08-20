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

#[cfg(test)]
mod tests {
    use super::checksum;

    #[test]
    fn checksum_matches_sha256() {
        assert_eq!(
            checksum(b"abc"),
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad,
            ]
        );
    }
}
