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
use std::sync::{Arc, Mutex};
use tracedecay_runtime_core::resident_memory::ResidentMemoryOperationV1;

use tracedecay_code_extraction::{ImportModuleKindV1, ImportNamespaceV1, ImportReexportScopeV1};
use tracedecay_domain::{
    CanonicalRelationEdgeV1, FileOccurrenceId, NodeKind, SourceSpan, SymbolIdentityDigest,
    SymbolOccurrenceId,
};

use crate::chunks::{CodeIndexImportEvidenceV1, CodeIndexUnresolvedReferenceV1};
use crate::graph_projection::unresolved_call_limitations;
use crate::lineage::LineageSymbolRecordV1;

use super::helpers::{
    CrossFileResolutionV1, ResolutionModulesV1, edge_order, resolve_selected_with_modules,
    selected_references, unresolved_import_calls_with_modules,
};
use super::resolution_index::{DefinitionPageV1, ResolutionIndexReaderV1, name_segments};
use super::resolution_view::ResolutionMemoryChangeV1;
use super::resolution_view::{
    NamedSymbolV1, ResolutionFileFactsV1, ResolutionFileV1, SymbolsByNameV1,
};
use super::sealed_parent::{SealedParentGenerationV1, SparseFileV1};
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
            // Symbols are stored in occurrence order, and an edit re-keys
            // every occurrence of the file, so same-named symbols can trade
            // places without any of them changing.
            for keys in sides.values_mut().flatten() {
                keys.sort_unstable();
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
/// the edited files' rows replaced by their successors' symbols. A lookup
/// releases its decoded page after taking the matching rows.
pub(super) struct SparseSymbolsByNameV1<'r> {
    index: &'r ResolutionIndexReaderV1<'r>,
    window: &'r SparseDefinitionWindowV1<'r>,
    index_of_path: &'r HashMap<&'r str, usize>,
    edited: HashMap<usize, Vec<NamedSymbolV1>>,
    edited_paths: HashSet<&'r str>,
    /// Only the identities needed by emitted edge evidence survive a page
    /// lookup. No decoded definition page or symbol record is cached here.
    targets: std::sync::Mutex<SparseTargetEvidenceV1>,
}

/// Shared only by the before/after lookup overlays for one selected-file
/// window. Canonical page payloads never outlive that window.
pub(super) struct SparseDefinitionWindowV1<'r> {
    index: &'r ResolutionIndexReaderV1<'r>,
    operation: ResidentMemoryOperationV1,
    state: Mutex<SparseDefinitionWindowStateV1>,
}

struct SparseDefinitionWindowStateV1 {
    pages: BTreeMap<usize, Arc<DefinitionPageV1>>,
    memory: super::ConstructionMemoryV1,
}

impl<'r> SparseDefinitionWindowV1<'r> {
    pub(super) fn new(
        index: &'r ResolutionIndexReaderV1<'r>,
        construction_operation: &ResidentMemoryOperationV1,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        Ok(Self {
            index,
            operation: construction_operation.clone(),
            state: Mutex::new(SparseDefinitionWindowStateV1 {
                pages: BTreeMap::new(),
                memory: super::ConstructionMemoryV1::new(construction_operation.clone())?,
            }),
        })
    }

