//! `tracedecay_configuration_set` through the production MCP `tools/call` path.
//!
//! Each assertion is a field a client reads from that response, or the
//! configuration get that follows it. Generated revision ids are compared
//! across the write and the later read; they are not pinned.

use std::path::Path;

use serde_json::{Value, json};
use tempfile::TempDir;

use super::ProductionProjectCompositionHarnessV1;
use super::journey_test_support::tool_answer;

const SETTING_KEY: &str = "diagnostics.prewarm.v1";
const ACCEPTED_KEY: &str = "configuration.idempotency.mcp-configuration-set";

fn initialize_project(project: &Path) {
    std::fs::create_dir_all(project.join("src")).expect("project source");
    std::fs::write(project.join("src/lib.rs"), "pub fn probe() {}\n").expect("project source file");
    let status = std::process::Command::new("git")
        .current_dir(project)
        .args(["init", "--quiet"])
        .status()
        .expect("initialize Git project");
    assert!(status.success(), "Git project initialization failed");
}

struct McpAnswer {
    refused: bool,
    payload: Value,
    result: Value,
}

async fn tools_call(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    tool_name: &str,
    arguments: Value,
) -> McpAnswer {
    let response = harness
        .call_tool(project, tool_name, arguments)
        .await
        .unwrap_or_else(|error| panic!("{tool_name} MCP call failed: {error}"));
    assert!(
        response.error.is_none(),
        "{tool_name} returned a JSON-RPC error: {:?}",
        response.error
    );
    let (refused, payload) = tool_answer(&response);
    let result = response.result.expect("MCP tool result");
    McpAnswer {
        refused,
        payload,
        result,
    }
}

fn project_layer(project_id: &str) -> Value {
    json!({
        "kind": "project",
        "project_id": project_id,
    })
}

fn set_arguments(
    layer: Value,
    key: &str,
    value: Value,
    expected_revision: &str,
    idempotency_key: &str,
) -> Value {
    json!({
        "layer": layer,
        "key": key,
        "value": value,
        "expected_revision": expected_revision,
        "idempotency_key": idempotency_key,
        "format": "json",
    })
}

fn boolean_value(value: bool) -> Value {
    json!({"kind": "boolean", "value": value})
}

fn setting_payload<'a>(answer: &'a McpAnswer) -> &'a Value {
    &answer.payload["outcome"]["value"]["payload"]
}

fn revision_id(answer: &McpAnswer) -> String {
    setting_payload(answer)["revision_id"]
        .as_str()
        .unwrap_or_else(|| panic!("configuration get omitted revision_id: {}", answer.payload))
        .to_owned()
}

async fn read_setting(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
) -> McpAnswer {
    tools_call(
        harness,
        project,
        "tracedecay_configuration_get",
        json!({"key": SETTING_KEY, "format": "json"}),
    )
    .await
}

fn assert_read_is(answer: &McpAnswer, value: bool, revision: &str) {
    assert!(
        !answer.refused,
        "configuration get refused: {}",
        answer.payload
    );
    assert_ne!(answer.result["isError"], true);
    assert_eq!(answer.payload["outcome"]["outcome"], "evidence");
    assert_eq!(setting_payload(answer)["key"], SETTING_KEY);
    assert_eq!(
        setting_payload(answer)["effective_value"],
        boolean_value(value)
    );
    assert_eq!(setting_payload(answer)["revision_id"], revision);
}

