//! CLI presentation for the closed Work application binding.

use std::io::{Read, Write};

use serde_json::Value;
use tracedecay_application::ApplicationResult;

use crate::cli::WorkInvocationArgs;

pub(crate) async fn run(invocation: WorkInvocationArgs) -> tracedecay::errors::Result<()> {
    let body = read_request(&invocation.request_file)?;
    let project_root = tracedecay::config::resolve_path_with_discovery(invocation.project);
    let operation = invocation.operation;
    let mut response =
        tracedecay::work_cli::invoke_work_cli_with_delivery(project_root.clone(), operation, body)
            .await?;
    let rendered = if invocation.json {
        work_json_line(&response.outcome)?
    } else {
        let outcome = response.outcome.as_ref().map_err(|problem| {
            tracedecay::errors::TraceDecayError::Config {
                message: format!("{}: {}", problem.problem.code, problem.problem.message),
            }
        })?;
        format!(
            "Work {}\nProject: {}\n{}\n",
            operation.route_segment().replace('-', " "),
            project_root.display(),
            serde_json::to_string_pretty(outcome)?
        )
    };

    let mut stdout = std::io::stdout().lock();
    let write_result = write_work_output(&mut stdout, rendered.as_bytes());
    drop(stdout);
    let delivery_settlement = classify_work_output(&write_result);
    match write_result {
        Ok(()) => {
            if let Some(delivery) = response.take_delivery() {
                match delivery_settlement {
                    WorkOutputSettlement::Delivered => delivery.acknowledge_delivered().await?,
                    WorkOutputSettlement::Dropped(reason) => {
                        let _ = delivery.acknowledge_dropped(reason).await;
                    }
                }
            }
        }
        Err(error) => {
            if let Some(delivery) = response.take_delivery() {
                let reason = match delivery_settlement {
                    WorkOutputSettlement::Dropped(reason) => reason,
                    WorkOutputSettlement::Delivered => {
                        tracedecay_domain::DeliveryDropReasonV1::Disconnected
                    }
                };
                let _ = delivery.acknowledge_dropped(reason).await;
            }
            return Err(error.into());
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkOutputSettlement {
    Delivered,
    Dropped(tracedecay_domain::DeliveryDropReasonV1),
}

fn write_work_output<W: Write>(writer: &mut W, rendered: &[u8]) -> std::io::Result<()> {
    writer.write_all(rendered).and_then(|()| writer.flush())
}

fn classify_work_output(result: &std::io::Result<()>) -> WorkOutputSettlement {
    if result.is_ok() {
        WorkOutputSettlement::Delivered
    } else {
        WorkOutputSettlement::Dropped(tracedecay_domain::DeliveryDropReasonV1::Disconnected)
    }
}

fn work_json_line(outcome: &ApplicationResult<Value>) -> serde_json::Result<String> {
    crate::cli::output::json::json_line(outcome)
}

fn read_request(path: &std::path::Path) -> tracedecay::errors::Result<Value> {
    let payload = if path == std::path::Path::new("-") {
        let mut payload = String::new();
        std::io::stdin().read_to_string(&mut payload)?;
        payload
    } else {
        std::fs::read_to_string(path)?
    };
    serde_json::from_str(&payload).map_err(|error| tracedecay::errors::TraceDecayError::Config {
        message: format!(
            "Work request file {} is not valid JSON: {error}",
            path.display()
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::work_json_line;
    use super::{WorkOutputSettlement, classify_work_output, write_work_output};
    use std::io::{self, Write};
    use tracedecay_application::{
        ApplicationProblem, ApplicationProblemEnvelope, ApplicationResult, RequestId,
        ResultContractRef, RetryDirective,
    };
    use tracedecay_tool_catalog::SchemaId;

    #[test]
    fn work_json_line_preserves_the_canonical_typed_problem() {
        let outcome: ApplicationResult<serde_json::Value> = Err(ApplicationProblemEnvelope::new(
            ResultContractRef::new(
                SchemaId::new("schema.work.start_attempt.result").unwrap(),
                1,
            )
            .unwrap(),
            RequestId::new("request.cli.work.7").unwrap(),
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
        ));

        let rendered = work_json_line(&outcome).expect("work JSON line");
        let problem: serde_json::Value =
            serde_json::from_str(rendered.trim_end()).expect("typed work problem JSON");
        assert_eq!(
            problem["contract"]["schema_id"],
            "schema.work.start_attempt.result"
        );
        assert_eq!(problem["contract"]["schema_revision"], 1);
        assert_eq!(problem["request_id"], "request.cli.work.7");
        assert_eq!(problem["problem"]["kind"], "not_found_or_not_authorized");
        assert_eq!(rendered.lines().count(), 1);
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
        assert_eq!(
            classify_work_output(&write_result),
            WorkOutputSettlement::Dropped(tracedecay_domain::DeliveryDropReasonV1::Disconnected)
        );
        assert_ne!(
            classify_work_output(&write_result),
            WorkOutputSettlement::Delivered
        );
    }
}
