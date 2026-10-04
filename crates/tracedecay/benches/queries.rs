//! Static (per-repo) query catalog used by the criterion bench.
//!
//! Queries are constructed once after the production composition publishes a
//! code-index generation. The `QueryContext` is sampled through mounted MCP
//! reads, so top-ranked symbols, qualified names, and file prefixes all come
//! from the same admitted, generation-pinned graph used by the timed calls.
//!
//! Write queries (`str_replace`, `multi_str_replace`, `insert_at`,
//! `ast_grep_rewrite`) declare a `scratch_path` + `init_content`: the bench
//! harness rewrites the scratch file before *every* timed iteration so the
//! tool's "match must be unique" precondition keeps holding. A
//! `git stash --include-untracked` at the end of the run reverts all
//! scratch-file churn.

#![allow(clippy::too_many_lines)]
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay_mcp::JsonRpcResponse;

/// One untimed setup call executed before an `Effect` iteration. `capture`
/// maps `result`-relative JSON paths to `{{token}}` names; captured values are
/// substituted into later step args and into the timed call's args, so
/// lifecycle chains can mint fresh identities per iteration.
pub struct PrimeStep {
    /// `{{token}}` values computed by the prime builder itself (no tool call
    /// needed) — substituted before `args`, so the step's own args may
    /// reference them.
    pub inject: Vec<(String, Value)>,
    pub tool: &'static str,
    pub args: Value,
    pub capture: &'static [(&'static str, &'static str)],
}

/// Builds the untimed prime chain for one effect iteration: given the sampled
/// context and the iteration counter, returns the ordered setup calls.
pub type PrimeFn = fn(&QueryContext, u64) -> Vec<PrimeStep>;

/// Distinguishes read-only queries from queries that mutate state.
#[derive(Clone)]
pub enum QueryKind {
    Read,
    Write {
        /// File path *relative to the project root* that must be (re)written
        /// before each timed iteration.
        scratch_path: String,
        /// Bytes the scratch file is reset to before each iter.
        init_content: String,
    },
    /// One-shot or stateful operations measured with fresh preconditions per
    /// iteration: the `prime` chain (untimed) recreates the entities the timed
    /// call consumes, so every iteration measures a real effect, not a replay.
    /// `cleanup` runs untimed AFTER the timed call — journaled restores that
    /// return the shared bench corpus to its precondition state.
    Effect {
        prime: PrimeFn,
        cleanup: Option<EffectCleanup>,
    },
}

/// Post-timed restore for an `Effect` query: `capture` reads identities out
/// of the timed response (e.g. a minted `effect_id`) into the token table,
/// then `steps` runs the restore chain untimed.
#[derive(Clone)]
pub struct EffectCleanup {
    pub capture: &'static [(&'static str, &'static str)],
    pub steps: PrimeFn,
}

/// One concrete tool invocation: the MCP tool name, its args, and the kind
/// (read vs. write) that drives criterion's iteration strategy.
#[derive(Clone)]
pub struct Query {
    pub label: &'static str,
    pub tool: &'static str,
    pub args: Value,
    pub kind: QueryKind,
}

impl Query {
    pub(crate) fn read(label: &'static str, tool: &'static str, args: Value) -> Self {
        Self {
            label,
            tool,
            args,
            kind: QueryKind::Read,
        }
    }

    pub(crate) fn write(
        label: &'static str,
        tool: &'static str,
        args: Value,
        scratch_path: String,
        init_content: String,
    ) -> Self {
        Self {
            label,
            tool,
            args,
            kind: QueryKind::Write {
                scratch_path,
                init_content,
            },
        }
    }
}

// Effect queries are constructed literally at the coverage call sites —
// constructor sugar lives there (`eq`/`eqc`), since this file is also the
// standalone `queries` bench root where effect groups never exist.

/// All queries we run for one tool. The harness invariant is `queries.len() == 5`.
pub struct ToolGroup {
    pub tool: &'static str,
    pub queries: Vec<Query>,
}