fn assert_completed_effect(
    answer: &McpAnswer,
    idempotency_key: &str,
    base_revision: &str,
) -> String {
    assert!(
        !answer.refused,
        "configuration set refused: {}",
        answer.payload
    );
    assert_ne!(answer.result["isError"], true);
    assert!(answer.result.get("problem").is_none());
    let effect = &answer.payload["outcome"]["value"];
    assert_eq!(answer.payload["outcome"]["outcome"], "effect");
    assert_eq!(effect["effect_class"], "configuration_write");
    assert_eq!(effect["idempotency_key"], idempotency_key);
    assert_eq!(effect["execution"]["termination"], "completed");
    assert_eq!(effect["reconciliation"], "pending");
    assert_eq!(effect["receipt"]["outcome"], "completed");
    assert_eq!(
        effect["execution"]["started_at"], effect["payload"]["created_at"],
        "the effect clock must be the durable commit time the receipt reports"
    );
    assert_eq!(
        effect["execution"]["effective_deadline"]["expires_at"],
        effect["payload"]["effective_deadline_at"],
        "the effect deadline must be the durable deadline the receipt reports"
    );
    assert_eq!(effect["payload"]["base_revision_id"], base_revision);
    let result_revision = effect["payload"]["result_revision_id"]
        .as_str()
        .unwrap_or_else(|| panic!("configuration set omitted result_revision_id: {effect}"))
        .to_owned();
    assert_ne!(result_revision, base_revision);
    result_revision
}

fn assert_conflict(answer: &McpAnswer) {
    assert!(answer.refused, "conflict must be an MCP isError");
    assert_eq!(answer.result["isError"], true);
    assert_eq!(answer.result["problem"]["kind"], "conflict");
    assert_eq!(answer.result["problem"]["code"], "configuration.conflict");
    assert_eq!(
        answer.result["problem"]["message"],
        "The configuration request conflicts with current state"
    );
    assert_eq!(answer.result["problem"]["retry"], "after_revalidate");
    assert_eq!(answer.result["problem"]["retryable"], true);
    assert_eq!(answer.result["problem"]["retry_scope"], "fresh_request");
    assert_eq!(answer.result["problem"]["terminality"], "pre_admission");
    assert_eq!(answer.result["problem"]["committed_receipt"], Value::Null);
    assert_eq!(
        answer.result["problem"]["legal_actions"],
        json!(["refresh"])
    );
    assert_eq!(answer.payload["problem"]["kind"], "conflict");
    assert_eq!(answer.payload["problem"]["code"], "configuration.conflict");
    assert_eq!(
        answer.payload["problem"]["message"],
        "The configuration request conflicts with current state"
    );
    assert_eq!(answer.payload["problem"]["retry"], "after_revalidate");
    assert_eq!(
        answer.payload["problem"]["legal_actions"],
        json!(["refresh"])
    );
}

