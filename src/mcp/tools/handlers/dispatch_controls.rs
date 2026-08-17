//! Exact admitted controls for the analytics handler entrypoint.

use serde_json::Value;
use tracedecay_application::{CancellationSignal, Deadline};

use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDbLeaseV1;
use crate::tracedecay::TraceDecay;

use super::super::ToolResult;
use super::{ToolCallRegistryOptions, analytics};

fn admitted_control(
    options: &ToolCallRegistryOptions<'_>,
    operation: &'static str,
) -> Result<(Deadline, CancellationSignal)> {
    let deadline = options
        .application_deadline
        .clone()
        .ok_or_else(|| TraceDecayError::Config {
            message: format!("{operation} request deadline is unavailable"),
        })?;
    let cancellation =
        options
            .application_cancellation
            .clone()
            .ok_or_else(|| TraceDecayError::Config {
                message: format!("{operation} cancellation authority is unavailable"),
            })?;
    Ok((deadline, cancellation))
}

pub(super) async fn dispatch_analytics(
    cg: &TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    let (deadline, cancellation) = admitted_control(&options, "analytics")?;
    analytics::handle_analytics(
        cg,
        args,
        options.accounting_db,
        options
            .session_authorities
            .project_registered
            .map(RegisteredGlobalDbLeaseV1::as_ref),
        deadline,
        cancellation,
    )
    .await
}
