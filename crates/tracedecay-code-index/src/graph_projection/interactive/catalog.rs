//! Bounded construction of the generation-pinned interactive catalog.

use std::borrow::Borrow;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::mem::{size_of, size_of_val};
use std::sync::Arc;

use serde::Deserialize;
use tracedecay_domain::{RelationEdgeKindV1, SanitizedCodeFileV1, SymbolOccurrenceId};
use tracedecay_graph_db::{
    GraphCancellation, GraphEntity, GraphEntityId, GraphGenerationId, GraphLayeredRowsV1,
    GraphProjectionIdentity, GraphProjectionReadRequest, GraphRelation, GraphRelationId,
    MAX_VERIFIED_GENERATION_RELATIONS, VerifiedGraphSnapshot,
};

use super::super::schema::{
    CatalogRecord, FILE_IMPORT_EDGE_KIND, FILE_LABEL, FILE_RECORD_PROPERTY, IMPORT_LABEL,
    IMPORT_RECORD_PROPERTY, SYMBOL_LABEL, SYMBOL_RECORD_PROPERTY, deserialize_catalog_property,
    file_entity_id, file_import_relation_id, has_label, import_entity_id,
};
use super::super::{
    CodeGraphProjectionError, EDGE_RECORD_PROPERTY, SymbolRecordV1, code_edge_kind_edge,
    symbol_entity_id, validate_symbol_record,
};
use super::build_charge::CatalogBuildChargeV1;
use super::models::{
    CatalogBuilder, CatalogLayerV1, CatalogSymbol, DeltaEntityV1, DeltaRelationV1,
    InteractiveCatalog, aggregate_allocation_bytes,
};
use crate::chunks::CodeIndexImportEvidenceV1;
use crate::production::resident_bytes::{btree_bytes, import_heap_bytes};

const CATALOG_SCAN_PAGE_ITEMS: usize = 1_024;

pub(super) fn entity_bytes(entity: &GraphEntity) -> usize {
    size_of::<GraphEntity>()
        .saturating_add(entity.identity.as_str().len())
        .saturating_add(
            entity
                .labels
                .iter()
                .map(|label| label.as_str().len() + 64)
                .sum::<usize>(),
        )
        .saturating_add(properties_bytes(&entity.properties))
}

pub(super) fn relation_bytes(relation: &GraphRelation) -> usize {
    size_of::<GraphRelation>()
        .saturating_add(relation.identity.as_str().len())
        .saturating_add(relation.from.as_str().len())
        .saturating_add(relation.to.as_str().len())
        .saturating_add(relation.kind.as_str().len())
        .saturating_add(properties_bytes(&relation.properties))
}

fn properties_bytes(
    properties: &BTreeMap<
        tracedecay_graph_db::GraphPropertyName,
        tracedecay_graph_db::GraphProperty,
    >,
) -> usize {
    properties.iter().fold(0_usize, |bytes, (name, property)| {
        bytes
            .saturating_add(name.as_str().len())
            .saturating_add(128)
            .saturating_add(match property {
                tracedecay_graph_db::GraphProperty::String(value) => value.capacity(),
                tracedecay_graph_db::GraphProperty::Bytes(value) => value.capacity(),
                _ => 0,
            })
    })
}

