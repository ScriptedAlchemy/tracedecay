//! Crash/restart recovery for Git index transactions.
//!
//! Recovery observes native state and journal evidence, then proves one
//! terminal receipt or quarantines the repository. It never invokes stage,
//! unstage, or commit a second time.

use thiserror::Error;
use tracedecay_domain::{
    DomainError, GitIndexJournalPhaseV1, GitIndexReceiptOutcomeV1, GitIndexTransactionJournalV1,
    GitIndexTransactionReceiptV1, RepositoryId, UtcMicros,
};
use tracedecay_store::{
    GitIndexTransactionRecordV1, GitIndexTransactionStore, GitIndexTransactionStoreError,
    GitIndexTransactionTerminalWriteV1,
};

#[derive(Debug, Error)]
pub(crate) enum GitIndexRecoveryError {
    #[error(transparent)]
    Domain(#[from] DomainError),
    #[error(transparent)]
    Store(#[from] GitIndexTransactionStoreError),
    #[error("native Git recovery could not prove a terminal outcome")]
    Indeterminate,
}

/// Native reconciliation sees the real repository state but cannot initiate a
/// fresh mutation. A `NeedsInspection` receipt is the required outcome when
/// state drift prevents proof.
pub(crate) trait GitIndexRecoveryExecutor {
    fn reconcile(
        &self,
        record: &GitIndexTransactionRecordV1,
    ) -> Result<GitIndexTransactionReceiptV1, GitIndexRecoveryError>;
}

pub(crate) struct GitIndexRecoveryCoordinator<'a, S, N> {
    store: &'a S,
    native: &'a N,
}

impl<'a, S, N> GitIndexRecoveryCoordinator<'a, S, N>
where
    S: GitIndexTransactionStore,
    N: GitIndexRecoveryExecutor,
{
    pub(crate) fn new(store: &'a S, native: &'a N) -> Self {
        Self { store, native }
    }

    pub(crate) fn recover_repository(
        &self,
        repository_id: &RepositoryId,
        observed_at: UtcMicros,
    ) -> Result<Vec<GitIndexTransactionReceiptV1>, GitIndexRecoveryError> {
        let records = self.store.recovery_candidates(repository_id)?;
        let mut receipts = Vec::with_capacity(records.len());
        for record in records {
            record.validate()?;
            if let Some(receipt) = &record.terminal_receipt {
                receipts.push(receipt.clone());
                continue;
            }
            let Ok(receipt) = self.native.reconcile(&record) else {
                self.store
                    .quarantine_repository(repository_id, &record.journal.transaction_id)?;
                return Err(GitIndexRecoveryError::Indeterminate);
            };
            if receipt.validate().is_err() {
                self.store
                    .quarantine_repository(repository_id, &record.journal.transaction_id)?;
                return Err(GitIndexRecoveryError::Indeterminate);
            }
            if receipt.transaction_id != record.journal.transaction_id
                || receipt.preview_id != record.preview.preview_id
                || receipt.operation != record.preview.operation
            {
                self.store
                    .quarantine_repository(repository_id, &record.journal.transaction_id)?;
                return Err(GitIndexRecoveryError::Indeterminate);
            }
            let Ok(preterminal_journal) =
                advance_to_terminal(self.store, &record, receipt.outcome, observed_at)
            else {
                self.store
                    .quarantine_repository(repository_id, &record.journal.transaction_id)?;
                return Err(GitIndexRecoveryError::Indeterminate);
            };
            let mut terminal_journal = preterminal_journal;
            terminal_journal.advance(terminal_phase(receipt.outcome), observed_at)?;
            let write = GitIndexTransactionTerminalWriteV1 {
                idempotency_key: record.idempotency_key.clone(),
                expected_phase_epoch: terminal_journal.phase_epoch,
                journal: terminal_journal,
                receipt: receipt.clone(),
            };
            write.validate()?;
            receipts.push(self.store.write_terminal(write)?);
        }
        Ok(receipts)
    }
}

fn advance_to_terminal<S>(
    store: &S,
    record: &GitIndexTransactionRecordV1,
    outcome: GitIndexReceiptOutcomeV1,
    observed_at: UtcMicros,
) -> Result<GitIndexTransactionJournalV1, GitIndexRecoveryError>
where
    S: GitIndexTransactionStore,
{
    let mut journal = record.journal.clone();
    let phases: &[GitIndexJournalPhaseV1] = match outcome {
        GitIndexReceiptOutcomeV1::AbortedNoChange | GitIndexReceiptOutcomeV1::NeedsInspection => {
            &[]
        }
        GitIndexReceiptOutcomeV1::Committed => match journal.phase {
            GitIndexJournalPhaseV1::NativeApplyStarted => {
                if journal.operation
                    == tracedecay_domain::GitIndexTransactionOperationV1::CommitIndex
                {
                    &[
                        GitIndexJournalPhaseV1::IndexCommitted,
                        GitIndexJournalPhaseV1::RefCommitted,
                        GitIndexJournalPhaseV1::Verifying,
                    ]
                } else {
                    &[
                        GitIndexJournalPhaseV1::IndexCommitted,
                        GitIndexJournalPhaseV1::Verifying,
                    ]
                }
            }
            GitIndexJournalPhaseV1::IndexCommitted
                if journal.operation
                    == tracedecay_domain::GitIndexTransactionOperationV1::CommitIndex =>
            {
                &[
                    GitIndexJournalPhaseV1::RefCommitted,
                    GitIndexJournalPhaseV1::Verifying,
                ]
            }
            GitIndexJournalPhaseV1::IndexCommitted | GitIndexJournalPhaseV1::RefCommitted => {
                &[GitIndexJournalPhaseV1::Verifying]
            }
            GitIndexJournalPhaseV1::Verifying => &[],
            _ => return Err(GitIndexRecoveryError::Indeterminate),
        },
    };

    for phase in phases {
        let expected_phase_epoch = journal.phase_epoch;
        journal.advance(*phase, observed_at)?;
        journal = store.compare_and_swap_journal(
            &record.idempotency_key,
            expected_phase_epoch,
            journal,
        )?;
    }
    Ok(journal)
}

const fn terminal_phase(outcome: GitIndexReceiptOutcomeV1) -> GitIndexJournalPhaseV1 {
    match outcome {
        GitIndexReceiptOutcomeV1::Committed => GitIndexJournalPhaseV1::Committed,
        GitIndexReceiptOutcomeV1::AbortedNoChange => GitIndexJournalPhaseV1::AbortedNoChange,
        GitIndexReceiptOutcomeV1::NeedsInspection => GitIndexJournalPhaseV1::NeedsInspection,
    }
}
