//! Stores booleans as individual bits - 8x smaller than `Vec<bool>`.
//!
//! Use this when you're tracking lots of boolean flags (like null bitmaps
//! or set membership). Phase 3b: storage split into an immutable
//! `BitVector` (refcounted [`Bytes`]) and a mutable [`BitVectorBuilder`]
//! that produces one via [`freeze`](BitVectorBuilder::freeze). This is
//! the Apache Arrow / Lance "Array + Builder" idiom; the immutable side
//! supports zero-copy mmap-backing for Phase 3c, while the builder
//! retains the cheap word-level mutations needed by succinct-index
//! construction (rank/select, wavelet trees, Elias-Fano).
//!
//! # Example
//!
//! ```no_run
//! # use grafeo_core::codec::bitvec::BitVector;
//! let bools = vec![true, false, true, true, false, false, true, false];
//! let bitvec = BitVector::from_bools(&bools);
//! // Stored as: 0b01001101 (1 byte instead of 8)
//!
//! assert_eq!(bitvec.get(0), Some(true));
//! assert_eq!(bitvec.get(1), Some(false));
//! assert_eq!(bitvec.count_ones(), 4);
//! ```

use std::io;

use bytes::{Bytes, BytesMut};
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};

/// Immutable bitset stored in a refcounted [`Bytes`] buffer of LE u64
/// words.
///
/// Supports bitwise combinators ([`and`](Self::and), [`or`](Self::or),
/// [`not`](Self::not), [`xor`](Self::xor)) that *return new bitvectors*.
/// Mutation lives on [`BitVectorBuilder`].
#[derive(Debug, Clone)]
pub struct BitVector {
    /// LE u64 words concatenated, refcounted. Heap-owned and mmap-backed
    /// columns share this type — only the constructor differs.
    data: Bytes,
    /// Number of bits stored.
    len: usize,
}

impl PartialEq for BitVector {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.data == other.data
    }
}

impl Eq for BitVector {}

impl serde::Serialize for BitVector {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        // On-disk shape stays `{data: Vec<u64>, len: usize}` for backward
        // compatibility with the v1 LpgStoreSection format. Internal
        // storage is now `Bytes`; we materialize a `Vec<u64>` only for
        // serialization.
        let words: Vec<u64> = (0..self.word_count())
            .map(|i| self.word_at(i).unwrap_or(0))
            .collect();
        let mut s = serializer.serialize_struct("BitVector", 2)?;
        s.serialize_field("data", &words)?;
        s.serialize_field("len", &self.len)?;
        s.end()
    }
}

impl<'de> Deserialize<'de> for BitVector {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        #[serde(field_identifier, rename_all = "lowercase")]
        enum Field {
            Data,
            Len,
        }

        struct BitVectorVisitor;

        impl<'de> Visitor<'de> for BitVectorVisitor {
            type Value = BitVector;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("struct BitVector with consistent data and len fields")
            }

            fn visit_seq<V>(self, mut seq: V) -> Result<BitVector, V::Error>
            where
                V: SeqAccess<'de>,
            {
                let data: Vec<u64> = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::invalid_length(0, &self))?;
                let len: usize = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::invalid_length(1, &self))?;
                validate_bitvec(len, &data).map_err(de::Error::custom)
            }

            fn visit_map<V>(self, mut map: V) -> Result<BitVector, V::Error>
            where
                V: MapAccess<'de>,
            {
                let mut data: Option<Vec<u64>> = None;
                let mut len: Option<usize> = None;

                while let Some(key) = map.next_key()? {
                    match key {
                        Field::Data => {
                            if data.is_some() {
                                return Err(de::Error::duplicate_field("data"));
                            }
                            data = Some(map.next_value()?);
                        }
                        Field::Len => {
                            if len.is_some() {
                                return Err(de::Error::duplicate_field("len"));
                            }
                            len = Some(map.next_value()?);
                        }
                    }
                }

                let data = data.ok_or_else(|| de::Error::missing_field("data"))?;
                let len = len.ok_or_else(|| de::Error::missing_field("len"))?;
                validate_bitvec(len, &data).map_err(de::Error::custom)
            }
        }

        const FIELDS: &[&str] = &["data", "len"];
        deserializer.deserialize_struct("BitVector", FIELDS, BitVectorVisitor)
    }
}

