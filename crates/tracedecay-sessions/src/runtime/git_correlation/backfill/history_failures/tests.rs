use std::path::Path;
use std::time::Duration;

use tracedecay_runtime_core::db::engine::{
    QueryExecutor, ReadSnapshot, TestConnection, Transaction, TransactionBehavior,
};

use super::*;
use crate::observation::ObservationCancellation;
use crate::runtime::git_correlation::ensure_git_correlation_schema;

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

    fn graph_runtime(
        &self,
    ) -> Result<&dyn GitEvidenceGraphRuntimePort, GitCorrelationError> {
        Err(GitCorrelationError::Unavailable(
            "failure receipt tests do not mount graph evidence".to_owned(),
        ))
    }
}

fn failure(activity_timestamp: i64) -> GitHistoryFailureRow {
    GitHistoryFailureRow {
        source_rowid: 7,
        activity_timestamp,
        provider: "codex".to_string(),
        session_id: "session-1".to_string(),
        project_path: "/repo".to_string(),
        window_start: 100,
        window_end: activity_timestamp,
        reason: GitHistoryFailureReason::UnsupportedSourceFraming,
        source_generation: None,
        reflog_digest: None,
    }
}

async fn stored_activity(conn: &TestConnection) -> Option<i64> {
    let mut rows = conn
        .query(
            "SELECT activity_timestamp FROM git_history_index_failures
              WHERE source_rowid = 7",
            (),
        )
        .await
        .unwrap();
    rows.next().await.unwrap().map(|row| row.get(0).unwrap())
}

#[tokio::test]
async fn receipt_upsert_never_regresses_or_downgrades_a_seal() {
    let directory = tempfile::tempdir().unwrap();
    let conn = TestConnection::open(&directory.path().join("sessions.db"));
    ensure_git_correlation_schema(&conn).await.unwrap();

    let mut sealed = failure(300);
    sealed.source_generation = Some("generation-300".to_string());
    sealed.reflog_digest = Some("digest-300".to_string());
    upsert_unresolved(&conn, &sealed).await.unwrap();
    upsert_unresolved(&conn, &failure(200)).await.unwrap();
    upsert_unresolved(&conn, &failure(300)).await.unwrap();

    assert_eq!(stored_activity(&conn).await, Some(300));
    let mut rows = conn
        .query(
            "SELECT source_generation, reflog_digest
               FROM git_history_index_failures
              WHERE source_rowid = 7",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>(0).unwrap(), "generation-300");
    assert_eq!(row.get::<String>(1).unwrap(), "digest-300");
}

#[tokio::test]
async fn stale_failure_cannot_resurrect_after_newer_success_frontier() {
    let directory = tempfile::tempdir().unwrap();
    let store = TestStore::open(&directory.path().join("sessions.db"));
    ensure_git_correlation_schema(&store.connection)
        .await
        .unwrap();
    let stale = failure(200);
    upsert_unresolved(&store.connection, &stale).await.unwrap();
    assert!(
        clear_unresolved(&store.connection, stale.source_rowid)
            .await
            .unwrap()
    );
    super::super::advance_history_frontier(
        &store.connection,
        GitHistoryIndexFrontier {
            activity_timestamp: 300,
            source_rowid: 7,
        },
    )
    .await
    .unwrap();

    let persisted = persist_unresolved(
        &store,
        &stale,
        None,
        &BoundedGitControl::new(ObservationCancellation::default(), Duration::from_secs(10)),
    )
    .await
    .unwrap();

    assert_eq!(persisted, None);
    assert_eq!(count_unresolved(&store.connection).await.unwrap(), 0);
}
