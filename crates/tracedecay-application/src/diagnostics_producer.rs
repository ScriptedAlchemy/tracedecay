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
//! tsc reports for it or its installed compiler changed; the rest republish
//! their previous result without running tsc. Each run's outcome is published to the owning project's status handle, and each
//! tsconfig's check within it, so a read can name the real state of the
//! project that owns the file it asked about (still running, compiler failed,
//! compiler missing, nothing resolved) instead of a bare "no producer" or a
//! clean page nothing checked.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};

use futures_util::FutureExt;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tracedecay_domain::{ContentDigest, UtcMicros};
use tracedecay_lsp::{
    SearchedTsconfig, TypeScriptCompilerAdmission, TypeScriptFileOwner, TypeScriptProject,
    TypeScriptProjectConfigInputs, run_typescript_compiler, typescript_build_info_program,
    typescript_file_owner, typescript_install_command, typescript_projects,
};
use tracedecay_runtime_core::native_runtime;

use crate::diagnose::{Diagnostic, Severity};
use crate::diagnostics_publication::{
    CodeIndexPublicationIdentityV1, CompilerDiagnosticPublicationOutcomeV1,
    code_index_logical_path, compiler_diagnostic_analyzer_revision_v1,
    compiler_diagnostic_configuration_revision_v1, publish_compiler_diagnostics_for_identity_v1,
};
use crate::diagnostics_store::DiagnosticsStore;

mod status;

#[cfg(all(test, unix))]
mod installed_compiler_tests;
#[cfg(all(test, unix))]
mod overlap_tests;

use status::TypeScriptProducerStatusWriterV1;
pub use status::{
    CompilerProducerRunV1, TypeScriptProducerStatusHandleV1, TypeScriptProducerStatusV1,
    TypeScriptProjectCheckV1,
};

/// What one project root's producer remembers between generations: the file
/// digests of the generation it last checked and how each project fared.
pub struct TypeScriptProducerStateV1 {
    status: TypeScriptProducerStatusWriterV1,
    admission: TypeScriptCompilerAdmission,
    checked_files: Option<BTreeMap<String, ContentDigest>>,
    projects: BTreeMap<PathBuf, ProjectResult>,
}

impl TypeScriptProducerStateV1 {
    /// Appoint this producer as the current writer of `status`. Construct its
    /// state before spawning it: a retained runtime may still have an older
    /// producer completing a check while the old route is being retired.
    #[must_use]
    pub fn new(
        status: TypeScriptProducerStatusHandleV1,
        admission: TypeScriptCompilerAdmission,
    ) -> Self {
        Self {
            status: status.start_producer(),
            admission,
            checked_files: None,
            projects: BTreeMap::new(),
        }
    }
}

#[derive(Clone)]
enum ProjectResult {
    Checked {
        reported: Vec<Diagnostic>,
        /// Logical paths of the program files the check read; `None` when its
        /// build-info named none inside the project root.
        program: Option<BTreeSet<String>>,
        compiler_revision: Option<[u8; 32]>,
    },
    Failed {
        reason: String,
        compiler_revision: Option<[u8; 32]>,
    },
}

impl ProjectResult {
    fn compiler_revision(&self) -> Option<[u8; 32]> {
        match self {
            Self::Checked {
                compiler_revision, ..
            }
            | Self::Failed {
                compiler_revision, ..
            } => *compiler_revision,
        }
    }
}

/// The bytes of a launcher, manifest or revision record the producer reads.
const MAX_SMALL_FILE_BYTES: u64 = 1024 * 1024;
/// An installed compiler past these bounds has no established identity, so
/// every run checks it from scratch; typescript@7 with its native package is
/// about 30 MiB in 530 files.
const MAX_COMPILER_BYTES: u64 = 512 * 1024 * 1024;
const MAX_COMPILER_ENTRIES: usize = 16 * 1024;
const MAX_COMPILER_DEPTH: usize = 32;

fn read_bounded(path: &Path) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_SMALL_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= MAX_SMALL_FILE_BYTES).then_some(bytes)
}

fn update_framed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

