use serde_json::{Map, Value, json};
use tempfile::TempDir;
use tracedecay::application::observation::{
    CaptureClaudeObservationOutcome, CaptureClaudeObservationRequest, GetObservationRequest,
    ObservationApplication, ObservationApplicationError, ObservationCancellation,
    ReplayObservationsRequest,
};
use tracedecay::privacy::{
    ClaudeRecordParseErrorV1, ClaudeRecordSanitizerV1, ClaudeSanitizerPolicyV1,
    PR5_MAX_CLAUDE_RECORD_BYTES, parse_claude_record_v1,
};
use tracedecay::store::GlobalDbObservationStore;
use tracedecay_domain::{
    ClaudeByteRangeV1, ClaudeFileGenerationV1, ClaudeObservationIdentityMaterialV1,
    ClaudeSourceCursorV1, ClaudeSourceIdentityV1, ObservationScopeV1, RetentionClass, SessionId,
};
use tracedecay_store::{
    ObservationPersistOutcome, ObservationProjectionStore, ObservationReplayRequest,
    ObservationStore, ProjectionPersistOutcome,
};

use crate::common::{isolated_lcm_db_path, open_lcm_db};

const GENERATION: u64 = 17;
const OBSERVATION_TABLES: &[&str] = &[
    "sanitization_receipts",
    "observations",
    "source_cursors",
    "projection_queue",
    "observation_projection_provenance",
    "observation_projection_checkpoints",
    "sessions",
    "session_messages",
    "session_messages_fts",
];

fn source(session_id: &str) -> ClaudeSourceIdentityV1 {
    ClaudeSourceIdentityV1::new(SessionId::new(session_id).unwrap()).unwrap()
}

fn request(
    session_id: &str,
    record: Value,
    expected_cursor: Option<ClaudeSourceCursorV1>,
) -> CaptureClaudeObservationRequest {
    let encoded_frame = serde_json::to_vec(&record).unwrap();
    let frame_end = u64::try_from(encoded_frame.len()).unwrap();
    let parsed_record = parse_claude_record_v1(
        &encoded_frame,
        ClaudeByteRangeV1::new(0, frame_end).unwrap(),
    )
    .unwrap();
    CaptureClaudeObservationRequest::new(
        parsed_record,
        ClaudeObservationIdentityMaterialV1::new(
            source(session_id),
            ObservationScopeV1::Profile,
            ClaudeFileGenerationV1::new(GENERATION).unwrap(),
            ClaudeByteRangeV1::new(0, frame_end).unwrap(),
        )
        .unwrap(),
        expected_cursor,
        RetentionClass::new("retention.observation-application-test").unwrap(),
        ObservationCancellation::default(),
    )
    .unwrap()
}

fn nested_value(mut value: Value, depth: usize) -> Value {
    for _ in 0..depth {
        let mut object = Map::new();
        object.insert("nested".to_string(), value);
        value = Value::Object(object);
    }
    value
}

async fn table_counts(tmp: &TempDir) -> Vec<i64> {
    let db = libsql::Builder::new_local(isolated_lcm_db_path(tmp))
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    let mut counts = Vec::with_capacity(OBSERVATION_TABLES.len());
    for table in OBSERVATION_TABLES {
        let mut rows = conn
            .query(&format!("SELECT COUNT(*) FROM {table}"), ())
            .await
            .unwrap();
        counts.push(rows.next().await.unwrap().unwrap().get(0).unwrap());
    }
    counts
}

async fn durable_text(tmp: &TempDir) -> Vec<String> {
    let db = libsql::Builder::new_local(isolated_lcm_db_path(tmp))
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT observation_json FROM observations
             UNION ALL SELECT receipt_json FROM sanitization_receipts
             UNION ALL SELECT observation_id || receipt_id || output_digest
                 FROM observation_projection_provenance
             UNION ALL SELECT projector_version || CAST(last_sequence AS TEXT)
                 FROM observation_projection_checkpoints
             UNION ALL SELECT observation_id || CAST(observation_sequence AS TEXT)
                 FROM projection_queue
             UNION ALL SELECT provider || session_id || project_key || project_path ||
                 COALESCE(title, '') || COALESCE(metadata_json, '') FROM sessions
             UNION ALL SELECT provider || message_id || session_id || role || text ||
                 COALESCE(kind, '') || COALESCE(model, '') || COALESCE(tool_names, '') ||
                 COALESCE(metadata_json, '') FROM session_messages
             UNION ALL SELECT text || role || COALESCE(kind, '') || COALESCE(model, '') ||
                 COALESCE(tool_names, '') FROM session_messages_fts",
            (),
        )
        .await
        .unwrap();
    let mut values = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        values.push(row.get(0).unwrap());
    }
    values
}

