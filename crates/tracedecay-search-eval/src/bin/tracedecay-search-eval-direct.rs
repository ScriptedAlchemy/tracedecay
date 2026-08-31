use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde::Serialize;
use serde_json::json;
use tracedecay_application::CancellationSignal;
use tracedecay_daemon_protocol::{
    DaemonClientIdentity, DaemonHandshake, DaemonInvocationClient, MovedStoreAdoption,
    SEMANTIC_EVALUATION_ISOLATED_DISPATCH_DEADLINE_MICROS,
};
use tracedecay_domain::errors::{Result as RuntimeResult, TraceDecayError};
use tracedecay_search_eval::{
    DirectEvaluationStatusV1, DirectWorkloadSummaryV1, GenerateCandidateOutputsOptions,
    SearchEvalError, compare_default_direct, compare_direct, generate_candidate_outputs,
    root_admitted_corpus_scope, validate_default_activation_workload, validate_direct_workload,
    write_daemon_native_qualification, write_generate_outputs,
};

#[cfg(feature = "hotpath")]
const HOTPATH_OUTPUT_FORMAT_ENV: &str = "HOTPATH_OUTPUT_FORMAT";
#[cfg(feature = "hotpath")]
const HOTPATH_OUTPUT_PATH_ENV: &str = "HOTPATH_OUTPUT_PATH";
#[cfg(feature = "hotpath")]
const HOTPATH_FOCUS_ENV: &str = "HOTPATH_FOCUS";

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
            default_value = "benchmark_data/search-quality/runs/candidate-outputs"
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
        profile: String,
    },
    /// Run the native evaluator in the owning daemon and write only its
    /// independently validated qualification evidence.
    QualifyNative {
        #[arg(long, default_value = ".")]
        project_root: PathBuf,
        #[arg(long)]
        profile: String,
        #[arg(long)]
        output: PathBuf,
    },
}

fn main() -> ExitCode {
    #[cfg(feature = "hotpath")]
    if let Err(message) = configure_hotpath_output() {
        return invalid("hotpath", message);
    }
    #[cfg(feature = "hotpath")]
    let _hotpath = hotpath::HotpathGuardBuilder::new("tracedecay-search-eval").build();
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
            profile,
        } => evaluate_and_publish(project_root, profile),
        Command::QualifyNative {
            project_root,
            profile,
            output,
        } => qualify_native(project_root, profile, output),
    }
}

#[cfg(feature = "hotpath")]
fn configure_hotpath_output() -> Result<(), String> {
    let output_path = std::env::var_os(HOTPATH_OUTPUT_PATH_ENV);
    let output_format = std::env::var_os(HOTPATH_OUTPUT_FORMAT_ENV);
    let focus = std::env::var_os(HOTPATH_FOCUS_ENV);
    if output_path
        .as_deref()
        .is_some_and(|path| path.to_str().is_none_or(str::is_empty))
    {
        return Err(format!(
            "{HOTPATH_OUTPUT_PATH_ENV} must be a non-empty Unicode path"
        ));
    }
    if output_format.as_deref().is_some_and(|format| {
        format.to_str().is_none_or(|format| {
            !matches!(
                format.to_ascii_lowercase().as_str(),
                "table" | "json" | "json-pretty" | "jsonpretty" | "none"
            )
        })
    }) {
        return Err(format!(
            "{HOTPATH_OUTPUT_FORMAT_ENV} must be one of table, json, json-pretty, or none"
        ));
    }
    if !hotpath_focus_is_supported(focus.as_deref()) {
        return Err(format!(
            "{HOTPATH_FOCUS_ENV} must be Unicode text; regular-expression form is unsupported"
        ));
    }
    let report_disabled = output_format
        .as_deref()
        .and_then(|format| format.to_str())
        .is_some_and(|format| format.eq_ignore_ascii_case("none"));
    if output_path.is_none() || report_disabled {
        // This evaluator writes a single JSON protocol document to stdout.
        // Profiling therefore stays silent unless the operator supplies an
        // explicit report destination.
        unsafe {
            std::env::set_var(HOTPATH_OUTPUT_FORMAT_ENV, "none");
            std::env::remove_var(HOTPATH_OUTPUT_PATH_ENV);
        }
    }
    Ok(())
}

#[cfg(any(feature = "hotpath", test))]
fn hotpath_focus_is_supported(focus: Option<&std::ffi::OsStr>) -> bool {
    focus.is_none_or(|focus| {
        focus.to_str().is_some_and(|focus| {
            focus
                .strip_prefix('/')
                .and_then(|pattern| pattern.strip_suffix('/'))
                .is_none()
        })
    })
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

fn evaluate_and_publish(project_root: PathBuf, evaluated_profile_id: String) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => return invalid("evaluate_and_publish", error),
    };
    #[cfg(feature = "hotpath")]
    hotpath::tokio_runtime!(runtime.handle());
    runtime.block_on(async move {
        let handshake = match handshake_for_eval_client(project_root) {
            Ok(handshake) => handshake,
            Err(error) => return invalid("evaluate_and_publish", error),
        };
        let client = match invocation_client_for_eval(handshake) {
            Ok(client) => client,
            Err(error) => return invalid("evaluate_and_publish", error),
        };
        match client
            .evaluate_and_publish_semantic_profile_until(
                &evaluated_profile_id,
                SEMANTIC_EVALUATION_ISOLATED_DISPATCH_DEADLINE_MICROS,
            )
            .await
        {
            Ok(result) => emit(&result, ExitCode::SUCCESS),
            Err(error) => invalid("evaluate_and_publish", error),
        }
    })
}