pub(super) fn build_interactive_catalog(
    snapshot: &VerifiedGraphSnapshot,
    projection: &GraphProjectionIdentity,
    projection_node_count: usize,
    cancellation: Arc<dyn GraphCancellation>,
    charge: &mut CatalogBuildChargeV1,
) -> Result<InteractiveCatalog, CodeGraphProjectionError> {
    let generation_bytes = snapshot.generation().as_str().len();
    charge.cover(generation_bytes)?;
    let rows = snapshot.layered_rows_with_allocation_admission(&mut |bytes| {
        charge.cover_read(generation_bytes.saturating_add(bytes))
    })?;
    let mut scan = CatalogScan::new(snapshot.generation().clone(), rows);
    // Every entity is recorded before the first relation, so each relation
    // resolves against complete files, symbols, and imports as it streams
    // past instead of being held until the scan ends.
    let read_page = |after_entity: Option<GraphEntityId>,
                     after_relation: Option<GraphRelationId>,
                     entities: bool,
                     held: usize,
                     charge: &mut CatalogBuildChargeV1| {
        let _span = tracing::trace_span!("code_graph.catalog.scan_page").entered();
        snapshot.read_projection_with_allocation_admission(
            GraphProjectionReadRequest {
                namespace: projection.namespace.clone(),
                projection: projection.projection.clone(),
                after_entity,
                after_relation,
                max_entities: if entities { CATALOG_SCAN_PAGE_ITEMS } else { 0 },
                max_relations: if entities { 0 } else { CATALOG_SCAN_PAGE_ITEMS },
                cancellation: Arc::clone(&cancellation),
            },
            &mut |bytes| charge.cover_read(held.saturating_add(bytes)),
        )
    };
    let mut after_entity = None;
    loop {
        check_cancelled(cancellation.as_ref())?;
        let page = read_page(after_entity, None, true, scan.held_bytes(), charge)?;
        let page_bytes = page
            .entities
            .iter()
            .map(entity_bytes)
            .fold(0_usize, usize::saturating_add);
        {
            let _span = tracing::trace_span!("code_graph.catalog.record_entities").entered();
            scan.record_entity_page(
                &page.entities,
                projection_node_count,
                cancellation.as_ref(),
                page_bytes,
                charge,
            )
        }?;
        charge.cover(scan.held_bytes())?;
        after_entity = page.next_entity;
        if after_entity.is_none() {
            break;
        }
    }
    let mut after_relation = None;
    loop {
        check_cancelled(cancellation.as_ref())?;
        let page = read_page(None, after_relation, false, scan.held_bytes(), charge)?;
        let page_bytes = page
            .relations
            .iter()
            .map(relation_bytes)
            .fold(0_usize, usize::saturating_add);
        {
            let _span = tracing::trace_span!("code_graph.catalog.record_relations").entered();
            scan.record_relation_page(&page.relations, cancellation.as_ref(), page_bytes, charge)
        }?;
        charge.cover(scan.held_bytes())?;
        after_relation = page.next_relation;
        if after_relation.is_none() {
            break;
        }
    }

    check_cancelled(cancellation.as_ref())?;
    charge.cover(
        scan.held_bytes()
            .saturating_add(scan.frozen_bytes())
            .saturating_add(aggregate_allocation_bytes(
                scan.catalog.files.values(),
                scan.catalog.symbols_by_logical_path.keys(),
                scan.dependency_edge_counts.keys(),
            )?),
    )?;
    {
        let _span = tracing::trace_span!("code_graph.catalog.finish").entered();
        scan.finish(projection_node_count)
    }
}

struct CatalogScan {
    generation: GraphGenerationId,
    /// What the scanned generation layers over, with the contribution of
    /// each delta row recorded as the scan passes it.
    layer: Option<LayerScan>,
    catalog: CatalogBuilder,
    /// Each import with whether its one file link has been seen.
    imports_by_entity: BTreeMap<GraphEntityId, (CodeIndexImportEvidenceV1, bool)>,
    degrees: SymbolDegreeCounts<GraphEntityId>,
    /// `calls`/`uses` edges between each ordered pair of distinct bound files.
    dependency_edge_counts: BTreeMap<(String, String), u64>,
    dependency_edges: u64,
    /// Heap the scan's own maps hold beyond their inline slots.
    heap: usize,
    scanned_entities: usize,
    scanned_relations: usize,
}

struct LayerScan {
    rows: GraphLayeredRowsV1,
    delta_entities: BTreeMap<GraphEntityId, DeltaEntityV1>,
    delta_relations: BTreeMap<GraphRelationId, DeltaRelationV1>,
    /// Heap the rows and recorded contributions hold.
    heap: usize,
}

