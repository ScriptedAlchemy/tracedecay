//! Exact immutable branch-snapshot reads.

use super::*;

/// Lists exact local branch refs. A branch name never selects a branch DB.
pub(crate) fn handle_branch_list(cg: &TraceDecay, args: &Value) -> ToolResult {
    match crate::branch::local_branch_snapshots(cg.project_root()) {
        Ok(snapshots) => {
            let snapshots = snapshots
                .into_iter()
                .map(|snapshot| {
                    json!({
                        "branch": snapshot.name,
                        "source_revision": snapshot.commit,
                    })
                })
                .collect::<Vec<_>>();
            let result = json!({
                "status": "complete",
                "snapshot_count": snapshots.len(),
                "snapshots": snapshots,
            });
            generic_tool_result(Some(cg.project_root()), args, &result, vec![])
        }
        Err(_) => generic_tool_result(
            Some(cg.project_root()),
            args,
            &json!({
                "status": "unavailable",
                "reason": "local_refs_unavailable",
                "retryable": false,
            }),
            vec![],
        )
        .with_semantic_error(true)
        .with_failure_message("local branch snapshots are unavailable"),
    }
}

fn branch_reference_unavailable(
    cg: &TraceDecay,
    args: &Value,
    field: &str,
    branch: &str,
) -> ToolResult {
    generic_tool_result(
        Some(cg.project_root()),
        args,
        &json!({
            "status": "unavailable",
            field: branch,
            "reason": "branch_ref_unavailable",
            "retryable": false,
        }),
        vec![],
    )
    .with_semantic_error(true)
    .with_failure_message(format!(
        "branch '{branch}' does not resolve to a local commit"
    ))
}

fn branch_search_unavailable(
    cg: &TraceDecay,
    args: &Value,
    branch: &str,
    revision: &tracedecay_domain::GitOidV1,
    unavailable: &crate::mcp::server::CodeIndexSearchUnavailableV1,
) -> ToolResult {
    let reason = unavailable.reason.as_str();
    generic_tool_result(
        Some(cg.project_root()),
        args,
        &json!({
            "status": "unavailable",
            "branch": branch,
            "source_revision": revision.as_str(),
            "code_generation": unavailable.code_generation,
            "reason": reason,
            "retryable": matches!(
                unavailable.reason,
                crate::mcp::server::CodeIndexSearchUnavailableReasonV1::GenerationUnavailable
                    | crate::mcp::server::CodeIndexSearchUnavailableReasonV1::CapacityUnavailable
            ),
        }),
        vec![],
    )
    .with_semantic_error(true)
    .with_failure_message(format!(
        "branch '{branch}' search is unavailable for commit {}: {reason}",
        revision.as_str()
    ))
}

/// Searches the generation sealed for the selected local ref's exact commit.
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
    let revision = match crate::branch::local_branch_commit(cg.project_root(), branch) {
        Ok(revision) => revision,
        Err(_) => return Ok(branch_reference_unavailable(cg, &args, "branch", branch)),
    };
    let Some(executor) = executor else {
        return Ok(branch_search_unavailable(
            cg,
            &args,
            branch,
            &revision,
            &crate::mcp::server::CodeIndexSearchUnavailableV1 {
                code_generation: None,
                reason:
                    crate::mcp::server::CodeIndexSearchUnavailableReasonV1::CapabilityUnavailable,
                semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                    reason: "code_index_unavailable",
                },
                coverage: crate::mcp::server::CodeIndexSearchCoverageV1::unavailable(
                    "code_index_unavailable",
                ),
            },
        ));
    };
    match executor(crate::mcp::server::CodeIndexSearchRequestV1 {
        project_root: cg.project_root().to_path_buf(),
        query: query.to_owned(),
        source_revision: Some(revision.clone()),
        limit,
        cursor: None,
        mode: crate::mcp::server::CodeIndexSearchModeV1::FallbackAllowed,
        authority: authority.cloned(),
        deadline,
        cancellation,
    })
    .await
    {
        crate::mcp::server::CodeIndexSearchOutcomeV1::Complete(complete) => {
            let results = complete
                .ordered_candidates
                .iter()
                .map(|ranked| {
                    let display = complete.display_by_anchor.get(&ranked.candidate.anchor_id);
                    json!({
                        "candidate": ranked,
                        "name": display.map(|value| value.name.as_str()),
                        "qualified_name": display.map(|value| value.qualified_name.as_str()),
                        "kind": display.map(|value| value.kind.as_str()),
                        "branch": branch,
                        "source_revision": revision.as_str(),
                        "code_generation": complete.code_generation,
                    })
                })
                .collect::<Vec<_>>();
            Ok(generic_tool_result(
                Some(cg.project_root()),
                &args,
                &json!({
                    "status": "complete",
                    "branch": branch,
                    "source_revision": revision.as_str(),
                    "code_generation": complete.code_generation,
                    "results": results,
                }),
                vec![],
            ))
        }
        crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(unavailable) => Ok(
            branch_search_unavailable(cg, &args, branch, &revision, &unavailable),
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
    generic_tool_result(
        Some(cg.project_root()),
        args,
        &json!({
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
        }),
        vec![],
    )
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

/// Compares generations sealed for the two selected local refs' exact commits.
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
    let base_revision = match crate::branch::local_branch_commit(cg.project_root(), base_name) {
        Ok(revision) => revision,
        Err(_) => return Ok(branch_reference_unavailable(cg, &args, "base", base_name)),
    };
    let head_revision = match crate::branch::local_branch_commit(cg.project_root(), head_name) {
        Ok(revision) => revision,
        Err(_) => return Ok(branch_reference_unavailable(cg, &args, "head", head_name)),
    };
    let Some(executor) = executor else {
        return Ok(branch_diff_unavailable(
            cg,
            &args,
            (base_name, &base_revision),
            (head_name, &head_revision),
            &crate::mcp::server::CodeIndexBranchDiffUnavailableV1 {
                base_generation: None,
                head_generation: None,
                reason:
                    crate::mcp::server::CodeIndexSearchUnavailableReasonV1::CapabilityUnavailable,
            },
        ));
    };
    match executor(crate::mcp::server::CodeIndexBranchDiffRequestV1 {
        project_root: cg.project_root().to_path_buf(),
        base_revision: base_revision.clone(),
        head_revision: head_revision.clone(),
        file_filter: args.get("file").and_then(Value::as_str).map(str::to_owned),
        kind_filter: args.get("kind").and_then(Value::as_str).map(str::to_owned),
        authority: authority.cloned(),
        deadline,
        cancellation,
    })
    .await
    {
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
                .map(|value| {
                    json!({
                        "base": branch_symbol_json(&value.base),
                        "head": branch_symbol_json(&value.head),
                    })
                })
                .collect::<Vec<_>>();
            let touched =
                unique_file_paths(
                    completed
                        .added
                        .iter()
                        .map(|symbol| symbol.file.as_str())
                        .chain(completed.removed.iter().map(|symbol| symbol.file.as_str()))
                        .chain(completed.changed.iter().flat_map(|value| {
                            [value.base.file.as_str(), value.head.file.as_str()]
                        })),
                );
            Ok(generic_tool_result(
                Some(cg.project_root()),
                &args,
                &json!({
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
                }),
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
