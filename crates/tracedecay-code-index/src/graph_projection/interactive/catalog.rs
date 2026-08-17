//! Bounded construction of the generation-pinned interactive catalog.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::Arc;

use tracedecay_domain::SanitizedCodeFileV1;
use tracedecay_graph_db::{
    GraphCancellation, GraphEntity, GraphEntityId, GraphProjectionIdentity,
    GraphProjectionReadRequest, GraphRelation, MAX_VERIFIED_GENERATION_RELATIONS,
    VerifiedGraphSnapshot,
};

use super::super::schema::{
    FILE_IMPORT_EDGE_KIND, FILE_LABEL, FILE_RECORD_PROPERTY, IMPORT_LABEL, IMPORT_RECORD_PROPERTY,
    SYMBOL_LABEL, SYMBOL_RECORD_PROPERTY, deserialize_property, file_entity_id,
    file_import_relation_id, has_label, import_entity_id,
};
use super::super::{
    CodeGraphProjectionError, SymbolRecordV1, symbol_entity_id, validate_symbol_record,
};
use super::models::{CatalogSymbol, InteractiveCatalog};
use crate::chunks::CodeIndexImportEvidenceV1;

const CATALOG_SCAN_PAGE_ITEMS: usize = 1_024;

pub(super) fn build_interactive_catalog(
    snapshot: &VerifiedGraphSnapshot,
    projection: &GraphProjectionIdentity,
    projection_node_count: usize,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<InteractiveCatalog, CodeGraphProjectionError> {
    let mut scan = CatalogScan::new();
    let mut after_entity = None;
    let mut after_relation = None;
    let mut entities_complete = false;
    let mut relations_complete = false;

    while !entities_complete || !relations_complete {
        check_cancelled(cancellation.as_ref())?;
        let page = snapshot.read_projection(GraphProjectionReadRequest {
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
        })?;

        if !entities_complete {
            scan.record_entity_page(&page.entities, projection_node_count, cancellation.as_ref())?;
            after_entity = page.next_entity;
            entities_complete = after_entity.is_none();
        }
        if !relations_complete {
            scan.record_relation_page(&page.relations, cancellation.as_ref())?;
            after_relation = page.next_relation;
            relations_complete = after_relation.is_none();
        }
    }

    check_cancelled(cancellation.as_ref())?;
    scan.finish(projection_node_count)
}

struct CatalogScan {
    catalog: InteractiveCatalog,
    imports_by_entity: BTreeMap<GraphEntityId, CodeIndexImportEvidenceV1>,
    import_links: BTreeMap<GraphEntityId, GraphRelation>,
    scanned_entities: usize,
    scanned_relations: usize,
}

impl CatalogScan {
    fn new() -> Self {
        Self {
            catalog: InteractiveCatalog {
                symbols: BTreeMap::new(),
                by_qualified_name: BTreeMap::new(),
                by_simple_name: BTreeMap::new(),
                by_file: BTreeMap::new(),
                by_logical_path: BTreeMap::new(),
                files: BTreeMap::new(),
                imports: Vec::new(),
            },
            imports_by_entity: BTreeMap::new(),
            import_links: BTreeMap::new(),
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
        Ok(())
    }

    fn record_file(&mut self, entity: &GraphEntity) -> Result<(), CodeGraphProjectionError> {
        let record: SanitizedCodeFileV1 = deserialize_property(entity, FILE_RECORD_PROPERTY)?;
        record
            .validate()
            .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
        if file_entity_id(&record.file_occurrence_id)? != entity.identity {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph file identity does not match its payload".to_owned(),
            ));
        }
        let previous = self.catalog.by_logical_path.insert(
            record.logical_path.clone(),
            record.file_occurrence_id.clone(),
        );
        if let Some(existing) = previous {
            if existing != record.file_occurrence_id {
                return Err(CodeGraphProjectionError::Corrupt(format!(
                    "code graph logical path `{}` is claimed by more than one file occurrence",
                    record.logical_path
                )));
            }
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
        let record: SymbolRecordV1 = deserialize_property(entity, SYMBOL_RECORD_PROPERTY)?;
        validate_symbol_record(&record)?;
        if symbol_entity_id(&record.occurrence)? != entity.identity {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph symbol identity does not match its payload".to_owned(),
            ));
        }
        if self.catalog.symbols.contains_key(&record.occurrence) {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph contains a duplicate symbol entity".to_owned(),
            ));
        }
        self.catalog.insert(
            record.occurrence.clone(),
            CatalogSymbol {
                binding: record.binding,
                metadata: record.metadata,
            },
        );
        Ok(())
    }

    fn record_import(&mut self, entity: &GraphEntity) -> Result<(), CodeGraphProjectionError> {
        let record: CodeIndexImportEvidenceV1 =
            deserialize_property(entity, IMPORT_RECORD_PROPERTY)?;
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
        self.scanned_relations = self
            .scanned_relations
            .checked_add(relations.len())
            .ok_or_else(|| {
                CodeGraphProjectionError::Corrupt(
                    "code graph interactive relation scan overflowed".to_owned(),
                )
            })?;
        if self.scanned_relations > MAX_VERIFIED_GENERATION_RELATIONS {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph interactive scan exceeded the verified relation ceiling".to_owned(),
            ));
        }
        for relation in relations {
            check_cancelled(cancellation)?;
            if relation.kind.as_str() != FILE_IMPORT_EDGE_KIND {
                continue;
            }
            if self
                .import_links
                .insert(relation.to.clone(), relation.clone())
                .is_some()
            {
                return Err(CodeGraphProjectionError::Corrupt(
                    "code graph import entity has duplicate file links".to_owned(),
                ));
            }
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
            let file = self
                .catalog
                .files
                .get(&import.file_occurrence_id)
                .ok_or_else(|| {
                    CodeGraphProjectionError::Corrupt(
                        "code graph import refers to a missing file occurrence".to_owned(),
                    )
                })?;
            if file.logical_path != import.logical_path {
                return Err(CodeGraphProjectionError::Corrupt(
                    "code graph import logical path does not match its file occurrence".to_owned(),
                ));
            }
            let expected_file = file_entity_id(&import.file_occurrence_id)?;
            let relation = self.import_links.get(identity).ok_or_else(|| {
                CodeGraphProjectionError::Corrupt(
                    "code graph import entity is missing its file link".to_owned(),
                )
            })?;
            if relation.from != expected_file {
                return Err(CodeGraphProjectionError::Corrupt(
                    "code graph import file link does not match its payload".to_owned(),
                ));
            }
            if relation.identity != file_import_relation_id(import)?
                || !relation.properties.is_empty()
            {
                return Err(CodeGraphProjectionError::Corrupt(
                    "code graph import file link is not canonical".to_owned(),
                ));
            }
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

        self.catalog.imports = self.imports_by_entity.into_values().collect();
        self.catalog.imports.sort_by(canonical_import_order);
        Ok(self.catalog)
    }
}

fn canonical_import_order(
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
