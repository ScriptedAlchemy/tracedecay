//! `tracedecay_diagnostics` as an MCP client calls it: one JSON-RPC
//! `tools/call` on the production composition, then the text and problem
//! record that client observes.

use crate::support::{
    extract_real_server_text, handle_real_server_tool_call, handle_real_server_tool_call_raw,
    production_composition_fixture, wait_for_current_graph,
};
use serde_json::{Value, json};
use tracedecay::mcp::McpServer;

const UNPUBLISHED_CODE: &str = "application.diagnostics.unsupported";
const UNPUBLISHED_MESSAGE: &str = "No diagnostic producer is configured for this scope.";

#[tokio::test]
async fn diagnostics_call_refuses_bad_arguments_and_reports_unpublished_reads() {
    let fixture = production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production diagnostics server");
    wait_for_current_graph(&server).await;

    assert_protocol_refusal(
        &server,
        json!({"scope": "package"}),
        "tool execution failed: config error: application surface request does not match its reviewed schema: `scope` package is not supported for diagnostics",
    )
    .await;
    assert_protocol_refusal(
        &server,
        json!({"scope": "file"}),
        "tool execution failed: config error: application surface request does not match its reviewed schema: `path` is required when `scope` is file",
    )
    .await;
    assert_protocol_refusal(
        &server,
        json!({"scope": "nope"}),
        "tool execution failed: config error: application surface request does not match its reviewed schema: `scope` `nope` is not one of workspace or file",
    )
    .await;
    assert_protocol_refusal(
        &server,
        json!({"format": "yaml"}),
        "tool execution failed: config error: application surface request does not match its reviewed schema: `format` must be markdown or json",
    )
    .await;

    let workspace = call_diagnostics(
        &server,
        json!({"scope": "workspace", "maximum_diagnostics": 5, "format": "json"}),
    )
    .await;
    let indexed_file = call_diagnostics(
        &server,
        json!({
            "scope": "file",
            "path": "src/main.rs",
            "maximum_diagnostics": 5,
            "format": "json"
        }),
    )
    .await;
    let missing_file = call_diagnostics(
        &server,
        json!({
            "scope": "file",
            "path": "src/does-not-exist.rs",
            "maximum_diagnostics": 5,
            "format": "json"
        }),
    )
    .await;
    let zero_page = call_diagnostics(
        &server,
        json!({"scope": "workspace", "maximum_diagnostics": 0, "format": "json"}),
    )
    .await;
    let oversized_page = call_diagnostics(
        &server,
        json!({"scope": "workspace", "maximum_diagnostics": 1001, "format": "json"}),
    )
    .await;
    let markdown =
        call_diagnostics(&server, json!({"scope": "workspace", "format": "markdown"})).await;

    for (label, result) in [
        ("workspace", &workspace),
        ("src/main.rs", &indexed_file),
        ("src/does-not-exist.rs", &missing_file),
        ("maximum_diagnostics 0", &zero_page),
        ("maximum_diagnostics 1001", &oversized_page),
    ] {
        assert_unpublished_json(label, result);
    }

    let workspace_id = request_id(&workspace);
    let file_id = request_id(&indexed_file);
    assert_ne!(
        workspace_id, file_id,
        "each diagnostics call mints its own request id"
    );
    assert_eq!(
        workspace["problem"]["code"], indexed_file["problem"]["code"],
        "a file read with no published generation uses the same authority state as the workspace"
    );

    assert_eq!(markdown["isError"], json!(true), "{markdown}");
    assert_eq!(markdown["problem"]["kind"], "unsupported");
    assert_eq!(markdown["problem"]["code"], UNPUBLISHED_CODE);
    assert_eq!(markdown["problem"]["message"], UNPUBLISHED_MESSAGE);
    assert_eq!(markdown["problem"]["retry"], "never");
    assert_eq!(markdown["problem"]["retryable"], json!(false));
    assert_eq!(markdown["problem"]["legal_actions"], json!([]));
    let markdown_text = extract_real_server_text(&markdown);
    assert!(
        markdown_text
            .starts_with("## diagnostics_read\n\n- Operation: `diagnostics_read`\n- Binding: `"),
        "{markdown_text}"
    );
    assert!(markdown_text.contains("\n- Status: `problem`"));
    assert!(
        markdown_text
            .contains("\n- Contract: `schema.application.primitive.diagnostics-read.result@1`")
    );
    assert!(markdown_text.contains("\n- Problem: `application.diagnostics.unsupported`"));
    assert!(markdown_text.contains("\n- Problem kind: `unsupported`"));
    assert!(markdown_text.contains("\n- Problem revision: `1`"));
    assert!(markdown_text.contains("\n- Owning layer: `application`"));
    assert!(markdown_text.contains("\n- Terminality: `pre_admission`"));
    assert!(
        markdown_text.contains("\n- Message: No diagnostic producer is configured for this scope.")
    );
    assert!(markdown_text.contains("\n- Retryable: `false`"));
    assert!(markdown_text.contains("\n- Retry: `never`"));
    assert!(markdown_text.contains("\n- Retry scope: `none`"));
    assert!(markdown_text.contains("\n- Retry after: `none`"));
    assert!(markdown_text.contains("\n- Legal actions: none"));
    assert!(markdown_text.contains("\n- Coverage: `not_available`"));
    assert!(
        !markdown_text.contains("findings_cleared"),
        "an unpublished read must not render as a clean empty page: {markdown_text}"
    );
    assert_ne!(
        markdown["problem"]["request_id"].as_str().unwrap(),
        workspace_id,
        "the markdown presentation is a separate call"
    );

    fixture.harness.shutdown().await;
}

