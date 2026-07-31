//! Git/branch/diff tool handlers: `diff_context`, `commit_context`, `pr_context`,
//! `changelog`, `branch_list`, `branch_search`, `branch_diff`, `affected`, and
//! git helper functions.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;

use serde_json::{Value, json};

use super::super::ToolResult;
use super::super::render;
use super::support::unique_file_paths;
use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::TraceDecay;

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitFileChange {
    path: String,
    status: &'static str,
}

struct GitPrComparison {
    merge_base: String,
    changes: Vec<GitFileChange>,
    commits: Vec<Value>,
}

fn git_error_result(cg: &TraceDecay, args: &Value, operation: &str, message: &str) -> ToolResult {
    let output = json!({
        "error": {
            "kind": "git",
            "operation": operation,
            "message": message,
        }
    });
    let text = render::finalize(Some(cg.project_root()), args, &output, || {
        render::generic_md(&output)
    });
    ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        vec![],
    )
    .with_semantic_error(true)
    .with_failure_message(message)
}

fn require_string_array_arg(args: &Value, name: &str) -> Result<Vec<String>> {
    args.get(name)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(std::string::ToString::to_string))
                .collect()
        })
        .ok_or_else(|| TraceDecayError::Config {
            message: format!("missing required parameter: {name} (array of strings)"),
        })
}

fn clamped_depth_arg(args: &Value, name: &str, default: usize, max: usize) -> usize {
    args.get(name)
        .and_then(serde_json::Value::as_u64)
        .map_or(default, |v| v.min(max as u64) as usize)
}

fn matches_test_file(
    path: &str,
    custom_glob: Option<&glob::Pattern>,
    files_with_inline_tests: &HashSet<String>,
) -> bool {
    if let Some(glob) = custom_glob {
        glob.matches(path)
    } else {
        crate::tracedecay::is_test_file(path) || files_with_inline_tests.contains(path)
    }
}

type FileDependentsByFile = HashMap<String, Vec<String>>;
type AffectedDependentsFuture<'a> =
    Pin<Box<dyn Future<Output = Result<FileDependentsByFile>> + Send + 'a>>;

pub(crate) trait AffectedTestDependents: Sync {
    fn get_file_dependents_batch<'a>(&'a self, files: &'a [String])
    -> AffectedDependentsFuture<'a>;
}

impl AffectedTestDependents for TraceDecay {
    fn get_file_dependents_batch<'a>(
        &'a self,
        files: &'a [String],
    ) -> AffectedDependentsFuture<'a> {
        Box::pin(async move {
            let mut dependents: FileDependentsByFile = HashMap::new();
            for file in files {
                dependents.insert(file.clone(), self.get_file_dependents(file).await?);
            }
            Ok(dependents)
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RankedAffectedTest {
    pub(crate) path: String,
    pub(crate) distance: usize,
}

pub(crate) struct AffectedTestTraversal {
    pub(crate) test_distances: HashMap<String, usize>,
}

pub(crate) fn rank_affected_tests(
    test_distances: &HashMap<String, usize>,
) -> Vec<RankedAffectedTest> {
    let mut ranked = test_distances
        .iter()
        .map(|(path, distance)| RankedAffectedTest {
            path: path.clone(),
            distance: *distance,
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        left.distance
            .cmp(&right.distance)
            .then_with(|| left.path.cmp(&right.path))
    });
    ranked
}

pub(crate) fn affected_test_proximity(distance: usize) -> &'static str {
    match distance {
        0 => "changed",
        1 => "direct",
        2 => "near",
        _ => "transitive",
    }
}

pub(crate) async fn collect_affected_test_files<D: AffectedTestDependents + ?Sized>(
    dependents_source: &D,
    files: &[String],
    max_depth: usize,
    custom_glob: Option<&glob::Pattern>,
    files_with_inline_tests: &HashSet<String>,
) -> Result<AffectedTestTraversal> {
    let mut test_distances: HashMap<String, usize> = HashMap::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut frontier = Vec::new();

    for file in files {
        if matches_test_file(file, custom_glob, files_with_inline_tests) {
            test_distances.insert(file.clone(), 0);
        }
        if visited.insert(file.clone()) {
            frontier.push(file.clone());
        }
    }
    frontier.sort();

    for depth in 0..max_depth {
        if frontier.is_empty() {
            break;
        }
        let dependents_by_file = dependents_source
            .get_file_dependents_batch(&frontier)
            .await?;
        let mut dependents = frontier
            .iter()
            .filter_map(|file| dependents_by_file.get(file))
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        dependents.sort();
        dependents.dedup();

        let mut next_frontier = Vec::new();
        for dep in dependents {
            if !visited.insert(dep.clone()) {
                continue;
            }
            if matches_test_file(&dep, custom_glob, files_with_inline_tests) {
                test_distances.insert(dep, depth + 1);
            } else {
                next_frontier.push(dep);
            }
        }
        frontier = next_frontier;
    }

    Ok(AffectedTestTraversal { test_distances })
}

/// Handles `tracedecay_affected` tool calls.
pub(super) async fn handle_affected(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let files = require_string_array_arg(&args, "files")?;
    let max_depth = clamped_depth_arg(&args, "depth", 5, 10);

    let custom_filter = args.get("filter").and_then(|v| v.as_str());
    let custom_glob = custom_filter.and_then(|p| glob::Pattern::new(p).ok());

    let files_with_inline_tests = cg.get_files_with_test_annotations().await?;
    let traversal = collect_affected_test_files(
        cg,
        &files,
        max_depth,
        custom_glob.as_ref(),
        &files_with_inline_tests,
    )
    .await?;

    let mut result = traversal.test_distances.keys().cloned().collect::<Vec<_>>();
    result.sort();
    let ranked = rank_affected_tests(&traversal.test_distances);
    let ranked_tests = ranked
        .iter()
        .enumerate()
        .map(|(index, test)| {
            json!({
                "path": test.path,
                "rank": index + 1,
                "distance": test.distance,
                "proximity": affected_test_proximity(test.distance),
            })
        })
        .collect::<Vec<_>>();
    let recommended_tests = ranked
        .iter()
        .filter(|test| test.distance <= 2)
        .map(|test| test.path.clone())
        .collect::<Vec<_>>();

    let touched_files = unique_file_paths(result.iter().map(std::string::String::as_str));
    let output = json!({
        "changed_files": files,
        "affected_tests": result,
        "count": result.len(),
        "ranked_tests": ranked_tests,
        "recommended_tests": recommended_tests,
        "ranking_metadata": {
            "strategy": "dependency_distance_then_path",
            "distance": "minimum file-dependency hops from the changed files",
            "recommended_proximity": ["changed", "direct", "near"],
            "compatibility_field": "affected_tests",
        },
    });

    let text = render::finalize(Some(cg.project_root()), &args, &output, || {
        render::generic_md(&output)
    });
    Ok(ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        touched_files,
    ))
}

