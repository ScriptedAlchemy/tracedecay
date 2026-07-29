#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tracedecay_domain::ProjectId;

use crate::application::host_admission::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
use crate::application::observation::ObservationCancellation;
use crate::sessions::shared::TranscriptIngestStats;
use crate::sessions::source::{
    FileDiscoveryReport, ParsedTranscript, SessionDraft, StoredCursor, TranscriptDiscoveryBounds,
    TranscriptIngestResult, TranscriptSource,
};
use crate::sessions::{SessionProvider, claude_observation, codex, git_correlation, source};

use super::failure::{
    IngestPassBounds, IngestPassCoverage, IngestPassOutcome, allocate_pass_byte_budgets,
    classify_claude_observation_failure, classify_transcript_ingest_failure,
    plan_round_robin_admission, scheduling_write_required,
};
use super::project::{
    home_dir, ingest_project_sources_for_provider_without_registered_authority,
    parse_git_log_commits, push_file_source, with_transcript_source_home,
};
use super::scheduler::{
    DiscoveredIngestUnit, TRANSCRIPT_INGEST_SOURCE_FRONTIER_KEY, TransientIngestAuthority,
    admit_fair_ingest_units, discover_ingest_units, finish_user_provider_coverage,
    ingest_sources_bounded as ingest_sources_bounded_with_store,
    merge_project_provider_backpressure, plan_user_provider_admission,
    read_ingest_frontier as read_ingest_frontier_with_store,
};
use super::startup::{
    StartupUserIngestGuard, TranscriptIngestOutcome,
    ingest_user_global_sources_for_startup_with_db,
    ingest_user_global_sources_for_startup_with_db_without_registered_authority,
};
use super::user::{
    ingest_user_global_sources_for_provider_with_roots_bounded,
    ingest_user_global_sources_for_provider_with_roots_without_registered_authority,
    provider_selected,
};
use super::user_provider::try_ingest_file_source_bounded;

const TEST_INGEST_BOUNDS: IngestPassBounds = IngestPassBounds {
    discovered_units: 16,
    units_per_pass: 8,
    units_per_source: 8,
    queue_depth: 8,
    bytes_per_unit: 1024,
    bytes_per_pass: 4096,
    retries: 0,
};

async fn ingest_sources_bounded(
    db: &crate::global_db::RegisteredGlobalDb,
    project_root: &Path,
    project_id: &ProjectId,
    sources: &[Box<dyn TranscriptSource>],
    bounds: IngestPassBounds,
    cancellation: &ObservationCancellation,
) -> IngestPassOutcome {
    let store = crate::store::GlobalDbTranscriptStore::new(db);
    let authority = TransientIngestAuthority::new(
        db.binding().shard_id.brain_id.clone(),
        db.binding().shard_id.profile_id.clone(),
        project_id,
        sources,
    );
    ingest_sources_bounded_with_store(
        &store,
        &authority,
        project_root,
        sources,
        bounds,
        cancellation,
    )
    .await
}

async fn read_ingest_frontier(db: &crate::global_db::RegisteredGlobalDb, key: &str) -> Option<u64> {
    let store = crate::store::GlobalDbTranscriptStore::new(db);
    read_ingest_frontier_with_store(&store, key).await
}

#[tokio::test]
async fn scoped_transcript_source_home_overrides_ambient_home_without_mutating_it() {
    let isolated_home = tempfile::tempdir().unwrap();
    let ambient_home = std::env::var_os("HOME");

    let resolved =
        with_transcript_source_home(isolated_home.path().to_path_buf(), async { home_dir() }).await;

    assert_eq!(resolved.as_deref(), Some(isolated_home.path()));
    assert_eq!(std::env::var_os("HOME"), ambient_home);
}

fn scheduler_test_project_id() -> ProjectId {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    ProjectId::new(format!(
        "scheduler-test-{}",
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    ))
    .unwrap()
}

async fn profile_test_runtime(temp: &tempfile::TempDir) -> HostAdmissionTestRuntimeV1 {
    HostAdmissionTestRuntimeV1::profile(temp.path().join("profile"))
        .await
        .unwrap()
}

async fn project_test_runtime(
    temp: &tempfile::TempDir,
    project_root: &Path,
    project_id: ProjectId,
) -> HostAdmissionTestRuntimeV1 {
    HostAdmissionTestRuntimeV1::project(temp.path().join("profile"), project_root, project_id)
        .await
        .unwrap()
}

#[tokio::test]
async fn missing_project_identity_fails_before_ingest_writes() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = profile_test_runtime(&temp).await;
    let db = runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap();

    let outcome = ingest_project_sources_for_provider_without_registered_authority(
        db,
        temp.path(),
        None,
        None,
        true,
    )
    .await;

    assert_eq!(outcome.failures.len(), 1);
    assert_eq!(outcome.failures[0].reason_code, "project_identity_missing");
    assert_eq!(outcome.stats, TranscriptIngestStats::default());
    assert!(
        db.get_parse_offset_result(TRANSCRIPT_INGEST_SOURCE_FRONTIER_KEY)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn unregistered_project_authority_fails_before_ingest_writes() {
    let temp = tempfile::tempdir().unwrap();
    let project_id = scheduler_test_project_id();
    let runtime = project_test_runtime(&temp, temp.path(), project_id.clone()).await;
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();

    let outcome = ingest_project_sources_for_provider_without_registered_authority(
        db,
        temp.path(),
        Some(project_id),
        Some(SessionProvider::Vibe),
        true,
    )
    .await;

    assert_eq!(outcome.failures.len(), 1);
    assert_eq!(
        outcome.failures[0].reason_code,
        "registered_authority_unavailable"
    );
    assert_eq!(outcome.stats, TranscriptIngestStats::default());
    assert!(
        db.get_parse_offset_result(TRANSCRIPT_INGEST_SOURCE_FRONTIER_KEY)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn mismatched_project_id_fails_before_scheduler_reads_or_writes() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let mounted_project_id = scheduler_test_project_id();
    let requested_project_id = scheduler_test_project_id();
    let runtime = project_test_runtime(&temp, &project, mounted_project_id.clone()).await;
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();

    let outcome = ingest_sources_bounded(
        db,
        &project,
        &requested_project_id,
        &[],
        TEST_INGEST_BOUNDS,
        &ObservationCancellation::default(),
    )
    .await;

    assert_eq!(outcome.failures.len(), 1);
    assert_eq!(
        outcome.failures[0].reason_code,
        "project_sessions_authority_mismatch"
    );
    assert_eq!(
        db.get_parse_offset_result(TRANSCRIPT_INGEST_SOURCE_FRONTIER_KEY)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        db.binding().shard_id.scope,
        tracedecay_store::StoreShardScopeV1::ProjectSessions {
            project_id: mounted_project_id
        }
    );
}

#[tokio::test]
async fn unregistered_profile_authority_fails_before_ingest_writes() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = profile_test_runtime(&temp).await;
    let db = runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap();

    let outcome = ingest_user_global_sources_for_provider_with_roots_without_registered_authority(
        db,
        temp.path(),
        Some(SessionProvider::Codex),
        Vec::new(),
    )
    .await;

    assert_eq!(outcome.failures.len(), 1);
    assert_eq!(
        outcome.failures[0].reason_code,
        "registered_authority_unavailable"
    );
    assert_eq!(outcome.stats, TranscriptIngestStats::default());
    assert!(
        db.get_parse_offset_result(TRANSCRIPT_INGEST_SOURCE_FRONTIER_KEY)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn cancelled_user_pass_reports_partial_coverage() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = profile_test_runtime(&temp).await;
    let db = runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap();
    let cancellation = ObservationCancellation::default();
    cancellation.cancel();

    let outcome = ingest_user_global_sources_for_provider_with_roots_bounded(
        (
            &db.binding().shard_id.brain_id,
            &db.binding().shard_id.profile_id,
            db,
        ),
        temp.path(),
        None,
        Vec::new(),
        TEST_INGEST_BOUNDS,
        &cancellation,
    )
    .await;

    assert_eq!(outcome.units_admitted, 0);
    assert_eq!(
        outcome.coverage,
        IngestPassCoverage::Partial { deferred_units: 9 }
    );
    assert!(
        outcome
            .failures
            .iter()
            .any(|failure| failure.reason_code == "ingest_pass_cancelled")
    );
}

#[tokio::test]
async fn cancelled_startup_user_ingest_stops_before_registry_reads() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = profile_test_runtime(&temp).await;
    let db = runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap();
    let cancellation = ObservationCancellation::default();
    cancellation.cancel();

    let outcome = ingest_user_global_sources_for_startup_with_db(
        &db.binding().shard_id.brain_id,
        &db.binding().shard_id.profile_id,
        db,
        db,
        temp.path(),
        &cancellation,
    )
    .await;

    assert_eq!(outcome.stats, TranscriptIngestStats::default());
    assert!(
        outcome
            .failures
            .iter()
            .any(|failure| failure.reason_code == "ingest_pass_cancelled")
    );
}

#[tokio::test]
async fn cancelled_codex_provider_stops_before_opening_the_next_jsonl_source() {
    let temp = tempfile::tempdir().unwrap();
    let project_id = scheduler_test_project_id();
    let runtime = project_test_runtime(&temp, temp.path(), project_id.clone()).await;
    let facade = runtime.facade();
    let cancellation = ObservationCancellation::default();
    cancellation.cancel();

    let outcome =
        codex::try_admit_codex_jsonl_observations_for_project_with_admission_and_cancellation(
            &temp.path().join("must-not-open.jsonl"),
            temp.path(),
            project_id,
            &facade,
            None,
            &cancellation,
        )
        .await
        .unwrap();

    assert_eq!(outcome.bytes_consumed, 0);
    assert!(outcome.source_deferred);
}

#[tokio::test]
async fn unregistered_startup_authority_fails_before_registry_reads() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = profile_test_runtime(&temp).await;
    let db = runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap();

    let outcome = ingest_user_global_sources_for_startup_with_db_without_registered_authority(
        db,
        db,
        temp.path(),
    )
    .await;

    assert_eq!(outcome.failures.len(), 1);
    assert_eq!(
        outcome.failures[0].reason_code,
        "registered_authority_unavailable"
    );
    assert_eq!(outcome.stats, TranscriptIngestStats::default());
}