fn conversational_record(message_id: &str, text: &str, secret: &str) -> Value {
    json!({
        "type": "user",
        "uuid": format!("record-{message_id}"),
        "timestamp": 1_750_000_000_i64,
        "api_key": secret,
        "message": {
            "id": message_id,
            "role": "user",
            "content": format!("{text}: {secret}")
        }
    })
}

#[tokio::test]
async fn secret_canary_is_absent_from_every_observation_sink_and_safe_representation() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let application = ObservationApplication::new(
        GlobalDbObservationStore::new(&db),
        ClaudeRecordSanitizerV1::pr5().unwrap(),
    );
    let session_id = "session.observation-privacy";
    let secret = "sk-proj-observation-sink-canary-1234567890";
    let record = conversational_record("message-private", "safe projected content", secret);

    let committed = application
        .capture_claude_observation(request(session_id, record.clone(), None))
        .await
        .unwrap();
    assert!(!format!("{committed:?}").contains(secret));
    let first_receipt = committed.sanitization_receipt().clone();
    let observation_id = match &committed {
        CaptureClaudeObservationOutcome::Persisted { outcome, .. } => {
            assert!(matches!(outcome, ObservationPersistOutcome::Committed(_)));
            outcome.receipt().observation().observation_id().clone()
        }
        other => panic!("sanitized record must persist, got {other:?}"),
    };
    let counts_after_commit = table_counts(&tmp).await;

    // Simulate a lost acknowledgement: retry the exact request after commit.
    let retry = application
        .capture_claude_observation(request(session_id, record.clone(), None))
        .await
        .unwrap();
    match &retry {
        CaptureClaudeObservationOutcome::Persisted { outcome, .. } => {
            assert!(matches!(
                outcome,
                ObservationPersistOutcome::ExactDuplicate(_)
            ));
        }
        other => panic!("exact retry must return the committed receipt, got {other:?}"),
    }
    assert_eq!(retry.sanitization_receipt(), &first_receipt);
    assert_eq!(table_counts(&tmp).await, counts_after_commit);
    assert!(!format!("{retry:?}").contains(secret));

    let projected = application
        .store()
        .project_observation(&observation_id)
        .await
        .unwrap();
    assert!(matches!(projected, ProjectionPersistOutcome::Projected(_)));
    assert!(!format!("{projected:?}").contains(secret));

    let point = application
        .get_observation(GetObservationRequest::new(
            observation_id.clone(),
            ObservationCancellation::default(),
        ))
        .await
        .unwrap();
    let replay = application
        .replay_observations(ReplayObservationsRequest::new(
            ObservationReplayRequest::new(0, 10).unwrap(),
            ObservationCancellation::default(),
        ))
        .await
        .unwrap();
    assert!(point.observation().is_some());
    assert_eq!(replay.observations().len(), 1);
    assert!(!format!("{point:?}{replay:?}").contains(secret));
    assert!(
        db.search_session_messages("claude", None, secret, 10)
            .await
            .is_empty()
    );
    assert_eq!(
        db.search_session_messages("claude", None, "safe projected content", 10)
            .await
            .len(),
        1
    );
    for value in durable_text(&tmp).await {
        assert!(
            !value.contains(secret),
            "secret leaked into durable text: {value}"
        );
    }

    let collision_secret = "sk-proj-collision-errors-canary-0987654321";
    let collision_record = conversational_record(
        "message-private",
        "different payload text",
        collision_secret,
    );
    assert_eq!(
        serde_json::to_vec(&collision_record).unwrap().len(),
        serde_json::to_vec(&record).unwrap().len(),
        "collision fixture must preserve the source identity range"
    );
    let collision = application
        .capture_claude_observation(request(session_id, collision_record, None))
        .await
        .expect_err("same identity with different sanitized payload must collide");
    assert!(matches!(collision, ObservationApplicationError::Store(_)));
    let safe_error = format!("{collision:?}\n{collision}");
    assert!(!safe_error.contains(secret));
    assert!(!safe_error.contains(collision_secret));
    for value in durable_text(&tmp).await {
        assert!(!value.contains(secret));
        assert!(!value.contains(collision_secret));
    }
}

