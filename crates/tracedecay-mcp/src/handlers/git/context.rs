//! `tracedecay_diff_context`, `tracedecay_changelog`, `tracedecay_commit_context`, and `tracedecay_pr_context`.

use super::super::dependency_hints;
use super::affected::collect_verified_affected_test_files;
use super::pr_context_cursor::{
    PrContextCursorBinding, PrContextCursorComparison, decode_pr_context_cursor,
    encode_pr_context_cursor, pr_context_cursor_authority,
};
use super::shell::{
    classify_file_role, default_pr_base_ref, git_changed_files, git_diff_file_changes,
    git_pr_comparison_controlled, git_recent_commits,
};
use super::*;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1;
use tracedecay_contracts::retrieval::{
    ChangelogCompleteV1, ChangelogPartialV1, ChangelogSurfaceRequestV1, CommitCategoryV1,
    CommitContextSummaryV1, CommitContextSurfaceRequestV1, CommitFileRoleV1, CommitSymbolEntryV1,
    CommitSymbolV1, ConfigSummaryKindV1, ConfigSummaryV1, DiffContextResultV1,
    DiffContextSurfaceRequestV1, GitComparedSymbolV1, GitContextSymbolV1, GitReadCompleteV1,
    GitReadPartialV1, GitReadUnavailableV1, PrAnalysisCoverageV1, PrContextCompleteV1,
    PrContextGraphPendingV1, PrContextSurfaceRequestV1, PrContextSymbolsUnavailableV1,
    PrCoverageSelectionV1, PrSelectionCoverageV1, PrSymbolChangesCompleteV1, PrSymbolEntryV1,
    PrSymbolPageV1, PrSymbolSelectionV1, SymbolChangesCompleteV1, SymbolChangesUnavailableV1,
};
use tracedecay_contracts::{InvocationAnalyticsV1, PrContextAnalyticsV1, PrContextStageTimingsV1};
use tracedecay_domain::{RelationEdgeKindV1, SymbolOccurrenceId};
use tracedecay_graph_query::VerifiedGraphQuery;

const VERIFIED_GRAPH_MAX_SYMBOLS: usize = 500_000;
const VERIFIED_GRAPH_MAX_RELATIONS: usize = 2_000_000;

struct SemanticSymbolDiff {
    base_generation: String,
    head_generation: String,
    added: Vec<tracedecay_query::code_search::CodeIndexBranchSymbolV1>,
    removed: Vec<tracedecay_query::code_search::CodeIndexBranchSymbolV1>,
    modified: Vec<tracedecay_query::code_search::CodeIndexBranchSymbolV1>,
}

struct SemanticSymbolDiffUnavailable {
    reason: &'static str,
    retryable: bool,
}

impl SemanticSymbolDiffUnavailable {
    fn coverage(&self) -> SymbolChangesUnavailableV1 {
        SymbolChangesUnavailableV1 {
            status: GitReadUnavailableV1::Unavailable,
            reason: self.reason.to_owned(),
            retryable: self.retryable,
        }
    }
}

fn exact_local_branch(reference: &str, active_branch: Option<&str>) -> Option<String> {
    match reference {
        "HEAD" => active_branch.map(str::to_owned),
        reference if reference.starts_with("refs/heads/") => reference
            .strip_prefix("refs/heads/")
            .filter(|branch| !branch.is_empty())
            .map(str::to_owned),
        reference
            if !reference.is_empty()
                && !reference.starts_with("refs/")
                && !reference.contains(['~', '^', ':']) =>
        {
            Some(reference.to_owned())
        }
        _ => None,
    }
}

fn local_branch_read_reason(
    error: &tracedecay_contracts::branch_snapshots::LocalBranchSnapshotErrorV1,
) -> SemanticSymbolDiffUnavailable {
    use tracedecay_contracts::branch_snapshots::LocalBranchSnapshotErrorV1;
    let (reason, retryable) = match error {
        LocalBranchSnapshotErrorV1::InvalidReference { .. } => ("branch_ref_invalid", false),
        LocalBranchSnapshotErrorV1::NotFound { .. } => ("branch_ref_not_found", false),
        LocalBranchSnapshotErrorV1::RepositoryUnavailable => ("repository_unavailable", true),
        LocalBranchSnapshotErrorV1::ReferenceUnavailable { .. }
        | LocalBranchSnapshotErrorV1::EnumerationUnavailable => ("branch_refs_unavailable", true),
        LocalBranchSnapshotErrorV1::InvalidLimit => ("invalid_request", false),
        LocalBranchSnapshotErrorV1::CapacityExceeded { .. } => {
            ("branch_read_capacity_unavailable", true)
        }
        LocalBranchSnapshotErrorV1::Cancelled => ("cancelled", false),
        LocalBranchSnapshotErrorV1::TimedOut => ("timed_out", true),
    };
    SemanticSymbolDiffUnavailable { reason, retryable }
}

fn compared_symbol(
    symbol: &tracedecay_query::code_search::CodeIndexBranchSymbolV1,
) -> GitComparedSymbolV1 {
    GitComparedSymbolV1 {
        id: symbol.symbol_occurrence_id.as_str().to_owned(),
        name: symbol.name.clone(),
        qualified_name: symbol.qualified_name.clone(),
        kind: symbol.kind.clone(),
        file: symbol.file.clone(),
        content_digest: symbol.content_digest.clone(),
    }
}