#[tokio::test]
async fn registered_project_roots_include_modern_registry_aliases() {
    let temp = tempfile::tempdir().unwrap();
    let canonical = temp.path().join("repo");
    let worktree = temp.path().join("repo-worktree");
    std::fs::create_dir_all(&canonical).unwrap();
    std::fs::create_dir_all(&worktree).unwrap();
    let canonical = std::fs::canonicalize(canonical).unwrap();
    let worktree = std::fs::canonicalize(worktree).unwrap();
    let runtime = profile_test_runtime(&temp).await;
    runtime
        .upsert_code_project("project-1", &canonical, None, None, None)
        .await
        .unwrap();
    runtime
        .upsert_project_alias(&worktree, "project-1")
        .await
        .unwrap();
    let roots = runtime.registered_project_roots_for_test().await.unwrap();

    assert!(
        roots.contains(&canonical),
        "missing {canonical:?} from {roots:?}"
    );
    assert!(
        roots.contains(&worktree),
        "missing {worktree:?} from {roots:?}"
    );
}

// macOS filesystems reject invalid UTF-8 path components with EILSEQ.
#[cfg(all(unix, not(target_os = "macos")))]
#[tokio::test]
async fn registered_project_roots_preserve_non_unicode_current_root() {
    use std::os::unix::ffi::OsStringExt;

    let temp = tempfile::tempdir().unwrap();
    let root = temp
        .path()
        .join(std::ffi::OsString::from_vec(b"repo-\xff".to_vec()));
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(root).unwrap();
    let runtime = profile_test_runtime(&temp).await;
    runtime
        .upsert_code_project("project-native", &root, None, None, None)
        .await
        .unwrap();
    let roots = runtime.registered_project_paths_for_test().await.unwrap();

    assert!(roots.contains(&root));
}

#[test]
fn provider_scoped_user_catch_up_excludes_unrelated_providers() {
    assert!(provider_selected(
        Some(SessionProvider::Hermes),
        SessionProvider::Hermes
    ));
    for unrelated in [
        SessionProvider::Codex,
        SessionProvider::Cursor,
        SessionProvider::Claude,
        SessionProvider::Vibe,
        SessionProvider::Cline,
        SessionProvider::RooCode,
        SessionProvider::Kilo,
        SessionProvider::Kiro,
    ] {
        assert!(!provider_selected(Some(SessionProvider::Hermes), unrelated));
    }
    assert!(provider_selected(None, SessionProvider::Codex));
    assert!(provider_selected(None, SessionProvider::Hermes));
}

#[test]
fn migrated_providers_never_use_legacy_transcript_sources() {
    for provider in [
        SessionProvider::Claude,
        SessionProvider::Codex,
        SessionProvider::Cursor,
        SessionProvider::Hermes,
        SessionProvider::Kiro,
        SessionProvider::Cline,
        SessionProvider::RooCode,
        SessionProvider::Kilo,
    ] {
        let mut sources = Vec::new();
        push_file_source(&mut sources, provider);
        assert!(
            sources.is_empty(),
            "{} used the legacy source",
            provider.id()
        );
    }
}

#[test]
fn transcript_failure_classification_is_bounded_and_drives_outcome_success() {
    let error =
        source::TranscriptIngestError::Store(tracedecay_store::TranscriptStoreError::Storage {
            operation: "private operation",
            source: Box::new(std::io::Error::other("private source detail")),
        });
    let failure = classify_transcript_ingest_failure("codex", "transcript", &error);

    assert_eq!(failure.provider, "codex");
    assert_eq!(failure.source, "transcript");
    assert_eq!(failure.reason_code, "transcript_storage_failed");
    assert!(failure.retryable);
    let outcome = TranscriptIngestOutcome::new(TranscriptIngestStats::default(), vec![failure]);
    assert!(!outcome.coverage.is_complete() || !outcome.failures.is_empty());
    let rendered = serde_json::to_string(&outcome.failures).unwrap();
    assert!(!rendered.contains("private operation"));
    assert!(!rendered.contains("private source detail"));
}

#[test]
fn bounded_pass_coverage_survives_transcript_outcome_boundary() {
    let partial = IngestPassOutcome {
        stats: TranscriptIngestStats {
            sessions_upserted: 1,
            messages_upserted: 2,
        },
        failures: Vec::new(),
        coverage: IngestPassCoverage::Partial { deferred_units: 3 },
        scheduling_state_written: true,
        units_admitted: 2,
        units_completed: 2,
        units_failed: 0,
        byte_bounds_enforced: true,
    }
    .into_transcript_outcome();

    assert_eq!(
        partial.coverage,
        IngestPassCoverage::Partial { deferred_units: 3 }
    );
    assert!(!partial.is_success());

    let backpressured = IngestPassOutcome {
        stats: TranscriptIngestStats::default(),
        failures: Vec::new(),
        coverage: IngestPassCoverage::Backpressured {
            admitted_units: 1,
            rejected_units: 4,
        },
        scheduling_state_written: false,
        units_admitted: 1,
        units_completed: 0,
        units_failed: 1,
        byte_bounds_enforced: false,
    }
    .into_transcript_outcome();

    assert_eq!(
        backpressured.coverage,
        IngestPassCoverage::Backpressured {
            admitted_units: 1,
            rejected_units: 4,
        }
    );
    assert!(!backpressured.is_success());
}

