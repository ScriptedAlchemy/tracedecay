use std::io::Write as _;
use std::num::NonZeroUsize;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tempfile::TempDir;
use tracedecay_domain::{
    CanonicalGitEvidenceKindV1, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
    CanonicalObservationEvidenceV1, CanonicalObservationFactV1, CanonicalObservationRelationsV1,
    DurableObservationV1, ObservationId, ObservationIdentityMaterialV1,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    PayloadReferenceV1, ProjectId, ProjectionGenerationId, ProviderId, RetentionClass,
    SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1,
    SensitivityV1, SessionId, UtcMicros,
};
use tracedecay_global_db::tests::harness::{HostAdmissionTestRuntimeV1, writer_telemetry};
use tracedecay_privacy::{ObservationRecordParseErrorV1, parse_normalized_observation_record_v1};
use tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1;
use tracedecay_runtime_core::db::engine::params;
use tracedecay_runtime_core::resident_memory::{
    DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1, ProcessResidentMemoryV1,
};
use tracedecay_sessions::admission::{HostAdmission, HostAdmissionScope};
use tracedecay_sessions::runtime::hosts::codex::{
    CodexDiscoveryHub, codex_observation_source_v2,
    try_admit_codex_jsonl_observations_for_profile_with_admission,
};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationCoverageReason, ObservationCursorAdvance,
    ObservationPersistOutcome, ObservationReplayRequest, ObservationWrite,
    build_observation_resolution_authorization_v1, build_observation_retrieval_anchor,
    runtime::DEFAULT_MAX_READERS_PER_HOT_SHARD,
};

use super::*;

const BATCH_PROVIDER: &str = "claude";
const BATCH_SIZE: usize = 8;

/// An isolated authority per facade: nothing here is process-global, so a
/// test that saturates its width cannot gate a sibling test.
pub(super) fn background_cpu_for_host_admission_test() -> Arc<ProcessBackgroundCpuV1> {
    Arc::new(ProcessBackgroundCpuV1::new(
        NonZeroUsize::new(4).expect("nonzero test CPU width"),
    ))
}

fn committed_transactions(database: &tracedecay_global_db::RegisteredGlobalDb) -> u64 {
    writer_telemetry(database)
        .transactions
        .committed_transactions
}

fn profile_facade<'a>(
    runtime: &'a HostAdmissionTestRuntimeV1,
) -> (
    HostAdmissionFacade<'a>,
    &'a tracedecay_global_db::RegisteredGlobalDb,
) {
    profile_facade_with_background_cpu(runtime, background_cpu_for_host_admission_test())
}

fn profile_facade_with_background_cpu<'a>(
    runtime: &'a HostAdmissionTestRuntimeV1,
    background_cpu: Arc<ProcessBackgroundCpuV1>,
) -> (
    HostAdmissionFacade<'a>,
    &'a tracedecay_global_db::RegisteredGlobalDb,
) {
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let shard = &database.binding().shard_id;
    let facade = HostAdmissionFacade::new(
        HostAdmissionAuthorities::for_profile(
            shard.brain_id.clone(),
            shard.profile_id.clone(),
            database,
        )
        .with_background_cpu(background_cpu),
    );
    (facade, database)
}

fn sequential_capture_requests(
    session_id: &SessionId,
    count: usize,
) -> Vec<CaptureObservationRequest> {
    generation_capture_requests(session_id, count, 1, None)
}

