use tracedecay_domain::{
    NativeIntegrationApprovalId, NativeIntegrationApprovalV1, NativeIntegrationPhaseV1,
    NativeIntegrationPreviewId, NativeIntegrationPreviewV1, NativeIntegrationReceiptV1,
    NativeIntegrationTerminalOutcomeV1, NativeIntegrationTransactionId,
    NativeIntegrationTransactionStatusV1, RepositoryId,
};
use tracedecay_store::{
    NativeIntegrationBeginResultV1, NativeIntegrationRecordV1, NativeIntegrationStoreError,
    NativeIntegrationStoreResult,
};

use crate::RegisteredGlobalDb;
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, Row, params};

use crate::git_index_transactions::database::{
    GitMutationDatabase, GitMutationReadSnapshot, GitMutationWriteTransaction,
};

/// Async canonical-store adapter for native integration transaction state.
///
/// The adapter borrows the already-mounted registered session database; it
/// never opens a database or derives a path. Every mutation owns one
/// `IMMEDIATE` transaction from that runtime through commit or rollback.
/// The synchronous `tracedecay-store` contract is bridged by the daemon's
/// bounded store actor.
pub struct GlobalDbNativeIntegrationStore<'db> {
    db: GitMutationDatabase<'db>,
}

impl<'db> GlobalDbNativeIntegrationStore<'db> {
    pub const fn new(db: &'db RegisteredGlobalDb) -> Self {
        Self {
            db: GitMutationDatabase::Registered(db),
        }
    }

    pub async fn save_preview(
        &self,
        preview: NativeIntegrationPreviewV1,
    ) -> NativeIntegrationStoreResult<()> {
        preview.validate().map_err(invalid_domain)?;
        let transaction = self.begin_write().await?;
        let outcome = insert_preview_if_absent(&transaction, &preview).await;
        commit_outcome(transaction, outcome).await
    }