#[test]
fn transcript_contract_failures_are_not_retryable() {
    let error = source::TranscriptIngestError::CursorKeyMismatch {
        expected: "private expected key".to_string(),
        actual: "private actual key".to_string(),
    };
    let failure = classify_transcript_ingest_failure("cursor", "hook", &error);

    assert_eq!(failure.reason_code, "transcript_cursor_key_mismatch");
    assert!(!failure.retryable);
}

#[test]
fn cursor_advance_receipt_collisions_are_permanent() {
    let error = claude_observation::ClaudeObservationIngestError::Store(
        tracedecay_store::ObservationStoreError::CursorAdvanceCollision,
    );

    let failure = classify_claude_observation_failure(&error);

    assert_eq!(failure.reason_code, "observation_cursor_advance_collision");
    assert!(!failure.retryable);
}

#[test]
fn transcript_privacy_and_non_durable_failures_are_bounded_and_permanent() {
    let privacy = source::TranscriptIngestError::Privacy(
        crate::privacy::PrivacySanitizerError::InvalidPolicy,
    );
    let privacy = classify_transcript_ingest_failure("claude", "hook", &privacy);
    assert_eq!(privacy.reason_code, "transcript_privacy_rejected");
    assert!(!privacy.retryable);

    let non_durable = source::TranscriptIngestError::NonDurableRecord {
        provider: "claude",
        offset: 7,
        end_offset: 99,
        reason: "private detail",
    };
    let non_durable = classify_transcript_ingest_failure("claude", "hook", &non_durable);
    assert_eq!(non_durable.reason_code, "transcript_record_non_durable");
    assert!(!non_durable.retryable);
    let serialized = serde_json::to_string(&non_durable).unwrap();
    assert!(!serialized.contains("private detail"));
    assert!(serialized.contains(r#""source_locator":{"start":7,"end":99}"#));

    let controlled = source::TranscriptIngestError::NonDurableRecord {
        provider: "claude",
        offset: 11,
        end_offset: 23,
        reason: "malformed snapshot JSON",
    };
    let controlled = classify_transcript_ingest_failure("claude", "snapshot", &controlled);
    assert_eq!(controlled.reason_code, "malformed_snapshot_json");
    assert_eq!(controlled.source_locator.unwrap().start(), 11);
    assert_eq!(controlled.source_locator.unwrap().end(), 23);

    let bounded_code = source::TranscriptIngestError::NonDurableRecord {
        provider: "claude",
        offset: 0,
        end_offset: 0,
        reason: "authority_unavailable",
    };
    let bounded_code = classify_transcript_ingest_failure("claude", "hook", &bounded_code);
    assert_eq!(bounded_code.reason_code, "authority_unavailable");
    assert_eq!(bounded_code.source_locator, None);
}

#[test]
fn transcript_source_contract_failures_are_bounded_and_permanent() {
    let errors = [
        source::TranscriptIngestError::Domain(tracedecay_domain::SessionId::new("").unwrap_err()),
        source::TranscriptIngestError::ObservationContract(
            tracedecay_domain::ClaudeFileGenerationV1::new(0).unwrap_err(),
        ),
        source::TranscriptIngestError::InvalidFrameState {
            provider: "private provider detail",
        },
        source::TranscriptIngestError::InvalidSourceIdentity {
            provider: "private provider detail",
            path: PathBuf::from("/private/source/path"),
        },
    ];

    for error in errors {
        let failure = classify_transcript_ingest_failure("claude", "hook", &error);
        assert_eq!(failure.reason_code, "transcript_source_contract_invalid");
        assert!(!failure.retryable);
        assert!(
            !serde_json::to_string(&failure)
                .unwrap()
                .contains("private provider detail")
        );
    }
}

#[test]
fn parse_git_log_commits_reads_sha_and_time_skipping_malformed() {
    let stdout = concat!(
        "ABCDEF1234567890 1700000000\n",
        "\n",
        "missing-time\n",
        "cafebabe not-a-number\n",
        "deadbeefdeadbeef 1700000200\n",
    );
    let commits = parse_git_log_commits(stdout);
    assert_eq!(
        commits,
        vec![
            git_correlation::ScannedCommit {
                sha: "abcdef1234567890".to_string(),
                committed_at: 1_700_000_000,
            },
            git_correlation::ScannedCommit {
                sha: "deadbeefdeadbeef".to_string(),
                committed_at: 1_700_000_200,
            },
        ]
    );
}

#[test]
fn parse_git_log_commits_empty_is_empty() {
    assert!(parse_git_log_commits("").is_empty());
}

/// End-to-end over the real scanner: a commit made *while a session is still
/// recording* must be attributed on the next sweep. This exercises the actual
/// `git log` invocation and window arithmetic, not a stubbed scan, because the
/// window bounds are where live commits were previously being missed.
#[tokio::test]
async fn live_session_commit_is_attributed_by_the_real_git_scan() {
    use std::process::Command;

    let repo = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(repo.path())
            .output()
            .unwrap()
    };
    assert!(git(&["init", "-q", "-b", "main"]).status.success());
    assert!(
        git(&["config", "user.email", "test@example.com"])
            .status
            .success()
    );
    assert!(git(&["config", "user.name", "Test"]).status.success());
    std::fs::write(repo.path().join("file.txt"), "one\n").unwrap();
    assert!(git(&["add", "file.txt"]).status.success());
    assert!(git(&["commit", "-q", "-m", "live commit"]).status.success());
    let sha = String::from_utf8(git(&["rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_string();

    let store = tempfile::tempdir().unwrap();
    let handle = crate::db::engine::TestConnection::open(&store.path().join("sessions.db"));
    let conn: &crate::db::engine::Connection = &handle;
    git_correlation::ensure_git_correlation_schema_in_transaction(conn)
        .await
        .unwrap();

    // A session recording "now" — the commit lands inside its span, exactly as
    // it does when an agent commits mid-session.
    let now = crate::tracedecay::current_timestamp();
    let worktree = git_correlation::normalize_worktree(&repo.path().to_string_lossy());
    for ts in [now - 60, now] {
        git_correlation::record_span_observation_in_transaction(
            conn,
            &git_correlation::SpanObservation {
                provider: "claude".to_string(),
                session_id: "live".to_string(),
                thread_id: None,
                branch: Some("main".to_string()),
                worktree: worktree.clone(),
                ts,
                source: git_correlation::SpanSource::HookRoute,
            },
            git_correlation::DEFAULT_SPAN_MERGE_GAP_SECS,
        )
        .await
        .unwrap();
    }

    let gap = git_correlation::DEFAULT_SPAN_MERGE_GAP_SECS;
    let inserted = git_correlation::run_commit_attribution_sweep(conn, gap, |target| {
        super::project::git_scan_commits(target, gap)
    })
    .await
    .unwrap();
    assert!(
        inserted >= 1,
        "a commit made during a live session must be attributed"
    );

    let hits = git_correlation::sessions_for_with_relation(
        conn,
        &git_correlation::SessionsForQuery {
            git_ref: git_correlation::GitRefFilter::parse("commit", &sha).unwrap(),
            since: None,
            until: None,
            limit: 10,
        },
        git_correlation::CommitRelationFilter::All,
    )
    .await
    .unwrap();
    assert_eq!(
        hits.len(),
        1,
        "the live session must be correlated: {hits:?}"
    );
    assert_eq!(hits[0].session_id, "live");
    assert_eq!(
        hits[0].span_overlap_kind,
        Some(git_correlation::SpanOverlapKind::WithinSpan)
    );

    // Replaying the sweep must not double-attribute.
    let again = git_correlation::run_commit_attribution_sweep(conn, gap, |target| {
        super::project::git_scan_commits(target, gap)
    })
    .await
    .unwrap();
    assert_eq!(again, 0, "re-sweeping an attributed commit is a no-op");
}

#[test]
fn startup_user_ingest_claims_are_single_flight_and_cancellation_safe() {
    let profile = tempfile::tempdir().unwrap().path().to_path_buf();
    let first = StartupUserIngestGuard::claim(profile.clone()).expect("first claim");
    assert!(StartupUserIngestGuard::claim(profile.clone()).is_none());

    drop(first);
    let mut retry = StartupUserIngestGuard::claim(profile.clone())
        .expect("an incomplete claim must release immediately");
    retry.completed = true;
    drop(retry);

    assert!(
        StartupUserIngestGuard::claim(profile).is_none(),
        "a completed sweep should suppress the startup herd during cooldown"
    );
}

#[test]
fn fair_round_robin_does_not_starve_later_sources() {
    let first = plan_round_robin_admission(3, 0, 1);
    assert_eq!(first.admitted_indices, vec![0]);
    assert_eq!(
        first.coverage,
        IngestPassCoverage::Partial { deferred_units: 2 }
    );

    let second = plan_round_robin_admission(3, 1, 1);
    assert_eq!(second.admitted_indices, vec![1]);

    let third = plan_round_robin_admission(3, 2, 1);
    assert_eq!(third.admitted_indices, vec![2]);

    let wrap = plan_round_robin_admission(3, 3, 1);
    assert_eq!(wrap.admitted_indices, vec![0]);
}

#[test]
fn zero_unit_budget_is_typed_backpressure() {
    let plan = plan_round_robin_admission(3, 0, 0);
    assert!(plan.admitted_indices.is_empty());
    assert_eq!(
        plan.coverage,
        IngestPassCoverage::Backpressured {
            admitted_units: 0,
            rejected_units: 3,
        }
    );
}

#[test]
fn fully_covered_pass_never_requires_scheduling_write() {
    let plan = plan_round_robin_admission(4, 7, 8);
    assert_eq!(plan.admitted_indices, vec![3, 0, 1, 2]);
    assert_eq!(plan.coverage, IngestPassCoverage::Complete);
    assert!(!scheduling_write_required(
        IngestPassCoverage::Complete,
        plan.admitted_indices.len(),
        false
    ));
}

#[test]
fn cancellation_and_empty_attempt_suppress_scheduling_writes() {
    assert!(!scheduling_write_required(
        IngestPassCoverage::Partial { deferred_units: 3 },
        2,
        true
    ));
    assert!(!scheduling_write_required(
        IngestPassCoverage::Partial { deferred_units: 3 },
        0,
        false
    ));
    assert!(scheduling_write_required(
        IngestPassCoverage::Partial { deferred_units: 3 },
        2,
        false
    ));
    assert!(scheduling_write_required(
        IngestPassCoverage::Backpressured {
            admitted_units: 2,
            rejected_units: 1
        },
        2,
        false
    ));
}

#[test]
fn fair_admission_interleaves_sources_without_starvation() {
    let sources = vec![
        boxed_source(FakeSource {
            provider: "a",
            paths: vec![PathBuf::from("a1"), PathBuf::from("a2")],
            fail: false,
            budget_log: None,
        }),
        boxed_source(FakeSource {
            provider: "b",
            paths: vec![PathBuf::from("b1"), PathBuf::from("b2")],
            fail: false,
            budget_log: None,
        }),
    ];
    let bounds = IngestPassBounds {
        units_per_pass: 4,
        units_per_source: 2,
        queue_depth: 4,
        ..TEST_INGEST_BOUNDS
    };
    let (units, deferred) = discover_ingest_units(&sources, Path::new("project"), bounds, 0);
    assert_eq!(deferred, 0);
    assert_eq!(
        units
            .iter()
            .map(|unit| unit.path.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        vec!["a1", "b1", "a2", "b2"]
    );
    let (admitted, coverage) = admit_fair_ingest_units(&units, 0, bounds);
    assert_eq!(coverage, IngestPassCoverage::Complete);
    let sources: Vec<&str> = admitted
        .iter()
        .map(|&index| units[index].source_id.as_str())
        .collect();
    assert_eq!(sources, vec!["a", "b", "a", "b"]);
}

#[test]
fn uneven_queues_make_progress_across_durable_passes() {
    let units = vec![
        unit("a", "a1", 0),
        unit("b", "b1", 1),
        unit("a", "a2", 0),
        unit("a", "a3", 0),
    ];
    let bounds = IngestPassBounds {
        units_per_pass: 2,
        units_per_source: 4,
        queue_depth: 2,
        ..TEST_INGEST_BOUNDS
    };
    let mut frontier = 0_u64;
    let mut attempted = Vec::new();
    for _ in 0..2 {
        let (admitted, coverage) = admit_fair_ingest_units(&units, frontier, bounds);
        assert_eq!(coverage, IngestPassCoverage::Partial { deferred_units: 2 });
        attempted.extend(
            admitted
                .iter()
                .map(|&index| units[index].path.to_string_lossy().into_owned()),
        );
        frontier = frontier.saturating_add(u64::try_from(admitted.len()).unwrap());
    }
    assert_eq!(attempted, vec!["a1", "b1", "a2", "a3"]);
}

#[test]
fn per_source_bound_advances_contiguously_without_starvation() {
    let units = vec![
        unit("a", "a1", 0),
        unit("b", "b1", 1),
        unit("a", "a2", 0),
        unit("a", "a3", 0),
    ];
    let bounds = IngestPassBounds {
        units_per_pass: 4,
        units_per_source: 1,
        queue_depth: 4,
        ..TEST_INGEST_BOUNDS
    };
    let mut frontier = 0_u64;
    let mut attempted = Vec::new();
    for _ in 0..3 {
        let (admitted, coverage) = admit_fair_ingest_units(&units, frontier, bounds);
        assert!(matches!(coverage, IngestPassCoverage::Backpressured { .. }));
        attempted.extend(
            admitted
                .iter()
                .map(|&index| units[index].path.to_string_lossy().into_owned()),
        );
        frontier = frontier.saturating_add(u64::try_from(admitted.len()).unwrap());
    }
    assert_eq!(attempted, vec!["a1", "b1", "a2", "a3"]);
}

#[test]
fn pass_byte_allocations_never_exceed_aggregate_cap() {
    let bounds = IngestPassBounds {
        bytes_per_unit: 4,
        bytes_per_pass: 10,
        ..TEST_INGEST_BOUNDS
    };
    let budgets = allocate_pass_byte_budgets(4, bounds);
    assert_eq!(budgets, vec![4, 4, 2]);
    assert_eq!(budgets.iter().copied().sum::<u64>(), 10);

    let zero = IngestPassBounds {
        bytes_per_unit: 4,
        bytes_per_pass: 0,
        ..TEST_INGEST_BOUNDS
    };
    assert!(allocate_pass_byte_budgets(4, zero).is_empty());
}

#[tokio::test]
async fn file_provider_reserves_one_aggregate_byte_budget_across_paths() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let paths = (0..3)
        .map(|index| {
            let path = temp.path().join(format!("{index}.jsonl"));
            std::fs::write(&path, b"{}\n").unwrap();
            path
        })
        .collect();
    let budget_log = Arc::new(Mutex::new(Vec::new()));
    let source = FakeSource {
        provider: "bounded",
        paths,
        fail: false,
        budget_log: Some(Arc::clone(&budget_log)),
    };
    let runtime = profile_test_runtime(&temp).await;
    let db = runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap();
    let store = crate::store::GlobalDbTranscriptStore::new(db);

    try_ingest_file_source_bounded(&store, &source, &project, 10)
        .await
        .unwrap();

    assert_eq!(*budget_log.lock().unwrap(), vec![3, 3, 4]);
}

#[test]
fn user_provider_admission_applies_discovery_bound_to_actual_work() {
    let bounds = IngestPassBounds {
        discovered_units: 2,
        bytes_per_unit: 4,
        bytes_per_pass: 8,
        ..TEST_INGEST_BOUNDS
    };
    let first = plan_user_provider_admission(5, 0, bounds);
    assert_eq!(first.admitted_indices, vec![0, 1]);
    assert_eq!(
        first.coverage,
        IngestPassCoverage::Partial { deferred_units: 3 }
    );
    let second = plan_user_provider_admission(5, 2, bounds);
    assert_eq!(second.admitted_indices, vec![2, 3]);
}

#[test]
fn provider_internal_deferral_is_typed_backpressure() {
    let coverage = finish_user_provider_coverage(IngestPassCoverage::Complete, 1, 1, 1);
    assert_eq!(
        coverage,
        IngestPassCoverage::Backpressured {
            admitted_units: 1,
            rejected_units: 1,
        }
    );
}

#[test]
fn project_provider_deferral_preserves_existing_deferred_work() {
    let coverage = merge_project_provider_backpressure(
        IngestPassCoverage::Partial { deferred_units: 2 },
        2,
        3,
        1,
    );
    assert_eq!(
        coverage,
        IngestPassCoverage::Backpressured {
            admitted_units: 5,
            rejected_units: 3,
        }
    );
}

#[test]
fn bounded_pass_work_backpressures_instead_of_growing() {
    let units = vec![
        unit("a", "a1", 0),
        unit("b", "b1", 1),
        unit("a", "a2", 0),
        unit("a", "a3", 0),
    ];
    // Per-source cap 2 and queue depth 3 force an explicit overload disposition.
    let bounds = IngestPassBounds {
        units_per_source: 2,
        queue_depth: 3,
        ..TEST_INGEST_BOUNDS
    };
    let (admitted, coverage) = admit_fair_ingest_units(&units, 0, bounds);
    assert_eq!(
        coverage,
        IngestPassCoverage::Backpressured {
            admitted_units: 3,
            rejected_units: 1,
        }
    );
    assert_eq!(admitted.len(), 3);
}

#[derive(Clone)]
struct FakeSource {
    provider: &'static str,
    paths: Vec<PathBuf>,
    fail: bool,
    budget_log: Option<Arc<Mutex<Vec<u64>>>>,
}

impl TranscriptSource for FakeSource {
    fn provider(&self) -> &'static str {
        self.provider
    }

    fn transcript_paths(&self, _project_root: &Path) -> Vec<PathBuf> {
        self.paths.clone()
    }

    fn try_parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> TranscriptIngestResult<Option<ParsedTranscript>> {
        if let (Some(log), Some(max_new_bytes)) = (&self.budget_log, max_new_bytes) {
            log.lock().unwrap().push(max_new_bytes);
        }
        if self.fail {
            if self.provider == "retryable" {
                return Err(source::TranscriptIngestError::ScanIo {
                    operation: "test",
                    path: path.to_path_buf(),
                    source: std::io::Error::other("retryable test failure"),
                });
            }
            return Err(source::TranscriptIngestError::InvalidFrameState {
                provider: self.provider,
            });
        }
        let _ = (path, project_root);
        Ok(Some(ParsedTranscript {
            draft: SessionDraft {
                session_id: format!("{}-{}", self.provider, path.display()),
                project_key: "project".into(),
                project_path: project_root.display().to_string(),
                title: Some("fake".into()),
                metadata_json: None,
                parent_session_id: None,
                is_subagent: false,
                agent_id: None,
                parent_tool_use_id: None,
            },
            messages: Vec::new(),
            new_cursor: StoredCursor {
                position: prev.position.saturating_add(1),
                mtime: 1,
                file_id: 1,
            },
        }))
    }

    fn parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> Option<ParsedTranscript> {
        self.try_parse_new(path, prev, project_root, max_new_bytes)
            .ok()
            .flatten()
    }
}

fn unit(source_id: &str, path: &str, source_index: usize) -> DiscoveredIngestUnit {
    DiscoveredIngestUnit {
        source_id: source_id.to_string(),
        path: PathBuf::from(path),
        source_index,
    }
}

fn boxed_source(source: FakeSource) -> Box<dyn TranscriptSource> {
    Box::new(source)
}

struct PageOrderedSource;

impl TranscriptSource for PageOrderedSource {
    fn provider(&self) -> &'static str {
        "page-ordered"
    }

    fn transcript_paths(&self, _project_root: &Path) -> Vec<PathBuf> {
        Vec::new()
    }

    fn discover_transcript_paths_page(
        &self,
        _project_root: &Path,
        _bounds: TranscriptDiscoveryBounds,
        _start_offset: usize,
    ) -> (FileDiscoveryReport, usize) {
        (
            FileDiscoveryReport {
                paths: vec![
                    PathBuf::from("z-newest"),
                    PathBuf::from("a-older"),
                    PathBuf::from("z-newest"),
                ],
                truncated: None,
                skipped_oversized_entries: 0,
                bytes_charged: 0,
            },
            0,
        )
    }

    fn parse_new(
        &self,
        _path: &Path,
        _prev: StoredCursor,
        _project_root: &Path,
        _max_new_bytes: Option<u64>,
    ) -> Option<ParsedTranscript> {
        None
    }
}

struct NoOpSource {
    provider: &'static str,
    path: PathBuf,
    attempts: Arc<Mutex<usize>>,
}

fn no_op_source_pair(first_attempts: Arc<Mutex<usize>>) -> Vec<Box<dyn TranscriptSource>> {
    vec![
        Box::new(NoOpSource {
            provider: "first",
            path: PathBuf::from("first"),
            attempts: first_attempts,
        }),
        Box::new(NoOpSource {
            provider: "second",
            path: PathBuf::from("second"),
            attempts: Arc::new(Mutex::new(0)),
        }),
    ]
}

impl TranscriptSource for NoOpSource {
    fn provider(&self) -> &'static str {
        self.provider
    }

    fn transcript_paths(&self, _project_root: &Path) -> Vec<PathBuf> {
        vec![self.path.clone()]
    }

    fn try_parse_new(
        &self,
        _path: &Path,
        _prev: StoredCursor,
        _project_root: &Path,
        _max_new_bytes: Option<u64>,
    ) -> TranscriptIngestResult<Option<ParsedTranscript>> {
        let mut attempts = self.attempts.lock().unwrap();
        *attempts = attempts.saturating_add(1);
        Ok(None)
    }

    fn parse_new(
        &self,
        _path: &Path,
        _prev: StoredCursor,
        _project_root: &Path,
        _max_new_bytes: Option<u64>,
    ) -> Option<ParsedTranscript> {
        None
    }
}

struct CancellingSource {
    inner: FakeSource,
    cancellation: ObservationCancellation,
}

impl TranscriptSource for CancellingSource {
    fn provider(&self) -> &'static str {
        self.inner.provider()
    }

    fn transcript_paths(&self, project_root: &Path) -> Vec<PathBuf> {
        self.inner.transcript_paths(project_root)
    }

    fn try_parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> TranscriptIngestResult<Option<ParsedTranscript>> {
        self.cancellation.cancel();
        self.inner
            .try_parse_new(path, prev, project_root, max_new_bytes)
    }

    fn parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> Option<ParsedTranscript> {
        self.cancellation.cancel();
        self.inner
            .parse_new(path, prev, project_root, max_new_bytes)
    }
}

