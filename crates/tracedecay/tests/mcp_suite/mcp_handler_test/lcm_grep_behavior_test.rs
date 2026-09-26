//! `tracedecay_lcm_grep` as an agent calls it: one concrete query in, the
//! transcript snippet the agent would read out. The production MCP server is
//! the subject.

#![cfg(feature = "test-transport")]

use crate::support::{
    TemporalLcmProjectionInput, activate_test_temporal_generation, extract_real_server_text,
    handle_real_server_tool_call_raw, open_active_project_session_db,
    persist_temporal_lcm_observation, real_mcp_server, retained_envelope_payload,
    setup_empty_project,
};
use serde_json::{Value, json};
use tracedecay::mcp::McpServer;
use tracedecay_domain::{CanonicalMessageRoleV1, UtcMicros};

const SESSION: &str = "ledger-recall-session";
const OTHER_SESSION: &str = "other-recall-session";
const USER_ID: &str = "msg-user-quicksilver";
const DECOY_ID: &str = "msg-assistant-decoy";
const HASH_ID: &str = "msg-issue-hash";
const CODEX_ID: &str = "msg-codex-quicksilver";
const OTHER_ID: &str = "msg-other-ledger";
const USER_TEXT: &str = "the ledger posts entry 17 against quicksilver";
const DECOY_TEXT: &str = "unrelated orchard balance stays untouched";
const HASH_TEXT: &str = "the log references issue#123 inside a Cursor transcript";
const CODEX_TEXT: &str = "codex also saw quicksilver but not the ledger";
const OTHER_TEXT: &str = "the ledger posts entry 17 in a different session";

/// Score the tool returns for the only exact-message hit from one source.
fn sole_source_score() -> Value {
    serde_json::from_str("3.999999").expect("sole-source score literal")
}

fn hit(
    provider: &str,
    session_id: &str,
    message_id: &str,
    role: &str,
    snippet: &str,
    store_id: i64,
) -> Value {
    json!({
        "kind": "raw_message",
        "provider": provider,
        "session_id": session_id,
        "message_id": message_id,
        "node_id": null,
        "store_id": store_id,
        "role": role,
        "snippet": snippet,
        "score": sole_source_score(),
    })
}

fn page(query: &str, provider: &str, omitted: u64, unknown: u64, hits: Vec<Value>) -> Value {
    let count = hits.len();
    let status = if omitted == 0 { "ok" } else { "partial" };
    json!({
        "status": status,
        "provider": provider,
        "query": query,
        "count": count,
        "sort": "relevance",
        "relationship_scope": "all",
        "message_type": "all",
        "capped_sessions": {},
        "omitted": omitted,
        "coverage": {
            "visible": 0,
            "hidden": 0,
            "unknown": unknown,
            "redacted": 0,
        },
        "hits": hits,
    })
}

/// The caller's page, minus temporal paths and anchor ids that name the temp
/// project. Coverage stays: it is how the caller learns a hit was withheld.
fn caller_page(payload: &Value) -> Value {
    json!({
        "status": payload["status"],
        "provider": payload["provider"],
        "query": payload["query"],
        "count": payload["count"],
        "sort": payload["sort"],
        "relationship_scope": payload["relationship_scope"],
        "message_type": payload["message_type"],
        "capped_sessions": payload["capped_sessions"],
        "omitted": payload["omitted"],
        "coverage": payload["temporal"]["coverage"],
        "hits": payload["hits"],
    })
}

fn jsonrpc_error(response: &Value) -> Value {
    json!({
        "code": response["error"]["code"],
        "message": response["error"]["message"],
        "data": response["error"]["data"],
    })
}

fn argument_error(detail: &str) -> Value {
    let message = format!(
        "tool execution failed: config error: invalid retained application request for tracedecay_lcm_grep: {detail}"
    );
    json!({
        "code": -32603,
        "message": message,
        "data": {
            "tool": "tracedecay_lcm_grep",
            "cli_fallback": "This tool is also available from the shell: `tracedecay tool lcm_grep ...` (`tracedecay tool lcm_grep --help` for parameters). If MCP calls keep failing or timing out, fall back to that CLI instead of querying .tracedecay databases directly.",
        },
    })
}

