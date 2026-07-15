use std::error::Error;
use std::future::Future;

use tracedecay_domain::{
    CanonicalObservationIdV1, DurableClaudeObservationV1, ObservationContractError,
    ObservationScopeV1, PayloadDigestV1, PayloadReferenceV1,
};

use crate::{SessionMessageRecord, SessionRecord};

pub const CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION: &str = "claude-session-message-v1";

/// Immutable provenance for one observation-derived searchable message row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionProvenance {
    observation_id: CanonicalObservationIdV1,
    receipt_id: String,
    projector_version: &'static str,
}

impl ProjectionProvenance {
    pub fn observation_id(&self) -> &CanonicalObservationIdV1 {
        &self.observation_id
    }

    pub fn receipt_id(&self) -> &str {
        &self.receipt_id
    }

    pub fn projector_version(&self) -> &'static str {
        self.projector_version
    }
}

/// Pure output of the PR5 Claude observation projector.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeSessionMessageProjection {
    session: SessionRecord,
    message: SessionMessageRecord,
    provenance: ProjectionProvenance,
    output_digest: PayloadDigestV1,
}

impl ClaudeSessionMessageProjection {
    pub fn session(&self) -> &SessionRecord {
        &self.session
    }

    pub fn message(&self) -> &SessionMessageRecord {
        &self.message
    }

    pub fn provenance(&self) -> &ProjectionProvenance {
        &self.provenance
    }

    pub fn output_digest(&self) -> &PayloadDigestV1 {
        &self.output_digest
    }
}