#[tokio::test]
async fn source_failure_is_isolated_and_does_not_block_siblings() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let project_id = scheduler_test_project_id();
    let ok_path = temp.path().join("ok.jsonl");
    let bad_path = temp.path().join("bad.jsonl");
    std::fs::write(&ok_path, b"{}\n").unwrap();
    std::fs::write(&bad_path, b"{}\n").unwrap();
    let runtime = project_test_runtime(&temp, &project, project_id.clone()).await;
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let sources = vec![
        boxed_source(FakeSource {
            provider: "alpha",
            paths: vec![ok_path.clone()],
            fail: false,
            budget_log: None,
        }),
        boxed_source(FakeSource {
            provider: "beta",
            paths: vec![bad_path.clone()],
            fail: true,
            budget_log: None,
        }),
        boxed_source(FakeSource {
            provider: "gamma",
            paths: vec![temp.path().join("gamma.jsonl")],
            fail: false,
            budget_log: None,
        }),
    ];
    std::fs::write(temp.path().join("gamma.jsonl"), b"{}\n").unwrap();
    let bounds = IngestPassBounds {
        units_per_source: 4,
        ..TEST_INGEST_BOUNDS
    };
    let outcome = ingest_sources_bounded(
        db,
        &project,
        &project_id,
        &sources,
        bounds,
        &ObservationCancellation::default(),
    )
    .await;
    assert_eq!(outcome.units_admitted, 3);
    assert_eq!(outcome.units_failed, 1);
    assert!(outcome.units_completed >= 1);
    assert!(!outcome.coverage.is_complete() || !outcome.failures.is_empty());
    assert!(
        outcome
            .failures
            .iter()
            .any(|failure| failure.provider == "beta")
    );
    assert!(
        !outcome
            .failures
            .iter()
            .any(|failure| failure.provider == "alpha" || failure.provider == "gamma")
    );
    assert_eq!(outcome.coverage, IngestPassCoverage::Complete);
    assert!(
        !outcome.scheduling_state_written,
        "fully covered passes must not persist a scheduling frontier"
    );
    assert!(
        db.get_parse_offset_result(TRANSCRIPT_INGEST_SOURCE_FRONTIER_KEY)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn retryable_source_respects_retry_and_pass_byte_bounds() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let project_id = scheduler_test_project_id();
    let path = temp.path().join("retryable.jsonl");
    std::fs::write(&path, b"{}\n").unwrap();
    let runtime = project_test_runtime(&temp, &project, project_id.clone()).await;
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let budget_log = Arc::new(Mutex::new(Vec::new()));
    let sources = vec![boxed_source(FakeSource {
        provider: "retryable",
        paths: vec![path],
        fail: true,
        budget_log: Some(Arc::clone(&budget_log)),
    })];
    let bounds = IngestPassBounds {
        bytes_per_unit: 4,
        bytes_per_pass: 12,
        retries: 2,
        ..TEST_INGEST_BOUNDS
    };

    let outcome = ingest_sources_bounded(
        db,
        &project,
        &project_id,
        &sources,
        bounds,
        &ObservationCancellation::default(),
    )
    .await;

    assert_eq!(*budget_log.lock().unwrap(), vec![4, 4, 4]);
    assert_eq!(outcome.units_failed, 1);
    assert_eq!(outcome.failures.len(), 1);
}

