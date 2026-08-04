//! Read-only branch snapshot tools.

use super::*;

/// Handles `tracedecay_branch_list` tool calls.
pub(crate) fn handle_branch_list(cg: &TraceDecay, args: &Value) -> ToolResult {
    let diagnostics = cg.branch_diagnostics();
    let mut result = serde_json::to_value(&diagnostics).unwrap_or(json!({}));
    if let Some(object) = result.as_object_mut() {
        object.insert(
            "branch_count".to_string(),
            json!(diagnostics.snapshot_count),
        );
    }

    generic_tool_result(Some(cg.project_root()), args, &result, vec![])
}

fn branch_search_unavailable(
    cg: &TraceDecay,
    args: &Value,
    branch: &str,
    source_revision: &tracedecay_domain::GitOidV1,
    unavailable: &crate::mcp::server::CodeIndexSearchUnavailableV1,
) -> ToolResult {
    let reason = unavailable.reason.as_str();
    let result = json!({
        "status": "unavailable",
        "branch": branch,
        "source_revision": source_revision.as_str(),
        "code_generation": unavailable.code_generation,
        "reason": reason,
        "retryable": matches!(
            unavailable.reason,
            crate::mcp::server::CodeIndexSearchUnavailableReasonV1::GenerationUnavailable
                | crate::mcp::server::CodeIndexSearchUnavailableReasonV1::CapacityUnavailable
        ),
    });
    generic_tool_result(Some(cg.project_root()), args, &result, vec![])
        .with_semantic_error(true)
        .with_failure_message(format!(
            "branch '{branch}' search is unavailable for commit {}: {reason}",
            source_revision.as_str()
        ))
}

/// Handles `tracedecay_branch_search` through the immutable code-generation
/// search authority selected by the branch tip's exact commit.
pub(crate) async fn handle_branch_search(
    cg: &TraceDecay,
    args: Value,
    executor: Option<&crate::mcp::server::CodeIndexSearchExecutor>,
    authority: Option<&crate::mcp::server::CodeIndexSearchAuthorityV1>,
    deadline: Option<tracedecay_application::Deadline>,
    cancellation: Option<tracedecay_application::CancellationSignal>,
) -> Result<ToolResult> {
    let branch = args
        .get("branch")
        .and_then(Value::as_str)
        .filter(|branch| !branch.is_empty())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: branch".to_string(),
        })?;
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .filter(|query| !query.is_empty())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: query".to_string(),
        })?;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(10, |value| value.min(500) as usize);
    let source_revision = crate::branch::local_branch_commit(cg.project_root(), branch)
        .map_err(|message| TraceDecayError::Config { message })?;
    let Some(executor) = executor else {
        let unavailable = crate::mcp::server::CodeIndexSearchUnavailableV1 {
            code_generation: None,
            reason: crate::mcp::server::CodeIndexSearchUnavailableReasonV1::CapabilityUnavailable,
            semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                reason: "code_index_unavailable",
            },
            coverage: crate::mcp::server::CodeIndexSearchCoverageV1::unavailable(
                "code_index_unavailable",
            ),
        };
        return Ok(branch_search_unavailable(
            cg,
            &args,
            branch,
            &source_revision,
            &unavailable,
        ));
    };
    let outcome = executor(crate::mcp::server::CodeIndexSearchRequestV1 {
        project_root: cg.project_root().to_path_buf(),
        query: query.to_owned(),
        source_revision: Some(source_revision.clone()),
        limit,
        cursor: None,
        mode: crate::mcp::server::CodeIndexSearchModeV1::FallbackAllowed,
        authority: authority.cloned(),
        deadline,
        cancellation,
    })
    .await;

    match outcome {
        crate::mcp::server::CodeIndexSearchOutcomeV1::Complete(complete) => {
            let results = complete
                .ordered_candidates
                .iter()
                .map(|ranked| {
                    let display = complete.display_by_anchor.get(&ranked.candidate.anchor_id);
                    json!({
                        "candidate": ranked,
                        "name": display.map(|display| display.name.as_str()),
                        "qualified_name": display.map(|display| display.qualified_name.as_str()),
                        "kind": display.map(|display| display.kind.as_str()),
                        "node_id": display.and_then(|display| display.node_id.as_deref()),
                        "branch": branch,
                        "source_revision": source_revision.as_str(),
                        "code_generation": complete.code_generation,
                    })
                })
                .collect::<Vec<_>>();
            let result = json!({
                "status": "complete",
                "branch": branch,
                "source_revision": source_revision.as_str(),
                "code_generation": complete.code_generation,
                "results": results,
            });
            Ok(generic_tool_result(
                Some(cg.project_root()),
                &args,
                &result,
                vec![],
            ))
        }
        crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(unavailable) => Ok(
            branch_search_unavailable(cg, &args, branch, &source_revision, &unavailable),
        ),
    }
}

