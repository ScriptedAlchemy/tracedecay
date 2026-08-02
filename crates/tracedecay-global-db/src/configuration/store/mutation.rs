//! Atomic mutation receipts and compare-and-swap transactions.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::activation::advance_component_desired_state;
use super::audit::{
    append_terminal_plan_event, has_matching_terminal_plan_event,
    insert_audit_event_with_receipt_digest, read_audit_event_from_transaction, seal_audit_target,
    terminal_plan_event_kind,
};
use super::codec::{
    CONFIGURATION_ACTIVATION_DESIRED_RECORDED, CONFIGURATION_AUTHORIZATION_NOT_RECORDED,
    StoredMutationReceipt, decode_id, redacted_direct_audit_target,
};
use super::read::{
    current_revision_id_from_executor, read_change_plan_from_executor, read_revision_from_executor,
    validate_snapshot_registry_completeness,
};
use super::revision::insert_revision;
use super::{
    ACCESS_RULES_SETTING_KEY, ActorId, CandidateDispositionV1, ChangePlanId,
    ConfigurationAuditEvent, ConfigurationAuditEventKindV1, ConfigurationCandidateV1,
    ConfigurationCommitV1, ConfigurationCurrentStateV1, ConfigurationError,
    ConfigurationIdempotencyKey, ConfigurationLayerIdV1, ConfigurationMutationAuthority,
    ConfigurationMutationReceipt, ConfigurationMutationReceiptV1, ConfigurationReceiptId,
    ConfigurationRegistry, ConfigurationRevisionId, ConfigurationRevisionRecordV1,
    ConfigurationSnapshotV1, ConfigurationStoreError, ConfigurationStoreResult,
    ConfigurationValueV1, DirectConfigurationMutation, Executor, ManifestDigest,
    ProtectedChangePlan, ProtectedChangeSnapshotError, QueryExecutor,
    RedactedConfigurationChangeV1, Row, SOURCE_BINDINGS_SETTING_KEY, ScopeControlOperationV1,
    ScopeRevalidationEvidenceV1, SettingKey, UtcMicros, WORK_TOPOLOGY_POLICY_SETTING_KEY,
    canonical_sha256, invalid_store_data, params, registry_default_candidate, unavailable_store,
};

pub(super) fn decode_stored_mutation_receipt(
    row: &Row,
) -> ConfigurationStoreResult<StoredMutationReceipt> {
    let receipt_id: ConfigurationReceiptId = decode_id(
        row.get::<String>(0).map_err(|error| {
            invalid_store_data(format!("read configuration receipt id: {error}"))
        })?,
        "receipt id",
    )?;
    let plan_id: Option<ChangePlanId> = row
        .get::<Option<String>>(1)
        .map_err(|error| {
            invalid_store_data(format!("read configuration receipt plan id: {error}"))
        })?
        .map(|value| decode_id(value, "receipt plan id"))
        .transpose()?;
    let actor_id: ActorId = decode_id(
        row.get::<String>(2).map_err(|error| {
            invalid_store_data(format!("read configuration receipt actor id: {error}"))
        })?,
        "receipt actor id",
    )?;
    let idempotency_key: ConfigurationIdempotencyKey = decode_id(
        row.get::<String>(3).map_err(|error| {
            invalid_store_data(format!(
                "read configuration receipt idempotency key: {error}"
            ))
        })?,
        "receipt idempotency key",
    )?;
    let base_revision_id: ConfigurationRevisionId = decode_id(
        row.get::<String>(4).map_err(|error| {
            invalid_store_data(format!(
                "read configuration receipt base revision id: {error}"
            ))
        })?,
        "receipt base revision id",
    )?;
    let result_revision_id: ConfigurationRevisionId = decode_id(
        row.get::<String>(5).map_err(|error| {
            invalid_store_data(format!(
                "read configuration receipt result revision id: {error}"
            ))
        })?,
        "receipt result revision id",
    )?;
    let operation_digest = ManifestDigest::new(row.get::<String>(6).map_err(|error| {
        invalid_store_data(format!(
            "read configuration receipt operation digest: {error}"
        ))
    })?)
    .map_err(ConfigurationStoreError::from)?;
    let authorization_policy_digest = row.get::<String>(7).map_err(|error| {
        invalid_store_data(format!(
            "read configuration receipt authorization digest: {error}"
        ))
    })?;
    let activation_status = row.get::<String>(8).map_err(|error| {
        invalid_store_data(format!(
            "read configuration receipt activation status: {error}"
        ))
    })?;
    let receipt_digest = ManifestDigest::new(row.get::<String>(9).map_err(|error| {
        invalid_store_data(format!("read configuration receipt digest: {error}"))
    })?)
    .map_err(ConfigurationStoreError::from)?;
    let created_at = row
        .get::<i64>(10)
        .map_err(|error| invalid_store_data(format!("read configuration receipt time: {error}")))?;
    let receipt = ConfigurationMutationReceiptV1 {
        receipt_id,
        actor_id,
        idempotency_key,
        base_revision_id,
        result_revision_id,
        operation_digest,
        receipt_digest,
        created_at: UtcMicros(created_at),
    };
    receipt.validate().map_err(ConfigurationStoreError::from)?;
    Ok(StoredMutationReceipt {
        receipt,
        plan_id,
        authorization_policy_digest,
        activation_status,
    })
}

