use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use tracedecay_runtime_core::db::engine::{
    Executor, QueryExecutor, ReadSnapshot, TestConnection, Transaction, TransactionBehavior, params,
};
use tracedecay_runtime_core::store_runtime::VerifiedGraphRuntimePortV1;

use super::*;
use crate::runtime::git_correlation::test_support::MemoryEvidenceGraphRuntime;
use crate::runtime::git_correlation::{
    ensure_git_correlation_receipt_schema_in_transaction, read_meta_value,
};

impl GitCorrelationWriteTxn for Transaction {
    async fn commit(self) -> Result<(), GitCorrelationError> {
        Transaction::commit(self)
            .await
            .map_err(GitCorrelationError::from)
    }
}

struct TestStore {
    connection: TestConnection,
    graph: std::sync::Arc<MemoryEvidenceGraphRuntime>,
    fail_next_write: AtomicBool,
}

impl TestStore {
    fn open(path: &Path) -> Self {
        Self::open_with_graph(
            path,
            std::sync::Arc::new(MemoryEvidenceGraphRuntime::default()),
        )
    }

    /// Reopen against the graph state a prior store instance published, the
    /// way a restarted daemon sees the durable graph next to its receipts.
    fn open_with_graph(path: &Path, graph: std::sync::Arc<MemoryEvidenceGraphRuntime>) -> Self {
        Self {
            connection: TestConnection::open(path),
            graph,
            fail_next_write: AtomicBool::new(false),
        }
    }

    fn fail_next_write(&self) {
        self.fail_next_write.store(true, Ordering::Release);
    }
}

impl GitCorrelationSessionStore for TestStore {
    type ReadSnapshot = ReadSnapshot;
    type WriteTxn<'txn> = Transaction;

    fn require_project_sessions_authority(&self) -> Result<(), GitCorrelationError> {
        Ok(())
    }

    async fn read_snapshot(&self) -> Result<ReadSnapshot, GitCorrelationError> {
        self.connection
            .read_snapshot()
            .await
            .map_err(GitCorrelationError::from)
    }

    async fn open_write_transaction(&self) -> Result<Transaction, GitCorrelationError> {
        if self.fail_next_write.swap(false, Ordering::AcqRel) {
            return Err(GitCorrelationError::Db(
                "injected frontier transaction failure".to_owned(),
            ));
        }
        self.connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(GitCorrelationError::from)
    }

    fn git_evidence_publication_lock(
        &self,
    ) -> Result<Arc<std::sync::Mutex<()>>, GitCorrelationError> {
        Ok(self.graph.git_evidence_publication_lock())
    }

    fn graph_runtime(&self) -> Result<&dyn VerifiedGraphRuntimePortV1, GitCorrelationError> {
        Ok(self.graph.as_ref())
    }
}

fn git(path: &Path, args: &[&str]) {
    let output = Command::new(
        tracedecay_runtime_core::git::try_git_program()
            .expect("absolute git executable should resolve"),
    )
    .current_dir(path)
    .args(args)
    .env("GIT_AUTHOR_NAME", "TraceDecay")
    .env("GIT_AUTHOR_EMAIL", "test@tracedecay.invalid")
    .env("GIT_COMMITTER_NAME", "TraceDecay")
    .env("GIT_COMMITTER_EMAIL", "test@tracedecay.invalid")
    .output()
    .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository_fixture() -> tempfile::TempDir {
    let fixture = tempfile::tempdir().unwrap();
    initialize_repository(fixture.path());
    fixture
}

struct FailCommitLogCall {
    calls: AtomicUsize,
    fail_on: usize,
}

impl GitReflogSource for FailCommitLogCall {
    fn reflog(&self, worktree: &Path) -> Option<String> {
        SystemGit.reflog(worktree)
    }

    fn current_branch(&self, worktree: &Path) -> Option<String> {
        SystemGit.current_branch(worktree)
    }

    fn commit_log(&self, worktree: &Path, branch: &str, since: i64) -> Option<String> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == self.fail_on {
            None
        } else {
            SystemGit.commit_log(worktree, branch, since)
        }
    }
}

