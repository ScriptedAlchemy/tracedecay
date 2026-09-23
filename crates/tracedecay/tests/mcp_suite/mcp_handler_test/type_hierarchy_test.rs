#![cfg(feature = "test-transport")]

//! Production MCP behavior of `tracedecay_type_hierarchy`.
//!
//! The tool walks incoming `implements` and `extends` edges only.
//! `maximum_depth` counts those edges, not the root, and a node already
//! visited is not returned again.

use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use tracedecay_mcp::jsonrpc::JsonRpcResponse;

use crate::support::{
    ProductionCompositionFixture, production_composition_fixture_with_sources,
    wait_for_current_graph,
};

const ANIMALS_TS: &str = "\
interface Named {
  name: string
}
class AnimalBase implements Named {
  name: string
}
class Cat extends AnimalBase {
  meow(): boolean { return true }
}
";

const SPEAKER_RS: &str = "\
pub trait Speaker {
    fn speak(&self) -> &'static str;
}

pub struct Person;

impl Speaker for Person {
    fn speak(&self) -> &'static str {
        \"hello\"
    }
}

pub fn caller() {
    callee();
}

fn callee() {}
";

const CYCLE_TS: &str = "\
interface LoopLeft extends LoopRight {
  left: number
}
interface LoopRight extends LoopLeft {
  right: number
}
";

/// `(name, kind, file, line, edge_kind, depth, parent name)` in walk depth order.
type Entry = (String, String, String, u64, String, u64, String);

fn entry(
    name: &str,
    kind: &str,
    file: &str,
    line: u64,
    edge_kind: &str,
    depth: u64,
    parent: &str,
) -> Entry {
    (
        name.to_owned(),
        kind.to_owned(),
        file.to_owned(),
        line,
        edge_kind.to_owned(),
        depth,
        parent.to_owned(),
    )
}

fn named_root() -> Entry {
    entry(
        "Named",
        "interface",
        "src/animals.ts",
        1,
        "root",
        0,
        "Named",
    )
}

fn animal_base() -> Entry {
    entry(
        "AnimalBase",
        "class",
        "src/animals.ts",
        4,
        "implements",
        1,
        "Named",
    )
}

fn cat_child() -> Entry {
    entry(
        "Cat",
        "class",
        "src/animals.ts",
        7,
        "extends",
        2,
        "AnimalBase",
    )
}

#[tokio::test]
async fn type_hierarchy_reports_literal_trees_and_typed_refusals() {
    let fixture = production_composition_fixture_with_sources(write_hierarchy_sources).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");
    wait_for_current_graph(&server).await;

    let named = symbol_id(&fixture, "Named", "interface", "src/animals.ts").await;
    let cat = symbol_id(&fixture, "Cat", "class", "src/animals.ts").await;
    let speaker = symbol_id(&fixture, "Speaker", "trait", "src/lib.rs").await;
    let caller = symbol_id(&fixture, "caller", "function", "src/lib.rs").await;
    let loop_left = symbol_id(&fixture, "LoopLeft", "interface", "src/cycle.ts").await;

    let markdown = call_tool(&fixture, json!({"node_id": named})).await;
    let markdown = tool_text(&markdown);
    let tree = format!(
        "src/animals.ts::Named (interface) src/animals.ts:1 node_id={named}\n    \
         |- implements src/animals.ts::AnimalBase (class) src/animals.ts:4"
    );
    assert!(
        markdown.contains(&tree) && markdown.contains("  |- extends src/animals.ts::Cat (class)"),
        "agents that omit format read the implements/extends tree: {markdown}"
    );

    assert_eq!(
        hierarchy(&fixture, json!({"node_id": named})).await,
        vec![named_root(), animal_base(), cat_child()],
        "the default depth reaches Cat through AnimalBase"
    );
    assert_eq!(
        hierarchy(&fixture, json!({"node_id": named, "maximum_depth": 1})).await,
        vec![named_root(), animal_base()]
    );
    assert_eq!(
        hierarchy(&fixture, json!({"node_id": named, "maximum_depth": 10})).await,
        vec![named_root(), animal_base(), cat_child()]
    );
    assert_eq!(
        hierarchy(&fixture, json!({"node_id": cat})).await,
        vec![entry("Cat", "class", "src/animals.ts", 7, "root", 0, "Cat")]
    );
    assert_eq!(
        hierarchy(&fixture, json!({"node_id": speaker})).await,
        vec![
            entry("Speaker", "trait", "src/lib.rs", 1, "root", 0, "Speaker"),
            entry(
                "Person",
                "impl",
                "src/lib.rs",
                7,
                "implements",
                1,
                "Speaker"
            ),
        ]
    );
    assert_eq!(
        hierarchy(&fixture, json!({"node_id": caller})).await,
        vec![entry(
            "caller",
            "function",
            "src/lib.rs",
            13,
            "root",
            0,
            "caller"
        )]
    );
    assert_eq!(
        hierarchy(&fixture, json!({"node_id": loop_left})).await,
        vec![
            entry(
                "LoopLeft",
                "interface",
                "src/cycle.ts",
                1,
                "root",
                0,
                "LoopLeft"
            ),
            entry(
                "LoopRight",
                "interface",
                "src/cycle.ts",
                4,
                "extends",
                1,
                "LoopLeft"
            ),
        ],
        "a cycle returns each type once"
    );

    for (arguments, context) in [
        (json!({}), "a missing node_id"),
        (json!({"node_id": "   "}), "a blank node_id"),
        (
            json!({"node_id": named, "maximum_depth": 0}),
            "maximum_depth 0",
        ),
        (
            json!({"node_id": named, "maximum_depth": 11}),
            "maximum_depth above the bound",
        ),
        (
            json!({"node_id": named, "maximum_depth": "1"}),
            "a string maximum_depth",
        ),
        (
            json!({"node_id": named, "max_depth": 1}),
            "the retired max_depth argument",
        ),
    ] {
        assert_refused(&call_tool(&fixture, arguments).await, context);
    }
    let absent = call_tool(&fixture, json!({"node_id": "symbol.absent-hierarchy-root"})).await;
    let absent = absent
        .result
        .as_ref()
        .unwrap_or_else(|| panic!("{absent:?}"));
    assert_eq!(
        (&absent["isError"], &absent["problem"]["kind"]),
        (&json!(true), &json!("not_found_or_not_authorized")),
        "an absent root is a typed miss, not an empty hierarchy: {absent}"
    );

    fixture.harness.shutdown().await;
}

