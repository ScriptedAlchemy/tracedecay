//! Session-store work follows the work it admits, not the store size.
//!
//! Every project-scope drain also converges the session Git evidence past its
//! durable frontier, picks pending external-source projections, and wakes the
//! temporal refresh that discovers which sessions moved. Any of those that
//! visits every session, pending row, or observation effect re-reads the whole
//! session store for each streamed message. Retention runs over the same store
//! on every maintenance tick, and a pass that scans it inside its write
//! transaction outlives the transaction lease once the store is large. A
//! refresh that copied, re-derived, or re-hashed the whole live session per
//! appended message reads more the longer that session runs. These tests
//! measure the process's own read bytes, or the writer's SQLite work, through
//! the public ingest and retention paths on the same store before and after
//! it, or one live session in it, grows.

use std::collections::BTreeMap;
use std::io::Read as _;
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
use tracedecay_global_db::tests::harness::{HostAdmissionTestRuntimeV1, writer_telemetry};
use tracedecay_global_db::{RegisteredGlobalDb, RegisteredGlobalDbLeaseV1};
use tracedecay_host_admission::session_ingest_authority::GlobalDbSessionIngestAuthority;
use tracedecay_host_admission::{HostAdmissionAuthorities, HostAdmissionFacade};
use tracedecay_lcm::LcmRetentionConfig;
use tracedecay_maintenance::retention::registered_store::run_registered_store_retention;
use tracedecay_privacy::{ObservationRecordParseErrorV1, parse_normalized_observation_record_v1};
use tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1;
use tracedecay_runtime_core::config::ProfileRoot;
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
use tracedecay_sessions::runtime::{
    TranscriptIngestOutcome, ingest_project_sources_for_provider, with_transcript_source_profile,
};

pub const PROVIDER: &str = "codex";
pub const MESSAGES_PER_SESSION: u64 = 5;
pub const BASE_SESSIONS: u64 = 100;
pub const GROWN_SESSIONS: u64 = 8 * BASE_SESSIONS;
pub const SEED_TIMESTAMP: i64 = 1_780_000_000;
pub const DRAIN_WINDOW: usize = 4_096;
pub const MAX_REFRESH_PASSES: usize = 4_096;
/// Retention runs long after the seeded messages, so every window has passed.
pub const RETENTION_NOW: i64 = SEED_TIMESTAMP + 400 * 86_400;

// Hosts title a session with its opening prompt, so real session rows carry
// kilobytes of text.
pub const PROMPT_TITLE_WORDS: usize = 500;
pub const BACKLOG_SESSIONS: u64 = 4;
pub const BASE_BACKLOG_MESSAGES: u64 = 40;
pub const GROWN_BACKLOG_MESSAGES: u64 = 16 * BASE_BACKLOG_MESSAGES;

