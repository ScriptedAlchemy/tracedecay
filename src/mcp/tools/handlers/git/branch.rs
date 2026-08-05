//! Exact immutable branch-snapshot reads.

use std::path::Path;
use std::sync::{Arc, LazyLock};

use super::*;

const MAX_BRANCH_REFS_PER_READ: usize = 128;
static BRANCH_REF_READ_ADMISSION: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(2)));

enum BranchRouteReadErrorV1 {
    Capacity,
    Task,
    Ref(crate::branch::LocalBranchSnapshotErrorV1),
}

async fn run_branch_ref_read<T, F>(
    project_root: std::path::PathBuf,
    max_refs: usize,
    after: Option<String>,
    deadline: Option<tracedecay_application::Deadline>,
    cancellation: Option<tracedecay_application::CancellationSignal>,
    operation: F,
) -> std::result::Result<T, BranchRouteReadErrorV1>
where
    T: Send + 'static,
    F: FnOnce(
            &Path,
            &crate::branch::LocalBranchReadControlV1,
        ) -> std::result::Result<T, crate::branch::LocalBranchSnapshotErrorV1>
        + Send
        + 'static,
{
    let permit = Arc::clone(&BRANCH_REF_READ_ADMISSION)
        .try_acquire_owned()
        .map_err(|_| BranchRouteReadErrorV1::Capacity)?;
    let terminal_control = crate::branch::LocalBranchReadControlV1 {
        max_refs,
        after: after.clone(),
        deadline: deadline.clone(),
        cancellation: cancellation.clone(),
    };
    let task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        operation(
            &project_root,
            &crate::branch::LocalBranchReadControlV1 {
                max_refs,
                after,
                deadline,
                cancellation,
            },
        )
        .map_err(BranchRouteReadErrorV1::Ref)
    });
    match crate::daemon::code_index_task_support::settle_owned_blocking_task(
        task,
        std::time::Duration::from_millis(10),
        || {
            terminal_control
                .termination()
                .map(BranchRouteReadErrorV1::Ref)
        },
    )
    .await
    {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err(BranchRouteReadErrorV1::Task),
        Err(reason) => Err(reason),
    }
}

fn branch_read_reason(error: &BranchRouteReadErrorV1) -> (&'static str, bool) {
    use crate::branch::LocalBranchSnapshotErrorV1;

    match error {
        BranchRouteReadErrorV1::Capacity => ("branch_read_capacity_unavailable", true),
        BranchRouteReadErrorV1::Task => ("branch_read_failed", true),
        BranchRouteReadErrorV1::Ref(LocalBranchSnapshotErrorV1::InvalidReference { .. }) => {
            ("branch_ref_invalid", false)
        }
        BranchRouteReadErrorV1::Ref(LocalBranchSnapshotErrorV1::NotFound { .. }) => {
            ("branch_ref_not_found", false)
        }
        BranchRouteReadErrorV1::Ref(LocalBranchSnapshotErrorV1::RepositoryUnavailable) => {
            ("repository_unavailable", true)
        }
        BranchRouteReadErrorV1::Ref(LocalBranchSnapshotErrorV1::ReferenceUnavailable {
            ..
        })
        | BranchRouteReadErrorV1::Ref(LocalBranchSnapshotErrorV1::EnumerationUnavailable) => {
            ("branch_refs_unavailable", true)
        }
        BranchRouteReadErrorV1::Ref(LocalBranchSnapshotErrorV1::InvalidLimit) => {
            ("invalid_request", false)
        }
        BranchRouteReadErrorV1::Ref(LocalBranchSnapshotErrorV1::CapacityExceeded { .. }) => {
            ("branch_read_capacity_unavailable", true)
        }
        BranchRouteReadErrorV1::Ref(LocalBranchSnapshotErrorV1::Cancelled) => ("cancelled", false),
        BranchRouteReadErrorV1::Ref(LocalBranchSnapshotErrorV1::TimedOut) => ("timed_out", true),
    }
}

