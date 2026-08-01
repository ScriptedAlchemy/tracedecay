use std::collections::BTreeSet;

use tracedecay_domain::{
    GitIndexIdempotencyKey, GitIndexJournalPhaseV1, GitIndexPreviewId, GitIndexPreviewV1,
    GitIndexReceiptOutcomeV1, GitIndexTransactionId, GitIndexTransactionJournalV1,
    GitIndexTransactionReceiptV1, RepositoryId,
};
use tracedecay_store::{
    GitIndexTransactionBeginRequestV1, GitIndexTransactionBeginResultV1,
    GitIndexTransactionRecordV1, GitIndexTransactionStoreError, GitIndexTransactionStoreResult,
    GitIndexTransactionTerminalWriteV1,
};

use crate::{RegisteredGlobalDb, registered::RegisteredGlobalDbWriteTransaction};
#[cfg(any(test, feature = "test-helpers"))]
use tracedecay_runtime_core::db::engine::{Connection, Transaction, TransactionBehavior};
use tracedecay_runtime_core::db::engine::{
    Executor, IntoParams, QueryExecutor, ReadSnapshot, Row, Rows, params,
};

/// Async canonical-store adapter for PR11 transaction state.
///
/// The adapter borrows the already-mounted registered session database; it never
/// opens a database or derives a path. Every mutation owns one `IMMEDIATE`
/// transaction from that runtime through commit or rollback.
pub struct GlobalDbGitIndexTransactionStore<'db> {
    db: GitIndexDatabase<'db>,
}

#[derive(Clone, Copy)]
enum GitIndexDatabase<'db> {
    Registered(&'db RegisteredGlobalDb),
    #[cfg(any(test, feature = "test-helpers"))]
    Engine(&'db Connection),
}

enum GitIndexWriteTransaction<'db> {
    Registered(RegisteredGlobalDbWriteTransaction<'db>),
    #[cfg(any(test, feature = "test-helpers"))]
    Engine(Transaction),
}

impl QueryExecutor for GitIndexWriteTransaction<'_> {
    async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<Rows>
    where
        P: IntoParams,
    {
        match self {
            Self::Registered(transaction) => transaction.query(sql, params).await,
            #[cfg(any(test, feature = "test-helpers"))]
            Self::Engine(transaction) => transaction.query(sql, params).await,
        }
    }
}

impl Executor for GitIndexWriteTransaction<'_> {
    async fn execute<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<u64>
    where
        P: IntoParams,
    {
        match self {
            Self::Registered(transaction) => transaction.execute(sql, params).await,
            #[cfg(any(test, feature = "test-helpers"))]
            Self::Engine(transaction) => transaction.execute(sql, params).await,
        }
    }

    async fn execute_batch(&self, sql: &str) -> tracedecay_runtime_core::db::engine::Result<()> {
        match self {
            Self::Registered(transaction) => transaction.execute_batch(sql).await,
            #[cfg(any(test, feature = "test-helpers"))]
            Self::Engine(transaction) => transaction.execute_batch(sql).await,
        }
    }
}

impl GitIndexWriteTransaction<'_> {
    async fn commit(self) -> tracedecay_runtime_core::db::engine::Result<()> {
        match self {
            Self::Registered(transaction) => transaction.commit().await,
            #[cfg(any(test, feature = "test-helpers"))]
            Self::Engine(transaction) => transaction.commit().await,
        }
    }

    async fn rollback(self) -> tracedecay_runtime_core::db::engine::Result<()> {
        match self {
            Self::Registered(transaction) => transaction.rollback().await,
            #[cfg(any(test, feature = "test-helpers"))]
            Self::Engine(transaction) => transaction.rollback().await,
        }
    }
}

