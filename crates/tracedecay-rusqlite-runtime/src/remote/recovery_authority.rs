//! Durable journal and compare-and-swap authority for remote recovery.
//!
//! All state changes use the already registered remote-node runtime handle.
//! Physical backup and publication locations remain private behind the effect
//! port; caller-provided paths or database handles never cross this boundary.

use std::sync::Arc;

use serde::{Serialize, de::DeserializeOwned};
use tracedecay_application::remote::{
    capture::RemoteWriterAuthorityV1,
    protocol::RemoteProtocolRequestV1,
    recovery::{
        BackupOperationStateV1, BackupRequestV1, PromotionCasReceiptV1, PromotionConfirmationV1,
        RecoveryAuthorityExpectationV1, RemoteRecoveryCallerV1, RemoteRecoveryCommittedV1,
        RemoteRecoveryControlPortV1, RemoteRecoveryInterruptionV1, RemoteRecoveryOperationErrorV1,
        RemoteRecoveryOperationPortV1, RemoteRecoveryOperationReceiptV1,
        RemoteRecoveryTerminationV1, StagedRestoreConfirmationV1, StagedRestoreProgressV1,
    },
};
use tracedecay_domain::{
    AuthorityEpoch, CurrentRemoteAuthorityStateV1, CurrentRemoteAuthorityV1, ManifestDigest,
    RemoteAuthorityUnavailableReasonV1, RemotePlacementRevisionV1, RemoteWriterFenceV1, UtcMicros,
    canonical_sha256,
};

use crate::exact_sql::{ExactSqlStatement, ExactSqlTransaction, ExactSqlValue};

use super::*;

mod journal;

