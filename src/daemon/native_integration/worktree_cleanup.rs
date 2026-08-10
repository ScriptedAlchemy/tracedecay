//! Durable inspect-confirm-remove-reconcile execution for native worktrees.

use std::path::Path;
use std::process::{Command, Output};

use tracedecay_application::git::{
    NativeWorktreeTargetV1, WorktreeCleanupReconcileRequestV1, WorktreeCleanupReconciliationV1,
    WorktreeCleanupRemovalV1, WorktreeCleanupRemoveRequestV1, WorktreeContractError,
    WorktreePresenceV1, worktree_confirmation_digest,
};
use tracedecay_application::{AuthorizedScopeSet, CancellationSignal};
use tracedecay_domain::{
    NativeWorktreeCleanupCommandV1, NativeWorktreeCleanupOutcomeV1, NativeWorktreeCleanupPhaseV1,
    NativeWorktreeCleanupReceiptV1, NativeWorktreeCleanupTransactionV1, UtcMicros,
};
use tracedecay_runtime_core::git::{GitCommandBounds, bounded_command_output, git_program};
use tracedecay_store::{
    NativeIntegrationStore, NativeIntegrationStoreError, NativeWorktreeCleanupBeginResultV1,
};

use super::worktree::{DaemonNativeWorktreeAuthority, WorktreeCleanupAdmissionV1, zero_digest};

enum CleanupNativeStateV1 {
    Removed,
    Present,
    Foreign,
    Uncertain,
}

impl DaemonNativeWorktreeAuthority {
    pub(super) fn remove_cleanup(
        &self,
        request: &WorktreeCleanupRemoveRequestV1,
        scope_set: &AuthorizedScopeSet,
        cancellation: &CancellationSignal,
    ) -> Result<WorktreeCleanupRemovalV1, WorktreeContractError> {
        if cancellation.is_cancelled() {
            return Ok(WorktreeCleanupRemovalV1::Unavailable);
        }
        let expected_confirmation = worktree_confirmation_digest(
            &request.target,
            &request.inspection_digest,
            request.confirmed_at,
        )?;
        if expected_confirmation != request.confirmation_digest {
            return Ok(WorktreeCleanupRemovalV1::Stale);
        }
        let (_, worktree_root) = self.target_root(&request.target, scope_set)?;
        if worktree_root == self.repository_root {
            return Ok(WorktreeCleanupRemovalV1::Denied);
        }

        let observed_at = now_at_least(request.confirmed_at);
        let initial = self.observe_target(&request.target, scope_set, observed_at, false)?;
        if initial.presence == WorktreePresenceV1::Stale {
            return Ok(if self.linked_admin_present(&worktree_root)? {
                WorktreeCleanupRemovalV1::DurabilityUncertain
            } else {
                WorktreeCleanupRemovalV1::AlreadyRemoved {
                    confirmation_digest: request.confirmation_digest.clone(),
                    observed_at,
                }
            });
        }
        if initial.presence == WorktreePresenceV1::Foreign {
            return Ok(WorktreeCleanupRemovalV1::Denied);
        }
        if initial.presence != WorktreePresenceV1::Present
            || initial.inspection_digest != request.inspection_digest
        {
            return Ok(WorktreeCleanupRemovalV1::Stale);
        }
        if !initial.removal_eligible() {
            return Ok(WorktreeCleanupRemovalV1::Denied);
        }

        let Some(admission) = self.holder_fence.try_cleanup(&worktree_root) else {
            return Ok(WorktreeCleanupRemovalV1::Denied);
        };
        let fenced = self.observe_target(
            &request.target,
            scope_set,
            crate::daemon_client::invocation_now_micros(),
            true,
        )?;
        if fenced.presence != WorktreePresenceV1::Present
            || fenced.inspection_digest != request.inspection_digest
            || !fenced.removal_eligible()
        {
            return Ok(if fenced.presence == WorktreePresenceV1::Foreign {
                WorktreeCleanupRemovalV1::Denied
            } else {
                WorktreeCleanupRemovalV1::Stale
            });
        }

        let worktree_id = request
            .target
            .worktree_id()
            .ok_or(WorktreeContractError::Inconsistent {
                field: "cleanup target worktree",
            })?
            .clone();
        let prepared_at = now_at_least(request.confirmed_at);
        let prepared = NativeWorktreeCleanupTransactionV1 {
            scope_set_id: request.scope_set_id.clone(),
            scope_set_revision: request.scope_set_revision,
            scope_set_digest: request.scope_set_digest.clone(),
            inspection_digest: request.inspection_digest.clone(),
            confirmed_at: request.confirmed_at,
            confirmation_digest: request.confirmation_digest.clone(),
            command: NativeWorktreeCleanupCommandV1 {
                project_id: self.project_id.clone(),
                repository_id: self.repository_id.clone(),
                worktree_id,
                repository_root: self.repository_root.clone(),
                worktree_root: worktree_root.clone(),
            },
            phase: NativeWorktreeCleanupPhaseV1::Prepared,
            phase_revision: 1,
            prepared_at,
            updated_at: prepared_at,
            terminal_outcome: None,
            transaction_digest: zero_digest()?,
        }
        .seal()?;

        let transaction = match self.store.begin_worktree_cleanup(prepared) {
            Ok(NativeWorktreeCleanupBeginResultV1::Started(transaction)) => *transaction,
            Ok(NativeWorktreeCleanupBeginResultV1::Replay(receipt)) => {
                return Ok(removal_from_outcome(
                    receipt.transaction.terminal_outcome,
                    &receipt.transaction.confirmation_digest,
                    receipt.completed_at,
                ));
            }
            Ok(NativeWorktreeCleanupBeginResultV1::RecoveryRequired(transaction)) => {
                return self.reconcile_for_remove(*transaction, scope_set, admission, cancellation);
            }
            Err(error) => return Ok(removal_store_error(error)),
        };

        if !cancellation.try_begin_commit() {
            return match self
                .write_terminal(transaction, NativeWorktreeCleanupOutcomeV1::AbortedNoChange)
            {
                Ok(_) => Ok(WorktreeCleanupRemovalV1::Unavailable),
                Err(outcome) => {
                    admission.retain_recovery();
                    Ok(outcome)
                }
            };
        }
        self.execute_remove(transaction, scope_set, admission)
    }

