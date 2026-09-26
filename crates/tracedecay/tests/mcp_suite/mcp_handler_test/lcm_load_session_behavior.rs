//! Caller-visible `tracedecay_lcm_load_session` behavior on the real MCP path.
//!
//! Each case is a JSON-RPC `tools/call`, the same request a host sends. The
//! expected documents are the messages, slices, and refusals that call returns.
//! Opaque cursors are used only as the continuation a host would pass back.

use std::sync::Arc;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;
use tracedecay_domain::{CanonicalMessageRoleV1, UtcMicros};

use crate::support::{
    activate_test_temporal_generation, extract_real_server_text, handle_real_server_tool_call,
    handle_real_server_tool_call_raw, open_active_project_session_db,
    persist_temporal_lcm_observation, real_mcp_server, setup_empty_project,
};

const SESSION: &str = "load-proof-session";
const USER_ID: &str = "load-proof-user";
const USER_BODY: &str = "user-asks-brass";
const USER_AT: i64 = 10;
const ASSISTANT_ID: &str = "load-proof-assistant";
const ASSISTANT_BODY: &str = "assistant-names-oak-hook";
const ASSISTANT_AT: i64 = 20;
const CODEX_ID: &str = "load-proof-codex";
const CODEX_BODY: &str = "codex-only-brass-note";
const CODEX_AT: i64 = 25;
const TOOL_ID: &str = "load-proof-tool";
const TOOL_BODY: &str = "tool-returns-token-91";
const TOOL_AT: i64 = 30;
/// `assistant-names-oak-hook` at offset 10, length 6.
const WINDOW: &str = "names-";
const INVALID: &str = "The retained operation request is invalid.";
const NOT_FOUND: &str = "The requested resource was not found or is not authorized";
const DEFAULT_LIMIT: u64 = 4_096;
const CLAMPED_LIMIT: u64 = 20_000;
const TOOL_META: &str = r#"{"evidence":{"ordering_domain":"snapshot_order","range":{"end":4,"start":3}},"facts":[{"content":"tool-returns-token-91","kind":"tool_result","success":true}],"native_record_kind":"message","provider":"cursor","relations":{"message_id":"load-proof-tool","session_id":"load-proof-session"},"stable_record_id":"record.mcp.load-proof-session.load-proof-tool","version":1}"#;
const ASSISTANT_META: &str = r#"{"evidence":{"ordering_domain":"snapshot_order","range":{"end":3,"start":2}},"facts":[{"content":"assistant-names-oak-hook","kind":"message","model":"test-model","role":"assistant","timestamp":20}],"native_record_kind":"message","provider":"cursor","relations":{"message_id":"load-proof-assistant","session_id":"load-proof-session"},"stable_record_id":"record.mcp.load-proof-session.load-proof-assistant","version":1}"#;
const USER_META: &str = r#"{"evidence":{"ordering_domain":"snapshot_order","range":{"end":2,"start":1}},"facts":[{"content":"user-asks-brass","kind":"message","model":"test-model","role":"user","timestamp":10}],"native_record_kind":"message","provider":"cursor","relations":{"message_id":"load-proof-user","session_id":"load-proof-session"},"stable_record_id":"record.mcp.load-proof-session.load-proof-user","version":1}"#;
const CODEX_META: &str = r#"{"evidence":{"ordering_domain":"snapshot_order","range":{"end":5,"start":4}},"facts":[{"content":"codex-only-brass-note","kind":"message","model":"test-model","role":"assistant","timestamp":25}],"native_record_kind":"message","provider":"codex","relations":{"message_id":"load-proof-codex","session_id":"load-proof-session"},"stable_record_id":"record.mcp.load-proof-session.load-proof-codex","version":1}"#;