/// Handles `tracedecay_diff_context` tool calls.
pub(super) async fn handle_diff_context(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    debug_assert!(
        args.is_object(),
        "handle_diff_context expects an object argument"
    );
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

    let text = render::finalize(Some(cg.project_root()), &args, &output, || {
        render::generic_md(&output)
    });
    Ok(ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        touched_files,
    ))
}

/// Diff two git refs and return changed file paths with coarse status.
fn git_diff_file_changes(
    project_root: &std::path::Path,
    from_ref: &str,
    to_ref: &str,
) -> std::result::Result<Vec<GitFileChange>, String> {
    let repo = gix::open(project_root).map_err(|e| format!("failed to open git repo: {e}"))?;

    let from_tree = repo
        .rev_parse_single(from_ref)
        .map_err(|e| format!("cannot resolve '{from_ref}': {e}"))?
        .object()
        .map_err(|e| format!("cannot read object for '{from_ref}': {e}"))?
        .peel_to_tree()
        .map_err(|e| format!("cannot peel '{from_ref}' to tree: {e}"))?;

    let to_tree = repo
        .rev_parse_single(to_ref)
        .map_err(|e| format!("cannot resolve '{to_ref}': {e}"))?
        .object()
        .map_err(|e| format!("cannot read object for '{to_ref}': {e}"))?
        .peel_to_tree()
        .map_err(|e| format!("cannot peel '{to_ref}' to tree: {e}"))?;

    let mut changed = Vec::new();
    from_tree
        .changes()
        .map_err(|e| format!("diff init failed: {e}"))?
        .for_each_to_obtain_tree(&to_tree, |change| {
            use gix::object::tree::diff::Change;
            // `for_each_to_obtain_tree` walks one level at a time — if an
            // entire subtree was added, deleted, or moved, the entry's
            // `entry_mode` is a tree, not a blob. We only want file paths
            // downstream, so skip tree entries before pushing. The earlier
            // `is_dir()` fallback after-the-fact missed deletions, where the
            // path no longer exists on disk.
            match &change {
                Change::Addition {
                    location,
                    entry_mode,
                    ..
                } => {
                    if !entry_mode.is_tree() {
                        changed.push(GitFileChange {
                            path: location.to_string(),
                            status: "added",
                        });
                    }
                }
                Change::Modification {
                    location,
                    entry_mode,
                    ..
                } => {
                    if !entry_mode.is_tree() {
                        changed.push(GitFileChange {
                            path: location.to_string(),
                            status: "modified",
                        });
                    }
                }
                Change::Deletion {
                    location,
                    entry_mode,
                    ..
                } => {
                    if !entry_mode.is_tree() {
                        changed.push(GitFileChange {
                            path: location.to_string(),
                            status: "deleted",
                        });
                    }
                }
                Change::Rewrite {
                    source_location,
                    location,
                    source_entry_mode,
                    entry_mode,
                    ..
                } => {
                    if !source_entry_mode.is_tree() {
                        changed.push(GitFileChange {
                            path: source_location.to_string(),
                            status: "deleted",
                        });
                    }
                    if !entry_mode.is_tree() {
                        changed.push(GitFileChange {
                            path: location.to_string(),
                            status: "added",
                        });
                    }
                }
            }
            Ok::<_, std::convert::Infallible>(std::ops::ControlFlow::Continue(()))
        })
        .map_err(|e| format!("tree diff failed: {e}"))?;

    // Belt-and-suspenders: even with the entry_mode check above, drop any
    // path that resolves to a directory on disk for additions/modifications.
    // Pure deletions can't be checked this way (the path is gone), which is
    // exactly why entry_mode.is_tree() above is the load-bearing filter.
    changed.retain(|change| !project_root.join(&change.path).is_dir());
    Ok(changed)
}

