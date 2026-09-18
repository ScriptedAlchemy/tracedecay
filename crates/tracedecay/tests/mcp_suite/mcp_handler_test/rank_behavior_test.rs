#![cfg(feature = "test-transport")]

//! `tracedecay_rank` through the production MCP `tools/call` path.
//!
//! Counts below are the edges written in the fixture source. Equal counts are
//! not ordered against each other: the handler breaks those ties by occurrence
//! id, which is not part of the ranking a caller asked for.

use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;

use crate::common::IsolatedEnv;
use crate::support::{
    ProductionCompositionFixture, handle_real_server_tool_call_raw,
    production_composition_fixture_with_sources, wait_for_current_graph,
};

/// `shared` is called by `left` and `right`. `left` is called only by `right`.
/// Incoming calls: shared 2, left 1, right 0. Outgoing calls: right 2, left 1,
/// shared 0.
const SCOPED_CALLS: &str = "\
pub fn shared() {}

pub fn left() {
    shared();
}

pub fn right() {
    shared();
    left();
}

pub struct Ignored;
";

/// A second call pair outside `src/scoped`. If path filtering is ignored,
/// `noise_target` shows up in a scoped ranking.
const ELSEWHERE_NOISE: &str = "\
pub fn noise_target() {}

pub fn noise_caller() {
    noise_target();
}
";

/// Circle implements Draw and Paint. Square implements only Draw.
const SHAPE_TRAITS: &str = "\
pub trait Draw {}
pub trait Paint {}

pub struct Circle;
impl Draw for Circle {}
impl Paint for Circle {}

pub struct Square;
impl Draw for Square {}
";

struct RankSession {
    _isolated_env: IsolatedEnv,
    fixture: ProductionCompositionFixture,
    server: Arc<McpServer>,
}

fn write_rank_sources(project: &std::path::Path) {
    fs::create_dir_all(project.join("src/scoped")).unwrap();
    fs::create_dir_all(project.join("src/elsewhere")).unwrap();
    fs::create_dir_all(project.join("src/shapes")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"rank_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "mod elsewhere;\nmod scoped;\nmod shapes;\n",
    )
    .unwrap();
    fs::write(project.join("src/scoped/mod.rs"), "mod calls;\n").unwrap();
    fs::write(project.join("src/scoped/calls.rs"), SCOPED_CALLS).unwrap();
    fs::write(project.join("src/elsewhere/mod.rs"), "mod noise;\n").unwrap();
    fs::write(project.join("src/elsewhere/noise.rs"), ELSEWHERE_NOISE).unwrap();
    fs::write(project.join("src/shapes/mod.rs"), "mod traits;\n").unwrap();
    fs::write(project.join("src/shapes/traits.rs"), SHAPE_TRAITS).unwrap();
}

async fn open_rank_session() -> RankSession {
    let (isolated_env, _) = IsolatedEnv::acquire().await;
    let fixture = production_composition_fixture_with_sources(write_rank_sources).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production rank server");
    wait_for_current_graph(&server).await;
    RankSession {
        _isolated_env: isolated_env,
        fixture,
        server,
    }
}

fn ranking_rows(payload: &Value) -> Vec<Value> {
    payload["ranking"]
        .as_array()
        .unwrap_or_else(|| panic!("ranking must be an array: {payload}"))
        .iter()
        .map(|row| {
            json!({
                "name": row["name"],
                "kind": row["kind"],
                "file": row["file"],
                "line": row["line"],
                "count": row["count"],
            })
        })
        .collect()
}

fn assert_rank(
    payload: &Value,
    edge_kind: &str,
    direction: &str,
    node_kind: Option<&str>,
    rows: &[Value],
) {
    assert_eq!(payload["edge_kind"], edge_kind, "{payload}");
    assert_eq!(payload["direction"], direction, "{payload}");
    match node_kind {
        Some(kind) => assert_eq!(payload["node_kind_filter"], kind, "{payload}"),
        None => assert_eq!(payload["node_kind_filter"], Value::Null, "{payload}"),
    }
    assert_eq!(payload["result_count"], rows.len(), "{payload}");
    assert_eq!(ranking_rows(payload), rows, "{payload}");
}

