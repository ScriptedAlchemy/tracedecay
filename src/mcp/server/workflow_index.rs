//! Daemon-backed workflow-index reads for the [`WorkflowIndexReadPort`]
//! implementation.
//!
//! This module owns the `ProjectSessions` handle so MCP handlers do not. It opens
//! the snapshot through [`GlobalDbWorkflowStore`], which is the one adapter that
//! refuses a non-ProjectSessions shard scope before a read snapshot exists;
//! nothing here reaches past that gate to build a reader itself.

use std::sync::Arc;

use tracedecay_sessions::{
    WorkflowIndexReadPort, WorkflowIndexState, WorkflowReadError, WorkflowRunDetail,
    WorkflowRunDetailFuture, WorkflowRunDetailOutcome, WorkflowRunDetailRequest,
    WorkflowRunListFuture, WorkflowRunListOutcome, WorkflowRunListRequest, WorkflowRunScope,
};

use crate::global_db::RegisteredGlobalDbLeaseV1;
use crate::store::GlobalDbWorkflowStore;
use tracedecay_sessions::runtime::git_correlation::{GitCorrelationError, GitScopeFilter};
use tracedecay_sessions::runtime::workflow_index::{
    RegisteredWorkflowIndexSnapshot, WorkflowIndexError,
};

/// Keeps the workflow index's own error text, so a read failure still reads the
/// same way after the boundary moved.
#[allow(clippy::needless_pass_by_value)] // used with `.map_err(workflow_error)`
fn workflow_error(err: WorkflowIndexError) -> WorkflowReadError {
    WorkflowReadError::new(err.to_string())
}

pub(crate) struct DaemonWorkflowIndexReadService {
    /// The active project's retained `ProjectSessions` authority. Reads borrow
    /// this handle and never discover or open another store.
    database: RegisteredGlobalDbLeaseV1,
}

impl DaemonWorkflowIndexReadService {
    pub(crate) const fn new(database: RegisteredGlobalDbLeaseV1) -> Self {
        Self { database }
    }

    /// Opens one snapshot for one command. The shard-scope refusal lives in
    /// [`GlobalDbWorkflowStore::open_workflow_index_snapshot`], so a wrong-scope
    /// handle fails here instead of reading another shard's rows.
    async fn snapshot(&self) -> Result<RegisteredWorkflowIndexSnapshot, WorkflowReadError> {
        GlobalDbWorkflowStore::new(self.database.as_ref())
            .open_workflow_index_snapshot()
            .await
            .map_err(workflow_error)
    }

    /// Reports a store without workflow-index tables as unavailable, so a
    /// missing schema is never answered as a built index that happens to be
    /// empty.
    ///
    /// One probe covers git-scope reads too: `ensure_registered_schema_for_admission`
    /// installs the git-correlation and workflow-index DDL in a single
    /// transaction, so the correlation tables cannot be absent while these are
    /// present.
    async fn schema_missing(
        snapshot: &RegisteredWorkflowIndexSnapshot,
    ) -> Result<bool, WorkflowReadError> {
        snapshot
            .workflow_tables_present()
            .await
            .map(|present| !present)
            .map_err(workflow_error)
    }

    async fn execute_runs(
        &self,
        command: WorkflowRunListRequest,
    ) -> Result<WorkflowRunListOutcome, WorkflowReadError> {
        let WorkflowRunListRequest { scope, limit } = command;
        let snapshot = self.snapshot().await?;
        if Self::schema_missing(&snapshot).await? {
            return Ok(WorkflowRunListOutcome::Unavailable(
                WorkflowIndexState::IndexNotBuilt,
            ));
        }
        let runs = match scope {
            WorkflowRunScope::Session { session_id } => snapshot
                .runs_for_session(&session_id, limit)
                .await
                .map_err(workflow_error)?,
            WorkflowRunScope::GitScope(filter) => {
                let filter = GitScopeFilter {
                    branch: filter.branch,
                    worktree: filter.worktree,
                    commit: filter.commit,
                };
                let session_ids = match self.database.git_scope_session_ids(&filter) {
                    Ok(session_ids) => session_ids,
                    Err(GitCorrelationError::Unavailable(_)) => {
                        return Ok(WorkflowRunListOutcome::Unavailable(
                            WorkflowIndexState::AuthorityNotRetained,
                        ));
                    }
                    Err(error) => return Err(WorkflowReadError::new(error.to_string())),
                };
                snapshot
                    .runs_for_git_scope(session_ids.as_deref(), limit)
                    .await
                    .map_err(workflow_error)?
            }
        };
        Ok(WorkflowRunListOutcome::Runs(runs))
    }