fn initialize_repository(path: &Path) {
    git(path, &["init", "-b", "main"]);
    std::fs::write(path.join("tracked"), "content").unwrap();
    git(path, &["add", "tracked"]);
    git(path, &["commit", "-m", "initial"]);
}

fn append_linear_history(path: &Path, commit_count: usize) {
    let mut stream = String::new();
    for index in 0..commit_count {
        let mark = index.saturating_add(1);
        let timestamp = mark;
        let message = format!("imported-{mark}");
        stream.push_str("commit refs/heads/imported\n");
        stream.push_str(&format!("mark :{mark}\n"));
        stream.push_str(&format!(
            "author TraceDecay <test@tracedecay.invalid> {timestamp} +0000\n"
        ));
        stream.push_str(&format!(
            "committer TraceDecay <test@tracedecay.invalid> {timestamp} +0000\n"
        ));
        stream.push_str(&format!("data {}\n{message}\n", message.len()));
        if index == 0 {
            stream.push_str("from refs/heads/main\n");
        } else {
            stream.push_str(&format!("from :{}\n", mark.saturating_sub(1)));
        }
    }
    stream.push_str("done\n");

    let mut child = Command::new(
        tracedecay_runtime_core::git::try_git_program()
            .expect("absolute git executable should resolve"),
    )
    .current_dir(path)
    .args(["fast-import", "--quiet"])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stream.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "git fast-import: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let timestamp = commit_count.to_string();
    let output = Command::new(
        tracedecay_runtime_core::git::try_git_program()
            .expect("absolute git executable should resolve"),
    )
    .current_dir(path)
    .args(["reset", "--hard", "refs/heads/imported"])
    .env("GIT_COMMITTER_DATE", format!("@{timestamp} +0000"))
    .output()
    .unwrap();
    assert!(
        output.status.success(),
        "git reset: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn head_commit_time(path: &Path) -> i64 {
    gix::discover(path)
        .unwrap()
        .head_commit()
        .unwrap()
        .time()
        .unwrap()
        .seconds
}

async fn prepare_store(path: &Path, project_path: &Path) -> TestStore {
    let store = TestStore::open(path);
    ensure_git_correlation_receipt_schema_in_transaction(&store.connection)
        .await
        .unwrap();
    store
        .connection
        .execute_batch(
            "CREATE TABLE sessions (
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                project_path TEXT NOT NULL,
                started_at INTEGER,
                ended_at INTEGER,
                PRIMARY KEY(provider, session_id)
            );
            CREATE TABLE session_messages (
                provider TEXT NOT NULL,
                message_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                timestamp INTEGER,
                PRIMARY KEY(provider, message_id)
            );",
        )
        .await
        .unwrap();
    store
        .connection
        .execute(
            "INSERT INTO sessions(provider, session_id, project_path, started_at, ended_at)
             VALUES ('codex', 'session-1', ?1, 0, ?2)",
            params![project_path.to_str().unwrap(), i64::MAX],
        )
        .await
        .unwrap();
    store
}

#[tokio::test]
async fn incremental_publication_failure_holds_frontier_until_retry_succeeds() {
    let repository = repository_fixture();
    let directory = tempfile::tempdir().unwrap();
    let store = prepare_store(&directory.path().join("sessions.db"), repository.path()).await;
    store.graph.fail_next_publication();

    let failed = run_incremental_backfill(&store, &SystemGit, 1)
        .await
        .unwrap();
    assert_eq!(failed.sessions_scanned, 1);
    assert_eq!(failed.skipped_git_error, 1);
    assert_eq!(
        read_meta_value(&store.connection, AUTO_BACKFILL_WATERMARK_KEY)
            .await
            .unwrap(),
        None,
        "a transient graph failure must not settle the source tuple"
    );

    let retried = run_incremental_backfill(&store, &SystemGit, 1)
        .await
        .unwrap();
    assert_eq!(retried.sessions_scanned, 1);
    assert_eq!(retried.skipped_git_error, 0);
    assert!(retried.spans_written > 0);
    assert_eq!(
        read_meta_value(&store.connection, AUTO_BACKFILL_WATERMARK_KEY)
            .await
            .unwrap(),
        Some(i64::MAX)
    );
    assert_eq!(
        run_incremental_backfill(&store, &SystemGit, 1)
            .await
            .unwrap()
            .sessions_scanned,
        0
    );
}

