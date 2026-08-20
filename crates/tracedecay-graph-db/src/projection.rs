use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{self, Write};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::limits::{
    MAX_GRAPH_BATCH_CANONICAL_BYTES, MAX_GRAPH_ENTITY_LABEL_BYTES, MAX_GRAPH_ENTITY_LABELS,
    MAX_GRAPH_IDENTIFIER_BYTES, MAX_GRAPH_PROPERTIES, MAX_GRAPH_PROPERTY_AGGREGATE_BYTES,
    MAX_GRAPH_PROPERTY_VALUE_BYTES, MAX_GRAPH_VECTOR_DIMENSION,
};
use crate::{GraphBudgetKind, GraphDbError, VectorMetric};

const RESERVED_PREFIX: &str = "__tracedecay_graph_db_";
const CHECKED_DIGEST_INTERVAL_BYTES: u64 = 64 * 1024;

pub trait GraphCancellation: Send + Sync {
    fn is_cancelled(&self) -> bool;
}

/// Typed no-cancellation authority for graph work that must run to
/// completion once durably begun (e.g. settlement after a committed
/// publication) or bounded reads that carry no caller cancellation.
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
opaque_id!(GraphGenerationId);

fn validate_opaque(kind: &str, value: &str) -> Result<(), GraphDbError> {
    if value.is_empty() {
        return Err(GraphDbError::invalid(format!("{kind} must not be empty")));
    }
    if value.len() > MAX_GRAPH_IDENTIFIER_BYTES {
        return Err(GraphDbError::invalid(format!(
            "{kind} exceeds {MAX_GRAPH_IDENTIFIER_BYTES} bytes"
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
        if self.dimension > MAX_GRAPH_VECTOR_DIMENSION {
            return Err(GraphDbError::budget_exhausted_count(
                GraphBudgetKind::Capacity,
                MAX_GRAPH_VECTOR_DIMENSION,
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
            Self::String(value) if value.len() > MAX_GRAPH_PROPERTY_VALUE_BYTES => {
                Err(GraphDbError::budget_exhausted_count(
                    GraphBudgetKind::Capacity,
                    MAX_GRAPH_PROPERTY_VALUE_BYTES,
                ))
            }
            Self::Bytes(value) if value.len() > MAX_GRAPH_PROPERTY_VALUE_BYTES => {
                Err(GraphDbError::budget_exhausted_count(
                    GraphBudgetKind::Capacity,
                    MAX_GRAPH_PROPERTY_VALUE_BYTES,
                ))
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
        validate_labels(&self.labels)?;
        validate_properties(&self.properties)
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
        validate_properties(&self.properties)
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
        normalize_mutations(&mut mutations)?;
        Ok(Self {
            namespace,
            projection,
            source_generation,
            next_watermark,
            mutations,
            cancellation,
        })
    }

    pub(crate) fn new_canonical_checked(
        namespace: GraphNamespace,
        projection: GraphProjectionId,
        source_generation: SourceGeneration,
        next_watermark: GraphWatermark,
        mutations: Vec<GraphMutation>,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<Self, GraphDbError> {
        let mut previous = None;
        for mutation in &mutations {
            check()?;
            mutation.validate()?;
            let key = mutation.sort_key();
            if previous.as_ref().is_some_and(|prior| prior > &key) {
                return Err(GraphDbError::invalid(
                    "canonical graph mutations are not ordered",
                ));
            }
            previous = Some(key);
        }
        check()?;
        Ok(Self {
            namespace,
            projection,
            source_generation,
            next_watermark,
            mutations,
            cancellation: Arc::new(NeverCancelled),
        })
    }

    pub(crate) fn validate_and_digest(&mut self) -> Result<String, GraphDbError> {
        self.validate_and_digest_with_limit(MAX_GRAPH_BATCH_CANONICAL_BYTES)
    }

    pub(crate) fn validate_and_digest_with_limit(
        &mut self,
        max_bytes: usize,
    ) -> Result<String, GraphDbError> {
        if self.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        normalize_mutations(&mut self.mutations)?;
        self.canonical_digest_checked_with_limit(max_bytes, &|| {
            if self.cancellation.is_cancelled() {
                Err(GraphDbError::Cancelled)
            } else {
                Ok(())
            }
        })
    }

    pub(crate) fn canonical_digest_checked(
        &self,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<String, GraphDbError> {
        self.canonical_digest_checked_with_limit(MAX_GRAPH_BATCH_CANONICAL_BYTES, check)
    }

    pub(crate) fn canonical_digest_checked_with_limit(
        &self,
        max_bytes: usize,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<String, GraphDbError> {
        #[cfg(test)]
        BATCH_CANONICALIZATIONS.with(|count| count.set(count.get() + 1));
        let mut digest = Sha256::new();
        let mut writer = CheckedProjectionDigestWriter {
            digest: &mut digest,
            total_bytes: 0,
            max_bytes,
            bytes_since_check: 0,
            check,
            failure: None,
        };
        let encoded = serde_json::to_writer(
            &mut writer,
            &(
                &self.namespace,
                &self.projection,
                &self.source_generation,
                &self.next_watermark,
                &self.mutations,
            ),
        );
        writer.finish()?;
        encoded.map_err(|error| {
            GraphDbError::invalid(format!("failed to canonicalize graph batch: {error}"))
        })?;
        Ok(hex::encode(digest.finalize()))
    }
}

#[cfg(test)]
thread_local! {
    static BATCH_CANONICALIZATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_batch_canonicalizations() {
    BATCH_CANONICALIZATIONS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn batch_canonicalizations() -> usize {
    BATCH_CANONICALIZATIONS.with(std::cell::Cell::get)
}

struct CheckedProjectionDigestWriter<'a> {
    digest: &'a mut Sha256,
    total_bytes: usize,
    max_bytes: usize,
    bytes_since_check: u64,
    check: &'a dyn Fn() -> Result<(), GraphDbError>,
    failure: Option<GraphDbError>,
}

impl CheckedProjectionDigestWriter<'_> {
    fn finish(mut self) -> Result<(), GraphDbError> {
        if let Some(error) = self.failure.take() {
            return Err(error);
        }
        (self.check)()
    }
}

impl Write for CheckedProjectionDigestWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(total_bytes) = self.total_bytes.checked_add(bytes.len()) else {
            self.failure = Some(GraphDbError::budget_exhausted(
                GraphBudgetKind::Write,
                u64::MAX,
            ));
            return Err(io::Error::other("graph batch digest input is too large"));
        };
        self.total_bytes = total_bytes;
        if self.total_bytes > self.max_bytes {
            self.failure = Some(GraphDbError::budget_exhausted_count(
                GraphBudgetKind::Write,
                self.max_bytes,
            ));
            return Err(io::Error::other(
                "graph batch canonical payload exceeds its product bound",
            ));
        }
        let length = u64::try_from(bytes.len())
            .map_err(|_| io::Error::other("graph batch digest input is too large"))?;
        self.bytes_since_check = self
            .bytes_since_check
            .checked_add(length)
            .ok_or_else(|| io::Error::other("graph batch digest check interval overflow"))?;
        if self.bytes_since_check >= CHECKED_DIGEST_INTERVAL_BYTES {
            self.bytes_since_check = 0;
            if let Err(error) = (self.check)() {
                self.failure = Some(error);
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "graph batch digest interrupted",
                ));
            }
        }
        self.digest.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn normalize_mutations(mutations: &mut Vec<GraphMutation>) -> Result<(), GraphDbError> {
    for mutation in mutations.iter() {
        mutation.validate()?;
    }
    let upserted_entities: BTreeSet<_> = mutations
        .iter()
        .filter_map(|mutation| match mutation {
            GraphMutation::UpsertEntity(entity) => Some(entity.identity.clone()),
            _ => None,
        })
        .collect();
    mutations.retain(|mutation| {
        !matches!(
            mutation,
            GraphMutation::DeleteEntity(identity) if upserted_entities.contains(identity)
        )
    });
    mutations.sort_by(|left, right| left.sort_key().cmp(&right.sort_key()));
    Ok(())
}

fn validate_labels(labels: &BTreeSet<GraphLabel>) -> Result<(), GraphDbError> {
    if labels.len() > MAX_GRAPH_ENTITY_LABELS {
        return Err(GraphDbError::budget_exhausted_count(
            GraphBudgetKind::Capacity,
            MAX_GRAPH_ENTITY_LABELS,
        ));
    }
    let mut bytes = 0usize;
    for label in labels {
        bytes = bytes.checked_add(label.as_str().len()).ok_or_else(|| {
            GraphDbError::budget_exhausted_count(
                GraphBudgetKind::Capacity,
                MAX_GRAPH_ENTITY_LABEL_BYTES,
            )
        })?;
        if bytes > MAX_GRAPH_ENTITY_LABEL_BYTES {
            return Err(GraphDbError::budget_exhausted_count(
                GraphBudgetKind::Capacity,
                MAX_GRAPH_ENTITY_LABEL_BYTES,
            ));
        }
    }
    Ok(())
}

fn validate_properties(
    properties: &BTreeMap<GraphPropertyName, GraphProperty>,
) -> Result<(), GraphDbError> {
    if properties.len() > MAX_GRAPH_PROPERTIES {
        return Err(GraphDbError::budget_exhausted_count(
            GraphBudgetKind::Capacity,
            MAX_GRAPH_PROPERTIES,
        ));
    }
    let mut bytes = 0usize;
    for (name, property) in properties {
        property.validate()?;
        bytes = bytes
            .checked_add(name.as_str().len())
            .and_then(|total| total.checked_add(property_payload_bytes(property)?))
            .ok_or_else(|| {
                GraphDbError::budget_exhausted_count(
                    GraphBudgetKind::Capacity,
                    MAX_GRAPH_PROPERTY_AGGREGATE_BYTES,
                )
            })?;
        if bytes > MAX_GRAPH_PROPERTY_AGGREGATE_BYTES {
            return Err(GraphDbError::budget_exhausted_count(
                GraphBudgetKind::Capacity,
                MAX_GRAPH_PROPERTY_AGGREGATE_BYTES,
            ));
        }
    }
    Ok(())
}

fn property_payload_bytes(property: &GraphProperty) -> Option<usize> {
    match property {
        GraphProperty::Bool(_) => Some(std::mem::size_of::<bool>()),
        GraphProperty::I64(_) => Some(std::mem::size_of::<i64>()),
        GraphProperty::F64(_) => Some(std::mem::size_of::<f64>()),
        GraphProperty::String(value) => Some(value.len()),
        GraphProperty::Bytes(value) => Some(value.len()),
        GraphProperty::Vector(vector) => {
            vector.values.len().checked_mul(std::mem::size_of::<f32>())
        }
    }
}

#[derive(Clone)]
/// A complete replacement input for one disposable derived projection.
///
/// Callers retain canonical source data elsewhere; this value only describes
/// the graph-index materialization to rebuild.
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
/// Metadata reported by one derived-index mutation.
///
/// It identifies the native graph state observed by this handle. It is not a
/// durable source-of-truth receipt and must not authorize loss of the
/// canonical projection input.
pub struct GraphCommit {
    pub sequence: u64,
    pub source_generation: SourceGeneration,
    pub watermark: GraphWatermark,
    pub digest: String,
    #[serde(skip)]
    pub(crate) generation_dependency_digest:
        Option<tracedecay_store::runtime::GraphDependencyGenerationClosureDigestV1>,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        GraphEntityId, GraphMutation, GraphNamespace, GraphProjectionId, GraphVector,
        GraphWatermark, GraphWriteBatch, NeverCancelled, SourceGeneration,
    };
    use crate::{
        GraphBudgetKind, GraphDbError, MAX_GRAPH_VECTOR_DIMENSION,
        MAX_VERIFIED_GENERATION_BATCH_MUTATIONS, VectorMetric,
    };

    #[test]
    fn full_generation_batch_accepts_more_than_incremental_stage_limit() {
        let mutations = (0..=MAX_VERIFIED_GENERATION_BATCH_MUTATIONS)
            .map(|index| {
                GraphMutation::DeleteEntity(
                    GraphEntityId::new(format!("entity-{index:05}")).unwrap(),
                )
            })
            .collect();
        let batch = GraphWriteBatch::new(
            GraphNamespace::new("namespace.full-generation").unwrap(),
            GraphProjectionId::new("projection.full-generation").unwrap(),
            SourceGeneration::new("source.full-generation").unwrap(),
            GraphWatermark::new("watermark.full-generation").unwrap(),
            mutations,
            Arc::new(NeverCancelled),
        )
        .unwrap();

        assert_eq!(
            batch.mutations.len(),
            MAX_VERIFIED_GENERATION_BATCH_MUTATIONS + 1
        );
    }

    #[test]
    fn vector_dimension_and_streamed_canonical_bytes_are_bounded() {
        assert_eq!(
            GraphVector::new(
                vec![0.0; MAX_GRAPH_VECTOR_DIMENSION + 1],
                MAX_GRAPH_VECTOR_DIMENSION + 1,
                VectorMetric::Cosine,
            ),
            Err(GraphDbError::budget_exhausted_count(
                GraphBudgetKind::Capacity,
                MAX_GRAPH_VECTOR_DIMENSION,
            ))
        );
        let batch = GraphWriteBatch::new(
            GraphNamespace::new("namespace.bounded").unwrap(),
            GraphProjectionId::new("projection.bounded").unwrap(),
            SourceGeneration::new("source.bounded").unwrap(),
            GraphWatermark::new("watermark.bounded").unwrap(),
            vec![],
            Arc::new(NeverCancelled),
        )
        .unwrap();
        assert_eq!(
            batch.canonical_digest_checked_with_limit(8, &|| Ok(())),
            Err(GraphDbError::budget_exhausted(GraphBudgetKind::Write, 8))
        );
    }
}
