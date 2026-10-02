//! Public interactive graph results and the generation-pinned lookup catalog.

use std::borrow::Borrow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::mem::size_of;
use std::sync::Arc;

use serde::{Serialize, Serializer};
use tracedecay_domain::process_heap::OwnerHeapV1;
use tracedecay_domain::{
    CanonicalRelationEdgeV1, FileOccurrenceId, RelationEdgeKindV1, SanitizedCodeFileV1,
    SymbolOccurrenceId, UnmodeledImportShapeV1,
};
use tracedecay_graph_db::GraphEntityId;

use super::super::{CodeGraphProjectionError, CodeGraphSymbolBindingV1, symbol_entity_id};
use crate::chunks::{CodeIndexImportEvidenceV1, CodeIndexUnresolvedReferenceV1};
use crate::lineage::LineageSymbolRecordV1;
use crate::production::resident_bytes::{
    ARC_HEADER_BYTES, hash_table_bytes, import_heap_bytes, opt, snapshot_file_heap_bytes,
    symbol_heap_bytes, unresolved_heap_bytes, vec_bytes,
};

/// One symbol as the interactive surface knows it. `metadata` is present for
/// every symbol published from production inputs; in-memory retrieval-only
/// publications truthfully carry `None` because no name/kind metadata was
/// published for them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeGraphSymbolSummaryV1 {
    pub occurrence: SymbolOccurrenceId,
    pub binding: Option<CodeGraphSymbolBindingV1>,
    pub metadata: Option<LineageSymbolRecordV1>,
}

/// A symbol's identity in the graph, as a relation key names it before any
/// read decodes the symbol.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CodeGraphSymbolRefV1(pub(in crate::graph_projection) GraphEntityId);

impl CodeGraphSymbolRefV1 {
    pub fn for_occurrence(
        occurrence: &SymbolOccurrenceId,
    ) -> Result<Self, CodeGraphProjectionError> {
        occurrence
            .validate()
            .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
        symbol_entity_id(occurrence).map(Self)
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl Serialize for CodeGraphSymbolRefV1 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// One relation a key walk reached: the far symbol and the edge's kind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeGraphRelationKeyV1 {
    pub neighbor: CodeGraphSymbolRefV1,
    pub kind: RelationEdgeKindV1,
}

/// Relation keys per seed of one walk step; `truncated` when the step's edge
/// fan-out reached its limit, so relations past it were not read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeGraphRelationKeysV1 {
    pub per_seed: Vec<Vec<CodeGraphRelationKeyV1>>,
    pub truncated: bool,
}

/// One semantic edge incident to a requested seed, with the far endpoint
/// hydrated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeGraphSemanticEdgeV1 {
    pub edge: CanonicalRelationEdgeV1,
    pub neighbor: CodeGraphSymbolSummaryV1,
}

/// One page of the generation's symbols in canonical occurrence order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeGraphSymbolPageV1 {
    pub symbols: Vec<CodeGraphSymbolSummaryV1>,
    pub has_more: bool,
}

/// True per-kind totals of the semantic edges incident to one symbol. Counts
/// are bounded by the symbol's actual degree, never by a truncation budget.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CodeGraphEdgeKindCountsV1 {
    pub outgoing: BTreeMap<RelationEdgeKindV1, u64>,
    pub incoming: BTreeMap<RelationEdgeKindV1, u64>,
}

/// True semantic in/out degree of one symbol occurrence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeGraphSymbolDegreesV1 {
    pub occurrence: SymbolOccurrenceId,
    pub outgoing: u64,
    pub incoming: u64,
}

/// One ranked symbol with its true semantic in/out degree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeGraphRankedSymbolV1 {
    pub summary: CodeGraphSymbolSummaryV1,
    pub outgoing: u64,
    pub incoming: u64,
}

/// A seed set's neighbors, ranked before the cut. `total` counts every
/// distinct neighbor the walk found; `walk_truncated` means a direction
/// stopped at its row limit, so more neighbors may exist.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeGraphRankedNeighborsV1 {
    pub neighbors: Vec<CodeGraphSymbolSummaryV1>,
    pub total: usize,
    pub walk_truncated: bool,
}

/// The most-connected symbols of one generation, ranked over every symbol
/// of the generation; `symbol_count` is that census size.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeGraphDegreeRankingV1 {
    pub ranked: Vec<CodeGraphRankedSymbolV1>,
    pub symbol_count: usize,
}