async fn exact_semantic_symbol_diff(
    ctx: &McpToolContext<'_>,
    base_ref: &str,
    head_ref: &str,
    expected_base_revision: Option<&str>,
    expected_head_revision: Option<&str>,
) -> std::result::Result<SemanticSymbolDiff, SemanticSymbolDiffUnavailable> {
    use tracedecay_query::code_search::{
        CODE_INDEX_BRANCH_DIFF_MAX_RESULTS_V1, CodeIndexBranchChangeV1,
        CodeIndexBranchDiffOutcomeV1, CodeIndexBranchDiffRequestV1,
        CodeIndexSearchUnavailableReasonV1,
    };

    let Some(base_branch) = exact_local_branch(base_ref, ctx.active_branch()) else {
        return Err(SemanticSymbolDiffUnavailable {
            reason: "exact_local_branch_required",
            retryable: false,
        });
    };
    let Some(head_branch) = exact_local_branch(head_ref, ctx.active_branch()) else {
        return Err(SemanticSymbolDiffUnavailable {
            reason: "exact_local_branch_required",
            retryable: false,
        });
    };
    let control = tracedecay_contracts::branch_snapshots::LocalBranchReadControlV1 {
        max_refs: 1,
        after: None,
        deadline: ctx.deadline().cloned(),
        cancellation: ctx.cancellation().cloned(),
    };
    let project_root = ctx.project_root().to_path_buf();
    let resolution_base = base_branch.clone();
    let resolution_head = head_branch.clone();
    let revisions = blocking_git_span_controlled(
        "semantic branch revisions",
        ctx.cancellation().cloned(),
        ctx.deadline().cloned(),
        move |_| {
            let base = tracedecay_query::native_git::local_branch_revision_controlled(
                &project_root,
                &resolution_base,
                &control,
            )?;
            let head = tracedecay_query::native_git::local_branch_revision_controlled(
                &project_root,
                &resolution_head,
                &control,
            )?;
            Ok::<_, tracedecay_contracts::branch_snapshots::LocalBranchSnapshotErrorV1>((
                base, head,
            ))
        },
    )
    .await
    .map_err(|_| SemanticSymbolDiffUnavailable {
        reason: "branch_read_failed",
        retryable: true,
    })?
    .map_err(|error| local_branch_read_reason(&error))?;
    if expected_base_revision.is_some_and(|expected| revisions.0.commit.as_str() != expected)
        || expected_head_revision.is_some_and(|expected| revisions.1.commit.as_str() != expected)
    {
        return Err(SemanticSymbolDiffUnavailable {
            reason: "comparison_revision_not_indexed_as_local_branch",
            retryable: false,
        });
    }
    let Some(executor) = ctx.code_index_branch_diff_executor() else {
        return Err(SemanticSymbolDiffUnavailable {
            reason: CodeIndexSearchUnavailableReasonV1::CapabilityUnavailable.as_str(),
            retryable: false,
        });
    };
    let base_reference = tracedecay_domain::RefId::new(format!("refs/heads/{base_branch}"))
        .map_err(|_| SemanticSymbolDiffUnavailable {
            reason: "branch_ref_invalid",
            retryable: false,
        })?;
    let head_reference = tracedecay_domain::RefId::new(format!("refs/heads/{head_branch}"))
        .map_err(|_| SemanticSymbolDiffUnavailable {
            reason: "branch_ref_invalid",
            retryable: false,
        })?;
    let mut cursor = None;
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut modified = Vec::new();
    let (base_generation, head_generation) = loop {
        let outcome = executor(CodeIndexBranchDiffRequestV1 {
            project_root: ctx.project_root().to_path_buf(),
            base_reference: base_reference.clone(),
            base_revision: revisions.0.commit.clone(),
            base_tree: revisions.0.tree.clone(),
            head_reference: head_reference.clone(),
            head_revision: revisions.1.commit.clone(),
            head_tree: revisions.1.tree.clone(),
            file_filter: None,
            kind_filter: None,
            limit: CODE_INDEX_BRANCH_DIFF_MAX_RESULTS_V1,
            cursor,
            authority: ctx.code_index_search_authority().cloned(),
            deadline: ctx.deadline().cloned(),
            cancellation: ctx.cancellation().cloned(),
        })
        .await;
        let (changes, generations, next) = match outcome {
            CodeIndexBranchDiffOutcomeV1::Complete(complete) => (
                complete.changes,
                (complete.base_generation, complete.head_generation),
                None,
            ),
            CodeIndexBranchDiffOutcomeV1::Partial(partial) => (
                partial.changes,
                (partial.base_generation, partial.head_generation),
                Some(partial.next_cursor),
            ),
            CodeIndexBranchDiffOutcomeV1::Unavailable(unavailable) => {
                let retryable = matches!(
                    unavailable.reason,
                    CodeIndexSearchUnavailableReasonV1::GenerationUnavailable
                        | CodeIndexSearchUnavailableReasonV1::CapacityUnavailable
                );
                return Err(SemanticSymbolDiffUnavailable {
                    reason: unavailable.reason.as_str(),
                    retryable,
                });
            }
        };
        for change in changes {
            match change {
                CodeIndexBranchChangeV1::Added { symbol } => added.push(symbol),
                CodeIndexBranchChangeV1::Removed { symbol } => removed.push(symbol),
                CodeIndexBranchChangeV1::Changed { head, .. } => modified.push(head),
            }
        }
        match next {
            Some(next) => cursor = Some(next),
            None => break generations,
        }
    };
    Ok(SemanticSymbolDiff {
        base_generation,
        head_generation,
        added,
        removed,
        modified,
    })
}

fn symbol_path(symbol: &CodeGraphSymbolSummaryV1) -> Result<&str> {
    symbol
        .binding
        .as_ref()
        .and_then(|binding| binding.logical_path.as_deref())
        .ok_or_else(|| {
            TraceDecayError::project_route(
                "verified-code-graph-symbol-binding-incomplete",
                false,
                format!(
                    "symbol {} has no admitted logical file binding",
                    symbol.occurrence.as_str()
                ),
            )
        })
}

fn symbol_metadata(
    symbol: &CodeGraphSymbolSummaryV1,
) -> Result<&tracedecay_code_index::lineage::LineageSymbolRecordV1> {
    symbol.metadata.as_ref().ok_or_else(|| {
        TraceDecayError::project_route(
            "verified-code-graph-symbol-metadata-incomplete",
            false,
            format!(
                "symbol {} has no admitted lineage metadata",
                symbol.occurrence.as_str()
            ),
        )
    })
}

fn context_symbol(symbol: &CodeGraphSymbolSummaryV1) -> Result<GitContextSymbolV1> {
    let metadata = symbol_metadata(symbol)?;
    Ok(GitContextSymbolV1 {
        id: symbol.occurrence.as_str().to_owned(),
        name: metadata.simple_name.as_str().to_owned(),
        kind: metadata.kind.as_str().to_owned(),
        file: symbol_path(symbol)?.to_owned(),
        line: metadata.start_line,
    })
}

fn all_symbols_in_files(
    graph: &VerifiedGraphQuery,
    files: &HashSet<String>,
) -> Result<Vec<CodeGraphSymbolSummaryV1>> {
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let page = graph.symbols_in_logical_files_page(
        files,
        None,
        VERIFIED_GRAPH_MAX_SYMBOLS,
        VERIFIED_GRAPH_MAX_SYMBOLS,
    )?;
    if page.has_more {
        return Err(TraceDecayError::project_route(
            "verified-code-graph-symbol-budget-exhausted",
            false,
            "the requested Git context exceeds the verified graph symbol budget",
        ));
    }
    Ok(page.symbols)
}

/// Runs one synchronous gix span on the blocking pool.
///
/// Repo open, tree diff, status classification, and rev-walk are all
/// synchronous and unbounded on a large or pathological repository. Running
/// them inline on a runtime worker starves every other request sharing that
/// worker. The sharper problem makes the carried git dispatch deadline
/// unenforceable: `tokio::time::timeout` can only preempt at an await point, so
/// an inline blocking call runs to completion regardless. Awaiting the
/// `spawn_blocking` join handle restores exactly that composition, which
/// `compute_pr_context` already relied on.
async fn blocking_git_span<T, F>(label: &str, work: F) -> Result<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|join_error| TraceDecayError::Config {
            message: format!("git {label} task failed: {join_error}"),
        })
}

struct CancelBlockingGitOnDrop {
    cancelled: Arc<AtomicBool>,
}

impl Drop for CancelBlockingGitOnDrop {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

struct MarkBlockingGitExited(Arc<AtomicBool>);

impl Drop for MarkBlockingGitExited {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[derive(Clone)]
struct BlockingGitWorkerState {
    cancelled: Arc<AtomicBool>,
    exited: Arc<AtomicBool>,
}

impl BlockingGitWorkerState {
    fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            exited: Arc::new(AtomicBool::new(false)),
        }
    }
}

async fn blocking_git_span_controlled<T, F>(
    label: &str,
    request_cancellation: Option<tracedecay_contracts::CancellationSignal>,
    request_deadline: Option<tracedecay_contracts::Deadline>,
    work: F,
) -> Result<T>
where
    F: FnOnce(&dyn Fn() -> bool) -> T + Send + 'static,
    T: Send + 'static,
{
    blocking_git_span_controlled_with_state(
        label,
        request_cancellation,
        request_deadline,
        BlockingGitWorkerState::new(),
        work,
    )
    .await
}

