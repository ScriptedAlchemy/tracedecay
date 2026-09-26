//! Exact immutable branch-snapshot reads.

use std::path::Path;
use std::sync::{Arc, LazyLock};

use super::*;
use tracedecay_contracts::retrieval::{
    BranchDiffCompleteV1, BranchDiffPartialV1, BranchDiffReferenceUnavailableV1,
    BranchDiffSummaryV1, BranchDiffSurfaceRequestV1, BranchDiffUnavailableV1, BranchListPageV1,
    BranchListSurfaceRequestV1, BranchReadUnavailableV1, BranchReferenceUnavailableV1,
    BranchSearchHitV1, BranchSearchPageV1, BranchSearchSurfaceRequestV1, BranchSearchUnavailableV1,
    BranchSnapshotEntryV1, BranchSymbolChangeV1, BranchSymbolV1, GitPageStatusV1,
    GitReadCompleteV1, GitReadPartialV1, GitReadUnavailableV1, GitReferenceLimitV1,
    GitResultLimitV1,
};

const MAX_BRANCH_REFS_PER_READ: usize = 128;
static BRANCH_REF_READ_ADMISSION: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(2)));

enum BranchRouteReadErrorV1 {
    Capacity,
    Task,
    Ref(tracedecay_contracts::branch_snapshots::LocalBranchSnapshotErrorV1),
}

async fn run_branch_ref_read<T, F>(
    project_root: std::path::PathBuf,
    max_refs: usize,
    after: Option<String>,
    deadline: Option<tracedecay_contracts::Deadline>,
    cancellation: Option<tracedecay_contracts::CancellationSignal>,
    operation: F,
) -> std::result::Result<T, BranchRouteReadErrorV1>
where
    T: Send + 'static,
    F: FnOnce(
            &Path,
            &tracedecay_contracts::branch_snapshots::LocalBranchReadControlV1,
        ) -> std::result::Result<
            T,
            tracedecay_contracts::branch_snapshots::LocalBranchSnapshotErrorV1,
        > + Send
        + 'static,
{
    let permit = Arc::clone(&BRANCH_REF_READ_ADMISSION)
        .try_acquire_owned()
        .map_err(|_| BranchRouteReadErrorV1::Capacity)?;
    let terminal_control = tracedecay_contracts::branch_snapshots::LocalBranchReadControlV1 {
        max_refs,
        after: after.clone(),
        deadline: deadline.clone(),
        cancellation: cancellation.clone(),
    };
    let task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        operation(
            &project_root,
            &tracedecay_contracts::branch_snapshots::LocalBranchReadControlV1 {
                max_refs,
                after,
                deadline,
                cancellation,
            },
        )
        .map_err(BranchRouteReadErrorV1::Ref)
    });
    match tracedecay_code_index_runtime::code_index_task_support::settle_owned_blocking_task(
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
    use tracedecay_contracts::branch_snapshots::LocalBranchSnapshotErrorV1;

    match error {
        // Both ceilings are the same answer to the caller: the local
        // admission semaphore refused the read, or the ref walk exceeded its
        // own bound. Either way the route is over capacity and retryable.
        BranchRouteReadErrorV1::Capacity
        | BranchRouteReadErrorV1::Ref(LocalBranchSnapshotErrorV1::CapacityExceeded { .. }) => {
            ("branch_read_capacity_unavailable", true)
        }
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
        BranchRouteReadErrorV1::Ref(
            LocalBranchSnapshotErrorV1::ReferenceUnavailable { .. }
            | LocalBranchSnapshotErrorV1::EnumerationUnavailable,
        ) => ("branch_refs_unavailable", true),
        BranchRouteReadErrorV1::Ref(LocalBranchSnapshotErrorV1::InvalidLimit) => {
            ("invalid_request", false)
        }
        BranchRouteReadErrorV1::Ref(LocalBranchSnapshotErrorV1::Cancelled) => ("cancelled", false),
        BranchRouteReadErrorV1::Ref(LocalBranchSnapshotErrorV1::TimedOut) => ("timed_out", true),
    }
}

