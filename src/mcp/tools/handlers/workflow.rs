//! Compiler / test workflow handlers: `diagnose`, `run_affected_tests`.
//!
//! Bridges raw toolchain output (`cargo check`, `cargo clippy`, `cargo test`)
//! to the code graph, so an agent receives diagnostics and test results
//! already attached to the symbols they affect.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use futures_util::stream::{self, StreamExt};
use serde_json::{Value, json};
use tracedecay_application::clock::now_micros;
use tracedecay_application::{
    CancellationObservation, CancellationSignal, CancellationStage, Deadline, OperationBudgetUsage,
    OperationReceipt, OperationTermination,
};
use tracedecay_domain::{CommitId, UtcMicros};
use url::Url;

use crate::application::operation_stream::{
    OperationEmitter, OperationEventError, operation_event_authority,
};
use crate::diagnose::{Severity, parse_cargo_output};
use crate::diagnostics_publication::CodeIndexPublicationIdentityPortV1;
use crate::diagnostics_query::DiagnosticsQuery;
use crate::diagnostics_store::DiagnosticsStore;
use crate::errors::{Result, TraceDecayError};
use crate::redundancy::{Fingerprint, body_token_window, redundancy_match_score, round4};
use crate::request_identity::{GlobalRequestSurface, mint_global_request_id};
use crate::tracedecay::{TraceDecay, is_test_file};
use crate::types::Node;

use super::super::ToolResult;
use super::super::render;
use super::support::{generic_tool_result, rendered_tool_result, unique_file_paths};

mod affected_test_failure;
mod test_request;
mod test_runner;

use test_request::{MAX_TEST_TIMEOUT_SECS, RunAffectedArgs, TestProfile};
use test_runner::{
    TestRunControl, TestRunFailure, TestRunOutput, cargo_test_args, parse_libtest_output,
    run_cargo_tests,
};

/// Maximum exact test identities admitted to one managed foreground request.
/// Each identity receives a separate Cargo invocation under the request's
/// shared deadline, cancellation, and output budget.
const MAX_TESTS_HARD_CAP: usize = 500;
/// Cap on cached fingerprint rows the near-duplicate lookup pulls per
/// diagnostic. A single diagnose call can resolve many diagnostics, so we
/// bound the candidate window query — a huge fingerprint cache must not be
/// able to blow up a diagnose call.
const MAX_NEAR_DUP_CANDIDATES: usize = 200;

/// Similarity threshold for near-duplicate cross-referencing in `diagnose`.
/// Mirrors the `tracedecay_redundancy` tool default.
const NEAR_DUP_THRESHOLD: f64 = 0.6;

/// Maximum near-duplicate matches attached per diagnostic.
const NEAR_DUP_MAX: usize = 3;

/// Bound concurrent reads while hashing changed files for a managed test run.
/// Large edit sets must not serialize hundreds of awaited `fs::read` calls.
const MANAGED_TEST_DIGEST_READ_CONCURRENCY: usize = 32;

#[derive(Debug, Clone)]
struct TestTarget {
    test_identity: String,
    qualified_name: String,
    node_id: String,
    covers_source_ids: Vec<String>,
}

impl TestTarget {
    fn new(node: &Node) -> Self {
        let prefix = format!("{}::", node.file_path);
        let test_identity = node
            .qualified_name
            .strip_prefix(&prefix)
            .unwrap_or_default()
            .to_owned();
        Self {
            test_identity,
            qualified_name: node.qualified_name.clone(),
            node_id: node.id.clone(),
            covers_source_ids: Vec::new(),
        }
    }

    fn add_source(&mut self, source_id: &str) {
        if !self.covers_source_ids.iter().any(|id| id == source_id) {
            self.covers_source_ids.push(source_id.to_string());
        }
    }

    fn matches_libtest_name(&self, name: &str) -> bool {
        name == self.test_identity
    }
}

fn validate_test_identity(identity: &str) -> std::result::Result<(), String> {
    if identity.trim().is_empty() || identity.trim() != identity {
        return Err("test identity is empty".to_owned());
    }
    if identity.starts_with('-') {
        return Err(format!("test identity `{identity}` cannot begin with `-`"));
    }
    if identity.contains('\0') {
        return Err("test identity contains a NUL byte".to_owned());
    }
    if identity.chars().any(char::is_whitespace) {
        return Err("test identity cannot contain whitespace".to_owned());
    }
    Ok(())
}

fn test_target_key(node: &Node) -> String {
    if node.qualified_name.is_empty() {
        node.id.clone()
    } else {
        node.qualified_name.clone()
    }
}

