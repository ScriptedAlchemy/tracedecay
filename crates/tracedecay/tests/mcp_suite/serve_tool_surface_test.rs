//! `tracedecay serve` lists a core tool set plus `tracedecay_tool_search`,
//! loads other catalog tools on demand, and still reaches every catalog tool.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{ChildStdout, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_tokenizer::count_ordinary_tokens;

use crate::common::{self, TestChildProcess, tracedecay_command_with_home};
use crate::serve_harness::{init_project_with_file, json_rpc_response};

const TOOL_SEARCH: &str = "tracedecay_tool_search";
const LIST_CHANGED: &str = "notifications/tools/list_changed";
/// Stubs (option A) measured on the same catalog, tokenizer, and real
/// `tracedecay serve` handshake at 8dcdcecd63: 229 names, 17 full schemas,
/// 212 stubs, 138_280 compact-JSON bytes, 30_664 o200k tokens.
const STUB_HANDSHAKE_O200K: u64 = 30_664;
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

fn tool_named<'a>(tools: &'a [Value], name: &str) -> &'a Value {
    tools
        .iter()
        .find(|tool| tool["name"] == name)
        .unwrap_or_else(|| panic!("missing tool {name} in {tools:?}"))
}

fn listed_names(stdout: &[u8], id: i64) -> BTreeSet<String> {
    tools_of(stdout, id)
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect()
}

fn tool_text(stdout: &[u8], id: i64) -> String {
    let response = json_rpc_response(stdout, id);
    response["result"]["content"]
        .as_array()
        .and_then(|content| content.first())
        .and_then(|block| block["text"].as_str())
        .unwrap_or_else(|| panic!("tools/call {id} carried no text: {response}"))
        .to_owned()
}

fn catalog_names_from_search_text(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|token| token.starts_with("tracedecay_") && *token != TOOL_SEARCH)
        .map(str::to_owned)
        .collect()
}

/// Compact-JSON size of one `tools/list` result, counted with the shipped
/// `o200k_base` tokenizer (`tracedecay_tokenizer::count_ordinary_tokens`).
struct ToolsListCost {
    names: BTreeSet<String>,
    bytes: usize,
    tokens: u64,
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
        tokens: count_ordinary_tokens(&compact)
            .unwrap_or_else(|error| panic!("o200k_base must count tools/list {id}: {error}")),
    }
}

fn list_changed_count(stdout: &[u8]) -> usize {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|message| message["method"] == LIST_CHANGED)
        .count()
}

/// One live `tracedecay serve` session driven as a generic MCP stdio client.
///
/// Writes one JSON-RPC line, then reads lines until the expected response or
/// notification arrives, the same way a host that is not Claude Code would.
struct PlainMcpClient {
    process: TestChildProcess,
    stdout: BufReader<ChildStdout>,
}

impl PlainMcpClient {
    fn start(home: &Path, project: &Path) -> Self {
        let mut child = tracedecay_command_with_home(home)
            .arg("serve")
            .arg("--path")
            .arg(project)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("tracedecay serve should start");
        let stdout = BufReader::new(child.stdout.take().expect("stdout should be piped"));
        Self {
            process: TestChildProcess::new(child),
            stdout,
        }
    }

    fn send(&mut self, request: &Value) {
        let stdin = self
            .process
            .stdin_mut()
            .expect("stdin should stay open for the session");
        writeln!(stdin, "{request}").expect("write MCP request");
        stdin.flush().expect("flush MCP request");
    }