async fn blocking_git_span_controlled_with_state<T, F>(
    label: &str,
    request_cancellation: Option<tracedecay_contracts::CancellationSignal>,
    request_deadline: Option<tracedecay_contracts::Deadline>,
    state: BlockingGitWorkerState,
    work: F,
) -> Result<T>
where
    F: FnOnce(&dyn Fn() -> bool) -> T + Send + 'static,
    T: Send + 'static,
{
    let cancel_on_drop = CancelBlockingGitOnDrop {
        cancelled: Arc::clone(&state.cancelled),
    };
    let worker_cancelled = Arc::clone(&state.cancelled);
    let worker_exited = Arc::clone(&state.exited);
    let worker_request_cancellation = request_cancellation.clone();
    let worker_request_deadline = request_deadline.clone();
    let mut worker = tokio::task::spawn_blocking(move || {
        let _mark_exited = MarkBlockingGitExited(worker_exited);
        let checkpoint = || {
            worker_cancelled.load(Ordering::Acquire)
                || worker_request_cancellation
                    .as_ref()
                    .is_some_and(tracedecay_contracts::CancellationSignal::is_cancelled)
                || worker_request_deadline.as_ref().is_some_and(|deadline| {
                    tracedecay_daemon_protocol::deadline_remaining(deadline).is_none()
                })
        };
        work(&checkpoint)
    });
    let joined = loop {
        tokio::select! {
            joined = &mut worker => break joined,
            () = tokio::time::sleep(std::time::Duration::from_millis(2)) => {
                let request_stopped = request_cancellation.as_ref().is_some_and(
                    tracedecay_contracts::CancellationSignal::is_cancelled,
                ) || request_deadline.as_ref().is_some_and(|deadline| {
                    tracedecay_daemon_protocol::deadline_remaining(deadline).is_none()
                });
                if request_stopped {
                    state.cancelled.store(true, Ordering::Release);
                    break worker.await;
                }
            }
        }
    }
    .map_err(|join_error| TraceDecayError::Config {
        message: format!("git {label} task failed: {join_error}"),
    })?;
    drop(cancel_on_drop);
    debug_assert!(state.exited.load(Ordering::Acquire));
    Ok(joined)
}

#[hotpath::measure(future = true, label = "mcp.git.diff_context.total")]
pub async fn compute_diff_context<F>(
    ctx: &McpToolContext<'_>,
    graph: F,
    args: Value,
) -> Result<GraphToolCompletionV1>
where
    F: Future<Output = Result<VerifiedGraphQuery>>,
{
    let request: DiffContextSurfaceRequestV1 =
        decode_primitive_request(&args, "tracedecay_diff_context")?;
    let depth = clamped_depth(request.depth, 2, 10);
    let graph = graph.await?;
    ctx.verify_graph_scope(&graph)?;

    let mut modified_symbols: Vec<GitContextSymbolV1> = Vec::new();
    let mut modified_seen: HashSet<String> = HashSet::new();
    let mut impacted_symbols: Vec<GitContextSymbolV1> = Vec::new();
    let mut impacted_seen: HashSet<String> = HashSet::new();
    let mut affected_tests: HashSet<String> = HashSet::new();
    let mut all_touched_files: Vec<String> = Vec::new();
    // Callers can (and in the wild do) pass the same path twice, e.g. when
    // synthesising the list from a directory walk that double-counts symlinked
    // or canonicalised entries. Dedup early so downstream loops don't emit
    // the same node N times for the same path.
    let files = unique_file_paths(request.files.iter().map(String::as_str));

    let requested_paths = files.iter().cloned().collect::<HashSet<_>>();
    let requested_symbols = hotpath::measure_block!(
        "mcp.git.diff_context.symbols",
        all_symbols_in_files(&graph, &requested_paths)?
    );

    // First pass: gather all modified symbols.
    let mut modified_ids: Vec<SymbolOccurrenceId> = Vec::new();
    for symbol in &requested_symbols {
        let path = symbol_path(symbol)?;
        all_touched_files.push(path.to_owned());
        // The occurrence identity is the generation-pinned deduplication key.
        if !modified_seen.insert(symbol.occurrence.as_str().to_owned()) {
            continue;
        }
        modified_symbols.push(context_symbol(symbol)?);
        modified_ids.push(symbol.occurrence.clone());
    }

    // Single multi-source BFS over the union of impact radii. Sharing a
    // `visited` set means each downstream node is walked at most once, even
    // when many modified symbols reach it through diamond dependencies, the
    // old per-symbol loop re-traversed the same subtree N times.
    let impacted = if modified_ids.is_empty() {
        tracedecay_code_index::graph_projection::CodeGraphImpactBatchV1 {
            impacted: Vec::new(),
            complete: true,
        }
    } else {
        hotpath::measure_block!(
            "mcp.git.diff_context.impact",
            graph.impact(
                &modified_ids,
                &[RelationEdgeKindV1::Calls, RelationEdgeKindV1::Uses],
                u32::try_from(depth).map_err(|error| TraceDecayError::Config {
                    message: format!("invalid diff context impact depth: {error}"),
                })?,
                PR_CONTEXT_MAX_IMPACT_NODES,
                PR_CONTEXT_MAX_IMPACT_EDGES,
            )?
        )
    };
    let impacted_paths = impacted
        .impacted
        .iter()
        .map(|impacted| symbol_path(&impacted.summary).map(str::to_owned))
        .collect::<Result<HashSet<_>>>()?;
    let annotation_paths = requested_paths
        .union(&impacted_paths)
        .cloned()
        .collect::<HashSet<_>>();
    let files_with_inline_tests = hotpath::measure_block!(
        "mcp.git.diff_context.test_annotations",
        graph.test_annotated_logical_files(
            Some(&annotation_paths),
            VERIFIED_GRAPH_MAX_SYMBOLS,
            VERIFIED_GRAPH_MAX_RELATIONS,
        )?
    );
    let has_tests = |path: &str| {
        tracedecay_code_index::is_test_file(path) || files_with_inline_tests.contains(path)
    };
    for impacted_symbol in &impacted.impacted {
        let impacted_node = &impacted_symbol.summary;
        // Drop seeds: callers want impacted symbols distinct from the
        // modified ones, mirroring the old per-node `if impacted.id == node.id`.
        if modified_seen.contains(impacted_node.occurrence.as_str()) {
            continue;
        }
        if !impacted_seen.insert(impacted_node.occurrence.as_str().to_owned()) {
            continue;
        }
        impacted_symbols.push(context_symbol(impacted_node)?);
        let path = symbol_path(impacted_node)?;
        if has_tests(path) {
            affected_tests.insert(path.to_owned());
        }
    }

    let traversal = hotpath::future!(
        collect_verified_affected_test_files(&graph, &files, depth, None),
        label = "mcp.git.diff_context.affected"
    )
    .await?;
    affected_tests.extend(traversal.test_distances.into_keys());

    let mut tests_sorted: Vec<String> = affected_tests.into_iter().collect();
    tests_sorted.sort();

    let touched_files = unique_file_paths(
        all_touched_files
            .iter()
            .map(String::as_str)
            .chain(files.iter().map(String::as_str)),
    );

    let result = DiffContextResultV1 {
        changed_files: files,
        modified_symbols,
        impacted_symbols_count: impacted_symbols.len(),
        impacted_symbols,
        impact_complete: impacted.complete,
        affected_tests: tests_sorted,
    };
    Ok(graph_tool_completion(
        GraphToolResultV1::DiffContext(result),
        touched_files,
    ))
}