/// Handles `tracedecay_diagnose`.
pub(super) async fn handle_diagnose(
    cg: &TraceDecay,
    args: Value,
    code_index_identity: Option<&dyn CodeIndexPublicationIdentityPortV1>,
) -> Result<ToolResult> {
    let cargo_output =
        args.get("cargo_output")
            .and_then(|v| v.as_str())
            .ok_or(TraceDecayError::Config {
                message: "missing required parameter: cargo_output".to_string(),
            })?;

    let severity_filter = args
        .get("severity")
        .and_then(|v| v.as_str())
        .unwrap_or("all");
    let include_callers = args
        .get("include_callers")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let max_diagnostics = args
        .get("max_diagnostics")
        .and_then(serde_json::Value::as_u64)
        .map_or(50_usize, |v| v.min(500) as usize);

    let mut diagnostics: Vec<_> = parse_cargo_output(cargo_output)
        .into_iter()
        .filter(|d| match severity_filter {
            "error" => d.severity == Severity::Error,
            "warning" => d.severity == Severity::Warning,
            _ => true,
        })
        .collect();
    let total = diagnostics.len();
    diagnostics.truncate(max_diagnostics);

    let mut items: Vec<Value> = Vec::with_capacity(diagnostics.len());
    let mut touched: HashSet<String> = HashSet::new();
    // Several diagnostics commonly share one enclosing function, so memoize the
    // near-duplicate lookup per node id across the loop — each node's cache read
    // (or fallback scan) then runs at most once per diagnose call.
    let mut near_dup_cache: HashMap<String, Vec<Value>> = HashMap::new();

    for d in &diagnostics {
        touched.insert(d.file.clone());

        let node = cg.node_at_location(&d.file, d.line).await?;
        // Cross-reference the redundancy index: if the enclosing node has a
        // cached fingerprint, surface near-duplicate functions so a
        // diagnostic points at code it may share logic with. Purely reads the
        // cache — never parses/warms files inside diagnose.
        let near_duplicates = match &node {
            Some(n) => {
                if !near_dup_cache.contains_key(&n.id) {
                    let dupes = near_duplicates_for_node(cg, n).await?;
                    near_dup_cache.insert(n.id.clone(), dupes);
                }
                near_dup_cache.get(&n.id).cloned().unwrap_or_default()
            }
            None => Vec::new(),
        };
        for dupe in &near_duplicates {
            if let Some(file) = dupe.get("file").and_then(Value::as_str) {
                touched.insert(file.to_string());
            }
        }
        let callers_json = if include_callers {
            match &node {
                Some(n) => {
                    let callers = cg.get_callers(&n.id, 1).await?;
                    let trimmed: Vec<Value> = callers
                        .into_iter()
                        .take(5)
                        .map(|(caller, _)| {
                            touched.insert(caller.file_path.clone());
                            json!({
                                "node_id": caller.id,
                                "name": caller.name,
                                "kind": caller.kind.as_str(),
                                "file": caller.file_path,
                                "line": caller.start_line,
                            })
                        })
                        .collect();
                    Value::Array(trimmed)
                }
                None => Value::Array(vec![]),
            }
        } else {
            Value::Null
        };

        items.push(json!({
            "severity": severity_string(d.severity),
            "code": d.code,
            "message": d.message,
            "file": d.file,
            "line": d.line,
            "column": d.column,
            "node": node.as_ref().map(|n| json!({
                "node_id": n.id,
                "name": n.name,
                "kind": n.kind.as_str(),
                "qualified_name": n.qualified_name,
                "start_line": n.start_line,
                "end_line": n.end_line,
            })),
            "callers": callers_json,
            "near_duplicates": near_duplicates,
        }));
    }

    // Populate the durable managed-diagnostics store so the LSP Problems
    // projection and every diagnostic read surface see these findings. Before
    // this, nothing in production ever wrote a diagnostic record.
    let publication =
        publish_parsed_compiler_diagnostics(cg, code_index_identity, &diagnostics).await;

    let mapped = items.iter().filter(|i| !i["node"].is_null()).count();
    let body = json!({
        "diagnostics_parsed": total,
        "diagnostics_returned": items.len(),
        "mapped_to_node": mapped,
        "unmapped": items.len() - mapped,
        "truncated": total > items.len(),
        "published": publication,
        "diagnostics": items,
    });
    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &body,
        touched.into_iter().collect(),
        || render::diagnostics_md(&body),
    ))
}

