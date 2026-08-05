use tracedecay_domain::{
    NativeIntegrationApprovalId, NativeIntegrationIdempotencyKey, NativeIntegrationJournalPhaseV1,
    NativeIntegrationJournalV1, NativeIntegrationPreviewV1, NativeIntegrationReceiptOutcomeV1,
    NativeIntegrationReceiptV1, NativeIntegrationRecoveryReceiptV1, NativeIntegrationTransactionId,
    RepositoryId,
};
use tracedecay_store::{
    NativeIntegrationBeginRequestV1, NativeIntegrationBeginResultV1, NativeIntegrationRecordV1,
    NativeIntegrationStoreError, NativeIntegrationStoreResult, NativeIntegrationTerminalWriteV1,
};

use crate::RegisteredGlobalDb;
#[cfg(any(test, feature = "test-helpers"))]
use tracedecay_runtime_core::db::engine::Connection;
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, Row, params};

use super::database::{GitMutationDatabase, GitMutationWriteTransaction};

pub struct GlobalDbNativeIntegrationStore<'db> {
    db: GitMutationDatabase<'db>,
}

impl<'db> GlobalDbNativeIntegrationStore<'db> {
    pub const fn new(db: &'db RegisteredGlobalDb) -> Self {
        Self {
            db: GitMutationDatabase::Registered(db),
        }
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub const fn for_engine_test(db: &'db Connection) -> Self {
        Self {
            db: GitMutationDatabase::Engine(db),
        }
    }

    pub async fn begin_or_replay(
        &self,
        request: NativeIntegrationBeginRequestV1,
    ) -> NativeIntegrationStoreResult<NativeIntegrationBeginResultV1> {
        request.validate().map_err(invalid_domain)?;
        let transaction = self.begin_write().await?;
        let outcome = async {
            if let Some(existing) =
                record_by_idempotency_key(&transaction, &request.idempotency_key).await?
            {
                if existing.input_digest != request.input_digest
                    || existing.approval_id != request.approval_id
                    || existing.approval_digest != request.approval_digest
                    || existing.preview != request.preview
                    || existing.journal.transaction_id != request.journal.transaction_id
                {
                    return Err(NativeIntegrationStoreError::IdempotencyConflict);
                }
                return Ok(match existing.terminal_receipt {
                    Some(receipt) => NativeIntegrationBeginResultV1::Replay(Box::new(receipt)),
                    None => NativeIntegrationBeginResultV1::RecoveryRequired(Box::new(existing)),
                });
            }
            if repository_has_active_quarantine(&transaction, &request.journal.repository_id)
                .await?
            {
                return Err(NativeIntegrationStoreError::RepositoryQuarantined);
            }
            if approval_exists(&transaction, &request.approval_id).await? {
                return Err(NativeIntegrationStoreError::ApprovalConsumed);
            }
            if transaction_exists(&transaction, &request.journal.transaction_id).await? {
                return Err(NativeIntegrationStoreError::IdempotencyConflict);
            }
            insert_preview_if_absent(&transaction, &request.preview).await?;
            transaction
                .execute(
                    "INSERT INTO native_integration_inputs
                        (idempotency_key, input_digest, approval_id, approval_digest,
                         transaction_id, preview_id, preview_digest, repository_id, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        request.idempotency_key.as_str(),
                        request.input_digest.as_str(),
                        request.approval_id.as_str(),
                        request.approval_digest.as_str(),
                        request.journal.transaction_id.as_str(),
                        request.preview.preview_id.as_str(),
                        request.preview.preview_digest.as_str(),
                        request.journal.repository_id.as_str(),
                        request.journal.started_at.0,
                    ],
                )
                .await
                .map_err(unavailable)?;
            insert_journal(&transaction, &request.idempotency_key, &request.journal).await?;
            let record = NativeIntegrationRecordV1 {
                idempotency_key: request.idempotency_key,
                input_digest: request.input_digest,
                approval_id: request.approval_id,
                approval_digest: request.approval_digest,
                preview: request.preview,
                journal: request.journal,
                terminal_receipt: None,
            };
            record.validate().map_err(invalid_domain)?;
            Ok(NativeIntegrationBeginResultV1::Started(Box::new(record)))
        }
        .await;
        commit_outcome(transaction, outcome).await
    }

    pub async fn read_transaction(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationRecordV1>> {
        transaction_id.validate().map_err(invalid_domain)?;
        let snapshot = self.db.read_snapshot().await.map_err(unavailable)?;
        record_by_transaction_id(&snapshot, transaction_id).await
    }

    pub async fn compare_and_swap_journal(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
        expected_revision: u64,
        replacement: NativeIntegrationJournalV1,
    ) -> NativeIntegrationStoreResult<NativeIntegrationJournalV1> {
        transaction_id.validate().map_err(invalid_domain)?;
        replacement.validate().map_err(invalid_domain)?;
        if replacement.phase.is_terminal() {
            return Err(NativeIntegrationStoreError::JournalConflict);
        }
        let transaction = self.begin_write().await?;
        let outcome = async {
            let current = record_by_transaction_id(&transaction, transaction_id)
                .await?
                .ok_or(NativeIntegrationStoreError::JournalConflict)?;
            if current.terminal_receipt.is_some()
                || current.journal.revision != expected_revision
                || !current.journal.permits_replacement(&replacement)
            {
                return Err(NativeIntegrationStoreError::JournalConflict);
            }
            let updated = update_journal(
                &transaction,
                transaction_id,
                expected_revision,
                &replacement,
            )
            .await?;
            if updated != 1 {
                return Err(NativeIntegrationStoreError::JournalConflict);
            }
            Ok(replacement)
        }
        .await;
        commit_outcome(transaction, outcome).await
    }

    pub async fn write_terminal(
        &self,
        write: NativeIntegrationTerminalWriteV1,
    ) -> NativeIntegrationStoreResult<NativeIntegrationReceiptV1> {
        write.validate().map_err(invalid_domain)?;
        let transaction = self.begin_write().await?;
        let outcome = async {
            let current = record_by_transaction_id(&transaction, &write.transaction_id)
                .await?
                .ok_or(NativeIntegrationStoreError::ReceiptConflict)?;
            if let Some(existing) = current.terminal_receipt {
                return if existing == write.receipt {
                    Ok(existing)
                } else {
                    Err(NativeIntegrationStoreError::ReceiptConflict)
                };
            }
            if current.journal.revision != write.expected_current_revision
                || !current.journal.permits_replacement(&write.journal)
            {
                return Err(NativeIntegrationStoreError::JournalConflict);
            }
            if write.receipt.outcome == NativeIntegrationReceiptOutcomeV1::NeedsInspection {
                ensure_active_quarantine(&transaction, &write.journal).await?;
            }
            let updated = update_journal(
                &transaction,
                &write.transaction_id,
                write.expected_current_revision,
                &write.journal,
            )
            .await?;
            if updated != 1 {
                return Err(NativeIntegrationStoreError::JournalConflict);
            }
            transaction
                .execute(
                    "INSERT INTO native_integration_receipts
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
    ) -> NativeIntegrationStoreResult<Vec<NativeIntegrationRecordV1>> {
        repository_id.validate().map_err(invalid_domain)?;
        let snapshot = self.db.read_snapshot().await.map_err(unavailable)?;
        let mut rows = snapshot
            .query(
                "SELECT input.idempotency_key
                 FROM native_integration_inputs AS input
                 JOIN native_integration_journals AS journal
                   ON journal.transaction_id = input.transaction_id
                 LEFT JOIN native_integration_receipts AS receipt
                   ON receipt.transaction_id = journal.transaction_id
                 LEFT JOIN git_repository_mutation_quarantines AS quarantine
                   ON quarantine.repository_id = journal.repository_id
                  AND quarantine.transaction_kind = 'native_integration'
                  AND quarantine.transaction_id = journal.transaction_id
                 WHERE journal.repository_id = ?1
                   AND (receipt.transaction_id IS NULL
                        OR (receipt.outcome = 'needs_inspection' AND quarantine.active = 1))
                 ORDER BY journal.started_at, journal.transaction_id",
                params![repository_id.as_str()],
            )
            .await
            .map_err(unavailable)?;
        let mut records = Vec::new();
        while let Some(row) = rows.next().await.map_err(unavailable)? {
            let key = NativeIntegrationIdempotencyKey::new(text(
                &row,
                0,
                "native integration recovery key",
            )?)
            .map_err(invalid_domain)?;
            records.push(
                record_by_idempotency_key(&snapshot, &key)
                    .await?
                    .ok_or_else(|| invalid("missing native integration recovery record"))?,
            );
        }
        Ok(records)
    }

    pub async fn recovery_repositories(&self) -> NativeIntegrationStoreResult<Vec<RepositoryId>> {
        let snapshot = self.db.read_snapshot().await.map_err(unavailable)?;
        let mut rows = snapshot
            .query(
                "SELECT DISTINCT journal.repository_id
                 FROM native_integration_journals AS journal
                 LEFT JOIN native_integration_receipts AS receipt
                   ON receipt.transaction_id = journal.transaction_id
                 LEFT JOIN git_repository_mutation_quarantines AS quarantine
                   ON quarantine.repository_id = journal.repository_id
                  AND quarantine.transaction_kind = 'native_integration'
                  AND quarantine.transaction_id = journal.transaction_id
                 WHERE receipt.transaction_id IS NULL
                    OR (receipt.outcome = 'needs_inspection' AND quarantine.active = 1)
                 ORDER BY journal.repository_id",
                (),
            )
            .await
            .map_err(unavailable)?;
        let mut repositories = Vec::new();
        while let Some(row) = rows.next().await.map_err(unavailable)? {
            repositories.push(
                RepositoryId::new(text(&row, 0, "native integration recovery repository")?)
                    .map_err(invalid_domain)?,
            );
        }
        Ok(repositories)
    }

    pub async fn quarantine_repository(
        &self,
        repository_id: &RepositoryId,
        transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<()> {
        repository_id.validate().map_err(invalid_domain)?;
        transaction_id.validate().map_err(invalid_domain)?;
        let transaction = self.begin_write().await?;
        let outcome = async {
            let record = record_by_transaction_id(&transaction, transaction_id)
                .await?
                .ok_or(NativeIntegrationStoreError::JournalConflict)?;
            if record.journal.repository_id != *repository_id {
                return Err(NativeIntegrationStoreError::JournalConflict);
            }
            ensure_active_quarantine(&transaction, &record.journal).await
        }
        .await;
        commit_outcome(transaction, outcome).await
    }

    pub async fn clear_repository_quarantine(
        &self,
        repository_id: &RepositoryId,
        transaction_id: &NativeIntegrationTransactionId,
        recovery_receipt: NativeIntegrationRecoveryReceiptV1,
    ) -> NativeIntegrationStoreResult<()> {
        repository_id.validate().map_err(invalid_domain)?;
        transaction_id.validate().map_err(invalid_domain)?;
        let transaction = self.begin_write().await?;
        let outcome = async {
            let record = record_by_transaction_id(&transaction, transaction_id)
                .await?
                .ok_or(NativeIntegrationStoreError::ReceiptConflict)?;
            let inspection = record
                .terminal_receipt
                .as_ref()
                .ok_or(NativeIntegrationStoreError::ReceiptConflict)?;
            recovery_receipt
                .validate_against(&record.journal, inspection)
                .map_err(invalid_domain)?;
            if record.journal.repository_id != *repository_id {
                return Err(NativeIntegrationStoreError::ReceiptConflict);
            }
            let updated = transaction
                .execute(
                    "UPDATE git_repository_mutation_quarantines
                     SET active = 0, resolved_at = ?1, resolution_receipt_json = ?2
                     WHERE repository_id = ?3
                       AND transaction_kind = 'native_integration'
                       AND transaction_id = ?4 AND active = 1",
                    params![
                        recovery_receipt.recovered_at.0,
                        encode(&recovery_receipt)?,
                        repository_id.as_str(),
                        transaction_id.as_str(),
                    ],
                )
                .await
                .map_err(unavailable)?;
            if updated != 1 {
                return Err(NativeIntegrationStoreError::RepositoryQuarantined);
            }
            Ok(())
        }
        .await;
        commit_outcome(transaction, outcome).await
    }

    async fn begin_write(&self) -> NativeIntegrationStoreResult<GitMutationWriteTransaction<'_>> {
        self.db.begin_write().await.map_err(unavailable)
    }
}

async fn insert_preview_if_absent<E>(
    transaction: &E,
    preview: &NativeIntegrationPreviewV1,
) -> NativeIntegrationStoreResult<()>
where
    E: Executor + QueryExecutor,
{
    let mut rows = transaction
        .query(
            "SELECT preview_json FROM native_integration_previews WHERE preview_id = ?1",
            params![preview.preview_id.as_str()],
        )
        .await
        .map_err(unavailable)?;
    if let Some(row) = rows.next().await.map_err(unavailable)? {
        let existing: NativeIntegrationPreviewV1 =
            decode(&text(&row, 0, "native integration preview")?)?;
        return if existing == *preview {
            Ok(())
        } else {
            Err(NativeIntegrationStoreError::PreviewConflict)
        };
    }
    transaction
        .execute(
            "INSERT INTO native_integration_previews
                (preview_id, preview_digest, repository_id, source_worktree_id,
                 destination_worktree_id, mode, created_at, expires_at, preview_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                preview.preview_id.as_str(),
                preview.preview_digest.as_str(),
                preview.repository_id.as_str(),
                preview.source_worktree_id.as_str(),
                preview.destination_worktree_id.as_str(),
                mode_code(preview.mode),
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
    key: &NativeIntegrationIdempotencyKey,
    journal: &NativeIntegrationJournalV1,
) -> NativeIntegrationStoreResult<()>
where
    E: Executor,
{
    transaction
        .execute(
            "INSERT INTO native_integration_journals
                (transaction_id, idempotency_key, repository_id, phase, revision,
                 started_at, updated_at, journal_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                journal.transaction_id.as_str(),
                key.as_str(),
                journal.repository_id.as_str(),
                phase_code(journal.phase),
                revision_i64(journal.revision)?,
                journal.started_at.0,
                journal.updated_at.0,
                encode(journal)?,
            ],
        )
        .await
        .map(|_| ())
        .map_err(unavailable)
}

async fn update_journal<E>(
    transaction: &E,
    transaction_id: &NativeIntegrationTransactionId,
    expected_revision: u64,
    replacement: &NativeIntegrationJournalV1,
) -> NativeIntegrationStoreResult<u64>
where
    E: Executor,
{
    transaction
        .execute(
            "UPDATE native_integration_journals
             SET phase = ?1, revision = ?2, updated_at = ?3, journal_json = ?4
             WHERE transaction_id = ?5 AND revision = ?6",
            params![
                phase_code(replacement.phase),
                revision_i64(replacement.revision)?,
                replacement.updated_at.0,
                encode(replacement)?,
                transaction_id.as_str(),
                revision_i64(expected_revision)?,
            ],
        )
        .await
        .map_err(unavailable)
}

async fn record_by_idempotency_key<Q>(
    query: &Q,
    key: &NativeIntegrationIdempotencyKey,
) -> NativeIntegrationStoreResult<Option<NativeIntegrationRecordV1>>
where
    Q: QueryExecutor,
{
    read_record(query, "WHERE input.idempotency_key = ?1", key.as_str()).await
}

async fn record_by_transaction_id<Q>(
    query: &Q,
    transaction_id: &NativeIntegrationTransactionId,
) -> NativeIntegrationStoreResult<Option<NativeIntegrationRecordV1>>
where
    Q: QueryExecutor,
{
    read_record(
        query,
        "WHERE input.transaction_id = ?1",
        transaction_id.as_str(),
    )
    .await
}

async fn read_record<Q>(
    query: &Q,
    predicate: &'static str,
    value: &str,
) -> NativeIntegrationStoreResult<Option<NativeIntegrationRecordV1>>
where
    Q: QueryExecutor,
{
    let sql = format!(
        "SELECT input.idempotency_key, input.input_digest, input.approval_id,
                input.approval_digest, preview.preview_json, journal.journal_json,
                receipt.receipt_json
         FROM native_integration_inputs AS input
         JOIN native_integration_previews AS preview ON preview.preview_id = input.preview_id
         JOIN native_integration_journals AS journal
           ON journal.transaction_id = input.transaction_id
         LEFT JOIN native_integration_receipts AS receipt
           ON receipt.transaction_id = journal.transaction_id
         {predicate}"
    );
    let mut rows = query
        .query(&sql, params![value])
        .await
        .map_err(unavailable)?;
    let Some(row) = rows.next().await.map_err(unavailable)? else {
        return Ok(None);
    };
    let record = decode_record(&row)?;
    if rows.next().await.map_err(unavailable)?.is_some() {
        return Err(invalid("duplicate native integration transaction"));
    }
    Ok(Some(record))
}

fn decode_record(row: &Row) -> NativeIntegrationStoreResult<NativeIntegrationRecordV1> {
    let record = NativeIntegrationRecordV1 {
        idempotency_key: NativeIntegrationIdempotencyKey::new(text(
            row,
            0,
            "native integration idempotency key",
        )?)
        .map_err(invalid_domain)?,
        input_digest: tracedecay_domain::ManifestDigest::new(text(
            row,
            1,
            "native integration input digest",
        )?)
        .map_err(invalid_domain)?,
        approval_id: NativeIntegrationApprovalId::new(text(
            row,
            2,
            "native integration approval id",
        )?)
        .map_err(invalid_domain)?,
        approval_digest: tracedecay_domain::ManifestDigest::new(text(
            row,
            3,
            "native integration approval digest",
        )?)
        .map_err(invalid_domain)?,
        preview: decode(&text(row, 4, "native integration preview")?)?,
        journal: decode(&text(row, 5, "native integration journal")?)?,
        terminal_receipt: row
            .get::<Option<String>>(6)
            .map_err(unavailable)?
            .map(|json| decode(&json))
            .transpose()?,
    };
    record.validate().map_err(invalid_domain)?;
    Ok(record)
}

async fn approval_exists<Q>(
    query: &Q,
    approval_id: &NativeIntegrationApprovalId,
) -> NativeIntegrationStoreResult<bool>
where
    Q: QueryExecutor,
{
    exists(
        query,
        "SELECT 1 FROM native_integration_inputs WHERE approval_id = ?1",
        approval_id.as_str(),
    )
    .await
}

async fn transaction_exists<Q>(
    query: &Q,
    transaction_id: &NativeIntegrationTransactionId,
) -> NativeIntegrationStoreResult<bool>
where
    Q: QueryExecutor,
{
    exists(
        query,
        "SELECT 1 FROM native_integration_inputs WHERE transaction_id = ?1",
        transaction_id.as_str(),
    )
    .await
}

async fn repository_has_active_quarantine<Q>(
    query: &Q,
    repository_id: &RepositoryId,
) -> NativeIntegrationStoreResult<bool>
where
    Q: QueryExecutor,
{
    exists(
        query,
        "SELECT 1 FROM git_repository_mutation_quarantines
         WHERE repository_id = ?1 AND active = 1 LIMIT 1",
        repository_id.as_str(),
    )
    .await
}

async fn exists<Q>(query: &Q, sql: &str, value: &str) -> NativeIntegrationStoreResult<bool>
where
    Q: QueryExecutor,
{
    let mut rows = query
        .query(sql, params![value])
        .await
        .map_err(unavailable)?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(unavailable)
}

async fn ensure_active_quarantine<E>(
    transaction: &E,
    journal: &NativeIntegrationJournalV1,
) -> NativeIntegrationStoreResult<()>
where
    E: Executor + QueryExecutor,
{
    let inserted = transaction
        .execute(
            "INSERT INTO git_repository_mutation_quarantines
                (repository_id, transaction_kind, transaction_id, active, created_at,
                 resolved_at, resolution_receipt_json)
             VALUES (?1, 'native_integration', ?2, 1, ?3, NULL, NULL)
             ON CONFLICT(repository_id, transaction_kind, transaction_id) DO NOTHING",
            params![
                journal.repository_id.as_str(),
                journal.transaction_id.as_str(),
                journal.updated_at.0,
            ],
        )
        .await
        .map_err(unavailable)?;
    if inserted == 1 || {
        let mut rows = transaction
            .query(
                "SELECT 1 FROM git_repository_mutation_quarantines
                     WHERE repository_id = ?1
                       AND transaction_kind = 'native_integration'
                       AND transaction_id = ?2 AND active = 1",
                params![
                    journal.repository_id.as_str(),
                    journal.transaction_id.as_str()
                ],
            )
            .await
            .map_err(unavailable)?;
        rows.next().await.map_err(unavailable)?.is_some()
    } {
        Ok(())
    } else {
        Err(NativeIntegrationStoreError::RepositoryQuarantined)
    }
}

async fn commit_outcome<T>(
    transaction: GitMutationWriteTransaction<'_>,
    outcome: NativeIntegrationStoreResult<T>,
) -> NativeIntegrationStoreResult<T> {
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

fn mode_code(mode: tracedecay_domain::NativeIntegrationMechanicalModeV1) -> &'static str {
    match mode {
        tracedecay_domain::NativeIntegrationMechanicalModeV1::FastForward => "fast_forward",
        tracedecay_domain::NativeIntegrationMechanicalModeV1::TwoParentMerge => "two_parent_merge",
        tracedecay_domain::NativeIntegrationMechanicalModeV1::CherryPickExactCommits => {
            "cherry_pick_exact_commits"
        }
    }
}

fn phase_code(phase: NativeIntegrationJournalPhaseV1) -> &'static str {
    match phase {
        NativeIntegrationJournalPhaseV1::Prepared => "prepared",
        NativeIntegrationJournalPhaseV1::NativeApplyStarted => "native_apply_started",
        NativeIntegrationJournalPhaseV1::ObjectsWritten => "objects_written",
        NativeIntegrationJournalPhaseV1::DestinationMaterialized => "destination_materialized",
        NativeIntegrationJournalPhaseV1::RefCommitted => "ref_committed",
        NativeIntegrationJournalPhaseV1::Verifying => "verifying",
        NativeIntegrationJournalPhaseV1::Committed => "committed",
        NativeIntegrationJournalPhaseV1::AbortedNoChange => "aborted_no_change",
        NativeIntegrationJournalPhaseV1::RolledBack => "rolled_back",
        NativeIntegrationJournalPhaseV1::NeedsInspection => "needs_inspection",
    }
}

fn receipt_outcome_code(outcome: NativeIntegrationReceiptOutcomeV1) -> &'static str {
    match outcome {
        NativeIntegrationReceiptOutcomeV1::Committed => "committed",
        NativeIntegrationReceiptOutcomeV1::AbortedNoChange => "aborted_no_change",
        NativeIntegrationReceiptOutcomeV1::RolledBack => "rolled_back",
        NativeIntegrationReceiptOutcomeV1::NeedsInspection => "needs_inspection",
    }
}

fn revision_i64(revision: u64) -> NativeIntegrationStoreResult<i64> {
    i64::try_from(revision).map_err(|_| invalid("native integration revision exceeds SQLite range"))
}

fn encode<T: serde::Serialize>(value: &T) -> NativeIntegrationStoreResult<String> {
    serde_json::to_string(value).map_err(|error| invalid(error.to_string()))
}

fn decode<T: serde::de::DeserializeOwned>(value: &str) -> NativeIntegrationStoreResult<T> {
    serde_json::from_str(value).map_err(|error| invalid(error.to_string()))
}

fn text(row: &Row, column: i32, field: &'static str) -> NativeIntegrationStoreResult<String> {
    row.get::<String>(column)
        .map_err(|error| invalid(format!("read {field}: {error}")))
}

fn invalid(message: impl Into<String>) -> NativeIntegrationStoreError {
    NativeIntegrationStoreError::InvalidData(message.into())
}

fn invalid_domain(error: tracedecay_domain::DomainError) -> NativeIntegrationStoreError {
    NativeIntegrationStoreError::InvalidData(error.to_string())
}

fn unavailable<T>(_error: T) -> NativeIntegrationStoreError {
    NativeIntegrationStoreError::Unavailable
}
