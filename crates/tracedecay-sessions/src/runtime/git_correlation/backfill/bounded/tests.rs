use std::path::Path;
use std::process::Command;
use std::time::Duration;

use tracedecay_runtime_core::db::engine::{
    Executor, QueryExecutor, ReadSnapshot, TestConnection, Transaction, TransactionBehavior, params,
};

use super::*;

impl GitCorrelationWriteTxn for Transaction {
    async fn commit(self) -> Result<(), GitCorrelationError> {
        Transaction::commit(self)
            .await
            .map_err(GitCorrelationError::from)
    }
}

struct TestStore {
    connection: TestConnection,
}

impl TestStore {
    fn open(path: &Path) -> Self {
        Self {
            connection: TestConnection::open(path),
        }
    }
}

impl GitCorrelationSessionStore for TestStore {
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
        self.connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(GitCorrelationError::from)
    }
}

fn git(path: &Path, args: &[&str]) {
    let output = Command::new(tracedecay_runtime_core::git::git_program())
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
    git(fixture.path(), &["init", "-b", "main"]);
    std::fs::write(fixture.path().join("tracked"), "content").unwrap();
    git(fixture.path(), &["add", "tracked"]);
    git(fixture.path(), &["commit", "-m", "initial"]);
    fixture
}

async fn prepare_store(path: &Path, project_path: &Path) -> TestStore {
    let store = TestStore::open(path);
    crate::runtime::git_correlation::ensure_git_correlation_schema(&store.connection)
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

async fn scalar(store: &TestStore, sql: &str) -> i64 {
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
    assert_eq!(
        scalar(&store, "SELECT COUNT(*) FROM session_git_spans").await,
        0
    );
    drop(store);

    let reopened = TestStore::open(&database);
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
    let spans = scalar(&reopened, "SELECT COUNT(*) FROM session_git_spans").await;
    let commits = scalar(&reopened, "SELECT COUNT(*) FROM commit_sessions").await;
    assert!(spans > 0);
    assert!(commits > 0);

    let repeated = run_bounded_history_index_page(
        &reopened,
        &options(false),
        &BoundedGitControl::new(ObservationCancellation::default(), Duration::from_secs(10)),
    )
    .await
    .unwrap();
    assert_eq!(repeated.stats.sessions_scanned, 0);
    assert_eq!(
        scalar(&reopened, "SELECT COUNT(*) FROM session_git_spans").await,
        spans
    );
    assert_eq!(
        scalar(&reopened, "SELECT COUNT(*) FROM commit_sessions").await,
        commits
    );
}

#[tokio::test]
async fn activity_change_finishes_sealed_candidate_before_newer_row() {
    let repository = repository_fixture();
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("sessions.db");
    let store = prepare_store(&database, repository.path()).await;
    store
        .connection
        .execute(
            "UPDATE sessions SET ended_at = 100 WHERE session_id = 'session-1'",
            (),
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
        100
    );

    store
        .connection
        .execute(
            "UPDATE sessions SET ended_at = 200 WHERE session_id = 'session-1'",
            (),
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
    assert_eq!(resumed.frontier.activity_timestamp, 100);
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
    assert_eq!(newer.frontier.activity_timestamp, 200);
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
        "session_git_spans",
        "commit_sessions",
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
