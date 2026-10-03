//! Criterion benchmark: tracedecay MCP tools against large, real-world repos.
//!
//! What it does:
//!  1. Reads `TRACEDECAY_BENCH_REPOS_DIR` (on-disk cache for the cloned repos).
//!     If unset, prints a message and registers zero benchmarks.
//!  2. For each selected repo (see `repos::REPOS`, optionally filtered with
//!     `TRACEDECAY_BENCH_REPOS=name1,name2`), shallow-clones it (`git fetch
//!     --depth 1`) at a constant ref the first time it is encountered.
//!  3. Opens the repos in one isolated production daemon composition. The
//!     daemon performs normal final-schema admission and waits for its
//!     background code-index scheduler to publish a complete generation.
//!  4. Samples that generation through mounted MCP calls to build one query
//!     catalog per repo:
//!     ≥ 5 queries per tool, each holding concrete `node_id` / qualified-name
//!     / file-pattern arguments drawn from real graph state. Write queries
//!     (`str_replace`, `multi_str_replace`, `insert_at`, `ast_grep_rewrite`)
//!     also declare a scratch file that is rewritten before every timed
//!     iteration via `iter_batched`.
//!  5. Runs every (repo × tool × query) combination through criterion with
//!     `sample_size = 10` and `measurement_time = 30s`.
//!  6. When all benches finish, runs `git stash --include-untracked` inside
//!     each prepared repo so mutations made by the write benches are reverted.
//!
//! Environment variables:
//!   TRACEDECAY_BENCH_REPOS_DIR   required, root directory for cloned repos
//!   TRACEDECAY_BENCH_REPOS       optional, comma-separated repo subset
//!   TRACEDECAY_BENCH_SKIP_CLONE  optional, fail rather than clone

#![allow(clippy::too_many_lines)]
mod coverage;
mod queries;
mod repos;

use std::path::PathBuf;
use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use serde_json::{Value, json};
use tokio::runtime::Runtime;

use tracedecay::daemon::ProductionProjectCompositionHarnessV1;

use queries::{
    Query, QueryContext, QueryKind, SCRATCH_DIR, ToolGroup, build_context, build_queries,
};
use repos::{Repo, ensure_cloned, repos_root, restore_repo, selected_repos};

/// Per-repo state we hand to criterion: mounted project + frozen query catalog.
struct RepoBench {
    dir: PathBuf,
    name: &'static str,
    groups: Vec<ToolGroup>,
    ctx: QueryContext,
}

async fn prepare_repo(
    harness: &ProductionProjectCompositionHarnessV1,
    dir: PathBuf,
    repo: Repo,
) -> Result<RepoBench, String> {
    let ctx = build_context(harness, &dir)
        .await
        .map_err(|error| format!("sample {}: {error}", repo.name))?;
    let groups = build_queries(&ctx);
    Ok(RepoBench {
        dir,
        name: repo.name,
        groups,
        ctx,
    })
}

/// Call a tool, honoring the typed `retryable`/`retry_after_millis` signal:
/// a warming catalog declines with `code-graph-unavailable` + `retry: after_delay`
/// until it is seated — the bench measures the served path, so wait for it
/// (bounded) instead of panicking on the transient typed problem.
async fn call_tool_retried(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &std::path::Path,
    tool: &str,
    args: Value,
) -> tracedecay_domain::errors::Result<tracedecay_mcp::jsonrpc::JsonRpcResponse> {
    let mut attempts = 0;
    loop {
        let response = harness.call_tool(project_root, tool, args.clone()).await?;
        let result = response.result.as_ref();
        let problem = result
            .and_then(|r| r.get("structuredContent"))
            .and_then(|s| s.get("problem"))
            .cloned()
            .or_else(|| result.and_then(content_text_problem));
        let retryable = problem
            .as_ref()
            .and_then(|p| p.get("retryable"))
            .and_then(Value::as_bool)
            == Some(true);
        if !retryable || attempts >= 120 {
            return Ok(response);
        }
        attempts += 1;
        let delay_ms = problem
            .as_ref()
            .and_then(|p| p.get("retry_after_millis"))
            .and_then(Value::as_u64)
            .unwrap_or(250)
            .clamp(50, 1000);
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
    }
}