#[tokio::test]
async fn aggregate_byte_budget_is_granted_once_across_the_pass() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let project_id = scheduler_test_project_id();
    let runtime = project_test_runtime(&temp, &project, project_id.clone()).await;
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let budget_log = Arc::new(Mutex::new(Vec::new()));
    let mut sources = Vec::new();
    for (provider, name) in [("a", "a.jsonl"), ("b", "b.jsonl"), ("c", "c.jsonl")] {
        let path = temp.path().join(name);
        std::fs::write(&path, b"{}\n").unwrap();
        sources.push(boxed_source(FakeSource {
            provider,
            paths: vec![path],
            fail: false,
            budget_log: Some(Arc::clone(&budget_log)),
        }));
    }
    let bounds = IngestPassBounds {
        bytes_per_unit: 4,
        bytes_per_pass: 10,
        ..TEST_INGEST_BOUNDS
    };
    let outcome = ingest_sources_bounded(
        db,
        &project,
        &project_id,
        &sources,
        bounds,
        &ObservationCancellation::default(),
    )
    .await;

    let grants = budget_log.lock().unwrap().clone();
    assert_eq!(grants, vec![4, 4, 2]);
    assert_eq!(grants.iter().copied().sum::<u64>(), 10);
    assert_eq!(outcome.units_admitted, 3);
    assert_eq!(outcome.coverage, IngestPassCoverage::Complete);
    assert!(outcome.byte_bounds_enforced);
}

