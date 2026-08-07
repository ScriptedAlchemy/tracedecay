//! Authorized, payload-free cross-domain locator projection.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CrossReferenceLocatorV1, CrossReferenceRelationV1, CrossReferenceTargetV1, ManifestDigest,
    canonical_sha256,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphDbError, GraphEntity, GraphEntityId, GraphEntityRef, GraphGenerationId,
    GraphGenerationManifest, GraphGenerationRelation, GraphLabel, GraphNamespace, GraphProjectionId,
    GraphProjectionIdentity, GraphProjectorRevision, GraphProperty, GraphPropertyName,
    GraphRelationId, GraphRelationKind, GraphTraversalDirection, GraphWatermark, SourceGeneration,
    TraversalRequest, VerifiedGraphSnapshot,
};

const CROSS_REFERENCE_PROJECTION: &str = "cross-reference-locators";
const TARGET_LABEL: &str = "CrossReferenceTarget";
const TARGET_RECORD_PROPERTY: &str = "target-record";
const LOCATOR_RECORD_PROPERTY: &str = "locator-record";

pub const CROSS_REFERENCE_PROJECTOR_REVISION_V1: &str = "cross-reference-projector.v1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CrossReferenceProjectionV1 {
    pub scope_digest: ManifestDigest,
    pub source_watermark: ManifestDigest,
    pub locators: Vec<CrossReferenceLocatorV1>,
}