    pub(super) fn reconcile_cleanup(
        &self,
        request: &WorktreeCleanupReconcileRequestV1,
        scope_set: &AuthorizedScopeSet,
        cancellation: &CancellationSignal,
    ) -> Result<WorktreeCleanupReconciliationV1, WorktreeContractError> {
        if cancellation.is_cancelled() {
            return Ok(WorktreeCleanupReconciliationV1::Unavailable);
        }
        let transaction = match self
            .store
            .read_worktree_cleanup(&request.confirmation_digest)
        {
            Ok(Some(transaction)) => transaction,
            Ok(None) => return Ok(WorktreeCleanupReconciliationV1::Stale),
            Err(error) => return Ok(reconciliation_store_error(error)),
        };
        if transaction.scope_set_id != request.scope_set_id
            || transaction.scope_set_revision != request.scope_set_revision
            || transaction.scope_set_digest != request.scope_set_digest
            || transaction.command.project_id != self.project_id
            || transaction.command.repository_id != self.repository_id
            || transaction.command.repository_root != self.repository_root
            || request.target.worktree_id() != Some(&transaction.command.worktree_id)
            || request.target.project_id() != &transaction.command.project_id
            || request.target.repository_id() != &transaction.command.repository_id
        {
            return Ok(WorktreeCleanupReconciliationV1::Stale);
        }
        let (_, authorized_root) = self.target_root(&request.target, scope_set)?;
        if authorized_root != transaction.command.worktree_root {
            return Ok(WorktreeCleanupReconciliationV1::Stale);
        }
        if transaction.phase == NativeWorktreeCleanupPhaseV1::Terminal {
            return Ok(reconciliation_from_outcome(
                transaction.terminal_outcome,
                &transaction.confirmation_digest,
                transaction.updated_at,
            ));
        }
        let Some(admission) = self.holder_fence.take_recovery(&authorized_root) else {
            return Ok(WorktreeCleanupReconciliationV1::Unavailable);
        };
        if cancellation.is_cancelled() {
            admission.retain_recovery();
            return Ok(WorktreeCleanupReconciliationV1::Unavailable);
        }
        self.reconcile_transaction(transaction, scope_set, admission)
    }

