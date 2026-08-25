use serde_json::{Map, Value, json};
use tempfile::TempDir;
use tracedecay::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay::privacy::{
    ClaudeRecordParseErrorV1, ClaudeRecordSanitizerV1, ClaudeSanitizerPolicyV1,
    PrivacySanitizerError, RecordSanitizerV1, parse_claude_record_v1,
    parse_normalized_observation_record_v1, parse_observation_record_v1,
};
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, ClaudeByteRangeV1,
    ClaudeFileGenerationV1, ClaudeObservationIdentityMaterialV1, ClaudeSourceCursorV1,
    ClaudeSourceIdentityV1, ObservationId, ObservationIdentityMaterialV1,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    ProviderId, RetentionClass, SessionId,
};
use tracedecay_store::observation::{NonDurableFrameReason, ObservationCursorAdvance};
use tracedecay_store::{
    ObservationPersistOutcome, ObservationProjectionStore, ObservationReplayRequest,
    ObservationStore, ProjectionPersistOutcome,
};
use tracedecay_usecases::host_admission::HostAdmissionScope;
use tracedecay_usecases::observation::{
    AdvanceNonDurableSourceCursorRequest, CaptureClaudeObservationOutcome,
    CaptureClaudeObservationRequest, CaptureObservationOutcome, CaptureObservationRequest,
    GetObservationRequest, ObservationApplication, ObservationApplicationError,
    ObservationCancellation, ReplayObservationsRequest,
};

const GENERATION: u64 = 17;
const OBSERVATION_TABLES: &[&str] = &[
    "sanitization_receipts",
    "observations",
    "source_cursors",
    "source_cursor_advances",
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

async fn profile_runtime(tmp: &TempDir) -> HostAdmissionTestRuntimeV1 {
    HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
        .await
        .unwrap()
}

async fn table_counts(runtime: &HostAdmissionTestRuntimeV1) -> Vec<i64> {
    let snapshot = TempDir::new().unwrap();
    let database_path = snapshot.path().join("sessions.db");
    runtime
        .snapshot_session_database_for_test(HostAdmissionScope::Profile, &database_path)
        .await
        .unwrap();
    let conn = rusqlite::Connection::open(database_path).unwrap();
    OBSERVATION_TABLES
        .iter()
        .map(|table| {
            conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), (), |row| {
                row.get(0)
            })
            .unwrap()
        })
        .collect()
}

