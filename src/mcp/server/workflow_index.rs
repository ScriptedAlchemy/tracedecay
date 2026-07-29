//! Daemon-backed workflow-index reads for the [`WorkflowIndexReadPort`]
//! implementation.
//!
//! This module owns the ProjectSessions handle so MCP handlers do not. It opens
//! the snapshot through [`GlobalDbWorkflowStore`], which is the one adapter that
//! refuses a non-ProjectSessions shard scope before a read snapshot exists;
//! nothing here reaches past that gate to build a reader itself.

use std::sync::Arc;

use serde_json::Value;

use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::mcp::tools::{
    WorkflowAgentView, WorkflowIndexReadPort, WorkflowRunDetailCommand, WorkflowRunDetailFuture,
    WorkflowRunDetailOutcome, WorkflowRunDetailView, WorkflowRunListCommand, WorkflowRunListFuture,
    WorkflowRunListOutcome, WorkflowRunScope,
};
use crate::sessions::workflow_index::{RegisteredWorkflowIndexSnapshot, WorkflowIndexError};
use crate::store::GlobalDbWorkflowStore;

/// Keeps the workflow index's own error text, so a read failure still reads the
/// same way after the boundary moved.
#[allow(clippy::needless_pass_by_value)] // used with `.map_err(workflow_error)`
fn workflow_error(err: WorkflowIndexError) -> TraceDecayError {
    TraceDecayError::Config {
        message: err.to_string(),
    }
}

fn serialize_row<T: serde::Serialize>(row: &T) -> Result<Value> {
    serde_json::to_value(row).map_err(Into::into)
}

pub(crate) struct DaemonWorkflowIndexReadService {
    /// The active project's retained ProjectSessions authority. Reads borrow
    /// this handle and never discover or open another store.
    database: Arc<RegisteredGlobalDb>,
}

impl DaemonWorkflowIndexReadService {
    pub(crate) const fn new(database: Arc<RegisteredGlobalDb>) -> Self {
        Self { database }
    }

    /// Opens one snapshot for one command. The shard-scope refusal lives in
    /// [`GlobalDbWorkflowStore::open_workflow_index_snapshot`], so a wrong-scope
    /// handle fails here instead of reading another shard's rows.
    async fn snapshot(&self) -> Result<RegisteredWorkflowIndexSnapshot> {
        GlobalDbWorkflowStore::new(self.database.as_ref())
            .open_workflow_index_snapshot()
            .await
            .map_err(workflow_error)
    }

    async fn execute_runs(
        &self,
        command: WorkflowRunListCommand,
    ) -> Result<WorkflowRunListOutcome> {
        let WorkflowRunListCommand { scope, limit } = command;
        let snapshot = self.snapshot().await?;
        let runs = match &scope {
            WorkflowRunScope::Session { session_id } => snapshot
                .runs_for_session(session_id, limit)
                .await
                .map_err(workflow_error)?,
            WorkflowRunScope::GitScope { filter } => snapshot
                .runs_for_git_scope(filter, limit)
                .await
                .map_err(workflow_error)?,
        };
        let runs = runs.iter().map(serialize_row).collect::<Result<Vec<_>>>()?;
        Ok(WorkflowRunListOutcome::Runs(runs))
    }

    /// Reads the run and its agents from one snapshot, so both are observed at
    /// the same database generation.
    async fn execute_run(
        &self,
        command: WorkflowRunDetailCommand,
    ) -> Result<WorkflowRunDetailOutcome> {
        let WorkflowRunDetailCommand { run_id, limit } = command;
        let snapshot = self.snapshot().await?;
        let Some(run) = snapshot.run_for_id(&run_id).await.map_err(workflow_error)? else {
            return Ok(WorkflowRunDetailOutcome::NotFound);
        };
        let agents = snapshot
            .agents_for_run(&run_id, limit)
            .await
            .map_err(workflow_error)?;
        let agents = agents
            .iter()
            .map(|agent| {
                Ok(WorkflowAgentView {
                    agent_label: agent.agent_label.clone(),
                    row: serialize_row(agent)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(WorkflowRunDetailOutcome::Run(WorkflowRunDetailView {
            run: serialize_row(&run)?,
            agents,
        }))
    }
}

impl WorkflowIndexReadPort for DaemonWorkflowIndexReadService {
    fn runs(&self, command: WorkflowRunListCommand) -> WorkflowRunListFuture<'_> {
        Box::pin(async move { self.execute_runs(command).await })
    }

    fn run(&self, command: WorkflowRunDetailCommand) -> WorkflowRunDetailFuture<'_> {
        Box::pin(async move { self.execute_run(command).await })
    }
}