#[tokio::test]
async fn zero_byte_pass_defers_work_without_frontier_advance() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let project_id = scheduler_test_project_id();
    let path = temp.path().join("a.jsonl");
    std::fs::write(&path, b"{}\n").unwrap();
    let runtime = project_test_runtime(&temp, &project, project_id.clone()).await;
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let budget_log = Arc::new(Mutex::new(Vec::new()));
    let sources = vec![boxed_source(FakeSource {
        provider: "a",
        paths: vec![path],
        fail: false,
        budget_log: Some(Arc::clone(&budget_log)),
    })];
    let bounds = IngestPassBounds {
        bytes_per_unit: 4,
        bytes_per_pass: 0,
        ..TEST_INGEST_BOUNDS
    };
    let outcome = ingest_sources_bounded(
        db,
        &project,
        &project_id,
        &sources,
        bounds,
        &ObservationCancellation::default(),
    )
    .await;

    assert!(budget_log.lock().unwrap().is_empty());
    assert_eq!(outcome.units_admitted, 0);
    assert!(matches!(
        outcome.coverage,
        IngestPassCoverage::Backpressured {
            admitted_units: 0,
            rejected_units: 1
        }
    ));
    assert!(!outcome.scheduling_state_written);
    assert_eq!(
        read_ingest_frontier(db, TRANSCRIPT_INGEST_SOURCE_FRONTIER_KEY).await,
        Some(0)
    );
}

