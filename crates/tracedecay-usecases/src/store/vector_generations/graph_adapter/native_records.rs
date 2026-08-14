use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, CodeGenerationId, CodeSearchChunkId, ManifestDigest,
    ProjectionKeyV1, ProjectionOperationV1, VectorGenerationIdV1,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphEntity, GraphEntityId, GraphLabel, GraphProjectionTelemetryRequest,
    GraphProperty, GraphRelation, GraphVector, GraphWatermark,
    semantic_vector_native::{
        BASE_GENERATION, BASE_KIND, BATCH_COUNT, BUILD_BATCH_LABEL, BUILD_ID, BUILD_LABEL,
        BUILD_MEMBER_LABEL, CHECKPOINT, CHUNK_DIGEST, CHUNK_ID, CONTAINS_KIND, CONTROL_ID,
        CONTROL_LABEL, EMBEDDING_KEY, EXPECTED_COUNT, GENERATION_CATALOG_KIND, GENERATION_ID,
        GENERATION_LABEL, GENERATION_RECEIPT_LABEL, GENERATION_TOMBSTONE_LABEL,
        GENERATION_VECTOR_LABEL, MANIFEST_DIGEST, ORDINAL, OUTPUT_DIGEST, PREPARED_DIGEST,
        PRIOR_DIGEST, RECEIPT, RECEIPT_COUNT, REQUEST_DIGEST, REVISION, ROW_COUNT,
        SOURCE_GENERATION, SOURCE_MANIFEST, STAGED_TOMBSTONE_LABEL, STAGED_VECTOR_LABEL,
        TARGET_PROJECTION, TOMBSTONE_COUNT, VECTOR, VECTOR_BYTES, VECTOR_COUNT,
    },
};

use super::super::identity::generation_identity_digest;
use super::super::{
    PreparedVectorGenerationV1, ProjectedChunkVectorV1, VectorGenerationBuildIdV1,
    VectorGenerationStateMachineV1, VectorGenerationStoreErrorV1, VectorProjectionCheckpointV1,
};
use super::persistence::{generation_label, map_graph_error, storage_error, vector_metric};
use super::snapshot::SemanticVectorVerifiedRead;

mod catalog;
mod scoped;
mod support;

pub(super) use catalog::{
    read_generation_catalog, read_generation_catalog_entry, read_generation_publication_pointer,
};
pub(super) use scoped::{
    ScopedBuildRecordsV1, ScopedGenerationRecordsV1, read_build_records, read_generation_records,
};

use support::{
    build_entity_id, build_id, bytes_property, content_digest, corrupt, digest, entity, entity_id,
    generation_entity_id, generation_id, generation_receipt_property, graph_label, i64_property,
    insert_entity, insert_relation, optional_bytes, optional_digest_property, optional_generation,
    parse_id, properties, relation, relation_kind, require_labels, required_bytes,
    required_property, required_string, required_u64, scoped_entity_id, string_property,
};

