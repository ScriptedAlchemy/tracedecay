use serde_json::Value;
use thiserror::Error;
use tracedecay_domain::{
    CanonicalObservationIdV1, ClaudeObservationIdentityMaterialV1, ClaudeSourceCursorV1,
    ObservationContractError, RetentionClass, SanitizationReceiptV1,
};
use tracedecay_store::{
    ObservationPersistOutcome, ObservationProjectionStatus, ObservationReplayRequest,
    ObservationStore, ObservationStoreError, ObservationWrite, StoredObservation,
};

use crate::privacy::{
    ClaudeRecordSanitizerV1, ClaudeSanitizationOutcomeV1, PrivacySanitizerError,
    SanitizationFindingV1,
};

/// One bounded, parsed Claude frame ready for the mandatory privacy boundary.
pub struct CaptureClaudeObservationRequest {
    record: Value,
    identity: ClaudeObservationIdentityMaterialV1,
    expected_cursor: Option<ClaudeSourceCursorV1>,
    retention_class: RetentionClass,
}

impl CaptureClaudeObservationRequest {
    pub fn new(
        record: Value,
        identity: ClaudeObservationIdentityMaterialV1,
        expected_cursor: Option<ClaudeSourceCursorV1>,
        retention_class: RetentionClass,
    ) -> Self {
        Self {
            record,
            identity,
            expected_cursor,
            retention_class,
        }
    }
}

/// Result of mandatory sanitization and, when permitted, authoritative persistence.
#[derive(Debug)]
pub enum CaptureClaudeObservationOutcome {
    Persisted {
        outcome: ObservationPersistOutcome,
        projection_status: ObservationProjectionStatus,
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
}

#[derive(Debug)]
pub struct ObservationReplayPage {
    observations: Vec<StoredObservation>,
    coverage: ObservationReplayCoverage,
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
}

#[derive(Debug, Error)]
pub enum ObservationApplicationError {
    #[error("Claude observation frame could not be encoded")]
    EncodeFrame(#[source] serde_json::Error),
    #[error("Claude observation contract is invalid")]
    Contract(#[from] ObservationContractError),
    #[error("Claude observation sanitization failed")]
    Privacy(#[from] PrivacySanitizerError),
    #[error("Claude observation store operation failed")]
    Store(#[from] ObservationStoreError),
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

    pub fn store(&self) -> &S {
        &self.store
    }

    pub async fn capture_claude_observation(
        &self,
        request: CaptureClaudeObservationRequest,
    ) -> Result<CaptureClaudeObservationOutcome, ObservationApplicationError> {
        let CaptureClaudeObservationRequest {
            record,
            identity,
            expected_cursor,
            retention_class,
        } = request;
        let encoded =
            serde_json::to_vec(&record).map_err(ObservationApplicationError::EncodeFrame)?;
        match self
            .sanitizer
            .sanitize(&encoded, identity, retention_class)?
        {
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
                let outcome = self.store.persist_observation(write).await?;
                Ok(CaptureClaudeObservationOutcome::Persisted {
                    outcome,
                    projection_status: ObservationProjectionStatus::Queued,
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
        observation_id: &CanonicalObservationIdV1,
    ) -> Result<ObservationPointRead, ObservationApplicationError> {
        let observation = self
            .store
            .get_observation(observation_id)
            .await
            .map_err(ObservationApplicationError::from)?;
        Ok(ObservationPointRead {
            observation,
            coverage: ObservationReplayCoverage::Complete,
        })
    }

    pub async fn replay_observations(
        &self,
        request: ObservationReplayRequest,
    ) -> Result<ObservationReplayPage, ObservationApplicationError> {
        let observations = self.store.replay_observations(request).await?;
        Ok(ObservationReplayPage {
            observations,
            coverage: ObservationReplayCoverage::Complete,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;
    use tracedecay_domain::{
        ClaudeByteRangeV1, ClaudeFileGenerationV1, ClaudeSourceIdentityV1, ObservationScopeV1,
        ProjectId, SessionId,
    };
    use tracedecay_store::{ObservationCommitReceipt, ObservationStoreResult};

    use super::*;

    #[derive(Default)]
    struct FakeStore {
        observations: Mutex<Vec<StoredObservation>>,
    }

    impl ObservationStore for FakeStore {
        async fn persist_observation(
            &self,
            write: ObservationWrite,
        ) -> ObservationStoreResult<ObservationPersistOutcome> {
            let sequence = 1;
            let observation = write.observation().clone();
            let cursor = write.next_cursor().clone();
            let receipt =
                ObservationCommitReceipt::new(sequence, observation.clone(), cursor.clone());
            self.observations
                .lock()
                .unwrap()
                .push(StoredObservation::new(
                    sequence,
                    observation,
                    cursor,
                    ObservationProjectionStatus::Queued,
                ));
            Ok(ObservationPersistOutcome::Committed(receipt))
        }

        async fn get_source_cursor(
            &self,
            _source: &ClaudeSourceIdentityV1,
            _scope: &ObservationScopeV1,
        ) -> ObservationStoreResult<Option<ClaudeSourceCursorV1>> {
            Ok(None)
        }

        async fn get_observation(
            &self,
            observation_id: &CanonicalObservationIdV1,
        ) -> ObservationStoreResult<Option<StoredObservation>> {
            Ok(self
                .observations
                .lock()
                .unwrap()
                .iter()
                .find(|stored| stored.observation().observation_id() == observation_id)
                .cloned())
        }

        async fn replay_observations(
            &self,
            _request: ObservationReplayRequest,
        ) -> ObservationStoreResult<Vec<StoredObservation>> {
            Ok(self.observations.lock().unwrap().clone())
        }
    }

    fn request(record: Value) -> CaptureClaudeObservationRequest {
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
            ClaudeByteRangeV1::new(0, 100).unwrap(),
        )
        .unwrap();
        CaptureClaudeObservationRequest::new(
            record,
            identity,
            None,
            RetentionClass::new("retention.application-test").unwrap(),
        )
    }

    fn application() -> ObservationApplication<FakeStore> {
        ObservationApplication::new(
            FakeStore::default(),
            ClaudeRecordSanitizerV1::pr5().unwrap(),
        )
    }

    #[tokio::test]
    async fn capture_redacts_before_the_store_and_replays_the_receipt_bound_row() {
        let application = application();
        let secret = "sk-proj-application-secret-1234567890";
        let outcome = application
            .capture_claude_observation(request(json!({
                "type": "user",
                "message": { "role": "user", "content": "hello" },
                "api_key": secret
            })))
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            CaptureClaudeObservationOutcome::Persisted { .. }
        ));
        let page = application
            .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
            .await
            .unwrap();
        assert_eq!(page.coverage(), ObservationReplayCoverage::Complete);
        assert_eq!(page.observations().len(), 1);
        let payload = page.observations()[0].observation().payload().to_string();
        assert!(!payload.contains(secret));
        assert!(payload.contains("TraceDecay redacted"));
    }

    #[tokio::test]
    async fn rejected_frames_never_reach_the_store() {
        let application = application();
        let outcome = application
            .capture_claude_observation(request(json!("not an object")))
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            CaptureClaudeObservationOutcome::Rejected { .. }
        ));
        assert!(application.store().observations.lock().unwrap().is_empty());
    }
}