/// Typed `kind:unavailable` + `retryable` on a terminal problem envelope means
/// the application authority never mounted under this composition — a
/// composition-level truth, so the lane is named and skipped rather than
/// measured as an error path or panicked on.
fn unavailable_problem(response: &tracedecay_mcp::jsonrpc::JsonRpcResponse) -> Option<String> {
    let problem = response_problem(response)?;
    let unavailable = problem.get("kind").and_then(Value::as_str) == Some("unavailable")
        && problem.get("retryable").and_then(Value::as_bool) == Some(true);
    unavailable.then(|| problem_code(&problem))
}

/// The response's problem envelope wherever the wire carries one
/// (structuredContent.problem or content[0].text JSON).
fn response_problem(response: &tracedecay_mcp::jsonrpc::JsonRpcResponse) -> Option<Value> {
    response
        .result
        .as_ref()
        .and_then(|r| r.get("structuredContent"))
        .and_then(|s| s.get("problem"))
        .cloned()
        .or_else(|| response.result.as_ref().and_then(content_text_problem))
}

/// Best available short code for a problem envelope (skip-ledger naming).
fn problem_code(problem: &Value) -> String {
    problem
        .get("diagnostic")
        .and_then(|d| d.get("code"))
        .and_then(Value::as_str)
        .or_else(|| problem.get("reason").and_then(Value::as_str))
        .or_else(|| problem.get("kind").and_then(Value::as_str))
        .unwrap_or("application.surface.unavailable")
        .to_owned()
}

/// Some surfaces (e.g. git branch ops) report typed unavailability only in
/// `content[0].text` JSON — `{status:"unavailable", retryable:true, reason}` —
/// with no structuredContent problem envelope. Normalize that into the same
/// retryable-unavailable signal the seeded ledger keys on.
fn content_text_problem(result: &Value) -> Option<Value> {
    let text = result
        .get("content")?
        .as_array()?
        .first()?
        .get("text")?
        .as_str()?;
    let mut parsed: Value = serde_json::from_str(text).ok()?;
    // A nested `problem` envelope is authoritative where present.
    if let Some(problem) = parsed.get("problem") {
        return Some(problem.clone());
    }
    let has_signal = parsed.get("kind").is_some()
        || parsed.get("status").is_some()
        || parsed.get("code").is_some();
    if !has_signal {
        return None;
    }
    let unavailable = parsed.get("status").and_then(Value::as_str) == Some("unavailable")
        || parsed.get("kind").and_then(Value::as_str) == Some("unavailable");
    if unavailable && let Value::Object(map) = &mut parsed {
        map.entry("kind")
            .or_insert_with(|| Value::from("unavailable"));
    }
    Some(parsed)
}

fn run_query(
    rt: &Runtime,
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &std::path::Path,
    q: &Query,
) -> Value {
    rt.block_on(async {
        // Preserve the complete wire response so criterion cannot optimize the
        // mounted JSON-RPC dispatch or response rendering away.
        match call_tool_retried(harness, project_root, q.tool, q.args.clone()).await {
            Ok(response) => {
                if response.error.is_some()
                    || response
                        .result
                        .as_ref()
                        .and_then(|result| result.get("isError"))
                        .and_then(Value::as_bool)
                        == Some(true)
                {
                    panic!("{} returned an error response: {response:?}", q.tool);
                }
                match serde_json::to_value(response) {
                    Ok(value) => value,
                    Err(error) => panic!("{} response serialization failed: {error}", q.tool),
                }
            }
            Err(error) => panic!("{} transport failed: {error}", q.tool),
        }
    })
}

// Lease-fence, capacity, and not-yet-cancellable verdicts are the contract's
// transient conflicts here: an in-flight mark_running, a leased attempt whose
// provider is still spawning, a run-control authority mid-admission, or a
// still-settling cancellation resolves them inside a bounded window, and the
// problem carries the AfterRevalidate directive. Prime/cleanup steps retry
// that window; the timed call still fails loudly.
pub(crate) const TRANSIENT_STEP_CODES: &[&str] = &[
    "fence-conflict",
    "capacity-exhausted",
    "not-cancellable",
    "authority-conflict",
    "live-holder",
    "deadline_exceeded",
];

