use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde::de::DeserializeOwned;
use tracedecay_domain::{
    ContentDigest, ManifestDigest, ProjectionBatchReceiptV1, VectorGenerationIdV1,
};
use tracedecay_graph_db::{
    GraphEntity, GraphEntityId, GraphLabel, GraphProperty, GraphPropertyName, GraphRelation,
    GraphRelationId, GraphRelationKind, semantic_vector_native,
};

use super::super::super::{VectorGenerationBuildIdV1, VectorGenerationStoreErrorV1};
use super::super::persistence::{map_graph_error, storage_error};

pub(super) fn entity<const N: usize, const P: usize>(
    identity: &str,
    labels: [&str; N],
    props: [(&str, GraphProperty); P],
) -> Result<GraphEntity, VectorGenerationStoreErrorV1> {
    GraphEntity::new(
        entity_id(identity)?,
        labels
            .into_iter()
            .map(graph_label)
            .collect::<Result<_, _>>()?,
        properties(props)?,
    )
    .map_err(map_graph_error)
}

pub(super) fn relation(
    from: &GraphEntityId,
    to: &GraphEntityId,
    kind: &str,
    discriminator: &str,
) -> Result<GraphRelation, VectorGenerationStoreErrorV1> {
    GraphRelation::new(
        semantic_vector_native::relation_id(from, to, kind, discriminator)
            .map_err(map_graph_error)?,
        from.clone(),
        to.clone(),
        relation_kind(kind)?,
        BTreeMap::new(),
    )
    .map_err(map_graph_error)
}

pub(super) fn properties<const N: usize>(
    values: [(&str, GraphProperty); N],
) -> Result<BTreeMap<GraphPropertyName, GraphProperty>, VectorGenerationStoreErrorV1> {
    values
        .into_iter()
        .map(|(name, value)| Ok((property_name(name)?, value)))
        .collect()
}

pub(super) fn insert_entity(
    entities: &mut BTreeMap<GraphEntityId, GraphEntity>,
    entity: GraphEntity,
) -> Result<(), VectorGenerationStoreErrorV1> {
    if entities.insert(entity.identity.clone(), entity).is_some() {
        Err(corrupt("duplicate semantic vector entity identity"))
    } else {
        Ok(())
    }
}

pub(super) fn insert_relation(
    relations: &mut BTreeMap<GraphRelationId, GraphRelation>,
    relation: GraphRelation,
) -> Result<(), VectorGenerationStoreErrorV1> {
    if relations
        .insert(relation.identity.clone(), relation)
        .is_some()
    {
        Err(corrupt("duplicate semantic vector relation identity"))
    } else {
        Ok(())
    }
}

pub(super) fn build_entity_id(
    id: &VectorGenerationBuildIdV1,
) -> Result<GraphEntityId, VectorGenerationStoreErrorV1> {
    semantic_vector_native::build_entity_id(id.0.as_str()).map_err(map_graph_error)
}

pub(super) fn generation_entity_id(
    id: &VectorGenerationIdV1,
) -> Result<GraphEntityId, VectorGenerationStoreErrorV1> {
    semantic_vector_native::generation_entity_id(id.as_digest().as_str()).map_err(map_graph_error)
}

pub(super) fn scoped_entity_id(
    kind: &str,
    owner: &str,
    member: &str,
) -> Result<GraphEntityId, VectorGenerationStoreErrorV1> {
    semantic_vector_native::scoped_entity_id(kind, owner, member).map_err(map_graph_error)
}

pub(super) fn entity_id(value: &str) -> Result<GraphEntityId, VectorGenerationStoreErrorV1> {
    GraphEntityId::new(value).map_err(map_graph_error)
}

pub(super) fn relation_kind(
    value: &str,
) -> Result<GraphRelationKind, VectorGenerationStoreErrorV1> {
    GraphRelationKind::new(value).map_err(map_graph_error)
}

pub(super) fn graph_label(value: &str) -> Result<GraphLabel, VectorGenerationStoreErrorV1> {
    GraphLabel::new(value).map_err(map_graph_error)
}

