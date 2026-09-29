//! Session-store work follows the work it admits, not the store size.
//!
//! Every project-scope drain also converges the session Git evidence past its
//! durable frontier, picks pending external-source projections, and wakes the
//! temporal refresh that discovers which sessions moved. Any of those that
//! visits every session, pending row, or observation effect re-reads the whole
//! session store for each streamed message. Retention runs over the same store
//! on every maintenance tick, and a pass that scans it inside its write
//! transaction outlives the transaction lease once the store is large. These
//! tests measure the process's own read bytes, or the writer's SQLite work,
//! through the public ingest and retention paths on the same store before and
//! after it grows.

#![cfg(target_os = "linux")]

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
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
use tracedecay_global_db::observation::retention::ObservationRetentionConfig;
use tracedecay_global_db::tests::harness::HostAdmissionTestRuntimeV1;
use tracedecay_global_db::{RegisteredGlobalDb, RegisteredGlobalDbLeaseV1};
use tracedecay_host_admission::{HostAdmissionAuthorities, HostAdmissionFacade};
use tracedecay_lcm::LcmRetentionConfig;
use tracedecay_maintenance::retention::registered_store::run_registered_store_retention;
use tracedecay_privacy::{ObservationRecordParseErrorV1, parse_normalized_observation_record_v1};
use tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1;
use tracedecay_session_runtime::session_sync::test_harness::{
    SessionTemporalRefreshWakeState, run_session_temporal_refresh_pass,
};
use tracedecay_session_runtime::session_temporal_refresh_scheduler::projector::{
    CanonicalSessionTemporalProjector, SessionTemporalRefreshPolicy,
};
use tracedecay_sessions::admission::{HostAdmission, HostAdmissionScope};
use tracedecay_sessions::observation::{
    CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
};
use tracedecay_sessions::repository_provenance::RepositoryProvenanceAdmissionContext;

const PROVIDER: &str = "codex";
const MESSAGES_PER_SESSION: u64 = 5;
const BASE_SESSIONS: u64 = 100;
const GROWN_SESSIONS: u64 = 8 * BASE_SESSIONS;
const SEED_TIMESTAMP: i64 = 1_780_000_000;
const DRAIN_WINDOW: usize = 4_096;
const MAX_REFRESH_PASSES: usize = 4_096;
/// Retention runs long after the seeded messages, so every window has passed.
const RETENTION_NOW: i64 = SEED_TIMESTAMP + 400 * 86_400;

/// Reads are measured for the whole process, so the probes must not overlap.
static MEASURED: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
// Hosts title a session with its opening prompt, so real session rows carry
// kilobytes of text.
const PROMPT_TITLE_WORDS: usize = 500;
const BACKLOG_SESSIONS: u64 = 4;
const BASE_BACKLOG_MESSAGES: u64 = 40;
const GROWN_BACKLOG_MESSAGES: u64 = 16 * BASE_BACKLOG_MESSAGES;

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
    title_words: usize,
) -> Vec<CaptureObservationRequest> {
    let source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new(PROVIDER).unwrap(),
        cursor.session_id.clone(),
    )
    .unwrap();
    let project_path = project.to_string_lossy().into_owned();
    let title = format!(
        "{} opening prompt {}",
        cursor.session_id.as_str(),
        "context ".repeat(title_words)
    );
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
            let envelope_title = title.clone();
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
                                title: Some(envelope_title.clone()),
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
            message_requests(
                project,
                scope,
                &mut cursor,
                MESSAGES_PER_SESSION,
                timestamp,
                PROMPT_TITLE_WORDS,
            ),
        )
        .await;
        drain(facade, scope).await;
        first.get_or_insert(cursor);
    }
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
    let requests = message_requests(project, scope, cursor, 1, timestamp, PROMPT_TITLE_WORDS);
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

struct DrainFixture {
    _tmp: TempDir,
    project: PathBuf,
    project_id: ProjectId,
    provenance: RepositoryProvenanceAdmissionContext,
    runtime: HostAdmissionTestRuntimeV1,
}

impl DrainFixture {
    async fn open() -> Self {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("drain-read-cost");
        std::fs::create_dir_all(&project).unwrap();
        run_git(&project, &["init", "-b", "main"]);
        run_git(
            &project,
            &[
                "-c",
                "user.name=TraceDecay",
                "-c",
                "user.email=tracedecay@example.invalid",
                "commit",
                "--allow-empty",
                "-m",
                "initial",
            ],
        );
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
        let provenance = RepositoryProvenanceAdmissionContext::from_authoritative_project_marker(
            &project,
            &project_id,
            &marker,
        )
        .unwrap();
        Self {
            _tmp: tmp,
            project,
            project_id,
            provenance,
            runtime,
        }
    }

