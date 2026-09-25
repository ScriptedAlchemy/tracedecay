//! Production TypeScript compiler-diagnostics producer.
//!
//! `tracedecay_diagnostics` reads generation-bound diagnostics; nothing in the
//! read path runs a compiler. This module is the producer side for projects
//! whose own toolchain can be run without installing anything: it runs the
//! project's `tsc` over the current clean generation and publishes through the
//! same compiler pillar `tracedecay_diagnose` uses, so the read surface sees
//! one authority regardless of who produced the records.
//!
//! A monorepo is many TypeScript projects (one tsconfig per package), so each
//! run checks every project that has a compiler and publishes their merged
//! findings as the one snapshot the generation carries. Each run's outcome is
//! recorded per project root, and each tsconfig's check within it, so a read
//! can name the real state of the project that owns the file it asked about
//! (still running, compiler failed, compiler missing, nothing resolved)
//! instead of a bare "no producer" or a clean page nothing checked.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use tracedecay_domain::{CodeGenerationId, UtcMicros};
use tracedecay_lsp::{
    SearchedTsconfig, TypeScriptFileOwner, TypeScriptProject, run_typescript_compiler,
    typescript_file_owner, typescript_install_command, typescript_projects,
};

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
    /// No project could be checked: every compiler failed (spawn failure, a
    /// tsconfig naming no inputs, a crash), or none is installed.
    CompilerFailed {
        reason: String,
    },
    PublicationFailed {
        reason: String,
    },
}

/// How one tsconfig fared in the most recent run. A project with no compiler
/// is not recorded: reads resolve that from the filesystem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeScriptProjectCheckV1 {
    /// Its findings are part of the published snapshot.
    Checked,
    Failed {
        reason: String,
    },
}

#[derive(Clone)]
struct ProducerRecord {
    run: CompilerProducerRunV1,
    checks: BTreeMap<PathBuf, TypeScriptProjectCheckV1>,
}

fn last_runs() -> &'static Mutex<BTreeMap<PathBuf, ProducerRecord>> {
    static LAST_RUNS: OnceLock<Mutex<BTreeMap<PathBuf, ProducerRecord>>> = OnceLock::new();
    LAST_RUNS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn last_record(project_root: &Path) -> Option<ProducerRecord> {
    last_runs()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(project_root)
        .cloned()
}

/// The most recent producer run recorded for `project_root`, if any.
#[must_use]
pub fn last_producer_run(project_root: &Path) -> Option<CompilerProducerRunV1> {
    last_record(project_root).map(|record| record.run)
}

fn record_producer_run(
    project_root: &Path,
    run: &CompilerProducerRunV1,
    checks: BTreeMap<PathBuf, TypeScriptProjectCheckV1>,
) {
    let record = ProducerRecord {
        run: run.clone(),
        checks,
    };
    last_runs()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(project_root.to_path_buf(), record);
}

/// Runs each project's own TypeScript compiler and publishes their merged
/// findings as the compiler pillar's clean-generation snapshot for the current
/// code-index generation. The outcome is recorded for [`last_producer_run`],
/// and each tsconfig's check for [`typescript_diagnostics_availability`].
///
/// A clean project publishes an empty snapshot, which is what lets a read
/// answer "no diagnostics" as evidence instead of as a missing producer.
#[hotpath::measure(label = "usecases.diagnostics.typescript_producer", future = true)]
pub async fn run_typescript_producer_v1(
    project_root: &Path,
    projects: &[TypeScriptProject],
    resolver: &dyn CodeIndexPublicationIdentityPortV1,
    store: &DiagnosticsStore<'_>,
    observed_at: UtcMicros,
) -> CompilerProducerRunV1 {
    let mut checks = BTreeMap::new();
    let run = produce(
        project_root,
        projects,
        resolver,
        store,
        observed_at,
        &mut checks,
    )
    .await;
    record_producer_run(project_root, &run, checks);
    run
}

