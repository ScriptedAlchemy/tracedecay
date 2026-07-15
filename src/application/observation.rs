use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;
use thiserror::Error;
use tracedecay_domain::{
    CanonicalObservationIdV1, ClaudeObservationIdentityMaterialV1, ClaudeSourceCursorV1,
    ObservationContractError, RetentionClass, SanitizationReceiptV1,
};
use tracedecay_store::observation::{CursorAdvanceOutcome, ObservationCursorAdvance};
use tracedecay_store::{
    ObservationPersistOutcome, ObservationProjectionStatus, ObservationReplayRequest,
    ObservationStore, ObservationStoreError, ObservationWrite, StoredObservation,
};

use crate::privacy::{
    ClaudeRecordSanitizerV1, ClaudeSanitizationOutcomeV1, ParsedClaudeRecordV1,
    PrivacySanitizerError, SanitizationFindingV1,
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
pub enum CaptureClaudeObservationRequestError {
    #[error("parsed Claude observation source range does not match observation identity")]
    SourceRangeMismatch,
}

/// One validated, bounded Claude frame ready for the mandatory privacy boundary.
pub struct CaptureClaudeObservationRequest {
    parsed_record: ParsedClaudeRecordV1,
    identity: ClaudeObservationIdentityMaterialV1,
    expected_cursor: Option<ClaudeSourceCursorV1>,
    retention_class: RetentionClass,
    cancellation: ObservationCancellation,
}

impl CaptureClaudeObservationRequest {
    pub fn new(
        parsed_record: ParsedClaudeRecordV1,
        identity: ClaudeObservationIdentityMaterialV1,
        expected_cursor: Option<ClaudeSourceCursorV1>,
        retention_class: RetentionClass,
        cancellation: ObservationCancellation,
    ) -> Result<Self, CaptureClaudeObservationRequestError> {
        let position = identity.position();
        if *parsed_record.source_range() != position {
            return Err(CaptureClaudeObservationRequestError::SourceRangeMismatch);
        }
        Ok(Self {
            parsed_record,
            identity,
            expected_cursor,
            retention_class,
            cancellation,
        })
    }
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
pub enum CaptureClaudeObservationOutcome {
    Persisted {
        outcome: ObservationPersistOutcome,
        projection_status: ObservationProjectionStatus,
        sanitized_record: Value,
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

impl CaptureClaudeObservationOutcome {
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
    #[error("Claude observation contract is invalid")]
    Contract(#[from] ObservationContractError),
    #[error("Claude observation sanitization failed")]
    Privacy(#[from] PrivacySanitizerError),
    #[error("Claude observation store operation failed")]
    Store(#[from] ObservationStoreError),
    #[error("persisted Claude observation is not readable from the authoritative store")]
    PersistedObservationUnavailable,
    #[error("Claude observation operation was cancelled")]
    Cancelled,
}

/// Application-owned composition of sanitizer and an already-authoritative store.
pub struct ObservationApplication<S> {
    store: S,
    sanitizer: ClaudeRecordSanitizerV1,
}

impl<S: ObservationStore> ObservationApplication<S> {
    pub fn new(store: S, sanitizer: ClaudeRecordSanitizerV1) -> Self {
        Self { store, sanitizer }
    }

    /// Advances a validated non-durable frame cursor without exposing the store.
    pub async fn advance_non_durable_source_cursor(
        &self,
        advance: ObservationCursorAdvance,
    ) -> Result<CursorAdvanceOutcome, ObservationApplicationError> {
        self.store
            .advance_source_cursor(advance)
            .await
            .map_err(ObservationApplicationError::from)
    }

    pub async fn capture_claude_observation(
        &self,
        request: CaptureClaudeObservationRequest,
    ) -> Result<CaptureClaudeObservationOutcome, ObservationApplicationError> {
        let CaptureClaudeObservationRequest {
            parsed_record,
            identity,
            expected_cursor,
            retention_class,
            cancellation,
        } = request;
        if cancellation.is_cancelled() {
            return Err(ObservationApplicationError::Cancelled);
        }
        let sanitized = self
            .sanitizer
            .sanitize_parsed(parsed_record, identity, retention_class)?;
        if cancellation.is_cancelled() {
            return Err(ObservationApplicationError::Cancelled);
        }
        match sanitized {
            ClaudeSanitizationOutcomeV1::Durable {
                observation,
                findings,
            } => {
                let identity = observation.identity();
                let next_cursor = ClaudeSourceCursorV1::new(
                    identity.source().clone(),
                    identity.scope().clone(),
                    identity.generation(),
                    identity.position().end(),
                )?;
                let write = ObservationWrite::new(observation, expected_cursor, next_cursor)?;
                if cancellation.is_cancelled() {
                    return Err(ObservationApplicationError::Cancelled);
                }
                let outcome = self.store.persist_observation(write).await?;
                if cancellation.is_cancelled() {
                    return Err(ObservationApplicationError::Cancelled);
                }
                let sanitized_record = outcome.receipt().observation().payload().clone();
                let observation_id = outcome.receipt().observation().observation_id();
                let stored = self.store.get_observation(observation_id).await?;
                if cancellation.is_cancelled() {
                    return Err(ObservationApplicationError::Cancelled);
                }
                let projection_status = stored
                    .ok_or(ObservationApplicationError::PersistedObservationUnavailable)?
                    .projection_status();
                Ok(CaptureClaudeObservationOutcome::Persisted {
                    outcome,
                    projection_status,
                    sanitized_record,
                    findings,
                })
            }
            ClaudeSanitizationOutcomeV1::Rejected { receipt, findings } => {
                Ok(CaptureClaudeObservationOutcome::Rejected { receipt, findings })
            }
            ClaudeSanitizationOutcomeV1::Quarantined { receipt, findings } => {
                Ok(CaptureClaudeObservationOutcome::Quarantined { receipt, findings })
            }
        }
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
            .get_observation(&observation_id)
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
            .replay_observations(lookahead.unwrap_or(request))
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
            has_more = !self.store.replay_observations(probe).await?.is_empty();
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
mod tests {
    use std::sync::Mutex;

    use serde_json::{Value, json};
    use tracedecay_domain::{
        ClaudeByteRangeV1, ClaudeFileGenerationV1, ClaudeSourceIdentityV1, ObservationScopeV1,
        ProjectId, SessionId,
    };
    use tracedecay_store::observation::{
        CursorAdvanceOutcome, NonDurableFrameReason, ObservationCursorAdvance,
    };
    use tracedecay_store::{ObservationCommitReceipt, ObservationStoreResult};

    use crate::privacy::{PR5_MAX_CLAUDE_RECORD_BYTES, parse_claude_record_v1};

    use super::*;

    #[derive(Default)]
    struct FakeStore {
        observations: Mutex<Vec<StoredObservation>>,
        source_cursors: Mutex<Vec<ClaudeSourceCursorV1>>,
        cancel_on_persist: Mutex<Option<ObservationCancellation>>,
        cancel_on_get: Mutex<Option<ObservationCancellation>>,
        cancel_on_replay: Mutex<Option<ObservationCancellation>>,
        cursor_advances: Mutex<Vec<ObservationCursorAdvance>>,
    }

    impl ObservationStore for FakeStore {
        async fn persist_observation(
            &self,
            write: ObservationWrite,
        ) -> ObservationStoreResult<ObservationPersistOutcome> {
            let mut observations = self.observations.lock().unwrap();
            if let Some(stored) = observations.iter().find(|stored| {
                stored.observation().observation_id() == write.observation().observation_id()
            }) {
                return Ok(ObservationPersistOutcome::ExactDuplicate(
                    ObservationCommitReceipt::new(
                        stored.sequence(),
                        stored.observation().clone(),
                        stored.committed_cursor().clone(),
                    ),
                ));
            }
            let sequence = u64::try_from(observations.len()).unwrap() + 1;
            let observation = write.observation().clone();
            let cursor = write.next_cursor().clone();
            let receipt =
                ObservationCommitReceipt::new(sequence, observation.clone(), cursor.clone());
            let mut cursors = self.source_cursors.lock().unwrap();
            cursors.retain(|existing| {
                existing.source() != cursor.source() || existing.scope() != cursor.scope()
            });
            cursors.push(cursor.clone());
            drop(cursors);
            observations.push(StoredObservation::new(
                sequence,
                observation,
                cursor,
                ObservationProjectionStatus::Queued,
            ));
            if let Some(cancellation) = self.cancel_on_persist.lock().unwrap().take() {
                cancellation.cancel();
            }
            Ok(ObservationPersistOutcome::Committed(receipt))
        }

        async fn get_source_cursor(
            &self,
            source: &ClaudeSourceIdentityV1,
            scope: &ObservationScopeV1,
        ) -> ObservationStoreResult<Option<ClaudeSourceCursorV1>> {
            Ok(self
                .source_cursors
                .lock()
                .unwrap()
                .iter()
                .find(|cursor| cursor.source() == source && cursor.scope() == scope)
                .cloned())
        }

        async fn advance_source_cursor(
            &self,
            advance: ObservationCursorAdvance,
        ) -> ObservationStoreResult<CursorAdvanceOutcome> {
            self.cursor_advances.lock().unwrap().push(advance.clone());
            let mut cursors = self.source_cursors.lock().unwrap();
            let position = cursors.iter().position(|cursor| {
                cursor.source() == advance.next_cursor().source()
                    && cursor.scope() == advance.next_cursor().scope()
            });
            let actual = position.map(|index| cursors[index].clone());
            if actual.as_ref() == Some(advance.next_cursor()) {
                return Ok(CursorAdvanceOutcome::ExactDuplicate);
            }
            if actual.as_ref() != advance.expected_cursor() {
                return Err(ObservationStoreError::CursorConflict {
                    expected: Box::new(advance.expected_cursor().cloned()),
                    actual: Box::new(actual),
                });
            }
            if let Some(index) = position {
                cursors[index] = advance.next_cursor().clone();
            } else {
                cursors.push(advance.next_cursor().clone());
            }
            Ok(CursorAdvanceOutcome::Committed)
        }

        async fn get_observation(
            &self,
            observation_id: &CanonicalObservationIdV1,
        ) -> ObservationStoreResult<Option<StoredObservation>> {
            let observation = self
                .observations
                .lock()
                .unwrap()
                .iter()
                .find(|stored| stored.observation().observation_id() == observation_id)
                .cloned();
            if let Some(cancellation) = self.cancel_on_get.lock().unwrap().take() {
                cancellation.cancel();
            }
            Ok(observation)
        }

        async fn replay_observations(
            &self,
            request: ObservationReplayRequest,
        ) -> ObservationStoreResult<Vec<StoredObservation>> {
            let observations = self
                .observations
                .lock()
                .unwrap()
                .iter()
                .filter(|stored| stored.sequence() > request.after_sequence())
                .take(request.limit())
                .cloned()
                .collect();
            if let Some(cancellation) = self.cancel_on_replay.lock().unwrap().take() {
                cancellation.cancel();
            }
            Ok(observations)
        }
    }

    fn request(record: &Value) -> CaptureClaudeObservationRequest {
        request_at(record, 0)
    }

    fn request_at(record: &Value, start: u64) -> CaptureClaudeObservationRequest {
        request_at_with_cancellation(record, start, ObservationCancellation::default())
    }

    fn request_at_with_cancellation(
        record: &Value,
        start: u64,
        cancellation: ObservationCancellation,
    ) -> CaptureClaudeObservationRequest {
        let encoded_frame = serde_json::to_vec(record).unwrap();
        let end = start + u64::try_from(encoded_frame.len()).unwrap();
        let parsed_record =
            parse_claude_record_v1(&encoded_frame, ClaudeByteRangeV1::new(start, end).unwrap())
                .unwrap();
        let source =
            ClaudeSourceIdentityV1::new(SessionId::new("session.application-test").unwrap())
                .unwrap();
        let scope = ObservationScopeV1::Project {
            project_id: ProjectId::new("project.application-test").unwrap(),
        };
        let identity = ClaudeObservationIdentityMaterialV1::new(
            source,
            scope,
            ClaudeFileGenerationV1::new(1).unwrap(),
            ClaudeByteRangeV1::new(start, end).unwrap(),
        )
        .unwrap();
        CaptureClaudeObservationRequest::new(
            parsed_record,
            identity,
            None,
            RetentionClass::new("retention.application-test").unwrap(),
            cancellation,
        )
        .unwrap()
    }

    fn application() -> ObservationApplication<FakeStore> {
        ObservationApplication::new(
            FakeStore::default(),
            ClaudeRecordSanitizerV1::pr5().unwrap(),
        )
    }

    #[tokio::test]
    async fn non_durable_cursor_advance_stays_inside_application_boundary() {
        let application = application();
        let source =
            ClaudeSourceIdentityV1::new(SessionId::new("session.cursor-advance").unwrap()).unwrap();
        let advance = ObservationCursorAdvance::new(
            source,
            ObservationScopeV1::Profile,
            ClaudeFileGenerationV1::new(1).unwrap(),
            None,
            ClaudeByteRangeV1::new(0, 4).unwrap(),
            NonDurableFrameReason::BlankFrame,
        )
        .unwrap();

        let outcome = application
            .advance_non_durable_source_cursor(advance)
            .await
            .unwrap();

        assert_eq!(outcome, CursorAdvanceOutcome::Committed);
        let advances = application.store.cursor_advances.lock().unwrap();
        assert_eq!(advances.len(), 1);
        assert_eq!(advances[0].covered(), ClaudeByteRangeV1::new(0, 4).unwrap());
        assert_eq!(advances[0].reason(), NonDurableFrameReason::BlankFrame);
    }

    #[tokio::test]
    async fn capture_redacts_before_the_store_and_replays_the_receipt_bound_row() {
        let application = application();
        let secret = "sk-proj-application-secret-1234567890";
        let outcome = application
            .capture_claude_observation(request(&json!({
                "type": "user",
                "message": { "role": "user", "content": "hello" },
                "api_key": secret
            })))
            .await
            .unwrap();
        let sanitized_record = match &outcome {
            CaptureClaudeObservationOutcome::Persisted {
                sanitized_record, ..
            } => sanitized_record,
            other => panic!("capture must persist, got {other:?}"),
        };
        assert!(!sanitized_record.to_string().contains(secret));
        assert!(matches!(
            outcome,
            CaptureClaudeObservationOutcome::Persisted { .. }
        ));
        let page = application
            .replay_observations(ReplayObservationsRequest::new(
                ObservationReplayRequest::new(0, 10).unwrap(),
                ObservationCancellation::default(),
            ))
            .await
            .unwrap();
        assert_eq!(page.coverage(), ObservationReplayCoverage::Complete);
        assert!(!page.has_more());
        assert_eq!(page.next_after_sequence(), None);
        assert_eq!(page.observations().len(), 1);
        let payload = page.observations()[0].observation().payload().to_string();
        assert!(!payload.contains(secret));
        assert!(payload.contains("TraceDecay redacted"));
    }

    #[test]
    fn structurally_rejected_frames_never_reach_the_store() {
        let application = application();
        let raw = serde_json::to_vec(&json!("not an object")).unwrap();
        let range = ClaudeByteRangeV1::new(0, u64::try_from(raw.len()).unwrap()).unwrap();
        assert!(parse_claude_record_v1(&raw, range).is_err());
        assert!(application.store.observations.lock().unwrap().is_empty());
    }

    #[test]
    fn request_accepts_only_bounded_parser_evidence_for_the_identity_range() {
        let identity = |start, end| {
            ClaudeObservationIdentityMaterialV1::new(
                ClaudeSourceIdentityV1::new(SessionId::new("session.frame-test").unwrap()).unwrap(),
                ObservationScopeV1::Profile,
                ClaudeFileGenerationV1::new(1).unwrap(),
                ClaudeByteRangeV1::new(start, end).unwrap(),
            )
            .unwrap()
        };
        let retention = || RetentionClass::new("retention.application-test").unwrap();

        assert!(parse_claude_record_v1(&[], ClaudeByteRangeV1::new(0, 1).unwrap()).is_err());
        let oversized = vec![b'x'; PR5_MAX_CLAUDE_RECORD_BYTES + 1];
        let oversized_end = u64::try_from(oversized.len()).unwrap();
        assert!(
            parse_claude_record_v1(
                &oversized,
                ClaudeByteRangeV1::new(0, oversized_end).unwrap()
            )
            .is_err()
        );

        let raw = b"{}";
        let parsed = parse_claude_record_v1(
            raw,
            ClaudeByteRangeV1::new(10, 10 + u64::try_from(raw.len()).unwrap()).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            CaptureClaudeObservationRequest::new(
                parsed,
                identity(0, u64::try_from(raw.len()).unwrap()),
                None,
                retention(),
                ObservationCancellation::default(),
            ),
            Err(CaptureClaudeObservationRequestError::SourceRangeMismatch)
        ));
    }

    #[tokio::test]
    async fn exact_duplicate_reports_authoritative_projection_status() {
        let application = application();
        let record = json!({
            "type": "user",
            "message": { "role": "user", "content": "duplicate" }
        });
        let first = application
            .capture_claude_observation(request(&record))
            .await
            .unwrap();
        let first_sanitized_record = match first {
            CaptureClaudeObservationOutcome::Persisted {
                sanitized_record, ..
            } => sanitized_record,
            other => panic!("first capture must persist, got {other:?}"),
        };
        {
            let mut observations = application.store.observations.lock().unwrap();
            let stored = observations[0].clone();
            observations[0] = StoredObservation::new(
                stored.sequence(),
                stored.observation().clone(),
                stored.committed_cursor().clone(),
                ObservationProjectionStatus::NotQueued,
            );
        }

        let duplicate = application
            .capture_claude_observation(request(&record))
            .await
            .unwrap();
        match duplicate {
            CaptureClaudeObservationOutcome::Persisted {
                outcome,
                projection_status,
                sanitized_record,
                ..
            } => {
                assert!(matches!(
                    outcome,
                    ObservationPersistOutcome::ExactDuplicate(_)
                ));
                assert_eq!(projection_status, ObservationProjectionStatus::NotQueued);
                assert_eq!(sanitized_record, first_sanitized_record);
            }
            other => panic!("duplicate must persist, got {other:?}"),
        }
        assert_eq!(application.store.observations.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn replay_reports_partial_coverage_and_a_truthful_continuation() {
        let application = application();
        for (index, start) in [0, 100, 200].into_iter().enumerate() {
            application
                .capture_claude_observation(request_at(
                    &json!({
                        "type": "user",
                        "message": {
                            "role": "user",
                            "content": format!("message {index}")
                        }
                    }),
                    start,
                ))
                .await
                .unwrap();
        }

        let first = application
            .replay_observations(ReplayObservationsRequest::new(
                ObservationReplayRequest::new(0, 2).unwrap(),
                ObservationCancellation::default(),
            ))
            .await
            .unwrap();
        assert_eq!(first.coverage(), ObservationReplayCoverage::Partial);
        assert!(first.has_more());
        assert_eq!(first.next_after_sequence(), Some(2));
        assert_eq!(first.observations().len(), 2);

        let second = application
            .replay_observations(ReplayObservationsRequest::new(
                ObservationReplayRequest::new(first.next_after_sequence().unwrap(), 2).unwrap(),
                ObservationCancellation::default(),
            ))
            .await
            .unwrap();
        assert_eq!(second.coverage(), ObservationReplayCoverage::Complete);
        assert!(!second.has_more());
        assert_eq!(second.next_after_sequence(), None);
        assert_eq!(second.observations().len(), 1);
    }

    #[tokio::test]
    async fn replay_at_the_store_limit_uses_a_bounded_probe_for_coverage() {
        let application = application();
        application
            .capture_claude_observation(request(&json!({
                "type": "user",
                "message": { "role": "user", "content": "replay seed" }
            })))
            .await
            .unwrap();
        {
            let mut observations = application.store.observations.lock().unwrap();
            let seed = observations[0].clone();
            *observations = (1..=1_001)
                .map(|sequence| {
                    StoredObservation::new(
                        sequence,
                        seed.observation().clone(),
                        seed.committed_cursor().clone(),
                        seed.projection_status(),
                    )
                })
                .collect();
        }

        let page = application
            .replay_observations(ReplayObservationsRequest::new(
                ObservationReplayRequest::new(0, 1_000).unwrap(),
                ObservationCancellation::default(),
            ))
            .await
            .unwrap();
        assert_eq!(page.coverage(), ObservationReplayCoverage::Partial);
        assert!(page.has_more());
        assert_eq!(page.next_after_sequence(), Some(1_000));
        assert_eq!(page.observations().len(), 1_000);
    }

    #[tokio::test]
    async fn pre_cancelled_capture_never_reaches_the_store() {
        let application = application();
        let cancellation = ObservationCancellation::default();
        cancellation.cancel();
        let result = application
            .capture_claude_observation(request_at_with_cancellation(
                &json!({
                    "type": "user",
                    "message": { "role": "user", "content": "cancelled" }
                }),
                0,
                cancellation,
            ))
            .await;

        assert!(matches!(
            result,
            Err(ObservationApplicationError::Cancelled)
        ));
        assert!(application.store.observations.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn cancellation_during_atomic_commit_is_reported_after_commit_and_retry_is_exact() {
        let application = application();
        let cancellation = ObservationCancellation::default();
        *application.store.cancel_on_persist.lock().unwrap() = Some(cancellation.clone());
        let record = json!({
            "type": "user",
            "message": { "role": "user", "content": "commit before acknowledgement" }
        });

        let first = application
            .capture_claude_observation(request_at_with_cancellation(&record, 0, cancellation))
            .await;
        assert!(matches!(first, Err(ObservationApplicationError::Cancelled)));
        assert_eq!(application.store.observations.lock().unwrap().len(), 1);

        let retry = application
            .capture_claude_observation(request(&record))
            .await
            .unwrap();
        let CaptureClaudeObservationOutcome::Persisted { outcome, .. } = retry else {
            panic!("retry must persist");
        };
        assert!(matches!(
            outcome,
            ObservationPersistOutcome::ExactDuplicate(_)
        ));
        assert_eq!(application.store.observations.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn cancellation_after_point_read_and_replay_discards_non_atomic_results() {
        let application = application();
        let capture = application
            .capture_claude_observation(request(&json!({
                "type": "user",
                "message": { "role": "user", "content": "read cancellation" }
            })))
            .await
            .unwrap();
        let observation_id = match capture {
            CaptureClaudeObservationOutcome::Persisted { outcome, .. } => {
                outcome.receipt().observation().observation_id().clone()
            }
            other => panic!("capture must persist, got {other:?}"),
        };

        let read_cancellation = ObservationCancellation::default();
        *application.store.cancel_on_get.lock().unwrap() = Some(read_cancellation.clone());
        let read = application
            .get_observation(GetObservationRequest::new(
                observation_id,
                read_cancellation,
            ))
            .await;
        assert!(matches!(read, Err(ObservationApplicationError::Cancelled)));

        let replay_cancellation = ObservationCancellation::default();
        *application.store.cancel_on_replay.lock().unwrap() = Some(replay_cancellation.clone());
        let replay = application
            .replay_observations(ReplayObservationsRequest::new(
                ObservationReplayRequest::new(0, 10).unwrap(),
                replay_cancellation,
            ))
            .await;
        assert!(matches!(
            replay,
            Err(ObservationApplicationError::Cancelled)
        ));
    }
}
