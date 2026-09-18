//! Observable behavior of `tracedecay_workflow_activate_definition`.
//!
//! Calls go through the production MCP server's `tools/call` path. A newly
//! registered candidate starts at revision 1; activation walks
//! candidate → validated → active, so the published disposition is revision 3.
//! An identical request replays that disposition, including the clock the
//! first activation committed, instead of minting a new one. A stale revision
//! and a later activation from `active` are journaled conflicts: both return
//! the same runtime refusal, and neither replaces the committed disposition.

#![cfg(feature = "test-transport")]

use serde_json::{Value, json};

use crate::support::{
    extract_real_server_text, handle_real_server_tool_call, production_composition_fixture,
};

const TOOL: &str = "tracedecay_workflow_activate_definition";
const ACTIVE_ID: &str = "workflow.mcp-activate-definition";
const MISSING_ID: &str = "workflow.mcp-activate-missing";
const UNKNOWN_OP_ID: &str = "workflow.mcp-activate-unknown-operation";
const STALE_CATALOG_ID: &str = "workflow.mcp-activate-stale-catalog";
const STALE_POLICY_ID: &str = "workflow.mcp-activate-stale-policy";
const UNKNOWN_STEP: &str = "step.activate-unknown";
const UNKNOWN_OPERATION: &str = "operation.work.not_a_mounted_operation";
const KNOWN_OPERATION: &str = "operation.work.start_attempt";
const UNPINNED: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
const FAKE_CATALOG: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const FAKE_POLICY: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const ACTIVATE_BINDING: &str = "binding.http.workflow.activate_definition";
const ACTIVATE_SCHEMA: &str = "schema.workflow.activate_definition.result";
const ADAPTER_SCHEMA: &str = "schema.tracedecay.http.adapter-problem.v1";

struct LivePins {
    policy: String,
    configuration: String,
    catalog: String,
}

async fn call_tool(
    server: &tracedecay::mcp::McpServer,
    tool: &str,
    arguments: Value,
) -> (Value, Value) {
    let result = handle_real_server_tool_call(server, tool, arguments).await;
    let envelope = serde_json::from_str(extract_real_server_text(&result))
        .unwrap_or_else(|error| panic!("{tool} returned invalid JSON ({error}): {result}"));
    (result, envelope)
}

async fn activate(
    server: &tracedecay::mcp::McpServer,
    definition_id: &str,
    definition_version: u64,
    expected_revision: u64,
) -> (Value, Value) {
    call_tool(
        server,
        TOOL,
        json!({
            "definition_id": definition_id,
            "definition_version": definition_version,
            "expected_revision": expected_revision
        }),
    )
    .await
}

fn definition(
    definition_id: &str,
    project_id: &str,
    step_id: &str,
    operation: &str,
    policy: &str,
    configuration: &str,
    catalog: &str,
) -> Value {
    json!({
        "definition_id": definition_id,
        "definition_version": 1,
        "project_id": project_id,
        "steps": [{
            "step_id": step_id,
            "operation": operation,
            "predecessors": [],
            "inputs": [],
            "outputs": [],
            "fan_out": null
        }],
        "pinned_policy_digest": policy,
        "pinned_configuration_digest": configuration,
        "pinned_catalog_digest": catalog
    })
}

async fn register(server: &tracedecay::mcp::McpServer, body: &Value) {
    let (result, envelope) = call_tool(
        server,
        "tracedecay_workflow_register_definition",
        json!({ "definition": body }),
    )
    .await;
    assert_eq!(result.get("isError"), None, "{envelope}");
    assert_eq!(
        envelope.pointer("/value/outcome/value/payload/definition_id"),
        Some(&body["definition_id"]),
        "{envelope}"
    );
    assert_eq!(
        envelope.pointer("/value/outcome/value/payload/definition_version"),
        Some(&json!(1)),
        "{envelope}"
    );
}

