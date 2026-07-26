//! Size observability read models and soft budgets (Plan 38 §7).
//!
//! Per-store size, per-table growth, and free-page ratio are first-class,
//! cheap-to-query telemetry. The raw numbers come from `PRAGMA page_count`,
//! `PRAGMA freelist_count`, `PRAGMA page_size`, and the `dbstat` virtual table.
//! Those reads live behind [`StoreSizeTelemetryPort`], whose implementation is
//! owned by the storage runtime (see the module docs on the port). This module
//! owns only the typed read models, the budget contract, and the pure
//! projections over them. A budget overage is always observable — never a silent
//! result — via [`StoreSizeBudgetV1::evaluate`].

use std::future::Future;
use std::pin::Pin;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::UtcMicros;

use crate::error::ApplicationContractError;

use super::identity::{FreePageRatioV1, StorageByteSizeV1, StoreKeyV1, TableNameV1};

/// One cheap size sample for a single store, derived from page-count pragmas.
///
/// `total_bytes` is `page_count * page_size`; `free_bytes` is
/// `freelist_pages * page_size`. Both are recorded so the free-page ratio and
/// the reclaimable-bytes estimate (Plan 38 §6 compaction) can be computed
/// without re-reading the store.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StoreSizeSampleV1 {
    pub store: StoreKeyV1,
    pub page_size_bytes: u32,
    pub page_count: u64,
    pub freelist_pages: u64,
    pub observed_at: UtcMicros,
}

impl StoreSizeSampleV1 {
    /// Validate the sample. A store with pages must have a non-zero page size,
    /// and the freelist can never exceed the total page count.
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.page_count > 0 && self.page_size_bytes == 0 {
            return Err(ApplicationContractError::ZeroValue {
                field: "storage sample page size",
            });
        }
        if self.freelist_pages > self.page_count {
            return Err(ApplicationContractError::InvalidRange {
                field: "storage sample freelist pages",
            });
        }
        Ok(())
    }

    #[must_use]
    pub fn total_bytes(&self) -> StorageByteSizeV1 {
        StorageByteSizeV1(
            self.page_count
                .saturating_mul(u64::from(self.page_size_bytes)),
        )
    }

    #[must_use]
    pub fn free_bytes(&self) -> StorageByteSizeV1 {
        StorageByteSizeV1(
            self.freelist_pages
                .saturating_mul(u64::from(self.page_size_bytes)),
        )
    }

    #[must_use]
    pub fn free_page_ratio(&self) -> FreePageRatioV1 {
        FreePageRatioV1::from_pages(self.freelist_pages, self.page_count)
    }
}

/// Per-table growth between two watermarks, derived from `dbstat` payload bytes.
///
/// `previous_bytes` is the byte total at the prior watermark; `current_bytes` is
/// the total now. The delta feeds retention/backlog reasoning; a table that only
/// ever grows (append-only evidence stores, per Plan 38 §3) is the signal that
/// motivates a retention window.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TableGrowthSampleV1 {
    pub store: StoreKeyV1,
    pub table: TableNameV1,
    pub previous_bytes: StorageByteSizeV1,
    pub current_bytes: StorageByteSizeV1,
    pub previous_observed_at: UtcMicros,
    pub current_observed_at: UtcMicros,
}

impl TableGrowthSampleV1 {
    /// Validate ordering: the current watermark must not precede the previous.
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.current_observed_at.0 < self.previous_observed_at.0 {
            return Err(ApplicationContractError::InvalidRange {
                field: "storage table growth watermark",
            });
        }
        Ok(())
    }

    /// Net growth in bytes since the previous watermark (saturating at zero; a
    /// shrink reports zero growth rather than a negative number).
    #[must_use]
    pub fn growth_bytes(&self) -> StorageByteSizeV1 {
        self.current_bytes.saturating_sub(self.previous_bytes)
    }

    #[must_use]
    pub fn is_growing(&self) -> bool {
        self.current_bytes > self.previous_bytes
    }
}

/// An owner-configured soft size budget for one store.
///
/// Exceeding the soft limit is a finding, never a silent state (Plan 38 §7).
/// The budget is *soft*: it drives a Doctor `OverBudgetStore` finding, not a
/// hard write rejection.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StoreSizeBudgetV1 {
    pub store: StoreKeyV1,
    pub soft_limit_bytes: StorageByteSizeV1,
}

