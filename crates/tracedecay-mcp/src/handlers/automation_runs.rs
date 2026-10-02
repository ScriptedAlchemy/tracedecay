//! Read-only inspection over the active project's durable automation ledger.

use std::path::Path;

use serde_json::Value;
use tracedecay_automation_runtime::automation::run_ledger::{
    find_run_record, load_run_records_page, read_run_artifact_payload,
};
use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_contracts::retrieval::{
    AUTOMATION_RUN_LIST_DEFAULT_LIMIT, AUTOMATION_RUN_LIST_MAX_LIMIT, AutomationReadStatusV1,
    AutomationRunArtifactViewResultV1, AutomationRunArtifactViewSurfaceRequestV1,
    AutomationRunListEntryV1, AutomationRunListResultV1, AutomationRunListSurfaceRequestV1,
    AutomationRunPageCompletenessV1, AutomationRunScopeV1, AutomationRunViewResultV1,
    AutomationRunViewSurfaceRequestV1,
};
use tracedecay_domain::errors::{Result, TraceDecayError};

use crate::handlers::graph::graph_tool_completion;
use crate::handlers::support::decode_primitive_request;

/// A reset refusal is terminal and keeps its authority; only a transient
/// read failure becomes the retryable route state.
fn ledger_unavailable(operation: &str, error: TraceDecayError) -> TraceDecayError {
    if error.reset_required_context().is_some() {
        return error;
    }
    TraceDecayError::project_route(
        "automation_run_ledger_unavailable",
        true,
        format!("automation run ledger is unavailable during {operation}: {error}"),
    )
}

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

#[hotpath::measure(label = "mcp.automation.run_list.total")]
pub async fn compute_run_list(
    dashboard_root: &Path,
    args: &Value,
) -> Result<GraphToolCompletionV1> {
    let request: AutomationRunListSurfaceRequestV1 =
        decode_primitive_request(args, "tracedecay_automation_run_list")?;
    let limit = request.limit.unwrap_or(AUTOMATION_RUN_LIST_DEFAULT_LIMIT);
    if !(1..=AUTOMATION_RUN_LIST_MAX_LIMIT).contains(&limit) {
        return Err(config_error(format!(
            "invalid arguments for tracedecay_automation_run_list: limit must be between 1 and {AUTOMATION_RUN_LIST_MAX_LIMIT}"
        )));
    }
    let page = hotpath::future!(
        load_run_records_page(dashboard_root, limit as usize),
        label = "mcp.automation.run_list.load"
    )
    .await
    .map_err(|error| ledger_unavailable("list", error))?;
    let completeness = if page.is_complete() {
        AutomationRunPageCompletenessV1::Known
    } else {
        AutomationRunPageCompletenessV1::Partial
    };
    let runs = page
        .records
        .iter()
        .map(AutomationRunListEntryV1::of)
        .collect::<Vec<_>>();
    Ok(graph_tool_completion(
        GraphToolResultV1::AutomationRunList(AutomationRunListResultV1 {
            status: AutomationReadStatusV1::Ok,
            scope: AutomationRunScopeV1::ActiveProject,
            count: runs.len(),
            runs,
            limit,
            has_more: page.has_more,
            malformed_row_count: page.malformed_row_count,
            completeness,
        }),
        Vec::new(),
    ))
}

#[hotpath::measure(label = "mcp.automation.run_view.total")]
pub async fn compute_run_view(
    dashboard_root: &Path,
    args: &Value,
) -> Result<GraphToolCompletionV1> {
    let request: AutomationRunViewSurfaceRequestV1 =
        decode_primitive_request(args, "tracedecay_automation_run_view")?;
    if request.run_id.is_empty() {
        return Err(config_error(
            "invalid arguments for tracedecay_automation_run_view: run_id must not be empty",
        ));
    }
    let run = hotpath::future!(
        find_run_record(dashboard_root, &request.run_id),
        label = "mcp.automation.run_view.load"
    )
    .await
    .map_err(|error| ledger_unavailable("view", error))?
    .ok_or_else(|| {
        TraceDecayError::not_found(format!("automation run not found: {}", request.run_id))
    })?;
    Ok(graph_tool_completion(
        GraphToolResultV1::AutomationRunView(Box::new(AutomationRunViewResultV1 {
            status: AutomationReadStatusV1::Ok,
            scope: AutomationRunScopeV1::ActiveProject,
            run,
        })),
        Vec::new(),
    ))
}

#[hotpath::measure(label = "mcp.automation.artifact_view.total")]
pub async fn compute_run_artifact_view(
    dashboard_root: &Path,
    args: &Value,
) -> Result<GraphToolCompletionV1> {
    let request: AutomationRunArtifactViewSurfaceRequestV1 =
        decode_primitive_request(args, "tracedecay_automation_run_artifact_view")?;
    let run_id = request.run_id.as_str();
    let kind = request.kind.as_str();
    let record = hotpath::future!(
        find_run_record(dashboard_root, run_id),
        label = "mcp.automation.artifact_view.load"
    )
    .await?
    .ok_or_else(|| TraceDecayError::not_found(format!("automation run not found: {run_id}")))?;
    let artifact = record
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == kind)
        .cloned()
        .ok_or_else(|| {
            TraceDecayError::not_found(format!(
                "automation run artifact not found: {run_id}/{kind}"
            ))
        })?;
    let payload = hotpath::future!(
        read_run_artifact_payload(dashboard_root, &record.run_id, &artifact),
        label = "mcp.automation.artifact_view.read"
    )
    .await?;
    Ok(graph_tool_completion(
        GraphToolResultV1::AutomationRunArtifactView(Box::new(AutomationRunArtifactViewResultV1 {
            status: AutomationReadStatusV1::Ok,
            run_id: record.run_id,
            artifact,
            payload,
        })),
        Vec::new(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_reset_refusal_keeps_its_authority_and_read_failures_stay_retryable() {
        let reset = ledger_unavailable(
            "list",
            TraceDecayError::reset_required("automation run ledger", "schema v1 row"),
        );
        assert_eq!(
            reset.reset_required_context(),
            Some(("automation run ledger", "schema v1 row"))
        );

        let transient = ledger_unavailable(
            "list",
            TraceDecayError::Config {
                message: "automation dashboard root is not a directory".to_owned(),
            },
        );
        assert_eq!(
            transient
                .project_route_context()
                .map(|(code, retryable, _)| (code, retryable)),
            Some(("automation_run_ledger_unavailable", true))
        );
    }
}
