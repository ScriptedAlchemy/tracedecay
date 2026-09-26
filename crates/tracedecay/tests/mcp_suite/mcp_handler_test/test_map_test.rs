//! Production MCP behavior of `tracedecay_test_map`.
//!
//! These tests call the tool the way a host does: a JSON-RPC `tools/call` on
//! the production composition, after the code graph is current. Occurrence
//! ids digest the temporary repository path, so they are not stable literals.
//! The mapping a caller can read (name, file, line, test, depth) is.

#![cfg(feature = "test-transport")]

use std::path::Path;

use serde_json::{Value, json};
use tracedecay_mcp::jsonrpc::JsonRpcResponse;

use crate::support::{
    ProductionCompositionFixture, extract_first_json_content,
    production_composition_fixture_with_sources, wait_for_current_graph,
};

const LIB_RS: &str = "\
pub fn greet(name: &str) -> String { format!(\"hello {name}\") }\n\
\n\
pub struct Marker;\n\
\n\
pub fn unused() -> i32 { 0 }\n\
\n\
fn hop2() -> i32 { 1 }\n\
\n\
fn hop1() -> i32 { hop2() }\n\
\n\
pub fn surface() -> i32 { hop1() }\n\
\n\
pub mod deep;\n\
";

const DEEP_RS: &str = "\
pub fn buried() -> i32 { 1 }\n\
\n\
fn relay() -> i32 { buried() }\n\
\n\
fn shuttle() -> i32 { relay() }\n\
\n\
pub fn doorway() -> i32 { shuttle() }\n\
";

const BEHAVIOR_RS: &str = "\
use test_map_probe::{greet, surface};\n\
\n\
#[test]\n\
fn covers_greet() {\n\
    let _ = greet(\"a\");\n\
}\n\
\n\
#[test]\n\
fn covers_surface() {\n\
    let _ = surface();\n\
}\n\
";

const DOORWAY_RS: &str = "\
use test_map_probe::deep::doorway;\n\
\n\
#[test]\n\
fn covers_doorway() {\n\
    let _ = doorway();\n\
}\n\
";

fn write_probe(project: &Path) {
    std::fs::create_dir_all(project.join("src")).expect("probe src");
    std::fs::create_dir_all(project.join("tests")).expect("probe tests");
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"test_map_probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("probe manifest");
    std::fs::write(project.join("src/lib.rs"), LIB_RS).expect("probe lib");
    std::fs::write(project.join("src/deep.rs"), DEEP_RS).expect("probe deep");
    std::fs::write(project.join("tests/behavior.rs"), BEHAVIOR_RS).expect("probe behavior");
    std::fs::write(project.join("tests/doorway.rs"), DOORWAY_RS).expect("probe doorway");
}

async fn open_probe() -> ProductionCompositionFixture {
    let fixture = production_composition_fixture_with_sources(write_probe).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");
    wait_for_current_graph(&server).await;
    fixture
}

async fn call_test_map(
    fixture: &ProductionCompositionFixture,
    mut arguments: Value,
) -> JsonRpcResponse {
    arguments
        .as_object_mut()
        .expect("test-map arguments")
        .entry("format".to_owned())
        .or_insert_with(|| json!("json"));
    fixture
        .harness
        .call_tool(&fixture.project_root, "tracedecay_test_map", arguments)
        .await
        .expect("production MCP accepted tools/call for tracedecay_test_map")
}

fn success_payload(response: &JsonRpcResponse) -> Value {
    assert!(
        response.error.is_none(),
        "tracedecay_test_map failed: {:?}",
        response.error
    );
    let result = response
        .result
        .as_ref()
        .expect("successful tools/call result");
    extract_first_json_content(result)
}

fn failure(response: &JsonRpcResponse) -> Value {
    let error = response
        .error
        .as_ref()
        .unwrap_or_else(|| panic!("expected a tool error, got {:?}", response.result));
    json!({
        "code": error.code,
        "message": error.message,
        "data": error.data,
    })
}