/// Changelog is git-first: the tree diff is the answer, and symbol enrichment
/// comes from `exact_semantic_symbol_diff`, which reports its own typed
/// coverage when the code index is unavailable. The computation therefore
/// takes no verified graph query at all, a repository that git itself refuses
/// must report its typed git error rather than whatever state the graph
/// projection mount is in.
#[hotpath::measure(future = true, label = "mcp.git.changelog.total")]
pub async fn compute_changelog(
    ctx: &McpToolContext<'_>,
    args: Value,
) -> Result<GraphToolCompletionV1> {
    let ChangelogSurfaceRequestV1 { from_ref, to_ref } =
        decode_primitive_request(&args, "tracedecay_changelog")?;

    // Use gix to diff the two trees, off the request runtime's workers.
    let changes = {
        let project_root = ctx.project_root().to_path_buf();
        let from_ref = from_ref.clone();
        let to_ref = to_ref.clone();
        match hotpath::future!(
            blocking_git_span("tree diff", move || {
                git_diff_file_changes(&project_root, &from_ref, &to_ref)
            }),
            label = "mcp.git.changelog.diff"
        )
        .await?
        {
            Ok(files) => files,
            Err(message) => {
                return Ok(graph_tool_completion(
                    GraphToolResultV1::Changelog(ChangelogResultV1::GitFailure(git_failure(
                        GitToolOperationV1::Diff,
                        message,
                    ))),
                    Vec::new(),
                ));
            }
        }
    };
    let changed_files: Vec<String> = changes.iter().map(|change| change.path.clone()).collect();
    let touched_files: Vec<String> = changed_files.clone();

    let symbol_diff = hotpath::future!(
        exact_semantic_symbol_diff(ctx, &from_ref, &to_ref, None, None),
        label = "mcp.git.changelog.symbol_diff"
    )
    .await;
    let result = match symbol_diff {
        Ok(diff) => ChangelogResultV1::Complete(ChangelogCompleteV1 {
            status: GitReadCompleteV1::Complete,
            from_ref,
            to_ref,
            changed_file_count: changed_files.len(),
            changed_files,
            base_generation: diff.base_generation,
            head_generation: diff.head_generation,
            symbols_added: diff.added.iter().map(compared_symbol).collect(),
            symbols_removed: diff.removed.iter().map(compared_symbol).collect(),
            symbols_modified: diff.modified.iter().map(compared_symbol).collect(),
            symbol_changes_coverage: SymbolChangesCompleteV1 {
                status: GitReadCompleteV1::Complete,
            },
        }),
        Err(unavailable) => ChangelogResultV1::Partial(ChangelogPartialV1 {
            status: GitReadPartialV1::Partial,
            from_ref,
            to_ref,
            changed_file_count: changed_files.len(),
            changed_files,
            symbols_added: Vec::new(),
            symbols_removed: Vec::new(),
            symbols_modified: Vec::new(),
            symbol_changes_coverage: unavailable.coverage(),
        }),
    };
    Ok(graph_tool_completion(
        GraphToolResultV1::Changelog(result),
        touched_files,
    ))
}

#[hotpath::measure(future = true, label = "mcp.git.commit_context.total")]
pub async fn compute_commit_context<F>(
    ctx: &McpToolContext<'_>,
    graph: F,
    args: Value,
) -> Result<GraphToolCompletionV1>
where
    F: Future<Output = Result<VerifiedGraphQuery>>,
{
    let request: CommitContextSurfaceRequestV1 =
        decode_primitive_request(&args, "tracedecay_commit_context")?;
    let staged_only = request.staged_only.unwrap_or(false);
    let graph = graph.await?;
    ctx.verify_graph_scope(&graph)?;
    let git_failure_completion = |operation, message| {
        graph_tool_completion(
            GraphToolResultV1::CommitContext(CommitContextResultV1::GitFailure(git_failure(
                operation, message,
            ))),
            Vec::new(),
        )
    };

    // gix status classification walks the whole worktree; keep it off the
    // request runtime's workers so the carried dispatch deadline can preempt it.
    let changed_files = {
        let project_root = ctx.project_root().to_path_buf();
        match hotpath::future!(
            blocking_git_span("status", move || {
                git_changed_files(&project_root, staged_only)
            }),
            label = "mcp.git.commit_context.status"
        )
        .await?
        {
            Ok(files) => files,
            Err(message) => {
                return Ok(git_failure_completion(GitToolOperationV1::Status, message));
            }
        }
    };

    let recent_commits = {
        let project_root = ctx.project_root().to_path_buf();
        hotpath::future!(
            blocking_git_span("rev-walk", move || git_recent_commits(&project_root, 5)),
            label = "mcp.git.commit_context.recent_commits"
        )
    };

    if changed_files.is_empty() {
        let recent_commits = match recent_commits.await? {
            Ok(commits) => commits,
            Err(message) => {
                return Ok(git_failure_completion(GitToolOperationV1::Log, message));
            }
        };
        let summary = CommitContextSummaryV1 {
            changed_files: Vec::new(),
            symbols_by_role: BTreeMap::new(),
            suggested_category: None,
            recent_commits,
            summary: "No changes detected.".to_owned(),
        };
        return Ok(graph_tool_completion(
            GraphToolResultV1::CommitContext(CommitContextResultV1::Summary(summary)),
            Vec::new(),
        ));
    }

    let changed_paths = changed_files.iter().cloned().collect::<HashSet<_>>();
    let files_with_inline_tests = hotpath::measure_block!(
        "mcp.git.commit_context.test_annotations",
        graph.test_annotated_logical_files(
            Some(&changed_paths),
            VERIFIED_GRAPH_MAX_SYMBOLS,
            VERIFIED_GRAPH_MAX_RELATIONS,
        )?
    );
    let graph_symbols = hotpath::measure_block!(
        "mcp.git.commit_context.symbols",
        all_symbols_in_files(&graph, &changed_paths)?
    );
    let mut symbols_by_file: HashMap<String, Vec<&CodeGraphSymbolSummaryV1>> = HashMap::new();
    for symbol in &graph_symbols {
        symbols_by_file
            .entry(symbol_path(symbol)?.to_owned())
            .or_default()
            .push(symbol);
    }

    let mut file_roles: Vec<CommitFileRoleV1> = Vec::new();
    let mut symbols_by_role: BTreeMap<GitFileRoleV1, Vec<CommitSymbolEntryV1>> = BTreeMap::new();

    for file in &changed_files {
        let role = classify_file_role(file, &files_with_inline_tests);
        let symbols = symbols_by_file.get(file).map_or(&[][..], Vec::as_slice);
        file_roles.push(CommitFileRoleV1 {
            file: file.clone(),
            role,
            symbols: symbols.len(),
        });

        // Config files (Cargo.toml, *.yaml, package.json, ...) explode into
        // one node per key. Surface a single summary entry per file instead.
        // Agents only need to know "Cargo.toml changed, N keys touched",
        // not the name of every dependency listed.
        if role == GitFileRoleV1::Config {
            symbols_by_role
                .entry(role)
                .or_default()
                .push(CommitSymbolEntryV1::ConfigSummary(ConfigSummaryV1 {
                    file: file.clone(),
                    kind: ConfigSummaryKindV1::ConfigSummary,
                    config_keys: symbols.len(),
                }));
            continue;
        }
        for symbol in symbols {
            let metadata = symbol_metadata(symbol)?;
            symbols_by_role
                .entry(role)
                .or_default()
                .push(CommitSymbolEntryV1::Symbol(CommitSymbolV1 {
                    name: metadata.simple_name.as_str().to_owned(),
                    kind: metadata.kind.as_str().to_owned(),
                    file: symbol_path(symbol)?.to_owned(),
                    line: metadata.start_line,
                }));
        }
    }

    let has_tests = file_roles.iter().any(|f| f.role == GitFileRoleV1::Test);
    let has_source = file_roles.iter().any(|f| f.role == GitFileRoleV1::Source);
    let category = match (has_source, has_tests) {
        (true, true) => CommitCategoryV1::SourceAndTests,
        (true, false) => CommitCategoryV1::Source,
        (false, true) => CommitCategoryV1::Test,
        (false, false) => CommitCategoryV1::Chore,
    };

    let recent_commits = match recent_commits.await? {
        Ok(commits) => commits,
        Err(message) => {
            return Ok(git_failure_completion(GitToolOperationV1::Log, message));
        }
    };

    let total_symbols: usize = symbols_by_role.values().map(Vec::len).sum();
    let summary = CommitContextSummaryV1 {
        changed_files: file_roles,
        symbols_by_role,
        suggested_category: Some(category),
        recent_commits,
        summary: format!(
            "{} file(s) changed, {} symbol(s) affected",
            changed_files.len(),
            total_symbols
        ),
    };
    Ok(graph_tool_completion(
        GraphToolResultV1::CommitContext(CommitContextResultV1::Summary(summary)),
        changed_files,
    ))
}

