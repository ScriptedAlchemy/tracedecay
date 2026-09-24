//! Per-project owner for the graph and port reads whose typed results are
//! computed by the project's handler authority.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{Map, Value};
use tracedecay_contracts::graph_tool::GraphToolCompletionV1;
use tracedecay_contracts::{
    ApplicationProblem, CancellationContext, CancellationSignal, CancellationState, Deadline,
    RequestId, ResolvedScope, now_micros,
};
use tracedecay_daemon_protocol::{
    DaemonInvocationOutcome, DaemonInvocationProblem, DaemonInvocationResponse,
};
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

use super::DaemonInvocationService;
use crate::project_runtime::ProjectRuntimeRegistryError;

/// One admitted graph-tool invocation.
pub struct GraphToolInvocationV1 {
    pub operation: ApplicationSurfaceOperation,
    pub arguments: Map<String, Value>,
    pub request_id: RequestId,
    pub deadline: Deadline,
    pub cancellation: CancellationSignal,
}

pub type GraphToolFuture<'a> =
    Pin<Box<dyn Future<Output = Result<GraphToolCompletionV1, ApplicationProblem>> + Send + 'a>>;

/// The project's handler authority that computes graph-tool results.
pub trait ProjectGraphToolPortV1: Send + Sync {
    fn execute(&self, invocation: GraphToolInvocationV1) -> GraphToolFuture<'_>;
}

/// The registered owner: the authorized scope it answers for and its port.
#[derive(Clone)]
pub struct RegisteredGraphToolOwnerV1 {
    scope: ResolvedScope,
    port: Arc<dyn ProjectGraphToolPortV1>,
}

impl RegisteredGraphToolOwnerV1 {
    pub fn new(scope: ResolvedScope, port: Arc<dyn ProjectGraphToolPortV1>) -> Self {
        Self { scope, port }
    }
}

/// Typed refusal from [`DaemonInvocationService::register_graph_tool_owner`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DaemonGraphToolOwnerRegistrationError {
    #[error(transparent)]
    Registry(#[from] ProjectRuntimeRegistryError),
    #[error("a graph-tool owner for a different authorized scope is already registered")]
    ForeignAuthority,
}

impl DaemonInvocationService {
    /// Registers this project's graph-tool owner. A later server for the same
    /// authorized scope (the full server replacing the core, a reopen)
    /// replaces the port; a foreign scope is refused.
    #[hotpath::skip]
    pub async fn register_graph_tool_owner(
        &self,
        project_root: PathBuf,
        owner: RegisteredGraphToolOwnerV1,
    ) -> Result<(), DaemonGraphToolOwnerRegistrationError> {
        self.project_runtimes
            .register_or_reconcile(
                project_root,
                |incumbent: &mut RegisteredGraphToolOwnerV1| {
                    if incumbent.scope == owner.scope {
                        incumbent.port = Arc::clone(&owner.port);
                        Ok(())
                    } else {
                        Err(DaemonGraphToolOwnerRegistrationError::ForeignAuthority)
                    }
                },
                || async { Ok(owner.clone()) },
            )
            .await
    }
}

#[hotpath::measure(label = "daemon.service.graph_tool.execute", future = true)]
pub(super) async fn execute_graph_tool(
    request_id: String,
    owner: RegisteredGraphToolOwnerV1,
    operation: ApplicationSurfaceOperation,
    arguments: Map<String, Value>,
    deadline: Deadline,
    cancellation: CancellationContext,
    request_cancellation: tracedecay_runtime_core::cancellation::CancellationToken,
) -> DaemonInvocationResponse {
    let (Ok(typed_request_id), Ok(signal)) = (
        RequestId::new(request_id.clone()),
        CancellationSignal::active(cancellation.token_id.as_str()),
    ) else {
        return DaemonInvocationResponse::problem(
            request_id,
            DaemonInvocationProblem::InvalidRequest,
        );
    };
    if let CancellationState::Cancelled { requested_at } = &cancellation.state {
        signal.cancel(*requested_at);
    }
    let execution = owner.port.execute(GraphToolInvocationV1 {
        operation,
        arguments,
        request_id: typed_request_id,
        deadline,
        cancellation: signal.clone(),
    });
    tokio::pin!(execution);
    let outcome = tokio::select! {
        outcome = &mut execution => outcome,
        () = request_cancellation.cancelled() => {
            signal.cancel(now_micros());
            execution.await
        }
    };
    let outcome = match outcome {
        Ok(completion) => DaemonInvocationOutcome::GraphTool {
            scope: owner.scope,
            completion,
        },
        Err(problem) => DaemonInvocationOutcome::ApplicationProblem { problem },
    };
    DaemonInvocationResponse::with_outcome(request_id, outcome)
}
