use std::future::Future;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use thiserror::Error;
use tokio::task::JoinSet;
use tracedecay_application::clock::now_micros;
use tracedecay_domain::{
    CanonicalObservationIdV1, ManifestDigest, ObservationContractError,
    ObservationIdentityMaterialV1, ObservationSourceCursorV1, ProjectionGenerationId,
    RetentionClass, SanitizationReceiptV1, SourceBindingIdentityV1,
};
use tracedecay_store::observation::{
    CursorAdvanceOutcome, ObservationCursorAdvance, ObservationIdentityCollisionDispositionV1,
};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationAdmissionPort, ObservationCaptureSink,
    ObservationCursorPort, ObservationPersistOutcome, ObservationProjectionStatus,
    ObservationReplayRequest, ObservationStore, ObservationStoreError, ObservationWrite,
    SESSION_MESSAGE_PROJECTOR_VERSION, StoredObservation,
    build_observation_resolution_authorization_v1, build_observation_retrieval_anchor_v2,
};

use crate::repository_provenance::RepositoryProvenanceAdmissionContext;
use tracedecay_private_fs::background_cpu::ProcessBackgroundCpuV1;
use tracedecay_runtime_core::privacy::{
    ObservationSanitizationOutcomeV1, ParsedObservationRecordV1, PrivacySanitizerError,
    RecordSanitizerV1, SanitizationFindingV1, SanitizedObservationRecordV1,
};

/// Cloneable, operation-local cancellation shared by application adapters.
#[derive(Clone, Debug, Default)]
pub struct ObservationCancellation {
    cancelled: Arc<AtomicBool>,
    notify: Arc<tokio::sync::Notify>,
}

impl ObservationCancellation {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Wait until this exact operation is cancelled without losing a signal
    /// between the readiness check and waiter registration.
    #[hotpath::skip]
    pub(crate) async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let notified = self.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }

    pub(crate) fn cancellation_flag(&self) -> &AtomicBool {
        self.cancelled.as_ref()
    }

    /// Carries this exact operation cancellation into verified graph
    /// publication, whose runtime contract settles cancellation around its
    /// durable head-CAS commit point.
    pub(crate) fn verified_graph_cancellation(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
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
    identity_collision_disposition: ObservationIdentityCollisionDispositionV1,
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
            identity_collision_disposition:
                ObservationIdentityCollisionDispositionV1::SettleTerminal,
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

    #[must_use]
    pub fn with_identity_collision_disposition(
        mut self,
        disposition: ObservationIdentityCollisionDispositionV1,
    ) -> Self {
        self.identity_collision_disposition = disposition;
        self
    }

    pub fn identity_collision_disposition(&self) -> ObservationIdentityCollisionDispositionV1 {
        self.identity_collision_disposition
    }
}

pub type CaptureClaudeObservationRequest = CaptureObservationRequest;
pub type CaptureClaudeObservationRequestError = CaptureObservationRequestError;

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

/// What the authoritative point read established about projection work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservationProjectionReadback {
    Authoritative(ObservationProjectionStatus),
    Unavailable,
}