#[tokio::test]
async fn tracedecay_lcm_load_session_returns_the_messages_the_caller_asked_for() {
    let (cg, _env, _dir) = setup_empty_project().await;
    // Message timestamps are Unix seconds. Knowledge time, which an `as_of` cutoff
    // cuts on, is that timestamp in UTC microseconds.
    let user = persist_temporal_lcm_observation(
        &cg,
        "cursor",
        SESSION,
        USER_ID,
        USER_BODY.to_owned(),
        CanonicalMessageRoleV1::User,
        1,
        USER_AT,
        UtcMicros(USER_AT.saturating_mul(1_000_000)),
    )
    .await;
    let assistant = persist_temporal_lcm_observation(
        &cg,
        "cursor",
        SESSION,
        ASSISTANT_ID,
        ASSISTANT_BODY.to_owned(),
        CanonicalMessageRoleV1::Assistant,
        2,
        ASSISTANT_AT,
        UtcMicros(ASSISTANT_AT.saturating_mul(1_000_000)),
    )
    .await;
    let tool = persist_temporal_lcm_observation(
        &cg,
        "cursor",
        SESSION,
        TOOL_ID,
        TOOL_BODY.to_owned(),
        CanonicalMessageRoleV1::Tool,
        3,
        TOOL_AT,
        UtcMicros(TOOL_AT.saturating_mul(1_000_000)),
    )
    .await;
    let codex = persist_temporal_lcm_observation(
        &cg,
        "codex",
        SESSION,
        CODEX_ID,
        CODEX_BODY.to_owned(),
        CanonicalMessageRoleV1::Assistant,
        4,
        CODEX_AT,
        UtcMicros(CODEX_AT.saturating_mul(1_000_000)),
    )
    .await;
    let db = open_active_project_session_db(&cg).await;
    activate_test_temporal_generation(&db, SESSION, vec![user, assistant, tool, codex]).await;
    let server = real_mcp_server(cg).await;

    let cursor_newest_first = load(
        &server,
        json!({"provider": "cursor", "session_id": SESSION}),
    )
    .await;
    let explicit_forensic = load(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "temporal_mode": {"kind": "forensic"}
        }),
    )
    .await;
    let window = load(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "roles": ["assistant"],
            "content_offset": 10,
            "content_limit": 6
        }),
    )
    .await;
    let past_end = load(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "roles": ["assistant"],
            "content_offset": 100,
            "content_limit": 6
        }),
    )
    .await;
    let tool_only = load(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "role": " tool "
        }),
    )
    .await;
    let user_and_assistant = load(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "role": "assistant",
            "roles": ["user"]
        }),
    )
    .await;
    let during_assistant = load(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "start_time": 15,
            "end_time": 25
        }),
    )
    .await;
    let as_of_assistant = load(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "temporal_mode": {
                "kind": "as_of",
                "cutoff": ASSISTANT_AT.saturating_mul(1_000_000)
            }
        }),
    )
    .await;
    let codex_only = load(&server, json!({"provider": "codex", "session_id": SESSION})).await;
    let every_provider = load(&server, json!({"session_id": SESSION})).await;
    let explicit_all = load(&server, json!({"provider": "all", "session_id": SESSION})).await;
    let clamped = load(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "roles": ["user"],
            "content_limit": 25_000
        }),
    )
    .await;
    let first_page = load(
        &server,
        json!({"provider": "cursor", "session_id": SESSION, "limit": 1}),
    )
    .await;
    let first_cursor = first_page["temporal"]["next_cursor"]
        .as_str()
        .unwrap_or_else(|| panic!("first page must continue: {first_page}"))
        .to_owned();
    let second_page = load(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "limit": 1,
            "cursor": first_cursor
        }),
    )
    .await;
    let second_cursor = second_page["temporal"]["next_cursor"]
        .as_str()
        .unwrap_or_else(|| panic!("second page must continue: {second_page}"))
        .to_owned();
    let third_page = load(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "limit": 1,
            "cursor": second_cursor
        }),
    )
    .await;
    let ghost = load(
        &server,
        json!({"provider": "cursor", "session_id": "ghost-load-session"}),
    )
    .await;
    let empty_session = load(&server, json!({"provider": "cursor", "session_id": "   "})).await;
    let empty_role = load(
        &server,
        json!({"provider": "cursor", "session_id": SESSION, "roles": [" "]}),
    )
    .await;
    let zero_content_limit = load(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "content_limit": 0
        }),
    )
    .await;
    let zero_limit = load(
        &server,
        json!({"provider": "cursor", "session_id": SESSION, "limit": 0}),
    )
    .await;
    let over_limit = load(
        &server,
        json!({"provider": "cursor", "session_id": SESSION, "limit": 101}),
    )
    .await;
    let as_of_without_cutoff = load_raw(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "temporal_mode": {"kind": "as_of"}
        }),
    )
    .await;
    let missing_session = load_raw(&server, json!({"provider": "cursor"})).await;
    let unknown_field = load_raw(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "not_a_field": true
        }),
    )
    .await;
    let negative_limit = load_raw(
        &server,
        json!({"provider": "cursor", "session_id": SESSION, "limit": -1}),
    )
    .await;

    let cursor_messages = json!([
        message(
            TOOL_ID,
            "cursor",
            "tool",
            TOOL_AT,
            TOOL_BODY,
            21,
            0,
            DEFAULT_LIMIT,
            false
        ),
        message(
            ASSISTANT_ID,
            "cursor",
            "assistant",
            ASSISTANT_AT,
            ASSISTANT_BODY,
            24,
            0,
            DEFAULT_LIMIT,
            false
        ),
        message(
            USER_ID,
            "cursor",
            "user",
            USER_AT,
            USER_BODY,
            15,
            0,
            DEFAULT_LIMIT,
            false
        ),
    ]);
    assert_loaded(
        &cursor_newest_first,
        "cursor",
        cursor_messages.clone(),
        DEFAULT_LIMIT,
        None,
        3,
    );
    assert_loaded(
        &explicit_forensic,
        "cursor",
        cursor_messages,
        DEFAULT_LIMIT,
        None,
        3,
    );
    assert!(
        cursor_newest_first["temporal"]["next_cursor"].is_null(),
        "three messages fit the default page: {cursor_newest_first}"
    );
    assert!(
        !cursor_newest_first.to_string().contains(CODEX_BODY),
        "a cursor load must not return the codex body: {cursor_newest_first}"
    );

    assert_loaded(
        &window,
        "cursor",
        json!([message(
            ASSISTANT_ID,
            "cursor",
            "assistant",
            ASSISTANT_AT,
            WINDOW,
            24,
            10,
            6,
            true
        )]),
        6,
        None,
        1,
    );
    assert_loaded(
        &past_end,
        "cursor",
        json!([message(
            ASSISTANT_ID,
            "cursor",
            "assistant",
            ASSISTANT_AT,
            "",
            24,
            24,
            6,
            false
        )]),
        6,
        None,
        1,
    );
    assert_loaded(
        &tool_only,
        "cursor",
        json!([message(
            TOOL_ID,
            "cursor",
            "tool",
            TOOL_AT,
            TOOL_BODY,
            21,
            0,
            DEFAULT_LIMIT,
            false
        )]),
        DEFAULT_LIMIT,
        None,
        1,
    );
    assert_loaded(
        &user_and_assistant,
        "cursor",
        json!([
            message(
                ASSISTANT_ID,
                "cursor",
                "assistant",
                ASSISTANT_AT,
                ASSISTANT_BODY,
                24,
                0,
                DEFAULT_LIMIT,
                false
            ),
            message(
                USER_ID,
                "cursor",
                "user",
                USER_AT,
                USER_BODY,
                15,
                0,
                DEFAULT_LIMIT,
                false
            ),
        ]),
        DEFAULT_LIMIT,
        None,
        2,
    );
    assert_loaded(
        &during_assistant,
        "cursor",
        json!([message(
            ASSISTANT_ID,
            "cursor",
            "assistant",
            ASSISTANT_AT,
            ASSISTANT_BODY,
            24,
            0,
            DEFAULT_LIMIT,
            false
        )]),
        DEFAULT_LIMIT,
        None,
        1,
    );
    // The assistant's knowledge time is 20_000_000 microseconds. Cutting `as_of`
    // there does not replay the earlier messages; the host is refused.
    assert_not_found(&as_of_assistant);
    assert!(
        !as_of_assistant.to_string().contains(USER_BODY),
        "{as_of_assistant}"
    );
    assert!(
        !as_of_assistant.to_string().contains(ASSISTANT_BODY),
        "{as_of_assistant}"
    );
    assert_loaded(
        &codex_only,
        "codex",
        json!([message(
            CODEX_ID,
            "codex",
            "assistant",
            CODEX_AT,
            CODEX_BODY,
            21,
            0,
            DEFAULT_LIMIT,
            false
        )]),
        DEFAULT_LIMIT,
        None,
        1,
    );
    let codex_text = codex_only.to_string();
    assert!(!codex_text.contains(USER_BODY), "{codex_only}");
    assert!(!codex_text.contains(ASSISTANT_BODY), "{codex_only}");
    assert!(!codex_text.contains(TOOL_BODY), "{codex_only}");

    let every_message = json!([
        message(
            TOOL_ID,
            "cursor",
            "tool",
            TOOL_AT,
            TOOL_BODY,
            21,
            0,
            DEFAULT_LIMIT,
            false
        ),
        message(
            CODEX_ID,
            "codex",
            "assistant",
            CODEX_AT,
            CODEX_BODY,
            21,
            0,
            DEFAULT_LIMIT,
            false
        ),
        message(
            ASSISTANT_ID,
            "cursor",
            "assistant",
            ASSISTANT_AT,
            ASSISTANT_BODY,
            24,
            0,
            DEFAULT_LIMIT,
            false
        ),
        message(
            USER_ID,
            "cursor",
            "user",
            USER_AT,
            USER_BODY,
            15,
            0,
            DEFAULT_LIMIT,
            false
        ),
    ]);
    assert_loaded(
        &every_provider,
        "all",
        every_message.clone(),
        DEFAULT_LIMIT,
        None,
        4,
    );
    assert_loaded(&explicit_all, "all", every_message, DEFAULT_LIMIT, None, 4);

    assert_loaded(
        &clamped,
        "cursor",
        json!([message(
            USER_ID,
            "cursor",
            "user",
            USER_AT,
            USER_BODY,
            15,
            0,
            CLAMPED_LIMIT,
            false
        )]),
        CLAMPED_LIMIT,
        Some(25_000),
        1,
    );

    assert_loaded(
        &first_page,
        "cursor",
        json!([message(
            TOOL_ID,
            "cursor",
            "tool",
            TOOL_AT,
            TOOL_BODY,
            21,
            0,
            DEFAULT_LIMIT,
            false
        )]),
        DEFAULT_LIMIT,
        None,
        3,
    );
    assert_loaded(
        &second_page,
        "cursor",
        json!([message(
            ASSISTANT_ID,
            "cursor",
            "assistant",
            ASSISTANT_AT,
            ASSISTANT_BODY,
            24,
            0,
            DEFAULT_LIMIT,
            false
        )]),
        DEFAULT_LIMIT,
        None,
        3,
    );
    assert_loaded(
        &third_page,
        "cursor",
        json!([message(
            USER_ID,
            "cursor",
            "user",
            USER_AT,
            USER_BODY,
            15,
            0,
            DEFAULT_LIMIT,
            false
        )]),
        DEFAULT_LIMIT,
        None,
        3,
    );
    assert!(
        third_page["temporal"]["next_cursor"].is_null(),
        "the third page is the end of the cursor session: {third_page}"
    );

    assert_eq!(ghost["messages"], json!([]));
    assert_eq!(ghost["session_id"], "ghost-load-session");
    assert_eq!(ghost["provider"], "cursor");
    assert_eq!(ghost["status"], "ok");
    assert_eq!(ghost["omitted"], 0);
    assert_eq!(ghost["content_limit"], json!(DEFAULT_LIMIT));
    assert_eq!(
        ghost["temporal"]["coverage"],
        json!({"visible": 0, "hidden": 0, "unknown": 0, "redacted": 0})
    );
    assert!(ghost["temporal"]["next_cursor"].is_null(), "{ghost}");
    let ghost_text = ghost.to_string();
    assert!(!ghost_text.contains(USER_BODY), "{ghost}");
    assert!(!ghost_text.contains(TOOL_BODY), "{ghost}");

    assert_invalid(&empty_session);
    assert_invalid(&empty_role);
    assert_invalid(&zero_content_limit);
    assert_invalid(&zero_limit);
    assert_invalid(&over_limit);
    assert_eq!(
        as_of_without_cutoff["error"]["code"], -32603,
        "{as_of_without_cutoff}"
    );
    assert!(
        as_of_without_cutoff["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("missing field `cutoff`")),
        "an as-of mode without its cutoff must fail decode: {as_of_without_cutoff}"
    );

    assert_decode(
        &missing_session,
        "tool execution failed: config error: invalid retained application request for tracedecay_lcm_load_session: missing field `session_id`",
    );
    assert_decode(
        &unknown_field,
        "tool execution failed: config error: invalid retained application request for tracedecay_lcm_load_session: not_a_field: unknown field `not_a_field`, expected one of `provider`, `session_id`, `cursor`, `temporal_mode`, `limit`, `role`, `roles`, `start_time`, `end_time`, `content_offset`, `content_limit`",
    );
    assert_decode(
        &negative_limit,
        "tool execution failed: config error: invalid retained application request for tracedecay_lcm_load_session: limit: invalid value: integer `-1`, expected u64",
    );

    server.shutdown().await;
}

