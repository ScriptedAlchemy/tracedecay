//! Compiler / test workflow handlers: `diagnose`, `run_affected_tests`.
//!
//! Bridges raw toolchain output (`cargo check`, `cargo clippy`, `cargo test`)
//! to the code graph, so an agent receives diagnostics and test results
//! already attached to the symbols they affect.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Output;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::process::Command;
use tokio::time::timeout;

use crate::diagnose::{parse_cargo_output, Severity};
use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::{is_test_file, TraceDecay};
use crate::types::{Node, NodeKind};

use super::super::render;
use super::super::ToolResult;
use super::support::unique_file_paths;

/// Maximum tests we'll allow `cargo test` to receive in one call. A loose
/// cap — libtest filters are passed as positional args so very long lists
/// can blow past OS argv limits on some platforms.
const MAX_TESTS_HARD_CAP: usize = 500;

#[derive(Debug, Clone)]
struct TestTarget {
    filter: String,
    qualified_name: String,
    node_id: String,
    covers_source_ids: Vec<String>,
}

impl TestTarget {
    fn new(node: &Node) -> Self {
        Self {
            filter: node.name.clone(),
            qualified_name: node.qualified_name.clone(),
            node_id: node.id.clone(),
            covers_source_ids: Vec::new(),
        }
    }

    fn add_source(&mut self, source_id: &str) {
        if !self.covers_source_ids.iter().any(|id| id == source_id) {
            self.covers_source_ids.push(source_id.to_string());
        }
    }

    fn matches_libtest_name(&self, name: &str) -> bool {
        name == self.filter
            || name.rsplit("::").next() == Some(self.filter.as_str())
            || (!self.qualified_name.is_empty() && name == self.qualified_name)
            || name == self.node_id
    }
}

fn test_target_key(node: &Node) -> String {
    if node.qualified_name.is_empty() {
        node.id.clone()
    } else {
        node.qualified_name.clone()
    }
}

#[derive(Debug)]
struct RunAffectedArgs {
    explicit_paths: Option<Vec<String>>,
    profile: String,
    timeout_secs: u64,
    max_tests: usize,
}

impl RunAffectedArgs {
    fn parse(args: &Value) -> Self {
        let explicit_paths = args.get("changed_paths").and_then(|v| {
            v.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
        });
        let profile = args
            .get("profile")
            .and_then(|v| v.as_str())
            .unwrap_or("debug")
            .to_string();
        let timeout_secs = args
            .get("timeout_secs")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(300);
        let max_tests = args
            .get("max_tests")
            .and_then(serde_json::Value::as_u64)
            .map_or(100_usize, |v| (v as usize).min(MAX_TESTS_HARD_CAP));

        Self {
            explicit_paths,
            profile,
            timeout_secs,
            max_tests,
        }
    }
}