impl LayerScan {
    fn new(rows: GraphLayeredRowsV1) -> Self {
        fn ids<T>(ids: &[T], len: impl Fn(&T) -> usize) -> usize {
            ids.iter()
                .fold(size_of_val(ids), |bytes, id| bytes.saturating_add(len(id)))
        }
        let entity = |id: &GraphEntityId| id.as_str().len();
        let relation = |id: &GraphRelationId| id.as_str().len();
        let heap = ids(&rows.hidden_entities, entity)
            .saturating_add(ids(&rows.delta_entities, entity))
            .saturating_add(ids(&rows.hidden_relations, relation))
            .saturating_add(ids(&rows.delta_relations, relation))
            .saturating_add(rows.base_generation.as_str().len());
        Self {
            rows,
            delta_entities: BTreeMap::new(),
            delta_relations: BTreeMap::new(),
            heap,
        }
    }

    fn record_entity(
        &mut self,
        entity: &GraphEntity,
        admit: &mut dyn FnMut(usize) -> Result<(), CodeGraphProjectionError>,
    ) -> Result<(), CodeGraphProjectionError> {
        if self
            .rows
            .delta_entities
            .binary_search(&entity.identity)
            .is_ok()
        {
            let delta = delta_entity(entity, admit)?;
            self.heap = self
                .heap
                .saturating_add(entity.identity.as_str().len())
                .saturating_add(delta_entity_heap_bytes(&delta));
            self.delta_entities.insert(entity.identity.clone(), delta);
        }
        Ok(())
    }

    fn record_relation(
        &mut self,
        relation: &GraphRelation,
        admit: &mut dyn FnMut(usize) -> Result<(), CodeGraphProjectionError>,
    ) -> Result<(), CodeGraphProjectionError> {
        if self
            .rows
            .delta_relations
            .binary_search(&relation.identity)
            .is_ok()
        {
            let delta = delta_relation(relation, admit)?;
            self.heap = self
                .heap
                .saturating_add(relation.identity.as_str().len())
                .saturating_add(delta_relation_heap_bytes(&delta));
            self.delta_relations
                .insert(relation.identity.clone(), delta);
        }
        Ok(())
    }

    fn held_bytes(&self) -> usize {
        self.heap
            .saturating_add(btree_bytes(
                self.delta_entities.len(),
                size_of::<(GraphEntityId, DeltaEntityV1)>(),
            ))
            .saturating_add(btree_bytes(
                self.delta_relations.len(),
                size_of::<(GraphRelationId, DeltaRelationV1)>(),
            ))
    }

    fn frozen_bytes(&self) -> usize {
        self.delta_entities
            .len()
            .saturating_mul(size_of::<(GraphEntityId, DeltaEntityV1)>())
            .saturating_add(
                self.delta_relations
                    .len()
                    .saturating_mul(size_of::<(GraphRelationId, DeltaRelationV1)>()),
            )
    }

    fn finish(self) -> CatalogLayerV1 {
        CatalogLayerV1 {
            base_generation: self.rows.base_generation,
            hidden_entities: self.rows.hidden_entities.into_boxed_slice(),
            hidden_relations: self.rows.hidden_relations.into_boxed_slice(),
            delta_entities: self.delta_entities.into(),
            delta_relations: self.delta_relations.into(),
        }
    }
}

/// What an entity row contributes, as the layer records it.
pub(super) fn delta_entity(
    entity: &GraphEntity,
    admit: &mut dyn FnMut(usize) -> Result<(), CodeGraphProjectionError>,
) -> Result<DeltaEntityV1, CodeGraphProjectionError> {
    Ok(if has_label(entity, FILE_LABEL) {
        DeltaEntityV1::File(decode_file_record(entity, admit)?.file_occurrence_id)
    } else if has_label(entity, SYMBOL_LABEL) {
        DeltaEntityV1::Symbol(decode_symbol_record(entity, admit)?.occurrence)
    } else if has_label(entity, IMPORT_LABEL) {
        DeltaEntityV1::Import(decode_import_record(entity, admit)?)
    } else {
        DeltaEntityV1::Other
    })
}

