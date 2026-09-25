//! Production TypeScript compiler-diagnostics producer.
//!
//! `tracedecay_diagnostics` reads generation-bound diagnostics; nothing in the
//! read path runs a compiler. This module is the producer side for projects
//! whose own toolchain can be run without installing anything: it runs the
//! project's `tsc` over the current clean generation and publishes through the
//! same compiler pillar `tracedecay_diagnose` uses, so the read surface sees
//! one authority regardless of who produced the records.
//!
//! Each run's outcome is recorded per project root. A read that finds no
//! publication consults that record to name the real state (still running,
//! compiler failed, nothing resolved) instead of a bare "no producer".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use tracedecay_domain::{CodeGenerationId, UtcMicros};
use tracedecay_lsp::{TypeScriptProducerState, run_typescript_compiler, typescript_producer_state};

use crate::diagnose::{Diagnostic, Severity};
use crate::diagnostics_publication::{
    CodeIndexPublicationIdentityPortV1, CompilerDiagnosticPublicationOutcomeV1,
    compiler_diagnostic_analyzer_revision_v1, compiler_diagnostic_configuration_revision_v1,
    publish_compiler_diagnostics_through_code_index_v1,
};
use crate::diagnostics_store::DiagnosticsStore;

/// Outcome of one producer run over one project root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompilerProducerRunV1 {
    Published {
        generation: CodeGenerationId,
        inserted: u64,
        unresolved: usize,
    },
    /// The compiler reported findings, but none resolved onto a file the
    /// code-index generation knows, so nothing was published.
    NoResolvableDiagnostics {
        unresolved: Vec<String>,
    },
    /// The code index has no complete generation for this root yet.
    CodeIndexGenerationUnavailable,
    /// The compiler could not check the project (spawn failure, a
    /// `tsconfig.json` naming no inputs, a crash).
    CompilerFailed {
        reason: String,
    },
    PublicationFailed {
        reason: String,
    },
}

fn last_runs() -> &'static Mutex<BTreeMap<PathBuf, CompilerProducerRunV1>> {
    static LAST_RUNS: OnceLock<Mutex<BTreeMap<PathBuf, CompilerProducerRunV1>>> = OnceLock::new();
    LAST_RUNS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// The most recent producer run recorded for `project_root`, if any.
#[must_use]
pub fn last_producer_run(project_root: &Path) -> Option<CompilerProducerRunV1> {
    last_runs()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(project_root)
        .cloned()
}

fn record_producer_run(project_root: &Path, run: &CompilerProducerRunV1) {
    last_runs()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(project_root.to_path_buf(), run.clone());
}

/// Runs the project's own TypeScript compiler and publishes its findings as
/// the compiler pillar's clean-generation snapshot for the current code-index
/// generation. The outcome is recorded for [`last_producer_run`].
///
/// A clean project publishes an empty snapshot, which is what lets a read
/// answer "no diagnostics" as evidence instead of as a missing producer.
#[hotpath::measure(label = "usecases.diagnostics.typescript_producer", future = true)]
pub async fn run_typescript_producer_v1(
    project_root: &Path,
    compiler: &Path,
    resolver: &dyn CodeIndexPublicationIdentityPortV1,
    store: &DiagnosticsStore<'_>,
    observed_at: UtcMicros,
) -> CompilerProducerRunV1 {
    let run = produce(project_root, compiler, resolver, store, observed_at).await;
    record_producer_run(project_root, &run);
    run
}

async fn produce(
    project_root: &Path,
    compiler: &Path,
    resolver: &dyn CodeIndexPublicationIdentityPortV1,
    store: &DiagnosticsStore<'_>,
    observed_at: UtcMicros,
) -> CompilerProducerRunV1 {
    let (Ok(analyzer_revision), Ok(configuration_revision)) = (
        compiler_diagnostic_analyzer_revision_v1(),
        compiler_diagnostic_configuration_revision_v1(),
    ) else {
        return CompilerProducerRunV1::PublicationFailed {
            reason: "compiler pillar identity is unavailable".to_owned(),
        };
    };
    let reported = match run_typescript_compiler(compiler, project_root).await {
        Ok(reported) => reported,
        Err(error) => {
            return CompilerProducerRunV1::CompilerFailed {
                reason: error.to_string(),
            };
        }
    };
    let parsed = reported
        .into_iter()
        .map(compiler_diagnostic)
        .collect::<Vec<_>>();
    match publish_compiler_diagnostics_through_code_index_v1(
        project_root,
        Some(resolver),
        store,
        &parsed,
        analyzer_revision,
        configuration_revision,
        observed_at,
    )
    .await
    {
        CompilerDiagnosticPublicationOutcomeV1::Published {
            generation,
            report,
            unresolved,
        } => CompilerProducerRunV1::Published {
            generation,
            inserted: report.inserted,
            unresolved: unresolved.len(),
        },
        CompilerDiagnosticPublicationOutcomeV1::NoResolvableDiagnostics { unresolved } => {
            CompilerProducerRunV1::NoResolvableDiagnostics {
                unresolved: unresolved.iter().map(ToString::to_string).collect(),
            }
        }
        CompilerDiagnosticPublicationOutcomeV1::CodeIndexIdentityUnavailable
        | CompilerDiagnosticPublicationOutcomeV1::CodeIndexGenerationUnavailable => {
            CompilerProducerRunV1::CodeIndexGenerationUnavailable
        }
        CompilerDiagnosticPublicationOutcomeV1::Failed { reason } => {
            CompilerProducerRunV1::PublicationFailed { reason }
        }
    }
}

