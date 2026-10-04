//! Bounded construction of the generation-pinned interactive catalog.

use std::borrow::Borrow;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Deserialize;
use tracedecay_domain::{RelationEdgeKindV1, SanitizedCodeFileV1, SymbolOccurrenceId};
use tracedecay_graph_db::{
    GraphCancellation, GraphEntity, GraphEntityId, GraphGenerationId, GraphLayeredRowsV1,
    GraphProjectionIdentity, GraphProjectionReadRequest, GraphRelation, GraphRelationId,
    MAX_VERIFIED_GENERATION_RELATIONS, VerifiedGraphSnapshot,
};

use super::super::schema::{
    FILE_IMPORT_EDGE_KIND, FILE_LABEL, FILE_RECORD_PROPERTY, IMPORT_LABEL, IMPORT_RECORD_PROPERTY,
    SYMBOL_LABEL, SYMBOL_RECORD_PROPERTY, deserialize_property, file_entity_id,
    file_import_relation_id, has_label, import_entity_id,
};
use super::super::{
    CodeGraphProjectionError, EDGE_RECORD_PROPERTY, SymbolRecordV1, code_edge_kind_edge,
    symbol_entity_id, validate_symbol_record,
};
use super::models::{
    CatalogBuilder, CatalogLayerV1, CatalogSymbol, DeltaEntityV1, DeltaRelationV1,
    InteractiveCatalog,
};
use crate::chunks::CodeIndexImportEvidenceV1;

const CATALOG_SCAN_PAGE_ITEMS: usize = 1_024;

