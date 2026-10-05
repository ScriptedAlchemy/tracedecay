//! Cross-file resolution of an edit over its sealed parent, re-deciding only
//! the call sites the edited files can move.
//!
//! Every resolver binds a reference to a symbol it looks up by simple name:
//! a segment of the reference, or a name an import forwards it under. Module
//! paths, package clauses, and manifests decide which file a lookup lands in.
//! So when the edited files keep their paths, languages, imports, package
//! declarations, and manifests, a site's outcome can move only if one of its
//! references can reach a name some edited file declares before or after
//! the edit, directly or through an import alias. Every other site keeps the
//! edges and call limitations its file sealed in the parent.
//!
//! The parent's resolution index names the files whose references spell such
//! a name, and answers every simple-name lookup, so resolution decodes only
//! the edited files, those referencing files, and the files a lookup walks
//! into.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, btree_map};
use std::sync::{Arc, OnceLock};

use tracedecay_code_extraction::{ImportModuleKindV1, ImportNamespaceV1, ImportReexportScopeV1};
use tracedecay_domain::{
    CanonicalRelationEdgeV1, FileOccurrenceId, NodeKind, SourceSpan, SymbolIdentityDigest,
    SymbolOccurrenceId,
};

use crate::chunks::{CodeIndexImportEvidenceV1, CodeIndexUnresolvedReferenceV1};
use crate::graph_projection::unresolved_call_limitations;
use crate::lineage::LineageSymbolRecordV1;

use super::helpers::{
    edge_order, resolve_selected_cross_file_references, selected_references,
    unresolved_import_calls,
};
use super::resolution_index::{ResolutionIndexReaderV1, name_segments};
use super::resolution_view::{NamedSymbolV1, ResolutionFileV1, SymbolsByNameV1};
use super::sealed_parent::{DecodeFailureV1, SealedParentGenerationV1, SparseFileV1};
use super::{CodeIndexProductionErrorV1, FileGenerationArtifactsV1, collect_bounded_ordered};

type SiteV1<'a> = (&'a SymbolOccurrenceId, SourceSpan);

fn contract(message: &str) -> CodeIndexProductionErrorV1 {
    CodeIndexProductionErrorV1::Contract(message.to_owned())
}

fn site(reference: &CodeIndexUnresolvedReferenceV1) -> SiteV1<'_> {
    (&reference.from_occurrence, reference.evidence_span)
}

/// Whether the edit from `before` to `after` changes an input that decides
/// which file a name lookup lands in rather than which names it finds. Such
/// an edit is resolved whole.
pub(super) fn moves_name_lookups(
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

/// What resolution reads of a symbol it finds by name. Its occurrence is
/// not among them: candidates are chosen as a set, so a symbol whose file
/// only changed occurrence binds the same references.
type ResolutionKeyV1<'s> = (
    &'s SymbolIdentityDigest,
    &'s str,
    &'s str,
    &'s str,
    &'s Option<String>,
    bool,
    &'s [String],
);

fn resolution_key(symbol: &LineageSymbolRecordV1) -> ResolutionKeyV1<'_> {
    (
        &symbol.identity,
        &symbol.qualified_name,
        &symbol.kind,
        &symbol.visibility,
        &symbol.signature,
        symbol.is_async,
        &symbol.derives,
    )
}

/// The names under which a lookup can land on a changed file's symbol.
///
/// A module's name moves a reference only as its last segment, the module
/// itself as the target: a path through the module walks the module tree,
/// which [`package_declarations`] holds fixed. Every other symbol's name
/// moves a reference wherever it appears, as a lookup key or as the owner
/// a member is looked up under.
struct ChangedNamesV1 {
    anywhere: HashSet<String>,
    last: HashSet<String>,
    /// Names whose symbols resolution reads identically on both sides of the
    /// edit. A reference to one binds as before; only an edge into an edited
    /// file is re-pointed at that file's new occurrence.
    carried: HashSet<String>,
}