impl<'db> GlobalDbGitIndexTransactionStore<'db> {
    pub const fn new(db: &'db RegisteredGlobalDb) -> Self {
        Self {
            db: GitIndexDatabase::Registered(db),
        }
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub const fn for_engine_test(db: &'db Connection) -> Self {
        Self {
            db: GitIndexDatabase::Engine(db),
        }
    }

    pub async fn save_preview(
        &self,
        preview: GitIndexPreviewV1,
    ) -> GitIndexTransactionStoreResult<()> {
        preview.validate().map_err(invalid_domain)?;
        let transaction = self.begin_write().await?;
        let outcome = insert_preview_if_absent(&transaction, &preview).await;
        commit_outcome(transaction, outcome).await
    }

    pub async fn read_preview(
        &self,
        preview_id: &GitIndexPreviewId,
    ) -> GitIndexTransactionStoreResult<Option<GitIndexPreviewV1>> {
        preview_id.validate().map_err(invalid_domain)?;
        let snapshot = self.read_snapshot().await?;
        read_preview_from_transaction(&snapshot, preview_id).await
    }

    /// Reads the durable transaction record bound to an application idempotency
    /// key without opening a writer. This is the read-only projection of the
    /// same record `begin_or_replay` reconstructs before it decides to start,
    /// replay, or require recovery.
    pub async fn read_record(
        &self,
        idempotency_key: &GitIndexIdempotencyKey,
    ) -> GitIndexTransactionStoreResult<Option<GitIndexTransactionRecordV1>> {
        idempotency_key.validate().map_err(invalid_domain)?;
        let snapshot = self.read_snapshot().await?;
        read_record_from_transaction(&snapshot, idempotency_key).await
    }

    /// Atomically binds a client input and its prepared journal to an immutable
    /// preview before native Git is permitted to run.
    pub async fn begin_or_replay(
        &self,
        request: GitIndexTransactionBeginRequestV1,
    ) -> GitIndexTransactionStoreResult<GitIndexTransactionBeginResultV1> {
        request.validate().map_err(invalid_domain)?;
        let transaction = self.begin_write().await?;
        let outcome = async {
            if let Some(existing) =
                read_record_from_transaction(&transaction, &request.idempotency_key).await?
            {
                if existing.input_digest != request.input_digest
                    || existing.preview != request.preview
                    || existing.journal.transaction_id != request.journal.transaction_id
                {
                    return Err(GitIndexTransactionStoreError::IdempotencyConflict);
                }
                return Ok(match existing.terminal_receipt {
                    Some(receipt) => GitIndexTransactionBeginResultV1::Replay(Box::new(receipt)),
                    None => GitIndexTransactionBeginResultV1::RecoveryRequired(Box::new(existing)),
                });
            }

            let repository_id = &request.preview.repository_snapshot.repository_id;
            if repository_has_active_quarantine(&transaction, repository_id).await? {
                return Err(GitIndexTransactionStoreError::RepositoryQuarantined);
            }
            if transaction_id_exists(&transaction, &request.journal.transaction_id).await? {
                return Err(GitIndexTransactionStoreError::IdempotencyConflict);
            }

            insert_preview_if_absent(&transaction, &request.preview).await?;
            transaction
                .execute(
                    "INSERT INTO git_index_transaction_inputs
                        (idempotency_key, input_digest, transaction_id, preview_id,
                         preview_digest, repository_id, worktree_id, operation, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        request.idempotency_key.as_str(),
                        request.input_digest.as_str(),
                        request.journal.transaction_id.as_str(),
                        request.preview.preview_id.as_str(),
                        request.preview.preview_digest.as_str(),
                        request.journal.repository_id.as_str(),
                        request.journal.worktree_id.as_str(),
                        operation_code(request.journal.operation),
                        request.journal.started_at.0,
                    ],
                )
                .await
                .map_err(unavailable)?;
            insert_journal(&transaction, &request.idempotency_key, &request.journal).await?;

            let record = GitIndexTransactionRecordV1 {
                idempotency_key: request.idempotency_key,
                input_digest: request.input_digest,
                preview: request.preview,
                journal: request.journal,
                terminal_receipt: None,
            };
            record.validate().map_err(invalid_domain)?;
            Ok(GitIndexTransactionBeginResultV1::Started(Box::new(record)))
        }
        .await;
        commit_outcome(transaction, outcome).await
    }

    pub async fn compare_and_swap_journal(
        &self,
        idempotency_key: &GitIndexIdempotencyKey,
        expected_phase_epoch: u64,
        replacement: GitIndexTransactionJournalV1,
    ) -> GitIndexTransactionStoreResult<GitIndexTransactionJournalV1> {
        idempotency_key.validate().map_err(invalid_domain)?;
        replacement.validate().map_err(invalid_domain)?;
        if replacement.phase.is_terminal() {
            return Err(GitIndexTransactionStoreError::JournalConflict);
        }

        let transaction = self.begin_write().await?;
        let outcome = async {
            let record = read_record_from_transaction(&transaction, idempotency_key)
                .await?
                .ok_or(GitIndexTransactionStoreError::JournalConflict)?;
            if record.terminal_receipt.is_some()
                || !journal_transition_matches(&record.journal, expected_phase_epoch, &replacement)
            {
                return Err(GitIndexTransactionStoreError::JournalConflict);
            }

            let updated = transaction
                .execute(
                    "UPDATE git_index_transaction_journals
                     SET phase = ?1, phase_epoch = ?2, updated_at = ?3, journal_json = ?4
                     WHERE transaction_id = ?5 AND phase_epoch = ?6",
                    params![
                        phase_code(replacement.phase),
                        phase_epoch_i64(replacement.phase_epoch)?,
                        replacement.updated_at.0,
                        encode(&replacement)?,
                        replacement.transaction_id.as_str(),
                        phase_epoch_i64(expected_phase_epoch)?,
                    ],
                )
                .await
                .map_err(unavailable)?;
            if updated != 1 {
                return Err(GitIndexTransactionStoreError::JournalConflict);
            }
            Ok(replacement)
        }
        .await;
        commit_outcome(transaction, outcome).await
    }

    /// Publishes the terminal journal transition and receipt in one immediate
    /// database transaction. A failed receipt insert rolls back the journal
    /// phase and any newly required quarantine, so restart recovery never
    /// observes a terminal phase without its immutable receipt or fence.
    pub async fn write_terminal(
        &self,
        write: GitIndexTransactionTerminalWriteV1,
    ) -> GitIndexTransactionStoreResult<GitIndexTransactionReceiptV1> {
        write.validate().map_err(invalid_domain)?;
        let transaction = self.begin_write().await?;
        let outcome = async {
            let record = read_record_from_transaction(&transaction, &write.idempotency_key)
                .await?
                .ok_or(GitIndexTransactionStoreError::ReceiptConflict)?;
            if let Some(existing) = record.terminal_receipt {
                return if existing == write.receipt {
                    Ok(existing)
                } else {
                    Err(GitIndexTransactionStoreError::ReceiptConflict)
                };
            }
            if write.receipt.outcome == GitIndexReceiptOutcomeV1::NeedsInspection {
                ensure_active_quarantine(&transaction, &write.journal).await?;
            } else if transaction_has_active_quarantine(
                &transaction,
                &write.journal.repository_id,
                &write.journal.transaction_id,
            )
            .await?
            {
                if !write.receipt.final_snapshot_captured {
                    return Err(GitIndexTransactionStoreError::RepositoryQuarantined);
                }
                resolve_active_quarantine(&transaction, &write.journal, &write.receipt).await?;
            }
            if !journal_transition_matches(
                &record.journal,
                record.journal.phase_epoch,
                &write.journal,
            ) || write.expected_phase_epoch != write.journal.phase_epoch
            {
                return Err(GitIndexTransactionStoreError::JournalConflict);
            }

            let terminal_record = GitIndexTransactionRecordV1 {
                journal: write.journal.clone(),
                terminal_receipt: Some(write.receipt.clone()),
                ..record
            };
            terminal_record.validate().map_err(invalid_domain)?;

            let updated = transaction
                .execute(
                    "UPDATE git_index_transaction_journals
                     SET phase = ?1, phase_epoch = ?2, updated_at = ?3, journal_json = ?4
                     WHERE transaction_id = ?5 AND phase_epoch = ?6",
                    params![
                        phase_code(write.journal.phase),
                        phase_epoch_i64(write.journal.phase_epoch)?,
                        write.journal.updated_at.0,
                        encode(&write.journal)?,
                        write.journal.transaction_id.as_str(),
                        phase_epoch_i64(record.journal.phase_epoch)?,
                    ],
                )
                .await
                .map_err(unavailable)?;
            if updated != 1 {
                return Err(GitIndexTransactionStoreError::JournalConflict);
            }
            transaction
                .execute(
                    "INSERT INTO git_index_transaction_receipts
                        (transaction_id, receipt_id, preview_id, receipt_digest, outcome,
                         committed_at, receipt_json)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        write.receipt.transaction_id.as_str(),
                        write.receipt.receipt_id.as_str(),
                        write.receipt.preview_id.as_str(),
                        write.receipt.receipt_digest.as_str(),
                        receipt_outcome_code(write.receipt.outcome),
                        write.receipt.committed_at.0,
                        encode(&write.receipt)?,
                    ],
                )
                .await
                .map_err(unavailable)?;
            Ok(write.receipt)
        }
        .await;
        commit_outcome(transaction, outcome).await
    }

