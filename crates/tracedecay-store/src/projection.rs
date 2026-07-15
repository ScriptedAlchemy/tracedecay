use std::error::Error;
use std::future::Future;

use tracedecay_domain::{
    CanonicalObservationIdV1, DurableObservationV1, ObservationContractError, PayloadDigestV1,
    PayloadReferenceV1,
};

use crate::{SessionMessageRecord, SessionRecord};

pub const SESSION_MESSAGE_PROJECTOR_VERSION_V1: &str = "claude-session-message-v1";
pub const CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION: &str = SESSION_MESSAGE_PROJECTOR_VERSION_V1;

/// Immutable provenance for one observation-derived searchable message row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionProvenance {
    observation_id: CanonicalObservationIdV1,
    receipt_id: String,
}

impl ProjectionProvenance {
    pub fn observation_id(&self) -> &CanonicalObservationIdV1 {
        &self.observation_id
    }

    pub fn receipt_id(&self) -> &str {
        &self.receipt_id
    }

    pub fn projector_version(&self) -> &'static str {
        SESSION_MESSAGE_PROJECTOR_VERSION_V1
    }
}

/// Non-blocking disposition for a valid observation that produces no view row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionSkipReason {
    NonConversationalRecord,
}

impl ProjectionSkipReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NonConversationalRecord => "non_conversational_record",
        }
    }
}

/// Deterministic effect derived from one receipt-bound observation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObservationProjection {
    Message(Box<SessionMessageProjection>),
    Skipped(ProjectionSkipReason),
}

impl ObservationProjection {
    pub fn message(&self) -> Option<&SessionMessageProjection> {
        match self {
            Self::Message(projection) => Some(projection),
            Self::Skipped(_) => None,
        }
    }

    pub fn skip_reason(&self) -> Option<ProjectionSkipReason> {
        match self {
            Self::Message(_) => None,
            Self::Skipped(reason) => Some(*reason),
        }
    }

    pub fn for_message(
        observation: &DurableObservationV1,
        session: SessionRecord,
        message: SessionMessageRecord,
    ) -> ProjectionStoreResult<Self> {
        let digest_value = serde_json::json!({
            "projector_version": SESSION_MESSAGE_PROJECTOR_VERSION_V1,
            "session": &session,
            "message": &message,
        });
        let output_digest = PayloadReferenceV1::for_payload(&digest_value)
            .map_err(ProjectionStoreError::Contract)?
            .digest()
            .clone();
        Ok(Self::Message(Box::new(SessionMessageProjection {
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
            },
            output_digest,
        })))
    }

    pub fn for_skip(
        _observation: &DurableObservationV1,
        reason: ProjectionSkipReason,
    ) -> ProjectionStoreResult<Self> {
        Ok(Self::Skipped(reason))
    }
}

/// Deterministic searchable message derived from one durable observation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionMessageProjection {
    session: SessionRecord,
    message: SessionMessageRecord,
    provenance: ProjectionProvenance,
    output_digest: PayloadDigestV1,
}

impl SessionMessageProjection {
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

pub type ClaudeObservationProjection = ObservationProjection;
pub type ClaudeSessionMessageProjection = SessionMessageProjection;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionCheckpoint {
    last_sequence: u64,
}

impl ProjectionCheckpoint {
    pub fn new(last_sequence: u64) -> Self {
        Self { last_sequence }
    }

    pub fn projector_version(&self) -> &'static str {
        SESSION_MESSAGE_PROJECTOR_VERSION_V1
    }

    pub fn last_sequence(&self) -> u64 {
        self.last_sequence
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectionPersistOutcome {
    Projected(ProjectionCheckpoint),
    Skipped {
        checkpoint: ProjectionCheckpoint,
        reason: ProjectionSkipReason,
    },
    ExactDuplicate(ProjectionCheckpoint),
}

impl ProjectionPersistOutcome {
    pub fn checkpoint(&self) -> &ProjectionCheckpoint {
        match self {
            Self::Projected(checkpoint) | Self::ExactDuplicate(checkpoint) => checkpoint,
            Self::Skipped { checkpoint, .. } => checkpoint,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionRebuildOutcome {
    checkpoint: ProjectionCheckpoint,
    projected_rows: usize,
    skipped_observations: usize,
}

impl ProjectionRebuildOutcome {
    pub fn new(
        checkpoint: ProjectionCheckpoint,
        projected_rows: usize,
        skipped_observations: usize,
    ) -> Self {
        Self {
            checkpoint,
            projected_rows,
            skipped_observations,
        }
    }

    pub fn checkpoint(&self) -> &ProjectionCheckpoint {
        &self.checkpoint
    }

    pub fn projected_rows(&self) -> usize {
        self.projected_rows
    }

    pub fn skipped_observations(&self) -> usize {
        self.skipped_observations
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectionStoreError {
    #[error("observation sequence {0} exceeds the supported integer range")]
    SequenceOverflow(u64),
    #[error("projector checkpoint gap: expected sequence {expected}, received {actual}")]
    Gap { expected: u64, actual: u64 },
    #[error("observation is not queued for projection")]
    NotQueued,
    #[error("observation does not exist")]
    ObservationNotFound,
    #[error("provider {0} does not have a projection mapper")]
    UnsupportedProvider(String),
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
    /// Returns at most one queued observation in authoritative sequence order.
    /// Callers retain cancellation and batch-budget control between items.
    fn next_queued_observation(
        &self,
    ) -> impl Future<Output = ProjectionStoreResult<Option<CanonicalObservationIdV1>>> + Send;

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