/// The same records under one source generation; `previous` is the cursor
/// the generation's first record must replace.
fn generation_capture_requests(
    session_id: &SessionId,
    count: usize,
    generation: u64,
    previous: Option<ObservationSourceCursorV1>,
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
                .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)
            },
        )
        .unwrap();
        let source = ObservationSourceIdentityV1::for_provider(
            ProviderId::new(BATCH_PROVIDER).unwrap(),
            session_id.clone(),
        )
        .unwrap();
        let expected_cursor = if start == 0 {
            previous.clone()
        } else {
            Some(
                ObservationSourceCursorV1::for_ordering(
                    source.clone(),
                    ObservationScopeV1::Profile,
                    ObservationSourceGenerationV1::new(generation).unwrap(),
                    ordering_domain,
                    start,
                )
                .unwrap(),
            )
        };
        requests.push(
            CaptureObservationRequest::new(
                parsed,
                ObservationIdentityMaterialV1::for_native_record(
                    source,
                    ObservationScopeV1::Profile,
                    ObservationSourceGenerationV1::new(generation).unwrap(),
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

#[asupersync::test]
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

#[asupersync::test]
async fn exact_parse_offset_replacement_keeps_an_unordered_fingerprint_and_foreign_winner() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let (facade, _) = profile_facade(&runtime);
    let scope = ObservationScopeV1::Profile;
    let key = "host-frontier://claude/refused";
    let first = ParseOffset {
        byte_offset: 100,
        mtime: 900,
        file_id: 7,
    };
    let later = ParseOffset {
        byte_offset: 200,
        mtime: 100,
        file_id: 7,
    };
    HostAdmission::replace_parse_offset(&facade, &scope, key, ParseOffset::default(), first)
        .await
        .unwrap();
    HostAdmission::replace_parse_offset(&facade, &scope, key, first, later)
        .await
        .unwrap();
    assert_eq!(
        HostAdmission::get_parse_offset(&facade, &scope, key)
            .await
            .unwrap(),
        Some(later)
    );
    let stale = HostAdmission::replace_parse_offset(&facade, &scope, key, first, first)
        .await
        .unwrap_err();
    assert_eq!(stale.reason_code, Some("parse_offset_conflict"));
    assert_eq!(
        HostAdmission::get_parse_offset(&facade, &scope, key)
            .await
            .unwrap(),
        Some(later)
    );
    let foreign = ParseOffset {
        byte_offset: 50,
        mtime: u64::MAX,
        file_id: 8,
    };
    HostAdmission::replace_parse_offset(&facade, &scope, key, later, foreign)
        .await
        .unwrap();
    assert_eq!(
        HostAdmission::replace_parse_offset(&facade, &scope, key, later, first)
            .await
            .unwrap_err()
            .reason_code,
        Some("parse_offset_conflict")
    );
    assert_eq!(
        HostAdmission::get_parse_offset(&facade, &scope, key)
            .await
            .unwrap(),
        Some(foreign)
    );
}

#[asupersync::test]
async fn jsonl_capture_resumes_after_durable_scope_coverage_through_a_fresh_facade() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("workspace");
    let registered = tmp.path().join("registered");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&registered).unwrap();
    let path = tmp.path().join("rollout.jsonl");
    let session = "session.host-admission.jsonl-coverage";
    let lines = [
        json!({"timestamp": "2026-01-01T00:00:00Z", "type": "session_meta",
            "payload": {"id": session, "cwd": workspace}}),
        json!({"timestamp": "2026-01-01T00:00:01Z", "type": "turn_context",
            "payload": {"cwd": registered}}),
        json!({"timestamp": "2026-01-01T00:00:02Z", "type": "event_msg",
            "payload": user_message_payload("project-owned message")}),
    ];
    let contents = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&path, &contents).unwrap();
    CodexDiscoveryHub::default()
        .configure_preparation_resources(
            Arc::new(ProcessResidentMemoryV1::new(
                DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1,
            )),
            background_cpu_for_host_admission_test(),
        )
        .unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path().join("profile"))
        .await
        .unwrap();
    let (facade, database) = profile_facade(&runtime);
    let roots = [registered];
    let first = try_admit_codex_jsonl_observations_for_profile_with_admission(
        &path, None, &roots, &facade, None,
    )
    .await
    .unwrap();
    assert_eq!(first.frames_persisted, 1);
    assert_eq!(first.frames_skipped, 2);
    let source = codex_observation_source_v2(session).unwrap();
    let scope = ObservationScopeV1::Profile;
    let covered = HostAdmission::get_source_cursor(&facade, &source, &scope)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(covered.position(), contents.len() as u64);
    assert!(covered.resume_fingerprint().is_some());
    let store = database.observation_store();
    assert_eq!(
        store
            .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
            .await
            .unwrap()
            .len(),
        1
    );
    drop(facade);
    let (fresh, _) = profile_facade(&runtime);
    let settled = try_admit_codex_jsonl_observations_for_profile_with_admission(
        &path, None, &roots, &fresh, None,
    )
    .await
    .unwrap();
    assert_eq!(settled.bytes_consumed, 0);
    assert_eq!(
        HostAdmission::get_source_cursor(&fresh, &source, &scope)
            .await
            .unwrap(),
        Some(covered)
    );
    let appended = [
        json!({"timestamp": "2026-01-01T00:00:03Z", "type": "turn_context",
            "payload": {"cwd": workspace}}),
        json!({"timestamp": "2026-01-01T00:00:04Z", "type": "event_msg",
            "payload": user_message_payload("profile-owned later message")}),
    ]
    .iter()
    .map(ToString::to_string)
    .collect::<Vec<_>>()
    .join("\n")
        + "\n";
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(appended.as_bytes())
        .unwrap();
    let resumed = try_admit_codex_jsonl_observations_for_profile_with_admission(
        &path, None, &roots, &fresh, None,
    )
    .await
    .unwrap();
    assert_eq!(resumed.frames_persisted, 2);
    assert_eq!(resumed.bytes_consumed, appended.len() as u64);
    assert_eq!(
        HostAdmission::get_source_cursor(&fresh, &source, &scope)
            .await
            .unwrap()
            .unwrap()
            .position(),
        (contents.len() + appended.len()) as u64
    );
    assert_eq!(
        store
            .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
            .await
            .unwrap()
            .len(),
        3
    );
    let mut previous = HostAdmission::get_source_cursor(&fresh, &source, &scope)
        .await
        .unwrap()
        .unwrap();
    let mut saw_lower_fingerprint = false;
    for ordinal in 0..16 {
        let line = format!(
            "{}\n",
            json!({"timestamp": "2026-01-01T00:00:05Z", "type": "event_msg",
                "payload": user_message_payload(format!("append {ordinal}"))})
        );
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(line.as_bytes())
            .unwrap();
        let pass = try_admit_codex_jsonl_observations_for_profile_with_admission(
            &path, None, &roots, &fresh, None,
        )
        .await
        .unwrap();
        assert_eq!(pass.frames_persisted, 1);
        let current = HostAdmission::get_source_cursor(&fresh, &source, &scope)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(current.position(), previous.position() + line.len() as u64);
        assert_eq!(current.generation(), previous.generation());
        if current.resume_fingerprint().unwrap() < previous.resume_fingerprint().unwrap() {
            saw_lower_fingerprint = true;
            break;
        }
        previous = current;
    }
    assert!(
        saw_lower_fingerprint,
        "the actual byte-frontier must commit even when its prefix fingerprint decreases"
    );
}