fn assert_counts_and_descending_order(payload: &Value, expected: &[(&str, u64)]) {
    let rows = ranking_rows(payload);
    let mut counts = BTreeMap::new();
    let mut previous = u64::MAX;
    for row in &rows {
        let name = row["name"].as_str().expect("rank row name");
        let count = row["count"].as_u64().expect("rank row count");
        assert!(
            count <= previous,
            "ranking must be non-increasing by count: {rows:?}"
        );
        previous = count;
        assert!(
            counts.insert(name.to_owned(), count).is_none(),
            "duplicate ranked name {name}: {rows:?}"
        );
    }
    let expected_map = expected
        .iter()
        .map(|(name, count)| ((*name).to_owned(), *count))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(counts, expected_map, "{payload}");
    assert_eq!(payload["result_count"], expected.len(), "{payload}");
}

async fn call_rank(server: &McpServer, arguments: Value) -> Value {
    handle_real_server_tool_call_raw(server, "tracedecay_rank", arguments).await
}

fn rank_payload(response: &Value) -> Value {
    assert!(
        response["error"].is_null(),
        "tracedecay_rank failed: {response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("rank result text missing: {response}"));
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("rank result is not JSON: {error}: {text}"))
}

async fn shutdown(session: RankSession) {
    let RankSession {
        _isolated_env,
        fixture,
        server,
    } = session;
    drop(server);
    fixture.harness.shutdown().await;
    drop(_isolated_env);
}

#[tokio::test]
async fn rank_orders_relationship_counts_for_calls_and_implements() {
    let session = open_rank_session().await;
    let server = &session.server;

    let incoming = rank_payload(
        &call_rank(
            server,
            json!({
                "edge_kind": "calls",
                "direction": "incoming",
                "node_kind": "function",
                "format": "json"
            }),
        )
        .await,
    );
    assert_eq!(incoming["edge_kind"], "calls");
    assert_eq!(incoming["direction"], "incoming");
    assert_eq!(incoming["node_kind_filter"], "function");
    assert_counts_and_descending_order(
        &incoming,
        &[
            ("left", 1),
            ("noise_caller", 0),
            ("noise_target", 1),
            ("right", 0),
            ("shared", 2),
        ],
    );
    assert_eq!(
        ranking_rows(&incoming)[0],
        json!({
            "name": "shared",
            "kind": "function",
            "file": "src/scoped/calls.rs",
            "line": 1,
            "count": 2
        }),
        "{incoming}"
    );

    let limited = rank_payload(
        &call_rank(
            server,
            json!({
                "edge_kind": "calls",
                "direction": "incoming",
                "node_kind": "function",
                "limit": 1,
                "format": "json"
            }),
        )
        .await,
    );
    assert_rank(
        &limited,
        "calls",
        "incoming",
        Some("function"),
        &[json!({
            "name": "shared",
            "kind": "function",
            "file": "src/scoped/calls.rs",
            "line": 1,
            "count": 2
        })],
    );

    let none = rank_payload(
        &call_rank(
            server,
            json!({
                "edge_kind": "calls",
                "direction": "incoming",
                "node_kind": "function",
                "limit": 0,
                "format": "json"
            }),
        )
        .await,
    );
    assert_rank(&none, "calls", "incoming", Some("function"), &[]);

    let outgoing = rank_payload(
        &call_rank(
            server,
            json!({
                "edge_kind": "calls",
                "direction": "outgoing",
                "node_kind": "function",
                "path": "src/scoped",
                "format": "json"
            }),
        )
        .await,
    );
    assert_rank(
        &outgoing,
        "calls",
        "outgoing",
        Some("function"),
        &[
            json!({"name": "right", "kind": "function", "file": "src/scoped/calls.rs", "line": 7, "count": 2}),
            json!({"name": "left", "kind": "function", "file": "src/scoped/calls.rs", "line": 3, "count": 1}),
            json!({"name": "shared", "kind": "function", "file": "src/scoped/calls.rs", "line": 1, "count": 0}),
        ],
    );

    let scoped = rank_payload(
        &call_rank(
            server,
            json!({
                "edge_kind": "calls",
                "direction": "incoming",
                "node_kind": "function",
                "path": "src/scoped",
                "format": "json"
            }),
        )
        .await,
    );
    assert_rank(
        &scoped,
        "calls",
        "incoming",
        Some("function"),
        &[
            json!({"name": "shared", "kind": "function", "file": "src/scoped/calls.rs", "line": 1, "count": 2}),
            json!({"name": "left", "kind": "function", "file": "src/scoped/calls.rs", "line": 3, "count": 1}),
            json!({"name": "right", "kind": "function", "file": "src/scoped/calls.rs", "line": 7, "count": 0}),
        ],
    );

    let implements = rank_payload(
        &call_rank(
            server,
            json!({
                "edge_kind": "implements",
                "direction": "incoming",
                "node_kind": "trait",
                "format": "json"
            }),
        )
        .await,
    );
    assert_rank(
        &implements,
        "implements",
        "incoming",
        Some("trait"),
        &[
            json!({"name": "Draw", "kind": "trait", "file": "src/shapes/traits.rs", "line": 1, "count": 2}),
            json!({"name": "Paint", "kind": "trait", "file": "src/shapes/traits.rs", "line": 2, "count": 1}),
        ],
    );

    let implementors = rank_payload(
        &call_rank(
            server,
            json!({
                "edge_kind": "implements",
                "direction": "outgoing",
                "node_kind": "struct",
                "format": "json"
            }),
        )
        .await,
    );
    assert_rank(
        &implementors,
        "implements",
        "outgoing",
        Some("struct"),
        &[
            json!({"name": "Circle", "kind": "struct", "file": "src/shapes/traits.rs", "line": 4, "count": 2}),
            json!({"name": "Square", "kind": "struct", "file": "src/shapes/traits.rs", "line": 8, "count": 1}),
            json!({"name": "Ignored", "kind": "struct", "file": "src/scoped/calls.rs", "line": 12, "count": 0}),
        ],
    );

    shutdown(session).await;
}

