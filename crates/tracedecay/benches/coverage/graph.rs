//! Graph-analysis family: similarity, redundancy, topology, impact, and
//! source reads — all generation-bound queries against the mounted graph.

use serde_json::{Value, json};

use crate::queries::{QueryContext, ToolGroup, five};

use super::{code::cqid, now_micros, rq};

const MATCH_CLASSES: [&str; 2] = ["conservative_exact", "rename_normalized_exact"];
const NAMES: [&str; 5] = ["main", "init", "parse", "run", "handle"];

fn repo_ids(ctx: &QueryContext) -> (Value, Value) {
    (
        json!(ctx.seeds.project_id.clone().unwrap_or_else(|| "missing".into())),
        json!(ctx
            .seeds
            .repository_id
            .clone()
            .unwrap_or_else(|| "missing".into())),
    )
}

fn path_at(ctx: &QueryContext, i: usize) -> String {
    crate::queries::dir(ctx, i)
}

pub(crate) fn groups(ctx: &QueryContext, out: &mut Vec<ToolGroup>) {
    let (pid, rid) = repo_ids(ctx);

    out.push(ToolGroup {
        tool: "tracedecay_similar",
        queries: five(|i| {
            rq(
                "tracedecay_similar",
                "similar",
                json!({
                    "project_id": pid,
                    "repository_id": rid,
                    "target": {
                        "kind": "symbol_occurrence",
                        "symbol_occurrence_id": cqid(ctx, i),
                    },
                    "match_classes": [MATCH_CLASSES[i % 2]],
                    "result_limit": 20,
                    "work_limit": 4,
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_redundancy",
        queries: five(|i| {
            rq(
                "tracedecay_redundancy",
                "redundancy",
                json!({
                    "project_id": pid,
                    "repository_id": rid,
                    "scope": {"kind": "path", "path": path_at(ctx, i)},
                    "match_classes": [MATCH_CLASSES[i % 2]],
                    "family_limit": 20,
                    "member_limit": 10,
                    "work_limit": 4,
                    "include_generated_paths": false,
                }),
            )
        }),
    });
    for (tool, label) in [
        ("tracedecay_recursion", "recursion"),
        ("tracedecay_inheritance_depth", "inheritance_depth"),
        ("tracedecay_dependency_depth", "dependency_depth"),
        ("tracedecay_distribution", "distribution"),
        ("tracedecay_unsafe_patterns", "unsafe_patterns"),
        ("tracedecay_unmounted_files", "unmounted_files"),
        ("tracedecay_test_risk", "test_risk"),
        ("tracedecay_todos", "todos"),
    ] {
        out.push(ToolGroup {
            tool,
            queries: five(|i| {
                rq(
                    tool,
                    label,
                    json!({"path": path_at(ctx, i), "limit": 25}),
                )
            }),
        });
    }
    out.push(ToolGroup {
        tool: "tracedecay_dsm",
        queries: five(|i| {
            rq(
                "tracedecay_dsm",
                "dsm",
                json!({"path": path_at(ctx, i), "max_files": 50, "shape": null}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_gini",
        queries: five(|i| {
            rq(
                "tracedecay_gini",
                "gini",
                json!({"path": path_at(ctx, i), "limit": 50, "metric": null, "scope": null}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_port_status",
        queries: five(|i| {
            rq(
                "tracedecay_port_status",
                "port_status",
                json!({
                    "source_dir": path_at(ctx, i),
                    "target_dir": path_at(ctx, i + 1),
                    "kinds": null,
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_port_order",
        queries: five(|i| {
            rq(
                "tracedecay_port_order",
                "port_order",
                json!({"source_dir": path_at(ctx, i), "kinds": null, "limit": 20}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_file_dependents",
        queries: five(|i| {
            rq(
                "tracedecay_file_dependents",
                "file_dependents",
                json!({"file": path_at(ctx, i)}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_test_map",
        queries: five(|i| {
            rq(
                "tracedecay_test_map",
                "test_map",
                if i % 2 == 0 {
                    json!({"file": path_at(ctx, i)})
                } else {
                    json!({"node_id": QueryContext::pick(&ctx.any_ids, i)})
                },
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_affected",
        queries: five(|i| {
            rq(
                "tracedecay_affected",
                "affected",
                json!({"files": [path_at(ctx, i)], "depth": 2}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_diff_context",
        queries: five(|i| {
            rq(
                "tracedecay_diff_context",
                "diff_context",
                json!({"files": [path_at(ctx, i)], "depth": 2}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_commit_context",
        queries: five(|i| {
            rq(
                "tracedecay_commit_context",
                "commit_context",
                json!({"staged_only": i % 2 == 0}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_pr_context",
        queries: five(|_i| {
            rq(
                "tracedecay_pr_context",
                "pr_context",
                json!({
                    "base_ref": "HEAD~1",
                    "head_ref": "HEAD",
                    "maximum_symbols": 64,
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_changelog",
        queries: five(|i| {
            rq(
                "tracedecay_changelog",
                "changelog",
                json!({"from_ref": format!("HEAD~{}", i + 1), "to_ref": "HEAD"}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_diagnose",
        queries: five(|i| {
            rq(
                "tracedecay_diagnose",
                "diagnose",
                json!({
                    "cargo_output": format!("error[E0308]: mismatched types in {}", NAMES[i]),
                    "include_callers": i % 2 == 0,
                    "max_diagnostics": 8,
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_diagnostics",
        queries: five(|i| {
            rq(
                "tracedecay_diagnostics",
                "diagnostics",
                json!({"path": path_at(ctx, i), "maximum_diagnostics": 25}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_run_affected_tests",
        queries: five(|i| {
            rq(
                "tracedecay_run_affected_tests",
                "run_affected",
                json!({
                    "changed_paths": [path_at(ctx, i)],
                    "max_tests": 5,
                    "timeout_secs": 30,
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_source_outline",
        queries: five(|i| {
            rq(
                "tracedecay_source_outline",
                "source_outline",
                json!({"file": path_at(ctx, i)}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_source_body",
        queries: five(|i| {
            rq(
                "tracedecay_source_body",
                "source_body",
                json!({"node_id": QueryContext::pick(&ctx.any_ids, i)}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_source_lines",
        queries: five(|i| {
            rq(
                "tracedecay_source_lines",
                "source_lines",
                json!({
                    "file": path_at(ctx, i),
                    "span": {"start_byte": 0, "end_byte": 2048},
                    "meta": {
                        "projection": "summary",
                        "order": "source_position",
                        "page": {"page_size": 20, "cursor": null},
                        "temporal": {"kind": "current"},
                    },
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_grep",
        queries: five(|i| {
            rq(
                "tracedecay_grep",
                "grep",
                json!({
                    "pattern": NAMES[i % NAMES.len()],
                    "path_glob": "**/*.rs",
                    "max_results": 25,
                    "fixed_strings": true,
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_ast_grep_search",
        queries: five(|_i| {
            rq(
                "tracedecay_ast_grep_search",
                "ast_grep_search",
                json!({
                    "pattern": "fn $F($$$ARGS) { $$$BODY }",
                    "path_glob": "**/*.rs",
                    "lang": "rust",
                    "max_results": 25,
                }),
            )
        }),
    });
    let _ = now_micros;
}