async fn load(server: &McpServer, arguments: Value) -> Value {
    let result =
        handle_real_server_tool_call(server, "tracedecay_lcm_load_session", arguments).await;
    serde_json::from_str(extract_real_server_text(&result)).expect("load-session JSON")
}

async fn load_raw(server: &Arc<McpServer>, arguments: Value) -> Value {
    handle_real_server_tool_call_raw(server, "tracedecay_lcm_load_session", arguments).await
}

fn assert_loaded(
    payload: &Value,
    provider: &str,
    messages: Value,
    content_limit: u64,
    clamped_from: Option<u64>,
    omitted: u64,
) {
    assert_eq!(payload["status"], "partial", "{payload}");
    assert_eq!(payload["provider"], provider, "{payload}");
    assert_eq!(payload["session_id"], SESSION, "{payload}");
    assert_eq!(payload["content_limit"], json!(content_limit), "{payload}");
    assert_eq!(payload["messages"], messages, "{payload}");
    assert_eq!(payload["omitted"], json!(omitted), "{payload}");
    assert_eq!(
        payload["temporal"]["coverage"]["unknown"],
        json!(omitted),
        "{payload}"
    );
    match clamped_from {
        Some(from) => assert_eq!(
            payload["content_limit_clamped_from"],
            json!(from),
            "{payload}"
        ),
        None => assert!(
            payload.get("content_limit_clamped_from").is_none(),
            "{payload}"
        ),
    }
}