/// Phase 6a: format-validation error returned by [`BitVector::from_mmap`].
///
/// Distinct from [`io::Error`] because format mismatches are a recoverable
/// classification problem (caller may try a different format version),
/// whereas I/O errors signal an unrelated underlying failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BitVectorFormatError {
    /// The provided byte buffer doesn't match the declared bit count.
    ByteLengthMismatch {
        /// Declared number of bits.
        len: usize,
        /// Bytes the caller should have provided.
        expected_bytes: usize,
        /// Bytes the caller actually provided.
        actual_bytes: usize,
    },
}

impl std::fmt::Display for BitVectorFormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ByteLengthMismatch {
                len,
                expected_bytes,
                actual_bytes,
            } => write!(
                f,
                "BitVector format mismatch: len={len} requires {expected_bytes} bytes (ceil(len/64)*8), got {actual_bytes}"
            ),
        }
    }
}

impl std::error::Error for BitVectorFormatError {}

/// Validates that `len` and `data` are consistent, returning a valid
/// `BitVector` or an error message.
fn validate_bitvec(len: usize, data: &[u64]) -> Result<BitVector, String> {
    let expected_words = len.div_ceil(64);
    if data.len() != expected_words {
        return Err(format!(
            "BitVector invariant violated: len={len} requires {expected_words} words, but data contains {} words",
            data.len()
        ));
    }
    Ok(BitVector {
        data: words_to_bytes(data),
        len,
    })
}

/// Encodes `words` as little-endian bytes wrapped in a refcounted `Bytes`.
fn words_to_bytes(words: &[u64]) -> Bytes {
    let mut buf = BytesMut::with_capacity(words.len() * 8);
    for &w in words {
        buf.extend_from_slice(&w.to_le_bytes());
    }
    buf.freeze()
}

impl BitVector {
    /// Reconstructs from pre-packed raw parts (legacy: `Vec<u64>` words).
    ///
    /// Used by section deserialization that holds words on the heap.
    /// Phase 3c will add [`from_bytes_storage`](Self::from_bytes_storage)
    /// for mmap-backed construction.
    #[must_use]
    pub fn from_raw_parts(data: Vec<u64>, len: usize) -> Self {
        Self {
            data: words_to_bytes(&data),
            len,
        }
    }

    /// Constructs from pre-encoded bytes (Phase 3c entry point).
    ///
    /// `data` must be `ceil(len / 64) * 8` bytes of little-endian
    /// `u64` words. Used by the mmap path so a column can hold a
    /// slice of mapped memory without copying.
    #[must_use]
    pub fn from_bytes_storage(data: Bytes, len: usize) -> Self {
        Self { data, len }
    }

    /// Phase 6a: zero-copy mmap constructor.
    ///
    /// Adopts a refcounted [`Bytes`] slice (typically produced by
    /// `Bytes::from_owner(mmap)` or a sub-slice thereof) as the
    /// backing storage without copying. The on-disk contract is
    /// little-endian `u64` words, with `len` total bits stored in
    /// `ceil(len / 64) * 8` bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the byte length doesn't match the expected
    /// `ceil(len / 64) * 8` for the declared bit count. This guards
    /// against truncated or oversized mmap regions.
    pub fn from_mmap(data: Bytes, len: usize) -> Result<Self, BitVectorFormatError> {
        let expected_bytes = len.div_ceil(64) * 8;
        if data.len() != expected_bytes {
            return Err(BitVectorFormatError::ByteLengthMismatch {
                len,
                expected_bytes,
                actual_bytes: data.len(),
            });
        }
        Ok(Self { data, len })
    }

    /// Creates an empty bit vector.
    #[must_use]
    pub fn new() -> Self {
        Self {
            data: Bytes::new(),
            len: 0,
        }
    }

