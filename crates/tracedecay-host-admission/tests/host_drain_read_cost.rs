//! A host drain's read cost follows the work it admits, not the store size.
//!
//! Every project-scope drain also converges the session Git evidence past its
//! durable frontier. That pass used to aggregate every session over the whole
//! message index before filtering to the frontier, so on a long-lived profile
//! each hook-driven drain re-read the session store: the operator daemon's
//! SQLite threads read terabytes from `sessions.db` in hours. This measures
//! the process's own read bytes around one single-message capture and drain,
//! on the same store before and after it grows eightfold.

#![cfg(target_os = "linux")]

use std::num::NonZeroUsize;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use serde_json::json;
use tempfile::TempDir;
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, ProjectId, ProviderId, RetentionClass, SessionId,
};
use tracedecay_global_db::tests::harness::HostAdmissionTestRuntimeV1;
use tracedecay_host_admission::{HostAdmissionAuthorities, HostAdmissionFacade};
use tracedecay_privacy::{ObservationRecordParseErrorV1, parse_normalized_observation_record_v1};
use tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1;
use tracedecay_sessions::admission::{HostAdmission, HostAdmissionScope};
use tracedecay_sessions::observation::{
    CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
};
use tracedecay_sessions::repository_provenance::RepositoryProvenanceAdmissionContext;

const PROVIDER: &str = "codex";
const MESSAGES_PER_SESSION: u64 = 40;
const BASE_SESSIONS: u64 = 40;
const GROWN_SESSIONS: u64 = 8 * BASE_SESSIONS;
const SEED_TIMESTAMP: i64 = 1_780_000_000;
const DRAIN_WINDOW: usize = 4_096;

fn process_read_bytes() -> u64 {
    let io = std::fs::read_to_string("/proc/self/io").unwrap();
    io.lines()
        .find_map(|line| line.strip_prefix("rchar: "))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

struct SessionCursor {
    session_id: SessionId,
    next_ordinal: u64,
    next_offset: u64,
}

fn session_cursor(index: u64) -> SessionCursor {
    SessionCursor {
        session_id: SessionId::new(format!("session.drain-read-cost.{index:05}")).unwrap(),
        next_ordinal: 0,
        next_offset: 0,
    }
}

fn message_requests(
    project: &Path,
    scope: &ObservationScopeV1,
    cursor: &mut SessionCursor,
    count: u64,
    timestamp: i64,
) -> Vec<CaptureObservationRequest> {
    let source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new(PROVIDER).unwrap(),
        cursor.session_id.clone(),
    )
    .unwrap();
    let project_path = project.to_string_lossy().into_owned();
    (0..count)
        .map(|_| {
            let ordinal = cursor.next_ordinal;
            cursor.next_ordinal += 1;
            let payload = json!({ "text": format!("drain read cost frame {ordinal}") });
            let encoded = serde_json::to_vec(&payload).unwrap();
            let start = cursor.next_offset;
            let end = start + u64::try_from(encoded.len()).unwrap();
            cursor.next_offset = end;
            let range = ObservationSourceRangeV1::new(start, end).unwrap();
            let ordering = ObservationOrderingDomainV1::FileBytes;
            let record = ObservationId::new(format!(
                "{}.message.{ordinal:05}",
                cursor.session_id.as_str()
            ))
            .unwrap();
            let session_id = cursor.session_id.clone();
            let envelope_record = record.clone();
            let envelope_project = project_path.clone();
            let message_timestamp = timestamp + i64::try_from(ordinal).unwrap();
            let parsed =
                parse_normalized_observation_record_v1(&encoded, range, ordering, move |native| {
                    CanonicalObservationEnvelopeV1::new(
                        ProviderId::new(PROVIDER).unwrap(),
                        "message",
                        envelope_record.clone(),
                        CanonicalObservationRelationsV1::new(session_id.clone())
                            .with_message_id(envelope_record.clone()),
                        vec![
                            CanonicalObservationFactV1::Session {
                                project_path: Some(envelope_project.clone()),
                                location_path: Some(envelope_project.clone()),
                                transcript_path: None,
                                title: None,
                                started_at: None,
                                ended_at: None,
                                source: Some("codex_rollout".to_owned()),
                                native_source: Some("codex".to_owned()),
                                profile: None,
                                location_provenance: Some("rollout_context".to_owned()),
                            },
                            CanonicalObservationFactV1::Message {
                                role: CanonicalMessageRoleV1::Assistant,
                                content: native,
                                model: None,
                                timestamp: Some(message_timestamp),
                            },
                        ],
                        CanonicalObservationEvidenceV1::new(ordering, range)
                            .with_native_timestamp(message_timestamp),
                    )
                    .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)
                })
                .unwrap();
            let expected_cursor = (start != 0).then(|| {
                ObservationSourceCursorV1::for_ordering(
                    source.clone(),
                    scope.clone(),
                    ObservationSourceGenerationV1::new(1).unwrap(),
                    ordering,
                    start,
                )
                .unwrap()
            });
            CaptureObservationRequest::new(
                parsed,
                ObservationIdentityMaterialV1::for_native_record(
                    source.clone(),
                    scope.clone(),
                    ObservationSourceGenerationV1::new(1).unwrap(),
                    range,
                    ordering,
                    record,
                )
                .unwrap(),
                expected_cursor,
                RetentionClass::new("retention.drain-read-cost").unwrap(),
                ObservationCancellation::default(),
            )
            .unwrap()
        })
        .collect()
}

