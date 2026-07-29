//! Typed contract for daemon-owned workflow-index reads.
//!
//! MCP owns selector parsing and rendering; the daemon owns the ProjectSessions
//! store and the shard-scope gate in front of it. Handlers in this tree
//! therefore name a [`WorkflowIndexReadPort`] instead of a `RegisteredGlobalDb`.
//!
//! Rows cross the boundary as their owning authority serializes them, so the
//! rendered payload is unchanged by the indirection.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;

use crate::errors::Result;
use crate::sessions::git_correlation::GitScopeFilter;

/// Which runs a list read covers. The selectors are mutually exclusive, so the
/// handler resolves exactly one before asking.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkflowRunScope {
    /// Runs spawned by one user thread.
    Session { session_id: String },
    /// Runs reachable from a git ref. The filter is already validated, so the
    /// daemon never re-parses caller strings.
    GitScope { filter: GitScopeFilter },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkflowRunListCommand {
    pub(crate) scope: WorkflowRunScope,
    pub(crate) limit: usize,
}

/// One run and its agents. `limit` bounds the agent page; the run lookup itself
/// is a single row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkflowRunDetailCommand {
    pub(crate) run_id: String,
    pub(crate) limit: usize,
}

/// One agent row plus the label the caller drills on. `row` is the agent
/// exactly as its owning authority serializes it, so MCP renders it verbatim
/// and only reads `agent_label` to select.
#[derive(Clone, Debug)]
pub(crate) struct WorkflowAgentView {
    pub(crate) agent_label: String,
    pub(crate) row: Value,
}

/// A run and its agents observed at one database generation. Both come from a
/// single port call so a concurrent ingest cannot split them across snapshots.
#[derive(Clone, Debug)]
pub(crate) struct WorkflowRunDetailView {
    pub(crate) run: Value,
    pub(crate) agents: Vec<WorkflowAgentView>,
}

/// Closed set of list results.
#[derive(Clone, Debug)]
pub(crate) enum WorkflowRunListOutcome {
    Runs(Vec<Value>),
    /// The daemon did not retain a project session authority. This is a state,
    /// not an empty run list: callers must report it as such.
    IndexUnavailable,
}

/// Closed set of run-detail results.
#[derive(Clone, Debug)]
pub(crate) enum WorkflowRunDetailOutcome {
    Run(WorkflowRunDetailView),
    /// The index answered and holds no run under this id.
    NotFound,
    IndexUnavailable,
}

pub(crate) type WorkflowRunListFuture<'a> =
    Pin<Box<dyn Future<Output = Result<WorkflowRunListOutcome>> + Send + 'a>>;
pub(crate) type WorkflowRunDetailFuture<'a> =
    Pin<Box<dyn Future<Output = Result<WorkflowRunDetailOutcome>> + Send + 'a>>;

/// The one path MCP handlers use to read the workflow index.
pub(crate) trait WorkflowIndexReadPort: Send + Sync {
    fn runs(&self, command: WorkflowRunListCommand) -> WorkflowRunListFuture<'_>;

    fn run(&self, command: WorkflowRunDetailCommand) -> WorkflowRunDetailFuture<'_>;
}

/// Lists runs through `port`, reporting the typed unavailable state when no
/// port is mounted.
pub(crate) async fn list_workflow_runs(
    port: Option<&dyn WorkflowIndexReadPort>,
    command: WorkflowRunListCommand,
) -> Result<WorkflowRunListOutcome> {
    match port {
        Some(port) => port.runs(command).await,
        None => Ok(WorkflowRunListOutcome::IndexUnavailable),
    }
}

/// Reads one run and its agents through `port`, reporting the typed unavailable
/// state when no port is mounted.
pub(crate) async fn read_workflow_run(
    port: Option<&dyn WorkflowIndexReadPort>,
    command: WorkflowRunDetailCommand,
) -> Result<WorkflowRunDetailOutcome> {
    match port {
        Some(port) => port.run(command).await,
        None => Ok(WorkflowRunDetailOutcome::IndexUnavailable),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Answers every read with the unavailable state while recording the
    /// command, so a test can assert what MCP asked for without a store.
    #[derive(Default)]
    struct RecordingPort {
        lists: Mutex<Vec<WorkflowRunListCommand>>,
        details: Mutex<Vec<WorkflowRunDetailCommand>>,
    }

    impl WorkflowIndexReadPort for RecordingPort {
        fn runs(&self, command: WorkflowRunListCommand) -> WorkflowRunListFuture<'_> {
            self.lists.lock().expect("lists").push(command);
            Box::pin(async { Ok(WorkflowRunListOutcome::IndexUnavailable) })
        }

        fn run(&self, command: WorkflowRunDetailCommand) -> WorkflowRunDetailFuture<'_> {
            self.details.lock().expect("details").push(command);
            Box::pin(async { Ok(WorkflowRunDetailOutcome::IndexUnavailable) })
        }
    }

    fn list_command() -> WorkflowRunListCommand {
        WorkflowRunListCommand {
            scope: WorkflowRunScope::GitScope {
                filter: GitScopeFilter {
                    branch: Some("main".to_string()),
                    worktree: None,
                    commit: None,
                },
            },
            limit: 7,
        }
    }

    fn detail_command() -> WorkflowRunDetailCommand {
        WorkflowRunDetailCommand {
            run_id: "wf_alpha".to_string(),
            limit: 7,
        }
    }

    /// An unretained authority is a state. It must not answer as an index that
    /// exists and happens to hold no runs.
    #[tokio::test]
    async fn absent_port_reports_unavailable_rather_than_an_empty_run_list() {
        let outcome = list_workflow_runs(None, list_command())
            .await
            .expect("list");
        assert!(matches!(outcome, WorkflowRunListOutcome::IndexUnavailable));

        let outcome = read_workflow_run(None, detail_command())
            .await
            .expect("detail");
        assert!(matches!(
            outcome,
            WorkflowRunDetailOutcome::IndexUnavailable
        ));
    }

    /// The validated selector and the caller's bounds cross the boundary
    /// verbatim, so the daemon never re-parses caller strings.
    #[tokio::test]
    async fn mounted_port_receives_the_validated_selector_and_bounds() {
        let port = RecordingPort::default();
        list_workflow_runs(Some(&port), list_command())
            .await
            .expect("list");
        read_workflow_run(Some(&port), detail_command())
            .await
            .expect("detail");

        assert_eq!(
            port.lists.lock().expect("lists").as_slice(),
            &[list_command()]
        );
        assert_eq!(
            port.details.lock().expect("details").as_slice(),
            &[detail_command()]
        );
    }
}
