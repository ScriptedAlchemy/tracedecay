//! What building a generation's code graph from its sealed segments holds.
//!
//! [`CodeIndexPublishedGenerationV1::graph_build_bound`] sizes each state the
//! streaming build keeps, from the generation it will project, so admission
//! can charge the build before it runs. Structures are sized the way
//! [`super::resident_bytes`] sizes a decode; graph rows are sized by emitting
//! a sample of files through the real row emitter and scaling each row kind
//! by its exact count.

use std::collections::{BTreeMap, HashSet};
use std::mem::size_of;
use std::sync::Arc;

use tracedecay_domain::{
    CodeSearchChunkV1, SanitizedCodeFileV1, SnapshotFileDispositionV1, SymbolOccurrenceId,
};
use tracedecay_graph_db::{
    GRAPH_ROW_SPILL_RUN_BYTES, GraphEntityId, GraphSpillRowFootprint, graph_stable_identity,
};

use super::partitioned_codec::FILE_WINDOW_FILES_PER_WORKER_V1;
use super::resident_bytes::{
    ARC_HEADER_BYTES, btree_bytes, chunk_bytes, edge_heap_bytes, file_bytes, import_heap_bytes,
    page_identity_bytes, symbol_bytes, unresolved_heap_bytes, vec_bytes,
};
use super::{
    CodeIndexProductionErrorV1, CodeIndexPublishedGenerationV1, FileGenerationArtifactsV1,
};
use crate::chunks::CodeIndexUnresolvedReferenceV1;
use crate::graph_projection::{
    CodeGraphRowSampleV1, CodeGraphSampleFileV1, CodeGraphSymbolBindingV1, sample_code_graph_rows,
};

/// Files whose rows are emitted to size every row kind.
const GRAPH_ROW_SAMPLE_FILES_V1: usize = 256;

/// What the sealed graph build of one generation holds at its peak, by the
/// state holding it. Each field is resident bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CodeGraphBuildBoundV1 {
    /// The segment buffers and the largest window of decoded pages.
    pub decode_window_bytes: u64,
    /// Every file reduced to what cross-file resolution reads.
    pub resolution_file_bytes: u64,
    /// Every file's chunk-derived symbol bindings.
    pub binding_bytes: u64,
    /// The bound symbol set edges are retained from.
    pub bound_set_bytes: u64,
    /// Cross-file edges and unresolved calls, listed and grouped by source.
    pub derived_bytes: u64,
    /// At the emission peak: batches not yet emitted.
    pub emit_batch_bytes: u64,
    /// At the emission peak: rows buffered in the spill and the entity
    /// identities it keeps.
    pub emit_spill_bytes: u64,
    /// At the emission peak: the batch being emitted as rows.
    pub emit_window_bytes: u64,
    /// The compact store build over the merged rows.
    pub store_bytes: u64,
}

impl CodeGraphBuildBoundV1 {
    /// Resolution holds every reduced file with its bindings and bound set,
    /// plus the decode window while windows are read, then the derived state.
    #[must_use]
    pub fn resolve_bytes(&self) -> u64 {
        self.resolution_file_bytes
            .saturating_add(self.binding_bytes)
            .saturating_add(self.bound_set_bytes)
            .saturating_add(self.decode_window_bytes.max(self.derived_bytes))
    }

    /// Emission holds the bound set and derived state beside the batches,
    /// spill, and emitted rows at their joint peak.
    #[must_use]
    pub fn emit_bytes(&self) -> u64 {
        self.bound_set_bytes
            .saturating_add(self.derived_bytes)
            .saturating_add(self.emit_batch_bytes)
            .saturating_add(self.emit_spill_bytes)
            .saturating_add(self.emit_window_bytes)
    }

    /// The build's peak: the largest of its three stages.
    #[must_use]
    pub fn peak_bytes(&self) -> u64 {
        self.resolve_bytes()
            .max(self.emit_bytes())
            .max(self.store_bytes)
    }
}

