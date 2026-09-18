//! Host-visible `tracedecay_retrieve` behavior on the MCP `tools/call` path.
//!
//! Each test sends the JSON-RPC call an agent host sends and compares the
//! response to a literal envelope. Timestamps in those envelopes are the
//! values used to store the handle, not values read back out of the result.

use super::support::{jsonrpc_request, response_with_id, run_server_with_messages, setup_server};
use serde_json::{Value, json};
use tracedecay::project::current_timestamp;
use tracedecay_mcp::response_handles::{RESPONSE_HANDLE_TTL_SECS, store_response_handle};

const STORED_BODY: &str = "αβγ-page";
const SHORT_BODY: &str = "short";
const ABSENT_HANDLE: &str = "rh_0123456789abcdef01234567";
const CLI_FALLBACK: &str = "This tool is also available from the shell: `tracedecay tool retrieve ...` (`tracedecay tool retrieve --help` for parameters). If MCP calls keep failing or timing out, fall back to that CLI instead of querying .tracedecay databases directly.";

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

fn text_result(id: i64, text: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{ "type": "text", "text": text }]
        }
    })
}

fn invalid_params(id: i64, message: &str, data: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32602,
            "message": message,
            "data": data
        }
    })
}

fn internal_error(id: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32603,
            "message": message,
            "data": {
                "tool": "tracedecay_retrieve",
                "cli_fallback": CLI_FALLBACK
            }
        }
    })
}

fn markdown_page(
    handle: &str,
    total_chars: usize,
    expires_at: i64,
    offset: usize,
    next_offset: &str,
    has_more: bool,
    content: &str,
) -> String {
    format!(
        "## Retrieved Response\n**handle:** `{handle}` ({total_chars} chars, expires at {expires_at})\n**offset:** {offset}\n**next_offset:** {next_offset}\n**has_more:** {has_more}\n\n{content}"
    )
}

#[tokio::test]
async fn retrieve_pages_the_stored_body_by_character_offset() {
    let (server, _dir) = setup_server().await;
    let project_root = server.cg().await.project_root().to_path_buf();
    let created_at = current_timestamp();
    let expires_at = created_at + RESPONSE_HANDLE_TTL_SECS;
    let stored = store_response_handle(&project_root, STORED_BODY, created_at)
        .expect("store the body an agent would retrieve");
    let handle = stored.handle;

    let responses = run_server_with_messages(
        server,
        vec![
            retrieve_call(1, json!({ "handle": handle })),
            retrieve_call(
                2,
                json!({
                    "handle": handle,
                    "offset": 1,
                    "max_chars": 2,
                    "format": "json"
                }),
            ),
        ],
    )
    .await;

    let full = response_with_id(&responses, json!(1));
    assert_eq!(
        full,
        text_result(
            1,
            &markdown_page(&handle, 8, expires_at, 0, "none", false, STORED_BODY)
        ),
        "default retrieve must return the whole stored body"
    );

    let window = json!({
        "handle": handle,
        "expired": false,
        "original_chars": 8,
        "total_chars": 8,
        "offset": 1,
        "next_offset": 3,
        "has_more": true,
        "created_at": created_at,
        "expires_at": expires_at,
        "content": "βγ"
    });
    let window_response = response_with_id(&responses, json!(2));
    assert_eq!(
        window_response,
        text_result(2, &window.to_string()),
        "offset 1 max_chars 2 must be the second and third characters, not bytes"
    );
    let window_text = window_response["result"]["content"][0]["text"]
        .as_str()
        .expect("window text");
    let parsed: Value = serde_json::from_str(window_text).expect("window page JSON");
    assert_eq!(parsed["content"], "βγ");
    assert_eq!(parsed["offset"], 1);
    assert_eq!(parsed["next_offset"], 3);
    assert_eq!(parsed["has_more"], true);
}