use journal::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteRecoveryPhysicalCommitV1<T> {
    pub output: T,
    pub policy_digest: ManifestDigest,
    pub committed_state_digest: ManifestDigest,
    pub committed_at: UtcMicros,
    pub units_consumed: u64,
    pub bytes_consumed: u64,
    pub interruption_observed_after_commit: Option<RemoteRecoveryInterruptionV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteRecoveryPhysicalEffectErrorV1 {
    RolledBack,
    ForwardRecoveryRequired,
    Cancelled,
    TimedOut,
    Unavailable,
    Corruption,
}

/// Idempotent private effects used by the durable journal.
///
/// An exact retry after process death must either return the original physical
/// result or continue recovery of that same operation identity. Implementations
/// must never expose a partially published restore or promotion.
pub trait RemoteRecoveryPhysicalEffectsV1: Send + Sync {
    fn current_authority(
        &self,
        expected: &RecoveryAuthorityExpectationV1,
        caller: &RemoteRecoveryCallerV1,
    ) -> Result<(CurrentRemoteAuthorityV1, u64), RemoteRecoveryPhysicalEffectErrorV1>;

    fn required_promotion_sink_ids(
        &self,
        expected: &RecoveryAuthorityExpectationV1,
    ) -> Result<Vec<String>, RemoteRecoveryPhysicalEffectErrorV1>;

    fn create_backup(
        &self,
        operation_id: &str,
        expected: &RecoveryAuthorityExpectationV1,
        caller: &RemoteRecoveryCallerV1,
        control: &dyn RemoteRecoveryControlPortV1,
        request_id: &tracedecay_application::RequestId,
    ) -> Result<
        RemoteRecoveryPhysicalCommitV1<BackupOperationStateV1>,
        RemoteRecoveryPhysicalEffectErrorV1,
    >;

    fn publish_staged_restore(
        &self,
        request: &StagedRestoreConfirmationV1,
        expected: &RecoveryAuthorityExpectationV1,
        caller: &RemoteRecoveryCallerV1,
        control: &dyn RemoteRecoveryControlPortV1,
        request_id: &tracedecay_application::RequestId,
    ) -> Result<
        RemoteRecoveryPhysicalCommitV1<StagedRestoreProgressV1>,
        RemoteRecoveryPhysicalEffectErrorV1,
    >;

    #[allow(clippy::too_many_arguments)]
    fn promote(
        &self,
        operation_id: &str,
        expected: &RecoveryAuthorityExpectationV1,
        replacement: &RemoteWriterFenceV1,
        required_sink_ids: &[String],
        caller: &RemoteRecoveryCallerV1,
        control: &dyn RemoteRecoveryControlPortV1,
        request_id: &tracedecay_application::RequestId,
    ) -> Result<
        RemoteRecoveryPhysicalCommitV1<PromotionCasReceiptV1>,
        RemoteRecoveryPhysicalEffectErrorV1,
    >;
}

#[derive(Clone)]
pub struct RemoteRecoverySqliteAuthorityV1 {
    handle: ExactSqlHandle,
    effects: Arc<dyn RemoteRecoveryPhysicalEffectsV1>,
}

impl RemoteRecoverySqliteAuthorityV1 {
    pub fn from_registered(
        handle: ExactSqlHandle,
        effects: Arc<dyn RemoteRecoveryPhysicalEffectsV1>,
    ) -> Result<Self, RemoteSqliteStorageErrorV1> {
        if !matches!(
            handle.binding().shard_id.scope,
            tracedecay_store::StoreShardScopeV1::RemoteNode { .. }
        ) {
            return Err(RemoteSqliteStorageErrorV1::BindingMismatch);
        }
        validate_final_schema(&handle)?;
        Ok(Self { handle, effects })
    }

    /// Publishes the authority-store value used by every later recovery CAS.
    /// A lower epoch, or a different writer at the same epoch, is rejected.
    pub fn publish_authority(
        &self,
        authority: &CurrentRemoteAuthorityV1,
        frontier_sequence: u64,
    ) -> Result<(), RemoteSqliteStorageErrorV1> {
        authority
            .validate()
            .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
        let key = authority_key_for_writer(&authority.fence)?;
        let transaction = self.handle.begin_immediate()?;
        if let Some((current, current_frontier)) = load_authority_in(&transaction, &key)?
            && (current.fence.authority_epoch > authority.fence.authority_epoch
                || (current.fence.authority_epoch == authority.fence.authority_epoch
                    && current != *authority)
                || (current.fence == authority.fence && frontier_sequence < current_frontier))
        {
            transaction.rollback()?;
            return Err(RemoteSqliteStorageErrorV1::Conflict);
        }
        transaction.execute(ExactSqlStatement::new(
            "INSERT INTO remote_recovery_authorities (
                authority_key, authority_json, frontier_sequence, updated_at
             ) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(authority_key) DO UPDATE SET
                authority_json = excluded.authority_json,
                frontier_sequence = excluded.frontier_sequence,
                updated_at = excluded.updated_at"
                .to_owned(),
            vec![
                text(&key),
                text(
                    &serde_json::to_string(authority)
                        .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?,
                ),
                ExactSqlValue::Integer(
                    i64::try_from(frontier_sequence)
                        .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?,
                ),
                ExactSqlValue::Integer(authority.observed_at.0),
            ],
        )?)?;
        transaction.commit()?;
        Ok(())
    }

    /// Completes durable promotion intents when their ProjectSessions target
    /// becomes available. The original admitted request and caller binding are
    /// journaled before the write gate is visible, so recovery never depends
    /// on an API caller retrying.
    pub fn reconcile_interrupted_promotions(
        &self,
        project_id: &tracedecay_domain::ProjectId,
    ) -> Result<u64, RemoteRecoveryOperationErrorV1> {
        let rows = query(
            &self.handle,
            "SELECT context_json FROM remote_recovery_operations
             WHERE operation_kind = 'promotion'
               AND state IN ('executing', 'forward_recovery_required')
             ORDER BY started_at, operation_id",
            Vec::new(),
        )
        .map_err(map_store_error)?;
        let mut reconciled = 0_u64;
        for row in rows.rows {
            let context = row_text(&row, 0)
                .map_err(|error| map_store_error(RemoteSqliteStorageErrorV1::from(error)))?;
            let (request, caller): (
                RemoteProtocolRequestV1<PromotionConfirmationV1>,
                RemoteRecoveryCallerV1,
            ) = serde_json::from_str(context)
                .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?;
            if &caller.scope.project_id != project_id {
                continue;
            }
            self.promote(&request, &caller, &RecoveryReconciliationControlV1)?;
            reconciled = reconciled
                .checked_add(1)
                .ok_or(RemoteRecoveryOperationErrorV1::Corruption)?;
        }
        Ok(reconciled)
    }

    fn ensure_authority_seeded(
        &self,
        expected: &RecoveryAuthorityExpectationV1,
        caller: &RemoteRecoveryCallerV1,
    ) -> Result<(), RemoteRecoveryOperationErrorV1> {
        let authority_key = authority_key_for_expectation(expected)
            .map_err(|_| RemoteRecoveryOperationErrorV1::InvalidRequest)?;
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(|_| RemoteRecoveryOperationErrorV1::Unavailable)?;
        let stored = load_authority_in(&transaction, &authority_key).map_err(map_store_error)?;
        transaction
            .rollback()
            .map_err(|_| RemoteRecoveryOperationErrorV1::Unavailable)?;
        if stored
            .as_ref()
            .is_some_and(|(authority, _)| !expected.matches_writer(&authority.fence))
        {
            return Ok(());
        }
        let (authority, frontier_sequence) = self
            .effects
            .current_authority(expected, caller)
            .map_err(map_physical_error)?;
        if !expected.matches_writer(&authority.fence) {
            return Err(RemoteRecoveryOperationErrorV1::StaleAuthority);
        }
        self.publish_authority(&authority, frontier_sequence)
            .map_err(map_store_error)
    }

    fn promotion_is_pending(
        &self,
        operation_id: &str,
    ) -> Result<bool, RemoteRecoveryOperationErrorV1> {
        let rows = query(
            &self.handle,
            "SELECT EXISTS(
                SELECT 1 FROM remote_recovery_operations
                WHERE operation_id = ?1 AND operation_kind = 'promotion'
                  AND state IN ('executing', 'forward_recovery_required')
             )",
            vec![text(operation_id)],
        )
        .map_err(map_store_error)?;
        let row = one_exact_row(rows)?;
        match row.values.first() {
            Some(ExactSqlValue::Integer(value)) => Ok(*value == 1),
            _ => Err(RemoteRecoveryOperationErrorV1::Corruption),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_operation<Request, Output>(
        &self,
        kind: &'static str,
        operation_id: &str,
        request: &RemoteProtocolRequestV1<Request>,
        expected: RecoveryAuthorityExpectationV1,
        caller: &RemoteRecoveryCallerV1,
        control: &dyn RemoteRecoveryControlPortV1,
        effect: impl FnOnce(
            &dyn RemoteRecoveryPhysicalEffectsV1,
        ) -> Result<
            RemoteRecoveryPhysicalCommitV1<Output>,
            RemoteRecoveryPhysicalEffectErrorV1,
        >,
    ) -> Result<RemoteRecoveryCommittedV1<Output>, RemoteRecoveryOperationErrorV1>
    where
        Request: Serialize,
        Output: Clone + DeserializeOwned + Serialize,
    {
        expected
            .validate()
            .map_err(|_| RemoteRecoveryOperationErrorV1::InvalidRequest)?;
        self.ensure_authority_seeded(&expected, caller)?;
        let input_digest = canonical_sha256(request)
            .map_err(|_| RemoteRecoveryOperationErrorV1::InvalidRequest)?;
        let context_json = serde_json::to_string(&(request, caller))
            .map_err(|_| RemoteRecoveryOperationErrorV1::InvalidRequest)?;
        let authority_key = authority_key_for_expectation(&expected)
            .map_err(|_| RemoteRecoveryOperationErrorV1::InvalidRequest)?;
        let started_at = request.sent_at;
        match begin_operation(
            &self.handle,
            kind,
            operation_id,
            &input_digest,
            &context_json,
            &authority_key,
            &expected,
            false,
            None,
            started_at,
        )? {
            BeginOperationV1::Completed(committed) => Ok(*committed),
            BeginOperationV1::Execute { pre_state_digest } => {
                if let Some(interruption) = control.interruption(&request.request_id) {
                    record_interruption(
                        &self.handle,
                        operation_id,
                        &input_digest,
                        interruption,
                        started_at,
                    )?;
                    return Err(match interruption {
                        RemoteRecoveryInterruptionV1::Cancelled => {
                            RemoteRecoveryOperationErrorV1::Cancelled
                        }
                        RemoteRecoveryInterruptionV1::DeadlineExceeded => {
                            RemoteRecoveryOperationErrorV1::TimedOut
                        }
                    });
                }
                let physical = match effect(self.effects.as_ref()) {
                    Ok(physical) => physical,
                    Err(error) => {
                        record_physical_failure(
                            &self.handle,
                            operation_id,
                            &input_digest,
                            error,
                            started_at,
                        )?;
                        return Err(map_physical_error(error));
                    }
                };
                let receipt = RemoteRecoveryOperationReceiptV1 {
                    request_id: request.request_id.clone(),
                    operation_id: operation_id.to_owned(),
                    caller: caller.clone(),
                    expected,
                    input_digest: input_digest.clone(),
                    pre_state_digest,
                    committed_state_digest: Some(physical.committed_state_digest),
                    policy_digest: physical.policy_digest,
                    started_at,
                    committed_at: physical.committed_at,
                    units_consumed: physical.units_consumed,
                    bytes_consumed: physical.bytes_consumed,
                    termination: RemoteRecoveryTerminationV1::Completed,
                    interruption_observed_after_commit: physical.interruption_observed_after_commit,
                };
                receipt
                    .validate()
                    .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?;
                finish_operation(
                    &self.handle,
                    operation_id,
                    &input_digest,
                    &physical.output,
                    &receipt,
                )?;
                Ok(RemoteRecoveryCommittedV1 {
                    authority: available_authority_state(
                        &self.handle,
                        &receipt.expected,
                        physical.committed_at,
                    ),
                    receipt,
                    output: physical.output,
                })
            }
        }
    }
}

struct RecoveryReconciliationControlV1;

impl RemoteRecoveryControlPortV1 for RecoveryReconciliationControlV1 {
    fn interruption(
        &self,
        _request_id: &tracedecay_application::RequestId,
    ) -> Option<RemoteRecoveryInterruptionV1> {
        None
    }
}

impl RemoteSqliteStorageV1 {
    pub fn recovery_writer(
        &self,
        expected: &RecoveryAuthorityExpectationV1,
    ) -> Result<RemoteWriterAuthorityV1, RemoteSqliteStorageErrorV1> {
        expected
            .validate()
            .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
        let rows = query(
            &self.handle,
            "SELECT writer_json FROM remote_authorities WHERE brain_id = ?1",
            vec![text(&expected.brain_id)],
        )?;
        let row = one_row(rows)?;
        let encoded = row_text(&row, 0)?;
        let writer: RemoteWriterAuthorityV1 =
            serde_json::from_str(encoded).map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
        if !expected.matches_writer(&writer.authority.fence) {
            return Err(RemoteSqliteStorageErrorV1::Conflict);
        }
        Ok(writer)
    }

    pub fn recovery_writer_for_lineage(
        &self,
        expected: &RecoveryAuthorityExpectationV1,
    ) -> Result<RemoteWriterAuthorityV1, RemoteSqliteStorageErrorV1> {
        expected
            .validate()
            .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
        let rows = query(
            &self.handle,
            "SELECT writer_json FROM remote_authorities WHERE brain_id = ?1",
            vec![text(&expected.brain_id)],
        )?;
        let row = one_row(rows)?;
        let encoded = row_text(&row, 0)?;
        let writer: RemoteWriterAuthorityV1 =
            serde_json::from_str(encoded).map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
        let fence = &writer.authority.fence;
        if fence.brain_id.as_str() != expected.brain_id
            || fence.shard_id.as_str() != expected.shard_id
            || fence.generation_id.as_str() != expected.generation_id
            || fence.authority_epoch.0 < expected.authority_epoch
        {
            return Err(RemoteSqliteStorageErrorV1::Conflict);
        }
        Ok(writer)
    }
}

impl RemoteRecoveryOperationPortV1 for RemoteRecoverySqliteAuthorityV1 {
    fn current_authority(
        &self,
        expected: &RecoveryAuthorityExpectationV1,
        observed_at: UtcMicros,
    ) -> CurrentRemoteAuthorityStateV1 {
        available_authority_state(&self.handle, expected, observed_at)
    }

    fn create_backup(
        &self,
        request: &RemoteProtocolRequestV1<BackupRequestV1>,
        caller: &RemoteRecoveryCallerV1,
        control: &dyn RemoteRecoveryControlPortV1,
    ) -> Result<RemoteRecoveryCommittedV1<BackupOperationStateV1>, RemoteRecoveryOperationErrorV1>
    {
        let expected = request.body.expected.clone();
        self.execute_operation(
            "backup",
            &request.body.operation_id,
            request,
            expected.clone(),
            caller,
            control,
            |effects| {
                effects.create_backup(
                    &request.body.operation_id,
                    &expected,
                    caller,
                    control,
                    &request.request_id,
                )
            },
        )
    }

    fn publish_staged_restore(
        &self,
        request: &RemoteProtocolRequestV1<StagedRestoreConfirmationV1>,
        caller: &RemoteRecoveryCallerV1,
        control: &dyn RemoteRecoveryControlPortV1,
    ) -> Result<RemoteRecoveryCommittedV1<StagedRestoreProgressV1>, RemoteRecoveryOperationErrorV1>
    {
        let expected = expectation_for_restore(request)?;
        self.execute_operation(
            "restore",
            &request.body.preview_id,
            request,
            expected.clone(),
            caller,
            control,
            |effects| {
                effects.publish_staged_restore(
                    &request.body,
                    &expected,
                    caller,
                    control,
                    &request.request_id,
                )
            },
        )
    }

    fn promote(
        &self,
        request: &RemoteProtocolRequestV1<PromotionConfirmationV1>,
        caller: &RemoteRecoveryCallerV1,
        control: &dyn RemoteRecoveryControlPortV1,
    ) -> Result<RemoteRecoveryCommittedV1<PromotionCasReceiptV1>, RemoteRecoveryOperationErrorV1>
    {
        let expected = expectation_for_promotion(request)?;
        if !self.promotion_is_pending(&request.body.preview_id)? {
            self.ensure_authority_seeded(&expected, caller)?;
        }
        let replacement = replacement_writer(request, caller)?;
        let required_sink_ids = self
            .effects
            .required_promotion_sink_ids(&expected)
            .map_err(map_physical_error)?;
        validate_sink_inventory(&required_sink_ids)?;
        let input_digest = canonical_sha256(&(request, &required_sink_ids))
            .map_err(|_| RemoteRecoveryOperationErrorV1::InvalidRequest)?;
        let context_json = serde_json::to_string(&(request, caller))
            .map_err(|_| RemoteRecoveryOperationErrorV1::InvalidRequest)?;
        let authority_key = authority_key_for_expectation(&expected)
            .map_err(|_| RemoteRecoveryOperationErrorV1::InvalidRequest)?;
        match begin_operation::<PromotionCasReceiptV1>(
            &self.handle,
            "promotion",
            &request.body.preview_id,
            &input_digest,
            &context_json,
            &authority_key,
            &expected,
            true,
            Some(&replacement),
            request.sent_at,
        )? {
            BeginOperationV1::Completed(committed) => Ok(*committed),
            BeginOperationV1::Execute { pre_state_digest } => {
                if let Some(interruption) = control.interruption(&request.request_id) {
                    record_interruption(
                        &self.handle,
                        &request.body.preview_id,
                        &input_digest,
                        interruption,
                        request.sent_at,
                    )?;
                    return Err(match interruption {
                        RemoteRecoveryInterruptionV1::Cancelled => {
                            RemoteRecoveryOperationErrorV1::Cancelled
                        }
                        RemoteRecoveryInterruptionV1::DeadlineExceeded => {
                            RemoteRecoveryOperationErrorV1::TimedOut
                        }
                    });
                }
                let physical = match self.effects.promote(
                    &request.body.preview_id,
                    &expected,
                    &replacement,
                    &required_sink_ids,
                    caller,
                    control,
                    &request.request_id,
                ) {
                    Ok(physical) => physical,
                    Err(error) => {
                        record_physical_failure(
                            &self.handle,
                            &request.body.preview_id,
                            &input_digest,
                            RemoteRecoveryPhysicalEffectErrorV1::ForwardRecoveryRequired,
                            request.sent_at,
                        )?;
                        return Err(map_physical_error(error));
                    }
                };
                validate_promotion_output(
                    &physical.output,
                    &expected,
                    &replacement,
                    &required_sink_ids,
                )?;
                publish_promoted_authorities(
                    &self.handle,
                    &authority_key,
                    &expected,
                    &replacement,
                    physical.output.published_frontier_sequence,
                    physical.committed_at,
                )?;
                persist_sink_receipts(
                    &self.handle,
                    &request.body.preview_id,
                    &physical.output,
                    physical.committed_at,
                )?;
                let receipt = RemoteRecoveryOperationReceiptV1 {
                    request_id: request.request_id.clone(),
                    operation_id: request.body.preview_id.clone(),
                    caller: caller.clone(),
                    expected,
                    input_digest: input_digest.clone(),
                    pre_state_digest,
                    committed_state_digest: Some(physical.committed_state_digest),
                    policy_digest: physical.policy_digest,
                    started_at: request.sent_at,
                    committed_at: physical.committed_at,
                    units_consumed: physical.units_consumed,
                    bytes_consumed: physical.bytes_consumed,
                    termination: RemoteRecoveryTerminationV1::Completed,
                    interruption_observed_after_commit: physical.interruption_observed_after_commit,
                };
                receipt
                    .validate()
                    .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?;
                finish_operation(
                    &self.handle,
                    &request.body.preview_id,
                    &input_digest,
                    &physical.output,
                    &receipt,
                )?;
                Ok(RemoteRecoveryCommittedV1 {
                    authority: available_authority_state(
                        &self.handle,
                        &receipt.expected,
                        physical.committed_at,
                    ),
                    receipt,
                    output: physical.output,
                })
            }
        }
    }
}

fn available_authority_state(
    handle: &ExactSqlHandle,
    expected: &RecoveryAuthorityExpectationV1,
    observed_at: UtcMicros,
) -> CurrentRemoteAuthorityStateV1 {
    let Ok(key) = authority_key_for_expectation(expected) else {
        return unavailable(
            RemoteAuthorityUnavailableReasonV1::FenceUnverified,
            observed_at,
        );
    };
    let Ok(rows) = query(
        handle,
        "SELECT authority_json FROM remote_recovery_authorities WHERE authority_key = ?1",
        vec![text(&key)],
    ) else {
        return unavailable(
            RemoteAuthorityUnavailableReasonV1::RegistryUnavailable,
            observed_at,
        );
    };
    let Ok(row) = one_row(rows) else {
        return unavailable(
            RemoteAuthorityUnavailableReasonV1::PlacementUnknown,
            observed_at,
        );
    };
    let Some(ExactSqlValue::Text(encoded)) = row.values.first() else {
        return unavailable(
            RemoteAuthorityUnavailableReasonV1::FenceUnverified,
            observed_at,
        );
    };
    match serde_json::from_str::<CurrentRemoteAuthorityV1>(encoded) {
        Ok(authority) => CurrentRemoteAuthorityStateV1::Available(authority),
        Err(_) => unavailable(
            RemoteAuthorityUnavailableReasonV1::FenceUnverified,
            observed_at,
        ),
    }
}

fn unavailable(
    reason: RemoteAuthorityUnavailableReasonV1,
    observed_at: UtcMicros,
) -> CurrentRemoteAuthorityStateV1 {
    CurrentRemoteAuthorityStateV1::Unavailable {
        reason,
        observed_at,
    }
}

fn expectation_for_restore(
    request: &RemoteProtocolRequestV1<StagedRestoreConfirmationV1>,
) -> Result<RecoveryAuthorityExpectationV1, RemoteRecoveryOperationErrorV1> {
    expectation_from_writer(
        request
            .expected_authority
            .as_ref()
            .ok_or(RemoteRecoveryOperationErrorV1::InvalidRequest)?,
        request.body.expected_authority_epoch,
        request.body.expected_placement_revision,
    )
}

fn expectation_for_promotion(
    request: &RemoteProtocolRequestV1<PromotionConfirmationV1>,
) -> Result<RecoveryAuthorityExpectationV1, RemoteRecoveryOperationErrorV1> {
    expectation_from_writer(
        request
            .expected_authority
            .as_ref()
            .ok_or(RemoteRecoveryOperationErrorV1::InvalidRequest)?,
        request.body.expected_authority_epoch,
        request.body.expected_placement_revision,
    )
}

fn expectation_from_writer(
    writer: &RemoteWriterFenceV1,
    epoch: u64,
    placement_revision: u64,
) -> Result<RecoveryAuthorityExpectationV1, RemoteRecoveryOperationErrorV1> {
    let expected = RecoveryAuthorityExpectationV1 {
        brain_id: writer.brain_id.as_str().to_owned(),
        shard_id: writer.shard_id.as_str().to_owned(),
        generation_id: writer.generation_id.as_str().to_owned(),
        authority_node_id: writer.authority_node_id.as_str().to_owned(),
        placement_revision,
        authority_epoch: epoch,
    };
    if expected.matches_writer(writer) {
        Ok(expected)
    } else {
        Err(RemoteRecoveryOperationErrorV1::InvalidRequest)
    }
}

fn replacement_writer(
    request: &RemoteProtocolRequestV1<PromotionConfirmationV1>,
    caller: &RemoteRecoveryCallerV1,
) -> Result<RemoteWriterFenceV1, RemoteRecoveryOperationErrorV1> {
    let current = request
        .expected_authority
        .as_ref()
        .ok_or(RemoteRecoveryOperationErrorV1::InvalidRequest)?;
    let authority_epoch = current
        .authority_epoch
        .0
        .checked_add(1)
        .ok_or(RemoteRecoveryOperationErrorV1::Conflict)?;
    let placement_revision = current
        .placement_revision
        .get()
        .checked_add(1)
        .ok_or(RemoteRecoveryOperationErrorV1::Conflict)?;
    Ok(RemoteWriterFenceV1 {
        brain_id: current.brain_id.clone(),
        shard_id: current.shard_id.clone(),
        generation_id: current.generation_id.clone(),
        placement_revision: RemotePlacementRevisionV1::new(placement_revision)
            .map_err(|_| RemoteRecoveryOperationErrorV1::Conflict)?,
        authority_epoch: AuthorityEpoch(authority_epoch),
        authority_node_id: caller.node_id.clone(),
    })
}

fn validate_promotion_output(
    output: &PromotionCasReceiptV1,
    expected: &RecoveryAuthorityExpectationV1,
    replacement: &RemoteWriterFenceV1,
    required_sink_ids: &[String],
) -> Result<(), RemoteRecoveryOperationErrorV1> {
    if output.previous_epoch != expected.authority_epoch
        || output.installed_epoch != replacement.authority_epoch.0
        || output.installed_placement_revision != replacement.placement_revision.get()
        || !output.old_authority_fenced
        || output.installed_sink_ids.len() != required_sink_ids.len()
        || required_sink_ids
            .iter()
            .any(|required| !output.installed_sink_ids.contains(required))
    {
        return Err(RemoteRecoveryOperationErrorV1::Corruption);
    }
    Ok(())
}

fn validate_sink_inventory(sink_ids: &[String]) -> Result<(), RemoteRecoveryOperationErrorV1> {
    let mut unique = std::collections::BTreeSet::new();
    if sink_ids.is_empty()
        || sink_ids.iter().any(|sink| {
            sink.is_empty()
                || sink.len() > 512
                || sink.trim() != sink
                || sink.chars().any(char::is_control)
                || !unique.insert(sink.as_str())
        })
    {
        return Err(RemoteRecoveryOperationErrorV1::Corruption);
    }
    Ok(())
}

fn persist_sink_receipts(
    handle: &ExactSqlHandle,
    operation_id: &str,
    output: &PromotionCasReceiptV1,
    installed_at: UtcMicros,
) -> Result<(), RemoteRecoveryOperationErrorV1> {
    let transaction = handle
        .begin_immediate()
        .map_err(|_| RemoteRecoveryOperationErrorV1::Unavailable)?;
    for sink_id in &output.installed_sink_ids {
        let result = transaction
            .execute(
                ExactSqlStatement::new(
                    "INSERT INTO remote_recovery_sink_installations (
                        operation_id, sink_id, installed_epoch, installed_at
                     ) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(operation_id, sink_id) DO NOTHING"
                        .to_owned(),
                    vec![
                        text(operation_id),
                        text(sink_id),
                        integer(output.installed_epoch)?,
                        ExactSqlValue::Integer(installed_at.0),
                    ],
                )
                .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?,
            )
            .map_err(|_| RemoteRecoveryOperationErrorV1::Unavailable)?;
        if result.changed_rows == 0 {
            let rows = transaction
                .query(
                    ExactSqlStatement::new(
                        "SELECT installed_epoch FROM remote_recovery_sink_installations
                         WHERE operation_id = ?1 AND sink_id = ?2"
                            .to_owned(),
                        vec![text(operation_id), text(sink_id)],
                    )
                    .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?,
                )
                .map_err(|_| RemoteRecoveryOperationErrorV1::Unavailable)?;
            let row = one_exact_row(rows)?;
            if exact_u64(&row, 0)? != output.installed_epoch {
                return Err(RemoteRecoveryOperationErrorV1::Conflict);
            }
        }
    }
    transaction
        .commit()
        .map_err(|_| RemoteRecoveryOperationErrorV1::Unavailable)?;
    Ok(())
}