    fn page(&self, page: usize) -> Result<Arc<DefinitionPageV1>, CodeIndexProductionErrorV1> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| contract("resolution page window lock is poisoned"))?;
        if let Some(rows) = state.pages.get(&page) {
            return Ok(Arc::clone(rows));
        }
        let held = state.memory.charged_bytes;
        state.memory.resize(
            held.checked_add(self.index.definition_page_read_bytes(page)?)
                .ok_or_else(|| contract("resolution page window charge exceeds u64"))?,
        )?;
        let rows = self.index.definition_page(page)?;
        let mut bytes = super::resident_bytes::btree_bytes(
            rows.len(),
            std::mem::size_of::<(String, BTreeMap<String, Vec<Arc<LineageSymbolRecordV1>>>)>(),
        );
        for (name, files) in &rows {
            bytes = bytes.saturating_add(name.capacity()).saturating_add(
                super::resident_bytes::btree_bytes(
                    files.len(),
                    std::mem::size_of::<(String, Vec<Arc<LineageSymbolRecordV1>>)>(),
                ),
            );
            for (path, symbols) in files {
                bytes = bytes.saturating_add(path.capacity()).saturating_add(
                    super::resident_bytes::vec_bytes(symbols, symbols.capacity(), |symbol| {
                        super::resident_bytes::symbol_bytes(symbol)
                    }),
                );
            }
        }
        let before = super::resident_bytes::btree_bytes(
            state.pages.len(),
            std::mem::size_of::<(usize, Arc<DefinitionPageV1>)>(),
        );
        let after = super::resident_bytes::btree_bytes(
            state.pages.len().saturating_add(1),
            std::mem::size_of::<(usize, Arc<DefinitionPageV1>)>(),
        );
        let bytes = bytes
            .saturating_add(after.saturating_sub(before))
            .saturating_add(super::resident_bytes::ARC_HEADER_BYTES)
            .saturating_add(std::mem::size_of::<DefinitionPageV1>());
        state.memory.resize(
            held.checked_add(
                u64::try_from(bytes)
                    .map_err(|_| contract("resolution page window charge exceeds u64"))?,
            )
            .ok_or_else(|| contract("resolution page window charge exceeds u64"))?,
        )?;
        let rows = Arc::new(rows);
        state.pages.insert(page, Arc::clone(&rows));
        Ok(rows)
    }

    pub(super) fn release(&self) -> Result<(), CodeIndexProductionErrorV1> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| contract("resolution page window lock is poisoned"))?;
        state.pages.clear();
        state.memory.resize(0)
    }
}

pub(super) struct SparseTargetEvidenceV1 {
    rows: BTreeMap<SymbolOccurrenceId, (usize, SymbolIdentityDigest)>,
    memory: super::ConstructionMemoryV1,
}

impl<'r> SparseSymbolsByNameV1<'r> {
    pub(super) fn new(
        index: &'r ResolutionIndexReaderV1<'r>,
        index_of_path: &'r HashMap<&'r str, usize>,
        window: &'r SparseDefinitionWindowV1<'r>,
        edited: impl Iterator<Item = (usize, &'r str, &'r Arc<FileGenerationArtifactsV1>)>,
        construction_operation: &ResidentMemoryOperationV1,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let mut by_page = HashMap::<usize, Vec<NamedSymbolV1>>::new();
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
        let bytes = super::resident_bytes::hash_table_bytes::<(usize, Vec<NamedSymbolV1>)>(
            by_page.capacity(),
        )
        .saturating_add(
            by_page
                .values()
                .map(|rows| {
                    rows.capacity()
                        .saturating_mul(std::mem::size_of::<NamedSymbolV1>())
                })
                .sum::<usize>(),
        )
        .saturating_add(super::resident_bytes::hash_table_bytes::<&str>(
            edited_paths.capacity(),
        ));
        let mut memory = super::ConstructionMemoryV1::new(construction_operation.clone())?;
        memory.resize(
            u64::try_from(bytes).map_err(|_| contract("resolution name index exceeds u64"))?,
        )?;
        Ok(Self {
            index,
            window,
            index_of_path,
            edited: by_page,
            edited_paths,
            targets: std::sync::Mutex::new(SparseTargetEvidenceV1 {
                rows: BTreeMap::new(),
                memory,
            }),
        })
    }

    pub(super) fn take_targets(
        &self,
    ) -> Result<SparseTargetEvidenceV1, CodeIndexProductionErrorV1> {
        let mut targets = self
            .targets
            .lock()
            .map_err(|_| contract("resolution target evidence lock is poisoned"))?;
        let empty = SparseTargetEvidenceV1 {
            rows: BTreeMap::new(),
            memory: super::ConstructionMemoryV1::new(targets.memory.operation.clone())?,
        };
        Ok(std::mem::replace(&mut *targets, empty))
    }
}

