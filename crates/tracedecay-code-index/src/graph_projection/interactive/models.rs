//! Public interactive graph results and the generation-pinned lookup catalog.

use std::collections::BTreeMap;

use tracedecay_domain::{
    CanonicalRelationEdgeV1, FileOccurrenceId, RelationEdgeKindV1, SanitizedCodeFileV1,
    SymbolOccurrenceId,
};

use super::super::CodeGraphSymbolBindingV1;
use crate::chunks::{CodeIndexImportEvidenceV1, CodeIndexUnresolvedReferenceV1};
use crate::lineage::LineageSymbolRecordV1;

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CatalogSymbol {
    pub(super) binding: Option<CodeGraphSymbolBindingV1>,
    pub(super) metadata: Option<LineageSymbolRecordV1>,
    pub(super) unresolved_calls: Vec<CodeIndexUnresolvedReferenceV1>,
    /// Semantic degree: `CodeRelationSource` relations leaving the symbol
    /// and `CodeRelationTarget` relations reaching it, the same counts
    /// [`super::CodeGraphInteractiveReader::degrees`] reads from adjacency.
    pub(super) outgoing: u64,
    pub(super) incoming: u64,
}

/// Generation-pinned catalog of every file, symbol, and import entity in one
/// published graph. It is derived from the verified snapshot and remains a
/// lookup cache rather than a second projection authority.
pub(in crate::graph_projection) struct InteractiveCatalog {
    pub(super) symbols: BTreeMap<SymbolOccurrenceId, CatalogSymbol>,
    pub(super) by_qualified_name: BTreeMap<String, Vec<SymbolOccurrenceId>>,
    /// Keyed by the lowercased trailing segment of the qualified name (split
    /// on `::`, then `.`); the projection does not carry a separate simple
    /// name, so this derivation is the documented lookup semantic.
    pub(super) by_simple_name: BTreeMap<String, Vec<SymbolOccurrenceId>>,
    pub(super) by_file: BTreeMap<FileOccurrenceId, Vec<SymbolOccurrenceId>>,
    /// Logical path (as published on each file entity's `SanitizedCodeFileV1`
    /// payload) to the file occurrence it names. Built from the projection's
    /// `FILE_LABEL` entities; two distinct file occurrences claiming the same
    /// logical path in one generation is a corrupt projection, refused while
    /// the catalog is built rather than resolved by picking a winner.
    pub(super) by_logical_path: BTreeMap<String, FileOccurrenceId>,
    pub(super) files: BTreeMap<FileOccurrenceId, SanitizedCodeFileV1>,
    pub(super) imports: Vec<CodeIndexImportEvidenceV1>,
    pub(super) unresolved_call_sources: BTreeMap<String, Vec<SymbolOccurrenceId>>,
    pub(super) symbols_by_kind: BTreeMap<String, u64>,
    symbols_by_logical_path: BTreeMap<String, u64>,
    /// Filled by [`Self::finalize`].
    pub(super) files_by_language: BTreeMap<String, u64>,
    pub(super) largest_files: Vec<CodeGraphFileSymbolCountV1>,
    pub(super) semantic_edges: u64,
}

impl InteractiveCatalog {
    pub(in crate::graph_projection) fn empty() -> Self {
        Self {
            symbols: BTreeMap::new(),
            by_qualified_name: BTreeMap::new(),
            by_simple_name: BTreeMap::new(),
            by_file: BTreeMap::new(),
            by_logical_path: BTreeMap::new(),
            files: BTreeMap::new(),
            imports: Vec::new(),
            unresolved_call_sources: BTreeMap::new(),
            symbols_by_kind: BTreeMap::new(),
            symbols_by_logical_path: BTreeMap::new(),
            files_by_language: BTreeMap::new(),
            largest_files: Vec::new(),
            semantic_edges: 0,
        }
    }

    /// Derives the generation-wide aggregates once every file and symbol,
    /// with its degrees, is recorded.
    pub(super) fn finalize(&mut self) {
        self.files_by_language.clear();
        for file in self.files.values() {
            if let Some(language) = &file.language {
                *self
                    .files_by_language
                    .entry(language.as_str().to_owned())
                    .or_default() += 1;
            }
        }
        let mut largest_files: Vec<_> = std::mem::take(&mut self.symbols_by_logical_path)
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
        self.largest_files = largest_files;
        self.semantic_edges = self.symbols.values().map(|symbol| symbol.outgoing).sum();
    }

    pub(super) fn insert(&mut self, occurrence: SymbolOccurrenceId, record: CatalogSymbol) {
        for reference in &record.unresolved_calls {
            if let Some(member) = reference.reference_name.rsplit('.').next() {
                let method = member.split("::").next().unwrap_or(member);
                let sources = self
                    .unresolved_call_sources
                    .entry(method.to_owned())
                    .or_default();
                if sources.last() != Some(&occurrence) {
                    sources.push(occurrence.clone());
                }
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

/// Lowercased trailing path segment of a qualified name.
fn derived_simple_name(qualified_name: &str) -> String {
    let tail = qualified_name.rsplit("::").next().unwrap_or(qualified_name);
    let tail = tail.rsplit('.').next().unwrap_or(tail);
    tail.to_lowercase()
}