pub fn process_read_bytes() -> u64 {
    let io = std::fs::read_to_string("/proc/self/io").unwrap();
    io.lines()
        .find_map(|line| line.strip_prefix("rchar: "))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

pub struct SessionCursor {
    pub session_id: SessionId,
    pub next_ordinal: u64,
    pub next_offset: u64,
}

pub fn session_cursor(index: u64) -> SessionCursor {
    SessionCursor {
        session_id: SessionId::new(format!("session.drain-read-cost.{index:05}")).unwrap(),
        next_ordinal: 0,
        next_offset: 0,
    }
}

pub fn message_requests(
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

pub async fn capture(facade: &HostAdmissionFacade<'_>, requests: Vec<CaptureObservationRequest>) {
    let expected = requests.len();
    let outcomes = facade.capture_observations(requests).await.unwrap();
    assert_eq!(outcomes.len(), expected);
    assert!(outcomes.iter().all(|outcome| matches!(
        outcome,
        CaptureObservationOutcome::Persisted { .. }
            | CaptureObservationOutcome::AcceptedForReplay { .. }
    )));
}

pub async fn drain(facade: &HostAdmissionFacade<'_>, scope: &ObservationScopeV1) -> u64 {
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
pub async fn seed_sessions(
    facade: &HostAdmissionFacade<'_>,
    project: &Path,
    scope: &ObservationScopeV1,
    sessions: std::ops::Range<u64>,
) -> SessionCursor {
    // Fixture population is a bounded admission batch; only the probes below
    // model individually streamed messages. Keep every session's full history.
    const SEED_SESSIONS_PER_BATCH: usize = 32;
    let batch_messages = SEED_SESSIONS_PER_BATCH * usize::try_from(MESSAGES_PER_SESSION).unwrap();
    let mut requests = Vec::with_capacity(batch_messages);
    let mut first = None;
    for index in sessions {
        let mut cursor = session_cursor(index);
        let timestamp = SEED_TIMESTAMP + i64::try_from(index * MESSAGES_PER_SESSION).unwrap();
        requests.extend(message_requests(
            project,
            scope,
            &mut cursor,
            MESSAGES_PER_SESSION,
            timestamp,
            PROMPT_TITLE_WORDS,
        ));
        first.get_or_insert(cursor);
        if requests.len() == batch_messages {
            capture(facade, std::mem::take(&mut requests)).await;
            drain(facade, scope).await;
        }
    }
    if !requests.is_empty() {
        capture(facade, requests).await;
        drain(facade, scope).await;
    }
    first.unwrap()
}

/// Captures and drains one new message on an existing session, returning the
/// bytes the process read to do it.
pub async fn probe_one_message(
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

pub fn run_git(project: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(project)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?} failed");
}

pub struct DrainFixture {
    pub _tmp: TempDir,
    pub project: PathBuf,
    pub project_id: ProjectId,
    pub provenance: RepositoryProvenanceAdmissionContext,
    pub runtime: HostAdmissionTestRuntimeV1,
}

impl DrainFixture {
    pub async fn open() -> Self {
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

    pub fn facade(&self) -> HostAdmissionFacade<'_> {
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

    pub fn scope(&self) -> ObservationScopeV1 {
        ObservationScopeV1::Project {
            project_id: self.project_id.clone(),
        }
    }
}

/// Compare drain queries at the same WAL phase. A threshold-triggered
/// checkpoint copies earlier messages' dirty pages, so including it in only
/// one probe measures periodic maintenance rather than this message's reads.
/// Ordinary capture/drain writes on a separate session advance the log to its
/// next generation; checkpoint policy and the probed session stay unchanged.
pub async fn probe_drain_after_wal_restart(
    facade: &HostAdmissionFacade<'_>,
    fixture: &DrainFixture,
    probed: &mut SessionCursor,
    padding: &mut SessionCursor,
    timestamp: i64,
) -> u64 {
    let (project, scope) = (fixture.project.as_path(), fixture.scope());
    let wal = session_store_path(fixture).with_extension("db-wal");
    let sequence = wal_checkpoint_sequence(&wal);
    let mut restarted = false;
    for _ in 0..DRAIN_WINDOW {
        capture(
            facade,
            message_requests(project, &scope, padding, 1, timestamp, PROMPT_TITLE_WORDS),
        )
        .await;
        assert_eq!(drain(facade, &scope).await, 1);
        if wal_checkpoint_sequence(&wal) != sequence {
            restarted = true;
            break;
        }
    }
    assert!(
        restarted,
        "ordinary ingest must restart the WAL before the probe"
    );
    let database = fixture
        .runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let checkpointed = writer_telemetry(database).wal.checkpointed_frames;
    let read = probe_one_message(facade, project, &scope, probed, timestamp).await;
    assert_eq!(
        writer_telemetry(database).wal.checkpointed_frames,
        checkpointed,
        "the drain query comparison must not include a periodic checkpoint"
    );
    read
}

/// Captures `messages_per_session` new messages on each of `sessions` without
/// draining, then drains that whole backlog, returning the bytes read per
/// projected message.
pub async fn backlog_drain_read_per_message(
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

/// Runs temporal refresh passes until one finds nothing to begin, project, or
/// complete, returning the refreshes completed.
pub async fn refresh_until_idle(
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

/// Streams `count` messages into the live session one at a time, capturing,
/// draining, and refreshing each, and returns the bytes the process read and
/// the VM steps the store's writer executed per message across that whole
/// ingest.
pub async fn ingest_live_messages(
    facade: &HostAdmissionFacade<'_>,
    fixture: &DrainFixture,
    database: &RegisteredGlobalDbLeaseV1,
    refresh: &Arc<SessionTemporalRefreshWakeState>,
    cursor: &mut SessionCursor,
    count: u64,
) -> (u64, u64) {
    let (project, scope) = (fixture.project.as_path(), fixture.scope());
    let (mut read, mut steps) = (0, 0);
    for _ in 0..count {
        let timestamp = SEED_TIMESTAMP + i64::try_from(cursor.next_ordinal).unwrap();
        let steps_before = writer_work(database).0;
        let before = process_read_bytes();
        capture(
            facade,
            message_requests(project, &scope, cursor, 1, timestamp, PROMPT_TITLE_WORDS),
        )
        .await;
        assert_eq!(drain(facade, &scope).await, 1);
        assert_eq!(refresh_until_idle(database, refresh).await, 1);
        read += process_read_bytes() - before;
        steps += writer_work(database).0 - steps_before;
    }
    (read / count, steps / count)
}

pub fn session_store_path(fixture: &DrainFixture) -> PathBuf {
    let mut stores = Vec::new();
    let mut pending = vec![fixture._tmp.path().join("profile")];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().is_some_and(|name| name == "sessions.db") {
                stores.push(path);
            }
        }
    }
    assert_eq!(stores.len(), 1, "one project session store: {stores:?}");
    stores.pop().unwrap()
}

/// The WAL header's checkpoint sequence, which SQLite advances each time the
/// writer restarts the log from its head after a complete checkpoint.
pub fn wal_checkpoint_sequence(wal: &Path) -> u32 {
    let mut header = [0; 16];
    std::fs::File::open(wal)
        .unwrap()
        .read_exact(&mut header)
        .unwrap();
    u32::from_be_bytes(header[12..16].try_into().unwrap())
}

/// The writer's position in the log: the checkpoint sequence of the log
/// generation and the last valid frame (`mxFrame` of the `-shm` wal-index
/// header).
#[derive(Clone, Copy)]
pub struct WalMark {
    pub sequence: u32,
    pub max_frame: u32,
}

pub fn wal_mark(store: &Path) -> WalMark {
    let shm = std::fs::read(store.with_extension("db-shm")).unwrap();
    WalMark {
        sequence: wal_checkpoint_sequence(&store.with_extension("db-wal")),
        max_frame: u32::from_ne_bytes(shm[16..20].try_into().unwrap()),
    }
}

/// Commit frames the writer appended between two marks, or `None` when the log
/// restarted since the first mark. A commit frame's header carries the
/// database size after the commit; every other frame carries zero there.
pub fn wal_commits(store: &Path, from: WalMark, to: WalMark) -> Option<usize> {
    use std::os::unix::fs::FileExt;
    if from.sequence != to.sequence || to.max_frame < from.max_frame {
        return None;
    }
    let wal = std::fs::File::open(store.with_extension("db-wal")).unwrap();
    let mut header = [0; 32];
    let sequence = |header: &[u8; 32]| u32::from_be_bytes(header[12..16].try_into().unwrap());
    wal.read_exact_at(&mut header, 0).ok()?;
    if sequence(&header) != from.sequence {
        return None;
    }
    let frame_size = 24 + u64::from(u32::from_be_bytes(header[8..12].try_into().unwrap()));
    let mut commits = 0;
    for frame in from.max_frame..to.max_frame {
        let mut database_size = [0; 4];
        wal.read_exact_at(&mut database_size, 32 + u64::from(frame) * frame_size + 4)
            .ok()?;
        commits += usize::from(database_size != [0; 4]);
    }
    // A restart after the marks rewrites the header first, so an unchanged
    // sequence means every frame read still belongs to the marked log.
    wal.read_exact_at(&mut header, 0).ok()?;
    (sequence(&header) == from.sequence).then_some(commits)
}

/// One project history pass over every host provider, as the session
/// temporal refresh runs it, reading transcripts under `home`.
pub async fn project_history_pass(
    fixture: &DrainFixture,
    database: &RegisteredGlobalDbLeaseV1,
    home: &Path,
) -> TranscriptIngestOutcome {
    let authority = GlobalDbSessionIngestAuthority::new(database.clone()).with_background_cpu(
        Arc::new(ProcessBackgroundCpuV1::new(NonZeroUsize::new(4).unwrap())),
    );
    let shard = &database.binding().shard_id;
    with_transcript_source_profile(
        ProfileRoot::under_home(home),
        ingest_project_sources_for_provider(
            &shard.brain_id,
            &shard.profile_id,
            &authority,
            &fixture.project,
            Some(fixture.project_id.clone()),
            None,
            true,
        ),
    )
    .await
}

/// Levels of every B-tree in the fixture's project session store: the pages
/// one point lookup in that tree reads on a cold reader.
pub fn session_store_tree_depths(fixture: &DrainFixture) -> BTreeMap<String, u32> {
    let connection = rusqlite::Connection::open_with_flags(
        session_store_path(fixture),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut statement = connection
        .prepare(
            "SELECT name, max(length(path) - length(replace(path, '/', ''))) \
             FROM dbstat WHERE pagetype != 'overflow' GROUP BY name",
        )
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Median over `windows` consecutive probes of `MEDIAN_PROBE_MESSAGES` streamed
/// messages each. The writer's PASSIVE checkpoint copies the whole log back
/// inside whichever probe crosses the soft limit; that copy is the amortized
/// cost of the frames every message wrote, which
/// `sustained_stream_with_concurrent_readers_keeps_the_wal_bounded` bounds,
/// so the median measures what reading one message costs.
pub const MEDIAN_PROBE_MESSAGES: u64 = 4;

pub async fn median_streamed_message_reads(
    facade: &HostAdmissionFacade<'_>,
    fixture: &DrainFixture,
    database: &RegisteredGlobalDbLeaseV1,
    refresh: &Arc<SessionTemporalRefreshWakeState>,
    live: &mut SessionCursor,
    windows: usize,
) -> u64 {
    let mut reads = Vec::with_capacity(windows);
    for _ in 0..windows {
        let (read, _) = ingest_live_messages(
            facade,
            fixture,
            database,
            refresh,
            live,
            MEDIAN_PROBE_MESSAGES,
        )
        .await;
        reads.push(read);
    }
    reads.sort_unstable();
    eprintln!("streamed message read bytes per probe window: {reads:?}");
    reads[windows / 2]
}

/// SQLite VM steps the store's writer executed, and its rolled-back
/// transactions: the work retention does inside its write transactions.
pub fn writer_work(database: &RegisteredGlobalDb) -> (u64, u64) {
    let writer = writer_telemetry(database);
    (
        writer.sqlite_vm.vm_steps,
        writer.transactions.rolled_back_transactions,
    )
}

/// Runs one maintenance retention tick and returns the VM steps its write
/// transactions executed.
pub async fn retention_writer_steps(database: &RegisteredGlobalDb) -> u64 {
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

/// Runs one maintenance retention tick and returns the bytes the process read.
pub async fn retention_tick_read_bytes(database: &RegisteredGlobalDb) -> u64 {
    let before = process_read_bytes();
    let report = run_registered_store_retention(
        database,
        &LcmRetentionConfig::default(),
        &ObservationRetentionConfig::default(),
        RETENTION_NOW,
    )
    .await;
    assert!(report.succeeded(), "retention must succeed");
    process_read_bytes() - before
}
