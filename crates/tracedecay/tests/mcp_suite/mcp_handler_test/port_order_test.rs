//! Behavior of `tracedecay_port_order` through the production MCP `tools/call` path.
//!
//! Line numbers below are the 1-based lines of the fixture sources. Level
//! descriptions use the handler's en dash (U+2013), not a hyphen.

use std::fs;

use serde_json::{Value, json};

use crate::support::{
    ProductionCompositionFixture, extract_json, production_composition_fixture_with_sources,
    wait_for_current_graph,
};

/// `order/chain.rs`. Functions only: `leaf` line 1, `zeta` line 5, `alpha`
/// line 7, `mid` line 9 (`leaf()`), `top` line 13 (`mid()`).
const ORDER_CHAIN: &str = "\
pub fn leaf() -> i32 {
    1
}

pub fn zeta() {}

pub fn alpha() {}

pub fn mid() -> i32 {
    leaf()
}

pub fn top() -> i32 {
    mid()
}
";

/// `cycle/scc.rs`. One SCC: `alpha` line 1, `beta` line 6, `gamma` line 10,
/// `hub` line 15.
///
/// Edges: alpha→beta, alpha→hub, beta→gamma, gamma→alpha, gamma→hub, hub→alpha.
const CYCLE_SCC: &str = "\
pub fn alpha() {
    beta();
    hub();
}

pub fn beta() {
    gamma();
}

pub fn gamma() {
    alpha();
    hub();
}

pub fn hub() {
    alpha();
}
";

/// `tied/leaves.rs`. Three independent functions, lines 1–3.
const TIED_LEAVES: &str = "\
pub fn zeta() {}
pub fn alpha() {}
pub fn middle() {}
";

const LEVEL_0: &str = "No internal dependencies. Port these first";
const LEVEL_1: &str = "Depends only on levels 0\u{2013}0";
const LEVEL_2: &str = "Depends only on levels 0\u{2013}1";
const CYCLE_NOTE: &str = "Mutual dependency. Port together, starting at `entry_point` and refactoring `break_point_candidate` to split the cycle.";
const BREAK_RATIONALE: &str = "Highest in-cycle in-degree. Refactoring its callers is the most effective way to fragment this SCC.";

async fn open_port_order_project() -> ProductionCompositionFixture {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("order")).unwrap();
        fs::create_dir_all(project.join("cycle")).unwrap();
        fs::create_dir_all(project.join("tied")).unwrap();
        fs::write(project.join("order/chain.rs"), ORDER_CHAIN).unwrap();
        fs::write(project.join("cycle/scc.rs"), CYCLE_SCC).unwrap();
        fs::write(project.join("tied/leaves.rs"), TIED_LEAVES).unwrap();
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production port-order server");
    wait_for_current_graph(&server).await;
    fixture
}

async fn call_port_order(fixture: &ProductionCompositionFixture, mut arguments: Value) -> Value {
    arguments
        .as_object_mut()
        .expect("port_order arguments are an object")
        .insert("format".to_owned(), json!("json"));
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, "tracedecay_port_order", arguments)
        .await
        .expect("production MCP tools/call");
    let result = response.result.unwrap_or_else(|| {
        panic!(
            "tracedecay_port_order failed: {:?}",
            response.error.as_ref().map(|error| &error.message)
        )
    });
    extract_json(&result)
}

fn assert_payload(actual: &Value, expected: Value) {
    assert_eq!(
        actual,
        &expected,
        "tracedecay_port_order payload:\n{}",
        serde_json::to_string_pretty(actual).unwrap_or_else(|_| actual.to_string())
    );
}

fn ordered_chain() -> Value {
    json!({
        "source_dir": "order",
        "total_symbols": 5,
        "returned": 5,
        "levels": [
            {
                "level": 0,
                "description": LEVEL_0,
                "symbols": [
                    {"name": "leaf", "kind": "function", "file": "order/chain.rs", "line": 1},
                    {"name": "zeta", "kind": "function", "file": "order/chain.rs", "line": 5},
                    {"name": "alpha", "kind": "function", "file": "order/chain.rs", "line": 7}
                ]
            },
            {
                "level": 1,
                "description": LEVEL_1,
                "symbols": [
                    {
                        "name": "mid",
                        "kind": "function",
                        "file": "order/chain.rs",
                        "line": 9,
                        "depends_on": ["leaf"]
                    }
                ]
            },
            {
                "level": 2,
                "description": LEVEL_2,
                "symbols": [
                    {
                        "name": "top",
                        "kind": "function",
                        "file": "order/chain.rs",
                        "line": 13,
                        "depends_on": ["mid"]
                    }
                ]
            }
        ],
        "cycles": []
    })
}

