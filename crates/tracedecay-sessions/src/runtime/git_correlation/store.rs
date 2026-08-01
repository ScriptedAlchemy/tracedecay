//! Narrow store port for git-correlation session authority.
//!
//! Production adapters live in the root `src/store/git_correlation.rs`.
//! Session logic depends only on this contract so backfill/query code does not
//! import the concrete registered global database type or full analytics event
//! rows.

use std::future::Future;

use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, ReadSnapshot};

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

/// The already-open project-sessions authority backfill reads and writes.
///
/// This is the inverted seam for the root `GlobalDbGitCorrelationStore`
/// adapter: backfill needs an authority check, a read snapshot, and a write
/// transaction, and nothing about the registered global database that supplies
/// them.
///
/// Root wiring: `impl GitCorrelationSessionStore for GlobalDbGitCorrelationStore<'_>`
/// in `src/store/git_correlation.rs`.
pub trait GitCorrelationSessionStore: Sync {
    /// Write transaction this authority hands out.
    type WriteTxn<'txn>: GitCorrelationWriteTxn
    where
        Self: 'txn;

    /// Fails unless the bound authority is a registered `ProjectSessions`
    /// shard. Git correlation writes are project-scoped by construction.
    fn require_project_sessions_authority(&self) -> Result<(), GitCorrelationError>;

    /// Opens a read snapshot over the authority.
    fn read_snapshot(
        &self,
    ) -> impl Future<Output = Result<ReadSnapshot, GitCorrelationError>> + Send;

    /// Opens a write transaction over the authority.
    fn open_write_transaction(
        &self,
    ) -> impl Future<Output = Result<Self::WriteTxn<'_>, GitCorrelationError>> + Send;
}