    /// Creates a bit vector from a slice of booleans.
    ///
    /// Routes through a [`BitVectorBuilder`] internally; Phase 3b
    /// preserves the original convenience API for callers that already
    /// have the bools materialized.
    #[must_use]
    pub fn from_bools(bools: &[bool]) -> Self {
        let mut builder = BitVectorBuilder::with_capacity(bools.len());
        for &b in bools {
            builder.push(b);
        }
        builder.freeze()
    }

    /// Creates a bit vector with all bits set to the same value.
    #[must_use]
    pub fn filled(len: usize, value: bool) -> Self {
        BitVectorBuilder::filled(len, value).freeze()
    }

    /// Creates a bit vector with all bits set to false.
    #[must_use]
    pub fn zeros(len: usize) -> Self {
        Self::filled(len, false)
    }

    /// Creates a bit vector with all bits set to true.
    #[must_use]
    pub fn ones(len: usize) -> Self {
        Self::filled(len, true)
    }

    /// Returns the number of bits.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns whether the bit vector is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Gets the bit at the given index.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<bool> {
        if index >= self.len {
            return None;
        }
        let word_idx = index / 64;
        let bit_idx = index % 64;
        let word = self.word_at(word_idx)?;
        Some((word & (1 << bit_idx)) != 0)
    }

    /// Returns the number of `u64` words backing this bit vector.
    #[must_use]
    pub fn word_count(&self) -> usize {
        self.data.len() / 8
    }

    /// Returns the word at `idx`, or `None` if out of range.
    ///
    /// Reads via `from_le_bytes`; supports unaligned `Bytes` slices
    /// (e.g., mmap-backed sub-slices in Phase 3c).
    #[must_use]
    pub fn word_at(&self, idx: usize) -> Option<u64> {
        let start = idx.checked_mul(8)?;
        let end = start.checked_add(8)?;
        let chunk: [u8; 8] = self.data.get(start..end)?.try_into().ok()?;
        Some(u64::from_le_bytes(chunk))
    }

    /// Returns the raw byte storage.
    ///
    /// Phase 3c serializers use this to write the storage out directly
    /// (the on-disk format already matches our LE word layout).
    #[must_use]
    pub fn data_bytes(&self) -> &Bytes {
        &self.data
    }

    /// Returns the number of bits set to true.
    #[must_use]
    pub fn count_ones(&self) -> usize {
        if self.is_empty() {
            return 0;
        }
        let full_words = self.len / 64;
        let remaining_bits = self.len % 64;

        let mut count: usize = (0..full_words)
            .map(|i| self.word_at(i).unwrap_or(0).count_ones() as usize)
            .sum();

        if remaining_bits > 0
            && let Some(word) = self.word_at(full_words)
        {
            let mask = (1u64 << remaining_bits) - 1;
            count += (word & mask).count_ones() as usize;
        }

        count
    }

    /// Returns the number of bits set to false.
    #[must_use]
    pub fn count_zeros(&self) -> usize {
        self.len - self.count_ones()
    }

    /// Converts back to a `Vec<bool>`.
    ///
    /// # Panics
    ///
    /// Panics if internal storage is shorter than `len()` bits — an
    /// invariant violation that would indicate a bug in
    /// [`from_bytes_storage`](Self::from_bytes_storage) caller's
    /// data validation.
    #[must_use]
    pub fn to_bools(&self) -> Vec<bool> {
        (0..self.len)
            .map(|i| self.get(i).expect("index within len"))
            .collect()
    }

    /// Returns an iterator over the bits.
    ///
    /// # Panics
    ///
    /// Panics if internal storage is shorter than `len()` bits.
    pub fn iter(&self) -> impl Iterator<Item = bool> + '_ {
        (0..self.len).map(move |i| self.get(i).expect("index within len"))
    }

    /// Returns an iterator over indices where bits are true.
    ///
    /// # Panics
    ///
    /// Panics if internal storage is shorter than `len()` bits.
    pub fn ones_iter(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.len).filter(move |&i| self.get(i).expect("index within len"))
    }

    /// Returns an iterator over indices where bits are false.
    ///
    /// # Panics
    ///
    /// Panics if internal storage is shorter than `len()` bits.
    pub fn zeros_iter(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.len).filter(move |&i| !self.get(i).expect("index within len"))
    }

    /// Returns the compression ratio (original bytes / compressed bytes).
    #[must_use]
    pub fn compression_ratio(&self) -> f64 {
        if self.is_empty() {
            return 1.0;
        }
        let original_size = self.len;
        let compressed_size = self.data.len();
        if compressed_size == 0 {
            return 1.0;
        }
        original_size as f64 / compressed_size as f64
    }

    /// Performs bitwise AND with another bit vector.
    /// The result has the length of the shorter vector.
    #[must_use]
    pub fn and(&self, other: &Self) -> Self {
        let len = self.len.min(other.len);
        let num_words = len.div_ceil(64);
        let words: Vec<u64> = (0..num_words)
            .map(|i| self.word_at(i).unwrap_or(0) & other.word_at(i).unwrap_or(0))
            .collect();
        Self {
            data: words_to_bytes(&words),
            len,
        }
    }

    /// Performs bitwise OR with another bit vector.
    #[must_use]
    pub fn or(&self, other: &Self) -> Self {
        let len = self.len.min(other.len);
        let num_words = len.div_ceil(64);
        let words: Vec<u64> = (0..num_words)
            .map(|i| self.word_at(i).unwrap_or(0) | other.word_at(i).unwrap_or(0))
            .collect();
        Self {
            data: words_to_bytes(&words),
            len,
        }
    }

    /// Performs bitwise NOT.
    #[must_use]
    pub fn not(&self) -> Self {
        let num_words = self.word_count();
        let words: Vec<u64> = (0..num_words)
            .map(|i| !self.word_at(i).unwrap_or(0))
            .collect();
        Self {
            data: words_to_bytes(&words),
            len: self.len,
        }
    }

    /// Performs bitwise XOR with another bit vector.
    #[must_use]
    pub fn xor(&self, other: &Self) -> Self {
        let len = self.len.min(other.len);
        let num_words = len.div_ceil(64);
        let words: Vec<u64> = (0..num_words)
            .map(|i| self.word_at(i).unwrap_or(0) ^ other.word_at(i).unwrap_or(0))
            .collect();
        Self {
            data: words_to_bytes(&words),
            len,
        }
    }

    /// Serializes to bytes.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the bit-vector length exceeds `u32::MAX`.
    pub fn to_bytes(&self) -> io::Result<Vec<u8>> {
        let len_u32 = u32::try_from(self.len).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "BitVector length {} exceeds u32::MAX, cannot serialize",
                    self.len
                ),
            )
        })?;
        let mut buf = Vec::with_capacity(4 + self.data.len());
        buf.extend_from_slice(&len_u32.to_le_bytes());
        // Storage is already LE bytes — append directly.
        buf.extend_from_slice(&self.data);
        Ok(buf)
    }

    /// Deserializes from bytes.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the byte slice is too short or contains invalid data.
    pub fn from_bytes(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() < 4 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "BitVector too short",
            ));
        }

        let len = u32::from_le_bytes(
            bytes[0..4]
                .try_into()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
        ) as usize;
        let num_words = len.div_ceil(64);
        let needed = 4 + num_words * 8;

        if bytes.len() < needed {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "BitVector truncated",
            ));
        }

        Ok(Self {
            data: Bytes::copy_from_slice(&bytes[4..needed]),
            len,
        })
    }
}

