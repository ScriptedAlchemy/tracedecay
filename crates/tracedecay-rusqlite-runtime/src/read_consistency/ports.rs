use std::future::Future;
use std::pin::Pin;
use std::time::{SystemTime, UNIX_EPOCH};

use tracedecay_store::{
    ShardWatermarkV1, SnapshotLeaseIdV1, SnapshotLeaseV1, StoreShardIdV1, UnavailableReasonV1,
};

pub type WatermarkFuture<'a> = Pin<Box<dyn Future<Output = WatermarkSourceState> + Send + 'a>>;

/// Published writer state. Infrastructure remains represented by the existing
/// driver-neutral unavailability reasons rather than an invented ledger error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatermarkSourceState {
    Available(ShardWatermarkV1),
    Unavailable(UnavailableReasonV1),
}

/// Narrow subscription to successful writer commits.
///
/// `wait_for_change` must complete immediately if the source has already moved
/// past `after`, and must be cancellation-safe when its future is dropped. This
/// closes the current/subscribe race without exposing the private commit ledger.
pub trait CommitWatermarkSource: Send + Sync {
    fn current(&self, shard_id: &StoreShardIdV1) -> WatermarkSourceState;

    fn wait_for_change<'a>(
        &'a self,
        shard_id: &'a StoreShardIdV1,
        after: &'a ShardWatermarkV1,
    ) -> WatermarkFuture<'a>;
}

/// Result of consulting the authoritative retained-snapshot registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RetainedSnapshotState {
    Retained(Box<SnapshotLeaseV1>),
    Expired,
    NotRetained,
    Unavailable(UnavailableReasonV1),
}

/// Narrow retained-snapshot lookup. Implementations own retention and pinning;
/// the coordinator never reconstructs a snapshot from commit history.
pub trait RetainedSnapshotRegistry: Send + Sync {
    fn lookup(&self, lease_id: &SnapshotLeaseIdV1) -> RetainedSnapshotState;
}

/// Wall-clock seam used only for the absolute UTC expiry carried by a lease.
/// Wait bounds themselves always use Tokio's monotonic clock.
pub trait ConsistencyClock: Send + Sync {
    fn utc_now_micros(&self) -> i64;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemConsistencyClock;

impl ConsistencyClock for SystemConsistencyClock {
    fn utc_now_micros(&self) -> i64 {
        let micros = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_micros())
            .unwrap_or(0);
        i64::try_from(micros).unwrap_or(i64::MAX)
    }
}
