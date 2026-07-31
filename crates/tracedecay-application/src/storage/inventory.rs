//! Retention inventory read models (Plan 38 §2, §3, and the branch lifecycle of
//! §1) that feed the remaining Storage Doctor finding kinds.
//!
//! These are the minimal typed observations the Doctor producers need to raise
//! `OrphanStore`, `StaleBranchDbs`, and `RetentionBacklog` findings. They carry
//! only the observed facts (identity resolution, live-ref presence, past-window
//! bytes); the collection itself is owned by the daemon storage runtime.

use serde::{Deserialize, Serialize};
use tracedecay_domain::UtcMicros;

use crate::error::ApplicationContractError;

use super::identity::{BranchRefV1, StorageByteSizeV1, StoreKeyV1, TableNameV1};

/// A store whose project identity no longer resolves to a live repository root
/// (identity-drift orphan, Plan 38 §2), reported with age and size.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OrphanStoreRecordV1 {
    pub store: StoreKeyV1,
    /// Whether the store's project identity still resolves to a live root.
    pub identity_resolves: bool,
    pub size_bytes: StorageByteSizeV1,
    /// When the store was first observed as unresolved.
    pub first_unresolved_at: UtcMicros,
    /// The current observation watermark, used to compute age.
    pub observed_at: UtcMicros,
}

impl OrphanStoreRecordV1 {
    /// Validate ordering of the observation watermarks.
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.observed_at.0 < self.first_unresolved_at.0 {
            return Err(ApplicationContractError::InvalidRange {
                field: "orphan store observation watermark",
            });
        }
        Ok(())
    }

    /// True when the store is an identity-drift orphan (identity does not
    /// resolve).
    #[must_use]
    pub fn is_orphan(&self) -> bool {
        !self.identity_resolves
    }

    /// Age in micros since the store was first seen unresolved (saturating).
    #[must_use]
    pub fn age_micros(&self) -> i64 {
        self.observed_at
            .0
            .saturating_sub(self.first_unresolved_at.0)
    }
}

/// A branch-scoped store whose git ref state is observed against live refs
/// (Plan 38 §1 branch lifecycle).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StaleBranchDbRecordV1 {
    pub store: StoreKeyV1,
    pub branch: BranchRefV1,
    /// Whether the branch ref still exists in the live repository.
    pub ref_present: bool,
    pub size_bytes: StorageByteSizeV1,
}

impl StaleBranchDbRecordV1 {
    /// True when the branch DB is stale: its ref is gone.
    #[must_use]
    pub fn is_stale(&self) -> bool {
        !self.ref_present
    }
}

/// Exact code-generation retention census. `superseded_*` reports every sealed
/// generation except the active pointer target; `collectable_*` is the subset
/// outside the vector-readable live set and rollback floor.
///
/// `stranded_scope_*` counts a disjoint storage class one level up: whole
/// `code-index-v1/<scope>/` directories whose canonical project root no longer
/// exists. They are not superseded generations of *this* scope — they are bytes
/// no scope-local census can reach at all — so they are reported alongside the
/// generation totals rather than folded into them.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeGenerationRetentionRecordV1 {
    pub store: StoreKeyV1,
    pub superseded_generation_count: u64,
    pub superseded_generation_bytes: StorageByteSizeV1,
    pub collectable_generation_count: u64,
    pub collectable_generation_bytes: StorageByteSizeV1,
    /// Scope roots under the shared `code-index-v1/` parent that no live
    /// canonical project root names. Absent (zero) when the reporter could not
    /// prove the live-root set, which is also when nothing may be collected.
    #[serde(default)]
    pub stranded_scope_count: u64,
    #[serde(default = "zero_storage_bytes")]
    pub stranded_scope_bytes: StorageByteSizeV1,
}

