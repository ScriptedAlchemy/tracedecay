//! Crash/restart recovery for Git index transactions.
//!
//! Recovery observes native state and journal evidence, then proves one
//! terminal receipt or quarantines the repository. It never invokes stage,
//! unstage, or commit a second time.

use thiserror::Error;
use tracedecay_domain::{
    DomainError, GitIndexJournalPhaseV1, GitIndexReceiptId, GitIndexReceiptOutcomeV1,
    GitIndexTransactionJournalV1, GitIndexTransactionReceiptV1, RepositoryId, UtcMicros,
};
use tracedecay_store::{
    GitIndexTransactionRecordV1, GitIndexTransactionStore, GitIndexTransactionStoreError,
    GitIndexTransactionTerminalWriteV1,
};

#[derive(Debug, Error)]
pub enum GitIndexRecoveryError {
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
pub trait GitIndexRecoveryExecutor {
    fn reconcile(
        &self,
        record: &GitIndexTransactionRecordV1,
    ) -> Result<GitIndexTransactionReceiptV1, GitIndexRecoveryError>;
}

pub struct GitIndexRecoveryCoordinator<'a, S, N> {
    store: &'a S,
    native: &'a N,
}

impl<'a, S, N> GitIndexRecoveryCoordinator<'a, S, N>
where
    S: GitIndexTransactionStore,
    N: GitIndexRecoveryExecutor,
{
    pub fn new(store: &'a S, native: &'a N) -> Self {
        Self { store, native }
    }

    pub fn recover_repository(
        &self,
        repository_id: &RepositoryId,
        observed_at: UtcMicros,
    ) -> Result<Vec<GitIndexTransactionReceiptV1>, GitIndexRecoveryError> {
        let records = self.store.recovery_candidates(repository_id)?;
        let mut receipts = Vec::with_capacity(records.len());
        for record in records {
            receipts.push(self.recover_record(&record, observed_at)?);
        }
        Ok(receipts)
    }

    /// Reconcile exactly one durable record. This is shared by startup and an
    /// admitted transaction whose native boundary became ambiguous; neither
    /// caller is allowed to invoke native apply a second time.
    #[hotpath::measure(label = "daemon.git.tx.recover_record")]
    pub fn recover_record(
        &self,
        record: &GitIndexTransactionRecordV1,
        observed_at: UtcMicros,
    ) -> Result<GitIndexTransactionReceiptV1, GitIndexRecoveryError> {
        record.validate()?;

        if let Some(original_receipt) = &record.terminal_receipt {
            if original_receipt.outcome != GitIndexReceiptOutcomeV1::NeedsInspection {
                hotpath::gauge!("daemon.git.tx.recovery.replayed_total").inc(1_u64);
                return Ok(original_receipt.clone());
            }
            let proof = self.reconcile_or_quarantine(record, observed_at)?;
            // A terminal inspection receipt has no post-boundary phase that
            // can distinguish a coincidental candidate from our mutation.
            // Exact restoration to the old snapshot is the only automatic
            // clear proof; any apparent commit remains quarantined for human
            // inspection rather than overwriting immutable terminal truth.
            if proof.outcome != GitIndexReceiptOutcomeV1::AbortedNoChange
                || !receipt_binds_record(&proof, record)
            {
                quarantine(self.store, record)?;
                return Err(GitIndexRecoveryError::Indeterminate);
            }
            let proof = recovery_proof_at(&proof, record, observed_at)?;
            self.store.clear_repository_quarantine(
                &record.journal.repository_id,
                &record.journal.transaction_id,
                proof.clone(),
            )?;
            hotpath::gauge!("daemon.git.tx.recovery.recovered_total").inc(1_u64);
            return Ok(proof);
        }

        let receipt = self.reconcile_or_quarantine(record, observed_at)?;
        if !receipt_binds_record(&receipt, record) {
            quarantine(self.store, record)?;
            return Err(GitIndexRecoveryError::Indeterminate);
        }
        let receipt = recovery_proof_at(&receipt, record, observed_at)?;

        let Ok(preterminal) = advance_to_terminal(self.store, record, receipt.outcome, observed_at)
        else {
            quarantine(self.store, record)?;
            return Err(GitIndexRecoveryError::Indeterminate);
        };
        if receipt.outcome == GitIndexReceiptOutcomeV1::NeedsInspection {
            // Persist the blocking truth first. If the subsequent atomic
            // receipt write fails, the repository still remains fenced.
            quarantine(self.store, record)?;
        }
        let mut terminal_journal = preterminal;
        if terminal_journal
            .advance(terminal_phase(receipt.outcome), observed_at)
            .is_err()
        {
            quarantine(self.store, record)?;
            return Err(GitIndexRecoveryError::Indeterminate);
        }
        let write = GitIndexTransactionTerminalWriteV1 {
            idempotency_key: record.idempotency_key.clone(),
            expected_phase_epoch: terminal_journal.phase_epoch,
            journal: terminal_journal,
            receipt: receipt.clone(),
        };
        if write.validate().is_err() {
            quarantine(self.store, record)?;
            return Err(GitIndexRecoveryError::Indeterminate);
        }
        if let Ok(stored) = self.store.write_terminal(write) {
            hotpath::gauge!("daemon.git.tx.recovery.recovered_total").inc(1_u64);
            Ok(stored)
        } else {
            quarantine(self.store, record)?;
            Err(GitIndexRecoveryError::Indeterminate)
        }
    }

    fn reconcile_or_quarantine(
        &self,
        record: &GitIndexTransactionRecordV1,
        observed_at: UtcMicros,
    ) -> Result<GitIndexTransactionReceiptV1, GitIndexRecoveryError> {
        // Native reconciliation observes real repository state; measuring it
        // apart from `recover_record` separates git observation cost from the
        // journal-advance and terminal-write I/O around it.
        if let Ok(receipt) = hotpath::measure_block!(
            "daemon.git.tx.recovery.reconcile",
            self.native.reconcile(record)
        ) {
            Ok(receipt)
        } else {
            quarantine(self.store, record)?;
            unobserved_needs_inspection(record, observed_at)
        }
    }
}