/// Symbol count of one logical file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeGraphFileSymbolCountV1 {
    pub logical_path: String,
    pub symbols: u64,
}

/// Generation-wide aggregates, derived once while the catalog is built, so
/// reading them never walks the census.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeGraphCensusV1 {
    pub symbols: u64,
    pub semantic_edges: u64,
    pub files: u64,
    pub symbols_by_kind: BTreeMap<String, u64>,
    pub files_by_language: BTreeMap<String, u64>,
    /// Most symbol-dense files first, ties by path.
    pub largest_files: Vec<CodeGraphFileSymbolCountV1>,
}

/// File-level `calls`/`uses` dependencies of one generation, folded once
/// while the catalog is built from every such edge whose endpoints are
/// bound to two different files.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CodeGraphFileDependenciesV1 {
    /// Every file's logical path to the logical paths it depends on.
    pub adjacency: Arc<HashMap<String, HashSet<String>>>,
    /// The `calls`/`uses` edges of the generation the adjacency folds.
    pub dependency_edges: u64,
}

/// One window of a symbol name search. `total` is the exact match count
/// when the scan reached the end of the generation, and `None` when it
/// stopped one match past the window (`has_more`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeGraphSymbolSearchPageV1 {
    pub symbols: Vec<CodeGraphSymbolSummaryV1>,
    pub has_more: bool,
    pub total: Option<u64>,
}

/// One symbol reached by a reverse-reachability (impact) expansion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeGraphImpactedSymbolV1 {
    pub summary: CodeGraphSymbolSummaryV1,
    pub depth: u32,
}

/// Impact expansion result. `complete` is `false` exactly when the
/// `max_symbols` ceiling stopped the expansion before the frontier drained,
/// so a truncated closure can never be mistaken for the full one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeGraphImpactBatchV1 {
    pub impacted: Vec<CodeGraphImpactedSymbolV1>,
    pub complete: bool,
}

/// Path search result. `path: None` with `complete: true` is a definitive
/// no-path verdict within the requested depth; `complete: false` means the
/// depth ceiling stopped the search while unexplored frontier remained.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeGraphPathSearchV1 {
    pub path: Option<Vec<CanonicalRelationEdgeV1>>,
    pub complete: bool,
}

/// Unresolved call sites that can name a queried callee, by kind of gap.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UnresolvedCallerGapsV1 {
    /// A receiver or import call whose exact target the seal could not bind.
    pub exact_target_unavailable: bool,
    /// Calls under `use` shapes the extractor could not model.
    pub unmodeled_imports: BTreeSet<UnmodeledImportShapeV1>,
}

impl UnresolvedCallerGapsV1 {
    pub fn is_empty(&self) -> bool {
        !self.exact_target_unavailable && self.unmodeled_imports.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CatalogSymbol {
    pub(super) binding: Option<CodeGraphSymbolBindingV1>,
    pub(super) metadata: Option<LineageSymbolRecordV1>,
    pub(super) unresolved_calls: Vec<CodeIndexUnresolvedReferenceV1>,
    /// Semantic degree: `CodeEdge.<kind>` rows leaving the symbol and rows
    /// reaching it, the same counts
    /// [`super::CodeGraphInteractiveReader::degrees`] reads from adjacency.
    pub(super) outgoing: u64,
    pub(super) incoming: u64,
}

/// A map frozen from the `BTreeMap` it was built in: its entries sorted by
/// key in one exactly sized allocation. It answers the same lookups and
/// ordered walks without B-tree nodes, so the catalog knows its own bytes.
pub(in crate::graph_projection) struct SortedMap<K, V>(Box<[(K, V)]>);

impl<K: Ord, V> SortedMap<K, V> {
    pub(in crate::graph_projection) fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.0
            .binary_search_by(|(entry, _)| entry.borrow().cmp(key))
            .ok()
            .map(|index| &self.0[index].1)
    }

    pub(in crate::graph_projection) fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.0.iter().map(|(key, value)| (key, value))
    }

    /// Entries whose key sorts after `key`.
    pub(in crate::graph_projection) fn after(&self, key: &K) -> impl Iterator<Item = (&K, &V)> {
        let start = self.0.partition_point(|(entry, _)| entry <= key);
        self.0[start..].iter().map(|(key, value)| (key, value))
    }