/// The revision of what each `node_modules/.bin/tsc` runs, `None` where it
/// cannot be established. The installed compiler is outside the indexed file
/// set, so unchanged sources never authorize reusing a check across it.
#[tracing::instrument(
    name = "usecases.diagnostics.typescript_compiler_revisions",
    level = "trace",
    skip_all
)]
fn installed_compiler_revisions(
    compilers: BTreeSet<PathBuf>,
) -> BTreeMap<PathBuf, Option<[u8; 32]>> {
    let mut packages = BTreeMap::new();
    compilers
        .into_iter()
        .map(|compiler| {
            let revision = installed_compiler_revision(&compiler, &mut packages);
            (compiler, revision)
        })
        .collect()
}

/// A package manager links `.bin/tsc` to the `bin.tsc` entry of the
/// `typescript` package installed beside it, either as a symlink or as a shim
/// that runs that entry, so the launcher and that package identify the
/// compiler. A symlink to anything else runs an implementation this cannot
/// name.
fn installed_compiler_revision(
    compiler: &Path,
    packages: &mut BTreeMap<PathBuf, Option<InstalledCompilerPackage>>,
) -> Option<[u8; 32]> {
    let package = std::fs::canonicalize(compiler.parent()?.parent()?.join("typescript")).ok()?;
    let installed = packages
        .entry(package.clone())
        .or_insert_with(|| installed_compiler_package(&package))
        .as_ref()?;
    if std::fs::symlink_metadata(compiler)
        .ok()?
        .file_type()
        .is_symlink()
        && std::fs::canonicalize(compiler).ok()? != installed.entry
    {
        return None;
    }
    let launcher = read_bounded(compiler)?;
    let mut hasher = Sha256::new();
    update_framed(&mut hasher, compiler.as_os_str().as_encoded_bytes());
    update_framed(&mut hasher, &launcher);
    hasher.update(installed.digest);
    Some(hasher.finalize().into())
}

struct InstalledCompilerPackage {
    /// The canonical `bin.tsc` entry the launcher runs.
    entry: PathBuf,
    /// Every installed byte the entry can load.
    digest: [u8; 32],
}

/// The launcher entry loads its implementation from elsewhere in the package,
/// and typescript@7 from a platform package it resolves among its
/// dependencies, which also carries the default libraries. Rather than model
/// that loading, the digest covers every file of the `typescript` package and
/// of each declared dependency installed where Node resolves it.
fn installed_compiler_package(package: &Path) -> Option<InstalledCompilerPackage> {
    let manifest: serde_json::Value =
        serde_json::from_slice(&read_bounded(&package.join("package.json"))?).ok()?;
    if manifest.get("name")?.as_str()? != "typescript"
        || manifest.get("version")?.as_str()?.is_empty()
    {
        return None;
    }
    let entry =
        std::fs::canonicalize(package.join(manifest.get("bin")?.get("tsc")?.as_str()?)).ok()?;
    if !entry.starts_with(package) {
        return None;
    }
    let mut dependencies = BTreeSet::new();
    for field in ["dependencies", "optionalDependencies"] {
        if let Some(declared) = manifest.get(field) {
            dependencies.extend(declared.as_object()?.keys().cloned());
        }
    }
    let mut budget = CompilerReadBudget {
        bytes: MAX_COMPILER_BYTES,
        entries: MAX_COMPILER_ENTRIES,
        buffer: vec![0; 64 * 1024],
    };
    let mut hasher = Sha256::new();
    update_framed(&mut hasher, package.as_os_str().as_encoded_bytes());
    hash_package_tree(&mut hasher, package, Path::new(""), 0, &mut budget)?;
    for name in dependencies {
        if !Path::new(&name)
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
        {
            return None;
        }
        let Some(dependency) = installed_dependency(package, &name) else {
            // Optional platform packages for other targets are never installed.
            continue;
        };
        update_framed(&mut hasher, name.as_bytes());
        update_framed(&mut hasher, dependency.as_os_str().as_encoded_bytes());
        hash_package_tree(&mut hasher, &dependency, Path::new(""), 0, &mut budget)?;
    }
    Some(InstalledCompilerPackage {
        entry,
        digest: hasher.finalize().into(),
    })
}

