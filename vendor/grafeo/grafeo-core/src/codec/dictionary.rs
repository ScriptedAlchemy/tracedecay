//! Dictionary encoding for repeated strings.
//!
//! If your data has lots of repeated strings (like node labels or edge types),
//! dictionary encoding stores each unique string once and references it by a
//! small integer code. A million "Person" labels becomes one string + a million
//! 4-byte codes instead of a million strings.
//!
//! # Example
//!
//! ```no_run
//! # use grafeo_core::codec::dictionary::DictionaryBuilder;
//! let mut builder = DictionaryBuilder::new();
//! builder.add("Person");
//! builder.add("Company");
//! builder.add("Person");  // same as first - reuses code 0
//! builder.add("Person");  // reuses code 0 again
//!
//! let dict = builder.build();
//! // Dictionary: ["Person", "Company"]
//! // Codes:      [0, 1, 0, 0]
//! assert_eq!(dict.dictionary_size(), 2);  // Only 2 unique strings stored
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use bytes::{Bytes, BytesMut};

/// Reads an LE u32 at byte offset `idx * 4`. Returns `None` if out of range.
#[inline]
fn read_code_at(bytes: &Bytes, idx: usize) -> Option<u32> {
    let start = idx.checked_mul(4)?;
    let end = start.checked_add(4)?;
    let chunk: [u8; 4] = bytes.get(start..end)?.try_into().ok()?;
    Some(u32::from_le_bytes(chunk))
}

/// Encodes `codes` as LE u32 bytes wrapped in a refcounted `Bytes`.
fn codes_to_bytes(codes: &[u32]) -> Bytes {
    let mut buf = BytesMut::with_capacity(codes.len() * 4);
    for &c in codes {
        buf.extend_from_slice(&c.to_le_bytes());
    }
    buf.freeze()
}

/// Reads a single bit from a u64-word bitmap encoded as LE bytes.
#[inline]
fn null_bit_at(bitmap: &Bytes, index: usize) -> bool {
    let word_idx = index / 64;
    let bit_idx = index % 64;
    let start = word_idx * 8;
    let Some(chunk) = bitmap.get(start..start + 8) else {
        return false;
    };
    let Ok(arr): Result<[u8; 8], _> = chunk.try_into() else {
        return false;
    };
    (u64::from_le_bytes(arr) & (1 << bit_idx)) != 0
}

/// Two-variant backing for u32 dictionary codes.
#[derive(Debug, Clone)]
enum CodeStore {
    Inline(Vec<u32>),
    Mapped(Bytes),
}

impl CodeStore {
    #[inline]
    fn as_slice(&self) -> Option<&[u32]> {
        match self {
            Self::Inline(v) => Some(v.as_slice()),
            Self::Mapped(_) => None,
        }
    }

    #[inline]
    fn code_at(&self, idx: usize) -> Option<u32> {
        match self {
            Self::Inline(v) => v.get(idx).copied(),
            Self::Mapped(b) => read_code_at(b, idx),
        }
    }

    fn to_bytes(&self) -> Bytes {
        match self {
            Self::Inline(v) => codes_to_bytes(v),
            Self::Mapped(b) => b.clone(),
        }
    }

    fn byte_len(&self) -> usize {
        match self {
            Self::Inline(v) => v.len() * 4,
            Self::Mapped(b) => b.len(),
        }
    }

    #[inline]
    fn len_codes(&self, code_count: usize) -> usize {
        match self {
            Self::Inline(v) => v.len(),
            Self::Mapped(_) => code_count,
        }
    }
}

/// Two-variant backing for the LE-u64 null bitmap.
#[derive(Debug, Clone)]
enum NullBitmap {
    Inline(Vec<u64>),
    Mapped(Bytes),
}

impl NullBitmap {
    #[inline]
    fn as_slice(&self) -> Option<&[u64]> {
        match self {
            Self::Inline(v) => Some(v.as_slice()),
            Self::Mapped(_) => None,
        }
    }

    #[inline]
    fn is_null(&self, index: usize) -> bool {
        match self {
            Self::Inline(words) => {
                let word_idx = index / 64;
                let bit_idx = index % 64;
                words
                    .get(word_idx)
                    .is_some_and(|w| (*w & (1u64 << bit_idx)) != 0)
            }
            Self::Mapped(b) => null_bit_at(b, index),
        }
    }
}