    pub(in crate::graph_projection) fn values(&self) -> impl Iterator<Item = &V> {
        self.0.iter().map(|(_, value)| value)
    }

    pub(in crate::graph_projection) fn len(&self) -> usize {
        self.0.len()
    }

    /// The entry slice plus what each key and value owns.
    fn bytes(&self, heap: impl Fn(&K, &V) -> usize) -> usize {
        self.0.iter().fold(
            self.0.len().saturating_mul(size_of::<(K, V)>()),
            |bytes, (key, value)| bytes.saturating_add(heap(key, value)),
        )
    }
}

impl<K, V> From<BTreeMap<K, V>> for SortedMap<K, V> {
    fn from(map: BTreeMap<K, V>) -> Self {
        Self(map.into_iter().collect())
    }
}

/// Symbol ids of one lookup key, frozen to their exact count.
type SymbolIds = Box<[SymbolOccurrenceId]>;

fn freeze_ids<K: Ord>(map: BTreeMap<K, Vec<SymbolOccurrenceId>>) -> SortedMap<K, SymbolIds> {
    SortedMap(
        map.into_iter()
            .map(|(key, ids)| (key, ids.into_boxed_slice()))
            .collect(),
    )
}

/// Generation-pinned catalog of every file, symbol, and import entity in one
/// published graph. It is derived from the verified snapshot and remains a
/// lookup cache rather than a second projection authority.
pub(in crate::graph_projection) struct InteractiveCatalog {
    pub(super) symbols: SortedMap<SymbolOccurrenceId, CatalogSymbol>,
    pub(super) by_qualified_name: SortedMap<String, SymbolIds>,
    /// Keyed by the lowercased trailing segment of the qualified name (split
    /// on `::`, then `.`); the projection does not carry a separate simple
    /// name, so this derivation is the documented lookup semantic.
    pub(super) by_simple_name: SortedMap<String, SymbolIds>,
    pub(super) by_file: SortedMap<FileOccurrenceId, SymbolIds>,
    /// Logical path (as published on each file entity's `SanitizedCodeFileV1`
    /// payload) to the file occurrence it names.
    pub(super) by_logical_path: SortedMap<String, FileOccurrenceId>,
    pub(super) files: SortedMap<FileOccurrenceId, SanitizedCodeFileV1>,
    pub(super) imports: Vec<CodeIndexImportEvidenceV1>,
    pub(super) unresolved_call_sources: SortedMap<String, SymbolIds>,
    pub(super) symbols_by_kind: SortedMap<String, u64>,
    pub(super) files_by_language: SortedMap<String, u64>,
    pub(super) largest_files: Vec<CodeGraphFileSymbolCountV1>,
    pub(super) semantic_edges: u64,
    pub(super) file_dependencies: CodeGraphFileDependenciesV1,
    /// The heap the catalog was built in, with the bytes of its pages when
    /// the build returned. Last, so every map above drops before the heap.
    pub(super) heap: Option<(OwnerHeapV1, u64)>,
}

/// The catalog while a scan fills it: ordered maps that take one entry at a
/// time, frozen into an [`InteractiveCatalog`] once every entity is recorded.
pub(super) struct CatalogBuilder {
    pub(super) symbols: BTreeMap<SymbolOccurrenceId, CatalogSymbol>,
    by_qualified_name: BTreeMap<String, Vec<SymbolOccurrenceId>>,
    by_simple_name: BTreeMap<String, Vec<SymbolOccurrenceId>>,
    by_file: BTreeMap<FileOccurrenceId, Vec<SymbolOccurrenceId>>,
    /// Two distinct file occurrences claiming the same logical path in one
    /// generation is a corrupt projection, refused while the catalog is
    /// built rather than resolved by picking a winner.
    pub(super) by_logical_path: BTreeMap<String, FileOccurrenceId>,
    pub(super) files: BTreeMap<FileOccurrenceId, SanitizedCodeFileV1>,
    unresolved_call_sources: BTreeMap<String, Vec<SymbolOccurrenceId>>,
    symbols_by_kind: BTreeMap<String, u64>,
    symbols_by_logical_path: BTreeMap<String, u64>,
}

