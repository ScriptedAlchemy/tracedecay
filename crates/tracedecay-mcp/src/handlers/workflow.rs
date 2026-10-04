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
use serde_json::Value;
use tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1;
use tracedecay_code_index::intake::content_digest;
use tracedecay_contracts::clock::now_micros;
use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_contracts::retrieval::{
    AffectedTestOutcomeV1, AffectedTestRunV1, AffectedTestsNotRunV1, ManagedTestTerminalV1,
    RunAffectedTestsResultV1, RunAffectedTestsSurfaceRequestV1,
};
use tracedecay_contracts::retrieval::{
    DiagnoseItemV1, DiagnosePublicationV1, DiagnoseResultV1, DiagnoseSeverityFilterV1,
    DiagnoseSeverityV1, DiagnoseSurfaceRequestV1, DiagnoseSymbolV1,
};
use tracedecay_contracts::{
    CancellationObservation, CancellationSignal, CancellationStage, Deadline, OperationBudgetUsage,
    OperationReceipt, OperationTermination,
};
use tracedecay_domain::{CodeGenerationId, CommitId, UtcMicros};
use tracedecay_domain::{RelationEdgeKindV1, SymbolOccurrenceId};
use url::Url;

use tracedecay_application::diagnose::{Severity, parse_cargo_output};
use tracedecay_application::diagnostics_publication::CodeIndexPublicationIdentityPortV1;
use tracedecay_application::diagnostics_store::DiagnosticsStore;
use tracedecay_application::operation_stream::{
    OperationEmitter, OperationEventError, operation_event_authority,
};
use tracedecay_code_index::is_test_file;
use tracedecay_contracts::feedback::TestResultProjectionV1;
use tracedecay_contracts::request_identity::{GlobalRequestSurface, mint_global_request_id};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::{
    ManagedTestRunOutcomeV1, ManagedTestRunStartV1, RegisteredGlobalDbLeaseV1,
};
use tracedecay_project::project::TraceDecay;

use crate::handlers::graph::graph_tool_completion;
use crate::handlers::support::decode_primitive_request;
use crate::handlers::unique_file_paths;

mod affected_test_failure;

use crate::workflow::refused_run;
use crate::{
    LibtestReport, RunAffectedArgs, TestProfile, TestRunControl, TestRunFailure, TestRunOutput,
    libtest_identity, parse_libtest_output, run_cargo_tests,
};
#[cfg(test)]
use crate::{MAX_TEST_TIMEOUT_SECS, cargo_test_args};

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

