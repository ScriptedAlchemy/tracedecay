//! Literal `tracedecay_recursion` results from production MCP `tools/call`.
//!
//! `length` is the number of call edges in the cycle. The chain repeats its
//! start symbol so the path is closed. Occurrence ids include the temp
//! project path, so which node the search starts on changes between runs.
//! Comparisons rotate each chain to its smallest `(name, file, line)` and
//! then pin names, kinds, files, and lines. Ids are still required to close
//! the cycle.

use std::path::Path;

use serde_json::{Value, json};

use super::{close_test_graph, handle_tool_call, init_test_project};
use crate::support::{expect_tool_error, extract_json, test_temp_dir};

const DIRECT_SOURCE: &str = "\
pub fn recurse(n: u32) -> u32 {
    if n == 0 { 0 } else { recurse(n - 1) }
}

pub fn leaf() -> u32 { 1 }
";

const MUTUAL_SOURCE: &str = "\
pub fn ping() { pong(); }
pub fn pong() { ping(); }
";

const NOISE_SOURCE: &str = "\
pub struct Triplet {
    rows: Vec<usize>,
}

impl Triplet {
    pub fn push(&mut self, row: usize) {
        self.rows.push(row);
    }
}
";

fn direct_cycle() -> Value {
    json!({
        "length": 1,
        "chain": [
            {"name": "recurse", "kind": "function", "file": "src/direct.rs", "line": 1},
            {"name": "recurse", "kind": "function", "file": "src/direct.rs", "line": 1}
        ]
    })
}

fn mutual_cycle() -> Value {
    json!({
        "length": 2,
        "chain": [
            {"name": "ping", "kind": "function", "file": "src/mutual.rs", "line": 1},
            {"name": "pong", "kind": "function", "file": "src/mutual.rs", "line": 2},
            {"name": "ping", "kind": "function", "file": "src/mutual.rs", "line": 1}
        ]
    })
}

fn full_report() -> Value {
    json!({
        "cycle_count": 2,
        "cycles": [direct_cycle(), mutual_cycle()]
    })
}

pub(super) fn public_recursion_report(payload: &Value) -> Value {
    let keys = sorted_keys(payload, "recursion payload");
    assert_eq!(
        keys,
        ["cycle_count", "cycles"],
        "recursion payload keys drifted: {payload}"
    );
    let cycles = payload["cycles"]
        .as_array()
        .unwrap_or_else(|| panic!("cycles must be an array: {payload}"));
    let cycles = cycles
        .iter()
        .map(|cycle| {
            let cycle_keys = sorted_keys(cycle, "cycle");
            assert_eq!(
                cycle_keys,
                ["chain", "length"],
                "cycle keys drifted: {cycle}"
            );
            let chain = cycle["chain"]
                .as_array()
                .unwrap_or_else(|| panic!("chain must be an array: {cycle}"));
            json!({
                "length": cycle["length"],
                "chain": canonical_public_chain(chain),
            })
        })
        .collect::<Vec<_>>();
    let mut cycles = cycles;
    cycles.sort_by(|left, right| cycle_order_key(left).cmp(&cycle_order_key(right)));
    json!({
        "cycle_count": payload["cycle_count"],
        "cycles": cycles,
    })
}

fn cycle_order_key(cycle: &Value) -> (i64, String) {
    (
        cycle["length"].as_i64().unwrap_or(i64::MAX),
        cycle["chain"].to_string(),
    )
}

fn canonical_public_chain(chain: &[Value]) -> Vec<Value> {
    let public = chain.iter().map(public_chain_node).collect::<Vec<_>>();
    assert!(
        public.len() >= 2,
        "a cycle chain must repeat its start: {public:?}"
    );
    assert_eq!(
        public.first(),
        public.last(),
        "a cycle chain must close on the same symbol: {public:?}"
    );
    let body = &public[..public.len() - 1];
    let start = body
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| public_node_order(left).cmp(&public_node_order(right)))
        .map(|(index, _)| index)
        .expect("a cycle body is non-empty");
    let mut rotated = body[start..]
        .iter()
        .chain(&body[..start])
        .cloned()
        .collect::<Vec<_>>();
    rotated.push(rotated[0].clone());
    rotated
}

fn public_node_order(node: &Value) -> (String, String, i64) {
    (
        node["name"].as_str().unwrap_or_default().to_owned(),
        node["file"].as_str().unwrap_or_default().to_owned(),
        node["line"].as_i64().unwrap_or(i64::MAX),
    )
}

