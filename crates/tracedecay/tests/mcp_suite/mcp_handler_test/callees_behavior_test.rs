//! User-visible `tracedecay_callees` behavior over a real MCP `tools/call`.
//!
//! The fixture is one indexed project. Assertions name the functions, files,
//! and lines the tool returns, not the graph walk that produced them.
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
/// impls are not direct call edges; `resolve_trait_dispatch` adds them.
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
    line: u64,
    dispatch_via_trait: bool,
    depth: Option<u64>,
    /// `name:file:line` of the trait method this concrete impl was reached
    /// through. `None` for a direct call edge.
    dispatch_from: Option<String>,
}

fn direct(name: &str, kind: &str, file: &str, line: u64, depth: u64) -> ObservedCallee {
    ObservedCallee {
        name: name.to_owned(),
        kind: kind.to_owned(),
        file: file.to_owned(),
        line,
        dispatch_via_trait: false,
        depth: Some(depth),
        dispatch_from: None,
    }
}

/// A concrete impl reached through the depth-1 trait method callee.
fn trait_impl(line: u64) -> ObservedCallee {
    ObservedCallee {
        name: "process".to_owned(),
        kind: "method".to_owned(),
        file: "src/dispatch.rs".to_owned(),
        line,
        dispatch_via_trait: true,
        depth: Some(1),
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

async fn call_json(server: &McpServer, tool_name: &str, arguments: Value) -> Value {
    let result = handle_real_server_tool_call(server, tool_name, arguments).await;
    let text = extract_real_server_text(&result);
    serde_json::from_str(text).unwrap_or_else(|error| panic!("{tool_name} JSON ({error}): {text}"))
}

async fn symbol_id(server: &McpServer, name: &str, file: &str, line: u64) -> String {
    let payload = call_json(
        server,
        "tracedecay_find_exact_symbol",
        json!({"name": name, "limit": 20, "format": "json"}),
    )
    .await;
    payload["matches"]
        .as_array()
        .and_then(|matches| {
            matches
                .iter()
                .find(|item| item["name"] == name && item["file"] == file && item["line"] == line)
        })
        .and_then(|item| item["id"].as_str())
        .unwrap_or_else(|| panic!("exact symbol {name} at {file}:{line} missing: {payload}"))
        .to_owned()
}

fn items(payload: &Value) -> &Vec<Value> {
    payload
        .pointer("/outcome/value/payload/items")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("tracedecay_callees must return a callee page: {payload}"))
}

fn observe(payload: &Value) -> Vec<ObservedCallee> {
    let label = |symbol: &Value| {
        format!(
            "{}:{}:{}",
            symbol["name"].as_str().unwrap(),
            symbol["file"].as_str().unwrap(),
            symbol["line"]
        )
    };
    let labels = items(payload)
        .iter()
        .map(|item| {
            (
                item["symbol"]["node_id"].as_str().unwrap().to_owned(),
                label(&item["symbol"]),
            )
        })
        .collect::<HashMap<_, _>>();
    items(payload)
        .iter()
        .map(|item| ObservedCallee {
            name: item["symbol"]["name"].as_str().unwrap().to_owned(),
            kind: item["symbol"]["kind"].as_str().unwrap().to_owned(),
            file: item["symbol"]["file"].as_str().unwrap().to_owned(),
            line: item["symbol"]["line"].as_u64().unwrap(),
            dispatch_via_trait: item["dispatch_via_trait"].as_bool().unwrap(),
            depth: item["depth"].as_u64(),
            dispatch_from: item["dispatch_from"].as_str().map(|id| {
                labels.get(id).cloned().unwrap_or_else(|| {
                    panic!("dispatch_from {id} is not a callee in this response: {payload}")
                })
            }),
        })
        .collect()
}

async fn assert_ids_are_exact_symbols(server: &McpServer, payload: &Value) {
    for item in items(payload) {
        let symbol = &item["symbol"];
        let (name, file, line) = (
            symbol["name"].as_str().unwrap(),
            symbol["file"].as_str().unwrap(),
            symbol["line"].as_u64().unwrap(),
        );
        assert_eq!(
            symbol["node_id"].as_str().unwrap(),
            symbol_id(server, name, file, line).await,
            "{name} at {file}:{line} must be the exact-symbol id"
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

fn chain_from_level_11(levels: u64) -> Vec<ObservedCallee> {
    (1..=levels)
        .map(|depth| {
            let level = 11 - depth;
            direct(
                &format!("level_{level}"),
                "function",
                "src/chain.rs",
                level + 1,
                depth,
            )
        })
        .collect()
}

#[tokio::test]
async fn tracedecay_callees_lists_direct_and_deeper_calls() {
    let fixture = open_project().await;
    let server = server(&fixture);
    let level_11 = symbol_id(&server, "level_11", "src/chain.rs", 12).await;
    let level_1 = symbol_id(&server, "level_1", "src/chain.rs", 2).await;
    let level_0 = symbol_id(&server, "level_0", "src/chain.rs", 1).await;
    let entry = symbol_id(&server, "entry", "src/lib.rs", 7).await;

    let one_hop = callees(&server, &level_11, json!({"maximum_depth": 1})).await;
    assert_eq!(observe(&one_hop), chain_from_level_11(1));
    assert_ids_are_exact_symbols(&server, &one_hop).await;

    let default_depth = callees(&server, &level_11, json!({})).await;
    assert_eq!(
        observe(&default_depth),
        chain_from_level_11(3),
        "a bare node_id walks three levels"
    );

    let deepest = callees(&server, &level_11, json!({"maximum_depth": 10})).await;
    assert_eq!(observe(&deepest), chain_from_level_11(10));

    let leaf = callees(&server, &level_0, json!({"maximum_depth": 3})).await;
    assert!(
        observe(&leaf).is_empty(),
        "level_0 calls nothing; got {leaf}"
    );
    let caller_of_leaf = callees(&server, &level_1, json!({"maximum_depth": 1})).await;
    assert_eq!(
        observe(&caller_of_leaf),
        vec![direct("level_0", "function", "src/chain.rs", 1, 1)]
    );

    let imported = callees(&server, &entry, json!({"maximum_depth": 1})).await;
    assert_eq!(
        observe(&imported),
        vec![direct("helper", "function", "src/sibling.rs", 1, 1)]
    );
    assert_ids_are_exact_symbols(&server, &imported).await;

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn tracedecay_callees_adds_trait_impls_unless_dispatch_is_off() {
    let fixture = open_project().await;
    let server = server(&fixture);
    let via_trait = symbol_id(&server, "via_trait", "src/dispatch.rs", 21).await;

    let direct_only = callees(
        &server,
        &via_trait,
        json!({"maximum_depth": 1, "resolve_trait_dispatch": false}),
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
    let default_payload = callees(&server, &via_trait, json!({"maximum_depth": 1})).await;
    let mut default_resolved = observe(&default_payload);
    default_resolved.sort_by(by_source);
    assert_eq!(
        default_resolved, expected,
        "trait dispatch resolution is on unless the caller turns it off"
    );
    assert_ids_are_exact_symbols(&server, &default_payload).await;

    fixture.harness.shutdown().await;
}

fn assert_refused(response: &Value, context: &str) {
    assert!(
        !response["error"].is_null() || response["result"]["isError"] == true,
        "{context} must be refused, not answered: {response}"
    );
}

#[tokio::test]
async fn tracedecay_callees_rejects_invalid_arguments() {
    let fixture = open_project().await;
    let server = server(&fixture);
    let level_0 = symbol_id(&server, "level_0", "src/chain.rs", 1).await;
    let refuse = |arguments: Value| {
        let server = Arc::clone(&server);
        async move { handle_real_server_tool_call_raw(&server, "tracedecay_callees", arguments).await }
    };

    assert_refused(&refuse(json!({"node_id": "   "})).await, "a blank node_id");
    assert_refused(
        &refuse(json!({"node_id": level_0, "maximum_depth": 0})).await,
        "maximum_depth 0",
    );
    assert_refused(
        &refuse(json!({"node_id": level_0, "maximum_depth": 11})).await,
        "maximum_depth above the traversal bound",
    );
    assert_refused(&refuse(json!({})).await, "a missing node_id");
    assert_refused(
        &refuse(json!({"node_id": level_0, "maximum_depth": "deep"})).await,
        "a non-integer maximum_depth",
    );
    let unknown = refuse(json!({"node_id": "symbol.absent-callee"})).await;
    let unknown: Value = serde_json::from_str(extract_real_server_text(&unknown["result"]))
        .unwrap_or_else(|error| panic!("unknown-occurrence callees JSON ({error}): {unknown}"));
    let evidence = &unknown["outcome"]["value"];
    assert_eq!(
        (&evidence["execution"]["termination"], &evidence["payload"]),
        (&json!("unavailable"), &Value::Null),
        "an unknown occurrence is a typed unavailable read, not an empty callee list: {unknown}"
    );
    for retired in [json!({"max_depth": 1}), json!({"resolve_dispatch": false})] {
        let mut arguments = json!({"node_id": level_0});
        for (key, value) in retired.as_object().unwrap() {
            arguments[key] = value.clone();
        }
        assert_refused(
            &refuse(arguments).await,
            &format!("retired argument {retired}"),
        );
    }

    fixture.harness.shutdown().await;
}
