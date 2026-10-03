//! Work/workflow family: ~45 tools measured against the real disposable
//! lifecycle `seed_work` minted (create → proposal → accept → admit →
//! placement → start → cancel), plus per-iteration fresh-task effect chains.
//! Args carry no `format` — the work surface rejects it.

use serde_json::{Value, json};

use crate::queries::{PrimeStep, Query, QueryContext, ToolGroup, five};

use super::{WorkSeeds, eqc, eqn, no_primes, now_micros, rqn};

fn w(ctx: &QueryContext) -> &WorkSeeds {
    ctx.seeds.work.as_ref().unwrap()
}

fn create_change() -> Value {
    super::work_create_change("{{iter}}", json!("{{now}}"))
}

fn step_prepare_create(ctx: &QueryContext) -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_prepare_graph_mutation",
        args: json!({
            "selection": w(ctx).selection,
            "change": create_change(),
            "evidence": [],
            "format": "json",
        }),
        capture: &[("dig:request", "request")],
    }
}

fn step_create() -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_create",
        args: json!("{{request}}"),
        capture: &[],
    }
}

fn step_generate(ctx: &QueryContext) -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_generate_proposal",
        args: json!({
            "selection": w(ctx).selection,
            "task_id": "task.bench.{{iter}}",
            "proposal_id": "proposal.bench.{{iter}}",
            "occurred_at": "{{now}}",
            "format": "json",
        }),
        capture: &[("dig:proposal", "proposal")],
    }
}

fn step_prepare_decide(ctx: &QueryContext, disposition: &str) -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_prepare_graph_mutation",
        args: json!({
            "selection": w(ctx).selection,
            "change": {
                "change": "decide_proposal",
                "proposal": "{{proposal}}",
                "disposition": disposition,
            },
            "evidence": [],
            "format": "json",
        }),
        capture: &[("dig:request", "request")],
    }
}

fn step_prepare_admit(ctx: &QueryContext) -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_prepare_graph_mutation",
        args: json!({
            "selection": w(ctx).selection,
            "change": {
                "change": "admit_execution",
                "task_id": "task.bench.{{iter}}",
                "based_on_version": "{{accepted_gv}}",
            },
            "evidence": [],
            "format": "json",
        }),
        capture: &[("dig:request", "request")],
    }
}

fn step_admit() -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_admit_execution",
        args: json!("{{request}}"),
        capture: &[("dig:execution_snapshot", "execution_snapshot")],
    }
}

fn placement_args_run(run: &str) -> Value {
    json!({
        "task_id": "task.bench.{{iter}}",
        "run_id": run,
        "target": {
            "kind": "no_managed_placement",
            "root": null,
            "network_free": true,
            "in_place_acknowledged": false,
        },
        "occurred_at": "{{now}}",
    })
}

fn placement_args() -> Value {
    placement_args_run("run.bench.{{iter}}")
}

fn step_preflight_run(run: &str) -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_placement_preflight",
        args: placement_args_run(run),
        capture: &[],
    }
}

fn step_preflight() -> PrimeStep {
    step_preflight_run("run.bench.{{iter}}")
}

fn step_admit_placement_run(run: &str) -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_admit_placement",
        args: placement_args_run(run),
        capture: &[("digany:authority_version,graph_version", "auth_v")],
    }
}

fn step_admit_placement() -> PrimeStep {
    step_admit_placement_run("run.bench.{{iter}}")
}

fn start_args_ids(ctx: &QueryContext, run: &str, attempt: &str) -> Value {
    json!({
        "task_id": "task.bench.{{iter}}",
        "run_id": run,
        "attempt_id": attempt,
        "operation": "operation.work.start_attempt",
        "execution_snapshot": "{{execution_snapshot}}",
        "worktree_root": ctx.project_root,
        "reference": null,
        "commit": w(ctx).commit,
        "instructions": "Bench lifecycle attempt.",
        "effect_state": "observational",
        "occurred_at": "{{now}}",
    })
}

fn start_args(ctx: &QueryContext) -> Value {
    start_args_ids(ctx, "run.bench.{{iter}}", "attempt.bench.{{iter}}")
}

fn step_start_ids(ctx: &QueryContext, run: &'static str, attempt: &'static str) -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_start_attempt",
        args: start_args_ids(ctx, run, attempt),
        capture: &[],
    }
}