/// Stores repeated strings efficiently by referencing them with integer codes.
///
/// Each unique string appears once in the dictionary. Values are stored as
/// LE u32 indices pointing into that dictionary, refcounted as
/// [`bytes::Bytes`] so heap-owned and mmap-backed columns share the same
/// type (revised D7).
#[derive(Debug, Clone)]
pub struct DictionaryEncoding {
    /// The dictionary of unique strings.
    dictionary: Arc<[Arc<str>]>,
    /// Encoded values: `Inline(Vec<u32>)` or `Mapped(Bytes)`.
    codes: CodeStore,
    /// Number of code values.
    code_count: usize,
    /// Optional null bitmap: `Inline(Vec<u64>)` or `Mapped(Bytes)`.
    null_bitmap: Option<NullBitmap>,
}

impl DictionaryEncoding {
    /// Creates a new dictionary encoding from a dictionary and codes (legacy
    /// `Vec<u32>` input).
    pub fn new(dictionary: Arc<[Arc<str>]>, codes: Vec<u32>) -> Self {
        let code_count = codes.len();
        Self {
            dictionary,
            codes: CodeStore::Inline(codes),
            code_count,
            null_bitmap: None,
        }
    }

    /// Constructs a dictionary encoding from pre-encoded bytes (Phase 3c
    /// entry point).
    ///
    /// `codes_bytes` must be `code_count * 4` bytes of LE u32 values.
    pub fn from_bytes_storage(
        dictionary: Arc<[Arc<str>]>,
        codes_bytes: Bytes,
        code_count: usize,
    ) -> Self {
        Self {
            dictionary,
            codes: CodeStore::Mapped(codes_bytes),
            code_count,
            null_bitmap: None,
        }
    }

    /// Adds a null bitmap to this encoding (legacy `Vec<u64>` input).
    pub fn with_nulls(mut self, null_bitmap: Vec<u64>) -> Self {
        self.null_bitmap = Some(NullBitmap::Inline(null_bitmap));
        self
    }

    /// Adds a pre-encoded null bitmap (Phase 3c entry point).
    pub fn with_null_bytes(mut self, null_bitmap: Bytes) -> Self {
        self.null_bitmap = Some(NullBitmap::Mapped(null_bitmap));
        self
    }

    /// Returns the number of values.
    pub fn len(&self) -> usize {
        self.codes.len_codes(self.code_count)
    }

    /// Returns whether the encoding is empty.
    pub fn is_empty(&self) -> bool {
        self.codes.len_codes(self.code_count) == 0
    }

    /// Returns the number of unique strings in the dictionary.
    pub fn dictionary_size(&self) -> usize {
        self.dictionary.len()
    }

    /// Returns the dictionary.
    pub fn dictionary(&self) -> &Arc<[Arc<str>]> {
        &self.dictionary
    }

    /// Returns the encoded codes as raw LE u32 bytes (always materialised).
    ///
    /// Phase 3b: codes storage is `bytes::Bytes`. Use [`Self::code_at`] for
    /// indexed access; this returns the raw byte storage for serializers
    /// that write the storage out directly.
    #[must_use]
    pub fn codes_bytes(&self) -> Bytes {
        self.codes.to_bytes()
    }

    /// Returns a direct `&[u32]` slice when the codes live in RAM.
    #[must_use]
    #[inline]
    pub fn as_codes_slice(&self) -> Option<&[u32]> {
        self.codes.as_slice()
    }

    /// Returns a direct `&[u64]` view of the null bitmap when it lives in
    /// RAM. `None` when there is no null bitmap or the bitmap is mmap-backed.
    #[must_use]
    #[inline]
    pub fn as_null_words_slice(&self) -> Option<&[u64]> {
        self.null_bitmap.as_ref().and_then(NullBitmap::as_slice)
    }

    /// Number of u32 codes stored.
    pub fn code_count(&self) -> usize {
        self.code_count
    }

    /// Returns the code at `idx`, or `None` if out of range.
    #[must_use]
    #[inline]
    pub fn code_at(&self, idx: usize) -> Option<u32> {
        self.codes.code_at(idx)
    }

