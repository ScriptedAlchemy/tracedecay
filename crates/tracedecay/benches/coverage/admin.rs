//! Admin / status family: store status, runtime, config read surface, health,
//! application summaries, project registry, observatory, automation, native
//! integration reads, and the multi_root + worktree read lanes.

use serde_json::{Value, json};

use crate::queries::{PrimeStep, QueryContext, ToolGroup, five};

use super::{eq, eqn, rq, rqn};

fn path_at(ctx: &QueryContext, i: usize) -> String {
    crate::queries::dir(ctx, i)
}

pub(crate) fn groups(ctx: &QueryContext, out: &mut Vec<ToolGroup>) {
    for (tool, label, extra) in [
        ("tracedecay_status", "status", json!({})),
        (
            "tracedecay_storage_status",
            "storage_status",
            json!({"include_details": true}),
        ),
        ("tracedecay_health", "health", json!({})),
        ("tracedecay_health_read", "health_read", json!({})),
        ("tracedecay_runtime", "runtime", json!({})),
        ("tracedecay_configuration_list", "config_list", json!({})),
        (
            "tracedecay_configuration_observed_state",
            "config_observed",
            json!({}),
        ),
        (
            "tracedecay_configuration_audit",
            "config_audit",
            json!({"limit": 25}),
        ),
        ("tracedecay_active_project", "active_project", json!({})),
        ("tracedecay_remote_status", "remote_status", json!({})),
        ("tracedecay_analytics", "analytics", json!({})),
        ("tracedecay_dashboard", "dashboard", json!({})),
        (
            "tracedecay_observatory_read",
            "observatory_read",
            json!({"window_days": 7}),
        ),
        (
            "tracedecay_skill_list",
            "skill_list",
            json!({"include_body": false}),
        ),
        ("tracedecay_hermes_skill_bridge", "hermes_bridge", json!({})),
    ] {
        out.push(ToolGroup {
            tool,
            queries: five(|i| {
                let mut args = extra.clone();
                if tool == "tracedecay_status" && i % 2 == 0 {
                    args["include_staleness"] = json!(true);
                    args["include_storage_health"] = json!(true);
                }
                rq(tool, label, args)
            }),
        });
    }
    if let Some(handle) = ctx.seeds.retrieve_handle.clone() {
        out.push(ToolGroup {
            tool: "tracedecay_retrieve",
            queries: five(|i| {
                rq(
                    "tracedecay_retrieve",
                    "retrieve",
                    json!({
                        "handle": handle,
                        "offset": i * 1024,
                        "max_chars": 4096,
                    }),
                )
            }),
        });
    }
    if let Some(skill_id) = ctx.seeds.skill_id.clone() {
        out.push(ToolGroup {
            tool: "tracedecay_skill_view",
            queries: five(|_i| {
                rq(
                    "tracedecay_skill_view",
                    "skill_view",
                    json!({
                        "id": skill_id,
                        "include_support_files": false,
                    }),
                )
            }),
        });
    }
    out.push(ToolGroup {
        tool: "tracedecay_configuration_get",
        queries: five(|_i| {
            rq(
                "tracedecay_configuration_get",
                "config_get",
                json!({"key": ctx.seeds.config_key.clone().unwrap_or_else(|| "diagnostics.prewarm.v1".into())}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_automation_run_list",
        queries: five(|i| {
            rq(
                "tracedecay_automation_run_list",
                "automation_runs",
                json!({"limit": 10 + i as u32}),
            )
        }),
    });
    // Native-integration journey: the seed drives inventory → stack_snapshot
    // → preflight → approve and the groups measure the remaining legs. All
    // five tools need the seeded transaction identity; without it the family
    // is a seed-ledger skip.
    if let Some(native) = ctx.seeds.native.clone() {
        out.push(ToolGroup {
            tool: "tracedecay_native_integration_status",
            queries: five(|_i| {
                rq(
                    "tracedecay_native_integration_status",
                    "native_status",
                    json!({"transaction_id": native.transaction_id}),
                )
            }),
        });
        out.push(ToolGroup {
            tool: "tracedecay_preflight_native_integration",
            queries: five(|_i| {
                eq(
                    "tracedecay_preflight_native_integration",
                    "native_preflight",
                    json!({
                        "snapshot": "{{snapshot}}",
                        "format": "json",
                    }),
                    |ctx, _iter| vec![native_snapshot_step(ctx)],
                )
            }),
        });
        out.push(ToolGroup {
            tool: "tracedecay_approve_native_integration",
            queries: five(|_i| {
                eq(
                    "tracedecay_approve_native_integration",
                    "native_approve",
                    json!({
                        "preview_id": "{{ni_preview_id}}",
                        "preview_digest": "{{ni_preview_digest}}",
                    }),
                    |ctx, _iter| vec![native_snapshot_step(ctx), native_preflight_step()],
                )
            }),
        });
        out.push(ToolGroup {
            tool: "tracedecay_apply_native_integration",
            queries: five(|_i| {
                eq(
                    "tracedecay_apply_native_integration",
                    "native_apply",
                    json!({
                        "preview_id": "{{ni_preview_id}}",
                        "preview_digest": "{{ni_preview_digest}}",
                        "approval_id": "{{ni_approval_id}}",
                        "approval_digest": "{{ni_approval_digest}}",
                        "transaction_id": "{{ni_transaction_id}}",
                    }),
                    |ctx, _iter| {
                        vec![
                            native_snapshot_step(ctx),
                            native_preflight_step(),
                            native_approve_step(),
                        ]
                    },
                )
            }),
        });
        out.push(ToolGroup {
            tool: "tracedecay_cancel_native_integration",
            queries: five(|_i| {
                eq(
                    "tracedecay_cancel_native_integration",
                    "native_cancel",
                    json!({"transaction_id": "{{ni_transaction_id}}"}),
                    |ctx, _iter| vec![native_snapshot_step(ctx), native_preflight_step()],
                )
            }),
        });
    }
    // worktree inventory + cleanup chain all claim the seeded scope_set
    // identity; cleanup tools target the registered worktree (repository
    // targets are only valid for inventory), and inspect seeds digests for
    // confirm/reconcile/remove.
    let wt_target = json!({
        "kind": "worktree",
        "project_id": ctx
            .seeds
            .project_id
            .clone()
            .unwrap_or_else(|| "td-bench-missing".into()),
        "repository_id": ctx
            .seeds
            .repository_id
            .clone()
            .unwrap_or_else(|| "td-bench-missing".into()),
        "worktree_id": ctx
            .seeds
            .worktree_id
            .clone()
            .unwrap_or_else(|| "worktree.bench.missing".into()),
    });
    let wt_claim = move |extra: Value| {
        let mut a = json!({
            "scope_set_id": ctx
                .seeds
                .scope_set_id
                .clone()
                .unwrap_or_else(|| "td-bench-missing".into()),
            "scope_set_revision": ctx.seeds.scope_set_revision.unwrap_or(1),
            "scope_set_digest": ctx
                .seeds
                .scope_set_digest
                .clone()
                .unwrap_or_else(|| "td-bench-missing".into()),
            "target": wt_target,
        });
        if let Value::Object(m) = extra {
            a.as_object_mut().map(|o| o.extend(m));
        }
        a
    };
    out.push(ToolGroup {
        tool: "tracedecay_worktree_inventory",
        queries: five(|_i| {
            rqn(
                "tracedecay_worktree_inventory",
                "wt_inventory",
                wt_claim(json!({
                    "target": {
                        "kind": "repository",
                        "project_id": ctx
                            .seeds
                            .project_id
                            .clone()
                            .unwrap_or_else(|| "td-bench-missing".into()),
                        "repository_id": ctx
                            .seeds
                            .repository_id
                            .clone()
                            .unwrap_or_else(|| "td-bench-missing".into()),
                    }
                })),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_worktree_cleanup_inspect",
        queries: five(|_i| {
            eqn(
                "tracedecay_worktree_cleanup_inspect",
                "wt_cleanup_inspect",
                wt_claim(json!({})),
                |_ctx, _iter| Vec::new(),
            )
        }),
    });
    for (tool, label) in [
        ("tracedecay_worktree_cleanup_confirm", "wt_cleanup_confirm"),
        (
            "tracedecay_worktree_cleanup_reconcile",
            "wt_cleanup_reconcile",
        ),
        ("tracedecay_worktree_cleanup_remove", "wt_cleanup_remove"),
    ] {
        out.push(ToolGroup {
            tool,
            queries: five(|_i| {
                eqn(
                    tool,
                    label,
                    wt_claim(json!({
                        "inspection_digest": "{{wt_inspection_digest}}",
                        "confirmation_digest": "{{wt_confirmation_digest}}",
                        "confirmed_at": "{{wt_confirmed_at}}",
                    })),
                    wt_cleanup_primes,
                )
            }),
        });
    }
    // multi_root: scope_set_read hits the CAS-minted set; execute re-runs one
    // federated read over it (git_status — the documented closed family).
    let scope_set_id = ctx
        .seeds
        .scope_set_id
        .clone()
        .unwrap_or_else(|| "td-bench-missing".into());
    out.push(ToolGroup {
        tool: "tracedecay_multi_root_scope_set_read",
        queries: five(|_i| {
            rqn(
                "tracedecay_multi_root_scope_set_read",
                "mr_scope_read",
                json!({"scope_set_id": scope_set_id}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_multi_root_execute",
        queries: five(|_i| {
            rqn(
                "tracedecay_multi_root_execute",
                "mr_execute",
                json!({
                    "scope_set_id": scope_set_id,
                    "scope_set_revision": ctx.seeds.scope_set_revision.unwrap_or(1),
                    "scope_set_digest": ctx
                        .seeds
                        .scope_set_digest
                        .clone()
                        .unwrap_or_else(|| "td-bench-missing".into()),
                    "operation": {
                        "kind": "query",
                        "request": {
                            "operation": "code_symbol_search",
                            "request": {
                                "query": ctx
                                    .function_qnames
                                    .first()
                                    .cloned()
                                    .unwrap_or_else(|| "fit".into()),
                                "lazy_index_ignored_dependencies": true,
                                "scope": {"path_prefix": null},
                                "meta": {
                                    "projection": "summary",
                                    "order": "relevance",
                                    "cursor": null,
                                },
                            },
                        },
                    },
                    "page": 0,
                }),
            )
        }),
    });
    // CAS re-commit of a fresh scope-set identity: expected_revision=null
    // mints, so the timed call is the real CAS create lane.
    out.push(ToolGroup {
        tool: "tracedecay_multi_root_scope_set_compare_and_swap",
        queries: five(|i| {
            eqn(
                "tracedecay_multi_root_scope_set_compare_and_swap",
                "mr_cas",
                json!({
                    "scope_set_id": format!("scope-set.bench.q{i}.{{{{iter}}}}"),
                    "expected_revision": null,
                    "roots": mr_roots(ctx),
                }),
                |_ctx, _iter| Vec::new(),
            )
        }),
    });

    // Wave-A reads still missing from the sweep list.
    out.push(ToolGroup {
        tool: "tracedecay_config",
        queries: five(|_i| {
            rq(
                "tracedecay_config",
                "config",
                json!({"key": "name", "path": "pyproject.toml"}),
            )
        }),
    });
    for (tool, label, args) in [
        ("tracedecay_project_context", "project_ctx", json!({})),
        (
            "tracedecay_project_list",
            "project_list",
            json!({"limit": 10}),
        ),
        (
            "tracedecay_project_search",
            "project_search",
            json!({"query": "scipy", "limit": 10}),
        ),
        (
            "tracedecay_memory_status",
            "memory_status",
            json!({"memory_scope": "project"}),
        ),
    ] {
        out.push(ToolGroup {
            tool,
            queries: five(|_i| rq(tool, label, args.clone())),
        });
    }
    out.push(ToolGroup {
        tool: "tracedecay_health_delta",
        queries: five(|i| {
            rq(
                "tracedecay_health_delta",
                "health_delta",
                json!({"meta": {
                    "order": "temporal_descending",
                    "page": {"page_size": 5 + i as u32, "cursor": null},
                    "projection": "summary",
                    "temporal": {"kind": "current"},
                }}),
            )
        }),
    });
    for (tool, label) in [
        ("tracedecay_automation_run_view", "automation_view"),
        (
            "tracedecay_automation_run_artifact_view",
            "automation_artifact",
        ),
    ] {
        out.push(ToolGroup {
            tool,
            queries: five(|_i| {
                let mut args = json!({
                    "run_id": ctx
                        .seeds
                        .automation_run_id
                        .clone()
                        .unwrap_or_else(|| "td-bench-missing".into()),
                });
                if tool == "tracedecay_automation_run_artifact_view" {
                    args["kind"] = json!("traces");
                    // The artifact-view request struct declares no `format`
                    // field and denies unknown keys — rqn keeps the wire clean.
                    rqn(tool, label, args)
                } else {
                    rq(tool, label, args)
                }
            }),
        });
    }
    // Scout lifecycle tools need an address minted by a hook-dispatched
    // scout run — unreachable in the bench composition. A schema-valid zeroed
    // ContextScoutAddressV1 reaches the miss path so each prime-less effect
    // either measures it or degrades to a named skip.
    let scout_addr = || {
        json!({
            "profile_id": vec![0u8; 16],
            "provider_id": vec![0u8; 16],
            "protected_session_id": vec![0u8; 32],
            "thread_id": vec![0u8; 16],
            "turn_id": vec![0u8; 16],
            "agent_id": vec![0u8; 16],
            "logical_message_id": vec![0u8; 16],
            "project_id": vec![0u8; 16],
        })
    };
    for (tool, label, args) in [
        (
            "tracedecay_context_scout_status",
            "scout_status",
            json!({"address": scout_addr()}),
        ),
        (
            "tracedecay_context_scout_recent",
            "scout_recent",
            json!({"address": scout_addr(), "limit": 5}),
        ),
        (
            "tracedecay_context_scout_explain",
            "scout_explain",
            json!({"address": scout_addr(), "limit": 5}),
        ),
        (
            "tracedecay_context_scout_capability",
            "scout_capability",
            json!({"address": scout_addr()}),
        ),
        (
            "tracedecay_context_scout_budget",
            "scout_budget",
            json!({"address": scout_addr()}),
        ),
        (
            "tracedecay_context_scout_claim",
            "scout_claim",
            json!({"address": scout_addr(), "window": "on_request", "idempotency_key": "bench-scout-claim-{{iter}}"}),
        ),
        (
            "tracedecay_context_scout_pause",
            "scout_pause",
            json!({"address": scout_addr(), "expected_revision": "revision.bench.0", "idempotency_key": "bench-scout-pause-{{iter}}"}),
        ),
        (
            "tracedecay_context_scout_resume",
            "scout_resume",
            json!({"address": scout_addr(), "expected_revision": "revision.bench.0", "idempotency_key": "bench-scout-resume-{{iter}}"}),
        ),
        (
            "tracedecay_context_scout_cancel",
            "scout_cancel",
            json!({"address": scout_addr(), "work": {"address": scout_addr(), "generation": 0, "input_watermark": vec![0u8; 32]}, "idempotency_key": "bench-scout-cancel-{{iter}}"}),
        ),
        (
            "tracedecay_context_scout_delivery",
            "scout_delivery",
            json!({"address": scout_addr(), "claim": {"envelope_id": vec![0u8; 16], "lease_id": vec![0u8; 16], "lease_expires_at": "{{now}}"}, "delivered_at": "{{now}}", "outcome": "displayed", "idempotency_key": "bench-scout-delivery-{{iter}}"}),
        ),
        (
            "tracedecay_context_scout_feedback",
            "scout_feedback",
            json!({"address": scout_addr(), "receipt": {"receipt_id": vec![0u8; 16], "envelope_id": vec![0u8; 16], "delivered_at": "{{now}}", "outcome": "displayed"}, "feedback": {"receipt_id": vec![0u8; 16], "kind": "explicitly_accepted"}, "idempotency_key": "bench-scout-feedback-{{iter}}"}),
        ),
        (
            "tracedecay_github_stack_signal_expand",
            "stack_signal_expand",
            json!({"signal_id": "td-bench-missing", "format": "json"}),
        ),
        (
            "tracedecay_source_edit_reconcile",
            "source_edit_reconcile",
            json!({
                "kind": "rename_symbol",
                "effect_id": "td-bench-missing",
                "idempotency_key": "bench-reconcile-{{iter}}",
                "attempt_idempotency_key": "bench-reconcile-attempt-{{iter}}",
                "input_digest": format!("sha256:{}", "0".repeat(64)),
                "disposition": "confirm_rolled_back",
                "confirm": true,
            }),
        ),
    ] {
        out.push(ToolGroup {
            tool,
            queries: five(|_i| eqn(tool, label, args.clone(), |_ctx, _iter| Vec::new())),
        });
    }
    let _ = path_at;
}

/// Prime chain for the worktree cleanup lifecycle: inspect mints the
/// inspection digest, confirm mints the confirmation digest + timestamp the
/// downstream reconcile/remove calls consume.
fn wt_cleanup_primes(ctx: &QueryContext, _iter: u64) -> Vec<PrimeStep> {
    let claim = json!({
        "scope_set_id": ctx
            .seeds
            .scope_set_id
            .clone()
            .unwrap_or_else(|| "td-bench-missing".into()),
        "scope_set_revision": ctx.seeds.scope_set_revision.unwrap_or(1),
        "scope_set_digest": ctx
            .seeds
            .scope_set_digest
            .clone()
            .unwrap_or_else(|| "td-bench-missing".into()),
        "target": {
            "kind": "repository",
            "project_id": ctx
                .seeds
                .project_id
                .clone()
                .unwrap_or_else(|| "td-bench-missing".into()),
            "repository_id": ctx
                .seeds
                .repository_id
                .clone()
                .unwrap_or_else(|| "td-bench-missing".into()),
        },
    });
    let mut confirm_args = claim.clone();
    confirm_args["inspection_digest"] = json!("{{wt_inspection_digest}}");
    vec![
        PrimeStep {
            inject: Vec::new(),
            tool: "tracedecay_worktree_cleanup_inspect",
            args: claim,
            capture: &[("dig:inspection_digest", "wt_inspection_digest")],
        },
        PrimeStep {
            inject: Vec::new(),
            tool: "tracedecay_worktree_cleanup_confirm",
            args: confirm_args,
            capture: &[
                ("dig:confirmation_digest", "wt_confirmation_digest"),
                ("digany:confirmed_at,confirmed_at_micros", "wt_confirmed_at"),
            ],
        },
    ]
}

/// One registered {project_id, root} pair, matching the seeded scope set.
fn mr_roots(ctx: &QueryContext) -> serde_json::Value {
    let project_id = ctx
        .seeds
        .project_id
        .clone()
        .unwrap_or_else(|| "td-bench-missing".into());
    let root = ctx.project_root.to_string_lossy().to_string();
    json!([{"project_id": project_id, "root": root}])
}

// ── native-integration prime steps ─────────────────────────────────────────
// Each timed leg re-mints the upstream chain: stack_snapshot → (preflight →
// approve) so every measured call binds a fresh, self-consistent transaction.

fn native_snapshot_step(ctx: &QueryContext) -> PrimeStep {
    let mut args = ctx
        .seeds
        .native
        .as_ref()
        .map(|n| n.snapshot_body.clone())
        .unwrap_or_else(|| json!({}));
    args["format"] = json!("json");
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_stack_snapshot",
        args,
        capture: &[("digany:sealed_snapshot,snapshot", "snapshot")],
    }
}

fn native_preflight_step() -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_preflight_native_integration",
        args: json!({"snapshot": "{{snapshot}}", "format": "json"}),
        capture: &[
            ("dig:transaction_id", "ni_transaction_id"),
            ("dig:preview_id", "ni_preview_id"),
            ("dig:preview_digest", "ni_preview_digest"),
        ],
    }
}

fn native_approve_step() -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_approve_native_integration",
        args: json!({
            "preview_id": "{{ni_preview_id}}",
            "preview_digest": "{{ni_preview_digest}}",
            "format": "json",
        }),
        capture: &[
            ("dig:approval_id", "ni_approval_id"),
            ("dig:approval_digest", "ni_approval_digest"),
        ],
    }
}
