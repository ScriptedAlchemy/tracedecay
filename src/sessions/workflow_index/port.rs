//! Narrow ports for workflow-index reads and workflow-run ingest writes.
//!
//! Production adapters live in `crate::store::workflow`. Workflow index/ingest
//! logic depends only on these contracts so it does not import the concrete
//! registered global database type.

use std::future::Future;

use crate::db::engine::{Executor, QueryExecutor};

use super::WorkflowIndexError;

/// Write transaction surface required by workflow ingest upserts.
pub(crate) trait WorkflowIngestWriteTxn: QueryExecutor + Executor + Sized + Send {
    fn commit(self) -> impl Future<Output = Result<(), WorkflowIndexError>> + Send;
}