/// Entity state minted during context build; `None`/empty means the producer
/// failed and the dependent groups are skipped (the reason lands in `skipped`).
///
/// Lives here (not in `coverage/`) because `QueryContext` carries it and this
/// file is compiled as two bench roots: the standalone `queries` bench has no
/// `coverage` module, so every type it names must resolve in this crate root.
#[derive(Default)]
pub struct Seeds {
    pub project_id: Option<String>,
    pub repository_id: Option<String>,
    pub branch: Option<String>,
    pub head_commit: Option<String>,
    /// First-parent ancestor of `head_commit` for range-diff coverage.
    pub parent_commit: Option<String>,
    /// (fact_id, related_fact_id, search_query, [entities])
    pub fact_pair: Option<(String, String, String, Vec<String>)>,
    /// Scalar boolean key toggled during seed (diagnostics.prewarm.v1).
    pub config_key: Option<String>,
    /// Latest configuration revision after the seed set/unset pair.
    pub config_revision: Option<String>,
    /// The seeded revision (rollback target).
    pub config_rollback_target: Option<String>,
    /// Toggled scalar value to set next ({kind:boolean,value}).
    pub config_scalar: Option<Value>,
    /// Effective work.topology_policy.v1 value for protected previews.
    pub topology_policy: Option<Value>,
    /// Code-query node identities minted via `code_symbol_search` (distinct
    /// from graph node ids; these tools consume the query-side identity).
    pub code_node_ids: Vec<String>,
    /// A graph node whose rename_preview is unblocked (source-edit lane needs
    /// a symbol the policy can actually rename — most corpus symbols refuse
    /// with ambiguous/hazard evidence, so seed time probes candidates).
    pub rename_node: Option<Value>,
    /// Symbol qualified names whose per-op dry_run completed small enough to
    /// return `expected_state` inline (big symbols truncate the preview into
    /// a result handle, which timing must not depend on).
    pub replace_target: Option<String>,
    pub insert_target: Option<String>,
    pub move_target: Option<String>,
    /// Ingested codex session id for this repo (td-bench-<name>).
    pub lcm_session: Option<String>,
    /// Canonical occurrence id minted by lcm_load_session.
    pub lcm_message_id: Option<String>,
    /// multi_root scope_set committed via compare_and_swap (id/revision/digest).
    pub scope_set_id: Option<String>,
    pub scope_set_revision: Option<i64>,
    pub scope_set_digest: Option<String>,
    /// session_refresh_begin handle + its selector arguments.
    pub refresh_handle: Option<String>,
    /// Operation id returned alongside the begin handle.
    pub refresh_operation_id: Option<String>,
    pub refresh_selectors: Option<Value>,
    pub automation_run_id: Option<String>,
    /// git_hunks producer output for `git_preview`/`git_apply`.
    pub preview_input_id: Option<String>,
    pub hunk_digests: Vec<String>,
    /// Repo-relative file left modified for the git working-tree lane.
    pub dirty_file: Option<String>,
    /// Skill id minted via profile skill file drop (managed skills surface).
    pub skill_id: Option<String>,
    /// First worktree entry id seen in the seeded worktree inventory (cleanup
    /// lane targets `kind:"worktree"` objects).
    pub worktree_id: Option<String>,
    /// Changed path whose run_affected_tests plan maps to covering tests
    /// (minted a request handle at seed time).
    pub test_results_path: Option<String>,
    /// Reversible-truncation handle (`rh_…`) minted by a deliberately fat
    /// search response at seed time; feeds `tracedecay_retrieve`.
    pub retrieve_handle: Option<String>,
    /// Tools whose application authority never mounts under the bench
    /// composition, discovered by seed-time probes — their groups are
    /// omitted and named in `skipped`.
    pub unavailable_tools: std::collections::BTreeSet<String>,
    /// Tools/families whose seed producer failed — the bench prints these so
    /// skipped coverage is visible instead of silent.
    pub skipped: Vec<String>,
    /// Real repo-relative file paths sampled from `tracedecay_files`.
    pub sample_files: Vec<String>,
    /// Work/workflow lifecycle artifacts minted by `seed_work`.
    pub work: Option<WorkSeeds>,
    /// A fresh work-executable binding committed this run; route resolution
    /// reads bindings at composition open, so the caller must reopen the
    /// harness once (then rebuild context) before the admit lane can pass.
    pub needs_reopen_for_provider: bool,
    /// Second bench branch pinned at HEAD~1 (branch_diff base ref; the tool
    /// resolves branch names, not raw oids).
    pub base_branch: Option<String>,
    /// Native-integration journey artifacts minted at seed (inventory →
    /// stack_snapshot → preflight → approve). Absent = family skipped.
    pub native: Option<NativeSeeds>,
}

