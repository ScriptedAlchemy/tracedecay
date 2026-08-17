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
use tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1;
use tracedecay_domain::{CommitId, UtcMicros};
use tracedecay_domain::{RelationEdgeKindV1, SymbolOccurrenceId};
use url::Url;

use crate::diagnose::{Severity, parse_cargo_output};
use crate::diagnostics_publication::CodeIndexPublicationIdentityPortV1;
use crate::diagnostics_query::DiagnosticsQuery;
use crate::diagnostics_store::DiagnosticsStore;
use crate::errors::{Result, TraceDecayError};
use crate::graph::redundancy_scan::{RedundancyOptions, RedundancyScanV1, redundancy_scan};
use crate::request_identity::{GlobalRequestSurface, mint_global_request_id};
use crate::tracedecay::{TraceDecay, is_test_file};
use tracedecay_usecases::operation_stream::{
    OperationEmitter, OperationEventError, operation_event_authority,
};

use super::super::ToolResult;
use super::super::render;
use super::support::{generic_tool_result, rendered_tool_result, unique_file_paths};

mod affected_test_failure;
mod test_identity;
mod test_request;
mod test_runner;

use test_identity::libtest_identity;
#[cfg(test)]
use test_request::MAX_TEST_TIMEOUT_SECS;
use test_request::{RunAffectedArgs, TestProfile};
#[cfg(test)]
use test_runner::cargo_test_args;
use test_runner::{
    TestRunControl, TestRunFailure, TestRunOutput, parse_libtest_output, run_cargo_tests,
};

/// Maximum exact test identities admitted to one managed foreground request.
/// Each identity receives a separate Cargo invocation under the request's
/// shared deadline, cancellation, and output budget.
const MAX_TESTS_HARD_CAP: usize = 500;
/// Maximum near-duplicate matches attached per diagnostic.
const NEAR_DUP_MAX: usize = 3;

/// Bound the canonical request-scoped redundancy result used to enrich one
/// diagnose response. The scan itself retains its paced comparison budget.
const DIAGNOSE_REDUNDANCY_PAIR_LIMIT: usize = 500;

/// Bound concurrent reads while hashing changed files for a managed test run.
/// Large edit sets must not serialize hundreds of awaited `fs::read` calls.
const MANAGED_TEST_DIGEST_READ_CONCURRENCY: usize = 32;

#[derive(Debug, Clone)]
struct GraphTestSymbol {
    id: String,
    kind: String,
    qualified_name: String,
    file_path: String,
}

#[derive(Debug, Clone)]
struct TestTarget {
    test_identity: String,
    qualified_name: String,
    node_id: String,
    covers_source_ids: Vec<String>,
}

