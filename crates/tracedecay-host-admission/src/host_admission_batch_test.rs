use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tempfile::TempDir;
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, DurableObservationV1,
    ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, PayloadReferenceV1, ProjectionGenerationId, ProviderId,
    RetentionClass, SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
    SanitizerDispositionV1, SensitivityV1, SessionId, UtcMicros,
};
use tracedecay_global_db::tests::harness::HostAdmissionTestRuntimeV1;
use tracedecay_private_fs::background_cpu::{
    ProcessBackgroundCpuV1, install_process_background_cpu, process_background_cpu,
};
use tracedecay_runtime_core::privacy::{
    ClaudeRecordParseErrorV1, parse_normalized_observation_record_v1,
};
use tracedecay_sessions::admission::{HostAdmission, HostAdmissionScope};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationPersistOutcome, ObservationWrite,
    build_observation_resolution_authorization_v1, build_observation_retrieval_anchor_v2,
};

use super::*;

const BATCH_PROVIDER: &str = "claude";
const BATCH_SIZE: usize = 8;

fn background_cpu_for_host_admission_test() -> Arc<ProcessBackgroundCpuV1> {
    process_background_cpu().unwrap_or_else(|| {
        install_process_background_cpu(NonZeroUsize::new(4).expect("nonzero test CPU width"))
            .expect("install canonical background CPU authority")
    })
}

fn committed_transactions(database: &tracedecay_global_db::RegisteredGlobalDb) -> u64 {
    database
        .runtime_client()
        .writer_telemetry_snapshot()
        .expect("registered database must expose rusqlite writer telemetry")
        .writer
        .expect("mounted writer must carry rusqlite writer telemetry")
        .transactions
        .committed_transactions
}

fn profile_facade<'a>(
    runtime: &'a HostAdmissionTestRuntimeV1,
) -> (
    HostAdmissionFacade<'a>,
    &'a tracedecay_global_db::RegisteredGlobalDb,
) {
    let _background_cpu = background_cpu_for_host_admission_test();
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let shard = &database.binding().shard_id;
    let facade = HostAdmissionFacade::new(HostAdmissionAuthorities::for_profile(
        shard.brain_id.clone(),
        shard.profile_id.clone(),
        database,
    ));
    (facade, database)
}

fn sequential_capture_requests(
    session_id: &SessionId,
    count: usize,
) -> Vec<CaptureObservationRequest> {
    let mut requests = Vec::with_capacity(count);
    let mut offset = 0_u64;
    for ordinal in 0..u64::try_from(count).expect("batch fits u64") {
        let payload = json!({ "text": format!("capture frame {ordinal}") });
        let encoded = serde_json::to_vec(&payload).unwrap();
        let start = offset;
        let end = start + u64::try_from(encoded.len()).unwrap();
        offset = end;
        let range = ObservationSourceRangeV1::new(start, end).unwrap();
        let ordering_domain = ObservationOrderingDomainV1::FileBytes;
        let record =
            ObservationId::new(format!("record.host-admission-capture.{ordinal}")).unwrap();
        let envelope_session = session_id.clone();
        let envelope_record = record.clone();
        let parsed = parse_normalized_observation_record_v1(
            &encoded,
            range,
            ordering_domain,
            move |native| {
                CanonicalObservationEnvelopeV1::new(
                    ProviderId::new(BATCH_PROVIDER).unwrap(),
                    "message",
                    envelope_record.clone(),
                    CanonicalObservationRelationsV1::new(envelope_session.clone())
                        .with_message_id(envelope_record.clone()),
                    vec![CanonicalObservationFactV1::Message {
                        role: CanonicalMessageRoleV1::Assistant,
                        content: native,
                        model: None,
                        timestamp: Some(1_750_000_000),
                    }],
                    CanonicalObservationEvidenceV1::new(ordering_domain, range),
                )
                .map_err(|_| ClaudeRecordParseErrorV1::NormalizationFailed)
            },
        )
        .unwrap();
        let source = ObservationSourceIdentityV1::for_provider(
            ProviderId::new(BATCH_PROVIDER).unwrap(),
            session_id.clone(),
        )
        .unwrap();
        let expected_cursor = (start != 0).then(|| {
            ObservationSourceCursorV1::for_ordering(
                source.clone(),
                ObservationScopeV1::Profile,
                ObservationSourceGenerationV1::new(1).unwrap(),
                ordering_domain,
                start,
            )
            .unwrap()
        });
        requests.push(
            CaptureObservationRequest::new(
                parsed,
                ObservationIdentityMaterialV1::for_native_record(
                    source,
                    ObservationScopeV1::Profile,
                    ObservationSourceGenerationV1::new(1).unwrap(),
                    range,
                    ordering_domain,
                    record,
                )
                .unwrap(),
                expected_cursor,
                RetentionClass::new("retention.host-admission-batch").unwrap(),
                ObservationCancellation::default(),
            )
            .unwrap(),
        );
    }
    requests
}

