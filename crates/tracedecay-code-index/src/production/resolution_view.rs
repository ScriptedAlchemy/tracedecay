//! What cross-file resolution reads of a generation's files.
//!
//! Resolution decides a reference against every file of a generation, but it
//! reads most files only for their path and language, and their symbols only
//! by simple name. A file set therefore offers those facts without its
//! artifacts, and the symbols by name through [`SymbolsByNameV1`], so a
//! sealed generation can answer them from its snapshot and resolution index
//! while decoding only the files a lookup actually walks into.

use std::collections::HashMap;
use std::hash::Hash;
use std::mem::size_of;
use std::sync::Arc;

use super::resident_bytes::{
    abstention_heap_bytes, edge_heap_bytes, go_method_set_heap_bytes, import_heap_bytes,
    symbol_bytes, unresolved_heap_bytes, vec_bytes,
};

use super::{CodeIndexProductionErrorV1, FileGenerationArtifactsV1};
use crate::chunks::{
    CodeIndexCallableArityV1, CodeIndexEdgeAbstentionV1, CodeIndexGoMethodSetRowV1,
    CodeIndexImportEvidenceV1, CodeIndexUnresolvedReferenceV1,
};
use crate::lineage::LineageSymbolRecordV1;
use tracedecay_domain::CanonicalRelationEdgeV1;

/// Changes in live representation bytes; the caller's construction guard is
/// the admission authority. Releases happen only after the old allocation dies.
#[derive(Clone, Copy, Debug)]
pub(super) enum ResolutionMemoryChangeV1 {
    Retain(usize),
    Release(usize),
}

impl ResolutionMemoryChangeV1 {
    pub(super) fn apply_to(self, live: usize) -> Result<usize, CodeIndexProductionErrorV1> {
        match self {
            Self::Retain(bytes) => live.checked_add(bytes),
            Self::Release(bytes) => live.checked_sub(bytes),
        }
        .ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "resolution live-byte adjustment overflowed or underflowed".to_owned(),
            )
        })
    }

    pub(super) fn apply_with(
        self,
        live: &mut usize,
        retain: impl FnOnce(usize) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        let next = self.apply_to(*live)?;
        retain(next)?;
        *live = next;
        Ok(())
    }

    pub(super) fn apply_to_u64(self, live: u64) -> Result<u64, CodeIndexProductionErrorV1> {
        let convert = |bytes| {
            u64::try_from(bytes).map_err(|_| {
                CodeIndexProductionErrorV1::Contract("resolution change exceeds u64".to_owned())
            })
        };
        match self {
            Self::Retain(bytes) => live.checked_add(convert(bytes)?),
            Self::Release(bytes) => live.checked_sub(convert(bytes)?),
        }
        .ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "resolution live-byte adjustment overflowed or underflowed".to_owned(),
            )
        })
    }
}

fn allocation_error(error: std::collections::TryReserveError) -> CodeIndexProductionErrorV1 {
    CodeIndexProductionErrorV1::Contract(format!("resolution allocation failed: {error}"))
}

/// The pinned Global allocator's exact Vec capacity and SwissTable allocation
/// class are admitted in full beside the still-live old body. Settlement can
/// only release bytes; it cannot admit an allocation after it happened.
fn settle_replacement(
    before: usize,
    replacement: usize,
    actual: usize,
    charge: &mut dyn FnMut(ResolutionMemoryChangeV1) -> Result<(), CodeIndexProductionErrorV1>,
) -> Result<(), CodeIndexProductionErrorV1> {
    let unused = replacement.checked_sub(actual).ok_or_else(|| {
        CodeIndexProductionErrorV1::Contract(
            "resolution allocation exceeded its admitted capacity class".to_owned(),
        )
    })?;
    let release = before.checked_add(unused).ok_or_else(|| {
        CodeIndexProductionErrorV1::Contract("resolution release exceeds usize".to_owned())
    })?;
    charge(ResolutionMemoryChangeV1::Release(release))
}

pub(super) fn reserve_resolution_vec<T>(
    values: &mut Vec<T>,
    additional: usize,
    charge: &mut dyn FnMut(ResolutionMemoryChangeV1) -> Result<(), CodeIndexProductionErrorV1>,
) -> Result<(), CodeIndexProductionErrorV1> {
    let needed = values.len().checked_add(additional).ok_or_else(|| {
        CodeIndexProductionErrorV1::Contract("resolution vector length overflowed".to_owned())
    })?;
    if needed <= values.capacity() {
        return Ok(());
    }
    let capacity = needed
        .max(values.capacity().checked_mul(2).ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract("resolution vector capacity overflowed".to_owned())
        })?)
        .max(4);
    let before = values
        .capacity()
        .checked_mul(size_of::<T>())
        .ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract("resolution vector bytes overflowed".to_owned())
        })?;
    let replacement = capacity.checked_mul(size_of::<T>()).ok_or_else(|| {
        CodeIndexProductionErrorV1::Contract("resolution vector bytes overflowed".to_owned())
    })?;
    let _layout = std::alloc::Layout::array::<T>(capacity).map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "resolution vector layout is invalid: {error}"
        ))
    })?;
    charge(ResolutionMemoryChangeV1::Retain(replacement))?;
    if let Err(error) = values.try_reserve_exact(capacity - values.len()) {
        charge(ResolutionMemoryChangeV1::Release(replacement))?;
        return Err(allocation_error(error));
    }
    let actual = values
        .capacity()
        .checked_mul(size_of::<T>())
        .ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract("resolution vector bytes overflowed".to_owned())
        })?;
    settle_replacement(before, replacement, actual, charge)
}