fn branch_read_unavailable(error: &BranchRouteReadErrorV1) -> BranchReadUnavailableV1 {
    let (reason, retryable) = branch_read_reason(error);
    BranchReadUnavailableV1 {
        status: GitReadUnavailableV1::Unavailable,
        reason: reason.to_owned(),
        retryable,
    }
}

/// Lists exact local branch refs. A branch name never selects a branch DB.
#[hotpath::measure(future = true, label = "mcp.git.branch_list.total")]
pub async fn compute_branch_list(
    ctx: &McpToolContext<'_>,
    args: Value,
) -> Result<GraphToolCompletionV1> {
    let request: BranchListSurfaceRequestV1 =
        decode_primitive_request(&args, "tracedecay_branch_list")?;
    let deadline = ctx.deadline().cloned();
    let cancellation = ctx.cancellation().cloned();
    let limit = request.limit.map_or(100, |value| {
        usize::try_from(value).map_or(MAX_BRANCH_REFS_PER_READ, |value| {
            value.min(MAX_BRANCH_REFS_PER_READ)
        })
    });
    if limit == 0 {
        return Err(TraceDecayError::Config {
            message: "branch-list limit must be positive".to_owned(),
        });
    }
    let after = request.after.filter(|after| !after.is_empty());
    let result = match hotpath::future!(
        run_branch_ref_read(
            ctx.project_root().to_path_buf(),
            limit,
            after,
            deadline,
            cancellation,
            tracedecay_query::native_git::local_branch_snapshots_controlled,
        ),
        label = "mcp.git.branch_list.ref_read"
    )
    .await
    {
        Ok(page) => BranchListResultV1::Page(BranchListPageV1 {
            status: if page.truncated {
                GitPageStatusV1::Partial
            } else {
                GitPageStatusV1::Complete
            },
            reason: page
                .truncated
                .then_some(GitReferenceLimitV1::ReferenceLimit),
            snapshot_count: page.snapshots.len(),
            examined: page.examined,
            limit,
            next_after: page.next_after,
            snapshots: page
                .snapshots
                .into_iter()
                .map(|snapshot| BranchSnapshotEntryV1 {
                    branch: snapshot.name,
                    source_revision: snapshot.commit,
                    source_tree: snapshot.tree,
                })
                .collect(),
        }),
        Err(error) => BranchListResultV1::Unavailable(branch_read_unavailable(&error)),
    };
    Ok(graph_tool_completion(
        GraphToolResultV1::BranchList(result),
        Vec::new(),
    ))
}

fn branch_unavailable_wire(
    reason: tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1,
) -> (&'static str, bool) {
    (
        reason.as_str(),
        matches!(
            reason,
            tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::GenerationUnavailable
                | tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::CapacityUnavailable
        ),
    )
}

fn branch_search_unavailable(
    branch: String,
    revision: &tracedecay_domain::GitOidV1,
    code_generation: Option<String>,
    reason: tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1,
) -> BranchSearchResultV1 {
    let (reason, retryable) = branch_unavailable_wire(reason);
    BranchSearchResultV1::SearchUnavailable(BranchSearchUnavailableV1 {
        status: GitReadUnavailableV1::Unavailable,
        branch,
        source_revision: revision.as_str().to_owned(),
        code_generation,
        reason: reason.to_owned(),
        retryable,
    })
}

fn exact_branch_source(
    ctx: &McpToolContext<'_>,
    branch: &str,
    revision: &tracedecay_domain::GitOidV1,
) -> Result<(std::path::PathBuf, tracedecay_domain::RefId)> {
    let source =
        tracedecay_runtime_core::branch_meta::load_branch_meta(&ctx.store_layout().data_root)
            .and_then(|meta| {
                meta.branches
                    .get(branch)
                    .and_then(|entry| entry.graph_source.clone())
            })
            .filter(|source| source.source_oid == revision.as_str());
    let (project_root, reference) = source.map_or_else(
        || {
            (
                ctx.project_root().to_path_buf(),
                format!("refs/heads/{branch}"),
            )
        },
        |source| {
            (
                std::path::PathBuf::from(source.worktree_root),
                source.reference,
            )
        },
    );
    let reference =
        tracedecay_domain::RefId::new(reference).map_err(|error| TraceDecayError::Config {
            message: format!("invalid branch source reference: {error}"),
        })?;
    Ok((project_root, reference))
}

