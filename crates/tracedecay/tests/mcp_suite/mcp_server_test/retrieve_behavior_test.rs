//! Host-visible `tracedecay_retrieve` behavior over JSON-RPC `tools/call`,
//! answered by the project's graph-tool owner.
//!
//! Handles below are the `rh_` prefix plus the first 12 bytes of SHA-256 of the
//! stored bytes. They are not taken from the store's return value, so a digest
//! change fails the call the same way a copied envelope handle would.
//!
//! JSON pages are the exact text the host receives. `serde_json` sorts object
//! keys because this build does not enable `preserve_order`.
//! Fresh records use a captured current time because the owner rejects
//! future-dated records as corrupt.

#![cfg(feature = "test-transport")]

use serde_json::{Value, json};
use tracedecay_runtime_core::tracedecay::current_timestamp;

use crate::support::{ProductionCompositionFixture, production_composition_fixture};

const EXPIRED_AT: i64 = 1_000_000_000;

const HELLO: &str = "Hello, retrieve.";
const HELLO_HANDLE: &str = "rh_4cfdb03cc4950792e96d771e";
const CRAB: &str = "ab🦀cd";
const CRAB_HANDLE: &str = "rh_85646496e4a65bc20aa95627";
const SHORT: &str = "short";
const SHORT_HANDLE: &str = "rh_f9b0078b5df596d2ea19010c";

fn tool_text(response: &Value) -> &str {
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("retrieve text missing: {response}"))
}

async fn response_handle_root(
    fixture: &ProductionCompositionFixture,
) -> tracedecay_mcp::response_handles::ResponseHandleStore {
    fixture
        .harness
        .server(&fixture.project_root)
        .expect("production retrieve server")
        .project_session_db_for_test()
        .map(tracedecay_mcp::response_handles::ResponseHandleStore::registered)
        .expect("registered project session store")
}

async fn retrieve(fixture: &ProductionCompositionFixture, arguments: Value) -> Value {
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, "tracedecay_retrieve", arguments)
        .await
        .expect("production retrieve invocation");
    serde_json::to_value(response).expect("JSON-RPC response")
}

#[tokio::test(flavor = "multi_thread")]
async fn retrieve_returns_stored_pages_as_literal_json() {
    let fixture = production_composition_fixture().await;
    let root = response_handle_root(&fixture).await;
    let stored_at = current_timestamp();
    let expires_at = stored_at + 86_400;
    root.publish(HELLO, stored_at).await.unwrap();

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
        format!(
            r#"{{"content":"Hello, retrieve.","created_at":{stored_at},"expired":false,"expires_at":{expires_at},"handle":"rh_4cfdb03cc4950792e96d771e","has_more":false,"next_offset":null,"offset":0,"original_chars":16,"total_chars":16}}"#
        )
    );
    assert_eq!(
        tool_text(&window),
        format!(
            r#"{{"content":"retrieve","created_at":{stored_at},"expired":false,"expires_at":{expires_at},"handle":"rh_4cfdb03cc4950792e96d771e","has_more":true,"next_offset":15,"offset":7,"original_chars":16,"total_chars":16}}"#
        )
    );
    assert_eq!(
        tool_text(&tail),
        format!(
            r#"{{"content":".","created_at":{stored_at},"expired":false,"expires_at":{expires_at},"handle":"rh_4cfdb03cc4950792e96d771e","has_more":false,"next_offset":null,"offset":15,"original_chars":16,"total_chars":16}}"#
        )
    );
    assert_eq!(
        tool_text(&end),
        format!(
            r#"{{"content":"","created_at":{stored_at},"expired":false,"expires_at":{expires_at},"handle":"rh_4cfdb03cc4950792e96d771e","has_more":false,"next_offset":null,"offset":16,"original_chars":16,"total_chars":16}}"#
        )
    );
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn retrieve_default_and_markdown_slice_characters_not_bytes() {
    let fixture = production_composition_fixture().await;
    let root = response_handle_root(&fixture).await;
    let stored_at = current_timestamp();
    let expires_at = stored_at + 86_400;
    root.publish(HELLO, stored_at).await.unwrap();
    root.publish(CRAB, stored_at).await.unwrap();

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

    let hello_markdown = format!(
        "## Retrieved Response\n**handle:** `rh_4cfdb03cc4950792e96d771e` (16 chars, expires at {expires_at})\n**offset:** 0\n**next_offset:** none\n**has_more:** false\n\nHello, retrieve."
    );
    assert_eq!(tool_text(&default_page), hello_markdown);
    assert_eq!(tool_text(&markdown_page), hello_markdown);
    assert_eq!(
        tool_text(&crab_json),
        format!(
            r#"{{"content":"🦀","created_at":{stored_at},"expired":false,"expires_at":{expires_at},"handle":"rh_85646496e4a65bc20aa95627","has_more":true,"next_offset":3,"offset":2,"original_chars":5,"total_chars":5}}"#
        )
    );
    assert_eq!(
        tool_text(&crab_markdown),
        format!(
            "## Retrieved Response\n**handle:** `rh_85646496e4a65bc20aa95627` (5 chars, expires at {expires_at})\n**offset:** 2\n**next_offset:** 4\n**has_more:** true\n\n🦀c"
        )
    );
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn retrieve_reports_missing_and_expired_handles() {
    let fixture = production_composition_fixture().await;
    let root = response_handle_root(&fixture).await;
    root.publish(SHORT, EXPIRED_AT).await.unwrap();

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
        r#"{"content":null,"expired":null,"handle":"rh_0123456789abcdef01234567","message":"Response handle was not found in this project's response-handle store.","reason_code":"handle_not_found","retry_instruction":"Re-run the original MCP tool in this project to regenerate the full response and a fresh handle.","retryable":true}"#
    );
    let expired = r#"{"content":null,"created_at":1000000000,"expired":true,"expires_at":1000086400,"handle":"rh_f9b0078b5df596d2ea19010c","message":"Response handle expired at 1000086400.","reason_code":"handle_expired","retry_instruction":"Re-run the original MCP tool in this project to regenerate the full response and a fresh handle.","retryable":true}"#;
    assert_eq!(tool_text(&first_expired), expired);
    assert_eq!(tool_text(&second_expired), expired);
    fixture.shutdown().await;
}