pub(super) fn reserve_resolution_map<K: Eq + Hash, V>(
    values: &mut HashMap<K, V>,
    charge: &mut dyn FnMut(ResolutionMemoryChangeV1) -> Result<(), CodeIndexProductionErrorV1>,
) -> Result<(), CodeIndexProductionErrorV1> {
    if values.len() < values.capacity() {
        return Ok(());
    }
    let requested = values.capacity().checked_add(1).ok_or_else(|| {
        CodeIndexProductionErrorV1::Contract("resolution map capacity overflowed".to_owned())
    })?;
    let capacity = super::resident_bytes::hash_table_capacity::<(K, V)>(requested)?;
    let before = super::resident_bytes::hash_table_bytes::<(K, V)>(values.capacity());
    let replacement = super::resident_bytes::hash_table_bytes::<(K, V)>(capacity);
    charge(ResolutionMemoryChangeV1::Retain(replacement))?;
    if let Err(error) = values.try_reserve(capacity - values.len()) {
        charge(ResolutionMemoryChangeV1::Release(replacement))?;
        return Err(allocation_error(error));
    }
    let actual = super::resident_bytes::hash_table_bytes::<(K, V)>(values.capacity());
    settle_replacement(before, replacement, actual, charge)
}

pub(super) fn reserve_resolution_set<T: Eq + Hash>(
    values: &mut std::collections::HashSet<T>,
    charge: &mut dyn FnMut(ResolutionMemoryChangeV1) -> Result<(), CodeIndexProductionErrorV1>,
) -> Result<(), CodeIndexProductionErrorV1> {
    if values.len() < values.capacity() {
        return Ok(());
    }
    let requested = values.capacity().checked_add(1).ok_or_else(|| {
        CodeIndexProductionErrorV1::Contract("resolution set capacity overflowed".to_owned())
    })?;
    let capacity = super::resident_bytes::hash_table_capacity::<T>(requested)?;
    let before = super::resident_bytes::hash_table_bytes::<T>(values.capacity());
    let replacement = super::resident_bytes::hash_table_bytes::<T>(capacity);
    charge(ResolutionMemoryChangeV1::Retain(replacement))?;
    if let Err(error) = values.try_reserve(capacity - values.len()) {
        charge(ResolutionMemoryChangeV1::Release(replacement))?;
        return Err(allocation_error(error));
    }
    let actual = super::resident_bytes::hash_table_bytes::<T>(values.capacity());
    settle_replacement(before, replacement, actual, charge)
}

/// The binding facts a resolution pass may retain after dropping a file page.
/// No source text, chunk, exact-authority, or clone payload is retained here.
#[derive(Clone)]
pub(super) struct CompactFileResolutionV1 {
    logical_path: String,
    language: String,
    symbols: Vec<Arc<LineageSymbolRecordV1>>,
    imports: Vec<CodeIndexImportEvidenceV1>,
    unresolved_references: Vec<CodeIndexUnresolvedReferenceV1>,
    callable_arities: Vec<CodeIndexCallableArityV1>,
    go_method_sets: Vec<CodeIndexGoMethodSetRowV1>,
    edges: Vec<CanonicalRelationEdgeV1>,
    edge_abstentions: Vec<CodeIndexEdgeAbstentionV1>,
    graph_owners: Vec<tracedecay_domain::SymbolOccurrenceId>,
}

/// Compose the decoded file's existing canonical allocation authorities. No
/// source estimate is inferred from encoded bytes or corpus cardinality.
pub(super) fn resolution_source_bytes(
    file: &FileGenerationArtifactsV1,
) -> Result<usize, CodeIndexProductionErrorV1> {
    let mut bytes = super::resident_bytes::file_bytes(file);
    for chunk in &file.artifacts.chunks.chunks {
        bytes = bytes
            .checked_add(super::resident_bytes::chunk_bytes(chunk))
            .ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "resolution source bytes overflowed".to_owned(),
                )
            })?;
    }
    for symbol in &file.artifacts.symbols {
        bytes = bytes.checked_add(symbol_bytes(symbol)).ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract("resolution source bytes overflowed".to_owned())
        })?;
    }
    Ok(bytes)
}

/// One traversal describes both retained vectors and cloned outer vectors.
/// Heap helpers remain the canonical capacity bounds; DTO cloning can only
/// reduce their nested String/Vec capacities to the source lengths.
fn resolution_rows_bytes(file: &dyn ResolutionFileFactsV1, cloned: bool) -> usize {
    fn capacity<T>(rows: &Vec<T>, cloned: bool) -> usize {
        if cloned { rows.len() } else { rows.capacity() }
    }
    vec_bytes(file.symbols(), capacity(file.symbols(), cloned), |_| 0)
        .saturating_add(vec_bytes(
            file.imports(),
            capacity(file.imports(), cloned),
            import_heap_bytes,
        ))
        .saturating_add(vec_bytes(
            file.unresolved_references(),
            capacity(file.unresolved_references(), cloned),
            unresolved_heap_bytes,
        ))
        .saturating_add(vec_bytes(
            file.callable_arities(),
            capacity(file.callable_arities(), cloned),
            |row| row.occurrence.as_str().len(),
        ))
        .saturating_add(vec_bytes(
            file.go_method_sets(),
            capacity(file.go_method_sets(), cloned),
            go_method_set_heap_bytes,
        ))
        .saturating_add(vec_bytes(
            file.edges(),
            capacity(file.edges(), cloned),
            edge_heap_bytes,
        ))
        .saturating_add(vec_bytes(
            file.edge_abstentions(),
            capacity(file.edge_abstentions(), cloned),
            abstention_heap_bytes,
        ))
}

