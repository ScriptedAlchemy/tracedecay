//! User-visible `tracedecay_callees` behavior over a real MCP `tools/call`.
//!
//! The fixture is one indexed project. Assertions name the functions, files,
//! and lines the handler returns, not the graph walk that produced them.
//! Occurrence ids are not literals: they are content hashes. Each returned
//! id must be the id `tracedecay_find_exact_symbol` gives for that same
//! name, file, and line, which is how an agent chains the two tools.

#![cfg(feature = "test-transport")]

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs;
use std::sync::Arc;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;
use tracedecay_contracts::retrieval::CalleeV1;

use crate::support::{
    ProductionCompositionFixture, extract_real_server_text, handle_real_server_tool_call,
    handle_real_server_tool_call_raw, production_composition_fixture_with_sources,
    wait_for_current_graph,
};

const LIB_RS: &str = "\
mod chain;
mod dispatch;
mod sibling;

use sibling::helper;

pub fn entry() {
    helper();
}
";

const SIBLING_RS: &str = "pub fn helper() {}\n";

const CHAIN_RS: &str = "\
pub fn level_0() {}\n\
pub fn level_1() { level_0(); }\n\
pub fn level_2() { level_1(); }\n\
pub fn level_3() { level_2(); }\n\
pub fn level_4() { level_3(); }\n\
pub fn level_5() { level_4(); }\n\
pub fn level_6() { level_5(); }\n\
pub fn level_7() { level_6(); }\n\
pub fn level_8() { level_7(); }\n\
pub fn level_9() { level_8(); }\n\
pub fn level_10() { level_9(); }\n\
pub fn level_11() { level_10(); }\n\
";

/// UFCS `Processor::process` binds the trait method. Concrete `process`
/// impls are not direct call edges; `resolve_dispatch` is what adds them.
const DISPATCH_RS: &str = "\
pub trait Processor {\n\
    fn process(&self, input: u32) -> u32;\n\
}\n\
\n\
pub struct Doubler;\n\
\n\
impl Processor for Doubler {\n\
    fn process(&self, input: u32) -> u32 {\n\
        input * 2\n\
    }\n\
}\n\
\n\
pub struct Tripler;\n\
\n\
impl Processor for Tripler {\n\
    fn process(&self, input: u32) -> u32 {\n\
        input * 3\n\
    }\n\
}\n\
\n\
pub fn via_trait(processor: &Doubler, input: u32) -> u32 {\n\
    Processor::process(processor, input)\n\
}\n\
";

#[derive(Clone, Debug, PartialEq, Eq)]
struct ObservedCallee {
    name: String,
    kind: String,
    file: String,
    line: u32,
    edge_kind: String,
    dispatch_via_trait: bool,
    depth: Option<u32>,
    /// `name:file:line` of the trait method this concrete impl was reached
    /// through. `None` for a direct call edge.
    dispatch_from: Option<String>,
}

fn direct(name: &str, kind: &str, file: &str, line: u32, depth: u32) -> ObservedCallee {
    ObservedCallee {
        name: name.to_owned(),
        kind: kind.to_owned(),
        file: file.to_owned(),
        line,
        edge_kind: "calls".to_owned(),
        dispatch_via_trait: false,
        depth: Some(depth),
        dispatch_from: None,
    }
}

fn trait_impl(line: u32) -> ObservedCallee {
    ObservedCallee {
        name: "process".to_owned(),
        kind: "method".to_owned(),
        file: "src/dispatch.rs".to_owned(),
        line,
        edge_kind: "calls".to_owned(),
        dispatch_via_trait: true,
        depth: None,
        dispatch_from: Some("process:src/dispatch.rs:2".to_owned()),
    }
}

async fn open_project() -> ProductionCompositionFixture {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src/lib.rs"), LIB_RS).unwrap();
        fs::write(project.join("src/chain.rs"), CHAIN_RS).unwrap();
        fs::write(project.join("src/dispatch.rs"), DISPATCH_RS).unwrap();
        fs::write(project.join("src/sibling.rs"), SIBLING_RS).unwrap();
    })
    .await;
    let server = server(&fixture);
    wait_for_current_graph(&server).await;
    fixture
}

fn server(fixture: &ProductionCompositionFixture) -> Arc<McpServer> {
    fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server for the callees fixture")
}

async fn shutdown(fixture: ProductionCompositionFixture) {
    fixture.harness.shutdown().await;
}

async fn call_json(server: &McpServer, tool_name: &str, arguments: Value) -> Value {
    let result = handle_real_server_tool_call(server, tool_name, arguments).await;
    let text = extract_real_server_text(&result);
    serde_json::from_str(text).unwrap_or_else(|error| panic!("{tool_name} JSON ({error}): {text}"))
}

