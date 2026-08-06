use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, CodeGenerationId, CodeSearchChunkId, ManifestDigest,
    ProjectionKeyV1, VectorGenerationIdV1,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphEntity, GraphEntityId, GraphLabel, GraphProjectionTelemetryRequest,
    GraphProperty, GraphRelation, GraphSnapshot, GraphVector, GraphWatermark,
};

use super::super::{
    ProjectedChunkVectorV1, PublishedVectorGenerationV1, VectorGenerationStateMachineV1,
    VectorGenerationStoreErrorV1, VectorProjectionCheckpointV1,
};
use super::persistence::{
    generation_label, graph_namespace, graph_projection, map_graph_error, storage_error,
    vector_metric,
};

mod catalog;
mod scoped;
mod support;

pub(super) use catalog::{
    generation_catalog_relation_id, read_build_catalog, read_generation_catalog,
    read_generation_catalog_entry,
};
pub(super) use scoped::{
    ScopedBuildRecordsV1, ScopedGenerationRecordsV1, read_build_records, read_generation_records,
};

use support::{
    build_entity_id, build_id, bytes_property, content_digest, corrupt, digest, entity, entity_id,
    generation_entity_id, generation_id, graph_label, i64_property, insert_entity, insert_relation,
    optional_bytes, optional_bytes_property, optional_digest_property, optional_generation,
    parse_id, properties,
    property_name, relation, relation_id, relation_kind, require_labels, required_bytes,
    required_property, required_string, required_u64, scoped_entity_id, string_property,
};

const CONTROL_ID: &str = "semantic-vector:control";
const ACTIVE_RELATION_ID: &str = "semantic-vector:active";
const CONTROL_LABEL: &str = "semantic-vector-control-v1";
const BUILD_LABEL: &str = "semantic-vector-build-v1";
const BUILD_MEMBER_LABEL: &str = "semantic-vector-build-member-v1";
const STAGED_VECTOR_LABEL: &str = "semantic-vector-staged-vector-v1";
const STAGED_TOMBSTONE_LABEL: &str = "semantic-vector-staged-tombstone-v1";
const BUILD_BATCH_LABEL: &str = "semantic-vector-build-batch-v1";
const GENERATION_LABEL: &str = "semantic-vector-generation-v1";
const GENERATION_VECTOR_LABEL: &str = "semantic-vector-generation-vector-v1";
const GENERATION_TOMBSTONE_LABEL: &str = "semantic-vector-generation-tombstone-v1";
const GENERATION_RECEIPT_LABEL: &str = "semantic-vector-generation-receipt-v1";
const CONTAINS_KIND: &str = "semantic_vector_contains";
const BASE_KIND: &str = "semantic_vector_base";
const ACTIVE_KIND: &str = "semantic_vector_active";
const BUILD_CATALOG_KIND: &str = "semantic_vector_build_catalog";
const GENERATION_CATALOG_KIND: &str = "semantic_vector_generation_catalog";
const REVISION: &str = "revision";
const BUILD_ID: &str = "build_id";
const GENERATION_ID: &str = "generation_id";
const CHUNK_ID: &str = "chunk_id";
const CHUNK_DIGEST: &str = "chunk_digest";
const OUTPUT_DIGEST: &str = "output_digest";
const TARGET_PROJECTION: &str = "target_projection";
const SOURCE_GENERATION: &str = "source_generation";
const SOURCE_MANIFEST: &str = "source_manifest";
const BASE_GENERATION: &str = "base_generation";
const EMBEDDING_KEY: &str = "embedding_key";
const CHECKPOINT: &str = "checkpoint";
const MANIFEST_DIGEST: &str = "manifest_digest";
const REQUEST_DIGEST: &str = "request_digest";
const PREPARED_DIGEST: &str = "prepared_digest";
const RECEIPT: &str = "receipt";
const PRIOR_DIGEST: &str = "prior_digest";
const ORDINAL: &str = "ordinal";
const ROW_COUNT: &str = "row_count";
const VECTOR_BYTES: &str = "vector_bytes";
const EXPECTED_COUNT: &str = "expected_count";
const VECTOR_COUNT: &str = "vector_count";
const TOMBSTONE_COUNT: &str = "tombstone_count";
const BATCH_COUNT: &str = "batch_count";
const RECEIPT_COUNT: &str = "receipt_count";
const VECTOR: &str = "vector";