/// Drop occurrence ids, then sort rows. Identity digests include the fixture
/// path, and the handler emits rows in digest order, so neither is a literal
/// a caller can predict. The remaining object is the mapping contract.
fn comparable_map(payload: &Value) -> Value {
    let mut payload = payload.clone();
    let coverage = payload["coverage"].as_array_mut().expect("coverage array");
    for row in coverage.iter_mut() {
        let row = row.as_object_mut().expect("coverage row");
        row.remove("source_id");
        let tests = row
            .get_mut("tests")
            .and_then(Value::as_array_mut)
            .expect("tests array");
        tests.sort_by(|left, right| {
            left["test_name"]
                .as_str()
                .unwrap_or("")
                .cmp(right["test_name"].as_str().unwrap_or(""))
                .then(
                    left["attribution_depth"]
                        .as_u64()
                        .cmp(&right["attribution_depth"].as_u64()),
                )
        });
    }
    coverage.sort_by(|left, right| {
        left["source_name"]
            .as_str()
            .unwrap_or("")
            .cmp(right["source_name"].as_str().unwrap_or(""))
    });
    let uncovered = payload["uncovered"]
        .as_array_mut()
        .expect("uncovered array");
    for row in uncovered.iter_mut() {
        row.as_object_mut().expect("uncovered row").remove("id");
    }
    uncovered.sort_by(|left, right| {
        left["name"]
            .as_str()
            .unwrap_or("")
            .cmp(right["name"].as_str().unwrap_or(""))
    });
    payload
}