fn call_step_transient(
    rt: &Runtime,
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &std::path::Path,
    tool: &str,
    args: Value,
) -> Result<Value, String> {
    for attempt in 0..240 {
        match rt.block_on(queries::call_json_tool(
            harness,
            project_root,
            tool,
            args.clone(),
        )) {
            Err(error)
                if attempt < 239
                    && TRANSIENT_STEP_CODES.iter().any(|code| error.contains(code)) =>
            {
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            result => return result,
        }
    }
    unreachable!()
}

/// Effect-lane setup: run the prime chain untimed, capturing `{{token}}`
/// values from each step's payload, then return the timed call's substituted
/// arguments. The result is fallible so the group warm-up can degrade an
/// unmountable prime authority into a named skip; measured iterations unwrap
/// into the same panic as before.
fn run_primes(
    rt: &Runtime,
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &std::path::Path,
    ctx: &QueryContext,
    q: &Query,
    prime: crate::queries::PrimeFn,
    iteration: &mut u64,
) -> Result<(Value, std::collections::HashMap<String, Value>), String> {
    let iter = *iteration;
    *iteration += 1;
    let mut tokens: std::collections::HashMap<String, Value> = [
        (String::from("iter"), Value::from(iter.to_string())),
        (String::from("now"), Value::from(coverage::now_micros())),
    ]
    .into_iter()
    .collect();
    for step in prime(ctx, iter) {
        for (name, mut value) in step.inject {
            coverage::substitute_tokens(&mut value, &tokens);
            tokens.insert(name, value);
        }
        // Every step observes the state its predecessors committed: rebind
        // `{{now}}` so an observation instant inside a step's args postdates
        // the prior step's publish, not the chain's start.
        tokens.insert(String::from("now"), Value::from(coverage::now_micros()));
        let mut args = step.args;
        coverage::substitute_tokens(&mut args, &tokens);
        let payload = call_step_transient(rt, harness, project_root, step.tool, args)
            .map_err(|error| format!("{} prime step {} failed: {error}", q.tool, step.tool))?;
        capture_tokens(&payload, step.capture, &mut tokens, q)?;
    }
    // The timed call observes state after its primes: verified-snapshot reads
    // filter on the caller's observation instant, so `{{now}}` in timed args
    // must postdate the primes, not share their start instant.
    tokens.insert(String::from("now"), Value::from(coverage::now_micros()));
    let mut args = q.args.clone();
    coverage::substitute_tokens(&mut args, &tokens);
    Ok((args, tokens))
}

fn capture_tokens(
    payload: &Value,
    capture: &[(&'static str, &'static str)],
    tokens: &mut std::collections::HashMap<String, Value>,
    q: &Query,
) -> Result<(), String> {
    for (path, token) in capture {
        match coverage::extract_token(payload, path) {
            Some(value) => {
                tokens.insert((*token).to_owned(), value);
            }
            None => {
                return Err(format!(
                    "{} step could not capture '{path}' (token '{token}'): {payload}",
                    q.tool
                ));
            }
        }
    }
    Ok(())
}

/// Post-timed restore: capture the timed response's minted identities into the
/// token table, then run the cleanup chain untimed (same fail-loud semantics).
fn run_cleanup(
    rt: &Runtime,
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &std::path::Path,
    ctx: &QueryContext,
    q: &Query,
    timed_payload: &Value,
    tokens: &mut std::collections::HashMap<String, Value>,
    cleanup: &crate::queries::EffectCleanup,
    iteration: u64,
) -> Result<(), String> {
    capture_tokens(timed_payload, cleanup.capture, tokens, q)?;
    for step in (cleanup.steps)(ctx, iteration) {
        // Cleanup calls commit real events after the timed call: `{{now}}`
        // must postdate it just as it does across prime steps.
        tokens.insert(String::from("now"), Value::from(coverage::now_micros()));
        for (name, mut value) in step.inject {
            coverage::substitute_tokens(&mut value, &tokens);
            tokens.insert(name, value);
        }
        let mut args = step.args;
        coverage::substitute_tokens(&mut args, tokens);
        let payload = call_step_transient(rt, harness, project_root, step.tool, args)
            .map_err(|error| format!("{} cleanup step {} failed: {error}", q.tool, step.tool))?;
        capture_tokens(&payload, step.capture, tokens, q)?;
    }
    Ok(())
}

fn run_effect_query(
    rt: &Runtime,
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &std::path::Path,
    q: &Query,
    args: Value,
) -> Result<Value, String> {
    rt.block_on(async {
        match call_tool_retried(harness, project_root, q.tool, args).await {
            Ok(response) => {
                queries::json_tool_payload(q.tool, &response).map_err(|e| e.to_string())
            }
            Err(error) => Err(format!("{} transport failed: {error}", q.tool)),
        }
    })
}

/// Re-create `scratch_path` (relative to `project_root`) with `init_content`.
/// Runs before every timed iteration of a write bench so the edit primitive's
/// uniqueness check keeps passing.
fn reset_scratch(project_root: &std::path::Path, scratch_path: &str, init_content: &str) {
    let abs = project_root.join(scratch_path);
    if let Some(parent) = abs.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&abs, init_content) {
        eprintln!(
            "[bench] WARNING: failed to write scratch {}: {e}",
            abs.display()
        );
    }
}

