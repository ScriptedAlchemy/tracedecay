use std::collections::{BTreeMap, BTreeSet};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use grafeo_common::types::{EdgeId, NodeId, PropertyKey, Value};
use grafeo_core::graph::GraphStore;
use grafeo_core::graph::lpg::{Edge, Node};
use sha2::{Digest, Sha256};

use crate::limits::{
    MAX_GRAPH_ENTITY_LABEL_BYTES, MAX_GRAPH_ENTITY_LABELS, MAX_GRAPH_IDENTIFIER_BYTES,
    MAX_GRAPH_PROPERTIES, MAX_GRAPH_PROPERTY_AGGREGATE_BYTES, MAX_GRAPH_PROPERTY_VALUE_BYTES,
};
use crate::{
    GraphCommit, GraphDbError, GraphEntity, GraphEntityId, GraphIdempotencyKey, GraphLabel,
    GraphNamespace, GraphProjectionId, GraphProperty, GraphPropertyName, GraphRelation,
    GraphRelationId, GraphRelationKind,
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

/// Durable `{kind}:{sha256}` stem used for graph entity and relation ids.
///
/// Kind and value are separated by a NUL byte so a kind cannot be smuggled
/// in as a prefix of the value. Code-graph symbols, git topology, and
/// workflow topology all mint ids through this function; the byte layout is
/// already sealed in stored graphs.
#[must_use]
pub fn graph_stable_identity(kind: &str, value: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(kind.as_bytes());
    digest.update([0]);
    digest.update(value.as_bytes());
    format!("{kind}:{}", hex::encode(digest.finalize()))
}

/// Width of the namespace id that leads every unique key.
const NAMESPACE_KEY_ID_BYTES: usize = 8;
const DIGEST_BYTES: usize = 32;
const RAW_IDENTITY_TAG: u8 = 0;
const DIGEST_IDENTITY_TAG: u8 = 1;
/// Leads a compact identity scalar; `validate_opaque` rejects graph
/// identifiers that start with it.
pub(crate) const COMPACT_IDENTITY_MARKER: char = '\u{1}';

/// The short id a namespace contributes to every unique key it owns: the
/// leading bytes of the namespace's SHA-256.
///
/// It is derived rather than allocated so a lookup needs no read and a sealed
/// copy rebuilds identical keys. Every indexed read re-checks the row's
/// namespace scalar, so two namespaces sharing an id surface as `Corrupt`
/// instead of aliasing each other's rows.
pub(crate) type NamespaceKeyId = [u8; NAMESPACE_KEY_ID_BYTES];

pub(crate) fn namespace_key_id(namespace: &GraphNamespace) -> NamespaceKeyId {
    let digest = Sha256::digest(namespace.as_str().as_bytes());
    let mut id = [0; NAMESPACE_KEY_ID_BYTES];
    id.copy_from_slice(&digest[..NAMESPACE_KEY_ID_BYTES]);
    id
}

/// Unique-key bytes for `identity` inside the namespace `namespace_id` names.
///
/// Keys are only ever matched exactly through a property hash index; no scan
/// orders by them. The encoding is injective: a `<kind>:<64 lowercase hex>`
/// identity (every [`graph_stable_identity`]) is stored as its kind followed
/// by the 32 digest bytes, anything else verbatim, and a tag byte separates
/// the two forms.
pub(crate) fn stable_key(namespace_id: &NamespaceKeyId, identity: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(NAMESPACE_KEY_ID_BYTES + 1 + identity.len());
    key.extend_from_slice(namespace_id);
    match digest_identity(identity) {
        Some((kind, digest)) => {
            key.push(DIGEST_IDENTITY_TAG);
            key.extend_from_slice(kind.as_bytes());
            key.extend_from_slice(&digest);
        }
        None => {
            key.push(RAW_IDENTITY_TAG);
            key.extend_from_slice(identity.as_bytes());
        }
    }
    key
}

/// The kind and digest of a `<kind>:<64 lowercase hex>` identity.
fn digest_identity(identity: &str) -> Option<(&str, [u8; DIGEST_BYTES])> {
    identity
        .rsplit_once(':')
        .and_then(|(kind, digest)| Some((kind, decode_lower_hex_digest(digest)?)))
}

/// Unpadded base64url width of a 32-byte digest.
const DIGEST_TEXT_BYTES: usize = 43;

/// The stored scalar for a relation identity, source, or target.
///
/// A `<kind>:<64 lowercase hex>` identity is stored as
/// [`COMPACT_IDENTITY_MARKER`], its kind, and its digest in unpadded
/// base64url; any other identity is stored verbatim. The value stays a
/// string because the sealed compact store keeps `Bytes` in its string
/// dictionary as marked hex, which would double the digest again. Only the
/// canonical spelling compacts and no graph identifier may start with the
/// marker, so [`decode_identity`] restores exactly the identity written.
pub(crate) fn encode_identity(identity: &str) -> Value {
    match digest_identity(identity) {
        Some((kind, digest)) => Value::from(format!(
            "{COMPACT_IDENTITY_MARKER}{kind}{}",
            URL_SAFE_NO_PAD.encode(digest)
        )),
        None => Value::from(identity),
    }
}

/// Reads back an identity written by [`encode_identity`].
pub(crate) fn decode_identity(
    value: Option<&Value>,
    description: &str,
) -> Result<String, GraphDbError> {
    let Some(Value::String(stored)) = value else {
        return Err(GraphDbError::Corrupt {
            message: format!("native {description} is missing or not a string"),
        });
    };
    let identity = match stored.strip_prefix(COMPACT_IDENTITY_MARKER) {
        None => stored.to_string(),
        Some(compact) => {
            let digest = compact
                .len()
                .checked_sub(DIGEST_TEXT_BYTES)
                .filter(|split| compact.is_char_boundary(*split))
                .and_then(|split| {
                    let (kind, digest) = compact.split_at(split);
                    let mut bytes = [0; DIGEST_BYTES];
                    let written = URL_SAFE_NO_PAD.decode_slice(digest, &mut bytes).ok()?;
                    (written == DIGEST_BYTES).then(|| format!("{kind}:{}", hex::encode(bytes)))
                });
            digest.ok_or_else(|| GraphDbError::Corrupt {
                message: format!("native {description} has a malformed compact digest"),
            })?
        }
    };
    if identity.len() > MAX_GRAPH_IDENTIFIER_BYTES {
        return Err(GraphDbError::Corrupt {
            message: format!("native {description} exceeds its product bound"),
        });
    }
    Ok(identity)
}

/// Only the canonical lowercase spelling compacts, so every identity has
/// exactly one key.
fn decode_lower_hex_digest(digest: &str) -> Option<[u8; DIGEST_BYTES]> {
    if digest.len() != DIGEST_BYTES * 2
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let mut bytes = [0; DIGEST_BYTES];
    hex::decode_to_slice(digest, &mut bytes).ok()?;
    Some(bytes)
}

/// A unique key as the indexed scalar: unpadded base64url of
/// [`stable_key`]'s bytes, a string for the same reason as
/// [`encode_identity`].
pub(crate) fn key_value(namespace: &GraphNamespace, identity: &str) -> Value {
    Value::from(URL_SAFE_NO_PAD.encode(stable_key(&namespace_key_id(namespace), identity)))
}

/// The indexed unique-key value for one entity.
///
/// Entity identity resolves through [`ENTITY_KEY_PROPERTY`], never through a
/// synthetic per-entity label. A label index would mint one native label, and
/// therefore one columnar node table, per entity, which caps out at grafeo's
/// `u16` table id long before a real repository graph is loaded.
pub(crate) fn entity_key_value(namespace: &GraphNamespace, identity: &GraphEntityId) -> Value {
    key_value(namespace, identity.as_str())
}

/// The indexed unique-key value for one relation locator. See
/// [`entity_key_value`] for why this is a property rather than a label.
pub(crate) fn relation_key_value(namespace: &GraphNamespace, identity: &GraphRelationId) -> Value {
    key_value(namespace, identity.as_str())
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
    let owner = format!(
        "{}:{}",
        hex::encode(namespace.as_str().as_bytes()),
        hex::encode(projection.as_str().as_bytes())
    );
    format!("{RELATION_OWNER_LABEL_PREFIX}{}", hex::encode(owner))
}

/// The indexed unique-key value for one projection-state node.
///
/// [`PROJECTION_KEY_PROPERTY`] is written on projection-state nodes only, so
/// this resolves to at most one node without scanning the projection's rows.
pub(crate) fn projection_state_key_value(
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
) -> Value {
    key_value(namespace, projection.as_str())
}

/// The indexed unique-key value for one publication receipt.
pub(crate) fn publication_key_value(
    namespace: &GraphNamespace,
    identity: &GraphIdempotencyKey,
) -> Value {
    key_value(namespace, identity.as_str())
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
    let namespace_hex = hex::encode(namespace.as_str().as_bytes());
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
    let mut properties = vec![
        (
            ENTITY_KEY_PROPERTY.to_owned(),
            key_value(namespace, entity.identity.as_str()),
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
            key_value(namespace, relation.identity.as_str()),
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
            encode_identity(relation.identity.as_str()),
        ),
        (
            RELATION_FROM_PROPERTY.to_owned(),
            encode_identity(relation.from.as_str()),
        ),
        (
            RELATION_TO_PROPERTY.to_owned(),
            encode_identity(relation.to.as_str()),
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

/// A native edge carries its owner scalars and payload, never the relation's
/// identity or endpoints: those are owned by its locator node, which
/// [`edge_locator`] resolves through the `RELATION_EDGE` index.
pub(crate) fn edge_properties(
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    relation: &GraphRelation,
) -> Vec<(String, Value)> {
    let mut properties = vec![
        (
            NAMESPACE_PROPERTY.to_owned(),
            Value::from(namespace.as_str()),
        ),
        (
            PROJECTION_PROPERTY.to_owned(),
            Value::from(projection.as_str()),
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
            key_value(namespace, projection.as_str()),
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
            key_value(namespace, key.as_str()),
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
    let identity = GraphRelationId::new(decode_identity(
        locator.get_property(RELATION_ID_PROPERTY),
        "relation identity",
    )?)
    .map_err(|error| persisted_validation_error("relation identity", error))?;
    let from = GraphEntityId::new(decode_identity(
        locator.get_property(RELATION_FROM_PROPERTY),
        "relation source",
    )?)
    .map_err(|error| persisted_validation_error("relation source", error))?;
    let to = GraphEntityId::new(decode_identity(
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

/// Identity-only relation decode: namespace, projection, kind, and identity.
/// Skips endpoint materialization and [`decode_graph_properties`] so an ID
/// fan-out does not allocate fields the caller discards.
pub(crate) struct DecodedRelationIdentity {
    pub identity: GraphRelationId,
    pub projection: GraphProjectionId,
    pub kind: GraphRelationKind,
}

pub(crate) fn decode_relation_identity(
    store: &dyn GraphStore,
    edge: &Edge,
    namespace: &GraphNamespace,
) -> Result<DecodedRelationIdentity, GraphDbError> {
    crate::hotpath_observe::record_relation_identity_decode();
    let stored_namespace =
        required_string(edge.get_property(NAMESPACE_PROPERTY), "relation namespace")?;
    if stored_namespace != namespace.as_str() {
        return Err(GraphDbError::Corrupt {
            message: "relation belongs to a foreign namespace".to_owned(),
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
    let identity = edge_relation_identity(store, edge_locator(store, edge.id)?)?;
    Ok(DecodedRelationIdentity {
        identity,
        projection,
        kind,
    })
}

/// The locator node that owns `edge`'s identity and endpoints.
///
/// Resolved through the `RELATION_EDGE` unique index. A locator deleted in
/// the live store keeps its index entry but loses its properties, so the
/// re-read of the indexed scalar discards such tombstones without
/// materializing the node.
pub(crate) fn edge_locator(store: &dyn GraphStore, edge: EdgeId) -> Result<NodeId, GraphDbError> {
    let value = relation_edge_value(edge)?;
    let key = PropertyKey::new(RELATION_EDGE_PROPERTY);
    let mut locators = store
        .find_nodes_by_property(RELATION_EDGE_PROPERTY, &value)
        .into_iter()
        .filter(|node| store.get_node_property(*node, &key).as_ref() == Some(&value));
    match (locators.next(), locators.next()) {
        (Some(locator), None) => Ok(locator),
        (None, _) => Err(GraphDbError::Corrupt {
            message: "native relation edge has no locator".to_owned(),
        }),
        (Some(_), Some(_)) => Err(GraphDbError::Corrupt {
            message: "native relation edge has duplicate locators".to_owned(),
        }),
    }
}

/// One identity scalar of a relation locator, read without materializing
/// the node.
pub(crate) fn locator_identity(
    store: &dyn GraphStore,
    locator: NodeId,
    property: &str,
    description: &str,
) -> Result<String, GraphDbError> {
    decode_identity(
        store
            .get_node_property(locator, &PropertyKey::new(property))
            .as_ref(),
        description,
    )
}

pub(crate) fn edge_relation_identity(
    store: &dyn GraphStore,
    locator: NodeId,
) -> Result<GraphRelationId, GraphDbError> {
    GraphRelationId::new(locator_identity(
        store,
        locator,
        RELATION_ID_PROPERTY,
        "relation identity",
    )?)
    .map_err(|error| persisted_validation_error("relation identity", error))
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
    }
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
/// A `CompactStore` files a multi-label node under a *composite* label, the
/// node's label set sorted and joined with `|`
/// (`grafeo-core/src/graph/compact/builder.rs:1129`), and its `get_node`
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
mod graph_stable_identity_tests {
    use super::graph_stable_identity;

    #[test]
    fn kind_and_value_stay_separated_by_a_nul() {
        assert_eq!(
            graph_stable_identity("symbol", "occ"),
            "symbol:199f069a8ccddbb90bd0626b5904f52fbb2d92879bdbbf2c5dc29c1ea4ab66fb"
        );
        assert_ne!(
            graph_stable_identity("symbol", "occ"),
            graph_stable_identity("symbolo", "cc")
        );
    }
}

#[cfg(test)]
mod stable_key_tests {
    use std::collections::BTreeSet;

    use grafeo_common::types::Value;

    use super::{
        decode_identity, encode_identity, graph_stable_identity, namespace_key_id, stable_key,
    };
    use crate::GraphNamespace;

    #[test]
    fn a_stable_identity_key_is_the_namespace_id_kind_and_raw_digest() {
        let workspace = namespace_key_id(&GraphNamespace::new("workspace").unwrap());
        let key = stable_key(&workspace, &graph_stable_identity("symbol", "occ"));

        let mut expected = vec![0x21, 0xa3, 0x23, 0x0e, 0x03, 0x77, 0x2a, 0x58, 1];
        expected.extend_from_slice(b"symbol");
        expected.extend_from_slice(
            &hex::decode("199f069a8ccddbb90bd0626b5904f52fbb2d92879bdbbf2c5dc29c1ea4ab66fb")
                .unwrap(),
        );
        assert_eq!(key, expected);
        assert_eq!(key.len(), 47);
        assert_eq!(
            stable_key(&workspace, "entity"),
            [
                &[0x21, 0xa3, 0x23, 0x0e, 0x03, 0x77, 0x2a, 0x58, 0][..],
                b"entity"
            ]
            .concat()
        );
    }

    #[test]
    fn keys_differ_whenever_namespace_or_identity_differs() {
        let digest = "ab".repeat(32);
        let identities = [
            format!("symbol:{digest}"),
            format!("symbol:{}", digest.to_uppercase()),
            format!("symbo:l{digest}"),
            format!("symbol:{digest}0"),
            format!("\u{1}symbol{}", "\u{ab}".repeat(32)),
            "symbol".to_owned(),
            String::new(),
        ];
        let namespaces = ["workspace", "generation:workspace"]
            .map(|namespace| namespace_key_id(&GraphNamespace::new(namespace).unwrap()));
        let keys: BTreeSet<Vec<u8>> = namespaces
            .iter()
            .flat_map(|namespace| {
                identities
                    .iter()
                    .map(|identity| stable_key(namespace, identity))
            })
            .collect();
        assert_eq!(keys.len(), 14);
    }

    #[test]
    fn relation_identities_store_their_digest_compactly_and_read_back_exactly() {
        let identity = graph_stable_identity("edge", "occ");
        assert_eq!(
            identity,
            "edge:a92cf2a4297d812859e387e8efac97838fc420befe35fa0f8b7e94ad9a139ff5"
        );
        let digest = identity.strip_prefix("edge:").unwrap();
        let stored = encode_identity(&identity);

        assert_eq!(
            stored,
            Value::from("\u{1}edgeqSzypCl9gShZ44fo76yXg4_EIL7-NfoPi36UrZoTn_U")
        );
        assert_eq!(stored.as_str().unwrap().len(), 48);
        assert_eq!(
            decode_identity(Some(&stored), "relation").unwrap(),
            identity
        );
        for verbatim in [
            format!("edge:{}", digest.to_uppercase()),
            "relation:a-b".to_owned(),
            format!("edge:{digest}0"),
        ] {
            let stored = encode_identity(&verbatim);
            assert_eq!(stored, Value::from(verbatim.as_str()));
            assert_eq!(
                decode_identity(Some(&stored), "relation").unwrap(),
                verbatim
            );
        }
    }
}
