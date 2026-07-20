use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::json;
use tracedecay::search_eval::{CompareOptions, compare, validate_fixture_root};
use tracedecay_domain::EvalOutcomeV1;

#[derive(Debug, Parser)]
#[command(
    name = "tracedecay-search-eval",
    about = "Validate and compare frozen Plan 15 search-quality runs"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Validate all checked-in fixture bytes and canonical digests.
    Validate {
        #[arg(long, default_value = "tests/fixtures/search_quality")]
        fixtures: PathBuf,
    },
    /// Compare one frozen run and emit one immutable terminal outcome.
    Compare {
        #[arg(long, default_value = "tests/fixtures/search_quality")]
        fixtures: PathBuf,
        #[arg(long)]
        run_manifest: Option<PathBuf>,
        #[arg(long, default_value = "benchmarks/search-quality/runs")]
        output_root: PathBuf,
        #[arg(long)]
        holdout_capability: Option<PathBuf>,
        #[arg(long)]
        saved_candidates: Option<PathBuf>,
        #[arg(long)]
        require_outcome: Option<OutcomeArg>,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
#[value(rename_all = "snake_case")]
enum OutcomeArg {
    InvalidRun,
    Blocked,
    Rejected,
    Inconclusive,
    RuntimeFallbackObserved,
    Accepted,
}

impl From<OutcomeArg> for EvalOutcomeV1 {
    fn from(value: OutcomeArg) -> Self {
        match value {
            OutcomeArg::InvalidRun => Self::InvalidRun,
            OutcomeArg::Blocked => Self::Blocked,
            OutcomeArg::Rejected => Self::Rejected,
            OutcomeArg::Inconclusive => Self::Inconclusive,
            OutcomeArg::RuntimeFallbackObserved => Self::RuntimeFallbackObserved,
            OutcomeArg::Accepted => Self::Accepted,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Validate { fixtures } => match validate_fixture_root(&fixtures) {
            Ok(summary) => emit(&summary, ExitCode::SUCCESS),
            Err(error) => emit(
                &json!({
                    "command": "validate",
                    "status": "invalid",
                    "outcome": EvalOutcomeV1::InvalidRun,
                    "rationale": error.to_string(),
                }),
                ExitCode::from(2),
            ),
        },
        Command::Compare {
            fixtures,
            run_manifest,
            output_root,
            holdout_capability,
            saved_candidates,
            require_outcome,
        } => {
            let required_outcome = require_outcome.map(Into::into);
            let options = CompareOptions {
                fixture_root: fixtures,
                run_manifest,
                output_root,
                holdout_capability,
                saved_candidates,
                required_outcome,
            };
            match compare(&options) {
                Ok(result) => {
                    let exit = if result.requirement_satisfied {
                        ExitCode::SUCCESS
                    } else {
                        outcome_exit(result.outcome)
                    };
                    emit(&result, exit)
                }
                Err(error) => emit(
                    &json!({
                        "command": "compare",
                        "outcome": EvalOutcomeV1::InvalidRun,
                        "required_outcome": required_outcome,
                        "requirement_satisfied": required_outcome == Some(EvalOutcomeV1::InvalidRun),
                        "rationale": error.to_string(),
                    }),
                    ExitCode::from(2),
                ),
            }
        }
    }
}

fn outcome_exit(outcome: EvalOutcomeV1) -> ExitCode {
    match outcome {
        EvalOutcomeV1::Accepted => ExitCode::SUCCESS,
        EvalOutcomeV1::InvalidRun => ExitCode::from(2),
        EvalOutcomeV1::Blocked => ExitCode::from(3),
        EvalOutcomeV1::Rejected => ExitCode::from(4),
        EvalOutcomeV1::Inconclusive => ExitCode::from(5),
        EvalOutcomeV1::RuntimeFallbackObserved => ExitCode::from(6),
    }
}

fn emit<T: Serialize>(value: &T, success: ExitCode) -> ExitCode {
    let mut stdout = std::io::stdout().lock();
    if serde_json::to_writer_pretty(&mut stdout, value)
        .and_then(|()| {
            use std::io::Write as _;
            stdout.write_all(b"\n").map_err(serde_json::Error::io)
        })
        .is_err()
    {
        eprintln!("failed to write evaluator result");
        ExitCode::from(2)
    } else {
        success
    }
}
