//! Allocation bounds for the buffers, edits, and result of a catalog carry.

use std::collections::BTreeMap;
use std::mem::size_of;

use tracedecay_domain::{FileOccurrenceId, SanitizedCodeFileV1, SymbolOccurrenceId};
use tracedecay_graph_db::{
    GraphEntityId, GraphLayeredRowsV1, GraphProperty, GraphPropertyName, GraphRelation,
    GraphRelationId,
};

use super::super::super::CodeGraphProjectionError;
use super::super::build_charge::CatalogBuildChargeV1;
use super::super::catalog::is_dependency_kind;
use super::super::models::{
    CatalogSymbol, DeltaEntityV1, DeltaRelationV1, InteractiveCatalog, SortedMap, SymbolIds,
    aggregate_allocation_bytes, binding_heap_bytes, unresolved_callee_name,
};
use super::{CatalogCarry, EntityContent, ListEdit, RelationContent, SymbolRow, corrupt};
use crate::chunks::CodeIndexImportEvidenceV1;
use crate::production::resident_bytes::{
    btree_bytes, import_heap_bytes, snapshot_file_heap_bytes, symbol_heap_bytes,
    unresolved_heap_bytes, vec_bytes,
};

/// The carry retains changed rows until every removal and addition has been
/// applied. Reserve their buffers first, then admit each row's owned heap and
/// the edits it can create before cloning it into the layer or edit maps.
/// Repeated keys deliberately count as distinct: replacement can only reduce
/// this bound, and calculating it needs no second index of the changed rows.
pub(super) struct CarryBuildBytes<'a> {
    rows: &'a GraphLayeredRowsV1,
    fixed: usize,
    heap: usize,
    edits: EditBytes,
    max_path: usize,
}

fn ids_bytes<'a, T: 'a>(ids: impl Iterator<Item = &'a T>, len: impl Fn(&T) -> usize) -> usize {
    ids.fold(0_usize, |bytes, id| {
        bytes.saturating_add(size_of::<T>()).saturating_add(len(id))
    })
}

/// Count the lowercased key without allocating the key inside its own bound.
/// String growth can retain twice the complete output capacity.
fn simple_name_bytes(qualified_name: &str) -> usize {
    let tail = qualified_name.rsplit("::").next().unwrap_or(qualified_name);
    let tail = tail.rsplit('.').next().unwrap_or(tail);
    let lowered = tail
        .chars()
        .flat_map(char::to_lowercase)
        .map(char::len_utf8)
        .fold(0_usize, usize::saturating_add);
    tail.len().max(lowered).saturating_mul(2)
}

impl<'a> CarryBuildBytes<'a> {
    pub(super) fn new(parent: &InteractiveCatalog, rows: &'a GraphLayeredRowsV1) -> Self {
        let entity_len = |id: &GraphEntityId| id.as_str().len();
        let relation_len = |id: &GraphRelationId| id.as_str().len();
        let row_entities = rows.hidden_entities.iter().chain(&rows.delta_entities);
        let row_relations = rows.hidden_relations.iter().chain(&rows.delta_relations);
        let row_ids = ids_bytes(row_entities, entity_len)
            .saturating_add(ids_bytes(row_relations, relation_len));
        let row_storage = vec_bytes(
            &rows.hidden_entities,
            rows.hidden_entities.capacity(),
            entity_len,
        )
        .saturating_add(vec_bytes(
            &rows.delta_entities,
            rows.delta_entities.capacity(),
            entity_len,
        ))
        .saturating_add(vec_bytes(
            &rows.hidden_relations,
            rows.hidden_relations.capacity(),
            relation_len,
        ))
        .saturating_add(vec_bytes(
            &rows.delta_relations,
            rows.delta_relations.capacity(),
            relation_len,
        ));
        let mut parent_ids = 0_usize;
        let mut parent_delta_ids = 0_usize;
        if let Some(layer) = &parent.layer {
            parent_delta_ids = ids_bytes(layer.delta_entities.keys(), entity_len)
                .saturating_add(ids_bytes(layer.delta_relations.keys(), relation_len));
            parent_ids = parent_delta_ids
                .saturating_add(ids_bytes(layer.hidden_entities.iter(), entity_len))
                .saturating_add(ids_bytes(layer.hidden_relations.iter(), relation_len));
        }
        let hidden_ids = ids_bytes(rows.hidden_entities.iter(), entity_len)
            .saturating_add(ids_bytes(rows.hidden_relations.iter(), relation_len));
        let fixed = usize::try_from(parent.retained_bytes())
            .unwrap_or(usize::MAX)
            // The read layer, merged ID lists, and temporary parent-key lists.
            .saturating_add(row_storage)
            .saturating_add(row_ids)
            .saturating_add(parent_ids)
            .saturating_add(parent_delta_ids)
            // The layer's trees and their eventual frozen arrays/hidden IDs.
            .saturating_add(btree_bytes(
                rows.delta_entities.len(),
                size_of::<(GraphEntityId, DeltaEntityV1)>(),
            ))
            .saturating_add(btree_bytes(
                rows.delta_relations.len(),
                size_of::<(GraphRelationId, DeltaRelationV1)>(),
            ))
            .saturating_add(
                rows.delta_entities
                    .len()
                    .saturating_mul(size_of::<(GraphEntityId, DeltaEntityV1)>()),
            )
            .saturating_add(
                rows.delta_relations
                    .len()
                    .saturating_mul(size_of::<(GraphRelationId, DeltaRelationV1)>()),
            )
            .saturating_add(hidden_ids)
            .saturating_add(rows.base_generation.as_str().len().saturating_mul(2));
        Self {
            rows,
            fixed,
            heap: 0,
            edits: EditBytes::default(),
            max_path: parent
                .symbols
                .values()
                .filter_map(|symbol| symbol.binding.as_ref()?.logical_path.as_ref())
                .map(String::len)
                .max()
                .unwrap_or(0),
        }
    }

