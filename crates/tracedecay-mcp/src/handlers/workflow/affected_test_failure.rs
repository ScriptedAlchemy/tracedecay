//! Typed terminal results for failed managed affected-test executions.

use tracedecay_contracts::OperationTermination;
use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_contracts::retrieval::{AffectedTestErrorV1, RunAffectedTestsResultV1};

use tracedecay_domain::errors::Result;

use super::{
    ManagedTestRun, TestTarget, emit_observed_test_results, parse_libtest_output,
    run_affected_tests_body,
};
use crate::handlers::graph::graph_tool_completion;
use crate::{TestRunFailure, TestRunOutput};

#[hotpath::measure(future = true, label = "mcp.workflow.affected_tests.failure")]
#[cfg_attr(
    not(feature = "hotpath"),
    expect(
        clippy::too_many_lines,
        reason = "Terminal test-failure mapping is one runner-output classify into a typed problem."
    )
)]
pub(super) async fn terminal_failure(
    managed: &ManagedTestRun,
    timeout_secs: u64,
    failure: TestRunFailure,
    test_names: &[String],
    truncated: bool,
    selected_targets: &[TestTarget],
) -> Result<GraphToolCompletionV1> {
    let mut partial = failure.partial_output().cloned();
    let failure_exit_code = match &failure {
        TestRunFailure::Harness { exit_code, .. } => *exit_code,
        _ => None,
    };
    if let Some(output) = &mut partial {
        output.exit_code = output.exit_code.or(failure_exit_code);
    }
    let (termination, output_bytes, kind, operation, message) = match failure {
        TestRunFailure::Spawn(error) => (
            OperationTermination::Failed,
            0,
            "cargo",
            "test",
            format!("failed to spawn cargo test: {error}"),
        ),
        TestRunFailure::Cancelled { output_bytes, .. } => (
            OperationTermination::Cancelled,
            output_bytes,
            "cargo",
            "test",
            "cargo test cancelled".to_owned(),
        ),
        TestRunFailure::Timeout { output_bytes, .. } => (
            OperationTermination::TimedOut,
            output_bytes,
            "cargo",
            "test",
            format!("cargo test timed out after {timeout_secs}s"),
        ),
        TestRunFailure::OutputLimit {
            stream,
            output_bytes,
            ..
        } => (
            OperationTermination::Failed,
            output_bytes,
            "cargo",
            "test",
            format!("cargo test {stream} exceeded its output bound"),
        ),
        TestRunFailure::Read { output_bytes, .. } => (
            OperationTermination::Failed,
            output_bytes,
            "cargo",
            "test",
            "cargo test output could not be read".to_owned(),
        ),
        TestRunFailure::Harness {
            exit_code,
            output_bytes,
            ..
        } => (
            OperationTermination::Failed,
            output_bytes,
            "cargo",
            "test",
            format!("cargo test returned nonzero exit status {exit_code:?}"),
        ),
        TestRunFailure::NoMatch {
            test_identity,
            output_bytes,
            ..
        } => (
            OperationTermination::Failed,
            output_bytes,
            "cargo",
            "test",
            format!("cargo test did not report the requested test `{test_identity}`"),
        ),
        TestRunFailure::InvalidIdentity { test_identity } => (
            OperationTermination::Failed,
            0,
            "invalid_test_identity",
            "test_identity",
            format!("test identity `{test_identity}` is not executable"),
        ),
    };
    let report = partial.as_ref().map_or_else(Default::default, |output| {
        hotpath::measure_block!(
            "mcp.workflow.affected_tests.parse",
            parse_libtest_output(&output.stdout)
        )
    });
    emit_observed_test_results(&managed.emitter, &report, test_names.len()).await?;
    let exit_code = partial
        .as_ref()
        .map_or(failure_exit_code, |output| output.exit_code);
    let receipt = managed
        .finish(termination, output_bytes, exit_code, &report)
        .await?;
    let partial = partial.unwrap_or(TestRunOutput {
        exit_code: failure_exit_code,
        stdout: String::new(),
        stderr: String::new(),
        output_bytes,
    });
    let run = hotpath::measure_block!("mcp.workflow.affected_tests.assemble", {
        let mut run = run_affected_tests_body(
            &partial,
            &report,
            test_names,
            truncated,
            selected_targets,
            managed.terminal(receipt),
        );
        run.error = Some(AffectedTestErrorV1 {
            kind: kind.to_owned(),
            operation: operation.to_owned(),
            message,
        });
        run
    });
    Ok(graph_tool_completion(
        GraphToolResultV1::RunAffectedTests(RunAffectedTestsResultV1::Ran(Box::new(run))),
        Vec::new(),
    ))
}