fn assert_invalid_request(answer: &McpAnswer, message: &str) {
    assert!(
        answer.refused,
        "invalid configuration set must be an MCP isError"
    );
    assert_eq!(answer.result["isError"], true);
    assert_eq!(
        answer.result["problem"]["kind"], "invalid_request",
        "problem record: {}",
        answer.result["problem"]
    );
    assert_eq!(
        answer.result["problem"]["code"],
        "configuration.invalid_request"
    );
    assert_eq!(
        answer.result["problem"]["message"], message,
        "problem record: {}",
        answer.result["problem"]
    );
    assert_eq!(answer.result["problem"]["retry"], "never");
    assert_eq!(answer.result["problem"]["retryable"], false);
    assert_eq!(answer.result["problem"]["retry_scope"], Value::Null);
    assert_eq!(answer.result["problem"]["terminality"], "pre_admission");
    assert_eq!(answer.result["problem"]["committed_receipt"], Value::Null);
    assert_eq!(answer.result["problem"]["legal_actions"], json!([]));
    assert_eq!(answer.payload["problem"]["kind"], "invalid_request");
    assert_eq!(
        answer.payload["problem"]["code"],
        "configuration.invalid_request"
    );
    assert_eq!(answer.payload["problem"]["message"], message);
    assert_eq!(answer.payload["problem"]["retry"], "never");
    assert_eq!(answer.payload["problem"]["legal_actions"], json!([]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configuration_set_over_mcp_persists_the_boolean_replays_and_refuses_conflicts() {
    let isolation = TempDir::new().expect("journey isolation");
    let project = isolation.path().join("project");
    initialize_project(&project);
    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("production composition");

    let before = read_setting(&harness, &project).await;
    assert!(
        !before.refused,
        "initial configuration get refused: {}",
        before.payload
    );
    assert_eq!(before.payload["outcome"]["outcome"], "evidence");
    assert_eq!(setting_payload(&before)["key"], SETTING_KEY);
    assert_eq!(
        setting_payload(&before)["effective_value"],
        boolean_value(false)
    );
    let project_id = before.payload["scope"]["project_id"]
        .as_str()
        .unwrap_or_else(|| panic!("configuration get omitted project_id: {}", before.payload))
        .to_owned();
    let initial_revision = revision_id(&before);
    let layer = project_layer(&project_id);
    let accepted = set_arguments(
        layer.clone(),
        SETTING_KEY,
        boolean_value(true),
        &initial_revision,
        ACCEPTED_KEY,
    );

    let written = tools_call(
        &harness,
        &project,
        "tracedecay_configuration_set",
        accepted.clone(),
    )
    .await;
    let committed_revision = assert_completed_effect(&written, ACCEPTED_KEY, &initial_revision);
    assert_read_is(
        &read_setting(&harness, &project).await,
        true,
        &committed_revision,
    );

    // Exact replay keeps the original expected revision. A distinct key would
    // be a new CAS write; this key must return the committed effect and leave
    // the revision where the following read finds it.
    let replayed = tools_call(&harness, &project, "tracedecay_configuration_set", accepted).await;
    assert!(
        !replayed.refused,
        "exact replay refused: {}",
        replayed.payload
    );
    assert_eq!(replayed.payload["outcome"]["outcome"], "effect");
    assert_eq!(
        replayed.payload["outcome"]["value"]["idempotency_key"],
        ACCEPTED_KEY
    );
    assert_eq!(
        replayed.payload["outcome"]["value"]["payload"]["result_revision_id"],
        committed_revision
    );
    assert_read_is(
        &read_setting(&harness, &project).await,
        true,
        &committed_revision,
    );

    let same_key_changed_value = tools_call(
        &harness,
        &project,
        "tracedecay_configuration_set",
        set_arguments(
            layer.clone(),
            SETTING_KEY,
            boolean_value(false),
            &committed_revision,
            ACCEPTED_KEY,
        ),
    )
    .await;
    assert_conflict(&same_key_changed_value);
    assert_read_is(
        &read_setting(&harness, &project).await,
        true,
        &committed_revision,
    );

    let stale_revision = tools_call(
        &harness,
        &project,
        "tracedecay_configuration_set",
        set_arguments(
            layer.clone(),
            SETTING_KEY,
            boolean_value(false),
            &initial_revision,
            "configuration.idempotency.mcp-configuration-set-stale",
        ),
    )
    .await;
    assert_conflict(&stale_revision);
    assert_read_is(
        &read_setting(&harness, &project).await,
        true,
        &committed_revision,
    );

    let cleared = tools_call(
        &harness,
        &project,
        "tracedecay_configuration_set",
        set_arguments(
            layer.clone(),
            SETTING_KEY,
            boolean_value(false),
            &committed_revision,
            "configuration.idempotency.mcp-configuration-set-clear",
        ),
    )
    .await;
    let cleared_revision = assert_completed_effect(
        &cleared,
        "configuration.idempotency.mcp-configuration-set-clear",
        &committed_revision,
    );
    assert_ne!(cleared_revision, committed_revision);
    assert_read_is(
        &read_setting(&harness, &project).await,
        false,
        &cleared_revision,
    );

    let wrong_layer = tools_call(
        &harness,
        &project,
        "tracedecay_configuration_set",
        set_arguments(
            json!({"kind": "default"}),
            SETTING_KEY,
            boolean_value(true),
            &cleared_revision,
            "configuration.idempotency.mcp-configuration-set-wrong-layer",
        ),
    )
    .await;
    // A project setting written on the default layer is refused before mutation.
    // The refusal is the shared not-found-or-not-authorized record, not a
    // validation message that would confirm the layer exists.
    assert!(
        wrong_layer.refused,
        "wrong-layer set must be an MCP isError"
    );
    assert_eq!(wrong_layer.result["isError"], true);
    assert_eq!(
        wrong_layer.result["problem"]["kind"],
        "not_found_or_not_authorized"
    );
    assert_eq!(
        wrong_layer.result["problem"]["code"],
        "not_found_or_not_authorized"
    );
    assert_eq!(
        wrong_layer.result["problem"]["message"],
        "The requested resource was not found or is not authorized"
    );
    assert_eq!(wrong_layer.result["problem"]["retry"], "never");
    assert_eq!(wrong_layer.result["problem"]["retryable"], false);
    assert_eq!(wrong_layer.result["problem"]["legal_actions"], json!([]));
    assert_eq!(
        wrong_layer.result["problem"]["committed_receipt"],
        Value::Null
    );
    assert_eq!(
        wrong_layer.payload["problem"]["kind"],
        "not_found_or_not_authorized"
    );
    assert_eq!(
        wrong_layer.payload["problem"]["code"],
        "not_found_or_not_authorized"
    );
    assert_eq!(
        wrong_layer.payload["problem"]["message"],
        "The requested resource was not found or is not authorized"
    );
    assert_read_is(
        &read_setting(&harness, &project).await,
        false,
        &cleared_revision,
    );

    let unknown_key = tools_call(
        &harness,
        &project,
        "tracedecay_configuration_set",
        set_arguments(
            layer.clone(),
            "not.a.setting",
            boolean_value(true),
            &cleared_revision,
            "configuration.idempotency.mcp-configuration-set-unknown-key",
        ),
    )
    .await;
    assert_invalid_request(
        &unknown_key,
        "The configuration request is invalid: setting key is not registered: not.a.setting",
    );
    assert_read_is(
        &read_setting(&harness, &project).await,
        false,
        &cleared_revision,
    );

    let wrong_kind = tools_call(
        &harness,
        &project,
        "tracedecay_configuration_set",
        set_arguments(
            layer,
            SETTING_KEY,
            json!({"kind": "unsigned", "value": 1}),
            &cleared_revision,
            "configuration.idempotency.mcp-configuration-set-wrong-kind",
        ),
    )
    .await;
    assert_invalid_request(
        &wrong_kind,
        "The configuration request is invalid: setting value kind does not match diagnostics.prewarm.v1: expected Boolean, got Unsigned",
    );
    assert_read_is(
        &read_setting(&harness, &project).await,
        false,
        &cleared_revision,
    );

    let malformed = harness
        .call_tool(
            &project,
            "tracedecay_configuration_set",
            json!({
                "layer": project_layer(&project_id),
                "key": SETTING_KEY,
                "value": boolean_value(true),
                "idempotency_key": "configuration.idempotency.mcp-configuration-set-malformed",
                "format": "json",
            }),
        )
        .await
        .expect("malformed configuration set response");
    let error = malformed
        .error
        .as_ref()
        .expect("a set missing expected_revision must be a JSON-RPC error");
    assert_eq!(error.code, -32602);
    let data = error.data.as_ref().expect("typed MCP error data");
    assert_eq!(data["tool"], "tracedecay_configuration_set");
    assert_eq!(data["reason_code"], "application_surface_invalid_request");
    assert_eq!(data["retryable"], false);
    assert_eq!(data["kind"], "invalid_request");
    assert_eq!(data["code"], "application_surface_invalid_request");
    assert_read_is(
        &read_setting(&harness, &project).await,
        false,
        &cleared_revision,
    );

    harness.shutdown().await;
}
