//! Cross-file resolution of an edit from its parent's resolution outputs,
//! re-deciding only the call sites the edited files can move.
//!
//! Every resolver binds a reference to a symbol it looks up by simple name:
//! a segment of the reference, or a name an import forwards it under. Module
//! paths, package clauses, and manifests decide which file a lookup lands in.
//! So when the edited files keep their paths, languages, imports, package
//! declarations, and manifests, a site's outcome can move only if one of its
//! references can reach a name some edited file declares before or after
//! the edit, directly or through an import alias. Every other site keeps the
//! edges and call limitations the parent derived for it.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

use tracedecay_code_extraction::{ImportModuleKindV1, ImportNamespaceV1, ImportReexportScopeV1};
use tracedecay_domain::{
    CanonicalRelationEdgeV1, EdgeAuthorityV1, FileOccurrenceId, NodeKind, SourceSpan,
    SymbolOccurrenceId,
};
#[cfg(test)]
use tracedecay_graph_db::GraphDbError;

use crate::chunks::{
    CodeIndexEdgeAbstentionV1, CodeIndexImportEvidenceV1, CodeIndexUnresolvedReferenceV1,
};
#[cfg(test)]
use crate::graph_projection::{SealedCodeGraphRowsError, unresolved_call_limitations};

#[cfg(test)]
use super::helpers::unresolved_import_calls;
use super::helpers::{
    collect_edge_evidence, edge_evidence, edge_order, resolve_selected_cross_file_references,
    selected_references,
};
use super::{
    CodeIndexProductionErrorV1, CodeIndexPublishedGenerationV1, FileGenerationArtifactsV1,
};

type SiteV1<'a> = (&'a SymbolOccurrenceId, SourceSpan);

/// Each edited file's child index, with the file its path held in the parent.
pub(super) type EditedFilesV1 = Vec<(usize, Arc<FileGenerationArtifactsV1>)>;

/// The call sites an in-place edit can move, and the references that decide
/// them.
pub(super) struct ChangedSitesV1<'f> {
    /// Every reference of each site some edited name can reach, and every
    /// reference of the edited files.
    selection: Vec<(usize, Vec<usize>)>,
    /// Those sites and the edited files' sites before the edit: where the
    /// parent's outputs no longer hold.
    moved: HashSet<SiteV1<'f>>,
}

impl<'f> ChangedSitesV1<'f> {
    /// The sites of `files` an edit can move, where each `edited` child index
    /// held the paired file when the parent resolved. `None` when an edited
    /// file moves what name lookups land in, which only a whole resolution
    /// decides.
    pub(super) fn new(
        files: &'f [Arc<FileGenerationArtifactsV1>],
        edited: &'f [(usize, Arc<FileGenerationArtifactsV1>)],
    ) -> Option<Self> {
        if edited
            .iter()
            .any(|(index, before)| moves_name_lookups(before, &files[*index]))
        {
            return None;
        }
        let names = reachable_changed_names(files, edited);
        let edited_indices = edited
            .iter()
            .map(|(index, _)| *index)
            .collect::<HashSet<_>>();
        let mut selection = Vec::new();
        for (index, file) in files.iter().enumerate() {
            let references = &file.artifacts.unresolved_references;
            let picks = if edited_indices.contains(&index) {
                (0..references.len()).collect::<Vec<_>>()
            } else {
                let sites = references
                    .iter()
                    .filter(|reference| names.can_move(&reference.reference_name))
                    .map(site)
                    .collect::<HashSet<_>>();
                if sites.is_empty() {
                    continue;
                }
                (0..references.len())
                    .filter(|&pick| sites.contains(&site(&references[pick])))
                    .collect()
            };
            if !picks.is_empty() {
                selection.push((index, picks));
            }
        }
        let moved =
            selected_references(files, Some(&selection))
                .map(|(_, reference)| site(reference))
                .chain(edited.iter().flat_map(|(_, before)| {
                    before.artifacts.unresolved_references.iter().map(site)
                }))
                .collect();
        Some(Self { selection, moved })
    }

    /// References re-decided at the moved sites.
    #[cfg(feature = "hotpath")]
    pub(super) fn resolved_references(&self) -> usize {
        self.selection.iter().map(|(_, picks)| picks.len()).sum()
    }

