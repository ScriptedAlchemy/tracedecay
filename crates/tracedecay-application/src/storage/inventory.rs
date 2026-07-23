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
}
