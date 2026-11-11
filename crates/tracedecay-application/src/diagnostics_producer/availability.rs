//! What a read can truthfully say about the TypeScript producer for the file
//! it asked about.
//!
//! A read never reads a configuration or runs a compiler. It answers from
//! what the producer's last run published: the project whose recorded
//! program holds the file owns it. Otherwise the tsconfig locations tsserver
//! would search (the file's directory up to the project root, and the
//! projects each one references) name the state that keeps the question
//! open, and only when the compiler recorded every one of them, for the
//! generation the read is about, without the file is the file unowned.

use std::path::{Path, PathBuf};

use tracedecay_domain::CodeGenerationId;

use super::status::{
    CompilerProducerRunV1, TypeScriptProducerStatusHandleV1, TypeScriptProducerStatusV1,
    TypeScriptProjectCheckV1, TypeScriptProjectStatusV1,
};
use crate::diagnostics_publication::code_index_logical_path;

const TSCONFIG: &str = "tsconfig.json";

/// A `tsconfig.json` location the owner search checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchedTsconfigV1 {
    /// Relative to the project root.
    pub path: PathBuf,
    /// The config exists, and its compiler recorded neither its program nor
    /// those of its references holding the file.
    pub present: bool,
}

/// The typed state a read can report about the TypeScript producer for the
/// project that owns what it asked about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeScriptDiagnosticsAvailabilityV1 {
    /// The project has a compiler; how its last check went and the
    /// producer's most recent run.
    Configured {
        tsconfig: PathBuf,
        compiler: PathBuf,
        check: Option<TypeScriptProjectCheckV1>,
        last_run: Option<CompilerProducerRunV1>,
    },
    /// The tsconfig nearest the file, or for a workspace read the first one,
    /// has no `node_modules/.bin/tsc`; `install_command` installs the
    /// workspace's dependencies. Without a compiler nothing says what the
    /// project holds.
    CompilerMissing {
        tsconfig: PathBuf,
        install_command: &'static str,
    },
    /// A referenced project's failed check did not establish whether it
    /// includes this file. Its failure cannot be assigned to the file.
    MembershipUnknown { reference: PathBuf },
    /// No tsconfig owns the file; where the owner search looked, nearest
    /// first. Empty for a workspace read, which found none under the root,
    /// and for a file outside it.
    NoTsconfig { searched: Vec<SearchedTsconfigV1> },
    /// A producer is appointed but has not published what it found yet.
    Discovering,
    /// No producer was appointed for this project's route, so none will run.
    NoProducer,
}

/// Resolves what a read can truthfully say about the TypeScript producer: the
/// project whose program holds `file` (or, for a workspace read, the first
/// checkable one) and the owner's last published run.
///
/// `generation` is the generation whose publication the read found, `None`
/// when it found none. Programs recorded for another generation prove only
/// that a file was in them: a project that did not hold the file then may
/// hold it now, so it is reported rather than ruled out.
#[must_use]
pub fn typescript_diagnostics_availability(
    project_root: &Path,
    file: Option<&Path>,
    status: &TypeScriptProducerStatusHandleV1,
    generation: Option<&CodeGenerationId>,
) -> TypeScriptDiagnosticsAvailabilityV1 {
    let Some(snapshot) = status.current() else {
        return if status.appointed() {
            TypeScriptDiagnosticsAvailabilityV1::Discovering
        } else {
            TypeScriptDiagnosticsAvailabilityV1::NoProducer
        };
    };
    let Some(file) = file else {
        return workspace_availability(&snapshot);
    };
    let Some(logical) = project_root
        .join(file)
        .to_str()
        .and_then(|file| code_index_logical_path(project_root, file))
    else {
        return TypeScriptDiagnosticsAvailabilityV1::NoTsconfig {
            searched: Vec::new(),
        };
    };
    if let Some(owner) = program_owner(project_root, &snapshot, &logical) {
        return owner;
    }
    let search = OwnerSearch {
        snapshot: &snapshot,
        current: generation.is_some() && snapshot.generation.as_ref() == generation,
    };
    let mut searched = Vec::new();
    let mut dir = Path::new(&logical).parent();
    while let Some(current) = dir {
        let relative = current.join(TSCONFIG);
        let candidate = project_root.join(&relative);
        let project = snapshot.projects.get(&candidate);
        if let Some(project) = project
            && let Some(open) = search.open_state(&candidate, project, &mut Vec::new(), false)
        {
            return open;
        }
        searched.push(SearchedTsconfigV1 {
            path: relative,
            present: project.is_some(),
        });
        dir = current.parent();
    }
    TypeScriptDiagnosticsAvailabilityV1::NoTsconfig { searched }
}

/// The checked project whose recorded program holds `logical`: one naming it
/// as a root before one that only imports it, then the nearest enclosing
/// tsconfig.
fn program_owner(
    project_root: &Path,
    snapshot: &TypeScriptProducerStatusV1,
    logical: &str,
) -> Option<TypeScriptDiagnosticsAvailabilityV1> {
    let file = project_root.join(logical);
    snapshot
        .projects
        .iter()
        .filter_map(|(tsconfig, project)| match project {
            TypeScriptProjectStatusV1::Checked {
                compiler,
                membership: Some(membership),
                ..
            } if membership.files.contains(logical) => {
                let enclosing = tsconfig
                    .parent()
                    .filter(|dir| file.starts_with(dir))
                    .map_or(0, |dir| dir.components().count());
                Some((
                    (membership.roots.contains(logical), enclosing),
                    tsconfig,
                    compiler,
                ))
            }
            _ => None,
        })
        // The first of equally ranked projects, in path order, owns it.
        .rev()
        .max_by_key(|(rank, _, _)| *rank)
        .map(
            |(_, tsconfig, compiler)| TypeScriptDiagnosticsAvailabilityV1::Configured {
                tsconfig: tsconfig.clone(),
                compiler: compiler.clone(),
                check: Some(TypeScriptProjectCheckV1::Checked),
                last_run: snapshot.run.clone(),
            },
        )
}