pub(super) fn read_cataloged_generation_records(
    snapshot: &SemanticVectorVerifiedRead,
    generation_id: &VectorGenerationIdV1,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<Option<ScopedGenerationRecordsV1>, VectorGenerationStoreErrorV1> {
    let records = read_generation_records(snapshot, generation_id, Arc::clone(&cancellation))?;
    let catalog = read_generation_catalog_entry(snapshot, generation_id, cancellation)?;
    match (records, catalog) {
        (None, None) => Ok(None),
        (Some(records), Some(catalog)) => {
            let rows = u64::try_from(records.generation.vectors().len()).map_err(storage_error)?;
            let local_vector_entities = u64::try_from(
                records
                    .entities
                    .values()
                    .filter(|entity| {
                        entity
                            .labels
                            .iter()
                            .any(|label| label.as_str() == GENERATION_VECTOR_LABEL)
                    })
                    .count(),
            )
            .map_err(storage_error)?;
            if catalog.base_generation.as_ref() != records.generation.base_generation()
                || catalog.rows != rows
                || catalog.vector_bytes != records.vector_bytes
                || local_vector_entities > catalog.rows
            {
                return Err(corrupt(
                    "semantic vector generation catalog record is inconsistent",
                ));
            }
            Ok(Some(records))
        }
        _ => Err(corrupt(
            "semantic vector generation records and catalog disagree",
        )),
    }
}

#[derive(Clone, Debug)]
pub(crate) struct NativeGraphStateV1 {
    pub entities: Vec<GraphEntity>,
    pub relations: Vec<GraphRelation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct NativeStateMetadataV1 {
    pub watermark: GraphWatermark,
    pub revision: u64,
}

/// Decoded generation-row metadata. Only the embedding key is consumed; the
/// remaining persisted identity properties are validated fail-closed during
/// decode without being retained.
#[derive(Clone, Debug)]
pub(super) struct NativeGenerationMetadataV1 {
    pub embedding_key: AdmittedEmbeddingProjectionKeyV1,
}

/// Encode one bounded projector batch directly into its immutable semantic
/// generation identity. The semantic identity is known from the admitted plan
/// before batch zero; no staged row namespace or terminal corpus rename exists.
/// Ordinary reuse is receipt-only: reused chunks name lineage on the batch
/// receipt, and the base generation's vector rows serve them.
pub(crate) fn encode_generation_batch_delta(
    state: &VectorGenerationStateMachineV1,
    build_id: &VectorGenerationBuildIdV1,
    prepared: &PreparedVectorGenerationV1,
    revision: u64,
) -> Result<NativeGraphStateV1, VectorGenerationStoreErrorV1> {
    let build = state
        .staged
        .get(build_id)
        .ok_or(VectorGenerationStoreErrorV1::UnknownBuild)?;
    let mut entities = BTreeMap::new();
    let mut relations = BTreeMap::new();
    let manifest_digest = generation_identity_digest(&build.plan)?;
    let generation_id = VectorGenerationIdV1::new(manifest_digest.clone());
    insert_entity(
        &mut entities,
        entity(
            CONTROL_ID,
            [CONTROL_LABEL],
            [(REVISION, i64_property(revision)?)],
        )?,
    )?;

    let owner = generation_entity_id(&generation_id)?;
    let vector_bytes = build.vectors.values().try_fold(0_u64, |total, vector| {
        total
            .checked_add(
                u64::try_from(vector.values.len())
                    .map_err(storage_error)?
                    .checked_mul(4)
                    .ok_or_else(|| corrupt("semantic vector byte count overflowed"))?,
            )
            .ok_or_else(|| corrupt("semantic vector byte count overflowed"))
    })?;
    let embedding = build
        .embedding_key
        .as_ref()
        .ok_or(VectorGenerationStoreErrorV1::IncompleteGeneration)?;
    insert_entity(
        &mut entities,
        entity(
            owner.as_str(),
            [GENERATION_LABEL],
            [
                (
                    GENERATION_ID,
                    string_property(generation_id.as_digest().as_str()),
                ),
                (
                    TARGET_PROJECTION,
                    bytes_property(&build.plan.target_projection_key)?,
                ),
                (
                    SOURCE_GENERATION,
                    string_property(&build.plan.source_generation.to_string()),
                ),
                (
                    SOURCE_MANIFEST,
                    string_property(build.plan.source_manifest_digest.as_str()),
                ),
                (
                    BASE_GENERATION,
                    optional_digest_property(
                        build.plan.base_generation.as_ref().map(|id| id.as_digest()),
                    ),
                ),
                (EMBEDDING_KEY, bytes_property(embedding)?),
                (CHECKPOINT, bytes_property(&build.checkpoint)?),
                (MANIFEST_DIGEST, string_property(manifest_digest.as_str())),
                (ROW_COUNT, i64_property(build.vectors.len())?),
                (VECTOR_BYTES, i64_property(vector_bytes)?),
                (TOMBSTONE_COUNT, i64_property(build.tombstones.len())?),
                (RECEIPT_COUNT, i64_property(build.batches.len())?),
            ],
        )?,
    )?;
    insert_relation(
        &mut relations,
        relation(
            &entity_id(CONTROL_ID)?,
            &owner,
            GENERATION_CATALOG_KIND,
            "generation-catalog",
        )?,
    )?;
    for receipt in &prepared.receipt.receipts {
        match receipt.operation {
            ProjectionOperationV1::Added | ProjectionOperationV1::Updated => {
                let vector = build.vectors.get(&receipt.chunk_id).ok_or_else(|| {
                    VectorGenerationStoreErrorV1::Corrupt(
                        "committed semantic vector receipt has no staged vector".to_owned(),
                    )
                })?;
                let child = scoped_entity_id(
                    "generation-vector",
                    generation_id.as_digest().as_str(),
                    &receipt.chunk_id.to_string(),
                )?;
                insert_entity(
                    &mut entities,
                    vector_entity(
                        child.as_str(),
                        GENERATION_VECTOR_LABEL,
                        GENERATION_ID,
                        generation_id.as_digest().as_str(),
                        vector,
                        embedding,
                        Some(generation_label(&generation_id)?),
                        Some(&generation_id),
                    )?,
                )?;
                insert_relation(
                    &mut relations,
                    relation(&owner, &child, CONTAINS_KIND, "vector")?,
                )?;
            }
            ProjectionOperationV1::Reused => {
                if !build.vectors.contains_key(&receipt.chunk_id) {
                    return Err(VectorGenerationStoreErrorV1::Corrupt(
                        "committed semantic reused receipt has no staged vector".to_owned(),
                    ));
                }
            }
            ProjectionOperationV1::Deleted => {
                let prior_digest = build.tombstones.get(&receipt.chunk_id).ok_or_else(|| {
                    VectorGenerationStoreErrorV1::Corrupt(
                        "committed semantic tombstone receipt has no staged tombstone".to_owned(),
                    )
                })?;
                let child = scoped_entity_id(
                    "generation-tombstone",
                    generation_id.as_digest().as_str(),
                    &receipt.chunk_id.to_string(),
                )?;
                insert_entity(
                    &mut entities,
                    entity(
                        child.as_str(),
                        [GENERATION_TOMBSTONE_LABEL],
                        [
                            (
                                GENERATION_ID,
                                string_property(generation_id.as_digest().as_str()),
                            ),
                            (CHUNK_ID, string_property(&receipt.chunk_id.to_string())),
                            (PRIOR_DIGEST, string_property(prior_digest.as_str())),
                        ],
                    )?,
                )?;
                insert_relation(
                    &mut relations,
                    relation(&owner, &child, CONTAINS_KIND, "tombstone")?,
                )?;
            }
        }
    }

    let (ordinal, committed) = build
        .batches
        .iter()
        .enumerate()
        .find(|(_, batch)| batch.request_digest == prepared.request.request_digest)
        .ok_or_else(|| {
            VectorGenerationStoreErrorV1::Corrupt(
                "committed semantic vector batch has no native receipt row".to_owned(),
            )
        })?;
    let batch_row = scoped_entity_id(
        "generation-receipt",
        generation_id.as_digest().as_str(),
        &ordinal.to_string(),
    )?;
    insert_entity(
        &mut entities,
        entity(
            batch_row.as_str(),
            [GENERATION_RECEIPT_LABEL],
            [
                (
                    GENERATION_ID,
                    string_property(generation_id.as_digest().as_str()),
                ),
                (RECEIPT, generation_receipt_property(&committed.receipt)?),
                (ORDINAL, i64_property(ordinal)?),
            ],
        )?,
    )?;
    insert_relation(
        &mut relations,
        relation(&owner, &batch_row, CONTAINS_KIND, "batch")?,
    )?;

    Ok(NativeGraphStateV1 {
        entities: entities.into_values().collect(),
        relations: relations.into_values().collect(),
    })
}

pub(super) fn read_state_metadata(
    snapshot: &SemanticVectorVerifiedRead,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<NativeStateMetadataV1, VectorGenerationStoreErrorV1> {
    read_optional_state_metadata(snapshot, cancellation)?.ok_or_else(|| {
        VectorGenerationStoreErrorV1::Unavailable(
            "semantic vector graph projection is missing".to_owned(),
        )
    })
}

/// Like [`read_state_metadata`], but a graph that has never installed the
/// semantic-vector projection reads as `None` ("no vectors exist") instead of
/// an unavailability error. Read paths that admit an empty store use this;
/// mutation paths keep requiring the installed projection.
pub(super) fn read_optional_state_metadata(
    snapshot: &SemanticVectorVerifiedRead,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<Option<NativeStateMetadataV1>, VectorGenerationStoreErrorV1> {
    let namespace = snapshot.projection().namespace.clone();
    let Some(telemetry) = snapshot
        .projection_telemetry(GraphProjectionTelemetryRequest {
            namespace: namespace.clone(),
            projection: snapshot.projection().projection.clone(),
            cancellation: Arc::clone(&cancellation),
        })
        .map_err(map_graph_error)?
    else {
        return Ok(None);
    };
    let control = snapshot
        .entity(
            &namespace,
            &entity_id(CONTROL_ID)?,
            Arc::clone(&cancellation),
        )
        .map_err(map_graph_error)?
        .ok_or_else(|| corrupt("semantic vector control entity is missing"))?;
    require_labels(&control, [CONTROL_LABEL])?;
    Ok(Some(NativeStateMetadataV1 {
        watermark: telemetry.watermark,
        revision: required_u64(&control, REVISION)?,
    }))
}

pub(super) fn read_generation_metadata(
    snapshot: &SemanticVectorVerifiedRead,
    generation_id: &VectorGenerationIdV1,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<Option<NativeGenerationMetadataV1>, VectorGenerationStoreErrorV1> {
    snapshot
        .entity(
            &snapshot.projection().namespace,
            &generation_entity_id(generation_id)?,
            cancellation,
        )
        .map_err(map_graph_error)?
        .map(|row| {
            require_labels(&row, [GENERATION_LABEL])?;
            if required_string(&row, GENERATION_ID)? != generation_id.as_digest().as_str() {
                return Err(corrupt(
                    "semantic vector generation identity is inconsistent",
                ));
            }
            let _: ProjectionKeyV1 = required_bytes(&row, TARGET_PROJECTION)?;
            let _: CodeGenerationId = parse_id(required_string(&row, SOURCE_GENERATION)?)?;
            let _: ManifestDigest = digest(required_string(&row, SOURCE_MANIFEST)?)?;
            Ok(NativeGenerationMetadataV1 {
                embedding_key: required_bytes(&row, EMBEDDING_KEY)?,
            })
        })
        .transpose()
}

#[allow(clippy::too_many_arguments)]
fn vector_entity(
    identity: &str,
    label: &str,
    owner_property: &str,
    owner: &str,
    vector: &ProjectedChunkVectorV1,
    embedding: &AdmittedEmbeddingProjectionKeyV1,
    generation_row_label: Option<GraphLabel>,
    generation_id: Option<&VectorGenerationIdV1>,
) -> Result<GraphEntity, VectorGenerationStoreErrorV1> {
    let mut labels = BTreeSet::from([graph_label(label)?]);
    if let Some(label) = generation_row_label {
        labels.insert(label);
    }
    GraphEntity::new(
        entity_id(identity)?,
        labels,
        properties([
            (owner_property, string_property(owner)),
            (CHUNK_ID, string_property(&vector.chunk_id.to_string())),
            (CHUNK_DIGEST, string_property(vector.chunk_digest.as_str())),
            (
                OUTPUT_DIGEST,
                string_property(vector.output_digest.as_str()),
            ),
            (
                generation_id
                    .map(super::persistence::search_vector_property)
                    .transpose()?
                    .as_ref()
                    .map_or(VECTOR, tracedecay_graph_db::GraphPropertyName::as_str),
                GraphProperty::Vector(
                    GraphVector::new(
                        vector.values.clone(),
                        vector.values.len(),
                        vector_metric(embedding.embedding_key().metric),
                    )
                    .map_err(map_graph_error)?,
                ),
            ),
        ])?,
    )
    .map_err(map_graph_error)
}

fn decode_vector(
    row: &GraphEntity,
    projection_key: &ProjectionKeyV1,
    source_generation: &CodeGenerationId,
    source_manifest_digest: &ManifestDigest,
) -> Result<(CodeSearchChunkId, ProjectedChunkVectorV1), VectorGenerationStoreErrorV1> {
    let chunk_id: CodeSearchChunkId = parse_id(required_string(row, CHUNK_ID)?)?;
    let vector_property = optional_generation(row, GENERATION_ID)?
        .as_ref()
        .map(super::persistence::search_vector_property)
        .transpose()?;
    let vector = match vector_property.as_ref().map_or_else(
        || required_property(row, VECTOR),
        |property| {
            row.properties
                .get(property)
                .ok_or_else(|| corrupt("semantic vector row is missing its indexed vector"))
        },
    )? {
        GraphProperty::Vector(vector) => vector.values.clone(),
        _ => return Err(corrupt("semantic vector row has a non-vector value")),
    };
    Ok((
        chunk_id.clone(),
        ProjectedChunkVectorV1 {
            projection_key: projection_key.clone(),
            source_generation: source_generation.clone(),
            source_manifest_digest: source_manifest_digest.clone(),
            chunk_id,
            chunk_digest: content_digest(required_string(row, CHUNK_DIGEST)?)?,
            values: vector,
            output_digest: content_digest(required_string(row, OUTPUT_DIGEST)?)?,
        },
    ))
}

fn rows_with_label<'a>(
    entities: &'a BTreeMap<GraphEntityId, GraphEntity>,
    label: &str,
) -> Result<Vec<&'a GraphEntity>, VectorGenerationStoreErrorV1> {
    let label = graph_label(label)?;
    Ok(entities
        .values()
        .filter(|row| row.labels.contains(&label))
        .collect())
}

fn rows_with_owner<'a>(
    entities: &'a BTreeMap<GraphEntityId, GraphEntity>,
    label: &str,
    owner_property: &str,
    owner: &str,
) -> Result<Vec<&'a GraphEntity>, VectorGenerationStoreErrorV1> {
    rows_with_label(entities, label)?
        .into_iter()
        .filter_map(|row| match required_string(row, owner_property) {
            Ok(value) if value == owner => Some(Ok(row)),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect()
}
