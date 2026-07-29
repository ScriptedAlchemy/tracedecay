//! Typed contract for daemon-owned workflow-index reads.
//!
//! MCP owns selector parsing and rendering; the daemon owns the ProjectSessions
//! store and the shard-scope gate in front of it. Handlers in this tree
//! therefore name a [`WorkflowIndexReadPort`] instead of a `RegisteredGlobalDb`.
//!
//! Typed workflow models cross the boundary; MCP serializes them only while
//! building the response payload.

pub(crate) use tracedecay_sessions::{
    WorkflowGitScope, WorkflowIndexReadPort, WorkflowIndexState as WorkflowIndexUnavailableReason,
    WorkflowRunDetail as WorkflowRunDetailView, WorkflowRunDetailFuture, WorkflowRunDetailOutcome,
    WorkflowRunDetailRequest as WorkflowRunDetailCommand, WorkflowRunListFuture,
    WorkflowRunListOutcome, WorkflowRunListRequest as WorkflowRunListCommand, WorkflowRunScope,
};

use crate::errors::{Result, TraceDecayError};

fn workflow_read_error(error: tracedecay_sessions::WorkflowReadError) -> TraceDecayError {
    TraceDecayError::Config {
        message: error.to_string(),
    }
}

/// Lists runs through `port`, reporting the typed unavailable state when no
/// port is mounted.
pub(crate) async fn list_workflow_runs(
    port: Option<&dyn WorkflowIndexReadPort>,
    command: WorkflowRunListCommand,
) -> Result<WorkflowRunListOutcome> {
    match port {
        Some(port) => port.runs(command).await.map_err(workflow_read_error),
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
        Some(port) => port.run(command).await.map_err(workflow_read_error),
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
                    WorkflowIndexUnavailableReason::IndexNotBuilt,
                ))
            })
        }

        fn run(&self, command: WorkflowRunDetailCommand) -> WorkflowRunDetailFuture<'_> {
            self.details.lock().expect("details").push(command);
            Box::pin(async {
                Ok(WorkflowRunDetailOutcome::Unavailable(
                    WorkflowIndexUnavailableReason::IndexNotBuilt,
                ))
            })
        }
    }

    fn list_command() -> WorkflowRunListCommand {
        WorkflowRunListCommand {
            scope: WorkflowRunScope::GitScope(WorkflowGitScope {
                branch: Some("main".to_string()),
                worktree: None,
                commit: None,
            }),
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

    /// The two unavailable states stay distinguishable on the wire. A caller
    /// that cannot tell a missing authority from a store without the schema
    /// cannot tell which one to route around.
    #[test]
    fn unavailable_reasons_are_distinct_on_the_wire() {
        let reasons = [
            WorkflowIndexUnavailableReason::AuthorityNotRetained,
            WorkflowIndexUnavailableReason::IndexNotBuilt,
        ];
        let wire = reasons.map(WorkflowIndexUnavailableReason::as_str);
        assert_eq!(wire, ["authority_not_retained", "workflow_index_not_built"]);
        assert!(!WorkflowIndexUnavailableReason::AuthorityNotRetained.is_retryable());
        assert!(WorkflowIndexUnavailableReason::IndexNotBuilt.is_retryable());
        assert_ne!(
            WorkflowIndexUnavailableReason::AuthorityNotRetained.message(),
            WorkflowIndexUnavailableReason::IndexNotBuilt.message()
        );
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
