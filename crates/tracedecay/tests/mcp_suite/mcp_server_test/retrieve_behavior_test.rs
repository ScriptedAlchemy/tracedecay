//! Host-visible `tracedecay_retrieve` behavior over JSON-RPC `tools/call`.
//!
//! Handles below are the `rh_` prefix plus the first 12 bytes of SHA-256 of the
//! stored bytes. They are not taken from the store's return value, so a digest
//! change fails the call the same way a copied envelope handle would.
//!
//! JSON pages are the exact text the host receives. `serde_json` sorts object
//! keys because this build does not enable `preserve_order`.

use crate::mcp_server_test::support::{
    jsonrpc_request, response_with_id, run_server_with_messages, setup_server,
};
use serde_json::{Value, json};
use tracedecay_mcp::response_handles::store_response_handle;

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

fn retrieve_call(id: i64, arguments: Value) -> String {
    jsonrpc_request(
        json!(id),
        "tools/call",
        json!({
            "name": "tracedecay_retrieve",
            "arguments": arguments,
        }),
    )
}

#[tokio::test]
async fn retrieve_returns_stored_pages_as_literal_json() {
    let (server, _dir) = setup_server().await;
    let root = server
        .cg()
        .await
        .store_layout()
        .response_handle_root
        .clone();
    store_response_handle(&root, HELLO, STORED_AT).unwrap();

    let responses = run_server_with_messages(
        server,
        vec![
            retrieve_call(1, json!({"handle": HELLO_HANDLE, "format": "json"})),
            retrieve_call(
                2,
                json!({"handle": HELLO_HANDLE, "format": "json", "offset": 7, "max_chars": 8}),
            ),
            retrieve_call(
                3,
                json!({"handle": HELLO_HANDLE, "format": "json", "offset": 15, "max_chars": 8}),
            ),
            retrieve_call(
                4,
                json!({"handle": HELLO_HANDLE, "format": "json", "offset": 16}),
            ),
        ],
    )
    .await;

    let first = response_with_id(&responses, json!(1));
    let window = response_with_id(&responses, json!(2));
    let tail = response_with_id(&responses, json!(3));
    let end = response_with_id(&responses, json!(4));
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
}

#[tokio::test]
async fn retrieve_default_and_markdown_slice_characters_not_bytes() {
    let (server, _dir) = setup_server().await;
    let root = server
        .cg()
        .await
        .store_layout()
        .response_handle_root
        .clone();
    store_response_handle(&root, HELLO, STORED_AT).unwrap();
    store_response_handle(&root, CRAB, STORED_AT).unwrap();

    let responses = run_server_with_messages(
        server,
        vec![
            retrieve_call(1, json!({"handle": HELLO_HANDLE})),
            retrieve_call(2, json!({"handle": HELLO_HANDLE, "format": "markdown"})),
            retrieve_call(
                3,
                json!({
                    "handle": CRAB_HANDLE,
                    "format": "json",
                    "offset": 2,
                    "max_chars": 1
                }),
            ),
            retrieve_call(
                4,
                json!({
                    "handle": CRAB_HANDLE,
                    "format": "markdown",
                    "offset": 2,
                    "max_chars": 2
                }),
            ),
        ],
    )
    .await;

    let default_page = response_with_id(&responses, json!(1));
    let markdown_page = response_with_id(&responses, json!(2));
    let crab_json = response_with_id(&responses, json!(3));
    let crab_markdown = response_with_id(&responses, json!(4));
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
}

#[tokio::test]
async fn retrieve_reports_missing_and_expired_handles() {
    let (server, _dir) = setup_server().await;
    let root = server
        .cg()
        .await
        .store_layout()
        .response_handle_root
        .clone();
    store_response_handle(&root, SHORT, EXPIRED_AT).unwrap();

    let responses = run_server_with_messages(
        server,
        vec![
            retrieve_call(
                1,
                json!({
                    "handle": "rh_0123456789abcdef01234567",
                    "format": "json"
                }),
            ),
            retrieve_call(2, json!({"handle": SHORT_HANDLE, "format": "json"})),
            retrieve_call(3, json!({"handle": SHORT_HANDLE, "format": "json"})),
        ],
    )
    .await;

    let missing = response_with_id(&responses, json!(1));
    let first_expired = response_with_id(&responses, json!(2));
    let second_expired = response_with_id(&responses, json!(3));
    assert_eq!(
        tool_text(&missing),
        r#"{"content":null,"expired":null,"handle":"rh_0123456789abcdef01234567","message":"Response handle was not found in this project's local cache.","reason_code":"handle_not_found","retry_instruction":"Re-run the original MCP tool in this project to regenerate the full response and a fresh handle.","retryable":true}"#
    );
    let expired = r#"{"content":null,"created_at":1000000000,"expired":true,"expires_at":1000086400,"handle":"rh_f9b0078b5df596d2ea19010c","message":"Response handle expired at 1000086400 and was removed from this project's local cache.","reason_code":"handle_expired","retry_instruction":"Re-run the original MCP tool in this project to regenerate the full response and a fresh handle.","retryable":true}"#;
    assert_eq!(tool_text(&first_expired), expired);
    assert_eq!(tool_text(&second_expired), expired);
}

