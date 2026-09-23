//! Behavioral proof of `tracedecay_signature_search` through the production MCP
//! `tools/call` path. Expectations are the signature text, location, and
//! async flag an agent reads back, not the scan that produced them.
//! Occurrence order is not part of the tool contract, so multi-match
//! assertions compare the set of records.

#![cfg(feature = "test-transport")]

use std::fs;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;

use crate::support::{
    dispatch_mcp_tool_call, extract_text, handle_real_server_tool_call_raw,
    production_composition_fixture_with_sources, warm_code_index_search,
};

const API_RS: &str = "\
pub async fn load_user(id: i32) -> Result<User, LoadError> {
    Ok(User)
}

pub fn format_user(user: &User, label: &str) -> String {
    String::new()
}

impl User {
    pub async fn save(&mut self, session: &Session) -> Result<(), LoadError> {
        Ok(())
    }

    pub fn name(&self) -> &str {
        \"user\"
    }
}

pub struct User;
pub struct Session;
pub struct LoadError;
";

const CACHE_RS: &str = "\
pub fn cached_user(id: i32) -> Result<User, LoadError> {
    Err(LoadError)
}

pub fn sync_load() -> u32 {
    1
}

pub fn write_pair(buf: &mut (u8, String)) -> u32 {
    0
}
";

const PAGED_RS: &str = "\
pub async fn load_paged(
    id: i32,
    page: u32,
) -> Result<User, LoadError> {
    Ok(User)
}
";

fn record(
    name: &str,
    qualified_name: &str,
    kind: &str,
    file: &str,
    line: u64,
    signature: &str,
    is_async: bool,
) -> Value {
    json!({
        "name": name,
        "qualified_name": qualified_name,
        "kind": kind,
        "file": file,
        "line": line,
        "signature": signature,
        "is_async": is_async,
    })
}

fn load_user() -> Value {
    record(
        "load_user",
        "src/api.rs::load_user",
        "function",
        "src/api.rs",
        1,
        "pub async fn load_user(id: i32) -> Result<User, LoadError>",
        true,
    )
}

fn format_user() -> Value {
    record(
        "format_user",
        "src/api.rs::format_user",
        "function",
        "src/api.rs",
        5,
        "pub fn format_user(user: &User, label: &str) -> String",
        false,
    )
}

fn save() -> Value {
    record(
        "save",
        "src/api.rs::User::save",
        "method",
        "src/api.rs",
        10,
        "pub async fn save(&mut self, session: &Session) -> Result<(), LoadError>",
        true,
    )
}

fn cached_user() -> Value {
    record(
        "cached_user",
        "src/cache.rs::cached_user",
        "function",
        "src/cache.rs",
        1,
        "pub fn cached_user(id: i32) -> Result<User, LoadError>",
        false,
    )
}

fn sync_load() -> Value {
    record(
        "sync_load",
        "src/cache.rs::sync_load",
        "function",
        "src/cache.rs",
        5,
        "pub fn sync_load() -> u32",
        false,
    )
}

fn write_pair() -> Value {
    record(
        "write_pair",
        "src/cache.rs::write_pair",
        "function",
        "src/cache.rs",
        9,
        "pub fn write_pair(buf: &mut (u8, String)) -> u32",
        false,
    )
}

fn load_paged() -> Value {
    record(
        "load_paged",
        "src/paged.rs::load_paged",
        "function",
        "src/paged.rs",
        1,
        "pub async fn load_paged(\n    id: i32,\n    page: u32,\n) -> Result<User, LoadError>",
        true,
    )
}

fn sort_key(value: &Value) -> (String, String, String) {
    (
        value["file"].as_str().unwrap_or_default().to_owned(),
        value["name"].as_str().unwrap_or_default().to_owned(),
        value["signature"].as_str().unwrap_or_default().to_owned(),
    )
}

async fn call_signature_search(server: &McpServer, arguments: Value) -> Value {
    handle_real_server_tool_call_raw(server, "tracedecay_signature_search", arguments).await
}

/// The served page's items.
async fn signature_items(server: &McpServer, arguments: Value) -> Vec<Value> {
    let response = call_signature_search(server, arguments).await;
    assert!(
        response["error"].is_null() && response["result"]["isError"].is_null(),
        "signature search failed: {response}"
    );
    let payload: Value = serde_json::from_str(extract_text(&response["result"]))
        .unwrap_or_else(|error| panic!("signature search JSON ({error}): {response}"));
    payload["outcome"]["value"]["payload"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("signature search page has no items: {payload}"))
        .clone()
}

async fn assert_match_set(server: &McpServer, arguments: Value, expected: &[Value]) {
    let items = signature_items(server, arguments.clone()).await;
    let mut actual = items
        .iter()
        .map(|item| {
            assert!(
                item["node_id"].as_str().is_some_and(|id| !id.is_empty()),
                "signature match carries its occurrence id: {item}"
            );
            json!({
                "name": item["name"],
                "qualified_name": item["qualified_name"],
                "kind": item["kind"],
                "file": item["file"],
                "line": item["line"],
                "signature": item["signature"],
                "is_async": item["is_async"],
            })
        })
        .collect::<Vec<_>>();
    actual.sort_by_key(sort_key);
    let mut expected = expected.to_vec();
    expected.sort_by_key(sort_key);
    assert_eq!(actual, expected, "signature search {arguments}");
}

