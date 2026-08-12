//! Exact admitted controls for the analytics handler entrypoint.

use std::sync::Arc;

use serde_json::Value;
use tracedecay_application::{CancellationSignal, Deadline};
use tracedecay_store::FactReadControl;

use crate::errors::{Result, TraceDecayError};
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
    let fact_read_control = FactReadControl::new(Arc::new(move || {
        cancellation.is_cancelled()
            || deadline.is_elapsed_at(tracedecay_application::clock::now_micros())
    }));
    analytics::handle_analytics(
        cg,
        args,
        options.global_db.map(std::sync::Arc::as_ref),
        options.accounting_db,
        options
            .session_authorities
            .project_registered
            .map(std::sync::Arc::as_ref),
        &fact_read_control,
    )
    .await
}
