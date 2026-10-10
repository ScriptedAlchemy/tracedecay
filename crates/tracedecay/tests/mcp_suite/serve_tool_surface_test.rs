//! `tracedecay serve` lists a core tool set, loads every other catalog tool
//! into the session's list on demand, and forwards calls to tools it does not
//! list.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::process::{Output, Stdio};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;

use crate::common::{self, TestChildProcess, tracedecay_command_with_home};
use crate::serve_harness::{init_project_with_file, json_rpc_response};

const TOOL_SEARCH: &str = "tracedecay_tool_search";
const LIST_CHANGED: &str = "notifications/tools/list_changed";
/// One tool search per catalog tool, each re-reading the daemon's catalog.
const SERVE_TIMEOUT: Duration = Duration::from_secs(120);

fn run_serve(home: &Path, project: &Path, extra_args: &[&str], requests: &[Value]) -> Output {
    let mut child = TestChildProcess::new(
        tracedecay_command_with_home(home)
            .arg("serve")
            .arg("--path")
            .arg(project)
            .args(extra_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("tracedecay serve should start"),
    );
    {
        let stdin = child.stdin_mut().expect("stdin should be piped");
        for request in requests {
            let _ = writeln!(stdin, "{request}");
        }
    }
    child
        .wait_with_output(SERVE_TIMEOUT)
        .expect("tracedecay serve should exit after stdin closes")
}

fn initialize() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "tool-surface-test", "version": "1" }
        }
    })
}

fn tools_list(id: i64) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": "tools/list" })
}

fn tool_call(id: i64, name: &str, arguments: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments }
    })
}

fn listed_names(stdout: &[u8], id: i64) -> BTreeSet<String> {
    tools_list_cost(stdout, id).names
}

/// Compact-JSON size of one `tools/list` result, with the MCP token budget
/// estimator (`json_bytes.div_ceil(4)`).
struct ToolsListCost {
    names: BTreeSet<String>,
    bytes: usize,
    tokens: usize,
}

fn tools_list_cost(stdout: &[u8], id: i64) -> ToolsListCost {
    let response = json_rpc_response(stdout, id);
    let result = &response["result"];
    let compact = serde_json::to_string(result)
        .unwrap_or_else(|error| panic!("tools/list {id} result must serialize: {error}"));
    let names = result["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list {id} carried no tool array: {response}"))
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect();
    ToolsListCost {
        names,
        bytes: compact.len(),
        tokens: compact.len().div_ceil(4),
    }
}

fn list_changed_count(stdout: &[u8]) -> usize {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|message| message["method"] == LIST_CHANGED)
        .count()
}

/// The session catalog `serve --all-tools` lists for `project`.
fn full_catalog_cost(home: &Path, project: &Path) -> ToolsListCost {
    let output = run_serve(
        home,
        project,
        &["--all-tools"],
        &[initialize(), tools_list(2)],
    );
    assert!(output.status.success(), "{output:?}");
    let cost = tools_list_cost(&output.stdout, 2);
    assert!(!cost.names.contains(TOOL_SEARCH), "{:?}", cost.names);
    cost
}

#[tokio::test]
async fn serve_lists_core_tools_and_reaches_every_catalog_tool() {
    let home = TempDir::new().unwrap();
    let project = init_project_with_file(home.path(), "pub fn tool_surface_marker() {}\n").await;
    let _daemon = common::spawn_tracedecay_daemon(home.path());
    let before = full_catalog_cost(home.path(), project.path());
    let catalog = before.names.clone();
    assert!(
        catalog.contains("tracedecay_impact") && catalog.contains("tracedecay_runtime"),
        "--all-tools must list the full session catalog: {catalog:?}"
    );

    let mut requests = vec![
        initialize(),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        tools_list(2),
        // A tool the session does not list still answers by exact name.
        tool_call(3, "tracedecay_runtime", &json!({ "format": "json" })),
        tool_call(4, TOOL_SEARCH, &json!({ "query": "blast radius impact" })),
        tools_list(5),
    ];
    let load_base = 1_000;
    for (offset, name) in (0_i64..).zip(&catalog) {
        requests.push(tool_call(
            load_base + offset,
            TOOL_SEARCH,
            &json!({ "query": name }),
        ));
    }
    requests.push(tools_list(6));
    let output = run_serve(home.path(), project.path(), &[], &requests);
    assert!(output.status.success(), "{output:?}");

    let initialize = json_rpc_response(&output.stdout, 1);
    assert!(
        initialize["result"]["instructions"]
            .as_str()
            .is_some_and(|instructions| instructions.contains(TOOL_SEARCH)),
        "initialize must tell the host how to load more tools: {initialize}"
    );

    let after = tools_list_cost(&output.stdout, 2);
    let core = after.names.clone();
    assert!(core.contains(TOOL_SEARCH), "{core:?}");
    assert!(
        core.contains("tracedecay_grep") && core.contains("tracedecay_source_body"),
        "{core:?}"
    );
    assert!(!core.contains("tracedecay_impact"), "{core:?}");
    assert!(
        core.len() * 8 < catalog.len(),
        "the core list ({}) must be a small slice of the catalog ({})",
        core.len(),
        catalog.len()
    );
    assert!(
        before.tokens > 50_000,
        "the unpruned --all-tools handshake must still carry the expensive catalog ({} tools, {} bytes, {} tokens)",
        before.names.len(),
        before.bytes,
        before.tokens
    );
    assert!(
        after.tokens * 8 < before.tokens,
        "default tools/list ({after_tools} tools, {after_bytes} bytes, {after_tokens} tokens) must be a small slice of --all-tools ({before_tools} tools, {before_bytes} bytes, {before_tokens} tokens)",
        after_tools = after.names.len(),
        after_bytes = after.bytes,
        after_tokens = after.tokens,
        before_tools = before.names.len(),
        before_bytes = before.bytes,
        before_tokens = before.tokens
    );
    eprintln!(
        "tools/list handshake cost: before (--all-tools) {} tools / {} bytes / {} tokens; after (default) {} tools / {} bytes / {} tokens",
        before.names.len(),
        before.bytes,
        before.tokens,
        after.names.len(),
        after.bytes,
        after.tokens
    );

    let runtime = json_rpc_response(&output.stdout, 3);
    assert!(
        runtime.get("error").is_none() && runtime["result"]["isError"] != json!(true),
        "an unlisted tool must answer tools/call by name: {runtime}"
    );

    let search = json_rpc_response(&output.stdout, 4);
    assert_eq!(search["result"]["isError"], json!(false), "{search}");
    let loaded = listed_names(&output.stdout, 5);
    assert!(loaded.contains("tracedecay_impact"), "{loaded:?}");
    assert!(loaded.is_superset(&core), "{loaded:?}");
    assert!(loaded.len() <= core.len() + 8, "{loaded:?}");

    let reached = listed_names(&output.stdout, 6);
    let missing = catalog.difference(&reached).collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "an exact-name tool search must load every catalog tool; missing {missing:?}"
    );
    assert!(
        list_changed_count(&output.stdout) > 1,
        "each load must announce notifications/tools/list_changed"
    );
}