/// What a relation row contributes, as the layer records it.
pub(super) fn delta_relation(
    relation: &GraphRelation,
    admit: &mut dyn FnMut(usize) -> Result<(), CodeGraphProjectionError>,
) -> Result<DeltaRelationV1, CodeGraphProjectionError> {
    if relation.kind.as_str() == FILE_IMPORT_EDGE_KIND {
        return Ok(DeltaRelationV1::ImportLink {
            import: relation.to.clone(),
        });
    }
    match code_edge_kind_edge(relation.kind.as_str()) {
        Some(kind) => {
            let (from, to) = edge_endpoints(relation, admit)?;
            Ok(DeltaRelationV1::Edge { kind, from, to })
        }
        None => Ok(DeltaRelationV1::Other),
    }
}

fn delta_entity_heap_bytes(delta: &DeltaEntityV1) -> usize {
    match delta {
        DeltaEntityV1::File(file) => file.as_str().len(),
        DeltaEntityV1::Symbol(occurrence) => occurrence.as_str().len(),
        DeltaEntityV1::Import(import) => import_heap_bytes(import),
        DeltaEntityV1::Other => 0,
    }
}

fn delta_relation_heap_bytes(delta: &DeltaRelationV1) -> usize {
    match delta {
        DeltaRelationV1::Edge { from, to, .. } => from.as_str().len() + to.as_str().len(),
        DeltaRelationV1::ImportLink { import } => import.as_str().len(),
        DeltaRelationV1::Other => 0,
    }
}

/// The fields of an edge record the file dependency fold reads, borrowed so
/// edges of other kinds allocate nothing.
#[derive(Deserialize)]
struct DependencyEdgeRecord {
    from_occurrence: String,
    to_occurrence: String,
}

impl CatalogRecord for DependencyEdgeRecord {
    const PROPERTY: &'static str = EDGE_RECORD_PROPERTY;
}

impl CatalogRecord for SanitizedCodeFileV1 {
    const PROPERTY: &'static str = FILE_RECORD_PROPERTY;
}

impl CatalogRecord for SymbolRecordV1 {
    const PROPERTY: &'static str = SYMBOL_RECORD_PROPERTY;
}

impl CatalogRecord for CodeIndexImportEvidenceV1 {
    const PROPERTY: &'static str = IMPORT_RECORD_PROPERTY;
}

/// Whether edges of `kind` fold into file dependencies.
pub(super) fn is_dependency_kind(kind: RelationEdgeKindV1) -> bool {
    matches!(kind, RelationEdgeKindV1::Calls | RelationEdgeKindV1::Uses)
}

/// The symbol occurrences a code edge row joins, as its payload names them.
pub(super) fn edge_endpoints(
    relation: &GraphRelation,
    admit: &mut dyn FnMut(usize) -> Result<(), CodeGraphProjectionError>,
) -> Result<(SymbolOccurrenceId, SymbolOccurrenceId), CodeGraphProjectionError> {
    let edge: DependencyEdgeRecord = deserialize_catalog_property(&relation.properties, admit)?;
    let endpoint = |occurrence: String| {
        SymbolOccurrenceId::new(occurrence)
            .map_err(|error| CodeGraphProjectionError::Corrupt(error.to_string()))
    };
    Ok((
        endpoint(edge.from_occurrence)?,
        endpoint(edge.to_occurrence)?,
    ))
}

impl CatalogScan {
    fn new(generation: GraphGenerationId, rows: Option<GraphLayeredRowsV1>) -> Self {
        let heap = generation.as_str().len();
        Self {
            generation,
            layer: rows.map(LayerScan::new),
            catalog: CatalogBuilder::new(),
            imports_by_entity: BTreeMap::new(),
            degrees: SymbolDegreeCounts::default(),
            dependency_edge_counts: BTreeMap::new(),
            dependency_edges: 0,
            heap,
            scanned_entities: 0,
            scanned_relations: 0,
        }
    }