fn stable_problem(envelope: &Value) -> Value {
    let request_id = envelope["request_id"].clone();
    assert_eq!(
        envelope["problem"]["request_id"], request_id,
        "refusal request identity drifted: {envelope}"
    );
    assert_eq!(
        envelope["problem"]["trace_id"], request_id,
        "refusal trace identity drifted: {envelope}"
    );
    let mut problem = envelope["problem"].clone();
    let Some(object) = problem.as_object_mut() else {
        panic!("refusal problem is not an object: {problem}");
    };
    object.insert("request_id".to_owned(), json!("<request>"));
    object.insert("trace_id".to_owned(), json!("<request>"));
    json!({
        "contract": envelope["contract"],
        "problem": problem,
    })
}

fn invalid_request_refusal() -> Value {
    json!({
        "contract": {
            "schema_id": "schema.application.retained.lcm-grep.result",
            "schema_revision": 1,
        },
        "problem": {
            "revision": 1,
            "kind": "invalid_request",
            "code": "application.retained.invalid-request",
            "message": "The retained operation request is invalid.",
            "diagnostic": {
                "code": "application.retained.invalid-request",
                "message": "The retained operation request is invalid.",
            },
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
            "request_id": "<request>",
            "trace_id": "<request>",
            "details": [],
            "legal_actions": ["correct_request"],
            "coverage": null,
        },
    })
}

fn unsupported_sort_refusal() -> Value {
    json!({
        "contract": {
            "schema_id": "schema.application.retained.lcm-grep.result",
            "schema_revision": 1,
        },
        "problem": {
            "revision": 1,
            "kind": "unsupported",
            "code": "application.retained.unsupported",
            "message": "The retained authority does not support this request.",
            "diagnostic": {
                "code": "application.retained.unsupported",
                "message": "The retained authority does not support this request.",
            },
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
            "request_id": "<request>",
            "trace_id": "<request>",
            "details": [],
            "legal_actions": ["correct_request"],
            "coverage": null,
        },
    })
}

async fn seed(
    cg: &tracedecay_project::project::TraceDecay,
    provider: &str,
    session_id: &str,
    message_id: &str,
    text: &str,
    role: CanonicalMessageRoleV1,
    ordinal: i64,
    timestamp: i64,
) -> TemporalLcmProjectionInput {
    persist_temporal_lcm_observation(
        cg,
        provider,
        session_id,
        message_id,
        text.to_owned(),
        role,
        ordinal,
        timestamp,
        UtcMicros(timestamp),
    )
    .await
}

async fn call(server: &McpServer, args: Value) -> Value {
    let response = handle_real_server_tool_call_raw(server, "tracedecay_lcm_grep", args).await;
    if !response["error"].is_null() {
        return json!({ "jsonrpc_error": jsonrpc_error(&response) });
    }
    let text = extract_real_server_text(&response["result"]);
    if let Some(payload) = retained_envelope_payload(text) {
        return json!({ "page": caller_page(&payload) });
    }
    let envelope: Value = serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("tracedecay_lcm_grep returned neither a page nor a refusal: {error}\n{text}")
    });
    json!({ "refusal": stable_problem(&envelope) })
}

