use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use tempfile::TempDir;
use tracedecay::application::host_admission::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
use tracedecay::storage::{read_repository_identity_marker, write_repository_identity_marker};
use tracedecay_domain::{
    ObservationScopeV1, ObservationSourceCursorV1, ObservationSourceIdentityV1, ProjectId,
    ProviderId, SessionId,
};
use tracedecay_sessions::runtime::claude::ClaudeSource;
use tracedecay_sessions::runtime::cline_like::ClineLikeSource;
use tracedecay_sessions::runtime::codex::CodexSource;
use tracedecay_sessions::runtime::cursor::{
    ingest_cursor_transcript_event as ingest_cursor_transcript_event_registered,
    try_ingest_cursor_transcript_event as try_ingest_cursor_transcript_event_registered,
};
use tracedecay_sessions::runtime::source::{TranscriptIngestError, TranscriptSource};
use tracedecay_sessions::runtime::{SessionMessageSearchResult, SessionProvider};
use tracedecay_store::ObservationReplayRequest;

use crate::claude::write_claude_transcript;
use crate::cline_like::{vscode_storage_root, write_task};
use crate::common::{EnvVarGuard, GLOBAL_DB_ENV_LOCK};
use crate::support::{init_git_repo, init_project, setup};

const TEST_PROJECT_ID: &str = "tracedecay-transcript-ingest-fixture";
static NEXT_TEST_PROJECT_ID: AtomicU64 = AtomicU64::new(1);

pub(super) fn fixture_project_id() -> ProjectId {
    ProjectId::new(TEST_PROJECT_ID).unwrap()
}

fn test_project_id(project: &Path) -> ProjectId {
    read_repository_identity_marker(project)
        .ok()
        .flatten()
        .and_then(|marker| ProjectId::new(marker.project_id).ok())
        .unwrap_or_else(fixture_project_id)
}

pub(super) fn mark_test_project(project: &Path) -> ProjectId {
    if !project.join(".git").exists() {
        init_git_repo(project);
    }
    let sequence = NEXT_TEST_PROJECT_ID.fetch_add(1, Ordering::Relaxed);
    let project_id = ProjectId::new(format!("{TEST_PROJECT_ID}-{sequence}"))
        .expect("valid unique fixture project id");
    assert!(write_repository_identity_marker(project, project_id.as_str()).unwrap());
    project_id
}

pub(super) struct ProjectSessionTestRuntime {
    runtime: HostAdmissionTestRuntimeV1,
    project_id: ProjectId,
}

impl ProjectSessionTestRuntime {
    pub(super) fn runtime(&self) -> &HostAdmissionTestRuntimeV1 {
        &self.runtime
    }

    pub(super) fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    pub(super) async fn get_parse_offset(
        &self,
        path: &str,
    ) -> Option<tracedecay::global_db::ParseOffset> {
        self.runtime
            .project_parse_offset_for_test(path)
            .await
            .unwrap()
    }

    pub(super) async fn session_message_count(&self) -> tracedecay::errors::Result<i64> {
        self.runtime.project_session_message_count_for_test().await
    }

    pub(super) async fn get_session(
        &self,
        provider: &str,
        session_id: &str,
    ) -> Option<tracedecay_sessions::runtime::SessionRecord> {
        self.runtime
            .project_session_for_test(provider, session_id)
            .await
            .unwrap()
    }

    pub(super) async fn get_session_message(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Option<tracedecay_sessions::runtime::SessionMessageRecord> {
        self.runtime
            .project_session_message_for_test(provider, message_id)
            .await
            .unwrap()
    }

    pub(super) async fn search_session_messages(
        &self,
        provider: &str,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
    ) -> Vec<SessionMessageSearchResult> {
        self.runtime
            .search_project_session_messages_for_test(provider, project_key, query, limit)
            .await
            .unwrap()
    }

    pub(super) async fn lcm_load_raw_message(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Option<tracedecay_sessions::runtime::lcm::LcmRawMessage> {
        self.runtime
            .project_lcm_raw_message_for_test(provider, message_id)
            .await
            .unwrap()
    }

    /// Known provider-usage observations from the canonical accounting
    /// authority, in commit order. Panics when the projection has not
    /// published a watermark: an unpublished authority is not "no usage".
    pub(super) async fn provider_usage_observations(
        &self,
        provider: &str,
    ) -> Vec<tracedecay_domain::ProviderUsageObservationV1> {
        match self
            .runtime
            .project_provider_usage_for_test(Some(provider), None, 100)
            .await
            .unwrap()
        {
            tracedecay_domain::ProviderUsageReadV1::Known { observations, .. } => observations,
            other => panic!("{provider} provider usage read is not Known: {other:?}"),
        }
    }
}

pub(super) async fn open_project_session_db(project: &Path) -> Option<ProjectSessionTestRuntime> {
    let profile_root = project.parent()?.join("tracedecay-test-profile");
    let project_id = read_repository_identity_marker(project)
        .ok()
        .flatten()
        .and_then(|marker| ProjectId::new(marker.project_id).ok())
        .unwrap_or_else(|| mark_test_project(project));
    let runtime = HostAdmissionTestRuntimeV1::project(profile_root, project, project_id.clone())
        .await
        .ok()?;
    Some(ProjectSessionTestRuntime {
        runtime,
        project_id,
    })
}

/// Mounts a second project through `primary`'s daemon registry, the way one
/// production daemon serves many projects. Two independent runtimes on one
/// profile cannot coexist: the profile session-relation graph is
/// single-writer.
pub(super) async fn open_sibling_project_session_db(
    primary: &ProjectSessionTestRuntime,
    project: &Path,
) -> ProjectSessionTestRuntime {
    let project_id = read_repository_identity_marker(project)
        .ok()
        .flatten()
        .and_then(|marker| ProjectId::new(marker.project_id).ok())
        .unwrap_or_else(|| mark_test_project(project));
    let runtime = primary
        .runtime
        .sibling_project(project, project_id.clone())
        .await
        .expect("sibling project mount through the shared daemon registry");
    ProjectSessionTestRuntime {
        runtime,
        project_id,
    }
}

pub(super) async fn try_ingest_source(
    runtime: &ProjectSessionTestRuntime,
    source: &dyn TranscriptSource,
    project_root: &Path,
    max_new_bytes: Option<u64>,
) -> tracedecay_sessions::runtime::source::TranscriptIngestResult<
    tracedecay_sessions::runtime::shared::TranscriptIngestStats,
> {
    runtime
        .runtime
        .ingest_project_transcript_source_for_test(source, project_root, max_new_bytes)
        .await
}

async fn ingest_cursor_transcript_event(
    event_json: &str,
    runtime: &ProjectSessionTestRuntime,
    project_id: ProjectId,
) -> tracedecay_sessions::runtime::cursor::CursorTranscriptIngestStats {
    ingest_cursor_transcript_event_registered(event_json, &runtime.runtime.facade(), project_id)
        .await
}

async fn try_ingest_cursor_transcript_event(
    event_json: &str,
    runtime: &ProjectSessionTestRuntime,
    project_id: ProjectId,
) -> tracedecay_sessions::runtime::source::TranscriptIngestResult<
    tracedecay_sessions::runtime::cursor::CursorTranscriptIngestStats,
> {
    try_ingest_cursor_transcript_event_registered(event_json, &runtime.runtime.facade(), project_id)
        .await
}

pub(super) async fn ingest_global_sources_for_provider(
    runtime: &ProjectSessionTestRuntime,
    project_root: &Path,
    provider: Option<SessionProvider>,
) -> tracedecay_sessions::runtime::shared::TranscriptIngestStats {
    runtime
        .runtime
        .ingest_project_provider_for_test(project_root, provider)
        .await
        .unwrap()
}

async fn parse_offset_for_task_history(
    runtime: &ProjectSessionTestRuntime,
    _project: &Path,
    path: &Path,
) -> Option<tracedecay::global_db::ParseOffset> {
    let path_text = path.to_string_lossy();
    if let Some(offset) = runtime.get_parse_offset(path_text.as_ref()).await {
        return Some(offset);
    }
    #[cfg(windows)]
    {
        let alternate = if path_text.contains('/') {
            path_text.replace('/', "\\")
        } else {
            path_text.replace('\\', "/")
        };
        if alternate != path_text
            && let Some(offset) = runtime.get_parse_offset(&alternate).await
        {
            return Some(offset);
        }
    }
    let task_dir = path.parent()?.file_name()?.to_string_lossy();
    let file_name = path.file_name()?.to_string_lossy();
    runtime
        .runtime
        .project_parse_offset_by_suffix_for_test(&format!("{task_dir}/{file_name}"))
        .await
        .ok()
        .flatten()
}

fn claude_cursor_key(source: &ClaudeSource, project: &Path) -> String {
    let paths = source.transcript_paths(project);
    assert_eq!(paths.len(), 1);
    paths[0].to_string_lossy().into_owned()
}

pub(super) async fn set_projection_failure(runtime: &ProjectSessionTestRuntime, enabled: bool) {
    runtime
        .runtime
        .set_project_projection_failure_for_test(enabled)
        .await
        .unwrap();
}

pub(super) async fn observation_source_cursor(
    runtime: &ProjectSessionTestRuntime,
    provider: &str,
    session_id: &str,
    project: &Path,
) -> Option<ObservationSourceCursorV1> {
    let source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new(provider).unwrap(),
        SessionId::new(session_id).unwrap(),
    )
    .unwrap();
    let project_id = test_project_id(project);
    let scope = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };
    assert_eq!(
        scope,
        ObservationScopeV1::Project {
            project_id: test_project_id(project)
        }
    );
    runtime
        .runtime
        .project_observation_source_cursor_for_test(&source)
        .await
        .ok()
        .flatten()
}

