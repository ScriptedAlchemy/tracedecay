use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde::Serialize;
use serde_json::json;
use tracedecay_query::search_quality::{DirectEvaluationStatusV1, SearchEvalError};
use tracedecay_search_eval::{
    DirectWorkloadSummaryV1, GenerateCandidateOutputsOptions, compare_default_direct,
    compare_direct, generate_candidate_outputs, root_admitted_corpus_scope,
    validate_default_workload, validate_direct_workload, write_generate_outputs,
};

#[derive(Debug, Parser)]
#[command(
    name = "tracedecay-search-eval",
    about = "Run direct exact/lexical/graph search-quality evaluation"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Validate the checked-in labeled workload. Without `--workload` the
    /// packaged workload is validated from its own materialized root.
    Validate {
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
        #[arg(long)]
        workload: Option<PathBuf>,
    },
    /// Run production retrieval and evaluate checked-in labels directly.
    /// Without `--workload` the packaged workload and corpus are evaluated
    /// from their own materialized root.
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
            default_value = "benchmark_data/search-quality/runs/candidate-outputs"
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
        } => match validate_requested_workload(&repo_root, workload.as_deref()) {
            Ok(summary) => emit(&summary, ExitCode::SUCCESS),
            Err(error) => invalid("validate", error),
        },
        Command::Compare {
            repo_root,
            workload,
            profiles,
        } => match workload.as_deref().map_or_else(
            || compare_default_direct(profiles.as_deref()),
            |workload| {
                compare_direct(
                    &repo_root,
                    Some(workload),
                    profiles.as_deref(),
                    root_admitted_corpus_scope,
                )
            },
        ) {
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
            admitted_scope: root_admitted_corpus_scope,
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

fn validate_requested_workload(
    repo_root: &std::path::Path,
    workload: Option<&std::path::Path>,
) -> Result<DirectWorkloadSummaryV1, SearchEvalError> {
    workload.map_or_else(validate_default_workload, |path| {
        validate_direct_workload(repo_root, Some(path))
    })
}

fn invalid(command: &str, error: impl std::fmt::Display) -> ExitCode {
    emit(
        &json!({
            "command": command,
            "status": "fail",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_validation_binds_the_packaged_workload_identity() {
        let summary = validate_requested_workload(std::path::Path::new("."), None)
            .expect("packaged workload validates");

        assert_eq!(summary.status, DirectEvaluationStatusV1::Pass);
        assert_eq!(
            summary.workload_digest,
            tracedecay_query::search_quality::packaged::WORKLOAD_SHA256,
            "validate must return the packaged workload identity, not a second pin"
        );
        assert_eq!(summary.profile_count, 1);
        assert_eq!(summary.query_count, 67);
    }

    #[test]
    fn compare_parses_a_comma_separated_profile_selection() {
        let cli = Cli::try_parse_from([
            "tracedecay-search-eval",
            "compare",
            "--profiles",
            "query-fallback",
        ])
        .expect("compare profile arguments parse");

        assert!(matches!(
            cli.command,
            Command::Compare {
                repo_root,
                workload: None,
                profiles: Some(profiles),
            } if repo_root == *"." && profiles == ["query-fallback"]
        ));
        assert!(
            Cli::try_parse_from(["tracedecay-search-eval", "evaluate-and-publish"]).is_err(),
            "the daemon publishing route no longer exists"
        );
    }
}
