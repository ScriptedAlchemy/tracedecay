//! CLI presentation for the closed Workflow application binding.

use std::io::Read;

use serde_json::Value;

use crate::cli::WorkflowInvocationArgs;

pub(crate) async fn run(invocation: WorkflowInvocationArgs) -> tracedecay::errors::Result<()> {
    let body = read_request(&invocation.request_file)?;
    let project_root = tracedecay::config::resolve_path_with_discovery(invocation.project);
    let operation = invocation.operation.into_runtime();
    let outcome =
        tracedecay::workflow_cli::invoke_workflow_cli(project_root.clone(), operation, body)
            .await?;
    if invocation.json {
        print!("{}", workflow_json_line(&outcome)?);
    } else {
        println!("Workflow {}", operation.as_str().replace('_', " "));
        println!("Project: {}", project_root.display());
        println!("{}", serde_json::to_string_pretty(&outcome)?);
    }
    Ok(())
}

fn workflow_json_line(outcome: &Value) -> serde_json::Result<String> {
    let mut line = serde_json::to_string(outcome)?;
    line.push('\n');
    Ok(line)
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

    #[test]
    fn workflow_json_line_is_compact_canonical_json() {
        let outcome = serde_json::json!({
            "status": "complete",
            "details": { "count": 2 }
        });

        assert_eq!(
            workflow_json_line(&outcome).expect("workflow JSON line"),
            "{\"details\":{\"count\":2},\"status\":\"complete\"}\n"
        );
    }
}
