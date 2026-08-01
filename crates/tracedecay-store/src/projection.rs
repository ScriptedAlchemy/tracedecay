use std::error::Error;
use std::future::Future;
use std::sync::OnceLock;

use serde_json::Value;
use tracedecay_domain::{
    CanonicalObservationIdV1, CanonicalWorkflowSemanticKindV1, DomainError, DurableObservationV1,
    ObservationContractError, PayloadDigestV1, PayloadReferenceV1, RetrievalAnchorId,
    SanitizationReceiptRefV1, derive_exact_observation_anchor_id,
};

use crate::{SessionMessageRecord, SessionRecord};

#[cfg(test)]
mod tests;

pub const SESSION_MESSAGE_PROJECTOR_VERSION_V1: &str = "claude-session-message-v1";
pub const SESSION_MESSAGE_PROJECTOR_VERSION_V2: &str = "claude-session-message-v2";
pub const SESSION_MESSAGE_PROJECTOR_VERSION_V3: &str = "claude-session-message-v3";
pub const SESSION_MESSAGE_PROJECTOR_VERSION_V4: &str = "claude-session-message-v4";
pub const SESSION_MESSAGE_PROJECTOR_VERSION: &str = SESSION_MESSAGE_PROJECTOR_VERSION_V4;
pub const CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION: &str = SESSION_MESSAGE_PROJECTOR_VERSION;

/// Immutable provenance for one observation-derived searchable message row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionProvenance {
    observation_id: CanonicalObservationIdV1,
    retrieval_anchor_id: RetrievalAnchorId,
    receipt: SanitizationReceiptRefV1,
}

impl ProjectionProvenance {
    fn for_observation(observation: &DurableObservationV1) -> ProjectionStoreResult<Self> {
        Ok(Self {
            observation_id: observation.observation_id().clone(),
            retrieval_anchor_id: derive_exact_observation_anchor_id(
                observation.scope(),
                observation.observation_id(),
            )?,
            receipt: observation.receipt().receipt().clone(),
        })
    }

    pub fn observation_id(&self) -> &CanonicalObservationIdV1 {
        &self.observation_id
    }

    pub fn retrieval_anchor_id(&self) -> &RetrievalAnchorId {
        &self.retrieval_anchor_id
    }

    pub fn receipt(&self) -> &SanitizationReceiptRefV1 {
        &self.receipt
    }

    pub fn receipt_id(&self) -> &str {
        self.receipt.receipt_id().as_str()
    }

    pub fn projector_version(&self) -> &'static str {
        SESSION_MESSAGE_PROJECTOR_VERSION
    }
}

/// Non-blocking disposition for a valid observation that produces no view row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionSkipReason {
    NonConversationalRecord,
    /// The observation's deterministic output identity is already owned by a
    /// different observation (duplicate-era provider records). The first
    /// binder keeps the output; this observation converges as a durable,
    /// auditable skip instead of wedging the projection queue.
    OutputCollision,
}

impl ProjectionSkipReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NonConversationalRecord => "non_conversational_record",
            Self::OutputCollision => "output_collision",
        }
    }
}

/// Deterministic effect derived from one receipt-bound observation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObservationProjection {
    Message(Box<SessionMessageProjection>),
    Composite {
        message: Option<Box<SessionMessageProjection>>,
        derived_messages: Vec<SessionMessageProjection>,
        workflow_facts: Vec<WorkflowFactProjection>,
    },
    Skipped(ProjectionSkipReason),
}

impl ObservationProjection {
    pub fn message(&self) -> Option<&SessionMessageProjection> {
        match self {
            Self::Message(projection) => Some(projection),
            Self::Composite { message, .. } => message.as_deref(),
            Self::Skipped(_) => None,
        }
    }

    pub fn workflow_facts(&self) -> &[WorkflowFactProjection] {
        match self {
            Self::Composite { workflow_facts, .. } => workflow_facts,
            Self::Message(_) | Self::Skipped(_) => &[],
        }
    }

    pub fn messages(&self) -> impl Iterator<Item = &SessionMessageProjection> {
        let derived_messages: &[SessionMessageProjection] = match self {
            Self::Composite {
                derived_messages, ..
            } => derived_messages,
            Self::Message(_) | Self::Skipped(_) => &[],
        };
        self.message().into_iter().chain(derived_messages)
    }

    pub fn output_count(&self) -> usize {
        self.messages().count() + self.workflow_facts().len()
    }

    pub fn skip_reason(&self) -> Option<ProjectionSkipReason> {
        match self {
            Self::Message(_) | Self::Composite { .. } => None,
            Self::Skipped(reason) => Some(*reason),
        }
    }

    pub fn for_message(
        observation: &DurableObservationV1,
        session: SessionRecord,
        message: SessionMessageRecord,
    ) -> ProjectionStoreResult<Self> {
        let provenance = ProjectionProvenance::for_observation(observation)?;
        Ok(Self::Message(Box::new(Self::message_projection(
            provenance, session, message, 0,
        ))))
    }

