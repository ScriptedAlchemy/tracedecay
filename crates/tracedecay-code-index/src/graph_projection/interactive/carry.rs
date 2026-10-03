//! The successor generation's catalog, carried from its predecessor's.
//!
//! A layered generation serves its sealed base's rows except the ones it
//! hides or carries in its own delta. Two generations over one base can
//! therefore differ only in those rows, so the successor's catalog is the
//! predecessor's with the contribution of every row that differs between
//! them reversed and re-applied: files, symbols and imports by record,
//! degrees and file dependencies by counted edge.
//!
//! The predecessor's own rows are never read: retention retires them once
//! the successor publishes. What the predecessor contributed is in its
//! catalog, which records each delta row's contribution; what the base
//! holds is read from the base the successor pins; what the successor adds
//! is read from the successor.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tracedecay_domain::{FileOccurrenceId, SanitizedCodeFileV1, SymbolOccurrenceId};
use tracedecay_graph_db::{
    GraphCancellation, GraphEntity, GraphEntityId, GraphEntityRef, GraphGenerationId,
    GraphGenerationRelation, GraphLayeredRowsV1, GraphProjectionIdentity, GraphRelation,
    GraphRelationId, GraphRelationRef, VerifiedGraphSnapshot,
};

use super::super::schema::{FILE_LABEL, IMPORT_LABEL, SYMBOL_LABEL, has_label};
use super::super::{CodeGraphProjectionError, CodeGraphSymbolBindingV1};
use super::catalog::{
    canonical_import_order, check_cancelled, decode_file_record, decode_import_record,
    decode_symbol_record, delta_relation, is_dependency_kind, validate_import_link,
};
use super::models::{
    CatalogLayerV1, CatalogSymbol, DeltaEntityV1, DeltaRelationV1, InteractiveCatalog, SortedMap,
    SymbolIds, derived_simple_name, fold_file_dependencies, frozen_ids, rank_largest_files,
    unresolved_callee_name,
};
use crate::chunks::{CodeIndexImportEvidenceV1, CodeIndexUnresolvedReferenceV1};
use crate::lineage::LineageSymbolRecordV1;

/// Why a successor scans its projection cold instead of carrying.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CatalogCarryDeclineV1 {
    /// The successor is a cold generation: it layers over no base.
    ColdHead,
    /// The predecessor is neither the successor's base nor layered over it.
    ForeignBase,
}

impl CatalogCarryDeclineV1 {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::ColdHead => "cold_head",
            Self::ForeignBase => "foreign_base",
        }
    }
}