#[asupersync::test]
async fn refused_record_coverage_conflict_cannot_authorize_a_later_capture() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let (facade, database) = profile_facade(&runtime);
    let session = SessionId::new("session.host-admission.coverage-conflict").unwrap();
    let mut requests = sequential_capture_requests(&session, 3);
    let first = requests.remove(0);
    let source = first.identity().source().clone();
    let scope = first.scope().clone();
    let generation = first.identity().generation();
    let original = HostAdmission::capture_observation(&facade, first)
        .await
        .unwrap();
    let original_id = match original {
        CaptureObservationOutcome::Persisted { outcome, .. }
        | CaptureObservationOutcome::AcceptedForReplay { outcome, .. } => {
            outcome.receipt().observation().observation_id().clone()
        }
        other => panic!("first record must persist: {other:?}"),
    };
    let before = HostAdmission::get_source_cursor(&facade, &source, &scope)
        .await
        .unwrap();
    let refused_range = requests.remove(0).identity().position();
    let later = requests.remove(0);
    let advance = ObservationCursorAdvance::for_ordering(
        source.clone(),
        scope.clone(),
        generation,
        ObservationOrderingDomainV1::FileBytes,
        before.clone(),
        refused_range,
        ObservationCoverageReason::AdmissionRefused,
    )
    .unwrap();
    let source_json = serde_json::to_string(&source).unwrap();
    let scope_json = serde_json::to_string(&scope).unwrap();
    let coverage_json = serde_json::to_string(&advance.coverage()).unwrap();
    let transaction = database.begin_write_transaction().await.unwrap();
    transaction
        .execute(
            "INSERT INTO source_cursor_advances
         (source_json, scope_json, coverage_json, reason, receipt_id)
         VALUES (?1, ?2, ?3, 'blank_frame', NULL)",
            params![
                source_json.as_str(),
                scope_json.as_str(),
                coverage_json.as_str()
            ],
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    let failed = facade
        .advance_non_durable_source_cursor(advance.clone(), ObservationCancellation::default())
        .await
        .unwrap_err();
    assert_eq!(
        failed.reason_code,
        Some("observation_cursor_advance_ledger_disagreement")
    );
    assert!(!failed.retryable);
    assert_eq!(
        HostAdmission::get_source_cursor(&facade, &source, &scope)
            .await
            .unwrap(),
        before
    );
    let later_error = HostAdmission::capture_observation(&facade, later)
        .await
        .unwrap_err();
    assert_eq!(later_error.reason_code, Some("cursor_conflict"));
    let store = database.observation_store();
    let retained = store.get_observation(&original_id).await.unwrap().unwrap();
    assert_eq!(
        store
            .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
            .await
            .unwrap()
            .len(),
        1
    );
    let snapshot = database.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT reason FROM source_cursor_advances
         WHERE source_json = ?1 AND scope_json = ?2 AND coverage_json = ?3",
            params![
                source_json.as_str(),
                scope_json.as_str(),
                coverage_json.as_str()
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "blank_frame"
    );
    drop(rows);
    drop(snapshot);

    let transaction = database.begin_write_transaction().await.unwrap();
    let mut rows = transaction
        .query(
            "SELECT sql FROM sqlite_master
             WHERE type = 'trigger' AND name = 'source_cursor_advances_immutable_delete_v1'",
            (),
        )
        .await
        .unwrap();
    let delete_guard = rows
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap();
    drop(rows);
    transaction
        .execute_batch("DROP TRIGGER source_cursor_advances_immutable_delete_v1")
        .await
        .unwrap();
    transaction
        .execute(
            "DELETE FROM source_cursor_advances
         WHERE source_json = ?1 AND scope_json = ?2 AND coverage_json = ?3",
            params![
                source_json.as_str(),
                scope_json.as_str(),
                coverage_json.as_str()
            ],
        )
        .await
        .unwrap();
    transaction.execute_batch(&delete_guard).await.unwrap();
    transaction.commit().await.unwrap();
    facade
        .advance_non_durable_source_cursor(advance, ObservationCancellation::default())
        .await
        .unwrap();
    let last = sequential_capture_requests(&session, 3).remove(2);
    let end = last.identity().position().end();
    let captured = HostAdmission::capture_observation(&facade, last)
        .await
        .unwrap();
    assert!(matches!(
        captured,
        CaptureObservationOutcome::Persisted { .. }
            | CaptureObservationOutcome::AcceptedForReplay { .. }
    ));
    assert_eq!(
        HostAdmission::get_source_cursor(&facade, &source, &scope)
            .await
            .unwrap()
            .unwrap()
            .position(),
        end
    );
    assert_eq!(
        store
            .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        store.get_observation(&original_id).await.unwrap().unwrap(),
        retained
    );
}

