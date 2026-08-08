//! Worktree-cleanup records within the canonical native-integration store.

use tracedecay_domain::{
    ManifestDigest, NativeWorktreeCleanupOutcomeV1, NativeWorktreeCleanupPhaseV1,
    NativeWorktreeCleanupReceiptV1, NativeWorktreeCleanupTransactionV1,
};
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, Row, params};
use tracedecay_store::{
    NativeIntegrationStoreError, NativeIntegrationStoreResult, NativeWorktreeCleanupBeginResultV1,
};

use super::GlobalDbNativeIntegrationStore;
use super::store::{commit_outcome, decode, encode, invalid, invalid_domain, text, unavailable};

impl GlobalDbNativeIntegrationStore<'_> {
    pub async fn begin_worktree_cleanup(
        &self,
        record: NativeWorktreeCleanupTransactionV1,
    ) -> NativeIntegrationStoreResult<NativeWorktreeCleanupBeginResultV1> {
        record.validate().map_err(invalid_domain)?;
        if record.phase != NativeWorktreeCleanupPhaseV1::Prepared
            || record.phase_revision != 1
            || record.terminal_outcome.is_some()
        {
            return Err(NativeIntegrationStoreError::CleanupTransactionConflict);
        }
        let transaction = self.begin_write().await?;
        let outcome = async {
            if let Some((existing, receipt)) =
                read_cleanup_record(&transaction, &record.confirmation_digest).await?
            {
                if !existing.same_intent(&record) {
                    return Err(NativeIntegrationStoreError::CleanupTransactionConflict);
                }
                return Ok(match receipt {
                    Some(receipt) => NativeWorktreeCleanupBeginResultV1::Replay(Box::new(receipt)),
                    None => {
                        NativeWorktreeCleanupBeginResultV1::RecoveryRequired(Box::new(existing))
                    }
                });
            }
            transaction
                .execute(
                    "INSERT INTO native_worktree_cleanup_transactions
                        (confirmation_digest, inspection_digest, confirmed_at,
                         scope_set_id, scope_set_revision, scope_set_digest, project_id,
                         repository_id, worktree_id, repository_root_json,
                         worktree_root_json, phase, phase_revision, updated_at,
                         transaction_json)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                    params![
                        record.confirmation_digest.as_str(),
                        record.inspection_digest.as_str(),
                        record.confirmed_at.0,
                        record.scope_set_id.as_str(),
                        i64::try_from(record.scope_set_revision.get()).map_err(|_| invalid(
                            "native worktree cleanup scope revision exceeds SQLite range"
                        ))?,
                        record.scope_set_digest.as_str(),
                        record.command.project_id.as_str(),
                        record.command.repository_id.as_str(),
                        record.command.worktree_id.as_str(),
                        encode(&record.command.repository_root)?,
                        encode(&record.command.worktree_root)?,
                        phase_code(record.phase),
                        revision_i64(record.phase_revision)?,
                        record.updated_at.0,
                        encode(&record)?,
                    ],
                )
                .await
                .map_err(unavailable)?;
            Ok(NativeWorktreeCleanupBeginResultV1::Started(Box::new(
                record,
            )))
        }
        .await;
        commit_outcome(transaction, outcome).await
    }

    pub async fn read_worktree_cleanup(
        &self,
        confirmation_digest: &ManifestDigest,
    ) -> NativeIntegrationStoreResult<Option<NativeWorktreeCleanupTransactionV1>> {
        confirmation_digest.validate().map_err(invalid_domain)?;
        let snapshot = self.read_snapshot().await?;
        Ok(read_cleanup_record(&snapshot, confirmation_digest)
            .await?
            .map(|(record, _)| record))
    }

    pub async fn pending_worktree_cleanups(
        &self,
        repository_id: &tracedecay_domain::RepositoryId,
        limit: u32,
    ) -> NativeIntegrationStoreResult<Vec<NativeWorktreeCleanupTransactionV1>> {
        if limit == 0 {
            return Err(NativeIntegrationStoreError::Unavailable);
        }
        let snapshot = self.read_snapshot().await?;
        let query_limit = i64::from(limit).saturating_add(1);
        let mut rows = snapshot
            .query(
                "SELECT transaction_json
                 FROM native_worktree_cleanup_transactions
                 WHERE repository_id = ?1 AND phase != 'terminal'
                 ORDER BY updated_at, confirmation_digest
                 LIMIT ?2",
                params![repository_id.as_str(), query_limit],
            )
            .await
            .map_err(unavailable)?;
        let mut pending = Vec::new();
        while let Some(row) = rows.next().await.map_err(unavailable)? {
            let payload = text(&row, 0, "pending cleanup transaction payload")?;
            pending.push(decode(&payload)?);
        }
        if pending.len()
            > usize::try_from(limit).map_err(|_| NativeIntegrationStoreError::Unavailable)?
        {
            return Err(NativeIntegrationStoreError::Unavailable);
        }
        Ok(pending)
    }

    pub async fn compare_and_swap_worktree_cleanup(
        &self,
        confirmation_digest: &ManifestDigest,
        expected_phase_revision: u64,
        replacement: NativeWorktreeCleanupTransactionV1,
    ) -> NativeIntegrationStoreResult<NativeWorktreeCleanupTransactionV1> {
        confirmation_digest.validate().map_err(invalid_domain)?;
        replacement.validate().map_err(invalid_domain)?;
        if replacement.confirmation_digest != *confirmation_digest
            || replacement.phase == NativeWorktreeCleanupPhaseV1::Terminal
            || replacement.terminal_outcome.is_some()
        {
            return Err(NativeIntegrationStoreError::StatusConflict);
        }
        let transaction = self.begin_write().await?;
        let outcome = async {
            let (current, receipt) = read_cleanup_record(&transaction, confirmation_digest)
                .await?
                .ok_or(NativeIntegrationStoreError::StatusConflict)?;
            if receipt.is_some()
                || !cleanup_transition_matches(&current, expected_phase_revision, &replacement)
            {
                return Err(NativeIntegrationStoreError::StatusConflict);
            }
            let changed =
                update_cleanup(&transaction, &replacement, expected_phase_revision).await?;
            if changed != 1 {
                return Err(NativeIntegrationStoreError::StatusConflict);
            }
            Ok(replacement)
        }
        .await;
        commit_outcome(transaction, outcome).await
    }

    pub async fn write_worktree_cleanup_terminal(
        &self,
        confirmation_digest: &ManifestDigest,
        expected_phase_revision: u64,
        receipt: NativeWorktreeCleanupReceiptV1,
    ) -> NativeIntegrationStoreResult<NativeWorktreeCleanupReceiptV1> {
        confirmation_digest.validate().map_err(invalid_domain)?;
        receipt.validate().map_err(invalid_domain)?;
        if receipt.transaction.confirmation_digest != *confirmation_digest {
            return Err(NativeIntegrationStoreError::CleanupReceiptConflict);
        }
        let transaction = self.begin_write().await?;
        let outcome = async {
            let (current, existing_receipt) =
                read_cleanup_record(&transaction, confirmation_digest)
                    .await?
                    .ok_or(NativeIntegrationStoreError::CleanupReceiptConflict)?;
            if let Some(existing) = existing_receipt {
                return if existing == receipt {
                    Ok(existing)
                } else {
                    Err(NativeIntegrationStoreError::CleanupReceiptConflict)
                };
            }
            if !cleanup_transition_matches(&current, expected_phase_revision, &receipt.transaction)
                || receipt.transaction.phase != NativeWorktreeCleanupPhaseV1::Terminal
            {
                return Err(NativeIntegrationStoreError::StatusConflict);
            }
            let changed =
                update_cleanup(&transaction, &receipt.transaction, expected_phase_revision).await?;
            if changed != 1 {
                return Err(NativeIntegrationStoreError::StatusConflict);
            }
            transaction
                .execute(
                    "INSERT INTO native_worktree_cleanup_receipts
                        (confirmation_digest, receipt_digest, outcome, completed_at, receipt_json)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        confirmation_digest.as_str(),
                        receipt.receipt_digest.as_str(),
                        outcome_code(
                            receipt
                                .transaction
                                .terminal_outcome
                                .ok_or(NativeIntegrationStoreError::CleanupReceiptConflict)?
                        ),
                        receipt.completed_at.0,
                        encode(&receipt)?,
                    ],
                )
                .await
                .map_err(unavailable)?;
            Ok(receipt)
        }
        .await;
        commit_outcome(transaction, outcome).await
    }
}

