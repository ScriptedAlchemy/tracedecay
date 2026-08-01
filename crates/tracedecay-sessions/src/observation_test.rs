use std::fs;
use std::process::Command;
use std::sync::Mutex;

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_domain::{
    ClaudeByteRangeV1, ClaudeFileGenerationV1, ClaudeSourceIdentityV1, EvidenceAvailabilityV1,
    ObservationScopeV1, ProjectId, RepositoryId, RetrievalAnchorTargetV2, SessionId, WorktreeId,
};
use tracedecay_store::observation::{
    CursorAdvanceOutcome, NonDurableFrameReason, ObservationCursorAdvance,
};
use tracedecay_store::{ObservationCommitReceipt, ObservationStore, ObservationStoreResult};

use tracedecay_runtime_core::privacy::parse_claude_record_v1;

use super::*;

#[derive(Default)]
struct FakeStore {
    observations: Mutex<Vec<StoredObservation>>,
    source_cursors: Mutex<Vec<ObservationSourceCursorV1>>,
    cancel_on_persist: Mutex<Option<ObservationCancellation>>,
    cancel_on_get: Mutex<Option<ObservationCancellation>>,
    cancel_on_replay: Mutex<Option<ObservationCancellation>>,
    cancel_on_advance: Mutex<Option<ObservationCancellation>>,
    cursor_advances: Mutex<Vec<ObservationCursorAdvance>>,
}