/// Publishes parsed compiler diagnostics into the durable managed-diagnostics
/// store as one clean-generation snapshot.
///
/// This is the production write path for the compiler pillar. Failure to
/// publish never fails the diagnose call — the caller still receives its
/// mapped diagnostics — but the outcome is reported in the response so a
/// silent no-op is observable.
///
/// Identity is resolved from the code-index generation authority, never minted
/// here. That is what lets the LSP feedback projection admit these records:
/// the projection compares a record's `file_occurrence_id` against the
/// saved-edit cycle's impact target and its `generation_id` against the cycle's
/// code-index generation, and both sides now come from the same mint. Without a
/// resolver — a direct, non-daemon server — the honest outcome is to publish
/// nothing under a named reason rather than to guess a repository-relative
/// path, which the projection could only refuse.
async fn publish_parsed_compiler_diagnostics(
    cg: &TraceDecay,
    code_index_identity: Option<&dyn CodeIndexPublicationIdentityPortV1>,
    parsed: &[crate::diagnose::Diagnostic],
) -> Value {
    use tracedecay_domain::ComponentVersion;

    let root = cg.project_root().to_path_buf();
    let Some(analyzer_revision) = ComponentVersion::new(format!(
        "analyzer.tracedecay-diagnose.{}",
        env!("CARGO_PKG_VERSION")
    ))
    .ok() else {
        return json!({ "status": "skipped", "reason": "analyzer-identity-unavailable" });
    };
    let Some(configuration_revision) =
        ComponentVersion::new("configuration.tracedecay-diagnose.v1".to_owned()).ok()
    else {
        return json!({ "status": "skipped", "reason": "configuration-identity-unavailable" });
    };
    let database = cg.dashboard_database_guard();
    let store = DiagnosticsStore::new(database.conn());
    let outcome =
        crate::diagnostics_publication::publish_compiler_diagnostics_through_code_index_v1(
            &root,
            code_index_identity,
            &store,
            parsed,
            analyzer_revision,
            configuration_revision,
        )
        .await;
    compiler_publication_report(&outcome)
}

/// Renders the typed publication outcome for the diagnose response. Every
/// refusal keeps its name so an empty Problems list is explainable.
fn compiler_publication_report(
    outcome: &crate::diagnostics_publication::CompilerDiagnosticPublicationOutcomeV1,
) -> Value {
    use crate::diagnostics_publication::CompilerDiagnosticPublicationOutcomeV1 as Outcome;

    let names = |skips: &[crate::diagnostics_publication::CompilerDiagnosticResolutionSkipV1]| {
        skips.iter().map(ToString::to_string).collect::<Vec<_>>()
    };
    match outcome {
        Outcome::CodeIndexIdentityUnavailable => {
            json!({ "status": "skipped", "reason": "code-index-identity-unavailable" })
        }
        Outcome::CodeIndexGenerationUnavailable => {
            json!({ "status": "skipped", "reason": "code-index-generation-unavailable" })
        }
        Outcome::NoResolvableDiagnostics { unresolved } => json!({
            "status": "skipped",
            "reason": "no-resolvable-diagnostics",
            "unresolved": names(unresolved),
        }),
        Outcome::Published {
            generation,
            report,
            unresolved,
        } => json!({
            "status": "published",
            "generation": generation.as_str(),
            "inserted": report.inserted,
            "cleared": report.cleared,
            "unresolved": names(unresolved),
            "rejected": report
                .rejected
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
        }),
        Outcome::Failed { reason } => json!({ "status": "failed", "reason": reason }),
    }
}

