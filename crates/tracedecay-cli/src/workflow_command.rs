//! CLI presentation for the closed Workflow application binding.

use crate::cli::WorkflowInvocationArgs;

#[hotpath::measure(label = "cli.workflow.invoke", future = true)]
pub(crate) async fn run(
    invocation: WorkflowInvocationArgs,
) -> tracedecay_domain::errors::Result<()> {
    #[cfg(feature = "hotpath")]
    hotpath::val!("cli.workflow.operation").set(&invocation.operation.operation_key());
    let body = crate::work_cli::application_cli::read_request(
        &invocation.request_file,
        crate::work_cli::application_cli::WORKFLOW,
    )?;
    let project_root = tracedecay_configuration::resolve_path_with_discovery(invocation.project);
    let operation = invocation.operation;
    let outcome =
        crate::workflow_cli::invoke_workflow_cli(project_root.clone(), operation, body).await?;
    print!(
        "{}",
        crate::work_cli::application_cli::render(
            crate::work_cli::application_cli::WORKFLOW,
            operation.route_segment(),
            &project_root,
            &outcome,
            invocation.json,
        )?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn workflow_json_line_preserves_the_canonical_typed_problem() {
        crate::work_cli::application_cli::tests::assert_json_problem(
            "schema.workflow.handoff_redeem.result",
            "request.cli.workflow.7",
        );
    }
}