fn current_authority_from_receipt(
    receipt: &RemoteRecoveryOperationReceiptV1,
) -> Result<CurrentRemoteAuthorityV1, RemoteRecoveryOperationErrorV1> {
    Ok(CurrentRemoteAuthorityV1 {
        fence: RemoteWriterFenceV1 {
            brain_id: tracedecay_domain::BrainId::new(receipt.expected.brain_id.clone())
                .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?,
            shard_id: tracedecay_domain::ShardId::new(receipt.expected.shard_id.clone())
                .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?,
            generation_id: tracedecay_domain::ProjectionGenerationId::new(
                receipt.expected.generation_id.clone(),
            )
            .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?,
            placement_revision: RemotePlacementRevisionV1::new(receipt.expected.placement_revision)
                .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?,
            authority_epoch: AuthorityEpoch(receipt.expected.authority_epoch),
            authority_node_id: tracedecay_domain::BrainNodeId::new(
                receipt.expected.authority_node_id.clone(),
            )
            .map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)?,
        },
        credential_revision: receipt.caller.enrollment_revision,
        observed_at: receipt.committed_at,
    })
}

fn authority_key_for_expectation(
    expected: &RecoveryAuthorityExpectationV1,
) -> Result<String, RemoteSqliteStorageErrorV1> {
    canonical_sha256(&(
        "tracedecay.remote-recovery-authority.v1",
        &expected.brain_id,
        &expected.shard_id,
        &expected.generation_id,
    ))
    .map(|digest| digest.as_str().to_owned())
    .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)
}