#[tokio::test]
async fn rank_refuses_missing_invalid_and_unpublished_relationships() {
    let session = open_rank_session().await;
    let server = &session.server;

    let missing = call_rank(server, json!({"format": "json"})).await;
    assert_eq!(missing["error"]["code"], -32602, "{missing}");
    assert_eq!(
        missing["error"]["message"], "missing required parameter: edge_kind",
        "{missing}"
    );
    assert_eq!(
        missing["error"]["data"]["tool"], "tracedecay_rank",
        "{missing}"
    );
    assert_eq!(
        missing["error"]["data"]["reason_code"], "missing_required_parameter",
        "{missing}"
    );
    assert_eq!(missing["error"]["data"]["retryable"], false, "{missing}");

    let invalid_kind = call_rank(server, json!({"edge_kind": "inherits", "format": "json"})).await;
    assert_eq!(invalid_kind["error"]["code"], -32603, "{invalid_kind}");
    assert_eq!(
        invalid_kind["error"]["message"],
        "tool execution failed: config error: invalid edge_kind 'inherits'. Valid values: implements, extends, calls, uses, contains, annotates, derives_macro",
        "{invalid_kind}"
    );

    let invalid_direction = call_rank(
        server,
        json!({"edge_kind": "calls", "direction": "sideways", "format": "json"}),
    )
    .await;
    assert_eq!(
        invalid_direction["error"]["code"], -32603,
        "{invalid_direction}"
    );
    assert_eq!(
        invalid_direction["error"]["message"],
        "tool execution failed: config error: invalid direction 'sideways'. Valid values: incoming, outgoing",
        "{invalid_direction}"
    );

    let derives = call_rank(
        server,
        json!({"edge_kind": "derives_macro", "format": "json"}),
    )
    .await;
    assert_eq!(derives["error"]["code"], -32602, "{derives}");
    assert_eq!(
        derives["error"]["message"],
        "tool project route failed: reason_code=verified-rank-unavailable retryable=false: the admitted graph generation does not publish derives_macro relations",
        "{derives}"
    );
    assert_eq!(
        derives["error"]["data"]["reason_code"], "verified-rank-unavailable",
        "{derives}"
    );
    assert_eq!(derives["error"]["data"]["retryable"], false, "{derives}");
    assert_eq!(
        derives["error"]["data"]["detail"],
        "the admitted graph generation does not publish derives_macro relations",
        "{derives}"
    );

    shutdown(session).await;
}