impl SparseTargetEvidenceV1 {
    fn insert(
        &mut self,
        index: usize,
        symbol: &LineageSymbolRecordV1,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        if self.rows.contains_key(&symbol.occurrence) {
            return Ok(());
        }
        let before = super::resident_bytes::btree_bytes(
            self.rows.len(),
            std::mem::size_of::<(SymbolOccurrenceId, (usize, SymbolIdentityDigest))>(),
        );
        let after = super::resident_bytes::btree_bytes(
            self.rows.len().saturating_add(1),
            std::mem::size_of::<(SymbolOccurrenceId, (usize, SymbolIdentityDigest))>(),
        );
        let additional = after
            .saturating_sub(before)
            .saturating_add(symbol.occurrence.as_str().len())
            .saturating_add(symbol.identity.as_str().len());
        let bytes = self
            .memory
            .charged_bytes
            .checked_add(
                u64::try_from(additional)
                    .map_err(|_| contract("resolution target evidence exceeds u64"))?,
            )
            .ok_or_else(|| contract("resolution target evidence exceeds u64"))?;
        self.memory.resize(bytes)?;
        self.rows
            .insert(symbol.occurrence.clone(), (index, symbol.identity.clone()));
        Ok(())
    }
    fn get(&self, occurrence: &SymbolOccurrenceId) -> Option<&(usize, SymbolIdentityDigest)> {
        self.rows.get(occurrence)
    }
}

impl SymbolsByNameV1 for SparseSymbolsByNameV1<'_> {
    fn get(&self, name: &str) -> Result<Option<Vec<NamedSymbolV1>>, CodeIndexProductionErrorV1> {
        let page = self.index.page_of(name);
        let rows = self.window.page(page)?;
        let mut result = Vec::new();
        if let Some(files) = rows.get(name) {
            for (path, symbols) in files {
                if self.edited_paths.contains(path.as_str()) {
                    continue;
                }
                let file_index = self.index_of_path.get(path.as_str()).ok_or_else(|| {
                    contract("sealed resolution index names a file outside its successor")
                })?;
                result.extend(
                    symbols
                        .iter()
                        .map(|symbol| (*file_index, Arc::clone(symbol))),
                );
            }
        }
        result.extend(
            self.edited
                .get(&page)
                .into_iter()
                .flatten()
                .filter(|(_, symbol)| symbol.simple_name == name)
                .cloned(),
        );
        result.sort_by(|left, right| {
            (left.0, &left.1.occurrence).cmp(&(right.0, &right.1.occurrence))
        });
        let mut targets = self
            .targets
            .lock()
            .map_err(|_| contract("resolution target evidence lock is poisoned"))?;
        for (index, symbol) in &result {
            targets.insert(*index, symbol)?;
        }
        Ok((!result.is_empty()).then_some(result))
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

fn retain_resolution_growth(
    memory: &mut super::ConstructionMemoryV1,
    change: ResolutionMemoryChangeV1,
) -> Result<(), CodeIndexProductionErrorV1> {
    memory.resize(change.apply_to_u64(memory.charged_bytes)?)
}

fn resolved_file_entry<'a>(
    files: &'a mut BTreeMap<usize, ResolvedFileV1>,
    index: usize,
    charge: &mut dyn FnMut(ResolutionMemoryChangeV1) -> Result<(), CodeIndexProductionErrorV1>,
) -> Result<&'a mut ResolvedFileV1, CodeIndexProductionErrorV1> {
    if !files.contains_key(&index) {
        let entry_bytes = std::mem::size_of::<(usize, ResolvedFileV1)>();
        let before = if files.is_empty() {
            0
        } else {
            super::resident_bytes::btree_bytes(files.len(), entry_bytes)
        };
        charge(ResolutionMemoryChangeV1::Retain(
            super::resident_bytes::btree_bytes(files.len().saturating_add(1), entry_bytes)
                .saturating_sub(before),
        ))?;
    }
    Ok(files.entry(index).or_default())
}

