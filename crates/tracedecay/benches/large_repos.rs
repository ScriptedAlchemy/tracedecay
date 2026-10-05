//! MCP catalog benchmarks through an isolated production composition.
//!
//! Finite audit mode times one direct dispatch per sample, with setup,
//! assertions, and cleanup outside that interval. It preserves catalog gaps
//! and typed failures; dispatch success alone is not functional verification.
//! Criterion mode retains the existing repeated repository benchmarks.
//!
//! Environment variables:
//!   TRACEDECAY_BENCH_REPOS_DIR   required, root directory for cloned repos
//!   TRACEDECAY_BENCH_REPOS       optional, comma-separated repo subset
//!   TRACEDECAY_BENCH_SKIP_CLONE  optional, fail rather than clone
//!   TRACEDECAY_BENCH_AUDIT       bounded finite dispatch/latency audit mode
//!   TRACEDECAY_BENCH_SAMPLES     positive sample count required by audit mode
//!   TRACEDECAY_BENCH_REPORT      explicit JSON report path required by audit mode
//!   TRACEDECAY_BENCH_SMALL_FIXTURE  use the pre-staged checked-in fixture repo

#![allow(clippy::too_many_lines)]
mod coverage;
mod queries;
mod repos;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::runtime::Runtime;

use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay_daemon_service::logging::{StderrTracingDefault, install_stderr_tracing};
use tracedecay_tool_catalog::SurfaceBindingV1;

use queries::{
    Query, QueryContext, QueryKind, SCRATCH_DIR, ToolGroup, build_context, build_queries,
};
use repos::{
    Repo, ensure_cloned, repos_root, restore_repo, selected_repos, small_fixture_enabled,
    small_fixture_repo,
};

/// Per-repo state we hand to criterion: mounted project + frozen query catalog.
struct RepoBench {
    dir: PathBuf,
    name: &'static str,
    groups: Vec<ToolGroup>,
    ctx: QueryContext,
}

#[derive(Clone)]
struct AuditControls {
    samples: usize,
    report: PathBuf,
}

#[derive(Default, Serialize)]
struct AuditReport {
    mode: &'static str,
    profile: String,
    checkout_git_commit: Option<String>,
    source_git_dirty: Option<bool>,
    binary_sha256: Option<String>,
    samples_requested: usize,
    catalog_tools: Vec<String>,
    catalog_surface_bindings: Vec<SurfaceBindingV1>,
    measured_surface: &'static str,
    operations: Vec<AuditOperation>,
    setup_skips: Vec<String>,
    runtime_skips: Vec<String>,
    functional_failures: Vec<String>,
    unrepresented_catalog_entries: Vec<String>,
    unmeasured_catalog_entries: Vec<String>,
    unverified_catalog_entries: Vec<String>,
    unlisted_query_tools: Vec<String>,
    dispatch_success_is_not_functionality: bool,
}

#[derive(Serialize)]
struct AuditOperation {
    repository: String,
    tool: String,
    query: String,
    query_index: usize,
    first_arguments: Option<Value>,
    kind: &'static str,
    samples_target: usize,
    samples: Vec<AuditSample>,
    first_response: Option<Value>,
    functional_samples_verified: usize,
    completion_measurements: Vec<AuditCompletion>,
    retrieval_measurements: Vec<AuditCompletion>,
}

#[derive(Serialize)]
struct AuditCompletion {
    sample: usize,
    elapsed_micros: u128,
    terminal: Option<Value>,
    error: Option<String>,
}