async fn capture(facade: &HostAdmissionFacade<'_>, requests: Vec<CaptureObservationRequest>) {
    let expected = requests.len();
    let outcomes = facade.capture_observations(requests).await.unwrap();
    assert_eq!(outcomes.len(), expected);
    assert!(outcomes.iter().all(|outcome| matches!(
        outcome,
        CaptureObservationOutcome::Persisted { .. }
            | CaptureObservationOutcome::AcceptedForReplay { .. }
    )));
}

async fn drain(facade: &HostAdmissionFacade<'_>, scope: &ObservationScopeV1) -> u64 {
    let mut projected = 0;
    loop {
        let outcome = facade
            .drain_projection_queue(
                PROVIDER,
                scope,
                &ObservationCancellation::default(),
                DRAIN_WINDOW,
            )
            .await
            .unwrap();
        projected += outcome.projected;
        if !outcome.deferred {
            return projected;
        }
    }
}

/// Seeds `sessions` with a full history each and returns the first session's
/// cursor, which the probes extend.
async fn seed_sessions(
    facade: &HostAdmissionFacade<'_>,
    project: &Path,
    scope: &ObservationScopeV1,
    sessions: std::ops::Range<u64>,
) -> SessionCursor {
    let mut first = None;
    for index in sessions {
        let mut cursor = session_cursor(index);
        let timestamp = SEED_TIMESTAMP + i64::try_from(index * MESSAGES_PER_SESSION).unwrap();
        capture(
            facade,
            message_requests(project, scope, &mut cursor, MESSAGES_PER_SESSION, timestamp),
        )
        .await;
        first.get_or_insert(cursor);
    }
    drain(facade, scope).await;
    first.unwrap()
}

/// Captures and drains one new message on an existing session, returning the
/// bytes the process read to do it.
async fn probe_one_message(
    facade: &HostAdmissionFacade<'_>,
    project: &Path,
    scope: &ObservationScopeV1,
    cursor: &mut SessionCursor,
    timestamp: i64,
) -> u64 {
    let probe_ordinal = cursor.next_ordinal;
    let before = process_read_bytes();
    let requests = message_requests(project, scope, cursor, 1, timestamp);
    capture(facade, requests).await;
    let projected = drain(facade, scope).await;
    let read = process_read_bytes() - before;
    assert_eq!(projected, 1, "the probe message must project in its drain");
    let message_id = format!("{}.message.{probe_ordinal:05}", cursor.session_id.as_str());
    assert!(
        facade
            .has_session_message(scope, PROVIDER, &message_id)
            .await
            .unwrap()
    );
    read
}

fn run_git(project: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(project)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?} failed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_message_drain_reads_do_not_scale_with_the_session_store() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("drain-read-cost");
    std::fs::create_dir_all(&project).unwrap();
    run_git(&project, &["init", "-b", "main"]);
    let project_id = ProjectId::new("project.drain-read-cost").unwrap();
    assert!(
        tracedecay_runtime_core::storage::write_repository_identity_marker(
            &project,
            project_id.as_str(),
        )
        .unwrap()
    );
    let marker = tracedecay_runtime_core::storage::read_repository_identity_marker(&project)
        .unwrap()
        .unwrap();
    let runtime = HostAdmissionTestRuntimeV1::project(
        tmp.path().join("profile"),
        &project,
        project_id.clone(),
    )
    .await
    .unwrap();
    let database = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let provenance = RepositoryProvenanceAdmissionContext::from_authoritative_project_marker(
        &project,
        &project_id,
        &marker,
    )
    .unwrap();
    let shard = &database.binding().shard_id;
    let facade = HostAdmissionFacade::new(
        HostAdmissionAuthorities::for_project(
            shard.brain_id.clone(),
            shard.profile_id.clone(),
            project_id.clone(),
            database,
        )
        .with_repository_provenance(provenance)
        .with_background_cpu(Arc::new(ProcessBackgroundCpuV1::new(
            NonZeroUsize::new(4).unwrap(),
        ))),
    );
    let scope = ObservationScopeV1::Project { project_id };

    let mut probed = seed_sessions(&facade, &project, &scope, 0..BASE_SESSIONS).await;
    let probe_timestamp = SEED_TIMESTAMP + 10_000_000;
    let base_read =
        probe_one_message(&facade, &project, &scope, &mut probed, probe_timestamp).await;

    seed_sessions(&facade, &project, &scope, BASE_SESSIONS..GROWN_SESSIONS).await;
    assert_eq!(
        runtime
            .project_session_message_count_for_test()
            .await
            .unwrap(),
        i64::try_from(GROWN_SESSIONS * MESSAGES_PER_SESSION + 1).unwrap(),
    );
    let grown_read =
        probe_one_message(&facade, &project, &scope, &mut probed, probe_timestamp + 1).await;

    eprintln!("single-message drain read bytes: base={base_read} grown={grown_read}");
    assert!(
        grown_read <= base_read * 2,
        "an 8x larger session store must not double one message's drain reads: \
         base={base_read} grown={grown_read}"
    );
}