impl CompactFileResolutionV1 {
    /// Admit every copied allocation and graph-owner workspace from borrowed
    /// artifacts before either the borrowed set or the first clone is built.
    /// The caller keeps this bound until its source owner has also dropped.
    pub(super) fn from_file(
        file: &FileGenerationArtifactsV1,
        retain: &mut dyn FnMut(u64) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let owners = file
            .artifacts
            .chunks
            .chunks
            .iter()
            .filter_map(|chunk| chunk.anchor.symbol_occurrence_id.as_ref())
            .chain(
                file.artifacts
                    .symbols
                    .iter()
                    .map(|symbol| &symbol.occurrence),
            );
        let mut owner_count = 0_usize;
        let mut owner_heaps = 0_usize;
        for owner in owners {
            owner_count = owner_count.checked_add(1).ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract("resolution owner count overflowed".to_owned())
            })?;
            owner_heaps = owner_heaps
                .checked_add(owner.as_str().len())
                .ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "resolution owner bytes overflowed".to_owned(),
                    )
                })?;
        }
        let graph_vec = owner_count
            .checked_mul(size_of::<tracedecay_domain::SymbolOccurrenceId>())
            .and_then(|bytes| bytes.checked_add(owner_heaps))
            .ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract("resolution owner bytes overflowed".to_owned())
            })?;
        let copy_bytes = size_of::<Self>()
            .checked_add(file.authority.logical_path.len())
            .and_then(|bytes| bytes.checked_add(file.extraction.language.as_str().len()))
            .and_then(|bytes| bytes.checked_add(resolution_rows_bytes(file, true)))
            .and_then(|bytes| bytes.checked_add(graph_vec))
            .ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract("resolution clone bytes overflowed".to_owned())
            })?;
        let workspace = super::resident_bytes::btree_bytes(
            owner_count,
            size_of::<&tracedecay_domain::SymbolOccurrenceId>(),
        );
        let bound = copy_bytes.checked_add(workspace).ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract("resolution clone workspace overflowed".to_owned())
        })?;
        retain(u64::try_from(bound).map_err(|_| {
            CodeIndexProductionErrorV1::Contract("resolution clone bytes exceed u64".to_owned())
        })?)?;
        let owners = super::graph_pages::owned_occurrences(file);
        let mut graph_owners = Vec::with_capacity(owners.len());
        graph_owners.extend(owners.into_iter().cloned());
        let facts = Self {
            logical_path: file.authority.logical_path.clone(),
            language: file.extraction.language.as_str().to_owned(),
            symbols: file.artifacts.symbols.clone(),
            imports: file.artifacts.imports.clone(),
            unresolved_references: file.artifacts.unresolved_references.clone(),
            callable_arities: file.artifacts.callable_arities.clone(),
            go_method_sets: file.artifacts.go_method_sets.clone(),
            edges: file.artifacts.edges.clone(),
            edge_abstentions: file.artifacts.edge_abstentions.clone(),
            graph_owners,
        };
        if facts
            .retained_bytes()
            .saturating_sub(facts.owned_symbol_bytes())
            > copy_bytes
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "resolution clone exceeded its admitted capacity bound".to_owned(),
            ));
        }
        Ok(facts)
    }
    pub(super) fn take_edges(&mut self) -> Vec<CanonicalRelationEdgeV1> {
        std::mem::take(&mut self.edges)
    }

    pub(super) fn graph_owners(&self) -> &[tracedecay_domain::SymbolOccurrenceId] {
        &self.graph_owners
    }

    /// The graph phase needs only occurrence ownership after resolution is sealed.
    pub(super) fn into_graph_owners(self) -> Vec<tracedecay_domain::SymbolOccurrenceId> {
        self.graph_owners
    }

    pub(super) fn owned_symbol_bytes(&self) -> usize {
        self.symbols.iter().map(|symbol| symbol_bytes(symbol)).sum()
    }

    pub(super) fn retained_row_bytes(&self) -> usize {
        resolution_rows_bytes(self, false)
    }

    pub(super) fn graph_owner_bytes(&self) -> usize {
        vec_bytes(&self.graph_owners, self.graph_owners.capacity(), |id| {
            id.as_str().len()
        })
    }

    pub(super) fn retained_bytes(&self) -> usize {
        size_of::<Self>()
            .saturating_add(self.logical_path.capacity())
            .saturating_add(self.language.capacity())
            .saturating_add(self.retained_row_bytes())
            .saturating_add(self.owned_symbol_bytes())
            .saturating_add(self.graph_owner_bytes())
    }
}

/// Only stable binding facts are accessible through this boundary. File I/O
/// completes fallibly before a compact fact set enters a resolution pass.
pub(crate) trait ResolutionFileFactsV1: Send + Sync {
    fn logical_path(&self) -> &str;
    fn language(&self) -> &str;
    fn symbols(&self) -> &Vec<Arc<LineageSymbolRecordV1>>;
    fn imports(&self) -> &Vec<CodeIndexImportEvidenceV1>;
    fn unresolved_references(&self) -> &Vec<CodeIndexUnresolvedReferenceV1>;
    fn callable_arities(&self) -> &Vec<CodeIndexCallableArityV1>;
    fn go_method_sets(&self) -> &Vec<CodeIndexGoMethodSetRowV1>;
    fn edges(&self) -> &Vec<CanonicalRelationEdgeV1>;
    fn edge_abstentions(&self) -> &Vec<CodeIndexEdgeAbstentionV1>;
}