fn authority_key_for_writer(
    writer: &RemoteWriterFenceV1,
) -> Result<String, RemoteSqliteStorageErrorV1> {
    canonical_sha256(&(
        "tracedecay.remote-recovery-authority.v1",
        &writer.brain_id,
        &writer.shard_id,
        &writer.generation_id,
    ))
    .map(|digest| digest.as_str().to_owned())
    .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)
}

fn map_physical_error(
    error: RemoteRecoveryPhysicalEffectErrorV1,
) -> RemoteRecoveryOperationErrorV1 {
    match error {
        RemoteRecoveryPhysicalEffectErrorV1::RolledBack
        | RemoteRecoveryPhysicalEffectErrorV1::ForwardRecoveryRequired => {
            RemoteRecoveryOperationErrorV1::RecoveryRequired
        }
        RemoteRecoveryPhysicalEffectErrorV1::Cancelled => RemoteRecoveryOperationErrorV1::Cancelled,
        RemoteRecoveryPhysicalEffectErrorV1::TimedOut => RemoteRecoveryOperationErrorV1::TimedOut,
        RemoteRecoveryPhysicalEffectErrorV1::Unavailable => {
            RemoteRecoveryOperationErrorV1::Unavailable
        }
        RemoteRecoveryPhysicalEffectErrorV1::Corruption => {
            RemoteRecoveryOperationErrorV1::Corruption
        }
    }
}