#[asupersync::test]
async fn provider_capture_reads_a_cursor_direct_host_admission_refuses() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let (facade, _) = profile_facade(&runtime);
    let source = |provider: &str| {
        ObservationSourceIdentityV1::for_provider(
            ProviderId::new(provider).unwrap(),
            SessionId::new("session.capture-split").unwrap(),
        )
        .unwrap()
    };

    let direct = facade.accept_replay("vibe", HostAdmissionScope::Profile);
    assert_eq!(direct.status, HostAdmissionStatus::Unknown);
    assert_eq!(direct.reason_code, Some("unknown_provider"));
    assert_eq!(
        HostAdmission::get_source_cursor(&facade, &source("vibe"), &ObservationScopeV1::Profile)
            .await,
        Ok(None)
    );

    let unknown =
        HostAdmission::get_source_cursor(&facade, &source("roo"), &ObservationScopeV1::Profile)
            .await
            .unwrap_err();
    assert_eq!(unknown.status, HostAdmissionStatus::Unknown);
    assert_eq!(unknown.reason_code, Some("unknown_provider"));
}

#[asupersync::test]
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

/// Every general reader held is a capacity answer about this pool, not a
/// failed authority write: a retry backoff keyed on the reason must see
/// backpressure, and the same capture must commit once readers return.
#[asupersync::test]
async fn saturated_reader_pool_reports_backpressure_not_authority_write_failure() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let (facade, database) = profile_facade(&runtime);
    let mut held = Vec::new();
    for _ in 0..DEFAULT_MAX_READERS_PER_HOT_SHARD {
        held.push(
            database
                .read_snapshot()
                .await
                .expect("hold a general reader"),
        );
    }
    let occupancy = database
        .read_connection()
        .reader_pool_occupancy()
        .expect("registered reader pool");
    assert_eq!(occupancy.available_general, 0, "{occupancy:?}");
    assert_eq!(
        occupancy.leased_general, DEFAULT_MAX_READERS_PER_HOT_SHARD,
        "{occupancy:?}"
    );

    let backpressured = |outcome: &HostAdmissionOutcome| {
        assert_eq!(
            (outcome.status, outcome.retryable, outcome.reason_code),
            (
                HostAdmissionStatus::Backpressured,
                true,
                Some("reader_pool_saturated")
            ),
            "cause: {:?}",
            outcome.cause
        );
    };
    let session_id = SessionId::new("session.host-admission-capture.saturated").unwrap();
    let source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new(BATCH_PROVIDER).unwrap(),
        session_id.clone(),
    )
    .unwrap();
    backpressured(
        &facade
            .get_source_cursor(&source, &ObservationScopeV1::Profile)
            .await
            .expect_err("a cursor read needs a reader"),
    );
    backpressured(
        &facade
            .capture(sequential_capture_requests(&session_id, 1).remove(0))
            .await,
    );

    drop(held);
    let recovered = facade
        .capture(sequential_capture_requests(&session_id, 1).remove(0))
        .await;
    assert_eq!(recovered.status, HostAdmissionStatus::AcceptedForReplay);
}