impl Default for BitVector {
    fn default() -> Self {
        Self::new()
    }
}

impl FromIterator<bool> for BitVector {
    fn from_iter<T: IntoIterator<Item = bool>>(iter: T) -> Self {
        let mut builder = BitVectorBuilder::new();
        for b in iter {
            builder.push(b);
        }
        builder.freeze()
    }
}

// ── BitVectorBuilder ─────────────────────────────────────────────────

/// Mutable bit-vector builder. Word-level mutations stay cheap (`Vec<u64>`
/// indexed access); call [`freeze`](Self::freeze) to produce an immutable
/// [`BitVector`].
///
/// Mirrors the `BytesMut` → `Bytes` and Apache Arrow `BooleanBuilder` →
/// `BooleanArray` pattern.
#[derive(Debug, Clone)]
pub struct BitVectorBuilder {
    data: Vec<u64>,
    len: usize,
}

impl BitVectorBuilder {
    /// Creates an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self {
            data: Vec::new(),
            len: 0,
        }
    }

    /// Creates a builder with capacity for at least `bits` bits.
    #[must_use]
    pub fn with_capacity(bits: usize) -> Self {
        let words = bits.div_ceil(64);
        Self {
            data: Vec::with_capacity(words),
            len: 0,
        }
    }

    /// Creates a builder with all bits set to `value`, length `len`.
    #[must_use]
    pub fn filled(len: usize, value: bool) -> Self {
        let num_words = len.div_ceil(64);
        let fill = if value { u64::MAX } else { 0 };
        Self {
            data: vec![fill; num_words],
            len,
        }
    }

    /// Creates a builder with all bits set to false.
    #[must_use]
    pub fn zeros(len: usize) -> Self {
        Self::filled(len, false)
    }

    /// Creates a builder with all bits set to true.
    #[must_use]
    pub fn ones(len: usize) -> Self {
        Self::filled(len, true)
    }

    /// Returns the current bit length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns whether no bits have been pushed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Gets the bit at the given index (read access during build).
    #[must_use]
    pub fn get(&self, index: usize) -> Option<bool> {
        if index >= self.len {
            return None;
        }
        let word_idx = index / 64;
        let bit_idx = index % 64;
        Some((self.data[word_idx] & (1 << bit_idx)) != 0)
    }

    /// Sets the bit at the given index.
    ///
    /// # Panics
    ///
    /// Panics if `index >= len()`.
    pub fn set(&mut self, index: usize, value: bool) {
        assert!(index < self.len, "Index out of bounds");
        let word_idx = index / 64;
        let bit_idx = index % 64;
        if value {
            self.data[word_idx] |= 1 << bit_idx;
        } else {
            self.data[word_idx] &= !(1 << bit_idx);
        }
    }

    /// Appends a bit to the end.
    pub fn push(&mut self, value: bool) {
        let word_idx = self.len / 64;
        let bit_idx = self.len % 64;
        if word_idx >= self.data.len() {
            self.data.push(0);
        }
        if value {
            self.data[word_idx] |= 1 << bit_idx;
        }
        self.len += 1;
    }

    /// Freezes the builder into an immutable [`BitVector`].
    #[must_use]
    pub fn freeze(self) -> BitVector {
        BitVector {
            data: words_to_bytes(&self.data),
            len: self.len,
        }
    }
}