pub(super) async fn receipt_for_idempotency_from_transaction(
    transaction: &impl QueryExecutor,
    actor_id: &ActorId,
    idempotency_key: &ConfigurationIdempotencyKey,
) -> ConfigurationStoreResult<Option<StoredMutationReceipt>> {
    let mut rows = transaction
        .query(
            "SELECT receipt_id, plan_id, actor_id, idempotency_key,
                    base_revision_id, result_revision_id, operation_digest,
                    authorization_policy_digest, activation_status, receipt_digest, created_at
             FROM configuration_mutation_receipts
             WHERE actor_id = ?1 AND idempotency_key = ?2",
            params![actor_id.as_str(), idempotency_key.as_str()],
        )
        .await
        .map_err(unavailable_store)?;
    let Some(row) = rows.next().await.map_err(unavailable_store)? else {
        return Ok(None);
    };
    let receipt = decode_stored_mutation_receipt(&row)?;
    if rows.next().await.map_err(unavailable_store)?.is_some() {
        return Err(invalid_store_data(
            "configuration idempotency key resolved to multiple receipts",
        ));
    }
    Ok(Some(receipt))
}

pub(super) fn authorization_policy_digest_for_commit(commit: &ConfigurationCommitV1) -> String {
    commit.change_plan.as_ref().map_or_else(
        || CONFIGURATION_AUTHORIZATION_NOT_RECORDED.to_owned(),
        |plan| plan.authorization_policy_digest.as_str().to_owned(),
    )
}

pub(super) async fn insert_mutation_receipt(
    transaction: &impl Executor,
    commit: &ConfigurationCommitV1,
) -> ConfigurationStoreResult<()> {
    commit
        .receipt
        .validate()
        .map_err(ConfigurationStoreError::from)?;
    let plan_id = commit
        .change_plan
        .as_ref()
        .map(|plan| plan.plan_id.as_str().to_owned());
    transaction
        .execute(
            "INSERT INTO configuration_mutation_receipts (
                receipt_id, plan_id, actor_id, idempotency_key,
                base_revision_id, result_revision_id, operation_digest,
                authorization_policy_digest, activation_status, receipt_digest, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                commit.receipt.receipt_id.as_str(),
                plan_id,
                commit.receipt.actor_id.as_str(),
                commit.receipt.idempotency_key.as_str(),
                commit.receipt.base_revision_id.as_str(),
                commit.receipt.result_revision_id.as_str(),
                commit.receipt.operation_digest.as_str(),
                authorization_policy_digest_for_commit(commit),
                CONFIGURATION_ACTIVATION_DESIRED_RECORDED,
                commit.receipt.receipt_digest.as_str(),
                commit.receipt.created_at.0,
            ],
        )
        .await
        .map_err(unavailable_store)?;
    Ok(())
}