pub(super) fn property_name(
    value: &str,
) -> Result<GraphPropertyName, VectorGenerationStoreErrorV1> {
    GraphPropertyName::new(value).map_err(map_graph_error)
}

pub(super) fn string_property(value: &str) -> GraphProperty {
    GraphProperty::String(value.to_owned())
}

pub(super) fn i64_property<T>(value: T) -> Result<GraphProperty, VectorGenerationStoreErrorV1>
where
    T: TryInto<i64>,
    T::Error: std::fmt::Display,
{
    value
        .try_into()
        .map(GraphProperty::I64)
        .map_err(storage_error)
}

pub(super) fn bytes_property<T: Serialize>(
    value: &T,
) -> Result<GraphProperty, VectorGenerationStoreErrorV1> {
    serde_json::to_vec(value)
        .map(GraphProperty::Bytes)
        .map_err(storage_error)
}

pub(super) fn generation_receipt_property(
    receipt: &ProjectionBatchReceiptV1,
) -> Result<GraphProperty, VectorGenerationStoreErrorV1> {
    semantic_vector_native::encode_generation_receipt(receipt)
        .map(GraphProperty::Bytes)
        .map_err(map_graph_error)
}

pub(super) fn required_generation_receipt(
    row: &GraphEntity,
) -> Result<ProjectionBatchReceiptV1, VectorGenerationStoreErrorV1> {
    match required_property(row, semantic_vector_native::RECEIPT)? {
        GraphProperty::Bytes(value) => {
            semantic_vector_native::decode_generation_receipt(value).map_err(map_graph_error)
        }
        _ => Err(corrupt(format!(
            "semantic vector entity {} has invalid {}",
            row.identity,
            semantic_vector_native::RECEIPT
        ))),
    }
}

pub(super) fn optional_digest_property(value: Option<&ManifestDigest>) -> GraphProperty {
    string_property(value.map(ManifestDigest::as_str).unwrap_or(""))
}

pub(super) fn required_property<'a>(
    row: &'a GraphEntity,
    name: &str,
) -> Result<&'a GraphProperty, VectorGenerationStoreErrorV1> {
    row.properties.get(&property_name(name)?).ok_or_else(|| {
        corrupt(format!(
            "semantic vector entity {} is missing {name}",
            row.identity
        ))
    })
}

pub(super) fn required_string<'a>(
    row: &'a GraphEntity,
    name: &str,
) -> Result<&'a str, VectorGenerationStoreErrorV1> {
    match required_property(row, name)? {
        GraphProperty::String(value) => Ok(value),
        _ => Err(corrupt(format!(
            "semantic vector entity {} has invalid {name}",
            row.identity
        ))),
    }
}

pub(super) fn required_u64(
    row: &GraphEntity,
    name: &str,
) -> Result<u64, VectorGenerationStoreErrorV1> {
    match required_property(row, name)? {
        GraphProperty::I64(value) => u64::try_from(*value).map_err(storage_error),
        _ => Err(corrupt(format!(
            "semantic vector entity {} has invalid {name}",
            row.identity
        ))),
    }
}

pub(super) fn required_bytes<T: DeserializeOwned>(
    row: &GraphEntity,
    name: &str,
) -> Result<T, VectorGenerationStoreErrorV1> {
    match required_property(row, name)? {
        GraphProperty::Bytes(value) => serde_json::from_slice(value).map_err(storage_error),
        _ => Err(corrupt(format!(
            "semantic vector entity {} has invalid {name}",
            row.identity
        ))),
    }
}

pub(super) fn optional_bytes<T: DeserializeOwned>(
    row: &GraphEntity,
    name: &str,
) -> Result<Option<T>, VectorGenerationStoreErrorV1> {
    required_bytes(row, name)
}

pub(super) fn optional_generation(
    row: &GraphEntity,
    name: &str,
) -> Result<Option<VectorGenerationIdV1>, VectorGenerationStoreErrorV1> {
    let value = required_string(row, name)?;
    if value.is_empty() {
        Ok(None)
    } else {
        generation_id(value).map(Some)
    }
}

