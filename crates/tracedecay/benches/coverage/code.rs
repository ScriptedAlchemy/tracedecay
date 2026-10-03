//! Code-navigation family: generation-bound queries consuming the code-query
//! node identities minted by `code_symbol_search` during seeding.

use serde_json::{Value, json};

use crate::queries::{EffectCleanup, PrimeStep, QueryContext, ToolGroup, five};

use super::{eqc, rq};

/// `SymbolGraphScope` — symbol-surface tools narrow by path prefix only.
fn scope() -> Value {
    json!({ "path_prefix": null })
}

/// `CodeQueryScope` — lexical/code-query tools pin the mounted generation.
fn cq_scope() -> Value {
    json!({
        "generation": "code-generation:unpinned-latest.v1",
        "path_prefix": null,
    })
}

fn meta(projection: &str, order: &str) -> Value {
    json!({"projection": projection, "order": order, "cursor": null})
}

/// Code-query node id (from `code_symbol_search`), falling back to graph ids
/// when the search seed minted nothing — a missing id still times the
/// not-found path rather than fabricating coverage.
pub(crate) fn cqid(ctx: &QueryContext, i: usize) -> String {
    if !ctx.seeds.code_node_ids.is_empty() {
        ctx.seeds.code_node_ids[i % ctx.seeds.code_node_ids.len()].clone()
    } else {
        QueryContext::pick(&ctx.function_ids, i)
    }
}

const NAMES: [&str; 5] = ["main", "init", "parse", "run", "handle"];

