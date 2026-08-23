use serde_json::json;
use tempfile::TempDir;
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, ProviderId, RetentionClass, SessionId,
};
use tracedecay_global_db::tests::harness::HostAdmissionTestRuntimeV1;
use tracedecay_runtime_core::privacy::{
    ClaudeRecordParseErrorV1, parse_normalized_observation_record_v1,
};
use tracedecay_sessions::admission::HostAdmission;

use super::*;

const BATCH_PROVIDER: &str = "claude";
const BATCH_SIZE: usize = 8;

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
    assert_eq!(
        committed_transactions(database) - before,
        BATCH_SIZE as u64 + 1,
        "one observation batch plus one external-source projection per receipt must commit"
    );
}