fn assert_invalid(payload: &Value) {
    assert_eq!(payload["problem"]["kind"], "invalid_request", "{payload}");
    assert_eq!(
        payload["problem"]["code"], "application.retained.invalid-request",
        "{payload}"
    );
    assert_eq!(payload["problem"]["message"], INVALID, "{payload}");
    assert_eq!(
        payload["problem"]["diagnostic"]["code"], "application.retained.invalid-request",
        "{payload}"
    );
    assert_eq!(
        payload["problem"]["diagnostic"]["message"], INVALID,
        "{payload}"
    );
    assert_eq!(
        payload["problem"]["legal_actions"],
        json!(["correct_request"]),
        "{payload}"
    );
    assert_eq!(payload["problem"]["retry"], "never", "{payload}");
    assert!(payload.get("messages").is_none(), "{payload}");
}

fn assert_not_found(payload: &Value) {
    let mut problem = payload["problem"].clone();
    let Some(object) = problem.as_object_mut() else {
        panic!("not-found problem: {payload}");
    };
    object.remove("request_id");
    object.remove("trace_id");
    assert_eq!(
        problem,
        json!({
            "revision": 1,
            "kind": "not_found_or_not_authorized",
            "code": "not_found_or_not_authorized",
            "message": NOT_FOUND,
            "diagnostic": null,
            "detail": null,
            "committed_receipt": null,
            "owning_layer": "application",
            "terminality": "pre_admission",
            "retryable": false,
            "retry": "never",
            "retry_scope": null,
            "retry_after_millis": null,
            "cancellation_stage": null,
            "unavailable_classification": null,
            "execution_failure_classification": null,
            "details": [],
            "legal_actions": [],
            "coverage": null
        }),
        "{payload}"
    );
    assert!(payload.get("messages").is_none(), "{payload}");
}

