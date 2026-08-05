//! CLI presentation for the closed Workflow application binding.

use std::io::Read;

use serde::Serialize;
use serde_json::Value;
use tracedecay_application::ApplicationResult;

use crate::cli::WorkflowInvocationArgs;
use tracedecay::workflow_cli::WorkflowCliInvocationResult;

pub(crate) async fn run(invocation: WorkflowInvocationArgs) -> tracedecay::errors::Result<()> {
    let body = read_request(&invocation.request_file)?;
    let project_root = tracedecay::config::resolve_path_with_discovery(invocation.project);
    let operation = invocation.operation;
    let outcome =
        tracedecay::workflow_cli::invoke_workflow_cli(project_root.clone(), operation, body)
            .await?;
    if invocation.json {
        print!("{}", json_line(&outcome)?);
    } else {
        println!("Workflow {}", operation.route_segment().replace('-', " "));
        println!("Project: {}", project_root.display());
        println!("{}", pretty_json(&outcome)?);
    }
    if let Some(message) = application_problem_message(&outcome) {
        return Err(tracedecay::errors::TraceDecayError::Config { message });
    }
    Ok(())
}

fn json_line(outcome: &WorkflowCliInvocationResult) -> serde_json::Result<String> {
    match outcome {
        WorkflowCliInvocationResult::RegisterDefinition(result) => {
            crate::cli::output::json::json_line(result)
        }
        WorkflowCliInvocationResult::ActivateDefinition(result) => {
            crate::cli::output::json::json_line(result)
        }
        WorkflowCliInvocationResult::ExecuteFanOut(result) => {
            crate::cli::output::json::json_line(result)
        }
        WorkflowCliInvocationResult::HandoffIssue(result) => {
            crate::cli::output::json::json_line(result)
        }
        WorkflowCliInvocationResult::HandoffRedeem(result) => {
            crate::cli::output::json::json_line(result)
        }
    }
}

fn pretty_json(outcome: &WorkflowCliInvocationResult) -> serde_json::Result<String> {
    match outcome {
        WorkflowCliInvocationResult::RegisterDefinition(result) => pretty_result(result),
        WorkflowCliInvocationResult::ActivateDefinition(result) => pretty_result(result),
        WorkflowCliInvocationResult::ExecuteFanOut(result) => pretty_result(result),
        WorkflowCliInvocationResult::HandoffIssue(result) => pretty_result(result),
        WorkflowCliInvocationResult::HandoffRedeem(result) => pretty_result(result),
    }
}

fn pretty_result<T: Serialize>(result: &ApplicationResult<T>) -> serde_json::Result<String> {
    match result {
        Ok(envelope) => serde_json::to_string_pretty(envelope),
        Err(envelope) => serde_json::to_string_pretty(envelope),
    }
}

fn application_problem_message(outcome: &WorkflowCliInvocationResult) -> Option<String> {
    let problem = match outcome {
        WorkflowCliInvocationResult::RegisterDefinition(result) => result.as_ref().err(),
        WorkflowCliInvocationResult::ActivateDefinition(result) => result.as_ref().err(),
        WorkflowCliInvocationResult::ExecuteFanOut(result) => result.as_ref().err(),
        WorkflowCliInvocationResult::HandoffIssue(result) => result.as_ref().err(),
        WorkflowCliInvocationResult::HandoffRedeem(result) => result.as_ref().err(),
    }?;
    Some(format!(
        "{}: {}",
        problem.problem.code, problem.problem.message
    ))
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