/// Resolve PR refs to their common ancestor and compare only changes reachable
/// from the head. This matches `git diff base...head`; comparing the two tip
/// trees directly would incorrectly report unrelated files added to an
/// advanced default branch as deletions in the PR.
fn git_pr_comparison(
    project_root: &std::path::Path,
    base_ref: &str,
    head_ref: &str,
) -> std::result::Result<GitPrComparison, String> {
    let repo = gix::open(project_root).map_err(|e| format!("failed to open git repo: {e}"))?;
    let base_commit = repo
        .rev_parse_single(base_ref)
        .map_err(|e| format!("cannot resolve '{base_ref}': {e}"))?
        .object()
        .map_err(|e| format!("cannot read object for '{base_ref}': {e}"))?
        .peel_to_commit()
        .map_err(|e| format!("cannot peel '{base_ref}' to commit: {e}"))?;
    let head_commit = repo
        .rev_parse_single(head_ref)
        .map_err(|e| format!("cannot resolve '{head_ref}': {e}"))?
        .object()
        .map_err(|e| format!("cannot read object for '{head_ref}': {e}"))?
        .peel_to_commit()
        .map_err(|e| format!("cannot peel '{head_ref}' to commit: {e}"))?;
    let merge_base = repo
        .merge_base(base_commit.id, head_commit.id)
        .map_err(|e| format!("cannot find merge base for '{base_ref}' and '{head_ref}': {e}"))?;
    let merge_base = merge_base.to_string();

    Ok(GitPrComparison {
        changes: git_diff_file_changes(project_root, &merge_base, head_ref)?,
        commits: git_commit_log(project_root, &merge_base, head_ref)?,
        merge_base,
    })
}

fn default_pr_base_ref(project_root: &std::path::Path) -> String {
    crate::branch::detect_default_branch(project_root).unwrap_or_else(|| "main".to_string())
}

/// Returns file paths changed in the working tree (unstaged + staged, or staged-only).
fn git_changed_files(
    project_root: &std::path::Path,
    staged_only: bool,
) -> std::result::Result<Vec<String>, String> {
    let repo = gix::open(project_root).map_err(|e| format!("failed to open git repo: {e}"))?;

    let head_tree = repo
        .head()
        .map_err(|e| format!("cannot read HEAD: {e}"))?
        .peel_to_commit()
        .map_err(|e| format!("cannot peel HEAD to commit: {e}"))?
        .tree()
        .map_err(|e| format!("cannot read HEAD tree: {e}"))?;

    // Compare HEAD tree against the index (staged changes)
    let index = repo
        .index()
        .map_err(|e| format!("cannot read index: {e}"))?;

    let mut changed = HashSet::new();

    // Walk the index to find files that differ from HEAD
    for entry in index.entries() {
        let path = entry.path(&index);
        let path_str = String::from_utf8_lossy(path.as_ref()).to_string();
        if path_str.is_empty() {
            continue;
        }

        let head_entry = head_tree
            .lookup_entry_by_path(std::path::Path::new(&path_str))
            .ok()
            .flatten();

        match head_entry {
            Some(he) => {
                // File exists in both - check if content differs
                if he.object_id() != entry.id {
                    changed.insert(path_str);
                }
            }
            None => {
                // New file (in index but not in HEAD)
                changed.insert(path_str);
            }
        }
    }

    // If not staged_only, also check working-tree modifications via mtime
    if !staged_only {
        for entry in index.entries() {
            let path = entry.path(&index);
            let path_str = String::from_utf8_lossy(path.as_ref()).to_string();
            if path_str.is_empty() {
                continue;
            }
            let full_path = project_root.join(&path_str);
            if let Ok(meta) = std::fs::metadata(&full_path) {
                use std::time::UNIX_EPOCH;
                let mtime = meta
                    .modified()
                    .unwrap_or(UNIX_EPOCH)
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as u32;
                // gix index entry stores mtime; if disk mtime is newer, file is modified
                if mtime > entry.stat.mtime.secs {
                    changed.insert(path_str);
                }
            }
        }
    }

    let mut result: Vec<String> = changed.into_iter().collect();
    result.sort();
    Ok(result)
}

