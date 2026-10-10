//! `tracedecay serve` lists a core tool set with full schemas, stubs the rest,
//! hydrates a stub on call, and still reaches every catalog tool.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::process::{Output, Stdio};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;

use crate::common::{self, TestChildProcess, tracedecay_command_with_home};
use crate::serve_harness::{init_project_with_file, json_rpc_response};

const LIST_CHANGED: &str = "notifications/tools/list_changed";
/// One tools/call per stub, each forwarded to the daemon.
const SERVE_TIMEOUT: Duration = Duration::from_secs(120);

fn run_serve(home: &Path, project: &Path, requests: &[Value]) -> Output {
    let mut child = TestChildProcess::new(
        tracedecay_command_with_home(home)
            .arg("serve")
            .arg("--path")
            .arg(project)
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

fn tools_of(stdout: &[u8], id: i64) -> Vec<Value> {
    json_rpc_response(stdout, id)["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list {id} carried no tool array"))
        .clone()
}

fn is_stub(tool: &Value) -> bool {
    tool["inputSchema"] == json!({"type": "object", "additionalProperties": true})
        && tool["description"].as_str().is_some_and(|description| {
            description.contains("full schema was elided")
                && description.contains("call it by name")
        })
}

fn tool_named<'a>(tools: &'a [Value], name: &str) -> &'a Value {
    tools
        .iter()
        .find(|tool| tool["name"] == name)
        .unwrap_or_else(|| panic!("missing tool {name} in {tools:?}"))
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

#[tokio::test]
async fn serve_lists_core_tools_and_reaches_every_catalog_tool() {
    let home = TempDir::new().unwrap();
    let project = init_project_with_file(home.path(), "pub fn tool_surface_marker() {}\n").await;
    let _daemon = common::spawn_tracedecay_daemon(home.path());

    let first = run_serve(home.path(), project.path(), &[initialize(), tools_list(2)]);
    assert!(first.status.success(), "{first:?}");
    let after = tools_list_cost(&first.stdout, 2);
    let catalog = after.names.clone();
    assert!(
        catalog.contains("tracedecay_impact") && catalog.contains("tracedecay_runtime"),
        "default serve must keep every catalog name: {catalog:?}"
    );
    let first_tools = tools_of(&first.stdout, 2);
    assert!(
        is_stub(tool_named(&first_tools, "tracedecay_impact")),
        "pruned tools must be stubs: {}",
        tool_named(&first_tools, "tracedecay_impact")
    );
    assert!(
        !is_stub(tool_named(&first_tools, "tracedecay_grep")),
        "core tools must keep their full schema: {}",
        tool_named(&first_tools, "tracedecay_grep")
    );
    let stubbed_names = first_tools
        .iter()
        .filter(|tool| is_stub(tool))
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    let stubbed = stubbed_names.len();
    let full = first_tools.len() - stubbed;
    assert!(
        stubbed > 100 && full * 8 < first_tools.len(),
        "most listed tools must be stubs ({full} full, {stubbed} stubbed, {} total)",
        first_tools.len()
    );

    let mut requests = vec![
        initialize(),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        tools_list(2),
        tool_call(3, "tracedecay_runtime", &json!({ "format": "json" })),
        tools_list(5),
    ];
    let load_base = 1_000;
    for (offset, name) in (0_i64..).zip(&stubbed_names) {
        requests.push(tool_call(load_base + offset, name, &json!({})));
    }
    requests.push(tools_list(6));
    let output = run_serve(home.path(), project.path(), &requests);
    assert!(output.status.success(), "{output:?}");

    let initialize = json_rpc_response(&output.stdout, 1);
    assert!(
        initialize["result"]["instructions"]
            .as_str()
            .is_some_and(|instructions| instructions.contains("Call a stub by name")),
        "initialize must tell the host how to hydrate stubs: {initialize}"
    );

    let runtime = json_rpc_response(&output.stdout, 3);
    assert!(
        runtime.get("error").is_none() && runtime["result"]["isError"] != json!(true),
        "a stub must answer tools/call by name: {runtime}"
    );
    let hydrated = tools_of(&output.stdout, 5);
    assert!(
        !is_stub(tool_named(&hydrated, "tracedecay_runtime")),
        "calling a stub must hydrate its full schema: {}",
        tool_named(&hydrated, "tracedecay_runtime")
    );
    assert!(
        is_stub(tool_named(&hydrated, "tracedecay_impact")),
        "uncalled stubs must stay stubs: {}",
        tool_named(&hydrated, "tracedecay_impact")
    );

    let reached = tools_of(&output.stdout, 6);
    let still_stubbed = catalog
        .iter()
        .filter(|name| is_stub(tool_named(&reached, name)))
        .collect::<Vec<_>>();
    assert!(
        still_stubbed.is_empty(),
        "calling every stub must hydrate every catalog tool; still stubbed {still_stubbed:?}"
    );
    assert!(
        list_changed_count(&output.stdout) > 1,
        "each hydrate must announce notifications/tools/list_changed"
    );

    let before = tools_list_cost(&output.stdout, 6);
    assert!(
        before.tokens > 50_000,
        "the fully hydrated list must still carry the expensive catalog ({} tools, {} bytes, {} tokens)",
        before.names.len(),
        before.bytes,
        before.tokens
    );
    assert!(
        after.tokens * 2 < before.tokens,
        "default stubbed tools/list ({after_tools} tools, {after_bytes} bytes, {after_tokens} tokens) must be cheaper than the hydrated catalog ({before_tools} tools, {before_bytes} bytes, {before_tokens} tokens)",
        after_tools = after.names.len(),
        after_bytes = after.bytes,
        after_tokens = after.tokens,
        before_tools = before.names.len(),
        before_bytes = before.bytes,
        before_tokens = before.tokens
    );
    eprintln!(
        "tools/list handshake cost: before (hydrated catalog) {} tools / {} bytes / {} tokens; after (default stubs) {} tools / {} bytes / {} tokens ({} full, {} stubbed)",
        before.names.len(),
        before.bytes,
        before.tokens,
        after.names.len(),
        after.bytes,
        after.tokens,
        full,
        stubbed
    );
    let stderr = String::from_utf8_lossy(&first.stderr);
    assert!(
        stderr.contains("mcp_tool_list_roster") && stderr.contains("sha256:"),
        "each served list must log a hash of the sorted tool names: {stderr}"
    );
}