/// The daemon publishes live pins only as validation denials. Repair each
/// named pin until validation admits the definition.
async fn discover_live_pins(server: &tracedecay::mcp::McpServer, project_id: &str) -> LivePins {
    let mut policy = UNPINNED.to_owned();
    let mut configuration = UNPINNED.to_owned();
    let mut catalog = UNPINNED.to_owned();
    for _ in 0..4 {
        let body = definition(
            "workflow.mcp-activate-pin-probe",
            project_id,
            "prepare",
            KNOWN_OPERATION,
            &policy,
            &configuration,
            &catalog,
        );
        let (_result, envelope) = call_tool(
            server,
            "tracedecay_workflow_validate_definition",
            json!({ "definition": body }),
        )
        .await;
        if envelope["kind"] == "success" {
            return LivePins {
                policy,
                configuration,
                catalog,
            };
        }
        let code = envelope
            .pointer("/value/problem/code")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let message = envelope
            .pointer("/value/problem/message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(pin) = code
            .strip_prefix("workflow.")
            .and_then(|rest| rest.strip_suffix(".pin_mismatch"))
        else {
            panic!("pin discovery stopped on a non-pin refusal: {envelope}");
        };
        let prefix = format!("pinned_{pin}_digest expected ");
        let Some(digest) = message
            .strip_prefix(&prefix)
            .and_then(|rest| rest.split_once(", observed "))
            .map(|(digest, _)| digest.to_owned())
        else {
            panic!("pin denial omitted the live digest: {envelope}");
        };
        match pin {
            "policy" => policy = digest,
            "configuration" => configuration = digest,
            "catalog" => catalog = digest,
            _ => panic!("unknown pin {pin}: {envelope}"),
        }
    }
    panic!("validation never admitted the discovered pins");
}

fn problem_record(
    kind: &str,
    code: &str,
    message: &str,
    diagnostic: Value,
    owning_layer: &str,
    legal_actions: Value,
) -> Value {
    json!({
        "revision": 1,
        "kind": kind,
        "code": code,
        "message": message,
        "diagnostic": diagnostic,
        "committed_receipt": null,
        "owning_layer": owning_layer,
        "terminality": "pre_admission",
        "retryable": false,
        "retry": "never",
        "retry_scope": null,
        "retry_after_millis": null,
        "cancellation_stage": null,
        "unavailable_classification": null,
        "execution_failure_classification": null,
        "details": [],
        "legal_actions": legal_actions,
        "coverage": null
    })
}

fn assert_refusal(
    result: &Value,
    envelope: &Value,
    schema_id: &str,
    binding_id: Option<&str>,
    problem: Value,
) {
    assert_eq!(result["isError"], json!(true), "{envelope}");
    assert_eq!(envelope["kind"], json!("problem"), "{envelope}");
    assert_eq!(
        envelope.pointer("/value/contract"),
        Some(&json!({ "schema_id": schema_id, "schema_revision": 1 })),
        "{envelope}"
    );
    match binding_id {
        Some(id) => assert_eq!(
            envelope.pointer("/value/binding_id"),
            Some(&json!(id)),
            "{envelope}"
        ),
        None => assert!(
            envelope.pointer("/value/binding_id").is_none(),
            "{envelope}"
        ),
    }
    let mut observed = envelope["value"]["problem"].clone();
    let object = observed
        .as_object_mut()
        .unwrap_or_else(|| panic!("activate refusal omitted problem: {envelope}"));
    let request_id = object.remove("request_id");
    let trace_id = object.remove("trace_id");
    assert_eq!(request_id, trace_id, "{envelope}");
    assert!(
        request_id
            .as_ref()
            .and_then(Value::as_str)
            .is_some_and(|id| id.starts_with("request.")),
        "{envelope}"
    );
    assert_eq!(observed, problem, "{envelope}");
}

fn assert_application_refusal(result: &Value, envelope: &Value, code: &str, message: &str) {
    assert_refusal(
        result,
        envelope,
        ACTIVATE_SCHEMA,
        Some(ACTIVATE_BINDING),
        problem_record(
            "invalid_request",
            code,
            message,
            json!({ "code": code, "message": message }),
            "application",
            json!(["correct_request"]),
        ),
    );
}

/// Compare-and-swap conflicts that occur inside the effect journal are not the
/// pre-journal catalog diagnostics. Both a stale `expected_revision` and an
/// activation that is illegal from `active` come back as this runtime refusal.
fn assert_journaled_lifecycle_refusal(result: &Value, envelope: &Value) {
    assert_refusal(
        result,
        envelope,
        ACTIVATE_SCHEMA,
        Some(ACTIVATE_BINDING),
        problem_record(
            "invalid_request",
            "workflow.invalid_request",
            "The Workflow application request is invalid",
            json!({
                "code": "workflow.invalid_request",
                "message": "The Workflow application request is invalid"
            }),
            "runtime",
            json!(["correct_request"]),
        ),
    );
}

fn disposition_without_clock(payload: &Value) -> Value {
    let mut value = payload.clone();
    value
        .as_object_mut()
        .unwrap_or_else(|| panic!("disposition was not an object: {payload}"))
        .remove("transitioned_at");
    value
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn activate_definition_publishes_active_revision_three() {
    let _env_lock = super::GLOBAL_DB_ENV_LOCK.lock().await;
    let production = production_composition_fixture().await;
    let project_id = production
        .harness
        .project_id(&production.project_root)
        .await
        .expect("registered fixture project");
    let server = production
        .harness
        .server(&production.project_root)
        .expect("production MCP server");

    let (malformed_result, malformed) = call_tool(
        &server,
        TOOL,
        json!({
            "definition_id": ACTIVE_ID,
            "definition_version": 1
        }),
    )
    .await;
    assert_refusal(
        &malformed_result,
        &malformed,
        ADAPTER_SCHEMA,
        None,
        problem_record(
            "invalid_request",
            "workflow.invalid_request",
            "The Workflow application request is invalid",
            json!({
                "code": "workflow.invalid_request",
                "message": "The Workflow application request is invalid"
            }),
            "adapter",
            json!([]),
        ),
    );

    let (missing_result, missing) = activate(&server, MISSING_ID, 1, 1).await;
    assert_refusal(
        &missing_result,
        &missing,
        ACTIVATE_SCHEMA,
        None,
        problem_record(
            "not_found_or_not_authorized",
            "not_found_or_not_authorized",
            "The requested resource was not found or is not authorized",
            Value::Null,
            "runtime",
            json!([]),
        ),
    );

    let pins = discover_live_pins(&server, &project_id).await;

    register(
        &server,
        &definition(
            STALE_CATALOG_ID,
            &project_id,
            "prepare",
            KNOWN_OPERATION,
            &pins.policy,
            &pins.configuration,
            FAKE_CATALOG,
        ),
    )
    .await;
    let (stale_catalog_result, stale_catalog) = activate(&server, STALE_CATALOG_ID, 1, 1).await;
    assert_application_refusal(
        &stale_catalog_result,
        &stale_catalog,
        "workflow.catalog.pin_mismatch",
        &format!(
            "pinned_catalog_digest expected {catalog}, observed {FAKE_CATALOG}; register a new immutable definition version with the live Work executable catalog digest",
            catalog = pins.catalog
        ),
    );

    register(
        &server,
        &definition(
            UNKNOWN_OP_ID,
            &project_id,
            UNKNOWN_STEP,
            UNKNOWN_OPERATION,
            &pins.policy,
            &pins.configuration,
            &pins.catalog,
        ),
    )
    .await;
    let (unknown_result, unknown) = activate(&server, UNKNOWN_OP_ID, 1, 1).await;
    assert_application_refusal(
        &unknown_result,
        &unknown,
        "workflow.catalog.operation_unknown",
        "steps[step.activate-unknown].operation observed operation.work.not_a_mounted_operation; expected an operation in the live Work executable catalog",
    );

    register(
        &server,
        &definition(
            STALE_POLICY_ID,
            &project_id,
            "prepare",
            KNOWN_OPERATION,
            FAKE_POLICY,
            &pins.configuration,
            &pins.catalog,
        ),
    )
    .await;
    let (stale_policy_result, stale_policy) = activate(&server, STALE_POLICY_ID, 1, 1).await;
    assert_application_refusal(
        &stale_policy_result,
        &stale_policy,
        "workflow.policy.pin_mismatch",
        &format!(
            "pinned_policy_digest expected {policy}, observed {FAKE_POLICY}; register a new immutable definition version with the live registered policy digest",
            policy = pins.policy
        ),
    );

    register(
        &server,
        &definition(
            ACTIVE_ID,
            &project_id,
            "prepare",
            KNOWN_OPERATION,
            &pins.policy,
            &pins.configuration,
            &pins.catalog,
        ),
    )
    .await;
    let (conflict_result, conflict) = activate(&server, ACTIVE_ID, 1, 99).await;
    assert_journaled_lifecycle_refusal(&conflict_result, &conflict);

    let (activated_result, activated) = activate(&server, ACTIVE_ID, 1, 1).await;
    assert_eq!(activated_result.get("isError"), None, "{activated}");
    assert_eq!(activated["kind"], json!("success"), "{activated}");
    assert_eq!(
        activated.pointer("/value/binding_id"),
        Some(&json!(ACTIVATE_BINDING)),
        "{activated}"
    );
    assert_eq!(
        activated.pointer("/value/contract"),
        Some(&json!({ "schema_id": ACTIVATE_SCHEMA, "schema_revision": 1 })),
        "{activated}"
    );
    assert_eq!(
        activated.pointer("/value/outcome/outcome"),
        Some(&json!("effect")),
        "{activated}"
    );
    assert_eq!(
        activated.pointer("/value/outcome/value/effect_class"),
        Some(&json!("administrative")),
        "{activated}"
    );
    assert_eq!(
        activated.pointer("/value/outcome/value/reconciliation"),
        Some(&json!("reconciled")),
        "{activated}"
    );
    assert_eq!(
        activated.pointer("/value/outcome/value/receipt/outcome"),
        Some(&json!("completed")),
        "{activated}"
    );
    assert_eq!(
        activated.pointer("/value/outcome/value/receipt/operation"),
        Some(&json!("use-case.workflow.activate_definition")),
        "{activated}"
    );
    let payload = activated
        .pointer("/value/outcome/value/payload")
        .cloned()
        .unwrap_or_else(|| panic!("activation omitted its disposition: {activated}"));
    assert_eq!(
        disposition_without_clock(&payload),
        json!({
            "definition_id": ACTIVE_ID,
            "definition_version": 1,
            "state": "active",
            "revision": 3
        }),
        "{payload}"
    );
    assert!(
        payload["transitioned_at"].is_i64(),
        "activation must commit a clock: {payload}"
    );
    let effect_id = activated
        .pointer("/value/outcome/value/effect_id")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("activation omitted effect id: {activated}"));
    assert!(
        effect_id.starts_with("effect.work.activate_definition."),
        "{effect_id}"
    );

    let (replay_result, replay) = activate(&server, ACTIVE_ID, 1, 1).await;
    assert_eq!(replay_result.get("isError"), None, "{replay}");
    assert_eq!(
        replay.pointer("/value/outcome/value/payload"),
        Some(&payload),
        "{replay}"
    );
    assert_eq!(
        replay.pointer("/value/outcome/value/effect_id"),
        Some(&json!(effect_id)),
        "{replay}"
    );
    assert_eq!(
        replay.pointer("/value/outcome/value/receipt/outcome"),
        Some(&json!("completed")),
        "{replay}"
    );

    let (illegal_result, illegal) = activate(&server, ACTIVE_ID, 1, 3).await;
    assert_journaled_lifecycle_refusal(&illegal_result, &illegal);

    let (still_active_result, still_active) = activate(&server, ACTIVE_ID, 1, 1).await;
    assert_eq!(still_active_result.get("isError"), None, "{still_active}");
    assert_eq!(
        still_active.pointer("/value/outcome/value/payload"),
        Some(&payload),
        "{still_active}"
    );
    assert_eq!(
        still_active.pointer("/value/outcome/value/effect_id"),
        Some(&json!(effect_id)),
        "{still_active}"
    );
}