impl StoreSizeBudgetV1 {
    /// Validate the budget. A zero soft limit is meaningless (every store would
    /// be perpetually over budget) and is rejected.
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.soft_limit_bytes.get() == 0 {
            return Err(ApplicationContractError::ZeroValue {
                field: "storage soft budget limit",
            });
        }
        Ok(())
    }

    /// Evaluate a size sample against this budget. The budget and sample must
    /// name the same store; a mismatch is a contract error rather than a silent
    /// pass. Never returns "within budget" for an oversized store.
    pub fn evaluate(
        &self,
        sample: &StoreSizeSampleV1,
    ) -> Result<StoreBudgetEvaluationV1, ApplicationContractError> {
        self.validate()?;
        sample.validate()?;
        if self.store != sample.store {
            return Err(ApplicationContractError::Inconsistent {
                field: "storage budget store mismatch",
            });
        }
        let total = sample.total_bytes();
        if total.get() > self.soft_limit_bytes.get() {
            Ok(StoreBudgetEvaluationV1::OverBudget {
                observed: total,
                soft_limit: self.soft_limit_bytes,
                overage: total.saturating_sub(self.soft_limit_bytes),
            })
        } else {
            Ok(StoreBudgetEvaluationV1::WithinBudget {
                observed: total,
                soft_limit: self.soft_limit_bytes,
            })
        }
    }
}

/// The outcome of evaluating a store size against its soft budget.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum StoreBudgetEvaluationV1 {
    WithinBudget {
        observed: StorageByteSizeV1,
        soft_limit: StorageByteSizeV1,
    },
    OverBudget {
        observed: StorageByteSizeV1,
        soft_limit: StorageByteSizeV1,
        overage: StorageByteSizeV1,
    },
}

impl StoreBudgetEvaluationV1 {
    #[must_use]
    pub const fn is_over_budget(&self) -> bool {
        matches!(self, Self::OverBudget { .. })
    }
}

/// The typed result of one telemetry read attempt.
///
/// The port is *total*: it never fails silently into a healthy or empty result.
/// A platform that cannot query `dbstat`/pragmas reports [`Self::Unsupported`];
/// a denied read reports [`Self::Denied`]; an undetermined read reports
/// [`Self::Unknown`]. Each maps to a distinct, honest Doctor evidence state.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum StorageTelemetryReadV1 {
    /// A size sample was observed.
    Observed { sample: StoreSizeSampleV1 },
    /// The runtime cannot expose page-count telemetry on this build/platform.
    Unsupported { store: StoreKeyV1 },
    /// Authorization to read the store's telemetry was denied.
    Denied { store: StoreKeyV1 },
    /// The telemetry state could not be determined.
    Unknown { store: StoreKeyV1 },
}

impl StorageTelemetryReadV1 {
    #[must_use]
    pub fn store(&self) -> &StoreKeyV1 {
        match self {
            Self::Observed { sample } => &sample.store,
            Self::Unsupported { store } | Self::Denied { store } | Self::Unknown { store } => store,
        }
    }
}

