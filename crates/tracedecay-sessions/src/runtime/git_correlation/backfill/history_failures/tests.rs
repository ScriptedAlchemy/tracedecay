use std::path::Path;
use std::time::Duration;

use tracedecay_runtime_core::db::engine::{
    QueryExecutor, ReadSnapshot, TestConnection, Transaction, TransactionBehavior,
};

use super::*;
use crate::observation::ObservationCancellation;
use crate::runtime::git_correlation::ensure_git_correlation_receipt_schema_in_transaction;

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
        self.connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(GitCorrelationError::from)
    }
}

fn failure(change_sequence: i64) -> GitHistoryFailureRow {
    GitHistoryFailureRow {
        source_rowid: 7,
        change_sequence,
        provider: "codex".to_string(),
        session_id: "session-1".to_string(),
        project_path: "/repo".to_string(),
        window_start: 100,
        window_end: change_sequence,
        reason: GitHistoryFailureReason::UnsupportedSourceFraming,
        source_generation: None,
        reflog_digest: None,
    }
}

async fn stored_activity(conn: &TestConnection) -> Option<i64> {
    let mut rows = conn
        .query(
            "SELECT change_sequence FROM git_history_index_failures
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
    ensure_git_correlation_receipt_schema_in_transaction(&conn)
        .await
        .unwrap();

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
    ensure_git_correlation_receipt_schema_in_transaction(&store.connection)
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
            change_sequence: 300,
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

/// A store written by the activity-time pass converges to a full rescan on
/// the change-sequence axis, and reopening it changes nothing further.
#[tokio::test]
async fn activity_time_positions_migrate_to_a_full_rescan_once() {
    let directory = tempfile::tempdir().unwrap();
    let conn = TestConnection::open(&directory.path().join("sessions.db"));
    ensure_git_correlation_receipt_schema_in_transaction(&conn)
        .await
        .unwrap();
    conn.execute_batch(
        "ALTER TABLE git_history_index_failures
             RENAME COLUMN change_sequence TO activity_timestamp;
         ALTER TABLE git_history_index_progress
             RENAME COLUMN change_sequence TO activity_timestamp;
         INSERT INTO git_history_index_failures (
             source_rowid, activity_timestamp, provider, session_id, project_path,
             window_start, window_end, reason
         ) VALUES (7, 1700000000, 'codex', 'session-1', '/repo', 100, 1700000000,
                   'unsupported_source_framing');
         INSERT INTO git_correlation_meta(key, value) VALUES
             ('auto_backfill_activity_watermark', 1700000000),
             ('git_history_session_rowid_frontier', 7);",
    )
    .await
    .unwrap();

    for _ in 0..2 {
        ensure_git_correlation_receipt_schema_in_transaction(&conn)
            .await
            .unwrap();
        assert_eq!(stored_activity(&conn).await, Some(0));
        assert_eq!(
            crate::runtime::git_correlation::read_history_frontier(&conn)
                .await
                .unwrap(),
            GitHistoryIndexFrontier {
                change_sequence: 0,
                source_rowid: 0,
            }
        );
        let mut legacy = conn
            .query(
                "SELECT COUNT(*) FROM git_correlation_meta
                  WHERE key = 'auto_backfill_activity_watermark'",
                (),
            )
            .await
            .unwrap();
        assert_eq!(
            legacy.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            0
        );
    }
}