impl ResolutionFileFactsV1 for FileGenerationArtifactsV1 {
    fn logical_path(&self) -> &str {
        &self.authority.logical_path
    }
    fn language(&self) -> &str {
        self.extraction.language.as_str()
    }
    fn symbols(&self) -> &Vec<Arc<LineageSymbolRecordV1>> {
        &self.artifacts.symbols
    }
    fn imports(&self) -> &Vec<CodeIndexImportEvidenceV1> {
        &self.artifacts.imports
    }
    fn unresolved_references(&self) -> &Vec<CodeIndexUnresolvedReferenceV1> {
        &self.artifacts.unresolved_references
    }
    fn callable_arities(&self) -> &Vec<CodeIndexCallableArityV1> {
        &self.artifacts.callable_arities
    }
    fn go_method_sets(&self) -> &Vec<CodeIndexGoMethodSetRowV1> {
        &self.artifacts.go_method_sets
    }
    fn edges(&self) -> &Vec<CanonicalRelationEdgeV1> {
        &self.artifacts.edges
    }
    fn edge_abstentions(&self) -> &Vec<CodeIndexEdgeAbstentionV1> {
        &self.artifacts.edge_abstentions
    }
}

impl<T: ResolutionFileFactsV1 + ?Sized> ResolutionFileFactsV1 for Arc<T> {
    fn logical_path(&self) -> &str {
        ResolutionFileFactsV1::logical_path(self.as_ref())
    }
    fn language(&self) -> &str {
        ResolutionFileFactsV1::language(self.as_ref())
    }
    fn symbols(&self) -> &Vec<Arc<LineageSymbolRecordV1>> {
        ResolutionFileFactsV1::symbols(self.as_ref())
    }
    fn imports(&self) -> &Vec<CodeIndexImportEvidenceV1> {
        ResolutionFileFactsV1::imports(self.as_ref())
    }
    fn unresolved_references(&self) -> &Vec<CodeIndexUnresolvedReferenceV1> {
        ResolutionFileFactsV1::unresolved_references(self.as_ref())
    }
    fn callable_arities(&self) -> &Vec<CodeIndexCallableArityV1> {
        ResolutionFileFactsV1::callable_arities(self.as_ref())
    }
    fn go_method_sets(&self) -> &Vec<CodeIndexGoMethodSetRowV1> {
        ResolutionFileFactsV1::go_method_sets(self.as_ref())
    }
    fn edges(&self) -> &Vec<CanonicalRelationEdgeV1> {
        ResolutionFileFactsV1::edges(self.as_ref())
    }
    fn edge_abstentions(&self) -> &Vec<CodeIndexEdgeAbstentionV1> {
        ResolutionFileFactsV1::edge_abstentions(self.as_ref())
    }
}

impl ResolutionFileFactsV1 for CompactFileResolutionV1 {
    fn logical_path(&self) -> &str {
        &self.logical_path
    }
    fn language(&self) -> &str {
        &self.language
    }
    fn symbols(&self) -> &Vec<Arc<LineageSymbolRecordV1>> {
        &self.symbols
    }
    fn imports(&self) -> &Vec<CodeIndexImportEvidenceV1> {
        &self.imports
    }
    fn unresolved_references(&self) -> &Vec<CodeIndexUnresolvedReferenceV1> {
        &self.unresolved_references
    }
    fn callable_arities(&self) -> &Vec<CodeIndexCallableArityV1> {
        &self.callable_arities
    }
    fn go_method_sets(&self) -> &Vec<CodeIndexGoMethodSetRowV1> {
        &self.go_method_sets
    }
    fn edges(&self) -> &Vec<CanonicalRelationEdgeV1> {
        &self.edges
    }
    fn edge_abstentions(&self) -> &Vec<CodeIndexEdgeAbstentionV1> {
        &self.edge_abstentions
    }
}

impl<T: ResolutionFileFactsV1 + ?Sized> ResolutionFileFactsV1 for &T {
    fn logical_path(&self) -> &str {
        ResolutionFileFactsV1::logical_path(*self)
    }
    fn language(&self) -> &str {
        ResolutionFileFactsV1::language(*self)
    }
    fn symbols(&self) -> &Vec<Arc<LineageSymbolRecordV1>> {
        ResolutionFileFactsV1::symbols(*self)
    }
    fn imports(&self) -> &Vec<CodeIndexImportEvidenceV1> {
        ResolutionFileFactsV1::imports(*self)
    }
    fn unresolved_references(&self) -> &Vec<CodeIndexUnresolvedReferenceV1> {
        ResolutionFileFactsV1::unresolved_references(*self)
    }
    fn callable_arities(&self) -> &Vec<CodeIndexCallableArityV1> {
        ResolutionFileFactsV1::callable_arities(*self)
    }
    fn go_method_sets(&self) -> &Vec<CodeIndexGoMethodSetRowV1> {
        ResolutionFileFactsV1::go_method_sets(*self)
    }
    fn edges(&self) -> &Vec<CanonicalRelationEdgeV1> {
        ResolutionFileFactsV1::edges(*self)
    }
    fn edge_abstentions(&self) -> &Vec<CodeIndexEdgeAbstentionV1> {
        ResolutionFileFactsV1::edge_abstentions(*self)
    }
}

/// A file source can fail when its binding facts first load. The returned
/// facts belong to this one resolution operation, never a decoded file cache.
pub(crate) trait ResolutionFileV1: Sync {
    fn logical_path(&self) -> &str;
    fn language(&self) -> &str;
    fn binding(&self) -> Result<&dyn ResolutionFileFactsV1, CodeIndexProductionErrorV1>;
}

impl<T: ResolutionFileFactsV1> ResolutionFileV1 for T {
    fn logical_path(&self) -> &str {
        ResolutionFileFactsV1::logical_path(self)
    }
    fn language(&self) -> &str {
        ResolutionFileFactsV1::language(self)
    }
    fn binding(&self) -> Result<&dyn ResolutionFileFactsV1, CodeIndexProductionErrorV1> {
        Ok(self)
    }
}

/// A symbol a simple name finds: its file's index in the set and its record.
pub(crate) type NamedSymbolV1 = (usize, Arc<LineageSymbolRecordV1>);

