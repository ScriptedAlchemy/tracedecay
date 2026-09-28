//! Production TypeScript compiler-diagnostics producer.
//!
//! `tracedecay_diagnostics` reads generation-bound diagnostics; nothing in the
//! read path runs a compiler. This module is the producer side for projects
//! whose own toolchain can be run without installing anything: it runs the
//! project's `tsc` over the current clean generation and publishes through the
//! same compiler pillar `tracedecay_diagnose` uses, so the read surface sees
//! one authority regardless of who produced the records.
//!
//! A monorepo is many TypeScript projects (one tsconfig per package). Every
//! generation carries one snapshot of all their merged findings, but a project
//! is rechecked only when the generation changed a file that can change what
//! tsc reports for it; the rest republish their previous result without
//! running tsc. Each run's outcome is recorded per project root, and each
//! tsconfig's check within it, so a read can name the real state of the
//! project that owns the file it asked about (still running, compiler failed,
//! compiler missing, nothing resolved) instead of a bare "no producer" or a
//! clean page nothing checked.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracedecay_domain::{CodeGenerationId, ContentDigest, UtcMicros};
use tracedecay_lsp::{
    SearchedTsconfig, TypeScriptFileOwner, TypeScriptProject, TypeScriptProjectConfigInputs,
    run_typescript_compiler, typescript_build_info_program, typescript_file_owner,
    typescript_install_command, typescript_projects,
};

use crate::diagnose::{Diagnostic, Severity};
use crate::diagnostics_publication::{
    CodeIndexPublicationIdentityV1, CompilerDiagnosticPublicationOutcomeV1,
    code_index_logical_path, compiler_diagnostic_analyzer_revision_v1,
    compiler_diagnostic_configuration_revision_v1, publish_compiler_diagnostics_for_identity_v1,
};
use crate::diagnostics_store::DiagnosticsStore;