    pub async fn recovery_candidates(
        &self,
        repository_id: &RepositoryId,
    ) -> GitIndexTransactionStoreResult<Vec<GitIndexTransactionRecordV1>> {
        repository_id.validate().map_err(invalid_domain)?;
        let snapshot = self.read_snapshot().await?;
        let inspection_recovery =
            needs_inspection_recovery_transactions(&snapshot, repository_id).await?;
        let records = records_for_repository(&snapshot, repository_id).await?;
        Ok(records
            .into_iter()
            .filter(|record| {
                record.journal.requires_recovery()
                    || inspection_recovery.contains(&record.journal.transaction_id)
            })
            .collect())
    }

    pub async fn recovery_repositories(&self) -> GitIndexTransactionStoreResult<Vec<RepositoryId>> {
        let snapshot = self.read_snapshot().await?;
        let mut rows = snapshot
            .query(
                "SELECT repository_id
                 FROM git_index_transaction_journals
                 WHERE phase NOT IN ('committed', 'aborted_no_change', 'needs_inspection')
                 UNION
                 SELECT journal.repository_id
                 FROM git_index_transaction_journals AS journal
                 JOIN git_index_transaction_receipts AS receipt
                   ON receipt.transaction_id = journal.transaction_id
                 LEFT JOIN git_index_repository_quarantines AS quarantine
                   ON quarantine.repository_id = journal.repository_id
                  AND quarantine.transaction_id = journal.transaction_id
                 WHERE receipt.outcome = 'needs_inspection'
                   AND (quarantine.active = 1 OR quarantine.transaction_id IS NULL)
                 UNION
                 SELECT repository_id
                 FROM git_index_repository_quarantines
                 WHERE active = 1
                 ORDER BY repository_id",
                (),
            )
            .await
            .map_err(unavailable)?;
        let mut repositories = Vec::new();
        while let Some(row) = rows.next().await.map_err(unavailable)? {
            let repository_id = RepositoryId::new(text(&row, 0, "recovery repository id")?)
                .map_err(invalid_domain)?;
            repositories.push(repository_id);
        }
        Ok(repositories)
    }