    fn read_message(&mut self) -> Value {
        let deadline = Instant::now() + SERVE_TIMEOUT;
        loop {
            let mut line = String::new();
            match self.stdout.read_line(&mut line) {
                Ok(0) => panic!("serve closed stdout before answering the next MCP frame"),
                Ok(_) => {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    return serde_json::from_str(line)
                        .unwrap_or_else(|error| panic!("invalid MCP frame {line:?}: {error}"));
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::Interrupted
                        && Instant::now() < deadline =>
                {
                    continue;
                }
                Err(error) => panic!("failed to read MCP frame: {error}"),
            }
        }
    }

    fn wait_until(&mut self, mut done: impl FnMut(&[Value]) -> bool) -> Vec<Value> {
        let mut frames = Vec::new();
        while !done(&frames) {
            frames.push(self.read_message());
        }
        frames
    }

    fn request(&mut self, request: Value) -> Value {
        let id = request["id"].clone();
        self.send(&request);
        let frames = self.wait_until(|frames| frames.iter().any(|frame| frame["id"] == id));
        frames
            .into_iter()
            .find(|frame| frame["id"] == id)
            .expect("response id")
    }
}

#[tokio::test]
async fn serve_lists_core_tools_and_reaches_every_catalog_tool() {
    let home = TempDir::new().unwrap();
    let project = init_project_with_file(home.path(), "pub fn tool_surface_marker() {}\n").await;
    let _daemon = common::spawn_tracedecay_daemon(home.path());

    let first = run_serve(home.path(), project.path(), &[initialize(), tools_list(2)]);
    assert!(first.status.success(), "{first:?}");
    let handshake = tools_list_cost(&first.stdout, 2);
    assert!(
        handshake.names.contains(TOOL_SEARCH)
            && handshake.names.contains("tracedecay_grep")
            && handshake.names.contains("tracedecay_status"),
        "default serve must list the core set plus tool search: {:?}",
        handshake.names
    );
    assert!(
        !handshake.names.contains("tracedecay_impact")
            && !handshake.names.contains("tracedecay_runtime"),
        "default serve must withhold non-core tools: {:?}",
        handshake.names
    );
    assert!(
        handshake.names.len() <= 20,
        "core plus search must stay a tiny list: {:?}",
        handshake.names
    );
    assert!(
        handshake.tokens * 2 < STUB_HANDSHAKE_O200K,
        "B/C handshake ({} tools, {} bytes, {} o200k) must beat stub option A ({} o200k)",
        handshake.names.len(),
        handshake.bytes,
        handshake.tokens,
        STUB_HANDSHAKE_O200K
    );

    eprintln!(
        "tools/list handshake cost: A stubs {} o200k (229 names, 17 full); \
         B/C core+search {} tools / {} bytes / {} o200k (C handshake equals B; \
         plugin skills are host context, not tools/list)",
        STUB_HANDSHAKE_O200K,
        handshake.names.len(),
        handshake.bytes,
        handshake.tokens
    );

    let output = run_serve(
        home.path(),
        project.path(),
        &[
            initialize(),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            tools_list(2),
            tool_call(3, "tracedecay_runtime", &json!({ "format": "json" })),
            tool_call(4, TOOL_SEARCH, &json!({ "query": "blast radius impact" })),
            tools_list(5),
            tool_call(6, TOOL_SEARCH, &json!({ "query": "" })),
            tool_call(7, TOOL_SEARCH, &json!({ "query": "tracedecay_runtime" })),
            tools_list(8),
        ],
    );
    assert!(output.status.success(), "{output:?}");

    let initialize = json_rpc_response(&output.stdout, 1);
    assert!(
        initialize["result"]["instructions"]
            .as_str()
            .is_some_and(|instructions| instructions.contains(TOOL_SEARCH)),
        "initialize must tell the host how to load more tools: {initialize}"
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
    assert!(loaded.is_superset(&handshake.names), "{loaded:?}");
    assert!(loaded.len() <= handshake.names.len() + 8, "{loaded:?}");

    let catalog = catalog_names_from_search_text(&tool_text(&output.stdout, 6));
    assert!(
        catalog.contains("tracedecay_impact") && catalog.contains("tracedecay_runtime"),
        "empty tool search must name every remaining catalog tool: {catalog:?}"
    );
    assert!(
        catalog.len() > 100,
        "empty search must reach the rest of the catalog, not a handful: {}",
        catalog.len()
    );

    let reached = listed_names(&output.stdout, 8);
    assert!(
        reached.contains("tracedecay_runtime"),
        "exact-name search must load the named tool: {reached:?}"
    );
    assert!(
        list_changed_count(&output.stdout) >= 2,
        "each new load must announce notifications/tools/list_changed"
    );
    let stderr = String::from_utf8_lossy(&first.stderr);
    assert!(
        stderr.contains("mcp_tool_list_roster") && stderr.contains("sha256:"),
        "each served list must log a hash of the sorted tool names: {stderr}"
    );
}

/// A generic MCP stdio client that is not Claude Code searches for a
/// withheld tool, receives `notifications/tools/list_changed`, re-lists the
/// full schema, and calls it. That is the recovery path hosts other than the
/// Claude plugin need.
#[tokio::test]
async fn plain_mcp_stdio_client_finds_and_calls_a_non_core_tool() {
    let home = TempDir::new().unwrap();
    let project = init_project_with_file(home.path(), "pub fn plain_mcp_stdio_marker() {}\n").await;
    let _daemon = common::spawn_tracedecay_daemon(home.path());
    let mut client = PlainMcpClient::start(home.path(), project.path());

    let initialize = client.request(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": { "tools": { "listChanged": true } },
            "clientInfo": { "name": "plain-mcp-stdio", "version": "1" }
        }
    }));
    assert!(
        initialize.get("error").is_none(),
        "plain MCP initialize must succeed: {initialize}"
    );
    client.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));

    let listed = client.request(tools_list(2));
    let first_tools = listed["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list must carry a tool array: {listed}"));
    let names: BTreeSet<&str> = first_tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(
        names.contains(TOOL_SEARCH),
        "a non-Claude host must see tool search: {names:?}"
    );
    assert!(
        !names.contains("tracedecay_impact"),
        "a non-core tool must stay off the first list: {names:?}"
    );
    assert!(
        tool_named(first_tools, "tracedecay_grep")["inputSchema"]["properties"].is_object(),
        "core tools stay fully described: {}",
        tool_named(first_tools, "tracedecay_grep")
    );

    client.send(&tool_call(
        3,
        TOOL_SEARCH,
        &json!({ "query": "tracedecay_impact" }),
    ));
    let after_search = client.wait_until(|frames| {
        frames.iter().any(|frame| frame["id"] == 3)
            && frames.iter().any(|frame| frame["method"] == LIST_CHANGED)
    });
    let search = after_search
        .iter()
        .find(|frame| frame["id"] == 3)
        .expect("tool search answer");
    assert_eq!(search["result"]["isError"], json!(false), "{search}");
    assert!(
        after_search
            .iter()
            .any(|frame| frame["method"] == LIST_CHANGED),
        "the client must receive notifications/tools/list_changed before it re-lists: {after_search:?}"
    );

    let relisted = client.request(tools_list(4));
    let second_tools = relisted["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("second tools/list must carry a tool array: {relisted}"));
    let impact = tool_named(second_tools, "tracedecay_impact");
    assert!(
        impact["inputSchema"]["properties"].is_object(),
        "the next tools/list after list_changed must carry the full schema: {impact}"
    );

    let call = client.request(tool_call(5, "tracedecay_impact", &json!({})));
    assert!(
        call.get("error").is_none() || call["error"]["code"] != json!(-32601),
        "the loaded name must be callable; method-not-found means the host never registered it: {call}"
    );
}
