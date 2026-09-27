//! Host-visible `tracedecay_retrieve` behavior over JSON-RPC `tools/call`,
//! answered by the project's graph-tool owner.
//!
//! Handles below are the `rh_` prefix plus the first 12 bytes of SHA-256 of the
//! stored bytes. They are not taken from the store's return value, so a digest
//! change fails the call the same way a copied envelope handle would.
//!
//! JSON pages are the exact text the host receives. `serde_json` sorts object
//! keys because this build does not enable `preserve_order`.

#![cfg(feature = "test-transport")]

use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};
use tracedecay_mcp::response_handles::store_response_handle;

use crate::support::{ProductionCompositionFixture, production_composition_fixture};

const STORED_AT: i64 = 4_102_444_800;
const EXPIRED_AT: i64 = 1_000_000_000;

const HELLO: &str = "Hello, retrieve.";
const HELLO_HANDLE: &str = "rh_4cfdb03cc4950792e96d771e";
const CRAB: &str = "ab🦀cd";
const CRAB_HANDLE: &str = "rh_85646496e4a65bc20aa95627";
const SHORT: &str = "short";
const SHORT_HANDLE: &str = "rh_f9b0078b5df596d2ea19010c";

const CLI_FALLBACK: &str = "This tool is also available from the shell: `tracedecay tool retrieve ...` \
(`tracedecay tool retrieve --help` for parameters). If MCP calls keep failing or timing out, \
fall back to that CLI instead of querying .tracedecay databases directly.";

fn tool_text(response: &Value) -> &str {
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("retrieve text missing: {response}"))
}

async fn response_handle_root(fixture: &ProductionCompositionFixture) -> PathBuf {
    fixture
        .harness
        .server(&fixture.project_root)
        .expect("production retrieve server")
        .cg()
        .await
        .store_layout()
        .response_handle_root
        .clone()
}

async fn retrieve(fixture: &ProductionCompositionFixture, arguments: Value) -> Value {
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, "tracedecay_retrieve", arguments)
        .await
        .expect("production retrieve invocation");
    serde_json::to_value(response).expect("JSON-RPC response")
}