/// Builds the successor's catalog from `parent`'s. `Ok(Err(_))` names why
/// the successor cannot carry and must scan.
pub(super) fn carry_interactive_catalog(
    parent: &InteractiveCatalog,
    snapshot: &VerifiedGraphSnapshot,
    projection: &GraphProjectionIdentity,
    projection_node_count: usize,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<Result<InteractiveCatalog, CatalogCarryDeclineV1>, CodeGraphProjectionError> {
    let Some(rows) = snapshot.layered_rows() else {
        return Ok(Err(CatalogCarryDeclineV1::ColdHead));
    };
    let parent_base = parent
        .layer
        .as_ref()
        .map_or(&parent.generation, |layer| &layer.base_generation);
    if *parent_base != rows.base_generation {
        return Ok(Err(CatalogCarryDeclineV1::ForeignBase));
    }
    let reads = RowReads {
        parent,
        snapshot,
        projection,
        rows: &rows,
        cancellation: &cancellation,
    };
    let (
        parent_hidden_entities,
        parent_delta_entities,
        parent_hidden_relations,
        parent_delta_relations,
    ) = match &parent.layer {
        Some(layer) => (
            &layer.hidden_entities[..],
            layer.delta_entities.keys().cloned().collect::<Vec<_>>(),
            &layer.hidden_relations[..],
            layer.delta_relations.keys().cloned().collect::<Vec<_>>(),
        ),
        None => (&[][..], Vec::new(), &[][..], Vec::new()),
    };
    let entity_ids = merge_sorted([
        &rows.hidden_entities[..],
        &rows.delta_entities[..],
        parent_hidden_entities,
        &parent_delta_entities[..],
    ]);
    let relation_ids = merge_sorted([
        &rows.hidden_relations[..],
        &rows.delta_relations[..],
        parent_hidden_relations,
        &parent_delta_relations[..],
    ]);
    hotpath::gauge!("code_graph.catalog.carry.rows_compared")
        .inc((entity_ids.len() + relation_ids.len()) as u64);

    let mut layer = CatalogLayerScan::new(&rows);
    let changed_entities = hotpath::measure_block!("code_graph.catalog.carry.read_entities", {
        let mut changed = Vec::new();
        for identity in entity_ids {
            check_cancelled(cancellation.as_ref())?;
            let (old, new) = reads.entity(&identity)?;
            if let Some(new) = &new {
                layer.record_entity(&identity, new);
            }
            if old != new {
                changed.push((old, new));
            }
        }
        Ok::<_, CodeGraphProjectionError>(changed)
    })?;
    let changed_relations = hotpath::measure_block!("code_graph.catalog.carry.read_relations", {
        let mut changed = Vec::new();
        for identity in relation_ids {
            check_cancelled(cancellation.as_ref())?;
            let (old, new) = reads.relation(&identity)?;
            if let Some(new) = &new {
                layer.record_relation(&identity, new);
            }
            if old != new {
                changed.push((old, new));
            }
        }
        Ok::<_, CodeGraphProjectionError>(changed)
    })?;
    hotpath::gauge!("code_graph.catalog.carry.rows_changed")
        .inc((changed_entities.len() + changed_relations.len()) as u64);

    hotpath::measure_block!("code_graph.catalog.carry.apply", {
        let mut carry = CatalogCarry::new(parent);
        for (old, _) in &changed_entities {
            check_cancelled(cancellation.as_ref())?;
            if let Some(old) = old {
                carry.remove_entity(old)?;
            }
        }
        for (old, _) in &changed_relations {
            check_cancelled(cancellation.as_ref())?;
            if let Some(old) = old {
                carry.remove_relation(old);
            }
        }
        for (_, new) in &changed_entities {
            check_cancelled(cancellation.as_ref())?;
            if let Some(new) = new {
                carry.add_entity(new)?;
            }
        }
        for (_, new) in &changed_relations {
            check_cancelled(cancellation.as_ref())?;
            if let Some(new) = new {
                carry.add_relation(new);
            }
        }
        check_cancelled(cancellation.as_ref())?;
        carry
            .finish(
                snapshot.generation().clone(),
                layer.finish(),
                projection_node_count,
            )
            .map(Ok)
    })
}

fn merge_sorted<T: Ord + Clone, const N: usize>(lists: [&[T]; N]) -> Vec<T> {
    let mut merged: Vec<T> = lists.iter().flat_map(|list| list.iter().cloned()).collect();
    merged.sort_unstable();
    merged.dedup();
    merged
}

fn corrupt(message: &str) -> CodeGraphProjectionError {
    CodeGraphProjectionError::Corrupt(message.to_owned())
}

/// A symbol entity's record without its degrees, which relations decide.
#[derive(Clone, PartialEq, Eq)]
struct SymbolRow {
    binding: Option<CodeGraphSymbolBindingV1>,
    metadata: Option<LineageSymbolRecordV1>,
    unresolved_calls: Vec<CodeIndexUnresolvedReferenceV1>,
}

impl SymbolRow {
    fn of(symbol: &CatalogSymbol) -> Self {
        Self {
            binding: symbol.binding.clone(),
            metadata: symbol.metadata.clone(),
            unresolved_calls: symbol.unresolved_calls.clone(),
        }
    }
}

/// What one entity row contributes to the catalog.
#[derive(PartialEq, Eq)]
enum EntityContent {
    File(SanitizedCodeFileV1),
    Symbol {
        identity: GraphEntityId,
        occurrence: SymbolOccurrenceId,
        row: SymbolRow,
    },
    Import {
        identity: GraphEntityId,
        import: CodeIndexImportEvidenceV1,
    },
    Other,
}

impl EntityContent {
    fn decode(entity: &GraphEntity) -> Result<Self, CodeGraphProjectionError> {
        Ok(if has_label(entity, FILE_LABEL) {
            Self::File(decode_file_record(entity)?)
        } else if has_label(entity, SYMBOL_LABEL) {
            let record = decode_symbol_record(entity)?;
            Self::Symbol {
                identity: entity.identity.clone(),
                occurrence: record.occurrence,
                row: SymbolRow {
                    binding: record.binding,
                    metadata: record.metadata,
                    unresolved_calls: record.unresolved_calls,
                },
            }
        } else if has_label(entity, IMPORT_LABEL) {
            Self::Import {
                identity: entity.identity.clone(),
                import: decode_import_record(entity)?,
            }
        } else {
            Self::Other
        })
    }

    /// The content a predecessor's delta row had, from the catalog that
    /// recorded it.
    fn recorded(
        parent: &InteractiveCatalog,
        identity: &GraphEntityId,
        entity: &DeltaEntityV1,
    ) -> Result<Self, CodeGraphProjectionError> {
        Ok(match entity {
            DeltaEntityV1::File(file) => Self::File(
                parent
                    .files
                    .get(file)
                    .cloned()
                    .ok_or_else(|| corrupt("catalog layer names a file the catalog lacks"))?,
            ),
            DeltaEntityV1::Symbol(occurrence) => {
                Self::Symbol {
                    identity: identity.clone(),
                    occurrence: occurrence.clone(),
                    row: SymbolRow::of(parent.symbols.get(occurrence).ok_or_else(|| {
                        corrupt("catalog layer names a symbol the catalog lacks")
                    })?),
                }
            }
            DeltaEntityV1::Import(import) => Self::Import {
                identity: identity.clone(),
                import: import.clone(),
            },
            DeltaEntityV1::Other => Self::Other,
        })
    }

    fn delta(&self) -> DeltaEntityV1 {
        match self {
            Self::File(file) => DeltaEntityV1::File(file.file_occurrence_id.clone()),
            Self::Symbol { occurrence, .. } => DeltaEntityV1::Symbol(occurrence.clone()),
            Self::Import { import, .. } => DeltaEntityV1::Import(import.clone()),
            Self::Other => DeltaEntityV1::Other,
        }
    }
}

/// What one relation row contributes to the catalog. An import link
/// carries its row so an added import's link can be proven canonical.
enum RelationContent {
    Edge(DeltaRelationV1),
    ImportLink {
        import: GraphEntityId,
        relation: Option<GraphRelation>,
    },
    Other,
}

impl PartialEq for RelationContent {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Edge(left), Self::Edge(right)) => left == right,
            (Self::ImportLink { import: left, .. }, Self::ImportLink { import: right, .. }) => {
                left == right
            }
            (Self::Other, Self::Other) => true,
            _ => false,
        }
    }
}

