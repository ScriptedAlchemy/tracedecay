use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde::Serialize;
use serde_json::json;
use tracedecay::application::semantic_runtime::SemanticEvaluationProfileCandidateV1;
use tracedecay::daemon::DaemonHandshake;
use tracedecay::daemon_client::DaemonInvocationClient;
use tracedecay::search_eval::{
    DirectEvaluationStatusV1, DirectWorkloadSummaryV1, GenerateCandidateOutputsOptions,
    SearchEvalError, compare_direct, generate_candidate_outputs, root_admitted_corpus_scope,
    validate_default_activation_workload, validate_direct_workload, write_generate_outputs,
};

#[derive(Debug, Parser)]
#[command(
    name = "tracedecay-search-eval",
    about = "Run direct query/semantic search-quality evaluation"
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
    /// Run the real direct evaluator in the owning daemon and publish only a
    /// passing profile bound to the daemon's current scope and generations.
    EvaluateAndPublish {
        #[arg(long, default_value = ".")]
        project_root: PathBuf,
        #[arg(long)]
        candidate: PathBuf,
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
        } => match compare_direct(
            &repo_root,
            workload.as_deref(),
            profiles.as_deref(),
            root_admitted_corpus_scope,
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
        Command::EvaluateAndPublish {
            project_root,
            candidate,
        } => evaluate_and_publish(project_root, candidate),
    }
}

fn validate_requested_workload(
    repo_root: &std::path::Path,
    workload: Option<&std::path::Path>,
) -> Result<DirectWorkloadSummaryV1, SearchEvalError> {
    workload.map_or_else(
        || validate_default_activation_workload(repo_root),
        |path| validate_direct_workload(repo_root, Some(path)),
    )
}

fn evaluate_and_publish(project_root: PathBuf, candidate_path: PathBuf) -> ExitCode {
    let candidate = match std::fs::read(&candidate_path)
        .map_err(|error| format!("read {}: {error}", candidate_path.display()))
        .and_then(|bytes| {
            serde_json::from_slice::<SemanticEvaluationProfileCandidateV1>(&bytes)
                .map_err(|error| format!("parse {}: {error}", candidate_path.display()))
        }) {
        Ok(candidate) => candidate,
        Err(error) => return invalid("evaluate_and_publish", error),
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => return invalid("evaluate_and_publish", error),
    };
    runtime.block_on(async move {
        let handshake =
            match DaemonHandshake::for_current_client(Some(project_root), None, false, false) {
                Ok(handshake) => handshake,
                Err(error) => return invalid("evaluate_and_publish", error),
            };
        let client = match DaemonInvocationClient::for_current(handshake) {
            Ok(client) => client,
            Err(error) => return invalid("evaluate_and_publish", error),
        };
        match client
            .evaluate_and_publish_semantic_profile(candidate)
            .await
        {
            Ok(result) => emit(&result, ExitCode::SUCCESS),
            Err(error) => invalid("evaluate_and_publish", error),
        }
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
    fn default_validation_uses_byte_pinned_activation_workload() {
        let summary = validate_requested_workload(&PathBuf::from(env!("CARGO_MANIFEST_DIR")), None)
            .expect("checked-in activation workload validates");

        assert_eq!(summary.status, DirectEvaluationStatusV1::Pass);
        assert_eq!(
            summary.workload_digest,
            "sha256:a8e1def7179a2aa8f490676724514b4284aaf727f7f9e501eda3b3e6554b1347"
        );
        assert_eq!(summary.profile_count, 3);
    }
}