impl Default for BitVectorBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl FromIterator<bool> for BitVectorBuilder {
    fn from_iter<T: IntoIterator<Item = bool>>(iter: T) -> Self {
        let mut builder = BitVectorBuilder::new();
        for b in iter {
            builder.push(b);
        }
        builder
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bitvec_basic() {
        let bools = vec![true, false, true, true, false, false, true, false];
        let bitvec = BitVector::from_bools(&bools);

        assert_eq!(bitvec.len(), 8);
        for (i, &expected) in bools.iter().enumerate() {
            assert_eq!(bitvec.get(i), Some(expected));
        }
    }

    #[test]
    fn test_bitvec_empty() {
        let bitvec = BitVector::new();
        assert!(bitvec.is_empty());
        assert_eq!(bitvec.get(0), None);
    }

    #[test]
    fn test_bitvec_builder_push() {
        let mut builder = BitVectorBuilder::new();
        builder.push(true);
        builder.push(false);
        builder.push(true);
        let bitvec = builder.freeze();

        assert_eq!(bitvec.len(), 3);
        assert_eq!(bitvec.get(0), Some(true));
        assert_eq!(bitvec.get(1), Some(false));
        assert_eq!(bitvec.get(2), Some(true));
    }

    #[test]
    fn test_bitvec_builder_set() {
        let mut builder = BitVectorBuilder::zeros(8);

        builder.set(0, true);
        builder.set(3, true);
        builder.set(7, true);
        let bitvec = builder.freeze();

        assert_eq!(bitvec.get(0), Some(true));
        assert_eq!(bitvec.get(1), Some(false));
        assert_eq!(bitvec.get(3), Some(true));
        assert_eq!(bitvec.get(7), Some(true));
    }

    #[test]
    fn test_bitvec_count() {
        let bools = vec![true, false, true, true, false, false, true, false];
        let bitvec = BitVector::from_bools(&bools);

        assert_eq!(bitvec.count_ones(), 4);
        assert_eq!(bitvec.count_zeros(), 4);
    }

    #[test]
    fn test_bitvec_filled() {
        let zeros = BitVector::zeros(100);
        assert_eq!(zeros.count_ones(), 0);
        assert_eq!(zeros.count_zeros(), 100);

        let ones = BitVector::ones(100);
        assert_eq!(ones.count_ones(), 100);
        assert_eq!(ones.count_zeros(), 0);
    }

    #[test]
    fn test_bitvec_to_bools() {
        let original = vec![true, false, true, true, false];
        let bitvec = BitVector::from_bools(&original);
        let restored = bitvec.to_bools();
        assert_eq!(original, restored);
    }

    #[test]
    fn test_bitvec_large() {
        // Test with more than 64 bits
        let bools: Vec<bool> = (0..200).map(|i| i % 3 == 0).collect();
        let bitvec = BitVector::from_bools(&bools);

        assert_eq!(bitvec.len(), 200);
        for (i, &expected) in bools.iter().enumerate() {
            assert_eq!(bitvec.get(i), Some(expected), "Mismatch at index {}", i);
        }
    }

    #[test]
    fn test_bitvec_and() {
        let a = BitVector::from_bools(&[true, true, false, false]);
        let b = BitVector::from_bools(&[true, false, true, false]);
        let result = a.and(&b);

        assert_eq!(result.to_bools(), vec![true, false, false, false]);
    }

    #[test]
    fn test_bitvec_or() {
        let a = BitVector::from_bools(&[true, true, false, false]);
        let b = BitVector::from_bools(&[true, false, true, false]);
        let result = a.or(&b);

        assert_eq!(result.to_bools(), vec![true, true, true, false]);
    }

    #[test]
    fn test_bitvec_not() {
        let a = BitVector::from_bools(&[true, false, true, false]);
        let result = a.not();

        // Note: NOT inverts all bits in the word, so we check the relevant bits
        assert_eq!(result.get(0), Some(false));
        assert_eq!(result.get(1), Some(true));
        assert_eq!(result.get(2), Some(false));
        assert_eq!(result.get(3), Some(true));
    }

    #[test]
    fn test_bitvec_xor() {
        let a = BitVector::from_bools(&[true, true, false, false]);
        let b = BitVector::from_bools(&[true, false, true, false]);
        let result = a.xor(&b);

        assert_eq!(result.to_bools(), vec![false, true, true, false]);
    }

    #[test]
    fn test_bitvec_serialization() {
        let bools = vec![true, false, true, true, false, false, true, false];
        let bitvec = BitVector::from_bools(&bools);
        let bytes = bitvec.to_bytes().unwrap();
        let restored = BitVector::from_bytes(&bytes).unwrap();
        assert_eq!(bitvec, restored);
    }

    #[test]
    fn test_bitvec_compression_ratio() {
        let bitvec = BitVector::zeros(64);
        let ratio = bitvec.compression_ratio();
        // 64 bools = 64 bytes original, 8 bytes compressed = 8x
        assert!((ratio - 8.0).abs() < 0.1);
    }

    #[test]
    fn test_bitvec_ones_iter() {
        let bools = vec![true, false, true, true, false];
        let bitvec = BitVector::from_bools(&bools);
        let ones: Vec<usize> = bitvec.ones_iter().collect();
        assert_eq!(ones, vec![0, 2, 3]);
    }

    #[test]
    fn test_bitvec_zeros_iter() {
        let bools = vec![true, false, true, true, false];
        let bitvec = BitVector::from_bools(&bools);
        let zeros: Vec<usize> = bitvec.zeros_iter().collect();
        assert_eq!(zeros, vec![1, 4]);
    }

    #[test]
    fn test_bitvec_from_iter() {
        let bitvec: BitVector = vec![true, false, true].into_iter().collect();
        assert_eq!(bitvec.len(), 3);
        assert_eq!(bitvec.get(0), Some(true));
        assert_eq!(bitvec.get(1), Some(false));
        assert_eq!(bitvec.get(2), Some(true));
    }

    #[test]
    fn test_bitvec_deserialize_roundtrip() {
        let bools = vec![true, false, true, true, false, false, true, false];
        let original = BitVector::from_bools(&bools);
        let json = serde_json::to_string(&original).unwrap();
        let restored: BitVector = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn test_bitvec_deserialize_invalid_len_too_large() {
        // len=200 requires ceil(200/64) = 4 words, but we only provide 1
        let json = r#"{"data":[42],"len":200}"#;
        let result: Result<BitVector, _> = serde_json::from_str(json);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("invariant violated"),
            "expected invariant error, got: {err_msg}"
        );
    }

    #[test]
    fn test_bitvec_deserialize_invalid_len_data_mismatch() {
        // len=10 requires ceil(10/64) = 1 word, but we provide 3
        let json = r#"{"data":[1,2,3],"len":10}"#;
        let result: Result<BitVector, _> = serde_json::from_str(json);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("invariant violated"),
            "expected invariant error, got: {err_msg}"
        );
    }