const CLEANUP_SELECT: &str = "SELECT txn.transaction_json, receipt.receipt_json
    FROM native_worktree_cleanup_transactions AS txn
    LEFT JOIN native_worktree_cleanup_receipts AS receipt
      ON receipt.confirmation_digest = txn.confirmation_digest
    WHERE txn.confirmation_digest = ?1";

async fn read_cleanup_record<Q>(
    query: &Q,
    confirmation_digest: &ManifestDigest,
) -> NativeIntegrationStoreResult<
    Option<(
        NativeWorktreeCleanupTransactionV1,
        Option<NativeWorktreeCleanupReceiptV1>,
    )>,
>
where
    Q: QueryExecutor,
{
    let mut rows = query
        .query(CLEANUP_SELECT, params![confirmation_digest.as_str()])
        .await
        .map_err(unavailable)?;
    let Some(row) = rows.next().await.map_err(unavailable)? else {
        return Ok(None);
    };
    let record: NativeWorktreeCleanupTransactionV1 =
        decode(&text(&row, 0, "native worktree cleanup transaction")?)?;
    let receipt = optional_receipt(&row)?;
    if record.confirmation_digest != *confirmation_digest
        || receipt
            .as_ref()
            .is_some_and(|receipt| receipt.transaction != record)
        || (record.phase == NativeWorktreeCleanupPhaseV1::Terminal) != receipt.is_some()
    {
        return Err(invalid("native worktree cleanup row identity mismatch"));
    }
    record.validate().map_err(invalid_domain)?;
    if let Some(receipt) = &receipt {
        receipt.validate().map_err(invalid_domain)?;
    }
    if rows.next().await.map_err(unavailable)?.is_some() {
        return Err(invalid("duplicate native worktree cleanup transaction"));
    }
    Ok(Some((record, receipt)))
}

