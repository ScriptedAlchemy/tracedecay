//! Compaction policy (Plan 38 §6).
//!
//! Stores accumulate unreclaimed free pages. This module decides *whether* an
//! incremental vacuum should be scheduled — never *when* it runs on the hot
//! path. The policy is a pure function of a size sample and a free-page-ratio
//! threshold. Placement is structurally constrained to a deferred background
//! lane ([`CompactionPlacementV1`]) so a compaction can never be scheduled to
//! compete with foreground writes (Plan 38 non-goal). This module owns no
//! scheduler and enacts nothing; it emits a typed decision the daemon consumes.

use serde::{Deserialize, Serialize};

use crate::error::ApplicationContractError;

use super::identity::{FreePageRatioV1, StorageByteSizeV1};
use super::telemetry::StoreSizeSampleV1;

/// The only placement a compaction may be scheduled into.
///
/// There is deliberately no "foreground" or "inline" variant: the type system
/// forbids expressing a compaction that competes with foreground writes. The
/// enum exists (rather than a bare marker) so a future off-hot-path lane can be
/// added through a versioned variant without widening this one's meaning.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CompactionPlacementV1 {
    /// A deferred, daemon-owned background lane, off the hot path, that yields
    /// to foreground writers.
    DeferredBackground,
}

/// The compaction trigger policy: a free-page-ratio threshold plus a floor on
/// reclaimable bytes so a tiny-but-fragmented store is not vacuumed pointlessly.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompactionTriggerPolicyV1 {
    /// Free-page ratio at or above which compaction becomes eligible.
    pub free_page_ratio_threshold: FreePageRatioV1,
    /// Minimum reclaimable free bytes below which compaction is not worth it.
    pub minimum_reclaimable_bytes: StorageByteSizeV1,
}

impl CompactionTriggerPolicyV1 {
    /// Validate the policy. A zero threshold would schedule compaction for every
    /// store on every pass; it is rejected in favor of an explicit positive
    /// ratio.
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.free_page_ratio_threshold.as_f64() <= 0.0 {
            return Err(ApplicationContractError::ZeroValue {
                field: "compaction free page ratio threshold",
            });
        }
        Ok(())
    }

    /// Decide whether to schedule incremental vacuum for a store from its size
    /// sample. Eligibility requires *both* the ratio threshold and the
    /// reclaimable-bytes floor, so fragmentation alone on a trivially small
    /// store is never scheduled.
    pub fn decide(
        &self,
        sample: &StoreSizeSampleV1,
    ) -> Result<CompactionDecisionV1, ApplicationContractError> {
        self.validate()?;
        sample.validate()?;
        let ratio = sample.free_page_ratio();
        let reclaimable = sample.free_bytes();
        let ratio_met = ratio.at_or_above(self.free_page_ratio_threshold);
        let bytes_met = reclaimable.get() >= self.minimum_reclaimable_bytes.get();
        if ratio_met && bytes_met {
            Ok(CompactionDecisionV1::ScheduleIncrementalVacuum {
                placement: CompactionPlacementV1::DeferredBackground,
                observed_free_page_ratio: ratio,
                reclaimable_bytes: reclaimable,
            })
        } else {
            Ok(CompactionDecisionV1::NotEligible {
                observed_free_page_ratio: ratio,
                reclaimable_bytes: reclaimable,
            })
        }
    }
}

/// The typed outcome of a compaction decision.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "decision")]
pub enum CompactionDecisionV1 {
    /// The store is below threshold or below the reclaimable floor.
    NotEligible {
        observed_free_page_ratio: FreePageRatioV1,
        reclaimable_bytes: StorageByteSizeV1,
    },
    /// Schedule an incremental vacuum in the deferred background lane.
    ScheduleIncrementalVacuum {
        placement: CompactionPlacementV1,
        observed_free_page_ratio: FreePageRatioV1,
        reclaimable_bytes: StorageByteSizeV1,
    },
}

impl CompactionDecisionV1 {
    #[must_use]
    pub const fn is_scheduled(&self) -> bool {
        matches!(self, Self::ScheduleIncrementalVacuum { .. })
    }

    /// The placement, if scheduled. Always the deferred background lane by
    /// construction — foreground placement is unrepresentable.
    #[must_use]
    pub const fn placement(&self) -> Option<CompactionPlacementV1> {
        match self {
            Self::ScheduleIncrementalVacuum { placement, .. } => Some(*placement),
            Self::NotEligible { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::identity::StoreKeyV1;
    use tracedecay_domain::UtcMicros;

    fn sample(page_count: u64, freelist_pages: u64) -> StoreSizeSampleV1 {
        StoreSizeSampleV1 {
            store: StoreKeyV1::new("graph.db").expect("valid"),
            page_size_bytes: 4096,
            page_count,
            freelist_pages,
            observed_at: UtcMicros(1),
        }
    }

    fn policy(ratio: f64, min_bytes: u64) -> CompactionTriggerPolicyV1 {
        CompactionTriggerPolicyV1 {
            free_page_ratio_threshold: FreePageRatioV1::new(ratio).expect("valid ratio"),
            minimum_reclaimable_bytes: StorageByteSizeV1(min_bytes),
        }
    }

    #[test]
    fn schedules_when_ratio_and_bytes_met() {
        // 100 pages, 30 freelist => ratio 0.30, free bytes 122_880.
        let decision = policy(0.25, 100_000)
            .decide(&sample(100, 30))
            .expect("decided");
        assert!(decision.is_scheduled());
        assert_eq!(
            decision.placement(),
            Some(CompactionPlacementV1::DeferredBackground)
        );
    }

    #[test]
    fn not_eligible_when_below_ratio() {
        let decision = policy(0.50, 0).decide(&sample(100, 30)).expect("decided");
        assert!(!decision.is_scheduled());
        assert!(decision.placement().is_none());
    }

    #[test]
    fn not_eligible_when_below_reclaimable_floor() {
        // ratio 0.30 meets 0.25, but free bytes 122_880 < 1_000_000 floor.
        let decision = policy(0.25, 1_000_000)
            .decide(&sample(100, 30))
            .expect("decided");
        assert!(!decision.is_scheduled());
    }

    #[test]
    fn rejects_zero_threshold() {
        let bad = CompactionTriggerPolicyV1 {
            free_page_ratio_threshold: FreePageRatioV1::new(0.0).expect("valid"),
            minimum_reclaimable_bytes: StorageByteSizeV1(1),
        };
        assert_eq!(
            bad.validate().expect_err("zero threshold"),
            ApplicationContractError::ZeroValue {
                field: "compaction free page ratio threshold"
            }
        );
    }
}
