//! CLI presentation for the closed Workflow application binding.

use crate::cli::WorkflowInvocationArgs;
use tracedecay_runtime_core::config::ProfileRoot;

#[tracing::instrument(name = "cli.workflow.invoke", level = "trace", skip_all)]
pub(crate) async fn run(
    profile: &ProfileRoot,
    invocation: WorkflowInvocationArgs,
) -> tracedecay_domain::errors::Result<()> {
    tracing::trace!(name: "cli.workflow.operation", value = ?invocation.operation.operation_key());
    let body = crate::application_cli::read_request(
        &invocation.request_file,
        crate::application_cli::WORKFLOW,
    )?;
    let project_root =
        tracedecay_configuration::resolve_path_with_discovery(profile, invocation.project);
    let operation = invocation.operation;
    let outcome =
        crate::workflow_cli::invoke_workflow_cli(profile, project_root.clone(), operation, body)
            .await?;
    print!(
        "{}",
        crate::application_cli::render(
            crate::application_cli::WORKFLOW,
            operation.route_segment(),
            &project_root,
            &outcome,
            invocation.json,
        )?
    );
    crate::application_cli::refused(
        crate::application_cli::WORKFLOW,
        operation.route_segment(),
        &outcome,
    )
}
