//! Immutable event envelopes for canonical Work product graph changes.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::{
    ActorId, BrainId, CatalogGenerationId, ConfigurationRevisionId, ManifestDigest,
    PolicyRevisionId, ProjectId, RepositoryId, RetrievalAnchorId, SourceStoreId, UserProfileId,
    UtcMicros, WorkCommandId, WorkGraphChangeV1, WorkGraphVersionV1,
};

pub const MAX_WORK_PRODUCT_EVENT_RELATION_SCOPES: usize = 256;
pub const MAX_WORK_PRODUCT_EVENT_EVIDENCE: usize = 1_024;
pub const MAX_WORK_PRODUCT_EVENT_SOURCE_WATERMARKS: usize = 256;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkProductEventContractError {
    #[error("Work product event identity is not canonical")]
    InvalidEventIdentity,
    #[error("Work product event sequence must be non-zero")]
    InvalidSequence,
    #[error("Work product event graph versions are not one canonical progression")]
    InvalidVersionProgression,
    #[error("Work product event payload is not canonical")]
    InvalidPayload,
    #[error("Work product event cannot cause itself")]
    SelfCausation,
    #[error("Work product event authorized relation scopes exceed their bound")]
    TooManyRelationScopes,
    #[error("Work product event repeats an authorized relation scope")]
    DuplicateRelationScope,
    #[error("Work product event evidence exceeds its bound")]
    TooMuchEvidence,
    #[error("Work product event repeats exact evidence")]
    DuplicateEvidence,
    #[error("Work product event evidence source is absent from its source watermark")]
    MissingEvidenceSourceWatermark,
    #[error("Work product event source watermark exceeds its bound")]
    TooManySourceWatermarks,
    #[error("Work product event source watermark sequence must be non-zero")]
    InvalidSourceWatermarkSequence,
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct WorkProductEventId(String);

impl WorkProductEventId {
    pub fn new(value: impl Into<String>) -> Result<Self, WorkProductEventContractError> {
        let value = value.into();
        if !crate::canonical_text::is_canonical_text_within(
            &value,
            crate::canonical_text::CANONICAL_TEXT_MAX_BYTES,
        ) {
            return Err(WorkProductEventContractError::InvalidEventIdentity);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for WorkProductEventId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<String> for WorkProductEventId {
    type Error = WorkProductEventContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl fmt::Display for WorkProductEventId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct WorkProductEventSequenceV1(u64);

impl WorkProductEventSequenceV1 {
    pub fn new(value: u64) -> Result<Self, WorkProductEventContractError> {
        if value == 0 {
            return Err(WorkProductEventContractError::InvalidSequence);
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for WorkProductEventSequenceV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(u64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProductProfileScopeV1 {
    pub brain_id: BrainId,
    pub profile_id: UserProfileId,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkProductAuthorizedRelationScopeV1 {
    Project {
        project_id: ProjectId,
    },
    Repository {
        project_id: ProjectId,
        repository_id: RepositoryId,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct WorkProductEventEvidenceV1 {
    pub source_store_id: SourceStoreId,
    pub anchor_id: RetrievalAnchorId,
    pub evidence_digest: ManifestDigest,
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(transparent)]
pub struct WorkProductSourceWatermarkV1 {
    components: BTreeMap<SourceStoreId, u64>,
}

impl WorkProductSourceWatermarkV1 {
    pub fn new(
        components: BTreeMap<SourceStoreId, u64>,
    ) -> Result<Self, WorkProductEventContractError> {
        if components.len() > MAX_WORK_PRODUCT_EVENT_SOURCE_WATERMARKS {
            return Err(WorkProductEventContractError::TooManySourceWatermarks);
        }
        if components.values().any(|sequence| *sequence == 0) {
            return Err(WorkProductEventContractError::InvalidSourceWatermarkSequence);
        }
        Ok(Self { components })
    }

    pub fn components(&self) -> &BTreeMap<SourceStoreId, u64> {
        &self.components
    }
}

impl<'de> Deserialize<'de> for WorkProductSourceWatermarkV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(BTreeMap::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkProductEventPayloadV1 {
    Created { graph: crate::WorkProductGraphV1 },
    Changed { change: WorkGraphChangeV1 },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProductEventInputV1 {
    pub event_id: WorkProductEventId,
    pub sequence: WorkProductEventSequenceV1,
    pub actor_id: ActorId,
    pub owner_scope: WorkProductProfileScopeV1,
    pub authorized_relation_scopes: Vec<WorkProductAuthorizedRelationScopeV1>,
    pub expected_graph_version: Option<WorkGraphVersionV1>,
    pub result_graph_version: WorkGraphVersionV1,
    pub command_id: WorkCommandId,
    pub canonical_input_digest: ManifestDigest,
    pub causation_event_id: Option<WorkProductEventId>,
    pub evidence: Vec<WorkProductEventEvidenceV1>,
    pub source_watermark: WorkProductSourceWatermarkV1,
    pub occurred_at: UtcMicros,
    #[schemars(with = "String")]
    pub policy_revision_id: PolicyRevisionId,
    pub configuration_revision_id: ConfigurationRevisionId,
    pub catalog_generation_id: CatalogGenerationId,
    pub payload: WorkProductEventPayloadV1,
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProductEventV1 {
    event_id: WorkProductEventId,
    sequence: WorkProductEventSequenceV1,
    actor_id: ActorId,
    owner_scope: WorkProductProfileScopeV1,
    authorized_relation_scopes: Vec<WorkProductAuthorizedRelationScopeV1>,
    expected_graph_version: Option<WorkGraphVersionV1>,
    result_graph_version: WorkGraphVersionV1,
    command_id: WorkCommandId,
    canonical_input_digest: ManifestDigest,
    causation_event_id: Option<WorkProductEventId>,
    evidence: Vec<WorkProductEventEvidenceV1>,
    source_watermark: WorkProductSourceWatermarkV1,
    occurred_at: UtcMicros,
    #[schemars(with = "String")]
    policy_revision_id: PolicyRevisionId,
    configuration_revision_id: ConfigurationRevisionId,
    catalog_generation_id: CatalogGenerationId,
    payload: WorkProductEventPayloadV1,
}

impl WorkProductEventV1 {
    pub fn new(mut input: WorkProductEventInputV1) -> Result<Self, WorkProductEventContractError> {
        let valid_progression = match (&input.expected_graph_version, &input.payload) {
            (None, WorkProductEventPayloadV1::Created { graph }) => {
                input.result_graph_version == WorkGraphVersionV1::initial()
                    && graph.version() == WorkGraphVersionV1::initial()
            }
            (Some(expected), WorkProductEventPayloadV1::Changed { .. }) => expected
                .next()
                .ok()
                .is_some_and(|next| next == input.result_graph_version),
            _ => false,
        };
        if !valid_progression {
            return Err(WorkProductEventContractError::InvalidVersionProgression);
        }
        if let WorkProductEventPayloadV1::Changed {
            change: WorkGraphChangeV1::RelationReplanDecided { proposal, .. },
        } = &input.payload
        {
            if proposal.validate().is_err() {
                return Err(WorkProductEventContractError::InvalidPayload);
            }
        }
        if input.causation_event_id.as_ref() == Some(&input.event_id) {
            return Err(WorkProductEventContractError::SelfCausation);
        }
        canonicalize_unique(
            &mut input.authorized_relation_scopes,
            MAX_WORK_PRODUCT_EVENT_RELATION_SCOPES,
            WorkProductEventContractError::TooManyRelationScopes,
            WorkProductEventContractError::DuplicateRelationScope,
        )?;
        canonicalize_unique(
            &mut input.evidence,
            MAX_WORK_PRODUCT_EVENT_EVIDENCE,
            WorkProductEventContractError::TooMuchEvidence,
            WorkProductEventContractError::DuplicateEvidence,
        )?;
        if input.evidence.iter().any(|evidence| {
            !input
                .source_watermark
                .components()
                .contains_key(&evidence.source_store_id)
        }) {
            return Err(WorkProductEventContractError::MissingEvidenceSourceWatermark);
        }
        Ok(Self {
            event_id: input.event_id,
            sequence: input.sequence,
            actor_id: input.actor_id,
            owner_scope: input.owner_scope,
            authorized_relation_scopes: input.authorized_relation_scopes,
            expected_graph_version: input.expected_graph_version,
            result_graph_version: input.result_graph_version,
            command_id: input.command_id,
            canonical_input_digest: input.canonical_input_digest,
            causation_event_id: input.causation_event_id,
            evidence: input.evidence,
            source_watermark: input.source_watermark,
            occurred_at: input.occurred_at,
            policy_revision_id: input.policy_revision_id,
            configuration_revision_id: input.configuration_revision_id,
            catalog_generation_id: input.catalog_generation_id,
            payload: input.payload,
        })
    }

    pub fn event_id(&self) -> &WorkProductEventId {
        &self.event_id
    }

    pub const fn sequence(&self) -> WorkProductEventSequenceV1 {
        self.sequence
    }

    pub fn actor_id(&self) -> &ActorId {
        &self.actor_id
    }

    pub const fn owner_scope(&self) -> &WorkProductProfileScopeV1 {
        &self.owner_scope
    }

    pub fn authorized_relation_scopes(&self) -> &[WorkProductAuthorizedRelationScopeV1] {
        &self.authorized_relation_scopes
    }

    pub const fn expected_graph_version(&self) -> Option<WorkGraphVersionV1> {
        self.expected_graph_version
    }

    pub const fn result_graph_version(&self) -> WorkGraphVersionV1 {
        self.result_graph_version
    }

    pub fn command_id(&self) -> &WorkCommandId {
        &self.command_id
    }

    pub fn canonical_input_digest(&self) -> &ManifestDigest {
        &self.canonical_input_digest
    }

    pub fn causation_event_id(&self) -> Option<&WorkProductEventId> {
        self.causation_event_id.as_ref()
    }

    pub fn evidence(&self) -> &[WorkProductEventEvidenceV1] {
        &self.evidence
    }

    pub const fn source_watermark(&self) -> &WorkProductSourceWatermarkV1 {
        &self.source_watermark
    }

    pub const fn occurred_at(&self) -> UtcMicros {
        self.occurred_at
    }

    pub fn policy_revision_id(&self) -> &PolicyRevisionId {
        &self.policy_revision_id
    }

    pub fn configuration_revision_id(&self) -> &ConfigurationRevisionId {
        &self.configuration_revision_id
    }

    pub fn catalog_generation_id(&self) -> &CatalogGenerationId {
        &self.catalog_generation_id
    }

    pub const fn payload(&self) -> &WorkProductEventPayloadV1 {
        &self.payload
    }
}

impl<'de> Deserialize<'de> for WorkProductEventV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(WorkProductEventInputV1::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

fn canonicalize_unique<T: Ord>(
    values: &mut Vec<T>,
    maximum: usize,
    too_many: WorkProductEventContractError,
    duplicate: WorkProductEventContractError,
) -> Result<(), WorkProductEventContractError> {
    if values.len() > maximum {
        return Err(too_many);
    }
    if values.iter().collect::<BTreeSet<_>>().len() != values.len() {
        return Err(duplicate);
    }
    values.sort();
    Ok(())
}