    /// `files`' cross-file edges: the parent's `parent_edges` where they
    /// still hold, re-resolved at the moved sites.
    pub(super) fn cross_file_edges<'e>(
        &self,
        files: &[Arc<FileGenerationArtifactsV1>],
        parent_edges: impl Iterator<Item = &'e CanonicalRelationEdgeV1>,
    ) -> Result<Vec<CanonicalRelationEdgeV1>, CodeIndexProductionErrorV1> {
        let resolved = resolve_selected_cross_file_references(files, &self.selection)?;
        let mut edges = parent_edges
            .filter(|edge| {
                !self
                    .moved
                    .contains(&(&edge.from_occurrence, edge.evidence_span))
            })
            .cloned()
            .chain(resolved)
            .collect::<Vec<_>>();
        edges.sort_by(edge_order);
        edges.dedup();
        Ok(edges)
    }

    /// `files`' call limitations: the parent's `parent_unresolved` where they
    /// still hold, re-derived at the moved sites against `cross_file_edges`.
    /// A site's limitations depend only on its own references and the edges
    /// bound at it, and the selection carries every reference of each site.
    #[cfg(test)]
    pub(super) fn unresolved_calls(
        &self,
        files: &[Arc<FileGenerationArtifactsV1>],
        cross_file_edges: &[CanonicalRelationEdgeV1],
        parent_unresolved: &[CodeIndexUnresolvedReferenceV1],
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<Vec<CodeIndexUnresolvedReferenceV1>, SealedCodeGraphRowsError> {
        let references = selected_references(files, Some(&self.selection))
            .map(|(index, reference)| (files[index].authority.logical_path.as_str(), reference))
            .collect::<Vec<_>>();
        let sites = references
            .iter()
            .map(|(_, reference)| site(reference))
            .collect::<HashSet<_>>();
        let at_sites = |edge: &&CanonicalRelationEdgeV1| {
            sites.contains(&(&edge.from_occurrence, edge.evidence_span))
        };
        let file_edges = self
            .selection
            .iter()
            .flat_map(|(index, _)| files[*index].artifacts.edges.iter())
            .filter(at_sites);
        let rederived = unresolved_call_limitations(
            &references,
            file_edges.chain(cross_file_edges.iter().filter(at_sites)),
            unresolved_import_calls(files, Some(&self.selection)),
            check,
        )?;
        let mut unresolved = parent_unresolved
            .iter()
            .filter(|reference| !self.moved.contains(&site(reference)))
            .cloned()
            .chain(rederived)
            .collect::<Vec<_>>();
        unresolved.sort();
        unresolved.dedup();
        Ok(unresolved)
    }
}

/// Pairs each file of `files` that `is_edited` marks with the `candidates`
/// file at its path, and counts the candidates left unpaired. `None` when an
/// edited path has no candidate.
pub(super) fn pair_edited_files(
    files: &[Arc<FileGenerationArtifactsV1>],
    is_edited: impl Fn(usize) -> bool,
    candidates: impl Iterator<Item = Arc<FileGenerationArtifactsV1>>,
) -> Option<(EditedFilesV1, usize)> {
    let mut by_path = candidates
        .map(|file| (file.authority.logical_path.clone(), file))
        .collect::<BTreeMap<_, _>>();
    let mut edited = Vec::new();
    for (index, file) in files.iter().enumerate() {
        if is_edited(index) {
            edited.push((index, by_path.remove(&file.authority.logical_path)?));
        }
    }
    Some((edited, by_path.len()))
}

/// A generation's edge evidence built over `parent`, whose files it shares
/// except those outside `shared`: each file's own edges, and cross-file
/// edges re-resolved only where the edit can move them.
pub(super) fn edge_evidence_over_parent(
    files: &[Arc<FileGenerationArtifactsV1>],
    parent: &CodeIndexPublishedGenerationV1,
    shared: &BTreeSet<FileOccurrenceId>,
) -> Result<
    (Vec<CanonicalRelationEdgeV1>, Vec<CodeIndexEdgeAbstentionV1>),
    CodeIndexProductionErrorV1,
> {
    // Shared files are the same files on both sides, so equal file counts
    // and a parent file at every edited path mean the same set of paths.
    let edited = pair_edited_files(
        files,
        |index| !shared.contains(&files[index].artifacts.chunks.document.file_occurrence_id),
        parent.files.iter().cloned(),
    )
    .filter(|_| parent.files.len() == files.len());
    let Some(sites) = edited
        .as_ref()
        .and_then(|(edited, _)| ChangedSitesV1::new(files, edited))
    else {
        return collect_edge_evidence(files);
    };
    #[cfg(feature = "hotpath")]
    hotpath::gauge!("code_index.build.references_resolved").inc(sites.resolved_references() as u64);
    // Only cross-file resolution emits name-resolved edges.
    let cross_file = sites.cross_file_edges(
        files,
        parent
            .edges
            .iter()
            .filter(|edge| edge.authority == EdgeAuthorityV1::NameResolved),
    )?;
    Ok(edge_evidence(files, cross_file))
}

fn site(reference: &CodeIndexUnresolvedReferenceV1) -> SiteV1<'_> {
    (&reference.from_occurrence, reference.evidence_span)
}

/// Whether the edit from `before` to `after` changes an input that decides
/// which file a name lookup lands in rather than which names it finds.
fn moves_name_lookups(
    before: &FileGenerationArtifactsV1,
    after: &FileGenerationArtifactsV1,
) -> bool {
    before.authority.logical_path != after.authority.logical_path
        || before.extraction.language != after.extraction.language
        || is_resolution_manifest(
            &after.authority.logical_path,
            after.extraction.language.as_str(),
        )
        || import_shapes(before) != import_shapes(after)
        || package_declarations(before) != package_declarations(after)
}