#[derive(Serialize)]
struct AuditSample {
    sample: usize,
    state: &'static str,
    latency_micros: u128,
    server_duration_micros: Option<u64>,
    outcome: AuditOutcome,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum AuditOutcome {
    DispatchSuccess,
    AsyncAdmission {
        run_id: String,
    },
    TypedFailure {
        code: String,
        retryable: bool,
        detail: Value,
    },
    TransportFailure {
        error: String,
    },
    DecodeFailure {
        error: String,
    },
}

fn audit_controls() -> Result<Option<AuditControls>, String> {
    if std::env::var_os("TRACEDECAY_BENCH_AUDIT").is_none() {
        return Ok(None);
    }
    let samples = std::env::var("TRACEDECAY_BENCH_SAMPLES")
        .map_err(|_| "TRACEDECAY_BENCH_SAMPLES is required in audit mode".to_owned())?
        .parse::<usize>()
        .map_err(|_| "TRACEDECAY_BENCH_SAMPLES must be a positive integer".to_owned())?;
    if samples == 0 {
        return Err("TRACEDECAY_BENCH_SAMPLES must be greater than zero".to_owned());
    }
    let report = std::env::var("TRACEDECAY_BENCH_REPORT")
        .map_err(|_| "TRACEDECAY_BENCH_REPORT is required in audit mode".to_owned())?;
    if report.trim().is_empty() {
        return Err("TRACEDECAY_BENCH_REPORT must not be empty".to_owned());
    }
    Ok(Some(AuditControls {
        samples,
        report: PathBuf::from(report),
    }))
}

async fn prepare_repo(
    harness: &ProductionProjectCompositionHarnessV1,
    dir: PathBuf,
    repo: Repo,
) -> Result<RepoBench, String> {
    let mut ctx = build_context(harness, &dir)
        .await
        .map_err(|error| format!("sample {}: {error}", repo.name))?;
    // Coverage seeds/groups live only under this root — `queries.rs` stays
    // coverage-free so the standalone `queries` bench still compiles.
    let seeds = coverage::seed_all(harness, &dir, &ctx.function_qnames, &ctx.files).await;
    ctx = build_context(harness, &dir)
        .await
        .map_err(|error| format!("resample {} after seeding: {error}", repo.name))?;
    ctx.seeds = seeds;
    let mut groups = build_queries(&ctx);
    if small_fixture_enabled() {
        groups.push(ToolGroup {
            tool: "tracedecay_callees",
            queries: vec![Query::prepared_read(
                "fixture_callees",
                "tracedecay_callees",
                json!({"node_id": "{{live_node}}", "maximum_depth": 3}),
                0,
                |_ctx, _iteration| {
                    vec![queries::prime_symbol(
                        "src/report.ts::buildFixtureReport".to_owned(),
                        &[("outcome.value.payload.items.0.node_id", "live_node")],
                    )]
                },
            )],
        });
    }
    groups.extend(coverage::coverage_groups(&ctx));
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
    if let Some(problem) = parsed.get("problem").filter(|problem| !problem.is_null()) {
        return Some(problem.clone());
    }
    let unavailable = parsed.get("status").and_then(Value::as_str) == Some("unavailable")
        || parsed.get("kind").and_then(Value::as_str) == Some("unavailable");
    let diagnostic_problem = parsed.get("kind").and_then(Value::as_str).is_some()
        && parsed
            .pointer("/diagnostic/code")
            .and_then(Value::as_str)
            .is_some();
    if !unavailable && !diagnostic_problem {
        return None;
    }
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
    "claim-generation-stale",
    "code-graph-stale",
];

fn call_step_transient(
    rt: &Runtime,
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &std::path::Path,
    tool: &str,
    args: Value,
) -> Result<Value, String> {
    if tool == "tracedecay_work_cancel_attempt" {
        let status = rt.block_on(coverage::wait_work_attempt(
            harness,
            project_root,
            serde_json::json!({
                "task_id": args["task_id"],
                "run_id": args["run_id"],
                "attempt_id": args["attempt_id"],
            }),
            &["running", "succeeded", "failed", "timed_out", "cancelled"],
        ))?;
        if coverage::dig_str(&status, "state") != Some("running") {
            return Ok(status);
        }
    }
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
            Ok(payload)
                if tool == "tracedecay_code_symbol_search"
                    && payload
                        .pointer("/outcome/value/payload/freshness/state")
                        .and_then(Value::as_str)
                        .is_some_and(|state| {
                            matches!(state, "last_complete_stale" | "possibly_stale")
                        }) =>
            {
                if attempt == 239 {
                    return Err(format!("{tool} setup never obtained a current generation"));
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            Ok(_) if tool == "tracedecay_work_start_attempt" => {
                let expected = match args.get("instructions").and_then(Value::as_str) {
                    Some(coverage::WORK_FAILURE_INSTRUCTIONS) => "failed",
                    Some(coverage::WORK_SOURCE_INSTRUCTIONS) => "succeeded",
                    _ => "running",
                };
                return rt.block_on(coverage::wait_work_attempt(
                    harness,
                    project_root,
                    serde_json::json!({
                        "task_id": args["task_id"],
                        "run_id": args["run_id"],
                        "attempt_id": args["attempt_id"],
                    }),
                    &[expected],
                ));
            }
            Ok(_) if tool == "tracedecay_work_attempt_status" => {
                return rt.block_on(coverage::wait_work_attempt(
                    harness,
                    project_root,
                    args.clone(),
                    &["succeeded", "failed", "timed_out", "cancelled"],
                ));
            }
            result => return result,
        }
    }
    unreachable!()
}

fn verify_dashboard_fixture(
    ctx: &QueryContext,
    root: &std::path::Path,
    payload: &Value,
) -> Result<(), String> {
    let port = payload["port"]
        .as_u64()
        .filter(|port| *port > 0 && *port <= u64::from(u16::MAX))
        .ok_or("dashboard omitted its actual bound port")?;
    let origin = format!("http://127.0.0.1:{port}");
    let launch_url = payload["url"]
        .as_str()
        .ok_or("dashboard omitted its launch URL")?;
    if payload["host"] != "127.0.0.1" || !launch_url.starts_with(&format!("{origin}/?token=")) {
        return Err("dashboard did not bind the fixture loopback listener".into());
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(4)))
        .build()
        .into();
    let anonymous = agent
        .get(&format!("{origin}/api/projects"))
        .call()
        .map_err(|error| error.to_string())?;
    if anonymous.status().as_u16() != 401 {
        return Err("dashboard admitted an unauthenticated fixture request".into());
    }
    let launched = agent
        .get(launch_url)
        .call()
        .map_err(|error| error.to_string())?;
    if launched.status().as_u16() != 303 {
        return Err("dashboard launch did not exchange the credential".into());
    }
    let cookie = launched
        .headers()
        .get("set-cookie")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .ok_or("dashboard launch omitted its session cookie")?;
    let mut response = agent
        .get(&format!("{origin}/api/projects"))
        .header("Cookie", cookie)
        .call()
        .map_err(|error| error.to_string())?;
    if response.status().as_u16() != 200 {
        return Err("authenticated dashboard fixture request failed".into());
    }
    let result: Value = response
        .body_mut()
        .read_json()
        .map_err(|error| error.to_string())?;
    let body = result
        .get("payload")
        .ok_or("dashboard project result omitted its payload")?;
    let root = root.canonicalize().map_err(|error| error.to_string())?;
    let root = root.to_string_lossy();
    if result["domain_state"] != "ready"
        || body["active_project_root"].as_str() != Some(root.as_ref())
        || !body["projects"].as_array().is_some_and(|projects| {
            projects.iter().any(|project| {
                project["project_root"].as_str() == Some(root.as_ref())
                    && project["project_id"].as_str() == ctx.seeds.project_id.as_deref()
                    && project["is_active"] == true
            })
        })
    {
        return Err("dashboard did not serve the actual enrolled fixture project".into());
    }
    Ok(())
}

fn retrieve_complete_response(
    rt: &Runtime,
    harness: &ProductionProjectCompositionHarnessV1,
    root: &std::path::Path,
    response: &Value,
) -> Result<Value, String> {
    if response.get("truncated").and_then(Value::as_bool) != Some(true) {
        return Ok(response.clone());
    }
    let handle = response["handle"]
        .as_str()
        .ok_or("truncated response has no live handle")?;
    let total = response["original_chars"]
        .as_u64()
        .ok_or("truncated response omitted its character count")?;
    let mut offset = 0_u64;
    let mut content = String::new();
    while offset < total {
        let result = rt.block_on(queries::call_json_tool(
            harness,
            root,
            "tracedecay_retrieve",
            json!({"handle": handle, "offset": offset, "max_chars": 4096, "format": "json"}),
        ))?;
        let page: tracedecay_contracts::retrieval::RetrievedPageV1 = serde_json::from_value(result)
            .map_err(|error| format!("response page did not decode: {error}"))?;
        let next = offset
            .checked_add(page.content.chars().count() as u64)
            .ok_or("response page character count overflowed")?;
        if page.handle != handle
            || page.expired
            || page.offset != offset
            || page.total_chars != total
            || page.original_chars != total
            || next <= offset
            || next > total
            || page.has_more != (next < total)
            || page.next_offset != (next < total).then_some(next)
        {
            return Err("response retrieval changed identity, length, or page continuity".into());
        }
        content.push_str(&page.content);
        offset = next;
    }
    serde_json::from_str(&content)
        .map_err(|error| format!("complete retrieved response did not decode: {error}"))
}