/// Minted once per run: the stack_snapshot body that seals, plus the live
/// transaction the status lane reads.
#[derive(Clone, Default)]
pub struct NativeSeeds {
    /// Validated `stack_snapshot` request body — the groups replay it to mint
    /// fresh per-iteration transactions.
    pub snapshot_body: Value,
    /// transaction_id of the seed-minted (approved) transaction.
    pub transaction_id: String,
}

/// Artifacts of one real disposable Work lifecycle plus one workflow
/// definition/run, minted once so the ~45 work_* and workflow_* tools measure
/// against real graph state. Field absence means that leg failed and only its
/// dependent groups skip.
#[derive(Default)]
pub struct WorkSeeds {
    pub selection: Value,
    pub task_id: String,
    pub run_id: String,
    pub attempt_id: String,
    /// Started-then-cancelled terminal attempt identity {task,run,attempt}.
    pub attempt_identity: Value,
    /// Second terminal attempt for duplicate-adjudication reads/effects.
    pub dup_attempt_identity: Option<Value>,
    /// verified_graph_version from the first generate_proposal.
    pub initial_version: Value,
    /// accepted graph_version integer (base for admit_execution).
    pub accepted_gv: i64,
    /// Current verified_version from a post-lifecycle work_views.
    pub current_version: Value,
    /// WorkExecutionSnapshot from admit_execution (start_attempt input).
    pub execution_snapshot: Value,
    /// Head commit id used by the seed attempts.
    pub commit: String,
    /// workflow_definition validated with environment-repaired pins.
    pub definition: Value,
    pub definition_id: String,
    pub actor_id: String,
    /// worktree_id from the start_run authority (handoff scope component).
    pub worktree_id: Option<String>,
    /// A live workflow run (id + current sequence) for run-control effects.
    pub wf_run_id: Option<String>,
    /// Projection generation pair for duplicate-adjudication evidence.
    pub work_generation: Option<Value>,
    pub topology_generation: Option<Value>,
}

/// Sampled data drawn from a freshly indexed graph plus seeded entity state.
/// Built once per repo.
pub struct QueryContext {
    pub function_ids: Vec<String>,
    pub struct_ids: Vec<String>,
    pub any_ids: Vec<String>,
    pub function_qnames: Vec<String>,
    pub dir_prefixes: Vec<String>,
    /// Mounted repo root on disk (file URIs, worktree targets).
    pub project_root: PathBuf,
    /// The flat `tracedecay_files` listing (file objects with `path`), so
    /// seed probes reuse the one successful fetch rather than re-reading it.
    pub files: Vec<Value>,
    /// Coverage-sweep seeds, populated by `coverage::seed_all` when the
    /// `large_repos` root wires it in; the standalone `queries` bench leaves
    /// it defaulted.
    pub seeds: Seeds,
}

impl QueryContext {
    /// Pick the i-th id with wrap-around. Returns `"missing"` if no samples
    /// exist, the tool handler will report a not-found error, which is still
    /// useful timing data (and the bench label keeps the case obvious).
    pub(crate) fn pick(slice: &[String], i: usize) -> String {
        if slice.is_empty() {
            "missing".to_string()
        } else {
            slice[i % slice.len()].clone()
        }
    }
}

