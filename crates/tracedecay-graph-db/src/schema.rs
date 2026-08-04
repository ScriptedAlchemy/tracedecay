use std::collections::{BTreeMap, BTreeSet};

use grafeo_common::types::{EdgeId, Value};
use grafeo_core::graph::lpg::{Edge, Node};

use crate::{
    GraphCommit, GraphDbError, GraphEntity, GraphEntityId, GraphIdempotencyKey, GraphLabel,
    GraphNamespace, GraphProjectionId, GraphProperty, GraphPropertyName, GraphRelation,
    GraphRelationId, GraphRelationKind, GraphVector, VectorMetric,
};

pub(crate) const FORMAT_LABEL: &str = "__tracedecay_graph_db_format";
pub(crate) const FORMAT_VERSION_PROPERTY: &str = "__tracedecay_graph_db_version";
pub(crate) const SCHEMA_PROPERTY: &str = "__tracedecay_graph_db_schema";
pub(crate) const FINAL_SCHEMA: &str = "native-scalars-v1";
pub(crate) const SEQUENCE_PROPERTY: &str = "__tracedecay_graph_db_sequence";

pub(crate) const ENTITY_LABEL: &str = "__tracedecay_graph_db_entity";
pub(crate) const RELATION_LABEL: &str = "__tracedecay_graph_db_relation_locator";
pub(crate) const PROJECTION_LABEL: &str = "__tracedecay_graph_db_projection";
pub(crate) const PUBLICATION_LABEL: &str = "__tracedecay_graph_db_publication";

pub(crate) const NAMESPACE_PROPERTY: &str = "__tracedecay_graph_db_namespace";
pub(crate) const PROJECTION_PROPERTY: &str = "__tracedecay_graph_db_projection";
pub(crate) const ENTITY_ID_PROPERTY: &str = "__tracedecay_graph_db_entity_id";
pub(crate) const RELATION_ID_PROPERTY: &str = "__tracedecay_graph_db_relation_id";
pub(crate) const RELATION_FROM_PROPERTY: &str = "__tracedecay_graph_db_relation_from";
pub(crate) const RELATION_TO_PROPERTY: &str = "__tracedecay_graph_db_relation_to";
pub(crate) const RELATION_KIND_PROPERTY: &str = "__tracedecay_graph_db_relation_kind";
pub(crate) const RELATION_EDGE_PROPERTY: &str = "__tracedecay_graph_db_relation_edge";
pub(crate) const ENTITY_KEY_PROPERTY: &str = "__tracedecay_graph_db_entity_key";
pub(crate) const RELATION_KEY_PROPERTY: &str = "__tracedecay_graph_db_relation_key";
pub(crate) const PROJECTION_KEY_PROPERTY: &str = "__tracedecay_graph_db_projection_key";
pub(crate) const PUBLICATION_KEY_PROPERTY: &str = "__tracedecay_graph_db_publication_key";
pub(crate) const SOURCE_GENERATION_PROPERTY: &str = "__tracedecay_graph_db_source_generation";
pub(crate) const WATERMARK_PROPERTY: &str = "__tracedecay_graph_db_watermark";
pub(crate) const DIGEST_PROPERTY: &str = "__tracedecay_graph_db_digest";
pub(crate) const PUBLICATION_DIGEST_PROPERTY: &str = "__tracedecay_graph_db_publication_digest";
pub(crate) const IDEMPOTENCY_KEY_PROPERTY: &str = "__tracedecay_graph_db_idempotency_key";
pub(crate) const COMMIT_SEQUENCE_PROPERTY: &str = "__tracedecay_graph_db_commit_sequence";