    #[test]
    fn test_bitvec_deserialize_valid_edge_cases() {
        // len=0 with empty data
        let json = r#"{"data":[],"len":0}"#;
        let bv: BitVector = serde_json::from_str(json).unwrap();
        assert_eq!(bv.len(), 0);
        assert!(bv.is_empty());

        // len=1 with one u64
        let json = r#"{"data":[1],"len":1}"#;
        let bv: BitVector = serde_json::from_str(json).unwrap();
        assert_eq!(bv.len(), 1);
        assert_eq!(bv.get(0), Some(true));

        // len=64 with one u64 (exactly fills one word)
        let json = r#"{"data":[18446744073709551615],"len":64}"#;
        let bv: BitVector = serde_json::from_str(json).unwrap();
        assert_eq!(bv.len(), 64);
        assert_eq!(bv.count_ones(), 64);
    }

    // ── Phase 3b: Bytes-backed storage ────────────────────────────────

    #[test]
    fn test_bitvec_word_at_returns_words_from_bytes() {
        // Pattern: alternating bits → 0xAAAAAAAAAAAAAAAA in word 0.
        let bools: Vec<bool> = (0..64).map(|i| i % 2 == 1).collect();
        let bv = BitVector::from_bools(&bools);
        assert_eq!(bv.word_count(), 1);
        assert_eq!(bv.word_at(0), Some(0xAAAA_AAAA_AAAA_AAAA));
    }

