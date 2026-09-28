//! First-party commands' `tracedecay_admin_project` requests to the project's
//! owner.

use tokio::time::Instant;
use tracedecay_contracts::graph_tool::GraphToolResultV1;
use tracedecay_contracts::retrieval::{
    AdminProjectCounterV1, AdminProjectResultV1, AdminProjectSurfaceRequestV1,
};
use tracedecay_daemon_protocol::DaemonHandshake;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::config::ProfileRoot;
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

/// Asks `project_path`'s owner for one action under the CLI tool deadline.
pub(crate) async fn admin_project(
    profile: &ProfileRoot,
    project_path: &std::path::Path,
    request: AdminProjectSurfaceRequestV1,
) -> Result<AdminProjectResultV1> {
    let handshake = super::client_handshake(profile, Some(project_path))?;
    let deadline = Instant::now() + crate::tool_command::tool_command_deadline()?;
    admin_project_until(profile, handshake, request, deadline).await
}

/// Asks the handshake project's owner for one action by `deadline`.
pub(crate) async fn admin_project_until(
    profile: &ProfileRoot,
    handshake: DaemonHandshake,
    request: AdminProjectSurfaceRequestV1,
    deadline: Instant,
) -> Result<AdminProjectResultV1> {
    match crate::tool_command::owner_operation_result(
        profile,
        handshake,
        ApplicationSurfaceOperation::AdminProject,
        serde_json::to_value(&request)?,
        deadline,
    )
    .await?
    {
        GraphToolResultV1::AdminProject(result) => Ok(*result),
        _ => Err(unexpected_admin_project_result()),
    }
}

/// The project's local usage counter.
pub(crate) async fn local_counter(
    profile: &ProfileRoot,
    project_path: &std::path::Path,
) -> Result<u64> {
    match admin_project(
        profile,
        project_path,
        AdminProjectSurfaceRequestV1::CounterGet {},
    )
    .await?
    {
        AdminProjectResultV1::Counter(AdminProjectCounterV1 { counter }) => Ok(counter),
        _ => Err(unexpected_admin_project_result()),
    }
}

/// The owner answered an action with another action's result.
pub(crate) fn unexpected_admin_project_result() -> TraceDecayError {
    TraceDecayError::project_route(
        "owner_result_mismatch",
        false,
        "the project owner answered tracedecay_admin_project with another action's result",
    )
}