fn step_pause_run_id(run: &str) -> PrimeStep {
    // `auth_v` is captured by the admit-placement step earlier in the same
    // prime chain; pause_run's result carries no authority_version/graph_version
    // fields, so a capture here would only discard the working value.
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_pause_run",
        args: json!({
            "task_id": "task.bench.{{iter}}",
            "run_id": run,
            "reason": "operator_request",
            "occurred_at": "{{now}}",
        }),
        capture: &[],
    }
}

// ── prime fns (fn pointers; ctx carries the seeds) ────────────────────────

fn p_prepare(ctx: &QueryContext, _i: u64) -> Vec<PrimeStep> {
    vec![step_prepare_create(ctx)]
}

fn p_create(ctx: &QueryContext, _i: u64) -> Vec<PrimeStep> {
    vec![step_prepare_create(ctx), step_create()]
}

fn p_generate(ctx: &QueryContext, _i: u64) -> Vec<PrimeStep> {
    vec![step_prepare_create(ctx), step_create(), step_generate(ctx)]
}

fn p_decide(ctx: &QueryContext, _i: u64, disposition: &str) -> Vec<PrimeStep> {
    let mut v = p_generate(ctx, 0);
    v.push(step_prepare_decide(ctx, disposition));
    v
}

fn p_decide_accepted(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    p_decide(ctx, i, "accepted")
}

fn p_decide_rejected(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    p_decide(ctx, i, "rejected")
}

fn step_accept() -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_accept_proposal",
        args: json!("{{request}}"),
        capture: &[("digany:graph_version", "accepted_gv")],
    }
}

fn p_accept(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    let mut v = p_decide(ctx, i, "accepted");
    v.push(step_accept());
    v
}

fn p_admit_chain(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    let mut v = p_accept(ctx, i);
    v.push(step_prepare_admit(ctx));
    v
}

fn p_admit(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    let mut v = p_admit_chain(ctx, i);
    v.push(step_admit());
    v
}

fn p_placement(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    let mut v = p_admit(ctx, i);
    v.push(step_preflight());
    v
}

fn p_admit_placement(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    let mut v = p_placement(ctx, i);
    v.push(step_admit_placement());
    v
}

fn p_admit_placement_run(ctx: &QueryContext, i: u64, run: &'static str) -> Vec<PrimeStep> {
    let mut v = p_accept(ctx, i);
    v.push(step_prepare_admit(ctx));
    v.push(step_admit());
    v.push(step_preflight_run(run));
    v.push(step_admit_placement_run(run));
    v
}

// Attempt lifecycle groups each run on their own run/attempt identity so a
// cancelled or paused attempt in one group never collides with another's
// prime replay, and every open attempt is released by a cleanup cancel.
fn p_start_cx(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    let mut v = p_admit_placement_run(ctx, i, "run.bench.cx.{{iter}}");
    v.push(step_start_ids(
        ctx,
        "run.bench.cx.{{iter}}",
        "attempt.bench.cx.{{iter}}",
    ));
    v
}

fn p_start_pz(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    let mut v = p_admit_placement_run(ctx, i, "run.bench.pz.{{iter}}");
    v.push(step_start_ids(
        ctx,
        "run.bench.pz.{{iter}}",
        "attempt.bench.pz.{{iter}}",
    ));
    v
}

fn p_pause_rz(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    let mut v = p_admit_placement_run(ctx, i, "run.bench.rz.{{iter}}");
    v.push(step_start_ids(
        ctx,
        "run.bench.rz.{{iter}}",
        "attempt.bench.rz.{{iter}}",
    ));
    v.push(step_pause_run_id("run.bench.rz.{{iter}}"));
    v
}

fn cancel_attempt_step(task: &str, run: &str, attempt: &str, request: &str) -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_cancel_attempt",
        args: json!({
            "task_id": task,
            "run_id": run,
            "attempt_id": attempt,
            "request_id": request,
            "occurred_at": "{{now}}",
        }),
        capture: &[],
    }
}

// The recovery sweep seals any `cancellation_*` residue the cancel left
// behind: a cancel that lands while the provider is still spawning parks the
// row in `cancellation_requested`, and the topology's single parallel-attempt
// slot stays occupied until the sweep completes it to `cancelled`.
fn resume_attempts_step() -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_resume_attempts",
        args: json!({"occurred_at": "{{now}}"}),
        capture: &[],
    }
}