#[tokio::test]
async fn incremental_missing_commit_log_holds_frontier_until_retry_succeeds() {
    let repository = repository_fixture();
    let directory = tempfile::tempdir().unwrap();
    let store = prepare_store(&directory.path().join("sessions.db"), repository.path()).await;
    let git = FailCommitLogCall {
        calls: AtomicUsize::new(0),
        fail_on: 0,
    };

    let failed = run_incremental_backfill(&store, &git, 1).await.unwrap();
    assert_eq!(failed.skipped_git_error, 1);
    assert!(!failed.frontier_advanced);
    assert_eq!(
        read_meta_value(&store.connection, AUTO_BACKFILL_WATERMARK_KEY)
            .await
            .unwrap(),
        None
    );

    let retried = run_incremental_backfill(&store, &git, 1).await.unwrap();
    assert_eq!(retried.skipped_git_error, 0);
    assert!(retried.frontier_advanced);
}

#[tokio::test]
async fn later_attribution_failure_returns_committed_backfill_progress() {
    let repository = repository_fixture();
    let directory = tempfile::tempdir().unwrap();
    let store = prepare_store(&directory.path().join("sessions.db"), repository.path()).await;
    let git = FailCommitLogCall {
        calls: AtomicUsize::new(0),
        fail_on: 1,
    };

    let partial = run_incremental_backfill_outcome(&store, &git, 1)
        .await
        .unwrap();
    assert!(partial.stats.frontier_advanced);
    assert!(partial.stats.spans_written > 0);
    assert_eq!(partial.stats.skipped_git_error, 1);
    assert!(matches!(
        partial.later_failure,
        Some(GitCorrelationError::Unavailable(_))
    ));

    let retried = run_incremental_backfill(&store, &SystemGit, 1)
        .await
        .unwrap();
    assert_eq!(retried.sessions_scanned, 0);
    assert_eq!(retried.skipped_git_error, 0);
}

#[tokio::test]
async fn later_frontier_failure_returns_committed_graph_progress() {
    let repository = repository_fixture();
    let directory = tempfile::tempdir().unwrap();
    let store = prepare_store(&directory.path().join("sessions.db"), repository.path()).await;
    store.fail_next_write();

    let partial = run_incremental_backfill_outcome(&store, &SystemGit, 1)
        .await
        .unwrap();
    assert!(partial.stats.spans_written > 0);
    assert!(!partial.stats.frontier_advanced);
    assert!(matches!(
        partial.later_failure,
        Some(GitCorrelationError::Db(_))
    ));
    assert_eq!(
        read_meta_value(&store.connection, AUTO_BACKFILL_WATERMARK_KEY)
            .await
            .unwrap(),
        None
    );

    let retried = run_incremental_backfill(&store, &SystemGit, 1)
        .await
        .unwrap();
    assert!(retried.frontier_advanced);
}

#[tokio::test]
async fn unavailable_attribution_target_is_a_retryable_error() {
    let repository = repository_fixture();
    let directory = tempfile::tempdir().unwrap();
    let store = prepare_store(&directory.path().join("sessions.db"), repository.path()).await;
    run_incremental_backfill(&store, &SystemGit, 1)
        .await
        .unwrap();

    let error = run_commit_attribution_sweep(&store, 0, |_| TargetScan::Unavailable)
        .await
        .unwrap_err();
    assert!(matches!(error, GitCorrelationError::Unavailable(_)));
}

#[tokio::test]
async fn incremental_permanent_exclusion_advances_frontier() {
    let plain_directory = tempfile::tempdir().unwrap();
    let database_directory = tempfile::tempdir().unwrap();
    let store = prepare_store(
        &database_directory.path().join("sessions.db"),
        plain_directory.path(),
    )
    .await;

    let excluded = run_incremental_backfill(&store, &SystemGit, 1)
        .await
        .unwrap();

    assert_eq!(excluded.sessions_scanned, 1);
    assert_eq!(excluded.skipped_not_worktree, 1);
    assert!(excluded.frontier_advanced);
    assert_eq!(
        read_meta_value(&store.connection, AUTO_BACKFILL_WATERMARK_KEY)
            .await
            .unwrap(),
        Some(i64::MAX)
    );
}

