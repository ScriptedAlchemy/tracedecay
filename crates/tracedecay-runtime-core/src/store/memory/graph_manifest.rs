use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};
use tracedecay_graph_db::{
    GraphEntity, GraphEntityId, GraphEntityRef, GraphGenerationId, GraphGenerationManifest,
    GraphGenerationRelation, GraphLabel, GraphProjectionIdentity, GraphRelationId,
    GraphRelationKind, GraphWatermark, MAX_VERIFIED_GENERATION_ENTITIES, SourceGeneration,
};
use tracedecay_store::{FactReadControl, FactStoreError, FactStoreResult};

use super::graph::graph_error;
use super::primitives::storage_message;

const OPERATION: &str = "project memory relation graph";

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct SourceRelation {
    pub(super) source: String,
    pub(super) target: String,
    pub(super) kind: String,
}

#[derive(Clone, Debug)]
pub(super) struct MemoryGraphSource {
    pub(super) owner: String,
    pub(super) entities: Vec<String>,
    pub(super) relations: BTreeSet<SourceRelation>,
}

pub(super) fn build_manifest(
    projection: GraphProjectionIdentity,
    source: &MemoryGraphSource,
    watermark: GraphWatermark,
    read_control: Option<&FactReadControl>,
) -> FactStoreResult<GraphGenerationManifest> {
    ensure_source_read_active(read_control)?;
    let mut entity_ids = BTreeSet::new();
    for identity in &source.entities {
        ensure_source_read_active(read_control)?;
        entity_ids.insert(GraphEntityId::new(identity.clone()).map_err(graph_error)?);
    }
    ensure_source_read_active(read_control)?;
    let mut relations = Vec::new();
    for relation in &source.relations {
        ensure_source_read_active(read_control)?;
        let from = GraphEntityId::new(relation.source.clone()).map_err(graph_error)?;
        let to = GraphEntityId::new(relation.target.clone()).map_err(graph_error)?;
        insert_projection_entity(&mut entity_ids, from.clone())?;
        insert_projection_entity(&mut entity_ids, to.clone())?;
        let relation_digest = hex::encode(Sha256::digest(
            format!(
                "{}\0{}\0{}",
                relation.source, relation.target, relation.kind
            )
            .as_bytes(),
        ));
        relations.push(
            GraphGenerationRelation::new(
                GraphRelationId::new(format!("memory-relation:{relation_digest}"))
                    .map_err(graph_error)?,
                GraphEntityRef::new(projection.clone(), from),
                GraphEntityRef::new(projection.clone(), to),
                GraphRelationKind::new(relation.kind.clone()).map_err(graph_error)?,
                BTreeMap::new(),
            )
            .map_err(graph_error)?,
        );
    }
    ensure_source_read_active(read_control)?;
    let mut entities = Vec::new();
    for identity in entity_ids {
        ensure_source_read_active(read_control)?;
        let label = label_for_entity(identity.as_str()).map_err(graph_error)?;
        let label = GraphLabel::new(label).map_err(graph_error)?;
        entities.push(
            GraphEntity::new(identity.clone(), BTreeSet::from([label]), BTreeMap::new())
                .map_err(graph_error)?,
        );
    }
    ensure_source_read_active(read_control)?;
    let generation = GraphGenerationId::new(format!("project-memory:{}", watermark.as_str()))
        .map_err(graph_error)?;
    let source_generation =
        SourceGeneration::new(watermark.as_str().to_owned()).map_err(graph_error)?;
    let check = || {
        if read_control.is_some_and(FactReadControl::interrupted) {
            Err(tracedecay_graph_db::GraphDbError::Cancelled)
        } else {
            Ok(())
        }
    };
    GraphGenerationManifest::new_checked(
        projection,
        generation,
        source_generation,
        watermark,
        Vec::new(),
        entities,
        relations,
        &check,
    )
    .map_err(|error| match error {
        tracedecay_graph_db::GraphDbError::Cancelled => FactStoreError::ReadCancelled,
        error => graph_error(error),
    })
}

pub(super) fn source_watermark(
    source: &MemoryGraphSource,
    read_control: Option<&FactReadControl>,
) -> FactStoreResult<GraphWatermark> {
    let mut hasher = Sha256::new();
    hash_source_component(&mut hasher, &source.owner);
    for entity in &source.entities {
        ensure_source_read_active(read_control)?;
        hash_source_component(&mut hasher, entity);
    }
    for relation in &source.relations {
        ensure_source_read_active(read_control)?;
        hash_source_component(&mut hasher, &relation.source);
        hash_source_component(&mut hasher, &relation.target);
        hash_source_component(&mut hasher, &relation.kind);
    }
    ensure_source_read_active(read_control)?;
    GraphWatermark::new(format!(
        "memory-relations:{}",
        hex::encode(hasher.finalize())
    ))
    .map_err(graph_error)
}

fn ensure_source_read_active(read_control: Option<&FactReadControl>) -> FactStoreResult<()> {
    if read_control.is_some_and(FactReadControl::interrupted) {
        return Err(FactStoreError::ReadCancelled);
    }
    Ok(())
}

fn hash_source_component(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
}

fn insert_projection_entity(
    entities: &mut BTreeSet<GraphEntityId>,
    entity: GraphEntityId,
) -> FactStoreResult<()> {
    if !entities.contains(&entity) && entities.len() >= MAX_VERIFIED_GENERATION_ENTITIES {
        return Err(storage_message(
            OPERATION,
            "canonical memory topology exceeds native graph entity capacity",
        ));
    }
    entities.insert(entity);
    Ok(())
}

fn label_for_entity(identity: &str) -> Result<&'static str, tracedecay_graph_db::GraphDbError> {
    if identity.starts_with("memory-fact:") {
        Ok("memory-fact-reference")
    } else if identity.starts_with("memory-entity:") {
        Ok("memory-entity-reference")
    } else if identity.starts_with("memory-assertion:") {
        Ok("memory-assertion-reference")
    } else if identity.starts_with("memory-anchor:") {
        Ok("retrieval-anchor-reference")
    } else {
        Err(tracedecay_graph_db::GraphDbError::invalid(
            "unknown memory relation entity identity",
        ))
    }
}
