//! Configuration effect family: set/unset/batch writes and the protected /
//! rollback plan chains. Every write re-reads its expected_revision inside the
//! prime so the timed call is the real mutation hop.

use std::collections::HashMap;

use serde_json::{Value, json};
use tracedecay_contracts::configuration::{
    ConfigurationMutationReceipt, ConfigurationProtectedPreviewRequestV1,
    ConfigurationRollbackPreviewRequestV1, ResolvedSetting,
};
use tracedecay_domain::configuration::{ConfigurationValueV1, ProtectedChangePlan};
use tracedecay_store::configuration::ConfigurationProtectedOperationV1;

use crate::queries::{PrimeStep, QueryContext, ToolGroup, five};

use super::eq;

const SCALAR_KEY: &str = "diagnostics.prewarm.v1";
const TOPOLOGY_KEY: &str = "work.topology_policy.v1";

pub(crate) fn configuration_effect_key(tool: &str) -> Option<&'static str> {
    match tool {
        "tracedecay_configuration_set"
        | "tracedecay_configuration_unset"
        | "tracedecay_configuration_batch" => Some(SCALAR_KEY),
        "tracedecay_configuration_protected_preview"
        | "tracedecay_configuration_protected_apply"
        | "tracedecay_configuration_rollback_preview"
        | "tracedecay_configuration_rollback_apply" => Some(TOPOLOGY_KEY),
        _ => None,
    }
}

fn surface_payload(response: &Value) -> &Value {
    response
        .pointer("/outcome/value/payload")
        .unwrap_or(response)
}

fn token<'a>(prepared: &'a HashMap<String, Value>, name: &str) -> Result<&'a Value, String> {
    prepared
        .get(name)
        .ok_or_else(|| format!("configuration fixture omitted prepared {name}"))
}

fn configuration_value(value: &Value) -> Result<ConfigurationValueV1, String> {
    serde_json::from_value(value.clone())
        .map_err(|error| format!("invalid configuration fixture value: {error}"))
}

fn prepared_topology(prepared: &HashMap<String, Value>) -> Result<ConfigurationValueV1, String> {
    configuration_value(
        &json!({"kind":"work_topology_policy", "value":token(prepared, "changed_policy")?}),
    )
}

pub(crate) fn verify_configuration_effect(
    ctx: &QueryContext,
    tool: &str,
    args: &Value,
    response: &Value,
    prepared: &HashMap<String, Value>,
    followup_get_payload: &Value,
) -> Option<Result<(), String>> {
    let key = configuration_effect_key(tool)?;
    Some((|| {
        let followup: ResolvedSetting = serde_json::from_value(
            surface_payload(followup_get_payload).clone(),
        )
        .map_err(|error| {
            format!("configuration follow-up omitted its complete typed setting: {error}")
        })?;
        if followup.key.as_str() != key || ctx.seeds.project_id.is_none() {
            return Err("configuration follow-up resolved another setting or lacked fixture project identity".into());
        }
        let payload = surface_payload(response);
        if tool.ends_with("_preview") {
            return verify_configuration_preview(tool, args, response, prepared, &followup);
        }
        let receipt: ConfigurationMutationReceipt = serde_json::from_value(payload.clone())
            .map_err(|error| {
                format!("configuration mutation omitted its typed receipt: {error}")
            })?;
        let expected_base = args
            .get("expected_revision")
            .or_else(|| args.get("expected_base_revision_id"))
            .and_then(Value::as_str)
            .ok_or_else(|| "configuration mutation omitted its expected base".to_owned())?;
        let operation = tool
            .strip_prefix("tracedecay_configuration_")
            .ok_or_else(|| "unknown configuration operation".to_owned())?;
        let effect = &response["outcome"]["value"];
        if receipt.base_revision_id.as_str() != expected_base
            || receipt.result_revision_id == receipt.base_revision_id
            || followup.revision_id != receipt.result_revision_id
            || followup.snapshot_id != receipt.snapshot_id
            || effect["receipt"]["idempotency_key"] != args["idempotency_key"]
            || args["idempotency_key"].as_str().is_none()
            || effect["receipt"]["outcome"] != "completed"
            || effect["receipt"]["operation"]
                != format!("use-case.application.configuration.{operation}")
            || effect["receipt"]["input_digest"].as_str() != Some(receipt.operation_digest.as_str())
            || effect["receipt"]["scope"]["project_id"].as_str() != ctx.seeds.project_id.as_deref()
            || effect["receipt"]["committed_state"].as_str().is_none()
        {
            return Err("configuration mutation receipt did not bind its actual base, replay key, result revision and public follow-up".into());
        }
        let (before, expected) = match tool {
            "tracedecay_configuration_set" => {
                if args["key"] != key || args["layer"] != project_layer(ctx) {
                    return Err("configuration set targeted another fixture key or layer".into());
                }
                (
                    configuration_value(token(prepared, "before_scalar")?)?,
                    configuration_value(&args["value"])?,
                )
            }
            "tracedecay_configuration_batch" => {
                let mutations = args["mutations"]
                    .as_array()
                    .ok_or_else(|| "configuration batch omitted mutations".to_owned())?;
                if mutations.len() != 1
                    || mutations[0]["operation"] != "set"
                    || mutations[0]["key"] != key
                    || mutations[0]["layer"] != project_layer(ctx)
                {
                    return Err(
                        "configuration batch did not target its single fixture scalar".into(),
                    );
                }
                (
                    configuration_value(token(prepared, "before_scalar")?)?,
                    configuration_value(&mutations[0]["value"])?,
                )
            }
            "tracedecay_configuration_unset" => {
                if args["key"] != key || args["layer"] != project_layer(ctx) {
                    return Err("configuration unset targeted another fixture key or layer".into());
                }
                (
                    configuration_value(token(prepared, "before_scalar")?)?,
                    ConfigurationValueV1::Boolean(false),
                )
            }
            "tracedecay_configuration_protected_apply" => {
                verify_apply_plan(args, prepared, &receipt)?;
                (
                    configuration_value(token(prepared, "before_topology")?)?,
                    prepared_topology(prepared)?,
                )
            }
            "tracedecay_configuration_rollback_apply" => {
                verify_apply_plan(args, prepared, &receipt)?;
                (
                    prepared_topology(prepared)?,
                    configuration_value(token(prepared, "before_topology")?)?,
                )
            }
            _ => return Err("unknown configuration fixture mutation".into()),
        };
        if before == expected || followup.effective_value != expected {
            return Err("configuration effect did not change the actual prior value into the requested fixture value".into());
        }
        Ok(())
    })())
}

