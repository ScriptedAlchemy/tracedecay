//! What one `tracedecay serve` session lists, what that handshake costs, and
//! that every tool stays callable from a plain MCP stdio client.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::process::{Output, Stdio};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_api::{WorkOperation, WorkflowOperation};
use tracedecay_tokenizer::count_ordinary_tokens;

use crate::common::{self, TestChildProcess, tracedecay_command_with_home};
use crate::serve_harness::{ServeSession, init_project_with_file, json_rpc_response, profile_root};

const LIST_CHANGED: &str = "notifications/tools/list_changed";
const ALWAYS_LOAD: &str = "anthropic/alwaysLoad";
const FULL_TOOL_LIST: &str = "--full-tool-list";
const COMPACT_PROFILE: &str = "--catalog-profile=compact";
const SERVE_TIMEOUT: Duration = Duration::from_secs(120);

fn serve_command(home: &Path, project: &Path, extra_args: &[&str]) -> std::process::Command {
    let mut command = tracedecay_command_with_home(home);
    command
        .arg("serve")
        .args(extra_args)
        .arg("--path")
        .arg(project)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn run_serve(home: &Path, project: &Path, extra_args: &[&str], requests: &[Value]) -> Output {
    let mut child = TestChildProcess::new(
        serve_command(home, project, extra_args)
            .spawn()
            .expect("tracedecay serve should start"),
    );
    {
        let stdin = child.stdin_mut().expect("stdin should be piped");
        for request in requests {
            writeln!(stdin, "{request}").expect("write MCP request");
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

fn tool_named<'a>(tools: &'a [Value], name: &str) -> &'a Value {
    tools
        .iter()
        .find(|tool| tool["name"] == name)
        .unwrap_or_else(|| panic!("missing tool {name} in {tools:?}"))
}

fn has_full_schema(tool: &Value) -> bool {
    tool["inputSchema"]["properties"].is_object()
}

fn always_loaded(tool: &Value) -> bool {
    tool["_meta"][ALWAYS_LOAD] == json!(true)
}

/// One `tools/list` result: its tools and the `o200k_base` token count of its
/// compact JSON, counted with the shipped tokenizer.
struct ServedList {
    tools: Vec<Value>,
    bytes: usize,
    tokens: u64,
}

impl ServedList {
    fn from_stdout(stdout: &[u8], id: i64) -> Self {
        let response = json_rpc_response(stdout, id);
        let result = &response["result"];
        let compact = serde_json::to_string(result).expect("tools/list result serializes");
        Self {
            tools: result["tools"]
                .as_array()
                .unwrap_or_else(|| panic!("tools/list {id} carried no tool array: {response}"))
                .clone(),
            bytes: compact.len(),
            tokens: count_ordinary_tokens(&compact)
                .unwrap_or_else(|error| panic!("o200k_base must count tools/list: {error}")),
        }
    }

    fn names(&self) -> BTreeSet<String> {
        self.tools
            .iter()
            .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
            .collect()
    }
}

fn plugin_agent_pins() -> BTreeSet<String> {
    // Relative to the crate directory both Cargo and the Bazel launcher run
    // from: a compile-time manifest path names the build's execroot instead.
    let agents = Path::new("../../plugin/agents");
    let mut pins = BTreeSet::new();
    for entry in std::fs::read_dir(agents)
        .unwrap_or_else(|error| panic!("read {}: {error}", agents.display()))
    {
        let path = entry.expect("agent entry").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let tools = raw
            .lines()
            .find_map(|line| line.strip_prefix("tools: "))
            .unwrap_or_else(|| panic!("{} is missing a tools: frontmatter line", path.display()));
        for entry in tools.split(',') {
            if let Some(name) = entry
                .trim()
                .strip_prefix("mcp__tracedecay__")
                .or_else(|| entry.trim().strip_prefix("mcp__plugin_tracedecay_graph__"))
            {
                pins.insert(name.to_owned());
            }
        }
    }
    assert!(
        !pins.is_empty(),
        "plugin/agents must pin at least one TraceDecay tool"
    );
    pins
}

#[tokio::test]
async fn default_serve_stubs_deferred_tools_and_keeps_every_name() {
    let home = TempDir::new().unwrap();
    let project = init_project_with_file(home.path(), "pub fn tool_surface_marker() {}\n").await;
    let _daemon = common::spawn_tracedecay_daemon(home.path());

    let stubbed = run_serve(
        home.path(),
        project.path(),
        &[],
        &[initialize(), tools_list(2)],
    );
    assert!(stubbed.status.success(), "{stubbed:?}");
    let full = run_serve(
        home.path(),
        project.path(),
        &[FULL_TOOL_LIST],
        &[initialize(), tools_list(2)],
    );
    assert!(full.status.success(), "{full:?}");
    let stubbed_list = ServedList::from_stdout(&stubbed.stdout, 2);
    let full_list = ServedList::from_stdout(&full.stdout, 2);

    assert_eq!(
        stubbed_list.names(),
        full_list.names(),
        "stubbing must keep every catalog name on the list"
    );
    let always: Vec<&Value> = full_list
        .tools
        .iter()
        .filter(|tool| always_loaded(tool))
        .collect();
    for name in [
        "tracedecay_grep",
        "tracedecay_retrieve",
        "tracedecay_status",
    ] {
        assert!(
            always_loaded(tool_named(&full_list.tools, name)),
            "{name} must be always-loaded in the catalog"
        );
    }
    for tool in &stubbed_list.tools {
        let name = tool["name"].as_str().expect("name");
        let full_tool = tool_named(&full_list.tools, name);
        if always_loaded(full_tool) {
            assert_eq!(
                tool, full_tool,
                "always-loaded {name} must keep its full definition"
            );
        } else {
            assert!(
                !has_full_schema(tool),
                "{name} is not always-loaded and must be a stub: {tool}"
            );
        }
    }
    assert!(
        has_full_schema(tool_named(&full_list.tools, "tracedecay_impact")),
        "--full-tool-list must keep every schema"
    );
    let missing: Vec<String> = plugin_agent_pins()
        .difference(&stubbed_list.names())
        .cloned()
        .collect();
    assert!(
        missing.is_empty(),
        "plugin agent pins must resolve: {missing:?}"
    );
    assert!(
        stubbed_list.tokens < full_list.tokens,
        "the stubbed handshake must cost less than the full list"
    );
    eprintln!(
        "tools/list handshake: default {} tools ({} full schemas) / {} bytes / {} o200k; \
         --full-tool-list {} tools / {} bytes / {} o200k",
        stubbed_list.tools.len(),
        always.len(),
        stubbed_list.bytes,
        stubbed_list.tokens,
        full_list.tools.len(),
        full_list.bytes,
        full_list.tokens,
    );

    let instructions = json_rpc_response(&stubbed.stdout, 1)["result"]["instructions"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        instructions.contains("Call a stub by name"),
        "initialize must say how to use a stub: {instructions}"
    );
    let stderr = String::from_utf8_lossy(&stubbed.stderr);
    assert!(
        stderr.contains("mcp_tool_list_roster") && stderr.contains("sha256:"),
        "each served list must log a hash of the sorted tool names: {stderr}"
    );
}

/// A plain MCP stdio client lists, calls a stub by name, receives
/// `notifications/tools/list_changed`, and re-lists the full schema.
#[tokio::test]
async fn plain_mcp_client_calls_a_stub_and_receives_its_schema() {
    let home = TempDir::new().unwrap();
    let project = init_project_with_file(home.path(), "pub fn plain_mcp_stdio_marker() {}\n").await;
    let _daemon = common::spawn_tracedecay_daemon(home.path());
    let mut client = ServeSession::start(serve_command(home.path(), project.path(), &[]));

    let initialized = client.request(initialize());
    assert!(initialized.get("error").is_none(), "{initialized}");
    client.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    client.await_project_ready();

    let first = client.request(tools_list(2));
    let runtime = tool_named(
        first["result"]["tools"].as_array().expect("tools"),
        "tracedecay_runtime",
    );
    assert!(
        !has_full_schema(runtime),
        "runtime starts as a stub: {runtime}"
    );

    client.send(&tool_call(
        3,
        "tracedecay_runtime",
        &json!({ "format": "json" }),
    ));
    let frames = client.wait_until(|frames| {
        frames.iter().any(|frame| frame["id"] == 3)
            && frames.iter().any(|frame| frame["method"] == LIST_CHANGED)
    });
    let answer = frames
        .iter()
        .find(|frame| frame["id"] == 3)
        .expect("call answer");
    assert!(
        answer.get("error").is_none() && answer["result"]["isError"] != json!(true),
        "a stub must answer tools/call by name: {answer}"
    );

    let second = client.request(tools_list(4));
    let runtime = tool_named(
        second["result"]["tools"].as_array().expect("tools"),
        "tracedecay_runtime",
    );
    assert!(
        has_full_schema(runtime),
        "the tools/list after list_changed must carry the full schema: {runtime}"
    );
    let impact = tool_named(
        second["result"]["tools"].as_array().expect("tools"),
        "tracedecay_impact",
    );
    assert!(
        !has_full_schema(impact),
        "uncalled stubs stay stubs: {impact}"
    );

    // A host that never re-lists only sees the stub, so the first rejected
    // guess must carry the schema the model needs to correct it.
    let guessed = client.request(tool_call(5, "tracedecay_impact", &json!({})));
    assert_eq!(guessed["result"]["isError"], json!(true), "{guessed}");
    let schema_text = guessed["result"]["content"]
        .as_array()
        .and_then(|content| {
            content.iter().find_map(|block| {
                block["text"]
                    .as_str()
                    .filter(|text| text.starts_with("Full input schema for tracedecay_impact: "))
            })
        })
        .unwrap_or_else(|| panic!("a rejected stub call must return its schema: {guessed}"));
    assert!(schema_text.contains(r#""node_id""#), "{schema_text}");
    let third = client.request(tools_list(6));
    assert!(has_full_schema(tool_named(
        third["result"]["tools"].as_array().expect("tools"),
        "tracedecay_impact",
    )));
}

/// Work and Workflow (handoff included) tool names from their operation
/// tables, independent of the catalog profile filter under test.
fn work_and_workflow_tool_names() -> BTreeSet<String> {
    let names: BTreeSet<String> = WorkOperation::ALL
        .into_iter()
        .map(|operation| format!("tracedecay_work_{}", operation.operation_key()))
        .chain(
            WorkflowOperation::ALL
                .into_iter()
                .map(|operation| format!("tracedecay_workflow_{}", operation.operation_key())),
        )
        .collect();
    assert!(
        names.contains("tracedecay_workflow_handoff_issue"),
        "handoff is a Workflow operation: {names:?}"
    );
    names
}

fn is_work_or_workflow_name(name: &str) -> bool {
    name.starts_with("tracedecay_work_") || name.starts_with("tracedecay_workflow_")
}

/// The profile's `config.toml` bytes and every path under its data directory.
fn profile_state(home: &Path) -> (Option<Vec<u8>>, BTreeSet<std::path::PathBuf>) {
    let root = profile_root(home);
    let config = std::fs::read(root.join("config.toml")).ok();
    let mut files = BTreeSet::new();
    let mut pending = vec![root.clone()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap_or_else(|error| {
            panic!("read {}: {error}", dir.display());
        }) {
            let path = entry.expect("profile entry").path();
            if path.is_dir() {
                pending.push(path.clone());
            }
            files.insert(path.strip_prefix(&root).expect("under root").to_path_buf());
        }
    }
    (config, files)
}

/// Lists past the readiness read, calls `tool` by name, and re-lists. Returns
/// the first list, the call answer, the `list_changed` frames seen from the
/// call through the re-list, and the re-list.
fn list_call_relist(
    mut session: ServeSession,
    tool: &str,
) -> (Vec<Value>, Value, usize, Vec<Value>) {
    let initialized = session.request(initialize());
    assert!(initialized.get("error").is_none(), "{initialized}");
    session.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    session.await_project_ready();
    let first = session.request(tools_list(2))["result"]["tools"]
        .as_array()
        .expect("tools")
        .clone();
    session.send(&tool_call(3, tool, &json!({})));
    let mut frames = session.wait_until(|frames| {
        frames.iter().any(|frame| frame["id"] == 3)
            && frames.iter().any(|frame| frame["method"] == LIST_CHANGED)
    });
    session.send(&tools_list(4));
    frames.extend(session.wait_until(|frames| frames.iter().any(|frame| frame["id"] == 4)));
    let answer = frames
        .iter()
        .find(|frame| frame["id"] == 3)
        .expect("call answer")
        .clone();
    let announced = frames
        .iter()
        .filter(|frame| frame["method"] == LIST_CHANGED)
        .count();
    let relisted = frames
        .iter()
        .find(|frame| frame["id"] == 4)
        .expect("re-list")["result"]["tools"]
        .as_array()
        .expect("tools")
        .clone();
    drop(session.finish(SERVE_TIMEOUT));
    (first, answer, announced, relisted)
}

/// `--catalog-profile=compact` changes what a session lists, never what it
/// can call or what the daemon stores.
#[tokio::test]
async fn compact_profile_withholds_work_and_workflow_tools_without_changing_authority() {
    let home = TempDir::new().unwrap();
    let project = init_project_with_file(home.path(), "pub fn catalog_profile_marker() {}\n").await;
    let _daemon = common::spawn_tracedecay_daemon(home.path());
    let oracle = work_and_workflow_tool_names();

    let default = run_serve(
        home.path(),
        project.path(),
        &[],
        &[initialize(), tools_list(2)],
    );
    assert!(default.status.success(), "{default:?}");
    let default_names = ServedList::from_stdout(&default.stdout, 2).names();
    let unlisted: Vec<&String> = oracle.difference(&default_names).collect();
    assert!(
        unlisted.is_empty(),
        "the default profile lists every Work and Workflow tool: {unlisted:?}"
    );

    for extra in [
        &[COMPACT_PROFILE][..],
        &[COMPACT_PROFILE, FULL_TOOL_LIST][..],
    ] {
        let compact = run_serve(
            home.path(),
            project.path(),
            extra,
            &[initialize(), tools_list(2)],
        );
        assert!(compact.status.success(), "{extra:?}: {compact:?}");
        let compact_names = ServedList::from_stdout(&compact.stdout, 2).names();
        let leaked: Vec<&String> = compact_names
            .iter()
            .filter(|name| is_work_or_workflow_name(name))
            .collect();
        assert!(
            leaked.is_empty(),
            "{extra:?} must list no Work or Workflow tool: {leaked:?}"
        );
        assert!(
            !compact_names.is_empty() && compact_names.is_subset(&default_names),
            "{extra:?} lists a non-empty subset of the default list: {compact_names:?}"
        );
        assert!(
            compact_names.len() < default_names.len(),
            "{extra:?} must list fewer tools than the default"
        );
    }

    let withheld = "tracedecay_work_views";
    let read_only = tool_named(
        &ServedList::from_stdout(&default.stdout, 2).tools,
        withheld,
    )["annotations"]["readOnlyHint"]
        .clone();
    assert_eq!(read_only, json!(true), "{withheld} must be a read");
    let (_, default_answer, _, _) = list_call_relist(
        ServeSession::start(serve_command(home.path(), project.path(), &[])),
        withheld,
    );

    let before = profile_state(home.path());
    let (first, compact_answer, announced, relisted) = list_call_relist(
        ServeSession::start(serve_command(
            home.path(),
            project.path(),
            &[COMPACT_PROFILE],
        )),
        withheld,
    );
    assert!(
        first.iter().all(|tool| tool["name"] != withheld),
        "{withheld} starts withheld"
    );
    assert_eq!(
        compact_answer["result"], default_answer["result"],
        "a withheld tool called by name reaches the same daemon authority"
    );
    assert_eq!(
        compact_answer["error"], default_answer["error"],
        "a withheld tool called by name reaches the same daemon authority"
    );
    assert_eq!(announced, 1, "the called tool is announced once");
    assert!(
        has_full_schema(tool_named(&relisted, withheld)),
        "the re-list after list_changed carries the full schema"
    );
    assert!(
        relisted
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .filter(|name| is_work_or_workflow_name(name))
            .all(|name| name == withheld),
        "only the called tool joins the list"
    );
    assert_eq!(
        profile_state(home.path()),
        before,
        "selecting a catalog profile stores nothing"
    );
}

#[test]
fn an_unknown_catalog_profile_fails_before_serving() {
    let home = TempDir::new().unwrap();
    let output = tracedecay_command_with_home(home.path())
        .args(["serve", "--catalog-profile=bogus"])
        .stdin(Stdio::null())
        .output()
        .expect("run tracedecay serve");
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(r#"unknown catalog profile "bogus""#)
            && ["default", "compact", "administrative", "host-limited"]
                .iter()
                .all(|name| stderr.contains(name)),
        "{stderr}"
    );
}