    pub(super) fn held_bytes(&self) -> usize {
        self.fixed
            .saturating_add(self.heap)
            .saturating_add(self.edits.bytes())
    }

    pub(super) fn reserve_buffer<T>(
        &mut self,
        buffer: &mut Vec<T>,
        charge: &mut CatalogBuildChargeV1,
    ) -> Result<(), CodeGraphProjectionError> {
        if buffer.len() < buffer.capacity() {
            return Ok(());
        }
        let old = buffer.capacity();
        let capacity = old.saturating_mul(2).max(4);
        self.fixed = self
            .fixed
            .saturating_add(capacity.saturating_sub(old).saturating_mul(size_of::<T>()));
        // Reallocation may hold the old slab alongside the new one. The
        // old slab is transient; only the resulting capacity stays live.
        charge.cover(
            self.held_bytes()
                .saturating_add(old.saturating_mul(size_of::<T>())),
        )?;
        buffer.reserve_exact(capacity.saturating_sub(buffer.len()));
        Ok(())
    }

    pub(super) fn record_entity(
        &mut self,
        identity: &GraphEntityId,
        old: Option<&EntityContent>,
        new: Option<&EntityContent>,
    ) -> usize {
        let heap = old
            .map_or(0, entity_heap)
            .saturating_add(new.map_or(0, entity_heap));
        if let Some(new) = new
            && self.rows.delta_entities.binary_search(identity).is_ok()
        {
            let delta = match new {
                EntityContent::File(file) => file.file_occurrence_id.as_str().len(),
                EntityContent::Symbol { occurrence, .. } => occurrence.as_str().len(),
                EntityContent::Import { import, .. } => import_heap_bytes(import),
                EntityContent::Other => 0,
            };
            self.heap = self
                .heap
                .saturating_add(identity.as_str().len())
                .saturating_add(delta);
        }
        if old != new {
            self.heap = self.heap.saturating_add(heap);
            for entity in [old, new].into_iter().flatten() {
                self.edits.entity(entity);
                if let EntityContent::Symbol { row, .. } = entity
                    && let Some(path) = row
                        .binding
                        .as_ref()
                        .and_then(|binding| binding.logical_path.as_ref())
                {
                    self.max_path = self.max_path.max(path.len());
                }
            }
            self.held_bytes()
        } else {
            // Equal rows are temporary, but coexist with previously kept rows.
            self.held_bytes().saturating_add(heap)
        }
    }

    pub(super) fn record_relation(
        &mut self,
        identity: &GraphRelationId,
        old: Option<&RelationContent>,
        new: Option<&RelationContent>,
    ) -> usize {
        let heap = old
            .map_or(0, relation_heap)
            .saturating_add(new.map_or(0, relation_heap));
        if let Some(new) = new
            && self.rows.delta_relations.binary_search(identity).is_ok()
        {
            let delta = match new {
                RelationContent::Edge(DeltaRelationV1::Edge { from, to, .. }) => {
                    from.as_str().len().saturating_add(to.as_str().len())
                }
                RelationContent::ImportLink { import, .. } => import.as_str().len(),
                RelationContent::Edge(_) | RelationContent::Other => 0,
            };
            self.heap = self
                .heap
                .saturating_add(identity.as_str().len())
                .saturating_add(delta);
        }
        if old != new {
            self.heap = self.heap.saturating_add(heap);
            for relation in [old, new].into_iter().flatten() {
                self.edits.relation(relation, self.max_path);
            }
            self.held_bytes()
        } else {
            self.held_bytes().saturating_add(heap)
        }
    }