async fn durable_text(runtime: &HostAdmissionTestRuntimeV1) -> Vec<String> {
    let snapshot = TempDir::new().unwrap();
    let database_path = snapshot.path().join("sessions.db");
    runtime
        .snapshot_session_database_for_test(HostAdmissionScope::Profile, &database_path)
        .await
        .unwrap();
    let conn = rusqlite::Connection::open(database_path).unwrap();
    let mut statement = conn
        .prepare(
            "SELECT observation_json FROM observations
             UNION ALL SELECT receipt_json FROM sanitization_receipts
             UNION ALL SELECT source_json || scope_json || cursor_json FROM source_cursors
             UNION ALL SELECT source_json || scope_json || coverage_json || reason ||
                 COALESCE(receipt_id, '') FROM source_cursor_advances
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
        )
        .unwrap();
    statement
        .query_map((), |row| row.get(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

async fn session_message_search_count(
    runtime: &HostAdmissionTestRuntimeV1,
    provider: &str,
    query: &str,
) -> usize {
    runtime
        .search_session_messages_for_test(
            HostAdmissionScope::Profile,
            provider,
            None,
            query,
            OBSERVATION_TABLES.len(),
        )
        .await
        .unwrap()
        .len()
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
    let runtime = profile_runtime(&tmp).await;
    let application = ObservationApplication::new(
        runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap(),
        ClaudeRecordSanitizerV1::claude_v1().unwrap(),
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
            assert!(matches!(**outcome, ObservationPersistOutcome::Committed(_)));
            outcome.receipt().observation().observation_id().clone()
        }
        other => panic!("sanitized record must persist, got {other:?}"),
    };
    let counts_after_commit = table_counts(&runtime).await;

    // Simulate a lost acknowledgement: retry the exact request after commit.
    let retry = application
        .capture_claude_observation(request(session_id, record.clone(), None))
        .await
        .unwrap();
    match &retry {
        CaptureClaudeObservationOutcome::Persisted { outcome, .. } => {
            assert!(matches!(
                **outcome,
                ObservationPersistOutcome::ExactDuplicate(_)
            ));
        }
        other => panic!("exact retry must return the committed receipt, got {other:?}"),
    }
    assert_eq!(retry.sanitization_receipt(), &first_receipt);
    assert_eq!(table_counts(&runtime).await, counts_after_commit);
    assert!(!format!("{retry:?}").contains(secret));

    let projected = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap()
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
    assert_eq!(
        session_message_search_count(&runtime, "claude", secret).await,
        0
    );
    assert_eq!(
        session_message_search_count(&runtime, "claude", "safe projected content").await,
        1
    );
    for value in durable_text(&runtime).await {
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
    for value in durable_text(&runtime).await {
        assert!(!value.contains(secret));
        assert!(!value.contains(collision_secret));
    }
}

#[tokio::test]
async fn rejected_and_quarantined_records_leave_every_authoritative_state_unchanged() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let application = ObservationApplication::new(
        runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap(),
        ClaudeRecordSanitizerV1::new(
            ClaudeSanitizerPolicyV1::claude_v1()
                .unwrap()
                .with_limits(1, usize::MAX, usize::MAX)
                .unwrap(),
        ),
    );
    let quarantine_application = ObservationApplication::new(
        runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap(),
        ClaudeRecordSanitizerV1::new(
            ClaudeSanitizerPolicyV1::claude_v1()
                .unwrap()
                .with_limits(usize::MAX, 2, usize::MAX)
                .unwrap(),
        ),
    );
    let session_id = "session.observation-nondurable";
    let rejected_secret = "sk-proj-rejected-canary-1234567890";
    let quarantined_secret = "sk-proj-quarantined-canary-1234567890";
    let before = table_counts(&runtime).await;

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

    assert_eq!(table_counts(&runtime).await, before);
    assert!(
        runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap()
            .get_source_cursor(&source(session_id), &ObservationScopeV1::Profile)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap()
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
async fn native_ordering_domain_survives_authoritative_capture() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let application = ObservationApplication::new(
        runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap(),
        RecordSanitizerV1::observation_v1().unwrap(),
    );
    let source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new("hermes").unwrap(),
        SessionId::new("session.native-ordering").unwrap(),
    )
    .unwrap();
    let range = ObservationSourceRangeV1::new(41, 42).unwrap();
    let ordering_domain = ObservationOrderingDomainV1::SqliteRowId;
    let record = serde_json::to_vec(&json!({ "text": "native ordering payload" })).unwrap();
    let parsed =
        parse_normalized_observation_record_v1(&record, range, ordering_domain, |native| {
            CanonicalObservationEnvelopeV1::new(
                ProviderId::new("hermes").unwrap(),
                "message",
                ObservationId::new("hermes.message.native-ordering").unwrap(),
                CanonicalObservationRelationsV1::new(
                    SessionId::new("session.native-ordering").unwrap(),
                )
                .with_message_id(ObservationId::new("hermes.message.native-ordering").unwrap()),
                vec![CanonicalObservationFactV1::Message {
                    role: CanonicalMessageRoleV1::Assistant,
                    content: native,
                    model: None,
                    timestamp: None,
                }],
                CanonicalObservationEvidenceV1::new(ordering_domain, range),
            )
            .map_err(|_| ClaudeRecordParseErrorV1::NormalizationFailed)
        })
        .unwrap();
    let request = CaptureObservationRequest::new(
        parsed,
        ObservationIdentityMaterialV1::for_native_record(
            source.clone(),
            ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(GENERATION).unwrap(),
            range,
            ordering_domain,
            ObservationId::new("hermes.message.native-ordering").unwrap(),
        )
        .unwrap(),
        None,
        RetentionClass::new("retention.observation-application-test").unwrap(),
        ObservationCancellation::default(),
    )
    .unwrap();

    let outcome = application.capture_observation(request).await.unwrap();
    let observation_id = match &outcome {
        CaptureObservationOutcome::Persisted { outcome, .. } => {
            assert!(matches!(**outcome, ObservationPersistOutcome::Committed(_)));
            outcome.receipt().observation().observation_id().clone()
        }
        other => panic!("native observation must persist, got {other:?}"),
    };
    let cursor = store
        .get_source_cursor(&source, &ObservationScopeV1::Profile)
        .await
        .unwrap()
        .expect("native cursor");
    assert_eq!(cursor.ordering_domain(), ordering_domain);
    assert_eq!(cursor.position(), 42);

    let projected = store.project_observation(&observation_id).await.unwrap();
    assert!(matches!(projected, ProjectionPersistOutcome::Projected(_)));
    assert_eq!(
        store.projection_checkpoint().await.unwrap().last_sequence(),
        1
    );
    assert!(store.next_queued_observation().await.unwrap().is_none());
}

const CROSS_PROVIDERS: &[&str] = &[
    "claude", "codex", "cursor", "hermes", "kiro", "cline", "roo-code", "kilo",
];

fn provider_source(provider: &str) -> ObservationSourceIdentityV1 {
    ObservationSourceIdentityV1::for_provider(
        ProviderId::new(provider).unwrap(),
        SessionId::new(format!("session.app-cross.{provider}")).unwrap(),
    )
    .unwrap()
}

fn provider_cursor(provider: &str, position: u64) -> ObservationSourceCursorV1 {
    ObservationSourceCursorV1::for_ordering(
        provider_source(provider),
        ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(GENERATION).unwrap(),
        ObservationOrderingDomainV1::SqliteRowId,
        position,
    )
    .unwrap()
}

fn provider_capture_request(
    provider: &str,
    record_id: &str,
    start: u64,
    end: u64,
    text: &str,
    expected_cursor: Option<ObservationSourceCursorV1>,
    cancellation: ObservationCancellation,
) -> CaptureObservationRequest {
    provider_capture_request_with_canonical_provider(
        provider,
        provider,
        record_id,
        ObservationSourceRangeV1::new(start, end).unwrap(),
        text,
        expected_cursor,
        cancellation,
    )
}

fn provider_capture_request_with_canonical_provider(
    provider: &str,
    canonical_provider: &str,
    record_id: &str,
    range: ObservationSourceRangeV1,
    text: &str,
    expected_cursor: Option<ObservationSourceCursorV1>,
    cancellation: ObservationCancellation,
) -> CaptureObservationRequest {
    let record = json!({ "text": text });
    let encoded = serde_json::to_vec(&record).unwrap();
    let ordering_domain = ObservationOrderingDomainV1::SqliteRowId;
    let canonical_provider = canonical_provider.to_owned();
    let record_owned = record_id.to_owned();
    let session_id = format!("session.app-cross.{provider}");
    let parsed =
        parse_normalized_observation_record_v1(&encoded, range, ordering_domain, move |native| {
            CanonicalObservationEnvelopeV1::new(
                ProviderId::new(&canonical_provider).unwrap(),
                "message",
                ObservationId::new(record_owned.clone()).unwrap(),
                CanonicalObservationRelationsV1::new(SessionId::new(session_id.clone()).unwrap())
                    .with_message_id(ObservationId::new(record_owned.clone()).unwrap()),
                vec![CanonicalObservationFactV1::Message {
                    role: CanonicalMessageRoleV1::Assistant,
                    content: native,
                    model: None,
                    timestamp: None,
                }],
                CanonicalObservationEvidenceV1::new(ordering_domain, range),
            )
            .map_err(|_| ClaudeRecordParseErrorV1::NormalizationFailed)
        })
        .unwrap();
    CaptureObservationRequest::new(
        parsed,
        ObservationIdentityMaterialV1::for_native_record(
            provider_source(provider),
            ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(GENERATION).unwrap(),
            range,
            ordering_domain,
            ObservationId::new(record_id).unwrap(),
        )
        .unwrap(),
        expected_cursor,
        RetentionClass::new("retention.observation-application-test").unwrap(),
        cancellation,
    )
    .unwrap()
}

#[tokio::test]
async fn missing_and_conflicting_canonical_identity_leave_authoritative_state_empty() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let application = ObservationApplication::new(
        runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap(),
        RecordSanitizerV1::observation_v1().unwrap(),
    );
    let range = ObservationSourceRangeV1::new(0, 1).unwrap();
    let ordering_domain = ObservationOrderingDomainV1::SqliteRowId;
    let raw = serde_json::to_vec(&json!({ "text": "identity canary" })).unwrap();
    let parsed = parse_observation_record_v1(&raw, range, ordering_domain).unwrap();
    let identity = ObservationIdentityMaterialV1::for_native_record(
        provider_source("codex"),
        ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(GENERATION).unwrap(),
        range,
        ordering_domain,
        ObservationId::new("codex.missing-canonical").unwrap(),
    )
    .unwrap();
    let missing = CaptureObservationRequest::new(
        parsed,
        identity,
        None,
        RetentionClass::new("retention.observation-application-test").unwrap(),
        ObservationCancellation::default(),
    )
    .unwrap();
    let missing_error = application
        .capture_observation(missing)
        .await
        .expect_err("provider-neutral capture requires canonical identity");
    assert!(matches!(
        missing_error,
        ObservationApplicationError::Privacy(PrivacySanitizerError::CanonicalEnvelopeRequired)
    ));
    assert_eq!(
        table_counts(&runtime).await,
        vec![0; OBSERVATION_TABLES.len()]
    );

    let conflicting = provider_capture_request_with_canonical_provider(
        "codex",
        "cursor",
        "codex.conflicting-provider",
        range,
        "identity canary",
        None,
        ObservationCancellation::default(),
    );
    let conflicting_error = application
        .capture_observation(conflicting)
        .await
        .expect_err("canonical and routing providers must agree");
    assert!(matches!(
        conflicting_error,
        ObservationApplicationError::Privacy(PrivacySanitizerError::CanonicalProviderMismatch)
    ));
    assert_eq!(
        table_counts(&runtime).await,
        vec![0; OBSERVATION_TABLES.len()]
    );
}

// These contract cases construct normalized canonical provider-tagged records directly.
// They do not exercise provider transcript parsers or JSONL framing.
#[tokio::test]
async fn cross_provider_capture_duplicate_conflict_cancel_non_durable_malformed_frame_and_commit_before_ack()
 {
    for provider in CROSS_PROVIDERS {
        let tmp = TempDir::new().unwrap();
        let runtime = profile_runtime(&tmp).await;
        let application = ObservationApplication::new(
            runtime
                .observation_store(HostAdmissionScope::Profile)
                .unwrap(),
            RecordSanitizerV1::observation_v1().unwrap(),
        );
        let store = runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap();
        let source = provider_source(provider);
        let record_id = format!("{provider}.message.app-cross");

        let cancelled = ObservationCancellation::default();
        cancelled.cancel();
        let cancelled_outcome = application
            .capture_observation(provider_capture_request(
                provider,
                &record_id,
                0,
                1,
                "cancelled before commit",
                None,
                cancelled,
            ))
            .await;
        assert!(
            matches!(
                cancelled_outcome,
                Err(ObservationApplicationError::Cancelled)
            ),
            "{provider}: pre-cancel must not capture, got {cancelled_outcome:?}"
        );
        assert!(
            store
                .get_source_cursor(&source, &ObservationScopeV1::Profile)
                .await
                .unwrap()
                .is_none(),
            "{provider}"
        );
        assert_eq!(
            table_counts(&runtime).await,
            vec![0; OBSERVATION_TABLES.len()]
        );

        let first = application
            .capture_observation(provider_capture_request(
                provider,
                &record_id,
                0,
                1,
                "stable application payload",
                None,
                ObservationCancellation::default(),
            ))
            .await
            .unwrap();
        let first_receipt = first.sanitization_receipt().clone();
        let observation_id = match &first {
            CaptureObservationOutcome::Persisted { outcome, .. } => {
                assert!(matches!(**outcome, ObservationPersistOutcome::Committed(_)));
                outcome.receipt().observation().observation_id().clone()
            }
            other => panic!("{provider}: first capture must persist, got {other:?}"),
        };
        let counts_after_commit = table_counts(&runtime).await;
        let cursor_after_commit = store
            .get_source_cursor(&source, &ObservationScopeV1::Profile)
            .await
            .unwrap();

        let duplicate = application
            .capture_observation(provider_capture_request(
                provider,
                &record_id,
                0,
                1,
                "stable application payload",
                None,
                ObservationCancellation::default(),
            ))
            .await
            .unwrap();
        match &duplicate {
            CaptureObservationOutcome::Persisted { outcome, .. } => {
                assert!(
                    matches!(**outcome, ObservationPersistOutcome::ExactDuplicate(_)),
                    "{provider}: exact retry must be ExactDuplicate, got {outcome:?}"
                );
            }
            other => panic!("{provider}: exact retry must persist as duplicate, got {other:?}"),
        }
        assert_eq!(
            duplicate.sanitization_receipt(),
            &first_receipt,
            "{provider}"
        );
        assert_eq!(
            table_counts(&runtime).await,
            counts_after_commit,
            "{provider}"
        );

        let conflict = application
            .capture_observation(provider_capture_request(
                provider,
                &record_id,
                0,
                1,
                "conflicting application payload",
                None,
                ObservationCancellation::default(),
            ))
            .await
            .expect_err("{provider}: conflicting identity must fail");
        assert!(
            matches!(
                conflict,
                ObservationApplicationError::Store(
                    tracedecay_store::ObservationStoreError::ObservationCollision { .. }
                )
            ),
            "{provider}: expected ObservationCollision, got {conflict:?}"
        );
        assert_eq!(
            table_counts(&runtime).await,
            counts_after_commit,
            "{provider}"
        );
        assert_eq!(
            store
                .get_source_cursor(&source, &ObservationScopeV1::Profile)
                .await
                .unwrap(),
            cursor_after_commit,
            "{provider}"
        );

        let malformed_advance = ObservationCursorAdvance::for_ordering(
            source.clone(),
            ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(GENERATION).unwrap(),
            ObservationOrderingDomainV1::SqliteRowId,
            cursor_after_commit.clone(),
            ObservationSourceRangeV1::new(1, 2).unwrap(),
            NonDurableFrameReason::MalformedFrame,
        )
        .unwrap();
        application
            .advance_non_durable_source_cursor(AdvanceNonDurableSourceCursorRequest::new(
                malformed_advance.clone(),
                ObservationCancellation::default(),
            ))
            .await
            .unwrap();
        assert_eq!(
            store
                .get_source_cursor(&source, &ObservationScopeV1::Profile)
                .await
                .unwrap(),
            Some(provider_cursor(provider, 2)),
            "{provider}"
        );
        let malformed_advance_retry = application
            .advance_non_durable_source_cursor(AdvanceNonDurableSourceCursorRequest::new(
                malformed_advance,
                ObservationCancellation::default(),
            ))
            .await
            .unwrap();
        assert_eq!(
            malformed_advance_retry,
            tracedecay_store::observation::CursorAdvanceOutcome::ExactDuplicate,
            "{provider}"
        );

        let captured_after_malformed_frame = application
            .capture_observation(provider_capture_request(
                provider,
                &format!("{provider}.message.app-after-malformed"),
                2,
                3,
                "captured after non-durable malformed frame",
                Some(provider_cursor(provider, 2)),
                ObservationCancellation::default(),
            ))
            .await
            .unwrap();
        assert!(
            matches!(
                &captured_after_malformed_frame,
                CaptureObservationOutcome::Persisted { outcome, .. }
                    if matches!(**outcome, ObservationPersistOutcome::Committed(_))
            ),
            "{provider}: capture after malformed-frame coverage must commit, got {captured_after_malformed_frame:?}"
        );

        let cancel_after = ObservationCancellation::default();
        cancel_after.cancel();
        assert!(
            matches!(
                application
                    .get_observation(GetObservationRequest::new(
                        observation_id.clone(),
                        cancel_after.clone(),
                    ))
                    .await,
                Err(ObservationApplicationError::Cancelled)
            ),
            "{provider}"
        );
        assert!(
            matches!(
                application
                    .replay_observations(ReplayObservationsRequest::new(
                        ObservationReplayRequest::new(0, 10).unwrap(),
                        cancel_after,
                    ))
                    .await,
                Err(ObservationApplicationError::Cancelled)
            ),
            "{provider}"
        );

        let replay_before_restart = application
            .replay_observations(ReplayObservationsRequest::new(
                ObservationReplayRequest::new(0, 10).unwrap(),
                ObservationCancellation::default(),
            ))
            .await
            .unwrap();
        assert_eq!(replay_before_restart.observations().len(), 2, "{provider}");
        let counts_before_restart = table_counts(&runtime).await;
        drop(application);
        drop(runtime);

        // Commit-before-ack / crash-restart: reopen and retry the original capture.
        let runtime = profile_runtime(&tmp).await;
        let application = ObservationApplication::new(
            runtime
                .observation_store(HostAdmissionScope::Profile)
                .unwrap(),
            RecordSanitizerV1::observation_v1().unwrap(),
        );
        let restarted = application
            .capture_observation(provider_capture_request(
                provider,
                &record_id,
                0,
                1,
                "stable application payload",
                None,
                ObservationCancellation::default(),
            ))
            .await
            .unwrap();
        match &restarted {
            CaptureObservationOutcome::Persisted { outcome, .. } => {
                assert!(
                    matches!(**outcome, ObservationPersistOutcome::ExactDuplicate(_)),
                    "{provider}: restart retry must be ExactDuplicate, got {outcome:?}"
                );
                assert_eq!(
                    outcome.receipt().observation().observation_id(),
                    &observation_id,
                    "{provider}"
                );
            }
            other => panic!("{provider}: restart retry must persist duplicate, got {other:?}"),
        }
        assert_eq!(
            table_counts(&runtime).await,
            counts_before_restart,
            "{provider}"
        );
        let replay_after_restart = application
            .replay_observations(ReplayObservationsRequest::new(
                ObservationReplayRequest::new(0, 10).unwrap(),
                ObservationCancellation::default(),
            ))
            .await
            .unwrap();
        assert_eq!(
            replay_after_restart.observations().len(),
            replay_before_restart.observations().len(),
            "{provider}"
        );
    }
}