    #[test]
    fn test_bitvec_word_at_out_of_range_returns_none() {
        let bv = BitVector::from_bools(&[true, false, true]);
        assert!(bv.word_at(bv.word_count()).is_none());
    }

    #[test]
    fn test_bitvec_data_bytes_length_matches_word_count() {
        let bools: Vec<bool> = (0..200).map(|i| i % 3 == 0).collect();
        let bv = BitVector::from_bools(&bools);
        assert_eq!(bv.data_bytes().len(), bv.word_count() * 8);
        // Round-trip via get(): every bit recoverable.
        for (i, &b) in bools.iter().enumerate() {
            assert_eq!(bv.get(i), Some(b));
        }
    }

    #[test]
    fn test_bitvec_serde_round_trip_with_bytes_storage() {
        let bools: Vec<bool> = (0..130).map(|i| i % 7 == 0).collect();
        let bv = BitVector::from_bools(&bools);
        // Serialize to JSON, deserialize back: the on-disk shape stays
        // `Vec<u64>` even though internal storage is now `Bytes`.
        let json = serde_json::to_string(&bv).unwrap();
        let bv2: BitVector = serde_json::from_str(&json).unwrap();
        assert_eq!(bv.len(), bv2.len());
        for (i, &b) in bools.iter().enumerate() {
            assert_eq!(bv2.get(i), Some(b));
        }
    }