impl TestTarget {
    /// The dispatched identity is the one Cargo's `--exact` filter matches:
    /// the module chain the file contributes to its test binary followed by
    /// the in-file chain the extractor observed. Dropping the file's own
    /// prefix filters every test out while `cargo test` still exits `0`.
    fn new(node: &GraphTestSymbol) -> Self {
        let test_identity =
            libtest_identity(&node.file_path, &node.qualified_name).unwrap_or_default();
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

fn test_target_key(node: &GraphTestSymbol) -> String {
    if node.qualified_name.is_empty() {
        node.id.clone()
    } else {
        node.qualified_name.clone()
    }
}

/// Handles `tracedecay_diagnose`.
pub(super) async fn handle_diagnose(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
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
    // Several diagnostics commonly share one enclosing function. Build one
    // request-scoped index from the canonical redundancy journey on first use,
    // then reuse it for every mapped diagnostic in this response.
    let mut near_duplicates_by_node: Option<HashMap<String, Vec<Value>>> = None;

    for d in &diagnostics {
        touched.insert(d.file.clone());

        let node = diagnostic_symbol_at_location(graph, &d.file, d.line)?;
        let near_duplicates = match &node {
            Some(n) => {
                if near_duplicates_by_node.is_none() {
                    near_duplicates_by_node = Some(diagnose_redundancy_index(cg, graph).await?);
                }
                near_duplicates_by_node
                    .as_ref()
                    .and_then(|index| index.get(n.occurrence.as_str()))
                    .cloned()
                    .unwrap_or_default()
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
                    let callers = graph.callers(
                        std::slice::from_ref(&n.occurrence),
                        &[RelationEdgeKindV1::Calls],
                        5,
                    )?;
                    let trimmed: Vec<Value> = callers
                        .into_iter()
                        .next()
                        .into_iter()
                        .flatten()
                        .take(5)
                        .map(|edge| {
                            diagnostic_symbol_json(&edge.neighbor).map(|caller| {
                                if let Some(file) = caller.get("file").and_then(Value::as_str) {
                                    touched.insert(file.to_owned());
                                }
                                caller
                            })
                        })
                        .collect::<Result<Vec<_>>>()?;
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
            "node": node.as_ref().map(diagnostic_symbol_json).transpose()?,
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

fn diagnostic_symbol_at_location(
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    file: &str,
    one_based_line: u32,
) -> Result<Option<CodeGraphSymbolSummaryV1>> {
    const MAX_FILE_SYMBOLS: usize = 50_000;
    let mut symbols = graph.symbols_in_logical_file(file, MAX_FILE_SYMBOLS + 1)?;
    if symbols.len() > MAX_FILE_SYMBOLS {
        return Err(diagnostic_graph_problem(
            "verified diagnostic location census exceeded its symbol budget",
        ));
    }
    let line = one_based_line.saturating_sub(1);
    let mut matched = Vec::new();
    for symbol in symbols.drain(..) {
        let metadata = symbol.metadata.as_ref().ok_or_else(|| {
            diagnostic_graph_problem("verified diagnostic symbol is missing extraction metadata")
        })?;
        let binding = symbol.binding.as_ref().ok_or_else(|| {
            diagnostic_graph_problem("verified diagnostic symbol is missing its file binding")
        })?;
        let logical_path = binding.logical_path.as_deref().ok_or_else(|| {
            diagnostic_graph_problem("verified diagnostic symbol is missing its logical path")
        })?;
        if logical_path != file || metadata.line_span == 0 {
            continue;
        }
        let Some(end_line) = metadata
            .start_line
            .checked_add(metadata.line_span.saturating_sub(1))
        else {
            return Err(diagnostic_graph_problem(
                "verified diagnostic symbol line span overflowed",
            ));
        };
        if metadata.start_line <= line && line <= end_line {
            matched.push(symbol);
        }
    }
    matched.sort_by(|left, right| {
        let left_metadata = left.metadata.as_ref();
        let right_metadata = right.metadata.as_ref();
        left_metadata
            .map(|metadata| metadata.line_span)
            .cmp(&right_metadata.map(|metadata| metadata.line_span))
            .then_with(|| left.occurrence.cmp(&right.occurrence))
    });
    Ok(matched.into_iter().next())
}

fn diagnostic_symbol_json(symbol: &CodeGraphSymbolSummaryV1) -> Result<Value> {
    let metadata = symbol.metadata.as_ref().ok_or_else(|| {
        diagnostic_graph_problem("verified diagnostic symbol is missing extraction metadata")
    })?;
    let binding = symbol.binding.as_ref().ok_or_else(|| {
        diagnostic_graph_problem("verified diagnostic symbol is missing its file binding")
    })?;
    let file = binding.logical_path.as_deref().ok_or_else(|| {
        diagnostic_graph_problem("verified diagnostic symbol is missing its logical path")
    })?;
    if metadata.line_span == 0 {
        return Err(diagnostic_graph_problem(
            "verified diagnostic symbol has an empty line span",
        ));
    }
    let end_line = metadata
        .start_line
        .checked_add(metadata.line_span - 1)
        .ok_or_else(|| diagnostic_graph_problem("verified diagnostic line span overflowed"))?;
    Ok(json!({
        "node_id": symbol.occurrence.as_str(),
        "name": metadata.simple_name,
        "kind": metadata.kind,
        "qualified_name": metadata.qualified_name,
        "file": file,
        "line": metadata.start_line,
        "start_line": metadata.start_line,
        "end_line": end_line,
    }))
}

fn diagnostic_graph_problem(detail: &str) -> TraceDecayError {
    TraceDecayError::project_route("verified-diagnostic-evidence-unavailable", false, detail)
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
    let store = DiagnosticsStore::new(database.as_ref().clone());
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

/// Runs the maintained redundancy journey once and indexes its already-ranked
/// structural pairs by both endpoint identities for diagnostic enrichment.
async fn diagnose_redundancy_index(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
) -> Result<HashMap<String, Vec<Value>>> {
    let options = RedundancyOptions {
        path_prefix: None,
        min_lines: 8,
        max_pairs: DIAGNOSE_REDUNDANCY_PAIR_LIMIT,
        threshold: 0.6,
        include_naming: false,
        include_generated: false,
    };
    let scan = redundancy_scan(cg, graph, &options).await?;
    Ok(near_duplicate_index(&scan))
}

fn near_duplicate_index(scan: &RedundancyScanV1) -> HashMap<String, Vec<Value>> {
    let mut index: HashMap<String, Vec<Value>> = HashMap::new();
    for pair in &scan.pairs {
        let left = json!({
            "name": pair.b.name,
            "file": pair.b.file,
            "line": pair.b.line,
            "id": pair.b.id,
            "ranking_score": pair.ranking_score,
            "severity": pair.severity,
            "overlap_kind": pair.overlap_kind,
        });
        let right = json!({
            "name": pair.a.name,
            "file": pair.a.file,
            "line": pair.a.line,
            "id": pair.a.id,
            "ranking_score": pair.ranking_score,
            "severity": pair.severity,
            "overlap_kind": pair.overlap_kind,
        });
        let left_matches = index.entry(pair.a.id.clone()).or_default();
        if left_matches.len() < NEAR_DUP_MAX {
            left_matches.push(left);
        }
        let right_matches = index.entry(pair.b.id.clone()).or_default();
        if right_matches.len() < NEAR_DUP_MAX {
            right_matches.push(right);
        }
    }
    index
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
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
    cancellation: Option<CancellationSignal>,
    code_index_identity: Option<&dyn CodeIndexPublicationIdentityPortV1>,
) -> Result<ToolResult> {
    handle_run_affected_tests_with_runner(
        cg,
        graph,
        args,
        cancellation,
        code_index_identity,
        run_cargo_tests,
    )
    .await
}

async fn handle_run_affected_tests_with_runner<Runner, RunFuture>(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
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

    let test_targets = collect_affected_test_targets(graph, &changed_paths)?;

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
            DiagnosticsQuery::new(database.as_ref().clone())
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

fn collect_affected_test_targets(
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    changed_paths: &[String],
) -> Result<HashMap<String, TestTarget>> {
    // Two paths feed into the test set:
    // a) Indirect coverage: for each changed callable, walk callers and keep
    //    test-shaped ones.
    // b) Direct changes: when a changed path is itself a test file or contains
    //    `#[test]` functions, dispatch those tests directly.
    let mut test_targets = HashMap::new();
    let mut annotations_by_file = HashMap::new();
    for path in changed_paths {
        let summaries = affected_test_symbols_in_file(graph, path)?;
        let nodes = graph_test_symbols(&summaries)?;
        let annotated = test_annotations_in_file(graph, path, &mut annotations_by_file)?;
        add_direct_test_targets(path, &nodes, annotated, &mut test_targets);
        add_indirect_test_targets(graph, &nodes, &mut annotations_by_file, &mut test_targets)?;
    }
    Ok(test_targets)
}

fn add_direct_test_targets(
    path: &str,
    nodes: &[GraphTestSymbol],
    test_annotated_in_file: &HashSet<String>,
    test_targets: &mut HashMap<String, TestTarget>,
) {
    let path_is_test_file = is_test_file(path);
    if !path_is_test_file && nodes.is_empty() {
        return;
    }

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
}

fn add_indirect_test_targets(
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    nodes: &[GraphTestSymbol],
    annotations_by_file: &mut HashMap<String, HashSet<String>>,
    test_targets: &mut HashMap<String, TestTarget>,
) -> Result<()> {
    const MAX_IMPACTED_SYMBOLS: usize = 20_000;
    const MAX_RELATIONS_PER_HOP: usize = 20_000;
    for node in nodes {
        if !is_callable(node) {
            continue;
        }
        let occurrence = SymbolOccurrenceId::new(node.id.clone()).map_err(|error| {
            affected_test_graph_problem(&format!(
                "verified affected-test occurrence is invalid: {error}"
            ))
        })?;
        let impact = graph.impact(
            std::slice::from_ref(&occurrence),
            &[RelationEdgeKindV1::Calls],
            3,
            MAX_IMPACTED_SYMBOLS,
            MAX_RELATIONS_PER_HOP,
        )?;
        if !impact.complete {
            return Err(affected_test_graph_problem(
                "verified affected-test caller expansion exceeded its budget",
            ));
        }
        for impacted in impact.impacted {
            if impacted.summary.occurrence == occurrence {
                continue;
            }
            let Some(caller) = graph_test_symbol(&impacted.summary)? else {
                continue;
            };
            if !is_callable(&caller) {
                continue;
            }
            if !is_test_file(&caller.file_path)
                && !test_annotations_in_file(graph, &caller.file_path, annotations_by_file)?
                    .contains(&caller.id)
            {
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

fn affected_test_symbols_in_file(
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    path: &str,
) -> Result<Vec<CodeGraphSymbolSummaryV1>> {
    const MAX_FILE_SYMBOLS: usize = 50_000;
    let symbols = graph.symbols_in_logical_file(path, MAX_FILE_SYMBOLS + 1)?;
    if symbols.len() > MAX_FILE_SYMBOLS {
        return Err(affected_test_graph_problem(
            "verified affected-test file census exceeded its symbol budget",
        ));
    }
    Ok(symbols)
}

fn graph_test_symbols(summaries: &[CodeGraphSymbolSummaryV1]) -> Result<Vec<GraphTestSymbol>> {
    summaries
        .iter()
        .filter_map(|summary| graph_test_symbol(summary).transpose())
        .collect()
}

fn graph_test_symbol(summary: &CodeGraphSymbolSummaryV1) -> Result<Option<GraphTestSymbol>> {
    let metadata = summary.metadata.as_ref().ok_or_else(|| {
        affected_test_graph_problem("verified affected-test symbol is missing extraction metadata")
    })?;
    if !matches!(metadata.kind.as_str(), "function" | "method") {
        return Ok(None);
    }
    let binding = summary.binding.as_ref().ok_or_else(|| {
        affected_test_graph_problem("verified affected-test symbol is missing its file binding")
    })?;
    let file_path = binding.logical_path.as_ref().ok_or_else(|| {
        affected_test_graph_problem("verified affected-test symbol is missing its logical path")
    })?;
    Ok(Some(GraphTestSymbol {
        id: summary.occurrence.as_str().to_owned(),
        kind: metadata.kind.clone(),
        qualified_name: metadata.qualified_name.clone(),
        file_path: file_path.clone(),
    }))
}

fn test_annotations_in_file<'a>(
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    path: &str,
    cache: &'a mut HashMap<String, HashSet<String>>,
) -> Result<&'a HashSet<String>> {
    if !cache.contains_key(path) {
        const MAX_ANNOTATION_RELATIONS: usize = 50_000;
        let symbols = affected_test_symbols_in_file(graph, path)?;
        let markers = symbols
            .iter()
            .filter(|symbol| {
                symbol.metadata.as_ref().is_some_and(|metadata| {
                    metadata.kind == "annotation_usage"
                        && matches!(
                            metadata.simple_name.as_str(),
                            "test" | "wasm_bindgen_test" | "rstest" | "parameterized"
                        )
                })
            })
            .map(|symbol| symbol.occurrence.clone())
            .collect::<HashSet<_>>();
        let occurrences = symbols
            .iter()
            .map(|symbol| symbol.occurrence.clone())
            .collect::<Vec<_>>();
        let annotated = graph
            .edges_among(
                &occurrences,
                &[RelationEdgeKindV1::Annotates],
                MAX_ANNOTATION_RELATIONS,
            )?
            .into_iter()
            .filter(|edge| markers.contains(&edge.edge.from_occurrence))
            .map(|edge| edge.edge.to_occurrence.as_str().to_owned())
            .collect();
        cache.insert(path.to_owned(), annotated);
    }
    cache.get(path).ok_or_else(|| {
        affected_test_graph_problem("verified affected-test annotation cache insertion failed")
    })
}

fn affected_test_graph_problem(detail: &str) -> TraceDecayError {
    TraceDecayError::project_route("verified-affected-test-evidence-unavailable", false, detail)
}

fn is_callable(node: &GraphTestSymbol) -> bool {
    matches!(node.kind.as_str(), "function" | "method")
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