fn verify_apply_plan(
    args: &Value,
    prepared: &HashMap<String, Value>,
    receipt: &ConfigurationMutationReceipt,
) -> Result<(), String> {
    if args["plan_id"] != *token(prepared, "plan_id")?
        || args["expected_base_revision_id"] != *token(prepared, "base_revision_id")?
        || args["operation_digest"] != *token(prepared, "operation_digest")?
        || args["operation_digest"].as_str() != Some(receipt.operation_digest.as_str())
    {
        return Err("configuration apply did not consume the exact public preview plan".into());
    }
    Ok(())
}

fn verify_configuration_preview(
    tool: &str,
    args: &Value,
    response: &Value,
    prepared: &HashMap<String, Value>,
    followup: &ResolvedSetting,
) -> Result<(), String> {
    let plan: ProtectedChangePlan = serde_json::from_value(surface_payload(response).clone())
        .map_err(|error| {
            format!("configuration preview omitted its complete typed plan: {error}")
        })?;
    plan.validate().map_err(|error| error.to_string())?;
    let mut request = args.clone();
    request
        .as_object_mut()
        .ok_or_else(|| "configuration preview arguments are not an object".to_owned())?
        .remove("format");
    let (operation, before, after, expected_base) = if tool
        == "tracedecay_configuration_protected_preview"
    {
        let request: ConfigurationProtectedPreviewRequestV1 =
            serde_json::from_value(request).map_err(|error| error.to_string())?;
        (
            ConfigurationProtectedOperationV1::Change(Box::new(request.change)),
            configuration_value(token(prepared, "before_topology")?)?,
            prepared_topology(prepared)?,
            request.expected_revision,
        )
    } else {
        let request: ConfigurationRollbackPreviewRequestV1 =
            serde_json::from_value(request).map_err(|error| error.to_string())?;
        let base = serde_json::from_value(token(prepared, "changed_revision")?.clone())
            .map_err(|error| format!("invalid rollback fixture base: {error}"))?;
        if args["target_revision_id"] != *token(prepared, "revision")? {
            return Err("configuration rollback preview targeted another fixture revision".into());
        }
        (
            ConfigurationProtectedOperationV1::Rollback {
                target_revision_id: request.target_revision_id,
                mode: request.mode,
            },
            prepared_topology(prepared)?,
            configuration_value(token(prepared, "before_topology")?)?,
            base,
        )
    };
    let digest = operation
        .operation_digest()
        .map_err(|error| error.to_string())?;
    let value = &response["outcome"]["value"];
    if plan.operation_digest != digest
        || plan.base_revision_id != expected_base
        || value["preview_id"].as_str() != Some(plan.plan_id.as_str())
        || value["preview_digest"].as_str() != Some(digest.as_str())
        || plan.redacted_changes.len() != 1
        || plan.redacted_changes[0].setting_key.as_str() != TOPOLOGY_KEY
        || plan.redacted_changes[0].before_digest == plan.redacted_changes[0].after_digest
        || followup.revision_id != plan.base_revision_id
        || followup.effective_value != before
        || before == after
    {
        return Err("configuration preview did not bind the canonical operation digest and leave the actual fixture head/value unchanged".into());
    }
    Ok(())
}