fn cleanup_cancel_start(_ctx: &QueryContext, _i: u64) -> Vec<PrimeStep> {
    vec![
        cancel_attempt_step(
            "task.bench.{{iter}}",
            "run.bench.{{iter}}",
            "attempt.bench.{{iter}}",
            "cancel.cleanup.bench.{{iter}}",
        ),
        resume_attempts_step(),
    ]
}

fn cleanup_cancel_cx(_ctx: &QueryContext, _i: u64) -> Vec<PrimeStep> {
    // The timed call already requested the cancellation; what the next
    // iteration needs is the topology's single parallel slot back. The
    // recovery sweep seals the parked cancellation to a terminal state —
    // a duplicate cancel would only burn the transient window on
    // `not-cancellable` while the row stays non-terminal.
    vec![resume_attempts_step()]
}

fn cleanup_cancel_pz(_ctx: &QueryContext, _i: u64) -> Vec<PrimeStep> {
    vec![
        cancel_attempt_step(
            "task.bench.{{iter}}",
            "run.bench.pz.{{iter}}",
            "attempt.bench.pz.{{iter}}",
            "cancel.cleanup.bench.pz.{{iter}}",
        ),
        resume_attempts_step(),
    ]
}

fn cleanup_cancel_rz(_ctx: &QueryContext, _i: u64) -> Vec<PrimeStep> {
    vec![
        cancel_attempt_step(
            "task.bench.{{iter}}",
            "run.bench.rz.{{iter}}",
            "attempt.bench.rz.{{iter}}",
            "cancel.cleanup.bench.rz.{{iter}}",
        ),
        resume_attempts_step(),
    ]
}

fn cleanup_cancel_retry(ctx: &QueryContext, _i: u64) -> Vec<PrimeStep> {
    // The retried attempt lands under the seeded attempt's task/run scope.
    let w = w(ctx);
    vec![
        PrimeStep {
            inject: Vec::new(),
            tool: "tracedecay_work_cancel_attempt",
            args: json!({
                "task_id": w.task_id,
                "run_id": w.run_id,
                "attempt_id": "attempt.retry.bench.{{iter}}",
                "request_id": "cancel.cleanup.bench.retry.{{iter}}",
                "occurred_at": "{{now}}",
            }),
            capture: &[],
        },
        resume_attempts_step(),
    ]
}

fn cleanup_cancel_synth(_ctx: &QueryContext, _i: u64) -> Vec<PrimeStep> {
    vec![
        cancel_attempt_step(
            "task.bench.synth.{{iter}}",
            "run.bench.synth.{{iter}}",
            "attempt.bench.synth.{{iter}}",
            "cancel.cleanup.bench.synth.{{iter}}",
        ),
        resume_attempts_step(),
    ]
}

// ── workflow primes ─────────────────────────────────────────────────────

fn wf_definition(ctx: &QueryContext, suffix: &str) -> Value {
    let mut def = w(ctx).definition.clone();
    def["definition_id"] = json!(format!("workflow.bench.{suffix}"));
    def["definition_version"] = json!(1);
    def
}

fn step_wf_register(ctx: &QueryContext, suffix: &str) -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_workflow_register_definition",
        args: json!({"definition": wf_definition(ctx, suffix), "format": "json"}),
        capture: &[],
    }
}

fn wf_activate_args(suffix: &str) -> Value {
    json!({
        "definition_id": format!("workflow.bench.{suffix}"),
        "definition_version": 1,
        "expected_revision": 1,
    })
}

fn wf_run_args(suffix: &str) -> Value {
    json!({
        "run_id": format!("workflow-run.bench.{suffix}"),
        "definition_id": format!("workflow.bench.{suffix}"),
        "definition_version": 1,
        "provider": {
            "route": {"provider_id": "provider.work.codex-cli", "route_id": "route.bench.workflow"},
            "backend": "codex_cli",
            "model": "bench",
            "priority": 1,
        },
        "fan_out": null,
        "command_id": format!("command.workflow.bench.start.{suffix}"),
    })
}