impl CatalogBuilder {
    pub(super) fn new() -> Self {
        Self {
            symbols: BTreeMap::new(),
            by_qualified_name: BTreeMap::new(),
            by_simple_name: BTreeMap::new(),
            by_file: BTreeMap::new(),
            by_logical_path: BTreeMap::new(),
            files: BTreeMap::new(),
            unresolved_call_sources: BTreeMap::new(),
            symbols_by_kind: BTreeMap::new(),
            symbols_by_logical_path: BTreeMap::new(),
        }
    }

    /// Derives the generation-wide aggregates once every file and symbol,
    /// with its degrees, is recorded, and freezes every map.
    pub(super) fn finish(
        self,
        imports: Vec<CodeIndexImportEvidenceV1>,
        file_dependencies: CodeGraphFileDependenciesV1,
    ) -> InteractiveCatalog {
        let mut files_by_language = BTreeMap::new();
        for file in self.files.values() {
            if let Some(language) = &file.language {
                *files_by_language
                    .entry(language.as_str().to_owned())
                    .or_default() += 1;
            }
        }
        let mut largest_files: Vec<_> = self
            .symbols_by_logical_path
            .into_iter()
            .map(|(logical_path, symbols)| CodeGraphFileSymbolCountV1 {
                logical_path,
                symbols,
            })
            .collect();
        largest_files.sort_by(|left, right| {
            right
                .symbols
                .cmp(&left.symbols)
                .then_with(|| left.logical_path.cmp(&right.logical_path))
        });
        let semantic_edges = self.symbols.values().map(|symbol| symbol.outgoing).sum();
        InteractiveCatalog {
            symbols: self.symbols.into(),
            by_qualified_name: freeze_ids(self.by_qualified_name),
            by_simple_name: freeze_ids(self.by_simple_name),
            by_file: freeze_ids(self.by_file),
            by_logical_path: self.by_logical_path.into(),
            files: self.files.into(),
            imports,
            unresolved_call_sources: freeze_ids(self.unresolved_call_sources),
            symbols_by_kind: self.symbols_by_kind.into(),
            files_by_language: files_by_language.into(),
            largest_files,
            semantic_edges,
            file_dependencies,
            heap: None,
        }
    }

    pub(super) fn insert(&mut self, occurrence: SymbolOccurrenceId, record: CatalogSymbol) {
        for reference in &record.unresolved_calls {
            let sources = self
                .unresolved_call_sources
                .entry(unresolved_callee_name(&reference.reference_name).to_owned())
                .or_default();
            if sources.last() != Some(&occurrence) {
                sources.push(occurrence.clone());
            }
        }
        if let Some(metadata) = &record.metadata {
            *self
                .symbols_by_kind
                .entry(metadata.kind.clone())
                .or_default() += 1;
            self.by_qualified_name
                .entry(metadata.qualified_name.clone())
                .or_default()
                .push(occurrence.clone());
            self.by_simple_name
                .entry(derived_simple_name(&metadata.qualified_name))
                .or_default()
                .push(occurrence.clone());
        }
        if let Some(binding) = &record.binding {
            if let Some(path) = &binding.logical_path {
                *self
                    .symbols_by_logical_path
                    .entry(path.clone())
                    .or_default() += 1;
            }
            self.by_file
                .entry(binding.file.clone())
                .or_default()
                .push(occurrence.clone());
        }
        self.symbols.insert(occurrence, record);
    }
}

impl InteractiveCatalog {
    /// What the catalog keeps resident: the pages of the heap it was built
    /// in, fragmentation included, and never less than [`Self::retained_bytes`].
    pub(in crate::graph_projection) fn resident_bytes(&self) -> u64 {
        self.retained_bytes()
            .max(self.heap.as_ref().map_or(0, |(_, bytes)| *bytes))
    }

