//! What building a generation's code graph from its sealed segments holds.
//!
//! [`CodeIndexPublishedGenerationV1::graph_build_bound`] sizes each state the
//! page-streaming build keeps, from the generation it will project, so
//! admission can charge the build before it runs. Graph rows are sized by
//! emitting a sample of files through the real row emitter and scaling each
//! row kind by its exact count.

use std::collections::{BTreeMap, HashSet};
use std::mem::size_of;
use std::sync::Arc;

use tracedecay_domain::{CodeSearchChunkV1, SanitizedCodeFileV1, SnapshotFileDispositionV1};
use tracedecay_graph_db::{
    GRAPH_ROW_SPILL_RUN_BYTES, GraphEntityId, GraphSpillRowFootprint, graph_stable_identity,
};

use super::partitioned_codec::FILE_WINDOW_FILES_PER_WORKER_V1;
use super::resident_bytes::{chunk_bytes, file_bytes, symbol_bytes};
use super::{
    CodeGraphPageDescriptorV1, CodeIndexProductionErrorV1, CodeIndexPublishedGenerationV1,
    FileGenerationArtifactsV1,
};
use crate::graph_projection::{
    CodeGraphRowSampleV1, CodeGraphSampleFileV1, sample_code_graph_rows,
};

/// Files whose rows are emitted to size every row kind.
const GRAPH_ROW_SAMPLE_FILES_V1: usize = 256;

/// What the sealed graph build of one generation holds at its peak, by the
/// state holding it. Each field is resident bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CodeGraphBuildBoundV1 {
    /// The segment buffers and the largest window of decoded pages.
    pub decode_window_bytes: u64,
    /// Retired graph-time resolution state; always zero for page builds.
    pub resolution_file_bytes: u64,
    /// Retired graph-time binding state; always zero for page builds.
    pub binding_bytes: u64,
    /// Retired graph-time bound set; always zero for page builds.
    pub bound_set_bytes: u64,
    /// Retired graph-time derived state; always zero for page builds.
    pub derived_bytes: u64,
    /// Page builds do not retain pending emission batches; always zero.
    pub emit_batch_bytes: u64,
    /// At the emission peak: rows buffered in the spill and the entity
    /// identities it keeps.
    pub emit_spill_bytes: u64,
    /// At the emission peak: the batch being emitted as rows.
    pub emit_window_bytes: u64,
    /// The identities and row index retained while spill runs merge.
    pub store_bytes: u64,
}

impl CodeGraphBuildBoundV1 {
    /// Page acquisition holds one bounded decode window. The other terms are
    /// retained for the public receipt shape and are zero for page builds.
    #[must_use]
    pub fn resolve_bytes(&self) -> u64 {
        self.resolution_file_bytes
            .saturating_add(self.binding_bytes)
            .saturating_add(self.bound_set_bytes)
            .saturating_add(self.decode_window_bytes.max(self.derived_bytes))
    }

