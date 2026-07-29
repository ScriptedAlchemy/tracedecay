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

/// Why the workflow index could not answer. Each variant is a distinct state
/// that a caller must be able to tell apart from a built index that holds
/// nothing, so none of them may render as a successful empty result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkflowIndexUnavailableReason {
    /// The daemon retained no project-session authority for this request, so
    /// there is no index to consult. Another mount is required, not a retry.
    AuthorityNotRetained,
    /// The store opened, but the workflow index has never been built here.
    /// Background ingest builds it, so this resolves on its own.
    WorkflowIndexNotBuilt,
    /// The workflow index is built, but git correlation is not, so a git-scope
    /// query has nothing to resolve refs against. Only git-scope reads can
    /// reach this: session and run reads do not consult correlation tables.
    GitCorrelationNotBuilt,
}

impl WorkflowIndexUnavailableReason {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::AuthorityNotRetained => "authority_not_retained",
            Self::WorkflowIndexNotBuilt => "workflow_index_not_built",
            Self::GitCorrelationNotBuilt => "git_correlation_not_built",
        }
    }

    /// Whether waiting can change the answer. Both not-built states are filled
    /// in by background ingest; a missing authority is not.
    pub(crate) const fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::WorkflowIndexNotBuilt | Self::GitCorrelationNotBuilt
        )
    }

    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::AuthorityNotRetained => "registered project session database is unavailable",
            Self::WorkflowIndexNotBuilt => {
                "the workflow index has not been built for this project yet"
            }
            Self::GitCorrelationNotBuilt => {
                "git correlation has not been built for this project yet, so workflow runs \
                 cannot be resolved by branch, worktree, or commit"
            }
        }
    }
}

/// Closed set of list results.
#[derive(Clone, Debug)]
pub(crate) enum WorkflowRunListOutcome {
    Runs(Vec<Value>),
    Unavailable(WorkflowIndexUnavailableReason),
}

/// Closed set of run-detail results.
#[derive(Clone, Debug)]
pub(crate) enum WorkflowRunDetailOutcome {
    Run(WorkflowRunDetailView),
    /// The index is built and holds no run under this id. Distinct from an
    /// unbuilt index, which cannot know whether the run exists.
    NotFound,
    Unavailable(WorkflowIndexUnavailableReason),
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
        None => Ok(WorkflowRunListOutcome::Unavailable(
            WorkflowIndexUnavailableReason::AuthorityNotRetained,
        )),
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
        None => Ok(WorkflowRunDetailOutcome::Unavailable(
            WorkflowIndexUnavailableReason::AuthorityNotRetained,
        )),
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
            Box::pin(async {
                Ok(WorkflowRunListOutcome::Unavailable(
                    WorkflowIndexUnavailableReason::WorkflowIndexNotBuilt,
                ))
            })
        }

        fn run(&self, command: WorkflowRunDetailCommand) -> WorkflowRunDetailFuture<'_> {
            self.details.lock().expect("details").push(command);
            Box::pin(async {
                Ok(WorkflowRunDetailOutcome::Unavailable(
                    WorkflowIndexUnavailableReason::WorkflowIndexNotBuilt,
                ))
            })
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
    /// exists and happens to hold no runs, and it must name itself as the
    /// missing authority rather than as an unbuilt index.
    #[tokio::test]
    async fn absent_port_reports_unavailable_rather_than_an_empty_run_list() {
        let outcome = list_workflow_runs(None, list_command())
            .await
            .expect("list");
        assert!(matches!(
            outcome,
            WorkflowRunListOutcome::Unavailable(
                WorkflowIndexUnavailableReason::AuthorityNotRetained
            )
        ));

        let outcome = read_workflow_run(None, detail_command())
            .await
            .expect("detail");
        assert!(matches!(
            outcome,
            WorkflowRunDetailOutcome::Unavailable(
                WorkflowIndexUnavailableReason::AuthorityNotRetained
            )
        ));
    }

    /// The three unavailable states stay distinguishable on the wire, and only
    /// the ones background ingest fills in invite a retry. Collapsing any pair
    /// would tell a caller to wait for something that will never arrive, or to
    /// give up on something that is still being built.
    #[test]
    fn unavailable_reasons_are_distinct_and_classify_retryability() {
        let reasons = [
            WorkflowIndexUnavailableReason::AuthorityNotRetained,
            WorkflowIndexUnavailableReason::WorkflowIndexNotBuilt,
            WorkflowIndexUnavailableReason::GitCorrelationNotBuilt,
        ];
        let wire = reasons.map(WorkflowIndexUnavailableReason::as_str);
        assert_eq!(
            wire,
            [
                "authority_not_retained",
                "workflow_index_not_built",
                "git_correlation_not_built"
            ]
        );

        assert!(!WorkflowIndexUnavailableReason::AuthorityNotRetained.is_retryable());
        assert!(WorkflowIndexUnavailableReason::WorkflowIndexNotBuilt.is_retryable());
        assert!(WorkflowIndexUnavailableReason::GitCorrelationNotBuilt.is_retryable());
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
