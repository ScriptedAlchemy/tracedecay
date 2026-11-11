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
//! is rechecked only when its installed compiler, the configuration that
//! compiler resolves, or a file its last check's program read could have
//! changed what tsc reports for it; the rest republish their previous result.
//! Which files a project holds is never read from its configuration: it is the
//! program the compiler recorded checking. A project that references others
//! is checked only while the compiler's own dry build finds their outputs
//! current, because tsc checks it against those outputs.
//!
//! Each run's outcome is published to the owning project's status handle with
//! every tsconfig's check and program, so a read can name the real state of
//! the project whose program holds the file it asked about (still running,
//! compiler failed, compiler missing, nothing resolved) instead of a bare "no
//! producer" or a clean page nothing checked ([`availability`]).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};

use futures_util::FutureExt;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tracedecay_domain::{ContentDigest, UtcMicros};
use tracedecay_lsp::{
    TypeScriptCompilerAdmission, TypeScriptConfiguration, TypeScriptProgram, TypeScriptProject,
    is_typescript_package_manifest, resolve_typescript_configuration, run_typescript_compiler,
    typescript_build_info_program, typescript_install_command, typescript_project,
    typescript_referenced_projects_out_of_date,
};
use tracedecay_runtime_core::native_runtime;

use crate::diagnose::{Diagnostic, Severity};
use crate::diagnostics_publication::{
    CodeIndexPublicationIdentityV1, CompilerDiagnosticPublicationOutcomeV1,
    code_index_logical_path, compiler_diagnostic_analyzer_revision_v1,
    compiler_diagnostic_configuration_revision_v1, publish_compiler_diagnostics_for_identity_v1,
};
use crate::diagnostics_store::DiagnosticsStore;

mod availability;
mod status;

#[cfg(all(test, unix))]
mod compiler_conformance_tests;
#[cfg(all(test, unix))]
mod installed_compiler_tests;
#[cfg(all(test, unix))]
mod overlap_tests;

pub use availability::{
    SearchedTsconfigV1, TypeScriptDiagnosticsAvailabilityV1, typescript_diagnostics_availability,
};
use status::TypeScriptProducerStatusWriterV1;
pub use status::{
    CompilerProducerRunV1, TypeScriptProducerStatusHandleV1, TypeScriptProducerStatusV1,
    TypeScriptProgramMembershipV1, TypeScriptProjectCheckV1, TypeScriptProjectStatusV1,
};

/// What one project root's producer remembers between generations: the file
/// digests of the generation it last checked and what each project's compiler
/// established about it then.
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

/// What the compiler established about one tsconfig the last time it ran.
#[derive(Clone)]
struct ProjectResult {
    compiler_revision: Option<[u8; 32]>,
    /// How the compiler resolved the tsconfig; `None` when it could not.
    configuration: Option<TypeScriptConfiguration>,
    outcome: ProjectOutcome,
}

#[derive(Clone)]
enum ProjectOutcome {
    Checked {
        reported: Vec<Diagnostic>,
        /// The program the check recorded; `None` when its build-info could
        /// not be read or named none of its roots inside the project root.
        membership: Option<TypeScriptProgramMembershipV1>,
    },
    Failed {
        reason: String,
    },
}

impl ProjectResult {
    fn failed(
        compiler_revision: Option<[u8; 32]>,
        configuration: Option<TypeScriptConfiguration>,
        reason: String,
    ) -> Self {
        Self {
            compiler_revision,
            configuration,
            outcome: ProjectOutcome::Failed { reason },
        }
    }

