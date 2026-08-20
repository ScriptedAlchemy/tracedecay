use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::chunks::CodeIndexImportEvidenceV1;
use crate::lineage::{GenerationSymbolIndexV1, LineageSymbolRecordV1};
use crate::production::CodeIndexPublishedGenerationV1;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    CanonicalRelationEdgeV1, ChunkerRevision, CodeGenerationId, CodeSearchChunkAnchorV1,
    CodeSearchChunkId, CodeSearchChunkV1, ContentDigest, FileOccurrenceId,
    LanguageDescriptorRevision, SanitizedCodeFileV1, SanitizerRevision, SensitivityDecision,
    SymbolOccurrenceId,
};
use tracedecay_graph_db::{
    GraphDbError, GraphEntity, GraphEntityId, GraphEntityRef, GraphGenerationManifest,
    GraphGenerationRelation, GraphLabel, GraphProjectionIdentity, GraphProjectorRevision,
    GraphProperty, GraphPropertyName, GraphRelationId, GraphRelationKind, GraphWatermark,
};

use super::schema::{
    FILE_IMPORT_EDGE_KIND, FILE_LABEL, FILE_RECORD_PROPERTY, IMPORT_LABEL, IMPORT_RECORD_PROPERTY,
    file_entity_id, file_import_relation_id_with, import_entity_id, serialize, stable_identity,
};
use super::{
    CHUNK_LABEL, CHUNK_RECORD_PROPERTY, CHUNK_SYMBOL_EDGE_KIND, CodeGraphProjectionError,
    CodeGraphSymbolBindingV1, EDGE_LABEL, EDGE_RECORD_PROPERTY, FILE_SYMBOL_EDGE_KIND,
    SOURCE_EDGE_KIND, SymbolRecordV1, TARGET_EDGE_KIND, build_code_graph_manifest_inputs_checked,
    compare_edges, current_generation_entity, symbol_entity, symbol_entity_id, validate_edge,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ChunkRecordV1 {
    id: CodeSearchChunkId,
    anchor: CodeSearchChunkAnchorV1,
    content_digest: ContentDigest,
    language_descriptor_revision: LanguageDescriptorRevision,
    chunker_revision: ChunkerRevision,
    sanitizer_revision: SanitizerRevision,
    sensitivity: SensitivityDecision,
}

pub fn build_published_code_graph_manifest_checked(
    projection: GraphProjectionIdentity,
    generation: &CodeIndexPublishedGenerationV1,
    projector_revision: &GraphProjectorRevision,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<GraphGenerationManifest, CodeGraphProjectionError> {
    check()?;
    generation
        .validate()
        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
    let generation_id = &generation.manifest().generation_id;
    if generation.symbols().generation_id != *generation_id {
        return Err(CodeGraphProjectionError::GenerationMismatch);
    }
    // A published generation is immutable, so this manifest is a pure function
    // of (generation, projection identity, projector revision). Seat retries
    // and the seat/reconcile duplicate publication of one sealed generation
    // reuse the first complete build instead of re-serializing and re-hashing
    // every entity and relation. Fail-closed: only a fully successful build is
    // memoized — an interrupted or deadline-exceeded build records nothing —
    // and the `check` above refuses a cancelled or expired request before a
    // memo hit can be served.
    if let Some(manifest) = generation.memoized_graph_manifest(&projection, projector_revision) {
        return Ok((*manifest).clone());
    }
    let manifest = build_code_graph_manifest_inputs_checked(
        projection.clone(),
        generation_id,
        generation.edges(),
        generation.chunks().chunks(),
        Some(ProductionCodeGraphInputs {
            files: &generation.snapshot().files,
            symbols: generation.symbols(),
            imports: generation.imports(),
        }),
        projector_revision,
        check,
    )?;
    generation.memoize_graph_manifest(
        projection,
        projector_revision.clone(),
        Arc::new(manifest.clone()),
    );
    Ok(manifest)
}

pub(super) struct BuiltProjection {
    pub(super) watermark: GraphWatermark,
    pub(super) entities: Vec<GraphEntity>,
    pub(super) relations: Vec<GraphGenerationRelation>,
}

#[derive(Clone, Copy)]
pub(super) struct ProductionCodeGraphInputs<'a> {
    pub(super) files: &'a [SanitizedCodeFileV1],
    pub(super) symbols: &'a GenerationSymbolIndexV1,
    pub(super) imports: &'a [CodeIndexImportEvidenceV1],
}

pub(super) fn build_projection(
    projection: &GraphProjectionIdentity,
    generation: &CodeGenerationId,
    edges: &[CanonicalRelationEdgeV1],
    chunks: &[CodeSearchChunkV1],
    production: Option<ProductionCodeGraphInputs<'_>>,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<BuiltProjection, CodeGraphProjectionError> {
    generation
        .validate()
        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
    let files = production
        .map(|inputs| {
            inputs
                .files
                .iter()
                .map(|file| (file.file_occurrence_id.clone(), file))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let symbol_metadata = production
        .map(|inputs| {
            inputs
                .symbols
                .symbols
                .iter()
                .map(|symbol| (symbol.occurrence.clone(), symbol))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let imports: &[CodeIndexImportEvidenceV1] = production.map_or(&[], |inputs| inputs.imports);
    for import in imports {
        check()?;
        import
            .validate()
            .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
        let file = files.get(&import.file_occurrence_id).ok_or_else(|| {
            CodeGraphProjectionError::Contract(
                "code graph import refers to a file outside its immutable snapshot".to_owned(),
            )
        })?;
        if file.logical_path != import.logical_path {
            return Err(CodeGraphProjectionError::Contract(
                "code graph import logical path does not match its file occurrence".to_owned(),
            ));
        }
    }
    let mut bindings = BTreeMap::<SymbolOccurrenceId, CodeGraphSymbolBindingV1>::new();
    for chunk in chunks {
        check()?;
        chunk
            .validate()
            .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
        if chunk.anchor.generation_id != *generation {
            return Err(CodeGraphProjectionError::GenerationMismatch);
        }
        if production.is_some() && !files.contains_key(&chunk.anchor.file_occurrence_id) {
            return Err(CodeGraphProjectionError::Contract(
                "code graph chunk refers to a file outside its immutable snapshot".to_owned(),
            ));
        }
        let Some(symbol) = chunk.anchor.symbol_occurrence_id.clone() else {
            continue;
        };
        let candidate = CodeGraphSymbolBindingV1 {
            file: chunk.anchor.file_occurrence_id.clone(),
            logical_path: files
                .get(&chunk.anchor.file_occurrence_id)
                .map(|file| file.logical_path.clone()),
            source_span: Some(chunk.anchor.source_span),
            chunk: Some(chunk.id.clone()),
            language_descriptor_revision: chunk.language_descriptor_revision.clone(),
        };
        match bindings.entry(symbol) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(candidate);
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let current = entry.get_mut();
                if current.file != candidate.file
                    || current.logical_path != candidate.logical_path
                    || current.language_descriptor_revision
                        != candidate.language_descriptor_revision
                {
                    return Err(CodeGraphProjectionError::Contract(
                        "one symbol occurrence has conflicting graph candidate bindings".to_owned(),
                    ));
                }
                if candidate.chunk < current.chunk {
                    current.chunk = candidate.chunk;
                }
                current.source_span = match (current.source_span, candidate.source_span) {
                    (Some(left), Some(right)) => Some(tracedecay_domain::SourceSpan {
                        start_byte: left.start_byte.min(right.start_byte),
                        end_byte: left.end_byte.max(right.end_byte),
                    }),
                    (left, right) => left.or(right),
                };
            }
        }
    }

    let mut retained_edges = Vec::new();
    for edge in edges {
        check()?;
        validate_edge(edge)?;
        if bindings.contains_key(&edge.from_occurrence)
            || symbol_metadata.contains_key(&edge.from_occurrence)
        {
            retained_edges.push(edge.clone());
        }
    }
    retained_edges.sort_by(compare_edges);
    retained_edges.dedup();

    let mut occurrences: BTreeSet<_> = bindings
        .keys()
        .chain(symbol_metadata.keys())
        .cloned()
        .collect();
    for edge in &retained_edges {
        occurrences.insert(edge.to_occurrence.clone());
    }
    let mut entities = Vec::with_capacity(
        files
            .len()
            .saturating_add(imports.len())
            .saturating_add(chunks.len())
            .saturating_add(occurrences.len())
            .saturating_add(retained_edges.len())
            .saturating_add(1),
    );
    let mut relations = Vec::with_capacity(
        retained_edges
            .len()
            .saturating_mul(2)
            .saturating_add(bindings.len().saturating_mul(2))
            .saturating_add(imports.len()),
    );

    // Every stable identity below is a serialize-and-hash; each is computed
    // exactly once and reused by the entity and every relation that names it,
    // instead of being re-derived per emission site.
    let mut file_ids = BTreeMap::<FileOccurrenceId, GraphEntityId>::new();
    for file in files.values() {
        check()?;
        let identity = file_entity_id(&file.file_occurrence_id)?;
        entities.push(file_entity(identity.clone(), file)?);
        file_ids.insert(file.file_occurrence_id.clone(), identity);
    }
    for import in imports {
        check()?;
        let identity = import_entity_id(import)?;
        let file_id = file_ids
            .get(&import.file_occurrence_id)
            .cloned()
            .ok_or_else(|| {
                CodeGraphProjectionError::Contract(
                    "code graph import refers to a file outside its immutable snapshot".to_owned(),
                )
            })?;
        relations.push(file_import_relation(
            projection,
            import,
            file_id,
            &identity,
        )?);
        entities.push(import_entity(identity, import)?);
    }

    let mut symbol_ids = BTreeMap::<SymbolOccurrenceId, GraphEntityId>::new();
    for occurrence in &occurrences {
        symbol_ids.insert(occurrence.clone(), symbol_entity_id(occurrence)?);
    }
    for chunk in chunks {
        check()?;
        let identity = chunk_entity_id(&chunk.id)?;
        if let Some(occurrence) = &chunk.anchor.symbol_occurrence_id {
            let symbol_id = require_symbol_id(&symbol_ids, occurrence)?;
            relations.push(chunk_symbol_relation(
                projection, &identity, chunk, occurrence, symbol_id,
            )?);
        }
        entities.push(chunk_entity(identity, chunk)?);
    }
    for occurrence in occurrences {
        let identity = require_symbol_id(&symbol_ids, &occurrence)?.clone();
        let record = SymbolRecordV1 {
            binding: bindings.get(&occurrence).cloned(),
            metadata: symbol_metadata
                .get(&occurrence)
                .map(|record| (*record).clone()),
            occurrence,
        };
        entities.push(symbol_entity(identity, record)?);
    }
    if production.is_some() {
        for (occurrence, binding) in &bindings {
            let file_id = file_ids.get(&binding.file).cloned().ok_or_else(|| {
                CodeGraphProjectionError::Contract(
                    "code graph binding refers to a file outside its immutable snapshot"
                        .to_owned(),
                )
            })?;
            let symbol_id = require_symbol_id(&symbol_ids, occurrence)?;
            relations.push(file_symbol_relation(
                projection, binding, file_id, occurrence, symbol_id,
            )?);
        }
    }
    for edge in &retained_edges {
        check()?;
        let (entity, source, target) = edge_artifacts(projection, edge, &symbol_ids)?;
        entities.push(entity);
        relations.push(source);
        relations.push(target);
    }
    let projection_node_count = entities.len().checked_add(1).ok_or_else(|| {
        CodeGraphProjectionError::Contract("code graph projection node count overflowed".to_owned())
    })?;
    entities.push(current_generation_entity(
        generation,
        projection_node_count,
    )?);

    Ok(BuiltProjection {
        watermark: GraphWatermark::new(stable_identity("watermark", generation.as_str()))?,
        entities,
        relations,
    })
}

fn require_symbol_id<'ids>(
    symbol_ids: &'ids BTreeMap<SymbolOccurrenceId, GraphEntityId>,
    occurrence: &SymbolOccurrenceId,
) -> Result<&'ids GraphEntityId, CodeGraphProjectionError> {
    symbol_ids.get(occurrence).ok_or_else(|| {
        CodeGraphProjectionError::Contract(
            "code graph relation names a symbol occurrence with no entity".to_owned(),
        )
    })
}

/// One retained edge's entity plus both endpoint relations, sharing a single
/// serialization and identity derivation of the edge payload.
fn edge_artifacts(
    projection: &GraphProjectionIdentity,
    edge: &CanonicalRelationEdgeV1,
    symbol_ids: &BTreeMap<SymbolOccurrenceId, GraphEntityId>,
) -> Result<(GraphEntity, GraphGenerationRelation, GraphGenerationRelation), CodeGraphProjectionError>
{
    let payload = serialize(edge)?;
    let identity = GraphEntityId::new(stable_identity("edge", &hex::encode(&payload)))?;
    let entity = GraphEntity::new(
        identity.clone(),
        BTreeSet::from([GraphLabel::new(EDGE_LABEL)?]),
        BTreeMap::from([(
            GraphPropertyName::new(EDGE_RECORD_PROPERTY)?,
            GraphProperty::Bytes(payload),
        )]),
    )?;
    let from = require_symbol_id(symbol_ids, &edge.from_occurrence)?;
    let to = require_symbol_id(symbol_ids, &edge.to_occurrence)?;
    let source = GraphGenerationRelation::new(
        GraphRelationId::new(stable_identity("source", identity.as_str()))?,
        GraphEntityRef::new(projection.clone(), from.clone()),
        GraphEntityRef::new(projection.clone(), identity.clone()),
        GraphRelationKind::new(SOURCE_EDGE_KIND)?,
        BTreeMap::new(),
    )?;
    let target = GraphGenerationRelation::new(
        GraphRelationId::new(stable_identity("target", identity.as_str()))?,
        GraphEntityRef::new(projection.clone(), identity),
        GraphEntityRef::new(projection.clone(), to.clone()),
        GraphRelationKind::new(TARGET_EDGE_KIND)?,
        BTreeMap::new(),
    )?;
    Ok((entity, source, target))
}

fn file_entity(
    identity: GraphEntityId,
    file: &SanitizedCodeFileV1,
) -> Result<GraphEntity, CodeGraphProjectionError> {
    file.validate()
        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
    GraphEntity::new(
        identity,
        BTreeSet::from([GraphLabel::new(FILE_LABEL)?]),
        BTreeMap::from([(
            GraphPropertyName::new(FILE_RECORD_PROPERTY)?,
            GraphProperty::Bytes(serialize(file)?),
        )]),
    )
    .map_err(Into::into)
}

fn import_entity(
    identity: GraphEntityId,
    import: &CodeIndexImportEvidenceV1,
) -> Result<GraphEntity, CodeGraphProjectionError> {
    GraphEntity::new(
        identity,
        BTreeSet::from([GraphLabel::new(IMPORT_LABEL)?]),
        BTreeMap::from([(
            GraphPropertyName::new(IMPORT_RECORD_PROPERTY)?,
            GraphProperty::Bytes(serialize(import)?),
        )]),
    )
    .map_err(Into::into)
}

fn chunk_entity(
    identity: GraphEntityId,
    chunk: &CodeSearchChunkV1,
) -> Result<GraphEntity, CodeGraphProjectionError> {
    let record = ChunkRecordV1 {
        id: chunk.id.clone(),
        anchor: chunk.anchor.clone(),
        content_digest: chunk.content_digest.clone(),
        language_descriptor_revision: chunk.language_descriptor_revision.clone(),
        chunker_revision: chunk.chunker_revision.clone(),
        sanitizer_revision: chunk.sanitizer_revision.clone(),
        sensitivity: chunk.sensitivity.clone(),
    };
    GraphEntity::new(
        identity,
        BTreeSet::from([GraphLabel::new(CHUNK_LABEL)?]),
        BTreeMap::from([(
            GraphPropertyName::new(CHUNK_RECORD_PROPERTY)?,
            GraphProperty::Bytes(serialize(&record)?),
        )]),
    )
    .map_err(Into::into)
}

fn file_symbol_relation(
    projection: &GraphProjectionIdentity,
    binding: &CodeGraphSymbolBindingV1,
    file_id: GraphEntityId,
    occurrence: &SymbolOccurrenceId,
    symbol_id: &GraphEntityId,
) -> Result<GraphGenerationRelation, CodeGraphProjectionError> {
    GraphGenerationRelation::new(
        GraphRelationId::new(stable_identity(
            "file-symbol",
            &format!("{}\0{}", binding.file.as_str(), occurrence.as_str()),
        ))?,
        GraphEntityRef::new(projection.clone(), file_id),
        GraphEntityRef::new(projection.clone(), symbol_id.clone()),
        GraphRelationKind::new(FILE_SYMBOL_EDGE_KIND)?,
        BTreeMap::new(),
    )
    .map_err(Into::into)
}

fn file_import_relation(
    projection: &GraphProjectionIdentity,
    import: &CodeIndexImportEvidenceV1,
    file_id: GraphEntityId,
    import_id: &GraphEntityId,
) -> Result<GraphGenerationRelation, CodeGraphProjectionError> {
    GraphGenerationRelation::new(
        file_import_relation_id_with(import, import_id)?,
        GraphEntityRef::new(projection.clone(), file_id),
        GraphEntityRef::new(projection.clone(), import_id.clone()),
        GraphRelationKind::new(FILE_IMPORT_EDGE_KIND)?,
        BTreeMap::new(),
    )
    .map_err(Into::into)
}

fn chunk_symbol_relation(
    projection: &GraphProjectionIdentity,
    chunk_id: &GraphEntityId,
    chunk: &CodeSearchChunkV1,
    occurrence: &SymbolOccurrenceId,
    symbol_id: &GraphEntityId,
) -> Result<GraphGenerationRelation, CodeGraphProjectionError> {
    GraphGenerationRelation::new(
        GraphRelationId::new(stable_identity(
            "chunk-symbol",
            &format!("{}\0{}", chunk.id.as_str(), occurrence.as_str()),
        ))?,
        GraphEntityRef::new(projection.clone(), chunk_id.clone()),
        GraphEntityRef::new(projection.clone(), symbol_id.clone()),
        GraphRelationKind::new(CHUNK_SYMBOL_EDGE_KIND)?,
        BTreeMap::new(),
    )
    .map_err(Into::into)
}

fn chunk_entity_id(chunk: &CodeSearchChunkId) -> Result<GraphEntityId, CodeGraphProjectionError> {
    GraphEntityId::new(stable_identity("chunk", chunk.as_str())).map_err(Into::into)
}

pub(super) fn validate_symbol_metadata(
    metadata: &LineageSymbolRecordV1,
    occurrence: &SymbolOccurrenceId,
) -> Result<(), CodeGraphProjectionError> {
    if metadata.occurrence != *occurrence {
        return Err(CodeGraphProjectionError::Contract(
            "code graph symbol metadata names a different occurrence".to_owned(),
        ));
    }
    metadata
        .identity
        .validate()
        .and_then(|()| metadata.file_identity.validate())
        .and_then(|()| metadata.content_digest.validate())
        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
    if metadata.qualified_name.is_empty() || metadata.kind.is_empty() {
        return Err(CodeGraphProjectionError::Contract(
            "code graph symbol metadata is incomplete".to_owned(),
        ));
    }
    Ok(())
}