#[tokio::test]
async fn lcm_grep_returns_the_matching_snippet_and_refuses_a_bad_query() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let db = open_active_project_session_db(&cg).await;
    // Activate each session before later observations advance the global
    // observation sequence past that session's frozen source frontier.
    let other = vec![
        seed(
            &cg,
            "cursor",
            OTHER_SESSION,
            OTHER_ID,
            OTHER_TEXT,
            CanonicalMessageRoleV1::User,
            1,
            50,
        )
        .await,
    ];
    activate_test_temporal_generation(&db, OTHER_SESSION, other).await;
    let ledger = vec![
        seed(
            &cg,
            "cursor",
            SESSION,
            USER_ID,
            USER_TEXT,
            CanonicalMessageRoleV1::User,
            1,
            10,
        )
        .await,
        seed(
            &cg,
            "cursor",
            SESSION,
            DECOY_ID,
            DECOY_TEXT,
            CanonicalMessageRoleV1::Assistant,
            2,
            20,
        )
        .await,
        seed(
            &cg,
            "cursor",
            SESSION,
            HASH_ID,
            HASH_TEXT,
            CanonicalMessageRoleV1::Assistant,
            3,
            30,
        )
        .await,
        seed(
            &cg,
            "codex",
            SESSION,
            CODEX_ID,
            CODEX_TEXT,
            CanonicalMessageRoleV1::User,
            4,
            40,
        )
        .await,
    ];
    activate_test_temporal_generation(&db, SESSION, ledger).await;
    let server = real_mcp_server(cg).await;

    let observed = json!({
        "hash_query": call(&server, json!({ "query": "issue#123" })).await,
        "session_phrase": call(&server, json!({
            "query": "ledger posts entry 17",
            "scope": "session",
            "session_id": SESSION,
        })).await,
        "other_session": call(&server, json!({
            "query": "ledger posts entry 17",
            "scope": "session",
            "session_id": OTHER_SESSION,
        })).await,
        "role_assistant": call(&server, json!({
            "query": "orchard balance",
            "scope": "session",
            "session_id": SESSION,
            "role": "assistant",
        })).await,
        "role_user_misses_assistant": call(&server, json!({
            "query": "orchard balance",
            "scope": "session",
            "session_id": SESSION,
            "role": "user",
        })).await,
        "provider_cursor": call(&server, json!({
            "provider": "cursor",
            "query": "quicksilver",
        })).await,
        "provider_codex": call(&server, json!({
            "provider": "codex",
            "query": "quicksilver",
        })).await,
        "provider_default_all": call(&server, json!({ "query": "quicksilver" })).await,
        "time_window": call(&server, json!({
            "query": "quicksilver",
            "start_time": 35,
            "end_time": 45,
        })).await,
        "miss": call(&server, json!({ "query": "qxqvnomatch" })).await,
        "missing_query": call(&server, json!({})).await,
        "bad_scope": call(&server, json!({
            "query": "quicksilver",
            "scope": "everything",
        })).await,
        "blank_query": call(&server, json!({ "query": "  " })).await,
        "session_without_id": call(&server, json!({
            "query": "quicksilver",
            "scope": "session",
        })).await,
        "unsupported_sort": call(&server, json!({
            "query": "quicksilver",
            "sort": "recency",
        })).await,
    });

    let expected = json!({
        "hash_query": { "page": page("issue#123", "all", 1, 1, vec![
            hit("cursor", SESSION, HASH_ID, "assistant", HASH_TEXT, 4),
        ])},
        "session_phrase": { "page": page("ledger posts entry 17", "all", 1, 1, vec![
            hit("cursor", SESSION, USER_ID, "user", USER_TEXT, 2),
        ])},
        "other_session": { "page": page("ledger posts entry 17", "all", 1, 1, vec![
            hit("cursor", OTHER_SESSION, OTHER_ID, "user", OTHER_TEXT, 1),
        ])},
        "role_assistant": { "page": page("orchard balance", "all", 1, 1, vec![
            hit("cursor", SESSION, DECOY_ID, "assistant", DECOY_TEXT, 3),
        ])},
        "role_user_misses_assistant": { "page": page("orchard balance", "all", 0, 0, vec![]) },
        "provider_cursor": { "page": page("quicksilver", "cursor", 1, 1, vec![
            hit("cursor", SESSION, USER_ID, "user", USER_TEXT, 2),
        ])},
        "provider_codex": { "page": page("quicksilver", "codex", 1, 1, vec![
            hit("codex", SESSION, CODEX_ID, "user", CODEX_TEXT, 5),
        ])},
        "provider_default_all": { "page": page("quicksilver", "all", 2, 2, vec![
            hit("codex", SESSION, CODEX_ID, "user", CODEX_TEXT, 5),
            hit("cursor", SESSION, USER_ID, "user", USER_TEXT, 2),
        ])},
        "time_window": { "page": page("quicksilver", "all", 1, 1, vec![
            hit("codex", SESSION, CODEX_ID, "user", CODEX_TEXT, 5),
        ])},
        "miss": { "page": page("qxqvnomatch", "all", 0, 0, vec![]) },
        "missing_query": { "jsonrpc_error": argument_error("missing field `query`") },
        "bad_scope": { "jsonrpc_error": argument_error(
            "scope: unknown variant `everything`, expected one of `current`, `session`, `all`"
        )},
        "blank_query": { "refusal": invalid_request_refusal() },
        "session_without_id": { "refusal": invalid_request_refusal() },
        "unsupported_sort": { "refusal": unsupported_sort_refusal() },
    });
    assert_eq!(observed, expected);

    server.shutdown().await;
}