    /// The parent clone is already in `fixed`. A second copy of the admitted
    /// edit storage covers newly added result records and intermediate edit
    /// maps while the original edits remain live. Parent list conversions and
    /// degree-only symbol edits need their own bounds, since they can touch a
    /// large parent record without that record appearing in the changed rows.
    pub(super) fn finish_bytes(
        &self,
        carry: &CatalogCarry<'_>,
    ) -> Result<usize, CodeGraphProjectionError> {
        fn list_peak<K: Ord>(
            parent: &SortedMap<K, SymbolIds>,
            edits: &BTreeMap<K, ListEdit>,
        ) -> usize {
            edits
                .iter()
                .map(|(key, edit)| {
                    let current = parent.get(key).map_or(&[][..], |ids| &ids[..]);
                    let count = current.len().saturating_add(edit.add.len());
                    count
                        .saturating_mul(size_of::<SymbolOccurrenceId>())
                        .saturating_add(
                            current
                                .iter()
                                .chain(&edit.add)
                                .map(|id| id.as_str().len())
                                .fold(0_usize, usize::saturating_add),
                        )
                })
                .max()
                .unwrap_or(0)
        }
        let parent = carry.parent;
        let lists = list_peak(&parent.by_qualified_name, &carry.by_qualified_name)
            .max(list_peak(&parent.by_simple_name, &carry.by_simple_name))
            .max(list_peak(&parent.by_file, &carry.by_file))
            .max(list_peak(
                &parent.unresolved_call_sources,
                &carry.unresolved_call_sources,
            ));
        let touched = carry
            .symbols
            .len()
            .saturating_add(carry.degree_deltas.len());
        let mut parent_pairs = parent.dependency_edge_counts.keys().peekable();
        let mut edit_pairs = carry.dependency_edge_counts.keys().peekable();
        let pairs = std::iter::from_fn(|| match (parent_pairs.peek(), edit_pairs.peek()) {
            (Some(parent), Some(edit)) if parent <= edit => parent_pairs.next(),
            (Some(_), None) => parent_pairs.next(),
            (_, Some(_)) => edit_pairs.next(),
            (None, None) => None,
        });
        let aggregates = aggregate_allocation_bytes(
            parent
                .files
                .values()
                .chain(carry.files.values().filter_map(Option::as_ref)),
            parent
                .symbols_by_logical_path
                .keys()
                .chain(carry.symbols_by_logical_path.keys()),
            pairs,
        )?;
        Ok(self
            .edits
            .bytes()
            .saturating_add(lists)
            .saturating_add(btree_bytes(touched, size_of::<&SymbolOccurrenceId>()))
            .saturating_add(btree_bytes(
                touched,
                size_of::<(SymbolOccurrenceId, Option<CatalogSymbol>)>(),
            ))
            .saturating_add(
                parent
                    .symbols
                    .len()
                    .saturating_add(carry.symbols.len())
                    .saturating_mul(size_of::<usize>()),
            )
            .saturating_add(
                parent
                    .imports
                    .len()
                    .saturating_add(carry.imports_added.len())
                    .saturating_mul(size_of::<CodeIndexImportEvidenceV1>()),
            )
            .saturating_add(
                carry
                    .imports_removed
                    .len()
                    .saturating_mul(2 * size_of::<&CodeIndexImportEvidenceV1>()),
            )
            .saturating_add(aggregates))
    }
}

fn symbol_row_heap(row: &SymbolRow) -> usize {
    row.binding
        .as_ref()
        .map_or(0, binding_heap_bytes)
        .saturating_add(row.metadata.as_ref().map_or(0, symbol_heap_bytes))
        .saturating_add(vec_bytes(
            &row.unresolved_calls,
            row.unresolved_calls.capacity(),
            unresolved_heap_bytes,
        ))
}

pub(super) fn entity_heap(entity: &EntityContent) -> usize {
    match entity {
        EntityContent::File(file) => snapshot_file_heap_bytes(file),
        EntityContent::Symbol {
            identity,
            occurrence,
            row,
        } => identity
            .as_str()
            .len()
            .saturating_add(occurrence.as_str().len())
            .saturating_add(size_of::<SymbolRow>())
            .saturating_add(symbol_row_heap(row)),
        EntityContent::Import { identity, import } => identity
            .as_str()
            .len()
            .saturating_add(import_heap_bytes(import)),
        EntityContent::Other => 0,
    }
}