pub async fn build_context(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
) -> Result<QueryContext, String> {
    // A first-index or post-upgrade rebuild publishes after minutes, while the
    // composition mount gates at 20s — the reads below answer typed
    // warming/unavailable states until the generation serves. Poll the same
    // mounted scheduler until it does (bounded, then the error propagates).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(240);
    loop {
        match try_build_context(harness, project_root).await {
            Ok(ctx) => return Ok(ctx),
            Err(e) if std::time::Instant::now() < deadline => {
                eprintln!("[bench] context not ready ({e}); waiting for index...");
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

async fn try_build_context(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
) -> Result<QueryContext, String> {
    let mut function_ids = Vec::new();
    let mut struct_ids = Vec::new();
    let mut any_ids = Vec::new();

    for kind in ["function", "method", "struct", "class", "module"] {
        let payload = call_json_tool(
            harness,
            project_root,
            "tracedecay_largest",
            json!({ "node_kind": kind, "limit": 64, "format": "json" }),
        )
        .await?;
        let ranking = payload
            .get("ranking")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("tracedecay_largest returned no ranking for kind '{kind}'"))?;
        for symbol in ranking {
            let Some(id) = symbol.get("id").and_then(Value::as_str) else {
                continue;
            };
            push_unique(&mut any_ids, id);
            match kind {
                "function" | "method" => push_unique(&mut function_ids, id),
                "struct" | "class" => push_unique(&mut struct_ids, id),
                _ => {}
            }
        }
    }

    function_ids.truncate(64);
    struct_ids.truncate(64);
    any_ids.truncate(64);

    let mut function_qnames = Vec::new();
    for id in &function_ids {
        let payload = call_json_tool(
            harness,
            project_root,
            "tracedecay_node",
            json!({ "node_id": id, "format": "json" }),
        )
        .await?;
        if let Some(qualified_name) = payload.get("qualified_name").and_then(Value::as_str)
            && !qualified_name.is_empty()
        {
            push_unique(&mut function_qnames, qualified_name);
        }
    }

    let files: Vec<Value> = list_repo_files(harness, project_root).await?;

    // Collect first-segment directory prefixes from the sample files (so
    // `path_prefix` queries are valid for *this* repo regardless of layout).
    let mut dir_prefixes: Vec<String> = files
        .iter()
        .filter_map(|file| file.get("path").and_then(Value::as_str))
        .filter_map(|path| {
            path.split_once('/')
                .map(|(directory, _)| directory.to_owned())
        })
        .collect();
    dir_prefixes.sort();
    dir_prefixes.dedup();
    dir_prefixes.truncate(5);

    require_samples("functions or methods", &function_ids)?;
    require_samples("structs or classes", &struct_ids)?;
    require_samples("graph symbols", &any_ids)?;
    require_samples("qualified function names", &function_qnames)?;
    require_samples("indexed directory prefixes", &dir_prefixes)?;

    // Seed producers live in `coverage/` (not compiled into the standalone
    // `queries` bench root): the caller wires `coverage::seed_all` when it
    // wants the sweep ledger.
    Ok(QueryContext {
        function_ids,
        struct_ids,
        any_ids,
        function_qnames,
        dir_prefixes,
        project_root: project_root.to_path_buf(),
        files,
        seeds: Seeds::default(),
    })
}

/// Flat `tracedecay_files` listing as value objects. Large listings come back
/// as a result-handle envelope whose usable entries live in `preview` (a
/// truncated JSON string); the complete file objects are recovered from the
/// prefix so every consumer sees the same shape.
pub(crate) async fn list_repo_files(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
) -> Result<Vec<Value>, String> {
    let file_payload = call_json_tool(
        harness,
        project_root,
        "tracedecay_files",
        json!({ "layout": "flat", "format": "json" }),
    )
    .await?;
    match file_payload.get("files").and_then(Value::as_array) {
        Some(files) => Ok(files.clone()),
        None => {
            let preview = file_payload
                .get("preview")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let re = regex::Regex::new(r#"\{"bytes":\d+,"path":"[^"]+","symbols":\d+\}"#)
                .map_err(|e| e.to_string())?;
            let recovered: Vec<Value> = re
                .find_iter(preview)
                .filter_map(|m| serde_json::from_str::<Value>(m.as_str()).ok())
                .collect();
            if recovered.is_empty() {
                return Err(format!(
                    "tracedecay_files returned no files array: {file_payload}"
                ));
            }
            Ok(recovered)
        }
    }
}

pub(crate) async fn call_json_tool(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    tool_name: &str,
    arguments: Value,
) -> Result<Value, String> {
    let response = harness
        .call_tool(project_root, tool_name, arguments)
        .await
        .map_err(|error| format!("{tool_name} transport failed: {error}"))?;
    json_tool_payload(tool_name, &response)
}

pub(crate) fn json_tool_payload(
    tool_name: &str,
    response: &JsonRpcResponse,
) -> Result<Value, String> {
    if let Some(error) = &response.error {
        return Err(format!("{tool_name} JSON-RPC failed: {error:?}"));
    }
    let result = response
        .result
        .as_ref()
        .ok_or_else(|| format!("{tool_name} returned no JSON-RPC result"))?;
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(format!("{tool_name} returned a tool error: {result}"));
    }
    let text = result
        .pointer("/content/0/text")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{tool_name} returned no text payload"))?;
    serde_json::from_str(text)
        .map_err(|error| format!("{tool_name} returned non-JSON output: {error}; text={text}"))
}

pub(crate) fn push_unique(values: &mut Vec<String>, value: &str) {
    if !values.iter().any(|existing| existing == value) {
        values.push(value.to_owned());
    }
}

fn require_samples(label: &str, values: &[String]) -> Result<(), String> {
    if values.is_empty() {
        Err(format!(
            "production graph sampling returned no {label}; refusing to benchmark not-found paths"
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn five<F: FnMut(usize) -> Query>(mut f: F) -> Vec<Query> {
    (0..5).map(&mut f).collect()
}

pub(crate) fn dir(ctx: &QueryContext, i: usize) -> String {
    if ctx.dir_prefixes.is_empty() {
        "src".to_string()
    } else {
        ctx.dir_prefixes[i % ctx.dir_prefixes.len()].clone()
    }
}

/// Root directory (relative to the project root) where all write-query
/// scratch files live. Kept on a single path so `git stash --include-untracked`
/// at end-of-bench reverts everything in one shot.
pub const SCRATCH_DIR: &str = ".tracedecay-bench-scratch";

pub(crate) fn scratch(name: &str) -> String {
    format!("{SCRATCH_DIR}/{name}")
}

pub fn build_queries(ctx: &QueryContext) -> Vec<ToolGroup> {
    // ── read query inputs ────────────────────────────────────────────────
    let search_terms = ["main", "init", "parse", "error", "config"];
    let context_tasks = [
        "How is configuration loaded at startup?",
        "Where are command-line arguments parsed?",
        "How are errors defined and propagated?",
        "How is logging configured?",
        "How are tests organized?",
    ];
    let kinds_for_largest = ["function", "method", "struct", "class", "module"];
    let file_globs = ["**/*.rs", "**/*.c", "**/*.py", "**/*.js", "**/*.ts"];
    let rank_kinds = ["calls", "uses", "contains", "type_of", "implements"];

    let mut groups: Vec<ToolGroup> = Vec::new();

    groups.push(ToolGroup {
        tool: "tracedecay_search",
        queries: five(|i| {
            Query::read(
                "term",
                "tracedecay_search",
                json!({ "query": search_terms[i], "limit": 20 }),
            )
        }),
    });

    groups.push(ToolGroup {
        tool: "tracedecay_context",
        queries: five(|i| {
            Query::read(
                "task",
                "tracedecay_context",
                json!({ "task": context_tasks[i], "max_nodes": 20 }),
            )
        }),
    });

    groups.push(ToolGroup {
        tool: "tracedecay_callers",
        queries: five(|i| {
            Query::read(
                "by_id",
                "tracedecay_callers",
                json!({ "node_id": QueryContext::pick(&ctx.seeds.code_node_ids, i), "maximum_depth": 3 }),
            )
        }),
    });

    groups.push(ToolGroup {
        tool: "tracedecay_callees",
        queries: five(|i| {
            Query::read(
                "by_id",
                "tracedecay_callees",
                json!({ "node_id": QueryContext::pick(&ctx.seeds.code_node_ids, i), "maximum_depth": 3 }),
            )
        }),
    });

    groups.push(ToolGroup {
        tool: "tracedecay_node",
        queries: five(|i| {
            Query::read(
                "by_id",
                "tracedecay_node",
                json!({ "node_id": QueryContext::pick(&ctx.any_ids, i) }),
            )
        }),
    });

    groups.push(ToolGroup {
        tool: "tracedecay_by_qualified_name",
        queries: five(|i| {
            Query::read(
                "qname",
                "tracedecay_by_qualified_name",
                json!({ "qualified_name": QueryContext::pick(&ctx.function_qnames, i) }),
            )
        }),
    });

    groups.push(ToolGroup {
        tool: "tracedecay_signature",
        queries: five(|i| {
            Query::read(
                "by_id",
                "tracedecay_signature",
                json!({ "node_id": QueryContext::pick(&ctx.seeds.code_node_ids, i) }),
            )
        }),
    });

    groups.push(ToolGroup {
        tool: "tracedecay_impact",
        queries: five(|i| {
            Query::read(
                "by_id",
                "tracedecay_impact",
                json!({ "node_id": QueryContext::pick(&ctx.seeds.code_node_ids, i), "max_depth": 2 }),
            )
        }),
    });

    groups.push(ToolGroup {
        tool: "tracedecay_files",
        queries: five(|i| {
            Query::read(
                "glob",
                "tracedecay_files",
                json!({ "pattern": file_globs[i] }),
            )
        }),
    });

    groups.push(ToolGroup {
        tool: "tracedecay_complexity",
        queries: {
            let mut v = Vec::with_capacity(5);
            v.push(Query::read(
                "all",
                "tracedecay_complexity",
                json!({ "limit": 20 }),
            ));
            for i in 0..4 {
                v.push(Query::read(
                    "scoped",
                    "tracedecay_complexity",
                    json!({ "path": dir(ctx, i), "limit": 20 }),
                ));
            }
            v
        },
    });

    groups.push(ToolGroup {
        tool: "tracedecay_doc_coverage",
        queries: {
            let mut v = vec![Query::read("all", "tracedecay_doc_coverage", json!({}))];
            for i in 0..4 {
                v.push(Query::read(
                    "scoped",
                    "tracedecay_doc_coverage",
                    json!({ "path": dir(ctx, i) }),
                ));
            }
            v
        },
    });

    groups.push(ToolGroup {
        tool: "tracedecay_largest",
        queries: five(|i| {
            Query::read(
                "by_kind",
                "tracedecay_largest",
                json!({ "node_kind": kinds_for_largest[i], "limit": 20 }),
            )
        }),
    });

    groups.push(ToolGroup {
        tool: "tracedecay_hotspots",
        queries: five(|i| {
            Query::read(
                "limit",
                "tracedecay_hotspots",
                json!({ "limit": 10 + (i as u32) * 10 }),
            )
        }),
    });

    groups.push(ToolGroup {
        tool: "tracedecay_god_class",
        queries: five(|i| {
            Query::read(
                "limit",
                "tracedecay_god_class",
                json!({ "limit": 5 + (i as u32) * 5 }),
            )
        }),
    });

    groups.push(ToolGroup {
        tool: "tracedecay_module_api",
        queries: five(|i| {
            Query::read(
                "scoped",
                "tracedecay_module_api",
                json!({ "path": dir(ctx, i) }),
            )
        }),
    });

    groups.push(ToolGroup {
        tool: "tracedecay_derives",
        queries: five(|i| {
            Query::read(
                "by_id",
                "tracedecay_derives",
                json!({ "node_id": QueryContext::pick(&ctx.struct_ids, i) }),
            )
        }),
    });

    groups.push(ToolGroup {
        tool: "tracedecay_dead_code",
        queries: five(|i| {
            Query::read(
                "scoped",
                "tracedecay_dead_code",
                json!({ "path": dir(ctx, i) }),
            )
        }),
    });

    groups.push(ToolGroup {
        tool: "tracedecay_rank",
        queries: five(|i| {
            Query::read(
                "by_kind",
                "tracedecay_rank",
                json!({ "edge_kind": rank_kinds[i], "limit": 20 }),
            )
        }),
    });

    groups.push(ToolGroup {
        tool: "tracedecay_coupling",
        queries: five(|i| {
            Query::read(
                "scoped",
                "tracedecay_coupling",
                json!({ "path": dir(ctx, i), "limit": 20 }),
            )
        }),
    });

    groups.push(ToolGroup {
        tool: "tracedecay_circular",
        queries: five(|i| {
            Query::read(
                "limit",
                "tracedecay_circular",
                json!({ "limit": 5 + (i as u32) * 5 }),
            )
        }),
    });

    // ── write queries ────────────────────────────────────────────────────
    //
    // Each iteration must start from a known file state because the edit
    // primitives reject ambiguous/zero matches. The harness re-writes
    // `scratch_path` from `init_content` before every timed iter
    // (via `iter_batched`).

    groups.push(ToolGroup {
        tool: "tracedecay_str_replace",
        queries: five(|i| {
            let path = scratch(&format!("str_replace_{i}.txt"));
            // Body grows with `i` so the 5 cases sweep small → larger payloads.
            let body = "ALPHA\nBETA\nGAMMA\nDELTA\n".repeat(1 + i * 4);
            let content = format!("{body}TARGET_{i}\nTAIL\n");
            Query::write(
                "vary_size",
                "tracedecay_str_replace",
                json!({
                    "path": path,
                    "old_str": format!("TARGET_{i}"),
                    "new_str": format!("REPLACED_{i}"),
                }),
                path.clone(),
                content,
            )
        }),
    });

    groups.push(ToolGroup {
        tool: "tracedecay_multi_str_replace",
        queries: five(|i| {
            let path = scratch(&format!("multi_{i}.txt"));
            let n = i + 1; // 1..=5 replacements
            let mut content = String::new();
            let mut replacements = Vec::with_capacity(n);
            for k in 0..n {
                let _ = write!(content, "MARK_{k}\nfiller line {k}\n");
                replacements.push([format!("MARK_{k}"), format!("DONE_{k}")]);
            }
            Query::write(
                "n_repls",
                "tracedecay_multi_str_replace",
                json!({ "path": path, "replacements": replacements }),
                path.clone(),
                content,
            )
        }),
    });

    groups.push(ToolGroup {
        tool: "tracedecay_insert_at",
        queries: five(|i| {
            let path = scratch(&format!("insert_{i}.txt"));
            // 5 distinct anchor lines; each iter inserts after the matching line.
            let lines: Vec<String> = (0..5).map(|k| format!("LINE_{k}")).collect();
            let content = format!("{}\n", lines.join("\n"));
            Query::write(
                "by_anchor",
                "tracedecay_insert_at",
                json!({
                    "path": path,
                    "anchor": format!("LINE_{i}"),
                    "content": "// inserted by bench\n",
                    "before": false,
                }),
                path.clone(),
                content,
            )
        }),
    });

    if ast_grep_on_path() {
        groups.push(ToolGroup {
            tool: "tracedecay_ast_grep_search",
            queries: five(|i| {
                Query::read(
                    "search_fn",
                    "tracedecay_ast_grep_search",
                    json!({
                        "pattern": *["fn $NAME($$$ARGS)", "let $X = $Y", "impl $T { $$$B }", "pub fn $NAME($$$A)", "match $E { $$$ARMS }"].iter().nth(i).unwrap_or(&""),
                        "lang": *["rust", "python", "rust", "rust", "python"].iter().nth(i).unwrap_or(&"rust"),
                        "max_results": 20,
                    }),
                )
            }),
        });
    }

    groups
}

/// Mirrors `tracedecay_mcp::ast_grep_available` without depending on
/// internal-module visibility: shells out to `ast-grep --version`. Cached
/// for the lifetime of the bench process.
fn ast_grep_on_path() -> bool {
    use std::sync::OnceLock;
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        std::process::Command::new("ast-grep")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    })
}
