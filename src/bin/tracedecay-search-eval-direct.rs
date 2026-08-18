use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde::Serialize;
use serde_json::json;
use tracedecay::daemon::DaemonHandshake;
use tracedecay::daemon_client::{
    DaemonInvocationClient, SEMANTIC_EVALUATION_ISOLATED_DISPATCH_DEADLINE_MICROS,
};
use tracedecay::search_eval::{
    DirectEvaluationStatusV1, DirectWorkloadSummaryV1, GenerateCandidateOutputsOptions,
    NativeQualificationExecutionResourceKeyV1, NativeQualificationExpectationsV1,
    NativeQualificationModelKeyV1, NativeQualificationPlatformV1, NativeQualificationRuntimeKeyV1,
    SearchEvalError, compare_default_direct, compare_direct, generate_candidate_outputs,
    root_admitted_corpus_scope, validate_default_activation_workload, validate_direct_workload,
    write_generate_outputs, write_packaged_native_qualification,
};
use tracedecay_application::CancellationSignal;
use tracedecay_usecases::semantic_runtime::SemanticEvaluationProfileCandidateV1;

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
    /// Run the native evaluator in the owning daemon and write only its
    /// independently validated qualification evidence.
    QualifyNative {
        #[arg(long, default_value = ".")]
        project_root: PathBuf,
        #[arg(long)]
        candidate: PathBuf,
        #[arg(long)]
        output: PathBuf,
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
            || compare_default_direct(&repo_root, profiles.as_deref()),
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
        Command::EvaluateAndPublish {
            project_root,
            candidate,
        } => evaluate_and_publish(project_root, candidate),
        Command::QualifyNative {
            project_root,
            candidate,
            output,
        } => qualify_native(project_root, candidate, output),
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
    let candidate = match read_semantic_candidate(&candidate_path) {
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
            .evaluate_and_publish_semantic_profile_until(
                candidate,
                SEMANTIC_EVALUATION_ISOLATED_DISPATCH_DEADLINE_MICROS,
            )
            .await
        {
            Ok(result) => emit(&result, ExitCode::SUCCESS),
            Err(error) => invalid("evaluate_and_publish", error),
        }
    })
}

fn qualify_native(project_root: PathBuf, candidate_path: PathBuf, output: PathBuf) -> ExitCode {
    let candidate = match read_semantic_candidate(&candidate_path) {
        Ok(candidate) => candidate,
        Err(error) => return invalid("qualify_native", error),
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => return invalid("qualify_native", error),
    };
    runtime.block_on(async move {
        let handshake =
            match DaemonHandshake::for_current_client(Some(project_root), None, false, false) {
                Ok(handshake) => handshake,
                Err(error) => return invalid("qualify_native", error),
            };
        let client = match DaemonInvocationClient::for_current(handshake) {
            Ok(client) => client,
            Err(error) => return invalid("qualify_native", error),
        };
        let cancellation =
            match CancellationSignal::active("cancellation.semantic-qualification.cli") {
                Ok(cancellation) => cancellation,
                Err(error) => return invalid("qualify_native", error),
            };
        match client
            .qualify_semantic_profile_until(
                candidate.clone(),
                SEMANTIC_EVALUATION_ISOLATED_DISPATCH_DEADLINE_MICROS,
                cancellation,
            )
            .await
        {
            Ok(result) => {
                let expectations = match qualification_expectations(&candidate) {
                    Ok(expectations) => expectations,
                    Err(error) => return invalid("qualify_native", error),
                };
                match write_native_qualification(
                    &output,
                    &result.qualification_bytes,
                    &expectations,
                ) {
                    Ok(()) => emit(
                        &json!({
                            "command": "qualify_native",
                            "status": "qualified",
                            "evaluated_profile_id": candidate.evaluated_profile_id,
                            "output": output,
                        }),
                        ExitCode::SUCCESS,
                    ),
                    Err(error) => invalid("qualify_native", error),
                }
            }
            Err(error) => invalid("qualify_native", error),
        }
    })
}

fn read_semantic_candidate(
    candidate_path: &std::path::Path,
) -> Result<SemanticEvaluationProfileCandidateV1, String> {
    std::fs::read(candidate_path)
        .map_err(|error| format!("read {}: {error}", candidate_path.display()))
        .and_then(|bytes| {
            serde_json::from_slice::<SemanticEvaluationProfileCandidateV1>(&bytes)
                .map_err(|error| format!("parse {}: {error}", candidate_path.display()))
        })
}

