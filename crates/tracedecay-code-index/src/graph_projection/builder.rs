use std::collections::{BTreeMap, BTreeSet};

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

use super::{
    CHUNK_LABEL, CHUNK_RECORD_PROPERTY, CHUNK_SYMBOL_EDGE_KIND, CodeGraphProjectionError,
    CodeGraphSymbolBindingV1, FILE_LABEL, FILE_RECORD_PROPERTY, FILE_SYMBOL_EDGE_KIND,
    SymbolRecordV1, build_code_graph_manifest_inputs_checked, compare_edges,
    current_generation_entity, edge_entity, serialize, source_relation, stable_identity,
    symbol_entity, symbol_entity_id, target_relation, validate_edge,
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
    generation
        .validate()
        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
    let generation_id = &generation.manifest().generation_id;
    if generation.symbols().generation_id != *generation_id {
        return Err(CodeGraphProjectionError::GenerationMismatch);
    }
    build_code_graph_manifest_inputs_checked(
        projection,
        generation_id,
        generation.edges(),
        generation.chunks().chunks(),
        Some(ProductionCodeGraphInputs {
            files: &generation.snapshot().files,
            symbols: generation.symbols(),
        }),
        projector_revision,
        check,
    )
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
            .saturating_add(chunks.len())
            .saturating_add(occurrences.len())
            .saturating_add(retained_edges.len())
            .saturating_add(1),
    );
    for file in files.values() {
        check()?;
        entities.push(file_entity(file)?);
    }
    for chunk in chunks {
        check()?;
        entities.push(chunk_entity(chunk)?);
    }
    for occurrence in occurrences {
        let record = SymbolRecordV1 {
            binding: bindings.get(&occurrence).cloned(),
            metadata: symbol_metadata
                .get(&occurrence)
                .map(|record| (*record).clone()),
            occurrence,
        };
        entities.push(symbol_entity(record)?);
    }
    for edge in &retained_edges {
        entities.push(edge_entity(edge)?);
    }
    let projection_node_count = entities.len().checked_add(1).ok_or_else(|| {
        CodeGraphProjectionError::Contract("code graph projection node count overflowed".to_owned())
    })?;
    entities.push(current_generation_entity(
        generation,
        projection_node_count,
    )?);

    let mut relations = Vec::with_capacity(
        retained_edges
            .len()
            .saturating_mul(2)
            .saturating_add(bindings.len().saturating_mul(2)),
    );
    if production.is_some() {
        for (occurrence, binding) in &bindings {
            relations.push(file_symbol_relation(projection, binding, occurrence)?);
        }
    }
    for chunk in chunks {
        if let Some(occurrence) = &chunk.anchor.symbol_occurrence_id {
            relations.push(chunk_symbol_relation(projection, chunk, occurrence)?);
        }
    }
    for edge in retained_edges {
        relations.push(source_relation(projection, &edge)?);
        relations.push(target_relation(projection, &edge)?);
    }
    Ok(BuiltProjection {
        watermark: GraphWatermark::new(stable_identity("watermark", generation.as_str()))?,
        entities,
        relations,
    })
}

fn file_entity(file: &SanitizedCodeFileV1) -> Result<GraphEntity, CodeGraphProjectionError> {
    file.validate()
        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
    GraphEntity::new(
        file_entity_id(&file.file_occurrence_id)?,
        BTreeSet::from([GraphLabel::new(FILE_LABEL)?]),
        BTreeMap::from([(
            GraphPropertyName::new(FILE_RECORD_PROPERTY)?,
            GraphProperty::Bytes(serialize(file)?),
        )]),
    )
    .map_err(Into::into)
}

fn chunk_entity(chunk: &CodeSearchChunkV1) -> Result<GraphEntity, CodeGraphProjectionError> {
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
        chunk_entity_id(&chunk.id)?,
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
    occurrence: &SymbolOccurrenceId,
) -> Result<GraphGenerationRelation, CodeGraphProjectionError> {
    GraphGenerationRelation::new(
        GraphRelationId::new(stable_identity(
            "file-symbol",
            &format!("{}\0{}", binding.file.as_str(), occurrence.as_str()),
        ))?,
        GraphEntityRef::new(projection.clone(), file_entity_id(&binding.file)?),
        GraphEntityRef::new(projection.clone(), symbol_entity_id(occurrence)?),
        GraphRelationKind::new(FILE_SYMBOL_EDGE_KIND)?,
        BTreeMap::new(),
    )
    .map_err(Into::into)
}

fn chunk_symbol_relation(
    projection: &GraphProjectionIdentity,
    chunk: &CodeSearchChunkV1,
    occurrence: &SymbolOccurrenceId,
) -> Result<GraphGenerationRelation, CodeGraphProjectionError> {
    GraphGenerationRelation::new(
        GraphRelationId::new(stable_identity(
            "chunk-symbol",
            &format!("{}\0{}", chunk.id.as_str(), occurrence.as_str()),
        ))?,
        GraphEntityRef::new(projection.clone(), chunk_entity_id(&chunk.id)?),
        GraphEntityRef::new(projection.clone(), symbol_entity_id(occurrence)?),
        GraphRelationKind::new(CHUNK_SYMBOL_EDGE_KIND)?,
        BTreeMap::new(),
    )
    .map_err(Into::into)
}

fn file_entity_id(file: &FileOccurrenceId) -> Result<GraphEntityId, CodeGraphProjectionError> {
    GraphEntityId::new(stable_identity("file", file.as_str())).map_err(Into::into)
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