impl ObservationStore for FakeStore {
    async fn persist_observation(
        &self,
        write: AnchoredObservationWrite,
    ) -> ObservationStoreResult<ObservationPersistOutcome> {
        let mut observations = self.observations.lock().unwrap();
        if let Some(stored) = observations.iter().find(|stored| {
            stored.observation().observation_id() == write.observation().observation_id()
        }) {
            return Ok(ObservationPersistOutcome::ExactDuplicate(
                stored.commit_receipt().clone(),
            ));
        }
        let (write, retrieval_anchor, projection_generation, repository_provenance) =
            write.into_parts();
        let sequence = u64::try_from(observations.len()).unwrap() + 1;
        let observation = write.observation().clone();
        let cursor = write.next_cursor().clone();
        let receipt = ObservationCommitReceipt::new(
            sequence,
            observation.clone(),
            cursor.clone(),
            retrieval_anchor,
            projection_generation,
        )?
        .with_repository_provenance_attachment(repository_provenance)?;
        let mut cursors = self.source_cursors.lock().unwrap();
        cursors.retain(|existing| {
            existing.source() != cursor.source() || existing.scope() != cursor.scope()
        });
        cursors.push(cursor.clone());
        drop(cursors);
        observations.push(StoredObservation::from_commit_receipt(
            receipt.clone(),
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
    ) -> ObservationStoreResult<Option<ObservationSourceCursorV1>> {
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
        if let Some(cancellation) = self.cancel_on_advance.lock().unwrap().take() {
            cancellation.cancel();
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
        ClaudeSourceIdentityV1::new(SessionId::new("session.application-test").unwrap()).unwrap();
    let scope = ObservationScopeV1::Project {
        project_id: ProjectId::new("project.application-test").unwrap(),
    };
    let generation = ClaudeFileGenerationV1::new(1).unwrap();
    let expected_cursor = (start != 0).then(|| {
        ObservationSourceCursorV1::new(source.clone(), scope.clone(), generation, start).unwrap()
    });
    let identity = ObservationIdentityMaterialV1::new(
        source,
        scope,
        generation,
        ClaudeByteRangeV1::new(start, end).unwrap(),
    )
    .unwrap();
    CaptureClaudeObservationRequest::new(
        parsed_record,
        identity,
        expected_cursor,
        RetentionClass::new("retention.application-test").unwrap(),
        cancellation,
    )
    .unwrap()
}

fn application() -> ObservationApplication<FakeStore> {
    ObservationApplication::new(
        FakeStore::default(),
        RecordSanitizerV1::claude_v1().unwrap(),
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
        .advance_non_durable_source_cursor(AdvanceNonDurableSourceCursorRequest::new(
            advance,
            ObservationCancellation::default(),
        ))
        .await
        .unwrap();

    assert_eq!(outcome, CursorAdvanceOutcome::Committed);
    let advances = application.store.cursor_advances.lock().unwrap();
    assert_eq!(advances.len(), 1);
    assert_eq!(advances[0].covered(), ClaudeByteRangeV1::new(0, 4).unwrap());
    assert_eq!(advances[0].reason(), NonDurableFrameReason::BlankFrame);
}

#[tokio::test]
async fn non_durable_cursor_advance_honors_cancellation_before_and_after_commit() {
    let application = application();
    let source =
        ClaudeSourceIdentityV1::new(SessionId::new("session.cursor-cancel").unwrap()).unwrap();
    let advance = ObservationCursorAdvance::new(
        source,
        ObservationScopeV1::Profile,
        ClaudeFileGenerationV1::new(1).unwrap(),
        None,
        ClaudeByteRangeV1::new(0, 4).unwrap(),
        NonDurableFrameReason::BlankFrame,
    )
    .unwrap();

    let cancelled = ObservationCancellation::default();
    cancelled.cancel();
    assert!(matches!(
        application
            .advance_non_durable_source_cursor(AdvanceNonDurableSourceCursorRequest::new(
                advance.clone(),
                cancelled,
            ))
            .await,
        Err(ObservationApplicationError::Cancelled)
    ));
    assert!(application.store.cursor_advances.lock().unwrap().is_empty());

    let cancelled_after_commit = ObservationCancellation::default();
    *application.store.cancel_on_advance.lock().unwrap() = Some(cancelled_after_commit.clone());
    assert!(matches!(
        application
            .advance_non_durable_source_cursor(AdvanceNonDurableSourceCursorRequest::new(
                advance.clone(),
                cancelled_after_commit,
            ))
            .await,
        Err(ObservationApplicationError::Cancelled)
    ));
    assert_eq!(application.store.cursor_advances.lock().unwrap().len(), 1);

    let retry = application
        .advance_non_durable_source_cursor(AdvanceNonDurableSourceCursorRequest::new(
            advance,
            ObservationCancellation::default(),
        ))
        .await
        .unwrap();
    assert_eq!(retry, CursorAdvanceOutcome::ExactDuplicate);
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
        CaptureObservationOutcome::Persisted {
            sanitized_record, ..
        } => sanitized_record,
        other => panic!("capture must persist, got {other:?}"),
    };
    assert!(!sanitized_record.payload().to_string().contains(secret));
    assert!(matches!(
        outcome,
        CaptureObservationOutcome::Persisted { .. }
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
    assert!(matches!(
        page.observations()[0]
            .repository_provenance_attachment()
            .availability(),
        EvidenceAvailabilityV1::Unavailable
    ));
    let payload = page.observations()[0].observation().payload().to_string();
    assert!(!payload.contains(secret));
    assert!(payload.contains("TraceDecay redacted"));
}

#[tokio::test]
async fn repository_provenance_is_bound_to_the_sanitized_observation_write() {
    let repository = TempDir::new().unwrap();
    let git = |args: &[&str]| {
        let output = Command::new(tracedecay_runtime_core::git::git_program())
            .args(args)
            .current_dir(repository.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.name", "TraceDecay Test"]);
    git(&["config", "user.email", "tracedecay@example.invalid"]);
    fs::write(repository.path().join("tracked.txt"), "content").unwrap();
    git(&["add", "--", "tracked.txt"]);
    git(&["commit", "-q", "-m", "initial"]);

    let application = application();
    let request = request(&json!({
        "type": "user",
        "message": { "role": "user", "content": "repository evidence" },
        "api_key": "sk-repository-provenance-secret-1234567890"
    }))
    .with_repository_provenance(Some(RepositoryProvenanceAdmissionContext::new(
        repository.path().to_path_buf(),
        ProjectId::new("project.application-test").unwrap(),
        RepositoryId::new("repository.application-test").unwrap(),
        Some(WorktreeId::new("worktree.application-test").unwrap()),
        [0x5a; 32],
    )));
    let outcome = application.capture_observation(request).await.unwrap();
    let CaptureObservationOutcome::Persisted { outcome, .. } = outcome else {
        panic!("repository observation must persist");
    };
    let attachment = outcome.receipt().repository_provenance_attachment();
    let EvidenceAvailabilityV1::Known(provenance) = attachment.availability() else {
        panic!("repository provenance must be known");
    };
    assert_eq!(
        provenance.source_observation(),
        Some(outcome.receipt().observation().observation_id())
    );
    assert!(matches!(
        attachment.anchor().map(tracedecay_domain::RetrievalAnchorRecordV2::target),
        Some(RetrievalAnchorTargetV2::RepositoryCapture { capture_id, .. })
            if capture_id == provenance.capture_id()
    ));
    let encoded = serde_json::to_string(attachment).unwrap();
    assert!(!encoded.contains(repository.path().to_string_lossy().as_ref()));
    assert!(!encoded.contains("sk-repository-provenance-secret"));
}

#[tokio::test]
async fn repository_provenance_context_refuses_cross_project_reuse() {
    let repository = TempDir::new().unwrap();
    let request = request(&json!({
        "type": "user",
        "message": { "role": "user", "content": "cross-project evidence" }
    }))
    .with_repository_provenance(Some(RepositoryProvenanceAdmissionContext::new(
        repository.path().to_path_buf(),
        ProjectId::new("project.other-authority").unwrap(),
        RepositoryId::new("repository.cross-project-test").unwrap(),
        Some(WorktreeId::new("worktree.cross-project-test").unwrap()),
        [0x5a; 32],
    )));

    let CaptureObservationOutcome::Persisted { outcome, .. } =
        application().capture_observation(request).await.unwrap()
    else {
        panic!("cross-project observation must still persist");
    };
    assert!(matches!(
        outcome
            .receipt()
            .repository_provenance_attachment()
            .availability(),
        EvidenceAvailabilityV1::Unavailable
    ));
}

#[test]
fn request_accepts_only_bounded_parser_evidence_for_the_identity_range() {
    let identity = |start, end| {
        ObservationIdentityMaterialV1::new(
            ClaudeSourceIdentityV1::new(SessionId::new("session.frame-test").unwrap()).unwrap(),
            ObservationScopeV1::Profile,
            ClaudeFileGenerationV1::new(1).unwrap(),
            ClaudeByteRangeV1::new(start, end).unwrap(),
        )
        .unwrap()
    };
    let retention = || RetentionClass::new("retention.application-test").unwrap();

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
        CaptureObservationOutcome::Persisted {
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
            stored.retrieval_anchor().clone(),
            stored.projection_generation().clone(),
            ObservationProjectionStatus::NotQueued,
        )
        .unwrap();
    }

    let duplicate = application
        .capture_claude_observation(request(&record))
        .await
        .unwrap();
    match duplicate {
        CaptureObservationOutcome::Persisted {
            outcome,
            projection_status,
            sanitized_record,
            ..
        } => {
            assert!(matches!(
                *outcome,
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
    let mut start = 0;
    for index in 0..3 {
        let record = json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": format!("message {index}")
            }
        });
        application
            .capture_claude_observation(request_at(&record, start))
            .await
            .unwrap();
        start += u64::try_from(serde_json::to_vec(&record).unwrap().len()).unwrap();
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
                    seed.retrieval_anchor().clone(),
                    seed.projection_generation().clone(),
                    seed.projection_status(),
                )
                .unwrap()
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
    let CaptureObservationOutcome::Persisted { outcome, .. } = retry else {
        panic!("retry must persist");
    };
    assert!(matches!(
        *outcome,
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
        CaptureObservationOutcome::Persisted { outcome, .. } => {
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