async fn symbol_id(server: &McpServer, name: &str, file: &str, line: u32) -> String {
    let payload = call_json(
        server,
        "tracedecay_find_exact_symbol",
        json!({"name": name, "limit": 20, "format": "json"}),
    )
    .await;
    payload["matches"]
        .as_array()
        .and_then(|matches| {
            matches.iter().find(|item| {
                item["name"] == name && item["file"] == file && item["line"] == u64::from(line)
            })
        })
        .and_then(|item| item["id"].as_str())
        .unwrap_or_else(|| panic!("exact symbol {name} at {file}:{line} missing: {payload}"))
        .to_owned()
}

fn observe(payload: &Value) -> Vec<ObservedCallee> {
    let items: Vec<CalleeV1> = serde_json::from_value(payload.clone()).unwrap_or_else(|error| {
        panic!("tracedecay_callees must return CalleeV1 rows: {error}; {payload}")
    });
    let labels = items
        .iter()
        .map(|item| {
            (
                item.node_id.clone(),
                format!("{}:{}:{}", item.name, item.file, item.line),
            )
        })
        .collect::<HashMap<_, _>>();
    items
        .iter()
        .map(|item| {
            let dispatch_from = item.dispatch_from.as_ref().map(|id| {
                labels.get(id).cloned().unwrap_or_else(|| {
                    panic!("dispatch_from {id} is not a callee in this response: {payload}")
                })
            });
            ObservedCallee {
                name: item.name.clone(),
                kind: item.kind.clone(),
                file: item.file.clone(),
                line: item.line,
                edge_kind: item.edge_kind.clone(),
                dispatch_via_trait: item.dispatch_via_trait,
                depth: item.depth,
                dispatch_from,
            }
        })
        .collect()
}

async fn assert_ids_are_exact_symbols(server: &McpServer, payload: &Value) {
    let items: Vec<CalleeV1> = serde_json::from_value(payload.clone())
        .unwrap_or_else(|error| panic!("callees rows: {error}; {payload}"));
    for item in items {
        let expected = symbol_id(server, &item.name, &item.file, item.line).await;
        assert_eq!(
            item.node_id, expected,
            "{} at {}:{} must be the exact-symbol id",
            item.name, item.file, item.line
        );
    }
}

async fn callees(server: &McpServer, node_id: &str, extra: Value) -> Value {
    let mut arguments = json!({"node_id": node_id, "format": "json"});
    if let (Some(target), Some(extra)) = (arguments.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            target.insert(key.clone(), value.clone());
        }
    }
    call_json(server, "tracedecay_callees", arguments).await
}

fn by_source(left: &ObservedCallee, right: &ObservedCallee) -> Ordering {
    (
        left.file.as_str(),
        left.line,
        left.dispatch_via_trait,
        left.name.as_str(),
        left.depth,
    )
        .cmp(&(
            right.file.as_str(),
            right.line,
            right.dispatch_via_trait,
            right.name.as_str(),
            right.depth,
        ))
}

#[tokio::test]
async fn tracedecay_callees_lists_direct_and_deeper_calls() {
    let fixture = open_project().await;
    let server = server(&fixture);
    let level_11 = symbol_id(&server, "level_11", "src/chain.rs", 12).await;
    let level_1 = symbol_id(&server, "level_1", "src/chain.rs", 2).await;
    let level_0 = symbol_id(&server, "level_0", "src/chain.rs", 1).await;
    let entry = symbol_id(&server, "entry", "src/lib.rs", 7).await;

    let one_hop = callees(&server, &level_11, json!({"max_depth": 1})).await;
    assert_eq!(
        observe(&one_hop),
        vec![direct("level_10", "function", "src/chain.rs", 11, 1)]
    );
    assert_ids_are_exact_symbols(&server, &one_hop).await;

    let default_depth = callees(&server, &level_11, json!({})).await;
    assert_eq!(
        observe(&default_depth),
        vec![
            direct("level_10", "function", "src/chain.rs", 11, 1),
            direct("level_9", "function", "src/chain.rs", 10, 2),
            direct("level_8", "function", "src/chain.rs", 9, 3),
        ]
    );

    let clamped = callees(&server, &level_11, json!({"max_depth": 99})).await;
    assert_eq!(
        observe(&clamped),
        vec![
            direct("level_10", "function", "src/chain.rs", 11, 1),
            direct("level_9", "function", "src/chain.rs", 10, 2),
            direct("level_8", "function", "src/chain.rs", 9, 3),
            direct("level_7", "function", "src/chain.rs", 8, 4),
            direct("level_6", "function", "src/chain.rs", 7, 5),
            direct("level_5", "function", "src/chain.rs", 6, 6),
            direct("level_4", "function", "src/chain.rs", 5, 7),
            direct("level_3", "function", "src/chain.rs", 4, 8),
            direct("level_2", "function", "src/chain.rs", 3, 9),
            direct("level_1", "function", "src/chain.rs", 2, 10),
        ]
    );

    let leaf = callees(&server, &level_0, json!({"max_depth": 3})).await;
    let caller_of_leaf = callees(&server, &level_1, json!({"max_depth": 1})).await;
    assert_eq!(
        observe(&leaf),
        Vec::<ObservedCallee>::new(),
        "level_0 calls nothing; got {leaf}"
    );
    assert_eq!(
        observe(&caller_of_leaf),
        vec![direct("level_0", "function", "src/chain.rs", 1, 1)]
    );

    let missing = callees(
        &server,
        "symbol.absent-callee",
        json!({"max_depth": 1, "resolve_dispatch": false}),
    )
    .await;
    assert_eq!(
        observe(&missing),
        Vec::<ObservedCallee>::new(),
        "an unknown occurrence is empty, not an error; got {missing}"
    );
    assert_eq!(
        observe(&caller_of_leaf),
        vec![direct("level_0", "function", "src/chain.rs", 1, 1)],
        "level_1 still calls level_0"
    );

    let imported = callees(&server, &entry, json!({"max_depth": 1})).await;
    assert_eq!(
        observe(&imported),
        vec![direct("helper", "function", "src/sibling.rs", 1, 1)]
    );
    assert_ids_are_exact_symbols(&server, &imported).await;

    shutdown(fixture).await;
}