/// Node's lookup for a bare specifier imported from inside `package`: its own
/// `node_modules`, then each ancestor's. That finds a dependency pnpm links
/// beside the package in its virtual store and one npm hoists to the root.
fn installed_dependency(package: &Path, name: &str) -> Option<PathBuf> {
    package
        .ancestors()
        .filter(|dir| dir.file_name() != Some("node_modules".as_ref()))
        .map(|dir| dir.join("node_modules").join(name))
        .find(|candidate| candidate.join("package.json").is_file())
        .and_then(|candidate| std::fs::canonicalize(candidate).ok())
}

struct CompilerReadBudget {
    bytes: u64,
    entries: usize,
    buffer: Vec<u8>,
}

/// Hashes every file under `root/relative` in name order, following the links
/// a package manager installs with. Nested `node_modules` are other packages,
/// reached only as declared dependencies.
fn hash_package_tree(
    hasher: &mut Sha256,
    root: &Path,
    relative: &Path,
    depth: usize,
    budget: &mut CompilerReadBudget,
) -> Option<()> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(root.join(relative)).ok()? {
        // Bound the listing before retaining names, including directories
        // whose subtrees contain no payload files.
        budget.entries = budget.entries.checked_sub(1)?;
        names.push(entry.ok()?.file_name());
    }
    names.sort();
    for name in names {
        if name == "node_modules" {
            continue;
        }
        let path = relative.join(&name);
        let metadata = std::fs::metadata(root.join(&path)).ok()?;
        if metadata.is_dir() {
            if depth == MAX_COMPILER_DEPTH {
                return None;
            }
            hash_package_tree(hasher, root, &path, depth + 1, budget)?;
        } else if metadata.is_file() {
            let mut file = std::fs::File::open(root.join(&path))
                .ok()?
                .take(budget.bytes.saturating_add(1));
            let mut content = Sha256::new();
            let mut length = 0_u64;
            loop {
                let read = file.read(&mut budget.buffer).ok()?;
                if read == 0 {
                    break;
                }
                content.update(&budget.buffer[..read]);
                length += read as u64;
            }
            budget.bytes = budget.bytes.checked_sub(length)?;
            update_framed(hasher, path.as_os_str().as_encoded_bytes());
            hasher.update(length.to_le_bytes());
            hasher.update(content.finalize());
        } else {
            return None;
        }
    }
    Some(())
}