fn run_primes(
    rt: &Runtime,
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &std::path::Path,
    ctx: &QueryContext,
    q: &Query,
    prime: crate::queries::PrimeFn,
    iteration: &mut u64,
) -> Result<(Value, std::collections::HashMap<String, Value>, u64), String> {
    let iter = *iteration;
    *iteration += 1;
    let mut tokens: std::collections::HashMap<String, Value> = [
        (String::from("iter"), Value::from(iter.to_string())),
        (String::from("now"), Value::from(coverage::now_micros())),
    ]
    .into_iter()
    .collect();
    if q.tool.starts_with("tracedecay_context_scout_") {
        let address = rt.block_on(coverage::seed_context_scout_address(harness, project_root))?;
        let recent = call_step_transient(
            rt,
            harness,
            project_root,
            "tracedecay_context_scout_recent",
            json!({"address": address, "limit": 1, "format": "json"}),
        )?;
        let work = coverage::extract_token(&recent, "digpath:payload:pending.0.work")
            .ok_or("Scout native seed omitted its queued work")?;
        tokens.insert("scout_seeded_work".to_owned(), work);
        tokens.insert("scout_address".to_owned(), address);
    }
    if q.tool == "tracedecay_github_stack_signal_expand" {
        let (args, evidence) = rt.block_on(coverage::prepare_github_stack_signal(
            harness,
            project_root,
            ctx,
        ))?;
        tokens.insert("native_signal_args".to_owned(), args.clone());
        tokens.insert("native_signal_evidence".to_owned(), evidence);
        return Ok((args, tokens, iter));
    }
    if matches!(
        q.tool,
        "tracedecay_stack_snapshot"
            | "tracedecay_preflight_native_integration"
            | "tracedecay_approve_native_integration"
            | "tracedecay_apply_native_integration"
    ) {
        let advance_source = matches!(
            q.tool,
            "tracedecay_approve_native_integration" | "tracedecay_apply_native_integration"
        );
        let body = rt.block_on(coverage::prepare_native_snapshot(
            harness,
            project_root,
            ctx,
            advance_source,
        ))?;
        tokens.insert("native_snapshot_body".to_owned(), body);
    }
    if q.tool.starts_with("tracedecay_worktree_cleanup_") {
        let claims = rt.block_on(coverage::prepare_worktree_claim(harness, project_root, ctx))?;
        let claims = claims
            .as_object()
            .ok_or_else(|| "cleanup claim producer returned a non-object".to_owned())?;
        for (field, value) in claims {
            tokens.insert(format!("wt_{field}"), value.clone());
        }
    }
    #[cfg(unix)]
    if q.tool == "tracedecay_source_edit_reconcile" {
        let (args, path) = rt.block_on(coverage::prepare_source_reconciliation(
            harness,
            project_root,
            iter,
        ))?;
        tokens.insert("reconcile_file".to_owned(), path);
        return Ok((args, tokens, iter));
    }
    let selection = match &q.kind {
        QueryKind::PreparedRead { selection, .. } => *selection as u64,
        _ => iter,
    };
    for step in prime(ctx, selection) {
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
        let producer_args = args.clone();
        let payload = call_step_transient(rt, harness, project_root, step.tool, args)
            .map_err(|error| format!("{} prime step {} failed: {error}", q.tool, step.tool))?;
        if q.tool == "tracedecay_session_lookup" && step.tool == "tracedecay_lcm_load_session" {
            verify_seeded_read(step.tool, &producer_args, &payload)
                .ok_or("session lookup producer has no literal fixture assertion")??;
        }
        if let Some(verified) =
            coverage::verify_context_scout_fixture(step.tool, &producer_args, &payload)
        {
            verified?;
        }
        capture_tokens(&payload, step.capture, &mut tokens, q)?;
    }
    // The timed call observes state after its primes: verified-snapshot reads
    // filter on the caller's observation instant, so `{{now}}` in timed args
    // must postdate the primes, not share their start instant.
    tokens.insert(String::from("now"), Value::from(coverage::now_micros()));
    let mut args = q.args.clone();
    coverage::substitute_tokens(&mut args, &tokens);
    Ok((args, tokens, iter))
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
    repository: &RepoBench,
    q: &Query,
    timed_payload: &Value,
    tokens: &mut std::collections::HashMap<String, Value>,
    iteration: u64,
) -> Result<(), String> {
    if q.tool.starts_with("tracedecay_context_scout_") {
        let mut args = q.args.clone();
        coverage::substitute_tokens(&mut args, tokens);
        rt.block_on(coverage::finish_context_scout_fixture(
            harness,
            &repository.dir,
            q.tool,
            args,
            timed_payload,
            tokens.get("scout_seeded_work"),
        ))?;
    }
    let QueryKind::Effect {
        cleanup: Some(cleanup),
        ..
    } = &q.kind
    else {
        return Ok(());
    };
    capture_tokens(timed_payload, cleanup.capture, tokens, q)?;
    for step in (cleanup.steps)(&repository.ctx, iteration) {
        // Cleanup calls commit real events after the timed call: `{{now}}`
        // must postdate it just as it does across prime steps.
        tokens.insert(String::from("now"), Value::from(coverage::now_micros()));
        for (name, mut value) in step.inject {
            coverage::substitute_tokens(&mut value, tokens);
            tokens.insert(name, value);
        }
        let mut args = step.args;
        coverage::substitute_tokens(&mut args, tokens);
        let payload = call_step_transient(rt, harness, &repository.dir, step.tool, args)
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
        match harness.call_tool(project_root, q.tool, args).await {
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
fn reset_scratch(
    project_root: &std::path::Path,
    scratch_path: &str,
    init_content: &str,
) -> Result<(), String> {
    let abs = project_root.join(scratch_path);
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create scratch directory: {error}"))?;
    }
    std::fs::write(&abs, init_content)
        .map_err(|error| format!("write scratch {}: {error}", abs.display()))
}

#[tracing::instrument(name = "bench.dispatch", skip_all, fields(tool = tool, sample = sample))]
fn audit_dispatch(
    rt: &Runtime,
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &std::path::Path,
    tool: &str,
    args: Value,
    sample: usize,
) -> (AuditSample, Option<Value>) {
    let started = std::time::Instant::now();
    let mut decoded = None;
    // Audit timing is one direct dispatch per sample. Readiness was established
    // during preparation, and retryable responses remain visible as failures.
    let response = rt.block_on(harness.call_tool(project_root, tool, args));
    let latency_micros = started.elapsed().as_micros();
    let server_duration_micros = response
        .as_ref()
        .ok()
        .and_then(|response| response.result.as_ref())
        .and_then(|result| result.pointer("/_meta/duration_us"))
        .and_then(Value::as_u64);
    let outcome = match response {
        Err(error) => AuditOutcome::TransportFailure {
            error: error.to_string(),
        },
        Ok(response) => {
            let is_error = response.error.is_some()
                || response
                    .result
                    .as_ref()
                    .and_then(|result| result.get("isError"))
                    .and_then(Value::as_bool)
                    == Some(true);
            let problem = response
                .result
                .as_ref()
                .and_then(|result| result.get("structuredContent"))
                .and_then(|structured| structured.get("problem"))
                .cloned()
                .or_else(|| is_error.then(|| response_problem(&response)).flatten())
                .or_else(|| {
                    unavailable_problem(&response)
                        .map(|code| json!({"diagnostic": {"code": code}, "retryable": true}))
                });
            if let Some(problem) = problem {
                decoded = Some(problem.clone());
                AuditOutcome::TypedFailure {
                    code: problem_code(&problem),
                    retryable: problem.get("retryable").and_then(Value::as_bool) == Some(true),
                    detail: problem,
                }
            } else {
                match queries::json_tool_payload(tool, &response) {
                    Ok(payload) => {
                        let problem = payload
                            .get("problem")
                            .filter(|problem| !problem.is_null())
                            .cloned()
                            .or_else(|| {
                                let status = payload.get("status").and_then(Value::as_str);
                                matches!(status, Some("error" | "unavailable" | "refused"))
                                    .then(|| payload.clone())
                            })
                            .or_else(|| {
                                [
                                    "/outcome/value/execution/termination",
                                    "/value/outcome/value/execution/termination",
                                ]
                                .into_iter()
                                .find_map(|path| payload.pointer(path).and_then(Value::as_str))
                                .filter(|termination| *termination != "completed")
                                .map(|termination| {
                                    json!({
                                        "diagnostic": {"code": format!("operation.{termination}")},
                                        "envelope": payload.clone()
                                    })
                                })
                            });
                        decoded = Some(payload);
                        match problem {
                            Some(problem) => AuditOutcome::TypedFailure {
                                code: problem_code(&problem),
                                retryable: problem.get("retryable").and_then(Value::as_bool)
                                    == Some(true),
                                detail: problem,
                            },
                            None => AuditOutcome::DispatchSuccess,
                        }
                    }
                    Err(error) => AuditOutcome::DecodeFailure { error },
                }
            }
        }
    };
    (
        AuditSample {
            sample,
            state: if sample == 0 { "first" } else { "repeat" },
            latency_micros,
            server_duration_micros,
            outcome,
        },
        decoded,
    )
}

fn write_audit_report(path: &std::path::Path, report: &AuditReport) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create report directory: {error}"))?;
    }
    let body =
        serde_json::to_vec_pretty(report).map_err(|error| format!("encode report: {error}"))?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("create report {}: {error}", path.display()))?;
    std::io::Write::write_all(&mut file, &body)
        .map_err(|error| format!("write report {}: {error}", path.display()))
}

