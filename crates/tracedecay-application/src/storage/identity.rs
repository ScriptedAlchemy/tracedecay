//! Bounded identity and measurement primitives for the storage retention read
//! models (Plan 38 §5–§7).
//!
//! These types are transport-neutral value objects. They carry no store,
//! runtime, or path capability; a [`StoreKeyV1`] names a store *logically* (for
//! example `sessions.db` or `branches/feature-x`) so read models and Doctor
//! producers can reference it without embedding an on-disk path or a filesystem
//! effect.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::ApplicationContractError;
use crate::identity::application_identifier;

application_identifier!(
    @no_conversions
    /// Logical name of one owner-profile store (for example `sessions.db`,
    /// `graph.db`, or `branches/feature-x`). Never an absolute on-disk path.
    StoreKeyV1 => ("storage store key", 256),
    /// A physical table name inside a store, used for per-table growth telemetry.
    TableNameV1 => ("storage table name", 128),
    /// A store-relative path to an incident-debris artifact (for example
    /// `sessions.db.corrupt-1721692800`). Store-relative, never absolute.
    RelativeArtifactPathV1 => ("storage relative artifact path", 512),
    /// The single logical quarantine location debris is collected into. A
    /// store-relative directory name, never an absolute path.
    QuarantineLocationV1 => ("storage quarantine location", 256),
);

/// A byte size measurement. A newtype keeps sizes from being confused with
/// counts, ratios, or timestamps in the read models and producers.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(transparent)]
pub struct StorageByteSizeV1(pub u64);

impl StorageByteSizeV1 {
    /// Zero bytes.
    pub const ZERO: Self = Self(0);

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Saturating difference, never underflowing below zero bytes.
    #[must_use]
    pub const fn saturating_sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }
}

/// A free-page ratio in the closed interval `[0.0, 1.0]`.
///
/// The ratio is `freelist_pages / page_count`. Construction clamps the inputs so
/// a malformed sample can never yield a ratio outside the unit interval or a
/// division by zero.
#[derive(Clone, Copy, Debug, Serialize, PartialEq)]
#[serde(transparent)]
pub struct FreePageRatioV1(f64);

impl FreePageRatioV1 {
    /// Compute the ratio from a freelist-page count and a total page count. A
    /// zero page count yields a zero ratio (an empty store carries no bloat),
    /// and the result is clamped into `[0.0, 1.0]`.
    #[must_use]
    pub fn from_pages(freelist_pages: u64, page_count: u64) -> Self {
        if page_count == 0 {
            return Self(0.0);
        }
        let ratio = (freelist_pages as f64) / (page_count as f64);
        Self(ratio.clamp(0.0, 1.0))
    }

    #[must_use]
    pub const fn as_f64(self) -> f64 {
        self.0
    }

    /// True when this ratio meets or exceeds `threshold`.
    #[must_use]
    pub fn at_or_above(self, threshold: FreePageRatioV1) -> bool {
        self.0 >= threshold.0
    }

    /// Validate and construct a ratio directly (for thresholds). Must be finite
    /// and within `[0.0, 1.0]`.
    pub fn new(value: f64) -> Result<Self, ApplicationContractError> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(ApplicationContractError::InvalidRange {
                field: "storage free page ratio",
            });
        }
        Ok(Self(value))
    }
}

impl Eq for FreePageRatioV1 {}

impl<'de> Deserialize<'de> for FreePageRatioV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(f64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_key_rejects_empty_untrimmed_and_control() {
        assert!(StoreKeyV1::new("").is_err());
        assert!(StoreKeyV1::new(" leading").is_err());
        assert!(StoreKeyV1::new("ctrl\u{0}char").is_err());
        assert_eq!(
            StoreKeyV1::new("sessions.db").expect("valid").as_str(),
            "sessions.db"
        );
    }

    #[test]
    fn free_page_ratio_zero_page_count_is_zero() {
        assert_eq!(FreePageRatioV1::from_pages(10, 0).as_f64(), 0.0);
    }

    #[test]
    fn free_page_ratio_clamps_and_computes() {
        let ratio = FreePageRatioV1::from_pages(1, 4);
        assert!((ratio.as_f64() - 0.25).abs() < f64::EPSILON);
        // freelist larger than page count is malformed; clamp to 1.0.
        assert_eq!(FreePageRatioV1::from_pages(10, 4).as_f64(), 1.0);
    }

    #[test]
    fn free_page_ratio_new_rejects_out_of_range() {
        assert!(FreePageRatioV1::new(-0.1).is_err());
        assert!(FreePageRatioV1::new(1.5).is_err());
        assert!(FreePageRatioV1::new(f64::NAN).is_err());
        assert!(FreePageRatioV1::new(0.5).is_ok());
    }

    #[test]
    fn free_page_ratio_at_or_above_threshold() {
        let sample = FreePageRatioV1::from_pages(1, 4);
        let threshold = FreePageRatioV1::new(0.25).expect("valid");
        assert!(sample.at_or_above(threshold));
        let lower = FreePageRatioV1::from_pages(1, 5);
        assert!(!lower.at_or_above(threshold));
    }

    #[test]
    fn byte_size_saturating_sub_never_underflows() {
        assert_eq!(
            StorageByteSizeV1(3).saturating_sub(StorageByteSizeV1(10)),
            StorageByteSizeV1::ZERO
        );
        assert_eq!(
            StorageByteSizeV1(10).saturating_sub(StorageByteSizeV1(3)),
            StorageByteSizeV1(7)
        );
    }
}