/// Look up cached near-duplicate matches for a diagnostic's enclosing
/// `node`, ranked and capped at [`NEAR_DUP_MAX`].
///
/// Consults the `redundancy_pairs` cache first (a cheap indexed lookup that
/// returns only pairs still fresh against the current fingerprints); if a
/// prior `tracedecay_redundancy` run left fresh pairs for this node they are
/// served directly. Otherwise falls back to the live token-window scan: reads
/// the fingerprint cache, pulls candidates from the ±25 % `body_tokens` window
/// (see [`body_token_window`]) capped at [`MAX_NEAR_DUP_CANDIDATES`], scores
/// with [`redundancy_match_score`] at [`NEAR_DUP_THRESHOLD`], and excludes the
/// node itself. Either path reads only cached data — no files are parsed or
/// warmed inside diagnose.
async fn near_duplicates_for_node(cg: &TraceDecay, node: &Node) -> Result<Vec<Value>> {
    // Fast path: fresh cached duplicate pairs from a prior redundancy run.
    let cached_pairs = cg.db().fresh_redundancy_pairs_for_node(&node.id).await?;
    if !cached_pairs.is_empty() {
        return near_duplicates_from_cached_pairs(cg, node, cached_pairs).await;
    }

    let Some(stored) = cg.db().get_fingerprint(&node.id).await? else {
        return Ok(Vec::new());
    };
    let self_fp: Fingerprint = stored.into();
    let (lo, hi) = body_token_window(self_fp.body_tokens);
    let lo = u32::try_from(lo).unwrap_or(u32::MAX);
    let hi = u32::try_from(hi).unwrap_or(u32::MAX);
    let candidates = cg
        .db()
        .fingerprints_in_token_window(lo, hi, MAX_NEAR_DUP_CANDIDATES)
        .await?;

    let cand_ids: Vec<String> = candidates
        .iter()
        .filter(|row| row.node_id != node.id)
        .map(|row| row.node_id.clone())
        .collect();
    if cand_ids.is_empty() {
        return Ok(Vec::new());
    }
    let cand_nodes = cg.db().get_nodes_by_ids(&cand_ids).await?;
    let nodes_by_id: HashMap<&str, &Node> = cand_nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    let mut matches: Vec<NearDupCandidate<'_>> = Vec::new();
    for row in &candidates {
        if row.node_id == node.id {
            continue;
        }
        let Some(cand_node) = nodes_by_id.get(row.node_id.as_str()) else {
            continue;
        };
        let cand_fp: Fingerprint = row.clone().into();
        if let Some(score) = redundancy_match_score(
            &node.name,
            &self_fp,
            &cand_node.name,
            &cand_fp,
            NEAR_DUP_THRESHOLD,
            false,
        ) {
            matches.push(NearDupCandidate {
                ranking_score: score.ranking_score,
                similarity: score.similarity,
                vector_cosine: score.vector_cosine,
                severity: score.severity,
                overlap_kind: score.overlap_kind,
                node: cand_node,
            });
        }
    }

    Ok(rank_and_emit(matches))
}

/// Resolve fresh cached duplicate pairs into the diagnose near-duplicate JSON
/// shape, ranked and capped at [`NEAR_DUP_MAX`].
///
/// The pairs are already freshness-validated by the reader; this only resolves
/// each partner node's metadata and feeds them through [`rank_and_emit`], the
/// same rank-and-render path the live scan uses, so the fast path and the
/// fallback produce identically ordered and shaped output.
async fn near_duplicates_from_cached_pairs(
    cg: &TraceDecay,
    node: &Node,
    pairs: Vec<crate::db::RedundancyPairRow>,
) -> Result<Vec<Value>> {
    let partner_ids: Vec<String> = pairs
        .iter()
        .map(|p| p.partner_of(&node.id).to_string())
        .collect();
    let partner_nodes = cg.db().get_nodes_by_ids(&partner_ids).await?;
    let nodes_by_id: HashMap<&str, &Node> =
        partner_nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    let mut matches: Vec<NearDupCandidate<'_>> = Vec::new();
    for pair in &pairs {
        if let Some(partner) = nodes_by_id.get(pair.partner_of(&node.id)) {
            matches.push(NearDupCandidate {
                ranking_score: pair.ranking_score,
                similarity: pair.similarity,
                vector_cosine: pair.vector_cosine,
                severity: &pair.severity,
                overlap_kind: &pair.overlap_kind,
                node: partner,
            });
        }
    }

    Ok(rank_and_emit(matches))
}

/// One near-duplicate candidate, unified across the cached-pair fast path and
/// the live token-window scan so both rank and emit through one code path. The
/// cached `RedundancyPairRow` and the live `RedundancyMatchScore` both carry
/// every field below.
struct NearDupCandidate<'a> {
    ranking_score: f64,
    similarity: f64,
    vector_cosine: f64,
    severity: &'a str,
    overlap_kind: &'a str,
    node: &'a Node,
}

/// Rank unified near-duplicate candidates by the full canonical key
/// (`ranking_score` desc, `similarity` desc, `vector_cosine` desc, then name,
/// then id — the same total order [`find_redundant_pairs`] applies), cap at
/// [`NEAR_DUP_MAX`], and render the diagnose JSON shape. Shared so the cached
/// fast path and the live scan produce identically ordered, identically shaped
/// output.
fn rank_and_emit(mut candidates: Vec<NearDupCandidate<'_>>) -> Vec<Value> {
    candidates.sort_by(|a, b| {
        b.ranking_score
            .partial_cmp(&a.ranking_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.similarity
                    .partial_cmp(&a.similarity)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                b.vector_cosine
                    .partial_cmp(&a.vector_cosine)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.node.name.cmp(&b.node.name))
            .then_with(|| a.node.id.cmp(&b.node.id))
    });
    candidates
        .into_iter()
        .take(NEAR_DUP_MAX)
        .map(|c| {
            json!({
                "name": c.node.name,
                "file": c.node.file_path,
                "line": c.node.start_line,
                "id": c.node.id,
                "ranking_score": round4(c.ranking_score),
                "severity": c.severity,
                "overlap_kind": c.overlap_kind,
            })
        })
        .collect()
}

