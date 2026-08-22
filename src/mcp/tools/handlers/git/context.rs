//! `tracedecay_diff_context`, `tracedecay_changelog`, `tracedecay_commit_context`, and `tracedecay_pr_context`.

use super::super::dependency_hints;
use super::affected::collect_verified_affected_test_files;
use super::pr_context_cursor::{
    PrContextCursorBinding, decode_pr_context_cursor, encode_pr_context_cursor,
    pr_context_cursor_authority,
};
use super::shell::{
    classify_file_role, default_pr_base_ref, git_changed_files, git_diff_file_changes,
    git_pr_comparison_controlled, git_recent_commits,
};
use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1;
use tracedecay_domain::{RelationEdgeKindV1, SymbolOccurrenceId};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;

const VERIFIED_GRAPH_MAX_SYMBOLS: usize = 500_000;
const VERIFIED_GRAPH_MAX_RELATIONS: usize = 2_000_000;

type VerifiedGraphQuery = crate::tracedecay::queries::graph::VerifiedGraphQuery;

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

fn symbol_value(symbol: &CodeGraphSymbolSummaryV1, include_signature: bool) -> Result<Value> {
    let metadata = symbol_metadata(symbol)?;
    let path = symbol_path(symbol)?;
    let mut value = json!({
        "id": symbol.occurrence.as_str(),
        "name": metadata.simple_name.as_str(),
        "kind": metadata.kind.as_str(),
        "file": path,
        "line": metadata.start_line,
    });
    if include_signature {
        value["signature"] = json!(metadata.signature.as_deref());
    }
    Ok(value)
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
/// worker, and — the sharper problem — makes the carried git dispatch deadline
/// unenforceable: `tokio::time::timeout` can only preempt at an await point, so
/// an inline blocking call runs to completion regardless. Awaiting the
/// `spawn_blocking` join handle restores exactly that composition, which
/// `handle_pr_context` already relied on.
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
    request_cancellation: Option<tracedecay_application::CancellationSignal>,
    request_deadline: Option<tracedecay_application::Deadline>,
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
    request_cancellation: Option<tracedecay_application::CancellationSignal>,
    request_deadline: Option<tracedecay_application::Deadline>,
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
                    .is_some_and(tracedecay_application::CancellationSignal::is_cancelled)
                || worker_request_deadline.as_ref().is_some_and(|deadline| {
                    crate::daemon_client::deadline_remaining(deadline).is_none()
                })
        };
        work(&checkpoint)
    });
    let joined = loop {
        tokio::select! {
            joined = &mut worker => break joined,
            () = tokio::time::sleep(std::time::Duration::from_millis(2)) => {
                let request_stopped = request_cancellation.as_ref().is_some_and(
                    tracedecay_application::CancellationSignal::is_cancelled,
                ) || request_deadline.as_ref().is_some_and(|deadline| {
                    crate::daemon_client::deadline_remaining(deadline).is_none()
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

/// Handles `tracedecay_diff_context` tool calls.
pub(crate) async fn handle_diff_context(
    cg: &TraceDecay,
    graph: &VerifiedGraphQuery,
    args: Value,
) -> Result<ToolResult> {
    require_object_args(&args, "tracedecay_diff_context")?;
    let files = require_string_array_arg(&args, "files")?;
    let depth = clamped_depth_arg(&args, "depth", 2, 10);

    let mut modified_symbols: Vec<Value> = Vec::new();
    let mut modified_seen: HashSet<String> = HashSet::new();
    let mut impacted_symbols: Vec<Value> = Vec::new();
    let mut impacted_seen: HashSet<String> = HashSet::new();
    let mut affected_tests: HashSet<String> = HashSet::new();
    let mut all_touched_files: Vec<String> = Vec::new();
    // Callers can (and in the wild do) pass the same path twice — e.g. when
    // synthesising the list from a directory walk that double-counts symlinked
    // or canonicalised entries. Dedup early so downstream loops don't emit
    // the same node N times for the same path.
    let files = unique_file_paths(files.iter().map(std::string::String::as_str));

    let requested_paths = files.iter().cloned().collect::<HashSet<_>>();
    let requested_symbols = all_symbols_in_files(graph, &requested_paths)?;

    // First pass: gather all modified symbols.
    let mut modified_ids: Vec<SymbolOccurrenceId> = Vec::new();
    for symbol in &requested_symbols {
        let path = symbol_path(symbol)?;
        all_touched_files.push(path.to_owned());
        // The occurrence identity is the generation-pinned deduplication key.
        if !modified_seen.insert(symbol.occurrence.as_str().to_owned()) {
            continue;
        }
        modified_symbols.push(symbol_value(symbol, false)?);
        modified_ids.push(symbol.occurrence.clone());
    }

    // Single multi-source BFS over the union of impact radii. Sharing a
    // `visited` set means each downstream node is walked at most once, even
    // when many modified symbols reach it through diamond dependencies — the
    // old per-symbol loop re-traversed the same subtree N times.
    let impacted = if modified_ids.is_empty() {
        tracedecay_code_index::graph_projection::CodeGraphImpactBatchV1 {
            impacted: Vec::new(),
            complete: true,
        }
    } else {
        graph.impact(
            &modified_ids,
            &[RelationEdgeKindV1::Calls, RelationEdgeKindV1::Uses],
            u32::try_from(depth).map_err(|error| TraceDecayError::Config {
                message: format!("invalid diff context impact depth: {error}"),
            })?,
            PR_CONTEXT_MAX_IMPACT_NODES,
            PR_CONTEXT_MAX_IMPACT_EDGES,
        )?
    };
    let files_with_inline_tests = graph.test_annotated_logical_files(
        None,
        VERIFIED_GRAPH_MAX_SYMBOLS,
        VERIFIED_GRAPH_MAX_RELATIONS,
    )?;
    let has_tests = |path: &str| {
        crate::tracedecay::is_test_file(path) || files_with_inline_tests.contains(path)
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
        impacted_symbols.push(symbol_value(impacted_node, false)?);
        let path = symbol_path(impacted_node)?;
        if has_tests(path) {
            affected_tests.insert(path.to_owned());
        }
    }

    let traversal =
        collect_verified_affected_test_files(graph, &files, depth, None, &files_with_inline_tests)
            .await?;
    affected_tests.extend(traversal.test_distances.into_keys());

    let mut tests_sorted: Vec<String> = affected_tests.into_iter().collect();
    tests_sorted.sort();

    let touched_files = unique_file_paths(
        all_touched_files
            .iter()
            .map(std::string::String::as_str)
            .chain(files.iter().map(std::string::String::as_str)),
    );

    let output = json!({
        "changed_files": files,
        "modified_symbols": modified_symbols,
        "impacted_symbols_count": impacted_symbols.len(),
        "impacted_symbols": impacted_symbols,
        "impact_complete": impacted.complete,
        "affected_tests": tests_sorted,
    });

    Ok(generic_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        touched_files,
    ))
}
/// Handles `tracedecay_changelog` tool calls.
pub(crate) async fn handle_changelog(
    cg: &TraceDecay,
    graph: &VerifiedGraphQuery,
    args: Value,
) -> Result<ToolResult> {
    require_object_args(&args, "tracedecay_changelog")?;
    let from_ref = args
        .get("from_ref")
        .and_then(|v| v.as_str())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: from_ref".to_string(),
        })?;

    let to_ref =
        args.get("to_ref")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TraceDecayError::Config {
                message: "missing required parameter: to_ref".to_string(),
            })?;

    // Use gix to diff the two trees, off the request runtime's workers.
    let changes = {
        let project_root = cg.project_root().to_path_buf();
        let from_ref = from_ref.to_owned();
        let to_ref = to_ref.to_owned();
        match blocking_git_span("tree diff", move || {
            git_diff_file_changes(&project_root, &from_ref, &to_ref)
        })
        .await?
        {
            Ok(files) => files,
            Err(e) => {
                return Ok(git_error_result(cg, &args, "diff", &e));
            }
        }
    };
    let changed_files: Vec<String> = changes.iter().map(|change| change.path.clone()).collect();
    let changed_paths = changed_files.iter().cloned().collect::<HashSet<_>>();
    let graph_symbols = all_symbols_in_files(graph, &changed_paths)?;
    let mut symbols_by_file: HashMap<String, Vec<Value>> = HashMap::new();
    for symbol in &graph_symbols {
        symbols_by_file
            .entry(symbol_path(symbol)?.to_owned())
            .or_default()
            .push(symbol_value(symbol, true)?);
    }

    // For each changed file, get current symbols from the graph
    let mut symbols_added: Vec<Value> = Vec::new();
    let mut symbols_modified: Vec<Value> = Vec::new();
    let mut modified: Vec<Value> = Vec::new();
    let mut file_symbols: HashMap<String, Vec<Value>> = HashMap::new();

    for change in &changes {
        let file = &change.path;
        let symbols = symbols_by_file.remove(file).unwrap_or_default();

        if symbols.is_empty() {
            // File was likely removed or not indexed
            modified.push(json!({
                "file": file,
                "status": change.status,
            }));
        } else if change.status == "added" {
            symbols_added.extend(symbols.iter().cloned());
        } else {
            symbols_modified.extend(symbols.iter().cloned());
        }
        file_symbols.insert(file.clone(), symbols);
    }

    let touched_files: Vec<String> = changed_files.clone();

    let result = json!({
        "from_ref": from_ref,
        "to_ref": to_ref,
        "changed_file_count": changed_files.len(),
        "changed_files": changed_files,
        "symbols_added": symbols_added,
        "symbols_modified": symbols_modified,
        "symbols_in_changed_files": file_symbols
            .values()
            .flatten()
            .cloned()
            .collect::<Vec<_>>(),
        "files_not_indexed": modified,
    });

    Ok(generic_tool_result(
        Some(cg.project_root()),
        &args,
        &result,
        touched_files,
    ))
}

/// Handles `tracedecay_commit_context` tool calls.
pub(crate) async fn handle_commit_context(
    cg: &TraceDecay,
    graph: &VerifiedGraphQuery,
    args: Value,
) -> Result<ToolResult> {
    let staged_only = args
        .get("staged_only")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    // gix status classification walks the whole worktree; keep it off the
    // request runtime's workers so the carried dispatch deadline can preempt it.
    let changed_files = {
        let project_root = cg.project_root().to_path_buf();
        match blocking_git_span("status", move || {
            git_changed_files(&project_root, staged_only)
        })
        .await?
        {
            Ok(files) => files,
            Err(e) => {
                return Ok(git_error_result(cg, &args, "status", &e));
            }
        }
    };

    if changed_files.is_empty() {
        let project_root = cg.project_root().to_path_buf();
        let recent_commits = blocking_git_span("rev-walk", move || {
            git_recent_commits(&project_root, 5).unwrap_or_default()
        })
        .await?;
        let output = json!({
            "changed_files": [],
            "symbols_by_role": {},
            "suggested_category": Value::Null,
            "recent_commits": recent_commits,
            "summary": "No changes detected.",
        });
        return Ok(generic_tool_result(
            Some(cg.project_root()),
            &args,
            &output,
            vec![],
        ));
    }

    let changed_paths = changed_files.iter().cloned().collect::<HashSet<_>>();
    let files_with_inline_tests = graph.test_annotated_logical_files(
        Some(&changed_paths),
        VERIFIED_GRAPH_MAX_SYMBOLS,
        VERIFIED_GRAPH_MAX_RELATIONS,
    )?;
    let graph_symbols = all_symbols_in_files(graph, &changed_paths)?;
    let mut symbols_by_file: HashMap<String, Vec<&CodeGraphSymbolSummaryV1>> = HashMap::new();
    for symbol in &graph_symbols {
        symbols_by_file
            .entry(symbol_path(symbol)?.to_owned())
            .or_default()
            .push(symbol);
    }

    let mut file_roles: Vec<Value> = Vec::new();
    let mut symbols_by_role: HashMap<&str, Vec<Value>> = HashMap::new();

    for file in &changed_files {
        let role = classify_file_role(file, &files_with_inline_tests);
        let symbols = symbols_by_file.get(file).map_or(&[][..], Vec::as_slice);
        file_roles.push(json!({"file": file, "role": role, "symbols": symbols.len()}));

        // Config files (Cargo.toml, *.yaml, package.json, ...) explode into
        // one node per key. Surface a single summary entry per file instead
        // — agents only need to know "Cargo.toml changed, N keys touched",
        // not the name of every dependency listed.
        if role == "config" {
            symbols_by_role.entry(role).or_default().push(json!({
                "file": file,
                "kind": "config_summary",
                "config_keys": symbols.len(),
            }));
            continue;
        }
        for symbol in symbols {
            let metadata = symbol_metadata(symbol)?;
            symbols_by_role.entry(role).or_default().push(json!({
                "name": metadata.simple_name.as_str(),
                "kind": metadata.kind.as_str(),
                "file": symbol_path(symbol)?,
                "line": metadata.start_line,
            }));
        }
    }

    let has_tests = file_roles.iter().any(|f| f["role"] == "test");
    let has_source = file_roles.iter().any(|f| f["role"] == "source");
    let category = match (has_source, has_tests) {
        (true, true) => "feature/fix (source + tests)",
        (true, false) => "feature/fix/refactor",
        (false, true) => "test",
        (false, false) => "chore/docs/config",
    };

    let recent_commits = {
        let project_root = cg.project_root().to_path_buf();
        blocking_git_span("rev-walk", move || {
            git_recent_commits(&project_root, 5).unwrap_or_default()
        })
        .await?
    };

    let total_symbols: usize = symbols_by_role.values().map(std::vec::Vec::len).sum();
    let output = json!({
        "changed_files": file_roles,
        "symbols_by_role": symbols_by_role,
        "suggested_category": category,
        "recent_commits": recent_commits,
        "summary": format!("{} file(s) changed, {} symbol(s) affected", changed_files.len(), total_symbols),
    });

    Ok(generic_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
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
    deadline: Option<tracedecay_application::Deadline>,
    cancellation: Option<tracedecay_application::CancellationSignal>,
}

impl PrContextControls {
    fn checkpoint(&self) -> Result<()> {
        if self
            .cancellation
            .as_ref()
            .is_some_and(tracedecay_application::CancellationSignal::is_cancelled)
        {
            return Err(TraceDecayError::project_route(
                "pr_context_cancelled",
                true,
                "PR context was cancelled",
            ));
        }
        if self
            .deadline
            .as_ref()
            .is_some_and(|deadline| crate::daemon_client::deadline_remaining(deadline).is_none())
        {
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
    u64::try_from(started.elapsed().as_micros()).map_or(u64::MAX, |value| value)
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

/// Handles `tracedecay_pr_context` tool calls.
pub(crate) async fn handle_pr_context<F>(
    cg: &TraceDecay,
    graph: F,
    args: Value,
    deadline: Option<tracedecay_application::Deadline>,
    cancellation: Option<tracedecay_application::CancellationSignal>,
    registered_project_session_db: Option<RegisteredGlobalDbLeaseV1>,
) -> Result<ToolResult>
where
    F: Future<Output = Result<VerifiedGraphQuery>>,
{
    require_object_args(&args, "tracedecay_pr_context")?;
    let controls = PrContextControls {
        deadline,
        cancellation,
    };
    controls.checkpoint()?;
    let total_started = std::time::Instant::now();
    let mut stage_timings = serde_json::Map::new();
    let base = args
        .get("base_ref")
        .and_then(|v| v.as_str())
        .map_or_else(|| default_pr_base_ref(cg.project_root()), str::to_owned);
    let head = args
        .get("head_ref")
        .and_then(|v| v.as_str())
        .unwrap_or("HEAD");

    let stage_started = std::time::Instant::now();
    let comparison = {
        let project_root = cg.project_root().to_path_buf();
        let base_ref = base.clone();
        let head_ref = head.to_owned();
        match blocking_git_span_controlled(
            "PR comparison",
            controls.cancellation.clone(),
            controls.deadline.clone(),
            move |cancelled| {
                git_pr_comparison_controlled(&project_root, &base_ref, &head_ref, cancelled)
            },
        )
        .await?
        {
            Ok(comparison) => comparison,
            Err(e) => {
                controls.checkpoint()?;
                return Ok(git_error_result(cg, &args, "diff", &e));
            }
        }
    };
    controls.checkpoint()?;
    stage_timings.insert("git".to_owned(), json!(elapsed_micros(stage_started)));
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
            .then_with(|| left.status.cmp(right.status))
    });
    let changed_files: Vec<String> = changes.iter().map(|change| change.path.clone()).collect();
    let changed_paths = changed_files.iter().cloned().collect::<HashSet<_>>();

    let maximum_symbols = args
        .get("maximum_symbols")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(PR_CONTEXT_DEFAULT_SYMBOLS)
        .clamp(1, PR_CONTEXT_MAX_SYMBOLS);
    let encoded_cursor = match args.get("cursor") {
        Some(Value::String(cursor)) => Some(cursor.as_str()),
        Some(_) => {
            return Err(TraceDecayError::Config {
                message: "PR context cursor must be a string".to_owned(),
            });
        }
        None => None,
    };

    let stage_started = std::time::Instant::now();
    let graph = match graph.await {
        Ok(graph) => graph,
        Err(error) if encoded_cursor.is_some() || !graph_enrichment_is_transient(&error) => {
            return Err(error);
        }
        Err(error) => {
            stage_timings.insert("graph".to_owned(), json!(elapsed_micros(stage_started)));
            let test_files_changed = changes
                .iter()
                .filter(|change| crate::tracedecay::is_test_file(&change.path))
                .map(|change| change.path.clone())
                .collect::<Vec<_>>();
            let output = json!({
                "status": "partial",
                "message": "Verified graph results pending while the generation warms; Git comparison results are available.",
                "base": base,
                "head": head,
                "base_oid": base_oid,
                "head_oid": head_oid,
                "merge_base": merge_base,
                "graph_generation": null,
                "commits": commits,
                "files_changed": changed_files.len(),
                "changes": changes,
                "symbols_added": 0,
                "symbols_modified": 0,
                "added": [],
                "modified": [],
                "next_cursor": null,
                "symbol_page": {
                    "limit": maximum_symbols,
                    "returned": 0,
                    "has_more": false,
                    "complete": false,
                    "selection": "unavailable",
                    "continuation_available": false,
                },
                "analysis_coverage": {
                    "seed_symbols_analyzed": 0,
                    "symbols_returned": 0,
                    "symbols_complete": false,
                    "impact_nodes_admitted": 0,
                    "impact_nodes_returned": 0,
                    "direct_call_edges_admitted": 0,
                    "impact_bytes_admitted": 0,
                    "impact_partial": true,
                    "complete": false,
                },
                "test_files_changed": test_files_changed,
                "affected_tests": [],
                "affected_tests_coverage": {
                    "complete": false,
                    "selection": "unavailable",
                },
                "impacted_modules": [],
                "impacted_modules_coverage": {
                    "complete": false,
                    "selection": "unavailable",
                },
                "verified_graph_evidence": dependency_hints::unavailable_evidence(&error),
            });
            stage_timings.insert("total".to_owned(), json!(elapsed_micros(total_started)));
            let timing_value = Value::Object(stage_timings.clone());
            tracing::info!(
                tool = "tracedecay_pr_context",
                files = changed_files.len(),
                symbols = 0,
                timings = %timing_value,
                "PR context returned Git evidence while graph enrichment was unavailable"
            );
            return Ok(
                generic_tool_result(Some(cg.project_root()), &args, &output, changed_files)
                    .with_internal_analytics(json!({
                        "stage_timings_us": stage_timings,
                        "symbol_coverage": output["symbol_page"],
                    })),
            );
        }
    };
    stage_timings.insert("graph".to_owned(), json!(elapsed_micros(stage_started)));

    let graph_generation = graph.generation().as_str().to_owned();
    let project_root = cg.project_root().to_string_lossy();
    let cursor_binding = PrContextCursorBinding {
        protocol: "tracedecay.pr-context.cursor.v1",
        project_root: &project_root,
        base_oid: &base_oid,
        head_oid: &head_oid,
        merge_base: &merge_base,
        graph_generation: &graph_generation,
        maximum_symbols,
        changes: &changes,
    };
    let cursor_authority = match registered_project_session_db.as_deref() {
        Some(session_db) => Some(pr_context_cursor_authority(session_db, &cursor_binding).await?),
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
    let mut files_with_inline_tests = graph.test_annotated_logical_files(
        Some(&changed_paths),
        VERIFIED_GRAPH_MAX_SYMBOLS,
        VERIFIED_GRAPH_MAX_RELATIONS,
    )?;
    controls.checkpoint()?;
    stage_timings.insert(
        "test_annotations".to_owned(),
        json!(elapsed_micros(stage_started)),
    );
    let added_paths: Vec<String> = changes
        .iter()
        .filter(|change| change.status == "added")
        .map(|change| change.path.clone())
        .collect();
    let added_path_set: HashSet<&str> = added_paths.iter().map(String::as_str).collect();
    for change in &changes {
        if crate::tracedecay::is_test_file(&change.path)
            || files_with_inline_tests.contains(&change.path)
        {
            test_files_changed.push(change.path.clone());
        }
    }
    test_files_changed.sort();
    test_files_changed.dedup();

    let stage_started = std::time::Instant::now();
    let symbol_page = graph.symbols_in_logical_files_page(
        &changed_paths,
        cursor_position.as_ref().map(|position| &position.after),
        maximum_symbols,
        VERIFIED_GRAPH_MAX_SYMBOLS,
    )?;
    controls.checkpoint()?;
    stage_timings.insert(
        "symbol_page".to_owned(),
        json!(elapsed_micros(stage_started)),
    );
    let symbol_has_more = symbol_page.has_more;
    let next_page_key = symbol_page
        .symbols
        .last()
        .map(|symbol| symbol.occurrence.clone());
    let mut added = Vec::new();
    let mut modified = Vec::new();
    let mut nodes = Vec::with_capacity(symbol_page.symbols.len());
    for symbol in symbol_page.symbols {
        controls.checkpoint()?;
        let path = symbol_path(&symbol)?;
        let value = symbol_value(&symbol, false)?;
        if added_path_set.contains(path) {
            added.push(value);
        } else {
            modified.push(value);
        }
        nodes.push(symbol);
    }
    let returned_symbols = added.len().saturating_add(modified.len());
    let symbols_added = added.len();
    let symbols_modified = modified.len();

    // Find transitively affected test files
    let stage_started = std::time::Instant::now();
    let mut affected_tests: HashSet<String> = HashSet::new();
    let impact = pr_context_impact_snapshot(&graph, &nodes, 2, prior_impact_budget, &controls)?;
    controls.checkpoint()?;
    let impact_paths: Vec<String> = impact
        .nodes
        .iter()
        .map(|node| symbol_path(node).map(str::to_owned))
        .collect::<Result<Vec<_>>>()?;
    let impact_path_set = impact_paths.iter().cloned().collect::<HashSet<_>>();
    files_with_inline_tests.extend(graph.test_annotated_logical_files(
        Some(&impact_path_set),
        VERIFIED_GRAPH_MAX_SYMBOLS,
        VERIFIED_GRAPH_MAX_RELATIONS,
    )?);
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
            && (crate::tracedecay::is_test_file(path) || files_with_inline_tests.contains(path))
        {
            affected_tests.insert(path.to_owned());
        }
    }
    stage_timings.insert("impact".to_owned(), json!(elapsed_micros(stage_started)));

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
    let output = json!({
        "base": base,
        "head": head,
        "base_oid": base_oid,
        "head_oid": head_oid,
        "merge_base": merge_base,
        "graph_generation": graph_generation,
        "commits": commits,
        "files_changed": changed_files.len(),
        "changes": changes,
        "symbols_added": symbols_added,
        "symbols_modified": symbols_modified,
        "added": added,
        "modified": modified,
        "next_cursor": next_cursor,
        "symbol_page": {
            "limit": maximum_symbols,
            "returned": returned_symbols,
            "has_more": symbol_has_more,
            "complete": symbol_complete,
            "selection": "stable_prefix",
            "continuation_available": symbol_has_more,
        },
        "analysis_coverage": {
            "seed_symbols_analyzed": nodes.len(),
            "symbols_returned": returned_symbols,
            "symbols_complete": symbol_complete,
            "impact_nodes_admitted": impact.nodes_admitted,
            "impact_nodes_returned": impact.nodes.len(),
            "direct_call_edges_admitted": impact.direct_call_edges_admitted,
            "impact_bytes_admitted": impact.bytes_admitted,
            "impact_partial": impact.partial,
            "complete": impact_complete,
        },
        "test_files_changed": test_files_changed,
        "affected_tests": affected_sorted,
        "affected_tests_coverage": {
            "complete": impact_complete,
            "selection": "deterministic_bounded_prefix",
        },
        "impacted_modules": impacted_sorted,
        "impacted_modules_coverage": {
            "complete": impact_complete,
            "selection": "deterministic_bounded_prefix",
        },
    });
    stage_timings.insert("assemble".to_owned(), json!(elapsed_micros(stage_started)));
    stage_timings.insert("total".to_owned(), json!(elapsed_micros(total_started)));
    let timing_value = Value::Object(stage_timings.clone());
    tracing::info!(
        tool = "tracedecay_pr_context",
        files = changed_files.len(),
        symbols = returned_symbols,
        timings = %timing_value,
        "PR context stage timings"
    );

    Ok(
        generic_tool_result(Some(cg.project_root()), &args, &output, changed_files)
            .with_internal_analytics(json!({
                "stage_timings_us": stage_timings,
                "symbol_coverage": output["symbol_page"],
            })),
    )
}

#[cfg(test)]
mod blocking_git_span_tests {
    use super::{
        BlockingGitWorkerState, blocking_git_span, blocking_git_span_controlled_with_state,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[tokio::test]
    async fn a_blocking_span_returns_the_synchronous_result_unchanged() {
        let value = blocking_git_span("test", || Ok::<_, String>(vec!["a".to_owned()]))
            .await
            .expect("the join must succeed");
        assert_eq!(value, Ok(vec!["a".to_owned()]));
        let failure = blocking_git_span("test", || Err::<Vec<String>, _>("boom".to_owned()))
            .await
            .expect("a failing gix call is still a successful join");
        assert_eq!(failure, Err("boom".to_owned()));
    }

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
            tracedecay_application::CancellationSignal::active("cancel.git-worker-test")
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