#[tokio::test]
async fn retrieve_returns_stored_pages_as_literal_json() {
    let fixture = production_composition_fixture().await;
    let root = response_handle_root(&fixture).await;
    store_response_handle(&root, HELLO, STORED_AT).unwrap();

    let first = retrieve(&fixture, json!({"handle": HELLO_HANDLE, "format": "json"})).await;
    let window = retrieve(
        &fixture,
        json!({"handle": HELLO_HANDLE, "format": "json", "offset": 7, "max_chars": 8}),
    )
    .await;
    let tail = retrieve(
        &fixture,
        json!({"handle": HELLO_HANDLE, "format": "json", "offset": 15, "max_chars": 8}),
    )
    .await;
    let end = retrieve(
        &fixture,
        json!({"handle": HELLO_HANDLE, "format": "json", "offset": 16}),
    )
    .await;

    assert_eq!(
        tool_text(&first),
        r#"{"content":"Hello, retrieve.","created_at":4102444800,"expired":false,"expires_at":4102531200,"handle":"rh_4cfdb03cc4950792e96d771e","has_more":false,"next_offset":null,"offset":0,"original_chars":16,"total_chars":16}"#
    );
    assert_eq!(
        tool_text(&window),
        r#"{"content":"retrieve","created_at":4102444800,"expired":false,"expires_at":4102531200,"handle":"rh_4cfdb03cc4950792e96d771e","has_more":true,"next_offset":15,"offset":7,"original_chars":16,"total_chars":16}"#
    );
    assert_eq!(
        tool_text(&tail),
        r#"{"content":".","created_at":4102444800,"expired":false,"expires_at":4102531200,"handle":"rh_4cfdb03cc4950792e96d771e","has_more":false,"next_offset":null,"offset":15,"original_chars":16,"total_chars":16}"#
    );
    assert_eq!(
        tool_text(&end),
        r#"{"content":"","created_at":4102444800,"expired":false,"expires_at":4102531200,"handle":"rh_4cfdb03cc4950792e96d771e","has_more":false,"next_offset":null,"offset":16,"original_chars":16,"total_chars":16}"#
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn retrieve_default_and_markdown_slice_characters_not_bytes() {
    let fixture = production_composition_fixture().await;
    let root = response_handle_root(&fixture).await;
    store_response_handle(&root, HELLO, STORED_AT).unwrap();
    store_response_handle(&root, CRAB, STORED_AT).unwrap();

    let default_page = retrieve(&fixture, json!({"handle": HELLO_HANDLE})).await;
    let markdown_page = retrieve(
        &fixture,
        json!({"handle": HELLO_HANDLE, "format": "markdown"}),
    )
    .await;
    let crab_json = retrieve(
        &fixture,
        json!({
            "handle": CRAB_HANDLE,
            "format": "json",
            "offset": 2,
            "max_chars": 1
        }),
    )
    .await;
    let crab_markdown = retrieve(
        &fixture,
        json!({
            "handle": CRAB_HANDLE,
            "format": "markdown",
            "offset": 2,
            "max_chars": 2
        }),
    )
    .await;

    let hello_markdown = "## Retrieved Response\n**handle:** `rh_4cfdb03cc4950792e96d771e` (16 chars, expires at 4102531200)\n**offset:** 0\n**next_offset:** none\n**has_more:** false\n\nHello, retrieve.";
    assert_eq!(tool_text(&default_page), hello_markdown);
    assert_eq!(tool_text(&markdown_page), hello_markdown);
    assert_eq!(
        tool_text(&crab_json),
        r#"{"content":"🦀","created_at":4102444800,"expired":false,"expires_at":4102531200,"handle":"rh_85646496e4a65bc20aa95627","has_more":true,"next_offset":3,"offset":2,"original_chars":5,"total_chars":5}"#
    );
    assert_eq!(
        tool_text(&crab_markdown),
        "## Retrieved Response\n**handle:** `rh_85646496e4a65bc20aa95627` (5 chars, expires at 4102531200)\n**offset:** 2\n**next_offset:** 4\n**has_more:** true\n\n🦀c"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn retrieve_reports_missing_and_expired_handles() {
    let fixture = production_composition_fixture().await;
    let root = response_handle_root(&fixture).await;
    store_response_handle(&root, SHORT, EXPIRED_AT).unwrap();

    let missing = retrieve(
        &fixture,
        json!({
            "handle": "rh_0123456789abcdef01234567",
            "format": "json"
        }),
    )
    .await;
    let first_expired = retrieve(&fixture, json!({"handle": SHORT_HANDLE, "format": "json"})).await;
    let second_expired =
        retrieve(&fixture, json!({"handle": SHORT_HANDLE, "format": "json"})).await;

    assert_eq!(
        tool_text(&missing),
        r#"{"content":null,"expired":null,"handle":"rh_0123456789abcdef01234567","message":"Response handle was not found in this project's local cache.","reason_code":"handle_not_found","retry_instruction":"Re-run the original MCP tool in this project to regenerate the full response and a fresh handle.","retryable":true}"#
    );
    let expired = r#"{"content":null,"created_at":1000000000,"expired":true,"expires_at":1000086400,"handle":"rh_f9b0078b5df596d2ea19010c","message":"Response handle expired at 1000086400 and was removed from this project's local cache.","reason_code":"handle_expired","retry_instruction":"Re-run the original MCP tool in this project to regenerate the full response and a fresh handle.","retryable":true}"#;
    assert_eq!(tool_text(&first_expired), expired);
    assert_eq!(tool_text(&second_expired), expired);
    fixture.shutdown().await;
}

fn config_refusal(detail: &str) -> Value {
    json!({
        "code": -32603,
        "message": format!("tool execution failed: config error: invalid arguments for tracedecay_retrieve: {detail}"),
        "data": {
            "tool": "tracedecay_retrieve",
            "cli_fallback": CLI_FALLBACK
        }
    })
}

#[tokio::test]
async fn retrieve_rejects_arguments_outside_its_typed_request() {
    let fixture = production_composition_fixture().await;
    let root = response_handle_root(&fixture).await;
    store_response_handle(&root, SHORT, STORED_AT).unwrap();

    assert_eq!(
        retrieve(&fixture, json!({})).await["error"],
        json!({
            "code": -32602,
            "message": "tracedecay_retrieve requires the `handle` argument copied from a truncated MCP response envelope.",
            "data": {
                "tool": "tracedecay_retrieve",
                "reason_code": "missing_handle_argument",
                "retryable": false,
                "retry_instruction": "Call `tracedecay_retrieve` again with the exact `handle` value emitted by the truncated response envelope."
            }
        })
    );
    assert_eq!(
        retrieve(&fixture, json!({"handle": "bogus"})).await["error"],
        json!({
            "code": -32602,
            "message": "invalid response handle: expected `rh_` followed by 24 hex characters copied from a truncated MCP response envelope",
            "data": {
                "tool": "tracedecay_retrieve",
                "reason_code": "invalid_handle",
                "retryable": false,
                "retry_instruction": "Pass the exact `handle` string from a truncated MCP response envelope; do not shorten or edit it."
            }
        })
    );
    assert_eq!(
        retrieve(&fixture, json!({"retrieve_handle": SHORT_HANDLE})).await["error"],
        config_refusal(
            "unknown field `retrieve_handle`, expected one of `handle`, `offset`, `max_chars`"
        )
    );
    // A non-string handle is a type error, not a missing argument.
    assert_eq!(
        retrieve(&fixture, json!({"handle": 5})).await["error"],
        config_refusal("invalid type: integer `5`, expected a string")
    );
    assert_eq!(
        retrieve(&fixture, json!({"handle": SHORT_HANDLE, "offset": -1})).await["error"],
        config_refusal("invalid value: integer `-1`, expected u64")
    );
    assert_eq!(
        retrieve(&fixture, json!({"handle": SHORT_HANDLE, "max_chars": 0})).await["error"],
        json!({
            "code": -32602,
            "message": "tool project route failed: reason_code=response_handle_invalid_page_size retryable=false: tracedecay_retrieve max_chars must be at least 1",
            "data": {
                "tool": "tracedecay_retrieve",
                "reason_code": "response_handle_invalid_page_size",
                "retryable": false,
                "detail": "tracedecay_retrieve max_chars must be at least 1"
            }
        })
    );
    assert_eq!(
        retrieve(&fixture, json!({"handle": SHORT_HANDLE, "offset": 6})).await["error"],
        json!({
            "code": -32602,
            "message": "tool project route failed: reason_code=response_handle_offset_out_of_range retryable=false: tracedecay_retrieve offset 6 exceeds stored response length 5",
            "data": {
                "tool": "tracedecay_retrieve",
                "reason_code": "response_handle_offset_out_of_range",
                "retryable": false,
                "detail": "tracedecay_retrieve offset 6 exceeds stored response length 5"
            }
        })
    );
    fixture.shutdown().await;
}

/// The owner sizes each page against the response frame the client sends: a
/// plain page fills the whole page budget, and a page whose JSON escapes
/// double its bytes shrinks until its frame fits.
#[tokio::test]
async fn retrieve_pages_fill_the_response_budget_the_client_frame_measures() {
    let fixture = production_composition_fixture().await;
    let root = response_handle_root(&fixture).await;
    let plain = "p".repeat(40_000);
    let plain_handle = store_response_handle(&root, &plain, STORED_AT)
        .unwrap()
        .handle;
    let quoted = "\"".repeat(40_000);
    let quoted_handle = store_response_handle(&root, &quoted, STORED_AT)
        .unwrap()
        .handle;

    let page = retrieve(&fixture, json!({"handle": plain_handle, "format": "json"})).await;
    let page: Value = serde_json::from_str(tool_text(&page)).unwrap();
    // MAX_RESPONSE_CHARS (15_000) less the 2_048-char page header allowance.
    assert_eq!(page["content"].as_str().unwrap(), "p".repeat(12_952));
    assert_eq!(page["next_offset"], 12_952);

    let response = retrieve(&fixture, json!({"handle": quoted_handle, "format": "json"})).await;
    let frame = serde_json::to_string(&response).unwrap();
    let page: Value = serde_json::from_str(tool_text(&response)).unwrap();
    let served = page["content"].as_str().unwrap().len();
    assert!(
        frame.len() <= 15_000,
        "a {served}-char quoted page framed {} bytes",
        frame.len()
    );
    assert!(
        served > 3_000,
        "an escape-heavy page still fills most of the frame: {served}"
    );
    assert_eq!(page["content"].as_str().unwrap(), "\"".repeat(served));
    assert_eq!(page["next_offset"], served);
    fixture.shutdown().await;
}

#[tokio::test]
async fn retrieve_reports_unreadable_records_as_typed_route_problems() {
    let fixture = production_composition_fixture().await;
    let root = response_handle_root(&fixture).await;
    let corrupt = store_response_handle(&root, "{\"items\":[1]}", STORED_AT).unwrap();
    fs::write(root.join(format!("{}.json", corrupt.handle)), "{not-json").unwrap();
    let unreadable = store_response_handle(&root, "{\"items\":[2]}", STORED_AT).unwrap();
    let unreadable_path = root.join(format!("{}.json", unreadable.handle));
    fs::remove_file(&unreadable_path).unwrap();
    fs::create_dir(&unreadable_path).unwrap();

    assert_eq!(
        retrieve(&fixture, json!({"handle": corrupt.handle})).await["error"],
        json!({
            "code": -32603,
            "message": "tool project route failed: reason_code=corrupt_handle_record retryable=true: corrupt response-handle record: cached payload failed integrity validation",
            "data": {
                "tool": "tracedecay_retrieve",
                "reason_code": "corrupt_handle_record",
                "retryable": true,
                "detail": "corrupt response-handle record: cached payload failed integrity validation"
            }
        })
    );
    assert_eq!(
        retrieve(&fixture, json!({"handle": unreadable.handle})).await["error"],
        json!({
            "code": -32603,
            "message": "tool project route failed: reason_code=handle_read_failed retryable=true: response-handle cache is unavailable",
            "data": {
                "tool": "tracedecay_retrieve",
                "reason_code": "handle_read_failed",
                "retryable": true,
                "detail": "response-handle cache is unavailable"
            }
        })
    );
    fixture.shutdown().await;
}