    /// An upper bound on what the scan holds so far: the catalog it is
    /// filling and the maps only the scan keeps.
    fn held_bytes(&self) -> usize {
        self.catalog
            .held_bytes()
            .saturating_add(self.heap)
            .saturating_add(btree_bytes(
                self.imports_by_entity.len(),
                size_of::<(GraphEntityId, (CodeIndexImportEvidenceV1, bool))>(),
            ))
            .saturating_add(btree_bytes(
                self.degrees.len(),
                size_of::<(GraphEntityId, (u64, u64))>(),
            ))
            .saturating_add(btree_bytes(
                self.dependency_edge_counts.len(),
                size_of::<((String, String), u64)>(),
            ))
            .saturating_add(self.layer.as_ref().map_or(0, LayerScan::held_bytes))
    }

    /// The arrays [`Self::finish`] allocates while the maps they freeze are
    /// still live.
    fn frozen_bytes(&self) -> usize {
        self.catalog
            .frozen_bytes()
            .saturating_add(
                self.imports_by_entity
                    .len()
                    .saturating_mul(2 * size_of::<CodeIndexImportEvidenceV1>()),
            )
            .saturating_add(
                self.dependency_edge_counts
                    .len()
                    .saturating_mul(size_of::<((String, String), u64)>()),
            )
            .saturating_add(self.layer.as_ref().map_or(0, LayerScan::frozen_bytes))
    }