/// Computes `tracedecay_diagnose`: parses compiler output, maps each
/// diagnostic to its graph symbol, and publishes the parse.
#[tracing::instrument(name = "mcp.workflow.diagnose.total", level = "trace", skip_all)]
pub async fn compute_diagnose(
    cg: &TraceDecay,
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    args: Value,
    code_index_identity: Option<&dyn CodeIndexPublicationIdentityPortV1>,
) -> Result<GraphToolCompletionV1> {
    let request: DiagnoseSurfaceRequestV1 = decode_primitive_request(&args, "tracedecay_diagnose")?;
    // The upstream text carries no trustworthy capture timestamp. Record the
    // one temporal fact this server owns, once, before parse and enrichment.
    let diagnostic_observed_at = now_micros();

    let severity_filter = request.severity.unwrap_or_default();
    let include_callers = request.include_callers.unwrap_or(true);
    let max_diagnostics = request
        .max_diagnostics
        .map_or(50_usize, |v| v.min(500) as usize);

    let mut diagnostics: Vec<_> = {
        let _span = tracing::trace_span!("mcp.workflow.diagnose.parse").entered();
        {
            parse_cargo_output(&request.cargo_output)
                .into_iter()
                .filter(|d| match severity_filter {
                    DiagnoseSeverityFilterV1::Error => d.severity == Severity::Error,
                    DiagnoseSeverityFilterV1::Warning => d.severity == Severity::Warning,
                    DiagnoseSeverityFilterV1::All => true,
                })
                .collect()
        }
    };
    let total = diagnostics.len();
    diagnostics.truncate(max_diagnostics);

    let mut items: Vec<DiagnoseItemV1> = Vec::with_capacity(diagnostics.len());
    let mut touched: HashSet<String> = HashSet::new();
    for d in &diagnostics {
        // Preserve the compiler spelling in the result; the graph uses
        // project-relative paths with forward slashes.
        let path = normalized_diagnostic_path(cg.project_root(), &d.file);
        touched.insert(path.clone());
        let node = diagnostic_symbol_at_location(graph, &path, d.line)?;
        let callers = if include_callers {
            match &node {
                Some(n) => {
                    let callers = graph.callers(
                        std::slice::from_ref(&n.occurrence),
                        &[RelationEdgeKindV1::Calls],
                        5,
                    )?;
                    let trimmed = callers
                        .into_iter()
                        .next()
                        .into_iter()
                        .flatten()
                        .take(5)
                        .map(|edge| {
                            diagnostic_symbol(&edge.neighbor).inspect(|caller| {
                                touched.insert(caller.file.clone());
                            })
                        })
                        .collect::<Result<Vec<_>>>()?;
                    Some(trimmed)
                }
                None => Some(Vec::new()),
            }
        } else {
            None
        };

        items.push(DiagnoseItemV1 {
            severity: severity_wire(d.severity),
            code: d.code.clone(),
            message: d.message.clone(),
            file: d.file.clone(),
            line: d.line,
            column: d.column,
            node: node.as_ref().map(diagnostic_symbol).transpose()?,
            callers,
        });
    }

    // Populate the durable managed-diagnostics store so the LSP Problems
    // projection and every diagnostic read surface see these findings.
    let published = publish_parsed_compiler_diagnostics(
        cg,
        code_index_identity,
        &diagnostics,
        diagnostic_observed_at,
    )
    .await;

    let mapped = items.iter().filter(|item| item.node.is_some()).count();
    let result = DiagnoseResultV1 {
        diagnostics_parsed: total as u64,
        diagnostics_returned: items.len() as u64,
        mapped_to_node: mapped as u64,
        unmapped: (items.len() - mapped) as u64,
        truncated: total > items.len(),
        published,
        diagnostics: items,
        freshness: None,
    };
    Ok(graph_tool_completion(
        GraphToolResultV1::Diagnose(result),
        touched.into_iter().collect(),
    ))
}

/// The graph-lookup form of one compiler-reported path: forward slashes,
/// relative to the project root when the compiler reported it absolute.
fn normalized_diagnostic_path(project_root: &Path, file: &str) -> String {
    let forward = file.replace('\\', "/");
    let path = Path::new(&forward);
    if path.is_absolute()
        && let Ok(relative) = path.strip_prefix(project_root)
    {
        return relative.to_string_lossy().into_owned();
    }
    forward
}

