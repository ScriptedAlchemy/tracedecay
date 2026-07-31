//! Narrow store port for git-correlation session authority.
//!
//! Production adapters live in `crate::store::git_correlation`. Session logic
//! depends only on this contract so backfill/query code does not import the
//! concrete registered global database type or full analytics event rows.

use std::future::Future;

use crate::db::engine::{Executor, QueryExecutor, ReadSnapshot};

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
pub(crate) trait AnalyticsSessionTimestampSource {
    fn as_analytics_session_timestamp(&self) -> Option<AnalyticsSessionTimestamp>;
}

impl AnalyticsSessionTimestampSource for AnalyticsSessionTimestamp {
    fn as_analytics_session_timestamp(&self) -> Option<AnalyticsSessionTimestamp> {
        Some(self.clone())
    }
}

/// Write transaction surface required by span/commit backfill.
pub(crate) trait GitCorrelationWriteTxn: QueryExecutor + Executor + Sized + Send {
    fn commit(self) -> impl Future<Output = Result<(), GitCorrelationError>> + Send;
}

/// Project-session store authority used by git-correlation backfill and reads.
///
/// Implementations must preserve `ProjectSessions` authority checks, watermark
/// ordering, idempotent span/commit writes, and fail-open-per-session skip
/// semantics owned by the backfill loop.
///
/// Snapshot/write associated types stay concrete (not `impl Trait`) so
/// monomorphized callers keep the same `Send` futures as direct
/// `RegisteredGlobalDb` usage.
pub(crate) trait GitCorrelationStore: Send + Sync {
    type WriteTxn<'a>: GitCorrelationWriteTxn
    where
        Self: 'a;

    fn require_project_sessions_authority(&self) -> Result<(), GitCorrelationError>;

    fn read_snapshot(
        &self,
    ) -> impl Future<Output = Result<ReadSnapshot, GitCorrelationError>> + Send;

    fn open_write_transaction(
        &self,
    ) -> impl Future<Output = Result<Self::WriteTxn<'_>, GitCorrelationError>> + Send;
}
