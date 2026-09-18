//! `tracedecay_workflow_list_definitions` through the production MCP server.
//!
//! The list operation has no query fields. An empty body returns every
//! registered definition, ordered by identity then version. An unknown field
//! is refused before admission and does not change that list.

#![cfg(feature = "test-transport")]

use serde_json::{Value, json};

use crate::support::{
    extract_real_server_text, handle_real_server_tool_call, production_composition_fixture,
};

const POLICY_DIGEST: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CONFIGURATION_DIGEST: &str =
    "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const CATALOG_DIGEST: &str =
    "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

fn definition(project_id: &str, definition_id: &str, version: u64) -> Value {
    json!({
        "definition_id": definition_id,
        "definition_version": version,
        "project_id": project_id,
        "pinned_policy_digest": POLICY_DIGEST,
        "pinned_configuration_digest": CONFIGURATION_DIGEST,
        "pinned_catalog_digest": CATALOG_DIGEST,
        "steps": [{
            "step_id": "step.mcp-list-proof",
            "operation": "operation.work.start_attempt",
            "predecessors": [],
            "inputs": [],
            "outputs": [],
            "fan_out": null
        }]
    })
}

async fn call(server: &tracedecay::mcp::McpServer, tool: &str, arguments: Value) -> (Value, Value) {
    let result = handle_real_server_tool_call(server, tool, arguments).await;
    let envelope: Value = serde_json::from_str(extract_real_server_text(&result))
        .unwrap_or_else(|error| panic!("{tool} returned invalid JSON ({error}): {result}"));
    (result, envelope)
}

fn assert_list_success(result: &Value, envelope: &Value, payload: &Value) {
    assert_ne!(
        result["isError"], true,
        "a successful list must not be an MCP error: {envelope}"
    );
    assert_eq!(envelope["kind"], "success", "{envelope}");
    assert_eq!(
        envelope["value"]["binding_id"], "binding.http.workflow.list_definitions",
        "{envelope}"
    );
    assert_eq!(
        envelope["value"]["contract"],
        json!({
            "schema_id": "schema.workflow.list_definitions.result",
            "schema_revision": 1
        }),
        "{envelope}"
    );
    assert_eq!(
        envelope["value"]["outcome"]["outcome"], "evidence",
        "{envelope}"
    );
    assert_eq!(
        envelope["value"]["outcome"]["value"]["payload"], payload,
        "{envelope}"
    );
}

/// Registers a definition so the list subject has a known row. Registration
/// itself is not the assertion.
async fn register(server: &tracedecay::mcp::McpServer, definition: &Value) {
    let (_result, envelope) = call(
        server,
        "tracedecay_workflow_register_definition",
        json!({ "definition": definition }),
    )
    .await;
    assert_eq!(
        envelope["kind"], "success",
        "definition registration is setup for the list read: {envelope}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn listed_definitions_are_the_registered_rows_and_unknown_fields_are_refused() {
    let production = production_composition_fixture().await;
    let server = production
        .harness
        .server(&production.project_root)
        .expect("production MCP server");
    let project_id = production
        .harness
        .project_id(&production.project_root)
        .await
        .expect("registered project identity");

    let (empty_result, empty) =
        call(&server, "tracedecay_workflow_list_definitions", json!({})).await;
    assert_list_success(&empty_result, &empty, &json!([]));

    let alpha_v1 = definition(&project_id, "workflow.mcp-list.alpha", 1);
    let alpha_v2 = definition(&project_id, "workflow.mcp-list.alpha", 2);
    let beta_v1 = definition(&project_id, "workflow.mcp-list.beta", 1);
    register(&server, &alpha_v1).await;

    let (one_result, one) = call(&server, "tracedecay_workflow_list_definitions", json!({})).await;
    assert_list_success(&one_result, &one, &json!([alpha_v1]));

    register(&server, &alpha_v2).await;
    register(&server, &beta_v1).await;

    let expected = json!([
        definition(&project_id, "workflow.mcp-list.alpha", 1),
        definition(&project_id, "workflow.mcp-list.alpha", 2),
        definition(&project_id, "workflow.mcp-list.beta", 1),
    ]);
    let (listed_result, listed) =
        call(&server, "tracedecay_workflow_list_definitions", json!({})).await;
    assert_list_success(&listed_result, &listed, &expected);

    let (refused_result, refused) = call(
        &server,
        "tracedecay_workflow_list_definitions",
        json!({ "limit": 1 }),
    )
    .await;
    assert_eq!(
        refused_result["isError"], true,
        "an unknown list field must be an MCP semantic error: {refused}"
    );
    assert_eq!(refused["kind"], "problem", "{refused}");
    assert!(
        refused["value"].get("binding_id").is_none(),
        "pre-admission refusal conceals the binding id: {refused}"
    );
    assert_eq!(
        refused["value"]["contract"],
        json!({
            "schema_id": "schema.tracedecay.http.adapter-problem.v1",
            "schema_revision": 1
        }),
        "{refused}"
    );
    let request_id = refused["value"]["request_id"]
        .as_str()
        .expect("problem request id");
    assert_eq!(refused["value"]["problem"]["request_id"], request_id);
    assert_eq!(refused["value"]["problem"]["trace_id"], request_id);
    assert_eq!(refused["value"]["problem"]["revision"], 1, "{refused}");
    assert_eq!(refused["value"]["problem"]["kind"], "invalid_request");
    assert_eq!(
        refused["value"]["problem"]["code"],
        "workflow.invalid_request"
    );
    assert_eq!(
        refused["value"]["problem"]["message"],
        "The Workflow application request is invalid"
    );
    assert_eq!(
        refused["value"]["problem"]["diagnostic"],
        json!({
            "code": "workflow.invalid_request",
            "message": "The Workflow application request is invalid"
        })
    );
    assert_eq!(refused["value"]["problem"]["owning_layer"], "adapter");
    assert_eq!(refused["value"]["problem"]["terminality"], "pre_admission");
    assert_eq!(refused["value"]["problem"]["retryable"], false);
    assert_eq!(refused["value"]["problem"]["retry"], "never");
    assert_eq!(refused["value"]["problem"]["legal_actions"], json!([]));
    assert_eq!(
        refused["value"]["problem"]["committed_receipt"],
        Value::Null
    );
    assert_eq!(refused["value"]["problem"]["details"], json!([]));

    let (after_refusal_result, after_refusal) =
        call(&server, "tracedecay_workflow_list_definitions", json!({})).await;
    assert_list_success(&after_refusal_result, &after_refusal, &expected);
}