fn diagnostic_symbol_at_location(
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
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

fn diagnostic_symbol(symbol: &CodeGraphSymbolSummaryV1) -> Result<DiagnoseSymbolV1> {
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
    let line = metadata
        .start_line
        .checked_add(1)
        .ok_or_else(|| diagnostic_graph_problem("verified diagnostic display line overflowed"))?;
    Ok(DiagnoseSymbolV1 {
        node_id: symbol.occurrence.as_str().to_owned(),
        name: metadata.simple_name.clone(),
        kind: metadata.kind.clone(),
        qualified_name: metadata.qualified_name.clone(),
        file: file.to_owned(),
        line,
        start_line: metadata.start_line,
        end_line,
    })
}

fn diagnostic_graph_problem(detail: &str) -> TraceDecayError {
    TraceDecayError::project_route("verified-diagnostic-evidence-unavailable", false, detail)
}

/// Publishes parsed compiler diagnostics into the durable managed-diagnostics
/// store as one clean-generation snapshot.
///
/// This is the production write path for the compiler pillar. Failure to
/// publish never fails the diagnose call. The caller still receives its
/// mapped diagnostics, but the outcome is reported in the response so a
/// silent no-op is observable.
///
/// Identity is resolved from the code-index generation authority, never minted
/// here. That is what lets the LSP feedback projection admit these records:
/// the projection compares a record's `file_occurrence_id` against the
/// saved-edit cycle's impact target and its `generation_id` against the cycle's
/// code-index generation, and both sides now come from the same mint. Without a
/// resolver, a direct, non-daemon server, the honest outcome is to publish
/// nothing under a named reason rather than to guess a repository-relative
/// path, which the projection could only refuse.
#[tracing::instrument(name = "mcp.workflow.diagnose.publish", level = "trace", skip_all)]
async fn publish_parsed_compiler_diagnostics(
    cg: &TraceDecay,
    code_index_identity: Option<&dyn CodeIndexPublicationIdentityPortV1>,
    parsed: &[tracedecay_application::diagnose::Diagnostic],
    observed_at: UtcMicros,
) -> DiagnosePublicationV1 {
    use tracedecay_application::diagnostics_publication::{
        compiler_diagnostic_analyzer_revision_v1, compiler_diagnostic_configuration_revision_v1,
    };

    let root = cg.project_root().to_path_buf();
    let Some(analyzer_revision) = compiler_diagnostic_analyzer_revision_v1().ok() else {
        return publication_skipped("analyzer-identity-unavailable", None);
    };
    let Some(configuration_revision) = compiler_diagnostic_configuration_revision_v1().ok() else {
        return publication_skipped("configuration-identity-unavailable", None);
    };
    let database = cg.dashboard_database_guard();
    let store = DiagnosticsStore::new(database.as_ref().clone());
    let outcome =
        tracedecay_application::diagnostics_publication::publish_compiler_diagnostics_through_code_index_v1(
            &root,
            code_index_identity,
            &store,
            parsed,
            analyzer_revision,
            configuration_revision,
            observed_at,
        )
        .await;
    compiler_publication_report(&outcome)
}

fn publication_skipped(reason: &str, unresolved: Option<Vec<String>>) -> DiagnosePublicationV1 {
    DiagnosePublicationV1::Skipped {
        reason: reason.to_owned(),
        unresolved,
    }
}

/// The typed publication outcome for the diagnose response. Every refusal
/// keeps its name so an empty Problems list is explainable.
fn compiler_publication_report(
    outcome: &tracedecay_application::diagnostics_publication::CompilerDiagnosticPublicationOutcomeV1,
) -> DiagnosePublicationV1 {
    use tracedecay_application::diagnostics_publication::CompilerDiagnosticPublicationOutcomeV1 as Outcome;

    let names = |skips: &[tracedecay_application::diagnostics_publication::CompilerDiagnosticResolutionSkipV1]| {
        skips.iter().map(ToString::to_string).collect::<Vec<_>>()
    };
    match outcome {
        Outcome::CodeIndexIdentityUnavailable => {
            publication_skipped("code-index-identity-unavailable", None)
        }
        Outcome::CodeIndexGenerationUnavailable => {
            publication_skipped("code-index-generation-unavailable", None)
        }
        Outcome::NoResolvableDiagnostics { unresolved } => {
            publication_skipped("no-resolvable-diagnostics", Some(names(unresolved)))
        }
        Outcome::Published {
            generation,
            report,
            unresolved,
        } => DiagnosePublicationV1::Published {
            generation: generation.as_str().to_owned(),
            publication_revision: report.publication_revision,
            inserted: report.inserted,
            cleared: report.cleared,
            unresolved: names(unresolved),
            rejected: report.rejected.iter().map(ToString::to_string).collect(),
        },
        Outcome::Failed { reason } => DiagnosePublicationV1::Failed {
            reason: reason.clone(),
        },
    }
}

fn severity_wire(s: Severity) -> DiagnoseSeverityV1 {
    match s {
        Severity::Error => DiagnoseSeverityV1::Error,
        Severity::Warning => DiagnoseSeverityV1::Warning,
        Severity::Note => DiagnoseSeverityV1::Note,
        Severity::Help => DiagnoseSeverityV1::Help,
    }
}

/// Where a managed run is durably recorded: the project sessions store, and
/// the requesting session the run is attributed to when the request named one.
pub struct ManagedTestRunRecording {
    pub store: RegisteredGlobalDbLeaseV1,
    pub session_id: Option<String>,
}

/// Computes `tracedecay_run_affected_tests` on the graph-tool owner's side:
/// selects the tests covering the changed-path manifest, runs them once each,
/// and reports every observed outcome. `recording` carries the owner's typed
/// refusal when the session store is not mounted; it is surfaced only once a
/// run would be recorded, after request validation and graph admission.
pub async fn compute_run_affected_tests<F>(
    cg: &TraceDecay,
    graph: F,
    args: Value,
    recording: Result<ManagedTestRunRecording>,
    cancellation: Option<CancellationSignal>,
) -> Result<GraphToolCompletionV1>
where
    F: Future<Output = Result<tracedecay_graph_query::VerifiedGraphQuery>>,
{
    let request: RunAffectedTestsSurfaceRequestV1 =
        decode_primitive_request(&args, "tracedecay_run_affected_tests")?;
    run_affected_tests_with_runner(cg, graph, request, recording, cancellation, run_cargo_tests)
        .await
}

#[tracing::instrument(name = "mcp.workflow.affected_tests.total", level = "trace", skip_all)]
#[expect(
    clippy::too_many_lines,
    reason = "Affected-test run is one select-and-execute through the injected runner."
)]
async fn run_affected_tests_with_runner<F, Runner, RunFuture>(
    cg: &TraceDecay,
    graph: F,
    request: RunAffectedTestsSurfaceRequestV1,
    recording: Result<ManagedTestRunRecording>,
    cancellation: Option<CancellationSignal>,
    runner: Runner,
) -> Result<GraphToolCompletionV1>
where
    F: Future<Output = Result<tracedecay_graph_query::VerifiedGraphQuery>>,
    Runner: FnOnce(PathBuf, TestProfile, Vec<String>, Duration, TestRunControl) -> RunFuture,
    RunFuture: Future<Output = std::result::Result<TestRunOutput, TestRunFailure>>,
{
    // The caller's manifest is the authority for the affected-test scope.
    // Graph admission stays unawaited until that scope is validated.
    let run_args = match RunAffectedArgs::from_request(request) {
        Ok(run_args) => run_args,
        Err(refused) => return Ok(not_run(*refused)),
    };
    let project_root = cg.project_root().to_path_buf();
    let changed_paths = run_args.changed_paths;
    if changed_paths.is_empty() {
        return Ok(not_run(empty_result("no changed files detected")));
    }

    let graph = &tracing::Instrument::instrument(
        graph,
        tracing::trace_span!("mcp.workflow.affected_tests.graph_admission"),
    )
    .await?;
    let test_targets = {
        let _span = tracing::trace_span!("mcp.workflow.affected_tests.graph").entered();
        collect_affected_test_targets(graph, &changed_paths)
    }?;

    if test_targets.is_empty() {
        return Ok(not_run(empty_result(&format!(
            "no tests cover the changed paths ({} file(s))",
            changed_paths.len()
        ))));
    }

    let (selected_targets, test_names, truncated) =
        select_test_targets(test_targets, run_args.max_tests);
    for test_name in &test_names {
        if let Err(message) = validate_test_identity(test_name) {
            return Ok(not_run(refused_run(
                "invalid_test_identity",
                "test_identity",
                &message,
            )));
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
    let managed = begin_test_run(
        cg,
        &changed_paths,
        ManagedTestRunAdmission {
            recording: recording?,
            started_at,
            deadline: effective_deadline,
            code_generation_id: graph.generation().clone(),
            requested_tests: test_names.len() as u64,
        },
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
    let cancellation = wait_for_test_run_cancellation(managed.emitter.clone(), cancellation);
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
                &managed,
                run_args.timeout_secs,
                failure,
                &test_names,
                truncated,
                &selected_targets,
            )
            .await;
        }
    };

    let report = {
        let _span = tracing::trace_span!("mcp.workflow.affected_tests.parse").entered();
        parse_libtest_output(&output.stdout)
    };
    if let Some(test_name) = missing_requested_test(&test_names, &report) {
        let any_requested_result = test_names
            .iter()
            .any(|requested| report.observed(requested));
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
            &managed,
            run_args.timeout_secs,
            failure,
            &test_names,
            truncated,
            &selected_targets,
        )
        .await;
    }
    emit_observed_test_results(&managed.emitter, &report, test_names.len()).await?;
    let receipt = managed
        .finish(
            OperationTermination::Completed,
            output.output_bytes,
            output.exit_code,
            &report,
        )
        .await?;

    let touched_files: Vec<String> = unique_file_paths(changed_paths.iter().map(String::as_str));
    let run = {
        let _span = tracing::trace_span!("mcp.workflow.affected_tests.assemble").entered();
        run_affected_tests_body(
            &output,
            &report,
            &test_names,
            truncated,
            &selected_targets,
            managed.terminal(receipt),
        )
    };
    Ok(graph_tool_completion(
        GraphToolResultV1::RunAffectedTests(RunAffectedTestsResultV1::Ran(Box::new(run))),
        touched_files,
    ))
}

