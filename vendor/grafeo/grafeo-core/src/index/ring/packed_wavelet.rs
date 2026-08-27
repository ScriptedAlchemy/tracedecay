//! Packed wavelet tree for the v2 Ring on-disk format (Phase 6c).
//!
//! The in-memory [`WaveletTree`] stores `height` `SuccinctBitVector`
//! levels alongside rank/select sampling caches. The bincode'd v1 format
//! serializes both the bit data AND the caches, even though the caches
//! are O(n) rebuildable from the bits alone (per
//! [`SuccinctBitVector::from_bitvec`]).
//!
//! v2 keeps only the bit data, packed as little-endian `u64` words, and
//! rebuilds caches on `to_wavelet_tree`. This shrinks the on-disk size
//! ~30-40% and removes schema overhead, while the reload cost is
//! unchanged (cache rebuild is the dominant term either way).
//!
//! ## Layout
//!
//! ```text
//! Header (40 bytes):
//!     0..4    magic "WTRE"
//!     4       version u8 = 1
//!     5..8    reserved (3 bytes, zero)
//!     8..12   height u32 LE
//!     12..16  padding (4 bytes, zero) — aligns u64 fields to 8-byte boundary
//!     16..24  sigma u64 LE                 // alphabet size
//!     24..32  len u64 LE                   // sequence length (== bits per level)
//!     32..40  symbol_count u64 LE          // length of the symbols region in elements
//!
//! symbols region: symbol_count * 8 bytes (u64 LE, sorted)
//! per-level region (height entries):
//!     bit_count: u64 LE
//!     word_count: u64 LE
//!     word_count * 8 bytes of LE u64 BitVector data
//! ```

use bytes::Bytes;

use crate::codec::BitVector;
use crate::codec::succinct::{SuccinctBitVector, WaveletTree};

const MAGIC: &[u8; 4] = b"WTRE";
const VERSION: u8 = 1;
const HEADER_SIZE: usize = 40;

/// Errors returned when parsing a packed wavelet tree from bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackedWaveletError {
    /// Buffer is too short to contain even the fixed-size header.
    TruncatedHeader,
    /// First 4 bytes don't match "WTRE".
    BadMagic,
    /// Version byte not recognized.
    UnsupportedVersion(u8),
    /// Recorded sizes overflow the input buffer.
    Truncated {
        /// Region we were trying to read.
        region: &'static str,
    },
    /// A field overflows the platform-native usize.
    SizeOverflow,
    /// Per-level bit count doesn't match the declared `len` field.
    BitCountMismatch {
        /// Level index where the mismatch was observed.
        level: usize,
        /// Bit count declared in the header.
        expected: u64,
        /// Bit count observed in the level.
        actual: u64,
    },
    /// Reconstructed parts violated a structural [`WaveletTree`]
    /// invariant — caught here rather than letting the tree return
    /// inconsistent answers from `access`/`rank`.
    InvariantViolation(crate::codec::succinct::WaveletInvariantError),
}

impl std::fmt::Display for PackedWaveletError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TruncatedHeader => write!(f, "packed wavelet header truncated"),
            Self::BadMagic => write!(f, "packed wavelet bad magic (expected 'WTRE')"),
            Self::UnsupportedVersion(v) => write!(f, "packed wavelet unsupported version {v}"),
            Self::Truncated { region } => write!(f, "packed wavelet truncated in {region}"),
            Self::SizeOverflow => write!(f, "packed wavelet size field overflows usize"),
            Self::BitCountMismatch {
                level,
                expected,
                actual,
            } => write!(
                f,
                "packed wavelet bit count mismatch at level {level}: expected {expected}, got {actual}"
            ),
            Self::InvariantViolation(e) => write!(f, "packed wavelet invariant violation: {e}"),
        }
    }
}

impl std::error::Error for PackedWaveletError {}