fn run_audit(
    rt: &Runtime,
    harness: &ProductionProjectCompositionHarnessV1,
    prepared: &[RepoBench],
    controls: &AuditControls,
) -> AuditReport {
    let mut report = AuditReport {
        mode: "bounded-tool-performance-audit",
        profile: std::env::var("TRACEDECAY_BENCH_PROFILE").unwrap_or_else(|_| "unknown".to_owned()),
        checkout_git_commit: git_identity("rev-parse", "HEAD"),
        source_git_dirty: git_dirty(),
        binary_sha256: std::env::var("TRACEDECAY_BENCH_BINARY_SHA256").ok(),
        samples_requested: controls.samples,
        dispatch_success_is_not_functionality: true,
        ..AuditReport::default()
    };
    report.catalog_tools = match tracedecay_mcp::get_maximal_tool_definitions() {
        Ok(definitions) => definitions
            .into_iter()
            .map(|definition| definition.name)
            .collect(),
        Err(error) => {
            report
                .runtime_skips
                .push(format!("maximal tool catalog unavailable: {error}"));
            Vec::new()
        }
    };
    report.measured_surface = "mcp";
    match tracedecay_contracts::application_catalog_contributions() {
        Ok(contributions) => {
            report.catalog_surface_bindings = contributions
                .into_iter()
                .flat_map(|contribution| contribution.bindings().to_vec())
                .collect();
        }
        Err(error) => report
            .runtime_skips
            .push(format!("application surface catalog unavailable: {error}")),
    }
    let mut represented = BTreeSet::new();
    let mut iteration = coverage::now_micros() as u64;

    for repository in prepared {
        for group in &repository.groups {
            if group.queries.is_empty() {
                report
                    .setup_skips
                    .push(format!("{}/{} has no queries", repository.name, group.tool));
                continue;
            }
            represented.insert(group.tool.to_owned());
            for (query_index, q) in group.queries.iter().enumerate() {
                let kind = match &q.kind {
                    QueryKind::Read | QueryKind::PreparedRead { .. } => "read",
                    QueryKind::Write { .. } => "write",
                    QueryKind::Effect { .. } => "effect",
                };
                let samples_target = if matches!(
                    q.kind,
                    QueryKind::Effect {
                        repeatable: false,
                        ..
                    }
                ) {
                    1
                } else {
                    controls.samples
                };
                let mut operation = AuditOperation {
                    repository: repository.name.to_owned(),
                    tool: q.tool.to_owned(),
                    query: q.label.to_owned(),
                    query_index,
                    first_arguments: None,
                    kind,
                    samples_target,
                    samples: Vec::new(),
                    first_response: None,
                    functional_samples_verified: 0,
                    completion_measurements: Vec::new(),
                    retrieval_measurements: Vec::new(),
                };
                for sample in 0..samples_target {
                    let setup =
                        match &q.kind {
                            QueryKind::Read => Ok((
                                q.args.clone(),
                                std::collections::HashMap::new(),
                                None,
                                iteration,
                            )),
                            QueryKind::Write {
                                scratch_path,
                                init_content,
                                expected_content,
                                ..
                            } => reset_scratch(&repository.dir, scratch_path, init_content)
                                .and_then(|_| {
                                    let mut dry_args = q.args.clone();
                                    dry_args["dry_run"] = json!(true);
                                    dry_args["format"] = json!("json");
                                    match rt.block_on(queries::call_json_tool(
                                        harness,
                                        &repository.dir,
                                        q.tool,
                                        dry_args,
                                    )) {
                                        Ok(payload) => {
                                            coverage::extract_token(&payload, "dig:expected_state")
                                                .map(|expected| {
                                                    let mut args = q.args.clone();
                                                    args["expected_state"] = expected;
                                                    args["idempotency_key"] = json!(format!(
                                                        "audit.{}.{}.{}.{}.{}",
                                                        repository.name,
                                                        q.tool,
                                                        query_index,
                                                        sample,
                                                        iteration
                                                    ));
                                                    (
                                                        args,
                                                        std::collections::HashMap::new(),
                                                        Some(expected_content.clone()),
                                                        iteration,
                                                    )
                                                })
                                                .ok_or_else(|| {
                                                    "dry-run returned no expected_state".to_owned()
                                                })
                                        }
                                        Err(error) => Err(error),
                                    }
                                }),
                            QueryKind::PreparedRead { prime, .. }
                            | QueryKind::Effect { prime, .. } => run_primes(
                                rt,
                                harness,
                                &repository.dir,
                                &repository.ctx,
                                q,
                                *prime,
                                &mut iteration,
                            )
                            .map(|(args, tokens, iter)| (args, tokens, None, iter)),
                        };
                    let (args, mut tokens, expected_content, prepared_iteration) = match setup {
                        Ok(args) => args,
                        Err(error) => {
                            report.setup_skips.push(format!(
                                "{}/{}:{} sample {}: {}",
                                repository.name, q.tool, q.label, sample, error
                            ));
                            continue;
                        }
                    };
                    let first_dispatch = operation.samples.is_empty();
                    if first_dispatch {
                        operation.first_arguments = Some(args.clone());
                    }
                    let verification_args = args.clone();
                    let (mut timed, timed_payload) =
                        audit_dispatch(rt, harness, &repository.dir, q.tool, args, sample);
                    timed.state = if first_dispatch { "first" } else { "repeat" };
                    if first_dispatch {
                        operation.first_response = timed_payload.clone();
                    }
                    if q.tool == "tracedecay_fact_store_curate"
                        && let Some(payload) = timed_payload.as_ref()
                    {
                        match coverage::verify_fact_curate_admission(
                            &repository.ctx,
                            &verification_args,
                            payload,
                        ) {
                            Ok(admission) => {
                                timed.outcome = AuditOutcome::AsyncAdmission {
                                    run_id: admission.run_id.as_str().to_owned(),
                                };
                                let started = std::time::Instant::now();
                                let terminal =
                                    rt.block_on(coverage::follow_fact_curate_completion(
                                        harness,
                                        &repository.dir,
                                        &repository.ctx,
                                        &verification_args,
                                        payload,
                                    ));
                                let elapsed_micros = started.elapsed().as_micros();
                                let (terminal, error) = match terminal {
                                    Ok(terminal) => {
                                        let terminal = serde_json::to_value(terminal);
                                        match terminal {
                                            Ok(terminal) => {
                                                let error = (terminal["run"]["status"]
                                                    != "succeeded")
                                                    .then(|| {
                                                        format!(
                                                            "curator settled as {}",
                                                            terminal["run"]["status"]
                                                        )
                                                    });
                                                (Some(terminal), error)
                                            }
                                            Err(error) => (None, Some(error.to_string())),
                                        }
                                    }
                                    Err(error) => (None, Some(error)),
                                };
                                if let Some(error) = &error {
                                    report.functional_failures.push(format!(
                                        "{}/{} sample {}: {}",
                                        repository.name, q.tool, sample, error
                                    ));
                                } else {
                                    operation.functional_samples_verified += 1;
                                }
                                operation.completion_measurements.push(AuditCompletion {
                                    sample,
                                    elapsed_micros,
                                    terminal,
                                    error,
                                });
                            }
                            Err(error) => report.functional_failures.push(format!(
                                "{}/{} sample {}: {}",
                                repository.name, q.tool, sample, error
                            )),
                        }
                    }
                    let timed_ok = matches!(timed.outcome, AuditOutcome::DispatchSuccess);
                    let admitted_async =
                        matches!(timed.outcome, AuditOutcome::AsyncAdmission { .. });
                    operation.samples.push(timed);
                    if !timed_ok && !admitted_async {
                        report.runtime_skips.push(format!(
                            "{}/{}:{} sample {} did not dispatch successfully",
                            repository.name, q.tool, q.label, sample
                        ));
                    }
                    let verified_payload = if timed_ok {
                        timed_payload.as_ref().and_then(|payload| {
                            if payload.get("truncated").and_then(Value::as_bool) != Some(true) {
                                return Some(payload.clone());
                            }
                            let started = std::time::Instant::now();
                            let hydrated =
                                retrieve_complete_response(rt, harness, &repository.dir, payload);
                            let (terminal, error) = match hydrated {
                                Ok(value) => (Some(value), None),
                                Err(error) => (None, Some(error)),
                            };
                            if let Some(error) = &error {
                                report.functional_failures.push(format!(
                                    "{}/{} sample {} retrieval: {}",
                                    repository.name, q.tool, sample, error
                                ));
                            }
                            operation.retrieval_measurements.push(AuditCompletion {
                                sample,
                                elapsed_micros: started.elapsed().as_micros(),
                                terminal: terminal.clone(),
                                error,
                            });
                            terminal
                        })
                    } else {
                        None
                    };
                    if timed_ok && let Some(payload) = verified_payload.as_ref() {
                        let verified = verify_seeded_read(q.tool, &verification_args, payload)
                            .or_else(|| {
                                (q.tool == "tracedecay_dashboard").then(|| {
                                    let verified = verify_dashboard_fixture(&repository.ctx, &repository.dir, payload);
                                    let stopped = rt.block_on(queries::call_json_tool(harness, &repository.dir,
                                        "tracedecay_dashboard", json!({"action": "stop", "format": "json"})))?;
                                    if stopped["status"] != "stopped" {
                                        return Err("fixture dashboard did not stop its owned listener".into());
                                    }
                                    verified
                                })
                            })
                            .or_else(|| coverage::verify_context_scout_fixture(q.tool, &verification_args, payload))
                            .or_else(|| {
                                (q.tool == "tracedecay_github_stack_signal_expand").then(|| {
                                    coverage::verify_github_stack_signal_fixture(
                                        &verification_args,
                                        payload,
                                        tokens.get("native_signal_evidence"),
                                    )
                                })
                            })
                            .or_else(|| {
                                coverage::verify_native_fixture(
                                    &repository.ctx,
                                    q.tool,
                                    &verification_args,
                                    payload,
                                    tokens.get("native_snapshot_body"),
                                )
                                .map(|verified| verified.and_then(|()| {
                                    if q.tool != "tracedecay_multi_root_scope_set_compare_and_swap" {
                                        return Ok(());
                                    }
                                    let persisted = call_step_transient(
                                        rt, harness, &repository.dir,
                                        "tracedecay_multi_root_scope_set_read",
                                        json!({"scope_set_id": verification_args["scope_set_id"], "format": "json"}),
                                    )?;
                                    let actual = persisted.pointer("/application/outcome/value/payload")
                                        .ok_or("scope-set persistence read omitted its canonical payload")?;
                                    let expected = payload.pointer("/application/outcome/value/payload/scope_set")
                                        .ok_or("scope-set CAS omitted its committed projection")?;
                                    if actual != expected {
                                        return Err("scope-set CAS did not persist the exact committed roots, revision, and digest".into());
                                    }
                                    Ok(())
                                }))
                            })
                            .or_else(|| {
                                coverage::verify_code_fixture_effect(
                                    &repository.dir,
                                    q.tool,
                                    &verification_args,
                                    payload,
                                )
                            })
                            .or_else(|| {
                                coverage::verify_code_fixture_read(
                                    q.tool,
                                    q.label,
                                    &verification_args,
                                    payload,
                                )
                            })
                            .or_else(|| {
                                coverage::verify_session_fixture(
                                    &repository.ctx,
                                    q.tool,
                                    q.label,
                                    &verification_args,
                                    payload,
                                    tokens.get("session_lookup_anchors"),
                                )
                            })
                            .or_else(|| {
                                coverage::verify_memory_fixture(
                                    &repository.ctx,
                                    q.tool,
                                    q.label,
                                    &verification_args,
                                    payload,
                                    &tokens,
                                )
                            })
                            .or_else(|| {
                                coverage::verify_fixture_read(
                                    q.tool,
                                    q.label,
                                    &verification_args,
                                    payload,
                                )
                            })
                            .or_else(|| {
                                coverage::verify_work_fixture(q.tool, &verification_args, payload)
                            })
                            .or_else(|| {
                                coverage::verify_work_read_fixture(
                                    &repository.ctx,
                                    q.tool,
                                    q.label,
                                    &verification_args,
                                    payload,
                                    &tokens,
                                )
                            })
                            .or_else(|| {
                                coverage::verify_feedback_fixture(
                                    &repository.ctx,
                                    q.tool,
                                    &verification_args,
                                    payload,
                                )
                            })
                            .or_else(|| {
                                coverage::verify_git_fixture(
                                    q.tool,
                                    &repository.dir,
                                    &verification_args,
                                    payload,
                                )
                            })
                            .or_else(|| {
                                coverage::configuration_effect_key(q.tool).map(|key| {
                                    let followup = call_step_transient(rt, harness, &repository.dir,
                                        "tracedecay_configuration_get", json!({"key": key, "format": "json"}))?;
                                    coverage::verify_configuration_effect(&repository.ctx, q.tool,
                                        &verification_args, payload, &tokens, &followup)
                                        .ok_or_else(|| "configuration effect omitted its behavioral verifier".to_owned())?
                                })
                            })
                            .or_else(|| {
                                coverage::verify_admin_fixture(
                                    &repository.ctx, &repository.dir, q.tool,
                                    &verification_args, payload,
                                )
                            })
                            .or_else(|| {
                                verify_source_lines(
                                    q.tool,
                                    &repository.dir,
                                    &verification_args,
                                    payload,
                                )
                            })
                            .or_else(|| {
                                verify_source_body(
                                    q.tool,
                                    &repository.dir,
                                    &verification_args,
                                    payload,
                                )
                            });
                        if let Some(verified) = verified {
                            match verified {
                                Ok(()) => operation.functional_samples_verified += 1,
                                Err(error) => report.functional_failures.push(format!(
                                    "{}/{}:{} sample {}: {}",
                                    repository.name, q.tool, q.label, sample, error
                                )),
                            }
                        }
                        if q.tool == "tracedecay_source_edit_reconcile" {
                            let body = payload.pointer("/outcome/value/payload").unwrap_or(payload);
                            let unchanged = tokens
                                .get("reconcile_file")
                                .and_then(Value::as_str)
                                .is_some_and(|path| {
                                    std::fs::read(repository.dir.join(path))
                                        .ok()
                                        .is_some_and(|bytes| bytes == b"before reconciliation\n")
                                });
                            if unchanged
                                && body.get("success") == Some(&json!(true))
                                && body.get("reconciled") == Some(&json!(true))
                                && body.pointer("/effect/reconciliation")
                                    == Some(&json!("reconciled"))
                                && body.pointer("/effect/receipt/outcome")
                                    == Some(&json!("completed"))
                            {
                                operation.functional_samples_verified += 1;
                            } else {
                                report.functional_failures.push(format!(
                                    "{}/{} sample {} did not reconcile the unchanged candidate",
                                    repository.name, q.tool, sample
                                ));
                            }
                        }
                    }
                    if timed_ok
                        && let (QueryKind::Effect { .. }, Some(payload)) = (&q.kind, timed_payload)
                        && let Err(error) = run_cleanup(
                            rt,
                            harness,
                            repository,
                            q,
                            &payload,
                            &mut tokens,
                            prepared_iteration,
                        )
                    {
                        report.runtime_skips.push(format!(
                            "{}/{}:{} sample {} cleanup failed: {}",
                            repository.name, q.tool, q.label, sample, error
                        ));
                    }
                    if timed_ok
                        && let (QueryKind::Write { scratch_path, .. }, Some(expected)) =
                            (&q.kind, expected_content)
                    {
                        let actual = std::fs::read_to_string(repository.dir.join(scratch_path));
                        if actual.as_deref().ok() != Some(expected.as_str()) {
                            report.functional_failures.push(format!(
                                "{}/{}:{} sample {} produced unexpected scratch content",
                                repository.name, q.tool, q.label, sample
                            ));
                        } else {
                            operation.functional_samples_verified += 1;
                        }
                    }
                }
                report.operations.push(operation);
            }
        }
    }
    let catalog = report
        .catalog_tools
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    report.unrepresented_catalog_entries = catalog.difference(&represented).cloned().collect();
    let measured = report
        .operations
        .iter()
        .filter(|operation| !operation.samples.is_empty())
        .map(|operation| operation.tool.clone())
        .collect::<BTreeSet<_>>();
    let verified = report
        .operations
        .iter()
        .filter(|operation| operation.functional_samples_verified > 0)
        .map(|operation| operation.tool.clone())
        .collect::<BTreeSet<_>>();
    report.unmeasured_catalog_entries = catalog.difference(&measured).cloned().collect();
    report.unverified_catalog_entries = catalog.difference(&verified).cloned().collect();
    report.unlisted_query_tools = represented.difference(&catalog).cloned().collect();
    report
}