const DOMAIN_LABEL_PREFIX: &str = "__tracedecay_graph_db_label_";
const OWNER_LABEL_PREFIX: &str = "__tracedecay_graph_db_owner_";
const OWNER_DOMAIN_LABEL_PREFIX: &str = "__tracedecay_graph_db_owner_label_";
const ENTITY_KEY_LABEL_PREFIX: &str = "__tracedecay_graph_db_entity_key_";
const RELATION_KEY_LABEL_PREFIX: &str = "__tracedecay_graph_db_relation_key_";
const RELATION_EDGE_LABEL_PREFIX: &str = "__tracedecay_graph_db_relation_edge_";
const RELATION_OWNER_LABEL_PREFIX: &str = "__tracedecay_graph_db_relation_owner_";
const PROJECTION_STATE_LABEL_PREFIX: &str = "__tracedecay_graph_db_projection_state_";
const PUBLICATION_KEY_LABEL_PREFIX: &str = "__tracedecay_graph_db_publication_key_";
const RELATION_TYPE_PREFIX: &str = "__tracedecay_graph_db_relation_";
const PROPERTY_PREFIX: &str = "__tracedecay_graph_db_property_";
const VECTOR_PREFIX: &str = "__tracedecay_graph_db_vector_";

pub(crate) const INDEXED_PROPERTIES: [&str; 5] = [
    ENTITY_KEY_PROPERTY,
    RELATION_KEY_PROPERTY,
    RELATION_EDGE_PROPERTY,
    PROJECTION_KEY_PROPERTY,
    PUBLICATION_KEY_PROPERTY,
];

pub(crate) fn stable_key(namespace: &GraphNamespace, identity: &str) -> String {
    format!(
        "{}:{}",
        hex::encode(namespace.as_str().as_bytes()),
        hex::encode(identity.as_bytes())
    )
}

pub(crate) fn projection_key(namespace: &GraphNamespace, projection: &GraphProjectionId) -> String {
    stable_key(namespace, projection.as_str())
}

fn key_label(prefix: &str, key: &str) -> String {
    format!("{prefix}{}", hex::encode(key.as_bytes()))
}

pub(crate) fn entity_key_label(namespace: &GraphNamespace, identity: &GraphEntityId) -> String {
    key_label(
        ENTITY_KEY_LABEL_PREFIX,
        &stable_key(namespace, identity.as_str()),
    )
}

pub(crate) fn relation_key_label(namespace: &GraphNamespace, identity: &GraphRelationId) -> String {
    key_label(
        RELATION_KEY_LABEL_PREFIX,
        &stable_key(namespace, identity.as_str()),
    )
}

pub(crate) fn relation_edge_label(edge: EdgeId) -> String {
    format!("{RELATION_EDGE_LABEL_PREFIX}{}", edge.as_u64())
}

pub(crate) fn relation_projection_label(
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
) -> String {
    key_label(
        RELATION_OWNER_LABEL_PREFIX,
        &projection_key(namespace, projection),
    )
}

pub(crate) fn projection_state_label(
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
) -> String {
    key_label(
        PROJECTION_STATE_LABEL_PREFIX,
        &projection_key(namespace, projection),
    )
}

pub(crate) fn publication_key_label(
    namespace: &GraphNamespace,
    identity: &GraphIdempotencyKey,
) -> String {
    key_label(
        PUBLICATION_KEY_LABEL_PREFIX,
        &stable_key(namespace, identity.as_str()),
    )
}

pub(crate) fn entity_projection_label(
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
) -> String {
    format!(
        "{OWNER_LABEL_PREFIX}{}_{}",
        hex::encode(namespace.as_str().as_bytes()),
        hex::encode(projection.as_str().as_bytes())
    )
}

pub(crate) fn entity_projection_domain_label(
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    label: &GraphLabel,
) -> String {
    format!(
        "{OWNER_DOMAIN_LABEL_PREFIX}{}_{}_{}",
        hex::encode(namespace.as_str().as_bytes()),
        hex::encode(projection.as_str().as_bytes()),
        hex::encode(label.as_str().as_bytes())
    )
}

pub(crate) fn relation_type_for_kind(kind: &GraphRelationKind) -> String {
    format!(
        "{RELATION_TYPE_PREFIX}{}",
        hex::encode(kind.as_str().as_bytes())
    )
}

pub(crate) fn relation_kind_from_type(value: &str) -> Result<GraphRelationKind, GraphDbError> {
    let encoded =
        value
            .strip_prefix(RELATION_TYPE_PREFIX)
            .ok_or_else(|| GraphDbError::Corrupt {
                message: "relation edge has a foreign native type".to_owned(),
            })?;
    GraphRelationKind::new(decode_utf8(encoded, "relation kind")?)
        .map_err(|error| persisted_validation_error("relation kind", error))
}

