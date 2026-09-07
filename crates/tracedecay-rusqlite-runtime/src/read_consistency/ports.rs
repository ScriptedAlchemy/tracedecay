use std::future::Future;
use std::pin::Pin;

use tracedecay_store::{ShardWatermarkV1, StoreShardIdV1, UnavailableReasonV1};

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