pub(super) fn validate_commit_bindings(
    commit: &ConfigurationCommitV1,
) -> ConfigurationStoreResult<()> {
    commit.validate().map_err(ConfigurationStoreError::from)?;
    if commit.next_revision.parent_revision_id.as_ref() != Some(&commit.expected_base_revision_id) {
        return Err(invalid_store_data(
            "configuration commit next revision does not name the expected base revision",
        ));
    }
    if commit.audit_event.actor_id != commit.receipt.actor_id
        || commit.audit_event.idempotency_key.as_ref() != Some(&commit.receipt.idempotency_key)
        || commit.audit_event.base_revision_id != commit.receipt.base_revision_id
        || commit.audit_event.result_revision_id.as_ref()
            != Some(&commit.receipt.result_revision_id)
        || commit.audit_event.operation_digest != commit.receipt.operation_digest
        || commit.audit_event.receipt_id.as_ref() != Some(&commit.receipt.receipt_id)
    {
        return Err(invalid_store_data(
            "configuration audit event does not bind the mutation receipt",
        ));
    }
    if let Some(plan) = &commit.change_plan {
        if plan.actor_id != commit.receipt.actor_id
            || plan.base_revision_id != commit.expected_base_revision_id
            || plan.operation_digest != commit.receipt.operation_digest
        {
            return Err(invalid_store_data(
                "configuration change plan does not bind the mutation receipt",
            ));
        }
        if terminal_plan_event_kind(commit.audit_event.event_kind).is_none() {
            return Err(invalid_store_data(
                "configuration change plan commit lacks a terminal applied audit event",
            ));
        }
    }
    Ok(())
}