/// Re-addresses one `tsc` report line as a compiler-pillar diagnostic. tsc
/// reports only errors and warnings, so the severity map is total.
fn compiler_diagnostic(reported: tracedecay_lsp::Diagnostic) -> Diagnostic {
    Diagnostic {
        severity: if reported.level == "warning" {
            Severity::Warning
        } else {
            Severity::Error
        },
        code: (!reported.code.is_empty()).then_some(reported.code),
        message: reported.message,
        file: reported.file,
        line: reported.line_start,
        column: reported.column,
    }
}

/// The typed state a read that found no publication for `project_root` can
/// report about the TypeScript producer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeScriptDiagnosticsAvailabilityV1 {
    /// The producer is configured; its most recent run, if it has run.
    Configured {
        compiler: PathBuf,
        last_run: Option<CompilerProducerRunV1>,
    },
    CompilerMissing,
    NoTsconfig,
}

/// Resolves what a read can truthfully say about the TypeScript producer for
/// `project_root`: the filesystem state plus the last recorded run.
#[must_use]
pub fn typescript_diagnostics_availability(
    project_root: &Path,
) -> TypeScriptDiagnosticsAvailabilityV1 {
    match typescript_producer_state(project_root) {
        TypeScriptProducerState::Configured { compiler } => {
            TypeScriptDiagnosticsAvailabilityV1::Configured {
                compiler,
                last_run: last_producer_run(project_root),
            }
        }
        TypeScriptProducerState::CompilerMissing => {
            TypeScriptDiagnosticsAvailabilityV1::CompilerMissing
        }
        TypeScriptProducerState::NoTsconfig => TypeScriptDiagnosticsAvailabilityV1::NoTsconfig,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CompilerProducerRunV1, compiler_diagnostic, last_producer_run, record_producer_run,
    };
    use crate::diagnose::Severity;

    #[test]
    fn tsc_report_lines_become_compiler_pillar_diagnostics() {
        let diagnostic = compiler_diagnostic(tracedecay_lsp::Diagnostic {
            file: "src/index.ts".to_owned(),
            line_start: 3,
            line_end: 3,
            column: 14,
            level: "error".to_owned(),
            code: "TS4023".to_owned(),
            message: "Exported variable 'value' has or is using name 'Hidden'.".to_owned(),
            driver: "typescript",
        });
        assert_eq!(diagnostic.severity, Severity::Error);
        assert_eq!(diagnostic.code.as_deref(), Some("TS4023"));
        assert_eq!((diagnostic.line, diagnostic.column), (3, 14));

        let warning = compiler_diagnostic(tracedecay_lsp::Diagnostic {
            file: "src/index.ts".to_owned(),
            line_start: 1,
            line_end: 1,
            column: 1,
            level: "warning".to_owned(),
            code: String::new(),
            message: "unused".to_owned(),
            driver: "typescript",
        });
        assert_eq!(warning.severity, Severity::Warning);
        assert_eq!(warning.code, None);
    }

    #[test]
    fn the_last_run_is_recorded_per_project_root() {
        let root = tempfile::tempdir().expect("project root");
        assert_eq!(last_producer_run(root.path()), None);
        let run = CompilerProducerRunV1::CompilerFailed {
            reason: "tsc exited with 2".to_owned(),
        };
        record_producer_run(root.path(), &run);
        assert_eq!(last_producer_run(root.path()), Some(run));
        let other = tempfile::tempdir().expect("other root");
        assert_eq!(last_producer_run(other.path()), None);
    }
}