/// Lists exact local branch refs. A branch name never selects a branch DB.
pub(crate) async fn handle_branch_list(
    cg: &TraceDecay,
    args: Value,
    deadline: Option<tracedecay_application::Deadline>,
    cancellation: Option<tracedecay_application::CancellationSignal>,
) -> Result<ToolResult> {
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(100, |value| {
            value.min(MAX_BRANCH_REFS_PER_READ as u64) as usize
        });
    if limit == 0 {
        return Err(TraceDecayError::Config {
            message: "branch-list limit must be positive".to_owned(),
        });
    }
    let after = args
        .get("after")
        .and_then(Value::as_str)
        .filter(|after| !after.is_empty())
        .map(str::to_owned);
    match run_branch_ref_read(
        cg.project_root().to_path_buf(),
        limit,
        after,
        deadline,
        cancellation,
        crate::branch::local_branch_snapshots_controlled,
    )
    .await
    {
        Ok(page) => {
            let snapshots = page
                .snapshots
                .into_iter()
                .map(|snapshot| {
                    json!({
                        "branch": snapshot.name,
                        "source_revision": snapshot.commit,
                        "source_tree": snapshot.tree,
                    })
                })
                .collect::<Vec<_>>();
            let result = json!({
                "status": if page.truncated { "partial" } else { "complete" },
                "reason": page.truncated.then_some("reference_limit"),
                "snapshot_count": snapshots.len(),
                "examined": page.examined,
                "limit": limit,
                "next_after": page.next_after,
                "snapshots": snapshots,
            });
            Ok(generic_tool_result(
                Some(cg.project_root()),
                &args,
                &result,
                vec![],
            ))
        }
        Err(error) => {
            let (reason, retryable) = branch_read_reason(&error);
            Ok(generic_tool_result(
                Some(cg.project_root()),
                &args,
                &json!({
                    "status": "unavailable",
                    "reason": reason,
                    "retryable": retryable,
                }),
                vec![],
            )
            .with_semantic_error(true)
            .with_failure_message("local branch snapshots are unavailable"))
        }
    }
}