pub(crate) fn entity_labels(
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    labels: &BTreeSet<GraphLabel>,
) -> Vec<String> {
    let mut native = vec![
        ENTITY_LABEL.to_owned(),
        entity_projection_label(namespace, projection),
    ];
    for label in labels {
        native.push(format!(
            "{DOMAIN_LABEL_PREFIX}{}",
            hex::encode(label.as_str().as_bytes())
        ));
        native.push(entity_projection_domain_label(namespace, projection, label));
    }
    native
}

pub(crate) fn relation_locator_labels(
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    relation: &GraphRelation,
    edge: EdgeId,
) -> Vec<String> {
    vec![
        RELATION_LABEL.to_owned(),
        relation_key_label(namespace, &relation.identity),
        relation_edge_label(edge),
        relation_projection_label(namespace, projection),
    ]
}

pub(crate) fn decode_entity_labels(
    labels: impl IntoIterator<Item = impl AsRef<str>>,
) -> Result<BTreeSet<GraphLabel>, GraphDbError> {
    labels
        .into_iter()
        .filter_map(|label| {
            label
                .as_ref()
                .strip_prefix(DOMAIN_LABEL_PREFIX)
                .map(ToOwned::to_owned)
        })
        .map(|encoded| {
            GraphLabel::new(decode_utf8(&encoded, "entity label")?)
                .map_err(|error| persisted_validation_error("entity label", error))
        })
        .collect()
}

pub(crate) fn entity_properties(
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    entity: &GraphEntity,
) -> Vec<(String, Value)> {
    let mut properties = vec![
        (
            ENTITY_KEY_PROPERTY.to_owned(),
            Value::from(stable_key(namespace, entity.identity.as_str())),
        ),
        (
            PROJECTION_KEY_PROPERTY.to_owned(),
            Value::from(projection_key(namespace, projection)),
        ),
        (
            NAMESPACE_PROPERTY.to_owned(),
            Value::from(namespace.as_str()),
        ),
        (
            PROJECTION_PROPERTY.to_owned(),
            Value::from(projection.as_str()),
        ),
        (
            ENTITY_ID_PROPERTY.to_owned(),
            Value::from(entity.identity.as_str()),
        ),
    ];
    properties.extend(
        entity
            .properties
            .iter()
            .map(|(name, property)| encode_graph_property(name, property)),
    );
    properties
}

pub(crate) fn relation_properties(
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    relation: &GraphRelation,
    edge: EdgeId,
) -> Result<Vec<(String, Value)>, GraphDbError> {
    let edge = i64::try_from(edge.as_u64()).map_err(|_| GraphDbError::Corrupt {
        message: "Grafeo edge identity exceeds the persisted scalar range".to_owned(),
    })?;
    let mut properties = vec![
        (
            RELATION_KEY_PROPERTY.to_owned(),
            Value::from(stable_key(namespace, relation.identity.as_str())),
        ),
        (
            PROJECTION_KEY_PROPERTY.to_owned(),
            Value::from(projection_key(namespace, projection)),
        ),
        (
            NAMESPACE_PROPERTY.to_owned(),
            Value::from(namespace.as_str()),
        ),
        (
            PROJECTION_PROPERTY.to_owned(),
            Value::from(projection.as_str()),
        ),
        (
            RELATION_ID_PROPERTY.to_owned(),
            Value::from(relation.identity.as_str()),
        ),
        (
            RELATION_FROM_PROPERTY.to_owned(),
            Value::from(relation.from.as_str()),
        ),
        (
            RELATION_TO_PROPERTY.to_owned(),
            Value::from(relation.to.as_str()),
        ),
        (
            RELATION_KIND_PROPERTY.to_owned(),
            Value::from(relation.kind.as_str()),
        ),
        (RELATION_EDGE_PROPERTY.to_owned(), Value::from(edge)),
    ];
    properties.extend(
        relation
            .properties
            .iter()
            .map(|(name, property)| encode_graph_property(name, property)),
    );
    Ok(properties)
}