impl ChangedNamesV1 {
    /// The names of the edited files' symbols that resolution reads
    /// differently after the edit, and the local name of each import alias
    /// that forwards such a name, under the same placement. A lookup only
    /// ever follows an import from its local name to the name it imports.
    fn new(
        edited: &[(
            usize,
            Arc<FileGenerationArtifactsV1>,
            Arc<FileGenerationArtifactsV1>,
        )],
        aliases: &BTreeSet<(String, String)>,
    ) -> Self {
        let mut names = Self {
            anywhere: HashSet::new(),
            last: HashSet::new(),
            carried: HashSet::new(),
        };
        for (_, before, after) in edited {
            let mut sides = HashMap::<&str, [Vec<ResolutionKeyV1<'_>>; 2]>::new();
            for (side, file) in [before, after].into_iter().enumerate() {
                for symbol in &file.artifacts.symbols {
                    sides.entry(&symbol.simple_name).or_default()[side]
                        .push(resolution_key(symbol));
                }
            }
            for symbol in before
                .artifacts
                .symbols
                .iter()
                .chain(&after.artifacts.symbols)
            {
                if sides[symbol.simple_name.as_str()][0] == sides[symbol.simple_name.as_str()][1] {
                    names.carried.insert(symbol.simple_name.clone());
                    names
                        .carried
                        .extend(name_segments(&symbol.simple_name).map(str::to_owned));
                    continue;
                }
                let placed = if symbol.kind == NodeKind::Module.as_str() {
                    &mut names.last
                } else {
                    &mut names.anywhere
                };
                placed.insert(symbol.simple_name.clone());
                placed.extend(name_segments(&symbol.simple_name).map(str::to_owned));
            }
        }
        loop {
            let before = names.anywhere.len() + names.last.len() + names.carried.len();
            for (local, imported) in aliases {
                if names.anywhere.contains(imported) {
                    names.anywhere.insert(local.clone());
                }
                if names.last.contains(imported) {
                    names.last.insert(local.clone());
                }
                if names.carried.contains(imported) {
                    names.carried.insert(local.clone());
                }
            }
            if names.anywhere.len() + names.last.len() + names.carried.len() == before {
                return names;
            }
        }
    }

    fn can_move(&self, reference_name: &str) -> bool {
        name_segments(reference_name).any(|segment| self.anywhere.contains(segment))
            || name_segments(reference_name)
                .last()
                .is_some_and(|segment| self.last.contains(segment))
    }

    fn all(&self) -> impl Iterator<Item = &String> {
        self.anywhere.iter().chain(&self.last)
    }
}

/// [`SymbolsByNameV1`] over a successor: the parent's resolution index with
/// the edited files' rows replaced by their successors' symbols. Each page is
/// read once, the first time a name on it is looked up.
pub(super) struct SparseSymbolsByNameV1<'r> {
    index: &'r ResolutionIndexReaderV1<'r>,
    index_of_path: &'r HashMap<&'r str, usize>,
    edited: HashMap<usize, Vec<NamedSymbolV1>>,
    edited_paths: HashSet<&'r str>,
    pages: Vec<OnceLock<HashMap<String, Vec<NamedSymbolV1>>>>,
    failure: &'r DecodeFailureV1,
}

impl<'r> SparseSymbolsByNameV1<'r> {
    pub(super) fn new(
        index: &'r ResolutionIndexReaderV1<'r>,
        index_of_path: &'r HashMap<&'r str, usize>,
        edited: impl Iterator<Item = (usize, &'r str, &'r Arc<FileGenerationArtifactsV1>)>,
        failure: &'r DecodeFailureV1,
    ) -> Self {
        let mut by_page: HashMap<usize, Vec<NamedSymbolV1>> = HashMap::new();
        let mut edited_paths = HashSet::new();
        for (file_index, path, file) in edited {
            edited_paths.insert(path);
            for symbol in &file.artifacts.symbols {
                by_page
                    .entry(index.page_of(&symbol.simple_name))
                    .or_default()
                    .push((file_index, Arc::clone(symbol)));
            }
        }
        Self {
            pages: (0..index.pages()).map(|_| OnceLock::new()).collect(),
            index,
            index_of_path,
            edited: by_page,
            edited_paths,
            failure,
        }
    }