/// `serde(default)` needs a value, and `StorageByteSizeV1` deliberately has no
/// `Default` impl; zero bytes is the only meaningful absence here.
fn zero_storage_bytes() -> StorageByteSizeV1 {
    StorageByteSizeV1::ZERO
}

impl CodeGenerationRetentionRecordV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.collectable_generation_count > self.superseded_generation_count
            || self.collectable_generation_bytes.get() > self.superseded_generation_bytes.get()
            || (self.superseded_generation_count == 0 && self.superseded_generation_bytes.get() > 0)
            || (self.collectable_generation_count == 0
                && self.collectable_generation_bytes.get() > 0)
            // Same invariant one level up: bytes are never reported without the
            // scopes that hold them.
            || (self.stranded_scope_count == 0 && self.stranded_scope_bytes.get() > 0)
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "code generation retention totals",
            });
        }
        Ok(())
    }

    #[must_use]
    pub fn has_collectable_generations(&self) -> bool {
        self.collectable_generation_count > 0 || self.collectable_generation_bytes.get() > 0
    }

    /// True when whole scope roots are unreachable by any scope-local retention
    /// pass. This is a storage problem even when the generation census inside
    /// the live scope is perfectly clean.
    #[must_use]
    pub fn has_stranded_scopes(&self) -> bool {
        self.stranded_scope_count > 0
    }
}

/// A retention-eligible slice of a store: rows or tables past their configured
/// window awaiting offload/collection (Plan 38 §3).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetentionBacklogRecordV1 {
    pub store: StoreKeyV1,
    pub table: TableNameV1,
    /// Bytes held by rows already past the retention window.
    pub past_window_bytes: StorageByteSizeV1,
    /// The oldest past-window row's timestamp (how far behind the watermark).
    pub oldest_past_window_at: UtcMicros,
    /// The retention-window watermark: rows older than this are eligible.
    pub window_watermark_at: UtcMicros,
}