fn project_layer(ctx: &QueryContext) -> serde_json::Value {
    json!({
        "kind": "project",
        "project_id": ctx.seeds.project_id.clone().unwrap_or_else(|| "missing".into()),
    })
}

/// Fresh revision read — the write surface is CAS-guarded so every iteration
/// must observe the current revision first.
fn revision_prime(key: &'static str) -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_configuration_get",
        args: json!({"key": key, "format": "json"}),
        capture: if key == SCALAR_KEY {
            &[
                ("digpath:payload:revision_id", "revision"),
                ("dig:effective_value", "before_scalar"),
                ("transform:toggle_boolean", "changed_scalar"),
            ]
        } else {
            &[
                ("digpath:payload:revision_id", "revision"),
                ("dig:effective_value", "before_topology"),
            ]
        },
    }
}

fn set_args(ctx: &QueryContext, iter_note: &str) -> serde_json::Value {
    json!({
        "layer": project_layer(ctx),
        "key": SCALAR_KEY,
        "value": "{{changed_scalar}}",
        "expected_revision": "{{revision}}",
        "idempotency_key": format!("bench-cfg-{iter_note}-{{{{iter}}}}"),
    })
}

fn changed_policy_step() -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_configuration_get",
        args: json!({"key": TOPOLOGY_KEY, "format": "json"}),
        capture: &[
            ("digpath:payload:revision_id", "revision"),
            ("dig:effective_value", "before_topology"),
            ("transform:trim_review_allowed", "changed_policy"),
        ],
    }
}