impl RelationContent {
    fn decode(relation: GraphRelation) -> Result<Self, CodeGraphProjectionError> {
        Ok(match delta_relation(&relation)? {
            edge @ DeltaRelationV1::Edge { .. } => Self::Edge(edge),
            DeltaRelationV1::ImportLink { import } => Self::ImportLink {
                import,
                relation: Some(relation),
            },
            DeltaRelationV1::Other => Self::Other,
        })
    }

    fn recorded(relation: &DeltaRelationV1) -> Self {
        match relation {
            edge @ DeltaRelationV1::Edge { .. } => Self::Edge(edge.clone()),
            DeltaRelationV1::ImportLink { import } => Self::ImportLink {
                import: import.clone(),
                relation: None,
            },
            DeltaRelationV1::Other => Self::Other,
        }
    }

    fn delta(&self) -> DeltaRelationV1 {
        match self {
            Self::Edge(edge) => edge.clone(),
            Self::ImportLink { import, .. } => DeltaRelationV1::ImportLink {
                import: import.clone(),
            },
            Self::Other => DeltaRelationV1::Other,
        }
    }
}

fn storage_relation(
    relation: GraphGenerationRelation,
) -> Result<GraphRelation, CodeGraphProjectionError> {
    GraphRelation::new(
        relation.identity,
        relation.from.identity,
        relation.to.identity,
        relation.kind,
        relation.properties,
    )
    .map_err(Into::into)
}

/// Where each row's predecessor and successor readings come from.
struct RowReads<'a> {
    parent: &'a InteractiveCatalog,
    snapshot: &'a VerifiedGraphSnapshot,
    projection: &'a GraphProjectionIdentity,
    rows: &'a GraphLayeredRowsV1,
    cancellation: &'a Arc<dyn GraphCancellation>,
}

/// Which generation's rows a reading comes from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    /// The row is the sealed base's.
    Base,
    /// The generation hides the base's row and carries none of its own.
    Hidden,
    /// The generation's own delta row.
    Delta,
}