fn not_run(not_run: AffectedTestsNotRunV1) -> GraphToolCompletionV1 {
    graph_tool_completion(
        GraphToolResultV1::RunAffectedTests(RunAffectedTestsResultV1::NotRun(not_run)),
        Vec::new(),
    )
}

async fn wait_for_test_run_cancellation(
    mut emitter: OperationEmitter,
    cancellation: Option<CancellationSignal>,
) {
    // CancellationSignal is still a polled atomic (no event wait API on this
    // type without changing application crate callers we do not own). When no
    // signal is attached, wait only on the emitter. Otherwise poll at 50ms.
    // Same cancel semantics, ~10x fewer timers than the prior 5ms wakeups.
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

/// What a managed run is admitted with: its durable recording, the time it
/// started, and the source identity it runs against.
struct ManagedTestRunAdmission {
    recording: ManagedTestRunRecording,
    started_at: UtcMicros,
    deadline: Deadline,
    code_generation_id: CodeGenerationId,
    requested_tests: u64,
}

/// One admitted managed run: its live lifecycle stream and its durable record.
pub(super) struct ManagedTestRun {
    pub(super) emitter: OperationEmitter,
    recording: ManagedTestRunRecording,
    started_at: UtcMicros,
    deadline: Deadline,
}

impl ManagedTestRun {
    fn operation_id(&self) -> String {
        self.emitter.binding().operation_id().to_string()
    }

    /// Records the admitted run durably. The live stream already admitted it,
    /// so a refused record settles the stream first: no subscriber waits on a
    /// run that will never execute.
    async fn record_start(&self, start: &ManagedTestRunStartV1) -> Result<()> {
        let Err(error) = self
            .recording
            .store
            .record_managed_test_run_start(start)
            .await
        else {
            return Ok(());
        };
        publish_test_run_terminal(
            &self.emitter,
            test_run_receipt(
                self.started_at,
                &self.deadline,
                OperationTermination::Failed,
                0,
            ),
        )
        .await?;
        Err(test_run_record_error(error))
    }

    /// Settles the durable record with the run's exit status and outcome
    /// counts, then publishes the terminal receipt. Readers rely on that
    /// order: a run whose live stream terminated has its outcome recorded
    /// unless recording it failed. The receipt is published either way, so
    /// no subscriber waits on a run that already ended.
    #[tracing::instrument(name = "mcp.workflow.affected_tests.finish", level = "trace", skip_all)]
    pub(super) async fn finish(
        &self,
        termination: OperationTermination,
        bytes_consumed: u64,
        exit_code: Option<i32>,
        report: &LibtestReport,
    ) -> Result<OperationReceipt> {
        let receipt =
            test_run_receipt(self.started_at, &self.deadline, termination, bytes_consumed);
        let outcome = ManagedTestRunOutcomeV1 {
            receipt: receipt.clone(),
            exit_code,
            results: report
                .results
                .iter()
                .map(|(test, passed)| TestResultProjectionV1 {
                    test: test.clone(),
                    passed: *passed,
                })
                .collect(),
            ignored: report.ignored.len() as u64,
        };
        let recorded = self
            .recording
            .store
            .record_managed_test_run_outcome(&self.operation_id(), &outcome)
            .await
            .map_err(test_run_record_error);
        publish_test_run_terminal(&self.emitter, receipt.clone()).await?;
        recorded?;
        Ok(receipt)
    }

    pub(super) fn terminal(&self, receipt: OperationReceipt) -> ManagedTestTerminalV1 {
        ManagedTestTerminalV1 {
            operation_id: self.operation_id(),
            result_tool: "tracedecay_test_results".to_owned(),
            session_id: self.recording.session_id.clone(),
            receipt,
        }
    }
}

#[tracing::instrument(name = "mcp.workflow.affected_tests.begin", level = "trace", skip_all)]
async fn begin_test_run(
    cg: &TraceDecay,
    changed_paths: &[String],
    admission: ManagedTestRunAdmission,
) -> Result<ManagedTestRun> {
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
    let document_content_digests =
        managed_test_document_content_digests(&root, changed_paths).await?;
    let emitter = operation_event_authority()
        .begin_managed_test_run(root_uri.clone(), request_id, admission.deadline.clone())
        .await
        .map_err(|error| test_run_event_error(&error))?;
    let run = ManagedTestRun {
        emitter,
        recording: admission.recording,
        started_at: admission.started_at,
        deadline: admission.deadline,
    };
    let start = ManagedTestRunStartV1 {
        operation_id: run.operation_id(),
        root_uri,
        session_id: run.recording.session_id.clone(),
        head_commit_id,
        code_generation_id: Some(admission.code_generation_id),
        document_content_digests,
        started_at: run.started_at,
        requested_tests: admission.requested_tests,
    };
    run.record_start(&start).await?;
    Ok(run)
}

#[tracing::instrument(
    name = "mcp.workflow.affected_tests.digests",
    level = "trace",
    skip_all
)]
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
                    Ok(uri) => Ok(Some((uri.to_string(), content_digest(&bytes)))),
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