/// Handles `tracedecay_diagnose`.
pub(super) async fn handle_diagnose(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let cargo_output =
        args.get("cargo_output")
            .and_then(|v| v.as_str())
            .ok_or(TraceDecayError::Config {
                message: "missing required parameter: cargo_output".to_string(),
            })?;

    let severity_filter = args
        .get("severity")
        .and_then(|v| v.as_str())
        .unwrap_or("all");
    let include_callers = args
        .get("include_callers")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let max_diagnostics = args
        .get("max_diagnostics")
        .and_then(serde_json::Value::as_u64)
        .map_or(50_usize, |v| v.min(500) as usize);

    let mut diagnostics: Vec<_> = parse_cargo_output(cargo_output)
        .into_iter()
        .filter(|d| match severity_filter {
            "error" => d.severity == Severity::Error,
            "warning" => d.severity == Severity::Warning,
            _ => true,
        })
        .collect();
    let total = diagnostics.len();
    diagnostics.truncate(max_diagnostics);

    let mut items: Vec<Value> = Vec::with_capacity(diagnostics.len());
    let mut touched: HashSet<String> = HashSet::new();

    for d in &diagnostics {
        touched.insert(d.file.clone());

        let node = cg.node_at_location(&d.file, d.line).await?;
        let callers_json = if include_callers {
            match &node {
                Some(n) => {
                    let callers = cg.get_callers(&n.id, 1).await?;
                    let trimmed: Vec<Value> = callers
                        .into_iter()
                        .take(5)
                        .map(|(caller, _)| {
                            touched.insert(caller.file_path.clone());
                            json!({
                                "node_id": caller.id,
                                "name": caller.name,
                                "kind": caller.kind.as_str(),
                                "file": caller.file_path,
                                "line": caller.start_line,
                            })
                        })
                        .collect();
                    Value::Array(trimmed)
                }
                None => Value::Array(vec![]),
            }
        } else {
            Value::Null
        };

        items.push(json!({
            "severity": severity_string(d.severity),
            "code": d.code,
            "message": d.message,
            "file": d.file,
            "line": d.line,
            "column": d.column,
            "node": node.as_ref().map(|n| json!({
                "node_id": n.id,
                "name": n.name,
                "kind": n.kind.as_str(),
                "qualified_name": n.qualified_name,
                "start_line": n.start_line,
                "end_line": n.end_line,
            })),
            "callers": callers_json,
        }));
    }

    let mapped = items.iter().filter(|i| !i["node"].is_null()).count();
    let body = json!({
        "diagnostics_parsed": total,
        "diagnostics_returned": items.len(),
        "mapped_to_node": mapped,
        "unmapped": items.len() - mapped,
        "truncated": total > items.len(),
        "diagnostics": items,
    });
    let text = render::finalize(Some(cg.project_root()), &args, &body, || {
        render::generic_md(&body)
    });
    Ok(ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        touched.into_iter().collect(),
    ))
}

fn severity_string(s: Severity) -> &'static str {
    match s {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Note => "note",
        Severity::Help => "help",
    }
}

/// Handles `tracedecay_run_affected_tests`.
pub(super) async fn handle_run_affected_tests(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let run_args = RunAffectedArgs::parse(&args);
    let project_root = cg.project_root().to_path_buf();

    // 1) Resolve changed paths — explicit list, or fall back to `git diff`.
    let changed_paths = match resolve_changed_paths(&project_root, run_args.explicit_paths).await {
        Ok(paths) => paths,
        Err(result) => return Ok(result),
    };
    if changed_paths.is_empty() {
        return Ok(empty_result("no changed files detected"));
    }

    let test_targets = collect_affected_test_targets(cg, &changed_paths).await?;

    if test_targets.is_empty() {
        return Ok(empty_result(&format!(
            "no tests cover the changed paths ({} file(s))",
            changed_paths.len()
        )));
    }

    let (selected_targets, test_names, truncated) =
        select_test_targets(test_targets, run_args.max_tests);

    // 3) Run cargo test --no-fail-fast with each test name as a libtest
    // filter. We use `--` to pass them through.
    let mut cmd = cargo_test_command(&project_root, &run_args.profile, &test_names);
    let run = cmd.output();
    let output = match timeout(Duration::from_secs(run_args.timeout_secs), run).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return Ok(error_result(
                "cargo",
                "test",
                &format!("failed to spawn cargo test: {e}"),
            ));
        }
        Err(_) => {
            return Ok(error_result(
                "cargo",
                "test",
                &format!("cargo test timed out after {}s", run_args.timeout_secs),
            ));
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let results = parse_libtest_output(&stdout);

    let touched_files: Vec<String> = unique_file_paths(changed_paths.iter().map(String::as_str));
    let body = run_affected_tests_body(
        &output,
        &results,
        &test_names,
        truncated,
        &selected_targets,
        &stderr,
        &stdout,
    );

    let text = render::finalize(Some(cg.project_root()), &args, &body, || {
        render::generic_md(&body)
    });
    Ok(ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        touched_files,
    ))
}

async fn resolve_changed_paths(
    project_root: &Path,
    explicit_paths: Option<Vec<String>>,
) -> std::result::Result<Vec<String>, ToolResult> {
    match explicit_paths {
        Some(paths) => Ok(paths),
        None => git_changed_paths(project_root)
            .await
            .map_err(|message| error_result("git", "diff", &message)),
    }
}