#[tokio::test]
async fn empty_capture_batch_opens_no_writer_transaction() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let (facade, database) = profile_facade(&runtime);
    let before = committed_transactions(database);
    let outcomes = HostAdmission::capture_observations(&facade, Vec::new())
        .await
        .unwrap();
    assert!(outcomes.is_empty());
    assert_eq!(committed_transactions(database), before);
}

#[tokio::test]
async fn mounted_capture_batch_reduces_writer_transactions() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let (facade, database) = profile_facade(&runtime);
    let session_id = SessionId::new("session.host-admission-capture.batch").unwrap();
    let requests = sequential_capture_requests(&session_id, BATCH_SIZE);
    let before = committed_transactions(database);
    let outcomes = HostAdmission::capture_observations(&facade, requests)
        .await
        .unwrap();
    assert_eq!(outcomes.len(), BATCH_SIZE);
    assert!(outcomes.iter().all(|outcome| {
        matches!(
            outcome,
            CaptureObservationOutcome::Persisted { .. }
                | CaptureObservationOutcome::AcceptedForReplay { .. }
        )
    }));
    let committed = committed_transactions(database) - before;
    assert!(
        committed < BATCH_SIZE as u64,
        "trait capture_observations must open fewer writer transactions than frames: committed_transactions={committed} frames={BATCH_SIZE}"
    );
    assert_eq!(
        committed, 2,
        "one observation batch plus one external-source batch must commit"
    );
}

#[tokio::test]
async fn production_facade_preparation_waits_for_shared_background_cpu() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let (facade, _) = profile_facade(&runtime);
    let authority = background_cpu_for_host_admission_test();
    let permits = (0..authority.width().get())
        .map(|_| authority.acquire())
        .collect::<Vec<_>>();
    let session_id = SessionId::new("session.host-admission-capture.shared-cpu").unwrap();
    let capture = facade.capture_observations(sequential_capture_requests(&session_id, 2));
    tokio::pin!(capture);

    assert!(
        tokio::time::timeout(Duration::from_millis(30), capture.as_mut())
            .await
            .is_err(),
        "the production facade must mount preparation beneath the canonical CPU authority"
    );
    drop(permits);

    let outcomes = capture.await.unwrap();
    assert_eq!(outcomes.len(), 2);
}

fn fixture_receipt(receipt_id: &str, payload: &serde_json::Value) -> SanitizationReceiptV1 {
    SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(receipt_id).unwrap(),
            tracedecay_domain::ComponentVersion::new("sanitizer.host-admission-batch.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(payload).unwrap()),
    )
    .unwrap()
}

fn sequential_observation(
    session_id: &SessionId,
    ordinal: u64,
    text: &str,
) -> DurableObservationV1 {
    let provider = ProviderId::new(BATCH_PROVIDER).unwrap();
    let source =
        ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone()).unwrap();
    let range = ObservationSourceRangeV1::new(ordinal, ordinal + 1).unwrap();
    let record = ObservationId::new(format!(
        "record.host-admission-persist.{}.{ordinal}",
        session_id.as_str()
    ))
    .unwrap();
    let relations = CanonicalObservationRelationsV1::new(session_id.clone()).with_message_id(
        ObservationId::new(format!(
            "message.host-admission-persist.{}.{ordinal}",
            session_id.as_str()
        ))
        .unwrap(),
    );
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider,
        "message",
        record.clone(),
        relations,
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: json!({"text": text}),
            model: None,
            timestamp: Some(1_750_000_000),
        }],
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::FileBytes, range),
    )
    .unwrap();
    let payload = serde_json::to_value(envelope).unwrap();
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(1).unwrap(),
        range,
        ObservationOrderingDomainV1::FileBytes,
        record,
    )
    .unwrap();
    DurableObservationV1::new(
        identity,
        fixture_receipt(
            &format!(
                "receipt.host-admission-persist.{}.{ordinal}",
                session_id.as_str()
            ),
            &payload,
        ),
        RetentionClass::new("retention.host-admission-batch").unwrap(),
        payload,
    )
    .unwrap()
}