/// Searches the generation sealed for the selected local ref's exact commit.
#[hotpath::measure(future = true, label = "mcp.git.branch_search.total")]
pub async fn compute_branch_search(
    ctx: &McpToolContext<'_>,
    args: Value,
) -> Result<GraphToolCompletionV1> {
    let request: BranchSearchSurfaceRequestV1 =
        decode_primitive_request(&args, "tracedecay_branch_search")?;
    let executor = ctx.code_index_search_executor();
    let authority = ctx.code_index_search_authority();
    let deadline = ctx.deadline().cloned();
    let cancellation = ctx.cancellation().cloned();
    let branch = Some(request.branch)
        .filter(|branch| !branch.is_empty())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: branch".to_string(),
        })?;
    let query = Some(request.query)
        .filter(|query| !query.is_empty())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: query".to_string(),
        })?;
    let limit = request.limit.map_or(10, |value| {
        usize::try_from(value).map_or(500, |value| value.min(500))
    });
    let cursor = crate::handlers::support::retrieval_cursor(&args)?;
    let revision_branch = branch.clone();
    let revision = match hotpath::future!(
        run_branch_ref_read(
            ctx.project_root().to_path_buf(),
            1,
            None,
            deadline.clone(),
            cancellation.clone(),
            move |root, control| {
                tracedecay_query::native_git::local_branch_revision_controlled(
                    root,
                    &revision_branch,
                    control,
                )
            },
        ),
        label = "mcp.git.branch_search.ref_read"
    )
    .await
    {
        Ok(revision) => revision,
        Err(error) => {
            let (reason, retryable) = branch_read_reason(&error);
            let result = BranchSearchResultV1::ReferenceUnavailable(BranchReferenceUnavailableV1 {
                status: GitReadUnavailableV1::Unavailable,
                branch,
                reason: reason.to_owned(),
                retryable,
            });
            return Ok(graph_tool_completion(
                GraphToolResultV1::BranchSearch(result),
                Vec::new(),
            ));
        }
    };
    let Some(executor) = executor else {
        let result = branch_search_unavailable(
            branch,
            &revision.commit,
            None,
            tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::CapabilityUnavailable,
        );
        return Ok(graph_tool_completion(
            GraphToolResultV1::BranchSearch(result),
            Vec::new(),
        ));
    };
    let (source_root, source_reference) = exact_branch_source(ctx, &branch, &revision.commit)?;
    let result = match hotpath::future!(
        executor(tracedecay_query::code_search::CodeIndexSearchRequestV1 {
            project_root: source_root,
            query,
            source_revision: Some(revision.commit.clone()),
            source_tree: Some(revision.tree.clone()),
            source_reference: Some(source_reference),
            limit,
            cursor,
            lexical_routing: tracedecay_query::retrieval::lexical::LexicalRoutingV1::default(),
            authority: authority.cloned(),
            deadline,
            cancellation,
        }),
        label = "mcp.git.branch_search.search"
    )
    .await
    {
        tracedecay_query::code_search::CodeIndexSearchOutcomeV1::Complete(complete) => {
            let next_cursor = complete
                .next_cursor
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?;
            let has_more = next_cursor.is_some();
            let source_reference = format!("refs/heads/{branch}");
            let results = hotpath::measure_block!("mcp.git.branch_search.assemble", {
                complete
                    .ordered_candidates
                    .iter()
                    .map(|ranked| {
                        let display = complete.display_by_anchor.get(&ranked.candidate.anchor_id);
                        BranchSearchHitV1 {
                            candidate: ranked.clone(),
                            name: display.map(|value| value.name.clone()),
                            qualified_name: display.map(|value| value.qualified_name.clone()),
                            kind: display.map(|value| value.kind.clone()),
                            path: display.map(|value| value.path.clone()),
                            branch: branch.clone(),
                            source_reference: source_reference.clone(),
                            source_revision: revision.commit.as_str().to_owned(),
                            source_tree: revision.tree.as_str().to_owned(),
                            code_generation: complete.code_generation.clone(),
                        }
                    })
                    .collect::<Vec<_>>()
            });
            BranchSearchResultV1::Page(BranchSearchPageV1 {
                status: if has_more {
                    GitPageStatusV1::Partial
                } else {
                    GitPageStatusV1::Complete
                },
                reason: has_more.then_some(GitResultLimitV1::ResultLimit),
                branch,
                source_reference,
                source_revision: revision.commit.as_str().to_owned(),
                source_tree: revision.tree.as_str().to_owned(),
                code_generation: complete.code_generation,
                next_cursor,
                results,
            })
        }
        tracedecay_query::code_search::CodeIndexSearchOutcomeV1::Unavailable(unavailable) => {
            branch_search_unavailable(
                branch,
                &revision.commit,
                unavailable.code_generation,
                unavailable.reason,
            )
        }
    };
    Ok(graph_tool_completion(
        GraphToolResultV1::BranchSearch(result),
        Vec::new(),
    ))
}