#[tokio::test]
async fn cancellation_during_unit_keeps_committed_work_without_frontier_write() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let project_id = scheduler_test_project_id();
    let first_path = temp.path().join("a.jsonl");
    let second_path = temp.path().join("b.jsonl");
    std::fs::write(&first_path, b"{}\n").unwrap();
    std::fs::write(&second_path, b"{}\n").unwrap();
    let runtime = project_test_runtime(&temp, &project, project_id.clone()).await;
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let cancellation = ObservationCancellation::default();
    let sources: Vec<Box<dyn TranscriptSource>> = vec![
        Box::new(CancellingSource {
            inner: FakeSource {
                provider: "a",
                paths: vec![first_path],
                fail: false,
                budget_log: None,
            },
            cancellation: cancellation.clone(),
        }),
        boxed_source(FakeSource {
            provider: "b",
            paths: vec![second_path],
            fail: false,
            budget_log: None,
        }),
    ];
    let bounds = IngestPassBounds {
        units_per_pass: 1,
        queue_depth: 1,
        ..TEST_INGEST_BOUNDS
    };
    let outcome =
        ingest_sources_bounded(db, &project, &project_id, &sources, bounds, &cancellation).await;

    assert_eq!(outcome.units_completed, 1);
    assert!(cancellation.is_cancelled());
    assert_eq!(
        outcome.coverage,
        IngestPassCoverage::Partial { deferred_units: 1 }
    );
    assert!(
        outcome
            .failures
            .iter()
            .any(|failure| failure.reason_code == "ingest_pass_cancelled")
    );
    assert!(!outcome.scheduling_state_written);
    assert_eq!(
        read_ingest_frontier(db, TRANSCRIPT_INGEST_SOURCE_FRONTIER_KEY).await,
        Some(0)
    );
}

#[tokio::test]
async fn partial_pass_writes_frontier_cancellation_does_not() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let project_id = scheduler_test_project_id();
    let runtime = project_test_runtime(&temp, &project, project_id.clone()).await;
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let mut sources = Vec::new();
    for (provider, name) in [("a", "a.jsonl"), ("b", "b.jsonl"), ("c", "c.jsonl")] {
        let path = temp.path().join(name);
        std::fs::write(&path, b"{}\n").unwrap();
        sources.push(boxed_source(FakeSource {
            provider,
            paths: vec![path],
            fail: false,
            budget_log: None,
        }));
    }
    let bounds = IngestPassBounds {
        units_per_pass: 1,
        units_per_source: 4,
        ..TEST_INGEST_BOUNDS
    };
    let first = ingest_sources_bounded(
        db,
        &project,
        &project_id,
        &sources,
        bounds,
        &ObservationCancellation::default(),
    )
    .await;
    assert!(matches!(
        first.coverage,
        IngestPassCoverage::Partial { deferred_units: 2 }
    ));
    assert!(first.scheduling_state_written);
    assert_eq!(
        read_ingest_frontier(db, TRANSCRIPT_INGEST_SOURCE_FRONTIER_KEY).await,
        Some(1)
    );

    let cancellation = ObservationCancellation::default();
    cancellation.cancel();
    let cancelled =
        ingest_sources_bounded(db, &project, &project_id, &sources, bounds, &cancellation).await;
    assert!(
        cancelled
            .failures
            .iter()
            .any(|failure| failure.reason_code == "ingest_pass_cancelled")
    );
    assert!(
        !cancelled.scheduling_state_written,
        "cancellation must not advance the scheduling frontier"
    );
    assert_eq!(
        read_ingest_frontier(db, TRANSCRIPT_INGEST_SOURCE_FRONTIER_KEY).await,
        Some(1)
    );

    let second = ingest_sources_bounded(
        db,
        &project,
        &project_id,
        &sources,
        bounds,
        &ObservationCancellation::default(),
    )
    .await;
    assert_eq!(second.units_admitted, 1);
    // Frontier advanced past source 0, so the next single-unit pass starts at 1.
    assert!(second.scheduling_state_written);
    assert_eq!(
        read_ingest_frontier(db, TRANSCRIPT_INGEST_SOURCE_FRONTIER_KEY).await,
        Some(2)
    );
}