    /// Returns the codes as a materialized `Vec<u32>` (allocates).
    ///
    /// Prefer [`Self::code_at`] or [`Self::code_count`] for reads. This exists
    /// for callers that need a contiguous slice and accept the allocation
    /// (e.g., legacy serialization paths).
    pub fn codes(&self) -> Vec<u32> {
        (0..self.code_count)
            .map(|i| self.codes.code_at(i).unwrap_or(0))
            .collect()
    }

    /// Returns whether the value at index is null.
    #[must_use]
    #[inline]
    pub fn is_null(&self, index: usize) -> bool {
        match &self.null_bitmap {
            Some(b) => b.is_null(index),
            None => false,
        }
    }

    /// Returns the string value at the given index.
    ///
    /// Returns `None` if the value is null.
    pub fn get(&self, index: usize) -> Option<&str> {
        if self.is_null(index) {
            return None;
        }
        let code = self.code_at(index)?;
        self.dictionary.get(code as usize).map(|s| s.as_ref())
    }

    /// Returns the code at the given index.
    pub fn get_code(&self, index: usize) -> Option<u32> {
        if self.is_null(index) {
            return None;
        }
        self.code_at(index)
    }

    /// Iterates over all values, yielding `Option<&str>`.
    pub fn iter(&self) -> impl Iterator<Item = Option<&str>> {
        (0..self.len()).map(move |i| self.get(i))
    }

    /// Returns the compression ratio (original size / compressed size).
    pub fn compression_ratio(&self) -> f64 {
        if self.is_empty() {
            return 1.0;
        }

        // Estimate original size: sum of string lengths
        let original_size: usize = (0..self.code_count)
            .map(|i| {
                let code = self.codes.code_at(i).unwrap_or(0) as usize;
                self.dictionary.get(code).map_or(0, |s| s.len())
            })
            .sum();

        // Compressed size: dictionary + codes
        let dict_size: usize = self.dictionary.iter().map(|s| s.len()).sum();
        let codes_size = self.codes.byte_len();
        let compressed_size = dict_size + codes_size;

        if compressed_size == 0 {
            return 1.0;
        }

        original_size as f64 / compressed_size as f64
    }

    /// Encodes a lookup value into a code, if it exists in the dictionary.
    pub fn encode(&self, value: &str) -> Option<u32> {
        self.dictionary
            .iter()
            .position(|s| s.as_ref() == value)
            .and_then(|i| u32::try_from(i).ok())
    }

    /// Returns the row offsets where the code matches `predicate` and the
    /// row is not null.
    ///
    /// Branches once on the storage variants so the per-row body is a tight
    /// loop over native slices in the common in-memory case.
    pub fn filter_by_code(&self, predicate: impl Fn(u32) -> bool) -> Vec<usize> {
        if self.code_count == 0 {
            return Vec::new();
        }

        let null_words: Option<&[u64]> = self.as_null_words_slice();

        match (self.codes.as_slice(), null_words, &self.null_bitmap) {
            // Hot path: codes inline, no nulls.
            (Some(codes), _, None) => codes
                .iter()
                .enumerate()
                .filter_map(|(i, &c)| predicate(c).then_some(i))
                .collect(),

            // Hot path: codes inline, null bitmap inline.
            (Some(codes), Some(nulls), Some(_)) => codes
                .iter()
                .enumerate()
                .filter_map(|(i, &c)| {
                    let word = nulls.get(i / 64).copied().unwrap_or(0);
                    let is_null = (word & (1u64 << (i % 64))) != 0;
                    (!is_null && predicate(c)).then_some(i)
                })
                .collect(),

            // Mixed / mmap path: fall back to per-element decode.
            _ => {
                let mut out = Vec::new();
                for i in 0..self.code_count {
                    if self.is_null(i) {
                        continue;
                    }
                    if let Some(c) = self.codes.code_at(i)
                        && predicate(c)
                    {
                        out.push(i);
                    }
                }
                out
            }
        }
    }
}

/// Builds a dictionary encoding by streaming values through.
///
/// Call [`add()`](Self::add) for each value - we'll automatically assign codes
/// and build the dictionary. Then [`build()`](Self::build) to get the final encoding.
#[derive(Debug)]
pub struct DictionaryBuilder {
    /// Map from string to code.
    string_to_code: HashMap<Arc<str>, u32>,
    /// Dictionary (code -> string).
    dictionary: Vec<Arc<str>>,
    /// Encoded values.
    codes: Vec<u32>,
    /// Null positions (for marking nulls).
    null_positions: Vec<usize>,
}