    fn record_entity_page(
        &mut self,
        entities: &[GraphEntity],
        projection_node_count: usize,
        cancellation: &dyn GraphCancellation,
        page_bytes: usize,
        charge: &mut CatalogBuildChargeV1,
    ) -> Result<(), CodeGraphProjectionError> {
        self.scanned_entities = self
            .scanned_entities
            .checked_add(entities.len())
            .ok_or_else(|| {
                CodeGraphProjectionError::Corrupt(
                    "code graph interactive entity scan overflowed".to_owned(),
                )
            })?;
        if self.scanned_entities > projection_node_count {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph interactive scan exceeded the declared projection node count"
                    .to_owned(),
            ));
        }
        for entity in entities {
            check_cancelled(cancellation)?;
            let held = self.held_bytes().saturating_add(page_bytes).saturating_add(
                btree_bytes(1, size_of::<(SymbolOccurrenceId, CatalogSymbol)>()).saturating_mul(10),
            );
            charge.cover(held)?;
            self.record_entity(entity, &mut |bytes| {
                charge.cover(held.saturating_add(bytes))
            })?;
        }
        Ok(())
    }

    fn record_entity(
        &mut self,
        entity: &GraphEntity,
        admit: &mut dyn FnMut(usize) -> Result<(), CodeGraphProjectionError>,
    ) -> Result<(), CodeGraphProjectionError> {
        if has_label(entity, FILE_LABEL) {
            self.catalog
                .insert_file(decode_file_record(entity, admit)?)?;
        }
        if has_label(entity, SYMBOL_LABEL) {
            self.record_symbol(entity, admit)?;
        }
        if has_label(entity, IMPORT_LABEL) {
            self.record_import(entity, admit)?;
        }
        if let Some(layer) = &mut self.layer {
            layer.record_entity(entity, admit)?;
        }
        Ok(())
    }

    fn record_symbol(
        &mut self,
        entity: &GraphEntity,
        admit: &mut dyn FnMut(usize) -> Result<(), CodeGraphProjectionError>,
    ) -> Result<(), CodeGraphProjectionError> {
        let record = decode_symbol_record(entity, admit)?;
        if self.catalog.symbols.contains_key(&record.occurrence) {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph contains a duplicate symbol entity".to_owned(),
            ));
        }
        if !record.unresolved_calls.is_empty() {
            self.catalog
                .insert_unresolved_source(entity.identity.clone(), record.occurrence.clone());
        }
        self.catalog.insert(
            record.occurrence.clone(),
            CatalogSymbol {
                binding: record.binding,
                metadata: record.metadata,
                unresolved_calls: record.unresolved_calls,
                outgoing: 0,
                incoming: 0,
            },
        );
        Ok(())
    }

    fn record_import(
        &mut self,
        entity: &GraphEntity,
        admit: &mut dyn FnMut(usize) -> Result<(), CodeGraphProjectionError>,
    ) -> Result<(), CodeGraphProjectionError> {
        let record = decode_import_record(entity, admit)?;
        let heap = import_heap_bytes(&record).saturating_add(entity.identity.as_str().len());
        if self
            .imports_by_entity
            .insert(entity.identity.clone(), (record, false))
            .is_some()
        {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph contains a duplicate import entity".to_owned(),
            ));
        }
        self.heap = self.heap.saturating_add(heap);
        Ok(())
    }

    fn record_relation_page(
        &mut self,
        relations: &[GraphRelation],
        cancellation: &dyn GraphCancellation,
        page_bytes: usize,
        charge: &mut CatalogBuildChargeV1,
    ) -> Result<(), CodeGraphProjectionError> {
        for relation in relations {
            check_cancelled(cancellation)?;
            let held = self
                .held_bytes()
                .saturating_add(page_bytes)
                .saturating_add(btree_bytes(2, size_of::<(GraphEntityId, (u64, u64))>()));
            charge.cover(held)?;
            let mut admit = |bytes| charge.cover(held.saturating_add(bytes));
            self.count_relation()?;
            match relation.kind.as_str() {
                FILE_IMPORT_EDGE_KIND => self.record_import_link(relation)?,
                kind => {
                    if let Some(edge_kind) = code_edge_kind_edge(kind) {
                        self.record_code_edge(relation, edge_kind, &mut admit)?;
                    }
                }
            }
            if let Some(layer) = &mut self.layer {
                layer.record_relation(relation, &mut admit)?;
            }
        }
        Ok(())
    }

    fn record_code_edge(
        &mut self,
        relation: &GraphRelation,
        kind: RelationEdgeKindV1,
        admit: &mut dyn FnMut(usize) -> Result<(), CodeGraphProjectionError>,
    ) -> Result<(), CodeGraphProjectionError> {
        for (endpoint, outgoing) in [(&relation.from, true), (&relation.to, false)] {
            let tallied = self.degrees.len();
            if outgoing {
                self.degrees.record_outgoing(endpoint.clone());
            } else {
                self.degrees.record_incoming(endpoint.clone());
            }
            if self.degrees.len() > tallied {
                self.heap = self.heap.saturating_add(endpoint.as_str().len());
            }
        }
        if is_dependency_kind(kind) {
            let (from, to) = edge_endpoints(relation, admit)?;
            self.dependency_edges = self.dependency_edges.saturating_add(1);
            if let (Some(source), Some(target)) = (
                self.catalog.bound_logical_path(&from),
                self.catalog.bound_logical_path(&to),
            ) && source != target
            {
                let pair = (source.to_owned(), target.to_owned());
                let pair_heap = source.len().saturating_add(target.len());
                let counted = self.dependency_edge_counts.entry(pair).or_default();
                if *counted == 0 {
                    self.heap = self.heap.saturating_add(pair_heap);
                }
                *counted += 1;
            }
        }
        Ok(())
    }

    fn count_relation(&mut self) -> Result<(), CodeGraphProjectionError> {
        self.scanned_relations = self.scanned_relations.checked_add(1).ok_or_else(|| {
            CodeGraphProjectionError::Corrupt(
                "code graph interactive relation scan overflowed".to_owned(),
            )
        })?;
        if self.scanned_relations > MAX_VERIFIED_GENERATION_RELATIONS {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph interactive scan exceeded the verified relation ceiling".to_owned(),
            ));
        }
        Ok(())
    }

    /// Validates an import's file link against the import and file the
    /// entity scan already recorded, keeping only that the link was seen.
    fn record_import_link(
        &mut self,
        relation: &GraphRelation,
    ) -> Result<(), CodeGraphProjectionError> {
        let Some((import, linked)) = self.imports_by_entity.get_mut(&relation.to) else {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph file-import relation targets a non-import entity".to_owned(),
            ));
        };
        if *linked {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph import entity has duplicate file links".to_owned(),
            ));
        }
        validate_import_link(
            import,
            self.catalog.files.get(&import.file_occurrence_id),
            Some(relation),
        )?;
        *linked = true;
        Ok(())
    }

    fn finish(
        mut self,
        projection_node_count: usize,
    ) -> Result<InteractiveCatalog, CodeGraphProjectionError> {
        if self.scanned_entities != projection_node_count {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph interactive scan does not match the declared projection node count"
                    .to_owned(),
            ));
        }
        if self.imports_by_entity.values().any(|(_, linked)| !linked) {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph import entities do not have exact file-link coverage".to_owned(),
            ));
        }
        for (occurrence, symbol) in &mut self.catalog.symbols {
            (symbol.outgoing, symbol.incoming) = self.degrees.take(&symbol_entity_id(occurrence)?);
        }
        self.degrees.require_drained()?;
        let mut imports = Vec::with_capacity(self.imports_by_entity.len());
        imports.extend(
            self.imports_by_entity
                .into_values()
                .map(|(import, _)| import),
        );
        imports.sort_by(canonical_import_order);
        Ok(self.catalog.finish(
            self.generation,
            self.layer.map(LayerScan::finish),
            imports,
            self.dependency_edge_counts,
            self.dependency_edges,
        ))
    }
}