pub(super) fn recorded_entity_heap(
    parent: &InteractiveCatalog,
    identity: &GraphEntityId,
    entity: &DeltaEntityV1,
) -> Result<usize, CodeGraphProjectionError> {
    Ok(match entity {
        DeltaEntityV1::File(file) => snapshot_file_heap_bytes(
            parent
                .files
                .get(file)
                .ok_or_else(|| corrupt("catalog layer names a file the catalog lacks"))?,
        ),
        DeltaEntityV1::Symbol(occurrence) => {
            let symbol = parent
                .symbols
                .get(occurrence)
                .ok_or_else(|| corrupt("catalog layer names a symbol the catalog lacks"))?;
            identity
                .as_str()
                .len()
                .saturating_add(occurrence.as_str().len())
                .saturating_add(size_of::<SymbolRow>())
                .saturating_add(symbol.binding.as_ref().map_or(0, binding_heap_bytes))
                .saturating_add(symbol.metadata.as_ref().map_or(0, symbol_heap_bytes))
                .saturating_add(vec_bytes(
                    &symbol.unresolved_calls,
                    symbol.unresolved_calls.capacity(),
                    unresolved_heap_bytes,
                ))
        }
        DeltaEntityV1::Import(import) => identity
            .as_str()
            .len()
            .saturating_add(import_heap_bytes(import)),
        DeltaEntityV1::Other => 0,
    })
}

pub(super) fn recorded_relation_heap(relation: &DeltaRelationV1) -> usize {
    match relation {
        DeltaRelationV1::Edge { from, to, .. } => {
            from.as_str().len().saturating_add(to.as_str().len())
        }
        DeltaRelationV1::ImportLink { import } => import.as_str().len(),
        DeltaRelationV1::Other => 0,
    }
}

fn graph_relation_heap(relation: &GraphRelation) -> usize {
    relation.properties.iter().fold(
        btree_bytes(
            relation.properties.len(),
            size_of::<(GraphPropertyName, GraphProperty)>(),
        )
        .saturating_add(relation.identity.as_str().len())
        .saturating_add(relation.from.as_str().len())
        .saturating_add(relation.to.as_str().len())
        .saturating_add(relation.kind.as_str().len()),
        |bytes, (name, property)| {
            bytes
                .saturating_add(name.as_str().len())
                .saturating_add(match property {
                    GraphProperty::String(value) => value.capacity(),
                    GraphProperty::Bytes(value) => value.capacity(),
                    GraphProperty::Bool(_) | GraphProperty::I64(_) | GraphProperty::F64(_) => 0,
                })
        },
    )
}

pub(super) fn relation_heap(relation: &RelationContent) -> usize {
    match relation {
        RelationContent::Edge(DeltaRelationV1::Edge { from, to, .. }) => {
            from.as_str().len().saturating_add(to.as_str().len())
        }
        RelationContent::ImportLink { import, relation } => import
            .as_str()
            .len()
            .saturating_add(relation.as_ref().map_or(0, graph_relation_heap)),
        RelationContent::Edge(_) | RelationContent::Other => 0,
    }
}

#[derive(Clone, Copy)]
enum EditMap {
    Symbols,
    Degrees,
    Files,
    Paths,
    ImportLinks,
    Qualified,
    Simple,
    ByFile,
    UnresolvedCalls,
    UnresolvedEntities,
    Kinds,
    Languages,
    SymbolPaths,
    Dependencies,
}

#[derive(Default)]
struct EditBytes {
    trees: [(usize, usize); EditMap::Dependencies as usize + 1],
    heap: usize,
}

impl EditBytes {
    fn tree<K, V>(&mut self, map: EditMap, count: usize) {
        let tree = &mut self.trees[map as usize];
        tree.0 = tree.0.saturating_add(count);
        tree.1 = size_of::<(K, V)>();
    }

    fn bytes(&self) -> usize {
        self.trees
            .iter()
            .fold(self.heap, |bytes, &(entries, entry_bytes)| {
                bytes.saturating_add(btree_bytes(entries, entry_bytes))
            })
    }