fn symbol_id(payload: &Value, bucket: &str, name_key: &str, name: &str, id_key: &str) -> String {
    payload[bucket]
        .as_array()
        .expect("symbol bucket")
        .iter()
        .find(|row| row[name_key].as_str() == Some(name))
        .unwrap_or_else(|| panic!("{name} missing from {bucket}: {payload}"))
        .get(id_key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{name} omitted {id_key}: {payload}"))
        .to_owned()
}

fn surface_mapping() -> Value {
    json!({
        "covered_symbols": 4,
        "uncovered_symbols": 1,
        "test_files": ["tests/behavior.rs"],
        "coverage": [
            {
                "source_name": "greet",
                "source_file": "src/lib.rs",
                "source_line": 1,
                "tests": [{
                    "test_name": "covers_greet",
                    "test_file": "tests/behavior.rs",
                    "test_line": 4,
                    "attribution_depth": 1
                }]
            },
            {
                "source_name": "hop1",
                "source_file": "src/lib.rs",
                "source_line": 9,
                "tests": [{
                    "test_name": "covers_surface",
                    "test_file": "tests/behavior.rs",
                    "test_line": 9,
                    "attribution_depth": 2
                }]
            },
            {
                "source_name": "hop2",
                "source_file": "src/lib.rs",
                "source_line": 7,
                "tests": [{
                    "test_name": "covers_surface",
                    "test_file": "tests/behavior.rs",
                    "test_line": 9,
                    "attribution_depth": 3
                }]
            },
            {
                "source_name": "surface",
                "source_file": "src/lib.rs",
                "source_line": 11,
                "tests": [{
                    "test_name": "covers_surface",
                    "test_file": "tests/behavior.rs",
                    "test_line": 9,
                    "attribution_depth": 1
                }]
            }
        ],
        "uncovered": [{
            "name": "unused",
            "file": "src/lib.rs",
            "line": 5
        }]
    })
}

#[tokio::test]
async fn test_map_reports_literal_coverage_and_typed_refusals() {
    let fixture = open_probe().await;
    let mapped = success_payload(&call_test_map(&fixture, json!({"file": "src/lib.rs"})).await);
    assert_eq!(comparable_map(&mapped), surface_mapping());

    let greet_id = symbol_id(&mapped, "coverage", "source_name", "greet", "source_id");
    let unused_id = symbol_id(&mapped, "uncovered", "name", "unused", "id");
    assert_ne!(greet_id, unused_id);

    let by_node = success_payload(&call_test_map(&fixture, json!({"node_id": greet_id})).await);
    assert_eq!(by_node["coverage"][0]["source_id"], greet_id);
    assert_eq!(
        comparable_map(&by_node),
        json!({
            "covered_symbols": 1,
            "uncovered_symbols": 0,
            "test_files": ["tests/behavior.rs"],
            "coverage": [surface_mapping()["coverage"][0].clone()],
            "uncovered": []
        })
    );

    let by_alias = success_payload(&call_test_map(&fixture, json!({"id": greet_id})).await);
    assert_eq!(by_alias["coverage"][0]["source_id"], greet_id);
    assert_eq!(
        comparable_map(&by_alias),
        json!({
            "covered_symbols": 1,
            "uncovered_symbols": 0,
            "test_files": ["tests/behavior.rs"],
            "coverage": [surface_mapping()["coverage"][0].clone()],
            "uncovered": []
        })
    );

    let by_unused = success_payload(&call_test_map(&fixture, json!({"node_id": unused_id})).await);
    assert_eq!(by_unused["uncovered"][0]["id"], unused_id);
    assert_eq!(
        comparable_map(&by_unused),
        json!({
            "covered_symbols": 0,
            "uncovered_symbols": 1,
            "test_files": [],
            "coverage": [],
            "uncovered": [surface_mapping()["uncovered"][0].clone()]
        })
    );

    assert_eq!(
        success_payload(&call_test_map(&fixture, json!({"file": "src/missing.rs"})).await,),
        json!({
            "covered_symbols": 0,
            "uncovered_symbols": 0,
            "test_files": [],
            "coverage": [],
            "uncovered": []
        })
    );
    assert_eq!(
        success_payload(
            &call_test_map(&fixture, json!({"node_id": "missing-test-map-symbol"})).await,
        ),
        json!({
            "covered_symbols": 0,
            "uncovered_symbols": 0,
            "test_files": [],
            "coverage": [],
            "uncovered": []
        })
    );

    assert_eq!(
        failure(&call_test_map(&fixture, json!({})).await),
        json!({
            "code": -32602,
            "message": "missing required parameter: 'file' or 'node_id'",
            "data": {
                "tool": "tracedecay_test_map",
                "reason_code": "missing_required_parameter",
                "retryable": false,
                "detail": "missing required parameter: 'file' or 'node_id'"
            }
        })
    );
    assert_eq!(
        failure(&call_test_map(&fixture, json!({"node_id": " lead"})).await),
        json!({
            "code": -32603,
            "message": "tool execution failed: config error: invalid test-map symbol occurrence: SymbolOccurrenceId is not canonical",
            "data": {
                "tool": "tracedecay_test_map",
                "cli_fallback": "This tool is also available from the shell: `tracedecay tool test_map ...` (`tracedecay tool test_map --help` for parameters). If MCP calls keep failing or timing out, fall back to that CLI instead of querying .tracedecay databases directly."
            }
        })
    );

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn test_map_refuses_when_the_test_is_beyond_depth_three() {
    // `doorway -> shuttle -> relay -> buried` plus the test that calls
    // `doorway` is four caller hops. The walk keeps the fourth hop so it can
    // tell a real depth-3 attribution from a truncated one, and refuses the
    // whole call instead of reporting `buried` as uncovered.
    let fixture = open_probe().await;
    assert_eq!(
        failure(&call_test_map(&fixture, json!({"file": "src/deep.rs"})).await),
        json!({
            "code": -32602,
            "message": "tool project route failed: reason_code=verified-test-evidence-unavailable retryable=false: verified test-map caller expansion exceeded its budget",
            "data": {
                "tool": "tracedecay_test_map",
                "reason_code": "verified-test-evidence-unavailable",
                "retryable": false,
                "detail": "verified test-map caller expansion exceeded its budget"
            }
        })
    );
    fixture.harness.shutdown().await;
}

const SHARED_RS: &str = "\
pub fn fan_alpha() -> i32 { 1 }\n\
\n\
pub fn fan_beta() -> i32 { 2 }\n\
\n\
pub fn fan_gamma() -> i32 { 3 }\n\
";

/// Four tests, each calling all three shared functions.
const FAN_IN_RS: &str = "\
use fan_in_probe::shared::{fan_alpha, fan_beta, fan_gamma};\n\
\n\
#[test]\n\
fn first() { fan_alpha(); fan_beta(); fan_gamma(); }\n\
\n\
#[test]\n\
fn second() { fan_alpha(); fan_beta(); fan_gamma(); }\n\
\n\
#[test]\n\
fn third() { fan_alpha(); fan_beta(); fan_gamma(); }\n\
\n\
#[test]\n\
fn fourth() { fan_alpha(); fan_beta(); fan_gamma(); }\n\
";

fn write_fan_in_probe(project: &Path) {
    std::fs::create_dir_all(project.join("src")).expect("probe src");
    std::fs::create_dir_all(project.join("tests")).expect("probe tests");
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"fan_in_probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("probe manifest");
    std::fs::write(project.join("src/lib.rs"), "pub mod shared;\n").expect("probe lib");
    std::fs::write(project.join("src/shared.rs"), SHARED_RS).expect("probe shared");
    std::fs::write(project.join("tests/fan_in.rs"), FAN_IN_RS).expect("probe tests");
}

/// `(graph point reads, adjacency queries, adjacency rows)` from the call's
/// `tracedecay_cost` trailer.
fn read_cost(response: &JsonRpcResponse) -> (u64, u64, u64) {
    let result = response
        .result
        .as_ref()
        .expect("successful tools/call result");
    let trailer = result["content"]
        .as_array()
        .expect("content blocks")
        .iter()
        .filter_map(|block| block["text"].as_str())
        .find_map(|text| text.strip_prefix("\ntracedecay_cost: "))
        .unwrap_or_else(|| panic!("no cost trailer: {result}"));
    let field = |name: &str| -> u64 {
        trailer
            .split(' ')
            .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
            .and_then(|value| value.parse().ok())
            .unwrap_or_else(|| panic!("{name} missing from {trailer:?}"))
    };
    (
        field("graph_sealed_reads") + field("graph_staging_reads"),
        field("adjacency_queries"),
        field("adjacency_rows"),
    )
}

/// Mapping M tests over N symbols reads each reached symbol once, however
/// many sources a test covers: the caller walk is batched across the file's
/// symbols, and each fan-out row costs exactly one edge read, so point reads
/// beyond the rows are symbol reads. Walking each source separately reads
/// every test once per source it calls.
#[tokio::test]
async fn test_map_reads_each_test_once_across_the_symbols_it_covers() {
    let fixture = production_composition_fixture_with_sources(write_fan_in_probe).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");
    wait_for_current_graph(&server).await;

    let response = call_test_map(&fixture, json!({"file": "src/shared.rs"})).await;
    let mapped = success_payload(&response);
    assert_eq!(mapped["covered_symbols"], 3, "{mapped}");
    assert_eq!(
        mapped["coverage"]
            .as_array()
            .expect("coverage rows")
            .iter()
            .map(|row| row["tests"].as_array().map_or(0, Vec::len))
            .collect::<Vec<_>>(),
        [4, 4, 4],
        "{mapped}"
    );
    // Three batched fan-outs: callers of the three sources (twelve call
    // rows), callers of the four tests (their four `#[test]` annotations),
    // and the annotation check over all seven reached symbols (those sixteen
    // rows again). The eight symbol reads are the four tests and their four
    // markers, once each; a walk per source would read each test three times.
    let (point_reads, adjacency_queries, adjacency_rows) = read_cost(&response);
    assert_eq!(
        (
            point_reads - adjacency_rows,
            adjacency_queries,
            adjacency_rows
        ),
        (8, 3, 32),
        "each reached symbol is read once"
    );

    fixture.harness.shutdown().await;
}