fn branch_reference_unavailable(
    cg: &TraceDecay,
    args: &Value,
    field: &str,
    branch: &str,
    error: &BranchRouteReadErrorV1,
) -> ToolResult {
    let (reason, retryable) = branch_read_reason(error);
    generic_tool_result(
        Some(cg.project_root()),
        args,
        &json!({
            "status": "unavailable",
            field: branch,
            "reason": reason,
            "retryable": retryable,
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
    let (reason, retryable) = branch_unavailable_wire(unavailable.reason);
    generic_tool_result(
        Some(cg.project_root()),
        args,
        &json!({
            "status": "unavailable",
            "branch": branch,
            "source_revision": revision.as_str(),
            "code_generation": unavailable.code_generation,
            "reason": reason,
            "retryable": retryable,
        }),
        vec![],
    )
    .with_semantic_error(true)
    .with_failure_message(format!(
        "branch '{branch}' search is unavailable for commit {}: {reason}",
        revision.as_str()
    ))
}

fn branch_unavailable_wire(
    reason: crate::mcp::server::CodeIndexSearchUnavailableReasonV1,
) -> (&'static str, bool) {
    (
        reason.as_str(),
        matches!(
            reason,
            crate::mcp::server::CodeIndexSearchUnavailableReasonV1::GenerationUnavailable
                | crate::mcp::server::CodeIndexSearchUnavailableReasonV1::CapacityUnavailable
        ),
    )
}

fn branch_search_page_status(has_more: bool) -> (&'static str, Option<&'static str>) {
    if has_more {
        ("partial", Some("result_limit"))
    } else {
        ("complete", None)
    }
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
        .map(str::to_owned)
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: branch".to_string(),
        })?;
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .filter(|query| !query.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: query".to_string(),
        })?;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(10, |value| value.min(500) as usize);
    let cursor = super::super::support::retrieval_cursor(&args)?;
    let revision_branch = branch.clone();
    let revision = match run_branch_ref_read(
        cg.project_root().to_path_buf(),
        1,
        None,
        deadline.clone(),
        cancellation.clone(),
        move |root, control| {
            crate::branch::local_branch_revision_controlled(root, &revision_branch, control)
        },
    )
    .await
    {
        Ok(revision) => revision,
        Err(error) => {
            return Ok(branch_reference_unavailable(
                cg, &args, "branch", &branch, &error,
            ));
        }
    };
    let Some(executor) = executor else {
        return Ok(branch_search_unavailable(
            cg,
            &args,
            &branch,
            &revision.commit,
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
    let source_reference =
        tracedecay_domain::RefId::new(format!("refs/heads/{branch}")).map_err(|error| {
            TraceDecayError::Config {
                message: format!("invalid branch reference: {error}"),
            }
        })?;
    match executor(crate::mcp::server::CodeIndexSearchRequestV1 {
        project_root: cg.project_root().to_path_buf(),
        query,
        source_revision: Some(revision.commit.clone()),
        source_tree: Some(revision.tree.clone()),
        source_reference: Some(source_reference),
        limit,
        cursor,
        mode: crate::mcp::server::CodeIndexSearchModeV1::FallbackAllowed,
        authority: authority.cloned(),
        deadline,
        cancellation,
    })
    .await
    {
        crate::mcp::server::CodeIndexSearchOutcomeV1::Complete(complete) => {
            let next_cursor = complete
                .next_cursor
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?;
            let (status, reason) = branch_search_page_status(next_cursor.is_some());
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
                        "source_reference": format!("refs/heads/{branch}"),
                        "source_revision": revision.commit.as_str(),
                        "source_tree": revision.tree.as_str(),
                        "code_generation": complete.code_generation,
                    })
                })
                .collect::<Vec<_>>();
            Ok(generic_tool_result(
                Some(cg.project_root()),
                &args,
                &json!({
                    "status": status,
                    "reason": reason,
                    "branch": branch,
                    "source_reference": format!("refs/heads/{branch}"),
                    "source_revision": revision.commit.as_str(),
                    "source_tree": revision.tree.as_str(),
                    "code_generation": complete.code_generation,
                    "next_cursor": next_cursor,
                    "results": results,
                }),
                vec![],
            ))
        }
        crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(unavailable) => Ok(
            branch_search_unavailable(cg, &args, &branch, &revision.commit, &unavailable),
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
    let (reason, retryable) = branch_unavailable_wire(unavailable.reason);
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
            "retryable": retryable,
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
        "symbol_identity": symbol.symbol_identity,
        "symbol_occurrence_id": symbol.symbol_occurrence_id,
        "file_identity": symbol.file_identity,
        "file_occurrence_id": symbol.file_occurrence_id,
        "name": symbol.name,
        "qualified_name": symbol.qualified_name,
        "kind": symbol.kind,
        "file": symbol.file,
        "content_digest": symbol.content_digest,
    })
}

fn branch_change_json(change: &crate::mcp::server::CodeIndexBranchChangeV1) -> Value {
    match change {
        crate::mcp::server::CodeIndexBranchChangeV1::Added { symbol } => json!({
            "change": "added",
            "symbol": branch_symbol_json(symbol),
        }),
        crate::mcp::server::CodeIndexBranchChangeV1::Removed { symbol } => json!({
            "change": "removed",
            "symbol": branch_symbol_json(symbol),
        }),
        crate::mcp::server::CodeIndexBranchChangeV1::Changed { base, head } => json!({
            "change": "changed",
            "base": branch_symbol_json(base),
            "head": branch_symbol_json(head),
        }),
    }
}

fn branch_change_files(change: &crate::mcp::server::CodeIndexBranchChangeV1) -> [&str; 2] {
    match change {
        crate::mcp::server::CodeIndexBranchChangeV1::Added { symbol }
        | crate::mcp::server::CodeIndexBranchChangeV1::Removed { symbol } => {
            [symbol.file.as_str(), symbol.file.as_str()]
        }
        crate::mcp::server::CodeIndexBranchChangeV1::Changed { base, head } => {
            [base.file.as_str(), head.file.as_str()]
        }
    }
}