    fn execute_remove(
        &self,
        transaction: NativeWorktreeCleanupTransactionV1,
        scope_set: &AuthorizedScopeSet,
        admission: WorktreeCleanupAdmissionV1,
    ) -> Result<WorktreeCleanupRemovalV1, WorktreeContractError> {
        let started =
            match self.advance_phase(transaction, NativeWorktreeCleanupPhaseV1::MutationStarted) {
                Ok(transaction) => transaction,
                Err(outcome) => {
                    admission.retain_recovery();
                    return Ok(outcome);
                }
            };
        let command = run_worktree_remove(
            &started.command.repository_root,
            &started.command.worktree_root,
        );
        let native_state = self.cleanup_native_state(&started, scope_set)?;
        match native_state {
            CleanupNativeStateV1::Removed => {
                let observed_at = now_at_least(started.updated_at);
                match self.write_terminal(started, NativeWorktreeCleanupOutcomeV1::Removed) {
                    Ok(receipt) => Ok(WorktreeCleanupRemovalV1::Removed {
                        confirmation_digest: receipt.transaction.confirmation_digest,
                        observed_at: receipt.completed_at,
                    }),
                    Err(outcome) => {
                        admission.retain_recovery();
                        Ok(outcome)
                    }
                }
                .map(|outcome| match outcome {
                    WorktreeCleanupRemovalV1::Removed {
                        confirmation_digest,
                        ..
                    } => WorktreeCleanupRemovalV1::Removed {
                        confirmation_digest,
                        observed_at,
                    },
                    other => other,
                })
            }
            CleanupNativeStateV1::Present
                if command.is_ok_and(|output| !output.status.success()) =>
            {
                match self.write_terminal(started, NativeWorktreeCleanupOutcomeV1::AbortedNoChange)
                {
                    Ok(_) => Ok(WorktreeCleanupRemovalV1::Denied),
                    Err(outcome) => {
                        admission.retain_recovery();
                        Ok(outcome)
                    }
                }
            }
            CleanupNativeStateV1::Foreign => {
                match self
                    .write_terminal(started, NativeWorktreeCleanupOutcomeV1::RefusedForeignDrift)
                {
                    Ok(_) => Ok(WorktreeCleanupRemovalV1::Denied),
                    Err(outcome) => {
                        admission.retain_recovery();
                        Ok(outcome)
                    }
                }
            }
            CleanupNativeStateV1::Present | CleanupNativeStateV1::Uncertain => {
                match self.advance_phase(started, NativeWorktreeCleanupPhaseV1::NeedsReconciliation)
                {
                    Ok(_) | Err(WorktreeCleanupRemovalV1::DurabilityUncertain) => {
                        admission.retain_recovery();
                        Ok(WorktreeCleanupRemovalV1::DurabilityUncertain)
                    }
                    Err(outcome) => {
                        admission.retain_recovery();
                        Ok(outcome)
                    }
                }
            }
        }
    }

    fn reconcile_for_remove(
        &self,
        transaction: NativeWorktreeCleanupTransactionV1,
        scope_set: &AuthorizedScopeSet,
        admission: WorktreeCleanupAdmissionV1,
        cancellation: &CancellationSignal,
    ) -> Result<WorktreeCleanupRemovalV1, WorktreeContractError> {
        if cancellation.is_cancelled() {
            admission.retain_recovery();
            return Ok(WorktreeCleanupRemovalV1::Unavailable);
        }
        Ok(
            match self.reconcile_transaction(transaction, scope_set, admission)? {
                WorktreeCleanupReconciliationV1::Removed {
                    confirmation_digest,
                    observed_at,
                } => WorktreeCleanupRemovalV1::Removed {
                    confirmation_digest,
                    observed_at,
                },
                WorktreeCleanupReconciliationV1::StillPresent
                | WorktreeCleanupReconciliationV1::Denied => WorktreeCleanupRemovalV1::Denied,
                WorktreeCleanupReconciliationV1::Stale => WorktreeCleanupRemovalV1::Stale,
                WorktreeCleanupReconciliationV1::DurabilityUncertain => {
                    WorktreeCleanupRemovalV1::DurabilityUncertain
                }
                WorktreeCleanupReconciliationV1::Unavailable => {
                    WorktreeCleanupRemovalV1::Unavailable
                }
            },
        )
    }