    fn load_page(&self, page: usize) -> HashMap<String, Vec<NamedSymbolV1>> {
        let mut by_name: HashMap<String, Vec<NamedSymbolV1>> = HashMap::new();
        match self.index.definition_page(page) {
            Ok(rows) => {
                for (name, files) in rows {
                    for (path, symbols) in files {
                        if self.edited_paths.contains(path.as_str()) {
                            continue;
                        }
                        let Some(file_index) = self.index_of_path.get(path.as_str()) else {
                            self.failure.record(contract(
                                "sealed resolution index names a file outside its successor",
                            ));
                            continue;
                        };
                        by_name.entry(name.clone()).or_default().extend(
                            symbols
                                .into_iter()
                                .map(|symbol| (*file_index, Arc::new(symbol))),
                        );
                    }
                }
            }
            Err(error) => self.failure.record(error),
        }
        for (file_index, symbol) in self.edited.get(&page).into_iter().flatten() {
            by_name
                .entry(symbol.simple_name.clone())
                .or_default()
                .push((*file_index, Arc::clone(symbol)));
        }
        for symbols in by_name.values_mut() {
            symbols.sort_by(|left, right| {
                (left.0, &left.1.occurrence).cmp(&(right.0, &right.1.occurrence))
            });
        }
        by_name
    }

    /// Every symbol a lookup has read, by occurrence.
    fn read_symbols(&self) -> impl Iterator<Item = &NamedSymbolV1> {
        self.pages
            .iter()
            .filter_map(OnceLock::get)
            .flat_map(HashMap::values)
            .flatten()
    }
}

impl SymbolsByNameV1 for SparseSymbolsByNameV1<'_> {
    fn get(&self, name: &str) -> Option<&[NamedSymbolV1]> {
        let page = self.index.page_of(name);
        self.pages[page]
            .get_or_init(|| self.load_page(page))
            .get(name)
            .map(Vec::as_slice)
    }
}

/// One successor file whose cross-file evidence the edit re-decided.
#[derive(Default)]
pub(super) struct ResolvedFileV1 {
    /// Its cross-file edges in canonical edge order, each with its target's
    /// logical path and symbol identity.
    pub(super) edges: Vec<(CanonicalRelationEdgeV1, String, SymbolIdentityDigest)>,
    /// Its call limitations in sorted order.
    pub(super) unresolved_calls: Vec<CodeIndexUnresolvedReferenceV1>,
}

/// The edit's resolution: every successor file whose evidence it re-decided.
pub(super) struct SparseResolutionV1 {
    pub(super) files: BTreeMap<usize, ResolvedFileV1>,
    pub(super) ambiguous_before: u64,
    pub(super) ambiguous_after: u64,
}