    /// Binds one message output to its observation provenance.
    ///
    /// The deterministic `output_digest` is a pure function of the projector
    /// version, ordinal, session, and message, so it is derived lazily on first
    /// use (see [`SessionMessageProjection::output_digest`]) instead of on
    /// every derivation. Read paths that only need the projected records —
    /// temporal hydration, occurrence materialization, parent resolution —
    /// therefore never pay the canonical-JSON plus SHA-256 cost, while write
    /// paths that persist the digest observe byte-identical values.
    fn message_projection(
        provenance: ProjectionProvenance,
        session: SessionRecord,
        message: SessionMessageRecord,
        output_ordinal: u32,
    ) -> SessionMessageProjection {
        SessionMessageProjection {
            session,
            message,
            provenance,
            output_digest: OnceLock::new(),
            output_ordinal,
        }
    }

    pub fn for_outputs(
        observation: &DurableObservationV1,
        messages: Vec<(SessionRecord, SessionMessageRecord)>,
        workflow_facts: Vec<(SessionRecord, WorkflowFactRecord)>,
    ) -> ProjectionStoreResult<Self> {
        if messages.is_empty() && workflow_facts.is_empty() {
            return Err(ProjectionStoreError::Contract(
                ObservationContractError::InvalidCanonicalPayload,
            ));
        }
        // Provenance is a pure function of the observation, so the anchor
        // derivation runs once per observation and is cloned across outputs
        // instead of once per output row.
        let provenance = ProjectionProvenance::for_observation(observation)?;
        let mut messages = messages
            .into_iter()
            .enumerate()
            .map(|(ordinal, (session, message))| {
                let ordinal = u32::try_from(ordinal).map_err(|_| {
                    ProjectionStoreError::Contract(
                        ObservationContractError::InvalidCanonicalPayload,
                    )
                })?;
                Ok(Self::message_projection(
                    provenance.clone(),
                    session,
                    message,
                    ordinal,
                ))
            })
            .collect::<ProjectionStoreResult<Vec<_>>>()?;
        let workflow_facts = workflow_facts
            .into_iter()
            .map(|(session, fact)| WorkflowFactProjection::new(provenance.clone(), session, fact))
            .collect::<Vec<_>>();
        if workflow_facts.is_empty() && messages.len() == 1 {
            return Ok(Self::Message(Box::new(messages.remove(0))));
        }
        let message = (!messages.is_empty()).then(|| Box::new(messages.remove(0)));
        Ok(Self::Composite {
            message,
            derived_messages: messages,
            workflow_facts,
        })
    }

    pub fn for_skip(
        _observation: &DurableObservationV1,
        reason: ProjectionSkipReason,
    ) -> ProjectionStoreResult<Self> {
        Ok(Self::Skipped(reason))
    }
}

/// Deterministic searchable message derived from one durable observation.
#[derive(Clone, Debug)]
pub struct SessionMessageProjection {
    session: SessionRecord,
    message: SessionMessageRecord,
    provenance: ProjectionProvenance,
    output_digest: OnceLock<PayloadDigestV1>,
    output_ordinal: u32,
}

/// `output_digest` is a memoized pure function of the projector version, the
/// ordinal, the session, and the message, so comparing those inputs is exactly
/// equivalent to comparing the derived digest. Equality must not depend on
/// whether a projection happens to have materialized its memo yet.
impl PartialEq for SessionMessageProjection {
    fn eq(&self, other: &Self) -> bool {
        self.session == other.session
            && self.message == other.message
            && self.provenance == other.provenance
            && self.output_ordinal == other.output_ordinal
    }
}

impl Eq for SessionMessageProjection {}

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

    /// Deterministic content digest of this output, derived on first use and
    /// memoized thereafter. The digested value is byte-identical to the
    /// eagerly derived form: same projector version, ordinal, session, and
    /// message, canonicalized by the same encoder.
    pub fn output_digest(&self) -> ProjectionStoreResult<&PayloadDigestV1> {
        if let Some(digest) = self.output_digest.get() {
            return Ok(digest);
        }
        let digest = message_output_digest(&self.session, &self.message, self.output_ordinal)?;
        Ok(self.output_digest.get_or_init(|| digest))
    }

    pub fn output_ordinal(&self) -> u32 {
        self.output_ordinal
    }
}

fn message_output_digest(
    session: &SessionRecord,
    message: &SessionMessageRecord,
    output_ordinal: u32,
) -> ProjectionStoreResult<PayloadDigestV1> {
    let digest_value = serde_json::json!({
        "projector_version": SESSION_MESSAGE_PROJECTOR_VERSION,
        "output_ordinal": output_ordinal,
        "session": session,
        "message": message,
    });
    Ok(PayloadReferenceV1::for_payload(&digest_value)
        .map_err(ProjectionStoreError::Contract)?
        .digest()
        .clone())
}