fn step_wf_activate(suffix: &str) -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_workflow_activate_definition",
        args: wf_activate_args(suffix),
        capture: &[("digany:revision", "wf_rev")],
    }
}

fn step_wf_start_run(suffix: &str) -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_workflow_start_run",
        args: wf_run_args(suffix),
        capture: &[("digany:sequence", "wf_seq")],
    }
}

fn p_wf_register(ctx: &QueryContext, _i: u64) -> Vec<PrimeStep> {
    vec![step_wf_register(ctx, "{{iter}}")]
}

fn p_wf_active(ctx: &QueryContext, _i: u64) -> Vec<PrimeStep> {
    vec![
        step_wf_register(ctx, "{{iter}}"),
        step_wf_activate("{{iter}}"),
    ]
}

fn p_wf_run(ctx: &QueryContext, _i: u64) -> Vec<PrimeStep> {
    vec![
        step_wf_register(ctx, "{{iter}}"),
        step_wf_activate("{{iter}}"),
        step_wf_start_run("{{iter}}"),
    ]
}

fn p_wf_paused(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    let mut v = p_wf_run(ctx, i);
    v.push(PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_workflow_pause_run",
        args: json!({
            "run_id": "workflow-run.bench.{{iter}}",
            "expected_sequence": "{{wf_seq}}",
            "command_id": "command.workflow.bench.pause.{{iter}}",
        }),
        capture: &[("digany:sequence", "wf_seq")],
    });
    v
}

/// Handoff scope: identical in issue + redeem; {{iter}} keeps it unique.
fn handoff_scope(ctx: &QueryContext) -> Value {
    let w = w(ctx);
    json!({
        "project_id": w.definition.get("project_id").cloned().unwrap_or(Value::Null),
        "repository_id": ctx.seeds.repository_id,
        "worktree_id": w.worktree_id,
        "definition_id": "workflow.bench.handoff.{{iter}}",
        "definition_version": 1,
        "step_id": "step.bench.inspect",
        "task_id": "task.bench.handoff.{{iter}}",
        "thread_id": "thread.bench.{{iter}}",
        "run_id": "workflow-run.bench.handoff.{{iter}}",
        "from_actor_id": w.actor_id,
        "to_actor_id": w.actor_id,
    })
}

fn handoff_frontier(ctx: &QueryContext) -> Value {
    json!({
        "task_id": "task.bench.handoff.{{iter}}",
        "work_version": 1,
        "attempts": [],
        "unknowns": [],
        "blockers": [],
        "legal_actions": ["Inspect the bench frontier."],
        "lineage": {
            "issued_by": w(ctx).actor_id,
            "issued_at": "{{now}}",
            "prior_frontier_digest": null,
        },
    })
}

fn issue_args(ctx: &QueryContext) -> Value {
    json!({
        "scope": handoff_scope(ctx),
        "secret": "bench-handoff-{{iter}}-ssssssssssssssssssssssssssssssss",
        "frontier": handoff_frontier(ctx),
    })
}

/// Primes for a live handoff run: register + activate + start_run on the
/// handoff-scoped definition.
fn p_handoff_run(ctx: &QueryContext, _i: u64) -> Vec<PrimeStep> {
    vec![
        step_wf_register(ctx, "handoff.{{iter}}"),
        step_wf_activate("handoff.{{iter}}"),
        step_wf_start_run("handoff.{{iter}}"),
    ]
}

fn p_handoff(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    let mut v = p_handoff_run(ctx, i);
    v.push(PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_workflow_handoff_issue",
        args: issue_args(ctx),
        capture: &[],
    });
    v
}

fn p_handoff_run_only(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    p_handoff_run(ctx, i)
}

/// Per-tool group with `queries` variants of one arg shape.
fn tg(tool: &'static str, queries: Vec<Query>) -> ToolGroup {
    ToolGroup { tool, queries }
}

/// One static arg shape ×5 — the harness invariant.
fn fiveq(q: &dyn Fn(usize) -> Query) -> Vec<Query> {
    five(|i| q(i))
}