/// Every symbol of a file set by simple name, each name's symbols in file
/// order and then in their file's symbol order.
pub(crate) trait SymbolsByNameV1: Sync {
    fn get(&self, name: &str) -> Result<Option<Vec<NamedSymbolV1>>, CodeIndexProductionErrorV1>;
}

/// [`SymbolsByNameV1`] over files held in memory.
pub(crate) struct FileSymbolsByNameV1<'f> {
    by_name: Vec<(usize, usize, &'f Arc<LineageSymbolRecordV1>)>,
}

impl<'f> FileSymbolsByNameV1<'f> {
    pub(crate) fn new<T: ResolutionFileV1>(
        files: &'f [T],
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        Self::new_charged(files, &mut |_| Ok(()))
    }

    pub(super) fn new_charged<T: ResolutionFileV1>(
        files: &'f [T],
        charge: &mut dyn FnMut(ResolutionMemoryChangeV1) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let mut count = 0_usize;
        for file in files {
            count = count
                .checked_add(file.binding()?.symbols().len())
                .ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "symbol lookup length overflowed".to_owned(),
                    )
                })?;
        }
        let mut by_name = Vec::new();
        reserve_resolution_vec(&mut by_name, count, charge)?;
        for (index, file) in files.iter().enumerate() {
            for (position, symbol) in file.binding()?.symbols().iter().enumerate() {
                by_name.push((index, position, symbol));
            }
        }
        // Explicit input ordinals preserve each name's file/symbol order
        // without stable-sort scratch or a separate allocation per name.
        by_name.sort_unstable_by(|left, right| {
            left.2
                .simple_name
                .cmp(&right.2.simple_name)
                .then_with(|| left.0.cmp(&right.0))
                .then_with(|| left.1.cmp(&right.1))
        });
        Ok(Self { by_name })
    }
}

impl SymbolsByNameV1 for FileSymbolsByNameV1<'_> {
    fn get(&self, name: &str) -> Result<Option<Vec<NamedSymbolV1>>, CodeIndexProductionErrorV1> {
        let start = self
            .by_name
            .partition_point(|row| row.2.simple_name.as_str() < name);
        let matching = &self.by_name[start..];
        let end = matching.partition_point(|row| row.2.simple_name.as_str() == name);
        Ok((end != 0).then(|| {
            matching[..end]
                .iter()
                .map(|row| (row.0, Arc::clone(row.2)))
                .collect()
        }))
    }
}

#[cfg(test)]
mod allocation_tests {
    use std::collections::{HashMap, HashSet};
    use std::mem::size_of;
    use std::num::NonZeroU64;
    use std::sync::Arc;

    use tracedecay_code_extraction::ImportReexportScopeV1;

    use tracedecay_runtime_core::resident_memory::{
        ProcessResidentMemoryV1, ProcessResidentSampleV1, ResidentMemoryComponentIdV1,
        ResidentMemoryPressureV1,
    };

    use super::{
        CompactFileResolutionV1, FileSymbolsByNameV1, ResolutionFileV1, ResolutionMemoryChangeV1,
        SymbolsByNameV1, reserve_resolution_map, reserve_resolution_set, reserve_resolution_vec,
        resolution_source_bytes,
    };
    use crate::production::resident_bytes::{hash_table_bytes, hash_table_capacity};
    use crate::production::{CodeIndexProductionErrorV1, ConstructionMemoryV1};

