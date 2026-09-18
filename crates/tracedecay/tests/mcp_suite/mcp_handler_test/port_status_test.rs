//! Behavior of `tracedecay_port_status` through the production MCP `tools/call` path.
//!
//! Line numbers below are the 1-based lines of the fixture sources.

use std::fs;

use serde_json::{Value, json};

use crate::support::{
    ProductionCompositionFixture, extract_text, production_composition_fixture_with_sources,
    wait_for_current_graph,
};

/// `source/biquad.rs`. Default-kind symbols: struct `Biquad` line 1, method
/// `process` line 6, method `reset` line 10, method `gain` line 12, function
/// `helper` line 17, enum `Mode` line 21.
const SOURCE_BIQUAD: &str = "\
pub struct Biquad {
    gain: f64,
}

impl Biquad {
    pub fn process(&self) -> f64 {
        self.gain
    }

    pub fn reset(&self) {}

    pub fn gain(&self) -> f64 {
        self.gain
    }
}

pub fn helper() -> i32 {
    1
}

pub enum Mode {
    Fast,
}
";

/// `target/biquad.ts`. Default-kind symbols: class `Biquad` line 1, method
/// `process` line 2, method `reset` line 6, class `Adaa` line 9, method `gain`
/// line 10, function `Helper` line 15, function `extra` line 19.
const TARGET_BIQUAD: &str = "\
export class Biquad {
  process(): number {
    return 1;
  }

  reset(): void {}
}

export class Adaa {
  gain(): number {
    return 0;
  }
}

export function Helper(): number {
  return 2;
}

export function extra(): void {}
";

async fn open_port_project() -> ProductionCompositionFixture {
    let (_isolated_env, _) = crate::common::IsolatedEnv::acquire().await;
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("source")).unwrap();
        fs::create_dir_all(project.join("target")).unwrap();
        fs::write(project.join("source/biquad.rs"), SOURCE_BIQUAD).unwrap();
        fs::write(project.join("target/biquad.ts"), TARGET_BIQUAD).unwrap();
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production port-status server");
    wait_for_current_graph(&server).await;
    fixture
}

async fn call_port_status(fixture: &ProductionCompositionFixture, mut arguments: Value) -> Value {
    arguments
        .as_object_mut()
        .expect("port_status arguments are an object")
        .insert("format".to_owned(), json!("json"));
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, "tracedecay_port_status", arguments)
        .await
        .expect("production MCP tools/call");
    let result = response.result.unwrap_or_else(|| {
        panic!(
            "tracedecay_port_status failed: {:?}",
            response.error.as_ref().map(|error| &error.message)
        )
    });
    let text = extract_text(&result);
    serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("tracedecay_port_status did not return JSON ({error}): {text}")
    })
}

fn assert_payload(actual: &Value, expected: Value) {
    assert_eq!(
        actual,
        &expected,
        "tracedecay_port_status payload:\n{}",
        serde_json::to_string_pretty(actual).unwrap_or_else(|_| actual.to_string())
    );
}

