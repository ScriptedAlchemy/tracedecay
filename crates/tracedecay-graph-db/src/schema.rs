use std::collections::{BTreeMap, BTreeSet};

use grafeo_common::types::{EdgeId, NodeId, Value};
use grafeo_core::graph::GraphStore;
use grafeo_core::graph::lpg::{Edge, Node};

use crate::limits::{
    MAX_GRAPH_ENTITY_LABEL_BYTES, MAX_GRAPH_ENTITY_LABELS, MAX_GRAPH_IDENTIFIER_BYTES,
    MAX_GRAPH_PROPERTIES, MAX_GRAPH_PROPERTY_AGGREGATE_BYTES, MAX_GRAPH_PROPERTY_VALUE_BYTES,
    MAX_GRAPH_VECTOR_DIMENSION,
};
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
/// Written on projection-state nodes only, so a projection lookup resolves to
/// one node instead of scanning every entity and relation that projection owns.
pub(crate) const PROJECTION_KEY_PROPERTY: &str = "__tracedecay_graph_db_projection_key";
pub(crate) const PUBLICATION_KEY_PROPERTY: &str = "__tracedecay_graph_db_publication_key";
pub(crate) const QUARANTINE_KEY_PROPERTY: &str = "__tracedecay_graph_db_recovery_quarantine_key";
pub(crate) const SOURCE_GENERATION_PROPERTY: &str = "__tracedecay_graph_db_source_generation";
pub(crate) const WATERMARK_PROPERTY: &str = "__tracedecay_graph_db_watermark";
pub(crate) const DIGEST_PROPERTY: &str = "__tracedecay_graph_db_digest";
pub(crate) const GENERATION_DEPENDENCY_DIGEST_PROPERTY: &str =
    "__tracedecay_graph_db_generation_dependency_digest";
pub(crate) const PUBLICATION_DIGEST_PROPERTY: &str = "__tracedecay_graph_db_publication_digest";
pub(crate) const PUBLICATION_INPUT_DIGEST_PROPERTY: &str =
    "__tracedecay_graph_db_publication_input_digest";
pub(crate) const IDEMPOTENCY_KEY_PROPERTY: &str = "__tracedecay_graph_db_idempotency_key";
pub(crate) const COMMIT_SEQUENCE_PROPERTY: &str = "__tracedecay_graph_db_commit_sequence";

const DOMAIN_LABEL_PREFIX: &str = "__tracedecay_graph_db_label_";
const OWNER_LABEL_PREFIX: &str = "__tracedecay_graph_db_owner_";
const OWNER_DOMAIN_LABEL_PREFIX: &str = "__tracedecay_graph_db_owner_label_";
const RELATION_OWNER_LABEL_PREFIX: &str = "__tracedecay_graph_db_relation_owner_";
const RELATION_TYPE_PREFIX: &str = "__tracedecay_graph_db_relation_";
const PROPERTY_PREFIX: &str = "__tracedecay_graph_db_property_";
const VECTOR_PREFIX: &str = "__tracedecay_graph_db_vector_";

/// The unique-key indexes every native lookup resolves through.
///
/// Each one addresses exactly one record kind, so a hit is a point read rather
/// than a scan the caller has to filter. They replaced a synthetic key *label*
/// per record: labels become columnar node tables, and one table per entity
/// exhausts grafeo's `u16` table id (32,767) on any real repository graph.
pub(crate) const INDEXED_PROPERTIES: [&str; 6] = [
    ENTITY_KEY_PROPERTY,
    RELATION_KEY_PROPERTY,
    RELATION_EDGE_PROPERTY,
    PROJECTION_KEY_PROPERTY,
    PUBLICATION_KEY_PROPERTY,
    QUARANTINE_KEY_PROPERTY,
];

pub(crate) fn encoded_namespace_key(namespace: &GraphNamespace) -> String {
    hex::encode(namespace.as_str().as_bytes())
}

pub(crate) fn stable_key_from_encoded(encoded_namespace: &str, identity: &str) -> String {
    format!("{}:{}", encoded_namespace, hex::encode(identity.as_bytes()))
}

pub(crate) fn stable_key(namespace: &GraphNamespace, identity: &str) -> String {
    stable_key_from_encoded(&encoded_namespace_key(namespace), identity)
}

pub(crate) fn projection_key(namespace: &GraphNamespace, projection: &GraphProjectionId) -> String {
    stable_key(namespace, projection.as_str())
}

fn key_label(prefix: &str, key: &str) -> String {
    format!("{prefix}{}", hex::encode(key.as_bytes()))
}