struct OwnerSearch<'a> {
    snapshot: &'a TypeScriptProducerStatusV1,
    /// Whether the recorded programs describe the generation the read is
    /// about.
    current: bool,
}

impl OwnerSearch<'_> {
    /// The state of `tsconfig` or a project it references that may hold the
    /// file without the published programs saying so; `None` when the
    /// compiler recorded all of them without it.
    fn open_state(
        &self,
        tsconfig: &Path,
        project: &TypeScriptProjectStatusV1,
        visiting: &mut Vec<PathBuf>,
        referenced: bool,
    ) -> Option<TypeScriptDiagnosticsAvailabilityV1> {
        let configured =
            |compiler: &PathBuf, check| TypeScriptDiagnosticsAvailabilityV1::Configured {
                tsconfig: tsconfig.to_path_buf(),
                compiler: compiler.clone(),
                check: Some(check),
                last_run: self.snapshot.run.clone(),
            };
        match project {
            TypeScriptProjectStatusV1::CompilerMissing { install_command } => {
                Some(TypeScriptDiagnosticsAvailabilityV1::CompilerMissing {
                    tsconfig: tsconfig.to_path_buf(),
                    install_command,
                })
            }
            TypeScriptProjectStatusV1::Checking { compiler } => {
                Some(TypeScriptDiagnosticsAvailabilityV1::Configured {
                    tsconfig: tsconfig.to_path_buf(),
                    compiler: compiler.clone(),
                    check: None,
                    last_run: self.snapshot.run.clone(),
                })
            }
            TypeScriptProjectStatusV1::Failed {
                compiler, reason, ..
            } => Some(if referenced {
                TypeScriptDiagnosticsAvailabilityV1::MembershipUnknown {
                    reference: tsconfig.to_path_buf(),
                }
            } else {
                configured(
                    compiler,
                    TypeScriptProjectCheckV1::Failed {
                        reason: reason.clone(),
                    },
                )
            }),
            TypeScriptProjectStatusV1::Checked {
                compiler,
                membership,
                references,
            } => {
                if membership.is_none() || !self.current {
                    return Some(configured(compiler, TypeScriptProjectCheckV1::Checked));
                }
                visiting.push(tsconfig.to_path_buf());
                let mut open = None;
                for reference in references {
                    if visiting.contains(reference) {
                        continue;
                    }
                    open = match self.snapshot.projects.get(reference) {
                        Some(referenced) => self.open_state(reference, referenced, visiting, true),
                        // A reference outside the producer's record, such as
                        // one outside the project root, proves nothing.
                        None => Some(configured(compiler, TypeScriptProjectCheckV1::Checked)),
                    };
                    if open.is_some() {
                        break;
                    }
                }
                visiting.pop();
                open
            }
        }
    }
}

/// A workspace read reports the first project with a compiler, else the
/// first without one, else that there is no tsconfig.
fn workspace_availability(
    snapshot: &TypeScriptProducerStatusV1,
) -> TypeScriptDiagnosticsAvailabilityV1 {
    let checkable = snapshot
        .projects
        .iter()
        .find_map(|(tsconfig, project)| match project {
            TypeScriptProjectStatusV1::Checked { compiler, .. } => {
                Some(TypeScriptDiagnosticsAvailabilityV1::Configured {
                    tsconfig: tsconfig.clone(),
                    compiler: compiler.clone(),
                    check: Some(TypeScriptProjectCheckV1::Checked),
                    last_run: snapshot.run.clone(),
                })
            }
            TypeScriptProjectStatusV1::Failed {
                compiler, reason, ..
            } => Some(TypeScriptDiagnosticsAvailabilityV1::Configured {
                tsconfig: tsconfig.clone(),
                compiler: compiler.clone(),
                check: Some(TypeScriptProjectCheckV1::Failed {
                    reason: reason.clone(),
                }),
                last_run: snapshot.run.clone(),
            }),
            TypeScriptProjectStatusV1::Checking { compiler } => {
                Some(TypeScriptDiagnosticsAvailabilityV1::Configured {
                    tsconfig: tsconfig.clone(),
                    compiler: compiler.clone(),
                    check: None,
                    last_run: snapshot.run.clone(),
                })
            }
            TypeScriptProjectStatusV1::CompilerMissing { .. } => None,
        });
    checkable
        .or_else(|| {
            snapshot
                .projects
                .iter()
                .find_map(|(tsconfig, project)| match project {
                    TypeScriptProjectStatusV1::CompilerMissing { install_command } => {
                        Some(TypeScriptDiagnosticsAvailabilityV1::CompilerMissing {
                            tsconfig: tsconfig.clone(),
                            install_command,
                        })
                    }
                    _ => None,
                })
        })
        .unwrap_or(TypeScriptDiagnosticsAvailabilityV1::NoTsconfig {
            searched: Vec::new(),
        })
}