fn verify_source_body(
    tool: &str,
    root: &std::path::Path,
    args: &Value,
    payload: &Value,
) -> Option<Result<(), String>> {
    if tool != "tracedecay_source_body" {
        return None;
    }
    let payload = payload.pointer("/outcome/value/payload").unwrap_or(payload);
    Some((|| {
        let path = payload
            .get("file")
            .and_then(Value::as_str)
            .ok_or("source_body omitted file")?;
        if !std::path::Path::new(path)
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
        {
            return Err("source_body returned an unsafe file path".to_owned());
        }
        let first = payload
            .get("start_line")
            .and_then(Value::as_u64)
            .and_then(|line| usize::try_from(line).ok())
            .filter(|line| *line > 0)
            .ok_or("source_body omitted valid first line")?;
        let last = payload
            .get("end_line")
            .and_then(Value::as_u64)
            .and_then(|line| usize::try_from(line).ok())
            .filter(|line| *line >= first)
            .ok_or("source_body omitted valid last line")?;
        let source = std::fs::read_to_string(root.join(path)).map_err(|error| error.to_string())?;
        let lines = source.lines().collect::<Vec<_>>();
        let expected = lines
            .get(first - 1..last)
            .ok_or("source_body returned out-of-range lines")?
            .join("\n");
        if expected.is_empty()
            || payload.get("body").and_then(Value::as_str) != Some(expected.as_str())
            || payload.get("node_id") != args.get("node_id")
        {
            return Err(
                "source_body differs from requested symbol identity or exact fixture lines"
                    .to_owned(),
            );
        }
        Ok(())
    })())
}

