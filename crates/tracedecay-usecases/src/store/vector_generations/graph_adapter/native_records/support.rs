use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde::de::DeserializeOwned;
use tracedecay_domain::{ContentDigest, ManifestDigest, VectorGenerationIdV1};
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

pub(super) fn optional_bytes_property<T: Serialize>(
    value: &Option<T>,
) -> Result<GraphProperty, VectorGenerationStoreErrorV1> {
    bytes_property(value)
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
