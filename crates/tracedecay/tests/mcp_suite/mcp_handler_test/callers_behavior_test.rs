//! `tracedecay_callers` through the production MCP `tools/call` path.
//!
//! The fixture is a two-file Rust crate:
//!
//! ```text
//! src/main.rs
//!   mod worker;
//!   use crate::worker::prepare_order;
//!
//!   fn main() {          // line 4
//!       prepare_order();
//!   }
//!
//! src/worker.rs
//!   pub fn prepare_order() { // line 1
//!       settle();
//!   }
//!
//!   fn also() {          // line 5
//!       settle();
//!   }
//!
//!   fn settle() {}       // line 9
//! ```
//!
//! `settle` is called by `also` and `prepare_order`. `prepare_order` is called
//! by `main`. The callee is not named `run`: that bare name is withheld from
//! cross-file binding, so a depth-2 walk would never reach `main`. `main`
//! has no callers.

#![cfg(feature = "test-transport")]

use std::fs;

use serde_json::{Value, json};
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay::mcp::McpServer;

use crate::support::{
    commit_worktree, dispatch_mcp_tool_call, handle_real_server_tool_call_raw,
    production_composition_fixture_with_sources, test_temp_dir, warm_code_index_search,
};

const MAIN_RS: &str = "\
mod worker;\n\
use crate::worker::prepare_order;\n\
\n\
fn main() {\n\
    prepare_order();\n\
}\n";

const WORKER_RS: &str = "\
pub fn prepare_order() {\n\
    settle();\n\
}\n\
\n\
fn also() {\n\
    settle();\n\
}\n\
\n\
fn settle() {}\n";

const UNKNOWN_OCCURRENCE: &str =
    "symbol.v1.sha256:4f4adb437af949d76698f841fde2eab2d2d4c62c56e24bdfa0f1de614219a34b";

async fn call_callers(server: &McpServer, arguments: Value) -> Value {
    handle_real_server_tool_call_raw(server, "tracedecay_callers", arguments).await
}

/// The evidence value of a successful application-surface answer.
fn evidence(response: &Value) -> Value {
    assert!(
        response["error"].is_null() && response["result"]["isError"].is_null(),
        "tracedecay_callers failed: {response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tracedecay_callers result has no text: {response}"));
    let payload: Value = serde_json::from_str(text).expect("callers JSON");
    payload["outcome"]["value"].clone()
}