fn verify_source_lines(
    tool: &str,
    root: &std::path::Path,
    args: &Value,
    payload: &Value,
) -> Option<Result<(), String>> {
    if tool != "tracedecay_source_lines" {
        return None;
    }
    let payload = payload.pointer("/outcome/value/payload").unwrap_or(payload);
    Some((|| {
        let path = payload["file"]
            .as_str()
            .ok_or("source_lines omitted its fixture path")?;
        let path = std::path::Path::new(path);
        if !path
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
        {
            return Err("source_lines returned a non-relative fixture path".to_owned());
        }
        let source = std::fs::read_to_string(root.join(path))
            .map_err(|error| format!("read source_lines fixture: {error}"))?;
        let bound = |key: &str| -> Result<usize, String> {
            args["span"][key]
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| format!("source_lines omitted {key}"))
        };
        let expected = source
            .get(bound("start_byte")?..bound("end_byte")?)
            .ok_or("source_lines requested an invalid UTF-8 span")?;
        if expected.is_empty()
            || payload["body"].as_str() != Some(expected)
            || payload.pointer("/references/0/span") != Some(&args["span"])
        {
            return Err(
                "source_lines body or reference differs from the requested fixture bytes"
                    .to_owned(),
            );
        }
        Ok(())
    })())
}

