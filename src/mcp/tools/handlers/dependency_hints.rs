use serde_json::{Value, json};
use tracedecay_code_index::chunks::CodeIndexImportEvidenceV1;

use crate::errors::{Result, TraceDecayError};
use crate::mcp::tools::render::{self, Md};
use crate::tracedecay::queries::graph::VerifiedGraphQuery;

pub(super) fn should_check_external_import_hint(result_count: usize, limit: usize) -> bool {
    result_count == 0 || result_count < limit.clamp(1, 20)
}

pub(super) fn lazy_indexing_requested(args: &Value) -> bool {
    args.get("lazy_index_ignored_dependencies")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub(super) async fn external_import_hint(
    graph: &VerifiedGraphQuery,
    query: &str,
    limit: usize,
    scope_prefix: Option<&str>,
    deadline: Option<&tracedecay_application::Deadline>,
    cancellation: Option<&tracedecay_application::CancellationSignal>,
) -> Result<Option<Value>> {
    let candidates =
        ignored_dependency_candidates(graph, query, limit, scope_prefix, deadline, cancellation)?;
    if candidates.is_empty() {
        return Ok(None);
    }
    Ok(Some(json!({
        "message": "Search results were sparse, and parser-backed imports contain matching external-module specifiers. Module resolution and ignored-source status are not verified by this advisory read.",
        "evidence": "parser_external_module_specifier",
        "resolution_status": "unverified",
        "candidates": candidates.into_iter().map(|candidate| json!({
            "module": candidate.module_specifier,
            "symbol": candidate.imported_name,
            "import_file": candidate.logical_path,
            "line": user_line(candidate.start_line),
        })).collect::<Vec<_>>(),
        "suggested_action": "verify_external_import_before_lazy_indexing",
    })))
}

pub(super) fn unavailable_hint(error: &TraceDecayError) -> Value {
    let (reason_code, retryable, detail) =
        if let Some((reason_code, retryable, detail)) = error.project_route_context() {
            (reason_code, retryable, detail.to_owned())
        } else if let Some((_authority, reason)) = error.reset_required_context() {
            ("code-graph-reset-required", false, reason.to_owned())
        } else {
            (
                "code-graph-import-hint-unavailable",
                false,
                error.to_string(),
            )
        };
    json!({
        "status": "unavailable",
        "reason_code": reason_code,
        "retryable": retryable,
        "detail": detail,
    })
}

pub(super) async fn lazy_index_ignored_dependency_candidates(
    graph: &VerifiedGraphQuery,
    query: &str,
    limit: usize,
    scope_prefix: Option<&str>,
    deadline: Option<&tracedecay_application::Deadline>,
    cancellation: Option<&tracedecay_application::CancellationSignal>,
) -> Result<Vec<String>> {
    let candidates =
        ignored_dependency_candidates(graph, query, limit, scope_prefix, deadline, cancellation)?;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    Err(TraceDecayError::project_route(
        "ignored_dependency_index_scheduler_unavailable",
        true,
        "ignored dependency indexing requires a bounded scheduler authority",
    ))
}

fn ignored_dependency_candidates(
    graph: &VerifiedGraphQuery,
    query: &str,
    limit: usize,
    scope_prefix: Option<&str>,
    deadline: Option<&tracedecay_application::Deadline>,
    cancellation: Option<&tracedecay_application::CancellationSignal>,
) -> Result<Vec<CodeIndexImportEvidenceV1>> {
    if cancellation.is_some_and(tracedecay_application::CancellationSignal::is_cancelled) {
        return Err(TraceDecayError::project_route(
            "code-graph-cancelled",
            false,
            "verified dependency import read was cancelled",
        ));
    }
    if deadline.is_some_and(|deadline| deadline.is_elapsed_at(tracedecay_application::now_micros()))
    {
        return Err(TraceDecayError::project_route(
            "code-graph-timed-out",
            true,
            "verified dependency import read exceeded its deadline",
        ));
    }
    graph.external_type_import_candidates(query, scope_prefix, limit.clamp(1, 20))
}

pub(super) fn append_external_import_hint_md(md: &mut Md, value: &Value) {
    let Some(hint) = value.get("external_import_hint") else {
        return;
    };
    if hint.get("status").and_then(Value::as_str) == Some("unavailable") {
        let detail = hint
            .get("detail")
            .and_then(Value::as_str)
            .unwrap_or("verified import evidence is unavailable");
        md.blank()
            .heading(3, "External Import Hint")
            .line(&format!("Hint unavailable: {detail}"));
        return;
    }
    let msg = hint
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("Matching parser-backed external import candidates were found.");
    md.blank().heading(3, "External Import Hint").line(msg);
    if let Some(candidates) = hint.get("candidates").and_then(Value::as_array) {
        for candidate in candidates {
            let module = render::field_str(candidate, "module");
            let symbol = render::field_str(candidate, "symbol");
            let file = render::field_str(candidate, "import_file");
            let line = render::field_i64(candidate, "line");
            md.bullet(&format!(
                "`{symbol}` is imported from external-module specifier `{module}` at {file}:{line}"
            ));
        }
    }
}

fn user_line(line: u32) -> u32 {
    line.saturating_add(1)
}