/// Boxed future returned by [`StoreSizeTelemetryPort`], mirroring the diagnostic
/// provider port convention (std `Future`, no extra runtime dependency).
pub type StorageTelemetryFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Transport-neutral port for cheap per-store size telemetry.
///
/// # Implementation seam
///
/// The implementation is owned by the storage runtime crate
/// (`tracedecay-rusqlite-runtime`), which holds the reader lease and can issue
/// `PRAGMA page_count` / `PRAGMA freelist_count` / `PRAGMA page_size` and query
/// the `dbstat` virtual table. This crate (fenced out of the runtime) defines
/// only the trait and the read models; the runtime adapter constructs
/// [`StoreSizeSampleV1`] / [`TableGrowthSampleV1`] and returns
/// [`StorageTelemetryReadV1`]. The pragmas are O(1) header reads and `dbstat`
/// aggregation is cheap, satisfying the "cheap to query" contract.
pub trait StoreSizeTelemetryPort {
    /// Read one cheap size sample for `store`.
    fn store_size<'a>(
        &'a self,
        context: &'a crate::RequestContext,
        store: &'a StoreKeyV1,
    ) -> StorageTelemetryFuture<'a, StorageTelemetryReadV1>;

    /// Read per-table growth samples for `store` between the runtime's retained
    /// watermarks. An empty vector means the store carries no per-table history
    /// yet, not that growth is zero.
    fn table_growth<'a>(
        &'a self,
        context: &'a crate::RequestContext,
        store: &'a StoreKeyV1,
    ) -> StorageTelemetryFuture<'a, Vec<TableGrowthSampleV1>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> StoreKeyV1 {
        StoreKeyV1::new("sessions.db").expect("valid store key")
    }

    fn sample(page_count: u64, freelist_pages: u64) -> StoreSizeSampleV1 {
        StoreSizeSampleV1 {
            store: store(),
            page_size_bytes: 4096,
            page_count,
            freelist_pages,
            observed_at: UtcMicros(1_000),
        }
    }

    #[test]
    fn sample_computes_totals_and_free_bytes() {
        let sample = sample(100, 25);
        assert_eq!(sample.total_bytes(), StorageByteSizeV1(409_600));
        assert_eq!(sample.free_bytes(), StorageByteSizeV1(102_400));
        assert!((sample.free_page_ratio().as_f64() - 0.25).abs() < f64::EPSILON);
    }

    #[test]
    fn sample_rejects_freelist_larger_than_page_count() {
        assert_eq!(
            sample(10, 11).validate().expect_err("freelist too large"),
            ApplicationContractError::InvalidRange {
                field: "storage sample freelist pages"
            }
        );
    }

    #[test]
    fn budget_over_and_within_are_distinct() {
        let budget = StoreSizeBudgetV1 {
            store: store(),
            soft_limit_bytes: StorageByteSizeV1(300_000),
        };
        // 100 pages * 4096 = 409_600 > 300_000 => over budget.
        let over = budget.evaluate(&sample(100, 0)).expect("evaluated");
        assert!(over.is_over_budget());
        assert_eq!(
            over,
            StoreBudgetEvaluationV1::OverBudget {
                observed: StorageByteSizeV1(409_600),
                soft_limit: StorageByteSizeV1(300_000),
                overage: StorageByteSizeV1(109_600),
            }
        );
        // 10 pages * 4096 = 40_960 < 300_000 => within budget.
        let within = budget.evaluate(&sample(10, 0)).expect("evaluated");
        assert!(!within.is_over_budget());
    }

    #[test]
    fn budget_rejects_store_mismatch() {
        let budget = StoreSizeBudgetV1 {
            store: StoreKeyV1::new("graph.db").expect("valid"),
            soft_limit_bytes: StorageByteSizeV1(10),
        };
        assert_eq!(
            budget.evaluate(&sample(1, 0)).expect_err("mismatch"),
            ApplicationContractError::Inconsistent {
                field: "storage budget store mismatch"
            }
        );
    }

    #[test]
    fn budget_rejects_zero_soft_limit() {
        let budget = StoreSizeBudgetV1 {
            store: store(),
            soft_limit_bytes: StorageByteSizeV1::ZERO,
        };
        assert_eq!(
            budget.validate().expect_err("zero limit"),
            ApplicationContractError::ZeroValue {
                field: "storage soft budget limit"
            }
        );
    }

    #[test]
    fn table_growth_reports_saturating_delta() {
        let table = TableNameV1::new("observations").expect("valid table");
        let growing = TableGrowthSampleV1 {
            store: store(),
            table: table.clone(),
            previous_bytes: StorageByteSizeV1(1_000),
            current_bytes: StorageByteSizeV1(1_800),
            previous_observed_at: UtcMicros(1),
            current_observed_at: UtcMicros(2),
        };
        assert!(growing.is_growing());
        assert_eq!(growing.growth_bytes(), StorageByteSizeV1(800));

        let shrunk = TableGrowthSampleV1 {
            current_bytes: StorageByteSizeV1(500),
            ..growing
        };
        assert!(!shrunk.is_growing());
        assert_eq!(shrunk.growth_bytes(), StorageByteSizeV1::ZERO);
    }

    #[test]
    fn telemetry_read_exposes_store_for_every_variant() {
        assert_eq!(
            StorageTelemetryReadV1::Unsupported { store: store() }.store(),
            &store()
        );
        assert_eq!(
            StorageTelemetryReadV1::Observed {
                sample: sample(1, 0)
            }
            .store(),
            &store()
        );
    }
}