async fn collect_affected_test_targets(
    cg: &TraceDecay,
    changed_paths: &[String],
) -> Result<HashMap<String, TestTarget>> {
    // Two paths feed into the test set:
    // a) Indirect coverage: for each changed callable, walk callers and keep
    //    test-shaped ones.
    // b) Direct changes: when a changed path is itself a test file or contains
    //    `#[test]` functions, dispatch those tests directly.
    let mut test_targets = HashMap::new();
    for path in changed_paths {
        let nodes = cg.get_nodes_by_file(path).await?;
        add_direct_test_targets(cg, path, &nodes, &mut test_targets).await?;
        add_indirect_test_targets(cg, &nodes, &mut test_targets).await?;
    }
    Ok(test_targets)
}

async fn add_direct_test_targets(
    cg: &TraceDecay,
    path: &str,
    nodes: &[Node],
    test_targets: &mut HashMap<String, TestTarget>,
) -> Result<()> {
    let path_is_test_file = is_test_file(path);
    if !path_is_test_file && nodes.is_empty() {
        return Ok(());
    }

    let candidate_ids: Vec<String> = nodes
        .iter()
        .filter(|n| is_callable(n))
        .map(|n| n.id.clone())
        .collect();
    let test_annotated_in_file = cg.get_test_annotated_node_ids(&candidate_ids).await?;

    for node in nodes {
        if !is_callable(node) {
            continue;
        }
        if !path_is_test_file && !test_annotated_in_file.contains(&node.id) {
            continue;
        }
        // The test "covers itself" so the per-test `covers_source_ids` field
        // remains useful.
        test_targets
            .entry(test_target_key(node))
            .or_insert_with(|| TestTarget::new(node))
            .add_source(&node.id);
    }

    Ok(())
}

async fn add_indirect_test_targets(
    cg: &TraceDecay,
    nodes: &[Node],
    test_targets: &mut HashMap<String, TestTarget>,
) -> Result<()> {
    for node in nodes {
        if !is_callable(node) {
            continue;
        }

        let callers = cg.get_callers(&node.id, 3).await?;
        let caller_ids: Vec<String> = callers.iter().map(|(n, _)| n.id.clone()).collect();
        let test_annotated = cg.get_test_annotated_node_ids(&caller_ids).await?;

        for (caller, _) in callers {
            if !is_test_file(&caller.file_path) && !test_annotated.contains(&caller.id) {
                continue;
            }
            if !is_callable(&caller) {
                continue;
            }
            test_targets
                .entry(test_target_key(&caller))
                .or_insert_with(|| TestTarget::new(&caller))
                .add_source(&node.id);
        }
    }

    Ok(())
}

fn is_callable(node: &Node) -> bool {
    matches!(node.kind, NodeKind::Function | NodeKind::Method)
}

fn select_test_targets(
    test_targets: HashMap<String, TestTarget>,
    max_tests: usize,
) -> (Vec<TestTarget>, Vec<String>, bool) {
    let mut selected_targets: Vec<TestTarget> = test_targets.into_values().collect();
    selected_targets.sort_by(|a, b| {
        a.qualified_name
            .cmp(&b.qualified_name)
            .then(a.node_id.cmp(&b.node_id))
    });
    let total_tests = selected_targets.len();
    selected_targets.truncate(max_tests);
    let truncated = total_tests > selected_targets.len();

    let mut test_names: Vec<String> = selected_targets
        .iter()
        .map(|target| target.filter.clone())
        .collect();
    test_names.sort();
    test_names.dedup();

    (selected_targets, test_names, truncated)
}

fn cargo_test_command(project_root: &Path, profile: &str, test_names: &[String]) -> Command {
    let mut cmd = Command::new("cargo");
    cmd.current_dir(project_root)
        .arg("test")
        .arg("--no-fail-fast");
    if profile == "release" {
        cmd.arg("--release");
    }
    cmd.arg("--");
    for name in test_names {
        cmd.arg(name);
    }
    cmd.kill_on_drop(true);
    cmd
}