pub(super) async fn replay_matches_commit(
    transaction: &impl QueryExecutor,
    stored: &StoredMutationReceipt,
    commit: &ConfigurationCommitV1,
) -> ConfigurationStoreResult<bool> {
    if stored.receipt != commit.receipt
        || stored.authorization_policy_digest != authorization_policy_digest_for_commit(commit)
        || stored.activation_status != CONFIGURATION_ACTIVATION_DESIRED_RECORDED
    {
        return Ok(false);
    }
    let expected_plan_id = commit.change_plan.as_ref().map(|plan| &plan.plan_id);
    if stored.plan_id.as_ref() != expected_plan_id {
        return Ok(false);
    }
    let stored_revision =
        read_revision_from_executor(transaction, &commit.next_revision.revision_id).await?;
    if stored_revision.as_ref() != Some(&commit.next_revision) {
        return Ok(false);
    }
    let stored_audit_event =
        read_audit_event_from_transaction(transaction, &commit.audit_event.event_id).await?;
    if stored_audit_event.as_ref() != Some(&commit.audit_event) {
        return Ok(false);
    }
    if let Some(plan) = &commit.change_plan {
        let stored_plan = read_change_plan_from_executor(transaction, &plan.plan_id).await?;
        if stored_plan.as_ref().map(|record| &record.plan) != Some(plan)
            || !has_matching_terminal_plan_event(transaction, plan, &commit.audit_event).await?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) async fn commit_configuration_transaction(
    transaction: &impl Executor,
    commit: &ConfigurationCommitV1,
    fault_after_revision: bool,
    sealed_target_reference: Option<&[u8]>,
) -> ConfigurationStoreResult<ConfigurationMutationReceiptV1> {
    if let Some(stored) = receipt_for_idempotency_from_transaction(
        transaction,
        &commit.receipt.actor_id,
        &commit.receipt.idempotency_key,
    )
    .await?
    {
        return if replay_matches_commit(transaction, &stored, commit).await? {
            Ok(stored.receipt)
        } else {
            Err(ConfigurationStoreError::IdempotencyConflict)
        };
    }

    let current_revision_id = current_revision_id_from_executor(transaction).await?;
    if current_revision_id != commit.expected_base_revision_id {
        return Err(ConfigurationStoreError::RevisionConflict);
    }
    if let Some(plan) = &commit.change_plan {
        let stored_plan = read_change_plan_from_executor(transaction, &plan.plan_id).await?;
        if stored_plan.as_ref().map(|record| &record.plan) != Some(plan) {
            return Err(ConfigurationStoreError::PlanStale);
        }
    }

    insert_revision(transaction, &commit.next_revision).await?;
    if fault_after_revision {
        return Err(invalid_store_data(
            "injected configuration commit crash after revision",
        ));
    }
    insert_mutation_receipt(transaction, commit).await?;
    advance_component_desired_state(
        transaction,
        &commit.next_revision.revision_id,
        commit.receipt.created_at,
    )
    .await?;
    if let Some(plan) = &commit.change_plan {
        append_terminal_plan_event(transaction, plan, &commit.audit_event).await?;
    }
    insert_audit_event_with_receipt_digest(
        transaction,
        &commit.audit_event,
        Some(&commit.receipt.receipt_digest),
        sealed_target_reference,
    )
    .await?;
    Ok(commit.receipt.clone())
}

pub(super) fn map_protected_change_snapshot_error(
    error: ProtectedChangeSnapshotError,
) -> ConfigurationError {
    match error {
        ProtectedChangeSnapshotError::Stale => ConfigurationError::PlanStale,
        ProtectedChangeSnapshotError::Domain(error) => ConfigurationError::validation(error),
        ProtectedChangeSnapshotError::IncompatibleValue(message) => {
            ConfigurationError::validation_message(message)
        }
    }
}

pub(super) fn map_store_error(error: ConfigurationStoreError) -> ConfigurationError {
    match error {
        ConfigurationStoreError::RevisionConflict => ConfigurationError::RevisionConflict,
        ConfigurationStoreError::PlanExpired => ConfigurationError::PlanExpired,
        ConfigurationStoreError::PlanStale => ConfigurationError::PlanStale,
        ConfigurationStoreError::IdempotencyConflict => ConfigurationError::IdempotencyConflict,
        ConfigurationStoreError::InvalidData(message) => {
            ConfigurationError::validation_message(message)
        }
        ConfigurationStoreError::Unavailable => ConfigurationError::Unavailable,
    }
}

pub(super) fn derived_identifier<T>(
    prefix: &str,
    digest: &ManifestDigest,
    field: &'static str,
) -> Result<T, ConfigurationError>
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Display,
{
    let digest = digest.as_str().strip_prefix("sha256:").ok_or_else(|| {
        ConfigurationError::validation_message("configuration digest is missing its sha256 prefix")
    })?;
    T::try_from(format!("{prefix}.{digest}")).map_err(|error| {
        ConfigurationError::validation_message(format!("invalid {field}: {error}"))
    })
}

pub(super) fn direct_operation_digest(
    mutation: &DirectConfigurationMutation,
) -> Result<ManifestDigest, ConfigurationError> {
    canonical_sha256(&("tracedecay.configuration.direct-mutation.v1", mutation))
        .map_err(ConfigurationError::validation)
}

pub(super) fn direct_idempotency_key(
    authority: &ConfigurationMutationAuthority,
    operation_digest: &ManifestDigest,
) -> Result<ConfigurationIdempotencyKey, ConfigurationError> {
    let digest = canonical_sha256(&(
        "tracedecay.configuration.direct-idempotency.v1",
        &authority.receipt.receipt_id,
        operation_digest,
    ))
    .map_err(ConfigurationError::validation)?;
    derived_identifier(
        "configuration.idempotency.direct.v1",
        &digest,
        "direct idempotency key",
    )
}

pub(super) fn result_revision_id(
    expected_revision_id: &ConfigurationRevisionId,
    idempotency_key: &ConfigurationIdempotencyKey,
    operation_digest: &ManifestDigest,
) -> Result<ConfigurationRevisionId, ConfigurationError> {
    let digest = canonical_sha256(&(
        "tracedecay.configuration.result-revision.v1",
        expected_revision_id,
        idempotency_key,
        operation_digest,
    ))
    .map_err(ConfigurationError::validation)?;
    derived_identifier(
        "configuration.revision.v1",
        &digest,
        "configuration result revision id",
    )
}

pub(super) fn mutation_provenance(
    layer: &ConfigurationLayerIdV1,
    revision_id: &ConfigurationRevisionId,
) -> Vec<ConfigurationCandidateV1> {
    vec![ConfigurationCandidateV1 {
        layer: layer.clone(),
        revision_id: revision_id.clone(),
        disposition: CandidateDispositionV1::Winning,
        safe_reason: None,
    }]
}

pub(super) fn replace_direct_effective_value(
    effective_values: &mut BTreeMap<SettingKey, ConfigurationValueV1>,
    provenance: &mut BTreeMap<SettingKey, Vec<ConfigurationCandidateV1>>,
    key: SettingKey,
    value: ConfigurationValueV1,
    layer: &ConfigurationLayerIdV1,
    revision_id: &ConfigurationRevisionId,
) {
    effective_values.insert(key.clone(), value);
    provenance.insert(key, mutation_provenance(layer, revision_id));
}

pub(super) fn apply_direct_mutation_to_snapshot(
    current: &ConfigurationSnapshotV1,
    mutation: &DirectConfigurationMutation,
    revision_id: &ConfigurationRevisionId,
    registry: &ConfigurationRegistry,
) -> Result<ConfigurationSnapshotV1, ConfigurationError> {
    fn apply(
        effective_values: &mut BTreeMap<SettingKey, ConfigurationValueV1>,
        provenance: &mut BTreeMap<SettingKey, Vec<ConfigurationCandidateV1>>,
        mutation: &DirectConfigurationMutation,
        revision_id: &ConfigurationRevisionId,
        registry: &ConfigurationRegistry,
    ) -> Result<(), ConfigurationError> {
        match mutation {
            DirectConfigurationMutation::Set { layer, key, value } => {
                registry
                    .validate_layer(key, layer)
                    .map_err(ConfigurationError::validation)?;
                registry
                    .validate_value(key, value)
                    .map_err(ConfigurationError::validation)?;
                replace_direct_effective_value(
                    effective_values,
                    provenance,
                    key.clone(),
                    value.clone(),
                    layer,
                    revision_id,
                );
            }
            DirectConfigurationMutation::Unset { layer, key } => {
                registry
                    .validate_layer(key, layer)
                    .map_err(ConfigurationError::validation)?;
                let definition = registry
                    .definition(key)
                    .map_err(ConfigurationError::validation)?;
                effective_values.insert(key.clone(), definition.default_value.clone());
                provenance.insert(
                    key.clone(),
                    vec![registry_default_candidate().map_err(ConfigurationError::validation)?],
                );
            }
            DirectConfigurationMutation::Batch { mutations } => {
                for mutation in mutations {
                    apply(
                        effective_values,
                        provenance,
                        mutation,
                        revision_id,
                        registry,
                    )?;
                }
            }
        }
        Ok(())
    }

    mutation.touched_keys()?;
    let mut effective_values = current.effective_values.clone();
    let mut provenance = current.provenance.clone();
    apply(
        &mut effective_values,
        &mut provenance,
        mutation,
        revision_id,
        registry,
    )?;
    let snapshot = ConfigurationSnapshotV1::new(effective_values, provenance)
        .map_err(ConfigurationError::validation)?;
    validate_snapshot_registry_completeness(&snapshot).map_err(map_store_error)?;
    Ok(snapshot)
}

pub(super) fn validate_direct_control_mutation(
    mutation: &DirectConfigurationMutation,
) -> Result<(), ConfigurationError> {
    match mutation {
        DirectConfigurationMutation::Set { key, value, .. } => {
            if [
                SOURCE_BINDINGS_SETTING_KEY,
                ACCESS_RULES_SETTING_KEY,
                WORK_TOPOLOGY_POLICY_SETTING_KEY,
            ]
            .contains(&key.as_str())
            {
                return Err(ConfigurationError::PolicyWideningForbidden);
            }
            if matches!(value, ConfigurationValueV1::CredentialReference(_)) {
                return Err(ConfigurationError::validation_message(
                    "credential references require the write-only credential operation",
                ));
            }
            value.validate().map_err(ConfigurationError::validation)
        }
        DirectConfigurationMutation::Unset { key, .. } => {
            if [
                SOURCE_BINDINGS_SETTING_KEY,
                ACCESS_RULES_SETTING_KEY,
                WORK_TOPOLOGY_POLICY_SETTING_KEY,
            ]
            .contains(&key.as_str())
            {
                return Err(ConfigurationError::PolicyWideningForbidden);
            }
            key.validate().map_err(ConfigurationError::validation)
        }
        DirectConfigurationMutation::Batch { mutations } => {
            mutation.touched_keys()?;
            for mutation in mutations {
                validate_direct_control_mutation(mutation)?;
            }
            Ok(())
        }
    }
}

pub(super) struct ConfigurationCommitDraft<'a, T> {
    pub(super) expected_base_revision_id: &'a ConfigurationRevisionId,
    pub(super) next_revision_id: ConfigurationRevisionId,
    pub(super) snapshot: ConfigurationSnapshotV1,
    pub(super) actor_id: &'a ActorId,
    pub(super) operation_kind: &'static str,
    pub(super) operation_digest: ManifestDigest,
    pub(super) idempotency_key: ConfigurationIdempotencyKey,
    pub(super) change_plan: Option<ProtectedChangePlan>,
    pub(super) event_kind: ConfigurationAuditEventKindV1,
    pub(super) created_at: UtcMicros,
    pub(super) target: &'a T,
}

