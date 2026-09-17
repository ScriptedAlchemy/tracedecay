//! Driver-free watermark and retained-snapshot observation contracts.

mod ports;

pub use crate::watermark::CommitWatermarkSubscription;
pub(crate) use crate::watermark::{CommitWatermarkPublicationError, CommittedWatermarkPublisher};
pub use ports::{CommitWatermarkSource, WatermarkSourceState};
