//! Behavior of `tracedecay_configuration_batch` through MCP `tools/call`.
//!
//! The production composition harness is the daemon's MCP server. Expected
//! values are the settings, receipts, and typed refusals a caller observes,
//! not the schema text or a digest recomputed beside the subject.

use std::path::Path;

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_mcp::JsonRpcResponse;

use crate::daemon::ProductionProjectCompositionHarnessV1;

const PREWARM_KEY: &str = "diagnostics.prewarm.v1";
const TIMINGS_KEY: &str = "telemetry.timings.v1";
const BATCH_KEY: &str = "configuration.idempotency.mcp-batch-proof";
const RESTORE_KEY: &str = "configuration.idempotency.mcp-batch-restore";

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

fn boolean_value(value: bool) -> Value {
    json!({"kind": "boolean", "value": value})
}

fn project_layer(project_id: &str) -> Value {
    json!({"kind": "project", "project_id": project_id})
}

fn set_mutation(layer: Value, key: &str, value: Value) -> Value {
    json!({
        "operation": "set",
        "layer": layer,
        "key": key,
        "value": value
    })
}

fn unset_mutation(layer: Value, key: &str) -> Value {
    json!({
        "operation": "unset",
        "layer": layer,
        "key": key
    })
}

fn batch_arguments(mutations: Value, expected_revision: &str, idempotency_key: &str) -> Value {
    json!({
        "mutations": mutations,
        "expected_revision": expected_revision,
        "idempotency_key": idempotency_key,
        "format": "json"
    })
}

async fn call_tool(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    tool_name: &str,
    arguments: Value,
) -> JsonRpcResponse {
    harness
        .call_tool(project, tool_name, arguments)
        .await
        .unwrap_or_else(|error| panic!("{tool_name} MCP tools/call failed: {error}"))
}

fn tool_text(response: &JsonRpcResponse) -> (bool, Value) {
    assert!(
        response.error.is_none(),
        "MCP transport rejected the call: {:?}",
        response.error
    );
    let result = response.result.as_ref().expect("tool result");
    let refused = result["isError"] == true;
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tool result has no text: {result}"));
    let payload = serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("tool result is not JSON: {error}; text={text}"));
    (refused, payload)
}

async fn read_setting(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    key: &str,
) -> Value {
    let response = call_tool(
        harness,
        project,
        "tracedecay_configuration_get",
        json!({"key": key, "format": "json"}),
    )
    .await;
    let (refused, payload) = tool_text(&response);
    assert!(!refused, "{key} read refused: {payload}");
    assert_eq!(payload["outcome"]["outcome"], "evidence");
    payload["outcome"]["value"]["payload"].clone()
}

async fn call_batch(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    arguments: Value,
) -> JsonRpcResponse {
    call_tool(
        harness,
        project,
        "tracedecay_configuration_batch",
        arguments,
    )
    .await
}

fn assert_problem(
    response: &JsonRpcResponse,
    kind: &str,
    code: &str,
    message: &str,
    retry: &str,
    legal_actions: Value,
) {
    let (refused, payload) = tool_text(response);
    assert!(refused, "expected a typed refusal, got {payload}");
    let problem = &payload["problem"];
    assert_eq!(problem["kind"], kind);
    assert_eq!(problem["code"], code);
    assert_eq!(problem["message"], message);
    assert_eq!(problem["retry"], retry);
    assert_eq!(problem["legal_actions"], legal_actions);
    assert_eq!(problem["owning_layer"], "application");
    assert_eq!(problem["terminality"], "pre_admission");
    assert_eq!(problem["committed_receipt"], Value::Null);
    assert_eq!(problem["retryable"], retry != "never");
    assert_eq!(
        problem["retry_scope"],
        if retry == "after_revalidate" {
            json!("fresh_request")
        } else {
            Value::Null
        }
    );
    if code == "not_found_or_not_authorized" {
        assert_eq!(problem["diagnostic"], Value::Null);
    } else {
        assert_eq!(problem["diagnostic"]["code"], code);
        assert_eq!(problem["diagnostic"]["message"], message);
    }
}

fn assert_schema_refusal(response: &JsonRpcResponse) {
    let error = response
        .error
        .as_ref()
        .unwrap_or_else(|| panic!("unknown field must fail before admission: {response:?}"));
    assert!(
        response.result.is_none(),
        "schema refusal is not a tool result"
    );
    assert_eq!(error.code, -32602);
    let data = error.data.as_ref().expect("schema refusal data");
    assert_eq!(data["tool"], "tracedecay_configuration_batch");
    assert_eq!(data["reason_code"], "application_surface_invalid_request");
    assert_eq!(data["kind"], "invalid_request");
    assert_eq!(data["code"], "application_surface_invalid_request");
    assert_eq!(data["retryable"], false);
    assert_eq!(
        error.message,
        "tool project route failed: reason_code=application_surface_invalid_request retryable=false: application surface request does not match its reviewed schema: configuration surface request is inconsistent with the application contract"
    );
}