impl DictionaryBuilder {
    /// Creates a new dictionary builder.
    pub fn new() -> Self {
        Self {
            string_to_code: HashMap::new(),
            dictionary: Vec::new(),
            codes: Vec::new(),
            null_positions: Vec::new(),
        }
    }

    /// Creates a new dictionary builder with estimated capacity.
    pub fn with_capacity(value_capacity: usize, dictionary_capacity: usize) -> Self {
        Self {
            string_to_code: HashMap::with_capacity(dictionary_capacity),
            dictionary: Vec::with_capacity(dictionary_capacity),
            codes: Vec::with_capacity(value_capacity),
            null_positions: Vec::new(),
        }
    }

    /// Adds a string value to the encoding.
    ///
    /// Returns the code assigned to this value.
    pub fn add(&mut self, value: &str) -> u32 {
        if let Some(&code) = self.string_to_code.get(value) {
            self.codes.push(code);
            code
        } else {
            // reason: dictionary size is bounded by u32 (codes are u32)
            #[allow(clippy::cast_possible_truncation)]
            let code = self.dictionary.len() as u32;
            let arc_value: Arc<str> = value.into();
            self.string_to_code.insert(arc_value.clone(), code);
            self.dictionary.push(arc_value);
            self.codes.push(code);
            code
        }
    }

    /// Adds a null value.
    pub fn add_null(&mut self) {
        let idx = self.codes.len();
        self.null_positions.push(idx);
        self.codes.push(0); // Placeholder code
    }

    /// Adds an optional value.
    pub fn add_optional(&mut self, value: Option<&str>) -> Option<u32> {
        match value {
            Some(v) => Some(self.add(v)),
            None => {
                self.add_null();
                None
            }
        }
    }

    /// Returns the current number of values.
    pub fn len(&self) -> usize {
        self.codes.len()
    }

    /// Returns whether the builder is empty.
    pub fn is_empty(&self) -> bool {
        self.codes.is_empty()
    }

    /// Returns the current dictionary size.
    pub fn dictionary_size(&self) -> usize {
        self.dictionary.len()
    }

    /// Builds the dictionary encoding.
    pub fn build(self) -> DictionaryEncoding {
        let null_bitmap = if self.null_positions.is_empty() {
            None
        } else {
            let num_words = (self.codes.len() + 63) / 64;
            let mut bitmap = vec![0u64; num_words];
            for &pos in &self.null_positions {
                let word_idx = pos / 64;
                let bit_idx = pos % 64;
                bitmap[word_idx] |= 1 << bit_idx;
            }
            Some(bitmap)
        };

        let dict: Arc<[Arc<str>]> = self.dictionary.into();

        let mut encoding = DictionaryEncoding::new(dict, self.codes);
        if let Some(bitmap) = null_bitmap {
            encoding = encoding.with_nulls(bitmap);
        }
        encoding
    }

    /// Clears the builder for reuse.
    pub fn clear(&mut self) {
        self.string_to_code.clear();
        self.dictionary.clear();
        self.codes.clear();
        self.null_positions.clear();
    }
}

impl Default for DictionaryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Extension trait for building dictionary encodings from iterators.
pub trait IntoDictionaryEncoding {
    /// Creates a dictionary encoding from an iterator of strings.
    fn into_dictionary_encoding(self) -> DictionaryEncoding;
}