async fn scalar(store: &TestStore, sql: &str) -> i64 {
    let mut rows = store.connection.query(sql, ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn text_scalar(store: &TestStore, sql: &str) -> String {
    let mut rows = store.connection.query(sql, ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

fn options(dry_run: bool) -> BackfillOptions {
    BackfillOptions {
        since: 0,
        limit_sessions: 1,
        merge_gap_secs: 0,
        max_commits_per_repo: 100,
        dry_run,
    }
}

#[tokio::test]
async fn persisted_partial_reopens_and_converges_exactly_once() {
    let repository = repository_fixture();
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("sessions.db");
    let store = prepare_store(&database, repository.path()).await;
    let partial = run_bounded_history_index_page(
        &store,
        &options(false),
        &BoundedGitControl::new(
            ObservationCancellation::default(),
            Duration::from_millis(700),
        ),
    )
    .await
    .unwrap();
    assert_eq!(partial.frontier.activity_timestamp, -1);
    assert_eq!(
        scalar(&store, "SELECT COUNT(*) FROM git_history_index_progress").await,
        1
    );
    let durable_graph = std::sync::Arc::clone(&store.graph);
    drop(store);

    let reopened = TestStore::open_with_graph(&database, durable_graph);
    let completed = run_bounded_history_index_page(
        &reopened,
        &options(false),
        &BoundedGitControl::new(ObservationCancellation::default(), Duration::from_secs(10)),
    )
    .await
    .unwrap();
    assert_eq!(completed.interruption, None);
    assert_eq!(completed.frontier.activity_timestamp, i64::MAX);
    assert_eq!(
        scalar(&reopened, "SELECT COUNT(*) FROM git_history_index_progress").await,
        0
    );
    assert!(completed.stats.spans_written > 0);
    assert!(completed.stats.commits_attributed > 0);

    let repeated = run_bounded_history_index_page(
        &reopened,
        &options(false),
        &BoundedGitControl::new(ObservationCancellation::default(), Duration::from_secs(10)),
    )
    .await
    .unwrap();
    assert_eq!(repeated.stats.sessions_scanned, 0);
}

#[tokio::test]
async fn staged_graph_replacement_publishes_nothing_and_retry_converges() {
    let repository = repository_fixture();
    append_linear_history(repository.path(), MAX_GRAPH_PAGE_EXAMINED_NODES + 1);
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("sessions.db");
    let store = prepare_store(&database, repository.path()).await;
    let timestamp = i64::try_from(MAX_GRAPH_PAGE_EXAMINED_NODES + 1).unwrap();
    store
        .connection
        .execute(
            "UPDATE sessions SET started_at = ?1, ended_at = ?1",
            params![timestamp],
        )
        .await
        .unwrap();
    let mut unbounded_commits = options(false);
    unbounded_commits.max_commits_per_repo = usize::MAX;

    let staged = run_bounded_history_index_page(
        &store,
        &unbounded_commits,
        &BoundedGitControl::new(ObservationCancellation::default(), Duration::from_secs(10)),
    )
    .await
    .unwrap();
    assert_eq!(
        staged.interruption,
        Some(BoundedBackfillInterruption::HistoryTraversalBudgetReached)
    );
    assert!(
        scalar(
            &store,
            "SELECT COUNT(*) FROM git_history_index_staged_spans"
        )
        .await
            > 0
    );

    std::fs::rename(
        repository.path().join(".git"),
        repository.path().join(".git-original"),
    )
    .unwrap();
    initialize_repository(repository.path());
    let reset = run_bounded_history_index_page(
        &store,
        &unbounded_commits,
        &BoundedGitControl::new(ObservationCancellation::default(), Duration::from_secs(10)),
    )
    .await
    .unwrap();
    assert_eq!(reset.frontier.activity_timestamp, -1);
    for table in [
        "git_history_index_progress",
        "git_history_index_staged_spans",
        "git_history_index_staged_commits",
    ] {
        assert_eq!(
            scalar(&store, &format!("SELECT COUNT(*) FROM {table}")).await,
            0,
            "{table}"
        );
    }

    store
        .connection
        .execute(
            "UPDATE sessions SET started_at = 0, ended_at = ?1",
            params![i64::MAX],
        )
        .await
        .unwrap();
    let completed = run_bounded_history_index_page(
        &store,
        &unbounded_commits,
        &BoundedGitControl::new(ObservationCancellation::default(), Duration::from_secs(10)),
    )
    .await
    .unwrap();
    assert_eq!(completed.interruption, None);
    assert_eq!(completed.frontier.activity_timestamp, i64::MAX);
    assert!(completed.stats.spans_written > 0);
}

#[tokio::test]
async fn publish_verification_restart_rejects_same_path_repository_replacement() {
    let repository = repository_fixture();
    let directory = tempfile::tempdir().unwrap();
    let store = prepare_store(&directory.path().join("sessions.db"), repository.path()).await;
    let control =
        BoundedGitControl::new(ObservationCancellation::default(), Duration::from_secs(10));
    let row = SessionActivityRow {
        provider: "codex".to_string(),
        session_id: "session-1".to_string(),
        project_path: repository.path().to_str().unwrap().to_string(),
        started_at: Some(0),
        ended_at: Some(i64::MAX),
        message_min_ts: None,
        message_max_ts: None,
    };
    let cursor = native::initialize_reflog_cursor(repository.path(), i64::MAX, &control).unwrap();
    let key = GitHistoryProgressKey { source_rowid: 1 };
    let mut progress = progress_from_cursor(key, i64::MAX, &row, 0, i64::MAX, cursor).unwrap();
    progress.scan_mode = GitHistoryScanMode::Graph;
    progress.capture_target_offset = Some(progress.reflog_byte_offset);
    progress.verify_byte_offset = progress.reflog_byte_offset;
    progress.verify_digest.clone_from(&progress.reflog_digest);
    progress.segment_cursor = 1;
    history_progress::insert_progress(&store.connection, &progress)
        .await
        .unwrap();
    history_progress::upsert_segment(
        &store.connection,
        &GitHistorySegmentRow {
            key,
            ordinal: 0,
            branch: Some("main".to_string()),
            start_ts: 0,
            end_ts: i64::MAX,
            tip_oid: progress.segment_tip_oid.clone(),
            applied: true,
            completed: true,
        },
    )
    .await
    .unwrap();
    for (boundary, timestamp) in [(0, 0), (1, i64::MAX)] {
        history_progress::upsert_staged_span(
            &store.connection,
            &history_progress::GitHistoryStagedSpanRow {
                key,
                segment_ordinal: 0,
                boundary,
                branch: Some("main".to_string()),
                timestamp,
            },
        )
        .await
        .unwrap();
    }

    let worktree = canonical_worktree_path(&progress).unwrap();
    let mut committed = false;
    let transitioned = advance_graph(
        &store,
        &worktree,
        &progress,
        &options(false),
        &mut GraphPageBudget::default(),
        &control,
        &mut committed,
    )
    .await
    .unwrap();
    assert!(matches!(transitioned, StreamGitEvidenceOutcome::Progressed));
    assert!(committed);
    let snapshot = store.read_snapshot().await.unwrap();
    let pending_verification = history_progress::read_progress(&snapshot, key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        pending_verification.scan_mode,
        GitHistoryScanMode::PublishVerify
    );
    drop(snapshot);

    std::fs::rename(
        repository.path().join(".git"),
        repository.path().join(".git-original"),
    )
    .unwrap();
    initialize_repository(repository.path());
    let restarted = run_bounded_history_index_page(
        &store,
        &options(false),
        &BoundedGitControl::new(ObservationCancellation::default(), Duration::from_secs(10)),
    )
    .await
    .unwrap();

    assert_eq!(restarted.interruption, None);
    assert_eq!(restarted.frontier.activity_timestamp, -1);
    for table in [
        "git_history_index_progress",
        "git_history_index_staged_spans",
    ] {
        assert_eq!(
            scalar(&store, &format!("SELECT COUNT(*) FROM {table}")).await,
            0,
            "{table}"
        );
    }
}

#[tokio::test]
async fn out_of_window_deep_history_is_bounded_and_resumes_from_durable_graph_state() {
    let repository = repository_fixture();
    append_linear_history(repository.path(), MAX_GRAPH_PAGE_EXAMINED_NODES + 1);
    let directory = tempfile::tempdir().unwrap();
    let store = prepare_store(&directory.path().join("sessions.db"), repository.path()).await;
    let timestamp = i64::try_from(MAX_GRAPH_PAGE_EXAMINED_NODES + 1).unwrap();
    store
        .connection
        .execute(
            "UPDATE sessions SET started_at = ?1, ended_at = ?1",
            params![timestamp],
        )
        .await
        .unwrap();
    let mut unbounded_commits = options(false);
    unbounded_commits.max_commits_per_repo = usize::MAX;

    let partial = run_bounded_history_index_page(
        &store,
        &unbounded_commits,
        &BoundedGitControl::new(ObservationCancellation::default(), Duration::from_secs(10)),
    )
    .await
    .unwrap();
    assert_eq!(
        partial.interruption,
        Some(BoundedBackfillInterruption::HistoryTraversalBudgetReached)
    );
    assert_eq!(partial.frontier.activity_timestamp, -1);
    assert_eq!(
        scalar(&store, "SELECT COUNT(*) FROM git_history_index_seen").await,
        i64::try_from(MAX_GRAPH_PAGE_EXAMINED_NODES).unwrap()
    );
    let completed = run_bounded_history_index_page(
        &store,
        &unbounded_commits,
        &BoundedGitControl::new(ObservationCancellation::default(), Duration::from_secs(10)),
    )
    .await
    .unwrap();
    assert_eq!(completed.interruption, None);
    assert_eq!(completed.frontier.activity_timestamp, timestamp);
    assert_eq!(
        scalar(&store, "SELECT COUNT(*) FROM git_history_index_progress").await,
        0
    );
    assert_eq!(completed.stats.commits_attributed, 1);
}

#[cfg(unix)]
#[tokio::test]
async fn resume_uses_sealed_canonical_worktree_after_alias_repoint() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let canonical = root.path().join("canonical");
    let replacement = root.path().join("replacement");
    let alias = root.path().join("admitted-alias");
    std::fs::create_dir(&canonical).unwrap();
    std::fs::create_dir(&replacement).unwrap();
    initialize_repository(&canonical);
    initialize_repository(&replacement);
    symlink(&canonical, &alias).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let store = prepare_store(&directory.path().join("sessions.db"), &alias).await;

    run_bounded_history_index_page(
        &store,
        &options(false),
        &BoundedGitControl::new(
            ObservationCancellation::default(),
            Duration::from_millis(700),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        scalar(&store, "SELECT COUNT(*) FROM git_history_index_progress").await,
        1
    );
    std::fs::remove_file(&alias).unwrap();
    symlink(&replacement, &alias).unwrap();
    let completed = run_bounded_history_index_page(
        &store,
        &options(false),
        &BoundedGitControl::new(ObservationCancellation::default(), Duration::from_secs(10)),
    )
    .await
    .unwrap();

    assert_eq!(completed.interruption, None);
    assert!(completed.stats.spans_written > 0);
}

/// Report whether `directory`'s filesystem accepts a name that is not valid
/// UTF-8.
///
/// `cfg(unix)` is a compile gate, not a filesystem capability: APFS refuses
/// such a name outright with `EILSEQ`, so a macOS run fails at the fixture
/// instead of exercising the backfill. Probing keeps the coverage everywhere
/// the bytes are really accepted and makes the skip visible where they are not.
#[cfg(unix)]
fn non_utf8_file_names_supported(directory: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStringExt as _;

    let probe = directory.join(std::ffi::OsString::from_vec(b"probe-\xff".to_vec()));
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(unix)]
#[tokio::test]
async fn non_utf8_canonical_worktree_resumes_exactly_then_fails_typed_publish() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    if !non_utf8_file_names_supported(root.path()) {
        println!(
            "skipping non_utf8_canonical_worktree_resumes_exactly_then_fails_typed_publish: \
             this filesystem refuses non-UTF-8 file names"
        );
        return;
    }
    let canonical = root
        .path()
        .join(OsString::from_vec(b"canonical-\xff".to_vec()));
    let replacement = root.path().join("replacement");
    let alias = root.path().join("admitted-alias");
    std::fs::create_dir(&canonical).unwrap();
    std::fs::create_dir(&replacement).unwrap();
    initialize_repository(&canonical);
    initialize_repository(&replacement);
    symlink(&canonical, &alias).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let store = prepare_store(&directory.path().join("sessions.db"), &alias).await;

    run_bounded_history_index_page(
        &store,
        &options(false),
        &BoundedGitControl::new(
            ObservationCancellation::default(),
            Duration::from_millis(700),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        scalar(&store, "SELECT COUNT(*) FROM git_history_index_progress").await,
        1
    );
    std::fs::remove_file(&alias).unwrap();
    symlink(&replacement, &alias).unwrap();
    let unsupported = run_bounded_history_index_page(
        &store,
        &options(false),
        &BoundedGitControl::new(ObservationCancellation::default(), Duration::from_secs(10)),
    )
    .await
    .unwrap();

    assert_eq!(unsupported.stats.skipped_git_error, 1);
    assert_eq!(unsupported.unresolved_failures, 1);
    assert_eq!(
        text_scalar(
            &store,
            "SELECT reason FROM git_history_index_failures LIMIT 1"
        )
        .await,
        "unsupported_canonical_worktree_encoding"
    );
    assert_eq!(
        scalar(&store, "SELECT COUNT(*) FROM git_history_index_progress").await,
        0
    );
}

#[tokio::test]
async fn activity_change_finishes_sealed_candidate_before_newer_row() {
    let repository = repository_fixture();
    let old_activity = head_commit_time(repository.path());
    let new_activity = old_activity.checked_add(1).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("sessions.db");
    let store = prepare_store(&database, repository.path()).await;
    store
        .connection
        .execute(
            "UPDATE sessions SET ended_at = ?1 WHERE session_id = 'session-1'",
            params![old_activity],
        )
        .await
        .unwrap();
    let partial = run_bounded_history_index_page(
        &store,
        &options(false),
        &BoundedGitControl::new(
            ObservationCancellation::default(),
            Duration::from_millis(700),
        ),
    )
    .await
    .unwrap();
    assert_eq!(partial.frontier.activity_timestamp, -1);
    assert_eq!(
        scalar(
            &store,
            "SELECT activity_timestamp FROM git_history_index_progress"
        )
        .await,
        old_activity
    );

    store
        .connection
        .execute(
            "UPDATE sessions SET ended_at = ?1 WHERE session_id = 'session-1'",
            params![new_activity],
        )
        .await
        .unwrap();
    let resumed = run_bounded_history_index_page(
        &store,
        &options(false),
        &BoundedGitControl::new(ObservationCancellation::default(), Duration::from_secs(10)),
    )
    .await
    .unwrap();
    assert_eq!(resumed.frontier.activity_timestamp, old_activity);
    assert_eq!(
        scalar(&store, "SELECT COUNT(*) FROM git_history_index_progress").await,
        0
    );

    let newer = run_bounded_history_index_page(
        &store,
        &options(false),
        &BoundedGitControl::new(ObservationCancellation::default(), Duration::from_secs(10)),
    )
    .await
    .unwrap();
    assert_eq!(newer.frontier.activity_timestamp, new_activity);
}

#[tokio::test]
async fn malformed_source_is_durable_while_later_sessions_advance_and_recovery_clears_it() {
    let malformed_repository = repository_fixture();
    let valid_repository = repository_fixture();
    let malformed_activity = head_commit_time(malformed_repository.path());
    let valid_activity =
        head_commit_time(valid_repository.path()).max(malformed_activity.checked_add(1).unwrap());
    let recovered_activity = valid_activity.checked_add(1).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("sessions.db");
    let store = prepare_store(&database, malformed_repository.path()).await;
    store
        .connection
        .execute(
            "UPDATE sessions SET ended_at = ?1 WHERE session_id = 'session-1'",
            params![malformed_activity],
        )
        .await
        .unwrap();
    store
        .connection
        .execute(
            "INSERT INTO sessions(provider, session_id, project_path, started_at, ended_at)
             VALUES ('codex', 'session-2', ?1, 0, ?2)",
            params![valid_repository.path().to_str().unwrap(), valid_activity],
        )
        .await
        .unwrap();
    let reflog = malformed_repository.path().join(".git/logs/HEAD");
    let valid_reflog = std::fs::read(&reflog).unwrap();
    let mut truncated_reflog = valid_reflog.clone();
    assert_eq!(truncated_reflog.pop(), Some(b'\n'));
    std::fs::write(&reflog, truncated_reflog).unwrap();
    let mut page_options = options(false);
    page_options.limit_sessions = 2;

    let outcome = run_bounded_history_index_page(
        &store,
        &page_options,
        &BoundedGitControl::new(ObservationCancellation::default(), Duration::from_secs(10)),
    )
    .await
    .unwrap();
    assert_eq!(outcome.interruption, None);
    assert_eq!(outcome.frontier.activity_timestamp, valid_activity);
    assert_eq!(outcome.stats.skipped_git_error, 1);
    assert_eq!(outcome.unresolved_failures, 1);
    assert_eq!(outcome.remaining_sessions, 0);
    assert!(outcome.stats.spans_written > 0);

    let empty_pass = run_bounded_history_index_page(
        &store,
        &page_options,
        &BoundedGitControl::new(ObservationCancellation::default(), Duration::from_secs(10)),
    )
    .await
    .unwrap();
    assert_eq!(empty_pass.stats.sessions_scanned, 0);
    assert_eq!(empty_pass.remaining_sessions, 0);
    assert_eq!(empty_pass.unresolved_failures, 1);

    std::fs::write(&reflog, valid_reflog).unwrap();
    store
        .connection
        .execute(
            "UPDATE sessions SET ended_at = ?1 WHERE session_id = 'session-1'",
            params![recovered_activity],
        )
        .await
        .unwrap();
    let recovered = run_bounded_history_index_page(
        &store,
        &page_options,
        &BoundedGitControl::new(ObservationCancellation::default(), Duration::from_secs(10)),
    )
    .await
    .unwrap();
    assert_eq!(recovered.frontier.activity_timestamp, recovered_activity);
    assert_eq!(recovered.unresolved_failures, 0);
    assert_eq!(
        scalar(&store, "SELECT COUNT(*) FROM git_history_index_failures").await,
        0
    );
}

#[tokio::test]
async fn dry_run_leaves_progress_evidence_and_frontier_untouched() {
    let repository = repository_fixture();
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("sessions.db");
    let store = prepare_store(&database, repository.path()).await;
    let outcome = run_bounded_history_index_page(
        &store,
        &options(true),
        &BoundedGitControl::new(ObservationCancellation::default(), Duration::from_secs(10)),
    )
    .await
    .unwrap();

    assert!(!outcome.committed);
    assert!(outcome.stats.spans_written > 0);
    assert!(outcome.stats.commits_attributed > 0);
    for table in [
        "git_history_index_progress",
        "git_history_index_segments",
        "git_history_index_pending",
        "git_history_index_seen",
        "git_history_index_failures",
        "git_correlation_meta",
    ] {
        assert_eq!(
            scalar(&store, &format!("SELECT COUNT(*) FROM {table}")).await,
            0,
            "{table}"
        );
    }
}

#[test]
fn cancellation_precedes_deadline() {
    let cancellation = ObservationCancellation::default();
    cancellation.cancel();
    let control = BoundedGitControl::new(cancellation, Duration::ZERO);
    assert_eq!(
        control.check().unwrap_err(),
        BoundedBackfillInterruption::Cancelled
    );
}

#[test]
fn interrupted_evidence_keeps_the_completed_row_frontier() {
    let frontier = GitHistoryIndexFrontier {
        activity_timestamp: 100,
        source_rowid: 7,
    };
    let outcome = interrupted_outcome(
        BackfillStats::default(),
        false,
        frontier,
        BoundedBackfillInterruption::CommandTimedOut,
    );
    assert_eq!(outcome.frontier, frontier);
    assert_eq!(outcome.remaining_sessions, 1);
}

#[test]
fn bounded_history_page_reports_unconsumed_session_suffix() {
    assert!(bounded_page_has_more(51, 50));
    assert!(!bounded_page_has_more(50, 50));
}
