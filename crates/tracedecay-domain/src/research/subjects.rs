use serde::{Deserialize, Serialize};

use super::error::DomainError;
use super::evidence::LogSafeText;
use super::id::{
    ActorId, AgentInstanceId, AuditReceiptId, CatalogGenerationId, CommitId, EntityId,
    EntityVersionId, GoalId, HostInstanceId, LocatorDigest, ManifestDigest, MessageId,
    OrchestrationAgentLabel, OrchestrationObservationId, ProjectId, ProviderId, RefId,
    RepositoryId, SessionId, SourceStoreId, ThreadId, ToolInvocationId, TurnId, WorktreeId,
};

/// Canonical entity categories needed by the research slice.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum EntityKind {
    Actor,
    Repository,
    Project,
    PullRequest,
    Check,
    Review,
    Release,
    Session,
    Message,
    Workflow,
    ResponseHandle,
    SourceRecord,
    WebSource,
    Document,
    Plan,
    Artifact,
    Other(LogSafeText),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct EntityRef {
    pub id: EntityId,
    pub kind: EntityKind,
}

impl EntityRef {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.id.validate()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct EntityVersionRef {
    pub entity: EntityRef,
    pub version: Option<EntityVersionId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct ActorRef {
    pub actor_id: ActorId,
    pub version: Option<EntityVersionId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct AuditReceiptRef {
    pub receipt_id: AuditReceiptId,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct CatalogSnapshotRefV1 {
    pub generation: CatalogGenerationId,
    pub digest: ManifestDigest,
}

impl CatalogSnapshotRefV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.generation.validate()?;
        self.digest.validate()
    }
}

/// Source-local position without literal path or source text.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourcePosition {
    ByteOffset { start: u64, end: u64 },
    RowId { row_id: i64 },
    Sequence { sequence: u64 },
    ObjectKey { digest: LocatorDigest },
}

impl SourcePosition {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::ByteOffset { start, end } if start > end => Err(DomainError::UnknownReference {
                field: "source position byte range",
            }),
            Self::ObjectKey { digest } => digest.validate(),
            Self::ByteOffset { .. } | Self::RowId { .. } | Self::Sequence { .. } => Ok(()),
        }
    }
}

/// Provider-linked activity identity and correlation evidence.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct ActivityResearchFacetV1 {
    pub provider: ProviderId,
    pub host: Option<HostInstanceId>,
    pub source_store_id: Option<SourceStoreId>,
    pub session_id: SessionId,
    pub thread_id: Option<ThreadId>,
    pub turn_id: Option<TurnId>,
    pub message_id: Option<MessageId>,
    pub agent_instance_id: Option<AgentInstanceId>,
    pub parent_session_id: Option<SessionId>,
    pub parent_tool_use_id: Option<ToolInvocationId>,
    pub orchestration_observation_id: Option<OrchestrationObservationId>,
    pub orchestration_agent_label: Option<OrchestrationAgentLabel>,
    pub goal_id: Option<GoalId>,
}

impl ActivityResearchFacetV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.provider.validate()?;
        self.session_id.validate()?;
        if let Some(value) = &self.source_store_id {
            value.validate()?;
        }
        if let Some(value) = &self.message_id {
            value.validate()?;
            if self.source_store_id.is_none() {
                return Err(DomainError::UnknownReference {
                    field: "message source_store_id",
                });
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct GitResearchSubjectV1 {
    pub repository_id: RepositoryId,
    pub project_id: Option<ProjectId>,
    pub worktree_id: Option<WorktreeId>,
    pub ref_id: Option<RefId>,
    pub commit_id: Option<CommitId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct DeliveryResearchSubjectV1 {
    pub repository_id: RepositoryId,
    pub delivery_entity: EntityRef,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct SourceResearchSubjectV1 {
    pub source_store_id: SourceStoreId,
    pub source_entity: EntityRef,
    pub source_position: Option<SourcePosition>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct WebResearchSubjectV1 {
    pub source_manifest: EntityRef,
    pub captured_document: Option<EntityRef>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct DocumentResearchSubjectV1 {
    pub document: EntityRef,
    pub version: Option<EntityVersionRef>,
}

/// Closed primary-subject union. Non-activity subjects may carry a separate activity facet.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(
    tag = "kind",
    content = "subject",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ResearchAnchorSubjectV1 {
    Activity(ActivityResearchFacetV1),
    Git(GitResearchSubjectV1),
    Delivery(DeliveryResearchSubjectV1),
    Source(SourceResearchSubjectV1),
    Web(WebResearchSubjectV1),
    Document(DocumentResearchSubjectV1),
}

impl ResearchAnchorSubjectV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Activity(value) => value.validate(),
            Self::Git(value) => value.repository_id.validate(),
            Self::Delivery(value) => {
                value.repository_id.validate()?;
                value.delivery_entity.validate()
            }
            Self::Source(value) => {
                value.source_store_id.validate()?;
                value.source_entity.validate()?;
                if let Some(position) = &value.source_position {
                    position.validate()?;
                }
                Ok(())
            }
            Self::Web(value) => {
                value.source_manifest.validate()?;
                if let Some(document) = &value.captured_document {
                    document.validate()?;
                }
                Ok(())
            }
            Self::Document(value) => {
                value.document.validate()?;
                if let Some(version) = &value.version {
                    version.entity.validate()?;
                    if version.entity.id != value.document.id {
                        return Err(DomainError::UnknownReference {
                            field: "document version",
                        });
                    }
                }
                Ok(())
            }
        }
    }

    pub(crate) fn activity_facet(&self) -> Option<&ActivityResearchFacetV1> {
        match self {
            Self::Activity(value) => Some(value),
            _ => None,
        }
    }
}