#[tokio::test]
async fn port_order_ports_leaves_first_and_reports_one_scc() {
    let (_isolated_env, _) = crate::common::IsolatedEnv::acquire().await;
    let fixture = open_port_order_project().await;

    // `mid` calls `leaf`; `top` calls `mid`. Leaves share level 0 in source order.
    let chain = call_port_order(
        &fixture,
        json!({"source_dir": "order", "kinds": ["function"]}),
    )
    .await;
    assert_payload(&chain, ordered_chain());

    // Unknown kinds are dropped when one supported kind remains. Default
    // kinds on a function-only file are the same payload.
    let mixed = call_port_order(
        &fixture,
        json!({"source_dir": "order", "kinds": ["function", "not_a_kind"]}),
    )
    .await;
    assert_payload(&mixed, ordered_chain());
    let defaults = call_port_order(&fixture, json!({"source_dir": "order"})).await;
    assert_payload(&defaults, ordered_chain());

    // Limit is applied after the tied level is sorted by file and line.
    // The omitted function is acyclic, so it is not reported as a cycle.
    let limited = call_port_order(
        &fixture,
        json!({"source_dir": "tied", "kinds": ["function"], "limit": 2}),
    )
    .await;
    assert_payload(
        &limited,
        json!({
            "source_dir": "tied",
            "total_symbols": 3,
            "returned": 2,
            "levels": [
                {
                    "level": 0,
                    "description": LEVEL_0,
                    "symbols": [
                        {"name": "zeta", "kind": "function", "file": "tied/leaves.rs", "line": 1},
                        {"name": "alpha", "kind": "function", "file": "tied/leaves.rs", "line": 2}
                    ]
                }
            ],
            "cycles": []
        }),
    );

    // In-cycle out-degree ascending, then in-degree descending:
    // hub (1, 2), beta (1, 1), alpha (2, 2), gamma (2, 1).
    // `hub` is the entry. `alpha` is the last node tied for highest in-degree.
    let cycle = call_port_order(
        &fixture,
        json!({"source_dir": "cycle", "kinds": ["function"]}),
    )
    .await;
    assert_payload(
        &cycle,
        json!({
            "source_dir": "cycle",
            "total_symbols": 4,
            "returned": 0,
            "levels": [],
            "cycles": [
                {
                    "size": 4,
                    "files": [
                        {"file": "cycle/scc.rs", "members_in_cycle": 4}
                    ],
                    "symbols": [
                        {
                            "name": "hub",
                            "kind": "function",
                            "file": "cycle/scc.rs",
                            "line": 15,
                            "in_cycle_out_degree": 1,
                            "in_cycle_in_degree": 2
                        },
                        {
                            "name": "beta",
                            "kind": "function",
                            "file": "cycle/scc.rs",
                            "line": 6,
                            "in_cycle_out_degree": 1,
                            "in_cycle_in_degree": 1
                        },
                        {
                            "name": "alpha",
                            "kind": "function",
                            "file": "cycle/scc.rs",
                            "line": 1,
                            "in_cycle_out_degree": 2,
                            "in_cycle_in_degree": 2
                        },
                        {
                            "name": "gamma",
                            "kind": "function",
                            "file": "cycle/scc.rs",
                            "line": 10,
                            "in_cycle_out_degree": 2,
                            "in_cycle_in_degree": 1
                        }
                    ],
                    "entry_point": {
                        "name": "hub",
                        "file": "cycle/scc.rs",
                        "line": 15
                    },
                    "break_point_candidate": {
                        "name": "alpha",
                        "file": "cycle/scc.rs",
                        "line": 1,
                        "rationale": BREAK_RATIONALE
                    },
                    "note": CYCLE_NOTE
                }
            ]
        }),
    );

    let empty = call_port_order(&fixture, json!({"source_dir": "missing"})).await;
    assert_payload(
        &empty,
        json!({
            "source_dir": "missing",
            "total_symbols": 0,
            "returned": 0,
            "levels": [],
            "cycles": []
        }),
    );

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn port_order_rejects_unknown_kinds_and_missing_source_dir() {
    let (_isolated_env, _) = crate::common::IsolatedEnv::acquire().await;
    let fixture = open_port_order_project().await;

    let unknown_kind = tool_error(
        &fixture,
        json!({"source_dir": "order", "kinds": ["not_a_kind"]}),
    )
    .await;
    assert_eq!(unknown_kind.0, -32603);
    assert_eq!(
        unknown_kind.1,
        "tool execution failed: config error: invalid parameter: kinds must contain at least one supported node kind"
    );
    assert_eq!(unknown_kind.2, "tracedecay_port_order");

    let missing_source_dir = tool_error(&fixture, json!({})).await;
    assert_eq!(missing_source_dir.0, -32603);
    assert_eq!(
        missing_source_dir.1,
        "tool execution failed: config error: invalid arguments for tracedecay_port_order: missing field `source_dir`"
    );
    assert_eq!(missing_source_dir.2, "tracedecay_port_order");

    fixture.harness.shutdown().await;
}

async fn tool_error(
    fixture: &ProductionCompositionFixture,
    mut arguments: Value,
) -> (i32, String, String) {
    arguments
        .as_object_mut()
        .expect("port_order arguments are an object")
        .insert("format".to_owned(), json!("json"));
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, "tracedecay_port_order", arguments)
        .await
        .expect("production MCP tools/call");
    assert!(
        response.result.is_none(),
        "invalid port_order input must not return a result: {:?}",
        response.result
    );
    let error = response
        .error
        .expect("invalid port_order input must return a JSON-RPC error");
    let tool = error
        .data
        .as_ref()
        .and_then(|data| data.get("tool"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    (error.code, error.message, tool)
}
