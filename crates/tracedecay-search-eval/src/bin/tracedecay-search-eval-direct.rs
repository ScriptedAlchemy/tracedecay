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

#[cfg(feature = "hotpath")]
const HOTPATH_OUTPUT_FORMAT_ENV: &str = "HOTPATH_OUTPUT_FORMAT";
#[cfg(feature = "hotpath")]
const HOTPATH_OUTPUT_PATH_ENV: &str = "HOTPATH_OUTPUT_PATH";
#[cfg(feature = "hotpath")]
const HOTPATH_FOCUS_ENV: &str = "HOTPATH_FOCUS";

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
    fn default_validation_uses_the_byte_pinned_packaged_workload() {
        let summary = validate_requested_workload(std::path::Path::new("."), None)
            .expect("packaged workload validates");

        assert_eq!(summary.status, DirectEvaluationStatusV1::Pass);
        assert_eq!(
            summary.workload_digest,
            "sha256:d6ee5a552dcb8df3ffb82a8b66054fc35db5ae2deb64cfe284e6e0303b793fc0"
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
