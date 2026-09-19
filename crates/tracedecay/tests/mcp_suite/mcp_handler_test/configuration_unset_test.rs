//! `tracedecay_configuration_unset` through a production MCP `tools/call`.
//!
//! The effect is the next configuration read, not a dispatch trace. Unset
//! drops the project override, so the read's sole candidate is the registry
//! default `configuration.registry.default.v1`. A repeat of the same request
//! returns that receipt again and does not write another revision.

use crate::support::{
    extract_real_server_text, handle_real_server_tool_call, production_composition_fixture,
};
use serde_json::{Value, json};
use tracedecay::mcp::McpServer;

const PREWARM_KEY: &str = "diagnostics.prewarm.v1";
const UNSET_KEY: &str = "configuration.idempotency.mcp-unset-prewarm";

fn registry_default_candidate() -> Value {
    json!({
        "layer": {"kind": "default"},
        "revision_id": "configuration.registry.default.v1",
        "disposition": "defaulted",
        "safe_reason": "registry_default"
    })
}

fn boolean_value(value: bool) -> Value {
    json!({"kind": "boolean", "value": value})
}

async fn call_mcp(server: &McpServer, tool: &str, arguments: Value) -> (Value, Value) {
    let result = handle_real_server_tool_call(server, tool, arguments).await;
    let parsed = serde_json::from_str(extract_real_server_text(&result))
        .unwrap_or_else(|error| panic!("{tool} returned invalid JSON ({error}): {result}"));
    (result, parsed)
}

fn payload<'a>(envelope: &'a Value, outcome: &str) -> &'a Value {
    assert_eq!(envelope["outcome"]["outcome"], json!(outcome), "{envelope}");
    envelope
        .pointer("/outcome/value/payload")
        .unwrap_or_else(|| panic!("{outcome} envelope omitted its payload: {envelope}"))
}

fn setting(server_result: &(Value, Value)) -> Value {
    payload(&server_result.1, "evidence").clone()
}