pub(super) async fn build_configuration_commit<T: Serialize>(
    transaction: &impl Executor,
    draft: ConfigurationCommitDraft<'_, T>,
) -> Result<(ConfigurationCommitV1, Vec<u8>), ConfigurationError> {
    let ConfigurationCommitDraft {
        expected_base_revision_id,
        next_revision_id,
        snapshot,
        actor_id,
        operation_kind,
        operation_digest,
        idempotency_key,
        change_plan,
        event_kind,
        created_at,
        target,
    } = draft;
    let receipt_id: ConfigurationReceiptId = derived_identifier(
        "configuration.receipt.v1",
        &canonical_sha256(&(
            "tracedecay.configuration.receipt.v1",
            actor_id,
            &idempotency_key,
            expected_base_revision_id,
            &next_revision_id,
            &operation_digest,
        ))
        .map_err(ConfigurationError::validation)?,
        "configuration receipt id",
    )?;
    let receipt_digest = canonical_sha256(&(
        "tracedecay.configuration.receipt-digest.v1",
        &receipt_id,
        actor_id,
        &idempotency_key,
        expected_base_revision_id,
        &next_revision_id,
        &operation_digest,
        created_at,
    ))
    .map_err(ConfigurationError::validation)?;
    let receipt = ConfigurationMutationReceiptV1 {
        receipt_id: receipt_id.clone(),
        actor_id: actor_id.clone(),
        idempotency_key: idempotency_key.clone(),
        base_revision_id: expected_base_revision_id.clone(),
        result_revision_id: next_revision_id.clone(),
        operation_digest: operation_digest.clone(),
        receipt_digest,
        created_at,
    };
    let event_id = derived_identifier(
        "configuration.audit.v1",
        &canonical_sha256(&(
            "tracedecay.configuration.audit-event.v1",
            &receipt_id,
            &event_kind,
        ))
        .map_err(ConfigurationError::validation)?,
        "configuration audit event id",
    )?;
    let (sealed_target_reference, target_commitment) =
        seal_audit_target(transaction, &event_id, target, created_at)
            .await
            .map_err(map_store_error)?;
    let audit_event = ConfigurationAuditEvent {
        event_id,
        event_kind,
        actor_id: actor_id.clone(),
        idempotency_key: Some(idempotency_key),
        base_revision_id: expected_base_revision_id.clone(),
        result_revision_id: Some(next_revision_id.clone()),
        operation_digest: operation_digest.clone(),
        target_commitment,
        receipt_id: Some(receipt_id),
        safe_reason_code: None,
        occurred_at: created_at,
    };
    Ok((
        ConfigurationCommitV1 {
            expected_base_revision_id: expected_base_revision_id.clone(),
            next_revision: ConfigurationRevisionRecordV1 {
                revision_id: next_revision_id,
                parent_revision_id: Some(expected_base_revision_id.clone()),
                snapshot,
                actor_id: actor_id.clone(),
                operation_kind: operation_kind.to_owned(),
                created_at,
            },
            receipt,
            change_plan,
            audit_event,
        },
        sealed_target_reference,
    ))
}