    /// Bytes the catalog holds: every entry slice and the strings, lists,
    /// and records each entry owns, and the shared dependency adjacency.
    pub(in crate::graph_projection) fn retained_bytes(&self) -> u64 {
        let id = |id: &SymbolOccurrenceId| id.as_str().len();
        let ids = |ids: &SymbolIds| {
            ids.iter().fold(
                ids.len().saturating_mul(size_of::<SymbolOccurrenceId>()),
                |bytes, occurrence| bytes.saturating_add(id(occurrence)),
            )
        };
        let symbol = |occurrence: &SymbolOccurrenceId, symbol: &CatalogSymbol| {
            id(occurrence)
                .saturating_add(symbol.binding.as_ref().map_or(0, binding_heap_bytes))
                .saturating_add(symbol.metadata.as_ref().map_or(0, symbol_heap_bytes))
                .saturating_add(vec_bytes(
                    &symbol.unresolved_calls,
                    symbol.unresolved_calls.capacity(),
                    unresolved_heap_bytes,
                ))
        };
        let named_ids = |name: &String, list: &SymbolIds| name.capacity().saturating_add(ids(list));
        let counted = |name: &String, _: &u64| name.capacity();
        let adjacency = &self.file_dependencies.adjacency;
        let adjacency_bytes = adjacency.iter().fold(
            ARC_HEADER_BYTES
                .saturating_add(size_of::<HashMap<String, HashSet<String>>>())
                .saturating_add(hash_table_bytes::<(String, HashSet<String>)>(
                    adjacency.capacity(),
                )),
            |bytes, (path, dependencies)| {
                dependencies.iter().fold(
                    bytes
                        .saturating_add(path.capacity())
                        .saturating_add(hash_table_bytes::<String>(dependencies.capacity())),
                    |bytes, dependency| bytes.saturating_add(dependency.capacity()),
                )
            },
        );
        let bytes = ARC_HEADER_BYTES
            .saturating_add(size_of::<Self>())
            .saturating_add(self.symbols.bytes(symbol))
            .saturating_add(self.by_qualified_name.bytes(named_ids))
            .saturating_add(self.by_simple_name.bytes(named_ids))
            .saturating_add(self.unresolved_call_sources.bytes(named_ids))
            .saturating_add(
                self.by_file
                    .bytes(|file, list| file.as_str().len().saturating_add(ids(list))),
            )
            .saturating_add(
                self.by_logical_path
                    .bytes(|path, file| path.capacity().saturating_add(file.as_str().len())),
            )
            .saturating_add(self.files.bytes(|id, file| {
                id.as_str()
                    .len()
                    .saturating_add(snapshot_file_heap_bytes(file))
            }))
            .saturating_add(vec_bytes(
                &self.imports,
                self.imports.capacity(),
                import_heap_bytes,
            ))
            .saturating_add(self.symbols_by_kind.bytes(counted))
            .saturating_add(self.files_by_language.bytes(counted))
            .saturating_add(vec_bytes(
                &self.largest_files,
                self.largest_files.capacity(),
                |file| file.logical_path.capacity(),
            ))
            .saturating_add(adjacency_bytes);
        u64::try_from(bytes).unwrap_or(u64::MAX)
    }

    pub(super) fn symbol_summary(
        occurrence: &SymbolOccurrenceId,
        record: &CatalogSymbol,
    ) -> CodeGraphSymbolSummaryV1 {
        CodeGraphSymbolSummaryV1 {
            occurrence: occurrence.clone(),
            binding: record.binding.clone(),
            metadata: record.metadata.clone(),
        }
    }

    pub(super) fn summary(
        &self,
        occurrence: &SymbolOccurrenceId,
    ) -> Option<CodeGraphSymbolSummaryV1> {
        self.symbols
            .get(occurrence)
            .map(|record| Self::symbol_summary(occurrence, record))
    }
}

fn binding_heap_bytes(binding: &CodeGraphSymbolBindingV1) -> usize {
    binding
        .file
        .as_str()
        .len()
        .saturating_add(binding.logical_path.as_ref().map_or(0, String::capacity))
        .saturating_add(opt(binding.chunk.as_ref().map(|chunk| chunk.as_str())))
        .saturating_add(binding.language_descriptor_revision.as_str().len())
}

/// Lowercased trailing path segment of a qualified name.
fn derived_simple_name(qualified_name: &str) -> String {
    let tail = qualified_name.rsplit("::").next().unwrap_or(qualified_name);
    let tail = tail.rsplit('.').next().unwrap_or(tail);
    tail.to_lowercase()
}

/// The callee name an unresolved call could bind: the member of a dotted
/// receiver call (without turbofish), otherwise the last path segment.
pub(super) fn unresolved_callee_name(reference_name: &str) -> &str {
    match reference_name.rsplit_once('.') {
        Some((_, member)) => member.split("::").next().unwrap_or(member),
        None => reference_name.rsplit("::").next().unwrap_or(reference_name),
    }
}