fn recovery_proof_at(
    proof: &GitIndexTransactionReceiptV1,
    record: &GitIndexTransactionRecordV1,
    observed_at: UtcMicros,
) -> Result<GitIndexTransactionReceiptV1, GitIndexRecoveryError> {
    GitIndexTransactionReceiptV1::new_with_final_snapshot(
        proof.receipt_id.clone(),
        record.journal.transaction_id.clone(),
        &record.preview,
        proof
            .final_snapshot_captured
            .then(|| proof.final_snapshot_digest.clone()),
        proof.new_index_tree.clone(),
        proof.new_head.clone(),
        proof.created_commit.clone(),
        proof.outcome,
        observed_at,
    )
    .map_err(GitIndexRecoveryError::Domain)
}

fn unobserved_needs_inspection(
    record: &GitIndexTransactionRecordV1,
    observed_at: UtcMicros,
) -> Result<GitIndexTransactionReceiptV1, GitIndexRecoveryError> {
    let receipt_id = GitIndexReceiptId::new(format!(
        "git-index-receipt.v1.{}",
        record.journal.transaction_id.as_str()
    ))?;
    GitIndexTransactionReceiptV1::new_with_final_snapshot(
        receipt_id,
        record.journal.transaction_id.clone(),
        &record.preview,
        None,
        record.preview.repository_snapshot.index.tree_id.clone(),
        record.preview.repository_snapshot.head.commit().cloned(),
        None,
        GitIndexReceiptOutcomeV1::NeedsInspection,
        observed_at,
    )
    .map_err(GitIndexRecoveryError::Domain)
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
    if !journal
        .phase
        .permits_recovered_outcome(journal.operation, outcome)
    {
        return Err(GitIndexRecoveryError::Indeterminate);
    }
    let phases: &[GitIndexJournalPhaseV1] = match outcome {
        GitIndexReceiptOutcomeV1::AbortedNoChange | GitIndexReceiptOutcomeV1::NeedsInspection => {
            &[]
        }
        GitIndexReceiptOutcomeV1::Committed => match journal.phase {
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

fn receipt_binds_record(
    receipt: &GitIndexTransactionReceiptV1,
    record: &GitIndexTransactionRecordV1,
) -> bool {
    record.receipt_binds_preview(receipt)
}

fn quarantine<S>(
    store: &S,
    record: &GitIndexTransactionRecordV1,
) -> Result<(), GitIndexRecoveryError>
where
    S: GitIndexTransactionStore,
{
    hotpath::gauge!("daemon.git.tx.recovery.quarantined_total").inc(1_u64);
    store.quarantine_repository(
        &record.journal.repository_id,
        &record.journal.transaction_id,
    )?;
    Ok(())
}

const fn terminal_phase(outcome: GitIndexReceiptOutcomeV1) -> GitIndexJournalPhaseV1 {
    match outcome {
        GitIndexReceiptOutcomeV1::Committed => GitIndexJournalPhaseV1::Committed,
        GitIndexReceiptOutcomeV1::AbortedNoChange => GitIndexJournalPhaseV1::AbortedNoChange,
        GitIndexReceiptOutcomeV1::NeedsInspection => GitIndexJournalPhaseV1::NeedsInspection,
    }
}
