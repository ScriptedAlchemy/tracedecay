use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde::Serialize;
use serde_json::json;
use tracedecay::search_eval::{
    DirectEvaluationStatusV1, GenerateCandidateOutputsOptions, compare_direct,
    generate_candidate_outputs, validate_direct_workload, write_generate_outputs,
};

#[derive(Debug, Parser)]
#[command(
    name = "tracedecay-search-eval",
    about = "Run direct PR9/PR10 search-quality evaluation"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Validate the checked-in labeled workload.
    Validate {
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
        #[arg(long)]
        workload: Option<PathBuf>,
    },
    /// Run production retrieval and evaluate checked-in labels directly.
    Compare {
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
        #[arg(long)]
        workload: Option<PathBuf>,
        #[arg(long, value_delimiter = ',')]
        profiles: Option<Vec<String>>,
    },
    /// Generate ordinary local candidate and resource outputs.
    GenerateCandidates {
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
        #[arg(long)]
        workload: Option<PathBuf>,
        #[arg(
            long,
            default_value = "benchmarks/search-quality/runs/candidate-outputs"
        )]
        output_root: PathBuf,
        #[arg(long, value_delimiter = ',')]
        profiles: Option<Vec<String>>,
    },
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Validate {
            repo_root,
            workload,
        } => match validate_direct_workload(&repo_root, workload.as_deref()) {
            Ok(summary) => emit(&summary, ExitCode::SUCCESS),
            Err(error) => invalid("validate", error),
        },
        Command::Compare {
            repo_root,
            workload,
            profiles,
        } => match compare_direct(&repo_root, workload.as_deref(), profiles.as_deref()) {
            Ok(report) => {
                let exit = if report.status == DirectEvaluationStatusV1::Pass {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(1)
                };
                emit(&report, exit)
            }
            Err(error) => invalid("compare", error),
        },
        Command::GenerateCandidates {
            repo_root,
            workload,
            output_root,
            profiles,
        } => match generate_candidate_outputs(&GenerateCandidateOutputsOptions {
            repo_root: &repo_root,
            workload_path: workload.as_deref(),
            profile_ids: profiles.as_deref(),
        }) {
            Ok(result) => match write_generate_outputs(&output_root, &result) {
                Ok(()) => emit(
                    &json!({
                        "command": "generate_candidates",
                        "status": "recorded",
                        "workload_digest": result.workload_digest,
                        "outputs": result.outputs.len(),
                        "output_root": output_root,
                    }),
                    ExitCode::SUCCESS,
                ),
                Err(error) => invalid("generate_candidates", error),
            },
            Err(error) => invalid("generate_candidates", error),
        },
    }
}

fn invalid(command: &str, error: impl std::fmt::Display) -> ExitCode {
    emit(
        &json!({
            "command": command,
            "status": "invalid",
            "rationale": error.to_string(),
        }),
        ExitCode::from(2),
    )
}

fn emit(value: &impl Serialize, exit: ExitCode) -> ExitCode {
    match serde_json::to_string_pretty(value) {
        Ok(json) => println!("{json}"),
        Err(error) => {
            eprintln!("serialize evaluator output: {error}");
            return ExitCode::from(2);
        }
    }
    exit
}