#[tokio::test]
async fn tracedecay_callees_adds_trait_impls_unless_dispatch_is_off() {
    let fixture = open_project().await;
    let server = server(&fixture);
    let via_trait = symbol_id(&server, "via_trait", "src/dispatch.rs", 21).await;

    let direct_only = callees(
        &server,
        &via_trait,
        json!({"max_depth": 1, "resolve_dispatch": false}),
    )
    .await;
    assert_eq!(
        observe(&direct_only),
        vec![direct("process", "method", "src/dispatch.rs", 2, 1)]
    );
    assert_ids_are_exact_symbols(&server, &direct_only).await;

    let expected = vec![
        direct("process", "method", "src/dispatch.rs", 2, 1),
        trait_impl(8),
        trait_impl(16),
    ];
    let resolved_payload = callees(
        &server,
        &via_trait,
        json!({"max_depth": 1, "resolve_dispatch": true}),
    )
    .await;
    let mut resolved = observe(&resolved_payload);
    resolved.sort_by(by_source);
    assert_eq!(resolved, expected);
    assert_ids_are_exact_symbols(&server, &resolved_payload).await;

    let mut default_resolved =
        observe(&callees(&server, &via_trait, json!({"max_depth": 1})).await);
    default_resolved.sort_by(by_source);
    assert_eq!(
        default_resolved, expected,
        "resolve_dispatch defaults to expanding trait impls"
    );

    shutdown(fixture).await;
}

fn assert_refusal(response: &Value, message: &str) {
    assert!(
        response["result"].is_null(),
        "refusal must not carry a result: {response}"
    );
    assert_eq!(response["error"]["code"], -32603, "{response}");
    assert_eq!(response["error"]["message"], message, "{response}");
    assert_eq!(
        response["error"]["data"]["tool"], "tracedecay_callees",
        "{response}"
    );
}

#[tokio::test]
async fn tracedecay_callees_rejects_invalid_arguments() {
    let fixture = open_project().await;
    let server = server(&fixture);
    let level_0 = symbol_id(&server, "level_0", "src/chain.rs", 1).await;

    let blank = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_callees",
        json!({"node_id": "   ", "format": "json"}),
    )
    .await;
    assert_refusal(
        &blank,
        "tool execution failed: config error: invalid parameter: node_id must not be empty",
    );

    let evidence_anchor = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_callees",
        json!({"node_id": "code-file:not-a-symbol", "format": "json"}),
    )
    .await;
    assert_refusal(
        &evidence_anchor,
        "tool execution failed: config error: invalid parameter: node_id `code-file:not-a-symbol` is an evidence anchor, not a graph symbol occurrence",
    );

    let zero_depth = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_callees",
        json!({"node_id": level_0, "max_depth": 0, "format": "json"}),
    )
    .await;
    assert_refusal(
        &zero_depth,
        "tool execution failed: config error: invalid parameter: max_depth must be at least 1",
    );

    let missing_node =
        handle_real_server_tool_call_raw(&server, "tracedecay_callees", json!({"format": "json"}))
            .await;
    assert_refusal(
        &missing_node,
        "tool execution failed: config error: invalid arguments for tracedecay_callees: missing field `node_id`",
    );

    let bad_depth = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_callees",
        json!({"node_id": "symbol.present", "max_depth": "deep", "format": "json"}),
    )
    .await;
    assert_refusal(
        &bad_depth,
        "tool execution failed: config error: invalid arguments for tracedecay_callees: invalid type: string \"deep\", expected u32",
    );

    shutdown(fixture).await;
}