pub(super) fn build_interactive_catalog(
    snapshot: &VerifiedGraphSnapshot,
    projection: &GraphProjectionIdentity,
    projection_node_count: usize,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<InteractiveCatalog, CodeGraphProjectionError> {
    let mut scan = CatalogScan::new(snapshot.generation().clone(), snapshot.layered_rows());
    let mut after_entity = None;
    let mut after_relation = None;
    let mut entities_complete = false;
    let mut relations_complete = false;

    while !entities_complete || !relations_complete {
        check_cancelled(cancellation.as_ref())?;
        let page = {
            let _span = tracing::trace_span!("code_graph.catalog.scan_page").entered();
            {
                snapshot.read_projection(GraphProjectionReadRequest {
                    namespace: projection.namespace.clone(),
                    projection: projection.projection.clone(),
                    after_entity: after_entity.clone(),
                    after_relation: after_relation.clone(),
                    max_entities: if entities_complete {
                        0
                    } else {
                        CATALOG_SCAN_PAGE_ITEMS
                    },
                    max_relations: if relations_complete {
                        0
                    } else {
                        CATALOG_SCAN_PAGE_ITEMS
                    },
                    cancellation: Arc::clone(&cancellation),
                })
            }
        }?;
        metrics::gauge!("code_graph.catalog.pages_scanned").increment(1.0);

        if !entities_complete {
            {
                let _span = tracing::trace_span!("code_graph.catalog.record_entities").entered();
                {
                    scan.record_entity_page(
                        &page.entities,
                        projection_node_count,
                        cancellation.as_ref(),
                    )
                }
            }?;
            metrics::gauge!("code_graph.catalog.entities_recorded")
                .increment((page.entities.len() as u64) as f64);
            after_entity = page.next_entity;
            entities_complete = after_entity.is_none();
        }
        if !relations_complete {
            {
                let _span = tracing::trace_span!("code_graph.catalog.record_relations").entered();
                scan.record_relation_page(&page.relations, cancellation.as_ref())
            }?;
            metrics::gauge!("code_graph.catalog.relations_recorded")
                .increment((page.relations.len() as u64) as f64);
            after_relation = page.next_relation;
            relations_complete = after_relation.is_none();
        }
    }

    check_cancelled(cancellation.as_ref())?;
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
    imports_by_entity: BTreeMap<GraphEntityId, CodeIndexImportEvidenceV1>,
    import_links: BTreeMap<GraphEntityId, GraphRelation>,
    degrees: SymbolDegreeCounts<GraphEntityId>,
    /// `calls`/`uses` edge endpoints, folded into file dependencies once
    /// every symbol's file is known.
    dependency_edges: Vec<(SymbolOccurrenceId, SymbolOccurrenceId)>,
    scanned_entities: usize,
    scanned_relations: usize,
}

struct LayerScan {
    rows: GraphLayeredRowsV1,
    delta_entities: BTreeMap<GraphEntityId, DeltaEntityV1>,
    delta_relations: BTreeMap<GraphRelationId, DeltaRelationV1>,
}

impl LayerScan {
    fn record_entity(&mut self, entity: &GraphEntity) -> Result<(), CodeGraphProjectionError> {
        if self
            .rows
            .delta_entities
            .binary_search(&entity.identity)
            .is_ok()
        {
            self.delta_entities
                .insert(entity.identity.clone(), delta_entity(entity)?);
        }
        Ok(())
    }

    fn record_relation(
        &mut self,
        relation: &GraphRelation,
    ) -> Result<(), CodeGraphProjectionError> {
        if self
            .rows
            .delta_relations
            .binary_search(&relation.identity)
            .is_ok()
        {
            self.delta_relations
                .insert(relation.identity.clone(), delta_relation(relation)?);
        }
        Ok(())
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
) -> Result<DeltaEntityV1, CodeGraphProjectionError> {
    Ok(if has_label(entity, FILE_LABEL) {
        DeltaEntityV1::File(decode_file_record(entity)?.file_occurrence_id)
    } else if has_label(entity, SYMBOL_LABEL) {
        DeltaEntityV1::Symbol(decode_symbol_record(entity)?.occurrence)
    } else if has_label(entity, IMPORT_LABEL) {
        DeltaEntityV1::Import(decode_import_record(entity)?)
    } else {
        DeltaEntityV1::Other
    })
}

/// What a relation row contributes, as the layer records it.
pub(super) fn delta_relation(
    relation: &GraphRelation,
) -> Result<DeltaRelationV1, CodeGraphProjectionError> {
    if relation.kind.as_str() == FILE_IMPORT_EDGE_KIND {
        return Ok(DeltaRelationV1::ImportLink {
            import: relation.to.clone(),
        });
    }
    match code_edge_kind_edge(relation.kind.as_str()) {
        Some(kind) => {
            let (from, to) = edge_endpoints(relation)?;
            Ok(DeltaRelationV1::Edge { kind, from, to })
        }
        None => Ok(DeltaRelationV1::Other),
    }
}

/// The fields of an edge record the file dependency fold reads, borrowed so
/// edges of other kinds allocate nothing.
#[derive(Deserialize)]
struct DependencyEdgeRecord {
    from_occurrence: String,
    to_occurrence: String,
}

/// Whether edges of `kind` fold into file dependencies.
pub(super) fn is_dependency_kind(kind: RelationEdgeKindV1) -> bool {
    matches!(kind, RelationEdgeKindV1::Calls | RelationEdgeKindV1::Uses)
}

/// The symbol occurrences a code edge row joins, as its payload names them.
pub(super) fn edge_endpoints(
    relation: &GraphRelation,
) -> Result<(SymbolOccurrenceId, SymbolOccurrenceId), CodeGraphProjectionError> {
    let edge: DependencyEdgeRecord =
        deserialize_property(&relation.properties, EDGE_RECORD_PROPERTY)?;
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
        Self {
            generation,
            layer: rows.map(|rows| LayerScan {
                rows,
                delta_entities: BTreeMap::new(),
                delta_relations: BTreeMap::new(),
            }),
            catalog: CatalogBuilder::new(),
            imports_by_entity: BTreeMap::new(),
            import_links: BTreeMap::new(),
            degrees: SymbolDegreeCounts::default(),
            dependency_edges: Vec::new(),
            scanned_entities: 0,
            scanned_relations: 0,
        }
    }

    fn record_entity_page(
        &mut self,
        entities: &[GraphEntity],
        projection_node_count: usize,
        cancellation: &dyn GraphCancellation,
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
            self.record_entity(entity)?;
        }
        Ok(())
    }

    fn record_entity(&mut self, entity: &GraphEntity) -> Result<(), CodeGraphProjectionError> {
        if has_label(entity, FILE_LABEL) {
            self.record_file(entity)?;
        }
        if has_label(entity, SYMBOL_LABEL) {
            self.record_symbol(entity)?;
        }
        if has_label(entity, IMPORT_LABEL) {
            self.record_import(entity)?;
        }
        if let Some(layer) = &mut self.layer {
            layer.record_entity(entity)?;
        }
        Ok(())
    }

    fn record_file(&mut self, entity: &GraphEntity) -> Result<(), CodeGraphProjectionError> {
        let record = decode_file_record(entity)?;
        let previous = self.catalog.by_logical_path.insert(
            record.logical_path.clone(),
            record.file_occurrence_id.clone(),
        );
        if let Some(existing) = previous
            && existing != record.file_occurrence_id
        {
            return Err(CodeGraphProjectionError::Corrupt(format!(
                "code graph logical path `{}` is claimed by more than one file occurrence",
                record.logical_path
            )));
        }
        if self
            .catalog
            .files
            .insert(record.file_occurrence_id.clone(), record)
            .is_some()
        {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph contains a duplicate file entity".to_owned(),
            ));
        }
        Ok(())
    }

    fn record_symbol(&mut self, entity: &GraphEntity) -> Result<(), CodeGraphProjectionError> {
        let record = decode_symbol_record(entity)?;
        if self.catalog.symbols.contains_key(&record.occurrence) {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph contains a duplicate symbol entity".to_owned(),
            ));
        }
        if !record.unresolved_calls.is_empty() {
            self.catalog
                .unresolved_sources_by_entity
                .insert(entity.identity.clone(), record.occurrence.clone());
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

    fn record_import(&mut self, entity: &GraphEntity) -> Result<(), CodeGraphProjectionError> {
        let record = decode_import_record(entity)?;
        if self
            .imports_by_entity
            .insert(entity.identity.clone(), record)
            .is_some()
        {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph contains a duplicate import entity".to_owned(),
            ));
        }
        Ok(())
    }

    fn record_relation_page(
        &mut self,
        relations: &[GraphRelation],
        cancellation: &dyn GraphCancellation,
    ) -> Result<(), CodeGraphProjectionError> {
        for relation in relations {
            check_cancelled(cancellation)?;
            self.count_relation()?;
            match relation.kind.as_str() {
                FILE_IMPORT_EDGE_KIND => self.record_import_link(relation.clone())?,
                kind => {
                    if let Some(edge_kind) = code_edge_kind_edge(kind) {
                        self.record_code_edge(relation, edge_kind)?;
                    }
                }
            }
            if let Some(layer) = &mut self.layer {
                layer.record_relation(relation)?;
            }
        }
        Ok(())
    }

    fn record_code_edge(
        &mut self,
        relation: &GraphRelation,
        kind: RelationEdgeKindV1,
    ) -> Result<(), CodeGraphProjectionError> {
        self.degrees.record_outgoing(relation.from.clone());
        self.degrees.record_incoming(relation.to.clone());
        if is_dependency_kind(kind) {
            self.dependency_edges.push(edge_endpoints(relation)?);
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

    fn record_import_link(
        &mut self,
        relation: GraphRelation,
    ) -> Result<(), CodeGraphProjectionError> {
        if self
            .import_links
            .insert(relation.to.clone(), relation)
            .is_some()
        {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph import entity has duplicate file links".to_owned(),
            ));
        }
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
        if self.import_links.len() != self.imports_by_entity.len() {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph import entities do not have exact file-link coverage".to_owned(),
            ));
        }

        for (identity, import) in &self.imports_by_entity {
            let file = self.catalog.files.get(&import.file_occurrence_id);
            let relation = self.import_links.get(identity);
            validate_import_link(import, file, relation)?;
        }
        if self
            .import_links
            .keys()
            .any(|identity| !self.imports_by_entity.contains_key(identity))
        {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph file-import relation targets a non-import entity".to_owned(),
            ));
        }

        for (occurrence, symbol) in &mut self.catalog.symbols {
            (symbol.outgoing, symbol.incoming) = self.degrees.take(&symbol_entity_id(occurrence)?);
        }
        self.degrees.require_drained()?;
        let mut imports: Vec<_> = self.imports_by_entity.into_values().collect();
        imports.sort_by(canonical_import_order);
        Ok(self.catalog.finish(
            self.generation,
            self.layer.map(LayerScan::finish),
            imports,
            &self.dependency_edges,
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
) -> Result<SanitizedCodeFileV1, CodeGraphProjectionError> {
    let record: SanitizedCodeFileV1 =
        deserialize_property(&entity.properties, FILE_RECORD_PROPERTY)?;
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
) -> Result<SymbolRecordV1, CodeGraphProjectionError> {
    let record: SymbolRecordV1 = deserialize_property(&entity.properties, SYMBOL_RECORD_PROPERTY)?;
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
) -> Result<CodeIndexImportEvidenceV1, CodeGraphProjectionError> {
    let record: CodeIndexImportEvidenceV1 =
        deserialize_property(&entity.properties, IMPORT_RECORD_PROPERTY)?;
    record.validate().map_err(|error| {
        CodeGraphProjectionError::Corrupt(format!(
            "code graph import row is not canonical: {error}"
        ))
    })?;
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
