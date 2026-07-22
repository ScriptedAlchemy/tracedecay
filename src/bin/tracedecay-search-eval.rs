use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use tracedecay::search_eval::{
    CompareOptions, GenerateCandidateOutputsOptions, compare, generate_candidate_outputs,
    sealed_holdout_label_set_digest, validate_fixture_root, write_generate_outputs,
};
use tracedecay_domain::{
    EvalOutcomeV1, EvidenceIndexV1, RelevanceJudgmentV1, RunManifestV1, SavedCandidateSetV1,
};

#[derive(Debug, Parser)]
#[command(
    name = "tracedecay-search-eval",
    about = "Validate and compare Plan 15 search-quality runs via direct local evaluation"
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
    /// Compute the canonical digest for a run manifest draft.
    RehashRun {
        #[arg(long)]
        input: PathBuf,
    },
    /// Compute the canonical digest for an evidence-index draft.
    RehashEvidence {
        #[arg(long)]
        input: PathBuf,
    },
    /// Compute the canonical digest for one judgment array.
    RehashLabels {
        #[arg(long)]
        input: PathBuf,
    },
    /// Compute the canonical digest for a saved-candidate draft.
    RehashCandidates {
        #[arg(long)]
        input: PathBuf,
    },
    /// Compare one frozen run and emit one immutable terminal outcome.
    Compare {
        #[arg(long, default_value = "tests/fixtures/search_quality")]
        fixtures: PathBuf,
        #[arg(long)]
        run_manifest: Option<PathBuf>,
        #[arg(long, default_value = "benchmarks/search-quality/runs")]
        output_root: PathBuf,
        /// Direct filesystem path to holdout labels for locked evaluation.
        #[arg(long)]
        holdout_labels: Option<PathBuf>,
        #[arg(long)]
        saved_candidates: Option<PathBuf>,
        #[arg(long)]
        require_outcome: Option<OutcomeArg>,
    },
    /// Generate production-bound train/validation candidate outputs plus sealed holdout input.
    GenerateCandidates {
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
        #[arg(long)]
        workload: Option<PathBuf>,
        #[arg(long, default_value = "benchmarks/search-quality/runs/candidate-outputs")]
        output_root: PathBuf,
        #[arg(long, value_delimiter = ',')]
        profiles: Option<Vec<String>>,
        #[arg(long, default_value_t = false)]
        include_holdout_candidates: bool,
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
        Command::RehashRun { input } => emit_operation(
            "rehash_run",
            (|| {
                let run: RunManifestV1 = read_json_file(&input)?;
                let digest = run.compute_digest().map_err(|error| error.to_string())?;
                serde_json::to_value(digest).map_err(|error| error.to_string())
            })(),
        ),
        Command::RehashEvidence { input } => emit_operation(
            "rehash_evidence",
            (|| {
                let index: EvidenceIndexV1 = read_json_file(&input)?;
                let digest = index.compute_digest().map_err(|error| error.to_string())?;
                serde_json::to_value(digest).map_err(|error| error.to_string())
            })(),
        ),
        Command::RehashLabels { input } => emit_operation(
            "rehash_labels",
            (|| {
                let judgments: Vec<RelevanceJudgmentV1> = read_json_file(&input)?;
                let digest = sealed_holdout_label_set_digest(&judgments)
                    .map_err(|error| error.to_string())?;
                serde_json::to_value(digest).map_err(|error| error.to_string())
            })(),
        ),
        Command::RehashCandidates { input } => emit_operation(
            "rehash_candidates",
            (|| {
                let candidates: SavedCandidateSetV1 = read_json_file(&input)?;
                let digest = candidates
                    .compute_digest()
                    .map_err(|error| error.to_string())?;
                serde_json::to_value(digest).map_err(|error| error.to_string())
            })(),
        ),
        Command::Compare {
            fixtures,
            run_manifest,
            output_root,
            holdout_labels,
            saved_candidates,
            require_outcome,
        } => {
            let required_outcome = require_outcome.map(Into::into);
            let options = CompareOptions {
                fixture_root: fixtures,
                run_manifest,
                output_root,
                holdout_labels,
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
        Command::GenerateCandidates {
            repo_root,
            workload,
            output_root,
            profiles,
            include_holdout_candidates,
        } => emit_operation(
            "generate_candidates",
            (|| {
                let result = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
                    repo_root: &repo_root,
                    workload_path: workload.as_deref(),
                    profile_ids: profiles.as_deref(),
                    include_holdout_candidates,
                })
                .map_err(|error| error.to_string())?;
                write_generate_outputs(&output_root, &result).map_err(|error| error.to_string())?;
                serde_json::to_value(json!({
                    "workload_digest": result.workload_digest,
                    "train_validation_outputs": result.train_validation_outputs.len(),
                    "sealed_holdout_queries": result.sealed_holdout_input.queries.len(),
                    "holdout_labels_included": result.sealed_holdout_input.holdout_labels_included,
                    "output_root": output_root,
                }))
                .map_err(|error| error.to_string())
            })(),
        ),
    }
}

fn read_json_file<T: DeserializeOwned>(path: &std::path::Path) -> Result<T, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn emit_operation(command: &'static str, result: Result<serde_json::Value, String>) -> ExitCode {
    match result {
        Ok(value) => emit(
            &json!({"command": command, "status": "ok", "result": value}),
            ExitCode::SUCCESS,
        ),
        Err(error) => emit(
            &json!({"command": command, "status": "invalid", "rationale": error}),
            ExitCode::from(2),
        ),
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
