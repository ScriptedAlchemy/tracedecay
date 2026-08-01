use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;
use tracedecay_domain::{
    CanonicalObservationIdV1, ObservationContractError, ObservationIdentityMaterialV1,
    ObservationSourceCursorV1, ProjectionGenerationId, RetentionClass, SanitizationReceiptV1,
    UtcMicros,
};
use tracedecay_store::observation::{CursorAdvanceOutcome, ObservationCursorAdvance};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationAdmissionPort, ObservationCaptureSink,
    ObservationCursorPort, ObservationPersistOutcome, ObservationProjectionStatus,
    ObservationReplayRequest, ObservationStoreError, ObservationWrite,
    SESSION_MESSAGE_PROJECTOR_VERSION, StoredObservation,
    build_observation_resolution_authorization_v1, build_observation_retrieval_anchor_v2,
};

use crate::repository_provenance::RepositoryProvenanceAdmissionContext;
use tracedecay_runtime_core::privacy::{
    ObservationSanitizationOutcomeV1, ParsedObservationRecordV1, PrivacySanitizerError,
    RecordSanitizerV1, SanitizationFindingV1, SanitizedObservationRecordV1,
};

/// Cloneable, operation-local cancellation shared by application adapters.
#[derive(Clone, Debug, Default)]
pub struct ObservationCancellation {
    cancelled: Arc<AtomicBool>,
}

impl ObservationCancellation {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CaptureObservationRequestError {
    #[error("parsed observation source range does not match observation identity")]
    SourceRangeMismatch,
    #[error("parsed observation ordering domain does not match observation identity")]
    OrderingDomainMismatch,
}

/// One validated, bounded provider record ready for the mandatory privacy boundary.
pub struct CaptureObservationRequest {
    parsed_record: ParsedObservationRecordV1,
    identity: ObservationIdentityMaterialV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
    resume_checkpoint: Option<(u64, u64)>,
    retention_class: RetentionClass,
    cancellation: ObservationCancellation,
    repository_provenance: Option<RepositoryProvenanceAdmissionContext>,
}

impl CaptureObservationRequest {
    pub fn new(
        parsed_record: ParsedObservationRecordV1,
        identity: ObservationIdentityMaterialV1,
        expected_cursor: Option<ObservationSourceCursorV1>,
        retention_class: RetentionClass,
        cancellation: ObservationCancellation,
    ) -> Result<Self, CaptureObservationRequestError> {
        let position = identity.position();
        if *parsed_record.source_range() != position {
            return Err(CaptureObservationRequestError::SourceRangeMismatch);
        }
        if parsed_record.ordering_domain() != identity.ordering_domain() {
            return Err(CaptureObservationRequestError::OrderingDomainMismatch);
        }
        Ok(Self {
            parsed_record,
            identity,
            expected_cursor,
            resume_checkpoint: None,
            retention_class,
            cancellation,
            repository_provenance: None,
        })
    }

    pub fn provider(&self) -> &str {
        self.identity.source().provider().as_str()
    }

    pub fn scope(&self) -> &tracedecay_domain::ObservationScopeV1 {
        self.identity.scope()
    }

    #[must_use]
    pub fn with_resume_checkpoint(mut self, file_identity: u64, resume_fingerprint: u64) -> Self {
        self.resume_checkpoint = Some((file_identity, resume_fingerprint));
        self
    }