    pub async fn quarantine_repository(
        &self,
        repository_id: &RepositoryId,
        transaction_id: &GitIndexTransactionId,
    ) -> GitIndexTransactionStoreResult<()> {
        repository_id.validate().map_err(invalid_domain)?;
        transaction_id.validate().map_err(invalid_domain)?;
        let transaction = self.begin_write().await?;
        let outcome =
            async {
                let record = record_by_transaction_id(&transaction, transaction_id)
                    .await?
                    .ok_or(GitIndexTransactionStoreError::JournalConflict)?;
                if record.journal.repository_id != *repository_id {
                    return Err(GitIndexTransactionStoreError::JournalConflict);
                }
                if record.terminal_receipt.as_ref().is_some_and(|receipt| {
                    receipt.outcome != GitIndexReceiptOutcomeV1::NeedsInspection
                }) {
                    return Err(GitIndexTransactionStoreError::ReceiptConflict);
                }
                ensure_active_quarantine(&transaction, &record.journal).await
            }
            .await;
        commit_outcome(transaction, outcome).await
    }

    pub async fn clear_repository_quarantine(
        &self,
        repository_id: &RepositoryId,
        transaction_id: &GitIndexTransactionId,
        recovery_receipt: GitIndexTransactionReceiptV1,
    ) -> GitIndexTransactionStoreResult<()> {
        repository_id.validate().map_err(invalid_domain)?;
        transaction_id.validate().map_err(invalid_domain)?;
        recovery_receipt.validate().map_err(invalid_domain)?;
        if recovery_receipt.outcome != GitIndexReceiptOutcomeV1::AbortedNoChange {
            return Err(GitIndexTransactionStoreError::ReceiptConflict);
        }

        let transaction = self.begin_write().await?;
        let outcome = async {
            let record = record_by_transaction_id(&transaction, transaction_id)
                .await?
                .ok_or(GitIndexTransactionStoreError::ReceiptConflict)?;
            if record.journal.repository_id != *repository_id {
                return Err(GitIndexTransactionStoreError::ReceiptConflict);
            }
            let Some(inspection) = record.terminal_receipt.as_ref() else {
                return Err(GitIndexTransactionStoreError::ReceiptConflict);
            };
            if inspection.outcome != GitIndexReceiptOutcomeV1::NeedsInspection
                || !recovery_receipt.final_snapshot_captured
                || recovery_receipt.committed_at <= inspection.committed_at
                || !record.receipt_binds_preview(&recovery_receipt)
            {
                return Err(GitIndexTransactionStoreError::ReceiptConflict);
            }
            let updated = transaction
                .execute(
                    "UPDATE git_index_repository_quarantines
                     SET active = 0, resolved_at = ?1, resolution_receipt_json = ?2
                     WHERE repository_id = ?3 AND transaction_id = ?4 AND active = 1",
                    params![
                        recovery_receipt.committed_at.0,
                        encode(&recovery_receipt)?,
                        repository_id.as_str(),
                        transaction_id.as_str(),
                    ],
                )
                .await
                .map_err(unavailable)?;
            if updated != 1 {
                return Err(GitIndexTransactionStoreError::RepositoryQuarantined);
            }
            Ok(())
        }
        .await;
        commit_outcome(transaction, outcome).await
    }