/// Returns the last N commit subjects from HEAD.
fn git_recent_commits(
    project_root: &std::path::Path,
    count: usize,
) -> std::result::Result<Vec<String>, String> {
    let repo = gix::open(project_root).map_err(|e| format!("failed to open git repo: {e}"))?;

    let mut commits = Vec::new();
    let head = repo
        .head()
        .map_err(|e| format!("cannot read HEAD: {e}"))?
        .into_peeled_id()
        .map_err(|e| format!("cannot peel HEAD: {e}"))?;

    let mut current_id = head.detach();

    for _ in 0..count {
        let commit = repo
            .find_object(current_id)
            .map_err(|e| format!("cannot find object: {e}"))?
            .try_into_commit()
            .map_err(|e| format!("not a commit: {e}"))?;

        let message = commit
            .message_raw()
            .map_err(|e| format!("cannot read commit message: {e}"))?;
        let subject = String::from_utf8_lossy(message.as_ref())
            .lines()
            .next()
            .unwrap_or("")
            .to_string();
        commits.push(subject);

        let parent_id = commit.parent_ids().next().map(gix::Id::detach);
        match parent_id {
            Some(pid) => current_id = pid,
            None => break,
        }
    }

    Ok(commits)
}

/// Returns commit subjects between two refs.
fn git_commit_log(
    project_root: &std::path::Path,
    base_ref: &str,
    head_ref: &str,
) -> std::result::Result<Vec<Value>, String> {
    let repo = gix::open(project_root).map_err(|e| format!("failed to open git repo: {e}"))?;

    let base_id = repo
        .rev_parse_single(base_ref)
        .map_err(|e| format!("cannot resolve '{base_ref}': {e}"))?
        .object()
        .map_err(|e| format!("cannot read object for '{base_ref}': {e}"))?
        .peel_to_commit()
        .map_err(|e| format!("cannot peel '{base_ref}' to commit: {e}"))?
        .id;

    let head_id = repo
        .rev_parse_single(head_ref)
        .map_err(|e| format!("cannot resolve '{head_ref}': {e}"))?
        .object()
        .map_err(|e| format!("cannot read object for '{head_ref}': {e}"))?
        .peel_to_commit()
        .map_err(|e| format!("cannot peel '{head_ref}' to commit: {e}"))?
        .id;

    let mut commits = Vec::new();
    let walk = repo
        .rev_walk([head_id])
        .with_hidden([base_id])
        .all()
        .map_err(|e| format!("cannot walk commits from '{base_ref}' to '{head_ref}': {e}"))?;

    // Include commits reachable from head but not base, including merge-shaped
    // histories where the merge base is not on the first-parent chain.
    for info in walk.take(100) {
        let info = info.map_err(|e| format!("cannot walk commit: {e}"))?;
        let commit = repo
            .find_object(info.id)
            .map_err(|e| format!("cannot find object: {e}"))?
            .try_into_commit()
            .map_err(|e| format!("not a commit: {e}"))?;

        let message = commit
            .message_raw()
            .map_err(|e| format!("cannot read message: {e}"))?;
        let subject = String::from_utf8_lossy(message.as_ref())
            .lines()
            .next()
            .unwrap_or("")
            .to_string();
        let short_id = format!("{:.7}", commit.id);
        commits.push(json!({"hash": short_id, "subject": subject}));
    }

    Ok(commits)
}

/// Classify a file path into a semantic role.
///
/// Inline tests inside source files don't make the file's role "test" —
/// that bucket is reserved for files that exist purely to host tests
/// (the path-based check). A `src/foo.rs` with a `#[cfg(test)] mod tests`
/// at the bottom still has role "source".
#[allow(clippy::ptr_arg)]
fn classify_file_role(path: &str, _files_with_inline_tests: &HashSet<String>) -> &'static str {
    if crate::tracedecay::is_test_file(path) {
        return "test";
    }
    let lower = path.to_lowercase();
    let ext = std::path::Path::new(&lower)
        .extension()
        .and_then(|e| e.to_str());
    // Config files
    if matches!(
        ext,
        Some("toml" | "yaml" | "yml" | "json" | "lock" | "ini" | "cfg")
    ) || lower.contains("config")
    {
        return "config";
    }
    // Documentation
    if matches!(ext, Some("md" | "rst" | "txt"))
        || lower.starts_with("docs/")
        || lower.starts_with("doc/")
    {
        return "docs";
    }
    "source"
}

