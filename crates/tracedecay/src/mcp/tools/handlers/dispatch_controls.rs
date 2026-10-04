//! The graph-tool owner's automation-ledger, managed-skill, Hermes-inventory
//! and analytics reads, computed under the authorities the serving MCP server
//! admitted.

use serde_json::Value;
use tracedecay_contracts::graph_tool::GraphToolCompletionV1;
use tracedecay_contracts::{CancellationSignal, Deadline};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_mcp::handlers::analytics::{AnalyticsAuthority, compute_analytics};
use tracedecay_mcp::handlers::automation_runs::{
    compute_run_artifact_view, compute_run_list, compute_run_view,
};
use tracedecay_mcp::handlers::skills::{
    SkillReadAuthority, compute_hermes_skill_bridge, compute_skill_list, compute_skill_view,
};
use tracedecay_mcp::handlers::unknown_tool_error;
use tracedecay_project::project::TraceDecay;
use tracedecay_runtime_core::config::ProfileRoot;
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

use super::ToolCallRegistryOptions;

/// Operations the owner answers from the ledger, the profile skill store, the
/// Hermes install or the analytics stores instead of the code graph.
pub(super) fn is_automation_read(operation: ApplicationSurfaceOperation) -> bool {
    matches!(
        operation,
        ApplicationSurfaceOperation::AutomationRunList
            | ApplicationSurfaceOperation::AutomationRunView
            | ApplicationSurfaceOperation::AutomationRunArtifactView
            | ApplicationSurfaceOperation::SkillList
            | ApplicationSurfaceOperation::SkillView
            | ApplicationSurfaceOperation::HermesSkillBridge
            | ApplicationSurfaceOperation::Analytics
    )
}

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

#[tracing::instrument(name = "mcp.dispatch.automation_read", level = "trace", skip_all)]
pub(super) async fn compute_automation_read(
    cg: &TraceDecay,
    operation: ApplicationSurfaceOperation,
    args: &Value,
    options: &ToolCallRegistryOptions<'_>,
) -> Result<GraphToolCompletionV1> {
    let dashboard_root = cg.store_layout().dashboard_root.as_path();
    let profile_root = options.profile.map(ProfileRoot::data_dir);
    let skills = SkillReadAuthority {
        profile_root,
        project_root: cg.project_root(),
        analytics_db: options.accounting_db,
    };
    match operation {
        ApplicationSurfaceOperation::AutomationRunList => {
            compute_run_list(dashboard_root, args).await
        }
        ApplicationSurfaceOperation::AutomationRunView => {
            compute_run_view(dashboard_root, args).await
        }
        ApplicationSurfaceOperation::AutomationRunArtifactView => {
            compute_run_artifact_view(dashboard_root, args).await
        }
        ApplicationSurfaceOperation::SkillList => compute_skill_list(&skills, args).await,
        ApplicationSurfaceOperation::SkillView => compute_skill_view(&skills, args).await,
        ApplicationSurfaceOperation::HermesSkillBridge => {
            compute_hermes_skill_bridge(options.profile.and_then(ProfileRoot::home), args)
        }
        ApplicationSurfaceOperation::Analytics => {
            let (deadline, cancellation) = admitted_control(options, "analytics")?;
            compute_analytics(
                cg,
                args,
                AnalyticsAuthority {
                    profile_root,
                    analytics_db: options.global_db.map(RegisteredGlobalDbLeaseV1::as_ref),
                    project_sessions: options
                        .session_authorities
                        .project
                        .map(RegisteredGlobalDbLeaseV1::as_ref),
                    deadline,
                    cancellation,
                },
            )
            .await
        }
        operation => Err(unknown_tool_error(operation.mcp_tool_name())),
    }
}
