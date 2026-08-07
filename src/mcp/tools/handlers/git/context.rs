//! `tracedecay_diff_context`, `tracedecay_changelog`, `tracedecay_commit_context`, and `tracedecay_pr_context`.

use super::pr_context_cursor::{
    PrContextCursorBinding, decode_pr_context_cursor, encode_pr_context_cursor,
    pr_context_cursor_authority,
};
use super::shell::{
    classify_file_role, default_pr_base_ref, git_changed_files, git_diff_file_changes,
    git_pr_comparison_controlled, git_recent_commits,
};
use super::*;
use crate::types::{Edge, EdgeKind, Node};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::db::{DatabaseEngineReadSnapshot, NodesByFilesPageKey};

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
pub(crate) async fn handle_diff_context(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
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

    // Pre-compute files containing inline test modules.
    let files_with_inline_tests = cg.get_files_with_test_annotations().await?;
    let has_tests = |path: &str| {
        crate::tracedecay::is_test_file(path) || files_with_inline_tests.contains(path)
    };

    // First pass: gather all modified symbols.
    let mut modified_ids: Vec<String> = Vec::new();
    for file in &files {
        let nodes = cg.get_nodes_by_file(file).await?;
        for node in &nodes {
            all_touched_files.push(node.file_path.clone());
            // Dedup by node id: `get_nodes_by_file` can return the same node
            // twice if the index contains duplicates from re-extraction, and
            // even when it doesn't, callers may legitimately want one entry
            // per node — never one entry per (file, node) pair.
            if !modified_seen.insert(node.id.clone()) {
                continue;
            }
            modified_symbols.push(json!({
                "id": node.id,
                "name": node.name,
                "kind": node.kind.as_str(),
                "file": node.file_path,
                "line": node.start_line,
            }));
            modified_ids.push(node.id.clone());
        }
    }

    // Single multi-source BFS over the union of impact radii. Sharing a
    // `visited` set means each downstream node is walked at most once, even
    // when many modified symbols reach it through diamond dependencies — the
    // old per-symbol loop re-traversed the same subtree N times.
    let impacted = cg.get_impact_radius_multi(&modified_ids, depth).await?;
    for impacted_node in &impacted {
        // Drop seeds: callers want impacted symbols distinct from the
        // modified ones, mirroring the old per-node `if impacted.id == node.id`.
        if modified_seen.contains(&impacted_node.id) {
            continue;
        }
        if !impacted_seen.insert(impacted_node.id.clone()) {
            continue;
        }
        impacted_symbols.push(json!({
            "id": impacted_node.id,
            "name": impacted_node.name,
            "kind": impacted_node.kind.as_str(),
            "file": impacted_node.file_path,
            "line": impacted_node.start_line,
        }));
        if has_tests(&impacted_node.file_path) {
            affected_tests.insert(impacted_node.file_path.clone());
        }
    }

    let traversal =
        collect_affected_test_files(cg, &files, depth, None, &files_with_inline_tests).await?;
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
pub(crate) async fn handle_changelog(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
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

    // For each changed file, get current symbols from the graph
    let mut symbols_added: Vec<Value> = Vec::new();
    let mut symbols_modified: Vec<Value> = Vec::new();
    let mut modified: Vec<Value> = Vec::new();
    let mut file_symbols: HashMap<String, Vec<Value>> = HashMap::new();

    for change in &changes {
        let file = &change.path;
        let nodes = cg.get_nodes_by_file(file).await?;
        let symbols: Vec<Value> = nodes
            .iter()
            .map(|n| {
                json!({
                    "id": n.id,
                    "name": n.name,
                    "kind": n.kind.as_str(),
                    "file": n.file_path,
                    "line": n.start_line,
                    "signature": n.signature,
                })
            })
            .collect();

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
pub(crate) async fn handle_commit_context(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
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

    // Pre-compute files with inline test modules.
    let files_with_inline_tests = cg.get_files_with_test_annotations().await?;

    let mut file_roles: Vec<Value> = Vec::new();
    let mut symbols_by_role: HashMap<&str, Vec<Value>> = HashMap::new();

    for file in &changed_files {
        let role = classify_file_role(file, &files_with_inline_tests);
        let nodes = cg.get_nodes_by_file(file).await?;
        file_roles.push(json!({"file": file, "role": role, "symbols": nodes.len()}));

        // Config files (Cargo.toml, *.yaml, package.json, ...) explode into
        // one node per key. Surface a single summary entry per file instead
        // — agents only need to know "Cargo.toml changed, N keys touched",
        // not the name of every dependency listed.
        if role == "config" {
            symbols_by_role.entry(role).or_default().push(json!({
                "file": file,
                "kind": "config_summary",
                "config_keys": nodes.len(),
            }));
            continue;
        }
        for node in &nodes {
            symbols_by_role.entry(role).or_default().push(json!({
                "name": node.name,
                "kind": node.kind.as_str(),
                "file": node.file_path,
                "line": node.start_line,
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

async fn pr_context_impact_snapshot(
    snapshot: &DatabaseEngineReadSnapshot,
    seed_nodes: &[Node],
    max_depth: usize,
    prior_budget: PrContextImpactBudget,
    controls: &PrContextControls,
) -> Result<PrContextImpact> {
    let mut impact = PrContextImpact {
        nodes_admitted: prior_budget.nodes_admitted,
        edges_admitted: prior_budget.edges_admitted,
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
        visited.insert(node.id.clone());
        frontier.push(node.id.clone());
        impact.nodes.push(node.clone());
    }
    for depth in 0..max_depth {
        if frontier.is_empty() {
            break;
        }
        let remaining_edges = PR_CONTEXT_MAX_IMPACT_EDGES.saturating_sub(impact.edges_admitted);
        if remaining_edges == 0 {
            impact.partial = true;
            break;
        }
        controls.checkpoint()?;
        let edge_page = snapshot
            .get_incoming_edges_bulk_page_controlled(&frontier, &[], remaining_edges, || {
                controls.checkpoint()
            })
            .await?;
        impact.edge_rows_read = impact.edge_rows_read.saturating_add(edge_page.rows_read);
        impact.partial |= edge_page.has_more;
        let mut next_ids = Vec::new();
        for edge in edge_page.edges {
            let bytes = pr_context_edge_bytes(&edge);
            if impact.bytes_admitted.saturating_add(bytes) > PR_CONTEXT_MAX_IMPACT_BYTES {
                impact.partial = true;
                break;
            }
            impact.bytes_admitted = impact.bytes_admitted.saturating_add(bytes);
            impact.edges_admitted = impact.edges_admitted.saturating_add(1);
            if depth == 0 && edge.kind == EdgeKind::Calls {
                impact.incoming_calls.push(edge.clone());
            }
            if visited.insert(edge.source.clone()) {
                next_ids.push(edge.source);
            }
        }
        if next_ids.is_empty() {
            break;
        }
        let remaining_nodes = PR_CONTEXT_MAX_IMPACT_NODES.saturating_sub(impact.nodes_admitted);
        if next_ids.len() > remaining_nodes {
            next_ids.truncate(remaining_nodes);
            impact.partial = true;
        }
        let nodes = snapshot
            .get_nodes_by_ids_controlled(&next_ids, || controls.checkpoint())
            .await?;
        frontier.clear();
        for node in nodes {
            let bytes = pr_context_node_bytes(&node);
            if impact.bytes_admitted.saturating_add(bytes) > PR_CONTEXT_MAX_IMPACT_BYTES {
                impact.partial = true;
                continue;
            }
            impact.bytes_admitted = impact.bytes_admitted.saturating_add(bytes);
            impact.nodes_admitted = impact.nodes_admitted.saturating_add(1);
            frontier.push(node.id.clone());
            impact.nodes.push(node);
        }
    }
    Ok(impact)
}

#[derive(Clone, Copy, Default)]
struct PrContextImpactBudget {
    nodes_admitted: usize,
    edges_admitted: usize,
    bytes_admitted: usize,
}

#[derive(Default)]
struct PrContextImpact {
    nodes: Vec<Node>,
    incoming_calls: Vec<Edge>,
    nodes_admitted: usize,
    edges_admitted: usize,
    edge_rows_read: usize,
    bytes_admitted: usize,
    partial: bool,
}

fn pr_context_node_bytes(node: &Node) -> usize {
    node.id
        .len()
        .saturating_add(node.name.len())
        .saturating_add(node.qualified_name.len())
        .saturating_add(node.file_path.len())
        .saturating_add(node.docstring.as_ref().map_or(0, String::len))
        .saturating_add(node.signature.as_ref().map_or(0, String::len))
}

fn pr_context_edge_bytes(edge: &Edge) -> usize {
    edge.source
        .len()
        .saturating_add(edge.target.len())
        .saturating_add(edge.kind.as_str().len())
}

/// Handles `tracedecay_pr_context` tool calls.
pub(crate) async fn handle_pr_context(
    cg: &TraceDecay,
    args: Value,
    deadline: Option<tracedecay_application::Deadline>,
    cancellation: Option<tracedecay_application::CancellationSignal>,
    registered_project_session_db: Option<Arc<RegisteredGlobalDb>>,
) -> Result<ToolResult> {
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
    let changed_paths: HashSet<&str> = changed_files.iter().map(String::as_str).collect();

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

    let graph_snapshot = cg
        .db()
        .begin_engine_read_snapshot("PR context graph snapshot")
        .await?;
    let graph_generation = graph_snapshot.graph_generation_identity().await?;
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
                    edges_admitted: position.impact_edges_admitted,
                    bytes_admitted: position.impact_bytes_admitted,
                }
            });

    let mut test_files_changed: Vec<String> = Vec::new();
    let mut impacted_modules: HashSet<String> = HashSet::new();

    // Pre-compute files with inline test modules.
    let stage_started = std::time::Instant::now();
    let mut files_with_inline_tests = graph_snapshot
        .get_files_with_test_annotations_for_paths_controlled(&changed_files, || {
            controls.checkpoint()
        })
        .await?;
    controls.checkpoint()?;
    stage_timings.insert(
        "test_annotations".to_owned(),
        json!(elapsed_micros(stage_started)),
    );
    let config_paths: Vec<String> = changes
        .iter()
        .filter(|change| classify_file_role(&change.path, &files_with_inline_tests) == "config")
        .map(|change| change.path.clone())
        .collect();
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
    let symbol_page = graph_snapshot
        .get_nodes_by_files_page_controlled(
            &changed_files,
            &config_paths,
            cursor_position.as_ref().map(|position| &position.page_key),
            maximum_symbols,
            || controls.checkpoint(),
        )
        .await?;
    controls.checkpoint()?;
    stage_timings.insert(
        "symbol_page".to_owned(),
        json!(elapsed_micros(stage_started)),
    );
    let symbol_has_more = symbol_page.has_more;
    let symbol_rows_read = symbol_page.rows_read;
    let next_page_key = symbol_page.entries.last().map(|entry| NodesByFilesPageKey {
        file_path: entry.file_path.clone(),
        start_line: entry.start_line,
        rowid: entry.rowid,
    });
    let mut added = Vec::new();
    let mut modified = Vec::new();
    let mut nodes = Vec::with_capacity(symbol_page.entries.len());
    for entry in symbol_page.entries {
        controls.checkpoint()?;
        let symbol = if entry.is_config_summary {
            json!({
                "file": &entry.file_path,
                "kind": "config_summary",
                "config_keys": Value::Null,
                "coverage": "bounded_representative",
            })
        } else {
            let node = entry
                .node
                .as_ref()
                .ok_or_else(|| TraceDecayError::Database {
                    message: "source symbol page entry has no node".to_owned(),
                    operation: "get_nodes_by_files_page".to_owned(),
                })?;
            json!({
                "name": &node.name,
                "kind": node.kind.as_str(),
                "file": &node.file_path,
                "line": node.start_line,
            })
        };
        if added_path_set.contains(entry.file_path.as_str()) {
            added.push(symbol);
        } else {
            modified.push(symbol);
        }
        if let Some(node) = entry.node {
            nodes.push(node);
        }
    }
    let returned_symbols = added.len().saturating_add(modified.len());
    let symbols_added = added.len();
    let symbols_modified = modified.len();

    // Find transitively affected test files
    let stage_started = std::time::Instant::now();
    let mut affected_tests: HashSet<String> = HashSet::new();
    let impact =
        pr_context_impact_snapshot(&graph_snapshot, &nodes, 2, prior_impact_budget, &controls)
            .await?;
    controls.checkpoint()?;
    let impact_paths: Vec<String> = impact
        .nodes
        .iter()
        .map(|node| node.file_path.clone())
        .collect();
    files_with_inline_tests.extend(
        graph_snapshot
            .get_files_with_test_annotations_for_paths_controlled(&impact_paths, || {
                controls.checkpoint()
            })
            .await?,
    );
    let impacted_by_id: HashMap<&str, &Node> = impact
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    for edge in &impact.incoming_calls {
        if let Some(caller) = impacted_by_id.get(edge.source.as_str())
            && !changed_paths.contains(caller.file_path.as_str())
        {
            let dir = caller
                .file_path
                .rfind('/')
                .map_or(caller.file_path.as_str(), |index| {
                    &caller.file_path[..index]
                });
            impacted_modules.insert(dir.to_owned());
        }
    }
    for impacted in &impact.nodes {
        if !changed_paths.contains(impacted.file_path.as_str())
            && (crate::tracedecay::is_test_file(&impacted.file_path)
                || files_with_inline_tests.contains(&impacted.file_path))
        {
            affected_tests.insert(impacted.file_path.clone());
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
            impact.edges_admitted,
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
        "symbols_added": symbols_added,
        "symbols_modified": symbols_modified,
        "added": added,
        "modified": modified,
        "next_cursor": next_cursor,
        "symbol_page": {
            "limit": maximum_symbols,
            "returned": returned_symbols,
            "rows_read": symbol_rows_read,
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
            "impact_edges_admitted": impact.edges_admitted,
            "impact_edge_rows_read": impact.edge_rows_read,
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
