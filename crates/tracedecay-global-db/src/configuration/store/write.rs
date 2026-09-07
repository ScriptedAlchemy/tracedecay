use std::collections::BTreeSet;

use super::codec::{
    CONFIGURATION_PLAN_PAYLOAD_SCHEMA_VERSION, CONFIGURATION_SNAPSHOT_ENTRY_PAYLOAD_SCHEMA_VERSION,
    StoredConfigurationPlanPayloadV2, StoredConfigurationSnapshotEntryV1,
};
use super::{
    CandidateDispositionV1, ConfigurationCandidateV1, ConfigurationLayerIdV1,
    ConfigurationProtectedPlanRecordV1, ConfigurationRevisionId, ConfigurationSnapshotV1,
    ConfigurationStoreError, ConfigurationStoreResult, ConfigurationValueV1, Executor, SettingKey,
    invalid_store_data, params, unavailable_store,
};

fn encode_snapshot_entry(
    value: Option<ConfigurationValueV1>,
    provenance: Vec<ConfigurationCandidateV1>,
) -> ConfigurationStoreResult<String> {
    serde_json::to_string(&StoredConfigurationSnapshotEntryV1 {
        schema_version: CONFIGURATION_SNAPSHOT_ENTRY_PAYLOAD_SCHEMA_VERSION,
        value,
        provenance,
    })
    .map_err(|error| invalid_store_data(format!("encode configuration snapshot entry: {error}")))
}

fn snapshot_entry_layer(provenance: &[ConfigurationCandidateV1]) -> (&'static str, Option<String>) {
    let layer = provenance
        .iter()
        .find(|candidate| {
            matches!(
                candidate.disposition,
                CandidateDispositionV1::Winning | CandidateDispositionV1::Defaulted
            )
        })
        .or_else(|| provenance.first())
        .map(|candidate| &candidate.layer);
    match layer {
        Some(ConfigurationLayerIdV1::Default) | None => ("default", None),
        Some(ConfigurationLayerIdV1::UserProfile { profile_id }) => {
            ("user_profile", Some(profile_id.as_str().to_owned()))
        }
        Some(ConfigurationLayerIdV1::Project { project_id }) => {
            ("project", Some(project_id.as_str().to_owned()))
        }
        Some(ConfigurationLayerIdV1::Collection { collection_id }) => {
            ("collection", Some(collection_id.as_str().to_owned()))
        }
    }
}

pub(super) async fn insert_snapshot_entries(
    transaction: &impl Executor,
    revision_id: &ConfigurationRevisionId,
    snapshot: &ConfigurationSnapshotV1,
) -> ConfigurationStoreResult<()> {
    let keys: BTreeSet<SettingKey> = snapshot
        .effective_values
        .keys()
        .chain(snapshot.provenance.keys())
        .cloned()
        .collect();
    for key in keys {
        let value = snapshot.effective_values.get(&key).cloned();
        let provenance = snapshot.provenance.get(&key).cloned().unwrap_or_default();
        let (layer_kind, layer_id) = snapshot_entry_layer(&provenance);
        let encoded_entry = encode_snapshot_entry(value, provenance)?;
        transaction
            .execute(
                "INSERT INTO configuration_entries (
                    revision_id, key, layer_kind, layer_id, schema_revision, typed_value
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    revision_id.as_str(),
                    key.as_str(),
                    layer_kind,
                    layer_id,
                    i64::from(CONFIGURATION_SNAPSHOT_ENTRY_PAYLOAD_SCHEMA_VERSION),
                    encoded_entry,
                ],
            )
            .await
            .map_err(unavailable_store)?;
    }
    Ok(())
}

fn encode_plan_payload(
    plan: &ConfigurationProtectedPlanRecordV1,
) -> ConfigurationStoreResult<Vec<u8>> {
    plan.validate().map_err(ConfigurationStoreError::from)?;
    serde_json::to_vec(&StoredConfigurationPlanPayloadV2 {
        schema_version: CONFIGURATION_PLAN_PAYLOAD_SCHEMA_VERSION,
        plan: plan.plan.clone(),
        operation: (&plan.operation).into(),
    })
    .map_err(|error| invalid_store_data(format!("encode configuration plan payload: {error}")))
}

pub(super) async fn insert_change_plan(
    transaction: &impl Executor,
    plan: &ConfigurationProtectedPlanRecordV1,
) -> ConfigurationStoreResult<()> {
    plan.validate().map_err(ConfigurationStoreError::from)?;
    let payload = encode_plan_payload(plan)?;
    let membership_digest = plan
        .plan
        .membership_digest
        .as_ref()
        .map(|value| value.as_str().to_owned());
    transaction
        .execute(
            "INSERT INTO configuration_change_plans (
                plan_id, actor_id, base_revision_id, operation_digest,
                resolved_scope_digest, membership_digest, authorization_policy_digest,
                policy_epoch, expires_at, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                plan.plan.plan_id.as_str(),
                plan.plan.actor_id.as_str(),
                plan.plan.base_revision_id.as_str(),
                plan.plan.operation_digest.as_str(),
                plan.plan.resolved_scope_digest.as_str(),
                membership_digest,
                plan.plan.authorization_policy_digest.as_str(),
                i64::try_from(plan.plan.policy_epoch).map_err(|_| {
                    invalid_store_data(
                        "configuration plan policy epoch exceeds SQLite integer range",
                    )
                })?,
                plan.plan.expires_at.0,
                plan.plan.created_at.0,
            ],
        )
        .await
        .map_err(unavailable_store)?;
    transaction
        .execute(
            "INSERT INTO configuration_change_plan_operations (
                plan_id, sequence, payload_schema_revision, sealed_typed_operation, operation_digest
             ) VALUES (?1, 0, ?2, ?3, ?4)",
            params![
                plan.plan.plan_id.as_str(),
                i64::from(CONFIGURATION_PLAN_PAYLOAD_SCHEMA_VERSION),
                payload,
                plan.plan.operation_digest.as_str(),
            ],
        )
        .await
        .map_err(unavailable_store)?;
    transaction
        .execute(
            "INSERT INTO configuration_change_plan_events (
                plan_id, sequence, event_kind, safe_reason_code, occurred_at
             ) VALUES (?1, 0, 'dry_run_created', NULL, ?2)",
            params![plan.plan.plan_id.as_str(), plan.plan.created_at.0],
        )
        .await
        .map_err(unavailable_store)?;
    Ok(())
}