async fn call_diagnostics(server: &McpServer, arguments: Value) -> Value {
    handle_real_server_tool_call(server, "tracedecay_diagnostics", arguments).await
}

async fn assert_protocol_refusal(server: &McpServer, arguments: Value, message: &str) {
    let response =
        handle_real_server_tool_call_raw(server, "tracedecay_diagnostics", arguments).await;
    assert!(
        response["result"].is_null(),
        "a refused diagnostics argument must not return a tool result: {response}"
    );
    assert_eq!(response["id"], json!(1));
    assert_eq!(response["error"]["code"], json!(-32603));
    assert_eq!(response["error"]["message"], message, "{response}");
    assert_eq!(response["error"]["data"]["tool"], "tracedecay_diagnostics");
}

fn assert_unpublished_json(label: &str, result: &Value) {
    assert_eq!(
        result["isError"],
        json!(true),
        "{label} should be a semantic failure: {result}"
    );
    assert_eq!(result["content"][0]["type"], "text", "{label}: {result}");
    let text = extract_real_server_text(result);
    let payload: Value = serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("{label} text was not JSON ({error}): {text}"));
    assert_eq!(result["problem"], payload["problem"], "{label}: {result}");
    assert!(
        payload.get("outcome").is_none(),
        "{label} must not be an evidence page: {payload}"
    );
    assert_eq!(
        payload["contract"]["schema_id"], "schema.application.primitive.diagnostics-read.result",
        "{label}: {payload}"
    );
    assert_eq!(
        payload["contract"]["schema_revision"],
        json!(1),
        "{label}: {payload}"
    );
    assert_eq!(
        payload["problem"]["kind"], "unsupported",
        "{label}: {payload}"
    );
    assert_eq!(
        payload["problem"]["code"], UNPUBLISHED_CODE,
        "{label}: {payload}"
    );
    assert_eq!(
        payload["problem"]["message"], UNPUBLISHED_MESSAGE,
        "{label}: {payload}"
    );
    assert_eq!(
        payload["problem"]["revision"],
        json!(1),
        "{label}: {payload}"
    );
    assert_eq!(
        payload["problem"]["owning_layer"], "application",
        "{label}: {payload}"
    );
    assert_eq!(
        payload["problem"]["terminality"], "pre_admission",
        "{label}: {payload}"
    );
    assert_eq!(
        payload["problem"]["retryable"],
        json!(false),
        "{label}: {payload}"
    );
    assert_eq!(payload["problem"]["retry"], "never", "{label}: {payload}");
    assert!(
        payload["problem"]["retry_scope"].is_null(),
        "{label}: {payload}"
    );
    assert!(
        payload["problem"]["retry_after_millis"].is_null(),
        "{label}: {payload}"
    );
    assert_eq!(
        payload["problem"]["legal_actions"],
        json!([]),
        "{label}: {payload}"
    );
    assert_eq!(
        payload["problem"]["details"],
        json!([]),
        "{label}: {payload}"
    );
    assert!(
        payload["problem"]["committed_receipt"].is_null(),
        "{label}: {payload}"
    );
    assert!(
        payload["problem"]["cancellation_stage"].is_null(),
        "{label}: {payload}"
    );
    assert!(
        payload["problem"]["unavailable_classification"].is_null(),
        "{label}: {payload}"
    );
    assert_eq!(
        payload["problem"]["diagnostic"],
        json!({
            "code": UNPUBLISHED_CODE,
            "message": UNPUBLISHED_MESSAGE,
        }),
        "{label}: {payload}"
    );
    let request_id = payload["request_id"]
        .as_str()
        .unwrap_or_else(|| panic!("{label} omitted request_id: {payload}"));
    assert!(!request_id.is_empty(), "{label}: {payload}");
    assert_eq!(payload["problem"]["request_id"], request_id, "{label}");
    assert_eq!(payload["problem"]["trace_id"], request_id, "{label}");
}

fn request_id(result: &Value) -> &str {
    result["problem"]["request_id"]
        .as_str()
        .expect("diagnostics problem request id")
}