fn severity_string(s: Severity) -> &'static str {
    match s {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Note => "note",
        Severity::Help => "help",
    }
}

/// Handles `tracedecay_run_affected_tests`.
pub(super) async fn handle_run_affected_tests(
    cg: &TraceDecay,
    args: Value,
    cancellation: Option<CancellationSignal>,
    code_index_identity: Option<&dyn CodeIndexPublicationIdentityPortV1>,
) -> Result<ToolResult> {
    handle_run_affected_tests_with_runner(
        cg,
        args,
        cancellation,
        code_index_identity,
        run_cargo_tests,
    )
    .await
}

async fn handle_run_affected_tests_with_runner<Runner, RunFuture>(
    cg: &TraceDecay,
    args: Value,
    cancellation: Option<CancellationSignal>,
    code_index_identity: Option<&dyn CodeIndexPublicationIdentityPortV1>,
    runner: Runner,
) -> Result<ToolResult>
where
    Runner: FnOnce(PathBuf, TestProfile, Vec<String>, Duration, TestRunControl) -> RunFuture,
    RunFuture: Future<Output = std::result::Result<TestRunOutput, TestRunFailure>>,
{
    let run_args = match RunAffectedArgs::parse(&args) {
        Ok(run_args) => run_args,
        Err(result) => return Ok(result),
    };
    let project_root = cg.project_root().to_path_buf();

    // The caller's manifest is the authority for the affected-test scope.
    let changed_paths = match resolve_changed_paths(&args, run_args.explicit_paths) {
        Ok(paths) => paths,
        Err(result) => return Ok(result),
    };
    if changed_paths.is_empty() {
        return Ok(empty_result(&args, "no changed files detected"));
    }

    let test_targets = collect_affected_test_targets(cg, &changed_paths).await?;

    if test_targets.is_empty() {
        return Ok(empty_result(
            &args,
            &format!(
                "no tests cover the changed paths ({} file(s))",
                changed_paths.len()
            ),
        ));
    }

    let (selected_targets, test_names, truncated) =
        select_test_targets(test_targets, run_args.max_tests);
    for test_name in &test_names {
        if let Err(message) = validate_test_identity(test_name) {
            return Ok(error_result(
                &args,
                "invalid_test_identity",
                "test_identity",
                &message,
            ));
        }
    }
    let started_at = now_micros();
    let effective_deadline = Deadline::new(UtcMicros(
        started_at.0.saturating_add(
            i64::try_from(run_args.timeout_secs)
                .unwrap_or(i64::MAX)
                .saturating_mul(1_000_000),
        ),
    ))
    .map_err(test_run_contract_error)?;
    let emitter = begin_test_run(
        cg,
        &changed_paths,
        effective_deadline.clone(),
        code_index_identity,
    )
    .await?;

    // 3) Execute each selected libtest identity exactly once. The runner
    // retains one deadline, cancellation control, and output budget across
    // the whole selected set.
    let control = TestRunControl::default();
    let run = runner(
        project_root.clone(),
        run_args.profile,
        test_names.clone(),
        Duration::from_secs(run_args.timeout_secs),
        control.clone(),
    );
    tokio::pin!(run);
    let cancellation = wait_for_test_run_cancellation(emitter.clone(), cancellation);
    tokio::pin!(cancellation);
    let run_result = tokio::select! {
        result = &mut run => result,
        () = &mut cancellation => {
            control.cancel();
            (&mut run).await
        }
    };
    let output = match run_result {
        Ok(output) => output,
        Err(failure) => {
            return affected_test_failure::terminal_failure(
                &emitter,
                &args,
                started_at,
                &effective_deadline,
                run_args.timeout_secs,
                failure,
                &test_names,
                truncated,
                &selected_targets,
            )
            .await;
        }
    };

    let results = parse_libtest_output(&output.stdout);
    if let Some(test_name) = missing_requested_test(&test_names, &results) {
        let any_requested_result = results
            .iter()
            .any(|(observed, _)| test_names.iter().any(|requested| requested == observed));
        let failure = if !any_requested_result && output.exit_code != Some(0) {
            TestRunFailure::Harness {
                exit_code: output.exit_code,
                output_bytes: output.output_bytes,
                partial: Some(output),
            }
        } else {
            TestRunFailure::NoMatch {
                test_identity: test_name.to_owned(),
                output_bytes: output.output_bytes,
                partial: Some(output),
            }
        };
        return affected_test_failure::terminal_failure(
            &emitter,
            &args,
            started_at,
            &effective_deadline,
            run_args.timeout_secs,
            failure,
            &test_names,
            truncated,
            &selected_targets,
        )
        .await;
    }
    emit_observed_test_results(&emitter, &results, test_names.len()).await?;
    let receipt = finish_test_run(
        &emitter,
        started_at,
        &effective_deadline,
        OperationTermination::Completed,
        output.output_bytes,
    )
    .await?;

    let touched_files: Vec<String> = unique_file_paths(changed_paths.iter().map(String::as_str));
    let body = run_affected_tests_body(
        output.exit_code,
        &results,
        &test_names,
        truncated,
        &selected_targets,
        &output.stderr,
        &output.stdout,
        managed_test_terminal(&emitter, &receipt),
    );

    Ok(generic_tool_result(
        Some(cg.project_root()),
        &args,
        &body,
        touched_files,
    ))
}

