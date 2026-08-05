//! Branch-scoped tools: `tracedecay_admin_branch_add`, `tracedecay_branch_list`, `tracedecay_branch_search`, `tracedecay_branch_diff`.

use super::*;

/// Daemon-only branch-add entry point used by the first-party CLI.
///
/// Branch preparation copies and syncs a graph database, so it must run inside
/// the managed daemon's database-authority scope rather than in the CLI process.
pub(crate) async fn handle_admin_branch_add(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let branch = require_admin_branch_name(&args)?;
    let outcome =
        TraceDecay::add_branch_tracking_with_options(cg.project_root(), branch, cg.open_options())
            .await?;
    let output = json!({ "outcome": admin_branch_add_outcome_name(&outcome) });
    Ok(ToolResult::new(
        json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string(&output).unwrap_or_default(),
            }]
        }),
        vec![],
    ))
}

fn require_admin_branch_name(args: &Value) -> Result<&str> {
    args.get("branch")
        .and_then(Value::as_str)
        .filter(|branch| !branch.is_empty())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: branch".to_string(),
        })
}

fn admin_branch_add_outcome_name(outcome: &crate::branch::BranchAddOutcome) -> &'static str {
    match outcome {
        crate::branch::BranchAddOutcome::NotIndexed => "not_indexed",
        crate::branch::BranchAddOutcome::AlreadyTracked => "already_tracked",
        crate::branch::BranchAddOutcome::Added => "added",
        crate::branch::BranchAddOutcome::Deferred => "deferred",
    }
}

/// Handles `tracedecay_branch_list` tool calls.
pub(crate) fn handle_branch_list(cg: &TraceDecay, args: &Value) -> ToolResult {
    let diagnostics = cg.branch_diagnostics();
    let mut result = serde_json::to_value(&diagnostics).unwrap_or(json!({}));
    if let Some(object) = result.as_object_mut() {
        object.insert(
            "branch_count".to_string(),
            json!(diagnostics.tracked_branch_count),
        );
    }

    generic_tool_result(Some(cg.project_root()), args, &result, vec![])
}

/// Handles `tracedecay_branch_search` tool calls.
pub(crate) async fn handle_branch_search(
    port: Option<&dyn tracedecay_application::BranchQueryPort>,
    args: Value,
    controls: tracedecay_application::BranchQueryControlsV1,
) -> Result<ToolResult> {
    let request: tracedecay_application::BranchSearchRequestV1 =
        serde_json::from_value(args.clone()).map_err(|error| TraceDecayError::Config {
            message: format!("invalid tracedecay_branch_search request: {error}"),
        })?;
    request
        .validate()
        .map_err(|error| TraceDecayError::Config {
            message: format!("invalid tracedecay_branch_search request: {error}"),
        })?;
    let outcome = invoke_branch_query(
        port,
        tracedecay_application::BranchQueryRequestV1::Search(request),
        controls,
    )
    .await;
    render_branch_query_outcome(&args, outcome)
}

/// Handles `tracedecay_branch_diff` tool calls.
///
/// Compares code graphs between two branches. For each symbol present in
/// either branch, reports whether it was added, removed, or changed
/// (signature differs).
pub(crate) async fn handle_branch_diff(
    port: Option<&dyn tracedecay_application::BranchQueryPort>,
    args: Value,
    controls: tracedecay_application::BranchQueryControlsV1,
) -> Result<ToolResult> {
    let request: tracedecay_application::BranchDiffRequestV1 = serde_json::from_value(args.clone())
        .map_err(|error| TraceDecayError::Config {
            message: format!("invalid tracedecay_branch_diff request: {error}"),
        })?;
    request
        .validate()
        .map_err(|error| TraceDecayError::Config {
            message: format!("invalid tracedecay_branch_diff request: {error}"),
        })?;
    let outcome = invoke_branch_query(
        port,
        tracedecay_application::BranchQueryRequestV1::Diff(request),
        controls,
    )
    .await;
    render_branch_query_outcome(&args, outcome)
}

async fn invoke_branch_query(
    port: Option<&dyn tracedecay_application::BranchQueryPort>,
    request: tracedecay_application::BranchQueryRequestV1,
    controls: tracedecay_application::BranchQueryControlsV1,
) -> tracedecay_application::BranchQueryOutcomeV1 {
    match port {
        Some(port) => port.execute(request, controls).await,
        None => tracedecay_application::BranchQueryOutcomeV1::Unavailable {
            reason:
                tracedecay_application::BranchQueryUnavailableReasonV1::GraphAuthorityUnavailable,
        },
    }
}

