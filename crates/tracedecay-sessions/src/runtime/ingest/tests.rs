#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use tracedecay_domain::{BrainId, ProjectId, UserProfileId};
use tracedecay_runtime_core::store_runtime::VerifiedGraphRuntimePortV1;
use tracedecay_store::StoreShardIdV1;

use crate::observation::ObservationCancellation;
use crate::runtime::shared::TranscriptIngestStats;
use crate::runtime::{SessionProvider, claude_observation, codex, git_correlation, source};

use super::failure::{
    IngestPassBounds, IngestPassCoverage, IngestPassOutcome, allocate_pass_byte_budgets,
    classify_claude_observation_failure, classify_transcript_ingest_failure,
    plan_round_robin_admission, scheduling_write_required,
};
use super::project::{home_dir, parse_git_log_commits, with_transcript_source_home};
use super::scheduler::{
    finish_user_provider_coverage, merge_project_provider_backpressure,
    plan_user_provider_admission,
};
use super::startup::{StartupUserIngestClaim, StartupUserIngestGuard, TranscriptIngestOutcome};
use super::user::{profile_sessions_authority_matches, provider_selected};

const TEST_INGEST_BOUNDS: IngestPassBounds = IngestPassBounds {
    discovered_units: 16,
    units_per_pass: 8,
    units_per_source: 8,
    queue_depth: 8,
    bytes_per_unit: 1024,
    bytes_per_pass: 4096,
    retries: 0,
};