/// Handles `tracedecay_changelog` tool calls.
pub(super) async fn handle_changelog(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    debug_assert!(
        args.is_object(),
        "handle_changelog expects an object argument"
    );
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

    // Use gix to diff the two trees
    let changes = match git_diff_file_changes(cg.project_root(), from_ref, to_ref) {
        Ok(files) => files,
        Err(e) => {
            return Ok(git_error_result(cg, &args, "diff", &e));
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

    let text = render::finalize(Some(cg.project_root()), &args, &result, || {
        render::generic_md(&result)
    });
    Ok(ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        touched_files,
    ))
}

/// Handles `tracedecay_commit_context` tool calls.
pub(super) async fn handle_commit_context(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let staged_only = args
        .get("staged_only")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let changed_files = match git_changed_files(cg.project_root(), staged_only) {
        Ok(files) => files,
        Err(e) => {
            return Ok(git_error_result(cg, &args, "status", &e));
        }
    };

    if changed_files.is_empty() {
        let output = json!({
            "changed_files": [],
            "symbols_by_role": {},
            "suggested_category": Value::Null,
            "recent_commits": git_recent_commits(cg.project_root(), 5).unwrap_or_default(),
            "summary": "No changes detected.",
        });
        let text = render::finalize(Some(cg.project_root()), &args, &output, || {
            render::generic_md(&output)
        });
        return Ok(ToolResult::new(
            json!({"content": [{"type": "text", "text": text}]}),
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

    let recent_commits = git_recent_commits(cg.project_root(), 5).unwrap_or_default();

    let total_symbols: usize = symbols_by_role.values().map(std::vec::Vec::len).sum();
    let output = json!({
        "changed_files": file_roles,
        "symbols_by_role": symbols_by_role,
        "suggested_category": category,
        "recent_commits": recent_commits,
        "summary": format!("{} file(s) changed, {} symbol(s) affected", changed_files.len(), total_symbols),
    });

    let text = render::finalize(Some(cg.project_root()), &args, &output, || {
        render::generic_md(&output)
    });
    Ok(ToolResult::new(
        json!({"content": [{"type": "text", "text": text}]}),
        changed_files,
    ))
}

/// Handles `tracedecay_pr_context` tool calls.
pub(super) async fn handle_pr_context(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let base = args
        .get("base_ref")
        .and_then(|v| v.as_str())
        .map_or_else(|| default_pr_base_ref(cg.project_root()), str::to_owned);
    let head = args
        .get("head_ref")
        .and_then(|v| v.as_str())
        .unwrap_or("HEAD");

    let comparison = match git_pr_comparison(cg.project_root(), &base, head) {
        Ok(comparison) => comparison,
        Err(e) => {
            return Ok(git_error_result(cg, &args, "diff", &e));
        }
    };
    let GitPrComparison {
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

    let text = render::finalize(Some(cg.project_root()), &args, &output, || {
        render::generic_md(&output)
    });
    Ok(ToolResult::new(
        json!({"content": [{"type": "text", "text": text}]}),
        changed_files,
    ))
}

// ── Cross-branch tools ─────────────────────────────────────────────────

/// Daemon-only branch-add entry point used by the first-party CLI.
///
/// Branch preparation copies and syncs a graph database, so it must run inside
/// the managed daemon's database-authority scope rather than in the CLI process.
pub(super) async fn handle_admin_branch_add(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let branch = require_admin_branch_name(&args)?;
    let outcome =
        TraceDecay::add_branch_tracking_with_options(cg.project_root(), branch, cg.open_options())
            .await?;
    let output = json!({ "outcome": admin_branch_add_outcome_name(&outcome) });
    Ok(ToolResult::new(
        json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string(&output).unwrap_or_default(),
            }]
        }),
        vec![],
    ))
}

fn require_admin_branch_name(args: &Value) -> Result<&str> {
    args.get("branch")
        .and_then(Value::as_str)
        .filter(|branch| !branch.is_empty())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: branch".to_string(),
        })
}

fn admin_branch_add_outcome_name(outcome: &crate::branch::BranchAddOutcome) -> &'static str {
    match outcome {
        crate::branch::BranchAddOutcome::NotIndexed => "not_indexed",
        crate::branch::BranchAddOutcome::AlreadyTracked => "already_tracked",
        crate::branch::BranchAddOutcome::Added => "added",
        crate::branch::BranchAddOutcome::Deferred => "deferred",
    }
}

/// Handles `tracedecay_branch_list` tool calls.
pub(super) fn handle_branch_list(cg: &TraceDecay, args: &Value) -> ToolResult {
    let diagnostics = cg.branch_diagnostics();
    let mut result = serde_json::to_value(&diagnostics).unwrap_or(json!({}));
    if let Some(object) = result.as_object_mut() {
        object.insert(
            "branch_count".to_string(),
            json!(diagnostics.tracked_branch_count),
        );
    }

    let text = render::finalize(Some(cg.project_root()), args, &result, || {
        render::generic_md(&result)
    });
    ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        vec![],
    )
}

/// Handles `tracedecay_branch_search` tool calls.
pub(super) async fn handle_branch_search(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let branch =
        args.get("branch")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TraceDecayError::Config {
                message: "missing required parameter: branch".to_string(),
            })?;
    let query =
        args.get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TraceDecayError::Config {
                message: "missing required parameter: query".to_string(),
            })?;
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(10, |v| v.min(500) as usize);

    let branch_cg = TraceDecay::open_branch_with_registered_configuration(
        cg.project_root(),
        branch,
        crate::tracedecay::TraceDecayOpenOptions::default(),
        cg.store_layout().clone(),
        cg.configuration_runtime().registered_database(),
        cg.profile_database().clone(),
        cg.store_runtime_registry().clone(),
    )
    .await?;
    let results = branch_cg.search(query, limit).await?;

    let items: Vec<Value> = results
        .iter()
        .map(|r| {
            json!({
                "id": r.node.id,
                "name": r.node.name,
                "kind": r.node.kind.as_str(),
                "file": r.node.file_path,
                "line": r.node.start_line,
                "signature": r.node.signature,
                "score": r.score,
                "branch": branch,
            })
        })
        .collect();

    let items = json!(items);
    let text = render::finalize(Some(cg.project_root()), &args, &items, || {
        render::generic_md(&items)
    });
    Ok(ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        vec![],
    ))
}