const PR_CONTEXT_DEFAULT_SYMBOLS: usize = 200;
const PR_CONTEXT_MAX_SYMBOLS: usize = 500;
const PR_CONTEXT_MAX_IMPACT_NODES: usize = 1_000;
const PR_CONTEXT_MAX_IMPACT_EDGES: usize = 2_000;
const PR_CONTEXT_MAX_IMPACT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone)]
struct PrContextControls {
    deadline: Option<tracedecay_contracts::Deadline>,
    cancellation: Option<tracedecay_contracts::CancellationSignal>,
}

impl PrContextControls {
    fn checkpoint(&self) -> Result<()> {
        if self
            .cancellation
            .as_ref()
            .is_some_and(tracedecay_contracts::CancellationSignal::is_cancelled)
        {
            return Err(TraceDecayError::project_route(
                "pr_context_cancelled",
                true,
                "PR context was cancelled",
            ));
        }
        if self.deadline.as_ref().is_some_and(|deadline| {
            tracedecay_daemon_protocol::deadline_remaining(deadline).is_none()
        }) {
            return Err(TraceDecayError::project_route(
                "tool_dispatch_deadline_exceeded",
                true,
                "PR context exceeded its dispatch deadline",
            ));
        }
        Ok(())
    }
}

fn elapsed_micros(started: std::time::Instant) -> u64 {
    tracedecay_runtime_core::tracedecay::saturating_duration_micros(started.elapsed())
}

fn pr_context_impact_snapshot(
    graph: &VerifiedGraphQuery,
    seed_nodes: &[CodeGraphSymbolSummaryV1],
    max_depth: usize,
    prior_budget: PrContextImpactBudget,
    controls: &PrContextControls,
) -> Result<PrContextImpact> {
    let mut impact = PrContextImpact {
        nodes_admitted: prior_budget.nodes_admitted,
        direct_call_edges_admitted: prior_budget.direct_call_edges_admitted,
        bytes_admitted: prior_budget.bytes_admitted,
        ..PrContextImpact::default()
    };
    let mut visited = HashSet::new();
    let mut frontier = Vec::new();
    for node in seed_nodes {
        let bytes = pr_context_node_bytes(node);
        if impact.nodes_admitted >= PR_CONTEXT_MAX_IMPACT_NODES
            || impact.bytes_admitted.saturating_add(bytes) > PR_CONTEXT_MAX_IMPACT_BYTES
        {
            impact.partial = true;
            continue;
        }
        impact.bytes_admitted = impact.bytes_admitted.saturating_add(bytes);
        impact.nodes_admitted = impact.nodes_admitted.saturating_add(1);
        visited.insert(node.occurrence.clone());
        frontier.push(node.occurrence.clone());
        impact.nodes.push(node.clone());
    }
    if frontier.is_empty() {
        return Ok(impact);
    }
    let remaining_edges =
        PR_CONTEXT_MAX_IMPACT_EDGES.saturating_sub(impact.direct_call_edges_admitted);
    let remaining_nodes = PR_CONTEXT_MAX_IMPACT_NODES.saturating_sub(impact.nodes_admitted);
    if remaining_edges == 0 || remaining_nodes == 0 {
        impact.partial = true;
        return Ok(impact);
    }
    controls.checkpoint()?;
    let incoming_calls = graph.callers(&frontier, &[RelationEdgeKindV1::Calls], remaining_edges)?;
    for edge in incoming_calls.into_iter().flatten() {
        controls.checkpoint()?;
        let bytes = pr_context_edge_bytes(&edge);
        if impact.bytes_admitted.saturating_add(bytes) > PR_CONTEXT_MAX_IMPACT_BYTES {
            impact.partial = true;
            break;
        }
        impact.bytes_admitted = impact.bytes_admitted.saturating_add(bytes);
        impact.direct_call_edges_admitted = impact.direct_call_edges_admitted.saturating_add(1);
        impact.incoming_calls.push(edge);
    }
    let depth = u32::try_from(max_depth).map_err(|error| TraceDecayError::Config {
        message: format!("invalid PR context impact depth: {error}"),
    })?;
    let graph_impact = graph.impact(
        &frontier,
        &[RelationEdgeKindV1::Calls, RelationEdgeKindV1::Uses],
        depth,
        remaining_nodes,
        remaining_edges,
    )?;
    impact.partial |= !graph_impact.complete;
    for impacted in graph_impact.impacted {
        controls.checkpoint()?;
        if !visited.insert(impacted.summary.occurrence.clone()) {
            continue;
        }
        let bytes = pr_context_node_bytes(&impacted.summary);
        if impact.bytes_admitted.saturating_add(bytes) > PR_CONTEXT_MAX_IMPACT_BYTES {
            impact.partial = true;
            continue;
        }
        impact.bytes_admitted = impact.bytes_admitted.saturating_add(bytes);
        impact.nodes_admitted = impact.nodes_admitted.saturating_add(1);
        impact.nodes.push(impacted.summary);
    }
    Ok(impact)
}

#[derive(Clone, Copy, Default)]
struct PrContextImpactBudget {
    nodes_admitted: usize,
    direct_call_edges_admitted: usize,
    bytes_admitted: usize,
}

#[derive(Default)]
struct PrContextImpact {
    nodes: Vec<CodeGraphSymbolSummaryV1>,
    incoming_calls: Vec<tracedecay_code_index::graph_projection::CodeGraphSemanticEdgeV1>,
    nodes_admitted: usize,
    direct_call_edges_admitted: usize,
    bytes_admitted: usize,
    partial: bool,
}

fn pr_context_node_bytes(node: &CodeGraphSymbolSummaryV1) -> usize {
    node.occurrence
        .as_str()
        .len()
        .saturating_add(node.metadata.as_ref().map_or(0, |metadata| {
            metadata
                .simple_name
                .len()
                .saturating_add(metadata.qualified_name.len())
                .saturating_add(metadata.signature.as_ref().map_or(0, String::len))
        }))
        .saturating_add(
            node.binding
                .as_ref()
                .and_then(|binding| binding.logical_path.as_ref())
                .map_or(0, String::len),
        )
}

fn pr_context_edge_bytes(
    edge: &tracedecay_code_index::graph_projection::CodeGraphSemanticEdgeV1,
) -> usize {
    edge.edge
        .from_occurrence
        .as_str()
        .len()
        .saturating_add(edge.edge.to_occurrence.as_str().len())
        .saturating_add("calls".len())
}

