use std::path::PathBuf;

use clap::{
    Args,
    builder::{PossibleValuesParser, TypedValueParser},
};
use tracedecay_api::WorkOperation;

fn work_operation_parser() -> impl TypedValueParser<Value = WorkOperation> {
    PossibleValuesParser::new(WorkOperation::ALL.map(WorkOperation::route_segment))
        .try_map(|segment| segment.parse::<WorkOperation>())
}

#[derive(Args)]
pub struct WorkInvocationArgs {
    /// Closed Work operation to invoke.
    #[arg(value_parser = work_operation_parser())]
    pub operation: WorkOperation,
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