/// Per-symbol semantic degree tallied from `CodeEdge.<kind>` rows while they
/// stream past (outgoing at the source symbol, incoming at the target), keyed
/// by the symbol's entity identity.
pub(super) struct SymbolDegreeCounts<K> {
    counts: BTreeMap<K, (u64, u64)>,
}

impl<K> Default for SymbolDegreeCounts<K> {
    fn default() -> Self {
        Self {
            counts: BTreeMap::new(),
        }
    }
}

impl<K: Ord> SymbolDegreeCounts<K> {
    pub(super) fn len(&self) -> usize {
        self.counts.len()
    }

    pub(super) fn record_outgoing(&mut self, symbol: K) {
        self.counts.entry(symbol).or_default().0 += 1;
    }

    pub(super) fn record_incoming(&mut self, symbol: K) {
        self.counts.entry(symbol).or_default().1 += 1;
    }

    pub(super) fn take<Q>(&mut self, symbol: &Q) -> (u64, u64)
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.counts.remove(symbol).unwrap_or_default()
    }

    /// Every tallied endpoint must have been claimed by a symbol entity.
    pub(super) fn require_drained(&self) -> Result<(), CodeGraphProjectionError> {
        if self.counts.is_empty() {
            Ok(())
        } else {
            Err(CodeGraphProjectionError::Corrupt(
                "code graph relation endpoint is not a symbol entity".to_owned(),
            ))
        }
    }
}

/// The file record a `CodeFile` entity carries, proven to be the entity's own.
pub(super) fn decode_file_record(
    entity: &GraphEntity,
    admit: &mut dyn FnMut(usize) -> Result<(), CodeGraphProjectionError>,
) -> Result<SanitizedCodeFileV1, CodeGraphProjectionError> {
    let record: SanitizedCodeFileV1 = deserialize_catalog_property(&entity.properties, admit)?;
    record
        .validate()
        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
    if file_entity_id(&record.file_occurrence_id)? != entity.identity {
        return Err(CodeGraphProjectionError::Corrupt(
            "code graph file identity does not match its payload".to_owned(),
        ));
    }
    Ok(record)
}

/// The symbol record a `CodeSymbol` entity carries, proven to be the
/// entity's own.
pub(super) fn decode_symbol_record(
    entity: &GraphEntity,
    admit: &mut dyn FnMut(usize) -> Result<(), CodeGraphProjectionError>,
) -> Result<SymbolRecordV1, CodeGraphProjectionError> {
    let record: SymbolRecordV1 = deserialize_catalog_property(&entity.properties, admit)?;
    validate_symbol_record(&record)?;
    if symbol_entity_id(&record.occurrence)? != entity.identity {
        return Err(CodeGraphProjectionError::Corrupt(
            "code graph symbol identity does not match its payload".to_owned(),
        ));
    }
    Ok(record)
}