async fn observation_source_cursor_position(
    runtime: &ProjectSessionTestRuntime,
    provider: &str,
    session_id: &str,
    project: &Path,
) -> Option<u64> {
    observation_source_cursor(runtime, provider, session_id, project)
        .await
        .map(|cursor| cursor.position())
}

pub(super) async fn durable_table_count(runtime: &ProjectSessionTestRuntime, table: &str) -> u64 {
    runtime
        .runtime
        .project_observation_table_count_for_test(table)
        .await
        .unwrap()
}

pub(super) async fn assert_secret_absent_from_observation_sinks(
    runtime: &ProjectSessionTestRuntime,
    provider: &str,
    secret: &str,
) {
    let stored = runtime
        .runtime
        .replay_observations(
            HostAdmissionScope::Project,
            ObservationReplayRequest::new(0, 100).unwrap(),
        )
        .await
        .unwrap();
    assert!(
        !stored.is_empty(),
        "{provider}: expected durable observations"
    );
    assert!(
        !format!("{stored:?}").contains(secret),
        "{provider}: secret leaked into durable observation state"
    );
    assert!(
        runtime
            .search_session_messages(provider, None, secret, 10)
            .await
            .is_empty(),
        "{provider}: secret leaked into the visible V1 projection"
    );
}

fn write_codex_rollout_fixture(home: &Path, project: &Path, session: &str) -> PathBuf {
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("rollout-2026-01-01T00-00-00-{session}.jsonl"));
    let contents = format!(
        "{}\n{}\n{}\n",
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:00.000Z",
            "type": "session_meta",
            "payload": {"id": session, "cwd": project.to_string_lossy(), "model": "gpt-5.5"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "Investigate the billing pipeline regression"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:02.000Z",
            "type": "event_msg",
            "payload": {
                "type": "agent_message",
                "message": "The billing pipeline regression is fixed."
            }
        }),
    );
    std::fs::write(&path, contents).unwrap();
    path
}

fn assert_no_transcript_adjacent_fallback_writer(
    runtime: &ProjectSessionTestRuntime,
    transcript: &Path,
) {
    let session_db = runtime
        .runtime
        .database_path(HostAdmissionScope::Project)
        .expect("registered project sessions path");
    assert!(
        session_db.exists(),
        "provider ingest must open the project session database"
    );
    assert!(
        !transcript
            .parent()
            .map(|parent| parent.join("sessions.db").exists()
                || parent.join("tracedecay.db").exists()
                || parent.join(".tracedecay").exists())
            .unwrap_or(false),
        "provider ingest must not create a transcript-adjacent fallback writer"
    );
    assert_ne!(
        session_db.parent().map(Path::to_path_buf),
        transcript.parent().map(Path::to_path_buf),
        "project session db must remain distinct from the transcript directory"
    );
}

