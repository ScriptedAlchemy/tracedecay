use std::collections::BTreeSet;
use std::sync::Arc;

use tracedecay_domain::VectorGenerationIdV1;
use tracedecay_graph_db::{
    GraphCancellation, GraphEntityId, GraphRelationId, GraphTraversalDirection, TraversalRequest,
};

use super::super::super::{VectorGenerationBuildIdV1, VectorGenerationStoreErrorV1};
use super::super::persistence::map_graph_error;
use super::{
    BASE_GENERATION, BUILD_CATALOG_KIND, BUILD_ID, CONTROL_ID, GENERATION_CATALOG_KIND,
    GENERATION_ID, ROW_COUNT, VECTOR_BYTES, build_id, entity_id, generation_entity_id,
    generation_id, optional_generation, relation, relation_kind, required_string, required_u64,
};

const MAX_CATALOG_RECORDS: usize = 10_000;

#[derive(Clone, Debug)]
pub(crate) struct NativeGenerationCatalogEntryV1 {
    pub generation_id: VectorGenerationIdV1,
    pub base_generation: Option<VectorGenerationIdV1>,
    pub rows: u64,
    pub vector_bytes: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct NativeBuildCatalogEntryV1 {
    pub build_id: VectorGenerationBuildIdV1,
    pub base_generation: Option<VectorGenerationIdV1>,
}

pub(crate) fn read_generation_catalog(
    snapshot: &super::super::snapshot::SemanticVectorVerifiedRead,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<Vec<NativeGenerationCatalogEntryV1>, VectorGenerationStoreErrorV1> {
    let visits = catalog_visits(snapshot, GENERATION_CATALOG_KIND, Arc::clone(&cancellation))?;
    visits
        .into_iter()
        .map(|identity| {
            let row = snapshot
                .entity(
                    &snapshot.projection().namespace,
                    &identity,
                    Arc::clone(&cancellation),
                )
                .map_err(map_graph_error)?
                .ok_or_else(|| corrupt("semantic vector generation catalog target is missing"))?;
            decode_generation_catalog_entry(&row)
        })
        .collect()
}

pub(crate) fn read_generation_catalog_entry(
    snapshot: &super::super::snapshot::SemanticVectorVerifiedRead,
    generation: &VectorGenerationIdV1,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<Option<NativeGenerationCatalogEntryV1>, VectorGenerationStoreErrorV1> {
    let namespace = snapshot.projection().namespace.clone();
    let relation_id = generation_catalog_relation_id(generation)?;
    let Some(catalog_relation) = snapshot
        .relation(&namespace, &relation_id, Arc::clone(&cancellation))
        .map_err(map_graph_error)?
    else {
        return Ok(None);
    };
    if catalog_relation.from != entity_id(CONTROL_ID)?
        || catalog_relation.to != generation_entity_id(generation)?
        || catalog_relation.kind != relation_kind(GENERATION_CATALOG_KIND)?
    {
        return Err(corrupt(
            "semantic vector generation catalog relation is inconsistent",
        ));
    }
    let row = snapshot
        .entity(&namespace, &generation_entity_id(generation)?, cancellation)
        .map_err(map_graph_error)?
        .ok_or_else(|| corrupt("semantic vector generation catalog target is missing"))?;
    let entry = decode_generation_catalog_entry(&row)?;
    if &entry.generation_id != generation {
        return Err(corrupt(
            "semantic vector generation catalog identity is inconsistent",
        ));
    }
    Ok(Some(entry))
}

pub(crate) fn read_build_catalog(
    snapshot: &super::super::snapshot::SemanticVectorVerifiedRead,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<Vec<NativeBuildCatalogEntryV1>, VectorGenerationStoreErrorV1> {
    let visits = catalog_visits(snapshot, BUILD_CATALOG_KIND, Arc::clone(&cancellation))?;
    visits
        .into_iter()
        .map(|identity| {
            let row = snapshot
                .entity(
                    &snapshot.projection().namespace,
                    &identity,
                    Arc::clone(&cancellation),
                )
                .map_err(map_graph_error)?
                .ok_or_else(|| corrupt("semantic vector build catalog target is missing"))?;
            Ok(NativeBuildCatalogEntryV1 {
                build_id: build_id(required_string(&row, BUILD_ID)?)?,
                base_generation: optional_generation(&row, BASE_GENERATION)?,
            })
        })
        .collect()
}

pub(crate) fn generation_catalog_relation_id(
    generation_id: &VectorGenerationIdV1,
) -> Result<GraphRelationId, VectorGenerationStoreErrorV1> {
    relation(
        &entity_id(CONTROL_ID)?,
        &generation_entity_id(generation_id)?,
        GENERATION_CATALOG_KIND,
        "generation-catalog",
    )
    .map(|relation| relation.identity)
}

fn catalog_visits(
    snapshot: &super::super::snapshot::SemanticVectorVerifiedRead,
    kind: &str,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<Vec<GraphEntityId>, VectorGenerationStoreErrorV1> {
    let result = snapshot
        .traverse(TraversalRequest {
            namespace: snapshot.projection().namespace.clone(),
            start: entity_id(CONTROL_ID)?,
            relation_kinds: BTreeSet::from([relation_kind(kind)?]),
            direction: GraphTraversalDirection::Outgoing,
            max_depth: 1,
            max_visits: MAX_CATALOG_RECORDS,
            max_results: MAX_CATALOG_RECORDS,
            cancellation,
        })
        .map_err(map_graph_error)?;
    let mut identities = result
        .visits
        .into_iter()
        .filter(|visit| visit.via_relation.is_some())
        .map(|visit| visit.entity)
        .collect::<Vec<_>>();
    identities.sort();
    if identities.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(corrupt(
            "semantic vector catalog contains duplicate targets",
        ));
    }
    Ok(identities)
}

fn decode_generation_catalog_entry(
    row: &tracedecay_graph_db::GraphEntity,
) -> Result<NativeGenerationCatalogEntryV1, VectorGenerationStoreErrorV1> {
    Ok(NativeGenerationCatalogEntryV1 {
        generation_id: generation_id(required_string(row, GENERATION_ID)?)?,
        base_generation: optional_generation(row, BASE_GENERATION)?,
        rows: required_u64(row, ROW_COUNT)?,
        vector_bytes: required_u64(row, VECTOR_BYTES)?,
    })
}

fn corrupt(message: impl Into<String>) -> VectorGenerationStoreErrorV1 {
    VectorGenerationStoreErrorV1::Corrupt(message.into())
}