/// Provider-neutral workflow row derived from one canonical semantic fact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowFactRecord {
    pub fact_ordinal: u32,
    pub semantic_kind: CanonicalWorkflowSemanticKindV1,
    pub provider_reference: Option<String>,
    pub item_id: Option<String>,
    pub parent_reference: Option<String>,
    pub list_reference: Option<String>,
    pub state: Option<String>,
    pub status: Option<String>,
    pub item_order: Option<u64>,
    pub native_revision: Option<String>,
    pub event_sequence: Option<u64>,
    pub source_sequence: Option<u64>,
    pub native_timestamp: Option<i64>,
    pub ordering_domain: String,
    pub content: Option<Value>,
    pub content_text: String,
}

/// Deterministic normalized workflow output and receipt provenance.
#[derive(Clone, Debug)]
pub struct WorkflowFactProjection {
    session: SessionRecord,
    fact: WorkflowFactRecord,
    provenance: ProjectionProvenance,
    output_digest: OnceLock<PayloadDigestV1>,
}

/// See [`SessionMessageProjection`]'s equality note: the memoized digest is a
/// pure function of the compared fields.
impl PartialEq for WorkflowFactProjection {
    fn eq(&self, other: &Self) -> bool {
        self.session == other.session
            && self.fact == other.fact
            && self.provenance == other.provenance
    }
}

impl Eq for WorkflowFactProjection {}

impl WorkflowFactProjection {
    fn new(
        provenance: ProjectionProvenance,
        session: SessionRecord,
        fact: WorkflowFactRecord,
    ) -> Self {
        Self {
            session,
            fact,
            provenance,
            output_digest: OnceLock::new(),
        }
    }

    pub fn session(&self) -> &SessionRecord {
        &self.session
    }

    pub fn fact(&self) -> &WorkflowFactRecord {
        &self.fact
    }

    pub fn provenance(&self) -> &ProjectionProvenance {
        &self.provenance
    }

    /// Deterministic content digest of this workflow output, derived on first
    /// use and memoized thereafter. Byte-identical to the eagerly derived form.
    pub fn output_digest(&self) -> ProjectionStoreResult<&PayloadDigestV1> {
        if let Some(digest) = self.output_digest.get() {
            return Ok(digest);
        }
        let digest = workflow_fact_output_digest(&self.session, &self.fact)?;
        Ok(self.output_digest.get_or_init(|| digest))
    }
}

fn workflow_fact_output_digest(
    session: &SessionRecord,
    fact: &WorkflowFactRecord,
) -> ProjectionStoreResult<PayloadDigestV1> {
    let digest_value = serde_json::json!({
        "projector_version": SESSION_MESSAGE_PROJECTOR_VERSION,
        "session": session,
        "fact": {
            "fact_ordinal": fact.fact_ordinal,
            "semantic_kind": fact.semantic_kind,
            "provider_reference": fact.provider_reference,
            "item_id": fact.item_id,
            "parent_reference": fact.parent_reference,
            "list_reference": fact.list_reference,
            "state": fact.state,
            "status": fact.status,
            "item_order": fact.item_order,
            "native_revision": fact.native_revision,
            "event_sequence": fact.event_sequence,
            "source_sequence": fact.source_sequence,
            "native_timestamp": fact.native_timestamp,
            "ordering_domain": fact.ordering_domain,
            "content": fact.content,
            "content_text": fact.content_text,
        },
    });
    Ok(PayloadReferenceV1::for_payload(&digest_value)
        .map_err(ProjectionStoreError::Contract)?
        .digest()
        .clone())
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
        SESSION_MESSAGE_PROJECTOR_VERSION
    }

    pub fn last_sequence(&self) -> u64 {
        self.last_sequence
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectionPersistOutcome {
    Projected(ProjectedObservation),
    Skipped {
        checkpoint: ProjectionCheckpoint,
        reason: ProjectionSkipReason,
    },
    ExactDuplicate(ProjectionCheckpoint),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectedObservation {
    checkpoint: ProjectionCheckpoint,
    output_count: usize,
}

impl ProjectedObservation {
    pub fn new(checkpoint: ProjectionCheckpoint, output_count: usize) -> Self {
        Self {
            checkpoint,
            output_count,
        }
    }

    pub fn checkpoint(&self) -> &ProjectionCheckpoint {
        &self.checkpoint
    }

    pub fn output_count(&self) -> usize {
        self.output_count
    }
}

impl ProjectionPersistOutcome {
    pub fn checkpoint(&self) -> &ProjectionCheckpoint {
        match self {
            Self::Projected(projected) => projected.checkpoint(),
            Self::ExactDuplicate(checkpoint) => checkpoint,
            Self::Skipped { checkpoint, .. } => checkpoint,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionRebuildOutcome {
    checkpoint: ProjectionCheckpoint,
    projected_rows: usize,
    skipped_observations: usize,
    complete: bool,
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
            complete: true,
        }
    }

    pub fn in_progress(
        checkpoint: ProjectionCheckpoint,
        projected_rows: usize,
        skipped_observations: usize,
    ) -> Self {
        Self {
            checkpoint,
            projected_rows,
            skipped_observations,
            complete: false,
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

    pub fn is_complete(&self) -> bool {
        self.complete
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
    #[error("projection anchor contract validation failed")]
    Anchor(#[from] DomainError),
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
