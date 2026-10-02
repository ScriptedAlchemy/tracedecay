use serde_json::{Value, json};

use super::{HookCompactionResultV1, HookRuntimeResultV1, HookRuntimeSurfaceRequestV1};

fn result_round_trips(wire: Value) -> HookRuntimeResultV1 {
    let decoded: HookRuntimeResultV1 = serde_json::from_value(wire.clone())
        .unwrap_or_else(|error| panic!("{wire} does not decode: {error}"));
    assert_eq!(serde_json::to_value(&decoded).unwrap(), wire);
    decoded
}

#[test]
fn every_emitted_result_shape_round_trips_exactly() {
    for wire in [
        json!({"action": "reset_counter", "reset": true}),
        json!({
            "action": "hook_v2_admit",
            "status": "accepted",
            "disposition": "accepted",
            "orchestration": "enqueued",
            "context_scout_address": null,
            "ready_guidance": null,
            "feedback_notice": null,
            "github_stack_signal_available": false,
        }),
        json!({
            "action": "hook_v2_admit",
            "status": "exact_duplicate",
            "disposition": "accepted",
            "context_scout_address": null,
            "ready_guidance": null,
        }),
        json!({
            "action": "hook_v2_admit",
            "status": "rejected",
            "disposition": "catchup_required",
            "reason": "admission_identity_conflict",
        }),
        json!({"action": "hook_v2_admit", "status": "rejected", "disposition": "catchup_required"}),
        json!({"action": "hook_v2_admit", "status": "backpressured"}),
        json!({"action": "hook_v2_delivery_receipt", "status": "superseded"}),
        json!({"action": "hook_v2_feedback_notice_delivery", "status": "stored"}),
        json!({
            "action": "hook_v2_feedback_notice_delivery",
            "status": "rejected",
            "disposition": "catchup_required",
        }),
        json!({
            "action": "ingest_transcript",
            "provider": "cursor",
            "user_scope": false,
            "completed": true,
            "status": "committed",
            "admission": {"status": "committed", "retryable": false},
            "messages_upserted": 2,
            "hint_outcomes": {"status": "unavailable", "reason": "profile_root_unavailable"},
            "observations_committed": 2,
        }),
        json!({"action": "hermes_receipt", "status": "awaiting_transcript"}),
        json!({"action": "hook_v2_profile_admit", "status": "exact_duplicate", "disposition": "accepted"}),
        json!({"action": "hook_v2_profile_admit", "status": "unavailable"}),
    ] {
        result_round_trips(wire);
    }
}

#[test]
fn each_compaction_outcome_decodes_to_the_variant_that_carries_its_fields() {
    let settled = json!({
        "action": "codex_compact",
        "status": "compressed",
        "reason": "pressure",
        "summary_nodes_created": 1,
        "summary_node_ids": ["node-1"],
        "relation_projection_status": "projected",
        "retry_status": null,
        "authority_outcome": "ready",
        "committed_state": null,
        "messages_upserted": 3,
    });
    let refused = json!({
        "action": "cursor_compact",
        "status": "unavailable",
        "reason": "lcm_daemon_authority_rejected",
        "authority_outcome": "denied",
        "committed_state": null,
        "summary_nodes_created": 0,
        "summary_node_ids": [],
        "messages_upserted": 0,
    });
    let skipped = json!({
        "action": "cursor_compact",
        "status": "skipped",
        "reason": "no messages to compact",
        "summary_nodes_created": 0,
        "summary_node_ids": [],
        "relation_projection_status": "not_applicable",
    });
    let no_session = json!({
        "action": "codex_compact",
        "status": "unavailable",
        "reason": "host_session_identity_unavailable",
        "messages_upserted": 0,
    });

    assert!(matches!(
        result_round_trips(settled),
        HookRuntimeResultV1::CodexCompact(HookCompactionResultV1::Settled { .. })
    ));
    assert!(matches!(
        result_round_trips(refused),
        HookRuntimeResultV1::CursorCompact(HookCompactionResultV1::Refused { .. })
    ));
    assert!(matches!(
        result_round_trips(skipped),
        HookRuntimeResultV1::CursorCompact(HookCompactionResultV1::NotRun { .. })
    ));
    assert!(matches!(
        result_round_trips(no_session),
        HookRuntimeResultV1::CodexCompact(HookCompactionResultV1::NoSession { .. })
    ));
}

fn request_error(wire: Value) -> String {
    serde_json::from_value::<HookRuntimeSurfaceRequestV1>(wire)
        .expect_err("request must be refused")
        .to_string()
}

#[test]
fn requests_accept_what_hosts_send_and_refuse_anything_else() {
    let admit = json!({
        "action": "hook_v2_admit",
        "envelope": {"schema_version": 2},
        "native_session_id": null,
        "native_lifecycle": null,
    });
    let decoded: HookRuntimeSurfaceRequestV1 = serde_json::from_value(admit.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), admit);
    let reset: HookRuntimeSurfaceRequestV1 =
        serde_json::from_value(json!({"action": "reset_counter"})).unwrap();
    assert_eq!(reset, HookRuntimeSurfaceRequestV1::ResetCounter {});

    assert_eq!(
        request_error(json!({"action": "reset_counter", "project_root": "/elsewhere"})),
        "unknown field `project_root`, there are no fields"
    );
    assert!(
        request_error(json!({
            "action": "ingest_transcript",
            "provider": "cursor",
            "user_scope": false,
            "event_json": "{}",
            "timeout_budget_ms": 250,
        }))
        .starts_with("unknown field `timeout_budget_ms`"),
    );
    assert!(
        request_error(json!({"action": "codex_compact", "event_json": "{}", "provider": "codex"}))
            .starts_with("unknown field `provider`"),
    );
    assert!(
        request_error(json!({"action": "hook_v2_status", "control": {}}))
            .contains("unknown variant `hook_v2_status`")
    );
}

#[test]
fn only_a_counter_reset_skips_the_session_stores() {
    let needs = |arguments: Value| {
        super::hook_runtime_needs_session_stores(arguments.as_object().expect("object arguments"))
    };
    assert!(!needs(json!({"action": "reset_counter"})));
    assert!(needs(
        json!({"action": "ingest_transcript", "user_scope": false})
    ));
    assert!(needs(json!({"action": "hook_v2_admit"})));
    assert!(needs(json!({"action": 42})));
    assert!(needs(json!({})));
}