pub(super) fn validate_apply_request(
    request: &tracedecay_domain::configuration::ProtectedApplyRequest,
) -> Result<(), ConfigurationError> {
    request
        .plan_id
        .validate()
        .map_err(ConfigurationError::validation)?;
    request
        .actor_id
        .validate()
        .map_err(ConfigurationError::validation)?;
    request
        .expected_base_revision_id
        .validate()
        .map_err(ConfigurationError::validation)?;
    request
        .operation_digest
        .validate()
        .map_err(ConfigurationError::validation)?;
    request
        .idempotency_key
        .validate()
        .map_err(ConfigurationError::validation)
}

pub(super) fn validate_plan_evidence(
    plan: &ProtectedChangePlan,
    evidence: &ScopeRevalidationEvidenceV1,
) -> Result<(), ConfigurationError> {
    if plan.resolved_scope_digest != evidence.resolved_scope_digest
        || plan.membership_digest != evidence.membership_digest
        || plan.authorization_policy_digest != evidence.authorization_policy_digest
        || plan.policy_epoch != evidence.policy_epoch
    {
        return Err(ConfigurationError::PlanStale);
    }
    Ok(())
}

pub(super) fn redacted_value_digest(
    value: Option<&ConfigurationValueV1>,
) -> Result<Option<ManifestDigest>, ConfigurationError> {
    value
        .map(|value| canonical_sha256(&("tracedecay.configuration.rollback-value.v1", value)))
        .transpose()
        .map_err(ConfigurationError::validation)
}