#[test]
fn profile_sessions_authority_rejects_a_wrong_brain_id() {
    let expected_brain = BrainId::new("brain.expected").unwrap();
    let foreign_brain = BrainId::new("brain.foreign").unwrap();
    let profile_id = UserProfileId::new("profile.authority-test").unwrap();
    let registered_shard = StoreShardIdV1::profile_sessions(foreign_brain, profile_id.clone());

    assert!(!profile_sessions_authority_matches(
        &registered_shard,
        &expected_brain,
        &profile_id,
    ));
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

#[tokio::test]
async fn cancelled_codex_provider_stops_before_opening_the_next_jsonl_source() {
    use crate::admission::test_support::PanicHostAdmission;

    let temp = tempfile::tempdir().unwrap();
    let project_id = ProjectId::new("scheduler-test-cancelled").unwrap();
    let cancellation = ObservationCancellation::default();
    cancellation.cancel();

    let outcome =
        codex::try_admit_codex_jsonl_observations_for_project_with_admission_and_cancellation(
            &temp.path().join("must-not-open.jsonl"),
            temp.path(),
            project_id,
            &PanicHostAdmission,
            None,
            &cancellation,
        )
        .await
        .unwrap();

    assert_eq!(outcome.bytes_consumed, 0);
    assert!(outcome.source_deferred);
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
        tracedecay_runtime_core::privacy::PrivacySanitizerError::InvalidPolicy,
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
fn still_mounting_admission_failures_keep_the_admission_retryability() {
    let error = crate::runtime::snapshot_observation::host_admission_error(
        "codex",
        crate::admission::HostAdmissionOutcome {
            status: crate::admission::HostAdmissionStatus::Unavailable,
            retryable: true,
            reason_code: Some("authority_write_failed"),
        },
    );

    let failure = classify_transcript_ingest_failure("codex", "observation", &error);

    assert_eq!(failure.reason_code, "authority_write_failed");
    assert!(
        failure.retryable,
        "a still-mounting write authority is a transient race, not a terminal block"
    );
}

#[test]
fn permanent_admission_failures_still_classify_permanent() {
    let error = crate::runtime::snapshot_observation::host_admission_error(
        "codex",
        crate::admission::HostAdmissionOutcome {
            status: crate::admission::HostAdmissionStatus::Degraded,
            retryable: false,
            reason_code: Some("invalid_observation_contract"),
        },
    );

    let failure = classify_transcript_ingest_failure("codex", "observation", &error);

    assert_eq!(failure.reason_code, "invalid_observation_contract");
    assert!(!failure.retryable);
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

use crate::runtime::git_correlation::test_support::MemoryEvidenceGraphRuntime;

struct GraphBackedTestStore {
    connection: tracedecay_runtime_core::db::engine::TestConnection,
    graph: MemoryEvidenceGraphRuntime,
}

impl git_correlation::GitCorrelationSessionStore for GraphBackedTestStore {
    type ReadSnapshot = tracedecay_runtime_core::db::engine::ReadSnapshot;
    type WriteTxn<'txn> = tracedecay_runtime_core::db::engine::Transaction;

    fn require_project_sessions_authority(
        &self,
    ) -> Result<(), git_correlation::GitCorrelationError> {
        Ok(())
    }

    async fn read_snapshot(
        &self,
    ) -> Result<
        tracedecay_runtime_core::db::engine::ReadSnapshot,
        git_correlation::GitCorrelationError,
    > {
        self.connection
            .read_snapshot()
            .await
            .map_err(git_correlation::GitCorrelationError::from)
    }

    async fn open_write_transaction(
        &self,
    ) -> Result<
        tracedecay_runtime_core::db::engine::Transaction,
        git_correlation::GitCorrelationError,
    > {
        self.connection
            .transaction_with_behavior(
                tracedecay_runtime_core::db::engine::TransactionBehavior::Immediate,
            )
            .await
            .map_err(git_correlation::GitCorrelationError::from)
    }

    fn git_evidence_publication_lock(
        &self,
    ) -> Result<&std::sync::Mutex<()>, git_correlation::GitCorrelationError> {
        Ok(self.graph.git_evidence_publication_lock())
    }

    fn graph_runtime(
        &self,
    ) -> Result<&dyn VerifiedGraphRuntimePortV1, git_correlation::GitCorrelationError> {
        Ok(&self.graph)
    }
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

    let store_dir = tempfile::tempdir().unwrap();
    let store = GraphBackedTestStore {
        connection: tracedecay_runtime_core::db::engine::TestConnection::open(
            &store_dir.path().join("sessions.db"),
        ),
        graph: MemoryEvidenceGraphRuntime::default(),
    };

    // A session recording "now" — the commit lands inside its span, exactly as
    // it does when an agent commits mid-session.
    let now = tracedecay_runtime_core::tracedecay::current_timestamp();
    let worktree = git_correlation::normalize_worktree(&repo.path().to_string_lossy());
    let live_span = git_correlation::SessionGitSpan {
        span_id: "span-live".to_string(),
        provider: "claude".to_string(),
        session_id: "live".to_string(),
        thread_id: None,
        branch: Some("main".to_string()),
        worktree,
        first_ts: now - 60,
        last_ts: now,
        event_count: 2,
        source: git_correlation::SpanSource::HookRoute,
    };
    git_correlation::publish_graph_evidence(&store, "live-ingest", &[live_span], &[]).unwrap();

    let gap = git_correlation::DEFAULT_SPAN_MERGE_GAP_SECS;
    let inserted = git_correlation::run_commit_attribution_sweep(&store, gap, |target| {
        super::project::git_scan_commits(target, gap)
    })
    .await
    .unwrap();
    assert!(
        inserted >= 1,
        "a commit made during a live session must be attributed"
    );

    let identity = git_correlation::git_evidence_projection_identity(
        tracedecay_graph_db::GraphNamespace::new("project").unwrap(),
    )
    .unwrap();
    let evidence = git_correlation::recover_git_evidence_projection(
        git_correlation::GitCorrelationSessionStore::graph_runtime(&store).unwrap(),
        &identity,
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    )
    .unwrap()
    .expect("attribution published the evidence projection");
    let hits = evidence.sessions_for_with_relation(
        &git_correlation::SessionsForQuery {
            git_ref: git_correlation::GitRefFilter::parse("commit", &sha).unwrap(),
            since: None,
            until: None,
            limit: 10,
        },
        git_correlation::CommitRelationFilter::All,
    );
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
    let again = git_correlation::run_commit_attribution_sweep(&store, gap, |target| {
        super::project::git_scan_commits(target, gap)
    })
    .await
    .unwrap();
    assert_eq!(again, 0, "re-sweeping an attributed commit is a no-op");
}

/// A fresh project has never published a Git evidence projection. The
/// attribution sweep must complete as a typed no-op — not report a retryable
/// unavailability, which put the ingest pass into an endless retry loop on
/// every fresh project (the hermes stock journey surfaced it as a
/// "graph projection has no relational verified head" warning storm).
#[tokio::test]
async fn attribution_sweep_over_a_never_published_projection_is_a_typed_no_op() {
    let store_dir = tempfile::tempdir().unwrap();
    let store = GraphBackedTestStore {
        connection: tracedecay_runtime_core::db::engine::TestConnection::open(
            &store_dir.path().join("sessions.db"),
        ),
        graph: MemoryEvidenceGraphRuntime::default(),
    };

    let gap = git_correlation::DEFAULT_SPAN_MERGE_GAP_SECS;
    let inserted = git_correlation::run_commit_attribution_sweep(&store, gap, |_| {
        panic!("a never-published projection has no span targets to scan")
    })
    .await
    .expect("the empty start is not an error");
    assert_eq!(inserted, 0, "nothing to attribute on the empty start");
}

#[test]
fn concurrent_same_session_observations_merge_under_the_publication_lock() {
    let store_dir = tempfile::tempdir().unwrap();
    let graph = MemoryEvidenceGraphRuntime::default();
    // Hold the first recovery read at a gate so the publications overlap:
    // one publisher sits inside its snapshot read while the other contends
    // for the publication lock. Without the lock, both would read the empty
    // projection and the merge below would lose an observation.
    graph.gate_snapshot_reads();
    let store = std::sync::Arc::new(GraphBackedTestStore {
        connection: tracedecay_runtime_core::db::engine::TestConnection::open(
            &store_dir.path().join("sessions.db"),
        ),
        graph,
    });
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let make_observation = |ts| git_correlation::SpanObservation {
        provider: "codex".to_owned(),
        session_id: "session-concurrent".to_owned(),
        thread_id: None,
        branch: Some("main".to_owned()),
        worktree: "/repo".to_owned(),
        ts,
        source: git_correlation::SpanSource::Ingest,
    };
    let first = make_observation(10);
    let second = make_observation(20);

    std::thread::scope(|scope| {
        for observation in [first, second] {
            let store = std::sync::Arc::clone(&store);
            let barrier = std::sync::Arc::clone(&barrier);
            scope.spawn(move || {
                barrier.wait();
                git_correlation::publish_transcript_graph_evidence(
                    store.as_ref(),
                    "concurrent-observation",
                    std::slice::from_ref(&observation),
                    &[],
                    git_correlation::DEFAULT_SPAN_MERGE_GAP_SECS,
                )
                .unwrap();
            });
        }
        store.graph.await_gated_snapshot_reader();
        store.graph.release_gated_snapshot_reads();
    });

    let identity = git_correlation::git_evidence_projection_identity(
        tracedecay_graph_db::GraphNamespace::new("project").unwrap(),
    )
    .unwrap();
    let evidence = git_correlation::recover_git_evidence_projection(
        git_correlation::GitCorrelationSessionStore::graph_runtime(store.as_ref()).unwrap(),
        &identity,
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    )
    .unwrap()
    .expect("concurrent publications produced a verified head");
    let spans = evidence.projection().spans();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].first_ts, 10);
    assert_eq!(spans[0].last_ts, 20);
    assert_eq!(spans[0].event_count, 2);
}

#[test]
fn admitted_bounded_publication_observes_live_operation_cancellation() {
    let store_dir = tempfile::tempdir().unwrap();
    let graph = MemoryEvidenceGraphRuntime::default();
    graph.gate_snapshot_reads();
    let store = std::sync::Arc::new(GraphBackedTestStore {
        connection: tracedecay_runtime_core::db::engine::TestConnection::open(
            &store_dir.path().join("sessions.db"),
        ),
        graph,
    });
    let cancellation = ObservationCancellation::default();
    let publication_cancellation = cancellation.verified_graph_cancellation();
    let publisher_store = std::sync::Arc::clone(&store);
    let publisher = std::thread::spawn(move || {
        git_correlation::publish_graph_evidence_controlled(
            publisher_store.as_ref(),
            "cancelled-bounded-publication",
            &[],
            &[],
            publication_cancellation,
        )
    });
    store.graph.await_gated_snapshot_reader();
    assert_eq!(
        store.graph.gated_snapshot_readers_entered(),
        1,
        "the publisher must be inside its recovery read before cancellation"
    );
    cancellation.cancel();
    store.graph.release_gated_snapshot_reads();

    assert_eq!(
        publisher.join().unwrap().unwrap_err(),
        git_correlation::GitCorrelationError::Cancelled
    );
}

#[test]
fn admitted_bounded_publication_settles_durable_success_before_late_cancellation() {
    let store_dir = tempfile::tempdir().unwrap();
    let graph = MemoryEvidenceGraphRuntime::default();
    graph.cancel_request_after_next_publish();
    let store = GraphBackedTestStore {
        connection: tracedecay_runtime_core::db::engine::TestConnection::open(
            &store_dir.path().join("sessions.db"),
        ),
        graph,
    };
    let cancellation = ObservationCancellation::default();

    let published = git_correlation::publish_graph_evidence_controlled(
        &store,
        "late-cancelled-bounded-publication",
        &[],
        &[],
        cancellation.verified_graph_cancellation(),
    );

    assert!(published.is_ok(), "verified-head CAS already committed");
    assert!(cancellation.is_cancelled());
}

#[test]
fn startup_user_ingest_claims_are_single_flight_and_cancellation_safe() {
    let profile = tempfile::tempdir().unwrap().path().to_path_buf();
    let StartupUserIngestClaim::Acquired(first) = StartupUserIngestGuard::claim(profile.clone())
    else {
        panic!("first claim must acquire");
    };
    assert!(matches!(
        StartupUserIngestGuard::claim(profile.clone()),
        StartupUserIngestClaim::Running
    ));

    drop(first);
    let StartupUserIngestClaim::Acquired(mut retry) =
        StartupUserIngestGuard::claim(profile.clone())
    else {
        panic!("an incomplete claim must release immediately");
    };
    retry.completed = true;
    drop(retry);

    assert!(
        matches!(
            StartupUserIngestGuard::claim(profile),
            StartupUserIngestClaim::RecentlyCompleted
        ),
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