fn assert_invalid_request(response: &Value, message: &str) {
    let problem = crate::support::refusal_problem(&response["result"]);
    assert_eq!(problem["kind"], "invalid_request");
    assert_eq!(problem["code"], "application.surface.invalid_request");
    assert_eq!(problem["message"], message);
}

fn assert_unavailable(response: &Value, code: &str, message: &str) {
    let problem = crate::support::refusal_problem(&response["result"]);
    assert_eq!(problem["kind"], "unavailable");
    assert_eq!(problem["code"], code);
    assert_eq!(problem["message"], message);
}

#[tokio::test(flavor = "multi_thread")]
async fn retrieve_rejects_arguments_outside_its_typed_request() {
    let fixture = production_composition_fixture().await;
    let root = response_handle_root(&fixture).await;
    root.publish(SHORT, current_timestamp()).await.unwrap();

    assert_invalid_request(
        &retrieve(&fixture, json!({})).await,
        "invalid arguments for tracedecay_retrieve: missing field `handle`",
    );
    assert_invalid_request(
        &retrieve(&fixture, json!({"handle": "bogus"})).await,
        "invalid response handle: expected `rh_` followed by 24 hex characters copied from a truncated MCP response envelope",
    );
    assert_invalid_request(
        &retrieve(&fixture, json!({"retrieve_handle": SHORT_HANDLE})).await,
        "invalid arguments for tracedecay_retrieve: unknown field `retrieve_handle`, expected one of `handle`, `offset`, `max_chars`",
    );
    // A non-string handle is a type error, not a missing argument.
    assert_invalid_request(
        &retrieve(&fixture, json!({"handle": 5})).await,
        "invalid arguments for tracedecay_retrieve: invalid type: integer `5`, expected a string",
    );
    assert_invalid_request(
        &retrieve(&fixture, json!({"handle": SHORT_HANDLE, "offset": -1})).await,
        "invalid arguments for tracedecay_retrieve: invalid value: integer `-1`, expected u64",
    );
    assert_unavailable(
        &retrieve(&fixture, json!({"handle": SHORT_HANDLE, "max_chars": 0})).await,
        "response_handle_invalid_page_size",
        "tracedecay_retrieve max_chars must be at least 1",
    );
    assert_unavailable(
        &retrieve(&fixture, json!({"handle": SHORT_HANDLE, "offset": 6})).await,
        "response_handle_offset_out_of_range",
        "tracedecay_retrieve offset 6 exceeds stored response length 5",
    );
    fixture.shutdown().await;
}

/// The owner sizes each page against the response frame the client sends: a
/// plain page fills the whole page budget, and a page whose JSON escapes
/// double its bytes shrinks until its frame fits.
#[tokio::test(flavor = "multi_thread")]
async fn retrieve_pages_fill_the_response_budget_the_client_frame_measures() {
    let fixture = production_composition_fixture().await;
    let root = response_handle_root(&fixture).await;
    let stored_at = current_timestamp();
    let plain = "p".repeat(40_000);
    let plain_handle = root.publish(&plain, stored_at).await.unwrap().handle;
    let quoted = "\"".repeat(40_000);
    let quoted_handle = root.publish(&quoted, stored_at).await.unwrap().handle;

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

#[tokio::test(flavor = "multi_thread")]
async fn retrieve_reports_corrupt_records_as_typed_route_problems() {
    let fixture = production_composition_fixture().await;
    let root = response_handle_root(&fixture).await;
    let corrupt = root
        .publish("{\"items\":[1]}", current_timestamp())
        .await
        .unwrap();
    // Tamper with the committed row behind the store's back.
    let database = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production retrieve server")
        .project_session_db_for_test()
        .expect("registered project session store");
    let changed = database
        .writer_connection()
        .expect("store writer")
        .execute(
            "UPDATE response_handles SET content = '{\"items\":[9]}' WHERE handle = ?1",
            tracedecay_runtime_core::db::engine::params![corrupt.handle.as_str()],
        )
        .await
        .expect("tamper with the stored row");
    assert_eq!(changed, 1);

    assert_unavailable(
        &retrieve(&fixture, json!({"handle": corrupt.handle})).await,
        "corrupt_handle_record",
        "corrupt response-handle record: cached payload failed integrity validation",
    );
    fixture.shutdown().await;
}
