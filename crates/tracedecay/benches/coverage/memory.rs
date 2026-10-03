//! Memory family: fact store reads, fact effect writes (add/update/remove/
//! supersede with per-iteration fresh ids), and the feedback read surface
//! (handles minted by `feedback_advisory_cycle`).

use serde_json::json;

use crate::queries::{PrimeStep, QueryContext, ToolGroup, five};

use super::{eq, fact_add_args, file_at, no_primes, rq};

fn doc_uri(ctx: &QueryContext, i: usize) -> String {
    format!("file://{}/{}", ctx.project_root.display(), file_at(ctx, i))
}

pub(crate) fn groups(ctx: &QueryContext, out: &mut Vec<ToolGroup>) {
    let pair = ctx.seeds.fact_pair.clone();
    let (fact_id, related_id, fact_query) = pair
        .as_ref()
        .map(|p| (p.0.clone(), p.1.clone(), p.2.clone()))
        .unwrap_or_else(|| ("missing".into(), "missing".into(), "missing".into()));
    let entities = pair
        .as_ref()
        .map(|p| p.3.clone())
        .unwrap_or_default();

    out.push(ToolGroup {
        tool: "tracedecay_fact_store_get",
        queries: five(|i| {
            rq(
                "tracedecay_fact_store_get",
                "get",
                json!({"fact_id": if i % 2 == 0 { fact_id.clone() } else { related_id.clone() }}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_fact_store_list",
        queries: five(|i| {
            rq(
                "tracedecay_fact_store_list",
                "list",
                json!({"limit": 10 + i as u32}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_fact_store_related",
        queries: five(|i| {
            rq(
                "tracedecay_fact_store_related",
                "related",
                json!({
                    "entity": entities
                        .get(i % entities.len().max(1))
                        .cloned()
                        .unwrap_or_else(|| "missing".into()),
                    "limit": 10,
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_fact_store_add",
        queries: five(|i| {
            eq(
                "tracedecay_fact_store_add",
                "add",
                fact_add_args(
                    "bench added fact",
                    &[
                        format!("bench-alpha-{i}"),
                        format!("bench-beta-{i}"),
                    ],
                    0.9,
                ),
                no_primes,
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_fact_store_update",
        queries: five(|_i| {
            eq(
                "tracedecay_fact_store_update",
                "update",
                json!({
                    "fact_id": "{{fact_id}}",
                    "content": "bench updated fact {{iter}}",
                    "entities": ["bench-updated-{{iter}}"],
                    "trust": 0.95,
                    "source_label": {"kind": "set", "value": "bench"},
                }),
                |_ctx, iter| vec![add_step(iter, "update")],
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_fact_store_remove",
        queries: five(|_i| {
            eq(
                "tracedecay_fact_store_remove",
                "remove",
                json!({
                    "fact_id": "{{fact_id}}",
                }),
                |_ctx, iter| vec![add_step(iter, "remove")],
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_fact_store_supersede",
        queries: five(|_i| {
            eq(
                "tracedecay_fact_store_supersede",
                "supersede",
                json!({
                    "fact_id": "{{old_fact_id}}",
                    "superseded_by": "{{new_fact_id}}",
                }),
                |_ctx, iter| {
                    vec![
                        add_step_named(iter, "sup-old", "old_fact_id"),
                        add_step_named(iter, "sup-new", "new_fact_id"),
                    ]
                },
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_fact_feedback",
        queries: five(|_i| {
            eq(
                "tracedecay_fact_feedback",
                "fact_feedback",
                json!({
                    "fact_id": "{{fact_id}}",
                    "action": "helpful",
                    "reason": "bench feedback {{iter}}",
                    "source_label": "bench",
                }),
                |_ctx, iter| vec![add_step(iter, "feedback")],
            )
        }),
    });
    for (tool, args) in [
        (
            "tracedecay_fact_store_search",
            json!({"query": fact_query, "limit": 10}),
        ),
        (
            "tracedecay_fact_store_probe",
            json!({"entity": entities.first().cloned().unwrap_or_else(|| "missing".into()), "limit": 10}),
        ),
        (
            "tracedecay_fact_store_reason",
            json!({"entities": entities, "limit": 10}),
        ),
        (
            "tracedecay_fact_store_contradict",
            json!({"limit": 10}),
        ),
        (
            "tracedecay_fact_store_curate",
            json!({"fact_review_limit": 10, "min_confidence_millionths": 0}),
        ),
    ] {
        out.push(ToolGroup {
            tool,
            queries: five(|_i| rq(tool, "read", args.clone())),
        });
    }
    out.push(ToolGroup {
        tool: "tracedecay_feedback_advisory_cycle",
        queries: five(|i| {
            rq(
                "tracedecay_feedback_advisory_cycle",
                "advisory_cycle",
                json!({"document_uri": doc_uri(ctx, i)}),
            )
        }),
    });
    // Feedback reads consume daemon-minted handles; each tool's handle lives
    // under a different name in the advisory-cycle payload.
    macro_rules! feedback_read {
        ($tool:literal, $label:literal, $spec:literal) => {
            out.push(ToolGroup {
                tool: $tool,
                queries: five(|_i| {
                    eq(
                        $tool,
                        $label,
                        json!({"request_handle": "{{request_handle}}"}),
                        |ctx, iter| {
                            vec![PrimeStep {
    inject: Vec::new(),
                                tool: "tracedecay_feedback_advisory_cycle",
                                args: json!({
                                    "document_uri": doc_uri(ctx, iter as usize),
                                    "format": "json",
                                }),
                                capture: &[($spec, "request_handle")],
                            }]
                        },
                    )
                }),
            });
        };
    }
    feedback_read!("tracedecay_feedback_impact", "impact", "digany:impact_handle");
    feedback_read!("tracedecay_feedback_list", "flist", "digany:list_handle,request_handle");
    feedback_read!("tracedecay_feedback_get", "get", "digany:get_handle");
    feedback_read!("tracedecay_feedback_expand", "expand", "digany:expansion_handle");
    feedback_read!("tracedecay_feedback_diagnostics", "diagnostics", "digany:diagnostics_handle");
    // affected_tests computes the affected set directly; test_results reads
    // the results a primed run_affected_tests execution retained.
    out.push(ToolGroup {
        tool: "tracedecay_affected_tests",
        queries: five(|i| {
            rq(
                "tracedecay_affected_tests",
                "affected_tests",
                json!({
                    "changed_paths": [ctx
                        .seeds
                        .test_results_path
                        .clone()
                        .unwrap_or_else(|| file_at(ctx, i as usize))],
                    "max_tests": 5,
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_test_results",
        queries: five(|_i| {
            eq(
                "tracedecay_test_results",
                "test_results",
                json!({}),
                affected_results_prime,
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_feedback_proximity",
        queries: five(|_i| {
            rq(
                "tracedecay_feedback_proximity",
                "proximity",
                json!({"observed_at": super::now_micros()}),
            )
        }),
    });
}

fn affected_results_prime(ctx: &QueryContext, iter: u64) -> Vec<PrimeStep> {
    vec![PrimeStep {
    inject: Vec::new(),
        tool: "tracedecay_run_affected_tests",
        args: json!({
            "changed_paths": [ctx
                .seeds
                .test_results_path
                .clone()
                .unwrap_or_else(|| file_at(ctx, iter as usize))],
            "max_tests": 5,
            "timeout_secs": 30,
            "format": "json",
        }),
        capture: &[],
    }]
}

fn add_step(iter: u64, tag: &str) -> PrimeStep {
    add_step_named(iter, tag, "fact_id")
}

fn add_step_named(iter: u64, tag: &str, token: &'static str) -> PrimeStep {
    let a = format!("bench-{tag}-a-{iter}");
    let b = format!("bench-{tag}-b-{iter}");
    let mut args = fact_add_args(&format!("bench seeded fact {tag} {iter}"), &[a, b], 0.9);
    args["format"] = json!("json");
    let capture: &[(&str, &str)] = match token {
        "old_fact_id" => &[("dig:fact_id", "old_fact_id")],
        "new_fact_id" => &[("dig:fact_id", "new_fact_id")],
        _ => &[("dig:fact_id", "fact_id")],
    };
    PrimeStep {
    inject: Vec::new(),
        tool: "tracedecay_fact_store_add",
        args,
        capture,
    }
}
