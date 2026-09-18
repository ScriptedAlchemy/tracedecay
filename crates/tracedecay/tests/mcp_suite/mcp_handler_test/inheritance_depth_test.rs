//! Production MCP proof for `tracedecay_inheritance_depth`.
//!
//! Every call is a JSON-RPC `tools/call` on the daemon composition, the same
//! path an agent uses. Occurrence ids are generation-local, so expectations
//! pin the fields an operator can read from source: name, kind, file, line,
//! and depth. Ranking order is pinned only when depth makes it unique.

#![cfg(feature = "test-transport")]

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay_mcp::JsonRpcResponse;

use crate::common::fixture::git_run;
use crate::support::test_temp_dir;

const HIERARCHY: &str = "\
pub trait Left {}
pub trait Right: Left {}
pub trait Join: Right {}
pub trait Apex: Join {}
pub fn ignored() -> u8 { 7 }
";

const SIDE: &str = "\
pub trait Outer {}
pub trait Inner: Outer {}
";

/// `Peak` extends both a root and that root's child. Depth is the longest
/// chain (2), not the sum of the two parents and not the first bound alone.
const WIDE: &str = "\
pub trait Shallow {}
pub trait Deep: Shallow {}
pub trait Peak: Shallow + Deep {}
";

const LONG_CHAIN: &str = "\
pub trait T000 {}
pub trait T001: T000 {}
pub trait T002: T001 {}
pub trait T003: T002 {}
pub trait T004: T003 {}
pub trait T005: T004 {}
pub trait T006: T005 {}
pub trait T007: T006 {}
pub trait T008: T007 {}
pub trait T009: T008 {}
pub trait T010: T009 {}
pub trait T011: T010 {}
pub fn outside_the_chain() -> u8 { 0 }
";

const CYCLE: &str = "\
pub trait Alpha: Beta {}
pub trait Beta: Alpha {}
";

struct OpenedProject {
    harness: ProductionProjectCompositionHarnessV1,
    root: PathBuf,
    _dir: crate::support::TestTempDir,
}

async fn open_project(files: &[(&str, &str)]) -> OpenedProject {
    let dir = test_temp_dir();
    let root = dir.path().join("project");
    for (relative, contents) in files {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("fixture directory");
        }
        fs::write(&path, contents).expect("fixture source");
    }
    git_run(&root, &["init", "--quiet"]);
    git_run(&root, &["add", "."]);
    git_run(
        &root,
        &[
            "-c",
            "user.name=TraceDecay Tests",
            "-c",
            "user.email=tests@tracedecay.invalid",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ],
    );
    let harness = ProductionProjectCompositionHarnessV1::open(dir.path(), [root.clone()])
        .await
        .expect("production MCP composition");
    let project = OpenedProject {
        harness,
        root,
        _dir: dir,
    };
    wait_for_graph(&project).await;
    project
}

async fn wait_for_graph(project: &OpenedProject) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let response = call_tool(
                project,
                "tracedecay_status",
                json!({
                    "format": "json",
                    "include_branch_diagnostics": false,
                    "include_storage_health": false,
                    "include_session_ingest": false,
                    "include_staleness": false,
                }),
            )
            .await;
            let status = json_body(&response);
            let freshness = &status["code_index_freshness"];
            let serving = &freshness["worktree"]["code_graph_serving"];
            match (
                freshness["status"].as_str(),
                serving["state"].as_str(),
                serving["reason"].as_str(),
                freshness["worktree"]["staleness_state"].as_str(),
            ) {
                (Some("current"), Some("ready"), _, _) => break,
                (Some("warming"), _, _, _)
                | (Some("stale"), Some("ready"), _, Some("verifying"))
                | (_, Some("pending"), _, _)
                | (_, Some("unavailable"), Some("generation_unavailable"), _) => {
                    tokio::task::yield_now().await;
                }
                other => panic!("graph readiness became {other:?}: {status}"),
            }
        }
    })
    .await
    .expect("graph did not become current");
}