fn render_branch_query_outcome(
    args: &Value,
    outcome: tracedecay_application::BranchQueryOutcomeV1,
) -> Result<ToolResult> {
    let touched = touched_files_from_branch_outcome(&outcome);
    let semantic_failure = !matches!(
        outcome,
        tracedecay_application::BranchQueryOutcomeV1::Complete { .. }
            | tracedecay_application::BranchQueryOutcomeV1::Partial { .. }
    );
    let failure_message = semantic_failure.then(|| branch_query_failure_message(&outcome));
    let payload = serde_json::to_value(&outcome)?;
    let result = generic_tool_result(None, args, &payload, touched);
    Ok(if let Some(message) = failure_message {
        result
            .with_semantic_error(true)
            .with_failure_message(message)
    } else {
        result
    })
}

fn touched_files_from_branch_outcome(
    outcome: &tracedecay_application::BranchQueryOutcomeV1,
) -> Vec<String> {
    let result = match outcome {
        tracedecay_application::BranchQueryOutcomeV1::Complete { result }
        | tracedecay_application::BranchQueryOutcomeV1::Partial { result, .. } => result,
        _ => return Vec::new(),
    };
    let tracedecay_application::BranchQueryResultV1::Diff(diff) = result else {
        return Vec::new();
    };
    unique_file_paths(
        diff.added
            .iter()
            .map(|symbol| symbol.file.as_str())
            .chain(diff.removed.iter().map(|symbol| symbol.file.as_str()))
            .chain(diff.changed.iter().map(|symbol| symbol.file.as_str())),
    )
}

fn branch_query_failure_message(outcome: &tracedecay_application::BranchQueryOutcomeV1) -> String {
    match outcome {
        tracedecay_application::BranchQueryOutcomeV1::Denied => {
            "branch query was not authorized".to_owned()
        }
        tracedecay_application::BranchQueryOutcomeV1::Cancelled => {
            "branch query was cancelled".to_owned()
        }
        tracedecay_application::BranchQueryOutcomeV1::TimedOut => {
            "branch query exceeded its deadline".to_owned()
        }
        tracedecay_application::BranchQueryOutcomeV1::Stale { reason } => {
            format!("branch query snapshot became stale: {reason:?}")
        }
        tracedecay_application::BranchQueryOutcomeV1::Unavailable { reason } => {
            format!("branch query is unavailable: {reason:?}")
        }
        tracedecay_application::BranchQueryOutcomeV1::Complete { .. }
        | tracedecay_application::BranchQueryOutcomeV1::Partial { .. } => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct DenyingPort {
        invoked: AtomicBool,
    }

    impl tracedecay_application::BranchQueryPort for DenyingPort {
        fn execute<'a>(
            &'a self,
            _request: tracedecay_application::BranchQueryRequestV1,
            _controls: tracedecay_application::BranchQueryControlsV1,
        ) -> tracedecay_application::BranchQueryFuture<'a> {
            self.invoked.store(true, Ordering::Release);
            Box::pin(async { tracedecay_application::BranchQueryOutcomeV1::Denied })
        }
    }

    #[test]
    fn admin_branch_add_requires_a_nonempty_branch_name() {
        for args in [serde_json::json!({}), serde_json::json!({ "branch": "" })] {
            let error = require_admin_branch_name(&args)
                .expect_err("branch add request without branch must fail");
            assert!(error.to_string().contains("branch"));
        }
    }

    #[test]
    fn admin_branch_add_outcomes_have_stable_wire_names() {
        assert_eq!(
            admin_branch_add_outcome_name(&crate::branch::BranchAddOutcome::NotIndexed),
            "not_indexed"
        );
        assert_eq!(
            admin_branch_add_outcome_name(&crate::branch::BranchAddOutcome::AlreadyTracked),
            "already_tracked"
        );
        assert_eq!(
            admin_branch_add_outcome_name(&crate::branch::BranchAddOutcome::Added),
            "added"
        );
        assert_eq!(
            admin_branch_add_outcome_name(&crate::branch::BranchAddOutcome::Deferred),
            "deferred"
        );
    }

    #[tokio::test]
    async fn absent_branch_query_port_is_typed_unavailable() {
        let result = handle_branch_search(
            None,
            json!({"branch": "main", "query": "needle"}),
            tracedecay_application::BranchQueryControlsV1::default(),
        )
        .await
        .expect("typed outcome");
        assert_eq!(result.semantic_error(), Some(true));
        assert!(
            result
                .failure_message()
                .is_some_and(|message| message.contains("GraphAuthorityUnavailable"))
        );
    }

    #[tokio::test]
    async fn branch_search_invokes_only_the_injected_application_port() {
        let port = DenyingPort {
            invoked: AtomicBool::new(false),
        };
        let result = handle_branch_search(
            Some(&port),
            json!({"branch": "main", "query": "needle"}),
            tracedecay_application::BranchQueryControlsV1::default(),
        )
        .await
        .expect("typed denial");
        assert!(port.invoked.load(Ordering::Acquire));
        assert_eq!(result.semantic_error(), Some(true));
        assert_eq!(
            result.failure_message(),
            Some("branch query was not authorized")
        );
    }
}
