//! CLI presentation for the closed Workflow application binding.

use std::io::Read;

use serde_json::Value;
use tracedecay_application::ApplicationResult;

use crate::cli::WorkflowInvocationArgs;

pub(crate) async fn run(invocation: WorkflowInvocationArgs) -> tracedecay::errors::Result<()> {
    let body = read_request(&invocation.request_file)?;
    let project_root = tracedecay::config::resolve_path_with_discovery(invocation.project);
    let operation = invocation.operation;
    let outcome =
        tracedecay::workflow_cli::invoke_workflow_cli(project_root.clone(), operation, body)
            .await?;
    if invocation.json {
        print!("{}", workflow_json_line(&outcome)?);
    } else {
        let outcome = outcome.map_err(|problem| tracedecay::errors::TraceDecayError::Config {
            message: format!("{}: {}", problem.problem.code, problem.problem.message),
        })?;
        println!("Workflow {}", operation.route_segment().replace('-', " "));
        println!("Project: {}", project_root.display());
        println!("{}", serde_json::to_string_pretty(&outcome)?);
    }
    Ok(())
}

fn workflow_json_line(outcome: &ApplicationResult<Value>) -> serde_json::Result<String> {
    crate::cli::output::json::json_line(outcome)
}

fn read_request(path: &std::path::Path) -> tracedecay::errors::Result<Value> {
    let payload = if path == std::path::Path::new("-") {
        let mut payload = String::new();
        std::io::stdin().read_to_string(&mut payload)?;
        payload
    } else {
        std::fs::read_to_string(path)?
    };
    serde_json::from_str(&payload).map_err(|error| tracedecay::errors::TraceDecayError::Config {
        message: format!(
            "Workflow request file {} is not valid JSON: {error}",
            path.display()
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::workflow_json_line;
    use tracedecay_application::{
        ApplicationProblem, ApplicationProblemEnvelope, ApplicationResult, RequestId,
        ResultContractRef, RetryDirective,
    };
    use tracedecay_tool_catalog::SchemaId;

    #[test]
    fn workflow_json_line_preserves_the_canonical_typed_problem() {
        let outcome: ApplicationResult<serde_json::Value> = Err(ApplicationProblemEnvelope::new(
            ResultContractRef::new(
                SchemaId::new("schema.workflow.handoff_redeem.result").unwrap(),
                1,
            )
            .unwrap(),
            RequestId::new("request.cli.workflow.7").unwrap(),
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
        )
        .expect("construct canonical workflow problem fixture"));

        let rendered = workflow_json_line(&outcome).expect("workflow JSON line");
        let problem: serde_json::Value =
            serde_json::from_str(rendered.trim_end()).expect("typed workflow problem JSON");
        assert_eq!(
            problem["contract"]["schema_id"],
            "schema.workflow.handoff_redeem.result"
        );
        assert_eq!(problem["contract"]["schema_revision"], 1);
        assert_eq!(problem["request_id"], "request.cli.workflow.7");
        assert_eq!(problem["problem"]["kind"], "not_found_or_not_authorized");
        assert_eq!(rendered.lines().count(), 1);
    }
}