fn sorted_keys<'a>(value: &'a Value, label: &str) -> Vec<&'a str> {
    let mut keys = value
        .as_object()
        .unwrap_or_else(|| panic!("{label} must be an object: {value}"))
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    keys.sort_unstable();
    keys
}

fn public_chain_node(node: &Value) -> Value {
    let keys = sorted_keys(node, "chain node");
    assert_eq!(
        keys,
        ["file", "id", "kind", "line", "name"],
        "chain node keys drifted: {node}"
    );
    assert!(
        node["id"].as_str().is_some_and(|id| !id.is_empty()),
        "chain node id must be a non-empty string: {node}"
    );
    json!({
        "name": node["name"],
        "kind": node["kind"],
        "file": node["file"],
        "line": node["line"],
    })
}

pub(super) fn assert_reported_cycles_close(payload: &Value) {
    let cycles = payload["cycles"]
        .as_array()
        .unwrap_or_else(|| panic!("cycles must be an array: {payload}"));
    for cycle in cycles {
        let chain = cycle["chain"]
            .as_array()
            .unwrap_or_else(|| panic!("chain must be an array: {cycle}"));
        let start = chain
            .first()
            .and_then(|node| node["id"].as_str())
            .unwrap_or_else(|| panic!("cycle is missing its start id: {cycle}"));
        let end = chain
            .last()
            .and_then(|node| node["id"].as_str())
            .unwrap_or_else(|| panic!("cycle is missing its closing id: {cycle}"));
        assert_eq!(
            start, end,
            "a reported cycle must return to its start symbol: {cycle}"
        );
    }
}

async fn call_recursion(graph: &impl super::AnalysisToolHost, arguments: Value) -> Value {
    let result = handle_tool_call(graph, "tracedecay_recursion", arguments, None, None)
        .await
        .unwrap_or_else(|error| panic!("tracedecay_recursion failed: {error}"));
    extract_json(&result.value)
}

#[tokio::test]
async fn recursion_reports_literal_cycles_and_refuses_non_positive_limit() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    fs_write_fixture(&project_root);
    let (graph, ()) = init_test_project(&project_root).await;

    let payload = call_recursion(&graph, json!({"format": "json", "limit": 10})).await;
    assert_eq!(
        public_recursion_report(&payload),
        full_report(),
        "default-sized recursion report: {payload}"
    );
    assert_reported_cycles_close(&payload);

    let scoped = call_recursion(&graph, json!({"format": "json", "path": "src/direct.rs"})).await;
    assert_eq!(
        public_recursion_report(&scoped),
        json!({"cycle_count": 1, "cycles": [direct_cycle()]}),
        "path filter must keep only the direct cycle: {scoped}"
    );

    let mutual = call_recursion(
        &graph,
        json!({"format": "json", "path": "src/mutual.rs", "limit": 10}),
    )
    .await;
    assert_eq!(
        public_recursion_report(&mutual),
        json!({"cycle_count": 1, "cycles": [mutual_cycle()]}),
        "path filter must keep only the mutual cycle: {mutual}"
    );

    let noise = call_recursion(&graph, json!({"format": "json", "path": "src/noise.rs"})).await;
    assert_eq!(
        public_recursion_report(&noise),
        json!({"cycle_count": 0, "cycles": []}),
        "receiver `.push` must not be a cycle when the same graph has real cycles: {noise}"
    );

    let limited = call_recursion(&graph, json!({"format": "json", "limit": 1})).await;
    assert_eq!(
        public_recursion_report(&limited),
        json!({"cycle_count": 1, "cycles": [direct_cycle()]}),
        "limit 1 keeps the shortest cycle: {limited}"
    );

    let error = expect_tool_error(
        handle_tool_call(
            &graph,
            "tracedecay_recursion",
            json!({"format": "json", "limit": 0}),
            None,
            None,
        )
        .await,
    );
    assert_eq!(
        error,
        "config error: tracedecay_recursion failed over production MCP: tool execution failed: config error: invalid parameter: tracedecay_recursion requires limit to be at least 1"
    );
    close_test_graph(graph).await;
}

fn fs_write_fixture(project_root: &Path) {
    std::fs::create_dir_all(project_root.join("src")).unwrap();
    std::fs::write(
        project_root.join("src/lib.rs"),
        "pub mod direct;\npub mod mutual;\npub mod noise;\n",
    )
    .unwrap();
    std::fs::write(project_root.join("src/direct.rs"), DIRECT_SOURCE).unwrap();
    std::fs::write(project_root.join("src/mutual.rs"), MUTUAL_SOURCE).unwrap();
    std::fs::write(project_root.join("src/noise.rs"), NOISE_SOURCE).unwrap();
}
