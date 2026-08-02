//! Public control-plane mutation orchestration over the atomic store primitives.

use super::activation::latest_component_activation_states;
use super::audit::{audit_from_transaction, insert_dry_run_audit_event};
use super::mutation::{
    build_configuration_commit, commit_configuration_transaction, commit_direct_in_transaction,
    current_state_from_transaction, derived_identifier, map_protected_change_snapshot_error,
    map_store_error, replay_control_receipt, result_revision_id, rollback_redacted_changes,
    validate_apply_request, validate_plan_evidence,
};
use super::read::{read_change_plan_from_executor, read_revision_from_executor};
use super::write::insert_change_plan;
use super::{
    AuthorizedActor, CONFIGURATION_AUDIT_PAGE_LIMIT, ChangePlanId, ComponentConfigurationState,
    ConfigurationAuditEventKindV1, ConfigurationAuditPage, ConfigurationAuditQuery,
    ConfigurationCommitDraft, ConfigurationControlStore, ConfigurationCurrentStateV1,
    ConfigurationError, ConfigurationMutationAuthority, ConfigurationMutationReceipt,
    ConfigurationOperationFuture, ConfigurationProtectedOperationV1,
    ConfigurationProtectedPlanRecordV1, ConfigurationRevisionId, ConfigurationRollbackRequest,
    DirectConfigurationMutation, GlobalDbConfigurationControlStore, ProtectedChange,
    ProtectedChangePlan, RollbackModeV1, ScopeRevalidationEvidenceV1,
    StoredConfigurationProtectedOperationV1, UtcMicros, canonical_sha256,
};