impl RowReads<'_> {
    fn parent_entity_source(&self, identity: &GraphEntityId) -> Source {
        match &self.parent.layer {
            Some(layer) if layer.delta_entities.get(identity).is_some() => Source::Delta,
            Some(layer) if layer.hides_entity(identity) => Source::Hidden,
            _ => Source::Base,
        }
    }

    fn successor_entity_source(&self, identity: &GraphEntityId) -> Source {
        if self.rows.delta_entities.binary_search(identity).is_ok() {
            Source::Delta
        } else if self.rows.hidden_entities.binary_search(identity).is_ok() {
            Source::Hidden
        } else {
            Source::Base
        }
    }

    fn parent_relation_source(&self, identity: &GraphRelationId) -> Source {
        match &self.parent.layer {
            Some(layer) if layer.delta_relations.get(identity).is_some() => Source::Delta,
            Some(layer) if layer.hides_relation(identity) => Source::Hidden,
            _ => Source::Base,
        }
    }

    fn successor_relation_source(&self, identity: &GraphRelationId) -> Source {
        if self.rows.delta_relations.binary_search(identity).is_ok() {
            Source::Delta
        } else if self.rows.hidden_relations.binary_search(identity).is_ok() {
            Source::Hidden
        } else {
            Source::Base
        }
    }

    /// `(predecessor, successor)` content of one entity. A row both serve
    /// from the base is read once and reported equal.
    fn entity(
        &self,
        identity: &GraphEntityId,
    ) -> Result<(Option<EntityContent>, Option<EntityContent>), CodeGraphProjectionError> {
        let reference = GraphEntityRef::new(self.projection.clone(), identity.clone());
        let base = || -> Result<Option<EntityContent>, CodeGraphProjectionError> {
            self.snapshot
                .base_entity(&reference, Arc::clone(self.cancellation))?
                .as_ref()
                .map(EntityContent::decode)
                .transpose()
        };
        let old = match self.parent_entity_source(identity) {
            Source::Delta => {
                let layer = self
                    .parent
                    .layer
                    .as_ref()
                    .ok_or_else(|| corrupt("catalog layer delta row without a layer"))?;
                let recorded = layer.delta_entities.get(identity).ok_or_else(|| {
                    corrupt("catalog layer delta row without a recorded contribution")
                })?;
                Some(EntityContent::recorded(self.parent, identity, recorded)?)
            }
            Source::Hidden => None,
            Source::Base => base()?,
        };
        let new = match self.successor_entity_source(identity) {
            Source::Delta => self
                .snapshot
                .entity(&reference, Arc::clone(self.cancellation))?
                .as_ref()
                .map(EntityContent::decode)
                .transpose()?,
            Source::Hidden => None,
            Source::Base => match self.parent_entity_source(identity) {
                Source::Base => return Ok((None, None)),
                _ => base()?,
            },
        };
        Ok((old, new))
    }

    /// `(predecessor, successor)` content of one relation.
    fn relation(
        &self,
        identity: &GraphRelationId,
    ) -> Result<(Option<RelationContent>, Option<RelationContent>), CodeGraphProjectionError> {
        let reference = GraphRelationRef::new(self.projection.clone(), identity.clone());
        let base = || -> Result<Option<RelationContent>, CodeGraphProjectionError> {
            self.snapshot
                .base_relation(&reference, Arc::clone(self.cancellation))?
                .map(|relation| RelationContent::decode(storage_relation(relation)?))
                .transpose()
        };
        let old = match self.parent_relation_source(identity) {
            Source::Delta => {
                let layer = self
                    .parent
                    .layer
                    .as_ref()
                    .ok_or_else(|| corrupt("catalog layer delta row without a layer"))?;
                let recorded = layer.delta_relations.get(identity).ok_or_else(|| {
                    corrupt("catalog layer delta row without a recorded contribution")
                })?;
                Some(RelationContent::recorded(recorded))
            }
            Source::Hidden => None,
            Source::Base => base()?,
        };
        let new = match self.successor_relation_source(identity) {
            Source::Delta => self
                .snapshot
                .relation(&reference, Arc::clone(self.cancellation))?
                .map(|relation| RelationContent::decode(storage_relation(relation)?))
                .transpose()?,
            Source::Hidden => None,
            Source::Base => match self.parent_relation_source(identity) {
                Source::Base => return Ok((None, None)),
                _ => base()?,
            },
        };
        Ok((old, new))
    }
}

/// The successor's layer as the carry records it: what each delta row
/// contributes, so the next successor carries from this catalog in turn.
struct CatalogLayerScan<'a> {
    rows: &'a GraphLayeredRowsV1,
    delta_entities: BTreeMap<GraphEntityId, DeltaEntityV1>,
    delta_relations: BTreeMap<GraphRelationId, DeltaRelationV1>,
}

impl<'a> CatalogLayerScan<'a> {
    fn new(rows: &'a GraphLayeredRowsV1) -> Self {
        Self {
            rows,
            delta_entities: BTreeMap::new(),
            delta_relations: BTreeMap::new(),
        }
    }

    fn record_entity(&mut self, identity: &GraphEntityId, content: &EntityContent) {
        if self.rows.delta_entities.binary_search(identity).is_ok() {
            self.delta_entities
                .insert(identity.clone(), content.delta());
        }
    }

    fn record_relation(&mut self, identity: &GraphRelationId, content: &RelationContent) {
        if self.rows.delta_relations.binary_search(identity).is_ok() {
            self.delta_relations
                .insert(identity.clone(), content.delta());
        }
    }

    fn finish(self) -> CatalogLayerV1 {
        CatalogLayerV1 {
            base_generation: self.rows.base_generation.clone(),
            hidden_entities: self.rows.hidden_entities.clone().into_boxed_slice(),
            hidden_relations: self.rows.hidden_relations.clone().into_boxed_slice(),
            delta_entities: self.delta_entities.into(),
            delta_relations: self.delta_relations.into(),
        }
    }
}

/// Every lookup key one symbol record contributes to.
struct SymbolListKeys<'a> {
    qualified_name: Option<&'a str>,
    simple_name: Option<String>,
    file: Option<&'a FileOccurrenceId>,
    callee_names: BTreeSet<&'a str>,
    kind: Option<&'a str>,
    logical_path: Option<&'a str>,
}

impl SymbolRow {
    fn list_keys(&self) -> SymbolListKeys<'_> {
        SymbolListKeys {
            qualified_name: self
                .metadata
                .as_ref()
                .map(|metadata| metadata.qualified_name.as_str()),
            simple_name: self
                .metadata
                .as_ref()
                .map(|metadata| derived_simple_name(&metadata.qualified_name)),
            file: self.binding.as_ref().map(|binding| &binding.file),
            callee_names: self
                .unresolved_calls
                .iter()
                .map(|call| unresolved_callee_name(&call.reference_name))
                .collect(),
            kind: self
                .metadata
                .as_ref()
                .map(|metadata| metadata.kind.as_str()),
            logical_path: self
                .binding
                .as_ref()
                .and_then(|binding| binding.logical_path.as_deref()),
        }
    }
}