fn qualify_native(
    project_root: PathBuf,
    evaluated_profile_id: String,
    output: PathBuf,
) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => return invalid("qualify_native", error),
    };
    #[cfg(feature = "hotpath")]
    hotpath::tokio_runtime!(runtime.handle());
    runtime.block_on(async move {
        let handshake = match handshake_for_eval_client(project_root) {
            Ok(handshake) => handshake,
            Err(error) => return invalid("qualify_native", error),
        };
        let client = match invocation_client_for_eval(handshake) {
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
                &evaluated_profile_id,
                SEMANTIC_EVALUATION_ISOLATED_DISPATCH_DEADLINE_MICROS,
                cancellation,
            )
            .await
        {
            Ok(result) => {
                match write_daemon_native_qualification(&output, &result.qualification_bytes) {
                    Ok(()) => emit(
                        &json!({
                            "command": "qualify_native",
                            "status": "qualified",
                            "evaluated_profile_id": evaluated_profile_id,
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

fn eval_daemon_client_identity() -> RuntimeResult<DaemonClientIdentity> {
    let profile_root = tracedecay_runtime_core::config::user_data_dir().ok_or_else(|| {
        TraceDecayError::Config {
            message: "could not determine TraceDecay user data directory".to_string(),
        }
    })?;
    let global_db_path = tracedecay_runtime_core::config::global_db_path().ok_or_else(|| {
        TraceDecayError::Config {
            message: "could not determine TraceDecay global database path".to_string(),
        }
    })?;
    Ok(DaemonClientIdentity::new(profile_root, global_db_path))
}

fn handshake_for_eval_client(project_root: PathBuf) -> RuntimeResult<DaemonHandshake> {
    Ok(DaemonHandshake {
        project_path: Some(project_root),
        scope_prefix: None,
        timings: false,
        allow_init: false,
        allow_initialize_root_routing: false,
        client_identity: eval_daemon_client_identity()?,
        client_version: env!("CARGO_PKG_VERSION").to_string(),
        client_instance_id: tracedecay_runtime_core::runtime_identity::process_run_id().to_string(),
        tool_list_changed_capable: false,
        catalog_version: String::new(),
        moved_store_adoption: MovedStoreAdoption::Never,
    })
}

/// Consume the composition-root daemon authority record through the typed
/// discovery authority. Discovery stays owned by that record; this binary
/// does not mint a second endpoint or parse the record itself.
fn invocation_client_for_eval(handshake: DaemonHandshake) -> RuntimeResult<DaemonInvocationClient> {
    tracedecay_daemon_identity::invocation_client_for_current(handshake)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt;

    use super::*;

    #[cfg(unix)]
    #[test]
    fn non_utf8_hotpath_focus_is_rejected() {
        let focus = std::ffi::OsString::from_vec(vec![0xff]);

        assert!(!hotpath_focus_is_supported(Some(focus.as_os_str())));
    }

    #[test]
    fn default_validation_uses_byte_pinned_activation_workload() {
        let summary = validate_requested_workload(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(std::path::Path::parent)
                .expect("workspace root above crates/tracedecay-search-eval"),
            None,
        )
        .expect("checked-in activation workload validates");

        assert_eq!(summary.status, DirectEvaluationStatusV1::Pass);
        assert_eq!(
            summary.workload_digest,
            "sha256:c0bcabaab7ea81312a6468262003d865e6ca293b2328b6c6332c731e5c1785d3"
        );
        assert_eq!(summary.profile_count, 3);
    }

    #[test]
    fn qualify_native_parses_daemon_owned_profile_and_output_path() {
        let cli = Cli::try_parse_from([
            "tracedecay-search-eval",
            "qualify-native",
            "--project-root",
            "project",
            "--profile",
            "hybrid-conservative",
            "--output",
            "qualification.json",
        ])
        .expect("qualify-native arguments parse");

        assert!(matches!(
            cli.command,
            Command::QualifyNative {
                project_root,
                profile,
                output,
            } if project_root == *"project"
                && profile == "hybrid-conservative"
                && output == *"qualification.json"
        ));
    }

    #[test]
    fn evaluate_and_publish_accepts_only_a_daemon_owned_profile_selection() {
        let cli = Cli::try_parse_from([
            "tracedecay-search-eval",
            "evaluate-and-publish",
            "--project-root",
            "project",
            "--profile",
            "hybrid-conservative",
        ])
        .expect("evaluate-and-publish profile arguments parse");

        assert!(matches!(
            cli.command,
            Command::EvaluateAndPublish {
                project_root,
                profile,
            } if project_root == *"project" && profile == "hybrid-conservative"
        ));
        assert!(
            Cli::try_parse_from([
                "tracedecay-search-eval",
                "evaluate-and-publish",
                "--candidate",
                "caller-authored.json",
            ])
            .is_err(),
            "the publishing route must not accept caller-authored candidate JSON"
        );
    }

    #[test]
    fn corrupt_native_qualification_bytes_are_rejected_without_writing() {
        let output = tempfile::tempdir()
            .expect("qualification output directory")
            .path()
            .join("qualification.json");
        let error =
            write_daemon_native_qualification(&output, b"not a native qualification document")
                .expect_err("corrupt daemon bytes must never be written");

        assert!(
            error
                .to_string()
                .contains("native qualification bytes are corrupt")
        );
        assert!(
            !output.exists(),
            "corrupt bytes must not create an artifact"
        );
    }
}