fn branch_diff_unavailable(
    cg: &TraceDecay,
    args: &Value,
    base: (&str, &tracedecay_domain::GitOidV1),
    head: (&str, &tracedecay_domain::GitOidV1),
    unavailable: &crate::mcp::server::CodeIndexBranchDiffUnavailableV1,
) -> ToolResult {
    let reason = unavailable.reason.as_str();
    let result = json!({
        "status": "unavailable",
        "base": base.0,
        "head": head.0,
        "base_revision": base.1.as_str(),
        "head_revision": head.1.as_str(),
        "base_generation": unavailable.base_generation,
        "head_generation": unavailable.head_generation,
        "reason": reason,
        "retryable": matches!(
            unavailable.reason,
            crate::mcp::server::CodeIndexSearchUnavailableReasonV1::GenerationUnavailable
                | crate::mcp::server::CodeIndexSearchUnavailableReasonV1::CapacityUnavailable
        ),
    });
    generic_tool_result(Some(cg.project_root()), args, &result, vec![])
        .with_semantic_error(true)
        .with_failure_message(format!(
            "branch diff {}..{} is unavailable: {reason}",
            base.0, head.0
        ))
}

fn branch_symbol_json(symbol: &crate::mcp::server::CodeIndexBranchSymbolV1) -> Value {
    json!({
        "name": symbol.name,
        "qualified_name": symbol.qualified_name,
        "kind": symbol.kind,
        "file": symbol.file,
        "content_digest": symbol.content_digest,
    })
}

/// Compares two exact local branch commits through their sealed code-index
/// generations. The active checkout and legacy shared graph never participate.
pub(crate) async fn handle_branch_diff(
    cg: &TraceDecay,
    args: Value,
    executor: Option<&crate::mcp::server::CodeIndexBranchDiffExecutor>,
    authority: Option<&crate::mcp::server::CodeIndexSearchAuthorityV1>,
    deadline: Option<tracedecay_application::Deadline>,
    cancellation: Option<tracedecay_application::CancellationSignal>,
) -> Result<ToolResult> {
    let base_name = args
        .get("base")
        .and_then(Value::as_str)
        .filter(|base| !base.is_empty())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: base".to_string(),
        })?;
    let head_name = args
        .get("head")
        .and_then(Value::as_str)
        .or_else(|| cg.active_branch())
        .filter(|head| !head.is_empty())
        .ok_or_else(|| TraceDecayError::Config {
            message: "cannot determine head branch — specify it explicitly".to_string(),
        })?;
    let base_revision = crate::branch::local_branch_commit(cg.project_root(), base_name)
        .map_err(|message| TraceDecayError::Config { message })?;
    let head_revision = crate::branch::local_branch_commit(cg.project_root(), head_name)
        .map_err(|message| TraceDecayError::Config { message })?;

    let Some(executor) = executor else {
        let unavailable = crate::mcp::server::CodeIndexBranchDiffUnavailableV1 {
            base_generation: None,
            head_generation: None,
            reason: crate::mcp::server::CodeIndexSearchUnavailableReasonV1::CapabilityUnavailable,
        };
        return Ok(branch_diff_unavailable(
            cg,
            &args,
            (base_name, &base_revision),
            (head_name, &head_revision),
            &unavailable,
        ));
    };
    let outcome = executor(crate::mcp::server::CodeIndexBranchDiffRequestV1 {
        project_root: cg.project_root().to_path_buf(),
        base_revision: base_revision.clone(),
        head_revision: head_revision.clone(),
        file_filter: args.get("file").and_then(Value::as_str).map(str::to_owned),
        kind_filter: args.get("kind").and_then(Value::as_str).map(str::to_owned),
        authority: authority.cloned(),
        deadline,
        cancellation,
    })
    .await;
    match outcome {
        crate::mcp::server::CodeIndexBranchDiffOutcomeV1::Complete(completed) => {
            let added = completed
                .added
                .iter()
                .map(branch_symbol_json)
                .collect::<Vec<_>>();
            let removed = completed
                .removed
                .iter()
                .map(branch_symbol_json)
                .collect::<Vec<_>>();
            let changed = completed
                .changed
                .iter()
                .map(|changed| {
                    json!({
                        "base": branch_symbol_json(&changed.base),
                        "head": branch_symbol_json(&changed.head),
                    })
                })
                .collect::<Vec<_>>();
            let touched = unique_file_paths(
                completed
                    .added
                    .iter()
                    .map(|symbol| symbol.file.as_str())
                    .chain(completed.removed.iter().map(|symbol| symbol.file.as_str()))
                    .chain(completed.changed.iter().flat_map(|changed| {
                        [changed.base.file.as_str(), changed.head.file.as_str()]
                    })),
            );
            let result = json!({
                "status": "complete",
                "base": base_name,
                "head": head_name,
                "base_revision": base_revision.as_str(),
                "head_revision": head_revision.as_str(),
                "base_generation": completed.base_generation,
                "head_generation": completed.head_generation,
                "summary": {
                    "added": added.len(),
                    "removed": removed.len(),
                    "changed": changed.len(),
                },
                "added": added,
                "removed": removed,
                "changed": changed,
            });
            Ok(generic_tool_result(
                Some(cg.project_root()),
                &args,
                &result,
                touched,
            ))
        }
        crate::mcp::server::CodeIndexBranchDiffOutcomeV1::Unavailable(unavailable) => {
            Ok(branch_diff_unavailable(
                cg,
                &args,
                (base_name, &base_revision),
                (head_name, &head_revision),
                &unavailable,
            ))
        }
    }
}