/// The import record a `CodeImport` entity carries, proven to be the
/// entity's own.
pub(super) fn decode_import_record(
    entity: &GraphEntity,
    admit: &mut dyn FnMut(usize) -> Result<(), CodeGraphProjectionError>,
) -> Result<CodeIndexImportEvidenceV1, CodeGraphProjectionError> {
    let record: CodeIndexImportEvidenceV1 =
        deserialize_catalog_property(&entity.properties, &mut *admit)?;
    record.validate().map_err(|error| {
        CodeGraphProjectionError::Corrupt(format!(
            "code graph import row is not canonical: {error}"
        ))
    })?;
    let serialized = super::super::schema::serialized_record_len(&record)?;
    // serde_json's byte Vec starts at 128 and grows by max(required, 2 *
    // capacity). Its reallocation peak is at most 128 + 3 * written bytes.
    // hex::encode then holds that Vec alongside exactly 2 * written bytes.
    // The final fixed-size identity and digest text fit in another 128 bytes.
    admit(
        import_heap_bytes(&record)
            .saturating_add(serialized.saturating_mul(3))
            .saturating_add(serialized.saturating_mul(2))
            .saturating_add(256),
    )?;
    if import_entity_id(&record)? != entity.identity {
        return Err(CodeGraphProjectionError::Corrupt(
            "code graph import identity does not match its payload".to_owned(),
        ));
    }
    Ok(record)
}

/// An import entity's file link: the import's file is published under the
/// import's logical path, and exactly one canonical relation from that file
/// names the import.
pub(super) fn validate_import_link(
    import: &CodeIndexImportEvidenceV1,
    file: Option<&SanitizedCodeFileV1>,
    relation: Option<&GraphRelation>,
) -> Result<(), CodeGraphProjectionError> {
    let file = file.ok_or_else(|| {
        CodeGraphProjectionError::Corrupt(
            "code graph import refers to a missing file occurrence".to_owned(),
        )
    })?;
    if file.logical_path != import.logical_path {
        return Err(CodeGraphProjectionError::Corrupt(
            "code graph import logical path does not match its file occurrence".to_owned(),
        ));
    }
    let relation = relation.ok_or_else(|| {
        CodeGraphProjectionError::Corrupt(
            "code graph import entity is missing its file link".to_owned(),
        )
    })?;
    if relation.from != file_entity_id(&import.file_occurrence_id)? {
        return Err(CodeGraphProjectionError::Corrupt(
            "code graph import file link does not match its payload".to_owned(),
        ));
    }
    if relation.identity != file_import_relation_id(import)? || !relation.properties.is_empty() {
        return Err(CodeGraphProjectionError::Corrupt(
            "code graph import file link is not canonical".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn canonical_import_order(
    left: &CodeIndexImportEvidenceV1,
    right: &CodeIndexImportEvidenceV1,
) -> Ordering {
    left.logical_path
        .cmp(&right.logical_path)
        .then(left.file_occurrence_id.cmp(&right.file_occurrence_id))
        .then(left.span.start_byte.cmp(&right.span.start_byte))
        .then(left.span.end_byte.cmp(&right.span.end_byte))
        .then(left.start_line.cmp(&right.start_line))
        .then(left.start_column.cmp(&right.start_column))
        .then(left.module_specifier.cmp(&right.module_specifier))
        .then(left.imported_name.cmp(&right.imported_name))
        .then(left.local_name.cmp(&right.local_name))
        .then(left.namespace.cmp(&right.namespace))
        .then(left.module_kind.cmp(&right.module_kind))
}

pub(super) fn check_cancelled(
    cancellation: &dyn GraphCancellation,
) -> Result<(), CodeGraphProjectionError> {
    if cancellation.is_cancelled() {
        return Err(CodeGraphProjectionError::Cancelled);
    }
    Ok(())
}