/// Re-decide the sites the edited files can move. `files` is the successor's
/// file set, `edited` each edited file's index there with its parent and
/// successor artifacts, and `occurrence_of_path` the successor file each
/// logical path names.
#[tracing::instrument(name = "code_index.sparse.resolve", level = "trace", skip_all)]
#[allow(clippy::too_many_arguments)]
pub(super) fn resolve_edit(
    parent: &SealedParentGenerationV1,
    index: &ResolutionIndexReaderV1<'_>,
    files: &[SparseFileV1<'_>],
    by_name: &SparseSymbolsByNameV1<'_>,
    before_files: &[SparseFileV1<'_>],
    before_by_name: &SparseSymbolsByNameV1<'_>,
    edited: &[(
        usize,
        Arc<FileGenerationArtifactsV1>,
        Arc<FileGenerationArtifactsV1>,
    )],
    index_of_path: &HashMap<&str, usize>,
    occurrence_of_path: &HashMap<&str, &FileOccurrenceId>,
    parent_key_of_path: &HashMap<&str, u32>,
) -> Result<SparseResolutionV1, CodeIndexProductionErrorV1> {
    let names = ChangedNamesV1::new(edited, &index.import_aliases()?);
    let edited_indices = edited
        .iter()
        .map(|(index, _, _)| *index)
        .collect::<BTreeSet<_>>();
    let mut referencing = BTreeSet::new();
    let mut reached = BTreeSet::new();
    let mut pages = BTreeMap::new();
    for (name, moves) in names
        .all()
        .map(|name| (name, true))
        .chain(names.carried.iter().map(|name| (name, false)))
    {
        let page = index.page_of(name);
        if let btree_map::Entry::Vacant(entry) = pages.entry(page) {
            entry.insert(index.reference_page(page)?);
        }
        for path in pages[&page].get(name.as_str()).into_iter().flatten() {
            let file_index = index_of_path.get(path.as_str()).ok_or_else(|| {
                contract("sealed resolution index names a file outside its successor")
            })?;
            if edited_indices.contains(file_index) {
                continue;
            }
            if moves {
                referencing.insert(*file_index);
            } else {
                reached.insert(*file_index);
            }
        }
    }
    drop(pages);
    let candidates = edited_indices
        .iter()
        .chain(&referencing)
        .copied()
        .collect::<BTreeSet<_>>();
    // Each referencing file decodes independently, so they decode across the
    // indexing pool before the selection reads them in order.
    collect_bounded_ordered(
        &candidates.iter().copied().collect::<Vec<_>>(),
        |&file_index| {
            files[file_index].artifacts();
            Ok::<_, CodeIndexProductionErrorV1>(())
        },
    )?;
    let mut selection = Vec::new();
    for file_index in candidates {
        let references = &files[file_index].as_ref().artifacts.unresolved_references;
        let picks = if edited_indices.contains(&file_index) {
            (0..references.len()).collect::<Vec<_>>()
        } else {
            let sites = references
                .iter()
                .filter(|reference| names.can_move(&reference.reference_name))
                .map(site)
                .collect::<HashSet<_>>();
            (0..references.len())
                .filter(|&pick| sites.contains(&site(&references[pick])))
                .collect()
        };
        if !picks.is_empty() {
            selection.push((file_index, picks));
        }
    }
    // A file the selection leaves out keeps every decision, including a
    // referencing file none of whose references the edit can move. It
    // re-seals only when one of its sealed edges lands in an edited file.
    let selected = selection
        .iter()
        .map(|(file_index, _)| *file_index)
        .collect::<BTreeSet<_>>();
    let edited_paths = edited_indices
        .iter()
        .map(|&file_index| files[file_index].logical_path())
        .collect::<HashSet<_>>();
    let dependents = collect_bounded_ordered(
        &reached
            .union(&referencing)
            .filter(|file_index| !selected.contains(file_index))
            .copied()
            .collect::<Vec<_>>(),
        |&file_index| {
            let parent_key = parent_key_of_path
                .get(files[file_index].logical_path())
                .ok_or_else(|| contract("a carried file has no parent segment"))?;
            let repoints = parent
                .file_evidence(*parent_key)?
                .is_some_and(|evidence| evidence.targets_any(&edited_paths));
            if repoints {
                files[file_index].artifacts();
            }
            Ok::<_, CodeIndexProductionErrorV1>(repoints.then_some(file_index))
        },
    )?
    .into_iter()
    .flatten()
    .collect::<BTreeSet<_>>();
    let moved = selected_references(files, Some(&selection))
        .map(|(_, reference)| site(reference))
        .chain(
            edited
                .iter()
                .flat_map(|(_, before, _)| before.artifacts.unresolved_references.iter().map(site)),
        )
        .collect::<HashSet<_>>();
    let ambiguous_before = if parent.statistics().ambiguous_name_drops.is_some() {
        let mut before_selection = selection
            .iter()
            .filter(|(file_index, _)| !edited_indices.contains(file_index))
            .cloned()
            .collect::<Vec<_>>();
        before_selection.extend(edited.iter().map(|(file_index, before, _)| {
            (
                *file_index,
                (0..before.artifacts.unresolved_references.len()).collect(),
            )
        }));
        before_selection.sort_by_key(|(file_index, _)| *file_index);
        resolve_selected_cross_file_references(before_files, before_by_name, &before_selection)?
            .ambiguous_name_drops
    } else {
        0
    };
    let resolved = resolve_selected_cross_file_references(files, by_name, &selection)?;
    let ambiguous_after = resolved.ambiguous_name_drops;

    // A resolved edge's target is a symbol some lookup read: a page row, or a
    // symbol of a file resolution decoded.
    let mut targets = HashMap::<&SymbolOccurrenceId, (usize, &SymbolIdentityDigest)>::new();
    for (file_index, symbol) in by_name.read_symbols() {
        targets.insert(&symbol.occurrence, (*file_index, &symbol.identity));
    }
    for (file_index, file) in files.iter().enumerate() {
        if let Some(file) = file.loaded() {
            for symbol in &file.artifacts.symbols {
                targets.insert(&symbol.occurrence, (file_index, &symbol.identity));
            }
        }
    }
    let owner_of = selection
        .iter()
        .flat_map(|(file_index, _)| {
            files[*file_index]
                .as_ref()
                .artifacts
                .symbols
                .iter()
                .map(move |symbol| (&symbol.occurrence, *file_index))
        })
        .collect::<HashMap<_, _>>();
    let mut result = BTreeMap::<usize, ResolvedFileV1>::new();
    for edge in resolved.edges {
        let owner = owner_of
            .get(&edge.from_occurrence)
            .ok_or_else(|| contract("a re-resolved edge leaves a file the edit did not select"))?;
        let (target_index, identity) = targets
            .get(&edge.to_occurrence)
            .ok_or_else(|| contract("a re-resolved edge targets a symbol no lookup read"))?;
        let path = files[*target_index].logical_path().to_owned();
        let identity = (*identity).clone();
        result
            .entry(*owner)
            .or_default()
            .edges
            .push((edge, path, identity));
    }
    for file_index in selection
        .iter()
        .map(|(file_index, _)| file_index)
        .chain(&dependents)
    {
        let entry = result.entry(*file_index).or_default();
        if edited_indices.contains(file_index) {
            continue;
        }
        let file = files[*file_index].as_ref();
        let parent_key = parent_key_of_path
            .get(files[*file_index].logical_path())
            .ok_or_else(|| contract("a carried file has no parent segment"))?;
        let Some(evidence) = parent.file_evidence(*parent_key)? else {
            continue;
        };
        for sealed in evidence.sealed_edges(file)? {
            if moved.contains(&(&sealed.from_occurrence, sealed.evidence_span)) {
                continue;
            }
            let target_file = occurrence_of_path
                .get(sealed.target_path.as_str())
                .ok_or_else(|| {
                    contract("sealed cross-file edge targets a file outside its successor")
                })?;
            let to_occurrence =
                crate::chunks::symbol_occurrence_id(target_file, &sealed.target_identity)
                    .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
            entry.edges.push((
                CanonicalRelationEdgeV1 {
                    from_occurrence: sealed.from_occurrence,
                    to_occurrence,
                    kind: sealed.kind,
                    authority: tracedecay_domain::EdgeAuthorityV1::NameResolved,
                    evidence_span: sealed.evidence_span,
                },
                sealed.target_path,
                sealed.target_identity,
            ));
        }
        for call in evidence.unresolved_calls(file)? {
            if !moved.contains(&site(call)) {
                entry.unresolved_calls.push(call.clone());
            }
        }
    }
    for entry in result.values_mut() {
        entry
            .edges
            .sort_by(|left, right| edge_order(&left.0, &right.0));
        entry.edges.dedup_by(|left, right| left.0 == right.0);
    }
    // A site's limitations depend only on its own references and the edges
    // bound at it, and the selection carries every reference of each site.
    let references = selected_references(files, Some(&selection))
        .map(|(file_index, reference)| (files[file_index].logical_path(), reference))
        .collect::<Vec<_>>();
    let sites = references
        .iter()
        .map(|(_, reference)| site(reference))
        .collect::<HashSet<_>>();
    let at_sites = |edge: &&CanonicalRelationEdgeV1| {
        sites.contains(&(&edge.from_occurrence, edge.evidence_span))
    };
    let file_edges = selection
        .iter()
        .flat_map(|(file_index, _)| files[*file_index].as_ref().artifacts.edges.iter())
        .filter(at_sites);
    let cross_file = result
        .values()
        .flat_map(|entry| entry.edges.iter().map(|(edge, _, _)| edge))
        .filter(at_sites)
        .cloned()
        .collect::<Vec<_>>();
    let rederived = unresolved_call_limitations(
        &references,
        file_edges.chain(cross_file.iter()),
        unresolved_import_calls(files, by_name, Some(&selection)),
        &|| Ok(()),
    )
    .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
    for call in rederived {
        let owner = owner_of
            .get(&call.from_occurrence)
            .ok_or_else(|| contract("a re-derived call limitation leaves the selection"))?;
        result
            .entry(*owner)
            .or_default()
            .unresolved_calls
            .push(call);
    }
    for entry in result.values_mut() {
        entry.unresolved_calls.sort();
        entry.unresolved_calls.dedup();
    }
    Ok(SparseResolutionV1 {
        files: result,
        ambiguous_before,
        ambiguous_after,
    })
}
