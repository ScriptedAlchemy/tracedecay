//! Typed terminal results for failed managed affected-test executions.

use serde_json::Value;
use tracedecay_application::{Deadline, OperationTermination};
use tracedecay_domain::UtcMicros;

use crate::application::operation_stream::OperationEmitter;
use crate::errors::Result;

use super::super::ToolResult;
use super::test_runner::TestRunFailure;
use super::{finish_test_run, managed_test_error_result};

pub(super) async fn terminal_failure(
    emitter: &OperationEmitter,
    args: &Value,
    started_at: UtcMicros,
    effective_deadline: &Deadline,
    timeout_secs: u64,
    failure: TestRunFailure,
) -> Result<ToolResult> {
    let (termination, output_bytes, kind, operation, message) = match failure {
        TestRunFailure::Spawn(error) => (
            OperationTermination::Failed,
            0,
            "cargo",
            "test",
            format!("failed to spawn cargo test: {error}"),
        ),
        TestRunFailure::Cancelled { output_bytes } => (
            OperationTermination::Cancelled,
            output_bytes,
            "cargo",
            "test",
            "cargo test cancelled".to_owned(),
        ),
        TestRunFailure::Timeout { output_bytes } => (
            OperationTermination::TimedOut,
            output_bytes,
            "cargo",
            "test",
            format!("cargo test timed out after {timeout_secs}s"),
        ),
        TestRunFailure::OutputLimit {
            stream,
            output_bytes,
        } => (
            OperationTermination::Failed,
            output_bytes,
            "cargo",
            "test",
            format!("cargo test {stream} exceeded its output bound"),
        ),
        TestRunFailure::Read { output_bytes } => (
            OperationTermination::Failed,
            output_bytes,
            "cargo",
            "test",
            "cargo test output could not be read".to_owned(),
        ),
        TestRunFailure::Harness {
            exit_code,
            output_bytes,
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
    let receipt = finish_test_run(
        emitter,
        started_at,
        effective_deadline,
        termination,
        output_bytes,
    )
    .await?;
    Ok(managed_test_error_result(
        args, kind, operation, &message, emitter, receipt,
    ))
}