/// tsc replays the diagnostics its build-info cached whenever the build-info
/// names the running compiler's version and the sources are unchanged, and
/// typescript@7 records fixed versions for its own default libraries. A
/// same-version change to the installed compiler would therefore replay the
/// previous implementation's findings, so a build-info is kept only while the
/// compiler revision recorded beside it is the one about to run. Only this
/// tsconfig's own pair of files is touched.
fn bind_build_info(build_info: &Path, compiler_revision: Option<[u8; 32]>) -> std::io::Result<()> {
    fn remove_if_present(path: &Path) -> std::io::Result<()> {
        match std::fs::remove_file(path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    let record = build_info.with_extension("compiler-revision");
    if let Some(revision) = compiler_revision
        && read_bounded(&record).as_deref() == Some(revision.as_slice())
    {
        return Ok(());
    }
    // Removed before the record names the new revision, so an interrupted
    // rebind never leaves an old build-info under a matching record.
    remove_if_present(build_info)?;
    match compiler_revision {
        Some(revision) => std::fs::write(&record, revision),
        None => remove_if_present(&record),
    }
}

/// Publishes the merged findings of each project's own TypeScript compiler as
/// the compiler pillar's clean-generation snapshot for `identity`.
///
/// A project is rechecked, incrementally against its build-info in
/// `build_info_dir`, only when `identity` changed a file that can change what
/// tsc reports for it since the generation `state` last checked, or when its
/// installed compiler is not the one that produced that result; every other
/// project republishes its previous result without running tsc. The outcome
/// and each tsconfig's check are published to the status handle `state` was
/// built with, which [`typescript_diagnostics_availability`] reads.
///
/// A clean project publishes an empty snapshot, which is what lets a read
/// answer "no diagnostics" as evidence instead of as a missing producer.
#[tracing::instrument(
    name = "usecases.diagnostics.typescript_producer",
    level = "trace",
    skip_all
)]
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
    let files = identity
        .file_digests()
        .map(|(path, digest)| (path.to_owned(), digest.clone()))
        .collect::<BTreeMap<_, _>>();
    let changed = state
        .checked_files
        .as_ref()
        .map(|previous| changed_paths(previous, &files));
    let compilers = projects
        .iter()
        .filter_map(|project| project.compiler.clone())
        .collect::<BTreeSet<_>>();
    let compiler_revisions =
        native_runtime::spawn_blocking(move || installed_compiler_revisions(compilers))
            .await
            .unwrap_or_else(|error| {
                // Every revision is then unknown, which forbids reuse.
                tracing::warn!(%error, "TypeScript compiler revisions were not computed");
                BTreeMap::new()
            });
    let mut results = BTreeMap::new();
    let mut stale = Vec::new();
    for project in projects {
        let Some(compiler) = &project.compiler else {
            continue;
        };
        let revision = compiler_revisions.get(compiler).copied().flatten();
        match (state.projects.get(&project.tsconfig), &changed) {
            (Some(previous), Some(changed))
                if revision.is_some()
                    && previous.compiler_revision() == revision
                    && !needs_check(project_root, &project.tsconfig, previous, changed) =>
            {
                results.insert(project.tsconfig.clone(), previous.clone());
            }
            _ => stale.push((project.tsconfig.clone(), compiler.clone(), revision)),
        }
    }
    let reused = results.len();
    let checked = stale.len();
    results.extend(check_projects(project_root, build_info_dir, stale, &state.admission).await);
    state.checked_files = Some(files);

    let mut parsed = Vec::new();
    let mut failures = Vec::new();
    for (tsconfig, result) in &results {
        let check = match result {
            ProjectResult::Checked { reported, .. } => {
                parsed.extend(reported.iter().cloned());
                TypeScriptProjectCheckV1::Checked
            }
            ProjectResult::Failed { reason, .. } => {
                failures.push(reason.clone());
                TypeScriptProjectCheckV1::Failed {
                    reason: reason.clone(),
                }
            }
        };
        checks.insert(tsconfig.clone(), check);
    }
    state.projects = results;
    let Some(publication) = state.status.begin_publication().await else {
        return CompilerProducerRunV1::Superseded;
    };
    let run = match (
        compiler_diagnostic_analyzer_revision_v1(),
        compiler_diagnostic_configuration_revision_v1(),
    ) {
        (Ok(analyzer_revision), Ok(configuration_revision)) => {
            if checks
                .values()
                .any(|check| *check == TypeScriptProjectCheckV1::Checked)
            {
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
                    CompilerDiagnosticPublicationOutcomeV1::NoResolvableDiagnostics {
                        unresolved,
                    } => CompilerProducerRunV1::NoResolvableDiagnostics {
                        unresolved: unresolved.iter().map(ToString::to_string).collect(),
                    },
                    CompilerDiagnosticPublicationOutcomeV1::CodeIndexIdentityUnavailable
                    | CompilerDiagnosticPublicationOutcomeV1::CodeIndexGenerationUnavailable => {
                        CompilerProducerRunV1::CodeIndexGenerationUnavailable
                    }
                    CompilerDiagnosticPublicationOutcomeV1::Failed { reason } => {
                        CompilerProducerRunV1::PublicationFailed { reason }
                    }
                }
            } else {
                CompilerProducerRunV1::CompilerFailed {
                    reason: if failures.is_empty() {
                        "no TypeScript project under the project root has a compiler installed"
                            .to_owned()
                    } else {
                        failures.join("; ")
                    },
                }
            }
        }
        _ => CompilerProducerRunV1::PublicationFailed {
            reason: "compiler pillar identity is unavailable".to_owned(),
        },
    };
    publication.publish(TypeScriptProducerStatusV1::new(run.clone(), checks));
    run
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

