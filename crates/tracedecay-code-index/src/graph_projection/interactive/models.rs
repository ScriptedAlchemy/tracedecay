//! Public interactive graph results and the generation-pinned lookup catalog.

use std::collections::BTreeMap;

use tracedecay_domain::{
    CanonicalRelationEdgeV1, FileOccurrenceId, RelationEdgeKindV1, SanitizedCodeFileV1,
    SymbolOccurrenceId,
};

use super::super::CodeGraphSymbolBindingV1;
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

/// Symbols of one generation ranked by total semantic degree.
///
/// `complete` is `false` exactly when the examination budget stopped the scan
/// before every symbol of the generation had been measured, so a ranking over
/// a prefix of the graph can never be mistaken for the whole graph's ranking.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeGraphDegreeRankingV1 {
    pub ranked: Vec<CodeGraphSymbolDegreesV1>,
    pub symbols_examined: usize,
    pub complete: bool,
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
}

/// Generation-pinned symbol catalog: every symbol entity of one published
/// generation, keyed for interactive lookup.
pub(in crate::graph_projection) struct SymbolCatalog {
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
}

impl SymbolCatalog {
    pub(super) fn insert(&mut self, occurrence: SymbolOccurrenceId, record: CatalogSymbol) {
        if let Some(metadata) = &record.metadata {
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
            self.by_file
                .entry(binding.file.clone())
                .or_default()
                .push(occurrence.clone());
        }
        self.symbols.insert(occurrence, record);
    }

    pub(super) fn summary(
        &self,
        occurrence: &SymbolOccurrenceId,
    ) -> Option<CodeGraphSymbolSummaryV1> {
        self.symbols
            .get(occurrence)
            .map(|record| CodeGraphSymbolSummaryV1 {
                occurrence: occurrence.clone(),
                binding: record.binding.clone(),
                metadata: record.metadata.clone(),
            })
    }
}

/// Lowercased trailing path segment of a qualified name.
fn derived_simple_name(qualified_name: &str) -> String {
    let tail = qualified_name.rsplit("::").next().unwrap_or(qualified_name);
    let tail = tail.rsplit('.').next().unwrap_or(tail);
    tail.to_lowercase()
}