fn graph_enrichment_is_transient(error: &TraceDecayError) -> bool {
    matches!(
        error.project_route_context(),
        Some(("code-graph-unavailable" | "code-graph-stale", true, _))
    )
}

const PR_CONTEXT_GRAPH_PENDING_MESSAGE: &str = "Verified graph results pending while the generation warms; Git comparison results are available.";
const PR_CONTEXT_HEAD_GENERATION_MISMATCH_MESSAGE: &str =
    "Git comparison is available, but the verified graph is not the compared head generation.";
const PR_CONTEXT_SYMBOLS_UNAVAILABLE_MESSAGE: &str =
    "Git comparison is available, but exact base/head symbol comparison is unavailable.";

/// The git comparison every PR-context outcome reports.
struct PrContextGitEvidence {
    base: String,
    head: String,
    base_oid: String,
    head_oid: String,
    merge_base: String,
    commits: Vec<GitCommitSubjectV1>,
    changes: Vec<GitFileChangeV1>,
}

impl PrContextGitEvidence {
    fn symbols_unavailable(
        self,
        message: &str,
        graph_generation: String,
        coverage: SymbolChangesUnavailableV1,
    ) -> PrContextResultV1 {
        PrContextResultV1::SymbolsUnavailable(Box::new(PrContextSymbolsUnavailableV1 {
            status: GitReadPartialV1::Partial,
            message: message.to_owned(),
            base: self.base,
            head: self.head,
            base_oid: self.base_oid,
            head_oid: self.head_oid,
            merge_base: self.merge_base,
            graph_generation,
            commits: self.commits,
            files_changed: self.changes.len(),
            changes: self.changes,
            symbols_added: 0,
            symbols_removed: 0,
            symbols_modified: 0,
            added: Vec::new(),
            removed: Vec::new(),
            modified: Vec::new(),
            symbol_changes_coverage: coverage,
            next_cursor: None,
        }))
    }
}