async fn call_tool(project: &OpenedProject, tool: &str, arguments: Value) -> JsonRpcResponse {
    project
        .harness
        .call_tool(&project.root, tool, arguments)
        .await
        .unwrap_or_else(|error| panic!("{tool} MCP call failed: {error}"))
}

async fn call_inheritance_depth(project: &OpenedProject, arguments: Value) -> JsonRpcResponse {
    call_tool(project, "tracedecay_inheritance_depth", arguments).await
}

fn tool_text<'a>(response: &'a JsonRpcResponse) -> &'a str {
    assert!(response.error.is_none(), "MCP error: {:?}", response.error);
    let result = response
        .result
        .as_ref()
        .unwrap_or_else(|| panic!("missing MCP result: {response:?}"));
    result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tool text missing: {result}"))
}

fn json_body(response: &JsonRpcResponse) -> Value {
    let text = tool_text(response);
    serde_json::from_str(text).unwrap_or_else(|error| panic!("tool JSON ({error}): {text}"))
}

fn without_ids(payload: &Value) -> Value {
    let mut payload = payload.clone();
    if let Some(items) = payload.get_mut("ranking").and_then(Value::as_array_mut) {
        for item in items {
            if let Some(object) = item.as_object_mut() {
                object.remove("id");
            }
        }
    }
    payload
}

/// Ids are not stable across fixture homes, but the ranking the caller sees
/// is still ordered by depth descending, then id ascending.
fn assert_occurrence_order(payload: &Value) {
    let ranking = payload["ranking"]
        .as_array()
        .unwrap_or_else(|| panic!("ranking missing: {payload}"));
    let mut seen = HashSet::new();
    let mut previous: Option<(u64, &str)> = None;
    for item in ranking {
        let id = item["id"]
            .as_str()
            .unwrap_or_else(|| panic!("ranking row has no id: {item}"));
        assert!(
            id.starts_with("symbol.v1."),
            "occurrence id is not the symbol identity: {item}"
        );
        assert!(seen.insert(id), "duplicate id {id}");
        let depth = item["depth"]
            .as_u64()
            .unwrap_or_else(|| panic!("depth missing: {item}"));
        if let Some((previous_depth, previous_id)) = previous {
            assert!(
                depth < previous_depth || (depth == previous_depth && id >= previous_id),
                "ranking is not depth-desc, id-asc: {previous_depth}/{previous_id} then {depth}/{id}"
            );
        }
        previous = Some((depth, id));
    }
}

fn expect_ranking(payload: &Value, expected: Value) {
    assert_occurrence_order(payload);
    assert_eq!(without_ids(payload), expected, "full payload: {payload}");
}

fn indexed(payload: &Value) -> BTreeMap<&str, (&str, &str, u64, u64)> {
    payload["ranking"]
        .as_array()
        .unwrap_or_else(|| panic!("ranking missing: {payload}"))
        .iter()
        .map(|item| {
            (
                item["name"].as_str().unwrap_or(""),
                (
                    item["kind"].as_str().unwrap_or(""),
                    item["file"].as_str().unwrap_or(""),
                    item["line"].as_u64().unwrap_or(u64::MAX),
                    item["depth"].as_u64().unwrap_or(u64::MAX),
                ),
            )
        })
        .collect()
}

fn strip_id_lines(text: &str) -> String {
    let mut rendered = String::new();
    for line in text.lines() {
        if line.trim_start().starts_with("**id:**") {
            continue;
        }
        rendered.push_str(line);
        rendered.push('\n');
    }
    rendered
}