pub(crate) fn edge_properties(
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    relation: &GraphRelation,
) -> Vec<(String, Value)> {
    let mut properties = vec![
        (
            RELATION_KEY_PROPERTY.to_owned(),
            Value::from(stable_key(namespace, relation.identity.as_str())),
        ),
        (
            PROJECTION_KEY_PROPERTY.to_owned(),
            Value::from(projection_key(namespace, projection)),
        ),
        (
            NAMESPACE_PROPERTY.to_owned(),
            Value::from(namespace.as_str()),
        ),
        (
            PROJECTION_PROPERTY.to_owned(),
            Value::from(projection.as_str()),
        ),
        (
            RELATION_ID_PROPERTY.to_owned(),
            Value::from(relation.identity.as_str()),
        ),
        (
            RELATION_FROM_PROPERTY.to_owned(),
            Value::from(relation.from.as_str()),
        ),
        (
            RELATION_TO_PROPERTY.to_owned(),
            Value::from(relation.to.as_str()),
        ),
        (
            RELATION_KIND_PROPERTY.to_owned(),
            Value::from(relation.kind.as_str()),
        ),
    ];
    properties.extend(
        relation
            .properties
            .iter()
            .map(|(name, property)| encode_graph_property(name, property)),
    );
    properties
}

pub(crate) fn projection_properties(
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    commit: &GraphCommit,
) -> Result<Vec<(String, Value)>, GraphDbError> {
    let mut properties = vec![
        (
            PROJECTION_KEY_PROPERTY.to_owned(),
            Value::from(projection_key(namespace, projection)),
        ),
        (
            NAMESPACE_PROPERTY.to_owned(),
            Value::from(namespace.as_str()),
        ),
        (
            PROJECTION_PROPERTY.to_owned(),
            Value::from(projection.as_str()),
        ),
    ];
    properties.extend(commit_properties(commit)?);
    Ok(properties)
}

pub(crate) fn publication_properties(
    namespace: &GraphNamespace,
    key: &GraphIdempotencyKey,
    publication_digest: &str,
    commit: &GraphCommit,
) -> Result<Vec<(String, Value)>, GraphDbError> {
    let mut properties = vec![
        (
            PUBLICATION_KEY_PROPERTY.to_owned(),
            Value::from(stable_key(namespace, key.as_str())),
        ),
        (
            NAMESPACE_PROPERTY.to_owned(),
            Value::from(namespace.as_str()),
        ),
        (
            IDEMPOTENCY_KEY_PROPERTY.to_owned(),
            Value::from(key.as_str()),
        ),
        (
            PUBLICATION_DIGEST_PROPERTY.to_owned(),
            Value::from(publication_digest),
        ),
    ];
    properties.extend(commit_properties(commit)?);
    Ok(properties)
}

fn commit_properties(commit: &GraphCommit) -> Result<Vec<(String, Value)>, GraphDbError> {
    let sequence = i64::try_from(commit.sequence)
        .map_err(|_| GraphDbError::unavailable("graph commit sequence exceeds i64"))?;
    Ok(vec![
        (COMMIT_SEQUENCE_PROPERTY.to_owned(), Value::from(sequence)),
        (
            SOURCE_GENERATION_PROPERTY.to_owned(),
            Value::from(commit.source_generation.as_str()),
        ),
        (
            WATERMARK_PROPERTY.to_owned(),
            Value::from(commit.watermark.as_str()),
        ),
        (
            DIGEST_PROPERTY.to_owned(),
            Value::from(commit.digest.as_str()),
        ),
    ])
}

pub(crate) fn decode_entity(node: &Node) -> Result<GraphEntity, GraphDbError> {
    require_label(node, ENTITY_LABEL, "entity")?;
    let identity = GraphEntityId::new(required_string(
        node.get_property(ENTITY_ID_PROPERTY),
        "entity identity",
    )?)
    .map_err(|error| persisted_validation_error("entity identity", error))?;
    let entity = GraphEntity::new(
        identity,
        decode_entity_labels(node.labels.iter())?,
        decode_graph_properties(
            node.properties_as_btree()
                .into_iter()
                .map(|(key, value)| (key.as_str().to_owned(), value)),
        )?,
    )
    .map_err(|error| persisted_validation_error("entity", error))?;
    Ok(entity)
}