#[tokio::test]
async fn retrieve_reports_cache_and_argument_failures() {
    let (server, _dir) = setup_server().await;
    let project_root = server.cg().await.project_root().to_path_buf();
    let created_at = current_timestamp();
    let expires_at = created_at + RESPONSE_HANDLE_TTL_SECS;
    let short =
        store_response_handle(&project_root, SHORT_BODY, created_at).expect("store the short body");
    let expired_at = created_at - RESPONSE_HANDLE_TTL_SECS - 5;
    let expired_until = expired_at + RESPONSE_HANDLE_TTL_SECS;
    let expired = store_response_handle(&project_root, "expired-body", expired_at)
        .expect("store an already expired body");

    let responses = run_server_with_messages(
        server,
        vec![
            retrieve_call(1, json!({})),
            retrieve_call(2, json!({ "handle": "bogus" })),
            retrieve_call(3, json!({ "retrieve_handle": short.handle })),
            retrieve_call(4, json!({ "handle": short.handle, "max_chars": 0 })),
            retrieve_call(5, json!({ "handle": short.handle, "offset": "start" })),
            retrieve_call(6, json!({ "handle": ABSENT_HANDLE })),
            retrieve_call(7, json!({ "handle": expired.handle, "format": "json" })),
            retrieve_call(8, json!({ "handle": short.handle, "offset": 5 })),
            retrieve_call(9, json!({ "handle": short.handle, "offset": 6 })),
        ],
    )
    .await;

    assert_eq!(
        response_with_id(&responses, json!(1)),
        invalid_params(
            1,
            "tracedecay_retrieve requires the `handle` argument copied from a truncated MCP response envelope.",
            json!({
                "tool": "tracedecay_retrieve",
                "reason_code": "missing_handle_argument",
                "retryable": false,
                "retry_instruction": "Call `tracedecay_retrieve` again with the exact `handle` value emitted by the truncated response envelope."
            })
        )
    );
    assert_eq!(
        response_with_id(&responses, json!(2)),
        invalid_params(
            2,
            "invalid response handle: expected `rh_` followed by 24 hex characters copied from a truncated MCP response envelope",
            json!({
                "tool": "tracedecay_retrieve",
                "reason_code": "invalid_handle",
                "retryable": false,
                "retry_instruction": "Pass the exact `handle` string from a truncated MCP response envelope; do not shorten or edit it."
            })
        )
    );
    assert_eq!(
        response_with_id(&responses, json!(3)),
        internal_error(
            3,
            "tool execution failed: config error: unknown tracedecay_retrieve argument `retrieve_handle`"
        )
    );
    assert_eq!(
        response_with_id(&responses, json!(4)),
        invalid_params(
            4,
            "tool project route failed: reason_code=response_handle_invalid_page_size retryable=false: tracedecay_retrieve max_chars must be at least 1",
            json!({
                "tool": "tracedecay_retrieve",
                "reason_code": "response_handle_invalid_page_size",
                "retryable": false,
                "detail": "tracedecay_retrieve max_chars must be at least 1"
            })
        )
    );
    assert_eq!(
        response_with_id(&responses, json!(5)),
        internal_error(
            5,
            "tool execution failed: config error: offset must be a non-negative integer"
        )
    );

    assert_eq!(
        response_with_id(&responses, json!(6)),
        text_result(
            6,
            "**handle:** rh_0123456789abcdef01234567\n**message:** Response handle was not found in this project's local cache.\n**reason_code:** handle_not_found\n**retry_instruction:** Re-run the original MCP tool in this project to regenerate the full response and a fresh handle.\n**retryable:** true\n"
        ),
        "a well-formed handle that was never stored is a retryable miss, not a protocol error"
    );

    let expired_page = json!({
        "handle": expired.handle,
        "expired": true,
        "content": null,
        "reason_code": "handle_expired",
        "message": format!(
            "Response handle expired at {expired_until} and was removed from this project's local cache."
        ),
        "retryable": true,
        "retry_instruction": "Re-run the original MCP tool in this project to regenerate the full response and a fresh handle.",
        "created_at": expired_at,
        "expires_at": expired_until
    });
    assert_eq!(
        response_with_id(&responses, json!(7)),
        text_result(7, &expired_page.to_string())
    );

    assert_eq!(
        response_with_id(&responses, json!(8)),
        text_result(
            8,
            &markdown_page(&short.handle, 5, expires_at, 5, "none", false, "")
        ),
        "offset equal to the stored length is an empty page, not an error"
    );
    assert_eq!(
        response_with_id(&responses, json!(9)),
        invalid_params(
            9,
            "tool project route failed: reason_code=response_handle_offset_out_of_range retryable=false: tracedecay_retrieve offset 6 exceeds stored response length 5",
            json!({
                "tool": "tracedecay_retrieve",
                "reason_code": "response_handle_offset_out_of_range",
                "retryable": false,
                "detail": "tracedecay_retrieve offset 6 exceeds stored response length 5"
            })
        )
    );
}