    fn reconcile_transaction(
        &self,
        transaction: NativeWorktreeCleanupTransactionV1,
        scope_set: &AuthorizedScopeSet,
        admission: WorktreeCleanupAdmissionV1,
    ) -> Result<WorktreeCleanupReconciliationV1, WorktreeContractError> {
        match self.cleanup_native_state(&transaction, scope_set)? {
            CleanupNativeStateV1::Removed => {
                match self.write_terminal(transaction, NativeWorktreeCleanupOutcomeV1::Removed) {
                    Ok(receipt) => Ok(WorktreeCleanupReconciliationV1::Removed {
                        confirmation_digest: receipt.transaction.confirmation_digest,
                        observed_at: receipt.completed_at,
                    }),
                    Err(_) => {
                        admission.retain_recovery();
                        Ok(WorktreeCleanupReconciliationV1::DurabilityUncertain)
                    }
                }
            }
            CleanupNativeStateV1::Present => {
                match self
                    .write_terminal(transaction, NativeWorktreeCleanupOutcomeV1::AbortedNoChange)
                {
                    Ok(_) => Ok(WorktreeCleanupReconciliationV1::StillPresent),
                    Err(_) => {
                        admission.retain_recovery();
                        Ok(WorktreeCleanupReconciliationV1::DurabilityUncertain)
                    }
                }
            }
            CleanupNativeStateV1::Foreign => {
                match self.write_terminal(
                    transaction,
                    NativeWorktreeCleanupOutcomeV1::RefusedForeignDrift,
                ) {
                    Ok(_) => Ok(WorktreeCleanupReconciliationV1::Denied),
                    Err(_) => {
                        admission.retain_recovery();
                        Ok(WorktreeCleanupReconciliationV1::DurabilityUncertain)
                    }
                }
            }
            CleanupNativeStateV1::Uncertain => {
                admission.retain_recovery();
                Ok(WorktreeCleanupReconciliationV1::DurabilityUncertain)
            }
        }
    }

    fn cleanup_native_state(
        &self,
        transaction: &NativeWorktreeCleanupTransactionV1,
        scope_set: &AuthorizedScopeSet,
    ) -> Result<CleanupNativeStateV1, WorktreeContractError> {
        let root = &transaction.command.worktree_root;
        if !root.exists() {
            return Ok(if self.linked_admin_present(root)? {
                CleanupNativeStateV1::Uncertain
            } else {
                CleanupNativeStateV1::Removed
            });
        }
        let target = NativeWorktreeTargetV1::Worktree {
            project_id: transaction.command.project_id.clone(),
            repository_id: transaction.command.repository_id.clone(),
            worktree_id: transaction.command.worktree_id.clone(),
        };
        let inspection = self.observe_target(
            &target,
            scope_set,
            crate::daemon_client::invocation_now_micros(),
            true,
        )?;
        Ok(match inspection.presence {
            WorktreePresenceV1::Present => CleanupNativeStateV1::Present,
            WorktreePresenceV1::Foreign => CleanupNativeStateV1::Foreign,
            WorktreePresenceV1::Stale | WorktreePresenceV1::Unavailable => {
                CleanupNativeStateV1::Uncertain
            }
        })
    }

    fn linked_admin_present(&self, worktree_root: &Path) -> Result<bool, WorktreeContractError> {
        let repository = gix::open(&self.repository_root)
            .map_err(|_| WorktreeContractError::AuthorityUnavailable)?;
        let worktrees = repository
            .worktrees()
            .map_err(|_| WorktreeContractError::AuthorityUnavailable)?;
        Ok(worktrees.into_iter().any(|worktree| {
            worktree
                .base()
                .is_ok_and(|base| base.as_path() == worktree_root)
        }))
    }

    fn advance_phase(
        &self,
        current: NativeWorktreeCleanupTransactionV1,
        phase: NativeWorktreeCleanupPhaseV1,
    ) -> Result<NativeWorktreeCleanupTransactionV1, WorktreeCleanupRemovalV1> {
        let mut replacement = current.clone();
        replacement.phase = phase;
        replacement.phase_revision = current.phase_revision.saturating_add(1);
        replacement.updated_at = now_at_least(current.updated_at);
        replacement.terminal_outcome = None;
        let replacement = replacement
            .seal()
            .map_err(|_| WorktreeCleanupRemovalV1::Unavailable)?;
        self.store
            .compare_and_swap_worktree_cleanup(
                &current.confirmation_digest,
                current.phase_revision,
                replacement,
            )
            .map_err(removal_store_error)
    }

    fn write_terminal(
        &self,
        current: NativeWorktreeCleanupTransactionV1,
        outcome: NativeWorktreeCleanupOutcomeV1,
    ) -> Result<NativeWorktreeCleanupReceiptV1, WorktreeCleanupRemovalV1> {
        let mut terminal = current.clone();
        terminal.phase = NativeWorktreeCleanupPhaseV1::Terminal;
        terminal.phase_revision = current.phase_revision.saturating_add(1);
        terminal.updated_at = now_at_least(current.updated_at);
        terminal.terminal_outcome = Some(outcome);
        let terminal = terminal
            .seal()
            .map_err(|_| WorktreeCleanupRemovalV1::Unavailable)?;
        let receipt = NativeWorktreeCleanupReceiptV1 {
            completed_at: terminal.updated_at,
            transaction: terminal,
            receipt_digest: zero_digest().map_err(|_| WorktreeCleanupRemovalV1::Unavailable)?,
        }
        .seal()
        .map_err(|_| WorktreeCleanupRemovalV1::Unavailable)?;
        self.store
            .write_worktree_cleanup_terminal(
                &current.confirmation_digest,
                current.phase_revision,
                receipt,
            )
            .map_err(removal_store_error)
    }
}