/// Membership changes of one lookup key's symbol list. An occurrence both
/// removed and added stays listed.
#[derive(Default)]
struct ListEdit {
    remove: BTreeSet<SymbolOccurrenceId>,
    add: BTreeSet<SymbolOccurrenceId>,
}

impl ListEdit {
    fn apply(&self, current: Option<&SymbolIds>) -> Option<SymbolIds> {
        let mut ids: BTreeSet<SymbolOccurrenceId> =
            current.into_iter().flatten().cloned().collect();
        for occurrence in self.remove.difference(&self.add) {
            ids.remove(occurrence);
        }
        ids.extend(self.add.iter().cloned());
        (!ids.is_empty()).then(|| frozen_ids(ids.into_iter().collect()))
    }
}

fn list_edits<K: Ord + Clone>(
    parent: &SortedMap<K, SymbolIds>,
    edits: BTreeMap<K, ListEdit>,
) -> BTreeMap<K, Option<SymbolIds>> {
    edits
        .into_iter()
        .map(|(key, edit)| {
            let next = edit.apply(parent.get(&key));
            (key, next)
        })
        .collect()
}

fn counted<K: Ord + Clone>(
    parent: &SortedMap<K, u64>,
    deltas: BTreeMap<K, i64>,
    what: &str,
) -> Result<BTreeMap<K, Option<u64>>, CodeGraphProjectionError> {
    deltas
        .into_iter()
        .map(|(key, delta)| {
            let current = parent.get(&key).copied().unwrap_or(0);
            let next = i128::from(current) + i128::from(delta);
            let next = u64::try_from(next).map_err(|_| {
                CodeGraphProjectionError::Corrupt(format!(
                    "code graph catalog carry drives a {what} count below zero"
                ))
            })?;
            Ok((key, (next != 0).then_some(next)))
        })
        .collect()
}

fn bump<K: Ord>(deltas: &mut BTreeMap<K, i64>, key: K, delta: i64) {
    *deltas.entry(key).or_default() += delta;
}

fn touch_list<K: Ord>(
    edits: &mut BTreeMap<K, ListEdit>,
    key: K,
    occurrence: &SymbolOccurrenceId,
    edit: Edit,
) {
    let list = edits.entry(key).or_default();
    match edit {
        Edit::Remove => list.remove.insert(occurrence.clone()),
        Edit::Add => list.add.insert(occurrence.clone()),
    };
}

#[derive(Clone, Copy)]
enum Edit {
    Remove,
    Add,
}

impl Edit {
    fn delta(self) -> i64 {
        match self {
            Self::Remove => -1,
            Self::Add => 1,
        }
    }
}

/// The predecessor catalog plus every edit the changed rows imply.
struct CatalogCarry<'a> {
    parent: &'a InteractiveCatalog,
    /// Final record of each touched symbol occurrence; `None` is removed.
    symbols: BTreeMap<SymbolOccurrenceId, Option<SymbolRow>>,
    /// `(outgoing, incoming)` change per endpoint, from the edges that differ.
    degree_deltas: BTreeMap<SymbolOccurrenceId, (i64, i64)>,
    files: BTreeMap<FileOccurrenceId, Option<SanitizedCodeFileV1>>,
    by_logical_path: BTreeMap<String, Option<FileOccurrenceId>>,
    imports_removed: BTreeMap<GraphEntityId, CodeIndexImportEvidenceV1>,
    imports_added: BTreeMap<GraphEntityId, CodeIndexImportEvidenceV1>,
    /// Import entity to its file link, `None` when the link left.
    import_links: BTreeMap<GraphEntityId, Option<GraphRelation>>,
    by_qualified_name: BTreeMap<String, ListEdit>,
    by_simple_name: BTreeMap<String, ListEdit>,
    by_file: BTreeMap<FileOccurrenceId, ListEdit>,
    unresolved_call_sources: BTreeMap<String, ListEdit>,
    unresolved_sources_by_entity: BTreeMap<GraphEntityId, Option<SymbolOccurrenceId>>,
    symbols_by_kind: BTreeMap<String, i64>,
    files_by_language: BTreeMap<String, i64>,
    symbols_by_logical_path: BTreeMap<String, i64>,
    dependency_edge_counts: BTreeMap<(String, String), i64>,
    dependency_edges: i64,
}

impl<'a> CatalogCarry<'a> {
    fn new(parent: &'a InteractiveCatalog) -> Self {
        Self {
            parent,
            symbols: BTreeMap::new(),
            degree_deltas: BTreeMap::new(),
            files: BTreeMap::new(),
            by_logical_path: BTreeMap::new(),
            imports_removed: BTreeMap::new(),
            imports_added: BTreeMap::new(),
            import_links: BTreeMap::new(),
            by_qualified_name: BTreeMap::new(),
            by_simple_name: BTreeMap::new(),
            by_file: BTreeMap::new(),
            unresolved_call_sources: BTreeMap::new(),
            unresolved_sources_by_entity: BTreeMap::new(),
            symbols_by_kind: BTreeMap::new(),
            files_by_language: BTreeMap::new(),
            symbols_by_logical_path: BTreeMap::new(),
            dependency_edge_counts: BTreeMap::new(),
            dependency_edges: 0,
        }
    }

