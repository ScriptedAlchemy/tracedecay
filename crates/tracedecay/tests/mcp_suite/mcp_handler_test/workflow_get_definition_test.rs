//! Behavior of the live MCP read `tracedecay_workflow_get_definition`.
//!
//! Master does not advertise a tool named `tracedecay_workflow_get`. Agents
//! fetch a definition through this operation. Every assertion below is the
//! JSON-RPC `tools/call` result a host observes against a production daemon
//! composition, not a handler mock.

#![cfg(all(feature = "test-transport", unix))]

use serde_json::{Value, json};

use crate::support::{
    extract_real_server_text, handle_real_server_tool_call, production_composition_fixture,
};

const TOOL: &str = "tracedecay_workflow_get_definition";
const STORED_DEFINITION_ID: &str = "workflow.mcp.get-definition.inspect";
const STORED_STEP_ID: &str = "step.mcp.get-definition.inspect";
const STORED_OPERATION: &str = "operation.work.start_attempt";
const MISSING_DEFINITION_ID: &str = "workflow.mcp.get-definition.missing";
const UNPUBLISHED_VERSION: u64 = 2;
const UNPINNED_DIGEST: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_get_definition_returns_the_stored_definition_and_conceals_a_miss() {
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

    let malformed = call(&server, json!({})).await;
    assert_eq!(malformed.result["isError"], json!(true));
    assert_eq!(malformed.envelope["kind"], "problem");
    assert!(malformed.envelope["value"].get("binding_id").is_none());
    assert_eq!(
        malformed.envelope["value"]["contract"]["schema_id"],
        "schema.tracedecay.http.adapter-problem.v1"
    );
    assert_eq!(
        malformed.envelope["value"]["contract"]["schema_revision"],
        1
    );
    assert_eq!(
        malformed.envelope["value"]["problem"],
        json!({
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
            "request_id": malformed.envelope["value"]["request_id"],
            "trace_id": malformed.envelope["value"]["request_id"],
            "details": [],
            "legal_actions": [],
            "coverage": null
        })
    );

    let version_zero = call(
        &server,
        json!({
            "definition_id": MISSING_DEFINITION_ID,
            "definition_version": 0
        }),
    )
    .await;
    assert_eq!(version_zero.result["isError"], json!(true));
    assert_eq!(version_zero.envelope["kind"], "problem");
    assert_eq!(
        version_zero.envelope["value"]["binding_id"],
        "binding.http.workflow.get_definition"
    );
    assert_eq!(
        version_zero.envelope["value"]["contract"]["schema_id"],
        "schema.workflow.get_definition.result"
    );
    assert_eq!(
        version_zero.envelope["value"]["contract"]["schema_revision"],
        1
    );
    assert_eq!(
        version_zero.envelope["value"]["problem"]["kind"],
        "invalid_request"
    );
    assert_eq!(
        version_zero.envelope["value"]["problem"]["code"],
        "workflow.invalid_request"
    );
    assert_eq!(
        version_zero.envelope["value"]["problem"]["message"],
        "The Workflow application request is invalid"
    );
    assert_eq!(
        version_zero.envelope["value"]["problem"]["owning_layer"],
        "runtime"
    );
    assert_eq!(
        version_zero.envelope["value"]["problem"]["legal_actions"],
        json!(["correct_request"])
    );
    assert_eq!(version_zero.envelope["value"]["problem"]["retry"], "never");
    assert_eq!(
        version_zero.envelope["value"]["problem"]["diagnostic"],
        json!({
            "code": "workflow.invalid_request",
            "message": "The Workflow application request is invalid"
        })
    );

    let definition = admit_definition(&server, &project_id).await;
    let found = call(
        &server,
        json!({
            "definition_id": STORED_DEFINITION_ID,
            "definition_version": 1
        }),
    )
    .await;
    assert_eq!(found.result["isError"], json!(null));
    assert_eq!(found.envelope["kind"], "success");
    assert_eq!(
        found.envelope["value"]["binding_id"],
        "binding.http.workflow.get_definition"
    );
    assert_eq!(
        found.envelope["value"]["contract"]["schema_id"],
        "schema.workflow.get_definition.result"
    );
    assert_eq!(found.envelope["value"]["contract"]["schema_revision"], 1);
    assert_eq!(found.envelope["value"]["outcome"]["outcome"], "evidence");
    assert_eq!(
        found.envelope["value"]["outcome"]["value"]["payload"],
        definition
    );

    let missing = concealed_miss(
        &server,
        json!({
            "definition_id": MISSING_DEFINITION_ID,
            "definition_version": 1
        }),
    )
    .await;
    let unpublished = concealed_miss(
        &server,
        json!({
            "definition_id": STORED_DEFINITION_ID,
            "definition_version": UNPUBLISHED_VERSION
        }),
    )
    .await;
    assert_eq!(
        stable_problem(&missing.problem),
        stable_problem(&unpublished.problem)
    );
    for envelope in [&missing.envelope, &unpublished.envelope] {
        let rendered = envelope.to_string();
        assert!(
            !rendered.contains(STORED_DEFINITION_ID),
            "a concealed get must not echo a stored definition id: {envelope}"
        );
        assert!(
            !rendered.contains(MISSING_DEFINITION_ID),
            "a concealed get must not echo the probed definition id: {envelope}"
        );
    }
}