pub(super) fn read_cataloged_generation_records(
    snapshot: &GraphSnapshot,
    generation_id: &VectorGenerationIdV1,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<Option<ScopedGenerationRecordsV1>, VectorGenerationStoreErrorV1> {
    let records = read_generation_records(snapshot, generation_id, Arc::clone(&cancellation))?;
    let catalog = read_generation_catalog_entry(snapshot, generation_id, cancellation)?;
    match (records, catalog) {
        (None, None) => Ok(None),
        (Some(records), Some(catalog)) => {
            let rows = u64::try_from(records.generation.vectors().len()).map_err(storage_error)?;
            if catalog.base_generation.as_ref() != records.generation.base_generation()
                || catalog.rows != rows
                || catalog.vector_bytes != records.vector_bytes
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
pub(super) struct NativeGenerationMeasureV1 {
    pub rows: u64,
    pub vector_bytes: u64,
}

#[derive(Clone, Debug)]
pub(super) struct NativeGraphStateV1 {
    pub revision: u64,
    pub entities: Vec<GraphEntity>,
    pub relations: Vec<GraphRelation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct NativeStateMetadataV1 {
    pub watermark: GraphWatermark,
    pub revision: u64,
    pub active_generation: Option<VectorGenerationIdV1>,
    pub active_row_count: u64,
}

#[derive(Clone, Debug)]
pub(super) struct NativeGenerationMetadataV1 {
    pub projection_key: ProjectionKeyV1,
    pub source_generation: CodeGenerationId,
    pub source_manifest_digest: ManifestDigest,
    pub embedding_key: AdmittedEmbeddingProjectionKeyV1,
}

pub(super) fn encode_state(
    state: &VectorGenerationStateMachineV1,
    revision: u64,
) -> Result<NativeGraphStateV1, VectorGenerationStoreErrorV1> {
    let mut entities = BTreeMap::new();
    let mut relations = BTreeMap::new();
    insert_entity(
        &mut entities,
        entity(
            CONTROL_ID,
            [CONTROL_LABEL],
            [(REVISION, i64_property(revision)?)],
        )?,
    )?;

    for (build_id, build) in &state.staged {
        let owner = build_entity_id(build_id)?;
        insert_entity(
            &mut entities,
            entity(
                owner.as_str(),
                [BUILD_LABEL],
                [
                    (BUILD_ID, string_property(build_id.0.as_str())),
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
                    (
                        EMBEDDING_KEY,
                        optional_bytes_property(&build.embedding_key)?,
                    ),
                    (CHECKPOINT, bytes_property(&build.checkpoint)?),
                    (
                        EXPECTED_COUNT,
                        i64_property(build.plan.expected_chunk_ids.len())?,
                    ),
                    (VECTOR_COUNT, i64_property(build.vectors.len())?),
                    (TOMBSTONE_COUNT, i64_property(build.tombstones.len())?),
                    (BATCH_COUNT, i64_property(build.batches.len())?),
                ],
            )?,
        )?;
        insert_relation(
            &mut relations,
            relation(
                &entity_id(CONTROL_ID)?,
                &owner,
                BUILD_CATALOG_KIND,
                "build-catalog",
            )?,
        )?;
        if let Some(base) = &build.plan.base_generation {
            insert_relation(
                &mut relations,
                relation(
                    &owner,
                    &generation_entity_id(base)?,
                    BASE_KIND,
                    "build-base",
                )?,
            )?;
        }
        for chunk_id in build.plan.expected_chunk_ids.iter() {
            let child =
                scoped_entity_id("build-member", build_id.0.as_str(), &chunk_id.to_string())?;
            insert_entity(
                &mut entities,
                entity(
                    child.as_str(),
                    [BUILD_MEMBER_LABEL],
                    [
                        (BUILD_ID, string_property(build_id.0.as_str())),
                        (CHUNK_ID, string_property(&chunk_id.to_string())),
                    ],
                )?,
            )?;
            insert_relation(
                &mut relations,
                relation(&owner, &child, CONTAINS_KIND, "member")?,
            )?;
        }
        for vector in build.vectors.values() {
            let child = scoped_entity_id(
                "staged-vector",
                build_id.0.as_str(),
                &vector.chunk_id.to_string(),
            )?;
            insert_entity(
                &mut entities,
                vector_entity(
                    child.as_str(),
                    STAGED_VECTOR_LABEL,
                    BUILD_ID,
                    build_id.0.as_str(),
                    vector,
                    build
                        .embedding_key
                        .as_ref()
                        .ok_or(VectorGenerationStoreErrorV1::IncompleteGeneration)?,
                    None,
                )?,
            )?;
            insert_relation(
                &mut relations,
                relation(&owner, &child, CONTAINS_KIND, "vector")?,
            )?;
        }
        for (chunk_id, prior_digest) in build.tombstones.iter() {
            let child = scoped_entity_id(
                "staged-tombstone",
                build_id.0.as_str(),
                &chunk_id.to_string(),
            )?;
            insert_entity(
                &mut entities,
                entity(
                    child.as_str(),
                    [STAGED_TOMBSTONE_LABEL],
                    [
                        (BUILD_ID, string_property(build_id.0.as_str())),
                        (CHUNK_ID, string_property(&chunk_id.to_string())),
                        (PRIOR_DIGEST, string_property(prior_digest.as_str())),
                    ],
                )?,
            )?;
            insert_relation(
                &mut relations,
                relation(&owner, &child, CONTAINS_KIND, "tombstone")?,
            )?;
        }
        for (ordinal, batch) in build.batches.iter().enumerate() {
            let child = scoped_entity_id(
                "build-batch",
                build_id.0.as_str(),
                batch.request_digest.as_str(),
            )?;
            insert_entity(
                &mut entities,
                entity(
                    child.as_str(),
                    [BUILD_BATCH_LABEL],
                    [
                        (BUILD_ID, string_property(build_id.0.as_str())),
                        (
                            REQUEST_DIGEST,
                            string_property(batch.request_digest.as_str()),
                        ),
                        (
                            PREPARED_DIGEST,
                            string_property(batch.prepared_digest.as_str()),
                        ),
                        (RECEIPT, bytes_property(&batch.receipt)?),
                        (ORDINAL, i64_property(ordinal)?),
                    ],
                )?,
            )?;
            insert_relation(
                &mut relations,
                relation(&owner, &child, CONTAINS_KIND, "batch")?,
            )?;
        }
    }

    for (generation_id, generation) in &state.published.generations {
        let owner = generation_entity_id(generation_id)?;
        let measure = generation_measure(generation)?;
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
                        bytes_property(&generation.projection_key)?,
                    ),
                    (
                        SOURCE_GENERATION,
                        string_property(&generation.source_generation.to_string()),
                    ),
                    (
                        SOURCE_MANIFEST,
                        string_property(generation.source_manifest_digest.as_str()),
                    ),
                    (
                        BASE_GENERATION,
                        optional_digest_property(
                            generation.base_generation.as_ref().map(|id| id.as_digest()),
                        ),
                    ),
                    (EMBEDDING_KEY, bytes_property(&generation.embedding_key)?),
                    (CHECKPOINT, bytes_property(&generation.checkpoint)?),
                    (
                        MANIFEST_DIGEST,
                        string_property(generation.manifest_digest.as_str()),
                    ),
                    (ROW_COUNT, i64_property(measure.rows)?),
                    (VECTOR_BYTES, i64_property(measure.vector_bytes)?),
                    (
                        TOMBSTONE_COUNT,
                        i64_property(generation.tombstone_digests.len())?,
                    ),
                    (RECEIPT_COUNT, i64_property(generation.receipts.len())?),
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
        if let Some(base) = &generation.base_generation {
            insert_relation(
                &mut relations,
                relation(
                    &owner,
                    &generation_entity_id(base)?,
                    BASE_KIND,
                    "generation-base",
                )?,
            )?;
        }
        for vector in generation.vectors.values() {
            let child = scoped_entity_id(
                "generation-vector",
                generation_id.as_digest().as_str(),
                &vector.chunk_id.to_string(),
            )?;
            insert_entity(
                &mut entities,
                vector_entity(
                    child.as_str(),
                    GENERATION_VECTOR_LABEL,
                    GENERATION_ID,
                    generation_id.as_digest().as_str(),
                    vector,
                    &generation.embedding_key,
                    Some(generation_label(generation_id)?),
                )?,
            )?;
            insert_relation(
                &mut relations,
                relation(&owner, &child, CONTAINS_KIND, "vector")?,
            )?;
        }
        for (chunk_id, prior_digest) in generation.tombstone_digests.iter() {
            let child = scoped_entity_id(
                "generation-tombstone",
                generation_id.as_digest().as_str(),
                &chunk_id.to_string(),
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
                        (CHUNK_ID, string_property(&chunk_id.to_string())),
                        (PRIOR_DIGEST, string_property(prior_digest.as_str())),
                    ],
                )?,
            )?;
            insert_relation(
                &mut relations,
                relation(&owner, &child, CONTAINS_KIND, "tombstone")?,
            )?;
        }
        for (ordinal, receipt) in generation.receipts.iter().enumerate() {
            let child = scoped_entity_id(
                "generation-receipt",
                generation_id.as_digest().as_str(),
                &ordinal.to_string(),
            )?;
            insert_entity(
                &mut entities,
                entity(
                    child.as_str(),
                    [GENERATION_RECEIPT_LABEL],
                    [
                        (
                            GENERATION_ID,
                            string_property(generation_id.as_digest().as_str()),
                        ),
                        (ORDINAL, i64_property(ordinal)?),
                        (RECEIPT, bytes_property(receipt)?),
                    ],
                )?,
            )?;
            insert_relation(
                &mut relations,
                relation(&owner, &child, CONTAINS_KIND, "receipt")?,
            )?;
        }
    }
    if let Some(active) = &state.published.active_generation {
        insert_relation(
            &mut relations,
            GraphRelation::new(
                relation_id(ACTIVE_RELATION_ID)?,
                entity_id(CONTROL_ID)?,
                generation_entity_id(active)?,
                relation_kind(ACTIVE_KIND)?,
                BTreeMap::new(),
            )
            .map_err(map_graph_error)?,
        )?;
    }
    Ok(NativeGraphStateV1 {
        revision,
        entities: entities.into_values().collect(),
        relations: relations.into_values().collect(),
    })
}

pub(super) fn read_state_metadata(
    snapshot: &GraphSnapshot,
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
    snapshot: &GraphSnapshot,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<Option<NativeStateMetadataV1>, VectorGenerationStoreErrorV1> {
    let namespace = graph_namespace()?;
    let Some(telemetry) = snapshot
        .projection_telemetry(GraphProjectionTelemetryRequest {
            namespace: namespace.clone(),
            projection: graph_projection()?,
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
    let active = snapshot
        .relation(
            &namespace,
            &relation_id(ACTIVE_RELATION_ID)?,
            Arc::clone(&cancellation),
        )
        .map_err(map_graph_error)?;
    let active_generation = active.as_ref().map(decode_active_relation).transpose()?;
    let active_row_count = match &active_generation {
        Some(id) => snapshot
            .entity(&namespace, &generation_entity_id(id)?, cancellation)
            .map_err(map_graph_error)?
            .ok_or_else(|| corrupt("active semantic vector generation is missing"))
            .and_then(|row| required_u64(&row, ROW_COUNT))?,
        None => 0,
    };
    Ok(Some(NativeStateMetadataV1 {
        watermark: telemetry.watermark,
        revision: required_u64(&control, REVISION)?,
        active_generation,
        active_row_count,
    }))
}

pub(super) fn read_generation_metadata(
    snapshot: &GraphSnapshot,
    generation_id: &VectorGenerationIdV1,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<Option<NativeGenerationMetadataV1>, VectorGenerationStoreErrorV1> {
    snapshot
        .entity(
            &graph_namespace()?,
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
            Ok(NativeGenerationMetadataV1 {
                projection_key: required_bytes(&row, TARGET_PROJECTION)?,
                source_generation: parse_id(required_string(&row, SOURCE_GENERATION)?)?,
                source_manifest_digest: digest(required_string(&row, SOURCE_MANIFEST)?)?,
                embedding_key: required_bytes(&row, EMBEDDING_KEY)?,
            })
        })
        .transpose()
}

pub(super) fn read_control_entity(
    snapshot: &GraphSnapshot,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<GraphEntity, VectorGenerationStoreErrorV1> {
    snapshot
        .entity(&graph_namespace()?, &entity_id(CONTROL_ID)?, cancellation)
        .map_err(map_graph_error)?
        .ok_or_else(|| corrupt("semantic vector control entity is missing"))
}

pub(super) fn set_control_revision(
    control: &mut GraphEntity,
    revision: u64,
) -> Result<(), VectorGenerationStoreErrorV1> {
    require_labels(control, [CONTROL_LABEL])?;
    control
        .properties
        .insert(property_name(REVISION)?, i64_property(revision)?);
    Ok(())
}

fn vector_entity(
    identity: &str,
    label: &str,
    owner_property: &str,
    owner: &str,
    vector: &ProjectedChunkVectorV1,
    embedding: &AdmittedEmbeddingProjectionKeyV1,
    generation_row_label: Option<GraphLabel>,
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
                VECTOR,
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
    let vector = match required_property(row, VECTOR)? {
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

fn decode_active_relation(
    relation: &GraphRelation,
) -> Result<VectorGenerationIdV1, VectorGenerationStoreErrorV1> {
    if relation.identity.as_str() != ACTIVE_RELATION_ID
        || relation.from.as_str() != CONTROL_ID
        || relation.kind.as_str() != ACTIVE_KIND
    {
        return Err(corrupt("semantic vector active relation is invalid"));
    }
    relation
        .to
        .as_str()
        .strip_prefix("semantic-vector:generation:")
        .ok_or_else(|| corrupt("semantic vector active target is invalid"))
        .and_then(generation_id)
}

fn generation_measure(
    generation: &PublishedVectorGenerationV1,
) -> Result<NativeGenerationMeasureV1, VectorGenerationStoreErrorV1> {
    let rows = u64::try_from(generation.vectors.len()).map_err(storage_error)?;
    let vector_bytes = generation.vectors.values().try_fold(0_u64, |total, row| {
        total
            .checked_add(
                u64::try_from(row.values.len())
                    .map_err(storage_error)?
                    .checked_mul(4)
                    .ok_or_else(|| corrupt("semantic vector byte count overflowed"))?,
            )
            .ok_or_else(|| corrupt("semantic vector byte count overflowed"))
    })?;
    Ok(NativeGenerationMeasureV1 { rows, vector_bytes })
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
