use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};

use super::{
    CommitSessionRecord, GitCorrelationError, GitCorrelationSessionStore, GitCorrelationWriteTxn,
    SpanObservation, publish_transcript_graph_evidence,
};

/// Maximum exact receipts replayed by one startup or host-admission pass.
pub const DEFAULT_GIT_EVIDENCE_PUBLICATION_REPLAY_LIMIT: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GitEvidencePublicationPayloadV1 {
    commit_records: Vec<CommitSessionRecord>,
    span_observations: Vec<SpanObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingGitEvidencePublicationV1 {
    receipt_id: String,
    publication_prefix: String,
    payload: GitEvidencePublicationPayloadV1,
    evidence_json: String,
}

/// Stages exact graph-publication material in the caller's raw transcript
/// transaction. Empty evidence creates no receipt.
#[hotpath::measure(
    label = "sessions.git_correlation.publication_outbox.enqueue",
    future = true
)]
pub async fn enqueue_git_evidence_publication(
    conn: &(impl Executor + ?Sized),
    publication_prefix: &str,
    commit_records: &[CommitSessionRecord],
    span_observations: &[SpanObservation],
) -> Result<Option<String>, GitCorrelationError> {
    if commit_records.is_empty() && span_observations.is_empty() {
        return Ok(None);
    }
    if publication_prefix.trim().is_empty() {
        return Err(GitCorrelationError::InvalidArgument(
            "Git evidence publication prefix must not be empty".to_owned(),
        ));
    }
    let payload = GitEvidencePublicationPayloadV1 {
        commit_records: commit_records.to_vec(),
        span_observations: span_observations.to_vec(),
    };
    let evidence_json = serde_json::to_string(&payload)?;
    let receipt_material = serde_json::to_vec(&(publication_prefix, &payload))?;
    let receipt_id = format!(
        "git-evidence-publication:{}",
        hex::encode(Sha256::digest(receipt_material))
    );
    conn.execute(
        "INSERT OR IGNORE INTO git_evidence_publication_outbox (
             receipt_id, publication_prefix, evidence_json
         ) VALUES (?1, ?2, ?3)",
        params![
            receipt_id.as_str(),
            publication_prefix,
            evidence_json.as_str()
        ],
    )
    .await?;
    let mut rows = conn
        .query(
            "SELECT publication_prefix, evidence_json
             FROM git_evidence_publication_outbox WHERE receipt_id = ?1",
            params![receipt_id.as_str()],
        )
        .await?;
    let row = rows.next().await?.ok_or_else(|| {
        GitCorrelationError::Db(
            "Git evidence publication receipt disappeared before commit".to_owned(),
        )
    })?;
    let stored_prefix = row.get::<String>(0)?;
    let stored_json = row.get::<String>(1)?;
    if stored_prefix != publication_prefix || stored_json != evidence_json {
        return Err(GitCorrelationError::Corrupt(
            "Git evidence publication receipt identity collision".to_owned(),
        ));
    }
    Ok(Some(receipt_id))
}

async fn read_pending_git_evidence_publications(
    conn: &(impl QueryExecutor + ?Sized),
    limit: usize,
) -> Result<Vec<PendingGitEvidencePublicationV1>, GitCorrelationError> {
    if limit == 0 {
        return Err(GitCorrelationError::InvalidArgument(
            "Git evidence publication replay limit must be positive".to_owned(),
        ));
    }
    let limit = i64::try_from(limit).map_err(|_| {
        GitCorrelationError::InvalidArgument(
            "Git evidence publication replay limit is too large".to_owned(),
        )
    })?;
    let mut rows = conn
        .query(
            "SELECT receipt_id, publication_prefix, evidence_json
             FROM git_evidence_publication_outbox
             ORDER BY created_at ASC, receipt_id ASC
             LIMIT ?1",
            params![limit],
        )
        .await?;
    let mut pending = Vec::new();
    while let Some(row) = rows.next().await? {
        let receipt_id = row.get::<String>(0)?;
        let publication_prefix = row.get::<String>(1)?;
        let evidence_json = row.get::<String>(2)?;
        let payload = serde_json::from_str(&evidence_json)?;
        pending.push(PendingGitEvidencePublicationV1 {
            receipt_id,
            publication_prefix,
            payload,
            evidence_json,
        });
    }
    Ok(pending)
}