impl CrossReferenceProjectionV1 {
    pub fn validate(&self) -> Result<(), CrossReferenceGraphError> {
        self.scope_digest
            .validate()
            .and_then(|()| self.source_watermark.validate())
            .map_err(|error| CrossReferenceGraphError::Contract(error.to_string()))?;
        if self.locators.is_empty() {
            return Err(CrossReferenceGraphError::EmptyProjection);
        }
        let mut prior = None;
        for locator in &self.locators {
            locator
                .validate()
                .map_err(|error| CrossReferenceGraphError::Contract(error.to_string()))?;
            if locator.scope_digest() != &self.scope_digest {
                return Err(CrossReferenceGraphError::MixedScope);
            }
            if prior
                .as_ref()
                .is_some_and(|prior| prior >= locator.locator_digest())
            {
                return Err(CrossReferenceGraphError::NonCanonicalLocators);
            }
            prior = Some(locator.locator_digest().clone());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum CrossReferenceGraphError {
    #[error("cross-reference projection requires at least one locator")]
    EmptyProjection,
    #[error("cross-reference projection mixes authorization scopes")]
    MixedScope,
    #[error("cross-reference locators are duplicated or out of order")]
    NonCanonicalLocators,
    #[error("cross-reference traversal is not authorized for this projection")]
    Denied,
    #[error("cross-reference generation does not match")]
    GenerationMismatch,
    #[error("cross-reference operation was cancelled")]
    Cancelled,
    #[error("cross-reference traversal budget was exhausted")]
    BudgetExhausted,
    #[error("cross-reference contract violation: {0}")]
    Contract(String),
    #[error("cross-reference graph is unavailable: {0}")]
    Unavailable(String),
    #[error("cross-reference graph is corrupt: {0}")]
    Corrupt(String),
}

impl From<GraphDbError> for CrossReferenceGraphError {
    fn from(error: GraphDbError) -> Self {
        match error {
            GraphDbError::Cancelled => Self::Cancelled,
            GraphDbError::BudgetExhausted | GraphDbError::DeadlineExceeded => {
                Self::BudgetExhausted
            }
            GraphDbError::InvalidRequest { message } => Self::Contract(message),
            GraphDbError::Corrupt { message }
            | GraphDbError::ResetRequired { message }
            | GraphDbError::DurabilityUncertain { message }
            | GraphDbError::ProjectionMismatch { message, .. }
            | GraphDbError::GenerationMismatch { message, .. } => Self::Corrupt(message),
            GraphDbError::Conflict => {
                Self::Unavailable("cross-reference publication conflict".to_owned())
            }
            GraphDbError::Unavailable { message } => Self::Unavailable(message),
            GraphDbError::Closed => Self::Unavailable("graph store is closed".to_owned()),
        }
    }
}

pub fn cross_reference_projection_identity(
    namespace: GraphNamespace,
) -> Result<GraphProjectionIdentity, CrossReferenceGraphError> {
    Ok(GraphProjectionIdentity::new(
        namespace,
        GraphProjectionId::new(CROSS_REFERENCE_PROJECTION)?,
    ))
}

pub fn cross_reference_generation_id(
    projection: &CrossReferenceProjectionV1,
    projector_revision: &GraphProjectorRevision,
) -> Result<GraphGenerationId, CrossReferenceGraphError> {
    projection.validate()?;
    let digest = canonical_sha256(&(
        "tracedecay.cross-reference-generation.v1",
        projection,
        projector_revision,
    ))
    .map_err(|error| CrossReferenceGraphError::Contract(error.to_string()))?;
    GraphGenerationId::new(format!("cross-reference:{}", digest.as_str())).map_err(Into::into)
}

pub fn build_cross_reference_manifest_checked(
    identity: GraphProjectionIdentity,
    projection: &CrossReferenceProjectionV1,
    projector_revision: &GraphProjectorRevision,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<GraphGenerationManifest, CrossReferenceGraphError> {
    check()?;
    projection.validate()?;
    if identity.projection.as_str() != CROSS_REFERENCE_PROJECTION {
        return Err(CrossReferenceGraphError::Contract(
            "cross-reference identity uses a foreign projector".to_owned(),
        ));
    }
    let targets = projection
        .locators
        .iter()
        .flat_map(|locator| [locator.source().clone(), locator.target().clone()])
        .collect::<BTreeSet<_>>();
    let entities = targets
        .iter()
        .map(target_entity)
        .collect::<Result<Vec<_>, _>>()?;
    let relations = projection
        .locators
        .iter()
        .map(|locator| locator_relation(&identity, locator))
        .collect::<Result<Vec<_>, _>>()?;
    GraphGenerationManifest::new_checked(
        identity,
        cross_reference_generation_id(projection, projector_revision)?,
        SourceGeneration::new(projection.source_watermark.as_str())?,
        GraphWatermark::new(projection.source_watermark.as_str())?,
        Vec::new(),
        entities,
        relations,
        check,
    )
    .map_err(Into::into)
}

#[derive(Clone)]
pub struct CrossReferenceStore {
    snapshot: Arc<VerifiedGraphSnapshot>,
    projection: GraphProjectionIdentity,
    generation: GraphGenerationId,
    scope_digest: ManifestDigest,
}

impl fmt::Debug for CrossReferenceStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CrossReferenceStore")
            .field("projection", &self.projection)
            .field("generation", &self.generation)
            .field("scope_digest", &self.scope_digest)
            .finish_non_exhaustive()
    }
}

impl CrossReferenceStore {
    pub fn from_verified_snapshot(
        snapshot: VerifiedGraphSnapshot,
        projection: &CrossReferenceProjectionV1,
    ) -> Result<Self, CrossReferenceGraphError> {
        let revision =
            GraphProjectorRevision::try_from(CROSS_REFERENCE_PROJECTOR_REVISION_V1.to_owned())?;
        let generation = cross_reference_generation_id(projection, &revision)?;
        if snapshot.generation() != &generation {
            return Err(CrossReferenceGraphError::GenerationMismatch);
        }
        Ok(Self {
            projection: snapshot.projection().clone(),
            generation,
            scope_digest: projection.scope_digest.clone(),
            snapshot: Arc::new(snapshot),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn related(
        &self,
        start: &CrossReferenceTargetV1,
        authorized_scope: &ManifestDigest,
        relations: &BTreeSet<CrossReferenceRelationV1>,
        direction: GraphTraversalDirection,
        max_depth: usize,
        max_results: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<CrossReferenceTargetV1>, CrossReferenceGraphError> {
        if authorized_scope != &self.scope_digest {
            return Err(CrossReferenceGraphError::Denied);
        }
        if relations.is_empty() || max_depth == 0 || max_results == 0 {
            return Err(CrossReferenceGraphError::Contract(
                "cross-reference traversal requires relation and positive bounds".to_owned(),
            ));
        }
        start
            .validate()
            .map_err(|error| CrossReferenceGraphError::Contract(error.to_string()))?;
        let result = self.snapshot.traverse(TraversalRequest {
            namespace: self.projection.namespace.clone(),
            start: target_entity_id(start)?,
            relation_kinds: relations
                .iter()
                .map(|relation| GraphRelationKind::new(relation.graph_kind()))
                .collect::<Result<BTreeSet<_>, _>>()?,
            direction,
            max_depth,
            max_visits: max_results.saturating_add(1),
            max_results,
            cancellation: Arc::clone(&cancellation),
        })?;
        result
            .visits
            .into_iter()
            .map(|visit| {
                target_from_ref(&self.snapshot, &visit.entity, Arc::clone(&cancellation))
            })
            .filter(|result| result.as_ref() != Ok(start))
            .collect()
    }
}

fn target_entity(
    target: &CrossReferenceTargetV1,
) -> Result<GraphEntity, CrossReferenceGraphError> {
    GraphEntity::new(
        target_entity_id(target)?,
        BTreeSet::from([
            GraphLabel::new(TARGET_LABEL)?,
            GraphLabel::new(format!("CrossReference{}", target.domain()))?,
        ]),
        BTreeMap::from([(
            GraphPropertyName::new(TARGET_RECORD_PROPERTY)?,
            GraphProperty::Bytes(serialize(target)?),
        )]),
    )
    .map_err(Into::into)
}

fn locator_relation(
    projection: &GraphProjectionIdentity,
    locator: &CrossReferenceLocatorV1,
) -> Result<GraphGenerationRelation, CrossReferenceGraphError> {
    GraphGenerationRelation::new(
        GraphRelationId::new(format!(
            "locator:{}",
            locator
                .locator_digest()
                .as_str()
                .trim_start_matches("sha256:")
        ))?,
        GraphEntityRef::new(projection.clone(), target_entity_id(locator.source())?),
        GraphEntityRef::new(projection.clone(), target_entity_id(locator.target())?),
        GraphRelationKind::new(locator.relation().graph_kind())?,
        BTreeMap::from([(
            GraphPropertyName::new(LOCATOR_RECORD_PROPERTY)?,
            GraphProperty::Bytes(serialize(locator)?),
        )]),
    )
    .map_err(Into::into)
}

fn target_from_ref(
    snapshot: &VerifiedGraphSnapshot,
    reference: &GraphEntityRef,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<CrossReferenceTargetV1, CrossReferenceGraphError> {
    let entity = snapshot.entity(reference, cancellation)?.ok_or_else(|| {
        CrossReferenceGraphError::Corrupt(
            "cross-reference traversal reached a missing target".to_owned(),
        )
    })?;
    let property = entity
        .properties
        .get(&GraphPropertyName::new(TARGET_RECORD_PROPERTY)?)
        .ok_or_else(|| {
            CrossReferenceGraphError::Corrupt(
                "cross-reference target record is missing".to_owned(),
            )
        })?;
    let GraphProperty::Bytes(bytes) = property else {
        return Err(CrossReferenceGraphError::Corrupt(
            "cross-reference target record has the wrong type".to_owned(),
        ));
    };
    let target: CrossReferenceTargetV1 = serde_json::from_slice(bytes)
        .map_err(|error| CrossReferenceGraphError::Corrupt(error.to_string()))?;
    target
        .validate()
        .map_err(|error| CrossReferenceGraphError::Corrupt(error.to_string()))?;
    Ok(target)
}

fn target_entity_id(
    target: &CrossReferenceTargetV1,
) -> Result<GraphEntityId, CrossReferenceGraphError> {
    let digest = canonical_sha256(&("tracedecay.cross-reference-target.v1", target))
        .map_err(|error| CrossReferenceGraphError::Contract(error.to_string()))?;
    GraphEntityId::new(format!(
        "target:{}",
        digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(Into::into)
}

fn serialize(value: &impl Serialize) -> Result<Vec<u8>, CrossReferenceGraphError> {
    serde_json::to_vec(value)
        .map_err(|error| CrossReferenceGraphError::Contract(error.to_string()))
}