impl ConfigurationControlStore for GlobalDbConfigurationControlStore<'_> {
    fn current(&self) -> ConfigurationOperationFuture<'_, ConfigurationCurrentStateV1> {
        Box::pin(async move {
            let read = self
                .db
                .read_snapshot()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            current_state_from_transaction(&read).await
        })
    }

    fn save_plan(
        &self,
        plan: &ProtectedChangePlan,
        operation: &ProtectedChange,
    ) -> ConfigurationOperationFuture<'_, ()> {
        let plan = plan.clone();
        let operation = operation.clone();
        Box::pin(async move {
            let record = ConfigurationProtectedPlanRecordV1 {
                plan,
                operation: ConfigurationProtectedOperationV1::Change(Box::new(operation)),
            };
            record.validate().map_err(ConfigurationError::validation)?;
            let transaction = self
                .db
                .begin_write_transaction()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            let outcome = match read_change_plan_from_executor(&transaction, &record.plan.plan_id)
                .await
                .map_err(map_store_error)?
            {
                Some(existing) if existing == record => Ok(()),
                Some(_) => Err(ConfigurationError::IdempotencyConflict),
                None => {
                    async {
                        insert_change_plan(&transaction, &record)
                            .await
                            .map_err(map_store_error)?;
                        insert_dry_run_audit_event(&transaction, &record)
                            .await
                            .map_err(map_store_error)
                    }
                    .await
                }
            };
            match outcome {
                Ok(()) => transaction
                    .commit()
                    .await
                    .map_err(|_| ConfigurationError::Unavailable),
                Err(error) => Err(error),
            }
        })
    }

    fn load_plan(
        &self,
        plan_id: &ChangePlanId,
    ) -> ConfigurationOperationFuture<'_, Option<ProtectedChangePlan>> {
        let plan_id = plan_id.clone();
        Box::pin(async move {
            plan_id.validate().map_err(ConfigurationError::validation)?;
            let read = self
                .db
                .read_snapshot()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            read_change_plan_from_executor(&read, &plan_id)
                .await
                .map_err(map_store_error)
                .map(|record| record.map(|record| record.plan))
        })
    }

    fn commit_direct(
        &self,
        authority: &ConfigurationMutationAuthority,
        mutation: &DirectConfigurationMutation,
        expected_revision: &ConfigurationRevisionId,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
        let authority = authority.clone();
        let mutation = mutation.clone();
        let expected_revision = expected_revision.clone();
        Box::pin(async move {
            let transaction = self
                .db
                .begin_write_transaction()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            let outcome = commit_direct_in_transaction(
                &transaction,
                &authority,
                &mutation,
                &expected_revision,
            )
            .await;
            match outcome {
                Ok(outcome) => transaction
                    .commit()
                    .await
                    .map(|()| outcome.receipt)
                    .map_err(|_| ConfigurationError::Unavailable),
                Err(error) => Err(error),
            }
        })
    }

    fn commit_protected(
        &self,
        authority: &ConfigurationMutationAuthority,
        request: &tracedecay_domain::configuration::ProtectedApplyRequest,
        plan: &ProtectedChangePlan,
        evidence: &ScopeRevalidationEvidenceV1,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
        let authority = authority.clone();
        let request = request.clone();
        let plan = plan.clone();
        let evidence = evidence.clone();
        Box::pin(async move {
            authority.validate_integrity()?;
            validate_apply_request(&request)?;
            plan.validate().map_err(ConfigurationError::validation)?;
            validate_plan_evidence(&plan, &evidence)?;
            if request.actor_id != authority.receipt.actor_id
                || request.expected_base_revision_id != plan.base_revision_id
                || request.operation_digest != plan.operation_digest
            {
                return Err(ConfigurationError::PlanStale);
            }
            let transaction = self
                .db
                .begin_write_transaction()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            let outcome = async {
                if let Some(receipt) = replay_control_receipt(
                    &transaction,
                    &authority.receipt.actor_id,
                    &request.idempotency_key,
                    &plan.base_revision_id,
                    &request.operation_digest,
                    Some(&plan.plan_id),
                )
                .await?
                {
                    return Ok(receipt);
                }
                let current = current_state_from_transaction(&transaction).await?;
                if current.revision_id != plan.base_revision_id {
                    return Err(ConfigurationError::PlanStale);
                }
                let record = read_change_plan_from_executor(&transaction, &plan.plan_id)
                    .await
                    .map_err(map_store_error)?
                    .ok_or(ConfigurationError::PlanStale)?;
                if record.plan != plan {
                    return Err(ConfigurationError::PlanStale);
                }
                let ConfigurationProtectedOperationV1::Change(change) = &record.operation else {
                    return Err(ConfigurationError::PlanStale);
                };
                if record
                    .operation
                    .operation_digest()
                    .map_err(ConfigurationError::validation)?
                    != request.operation_digest
                {
                    return Err(ConfigurationError::PlanStale);
                }
                let next_revision_id = result_revision_id(
                    &plan.base_revision_id,
                    &request.idempotency_key,
                    &request.operation_digest,
                )?;
                let snapshot = current
                    .snapshot
                    .apply_protected_change(change, &next_revision_id)
                    .map_err(map_protected_change_snapshot_error)?;
                let sealed_target =
                    StoredConfigurationProtectedOperationV1::from(&record.operation);
                let (commit, sealed_target_reference) = build_configuration_commit(
                    &transaction,
                    ConfigurationCommitDraft {
                        expected_base_revision_id: &plan.base_revision_id,
                        next_revision_id,
                        snapshot,
                        actor_id: &authority.receipt.actor_id,
                        operation_kind: "protected_apply",
                        operation_digest: request.operation_digest.clone(),
                        idempotency_key: request.idempotency_key.clone(),
                        change_plan: Some(plan.clone()),
                        event_kind: ConfigurationAuditEventKindV1::Applied,
                        created_at: authority.receipt.issued_at,
                        target: &sealed_target,
                    },
                )
                .await?;
                let receipt = commit_configuration_transaction(
                    &transaction,
                    &commit,
                    false,
                    Some(&sealed_target_reference),
                )
                .await
                .map_err(map_store_error)?;
                Ok(ConfigurationMutationReceipt {
                    receipt_id: receipt.receipt_id,
                    base_revision_id: receipt.base_revision_id,
                    result_revision_id: receipt.result_revision_id,
                    snapshot_id: commit.next_revision.snapshot.snapshot_id,
                    operation_digest: receipt.operation_digest,
                    created_at: receipt.created_at,
                })
            }
            .await;
            match outcome {
                Ok(receipt) => transaction
                    .commit()
                    .await
                    .map(|()| receipt)
                    .map_err(|_| ConfigurationError::Unavailable),
                Err(error) => Err(error),
            }
        })
    }

    fn dry_run_rollback(
        &self,
        authority: &ConfigurationMutationAuthority,
        rollback: &ConfigurationRollbackRequest,
        now: UtcMicros,
    ) -> ConfigurationOperationFuture<'_, ProtectedChangePlan> {
        let authority = authority.clone();
        let rollback = rollback.clone();
        Box::pin(async move {
            if rollback.mode == RollbackModeV1::Partial {
                return Err(ConfigurationError::Unavailable);
            }
            authority.validate_integrity()?;
            rollback
                .target_revision_id
                .validate()
                .map_err(ConfigurationError::validation)?;
            let transaction = self
                .db
                .begin_write_transaction()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            let outcome = async {
                let current = current_state_from_transaction(&transaction).await?;
                if current.revision_id != authority.receipt.expected_configuration_revision {
                    return Err(ConfigurationError::RevisionConflict);
                }
                let target =
                    read_revision_from_executor(&transaction, &rollback.target_revision_id)
                        .await
                        .map_err(map_store_error)?
                        .ok_or(ConfigurationError::PlanStale)?;
                let operation = ConfigurationProtectedOperationV1::Rollback {
                    target_revision_id: rollback.target_revision_id.clone(),
                    mode: rollback.mode,
                };
                let operation_digest = operation
                    .operation_digest()
                    .map_err(ConfigurationError::validation)?;
                let plan_id = derived_identifier(
                    "configuration.plan.rollback.v1",
                    &canonical_sha256(&(
                        "tracedecay.configuration.rollback-plan.v1",
                        &authority.receipt.actor_id,
                        &current.revision_id,
                        &operation_digest,
                        &authority.receipt.scope_digest,
                        authority.receipt.policy_epoch,
                        &authority.receipt.policy_digest,
                        now,
                    ))
                    .map_err(ConfigurationError::validation)?,
                    "configuration rollback plan id",
                )?;
                let changes = rollback_redacted_changes(&current.snapshot, &target.snapshot)?;
                if changes.is_empty() {
                    return Err(ConfigurationError::PlanStale);
                }
                let plan = ProtectedChangePlan {
                    plan_id,
                    actor_id: authority.receipt.actor_id.clone(),
                    base_revision_id: current.revision_id,
                    operation_digest,
                    resolved_scope_digest: authority.receipt.scope_digest.clone(),
                    membership_digest: None,
                    authorization_policy_digest: authority.receipt.policy_digest.clone(),
                    policy_epoch: authority.receipt.policy_epoch,
                    expires_at: UtcMicros(now.0.saturating_add(300_000_000)),
                    created_at: now,
                    redacted_changes: changes,
                };
                let record = ConfigurationProtectedPlanRecordV1 {
                    plan: plan.clone(),
                    operation,
                };
                record.validate().map_err(ConfigurationError::validation)?;
                match read_change_plan_from_executor(&transaction, &plan.plan_id)
                    .await
                    .map_err(map_store_error)?
                {
                    Some(existing) if existing == record => Ok(plan),
                    Some(_) => Err(ConfigurationError::IdempotencyConflict),
                    None => {
                        insert_change_plan(&transaction, &record)
                            .await
                            .map_err(map_store_error)?;
                        insert_dry_run_audit_event(&transaction, &record)
                            .await
                            .map_err(map_store_error)?;
                        Ok(plan)
                    }
                }
            }
            .await;
            match outcome {
                Ok(plan) => transaction
                    .commit()
                    .await
                    .map(|()| plan)
                    .map_err(|_| ConfigurationError::Unavailable),
                Err(error) => Err(error),
            }
        })
    }

    fn apply_rollback(
        &self,
        authority: &ConfigurationMutationAuthority,
        request: &tracedecay_domain::configuration::ProtectedApplyRequest,
        plan: &ProtectedChangePlan,
        evidence: &ScopeRevalidationEvidenceV1,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
        let authority = authority.clone();
        let request = request.clone();
        let plan = plan.clone();
        let evidence = evidence.clone();
        Box::pin(async move {
            authority.validate_integrity()?;
            validate_apply_request(&request)?;
            plan.validate().map_err(ConfigurationError::validation)?;
            validate_plan_evidence(&plan, &evidence)?;
            if request.actor_id != authority.receipt.actor_id
                || request.expected_base_revision_id != plan.base_revision_id
                || request.operation_digest != plan.operation_digest
            {
                return Err(ConfigurationError::PlanStale);
            }
            let transaction = self
                .db
                .begin_write_transaction()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            let outcome = async {
                if let Some(receipt) = replay_control_receipt(
                    &transaction,
                    &authority.receipt.actor_id,
                    &request.idempotency_key,
                    &plan.base_revision_id,
                    &request.operation_digest,
                    Some(&plan.plan_id),
                )
                .await?
                {
                    return Ok(receipt);
                }
                let current = current_state_from_transaction(&transaction).await?;
                if current.revision_id != plan.base_revision_id {
                    return Err(ConfigurationError::PlanStale);
                }
                let record = read_change_plan_from_executor(&transaction, &plan.plan_id)
                    .await
                    .map_err(map_store_error)?
                    .ok_or(ConfigurationError::PlanStale)?;
                if record.plan != plan {
                    return Err(ConfigurationError::PlanStale);
                }
                let ConfigurationProtectedOperationV1::Rollback {
                    target_revision_id,
                    mode,
                } = &record.operation
                else {
                    return Err(ConfigurationError::PlanStale);
                };
                if *mode == RollbackModeV1::Partial {
                    return Err(ConfigurationError::Unavailable);
                }
                if record
                    .operation
                    .operation_digest()
                    .map_err(ConfigurationError::validation)?
                    != request.operation_digest
                {
                    return Err(ConfigurationError::PlanStale);
                }
                let target = read_revision_from_executor(&transaction, target_revision_id)
                    .await
                    .map_err(map_store_error)?
                    .ok_or(ConfigurationError::PlanStale)?;
                let next_revision_id = result_revision_id(
                    &plan.base_revision_id,
                    &request.idempotency_key,
                    &request.operation_digest,
                )?;
                let sealed_target =
                    StoredConfigurationProtectedOperationV1::from(&record.operation);
                let (commit, sealed_target_reference) = build_configuration_commit(
                    &transaction,
                    ConfigurationCommitDraft {
                        expected_base_revision_id: &plan.base_revision_id,
                        next_revision_id,
                        snapshot: target.snapshot,
                        actor_id: &authority.receipt.actor_id,
                        operation_kind: "rollback_apply",
                        operation_digest: request.operation_digest.clone(),
                        idempotency_key: request.idempotency_key.clone(),
                        change_plan: Some(plan.clone()),
                        event_kind: ConfigurationAuditEventKindV1::RollbackApplied,
                        created_at: authority.receipt.issued_at,
                        target: &sealed_target,
                    },
                )
                .await?;
                let receipt = commit_configuration_transaction(
                    &transaction,
                    &commit,
                    false,
                    Some(&sealed_target_reference),
                )
                .await
                .map_err(map_store_error)?;
                Ok(ConfigurationMutationReceipt {
                    receipt_id: receipt.receipt_id,
                    base_revision_id: receipt.base_revision_id,
                    result_revision_id: receipt.result_revision_id,
                    snapshot_id: commit.next_revision.snapshot.snapshot_id,
                    operation_digest: receipt.operation_digest,
                    created_at: receipt.created_at,
                })
            }
            .await;
            match outcome {
                Ok(receipt) => transaction
                    .commit()
                    .await
                    .map(|()| receipt)
                    .map_err(|_| ConfigurationError::Unavailable),
                Err(error) => Err(error),
            }
        })
    }

    fn audit(
        &self,
        actor: &AuthorizedActor,
        query: &ConfigurationAuditQuery,
    ) -> ConfigurationOperationFuture<'_, ConfigurationAuditPage> {
        let actor = actor.clone();
        let query = query.clone();
        Box::pin(async move {
            actor.validate()?;
            if query.limit == 0 || query.limit > CONFIGURATION_AUDIT_PAGE_LIMIT {
                return Err(ConfigurationError::validation_message(
                    "configuration audit limit must be between 1 and 1000",
                ));
            }
            let read = self
                .db
                .read_snapshot()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            let mut events =
                audit_from_transaction(&read, query.after_event_id.as_ref(), query.limit + 1)
                    .await
                    .map_err(map_store_error)?;
            let next_after_event_id = if events.len() > query.limit {
                events.pop();
                events.last().map(|event| event.event_id.clone())
            } else {
                None
            };
            Ok(ConfigurationAuditPage {
                events,
                next_after_event_id,
            })
        })
    }

    fn observed_state(
        &self,
        actor: &AuthorizedActor,
    ) -> ConfigurationOperationFuture<'_, Vec<ComponentConfigurationState>> {
        let actor = actor.clone();
        Box::pin(async move {
            actor.validate()?;
            let read = self
                .db
                .read_snapshot()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            latest_component_activation_states(&read)
                .await
                .map_err(map_store_error)
                .map(|states| {
                    states
                        .into_iter()
                        .map(|state| ComponentConfigurationState {
                            component: state.component,
                            desired_revision_id: state.desired_revision_id,
                            observed_revision_id: state
                                .observed_revision_id
                                .or(state.last_working_revision_id),
                            restart_required: state.restart_required,
                            activation_error_code: state.activation_error_code,
                        })
                        .collect()
                })
        })
    }
}