#[tokio::test]
async fn claude_restart_ingests_only_the_appended_suffix() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let path = write_claude_transcript(&home, &project, "claude-restart");
    let source = ClaudeSource::with_home(&home);
    let path_key = claude_cursor_key(&source, &project);

    let db = open_project_session_db(&project).await.unwrap();
    let first = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(first.messages_upserted, 2);
    let first_offset = db.get_parse_offset(&path_key).await.unwrap();
    let first_session = db.get_session("claude", "claude-restart").await.unwrap();

    let mut transcript = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(
        transcript,
        "{}",
        serde_json::json!({
            "type": "user",
            "cwd": project,
            "sessionId": "claude-restart",
            "uuid": "u3",
            "timestamp": "2026-01-01T00:00:10.000Z",
            "message": {"role": "user", "content": "Verify the billing suffix."}
        })
    )
    .unwrap();
    drop(transcript);
    drop(db);

    let rejected = open_project_session_db(&project).await.unwrap();
    set_projection_failure(&rejected, true).await;
    let failed = try_ingest_source(&rejected, &source, &project, None).await;
    assert!(
        failed.is_err(),
        "projection failure must surface as an ingest error"
    );
    assert_eq!(
        rejected.get_parse_offset(&path_key).await,
        Some(first_offset)
    );
    assert_eq!(rejected.session_message_count().await.unwrap(), 2);
    assert_eq!(
        rejected.get_session("claude", "claude-restart").await,
        Some(first_session)
    );
    assert!(rejected.get_session_message("claude", "u3").await.is_none());
    assert!(
        rejected
            .lcm_load_raw_message("claude", "u3")
            .await
            .is_none()
    );
    set_projection_failure(&rejected, false).await;
    drop(rejected);

    let reopened = open_project_session_db(&project).await.unwrap();
    let suffix = try_ingest_source(&reopened, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(suffix.messages_upserted, 1);
    let final_offset = reopened.get_parse_offset(&path_key).await.unwrap();
    assert!(final_offset.byte_offset > first_offset.byte_offset);
    assert_eq!(
        final_offset.byte_offset,
        std::fs::metadata(&path).unwrap().len()
    );
    drop(reopened);

    let replay = open_project_session_db(&project).await.unwrap();
    let unchanged = try_ingest_source(&replay, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(unchanged.sessions_upserted, 0);
    assert_eq!(unchanged.messages_upserted, 0);
    assert_eq!(replay.get_parse_offset(&path_key).await, Some(final_offset));
    assert_eq!(replay.session_message_count().await.unwrap(), 3);
}

#[tokio::test]
async fn claude_malformed_complete_frame_retries_suffix_without_gap_or_duplicate() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let path = write_claude_transcript(&home, &project, "claude-malformed-frame");
    let source = ClaudeSource::with_home(&home);
    let path_key = claude_cursor_key(&source, &project);
    let valid_prefix = std::fs::read_to_string(&path).unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let initial = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(initial.messages_upserted, 2);
    let prefix_offset = db.get_parse_offset(&path_key).await.unwrap();
    assert_eq!(prefix_offset.byte_offset, valid_prefix.len() as u64);
    drop(db);

    let suffix = serde_json::json!({
        "type": "user",
        "cwd": project,
        "sessionId": "claude-malformed-frame",
        "uuid": "u4",
        "timestamp": "2026-01-01T00:00:15.000Z",
        "message": {"role": "user", "content": "Valid suffix after malformed frame."}
    });
    std::fs::write(
        &path,
        format!("{valid_prefix}{{\"type\":\"user\",\"cwd\":}}\n{suffix}\n"),
    )
    .unwrap();

    let rejected = open_project_session_db(&project).await.unwrap();
    let malformed = try_ingest_source(&rejected, &source, &project, None).await;
    assert!(
        matches!(
            malformed,
            Err(TranscriptIngestError::NonDurableRecord {
                provider: "claude",
                ..
            })
        ),
        "malformed complete frame must surface as a non-durable scanner error, got {malformed:?}"
    );
    assert_eq!(
        rejected
            .get_parse_offset(&path_key)
            .await
            .unwrap()
            .byte_offset,
        prefix_offset.byte_offset
    );
    assert_eq!(rejected.session_message_count().await.unwrap(), 2);
    assert!(rejected.get_session_message("claude", "u4").await.is_none());
    drop(rejected);

    let repaired = serde_json::json!({
        "type": "user",
        "cwd": project,
        "sessionId": "claude-malformed-frame",
        "uuid": "u3",
        "timestamp": "2026-01-01T00:00:10.000Z",
        "message": {"role": "user", "content": "Recovered malformed frame."}
    });
    std::fs::write(&path, format!("{valid_prefix}{repaired}\n{suffix}\n")).unwrap();

    let retry = open_project_session_db(&project).await.unwrap();
    let recovered = try_ingest_source(&retry, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(recovered.messages_upserted, 2);
    assert_eq!(retry.session_message_count().await.unwrap(), 4);
    assert!(retry.get_session_message("claude", "u3").await.is_some());
    assert!(retry.get_session_message("claude", "u4").await.is_some());
    let final_offset = retry.get_parse_offset(&path_key).await.unwrap();
    assert_eq!(
        final_offset.byte_offset,
        std::fs::metadata(&path).unwrap().len()
    );
    drop(retry);

    let replay = open_project_session_db(&project).await.unwrap();
    let unchanged = try_ingest_source(&replay, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(unchanged.sessions_upserted, 0);
    assert_eq!(unchanged.messages_upserted, 0);
    assert_eq!(replay.get_parse_offset(&path_key).await, Some(final_offset));
    assert_eq!(replay.session_message_count().await.unwrap(), 4);
    assert!(replay.get_session_message("claude", "u4").await.is_some());
}

#[tokio::test]
async fn claude_restart_defers_a_partial_final_line() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let path = write_claude_transcript(&home, &project, "claude-partial");
    let source = ClaudeSource::with_home(&home);
    let path_key = claude_cursor_key(&source, &project);
    let complete_len = std::fs::metadata(&path).unwrap().len();
    let partial = serde_json::json!({
        "type": "user",
        "cwd": project,
        "sessionId": "claude-partial",
        "uuid": "u3",
        "timestamp": "2026-01-01T00:00:10.000Z",
        "message": {"role": "user", "content": "Deferred Claude partial line."}
    })
    .to_string();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(partial.as_bytes())
        .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let first = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(first.messages_upserted, 2);
    let committed_offset = db.get_parse_offset(&path_key).await.unwrap();
    assert_eq!(committed_offset.byte_offset, complete_len);
    drop(db);

    let reopened = open_project_session_db(&project).await.unwrap();
    let still_partial = try_ingest_source(&reopened, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(still_partial.messages_upserted, 0);
    assert_eq!(
        reopened.get_parse_offset(&path_key).await,
        Some(committed_offset)
    );

    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"\n")
        .unwrap();
    let completed = try_ingest_source(&reopened, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(completed.messages_upserted, 1);
    assert_eq!(reopened.session_message_count().await.unwrap(), 3);
    let final_offset = reopened.get_parse_offset(&path_key).await.unwrap();
    assert_eq!(
        final_offset.byte_offset,
        std::fs::metadata(&path).unwrap().len()
    );
    drop(reopened);

    let replay = open_project_session_db(&project).await.unwrap();
    let unchanged = try_ingest_source(&replay, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(unchanged.sessions_upserted, 0);
    assert_eq!(unchanged.messages_upserted, 0);
    assert_eq!(replay.get_parse_offset(&path_key).await, Some(final_offset));
    assert_eq!(replay.session_message_count().await.unwrap(), 3);
    assert!(replay.get_session_message("claude", "u3").await.is_some());
}

#[tokio::test]
async fn cline_content_hash_cursor_survives_restart_and_incomplete_rewrite() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let history_path = write_task(
        &vscode_storage_root(&home, "saoudrizwan.claude-dev"),
        &project,
        "cline-restart",
    );
    let source = ClineLikeSource::cline_with_home(&home);

    let db = open_project_session_db(&project).await.unwrap();
    let first = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    // user + assistant + usage companion rows
    assert_eq!(first.messages_upserted, 3);
    let offset = parse_offset_for_task_history(&db, &project, &history_path)
        .await
        .unwrap();
    let durable_count = db.session_message_count().await.unwrap();
    drop(db);

    let reopened = open_project_session_db(&project).await.unwrap();
    let replay = try_ingest_source(&reopened, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(replay.sessions_upserted, 0);
    assert_eq!(replay.messages_upserted, 0);
    assert_eq!(
        parse_offset_for_task_history(&reopened, &project, &history_path).await,
        Some(offset)
    );
    assert_eq!(
        reopened
            .search_session_messages("cline", None, "billing pipeline", 10)
            .await
            .len(),
        2
    );
    drop(reopened);

    std::fs::write(
        &history_path,
        r#"[{"role":"user","content":"Incomplete Cline rewrite."}"#,
    )
    .unwrap();
    let incomplete = open_project_session_db(&project).await.unwrap();
    let deferred = try_ingest_source(&incomplete, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(deferred.messages_upserted, 0);
    assert_eq!(
        incomplete.session_message_count().await.unwrap(),
        durable_count
    );
    // Incomplete JSON must not advance past the last successfully committed hash.
    assert_eq!(
        parse_offset_for_task_history(&incomplete, &project, &history_path).await,
        Some(offset)
    );
    assert!(
        incomplete
            .search_session_messages("cline", None, "Incomplete Cline rewrite", 10)
            .await
            .is_empty()
    );
    drop(incomplete);

    std::fs::write(
        &history_path,
        serde_json::to_string_pretty(&serde_json::json!([
            {
                "role": "user",
                "content": "Investigate the billing pipeline regression",
                "ts": 1_800_000_000_i64
            },
            {
                "role": "assistant",
                "content": "The billing pipeline regression is fixed.",
                "ts": 1_800_000_010_i64
            },
            {
                "role": "user",
                "content": "Verify the completed Cline rewrite.",
                "ts": 1_800_000_020_i64
            }
        ]))
        .unwrap(),
    )
    .unwrap();
    let completed = open_project_session_db(&project).await.unwrap();
    let completed_stats = try_ingest_source(&completed, &source, &project, None)
        .await
        .unwrap();
    assert!(completed_stats.messages_upserted > 0);
    assert_eq!(
        completed
            .search_session_messages("cline", None, "completed Cline rewrite", 10)
            .await
            .len(),
        1
    );
    assert_ne!(
        parse_offset_for_task_history(&completed, &project, &history_path)
            .await
            .unwrap(),
        offset
    );
}

#[tokio::test]
async fn cursor_restart_is_idempotent_and_ingests_only_the_appended_suffix() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let transcript_path = tmp.path().join("cursor-restart.jsonl");
    std::fs::write(
        &transcript_path,
        "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"First restart message.\"}]}}\n",
    )
    .unwrap();
    let event = serde_json::json!({
        "session_id": "cursor-restart",
        "transcript_path": transcript_path,
        "workspace_roots": [project]
    });

    let db = open_project_session_db(&project).await.unwrap();
    let first =
        ingest_cursor_transcript_event(&event.to_string(), &db, test_project_id(&project)).await;
    assert_eq!(first.messages_upserted, 1);
    let first_cursor =
        observation_source_cursor_position(&db, "cursor", "cursor-restart", &project)
            .await
            .expect("cursor observation frontier after first ingest");
    assert_no_transcript_adjacent_fallback_writer(&db, &transcript_path);
    drop(db);

    let reopened = open_project_session_db(&project).await.unwrap();
    let unchanged =
        ingest_cursor_transcript_event(&event.to_string(), &reopened, test_project_id(&project))
            .await;
    assert_eq!(unchanged.sessions_upserted, 0);
    assert_eq!(unchanged.messages_upserted, 0);
    assert_eq!(
        observation_source_cursor_position(&reopened, "cursor", "cursor-restart", &project).await,
        Some(first_cursor)
    );

    let mut transcript = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript_path)
        .unwrap();
    transcript
        .write_all(
            b"{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Second restart message.\"}]}}\n",
        )
        .unwrap();
    drop(transcript);
    drop(reopened);

    let catchup = open_project_session_db(&project).await.unwrap();
    let suffix =
        ingest_cursor_transcript_event(&event.to_string(), &catchup, test_project_id(&project))
            .await;
    assert_eq!(suffix.messages_upserted, 1);
    let final_cursor =
        observation_source_cursor_position(&catchup, "cursor", "cursor-restart", &project)
            .await
            .expect("cursor observation frontier after suffix");
    assert!(final_cursor > first_cursor);
    assert_eq!(
        catchup
            .search_session_messages("cursor", None, "restart message", 10)
            .await
            .len(),
        2
    );
}

#[tokio::test]
async fn cursor_restart_defers_a_partial_final_line() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let transcript_path = tmp.path().join("cursor-partial.jsonl");
    let complete = "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Complete partial test line.\"}]}}\n";
    let partial = "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Deferred partial test line.\"}]}}";
    std::fs::write(&transcript_path, format!("{complete}{partial}")).unwrap();
    let event = serde_json::json!({
        "session_id": "cursor-partial",
        "transcript_path": transcript_path,
        "workspace_roots": [project]
    });

    let db = open_project_session_db(&project).await.unwrap();
    let first =
        ingest_cursor_transcript_event(&event.to_string(), &db, test_project_id(&project)).await;
    assert_eq!(first.messages_upserted, 1);
    let committed_cursor =
        observation_source_cursor_position(&db, "cursor", "cursor-partial", &project)
            .await
            .expect("cursor observation frontier for complete prefix");
    assert_eq!(committed_cursor, complete.len() as u64);
    drop(db);

    let reopened = open_project_session_db(&project).await.unwrap();
    let still_partial =
        ingest_cursor_transcript_event(&event.to_string(), &reopened, test_project_id(&project))
            .await;
    assert_eq!(still_partial.messages_upserted, 0);
    assert_eq!(
        observation_source_cursor_position(&reopened, "cursor", "cursor-partial", &project).await,
        Some(committed_cursor)
    );

    let mut transcript = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript_path)
        .unwrap();
    transcript.write_all(b"\n").unwrap();
    drop(transcript);

    let completed =
        ingest_cursor_transcript_event(&event.to_string(), &reopened, test_project_id(&project))
            .await;
    assert_eq!(completed.messages_upserted, 1);
    assert_eq!(
        reopened
            .search_session_messages("cursor", None, "partial test line", 10)
            .await
            .len(),
        2
    );
}