#[tokio::test]
async fn rejected_and_quarantined_records_leave_every_authoritative_state_unchanged() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let application = ObservationApplication::new(
        GlobalDbObservationStore::new(&db),
        ClaudeRecordSanitizerV1::new(
            ClaudeSanitizerPolicyV1::pr5()
                .unwrap()
                .with_limits(1, usize::MAX, usize::MAX)
                .unwrap(),
        ),
    );
    let quarantine_application = ObservationApplication::new(
        GlobalDbObservationStore::new(&db),
        ClaudeRecordSanitizerV1::new(
            ClaudeSanitizerPolicyV1::pr5()
                .unwrap()
                .with_limits(usize::MAX, 2, usize::MAX)
                .unwrap(),
        ),
    );
    let session_id = "session.observation-nondurable";
    let rejected_secret = "sk-proj-rejected-canary-1234567890";
    let quarantined_secret = "sk-proj-quarantined-canary-1234567890";
    let before = table_counts(&tmp).await;

    let rejected = application
        .capture_claude_observation(request(
            session_id,
            json!({"payload": rejected_secret}),
            None,
        ))
        .await
        .unwrap();
    assert!(matches!(
        rejected,
        CaptureClaudeObservationOutcome::Rejected { .. }
    ));
    assert!(!format!("{rejected:?}").contains(rejected_secret));

    let quarantined = quarantine_application
        .capture_claude_observation(request(
            session_id,
            nested_value(json!(quarantined_secret), 4),
            None,
        ))
        .await
        .unwrap();
    assert!(matches!(
        quarantined,
        CaptureClaudeObservationOutcome::Quarantined { .. }
    ));
    assert!(!format!("{quarantined:?}").contains(quarantined_secret));

    assert_eq!(table_counts(&tmp).await, before);
    assert!(
        application
            .store()
            .get_source_cursor(&source(session_id), &ObservationScopeV1::Profile)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        application
            .store()
            .projection_checkpoint()
            .await
            .unwrap()
            .last_sequence(),
        0
    );
    assert!(
        application
            .replay_observations(ReplayObservationsRequest::new(
                ObservationReplayRequest::new(0, 10).unwrap(),
                ObservationCancellation::default(),
            ))
            .await
            .unwrap()
            .observations()
            .is_empty()
    );
}

#[tokio::test]
async fn malformed_partial_and_oversized_frames_leave_authoritative_state_unchanged() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let application = ObservationApplication::new(
        GlobalDbObservationStore::new(&db),
        ClaudeRecordSanitizerV1::pr5().unwrap(),
    );
    let session_id = "session.observation-invalid-frame";
    let before = table_counts(&tmp).await;

    for frame in [
        br#"{"type":"user",malformed}"#.as_slice(),
        br#"{"type":"user""#.as_slice(),
    ] {
        let frame_end = u64::try_from(frame.len()).unwrap();
        assert_eq!(
            parse_claude_record_v1(frame, ClaudeByteRangeV1::new(0, frame_end).unwrap()).err(),
            Some(ClaudeRecordParseErrorV1::Malformed)
        );
    }
    let oversized = vec![b'x'; PR5_MAX_CLAUDE_RECORD_BYTES + 1];
    assert_eq!(
        parse_claude_record_v1(
            &oversized,
            ClaudeByteRangeV1::new(0, u64::try_from(oversized.len()).unwrap()).unwrap(),
        )
        .err(),
        Some(ClaudeRecordParseErrorV1::TooLarge)
    );

    assert_eq!(table_counts(&tmp).await, before);
    assert!(
        application
            .store()
            .get_source_cursor(&source(session_id), &ObservationScopeV1::Profile)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        application
            .store()
            .projection_checkpoint()
            .await
            .unwrap()
            .last_sequence(),
        0
    );
}