/// Receipt-validated, deterministic mapping from one Claude JSONL observation
/// to the existing searchable session/message product rows.
pub fn project_claude_observation(
    observation: &DurableClaudeObservationV1,
) -> ProjectionStoreResult<ClaudeSessionMessageProjection> {
    if !observation
        .receipt()
        .disposition()
        .permits_durable_payload()
        || observation.receipt().payload() != Some(observation.payload_reference())
    {
        return Err(ProjectionStoreError::Contract(
            ObservationContractError::ReceiptPayloadMismatch,
        ));
    }

    let record = observation.payload();
    let record_type = record
        .get("type")
        .and_then(serde_json::Value::as_str)
        .ok_or(ProjectionStoreError::InvalidClaudePayload(
            "missing record type",
        ))?;
    if !matches!(record_type, "user" | "assistant") {
        return Err(ProjectionStoreError::InvalidClaudePayload(
            "record is not a conversational turn",
        ));
    }
    let message_value = record.get("message").unwrap_or(record);
    let role = message_value
        .get("role")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(record_type)
        .to_string();
    let content = message_value.get("content").unwrap_or(message_value);
    let index_content = if role == "assistant" {
        content.as_array().map(|blocks| {
            serde_json::Value::Array(
                blocks
                    .iter()
                    .filter(|block| {
                        !matches!(
                            block.get("type").and_then(serde_json::Value::as_str),
                            Some("thinking" | "redacted_thinking")
                        )
                    })
                    .cloned()
                    .collect(),
            )
        })
    } else {
        None
    };
    let index_content = index_content.as_ref().unwrap_or(content);
    let text = index_content
        .as_str()
        .map(ToString::to_string)
        .unwrap_or_else(|| index_content.to_string());
    if text.trim().is_empty() {
        return Err(ProjectionStoreError::InvalidClaudePayload(
            "message content is empty",
        ));
    }

    let session_id = observation.source().session_id().as_str().to_string();
    let position = observation.identity().position();
    let ordinal = i64::try_from(position.start())
        .map_err(|_| ProjectionStoreError::SequenceOverflow(position.start()))?;
    let message_id = message_value
        .get("id")
        .and_then(serde_json::Value::as_str)
        .or_else(|| record.get("uuid").and_then(serde_json::Value::as_str))
        .filter(|id| !id.is_empty())
        .map_or_else(
            || format!("{session_id}:{}", position.start()),
            ToString::to_string,
        );
    let scope_key = match observation.scope() {
        ObservationScopeV1::Profile => "user".to_string(),
        ObservationScopeV1::Project { project_id } => project_id.as_str().to_string(),
    };
    let mut tool_names = index_content
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| {
            matches!(
                block.get("type").and_then(serde_json::Value::as_str),
                Some("tool_use" | "tool_call" | "function_call")
            )
        })
        .filter_map(|block| block.get("name").and_then(serde_json::Value::as_str))
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    tool_names.sort();
    tool_names.dedup();

    let session = SessionRecord {
        provider: "claude".to_string(),
        session_id: session_id.clone(),
        project_key: scope_key.clone(),
        project_path: scope_key,
        title: None,
        started_at: None,
        ended_at: None,
        transcript_path: None,
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    };
    let message = SessionMessageRecord {
        provider: "claude".to_string(),
        message_id,
        session_id,
        role,
        timestamp: record.get("timestamp").and_then(serde_json::Value::as_i64),
        ordinal,
        text,
        kind: Some("message".to_string()),
        model: message_value
            .get("model")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string),
        tool_names: (!tool_names.is_empty()).then(|| tool_names.join(",")),
        source_path: None,
        source_offset: Some(ordinal),
        metadata_json: None,
    };
    let digest_value = serde_json::json!({
        "projector_version": CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION,
        "session": &session,
        "message": &message,
    });
    let output_digest = PayloadReferenceV1::for_payload(&digest_value)
        .map_err(ProjectionStoreError::Contract)?
        .digest()
        .clone();
    Ok(ClaudeSessionMessageProjection {
        session,
        message,
        provenance: ProjectionProvenance {
            observation_id: observation.observation_id().clone(),
            receipt_id: observation
                .receipt()
                .receipt()
                .receipt_id()
                .as_str()
                .to_string(),
            projector_version: CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION,
        },
        output_digest,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionCheckpoint {
    projector_version: &'static str,
    last_sequence: u64,
}

impl ProjectionCheckpoint {
    pub fn new(last_sequence: u64) -> Self {
        Self {
            projector_version: CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION,
            last_sequence,
        }
    }

    pub fn projector_version(&self) -> &'static str {
        self.projector_version
    }

    pub fn last_sequence(&self) -> u64 {
        self.last_sequence
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectionPersistOutcome {
    Projected(ProjectionCheckpoint),
    ExactDuplicate(ProjectionCheckpoint),
}

impl ProjectionPersistOutcome {
    pub fn checkpoint(&self) -> &ProjectionCheckpoint {
        match self {
            Self::Projected(checkpoint) | Self::ExactDuplicate(checkpoint) => checkpoint,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionRebuildOutcome {
    checkpoint: ProjectionCheckpoint,
    projected_rows: usize,
}

impl ProjectionRebuildOutcome {
    pub fn new(checkpoint: ProjectionCheckpoint, projected_rows: usize) -> Self {
        Self {
            checkpoint,
            projected_rows,
        }
    }

    pub fn checkpoint(&self) -> &ProjectionCheckpoint {
        &self.checkpoint
    }

    pub fn projected_rows(&self) -> usize {
        self.projected_rows
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectionStoreError {
    #[error("Claude observation payload is not projectable: {0}")]
    InvalidClaudePayload(&'static str),
    #[error("observation sequence {0} exceeds the supported integer range")]
    SequenceOverflow(u64),
    #[error("projector checkpoint gap: expected sequence {expected}, received {actual}")]
    Gap { expected: u64, actual: u64 },
    #[error("observation is not queued for projection")]
    NotQueued,
    #[error("observation does not exist")]
    ObservationNotFound,
    #[error("projection output collided at {provider}/{message_id}")]
    OutputCollision {
        provider: String,
        message_id: String,
    },
    #[error("projection provenance collided with an existing output")]
    ProvenanceCollision,
    #[error("projection rebuild frontier {frontier} is past committed sequence {committed}")]
    InvalidRebuildFrontier { frontier: u64, committed: u64 },
    #[error("observation contract validation failed")]
    Contract(#[source] ObservationContractError),
    #[error("projection storage operation {operation} failed")]
    Storage {
        operation: &'static str,
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
}

pub type ProjectionStoreResult<T> = Result<T, ProjectionStoreError>;

pub trait ObservationProjectionStore: Send + Sync {
    fn project_observation(
        &self,
        observation_id: &CanonicalObservationIdV1,
    ) -> impl Future<Output = ProjectionStoreResult<ProjectionPersistOutcome>> + Send;

    fn projection_checkpoint(
        &self,
    ) -> impl Future<Output = ProjectionStoreResult<ProjectionCheckpoint>> + Send;

    fn rebuild_projection(
        &self,
        frontier_sequence: u64,
    ) -> impl Future<Output = ProjectionStoreResult<ProjectionRebuildOutcome>> + Send;
}
