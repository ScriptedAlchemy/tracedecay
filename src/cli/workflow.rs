use std::path::PathBuf;

use clap::Args;
use tracedecay_api::WorkflowOperation;

#[derive(Args)]
pub struct WorkflowInvocationArgs {
    /// Closed Workflow operation to invoke.
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