pub(crate) fn decode_relation(locator: &Node, edge: &Edge) -> Result<GraphRelation, GraphDbError> {
    require_label(locator, RELATION_LABEL, "relation locator")?;
    let stored_edge = required_i64(
        locator.get_property(RELATION_EDGE_PROPERTY),
        "relation edge identity",
    )?;
    if stored_edge < 0 || u64::try_from(stored_edge).ok() != Some(edge.id.as_u64()) {
        return Err(GraphDbError::Corrupt {
            message: "relation locator does not match its Grafeo edge".to_owned(),
        });
    }
    let identity = GraphRelationId::new(required_string(
        locator.get_property(RELATION_ID_PROPERTY),
        "relation identity",
    )?)
    .map_err(|error| persisted_validation_error("relation identity", error))?;
    let from = GraphEntityId::new(required_string(
        locator.get_property(RELATION_FROM_PROPERTY),
        "relation source",
    )?)
    .map_err(|error| persisted_validation_error("relation source", error))?;
    let to = GraphEntityId::new(required_string(
        locator.get_property(RELATION_TO_PROPERTY),
        "relation target",
    )?)
    .map_err(|error| persisted_validation_error("relation target", error))?;
    let kind = GraphRelationKind::new(required_string(
        locator.get_property(RELATION_KIND_PROPERTY),
        "relation kind",
    )?)
    .map_err(|error| persisted_validation_error("relation kind", error))?;
    if relation_kind_from_type(edge.edge_type.as_str())? != kind {
        return Err(GraphDbError::Corrupt {
            message: "relation kind does not match its native edge type".to_owned(),
        });
    }
    GraphRelation::new(
        identity,
        from,
        to,
        kind,
        decode_graph_properties(
            locator
                .properties_as_btree()
                .into_iter()
                .map(|(key, value)| (key.as_str().to_owned(), value)),
        )?,
    )
    .map_err(|error| persisted_validation_error("relation", error))
}

pub(crate) fn decode_graph_properties(
    properties: impl IntoIterator<Item = (impl AsRef<str>, Value)>,
) -> Result<BTreeMap<GraphPropertyName, GraphProperty>, GraphDbError> {
    let mut decoded = BTreeMap::new();
    for (key, value) in properties {
        let key = key.as_ref();
        if matches!(value, Value::Null) {
            continue;
        }
        let Some(encoded) = key.strip_prefix(PROPERTY_PREFIX) else {
            if let Some(vector) = decode_vector_property(key, value.clone())?
                && decoded.insert(vector.0, vector.1).is_some()
            {
                return Err(GraphDbError::Corrupt {
                    message: "entity repeats a native graph property".to_owned(),
                });
            }
            continue;
        };
        let (tag, name) = encoded
            .split_once('_')
            .ok_or_else(|| GraphDbError::Corrupt {
                message: "native graph property key is malformed".to_owned(),
            })?;
        let name = GraphPropertyName::new(decode_utf8(name, "property name")?)
            .map_err(|error| persisted_validation_error("property name", error))?;
        let property = match (tag, value) {
            ("bool", Value::Bool(value)) => GraphProperty::Bool(value),
            ("i64", Value::Int64(value)) => GraphProperty::I64(value),
            ("f64", Value::Float64(value)) if value.is_finite() => GraphProperty::F64(value),
            ("str", Value::String(value)) => GraphProperty::String(value.to_string()),
            ("bytes", Value::Bytes(value)) => GraphProperty::Bytes(value.to_vec()),
            _ => {
                return Err(GraphDbError::Corrupt {
                    message: format!("native graph property `{key}` has the wrong scalar type"),
                });
            }
        };
        if decoded.insert(name, property).is_some() {
            return Err(GraphDbError::Corrupt {
                message: "entity repeats a native graph property".to_owned(),
            });
        }
    }
    Ok(decoded)
}