fn branch_symbol(
    symbol: &tracedecay_query::code_search::CodeIndexBranchSymbolV1,
) -> BranchSymbolV1 {
    BranchSymbolV1 {
        symbol_identity: symbol.symbol_identity.as_str().to_owned(),
        symbol_occurrence_id: symbol.symbol_occurrence_id.as_str().to_owned(),
        file_identity: symbol.file_identity.as_str().to_owned(),
        file_occurrence_id: symbol.file_occurrence_id.as_str().to_owned(),
        name: symbol.name.clone(),
        qualified_name: symbol.qualified_name.clone(),
        kind: symbol.kind.clone(),
        file: symbol.file.clone(),
        content_digest: symbol.content_digest.clone(),
    }
}

fn branch_change(
    change: &tracedecay_query::code_search::CodeIndexBranchChangeV1,
) -> BranchSymbolChangeV1 {
    match change {
        tracedecay_query::code_search::CodeIndexBranchChangeV1::Added { symbol } => {
            BranchSymbolChangeV1::Added {
                symbol: branch_symbol(symbol),
            }
        }
        tracedecay_query::code_search::CodeIndexBranchChangeV1::Removed { symbol } => {
            BranchSymbolChangeV1::Removed {
                symbol: branch_symbol(symbol),
            }
        }
        tracedecay_query::code_search::CodeIndexBranchChangeV1::Changed { base, head } => {
            BranchSymbolChangeV1::Changed {
                base: Box::new(branch_symbol(base)),
                head: Box::new(branch_symbol(head)),
            }
        }
    }
}

fn branch_change_files(
    change: &tracedecay_query::code_search::CodeIndexBranchChangeV1,
) -> [&str; 2] {
    match change {
        tracedecay_query::code_search::CodeIndexBranchChangeV1::Added { symbol }
        | tracedecay_query::code_search::CodeIndexBranchChangeV1::Removed { symbol } => {
            [symbol.file.as_str(), symbol.file.as_str()]
        }
        tracedecay_query::code_search::CodeIndexBranchChangeV1::Changed { base, head } => {
            [base.file.as_str(), head.file.as_str()]
        }
    }
}

