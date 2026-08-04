//! Typed terminal results for failed managed affected-test executions.

use serde_json::{Value, json};
use tracedecay_application::{Deadline, OperationTermination};
use tracedecay_domain::UtcMicros;

use crate::application::operation_stream::OperationEmitter;
use crate::errors::Result;

use super::super::ToolResult;
use super::test_runner::{TestRunFailure, TestRunOutput};
use super::{
    TestTarget, emit_observed_test_results, finish_test_run, managed_test_terminal,
    parse_libtest_output, run_affected_tests_body,
};

pub(super) async fn terminal_failure(
    emitter: &OperationEmitter,
    args: &Value,
    started_at: UtcMicros,
    effective_deadline: &Deadline,
    timeout_secs: u64,
    failure: TestRunFailure,
    test_names: &[String],
    truncated: bool,
    selected_targets: &[TestTarget],
) -> Result<ToolResult> {
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
    let results = partial
        .as_ref()
        .map_or_else(Vec::new, |output| parse_libtest_output(&output.stdout));
    emit_observed_test_results(emitter, &results, test_names.len()).await?;
    let receipt = finish_test_run(
        emitter,
        started_at,
        effective_deadline,
        termination,
        output_bytes,
    )
    .await?;
    let partial = partial.unwrap_or(TestRunOutput {
        exit_code: failure_exit_code,
        stdout: String::new(),
        stderr: String::new(),
        output_bytes,
    });
    let mut body = run_affected_tests_body(
        partial.exit_code.or(failure_exit_code),
        &results,
        test_names,
        truncated,
        selected_targets,
        &partial.stderr,
        &partial.stdout,
        managed_test_terminal(emitter, &receipt),
    );
    body["error"] = json!({
        "kind": kind,
        "operation": operation,
        "message": message,
    });
    Ok(super::super::support::generic_tool_result(
        None,
        args,
        &body,
        vec![],
    ))
}
