//! Configuration payload codecs and immutable projections.

use super::{
    ChangePlanId, ConfigurationAuditEvent, ConfigurationCandidateV1, ConfigurationError,
    ConfigurationMutationReceiptV1, ConfigurationRevisionId, ConfigurationSnapshotV1,
    ConfigurationStoreError, ConfigurationStoreResult, ConfigurationValueV1,
    DirectConfigurationMutation, Executor, ManifestDigest, Row, SettingKey,
};
use serde::{Deserialize, Serialize};
pub(super) use tracedecay_rusqlite_runtime::repository::{
    CONFIGURATION_PLAN_PAYLOAD_SCHEMA_VERSION, StoredChangePlanColumns,
    StoredConfigurationPlanPayload, StoredConfigurationProtectedOperationV1,
};

pub(super) const CONFIGURATION_SNAPSHOT_ENTRY_PAYLOAD_SCHEMA_VERSION: u16 = 1;
pub(super) const CONFIGURATION_AUDIT_PAYLOAD_SCHEMA_VERSION: u16 = 1;
pub(super) const CONFIGURATION_SEALED_AUDIT_TARGET_SCHEMA_VERSION: u16 = 1;

/// `configuration_entries` remains the per-setting storage table, but its
/// payload must retain the full resolver snapshot. The indexed layer columns
/// are copied only from an already-typed candidate; they never create or
/// upgrade an authority reference.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StoredConfigurationSnapshotEntryV1 {
    pub(super) schema_version: u16,
    pub(super) value: Option<ConfigurationValueV1>,
    pub(super) provenance: Vec<ConfigurationCandidateV1>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StoredConfigurationAuditPayloadV1 {
    pub(super) schema_version: u16,
    pub(super) event: ConfigurationAuditEvent,
}

/// This payload never crosses the audit read API. The current crypto contract
/// provides canonical integrity commitments, not a database-key encryption
/// lifecycle, so the reference is kept in a private BLOB while readers receive
/// only its event-scoped commitment.
#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SealedAuditTargetReferenceV1<T> {
    pub(super) schema_version: u16,
    pub(super) target: T,
}

#[derive(Debug)]
pub(super) struct StoredRevisionMetadata {
    pub(super) revision_id: String,
    pub(super) parent_revision_id: Option<String>,
    pub(super) snapshot_id: String,
    pub(super) effective_behavior_digest: String,
    pub(super) resolution_provenance_digest: String,
    pub(super) actor_id: String,
    pub(super) operation_kind: String,
    pub(super) created_at: i64,
}

#[derive(Debug)]
pub(super) struct StoredMutationReceipt {
    pub(super) receipt: ConfigurationMutationReceiptV1,
    pub(super) plan_id: Option<ChangePlanId>,
    pub(super) activation_status: String,
}

#[derive(Serialize)]
pub(super) struct RedactedDirectConfigurationAuditTargetV1 {
    target_scope_digest: ManifestDigest,
    setting_keys: Vec<SettingKey>,
}

pub(super) fn redacted_direct_audit_target(
    mutation: &DirectConfigurationMutation,
) -> Result<RedactedDirectConfigurationAuditTargetV1, ConfigurationError> {
    Ok(RedactedDirectConfigurationAuditTargetV1 {
        target_scope_digest: mutation.target_scope_digest()?,
        setting_keys: mutation.touched_keys()?.into_iter().collect(),
    })
}

pub(super) fn invalid_store_data(message: impl Into<String>) -> ConfigurationStoreError {
    ConfigurationStoreError::InvalidData(message.into())
}

pub(super) fn unavailable_store(error: impl std::fmt::Display) -> ConfigurationStoreError {
    ConfigurationStoreError::unavailable(error)
}

pub(super) fn decode_id<T>(value: String, field: &'static str) -> ConfigurationStoreResult<T>
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Display,
{
    T::try_from(value).map_err(|error| {
        invalid_store_data(format!("invalid stored configuration {field}: {error}"))
    })
}

/// Writes a revision's immutable projections. The statement rows come from
/// the shared repository module so both writers emit identical SQL.
pub(super) async fn insert_configuration_projections(
    transaction: &impl Executor,
    revision_id: &ConfigurationRevisionId,
    snapshot: &ConfigurationSnapshotV1,
) -> ConfigurationStoreResult<()> {
    let statements = tracedecay_rusqlite_runtime::repository::configuration_projection_statements(
        revision_id,
        snapshot,
    )
    .map_err(ConfigurationStoreError::from)?;
    for statement in statements {
        transaction
            .execute(
                &statement.sql,
                statement
                    .params
                    .into_iter()
                    .map(tracedecay_runtime_core::db::engine::Value::from)
                    .collect::<Vec<_>>(),
            )
            .await
            .map_err(unavailable_store)?;
    }
    Ok(())
}

/// Extracts the shared plan projection columns from one engine row, keeping
/// the per-column read errors. Validation of the stored record lives in
/// `decode_stored_change_plan_rows` so both writers decode identically.
pub(super) fn plan_row_columns(row: &Row) -> ConfigurationStoreResult<StoredChangePlanColumns> {
    Ok(StoredChangePlanColumns {
        plan_id: row
            .get::<String>(0)
            .map_err(|error| invalid_store_data(format!("read configuration plan id: {error}")))?,
        actor_id: row.get::<String>(1).map_err(|error| {
            invalid_store_data(format!("read configuration plan actor id: {error}"))
        })?,
        base_revision_id: row.get::<String>(2).map_err(|error| {
            invalid_store_data(format!("read configuration plan base revision id: {error}"))
        })?,
        operation_digest: row.get::<String>(3).map_err(|error| {
            invalid_store_data(format!("read configuration plan operation digest: {error}"))
        })?,
        resolved_scope_digest: row.get::<String>(4).map_err(|error| {
            invalid_store_data(format!("read configuration plan scope digest: {error}"))
        })?,
        membership_digest: row.get::<Option<String>>(5).map_err(|error| {
            invalid_store_data(format!(
                "read configuration plan membership digest: {error}"
            ))
        })?,
        authorization_policy_digest: row.get::<String>(6).map_err(|error| {
            invalid_store_data(format!("read configuration plan policy digest: {error}"))
        })?,
        policy_epoch: row.get::<i64>(7).map_err(|error| {
            invalid_store_data(format!("read configuration plan policy epoch: {error}"))
        })?,
        expires_at: row.get::<i64>(8).map_err(|error| {
            invalid_store_data(format!("read configuration plan expiry: {error}"))
        })?,
        created_at: row.get::<i64>(9).map_err(|error| {
            invalid_store_data(format!("read configuration plan creation time: {error}"))
        })?,
        operation_sequence: row.get::<Option<i64>>(10).map_err(|error| {
            invalid_store_data(format!(
                "read configuration plan operation sequence: {error}"
            ))
        })?,
        payload_schema_revision: row.get::<Option<i64>>(11).map_err(|error| {
            invalid_store_data(format!(
                "read configuration plan payload schema revision: {error}"
            ))
        })?,
        sealed_typed_operation: row.get::<Option<Vec<u8>>>(12).map_err(|error| {
            invalid_store_data(format!("read configuration plan sealed payload: {error}"))
        })?,
        operation_row_digest: row.get::<Option<String>>(13).map_err(|error| {
            invalid_store_data(format!(
                "read configuration plan operation digest payload: {error}"
            ))
        })?,
    })
}
