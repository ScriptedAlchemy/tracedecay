//! CLI presentation for the closed Workflow application binding.

use std::io::Read;

use serde_json::Value;
use tracedecay_contracts::ApplicationResult;

use crate::cli::WorkflowInvocationArgs;

#[hotpath::measure(label = "cli.workflow.invoke", future = true)]
pub(crate) async fn run(
    invocation: WorkflowInvocationArgs,
) -> tracedecay_domain::errors::Result<()> {
    #[cfg(feature = "hotpath")]
    hotpath::val!("cli.workflow.operation").set(&invocation.operation.operation_key());
    let body = read_request(&invocation.request_file)?;
    let project_root = tracedecay_configuration::resolve_path_with_discovery(invocation.project);
    let operation = invocation.operation;
    let outcome =
        crate::workflow_cli::invoke_workflow_cli(project_root.clone(), operation, body).await?;
    if invocation.json {
        print!("{}", workflow_json_line(&outcome)?);
    } else {
        let outcome =
            outcome.map_err(
                |problem| tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!("{}: {}", problem.problem.code, problem.problem.message),
                },
            )?;
        println!("Workflow {}", operation.route_segment().replace('-', " "));
        println!("Project: {}", project_root.display());
        println!("{}", serde_json::to_string_pretty(&outcome)?);
    }
    Ok(())
}

fn workflow_json_line(outcome: &ApplicationResult<Value>) -> serde_json::Result<String> {
    crate::cli::output::json::json_line(outcome)
}

fn read_request(path: &std::path::Path) -> tracedecay_domain::errors::Result<Value> {
    let payload = if path == std::path::Path::new("-") {
        let mut payload = String::new();
        std::io::stdin().read_to_string(&mut payload)?;
        payload
    } else {
        std::fs::read_to_string(path)?
    };
    serde_json::from_str(&payload).map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "Workflow request file {} is not valid JSON: {error}",
                path.display()
            ),
        }
    })
}
