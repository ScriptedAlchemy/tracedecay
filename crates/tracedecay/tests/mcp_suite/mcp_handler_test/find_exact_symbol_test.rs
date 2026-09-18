//! Production MCP behavior for `tracedecay_find_exact_symbol`.
//!
//! Calls go through `tools/call` on the production server. Expectations are
//! the symbol identity a caller reads, not the index that produced it.

#![cfg(feature = "test-transport")]

use crate::support::{
    ProductionCompositionFixture, extract_real_server_text, handle_real_server_tool_call_raw,
    production_composition_fixture_with_sources, warm_code_index_search,
};
use serde_json::{Value, json};
use std::fs;
use std::sync::Arc;
use tracedecay::mcp::McpServer;

const SOLVERS_SOURCE: &str = "\
pub struct Solvers {
    pub gmres: u32,
}

pub fn gmres(x: u32) -> u32 {
    x + 1
}
";

const SHARED_TOKEN_SOURCE: &str = "\
pub fn shared_token() -> u32 {
    1
}
";

fn symbol(
    name: &str,
    qualified_name: &str,
    kind: &str,
    file: &str,
    line: u64,
    signature: &str,
) -> Value {
    json!({
        "name": name,
        "qualified_name": qualified_name,
        "kind": kind,
        "file": file,
        "line": line,
        "signature": signature,
    })
}

fn gmres_field() -> Value {
    symbol(
        "gmres",
        "src/lib.rs::Solvers::gmres",
        "field",
        "src/lib.rs",
        2,
        "pub gmres: u32",
    )
}

fn gmres_function() -> Value {
    symbol(
        "gmres",
        "src/lib.rs::gmres",
        "function",
        "src/lib.rs",
        5,
        "pub fn gmres(x: u32) -> u32",
    )
}

fn shared_token(file: &str) -> Value {
    symbol(
        "shared_token",
        &format!("{file}::shared_token"),
        "function",
        file,
        1,
        "pub fn shared_token() -> u32",
    )
}

async fn indexed_project() -> (ProductionCompositionFixture, Arc<McpServer>) {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).expect("fixture src dir");
        fs::write(project.join("src/lib.rs"), SOLVERS_SOURCE).expect("solvers source");
        fs::write(project.join("src/billing.rs"), SHARED_TOKEN_SOURCE).expect("billing source");
        fs::write(project.join("src/ledger.rs"), SHARED_TOKEN_SOURCE).expect("ledger source");
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    warm_code_index_search(&server, "gmres").await;
    (fixture, server)
}

async fn exact_payload(server: &McpServer, arguments: Value) -> Value {
    let response =
        handle_real_server_tool_call_raw(server, "tracedecay_find_exact_symbol", arguments).await;
    assert!(
        response["error"].is_null(),
        "exact-symbol call must succeed: {response}"
    );
    serde_json::from_str(extract_real_server_text(&response["result"]))
        .expect("exact-symbol response JSON")
}

fn sorted_without_ids(mut payload: Value) -> Value {
    let matches = payload["matches"]
        .as_array_mut()
        .expect("exact-symbol matches array");
    for item in matches.iter_mut() {
        item.as_object_mut()
            .expect("exact-symbol match object")
            .remove("id");
    }
    matches.sort_by(|left, right| {
        (
            left["file"].as_str(),
            left["line"].as_u64(),
            left["kind"].as_str(),
        )
            .cmp(&(
                right["file"].as_str(),
                right["line"].as_u64(),
                right["kind"].as_str(),
            ))
    });
    payload
}

fn occurrence_ids(payload: &Value) -> Vec<String> {
    payload["matches"]
        .as_array()
        .expect("exact-symbol matches array")
        .iter()
        .map(|item| {
            item["id"]
                .as_str()
                .expect("exact-symbol match occurrence id")
                .to_owned()
        })
        .collect()
}

#[tokio::test]
async fn find_exact_symbol_returns_every_bare_name_hit() {
    let (fixture, server) = indexed_project().await;

    let gmres = exact_payload(
        &server,
        json!({"name": "gmres", "limit": 20, "format": "json"}),
    )
    .await;
    let gmres_ids = occurrence_ids(&gmres);
    assert_eq!(
        sorted_without_ids(gmres),
        json!({
            "name": "gmres",
            "count": 2,
            "matches": [gmres_field(), gmres_function()],
        })
    );
    assert_eq!(gmres_ids.len(), 2);
    assert_ne!(gmres_ids[0], gmres_ids[1]);

    let folded = exact_payload(&server, json!({"name": "Gmres", "format": "json"})).await;
    assert_eq!(folded["name"], "Gmres");
    assert_eq!(folded["count"], 2);
    assert_eq!(
        sorted_without_ids(folded)["matches"],
        json!([gmres_field(), gmres_function()])
    );

    for name in [
        "gmre",
        "src/lib.rs::gmres",
        "Solvers::gmres",
        "shared_tok",
        "not_a_symbol",
    ] {
        assert_eq!(
            exact_payload(&server, json!({"name": name, "format": "json"})).await,
            json!({"name": name, "count": 0, "matches": []}),
            "a non-equal bare name must not match"
        );
    }

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn find_exact_symbol_applies_limit_and_rejects_bad_arguments() {
    let (fixture, server) = indexed_project().await;
    let billing = shared_token("src/billing.rs");
    let ledger = shared_token("src/ledger.rs");

    let all = exact_payload(&server, json!({"name": "shared_token", "format": "json"})).await;
    assert_eq!(
        sorted_without_ids(all),
        json!({
            "name": "shared_token",
            "count": 2,
            "matches": [billing.clone(), ledger.clone()],
        })
    );

    let capped = exact_payload(
        &server,
        json!({"name": "shared_token", "limit": 1, "format": "json"}),
    )
    .await;
    assert_eq!(capped["name"], "shared_token");
    assert_eq!(capped["count"], 1);
    let only = sorted_without_ids(capped)["matches"][0].clone();
    assert!(
        only == billing || only == ledger,
        "limit 1 must return one indexed declaration, got {only}"
    );

    let missing = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_find_exact_symbol",
        json!({"format": "json"}),
    )
    .await;
    assert_eq!(missing["result"], Value::Null);
    assert_eq!(missing["error"]["code"], -32602);
    assert_eq!(
        missing["error"]["message"],
        "missing required parameter: name"
    );
    assert_eq!(
        missing["error"]["data"],
        json!({
            "tool": "tracedecay_find_exact_symbol",
            "reason_code": "missing_required_parameter",
            "retryable": false,
            "detail": "missing required parameter: name",
        })
    );

    let zero = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_find_exact_symbol",
        json!({"name": "shared_token", "limit": 0, "format": "json"}),
    )
    .await;
    assert_eq!(zero["result"], Value::Null);
    assert_eq!(zero["error"]["code"], -32602);
    assert_eq!(
        zero["error"]["message"],
        "tool project route failed: reason_code=code-graph-invalid-request retryable=false: the code-graph read request is invalid: code graph name resolution limit must be positive"
    );
    assert_eq!(
        zero["error"]["data"],
        json!({
            "tool": "tracedecay_find_exact_symbol",
            "reason_code": "code-graph-invalid-request",
            "retryable": false,
            "detail": "the code-graph read request is invalid: code graph name resolution limit must be positive",
        })
    );

    fixture.harness.shutdown().await;
}
