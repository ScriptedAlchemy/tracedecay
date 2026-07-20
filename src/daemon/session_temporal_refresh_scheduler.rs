mod projector;
mod registry;
#[cfg(test)]
mod tests;
mod wake;
mod worker;

pub(super) use registry::SessionTemporalRefreshSchedulerRegistry;
pub(crate) use wake::{
    SessionTemporalRefreshBlocker, SessionTemporalRefreshRetryClass,
    SessionTemporalRefreshUnavailableReason, SessionTemporalRefreshWake,
    SessionTemporalRefreshWorkerStatus,
};

const MAX_PENDING_REFRESH_REQUESTS: usize = 128;