#[hotpath::measure(future = true, label = "mcp.pr_context.total")]
pub async fn compute_pr_context<F>(
    ctx: &McpToolContext<'_>,
    graph: F,
    args: Value,
) -> Result<GraphToolCompletionV1>
where
    F: Future<Output = Result<VerifiedGraphQuery>>,
{
    let request: PrContextSurfaceRequestV1 =
        decode_primitive_request(&args, "tracedecay_pr_context")?;
    let controls = PrContextControls {
        deadline: ctx.deadline().cloned(),
        cancellation: ctx.cancellation().cloned(),
    };
    controls.checkpoint()?;
    let total_started = std::time::Instant::now();
    let mut timings = PrContextStageTimingsV1::default();
    let base = request
        .base_ref
        .unwrap_or_else(|| default_pr_base_ref(ctx.project_root()));
    let head = request.head_ref.unwrap_or_else(|| "HEAD".to_owned());

    let stage_started = std::time::Instant::now();
    let comparison = {
        let project_root = ctx.project_root().to_path_buf();
        let base_ref = base.clone();
        let head_ref = head.clone();
        match hotpath::future!(
            blocking_git_span_controlled(
                "PR comparison",
                controls.cancellation.clone(),
                controls.deadline.clone(),
                move |cancelled| {
                    git_pr_comparison_controlled(&project_root, &base_ref, &head_ref, cancelled)
                },
            ),
            label = "mcp.pr_context.git"
        )
        .await?
        {
            Ok(comparison) => comparison,
            Err(message) => {
                controls.checkpoint()?;
                return Ok(graph_tool_completion(
                    GraphToolResultV1::PrContext(PrContextResultV1::GitFailure(git_failure(
                        GitToolOperationV1::Diff,
                        message,
                    ))),
                    Vec::new(),
                ));
            }
        }
    };
    controls.checkpoint()?;
    timings.git = elapsed_micros(stage_started);
    let GitPrComparison {
        base_oid,
        head_oid,
        merge_base,
        mut changes,
        commits,
    } = comparison;
    changes.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.status.cmp(&right.status))
    });
    let changed_files: Vec<String> = changes.iter().map(|change| change.path.clone()).collect();
    let changed_paths = changed_files.iter().cloned().collect::<HashSet<_>>();

    let maximum_symbols = request
        .maximum_symbols
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(PR_CONTEXT_DEFAULT_SYMBOLS)
        .clamp(1, PR_CONTEXT_MAX_SYMBOLS);
    let encoded_cursor = request.cursor.as_deref();
    let evidence = PrContextGitEvidence {
        base,
        head,
        base_oid,
        head_oid,
        merge_base,
        commits,
        changes,
    };

    let stage_started = std::time::Instant::now();
    let graph = match hotpath::future!(graph, label = "mcp.pr_context.graph_admission").await {
        Ok(graph) => {
            ctx.verify_graph_scope(&graph)?;
            graph
        }
        Err(error) if encoded_cursor.is_some() || !graph_enrichment_is_transient(&error) => {
            return Err(error);
        }
        Err(error) => {
            timings.graph = elapsed_micros(stage_started);
            let test_files_changed = evidence
                .changes
                .iter()
                .filter(|change| tracedecay_code_index::is_test_file(&change.path))
                .map(|change| change.path.clone())
                .collect::<Vec<_>>();
            let symbol_page = PrSymbolPageV1 {
                limit: maximum_symbols,
                returned: 0,
                has_more: false,
                complete: false,
                selection: PrSymbolSelectionV1::Unavailable,
                continuation_available: false,
            };
            let unavailable_coverage = PrSelectionCoverageV1 {
                complete: false,
                selection: PrCoverageSelectionV1::Unavailable,
            };
            let result = PrContextGraphPendingV1 {
                status: GitReadPartialV1::Partial,
                message: PR_CONTEXT_GRAPH_PENDING_MESSAGE.to_owned(),
                base: evidence.base,
                head: evidence.head,
                base_oid: evidence.base_oid,
                head_oid: evidence.head_oid,
                merge_base: evidence.merge_base,
                graph_generation: None,
                commits: evidence.commits,
                files_changed: evidence.changes.len(),
                changes: evidence.changes,
                symbols_added: 0,
                symbols_modified: 0,
                added: Vec::new(),
                modified: Vec::new(),
                next_cursor: None,
                symbol_page: symbol_page.clone(),
                analysis_coverage: PrAnalysisCoverageV1 {
                    seed_symbols_analyzed: 0,
                    symbols_returned: 0,
                    symbols_complete: false,
                    impact_nodes_admitted: 0,
                    impact_nodes_returned: 0,
                    direct_call_edges_admitted: 0,
                    impact_bytes_admitted: 0,
                    impact_partial: true,
                    complete: false,
                },
                test_files_changed,
                affected_tests: Vec::new(),
                affected_tests_coverage: unavailable_coverage.clone(),
                impacted_modules: Vec::new(),
                impacted_modules_coverage: unavailable_coverage,
                verified_graph_evidence: dependency_hints::unavailable_evidence(&error),
            };
            timings.total = elapsed_micros(total_started);
            tracing::info!(
                tool = "tracedecay_pr_context",
                files = changed_files.len(),
                symbols = 0,
                timings = ?timings,
                "PR context returned Git evidence while graph enrichment was unavailable"
            );
            return Ok(pr_context_completion(
                PrContextResultV1::GraphPending(Box::new(result)),
                changed_files,
                timings,
                symbol_page,
            ));
        }
    };
    timings.graph = elapsed_micros(stage_started);

    let stage_started = std::time::Instant::now();
    let symbol_diff = match hotpath::future!(
        exact_semantic_symbol_diff(
            ctx,
            &evidence.base,
            &evidence.head,
            Some(evidence.merge_base.as_str()),
            Some(evidence.head_oid.as_str()),
        ),
        label = "mcp.pr_context.symbol_diff"
    )
    .await
    {
        Ok(diff) if diff.head_generation == graph.generation().as_str() => diff,
        Ok(_) => {
            let result = evidence.symbols_unavailable(
                PR_CONTEXT_HEAD_GENERATION_MISMATCH_MESSAGE,
                graph.generation().as_str().to_owned(),
                SymbolChangesUnavailableV1 {
                    status: GitReadUnavailableV1::Unavailable,
                    reason: "head_generation_mismatch".to_owned(),
                    retryable: true,
                },
            );
            return Ok(graph_tool_completion(
                GraphToolResultV1::PrContext(result),
                changed_files,
            ));
        }
        Err(unavailable) => {
            let result = evidence.symbols_unavailable(
                PR_CONTEXT_SYMBOLS_UNAVAILABLE_MESSAGE,
                graph.generation().as_str().to_owned(),
                unavailable.coverage(),
            );
            return Ok(graph_tool_completion(
                GraphToolResultV1::PrContext(result),
                changed_files,
            ));
        }
    };
    controls.checkpoint()?;
    timings.symbol_diff = Some(elapsed_micros(stage_started));

    let graph_generation = graph.generation().as_str().to_owned();
    // Byte-exact worktree identity: a lossy string would let two distinct
    // non-UTF-8 roots mint interchangeable cursors.
    let project_root =
        tracedecay_runtime_core::os_str_bytes::native_os_str_bytes(ctx.project_root().as_os_str());
    let cursor_binding = PrContextCursorBinding::new(
        ctx,
        &project_root,
        PrContextCursorComparison {
            base_oid: &evidence.base_oid,
            head_oid: &evidence.head_oid,
            merge_base: &evidence.merge_base,
            graph_generation: &graph_generation,
            maximum_symbols,
            changes: &evidence.changes,
        },
    );
    let cursor_authority = match ctx.authorized_project_session_db() {
        Some(_) => Some(
            hotpath::future!(
                pr_context_cursor_authority(ctx, &cursor_binding),
                label = "mcp.pr_context.cursor_authority"
            )
            .await?,
        ),
        None if encoded_cursor.is_some() => {
            return Err(TraceDecayError::Config {
                message: "PR context cursor authority is unavailable".to_owned(),
            });
        }
        None => None,
    };
    let cursor_position = match (encoded_cursor, cursor_authority.as_ref()) {
        (Some(cursor), Some((snapshot, authenticator))) => {
            Some(decode_pr_context_cursor(cursor, snapshot, authenticator)?)
        }
        _ => None,
    };
    let prior_impact_budget =
        cursor_position
            .as_ref()
            .map_or_else(PrContextImpactBudget::default, |position| {
                PrContextImpactBudget {
                    nodes_admitted: position.impact_nodes_admitted,
                    direct_call_edges_admitted: position.direct_call_edges_admitted,
                    bytes_admitted: position.impact_bytes_admitted,
                }
            });

    let mut test_files_changed: Vec<String> = Vec::new();
    let mut impacted_modules: HashSet<String> = HashSet::new();

    // Pre-compute files with inline test modules.
    let stage_started = std::time::Instant::now();
    let mut files_with_inline_tests = hotpath::measure_block!(
        "mcp.pr_context.test_annotations.changed",
        graph.test_annotated_logical_files(
            Some(&changed_paths),
            VERIFIED_GRAPH_MAX_SYMBOLS,
            VERIFIED_GRAPH_MAX_RELATIONS,
        )?
    );
    controls.checkpoint()?;
    timings.test_annotations = Some(elapsed_micros(stage_started));
    let added_ids = symbol_diff
        .added
        .iter()
        .map(|symbol| symbol.symbol_occurrence_id.as_str())
        .collect::<HashSet<_>>();
    let modified_ids = symbol_diff
        .modified
        .iter()
        .map(|symbol| symbol.symbol_occurrence_id.as_str())
        .collect::<HashSet<_>>();
    for change in &evidence.changes {
        if tracedecay_code_index::is_test_file(&change.path)
            || files_with_inline_tests.contains(&change.path)
        {
            test_files_changed.push(change.path.clone());
        }
    }
    test_files_changed.sort();
    test_files_changed.dedup();

    let stage_started = std::time::Instant::now();
    let symbol_page = hotpath::measure_block!(
        "mcp.pr_context.symbol_page",
        graph.symbols_in_logical_files_page(
            &changed_paths,
            cursor_position.as_ref().map(|position| &position.after),
            maximum_symbols,
            VERIFIED_GRAPH_MAX_SYMBOLS,
        )?
    );
    controls.checkpoint()?;
    timings.symbol_page = Some(elapsed_micros(stage_started));
    let symbol_has_more = symbol_page.has_more;
    let next_page_key = symbol_page
        .symbols
        .last()
        .map(|symbol| symbol.occurrence.clone());
    let mut added = Vec::new();
    let mut modified = Vec::new();
    let mut nodes = Vec::with_capacity(symbol_page.symbols.len());
    let mut config_key_counts = HashMap::<(bool, String), usize>::new();
    for symbol in symbol_page.symbols {
        controls.checkpoint()?;
        let path = symbol_path(&symbol)?;
        let is_added = added_ids.contains(symbol.occurrence.as_str());
        let is_modified = modified_ids.contains(symbol.occurrence.as_str());
        if !is_added && !is_modified {
            continue;
        }
        if classify_file_role(path, &files_with_inline_tests) == GitFileRoleV1::Config {
            *config_key_counts
                .entry((is_added, path.to_owned()))
                .or_default() += 1;
            continue;
        }
        let entry = PrSymbolEntryV1::Symbol(context_symbol(&symbol)?);
        if is_added {
            added.push(entry);
        } else {
            modified.push(entry);
        }
        nodes.push(symbol);
    }
    let mut config_summaries = config_key_counts.into_iter().collect::<Vec<_>>();
    // Added summaries sort before modified ones, then by path.
    config_summaries.sort_by(|left, right| (!left.0.0, &left.0.1).cmp(&(!right.0.0, &right.0.1)));
    for ((is_added, path), config_keys) in config_summaries {
        let summary = PrSymbolEntryV1::ConfigSummary(ConfigSummaryV1 {
            file: path,
            kind: ConfigSummaryKindV1::ConfigSummary,
            config_keys,
        });
        if is_added {
            added.push(summary);
        } else {
            modified.push(summary);
        }
    }
    let removed = symbol_diff
        .removed
        .iter()
        .map(compared_symbol)
        .collect::<Vec<_>>();
    let returned_symbols = added.len().saturating_add(modified.len());

    // Find transitively affected test files
    let stage_started = std::time::Instant::now();
    let mut affected_tests: HashSet<String> = HashSet::new();
    let impact = hotpath::measure_block!(
        "mcp.pr_context.impact",
        pr_context_impact_snapshot(&graph, &nodes, 2, prior_impact_budget, &controls)?
    );
    controls.checkpoint()?;
    let impact_paths: Vec<String> = impact
        .nodes
        .iter()
        .map(|node| symbol_path(node).map(str::to_owned))
        .collect::<Result<Vec<_>>>()?;
    let impact_path_set = impact_paths.iter().cloned().collect::<HashSet<_>>();
    files_with_inline_tests.extend(hotpath::measure_block!(
        "mcp.pr_context.test_annotations.impacted",
        graph.test_annotated_logical_files(
            Some(&impact_path_set),
            VERIFIED_GRAPH_MAX_SYMBOLS,
            VERIFIED_GRAPH_MAX_RELATIONS,
        )?
    ));
    let impacted_by_id: HashMap<&str, &CodeGraphSymbolSummaryV1> = impact
        .nodes
        .iter()
        .map(|node| (node.occurrence.as_str(), node))
        .collect();
    for edge in &impact.incoming_calls {
        if let Some(caller) = impacted_by_id.get(edge.edge.from_occurrence.as_str())
            && !changed_paths.contains(symbol_path(caller)?)
        {
            let caller_path = symbol_path(caller)?;
            let dir = caller_path
                .rfind('/')
                .map_or(caller_path, |index| &caller_path[..index]);
            impacted_modules.insert(dir.to_owned());
        }
    }
    for impacted in &impact.nodes {
        let path = symbol_path(impacted)?;
        if !changed_paths.contains(path)
            && (tracedecay_code_index::is_test_file(path) || files_with_inline_tests.contains(path))
        {
            affected_tests.insert(path.to_owned());
        }
    }
    timings.impact = Some(elapsed_micros(stage_started));

    let mut impacted_sorted: Vec<String> = impacted_modules.into_iter().collect();
    impacted_sorted.sort();
    let mut affected_sorted: Vec<String> = affected_tests.into_iter().collect();
    affected_sorted.sort();

    let stage_started = std::time::Instant::now();
    let symbol_complete = !symbol_has_more;
    let impact_complete = symbol_complete && !impact.partial;
    let next_cursor = if symbol_complete {
        None
    } else {
        let key = next_page_key
            .as_ref()
            .ok_or_else(|| TraceDecayError::Config {
                message: "PR context page has more symbols without a continuation key".to_owned(),
            })?;
        let (snapshot, authenticator) =
            cursor_authority
                .as_ref()
                .ok_or_else(|| TraceDecayError::Config {
                    message: "PR context cursor authority is unavailable".to_owned(),
                })?;
        Some(encode_pr_context_cursor(
            key,
            impact.nodes_admitted,
            impact.direct_call_edges_admitted,
            impact.bytes_admitted,
            snapshot,
            authenticator,
        )?)
    };
    let symbol_page = PrSymbolPageV1 {
        limit: maximum_symbols,
        returned: returned_symbols,
        has_more: symbol_has_more,
        complete: symbol_complete,
        selection: PrSymbolSelectionV1::StablePrefix,
        continuation_available: symbol_has_more,
    };
    let bounded_coverage = PrSelectionCoverageV1 {
        complete: impact_complete,
        selection: PrCoverageSelectionV1::DeterministicBoundedPrefix,
    };
    let result = PrContextCompleteV1 {
        status: GitReadCompleteV1::Complete,
        base: evidence.base,
        head: evidence.head,
        base_oid: evidence.base_oid,
        head_oid: evidence.head_oid,
        merge_base: evidence.merge_base,
        graph_generation,
        commits: evidence.commits,
        files_changed: evidence.changes.len(),
        changes: evidence.changes,
        symbols_added: added.len(),
        symbols_removed: removed.len(),
        symbols_modified: modified.len(),
        added,
        removed,
        modified,
        symbol_changes_coverage: PrSymbolChangesCompleteV1 {
            status: GitReadCompleteV1::Complete,
            base_generation: symbol_diff.base_generation,
            head_generation: symbol_diff.head_generation,
        },
        next_cursor,
        symbol_page: symbol_page.clone(),
        analysis_coverage: PrAnalysisCoverageV1 {
            seed_symbols_analyzed: nodes.len(),
            symbols_returned: returned_symbols,
            symbols_complete: symbol_complete,
            impact_nodes_admitted: impact.nodes_admitted,
            impact_nodes_returned: impact.nodes.len(),
            direct_call_edges_admitted: impact.direct_call_edges_admitted,
            impact_bytes_admitted: impact.bytes_admitted,
            impact_partial: impact.partial,
            complete: impact_complete,
        },
        test_files_changed,
        affected_tests: affected_sorted,
        affected_tests_coverage: bounded_coverage.clone(),
        impacted_modules: impacted_sorted,
        impacted_modules_coverage: bounded_coverage,
    };
    timings.assemble = Some(elapsed_micros(stage_started));
    timings.total = elapsed_micros(total_started);
    tracing::info!(
        tool = "tracedecay_pr_context",
        files = changed_files.len(),
        symbols = returned_symbols,
        timings = ?timings,
        "PR context stage timings"
    );

    Ok(pr_context_completion(
        PrContextResultV1::Complete(Box::new(result)),
        changed_files,
        timings,
        symbol_page,
    ))
}