fn write_hierarchy_sources(project: &Path) {
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"hierarchy_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(project.join("src/animals.ts"), ANIMALS_TS).unwrap();
    fs::write(project.join("src/cycle.ts"), CYCLE_TS).unwrap();
    fs::write(project.join("src/lib.rs"), SPEAKER_RS).unwrap();
}

async fn hierarchy(fixture: &ProductionCompositionFixture, mut arguments: Value) -> Vec<Entry> {
    arguments["format"] = json!("json");
    let response = call_tool(fixture, arguments).await;
    let payload: Value = serde_json::from_str(tool_text(&response)).unwrap_or_else(|error| {
        panic!("type hierarchy JSON did not parse ({error}): {response:?}")
    });
    let items = payload["outcome"]["value"]["payload"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("type hierarchy page has no items: {payload}"));
    let name_of = |node_id: &Value| {
        items
            .iter()
            .find(|item| &item["symbol"]["node_id"] == node_id)
            .and_then(|item| item["symbol"]["name"].as_str())
            .unwrap_or_else(|| panic!("parent {node_id} is not on the page: {payload}"))
            .to_owned()
    };
    let mut entries = items
        .iter()
        .map(|item| {
            let symbol = &item["symbol"];
            (
                symbol["name"].as_str().unwrap().to_owned(),
                symbol["kind"].as_str().unwrap().to_owned(),
                symbol["file"].as_str().unwrap().to_owned(),
                symbol["line"].as_u64().unwrap(),
                item["edge_kind"].as_str().unwrap().to_owned(),
                item["depth"].as_u64().unwrap(),
                name_of(&item["parent_node_id"]),
            )
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| (left.5, &left.0).cmp(&(right.5, &right.0)));
    entries
}

async fn call_tool(fixture: &ProductionCompositionFixture, arguments: Value) -> JsonRpcResponse {
    fixture
        .harness
        .call_tool(
            &fixture.project_root,
            "tracedecay_type_hierarchy",
            arguments,
        )
        .await
        .expect("production MCP tools/call")
}

fn tool_text(response: &JsonRpcResponse) -> &str {
    assert!(
        response.error.is_none(),
        "type hierarchy returned a protocol error: {response:?}"
    );
    let result = response
        .result
        .as_ref()
        .unwrap_or_else(|| panic!("type hierarchy returned no result: {response:?}"));
    assert_ne!(
        result.get("isError"),
        Some(&json!(true)),
        "type hierarchy refused: {result}"
    );
    result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("type hierarchy text missing: {result}"))
}

fn assert_refused(response: &JsonRpcResponse, context: &str) {
    let refused_at_parse = response
        .error
        .as_ref()
        .and_then(|error| error.data.as_ref())
        .is_some_and(|data| data["reason_code"] == "application_surface_invalid_request");
    let refused_by_contract = response.result.as_ref().is_some_and(|result| {
        result["isError"] == true && result["problem"]["kind"] == "invalid_request"
    });
    assert!(
        refused_at_parse || refused_by_contract,
        "{context} must be a typed invalid request: {response:?}"
    );
}

async fn symbol_id(
    fixture: &ProductionCompositionFixture,
    name: &str,
    kind: &str,
    file: &str,
) -> String {
    let response = fixture
        .harness
        .call_tool(
            &fixture.project_root,
            "tracedecay_find_exact_symbol",
            json!({"name": name, "limit": 20, "format": "json"}),
        )
        .await
        .expect("production exact-symbol call");
    let payload: Value = serde_json::from_str(tool_text(&response)).expect("exact-symbol JSON");
    payload["matches"]
        .as_array()
        .and_then(|matches| {
            matches
                .iter()
                .find(|item| item["name"] == name && item["kind"] == kind && item["file"] == file)
        })
        .and_then(|item| item["id"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("{kind} {name} in {file} missing from {payload}"))
}
