//! Production MCP behavior of `tracedecay_type_hierarchy`.
//!
//! The tool walks incoming `implements` and `extends` edges only. `max_depth`
//! counts those edges, not the root, and a node already rendered is not
//! rendered again.

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

const NAMED_TREE: &str = "\
Named (interface) -- src/animals.ts:1
|- implements AnimalBase (class) -- src/animals.ts:4
  |- extends Cat (class) -- src/animals.ts:7
";

const NAMED_DEPTH_ONE_TREE: &str = "\
Named (interface) -- src/animals.ts:1
|- implements AnimalBase (class) -- src/animals.ts:4
";

const NAMED_ROOT_ONLY: &str = "Named (interface) -- src/animals.ts:1\n";

const CAT_TREE: &str = "Cat (class) -- src/animals.ts:7\n";

const SPEAKER_TREE: &str = "\
Speaker (trait) -- src/lib.rs:1
|- implements Person (impl) -- src/lib.rs:7
";

const CALLER_TREE: &str = "caller (function) -- src/lib.rs:13\n";

const LOOP_LEFT_TREE: &str = "\
LoopLeft (interface) -- src/cycle.ts:1
|- extends LoopRight (interface) -- src/cycle.ts:4
";

const NAMED_MARKDOWN: &str = "\
## Type Hierarchy
**root:** Named (interface) - src/animals.ts:1
**max_depth:** 5

```text
Named (interface) -- src/animals.ts:1
|- implements AnimalBase (class) -- src/animals.ts:4
  |- extends Cat (class) -- src/animals.ts:7
```
";

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
    assert_eq!(tool_text(&markdown), NAMED_MARKDOWN);

    assert_eq!(
        tool_json(&call_tool(&fixture, json!({"node_id": named, "format": "json"})).await),
        hierarchy_json(
            &named,
            "Named",
            "interface",
            "src/animals.ts",
            1,
            5,
            NAMED_TREE
        )
    );
    assert_eq!(
        tool_json(
            &call_tool(
                &fixture,
                json!({"id": named, "format": "json", "max_depth": 5}),
            )
            .await
        ),
        hierarchy_json(
            &named,
            "Named",
            "interface",
            "src/animals.ts",
            1,
            5,
            NAMED_TREE
        )
    );
    assert_eq!(
        tool_json(
            &call_tool(
                &fixture,
                json!({"node_id": named, "format": "json", "max_depth": 1}),
            )
            .await
        ),
        hierarchy_json(
            &named,
            "Named",
            "interface",
            "src/animals.ts",
            1,
            1,
            NAMED_DEPTH_ONE_TREE
        )
    );
    assert_eq!(
        tool_json(
            &call_tool(
                &fixture,
                json!({"node_id": named, "format": "json", "max_depth": 0}),
            )
            .await
        ),
        hierarchy_json(
            &named,
            "Named",
            "interface",
            "src/animals.ts",
            1,
            0,
            NAMED_ROOT_ONLY
        )
    );
    assert_eq!(
        tool_json(
            &call_tool(
                &fixture,
                json!({"node_id": named, "format": "json", "max_depth": 11}),
            )
            .await
        ),
        hierarchy_json(
            &named,
            "Named",
            "interface",
            "src/animals.ts",
            1,
            10,
            NAMED_TREE
        )
    );
    assert_eq!(
        tool_json(
            &call_tool(
                &fixture,
                json!({"node_id": named, "format": "json", "max_depth": -3}),
            )
            .await
        ),
        hierarchy_json(
            &named,
            "Named",
            "interface",
            "src/animals.ts",
            1,
            5,
            NAMED_TREE
        )
    );
    assert_eq!(
        tool_json(
            &call_tool(
                &fixture,
                json!({"node_id": named, "format": "json", "max_depth": "1"}),
            )
            .await
        ),
        hierarchy_json(
            &named,
            "Named",
            "interface",
            "src/animals.ts",
            1,
            5,
            NAMED_TREE
        )
    );
    assert_eq!(
        tool_json(&call_tool(&fixture, json!({"node_id": cat, "format": "json"})).await),
        hierarchy_json(&cat, "Cat", "class", "src/animals.ts", 7, 5, CAT_TREE)
    );
    assert_eq!(
        tool_json(&call_tool(&fixture, json!({"node_id": speaker, "format": "json"})).await),
        hierarchy_json(
            &speaker,
            "Speaker",
            "trait",
            "src/lib.rs",
            1,
            5,
            SPEAKER_TREE
        )
    );
    assert_eq!(
        tool_json(&call_tool(&fixture, json!({"node_id": caller, "format": "json"})).await),
        hierarchy_json(
            &caller,
            "caller",
            "function",
            "src/lib.rs",
            13,
            5,
            CALLER_TREE
        )
    );
    assert_eq!(
        tool_json(&call_tool(&fixture, json!({"node_id": loop_left, "format": "json"})).await),
        hierarchy_json(
            &loop_left,
            "LoopLeft",
            "interface",
            "src/cycle.ts",
            1,
            5,
            LOOP_LEFT_TREE
        )
    );

    assert_protocol_error(
        &call_tool(&fixture, json!({"format": "json"})).await,
        "config error: missing required parameter: node_id",
    );
    assert_protocol_error(
        &call_tool(&fixture, json!({"node_id": "   ", "format": "json"})).await,
        "config error: invalid parameter: node_id must not be empty",
    );
    assert_protocol_error(
        &call_tool(
            &fixture,
            json!({"node_id": "not canonical id", "format": "json"}),
        )
        .await,
        "config error: invalid node_id 'not canonical id': SymbolOccurrenceId is not canonical",
    );
    assert_protocol_error(
        &call_tool(
            &fixture,
            json!({"node_id": "absent-symbol", "format": "json"}),
        )
        .await,
        "config error: node not found in verified generation: absent-symbol",
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

fn hierarchy_json(
    id: &str,
    name: &str,
    kind: &str,
    file: &str,
    line: u32,
    max_depth: u64,
    tree: &str,
) -> Value {
    json!({
        "root": {
            "id": id,
            "name": name,
            "kind": kind,
            "file": file,
            "line": line,
        },
        "max_depth": max_depth,
        "tree": tree,
    })
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
    let result = success_result(response);
    result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("type hierarchy text missing: {result}"))
}

fn tool_json(response: &JsonRpcResponse) -> Value {
    serde_json::from_str(tool_text(response))
        .unwrap_or_else(|error| panic!("type hierarchy JSON did not parse ({error}): {response:?}"))
}

fn success_result(response: &JsonRpcResponse) -> &Value {
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
    result
}

fn assert_protocol_error(response: &JsonRpcResponse, message: &str) {
    assert!(
        response.result.is_none(),
        "refused type hierarchy must not carry a result: {response:?}"
    );
    let error = response
        .error
        .as_ref()
        .unwrap_or_else(|| panic!("missing protocol error: {response:?}"));
    assert_eq!(error.code, -32603);
    assert_eq!(error.message, message);
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
    let payload = tool_json(&response);
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
