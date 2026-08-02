//! `tracedecay_diff_context`, `tracedecay_changelog`, `tracedecay_commit_context`, and `tracedecay_pr_context`.

use super::shell::{
    classify_file_role, default_pr_base_ref, git_changed_files, git_diff_file_changes,
    git_pr_comparison_controlled, git_recent_commits,
};
use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracedecay_global_db::RegisteredGlobalDb;

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

struct CancelBlockingGitOnDrop(Arc<AtomicBool>);

impl Drop for CancelBlockingGitOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
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
    let _cancel_on_drop = CancelBlockingGitOnDrop(Arc::clone(&state.cancelled));
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
    debug_assert!(state.exited.load(Ordering::Acquire));
    Ok(joined)
}

fn pr_context_checkpoint(
    cancellation: Option<&tracedecay_application::CancellationSignal>,
    deadline: Option<&tracedecay_application::Deadline>,
) -> Result<()> {
    if cancellation.is_some_and(tracedecay_application::CancellationSignal::is_cancelled) {
        return Err(TraceDecayError::project_route(
            "pr_context_cancelled",
            true,
            "PR context was cancelled",
        ));
    }
    if deadline.is_some_and(|deadline| crate::daemon_client::deadline_remaining(deadline).is_none())
    {
        return Err(TraceDecayError::project_route(
            "tool_dispatch_deadline_exceeded",
            true,
            "PR context exceeded its dispatch deadline",
        ));
    }
    Ok(())
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

/// Handles `tracedecay_pr_context` tool calls.
pub(crate) async fn handle_pr_context(
    cg: &TraceDecay,
    args: Value,
    deadline: Option<tracedecay_application::Deadline>,
    cancellation: Option<tracedecay_application::CancellationSignal>,
    _registered_project_session_db: Option<Arc<RegisteredGlobalDb>>,
) -> Result<ToolResult> {
    pr_context_checkpoint(cancellation.as_ref(), deadline.as_ref())?;
    let base = args
        .get("base_ref")
        .and_then(|v| v.as_str())
        .map_or_else(|| default_pr_base_ref(cg.project_root()), str::to_owned);
    let head = args
        .get("head_ref")
        .and_then(|v| v.as_str())
        .unwrap_or("HEAD");

    // The gix repo open, merge-base resolution, tree diff, and revwalk are all
    // synchronous and unbounded on a diverged or pathological ref. Run them on
    // the blocking pool so they never starve the async worker and so the
    // dispatch deadline enforced in `dispatch_git_tools` can actually preempt
    // this span (a `tokio::time::timeout` cannot interrupt an inline blocking
    // call — only the `spawn_blocking` join future it awaits here).
    let comparison = {
        let project_root = cg.project_root().to_path_buf();
        let base_ref = base.clone();
        let head_ref = head.to_owned();
        match blocking_git_span_controlled(
            "PR comparison",
            cancellation.clone(),
            deadline.clone(),
            move |cancelled| {
                git_pr_comparison_controlled(&project_root, &base_ref, &head_ref, cancelled)
            },
        )
        .await?
        {
            Ok(comparison) => comparison,
            Err(e) => {
                pr_context_checkpoint(cancellation.as_ref(), deadline.as_ref())?;
                return Ok(git_error_result(cg, &args, "diff", &e));
            }
        }
    };
    pr_context_checkpoint(cancellation.as_ref(), deadline.as_ref())?;
    let GitPrComparison {
        base_oid,
        head_oid,
        merge_base,
        changes,
        commits,
    } = comparison;
    let changed_files: Vec<String> = changes.iter().map(|change| change.path.clone()).collect();

    let mut symbols_added: Vec<Value> = Vec::new();
    let mut symbols_modified: Vec<Value> = Vec::new();
    let mut test_files_changed: Vec<String> = Vec::new();
    let mut impacted_modules: HashSet<String> = HashSet::new();

    // Pre-compute files with inline test modules.
    let files_with_inline_tests = cg.get_files_with_test_annotations().await?;
    let has_tests = |path: &str| {
        crate::tracedecay::is_test_file(path) || files_with_inline_tests.contains(path)
    };

    for change in &changes {
        let file = &change.path;
        if has_tests(file) {
            test_files_changed.push(file.clone());
        }

        let nodes = cg.get_nodes_by_file(file).await?;

        // Config files explode into one node per key — Cargo.toml with 50
        // dependencies blows past the response budget. Treat them as a
        // single summary symbol attributed to `symbols_modified` (they're
        // never "added" since the file pre-exists in a typical PR).
        if classify_file_role(file, &files_with_inline_tests) == "config" {
            symbols_modified.push(json!({
                "file": file,
                "kind": "config_summary",
                "config_keys": nodes.len(),
            }));
            continue;
        }

        for node in &nodes {
            let sym = json!({
                "name": node.name,
                "kind": node.kind.as_str(),
                "file": node.file_path,
                "line": node.start_line,
            });

            // Only brand symbols as added when the file itself is added. For
            // modified files the graph only has the post-change symbol set, so
            // per-symbol added/modified inference would overstate additions.
            let callers = cg.get_callers(&node.id, 1).await?;
            let has_external_callers = callers
                .iter()
                .any(|(c, _)| !changed_files.contains(&c.file_path));

            if change.status == "added" {
                symbols_added.push(sym);
            } else {
                symbols_modified.push(sym);
            }

            if has_external_callers {
                for (caller, _) in &callers {
                    if !changed_files.contains(&caller.file_path) {
                        let dir = caller
                            .file_path
                            .rfind('/')
                            .map_or(caller.file_path.as_str(), |i| &caller.file_path[..i]);
                        impacted_modules.insert(dir.to_string());
                    }
                }
            }
        }
    }

    // Find transitively affected test files
    let mut affected_tests: HashSet<String> = HashSet::new();
    for file in &changed_files {
        if has_tests(file) {
            continue;
        }
        let nodes = cg.get_nodes_by_file(file).await?;
        for node in &nodes {
            let impact = cg.get_impact_radius(&node.id, 2).await?;
            for impacted in &impact.nodes {
                if has_tests(&impacted.file_path) {
                    affected_tests.insert(impacted.file_path.clone());
                }
            }
        }
    }

    let mut impacted_sorted: Vec<String> = impacted_modules.into_iter().collect();
    impacted_sorted.sort();
    let mut affected_sorted: Vec<String> = affected_tests.into_iter().collect();
    affected_sorted.sort();

    let output = json!({
        "base": base,
        "head": head,
        "base_oid": base_oid,
        "head_oid": head_oid,
        "merge_base": merge_base,
        "commits": commits,
        "files_changed": changed_files.len(),
        "symbols_added": symbols_added.len(),
        "symbols_modified": symbols_modified.len(),
        "added": symbols_added,
        "modified": symbols_modified,
        "test_files_changed": test_files_changed,
        "affected_tests": affected_sorted,
        "impacted_modules": impacted_sorted,
    });

    Ok(generic_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        changed_files,
    ))
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
    async fn cancellation_waits_for_the_blocking_worker_to_exit() {
        let cancellation =
            tracedecay_application::CancellationSignal::active("cancel.pr-context-worker")
                .expect("cancellation");
        let trigger = cancellation.clone();
        let state = BlockingGitWorkerState::new();
        let observed = state.clone();
        let canceller = tokio::spawn(async move {
            tokio::task::yield_now().await;
            trigger.cancel(tracedecay_domain::UtcMicros(1));
        });

        let value = blocking_git_span_controlled_with_state(
            "test",
            Some(cancellation),
            None,
            state,
            |cancelled| {
                while !cancelled() {
                    std::thread::yield_now();
                }
                7_u8
            },
        )
        .await
        .expect("cancelled worker joins");

        canceller.await.expect("canceller joins");
        assert_eq!(value, 7);
        assert!(observed.cancelled.load(Ordering::Acquire));
        assert!(observed.exited.load(Ordering::Acquire));
    }
}

// ── Cross-branch tools ─────────────────────────────────────────────────
