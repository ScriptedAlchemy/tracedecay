//! The project sessions authority Git evidence is read from and written to.

use std::future::Future;

use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor};

use super::GitCorrelationError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsSessionTimestamp {
    pub provider: String,
    pub session_id: String,
    pub timestamp: i64,
}

pub trait AnalyticsSessionTimestampSource {
    fn as_analytics_session_timestamp(&self) -> Option<AnalyticsSessionTimestamp>;
}

impl AnalyticsSessionTimestampSource for AnalyticsSessionTimestamp {
    fn as_analytics_session_timestamp(&self) -> Option<AnalyticsSessionTimestamp> {
        Some(self.clone())
    }
}

pub trait GitCorrelationWriteTxn: QueryExecutor + Executor + Sized + Send {
    fn commit(self) -> impl Future<Output = Result<(), GitCorrelationError>> + Send;
}

/// The already-open project sessions authority: Git evidence rows, session
/// activity, and bounded-history receipts all live in its store.
pub trait GitCorrelationSessionStore: Sync {
    /// A read view whose lifetime retains the exact client authority that
    /// issued it. Production stores use a guarded database-engine snapshot;
    /// standalone engine snapshots are confined to test stores.
    type ReadSnapshot: QueryExecutor + Send + Sync;

    type WriteTxn<'txn>: GitCorrelationWriteTxn
    where
        Self: 'txn;

    fn require_project_sessions_authority(&self) -> Result<(), GitCorrelationError>;

    fn read_snapshot(
        &self,
    ) -> impl Future<Output = Result<Self::ReadSnapshot, GitCorrelationError>> + Send;

    fn open_write_transaction(
        &self,
    ) -> impl Future<Output = Result<Self::WriteTxn<'_>, GitCorrelationError>> + Send;
}