fn run_git(project: &std::path::Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(project)
        .output()
        .expect("run Git fixture command");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Canonical Git evidence is recorded as rows in the project sessions store
/// itself, in the capture's own transaction: no graph runtime is involved.
#[asupersync::test]
async fn canonical_capture_records_git_evidence_rows_without_a_graph_runtime() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("source-graph-unavailable");
    std::fs::create_dir_all(&project).unwrap();
    run_git(&project, &["init", "-b", "capture-branch"]);
    let project_id = ProjectId::new("project.graph-unavailable-capture").unwrap();
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
        .with_background_cpu(background_cpu_for_host_admission_test()),
    );

    let project_path = project.to_string_lossy().into_owned();
    let listed =
        capture_session_on_branch(&facade, database, &project_id, "codex", &project_path).await;
    let correlation = tracedecay_global_db::GlobalDbGitCorrelationStore::new(database);
    let health = correlation.correlation_index_health().await.unwrap();
    assert_eq!((health.span_count, health.commit_count), (1, 0));
    assert_eq!(listed, ["session.codex-git-capture"]);
}

/// A session whose observations name no folder records the project id as its
/// path. Live hook drains carry no repository provenance, so the admitted
/// project root alone must place the session in the project's worktree.
#[asupersync::test]
async fn hook_drain_without_provenance_places_project_id_sessions_in_the_project_root() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("hook-project");
    std::fs::create_dir_all(&project).unwrap();
    run_git(&project, &["init", "-b", "capture-branch"]);
    run_git(
        &project,
        &[
            "-c",
            "user.email=t@t.invalid",
            "-c",
            "user.name=Test",
            "commit",
            "--allow-empty",
            "-qm",
            "init",
        ],
    );
    let project_id = ProjectId::new("project.hook-capture").unwrap();
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
    let shard = &database.binding().shard_id;
    let facade = HostAdmissionFacade::new(
        HostAdmissionAuthorities::for_project(
            shard.brain_id.clone(),
            shard.profile_id.clone(),
            project_id.clone(),
            database,
        )
        .with_project_root(project.clone())
        .with_background_cpu(background_cpu_for_host_admission_test()),
    );

    let listed = capture_session_on_branch(
        &facade,
        database,
        &project_id,
        "claude",
        project_id.as_str(),
    )
    .await;
    assert_eq!(listed, ["session.claude-git-capture"]);
}

