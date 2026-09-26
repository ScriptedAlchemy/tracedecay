#![cfg(all(feature = "test-transport", unix))]

//! `tracedecay_workflow_register_definition` as an agent calls it: one
//! `tools/call` against the production MCP server, then the owner's envelope.

use crate::support::{extract_real_server_text, handle_real_server_tool_call_raw};
use serde_json::{Value, json};
use tracedecay::mcp::McpServer;

const POLICY_DIGEST: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CONFIGURATION_DIGEST: &str =
    "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const CATALOG_DIGEST: &str =
    "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

async fn call_register(server: &McpServer, arguments: Value) -> (Value, Value) {
    let response = handle_real_server_tool_call_raw(
        server,
        "tracedecay_workflow_register_definition",
        arguments,
    )
    .await;
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);
    assert!(
        response.get("error").is_none(),
        "a Workflow refusal is a tool result, not a JSON-RPC error: {response}"
    );
    let result = response["result"].clone();
    assert_eq!(result["content"][0]["type"], "text");
    let body = serde_json::from_str(extract_real_server_text(&result))
        .unwrap_or_else(|error| panic!("register_definition JSON ({error}): {result}"));
    (result, body)
}

fn definition(
    project_id: &str,
    definition_id: &str,
    version: u64,
    step_id: &str,
    output: &str,
) -> Value {
    json!({
        "definition_id": definition_id,
        "definition_version": version,
        "project_id": project_id,
        "steps": [{
            "step_id": step_id,
            "operation": "operation.work.start_attempt",
            "predecessors": [],
            "inputs": [],
            "outputs": [output],
            "fan_out": null
        }],
        "pinned_policy_digest": POLICY_DIGEST,
        "pinned_configuration_digest": CONFIGURATION_DIGEST,
        "pinned_catalog_digest": CATALOG_DIGEST
    })
}

fn pin_request_identity(actual: &Value, expected: &mut Value) {
    let request_id = actual
        .pointer("/value/request_id")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("register_definition omitted request_id: {actual}"))
        .to_owned();
    assert!(
        request_id.starts_with("request."),
        "request identity must stay a request id, got {request_id}"
    );
    assert_eq!(
        actual
            .pointer("/value/problem/request_id")
            .and_then(Value::as_str),
        Some(request_id.as_str())
    );
    assert_eq!(
        actual
            .pointer("/value/problem/trace_id")
            .and_then(Value::as_str),
        Some(request_id.as_str())
    );
    for pointer in [
        "/value/request_id",
        "/value/problem/request_id",
        "/value/problem/trace_id",
    ] {
        *expected
            .pointer_mut(pointer)
            .unwrap_or_else(|| panic!("expected envelope missing {pointer}")) = json!(request_id);
    }
}

fn assert_problem(result: &Value, body: &Value, expected: Value) {
    assert_eq!(result["isError"], true);
    let mut expected = expected;
    pin_request_identity(body, &mut expected);
    assert_eq!(body, &expected);
}

fn registered_payload(body: &Value) -> &Value {
    body.pointer("/value/outcome/value/payload")
        .unwrap_or_else(|| panic!("register_definition success omitted payload: {body}"))
}