impl ResolvedFileV1 {
    fn retained_bytes(&self) -> usize {
        super::resident_bytes::vec_bytes(
            &self.edges,
            self.edges.capacity(),
            |(edge, path, identity)| {
                super::resident_bytes::edge_heap_bytes(edge)
                    .saturating_add(path.len())
                    .saturating_add(identity.as_str().len())
            },
        )
        .saturating_add(super::resident_bytes::vec_bytes(
            &self.unresolved_calls,
            self.unresolved_calls.capacity(),
            super::resident_bytes::unresolved_heap_bytes,
        ))
    }
}

/// The edit's resolution: every successor file whose evidence it re-decided.
pub(super) struct SparseResolutionV1 {
    pub(super) files: BTreeMap<usize, ResolvedFileV1>,
    pub(super) ambiguous_before: u64,
    pub(super) ambiguous_after: u64,
    pub(super) output_memory: super::ConstructionMemoryV1,
}

/// Consume each sorted selection once, borrowing the current file window.
fn selection_window<'a>(
    selection: &'a super::helpers::ReferenceSelectionV1,
    cursor: &mut usize,
    last: usize,
) -> &'a super::helpers::ReferenceSelectionV1 {
    let start = *cursor;
    while *cursor < selection.len() && selection[*cursor].0 <= last {
        *cursor += 1;
    }
    &selection[start..*cursor]
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
    window: &SparseDefinitionWindowV1<'_>,
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
            files[file_index].binding()?;
            Ok::<_, CodeIndexProductionErrorV1>(())
        },
    )?;
    let mut selection = Vec::new();
    for file_index in candidates {
        let references = files[file_index].binding()?.unresolved_references();
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
                files[file_index].binding()?;
            }
            Ok::<_, CodeIndexProductionErrorV1>(repoints.then_some(file_index))
        },
    )?
    .into_iter()
    .flatten()
    .collect::<BTreeSet<_>>();
    let moved = selected_references(files, Some(&selection))?
        .into_iter()
        .map(|(_, reference)| site(reference))
        .chain(
            edited
                .iter()
                .flat_map(|(_, before, _)| before.artifacts.unresolved_references.iter().map(site)),
        )
        .collect::<HashSet<_>>();
    let mut before_selection = Vec::new();
    if parent.statistics().ambiguous_name_drops.is_some() {
        before_selection.extend(
            selection
                .iter()
                .filter(|(index, _)| !edited_indices.contains(index))
                .cloned(),
        );
        before_selection.extend(edited.iter().map(|(index, before, _)| {
            (
                *index,
                (0..before.artifacts.unresolved_references.len()).collect(),
            )
        }));
        before_selection.sort_by_key(|(index, _)| *index);
    }
    let units = selection
        .iter()
        .chain(&before_selection)
        .map(|(index, _)| *index)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut before_modules = ResolutionModulesV1::new(before_files)?;
    let mut modules = ResolutionModulesV1::new(files)?;
    let mut output_memory = super::ConstructionMemoryV1::new(window.operation.clone())?;
    let mut resolved = CrossFileResolutionV1 {
        edges: Vec::new(),
        gaps: Vec::new(),
        ambiguous_name_drops: 0,
    };
    let mut import_unresolved = Vec::new();
    let mut ambiguous_before = 0_u64;
    let mut before_cursor = 0;
    let mut after_cursor = 0;
    for units in units.chunks(crate::parallelism::indexing_workers().max(1)) {
        let last = units[units.len() - 1];
        let before = selection_window(&before_selection, &mut before_cursor, last);
        let after = selection_window(&selection, &mut after_cursor, last);
        let base = output_memory.charged_bytes;
        if !before.is_empty() {
            ambiguous_before = ambiguous_before.saturating_add(
                resolve_selected_with_modules(
                    before_files,
                    before_by_name,
                    &before_modules,
                    before,
                    &mut |change| retain_resolution_growth(&mut output_memory, change),
                )?
                .ambiguous_name_drops,
            );
            for (index, _) in before {
                before_modules.release_file_memo(*index);
            }
            output_memory.resize(base)?;
        }
        let mut charge = |change| retain_resolution_growth(&mut output_memory, change);
        let result = resolve_selected_with_modules(files, by_name, &modules, after, &mut charge)?;
        super::resolution_view::reserve_resolution_vec(
            &mut resolved.edges,
            result.edges.len(),
            &mut charge,
        )?;
        super::resolution_view::reserve_resolution_vec(
            &mut resolved.gaps,
            result.gaps.len(),
            &mut charge,
        )?;
        resolved.edges.extend(result.edges);
        resolved.gaps.extend(result.gaps);
        resolved.ambiguous_name_drops = resolved
            .ambiguous_name_drops
            .saturating_add(result.ambiguous_name_drops);
        let imports = unresolved_import_calls_with_modules(
            files,
            by_name,
            &modules,
            Some(after),
            &mut charge,
        )?;
        super::resolution_view::reserve_resolution_vec(
            &mut import_unresolved,
            imports.len(),
            &mut charge,
        )?;
        import_unresolved.extend(imports);
        for (index, _) in after {
            modules.release_file_memo(*index);
        }
        window.release()?;
        let held = super::resident_bytes::vec_bytes(
            &resolved.edges,
            resolved.edges.capacity(),
            super::resident_bytes::edge_heap_bytes,
        )
        .saturating_add(super::resident_bytes::vec_bytes(
            &resolved.gaps,
            resolved.gaps.capacity(),
            super::resident_bytes::unresolved_heap_bytes,
        ))
        .saturating_add(super::resident_bytes::vec_bytes(
            &import_unresolved,
            import_unresolved.capacity(),
            super::resident_bytes::unresolved_heap_bytes,
        ));
        output_memory.resize(
            u64::try_from(held).map_err(|_| contract("sparse resolution charge exceeds u64"))?,
        )?;
    }
    resolved.edges.sort_unstable_by(edge_order);
    resolved.edges.dedup();
    let ambiguous_after = resolved.ambiguous_name_drops;

    // A resolved edge's target is a symbol some lookup read: a page row, or a
    // symbol of a file resolution decoded.
    let mut targets = by_name.take_targets()?;
    for (file_index, file) in files.iter().enumerate() {
        if let Some(file) = file.loaded() {
            for symbol in file.symbols() {
                targets.insert(file_index, symbol)?;
            }
        }
    }
    let mut charge = |bytes| retain_resolution_growth(&mut output_memory, bytes);
    let mut owner_of = HashMap::new();
    for (file_index, _) in &selection {
        for symbol in files[*file_index].binding()?.symbols() {
            if !owner_of.contains_key(&symbol.occurrence) {
                super::resolution_view::reserve_resolution_map(&mut owner_of, &mut charge)?;
            }
            owner_of.insert(&symbol.occurrence, *file_index);
        }
    }
    let mut result = BTreeMap::<usize, ResolvedFileV1>::new();
    for edge in resolved.edges {
        let owner = owner_of
            .get(&edge.from_occurrence)
            .ok_or_else(|| contract("a re-resolved edge leaves a file the edit did not select"))?;
        let (target_index, identity) = targets
            .get(&edge.to_occurrence)
            .ok_or_else(|| contract("a re-resolved edge targets a symbol no lookup read"))?;
        let path = files[*target_index].logical_path();
        let entry = resolved_file_entry(&mut result, *owner, &mut charge)?;
        super::resolution_view::reserve_resolution_vec(&mut entry.edges, 1, &mut charge)?;
        charge(ResolutionMemoryChangeV1::Retain(
            path.len().saturating_add(identity.as_str().len()),
        ))?;
        entry.edges.push((edge, path.to_owned(), identity.clone()));
    }
    for file_index in selection
        .iter()
        .map(|(file_index, _)| file_index)
        .chain(&dependents)
    {
        let entry = resolved_file_entry(&mut result, *file_index, &mut charge)?;
        if edited_indices.contains(file_index) {
            continue;
        }
        let file = files[*file_index].binding()?;
        let parent_key = parent_key_of_path
            .get(files[*file_index].logical_path())
            .ok_or_else(|| contract("a carried file has no parent segment"))?;
        let Some(evidence) = parent.file_evidence(*parent_key)? else {
            continue;
        };
        evidence.for_each_sealed_edge(
            file,
            &window.operation,
            &mut |from, kind, evidence_span, target_path, target_identity| {
                if moved.contains(&(from, evidence_span)) {
                    return Ok(());
                }
                let target_file = occurrence_of_path.get(target_path).ok_or_else(|| {
                    contract("sealed cross-file edge targets a file outside its successor")
                })?;
                super::resolution_view::reserve_resolution_vec(&mut entry.edges, 1, &mut charge)?;
                // The source and rebound target share the canonical symbol
                // occurrence digest domain, hence the same wire length.
                charge(ResolutionMemoryChangeV1::Retain(
                    super::resident_bytes::edge_endpoint_heap_bytes(from, from)
                        .saturating_add(target_path.len())
                        .saturating_add(target_identity.as_str().len()),
                ))?;
                let to_occurrence =
                    crate::chunks::symbol_occurrence_id(target_file, target_identity)
                        .map_err(|error| contract(&error.to_string()))?;
                entry.edges.push((
                    CanonicalRelationEdgeV1 {
                        from_occurrence: from.clone(),
                        to_occurrence,
                        kind,
                        authority: tracedecay_domain::EdgeAuthorityV1::NameResolved,
                        evidence_span,
                    },
                    target_path.to_owned(),
                    target_identity.clone(),
                ));
                Ok(())
            },
        )?;
        evidence.for_each_unresolved_call(file, &window.operation, &mut |call| {
            if !moved.contains(&site(call)) {
                super::resolution_view::reserve_resolution_vec(
                    &mut entry.unresolved_calls,
                    1,
                    &mut charge,
                )?;
                charge(ResolutionMemoryChangeV1::Retain(
                    super::resident_bytes::unresolved_heap_bytes(call),
                ))?;
                entry.unresolved_calls.push(call.clone());
            }
            Ok(())
        })?;
    }
    for entry in result.values_mut() {
        entry
            .edges
            .sort_unstable_by(|left, right| edge_order(&left.0, &right.0));
        entry.edges.dedup_by(|left, right| left.0 == right.0);
    }
    // A site's limitations depend only on its own references and the edges
    // bound at it, and the selection carries every reference of each site.
    let reference_count = selection
        .iter()
        .map(|(_, picks)| picks.len())
        .sum::<usize>();
    charge(ResolutionMemoryChangeV1::Retain(
        reference_count.saturating_mul(
            std::mem::size_of::<(usize, &CodeIndexUnresolvedReferenceV1)>().saturating_mul(2)
                + std::mem::size_of::<(&str, &CodeIndexUnresolvedReferenceV1)>(),
        ),
    ))?;
    let references = selected_references(files, Some(&selection))?
        .into_iter()
        .map(|(file_index, reference)| (files[file_index].logical_path(), reference))
        .collect::<Vec<_>>();
    let mut sites = HashSet::new();
    for (_, reference) in &references {
        if !sites.contains(&site(reference)) {
            super::resolution_view::reserve_resolution_set(&mut sites, &mut charge)?;
            sites.insert(site(reference));
        }
    }
    let at_sites = |edge: &&CanonicalRelationEdgeV1| {
        sites.contains(&(&edge.from_occurrence, edge.evidence_span))
    };
    let mut file_edges = Vec::new();
    for (file_index, _) in &selection {
        for edge in files[*file_index]
            .binding()?
            .edges()
            .iter()
            .filter(at_sites)
        {
            super::resolution_view::reserve_resolution_vec(&mut file_edges, 1, &mut charge)?;
            file_edges.push(edge);
        }
    }
    let cross_file = || {
        result
            .values()
            .flat_map(|entry| entry.edges.iter().map(|(edge, _, _)| edge))
            .filter(at_sites)
    };
    let edge_count = file_edges.len().saturating_add(cross_file().count());
    // Admit the canonical projector's site maps and worst-case cloned call
    // rows before it builds them. Import rows move without cloning their heaps.
    charge(ResolutionMemoryChangeV1::Retain(
        super::resident_bytes::btree_bytes(
            references.len(),
            std::mem::size_of::<((&SymbolOccurrenceId, SourceSpan), Option<&str>)>(),
        )
        .saturating_add(super::resident_bytes::btree_bytes(
            edge_count,
            std::mem::size_of::<((&SymbolOccurrenceId, SourceSpan), &str)>(),
        ))
        .saturating_add(super::resident_bytes::btree_bytes(
            edge_count,
            std::mem::size_of::<(&SymbolOccurrenceId, SourceSpan)>(),
        ))
        .saturating_add(
            references
                .len()
                .saturating_add(import_unresolved.len())
                .saturating_mul(2)
                .max(4)
                .saturating_mul(std::mem::size_of::<CodeIndexUnresolvedReferenceV1>()),
        )
        .saturating_add(
            references
                .iter()
                .map(|(_, reference)| super::resident_bytes::unresolved_heap_bytes(reference))
                .sum::<usize>(),
        ),
    ))?;
    let rederived = unresolved_call_limitations(
        &references,
        file_edges.iter().copied().chain(cross_file()),
        import_unresolved,
        &|| Ok(()),
    )
    .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
    for call in rederived.into_iter().chain(resolved.gaps) {
        let owner = owner_of
            .get(&call.from_occurrence)
            .ok_or_else(|| contract("a re-derived call limitation leaves the selection"))?;
        let entry = resolved_file_entry(&mut result, *owner, &mut charge)?;
        super::resolution_view::reserve_resolution_vec(
            &mut entry.unresolved_calls,
            1,
            &mut charge,
        )?;
        entry.unresolved_calls.push(call);
    }
    for entry in result.values_mut() {
        entry.unresolved_calls.sort_unstable();
        entry.unresolved_calls.dedup();
    }
    drop(charge);
    drop(file_edges);
    drop(sites);
    drop(references);
    drop(owner_of);
    drop(targets);
    let held = if result.is_empty() {
        0
    } else {
        super::resident_bytes::btree_bytes(
            result.len(),
            std::mem::size_of::<(usize, ResolvedFileV1)>(),
        )
        .saturating_add(
            result
                .values()
                .map(ResolvedFileV1::retained_bytes)
                .sum::<usize>(),
        )
    };
    output_memory.resize(
        u64::try_from(held).map_err(|_| contract("sparse resolution charge exceeds u64"))?,
    )?;
    Ok(SparseResolutionV1 {
        files: result,
        ambiguous_before,
        ambiguous_after,
        output_memory,
    })
}

