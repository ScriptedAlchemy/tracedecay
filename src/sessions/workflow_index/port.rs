//! Narrow ports for workflow-index reads and workflow-run ingest writes.
//!
//! Production adapters live in `crate::store::workflow`. Workflow index/ingest
//! logic depends only on these contracts so it does not import the concrete
//! registered global database type.

use std::future::Future;

use tracedecay_domain::ProjectId;

use crate::db::engine::{Executor, QueryExecutor};

use super::{WorkflowAgent, WorkflowIndexError, WorkflowRun};

/// Write transaction surface required by workflow ingest upserts.
pub(crate) trait WorkflowIngestWriteTxn: QueryExecutor + Executor + Sized + Send {
    fn commit(self) -> impl Future<Output = Result<(), WorkflowIndexError>> + Send;
}

/// Fail-open ingest sink for discovered workflow runs.
///
/// Implementations must preserve `ProjectSessions` authority checks, watermark
/// ordering, idempotent run/agent upserts, and fail-open-per-run skip
/// semantics owned by the ingest sweep.
pub(crate) trait WorkflowIngestSink: Send + Sync {
    fn matches_project_sessions_authority(&self, project_id: &ProjectId) -> bool;

    fn read_ingest_watermark(&self) -> impl Future<Output = Option<i64>> + Send;

    fn upsert_workflow_run(
        &self,
        run: &WorkflowRun,
        agents: &[WorkflowAgent],
    ) -> impl Future<Output = Result<(), WorkflowIndexError>> + Send;

    fn bump_ingest_watermark(&self, value: i64) -> impl Future<Output = ()> + Send;
}