async fn wait_for_test_run_cancellation(
    mut emitter: OperationEmitter,
    cancellation: Option<CancellationSignal>,
) {
    // CancellationSignal is still a polled atomic (no event wait API on this
    // type without changing application crate callers we do not own). When no
    // signal is attached, wait only on the emitter. Otherwise poll at 50ms —
    // same cancel semantics, ~10x fewer timers than the prior 5ms wakeups.
    let Some(cancellation) = cancellation else {
        emitter.cancelled().await;
        return;
    };
    loop {
        if cancellation.is_cancelled() {
            let _ = emitter.request_managed_test_cancellation().await;
            return;
        }
        tokio::select! {
            () = emitter.cancelled() => return,
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }
}

async fn begin_test_run(
    cg: &TraceDecay,
    changed_paths: &[String],
    deadline: Deadline,
    code_index_identity: Option<&dyn CodeIndexPublicationIdentityPortV1>,
) -> Result<OperationEmitter> {
    let root = cg
        .project_root()
        .canonicalize()
        .map_err(|error| TraceDecayError::Config {
            message: format!("managed test-run root is unavailable: {error}"),
        })?;
    let head_commit_id = current_head_commit_id(&root);
    let root_uri = Url::from_directory_path(&root)
        .map_err(|()| TraceDecayError::Config {
            message: "managed test-run root URI is invalid".to_owned(),
        })?
        .to_string();
    let request_id =
        mint_global_request_id(GlobalRequestSurface::ManagedTestRun).map_err(|error| {
            TraceDecayError::Config {
                message: error.to_string(),
            }
        })?;
    let database = cg.dashboard_database_guard();
    let code_generation_id = match code_index_identity {
        Some(identity) => identity
            .resolve(root.clone())
            .await
            .map(|identity| identity.generation_id().clone()),
        None => {
            DiagnosticsQuery::new(database.conn())
                .current_generation()
                .await
                .generation
        }
    };
    let document_content_digests =
        managed_test_document_content_digests(&root, changed_paths).await?;
    operation_event_authority()
        .begin_managed_test_run(
            root_uri,
            request_id,
            head_commit_id,
            code_generation_id,
            document_content_digests,
            deadline,
        )
        .await
        .map_err(test_run_event_error)
}

async fn managed_test_document_content_digests(
    root: &Path,
    changed_paths: &[String],
) -> Result<BTreeMap<String, tracedecay_domain::ContentDigest>> {
    let mut validated = Vec::with_capacity(changed_paths.len());
    for changed_path in changed_paths {
        let relative = Path::new(changed_path);
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(TraceDecayError::Config {
                message: format!("managed test-run path is invalid: {changed_path}"),
            });
        }
        validated.push((changed_path.clone(), root.join(relative)));
    }

    let mut outcomes = stream::iter(validated.into_iter().enumerate())
        .map(|(index, (changed_path, absolute))| async move {
            let outcome = match tokio::fs::read(&absolute).await {
                Ok(bytes) => match Url::from_file_path(&absolute) {
                    Ok(uri) => Ok(Some((
                        uri.to_string(),
                        crate::code_index::intake::content_digest(&bytes),
                    ))),
                    Err(()) => Err(TraceDecayError::Config {
                        message: format!("managed test-run source URI is invalid: {changed_path}"),
                    }),
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(TraceDecayError::Config {
                    message: format!(
                        "managed test-run source is unavailable for {changed_path}: {error}"
                    ),
                }),
            };
            (index, outcome)
        })
        .buffer_unordered(MANAGED_TEST_DIGEST_READ_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

    // Restore input order so the first hard error matches the serial path.
    outcomes.sort_by_key(|(index, _)| *index);
    let mut digests = BTreeMap::new();
    for (_, outcome) in outcomes {
        if let Some((uri, digest)) = outcome? {
            digests.insert(uri, digest);
        }
    }
    Ok(digests)
}

fn current_head_commit_id(root: &Path) -> Option<CommitId> {
    let repository = gix::open(root).ok()?;
    let commit = repository.head_commit().ok()?;
    CommitId::new(commit.id().to_hex().to_string()).ok()
}

async fn emit_observed_test_results(
    emitter: &OperationEmitter,
    results: &[(String, bool)],
    requested_total: usize,
) -> Result<()> {
    for (test, passed) in results {
        emitter
            .test_result(test.clone(), *passed)
            .await
            .map_err(test_run_event_error)?;
    }
    emitter
        .progress(results.len() as u64, Some(requested_total as u64))
        .await
        .map(|_| ())
        .map_err(test_run_event_error)
}

async fn finish_test_run(
    emitter: &OperationEmitter,
    started_at: UtcMicros,
    effective_deadline: &Deadline,
    termination: OperationTermination,
    bytes_consumed: u64,
) -> Result<OperationReceipt> {
    let ended_at = now_micros();
    let elapsed_micros = ended_at.0.saturating_sub(started_at.0) as u64;
    let cancellation = matches!(
        termination,
        OperationTermination::Cancelled | OperationTermination::TimedOut
    )
    .then_some(CancellationObservation {
        stage: CancellationStage::DuringRead,
        observed_at: ended_at,
    });
    let receipt = OperationReceipt {
        started_at,
        ended_at,
        effective_deadline: effective_deadline.clone(),
        cancellation,
        budget: OperationBudgetUsage {
            units_consumed: 1,
            bytes_consumed,
            elapsed_micros,
        },
        termination,
    };
    emitter
        .terminal(receipt.clone())
        .await
        .map_err(test_run_event_error)?;
    Ok(receipt)
}

fn test_run_event_error(error: OperationEventError) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("managed test-run lifecycle failed: {error}"),
    }
}

