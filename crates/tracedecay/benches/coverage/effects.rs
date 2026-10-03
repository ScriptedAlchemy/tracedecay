//! Configuration effect family: set/unset/batch writes and the protected /
//! rollback plan chains. Every write re-reads its expected_revision inside the
//! prime so the timed call is the real mutation hop.

use serde_json::json;

use crate::queries::{PrimeStep, QueryContext, ToolGroup, five};

use super::eq;

const SCALAR_KEY: &str = "diagnostics.prewarm.v1";
const TOPOLOGY_KEY: &str = "work.topology_policy.v1";

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
        capture: &[
            ("dig:revision_id", "revision"),
            ("dig:effective_value", "effective"),
        ],
    }
}

/// Strip the last entry of `review_topology.allowed` — same safe change the
/// sweep applies (matches `_changed_topology_policy`).
fn change_topology_primes() -> Vec<PrimeStep> {
    vec![PrimeStep {
    inject: Vec::new(),
        tool: "tracedecay_configuration_get",
        args: json!({"key": TOPOLOGY_KEY, "format": "json"}),
        capture: &[("dig:revision_id", "revision")],
    }]
}

fn set_args(ctx: &QueryContext, iter_note: &str) -> serde_json::Value {
    json!({
        "layer": project_layer(ctx),
        "key": SCALAR_KEY,
        "value": {"kind": "boolean", "value": true},
        "expected_revision": "{{revision}}",
        "idempotency_key": format!("bench-cfg-{iter_note}-{{{{iter}}}}"),
    })
}

/// Shrink `effective.review_topology.allowed` by one — implemented as a
/// substitution on the captured `{{effective}}` is not possible (tokens are
/// values, not transforms), so the change is built inline: `allowed` loses
/// its tail via a second prime that re-reads and mutates client-side.
/// Instead we replace the whole policy with a minimal legal one derived from
/// the captured value — carried by a dedicated capture spec that returns the
/// trimmed policy directly.
fn changed_policy_step() -> PrimeStep {
    PrimeStep {
    inject: Vec::new(),
        tool: "tracedecay_configuration_get",
        args: json!({"key": TOPOLOGY_KEY, "format": "json"}),
        capture: &[
            ("dig:revision_id", "revision"),
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
                        "value": {"kind": "boolean", "value": i % 2 == 0},
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
                |_ctx, _iter| {
                    let mut steps = change_topology_primes();
                    steps.push(changed_policy_step());
                    steps
                },
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
                    let mut steps = change_topology_primes();
                    steps.push(changed_policy_step());
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
                        revision_prime(TOPOLOGY_KEY),
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
                            capture: &[],
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
                        revision_prime(TOPOLOGY_KEY),
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