/// Outcome of one producer run over one project root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompilerProducerRunV1 {
    Published {
        generation: CodeGenerationId,
        inserted: u64,
        unresolved: usize,
        /// Projects tsc checked for this generation.
        checked: usize,
        /// Projects whose previous result this generation republished.
        reused: usize,
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

/// What one project root's producer remembers between generations: the file
/// digests of the generation it last checked and how each project fared.
#[derive(Default)]
pub struct TypeScriptProducerStateV1 {
    checked_files: Option<BTreeMap<String, ContentDigest>>,
    projects: BTreeMap<PathBuf, ProjectResult>,
}

#[derive(Clone)]
enum ProjectResult {
    Checked {
        reported: Vec<Diagnostic>,
        /// Logical paths of the program files the check read; `None` when its
        /// build-info named none inside the project root.
        program: Option<BTreeSet<String>>,
    },
    Failed {
        reason: String,
    },
}

/// Projects checked at once. Each check is one mostly single-threaded tsc
/// process, and a large project holds over a gigabyte while it runs.
const CONCURRENT_CHECKS: usize = 4;

/// Publishes the merged findings of each project's own TypeScript compiler as
/// the compiler pillar's clean-generation snapshot for `identity`.
///
/// A project is rechecked, incrementally against its build-info in
/// `build_info_dir`, only when `identity` changed a file that can change what
/// tsc reports for it since the generation `state` last checked; every other
/// project republishes its previous result without running tsc. The outcome
/// is recorded for [`last_producer_run`], and each tsconfig's check for
/// [`typescript_diagnostics_availability`].
///
/// A clean project publishes an empty snapshot, which is what lets a read
/// answer "no diagnostics" as evidence instead of as a missing producer.
#[hotpath::measure(label = "usecases.diagnostics.typescript_producer", future = true)]
pub async fn run_typescript_producer_v1(
    project_root: &Path,
    projects: &[TypeScriptProject],
    identity: &CodeIndexPublicationIdentityV1,
    store: &DiagnosticsStore<'_>,
    build_info_dir: &Path,
    state: &mut TypeScriptProducerStateV1,
    observed_at: UtcMicros,
) -> CompilerProducerRunV1 {
    let mut checks = BTreeMap::new();
    let run = produce(
        project_root,
        projects,
        identity,
        store,
        build_info_dir,
        state,
        observed_at,
        &mut checks,
    )
    .await;
    record_producer_run(project_root, &run, checks);
    run
}

#[expect(
    clippy::too_many_arguments,
    reason = "One producer run: its inputs, the state it carries, and the checks it records."
)]
async fn produce(
    project_root: &Path,
    projects: &[TypeScriptProject],
    identity: &CodeIndexPublicationIdentityV1,
    store: &DiagnosticsStore<'_>,
    build_info_dir: &Path,
    state: &mut TypeScriptProducerStateV1,
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
    let files = identity
        .file_digests()
        .map(|(path, digest)| (path.to_owned(), digest.clone()))
        .collect::<BTreeMap<_, _>>();
    let changed = state
        .checked_files
        .as_ref()
        .map(|previous| changed_paths(previous, &files));
    let mut results = BTreeMap::new();
    let mut stale = Vec::new();
    for project in projects {
        let Some(compiler) = &project.compiler else {
            continue;
        };
        match (state.projects.get(&project.tsconfig), &changed) {
            (Some(previous), Some(changed))
                if !needs_check(project_root, &project.tsconfig, previous, changed) =>
            {
                results.insert(project.tsconfig.clone(), previous.clone());
            }
            _ => stale.push((project.tsconfig.clone(), compiler.clone())),
        }
    }
    let reused = results.len();
    let checked = stale.len();
    results.extend(check_projects(project_root, build_info_dir, stale).await);
    state.checked_files = Some(files);

    let mut parsed = Vec::new();
    let mut failures = Vec::new();
    for (tsconfig, result) in &results {
        let check = match result {
            ProjectResult::Checked { reported, .. } => {
                parsed.extend(reported.iter().cloned());
                TypeScriptProjectCheckV1::Checked
            }
            ProjectResult::Failed { reason } => {
                failures.push(reason.clone());
                TypeScriptProjectCheckV1::Failed {
                    reason: reason.clone(),
                }
            }
        };
        checks.insert(tsconfig.clone(), check);
    }
    state.projects = results;
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
    match publish_compiler_diagnostics_for_identity_v1(
        project_root,
        identity,
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
            checked,
            reused,
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

/// Logical paths whose content differs between two generations, including
/// files one of them lacks.
fn changed_paths(
    previous: &BTreeMap<String, ContentDigest>,
    current: &BTreeMap<String, ContentDigest>,
) -> BTreeSet<String> {
    let added_or_edited = current
        .iter()
        .filter(|(path, digest)| previous.get(*path) != Some(*digest))
        .map(|(path, _)| path.clone());
    let removed = previous
        .keys()
        .filter(|path| !current.contains_key(*path))
        .cloned();
    added_or_edited.chain(removed).collect()
}

/// Whether a change can alter what tsc reports for a project since its last
/// result: a program file its check read, or one of its config inputs. A
/// failed check read no program, so only its config inputs retry it.
fn needs_check(
    project_root: &Path,
    tsconfig: &Path,
    previous: &ProjectResult,
    changed: &BTreeSet<String>,
) -> bool {
    if changed.is_empty() {
        return false;
    }
    let program = match previous {
        ProjectResult::Checked { program: None, .. } => return true,
        ProjectResult::Checked {
            program: Some(program),
            ..
        } => Some(program),
        ProjectResult::Failed { .. } => None,
    };
    let config = TypeScriptProjectConfigInputs::load(project_root, tsconfig);
    changed.iter().any(|path| {
        program.is_some_and(|program| program.contains(path))
            || config.affected_by(&project_root.join(path))
    })
}

/// Checks `stale` projects, at most [`CONCURRENT_CHECKS`] at a time, each
/// against its own build-info in `build_info_dir`.
async fn check_projects(
    project_root: &Path,
    build_info_dir: &Path,
    stale: Vec<(PathBuf, PathBuf)>,
) -> BTreeMap<PathBuf, ProjectResult> {
    if stale.is_empty() {
        return BTreeMap::new();
    }
    if let Err(error) = tokio::fs::create_dir_all(build_info_dir).await {
        let reason = format!(
            "cannot create the TypeScript build-info directory '{}': {error}",
            build_info_dir.display()
        );
        return stale
            .into_iter()
            .map(|(tsconfig, _)| {
                (
                    tsconfig,
                    ProjectResult::Failed {
                        reason: reason.clone(),
                    },
                )
            })
            .collect();
    }
    let permits = Arc::new(Semaphore::new(CONCURRENT_CHECKS));
    let mut tasks = JoinSet::new();
    let mut spawned = BTreeMap::new();
    for (tsconfig, compiler) in stale {
        let build_info = build_info_dir.join(build_info_file_name(&tsconfig));
        let root = project_root.to_path_buf();
        let permits = Arc::clone(&permits);
        let key = tsconfig.clone();
        let task = tasks.spawn(async move {
            let Ok(_permit) = permits.acquire_owned().await else {
                return ProjectResult::Failed {
                    reason: "the TypeScript check admission closed".to_owned(),
                };
            };
            match run_typescript_compiler(&compiler, &root, &tsconfig, Some(&build_info)).await {
                Ok(reported) => ProjectResult::Checked {
                    reported: reported.into_iter().map(compiler_diagnostic).collect(),
                    program: program_paths(&root, &build_info),
                },
                Err(error) => ProjectResult::Failed {
                    reason: error.to_string(),
                },
            }
        });
        spawned.insert(task.id(), key);
    }
    let mut results = BTreeMap::new();
    while let Some(joined) = tasks.join_next_with_id().await {
        let (id, result) = match joined {
            Ok((id, result)) => (id, result),
            Err(error) => (
                error.id(),
                ProjectResult::Failed {
                    reason: format!("the TypeScript check task ended: {error}"),
                },
            ),
        };
        if let Some(tsconfig) = spawned.remove(&id) {
            results.insert(tsconfig, result);
        }
    }
    results
}

/// One build-info file per tsconfig path, so linked worktrees sharing a store
/// never share incremental state.
fn build_info_file_name(tsconfig: &Path) -> String {
    let digest = Sha256::digest(tsconfig.to_string_lossy().as_bytes());
    format!("{}.tsbuildinfo", hex::encode(digest))
}

fn program_paths(project_root: &Path, build_info: &Path) -> Option<BTreeSet<String>> {
    let program = typescript_build_info_program(build_info)?
        .iter()
        .filter_map(|file| code_index_logical_path(project_root, file.to_str()?))
        .collect::<BTreeSet<_>>();
    (!program.is_empty()).then_some(program)
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

    #[cfg(unix)]
    use std::path::PathBuf;

    #[cfg(unix)]
    use tracedecay_domain::UtcMicros;
    #[cfg(unix)]
    use tracedecay_domain::test_fixtures::id;
    #[cfg(unix)]
    use tracedecay_lsp::typescript_projects;
    #[cfg(unix)]
    use tracedecay_runtime_core::test_executable::write_executable_script;

    use super::{
        CompilerProducerRunV1, TypeScriptDiagnosticsAvailabilityV1, TypeScriptProjectCheckV1,
        compiler_diagnostic, last_producer_run, record_producer_run,
        typescript_diagnostics_availability,
    };
    #[cfg(unix)]
    use super::{TypeScriptProducerStateV1, run_typescript_producer_v1};
    use crate::diagnose::Severity;
    #[cfg(unix)]
    use crate::diagnostics_publication::CodeIndexPublicationIdentityV1;
    #[cfg(unix)]
    use crate::diagnostics_store::DiagnosticsStore;

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

    /// A monorepo of two checkable packages and one whose check fails. The
    /// stand-in compiler logs each tsconfig it is pointed at, reports one
    /// finding per package, and records that package's source as the program
    /// in the build-info it is handed, as tsc does.
    #[cfg(unix)]
    struct ProducerFixture {
        _temp: tempfile::TempDir,
        root: PathBuf,
        build_info_dir: PathBuf,
        log: PathBuf,
        conn: tracedecay_runtime_core::db::engine::TestConnection,
    }

    #[cfg(unix)]
    impl ProducerFixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().expect("tempdir");
            let root = temp.path().join("project");
            for package in ["app", "lib", "broken"] {
                let dir = root.join("packages").join(package);
                std::fs::create_dir_all(dir.join("src")).expect("package");
                std::fs::write(dir.join("tsconfig.json"), "{ \"include\": [\"src\"] }")
                    .expect("tsconfig");
                std::fs::write(
                    dir.join("src/index.ts"),
                    format!("export const {package} = 1;\n"),
                )
                .expect("source");
            }
            std::fs::write(root.join("README.md"), "# fixture\n").expect("readme");
            let log = temp.path().join("tsc.log");
            let bin = root.join("node_modules/.bin");
            std::fs::create_dir_all(&bin).expect("bin");
            write_executable_script(
                &bin.join("tsc"),
                format!(
                    "#!/bin/sh\n\
                     echo \"$2\" >> {log}\n\
                     dir=$(dirname \"$2\")\n\
                     package=$(basename \"$dir\")\n\
                     [ \"$6 $7\" = \"--incremental --tsBuildInfoFile\" ] || exit 9\n\
                     printf '{{\"fileNames\":[\"%s/src/index.ts\"]}}' \"$dir\" > \"$8\"\n\
                     if [ \"$package\" = broken ]; then echo \"error TS5083: Cannot read file.\"; exit 1; fi\n\
                     echo \"packages/$package/src/index.ts(1,1): error TS2322: $package finding.\"\n\
                     exit 2\n",
                    log = log.display()
                ),
            ).expect("write script");
            let conn = tracedecay_runtime_core::db::engine::TestConnection::open(
                &temp.path().join("diagnostics.db"),
            );
            Self {
                build_info_dir: temp.path().join("store/typescript-build-info"),
                _temp: temp,
                root,
                log,
                conn,
            }
        }

        /// The code-index identity of the files on disk now.
        fn identity(&self, generation: &str) -> CodeIndexPublicationIdentityV1 {
            let files = [
                "README.md",
                "packages/app/src/index.ts",
                "packages/lib/src/index.ts",
                "packages/broken/src/index.ts",
            ]
            .map(|path| {
                let bytes = std::fs::read(self.root.join(path)).expect("fixture file");
                (
                    path.to_owned(),
                    id(&format!("file.{}", path.replace('/', "."))),
                    tracedecay_code_index::intake::content_digest(&bytes),
                )
            });
            CodeIndexPublicationIdentityV1::new(
                id(generation),
                id("repository.fixture"),
                Some(id("worktree.fixture")),
                Some(id("ref.main")),
                None,
                files,
            )
        }

        /// Package names tsc checked since the last call.
        fn checked(&self) -> Vec<String> {
            let log = std::fs::read_to_string(&self.log).unwrap_or_default();
            std::fs::write(&self.log, "").expect("reset log");
            let mut checked = log
                .lines()
                .map(|tsconfig| {
                    tsconfig
                        .trim_end_matches("/tsconfig.json")
                        .rsplit('/')
                        .next()
                        .expect("package")
                        .to_owned()
                })
                .collect::<Vec<_>>();
            checked.sort();
            checked
        }

        async fn run(
            &self,
            generation: &str,
            state: &mut TypeScriptProducerStateV1,
        ) -> CompilerProducerRunV1 {
            let store = DiagnosticsStore::new_runtime(&self.conn);
            run_typescript_producer_v1(
                &self.root,
                &typescript_projects(&self.root),
                &self.identity(generation),
                &store,
                &self.build_info_dir,
                state,
                UtcMicros(1_700_000_000_000_000),
            )
            .await
        }

        async fn findings(&self, generation: &str) -> Vec<(String, String)> {
            let store = DiagnosticsStore::new_runtime(&self.conn);
            let mut findings = store
                .current_records(&id(generation))
                .await
                .expect("current records")
                .into_iter()
                .map(|record| {
                    (
                        record.file_occurrence_id.as_str().to_owned(),
                        record.message,
                    )
                })
                .collect::<Vec<_>>();
            findings.sort();
            findings
        }
    }

    #[cfg(unix)]
    fn published(generation: &str, checked: usize, reused: usize) -> CompilerProducerRunV1 {
        CompilerProducerRunV1::Published {
            generation: id(generation),
            inserted: 2,
            unresolved: 0,
            checked,
            reused,
        }
    }

    /// Every generation publishes all three projects' results, but tsc runs
    /// only for a project the generation changed an input of: the edited
    /// package after a one-file edit, nothing after an edit no project reads.
    /// The failed package is not retried until its own inputs change.
    #[cfg(unix)]
    #[tokio::test]
    async fn only_projects_a_generation_changed_are_rechecked() {
        let fixture = ProducerFixture::new();
        let mut state = TypeScriptProducerStateV1::default();
        let both = vec![
            (
                "file.packages.app.src.index.ts".to_owned(),
                "app finding.".to_owned(),
            ),
            (
                "file.packages.lib.src.index.ts".to_owned(),
                "lib finding.".to_owned(),
            ),
        ];

        assert_eq!(
            fixture.run("generation.producer.1", &mut state).await,
            published("generation.producer.1", 3, 0)
        );
        assert_eq!(fixture.checked(), ["app", "broken", "lib"]);
        assert_eq!(fixture.findings("generation.producer.1").await, both);

        std::fs::write(
            fixture.root.join("packages/app/src/index.ts"),
            "export const app = 2;\n",
        )
        .expect("edit app");
        assert_eq!(
            fixture.run("generation.producer.2", &mut state).await,
            published("generation.producer.2", 1, 2)
        );
        assert_eq!(fixture.checked(), ["app"]);
        assert_eq!(fixture.findings("generation.producer.2").await, both);

        std::fs::write(fixture.root.join("README.md"), "# edited\n").expect("edit readme");
        assert_eq!(
            fixture.run("generation.producer.3", &mut state).await,
            published("generation.producer.3", 0, 3)
        );
        assert_eq!(fixture.checked(), Vec::<String>::new());
        assert_eq!(fixture.findings("generation.producer.3").await, both);

        std::fs::write(
            fixture.root.join("packages/broken/src/index.ts"),
            "export const broken = 2;\n",
        )
        .expect("edit broken");
        assert_eq!(
            fixture.run("generation.producer.4", &mut state).await,
            published("generation.producer.4", 1, 2)
        );
        assert_eq!(fixture.checked(), ["broken"]);
    }
}