#[tokio::test]
async fn port_status_reports_cross_language_partial_coverage() {
    let fixture = open_port_project().await;

    // `Biquad` matches across struct/class. `helper` matches `Helper`.
    // `Biquad::gain` does not match `Adaa::gain`. Coverage is 4/6 = 66.7.
    let partial = call_port_status(
        &fixture,
        json!({"source_dir": "source", "target_dir": "target"}),
    )
    .await;
    assert_payload(
        &partial,
        json!({
            "source_dir": "source",
            "target_dir": "target",
            "source_count": 6,
            "target_count": 7,
            "matched": 4,
            "unmatched": 2,
            "target_only": 3,
            "coverage_percent": 66.7,
            "unmatched_by_file": {
                "source/biquad.rs": [
                    {"name": "gain", "kind": "method", "line": 12},
                    {"name": "Mode", "kind": "enum", "line": 21}
                ]
            },
            "matched_symbols": [
                {
                    "name": "Biquad",
                    "source_kind": "struct",
                    "target_kind": "class",
                    "source_file": "source/biquad.rs",
                    "target_file": "target/biquad.ts"
                },
                {
                    "name": "process",
                    "source_kind": "method",
                    "target_kind": "method",
                    "source_file": "source/biquad.rs",
                    "target_file": "target/biquad.ts"
                },
                {
                    "name": "reset",
                    "source_kind": "method",
                    "target_kind": "method",
                    "source_file": "source/biquad.rs",
                    "target_file": "target/biquad.ts"
                },
                {
                    "name": "helper",
                    "source_kind": "function",
                    "target_kind": "function",
                    "source_file": "source/biquad.rs",
                    "target_file": "target/biquad.ts"
                }
            ],
            "target_only_symbols": [
                {"name": "Adaa", "kind": "class", "file": "target/biquad.ts", "line": 9},
                {"name": "gain", "kind": "method", "file": "target/biquad.ts", "line": 10},
                {"name": "extra", "kind": "function", "file": "target/biquad.ts", "line": 19}
            ]
        }),
    );

    // Methods only. Unknown kinds are dropped when one supported kind remains.
    // `Biquad::process` and `Biquad::reset` match; `Adaa::gain` does not.
    let methods = call_port_status(
        &fixture,
        json!({
            "source_dir": "source",
            "target_dir": "target",
            "kinds": ["method", "not_a_kind"]
        }),
    )
    .await;
    assert_payload(
        &methods,
        json!({
            "source_dir": "source",
            "target_dir": "target",
            "source_count": 3,
            "target_count": 3,
            "matched": 2,
            "unmatched": 1,
            "target_only": 1,
            "coverage_percent": 66.7,
            "unmatched_by_file": {
                "source/biquad.rs": [
                    {"name": "gain", "kind": "method", "line": 12}
                ]
            },
            "matched_symbols": [
                {
                    "name": "process",
                    "source_kind": "method",
                    "target_kind": "method",
                    "source_file": "source/biquad.rs",
                    "target_file": "target/biquad.ts"
                },
                {
                    "name": "reset",
                    "source_kind": "method",
                    "target_kind": "method",
                    "source_file": "source/biquad.rs",
                    "target_file": "target/biquad.ts"
                }
            ],
            "target_only_symbols": [
                {"name": "gain", "kind": "method", "file": "target/biquad.ts", "line": 10}
            ]
        }),
    );

    // An empty source side is zero coverage, not an error, and still names
    // every symbol that exists only in the target.
    let missing_source = call_port_status(
        &fixture,
        json!({"source_dir": "nowhere", "target_dir": "target"}),
    )
    .await;
    assert_payload(
        &missing_source,
        json!({
            "source_dir": "nowhere",
            "target_dir": "target",
            "source_count": 0,
            "target_count": 7,
            "matched": 0,
            "unmatched": 0,
            "target_only": 7,
            "coverage_percent": 0.0,
            "unmatched_by_file": {},
            "matched_symbols": [],
            "target_only_symbols": [
                {"name": "Biquad", "kind": "class", "file": "target/biquad.ts", "line": 1},
                {"name": "process", "kind": "method", "file": "target/biquad.ts", "line": 2},
                {"name": "reset", "kind": "method", "file": "target/biquad.ts", "line": 6},
                {"name": "Adaa", "kind": "class", "file": "target/biquad.ts", "line": 9},
                {"name": "gain", "kind": "method", "file": "target/biquad.ts", "line": 10},
                {"name": "Helper", "kind": "function", "file": "target/biquad.ts", "line": 15},
                {"name": "extra", "kind": "function", "file": "target/biquad.ts", "line": 19}
            ]
        }),
    );

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn port_status_rejects_unknown_kinds_and_missing_source_dir() {
    let fixture = open_port_project().await;

    let unknown_kind = tool_error(
        &fixture,
        json!({"source_dir": "source", "target_dir": "target", "kinds": ["not_a_kind"]}),
    )
    .await;
    assert_eq!(unknown_kind.0, -32603);
    assert_eq!(
        unknown_kind.1,
        "tool execution failed: config error: invalid parameter: kinds must contain at least one supported node kind"
    );
    assert_eq!(unknown_kind.2, "tracedecay_port_status");

    let missing_source_dir = tool_error(&fixture, json!({"target_dir": "target"})).await;
    assert_eq!(missing_source_dir.0, -32603);
    assert_eq!(
        missing_source_dir.1,
        "tool execution failed: config error: invalid arguments for tracedecay_port_status: missing field `source_dir`"
    );
    assert_eq!(missing_source_dir.2, "tracedecay_port_status");

    fixture.harness.shutdown().await;
}

async fn tool_error(
    fixture: &ProductionCompositionFixture,
    mut arguments: Value,
) -> (i32, String, String) {
    arguments
        .as_object_mut()
        .expect("port_status arguments are an object")
        .insert("format".to_owned(), json!("json"));
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, "tracedecay_port_status", arguments)
        .await
        .expect("production MCP tools/call");
    assert!(
        response.result.is_none(),
        "invalid port_status input must not return a result: {:?}",
        response.result
    );
    let error = response
        .error
        .expect("invalid port_status input must return a JSON-RPC error");
    let tool = error
        .data
        .as_ref()
        .and_then(|data| data.get("tool"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    (error.code, error.message, tool)
}