fn verify_seeded_read(tool: &str, args: &Value, payload: &Value) -> Option<Result<(), String>> {
    let payload = payload.pointer("/outcome/value/payload").unwrap_or(payload);
    let expected = match tool {
        "tracedecay_retrieve" => vec![
            (
                "/content",
                json!(
                    "{\"marker\":\"bench-retrieve-payload-4a71c8\",\"items\":[\"real production response handle\"]}"
                ),
            ),
            ("/expired", json!(false)),
            ("/has_more", json!(false)),
        ],
        "tracedecay_skill_view" => vec![
            ("/status", json!("ok")),
            ("/skill/metadata/id", json!("bench-skill-4a71c8")),
            (
                "/skill/body_markdown",
                json!("Benchmark managed skill body marker."),
            ),
            ("/support_files_included", json!(false)),
        ],
        "tracedecay_automation_run_view" => vec![
            ("/status", json!("ok")),
            ("/run/run_id", json!("bench-run-performance-4a71c8")),
            ("/run/status", json!("succeeded")),
            ("/run/accepted_count", json!(1)),
        ],
        "tracedecay_automation_run_artifact_view" => vec![
            ("/status", json!("ok")),
            ("/run_id", json!("bench-run-performance-4a71c8")),
            ("/payload/marker", json!("bench-automation-artifact-4a71c8")),
            ("/payload/status", json!("captured")),
        ],
        "tracedecay_lcm_load_session" => vec![
            ("/status", json!("ok")),
            ("/provider", json!("codex")),
            ("/session_id", args["session_id"].clone()),
        ],
        "tracedecay_lcm_expand" => vec![
            ("/status", json!("ok")),
            ("/state", json!("available")),
            ("/session_id", args["session_id"].clone()),
            (
                "/expansion/raw_message/message_id",
                args["target"]["message_id"].clone(),
            ),
            ("/expansion/content_range/truncated", json!(false)),
        ],
        "tracedecay_lcm_grep" if args["query"] == "bench" => {
            vec![("/status", json!("ok")), ("/count", json!(2))]
        }
        "tracedecay_lcm_expand_query" if args["prompt"] == "bench" => vec![
            ("/status", json!("ok")),
            ("/context_truncated", json!(false)),
        ],
        "tracedecay_message_search" if args["query"] == "bench" => vec![
            ("/outcome", json!("complete")),
            ("/count", json!(2)),
            ("/refresh_required", json!(false)),
            ("/provider", json!("codex")),
        ],
        _ => return None,
    };
    let transcript_array = match tool {
        "tracedecay_lcm_load_session" => Some(("/messages", "/content")),
        "tracedecay_lcm_grep" => Some(("/hits", "/snippet")),
        "tracedecay_lcm_expand_query" => Some(("/context_blocks", "/content")),
        "tracedecay_message_search" => Some(("/results", "/message/text")),
        _ => None,
    };
    if let Some((path, field)) = transcript_array {
        let contents = payload
            .pointer(path)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|item| item.pointer(field).and_then(Value::as_str))
                    .collect::<Vec<_>>()
            });
        if !contents.as_ref().is_some_and(|contents| {
            contents.len() == 2
                && contents.contains(&Some("bench sweep user message"))
                && contents.contains(&Some("bench sweep assistant reply"))
        }) {
            return Some(Err(format!(
                "{path} omitted or altered the two seeded transcript messages"
            )));
        }
    }
    if tool == "tracedecay_lcm_expand" {
        let expected = match payload
            .pointer("/expansion/raw_message/role")
            .and_then(Value::as_str)
        {
            Some("user") => "bench sweep user message",
            Some("assistant") => "bench sweep assistant reply",
            _ => {
                return Some(Err(
                    "expanded fixture message has an unexpected role".to_owned()
                ));
            }
        };
        if payload
            .pointer("/expansion/content")
            .and_then(Value::as_str)
            != Some(expected)
        {
            return Some(Err("expanded fixture message content changed".to_owned()));
        }
    }
    Some(expected.into_iter().try_for_each(|(path, expected)| {
        if payload.pointer(path) == Some(&expected) {
            Ok(())
        } else {
            Err(format!(
                "{path} expected {expected}, received {:?}",
                payload.pointer(path)
            ))
        }
    }))
}

fn git_identity(command: &str, argument: &str) -> Option<String> {
    std::process::Command::new("git")
        .args([command, argument])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_owned())
}

fn git_dirty() -> Option<bool> {
    std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| !output.stdout.is_empty())
}

fn audit_preflight_failure(controls: Option<&AuditControls>, reason: String) -> bool {
    let Some(controls) = controls else {
        eprintln!("[bench] {reason}");
        return false;
    };
    let mut report = AuditReport {
        mode: "bounded-tool-performance-audit",
        profile: std::env::var("TRACEDECAY_BENCH_PROFILE").unwrap_or_else(|_| "unknown".to_owned()),
        checkout_git_commit: git_identity("rev-parse", "HEAD"),
        source_git_dirty: git_dirty(),
        binary_sha256: std::env::var("TRACEDECAY_BENCH_BINARY_SHA256").ok(),
        samples_requested: controls.samples,
        dispatch_success_is_not_functionality: true,
        ..AuditReport::default()
    };
    report.catalog_tools = match tracedecay_mcp::get_maximal_tool_definitions() {
        Ok(definitions) => definitions
            .into_iter()
            .map(|definition| definition.name)
            .collect(),
        Err(error) => {
            report
                .runtime_skips
                .push(format!("maximal tool catalog unavailable: {error}"));
            Vec::new()
        }
    };
    report.setup_skips.push(reason);
    if let Err(error) = write_audit_report(&controls.report, &report) {
        eprintln!("[bench] audit failure report failed: {error}");
    }
    std::process::exit(2);
}

