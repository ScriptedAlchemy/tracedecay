//! Pins the process background CPU authority as the gate on host observation
//! capture, in both directions.
//!
//! `HostAdmissionFacade::application` mounts the authority the composition
//! root injected through `HostAdmissionAuthorities::with_background_cpu` and
//! converts its absence into `Unavailable`/`background_cpu_unavailable`. That
//! makes the authority a hard precondition for ingest, not an optimization: a
//! facade composed without it rejects every frame before preparation and
//! stops ingesting host observations silently.
//!
//! In production the daemon is the sole owner — profile worker-plan admission
//! during daemon bootstrap (`install_profile_worker_plan` ->
//! `tracedecay_code_index::parallelism::install_worker_plan`) constructs it at
//! the effective indexing width and hands it to every ingest composition, and
//! `serve` and the host hooks route to that daemon rather than capturing
//! in-process. This test covers the contract from the capture side so a
//! regression that dropped the injection surfaces as a failing test rather
//! than silent ingest loss.

use std::num::NonZeroUsize;

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
use tracedecay_host_admission::{HostAdmissionAuthorities, HostAdmissionFacade};
use tracedecay_privacy::{ClaudeRecordParseErrorV1, parse_normalized_observation_record_v1};
use tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1;
use tracedecay_sessions::admission::{HostAdmissionScope, HostAdmissionStatus};
use tracedecay_sessions::observation::{
    CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
};

const PROVIDER: &str = "claude";
const FRAMES: usize = 2;

/// Kept explicit and small: this proves admission, never throughput. It must
/// not be read as a recommended or minimum production width.
const TEST_WIDTH: usize = 2;

fn capture_requests(session_id: &SessionId, count: usize) -> Vec<CaptureObservationRequest> {
    let mut requests = Vec::with_capacity(count);
    let mut offset = 0_u64;
    for ordinal in 0..u64::try_from(count).expect("batch fits u64") {
        let payload = json!({ "text": format!("capture frame {ordinal}") });
        let encoded = serde_json::to_vec(&payload).expect("payload encodes");
        let start = offset;
        let end = start + u64::try_from(encoded.len()).expect("frame length fits u64");
        offset = end;
        let range = ObservationSourceRangeV1::new(start, end).expect("valid source range");
        let ordering_domain = ObservationOrderingDomainV1::FileBytes;
        let record = ObservationId::new(format!("record.background-cpu-admission.{ordinal}"))
            .expect("valid record id");
        let envelope_session = session_id.clone();
        let envelope_record = record.clone();
        let parsed = parse_normalized_observation_record_v1(
            &encoded,
            range,
            ordering_domain,
            move |native| {
                CanonicalObservationEnvelopeV1::new(
                    ProviderId::new(PROVIDER).expect("valid provider"),
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
        .expect("record normalizes");
        let source = ObservationSourceIdentityV1::for_provider(
            ProviderId::new(PROVIDER).expect("valid provider"),
            session_id.clone(),
        )
        .expect("valid source identity");
        let expected_cursor = (start != 0).then(|| {
            ObservationSourceCursorV1::for_ordering(
                source.clone(),
                ObservationScopeV1::Profile,
                ObservationSourceGenerationV1::new(1).expect("valid generation"),
                ordering_domain,
                start,
            )
            .expect("valid expected cursor")
        });
        requests.push(
            CaptureObservationRequest::new(
                parsed,
                ObservationIdentityMaterialV1::for_native_record(
                    source,
                    ObservationScopeV1::Profile,
                    ObservationSourceGenerationV1::new(1).expect("valid generation"),
                    range,
                    ordering_domain,
                    record,
                )
                .expect("valid identity material"),
                expected_cursor,
                RetentionClass::new("retention.background-cpu-admission")
                    .expect("valid retention class"),
                ObservationCancellation::default(),
            )
            .expect("valid capture request"),
        );
    }
    requests
}

/// One facade composed without the authority, then the same authorities with
/// it injected: nothing process-global changes between the two halves.
#[tokio::test]
async fn capture_is_rejected_without_the_authority_and_admitted_with_it() {
    let tmp = TempDir::new().expect("temp profile root");
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .expect("registered profile runtime");
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let shard = &database.binding().shard_id;
    let authorities = HostAdmissionAuthorities::for_profile(
        shard.brain_id.clone(),
        shard.profile_id.clone(),
        database,
    );
    let facade = HostAdmissionFacade::new(authorities.clone());

    // ---- No authority injected: capture fails closed against a real store. ----
    let session_id = SessionId::new("session.background-cpu-admission").expect("valid session id");
    let rejected = facade
        .capture_observations(capture_requests(&session_id, FRAMES))
        .await
        .expect_err("capture must be rejected while no CPU authority is installed");
    assert_eq!(
        rejected.status,
        HostAdmissionStatus::Unavailable,
        "an absent CPU authority must fail capture closed"
    );
    assert_eq!(
        rejected.reason_code,
        Some("background_cpu_unavailable"),
        "the rejection must be the authority-missing reason, not an unrelated one"
    );

    // ---- Inject the authority the daemon's worker plan provides. ----
    let width = NonZeroUsize::new(TEST_WIDTH).expect("nonzero width");
    let installed = std::sync::Arc::new(ProcessBackgroundCpuV1::new(width));
    assert_eq!(installed.width(), width);
    let facade = HostAdmissionFacade::new(
        authorities.with_background_cpu(std::sync::Arc::clone(&installed)),
    );

    // ---- Injected: the same capture, same authorities, now admitted. ----
    let outcomes = facade
        .capture_observations(capture_requests(&session_id, FRAMES))
        .await
        .expect("capture must be admitted once the authority is installed");
    assert_eq!(
        outcomes.len(),
        FRAMES,
        "every frame must reach a capture route"
    );
    assert!(
        outcomes.iter().all(|outcome| matches!(
            outcome,
            CaptureObservationOutcome::Persisted { .. }
                | CaptureObservationOutcome::AcceptedForReplay { .. }
        )),
        "admitted frames must be persisted or accepted for replay: {outcomes:?}"
    );
    assert_eq!(
        installed.active_units(),
        0,
        "preparation must release every unit it took"
    );
}