fn bytes(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// A `HashSet` of `entries` values of `slot` bytes: power-of-two buckets at
/// a 7/8 load, each with a control byte.
fn hash_set_bytes(entries: usize, slot: usize) -> usize {
    if entries == 0 {
        return 0;
    }
    entries
        .saturating_mul(8)
        .div_ceil(7)
        .next_power_of_two()
        .saturating_mul(slot.saturating_add(1))
}

/// A `Vec` grown by pushes holds up to twice its length.
fn grown_vec_bytes(entries: usize, slot: usize) -> usize {
    entries.saturating_mul(slot).saturating_mul(2)
}

/// One file's contribution to each stage of the build.
struct FileGraphCost<'a> {
    file: &'a FileGenerationArtifactsV1,
    snapshot: &'a SanitizedCodeFileV1,
    /// The page as its window decodes it, with its chunks and symbols.
    decoded_page: usize,
    reduced: usize,
    bindings: usize,
    batch: usize,
    bound_occurrences: usize,
    bound_heap: usize,
    binding_count: usize,
}

impl<'a> FileGraphCost<'a> {
    fn of(file: &'a FileGenerationArtifactsV1, snapshot: &'a SanitizedCodeFileV1) -> Self {
        let artifacts = &file.artifacts;
        let chunks = &artifacts.chunks.chunks;
        let symbols = &artifacts.symbols;
        let symbol_records = symbols
            .iter()
            .map(|symbol| symbol_bytes(symbol))
            .fold(0_usize, usize::saturating_add);
        let decoded_page = file_bytes(file)
            .saturating_add(
                chunks
                    .iter()
                    .map(|chunk| chunk_bytes(chunk))
                    .fold(0_usize, usize::saturating_add),
            )
            .saturating_add(symbol_records);
        let edges = vec_bytes(&artifacts.edges, artifacts.edges.len(), edge_heap_bytes);
        let imports = vec_bytes(
            &artifacts.imports,
            artifacts.imports.len(),
            import_heap_bytes,
        );
        let handles = symbols.len().saturating_mul(size_of::<Arc<()>>());
        let reduced = ARC_HEADER_BYTES
            .saturating_add(size_of::<FileGenerationArtifactsV1>())
            .saturating_add(page_identity_bytes(file))
            .saturating_add(edges)
            .saturating_add(imports)
            .saturating_add(vec_bytes(
                &artifacts.unresolved_references,
                artifacts.unresolved_references.len(),
                unresolved_heap_bytes,
            ))
            .saturating_add(handles)
            .saturating_add(symbol_records);
        let (binding_count, binding_heap) = binding_heap(chunks, snapshot);
        let bindings = btree_bytes(
            binding_count,
            size_of::<(SymbolOccurrenceId, CodeGraphSymbolBindingV1)>(),
        )
        .saturating_add(binding_heap);
        let batch = edges
            .saturating_add(imports)
            .saturating_add(handles)
            .saturating_add(symbol_records)
            .saturating_add(bindings)
            .saturating_add(size_of::<&SanitizedCodeFileV1>());
        let bound = chunks
            .iter()
            .filter_map(|chunk| chunk.anchor.symbol_occurrence_id.as_ref())
            .chain(symbols.iter().map(|symbol| &symbol.occurrence))
            .collect::<HashSet<_>>();
        Self {
            file,
            snapshot,
            decoded_page,
            reduced,
            bindings,
            batch,
            bound_occurrences: bound.len(),
            bound_heap: bound
                .iter()
                .map(|occurrence| occurrence.as_str().len())
                .fold(0_usize, usize::saturating_add),
            binding_count,
        }
    }