/// Captures and drains one `provider` session recording `project_path`, then
/// lists the sessions Git evidence places on `capture-branch`.
async fn capture_session_on_branch(
    facade: &HostAdmissionFacade<'_>,
    database: &tracedecay_global_db::RegisteredGlobalDb,
    project_id: &ProjectId,
    provider: &str,
    project_path: &str,
) -> Vec<String> {
    let session_id = SessionId::new(format!("session.{provider}-git-capture")).unwrap();
    let message_id = ObservationId::new(format!("message.{provider}-git-capture")).unwrap();
    let payload = json!({"text": "ordinary canonical message remains available"});
    let encoded = serde_json::to_vec(&payload).unwrap();
    let range = ObservationSourceRangeV1::new(0, encoded.len() as u64).unwrap();
    let parsed = parse_normalized_observation_record_v1(
        &encoded,
        range,
        ObservationOrderingDomainV1::FileBytes,
        {
            let session_id = session_id.clone();
            let message_id = message_id.clone();
            let project_path = project_path.to_owned();
            move |native| {
                CanonicalObservationEnvelopeV1::new(
                    ProviderId::new(provider).unwrap(),
                    "message",
                    message_id.clone(),
                    CanonicalObservationRelationsV1::new(session_id.clone())
                        .with_message_id(message_id.clone()),
                    vec![
                        CanonicalObservationFactV1::Session {
                            project_path: Some(project_path.clone()),
                            location_path: Some(project_path.clone()),
                            transcript_path: None,
                            title: None,
                            started_at: None,
                            ended_at: None,
                            source: Some("codex_rollout".to_owned()),
                            native_source: Some("codex".to_owned()),
                            profile: None,
                            location_provenance: Some("rollout_context".to_owned()),
                        },
                        CanonicalObservationFactV1::Git {
                            evidence_kind: CanonicalGitEvidenceKindV1::Branch,
                            reference: Some("capture-branch".to_owned()),
                            content: None,
                        },
                        CanonicalObservationFactV1::Message {
                            role: CanonicalMessageRoleV1::Assistant,
                            content: native,
                            model: None,
                            timestamp: Some(1_785_000_000),
                        },
                    ],
                    CanonicalObservationEvidenceV1::new(
                        ObservationOrderingDomainV1::FileBytes,
                        range,
                    )
                    .with_native_timestamp(1_785_000_000),
                )
                .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)
            }
        },
    )
    .unwrap();
    let scope = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };
    let source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new(provider).unwrap(),
        session_id.clone(),
    )
    .unwrap();
    let request = CaptureObservationRequest::new(
        parsed,
        ObservationIdentityMaterialV1::for_native_record(
            source,
            scope.clone(),
            ObservationSourceGenerationV1::new(1).unwrap(),
            range,
            ObservationOrderingDomainV1::FileBytes,
            message_id.clone(),
        )
        .unwrap(),
        None,
        RetentionClass::new("retention.graph-unavailable-capture").unwrap(),
        ObservationCancellation::default(),
    )
    .unwrap();

    assert!(matches!(
        facade.capture_observation(request).await.unwrap(),
        CaptureObservationOutcome::Persisted { .. }
            | CaptureObservationOutcome::AcceptedForReplay { .. }
    ));
    let drained = facade
        .drain_projection_queue(provider, &scope, &ObservationCancellation::default(), 1)
        .await
        .unwrap();
    assert!(!drained.deferred);
    assert!(
        facade
            .has_session_message(&scope, provider, message_id.as_str())
            .await
            .unwrap()
    );
    let (hits, _) = tracedecay_global_db::GlobalDbGitCorrelationStore::new(database)
        .sessions_for_with_relation_and_presence(
            &tracedecay_sessions::runtime::git_correlation::SessionsForQuery {
                git_ref: tracedecay_sessions::runtime::git_correlation::GitRefFilter::Branch(
                    "capture-branch".to_owned(),
                ),
                since: None,
                until: None,
                limit: 5,
            },
            tracedecay_sessions::runtime::git_correlation::CommitRelationFilter::Produced,
        )
        .await
        .unwrap();
    hits.into_iter().map(|hit| hit.session_id).collect()
}