async fn assert_ids_match_exact_symbol(server: &McpServer, arguments: Value) {
    for item in signature_items(server, arguments).await {
        let name = item["name"].as_str().expect("match name");
        let response = handle_real_server_tool_call_raw(
            server,
            "tracedecay_find_exact_symbol",
            json!({"name": name, "limit": 20, "format": "json"}),
        )
        .await;
        let exact: Value = serde_json::from_str(extract_text(&response["result"]))
            .unwrap_or_else(|error| panic!("exact symbol JSON for {name} ({error}): {response}"));
        let hits = exact["matches"]
            .as_array()
            .unwrap_or_else(|| panic!("exact symbol payload for {name}: {exact}"))
            .iter()
            .filter(|candidate| candidate["name"] == name)
            .collect::<Vec<_>>();
        assert_eq!(hits.len(), 1, "exact symbol hits for {name}: {exact}");
        assert_eq!(item["node_id"], hits[0]["id"], "{name}");
    }
}

fn assert_refused(response: &Value, context: &str) {
    let refused_at_parse =
        response["error"]["data"]["reason_code"] == "application_surface_invalid_request";
    let refused_by_contract = response["result"]["isError"] == true
        && response["result"]["problem"]["kind"] == "invalid_request";
    assert!(
        refused_at_parse || refused_by_contract,
        "{context} must be a typed invalid request: {response}"
    );
}

#[tokio::test]
async fn signature_search_matches_signature_shape_and_rejects_an_unfiltered_call() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src/api.rs"), API_RS).unwrap();
        fs::write(project.join("src/cache.rs"), CACHE_RS).unwrap();
        fs::write(project.join("src/paged.rs"), PAGED_RS).unwrap();
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production graph server");
    warm_code_index_search(&server, "load_user").await;

    assert_refused(
        &call_signature_search(&server, json!({})).await,
        "no filter",
    );
    assert_refused(
        &call_signature_search(&server, json!({"params": []})).await,
        "an empty params filter",
    );
    for retired in [
        json!({"async": true}),
        json!({"returns": "u32", "path": "src/cache.rs"}),
        json!({"returns": "u32", "limit": 1}),
    ] {
        assert_refused(
            &call_signature_search(&server, retired.clone()).await,
            &format!("retired argument {retired}"),
        );
    }

    let user_results = json!({"returns": "Result<User, LoadError>"});
    assert_match_set(
        &server,
        user_results.clone(),
        &[load_user(), cached_user(), load_paged()],
    )
    .await;
    assert_ids_match_exact_symbol(&server, user_results).await;
    assert_match_set(&server, json!({"returns": "result<User, LoadError>"}), &[]).await;

    assert_match_set(&server, json!({"params": ["label"]}), &[format_user()]).await;
    assert_match_set(&server, json!({"params": ["Label"]}), &[]).await;

    let save_only = json!({
        "params": ["&mut self"],
        "is_async": true,
        "returns": "LoadError",
        "scope": {"path_prefix": "src/api.rs"},
    });
    assert_match_set(&server, save_only.clone(), &[save()]).await;
    assert_ids_match_exact_symbol(&server, save_only).await;

    assert_match_set(&server, json!({"params": ["u32"]}), &[load_paged()]).await;
    assert_match_set(
        &server,
        json!({"returns": "u32"}),
        &[sync_load(), write_pair()],
    )
    .await;
    assert_match_set(
        &server,
        json!({"params": ["(u8, String)"]}),
        &[write_pair()],
    )
    .await;
    assert_match_set(&server, json!({"returns": "(u8, String)"}), &[]).await;
    // `save` returns `Result<(), LoadError>`: its parenthesized return type is
    // not part of the parameter list.
    assert_match_set(&server, json!({"params": ["Result"]}), &[]).await;

    assert_match_set(
        &server,
        json!({"is_async": false, "scope": {"path_prefix": "src/cache.rs"}}),
        &[cached_user(), sync_load(), write_pair()],
    )
    .await;
    assert_match_set(
        &server,
        json!({"returns": "User"}),
        &[load_user(), cached_user(), load_paged()],
    )
    .await;
    assert_match_set(&server, json!({"params": ["label"], "returns": "u32"}), &[]).await;

    let markdown = dispatch_mcp_tool_call(
        &server,
        "tracedecay_signature_search",
        json!({"params": ["label"]}),
    )
    .await;
    let markdown = extract_text(&markdown["result"]);
    assert!(
        markdown.contains("src/api.rs::format_user (function) src/api.rs:5")
            && markdown.contains("  pub fn format_user(user: &User, label: &str) -> String"),
        "markdown lists the match and its signature: {markdown}"
    );

    fixture.harness.shutdown().await;
}