pub(super) fn require_labels<const N: usize>(
    row: &GraphEntity,
    expected: [&str; N],
) -> Result<(), VectorGenerationStoreErrorV1> {
    let expected = expected
        .into_iter()
        .map(graph_label)
        .collect::<Result<BTreeSet<_>, _>>()?;
    if row.labels == expected {
        Ok(())
    } else {
        Err(corrupt(format!(
            "semantic vector entity {} has invalid labels",
            row.identity
        )))
    }
}

pub(super) fn digest(value: &str) -> Result<ManifestDigest, VectorGenerationStoreErrorV1> {
    ManifestDigest::try_from(value.to_owned()).map_err(storage_error)
}

pub(super) fn content_digest(value: &str) -> Result<ContentDigest, VectorGenerationStoreErrorV1> {
    ContentDigest::try_from(value.to_owned()).map_err(storage_error)
}

pub(super) fn generation_id(
    value: &str,
) -> Result<VectorGenerationIdV1, VectorGenerationStoreErrorV1> {
    digest(value).map(VectorGenerationIdV1::new)
}

pub(super) fn build_id(
    value: &str,
) -> Result<VectorGenerationBuildIdV1, VectorGenerationStoreErrorV1> {
    digest(value).map(VectorGenerationBuildIdV1)
}

pub(super) fn parse_id<T>(value: &str) -> Result<T, VectorGenerationStoreErrorV1>
where
    T: TryFrom<String>,
    T::Error: std::fmt::Display,
{
    T::try_from(value.to_owned()).map_err(storage_error)
}