/// Manifests whose symbols name crates, Go modules, workspace packages, and
/// TypeScript path aliases.
fn is_resolution_manifest(logical_path: &str, language: &str) -> bool {
    let name = logical_path.rsplit('/').next().unwrap_or(logical_path);
    matches!(language, "json" | "toml") || name == "go.mod"
}

/// A file's imports without their source positions and the importing file's
/// occurrence, which an edit moves and no resolver reads.
fn import_shapes(file: &FileGenerationArtifactsV1) -> Vec<ImportShapeV1<'_>> {
    file.artifacts
        .imports
        .iter()
        .map(|binding| {
            let CodeIndexImportEvidenceV1 {
                logical_path,
                file_occurrence_id: _,
                module_specifier,
                imported_name,
                local_name,
                is_public,
                reexport_scope,
                is_glob,
                namespace,
                module_kind,
                span: _,
                start_line: _,
                start_column: _,
            } = binding;
            (
                logical_path,
                module_specifier,
                imported_name,
                local_name,
                *is_public,
                reexport_scope,
                *is_glob,
                namespace,
                module_kind,
            )
        })
        .collect()
}

type ImportShapeV1<'a> = (
    &'a String,
    &'a String,
    &'a Option<String>,
    &'a Option<String>,
    bool,
    &'a Option<ImportReexportScopeV1>,
    bool,
    &'a ImportNamespaceV1,
    &'a ImportModuleKindV1,
);

/// The module and package clauses a file declares.
fn package_declarations(file: &FileGenerationArtifactsV1) -> BTreeMap<&str, &str> {
    file.artifacts
        .symbols
        .iter()
        .filter(|symbol| {
            symbol.kind == NodeKind::Module.as_str()
                || symbol.kind == NodeKind::GoPackage.as_str()
                || symbol.kind == NodeKind::Package.as_str()
        })
        .map(|symbol| (symbol.qualified_name.as_str(), symbol.kind.as_str()))
        .collect()
}

/// The names under which a lookup can land on a changed file's symbol.
///
/// A module's name moves a reference only as its last segment, the module
/// itself as the target: a path through the module walks the module tree,
/// which [`package_declarations`] holds fixed. Every other symbol's name
/// moves a reference wherever it appears, as a lookup key or as the owner
/// a member is looked up under.
struct ChangedNamesV1<'a> {
    anywhere: HashSet<&'a str>,
    last: HashSet<&'a str>,
}

impl<'a> ChangedNamesV1<'a> {
    fn can_move(&self, reference_name: &str) -> bool {
        name_segments(reference_name).any(|segment| self.anywhere.contains(segment))
            || name_segments(reference_name)
                .last()
                .is_some_and(|segment| self.last.contains(segment))
    }

    /// Adds the local name of each import that forwards a name already
    /// held, under the same placement, until no import adds one.
    fn forward_through_imports(&mut self, files: &'a [Arc<FileGenerationArtifactsV1>]) {
        let aliases = files
            .iter()
            .flat_map(|file| file.artifacts.imports.iter())
            .filter_map(|binding| {
                let local = binding.local_name.as_deref()?;
                let imported = binding.imported_name.as_deref()?;
                (local != imported).then_some((local, imported))
            })
            .collect::<Vec<_>>();
        loop {
            let before = self.anywhere.len() + self.last.len();
            for &(local, imported) in &aliases {
                if self.anywhere.contains(imported) {
                    self.anywhere.insert(local);
                }
                if self.last.contains(imported) {
                    self.last.insert(local);
                }
            }
            if self.anywhere.len() + self.last.len() == before {
                return;
            }
        }
    }
}

/// The changed files' symbols' names before and after the edit, and the
/// local name of each import that forwards such a name, under the same
/// placement. A lookup only ever follows an import from its local name to
/// the name it imports.
fn reachable_changed_names<'a>(
    files: &'a [Arc<FileGenerationArtifactsV1>],
    changed: &'a [(usize, Arc<FileGenerationArtifactsV1>)],
) -> ChangedNamesV1<'a> {
    let mut names = ChangedNamesV1 {
        anywhere: HashSet::new(),
        last: HashSet::new(),
    };
    for (index, before) in changed {
        for symbol in before
            .artifacts
            .symbols
            .iter()
            .chain(&files[*index].artifacts.symbols)
        {
            let placed = if symbol.kind == NodeKind::Module.as_str() {
                &mut names.last
            } else {
                &mut names.anywhere
            };
            placed.insert(symbol.simple_name.as_str());
            placed.extend(name_segments(&symbol.simple_name));
        }
    }
    names.forward_through_imports(files);
    names
}

/// The identifiers a reference or symbol name joins with `::` and `.`.
fn name_segments(name: &str) -> impl Iterator<Item = &str> {
    name.split("::")
        .flat_map(|part| part.split('.'))
        .filter(|segment| !segment.is_empty())
}
