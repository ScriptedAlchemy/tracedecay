//! `tracedecay tool` for the closed Work and Workflow families.
//!
//! These tools run through the canonical owner their MCP calls reach, invoked
//! over the daemon socket instead of through a daemon MCP tool call, so the CLI
//! prints the MCP tool result byte-for-byte, bound to the family's typed result
//! contract, under a CLI request identity.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use serde_json::Value;
use tokio::time::Instant;
use tracedecay_api::WorkOperation;
use tracedecay_contracts::feedback::observations::FeedbackSourceEventV1;
use tracedecay_contracts::request_identity::{GlobalRequestSurface, mint_global_request_id};
use tracedecay_contracts::{
    ApplicationInvocation, ApplicationInvocationExecutor, ApplicationInvocationFuture,
    ApplicationResponse, CancellationSignal, Deadline, InvocationError,
    RUNTIME_MOUNTING_REASON_CODE,
};
use tracedecay_daemon_protocol::{
    DaemonInvocationClient, DaemonInvocationDelivery, DaemonInvocationError,
    DaemonInvocationExecutor, DaemonInvocationExecutorFuture, DaemonInvocationOutcome,
    DaemonInvocationRequest, DaemonInvocationResponse, InvocationCancellationPolicy,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_domain::{ManifestDigest, UtcMicros};
use tracedecay_mcp::tools::binding::{work_operation_for_tool, workflow_operation_for_tool};

use super::{
    OWNER_MOUNT_RESEND_DELAY, cli_request_controls, rendered_tool_output,
    tool_result_process_outcome,
};
use crate::work_cli::{WorkCliDelivery, work_delivery_is_eligible};

#[derive(Clone, Copy)]
pub(super) enum FamilyTool {
    Work(WorkOperation),
    Workflow,
}

impl FamilyTool {
    pub(super) fn from_tool_name(tool_name: &str) -> Option<Self> {
        work_operation_for_tool(tool_name)
            .map(Self::Work)
            .or_else(|| workflow_operation_for_tool(tool_name).map(|_| Self::Workflow))
    }
}

/// Run one Work or Workflow tool and print the tool result its MCP call returns.
#[hotpath::measure(label = "cli.tool.application_family", future = true)]
pub(super) async fn dispatch_cli_family_tool(
    tool: FamilyTool,
    tool_name: &str,
    tool_args: Value,
    project: Option<PathBuf>,
    raw_json: bool,
    deadline: Instant,
) -> Result<()> {
    let request_id =
        mint_global_request_id(GlobalRequestSurface::Cli).map_err(|_| TraceDecayError::Config {
            message: format!("could not allocate a request id for {tool_name}"),
        })?;
    let handshake = tracedecay::daemon::handshake_for_current_client(project, None, false, false)?;
    let executor = FamilyToolExecutor {
        client: tracedecay_daemon_identity::invocation_client_for_current(handshake)?,
        tool,
        delivery: Mutex::new(None),
        mounting: Mutex::new(false),
    };
    // A cold daemon refuses with the mounting problem while the project open
    // warms; that refusal precedes admission, so it is re-sent until the CLI
    // deadline like every other surface.
    let mut result = loop {
        let (request_deadline, cancellation) = cli_request_controls(&request_id, deadline)?;
        let result = match tool {
            FamilyTool::Work(_) => {
                tracedecay::mcp::tools::execute_work_tool_surface(
                    tool_name,
                    tool_args.clone(),
                    Some(&executor),
                    Some(request_id.clone()),
                    Some(request_deadline),
                    Some(cancellation),
                )
                .await?
            }
            FamilyTool::Workflow => {
                tracedecay::mcp::tools::execute_workflow_tool_surface(
                    tool_name,
                    tool_args.clone(),
                    Some(&executor),
                    Some(request_id.clone()),
                    Some(request_deadline),
                    Some(cancellation),
                )
                .await?
            }
        };
        if !executor.take_mounting()
            || deadline.saturating_duration_since(Instant::now()) <= OWNER_MOUNT_RESEND_DELAY
        {
            break result;
        }
        tokio::time::sleep(OWNER_MOUNT_RESEND_DELAY).await;
    };
    tracedecay_mcp::tool_errors::mark_semantic_tool_error(&mut result);
    let rendered = rendered_tool_output(&result.value, raw_json);
    let written = {
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "{rendered}").and_then(|()| stdout.flush())
    };
    // The daemon holds an eligible Work terminal open until the caller says
    // whether it reached the output boundary.
    if let Some(delivery) = executor.take_delivery() {
        match &written {
            Ok(()) => delivery.acknowledge_delivered().await?,
            Err(write_error) => {
                if let Err(error) = delivery
                    .acknowledge_dropped(tracedecay_domain::DeliveryDropReasonV1::Disconnected)
                    .await
                {
                    tracing::warn!(
                        tool = tool_name,
                        %write_error,
                        %error,
                        "Work delivery drop acknowledgement failed after an output write failure"
                    );
                }
            }
        }
    }
    written?;
    tool_result_process_outcome(&result.value, tool_name)
}

