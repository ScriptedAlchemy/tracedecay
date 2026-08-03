use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use grafeo_common::types::{EdgeId, NodeId, Value};
use grafeo_core::graph::Direction;
use grafeo_engine::GrafeoDB;
use serde::{Deserialize, Serialize};

use crate::{
    GraphCommit, GraphDbError, GraphEntity, GraphIdempotencyKey, GraphNamespace, GraphProjectionId,
    GraphRelation, GraphWatermark,
};

pub(crate) const FORMAT_LABEL: &str = "__tracedecay_graph_db_format";
pub(crate) const FORMAT_VERSION_PROPERTY: &str = "__tracedecay_graph_db_version";
pub(crate) const SEQUENCE_PROPERTY: &str = "__tracedecay_graph_db_sequence";
pub(crate) const ENTITY_LABEL: &str = "__tracedecay_graph_db_entity";
pub(crate) const COMMIT_LABEL: &str = "__tracedecay_graph_db_commit";
pub(crate) const PUBLICATION_LABEL: &str = "__tracedecay_graph_db_publication";
pub(crate) const RELATION_TYPE: &str = "__tracedecay_graph_db_relation";
pub(crate) const PAYLOAD_PROPERTY: &str = "__tracedecay_graph_db_payload";

pub(crate) type StableKey = (String, String);
pub(crate) type EntityIndex = BTreeMap<StableKey, (NodeId, StoredEntity)>;
pub(crate) type RelationIndex = BTreeMap<StableKey, (EdgeId, StoredRelation)>;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct StoredEntity {
    pub(crate) namespace: GraphNamespace,
    pub(crate) projection: GraphProjectionId,
    pub(crate) entity: GraphEntity,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct StoredRelation {
    pub(crate) namespace: GraphNamespace,
    pub(crate) projection: GraphProjectionId,
    pub(crate) relation: GraphRelation,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct StoredCommit {
    pub(crate) namespace: GraphNamespace,
    pub(crate) projection: GraphProjectionId,
    pub(crate) commit: GraphCommit,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct StoredPublication {
    pub(crate) namespace: GraphNamespace,
    pub(crate) key: GraphIdempotencyKey,
    pub(crate) digest: String,
    pub(crate) commit: GraphCommit,
}

#[derive(Clone)]
pub(crate) struct StateCache {
    pub(crate) marker: NodeId,
    pub(crate) sequence: u64,
    pub(crate) entities: EntityIndex,
    pub(crate) relations: RelationIndex,
    entities_by_node: HashMap<NodeId, StableKey>,
    relations_by_edge: HashMap<EdgeId, StableKey>,
    relations_by_entity: HashMap<StableKey, BTreeSet<StableKey>>,
    watermarks: BTreeMap<StableKey, (u64, GraphWatermark)>,
    publications: BTreeMap<StableKey, StoredPublication>,
}

impl StateCache {
    pub(crate) fn load(database: &GrafeoDB) -> Result<Self, GraphDbError> {
        let store = database.graph_store();
        let markers = store.nodes_by_label(FORMAT_LABEL);
        if markers.len() != 1 {
            return Err(GraphDbError::Corrupt {
                message: "live store lost its exact format marker".to_owned(),
            });
        }
        let marker_node = store
            .get_node(markers[0])
            .ok_or_else(|| GraphDbError::Corrupt {
                message: "format marker is unreadable".to_owned(),
            })?;
        let sequence_i64 = marker_node
            .get_property(SEQUENCE_PROPERTY)
            .and_then(Value::as_int64)
            .ok_or_else(|| GraphDbError::Corrupt {
                message: "format marker has no valid commit sequence".to_owned(),
            })?;
        let sequence = u64::try_from(sequence_i64).map_err(|_| GraphDbError::Corrupt {
            message: "format marker has a negative commit sequence".to_owned(),
        })?;

        let mut entities = EntityIndex::new();
        let mut entities_by_node = HashMap::new();
        for node_id in store.nodes_by_label(ENTITY_LABEL) {
            let node = store
                .get_node(node_id)
                .ok_or_else(|| GraphDbError::Corrupt {
                    message: "entity node is unreadable".to_owned(),
                })?;
            let stored: StoredEntity =
                parse_payload(node.get_property(PAYLOAD_PROPERTY), "entity node payload")?;
            stored
                .entity
                .validate()
                .map_err(|error| persisted_validation_error("entity", error))?;
            let key = stable_key(&stored.namespace, stored.entity.identity.as_str());
            if entities.insert(key.clone(), (node_id, stored)).is_some()
                || entities_by_node.insert(node_id, key).is_some()
            {
                return Err(GraphDbError::Corrupt {
                    message: "duplicate entity identity or local node ID".to_owned(),
                });
            }
        }

        let mut relations = RelationIndex::new();
        let mut relations_by_edge = HashMap::new();
        let mut relations_by_entity: HashMap<StableKey, BTreeSet<StableKey>> = HashMap::new();
        let mut seen_edges = HashSet::new();
        for (node_id, _) in entities.values() {
            for (_, edge_id) in store.edges_from(*node_id, Direction::Outgoing) {
                if !seen_edges.insert(edge_id) {
                    continue;
                }
                let edge = store
                    .get_edge(edge_id)
                    .ok_or_else(|| GraphDbError::Corrupt {
                        message: "relation edge is unreadable".to_owned(),
                    })?;
                if edge.edge_type.as_str() != RELATION_TYPE {
                    continue;
                }
                let stored: StoredRelation =
                    parse_payload(edge.get_property(PAYLOAD_PROPERTY), "relation edge payload")?;
                stored
                    .relation
                    .validate()
                    .map_err(|error| persisted_validation_error("relation", error))?;
                let expected_source = entities
                    .get(&stable_key(
                        &stored.namespace,
                        stored.relation.from.as_str(),
                    ))
                    .map(|(node, _)| *node);
                let expected_target = entities
                    .get(&stable_key(&stored.namespace, stored.relation.to.as_str()))
                    .map(|(node, _)| *node);
                if expected_source != Some(edge.src) || expected_target != Some(edge.dst) {
                    return Err(GraphDbError::Corrupt {
                        message: "relation payload endpoints do not match its Grafeo edge"
                            .to_owned(),
                    });
                }
                let key = stable_key(&stored.namespace, stored.relation.identity.as_str());
                for endpoint in [&stored.relation.from, &stored.relation.to] {
                    relations_by_entity
                        .entry(stable_key(&stored.namespace, endpoint.as_str()))
                        .or_default()
                        .insert(key.clone());
                }
                if relations.insert(key.clone(), (edge_id, stored)).is_some()
                    || relations_by_edge.insert(edge_id, key).is_some()
                {
                    return Err(GraphDbError::Corrupt {
                        message: "duplicate relation identity or local edge ID".to_owned(),
                    });
                }
            }
        }

        let commits: Vec<StoredCommit> =
            load_labeled_payloads(database, COMMIT_LABEL, "commit payload")?;
        let mut watermarks = BTreeMap::new();
        for stored in commits {
            let key = stable_key(&stored.namespace, stored.projection.as_str());
            let replace = watermarks
                .get(&key)
                .is_none_or(|(sequence, _)| *sequence < stored.commit.sequence);
            if replace {
                watermarks.insert(key, (stored.commit.sequence, stored.commit.watermark));
            }
        }
        let stored_publications: Vec<StoredPublication> =
            load_labeled_payloads(database, PUBLICATION_LABEL, "publication payload")?;
        let mut publications = BTreeMap::new();
        for publication in stored_publications {
            let key = stable_key(&publication.namespace, publication.key.as_str());
            if publications.insert(key, publication).is_some() {
                return Err(GraphDbError::Corrupt {
                    message: "duplicate publication idempotency identity".to_owned(),
                });
            }
        }
        Ok(Self {
            marker: markers[0],
            sequence,
            entities,
            relations,
            entities_by_node,
            relations_by_edge,
            relations_by_entity,
            watermarks,
            publications,
        })
    }

    pub(crate) fn entity_by_node(&self, node: NodeId) -> Option<&StoredEntity> {
        self.entities_by_node
            .get(&node)
            .and_then(|key| self.entities.get(key))
            .map(|(_, entity)| entity)
    }

    pub(crate) fn relation_by_edge(&self, edge: EdgeId) -> Option<&StoredRelation> {
        self.relations_by_edge
            .get(&edge)
            .and_then(|key| self.relations.get(key))
            .map(|(_, relation)| relation)
    }

    pub(crate) fn insert_entity(&mut self, key: StableKey, node: NodeId, stored: StoredEntity) {
        if let Some((previous, _)) = self.entities.get(&key) {
            self.entities_by_node.remove(previous);
        }
        self.entities_by_node.insert(node, key.clone());
        self.entities.insert(key, (node, stored));
    }

    pub(crate) fn remove_entity(&mut self, key: &StableKey) -> Option<(NodeId, StoredEntity)> {
        let removed = self.entities.remove(key);
        if let Some((node, _)) = &removed {
            self.entities_by_node.remove(node);
        }
        removed
    }

    pub(crate) fn insert_relation(&mut self, key: StableKey, edge: EdgeId, stored: StoredRelation) {
        self.remove_relation(&key);
        for endpoint in [&stored.relation.from, &stored.relation.to] {
            self.relations_by_entity
                .entry(stable_key(&stored.namespace, endpoint.as_str()))
                .or_default()
                .insert(key.clone());
        }
        self.relations_by_edge.insert(edge, key.clone());
        self.relations.insert(key, (edge, stored));
    }

    pub(crate) fn remove_relation(&mut self, key: &StableKey) -> Option<(EdgeId, StoredRelation)> {
        let removed = self.relations.remove(key);
        if let Some((edge, stored)) = &removed {
            self.relations_by_edge.remove(edge);
            for endpoint in [&stored.relation.from, &stored.relation.to] {
                let endpoint_key = stable_key(&stored.namespace, endpoint.as_str());
                let empty =
                    self.relations_by_entity
                        .get_mut(&endpoint_key)
                        .is_some_and(|relations| {
                            relations.remove(key);
                            relations.is_empty()
                        });
                if empty {
                    self.relations_by_entity.remove(&endpoint_key);
                }
            }
        }
        removed
    }

    pub(crate) fn relations_for_entity(
        &self,
        entity: &StableKey,
    ) -> impl Iterator<Item = &StableKey> {
        self.relations_by_entity
            .get(entity)
            .into_iter()
            .flat_map(|relations| relations.iter())
    }

    pub(crate) fn latest_watermark(
        &self,
        namespace: &GraphNamespace,
        projection: &GraphProjectionId,
    ) -> Option<&GraphWatermark> {
        self.watermarks
            .get(&stable_key(namespace, projection.as_str()))
            .map(|(_, watermark)| watermark)
    }

    pub(crate) fn publication(
        &self,
        namespace: &GraphNamespace,
        key: &GraphIdempotencyKey,
    ) -> Option<&StoredPublication> {
        self.publications.get(&stable_key(namespace, key.as_str()))
    }

    pub(crate) fn record_commit(&mut self, stored: StoredCommit) {
        self.sequence = stored.commit.sequence;
        self.watermarks.insert(
            stable_key(&stored.namespace, stored.projection.as_str()),
            (stored.commit.sequence, stored.commit.watermark.clone()),
        );
    }

    pub(crate) fn record_publication(&mut self, stored: StoredPublication) {
        self.publications
            .insert(stable_key(&stored.namespace, stored.key.as_str()), stored);
    }
}

pub(crate) fn stable_key(namespace: &GraphNamespace, identity: &str) -> StableKey {
    (namespace.as_str().to_owned(), identity.to_owned())
}

pub(crate) fn serialize_payload<T: Serialize>(value: &T) -> Result<String, GraphDbError> {
    serde_json::to_string(value)
        .map_err(|error| GraphDbError::invalid(format!("payload serialization failed: {error}")))
}

fn load_labeled_payloads<T: for<'de> Deserialize<'de>>(
    database: &GrafeoDB,
    label: &str,
    description: &str,
) -> Result<Vec<T>, GraphDbError> {
    let store = database.graph_store();
    store
        .nodes_by_label(label)
        .into_iter()
        .map(|node_id| {
            let node = store
                .get_node(node_id)
                .ok_or_else(|| GraphDbError::Corrupt {
                    message: format!("{description} node is unreadable"),
                })?;
            parse_payload(node.get_property(PAYLOAD_PROPERTY), description)
        })
        .collect()
}

fn parse_payload<T: for<'de> Deserialize<'de>>(
    value: Option<&Value>,
    description: &str,
) -> Result<T, GraphDbError> {
    let json = value
        .and_then(Value::as_str)
        .ok_or_else(|| GraphDbError::Corrupt {
            message: format!("{description} is missing or not a string"),
        })?;
    serde_json::from_str(json).map_err(|error| GraphDbError::Corrupt {
        message: format!("invalid {description}: {error}"),
    })
}

fn persisted_validation_error(description: &str, error: GraphDbError) -> GraphDbError {
    GraphDbError::Corrupt {
        message: format!("invalid persisted {description}: {error}"),
    }
}