/// Counts durable, not-yet-settled graph-publication receipts.
pub async fn pending_git_evidence_publication_count<S: GitCorrelationSessionStore>(
    session_store: &S,
) -> Result<u64, GitCorrelationError> {
    session_store.require_project_sessions_authority()?;
    let snapshot = session_store.read_snapshot().await?;
    let mut rows = snapshot
        .query(
            "SELECT COUNT(*) FROM git_evidence_publication_outbox",
            params![],
        )
        .await?;
    let count = rows
        .next()
        .await?
        .ok_or_else(|| GitCorrelationError::Db("Git evidence outbox count is absent".to_owned()))?
        .get::<i64>(0)?;
    u64::try_from(count).map_err(|_| {
        GitCorrelationError::Corrupt("Git evidence outbox count is negative".to_owned())
    })
}

/// Replays bounded exact receipts and deletes each receipt only after its
/// verified graph publication succeeds. A crash between those two commit
/// points safely republishes the content-addressed generation.
#[hotpath::measure(
    label = "sessions.git_correlation.publication_outbox.replay",
    future = true
)]
pub async fn replay_pending_git_evidence_publications<S: GitCorrelationSessionStore>(
    session_store: &S,
    limit: usize,
) -> Result<usize, GitCorrelationError> {
    session_store.require_project_sessions_authority()?;
    let snapshot = session_store.read_snapshot().await?;
    let pending = read_pending_git_evidence_publications(&snapshot, limit).await?;
    drop(snapshot);
    let mut replayed = 0_usize;
    for receipt in pending {
        publish_transcript_graph_evidence(
            session_store,
            &receipt.publication_prefix,
            &receipt.payload.span_observations,
            &receipt.payload.commit_records,
            super::DEFAULT_SPAN_MERGE_GAP_SECS,
        )?;
        let transaction = session_store.open_write_transaction().await?;
        let deleted = transaction
            .execute(
                "DELETE FROM git_evidence_publication_outbox
                 WHERE receipt_id = ?1 AND publication_prefix = ?2 AND evidence_json = ?3",
                params![
                    receipt.receipt_id.as_str(),
                    receipt.publication_prefix.as_str(),
                    receipt.evidence_json.as_str()
                ],
            )
            .await?;
        if deleted == 0 {
            let mut rows = transaction
                .query(
                    "SELECT publication_prefix, evidence_json
                     FROM git_evidence_publication_outbox WHERE receipt_id = ?1",
                    params![receipt.receipt_id.as_str()],
                )
                .await?;
            if let Some(row) = rows.next().await? {
                let stored_prefix = row.get::<String>(0)?;
                let stored_json = row.get::<String>(1)?;
                return Err(GitCorrelationError::Corrupt(format!(
                    "Git evidence publication receipt changed before settlement: prefix_match={}, payload_match={}",
                    stored_prefix == receipt.publication_prefix,
                    stored_json == receipt.evidence_json,
                )));
            }
        } else if deleted != 1 {
            return Err(GitCorrelationError::Corrupt(
                "Git evidence publication receipt settlement deleted multiple rows".to_owned(),
            ));
        }
        GitCorrelationWriteTxn::commit(transaction).await?;
        if deleted == 1 {
            replayed = replayed.saturating_add(1);
        }
    }
    Ok(replayed)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use tracedecay_runtime_core::db::engine::{
        Executor, QueryExecutor, ReadSnapshot, TestConnection, Transaction, TransactionBehavior,
        params,
    };
    use tracedecay_runtime_core::store_runtime::VerifiedGraphRuntimePortV1;

    use super::{
        enqueue_git_evidence_publication, pending_git_evidence_publication_count,
        replay_pending_git_evidence_publications,
    };
    use crate::runtime::git_correlation::test_support::MemoryEvidenceGraphRuntime;
    use crate::runtime::git_correlation::{
        CommitEvidence, CommitRelation, CommitSessionRecord, GitCorrelationError,
        GitCorrelationSessionStore, SpanObservation, SpanOverlapKind, SpanSource,
        ensure_git_correlation_receipt_schema_in_transaction,
    };

    struct TestStore {
        connection: TestConnection,
        graph: Arc<MemoryEvidenceGraphRuntime>,
    }

    impl TestStore {
        fn open(path: &Path, graph: Arc<MemoryEvidenceGraphRuntime>) -> Self {
            Self {
                connection: TestConnection::open(path),
                graph,
            }
        }
    }

    impl GitCorrelationSessionStore for TestStore {
        type ReadSnapshot = ReadSnapshot;
        type WriteTxn<'txn> = Transaction;

        fn require_project_sessions_authority(&self) -> Result<(), GitCorrelationError> {
            Ok(())
        }

        async fn read_snapshot(&self) -> Result<Self::ReadSnapshot, GitCorrelationError> {
            self.connection
                .read_snapshot()
                .await
                .map_err(GitCorrelationError::from)
        }

        async fn open_write_transaction(&self) -> Result<Self::WriteTxn<'_>, GitCorrelationError> {
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

    fn span() -> SpanObservation {
        SpanObservation {
            provider: "codex".to_owned(),
            session_id: "session-outbox".to_owned(),
            thread_id: Some("thread-outbox".to_owned()),
            branch: Some("main".to_owned()),
            worktree: "/repo".to_owned(),
            ts: 100,
            source: SpanSource::Ingest,
        }
    }

    fn commit() -> CommitSessionRecord {
        CommitSessionRecord {
            commit_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            provider: "codex".to_owned(),
            session_id: "session-outbox".to_owned(),
            branch: Some("main".to_owned()),
            worktree: Some("/repo".to_owned()),
            committed_at: 100,
            span_overlap_kind: SpanOverlapKind::Direct,
            span_id: None,
            relation: CommitRelation::Produced,
            evidence: CommitEvidence::ToolResult,
            confidence: 100,
            evidence_message_id: Some("message-outbox".to_owned()),
        }
    }

    async fn count_rows(store: &TestStore, table: &str) -> i64 {
        let mut rows = store
            .connection
            .query(&format!("SELECT COUNT(*) FROM {table}"), ())
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get(0).unwrap()
    }

    #[tokio::test]
    async fn failed_post_commit_publication_replays_once_after_restart() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("sessions.db");
        let graph = Arc::new(MemoryEvidenceGraphRuntime::default());
        let store = TestStore::open(&database, Arc::clone(&graph));
        ensure_git_correlation_receipt_schema_in_transaction(&store.connection)
            .await
            .unwrap();
        store
            .connection
            .execute_batch("CREATE TABLE committed_transcript_rows (id TEXT PRIMARY KEY);")
            .await
            .unwrap();

        let transaction = store.open_write_transaction().await.unwrap();
        transaction
            .execute(
                "INSERT INTO committed_transcript_rows(id) VALUES (?1)",
                params!["raw-message-outbox"],
            )
            .await
            .unwrap();
        let receipt = enqueue_git_evidence_publication(
            &transaction,
            "transcript-git-evidence",
            &[commit()],
            &[span()],
        )
        .await
        .unwrap()
        .expect("non-empty evidence creates a durable receipt");
        transaction.commit().await.unwrap();

        assert_eq!(count_rows(&store, "committed_transcript_rows").await, 1);
        assert_eq!(
            pending_git_evidence_publication_count(&store)
                .await
                .unwrap(),
            1
        );
        graph.fail_next_publication();
        assert!(
            replay_pending_git_evidence_publications(&store, 8)
                .await
                .is_err()
        );
        assert_eq!(
            pending_git_evidence_publication_count(&store)
                .await
                .unwrap(),
            1
        );
        assert_eq!(graph.successful_publications(), 0);
        drop(store);

        let restarted = TestStore::open(&database, Arc::clone(&graph));
        assert_eq!(
            replay_pending_git_evidence_publications(&restarted, 8)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            pending_git_evidence_publication_count(&restarted)
                .await
                .unwrap(),
            0
        );
        assert_eq!(graph.successful_publications(), 1);
        assert_eq!(
            replay_pending_git_evidence_publications(&restarted, 8)
                .await
                .unwrap(),
            0
        );
        assert_eq!(graph.successful_publications(), 1);

        let projection = crate::runtime::git_correlation::recover_git_evidence_projection(
            graph.as_ref(),
            &crate::runtime::git_correlation::git_evidence_projection_identity(
                tracedecay_graph_db::GraphNamespace::new("project").unwrap(),
            )
            .unwrap(),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .unwrap()
        .expect("replayed projection");
        assert_eq!(projection.projection().spans().len(), 1);
        assert_eq!(projection.projection().commit_sessions().len(), 1);
        assert!(!receipt.is_empty());
    }
}