    pub fn with_repository_provenance(
        mut self,
        repository_provenance: Option<RepositoryProvenanceAdmissionContext>,
    ) -> Self {
        self.repository_provenance = repository_provenance;
        self
    }
}

pub type CaptureClaudeObservationRequest = CaptureObservationRequest;
pub type CaptureClaudeObservationRequestError = CaptureObservationRequestError;

fn observation_ingested_at() -> UtcMicros {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    UtcMicros(i64::try_from(micros).unwrap_or(i64::MAX))
}

pub struct GetObservationRequest {
    observation_id: CanonicalObservationIdV1,
    cancellation: ObservationCancellation,
}

impl GetObservationRequest {
    pub fn new(
        observation_id: CanonicalObservationIdV1,
        cancellation: ObservationCancellation,
    ) -> Self {
        Self {
            observation_id,
            cancellation,
        }
    }
}

pub struct ReplayObservationsRequest {
    replay: ObservationReplayRequest,
    cancellation: ObservationCancellation,
}

pub struct AdvanceNonDurableSourceCursorRequest {
    advance: ObservationCursorAdvance,
    cancellation: ObservationCancellation,
}

impl AdvanceNonDurableSourceCursorRequest {
    pub fn new(advance: ObservationCursorAdvance, cancellation: ObservationCancellation) -> Self {
        Self {
            advance,
            cancellation,
        }
    }
}

impl ReplayObservationsRequest {
    pub fn new(replay: ObservationReplayRequest, cancellation: ObservationCancellation) -> Self {
        Self {
            replay,
            cancellation,
        }
    }
}

/// Result of mandatory sanitization and, when permitted, authoritative persistence.
#[derive(Debug)]
pub enum CaptureObservationOutcome {
    Persisted {
        outcome: Box<ObservationPersistOutcome>,
        projection_status: ObservationProjectionStatus,
        sanitized_record: Box<SanitizedObservationRecordV1>,
        findings: Vec<SanitizationFindingV1>,
    },
    Rejected {
        receipt: SanitizationReceiptV1,
        findings: Vec<SanitizationFindingV1>,
    },
    Quarantined {
        receipt: SanitizationReceiptV1,
        findings: Vec<SanitizationFindingV1>,
    },
}

impl CaptureObservationOutcome {
    pub fn sanitization_receipt(&self) -> &SanitizationReceiptV1 {
        match self {
            Self::Persisted { outcome, .. } => outcome.receipt().sanitization_receipt(),
            Self::Rejected { receipt, .. } | Self::Quarantined { receipt, .. } => receipt,
        }
    }

    pub fn findings(&self) -> &[SanitizationFindingV1] {
        match self {
            Self::Persisted { findings, .. }
            | Self::Rejected { findings, .. }
            | Self::Quarantined { findings, .. } => findings,
        }
    }
}

pub type CaptureClaudeObservationOutcome = CaptureObservationOutcome;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservationReplayCoverage {
    Complete,
    Partial,
}

#[derive(Debug)]
pub struct ObservationReplayPage {
    observations: Vec<StoredObservation>,
    coverage: ObservationReplayCoverage,
    has_more: bool,
    next_after_sequence: Option<u64>,
}

#[derive(Debug)]
pub struct ObservationPointRead {
    observation: Option<StoredObservation>,
    coverage: ObservationReplayCoverage,
}

impl ObservationPointRead {
    pub fn observation(&self) -> Option<&StoredObservation> {
        self.observation.as_ref()
    }

    pub fn coverage(&self) -> ObservationReplayCoverage {
        self.coverage
    }
}

impl ObservationReplayPage {
    pub fn observations(&self) -> &[StoredObservation] {
        &self.observations
    }

    pub fn coverage(&self) -> ObservationReplayCoverage {
        self.coverage
    }

    pub fn has_more(&self) -> bool {
        self.has_more
    }