async fn produce(
    project_root: &Path,
    projects: &[TypeScriptProject],
    resolver: &dyn CodeIndexPublicationIdentityPortV1,
    store: &DiagnosticsStore<'_>,
    observed_at: UtcMicros,
    checks: &mut BTreeMap<PathBuf, TypeScriptProjectCheckV1>,
) -> CompilerProducerRunV1 {
    let (Ok(analyzer_revision), Ok(configuration_revision)) = (
        compiler_diagnostic_analyzer_revision_v1(),
        compiler_diagnostic_configuration_revision_v1(),
    ) else {
        return CompilerProducerRunV1::PublicationFailed {
            reason: "compiler pillar identity is unavailable".to_owned(),
        };
    };
    // ponytail: projects are checked one after another, one tsc start per
    // tsconfig per generation; batch through `tsc --build` if a large
    // monorepo measures that cost as the bottleneck.
    let mut parsed = Vec::new();
    let mut failures = Vec::new();
    for project in projects {
        let Some(compiler) = &project.compiler else {
            continue;
        };
        let check = match run_typescript_compiler(compiler, project_root, &project.tsconfig).await {
            Ok(reported) => {
                parsed.extend(reported.into_iter().map(compiler_diagnostic));
                TypeScriptProjectCheckV1::Checked
            }
            Err(error) => {
                failures.push(error.to_string());
                TypeScriptProjectCheckV1::Failed {
                    reason: error.to_string(),
                }
            }
        };
        checks.insert(project.tsconfig.clone(), check);
    }
    if !checks
        .values()
        .any(|check| *check == TypeScriptProjectCheckV1::Checked)
    {
        return CompilerProducerRunV1::CompilerFailed {
            reason: if failures.is_empty() {
                "no TypeScript project under the project root has a compiler installed".to_owned()
            } else {
                failures.join("; ")
            },
        };
    }
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

/// The typed state a read can report about the TypeScript producer for the
/// project that owns what it asked about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeScriptDiagnosticsAvailabilityV1 {
    /// The owning project has a compiler; how its last check went and the
    /// producer's most recent run, if it has run.
    Configured {
        tsconfig: PathBuf,
        compiler: PathBuf,
        check: Option<TypeScriptProjectCheckV1>,
        last_run: Option<CompilerProducerRunV1>,
    },
    /// The owning tsconfig exists but no `node_modules/.bin/tsc` is installed
    /// for it; `install_command` installs the workspace's dependencies.
    CompilerMissing {
        tsconfig: PathBuf,
        install_command: &'static str,
    },
    /// No tsconfig owns the file; where the owner search looked, nearest
    /// first. Empty for a workspace read, which searched the whole tree.
    NoTsconfig { searched: Vec<SearchedTsconfig> },
}

/// Resolves what a read can truthfully say about the TypeScript producer: the
/// project owning `file` (or, for a workspace read, the first checkable one),
/// its filesystem state, and the last recorded run.
#[must_use]
pub fn typescript_diagnostics_availability(
    project_root: &Path,
    file: Option<&Path>,
) -> TypeScriptDiagnosticsAvailabilityV1 {
    let project = match file {
        Some(file) => match typescript_file_owner(project_root, file) {
            TypeScriptFileOwner::Owned(project) => project,
            TypeScriptFileOwner::Unowned { searched } => {
                return TypeScriptDiagnosticsAvailabilityV1::NoTsconfig { searched };
            }
        },
        None => {
            let projects = typescript_projects(project_root);
            let checkable = projects.iter().find(|project| project.compiler.is_some());
            let Some(project) = checkable.or(projects.first()).cloned() else {
                return TypeScriptDiagnosticsAvailabilityV1::NoTsconfig {
                    searched: Vec::new(),
                };
            };
            project
        }
    };
    let TypeScriptProject { tsconfig, compiler } = project;
    match compiler {
        Some(compiler) => {
            let record = last_record(project_root);
            TypeScriptDiagnosticsAvailabilityV1::Configured {
                check: record
                    .as_ref()
                    .and_then(|record| record.checks.get(&tsconfig).cloned()),
                last_run: record.map(|record| record.run),
                tsconfig,
                compiler,
            }
        }
        None => TypeScriptDiagnosticsAvailabilityV1::CompilerMissing {
            install_command: typescript_install_command(
                project_root,
                tsconfig.parent().unwrap_or(project_root),
            ),
            tsconfig,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use super::{
        CompilerProducerRunV1, TypeScriptDiagnosticsAvailabilityV1, TypeScriptProjectCheckV1,
        compiler_diagnostic, last_producer_run, record_producer_run,
        typescript_diagnostics_availability,
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
        record_producer_run(root.path(), &run, BTreeMap::new());
        assert_eq!(last_producer_run(root.path()), Some(run));
        let other = tempfile::tempdir().expect("other root");
        assert_eq!(last_producer_run(other.path()), None);
    }

    /// A read about a file reports the check of the tsconfig that owns it,
    /// not whichever package happened to publish.
    #[test]
    fn availability_reports_the_owning_tsconfigs_own_check() {
        let root = tempfile::tempdir().expect("project root");
        let root = root.path();
        for package in ["app", "lib"] {
            let dir = root.join("packages").join(package);
            std::fs::create_dir_all(dir.join("src")).expect("package");
            std::fs::write(dir.join("tsconfig.json"), "{}").expect("tsconfig");
        }
        let bin = root.join("node_modules/.bin");
        std::fs::create_dir_all(&bin).expect("bin");
        std::fs::write(bin.join(if cfg!(windows) { "tsc.cmd" } else { "tsc" }), "").expect("tsc");
        let lib = root.join("packages/lib/tsconfig.json");
        let failed = TypeScriptProjectCheckV1::Failed {
            reason: "TS5083: Cannot read file".to_owned(),
        };
        record_producer_run(
            root,
            &CompilerProducerRunV1::CodeIndexGenerationUnavailable,
            BTreeMap::from([
                (
                    root.join("packages/app/tsconfig.json"),
                    TypeScriptProjectCheckV1::Checked,
                ),
                (lib.clone(), failed.clone()),
            ]),
        );

        let TypeScriptDiagnosticsAvailabilityV1::Configured {
            tsconfig, check, ..
        } = typescript_diagnostics_availability(root, Some(Path::new("packages/lib/src/a.ts")))
        else {
            panic!("lib owns its sources and the root compiler checks it");
        };
        assert_eq!((tsconfig, check), (lib, Some(failed)));
    }
}