/// Handles `tracedecay_branch_diff` tool calls.
///
/// Compares code graphs between two branches. For each symbol present in
/// either branch, reports whether it was added, removed, or changed
/// (signature differs).
pub(super) async fn handle_branch_diff(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let project_root = cg.project_root();
    let tracedecay_dir = &cg.store_layout().data_root;

    // Resolve base and head branches
    let meta = crate::branch_meta::load_branch_meta(tracedecay_dir).ok_or_else(|| {
        TraceDecayError::Config {
            message: "no branch tracking configured — run `tracedecay branch add` first"
                .to_string(),
        }
    })?;

    let base_name = args
        .get("base")
        .and_then(|v| v.as_str())
        .unwrap_or(&meta.default_branch);
    let head_name = args
        .get("head")
        .and_then(|v| v.as_str())
        .or_else(|| cg.active_branch())
        .ok_or_else(|| TraceDecayError::Config {
            message: "cannot determine head branch — specify it explicitly".to_string(),
        })?;

    if base_name == head_name {
        // pr_context returns empty arrays for the same-ref case; do the same here
        // so callers get a consistent shape and can simply check the summary.
        let result = json!({
            "base": base_name,
            "head": head_name,
            "note": format!("base and head are the same branch: '{base_name}'"),
            "summary": { "added": 0, "removed": 0, "changed": 0 },
            "added": [],
            "removed": [],
            "changed": [],
        });
        let text = render::finalize(Some(cg.project_root()), &args, &result, || {
            render::generic_md(&result)
        });
        return Ok(ToolResult::new(
            json!({
                "content": [{ "type": "text", "text": text }]
            }),
            vec![],
        ));
    }

    let file_filter = args.get("file").and_then(|v| v.as_str());
    let kind_filter = args.get("kind").and_then(|v| v.as_str());

    let base_cg = TraceDecay::open_branch_with_registered_configuration(
        project_root,
        base_name,
        crate::tracedecay::TraceDecayOpenOptions::default(),
        cg.store_layout().clone(),
        cg.configuration_runtime().registered_database(),
        cg.profile_database().clone(),
        cg.store_runtime_registry().clone(),
    )
    .await?;
    let head_cg = if cg.active_branch() == Some(head_name) && !cg.is_fallback() {
        None // use the already-open cg
    } else {
        Some(
            TraceDecay::open_branch_with_registered_configuration(
                project_root,
                head_name,
                crate::tracedecay::TraceDecayOpenOptions::default(),
                cg.store_layout().clone(),
                cg.configuration_runtime().registered_database(),
                cg.profile_database().clone(),
                cg.store_runtime_registry().clone(),
            )
            .await?,
        )
    };
    let head_ref = head_cg.as_ref().unwrap_or(cg);

    let base_files = base_cg.get_all_files().await?;
    let head_files = head_ref.get_all_files().await?;

    // Build file sets for filtering — only compare files present in either branch
    let base_file_set: HashSet<&str> = base_files.iter().map(|f| f.path.as_str()).collect();
    let head_file_set: HashSet<&str> = head_files.iter().map(|f| f.path.as_str()).collect();
    let all_files: HashSet<&str> = base_file_set.union(&head_file_set).copied().collect();

    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    let mut touched = Vec::new();

    for file_path in &all_files {
        if let Some(filter) = file_filter
            && !file_path.starts_with(filter)
            && *file_path != filter
        {
            continue;
        }

        let base_nodes = base_cg.get_nodes_by_file(file_path).await?;
        let head_nodes = head_ref.get_nodes_by_file(file_path).await?;

        // Index by qualified_name for matching
        let base_map: HashMap<&str, &crate::types::Node> = base_nodes
            .iter()
            .map(|n| (n.qualified_name.as_str(), n))
            .collect();
        let head_map: HashMap<&str, &crate::types::Node> = head_nodes
            .iter()
            .map(|n| (n.qualified_name.as_str(), n))
            .collect();

        for (qn, node) in &head_map {
            if let Some(filter) = kind_filter
                && node.kind.as_str() != filter
            {
                continue;
            }
            if !base_map.contains_key(qn) {
                added.push(json!({
                    "name": node.name,
                    "qualified_name": node.qualified_name,
                    "kind": node.kind.as_str(),
                    "file": node.file_path,
                    "line": node.start_line,
                    "signature": node.signature,
                }));
                touched.push(node.file_path.clone());
            }
        }

        for (qn, node) in &base_map {
            if let Some(filter) = kind_filter
                && node.kind.as_str() != filter
            {
                continue;
            }
            if !head_map.contains_key(qn) {
                removed.push(json!({
                    "name": node.name,
                    "qualified_name": node.qualified_name,
                    "kind": node.kind.as_str(),
                    "file": node.file_path,
                    "line": node.start_line,
                    "signature": node.signature,
                }));
                touched.push(node.file_path.clone());
            }
        }

        // Changed: in both but signature differs
        for (qn, head_node) in &head_map {
            if let Some(filter) = kind_filter
                && head_node.kind.as_str() != filter
            {
                continue;
            }
            if let Some(base_node) = base_map.get(qn)
                && base_node.signature != head_node.signature
            {
                changed.push(json!({
                    "name": head_node.name,
                    "qualified_name": head_node.qualified_name,
                    "kind": head_node.kind.as_str(),
                    "file": head_node.file_path,
                    "line": head_node.start_line,
                    "base_signature": base_node.signature,
                    "head_signature": head_node.signature,
                }));
                touched.push(head_node.file_path.clone());
            }
        }
    }

    let result = json!({
        "base": base_name,
        "head": head_name,
        "summary": {
            "added": added.len(),
            "removed": removed.len(),
            "changed": changed.len(),
        },
        "added": added,
        "removed": removed,
        "changed": changed,
    });

    let touched_files = unique_file_paths(touched.iter().map(std::string::String::as_str));
    let text = render::finalize(Some(cg.project_root()), &args, &result, || {
        render::generic_md(&result)
    });
    Ok(ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        touched_files,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeAffectedTestDependents {
        dependents: HashMap<String, Vec<String>>,
        frontiers: std::sync::Mutex<Vec<Vec<String>>>,
    }

    impl AffectedTestDependents for FakeAffectedTestDependents {
        fn get_file_dependents_batch<'a>(
            &'a self,
            files: &'a [String],
        ) -> AffectedDependentsFuture<'a> {
            Box::pin(async move {
                self.frontiers.lock().unwrap().push(files.to_vec());
                Ok(files
                    .iter()
                    .map(|file| {
                        (
                            file.clone(),
                            self.dependents.get(file).cloned().unwrap_or_default(),
                        )
                    })
                    .collect())
            })
        }
    }

    fn fake_affected_test_dependents(reverse: bool) -> FakeAffectedTestDependents {
        let mut root = vec![
            "tests/direct_test.rs".to_string(),
            "src/b.rs".to_string(),
            "src/a.rs".to_string(),
        ];
        let mut a = vec!["tests/near_test.rs".to_string(), "src/leaf.rs".to_string()];
        let mut b = vec!["src/root.rs".to_string(), "tests/near_test.rs".to_string()];
        if reverse {
            root.reverse();
            a.reverse();
            b.reverse();
        }
        FakeAffectedTestDependents {
            dependents: HashMap::from([
                ("src/root.rs".to_string(), root),
                ("src/a.rs".to_string(), a),
                ("src/b.rs".to_string(), b),
                (
                    "src/leaf.rs".to_string(),
                    vec!["tests/transitive_test.rs".to_string()],
                ),
            ]),
            frontiers: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn serial_affected_test_set(
        source: &FakeAffectedTestDependents,
        files: &[String],
        max_depth: usize,
    ) -> HashSet<String> {
        let mut affected = HashSet::new();
        let mut visited = HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        for file in files {
            if crate::tracedecay::is_test_file(file) {
                affected.insert(file.clone());
            }
            if visited.insert(file.clone()) {
                queue.push_back((file.clone(), 0));
            }
        }
        while let Some((file, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            for dependent in source.dependents.get(&file).into_iter().flatten() {
                if !visited.insert(dependent.clone()) {
                    continue;
                }
                if crate::tracedecay::is_test_file(dependent) {
                    affected.insert(dependent.clone());
                } else {
                    queue.push_back((dependent.clone(), depth + 1));
                }
            }
        }
        affected
    }

    #[tokio::test]
    async fn affected_traversal_batches_one_database_read_per_frontier() {
        let source = fake_affected_test_dependents(false);
        let traversal = collect_affected_test_files(
            &source,
            &["src/root.rs".to_string()],
            5,
            None,
            &HashSet::new(),
        )
        .await
        .unwrap();

        assert_eq!(
            *source.frontiers.lock().unwrap(),
            vec![
                vec!["src/root.rs".to_string()],
                vec!["src/a.rs".to_string(), "src/b.rs".to_string()],
                vec!["src/leaf.rs".to_string()],
            ]
        );
        assert_eq!(source.frontiers.lock().unwrap().len(), 3);
        assert_eq!(traversal.test_distances.len(), 3);
    }

    #[tokio::test]
    async fn affected_traversal_preserves_set_parity_and_ranks_deterministically() {
        let expected_set = HashSet::from([
            "tests/changed_test.rs".to_string(),
            "tests/direct_test.rs".to_string(),
            "tests/near_test.rs".to_string(),
            "tests/transitive_test.rs".to_string(),
        ]);
        let mut ranked_runs = Vec::new();

        for reverse in [false, true] {
            let source = fake_affected_test_dependents(reverse);
            let files = [
                "tests/changed_test.rs".to_string(),
                "src/root.rs".to_string(),
            ];
            let serial_set = serial_affected_test_set(&source, &files, 5);
            let traversal = collect_affected_test_files(&source, &files, 5, None, &HashSet::new())
                .await
                .unwrap();
            let batched_set = traversal
                .test_distances
                .keys()
                .cloned()
                .collect::<HashSet<_>>();
            assert_eq!(serial_set, expected_set);
            assert_eq!(batched_set, serial_set);
            ranked_runs.push(rank_affected_tests(&traversal.test_distances));
        }

        assert_eq!(ranked_runs[0], ranked_runs[1]);
        assert_eq!(
            ranked_runs[0],
            vec![
                RankedAffectedTest {
                    path: "tests/changed_test.rs".to_string(),
                    distance: 0,
                },
                RankedAffectedTest {
                    path: "tests/direct_test.rs".to_string(),
                    distance: 1,
                },
                RankedAffectedTest {
                    path: "tests/near_test.rs".to_string(),
                    distance: 2,
                },
                RankedAffectedTest {
                    path: "tests/transitive_test.rs".to_string(),
                    distance: 3,
                },
            ]
        );
    }

    fn test_git(root: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "TraceDecay Test")
            .env("GIT_AUTHOR_EMAIL", "test@tracedecay.invalid")
            .env("GIT_COMMITTER_NAME", "TraceDecay Test")
            .env("GIT_COMMITTER_EMAIL", "test@tracedecay.invalid")
            .output()
            .expect("git command should run");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn admin_branch_add_requires_a_nonempty_branch_name() {
        for args in [serde_json::json!({}), serde_json::json!({ "branch": "" })] {
            let error = require_admin_branch_name(&args)
                .expect_err("branch add request without branch must fail");
            assert!(error.to_string().contains("branch"));
        }
    }

    #[test]
    fn admin_branch_add_outcomes_have_stable_wire_names() {
        assert_eq!(
            admin_branch_add_outcome_name(&crate::branch::BranchAddOutcome::NotIndexed),
            "not_indexed"
        );
        assert_eq!(
            admin_branch_add_outcome_name(&crate::branch::BranchAddOutcome::AlreadyTracked),
            "already_tracked"
        );
        assert_eq!(
            admin_branch_add_outcome_name(&crate::branch::BranchAddOutcome::Added),
            "added"
        );
        assert_eq!(
            admin_branch_add_outcome_name(&crate::branch::BranchAddOutcome::Deferred),
            "deferred"
        );
    }

    #[test]
    fn pr_comparison_anchors_at_merge_base_when_base_advanced() {
        let temp = tempfile::tempdir().expect("temp repo");
        let root = temp.path();
        test_git(root, &["init", "-b", "main"]);
        std::fs::write(root.join("common.txt"), "common\n").expect("write common");
        test_git(root, &["add", "."]);
        test_git(root, &["commit", "-m", "common"]);
        test_git(root, &["switch", "-c", "feature"]);
        std::fs::write(root.join("feature.txt"), "feature\n").expect("write feature");
        test_git(root, &["add", "."]);
        test_git(root, &["commit", "-m", "feature"]);
        test_git(root, &["switch", "main"]);
        std::fs::write(root.join("main-only.txt"), "main\n").expect("write main");
        test_git(root, &["add", "."]);
        test_git(root, &["commit", "-m", "main advanced"]);

        let comparison = git_pr_comparison(root, "main", "feature").expect("PR comparison");
        let paths: Vec<_> = comparison
            .changes
            .iter()
            .map(|change| change.path.as_str())
            .collect();

        assert_eq!(paths, ["feature.txt"]);
        assert_eq!(comparison.commits.len(), 1);
        assert_eq!(comparison.commits[0]["subject"], "feature");
    }

    #[test]
    fn pr_context_default_base_detects_master() {
        let temp = tempfile::tempdir().expect("temp repo");
        test_git(temp.path(), &["init", "-b", "master"]);
        std::fs::write(temp.path().join("README.md"), "test\n").expect("write fixture");
        test_git(temp.path(), &["add", "."]);
        test_git(temp.path(), &["commit", "-m", "initial"]);
        assert_eq!(default_pr_base_ref(temp.path()), "master");
    }

    #[test]
    fn config_files_classified_as_config_not_source() {
        let empty: HashSet<String> = HashSet::new();
        assert_eq!(classify_file_role("Cargo.toml", &empty), "config");
        assert_eq!(classify_file_role("package.json", &empty), "config");
        assert_eq!(classify_file_role("foo.yaml", &empty), "config");
        assert_eq!(classify_file_role("config.ini", &empty), "config");
    }

    /// Regression for bug #3 follow-up: a source file with `#[cfg(test)] mod
    /// tests` at the bottom is still a source file — its role must not flip
    /// to "test" just because it contains inline tests. Only the path-based
    /// `is_test_file` check governs role classification.
    #[test]
    fn source_file_with_inline_tests_keeps_source_role() {
        let mut with_inline: HashSet<String> = HashSet::new();
        with_inline.insert("src/lib.rs".to_string());
        assert_eq!(classify_file_role("src/lib.rs", &with_inline), "source");
    }

    #[test]
    fn path_based_test_files_classify_as_test() {
        let empty: HashSet<String> = HashSet::new();
        assert_eq!(classify_file_role("tests/integration.rs", &empty), "test");
        assert_eq!(classify_file_role("src/foo_test.rs", &empty), "test");
    }
}