/// Serializes a [`WaveletTree`] to the v2 packed format.
#[must_use]
pub fn serialize_wavelet_tree(tree: &WaveletTree) -> Vec<u8> {
    let symbols = tree.symbols_slice();
    let height = tree.height();
    let levels = tree.levels_slice();
    let sigma = tree.sigma();
    let len = tree.len() as u64;

    // Estimate total size to pre-allocate.
    let symbols_bytes = symbols.len() * 8;
    let level_bytes: usize = levels
        .iter()
        .map(|sbv| 16 /* bit_count + word_count */ + sbv.inner().data_bytes().len())
        .sum();
    let total = HEADER_SIZE + symbols_bytes + level_bytes;

    let mut buf = Vec::with_capacity(total);
    // Header (40 bytes total — see module-top layout doc):
    buf.extend_from_slice(MAGIC); // 0..4
    buf.push(VERSION); // 4
    buf.extend_from_slice(&[0u8; 3]); // 5..8 reserved
    buf.extend_from_slice(&u32::try_from(height).unwrap_or(u32::MAX).to_le_bytes()); // 8..12
    buf.extend_from_slice(&[0u8; 4]); // 12..16 padding to align sigma
    buf.extend_from_slice(&sigma.to_le_bytes()); // 16..24
    buf.extend_from_slice(&len.to_le_bytes()); // 24..32
    buf.extend_from_slice(&(symbols.len() as u64).to_le_bytes()); // 32..40 symbol_count

    // Symbols.
    for &sym in symbols {
        buf.extend_from_slice(&sym.to_le_bytes());
    }

    // Levels.
    for sbv in levels {
        let bv = sbv.inner();
        let bit_count = bv.len() as u64;
        let word_data = bv.data_bytes();
        let word_count = (word_data.len() / 8) as u64;
        buf.extend_from_slice(&bit_count.to_le_bytes());
        buf.extend_from_slice(&word_count.to_le_bytes());
        buf.extend_from_slice(word_data);
    }

    buf
}