#[tokio::test]
async fn retrieve_rejects_bad_arguments_with_typed_errors() {
    let (server, _dir) = setup_server().await;
    let root = server
        .cg()
        .await
        .store_layout()
        .response_handle_root
        .clone();
    store_response_handle(&root, SHORT, STORED_AT).unwrap();

    let responses = run_server_with_messages(
        server,
        vec![
            retrieve_call(1, json!({})),
            retrieve_call(2, json!({"handle": "bogus"})),
            retrieve_call(3, json!({"retrieve_handle": SHORT_HANDLE})),
            retrieve_call(4, json!({"handle": SHORT_HANDLE, "max_chars": 0})),
            retrieve_call(5, json!({"handle": SHORT_HANDLE, "offset": -1})),
            retrieve_call(6, json!({"handle": SHORT_HANDLE, "offset": 6})),
        ],
    )
    .await;

    assert_eq!(
        response_with_id(&responses, json!(1)),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {
                "code": -32602,
                "message": "tracedecay_retrieve requires the `handle` argument copied from a truncated MCP response envelope.",
                "data": {
                    "tool": "tracedecay_retrieve",
                    "reason_code": "missing_handle_argument",
                    "retryable": false,
                    "retry_instruction": "Call `tracedecay_retrieve` again with the exact `handle` value emitted by the truncated response envelope."
                }
            }
        })
    );
    assert_eq!(
        response_with_id(&responses, json!(2)),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "error": {
                "code": -32602,
                "message": "invalid response handle: expected `rh_` followed by 24 hex characters copied from a truncated MCP response envelope",
                "data": {
                    "tool": "tracedecay_retrieve",
                    "reason_code": "invalid_handle",
                    "retryable": false,
                    "retry_instruction": "Pass the exact `handle` string from a truncated MCP response envelope; do not shorten or edit it."
                }
            }
        })
    );
    assert_eq!(
        response_with_id(&responses, json!(3)),
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "error": {
                "code": -32603,
                "message": "tool execution failed: config error: unknown tracedecay_retrieve argument `retrieve_handle`",
                "data": {
                    "tool": "tracedecay_retrieve",
                    "cli_fallback": CLI_FALLBACK
                }
            }
        })
    );
    assert_eq!(
        response_with_id(&responses, json!(4)),
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "error": {
                "code": -32602,
                "message": "tool project route failed: reason_code=response_handle_invalid_page_size retryable=false: tracedecay_retrieve max_chars must be at least 1",
                "data": {
                    "tool": "tracedecay_retrieve",
                    "reason_code": "response_handle_invalid_page_size",
                    "retryable": false,
                    "detail": "tracedecay_retrieve max_chars must be at least 1"
                }
            }
        })
    );
    assert_eq!(
        response_with_id(&responses, json!(5)),
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "error": {
                "code": -32603,
                "message": "tool execution failed: config error: offset must be a non-negative integer",
                "data": {
                    "tool": "tracedecay_retrieve",
                    "cli_fallback": CLI_FALLBACK
                }
            }
        })
    );
    assert_eq!(
        response_with_id(&responses, json!(6)),
        json!({
            "jsonrpc": "2.0",
            "id": 6,
            "error": {
                "code": -32602,
                "message": "tool project route failed: reason_code=response_handle_offset_out_of_range retryable=false: tracedecay_retrieve offset 6 exceeds stored response length 5",
                "data": {
                    "tool": "tracedecay_retrieve",
                    "reason_code": "response_handle_offset_out_of_range",
                    "retryable": false,
                    "detail": "tracedecay_retrieve offset 6 exceeds stored response length 5"
                }
            }
        })
    );
}
