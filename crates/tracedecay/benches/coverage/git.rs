//! Git family: status/diff/hunks/blame/history/preview/apply plus the branch
//! and git-adjacent reads.

use serde_json::json;

use crate::queries::{PrimeStep, Query, QueryContext, ToolGroup, five};

use super::{eq, rq};

fn path_at(ctx: &QueryContext, i: usize) -> String {
    crate::queries::dir(ctx, i)
}

pub(crate) fn groups(ctx: &QueryContext, out: &mut Vec<ToolGroup>) {
    out.push(ToolGroup {
        tool: "tracedecay_git_status",
        queries: five(|_i| rq("tracedecay_git_status", "status", json!({}))),
    });
    // Diff reads alternate the working-tree scope with a real commit range
    // when ancestry exists (shallow clones keep 64 commits by design).
    let range_base = ctx.seeds.parent_commit.clone();
    let range_head = ctx.seeds.head_commit.clone();
    out.push(ToolGroup {
        tool: "tracedecay_git_diff",
        queries: five(|i| {
            rq(
                "tracedecay_git_diff",
                "diff",
                if i % 2 == 0 && range_base.is_some() && range_head.is_some() {
                    json!({
                        "scope": "commit_range",
                        "base": range_base,
                        "head": range_head,
                        "max_bytes": 65536,
                        "max_entries": 64,
                    })
                } else {
                    json!({
                        "scope": "working_tree",
                        "max_bytes": 65536,
                        "max_entries": 64,
                    })
                },
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_git_hunks",
        queries: five(|i| {
            rq(
                "tracedecay_git_hunks",
                "hunks",
                json!({
                    "scope": *["working_tree", "staged"].iter().nth(i % 2).unwrap_or(&""),
                    "max_bytes": 65536,
                    "max_entries": 64,
                }),
            )
        }),
    });
    if !ctx.seeds.unavailable_tools.contains("tracedecay_git_blame") {
        out.push(ToolGroup {
            tool: "tracedecay_git_blame",
            queries: five(|i| {
                rq(
                    "tracedecay_git_blame",
                    "blame",
                    json!({
                        "path": path_at(ctx, i),
                        "max_bytes": 65536,
                        "max_entries": 64,
                    }),
                )
            }),
        });
    }
    out.push(ToolGroup {
        tool: "tracedecay_git_history",
        queries: five(|i| {
            rq(
                "tracedecay_git_history",
                "history",
                json!({
                    "path": path_at(ctx, i),
                    "max_bytes": 65536,
                    "max_entries": 64,
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_branch_list",
        queries: five(|i| {
            rq(
                "tracedecay_branch_list",
                "branch_list",
                json!({"limit": 20 + i}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_branch_search",
        queries: five(|i| {
            rq(
                "tracedecay_branch_search",
                "branch_search",
                json!({
                    "branch": ctx.seeds.branch.clone().unwrap_or_else(|| "main".into()),
                    "query": *["feat", "fix", "main", "dev", "rel"].iter().nth(i).unwrap_or(&""),
                    "limit": 10,
                }),
            )
        }),
    });
    if let (Some(base), Some(head)) = (ctx.seeds.base_branch.clone(), ctx.seeds.branch.clone()) {
        out.push(ToolGroup {
            tool: "tracedecay_branch_diff",
            queries: five(|_i| {
                rq(
                    "tracedecay_branch_diff",
                    "branch_diff",
                    json!({
                        "base": base,
                        "head": head,
                        "limit": 32,
                    }),
                )
            }),
        });
    }

    // git_preview: mints a stage preview from live hunk digests; re-mint the
    // preview input each iteration since previews expire on consumption.
    fn preview_primes(_ctx: &QueryContext, _iter: u64) -> Vec<PrimeStep> {
        vec![PrimeStep {
            inject: Vec::new(),
            tool: "tracedecay_git_hunks",
            args: json!({"scope": "working_tree", "format": "json"}),
            capture: &[
                ("dig:preview_input_id", "preview_input_id"),
                ("deep_array:hunk_digests", "hunk_digests"),
            ],
        }]
    }
    // Preview/apply lanes only run when seeding proved the hunk evidence
    // path serves a real preview input for this composition's dirty file.
    let preview_ready = ctx.seeds.preview_input_id.is_some() && !ctx.seeds.hunk_digests.is_empty();
    if preview_ready {
        out.push(ToolGroup {
            tool: "tracedecay_git_preview",
            queries: five(|_i| {
                eq(
                    "tracedecay_git_preview",
                    "preview_stage",
                    json!({
                        "operation": "stage_hunks",
                        "preview_input_id": "{{preview_input_id}}",
                        "selected_hunk_digests": "{{hunk_digests}}",
                    }),
                    preview_primes,
                )
            }),
        });
        out.push(ToolGroup {
            tool: "tracedecay_git_apply",
            queries: five(|_i| {
                eq(
                    "tracedecay_git_apply",
                    "apply_stage",
                    json!({
                        "preview_id": "{{preview_id}}",
                        "preview_digest": "{{preview_digest}}",
                        "idempotency_key": "bench-git-apply-{{iter}}",
                    }),
                    |ctx, iter| {
                        let mut steps = preview_primes(ctx, iter);
                        steps.push(PrimeStep {
                            inject: Vec::new(),
                            tool: "tracedecay_git_preview",
                            args: json!({
                                "operation": "stage_hunks",
                                "preview_input_id": "{{preview_input_id}}",
                                "selected_hunk_digests": "{{hunk_digests}}",
                                "format": "json",
                            }),
                            capture: &[
                                ("dig:preview_id", "preview_id"),
                                ("dig:preview_digest", "preview_digest"),
                            ],
                        });
                        steps
                    },
                )
            }),
        });
    }
    let _ = Query::read;
}
