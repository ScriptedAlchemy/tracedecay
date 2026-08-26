//! Durable journal transitions for daemon-owned Git index mutations.

use thiserror::Error;
use tracedecay_domain::{
    DomainError, GitIndexIdempotencyKey, GitIndexJournalPhaseV1, GitIndexReceiptOutcomeV1,
    GitIndexTransactionJournalV1, GitIndexTransactionReceiptV1, UtcMicros,
};
use tracedecay_store::{
    GitIndexTransactionBeginRequestV1, GitIndexTransactionBeginResultV1, GitIndexTransactionStore,
    GitIndexTransactionStoreError, GitIndexTransactionTerminalWriteV1,
};

#[derive(Debug, Error)]
pub(crate) enum GitIndexJournalError {
    #[error(transparent)]
    Domain(#[from] DomainError),
    #[error(transparent)]
    Store(#[from] GitIndexTransactionStoreError),
}

pub(crate) struct DurableGitIndexJournal<'a, S> {
    store: &'a S,
}

impl<'a, S> DurableGitIndexJournal<'a, S>
where
    S: GitIndexTransactionStore,
{
    pub(crate) fn new(store: &'a S) -> Self {
        Self { store }
    }

    pub(crate) fn begin_or_replay(
        &self,
        request: GitIndexTransactionBeginRequestV1,
    ) -> Result<GitIndexTransactionBeginResultV1, GitIndexJournalError> {
        request.validate()?;
        Ok(self.store.begin_or_replay(request)?)
    }

    pub(crate) fn advance(
        &self,
        idempotency_key: &GitIndexIdempotencyKey,
        current: &GitIndexTransactionJournalV1,
        next: GitIndexJournalPhaseV1,
        updated_at: UtcMicros,
    ) -> Result<GitIndexTransactionJournalV1, GitIndexJournalError> {
        let mut replacement = current.clone();
        let expected_phase_epoch = replacement.phase_epoch;
        replacement.advance(next, updated_at)?;
        Ok(self.store.compare_and_swap_journal(
            idempotency_key,
            expected_phase_epoch,
            replacement,
        )?)
    }

    pub(crate) fn write_terminal(
        &self,
        idempotency_key: &GitIndexIdempotencyKey,
        current: &GitIndexTransactionJournalV1,
        receipt: GitIndexTransactionReceiptV1,
        updated_at: UtcMicros,
    ) -> Result<GitIndexTransactionReceiptV1, GitIndexJournalError> {
        let terminal = match receipt.outcome {
            GitIndexReceiptOutcomeV1::Committed => GitIndexJournalPhaseV1::Committed,
            GitIndexReceiptOutcomeV1::AbortedNoChange => GitIndexJournalPhaseV1::AbortedNoChange,
            GitIndexReceiptOutcomeV1::NeedsInspection => GitIndexJournalPhaseV1::NeedsInspection,
        };
        let mut journal = current.clone();
        journal.advance(terminal, updated_at)?;
        let write = GitIndexTransactionTerminalWriteV1 {
            idempotency_key: idempotency_key.clone(),
            expected_phase_epoch: journal.phase_epoch,
            journal,
            receipt,
        };
        write.validate()?;
        Ok(self.store.write_terminal(write)?)
    }
}