/// An unknown request body is an adapter refusal, a foreign project is hidden
/// as not-found, the admitted definition is returned verbatim, an exact retry
/// returns that same definition, and a different body under the same id and
/// version is a runtime invalid request that still names the binding.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn register_definition_returns_the_submitted_definition_and_typed_refusals() {
    let production = crate::support::production_composition_fixture().await;
    let project_id = production
        .harness
        .project_id(&production.project_root)
        .await
        .expect("registered fixture project");
    let server = production
        .harness
        .server(&production.project_root)
        .expect("production MCP server");

    let (malformed_result, malformed) = call_register(
        &server,
        json!({
            "not_a_register_field": true
        }),
    )
    .await;
    assert_problem(
        &malformed_result,
        &malformed,
        json!({
            "kind": "problem",
            "value": {
                "contract": {
                    "schema_id": "schema.tracedecay.http.adapter-problem.v1",
                    "schema_revision": 1
                },
                "request_id": "request.placeholder",
                "problem": {
                    "revision": 1,
                    "kind": "invalid_request",
                    "code": "workflow.invalid_request",
                    "message": "The Workflow application request is invalid",
                    "diagnostic": {
                        "code": "workflow.invalid_request",
                        "message": "The Workflow application request is invalid"
                    },
                    "detail": null,
                    "committed_receipt": null,
                    "owning_layer": "adapter",
                    "terminality": "pre_admission",
                    "retryable": false,
                    "retry": "never",
                    "retry_scope": null,
                    "retry_after_millis": null,
                    "cancellation_stage": null,
                    "unavailable_classification": null,
                    "execution_failure_classification": null,
                    "request_id": "request.placeholder",
                    "trace_id": "request.placeholder",
                    "details": [],
                    "legal_actions": [],
                    "coverage": null
                }
            }
        }),
    );

    let foreign = definition(
        "project.not-this-mounted-project",
        "workflow.mcp-register-proof",
        1,
        "prepare",
        "finding",
    );
    let (foreign_result, foreign_body) = call_register(
        &server,
        json!({
            "definition": foreign
        }),
    )
    .await;
    assert_problem(
        &foreign_result,
        &foreign_body,
        json!({
            "kind": "problem",
            "value": {
                "contract": {
                    "schema_id": "schema.workflow.register_definition.result",
                    "schema_revision": 1
                },
                "request_id": "request.placeholder",
                "problem": {
                    "revision": 1,
                    "kind": "not_found_or_not_authorized",
                    "code": "not_found_or_not_authorized",
                    "message": "The requested resource was not found or is not authorized",
                    "diagnostic": null,
                    "detail": null,
                    "committed_receipt": null,
                    "owning_layer": "runtime",
                    "terminality": "pre_admission",
                    "retryable": false,
                    "retry": "never",
                    "retry_scope": null,
                    "retry_after_millis": null,
                    "cancellation_stage": null,
                    "unavailable_classification": null,
                    "execution_failure_classification": null,
                    "request_id": "request.placeholder",
                    "trace_id": "request.placeholder",
                    "details": [],
                    "legal_actions": [],
                    "coverage": null
                }
            }
        }),
    );
    assert!(
        foreign_body.pointer("/value/binding_id").is_none(),
        "a concealed refusal must not reveal the Workflow binding: {foreign_body}"
    );

    let admitted = definition(
        &project_id,
        "workflow.mcp-register-proof",
        1,
        "prepare",
        "finding",
    );
    let (admitted_result, admitted_body) = call_register(
        &server,
        json!({
            "definition": admitted.clone()
        }),
    )
    .await;
    assert!(admitted_result.get("isError").is_none());
    assert_eq!(admitted_body["kind"], "success");
    assert_eq!(
        admitted_body["value"]["binding_id"],
        "binding.http.workflow.register_definition"
    );
    assert_eq!(
        admitted_body["value"]["contract"],
        json!({
            "schema_id": "schema.workflow.register_definition.result",
            "schema_revision": 1
        })
    );
    assert_eq!(admitted_body["value"]["scope"]["project_id"], project_id);
    assert_eq!(admitted_body["value"]["outcome"]["outcome"], "effect");
    assert_eq!(
        admitted_body["value"]["outcome"]["value"]["effect_class"],
        "administrative"
    );
    assert_eq!(
        admitted_body["value"]["outcome"]["value"]["reconciliation"],
        "reconciled"
    );
    assert_eq!(
        admitted_body["value"]["outcome"]["value"]["receipt"]["outcome"],
        "completed"
    );
    assert_eq!(
        admitted_body["value"]["outcome"]["value"]["receipt"]["effect_class"],
        "administrative"
    );
    assert_eq!(registered_payload(&admitted_body), &admitted);
    assert_eq!(
        registered_payload(&admitted_body)["definition_id"],
        "workflow.mcp-register-proof"
    );
    assert_eq!(registered_payload(&admitted_body)["definition_version"], 1);
    assert_eq!(
        registered_payload(&admitted_body)["steps"][0]["step_id"],
        "prepare"
    );
    assert_eq!(
        registered_payload(&admitted_body)["steps"][0]["operation"],
        "operation.work.start_attempt"
    );
    assert_eq!(
        registered_payload(&admitted_body)["steps"][0]["outputs"],
        json!(["finding"])
    );
    assert_eq!(
        registered_payload(&admitted_body)["pinned_policy_digest"],
        POLICY_DIGEST
    );
    assert_eq!(
        registered_payload(&admitted_body)["pinned_configuration_digest"],
        CONFIGURATION_DIGEST
    );
    assert_eq!(
        registered_payload(&admitted_body)["pinned_catalog_digest"],
        CATALOG_DIGEST
    );

    let (replay_result, replay_body) = call_register(
        &server,
        json!({
            "definition": admitted.clone()
        }),
    )
    .await;
    assert!(replay_result.get("isError").is_none());
    assert_eq!(replay_body["kind"], "success");
    assert_eq!(registered_payload(&replay_body), &admitted);

    let replacement = definition(
        &project_id,
        "workflow.mcp-register-proof",
        1,
        "collect",
        "summary",
    );
    let (conflict_result, conflict_body) = call_register(
        &server,
        json!({
            "definition": replacement
        }),
    )
    .await;
    assert_problem(
        &conflict_result,
        &conflict_body,
        json!({
            "kind": "problem",
            "value": {
                "binding_id": "binding.http.workflow.register_definition",
                "contract": {
                    "schema_id": "schema.workflow.register_definition.result",
                    "schema_revision": 1
                },
                "request_id": "request.placeholder",
                "problem": {
                    "revision": 1,
                    "kind": "invalid_request",
                    "code": "workflow.invalid_request",
                    "message": "The Workflow application request is invalid",
                    "diagnostic": {
                        "code": "workflow.invalid_request",
                        "message": "The Workflow application request is invalid"
                    },
                    "detail": null,
                    "committed_receipt": null,
                    "owning_layer": "runtime",
                    "terminality": "pre_admission",
                    "retryable": false,
                    "retry": "never",
                    "retry_scope": null,
                    "retry_after_millis": null,
                    "cancellation_stage": null,
                    "unavailable_classification": null,
                    "execution_failure_classification": null,
                    "request_id": "request.placeholder",
                    "trace_id": "request.placeholder",
                    "details": [],
                    "legal_actions": ["correct_request"],
                    "coverage": null
                }
            }
        }),
    );

    let second_version = definition(
        &project_id,
        "workflow.mcp-register-proof",
        2,
        "collect",
        "summary",
    );
    let (second_result, second_body) = call_register(
        &server,
        json!({
            "definition": second_version.clone()
        }),
    )
    .await;
    assert!(second_result.get("isError").is_none());
    assert_eq!(second_body["kind"], "success");
    assert_eq!(registered_payload(&second_body), &second_version);
    assert_eq!(registered_payload(&second_body)["definition_version"], 2);
    assert_eq!(
        registered_payload(&second_body)["steps"][0]["step_id"],
        "collect"
    );
    assert_eq!(
        registered_payload(&second_body)["steps"][0]["outputs"],
        json!(["summary"])
    );
}