    async fn begin_write(&self) -> GitIndexTransactionStoreResult<GitIndexWriteTransaction<'_>> {
        match self.db {
            GitIndexDatabase::Registered(db) => db
                .begin_write_transaction()
                .await
                .map(GitIndexWriteTransaction::Registered)
                .map_err(unavailable),
            #[cfg(any(test, feature = "test-helpers"))]
            GitIndexDatabase::Engine(db) => db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .await
                .map(GitIndexWriteTransaction::Engine)
                .map_err(unavailable),
        }
    }

    async fn read_snapshot(&self) -> GitIndexTransactionStoreResult<ReadSnapshot> {
        match self.db {
            GitIndexDatabase::Registered(db) => db.read_snapshot().await.map_err(unavailable),
            #[cfg(any(test, feature = "test-helpers"))]
            GitIndexDatabase::Engine(db) => db.read_snapshot().await.map_err(unavailable),
        }
    }
}

async fn commit_outcome<T>(
    transaction: GitIndexWriteTransaction<'_>,
    outcome: GitIndexTransactionStoreResult<T>,
) -> GitIndexTransactionStoreResult<T> {
    match outcome {
        Ok(value) => transaction
            .commit()
            .await
            .map(|()| value)
            .map_err(unavailable),
        Err(error) => match transaction.rollback().await {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(unavailable(rollback_error)),
        },
    }
}

async fn insert_preview_if_absent<E>(
    transaction: &E,
    preview: &GitIndexPreviewV1,
) -> GitIndexTransactionStoreResult<()>
where
    E: Executor,
{
    if let Some(existing) = read_preview_from_transaction(transaction, &preview.preview_id).await? {
        return if existing == *preview {
            Ok(())
        } else {
            Err(GitIndexTransactionStoreError::PreviewConflict)
        };
    }
    transaction
        .execute(
            "INSERT INTO git_index_preview_commitments
                (preview_id, preview_digest, repository_id, worktree_id, operation,
                 repository_snapshot_digest, commit_intent_digest, created_at, expires_at,
                 preview_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                preview.preview_id.as_str(),
                preview.preview_digest.as_str(),
                preview.repository_snapshot.repository_id.as_str(),
                preview
                    .repository_snapshot
                    .worktree_id
                    .as_ref()
                    .map(tracedecay_domain::WorktreeId::as_str),
                operation_code(preview.operation),
                preview.repository_snapshot_digest.as_str(),
                preview
                    .commit_intent_digest
                    .as_ref()
                    .map(tracedecay_domain::ManifestDigest::as_str),
                preview.created_at.0,
                preview.expires_at.0,
                encode(preview)?,
            ],
        )
        .await
        .map(|_| ())
        .map_err(unavailable)
}

async fn insert_journal<E>(
    transaction: &E,
    idempotency_key: &GitIndexIdempotencyKey,
    journal: &GitIndexTransactionJournalV1,
) -> GitIndexTransactionStoreResult<()>
where
    E: Executor,
{
    transaction
        .execute(
            "INSERT INTO git_index_transaction_journals
                (transaction_id, idempotency_key, preview_id, preview_digest, repository_id,
                 worktree_id, operation, expected_snapshot_digest, phase, phase_epoch,
                 started_at, updated_at, journal_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                journal.transaction_id.as_str(),
                idempotency_key.as_str(),
                journal.preview_id.as_str(),
                journal.preview_digest.as_str(),
                journal.repository_id.as_str(),
                journal.worktree_id.as_str(),
                operation_code(journal.operation),
                journal.expected_snapshot_digest.as_str(),
                phase_code(journal.phase),
                phase_epoch_i64(journal.phase_epoch)?,
                journal.started_at.0,
                journal.updated_at.0,
                encode(journal)?,
            ],
        )
        .await
        .map(|_| ())
        .map_err(unavailable)
}

async fn read_preview_from_transaction<Q>(
    transaction: &Q,
    preview_id: &GitIndexPreviewId,
) -> GitIndexTransactionStoreResult<Option<GitIndexPreviewV1>>
where
    Q: QueryExecutor,
{
    let mut rows = transaction
        .query(
            "SELECT preview_json FROM git_index_preview_commitments WHERE preview_id = ?1",
            params![preview_id.as_str()],
        )
        .await
        .map_err(unavailable)?;
    let Some(row) = rows.next().await.map_err(unavailable)? else {
        return Ok(None);
    };
    let preview: GitIndexPreviewV1 = decode(&text(&row, 0, "preview commitment")?)?;
    if preview.preview_id != *preview_id {
        return Err(invalid(
            "git index preview commitment key does not bind its payload",
        ));
    }
    preview.validate().map_err(invalid_domain)?;
    if rows.next().await.map_err(unavailable)?.is_some() {
        return Err(invalid("duplicate git index preview commitment"));
    }
    Ok(Some(preview))
}

