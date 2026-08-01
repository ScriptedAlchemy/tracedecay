//! Narrow ports for workflow-index reads and workflow-run ingest writes.
//!
//! Production adapters live in the root `src/store/workflow.rs`. Workflow
//! index/ingest logic depends only on these contracts so it does not import the
//! concrete registered global database type.

use std::future::Future;

use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor};

use super::WorkflowIndexError;
use crate::{WorkflowAgent, WorkflowRun};

/// Write transaction surface required by workflow ingest upserts.
pub trait WorkflowIngestWriteTxn: QueryExecutor + Executor + Sized + Send {
    fn commit(self) -> impl Future<Output = Result<(), WorkflowIndexError>> + Send;
}

/// Authoritative sink one workflow-ingest sweep upserts through.
///
/// This is the inverted seam for the root `GlobalDbWorkflowStore` adapter.
/// The sweep needs an authority check, a watermark it can advance, and a
/// run/agent upsert; it needs nothing about the registered database that backs
/// them.
///
/// Root wiring: `impl WorkflowIngestSink for GlobalDbWorkflowStore<'_>` in
/// `src/store/workflow.rs`.
pub trait WorkflowIngestSink: Sync {
    /// Whether the bound authority is the registered `ProjectSessions` shard
    /// for `project_id`. A mismatch aborts the sweep instead of writing.
    fn matches_project_sessions_authority(&self, project_id: &tracedecay_domain::ProjectId)
    -> bool;

    /// Reads the ingest watermark. `None` means the store was unreadable;
    /// a store with no watermark yet reports `Some(0)`.
    fn read_ingest_watermark(&self) -> impl Future<Output = Option<i64>> + Send;

    /// Advances the ingest watermark. Failures are logged, not surfaced: the
    /// next sweep re-reads the same runs.
    fn bump_ingest_watermark(&self, value: i64) -> impl Future<Output = ()> + Send;

    /// Upserts one workflow run together with its full agent roster inside a
    /// single write transaction.
    fn upsert_workflow_run(
        &self,
        run: &WorkflowRun,
        agents: &[WorkflowAgent],
    ) -> impl Future<Output = Result<(), WorkflowIndexError>> + Send;
}