async fn read_prewarm(server: &McpServer) -> (Value, Value) {
    call_mcp(
        server,
        "tracedecay_configuration_get",
        json!({"key": PREWARM_KEY}),
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configuration_unset_removes_the_project_override_and_replays() {
    let production = production_composition_fixture().await;
    let server = production
        .harness
        .server(&production.project_root)
        .expect("production MCP server");
    let project_id = production
        .harness
        .project_id(&production.project_root)
        .await
        .expect("registered fixture project");

    let before = setting(&read_prewarm(&server).await);
    assert_eq!(before["key"], json!(PREWARM_KEY));
    assert_eq!(before["effective_value"], boolean_value(false));
    assert_eq!(before["candidates"], json!([registry_default_candidate()]));
    let initial_revision = before["revision_id"]
        .as_str()
        .expect("initial configuration revision")
        .to_owned();

    let (set_result, set_envelope) = call_mcp(
        &server,
        "tracedecay_configuration_set",
        json!({
            "layer": {"kind": "project", "project_id": project_id},
            "key": PREWARM_KEY,
            "value": boolean_value(true),
            "expected_revision": initial_revision,
            "idempotency_key": "configuration.idempotency.mcp-unset-proof-set"
        }),
    )
    .await;
    assert_eq!(set_result["isError"], Value::Null, "{set_envelope}");
    let set_payload = payload(&set_envelope, "effect");
    assert_eq!(
        set_envelope["outcome"]["value"]["receipt"]["operation"],
        json!("use-case.application.configuration.set")
    );
    let override_revision = set_payload["result_revision_id"]
        .as_str()
        .expect("set result revision")
        .to_owned();
    assert_ne!(override_revision, initial_revision);

    let overridden = setting(&read_prewarm(&server).await);
    assert_eq!(overridden["effective_value"], boolean_value(true));
    assert_eq!(overridden["revision_id"], json!(override_revision));
    assert_eq!(
        overridden["candidates"],
        json!([{
            "layer": {"kind": "project", "project_id": project_id},
            "revision_id": override_revision,
            "disposition": "winning",
            "safe_reason": null
        }])
    );

    let (unset_result, unset_envelope) = call_mcp(
        &server,
        "tracedecay_configuration_unset",
        json!({
            "layer": {"kind": "project", "project_id": project_id},
            "key": PREWARM_KEY,
            "expected_revision": override_revision,
            "idempotency_key": UNSET_KEY
        }),
    )
    .await;
    assert_eq!(unset_result["isError"], Value::Null, "{unset_envelope}");
    assert_eq!(unset_envelope["outcome"]["outcome"], json!("effect"));
    let effect = &unset_envelope["outcome"]["value"];
    assert_eq!(effect["effect_class"], json!("configuration_write"));
    assert_eq!(effect["idempotency_key"], json!(UNSET_KEY));
    assert_eq!(
        effect["receipt"]["operation"],
        json!("use-case.application.configuration.unset")
    );
    assert_eq!(
        effect["receipt"]["effect_class"],
        json!("configuration_write")
    );
    assert_eq!(effect["receipt"]["outcome"], json!("completed"));
    assert_eq!(effect["receipt"]["idempotency_key"], json!(UNSET_KEY));
    let unset_payload = &effect["payload"];
    assert_eq!(unset_payload["base_revision_id"], json!(override_revision));
    let restored_revision = unset_payload["result_revision_id"]
        .as_str()
        .expect("unset result revision")
        .to_owned();
    assert_ne!(restored_revision, override_revision);

    let restored = setting(&read_prewarm(&server).await);
    assert_eq!(restored["key"], json!(PREWARM_KEY));
    assert_eq!(restored["effective_value"], boolean_value(false));
    assert_eq!(restored["revision_id"], json!(restored_revision));
    assert_eq!(
        restored["candidates"],
        json!([registry_default_candidate()])
    );

    let (replay_result, replay_envelope) = call_mcp(
        &server,
        "tracedecay_configuration_unset",
        json!({
            "layer": {"kind": "project", "project_id": project_id},
            "key": PREWARM_KEY,
            "expected_revision": override_revision,
            "idempotency_key": UNSET_KEY
        }),
    )
    .await;
    assert_eq!(replay_result["isError"], Value::Null, "{replay_envelope}");
    assert_eq!(replay_envelope["outcome"]["outcome"], json!("effect"));
    assert_eq!(
        replay_envelope["outcome"]["value"]["payload"], *unset_payload,
        "the same unset request must replay the committed receipt"
    );
    assert_eq!(
        replay_envelope["outcome"]["value"]["idempotency_key"],
        json!(UNSET_KEY)
    );
    let after_replay = setting(&read_prewarm(&server).await);
    assert_eq!(after_replay["effective_value"], boolean_value(false));
    assert_eq!(after_replay["revision_id"], json!(restored_revision));
    assert_eq!(
        after_replay["candidates"],
        json!([registry_default_candidate()])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configuration_unset_refuses_stale_protected_unknown_and_default_layer() {
    let production = production_composition_fixture().await;
    let server = production
        .harness
        .server(&production.project_root)
        .expect("production MCP server");
    let project_id = production
        .harness
        .project_id(&production.project_root)
        .await
        .expect("registered fixture project");
    let initial = setting(&read_prewarm(&server).await);
    let initial_revision = initial["revision_id"]
        .as_str()
        .expect("initial configuration revision")
        .to_owned();

    let (_, set_envelope) = call_mcp(
        &server,
        "tracedecay_configuration_set",
        json!({
            "layer": {"kind": "project", "project_id": project_id},
            "key": PREWARM_KEY,
            "value": boolean_value(true),
            "expected_revision": initial_revision,
            "idempotency_key": "configuration.idempotency.mcp-unset-refusal-set"
        }),
    )
    .await;
    let override_revision = payload(&set_envelope, "effect")["result_revision_id"]
        .as_str()
        .expect("set result revision")
        .to_owned();

    let (stale_result, stale) = call_mcp(
        &server,
        "tracedecay_configuration_unset",
        json!({
            "layer": {"kind": "project", "project_id": project_id},
            "key": PREWARM_KEY,
            "expected_revision": "revision.stale-configuration-unset",
            "idempotency_key": "configuration.idempotency.mcp-unset-stale"
        }),
    )
    .await;
    assert_eq!(stale_result["isError"], json!(true), "{stale}");
    assert_eq!(stale["problem"]["kind"], json!("conflict"));
    assert_eq!(stale["problem"]["code"], json!("configuration.conflict"));
    assert_eq!(
        stale["problem"]["message"],
        json!("The configuration request conflicts with current state")
    );
    assert_eq!(
        stale["problem"]["diagnostic"]["code"],
        json!("configuration.conflict")
    );
    assert_eq!(
        stale["problem"]["diagnostic"]["message"],
        json!("The configuration request conflicts with current state")
    );
    assert_eq!(stale["problem"]["retry"], json!("after_revalidate"));
    assert_eq!(stale["problem"]["retryable"], json!(true));
    assert_eq!(stale["problem"]["retry_scope"], json!("fresh_request"));
    assert_eq!(stale["problem"]["legal_actions"], json!(["refresh"]));

    let (protected_result, protected) = call_mcp(
        &server,
        "tracedecay_configuration_unset",
        json!({
            "layer": {"kind": "project", "project_id": project_id},
            "key": "scope.source_bindings.v1",
            "expected_revision": override_revision,
            "idempotency_key": "configuration.idempotency.mcp-unset-protected"
        }),
    )
    .await;
    assert_eq!(protected_result["isError"], json!(true), "{protected}");
    assert_eq!(protected["problem"]["kind"], json!("invalid_request"));
    assert_eq!(
        protected["problem"]["code"],
        json!("configuration.policy_widening_forbidden")
    );
    assert_eq!(
        protected["problem"]["message"],
        json!("Configuration policy widening is forbidden")
    );
    assert_eq!(
        protected["problem"]["diagnostic"]["code"],
        json!("configuration.policy_widening_forbidden")
    );
    assert_eq!(
        protected["problem"]["diagnostic"]["message"],
        json!("Configuration policy widening is forbidden")
    );
    assert_eq!(protected["problem"]["retry"], json!("never"));
    assert_eq!(protected["problem"]["legal_actions"], json!([]));

    let (unknown_result, unknown) = call_mcp(
        &server,
        "tracedecay_configuration_unset",
        json!({
            "layer": {"kind": "project", "project_id": project_id},
            "key": "missing.setting.v1",
            "expected_revision": override_revision,
            "idempotency_key": "configuration.idempotency.mcp-unset-unknown"
        }),
    )
    .await;
    assert_eq!(unknown_result["isError"], json!(true), "{unknown}");
    assert_eq!(unknown["problem"]["kind"], json!("invalid_request"));
    assert_eq!(
        unknown["problem"]["code"],
        json!("configuration.invalid_request")
    );
    assert_eq!(
        unknown["problem"]["message"],
        json!(
            "The configuration request is invalid: setting key is not registered: missing.setting.v1"
        )
    );
    assert_eq!(
        unknown["problem"]["diagnostic"]["message"],
        json!(
            "The configuration request is invalid: setting key is not registered: missing.setting.v1"
        )
    );
    assert_eq!(unknown["problem"]["retry"], json!("never"));
    assert_eq!(unknown["problem"]["legal_actions"], json!([]));

    let (default_layer_result, default_layer) = call_mcp(
        &server,
        "tracedecay_configuration_unset",
        json!({
            "layer": {"kind": "default"},
            "key": PREWARM_KEY,
            "expected_revision": override_revision,
            "idempotency_key": "configuration.idempotency.mcp-unset-default-layer"
        }),
    )
    .await;
    assert_eq!(
        default_layer_result["isError"],
        json!(true),
        "{default_layer}"
    );
    assert_eq!(
        default_layer["problem"]["kind"],
        json!("not_found_or_not_authorized")
    );
    assert_eq!(
        default_layer["problem"]["code"],
        json!("not_found_or_not_authorized")
    );
    assert_eq!(
        default_layer["problem"]["message"],
        json!("The requested resource was not found or is not authorized")
    );
    assert_eq!(default_layer["problem"]["diagnostic"], Value::Null);
    assert_eq!(default_layer["problem"]["retry"], json!("never"));
    assert_eq!(default_layer["problem"]["legal_actions"], json!([]));

    let still = setting(&read_prewarm(&server).await);
    assert_eq!(still["effective_value"], boolean_value(true));
    assert_eq!(still["revision_id"], json!(override_revision));
    assert_eq!(
        still["candidates"],
        json!([{
            "layer": {"kind": "project", "project_id": project_id},
            "revision_id": override_revision,
            "disposition": "winning",
            "safe_reason": null
        }])
    );
}