pub(super) fn rollback_redacted_changes(
    current: &ConfigurationSnapshotV1,
    target: &ConfigurationSnapshotV1,
) -> Result<Vec<RedactedConfigurationChangeV1>, ConfigurationError> {
    let keys = current
        .effective_values
        .keys()
        .chain(target.effective_values.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    keys.into_iter()
        .filter_map(|setting_key| {
            let before = current.effective_values.get(&setting_key);
            let after = target.effective_values.get(&setting_key);
            (before != after).then_some((setting_key, before, after))
        })
        .map(|(setting_key, before, after)| {
            Ok(RedactedConfigurationChangeV1 {
                setting_key,
                operation: ScopeControlOperationV1::Rollback,
                before_digest: redacted_value_digest(before)?,
                after_digest: redacted_value_digest(after)?,
            })
        })
        .collect()
}

pub(super) async fn current_state_from_transaction(
    transaction: &impl QueryExecutor,
) -> Result<ConfigurationCurrentStateV1, ConfigurationError> {
    let revision_id = current_revision_id_from_executor(transaction)
        .await
        .map_err(map_store_error)?;
    let revision = read_revision_from_executor(transaction, &revision_id)
        .await
        .map_err(map_store_error)?
        .ok_or_else(|| {
            ConfigurationError::validation_message("current configuration revision disappeared")
        })?;
    Ok(ConfigurationCurrentStateV1 {
        revision_id: revision.revision_id,
        snapshot: revision.snapshot,
    })
}

pub(super) async fn replay_control_receipt(
    transaction: &impl QueryExecutor,
    actor_id: &ActorId,
    idempotency_key: &ConfigurationIdempotencyKey,
    expected_base_revision_id: &ConfigurationRevisionId,
    operation_digest: &ManifestDigest,
    expected_plan_id: Option<&ChangePlanId>,
) -> Result<Option<ConfigurationMutationReceipt>, ConfigurationError> {
    let Some(stored) =
        receipt_for_idempotency_from_transaction(transaction, actor_id, idempotency_key)
            .await
            .map_err(map_store_error)?
    else {
        return Ok(None);
    };
    if stored.receipt.base_revision_id != *expected_base_revision_id
        || stored.receipt.operation_digest != *operation_digest
        || stored.plan_id.as_ref() != expected_plan_id
    {
        return Err(ConfigurationError::IdempotencyConflict);
    }
    let revision = read_revision_from_executor(transaction, &stored.receipt.result_revision_id)
        .await
        .map_err(map_store_error)?
        .ok_or_else(|| {
            ConfigurationError::validation_message(
                "configuration receipt result revision disappeared",
            )
        })?;
    Ok(Some(ConfigurationMutationReceipt {
        receipt_id: stored.receipt.receipt_id,
        base_revision_id: stored.receipt.base_revision_id,
        result_revision_id: stored.receipt.result_revision_id,
        snapshot_id: revision.snapshot.snapshot_id,
        operation_digest: stored.receipt.operation_digest,
        created_at: stored.receipt.created_at,
    }))
}

pub struct ConfigurationDirectCommitOutcomeV1 {
    pub receipt: ConfigurationMutationReceipt,
    pub current: ConfigurationCurrentStateV1,
}

pub async fn commit_direct_in_transaction<E>(
    transaction: &E,
    authority: &ConfigurationMutationAuthority,
    mutation: &DirectConfigurationMutation,
    expected_revision: &ConfigurationRevisionId,
) -> Result<ConfigurationDirectCommitOutcomeV1, ConfigurationError>
where
    E: QueryExecutor + Executor + Sync,
{
    authority.validate_integrity()?;
    expected_revision
        .validate()
        .map_err(ConfigurationError::validation)?;
    validate_direct_control_mutation(mutation)?;
    if authority.receipt.scope_digest != mutation.target_scope_digest()? {
        return Err(ConfigurationError::MutationAuthorityRejected);
    }
    let operation_digest = direct_operation_digest(mutation)?;
    let idempotency_key = direct_idempotency_key(authority, &operation_digest)?;
    let next_revision_id =
        result_revision_id(expected_revision, &idempotency_key, &operation_digest)?;
    if let Some(receipt) = replay_control_receipt(
        transaction,
        &authority.receipt.actor_id,
        &idempotency_key,
        expected_revision,
        &operation_digest,
        None,
    )
    .await?
    {
        let current = current_state_from_transaction(transaction).await?;
        return Ok(ConfigurationDirectCommitOutcomeV1 { receipt, current });
    }
    let current = current_state_from_transaction(transaction).await?;
    if &current.revision_id != expected_revision {
        return Err(ConfigurationError::RevisionConflict);
    }
    let snapshot = apply_direct_mutation_to_snapshot(
        &current.snapshot,
        mutation,
        &next_revision_id,
        &ConfigurationRegistry::core().map_err(ConfigurationError::validation)?,
    )?;
    let audit_target = redacted_direct_audit_target(mutation)?;
    let (commit, sealed_target_reference) = build_configuration_commit(
        transaction,
        ConfigurationCommitDraft {
            expected_base_revision_id: expected_revision,
            next_revision_id,
            snapshot,
            actor_id: &authority.receipt.actor_id,
            operation_kind: "direct_mutation",
            operation_digest,
            idempotency_key,
            change_plan: None,
            event_kind: ConfigurationAuditEventKindV1::Applied,
            created_at: authority.receipt.issued_at,
            target: &audit_target,
        },
    )
    .await?;
    let receipt = commit_configuration_transaction(
        transaction,
        &commit,
        false,
        Some(&sealed_target_reference),
    )
    .await
    .map_err(map_store_error)?;
    let receipt = ConfigurationMutationReceipt {
        receipt_id: receipt.receipt_id,
        base_revision_id: receipt.base_revision_id,
        result_revision_id: receipt.result_revision_id,
        snapshot_id: commit.next_revision.snapshot.snapshot_id,
        operation_digest: receipt.operation_digest,
        created_at: receipt.created_at,
    };
    let current = current_state_from_transaction(transaction).await?;
    Ok(ConfigurationDirectCommitOutcomeV1 { receipt, current })
}