#[tokio::test]
async fn claude_incremental_ingest_converges_with_clean_rebuild() {
    let incremental_tmp = TempDir::new().unwrap();
    let (incremental_home, incremental_project) = setup(&incremental_tmp);
    let incremental_path = write_claude_transcript(
        &incremental_home,
        &incremental_project,
        "claude-convergence",
    );
    let incremental_source = ClaudeSource::with_home(&incremental_home);
    let incremental_db = open_project_session_db(&incremental_project).await.unwrap();
    assert_eq!(
        try_ingest_source(
            &incremental_db,
            &incremental_source,
            &incremental_project,
            None,
        )
        .await
        .unwrap()
        .messages_upserted,
        2
    );
    let suffix = serde_json::json!({
        "type": "user",
        "cwd": incremental_project,
        "sessionId": "claude-convergence",
        "uuid": "u3",
        "timestamp": "2026-01-01T00:00:10.000Z",
        "message": {"role": "user", "content": "Verify billing convergence."}
    });
    writeln!(
        std::fs::OpenOptions::new()
            .append(true)
            .open(&incremental_path)
            .unwrap(),
        "{suffix}"
    )
    .unwrap();
    assert_eq!(
        try_ingest_source(
            &incremental_db,
            &incremental_source,
            &incremental_project,
            None,
        )
        .await
        .unwrap()
        .messages_upserted,
        1
    );
    let mut incremental_messages = incremental_db
        .search_session_messages("claude", None, "billing", 10)
        .await
        .into_iter()
        .map(|hit| (hit.message.message_id, hit.message.role, hit.message.text))
        .collect::<Vec<_>>();
    incremental_messages.sort();
    drop(incremental_db);

    let rebuild_tmp = TempDir::new().unwrap();
    let (rebuild_home, rebuild_project) = setup(&rebuild_tmp);
    let rebuild_path =
        write_claude_transcript(&rebuild_home, &rebuild_project, "claude-convergence");
    let rebuild_suffix = serde_json::json!({
        "type": "user",
        "cwd": rebuild_project,
        "sessionId": "claude-convergence",
        "uuid": "u3",
        "timestamp": "2026-01-01T00:00:10.000Z",
        "message": {"role": "user", "content": "Verify billing convergence."}
    });
    writeln!(
        std::fs::OpenOptions::new()
            .append(true)
            .open(&rebuild_path)
            .unwrap(),
        "{rebuild_suffix}"
    )
    .unwrap();
    let rebuild_source = ClaudeSource::with_home(&rebuild_home);
    let rebuild_db = open_project_session_db(&rebuild_project).await.unwrap();
    assert_eq!(
        try_ingest_source(&rebuild_db, &rebuild_source, &rebuild_project, None)
            .await
            .unwrap()
            .messages_upserted,
        3
    );

    let mut rebuilt_messages = rebuild_db
        .search_session_messages("claude", None, "billing", 10)
        .await
        .into_iter()
        .map(|hit| (hit.message.message_id, hit.message.role, hit.message.text))
        .collect::<Vec<_>>();
    rebuilt_messages.sort();
    assert_eq!(incremental_messages, rebuilt_messages);
    assert_eq!(incremental_messages.len(), 3);
}