    use crate::production::sealed_parent::{
        SealedParentGenerationV1, SparseFileSourceV1, SparseFileV1,
    };
    use crate::production::sparse_increment_tests::{owner, request, scope};
    use crate::production::sparse_successor::resolve_staged_generation;
    use crate::production::{
        CodeIndexPublishedBuildV1, MemorySealedPublicationStoreV1, SealedSegmentReaderV1,
        UninterruptibleCodeIndexControlV1,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn published_fixture() -> (MemorySealedPublicationStoreV1, CodeIndexPublishedBuildV1) {
        let store = MemorySealedPublicationStoreV1::default();
        let mut owner = owner(&store);
        let request = request(
            &[(
                "src/lib.rs",
                "rust",
                "pub fn alpha() {}\npub fn beta() { alpha(); }\n",
            )],
            1_000_000,
        );
        let published = owner
            .build_and_publish(request, &UninterruptibleCodeIndexControlV1)
            .unwrap();
        (store, published)
    }

    #[test]
    fn symbol_lookup_preserves_duplicate_order_and_admits_before_retaining_rows() {
        let store = MemorySealedPublicationStoreV1::default();
        let mut owner = owner(&store);
        let published = owner
            .build_and_publish(
                request(
                    &[
                        (
                            "src/a.rs",
                            "rust",
                            "mod z { pub fn shared() {} } mod a { pub fn shared() {} }",
                        ),
                        ("src/b.rs", "rust", "pub fn shared() {} pub fn other() {}"),
                    ],
                    1_000_000,
                ),
                &UninterruptibleCodeIndexControlV1,
            )
            .unwrap();
        let decoded = crate::production::worker_tests::decoded_generation(&published);
        let authority = memory(1);
        let mut guard = ConstructionMemoryV1::new(authority.begin_operation()).unwrap();
        assert!(
            FileSymbolsByNameV1::new_charged(&decoded.files, &mut |change| {
                guard.resize(change.apply_to_u64(guard.charged_bytes)?)
            })
            .is_err()
        );
        assert_eq!(authority.snapshot().used_bytes, 0);

        let authority = memory(1024 * 1024);
        let mut guard = ConstructionMemoryV1::new(authority.begin_operation()).unwrap();
        let lookup = FileSymbolsByNameV1::new_charged(&decoded.files, &mut |change| {
            guard.resize(change.apply_to_u64(guard.charged_bytes)?)
        })
        .unwrap();
        let expected = decoded
            .files
            .iter()
            .enumerate()
            .flat_map(|(index, file)| {
                file.artifacts
                    .symbols
                    .iter()
                    .filter(|symbol| symbol.simple_name == "shared")
                    .map(move |symbol| (index, symbol))
            })
            .collect::<Vec<_>>();
        assert_eq!(expected.len(), 3);
        let actual = lookup.get("shared").unwrap().unwrap();
        assert_eq!(actual.len(), expected.len());
        for ((actual_index, actual_symbol), (expected_index, expected_symbol)) in
            actual.iter().zip(expected)
        {
            assert_eq!(*actual_index, expected_index);
            assert!(Arc::ptr_eq(actual_symbol, expected_symbol));
        }
        assert!(lookup.get("absent").unwrap().is_none());
        drop(lookup);
        drop(guard);
        assert_eq!(authority.snapshot().used_bytes, 0);
    }

    fn memory(limit: u64) -> Arc<ProcessResidentMemoryV1> {
        let limit = NonZeroU64::new(limit).unwrap();
        Arc::new(ProcessResidentMemoryV1::with_pressure(
            limit,
            Arc::new(ResidentMemoryPressureV1::with_sampler(
                limit,
                Arc::new(|| {
                    Some(ProcessResidentSampleV1 {
                        resident_bytes: 0,
                        unreclaimable_bytes: 0,
                        swapped_bytes: 0,
                        cgroup_headroom_bytes: None,
                    })
                }),
            )),
        ))
    }

    #[test]
    fn vector_replacement_needs_old_and_new_bodies_then_settles_to_actual_capacity() {
        let mut values = vec![1_usize, 2, 3, 4];
        let old_capacity = values.capacity();
        let old = old_capacity * size_of::<usize>();
        let replacement = old_capacity * 2 * size_of::<usize>();
        let authority = memory((old + replacement) as u64);
        let mut guard = ConstructionMemoryV1::new(authority.begin_operation()).unwrap();
        guard.resize(old as u64).unwrap();
        let component = ResidentMemoryComponentIdV1::new("test.resolution.foreign.v1").unwrap();
        let foreign = authority
            .reserve_process_shared(component, NonZeroU64::new(1).unwrap())
            .unwrap();
        let result = reserve_resolution_vec(&mut values, 1, &mut |change| {
            guard.resize(change.apply_to_u64(guard.charged_bytes)?)
        });
        assert!(matches!(
            result,
            Err(CodeIndexProductionErrorV1::Publication(_))
        ));
        assert_eq!(values, vec![1, 2, 3, 4]);
        assert_eq!(values.capacity(), old_capacity);
        assert_eq!(guard.charged_bytes, old as u64);
        assert_eq!(authority.snapshot().used_bytes, old as u64 + 1);
        drop(foreign);
        let mut peak = 0;
        reserve_resolution_vec(&mut values, 1, &mut |change| {
            guard.resize(change.apply_to_u64(guard.charged_bytes)?)?;
            peak = peak.max(authority.snapshot().used_bytes);
            Ok(())
        })
        .unwrap();
        assert_eq!(peak, (old + replacement) as u64);
        let actual = values.capacity() * size_of::<usize>();
        assert_eq!(guard.charged_bytes, actual as u64);
        assert_eq!(authority.snapshot().used_bytes, actual as u64);
        let charged = guard.charged_bytes;
        let capacity = values.capacity();
        assert!(
            reserve_resolution_vec(&mut values, usize::MAX, &mut |change| {
                guard.resize(change.apply_to_u64(guard.charged_bytes)?)
            })
            .is_err()
        );
        assert_eq!(values.capacity(), capacity);
        assert_eq!(guard.charged_bytes, charged);
        drop(values);
        drop(guard);
        assert_eq!(authority.snapshot().used_bytes, 0);
    }

    #[test]
    fn hash_replacement_denial_preserves_owners_and_success_releases_the_old_body() {
        let mut values = HashMap::<usize, usize>::with_capacity(3);
        for index in 0..values.capacity() {
            values.insert(index, index + 1);
        }
        let old_capacity = values.capacity();
        let old = hash_table_bytes::<(usize, usize)>(old_capacity);
        let replacement = hash_table_bytes::<(usize, usize)>(
            hash_table_capacity::<(usize, usize)>(old_capacity + 1).unwrap(),
        );
        let authority = memory((old + replacement) as u64);
        let mut guard = ConstructionMemoryV1::new(authority.begin_operation()).unwrap();
        guard.resize(old as u64).unwrap();
        let component = ResidentMemoryComponentIdV1::new("test.resolution.foreign.v1").unwrap();
        let foreign = authority
            .reserve_process_shared(component, NonZeroU64::new(1).unwrap())
            .unwrap();
        assert!(
            reserve_resolution_map(&mut values, &mut |change| {
                guard.resize(change.apply_to_u64(guard.charged_bytes)?)
            })
            .is_err()
        );
        assert_eq!(values.capacity(), old_capacity);
        for index in 0..old_capacity {
            assert_eq!(values.get(&index), Some(&(index + 1)));
        }
        assert_eq!(guard.charged_bytes, old as u64);
        assert_eq!(authority.snapshot().used_bytes, old as u64 + 1);
        drop(foreign);
        let mut peak = 0;
        reserve_resolution_map(&mut values, &mut |change| {
            guard.resize(change.apply_to_u64(guard.charged_bytes)?)?;
            peak = peak.max(authority.snapshot().used_bytes);
            Ok(())
        })
        .unwrap();
        assert_eq!(peak, (old + replacement) as u64);
        assert_eq!(
            guard.charged_bytes,
            hash_table_bytes::<(usize, usize)>(values.capacity()) as u64
        );
        drop(values);
        drop(guard);
        assert_eq!(authority.snapshot().used_bytes, 0);
    }

    #[test]
    fn tiny_hash_entries_use_the_allocated_class_and_checked_overflow() {
        fn assert_class<T: Eq + std::hash::Hash>() {
            let capacity = hash_table_capacity::<T>(1).unwrap();
            let mut values = HashSet::<T>::new();
            let mut live = 0;
            reserve_resolution_set(&mut values, &mut |change| {
                live = change.apply_to(live)?;
                Ok(())
            })
            .unwrap();
            assert_eq!(values.capacity(), capacity);
            assert_eq!(live, hash_table_bytes::<T>(values.capacity()));
        }
        assert_class::<u8>();
        assert_class::<u16>();
        assert_class::<[u8; 3]>();
        assert_class::<usize>();
        assert_eq!(hash_table_capacity::<u8>(0).unwrap(), 0);
        assert!(hash_table_capacity::<u8>(usize::MAX).is_err());
        assert!(hash_table_capacity::<[usize; 32]>(usize::MAX / 32).is_err());
        assert!(
            ResolutionMemoryChangeV1::Retain(1)
                .apply_to(usize::MAX)
                .is_err()
        );
        assert!(ResolutionMemoryChangeV1::Release(1).apply_to(0).is_err());
        let mut live = 9;
        assert!(
            ResolutionMemoryChangeV1::Retain(2)
                .apply_with(&mut live, |_| {
                    Err(CodeIndexProductionErrorV1::Contract("refused".to_owned()))
                })
                .is_err()
        );
        assert_eq!(
            live, 9,
            "a refused parent reservation cannot change the local live base"
        );
    }
    #[test]
    fn compact_fact_admission_precedes_clones_and_settles_after_source_release() {
        let (store, _published) = published_fixture();
        let mut decoded = store.decode_active(&scope()).unwrap().unwrap();
        let file = decoded.files.remove(0);
        drop(decoded);
        let symbol = &file.artifacts.symbols[0];
        let original_symbol_owners = Arc::strong_count(symbol);
        let denied = memory(1);
        let mut guard = ConstructionMemoryV1::new(denied.begin_operation()).unwrap();
        let mut admissions = 0;
        let result = CompactFileResolutionV1::from_file(&file, &mut |bytes| {
            admissions += 1;
            assert_eq!(
                Arc::strong_count(symbol),
                original_symbol_owners,
                "admission must precede the first symbol-handle clone"
            );
            guard.resize(bytes)
        });
        assert!(matches!(
            result,
            Err(CodeIndexProductionErrorV1::Publication(_))
        ));
        assert_eq!(admissions, 1);
        assert_eq!(denied.snapshot().used_bytes, 0);
        assert_eq!(Arc::strong_count(symbol), original_symbol_owners);
        drop(guard);

        let authority = memory(64 * 1024 * 1024);
        let mut guard = ConstructionMemoryV1::new(authority.begin_operation()).unwrap();
        let source_bytes = resolution_source_bytes(&file).unwrap() as u64;
        let weak_source = Arc::downgrade(&file);
        let facts = CompactFileResolutionV1::from_file(&file, &mut |copies| {
            assert_eq!(
                Arc::strong_count(&file.artifacts.symbols[0]),
                original_symbol_owners
            );
            guard.resize(source_bytes.checked_add(copies).unwrap())
        })
        .unwrap();
        let overlap = guard.charged_bytes;
        assert_eq!(authority.snapshot().used_bytes, overlap);
        drop(file);
        assert!(weak_source.upgrade().is_none());
        let retained = facts.retained_bytes() as u64;
        assert!(
            retained < overlap,
            "source and graph workspace must not accumulate"
        );
        guard.resize(retained).unwrap();
        assert_eq!(authority.snapshot().used_bytes, retained);
        drop(facts);
        drop(guard);
        assert_eq!(authority.snapshot().used_bytes, 0);
    }

    #[test]
    fn module_scoped_reexports_are_charged_in_source_and_compact_facts() {
        let store = MemorySealedPublicationStoreV1::default();
        let mut owner = owner(&store);
        let request = request(
            &[
                ("src/lib.rs", "rust", "pub mod long;\n"),
                ("src/long/mod.rs", "rust", "pub mod module;\n"),
                (
                    "src/long/module/mod.rs",
                    "rust",
                    "pub struct Item;\npub mod child;\n",
                ),
                (
                    "src/long/module/child.rs",
                    "rust",
                    "pub(in crate::long::module) use super::Item as ScopedItem;\n\
                     pub fn example(_: ScopedItem) {}\n",
                ),
            ],
            1_000_000,
        );
        let _published = owner
            .build_and_publish(request, &UninterruptibleCodeIndexControlV1)
            .unwrap();
        let mut decoded = store.decode_active(&scope()).unwrap().unwrap();
        let file_index = decoded
            .files
            .iter()
            .position(|file| file.authority.logical_path == "src/long/module/child.rs")
            .unwrap();
        let mut file = decoded.files.remove(file_index);
        drop(decoded);
        let index = file
            .artifacts
            .imports
            .iter()
            .position(|import| {
                matches!(
                    import.reexport_scope,
                    Some(ImportReexportScopeV1::Module(_))
                )
            })
            .expect("the real Rust extractor preserves module-scoped reexports");
        let authority = memory(64 * 1024 * 1024);
        let mut with_guard = ConstructionMemoryV1::new(authority.begin_operation()).unwrap();
        let with_scope =
            CompactFileResolutionV1::from_file(&file, &mut |bytes| with_guard.resize(bytes))
                .unwrap();
        let with_source = resolution_source_bytes(&file).unwrap();
        let scope = Arc::get_mut(&mut file).unwrap().artifacts.imports[index]
            .reexport_scope
            .take()
            .unwrap();
        let ImportReexportScopeV1::Module(module) = scope else {
            panic!("the selected reexport has an owned module path");
        };
        let without_source = resolution_source_bytes(&file).unwrap();
        assert_eq!(with_source - without_source, module.capacity());
        let mut without_guard = ConstructionMemoryV1::new(authority.begin_operation()).unwrap();
        let without_scope =
            CompactFileResolutionV1::from_file(&file, &mut |bytes| without_guard.resize(bytes))
                .unwrap();
        let Some(ImportReexportScopeV1::Module(copied_module)) =
            &with_scope.imports[index].reexport_scope
        else {
            panic!("compact facts retain the scoped import");
        };
        assert_eq!(
            with_scope.retained_bytes() - without_scope.retained_bytes(),
            copied_module.capacity()
        );
        let with_bytes = with_scope.retained_bytes() - with_scope.owned_symbol_bytes();
        let without_bytes = without_scope.retained_bytes() - without_scope.owned_symbol_bytes();
        with_guard.resize(with_bytes as u64).unwrap();
        without_guard.resize(without_bytes as u64).unwrap();
        assert_eq!(
            authority.snapshot().used_bytes,
            (with_bytes + without_bytes) as u64
        );
        drop(with_scope);
        drop(without_scope);
        drop(with_guard);
        drop(without_guard);
        assert_eq!(authority.snapshot().used_bytes, 0);
    }

    #[test]
    fn sparse_decoded_and_lazy_facts_keep_only_their_actual_same_operation_charge() {
        let (store, published) = published_fixture();
        let decoded = store.decode_active(&scope()).unwrap().unwrap();
        let file = &decoded.files[0];
        let authority = memory(64 * 1024 * 1024);
        let operation = authority.begin_operation();
        let view = SparseFileV1::decoded(
            &file.authority.logical_path,
            file.extraction.language.as_str(),
            Arc::clone(file),
            operation.clone(),
        )
        .unwrap();
        let loaded = view.loaded().unwrap();
        let expected = loaded.retained_bytes() - loaded.owned_symbol_bytes();
        assert_eq!(authority.snapshot().used_bytes, expected as u64);
        drop(view);
        assert_eq!(authority.snapshot().used_bytes, 0);
        let denied = memory(1);
        assert!(matches!(
            SparseFileV1::decoded(
                &file.authority.logical_path,
                file.extraction.language.as_str(),
                Arc::clone(file),
                denied.begin_operation()
            ),
            Err(CodeIndexProductionErrorV1::Publication(_))
        ));
        assert_eq!(denied.snapshot().used_bytes, 0);

        let parent = SealedParentGenerationV1::open(published.sealed_generation().clone()).unwrap();
        let descriptor = &parent.generation().file_segments[0];
        let row = &parent.snapshot().files[descriptor.file_key as usize];
        let view = SparseFileV1::carried(
            &row.logical_path,
            row.language.as_ref().unwrap().as_str(),
            SparseFileSourceV1 {
                parent: &parent,
                descriptor,
                file_occurrence_id: &row.file_occurrence_id,
                generation_id: &parent.manifest().generation_id,
                snapshot_digest: &parent.manifest().snapshot_digest,
                construction_operation: operation,
            },
        );
        assert_eq!(authority.snapshot().used_bytes, 0);
        view.binding().unwrap();
        assert_eq!(
            authority.snapshot().used_bytes,
            view.loaded().unwrap().retained_bytes() as u64
        );
        drop(view);
        assert_eq!(authority.snapshot().used_bytes, 0);
        let denied_view = SparseFileV1::carried(
            &row.logical_path,
            row.language.as_ref().unwrap().as_str(),
            SparseFileSourceV1 {
                parent: &parent,
                descriptor,
                file_occurrence_id: &row.file_occurrence_id,
                generation_id: &parent.manifest().generation_id,
                snapshot_digest: &parent.manifest().snapshot_digest,
                construction_operation: denied.begin_operation(),
            },
        );
        assert!(matches!(
            denied_view.binding(),
            Err(CodeIndexProductionErrorV1::Publication(_))
        ));
        assert!(denied_view.loaded().is_none());
        assert_eq!(denied.snapshot().used_bytes, 0);
    }

    #[test]
    fn cold_fact_vector_admission_precedes_canonical_segment_reads() {
        let (_store, published) = published_fixture();
        let parent = SealedParentGenerationV1::open(published.sealed_generation().clone()).unwrap();
        let canonical = published.sealed_generation().segment_reader();
        let reads = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&reads);
        let reader: Arc<SealedSegmentReaderV1> = Arc::new(move |segment, bytes| {
            observed.fetch_add(1, Ordering::Relaxed);
            canonical(segment, bytes)
        });
        let authority = memory(1);
        let mut guard = ConstructionMemoryV1::new(authority.begin_operation()).unwrap();
        let result = resolve_staged_generation(
            parent.manifest(),
            parent.snapshot(),
            &parent.generation().file_segments,
            reader,
            &UninterruptibleCodeIndexControlV1,
            &mut |bytes| guard.resize(bytes),
            &mut |_| {
                Err(CodeIndexProductionErrorV1::Contract(
                    "unexpected publication".to_owned(),
                ))
            },
        );
        assert!(matches!(
            result,
            Err(CodeIndexProductionErrorV1::Publication(_))
        ));
        assert_eq!(
            reads.load(Ordering::Relaxed),
            0,
            "outer fact/order allocation admission must precede file decode and clones"
        );
        assert_eq!(authority.snapshot().used_bytes, 0);
    }
}
