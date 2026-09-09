//! Bounded compaction for stores retained by a live runtime.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracedecay_contracts::storage::compaction::CompactionTriggerPolicyV1;
use tracedecay_contracts::storage::identity::{FreePageRatioV1, StorageByteSizeV1, StoreKeyV1};
use tracedecay_contracts::storage::telemetry::StoreSizeSampleV1;
use tracedecay_domain::UtcMicros;
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::db::Database;

use tracedecay_contracts::storage::compaction::CompactionThresholdConfig;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveStoreCompactionFailureV1 {
    StoreSizeSampleFailed,
    InvalidCompactionPolicy,
    IncrementalVacuumFailed,
    PostCompactionSampleFailed,
}

impl LiveStoreCompactionFailureV1 {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StoreSizeSampleFailed => "store_size_sample_failed",
            Self::InvalidCompactionPolicy => "invalid_compaction_policy",
            Self::IncrementalVacuumFailed => "incremental_vacuum_failed",
            Self::PostCompactionSampleFailed => "post_compaction_sample_failed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveStoreCompactionOutcomeV1 {
    NotScheduled,
    Compacted {
        freelist_before: u64,
        freelist_after: u64,
    },
    Failed(LiveStoreCompactionFailureV1),
}

enum RetainedCompactionStore<'a> {
    Registered(&'a RegisteredGlobalDb),
    Project(&'a Database),
}

impl RetainedCompactionStore<'_> {
    async fn storage_page_counts(&self) -> tracedecay_domain::errors::Result<(u64, u64, u64)> {
        match self {
            Self::Registered(database) => database.storage_page_counts().await,
            Self::Project(database) => database.storage_page_counts().await,
        }
    }

    async fn run_bounded_incremental_compaction(
        &self,
        max_pages: u64,
    ) -> tracedecay_domain::errors::Result<()> {
        match self {
            Self::Registered(database) => {
                database.run_bounded_incremental_compaction(max_pages).await
            }
            Self::Project(database) => database.run_incremental_vacuum(max_pages).await,
        }
    }
}

#[hotpath::measure(label = "maintenance.live_compaction.registered", future = true)]
pub async fn compact_registered_store(
    database: &RegisteredGlobalDb,
    config: &CompactionThresholdConfig,
) -> LiveStoreCompactionOutcomeV1 {
    compact_store(RetainedCompactionStore::Registered(database), config).await
}

#[hotpath::measure(label = "maintenance.live_compaction.project", future = true)]
pub async fn compact_project_store(
    database: &Database,
    config: &CompactionThresholdConfig,
) -> LiveStoreCompactionOutcomeV1 {
    compact_store(RetainedCompactionStore::Project(database), config).await
}

async fn compact_store(
    store: RetainedCompactionStore<'_>,
    config: &CompactionThresholdConfig,
) -> LiveStoreCompactionOutcomeV1 {
    let Ok((page_size, page_count, freelist)) = store.storage_page_counts().await else {
        return LiveStoreCompactionOutcomeV1::Failed(
            LiveStoreCompactionFailureV1::StoreSizeSampleFailed,
        );
    };
    let Ok(scheduled) = compaction_is_scheduled(page_size, page_count, freelist, config) else {
        return LiveStoreCompactionOutcomeV1::Failed(
            LiveStoreCompactionFailureV1::InvalidCompactionPolicy,
        );
    };
    if !scheduled {
        return LiveStoreCompactionOutcomeV1::NotScheduled;
    }
    if store
        .run_bounded_incremental_compaction(u64::from(config.max_pages_per_tick.max(1)))
        .await
        .is_err()
    {
        return LiveStoreCompactionOutcomeV1::Failed(
            LiveStoreCompactionFailureV1::IncrementalVacuumFailed,
        );
    }
    let Ok((_, _, freelist_after)) = store.storage_page_counts().await else {
        return LiveStoreCompactionOutcomeV1::Failed(
            LiveStoreCompactionFailureV1::PostCompactionSampleFailed,
        );
    };
    LiveStoreCompactionOutcomeV1::Compacted {
        freelist_before: freelist,
        freelist_after,
    }
}

fn compaction_is_scheduled(
    page_size: u64,
    page_count: u64,
    freelist: u64,
    config: &CompactionThresholdConfig,
) -> Result<bool, ()> {
    if page_size == 0 || page_count == 0 {
        return Ok(false);
    }
    let observed_at = duration_to_utc_micros(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| ())?,
    )?;
    let sample = store_size_sample_at(page_size, page_count, freelist, observed_at)?;
    let policy = CompactionTriggerPolicyV1 {
        free_page_ratio_threshold: FreePageRatioV1::new(config.free_page_ratio_threshold)
            .map_err(|_| ())?,
        minimum_reclaimable_bytes: StorageByteSizeV1(config.minimum_reclaimable_bytes),
    };
    policy
        .decide(&sample)
        .map(|decision| decision.is_scheduled())
        .map_err(|_| ())
}

fn duration_to_utc_micros(duration: Duration) -> Result<UtcMicros, ()> {
    i64::try_from(duration.as_micros())
        .map(UtcMicros)
        .map_err(|_| ())
}

fn store_size_sample_at(
    page_size: u64,
    page_count: u64,
    freelist: u64,
    observed_at: UtcMicros,
) -> Result<StoreSizeSampleV1, ()> {
    Ok(StoreSizeSampleV1 {
        store: StoreKeyV1::new("store.db").map_err(|_| ())?,
        page_size_bytes: u32::try_from(page_size).map_err(|_| ())?,
        page_count,
        freelist_pages: freelist,
        observed_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(threshold: f64, minimum_reclaimable_bytes: u64) -> CompactionThresholdConfig {
        CompactionThresholdConfig {
            free_page_ratio_threshold: threshold,
            minimum_reclaimable_bytes,
            max_pages_per_tick: 16,
        }
    }

    #[test]
    fn compaction_policy_distinguishes_ineligible_eligible_and_invalid() {
        assert_eq!(
            compaction_is_scheduled(4_096, 100, 50, &config(0.75, 0)),
            Ok(false)
        );
        assert_eq!(
            compaction_is_scheduled(4_096, 100, 50, &config(0.5, 0)),
            Ok(true)
        );
        assert_eq!(
            compaction_is_scheduled(4_096, 100, 50, &config(0.0, 0)),
            Err(())
        );
    }

    #[test]
    fn compaction_clock_conversion_preserves_sub_millisecond_microseconds_and_refuses_overflow() {
        let observed_at =
            duration_to_utc_micros(std::time::Duration::new(1_700_000_000, 123_456_000))
                .expect("unix duration fits");

        assert_eq!(observed_at, UtcMicros(1_700_000_000_123_456));
        assert_eq!(duration_to_utc_micros(std::time::Duration::MAX), Err(()));
    }
}