    fn list<K>(&mut self, map: EditMap, key_bytes: usize, occurrence: &SymbolOccurrenceId) {
        self.tree::<K, ListEdit>(map, 1);
        // Each distinct list owns its own add/remove BTreeSets. One list per
        // edit bounds their sparse roots; applying them later builds a Vec.
        self.heap = self
            .heap
            .saturating_add(key_bytes)
            .saturating_add(btree_bytes(1, size_of::<SymbolOccurrenceId>()))
            .saturating_add(occurrence.as_str().len());
    }

    fn entity(&mut self, entity: &EntityContent) {
        match entity {
            EntityContent::File(file) => {
                self.tree::<FileOccurrenceId, Option<SanitizedCodeFileV1>>(EditMap::Files, 1);
                self.tree::<String, Option<FileOccurrenceId>>(EditMap::Paths, 1);
                self.heap = self
                    .heap
                    .saturating_add(snapshot_file_heap_bytes(file))
                    .saturating_add(file.file_occurrence_id.as_str().len().saturating_mul(2))
                    .saturating_add(file.logical_path.len());
                if let Some(language) = &file.language {
                    self.tree::<String, i64>(EditMap::Languages, 1);
                    self.heap = self.heap.saturating_add(language.as_str().len());
                }
            }
            EntityContent::Symbol {
                identity,
                occurrence,
                row,
            } => {
                self.tree::<SymbolOccurrenceId, Option<SymbolRow>>(EditMap::Symbols, 1);
                self.heap = self
                    .heap
                    .saturating_add(occurrence.as_str().len())
                    .saturating_add(symbol_row_heap(row));
                if let Some(metadata) = &row.metadata {
                    self.list::<String>(
                        EditMap::Qualified,
                        metadata.qualified_name.len(),
                        occurrence,
                    );
                    self.list::<String>(
                        EditMap::Simple,
                        simple_name_bytes(&metadata.qualified_name),
                        occurrence,
                    );
                    self.tree::<String, i64>(EditMap::Kinds, 1);
                    self.heap = self.heap.saturating_add(metadata.kind.len());
                }
                if let Some(binding) = &row.binding {
                    self.list::<FileOccurrenceId>(
                        EditMap::ByFile,
                        binding.file.as_str().len(),
                        occurrence,
                    );
                    if let Some(path) = &binding.logical_path {
                        self.tree::<String, i64>(EditMap::SymbolPaths, 1);
                        self.heap = self.heap.saturating_add(path.len());
                    }
                }
                if !row.unresolved_calls.is_empty() {
                    self.tree::<GraphEntityId, Option<SymbolOccurrenceId>>(
                        EditMap::UnresolvedEntities,
                        1,
                    );
                    self.heap = self
                        .heap
                        .saturating_add(identity.as_str().len())
                        .saturating_add(occurrence.as_str().len());
                }
                for call in &row.unresolved_calls {
                    self.list::<String>(
                        EditMap::UnresolvedCalls,
                        unresolved_callee_name(&call.reference_name).len(),
                        occurrence,
                    );
                }
                self.heap = self
                    .heap
                    .saturating_add(btree_bytes(row.unresolved_calls.len(), size_of::<&str>()));
            }
            EntityContent::Import { identity, import } => {
                // Removed and added imports are separate maps, each of which
                // may have a sparsely populated root.
                self.heap = self
                    .heap
                    .saturating_add(btree_bytes(
                        1,
                        size_of::<(GraphEntityId, CodeIndexImportEvidenceV1)>(),
                    ))
                    .saturating_add(identity.as_str().len())
                    .saturating_add(import_heap_bytes(import));
            }
            EntityContent::Other => {}
        }
    }

    fn relation(&mut self, relation: &RelationContent, max_path: usize) {
        match relation {
            RelationContent::Edge(DeltaRelationV1::Edge { kind, from, to }) => {
                self.tree::<SymbolOccurrenceId, (i64, i64)>(EditMap::Degrees, 2);
                self.heap = self
                    .heap
                    .saturating_add(from.as_str().len())
                    .saturating_add(to.as_str().len());
                if is_dependency_kind(*kind) {
                    self.tree::<(String, String), i64>(EditMap::Dependencies, 1);
                    self.heap = self.heap.saturating_add(max_path.saturating_mul(2));
                }
            }
            RelationContent::ImportLink { import, relation } => {
                self.tree::<GraphEntityId, Option<GraphRelation>>(EditMap::ImportLinks, 1);
                self.heap = self
                    .heap
                    .saturating_add(import.as_str().len())
                    .saturating_add(relation.as_ref().map_or(0, graph_relation_heap));
            }
            RelationContent::Edge(_) | RelationContent::Other => {}
        }
    }
}