    /// The logical path `occurrence` is bound to in the state the edits so
    /// far describe.
    fn current_logical_path(&self, occurrence: &SymbolOccurrenceId) -> Option<&str> {
        match self.symbols.get(occurrence) {
            Some(Some(row)) => row.binding.as_ref()?.logical_path.as_deref(),
            Some(None) => None,
            None => self.parent.bound_logical_path(occurrence),
        }
    }

    fn current_file(&self, file: &FileOccurrenceId) -> Option<&SanitizedCodeFileV1> {
        match self.files.get(file) {
            Some(file) => file.as_ref(),
            None => self.parent.files.get(file),
        }
    }

    fn remove_entity(&mut self, entity: &EntityContent) -> Result<(), CodeGraphProjectionError> {
        match entity {
            EntityContent::File(record) => {
                if self.parent.files.get(&record.file_occurrence_id) != Some(record) {
                    return Err(corrupt(
                        "code graph catalog carry removes a file its predecessor did not hold",
                    ));
                }
                if self.parent.by_logical_path.get(&record.logical_path)
                    != Some(&record.file_occurrence_id)
                {
                    return Err(corrupt(
                        "code graph catalog carry removes a file under a path it did not claim",
                    ));
                }
                self.by_logical_path
                    .insert(record.logical_path.clone(), None);
                if let Some(language) = &record.language {
                    bump(
                        &mut self.files_by_language,
                        language.as_str().to_owned(),
                        -1,
                    );
                }
                self.files.insert(record.file_occurrence_id.clone(), None);
            }
            EntityContent::Symbol {
                identity,
                occurrence,
                row,
            } => {
                let held = self.parent.symbols.get(occurrence).ok_or_else(|| {
                    corrupt(
                        "code graph catalog carry removes a symbol its predecessor did not hold",
                    )
                })?;
                if SymbolRow::of(held) != *row {
                    return Err(corrupt(
                        "code graph catalog carry removes a symbol record its predecessor did not hold",
                    ));
                }
                self.edit_symbol_lists(occurrence, row, Edit::Remove);
                if !row.unresolved_calls.is_empty() {
                    self.unresolved_sources_by_entity
                        .insert(identity.clone(), None);
                }
                self.symbols.insert(occurrence.clone(), None);
            }
            EntityContent::Import { identity, import } => {
                self.imports_removed
                    .insert(identity.clone(), import.clone());
            }
            EntityContent::Other => {}
        }
        Ok(())
    }

    fn add_entity(&mut self, entity: &EntityContent) -> Result<(), CodeGraphProjectionError> {
        match entity {
            EntityContent::File(record) => {
                if self.current_file(&record.file_occurrence_id).is_some() {
                    return Err(corrupt("code graph contains a duplicate file entity"));
                }
                let claimed = match self.by_logical_path.get(&record.logical_path) {
                    Some(claim) => claim.as_ref(),
                    None => self.parent.by_logical_path.get(&record.logical_path),
                };
                if claimed.is_some_and(|claim| *claim != record.file_occurrence_id) {
                    return Err(CodeGraphProjectionError::Corrupt(format!(
                        "code graph logical path `{}` is claimed by more than one file occurrence",
                        record.logical_path
                    )));
                }
                self.by_logical_path.insert(
                    record.logical_path.clone(),
                    Some(record.file_occurrence_id.clone()),
                );
                if let Some(language) = &record.language {
                    bump(&mut self.files_by_language, language.as_str().to_owned(), 1);
                }
                self.files
                    .insert(record.file_occurrence_id.clone(), Some(record.clone()));
            }
            EntityContent::Symbol {
                identity,
                occurrence,
                row,
            } => {
                let present = match self.symbols.get(occurrence) {
                    Some(row) => row.is_some(),
                    None => self.parent.symbols.get(occurrence).is_some(),
                };
                if present {
                    return Err(corrupt("code graph contains a duplicate symbol entity"));
                }
                self.edit_symbol_lists(occurrence, row, Edit::Add);
                if !row.unresolved_calls.is_empty() {
                    self.unresolved_sources_by_entity
                        .insert(identity.clone(), Some(occurrence.clone()));
                }
                self.symbols.insert(occurrence.clone(), Some(row.clone()));
            }
            EntityContent::Import { identity, import } => {
                if self
                    .imports_added
                    .insert(identity.clone(), import.clone())
                    .is_some()
                {
                    return Err(corrupt("code graph contains a duplicate import entity"));
                }
            }
            EntityContent::Other => {}
        }
        Ok(())
    }