fn bench_all(c: &mut Criterion) {
    let Some(root) = repos_root() else {
        eprintln!(
            "[bench] TRACEDECAY_BENCH_REPOS_DIR is unset, skipping large-repo benchmarks. \
             Set it to a writable directory to enable them."
        );
        return;
    };
    if let Err(e) = std::fs::create_dir_all(&root) {
        eprintln!("[bench] cannot create {}: {e}", root.display());
        return;
    }

    let rt = Runtime::new().expect("create tokio runtime");

    let repos = selected_repos();
    if repos.is_empty() {
        eprintln!("[bench] no repos selected (TRACEDECAY_BENCH_REPOS filter excluded everything)");
        return;
    }

    let mut cloned = Vec::new();
    for repo in &repos {
        match ensure_cloned(&root, *repo) {
            Ok(dir) => cloned.push((*repo, dir)),
            Err(e) => eprintln!("[bench] skipping {}: {e}", repo.name),
        }
    }
    if cloned.is_empty() {
        return;
    }

    // Transcripts must land in the isolation profile's codex home BEFORE the
    // composition opens; session ingest discovers them during open.
    let transcript_repos: Vec<(String, PathBuf)> = cloned
        .iter()
        .map(|(repo, dir)| (repo.name.to_owned(), dir.clone()))
        .collect();
    coverage::seed_transcripts(&root, &transcript_repos);

    eprintln!(
        "[bench] mounting {} repositories in the production composition...",
        cloned.len()
    );
    let project_roots: Vec<PathBuf> = cloned.iter().map(|(_, dir)| dir.clone()).collect();
    // A dirty worktree re-verify or a cold rebuild can outlast the open's
    // internal publish gate; the durable profile state makes each retry pick
    // up where the last attempt left off, so retry bounded instead of dying.
    // A cold profile seats the whole code index inside open; on a large repo
    // that takes many publish-gate retries before the composition admits.
    let open_deadline = std::time::Instant::now() + std::time::Duration::from_secs(900);
    let harness = loop {
        match rt.block_on(ProductionProjectCompositionHarnessV1::open(
            &root,
            project_roots.iter().cloned(),
        )) {
            Ok(harness) => break harness,
            Err(error) => {
                if std::time::Instant::now() >= open_deadline {
                    eprintln!("[bench] production composition failed: {error}");
                    return;
                }
                eprintln!("[bench] composition open not ready ({error}); retrying...");
                std::thread::sleep(std::time::Duration::from_secs(10));
            }
        }
    };

    let mut harness = harness;
    let mut prepared: Vec<RepoBench> = Vec::new();
    for (repo, dir) in cloned {
        match rt.block_on(prepare_repo(&harness, dir.clone(), repo)) {
            Ok(mut rb) => {
                if rb.ctx.seeds.needs_reopen_for_provider {
                    // The provider binding only mounts at composition open;
                    // reopen once so the admit lane measures a routed path.
                    eprintln!(
                        "[bench] reopening composition so the seeded work provider binding mounts..."
                    );
                    rt.block_on(harness.shutdown());
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(240);
                    harness = loop {
                        match rt.block_on(ProductionProjectCompositionHarnessV1::open(
                            &root,
                            project_roots.iter().cloned(),
                        )) {
                            Ok(harness) => break harness,
                            Err(error) => {
                                if std::time::Instant::now() >= deadline {
                                    eprintln!("[bench] composition reopen failed: {error}");
                                    return;
                                }
                                eprintln!(
                                    "[bench] composition reopen not ready ({error}); retrying..."
                                );
                                std::thread::sleep(std::time::Duration::from_secs(10));
                            }
                        }
                    };
                    match rt.block_on(prepare_repo(&harness, dir, repo)) {
                        Ok(rebuilt) => rb = rebuilt,
                        Err(e) => {
                            eprintln!("[bench] skipping {}: {e}", repo.name);
                            continue;
                        }
                    }
                }
                prepared.push(rb);
            }
            Err(e) => eprintln!("[bench] skipping {}: {e}", repo.name),
        }
    }

    for rb in &prepared {
        for group in &rb.groups {
            // Warm every group once up front: lazy authorities mount on first
            // touch, and a group whose authority never mounts degrades to a
            // named runtime skip instead of a timed panic. Effect groups warm
            // with one full prime→timed→cleanup cycle so an unmountable prime
            // authority degrades the same way.
            let mut unavailable: Option<String> = None;
            let mut effect_warm_failed: Option<String> = None;
            if let Some(first) = group.queries.first() {
                match &first.kind {
                    QueryKind::Read => {
                        // Every query in the group is warmed: per-index arg
                        // variation means a later query can fail where the
                        // first succeeds, and a terminal error response at
                        // warm-up skips the group by name instead of panicking
                        // mid-measure. `unavailable` is the common case
                        // (authority never mounted); invalid_request/not_found
                        // covers stale seeds.
                        let mut read_warm_error: Option<String> = None;
                        for q in &group.queries {
                            match rt.block_on(call_tool_retried(
                                &harness,
                                &rb.dir,
                                q.tool,
                                q.args.clone(),
                            )) {
                                Ok(resp) => {
                                    if resp.error.is_some()
                                        || resp
                                            .result
                                            .as_ref()
                                            .and_then(|r| r.get("isError"))
                                            .and_then(Value::as_bool)
                                            == Some(true)
                                    {
                                        let code = response_problem(&resp)
                                            .map(|p| problem_code(&p))
                                            .unwrap_or_else(|| "unknown".to_owned());
                                        read_warm_error =
                                            Some(format!("warm-up error response ({code})"));
                                        break;
                                    }
                                    if unavailable.is_none() {
                                        unavailable = unavailable_problem(&resp);
                                    }
                                }
                                Err(e) => {
                                    read_warm_error =
                                        Some(format!("warm-up transport failed: {e}"));
                                    break;
                                }
                            }
                        }
                        if let Some(e) = read_warm_error {
                            eprintln!("[bench] SKIP {}/{}: {e}", rb.name, group.tool);
                            continue;
                        }
                    }
                    QueryKind::Effect { prime, cleanup } => {
                        let mut iteration = coverage::now_micros() as u64;
                        let warm = run_primes(
                            &rt,
                            &harness,
                            &rb.dir,
                            &rb.ctx,
                            first,
                            *prime,
                            &mut iteration,
                        )
                        .and_then(|(args, mut tokens)| {
                            run_effect_query(&rt, &harness, &rb.dir, first, args).and_then(
                                |payload| {
                                    if let Some(cleanup) = cleanup {
                                        run_cleanup(
                                            &rt,
                                            &harness,
                                            &rb.dir,
                                            &rb.ctx,
                                            first,
                                            &payload,
                                            &mut tokens,
                                            cleanup,
                                            iteration,
                                        )
                                    } else {
                                        Ok(())
                                    }
                                },
                            )
                        });
                        if let Err(e) = warm {
                            effect_warm_failed = Some(e);
                        }
                    }
                    QueryKind::Write { .. } => {}
                }
            }
            if let Some(e) = effect_warm_failed {
                eprintln!(
                    "[bench] SKIP {}/{}: effect warm-up failed: {e}",
                    rb.name, group.tool
                );
                continue;
            }
            if let Some(code) = unavailable {
                eprintln!(
                    "[bench] SKIP {}/{}: authority unavailable ({code})",
                    rb.name, group.tool
                );
                continue;
            }
            let mut g = c.benchmark_group(format!("{}/{}", rb.name, group.tool));
            g.throughput(Throughput::Elements(1));
            for (i, q) in group.queries.iter().enumerate() {
                let id = BenchmarkId::new(q.label, i);
                match &q.kind {
                    QueryKind::Read => {
                        g.bench_with_input(id, q, |b, q| {
                            b.iter(|| run_query(&rt, &harness, &rb.dir, q));
                        });
                    }
                    QueryKind::Write {
                        scratch_path,
                        init_content,
                    } => {
                        let root = rb.dir.clone();
                        let scratch = scratch_path.clone();
                        let init = init_content.clone();
                        g.bench_with_input(id, q, |b, q| {
                            // Persisted attempt receipts bind their
                            // authority; a run-unique iteration base keeps
                            // idempotency keys distinct across runs.
                            let mut iteration = coverage::now_micros() as u64;
                            b.iter_batched(
                                || {
                                    reset_scratch(&root, &scratch, &init);
                                    // Journaled edit contract: dry-run over the
                                    // fresh scratch returns `expected_state`;
                                    // the timed apply carries it plus a fresh key.
                                    let mut dry_args = q.args.clone();
                                    dry_args["dry_run"] = json!(true);
                                    dry_args["format"] = json!("json");
                                    let payload = rt
                                        .block_on(queries::call_json_tool(
                                            &harness, &rb.dir, q.tool, dry_args,
                                        ))
                                        .unwrap_or_else(|error| {
                                            panic!(
                                                "{} dry-run preview failed: {error}",
                                                q.tool
                                            )
                                        });
                                    let expected = crate::coverage::extract_token(
                                        &payload,
                                        "dig:expected_state",
                                    )
                                    .unwrap_or_else(|| {
                                        panic!(
                                            "{} dry-run returned no expected_state",
                                            q.tool
                                        )
                                    });
                                    let mut args = q.args.clone();
                                    args["expected_state"] = expected;
                                    args["idempotency_key"] =
                                        json!(format!("bench.{}.{}.{}", q.tool, i, iteration));
                                    iteration += 1;
                                    args
                                },
                                |args| {
                                    rt.block_on(async {
                                        match call_tool_retried(
                                            &harness, &rb.dir, q.tool, args,
                                        )
                                        .await
                                        {
                                            Ok(response) => {
                                                if response.error.is_some()
                                                    || response
                                                        .result
                                                        .as_ref()
                                                        .and_then(|r| r.get("isError"))
                                                        .and_then(Value::as_bool)
                                                        == Some(true)
                                                {
                                                    panic!(
                                                        "{} returned an error response: {response:?}",
                                                        q.tool
                                                    );
                                                }
                                                match serde_json::to_value(response) {
                                                    Ok(value) => value,
                                                    Err(error) => panic!(
                                                        "{} response serialization failed: {error}",
                                                        q.tool
                                                    ),
                                                }
                                            }
                                            Err(error) => panic!(
                                                "{} transport failed: {error}",
                                                q.tool
                                            ),
                                        }
                                    })
                                },
                                BatchSize::SmallInput,
                            );
                        });
                    }
                    QueryKind::Effect { prime, cleanup } => {
                        g.bench_with_input(id, q, |b, q| {
                            let mut iteration = coverage::now_micros() as u64;
                            b.iter_batched(
                                || {
                                    (
                                        run_primes(
                                            &rt,
                                            &harness,
                                            &rb.dir,
                                            &rb.ctx,
                                            q,
                                            *prime,
                                            &mut iteration,
                                        )
                                        .unwrap_or_else(|e| panic!("{e}")),
                                        iteration,
                                    )
                                },
                                |((args, mut tokens), iter)| {
                                    let payload = run_effect_query(&rt, &harness, &rb.dir, q, args)
                                        .unwrap_or_else(|e| panic!("{e}"));
                                    if let Some(cleanup) = cleanup {
                                        run_cleanup(
                                            &rt,
                                            &harness,
                                            &rb.dir,
                                            &rb.ctx,
                                            q,
                                            &payload,
                                            &mut tokens,
                                            cleanup,
                                            iter,
                                        )
                                        .unwrap_or_else(|e| panic!("{e}"));
                                    }
                                },
                                BatchSize::SmallInput,
                            );
                        });
                    }
                }
            }
            g.finish();
        }
    }

    rt.block_on(harness.shutdown());

    // Revert all scratch-file churn (and any other accidental edits) in each
    // repo we touched. `git stash --include-untracked` puts everything aside;
    // we then drop the stash so the working tree matches the pinned ref again.
    for rb in &prepared {
        eprintln!(
            "[bench] reverting changes in {} (git stash + drop)...",
            rb.name
        );
        let _ = std::fs::remove_dir_all(rb.dir.join(SCRATCH_DIR));
        if let Err(e) = restore_repo(&rb.dir) {
            eprintln!("[bench] WARNING: revert failed for {}: {e}", rb.name);
        }
    }
}

fn criterion_config() -> Criterion {
    Criterion::default()
        .sample_size(10)
        .measurement_time(Duration::from_secs(30))
}

criterion_group! {
    name = benches;
    config = criterion_config();
    targets = bench_all
}
criterion_main!(benches);