fn encode_graph_property(name: &GraphPropertyName, property: &GraphProperty) -> (String, Value) {
    let encoded = hex::encode(name.as_str().as_bytes());
    match property {
        GraphProperty::Bool(value) => (
            format!("{PROPERTY_PREFIX}bool_{encoded}"),
            Value::Bool(*value),
        ),
        GraphProperty::I64(value) => (
            format!("{PROPERTY_PREFIX}i64_{encoded}"),
            Value::Int64(*value),
        ),
        GraphProperty::F64(value) => (
            format!("{PROPERTY_PREFIX}f64_{encoded}"),
            Value::Float64(*value),
        ),
        GraphProperty::String(value) => (
            format!("{PROPERTY_PREFIX}str_{encoded}"),
            Value::from(value.as_str()),
        ),
        GraphProperty::Bytes(value) => (
            format!("{PROPERTY_PREFIX}bytes_{encoded}"),
            Value::Bytes(value.clone().into()),
        ),
        GraphProperty::Vector(vector) => (
            vector_property_key(name, vector.dimension, vector.metric),
            Value::Vector(vector.values.clone().into()),
        ),
    }
}

pub(crate) fn vector_property_key(
    name: &GraphPropertyName,
    dimension: usize,
    metric: VectorMetric,
) -> String {
    format!(
        "{VECTOR_PREFIX}{}_{}_{}",
        hex::encode(name.as_str().as_bytes()),
        dimension,
        metric.storage_tag()
    )
}

fn decode_vector_property(
    key: &str,
    value: Value,
) -> Result<Option<(GraphPropertyName, GraphProperty)>, GraphDbError> {
    let Some(encoded) = key.strip_prefix(VECTOR_PREFIX) else {
        return Ok(None);
    };
    let mut parts = encoded.rsplitn(3, '_');
    let metric = parts.next().and_then(|value| match value {
        "cos" => Some(VectorMetric::Cosine),
        "dot" => Some(VectorMetric::DotProduct),
        "l2" => Some(VectorMetric::Euclidean),
        _ => None,
    });
    let dimension = parts.next().and_then(|value| value.parse::<usize>().ok());
    let name = parts.next();
    let (Some(metric), Some(dimension), Some(name), Value::Vector(values)) =
        (metric, dimension, name, value)
    else {
        return Err(GraphDbError::Corrupt {
            message: format!("native vector property `{key}` is malformed"),
        });
    };
    let name = GraphPropertyName::new(decode_utf8(name, "vector property name")?)
        .map_err(|error| persisted_validation_error("vector property name", error))?;
    let vector = GraphVector::new(values.to_vec(), dimension, metric)
        .map_err(|error| persisted_validation_error("vector", error))?;
    Ok(Some((name, GraphProperty::Vector(vector))))
}

fn require_label(node: &Node, label: &str, description: &str) -> Result<(), GraphDbError> {
    if !node.has_label(label) {
        return Err(GraphDbError::Corrupt {
            message: format!("native {description} has the wrong label"),
        });
    }
    Ok(())
}

pub(crate) fn required_string(
    value: Option<&Value>,
    description: &str,
) -> Result<String, GraphDbError> {
    value
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| GraphDbError::Corrupt {
            message: format!("native {description} is missing or not a string"),
        })
}

pub(crate) fn required_i64(value: Option<&Value>, description: &str) -> Result<i64, GraphDbError> {
    value
        .and_then(Value::as_int64)
        .ok_or_else(|| GraphDbError::Corrupt {
            message: format!("native {description} is missing or not an integer"),
        })
}

fn decode_utf8(value: &str, description: &str) -> Result<String, GraphDbError> {
    let bytes = hex::decode(value).map_err(|error| GraphDbError::Corrupt {
        message: format!("native {description} encoding is invalid: {error}"),
    })?;
    String::from_utf8(bytes).map_err(|error| GraphDbError::Corrupt {
        message: format!("native {description} is not UTF-8: {error}"),
    })
}

fn persisted_validation_error(description: &str, error: GraphDbError) -> GraphDbError {
    GraphDbError::Corrupt {
        message: format!("invalid persisted {description}: {error}"),
    }
}