/// Daemon socket executor that retains the delivery authority of an eligible
/// Work terminal and notes an owner-mounting refusal for re-send.
struct FamilyToolExecutor {
    client: DaemonInvocationClient,
    tool: FamilyTool,
    delivery: Mutex<Option<DaemonInvocationDelivery>>,
    mounting: Mutex<bool>,
}

impl FamilyToolExecutor {
    fn take_delivery(&self) -> Option<WorkCliDelivery> {
        self.delivery
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .map(WorkCliDelivery::new)
    }

    fn take_mounting(&self) -> bool {
        std::mem::take(&mut *self.mounting.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn observe(
        &self,
        response: &DaemonInvocationResponse,
        delivery: Option<DaemonInvocationDelivery>,
    ) {
        let mounting = matches!(
            &response.outcome,
            DaemonInvocationOutcome::ApplicationProblem { problem }
                if problem
                    .diagnostic()
                    .is_some_and(|diagnostic| diagnostic.code == RUNTIME_MOUNTING_REASON_CODE)
        );
        *self.mounting.lock().unwrap_or_else(PoisonError::into_inner) = mounting;
        let eligible = match (self.tool, &response.outcome) {
            (
                FamilyTool::Work(operation),
                DaemonInvocationOutcome::WorkApplication { outcome, .. },
            ) => work_delivery_is_eligible(operation, outcome),
            _ => false,
        };
        // An ineligible delivery is dropped here: the daemon awaits no ACK
        // for it, and the unacknowledged connection never returns to the pool.
        *self.delivery.lock().unwrap_or_else(PoisonError::into_inner) =
            delivery.filter(|_| eligible);
    }
}

impl ApplicationInvocationExecutor for FamilyToolExecutor {
    fn invoke(
        &self,
        invocation: ApplicationInvocation,
    ) -> ApplicationInvocationFuture<'_, std::result::Result<ApplicationResponse, InvocationError>>
    {
        ApplicationInvocationExecutor::invoke(&self.client, invocation)
    }
}

impl DaemonInvocationExecutor for FamilyToolExecutor {
    fn invoke_controlled(
        &self,
        request: DaemonInvocationRequest,
        deadline: Deadline,
        cancellation: CancellationSignal,
        policy: InvocationCancellationPolicy,
    ) -> DaemonInvocationExecutorFuture<
        '_,
        std::result::Result<DaemonInvocationResponse, DaemonInvocationError>,
    > {
        Box::pin(async move {
            let (response, delivery) = self
                .client
                .invoke_controlled_with_delivery(request, deadline, cancellation, policy)
                .await?
                .into_parts();
            self.observe(&response, delivery);
            Ok(response)
        })
    }

    fn observe_feedback(
        &self,
        subject_digest: ManifestDigest,
        observed_at: UtcMicros,
        event: FeedbackSourceEventV1,
    ) -> DaemonInvocationExecutorFuture<'_, Result<()>> {
        DaemonInvocationExecutor::observe_feedback(&self.client, subject_digest, observed_at, event)
    }
}