fn assert_decode(response: &Value, message: &str) {
    assert_eq!(response["error"]["code"], -32603, "{response}");
    assert_eq!(
        response["error"]["data"]["tool"], "tracedecay_lcm_load_session",
        "{response}"
    );
    assert_eq!(response["error"]["message"], message, "{response}");
}

#[allow(clippy::too_many_arguments)]
fn message(
    message_id: &str,
    provider: &str,
    role: &str,
    _timestamp: i64,
    content: &str,
    total_chars: u64,
    offset: u64,
    limit: u64,
    truncated: bool,
) -> Value {
    let (timestamp, metadata_json) = match message_id {
        TOOL_ID => (Value::Null, TOOL_META),
        ASSISTANT_ID => (json!(ASSISTANT_AT), ASSISTANT_META),
        USER_ID => (json!(USER_AT), USER_META),
        CODEX_ID => (json!(CODEX_AT), CODEX_META),
        other => panic!("unexpected seeded message {other}"),
    };
    json!({
        "provider": provider,
        "message_id": message_id,
        "session_id": SESSION,
        "store_id": null,
        "role": role,
        "ordinal": 0,
        "timestamp": timestamp,
        "content": content,
        "content_range": {
            "offset": offset,
            "limit": limit,
            "returned_chars": content.chars().count(),
            "total_chars": total_chars,
            "truncated": truncated
        },
        "content_hash": null,
        "storage_kind": "canonical_occurrence",
        "payload_ref": null,
        "metadata_json": metadata_json
    })
}
