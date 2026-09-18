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
    extract_text, handle_real_server_tool_call_raw, production_composition_fixture_with_sources,
    warm_code_index_search,
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

const MISSING_FILTER: &str = "missing required parameter: one of 'returns', 'params', or 'async'";

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
        "unavailable_fields": [],
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

fn matches_without_ids(payload: &Value) -> Vec<Value> {
    let matches = payload["matches"]
        .as_array()
        .unwrap_or_else(|| panic!("signature search payload has no matches array: {payload}"));
    let mut stripped = matches
        .iter()
        .map(|item| {
            let mut item = item.clone();
            {
                let Some(object) = item.as_object_mut() else {
                    panic!("signature match is not an object: {item}");
                };
                let Some(id) = object.remove("id") else {
                    panic!("signature match is missing an id field");
                };
                let Value::String(id) = id else {
                    panic!("signature match id is not a string: {id}");
                };
                if id.is_empty() {
                    panic!("signature match id is empty");
                }
            }
            item
        })
        .collect::<Vec<_>>();
    stripped.sort_by_key(sort_key);
    stripped
}

async fn call_signature_search(server: &McpServer, arguments: Value) -> Value {
    handle_real_server_tool_call_raw(server, "tracedecay_signature_search", arguments).await
}

async fn signature_json(server: &McpServer, mut arguments: Value) -> Value {
    arguments
        .as_object_mut()
        .expect("signature search arguments")
        .insert("format".to_owned(), json!("json"));
    let response = call_signature_search(server, arguments).await;
    assert!(
        response["error"].is_null(),
        "signature search failed: {response}"
    );
    serde_json::from_str(extract_text(&response["result"]))
        .unwrap_or_else(|error| panic!("signature search JSON ({error}): {response}"))
}

fn assert_match_set(payload: &Value, expected: &[Value]) {
    assert_eq!(
        payload.as_object().map(|object| object.len()),
        Some(2),
        "signature search payload gained or lost fields: {payload}"
    );
    assert_eq!(payload["match_count"], expected.len(), "{payload}");
    let actual = matches_without_ids(payload);
    let mut expected = expected.to_vec();
    expected.sort_by_key(sort_key);
    assert_eq!(actual, expected, "signature search payload: {payload}");
}

async fn assert_ids_match_exact_symbol(server: &McpServer, payload: &Value) {
    let matches = payload["matches"]
        .as_array()
        .unwrap_or_else(|| panic!("signature search payload has no matches array: {payload}"));
    for item in matches {
        let name = item["name"]
            .as_str()
            .unwrap_or_else(|| panic!("match has no name: {item}"));
        let response = handle_real_server_tool_call_raw(
            server,
            "tracedecay_find_exact_symbol",
            json!({"name": name, "limit": 20, "format": "json"}),
        )
        .await;
        assert!(
            response["error"].is_null(),
            "exact symbol lookup for {name} failed: {response}"
        );
        let exact: Value = serde_json::from_str(extract_text(&response["result"]))
            .unwrap_or_else(|error| panic!("exact symbol JSON for {name} ({error}): {response}"));
        let hits = exact["matches"]
            .as_array()
            .unwrap_or_else(|| panic!("exact symbol payload for {name}: {exact}"))
            .iter()
            .filter(|candidate| candidate["name"] == name)
            .collect::<Vec<_>>();
        assert_eq!(hits.len(), 1, "exact symbol hits for {name}: {exact}");
        assert_eq!(item["id"], hits[0]["id"], "{name}");
    }
}