pub(crate) fn current_head_commit_id(root: &Path) -> Option<CommitId> {
    let repository = tracedecay_runtime_core::git_open::open(root).ok()?;
    let commit = repository.head_commit().ok()?;
    CommitId::new(commit.id().to_hex().to_string()).ok()
}

#[tracing::instrument(name = "mcp.workflow.affected_tests.emit", level = "trace", skip_all)]
async fn emit_observed_test_results(
    emitter: &OperationEmitter,
    report: &LibtestReport,
    requested_total: usize,
) -> Result<()> {
    for (test, passed) in &report.results {
        emitter
            .test_result(test.clone(), *passed)
            .await
            .map_err(|error| test_run_event_error(&error))?;
    }
    emitter
        .progress(
            (report.results.len() + report.ignored.len()) as u64,
            Some(requested_total as u64),
        )
        .await
        .map(|_| ())
        .map_err(|error| test_run_event_error(&error))
}

fn test_run_receipt(
    started_at: UtcMicros,
    effective_deadline: &Deadline,
    termination: OperationTermination,
    bytes_consumed: u64,
) -> OperationReceipt {
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
    OperationReceipt {
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
    }
}

async fn publish_test_run_terminal(
    emitter: &OperationEmitter,
    receipt: OperationReceipt,
) -> Result<()> {
    emitter
        .terminal(receipt)
        .await
        .map(|_| ())
        .map_err(|error| test_run_event_error(&error))
}