async fn read_record_from_transaction<Q>(
    transaction: &Q,
    idempotency_key: &GitIndexIdempotencyKey,
) -> GitIndexTransactionStoreResult<Option<GitIndexTransactionRecordV1>>
where
    Q: QueryExecutor,
{
    let mut rows = transaction
        .query(
            "SELECT input.idempotency_key, input.input_digest, preview.preview_json,
                    journal.journal_json, receipt.receipt_json
             FROM git_index_transaction_inputs AS input
             JOIN git_index_preview_commitments AS preview
               ON preview.preview_id = input.preview_id
             JOIN git_index_transaction_journals AS journal
               ON journal.transaction_id = input.transaction_id
              AND journal.idempotency_key = input.idempotency_key
             LEFT JOIN git_index_transaction_receipts AS receipt
               ON receipt.transaction_id = journal.transaction_id
             WHERE input.idempotency_key = ?1",
            params![idempotency_key.as_str()],
        )
        .await
        .map_err(unavailable)?;
    let Some(row) = rows.next().await.map_err(unavailable)? else {
        return Ok(None);
    };
    let record = decode_record(&row)?;
    if record.idempotency_key != *idempotency_key {
        return Err(invalid(
            "git index transaction input key does not bind its payload",
        ));
    }
    if rows.next().await.map_err(unavailable)?.is_some() {
        return Err(invalid("duplicate git index transaction input"));
    }
    Ok(Some(record))
}

async fn record_by_transaction_id<Q>(
    transaction: &Q,
    transaction_id: &GitIndexTransactionId,
) -> GitIndexTransactionStoreResult<Option<GitIndexTransactionRecordV1>>
where
    Q: QueryExecutor,
{
    let mut rows = transaction
        .query(
            "SELECT input.idempotency_key, input.input_digest, preview.preview_json,
                    journal.journal_json, receipt.receipt_json
             FROM git_index_transaction_inputs AS input
             JOIN git_index_preview_commitments AS preview
               ON preview.preview_id = input.preview_id
             JOIN git_index_transaction_journals AS journal
               ON journal.transaction_id = input.transaction_id
              AND journal.idempotency_key = input.idempotency_key
             LEFT JOIN git_index_transaction_receipts AS receipt
               ON receipt.transaction_id = journal.transaction_id
             WHERE input.transaction_id = ?1",
            params![transaction_id.as_str()],
        )
        .await
        .map_err(unavailable)?;
    let Some(row) = rows.next().await.map_err(unavailable)? else {
        return Ok(None);
    };
    let record = decode_record(&row)?;
    if record.journal.transaction_id != *transaction_id {
        return Err(invalid(
            "git index transaction journal key does not bind its payload",
        ));
    }
    if rows.next().await.map_err(unavailable)?.is_some() {
        return Err(invalid("duplicate git index transaction journal"));
    }
    Ok(Some(record))
}

async fn records_for_repository<Q>(
    transaction: &Q,
    repository_id: &RepositoryId,
) -> GitIndexTransactionStoreResult<Vec<GitIndexTransactionRecordV1>>
where
    Q: QueryExecutor,
{
    let mut rows = transaction
        .query(
            "SELECT input.idempotency_key, input.input_digest, preview.preview_json,
                    journal.journal_json, receipt.receipt_json
             FROM git_index_transaction_inputs AS input
             JOIN git_index_preview_commitments AS preview
               ON preview.preview_id = input.preview_id
             JOIN git_index_transaction_journals AS journal
               ON journal.transaction_id = input.transaction_id
              AND journal.idempotency_key = input.idempotency_key
             LEFT JOIN git_index_transaction_receipts AS receipt
               ON receipt.transaction_id = journal.transaction_id
             WHERE journal.repository_id = ?1
             ORDER BY journal.updated_at, journal.transaction_id",
            params![repository_id.as_str()],
        )
        .await
        .map_err(unavailable)?;
    let mut records = Vec::new();
    while let Some(row) = rows.next().await.map_err(unavailable)? {
        let record = decode_record(&row)?;
        if record.journal.repository_id != *repository_id {
            return Err(invalid(
                "git index transaction repository index is inconsistent",
            ));
        }
        records.push(record);
    }
    Ok(records)
}

fn decode_record(row: &Row) -> GitIndexTransactionStoreResult<GitIndexTransactionRecordV1> {
    let stored_idempotency_key = text(row, 0, "transaction idempotency key")?;
    let stored_input_digest = text(row, 1, "transaction input digest")?;
    let preview: GitIndexPreviewV1 = decode(&text(row, 2, "transaction preview")?)?;
    let journal: GitIndexTransactionJournalV1 = decode(&text(row, 3, "transaction journal")?)?;
    let terminal_receipt = optional_text(row, 4, "transaction receipt")?
        .map(|value| decode(&value))
        .transpose()?;
    let record = GitIndexTransactionRecordV1 {
        idempotency_key: GitIndexIdempotencyKey::new(stored_idempotency_key.clone())
            .map_err(invalid_domain)?,
        input_digest: tracedecay_domain::ManifestDigest::new(stored_input_digest.clone())
            .map_err(invalid_domain)?,
        preview,
        journal,
        terminal_receipt,
    };
    if record.idempotency_key.as_str() != stored_idempotency_key
        || record.input_digest.as_str() != stored_input_digest
    {
        return Err(invalid(
            "git index transaction scalar bindings are inconsistent",
        ));
    }
    record.validate().map_err(invalid_domain)?;
    Ok(record)
}