/// `(name, file, line, depth)` rows in source order.
fn caller_rows(evidence: &Value) -> Vec<(String, String, u64, u64)> {
    let mut rows = evidence["payload"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("caller page has no items: {evidence}"))
        .iter()
        .map(|item| {
            (
                item["symbol"]["name"].as_str().unwrap().to_owned(),
                item["symbol"]["file"].as_str().unwrap().to_owned(),
                item["symbol"]["line"].as_u64().unwrap(),
                item["depth"].as_u64().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| (&left.1, left.2).cmp(&(&right.1, right.2)));
    rows
}

fn row(name: &str, file: &str, line: u64, depth: u64) -> (String, String, u64, u64) {
    (name.to_owned(), file.to_owned(), line, depth)
}

/// A request the surface refuses before any traversal, as a typed problem.
fn assert_invalid_request(response: &Value, context: &str) {
    let refused_at_parse =
        response["error"]["data"]["reason_code"] == "application_surface_invalid_request";
    let refused_by_contract = response["result"]["isError"] == true
        && response["result"]["structuredContent"]["problem"]["kind"] == "invalid_request";
    assert!(
        refused_at_parse || refused_by_contract,
        "{context} must be a typed invalid request: {response}"
    );
}

async fn function_id(server: &McpServer, name: &str) -> String {
    let response = handle_real_server_tool_call_raw(
        server,
        "tracedecay_find_exact_symbol",
        json!({"name": name, "limit": 20}),
    )
    .await;
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("exact-symbol response has no text: {response}"));
    let payload: Value = serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("exact-symbol response is not JSON: {error}: {text}"));
    let matches = payload["matches"]
        .as_array()
        .unwrap_or_else(|| panic!("exact-symbol response has no matches: {payload}"));
    let ids = matches
        .iter()
        .filter(|item| item["name"] == name && item["kind"] == "function")
        .filter_map(|item| item["id"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        ids.len(),
        1,
        "expected one function named {name}, got {payload}"
    );
    ids[0].to_owned()
}

#[tokio::test]
async fn tracedecay_callers_reports_literal_call_sites_and_typed_rejections() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src/main.rs"), MAIN_RS).unwrap();
        fs::write(project.join("src/worker.rs"), WORKER_RS).unwrap();
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    warm_code_index_search(&server, "settle").await;

    let settle_id = function_id(&server, "settle").await;
    let also_id = function_id(&server, "also").await;
    let prepare_order_id = function_id(&server, "prepare_order").await;
    let main_id = function_id(&server, "main").await;
    let direct = vec![
        row("prepare_order", "src/worker.rs", 1, 1),
        row("also", "src/worker.rs", 5, 1),
    ];

    let depth_one =
        evidence(&call_callers(&server, json!({"node_id": settle_id, "maximum_depth": 1})).await);
    assert_eq!(
        caller_rows(&depth_one),
        direct,
        "maximum_depth 1 must list only the two functions that call settle"
    );
    assert_eq!(depth_one["coverage"]["completeness"], "complete");

    let default_depth = evidence(&call_callers(&server, json!({"node_id": settle_id})).await);
    let mut transitive = vec![row("main", "src/main.rs", 4, 2)];
    transitive.extend(direct.clone());
    assert_eq!(
        caller_rows(&default_depth),
        transitive,
        "a bare node_id defaults to a walk that reaches main through prepare_order"
    );

    let prepare_order_callers = evidence(
        &call_callers(
            &server,
            json!({"node_id": prepare_order_id, "maximum_depth": 1}),
        )
        .await,
    );
    assert_eq!(
        caller_rows(&prepare_order_callers),
        vec![row("main", "src/main.rs", 4, 1)],
        "prepare_order's only caller is main at src/main.rs:4"
    );

    let no_callers = evidence(&call_callers(&server, json!({"node_id": main_id})).await);
    assert!(
        caller_rows(&no_callers).is_empty(),
        "main has no callers; settle in the same graph does: {no_callers}"
    );
    assert_eq!(no_callers["coverage"]["completeness"], "complete");

    let unknown = call_callers(&server, json!({"node_id": UNKNOWN_OCCURRENCE})).await;
    assert!(
        !unknown["error"].is_null() || unknown["result"]["isError"] == true,
        "an unknown occurrence cannot be mistaken for a known function with no callers: {unknown}"
    );

    let markdown = dispatch_mcp_tool_call(
        &server,
        "tracedecay_callers",
        json!({"node_id": settle_id, "maximum_depth": 1}),
    )
    .await;
    let markdown = markdown["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("markdown callers text: {markdown}"));
    for (name, line, id) in [
        ("also", 5, &also_id),
        ("prepare_order", 1, &prepare_order_id),
    ] {
        let expected =
            format!("src/worker.rs::{name} (function) src/worker.rs:{line} node_id={id} depth=1");
        assert!(
            markdown.contains(&expected),
            "agents that omit format receive one line per caller ({expected}): {markdown}"
        );
    }

    assert_invalid_request(&call_callers(&server, json!({})).await, "a missing node_id");
    assert_invalid_request(
        &call_callers(&server, json!({"node_id": "   "})).await,
        "a blank node_id",
    );
    assert_invalid_request(
        &call_callers(&server, json!({"node_id": settle_id, "maximum_depth": 0})).await,
        "maximum_depth 0",
    );
    assert_invalid_request(
        &call_callers(&server, json!({"node_id": settle_id, "max_depth": 1})).await,
        "the retired max_depth argument",
    );

    fixture.harness.shutdown().await;
}

/// A registered-project selector reads the selected project's own graph. The
/// target's symbols do not exist in the active project, so an aliased read of
/// the active graph could not produce these rows.
#[tokio::test]
async fn tracedecay_callers_reads_the_selected_registered_project() {
    let isolation = test_temp_dir();
    let active = isolation.path().join("active");
    let target = isolation.path().join("target");
    fs::create_dir_all(active.join("src")).unwrap();
    fs::write(active.join("src/lib.rs"), "pub fn active_only() {}\n").unwrap();
    fs::create_dir_all(target.join("src")).unwrap();
    fs::write(target.join("src/main.rs"), MAIN_RS).unwrap();
    fs::write(target.join("src/worker.rs"), WORKER_RS).unwrap();
    commit_worktree(&active, "active project");
    commit_worktree(&target, "target project");
    let harness = Box::pin(ProductionProjectCompositionHarnessV1::open(
        isolation.path(),
        vec![active.clone(), target.clone()],
    ))
    .await
    .expect("two-project production composition");
    let target_server = harness.server(&target).expect("target project server");
    warm_code_index_search(&target_server, "settle").await;
    let settle_id = function_id(&target_server, "settle").await;
    let main_id = function_id(&target_server, "main").await;
    let target_project_id = target_server
        .cg()
        .await
        .store_layout()
        .identity
        .project_id
        .clone()
        .expect("target project identity");
    let active_server = harness.server(&active).expect("active project server");
    warm_code_index_search(&active_server, "active_only").await;
    let selector = json!({"project_id": target_project_id});

    let unselected = call_callers(&active_server, json!({"node_id": settle_id})).await;
    assert!(
        !unselected["error"].is_null() || unselected["result"]["isError"] == true,
        "the target's symbol is absent from the active graph: {unselected}"
    );

    let selected = evidence(
        &call_callers(
            &active_server,
            json!({"node_id": settle_id, "maximum_depth": 1, "project_selector": selector}),
        )
        .await,
    );
    assert_eq!(
        caller_rows(&selected),
        vec![
            row("prepare_order", "src/worker.rs", 1, 1),
            row("also", "src/worker.rs", 5, 1),
        ],
        "the selector routes the read to the target project's graph"
    );

    let chain = handle_real_server_tool_call_raw(
        &active_server,
        "tracedecay_call_chain",
        json!({"from_node_id": main_id, "to_node_id": settle_id, "project_selector": selector}),
    )
    .await;
    let chain = evidence(&chain);
    assert_eq!(
        chain["payload"]["node_ids"].as_array().map(Vec::len),
        Some(3),
        "main -> prepare_order -> settle exists only in the target graph: {chain}"
    );

    let dependents = evidence(
        &handle_real_server_tool_call_raw(
            &active_server,
            "tracedecay_file_dependents",
            json!({"file": "src/worker.rs", "project_selector": selector}),
        )
        .await,
    );
    assert_eq!(
        dependents["payload"]["file"], "src/worker.rs",
        "file dependents answer from the target project: {dependents}"
    );

    let unregistered = call_callers(
        &active_server,
        json!({"node_id": settle_id, "project_selector": {"project_id": "project.not-registered"}}),
    )
    .await;
    assert!(
        unregistered["result"].is_null()
            && unregistered["error"]["data"]["reason_code"]
                .as_str()
                .is_some_and(|code| code.starts_with("project_route")),
        "an unregistered selection is a typed route state: {unregistered}"
    );

    harness.shutdown().await;
}

const MACRO_LIB_RS: &str = "\
macro_rules! cfg_rt { ($($item:item)*) => { $($item)* } }\n\
macro_rules! route { ($($tokens:tt)*) => {}; }\n\
\n\
cfg_rt! {\n\
    pub mod foo {\n\
        pub fn bar() {}\n\
    }\n\
}\n\
\n\
route! { \"/\" => handler() }\n\
\n\
pub fn entry() {\n\
    foo::bar();\n\
}\n\
\n\
pub fn handler() {}\n";

/// An item-list macro body (`cfg_rt! { ... }`) is parsed, so `bar` is a symbol
/// with a complete caller answer. A body that is not an item list is not
/// expanded: its call is attributed to the `route!` invocation, and the answer
/// is partial with a typed `macro_body_unparsed` omission naming it.
#[tokio::test]
async fn tracedecay_callers_parses_item_macro_bodies_and_discloses_unexpanded_ones() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src/lib.rs"), MACRO_LIB_RS).unwrap();
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    warm_code_index_search(&server, "handler").await;

    let bar_id = function_id(&server, "bar").await;
    let bar_callers =
        evidence(&call_callers(&server, json!({"node_id": bar_id, "maximum_depth": 1})).await);
    assert_eq!(
        caller_rows(&bar_callers),
        vec![row("entry", "src/lib.rs", 12, 1)],
        "bar inside cfg_rt! is called by entry: {bar_callers}"
    );
    assert_eq!(bar_callers["coverage"]["completeness"], "complete");
    assert_eq!(bar_callers["omissions"], json!([]));

    let handler_id = function_id(&server, "handler").await;
    let handler_callers =
        evidence(&call_callers(&server, json!({"node_id": handler_id, "maximum_depth": 1})).await);
    assert_eq!(
        caller_rows(&handler_callers),
        vec![row("route!", "src/lib.rs", 10, 1)],
        "the call inside the unexpanded route! body is attributed to the invocation"
    );
    assert_eq!(handler_callers["coverage"]["completeness"], "partial");
    assert_eq!(
        handler_callers["omissions"],
        json!([{"domain": "graph", "count": 1, "reason": "macro_body_unparsed"}])
    );
    assert_eq!(
        handler_callers["payload"]["support_gaps"],
        json!([{"provider": "code_index", "language": null, "reason": "macro_body_unparsed: route!"}])
    );

    fixture.harness.shutdown().await;
}
