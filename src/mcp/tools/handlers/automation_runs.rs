//! Read-only MCP inspection over the active project's durable automation ledger.

use serde_json::{Value, json};
use tracedecay_agent_hosts::automation::run_ledger::{
    AutomationRunLedgerRecord, find_run_record, load_run_records_page,
};

use crate::errors::{Result, TraceDecayError};
use crate::mcp::tools::ToolResult;
use crate::tracedecay::TraceDecay;

use super::super::renderers;
use super::support::tool_json_with_md;

const DEFAULT_RUN_LIMIT: usize = 50;
const MAX_RUN_LIMIT: usize = 200;

fn ledger_unavailable(operation: &str, error: TraceDecayError) -> TraceDecayError {
    TraceDecayError::project_route(
        "automation_run_ledger_unavailable",
        true,
        format!("automation run ledger is unavailable during {operation}: {error}"),
    )
}

fn run_not_found(run_id: &str) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("automation run not found: {run_id}"),
    }
}

fn parse_limit(args: &Value) -> usize {
    args.get("limit")
        .and_then(Value::as_u64)
        .map_or(DEFAULT_RUN_LIMIT, |limit| {
            usize::try_from(limit)
                .unwrap_or(MAX_RUN_LIMIT)
                .clamp(1, MAX_RUN_LIMIT)
        })
}

fn required_run_id(args: &Value) -> Result<&str> {
    args.get("run_id")
        .and_then(Value::as_str)
        .filter(|run_id| !run_id.is_empty())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: run_id".to_owned(),
        })
}

fn run_summary(record: &AutomationRunLedgerRecord) -> Value {
    json!({
        "run_id": record.run_id,
        "task": record.task,
        "task_key": record.task_key,
        "trigger": record.trigger,
        "backend": record.backend,
        "model": record.model,
        "status": record.status,
        "reviewed_count": record.reviewed_count,
        "accepted_count": record.accepted_count,
        "rejected_count": record.rejected_count,
        "skipped_count": record.skipped_count,
        "error": record.error,
        "started_at": record.started_at,
        "completed_at": record.completed_at,
        "artifact_kinds": record.artifacts.iter().map(|artifact| &artifact.kind).collect::<Vec<_>>(),
    })
}

pub(super) async fn handle_list(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let limit = parse_limit(&args);
    let page = load_run_records_page(&cg.store_layout().dashboard_root, limit)
        .await
        .map_err(|error| ledger_unavailable("list", error))?;
    let completeness = if page.is_complete() {
        "known"
    } else {
        "partial"
    };
    let runs = page.records.iter().map(run_summary).collect::<Vec<_>>();
    let payload = json!({
        "status": "ok",
        "scope": "active_project",
        "runs": runs,
        "count": runs.len(),
        "limit": limit,
        "has_more": page.has_more,
        "malformed_row_count": page.malformed_row_count,
        "completeness": completeness,
    });
    Ok(tool_json_with_md(
        Some(cg.project_root()),
        &args,
        &payload,
        || renderers::automation_run_list_md(&payload),
    ))
}

pub(super) async fn handle_view(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let run_id = required_run_id(&args)?;
    let record = find_run_record(&cg.store_layout().dashboard_root, run_id)
        .await
        .map_err(|error| ledger_unavailable("view", error))?
        .ok_or_else(|| run_not_found(run_id))?;
    let payload = json!({
        "status": "ok",
        "scope": "active_project",
        "run": record,
    });
    Ok(tool_json_with_md(
        Some(cg.project_root()),
        &args,
        &payload,
        || renderers::automation_run_view_md(&payload),
    ))
}
