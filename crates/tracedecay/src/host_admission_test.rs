use std::sync::Arc;

use serde_json::json;
use tempfile::TempDir;
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    ProviderId, RetentionClass, SessionId,
};
use tracedecay_privacy::{ObservationRecordParseErrorV1, parse_normalized_observation_record_v1};
use tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1;
use tracedecay_store::ObservationPersistOutcome;

use tracedecay_host_admission::*;
use tracedecay_sessions::observation::{
    CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
};

fn host_capture_request(scope: ObservationScopeV1, record_id: &str) -> CaptureObservationRequest {
    // Host admission sanitizes through the single provider-neutral
    // observation path (RecordSanitizerV1::observation_v1), so the fixture
    // must present a canonical observation envelope rather than a raw
    // provider frame.
    let encoded = serde_json::to_vec(&json!({ "text": "host provenance fixture" })).unwrap();
    let range = ObservationSourceRangeV1::new(0, u64::try_from(encoded.len()).unwrap()).unwrap();
    let ordering_domain = ObservationOrderingDomainV1::SqliteRowId;
    // One source lane per record. Every fixture frame starts at offset 0 with
    // no expected cursor, so sharing a session would make the second distinct
    // record collide with the first record's advanced cursor and short-circuit
    // as `cursor_conflict` before the authority is ever consulted. Replays
    // still resolve as exact duplicates because they reuse `record_id`.
    let session_id = format!("session.{record_id}");
    let envelope_session = session_id.clone();
    let envelope_record = record_id.to_owned();
    let parsed =
        parse_normalized_observation_record_v1(&encoded, range, ordering_domain, move |native| {
            CanonicalObservationEnvelopeV1::new(
                ProviderId::new("claude").unwrap(),
                "message",
                ObservationId::new(envelope_record.clone()).unwrap(),
                CanonicalObservationRelationsV1::new(
                    SessionId::new(envelope_session.clone()).unwrap(),
                )
                .with_message_id(ObservationId::new(envelope_record.clone()).unwrap()),
                vec![CanonicalObservationFactV1::Message {
                    role: CanonicalMessageRoleV1::User,
                    content: native,
                    model: None,
                    timestamp: None,
                }],
                CanonicalObservationEvidenceV1::new(ordering_domain, range),
            )
            .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)
        })
        .unwrap();
    CaptureObservationRequest::new(
        parsed,
        ObservationIdentityMaterialV1::for_native_record(
            ObservationSourceIdentityV1::for_provider(
                ProviderId::new("claude").unwrap(),
                SessionId::new(session_id).unwrap(),
            )
            .unwrap(),
            scope,
            ObservationSourceGenerationV1::new(41).unwrap(),
            range,
            ordering_domain,
            ObservationId::new(record_id).unwrap(),
        )
        .unwrap(),
        None,
        RetentionClass::new("retention.host-provenance-test").unwrap(),
        ObservationCancellation::default(),
    )
    .unwrap()
}

#[tokio::test]
async fn projectless_profile_capture_uses_the_daemon_profile_worker_plan() {
    let root = TempDir::new().unwrap();
    let profile_root = root.path().join("profile");
    let identity =
        tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root).unwrap();
    let _daemon_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        identity.profile_root(),
        1,
        "projectless-host-admission-worker-plan-test",
    )
    .unwrap();
    let registry = tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1::open(identity.clone())
        .await
        .unwrap();
    let profile_registered = registry.profile_sessions().await.unwrap();
    let invocation = crate::daemon::invocation_state::DaemonInvocationState::default();

    let installed = invocation
        .install_profile_worker_plan_for_test(identity.clone(), profile_registered.clone())
        .await
        .unwrap();
    assert_eq!(
        tracedecay_code_index::parallelism::installed_worker_status(),
        Some(installed.status)
    );
    let background_cpu = installed.background_cpu;

    let facade = facade_with_background_cpu(
        &background_cpu,
        HostAdmissionAuthorities::for_profile(
            identity.brain_id().clone(),
            identity.profile_id().clone(),
            profile_registered.as_ref(),
        ),
    );
    let outcome = facade
        .capture_observation(host_capture_request(
            ObservationScopeV1::Profile,
            "host.projectless-worker-plan",
        ))
        .await
        .unwrap();
    let CaptureObservationOutcome::AcceptedForReplay { outcome, .. } = outcome else {
        panic!("registered projectless capture must commit through the mounted process authority");
    };
    assert!(
        matches!(*outcome, ObservationPersistOutcome::Committed(_)),
        "fresh projectless capture must be durably committed"
    );
}

fn facade_with_background_cpu<'a>(
    background_cpu: &Arc<ProcessBackgroundCpuV1>,
    authorities: HostAdmissionAuthorities<'a>,
) -> HostAdmissionFacade<'a> {
    HostAdmissionFacade::new(authorities.with_background_cpu(Arc::clone(background_cpu)))
}