/// Parses a [`WaveletTree`] from the v2 packed format. Rebuilds rank/select
/// caches per level via [`SuccinctBitVector::from_bitvec`].
///
/// `data` is consumed via `Bytes::slice` so the underlying allocation is
/// shared with the caller. Per-level `BitVector`s adopt their slices via
/// [`BitVector::from_mmap`], so a mmap-backed buffer never copies.
///
/// # Errors
///
/// Returns a [`PackedWaveletError`] on truncation, magic/version
/// mismatch, or per-level bit-count inconsistency.
///
/// # Panics
///
/// Internal `expect` calls describe invariants that the bounds checks
/// above already guarantee — every indexed read is preceded by an
/// explicit length check. Does not panic in normal operation.
pub fn deserialize_wavelet_tree(data: Bytes) -> Result<WaveletTree, PackedWaveletError> {
    if data.len() < HEADER_SIZE {
        return Err(PackedWaveletError::TruncatedHeader);
    }
    if &data[0..4] != MAGIC {
        return Err(PackedWaveletError::BadMagic);
    }
    let version = data[4];
    if version != VERSION {
        return Err(PackedWaveletError::UnsupportedVersion(version));
    }
    // Header offsets (per module-top layout doc):
    let height_raw = u32::from_le_bytes(data[8..12].try_into().expect("4-byte slice"));
    // 12..16 is padding.
    let sigma = u64::from_le_bytes(data[16..24].try_into().expect("8-byte slice"));
    let len_raw = u64::from_le_bytes(data[24..32].try_into().expect("8-byte slice"));
    let symbol_count_raw = u64::from_le_bytes(data[32..40].try_into().expect("8-byte slice"));

    let height = usize::try_from(height_raw).map_err(|_| PackedWaveletError::SizeOverflow)?;
    let len_usize = usize::try_from(len_raw).map_err(|_| PackedWaveletError::SizeOverflow)?;
    let symbol_count =
        usize::try_from(symbol_count_raw).map_err(|_| PackedWaveletError::SizeOverflow)?;

    let mut cursor = HEADER_SIZE;

    // Symbols region.
    let symbols_bytes = symbol_count
        .checked_mul(8)
        .ok_or(PackedWaveletError::SizeOverflow)?;
    let symbols_end = cursor
        .checked_add(symbols_bytes)
        .ok_or(PackedWaveletError::SizeOverflow)?;
    if symbols_end > data.len() {
        return Err(PackedWaveletError::Truncated { region: "symbols" });
    }
    let mut symbols: Vec<u64> = Vec::with_capacity(symbol_count);
    for i in 0..symbol_count {
        // Inner offsets are safe: `symbols_end = cursor + symbols_bytes`
        // is bounds-checked above, and i < symbol_count implies
        // `cursor + i*8 + 8 <= symbols_end`.
        let off = cursor + i * 8;
        let chunk: [u8; 8] = data[off..off + 8].try_into().expect("8-byte slice");
        symbols.push(u64::from_le_bytes(chunk));
    }
    cursor = symbols_end;

    // Levels region.
    let mut levels: Vec<SuccinctBitVector> = Vec::with_capacity(height);
    for level_idx in 0..height {
        let level_header_end = cursor
            .checked_add(16)
            .ok_or(PackedWaveletError::SizeOverflow)?;
        if level_header_end > data.len() {
            return Err(PackedWaveletError::Truncated {
                region: "level header",
            });
        }
        // Header bounds verified above: `level_header_end = cursor + 16`
        // doesn't overflow and is in range.
        let bit_count =
            u64::from_le_bytes(data[cursor..cursor + 8].try_into().expect("8-byte slice"));
        let word_count = u64::from_le_bytes(
            data[cursor + 8..cursor + 16]
                .try_into()
                .expect("8-byte slice"),
        );
        cursor = level_header_end;

        if bit_count != len_raw {
            return Err(PackedWaveletError::BitCountMismatch {
                level: level_idx,
                expected: len_raw,
                actual: bit_count,
            });
        }

        let level_bytes = usize::try_from(
            word_count
                .checked_mul(8)
                .ok_or(PackedWaveletError::SizeOverflow)?,
        )
        .map_err(|_| PackedWaveletError::SizeOverflow)?;
        let level_data_end = cursor
            .checked_add(level_bytes)
            .ok_or(PackedWaveletError::SizeOverflow)?;
        if level_data_end > data.len() {
            return Err(PackedWaveletError::Truncated {
                region: "level data",
            });
        }
        let level_slice = data.slice(cursor..level_data_end);
        cursor = level_data_end;

        let bv = BitVector::from_mmap(level_slice, len_usize).map_err(|_| {
            PackedWaveletError::Truncated {
                region: "level bits",
            }
        })?;
        levels.push(SuccinctBitVector::from_bitvec(bv));
    }

    WaveletTree::from_packed_parts(levels, height, sigma, len_usize, symbols)
        .map_err(PackedWaveletError::InvariantViolation)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_tree(seq: &[u64]) -> WaveletTree {
        WaveletTree::new(seq)
    }

    fn assert_trees_equal(orig: &WaveletTree, restored: &WaveletTree) {
        assert_eq!(orig.len(), restored.len());
        assert_eq!(orig.sigma(), restored.sigma());
        for i in 0..orig.len() {
            assert_eq!(
                orig.access(i),
                restored.access(i),
                "access mismatch at position {i}"
            );
        }
    }

    #[test]
    fn alix_packed_wavelet_roundtrip_small() {
        let seq = vec![1u64, 3, 2, 1, 2, 3, 1, 2];
        let tree = build_tree(&seq);
        let bytes = serialize_wavelet_tree(&tree);
        let restored = deserialize_wavelet_tree(Bytes::from(bytes)).expect("deserialize");
        assert_trees_equal(&tree, &restored);
    }

    #[test]
    fn gus_packed_wavelet_roundtrip_large() {
        // 1024 symbols drawn from an alphabet of 16 — exercises multi-level
        // wavelet structure at non-trivial size.
        let seq: Vec<u64> = (0..1024u64).map(|i| (i * 7) % 16).collect();
        let tree = build_tree(&seq);
        let bytes = serialize_wavelet_tree(&tree);
        let restored = deserialize_wavelet_tree(Bytes::from(bytes)).expect("deserialize");
        assert_trees_equal(&tree, &restored);
    }

    #[test]
    fn vincent_packed_wavelet_empty() {
        let tree = WaveletTree::new(&[]);
        let bytes = serialize_wavelet_tree(&tree);
        let restored = deserialize_wavelet_tree(Bytes::from(bytes)).expect("deserialize");
        assert_eq!(restored.len(), 0);
        assert!(restored.is_empty());
    }

    #[test]
    fn jules_packed_wavelet_single_symbol() {
        // sigma = 1; height ends up = 1 per `WaveletTree::new`.
        let seq = vec![42u64; 16];
        let tree = build_tree(&seq);
        let bytes = serialize_wavelet_tree(&tree);
        let restored = deserialize_wavelet_tree(Bytes::from(bytes)).expect("deserialize");
        assert_trees_equal(&tree, &restored);
    }

    #[test]
    fn mia_packed_wavelet_bad_magic_rejected() {
        let bad = Bytes::from(vec![0u8; HEADER_SIZE]);
        assert_eq!(
            deserialize_wavelet_tree(bad).unwrap_err(),
            PackedWaveletError::BadMagic
        );
    }

    #[test]
    fn shosanna_packed_wavelet_truncated_header_rejected() {
        let short = Bytes::from(vec![b'W', b'T', b'R', b'E']);
        assert_eq!(
            deserialize_wavelet_tree(short).unwrap_err(),
            PackedWaveletError::TruncatedHeader
        );
    }

    #[test]
    fn beatrix_packed_wavelet_unsupported_version_rejected() {
        let mut buf = vec![0u8; HEADER_SIZE];
        buf[..4].copy_from_slice(MAGIC);
        buf[4] = 99;
        assert_eq!(
            deserialize_wavelet_tree(Bytes::from(buf)).unwrap_err(),
            PackedWaveletError::UnsupportedVersion(99)
        );
    }

    #[test]
    fn hans_packed_wavelet_size_smaller_than_bincode() {
        // The whole point of v2: smaller than bincode by removing
        // redundant rank/select caches and schema overhead.
        let seq: Vec<u64> = (0..2048u64).map(|i| (i * 11) % 64).collect();
        let tree = build_tree(&seq);
        let v2_bytes = serialize_wavelet_tree(&tree);
        let v1_bytes = bincode::serde::encode_to_vec(&tree, bincode::config::standard())
            .expect("bincode encode");
        eprintln!(
            "v1 bincode: {} bytes, v2 packed: {} bytes",
            v1_bytes.len(),
            v2_bytes.len()
        );
        assert!(
            v2_bytes.len() < v1_bytes.len(),
            "v2 packed must be smaller than v1 bincode (v1={}, v2={})",
            v1_bytes.len(),
            v2_bytes.len()
        );
    }

    #[test]
    fn django_packed_wavelet_zero_copy_per_level() {
        // Round-trip and confirm restored levels' inner BitVector data
        // shares the underlying source allocation (zero-copy mmap path).
        let seq = vec![1u64, 2, 3, 4, 5, 6, 7, 8];
        let tree = build_tree(&seq);
        let bytes = serialize_wavelet_tree(&tree);
        let source = Bytes::from(bytes);
        let source_ptr = source.as_ptr();
        let source_len = source.len();

        let restored = deserialize_wavelet_tree(source).expect("deserialize");
        for (idx, level) in restored.levels_slice().iter().enumerate() {
            let inner_ptr = level.inner().data_bytes().as_ptr();
            let offset = inner_ptr as usize - source_ptr as usize;
            assert!(
                offset < source_len,
                "level {idx}: inner BitVector should be inside source allocation; offset={offset}"
            );
        }
    }
}