pub(crate) fn groups(ctx: &QueryContext, out: &mut Vec<ToolGroup>) {
    out.push(ToolGroup {
        tool: "tracedecay_code_symbol_search",
        queries: five(|i| {
            rq(
                "tracedecay_code_symbol_search",
                "symbol_search",
                json!({
                    "query": NAMES[i % NAMES.len()],
                    "lazy_index_ignored_dependencies": true,
                    "scope": scope(),
                    "meta": meta("summary", "relevance"),
                }),
            )
        }),
    });
    for tool in [
        "tracedecay_code_declaration",
        "tracedecay_code_references",
        "tracedecay_code_type_definition",
    ] {
        out.push(ToolGroup {
            tool,
            queries: five(|i| {
                rq(
                    tool,
                    "code_nav",
                    json!({
                        "node_id": cqid(ctx, i),
                        "scope": cq_scope(),
                        "meta": meta("summary", "relevance"),
                    }),
                )
            }),
        });
    }
    out.push(ToolGroup {
        tool: "tracedecay_code_phrase_search",
        queries: five(|i| {
            rq(
                "tracedecay_code_phrase_search",
                "phrase_search",
                json!({
                    "query": NAMES[i % NAMES.len()],
                    "phrases": [format!("{} {}", NAMES[i % NAMES.len()], NAMES[(i + 1) % NAMES.len()])],
                    "scope": cq_scope(),
                    "meta": meta("summary", "relevance"),
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_code_exact_occurrence",
        queries: five(|i| {
            rq(
                "tracedecay_code_exact_occurrence",
                "exact_occurrence",
                json!({
                    "literal": NAMES[i % NAMES.len()],
                    "scope": cq_scope(),
                    "meta": meta("references_only", "source_position"),
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_code_facets",
        queries: five(|i| {
            rq(
                "tracedecay_code_facets",
                "facets",
                json!({
                    "dimension": *["kind", "language", "path", "kind", "language"].iter().nth(i).unwrap_or(&"kind"),
                    "scope": cq_scope(),
                    "meta": meta("summary", "stable_identity"),
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_code_timeline",
        queries: five(|i| {
            rq(
                "tracedecay_code_timeline",
                "timeline",
                json!({
                    "scope": {
                        "generation": "code-generation:unpinned-latest.v1",
                        "path_prefix": crate::queries::dir(ctx, i),
                    },
                    "meta": meta("summary", "temporal_descending"),
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_type_hierarchy",
        queries: five(|i| {
            rq(
                "tracedecay_type_hierarchy",
                "type_hierarchy",
                json!({
                    "node_id": cqid(ctx, i),
                    "maximum_depth": 4,
                    "scope": scope(),
                    "meta": meta("summary", "relevance"),
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_call_chain",
        queries: five(|i| {
            rq(
                "tracedecay_call_chain",
                "call_chain",
                json!({
                    "from_node_id": cqid(ctx, i),
                    "to_node_id": cqid(ctx, i + 1),
                    "maximum_depth": 4,
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_constructors",
        queries: five(|i| {
            rq(
                "tracedecay_constructors",
                "constructors",
                json!({"struct": QueryContext::pick(&ctx.struct_ids, i), "limit": 10}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_field_sites",
        queries: five(|i| {
            rq(
                "tracedecay_field_sites",
                "field_sites",
                json!({"field": NAMES[i % NAMES.len()], "limit": 20, "writes_only": i % 2 == 0}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_implementations",
        queries: five(|i| {
            rq(
                "tracedecay_implementations",
                "implementations",
                json!({
                    "selector": {"selector": "trait", "name": NAMES[i % NAMES.len()]},
                    "scope": scope(),
                    "meta": meta("summary", "relevance"),
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_qualified_name",
        queries: five(|i| {
            rq(
                "tracedecay_qualified_name",
                "qualified_name",
                json!({
                    "qualified_name": QueryContext::pick(&ctx.function_qnames, i),
                    "page": {"page_size": 10, "cursor": null},
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_signature_search",
        queries: five(|i| {
            rq(
                "tracedecay_signature_search",
                "signature_search",
                json!({
                    "returns": *["i32", "bool", "String", "void", "Result"].iter().nth(i).unwrap_or(&""),
                    "params": [NAMES[i % NAMES.len()]],
                    "scope": scope(),
                    "meta": meta("summary", "relevance"),
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_find_exact_symbol",
        queries: five(|i| {
            rq(
                "tracedecay_find_exact_symbol",
                "find_exact",
                json!({
                    "name": QueryContext::pick(&ctx.function_qnames, i)
                        .rsplit("::")
                        .next()
                        .unwrap_or("main")
                        .to_owned(),
                    "limit": 10,
                    "lazy_index_ignored_dependencies": true,
                }),
            )
        }),
    });

    // ── Source-edit family: each tool's timed lane measures its full
    // plan+verify path (dry_run), and move_symbol additionally runs the real
    // apply → source_edit_rollback pair. Rollback material is retained only
    // for move effects (tracedecay-source-edit journal: non-move operations
    // persist no rollback record), so rename/replace/insert applies could
    // never be cleaned up; their dry_run lanes use a fresh input per
    // iteration so the measured plan is not an idempotent replay. ──
    out.push(ToolGroup {
        tool: "tracedecay_rename_preview",
        queries: five(|i| {
            rq(
                "tracedecay_rename_preview",
                "rename_preview",
                json!({"node_id": cqid(ctx, i), "new_name": format!("bench_renamed_{i}")}),
            )
        }),
    });
    // The plan lane only runs when seed probing found a renameable node —
    // an absent target is reported via seeds.skipped, not a fabricated one.
    if ctx.seeds.rename_node.is_some() {
        out.push(ToolGroup {
            tool: "tracedecay_rename_symbol",
            queries: five(|_i| {
                eqc(
                "tracedecay_rename_symbol",
                "rename_plan",
                json!({
                    "node_id": "{{rp_id}}",
                    "qualified_name": "{{rp_qname}}",
                    "kind": "{{rp_kind}}",
                    "file": "{{rp_file}}",
                    "old_name": "{{rp_name}}",
                    "new_name": "bench_plan_{{iter}}",
                    "dry_run": true,
                    "format": "json",
                    }),
                    p_rename,
                    no_cleanup(),
                )
            }),
        });
    }
    // Symbol-edit apply lanes: each runs only against the symbol seed time
    // proved unblocked + small; absence is reported via seeds.skipped.
    if ctx.seeds.replace_target.is_some() {
        out.push(ToolGroup {
            tool: "tracedecay_replace_symbol",
            queries: five(|_i| {
                eqc(
                    "tracedecay_replace_symbol",
                    "replace_plan",
                    json!({
                        "symbol": "{{sym}}",
                        "new_source": "pub fn bench_target() -> i32 { 77 }",
                        "dry_run": true,
                        "format": "json",
                    }),
                    p_replace,
                    no_cleanup(),
                )
            }),
        });
    }
    if ctx.seeds.insert_target.is_some() {
        out.push(ToolGroup {
            tool: "tracedecay_insert_at_symbol",
            queries: five(|_i| {
                eqc(
                    "tracedecay_insert_at_symbol",
                    "insert_plan",
                    json!({
                        "symbol": "{{sym}}",
                        "content": "// bench plan marker",
                        "position": "after",
                        "dry_run": true,
                        "format": "json",
                    }),
                    p_insert,
                    no_cleanup(),
                )
            }),
        });
    }
    if ctx.seeds.move_target.is_some() {
        out.push(ToolGroup {
            tool: "tracedecay_move_symbol",
            queries: five(|_i| {
                eqc(
                    "tracedecay_move_symbol",
                    "move_apply",
                    json!({
                        "symbol": "{{sym}}",
                        "dest_file": "{{dest_file}}",
                        "dry_run": false,
                        "update_references": false,
                        "expected_state": "{{expected_state}}",
                        "idempotency_key": "bench-move-src-{{iter}}",
                    }),
                    p_move,
                    rollback_cleanup("bench-move"),
                )
            }),
        });
        // source_edit_rollback itself: a fresh journaled move per iteration
        // mints the receipt identity the timed rollback consumes. Only
        // move_symbol retains rollback material, so this pair is also the
        // sole real-apply coverage in the family.

        out.push(ToolGroup {
            tool: "tracedecay_source_edit_rollback",
            queries: five(|_i| {
                eqc(
                    "tracedecay_source_edit_rollback",
                    "rollback_apply",
                    json!({
                        "effect_id": "{{jr_effect_id}}",
                        "original_idempotency_key": "bench-jr-src-{{iter}}",
                        "idempotency_key": "bench-jr-rollback-{{iter}}",
                        "original_input_digest": "{{jr_input_digest}}",
                        "expected_state": "{{jr_committed_state}}",
                        "confirm": true,
                    }),
                    p_journaled_move,
                    no_cleanup(),
                )
            }),
        });
    }
}

fn edit_dest(ctx: &QueryContext) -> String {
    ctx.seeds
        .sample_files
        .first()
        .cloned()
        .unwrap_or_else(|| "src/__init__.py".to_owned())
}

fn p_rename(ctx: &QueryContext, _iter: u64) -> Vec<PrimeStep> {
    let node_id = ctx
        .seeds
        .rename_node
        .as_ref()
        .and_then(|node| node.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("missing")
        .to_owned();
    vec![
        PrimeStep {
            inject: Vec::new(),
            tool: "tracedecay_rename_preview",
            args: json!({
                "node_id": node_id,
                "new_name": "bench_renamed_{{iter}}",
                "format": "json",
            }),
            capture: &[
                ("digpath:node:id", "rp_id"),
                ("digpath:node:qualified_name", "rp_qname"),
                ("digpath:node:kind", "rp_kind"),
                ("digpath:node:file", "rp_file"),
                ("digpath:node:name", "rp_name"),
            ],
        },
    ]
}

fn sym_inject(ctx: &QueryContext, target: &Option<String>) -> Vec<(String, Value)> {
    vec![
        (
            "sym".to_owned(),
            json!(target.clone().unwrap_or_else(|| "missing".to_owned())),
        ),
        (
            "sym_source".to_owned(),
            json!("pub fn bench_target() -> i32 { 42 }"),
        ),
        ("dest_file".to_owned(), json!(edit_dest(ctx))),
    ]
}

fn p_replace(ctx: &QueryContext, _iter: u64) -> Vec<PrimeStep> {
    vec![PrimeStep {
        inject: sym_inject(ctx, &ctx.seeds.replace_target),
        tool: "tracedecay_replace_symbol",
        args: json!({
            "symbol": "{{sym}}",
            "new_source": "{{sym_source}}",
            "dry_run": true,
            "format": "json",
        }),
        capture: &[("dig:expected_state", "expected_state")],
    }]
}

fn p_insert(ctx: &QueryContext, _iter: u64) -> Vec<PrimeStep> {
    vec![PrimeStep {
        inject: sym_inject(ctx, &ctx.seeds.insert_target),
        tool: "tracedecay_insert_at_symbol",
        args: json!({
            "symbol": "{{sym}}",
            "content": "// bench insert marker",
            "position": "after",
            "dry_run": true,
            "format": "json",
        }),
        capture: &[("dig:expected_state", "expected_state")],
    }]
}

fn p_move(ctx: &QueryContext, _iter: u64) -> Vec<PrimeStep> {
    vec![PrimeStep {
        inject: sym_inject(ctx, &ctx.seeds.move_target),
        tool: "tracedecay_move_symbol",
        args: json!({
            "symbol": "{{sym}}",
            "dest_file": "{{dest_file}}",
            "dry_run": true,
            "format": "json",
        }),
        capture: &[("dig:expected_state", "expected_state")],
    }]
}

fn p_journaled_move(ctx: &QueryContext, _iter: u64) -> Vec<PrimeStep> {
    vec![
        PrimeStep {
            inject: sym_inject(ctx, &ctx.seeds.move_target),
            tool: "tracedecay_move_symbol",
            args: json!({
                "symbol": "{{sym}}",
                "dest_file": "{{dest_file}}",
                "dry_run": true,
                "format": "json",
            }),
            capture: &[("dig:expected_state", "jr_expected_state")],
        },
        PrimeStep {
            inject: Vec::new(),
            tool: "tracedecay_move_symbol",
            args: json!({
                "symbol": "{{sym}}",
                "dest_file": "{{dest_file}}",
                "dry_run": false,
                "update_references": false,
                "expected_state": "{{jr_expected_state}}",
                "idempotency_key": "bench-jr-src-{{iter}}",
                "format": "json",
            }),
            capture: &[
                ("dig:effect_id", "jr_effect_id"),
                ("dig:input_digest", "jr_input_digest"),
                ("dig:committed_state", "jr_committed_state"),
            ],
        },
    ]
}

/// Journaled restore after a timed source-edit apply: the timed response's
/// receipt mints every identity the rollback consumes.
fn rollback_cleanup(key_prefix: &'static str) -> EffectCleanup {
    let _ = key_prefix;
    EffectCleanup {
        capture: &[
            ("dig:effect_id", "rb_effect_id"),
            ("dig:input_digest", "rb_input_digest"),
            ("dig:committed_state", "rb_committed_state"),
        ],
        steps: rb_move,
    }
}

fn rb_args(prefix: &str) -> Value {
    json!({
        "effect_id": "{{rb_effect_id}}",
        "original_idempotency_key": format!("{prefix}-src-{{{{iter}}}}"),
        "idempotency_key": format!("{prefix}-rollback-{{{{iter}}}}"),
        "original_input_digest": "{{rb_input_digest}}",
        "expected_state": "{{rb_committed_state}}",
        "confirm": true,
        "format": "json",
    })
}

fn rb_move(_ctx: &QueryContext, _iter: u64) -> Vec<PrimeStep> {
    vec![PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_source_edit_rollback",
        args: rb_args("bench-move"),
        capture: &[],
    }]
}

/// `eqc` needs an `EffectCleanup` value; a zero-step one expresses "no
/// restore needed" without `Option` plumbing at the call site.
fn no_cleanup() -> EffectCleanup {
    EffectCleanup {
        capture: &[],
        steps: |_ctx, _iter| Vec::new(),
    }
}