#[cfg(test)]
mod tests {
    use super::selection_window;

    #[test]
    fn sorted_resolution_windows_borrow_each_selected_file_once() {
        let before = vec![(1, vec![2, 4]), (3, vec![0]), (9, vec![1])];
        let after = vec![(0, vec![1]), (3, vec![2]), (8, vec![0, 1])];
        let mut before_cursor = 0;
        let mut after_cursor = 0;
        let mut visited_before = Vec::new();
        let mut visited_after = Vec::new();
        for last in [1, 8, 9] {
            let before_window = selection_window(&before, &mut before_cursor, last);
            let after_window = selection_window(&after, &mut after_cursor, last);
            for (index, picks) in before_window {
                let original = before.iter().find(|(file, _)| file == index).unwrap();
                assert!(std::ptr::eq(picks.as_slice(), original.1.as_slice()));
                visited_before.push(*index);
            }
            for (index, picks) in after_window {
                let original = after.iter().find(|(file, _)| file == index).unwrap();
                assert!(std::ptr::eq(picks.as_slice(), original.1.as_slice()));
                visited_after.push(*index);
            }
        }
        assert_eq!(visited_before, vec![1, 3, 9]);
        assert_eq!(visited_after, vec![0, 3, 8]);
        assert!(selection_window(&before, &mut before_cursor, usize::MAX).is_empty());
        assert!(selection_window(&after, &mut after_cursor, usize::MAX).is_empty());
    }
}