fn test_run_contract_error(error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("managed test-run contract failed: {error}"),
    }
}

fn resolve_changed_paths(
    args: &Value,
    explicit_paths: Option<Vec<String>>,
) -> std::result::Result<Vec<String>, ToolResult> {
    match explicit_paths {
        Some(paths) => Ok(paths),
        None => Err(error_result(
            args,
            "invalid_request",
            "changed_paths",
            "`changed_paths` is required and must explicitly scope the affected-test run",
        )),
    }
}

async fn collect_affected_test_targets(
    cg: &TraceDecay,
    changed_paths: &[String],
) -> Result<HashMap<String, TestTarget>> {
    // Two paths feed into the test set:
    // a) Indirect coverage: for each changed callable, walk callers and keep
    //    test-shaped ones.
    // b) Direct changes: when a changed path is itself a test file or contains
    //    `#[test]` functions, dispatch those tests directly.
    let mut test_targets = HashMap::new();
    for path in changed_paths {
        let nodes = cg.get_nodes_by_file(path).await?;
        add_direct_test_targets(cg, path, &nodes, &mut test_targets).await?;
        add_indirect_test_targets(cg, &nodes, &mut test_targets).await?;
    }
    Ok(test_targets)
}

async fn add_direct_test_targets(
    cg: &TraceDecay,
    path: &str,
    nodes: &[Node],
    test_targets: &mut HashMap<String, TestTarget>,
) -> Result<()> {
    let path_is_test_file = is_test_file(path);
    if !path_is_test_file && nodes.is_empty() {
        return Ok(());
    }

    let candidate_ids: Vec<String> = nodes
        .iter()
        .filter(|n| is_callable(n))
        .map(|n| n.id.clone())
        .collect();
    let test_annotated_in_file = cg.get_test_annotated_node_ids(&candidate_ids).await?;

    for node in nodes {
        if !is_callable(node) {
            continue;
        }
        if !path_is_test_file && !test_annotated_in_file.contains(&node.id) {
            continue;
        }
        // The test "covers itself" so the per-test `covers_source_ids` field
        // remains useful.
        test_targets
            .entry(test_target_key(node))
            .or_insert_with(|| TestTarget::new(node))
            .add_source(&node.id);
    }

    Ok(())
}