pub fn groups(ctx: &QueryContext, out: &mut Vec<ToolGroup>) {
    let Some(w) = ctx.seeds.work.as_ref() else {
        return;
    };
    let sel = || w.selection.clone();
    let ident = || w.attempt_identity.clone();
    let dup = || w.dup_attempt_identity.clone().unwrap_or(Value::Null);
    let quantities = || {
        json!({
            "wall_micros": null, "token_count": null, "cost_micros": null,
            "test_count": null, "effect_count": null,
            "evidence": "owner_receipt",
            "effect_outcome": "not_applicable",
            "coverage": "known",
        })
    };

    // ── reads ────────────────────────────────────────────────────────────
    let now = now_micros();
    out.push(tg(
        "tracedecay_work_views",
        vec![
            rqn(
                "tracedecay_work_views",
                "current",
                json!({
                    "selection": sel(), "mode": {"mode": "current"},
                    "continuation": null, "observed_at": now,
                }),
            ),
            rqn(
                "tracedecay_work_views",
                "as_of",
                json!({
                    "selection": sel(),
                    "mode": {"mode": "as_of", "valid_at": now},
                    "continuation": null, "observed_at": now,
                }),
            ),
            rqn(
                "tracedecay_work_views",
                "current_again",
                json!({
                    "selection": sel(), "mode": {"mode": "current"},
                    "continuation": null, "observed_at": now,
                }),
            ),
            rqn(
                "tracedecay_work_views",
                "windowed",
                json!({
                    "selection": sel(),
                    "mode": {"mode": "evolution", "from_valid_at": 0, "through_valid_at": now},
                    "continuation": null, "observed_at": now,
                }),
            ),
            rqn(
                "tracedecay_work_views",
                "current_paged",
                json!({
                    "selection": sel(), "mode": {"mode": "current"},
                    "continuation": null, "observed_at": now,
                }),
            ),
        ],
    ));
    for (tool, args) in [
        ("tracedecay_work_topology", json!({"page_size": 8})),
        (
            "tracedecay_work_topology_metrics",
            json!({
                "horizon": {"since_micros": 0, "until_micros": now},
                "max_events": 8,
            }),
        ),
        ("tracedecay_work_list_attempts", json!({"page_size": 8})),
        ("tracedecay_work_execution_history", json!({"page_size": 8})),
        ("tracedecay_work_hydrate_artifacts", json!({"page_size": 8})),
    ] {
        out.push(tg(tool, fiveq(&|_| rqn(tool, "page", args.clone()))));
    }
    for (tool, args) in [
        (
            "tracedecay_work_attempt_status",
            json!({
                "task_id": w.task_id, "run_id": w.run_id, "attempt_id": w.attempt_id,
            }),
        ),
        (
            "tracedecay_work_run_control",
            json!({
                "task_id": w.task_id, "run_id": w.run_id,
            }),
        ),
        (
            "tracedecay_work_placement_status",
            json!({
                "task_id": w.task_id, "run_id": w.run_id,
            }),
        ),
    ] {
        out.push(tg(tool, fiveq(&|_| rqn(tool, "identity", args.clone()))));
    }
    out.push(tg(
        "tracedecay_work_retrieve_evidence",
        fiveq(&|_| {
            rqn(
                "tracedecay_work_retrieve_evidence",
                "evidence",
                json!({
                    "selection": sel(), "task_id": w.task_id,
                    "verified_version": w.current_version,
                    "temporal": {"kind": "current"},
                    "page_size": 8, "observed_at": now_micros(),
                }),
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_experience",
        fiveq(&|_| {
            rqn(
                "tracedecay_work_experience",
                "experience",
                json!({
                    "selection": sel(), "task_id": w.task_id,
                    "verified_version": w.current_version,
                    "evidence_not_before": 0,
                    "expertise_categories": ["testing"],
                    "limit": 8, "observed_at": now_micros(),
                }),
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_compare_proposal",
        fiveq(&|_| {
            rqn(
                "tracedecay_work_compare_proposal",
                "compare",
                json!({
                    "selection": sel(), "task_id": w.task_id,
                    "old_version": w.initial_version,
                    "new_version": w.current_version,
                    "observed_at": now_micros(),
                }),
            )
        }),
    ));

    // ── write/effect chain ────────────────────────────────────────────────
    out.push(tg(
        "tracedecay_work_prepare_graph_mutation",
        fiveq(&|_| {
            eqn(
                "tracedecay_work_prepare_graph_mutation",
                "prepare_create",
                json!({
                    "selection": sel(), "change": create_change(), "evidence": [],
                }),
                no_primes,
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_mutate_graph",
        fiveq(&|_| {
            eqn(
                "tracedecay_work_mutate_graph",
                "commit",
                json!({"mutation": "create_task", "request": "{{request}}"}),
                p_prepare,
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_create",
        fiveq(&|_| {
            eqn(
                "tracedecay_work_create",
                "commit",
                json!("{{request}}"),
                p_prepare,
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_generate_proposal",
        fiveq(&|_| {
            eqn(
                "tracedecay_work_generate_proposal",
                "generate",
                json!({
                    "selection": sel(),
                    "task_id": "task.bench.{{iter}}",
                    "proposal_id": "proposal.bench.{{iter}}",
                    "occurred_at": "{{now}}",
                }),
                p_create,
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_review_proposal",
        fiveq(&|_| {
            eqn(
                "tracedecay_work_review_proposal",
                "review",
                json!("{{request}}"),
                p_decide_rejected,
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_accept_proposal",
        fiveq(&|_| {
            eqn(
                "tracedecay_work_accept_proposal",
                "accept",
                json!("{{request}}"),
                p_decide_accepted,
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_admit_execution",
        fiveq(&|_| {
            eqn(
                "tracedecay_work_admit_execution",
                "admit",
                json!("{{request}}"),
                p_admit_chain,
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_placement_preflight",
        fiveq(&|_| {
            eqn(
                "tracedecay_work_placement_preflight",
                "preflight",
                placement_args(),
                p_admit,
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_admit_placement",
        fiveq(&|_| {
            eqn(
                "tracedecay_work_admit_placement",
                "admit",
                placement_args(),
                p_admit,
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_start_attempt",
        fiveq(&|_| {
            eqc(
                "tracedecay_work_start_attempt",
                "start",
                start_args(ctx),
                p_admit_placement,
                crate::queries::EffectCleanup {
                    capture: &[],
                    steps: cleanup_cancel_start,
                },
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_cancel_attempt",
        fiveq(&|_| {
            eqc(
                "tracedecay_work_cancel_attempt",
                "cancel",
                json!({
                    "task_id": "task.bench.{{iter}}",
                    "run_id": "run.bench.cx.{{iter}}",
                    "attempt_id": "attempt.bench.cx.{{iter}}",
                    "request_id": "cancel.bench.{{iter}}",
                    "occurred_at": "{{now}}",
                }),
                p_start_cx,
                crate::queries::EffectCleanup {
                    capture: &[],
                    steps: cleanup_cancel_cx,
                },
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_pause_run",
        fiveq(&|_| {
            eqc(
                "tracedecay_work_pause_run",
                "pause",
                json!({
                    "task_id": "task.bench.{{iter}}",
                    "run_id": "run.bench.pz.{{iter}}",
                    "reason": "operator_request",
                    "occurred_at": "{{now}}",
                }),
                p_start_pz,
                crate::queries::EffectCleanup {
                    capture: &[],
                    steps: cleanup_cancel_pz,
                },
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_resume_run",
        fiveq(&|_| {
            eqc(
                "tracedecay_work_resume_run",
                "resume",
                json!({
                    "task_id": "task.bench.{{iter}}",
                    "run_id": "run.bench.rz.{{iter}}",
                    "reason": "operator_request",
                    "expected_authority_version": "{{auth_v}}",
                    "occurred_at": "{{now}}",
                }),
                p_pause_rz,
                crate::queries::EffectCleanup {
                    capture: &[],
                    steps: cleanup_cancel_rz,
                },
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_release_placement",
        fiveq(&|_| {
            eqn(
                "tracedecay_work_release_placement",
                "release",
                json!({
                    "task_id": "task.bench.{{iter}}",
                    "run_id": "run.bench.{{iter}}",
                    "expected_authority_version": "{{auth_v}}",
                    "occurred_at": "{{now}}",
                }),
                p_admit_placement,
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_resume_attempts",
        fiveq(&|_| {
            eqn(
                "tracedecay_work_resume_attempts",
                "resume_all",
                json!({
                    "occurred_at": "{{now}}",
                }),
                no_primes,
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_prepare_duplicate_adjudication",
        fiveq(&|_| {
            eqn(
                "tracedecay_work_prepare_duplicate_adjudication",
                "prepare",
                json!({
                    "first_attempt": ident(),
                    "second_attempt": dup(),
                    "verdict": "not_duplicate",
                    "quantities": quantities(),
                    "reason": "bench duplicate probe",
                }),
                no_primes,
            )
        }),
    ));
    if w.work_generation.is_some()
        && w.topology_generation.is_some()
        && w.dup_attempt_identity.is_some()
    {
        out.push(tg(
            "tracedecay_work_adjudicate_duplicate",
            fiveq(&|_| {
                eqn(
                    "tracedecay_work_adjudicate_duplicate",
                    "commit",
                    json!({
                        "first_attempt": ident(),
                        "second_attempt": dup(),
                        "evidence": {
                            "work_generation": w.work_generation,
                            "topology_generation": w.topology_generation,
                        },
                        "verdict": "not_duplicate",
                        "quantities": quantities(),
                        "reason": "bench duplicate probe",
                        "command_id": "command.bench.dup.{{iter}}",
                        "occurred_at": "{{now}}",
                        "expected_revision": null,
                    }),
                    no_primes,
                )
            }),
        ));
    }
    out.push(tg(
        "tracedecay_work_adjudicate_leak",
        fiveq(&|_| {
            eqn(
                "tracedecay_work_adjudicate_leak",
                "leak",
                json!({
                    "adjudication_id": "adjudication.bench.{{iter}}",
                    "attempt": ident(),
                    "command_id": "command.bench.leak.{{iter}}",
                    "detection_horizon_micros": 3_600_000_000_i64,
                    "expected_revision": null,
                }),
                no_primes,
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_retry_attempt",
        fiveq(&|_| {
            eqc(
                "tracedecay_work_retry_attempt",
                "retry",
                json!({
                    "original_attempt": ident(),
                    "new_attempt_id": "attempt.retry.bench.{{iter}}",
                    "failure": {
                        "source": "runtime",
                        "cause": "runtime_failure",
                        "evidence_ref": "bench",
                    },
                    "command_id": "command.bench.retry.{{iter}}",
                }),
                no_primes,
                crate::queries::EffectCleanup {
                    capture: &[],
                    steps: cleanup_cancel_retry,
                },
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_synthesize",
        fiveq(&|_| {
            eqc(
                "tracedecay_work_synthesize",
                "synthesize",
                json!({
                    "output_name": "bench.synthesis",
                    "sources": [ident()],
                    "start": {
                        "task_id": "task.bench.synth.{{iter}}",
                        "run_id": "run.bench.synth.{{iter}}",
                        "attempt_id": "attempt.bench.synth.{{iter}}",
                        "operation": "operation.work.start_attempt",
                        "execution_snapshot": w.execution_snapshot,
                        "worktree_root": ctx.project_root,
                        "commit": w.commit,
                        "instructions": "Bench synthesis attempt.",
                        "effect_state": "observational",
                        "occurred_at": "{{now}}",
                    },
                }),
                no_primes,
                crate::queries::EffectCleanup {
                    capture: &[],
                    steps: cleanup_cancel_synth,
                },
            )
        }),
    ));

    if !w.definition_id.is_empty() {
        out.push(tg(
            "tracedecay_workflow_list_definitions",
            fiveq(&|_| rqn("tracedecay_workflow_list_definitions", "list", json!({}))),
        ));
        out.push(tg(
            "tracedecay_workflow_get_definition",
            fiveq(&|_| {
                rqn(
                    "tracedecay_workflow_get_definition",
                    "get",
                    json!({
                        "definition_id": w.definition_id, "definition_version": 1,
                    }),
                )
            }),
        ));
        out.push(tg(
            "tracedecay_workflow_definition_history",
            fiveq(&|_| {
                rqn(
                    "tracedecay_workflow_definition_history",
                    "history",
                    json!({
                        "definition_id": w.definition_id,
                    }),
                )
            }),
        ));
        out.push(tg(
            "tracedecay_workflow_diff_definition",
            fiveq(&|_| {
                rqn(
                    "tracedecay_workflow_diff_definition",
                    "diff",
                    json!({
                        "definition_id": w.definition_id, "from_version": 1, "to_version": 1,
                    }),
                )
            }),
        ));
        out.push(tg(
            "tracedecay_workflow_validate_definition",
            fiveq(&|_| {
                rqn(
                    "tracedecay_workflow_validate_definition",
                    "validate",
                    json!({
                        "definition": w.definition,
                    }),
                )
            }),
        ));
        out.push(tg(
            "tracedecay_workflow_register_definition",
            fiveq(&|_| {
                eqn(
                    "tracedecay_workflow_register_definition",
                    "register",
                    json!({
                        "definition": wf_definition(ctx, "{{iter}}"),
                    }),
                    p_wf_register,
                )
            }),
        ));
        out.push(tg(
            "tracedecay_workflow_activate_definition",
            fiveq(&|_| {
                eqn(
                    "tracedecay_workflow_activate_definition",
                    "activate",
                    wf_activate_args("{{iter}}"),
                    p_wf_register,
                )
            }),
        ));
        out.push(tg(
            "tracedecay_workflow_reject_definition",
            fiveq(&|_| {
                eqn(
                    "tracedecay_workflow_reject_definition",
                    "reject",
                    wf_activate_args("{{iter}}"),
                    p_wf_register,
                )
            }),
        ));
        out.push(tg(
            "tracedecay_workflow_retire_definition",
            fiveq(&|_| {
                eqn(
                    "tracedecay_workflow_retire_definition",
                    "retire",
                    json!({
                        "definition_id": "workflow.bench.{{iter}}",
                        "definition_version": 1,
                        "expected_revision": "{{wf_rev}}",
                    }),
                    p_wf_active,
                )
            }),
        ));
        out.push(tg(
            "tracedecay_workflow_start_run",
            fiveq(&|_| {
                eqn(
                    "tracedecay_workflow_start_run",
                    "start",
                    wf_run_args("{{iter}}"),
                    p_wf_active,
                )
            }),
        ));
        out.push(tg(
            "tracedecay_workflow_pause_run",
            fiveq(&|_| {
                eqn(
                    "tracedecay_workflow_pause_run",
                    "pause",
                    json!({
                        "run_id": "workflow-run.bench.{{iter}}",
                        "expected_sequence": "{{wf_seq}}",
                        "command_id": "command.workflow.bench.pause.{{iter}}",
                    }),
                    p_wf_run,
                )
            }),
        ));
        out.push(tg(
            "tracedecay_workflow_resume_run",
            fiveq(&|_| {
                eqn(
                    "tracedecay_workflow_resume_run",
                    "resume",
                    json!({
                        "run_id": "workflow-run.bench.{{iter}}",
                        "expected_sequence": "{{wf_seq}}",
                        "command_id": "command.workflow.bench.resume.{{iter}}",
                    }),
                    p_wf_paused,
                )
            }),
        ));
        out.push(tg(
            "tracedecay_workflow_cancel_run",
            fiveq(&|_| {
                eqn(
                    "tracedecay_workflow_cancel_run",
                    "cancel",
                    json!({
                        "run_id": "workflow-run.bench.{{iter}}",
                        "expected_sequence": "{{wf_seq}}",
                        "command_id": "command.workflow.bench.cancel.{{iter}}",
                    }),
                    p_wf_run,
                )
            }),
        ));
        if w.worktree_id.is_some() && !w.actor_id.is_empty() {
            out.push(tg(
                "tracedecay_workflow_handoff_issue",
                fiveq(&|_| {
                    eqn(
                        "tracedecay_workflow_handoff_issue",
                        "issue",
                        issue_args(ctx),
                        p_handoff_run_only,
                    )
                }),
            ));
            out.push(tg(
                "tracedecay_workflow_handoff_redeem",
                fiveq(&|_| {
                    eqn(
                        "tracedecay_workflow_handoff_redeem",
                        "redeem",
                        json!({
                            "secret": "bench-handoff-{{iter}}-ssssssssssssssssssssssssssssssss",
                            "expected_scope": handoff_scope(ctx),
                        }),
                        p_handoff,
                    )
                }),
            ));
        }
        if let Some(run_id) = &w.wf_run_id {
            out.push(tg(
                "tracedecay_workflow_get_run",
                fiveq(&|_| {
                    rqn(
                        "tracedecay_workflow_get_run",
                        "get_run",
                        json!({"run_id": run_id}),
                    )
                }),
            ));
        }
    }
}