fn anchored_write(
    observation: DurableObservationV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
) -> AnchoredObservationWrite {
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        observation.identity().generation(),
        observation.identity().ordering_domain(),
        observation.identity().position().end(),
    )
    .unwrap();
    let write = ObservationWrite::new(observation, expected_cursor, next_cursor).unwrap();
    let projection_generation =
        ProjectionGenerationId::new("projection.host-admission-batch.v1").unwrap();
    let authorization =
        build_observation_resolution_authorization_v1(write.observation(), "host-admission-batch")
            .unwrap();
    let anchor = build_observation_retrieval_anchor_v2(
        write.observation(),
        projection_generation.clone(),
        UtcMicros(1),
        authorization,
    )
    .unwrap();
    AnchoredObservationWrite::new(write, anchor, projection_generation).unwrap()
}

fn sequential_writes(session_id: &SessionId, count: usize) -> Vec<AnchoredObservationWrite> {
    let mut writes = Vec::with_capacity(count);
    let mut expected = None;
    for ordinal in 0..u64::try_from(count).expect("batch fits u64") {
        let observation = sequential_observation(session_id, ordinal, &format!("frame {ordinal}"));
        let write = anchored_write(observation, expected);
        expected = Some(write.next_cursor().clone());
        writes.push(write);
    }
    writes
}

fn colliding_rewrite(
    session_id: &SessionId,
    expected_cursor: Option<ObservationSourceCursorV1>,
) -> AnchoredObservationWrite {
    let provider = ProviderId::new(BATCH_PROVIDER).unwrap();
    let source =
        ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone()).unwrap();
    let range = ObservationSourceRangeV1::new(0, 1).unwrap();
    let record = ObservationId::new(format!(
        "record.host-admission-persist.{}.0",
        session_id.as_str()
    ))
    .unwrap();
    let relations = CanonicalObservationRelationsV1::new(session_id.clone()).with_message_id(
        ObservationId::new(format!(
            "message.host-admission-persist.{}.0",
            session_id.as_str()
        ))
        .unwrap(),
    );
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider,
        "message",
        record.clone(),
        relations,
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: json!({"text": "rewritten colliding frame"}),
            model: None,
            timestamp: Some(1_750_000_000),
        }],
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::FileBytes, range),
    )
    .unwrap();
    let payload = serde_json::to_value(envelope).unwrap();
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(2).unwrap(),
        range,
        ObservationOrderingDomainV1::FileBytes,
        record,
    )
    .unwrap();
    let observation = DurableObservationV1::new(
        identity,
        fixture_receipt(
            &format!(
                "receipt.host-admission-persist.{}.collision",
                session_id.as_str()
            ),
            &payload,
        ),
        RetentionClass::new("retention.host-admission-batch").unwrap(),
        payload,
    )
    .unwrap();
    anchored_write(observation, expected_cursor)
}

#[tokio::test]
async fn empty_persist_batch_opens_no_writer_transaction() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let (facade, database) = profile_facade(&runtime);
    let before = committed_transactions(database);
    let outcomes = facade
        .persist_observations(BATCH_PROVIDER, &ObservationScopeV1::Profile, Vec::new())
        .await
        .unwrap();
    assert!(outcomes.is_empty());
    assert_eq!(committed_transactions(database), before);
}

#[tokio::test]
async fn n_persist_observation_calls_open_n_writer_transactions() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let (facade, database) = profile_facade(&runtime);
    let session_id = SessionId::new("session.host-admission-persist.one-by-one").unwrap();
    let writes = sequential_writes(&session_id, BATCH_SIZE);
    let before = committed_transactions(database);
    for write in writes {
        assert!(matches!(
            facade
                .persist_observation(BATCH_PROVIDER, &ObservationScopeV1::Profile, write)
                .await
                .unwrap(),
            ObservationPersistOutcome::Committed(_)
        ));
    }
    let committed = committed_transactions(database) - before;
    assert_eq!(
        committed, BATCH_SIZE as u64,
        "one persist_observation still opens one writer transaction: committed_transactions={committed} frames={BATCH_SIZE}"
    );
}