fn assert_missing_filter(response: &Value) {
    assert_eq!(response["id"], 1);
    assert!(response["result"].is_null(), "{response}");
    assert_eq!(response["error"]["code"], -32602);
    assert_eq!(response["error"]["message"], MISSING_FILTER);
    assert_eq!(
        response["error"]["data"]["tool"],
        "tracedecay_signature_search"
    );
    assert_eq!(
        response["error"]["data"]["reason_code"],
        "missing_required_parameter"
    );
    assert_eq!(response["error"]["data"]["retryable"], false);
    assert_eq!(response["error"]["data"]["detail"], MISSING_FILTER);
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

    assert_missing_filter(&call_signature_search(&server, json!({})).await);
    assert_missing_filter(&call_signature_search(&server, json!({"params": []})).await);
    assert_missing_filter(&call_signature_search(&server, json!({"async": "yes"})).await);

    let user_results = signature_json(&server, json!({"returns": "Result<User, LoadError>"})).await;
    assert_match_set(&user_results, &[load_user(), cached_user(), load_paged()]);
    assert_ids_match_exact_symbol(&server, &user_results).await;

    let wrong_case = signature_json(&server, json!({"returns": "result<User, LoadError>"})).await;
    assert_eq!(wrong_case, json!({"match_count": 0, "matches": []}));

    let label = signature_json(&server, json!({"params": ["label"]})).await;
    assert_match_set(&label, &[format_user()]);
    assert_ids_match_exact_symbol(&server, &label).await;
    let wrong_label = signature_json(&server, json!({"params": ["Label"]})).await;
    assert_eq!(wrong_label, json!({"match_count": 0, "matches": []}));

    let save_only = signature_json(
        &server,
        json!({
            "params": ["&mut self"],
            "async": true,
            "returns": "LoadError",
            "path": "src/api.rs",
        }),
    )
    .await;
    assert_match_set(&save_only, &[save()]);
    assert_ids_match_exact_symbol(&server, &save_only).await;

    let param_u32 = signature_json(&server, json!({"params": ["u32"]})).await;
    assert_match_set(&param_u32, &[load_paged()]);
    let return_u32 = signature_json(&server, json!({"returns": "u32"})).await;
    assert_match_set(&return_u32, &[sync_load(), write_pair()]);

    let nested_params = signature_json(&server, json!({"params": ["(u8, String)"]})).await;
    assert_match_set(&nested_params, &[write_pair()]);
    let nested_as_return = signature_json(&server, json!({"returns": "(u8, String)"})).await;
    assert_eq!(nested_as_return, json!({"match_count": 0, "matches": []}));

    let cache_sync = signature_json(&server, json!({"async": false, "path": "src/cache.rs"})).await;
    assert_match_set(&cache_sync, &[cached_user(), sync_load(), write_pair()]);

    let returning_user = signature_json(&server, json!({"returns": "User"})).await;
    assert_match_set(&returning_user, &[load_user(), cached_user(), load_paged()]);

    let contradicted =
        signature_json(&server, json!({"params": ["label"], "returns": "u32"})).await;
    assert_eq!(contradicted, json!({"match_count": 0, "matches": []}));

    let limited = signature_json(
        &server,
        json!({"returns": "Result<User, LoadError>", "limit": 0}),
    )
    .await;
    assert_eq!(limited["match_count"], 1);
    let limited_matches = matches_without_ids(&limited);
    assert_eq!(limited_matches.len(), 1);
    assert!(
        [load_user(), cached_user(), load_paged()].contains(&limited_matches[0]),
        "limit 0 did not return one complete Result<User, LoadError> match: {limited}"
    );

    let empty_markdown = call_signature_search(
        &server,
        json!({"returns": "result<User, LoadError>", "format": "markdown"}),
    )
    .await;
    assert!(empty_markdown["error"].is_null(), "{empty_markdown}");
    assert_eq!(
        extract_text(&empty_markdown["result"]),
        "**match_count:** 0\nmatches: none\n"
    );

    let format_user_id = label["matches"][0]["id"]
        .as_str()
        .expect("format_user occurrence id");
    let markdown =
        call_signature_search(&server, json!({"params": ["label"], "format": "markdown"})).await;
    assert!(markdown["error"].is_null(), "{markdown}");
    assert_eq!(
        extract_text(&markdown["result"]),
        format!(
            "**match_count:** 1\n\n## matches\n- **format_user**\n  **kind:** function\n  **file:** src/api.rs\n  **line:** 5\n  **id:** `{format_user_id}`\n  **signature:** `pub fn format_user(user: &User, label: &str) -> String`\n  **is_async:** false\n  **qualified_name:** `src/api.rs::format_user`\n  **unavailable_fields:** none\n"
        )
    );

    fixture.harness.shutdown().await;
}