struct ToolCall {
    result: Value,
    envelope: Value,
}

async fn call(server: &tracedecay::mcp::McpServer, arguments: Value) -> ToolCall {
    call_named(server, TOOL, arguments).await
}

async fn call_named(server: &tracedecay::mcp::McpServer, tool: &str, arguments: Value) -> ToolCall {
    let result = handle_real_server_tool_call(server, tool, arguments).await;
    let envelope = serde_json::from_str(extract_real_server_text(&result))
        .unwrap_or_else(|error| panic!("{tool} returned invalid JSON ({error}): {result}"));
    ToolCall { result, envelope }
}

struct ConcealedMiss {
    envelope: Value,
    problem: Value,
}

async fn concealed_miss(server: &tracedecay::mcp::McpServer, arguments: Value) -> ConcealedMiss {
    let call = call(server, arguments).await;
    assert_eq!(call.result["isError"], json!(true));
    assert_eq!(call.envelope["kind"], "problem");
    assert!(call.envelope["value"].get("binding_id").is_none());
    assert_eq!(
        call.envelope["value"]["contract"]["schema_id"],
        "schema.workflow.get_definition.result"
    );
    assert_eq!(call.envelope["value"]["contract"]["schema_revision"], 1);
    let problem = json!({
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
        "request_id": call.envelope["value"]["request_id"],
        "trace_id": call.envelope["value"]["request_id"],
        "details": [],
        "legal_actions": [],
        "coverage": null
    });
    assert_eq!(call.envelope["value"]["problem"], problem);
    ConcealedMiss {
        envelope: call.envelope,
        problem,
    }
}

/// Store one definition through the public register operation.
///
/// Pin values are not reconstructed here. Validation is the only channel that
/// publishes the live policy, configuration, and catalog digests, so this
/// arrangement consumes those denials and then registers the repaired body.
/// The subject of the test is the subsequent get.
async fn admit_definition(server: &tracedecay::mcp::McpServer, project_id: &str) -> Value {
    let mut definition = json!({
        "definition_id": STORED_DEFINITION_ID,
        "definition_version": 1,
        "project_id": project_id,
        "steps": [{
            "step_id": STORED_STEP_ID,
            "operation": STORED_OPERATION,
            "predecessors": [],
            "inputs": [],
            "outputs": [],
            "fan_out": null
        }],
        "pinned_policy_digest": UNPINNED_DIGEST,
        "pinned_configuration_digest": UNPINNED_DIGEST,
        "pinned_catalog_digest": UNPINNED_DIGEST
    });
    for _ in 0..4 {
        let validated = call_named(
            server,
            "tracedecay_workflow_validate_definition",
            json!({ "definition": definition }),
        )
        .await;
        if validated.envelope["kind"] == "success" {
            let registered = call_named(
                server,
                "tracedecay_workflow_register_definition",
                json!({ "definition": definition }),
            )
            .await;
            assert_eq!(
                registered.envelope["kind"], "success",
                "register must store the admitted definition: {}",
                registered.envelope
            );
            return definition;
        }
        let diagnostic = &validated.envelope["value"]["problem"]["diagnostic"];
        let code = diagnostic["code"].as_str().unwrap_or("");
        let message = diagnostic["message"].as_str().unwrap_or("");
        let Some((field, digest)) = admitted_pin(code, message) else {
            panic!(
                "validation did not admit the definition or name a pin: {}",
                validated.envelope
            );
        };
        definition[field] = json!(digest);
    }
    panic!("validation never admitted the discovered workflow pins");
}

fn admitted_pin<'a>(code: &str, message: &'a str) -> Option<(&'a str, &'a str)> {
    if !code.ends_with(".pin_mismatch") {
        return None;
    }
    let (field, rest) = message.split_once(" expected ")?;
    let (digest, _) = rest.split_once(", observed ")?;
    field.starts_with("pinned_").then_some((field, digest))
}

fn stable_problem(problem: &Value) -> Value {
    let mut stable = problem.clone();
    if let Some(object) = stable.as_object_mut() {
        object.insert("request_id".to_owned(), Value::Null);
        object.insert("trace_id".to_owned(), Value::Null);
    }
    stable
}