    // ── Phase 6a: from_mmap zero-copy constructor ─────────────────────

    /// Round-trip via `from_mmap`: take an existing BitVector's bytes,
    /// adopt them as a fresh BitVector, verify all bits.
    #[test]
    fn alix_from_mmap_round_trips_via_data_bytes() {
        let bools: Vec<bool> = (0..200).map(|i| i % 5 == 0).collect();
        let original = BitVector::from_bools(&bools);
        let bytes = original.data_bytes().clone();
        let mmapped = BitVector::from_mmap(bytes, original.len()).expect("from_mmap");
        assert_eq!(mmapped.len(), original.len());
        for (i, &b) in bools.iter().enumerate() {
            assert_eq!(mmapped.get(i), Some(b), "bit {i}");
        }
        // word-by-word equivalence locks the LE contract.
        for i in 0..original.word_count() {
            assert_eq!(mmapped.word_at(i), original.word_at(i));
        }
    }

    /// Truncated buffer is rejected — guards against partial mmap regions.
    #[test]
    fn gus_from_mmap_rejects_short_buffer() {
        // len=200 needs ceil(200/64)*8 = 32 bytes; provide only 16.
        let short = bytes::Bytes::from(vec![0u8; 16]);
        let result = BitVector::from_mmap(short, 200);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(
            err,
            BitVectorFormatError::ByteLengthMismatch {
                len: 200,
                expected_bytes: 32,
                actual_bytes: 16,
            }
        ));
    }

    /// Oversized buffer is also rejected — symmetric guard.
    #[test]
    fn vincent_from_mmap_rejects_long_buffer() {
        // len=10 needs ceil(10/64)*8 = 8 bytes; provide 16.
        let long = bytes::Bytes::from(vec![0u8; 16]);
        let result = BitVector::from_mmap(long, 10);
        assert!(result.is_err());
    }

    /// Empty bitvector via from_mmap — len=0, 0 bytes.
    #[test]
    fn jules_from_mmap_handles_empty() {
        let empty = bytes::Bytes::new();
        let bv = BitVector::from_mmap(empty, 0).expect("empty mmap");
        assert_eq!(bv.len(), 0);
        assert!(bv.is_empty());
    }

    /// Zero-copy assertion: mmap-backed BitVector shares the underlying
    /// allocation with the source `Bytes` (no heap copy).
    ///
    /// We verify by holding both Arcs and asserting `Bytes::as_ptr` returns
    /// the same address, then mutating one side is impossible (Bytes is
    /// immutable) — the test confirms shape, not write-isolation.
    #[test]
    fn mia_from_mmap_is_zero_copy() {
        let original = BitVector::from_bools(&[true; 1024]);
        let source_bytes = original.data_bytes().clone();
        let source_ptr = source_bytes.as_ptr();

        let mmapped = BitVector::from_mmap(source_bytes, 1024).expect("from_mmap");
        // The mmapped bitvector's storage points at the same address —
        // no allocation occurred.
        assert_eq!(
            mmapped.data_bytes().as_ptr(),
            source_ptr,
            "from_mmap must NOT allocate; Bytes refcount sharing required"
        );
    }

    /// LE word contract: a hand-crafted LE byte pattern decodes to the
    /// expected u64 words. Guards against accidental endianness drift.
    #[test]
    fn shosanna_from_mmap_locks_le_word_contract() {
        // Bytes [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08] in LE
        // decode to 0x0807060504030201.
        let bytes = bytes::Bytes::from(vec![
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, // word 0
            0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, // word 1: all-ones
        ]);
        let bv = BitVector::from_mmap(bytes, 128).expect("from_mmap");
        assert_eq!(bv.word_at(0), Some(0x0807_0605_0403_0201));
        assert_eq!(bv.word_at(1), Some(0xFFFF_FFFF_FFFF_FFFF));
    }
}