async fn add_indirect_test_targets(
    cg: &TraceDecay,
    nodes: &[Node],
    test_targets: &mut HashMap<String, TestTarget>,
) -> Result<()> {
    for node in nodes {
        if !is_callable(node) {
            continue;
        }

        let callers = cg.get_callers(&node.id, 3).await?;
        let caller_ids: Vec<String> = callers.iter().map(|(n, _)| n.id.clone()).collect();
        let test_annotated = cg.get_test_annotated_node_ids(&caller_ids).await?;

        for (caller, _) in callers {
            if !is_test_file(&caller.file_path) && !test_annotated.contains(&caller.id) {
                continue;
            }
            if !is_callable(&caller) {
                continue;
            }
            test_targets
                .entry(test_target_key(&caller))
                .or_insert_with(|| TestTarget::new(&caller))
                .add_source(&node.id);
        }
    }

    Ok(())
}

fn is_callable(node: &Node) -> bool {
    node.kind.is_callable_kind()
}

fn select_test_targets(
    test_targets: HashMap<String, TestTarget>,
    max_tests: usize,
) -> (Vec<TestTarget>, Vec<String>, bool) {
    let mut selected_targets: Vec<TestTarget> = test_targets.into_values().collect();
    selected_targets.sort_by(|a, b| {
        a.qualified_name
            .cmp(&b.qualified_name)
            .then(a.node_id.cmp(&b.node_id))
    });
    let total_tests = selected_targets.len();
    selected_targets.truncate(max_tests);
    let truncated = total_tests > selected_targets.len();

    let mut test_names: Vec<String> = selected_targets
        .iter()
        .map(|target| target.test_identity.clone())
        .collect();
    test_names.sort();
    test_names.dedup();

    (selected_targets, test_names, truncated)
}

fn missing_requested_test<'a>(
    requested: &'a [String],
    results: &[(String, bool)],
) -> Option<&'a str> {
    requested.iter().find_map(|requested| {
        (!results.iter().any(|(observed, _)| observed == requested)).then_some(requested.as_str())
    })
}

fn run_affected_tests_body(
    exit_code: Option<i32>,
    results: &[(String, bool)],
    test_names: &[String],
    truncated: bool,
    selected_targets: &[TestTarget],
    stderr: &str,
    stdout: &str,
    terminal: Value,
) -> Value {
    let passed = results.iter().filter(|(_, ok)| *ok).count();
    let failed = results.iter().filter(|(_, ok)| !*ok).count();

    json!({
        "exit_code": exit_code,
        "passed": passed,
        "failed": failed,
        "total_observed": results.len(),
        "dispatched_tests": test_names,
        "truncated": truncated,
        "results": results
            .iter()
            .map(|(name, ok)| {
                json!({
                    "test": name,
                    "passed": ok,
                    "covers_source_ids": covered_source_ids(name, selected_targets),
                })
            })
            .collect::<Vec<_>>(),
        "stderr_tail": tail(stderr, 2000),
        "stdout_tail": tail(stdout, 2000),
        "terminal": terminal,
    })
}

fn managed_test_terminal(emitter: &OperationEmitter, receipt: &OperationReceipt) -> Value {
    json!({
        "operation_id": emitter.binding().operation_id().to_string(),
        "result_tool": "tracedecay_test_results",
        "receipt": receipt,
    })
}

fn covered_source_ids(name: &str, selected_targets: &[TestTarget]) -> Vec<String> {
    let mut covers = Vec::new();
    for target in selected_targets {
        if target.matches_libtest_name(name) {
            for source_id in &target.covers_source_ids {
                if !covers.contains(source_id) {
                    covers.push(source_id.clone());
                }
            }
        }
    }
    covers
}

/// Wraps a short status message in a normal `ToolResult`.
fn empty_result(args: &Value, message: &str) -> ToolResult {
    let value = json!({
        "passed": 0, "failed": 0, "results": [], "note": message
    });
    generic_tool_result(None, args, &value, vec![])
}

fn error_result(args: &Value, kind: &str, operation: &str, message: &str) -> ToolResult {
    let value = json!({
        "passed": 0,
        "failed": 0,
        "results": [],
        "error": {
            "kind": kind,
            "operation": operation,
            "message": message,
        }
    });
    generic_tool_result(None, args, &value, vec![])
}

/// Returns the last `n` characters of `s`, trimmed to a char boundary.
fn tail(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut start = s.len() - n;
    while !s.is_char_boundary(start) && start < s.len() {
        start += 1;
    }
    s[start..].to_string()
}

#[cfg(test)]
#[path = "workflow/affected_tests_tests.rs"]
mod affected_tests_tests;