fn pr_context_completion(
    result: PrContextResultV1,
    touched_files: Vec<String>,
    stage_timings_us: PrContextStageTimingsV1,
    symbol_coverage: PrSymbolPageV1,
) -> GraphToolCompletionV1 {
    let mut completion = graph_tool_completion(GraphToolResultV1::PrContext(result), touched_files);
    completion.analytics = Some(InvocationAnalyticsV1 {
        pr_context: Some(PrContextAnalyticsV1 {
            stage_timings_us,
            symbol_coverage,
        }),
        ..InvocationAnalyticsV1::default()
    });
    completion
}

#[cfg(test)]
mod blocking_git_span_tests {
    use super::{
        BlockingGitWorkerState, blocking_git_span, blocking_git_span_controlled_with_state,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[tokio::test]
    async fn a_blocking_span_does_not_starve_the_runtime_worker() {
        // Single-threaded runtime: another task can only make progress if the
        // synchronous work is genuinely off the worker. Running it inline would
        // leave `progressed` false when the span returns.
        let progressed = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&progressed);
        let ticker = tokio::spawn(async move {
            flag.store(true, Ordering::Release);
        });
        let value = blocking_git_span("test", || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            7_u8
        })
        .await
        .expect("the join must succeed");
        assert_eq!(value, 7);
        assert!(
            progressed.load(Ordering::Acquire),
            "a concurrent task must have run while the gix span was blocking"
        );
        ticker.await.expect("ticker joins");
    }

    #[tokio::test]
    async fn dropping_a_cancelled_git_span_stops_the_live_worker() {
        let state = BlockingGitWorkerState::new();
        let observed = state.clone();
        let span = blocking_git_span_controlled_with_state(
            "live cancellation test",
            None,
            None,
            state,
            move |cancelled| {
                while !cancelled() {
                    std::thread::yield_now();
                }
            },
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), span)
                .await
                .is_err(),
            "the deadline must drop the in-flight join"
        );
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !observed.exited.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancelled blocking worker exits promptly");
    }

    #[tokio::test]
    async fn request_cancellation_joins_the_live_git_worker() {
        let cancellation =
            tracedecay_contracts::CancellationSignal::active("cancel.git-worker-test")
                .expect("valid cancellation");
        let canceller = cancellation.clone();
        let state = BlockingGitWorkerState::new();
        let observed = state.clone();
        let trigger = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            canceller.cancel(tracedecay_domain::UtcMicros(1));
        });
        blocking_git_span_controlled_with_state(
            "request cancellation test",
            Some(cancellation),
            None,
            state,
            move |cancelled| {
                while !cancelled() {
                    std::thread::yield_now();
                }
            },
        )
        .await
        .expect("cancelled worker joins");
        trigger.await.expect("cancellation trigger joins");
        assert!(observed.exited.load(Ordering::Acquire));
    }
}

// ── Cross-branch tools ─────────────────────────────────────────────────
