#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use tracedecay_domain::ProjectId;

use crate::application::host_admission::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
use crate::application::observation::ObservationCancellation;
use crate::sessions::shared::TranscriptIngestStats;
use crate::sessions::{SessionProvider, claude_observation, codex, git_correlation, source};

use super::failure::{
    IngestPassBounds, IngestPassCoverage, IngestPassOutcome, allocate_pass_byte_budgets,
    classify_claude_observation_failure, classify_transcript_ingest_failure,
    plan_round_robin_admission, scheduling_write_required,
};
use super::project::{
    home_dir, ingest_project_sources_for_provider,
    ingest_project_sources_for_provider_without_registered_authority, parse_git_log_commits,
    with_transcript_source_home,
};
use super::scheduler::{
    USER_INGEST_PROVIDER_FRONTIER_KEY, finish_user_provider_coverage,
    merge_project_provider_backpressure, plan_user_provider_admission,
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

const TEST_INGEST_BOUNDS: IngestPassBounds = IngestPassBounds {
    discovered_units: 16,
    units_per_pass: 8,
    units_per_source: 8,
    queue_depth: 8,
    bytes_per_unit: 1024,
    bytes_per_pass: 4096,
    retries: 0,
};
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
}

#[tokio::test]
async fn mismatched_project_id_fails_before_provider_catch_up() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let mounted_project_id = scheduler_test_project_id();
    let requested_project_id = scheduler_test_project_id();
    let runtime = project_test_runtime(&temp, &project, mounted_project_id.clone()).await;
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();

    let outcome = ingest_project_sources_for_provider(
        &db.binding().shard_id.brain_id,
        &db.binding().shard_id.profile_id,
        db,
        &project,
        Some(requested_project_id),
        None,
        true,
    )
    .await;

    assert_eq!(outcome.failures.len(), 1);
    assert_eq!(
        outcome.failures[0].reason_code,
        "project_sessions_authority_mismatch"
    );
    assert_eq!(outcome.stats, TranscriptIngestStats::default());
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
        db.get_parse_offset_result(USER_INGEST_PROVIDER_FRONTIER_KEY)
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