#[tokio::test]
async fn cursor_malformed_frame_is_covered_and_valid_suffix_commits_once() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let transcript_path = tmp.path().join("cursor-malformed.jsonl");
    let valid = "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Valid prefix before malformed.\"}]}}\n";
    std::fs::write(&transcript_path, valid).unwrap();
    let event = serde_json::json!({
        "session_id": "cursor-malformed",
        "transcript_path": transcript_path,
        "workspace_roots": [project]
    });

    let db = open_project_session_db(&project).await.unwrap();
    assert_eq!(
        ingest_cursor_transcript_event(&event.to_string(), &db, test_project_id(&project))
            .await
            .messages_upserted,
        1
    );
    let prefix_cursor =
        observation_source_cursor_position(&db, "cursor", "cursor-malformed", &project)
            .await
            .unwrap();

    let suffix = "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Recovered suffix after malformed.\"}]}}\n";
    std::fs::write(
        &transcript_path,
        format!("{valid}{{\"role\":\"user\",\"message\":}}\n{suffix}"),
    )
    .unwrap();
    let covered =
        try_ingest_cursor_transcript_event(&event.to_string(), &db, test_project_id(&project))
            .await
            .expect("complete malformed frame must be covered");
    assert_eq!(covered.messages_upserted, 1);
    let covered_cursor =
        observation_source_cursor_position(&db, "cursor", "cursor-malformed", &project)
            .await
            .unwrap();
    assert!(covered_cursor > prefix_cursor);
    assert_eq!(
        db.search_session_messages("cursor", None, "Recovered", 10)
            .await
            .len(),
        1
    );

    let late = "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Late frame after covered malformed input.\"}]}}\n";
    std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript_path)
        .unwrap()
        .write_all(late.as_bytes())
        .unwrap();
    let recovered =
        try_ingest_cursor_transcript_event(&event.to_string(), &db, test_project_id(&project))
            .await
            .expect("late append after covered malformed frame must ingest");
    assert_eq!(recovered.messages_upserted, 1);
    assert_eq!(db.session_message_count().await.unwrap(), 3);
    assert_eq!(
        db.search_session_messages("cursor", None, "Recovered suffix", 10)
            .await
            .len(),
        1
    );
    assert_eq!(
        db.search_session_messages("cursor", None, "Late frame", 10)
            .await
            .len(),
        1
    );
    let final_cursor =
        observation_source_cursor_position(&db, "cursor", "cursor-malformed", &project)
            .await
            .unwrap();
    assert!(final_cursor > prefix_cursor);
    assert_eq!(
        final_cursor,
        std::fs::metadata(&transcript_path).unwrap().len()
    );
}

#[tokio::test]
async fn cursor_blank_unsupported_and_oversized_frames_are_covered() {
    const OVERSIZED_CURSOR_FRAME_BYTES: usize = 16 * 1024 * 1024 + 1;

    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let transcript_path = tmp.path().join("cursor-covered-frames.jsonl");
    let prefix = b"{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Cursor covered-frame prefix.\"}]}}\n";
    let unsupported = b"{\"type\":\"unknown_cursor_record\",\"payload\":{\"opaque\":true}}\n";
    let suffix = b"{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Cursor suffix after covered frames.\"}]}}\n";
    let mut transcript = std::fs::File::create(&transcript_path).unwrap();
    transcript.write_all(prefix).unwrap();
    transcript.write_all(b"\n").unwrap();
    transcript.write_all(unsupported).unwrap();
    transcript
        .write_all(&vec![b'x'; OVERSIZED_CURSOR_FRAME_BYTES])
        .unwrap();
    transcript.write_all(b"\n").unwrap();
    transcript.write_all(suffix).unwrap();
    drop(transcript);

    let event = serde_json::json!({
        "session_id": "cursor-covered-frames",
        "transcript_path": transcript_path,
        "workspace_roots": [project]
    });
    let db = open_project_session_db(&project).await.unwrap();
    let covered =
        try_ingest_cursor_transcript_event(&event.to_string(), &db, test_project_id(&project))
            .await
            .expect("non-durable complete frames must be covered");
    assert_eq!(covered.messages_upserted, 1);
    assert!(covered.source_deferred);
    let mut messages_upserted = covered.messages_upserted;
    let mut deferred = covered.source_deferred;
    for _ in 0..3 {
        if !deferred {
            break;
        }
        let catchup =
            try_ingest_cursor_transcript_event(&event.to_string(), &db, test_project_id(&project))
                .await
                .expect("bounded follow-up pass must make durable progress");
        assert!(catchup.bytes_consumed > 0);
        messages_upserted += catchup.messages_upserted;
        deferred = catchup.source_deferred;
    }
    assert_eq!(messages_upserted, 2);
    assert!(!deferred, "bounded passes must reach the valid suffix");
    assert_eq!(db.session_message_count().await.unwrap(), 2);
    assert_eq!(
        db.search_session_messages("cursor", None, "suffix", 10)
            .await
            .len(),
        1
    );
    assert_eq!(
        observation_source_cursor_position(&db, "cursor", "cursor-covered-frames", &project,).await,
        Some(std::fs::metadata(&transcript_path).unwrap().len())
    );
    assert_eq!(
        ingest_cursor_transcript_event(&event.to_string(), &db, test_project_id(&project))
            .await
            .messages_upserted,
        0
    );
}

#[tokio::test]
async fn cursor_truncation_replacement_preserves_distinct_generations() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let transcript_path = tmp.path().join("cursor-replaced.jsonl");
    std::fs::write(
        &transcript_path,
        "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"orig-cursor-repl-9f3a first generation.\"}]}}\n",
    )
    .unwrap();
    let event = serde_json::json!({
        "session_id": "cursor-replaced",
        "transcript_path": transcript_path,
        "workspace_roots": [project]
    });

    let db = open_project_session_db(&project).await.unwrap();
    assert_eq!(
        ingest_cursor_transcript_event(&event.to_string(), &db, test_project_id(&project))
            .await
            .messages_upserted,
        1
    );
    assert_eq!(
        db.search_session_messages("cursor", None, "orig-cursor-repl-9f3a", 10)
            .await
            .len(),
        1
    );

    // Truncate/replace the whole file (new head fingerprint / generation).
    std::fs::write(
        &transcript_path,
        "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"repl-cursor-gen-9f3a second generation.\"}]}}\n{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"repl-cursor-catchup-9f3a reply.\"}]}}\n",
    )
    .unwrap();
    let replaced =
        ingest_cursor_transcript_event(&event.to_string(), &db, test_project_id(&project)).await;
    assert_eq!(replaced.messages_upserted, 2);
    assert_eq!(db.session_message_count().await.unwrap(), 3);
    assert_eq!(
        db.search_session_messages("cursor", None, "orig-cursor-repl-9f3a", 10)
            .await
            .len(),
        1
    );
    assert_eq!(
        db.search_session_messages("cursor", None, "repl-cursor-gen-9f3a", 10)
            .await
            .len(),
        1
    );
    assert_eq!(
        db.search_session_messages("cursor", None, "repl-cursor-catchup-9f3a", 10)
            .await
            .len(),
        1
    );
    // Exact replay is a durable no-op.
    assert_eq!(
        ingest_cursor_transcript_event(&event.to_string(), &db, test_project_id(&project))
            .await
            .messages_upserted,
        0
    );
}

