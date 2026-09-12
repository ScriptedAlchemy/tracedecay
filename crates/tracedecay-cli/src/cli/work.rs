use std::path::PathBuf;
use std::str::FromStr;

use clap::{
    Args,
    builder::{PossibleValuesParser, TypedValueParser},
};
use tracedecay_api::{WorkOperation, WorkflowOperation};

pub(super) trait ApplicationOperation:
    Clone + Send + Sync + 'static + FromStr<Err = String>
{
    const HELP: &'static str;

    fn values() -> PossibleValuesParser;
}

fn operation_parser<O: ApplicationOperation>() -> impl TypedValueParser<Value = O> {
    O::values().try_map(|segment| segment.parse::<O>())
}

#[derive(Args)]
pub struct ApplicationInvocationArgs<O: ApplicationOperation> {
    #[arg(value_parser = operation_parser::<O>(), help = O::HELP)]
    pub operation: O,
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

impl ApplicationOperation for WorkOperation {
    const HELP: &'static str = "Closed Work operation to invoke";

    fn values() -> PossibleValuesParser {
        PossibleValuesParser::new(WorkOperation::ALL.map(WorkOperation::route_segment))
    }
}

impl ApplicationOperation for WorkflowOperation {
    const HELP: &'static str = "Closed Workflow operation to invoke";

    fn values() -> PossibleValuesParser {
        PossibleValuesParser::new(WorkflowOperation::ALL.map(WorkflowOperation::route_segment))
    }
}

pub type WorkInvocationArgs = ApplicationInvocationArgs<WorkOperation>;
