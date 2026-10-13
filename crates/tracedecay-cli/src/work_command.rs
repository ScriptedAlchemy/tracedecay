//! CLI presentation for the closed Work application binding.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use tracedecay_api::WorkOperation;
use tracedecay_contracts::ApplicationResult;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::config::ProfileRoot;

use crate::cli::WorkInvocationArgs;

/// How long `--until-settled` lets providers progress before asking again.
const SETTLE_POLL_INTERVAL: Duration = Duration::from_secs(2);

#[tracing::instrument(name = "cli.work.invoke", level = "trace", skip_all)]
pub(crate) async fn run(profile: &ProfileRoot, args: WorkInvocationArgs) -> Result<()> {
    let WorkInvocationArgs {
        invocation,
        until_settled,
    } = args;
    tracing::trace!(name: "cli.work.operation", value = ?invocation.operation.operation_key());
    let operation = invocation.operation;
    if until_settled && operation != WorkOperation::AdvanceJob {
        return Err(TraceDecayError::Config {
            message: "--until-settled applies only to `tracedecay work advance-job`".to_owned(),
        });
    }
    let body = crate::application_cli::read_request(
        &invocation.request_file,
        crate::application_cli::WORK,
    )?;
    let project_root =
        tracedecay_configuration::resolve_path_with_discovery(profile, invocation.project);
    loop {
        let outcome = invoke_once(
            profile,
            &project_root,
            operation,
            body.clone(),
            invocation.json,
        )
        .await?;
        if !until_settled {
            return Ok(());
        }
        match job_state(&outcome)? {
            JobState::Advancing => {}
            JobState::Waiting => tokio::time::sleep(SETTLE_POLL_INTERVAL).await,
            JobState::Completed => return Ok(()),
            JobState::Refused { code, message } => {
                return Err(TraceDecayError::tool_refused(
                    "work advance-job",
                    Some(code),
                    Some(message),
                ));
            }
        }
    }
}

enum JobState {
    Advancing,
    Waiting,
    Completed,
    Refused { code: String, message: String },
}

/// The settled-or-not reading of one successful `advance-job` result.
fn job_state(outcome: &ApplicationResult<Value>) -> Result<JobState> {
    let state = outcome
        .as_ref()
        .ok()
        .and_then(|envelope| envelope.outcome.payload())
        .map(|payload| &payload["state"]);
    let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
    match state.and_then(|state| state["state"].as_str()) {
        Some("advancing") => Ok(JobState::Advancing),
        Some("waiting") => Ok(JobState::Waiting),
        Some("completed") => Ok(JobState::Completed),
        Some("refused") => {
            let problem = state.map_or(&Value::Null, |state| &state["problem"]);
            Ok(JobState::Refused {
                code: text(&problem["code"]),
                message: text(&problem["message"]),
            })
        }
        _ => Err(TraceDecayError::Config {
            message: "the daemon returned an advance-job result without a job state".to_owned(),
        }),
    }
}

/// One-line-per-step progress for a human reading `advance-job` output.
fn render_job_progress(outcome: &ApplicationResult<Value>) -> Option<String> {
    let payload = outcome.as_ref().ok()?.outcome.payload()?;
    let step_name = |step: &Value| {
        let name = step["step"].as_str().unwrap_or_default();
        match step["task"].as_str() {
            Some(task) => format!("{name} {task}"),
            None => name.to_owned(),
        }
    };
    let mut lines = payload["performed"]
        .as_array()?
        .iter()
        .map(|record| {
            format!(
                "performed {}  {}  {}",
                step_name(&record["step"]),
                record["receipt"]["operation"].as_str().unwrap_or_default(),
                record["request_id"].as_str().unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    let state = &payload["state"];
    lines.push(match state["state"].as_str()? {
        "waiting" => format!(
            "waiting   {}",
            state["on"]
                .as_array()?
                .iter()
                .map(|wait| match wait["wait"].as_str() {
                    Some("attempt") => format!(
                        "attempt {} {}",
                        wait["identity"]["attempt_id"].as_str().unwrap_or_default(),
                        wait["state"].as_str().unwrap_or_default()
                    ),
                    Some("capacity") =>
                        format!("capacity for {}", wait["task"].as_str().unwrap_or_default()),
                    _ => format!(
                        "proposal route for {}",
                        wait["task"].as_str().unwrap_or_default()
                    ),
                })
                .collect::<Vec<_>>()
                .join("; ")
        ),
        "refused" => format!(
            "refused   {}  {}: {}",
            step_name(&state["step"]),
            state["problem"]["code"].as_str().unwrap_or_default(),
            state["problem"]["message"].as_str().unwrap_or_default(),
        ),
        "completed" => format!(
            "completed synthesis {}",
            state["synthesis"]["identity"]["attempt_id"]
                .as_str()
                .unwrap_or_default()
        ),
        other => other.to_owned(),
    });
    Some(lines.join("\n") + "\n")
}

async fn invoke_once(
    profile: &ProfileRoot,
    project_root: &Path,
    operation: WorkOperation,
    body: Value,
    json: bool,
) -> Result<ApplicationResult<Value>> {
    // The application round-trip timed apart from `cli.work.invoke` so daemon
    // latency is separable from request parsing, render, and delivery
    // settlement.
    let mut response = tracing::Instrument::instrument(
        crate::application_cli::invoke(
            profile,
            project_root,
            crate::application_cli::ApplicationOperation::Work(operation),
            body,
        ),
        tracing::trace_span!("cli.work.request"),
    )
    .await?;
    let rendered = match (operation, json) {
        (WorkOperation::AdvanceJob, false) => render_job_progress(&response.outcome),
        _ => None,
    };
    let rendered = match rendered {
        Some(rendered) => rendered,
        None => crate::application_cli::render(
            crate::application_cli::WORK,
            operation.route_segment(),
            project_root,
            &response.outcome,
            json,
        )?,
    };

    let mut stdout = std::io::stdout().lock();
    let write_result = write_work_output(&mut stdout, rendered.as_bytes());
    drop(stdout);
    match write_result {
        Ok(()) => {
            if let Some(delivery) = response.take_delivery() {
                delivery.acknowledge_delivered().await?;
            }
        }
        Err(error) => {
            if let Some(delivery) = response.take_delivery() {
                let _ = delivery
                    .acknowledge_dropped(tracedecay_domain::DeliveryDropReasonV1::Disconnected)
                    .await;
            }
            return Err(error.into());
        }
    }
    crate::application_cli::refused(
        crate::application_cli::WORK,
        operation.route_segment(),
        &response.outcome,
    )?;
    Ok(response.outcome)
}

fn write_work_output<W: Write>(writer: &mut W, rendered: &[u8]) -> std::io::Result<()> {
    writer.write_all(rendered).and_then(|()| writer.flush())
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};

    use super::write_work_output;

    #[test]
    fn work_json_line_preserves_the_canonical_typed_problem() {
        crate::application_cli::tests::assert_json_problem(
            "schema.work.start_attempt.result",
            "request.cli.work.7",
        );
    }

    struct BrokenPipeWriter;

    impl Write for BrokenPipeWriter {
        fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "stdout closed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn broken_pipe_output_selects_dropped_disconnected_settlement() {
        let mut writer = BrokenPipeWriter;
        let write_result = write_work_output(&mut writer, b"work output\n");

        assert_eq!(
            write_result
                .as_ref()
                .expect_err("broken pipe should fail output")
                .kind(),
            io::ErrorKind::BrokenPipe
        );
    }
}