    pub fn next_after_sequence(&self) -> Option<u64> {
        self.next_after_sequence
    }
}

#[derive(Debug, Error)]
pub enum ObservationApplicationError {
    #[error("observation contract is invalid")]
    Contract(#[from] ObservationContractError),
    #[error("observation sanitization failed")]
    Privacy(#[from] PrivacySanitizerError),
    #[error("observation store operation failed")]
    Store(#[from] ObservationStoreError),
    #[error("persisted observation is not readable from the authoritative store")]
    PersistedObservationUnavailable,
    #[error("observation operation was cancelled")]
    Cancelled,
}

/// Application-owned composition of sanitizer and an already-authoritative store.
pub struct ObservationApplication<S> {
    store: S,
    sanitizer: RecordSanitizerV1,
}

impl<S> ObservationApplication<S>
where
    S: ObservationCaptureSink + ObservationCursorPort + ObservationAdmissionPort,
{
    pub fn new(store: S, sanitizer: RecordSanitizerV1) -> Self {
        Self { store, sanitizer }
    }

    /// Advances a validated non-durable frame cursor without exposing the store.
    pub async fn advance_non_durable_source_cursor(
        &self,
        request: AdvanceNonDurableSourceCursorRequest,
    ) -> Result<CursorAdvanceOutcome, ObservationApplicationError> {
        let AdvanceNonDurableSourceCursorRequest {
            advance,
            cancellation,
        } = request;
        if cancellation.is_cancelled() {
            return Err(ObservationApplicationError::Cancelled);
        }
        let outcome = self
            .store
            .advance_admitted_source_cursor(advance)
            .await
            .map_err(ObservationApplicationError::from)?;
        if cancellation.is_cancelled() {
            return Err(ObservationApplicationError::Cancelled);
        }
        Ok(outcome)
    }

    /// Boxes the whole admission future at this shared chokepoint so every
    /// caller (the cursor JSONL per-frame loop, composer, Claude, Hermes,
    /// snapshot) inherits a bounded debug poll frame without pinning at the
    /// call site. Keeping this the boxing boundary is what lets the busy
    /// ingest loops drop their per-frame `Box::pin`.
    pub fn capture_observation(
        &self,
        request: CaptureObservationRequest,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<CaptureObservationOutcome, ObservationApplicationError>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let CaptureObservationRequest {
                parsed_record,
                identity,
                expected_cursor,
                resume_checkpoint,
                retention_class,
                cancellation,
                repository_provenance,
            } = request;
            if cancellation.is_cancelled() {
                return Err(ObservationApplicationError::Cancelled);
            }
            let sanitized =
                self.sanitizer
                    .sanitize_parsed(parsed_record, identity, retention_class)?;
            if cancellation.is_cancelled() {
                return Err(ObservationApplicationError::Cancelled);
            }
            match sanitized {
                ObservationSanitizationOutcomeV1::Durable {
                    observation,
                    sanitized_record,
                    findings,
                } => {
                    let identity = observation.identity();
                    let mut next_cursor = ObservationSourceCursorV1::for_ordering(
                        identity.source().clone(),
                        identity.scope().clone(),
                        identity.generation(),
                        identity.ordering_domain(),
                        identity.position().end(),
                    )?;
                    if let Some((file_identity, resume_fingerprint)) = resume_checkpoint {
                        next_cursor =
                            next_cursor.with_resume_checkpoint(file_identity, resume_fingerprint);
                    }
                    let projection_generation =
                        ProjectionGenerationId::new(SESSION_MESSAGE_PROJECTOR_VERSION)
                            .map_err(ObservationStoreError::RetrievalAnchorContract)?;
                    let ingested_at = observation_ingested_at();
                    let authorization = build_observation_resolution_authorization_v1(
                        &observation,
                        "observation-capture.v1",
                    )?;
                    let repository_provenance = repository_provenance.map_or_else(
                        crate::repository_provenance::PreparedRepositoryProvenanceV1::unavailable,
                        |context| {
                            context.capture_after_sanitization(
                                &observation,
                                &projection_generation,
                                ingested_at,
                                authorization.clone(),
                            )
                        },
                    );
                    let retrieval_anchor = build_observation_retrieval_anchor_v2(
                        &observation,
                        projection_generation.clone(),
                        ingested_at,
                        authorization,
                    )?;
                    let write = AnchoredObservationWrite::new(
                        ObservationWrite::new(*observation, expected_cursor, next_cursor)?,
                        retrieval_anchor,
                        projection_generation,
                    )?
                    .with_repository_provenance_attachment(
                        repository_provenance.availability().clone(),
                        repository_provenance.anchor().cloned(),
                    )?;
                    if cancellation.is_cancelled() {
                        return Err(ObservationApplicationError::Cancelled);
                    }
                    let outcome = self.store.persist_admitted_observation(write).await?;
                    if cancellation.is_cancelled() {
                        return Err(ObservationApplicationError::Cancelled);
                    }
                    let observation_id = outcome.receipt().observation().observation_id();
                    let stored = self.store.read_admitted_observation(observation_id).await?;
                    if cancellation.is_cancelled() {
                        return Err(ObservationApplicationError::Cancelled);
                    }
                    let projection_status = stored
                        .ok_or(ObservationApplicationError::PersistedObservationUnavailable)?
                        .projection_status();
                    Ok(CaptureObservationOutcome::Persisted {
                        outcome: Box::new(outcome),
                        projection_status,
                        sanitized_record: Box::new(sanitized_record),
                        findings,
                    })
                }
                ObservationSanitizationOutcomeV1::Rejected { receipt, findings } => {
                    Ok(CaptureObservationOutcome::Rejected { receipt, findings })
                }
                ObservationSanitizationOutcomeV1::Quarantined { receipt, findings } => {
                    Ok(CaptureObservationOutcome::Quarantined { receipt, findings })
                }
            }
        })
    }

    pub async fn capture_claude_observation(
        &self,
        request: CaptureClaudeObservationRequest,
    ) -> Result<CaptureClaudeObservationOutcome, ObservationApplicationError> {
        self.capture_observation(request).await
    }

    pub async fn get_observation(
        &self,
        request: GetObservationRequest,
    ) -> Result<ObservationPointRead, ObservationApplicationError> {
        let GetObservationRequest {
            observation_id,
            cancellation,
        } = request;
        if cancellation.is_cancelled() {
            return Err(ObservationApplicationError::Cancelled);
        }
        let observation = self
            .store
            .read_admitted_observation(&observation_id)
            .await
            .map_err(ObservationApplicationError::from)?;
        if cancellation.is_cancelled() {
            return Err(ObservationApplicationError::Cancelled);
        }
        Ok(ObservationPointRead {
            observation,
            coverage: ObservationReplayCoverage::Complete,
        })
    }

    pub async fn replay_observations(
        &self,
        request: ReplayObservationsRequest,
    ) -> Result<ObservationReplayPage, ObservationApplicationError> {
        let ReplayObservationsRequest {
            replay,
            cancellation,
        } = request;
        if cancellation.is_cancelled() {
            return Err(ObservationApplicationError::Cancelled);
        }
        let request = replay;
        let limit = request.limit();
        let lookahead = limit
            .checked_add(1)
            .and_then(|limit| ObservationReplayRequest::new(request.after_sequence(), limit).ok());
        let mut observations = self
            .store
            .replay_admitted_observations(lookahead.unwrap_or(request))
            .await?;
        if cancellation.is_cancelled() {
            return Err(ObservationApplicationError::Cancelled);
        }
        let mut has_more = observations.len() > limit;
        observations.truncate(limit);
        if !has_more && observations.len() == limit && lookahead.is_none() {
            let after_sequence = observations
                .last()
                .map_or(request.after_sequence(), StoredObservation::sequence);
            let probe = ObservationReplayRequest::new(after_sequence, 1)?;
            if cancellation.is_cancelled() {
                return Err(ObservationApplicationError::Cancelled);
            }
            has_more = !self
                .store
                .replay_admitted_observations(probe)
                .await?
                .is_empty();
            if cancellation.is_cancelled() {
                return Err(ObservationApplicationError::Cancelled);
            }
        }
        let next_after_sequence = if has_more {
            observations.last().map(StoredObservation::sequence)
        } else {
            None
        };
        Ok(ObservationReplayPage {
            observations,
            coverage: if has_more {
                ObservationReplayCoverage::Partial
            } else {
                ObservationReplayCoverage::Complete
            },
            has_more,
            next_after_sequence,
        })
    }
}

#[cfg(test)]
#[path = "observation_test.rs"]
mod tests;