    /// Reads the run and its agents from one snapshot, so both are observed at
    /// the same database generation.
    async fn execute_run(
        &self,
        command: WorkflowRunDetailRequest,
    ) -> Result<WorkflowRunDetailOutcome, WorkflowReadError> {
        let WorkflowRunDetailRequest { run_id, limit } = command;
        let snapshot = self.snapshot().await?;
        // Without this, a store with no schema answers `NotFound`, which claims
        // the index looked and the run is absent. It cannot know that.
        if Self::schema_missing(&snapshot).await? {
            return Ok(WorkflowRunDetailOutcome::Unavailable(
                WorkflowIndexState::IndexNotBuilt,
            ));
        }
        let Some(mut run) = snapshot.run_for_id(&run_id).await.map_err(workflow_error)? else {
            return Ok(WorkflowRunDetailOutcome::NotFound);
        };
        let agent_count = snapshot
            .agent_count_for_run(&run_id)
            .await
            .map_err(workflow_error)?;
        run.agent_count = agent_count;
        let agents = snapshot
            .agents_for_run(&run_id, limit)
            .await
            .map_err(workflow_error)?;
        let agents_complete =
            i64::try_from(agents.len()).is_ok_and(|returned| returned == agent_count);
        Ok(WorkflowRunDetailOutcome::Run(Box::new(WorkflowRunDetail {
            run,
            agents,
            agent_count,
            agents_complete,
        })))
    }

    /// Resolves a label with an exact predicate, so a missing agent is never
    /// inferred from the bounded prefix used by run detail.
    async fn execute_agent(
        &self,
        run_id: String,
        agent_label: String,
    ) -> Result<WorkflowRunDetailOutcome, WorkflowReadError> {
        let snapshot = self.snapshot().await?;
        if Self::schema_missing(&snapshot).await? {
            return Ok(WorkflowRunDetailOutcome::Unavailable(
                WorkflowIndexState::IndexNotBuilt,
            ));
        }
        let Some(mut run) = snapshot.run_for_id(&run_id).await.map_err(workflow_error)? else {
            return Ok(WorkflowRunDetailOutcome::NotFound);
        };
        let agent_count = snapshot
            .agent_count_for_run(&run_id)
            .await
            .map_err(workflow_error)?;
        run.agent_count = agent_count;
        let agents = snapshot
            .agent_for_run_label(&run_id, &agent_label)
            .await
            .map_err(workflow_error)?
            .into_iter()
            .collect();
        Ok(WorkflowRunDetailOutcome::Run(Box::new(WorkflowRunDetail {
            run,
            agents,
            agent_count,
            agents_complete: true,
        })))
    }
}

impl WorkflowIndexReadPort for DaemonWorkflowIndexReadService {
    fn runs(&self, command: WorkflowRunListRequest) -> WorkflowRunListFuture<'_> {
        Box::pin(async move { self.execute_runs(command).await })
    }

    fn run(&self, command: WorkflowRunDetailRequest) -> WorkflowRunDetailFuture<'_> {
        Box::pin(async move { self.execute_run(command).await })
    }

    fn agent(&self, run_id: String, agent_label: String) -> WorkflowRunDetailFuture<'_> {
        Box::pin(async move { self.execute_agent(run_id, agent_label).await })
    }
}