fn integer(value: u64) -> Result<ExactSqlValue, RemoteRecoveryOperationErrorV1> {
    i64::try_from(value)
        .map(ExactSqlValue::Integer)
        .map_err(|_| RemoteRecoveryOperationErrorV1::InvalidRequest)
}

fn exact_text(
    row: &crate::exact_sql::ExactSqlRow,
    index: usize,
) -> Result<&str, RemoteRecoveryOperationErrorV1> {
    match row.values.get(index) {
        Some(ExactSqlValue::Text(value)) => Ok(value),
        _ => Err(RemoteRecoveryOperationErrorV1::Corruption),
    }
}

fn exact_text_store(
    row: &crate::exact_sql::ExactSqlRow,
    index: usize,
) -> Result<&str, RemoteSqliteStorageErrorV1> {
    match row.values.get(index) {
        Some(ExactSqlValue::Text(value)) => Ok(value),
        _ => Err(RemoteSqliteStorageErrorV1::Corruption),
    }
}

fn exact_u64(
    row: &crate::exact_sql::ExactSqlRow,
    index: usize,
) -> Result<u64, RemoteRecoveryOperationErrorV1> {
    match row.values.get(index) {
        Some(ExactSqlValue::Integer(value)) => {
            u64::try_from(*value).map_err(|_| RemoteRecoveryOperationErrorV1::Corruption)
        }
        _ => Err(RemoteRecoveryOperationErrorV1::Corruption),
    }
}

fn exact_u64_store(
    row: &crate::exact_sql::ExactSqlRow,
    index: usize,
) -> Result<u64, RemoteSqliteStorageErrorV1> {
    match row.values.get(index) {
        Some(ExactSqlValue::Integer(value)) => {
            u64::try_from(*value).map_err(|_| RemoteSqliteStorageErrorV1::Corruption)
        }
        _ => Err(RemoteSqliteStorageErrorV1::Corruption),
    }
}

fn one_exact_row(
    rows: crate::exact_sql::ExactSqlRows,
) -> Result<crate::exact_sql::ExactSqlRow, RemoteRecoveryOperationErrorV1> {
    let mut rows = rows.rows.into_iter();
    match (rows.next(), rows.next()) {
        (Some(row), None) => Ok(row),
        _ => Err(RemoteRecoveryOperationErrorV1::Corruption),
    }
}