fn optional_receipt(
    row: &Row,
) -> NativeIntegrationStoreResult<Option<NativeWorktreeCleanupReceiptV1>> {
    row.get::<Option<String>>(1)
        .map_err(|error| invalid(format!("read native worktree cleanup receipt: {error}")))?
        .map(|value| decode(&value))
        .transpose()
}

async fn update_cleanup<E>(
    transaction: &E,
    replacement: &NativeWorktreeCleanupTransactionV1,
    expected_phase_revision: u64,
) -> NativeIntegrationStoreResult<u64>
where
    E: Executor,
{
    transaction
        .execute(
            "UPDATE native_worktree_cleanup_transactions
             SET phase = ?1, phase_revision = ?2, updated_at = ?3, transaction_json = ?4
             WHERE confirmation_digest = ?5 AND phase_revision = ?6",
            params![
                phase_code(replacement.phase),
                revision_i64(replacement.phase_revision)?,
                replacement.updated_at.0,
                encode(replacement)?,
                replacement.confirmation_digest.as_str(),
                revision_i64(expected_phase_revision)?,
            ],
        )
        .await
        .map_err(unavailable)
}

fn cleanup_transition_matches(
    current: &NativeWorktreeCleanupTransactionV1,
    expected_phase_revision: u64,
    replacement: &NativeWorktreeCleanupTransactionV1,
) -> bool {
    current.phase_revision == expected_phase_revision
        && replacement.phase_revision == expected_phase_revision.saturating_add(1)
        && current.phase != NativeWorktreeCleanupPhaseV1::Terminal
        && current.same_identity(replacement)
        && matches!(
            (current.phase, replacement.phase),
            (
                NativeWorktreeCleanupPhaseV1::Prepared,
                NativeWorktreeCleanupPhaseV1::MutationStarted
                    | NativeWorktreeCleanupPhaseV1::Terminal
            ) | (
                NativeWorktreeCleanupPhaseV1::MutationStarted,
                NativeWorktreeCleanupPhaseV1::NeedsReconciliation
                    | NativeWorktreeCleanupPhaseV1::Terminal
            ) | (
                NativeWorktreeCleanupPhaseV1::NeedsReconciliation,
                NativeWorktreeCleanupPhaseV1::Terminal
            )
        )
}

fn revision_i64(revision: u64) -> NativeIntegrationStoreResult<i64> {
    i64::try_from(revision)
        .map_err(|_| invalid("native worktree cleanup revision exceeds SQLite range"))
}

fn phase_code(phase: NativeWorktreeCleanupPhaseV1) -> &'static str {
    match phase {
        NativeWorktreeCleanupPhaseV1::Prepared => "prepared",
        NativeWorktreeCleanupPhaseV1::MutationStarted => "mutation_started",
        NativeWorktreeCleanupPhaseV1::NeedsReconciliation => "needs_reconciliation",
        NativeWorktreeCleanupPhaseV1::Terminal => "terminal",
    }
}

fn outcome_code(outcome: NativeWorktreeCleanupOutcomeV1) -> &'static str {
    match outcome {
        NativeWorktreeCleanupOutcomeV1::Removed => "removed",
        NativeWorktreeCleanupOutcomeV1::AbortedNoChange => "aborted_no_change",
        NativeWorktreeCleanupOutcomeV1::RefusedForeignDrift => "refused_foreign_drift",
    }
}