fn branch_change_summary(
    changes: &[tracedecay_query::code_search::CodeIndexBranchChangeV1],
) -> BranchDiffSummaryV1 {
    changes.iter().fold(
        BranchDiffSummaryV1 {
            added: 0,
            removed: 0,
            changed: 0,
        },
        |mut summary, change| {
            match change {
                tracedecay_query::code_search::CodeIndexBranchChangeV1::Added { .. } => {
                    summary.added += 1;
                }
                tracedecay_query::code_search::CodeIndexBranchChangeV1::Removed { .. } => {
                    summary.removed += 1;
                }
                tracedecay_query::code_search::CodeIndexBranchChangeV1::Changed { .. } => {
                    summary.changed += 1;
                }
            }
            summary
        },
    )
}

/// Compares generations sealed for the two selected local refs' exact commits.
#[hotpath::measure(future = true, label = "mcp.git.branch_diff.total")]
pub async fn compute_branch_diff(
    ctx: &McpToolContext<'_>,
    args: Value,
) -> Result<GraphToolCompletionV1> {
    let request: BranchDiffSurfaceRequestV1 =
        decode_primitive_request(&args, "tracedecay_branch_diff")?;
    let executor = ctx.code_index_branch_diff_executor();
    let authority = ctx.code_index_search_authority();
    let deadline = ctx.deadline().cloned();
    let cancellation = ctx.cancellation().cloned();
    let base_name = Some(request.base)
        .filter(|base| !base.is_empty())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: base".to_string(),
        })?;
    let head_name = request
        .head
        .or_else(|| ctx.active_branch().map(str::to_owned))
        .filter(|head| !head.is_empty())
        .ok_or_else(|| TraceDecayError::Config {
            message: "cannot determine head branch. Specify it explicitly".to_string(),
        })?;
    let limit = request.limit.map_or(100, |value| {
        usize::try_from(value).map_or(
            tracedecay_query::code_search::CODE_INDEX_BRANCH_DIFF_MAX_RESULTS_V1,
            |value| value.min(tracedecay_query::code_search::CODE_INDEX_BRANCH_DIFF_MAX_RESULTS_V1),
        )
    });
    if limit == 0 {
        return Err(TraceDecayError::Config {
            message: "branch-diff limit must be positive".to_owned(),
        });
    }
    if request
        .cursor
        .as_ref()
        .is_some_and(|cursor| cursor.len() > 4_096)
    {
        return Err(TraceDecayError::Config {
            message: "branch-diff cursor exceeds its byte bound".to_owned(),
        });
    }
    let resolution_base = base_name.clone();
    let resolution_head = head_name.clone();
    let (base_revision, head_revision) = match hotpath::future!(
        run_branch_ref_read(
            ctx.project_root().to_path_buf(),
            1,
            None,
            deadline.clone(),
            cancellation.clone(),
            move |root, control| {
                let base = tracedecay_query::native_git::local_branch_revision_controlled(
                    root,
                    &resolution_base,
                    control,
                )?;
                let head = tracedecay_query::native_git::local_branch_revision_controlled(
                    root,
                    &resolution_head,
                    control,
                )?;
                Ok((base, head))
            },
        ),
        label = "mcp.git.branch_diff.ref_read"
    )
    .await
    {
        Ok(revisions) => revisions,
        Err(error) => {
            let (reason, retryable) = branch_read_reason(&error);
            let result =
                BranchDiffResultV1::ReferenceUnavailable(BranchDiffReferenceUnavailableV1 {
                    status: GitReadUnavailableV1::Unavailable,
                    base_or_head: format!("{base_name}..{head_name}"),
                    reason: reason.to_owned(),
                    retryable,
                });
            return Ok(graph_tool_completion(
                GraphToolResultV1::BranchDiff(result),
                Vec::new(),
            ));
        }
    };
    let diff_unavailable = |base_generation, head_generation, reason| {
        let (reason, retryable) = branch_unavailable_wire(reason);
        BranchDiffResultV1::DiffUnavailable(BranchDiffUnavailableV1 {
            status: GitReadUnavailableV1::Unavailable,
            base: base_name.clone(),
            head: head_name.clone(),
            base_revision: base_revision.commit.as_str().to_owned(),
            head_revision: head_revision.commit.as_str().to_owned(),
            base_generation,
            head_generation,
            reason: reason.to_owned(),
            retryable,
        })
    };
    let Some(executor) = executor else {
        let result = diff_unavailable(
            None,
            None,
            tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::CapabilityUnavailable,
        );
        return Ok(graph_tool_completion(
            GraphToolResultV1::BranchDiff(result),
            Vec::new(),
        ));
    };
    let outcome = hotpath::future!(
        executor(
            tracedecay_query::code_search::CodeIndexBranchDiffRequestV1 {
                project_root: ctx.project_root().to_path_buf(),
                base_reference: tracedecay_domain::RefId::new(format!("refs/heads/{base_name}"))
                    .map_err(|error| TraceDecayError::Config {
                        message: format!("invalid base branch reference: {error}"),
                    })?,
                base_revision: base_revision.commit.clone(),
                base_tree: base_revision.tree.clone(),
                head_reference: tracedecay_domain::RefId::new(format!("refs/heads/{head_name}"))
                    .map_err(|error| TraceDecayError::Config {
                        message: format!("invalid head branch reference: {error}"),
                    })?,
                head_revision: head_revision.commit.clone(),
                head_tree: head_revision.tree.clone(),
                file_filter: request.file,
                kind_filter: request.kind,
                limit,
                cursor: request.cursor,
                authority: authority.cloned(),
                deadline,
                cancellation,
            }
        ),
        label = "mcp.git.branch_diff.diff"
    )
    .await;
    let (result, touched) = match outcome {
        tracedecay_query::code_search::CodeIndexBranchDiffOutcomeV1::Complete(completed) => {
            let touched = unique_file_paths(completed.changes.iter().flat_map(branch_change_files));
            let result = hotpath::measure_block!(
                "mcp.git.branch_diff.assemble",
                BranchDiffResultV1::Complete(BranchDiffCompleteV1 {
                    status: GitReadCompleteV1::Complete,
                    base: base_name,
                    head: head_name,
                    base_revision: base_revision.commit.as_str().to_owned(),
                    base_tree: base_revision.tree.as_str().to_owned(),
                    head_revision: head_revision.commit.as_str().to_owned(),
                    head_tree: head_revision.tree.as_str().to_owned(),
                    base_generation: completed.base_generation,
                    head_generation: completed.head_generation,
                    total_changes: completed.total_changes,
                    summary: branch_change_summary(&completed.changes),
                    changes: completed.changes.iter().map(branch_change).collect(),
                })
            );
            (result, touched)
        }
        tracedecay_query::code_search::CodeIndexBranchDiffOutcomeV1::Partial(partial) => {
            let touched = unique_file_paths(partial.changes.iter().flat_map(branch_change_files));
            let result = hotpath::measure_block!(
                "mcp.git.branch_diff.assemble",
                BranchDiffResultV1::Partial(BranchDiffPartialV1 {
                    status: GitReadPartialV1::Partial,
                    reason: match partial.reason {
                        tracedecay_query::code_search::CodeIndexBranchDiffPartialReasonV1::ResultLimit => {
                            GitResultLimitV1::ResultLimit
                        }
                    },
                    base: base_name,
                    head: head_name,
                    base_revision: base_revision.commit.as_str().to_owned(),
                    base_tree: base_revision.tree.as_str().to_owned(),
                    head_revision: head_revision.commit.as_str().to_owned(),
                    head_tree: head_revision.tree.as_str().to_owned(),
                    base_generation: partial.base_generation,
                    head_generation: partial.head_generation,
                    total_changes: partial.total_changes,
                    next_cursor: partial.next_cursor,
                    summary: branch_change_summary(&partial.changes),
                    changes: partial.changes.iter().map(branch_change).collect(),
                })
            );
            (result, touched)
        }
        tracedecay_query::code_search::CodeIndexBranchDiffOutcomeV1::Unavailable(unavailable) => (
            diff_unavailable(
                unavailable.base_generation,
                unavailable.head_generation,
                unavailable.reason,
            ),
            Vec::new(),
        ),
    };
    Ok(graph_tool_completion(
        GraphToolResultV1::BranchDiff(result),
        touched,
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::test_support::{
        branched_repository, fixture_context, fixture_project, fixture_project_on_branch,
        ref_read_guard,
    };
    use super::*;
    use crate::ToolResult;
    use crate::handlers::graph_tool::render_graph_tool;

    fn rendered(completion: GraphToolCompletionV1) -> ToolResult {
        render_graph_tool(None, &json!({}), completion).expect("rendered git-context result")
    }

    /// A context with no admitted code-index authority must produce the typed
    /// capability-unavailable answer, not an empty result set that reads like
    /// "this branch contains no matches".
    #[tokio::test]
    async fn branch_search_without_an_admitted_executor_is_capability_unavailable() {
        let _serialized = ref_read_guard().await;
        let repo = branched_repository();
        let project = fixture_project(repo.path());
        let ctx = fixture_context(&project);

        let result = rendered(
            compute_branch_search(&ctx, json!({ "branch": "feature", "query": "after" }))
                .await
                .expect("an unadmitted executor returns a typed result, not an error"),
        );

        assert_eq!(result.semantic_error(), Some(true));
        let message = result.failure_message().unwrap_or_default();
        assert!(
            message.contains("search is unavailable") && message.contains("code_index_unavailable"),
            "absent code-index authority must be named as such, got {message:?}"
        );
    }

    /// The same absence on the branch-diff route, which reads a different
    /// executor slot off the same context.
    #[tokio::test]
    async fn branch_diff_without_an_admitted_executor_is_capability_unavailable() {
        let _serialized = ref_read_guard().await;
        let repo = branched_repository();
        let project = fixture_project(repo.path());
        let ctx = fixture_context(&project);

        let result = rendered(
            compute_branch_diff(&ctx, json!({ "base": "main", "head": "feature" }))
                .await
                .expect("an unadmitted executor returns a typed result, not an error"),
        );

        assert_eq!(result.semantic_error(), Some(true));
        let message = result.failure_message().unwrap_or_default();
        assert!(
            message.contains("branch diff main..feature is unavailable")
                && message.contains("code_index_unavailable"),
            "absent code-index authority must be named as such, got {message:?}"
        );
    }

    /// Branch diff takes its head from the context's active branch when the
    /// caller names only a base, and reports a typed argument error when
    /// neither the arguments nor the context resolve one.
    #[tokio::test]
    async fn branch_diff_head_comes_from_the_context_active_branch() {
        let _serialized = ref_read_guard().await;
        let repo = branched_repository();

        let without_project = fixture_project(repo.path());
        let without_branch = compute_branch_diff(
            &fixture_context(&without_project),
            json!({ "base": "main" }),
        )
        .await;
        assert!(matches!(
            without_branch,
            Err(TraceDecayError::Config { .. })
        ));

        let with_project = fixture_project_on_branch(repo.path(), "feature");
        let with_branch = rendered(
            compute_branch_diff(&fixture_context(&with_project), json!({ "base": "main" }))
                .await
                .expect("the context's active branch resolves head"),
        );
        assert_eq!(with_branch.semantic_error(), Some(true));
    }

    #[test]
    fn corruption_reset_required_has_a_stable_non_retryable_wire_code() {
        assert_eq!(
            branch_unavailable_wire(
                tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::CorruptionResetRequired,
            ),
            ("index_corruption_reset_required", false),
        );
    }

    #[tokio::test]
    async fn branch_ref_route_reports_capacity_without_queueing() {
        let _serialized = ref_read_guard().await;
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
        let _serialized = ref_read_guard().await;
        let cancellation =
            tracedecay_contracts::CancellationSignal::active("branch-ref-owned-settlement")
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
        cancellation.cancel(tracedecay_contracts::clock::now_micros());
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
                tracedecay_contracts::branch_snapshots::LocalBranchSnapshotErrorV1::Cancelled
            ))
        ));
    }
}