fn run_affected_tests_body(
    output: &Output,
    results: &[(String, bool)],
    test_names: &[String],
    truncated: bool,
    selected_targets: &[TestTarget],
    stderr: &str,
    stdout: &str,
) -> Value {
    let passed = results.iter().filter(|(_, ok)| *ok).count();
    let failed = results.iter().filter(|(_, ok)| !*ok).count();

    json!({
        "exit_code": output.status.code(),
        "passed": passed,
        "failed": failed,
        "total_observed": results.len(),
        "dispatched_tests": test_names,
        "truncated": truncated,
        "results": results
            .iter()
            .map(|(name, ok)| {
                json!({
                    "test": name,
                    "passed": ok,
                    "covers_source_ids": covered_source_ids(name, selected_targets),
                })
            })
            .collect::<Vec<_>>(),
        "stderr_tail": tail(stderr, 2000),
        "stdout_tail": tail(stdout, 2000),
    })
}

fn covered_source_ids(name: &str, selected_targets: &[TestTarget]) -> Vec<String> {
    let mut covers = Vec::new();
    for target in selected_targets {
        if target.matches_libtest_name(name) {
            for source_id in &target.covers_source_ids {
                if !covers.contains(source_id) {
                    covers.push(source_id.clone());
                }
            }
        }
    }
    covers
}

/// Wraps a short status message in a normal `ToolResult`.
fn empty_result(message: &str) -> ToolResult {
    ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": serde_json::to_string(&json!({
                "passed": 0, "failed": 0, "results": [], "note": message
            })).unwrap_or_default() }]
        }),
        vec![],
    )
}

fn error_result(kind: &str, operation: &str, message: &str) -> ToolResult {
    ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": serde_json::to_string(&json!({
                "passed": 0,
                "failed": 0,
                "results": [],
                "error": {
                    "kind": kind,
                    "operation": operation,
                    "message": message,
                }
            })).unwrap_or_default() }]
        }),
        vec![],
    )
}

/// Returns the last `n` characters of `s`, trimmed to a char boundary.
fn tail(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut start = s.len() - n;
    while !s.is_char_boundary(start) && start < s.len() {
        start += 1;
    }
    s[start..].to_string()
}

/// Returns files changed in the working tree relative to HEAD (`git diff
/// --name-only HEAD`).
async fn git_changed_paths(
    project_root: &std::path::Path,
) -> std::result::Result<Vec<String>, String> {
    let output = Command::new("git")
        .args(["diff", "--name-only", "HEAD"])
        .current_dir(project_root)
        .output()
        .await
        .map_err(|e| format!("failed to spawn git diff: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git diff failed: {}", stderr.trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// Parses libtest stdout for `test <name> ... ok` / `... FAILED` lines.
/// Returns `(test_name, passed)` pairs. Robust to colour codes by trimming
/// the common ANSI reset prefix.
fn parse_libtest_output(stdout: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    for raw in stdout.lines() {
        let line = raw.trim_start_matches("\u{1b}[0m").trim();
        let Some(rest) = line.strip_prefix("test ") else {
            continue;
        };
        // Skip the "running N tests" / summary lines, which start with "test " followed
        // by something other than a test name (e.g. "test result:").
        if rest.starts_with("result:") {
            continue;
        }
        let Some((name, status)) = rest.rsplit_once(" ... ") else {
            continue;
        };
        let status = status.trim();
        let passed = match status {
            "ok" => true,
            "FAILED" | "failed" => false,
            // Skip "ignored", "bench", incomplete lines, etc.
            _ => continue,
        };
        out.push((name.trim().to_string(), passed));
    }
    out
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_libtest_pass_and_fail() {
        let stdout = "\
running 3 tests
test foo ... ok
test bar ... FAILED
test baz ... ignored
test result: FAILED. 1 passed; 1 failed; 1 ignored
";
        let results = parse_libtest_output(stdout);
        assert_eq!(results, vec![("foo".into(), true), ("bar".into(), false)]);
    }

    #[test]
    fn tail_handles_short_input() {
        assert_eq!(tail("hello", 100), "hello");
        assert_eq!(tail("0123456789", 4), "6789");
    }
}