/// The lazy window bounds waiting futures; the shared admission accounts for
/// children until supervision has killed and reaped them.
async fn check_projects(
    project_root: &Path,
    build_info_dir: &Path,
    stale: Vec<(PathBuf, PathBuf, Option<[u8; 32]>)>,
    admission: &TypeScriptCompilerAdmission,
) -> BTreeMap<PathBuf, ProjectResult> {
    if stale.is_empty() {
        return BTreeMap::new();
    }
    let build_info = build_info_dir.to_path_buf();
    if let Err(error) = native_runtime::spawn_blocking(move || std::fs::create_dir_all(build_info))
        .await
        .unwrap_or_else(|error| Err(std::io::Error::other(error)))
    {
        let reason = format!(
            "cannot create the TypeScript build-info directory '{}': {error}",
            build_info_dir.display()
        );
        return stale
            .into_iter()
            .map(|(tsconfig, _, compiler_revision)| {
                (
                    tsconfig,
                    ProjectResult::Failed {
                        reason: reason.clone(),
                        compiler_revision,
                    },
                )
            })
            .collect();
    }
    futures_util::stream::iter(stale)
        .map(|(tsconfig, compiler, compiler_revision)| {
            let build_info = build_info_dir.join(build_info_file_name(&tsconfig));
            let root = project_root.to_path_buf();
            async move {
                let check = async {
                    let _build_info_guard = admission.serialize_build_info(&build_info).await;
                    let bound = build_info.clone();
                    if let Err(error) = native_runtime::spawn_blocking(move || {
                        bind_build_info(&bound, compiler_revision)
                    })
                    .await
                    .unwrap_or_else(|error| Err(std::io::Error::other(error)))
                    {
                        // Nothing ran under this compiler, so the next run retries.
                        return ProjectResult::Failed {
                            reason: format!(
                                "cannot bind the TypeScript build-info '{}' to the installed \
                                 compiler: {error}",
                                build_info.display()
                            ),
                            compiler_revision: None,
                        };
                    }
                    match run_typescript_compiler(
                        admission,
                        &compiler,
                        &root,
                        &tsconfig,
                        Some(&build_info),
                    )
                    .await
                    {
                        Ok(reported) => ProjectResult::Checked {
                            reported: reported.into_iter().map(compiler_diagnostic).collect(),
                            program: program_paths(&root, &build_info),
                            compiler_revision,
                        },
                        Err(error) => ProjectResult::Failed {
                            reason: error.to_string(),
                            compiler_revision,
                        },
                    }
                };
                let result = AssertUnwindSafe(check)
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|_| ProjectResult::Failed {
                        reason: "the TypeScript check panicked".to_owned(),
                        compiler_revision,
                    });
                (tsconfig, result)
            }
        })
        .buffer_unordered(admission.concurrency())
        .collect()
        .await
}