pub(crate) fn groups(ctx: &QueryContext, out: &mut Vec<ToolGroup>) {
    out.push(ToolGroup {
        tool: "tracedecay_configuration_set",
        queries: five(|i| {
            eq(
                "tracedecay_configuration_set",
                "set",
                set_args(ctx, &format!("set-{i}")),
                |_ctx, _iter| vec![revision_prime(SCALAR_KEY)],
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_configuration_unset",
        queries: five(|i| {
            eq(
                "tracedecay_configuration_unset",
                "unset",
                json!({
                    "layer": project_layer(ctx),
                    "key": SCALAR_KEY,
                    "expected_revision": "{{revision2}}",
                    "idempotency_key": format!("bench-cfg-unset-{i}-{{{{iter}}}}"),
                }),
                |ctx, _iter| {
                    vec![
                        revision_prime(SCALAR_KEY),
                        PrimeStep {
                            inject: Vec::new(),
                            tool: "tracedecay_configuration_set",
                            args: json!({
                                "layer": project_layer(ctx),
                                "key": SCALAR_KEY,
                                "value": {"kind": "boolean", "value": true},
                                "expected_revision": "{{revision}}",
                                "idempotency_key": "bench-cfg-unset-prime-{{iter}}",
                                "format": "json",
                            }),
                            capture: &[("dig:result_revision_id", "revision2")],
                        },
                        revision_prime(SCALAR_KEY),
                    ]
                },
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_configuration_batch",
        queries: five(|i| {
            eq(
                "tracedecay_configuration_batch",
                "batch",
                json!({
                    "mutations": [{
                        "operation": "set",
                        "layer": project_layer(ctx),
                        "key": SCALAR_KEY,
                        "value": "{{changed_scalar}}",
                    }],
                    "expected_revision": "{{revision}}",
                    "idempotency_key": format!("bench-cfg-batch-{i}-{{{{iter}}}}"),
                }),
                |_ctx, _iter| vec![revision_prime(SCALAR_KEY)],
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_configuration_protected_preview",
        queries: five(|_i| {
            eq(
                "tracedecay_configuration_protected_preview",
                "protected_preview",
                json!({
                    "change": {"kind": "replace_work_topology_policy", "value": "{{changed_policy}}"},
                    "expected_revision": "{{revision}}",
                }),
                |_ctx, _iter| vec![changed_policy_step()],
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_configuration_protected_apply",
        queries: five(|i| {
            eq(
                "tracedecay_configuration_protected_apply",
                "protected_apply",
                json!({
                    "plan_id": "{{plan_id}}",
                    "expected_base_revision_id": "{{base_revision_id}}",
                    "operation_digest": "{{operation_digest}}",
                    "idempotency_key": format!("bench-cfg-papply-{i}-{{{{iter}}}}"),
                }),
                |_ctx, _iter| {
                    let mut steps = vec![changed_policy_step()];
                    steps.push(PrimeStep {
    inject: Vec::new(),
                        tool: "tracedecay_configuration_protected_preview",
                        args: json!({
                            "change": {"kind": "replace_work_topology_policy", "value": "{{changed_policy}}"},
                            "expected_revision": "{{revision}}",
                            "format": "json",
                        }),
                        capture: &[
                            ("dig:plan_id", "plan_id"),
                            ("dig:base_revision_id", "base_revision_id"),
                            ("dig:operation_digest", "operation_digest"),
                        ],
                    });
                    steps
                },
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_configuration_rollback_preview",
        queries: five(|_i| {
            eq(
                "tracedecay_configuration_rollback_preview",
                "rollback_preview",
                json!({
                    "target_revision_id": "{{revision}}",
                    "mode": "all_or_nothing",
                }),
                |_ctx, _iter| {
                    // Rolling back to the current head is a stale no-op —
                    // commit a real change first so the pre-change revision
                    // is a valid rollback target.
                    vec![
                        changed_policy_step(),
                        PrimeStep {
    inject: Vec::new(),
                            tool: "tracedecay_configuration_protected_preview",
                            args: json!({
                                "change": {"kind": "replace_work_topology_policy", "value": "{{changed_policy}}"},
                                "expected_revision": "{{revision}}",
                                "format": "json",
                            }),
                            capture: &[
                                ("dig:plan_id", "plan_id"),
                                ("dig:base_revision_id", "base_revision_id"),
                                ("dig:operation_digest", "operation_digest"),
                            ],
                        },
                        PrimeStep {
    inject: Vec::new(),
                            tool: "tracedecay_configuration_protected_apply",
                            args: json!({
                                "plan_id": "{{plan_id}}",
                                "expected_base_revision_id": "{{base_revision_id}}",
                                "operation_digest": "{{operation_digest}}",
                                "idempotency_key": "bench-cfg-rpreview-prime-{{iter}}",
                                "format": "json",
                            }),
                            capture: &[("dig:result_revision_id", "changed_revision")],
                        },
                    ]
                },
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_configuration_rollback_apply",
        queries: five(|i| {
            eq(
                "tracedecay_configuration_rollback_apply",
                "rollback_apply",
                json!({
                    "plan_id": "{{plan_id}}",
                    "expected_base_revision_id": "{{base_revision_id}}",
                    "operation_digest": "{{operation_digest}}",
                    "idempotency_key": format!("bench-cfg-rapply-{i}-{{{{iter}}}}"),
                }),
                |_ctx, _iter| {
                    // prime chain: set → preview → apply → rollback_preview
                    vec![
                        changed_policy_step(),
                        PrimeStep {
    inject: Vec::new(),
                            tool: "tracedecay_configuration_protected_preview",
                            args: json!({
                                "change": {"kind": "replace_work_topology_policy", "value": "{{changed_policy}}"},
                                "expected_revision": "{{revision}}",
                                "format": "json",
                            }),
                            capture: &[
                                ("dig:plan_id", "plan_id"),
                                ("dig:base_revision_id", "base_revision_id"),
                                ("dig:operation_digest", "operation_digest"),
                            ],
                        },
                        PrimeStep {
    inject: Vec::new(),
                            tool: "tracedecay_configuration_protected_apply",
                            args: json!({
                                "plan_id": "{{plan_id}}",
                                "expected_base_revision_id": "{{base_revision_id}}",
                                "operation_digest": "{{operation_digest}}",
                                "idempotency_key": "bench-cfg-rapply-prime-{{iter}}",
                                "format": "json",
                            }),
                            capture: &[("dig:result_revision_id", "changed_revision")],
                        },
                        PrimeStep {
    inject: Vec::new(),
                            tool: "tracedecay_configuration_rollback_preview",
                            args: json!({
                                "target_revision_id": "{{revision}}",
                                "mode": "all_or_nothing",
                                "format": "json",
                            }),
                            capture: &[
                                ("dig:plan_id", "plan_id"),
                                ("dig:base_revision_id", "base_revision_id"),
                                ("dig:operation_digest", "operation_digest"),
                            ],
                        },
                    ]
                },
            )
        }),
    });
}