pub(super) fn corrupt(message: impl Into<String>) -> VectorGenerationStoreErrorV1 {
    VectorGenerationStoreErrorV1::Corrupt(message.into())
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::{
        CodeChunkProjectionReceiptV1, CodeGenerationId, CodeSearchChunkId, ContentDigest,
        ManifestDigest, ProjectionBatchReceiptV1, ProjectionKeyV1, ProjectionKindV1,
        ProjectionOperationV1, ProjectionOutcomeV1,
    };
    use tracedecay_graph_db::{GraphProperty, MAX_GRAPH_PROPERTY_VALUE_BYTES};

    use super::{bytes_property, entity, generation_receipt_property, required_generation_receipt};

    fn digest(seed: u8) -> ManifestDigest {
        ManifestDigest::try_from(format!("sha256:{:064x}", seed as u64)).expect("digest")
    }

    fn content(seed: u8) -> ContentDigest {
        ContentDigest::try_from(format!("sha256:{:064x}", seed as u64)).expect("content digest")
    }

    fn fat_page_receipt(chunk_id_len: usize) -> ProjectionBatchReceiptV1 {
        let projection_key = ProjectionKeyV1 {
            kind: ProjectionKindV1::Embedding,
            schema_revision: "tracedecay.embedding-projection.v1".to_owned(),
            profile_digest: digest(1),
        };
        let source_generation =
            CodeGenerationId::try_from("generation.evaluation-clean-measurement-source".to_owned())
                .expect("source generation");
        let request_digest = digest(2);
        let source_manifest = digest(3);
        let chunk_stem = "a".repeat(chunk_id_len.saturating_sub(8));
        let receipts = (0..512)
            .map(|index| {
                let chunk_id = CodeSearchChunkId::try_from(format!("{chunk_stem}{index:08}"))
                    .expect("chunk id");
                CodeChunkProjectionReceiptV1 {
                    projection_key: projection_key.clone(),
                    request_digest: request_digest.clone(),
                    prior_generation: None,
                    source_generation: source_generation.clone(),
                    source_manifest_digest: source_manifest.clone(),
                    chunk_id,
                    prior_chunk_digest: None,
                    current_chunk_digest: Some(content(4)),
                    operation: ProjectionOperationV1::Added,
                    outcome: ProjectionOutcomeV1::Applied,
                    output_digest: Some(content(5)),
                }
            })
            .collect();
        ProjectionBatchReceiptV1 {
            target_projection_key: projection_key,
            request_digest,
            source_generation,
            source_manifest_digest: source_manifest,
            receipts,
            reused_count: 0,
            publication_digest: digest(6),
        }
    }

    fn fat_reused_receipt(count: usize) -> ProjectionBatchReceiptV1 {
        let mut receipt = fat_page_receipt(32);
        receipt.reused_count = u64::try_from(count).expect("reused count");
        let template = receipt.receipts[0].clone();
        receipt.receipts = (0..count)
            .map(|index| {
                let mut chunk = template.clone();
                chunk.chunk_id =
                    CodeSearchChunkId::try_from(format!("chunk-{index:08}")).expect("chunk id");
                chunk.operation = ProjectionOperationV1::Reused;
                chunk.outcome = ProjectionOutcomeV1::Reused;
                chunk.output_digest = None;
                chunk
            })
            .collect();
        receipt
    }

    #[test]
    fn incremental_reused_corpus_receipt_exceeds_property_ceiling_until_paged() {
        let receipt = fat_reused_receipt(21_700);
        let encoded = serde_json::to_vec(&receipt).expect("encode fat reused receipt");
        assert!(
            encoded.len() > MAX_GRAPH_PROPERTY_VALUE_BYTES,
            "unsplit reused corpus must be the named 1 MiB capacity miss, got {}",
            encoded.len()
        );
        let error = entity(
            "generation-receipt:reused-fat",
            ["semantic-vector-generation-receipt-v1"],
            [("receipt", bytes_property(&receipt).expect("bytes"))],
        )
        .expect_err("fat reused receipt must fail entity admission");
        let message = error.to_string();
        assert!(
            message.contains("capacity budget is exhausted (limit 1048576)"),
            "expected named capacity 1 MiB, got {message}"
        );
        let slim_unsplit = generation_receipt_property(&receipt).expect("slim reused receipt");
        let GraphProperty::Bytes(unsplit_bytes) = &slim_unsplit else {
            panic!("receipt property must be bytes");
        };
        assert!(
            unsplit_bytes.len() > MAX_GRAPH_PROPERTY_VALUE_BYTES,
            "identity-once persist still exceeds 1 MiB for an unsplit reused corpus, got {}",
            unsplit_bytes.len()
        );
        let paged = fat_reused_receipt(512);
        let property = generation_receipt_property(&paged).expect("slim paged receipt");
        let GraphProperty::Bytes(bytes) = &property else {
            panic!("receipt property must be bytes");
        };
        assert!(
            bytes.len() < MAX_GRAPH_PROPERTY_VALUE_BYTES,
            "a 512-item slim reused page must stay under 1 MiB, got {}",
            bytes.len()
        );
    }

    #[test]
    fn slim_generation_receipt_stays_under_property_ceiling_and_round_trips() {
        let receipt = fat_page_receipt(400);
        let property = generation_receipt_property(&receipt).expect("slim receipt");
        let GraphProperty::Bytes(bytes) = &property else {
            panic!("receipt property must be bytes");
        };
        assert!(
            bytes.len() < MAX_GRAPH_PROPERTY_VALUE_BYTES,
            "slim page receipt must stay under 1 MiB, got {}",
            bytes.len()
        );
        let row = entity(
            "generation-receipt:slim",
            ["semantic-vector-generation-receipt-v1"],
            [("receipt", property)],
        )
        .expect("slim receipt must admit");
        let restored = required_generation_receipt(&row).expect("decode slim receipt");
        assert_eq!(restored, receipt);
    }

    #[test]
    fn generation_receipt_read_accepts_previously_persisted_fat_form() {
        let receipt = fat_page_receipt(32);
        let row = entity(
            "generation-receipt:legacy",
            ["semantic-vector-generation-receipt-v1"],
            [("receipt", bytes_property(&receipt).expect("fat bytes"))],
        )
        .expect("short fat receipt still fits");
        assert_eq!(
            required_generation_receipt(&row).expect("decode fat receipt"),
            receipt
        );
    }
}