#[tokio::test]
async fn persist_observations_opens_one_writer_transaction_for_the_batch() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let (facade, database) = profile_facade(&runtime);
    let session_id = SessionId::new("session.host-admission-persist.one-txn").unwrap();
    let writes = sequential_writes(&session_id, BATCH_SIZE);
    let before = committed_transactions(database);
    let outcomes = facade
        .persist_observations(BATCH_PROVIDER, &ObservationScopeV1::Profile, writes)
        .await
        .unwrap();
    assert_eq!(outcomes.len(), BATCH_SIZE);
    assert!(
        outcomes
            .iter()
            .all(|outcome| matches!(outcome, ObservationPersistOutcome::Committed(_)))
    );
    let committed = committed_transactions(database) - before;
    assert_eq!(
        committed, 1,
        "persist_observations must open one writer transaction: committed_transactions={committed} frames={BATCH_SIZE}"
    );
}

#[tokio::test]
async fn persist_observations_keeps_cursor_cas_collision_and_file_identity() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let (facade, _database) = profile_facade(&runtime);
    let session_id = SessionId::new("session.host-admission-persist.authority").unwrap();
    let writes = sequential_writes(&session_id, 2);
    let first = writes[0].clone();
    let second = writes[1].clone();
    facade
        .persist_observation(BATCH_PROVIDER, &ObservationScopeV1::Profile, first.clone())
        .await
        .unwrap();

    let stale = facade
        .persist_observations(
            BATCH_PROVIDER,
            &ObservationScopeV1::Profile,
            vec![first.clone()],
        )
        .await
        .unwrap();
    assert!(matches!(
        stale.as_slice(),
        [ObservationPersistOutcome::ExactDuplicate(_)]
    ));

    let cas_error = facade
        .persist_observations(
            BATCH_PROVIDER,
            &ObservationScopeV1::Profile,
            vec![anchored_write(
                sequential_observation(&session_id, 2, "stale expected"),
                Some(second.next_cursor().clone()),
            )],
        )
        .await
        .unwrap_err();
    assert_eq!(cas_error.reason_code, Some("cursor_conflict"));

    let collision = facade
        .persist_observations(
            BATCH_PROVIDER,
            &ObservationScopeV1::Profile,
            vec![colliding_rewrite(
                &session_id,
                Some(first.next_cursor().clone()),
            )],
        )
        .await
        .unwrap_err();
    assert_eq!(
        collision.reason_code,
        Some("observation_identity_collision")
    );

    let identity_session = SessionId::new("session.host-admission-persist.file-identity").unwrap();
    let identity_writes = sequential_writes(&identity_session, 2);
    let identity_first = identity_writes[0].clone();
    let identity_second = identity_writes[1].clone();
    facade
        .persist_observation(BATCH_PROVIDER, &ObservationScopeV1::Profile, identity_first)
        .await
        .unwrap();
    let resume = identity_second
        .next_cursor()
        .clone()
        .with_resume_checkpoint(0xfeed_face, 0xcafe_babe);
    let resumed = ObservationWrite::new(
        identity_second.observation().clone(),
        identity_second.expected_cursor().cloned(),
        resume,
    )
    .unwrap();
    let projection = identity_second.projection_generation().clone();
    let resumed = AnchoredObservationWrite::new(
        resumed,
        identity_second.retrieval_anchor().clone(),
        projection,
    )
    .unwrap();
    let outcomes = facade
        .persist_observations(BATCH_PROVIDER, &ObservationScopeV1::Profile, vec![resumed])
        .await
        .unwrap();
    assert!(matches!(
        outcomes.as_slice(),
        [ObservationPersistOutcome::Committed(_)]
    ));
    let cursor = facade
        .get_source_cursor(
            identity_second.observation().source(),
            identity_second.observation().scope(),
        )
        .await
        .unwrap()
        .expect("committed cursor");
    assert_eq!(cursor.file_identity(), Some(0xfeed_face));
    assert_eq!(cursor.resume_fingerprint(), Some(0xcafe_babe));
}

#[tokio::test]
async fn n_capture_observation_calls_open_at_least_n_writer_transactions() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let (facade, database) = profile_facade(&runtime);
    let session_id = SessionId::new("session.host-admission-capture.one-by-one").unwrap();
    let requests = sequential_capture_requests(&session_id, BATCH_SIZE);
    let before = committed_transactions(database);
    for request in requests {
        HostAdmission::capture_observation(&facade, request)
            .await
            .unwrap();
    }
    let committed = committed_transactions(database) - before;
    assert!(
        committed >= BATCH_SIZE as u64,
        "N capture_observation calls must open at least N writer transactions: committed_transactions={committed} frames={BATCH_SIZE}"
    );
}