    fn membership(&self) -> Option<&TypeScriptProgramMembershipV1> {
        match &self.outcome {
            ProjectOutcome::Checked { membership, .. } => membership.as_ref(),
            ProjectOutcome::Failed { .. } => None,
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
/// `projects` are the tsconfigs found under the root; the compiler adds the
/// ones they reference. Each is rechecked, incrementally against its
/// build-info in `build_info_dir`, unless its installed compiler and the
/// configuration that compiler resolves are those of its previous result and
/// `identity` changed nothing its previous program, its references' programs
/// or its package manifests could have read; every other project republishes
/// its previous result. The outcome, each tsconfig's check and the program it
/// recorded are published to the status handle `state` was built with, which
/// [`typescript_diagnostics_availability`] reads.
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
    let files = identity
        .file_digests()
        .map(|(path, digest)| (path.to_owned(), digest.clone()))
        .collect::<BTreeMap<_, _>>();
    let changes = state
        .checked_files
        .as_ref()
        .map(|previous| GenerationChanges::between(previous, &files));
    let resolved = resolve_projects(project_root, projects, state, changes.as_ref()).await;

    let mut results = BTreeMap::new();
    let mut stale = Vec::new();
    for (tsconfig, project) in &resolved.checkable {
        let configuration = match &project.configuration {
            Ok(configuration) => configuration,
            Err(reason) => {
                results.insert(
                    tsconfig.clone(),
                    ProjectResult::failed(project.revision, None, reason.clone()),
                );
                continue;
            }
        };
        match (state.projects.get(tsconfig), &changes) {
            (Some(previous), Some(changes))
                if project.revision.is_some()
                    && previous.compiler_revision == project.revision
                    && previous.configuration.as_ref() == Some(configuration)
                    && !changes.affect(
                        project_root,
                        tsconfig,
                        previous,
                        configuration,
                        &state.projects,
                    ) =>
            {
                results.insert(tsconfig.clone(), previous.clone());
            }
            _ => stale.push(StaleProject {
                tsconfig: tsconfig.clone(),
                compiler: project.compiler.clone(),
                compiler_revision: project.revision,
                configuration: configuration.clone(),
            }),
        }
    }
    // A configuration the compiler refused was established this run too.
    let reused = results.len()
        - resolved
            .checkable
            .values()
            .filter(|project| project.configuration.is_err())
            .count();
    let checked = resolved.checkable.len() - reused;
    results.extend(check_projects(project_root, build_info_dir, stale, &state.admission).await);
    state.checked_files = Some(files);

    let mut parsed = Vec::new();
    let mut failures = Vec::new();
    let mut statuses = BTreeMap::new();
    for tsconfig in &resolved.missing {
        statuses.insert(
            tsconfig.clone(),
            TypeScriptProjectStatusV1::CompilerMissing {
                install_command: typescript_install_command(
                    project_root,
                    tsconfig.parent().unwrap_or(project_root),
                ),
            },
        );
    }
    for (tsconfig, result) in &results {
        let Some(project) = resolved.checkable.get(tsconfig) else {
            continue;
        };
        let compiler = project.compiler.clone();
        let references = result
            .configuration
            .as_ref()
            .map(|configuration| configuration.references().to_vec())
            .unwrap_or_default();
        let status = match &result.outcome {
            ProjectOutcome::Checked {
                reported,
                membership,
            } => {
                parsed.extend(reported.iter().cloned());
                TypeScriptProjectStatusV1::Checked {
                    compiler,
                    membership: membership.clone(),
                    references,
                }
            }
            ProjectOutcome::Failed { reason } => {
                failures.push(reason.clone());
                TypeScriptProjectStatusV1::Failed {
                    compiler,
                    reason: reason.clone(),
                    references,
                }
            }
        };
        statuses.insert(tsconfig.clone(), status);
    }
    state.projects = results;
    let Some(publication) = state.status.begin_publication().await else {
        return CompilerProducerRunV1::Superseded;
    };
    let any_checked = statuses
        .values()
        .any(|status| matches!(status, TypeScriptProjectStatusV1::Checked { .. }));
    let run = match (
        compiler_diagnostic_analyzer_revision_v1(),
        compiler_diagnostic_configuration_revision_v1(),
    ) {
        (Ok(analyzer_revision), Ok(configuration_revision)) if any_checked => {
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
        (Ok(_), Ok(_)) => CompilerProducerRunV1::CompilerFailed {
            reason: if failures.is_empty() {
                NO_COMPILER_INSTALLED.to_owned()
            } else {
                failures.join("; ")
            },
        },
        _ => CompilerProducerRunV1::PublicationFailed {
            reason: "compiler pillar identity is unavailable".to_owned(),
        },
    };
    publication.publish(TypeScriptProducerStatusV1::new(
        Some(run.clone()),
        Some(identity.generation_id().clone()),
        statuses,
    ));
    run
}

/// Publishes what discovery alone establishes, before any check: each
/// tsconfig's compiler about to run or its missing one, and, when none has a
/// compiler, that nothing will be checked. A status retained from this
/// owner's completed check is kept while a compiler is there to replace it.
/// Returns the run it published when no tsconfig can be checked.
pub async fn publish_typescript_discovery_v1(
    project_root: &Path,
    projects: &[TypeScriptProject],
    state: &mut TypeScriptProducerStateV1,
) -> Option<CompilerProducerRunV1> {
    let checkable = projects.iter().any(|project| project.compiler.is_some());
    if checkable && state.status.has_status() {
        return None;
    }
    let statuses = projects
        .iter()
        .map(|project| {
            let status = match &project.compiler {
                Some(compiler) => TypeScriptProjectStatusV1::Checking {
                    compiler: compiler.clone(),
                },
                None => TypeScriptProjectStatusV1::CompilerMissing {
                    install_command: typescript_install_command(
                        project_root,
                        project.tsconfig.parent().unwrap_or(project_root),
                    ),
                },
            };
            (project.tsconfig.clone(), status)
        })
        .collect();
    let run = (!checkable).then(|| CompilerProducerRunV1::CompilerFailed {
        reason: NO_COMPILER_INSTALLED.to_owned(),
    });
    // A superseded producer's later publications are refused the same way.
    let publication = state.status.begin_publication().await?;
    publication.publish(TypeScriptProducerStatusV1::new(run.clone(), None, statuses));
    run
}

const NO_COMPILER_INSTALLED: &str =
    "no TypeScript project under the project root has a compiler installed";

/// The project set the compiler resolved for one generation: the discovered
/// tsconfigs and every configuration under the root they reference.
struct ResolvedProjects {
    checkable: BTreeMap<PathBuf, CheckableProject>,
    /// Tsconfigs with no compiler installed for them.
    missing: BTreeSet<PathBuf>,
}

struct CheckableProject {
    compiler: PathBuf,
    revision: Option<[u8; 32]>,
    /// How the compiler resolved the tsconfig, or why it could not.
    configuration: Result<TypeScriptConfiguration, String>,
}

/// Asks each project's compiler how it resolves its tsconfig, following the
/// references it reports to projects under the root. A configuration is
/// reused only when its compiler is unchanged and the generation changed
/// nothing at all, since any file may be one it reads.
async fn resolve_projects(
    project_root: &Path,
    discovered: &[TypeScriptProject],
    state: &TypeScriptProducerStateV1,
    changes: Option<&GenerationChanges>,
) -> ResolvedProjects {
    let mut resolved = ResolvedProjects {
        checkable: BTreeMap::new(),
        missing: BTreeSet::new(),
    };
    let mut revisions = BTreeMap::new();
    let mut pending = discovered.to_vec();
    while !pending.is_empty() {
        let compilers = pending
            .iter()
            .filter_map(|project| project.compiler.clone())
            .filter(|compiler| !revisions.contains_key(compiler))
            .collect::<BTreeSet<_>>();
        revisions.extend(
            native_runtime::spawn_blocking(move || installed_compiler_revisions(compilers))
                .await
                .unwrap_or_else(|error| {
                    // Every revision is then unknown, which forbids reuse.
                    tracing::warn!(%error, "TypeScript compiler revisions were not computed");
                    BTreeMap::new()
                }),
        );
        let mut queries = Vec::new();
        for TypeScriptProject { tsconfig, compiler } in pending.drain(..) {
            let Some(compiler) = compiler else {
                resolved.missing.insert(tsconfig);
                continue;
            };
            let revision = revisions.get(&compiler).copied().flatten();
            let unchanged = state.projects.get(&tsconfig).and_then(|previous| {
                (revision.is_some()
                    && previous.compiler_revision == revision
                    && changes.is_some_and(GenerationChanges::is_empty))
                .then(|| previous.configuration.clone())
                .flatten()
            });
            queries.push((tsconfig, compiler, revision, unchanged));
        }
        let configured = futures_util::stream::iter(queries)
            .map(|(tsconfig, compiler, revision, unchanged)| async move {
                let configuration = match unchanged {
                    Some(configuration) => Ok(configuration),
                    None => resolve_typescript_configuration(
                        &state.admission,
                        &compiler,
                        project_root,
                        &tsconfig,
                    )
                    .await
                    .map_err(|error| error.to_string()),
                };
                (
                    tsconfig,
                    CheckableProject {
                        compiler,
                        revision,
                        configuration,
                    },
                )
            })
            .buffer_unordered(state.admission.concurrency())
            .collect::<Vec<_>>()
            .await;
        resolved.checkable.extend(configured);
        let references = resolved
            .checkable
            .values()
            .filter_map(|project| project.configuration.as_ref().ok())
            .flat_map(TypeScriptConfiguration::references)
            .filter(|reference| {
                reference.starts_with(project_root)
                    && !resolved.checkable.contains_key(*reference)
                    && !resolved.missing.contains(*reference)
            })
            .cloned()
            .collect::<BTreeSet<_>>();
        let root = project_root.to_path_buf();
        pending = native_runtime::spawn_blocking(move || {
            references
                .into_iter()
                .map(|reference| typescript_project(&root, reference))
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "referenced TypeScript projects were not resolved");
            Vec::new()
        });
    }
    resolved
}

/// What one generation changed since the last checked one, by logical path.
struct GenerationChanges {
    /// Added, edited and removed files.
    changed: BTreeSet<String>,
    /// Whether a file was added that tsc could take as a source or resolve an
    /// import to. tsc resolves modules and expands `include` only to these
    /// extensions and package manifests, so another file can neither enter a
    /// program nor redirect one of its imports.
    added_module_candidate: bool,
}

/// Every extension tsc reads as a source, declaration or importable module.
const MODULE_EXTENSIONS: [&str; 9] = ["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "json"];

impl GenerationChanges {
    fn between(
        previous: &BTreeMap<String, ContentDigest>,
        current: &BTreeMap<String, ContentDigest>,
    ) -> Self {
        let mut changed = BTreeSet::new();
        let mut added_module_candidate = false;
        for (path, digest) in current {
            match previous.get(path) {
                Some(previous) if previous == digest => {}
                Some(_) => {
                    changed.insert(path.clone());
                }
                None => {
                    added_module_candidate |= Path::new(path)
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| MODULE_EXTENSIONS.contains(&extension));
                    changed.insert(path.clone());
                }
            }
        }
        changed.extend(
            previous
                .keys()
                .filter(|path| !current.contains_key(*path))
                .cloned(),
        );
        Self {
            changed,
            added_module_candidate,
        }
    }

    fn is_empty(&self) -> bool {
        self.changed.is_empty()
    }

    /// Whether these changes can alter what tsc reports for a project whose
    /// compiler and resolved configuration are those of `previous`: an added
    /// module candidate, a file its program or a referenced project's
    /// program read, or a package manifest governing the tsconfig or one of
    /// those files. A program the producer could not record cannot rule a
    /// change out, and neither can a reference it has no program for. Nor can
    /// a failed check: typescript@7 reports a configuration it could not read
    /// without the error, so any change may be the one that repairs it.
    fn affect(
        &self,
        project_root: &Path,
        tsconfig: &Path,
        previous: &ProjectResult,
        configuration: &TypeScriptConfiguration,
        projects: &BTreeMap<PathBuf, ProjectResult>,
    ) -> bool {
        if self.changed.is_empty() {
            return false;
        }
        if self.added_module_candidate {
            return true;
        }
        let mut read = Vec::new();
        match &previous.outcome {
            ProjectOutcome::Checked {
                membership: Some(membership),
                ..
            } => read.push(membership),
            ProjectOutcome::Checked {
                membership: None, ..
            }
            | ProjectOutcome::Failed { .. } => return true,
        }
        let mut references = configuration.references().to_vec();
        let mut visited = BTreeSet::new();
        while let Some(reference) = references.pop() {
            if !visited.insert(reference.clone()) {
                continue;
            }
            let Some(membership) = projects.get(&reference).and_then(ProjectResult::membership)
            else {
                return true;
            };
            read.push(membership);
            if let Some(configuration) = projects
                .get(&reference)
                .and_then(|result| result.configuration.as_ref())
            {
                references.extend_from_slice(configuration.references());
            }
        }
        let tsconfig = tsconfig
            .to_str()
            .and_then(|tsconfig| code_index_logical_path(project_root, tsconfig));
        self.changed.iter().any(|path| {
            read.iter()
                .any(|membership| membership.files.contains(path))
                || is_governing_manifest(path, tsconfig.as_deref(), &read)
        })
    }
}

/// A changed package manifest or lockfile decides how imports resolve and
/// which module format sources have below its directory.
fn is_governing_manifest(
    path: &str,
    tsconfig: Option<&str>,
    read: &[&TypeScriptProgramMembershipV1],
) -> bool {
    let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
    if !is_typescript_package_manifest(name) {
        return false;
    }
    let governs = |file: &str| dir.is_empty() || file.starts_with(&format!("{dir}/"));
    tsconfig.is_none_or(governs)
        || read
            .iter()
            .any(|membership| membership.files.iter().any(|file| governs(file)))
}

struct StaleProject {
    tsconfig: PathBuf,
    compiler: PathBuf,
    compiler_revision: Option<[u8; 32]>,
    configuration: TypeScriptConfiguration,
}

/// The lazy window bounds waiting futures; the shared admission accounts for
/// children until supervision has killed and reaped them.
async fn check_projects(
    project_root: &Path,
    build_info_dir: &Path,
    stale: Vec<StaleProject>,
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
            .map(|project| {
                (
                    project.tsconfig,
                    ProjectResult::failed(
                        project.compiler_revision,
                        Some(project.configuration),
                        reason.clone(),
                    ),
                )
            })
            .collect();
    }
    futures_util::stream::iter(stale)
        .map(|project| {
            let build_info = build_info_dir.join(build_info_file_name(&project.tsconfig));
            let root = project_root.to_path_buf();
            async move {
                let tsconfig = project.tsconfig.clone();
                let compiler_revision = project.compiler_revision;
                let configuration = project.configuration.clone();
                let result =
                    AssertUnwindSafe(check_project(admission, &root, &build_info, project))
                        .catch_unwind()
                        .await
                        .unwrap_or_else(|_| {
                            ProjectResult::failed(
                                compiler_revision,
                                Some(configuration),
                                "the TypeScript check panicked".to_owned(),
                            )
                        });
                (tsconfig, result)
            }
        })
        .buffer_unordered(admission.concurrency())
        .collect()
        .await
}

async fn check_project(
    admission: &TypeScriptCompilerAdmission,
    project_root: &Path,
    build_info: &Path,
    project: StaleProject,
) -> ProjectResult {
    let StaleProject {
        tsconfig,
        compiler,
        compiler_revision,
        configuration,
    } = project;
    let bound = build_info.to_path_buf();
    if let Err(error) =
        native_runtime::spawn_blocking(move || bind_build_info(&bound, compiler_revision))
            .await
            .unwrap_or_else(|error| Err(std::io::Error::other(error)))
    {
        // Nothing ran under this compiler, so the next run retries.
        return ProjectResult::failed(
            None,
            Some(configuration),
            format!(
                "cannot bind the TypeScript build-info '{}' to the installed compiler: {error}",
                build_info.display()
            ),
        );
    }
    let reported = match run_typescript_compiler(
        admission,
        &compiler,
        project_root,
        &tsconfig,
        Some(build_info),
    )
    .await
    {
        Ok(reported) => reported,
        Err(error) => {
            return ProjectResult::failed(
                compiler_revision,
                Some(configuration),
                error.to_string(),
            );
        }
    };
    let (root, recorded) = (project_root.to_path_buf(), build_info.to_path_buf());
    let membership = native_runtime::spawn_blocking(move || {
        typescript_build_info_program(&recorded)
            .and_then(|program| program_membership(&root, &program))
    })
    .await
    .unwrap_or_else(|error| {
        tracing::warn!(%error, "the TypeScript program was not read");
        None
    });
    // A project with no root of its own, such as a solution config, reads
    // nothing its references emit.
    let reads_references = !configuration.references().is_empty()
        && membership
            .as_ref()
            .is_none_or(|membership| !membership.roots.is_empty());
    if reads_references {
        match typescript_referenced_projects_out_of_date(
            admission,
            &compiler,
            project_root,
            configuration.references(),
        )
        .await
        {
            Ok(None) => {}
            Ok(Some(statement)) => {
                return ProjectResult::failed(
                    compiler_revision,
                    Some(configuration),
                    format!(
                        "tsc checks it against what its referenced projects emit, and those \
                         outputs are missing or out of date ({statement}); build them with \
                         `tsc --build`"
                    ),
                );
            }
            Err(error) => {
                return ProjectResult::failed(
                    compiler_revision,
                    Some(configuration),
                    error.to_string(),
                );
            }
        }
    }
    ProjectResult {
        compiler_revision,
        configuration: Some(configuration),
        outcome: ProjectOutcome::Checked {
            reported: reported.into_iter().map(compiler_diagnostic).collect(),
            membership,
        },
    }
}

/// One build-info file per tsconfig path, so linked worktrees sharing a store
/// never share incremental state; [`bind_build_info`] keeps the compiler
/// revision that wrote it beside it.
fn build_info_file_name(tsconfig: &Path) -> String {
    let digest = Sha256::digest(tsconfig.to_string_lossy().as_bytes());
    format!("{}.tsbuildinfo", hex::encode(digest))
}

/// The recorded program's files inside the project root, by logical path.
/// `None` when it names roots but none of them there, since then nothing
/// says which of the root's files it holds.
fn program_membership(
    project_root: &Path,
    program: &TypeScriptProgram,
) -> Option<TypeScriptProgramMembershipV1> {
    let logical = |files: &[PathBuf]| {
        files
            .iter()
            .filter_map(|file| code_index_logical_path(project_root, file.to_str()?))
            .collect::<BTreeSet<_>>()
    };
    let roots = logical(&program.roots);
    (program.roots.is_empty() || !roots.is_empty()).then(|| TypeScriptProgramMembershipV1 {
        roots,
        files: logical(&program.files),
    })
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

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::Path;

    #[cfg(unix)]
    use std::sync::Arc;

    use tracedecay_domain::test_fixtures::id;
    use tracedecay_lsp::TypeScriptCompilerAdmission;
    #[cfg(unix)]
    use tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1;
    #[cfg(unix)]
    use tracedecay_runtime_core::test_executable::write_executable_script;

    #[cfg(unix)]
    use super::TypeScriptProducerStateV1;
    #[cfg(unix)]
    use super::installed_compiler_tests::InstalledCompilerProject;
    use super::{
        CompilerProducerRunV1, TypeScriptDiagnosticsAvailabilityV1,
        TypeScriptProducerStatusHandleV1, TypeScriptProducerStatusV1,
        TypeScriptProgramMembershipV1, TypeScriptProjectCheckV1, TypeScriptProjectStatusV1,
        compiler_diagnostic, typescript_diagnostics_availability,
    };
    use crate::diagnose::Severity;

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

    fn membership(files: &[&str]) -> Option<TypeScriptProgramMembershipV1> {
        let files = files
            .iter()
            .map(|file| (*file).to_owned())
            .collect::<BTreeSet<_>>();
        Some(TypeScriptProgramMembershipV1::new(files.clone(), files))
    }

    async fn published(
        status: &TypeScriptProducerStatusHandleV1,
        snapshot: TypeScriptProducerStatusV1,
    ) {
        status
            .start_producer()
            .begin_publication()
            .await
            .unwrap()
            .publish(snapshot);
    }

    /// A read about a file reports the check of the project whose recorded
    /// program holds it, else the state of the nearest tsconfig that might,
    /// not whichever package happened to publish.
    #[tokio::test]
    async fn availability_reports_the_holding_or_nearest_projects_own_check() {
        let root = Path::new("/workspace");
        let compiler = root.join("node_modules/.bin/tsc");
        let app = root.join("packages/app/tsconfig.json");
        let lib = root.join("packages/lib/tsconfig.json");
        let failed = TypeScriptProjectCheckV1::Failed {
            reason: "TS5083: Cannot read file".to_owned(),
        };
        let status = TypeScriptProducerStatusHandleV1::default();
        assert_eq!(
            typescript_diagnostics_availability(root, None, &status, None),
            TypeScriptDiagnosticsAvailabilityV1::NoProducer
        );
        let writer = status.start_producer();
        assert_eq!(
            typescript_diagnostics_availability(root, None, &status, None),
            TypeScriptDiagnosticsAvailabilityV1::Discovering,
            "an appointed producer has not published what it found yet"
        );
        drop(writer);
        let generation: tracedecay_domain::CodeGenerationId = id("generation.availability");
        published(
            &status,
            TypeScriptProducerStatusV1::new(
                Some(CompilerProducerRunV1::CodeIndexGenerationUnavailable),
                Some(generation.clone()),
                BTreeMap::from([
                    (
                        app.clone(),
                        TypeScriptProjectStatusV1::Checked {
                            compiler: compiler.clone(),
                            membership: membership(&["packages/app/src/a.ts", "shared/util.ts"]),
                            references: Vec::new(),
                        },
                    ),
                    (
                        lib.clone(),
                        TypeScriptProjectStatusV1::Failed {
                            compiler: compiler.clone(),
                            reason: "TS5083: Cannot read file".to_owned(),
                            references: Vec::new(),
                        },
                    ),
                ]),
            ),
        )
        .await;

        let owner = |file: &str, generation| match typescript_diagnostics_availability(
            root,
            Some(Path::new(file)),
            &status,
            generation,
        ) {
            TypeScriptDiagnosticsAvailabilityV1::Configured {
                tsconfig, check, ..
            } => Some((tsconfig, check)),
            TypeScriptDiagnosticsAvailabilityV1::NoTsconfig { .. } => None,
            other => panic!("{file}: {other:?}"),
        };
        assert_eq!(
            owner("packages/lib/src/b.ts", Some(&generation)),
            Some((lib, Some(failed))),
            "lib's failed check may hold its sources"
        );
        assert_eq!(
            owner("shared/util.ts", Some(&generation)),
            Some((app.clone(), Some(TypeScriptProjectCheckV1::Checked))),
            "app's program read the shared file"
        );
        assert_eq!(
            owner("packages/app/src/new.ts", None),
            Some((app.clone(), Some(TypeScriptProjectCheckV1::Checked))),
            "a program recorded for another generation does not rule a file out"
        );
        let TypeScriptDiagnosticsAvailabilityV1::NoTsconfig { searched } =
            typescript_diagnostics_availability(
                root,
                Some(Path::new("packages/app/src/new.ts")),
                &status,
                Some(&generation),
            )
        else {
            panic!("app's program for this generation does not hold it");
        };
        assert_eq!(
            searched
                .iter()
                .map(|candidate| (candidate.path.to_str().unwrap(), candidate.present))
                .collect::<Vec<_>>(),
            [
                ("packages/app/src/tsconfig.json", false),
                ("packages/app/tsconfig.json", true),
                ("packages/tsconfig.json", false),
                ("tsconfig.json", false),
            ]
        );
        let TypeScriptDiagnosticsAvailabilityV1::Configured { tsconfig, .. } =
            typescript_diagnostics_availability(root, None, &status, None)
        else {
            panic!("a workspace read names the first project with a compiler");
        };
        assert_eq!(tsconfig, app);
    }

    #[tokio::test]
    async fn unrelated_failed_reference_leaves_file_membership_unknown() {
        let root = Path::new("/workspace");
        let solution = root.join("tsconfig.json");
        let app = root.join("packages/app/tsconfig.json");
        let compiler = root.join("node_modules/.bin/tsc");
        let generation: tracedecay_domain::CodeGenerationId = id("generation.failed-reference");
        let status = TypeScriptProducerStatusHandleV1::default();
        published(
            &status,
            TypeScriptProducerStatusV1::new(
                Some(CompilerProducerRunV1::CompilerFailed {
                    reason: "referenced check failed".to_owned(),
                }),
                Some(generation.clone()),
                BTreeMap::from([
                    (
                        solution,
                        TypeScriptProjectStatusV1::Checked {
                            compiler: compiler.clone(),
                            membership: membership(&[]),
                            references: vec![app.clone()],
                        },
                    ),
                    (
                        app.clone(),
                        TypeScriptProjectStatusV1::Failed {
                            compiler,
                            reason: "TS5083: Cannot read file".to_owned(),
                            references: Vec::new(),
                        },
                    ),
                ]),
            ),
        )
        .await;

        assert_eq!(
            typescript_diagnostics_availability(
                root,
                Some(Path::new("scripts/release.ts")),
                &status,
                Some(&generation),
            ),
            TypeScriptDiagnosticsAvailabilityV1::MembershipUnknown {
                reference: app.clone(),
                reason: "TS5083: Cannot read file".to_owned(),
            },
        );
        assert!(matches!(
            typescript_diagnostics_availability(
                root,
                Some(Path::new("packages/app/src/main.ts")),
                &status,
                Some(&generation),
            ),
            TypeScriptDiagnosticsAvailabilityV1::Configured {
                tsconfig,
                check: Some(TypeScriptProjectCheckV1::Failed { .. }),
                ..
            } if tsconfig == app
        ));
    }

    /// Every generation publishes all three projects' results, but tsc checks
    /// only a project the generation changed an input of: the edited package
    /// after a one-file edit, nothing that checked cleanly after an edit no
    /// program read. The failed package is retried on any change until it
    /// checks, and an unchanged generation runs no compiler at all.
    #[cfg(unix)]
    #[tokio::test]
    async fn only_projects_a_generation_changed_are_rechecked() {
        let project = monorepo();
        project.log_compiler_runs();
        let mut state = TypeScriptProducerStateV1::new(
            TypeScriptProducerStatusHandleV1::default(),
            admission(),
        );
        let both = vec![
            ("packages.app.src.index.ts".to_owned(), "TS2322".to_owned()),
            ("packages.lib.src.index.ts".to_owned(), "TS2322".to_owned()),
        ];

        assert_eq!(
            project.run("generation.producer.1", &mut state).await,
            published_run("generation.producer.1", 3, 0)
        );
        assert_eq!(project.checked(), CHECKED_ALL);
        assert_eq!(
            project.located_findings("generation.producer.1").await,
            both
        );

        std::fs::write(
            project.root.join("packages/app/src/index.ts"),
            "export const app: string = 2;\n",
        )
        .unwrap();
        assert_eq!(
            project.run("generation.producer.2", &mut state).await,
            published_run("generation.producer.2", 2, 1)
        );
        assert_eq!(
            project.checked(),
            [
                "packages/app/tsconfig.json",
                "packages/broken/tsconfig.json"
            ]
        );
        assert_eq!(
            project.located_findings("generation.producer.2").await,
            both
        );

        project.edit_readme("# edited\n");
        assert_eq!(
            project.run("generation.producer.3", &mut state).await,
            published_run("generation.producer.3", 1, 2)
        );
        assert_eq!(project.checked(), ["packages/broken/tsconfig.json"]);
        assert_eq!(
            project.located_findings("generation.producer.3").await,
            both
        );

        assert_eq!(
            project.run("generation.producer.4", &mut state).await,
            published_run("generation.producer.4", 0, 3)
        );
        assert_eq!(project.runs(), 0, "nothing changed, so no compiler ran");

        std::fs::write(
            project.root.join("packages/broken/tsconfig.json"),
            r#"{"include":["src"]}"#,
        )
        .unwrap();
        project.run("generation.producer.5", &mut state).await;
        assert_eq!(project.checked(), ["packages/broken/tsconfig.json"]);
        project.edit_readme("# repaired\n");
        assert_eq!(
            project.run("generation.producer.6", &mut state).await,
            published_run("generation.producer.6", 0, 3),
            "the repaired package checks cleanly and is reused like the others"
        );
    }

    #[cfg(unix)]
    const CHECKED_ALL: [&str; 3] = [
        "packages/app/tsconfig.json",
        "packages/broken/tsconfig.json",
        "packages/lib/tsconfig.json",
    ];

    /// Two packages with one real finding each and one whose base config is
    /// missing.
    #[cfg(unix)]
    fn monorepo() -> InstalledCompilerProject {
        InstalledCompilerProject::with_files(&[
            ("README.md", "# fixture\n"),
            ("packages/app/tsconfig.json", r#"{"include":["src"]}"#),
            (
                "packages/app/src/index.ts",
                "export const app: string = 1;\n",
            ),
            ("packages/lib/tsconfig.json", r#"{"include":["src"]}"#),
            (
                "packages/lib/src/index.ts",
                "export const lib: string = 1;\n",
            ),
            (
                "packages/broken/tsconfig.json",
                r#"{"extends":"./base.json","include":["src"]}"#,
            ),
            ("packages/broken/src/index.ts", "export const broken = 1;\n"),
        ])
    }

    #[cfg(unix)]
    const CHECKED_CLEAN: [&str; 2] = ["packages/app/tsconfig.json", "packages/lib/tsconfig.json"];

    /// The two checkable packages of [`monorepo`] alone.
    #[cfg(unix)]
    fn clean_monorepo() -> InstalledCompilerProject {
        InstalledCompilerProject::with_files(&[
            ("README.md", "# fixture\n"),
            ("packages/app/tsconfig.json", r#"{"include":["src"]}"#),
            (
                "packages/app/src/index.ts",
                "export const app: string = 1;\n",
            ),
            ("packages/lib/tsconfig.json", r#"{"include":["src"]}"#),
            (
                "packages/lib/src/index.ts",
                "export const lib: string = 1;\n",
            ),
        ])
    }

    #[cfg(unix)]
    fn published_run(generation: &str, checked: usize, reused: usize) -> CompilerProducerRunV1 {
        CompilerProducerRunV1::Published {
            generation: id(generation),
            inserted: 2,
            unresolved: 0,
            checked,
            reused,
        }
    }

    /// TypeScript is installed outside the indexed sources, so upgrading the
    /// compiler must invalidate clean reuse even when no source digest changes.
    #[cfg(unix)]
    #[tokio::test]
    async fn compiler_upgrade_or_missing_evidence_rechecks_unchanged_sources() {
        let project = clean_monorepo();
        project.log_compiler_runs();
        let mut state = TypeScriptProducerStateV1::new(
            TypeScriptProducerStatusHandleV1::default(),
            admission(),
        );
        let manifest = project.typescript.join("package.json");
        let installed = std::fs::read_to_string(&manifest).unwrap();
        let replace_manifest = |text: &str| {
            std::fs::remove_file(&manifest).unwrap();
            std::fs::write(&manifest, text).unwrap();
        };

        project.run("generation.compiler.1", &mut state).await;
        assert_eq!(project.checked(), CHECKED_CLEAN);
        project.edit_readme("# second\n");
        project.run("generation.compiler.2", &mut state).await;
        assert_eq!(project.checked(), Vec::<String>::new());

        replace_manifest(&format!("{installed}\n"));
        project.edit_readme("# third\n");
        project.run("generation.compiler.3", &mut state).await;
        assert_eq!(
            project.checked(),
            CHECKED_CLEAN,
            "a changed compiler package must not reuse earlier compiler evidence"
        );
        project.edit_readme("# fourth\n");
        project.run("generation.compiler.4", &mut state).await;
        assert_eq!(project.checked(), Vec::<String>::new());

        // The launcher reads this manifest to find the native compiler, so the
        // compiler now fails too; either way nothing is reused.
        replace_manifest("{ malformed");
        project.edit_readme("# fifth\n");
        let run = project.run("generation.compiler.5", &mut state).await;
        assert!(
            matches!(run, CompilerProducerRunV1::CompilerFailed { .. }),
            "unknown installed compiler evidence must not authorize clean reuse: {run:?}"
        );
        assert!(
            project.runs() >= CHECKED_CLEAN.len(),
            "every project ran again"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn compiler_launcher_replacement_invalidates_reuse() {
        let project = clean_monorepo();
        project.log_compiler_runs();
        let mut state = TypeScriptProducerStateV1::new(
            TypeScriptProducerStatusHandleV1::default(),
            admission(),
        );
        project.run("generation.launcher.1", &mut state).await;
        assert_eq!(project.checked(), CHECKED_CLEAN);
        let mut script = std::fs::read_to_string(project.launcher()).unwrap();
        script.push_str("\n# replacement launcher\n");
        std::fs::remove_file(project.launcher()).unwrap();
        write_executable_script(&project.launcher(), script).unwrap();

        project.edit_readme("# second\n");
        project.run("generation.launcher.2", &mut state).await;
        assert_eq!(project.checked(), CHECKED_CLEAN);
    }

    /// A producer run reports to the handle of the owner it was built for. A
    /// successor owner opened at the same path reads no status until its own
    /// producer runs, so the predecessor's run is never taken for its own.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_successor_owner_does_not_inherit_the_predecessors_status() {
        let project = monorepo();
        let predecessor = TypeScriptProducerStatusHandleV1::default();
        let mut state = TypeScriptProducerStateV1::new(predecessor.clone(), admission());
        let run = project.run("generation.successor.1", &mut state).await;
        assert_eq!(
            predecessor
                .current()
                .and_then(|status| status.run().cloned()),
            Some(run)
        );

        let file = Path::new("packages/app/src/index.ts");
        let successor = TypeScriptProducerStatusHandleV1::default();
        let _successor_producer = TypeScriptProducerStateV1::new(successor.clone(), admission());
        assert_eq!(
            typescript_diagnostics_availability(&project.root, Some(file), &successor, None),
            TypeScriptDiagnosticsAvailabilityV1::Discovering
        );
        let TypeScriptDiagnosticsAvailabilityV1::Configured {
            tsconfig, check, ..
        } = typescript_diagnostics_availability(
            &project.root,
            Some(file),
            &predecessor,
            Some(&id("generation.successor.1")),
        )
        else {
            panic!("app is checkable");
        };
        assert_eq!(
            (tsconfig, check),
            (
                project.root.join("packages/app/tsconfig.json"),
                Some(TypeScriptProjectCheckV1::Checked)
            )
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn isolated_producers_share_compiler_admission_and_keep_partial_findings() {
        let projects = [monorepo(), monorepo(), monorepo()];
        let counter = tempfile::tempdir().unwrap();
        for project in &projects {
            project.log_compiler_runs();
            let script = std::fs::read_to_string(project.launcher()).unwrap();
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
            std::fs::remove_file(project.launcher()).unwrap();
            write_executable_script(&project.launcher(), script).unwrap();
        }
        let admission = admission();
        let mut states = projects
            .iter()
            .map(|_| {
                TypeScriptProducerStateV1::new(
                    TypeScriptProducerStatusHandleV1::default(),
                    admission.clone(),
                )
            })
            .collect::<Vec<_>>();
        let runs = futures_util::future::join_all(
            projects
                .iter()
                .zip(states.iter_mut())
                .map(|(project, state)| project.run("generation.shared", state)),
        )
        .await;
        assert!(
            runs.iter()
                .all(|run| run == &published_run("generation.shared", 3, 0)),
            "{runs:?}"
        );
        for project in &projects {
            assert_eq!(project.checked(), CHECKED_ALL);
            assert_eq!(project.located_findings("generation.shared").await.len(), 2);
        }
        let counts = std::fs::read_to_string(counter.path().join("seen")).unwrap();
        let counts = counts
            .lines()
            .map(|line| line.trim().parse::<usize>().unwrap())
            .collect::<Vec<_>>();
        assert!(
            counts.len() >= CHECKED_ALL.len() * projects.len(),
            "{counts:?}"
        );
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

    /// Without a compiler nothing says what a tsconfig holds, so a read names
    /// the nearest one's missing compiler rather than claiming it owns the
    /// file, and with no tsconfig at all there is nothing to install for.
    #[tokio::test]
    async fn discovery_alone_reports_missing_compilers_without_claiming_membership() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        for (path, text) in [
            ("pnpm-lock.yaml", "lockfileVersion: '9.0'\n"),
            ("packages/app/tsconfig.json", "{}"),
            ("packages/lib/tsconfig.json", "{}"),
        ] {
            std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
            std::fs::write(root.join(path), text).unwrap();
        }
        let status = TypeScriptProducerStatusHandleV1::default();
        let mut state = super::TypeScriptProducerStateV1::new(
            status.clone(),
            TypeScriptCompilerAdmission::new(std::sync::Arc::new(
                tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1::new(
                    std::num::NonZeroUsize::new(1).unwrap(),
                ),
            )),
        );
        let run = super::publish_typescript_discovery_v1(
            root,
            &tracedecay_lsp::typescript_projects(root).expect("tsconfig discovery"),
            &mut state,
        )
        .await;
        assert!(
            matches!(run, Some(CompilerProducerRunV1::CompilerFailed { .. })),
            "{run:?}"
        );
        for (file, tsconfig) in [
            (
                Some("packages/lib/src/index.ts"),
                "packages/lib/tsconfig.json",
            ),
            (None, "packages/app/tsconfig.json"),
        ] {
            assert_eq!(
                typescript_diagnostics_availability(root, file.map(Path::new), &status, None),
                TypeScriptDiagnosticsAvailabilityV1::CompilerMissing {
                    tsconfig: root.join(tsconfig),
                    install_command: "pnpm install",
                },
                "{file:?}"
            );
        }
        let TypeScriptDiagnosticsAvailabilityV1::NoTsconfig { searched } =
            typescript_diagnostics_availability(
                root,
                Some(Path::new("scripts/release.ts")),
                &status,
                None,
            )
        else {
            panic!("no tsconfig is near the script");
        };
        assert!(
            searched.iter().all(|candidate| !candidate.present),
            "{searched:?}"
        );

        let empty = TypeScriptProducerStatusHandleV1::default();
        let mut state =
            super::TypeScriptProducerStateV1::new(empty.clone(), state.admission.clone());
        super::publish_typescript_discovery_v1(root, &[], &mut state).await;
        assert_eq!(
            typescript_diagnostics_availability(root, None, &empty, None),
            TypeScriptDiagnosticsAvailabilityV1::NoTsconfig {
                searched: Vec::new()
            }
        );
    }
}
