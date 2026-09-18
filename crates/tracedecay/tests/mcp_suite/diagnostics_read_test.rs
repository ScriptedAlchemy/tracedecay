//! `tracedecay_diagnostics` is the public MCP spelling of `diagnostics_read`.
//!
//! These tests call that tool through a real MCP `tools/call` on the production
//! composition. A workspace with no diagnostic publication must not look like a
//! clean page: callers have to see the missing producer.

use serde_json::{Value, json};

use crate::support::{
    extract_real_server_text, handle_real_server_tool_call, handle_real_server_tool_call_raw,
    production_composition_fixture,
};

const JSON_RPC_ID_ONE_DIGEST: &str = "6b86b273ff34fce19d6b804eff5a3f57";
const ABSENT_PRODUCER_CODE: &str = "application.diagnostics.unsupported";
const ABSENT_PRODUCER_MESSAGE: &str = "No diagnostic producer is configured for this scope.";

#[tokio::test]
async fn diagnostics_read_names_a_missing_producer_and_rejects_a_bad_scope() {
    let fixture = production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");

    for arguments in [
        json!({"scope": "workspace", "maximum_diagnostics": 1}),
        json!({"scope": "file", "path": "src/main.rs", "maximum_diagnostics": 1}),
    ] {
        let result =
            handle_real_server_tool_call(&server, "tracedecay_diagnostics", arguments).await;
        assert_absent_producer(&result);
    }

    let markdown = handle_real_server_tool_call(
        &server,
        "tracedecay_diagnostics",
        json!({"scope": "workspace", "maximum_diagnostics": 1, "format": "markdown"}),
    )
    .await;
    assert_absent_producer_markdown(&markdown);

    let missing_path = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_diagnostics",
        json!({"scope": "file", "maximum_diagnostics": 1}),
    )
    .await;
    assert_eq!(
        missing_path["error"],
        json!({
            "code": -32602,
            "message": "tool project route failed: reason_code=application_surface_invalid_request retryable=false: application surface request does not match its reviewed schema: `path` is required when `scope` is file",
            "data": {
                "tool": "tracedecay_diagnostics",
                "reason_code": "application_surface_invalid_request",
                "retryable": false,
                "detail": "application surface request does not match its reviewed schema: `path` is required when `scope` is file",
                "kind": "invalid_request",
                "code": "application_surface_invalid_request"
            }
        })
    );

    let package_scope = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_diagnostics",
        json!({"scope": "package", "path": "src"}),
    )
    .await;
    assert_eq!(
        package_scope["error"],
        json!({
            "code": -32602,
            "message": "tool project route failed: reason_code=application_surface_invalid_request retryable=false: application surface request does not match its reviewed schema: `scope` package is not supported for diagnostics",
            "data": {
                "tool": "tracedecay_diagnostics",
                "reason_code": "application_surface_invalid_request",
                "retryable": false,
                "detail": "application surface request does not match its reviewed schema: `scope` package is not supported for diagnostics",
                "kind": "invalid_request",
                "code": "application_surface_invalid_request"
            }
        })
    );

    fixture.harness.shutdown().await;
}

fn assert_absent_producer(result: &Value) {
    assert_eq!(result["isError"], json!(true));
    assert_eq!(result["content"][0]["type"], "text");
    let text = extract_real_server_text(result);
    let envelope: Value = serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("diagnostics JSON should be the problem envelope: {error}\n{text}")
    });
    let request_id = assert_mcp_request_id(envelope["request_id"].as_str());
    assert_eq!(envelope, absent_producer_envelope(&request_id));
    assert_eq!(result["problem"], envelope["problem"]);
}

fn assert_absent_producer_markdown(result: &Value) {
    assert_eq!(result["isError"], json!(true));
    let request_id = assert_mcp_request_id(result["problem"]["request_id"].as_str());
    assert_eq!(result["problem"], absent_producer_problem(&request_id));
    assert_eq!(
        extract_real_server_text(result),
        format!(
            "\
## diagnostics\\_read
- Operation: `diagnostics_read`
- Binding: `binding.mcp.diagnostics_read.v1`
- Status: `problem`
- Contract: `schema.application.primitive.diagnostics-read.result@1`
- Problem: `{ABSENT_PRODUCER_CODE}`
- Problem kind: `unsupported`
- Problem revision: `1`
- Owning layer: `application`
- Terminality: `pre_admission`
- Request: `{request_id}`
- Trace: `{request_id}`
- Message: {ABSENT_PRODUCER_MESSAGE}
- Retryable: `false`
- Retry: `never`
- Retry scope: `none`
- Retry after: `none`
- Cancellation stage: `none`
- Details: none
- Legal actions: `none`
- Coverage: `not_available`"
        )
    );
}

fn assert_mcp_request_id(request_id: Option<&str>) -> String {
    let request_id = request_id.expect("diagnostics request id");
    assert!(
        request_id.starts_with("request.mcp.")
            && request_id.ends_with(&format!(".{JSON_RPC_ID_ONE_DIGEST}")),
        "diagnostics request id must bind JSON-RPC id 1, got {request_id}"
    );
    request_id.to_owned()
}

fn absent_producer_envelope(request_id: &str) -> Value {
    json!({
        "contract": {
            "schema_id": "schema.application.primitive.diagnostics-read.result",
            "schema_revision": 1
        },
        "request_id": request_id,
        "problem": absent_producer_problem(request_id)
    })
}

fn absent_producer_problem(request_id: &str) -> Value {
    json!({
        "revision": 1,
        "kind": "unsupported",
        "code": ABSENT_PRODUCER_CODE,
        "message": ABSENT_PRODUCER_MESSAGE,
        "diagnostic": {
            "code": ABSENT_PRODUCER_CODE,
            "message": ABSENT_PRODUCER_MESSAGE
        },
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
        "request_id": request_id,
        "trace_id": request_id,
        "details": [],
        "legal_actions": [],
        "coverage": null
    })
}