async fn transaction_id_exists<Q>(
    transaction: &Q,
    transaction_id: &GitIndexTransactionId,
) -> GitIndexTransactionStoreResult<bool>
where
    Q: QueryExecutor,
{
    let mut rows = transaction
        .query(
            "SELECT 1 FROM git_index_transaction_inputs WHERE transaction_id = ?1",
            params![transaction_id.as_str()],
        )
        .await
        .map_err(unavailable)?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(unavailable)
}

async fn repository_has_active_quarantine<Q>(
    transaction: &Q,
    repository_id: &RepositoryId,
) -> GitIndexTransactionStoreResult<bool>
where
    Q: QueryExecutor,
{
    let mut rows = transaction
        .query(
            "SELECT 1 FROM git_index_repository_quarantines
             WHERE repository_id = ?1 AND active = 1 LIMIT 1",
            params![repository_id.as_str()],
        )
        .await
        .map_err(unavailable)?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(unavailable)
}

async fn transaction_has_active_quarantine<Q>(
    transaction: &Q,
    repository_id: &RepositoryId,
    transaction_id: &GitIndexTransactionId,
) -> GitIndexTransactionStoreResult<bool>
where
    Q: QueryExecutor,
{
    let mut rows = transaction
        .query(
            "SELECT 1 FROM git_index_repository_quarantines
             WHERE repository_id = ?1 AND transaction_id = ?2 AND active = 1",
            params![repository_id.as_str(), transaction_id.as_str()],
        )
        .await
        .map_err(unavailable)?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(unavailable)
}

/// Create the durable fence once. A prior proven clear is immutable evidence:
/// it must not be silently reactivated or have its recovery receipt erased.
async fn ensure_active_quarantine<E>(
    transaction: &E,
    journal: &GitIndexTransactionJournalV1,
) -> GitIndexTransactionStoreResult<()>
where
    E: Executor,
{
    let inserted = transaction
        .execute(
            "INSERT INTO git_index_repository_quarantines
                (repository_id, transaction_id, active, created_at,
                 resolved_at, resolution_receipt_json)
             VALUES (?1, ?2, 1, ?3, NULL, NULL)
             ON CONFLICT(repository_id, transaction_id) DO NOTHING",
            params![
                journal.repository_id.as_str(),
                journal.transaction_id.as_str(),
                journal.updated_at.0,
            ],
        )
        .await
        .map_err(unavailable)?;
    if inserted == 1
        || transaction_has_active_quarantine(
            transaction,
            &journal.repository_id,
            &journal.transaction_id,
        )
        .await?
    {
        Ok(())
    } else {
        Err(GitIndexTransactionStoreError::ReceiptConflict)
    }
}

/// Resolve a fence created after admission in the same atomic write that
/// publishes a native-observed terminal receipt. The retained resolution row
/// prevents a crash between receipt publication and fence clearing from
/// permanently quarantining a transaction that recovery already proved.
async fn resolve_active_quarantine<E>(
    transaction: &E,
    journal: &GitIndexTransactionJournalV1,
    receipt: &GitIndexTransactionReceiptV1,
) -> GitIndexTransactionStoreResult<()>
where
    E: Executor,
{
    let updated = transaction
        .execute(
            "UPDATE git_index_repository_quarantines
             SET active = 0, resolved_at = ?1, resolution_receipt_json = ?2
             WHERE repository_id = ?3 AND transaction_id = ?4 AND active = 1",
            params![
                receipt.committed_at.0,
                encode(receipt)?,
                journal.repository_id.as_str(),
                journal.transaction_id.as_str(),
            ],
        )
        .await
        .map_err(unavailable)?;
    if updated == 1 {
        Ok(())
    } else {
        Err(GitIndexTransactionStoreError::RepositoryQuarantined)
    }
}