    pub async fn read_preview(
        &self,
        preview_id: &NativeIntegrationPreviewId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationPreviewV1>> {
        preview_id.validate().map_err(invalid_domain)?;
        let snapshot = self.read_snapshot().await?;
        read_preview_from_transaction(&snapshot, preview_id).await
    }

    /// Persists one issued approval commitment.
    ///
    /// Issuance is idempotent for byte-identical approvals; a different
    /// approval under the same identity or digest is a conflict, never an
    /// overwrite. Consumption is not recorded here: an approval is consumed
    /// exactly when a transaction row binds its unique `approval_id`.
    pub async fn save_approval(
        &self,
        approval: NativeIntegrationApprovalV1,
    ) -> NativeIntegrationStoreResult<()> {
        approval.validate().map_err(invalid_domain)?;
        let transaction = self.begin_write().await?;
        let outcome = insert_approval_if_absent(&transaction, &approval).await;
        commit_outcome(transaction, outcome).await
    }

    pub async fn read_approval(
        &self,
        approval_id: &NativeIntegrationApprovalId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationApprovalV1>> {
        approval_id.validate().map_err(invalid_domain)?;
        let snapshot = self.read_snapshot().await?;
        read_approval_from_transaction(&snapshot, approval_id).await
    }

    /// Atomically consumes the approval and inserts the `Prepared` record.
    pub async fn begin_or_replay(
        &self,
        record: NativeIntegrationRecordV1,
    ) -> NativeIntegrationStoreResult<NativeIntegrationBeginResultV1> {
        record.validate().map_err(invalid_domain)?;
        if record.terminal_receipt.is_some()
            || record.status.terminal_outcome.is_some()
            || record.status.phase != NativeIntegrationPhaseV1::Prepared
        {
            return Err(NativeIntegrationStoreError::TransactionConflict);
        }
        let transaction = self.begin_write().await?;
        let outcome = async {
            let transaction_id = &record.status.transaction_id;
            if let Some(existing) =
                read_record_from_transaction(&transaction, transaction_id).await?
            {
                if existing.preview != record.preview || existing.approval != record.approval {
                    return Err(NativeIntegrationStoreError::TransactionConflict);
                }
                return Ok(match existing.terminal_receipt {
                    Some(receipt) => NativeIntegrationBeginResultV1::Replay(Box::new(receipt)),
                    None => NativeIntegrationBeginResultV1::RecoveryRequired(Box::new(existing)),
                });
            }

            if approval_consumed_in_transaction(&transaction, &record.approval.approval_id).await? {
                return Err(NativeIntegrationStoreError::ApprovalConflict);
            }
            if let Some(issued) =
                read_approval_from_transaction(&transaction, &record.approval.approval_id).await?
                && issued != record.approval
            {
                return Err(NativeIntegrationStoreError::ApprovalConflict);
            }
            let repository_id = &record.preview.repository_snapshot.repository_id;
            if repository_has_active_quarantine(&transaction, repository_id).await? {
                return Err(NativeIntegrationStoreError::RepositoryQuarantined);
            }

            insert_preview_if_absent(&transaction, &record.preview).await?;
            transaction
                .execute(
                    "INSERT INTO native_integration_transactions
                        (transaction_id, approval_id, preview_id, preview_digest,
                         repository_id, destination_ref, expected_destination_tip,
                         phase, phase_revision, cancellation_requested, terminal_outcome,
                         updated_at, status_json, approval_json)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, ?11, ?12, ?13)",
                    params![
                        record.status.transaction_id.as_str(),
                        record.approval.approval_id.as_str(),
                        record.status.preview_id.as_str(),
                        record.status.preview_digest.as_str(),
                        record.status.repository_id.as_str(),
                        record.status.destination_ref.as_str(),
                        record.status.expected_destination_tip.as_str(),
                        phase_code(record.status.phase),
                        phase_revision_i64(record.status.phase_revision)?,
                        i64::from(record.status.cancellation_requested),
                        record.status.updated_at.0,
                        encode(&record.status)?,
                        encode(&record.approval)?,
                    ],
                )
                .await
                .map_err(unavailable)?;
            Ok(NativeIntegrationBeginResultV1::Started(Box::new(record)))
        }
        .await;
        commit_outcome(transaction, outcome).await
    }

    pub async fn read_status(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationTransactionStatusV1>> {
        transaction_id.validate().map_err(invalid_domain)?;
        let snapshot = self.read_snapshot().await?;
        read_status_from_transaction(&snapshot, transaction_id).await
    }

    pub async fn read_record(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationRecordV1>> {
        transaction_id.validate().map_err(invalid_domain)?;
        let snapshot = self.read_snapshot().await?;
        read_record_from_transaction(&snapshot, transaction_id).await
    }

    pub async fn read_receipt(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationReceiptV1>> {
        transaction_id.validate().map_err(invalid_domain)?;
        let snapshot = self.read_snapshot().await?;
        read_receipt_from_transaction(&snapshot, transaction_id).await
    }

    pub async fn compare_and_swap_status(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
        expected_phase_revision: u64,
        replacement: NativeIntegrationTransactionStatusV1,
    ) -> NativeIntegrationStoreResult<NativeIntegrationTransactionStatusV1> {
        transaction_id.validate().map_err(invalid_domain)?;
        replacement.validate().map_err(invalid_domain)?;
        // Terminal states are only reachable through `write_terminal`, which
        // publishes the immutable receipt in the same database transaction.
        if replacement.phase == NativeIntegrationPhaseV1::Terminal
            || replacement.terminal_outcome.is_some()
        {
            return Err(NativeIntegrationStoreError::StatusConflict);
        }
        let transaction = self.begin_write().await?;
        let outcome = async {
            let current = read_status_from_transaction(&transaction, transaction_id)
                .await?
                .ok_or(NativeIntegrationStoreError::StatusConflict)?;
            if !status_transition_matches(&current, expected_phase_revision, &replacement) {
                return Err(NativeIntegrationStoreError::StatusConflict);
            }
            let updated =
                update_status_row(&transaction, &replacement, expected_phase_revision).await?;
            if updated != 1 {
                return Err(NativeIntegrationStoreError::StatusConflict);
            }
            Ok(replacement)
        }
        .await;
        commit_outcome(transaction, outcome).await
    }

    /// Publishes the terminal status transition and its receipt in one
    /// immediate database transaction, so restart recovery never observes a
    /// terminal phase without its immutable receipt or quarantine fence.
    pub async fn write_terminal(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
        expected_phase_revision: u64,
        receipt: NativeIntegrationReceiptV1,
    ) -> NativeIntegrationStoreResult<NativeIntegrationReceiptV1> {
        transaction_id.validate().map_err(invalid_domain)?;
        receipt.validate().map_err(invalid_domain)?;
        let Some(outcome_code) = receipt.status.terminal_outcome.map(terminal_outcome_code) else {
            return Err(NativeIntegrationStoreError::ReceiptConflict);
        };
        if receipt.status.transaction_id != *transaction_id {
            return Err(NativeIntegrationStoreError::ReceiptConflict);
        }
        let transaction = self.begin_write().await?;
        let write = async {
            let record = read_record_from_transaction(&transaction, transaction_id)
                .await?
                .ok_or(NativeIntegrationStoreError::ReceiptConflict)?;
            if let Some(existing) = record.terminal_receipt {
                return if existing == receipt {
                    Ok(existing)
                } else {
                    Err(NativeIntegrationStoreError::ReceiptConflict)
                };
            }
            if !status_transition_matches(&record.status, expected_phase_revision, &receipt.status)
            {
                return Err(NativeIntegrationStoreError::StatusConflict);
            }
            if receipt.status.terminal_outcome
                == Some(NativeIntegrationTerminalOutcomeV1::NeedsInspection)
            {
                ensure_active_quarantine(
                    &transaction,
                    &record.status.repository_id,
                    transaction_id,
                    receipt.status.updated_at.0,
                )
                .await?;
            }
            let updated =
                update_status_row(&transaction, &receipt.status, expected_phase_revision).await?;
            if updated != 1 {
                return Err(NativeIntegrationStoreError::StatusConflict);
            }
            transaction
                .execute(
                    "INSERT INTO native_integration_receipts
                        (transaction_id, receipt_digest, preview_id, outcome,
                         completed_at, receipt_json)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        receipt.status.transaction_id.as_str(),
                        receipt.receipt_digest.as_str(),
                        receipt.status.preview_id.as_str(),
                        outcome_code,
                        receipt.completed_at.0,
                        encode(&receipt)?,
                    ],
                )
                .await
                .map_err(unavailable)?;
            Ok(receipt)
        }
        .await;
        commit_outcome(transaction, write).await
    }

    /// Every transaction that has not reached its terminal receipt, oldest
    /// first. Restart recovery replays these through the coordinator.
    pub async fn pending_transactions(
        &self,
        repository_id: Option<&RepositoryId>,
    ) -> NativeIntegrationStoreResult<Vec<NativeIntegrationRecordV1>> {
        if let Some(repository_id) = repository_id {
            repository_id.validate().map_err(invalid_domain)?;
        }
        let snapshot = self.read_snapshot().await?;
        let mut rows = snapshot
            .query(
                &format!(
                    "{RECORD_SELECT}
                     WHERE txn.terminal_outcome IS NULL
                       AND (?1 IS NULL OR txn.repository_id = ?1)
                     ORDER BY txn.updated_at, txn.transaction_id"
                ),
                params![repository_id.map(RepositoryId::as_str)],
            )
            .await
            .map_err(unavailable)?;
        let mut records = Vec::new();
        while let Some(row) = rows.next().await.map_err(unavailable)? {
            records.push(decode_record(&row)?);
        }
        Ok(records)
    }

    pub async fn approval_consumed(
        &self,
        approval_id: &NativeIntegrationApprovalId,
    ) -> NativeIntegrationStoreResult<bool> {
        approval_id.validate().map_err(invalid_domain)?;
        let snapshot = self.read_snapshot().await?;
        approval_consumed_in_transaction(&snapshot, approval_id).await
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
            let record = read_record_from_transaction(&transaction, transaction_id)
                .await?
                .ok_or(NativeIntegrationStoreError::StatusConflict)?;
            if record.status.repository_id != *repository_id {
                return Err(NativeIntegrationStoreError::StatusConflict);
            }
            if record.terminal_receipt.as_ref().is_some_and(|receipt| {
                receipt.status.terminal_outcome
                    != Some(NativeIntegrationTerminalOutcomeV1::NeedsInspection)
            }) {
                return Err(NativeIntegrationStoreError::ReceiptConflict);
            }
            ensure_active_quarantine(
                &transaction,
                repository_id,
                transaction_id,
                record.status.updated_at.0,
            )
            .await
        }
        .await;
        commit_outcome(transaction, outcome).await
    }

    pub(super) async fn begin_write(
        &self,
    ) -> NativeIntegrationStoreResult<GitMutationWriteTransaction<'_>> {
        self.db.begin_write().await.map_err(unavailable)
    }

    pub(super) async fn read_snapshot(
        &self,
    ) -> NativeIntegrationStoreResult<GitMutationReadSnapshot> {
        self.db.read_snapshot().await.map_err(unavailable)
    }
}

/// One canonical projection of a full record row: status + approval from the
/// transaction row, preview joined by identity, receipt left-joined.
const RECORD_SELECT: &str = "SELECT preview.preview_json, txn.approval_json, txn.status_json,
            receipt.receipt_json
     FROM native_integration_transactions AS txn
     JOIN native_integration_previews AS preview
       ON preview.preview_id = txn.preview_id
     LEFT JOIN native_integration_receipts AS receipt
       ON receipt.transaction_id = txn.transaction_id";

pub(super) async fn commit_outcome<T>(
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

async fn insert_preview_if_absent<E>(
    transaction: &E,
    preview: &NativeIntegrationPreviewV1,
) -> NativeIntegrationStoreResult<()>
where
    E: Executor,
{
    let mut rows = transaction
        .query(
            "SELECT preview_json FROM native_integration_previews
             WHERE preview_id = ?1 OR preview_digest = ?2
             ORDER BY preview_id",
            params![preview.preview_id.as_str(), preview.preview_digest.as_str(),],
        )
        .await
        .map_err(unavailable)?;
    let mut existing = Vec::new();
    while let Some(row) = rows.next().await.map_err(unavailable)? {
        existing.push(text(&row, 0, "preview commitment")?);
    }
    drop(rows);
    if !existing.is_empty() {
        let stored: NativeIntegrationPreviewV1 = decode(&existing[0])?;
        return if existing.len() == 1 && stored == *preview {
            Ok(())
        } else {
            Err(NativeIntegrationStoreError::PreviewConflict)
        };
    }
    transaction
        .execute(
            "INSERT INTO native_integration_previews
                (preview_id, preview_digest, repository_id, destination_ref,
                 created_at, expires_at, preview_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                preview.preview_id.as_str(),
                preview.preview_digest.as_str(),
                preview.repository_snapshot.repository_id.as_str(),
                preview.repository_snapshot.destination_ref.as_str(),
                preview.created_at.0,
                preview.expires_at.0,
                encode(preview)?,
            ],
        )
        .await
        .map(|_| ())
        .map_err(unavailable)
}

async fn insert_approval_if_absent<E>(
    transaction: &E,
    approval: &NativeIntegrationApprovalV1,
) -> NativeIntegrationStoreResult<()>
where
    E: Executor,
{
    let mut rows = transaction
        .query(
            "SELECT approval_json FROM native_integration_approvals
             WHERE approval_id = ?1 OR approval_digest = ?2
             ORDER BY approval_id",
            params![
                approval.approval_id.as_str(),
                approval.approval_digest.as_str(),
            ],
        )
        .await
        .map_err(unavailable)?;
    let mut existing = Vec::new();
    while let Some(row) = rows.next().await.map_err(unavailable)? {
        existing.push(text(&row, 0, "approval commitment")?);
    }
    drop(rows);
    if !existing.is_empty() {
        let stored: NativeIntegrationApprovalV1 = decode(&existing[0])?;
        return if existing.len() == 1 && stored == *approval {
            Ok(())
        } else {
            Err(NativeIntegrationStoreError::ApprovalConflict)
        };
    }
    transaction
        .execute(
            "INSERT INTO native_integration_approvals
                (approval_id, approval_digest, preview_id, preview_digest,
                 principal, issued_at, expires_at, approval_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                approval.approval_id.as_str(),
                approval.approval_digest.as_str(),
                approval.preview_id.as_str(),
                approval.preview_digest.as_str(),
                approval.principal.as_str(),
                approval.issued_at.0,
                approval.expires_at.0,
                encode(approval)?,
            ],
        )
        .await
        .map(|_| ())
        .map_err(unavailable)
}

async fn read_preview_from_transaction<Q>(
    transaction: &Q,
    preview_id: &NativeIntegrationPreviewId,
) -> NativeIntegrationStoreResult<Option<NativeIntegrationPreviewV1>>
where
    Q: QueryExecutor,
{
    let mut rows = transaction
        .query(
            "SELECT preview_json FROM native_integration_previews WHERE preview_id = ?1",
            params![preview_id.as_str()],
        )
        .await
        .map_err(unavailable)?;
    let Some(row) = rows.next().await.map_err(unavailable)? else {
        return Ok(None);
    };
    let preview: NativeIntegrationPreviewV1 = decode(&text(&row, 0, "preview commitment")?)?;
    if preview.preview_id != *preview_id {
        return Err(invalid(
            "native integration preview commitment key does not bind its payload",
        ));
    }
    preview.validate().map_err(invalid_domain)?;
    if rows.next().await.map_err(unavailable)?.is_some() {
        return Err(invalid("duplicate native integration preview commitment"));
    }
    Ok(Some(preview))
}

async fn read_approval_from_transaction<Q>(
    transaction: &Q,
    approval_id: &NativeIntegrationApprovalId,
) -> NativeIntegrationStoreResult<Option<NativeIntegrationApprovalV1>>
where
    Q: QueryExecutor,
{
    let mut rows = transaction
        .query(
            "SELECT approval_json FROM native_integration_approvals WHERE approval_id = ?1",
            params![approval_id.as_str()],
        )
        .await
        .map_err(unavailable)?;
    let Some(row) = rows.next().await.map_err(unavailable)? else {
        return Ok(None);
    };
    let approval: NativeIntegrationApprovalV1 = decode(&text(&row, 0, "approval commitment")?)?;
    if approval.approval_id != *approval_id {
        return Err(invalid(
            "native integration approval commitment key does not bind its payload",
        ));
    }
    approval.validate().map_err(invalid_domain)?;
    if rows.next().await.map_err(unavailable)?.is_some() {
        return Err(invalid("duplicate native integration approval commitment"));
    }
    Ok(Some(approval))
}

async fn read_status_from_transaction<Q>(
    transaction: &Q,
    transaction_id: &NativeIntegrationTransactionId,
) -> NativeIntegrationStoreResult<Option<NativeIntegrationTransactionStatusV1>>
where
    Q: QueryExecutor,
{
    let mut rows = transaction
        .query(
            "SELECT status_json FROM native_integration_transactions WHERE transaction_id = ?1",
            params![transaction_id.as_str()],
        )
        .await
        .map_err(unavailable)?;
    let Some(row) = rows.next().await.map_err(unavailable)? else {
        return Ok(None);
    };
    let status: NativeIntegrationTransactionStatusV1 =
        decode(&text(&row, 0, "transaction status")?)?;
    if status.transaction_id != *transaction_id {
        return Err(invalid(
            "native integration transaction key does not bind its payload",
        ));
    }
    status.validate().map_err(invalid_domain)?;
    if rows.next().await.map_err(unavailable)?.is_some() {
        return Err(invalid("duplicate native integration transaction"));
    }
    Ok(Some(status))
}

async fn read_record_from_transaction<Q>(
    transaction: &Q,
    transaction_id: &NativeIntegrationTransactionId,
) -> NativeIntegrationStoreResult<Option<NativeIntegrationRecordV1>>
where
    Q: QueryExecutor,
{
    let mut rows = transaction
        .query(
            &format!("{RECORD_SELECT} WHERE txn.transaction_id = ?1"),
            params![transaction_id.as_str()],
        )
        .await
        .map_err(unavailable)?;
    let Some(row) = rows.next().await.map_err(unavailable)? else {
        return Ok(None);
    };
    let record = decode_record(&row)?;
    if record.status.transaction_id != *transaction_id {
        return Err(invalid(
            "native integration transaction key does not bind its payload",
        ));
    }
    if rows.next().await.map_err(unavailable)?.is_some() {
        return Err(invalid("duplicate native integration transaction"));
    }
    Ok(Some(record))
}

async fn read_receipt_from_transaction<Q>(
    transaction: &Q,
    transaction_id: &NativeIntegrationTransactionId,
) -> NativeIntegrationStoreResult<Option<NativeIntegrationReceiptV1>>
where
    Q: QueryExecutor,
{
    let mut rows = transaction
        .query(
            "SELECT receipt_json FROM native_integration_receipts WHERE transaction_id = ?1",
            params![transaction_id.as_str()],
        )
        .await
        .map_err(unavailable)?;
    let Some(row) = rows.next().await.map_err(unavailable)? else {
        return Ok(None);
    };
    let receipt: NativeIntegrationReceiptV1 = decode(&text(&row, 0, "terminal receipt")?)?;
    if receipt.status.transaction_id != *transaction_id {
        return Err(invalid(
            "native integration receipt key does not bind its payload",
        ));
    }
    receipt.validate().map_err(invalid_domain)?;
    if rows.next().await.map_err(unavailable)?.is_some() {
        return Err(invalid("duplicate native integration receipt"));
    }
    Ok(Some(receipt))
}

fn decode_record(row: &Row) -> NativeIntegrationStoreResult<NativeIntegrationRecordV1> {
    let preview: NativeIntegrationPreviewV1 = decode(&text(row, 0, "record preview")?)?;
    let approval: NativeIntegrationApprovalV1 = decode(&text(row, 1, "record approval")?)?;
    let status: NativeIntegrationTransactionStatusV1 = decode(&text(row, 2, "record status")?)?;
    let terminal_receipt = optional_text(row, 3, "record receipt")?
        .map(|value| decode(&value))
        .transpose()?;
    let record = NativeIntegrationRecordV1 {
        preview,
        approval,
        status,
        terminal_receipt,
    };
    record.validate().map_err(invalid_domain)?;
    Ok(record)
}

async fn approval_consumed_in_transaction<Q>(
    transaction: &Q,
    approval_id: &NativeIntegrationApprovalId,
) -> NativeIntegrationStoreResult<bool>
where
    Q: QueryExecutor,
{
    let mut rows = transaction
        .query(
            "SELECT 1 FROM native_integration_transactions WHERE approval_id = ?1",
            params![approval_id.as_str()],
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
) -> NativeIntegrationStoreResult<bool>
where
    Q: QueryExecutor,
{
    let mut rows = transaction
        .query(
            "SELECT 1 FROM native_integration_repository_quarantines
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

/// Create the durable fence once; an existing fence for the same transaction
/// is already the evidence this write wants.
async fn ensure_active_quarantine<E>(
    transaction: &E,
    repository_id: &RepositoryId,
    transaction_id: &NativeIntegrationTransactionId,
    created_at: i64,
) -> NativeIntegrationStoreResult<()>
where
    E: Executor,
{
    transaction
        .execute(
            "INSERT INTO native_integration_repository_quarantines
                (repository_id, transaction_id, active, created_at)
             VALUES (?1, ?2, 1, ?3)
             ON CONFLICT(repository_id, transaction_id) DO NOTHING",
            params![repository_id.as_str(), transaction_id.as_str(), created_at,],
        )
        .await
        .map(|_| ())
        .map_err(unavailable)
}

async fn update_status_row<E>(
    transaction: &E,
    replacement: &NativeIntegrationTransactionStatusV1,
    expected_phase_revision: u64,
) -> NativeIntegrationStoreResult<u64>
where
    E: Executor,
{
    transaction
        .execute(
            "UPDATE native_integration_transactions
             SET phase = ?1, phase_revision = ?2, cancellation_requested = ?3,
                 terminal_outcome = ?4, updated_at = ?5, status_json = ?6
             WHERE transaction_id = ?7 AND phase_revision = ?8",
            params![
                phase_code(replacement.phase),
                phase_revision_i64(replacement.phase_revision)?,
                i64::from(replacement.cancellation_requested),
                replacement.terminal_outcome.map(terminal_outcome_code),
                replacement.updated_at.0,
                encode(replacement)?,
                replacement.transaction_id.as_str(),
                phase_revision_i64(expected_phase_revision)?,
            ],
        )
        .await
        .map_err(unavailable)
}

/// A status replacement may advance phase, revision, cancellation, and
/// timestamps; it can never rebind the transaction's immutable identity.
fn status_transition_matches(
    current: &NativeIntegrationTransactionStatusV1,
    expected_phase_revision: u64,
    replacement: &NativeIntegrationTransactionStatusV1,
) -> bool {
    current.phase_revision == expected_phase_revision
        && replacement.phase_revision == expected_phase_revision.saturating_add(1)
        && current.terminal_outcome.is_none()
        && current.phase <= replacement.phase
        && current.transaction_id == replacement.transaction_id
        && current.preview_id == replacement.preview_id
        && current.preview_digest == replacement.preview_digest
        && current.approval_id == replacement.approval_id
        && current.repository_id == replacement.repository_id
        && current.destination_ref == replacement.destination_ref
        && current.expected_destination_tip == replacement.expected_destination_tip
}

fn phase_revision_i64(phase_revision: u64) -> NativeIntegrationStoreResult<i64> {
    i64::try_from(phase_revision)
        .map_err(|_| invalid("native integration phase revision exceeds SQLite range"))
}

fn phase_code(phase: NativeIntegrationPhaseV1) -> &'static str {
    match phase {
        NativeIntegrationPhaseV1::Prepared => "prepared",
        NativeIntegrationPhaseV1::CandidateVerified => "candidate_verified",
        NativeIntegrationPhaseV1::RefCommitStarted => "ref_commit_started",
        NativeIntegrationPhaseV1::FinalStateVerification => "final_state_verification",
        NativeIntegrationPhaseV1::Terminal => "terminal",
    }
}

fn terminal_outcome_code(outcome: NativeIntegrationTerminalOutcomeV1) -> &'static str {
    match outcome {
        NativeIntegrationTerminalOutcomeV1::Committed => "committed",
        NativeIntegrationTerminalOutcomeV1::AbortedNoChange => "aborted_no_change",
        NativeIntegrationTerminalOutcomeV1::RolledBack => "rolled_back",
        NativeIntegrationTerminalOutcomeV1::NeedsInspection => "needs_inspection",
    }
}

pub(super) fn encode<T: serde::Serialize>(value: &T) -> NativeIntegrationStoreResult<String> {
    serde_json::to_string(value).map_err(|error| invalid(error.to_string()))
}

pub(super) fn decode<T: serde::de::DeserializeOwned>(
    value: &str,
) -> NativeIntegrationStoreResult<T> {
    serde_json::from_str(value).map_err(|error| invalid(error.to_string()))
}

pub(super) fn text(
    row: &Row,
    column: i32,
    field: &'static str,
) -> NativeIntegrationStoreResult<String> {
    row.get::<String>(column)
        .map_err(|error| invalid(format!("read {field}: {error}")))
}

fn optional_text(
    row: &Row,
    column: i32,
    field: &'static str,
) -> NativeIntegrationStoreResult<Option<String>> {
    row.get::<Option<String>>(column)
        .map_err(|error| invalid(format!("read {field}: {error}")))
}

pub(super) fn invalid(message: impl Into<String>) -> NativeIntegrationStoreError {
    NativeIntegrationStoreError::InvalidData(message.into())
}

#[allow(clippy::needless_pass_by_value)]
pub(super) fn invalid_domain(error: tracedecay_domain::DomainError) -> NativeIntegrationStoreError {
    NativeIntegrationStoreError::InvalidData(error.to_string())
}

pub(super) fn unavailable<T>(_error: T) -> NativeIntegrationStoreError {
    NativeIntegrationStoreError::Unavailable
}