fn qualification_expectations(
    candidate: &SemanticEvaluationProfileCandidateV1,
) -> Result<NativeQualificationExpectationsV1, String> {
    let semantic =
        candidate.compatibility.semantic.as_ref().ok_or_else(|| {
            "candidate does not contain admitted semantic runtime pins".to_owned()
        })?;
    let runtime = NativeQualificationRuntimeKeyV1 {
        implementation_revision: semantic.implementation_revision.clone(),
        fusion_revision: semantic.fusion_revision.clone(),
        runtime_compatibility_digest: semantic.runtime_compatibility_digest.clone(),
        model: NativeQualificationModelKeyV1::from_admitted_projection(&semantic.projection),
        search_index_key: semantic.search_index_key.clone(),
        execution_resources: NativeQualificationExecutionResourceKeyV1 {
            model_bytes: semantic.resources.model_bytes,
            tokenizer_bytes: semantic.resources.tokenizer_bytes,
            threads: semantic.resources.threads,
            max_concurrent_sessions: semantic.resources.max_concurrent_sessions,
            batch_size: semantic.resources.batch_size,
            sequence_length: semantic.resources.sequence_length,
            load_deadline_ms: semantic.resources.load_deadline_ms,
        },
    };
    NativeQualificationExpectationsV1::packaged_default(
        candidate.evaluated_profile_id.clone(),
        runtime,
        NativeQualificationPlatformV1::current(),
    )
    .map_err(|error| error.to_string())
}

fn write_native_qualification(
    output: &std::path::Path,
    qualification_bytes: &[u8],
    expectations: &NativeQualificationExpectationsV1,
) -> Result<(), String> {
    write_packaged_native_qualification(output, qualification_bytes, expectations)
        .map_err(|error| error.to_string())
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

    fn qualification_expectations_for_corrupt_bytes() -> NativeQualificationExpectationsV1 {
        let runtime = serde_json::from_value(serde_json::json!({
            "implementation_revision": "semantic.qualification-cli-test.v1",
            "fusion_revision": "fusion.qualification-cli-test.v1",
            "runtime_compatibility_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "model": {
                "model_artifact_digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "tokenizer_digest": "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                "config_digest": "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
                "query_instruction_digest": null,
                "document_instruction_digest": null,
                "pooling": "mean",
                "truncation_side": "right",
                "truncation_length": 1,
                "inference_batch_size": 1,
                "inference_batch_bytes": 4,
                "runtime_backend": "qualification-cli-test",
                "runtime_build_revision": "qualification-cli-test.v1",
                "device_class": "cpu",
                "dimensions": 1,
                "metric": "cosine",
                "normalization": "l2",
                "precision": "fp32",
                "chunk_schema_revision": "chunk.schema.qualification-cli-test.v1",
                "chunker_revision": "chunker.qualification-cli-test.v1"
            },
            "search_index_key": {
                "kind": "exact_flat",
                "schema_revision": "semantic.search-index.qualification-cli-test.v1",
                "profile_digest": "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"
            },
            "execution_resources": {
                "model_bytes": 1,
                "tokenizer_bytes": 1,
                "threads": 1,
                "max_concurrent_sessions": 1,
                "batch_size": 1,
                "sequence_length": 1,
                "load_deadline_ms": 1
            }
        }))
        .expect("native qualification runtime test pins");
        NativeQualificationExpectationsV1::packaged_default(
            "hybrid-conservative".to_owned(),
            runtime,
            NativeQualificationPlatformV1::current(),
        )
        .expect("packaged workload and corpus metadata")
    }

    #[test]
    fn default_validation_uses_byte_pinned_activation_workload() {
        let summary = validate_requested_workload(&PathBuf::from(env!("CARGO_MANIFEST_DIR")), None)
            .expect("checked-in activation workload validates");

        assert_eq!(summary.status, DirectEvaluationStatusV1::Pass);
        assert_eq!(
            summary.workload_digest,
            "sha256:068eeb1726539df4575f0b0b516403c7123dbd157acfb22aa2a89c4fcfbb5610"
        );
        assert_eq!(summary.profile_count, 3);
    }

    #[test]
    fn qualify_native_parses_candidate_and_output_paths() {
        let cli = Cli::try_parse_from([
            "tracedecay-search-eval",
            "qualify-native",
            "--project-root",
            "project",
            "--candidate",
            "candidate.json",
            "--output",
            "qualification.json",
        ])
        .expect("qualify-native arguments parse");

        assert!(matches!(
            cli.command,
            Command::QualifyNative {
                project_root,
                candidate,
                output,
            } if project_root == *"project"
                && candidate == *"candidate.json"
                && output == *"qualification.json"
        ));
    }

    #[test]
    fn corrupt_native_qualification_bytes_are_rejected_without_writing() {
        let output = tempfile::tempdir()
            .expect("qualification output directory")
            .path()
            .join("qualification.json");
        let expectations = qualification_expectations_for_corrupt_bytes();

        let error = write_native_qualification(
            &output,
            b"not a native qualification document",
            &expectations,
        )
        .expect_err("corrupt daemon bytes must never be written");

        assert!(error.contains("native qualification bytes are corrupt"));
        assert!(
            !output.exists(),
            "corrupt bytes must not create an artifact"
        );
    }
}
