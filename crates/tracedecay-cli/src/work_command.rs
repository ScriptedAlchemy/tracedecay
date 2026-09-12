//! CLI presentation for the closed Work application binding.

use std::io::Write;

use crate::cli::WorkInvocationArgs;

#[hotpath::measure(label = "cli.work.invoke", future = true)]
pub(crate) async fn run(invocation: WorkInvocationArgs) -> tracedecay_domain::errors::Result<()> {
    #[cfg(feature = "hotpath")]
    hotpath::val!("cli.work.operation").set(&invocation.operation.operation_key());
    let body = crate::work_cli::application_cli::read_request(
        &invocation.request_file,
        crate::work_cli::application_cli::WORK,
    )?;
    let project_root = tracedecay_configuration::resolve_path_with_discovery(invocation.project);
    let operation = invocation.operation;
    // The application round-trip timed apart from `cli.work.invoke` so daemon
    // latency is separable from request parsing, render, and delivery
    // settlement.
    let mut response = hotpath::future!(
        crate::work_cli::invoke_work_cli_with_delivery(project_root.clone(), operation, body),
        label = "cli.work.request"
    )
    .await?;
    let rendered = crate::work_cli::application_cli::render(
        crate::work_cli::application_cli::WORK,
        operation.route_segment(),
        &project_root,
        &response.outcome,
        invocation.json,
    )?;

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

#[cfg(test)]
mod tests {
    use std::io::{self, Write};

    use super::{WorkOutputSettlement, classify_work_output, write_work_output};

    #[test]
    fn work_json_line_preserves_the_canonical_typed_problem() {
        crate::work_cli::application_cli::tests::assert_json_problem(
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
