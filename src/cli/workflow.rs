use std::path::PathBuf;

use clap::{Args, ValueEnum};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum WorkflowCliOperationArg {
    RegisterDefinition,
    ValidateDefinition,
    GetDefinition,
    ListDefinitions,
    DefinitionHistory,
    DiffDefinition,
    ActivateDefinition,
    RetireDefinition,
    ExecuteFanOut,
    HandoffIssue,
    HandoffRedeem,
}

impl WorkflowCliOperationArg {
    pub fn into_runtime(self) -> tracedecay::workflow_cli::WorkflowCliOperation {
        match self {
            Self::RegisterDefinition => {
                tracedecay::workflow_cli::WorkflowCliOperation::RegisterDefinition
            }
            Self::ValidateDefinition => {
                tracedecay::workflow_cli::WorkflowCliOperation::ValidateDefinition
            }
            Self::GetDefinition => tracedecay::workflow_cli::WorkflowCliOperation::GetDefinition,
            Self::ListDefinitions => {
                tracedecay::workflow_cli::WorkflowCliOperation::ListDefinitions
            }
            Self::DefinitionHistory => {
                tracedecay::workflow_cli::WorkflowCliOperation::DefinitionHistory
            }
            Self::DiffDefinition => tracedecay::workflow_cli::WorkflowCliOperation::DiffDefinition,
            Self::ActivateDefinition => {
                tracedecay::workflow_cli::WorkflowCliOperation::ActivateDefinition
            }
            Self::RetireDefinition => {
                tracedecay::workflow_cli::WorkflowCliOperation::RetireDefinition
            }
            Self::ExecuteFanOut => tracedecay::workflow_cli::WorkflowCliOperation::ExecuteFanOut,
            Self::HandoffIssue => tracedecay::workflow_cli::WorkflowCliOperation::HandoffIssue,
            Self::HandoffRedeem => tracedecay::workflow_cli::WorkflowCliOperation::HandoffRedeem,
        }
    }
}

#[derive(Args)]
pub struct WorkflowInvocationArgs {
    /// Closed Workflow operation to invoke.
    #[arg(value_enum)]
    pub operation: WorkflowCliOperationArg,
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