    fn facade(&self) -> HostAdmissionFacade<'_> {
        let database = self
            .runtime
            .registered_database(HostAdmissionScope::Project)
            .unwrap();
        let shard = &database.binding().shard_id;
        HostAdmissionFacade::new(
            HostAdmissionAuthorities::for_project(
                shard.brain_id.clone(),
                shard.profile_id.clone(),
                self.project_id.clone(),
                database,
            )
            .with_repository_provenance(self.provenance.clone())
            .with_background_cpu(Arc::new(ProcessBackgroundCpuV1::new(
                NonZeroUsize::new(4).unwrap(),
            ))),
        )
    }

    fn scope(&self) -> ObservationScopeV1 {
        ObservationScopeV1::Project {
            project_id: self.project_id.clone(),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_message_drain_reads_do_not_scale_with_the_session_store() {
    let _measured = MEASURED.lock().await;
    let fixture = DrainFixture::open().await;
    let facade = fixture.facade();
    let (project, scope) = (fixture.project.as_path(), fixture.scope());

    let mut probed = seed_sessions(&facade, project, &scope, 0..BASE_SESSIONS).await;
    let probe_timestamp = SEED_TIMESTAMP + 10_000_000;
    let base_read = probe_one_message(&facade, project, &scope, &mut probed, probe_timestamp).await;

    seed_sessions(&facade, project, &scope, BASE_SESSIONS..GROWN_SESSIONS).await;
    assert_eq!(
        fixture
            .runtime
            .project_session_message_count_for_test()
            .await
            .unwrap(),
        i64::try_from(GROWN_SESSIONS * MESSAGES_PER_SESSION + 1).unwrap(),
    );
    let grown_read =
        probe_one_message(&facade, project, &scope, &mut probed, probe_timestamp + 1).await;

    eprintln!("single-message drain read bytes: base={base_read} grown={grown_read}");
    assert!(
        grown_read <= base_read * 2,
        "an 8x larger session store must not double one message's drain reads: \
         base={base_read} grown={grown_read}"
    );
}

/// Captures `messages_per_session` new messages on each of `sessions` without
/// draining, then drains that whole backlog, returning the bytes read per
/// projected message.
async fn backlog_drain_read_per_message(
    facade: &HostAdmissionFacade<'_>,
    project: &Path,
    scope: &ObservationScopeV1,
    sessions: std::ops::Range<u64>,
    messages_per_session: u64,
) -> u64 {
    let backlog = (sessions.end - sessions.start) * messages_per_session;
    for index in sessions {
        let mut cursor = session_cursor(index);
        let timestamp = SEED_TIMESTAMP + i64::try_from(index * 10_000).unwrap();
        capture(
            facade,
            message_requests(
                project,
                scope,
                &mut cursor,
                messages_per_session,
                timestamp,
                1,
            ),
        )
        .await;
    }
    let before = process_read_bytes();
    let projected = drain(facade, scope).await;
    let read = process_read_bytes() - before;
    assert_eq!(
        projected, backlog,
        "the drain must project the whole backlog"
    );
    read / backlog
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_backlog_drain_reads_scale_with_the_backlog() {
    let _measured = MEASURED.lock().await;
    let fixture = DrainFixture::open().await;
    let facade = fixture.facade();
    let (project, scope) = (fixture.project.as_path(), fixture.scope());

    let base = backlog_drain_read_per_message(
        &facade,
        project,
        &scope,
        0..BACKLOG_SESSIONS,
        BASE_BACKLOG_MESSAGES,
    )
    .await;
    let grown = backlog_drain_read_per_message(
        &facade,
        project,
        &scope,
        BACKLOG_SESSIONS..2 * BACKLOG_SESSIONS,
        GROWN_BACKLOG_MESSAGES,
    )
    .await;

    eprintln!("backlog drain read bytes per message: base={base} grown={grown}");
    assert!(
        grown <= base * 2,
        "draining a 16x larger backlog must not double each message's reads: \
         base={base} grown={grown}"
    );
}

/// Runs temporal refresh passes until one finds nothing to begin, project, or
/// complete, returning the refreshes completed.
async fn refresh_until_idle(
    database: &RegisteredGlobalDbLeaseV1,
    state: &Arc<SessionTemporalRefreshWakeState>,
) -> usize {
    let mut completed = 0;
    for _ in 0..MAX_REFRESH_PASSES {
        let report = run_session_temporal_refresh_pass(
            database,
            state,
            &CanonicalSessionTemporalProjector,
            SessionTemporalRefreshPolicy::default(),
        )
        .await;
        assert_eq!(
            (
                report.failed,
                report.retryable_errors,
                report.terminal_errors,
                report.deadline_errors
            ),
            (0, 0, 0, 0),
            "refresh must not fail: {report:?}"
        );
        completed += report.completed;
        if !report.saturated
            && report.begun == 0
            && report.joined == 0
            && report.projected_batches == 0
            && report.completed == 0
        {
            return completed;
        }
    }
    panic!("temporal refresh did not settle within {MAX_REFRESH_PASSES} passes");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streamed_message_refresh_reads_do_not_scale_with_the_session_store() {
    let _measured = MEASURED.lock().await;
    let fixture = DrainFixture::open().await;
    let facade = fixture.facade();
    let database = fixture
        .runtime
        .registered_database_lease(HostAdmissionScope::Project)
        .unwrap();
    let refresh = Arc::new(SessionTemporalRefreshWakeState::default());
    let (project, scope) = (fixture.project.as_path(), fixture.scope());

    let mut probed = seed_sessions(&facade, project, &scope, 0..BASE_SESSIONS).await;
    assert_eq!(
        refresh_until_idle(&database, &refresh).await,
        usize::try_from(BASE_SESSIONS).unwrap()
    );
    let probe_timestamp = SEED_TIMESTAMP + 10_000_000;
    let before = process_read_bytes();
    probe_one_message(&facade, project, &scope, &mut probed, probe_timestamp).await;
    assert_eq!(refresh_until_idle(&database, &refresh).await, 1);
    let base_read = process_read_bytes() - before;

    seed_sessions(&facade, project, &scope, BASE_SESSIONS..GROWN_SESSIONS).await;
    assert_eq!(
        refresh_until_idle(&database, &refresh).await,
        usize::try_from(GROWN_SESSIONS - BASE_SESSIONS).unwrap()
    );
    let before = process_read_bytes();
    probe_one_message(&facade, project, &scope, &mut probed, probe_timestamp + 1).await;
    assert_eq!(refresh_until_idle(&database, &refresh).await, 1);
    let grown_read = process_read_bytes() - before;

    eprintln!("streamed message ingest read bytes: base={base_read} grown={grown_read}");
    assert!(
        grown_read <= base_read * 2,
        "an 8x larger session store must not double one streamed message's reads: \
         base={base_read} grown={grown_read}"
    );
}

/// SQLite VM steps the store's writer executed, and its rolled-back
/// transactions: the work retention does inside its write transactions.
fn writer_work(database: &RegisteredGlobalDb) -> (u64, u64) {
    let writer = database
        .runtime_client()
        .writer_telemetry_snapshot()
        .expect("registered database must expose rusqlite writer telemetry")
        .writer
        .expect("mounted writer must carry rusqlite writer telemetry");
    (
        writer.sqlite_vm.vm_steps,
        writer.transactions.rolled_back_transactions,
    )
}

/// Runs one maintenance retention tick and returns the VM steps its write
/// transactions executed.
async fn retention_writer_steps(database: &RegisteredGlobalDb) -> u64 {
    let (steps_before, rolled_back_before) = writer_work(database);
    let report = run_registered_store_retention(
        database,
        &LcmRetentionConfig::default(),
        &ObservationRetentionConfig::default(),
        RETENTION_NOW,
    )
    .await;
    assert!(report.succeeded(), "retention must succeed");
    let (steps_after, rolled_back_after) = writer_work(database);
    assert_eq!(
        rolled_back_after, rolled_back_before,
        "every retention transaction must commit"
    );
    steps_after - steps_before
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retention_write_transactions_do_not_scale_with_the_session_store() {
    let _measured = MEASURED.lock().await;
    let fixture = DrainFixture::open().await;
    let facade = fixture.facade();
    let database = fixture
        .runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let (project, scope) = (fixture.project.as_path(), fixture.scope());

    seed_sessions(&facade, project, &scope, 0..BASE_SESSIONS).await;
    let base_steps = retention_writer_steps(database).await;
    seed_sessions(&facade, project, &scope, BASE_SESSIONS..GROWN_SESSIONS).await;
    let grown_steps = retention_writer_steps(database).await;

    eprintln!("retention writer VM steps: base={base_steps} grown={grown_steps}");
    assert!(
        grown_steps <= base_steps * 2,
        "an 8x larger session store must not double the work retention does inside \
         its write transactions: base={base_steps} grown={grown_steps}"
    );
}