fn run_worktree_remove(repository_root: &Path, worktree_root: &Path) -> Result<Output, ()> {
    let mut command = Command::new(git_program());
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .arg("worktree")
        .arg("remove")
        .arg(worktree_root)
        .current_dir(repository_root);
    bounded_command_output(command, None, &GitCommandBounds::default()).map_err(|_| ())
}

fn now_at_least(floor: UtcMicros) -> UtcMicros {
    let now = crate::daemon_client::invocation_now_micros();
    UtcMicros(now.0.max(floor.0))
}

fn removal_store_error(error: NativeIntegrationStoreError) -> WorktreeCleanupRemovalV1 {
    match error {
        NativeIntegrationStoreError::CleanupTransactionConflict
        | NativeIntegrationStoreError::CleanupReceiptConflict
        | NativeIntegrationStoreError::StatusConflict
        | NativeIntegrationStoreError::TransactionConflict
        | NativeIntegrationStoreError::PreviewConflict
        | NativeIntegrationStoreError::ApprovalConflict
        | NativeIntegrationStoreError::ReceiptConflict
        | NativeIntegrationStoreError::InvalidData(_) => WorktreeCleanupRemovalV1::Stale,
        NativeIntegrationStoreError::RepositoryQuarantined => WorktreeCleanupRemovalV1::Denied,
        NativeIntegrationStoreError::DurabilityUncertain => {
            WorktreeCleanupRemovalV1::DurabilityUncertain
        }
        NativeIntegrationStoreError::Unavailable | NativeIntegrationStoreError::ResetRequired => {
            WorktreeCleanupRemovalV1::Unavailable
        }
    }
}

fn reconciliation_store_error(
    error: NativeIntegrationStoreError,
) -> WorktreeCleanupReconciliationV1 {
    match removal_store_error(error) {
        WorktreeCleanupRemovalV1::Denied => WorktreeCleanupReconciliationV1::Denied,
        WorktreeCleanupRemovalV1::Stale => WorktreeCleanupReconciliationV1::Stale,
        WorktreeCleanupRemovalV1::DurabilityUncertain => {
            WorktreeCleanupReconciliationV1::DurabilityUncertain
        }
        _ => WorktreeCleanupReconciliationV1::Unavailable,
    }
}

fn removal_from_outcome(
    outcome: Option<NativeWorktreeCleanupOutcomeV1>,
    confirmation_digest: &tracedecay_domain::ManifestDigest,
    observed_at: UtcMicros,
) -> WorktreeCleanupRemovalV1 {
    match outcome {
        Some(NativeWorktreeCleanupOutcomeV1::Removed) => WorktreeCleanupRemovalV1::Removed {
            confirmation_digest: confirmation_digest.clone(),
            observed_at,
        },
        Some(NativeWorktreeCleanupOutcomeV1::RefusedForeignDrift) => {
            WorktreeCleanupRemovalV1::Denied
        }
        Some(NativeWorktreeCleanupOutcomeV1::AbortedNoChange) => {
            WorktreeCleanupRemovalV1::Unavailable
        }
        None => WorktreeCleanupRemovalV1::DurabilityUncertain,
    }
}

fn reconciliation_from_outcome(
    outcome: Option<NativeWorktreeCleanupOutcomeV1>,
    confirmation_digest: &tracedecay_domain::ManifestDigest,
    observed_at: UtcMicros,
) -> WorktreeCleanupReconciliationV1 {
    match outcome {
        Some(NativeWorktreeCleanupOutcomeV1::Removed) => WorktreeCleanupReconciliationV1::Removed {
            confirmation_digest: confirmation_digest.clone(),
            observed_at,
        },
        Some(NativeWorktreeCleanupOutcomeV1::AbortedNoChange) => {
            WorktreeCleanupReconciliationV1::StillPresent
        }
        Some(NativeWorktreeCleanupOutcomeV1::RefusedForeignDrift) => {
            WorktreeCleanupReconciliationV1::Denied
        }
        None => WorktreeCleanupReconciliationV1::DurabilityUncertain,
    }
}