async fn needs_inspection_recovery_transactions<Q>(
    transaction: &Q,
    repository_id: &RepositoryId,
) -> GitIndexTransactionStoreResult<BTreeSet<GitIndexTransactionId>>
where
    Q: QueryExecutor,
{
    let mut rows = transaction
        .query(
            "SELECT journal.transaction_id
             FROM git_index_transaction_journals AS journal
             JOIN git_index_transaction_receipts AS receipt
               ON receipt.transaction_id = journal.transaction_id
             LEFT JOIN git_index_repository_quarantines AS quarantine
               ON quarantine.repository_id = journal.repository_id
              AND quarantine.transaction_id = journal.transaction_id
             WHERE journal.repository_id = ?1
               AND receipt.outcome = 'needs_inspection'
               AND (quarantine.active = 1 OR quarantine.transaction_id IS NULL)",
            params![repository_id.as_str()],
        )
        .await
        .map_err(unavailable)?;
    let mut transactions = BTreeSet::new();
    while let Some(row) = rows.next().await.map_err(unavailable)? {
        transactions.insert(
            GitIndexTransactionId::new(text(&row, 0, "inspection transaction id")?)
                .map_err(invalid_domain)?,
        );
    }
    Ok(transactions)
}

fn journal_transition_matches(
    current: &GitIndexTransactionJournalV1,
    expected_phase_epoch: u64,
    replacement: &GitIndexTransactionJournalV1,
) -> bool {
    current.phase_epoch == expected_phase_epoch
        && replacement.phase_epoch == expected_phase_epoch.saturating_add(1)
        && current.phase.permits_successor(replacement.phase)
        && current.transaction_id == replacement.transaction_id
        && current.preview_id == replacement.preview_id
        && current.preview_digest == replacement.preview_digest
        && current.repository_id == replacement.repository_id
        && current.worktree_id == replacement.worktree_id
        && current.operation == replacement.operation
        && current.expected_snapshot_digest == replacement.expected_snapshot_digest
        && current.started_at == replacement.started_at
}

fn phase_epoch_i64(phase_epoch: u64) -> GitIndexTransactionStoreResult<i64> {
    i64::try_from(phase_epoch)
        .map_err(|_| invalid("git index transaction phase epoch exceeds SQLite range"))
}

fn operation_code(operation: tracedecay_domain::GitIndexTransactionOperationV1) -> &'static str {
    match operation {
        tracedecay_domain::GitIndexTransactionOperationV1::StageHunks => "stage_hunks",
        tracedecay_domain::GitIndexTransactionOperationV1::UnstageHunks => "unstage_hunks",
        tracedecay_domain::GitIndexTransactionOperationV1::CommitIndex => "commit_index",
    }
}

fn phase_code(phase: GitIndexJournalPhaseV1) -> &'static str {
    match phase {
        GitIndexJournalPhaseV1::Prepared => "prepared",
        GitIndexJournalPhaseV1::NativeApplyStarted => "native_apply_started",
        GitIndexJournalPhaseV1::IndexCommitted => "index_committed",
        GitIndexJournalPhaseV1::RefCommitted => "ref_committed",
        GitIndexJournalPhaseV1::Verifying => "verifying",
        GitIndexJournalPhaseV1::Committed => "committed",
        GitIndexJournalPhaseV1::AbortedNoChange => "aborted_no_change",
        GitIndexJournalPhaseV1::NeedsInspection => "needs_inspection",
    }
}

fn receipt_outcome_code(outcome: GitIndexReceiptOutcomeV1) -> &'static str {
    match outcome {
        GitIndexReceiptOutcomeV1::Committed => "committed",
        GitIndexReceiptOutcomeV1::AbortedNoChange => "aborted_no_change",
        GitIndexReceiptOutcomeV1::NeedsInspection => "needs_inspection",
    }
}

fn encode<T: serde::Serialize>(value: &T) -> GitIndexTransactionStoreResult<String> {
    serde_json::to_string(value).map_err(|error| invalid(error.to_string()))
}

fn decode<T: serde::de::DeserializeOwned>(value: &str) -> GitIndexTransactionStoreResult<T> {
    serde_json::from_str(value).map_err(|error| invalid(error.to_string()))
}

fn text(row: &Row, column: i32, field: &'static str) -> GitIndexTransactionStoreResult<String> {
    row.get::<String>(column)
        .map_err(|error| invalid(format!("read {field}: {error}")))
}

fn optional_text(
    row: &Row,
    column: i32,
    field: &'static str,
) -> GitIndexTransactionStoreResult<Option<String>> {
    row.get::<Option<String>>(column)
        .map_err(|error| invalid(format!("read {field}: {error}")))
}

fn invalid(message: impl Into<String>) -> GitIndexTransactionStoreError {
    GitIndexTransactionStoreError::InvalidData(message.into())
}

#[allow(clippy::needless_pass_by_value)]
fn invalid_domain(error: tracedecay_domain::DomainError) -> GitIndexTransactionStoreError {
    GitIndexTransactionStoreError::InvalidData(error.to_string())
}

fn unavailable<T>(_error: T) -> GitIndexTransactionStoreError {
    GitIndexTransactionStoreError::Unavailable
}