/// Result of mandatory sanitization and, when permitted, authoritative persistence.
#[derive(Debug)]
pub enum CaptureObservationOutcome {
    Persisted {
        outcome: Box<ObservationPersistOutcome>,
        projection_status: ObservationProjectionReadback,
        sanitized_record: Box<SanitizedObservationRecordV1>,
        findings: Vec<SanitizationFindingV1>,
    },
    AcceptedForReplay {
        durable_observation_id: CanonicalObservationIdV1,
        projection_state: ExternalSourceProjectionStateV1,
        retry_handle: ExternalSourceProjectionRetryHandleV1,
        outcome: Box<ObservationPersistOutcome>,
        projection_status: ObservationProjectionReadback,
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
            Self::Persisted { outcome, .. } | Self::AcceptedForReplay { outcome, .. } => {
                outcome.receipt().sanitization_receipt()
            }
            Self::Rejected { receipt, .. } | Self::Quarantined { receipt, .. } => receipt,
        }
    }

    pub fn findings(&self) -> &[SanitizationFindingV1] {
        match self {
            Self::Persisted { findings, .. }
            | Self::AcceptedForReplay { findings, .. }
            | Self::Rejected { findings, .. }
            | Self::Quarantined { findings, .. } => findings,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalSourceProjectionStateV1 {
    Pending,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalSourceProjectionRetryHandleV1 {
    binding: SourceBindingIdentityV1,
    source_receipt_digest: ManifestDigest,
}

impl ExternalSourceProjectionRetryHandleV1 {
    pub fn new(binding: SourceBindingIdentityV1, source_receipt_digest: ManifestDigest) -> Self {
        Self {
            binding,
            source_receipt_digest,
        }
    }

    pub fn binding(&self) -> &SourceBindingIdentityV1 {
        &self.binding
    }

    pub fn source_receipt_digest(&self) -> &ManifestDigest {
        &self.source_receipt_digest
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
    #[error("observation operation was cancelled")]
    Cancelled,
    #[error("observation batch contains a non-durable privacy outcome")]
    BatchContainsNonDurable,
    #[error("observation batch worker stopped before completing")]
    BatchWorkerStopped,
}

enum PreparedObservationCapture {
    Durable {
        // Boxed: this field dominates the enum (the variant was ~2.5 KiB
        // against ~112 bytes for the rejection variants), so every Rejected and
        // Quarantined value paid for it.
        write: Box<AnchoredObservationWrite>,
        sanitized_record: SanitizedObservationRecordV1,
        findings: Vec<SanitizationFindingV1>,
        cancellation: ObservationCancellation,
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

struct DurableObservationCapture {
    sanitized_record: SanitizedObservationRecordV1,
    findings: Vec<SanitizationFindingV1>,
    cancellation: ObservationCancellation,
}

struct PersistedObservationCapture {
    outcome: ObservationPersistOutcome,
    stored: Option<StoredObservation>,
    durable: DurableObservationCapture,
}

/// Bounds independent batch preparation.
///
/// The daemon owns the actual width selection. Keeping the bound here makes
/// the application boundary deterministic while allowing composition to pass
/// the shared, memory-aware daemon plan without creating another CPU pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ObservationBatchConcurrency(NonZeroUsize);

impl ObservationBatchConcurrency {
    #[hotpath::skip]
    pub const fn new(max_in_flight: NonZeroUsize) -> Self {
        Self(max_in_flight)
    }

    #[hotpath::skip]
    const fn max_in_flight(self) -> usize {
        self.0.get()
    }
}

impl Default for ObservationBatchConcurrency {
    fn default() -> Self {
        Self(NonZeroUsize::MIN)
    }
}

/// Application-owned composition of sanitizer and an already-authoritative store.
pub struct ObservationApplication<S> {
    store: S,
    sanitizer: RecordSanitizerV1,
    batch_concurrency: ObservationBatchConcurrency,
    background_cpu: Option<Arc<ProcessBackgroundCpuV1>>,
}

impl<S> ObservationApplication<S>
where
    S: ObservationCaptureSink + ObservationCursorPort + ObservationAdmissionPort,
{
    pub fn new(store: S, sanitizer: RecordSanitizerV1) -> Self {
        Self {
            store,
            sanitizer,
            batch_concurrency: ObservationBatchConcurrency::default(),
            background_cpu: None,
        }
    }

    /// Applies a narrow injected bound in projectless or test composition.
    ///
    /// Production composition must use [`Self::with_background_cpu`] so active
    /// preparation participates in the process-wide CPU authority.
    #[must_use]
    pub fn with_batch_concurrency(mut self, batch_concurrency: NonZeroUsize) -> Self {
        self.batch_concurrency = ObservationBatchConcurrency::new(batch_concurrency);
        self
    }

    /// Mounts the daemon's canonical background-CPU authority for independent
    /// preparation. Cursor-dependent persistence remains ordered in the one
    /// store-owned batch transaction.
    #[must_use]
    pub fn with_background_cpu(mut self, background_cpu: Arc<ProcessBackgroundCpuV1>) -> Self {
        self.batch_concurrency = ObservationBatchConcurrency::new(background_cpu.width());
        self.background_cpu = Some(background_cpu);
        self
    }

    /// Advances a validated non-durable frame cursor without exposing the store.
    #[hotpath::measure(label = "sessions.observation.advance_cursor", future = true)]
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

    fn prepare_capture(
        &self,
        request: CaptureObservationRequest,
    ) -> Result<PreparedObservationCapture, ObservationApplicationError> {
        Self::prepare_capture_with_sanitizer(&self.sanitizer, request)
    }

    fn prepare_capture_with_sanitizer(
        sanitizer: &RecordSanitizerV1,
        request: CaptureObservationRequest,
    ) -> Result<PreparedObservationCapture, ObservationApplicationError> {
        #[cfg(test)]
        tests::observe_capture_preparation(&request);
        let CaptureObservationRequest {
            parsed_record,
            identity,
            expected_cursor,
            resume_checkpoint,
            retention_class,
            cancellation,
            repository_provenance,
            identity_collision_disposition,
        } = request;
        if cancellation.is_cancelled() {
            return Err(ObservationApplicationError::Cancelled);
        }
        let sanitized = sanitizer.sanitize_parsed(parsed_record, identity, retention_class)?;
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
                let ingested_at = now_micros();
                let authorization = build_observation_resolution_authorization_v1(
                    &observation,
                    tracedecay_store::OBSERVATION_CAPTURE_AUTHORITY_V1,
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
                .with_identity_collision_disposition(identity_collision_disposition)
                .with_repository_provenance_attachment(
                    repository_provenance.availability().clone(),
                    repository_provenance.anchor().cloned(),
                )?;
                if cancellation.is_cancelled() {
                    return Err(ObservationApplicationError::Cancelled);
                }
                Ok(PreparedObservationCapture::Durable {
                    write: Box::new(write),
                    sanitized_record,
                    findings,
                    cancellation,
                })
            }
            ObservationSanitizationOutcomeV1::Rejected { receipt, findings } => {
                Ok(PreparedObservationCapture::Rejected { receipt, findings })
            }
            ObservationSanitizationOutcomeV1::Quarantined { receipt, findings } => {
                Ok(PreparedObservationCapture::Quarantined { receipt, findings })
            }
        }
    }

    #[hotpath::measure(label = "sessions.observation.readback", future = true)]
    async fn persisted_outcome(
        &self,
        outcome: ObservationPersistOutcome,
        sanitized_record: SanitizedObservationRecordV1,
        findings: Vec<SanitizationFindingV1>,
        cancellation: &ObservationCancellation,
    ) -> Result<CaptureObservationOutcome, ObservationApplicationError> {
        if cancellation.is_cancelled() {
            return Err(ObservationApplicationError::Cancelled);
        }
        let observation_id = outcome.receipt().observation().observation_id();
        let stored = self.store.read_admitted_observation(observation_id).await?;
        if cancellation.is_cancelled() {
            return Err(ObservationApplicationError::Cancelled);
        }
        Ok(Self::persisted_outcome_from_readback(
            outcome,
            sanitized_record,
            findings,
            stored,
        ))
    }

    fn persisted_outcome_from_readback(
        outcome: ObservationPersistOutcome,
        sanitized_record: SanitizedObservationRecordV1,
        findings: Vec<SanitizationFindingV1>,
        stored: Option<StoredObservation>,
    ) -> CaptureObservationOutcome {
        // Preserve authoritative projection state when visible. A
        // new commit establishes queued state. Duplicate receipts
        // prove durability but carry no projection status, so a
        // trailing reader snapshot remains explicitly unavailable.
        let projection_status = match (stored, &outcome) {
            (Some(stored), _) => {
                ObservationProjectionReadback::Authoritative(stored.projection_status())
            }
            (None, ObservationPersistOutcome::Committed(_)) => {
                ObservationProjectionReadback::Authoritative(ObservationProjectionStatus::Queued)
            }
            (
                None,
                ObservationPersistOutcome::ExactDuplicate(_)
                | ObservationPersistOutcome::CoveredDuplicate(_),
            ) => ObservationProjectionReadback::Unavailable,
        };
        CaptureObservationOutcome::Persisted {
            outcome: Box::new(outcome),
            projection_status,
            sanitized_record: Box::new(sanitized_record),
            findings,
        }
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
        Box::pin(hotpath::future!(
            async move {
                match self.prepare_capture(request)? {
                    PreparedObservationCapture::Durable {
                        write,
                        sanitized_record,
                        findings,
                        cancellation,
                    } => {
                        let outcome = self.store.persist_admitted_observation(*write).await?;
                        self.persisted_outcome(outcome, sanitized_record, findings, &cancellation)
                            .await
                    }
                    PreparedObservationCapture::Rejected { receipt, findings } => {
                        Ok(CaptureObservationOutcome::Rejected { receipt, findings })
                    }
                    PreparedObservationCapture::Quarantined { receipt, findings } => {
                        Ok(CaptureObservationOutcome::Quarantined { receipt, findings })
                    }
                }
            },
            label = "sessions.observation.capture"
        ))
    }

    #[hotpath::skip]
    pub async fn capture_claude_observation(
        &self,
        request: CaptureClaudeObservationRequest,
    ) -> Result<CaptureClaudeObservationOutcome, ObservationApplicationError> {
        self.capture_observation(request).await
    }

    #[hotpath::measure(label = "sessions.observation.get", future = true)]
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

    #[hotpath::measure(label = "sessions.observation.replay", future = true)]
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

impl<S> ObservationApplication<S>
where
    S: ObservationStore + ObservationCaptureSink + ObservationCursorPort + ObservationAdmissionPort,
{
    #[hotpath::measure(label = "sessions.observation.prepare_batch", future = true)]
    async fn prepare_batch_captures(
        &self,
        requests: Vec<CaptureObservationRequest>,
    ) -> Result<Vec<PreparedObservationCapture>, ObservationApplicationError> {
        let total = requests.len();
        let mut pending = requests.into_iter().enumerate();
        let limit = self.batch_concurrency.max_in_flight().min(total);
        let mut tasks = JoinSet::new();
        let mut prepared = (0..total).map(|_| None).collect::<Vec<_>>();

        loop {
            while tasks.len() < limit {
                let Some((index, request)) = pending.next() else {
                    break;
                };
                if request.cancellation.is_cancelled() {
                    tasks.abort_all();
                    return Err(ObservationApplicationError::Cancelled);
                }
                let sanitizer = self.sanitizer.clone();
                let background_cpu = self.background_cpu.clone();
                tasks.spawn_blocking(move || {
                    let capture = match background_cpu {
                        Some(authority) => authority.with_permit(|| {
                            Self::prepare_capture_with_sanitizer(&sanitizer, request)
                        }),
                        None => Self::prepare_capture_with_sanitizer(&sanitizer, request),
                    };
                    (index, capture)
                });
            }

            let Some(joined) = tasks.join_next().await else {
                break;
            };
            let (index, capture) =
                joined.map_err(|_| ObservationApplicationError::BatchWorkerStopped)?;
            prepared[index] = Some(capture?);
        }

        prepared
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or(ObservationApplicationError::BatchWorkerStopped)
    }

    fn persisted_batch_outcomes(
        persisted: Vec<PersistedObservationCapture>,
    ) -> Result<Vec<CaptureObservationOutcome>, ObservationApplicationError> {
        let mut outcomes = Vec::with_capacity(persisted.len());
        for persisted_capture in persisted {
            if persisted_capture.durable.cancellation.is_cancelled() {
                return Err(ObservationApplicationError::Cancelled);
            }
            outcomes.push(Self::persisted_outcome_from_readback(
                persisted_capture.outcome,
                persisted_capture.durable.sanitized_record,
                persisted_capture.durable.findings,
                persisted_capture.stored,
            ));
        }
        Ok(outcomes)
    }

    /// Sanitizes every request, then persists durable writes through one
    /// store-owned `persist_observations` call. Empty input returns empty
    /// without touching persist authority. A sanitizer reject or quarantine
    /// in the batch refuses before persistence so the stream owner can retry
    /// one request at a time and advance typed coverage between records.
    #[hotpath::measure(label = "sessions.observation.capture_batch", future = true)]
    pub async fn capture_observations(
        &self,
        requests: Vec<CaptureObservationRequest>,
    ) -> Result<Vec<CaptureObservationOutcome>, ObservationApplicationError> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        let prepared = self.prepare_batch_captures(requests).await?;
        if prepared.iter().any(|capture| {
            matches!(
                capture,
                PreparedObservationCapture::Durable { cancellation, .. }
                    if cancellation.is_cancelled()
            )
        }) {
            return Err(ObservationApplicationError::Cancelled);
        }
        let all_durable = prepared
            .iter()
            .all(|prepared| matches!(prepared, PreparedObservationCapture::Durable { .. }));
        if !all_durable {
            return Err(ObservationApplicationError::BatchContainsNonDurable);
        }
        let mut writes = Vec::with_capacity(prepared.len());
        let mut durable = Vec::with_capacity(prepared.len());
        for prepared in prepared {
            match prepared {
                PreparedObservationCapture::Durable {
                    write,
                    sanitized_record,
                    findings,
                    cancellation,
                } => {
                    writes.push(*write);
                    durable.push(DurableObservationCapture {
                        sanitized_record,
                        findings,
                        cancellation,
                    });
                }
                PreparedObservationCapture::Rejected { .. }
                | PreparedObservationCapture::Quarantined { .. } => {
                    return Err(ObservationApplicationError::BatchContainsNonDurable);
                }
            }
        }
        let persist_outcomes = self.store.persist_observations(writes).await?;
        if persist_outcomes.len() != durable.len() {
            return Err(ObservationApplicationError::Store(
                ObservationStoreError::Storage {
                    operation: "persist_observations",
                    source: Box::new(std::io::Error::other(
                        "persist_observations returned a different outcome count",
                    )),
                },
            ));
        }
        let persisted = durable
            .into_iter()
            .zip(persist_outcomes)
            .map(|(durable, persisted)| {
                let (outcome, stored) = persisted.into_parts();
                PersistedObservationCapture {
                    outcome,
                    stored,
                    durable,
                }
            })
            .collect();
        Self::persisted_batch_outcomes(persisted)
    }
}

#[cfg(test)]
#[path = "observation_test.rs"]
mod tests;