#[tokio::test]
async fn cursor_commit_before_projection_ack_retries_without_duplicate() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let transcript_path = tmp.path().join("cursor-commit-before-ack.jsonl");
    let first_line = "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Committed before projection ack.\"}]}}\n";
    std::fs::write(&transcript_path, first_line).unwrap();
    let event = serde_json::json!({
        "session_id": "cursor-commit-before-ack",
        "transcript_path": transcript_path,
        "workspace_roots": [project]
    });

    // Fail projection on the first durable message write.
    let rejected = open_project_session_db(&project).await.unwrap();
    set_projection_failure(&rejected, true).await;
    let failed = try_ingest_cursor_transcript_event(
        &event.to_string(),
        &rejected,
        test_project_id(&project),
    )
    .await
    .expect_err("projection failure must surface");
    assert!(
        matches!(
            failed,
            TranscriptIngestError::Store(_)
                | TranscriptIngestError::InvalidFrameState { .. }
                | TranscriptIngestError::NonDurableRecord { .. }
                | TranscriptIngestError::HostAdmission { .. }
        ),
        "unexpected Cursor projection failure: {failed:?}"
    );
    assert_eq!(rejected.session_message_count().await.unwrap(), 0);
    // Admission commits the observation, receipt, cursor, and projection queue
    // before the V1 projection acknowledgement fails.
    let committed_cursor = first_line.len() as u64;
    assert_eq!(
        observation_source_cursor_position(
            &rejected,
            "cursor",
            "cursor-commit-before-ack",
            &project,
        )
        .await,
        Some(committed_cursor)
    );
    assert_eq!(durable_table_count(&rejected, "observations").await, 1);
    assert_eq!(
        durable_table_count(&rejected, "sanitization_receipts").await,
        1
    );
    assert_eq!(durable_table_count(&rejected, "projection_queue").await, 1);
    set_projection_failure(&rejected, false).await;
    drop(rejected);

    let retried = open_project_session_db(&project).await.unwrap();
    let recovered =
        try_ingest_cursor_transcript_event(&event.to_string(), &retried, test_project_id(&project))
            .await
            .expect("retry after projection repair");
    assert_eq!(recovered.messages_upserted, 1);
    assert_eq!(
        retried
            .search_session_messages("cursor", None, "Committed before projection", 10)
            .await
            .len(),
        1
    );
    assert_eq!(durable_table_count(&retried, "observations").await, 1);
    assert_eq!(
        durable_table_count(&retried, "sanitization_receipts").await,
        1
    );
    assert_eq!(durable_table_count(&retried, "projection_queue").await, 0);
    assert_eq!(
        observation_source_cursor_position(
            &retried,
            "cursor",
            "cursor-commit-before-ack",
            &project,
        )
        .await,
        Some(committed_cursor)
    );
    assert_eq!(
        ingest_cursor_transcript_event(&event.to_string(), &retried, test_project_id(&project))
            .await
            .messages_upserted,
        0
    );
    assert_eq!(durable_table_count(&retried, "observations").await, 1);
    assert_eq!(durable_table_count(&retried, "projection_queue").await, 0);
}

#[tokio::test]
async fn codex_restart_partial_malformed_and_crash_before_commit() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let path = write_codex_rollout_fixture(&home, &project, "codex-restart");
    let source = CodexSource::with_home(&home);

    let db = open_project_session_db(&project).await.unwrap();
    let first = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(first.messages_upserted, 2);
    assert_eq!(db.session_message_count().await.unwrap(), 2);
    let path_key = db
        .get_session("codex", "codex-restart")
        .await
        .expect("Codex session ingested")
        .transcript_path
        .expect("Codex durable transcript path");
    let first_offset = db.get_parse_offset(&path_key).await.unwrap();
    assert_no_transcript_adjacent_fallback_writer(&db, &path);
    drop(db);

    // Partial final line: frontier stays at the last complete frame.
    let prefix = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        format!(
            "{prefix}{{\"timestamp\":\"2026-01-01T00:00:03.000Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":\"Partial Codex"
        ),
    )
    .unwrap();
    let partial = open_project_session_db(&project).await.unwrap();
    assert_eq!(
        try_ingest_source(&partial, &source, &project, None)
            .await
            .unwrap()
            .messages_upserted,
        0
    );
    assert_eq!(
        partial
            .get_parse_offset(&path_key)
            .await
            .unwrap()
            .byte_offset,
        first_offset.byte_offset
    );
    drop(partial);

    // Complete the line, then inject a projection failure before the suffix commits.
    let suffix = serde_json::json!({
        "timestamp": "2026-01-01T00:00:03.000Z",
        "type": "event_msg",
        "payload": {"type": "user_message", "message": "Codex suffix after restart"}
    });
    std::fs::write(&path, format!("{prefix}{suffix}\n")).unwrap();
    let rejected = open_project_session_db(&project).await.unwrap();
    set_projection_failure(&rejected, true).await;
    let failed = try_ingest_source(&rejected, &source, &project, None).await;
    assert!(
        failed.is_err(),
        "projection failure must surface as an ingest error"
    );
    assert_eq!(
        rejected
            .get_parse_offset(&path_key)
            .await
            .unwrap()
            .byte_offset,
        first_offset.byte_offset
    );
    assert!(
        rejected
            .search_session_messages("codex", None, "Codex suffix after restart", 10)
            .await
            .is_empty()
    );
    set_projection_failure(&rejected, false).await;
    drop(rejected);

    let recovered = open_project_session_db(&project).await.unwrap();
    assert_eq!(
        try_ingest_source(&recovered, &source, &project, None)
            .await
            .unwrap()
            .messages_upserted,
        1
    );
    assert_eq!(
        recovered
            .search_session_messages("codex", None, "Codex suffix after restart", 10)
            .await
            .len(),
        1
    );

    // Malformed complete frame between known-good prefix and a later valid line:
    // classic Codex JSONL skip policy advances past the bad line without durable
    // rows for it, then ingests the valid suffix.
    let malformed_suffix = serde_json::json!({
        "timestamp": "2026-01-01T00:00:04.000Z",
        "type": "event_msg",
        "payload": {"type": "user_message", "message": "Codex after malformed frame"}
    });
    let current = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        format!("{current}{{\"type\":\"event_msg\",malformed}}\n{malformed_suffix}\n"),
    )
    .unwrap();
    assert_eq!(
        try_ingest_source(&recovered, &source, &project, None)
            .await
            .unwrap()
            .messages_upserted,
        1
    );
    assert_eq!(
        recovered
            .search_session_messages("codex", None, "malformed", 10)
            .await
            .len(),
        1
    );
    assert_eq!(
        try_ingest_source(&recovered, &source, &project, None)
            .await
            .unwrap()
            .messages_upserted,
        0
    );
}