fn test_run_event_error(error: &OperationEventError) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("managed test-run lifecycle failed: {error}"),
    }
}

fn test_run_record_error(message: String) -> TraceDecayError {
    TraceDecayError::Database {
        message,
        operation: "record managed test run".to_owned(),
    }
}

fn test_run_contract_error(error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("managed test-run contract failed: {error}"),
    }
}

fn collect_affected_test_targets(
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
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
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
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
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
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
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    path: &str,
    cache: &'a mut HashMap<String, HashSet<String>>,
) -> Result<&'a HashSet<String>> {
    if !cache.contains_key(path) {
        const MAX_ANNOTATION_RELATIONS: usize = 50_000;
        let symbols = affected_test_symbols_in_file(graph, path)?;
        let markers = symbols
            .iter()
            .filter(|symbol| {
                symbol
                    .metadata
                    .as_ref()
                    .is_some_and(tracedecay_code_index::is_test_marker)
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
            .filter(|edge| markers.contains(&edge.from_occurrence))
            .map(|edge| edge.to_occurrence.as_str().to_owned())
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

fn missing_requested_test<'a>(requested: &'a [String], report: &LibtestReport) -> Option<&'a str> {
    requested
        .iter()
        .find_map(|requested| (!report.observed(requested)).then_some(requested.as_str()))
}

fn run_affected_tests_body(
    output: &crate::TestRunOutput,
    report: &LibtestReport,
    test_names: &[String],
    truncated: bool,
    selected_targets: &[TestTarget],
    terminal: ManagedTestTerminalV1,
) -> AffectedTestRunV1 {
    let results = &report.results;
    let passed = results.iter().filter(|(_, ok)| *ok).count();
    AffectedTestRunV1 {
        exit_code: output.exit_code,
        passed: passed as u64,
        failed: (results.len() - passed) as u64,
        ignored: report.ignored.len() as u64,
        total_observed: (results.len() + report.ignored.len()) as u64,
        dispatched_tests: test_names.to_vec(),
        truncated,
        results: results
            .iter()
            .map(|(name, ok)| AffectedTestOutcomeV1 {
                test: name.clone(),
                passed: *ok,
                covers_source_ids: covered_source_ids(name, selected_targets),
            })
            .collect(),
        stderr_tail: tail(&output.stderr, 2000),
        stdout_tail: tail(&output.stdout, 2000),
        terminal,
        error: None,
    }
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

/// A run with nothing to dispatch.
fn empty_result(message: &str) -> AffectedTestsNotRunV1 {
    AffectedTestsNotRunV1 {
        passed: 0,
        failed: 0,
        results: Vec::new(),
        note: Some(message.to_owned()),
        error: None,
    }
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