    /// Emission holds the spill and the page currently emitted as rows.
    #[must_use]
    pub fn emit_bytes(&self) -> u64 {
        self.bound_set_bytes
            .saturating_add(self.derived_bytes)
            .saturating_add(self.emit_batch_bytes)
            .saturating_add(self.emit_spill_bytes)
            .saturating_add(self.emit_window_bytes)
            .saturating_add(self.decode_window_bytes)
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

const SIZING_NAMESPACE_BYTES: u64 = 6;

#[derive(Clone, Copy, Default)]
struct PageBuildCostV1 {
    decode: u64,
    entity_buffered: u64,
    entity_resident: u64,
    relation_buffered: u64,
    relation_resident: u64,
    identities: u64,
    entities: u64,
    relations: u64,
    external_endpoints: u64,
    max_entity_buffered: u64,
    max_entity_resident: u64,
}

impl PageBuildCostV1 {
    fn of(page: &CodeGraphPageDescriptorV1, namespace_bytes: u64) -> Self {
        let footprint = &page.build_footprint;
        let projection_growth = namespace_bytes
            .saturating_sub(SIZING_NAMESPACE_BYTES)
            .saturating_mul(2)
            .saturating_mul(footprint.relation_count);
        Self {
            decode: footprint.decode_bytes,
            entity_buffered: footprint.entity_spill_buffered,
            entity_resident: footprint.entity_spill_resident,
            relation_buffered: footprint
                .relation_spill_buffered
                .saturating_add(projection_growth),
            relation_resident: footprint
                .relation_spill_resident
                .saturating_add(projection_growth),
            identities: footprint.identity_bytes,
            entities: footprint.entity_count,
            relations: footprint.relation_count,
            external_endpoints: footprint.external_endpoint_count,
            max_entity_buffered: footprint.max_entity_spill_buffered,
            max_entity_resident: footprint.max_entity_spill_resident,
        }
    }
}

#[derive(Default)]
struct LayeredSpillBuffersV1 {
    entity_buffered: u64,
    entity_resident: u64,
    relation_buffered: u64,
    relation_resident: u64,
    identities: u64,
}

impl LayeredSpillBuffersV1 {
    fn resident(&self) -> u64 {
        self.entity_resident
            .saturating_add(self.relation_resident)
            .saturating_add(self.identities)
    }

    fn push(&mut self, cost: PageBuildCostV1) -> u64 {
        self.entity_buffered = self.entity_buffered.saturating_add(cost.entity_buffered);
        self.entity_resident = self.entity_resident.saturating_add(cost.entity_resident);
        self.relation_buffered = self
            .relation_buffered
            .saturating_add(cost.relation_buffered);
        self.relation_resident = self
            .relation_resident
            .saturating_add(cost.relation_resident);
        self.identities = self.identities.saturating_add(cost.identities);
        let peak = self.resident();
        if self.entity_buffered >= GRAPH_ROW_SPILL_RUN_BYTES as u64 {
            self.entity_buffered = 0;
            self.entity_resident = 0;
        }
        if self.relation_buffered >= GRAPH_ROW_SPILL_RUN_BYTES as u64 {
            self.relation_buffered = 0;
            self.relation_resident = 0;
        }
        peak
    }
}

pub(crate) fn layered_page_graph_build_bound<'a>(
    changed: impl IntoIterator<
        Item = (
            Option<&'a CodeGraphPageDescriptorV1>,
            Option<&'a CodeGraphPageDescriptorV1>,
        ),
    >,
    base_pages: &[CodeGraphPageDescriptorV1],
    namespace_bytes: usize,
) -> CodeGraphBuildBoundV1 {
    let namespace_bytes = u64::try_from(namespace_bytes).unwrap_or(u64::MAX);
    let mut spill = LayeredSpillBuffersV1::default();
    let mut decode_window = 0_u64;
    let mut emit_peak = 0_u64;
    let mut emit_window = 0_u64;
    let mut hidden_bytes = 0_u64;
    let mut endpoint_slots = 0_u64;
    let mut delta_entities = 0_u64;
    let mut delta_relations = 0_u64;
    let max_base_entity = base_pages
        .iter()
        .map(|page| PageBuildCostV1::of(page, namespace_bytes))
        .max_by_key(|cost| cost.max_entity_resident)
        .unwrap_or_default();
    for (child, base) in changed {
        let child = child.map(|page| PageBuildCostV1::of(page, namespace_bytes));
        let base = base.map(|page| PageBuildCostV1::of(page, namespace_bytes));
        decode_window = decode_window.max(
            child
                .map_or(0, |cost| cost.decode)
                .saturating_add(base.map_or(0, |cost| cost.decode)),
        );
        if let Some(base) = base {
            hidden_bytes = hidden_bytes
                .saturating_add(base.identities)
                .saturating_add(base.relation_buffered);
        }
        if let Some(child) = child {
            let page_window = child
                .entity_resident
                .saturating_add(child.relation_resident);
            emit_window = emit_window.max(page_window);
            emit_peak = emit_peak.max(
                spill
                    .resident()
                    .saturating_add(hidden_bytes)
                    .saturating_add(page_window),
            );
            emit_peak = emit_peak.max(
                spill
                    .push(child)
                    .saturating_add(hidden_bytes)
                    .saturating_add(page_window),
            );
            endpoint_slots = endpoint_slots.saturating_add(child.external_endpoints);
            delta_entities = delta_entities.saturating_add(child.entities);
            delta_relations = delta_relations.saturating_add(child.relations);
        }
    }
    let endpoint_resident = endpoint_slots.saturating_mul(max_base_entity.max_entity_resident);
    let endpoint_buffered = endpoint_slots.saturating_mul(max_base_entity.max_entity_buffered);
    let endpoint_identities = endpoint_slots.saturating_mul(
        max_base_entity
            .identities
            .checked_div(max_base_entity.entities.max(1))
            .unwrap_or(0),
    );
    let endpoint_cost = PageBuildCostV1 {
        entity_buffered: endpoint_buffered,
        entity_resident: endpoint_resident,
        identities: endpoint_identities,
        entities: endpoint_slots,
        ..PageBuildCostV1::default()
    };
    emit_window = emit_window.max(max_base_entity.max_entity_resident);
    emit_peak = emit_peak.max(
        spill
            .push(endpoint_cost)
            .saturating_add(hidden_bytes)
            .saturating_add(max_base_entity.max_entity_resident),
    );
    delta_entities = delta_entities.saturating_add(endpoint_slots);
    let identity_store = spill.identities.saturating_add(hidden_bytes);
    let row_index_records = delta_entities
        .saturating_add(delta_relations)
        .saturating_mul(size_of::<([u8; 16], ([u64; 4], u32, u32))>() as u64)
        .saturating_mul(2);
    CodeGraphBuildBoundV1 {
        decode_window_bytes: decode_window,
        resolution_file_bytes: 0,
        binding_bytes: 0,
        bound_set_bytes: 0,
        derived_bytes: 0,
        emit_batch_bytes: 0,
        emit_spill_bytes: emit_peak,
        emit_window_bytes: emit_window,
        store_bytes: identity_store.saturating_add(row_index_records),
    }
}

pub(crate) fn sealed_page_graph_build_bound(
    pages: &[CodeGraphPageDescriptorV1],
    namespace_bytes: usize,
) -> CodeGraphBuildBoundV1 {
    layered_page_graph_build_bound(
        pages.iter().map(|page| (Some(page), None)),
        &[],
        namespace_bytes,
    )
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
    bound_occurrences: usize,
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
        let binding_count = binding_count(chunks);
        let bound = chunks
            .iter()
            .filter_map(|chunk| chunk.anchor.symbol_occurrence_id.as_ref())
            .chain(symbols.iter().map(|symbol| &symbol.occurrence))
            .collect::<HashSet<_>>();
        Self {
            file,
            snapshot,
            decoded_page,
            bound_occurrences: bound.len(),
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

/// Distinct symbols a file's chunks bind.
fn binding_count(chunks: &[Arc<CodeSearchChunkV1>]) -> usize {
    let mut seen = HashSet::new();
    for chunk in chunks {
        let Some(occurrence) = chunk.anchor.symbol_occurrence_id.as_ref() else {
            continue;
        };
        seen.insert(occurrence);
    }
    seen.len()
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
    #[tracing::instrument(name = "code_index.graph_build_bound", level = "trace", skip_all)]
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
        let file_edges = total(|cost| cost.file.artifacts.edges.len());
        let cross_file_edges = self.edges.len().saturating_sub(file_edges);

        // The spill keeps every entity identity in a vector grown by pushes.
        let identity_bytes = size_of::<GraphEntityId>()
            .saturating_mul(2)
            .saturating_add(graph_stable_identity("symbol", "").len());
        let mut spill = SpillBuffers::default();
        let mut emit = (0_usize, 0_usize);
        let mut record = |spilled: usize, window: usize| {
            if spilled.saturating_add(window) > emit.0.saturating_add(emit.1) {
                emit = (spilled, window);
            }
        };
        for window in costs.chunks(window_files) {
            let (mut entities, mut relations) = (
                GraphSpillRowFootprint::default(),
                GraphSpillRowFootprint::default(),
            );
            let mut entity_rows = 0_usize;
            for cost in window {
                let (entity, relation) = cost.rows(&sample);
                entities = sum(&[entities, entity]);
                relations = sum(&[relations, relation]);
                entity_rows = entity_rows.saturating_add(cost.entity_count());
            }
            let rows = entities.resident.saturating_add(relations.resident);
            record(spill.resident(identity_bytes), rows);
            let spilled = spill.push(entities, relations, entity_rows, identity_bytes);
            record(spilled, rows);
        }
        let unsegmented = self
            .snapshot
            .files
            .iter()
            .filter(|file| file.disposition != SnapshotFileDispositionV1::Present)
            .count();
        let cross_relations = sample.edge_relations.scaled(cross_file_edges);
        let cross_entities = sample.file_entities.scaled(unsegmented);
        let spilled = spill.push(cross_entities, cross_relations, unsegmented, identity_bytes);
        record(
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
        let store = spill_finish_bytes(entity_rows, relation_rows, identity_bytes);

        Ok(CodeGraphBuildBoundV1 {
            decode_window_bytes: bytes(decode_window),
            resolution_file_bytes: 0,
            binding_bytes: 0,
            bound_set_bytes: 0,
            derived_bytes: 0,
            emit_batch_bytes: 0,
            emit_spill_bytes: bytes(emit.0),
            emit_window_bytes: bytes(emit.1),
            store_bytes: bytes(store),
        })
    }
}

/// Finishing the spill retains sorted entity identities and builds one
/// fixed-width digest-index record per row. Canonical row bytes remain in
/// spill files and pass through bounded I/O buffers.
fn spill_finish_bytes(entity_rows: usize, relation_rows: usize, identity_bytes: usize) -> usize {
    type RowIndexRecord = ([u8; 16], ([u64; 4], u32, u32));

    entity_rows
        .saturating_mul(identity_bytes)
        .saturating_add(grown_vec_bytes(entity_rows, size_of::<RowIndexRecord>()))
        .saturating_add(grown_vec_bytes(relation_rows, size_of::<RowIndexRecord>()))
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::{FileOccurrenceId, ManifestDigest};

    use super::super::CodeGraphPageBuildFootprintV1;
    use super::*;

    fn page(
        file_key: u32,
        build_footprint: CodeGraphPageBuildFootprintV1,
    ) -> CodeGraphPageDescriptorV1 {
        CodeGraphPageDescriptorV1 {
            file_key,
            file_occurrence_id: FileOccurrenceId::new(format!("file.fixture.{file_key}"))
                .expect("file occurrence"),
            logical_path: format!("src/file-{file_key}.rs"),
            page_digest: ManifestDigest::new(format!("sha256:{}", "a".repeat(64)))
                .expect("page digest"),
            size_bytes: 1,
            build_footprint,
        }
    }

    fn refresh_bound(
        child_external_endpoints: u64,
        unrelated_base_entity_bytes: u64,
    ) -> CodeGraphBuildBoundV1 {
        let changed_footprint = CodeGraphPageBuildFootprintV1 {
            decode_bytes: 2,
            entity_spill_buffered: 1_000,
            entity_spill_resident: 1_200,
            relation_spill_buffered: 100_000,
            relation_spill_resident: 120_000,
            identity_bytes: 4_000,
            entity_count: 10,
            relation_count: 1_000,
            external_endpoint_count: child_external_endpoints,
            max_entity_spill_buffered: 100,
            max_entity_spill_resident: 120,
        };
        let base_changed = page(0, changed_footprint.clone());
        let child_changed = page(0, changed_footprint);
        let unrelated = page(
            1,
            CodeGraphPageBuildFootprintV1 {
                decode_bytes: 2,
                identity_bytes: 400,
                entity_count: 1,
                max_entity_spill_buffered: unrelated_base_entity_bytes,
                max_entity_spill_resident: unrelated_base_entity_bytes,
                ..CodeGraphPageBuildFootprintV1::default()
            },
        );
        layered_page_graph_build_bound(
            [(Some(&child_changed), Some(&base_changed))],
            &[base_changed.clone(), unrelated],
            "fixture".len(),
        )
    }

    #[test]
    fn local_relations_charge_no_copies_of_an_unrelated_large_base_entity() {
        let small_unrelated = refresh_bound(0, 120);
        let large_unrelated = refresh_bound(0, 100 * 1024);

        assert_eq!(large_unrelated.peak_bytes(), small_unrelated.peak_bytes());
    }

    #[test]
    fn each_external_endpoint_charges_one_copy_of_the_largest_base_entity() {
        let local = refresh_bound(0, 100 * 1024);
        let three_external = refresh_bound(3, 100 * 1024);

        assert!(three_external.emit_spill_bytes >= 3 * (100 * 1024 + 400));
        assert!(three_external.peak_bytes() > local.peak_bytes());
    }

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