#[tokio::test]
async fn legacy_cline_crash_before_commit_keeps_content_hash_frontier() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let history_path = write_task(
        &vscode_storage_root(&home, "saoudrizwan.claude-dev"),
        &project,
        "cline-crash",
    );
    let source = ClineLikeSource::cline_with_home(&home);

    let db = open_project_session_db(&project).await.unwrap();
    assert_eq!(
        try_ingest_source(&db, &source, &project, None)
            .await
            .unwrap()
            .messages_upserted,
        3
    );
    let offset = parse_offset_for_task_history(&db, &project, &history_path)
        .await
        .unwrap();
    drop(db);

    std::fs::write(
        &history_path,
        serde_json::to_string_pretty(&serde_json::json!([
            {
                "role": "user",
                "content": "Investigate the billing pipeline regression",
                "ts": 1_800_000_000_i64
            },
            {
                "role": "assistant",
                "content": "The billing pipeline regression is fixed.",
                "ts": 1_800_000_010_i64
            },
            {
                "role": "user",
                "content": "Crash before commit suffix for Cline.",
                "ts": 1_800_000_020_i64
            }
        ]))
        .unwrap(),
    )
    .unwrap();

    let rejected = open_project_session_db(&project).await.unwrap();
    set_projection_failure(&rejected, true).await;
    assert!(
        try_ingest_source(&rejected, &source, &project, None)
            .await
            .is_err(),
        "projection failure must surface as an ingest error"
    );
    assert!(
        rejected
            .search_session_messages("cline", None, "Crash before commit suffix", 10)
            .await
            .is_empty()
    );
    // Content-hash frontier must remain at the last successful commit.
    assert_eq!(
        parse_offset_for_task_history(&rejected, &project, &history_path).await,
        Some(offset)
    );
    set_projection_failure(&rejected, false).await;
    drop(rejected);

    let recovered = open_project_session_db(&project).await.unwrap();
    assert_eq!(
        try_ingest_source(&recovered, &source, &project, None)
            .await
            .unwrap()
            .messages_upserted,
        4
    );
    assert_eq!(
        recovered
            .search_session_messages("cline", None, "Crash before commit suffix", 10)
            .await
            .len(),
        1
    );
    assert_eq!(
        try_ingest_source(&recovered, &source, &project, None)
            .await
            .unwrap()
            .messages_upserted,
        0
    );
}

#[tokio::test]
async fn claude_and_codex_jsonl_truncation_replacement_preserves_prior_and_new_frames() {
    for provider in ["claude", "codex"] {
        let tmp = TempDir::new().unwrap();
        let (home, project) = setup(&tmp);
        let (path, source): (PathBuf, Box<dyn TranscriptSource>) = match provider {
            "claude" => {
                let path = write_claude_transcript(&home, &project, "claude-trunc-repl");
                (path, Box::new(ClaudeSource::with_home(&home)))
            }
            "codex" => {
                let path = write_codex_rollout_fixture(&home, &project, "codex-trunc-repl");
                (path, Box::new(CodexSource::with_home(&home)))
            }
            _ => unreachable!(),
        };

        let db = open_project_session_db(&project).await.unwrap();
        let first = try_ingest_source(&db, source.as_ref(), &project, None)
            .await
            .unwrap();
        assert!(
            first.messages_upserted >= 2,
            "{provider}: initial ingest must commit provider frames"
        );
        let first_count = db.session_message_count().await.unwrap();
        assert_eq!(
            db.search_session_messages(provider, None, "Investigate", 10)
                .await
                .len(),
            1,
            "{provider}"
        );
        drop(db);

        // Truncate/replace the whole JSONL file (new head fingerprint / generation).
        match provider {
            "claude" => {
                let cwd = project.to_string_lossy();
                std::fs::write(
                    &path,
                    format!(
                        "{}\n{}\n",
                        serde_json::json!({
                            "type": "user",
                            "cwd": cwd,
                            "sessionId": "claude-trunc-repl",
                            "uuid": "u-repl-1",
                            "timestamp": "2026-01-02T00:00:00.000Z",
                            "message": {
                                "role": "user",
                                "content": "claude-trunc-repl-9f3a second generation."
                            }
                        }),
                        serde_json::json!({
                            "type": "assistant",
                            "cwd": cwd,
                            "sessionId": "claude-trunc-repl",
                            "uuid": "u-repl-2",
                            "timestamp": "2026-01-02T00:00:05.000Z",
                            "message": {
                                "role": "assistant",
                                "content": [
                                    {
                                        "type": "text",
                                        "text": "claude-trunc-repl-catchup-9f3a reply."
                                    }
                                ]
                            }
                        }),
                    ),
                )
                .unwrap();
            }
            "codex" => {
                std::fs::write(
                    &path,
                    format!(
                        "{}\n{}\n{}\n",
                        serde_json::json!({
                            "timestamp": "2026-01-02T00:00:00.000Z",
                            "type": "session_meta",
                            "payload": {
                                "id": "codex-trunc-repl",
                                "cwd": project.to_string_lossy(),
                                "model": "gpt-5.5"
                            }
                        }),
                        serde_json::json!({
                            "timestamp": "2026-01-02T00:00:01.000Z",
                            "type": "event_msg",
                            "payload": {
                                "type": "user_message",
                                "message": "codex-trunc-repl-9f3a second generation."
                            }
                        }),
                        serde_json::json!({
                            "timestamp": "2026-01-02T00:00:02.000Z",
                            "type": "event_msg",
                            "payload": {
                                "type": "agent_message",
                                "message": "codex-trunc-repl-catchup-9f3a reply."
                            }
                        }),
                    ),
                )
                .unwrap();
            }
            _ => unreachable!(),
        }

        let replaced = open_project_session_db(&project).await.unwrap();
        let stats = try_ingest_source(&replaced, source.as_ref(), &project, None)
            .await
            .unwrap();
        assert_eq!(stats.messages_upserted, 2, "{provider}");
        assert_eq!(
            replaced.session_message_count().await.unwrap(),
            first_count + 2,
            "{provider}: prior generation rows must remain searchable"
        );
        assert_eq!(
            replaced
                .search_session_messages(provider, None, "Investigate", 10)
                .await
                .len(),
            1,
            "{provider}"
        );
        assert_eq!(
            replaced
                .search_session_messages(provider, None, &format!("{provider}-trunc-repl-9f3a"), 10)
                .await
                .len(),
            1,
            "{provider}"
        );
        assert_eq!(
            replaced
                .search_session_messages(
                    provider,
                    None,
                    &format!("{provider}-trunc-repl-catchup-9f3a"),
                    10
                )
                .await
                .len(),
            1,
            "{provider}"
        );
        assert_eq!(
            try_ingest_source(&replaced, source.as_ref(), &project, None)
                .await
                .unwrap()
                .messages_upserted,
            0,
            "{provider}: exact replay must be a durable no-op"
        );
    }
}