    fn edit_symbol_lists(&mut self, occurrence: &SymbolOccurrenceId, row: &SymbolRow, edit: Edit) {
        let keys = row.list_keys();
        let delta = edit.delta();
        if let Some(qualified_name) = keys.qualified_name {
            touch_list(
                &mut self.by_qualified_name,
                qualified_name.to_owned(),
                occurrence,
                edit,
            );
        }
        if let Some(simple_name) = keys.simple_name {
            touch_list(&mut self.by_simple_name, simple_name, occurrence, edit);
        }
        if let Some(file) = keys.file {
            touch_list(&mut self.by_file, file.clone(), occurrence, edit);
        }
        for callee in keys.callee_names {
            touch_list(
                &mut self.unresolved_call_sources,
                callee.to_owned(),
                occurrence,
                edit,
            );
        }
        if let Some(kind) = keys.kind {
            bump(&mut self.symbols_by_kind, kind.to_owned(), delta);
        }
        if let Some(logical_path) = keys.logical_path {
            bump(
                &mut self.symbols_by_logical_path,
                logical_path.to_owned(),
                delta,
            );
        }
    }

    fn remove_relation(&mut self, relation: &RelationContent) {
        match relation {
            RelationContent::ImportLink { import, .. } => {
                self.import_links.insert(import.clone(), None);
            }
            RelationContent::Edge(DeltaRelationV1::Edge { kind, from, to }) => {
                if is_dependency_kind(*kind) {
                    self.dependency_edges -= 1;
                    // The edge left with the predecessor's bindings in force.
                    if let (Some(source), Some(target)) = (
                        self.parent.bound_logical_path(from),
                        self.parent.bound_logical_path(to),
                    ) && source != target
                    {
                        bump(
                            &mut self.dependency_edge_counts,
                            (source.to_owned(), target.to_owned()),
                            -1,
                        );
                    }
                }
                self.degree_deltas.entry(from.clone()).or_default().0 -= 1;
                self.degree_deltas.entry(to.clone()).or_default().1 -= 1;
            }
            RelationContent::Edge(_) | RelationContent::Other => {}
        }
    }

    fn add_relation(&mut self, relation: &RelationContent) {
        match relation {
            RelationContent::ImportLink { import, relation } => {
                self.import_links.insert(import.clone(), relation.clone());
            }
            RelationContent::Edge(DeltaRelationV1::Edge { kind, from, to }) => {
                if is_dependency_kind(*kind) {
                    self.dependency_edges += 1;
                    let pair = match (
                        self.current_logical_path(from),
                        self.current_logical_path(to),
                    ) {
                        (Some(source), Some(target)) if source != target => {
                            Some((source.to_owned(), target.to_owned()))
                        }
                        _ => None,
                    };
                    if let Some(pair) = pair {
                        bump(&mut self.dependency_edge_counts, pair, 1);
                    }
                }
                self.degree_deltas.entry(from.clone()).or_default().0 += 1;
                self.degree_deltas.entry(to.clone()).or_default().1 += 1;
            }
            RelationContent::Edge(_) | RelationContent::Other => {}
        }
    }