fn assert_default_candidate(setting: &Value) {
    assert_eq!(
        setting["candidates"],
        json!([{
            "layer": {"kind": "default"},
            "revision_id": "configuration.registry.default.v1",
            "disposition": "defaulted",
            "safe_reason": "registry_default"
        }])
    );
}

fn assert_winning_candidate(setting: &Value, project_id: &str, revision_id: &str) {
    assert_eq!(
        setting["candidates"],
        json!([{
            "layer": {"kind": "project", "project_id": project_id},
            "revision_id": revision_id,
            "disposition": "winning",
            "safe_reason": null
        }])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configuration_batch_applies_both_settings_and_refuses_the_other_inputs() {
    let isolation = TempDir::new().expect("journey isolation");
    let project = isolation.path().join("project");
    initialize_project(&project);
    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("production composition");

    let enrolled = call_tool(
        &harness,
        &project,
        "tracedecay_configuration_get",
        json!({"key": PREWARM_KEY, "format": "json"}),
    )
    .await;
    let (refused, enrolled_payload) = tool_text(&enrolled);
    assert!(!refused, "initial prewarm read refused: {enrolled_payload}");
    let project_id = enrolled_payload["scope"]["project_id"]
        .as_str()
        .expect("enrolled project id")
        .to_owned();
    let prewarm = enrolled_payload["outcome"]["value"]["payload"].clone();
    let timings = read_setting(&harness, &project, TIMINGS_KEY).await;
    let initial_revision = prewarm["revision_id"]
        .as_str()
        .expect("initial revision")
        .to_owned();
    assert_eq!(initial_revision, "configuration.initial.canonical.v1");
    assert_eq!(timings["revision_id"], initial_revision);
    assert_eq!(prewarm["key"], PREWARM_KEY);
    assert_eq!(timings["key"], TIMINGS_KEY);
    assert_eq!(prewarm["effective_value"], boolean_value(false));
    assert_eq!(timings["effective_value"], boolean_value(true));
    assert_default_candidate(&prewarm);
    assert_default_candidate(&timings);

    let layer = project_layer(&project_id);
    let applied = call_batch(
        &harness,
        &project,
        batch_arguments(
            json!([
                set_mutation(layer.clone(), PREWARM_KEY, boolean_value(true)),
                set_mutation(layer.clone(), TIMINGS_KEY, boolean_value(false))
            ]),
            &initial_revision,
            BATCH_KEY,
        ),
    )
    .await;
    let (refused, applied_payload) = tool_text(&applied);
    assert!(!refused, "batch refused: {applied_payload}");
    assert_eq!(
        applied_payload["contract"]["schema_id"],
        "schema.application.configuration.configuration_batch.result"
    );
    assert_eq!(applied_payload["outcome"]["outcome"], "effect");
    let effect = &applied_payload["outcome"]["value"];
    assert_eq!(effect["effect_class"], "configuration_write");
    assert_eq!(effect["reconciliation"], "pending");
    assert_eq!(effect["idempotency_key"], BATCH_KEY);
    assert_eq!(effect["receipt"]["effect_class"], "configuration_write");
    assert_eq!(
        effect["receipt"]["operation"],
        "use-case.application.configuration.batch"
    );
    assert_eq!(effect["receipt"]["outcome"], "completed");
    assert_eq!(effect["receipt"]["idempotency_key"], BATCH_KEY);
    assert_eq!(effect["execution"]["termination"], "completed");
    assert_eq!(effect["execution"]["cancellation"], Value::Null);
    assert_eq!(
        effect["execution"]["started_at"],
        effect["payload"]["created_at"]
    );
    assert_eq!(
        effect["execution"]["ended_at"],
        effect["payload"]["created_at"]
    );
    assert_eq!(
        effect["execution"]["effective_deadline"]["expires_at"],
        effect["payload"]["effective_deadline_at"]
    );
    assert_eq!(effect["payload"]["base_revision_id"], initial_revision);
    let committed_revision = effect["payload"]["result_revision_id"]
        .as_str()
        .expect("committed revision")
        .to_owned();
    assert_ne!(committed_revision, initial_revision);

    let committed_prewarm = read_setting(&harness, &project, PREWARM_KEY).await;
    let committed_timings = read_setting(&harness, &project, TIMINGS_KEY).await;
    assert_eq!(committed_prewarm["effective_value"], boolean_value(true));
    assert_eq!(committed_timings["effective_value"], boolean_value(false));
    assert_eq!(committed_prewarm["revision_id"], committed_revision);
    assert_eq!(committed_timings["revision_id"], committed_revision);
    assert_winning_candidate(&committed_prewarm, &project_id, &committed_revision);
    assert_winning_candidate(&committed_timings, &project_id, &committed_revision);

    let observed = call_tool(
        &harness,
        &project,
        "tracedecay_configuration_observed_state",
        json!({"format": "json"}),
    )
    .await;
    let (refused, observed_payload) = tool_text(&observed);
    assert!(!refused, "observed state refused: {observed_payload}");
    assert_eq!(
        observed_payload["outcome"]["value"]["payload"],
        json!([{
            "component": "configuration.runtime-cache",
            "desired_revision_id": committed_revision,
            "observed_revision_id": committed_revision,
            "last_working_revision_id": committed_revision,
            "restart_required": false,
            "activation_error_code": null,
            "drift": "current"
        }])
    );

    let replay = call_batch(
        &harness,
        &project,
        batch_arguments(
            json!([
                set_mutation(layer.clone(), PREWARM_KEY, boolean_value(true)),
                set_mutation(layer.clone(), TIMINGS_KEY, boolean_value(false))
            ]),
            &initial_revision,
            BATCH_KEY,
        ),
    )
    .await;
    let (refused, replay_payload) = tool_text(&replay);
    assert!(!refused, "exact replay refused: {replay_payload}");
    // The envelope request id is minted per MCP call. The durable effect is not.
    assert_ne!(replay_payload["request_id"], applied_payload["request_id"]);
    assert_eq!(
        replay_payload["outcome"]["value"],
        applied_payload["outcome"]["value"]
    );
    assert_eq!(
        read_setting(&harness, &project, PREWARM_KEY).await["revision_id"],
        committed_revision
    );

    let changed = call_batch(
        &harness,
        &project,
        batch_arguments(
            json!([
                set_mutation(layer.clone(), PREWARM_KEY, boolean_value(false)),
                set_mutation(layer.clone(), TIMINGS_KEY, boolean_value(false))
            ]),
            &initial_revision,
            BATCH_KEY,
        ),
    )
    .await;
    assert_problem(
        &changed,
        "conflict",
        "configuration.conflict",
        "The configuration request conflicts with current state",
        "after_revalidate",
        json!(["refresh"]),
    );

    let stale = call_batch(
        &harness,
        &project,
        batch_arguments(
            json!([
                set_mutation(layer.clone(), PREWARM_KEY, boolean_value(true)),
                set_mutation(layer.clone(), TIMINGS_KEY, boolean_value(true))
            ]),
            &initial_revision,
            "configuration.idempotency.mcp-batch-stale",
        ),
    )
    .await;
    assert_problem(
        &stale,
        "conflict",
        "configuration.conflict",
        "The configuration request conflicts with current state",
        "after_revalidate",
        json!(["refresh"]),
    );

    // Grant admission collapses an empty batch and a mixed-layer batch to one
    // target refusal. Duplicate keys pass that gate and keep their own message.
    let empty = call_batch(
        &harness,
        &project,
        batch_arguments(
            json!([]),
            &committed_revision,
            "configuration.idempotency.mcp-batch-empty",
        ),
    )
    .await;
    assert_problem(
        &empty,
        "invalid_request",
        "configuration.invalid_request",
        "The configuration request is invalid: invalid configuration mutation target",
        "never",
        json!([]),
    );

    let duplicate = call_batch(
        &harness,
        &project,
        batch_arguments(
            json!([
                set_mutation(layer.clone(), PREWARM_KEY, boolean_value(false)),
                set_mutation(layer.clone(), PREWARM_KEY, boolean_value(true))
            ]),
            &committed_revision,
            "configuration.idempotency.mcp-batch-duplicate",
        ),
    )
    .await;
    assert_problem(
        &duplicate,
        "invalid_request",
        "configuration.invalid_request",
        "The configuration request is invalid: direct configuration batch contains duplicate keys",
        "never",
        json!([]),
    );

    let mixed = call_batch(
        &harness,
        &project,
        batch_arguments(
            json!([
                set_mutation(layer.clone(), PREWARM_KEY, boolean_value(false)),
                set_mutation(json!({"kind": "default"}), TIMINGS_KEY, boolean_value(true))
            ]),
            &committed_revision,
            "configuration.idempotency.mcp-batch-layers",
        ),
    )
    .await;
    assert_problem(
        &mixed,
        "invalid_request",
        "configuration.invalid_request",
        "The configuration request is invalid: invalid configuration mutation target",
        "never",
        json!([]),
    );

    let unknown_key = call_batch(
        &harness,
        &project,
        batch_arguments(
            json!([set_mutation(
                layer.clone(),
                "not.a.setting",
                boolean_value(true)
            )]),
            &committed_revision,
            "configuration.idempotency.mcp-batch-unknown-key",
        ),
    )
    .await;
    assert_problem(
        &unknown_key,
        "invalid_request",
        "configuration.invalid_request",
        "The configuration request is invalid: setting key is not registered: not.a.setting",
        "never",
        json!([]),
    );

    let wrong_type = call_batch(
        &harness,
        &project,
        batch_arguments(
            json!([
                set_mutation(layer.clone(), PREWARM_KEY, boolean_value(false)),
                set_mutation(
                    layer.clone(),
                    TIMINGS_KEY,
                    json!({"kind": "unsigned", "value": 1})
                )
            ]),
            &committed_revision,
            "configuration.idempotency.mcp-batch-type",
        ),
    )
    .await;
    assert_problem(
        &wrong_type,
        "invalid_request",
        "configuration.invalid_request",
        "The configuration request is invalid: setting value kind does not match telemetry.timings.v1: expected Boolean, got Unsigned",
        "never",
        json!([]),
    );

    let foreign = call_batch(
        &harness,
        &project,
        batch_arguments(
            json!([set_mutation(
                project_layer("project.configuration-batch.foreign"),
                PREWARM_KEY,
                boolean_value(false)
            )]),
            &committed_revision,
            "configuration.idempotency.mcp-batch-foreign",
        ),
    )
    .await;
    assert_problem(
        &foreign,
        "not_found_or_not_authorized",
        "not_found_or_not_authorized",
        "The requested resource was not found or is not authorized",
        "never",
        json!([]),
    );

    let widening = call_batch(
        &harness,
        &project,
        batch_arguments(
            json!([set_mutation(
                layer.clone(),
                "scope.source_bindings.v1",
                json!({"kind": "source_bindings", "value": []})
            )]),
            &committed_revision,
            "configuration.idempotency.mcp-batch-policy",
        ),
    )
    .await;
    assert_problem(
        &widening,
        "invalid_request",
        "configuration.policy_widening_forbidden",
        "Configuration policy widening is forbidden",
        "never",
        json!([]),
    );

    let mut unknown_field = batch_arguments(
        json!([set_mutation(
            layer.clone(),
            PREWARM_KEY,
            boolean_value(false)
        )]),
        &committed_revision,
        "configuration.idempotency.mcp-batch-unknown-field",
    );
    unknown_field["repository_path"] = json!("/tmp/not-a-repository");
    let schema = call_batch(&harness, &project, unknown_field).await;
    assert_schema_refusal(&schema);

    let unchanged_prewarm = read_setting(&harness, &project, PREWARM_KEY).await;
    let unchanged_timings = read_setting(&harness, &project, TIMINGS_KEY).await;
    assert_eq!(unchanged_prewarm["effective_value"], boolean_value(true));
    assert_eq!(unchanged_timings["effective_value"], boolean_value(false));
    assert_eq!(unchanged_prewarm["revision_id"], committed_revision);
    assert_eq!(unchanged_timings["revision_id"], committed_revision);

    let restored = call_batch(
        &harness,
        &project,
        batch_arguments(
            json!([
                unset_mutation(layer.clone(), PREWARM_KEY),
                unset_mutation(layer, TIMINGS_KEY)
            ]),
            &committed_revision,
            RESTORE_KEY,
        ),
    )
    .await;
    let (refused, restored_payload) = tool_text(&restored);
    assert!(!refused, "restore batch refused: {restored_payload}");
    assert_eq!(
        restored_payload["outcome"]["value"]["payload"]["base_revision_id"],
        committed_revision
    );
    let restored_revision = restored_payload["outcome"]["value"]["payload"]["result_revision_id"]
        .as_str()
        .expect("restored revision")
        .to_owned();
    assert_ne!(restored_revision, committed_revision);
    let restored_prewarm = read_setting(&harness, &project, PREWARM_KEY).await;
    let restored_timings = read_setting(&harness, &project, TIMINGS_KEY).await;
    assert_eq!(restored_prewarm["effective_value"], boolean_value(false));
    assert_eq!(restored_timings["effective_value"], boolean_value(true));
    assert_eq!(restored_prewarm["revision_id"], restored_revision);
    assert_eq!(restored_timings["revision_id"], restored_revision);
    assert_default_candidate(&restored_prewarm);
    assert_default_candidate(&restored_timings);

    harness.shutdown().await;
}