#[tokio::test]
async fn codex_incomplete_tail_retained_across_append_then_completes() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let path = write_codex_rollout_fixture(&home, &project, "codex-partial-append");
    let source = CodexSource::with_home(&home);

    let db = open_project_session_db(&project).await.unwrap();
    assert_eq!(
        try_ingest_source(&db, &source, &project, None)
            .await
            .unwrap()
            .messages_upserted,
        2
    );
    let path_key = db
        .get_session("codex", "codex-partial-append")
        .await
        .expect("Codex session ingested")
        .transcript_path
        .expect("Codex durable transcript path");
    let committed = db.get_parse_offset(&path_key).await.unwrap();
    drop(db);

    let partial = serde_json::json!({
        "timestamp": "2026-01-01T00:00:03.000Z",
        "type": "event_msg",
        "payload": {"type": "user_message", "message": "Codex deferred partial append line"}
    })
    .to_string();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(partial.as_bytes())
        .unwrap();

    let still_open = open_project_session_db(&project).await.unwrap();
    assert_eq!(
        try_ingest_source(&still_open, &source, &project, None)
            .await
            .unwrap()
            .messages_upserted,
        0
    );
    assert_eq!(
        still_open
            .get_parse_offset(&path_key)
            .await
            .unwrap()
            .byte_offset,
        committed.byte_offset
    );
    drop(still_open);

    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"\n")
        .unwrap();

    let completed = open_project_session_db(&project).await.unwrap();
    assert_eq!(
        try_ingest_source(&completed, &source, &project, None)
            .await
            .unwrap()
            .messages_upserted,
        1
    );
    assert_eq!(
        completed
            .search_session_messages("codex", None, "deferred partial append", 10)
            .await
            .len(),
        1
    );
    let final_offset = completed.get_parse_offset(&path_key).await.unwrap();
    assert_eq!(
        final_offset.byte_offset,
        std::fs::metadata(&path).unwrap().len()
    );
    assert_eq!(
        try_ingest_source(&completed, &source, &project, None)
            .await
            .unwrap()
            .messages_upserted,
        0
    );
}

#[tokio::test]
async fn cursor_jsonl_rotation_rename_rescans_replacement_without_gap() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let transcript_path = tmp.path().join("cursor-rotated.jsonl");
    std::fs::write(
        &transcript_path,
        "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"orig-cursor-rot-9f3a first generation.\"}]}}\n",
    )
    .unwrap();
    let event = serde_json::json!({
        "session_id": "cursor-rotated",
        "transcript_path": transcript_path,
        "workspace_roots": [project]
    });

    let db = open_project_session_db(&project).await.unwrap();
    assert_eq!(
        ingest_cursor_transcript_event(&event.to_string(), &db, test_project_id(&project))
            .await
            .messages_upserted,
        1
    );

    // Rotate: rename the active handle aside, then write a replacement at the
    // same path (new file identity / generation).
    let archived = tmp.path().join("cursor-rotated.archived.jsonl");
    std::fs::rename(&transcript_path, &archived).unwrap();
    std::fs::write(
        &transcript_path,
        "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"rot-cursor-gen-9f3a second generation.\"}]}}\n{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"rot-cursor-catchup-9f3a reply.\"}]}}\n",
    )
    .unwrap();

    let rotated =
        ingest_cursor_transcript_event(&event.to_string(), &db, test_project_id(&project)).await;
    assert_eq!(rotated.messages_upserted, 2);
    assert_eq!(db.session_message_count().await.unwrap(), 3);
    assert_eq!(
        db.search_session_messages("cursor", None, "orig-cursor-rot-9f3a", 10)
            .await
            .len(),
        1
    );
    assert_eq!(
        db.search_session_messages("cursor", None, "rot-cursor-gen-9f3a", 10)
            .await
            .len(),
        1
    );
    assert_eq!(
        db.search_session_messages("cursor", None, "rot-cursor-catchup-9f3a", 10)
            .await
            .len(),
        1
    );
    assert_eq!(
        ingest_cursor_transcript_event(&event.to_string(), &db, test_project_id(&project))
            .await
            .messages_upserted,
        0
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn codex_observation_commit_before_ack_replays_after_reopen() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let _home = EnvVarGuard::set("HOME", &home);
    init_git_repo(&project);
    mark_test_project(&project);
    let path = write_codex_rollout_fixture(&home, &project, "codex-commit-before-ack");

    let rejected = open_project_session_db(&project).await.unwrap();
    assert_no_transcript_adjacent_fallback_writer(&rejected, &path);
    set_projection_failure(&rejected, true).await;
    let _ =
        ingest_global_sources_for_provider(&rejected, &project, Some(SessionProvider::Codex)).await;
    assert_eq!(rejected.session_message_count().await.unwrap(), 0);
    assert!(
        rejected
            .search_session_messages("codex", None, "billing pipeline regression", 10)
            .await
            .is_empty()
    );
    let committed_cursor =
        observation_source_cursor_position(&rejected, "codex", "codex-commit-before-ack", &project)
            .await
            .expect("Codex observation frontier commits before projection ack");
    assert!(committed_cursor > 0);
    assert!(durable_table_count(&rejected, "observations").await >= 1);
    assert!(durable_table_count(&rejected, "sanitization_receipts").await >= 1);
    assert!(durable_table_count(&rejected, "projection_queue").await >= 1);
    set_projection_failure(&rejected, false).await;
    drop(rejected);

    let recovered = open_project_session_db(&project).await.unwrap();
    assert_eq!(
        ingest_global_sources_for_provider(&recovered, &project, Some(SessionProvider::Codex))
            .await
            .messages_upserted,
        2
    );
    assert_eq!(
        recovered
            .search_session_messages("codex", None, "fixed", 10)
            .await
            .len(),
        1
    );
    assert_eq!(
        observation_source_cursor_position(
            &recovered,
            "codex",
            "codex-commit-before-ack",
            &project,
        )
        .await,
        Some(committed_cursor)
    );
    assert_eq!(durable_table_count(&recovered, "projection_queue").await, 0);
    assert_eq!(
        ingest_global_sources_for_provider(&recovered, &project, Some(SessionProvider::Codex))
            .await
            .messages_upserted,
        0
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn claude_observation_commit_before_ack_replays_after_reopen() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let _home = EnvVarGuard::set("HOME", &home);
    init_git_repo(&project);
    mark_test_project(&project);
    let path = write_claude_transcript(&home, &project, "claude-commit-before-ack");

    let rejected = open_project_session_db(&project).await.unwrap();
    assert_no_transcript_adjacent_fallback_writer(&rejected, &path);
    set_projection_failure(&rejected, true).await;
    let _ = ingest_global_sources_for_provider(&rejected, &project, Some(SessionProvider::Claude))
        .await;
    assert_eq!(rejected.session_message_count().await.unwrap(), 0);
    assert!(
        rejected
            .search_session_messages("claude", None, "billing pipeline regression", 10)
            .await
            .is_empty()
    );
    let observations = durable_table_count(&rejected, "observations").await;
    let receipts = durable_table_count(&rejected, "sanitization_receipts").await;
    let queued = durable_table_count(&rejected, "projection_queue").await;
    assert!(
        observations >= 1,
        "Claude observation commits before projection ack"
    );
    assert!(
        receipts >= 1,
        "Claude sanitization receipts commit with observations"
    );
    assert!(
        queued >= 1,
        "Claude projection work stays queued across the failed ack"
    );
    set_projection_failure(&rejected, false).await;
    drop(rejected);

    let recovered = open_project_session_db(&project).await.unwrap();
    assert_eq!(
        ingest_global_sources_for_provider(&recovered, &project, Some(SessionProvider::Claude))
            .await
            .messages_upserted,
        2
    );
    assert_eq!(
        recovered
            .search_session_messages("claude", None, "fixed", 10)
            .await
            .len(),
        1
    );
    assert_eq!(
        durable_table_count(&recovered, "observations").await,
        observations
    );
    assert_eq!(
        durable_table_count(&recovered, "sanitization_receipts").await,
        receipts
    );
    assert_eq!(durable_table_count(&recovered, "projection_queue").await, 0);
    assert_eq!(
        ingest_global_sources_for_provider(&recovered, &project, Some(SessionProvider::Claude))
            .await
            .messages_upserted,
        0
    );
}
