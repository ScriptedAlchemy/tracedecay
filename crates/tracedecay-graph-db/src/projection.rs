use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{GraphDbError, VectorMetric};

const MAX_OPAQUE_ID_BYTES: usize = 1024;
const RESERVED_PREFIX: &str = "__tracedecay_graph_db_";

pub trait GraphCancellation: Send + Sync {
    fn is_cancelled(&self) -> bool;
}

#[derive(Debug)]
pub struct NeverCancelled;

impl GraphCancellation for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

macro_rules! opaque_id {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, GraphDbError> {
                let value = value.into();
                validate_opaque(stringify!($name), &value)?;
                Ok(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

opaque_id!(GraphNamespace);
opaque_id!(GraphProjectionId);
opaque_id!(GraphEntityId);
opaque_id!(GraphRelationId);
opaque_id!(GraphLabel);
opaque_id!(GraphPropertyName);
opaque_id!(GraphRelationKind);
opaque_id!(GraphIdempotencyKey);
opaque_id!(SourceGeneration);
opaque_id!(GraphWatermark);

fn validate_opaque(kind: &str, value: &str) -> Result<(), GraphDbError> {
    if value.is_empty() {
        return Err(GraphDbError::invalid(format!("{kind} must not be empty")));
    }
    if value.len() > MAX_OPAQUE_ID_BYTES {
        return Err(GraphDbError::invalid(format!(
            "{kind} exceeds {MAX_OPAQUE_ID_BYTES} bytes"
        )));
    }
    if value.starts_with(RESERVED_PREFIX) {
        return Err(GraphDbError::invalid(format!(
            "{kind} uses the reserved graph database prefix"
        )));
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct GraphVector {
    pub values: Vec<f32>,
    pub dimension: usize,
    pub metric: VectorMetric,
}

impl GraphVector {
    pub fn new(
        values: Vec<f32>,
        dimension: usize,
        metric: VectorMetric,
    ) -> Result<Self, GraphDbError> {
        let vector = Self {
            values,
            dimension,
            metric,
        };
        vector.validate()?;
        Ok(vector)
    }

    pub(crate) fn validate(&self) -> Result<(), GraphDbError> {
        if self.dimension == 0 {
            return Err(GraphDbError::invalid(
                "vector dimension must be greater than zero",
            ));
        }
        if self.values.len() != self.dimension {
            return Err(GraphDbError::invalid(format!(
                "vector has {} values but declares dimension {}",
                self.values.len(),
                self.dimension
            )));
        }
        if self.values.iter().any(|value| !value.is_finite()) {
            return Err(GraphDbError::invalid("vector values must all be finite"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value")]
pub enum GraphProperty {
    Bool(bool),
    I64(i64),
    F64(f64),
    String(String),
    Bytes(Vec<u8>),
    Vector(GraphVector),
}

impl GraphProperty {
    fn validate(&self) -> Result<(), GraphDbError> {
        match self {
            Self::F64(value) if !value.is_finite() => {
                Err(GraphDbError::invalid("floating properties must be finite"))
            }
            Self::Vector(vector) => vector.validate(),
            Self::String(value) if value.len() > MAX_OPAQUE_ID_BYTES * 1024 => {
                Err(GraphDbError::invalid("string property is too large"))
            }
            Self::Bytes(value) if value.len() > MAX_OPAQUE_ID_BYTES * 1024 => {
                Err(GraphDbError::invalid("byte property is too large"))
            }
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct GraphEntity {
    pub identity: GraphEntityId,
    pub labels: BTreeSet<GraphLabel>,
    pub properties: BTreeMap<GraphPropertyName, GraphProperty>,
}

impl GraphEntity {
    pub fn new(
        identity: GraphEntityId,
        labels: BTreeSet<GraphLabel>,
        properties: BTreeMap<GraphPropertyName, GraphProperty>,
    ) -> Result<Self, GraphDbError> {
        let entity = Self {
            identity,
            labels,
            properties,
        };
        entity.validate()?;
        Ok(entity)
    }

    pub(crate) fn validate(&self) -> Result<(), GraphDbError> {
        for property in self.properties.values() {
            property.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct GraphRelation {
    pub identity: GraphRelationId,
    pub from: GraphEntityId,
    pub to: GraphEntityId,
    pub kind: GraphRelationKind,
    pub properties: BTreeMap<GraphPropertyName, GraphProperty>,
}

impl GraphRelation {
    pub fn new(
        identity: GraphRelationId,
        from: GraphEntityId,
        to: GraphEntityId,
        kind: GraphRelationKind,
        properties: BTreeMap<GraphPropertyName, GraphProperty>,
    ) -> Result<Self, GraphDbError> {
        let relation = Self {
            identity,
            from,
            to,
            kind,
            properties,
        };
        relation.validate()?;
        Ok(relation)
    }

    pub(crate) fn validate(&self) -> Result<(), GraphDbError> {
        for property in self.properties.values() {
            property.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum GraphMutation {
    UpsertEntity(GraphEntity),
    DeleteEntity(GraphEntityId),
    UpsertRelation(GraphRelation),
    DeleteRelation(GraphRelationId),
}

impl GraphMutation {
    pub(crate) fn sort_key(&self) -> (u8, &str) {
        match self {
            Self::DeleteRelation(identity) => (0, identity.as_str()),
            Self::DeleteEntity(identity) => (1, identity.as_str()),
            Self::UpsertEntity(entity) => (2, entity.identity.as_str()),
            Self::UpsertRelation(relation) => (3, relation.identity.as_str()),
        }
    }

    fn validate(&self) -> Result<(), GraphDbError> {
        match self {
            Self::UpsertEntity(entity) => entity.validate(),
            Self::UpsertRelation(relation) => relation.validate(),
            Self::DeleteEntity(_) | Self::DeleteRelation(_) => Ok(()),
        }
    }
}

#[derive(Clone)]
pub struct GraphWriteBatch {
    pub namespace: GraphNamespace,
    pub projection: GraphProjectionId,
    pub source_generation: SourceGeneration,
    pub next_watermark: GraphWatermark,
    pub mutations: Vec<GraphMutation>,
    pub cancellation: Arc<dyn GraphCancellation>,
}

impl fmt::Debug for GraphWriteBatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GraphWriteBatch")
            .field("namespace", &self.namespace)
            .field("projection", &self.projection)
            .field("source_generation", &self.source_generation)
            .field("next_watermark", &self.next_watermark)
            .field("mutations", &self.mutations)
            .finish_non_exhaustive()
    }
}

impl GraphWriteBatch {
    pub fn new(
        namespace: GraphNamespace,
        projection: GraphProjectionId,
        source_generation: SourceGeneration,
        next_watermark: GraphWatermark,
        mut mutations: Vec<GraphMutation>,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Self, GraphDbError> {
        for mutation in &mutations {
            mutation.validate()?;
        }
        mutations.sort_by(|left, right| left.sort_key().cmp(&right.sort_key()));
        Ok(Self {
            namespace,
            projection,
            source_generation,
            next_watermark,
            mutations,
            cancellation,
        })
    }

    pub(crate) fn validate_and_digest(&mut self) -> Result<String, GraphDbError> {
        if self.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        for mutation in &self.mutations {
            mutation.validate()?;
        }
        self.mutations
            .sort_by(|left, right| left.sort_key().cmp(&right.sort_key()));
        let canonical = serde_json::to_vec(&(
            &self.namespace,
            &self.projection,
            &self.source_generation,
            &self.next_watermark,
            &self.mutations,
        ))
        .map_err(|error| {
            GraphDbError::invalid(format!("failed to canonicalize graph batch: {error}"))
        })?;
        Ok(hex::encode(Sha256::digest(canonical)))
    }
}

#[derive(Clone)]
pub struct ProjectionReplacement {
    pub namespace: GraphNamespace,
    pub projection: GraphProjectionId,
    pub source_generation: SourceGeneration,
    pub next_watermark: GraphWatermark,
    pub entities: Vec<GraphEntity>,
    pub relations: Vec<GraphRelation>,
    pub cancellation: Arc<dyn GraphCancellation>,
}

impl fmt::Debug for ProjectionReplacement {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectionReplacement")
            .field("namespace", &self.namespace)
            .field("projection", &self.projection)
            .field("source_generation", &self.source_generation)
            .field("next_watermark", &self.next_watermark)
            .field("entities", &self.entities)
            .field("relations", &self.relations)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GraphCommit {
    pub sequence: u64,
    pub source_generation: SourceGeneration,
    pub watermark: GraphWatermark,
    pub digest: String,
}