/// The indexed unique-key value for one entity.
///
/// Entity identity resolves through [`ENTITY_KEY_PROPERTY`], never through a
/// synthetic per-entity label. A label index would mint one native label — and
/// therefore one columnar node table — per entity, which caps out at grafeo's
/// `u16` table id long before a real repository graph is loaded.
pub(crate) fn entity_key_value(namespace: &GraphNamespace, identity: &GraphEntityId) -> Value {
    Value::from(stable_key(namespace, identity.as_str()))
}

/// The indexed unique-key value for one relation locator. See
/// [`entity_key_value`] for why this is a property rather than a label.
pub(crate) fn relation_key_value(namespace: &GraphNamespace, identity: &GraphRelationId) -> Value {
    Value::from(stable_key(namespace, identity.as_str()))
}

/// The indexed unique-key value for a relation locator's native edge.
///
/// Stored as the same `i64` scalar [`relation_properties`] writes, so the
/// lookup and the persisted row cannot drift.
pub(crate) fn relation_edge_value(edge: EdgeId) -> Result<Value, GraphDbError> {
    i64::try_from(edge.as_u64())
        .map(Value::from)
        .map_err(|_| GraphDbError::Corrupt {
            message: "Grafeo edge identity exceeds the persisted scalar range".to_owned(),
        })
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

/// The indexed unique-key value for one projection-state node.
///
/// [`PROJECTION_KEY_PROPERTY`] is written on projection-state nodes only, so
/// this resolves to at most one node without scanning the projection's rows.
pub(crate) fn projection_state_key_value(
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
) -> Value {
    Value::from(projection_key(namespace, projection))
}

/// The indexed unique-key value for one publication receipt.
pub(crate) fn publication_key_value(
    namespace: &GraphNamespace,
    identity: &GraphIdempotencyKey,
) -> Value {
    Value::from(stable_key(namespace, identity.as_str()))
}

pub(crate) fn entity_projection_label(
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
) -> String {
    format!(
        "{OWNER_LABEL_PREFIX}{}_{}",
        encoded_namespace_key(namespace),
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
    let namespace_hex = encoded_namespace_key(namespace);
    let projection_hex = hex::encode(projection.as_str().as_bytes());
    let mut native = vec![
        ENTITY_LABEL.to_owned(),
        format!("{OWNER_LABEL_PREFIX}{namespace_hex}_{projection_hex}"),
    ];
    for label in labels {
        let label_hex = hex::encode(label.as_str().as_bytes());
        native.push(format!("{DOMAIN_LABEL_PREFIX}{label_hex}"));
        native.push(format!(
            "{OWNER_DOMAIN_LABEL_PREFIX}{namespace_hex}_{projection_hex}_{label_hex}"
        ));
    }
    native
}

pub(crate) fn relation_locator_labels(
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
) -> Vec<String> {
    vec![
        RELATION_LABEL.to_owned(),
        relation_projection_label(namespace, projection),
    ]
}

pub(crate) fn decode_entity_labels(
    labels: impl IntoIterator<Item = impl AsRef<str>>,
) -> Result<BTreeSet<GraphLabel>, GraphDbError> {
    let mut decoded = BTreeSet::new();
    let mut decoded_bytes = 0usize;
    for label in labels {
        let Some(encoded) = label.as_ref().strip_prefix(DOMAIN_LABEL_PREFIX) else {
            continue;
        };
        if decoded.len() >= MAX_GRAPH_ENTITY_LABELS
            || encoded.len() > MAX_GRAPH_IDENTIFIER_BYTES.saturating_mul(2)
        {
            return Err(GraphDbError::Corrupt {
                message: "native entity labels exceed their product bound".to_owned(),
            });
        }
        let value = decode_utf8(encoded, "entity label")?;
        decoded_bytes =
            decoded_bytes
                .checked_add(value.len())
                .ok_or_else(|| GraphDbError::Corrupt {
                    message: "native entity label bytes overflow their product bound".to_owned(),
                })?;
        if decoded_bytes > MAX_GRAPH_ENTITY_LABEL_BYTES {
            return Err(GraphDbError::Corrupt {
                message: "native entity label bytes exceed their product bound".to_owned(),
            });
        }
        decoded.insert(
            GraphLabel::new(value)
                .map_err(|error| persisted_validation_error("entity label", error))?,
        );
    }
    Ok(decoded)
}

pub(crate) fn entity_properties(
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    entity: &GraphEntity,
) -> Vec<(String, Value)> {
    let encoded_namespace = encoded_namespace_key(namespace);
    let mut properties = vec![
        (
            ENTITY_KEY_PROPERTY.to_owned(),
            Value::from(stable_key_from_encoded(
                &encoded_namespace,
                entity.identity.as_str(),
            )),
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
    let encoded_namespace = encoded_namespace_key(namespace);
    let mut properties = vec![
        (
            RELATION_KEY_PROPERTY.to_owned(),
            Value::from(stable_key_from_encoded(
                &encoded_namespace,
                relation.identity.as_str(),
            )),
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
    let encoded_namespace = encoded_namespace_key(namespace);
    let mut properties = vec![
        (
            RELATION_KEY_PROPERTY.to_owned(),
            Value::from(stable_key_from_encoded(
                &encoded_namespace,
                relation.identity.as_str(),
            )),
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
            Value::from(stable_key_from_encoded(
                &encoded_namespace_key(namespace),
                projection.as_str(),
            )),
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
    input_digest: &str,
    commit: &GraphCommit,
) -> Result<Vec<(String, Value)>, GraphDbError> {
    let mut properties = vec![
        (
            PUBLICATION_KEY_PROPERTY.to_owned(),
            Value::from(stable_key_from_encoded(
                &encoded_namespace_key(namespace),
                key.as_str(),
            )),
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
        (
            PUBLICATION_INPUT_DIGEST_PROPERTY.to_owned(),
            Value::from(input_digest),
        ),
    ];
    properties.extend(commit_properties(commit)?);
    Ok(properties)
}

fn commit_properties(commit: &GraphCommit) -> Result<Vec<(String, Value)>, GraphDbError> {
    let sequence = i64::try_from(commit.sequence)
        .map_err(|_| GraphDbError::unavailable("graph commit sequence exceeds i64"))?;
    let mut properties = vec![
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
    ];
    if let Some(dependency_digest) = &commit.generation_dependency_digest {
        properties.push((
            GENERATION_DEPENDENCY_DIGEST_PROPERTY.to_owned(),
            Value::from(dependency_digest.as_str()),
        ));
    }
    Ok(properties)
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
        decode_entity_labels(native_labels(node))?,
        decode_graph_properties(
            node.properties
                .iter()
                .map(|(key, value)| (key.as_str(), value.clone())),
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
                .properties
                .iter()
                .map(|(key, value)| (key.as_str(), value.clone())),
        )?,
    )
    .map_err(|error| persisted_validation_error("relation", error))
}

/// Identity-only relation decode: namespace, projection, kind, endpoints, and
/// identity. Skips [`decode_graph_properties`] so an ID fan-out does not pay
/// property allocation for rows the caller will discard.
pub(crate) struct DecodedRelationIdentity {
    pub identity: GraphRelationId,
    pub projection: GraphProjectionId,
    pub kind: GraphRelationKind,
    pub from: GraphEntityId,
    pub to: GraphEntityId,
}

pub(crate) fn decode_relation_identity(
    edge: &Edge,
    namespace: &GraphNamespace,
) -> Result<DecodedRelationIdentity, GraphDbError> {
    crate::hotpath_observe::record_relation_identity_decode();
    let stored_namespace =
        required_string(edge.get_property(NAMESPACE_PROPERTY), "relation namespace")?;
    if stored_namespace != namespace.as_str() {
        return Err(GraphDbError::Corrupt {
            message: "outgoing relation belongs to a foreign namespace".to_owned(),
        });
    }
    let projection = GraphProjectionId::new(required_string(
        edge.get_property(PROJECTION_PROPERTY),
        "relation projection",
    )?)
    .map_err(|error| persisted_validation_error("relation projection", error))?;
    let kind = relation_kind_from_type(edge.edge_type.as_str())?;
    let scalar_kind = required_string(edge.get_property(RELATION_KIND_PROPERTY), "relation kind")?;
    if kind.as_str() != scalar_kind {
        return Err(GraphDbError::Corrupt {
            message: "traversal relation native type and kind disagree".to_owned(),
        });
    }
    let identity = GraphRelationId::new(required_string(
        edge.get_property(RELATION_ID_PROPERTY),
        "relation identity",
    )?)
    .map_err(|error| persisted_validation_error("relation identity", error))?;
    let from = GraphEntityId::new(required_string(
        edge.get_property(RELATION_FROM_PROPERTY),
        "relation source",
    )?)
    .map_err(|error| persisted_validation_error("relation source", error))?;
    let to = GraphEntityId::new(required_string(
        edge.get_property(RELATION_TO_PROPERTY),
        "relation target",
    )?)
    .map_err(|error| persisted_validation_error("relation target", error))?;
    Ok(DecodedRelationIdentity {
        identity,
        projection,
        kind,
        from,
        to,
    })
}

pub(crate) fn decode_graph_properties(
    properties: impl IntoIterator<Item = (impl AsRef<str>, Value)>,
) -> Result<BTreeMap<GraphPropertyName, GraphProperty>, GraphDbError> {
    crate::hotpath_observe::record_property_decode();
    let mut decoded = BTreeMap::new();
    let mut decoded_bytes = 0usize;
    for (key, value) in properties {
        let key = key.as_ref();
        if matches!(value, Value::Null) {
            continue;
        }
        let Some(encoded) = key.strip_prefix(PROPERTY_PREFIX) else {
            if let Some((name, property)) = decode_vector_property(key, value.clone())? {
                require_decoded_property_budget(&decoded, &mut decoded_bytes, &name, &property)?;
                if decoded.insert(name, property).is_some() {
                    return Err(GraphDbError::Corrupt {
                        message: "entity repeats a native graph property".to_owned(),
                    });
                }
            }
            continue;
        };
        let (tag, name) = encoded
            .split_once('_')
            .ok_or_else(|| GraphDbError::Corrupt {
                message: "native graph property key is malformed".to_owned(),
            })?;
        if name.len() > MAX_GRAPH_IDENTIFIER_BYTES.saturating_mul(2) {
            return Err(GraphDbError::Corrupt {
                message: "native graph property name exceeds its product bound".to_owned(),
            });
        }
        let name = GraphPropertyName::new(decode_utf8(name, "property name")?)
            .map_err(|error| persisted_validation_error("property name", error))?;
        let property = match (tag, value) {
            ("bool", Value::Bool(value)) => GraphProperty::Bool(value),
            ("i64", Value::Int64(value)) => GraphProperty::I64(value),
            ("f64", Value::Float64(value)) if value.is_finite() => GraphProperty::F64(value),
            ("str", Value::String(value)) if value.len() <= MAX_GRAPH_PROPERTY_VALUE_BYTES => {
                GraphProperty::String(value.to_string())
            }
            ("bytes", Value::Bytes(value)) if value.len() <= MAX_GRAPH_PROPERTY_VALUE_BYTES => {
                GraphProperty::Bytes(value.to_vec())
            }
            _ => {
                return Err(GraphDbError::Corrupt {
                    message: format!("native graph property `{key}` has the wrong scalar type"),
                });
            }
        };
        require_decoded_property_budget(&decoded, &mut decoded_bytes, &name, &property)?;
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
    if dimension == 0
        || dimension > MAX_GRAPH_VECTOR_DIMENSION
        || values.len() != dimension
        || name.len() > MAX_GRAPH_IDENTIFIER_BYTES.saturating_mul(2)
    {
        return Err(GraphDbError::Corrupt {
            message: format!("native vector property `{key}` exceeds its product bound"),
        });
    }
    let name = GraphPropertyName::new(decode_utf8(name, "vector property name")?)
        .map_err(|error| persisted_validation_error("vector property name", error))?;
    let vector = GraphVector::new(values.to_vec(), dimension, metric)
        .map_err(|error| persisted_validation_error("vector", error))?;
    Ok(Some((name, GraphProperty::Vector(vector))))
}

fn require_decoded_property_budget(
    decoded: &BTreeMap<GraphPropertyName, GraphProperty>,
    decoded_bytes: &mut usize,
    name: &GraphPropertyName,
    property: &GraphProperty,
) -> Result<(), GraphDbError> {
    if decoded.len() >= MAX_GRAPH_PROPERTIES {
        return Err(GraphDbError::Corrupt {
            message: "native graph properties exceed their product bound".to_owned(),
        });
    }
    let payload_bytes = match property {
        GraphProperty::Bool(_) => std::mem::size_of::<bool>(),
        GraphProperty::I64(_) => std::mem::size_of::<i64>(),
        GraphProperty::F64(_) => std::mem::size_of::<f64>(),
        GraphProperty::String(value) => value.len(),
        GraphProperty::Bytes(value) => value.len(),
        GraphProperty::Vector(vector) => vector
            .values
            .len()
            .checked_mul(std::mem::size_of::<f32>())
            .ok_or_else(|| GraphDbError::Corrupt {
                message: "native graph vector byte length overflowed".to_owned(),
            })?,
    };
    *decoded_bytes = decoded_bytes
        .checked_add(name.as_str().len())
        .and_then(|bytes| bytes.checked_add(payload_bytes))
        .ok_or_else(|| GraphDbError::Corrupt {
            message: "native graph property bytes overflow their product bound".to_owned(),
        })?;
    if *decoded_bytes > MAX_GRAPH_PROPERTY_AGGREGATE_BYTES {
        return Err(GraphDbError::Corrupt {
            message: "native graph property bytes exceed their product bound".to_owned(),
        });
    }
    Ok(())
}

/// Separates the labels grafeo's columnar builder fuses into one composite key.
///
/// TraceDecay's own labels are ASCII prefixes over hex, so this byte never
/// occurs inside one and the split is unambiguous.
const COMPACT_LABEL_SEPARATOR: char = '|';

/// Every native label `node` carries, whichever store it came from.
///
/// A `CompactStore` files a multi-label node under a *composite* label — the
/// node's label set sorted and joined with `|`
/// (`grafeo-core/src/graph/compact/builder.rs:1129`) — and its `get_node`
/// restores that composite as the node's single label
/// (`compact/graph_store_impl.rs:31`). An entity carries a record label plus
/// owner and domain labels, so reading `node.labels` directly sees one fused
/// string on a compacted generation and none of the labels that were fused
/// into it. Flattening here is what lets one decode path serve both the live
/// `LpgStore` and a compacted base.
pub(crate) fn native_labels(node: &Node) -> impl Iterator<Item = &str> {
    node.labels
        .iter()
        .flat_map(|label| label.as_str().split(COMPACT_LABEL_SEPARATOR))
}

/// Whether `node` carries `label`, reading through a compacted composite key.
pub(crate) fn has_native_label(node: &Node, label: &str) -> bool {
    native_labels(node).any(|stored| stored == label)
}

/// Every label key under which `store` files nodes carrying `label`.
///
/// `nodes_by_label` is an exact-string lookup into the store's label table, so
/// on a compacted base it only answers for the fused composite key, never for
/// one of the labels inside it. Expanding through `all_labels` gives `label`
/// itself on a live `LpgStore`, the composites that fuse it on a compacted
/// base, and both on a layered store whose overlay has taken new writes.
///
/// Falls back to `label` when nothing matches so a caller that feeds this to a
/// `ProjectionSpec` still filters: an empty label set there means *no filter*,
/// which would silently widen the projection to the whole store.
#[hotpath::measure(label = "graph_db.schema.label_keys")]
pub(crate) fn label_keys(store: &dyn GraphStore, label: &str) -> Vec<String> {
    crate::hotpath_observe::record_label_universe_scan();
    let keys: Vec<String> = store
        .all_labels()
        .into_iter()
        .filter(|key| key.split(COMPACT_LABEL_SEPARATOR).any(|part| part == label))
        .collect();
    if keys.is_empty() {
        return vec![label.to_owned()];
    }
    keys
}

/// Every node carrying `label`, across whichever key the store files it under.
///
/// Each node belongs to exactly one label table, so the union needs no dedupe.
#[hotpath::measure(label = "graph_db.schema.nodes_with_label")]
pub(crate) fn nodes_with_label(store: &dyn GraphStore, label: &str) -> Vec<NodeId> {
    let keys = label_keys(store, label);
    if let [only] = keys.as_slice() {
        return store.nodes_by_label(only);
    }
    keys.iter()
        .flat_map(|key| store.nodes_by_label(key))
        .collect()
}

/// How many nodes carry `label`. See [`nodes_with_label`].
#[hotpath::measure(label = "graph_db.schema.nodes_with_label_count")]
pub(crate) fn nodes_with_label_count(store: &dyn GraphStore, label: &str) -> usize {
    label_keys(store, label)
        .iter()
        .map(|key| store.nodes_by_label_count(key))
        .sum()
}

fn require_label(node: &Node, label: &str, description: &str) -> Result<(), GraphDbError> {
    if !has_native_label(node, label) {
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
    match value.and_then(Value::as_str) {
        Some(value) if value.len() <= MAX_GRAPH_IDENTIFIER_BYTES => Ok(value.to_owned()),
        _ => Err(GraphDbError::Corrupt {
            message: format!(
                "native {description} is missing, not a string, or exceeds its product bound"
            ),
        }),
    }
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

#[cfg(test)]
mod tests {
    use grafeo_common::types::Value;

    use super::decode_vector_property;
    use crate::{GraphDbError, MAX_GRAPH_VECTOR_DIMENSION};

    #[test]
    fn persisted_vector_dimension_over_product_limit_is_corrupt() {
        let key = format!(
            "__tracedecay_graph_db_vector_{}_{dimension}_cos",
            hex::encode("embedding"),
            dimension = MAX_GRAPH_VECTOR_DIMENSION + 1,
        );
        let error = decode_vector_property(
            &key,
            Value::Vector(vec![0.0; MAX_GRAPH_VECTOR_DIMENSION + 1].into()),
        )
        .unwrap_err();

        assert!(matches!(error, GraphDbError::Corrupt { .. }));
    }
}
