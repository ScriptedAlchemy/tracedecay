//! Narrow store port for git-correlation session authority.
//!
//! Production adapters live in `crate::store::git_correlation`. Session logic
//! depends only on this contract so backfill/query code does not import the
//! concrete registered global database type or full analytics event rows.

use std::future::Future;

use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor};

use super::GitCorrelationError;

/// Session-scoped analytics timestamp consumed by git-correlation backfill.
///
/// Only provider/session identity and the event timestamp are retained. Full
/// analytics event rows stay outside the sessions layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsSessionTimestamp {
    pub provider: String,
    pub session_id: String,
    pub timestamp: i64,
}

/// Borrowed view used to project analytics rows into
/// [`AnalyticsSessionTimestamp`] without pulling infrastructure DTOs into
/// sessions.
pub trait AnalyticsSessionTimestampSource {
    fn as_analytics_session_timestamp(&self) -> Option<AnalyticsSessionTimestamp>;
}

impl AnalyticsSessionTimestampSource for AnalyticsSessionTimestamp {
    fn as_analytics_session_timestamp(&self) -> Option<AnalyticsSessionTimestamp> {
        Some(self.clone())
    }
}

/// Write transaction surface required by span/commit backfill.
pub trait GitCorrelationWriteTxn: QueryExecutor + Executor + Sized + Send {
    fn commit(self) -> impl Future<Output = Result<(), GitCorrelationError>> + Send;
}