/// One build-info file per tsconfig path, so linked worktrees sharing a store
/// never share incremental state; [`bind_build_info`] keeps the compiler
/// revision that wrote it beside it.
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
/// its filesystem state, and the owner's last published run.
#[must_use]
pub fn typescript_diagnostics_availability(
    project_root: &Path,
    file: Option<&Path>,
    status: &TypeScriptProducerStatusHandleV1,
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
            let status = status.current();
            TypeScriptDiagnosticsAvailabilityV1::Configured {
                check: status
                    .as_ref()
                    .and_then(|status| status.checks.get(&tsconfig).cloned()),
                last_run: status.map(|status| status.run.clone()),
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
    use std::sync::Arc;

    #[cfg(unix)]
    use tracedecay_domain::UtcMicros;
    #[cfg(unix)]
    use tracedecay_domain::test_fixtures::id;
    #[cfg(unix)]
    use tracedecay_lsp::{TypeScriptCompilerAdmission, typescript_projects};
    #[cfg(unix)]
    use tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1;
    #[cfg(unix)]
    use tracedecay_runtime_core::test_executable::write_executable_script;

    use super::{
        CompilerProducerRunV1, TypeScriptDiagnosticsAvailabilityV1,
        TypeScriptProducerStatusHandleV1, TypeScriptProducerStatusV1, TypeScriptProjectCheckV1,
        compiler_diagnostic, typescript_diagnostics_availability,
    };
    #[cfg(unix)]
    use super::{TypeScriptProducerStateV1, run_typescript_producer_v1};
    use crate::diagnose::Severity;
    #[cfg(unix)]
    use crate::diagnostics_publication::CodeIndexPublicationIdentityV1;
    #[cfg(unix)]
    use crate::diagnostics_store::DiagnosticsStore;

    #[cfg(unix)]
    fn admission() -> TypeScriptCompilerAdmission {
        TypeScriptCompilerAdmission::new(Arc::new(ProcessBackgroundCpuV1::new(
            std::num::NonZeroUsize::new(4).unwrap(),
        )))
    }

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

    /// A read about a file reports the check of the tsconfig that owns it,
    /// not whichever package happened to publish.
    #[tokio::test]
    async fn availability_reports_the_owning_tsconfigs_own_check() {
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
        let status = TypeScriptProducerStatusHandleV1::default();
        status
            .start_producer()
            .begin_publication()
            .await
            .unwrap()
            .publish(TypeScriptProducerStatusV1::new(
                CompilerProducerRunV1::CodeIndexGenerationUnavailable,
                BTreeMap::from([
                    (
                        root.join("packages/app/tsconfig.json"),
                        TypeScriptProjectCheckV1::Checked,
                    ),
                    (lib.clone(), failed.clone()),
                ]),
            ));

        let TypeScriptDiagnosticsAvailabilityV1::Configured {
            tsconfig, check, ..
        } = typescript_diagnostics_availability(
            root,
            Some(Path::new("packages/lib/src/a.ts")),
            &status,
        )
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
            std::fs::create_dir_all(root.join("node_modules/typescript/bin"))
                .expect("installed TypeScript package");
            std::fs::write(
                root.join("node_modules/typescript/package.json"),
                r#"{"name":"typescript","version":"5.9.3","bin":{"tsc":"./bin/tsc"}}"#,
            )
            .expect("installed TypeScript manifest");
            std::fs::write(root.join("node_modules/typescript/bin/tsc"), "")
                .expect("installed TypeScript entry");
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
        let mut state = TypeScriptProducerStateV1::new(
            TypeScriptProducerStatusHandleV1::default(),
            admission(),
        );
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

    /// TypeScript is installed outside the indexed sources, so upgrading the
    /// compiler must invalidate clean reuse even when no source digest changes.
    #[cfg(unix)]
    #[tokio::test]
    async fn compiler_upgrade_or_missing_evidence_rechecks_unchanged_sources() {
        let fixture = ProducerFixture::new();
        let mut state = TypeScriptProducerStateV1::new(
            TypeScriptProducerStatusHandleV1::default(),
            admission(),
        );
        let manifest = fixture.root.join("node_modules/typescript/package.json");

        fixture.run("generation.compiler.1", &mut state).await;
        assert_eq!(fixture.checked(), ["app", "broken", "lib"]);
        fixture.run("generation.compiler.2", &mut state).await;
        assert_eq!(fixture.checked(), Vec::<String>::new());

        std::fs::write(
            &manifest,
            r#"{"name":"typescript","version":"5.10.0","bin":{"tsc":"./bin/tsc"}}"#,
        )
        .expect("upgrade compiler");
        fixture.run("generation.compiler.3", &mut state).await;
        assert_eq!(
            fixture.checked(),
            ["app", "broken", "lib"],
            "a TypeScript version change must not reuse earlier compiler evidence"
        );
        fixture.run("generation.compiler.4", &mut state).await;
        assert_eq!(fixture.checked(), Vec::<String>::new());

        std::fs::write(&manifest, "{ malformed").expect("corrupt manifest");
        fixture.run("generation.compiler.5", &mut state).await;
        assert_eq!(
            fixture.checked(),
            ["app", "broken", "lib"],
            "unknown installed compiler evidence must not authorize clean reuse"
        );
        std::fs::remove_file(&manifest).expect("remove manifest");
        fixture.run("generation.compiler.6", &mut state).await;
        assert_eq!(fixture.checked(), ["app", "broken", "lib"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn compiler_launcher_replacement_invalidates_reuse() {
        let fixture = ProducerFixture::new();
        let mut state = TypeScriptProducerStateV1::new(
            TypeScriptProducerStatusHandleV1::default(),
            admission(),
        );
        fixture.run("generation.compiler.1", &mut state).await;
        assert_eq!(fixture.checked(), ["app", "broken", "lib"]);
        let compiler = fixture.root.join("node_modules/.bin/tsc");
        let mut script = std::fs::read_to_string(&compiler).expect("existing compiler");
        script.push_str("\n# replacement launcher\n");
        std::fs::write(&compiler, script).expect("replace launcher");

        fixture.run("generation.compiler.2", &mut state).await;
        assert_eq!(fixture.checked(), ["app", "broken", "lib"]);
    }

    /// A producer run reports to the handle of the owner it was built for. A
    /// successor owner opened at the same path reads no status until its own
    /// producer runs, so the predecessor's run is never taken for its own.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_successor_owner_does_not_inherit_the_predecessors_status() {
        let fixture = ProducerFixture::new();
        let predecessor = TypeScriptProducerStatusHandleV1::default();
        let mut state = TypeScriptProducerStateV1::new(predecessor.clone(), admission());
        let run = fixture.run("generation.producer.1", &mut state).await;
        assert_eq!(
            predecessor.current().map(|status| status.run().clone()),
            Some(run)
        );

        let successor = TypeScriptProducerStatusHandleV1::default();
        let file = Path::new("packages/app/src/index.ts");
        let TypeScriptDiagnosticsAvailabilityV1::Configured {
            check, last_run, ..
        } = typescript_diagnostics_availability(&fixture.root, Some(file), &successor)
        else {
            panic!("app is checkable");
        };
        assert_eq!((check, last_run), (None, None));
        let TypeScriptDiagnosticsAvailabilityV1::Configured { check, .. } =
            typescript_diagnostics_availability(&fixture.root, Some(file), &predecessor)
        else {
            panic!("app is checkable");
        };
        assert_eq!(check, Some(TypeScriptProjectCheckV1::Checked));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn isolated_producers_share_compiler_admission_and_keep_partial_findings() {
        let fixtures = [
            ProducerFixture::new(),
            ProducerFixture::new(),
            ProducerFixture::new(),
        ];
        let counter = tempfile::tempdir().unwrap();
        for fixture in &fixtures {
            let compiler = fixture.root.join("node_modules/.bin/tsc");
            let script = std::fs::read_to_string(&compiler).unwrap();
            let script = script.replacen(
                "#!/bin/sh\n",
                &format!(
                    "#!/bin/sh\n\
                     d='{}'\n\
                     mkdir \"$d/active.$$\"\n\
                     ls -d \"$d\"/active.* | wc -l >> \"$d/seen\"\n\
                     sleep 0.3\n\
                     rmdir \"$d/active.$$\"\n",
                    counter.path().display(),
                ),
                1,
            );
            write_executable_script(&compiler, script).unwrap();
        }
        let admission = admission();
        let mut states = fixtures
            .iter()
            .map(|_| {
                TypeScriptProducerStateV1::new(
                    TypeScriptProducerStatusHandleV1::default(),
                    admission.clone(),
                )
            })
            .collect::<Vec<_>>();
        let runs = futures_util::future::join_all(
            fixtures
                .iter()
                .zip(states.iter_mut())
                .map(|(fixture, state)| fixture.run("generation.shared", state)),
        )
        .await;
        assert!(
            runs.iter()
                .all(|run| run == &published("generation.shared", 3, 0)),
            "{runs:?}"
        );
        for fixture in &fixtures {
            assert_eq!(fixture.checked(), ["app", "broken", "lib"]);
            assert_eq!(fixture.findings("generation.shared").await.len(), 2);
        }
        let counts = std::fs::read_to_string(counter.path().join("seen")).unwrap();
        let counts = counts
            .lines()
            .map(|line| line.trim().parse::<usize>().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(counts.len(), 9);
        assert!(
            counts.iter().all(|count| *count <= admission.concurrency()),
            "{counts:?}"
        );
        assert!(counts.iter().any(|count| *count > 1), "{counts:?}");
        assert_eq!(
            std::fs::read_dir(counter.path()).unwrap().count(),
            1,
            "every child removed its active marker",
        );
    }
}