    fn finish(
        self,
        generation: GraphGenerationId,
        layer: CatalogLayerV1,
        projection_node_count: usize,
    ) -> Result<InteractiveCatalog, CodeGraphProjectionError> {
        let parent = self.parent;
        let imports = self.carried_imports()?;

        let mut semantic_edges = i128::from(parent.semantic_edges);
        let mut symbol_edits = BTreeMap::new();
        let touched: BTreeSet<&SymbolOccurrenceId> = self
            .symbols
            .keys()
            .chain(self.degree_deltas.keys())
            .collect();
        for occurrence in touched {
            let held = parent.symbols.get(occurrence);
            let row = match self.symbols.get(occurrence) {
                Some(None) => {
                    semantic_edges -= i128::from(held.map_or(0, |held| held.outgoing));
                    symbol_edits.insert(occurrence.clone(), None);
                    continue;
                }
                Some(Some(row)) => Some(row),
                None => None,
            };
            let (outgoing, incoming) = held.map_or((0, 0), |held| (held.outgoing, held.incoming));
            let (outgoing_delta, incoming_delta) = self
                .degree_deltas
                .get(occurrence)
                .copied()
                .unwrap_or((0, 0));
            let degree = |current: u64, delta: i64| {
                u64::try_from(i128::from(current) + i128::from(delta)).map_err(|_| {
                    corrupt("code graph catalog carry drives a symbol degree below zero")
                })
            };
            let symbol = match (row, held) {
                (Some(row), _) => CatalogSymbol {
                    binding: row.binding.clone(),
                    metadata: row.metadata.clone(),
                    unresolved_calls: row.unresolved_calls.clone(),
                    outgoing: degree(outgoing, outgoing_delta)?,
                    incoming: degree(incoming, incoming_delta)?,
                },
                (None, Some(held)) => CatalogSymbol {
                    outgoing: degree(outgoing, outgoing_delta)?,
                    incoming: degree(incoming, incoming_delta)?,
                    ..held.clone()
                },
                (None, None) => {
                    return Err(corrupt(
                        "code graph relation endpoint is not a symbol entity",
                    ));
                }
            };
            semantic_edges += i128::from(symbol.outgoing) - i128::from(outgoing);
            symbol_edits.insert(occurrence.clone(), Some(symbol));
        }
        let semantic_edges = u64::try_from(semantic_edges)
            .map_err(|_| corrupt("code graph catalog carry drives the edge census below zero"))?;

        let files = parent.files.edited(self.files);
        let symbols_by_logical_path = parent.symbols_by_logical_path.edited(counted(
            &parent.symbols_by_logical_path,
            self.symbols_by_logical_path,
            "file symbol",
        )?);
        let dependency_edge_counts = parent.dependency_edge_counts.edited(counted(
            &parent.dependency_edge_counts,
            self.dependency_edge_counts,
            "file dependency",
        )?);
        let dependency_edges = u64::try_from(
            i128::from(parent.file_dependencies.dependency_edges)
                + i128::from(self.dependency_edges),
        )
        .map_err(|_| corrupt("code graph catalog carry drives the dependency census below zero"))?;
        let file_dependencies =
            fold_file_dependencies(&dependency_edge_counts, &files, dependency_edges);
        let catalog = InteractiveCatalog {
            generation,
            layer: Some(layer),
            symbols: parent.symbols.edited(symbol_edits),
            by_qualified_name: parent.by_qualified_name.edited(list_edits(
                &parent.by_qualified_name,
                self.by_qualified_name,
            )),
            by_simple_name: parent
                .by_simple_name
                .edited(list_edits(&parent.by_simple_name, self.by_simple_name)),
            by_file: parent
                .by_file
                .edited(list_edits(&parent.by_file, self.by_file)),
            by_logical_path: parent.by_logical_path.edited(self.by_logical_path),
            files,
            imports,
            unresolved_call_sources: parent.unresolved_call_sources.edited(list_edits(
                &parent.unresolved_call_sources,
                self.unresolved_call_sources,
            )),
            unresolved_sources_by_entity: parent
                .unresolved_sources_by_entity
                .edited(self.unresolved_sources_by_entity),
            symbols_by_kind: parent.symbols_by_kind.edited(counted(
                &parent.symbols_by_kind,
                self.symbols_by_kind,
                "symbol kind",
            )?),
            files_by_language: parent.files_by_language.edited(counted(
                &parent.files_by_language,
                self.files_by_language,
                "file language",
            )?),
            largest_files: rank_largest_files(&symbols_by_logical_path),
            symbols_by_logical_path,
            semantic_edges,
            dependency_edge_counts,
            file_dependencies,
            heap: None,
        };
        // Every entity is a file, a symbol, an import, or the generation
        // marker: what a scan of the successor would have counted.
        let carried_nodes = catalog
            .files
            .len()
            .saturating_add(catalog.symbols.len())
            .saturating_add(catalog.imports.len())
            .saturating_add(1);
        if carried_nodes != projection_node_count {
            return Err(corrupt(
                "code graph catalog carry does not match the declared projection node count",
            ));
        }
        Ok(catalog)
    }

    /// The predecessor's imports less the removed ones plus the added ones,
    /// each added import's file link proven, in canonical order.
    fn carried_imports(&self) -> Result<Vec<CodeIndexImportEvidenceV1>, CodeGraphProjectionError> {
        for (identity, link) in &self.import_links {
            let consistent = match link {
                Some(_) => self.imports_added.contains_key(identity),
                None => self.imports_removed.contains_key(identity),
            };
            if !consistent {
                return Err(corrupt(
                    "code graph file-import relation targets a non-import entity",
                ));
            }
        }
        for (identity, import) in &self.imports_added {
            validate_import_link(
                import,
                self.current_file(&import.file_occurrence_id),
                self.import_links.get(identity).and_then(Option::as_ref),
            )?;
        }
        if self
            .imports_removed
            .keys()
            .any(|identity| !self.import_links.contains_key(identity))
        {
            return Err(corrupt(
                "code graph import entity left without its file link",
            ));
        }
        let mut removed: Vec<&CodeIndexImportEvidenceV1> = self.imports_removed.values().collect();
        removed.sort_by(|left, right| canonical_import_order(left, right));
        let mut matched = 0_usize;
        let mut imports: Vec<CodeIndexImportEvidenceV1> =
            Vec::with_capacity(self.parent.imports.len() + self.imports_added.len());
        for import in &self.parent.imports {
            let gone = removed
                .binary_search_by(|candidate| canonical_import_order(candidate, import))
                .is_ok_and(|index| removed[index] == import);
            if gone {
                matched += 1;
            } else {
                imports.push(import.clone());
            }
        }
        if matched != removed.len() {
            return Err(corrupt(
                "code graph catalog carry removes an import its predecessor did not hold",
            ));
        }
        imports.extend(self.imports_added.values().cloned());
        imports.sort_by(canonical_import_order);
        Ok(imports)
    }
}