impl<'a, I> IntoDictionaryEncoding for I
where
    I: IntoIterator<Item = &'a str>,
{
    fn into_dictionary_encoding(self) -> DictionaryEncoding {
        let mut builder = DictionaryBuilder::new();
        for s in self {
            builder.add(s);
        }
        builder.build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dictionary_builder_basic() {
        let mut builder = DictionaryBuilder::new();
        builder.add("apple");
        builder.add("banana");
        builder.add("apple");
        builder.add("cherry");
        builder.add("apple");

        let dict = builder.build();

        assert_eq!(dict.len(), 5);
        assert_eq!(dict.dictionary_size(), 3);

        assert_eq!(dict.get(0), Some("apple"));
        assert_eq!(dict.get(1), Some("banana"));
        assert_eq!(dict.get(2), Some("apple"));
        assert_eq!(dict.get(3), Some("cherry"));
        assert_eq!(dict.get(4), Some("apple"));
    }

    #[test]
    fn test_dictionary_codes() {
        let mut builder = DictionaryBuilder::new();
        let code_apple = builder.add("apple");
        let code_banana = builder.add("banana");
        let code_apple2 = builder.add("apple");

        assert_eq!(code_apple, code_apple2);
        assert_ne!(code_apple, code_banana);

        let dict = builder.build();
        assert_eq!(dict.codes(), vec![0, 1, 0]);
    }

    #[test]
    fn test_dictionary_with_nulls() {
        let mut builder = DictionaryBuilder::new();
        builder.add("apple");
        builder.add_null();
        builder.add("banana");
        builder.add_null();

        let dict = builder.build();

        assert_eq!(dict.len(), 4);
        assert_eq!(dict.get(0), Some("apple"));
        assert_eq!(dict.get(1), None);
        assert!(dict.is_null(1));
        assert_eq!(dict.get(2), Some("banana"));
        assert_eq!(dict.get(3), None);
        assert!(dict.is_null(3));
    }

    #[test]
    fn test_dictionary_encode_lookup() {
        let mut builder = DictionaryBuilder::new();
        builder.add("apple");
        builder.add("banana");
        builder.add("cherry");

        let dict = builder.build();

        assert_eq!(dict.encode("apple"), Some(0));
        assert_eq!(dict.encode("banana"), Some(1));
        assert_eq!(dict.encode("cherry"), Some(2));
        assert_eq!(dict.encode("date"), None);
    }

    #[test]
    fn test_dictionary_filter_by_code() {
        let mut builder = DictionaryBuilder::new();
        builder.add("apple");
        builder.add("banana");
        builder.add("apple");
        builder.add("cherry");
        builder.add("apple");

        let dict = builder.build();
        let apple_code = dict.encode("apple").unwrap();

        let indices = dict.filter_by_code(|code| code == apple_code);
        assert_eq!(indices, vec![0, 2, 4]);
    }

    #[test]
    fn test_compression_ratio() {
        let mut builder = DictionaryBuilder::new();

        // Add many repeated long strings
        for _ in 0..100 {
            builder.add("this_is_a_very_long_string_that_repeats_many_times");
        }

        let dict = builder.build();

        // Compression ratio should be > 1 for highly repetitive data
        let ratio = dict.compression_ratio();
        assert!(ratio > 1.0, "Expected compression ratio > 1, got {}", ratio);
    }

    #[test]
    fn test_into_dictionary_encoding() {
        let strings = vec!["apple", "banana", "apple", "cherry"];
        let dict: DictionaryEncoding = strings.into_iter().into_dictionary_encoding();

        assert_eq!(dict.len(), 4);
        assert_eq!(dict.dictionary_size(), 3);
    }

    #[test]
    fn test_empty_dictionary() {
        let builder = DictionaryBuilder::new();
        let dict = builder.build();

        assert!(dict.is_empty());
        assert_eq!(dict.dictionary_size(), 0);
        assert_eq!(dict.get(0), None);
    }

    #[test]
    fn test_single_value() {
        let mut builder = DictionaryBuilder::new();
        builder.add("only_value");

        let dict = builder.build();

        assert_eq!(dict.len(), 1);
        assert_eq!(dict.dictionary_size(), 1);
        assert_eq!(dict.get(0), Some("only_value"));
    }

    #[test]
    fn test_all_unique() {
        let mut builder = DictionaryBuilder::new();
        builder.add("a");
        builder.add("b");
        builder.add("c");
        builder.add("d");

        let dict = builder.build();

        assert_eq!(dict.len(), 4);
        assert_eq!(dict.dictionary_size(), 4);
        assert_eq!(dict.codes(), vec![0, 1, 2, 3]);
    }

    #[test]
    fn test_all_same() {
        let mut builder = DictionaryBuilder::new();
        for _ in 0..10 {
            builder.add("same");
        }

        let dict = builder.build();

        assert_eq!(dict.len(), 10);
        assert_eq!(dict.dictionary_size(), 1);
        assert!(dict.codes().iter().all(|&c| c == 0));
    }

    #[test]
    fn test_dict_inline_codes_slice_via_builder() {
        let mut b = DictionaryBuilder::new();
        for s in ["alpha", "beta", "alpha", "gamma", "beta"] {
            b.add(s);
        }
        let dict = b.build();

        let slice = dict.as_codes_slice();
        assert!(
            slice.is_some(),
            "DictionaryBuilder must produce Inline codes for fast scan"
        );
        let codes = slice.unwrap();
        assert_eq!(codes.len(), 5);
        assert_eq!(codes[0], codes[2]); // both "alpha"
        assert_eq!(codes[1], codes[4]); // both "beta"
    }

    #[test]
    fn test_dict_mapped_codes_yield_no_slice() {
        let dictionary: Arc<[Arc<str>]> = Arc::from(vec![Arc::from("a"), Arc::from("b")]);
        let codes_le: Vec<u8> = [0u32, 1, 0, 1]
            .iter()
            .flat_map(|c| c.to_le_bytes())
            .collect();
        let dict =
            DictionaryEncoding::from_bytes_storage(dictionary, bytes::Bytes::from(codes_le), 4);
        assert!(dict.as_codes_slice().is_none());
        assert_eq!(dict.code_at(0), Some(0));
        assert_eq!(dict.code_at(3), Some(1));
    }

    #[test]
    fn test_dict_inline_null_words_slice_via_with_nulls() {
        let mut b = DictionaryBuilder::new();
        for s in ["x", "y", "z"] {
            b.add(s);
        }
        let dict = b.build().with_nulls(vec![0b0000_0010_u64]); // index 1 is null

        assert!(
            dict.as_null_words_slice().is_some(),
            "with_nulls(Vec<u64>) must produce Inline null bitmap"
        );
        assert!(!dict.is_null(0));
        assert!(dict.is_null(1));
        assert!(!dict.is_null(2));
    }

    #[test]
    fn test_dict_filter_by_code_matches_between_inline_and_mapped() {
        let strings = ["a", "b", "c", "a", "a", "b", "c", "a", "b", "a"];
        let mut b = DictionaryBuilder::new();
        for s in strings {
            b.add(s);
        }
        let inline = b.build();

        let codes_b = inline.codes_bytes();
        let dict_arc = inline.dictionary().clone();
        let mapped = DictionaryEncoding::from_bytes_storage(dict_arc, codes_b, strings.len());

        let target = inline.encode("a").unwrap();
        let from_inline = inline.filter_by_code(|c| c == target);
        let from_mapped = mapped.filter_by_code(|c| c == target);
        assert_eq!(from_inline, from_mapped);
        assert_eq!(from_inline, vec![0, 3, 4, 7, 9]);
    }

    #[test]
    fn test_dict_filter_by_code_respects_inline_null_bitmap() {
        let mut b = DictionaryBuilder::new();
        for s in ["x", "y", "x", "y"] {
            b.add(s);
        }
        // Mark index 2 ("x") as null.
        let dict = b.build().with_nulls(vec![0b0000_0100_u64]);
        let target = dict.encode("x").unwrap();
        let hits = dict.filter_by_code(|c| c == target);
        assert_eq!(hits, vec![0]); // index 2 excluded by null bit
    }

    #[test]
    fn test_dict_filter_by_code_fallback_with_mapped_codes_and_mapped_nulls() {
        let dictionary: Arc<[Arc<str>]> = Arc::from(vec![Arc::from("a"), Arc::from("b")]);

        // Codes: a, b, a, b, a -> 0, 1, 0, 1, 0
        let codes_le: Vec<u8> = [0u32, 1, 0, 1, 0]
            .iter()
            .flat_map(|c| c.to_le_bytes())
            .collect();
        let codes_bytes = bytes::Bytes::from(codes_le);

        // Null bitmap as LE u64 bytes: mark index 2 (the third "a") as null.
        // 0b0000_0100 -> bit 2 set
        let null_le: Vec<u8> = 0b0000_0100_u64.to_le_bytes().to_vec();
        let null_bytes = bytes::Bytes::from(null_le);

        let dict = DictionaryEncoding::from_bytes_storage(dictionary, codes_bytes, 5)
            .with_null_bytes(null_bytes);

        let target_a = dict.encode("a").expect("'a' is in dictionary");
        // "a" is at indices 0, 2, 4. Index 2 is null. So result should be [0, 4].
        let hits = dict.filter_by_code(|c| c == target_a);
        assert_eq!(hits, vec![0, 4]);
    }
}