    fn sample(&self) -> CodeGraphSampleFileV1<'a> {
        let artifacts = &self.file.artifacts;
        CodeGraphSampleFileV1 {
            snapshot: self.snapshot,
            logical_path: self.file.authority.logical_path.as_str(),
            chunks: &artifacts.chunks.chunks,
            symbols: &artifacts.symbols,
            imports: &artifacts.imports,
            edges: &artifacts.edges,
            unresolved: &artifacts.unresolved_references,
        }
    }

    /// The spill footprint of this file's entity and relation rows.
    fn rows(
        &self,
        sample: &CodeGraphRowSampleV1,
    ) -> (GraphSpillRowFootprint, GraphSpillRowFootprint) {
        let artifacts = &self.file.artifacts;
        let imports = artifacts.imports.len();
        let entities = [
            sample.file_entities.scaled(1),
            sample.import_entities.scaled(imports),
            sample.symbol_entities.scaled(self.bound_occurrences),
        ];
        let relations = [
            sample.import_relations.scaled(imports),
            sample.binding_relations.scaled(self.binding_count),
            sample.edge_relations.scaled(artifacts.edges.len()),
        ];
        (sum(&entities), sum(&relations))
    }

    fn entity_count(&self) -> usize {
        1_usize
            .saturating_add(self.file.artifacts.imports.len())
            .saturating_add(self.bound_occurrences)
    }
}

fn sum(footprints: &[GraphSpillRowFootprint]) -> GraphSpillRowFootprint {
    footprints
        .iter()
        .fold(GraphSpillRowFootprint::default(), |total, row| {
            GraphSpillRowFootprint {
                buffered: total.buffered.saturating_add(row.buffered),
                resident: total.resident.saturating_add(row.resident),
            }
        })
}

/// Distinct symbols a file's chunks bind and the heap of those bindings.
fn binding_heap(
    chunks: &[Arc<CodeSearchChunkV1>],
    snapshot: &SanitizedCodeFileV1,
) -> (usize, usize) {
    let mut seen = HashSet::new();
    let mut heap = 0_usize;
    for chunk in chunks {
        let Some(occurrence) = chunk.anchor.symbol_occurrence_id.as_ref() else {
            continue;
        };
        if seen.insert(occurrence) {
            heap = heap
                .saturating_add(occurrence.as_str().len())
                .saturating_add(snapshot.file_occurrence_id.as_str().len())
                .saturating_add(snapshot.logical_path.len())
                .saturating_add(chunk.id.as_str().len())
                .saturating_add(chunk.language_descriptor_revision.as_str().len());
        }
    }
    (seen.len(), heap)
}

/// Two spill buffers, one per row kind, each written out as a run once its
/// buffered bytes reach [`GRAPH_ROW_SPILL_RUN_BYTES`].
#[derive(Default)]
struct SpillBuffers {
    entities: GraphSpillRowFootprint,
    relations: GraphSpillRowFootprint,
    identities: usize,
}

impl SpillBuffers {
    fn resident(&self, identity_bytes: usize) -> usize {
        self.entities
            .resident
            .saturating_add(self.relations.resident)
            .saturating_add(self.identities.saturating_mul(identity_bytes))
    }

    /// Buffers one batch's rows and returns the resident bytes just before
    /// any run it fills is written.
    fn push(
        &mut self,
        entities: GraphSpillRowFootprint,
        relations: GraphSpillRowFootprint,
        entity_rows: usize,
        identity_bytes: usize,
    ) -> usize {
        self.identities = self.identities.saturating_add(entity_rows);
        for (buffer, rows) in [
            (&mut self.entities, entities),
            (&mut self.relations, relations),
        ] {
            buffer.buffered = buffer.buffered.saturating_add(rows.buffered);
            buffer.resident = buffer.resident.saturating_add(rows.resident);
        }
        let peak = self.resident(identity_bytes);
        for buffer in [&mut self.entities, &mut self.relations] {
            if buffer.buffered >= GRAPH_ROW_SPILL_RUN_BYTES {
                *buffer = GraphSpillRowFootprint::default();
            }
        }
        peak
    }
}