impl RetentionBacklogRecordV1 {
    /// Validate that the oldest past-window row is not newer than the watermark
    /// (that would mean there is no backlog to report).
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.has_backlog() && self.oldest_past_window_at.0 >= self.window_watermark_at.0 {
            return Err(ApplicationContractError::Inconsistent {
                field: "retention backlog watermark",
            });
        }
        Ok(())
    }

    /// True when there are bytes past the retention window awaiting collection.
    #[must_use]
    pub fn has_backlog(&self) -> bool {
        self.past_window_bytes.get() > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> StoreKeyV1 {
        StoreKeyV1::new("graph.db").expect("valid")
    }

    #[test]
    fn orphan_detects_unresolved_identity_and_age() {
        let record = OrphanStoreRecordV1 {
            store: store(),
            identity_resolves: false,
            size_bytes: StorageByteSizeV1(1_000),
            first_unresolved_at: UtcMicros(100),
            observed_at: UtcMicros(400),
        };
        assert!(record.is_orphan());
        assert_eq!(record.age_micros(), 300);
        assert!(record.validate().is_ok());
    }

    #[test]
    fn orphan_resolved_identity_is_not_orphan() {
        let record = OrphanStoreRecordV1 {
            store: store(),
            identity_resolves: true,
            size_bytes: StorageByteSizeV1(1_000),
            first_unresolved_at: UtcMicros(100),
            observed_at: UtcMicros(400),
        };
        assert!(!record.is_orphan());
    }

    #[test]
    fn stale_branch_detects_missing_ref() {
        let gone = StaleBranchDbRecordV1 {
            store: store(),
            branch: BranchRefV1::new("feature-x").expect("valid"),
            ref_present: false,
            size_bytes: StorageByteSizeV1(1_000),
        };
        assert!(gone.is_stale());
        let live = StaleBranchDbRecordV1 {
            ref_present: true,
            ..gone
        };
        assert!(!live.is_stale());
    }

    #[test]
    fn retention_backlog_detects_past_window_bytes() {
        let record = RetentionBacklogRecordV1 {
            store: store(),
            table: TableNameV1::new("lcm_raw_messages").expect("valid"),
            past_window_bytes: StorageByteSizeV1(3_800),
            oldest_past_window_at: UtcMicros(10),
            window_watermark_at: UtcMicros(100),
        };
        assert!(record.has_backlog());
        assert!(record.validate().is_ok());
    }

    #[test]
    fn retention_backlog_rejects_inconsistent_watermark() {
        let record = RetentionBacklogRecordV1 {
            store: store(),
            table: TableNameV1::new("lcm_raw_messages").expect("valid"),
            past_window_bytes: StorageByteSizeV1(3_800),
            oldest_past_window_at: UtcMicros(200),
            window_watermark_at: UtcMicros(100),
        };
        assert!(record.validate().is_err());
    }

    #[test]
    fn code_generation_retention_rejects_collectable_totals_above_superseded_totals() {
        let record = CodeGenerationRetentionRecordV1 {
            store: StoreKeyV1::new("code-index-v1").expect("valid"),
            superseded_generation_count: 3,
            superseded_generation_bytes: StorageByteSizeV1(3_000),
            collectable_generation_count: 4,
            collectable_generation_bytes: StorageByteSizeV1(2_000),
            stranded_scope_count: 0,
            stranded_scope_bytes: StorageByteSizeV1(0),
        };

        assert!(record.validate().is_err());
    }

    #[test]
    fn code_generation_retention_rejects_bytes_without_generations() {
        let record = CodeGenerationRetentionRecordV1 {
            store: StoreKeyV1::new("code-index-v1").expect("valid"),
            superseded_generation_count: 1,
            superseded_generation_bytes: StorageByteSizeV1(1_000),
            collectable_generation_count: 0,
            collectable_generation_bytes: StorageByteSizeV1(1),
            stranded_scope_count: 0,
            stranded_scope_bytes: StorageByteSizeV1(0),
        };

        assert!(record.validate().is_err());
    }

    #[test]
    fn code_generation_retention_rejects_stranded_bytes_without_stranded_scopes() {
        let record = CodeGenerationRetentionRecordV1 {
            store: StoreKeyV1::new("code-index-v1").expect("valid"),
            superseded_generation_count: 0,
            superseded_generation_bytes: StorageByteSizeV1(0),
            collectable_generation_count: 0,
            collectable_generation_bytes: StorageByteSizeV1(0),
            stranded_scope_count: 0,
            stranded_scope_bytes: StorageByteSizeV1(7_730_941_132),
        };

        assert!(record.validate().is_err());
    }

    #[test]
    fn stranded_scopes_are_a_problem_even_with_a_clean_generation_census() {
        let record = CodeGenerationRetentionRecordV1 {
            store: StoreKeyV1::new("code-index-v1").expect("valid"),
            superseded_generation_count: 3,
            superseded_generation_bytes: StorageByteSizeV1(3_000),
            collectable_generation_count: 0,
            collectable_generation_bytes: StorageByteSizeV1(0),
            stranded_scope_count: 2,
            stranded_scope_bytes: StorageByteSizeV1(7_730_941_132),
        };

        assert!(record.validate().is_ok());
        assert!(!record.has_collectable_generations());
        assert!(record.has_stranded_scopes());
    }

    #[test]
    fn stranded_scope_totals_default_to_zero_for_records_without_them() {
        let record: CodeGenerationRetentionRecordV1 = serde_json::from_str(
            r#"{
                "store": "code-index-v1",
                "superseded_generation_count": 3,
                "superseded_generation_bytes": 3000,
                "collectable_generation_count": 1,
                "collectable_generation_bytes": 1000
            }"#,
        )
        .expect("records predating scope reconciliation stay readable");

        assert_eq!(record.stranded_scope_count, 0);
        assert_eq!(record.stranded_scope_bytes, StorageByteSizeV1(0));
        assert!(!record.has_stranded_scopes());
    }
}