#[tokio::test]
async fn production_frontier_rotates_before_a_continuously_busy_source() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let project_id = scheduler_test_project_id();
    let runtime = project_test_runtime(&temp, &project, project_id.clone()).await;
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let first_log = Arc::new(Mutex::new(Vec::new()));
    let second_log = Arc::new(Mutex::new(Vec::new()));
    let sources = vec![
        boxed_source(FakeSource {
            provider: "busy",
            paths: vec!["a1", "a2", "a3"]
                .into_iter()
                .map(PathBuf::from)
                .collect(),
            fail: false,
            budget_log: Some(Arc::clone(&first_log)),
        }),
        boxed_source(FakeSource {
            provider: "peer",
            paths: vec![PathBuf::from("b1")],
            fail: false,
            budget_log: Some(Arc::clone(&second_log)),
        }),
    ];
    let bounds = IngestPassBounds {
        discovered_units: 2,
        units_per_pass: 1,
        units_per_source: 1,
        queue_depth: 1,
        ..TEST_INGEST_BOUNDS
    };

    let first = ingest_sources_bounded(
        db,
        &project,
        &project_id,
        &sources,
        bounds,
        &ObservationCancellation::default(),
    )
    .await;
    let second = ingest_sources_bounded(
        db,
        &project,
        &project_id,
        &sources,
        bounds,
        &ObservationCancellation::default(),
    )
    .await;

    assert!(first.scheduling_state_written);
    assert!(second.scheduling_state_written);
    assert_eq!(first_log.lock().unwrap().len(), 1);
    assert_eq!(second_log.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn terminal_source_failure_rotates_to_a_healthy_peer() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let project_id = scheduler_test_project_id();
    let runtime = project_test_runtime(&temp, &project, project_id.clone()).await;
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let healthy_log = Arc::new(Mutex::new(Vec::new()));
    let sources = vec![
        boxed_source(FakeSource {
            provider: "failed",
            paths: vec![PathBuf::from("failed")],
            fail: true,
            budget_log: None,
        }),
        boxed_source(FakeSource {
            provider: "healthy",
            paths: vec![PathBuf::from("healthy")],
            fail: false,
            budget_log: Some(Arc::clone(&healthy_log)),
        }),
    ];
    let bounds = IngestPassBounds {
        discovered_units: 2,
        units_per_pass: 1,
        units_per_source: 1,
        queue_depth: 1,
        ..TEST_INGEST_BOUNDS
    };

    let failed = ingest_sources_bounded(
        db,
        &project,
        &project_id,
        &sources,
        bounds,
        &ObservationCancellation::default(),
    )
    .await;
    let healthy = ingest_sources_bounded(
        db,
        &project,
        &project_id,
        &sources,
        bounds,
        &ObservationCancellation::default(),
    )
    .await;

    assert_eq!(failed.units_failed, 1);
    assert!(failed.scheduling_state_written);
    assert_eq!(healthy.units_completed, 1);
    assert_eq!(healthy_log.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn attempted_no_op_does_not_write_partial_scheduling_state() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let project_id = scheduler_test_project_id();
    let runtime = project_test_runtime(&temp, &project, project_id.clone()).await;
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let first_attempts = Arc::new(Mutex::new(0usize));
    let second_attempts = Arc::new(Mutex::new(0usize));
    let sources: Vec<Box<dyn TranscriptSource>> = vec![
        Box::new(NoOpSource {
            provider: "first",
            path: PathBuf::from("first"),
            attempts: Arc::clone(&first_attempts),
        }),
        Box::new(NoOpSource {
            provider: "second",
            path: PathBuf::from("second"),
            attempts: Arc::clone(&second_attempts),
        }),
    ];
    let bounds = IngestPassBounds {
        discovered_units: 2,
        units_per_pass: 1,
        units_per_source: 1,
        queue_depth: 1,
        ..TEST_INGEST_BOUNDS
    };

    let first = ingest_sources_bounded(
        db,
        &project,
        &project_id,
        &sources,
        bounds,
        &ObservationCancellation::default(),
    )
    .await;
    let second = ingest_sources_bounded(
        db,
        &project,
        &project_id,
        &sources,
        bounds,
        &ObservationCancellation::default(),
    )
    .await;

    assert_eq!(first.units_admitted, 1);
    assert_eq!(second.units_admitted, 1);
    assert!(!first.coverage.is_complete());
    assert!(!second.coverage.is_complete());
    assert!(!first.scheduling_state_written);
    assert!(!second.scheduling_state_written);
    assert_eq!(*first_attempts.lock().unwrap(), 1);
    assert_eq!(*second_attempts.lock().unwrap(), 1);
    assert_eq!(
        read_ingest_frontier(db, TRANSCRIPT_INGEST_SOURCE_FRONTIER_KEY).await,
        Some(0)
    );
}

#[tokio::test]
async fn transient_frontier_isolated_by_profile_authority() {
    let first_temp = tempfile::tempdir().unwrap();
    let second_temp = tempfile::tempdir().unwrap();
    let first_project = first_temp.path().join("project");
    let second_project = second_temp.path().join("project");
    std::fs::create_dir_all(&first_project).unwrap();
    std::fs::create_dir_all(&second_project).unwrap();
    let project_id = scheduler_test_project_id();
    let first_runtime = project_test_runtime(&first_temp, &first_project, project_id.clone()).await;
    let second_runtime =
        project_test_runtime(&second_temp, &second_project, project_id.clone()).await;
    let first_db = first_runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let second_db = second_runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let first_attempts = Arc::new(Mutex::new(0usize));
    let second_attempts = Arc::new(Mutex::new(0usize));
    let first_sources = no_op_source_pair(Arc::clone(&first_attempts));
    let second_sources = no_op_source_pair(Arc::clone(&second_attempts));
    let bounds = IngestPassBounds {
        discovered_units: 2,
        units_per_pass: 1,
        units_per_source: 1,
        queue_depth: 1,
        ..TEST_INGEST_BOUNDS
    };

    ingest_sources_bounded(
        first_db,
        &first_project,
        &project_id,
        &first_sources,
        bounds,
        &ObservationCancellation::default(),
    )
    .await;
    ingest_sources_bounded(
        second_db,
        &second_project,
        &project_id,
        &second_sources,
        bounds,
        &ObservationCancellation::default(),
    )
    .await;

    assert_eq!(*first_attempts.lock().unwrap(), 1);
    assert_eq!(*second_attempts.lock().unwrap(), 1);
}

#[test]
fn discovery_respects_per_source_and_total_bounds() {
    let sources = vec![
        boxed_source(FakeSource {
            provider: "a",
            paths: vec![
                PathBuf::from("a1"),
                PathBuf::from("a2"),
                PathBuf::from("a3"),
            ],
            fail: false,
            budget_log: None,
        }),
        boxed_source(FakeSource {
            provider: "b",
            paths: vec![PathBuf::from("b1"), PathBuf::from("b2")],
            fail: false,
            budget_log: None,
        }),
    ];
    let bounds = IngestPassBounds {
        discovered_units: 3,
        units_per_source: 2,
        ..TEST_INGEST_BOUNDS
    };
    let (units, deferred) = discover_ingest_units(&sources, Path::new("/tmp/project"), bounds, 0);
    assert_eq!(deferred, 2);
    assert_eq!(units.len(), 3);
    assert_eq!(units.iter().filter(|unit| unit.source_id == "a").count(), 2);
    assert_eq!(units.iter().filter(|unit| unit.source_id == "b").count(), 1);
    assert_eq!(
        units
            .iter()
            .map(|unit| unit.path.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        vec!["a1", "b1", "a2"],
        "source zero cannot consume the global discovery cap before source one"
    );
}

#[test]
fn discovery_preserves_provider_page_order_while_deduplicating() {
    let sources: Vec<Box<dyn TranscriptSource>> = vec![Box::new(PageOrderedSource)];
    let bounds = IngestPassBounds {
        discovered_units: 4,
        units_per_pass: 4,
        units_per_source: 4,
        queue_depth: 4,
        ..TEST_INGEST_BOUNDS
    };

    let (units, deferred) = discover_ingest_units(&sources, Path::new("/tmp/project"), bounds, 0);

    assert_eq!(deferred, 0);
    assert_eq!(
        units
            .iter()
            .map(|unit| unit.path.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        vec!["z-newest", "a-older"],
        "generic ingest must not replace provider page order with lexical order"
    );
}

#[test]
fn discovery_emits_typed_deferred_backpressure_for_oversized_paths() {
    let long = "p".repeat(TranscriptDiscoveryBounds::default_walk().max_path_bytes + 8);
    let sources = vec![boxed_source(FakeSource {
        provider: "a",
        paths: vec![PathBuf::from("ok"), PathBuf::from(long)],
        fail: false,
        budget_log: None,
    })];
    let bounds = IngestPassBounds {
        discovered_units: 8,
        ..TEST_INGEST_BOUNDS
    };
    let (units, deferred) = discover_ingest_units(&sources, Path::new("/tmp/project"), bounds, 0);
    assert_eq!(units.len(), 1);
    assert_eq!(units[0].path, PathBuf::from("ok"));
    assert!(
        deferred >= 1,
        "oversized path must defer with typed backpressure"
    );
    let serialized = format!("{units:?}");
    assert!(
        !serialized.contains(&"p".repeat(64)),
        "unit list must not retain oversized path payloads"
    );
}

#[test]
fn over_cap_source_and_single_peer_both_progress_across_discovery_passes() {
    let sources = vec![
        boxed_source(FakeSource {
            provider: "a",
            paths: vec![
                PathBuf::from("a1"),
                PathBuf::from("a2"),
                PathBuf::from("a3"),
            ],
            fail: false,
            budget_log: None,
        }),
        boxed_source(FakeSource {
            provider: "b",
            paths: vec![PathBuf::from("b1")],
            fail: false,
            budget_log: None,
        }),
    ];
    let bounds = IngestPassBounds {
        discovered_units: 2,
        units_per_pass: 2,
        units_per_source: 2,
        queue_depth: 2,
        ..TEST_INGEST_BOUNDS
    };
    let (first, first_deferred) = discover_ingest_units(&sources, Path::new("project"), bounds, 0);
    assert_eq!(first_deferred, 2);
    assert_eq!(
        first
            .iter()
            .map(|unit| unit.path.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        vec!["a1", "b1"]
    );

    let (second, second_deferred) =
        discover_ingest_units(&sources, Path::new("project"), bounds, 2);
    assert_eq!(second_deferred, 2);
    assert_eq!(
        second
            .iter()
            .map(|unit| unit.path.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        vec!["a2", "a3"]
    );
}

#[test]
fn exhausted_discovery_frontier_restarts_the_bounded_cycle() {
    let sources = vec![boxed_source(FakeSource {
        provider: "a",
        paths: vec![
            PathBuf::from("a1"),
            PathBuf::from("a2"),
            PathBuf::from("a3"),
        ],
        fail: false,
        budget_log: None,
    })];
    let bounds = IngestPassBounds {
        discovered_units: 2,
        units_per_pass: 2,
        units_per_source: 2,
        queue_depth: 2,
        ..TEST_INGEST_BOUNDS
    };

    let (units, deferred) = discover_ingest_units(&sources, Path::new("project"), bounds, 3);

    assert_eq!(deferred, 1);
    assert_eq!(
        units
            .iter()
            .map(|unit| unit.path.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        vec!["a1", "a2"]
    );
}