#[tokio::test]
async fn inheritance_depth_ranks_literal_extends_depths() {
    let mut project = open_project(&[
        (
            "src/lib.rs",
            "pub mod hierarchy;\npub mod side;\npub mod wide;\n",
        ),
        ("src/hierarchy.rs", HIERARCHY),
        ("src/side.rs", SIDE),
        ("src/wide.rs", WIDE),
    ])
    .await;

    let all = json_body(&call_inheritance_depth(&project, json!({"format": "json"})).await);
    assert_eq!(all["result_count"], 9);
    assert_eq!(
        indexed(&all),
        [
            ("Apex", ("trait", "src/hierarchy.rs", 4, 3)),
            ("Join", ("trait", "src/hierarchy.rs", 3, 2)),
            ("Right", ("trait", "src/hierarchy.rs", 2, 1)),
            ("Left", ("trait", "src/hierarchy.rs", 1, 0)),
            ("Inner", ("trait", "src/side.rs", 2, 1)),
            ("Outer", ("trait", "src/side.rs", 1, 0)),
            ("Peak", ("trait", "src/wide.rs", 3, 2)),
            ("Deep", ("trait", "src/wide.rs", 2, 1)),
            ("Shallow", ("trait", "src/wide.rs", 1, 0)),
        ]
        .into_iter()
        .collect()
    );
    assert_occurrence_order(&all);

    expect_ranking(
        &json_body(
            &call_inheritance_depth(
                &project,
                json!({"format": "json", "path": "src/hierarchy.rs"}),
            )
            .await,
        ),
        json!({
            "result_count": 4,
            "ranking": [
                {"name": "Apex", "kind": "trait", "file": "src/hierarchy.rs", "line": 4, "depth": 3},
                {"name": "Join", "kind": "trait", "file": "src/hierarchy.rs", "line": 3, "depth": 2},
                {"name": "Right", "kind": "trait", "file": "src/hierarchy.rs", "line": 2, "depth": 1},
                {"name": "Left", "kind": "trait", "file": "src/hierarchy.rs", "line": 1, "depth": 0}
            ]
        }),
    );
    expect_ranking(
        &json_body(
            &call_inheritance_depth(&project, json!({"format": "json", "path": "src/wide.rs"}))
                .await,
        ),
        json!({
            "result_count": 3,
            "ranking": [
                {"name": "Peak", "kind": "trait", "file": "src/wide.rs", "line": 3, "depth": 2},
                {"name": "Deep", "kind": "trait", "file": "src/wide.rs", "line": 2, "depth": 1},
                {"name": "Shallow", "kind": "trait", "file": "src/wide.rs", "line": 1, "depth": 0}
            ]
        }),
    );

    let missing = json_body(
        &call_inheritance_depth(&project, json!({"format": "json", "path": "src/hierarchy"})).await,
    );
    assert_eq!(missing, json!({"result_count": 0, "ranking": []}));

    expect_ranking(
        &json_body(
            &call_inheritance_depth(
                &project,
                json!({"format": "json", "path": "src/hierarchy.rs", "limit": 1}),
            )
            .await,
        ),
        json!({
            "result_count": 1,
            "ranking": [
                {"name": "Apex", "kind": "trait", "file": "src/hierarchy.rs", "line": 4, "depth": 3}
            ]
        }),
    );
    expect_ranking(
        &json_body(
            &call_inheritance_depth(
                &project,
                json!({"format": "json", "path": "src/hierarchy.rs", "limit": 0}),
            )
            .await,
        ),
        json!({"result_count": 0, "ranking": []}),
    );

    let markdown =
        tool_text(&call_inheritance_depth(&project, json!({"path": "src/hierarchy.rs"})).await);
    assert_eq!(
        strip_id_lines(markdown),
        "\
**result_count:** 4

## ranking
**kind:** trait
**file:** src/hierarchy.rs

- **Apex**
  **line:** 4
  **depth:** 3
- **Join**
  **line:** 3
  **depth:** 2
- **Right**
  **line:** 2
  **depth:** 1
- **Left**
  **line:** 1
  **depth:** 0
"
    );

    project.harness.shutdown().await;
}