fn branch_change_counts(
    changes: &[crate::mcp::server::CodeIndexBranchChangeV1],
) -> (usize, usize, usize) {
    changes
        .iter()
        .fold((0, 0, 0), |counts, change| match change {
            crate::mcp::server::CodeIndexBranchChangeV1::Added { .. } => {
                (counts.0 + 1, counts.1, counts.2)
            }
            crate::mcp::server::CodeIndexBranchChangeV1::Removed { .. } => {
                (counts.0, counts.1 + 1, counts.2)
            }
            crate::mcp::server::CodeIndexBranchChangeV1::Changed { .. } => {
                (counts.0, counts.1, counts.2 + 1)
            }
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
        .map(str::to_owned)
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: base".to_string(),
        })?;
    let head_name = args
        .get("head")
        .and_then(Value::as_str)
        .or_else(|| cg.active_branch())
        .filter(|head| !head.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| TraceDecayError::Config {
            message: "cannot determine head branch — specify it explicitly".to_string(),
        })?;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(100, |value| {
            value.min(crate::mcp::server::CODE_INDEX_BRANCH_DIFF_MAX_RESULTS_V1 as u64) as usize
        });
    if limit == 0 {
        return Err(TraceDecayError::Config {
            message: "branch-diff limit must be positive".to_owned(),
        });
    }
    let cursor = args
        .get("cursor")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if cursor.as_ref().is_some_and(|cursor| cursor.len() > 4_096) {
        return Err(TraceDecayError::Config {
            message: "branch-diff cursor exceeds its byte bound".to_owned(),
        });
    }
    let resolution_base = base_name.clone();
    let resolution_head = head_name.clone();
    let (base_revision, head_revision) = match run_branch_ref_read(
        cg.project_root().to_path_buf(),
        1,
        None,
        deadline.clone(),
        cancellation.clone(),
        move |root, control| {
            let base =
                crate::branch::local_branch_revision_controlled(root, &resolution_base, control)?;
            let head =
                crate::branch::local_branch_revision_controlled(root, &resolution_head, control)?;
            Ok((base, head))
        },
    )
    .await
    {
        Ok(revisions) => revisions,
        Err(error) => {
            return Ok(branch_reference_unavailable(
                cg,
                &args,
                "base_or_head",
                &format!("{base_name}..{head_name}"),
                &error,
            ));
        }
    };
    let Some(executor) = executor else {
        return Ok(branch_diff_unavailable(
            cg,
            &args,
            (&base_name, &base_revision.commit),
            (&head_name, &head_revision.commit),
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
        base_reference: tracedecay_domain::RefId::new(format!("refs/heads/{base_name}")).map_err(
            |error| TraceDecayError::Config {
                message: format!("invalid base branch reference: {error}"),
            },
        )?,
        base_revision: base_revision.commit.clone(),
        base_tree: base_revision.tree.clone(),
        head_reference: tracedecay_domain::RefId::new(format!("refs/heads/{head_name}")).map_err(
            |error| TraceDecayError::Config {
                message: format!("invalid head branch reference: {error}"),
            },
        )?,
        head_revision: head_revision.commit.clone(),
        head_tree: head_revision.tree.clone(),
        file_filter: args.get("file").and_then(Value::as_str).map(str::to_owned),
        kind_filter: args.get("kind").and_then(Value::as_str).map(str::to_owned),
        limit,
        cursor,
        authority: authority.cloned(),
        deadline,
        cancellation,
    })
    .await
    {
        crate::mcp::server::CodeIndexBranchDiffOutcomeV1::Complete(completed) => {
            let (added, removed, changed) = branch_change_counts(&completed.changes);
            let changes = completed
                .changes
                .iter()
                .map(branch_change_json)
                .collect::<Vec<_>>();
            let touched = unique_file_paths(completed.changes.iter().flat_map(branch_change_files));
            Ok(generic_tool_result(
                Some(cg.project_root()),
                &args,
                &json!({
                    "status": "complete",
                    "base": base_name,
                    "head": head_name,
                    "base_revision": base_revision.commit.as_str(),
                    "base_tree": base_revision.tree.as_str(),
                    "head_revision": head_revision.commit.as_str(),
                    "head_tree": head_revision.tree.as_str(),
                    "base_generation": completed.base_generation,
                    "head_generation": completed.head_generation,
                    "total_changes": completed.total_changes,
                    "summary": {
                        "added": added,
                        "removed": removed,
                        "changed": changed,
                    },
                    "changes": changes,
                }),
                touched,
            ))
        }
        crate::mcp::server::CodeIndexBranchDiffOutcomeV1::Partial(partial) => {
            let (added, removed, changed) = branch_change_counts(&partial.changes);
            let changes = partial
                .changes
                .iter()
                .map(branch_change_json)
                .collect::<Vec<_>>();
            let touched = unique_file_paths(partial.changes.iter().flat_map(branch_change_files));
            Ok(generic_tool_result(
                Some(cg.project_root()),
                &args,
                &json!({
                    "status": "partial",
                    "reason": partial.reason.as_str(),
                    "base": base_name,
                    "head": head_name,
                    "base_revision": base_revision.commit.as_str(),
                    "base_tree": base_revision.tree.as_str(),
                    "head_revision": head_revision.commit.as_str(),
                    "head_tree": head_revision.tree.as_str(),
                    "base_generation": partial.base_generation,
                    "head_generation": partial.head_generation,
                    "total_changes": partial.total_changes,
                    "next_cursor": partial.next_cursor,
                    "summary": {
                        "added": added,
                        "removed": removed,
                        "changed": changed,
                    },
                    "changes": changes,
                }),
                touched,
            ))
        }
        crate::mcp::server::CodeIndexBranchDiffOutcomeV1::Unavailable(unavailable) => {
            Ok(branch_diff_unavailable(
                cg,
                &args,
                (&base_name, &base_revision.commit),
                (&head_name, &head_revision.commit),
                &unavailable,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_search_continuation_is_reported_as_partial() {
        assert_eq!(
            branch_search_page_status(true),
            ("partial", Some("result_limit"))
        );
        assert_eq!(branch_search_page_status(false), ("complete", None));
    }

    #[test]
    fn corruption_reset_required_has_a_stable_non_retryable_wire_code() {
        assert_eq!(
            branch_unavailable_wire(
                crate::mcp::server::CodeIndexSearchUnavailableReasonV1::CorruptionResetRequired,
            ),
            ("index_corruption_reset_required", false),
        );
    }

    #[tokio::test]
    async fn branch_ref_route_reports_capacity_without_queueing() {
        let first = Arc::clone(&BRANCH_REF_READ_ADMISSION)
            .acquire_owned()
            .await
            .expect("first permit");
        let second = Arc::clone(&BRANCH_REF_READ_ADMISSION)
            .acquire_owned()
            .await
            .expect("second permit");
        let result = run_branch_ref_read(
            std::path::PathBuf::from("/unread"),
            1,
            None,
            None,
            None,
            |_root, _control| Ok(()),
        )
        .await;

        assert!(matches!(result, Err(BranchRouteReadErrorV1::Capacity)));
        drop((first, second));
    }

    #[tokio::test]
    async fn cancelled_branch_ref_read_owns_worker_until_settlement() {
        let cancellation =
            tracedecay_application::CancellationSignal::active("branch-ref-owned-settlement")
                .expect("cancellation");
        let worker_cancellation = cancellation.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let mut read = tokio::spawn(run_branch_ref_read(
            std::path::PathBuf::from("/fixture"),
            1,
            None,
            None,
            Some(worker_cancellation),
            move |_root, _control| {
                started_tx.send(()).expect("worker started");
                release_rx.recv().expect("release worker");
                Ok(())
            },
        ));
        started_rx.await.expect("blocking worker started");
        cancellation.cancel(tracedecay_application::clock::now_micros());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(25), &mut read)
                .await
                .is_err(),
            "cancellation observation must not detach the blocking ref worker"
        );
        release_tx.send(()).expect("release blocking worker");
        assert!(matches!(
            read.await.expect("branch read task"),
            Err(BranchRouteReadErrorV1::Ref(
                crate::branch::LocalBranchSnapshotErrorV1::Cancelled
            ))
        ));
    }
}
