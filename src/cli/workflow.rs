use std::path::PathBuf;

use clap::{
    Args,
    builder::{PossibleValuesParser, TypedValueParser},
};
use tracedecay_api::WorkflowOperation;

fn workflow_operation_parser() -> impl TypedValueParser<Value = WorkflowOperation> {
    PossibleValuesParser::new(WorkflowOperation::ALL.map(WorkflowOperation::route_segment))
        .try_map(|segment| segment.parse::<WorkflowOperation>())
}

#[derive(Args)]
pub struct WorkflowInvocationArgs {
    /// Closed Workflow operation to invoke.
    #[arg(value_parser = workflow_operation_parser())]
    pub operation: WorkflowOperation,
    /// Strict typed request JSON file, or `-` to read it from stdin.
    #[arg(long, value_name = "FILE")]
    pub request_file: PathBuf,
    /// Project root; defaults to the nearest initialized project.
    #[arg(long)]
    pub project: Option<String>,
    /// Emit one canonical JSON object and newline.
    #[arg(long)]
    pub json: bool,
}