#[asupersync::test]
async fn production_facade_preparation_waits_for_shared_background_cpu() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let authority = background_cpu_for_host_admission_test();
    let (facade, _) = profile_facade_with_background_cpu(&runtime, Arc::clone(&authority));
    let permits = (0..authority.width().get())
        .map(|_| authority.acquire())
        .collect::<Vec<_>>();
    let session_id = SessionId::new("session.host-admission-capture.shared-cpu").unwrap();
    let capture = facade.capture_observations(sequential_capture_requests(&session_id, 2));
    let mut capture = std::pin::pin!(capture);

    assert!(
        asupersync::time::timeout(
            asupersync::time::wall_now(),
            Duration::from_millis(30),
            capture.as_mut()
        )
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
    let anchor = build_observation_retrieval_anchor(
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

#[asupersync::test]
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

#[asupersync::test]
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

#[asupersync::test]
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

#[asupersync::test]
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

#[asupersync::test]
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

/// A rewritten transcript re-offers its unchanged records under a new source
/// generation. Each resolves to its retained row as a covered duplicate, and
/// that row's projection already landed, so re-offering it must not collide
/// with its own external-source receipt.
#[asupersync::test]
async fn records_reoffered_under_a_new_generation_settle_as_covered_duplicates() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let (facade, database) = profile_facade(&runtime);
    let session_id = SessionId::new("session.host-admission-capture.regeneration").unwrap();
    HostAdmission::capture_observations(
        &facade,
        sequential_capture_requests(&session_id, BATCH_SIZE),
    )
    .await
    .unwrap();
    let source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new(BATCH_PROVIDER).unwrap(),
        session_id.clone(),
    )
    .unwrap();
    let first_generation_end = facade
        .get_source_cursor(&source, &ObservationScopeV1::Profile)
        .await
        .unwrap()
        .expect("first generation cursor");

    let before = committed_transactions(database);
    let outcomes = HostAdmission::capture_observations(
        &facade,
        generation_capture_requests(
            &session_id,
            BATCH_SIZE,
            2,
            Some(first_generation_end.clone()),
        ),
    )
    .await
    .expect("re-offered records settle instead of colliding with their own receipts");

    assert_eq!(outcomes.len(), BATCH_SIZE);
    assert!(outcomes.iter().all(|outcome| matches!(
        outcome,
        CaptureObservationOutcome::Persisted { outcome, .. }
            if matches!(**outcome, ObservationPersistOutcome::CoveredDuplicate(_))
    )));
    assert_eq!(
        committed_transactions(database) - before,
        1,
        "only the observation batch commits; retained rows project nothing"
    );
    let cursor = facade
        .get_source_cursor(&source, &ObservationScopeV1::Profile)
        .await
        .unwrap()
        .expect("second generation cursor");
    assert_eq!(
        cursor.generation(),
        ObservationSourceGenerationV1::new(2).unwrap()
    );
    assert_eq!(cursor.position(), first_generation_end.position());
}

/// Current Codex `item_completed/UserMessage` payload; the item id is derived
/// from the text, so identical prompts share one message identity.
fn user_message_payload(text: impl AsRef<str>) -> serde_json::Value {
    let text = text.as_ref();
    let id = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    serde_json::json!({
        "type": "item_completed",
        "item": {
            "type": "UserMessage",
            "id": format!("user-item-{id:016x}"),
            "content": [{"type": "text", "text": text}],
        },
    })
}