#[tokio::test]
async fn inheritance_depth_default_limit_keeps_ten_deepest() {
    let mut project = open_project(&[
        ("src/lib.rs", "pub mod long;\n"),
        ("src/long.rs", LONG_CHAIN),
    ])
    .await;

    expect_ranking(
        &json_body(&call_inheritance_depth(&project, json!({"format": "json"})).await),
        json!({
            "result_count": 10,
            "ranking": [
                {"name": "T011", "kind": "trait", "file": "src/long.rs", "line": 12, "depth": 11},
                {"name": "T010", "kind": "trait", "file": "src/long.rs", "line": 11, "depth": 10},
                {"name": "T009", "kind": "trait", "file": "src/long.rs", "line": 10, "depth": 9},
                {"name": "T008", "kind": "trait", "file": "src/long.rs", "line": 9, "depth": 8},
                {"name": "T007", "kind": "trait", "file": "src/long.rs", "line": 8, "depth": 7},
                {"name": "T006", "kind": "trait", "file": "src/long.rs", "line": 7, "depth": 6},
                {"name": "T005", "kind": "trait", "file": "src/long.rs", "line": 6, "depth": 5},
                {"name": "T004", "kind": "trait", "file": "src/long.rs", "line": 5, "depth": 4},
                {"name": "T003", "kind": "trait", "file": "src/long.rs", "line": 4, "depth": 3},
                {"name": "T002", "kind": "trait", "file": "src/long.rs", "line": 3, "depth": 2}
            ]
        }),
    );
    expect_ranking(
        &json_body(&call_inheritance_depth(&project, json!({"format": "json", "limit": 12})).await),
        json!({
            "result_count": 12,
            "ranking": [
                {"name": "T011", "kind": "trait", "file": "src/long.rs", "line": 12, "depth": 11},
                {"name": "T010", "kind": "trait", "file": "src/long.rs", "line": 11, "depth": 10},
                {"name": "T009", "kind": "trait", "file": "src/long.rs", "line": 10, "depth": 9},
                {"name": "T008", "kind": "trait", "file": "src/long.rs", "line": 9, "depth": 8},
                {"name": "T007", "kind": "trait", "file": "src/long.rs", "line": 8, "depth": 7},
                {"name": "T006", "kind": "trait", "file": "src/long.rs", "line": 7, "depth": 6},
                {"name": "T005", "kind": "trait", "file": "src/long.rs", "line": 6, "depth": 5},
                {"name": "T004", "kind": "trait", "file": "src/long.rs", "line": 5, "depth": 4},
                {"name": "T003", "kind": "trait", "file": "src/long.rs", "line": 4, "depth": 3},
                {"name": "T002", "kind": "trait", "file": "src/long.rs", "line": 3, "depth": 2},
                {"name": "T001", "kind": "trait", "file": "src/long.rs", "line": 2, "depth": 1},
                {"name": "T000", "kind": "trait", "file": "src/long.rs", "line": 1, "depth": 0}
            ]
        }),
    );

    project.harness.shutdown().await;
}

#[tokio::test]
async fn inheritance_depth_cycle_is_unavailable() {
    let mut project =
        open_project(&[("src/lib.rs", "pub mod cycle;\n"), ("src/cycle.rs", CYCLE)]).await;

    let response = call_inheritance_depth(&project, json!({"format": "json"})).await;
    let error = response
        .error
        .as_ref()
        .unwrap_or_else(|| panic!("cycle ranked as success: {:?}", response.result));
    assert_eq!(error.code, -32602);
    assert_eq!(
        error.message,
        "tool project route failed: reason_code=verified-inheritance-depth-unavailable retryable=false: the admitted extends relation contains a cycle"
    );
    assert_eq!(
        error.data,
        Some(json!({
            "tool": "tracedecay_inheritance_depth",
            "reason_code": "verified-inheritance-depth-unavailable",
            "retryable": false,
            "detail": "the admitted extends relation contains a cycle"
        }))
    );
    assert!(response.result.is_none());

    project.harness.shutdown().await;
}