impl CodeIndexPublishedGenerationV1 {
    /// What building this generation's code graph from its sealed segments
    /// holds at its peak, sized before the build runs.
    #[hotpath::measure(label = "code_index.graph_build_bound")]
    pub fn graph_build_bound(&self) -> Result<CodeGraphBuildBoundV1, CodeIndexProductionErrorV1> {
        let snapshot_files = self
            .snapshot
            .files
            .iter()
            .map(|file| (&file.file_occurrence_id, file))
            .collect::<BTreeMap<_, _>>();
        let costs = self
            .files
            .iter()
            .map(|file| {
                snapshot_files
                    .get(&file.extraction.file_occurrence_id)
                    .map(|snapshot| FileGraphCost::of(file, snapshot))
                    .ok_or_else(|| {
                        CodeIndexProductionErrorV1::Contract(
                            "generation file is outside its snapshot".to_owned(),
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let step = costs.len().div_ceil(GRAPH_ROW_SAMPLE_FILES_V1).max(1);
        let sampled = costs
            .iter()
            .step_by(step)
            .map(FileGraphCost::sample)
            .collect::<Vec<_>>();
        let sample = sample_code_graph_rows(&self.manifest.generation_id, &sampled)
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;

        let window_files = crate::parallelism::indexing_workers()
            .max(1)
            .saturating_mul(FILE_WINDOW_FILES_PER_WORKER_V1);
        // Each segment buffer keeps the capacity of the largest segment it
        // ever held, and one window of pages is decoded at a time.
        let mut buffers = vec![0_usize; window_files];
        let mut largest_window = 0_usize;
        for window in costs.chunks(window_files) {
            let mut decoded = 0_usize;
            for (slot, cost) in window.iter().enumerate() {
                buffers[slot] = buffers[slot].max(cost.decoded_page);
                decoded = decoded.saturating_add(cost.decoded_page);
            }
            largest_window = largest_window.max(decoded);
        }
        let decode_window = largest_window.saturating_add(buffers.iter().sum::<usize>());

        let total = |value: fn(&FileGraphCost<'_>) -> usize| {
            costs.iter().map(value).fold(0_usize, usize::saturating_add)
        };
        let resolution = total(|cost| cost.reduced);
        let bindings = total(|cost| cost.bindings);
        let bound_occurrences = total(|cost| cost.bound_occurrences);
        let bound_set = hash_set_bytes(bound_occurrences, size_of::<SymbolOccurrenceId>())
            .saturating_add(total(|cost| cost.bound_heap));

        let file_edges = total(|cost| cost.file.artifacts.edges.len());
        let cross_file_edges = self.edges.len().saturating_sub(file_edges);
        let edge_bytes = if self.edges.is_empty() {
            0
        } else {
            vec_bytes(&self.edges, self.edges.len(), edge_heap_bytes) / self.edges.len()
        };
        let unresolved = self
            .files
            .iter()
            .flat_map(|file| file.artifacts.unresolved_references.iter())
            .map(|reference| {
                size_of::<CodeIndexUnresolvedReferenceV1>()
                    .saturating_add(unresolved_heap_bytes(reference))
            })
            .fold(0_usize, usize::saturating_add);
        let derived = cross_file_edges
            .saturating_mul(edge_bytes)
            .saturating_add(unresolved.saturating_mul(2));

        // The spill keeps every entity identity in a vector grown by pushes.
        let identity_bytes = size_of::<GraphEntityId>()
            .saturating_mul(2)
            .saturating_add(graph_stable_identity("symbol", "").len());
        let mut canonical_rows = 0_usize;
        let mut remaining = total(|cost| cost.batch);
        let mut spill = SpillBuffers::default();
        let mut emit = (0_usize, 0_usize, 0_usize);
        let mut record = |batch: usize, spilled: usize, window: usize| {
            if batch.saturating_add(spilled).saturating_add(window)
                > emit.0.saturating_add(emit.1).saturating_add(emit.2)
            {
                emit = (batch, spilled, window);
            }
        };
        for window in costs.chunks(window_files) {
            let (mut entities, mut relations) = (
                GraphSpillRowFootprint::default(),
                GraphSpillRowFootprint::default(),
            );
            let mut entity_rows = 0_usize;
            let mut batch = 0_usize;
            for cost in window {
                let (entity, relation) = cost.rows(&sample);
                entities = sum(&[entities, entity]);
                relations = sum(&[relations, relation]);
                entity_rows = entity_rows.saturating_add(cost.entity_count());
                batch = batch.saturating_add(cost.batch);
            }
            canonical_rows = canonical_rows
                .saturating_add(entities.buffered)
                .saturating_add(relations.buffered);
            let rows = entities.resident.saturating_add(relations.resident);
            record(remaining, spill.resident(identity_bytes), rows);
            remaining = remaining.saturating_sub(batch);
            let spilled = spill.push(entities, relations, entity_rows, identity_bytes);
            record(remaining, spilled, rows);
        }
        let unsegmented = self
            .snapshot
            .files
            .iter()
            .filter(|file| file.disposition != SnapshotFileDispositionV1::Present)
            .count();
        let cross_relations = sample.edge_relations.scaled(cross_file_edges);
        let cross_entities = sample.file_entities.scaled(unsegmented);
        canonical_rows = canonical_rows
            .saturating_add(cross_entities.buffered)
            .saturating_add(cross_relations.buffered);
        let spilled = spill.push(cross_entities, cross_relations, unsegmented, identity_bytes);
        record(
            0,
            spilled,
            cross_entities
                .resident
                .saturating_add(cross_relations.resident),
        );

        let entity_rows = total(|cost| cost.entity_count()).saturating_add(unsegmented);
        let relation_rows = total(|cost| {
            cost.file
                .artifacts
                .imports
                .len()
                .saturating_add(cost.binding_count)
                .saturating_add(cost.file.artifacts.edges.len())
        })
        .saturating_add(cross_file_edges);
        let store = compact_store_bytes(
            entity_rows,
            relation_rows,
            identity_bytes,
            canonical_rows,
            &sample,
            &costs,
        );

        Ok(CodeGraphBuildBoundV1 {
            decode_window_bytes: bytes(decode_window),
            resolution_file_bytes: bytes(resolution),
            binding_bytes: bytes(bindings),
            bound_set_bytes: bytes(bound_set),
            derived_bytes: bytes(derived),
            emit_batch_bytes: bytes(emit.0),
            emit_spill_bytes: bytes(emit.1),
            emit_window_bytes: bytes(emit.2),
            store_bytes: bytes(store),
        })
    }
}

/// The compact store build: the merged entity identities its relations
/// resolve endpoints through, one node per entity and per relation locator,
/// one edge per relation, and the largest column encoded at once. The reopen
/// that proves the written container loads it whole, since the compact store
/// is heap resident, about the rows' canonical bytes, while the allocator
/// still holds the write's working set.
fn compact_store_bytes(
    entity_rows: usize,
    relation_rows: usize,
    identity_bytes: usize,
    canonical_rows: usize,
    sample: &CodeGraphRowSampleV1,
    costs: &[FileGraphCost<'_>],
) -> usize {
    let nodes = entity_rows.saturating_add(relation_rows);
    let node_topology = hash_set_bytes(nodes, size_of::<(u64, (usize, u32))>())
        .saturating_add(grown_vec_bytes(nodes, size_of::<u64>()));
    let edge_topology = hash_set_bytes(relation_rows, size_of::<u64>())
        .saturating_add(grown_vec_bytes(relation_rows, size_of::<(u64, u32, u32)>()));
    let symbol_rows = costs
        .iter()
        .map(|cost| cost.bound_occurrences)
        .fold(0_usize, usize::saturating_add);
    let largest_column = sample.symbol_entities.scaled(symbol_rows).buffered;
    entity_rows
        .saturating_mul(identity_bytes)
        .saturating_add(node_topology)
        .saturating_add(edge_topology)
        .saturating_add(largest_column)
        .saturating_add(canonical_rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spill_buffers_write_a_run_once_a_kind_reaches_the_run_size() {
        let mut spill = SpillBuffers::default();
        let half = GraphSpillRowFootprint {
            buffered: GRAPH_ROW_SPILL_RUN_BYTES / 2,
            resident: GRAPH_ROW_SPILL_RUN_BYTES / 2 + 100,
        };
        let none = GraphSpillRowFootprint::default();
        assert_eq!(
            spill.push(half, none, 1, 10),
            GRAPH_ROW_SPILL_RUN_BYTES / 2 + 110
        );
        assert_eq!(
            spill.push(half, none, 1, 10),
            GRAPH_ROW_SPILL_RUN_BYTES + 220
        );
        assert_eq!(spill.resident(10), 20);
    }
}