fn bench_all(c: &mut Criterion) {
    install_stderr_tracing(StderrTracingDefault::Warn);
    let audit = match audit_controls() {
        Ok(value) => value,
        Err(error) => {
            eprintln!("[bench] invalid bounded audit controls: {error}");
            std::process::exit(2);
        }
    };
    let Some(root) = repos_root() else {
        if audit_preflight_failure(
            audit.as_ref(),
            "TRACEDECAY_BENCH_REPOS_DIR is unset".to_owned(),
        ) {
            return;
        }
        return;
    };
    if let Err(e) = std::fs::create_dir_all(&root) {
        if audit_preflight_failure(
            audit.as_ref(),
            format!("cannot create benchmark root {}: {e}", root.display()),
        ) {
            return;
        }
        return;
    }

    let rt = Runtime::new().expect("create tokio runtime");

    let mut preparation_skips = Vec::new();
    let repos = if small_fixture_enabled() {
        match small_fixture_repo(&root) {
            Ok((repo, dir)) => vec![(repo, dir)],
            Err(error) => {
                if audit_preflight_failure(
                    audit.as_ref(),
                    format!("small fixture unavailable: {error}"),
                ) {
                    return;
                }
                return;
            }
        }
    } else {
        selected_repos()
            .into_iter()
            .filter_map(|repo| match ensure_cloned(&root, repo) {
                Ok(dir) => Some((repo, dir)),
                Err(error) => {
                    eprintln!("[bench] skipping {}: {error}", repo.name);
                    preparation_skips.push(format!("{}/clone: {error}", repo.name));
                    None
                }
            })
            .collect()
    };
    if repos.is_empty() {
        if audit_preflight_failure(audit.as_ref(), "no repositories selected".to_owned()) {
            return;
        }
        return;
    }

    // Transcripts must land in the isolation profile's codex home BEFORE the
    // composition opens; session ingest discovers them during open.
    let transcript_repos: Vec<(String, PathBuf)> = repos
        .iter()
        .map(|(repo, dir)| (repo.name.to_owned(), dir.clone()))
        .collect();
    coverage::seed_transcripts(&root, &transcript_repos);

    eprintln!(
        "[bench] mounting {} repositories in the production composition...",
        repos.len()
    );
    let mut project_roots: Vec<PathBuf> = repos.iter().map(|(_, dir)| dir.clone()).collect();
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
                    if audit_preflight_failure(
                        audit.as_ref(),
                        format!("production composition failed: {error}"),
                    ) {
                        return;
                    }
                    return;
                }
                eprintln!("[bench] composition open not ready ({error}); retrying...");
                std::thread::sleep(std::time::Duration::from_secs(10));
            }
        }
    };

    let mut harness = harness;
    let mut prepared: Vec<RepoBench> = Vec::new();
    for (repo, dir) in repos {
        match rt.block_on(prepare_repo(&harness, dir.clone(), repo)) {
            Ok(mut rb) => {
                if rb.ctx.seeds.needs_reopen_for_provider
                    || rb.ctx.seeds.needs_reopen_for_native_worktree
                {
                    for additional in &rb.ctx.seeds.additional_project_roots {
                        if !project_roots.contains(additional) {
                            project_roots.push(additional.clone());
                        }
                    }
                    eprintln!(
                        "[bench] reopening composition to mount seeded provider and worktree routes..."
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
                                    if audit_preflight_failure(
                                        audit.as_ref(),
                                        format!("composition reopen failed: {error}"),
                                    ) {
                                        return;
                                    }
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
                            preparation_skips.push(format!("{}/prepare: {e}", repo.name));
                            continue;
                        }
                    }
                }
                prepared.push(rb);
            }
            Err(e) => {
                eprintln!("[bench] skipping {}: {e}", repo.name);
                preparation_skips.push(format!("{}/prepare: {e}", repo.name));
            }
        }
    }

    if let Some(controls) = audit {
        let mut report = run_audit(&rt, &harness, &prepared, &controls);
        report.setup_skips.extend(preparation_skips.iter().cloned());
        for rb in &prepared {
            report.setup_skips.extend(
                rb.ctx
                    .seeds
                    .skipped
                    .iter()
                    .map(|reason| format!("{}/seed: {reason}", rb.name)),
            );
            report.setup_skips.extend(
                rb.ctx
                    .seeds
                    .unavailable_tools
                    .iter()
                    .map(|tool| format!("{}/seed: {tool} authority unavailable", rb.name)),
            );
        }
        rt.block_on(harness.shutdown());
        for rb in &prepared {
            let scratch = rb.dir.join(SCRATCH_DIR);
            if let Err(error) = std::fs::remove_dir_all(&scratch)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                report.functional_failures.push(format!(
                    "{}/cleanup {}: {error}",
                    rb.name,
                    scratch.display()
                ));
            }
            if let Err(error) = restore_repo(&rb.dir) {
                report
                    .functional_failures
                    .push(format!("{}/restore: {error}", rb.name));
            }
        }
        if let Err(error) = write_audit_report(&controls.report, &report) {
            eprintln!("[bench] bounded audit report failed: {error}");
            std::process::exit(1);
        }
        eprintln!(
            "[bench] bounded audit report written to {} ({} operations, {} setup skips, {} runtime skips)",
            controls.report.display(),
            report.operations.len(),
            report.setup_skips.len(),
            report.runtime_skips.len()
        );
        eprintln!(
            "[bench] catalog coverage: {} tools, {} measured, {} behaviorally verified",
            report.catalog_tools.len(),
            report.catalog_tools.len() - report.unmeasured_catalog_entries.len(),
            report.catalog_tools.len() - report.unverified_catalog_entries.len(),
        );
        if !report.unrepresented_catalog_entries.is_empty()
            || !report.unmeasured_catalog_entries.is_empty()
            || !report.unverified_catalog_entries.is_empty()
            || !report.unlisted_query_tools.is_empty()
            || !report.setup_skips.is_empty()
            || !report.runtime_skips.is_empty()
            || !report.functional_failures.is_empty()
        {
            std::process::exit(if preparation_skips.is_empty() { 1 } else { 2 });
        }
        return;
    }

    for rb in &prepared {
        for group in &rb.groups {
            if group.queries.iter().any(|query| {
                matches!(
                    query.kind,
                    QueryKind::Effect {
                        repeatable: false,
                        ..
                    }
                )
            }) {
                eprintln!(
                    "[bench] {}/{} requires a one-shot fixture; use bounded audit",
                    rb.name, group.tool
                );
                continue;
            }
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
                    QueryKind::PreparedRead { prime, .. } => {
                        let mut iteration = coverage::now_micros() as u64;
                        if let Err(error) = run_primes(
                            &rt,
                            &harness,
                            &rb.dir,
                            &rb.ctx,
                            first,
                            *prime,
                            &mut iteration,
                        )
                        .and_then(|(args, _, _)| {
                            run_effect_query(&rt, &harness, &rb.dir, first, args)
                        }) {
                            effect_warm_failed = Some(error);
                        }
                    }
                    QueryKind::Effect { prime, .. } => {
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
                        .and_then(
                            |(args, mut tokens, prepared_iteration)| {
                                run_effect_query(&rt, &harness, &rb.dir, first, args).and_then(
                                    |payload| {
                                        run_cleanup(
                                            &rt,
                                            &harness,
                                            rb,
                                            first,
                                            &payload,
                                            &mut tokens,
                                            prepared_iteration,
                                        )
                                    },
                                )
                            },
                        );
                        if let Err(e) = warm {
                            effect_warm_failed = Some(e);
                        }
                    }
                    QueryKind::Write { .. } => {}
                }
            }
            if let Some(e) = effect_warm_failed {
                eprintln!(
                    "[bench] SKIP {}/{}: prepared warm-up failed: {e}",
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
                    QueryKind::PreparedRead { prime, .. } => {
                        g.bench_with_input(id, q, |b, q| {
                            let mut iteration = coverage::now_micros() as u64;
                            b.iter_batched(
                                || {
                                    run_primes(
                                        &rt,
                                        &harness,
                                        &rb.dir,
                                        &rb.ctx,
                                        q,
                                        *prime,
                                        &mut iteration,
                                    )
                                    .unwrap_or_else(|error| panic!("{error}"))
                                },
                                |(args, _, _)| {
                                    run_effect_query(&rt, &harness, &rb.dir, q, args)
                                        .unwrap_or_else(|error| panic!("{error}"))
                                },
                                BatchSize::PerIteration,
                            );
                        });
                    }
                    QueryKind::Write {
                        scratch_path,
                        init_content,
                        ..
                    } => {
                        let root = rb.dir.clone();
                        let scratch = scratch_path.clone();
                        let init = init_content.clone();
                        g.bench_with_input(id, q, |b, q| {
                            // Persisted attempt receipts bind their
                            // authority; a run-unique iteration base keeps
                            // idempotency keys distinct across runs.
                            let mut iteration = coverage::now_micros() as u64;
                            // PerIteration, not SmallInput: a batch larger
                            // than one runs every setup before any routine,
                            // so a shared scratch file would be reseeded
                            // ahead of the previous iteration's timed apply.
                            b.iter_batched(
                                || {
                                    reset_scratch(&root, &scratch, &init)
                                        .unwrap_or_else(|error| panic!("scratch setup failed: {error}"));
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
                                BatchSize::PerIteration,
                            );
                        });
                    }
                    QueryKind::Effect { prime, .. } => {
                        g.bench_with_input(id, q, |b, q| {
                            let mut iteration = coverage::now_micros() as u64;
                            // PerIteration: primed state is consumed by the
                            // paired timed call, so a batch must not run
                            // several primes ahead of their routines.
                            b.iter_batched(
                                || {
                                    run_primes(
                                        &rt,
                                        &harness,
                                        &rb.dir,
                                        &rb.ctx,
                                        q,
                                        *prime,
                                        &mut iteration,
                                    )
                                    .unwrap_or_else(|e| panic!("{e}"))
                                },
                                |(args, mut tokens, iter)| {
                                    let payload = run_effect_query(&rt, &harness, &rb.dir, q, args)
                                        .unwrap_or_else(|e| panic!("{e}"));
                                    run_cleanup(&rt, &harness, rb, q, &payload, &mut tokens, iter)
                                        .unwrap_or_else(|e| panic!("{e}"));
                                },
                                BatchSize::PerIteration,
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
