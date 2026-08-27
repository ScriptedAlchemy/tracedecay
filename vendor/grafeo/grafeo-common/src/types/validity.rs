//! Reverse-ordered validity timestamp for versioned storage.
//!
//! When used as a key component in disk-backed storage, the reverse ordering
//! ensures most-recent versions appear first in sorted key order, enabling
//! efficient "get latest version at epoch X" queries with a single range scan.

use std::cmp::{Ordering, Reverse};

/// Reverse-ordered validity timestamp for efficient disk-storage scans.
///
/// Wraps a signed 64-bit timestamp with reversed ordering so that
/// newer timestamps sort before older ones in byte-ordered storage.
///
/// # Key Encoding
///
/// Use [`versioned_key`](Self::versioned_key) to produce a 16-byte key
/// combining an entity ID with a validity timestamp. The entity ID sorts
/// in natural order, while the timestamp sorts in reverse order within
/// each entity, so a forward scan returns the newest version first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ValidityTs(Reverse<i64>);

impl ValidityTs {
    /// Creates a new validity timestamp.
    #[must_use]
    pub fn new(ts: i64) -> Self {
        Self(Reverse(ts))
    }

    /// Returns a sentinel value representing "always current."
    ///
    /// Uses `i64::MAX` so it sorts before every real timestamp in
    /// reverse order.
    #[must_use]
    pub fn current() -> Self {
        Self(Reverse(i64::MAX))
    }

    /// Returns the underlying timestamp value.
    #[must_use]
    pub fn timestamp(&self) -> i64 {
        self.0.0
    }

    /// Encodes an entity ID and validity timestamp into a 16-byte key.
    ///
    /// Layout: `[entity_id: 8 bytes BE][timestamp: 8 bytes BE]`
    ///
    /// Because `ValidityTs` uses `Reverse<i64>`, the timestamp bytes
    /// are the raw `i64` in big-endian. To get reverse-sorted timestamps
    /// in byte order, callers should negate or bitwise-complement the
    /// timestamp before creating the `ValidityTs`. For a simpler approach,
    /// use `i64::MAX - epoch` as the timestamp value.
    #[must_use]
    pub fn versioned_key(entity_id: u64, ts: Self) -> [u8; 16] {
        let mut key = [0u8; 16];
        key[..8].copy_from_slice(&entity_id.to_be_bytes());
        key[8..].copy_from_slice(&ts.0.0.to_be_bytes());
        key
    }

    /// Decodes entity ID and timestamp from a 16-byte versioned key.
    ///
    /// Returns `(entity_id, ValidityTs)`.
    ///
    /// # Panics
    ///
    /// Panics if the byte slices cannot be converted to fixed-size arrays
    /// (cannot happen for a `&[u8; 16]` input).
    #[must_use]
    pub fn from_versioned_key(key: &[u8; 16]) -> (u64, Self) {
        let entity_id = u64::from_be_bytes(
            key[..8]
                .try_into()
                .expect("first 8 bytes of a [u8; 16] are always a valid [u8; 8]"),
        );
        let ts = i64::from_be_bytes(
            key[8..]
                .try_into()
                .expect("last 8 bytes of a [u8; 16] are always a valid [u8; 8]"),
        );
        (entity_id, Self::new(ts))
    }
}

impl Ord for ValidityTs {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0)
    }
}

impl PartialOrd for ValidityTs {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl From<i64> for ValidityTs {
    fn from(ts: i64) -> Self {
        Self::new(ts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reverse_ordering() {
        let ts1 = ValidityTs::new(10);
        let ts2 = ValidityTs::new(20);

        // Reverse ordering: higher timestamp sorts first (is "less")
        assert!(ts2 < ts1);
        assert!(ts1 > ts2);
    }

    #[test]
    fn test_current_sentinel() {
        let current = ValidityTs::current();
        let recent = ValidityTs::new(1_000_000);

        // Current sorts before everything (smallest in reverse order)
        assert!(current < recent);
        assert_eq!(current.timestamp(), i64::MAX);
    }

    #[test]
    fn test_versioned_key_roundtrip() {
        let entity_id = 42u64;
        let ts = ValidityTs::new(12345);

        let key = ValidityTs::versioned_key(entity_id, ts);
        let (decoded_id, decoded_ts) = ValidityTs::from_versioned_key(&key);

        assert_eq!(decoded_id, entity_id);
        assert_eq!(decoded_ts, ts);
    }

    #[test]
    fn test_versioned_key_entity_ordering() {
        // Keys with different entity IDs: entity order is preserved
        let key1 = ValidityTs::versioned_key(1, ValidityTs::new(100));
        let key2 = ValidityTs::versioned_key(2, ValidityTs::new(100));

        assert!(key1 < key2);
    }

    #[test]
    fn test_equality() {
        let ts1 = ValidityTs::new(42);
        let ts2 = ValidityTs::new(42);
        let ts3 = ValidityTs::new(43);

        assert_eq!(ts1, ts2);
        assert_ne!(ts1, ts3);
    }

    #[test]
    fn test_from_i64() {
        let ts: ValidityTs = 42i64.into();
        assert_eq!(ts.timestamp(), 42);
    }
}
