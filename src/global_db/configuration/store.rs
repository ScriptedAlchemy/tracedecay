//! SQLite primitives for configuration migration receipts and quarantine.
//!
//! The shared global-db lifecycle wires this adapter into its transaction
//! boundary. This module intentionally does not register itself or mutate the
//! legacy configuration file.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;

use libsql::{Connection, Row, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::configuration::{
    CandidateDispositionV1, ChangePlanId, ConfigurationAuditEvent, ConfigurationAuditEventId,
    ConfigurationAuditEventKindV1, ConfigurationCandidateV1, ConfigurationIdempotencyKey,
    ConfigurationLayerIdV1, ConfigurationReceiptId, ConfigurationRevisionId,
    ConfigurationSnapshotId, ConfigurationSnapshotV1, ConfigurationValueV1, ProtectedChangePlan,
    SettingKey,
};
use tracedecay_domain::{ActorId, ManifestDigest, UtcMicros, canonical_sha256};
use tracedecay_store::configuration::{
    ConfigurationCommitV1, ConfigurationMutationReceiptV1, ConfigurationRevisionRecordV1,
    ConfigurationRevisionStore, ConfigurationStoreError, ConfigurationStoreResult,
};

use super::migration::{
    CONFIGURATION_CONTROL_PLANE_MIGRATION_RECEIPT_NAME, ConfigurationMigrationError,
    ConfigurationMigrationQuarantineEntryV1, ConfigurationMigrationReceiptV1,
    ConfigurationMigrationStore, LegacyConfigurationSourceKindV1,
};
use super::schema::{ConfigurationSchemaError, ensure_configuration_schema};
use crate::config::resolver::ConfigurationResolutionV1;

#[derive(Debug, Error)]
pub enum ConfigurationStorageError {
    #[error("configuration schema error: {0}")]
    Schema(#[from] ConfigurationSchemaError),
    #[error("configuration storage error: {0}")]
    Sql(#[from] libsql::Error),
    #[error("configuration storage encoded invalid data: {0}")]
    Encoding(String),
}

/// Narrow asynchronous SQLite adapter. It owns no migration registration or
/// configuration policy logic; callers must supply validated typed values.
pub struct ConfigurationSqlStore<'a> {
    connection: &'a Connection,
}

impl<'a> ConfigurationSqlStore<'a> {
    pub fn new(connection: &'a Connection) -> Self {
        Self { connection }
    }

    pub async fn ensure_schema(&self) -> Result<(), ConfigurationStorageError> {
        ensure_configuration_schema(self.connection).await?;
        Ok(())
    }

    pub async fn migration_receipt(
        &self,
        receipt_name: &str,
        source_snapshot_digest: &ManifestDigest,
    ) -> Result<Option<ConfigurationMigrationReceiptV1>, ConfigurationStorageError> {
        let mut rows = self
            .connection
            .query(
                "SELECT initial_revision_id, initial_snapshot_id, created_at
                 FROM configuration_migration_receipts
                 WHERE receipt_name = ?1 AND source_snapshot_digest = ?2",
                params![receipt_name, source_snapshot_digest.as_str()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let initial_revision_id = ConfigurationRevisionId::new(row.get::<String>(0)?)
            .map_err(|error| ConfigurationStorageError::Encoding(error.to_string()))?;
        let initial_snapshot_id = ConfigurationSnapshotId::new(row.get::<String>(1)?)
            .map_err(|error| ConfigurationStorageError::Encoding(error.to_string()))?;
        let created_at = tracedecay_domain::UtcMicros(row.get::<i64>(2)?);
        let receipt_name = match receipt_name {
            super::migration::CONFIGURATION_CONTROL_PLANE_MIGRATION_RECEIPT_NAME => {
                super::migration::CONFIGURATION_CONTROL_PLANE_MIGRATION_RECEIPT_NAME
            }
            _ => {
                return Err(ConfigurationStorageError::Encoding(
                    "unrecognized configuration migration receipt name".to_owned(),
                ));
            }
        };
        Ok(Some(ConfigurationMigrationReceiptV1 {
            receipt_name,
            source_snapshot_digest: source_snapshot_digest.clone(),
            initial_revision_id,
            initial_snapshot_id,
            created_at,
        }))
    }
}

fn source_kind_name(source_kind: LegacyConfigurationSourceKindV1) -> &'static str {
    source_kind.as_str()
}

impl ConfigurationMigrationStore for ConfigurationSqlStore<'_> {
    fn receipt(
        &self,
        receipt_name: &'static str,
        source_snapshot_digest: &ManifestDigest,
    ) -> impl Future<
        Output = Result<Option<ConfigurationMigrationReceiptV1>, ConfigurationMigrationError>,
    > + Send {
        async move {
            self.migration_receipt(receipt_name, source_snapshot_digest)
                .await
                .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))
        }
    }

    fn commit_initial_migration(
        &self,
        receipt: &ConfigurationMigrationReceiptV1,
        resolution: &ConfigurationResolutionV1,
        quarantine: &[ConfigurationMigrationQuarantineEntryV1],
    ) -> impl Future<Output = Result<(), ConfigurationMigrationError>> + Send {
        async move {
            resolution
                .snapshot
                .validate()
                .map_err(ConfigurationMigrationError::Domain)?;
            if receipt.receipt_name != CONFIGURATION_CONTROL_PLANE_MIGRATION_RECEIPT_NAME
                || receipt.initial_snapshot_id != resolution.snapshot.snapshot_id
            {
                return Err(ConfigurationMigrationError::Store(
                    "migration receipt does not bind the initial snapshot".to_owned(),
                ));
            }

            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .await
                .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?;
            let outcome = commit_initial_migration_transaction(
                &transaction,
                receipt,
                resolution,
                quarantine,
                false,
            )
            .await;
            match outcome {
                Ok(()) => transaction
                    .commit()
                    .await
                    .map_err(|error| ConfigurationMigrationError::Store(error.to_string())),
                Err(error) => {
                    let _ = transaction.rollback().await;
                    Err(error)
                }
            }
        }
    }
}

async fn commit_initial_migration_transaction(
    transaction: &libsql::Transaction,
    receipt: &ConfigurationMigrationReceiptV1,
    resolution: &ConfigurationResolutionV1,
    quarantine: &[ConfigurationMigrationQuarantineEntryV1],
    fault_after_receipt: bool,
) -> Result<(), ConfigurationMigrationError> {
    if migration_receipt_exists(
        transaction,
        receipt.receipt_name,
        &receipt.source_snapshot_digest,
        &receipt.initial_revision_id,
        &receipt.initial_snapshot_id,
    )
    .await?
    {
        return Ok(());
    }

    transaction
        .execute(
            "INSERT INTO configuration_revisions (
                revision_id, parent_revision_id, snapshot_id,
                effective_behavior_digest, resolution_provenance_digest,
                actor_id, operation_kind, created_at
             ) VALUES (?1, NULL, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                receipt.initial_revision_id.as_str(),
                receipt.initial_snapshot_id.as_str(),
                resolution.snapshot.effective_behavior_digest.as_str(),
                resolution.snapshot.resolution_provenance_digest.as_str(),
                "actor.configuration-migration",
                "legacy_read_only_migration",
                receipt.created_at.0,
            ],
        )
        .await
        .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?;

    insert_snapshot_entries(
        transaction,
        &receipt.initial_revision_id,
        &resolution.snapshot,
    )
    .await
    .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?;

    for entry in quarantine {
        transaction
            .execute(
                "INSERT OR IGNORE INTO configuration_migration_quarantine (
                    source_kind, source_key_digest, reason_code,
                    redacted_value_digest, quarantined_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    source_kind_name(entry.source_kind),
                    entry.source_key_digest.as_str(),
                    entry.reason.as_str(),
                    entry.redacted_value_digest.as_str(),
                    entry.quarantined_at.0,
                ],
            )
            .await
            .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?;
    }

    transaction
        .execute(
            "INSERT INTO configuration_migration_receipts (
                receipt_name, source_snapshot_digest, initial_revision_id,
                initial_snapshot_id, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                receipt.receipt_name,
                receipt.source_snapshot_digest.as_str(),
                receipt.initial_revision_id.as_str(),
                receipt.initial_snapshot_id.as_str(),
                receipt.created_at.0,
            ],
        )
        .await
        .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?;

    if fault_after_receipt {
        return Err(ConfigurationMigrationError::Store(
            "injected migration crash after receipt".to_owned(),
        ));
    }

    let audit_digest = canonical_sha256(&(
        "tracedecay.configuration.migration-audit.v1",
        receipt.receipt_name,
        &receipt.source_snapshot_digest,
        &receipt.initial_revision_id,
        &receipt.initial_snapshot_id,
    ))
    .map_err(ConfigurationMigrationError::Domain)?;
    let audit_suffix = audit_digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or_else(|| ConfigurationMigrationError::Store("invalid audit digest".to_owned()))?;
    let audit_event = ConfigurationAuditEvent {
        event_id: decode_id(
            format!("configuration.audit.migration.{audit_suffix}"),
            "migration audit event id",
        )
        .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?,
        event_kind: ConfigurationAuditEventKindV1::Recovered,
        actor_id: decode_id(
            "actor.configuration-migration".to_owned(),
            "migration actor id",
        )
        .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?,
        idempotency_key: None,
        base_revision_id: receipt.initial_revision_id.clone(),
        result_revision_id: Some(receipt.initial_revision_id.clone()),
        operation_digest: audit_digest.clone(),
        target_commitment: audit_digest,
        receipt_id: None,
        safe_reason_code: None,
        occurred_at: receipt.created_at,
    };
    insert_audit_event_with_receipt_digest(transaction, &audit_event, None)
        .await
        .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?;
    Ok(())
}

async fn migration_receipt_exists(
    transaction: &libsql::Transaction,
    receipt_name: &str,
    source_snapshot_digest: &ManifestDigest,
    initial_revision_id: &ConfigurationRevisionId,
    initial_snapshot_id: &ConfigurationSnapshotId,
) -> Result<bool, ConfigurationMigrationError> {
    let mut rows = transaction
        .query(
            "SELECT initial_revision_id, initial_snapshot_id
             FROM configuration_migration_receipts
             WHERE receipt_name = ?1 AND source_snapshot_digest = ?2",
            params![receipt_name, source_snapshot_digest.as_str()],
        )
        .await
        .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?
    else {
        return Ok(false);
    };
    let stored_revision = row
        .get::<String>(0)
        .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?;
    let stored_snapshot = row
        .get::<String>(1)
        .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?;
    if stored_revision != initial_revision_id.as_str()
        || stored_snapshot != initial_snapshot_id.as_str()
    {
        return Err(ConfigurationMigrationError::Store(
            "configuration migration replay conflicts with stored receipt".to_owned(),
        ));
    }
    Ok(true)
}

const CONFIGURATION_SNAPSHOT_ENTRY_PAYLOAD_SCHEMA_VERSION: u16 = 1;
const CONFIGURATION_PLAN_PAYLOAD_SCHEMA_VERSION: u16 = 1;
const CONFIGURATION_AUDIT_PAYLOAD_SCHEMA_VERSION: u16 = 1;
const CONFIGURATION_AUTHORIZATION_NOT_RECORDED: &str = "not_recorded_by_configuration_store_v1";
const CONFIGURATION_ACTIVATION_NOT_RECORDED: &str = "not_recorded_by_configuration_store_v1";

/// `configuration_entries` remains the per-setting storage table, but its
/// payload must retain the full resolver snapshot. The indexed layer columns
/// are copied only from an already-typed candidate; they never create or
/// upgrade an authority reference.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredConfigurationSnapshotEntryV1 {
    schema_version: u16,
    value: Option<ConfigurationValueV1>,
    provenance: Vec<ConfigurationCandidateV1>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredConfigurationPlanPayloadV1 {
    schema_version: u16,
    plan: ProtectedChangePlan,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredConfigurationAuditPayloadV1 {
    schema_version: u16,
    event: ConfigurationAuditEvent,
}

#[derive(Debug)]
struct StoredRevisionMetadata {
    revision_id: String,
    parent_revision_id: Option<String>,
    snapshot_id: String,
    effective_behavior_digest: String,
    resolution_provenance_digest: String,
    actor_id: String,
    operation_kind: String,
    created_at: i64,
}

#[derive(Debug)]
struct StoredMutationReceipt {
    receipt: ConfigurationMutationReceiptV1,
    plan_id: Option<ChangePlanId>,
    authorization_policy_digest: String,
    activation_status: String,
}

fn invalid_store_data(message: impl Into<String>) -> ConfigurationStoreError {
    ConfigurationStoreError::InvalidData(message.into())
}

fn unavailable_store(_error: libsql::Error) -> ConfigurationStoreError {
    ConfigurationStoreError::Unavailable
}

fn decode_id<T>(value: String, field: &'static str) -> ConfigurationStoreResult<T>
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Display,
{
    T::try_from(value).map_err(|error| {
        invalid_store_data(format!("invalid stored configuration {field}: {error}"))
    })
}

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

fn decode_snapshot_entry(
    value: &str,
) -> ConfigurationStoreResult<StoredConfigurationSnapshotEntryV1> {
    let entry =
        serde_json::from_str::<StoredConfigurationSnapshotEntryV1>(value).map_err(|error| {
            invalid_store_data(format!("decode configuration snapshot entry: {error}"))
        })?;
    if entry.schema_version != CONFIGURATION_SNAPSHOT_ENTRY_PAYLOAD_SCHEMA_VERSION {
        return Err(invalid_store_data(
            "unsupported configuration snapshot entry payload schema version",
        ));
    }
    Ok(entry)
}

fn snapshot_from_entries(
    entries: Vec<(String, i64, String)>,
    expected_snapshot_id: &str,
    expected_behavior_digest: &str,
    expected_provenance_digest: &str,
) -> ConfigurationStoreResult<ConfigurationSnapshotV1> {
    let mut effective_values = BTreeMap::new();
    let mut provenance = BTreeMap::new();

    for (stored_key, schema_revision, encoded_entry) in entries {
        if schema_revision != i64::from(CONFIGURATION_SNAPSHOT_ENTRY_PAYLOAD_SCHEMA_VERSION) {
            return Err(invalid_store_data(
                "unsupported configuration entry schema revision",
            ));
        }
        let key = SettingKey::new(stored_key).map_err(|error| {
            invalid_store_data(format!("invalid stored configuration key: {error}"))
        })?;
        let entry = decode_snapshot_entry(&encoded_entry)?;
        if entry.value.is_none() && entry.provenance.is_empty() {
            return Err(invalid_store_data(
                "configuration snapshot entry has neither value nor provenance",
            ));
        }
        if effective_values.contains_key(&key) || provenance.contains_key(&key) {
            return Err(invalid_store_data(
                "configuration snapshot contains duplicate setting entries",
            ));
        }
        if let Some(value) = entry.value {
            effective_values.insert(key.clone(), value);
        }
        if !entry.provenance.is_empty() {
            provenance.insert(key, entry.provenance);
        }
    }

    let snapshot = ConfigurationSnapshotV1::new(effective_values, provenance)
        .map_err(ConfigurationStoreError::from)?;
    if snapshot.snapshot_id.as_str() != expected_snapshot_id
        || snapshot.effective_behavior_digest.as_str() != expected_behavior_digest
        || snapshot.resolution_provenance_digest.as_str() != expected_provenance_digest
    {
        return Err(invalid_store_data(
            "stored configuration snapshot payload does not match revision metadata",
        ));
    }
    Ok(snapshot)
}

async fn snapshot_from_connection(
    connection: &Connection,
    revision_id: &ConfigurationRevisionId,
    expected_snapshot_id: &str,
    expected_behavior_digest: &str,
    expected_provenance_digest: &str,
) -> ConfigurationStoreResult<ConfigurationSnapshotV1> {
    let mut rows = connection
        .query(
            "SELECT key, schema_revision, typed_value
             FROM configuration_entries
             WHERE revision_id = ?1
             ORDER BY key ASC",
            params![revision_id.as_str()],
        )
        .await
        .map_err(unavailable_store)?;
    let mut entries = Vec::new();
    while let Some(row) = rows.next().await.map_err(unavailable_store)? {
        entries.push((
            row.get::<String>(0).map_err(|error| {
                invalid_store_data(format!("read configuration entry key: {error}"))
            })?,
            row.get::<i64>(1).map_err(|error| {
                invalid_store_data(format!("read configuration entry schema revision: {error}"))
            })?,
            row.get::<String>(2).map_err(|error| {
                invalid_store_data(format!("read configuration entry payload: {error}"))
            })?,
        ));
    }
    drop(rows);
    snapshot_from_entries(
        entries,
        expected_snapshot_id,
        expected_behavior_digest,
        expected_provenance_digest,
    )
}

async fn snapshot_from_transaction(
    transaction: &Transaction,
    revision_id: &ConfigurationRevisionId,
    expected_snapshot_id: &str,
    expected_behavior_digest: &str,
    expected_provenance_digest: &str,
) -> ConfigurationStoreResult<ConfigurationSnapshotV1> {
    let mut rows = transaction
        .query(
            "SELECT key, schema_revision, typed_value
             FROM configuration_entries
             WHERE revision_id = ?1
             ORDER BY key ASC",
            params![revision_id.as_str()],
        )
        .await
        .map_err(unavailable_store)?;
    let mut entries = Vec::new();
    while let Some(row) = rows.next().await.map_err(unavailable_store)? {
        entries.push((
            row.get::<String>(0).map_err(|error| {
                invalid_store_data(format!("read configuration entry key: {error}"))
            })?,
            row.get::<i64>(1).map_err(|error| {
                invalid_store_data(format!("read configuration entry schema revision: {error}"))
            })?,
            row.get::<String>(2).map_err(|error| {
                invalid_store_data(format!("read configuration entry payload: {error}"))
            })?,
        ));
    }
    drop(rows);
    snapshot_from_entries(
        entries,
        expected_snapshot_id,
        expected_behavior_digest,
        expected_provenance_digest,
    )
}

fn decode_revision_metadata(row: &Row) -> ConfigurationStoreResult<StoredRevisionMetadata> {
    Ok(StoredRevisionMetadata {
        revision_id: row.get::<String>(0).map_err(|error| {
            invalid_store_data(format!("read configuration revision id: {error}"))
        })?,
        parent_revision_id: row.get::<Option<String>>(1).map_err(|error| {
            invalid_store_data(format!("read configuration parent revision id: {error}"))
        })?,
        snapshot_id: row.get::<String>(2).map_err(|error| {
            invalid_store_data(format!("read configuration snapshot id: {error}"))
        })?,
        effective_behavior_digest: row.get::<String>(3).map_err(|error| {
            invalid_store_data(format!("read configuration behavior digest: {error}"))
        })?,
        resolution_provenance_digest: row.get::<String>(4).map_err(|error| {
            invalid_store_data(format!("read configuration provenance digest: {error}"))
        })?,
        actor_id: row
            .get::<String>(5)
            .map_err(|error| invalid_store_data(format!("read configuration actor id: {error}")))?,
        operation_kind: row.get::<String>(6).map_err(|error| {
            invalid_store_data(format!("read configuration operation kind: {error}"))
        })?,
        created_at: row.get::<i64>(7).map_err(|error| {
            invalid_store_data(format!("read configuration creation time: {error}"))
        })?,
    })
}

fn revision_from_metadata(
    metadata: StoredRevisionMetadata,
    snapshot: ConfigurationSnapshotV1,
) -> ConfigurationStoreResult<ConfigurationRevisionRecordV1> {
    let revision_id: ConfigurationRevisionId = decode_id(metadata.revision_id, "revision id")?;
    let parent_revision_id: Option<ConfigurationRevisionId> = metadata
        .parent_revision_id
        .map(|value| decode_id(value, "parent revision id"))
        .transpose()?;
    let actor_id: ActorId = decode_id(metadata.actor_id, "actor id")?;
    let record = ConfigurationRevisionRecordV1 {
        revision_id,
        parent_revision_id,
        snapshot,
        actor_id,
        operation_kind: metadata.operation_kind,
        created_at: UtcMicros(metadata.created_at),
    };
    record.validate().map_err(ConfigurationStoreError::from)?;
    Ok(record)
}

async fn read_revision_from_connection(
    connection: &Connection,
    revision_id: &ConfigurationRevisionId,
) -> ConfigurationStoreResult<Option<ConfigurationRevisionRecordV1>> {
    let mut rows = connection
        .query(
            "SELECT revision_id, parent_revision_id, snapshot_id,
                    effective_behavior_digest, resolution_provenance_digest,
                    actor_id, operation_kind, created_at
             FROM configuration_revisions
             WHERE revision_id = ?1",
            params![revision_id.as_str()],
        )
        .await
        .map_err(unavailable_store)?;
    let Some(row) = rows.next().await.map_err(unavailable_store)? else {
        return Ok(None);
    };
    let metadata = decode_revision_metadata(&row)?;
    if rows.next().await.map_err(unavailable_store)?.is_some() {
        return Err(invalid_store_data(
            "configuration revision id resolved to multiple rows",
        ));
    }
    drop(rows);
    let snapshot = snapshot_from_connection(
        connection,
        revision_id,
        &metadata.snapshot_id,
        &metadata.effective_behavior_digest,
        &metadata.resolution_provenance_digest,
    )
    .await?;
    Ok(Some(revision_from_metadata(metadata, snapshot)?))
}

async fn read_revision_from_transaction(
    transaction: &Transaction,
    revision_id: &ConfigurationRevisionId,
) -> ConfigurationStoreResult<Option<ConfigurationRevisionRecordV1>> {
    let mut rows = transaction
        .query(
            "SELECT revision_id, parent_revision_id, snapshot_id,
                    effective_behavior_digest, resolution_provenance_digest,
                    actor_id, operation_kind, created_at
             FROM configuration_revisions
             WHERE revision_id = ?1",
            params![revision_id.as_str()],
        )
        .await
        .map_err(unavailable_store)?;
    let Some(row) = rows.next().await.map_err(unavailable_store)? else {
        return Ok(None);
    };
    let metadata = decode_revision_metadata(&row)?;
    if rows.next().await.map_err(unavailable_store)?.is_some() {
        return Err(invalid_store_data(
            "configuration revision id resolved to multiple rows",
        ));
    }
    drop(rows);
    let snapshot = snapshot_from_transaction(
        transaction,
        revision_id,
        &metadata.snapshot_id,
        &metadata.effective_behavior_digest,
        &metadata.resolution_provenance_digest,
    )
    .await?;
    Ok(Some(revision_from_metadata(metadata, snapshot)?))
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

async fn insert_snapshot_entries(
    transaction: &Transaction,
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

async fn insert_revision(
    transaction: &Transaction,
    revision: &ConfigurationRevisionRecordV1,
) -> ConfigurationStoreResult<()> {
    revision.validate().map_err(ConfigurationStoreError::from)?;
    let parent_revision_id = revision
        .parent_revision_id
        .as_ref()
        .map(|value| value.as_str().to_owned());
    transaction
        .execute(
            "INSERT INTO configuration_revisions (
                revision_id, parent_revision_id, snapshot_id,
                effective_behavior_digest, resolution_provenance_digest,
                actor_id, operation_kind, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                revision.revision_id.as_str(),
                parent_revision_id,
                revision.snapshot.snapshot_id.as_str(),
                revision.snapshot.effective_behavior_digest.as_str(),
                revision.snapshot.resolution_provenance_digest.as_str(),
                revision.actor_id.as_str(),
                revision.operation_kind.as_str(),
                revision.created_at.0,
            ],
        )
        .await
        .map_err(unavailable_store)?;
    insert_snapshot_entries(transaction, &revision.revision_id, &revision.snapshot).await
}

fn encode_plan_payload(plan: &ProtectedChangePlan) -> ConfigurationStoreResult<Vec<u8>> {
    serde_json::to_vec(&StoredConfigurationPlanPayloadV1 {
        schema_version: CONFIGURATION_PLAN_PAYLOAD_SCHEMA_VERSION,
        plan: plan.clone(),
    })
    .map_err(|error| invalid_store_data(format!("encode configuration plan payload: {error}")))
}

fn decode_plan_row(row: &Row) -> ConfigurationStoreResult<ProtectedChangePlan> {
    let stored_plan_id = row
        .get::<String>(0)
        .map_err(|error| invalid_store_data(format!("read configuration plan id: {error}")))?;
    let stored_actor_id = row.get::<String>(1).map_err(|error| {
        invalid_store_data(format!("read configuration plan actor id: {error}"))
    })?;
    let stored_base_revision_id = row.get::<String>(2).map_err(|error| {
        invalid_store_data(format!("read configuration plan base revision id: {error}"))
    })?;
    let stored_operation_digest = row.get::<String>(3).map_err(|error| {
        invalid_store_data(format!("read configuration plan operation digest: {error}"))
    })?;
    let stored_scope_digest = row.get::<String>(4).map_err(|error| {
        invalid_store_data(format!("read configuration plan scope digest: {error}"))
    })?;
    let stored_membership_digest = row.get::<Option<String>>(5).map_err(|error| {
        invalid_store_data(format!(
            "read configuration plan membership digest: {error}"
        ))
    })?;
    let stored_policy_digest = row.get::<String>(6).map_err(|error| {
        invalid_store_data(format!("read configuration plan policy digest: {error}"))
    })?;
    let stored_policy_epoch = row.get::<i64>(7).map_err(|error| {
        invalid_store_data(format!("read configuration plan policy epoch: {error}"))
    })?;
    let stored_expires_at = row
        .get::<i64>(8)
        .map_err(|error| invalid_store_data(format!("read configuration plan expiry: {error}")))?;
    let stored_created_at = row.get::<i64>(9).map_err(|error| {
        invalid_store_data(format!("read configuration plan creation time: {error}"))
    })?;
    let sequence = row.get::<Option<i64>>(10).map_err(|error| {
        invalid_store_data(format!(
            "read configuration plan operation sequence: {error}"
        ))
    })?;
    let payload_schema_revision = row.get::<Option<i64>>(11).map_err(|error| {
        invalid_store_data(format!(
            "read configuration plan payload schema revision: {error}"
        ))
    })?;
    let sealed_payload = row.get::<Option<Vec<u8>>>(12).map_err(|error| {
        invalid_store_data(format!("read configuration plan sealed payload: {error}"))
    })?;
    let operation_digest = row.get::<Option<String>>(13).map_err(|error| {
        invalid_store_data(format!(
            "read configuration plan operation digest payload: {error}"
        ))
    })?;

    if sequence != Some(0)
        || payload_schema_revision != Some(i64::from(CONFIGURATION_PLAN_PAYLOAD_SCHEMA_VERSION))
    {
        return Err(invalid_store_data(
            "configuration plan does not contain its canonical initial operation payload",
        ));
    }
    let Some(sealed_payload) = sealed_payload else {
        return Err(invalid_store_data(
            "configuration plan operation payload is missing",
        ));
    };
    let payload = serde_json::from_slice::<StoredConfigurationPlanPayloadV1>(&sealed_payload)
        .map_err(|error| {
            invalid_store_data(format!("decode configuration plan payload: {error}"))
        })?;
    if payload.schema_version != CONFIGURATION_PLAN_PAYLOAD_SCHEMA_VERSION {
        return Err(invalid_store_data(
            "unsupported configuration plan payload schema version",
        ));
    }
    payload
        .plan
        .validate()
        .map_err(ConfigurationStoreError::from)?;
    let stored_policy_epoch = u64::try_from(stored_policy_epoch)
        .map_err(|_| invalid_store_data("configuration plan policy epoch is negative"))?;
    if payload.plan.plan_id.as_str() != stored_plan_id
        || payload.plan.actor_id.as_str() != stored_actor_id
        || payload.plan.base_revision_id.as_str() != stored_base_revision_id
        || payload.plan.operation_digest.as_str() != stored_operation_digest
        || payload.plan.operation_digest.as_str() != operation_digest.as_deref().unwrap_or_default()
        || payload.plan.resolved_scope_digest.as_str() != stored_scope_digest
        || payload
            .plan
            .membership_digest
            .as_ref()
            .map(|value| value.as_str())
            != stored_membership_digest.as_deref()
        || payload.plan.authorization_policy_digest.as_str() != stored_policy_digest
        || payload.plan.policy_epoch != stored_policy_epoch
        || payload.plan.expires_at.0 != stored_expires_at
        || payload.plan.created_at.0 != stored_created_at
    {
        return Err(invalid_store_data(
            "configuration plan payload does not match immutable projections",
        ));
    }
    Ok(payload.plan)
}

async fn read_change_plan_from_connection(
    connection: &Connection,
    plan_id: &ChangePlanId,
) -> ConfigurationStoreResult<Option<ProtectedChangePlan>> {
    let mut rows = connection
        .query(
            "SELECT p.plan_id, p.actor_id, p.base_revision_id, p.operation_digest,
                    p.resolved_scope_digest, p.membership_digest,
                    p.authorization_policy_digest, p.policy_epoch, p.expires_at, p.created_at,
                    o.sequence, o.payload_schema_revision, o.sealed_typed_operation,
                    o.operation_digest
             FROM configuration_change_plans p
             LEFT JOIN configuration_change_plan_operations o ON o.plan_id = p.plan_id
             WHERE p.plan_id = ?1
             ORDER BY o.sequence ASC",
            params![plan_id.as_str()],
        )
        .await
        .map_err(unavailable_store)?;
    let mut plans = Vec::new();
    while let Some(row) = rows.next().await.map_err(unavailable_store)? {
        plans.push(decode_plan_row(&row)?);
    }
    if plans.len() > 1 {
        return Err(invalid_store_data(
            "configuration plan has multiple operation payloads unsupported by this contract",
        ));
    }
    Ok(plans.pop())
}

async fn read_change_plan_from_transaction(
    transaction: &Transaction,
    plan_id: &ChangePlanId,
) -> ConfigurationStoreResult<Option<ProtectedChangePlan>> {
    let mut rows = transaction
        .query(
            "SELECT p.plan_id, p.actor_id, p.base_revision_id, p.operation_digest,
                    p.resolved_scope_digest, p.membership_digest,
                    p.authorization_policy_digest, p.policy_epoch, p.expires_at, p.created_at,
                    o.sequence, o.payload_schema_revision, o.sealed_typed_operation,
                    o.operation_digest
             FROM configuration_change_plans p
             LEFT JOIN configuration_change_plan_operations o ON o.plan_id = p.plan_id
             WHERE p.plan_id = ?1
             ORDER BY o.sequence ASC",
            params![plan_id.as_str()],
        )
        .await
        .map_err(unavailable_store)?;
    let mut plans = Vec::new();
    while let Some(row) = rows.next().await.map_err(unavailable_store)? {
        plans.push(decode_plan_row(&row)?);
    }
    if plans.len() > 1 {
        return Err(invalid_store_data(
            "configuration plan has multiple operation payloads unsupported by this contract",
        ));
    }
    Ok(plans.pop())
}

async fn insert_change_plan(
    transaction: &Transaction,
    plan: &ProtectedChangePlan,
) -> ConfigurationStoreResult<()> {
    plan.validate().map_err(ConfigurationStoreError::from)?;
    let payload = encode_plan_payload(plan)?;
    let membership_digest = plan
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
                plan.plan_id.as_str(),
                plan.actor_id.as_str(),
                plan.base_revision_id.as_str(),
                plan.operation_digest.as_str(),
                plan.resolved_scope_digest.as_str(),
                membership_digest,
                plan.authorization_policy_digest.as_str(),
                i64::try_from(plan.policy_epoch).map_err(|_| {
                    invalid_store_data(
                        "configuration plan policy epoch exceeds SQLite integer range",
                    )
                })?,
                plan.expires_at.0,
                plan.created_at.0,
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
                plan.plan_id.as_str(),
                i64::from(CONFIGURATION_PLAN_PAYLOAD_SCHEMA_VERSION),
                payload,
                plan.operation_digest.as_str(),
            ],
        )
        .await
        .map_err(unavailable_store)?;
    transaction
        .execute(
            "INSERT INTO configuration_change_plan_events (
                plan_id, sequence, event_kind, safe_reason_code, occurred_at
             ) VALUES (?1, 0, 'dry_run_created', NULL, ?2)",
            params![plan.plan_id.as_str(), plan.created_at.0],
        )
        .await
        .map_err(unavailable_store)?;
    Ok(())
}

fn encode_audit_payload(event: &ConfigurationAuditEvent) -> ConfigurationStoreResult<String> {
    serde_json::to_string(&StoredConfigurationAuditPayloadV1 {
        schema_version: CONFIGURATION_AUDIT_PAYLOAD_SCHEMA_VERSION,
        event: event.clone(),
    })
    .map_err(|error| invalid_store_data(format!("encode configuration audit payload: {error}")))
}

fn decode_audit_row(row: &Row) -> ConfigurationStoreResult<ConfigurationAuditEvent> {
    let stored_event_id = row.get::<String>(0).map_err(|error| {
        invalid_store_data(format!("read configuration audit event id: {error}"))
    })?;
    let stored_actor_id = row.get::<String>(1).map_err(|error| {
        invalid_store_data(format!("read configuration audit actor id: {error}"))
    })?;
    let stored_idempotency_key = row.get::<Option<String>>(2).map_err(|error| {
        invalid_store_data(format!("read configuration audit idempotency key: {error}"))
    })?;
    let encoded_payload = row.get::<String>(3).map_err(|error| {
        invalid_store_data(format!(
            "read configuration audit operation payload: {error}"
        ))
    })?;
    let stored_base_revision_id = row.get::<String>(4).map_err(|error| {
        invalid_store_data(format!(
            "read configuration audit base revision id: {error}"
        ))
    })?;
    let stored_result_revision_id = row.get::<Option<String>>(5).map_err(|error| {
        invalid_store_data(format!(
            "read configuration audit result revision id: {error}"
        ))
    })?;
    let stored_target_commitment = row.get::<String>(6).map_err(|error| {
        invalid_store_data(format!(
            "read configuration audit target commitment: {error}"
        ))
    })?;
    let stored_receipt_digest = row.get::<Option<String>>(7).map_err(|error| {
        invalid_store_data(format!("read configuration audit receipt digest: {error}"))
    })?;
    let stored_safe_reason_code = row.get::<Option<String>>(8).map_err(|error| {
        invalid_store_data(format!("read configuration audit safe reason: {error}"))
    })?;
    let stored_occurred_at = row
        .get::<i64>(9)
        .map_err(|error| invalid_store_data(format!("read configuration audit time: {error}")))?;

    let payload = serde_json::from_str::<StoredConfigurationAuditPayloadV1>(&encoded_payload)
        .map_err(|error| {
            invalid_store_data(format!("decode configuration audit payload: {error}"))
        })?;
    if payload.schema_version != CONFIGURATION_AUDIT_PAYLOAD_SCHEMA_VERSION {
        return Err(invalid_store_data(
            "unsupported configuration audit payload schema version",
        ));
    }
    payload
        .event
        .validate()
        .map_err(ConfigurationStoreError::from)?;
    let event = payload.event;
    if event.event_id.as_str() != stored_event_id
        || event.actor_id.as_str() != stored_actor_id
        || event.idempotency_key.as_ref().map(|value| value.as_str())
            != stored_idempotency_key.as_deref()
        || event.base_revision_id.as_str() != stored_base_revision_id
        || event
            .result_revision_id
            .as_ref()
            .map(|value| value.as_str())
            != stored_result_revision_id.as_deref()
        || event.target_commitment.as_str() != stored_target_commitment
        || event.receipt_id.is_some() != stored_receipt_digest.is_some()
        || event.safe_reason_code.as_deref() != stored_safe_reason_code.as_deref()
        || event.occurred_at.0 != stored_occurred_at
    {
        return Err(invalid_store_data(
            "configuration audit payload does not match immutable projections",
        ));
    }
    Ok(event)
}

async fn read_audit_event_from_transaction(
    transaction: &Transaction,
    event_id: &ConfigurationAuditEventId,
) -> ConfigurationStoreResult<Option<ConfigurationAuditEvent>> {
    let mut rows = transaction
        .query(
            "SELECT event_id, actor_id, idempotency_key, operation_kind,
                    base_revision_id, result_revision_id, event_scoped_target_commitment,
                    receipt_digest, safe_reason_code, occurred_at
             FROM configuration_audit_events
             WHERE event_id = ?1",
            params![event_id.as_str()],
        )
        .await
        .map_err(unavailable_store)?;
    let Some(row) = rows.next().await.map_err(unavailable_store)? else {
        return Ok(None);
    };
    let event = decode_audit_row(&row)?;
    if rows.next().await.map_err(unavailable_store)?.is_some() {
        return Err(invalid_store_data(
            "configuration audit event id resolved to multiple rows",
        ));
    }
    Ok(Some(event))
}

async fn insert_audit_event_with_receipt_digest(
    transaction: &Transaction,
    event: &ConfigurationAuditEvent,
    receipt_digest: Option<&ManifestDigest>,
) -> ConfigurationStoreResult<()> {
    event.validate().map_err(ConfigurationStoreError::from)?;
    let encoded_payload = encode_audit_payload(event)?;
    let idempotency_key = event
        .idempotency_key
        .as_ref()
        .map(|value| value.as_str().to_owned());
    let result_revision_id = event
        .result_revision_id
        .as_ref()
        .map(|value| value.as_str().to_owned());
    let receipt_digest = receipt_digest.map(|value| value.as_str().to_owned());
    transaction
        .execute(
            "INSERT INTO configuration_audit_events (
                event_id, actor_id, idempotency_key, operation_kind,
                base_revision_id, result_revision_id, sealed_target_reference,
                event_scoped_target_commitment, receipt_digest, correlation_id,
                safe_reason_code, occurred_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8, NULL, ?9, ?10)",
            params![
                event.event_id.as_str(),
                event.actor_id.as_str(),
                idempotency_key,
                encoded_payload,
                event.base_revision_id.as_str(),
                result_revision_id,
                event.target_commitment.as_str(),
                receipt_digest,
                event.safe_reason_code.clone(),
                event.occurred_at.0,
            ],
        )
        .await
        .map_err(unavailable_store)?;
    Ok(())
}

async fn insert_audit_event(
    transaction: &Transaction,
    event: &ConfigurationAuditEvent,
    receipt: &ConfigurationMutationReceiptV1,
) -> ConfigurationStoreResult<()> {
    insert_audit_event_with_receipt_digest(transaction, event, Some(&receipt.receipt_digest)).await
}

fn terminal_plan_event_kind(event_kind: ConfigurationAuditEventKindV1) -> Option<&'static str> {
    match event_kind {
        ConfigurationAuditEventKindV1::Applied => Some("applied"),
        ConfigurationAuditEventKindV1::RollbackApplied => Some("rollback_applied"),
        _ => None,
    }
}

fn is_terminal_plan_event(event_kind: &str) -> bool {
    matches!(event_kind, "applied" | "rollback_applied")
}

async fn append_terminal_plan_event(
    transaction: &Transaction,
    plan: &ProtectedChangePlan,
    audit_event: &ConfigurationAuditEvent,
) -> ConfigurationStoreResult<()> {
    let Some(terminal_kind) = terminal_plan_event_kind(audit_event.event_kind) else {
        return Err(invalid_store_data(
            "configuration commit with a plan requires an applied terminal audit event",
        ));
    };
    let mut rows = transaction
        .query(
            "SELECT sequence, event_kind
             FROM configuration_change_plan_events
             WHERE plan_id = ?1
             ORDER BY sequence ASC",
            params![plan.plan_id.as_str()],
        )
        .await
        .map_err(unavailable_store)?;
    let mut saw_dry_run = false;
    let mut terminal_count = 0usize;
    let mut last_sequence = None;
    while let Some(row) = rows.next().await.map_err(unavailable_store)? {
        let sequence = row.get::<i64>(0).map_err(|error| {
            invalid_store_data(format!("read configuration plan event sequence: {error}"))
        })?;
        let event_kind = row.get::<String>(1).map_err(|error| {
            invalid_store_data(format!("read configuration plan event kind: {error}"))
        })?;
        if sequence < 0 || last_sequence.is_some_and(|previous| sequence <= previous) {
            return Err(invalid_store_data(
                "configuration plan events are not strictly ordered",
            ));
        }
        if sequence == 0 && event_kind == "dry_run_created" {
            saw_dry_run = true;
        }
        if is_terminal_plan_event(&event_kind) {
            terminal_count += 1;
        }
        last_sequence = Some(sequence);
    }
    drop(rows);
    if !saw_dry_run || terminal_count != 0 {
        return Err(ConfigurationStoreError::PlanStale);
    }
    let sequence = last_sequence
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| invalid_store_data("configuration plan event sequence overflow"))?;
    transaction
        .execute(
            "INSERT INTO configuration_change_plan_events (
                plan_id, sequence, event_kind, safe_reason_code, occurred_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                plan.plan_id.as_str(),
                sequence,
                terminal_kind,
                audit_event.safe_reason_code.clone(),
                audit_event.occurred_at.0,
            ],
        )
        .await
        .map_err(unavailable_store)?;
    Ok(())
}

async fn has_matching_terminal_plan_event(
    transaction: &Transaction,
    plan: &ProtectedChangePlan,
    audit_event: &ConfigurationAuditEvent,
) -> ConfigurationStoreResult<bool> {
    let Some(expected_kind) = terminal_plan_event_kind(audit_event.event_kind) else {
        return Ok(false);
    };
    let mut rows = transaction
        .query(
            "SELECT event_kind
             FROM configuration_change_plan_events
             WHERE plan_id = ?1",
            params![plan.plan_id.as_str()],
        )
        .await
        .map_err(unavailable_store)?;
    let mut terminal_count = 0usize;
    let mut matched = false;
    while let Some(row) = rows.next().await.map_err(unavailable_store)? {
        let event_kind = row.get::<String>(0).map_err(|error| {
            invalid_store_data(format!("read configuration plan terminal event: {error}"))
        })?;
        if is_terminal_plan_event(&event_kind) {
            terminal_count += 1;
            matched |= event_kind == expected_kind;
        }
    }
    Ok(terminal_count == 1 && matched)
}

fn decode_stored_mutation_receipt(row: &Row) -> ConfigurationStoreResult<StoredMutationReceipt> {
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

async fn receipt_for_idempotency_from_transaction(
    transaction: &Transaction,
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

fn authorization_policy_digest_for_commit(commit: &ConfigurationCommitV1) -> String {
    commit
        .change_plan
        .as_ref()
        .map(|plan| plan.authorization_policy_digest.as_str().to_owned())
        .unwrap_or_else(|| CONFIGURATION_AUTHORIZATION_NOT_RECORDED.to_owned())
}

async fn insert_mutation_receipt(
    transaction: &Transaction,
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
                CONFIGURATION_ACTIVATION_NOT_RECORDED,
                commit.receipt.receipt_digest.as_str(),
                commit.receipt.created_at.0,
            ],
        )
        .await
        .map_err(unavailable_store)?;
    Ok(())
}

async fn current_revision_id_from_connection(
    connection: &Connection,
) -> ConfigurationStoreResult<ConfigurationRevisionId> {
    let mut rows = connection
        .query(
            "SELECT revision_id
             FROM configuration_revisions AS candidate
             WHERE NOT EXISTS (
                 SELECT 1
                 FROM configuration_revisions AS child
                 WHERE child.parent_revision_id = candidate.revision_id
             )
             ORDER BY created_at ASC, revision_id ASC",
            (),
        )
        .await
        .map_err(unavailable_store)?;
    let Some(row) = rows.next().await.map_err(unavailable_store)? else {
        return Err(invalid_store_data(
            "configuration store has no current revision",
        ));
    };
    let revision_id: ConfigurationRevisionId = decode_id(
        row.get::<String>(0).map_err(|error| {
            invalid_store_data(format!("read current configuration revision: {error}"))
        })?,
        "current revision id",
    )?;
    if rows.next().await.map_err(unavailable_store)?.is_some() {
        return Err(invalid_store_data(
            "configuration revision history has multiple current leaves",
        ));
    }
    Ok(revision_id)
}

async fn current_revision_id_from_transaction(
    transaction: &Transaction,
) -> ConfigurationStoreResult<ConfigurationRevisionId> {
    let mut rows = transaction
        .query(
            "SELECT revision_id
             FROM configuration_revisions AS candidate
             WHERE NOT EXISTS (
                 SELECT 1
                 FROM configuration_revisions AS child
                 WHERE child.parent_revision_id = candidate.revision_id
             )
             ORDER BY created_at ASC, revision_id ASC",
            (),
        )
        .await
        .map_err(unavailable_store)?;
    let Some(row) = rows.next().await.map_err(unavailable_store)? else {
        return Err(invalid_store_data(
            "configuration store has no current revision",
        ));
    };
    let revision_id: ConfigurationRevisionId = decode_id(
        row.get::<String>(0).map_err(|error| {
            invalid_store_data(format!("read current configuration revision: {error}"))
        })?,
        "current revision id",
    )?;
    if rows.next().await.map_err(unavailable_store)?.is_some() {
        return Err(invalid_store_data(
            "configuration revision history has multiple current leaves",
        ));
    }
    Ok(revision_id)
}

fn validate_commit_bindings(commit: &ConfigurationCommitV1) -> ConfigurationStoreResult<()> {
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

async fn replay_matches_commit(
    transaction: &Transaction,
    stored: &StoredMutationReceipt,
    commit: &ConfigurationCommitV1,
) -> ConfigurationStoreResult<bool> {
    if stored.receipt != commit.receipt
        || stored.authorization_policy_digest != authorization_policy_digest_for_commit(commit)
        || stored.activation_status != CONFIGURATION_ACTIVATION_NOT_RECORDED
    {
        return Ok(false);
    }
    let expected_plan_id = commit.change_plan.as_ref().map(|plan| &plan.plan_id);
    if stored.plan_id.as_ref() != expected_plan_id {
        return Ok(false);
    }
    let stored_revision =
        read_revision_from_transaction(transaction, &commit.next_revision.revision_id).await?;
    if stored_revision.as_ref() != Some(&commit.next_revision) {
        return Ok(false);
    }
    let stored_audit_event =
        read_audit_event_from_transaction(transaction, &commit.audit_event.event_id).await?;
    if stored_audit_event.as_ref() != Some(&commit.audit_event) {
        return Ok(false);
    }
    if let Some(plan) = &commit.change_plan {
        let stored_plan = read_change_plan_from_transaction(transaction, &plan.plan_id).await?;
        if stored_plan.as_ref() != Some(plan)
            || !has_matching_terminal_plan_event(transaction, plan, &commit.audit_event).await?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn commit_configuration_transaction(
    transaction: &Transaction,
    commit: &ConfigurationCommitV1,
    fault_after_revision: bool,
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

    let current_revision_id = current_revision_id_from_transaction(transaction).await?;
    if current_revision_id != commit.expected_base_revision_id {
        return Err(ConfigurationStoreError::RevisionConflict);
    }
    if let Some(plan) = &commit.change_plan {
        let stored_plan = read_change_plan_from_transaction(transaction, &plan.plan_id).await?;
        if stored_plan.as_ref() != Some(plan) {
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
    if let Some(plan) = &commit.change_plan {
        append_terminal_plan_event(transaction, plan, &commit.audit_event).await?;
    }
    insert_audit_event(transaction, &commit.audit_event, &commit.receipt).await?;
    Ok(commit.receipt.clone())
}

impl ConfigurationSqlStore<'_> {
    pub async fn current_revision(
        &self,
    ) -> ConfigurationStoreResult<ConfigurationRevisionRecordV1> {
        let revision_id = current_revision_id_from_connection(self.connection).await?;
        read_revision_from_connection(self.connection, &revision_id)
            .await?
            .ok_or_else(|| invalid_store_data("current configuration revision disappeared"))
    }

    pub async fn read_revision(
        &self,
        revision_id: &ConfigurationRevisionId,
    ) -> ConfigurationStoreResult<Option<ConfigurationRevisionRecordV1>> {
        revision_id
            .validate()
            .map_err(ConfigurationStoreError::from)?;
        read_revision_from_connection(self.connection, revision_id).await
    }

    pub async fn save_change_plan(
        &self,
        plan: &ProtectedChangePlan,
    ) -> ConfigurationStoreResult<()> {
        plan.validate().map_err(ConfigurationStoreError::from)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(unavailable_store)?;
        let outcome = match read_change_plan_from_transaction(&transaction, &plan.plan_id).await {
            Ok(Some(existing)) if existing == *plan => Ok(()),
            Ok(Some(_)) => Err(invalid_store_data(
                "configuration change plan id conflicts with immutable prior input",
            )),
            Ok(None) => insert_change_plan(&transaction, plan).await,
            Err(error) => Err(error),
        };
        match outcome {
            Ok(()) => transaction.commit().await.map_err(unavailable_store),
            Err(error) => {
                let _ = transaction.rollback().await;
                Err(error)
            }
        }
    }

    pub async fn read_change_plan(
        &self,
        plan_id: &ChangePlanId,
    ) -> ConfigurationStoreResult<Option<ProtectedChangePlan>> {
        plan_id.validate().map_err(ConfigurationStoreError::from)?;
        read_change_plan_from_connection(self.connection, plan_id).await
    }

    pub async fn commit(
        &self,
        commit: ConfigurationCommitV1,
    ) -> ConfigurationStoreResult<ConfigurationMutationReceiptV1> {
        validate_commit_bindings(&commit)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(unavailable_store)?;
        let outcome = commit_configuration_transaction(&transaction, &commit, false).await;
        match outcome {
            Ok(receipt) => transaction
                .commit()
                .await
                .map(|_| receipt)
                .map_err(unavailable_store),
            Err(error) => {
                let _ = transaction.rollback().await;
                Err(error)
            }
        }
    }

    pub async fn audit(
        &self,
        after: Option<&ConfigurationAuditEventId>,
        limit: usize,
    ) -> ConfigurationStoreResult<Vec<ConfigurationAuditEvent>> {
        if limit == 0 {
            return Err(invalid_store_data(
                "configuration audit limit must be non-zero",
            ));
        }
        let limit = i64::try_from(limit).map_err(|_| {
            invalid_store_data("configuration audit limit exceeds SQLite integer range")
        })?;
        let cursor = if let Some(after) = after {
            after.validate().map_err(ConfigurationStoreError::from)?;
            let mut rows = self
                .connection
                .query(
                    "SELECT occurred_at FROM configuration_audit_events WHERE event_id = ?1",
                    params![after.as_str()],
                )
                .await
                .map_err(unavailable_store)?;
            let Some(row) = rows.next().await.map_err(unavailable_store)? else {
                return Err(invalid_store_data(
                    "configuration audit cursor does not exist",
                ));
            };
            let occurred_at = row.get::<i64>(0).map_err(|error| {
                invalid_store_data(format!("read configuration audit cursor time: {error}"))
            })?;
            Some((occurred_at, after.as_str().to_owned()))
        } else {
            None
        };
        let mut rows = match cursor {
            Some((occurred_at, event_id)) => self
                .connection
                .query(
                    "SELECT event_id, actor_id, idempotency_key, operation_kind,
                            base_revision_id, result_revision_id, event_scoped_target_commitment,
                            receipt_digest, safe_reason_code, occurred_at
                     FROM configuration_audit_events
                     WHERE occurred_at > ?1 OR (occurred_at = ?1 AND event_id > ?2)
                     ORDER BY occurred_at ASC, event_id ASC
                     LIMIT ?3",
                    params![occurred_at, event_id, limit],
                )
                .await
                .map_err(unavailable_store)?,
            None => self
                .connection
                .query(
                    "SELECT event_id, actor_id, idempotency_key, operation_kind,
                            base_revision_id, result_revision_id, event_scoped_target_commitment,
                            receipt_digest, safe_reason_code, occurred_at
                     FROM configuration_audit_events
                     ORDER BY occurred_at ASC, event_id ASC
                     LIMIT ?1",
                    params![limit],
                )
                .await
                .map_err(unavailable_store)?,
        };
        let mut events = Vec::new();
        while let Some(row) = rows.next().await.map_err(unavailable_store)? {
            events.push(decode_audit_row(&row)?);
        }
        Ok(events)
    }
}

impl ConfigurationRevisionStore for ConfigurationSqlStore<'_> {
    fn current_revision(
        &self,
    ) -> impl Future<Output = ConfigurationStoreResult<ConfigurationRevisionRecordV1>> + Send {
        async move { ConfigurationSqlStore::current_revision(self).await }
    }

    fn read_revision(
        &self,
        revision_id: &ConfigurationRevisionId,
    ) -> impl Future<Output = ConfigurationStoreResult<Option<ConfigurationRevisionRecordV1>>> + Send
    {
        async move { ConfigurationSqlStore::read_revision(self, revision_id).await }
    }

    fn save_change_plan(
        &self,
        plan: &ProtectedChangePlan,
    ) -> impl Future<Output = ConfigurationStoreResult<()>> + Send {
        async move { ConfigurationSqlStore::save_change_plan(self, plan).await }
    }

    fn read_change_plan(
        &self,
        plan_id: &ChangePlanId,
    ) -> impl Future<Output = ConfigurationStoreResult<Option<ProtectedChangePlan>>> + Send {
        async move { ConfigurationSqlStore::read_change_plan(self, plan_id).await }
    }

    fn commit(
        &self,
        commit: ConfigurationCommitV1,
    ) -> impl Future<Output = ConfigurationStoreResult<ConfigurationMutationReceiptV1>> + Send {
        async move { ConfigurationSqlStore::commit(self, commit).await }
    }

    fn audit(
        &self,
        after: Option<&ConfigurationAuditEventId>,
        limit: usize,
    ) -> impl Future<Output = ConfigurationStoreResult<Vec<ConfigurationAuditEvent>>> + Send {
        async move { ConfigurationSqlStore::audit(self, after, limit).await }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::config::registry::ConfigurationRegistry;
    use crate::config::resolver::resolve_configuration;
    use tracedecay_domain::configuration::{
        AuthorityRef, ConfigurationAuditEventKindV1, ConfigurationCandidateV1,
        ConfigurationLayerIdV1, ConfigurationValueV1, ProtectedChangePlan,
        RedactedConfigurationChangeV1, SOURCE_BINDINGS_SETTING_KEY, ScopeControlOperationV1,
        ScopeSourceBinding, SourceBindingId, SourceKindV1,
    };
    use tracedecay_domain::{AccessPolicyDigest, LocatorDigest, ProjectId, UtcMicros};

    async fn setup() -> (tempfile::TempDir, libsql::Connection) {
        let directory = tempfile::tempdir().unwrap();
        let database = libsql::Builder::new_local(directory.path().join("configuration.db"))
            .build()
            .await
            .unwrap();
        let connection = database.connect().unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .await
            .unwrap();
        ensure_configuration_schema(&connection).await.unwrap();
        (directory, connection)
    }

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn migration_fixture() -> (
        ConfigurationMigrationReceiptV1,
        ConfigurationResolutionV1,
        Vec<ConfigurationMigrationQuarantineEntryV1>,
    ) {
        let resolution =
            resolve_configuration(&ConfigurationRegistry::core().unwrap(), &[]).unwrap();
        let receipt = ConfigurationMigrationReceiptV1 {
            receipt_name: CONFIGURATION_CONTROL_PLANE_MIGRATION_RECEIPT_NAME,
            source_snapshot_digest: digest('a'),
            initial_revision_id: ConfigurationRevisionId::new("configuration.revision.initial")
                .unwrap(),
            initial_snapshot_id: resolution.snapshot.snapshot_id.clone(),
            created_at: UtcMicros(1),
        };
        let quarantine = vec![ConfigurationMigrationQuarantineEntryV1 {
            source_kind: LegacyConfigurationSourceKindV1::ConfigJson,
            source_key_digest: digest('b'),
            reason: super::super::migration::ConfigurationMigrationQuarantineReasonV1::UnknownKey,
            redacted_value_digest: digest('c'),
            quarantined_at: UtcMicros(1),
        }];
        (receipt, resolution, quarantine)
    }

    async fn count(connection: &Connection, table: &str) -> i64 {
        let mut rows = connection
            .query(&format!("SELECT COUNT(*) FROM {table}"), ())
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
    }

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn root_revision() -> ConfigurationRevisionRecordV1 {
        ConfigurationRevisionRecordV1 {
            revision_id: id("configuration.revision.root"),
            parent_revision_id: None,
            snapshot: ConfigurationSnapshotV1::new(BTreeMap::new(), BTreeMap::new()).unwrap(),
            actor_id: id("actor.configuration.fixture"),
            operation_kind: "migration".to_owned(),
            created_at: UtcMicros(1),
        }
    }

    fn source_binding_snapshot(revision_id: &ConfigurationRevisionId) -> ConfigurationSnapshotV1 {
        let key = SettingKey::new(SOURCE_BINDINGS_SETTING_KEY).unwrap();
        let project_id: ProjectId = id("project.authoritative.fixture");
        let binding = ScopeSourceBinding::new(
            id::<SourceBindingId>("binding.authoritative.fixture"),
            SourceKindV1::Cursor,
            LocatorDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
            AuthorityRef::Project(project_id.clone()),
        )
        .unwrap();
        let candidate = ConfigurationCandidateV1 {
            layer: ConfigurationLayerIdV1::Project { project_id },
            revision_id: revision_id.clone(),
            disposition: CandidateDispositionV1::Winning,
            safe_reason: None,
        };
        ConfigurationSnapshotV1::new(
            BTreeMap::from([(
                key.clone(),
                ConfigurationValueV1::SourceBindings(vec![binding]),
            )]),
            BTreeMap::from([(key, vec![candidate])]),
        )
        .unwrap()
    }

    fn protected_plan(base_revision_id: &ConfigurationRevisionId) -> ProtectedChangePlan {
        ProtectedChangePlan {
            plan_id: id("configuration.plan.fixture"),
            actor_id: id("actor.configuration.fixture"),
            base_revision_id: base_revision_id.clone(),
            operation_digest: digest('b'),
            resolved_scope_digest: digest('c'),
            membership_digest: Some(digest('d')),
            authorization_policy_digest: id::<AccessPolicyDigest>(&format!(
                "sha256:{}",
                "e".repeat(64)
            )),
            policy_epoch: 7,
            expires_at: UtcMicros(100),
            created_at: UtcMicros(10),
            redacted_changes: vec![RedactedConfigurationChangeV1 {
                setting_key: SettingKey::new(SOURCE_BINDINGS_SETTING_KEY).unwrap(),
                operation: ScopeControlOperationV1::SourceBind,
                before_digest: Some(digest('f')),
                after_digest: Some(digest('a')),
            }],
        }
    }

    fn protected_commit(
        root: &ConfigurationRevisionRecordV1,
    ) -> (ProtectedChangePlan, ConfigurationCommitV1) {
        let next_revision_id: ConfigurationRevisionId = id("configuration.revision.child");
        let next_revision = ConfigurationRevisionRecordV1 {
            revision_id: next_revision_id.clone(),
            parent_revision_id: Some(root.revision_id.clone()),
            snapshot: source_binding_snapshot(&next_revision_id),
            actor_id: root.actor_id.clone(),
            operation_kind: "protected_apply".to_owned(),
            created_at: UtcMicros(20),
        };
        let plan = protected_plan(&root.revision_id);
        let receipt = ConfigurationMutationReceiptV1 {
            receipt_id: id("configuration.receipt.fixture"),
            actor_id: root.actor_id.clone(),
            idempotency_key: id("configuration.idempotency.fixture"),
            base_revision_id: root.revision_id.clone(),
            result_revision_id: next_revision_id.clone(),
            operation_digest: plan.operation_digest.clone(),
            receipt_digest: digest('9'),
            created_at: UtcMicros(21),
        };
        let audit_event = ConfigurationAuditEvent {
            event_id: id("configuration.audit.fixture"),
            event_kind: ConfigurationAuditEventKindV1::Applied,
            actor_id: root.actor_id.clone(),
            idempotency_key: Some(receipt.idempotency_key.clone()),
            base_revision_id: root.revision_id.clone(),
            result_revision_id: Some(next_revision_id),
            operation_digest: plan.operation_digest.clone(),
            target_commitment: digest('8'),
            receipt_id: Some(receipt.receipt_id.clone()),
            safe_reason_code: None,
            occurred_at: UtcMicros(22),
        };
        (
            plan.clone(),
            ConfigurationCommitV1 {
                expected_base_revision_id: root.revision_id.clone(),
                next_revision,
                receipt,
                change_plan: Some(plan),
                audit_event,
            },
        )
    }

    async fn seed_revision(connection: &Connection, revision: &ConfigurationRevisionRecordV1) {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        insert_revision(&transaction, revision).await.unwrap();
        transaction.commit().await.unwrap();
    }

    #[tokio::test]
    async fn production_migration_store_commits_revision_quarantine_receipt_and_audit_atomically() {
        let (_directory, connection) = setup().await;
        let store = ConfigurationSqlStore::new(&connection);
        let (receipt, resolution, quarantine) = migration_fixture();

        store
            .commit_initial_migration(&receipt, &resolution, &quarantine)
            .await
            .unwrap();

        assert_eq!(count(&connection, "configuration_revisions").await, 1);
        assert_eq!(count(&connection, "configuration_entries").await, 5);
        assert_eq!(
            count(&connection, "configuration_migration_quarantine").await,
            1
        );
        assert_eq!(
            count(&connection, "configuration_migration_receipts").await,
            1
        );
        assert_eq!(count(&connection, "configuration_audit_events").await, 1);
        assert_eq!(
            store.current_revision().await.unwrap().snapshot,
            resolution.snapshot
        );
        assert!(matches!(
            store.audit(None, 1).await.unwrap().as_slice(),
            [ConfigurationAuditEvent {
                event_kind: ConfigurationAuditEventKindV1::Recovered,
                ..
            }]
        ));
    }

    #[tokio::test]
    async fn production_migration_store_replays_exact_receipt_idempotently() {
        let (_directory, connection) = setup().await;
        let store = ConfigurationSqlStore::new(&connection);
        let (receipt, resolution, quarantine) = migration_fixture();

        store
            .commit_initial_migration(&receipt, &resolution, &quarantine)
            .await
            .unwrap();
        store
            .commit_initial_migration(&receipt, &resolution, &quarantine)
            .await
            .unwrap();

        assert_eq!(count(&connection, "configuration_revisions").await, 1);
        assert_eq!(
            count(&connection, "configuration_migration_receipts").await,
            1
        );
        assert_eq!(count(&connection, "configuration_audit_events").await, 1);
    }

    #[tokio::test]
    async fn production_migration_store_rejects_conflicting_replay() {
        let (_directory, connection) = setup().await;
        let store = ConfigurationSqlStore::new(&connection);
        let (receipt, resolution, quarantine) = migration_fixture();
        store
            .commit_initial_migration(&receipt, &resolution, &quarantine)
            .await
            .unwrap();

        let mut conflicting = receipt;
        conflicting.initial_revision_id =
            ConfigurationRevisionId::new("configuration.revision.conflict").unwrap();
        assert!(
            store
                .commit_initial_migration(&conflicting, &resolution, &quarantine)
                .await
                .is_err()
        );
        assert_eq!(count(&connection, "configuration_revisions").await, 1);
        assert_eq!(count(&connection, "configuration_audit_events").await, 1);
    }

    #[tokio::test]
    async fn injected_crash_rolls_back_every_migration_table() {
        let (directory, connection) = setup().await;
        let (receipt, resolution, quarantine) = migration_fixture();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();

        assert!(
            commit_initial_migration_transaction(
                &transaction,
                &receipt,
                &resolution,
                &quarantine,
                true,
            )
            .await
            .is_err()
        );
        drop(transaction);
        drop(connection);

        let reopened_database =
            libsql::Builder::new_local(directory.path().join("configuration.db"))
                .build()
                .await
                .unwrap();
        let connection = reopened_database.connect().unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .await
            .unwrap();

        assert_eq!(count(&connection, "configuration_revisions").await, 0);
        assert_eq!(count(&connection, "configuration_entries").await, 0);
        assert_eq!(
            count(&connection, "configuration_migration_quarantine").await,
            0
        );
        assert_eq!(
            count(&connection, "configuration_migration_receipts").await,
            0
        );
        assert_eq!(count(&connection, "configuration_audit_events").await, 0);
    }

    #[tokio::test]
    async fn revision_store_round_trips_typed_snapshot_plan_receipt_and_audit() {
        let (_directory, connection) = setup().await;
        let root = root_revision();
        seed_revision(&connection, &root).await;
        let store = ConfigurationSqlStore::new(&connection);
        let (plan, commit) = protected_commit(&root);

        assert_eq!(store.current_revision().await.unwrap(), root);
        store.save_change_plan(&plan).await.unwrap();
        store.save_change_plan(&plan).await.unwrap();
        assert_eq!(
            store.read_change_plan(&plan.plan_id).await.unwrap(),
            Some(plan)
        );

        let receipt = store.commit(commit.clone()).await.unwrap();
        assert_eq!(receipt, commit.receipt);
        assert_eq!(
            store
                .read_revision(&commit.next_revision.revision_id)
                .await
                .unwrap(),
            Some(commit.next_revision.clone())
        );
        assert_eq!(
            store.current_revision().await.unwrap(),
            commit.next_revision.clone()
        );
        assert_eq!(store.commit(commit.clone()).await.unwrap(), receipt);

        let mut changed_input = commit.clone();
        changed_input.audit_event.safe_reason_code = Some("changed_input".to_owned());
        assert_eq!(
            store.commit(changed_input).await,
            Err(ConfigurationStoreError::IdempotencyConflict)
        );

        let mut stale = commit.clone();
        stale.change_plan = None;
        stale.next_revision.revision_id = id("configuration.revision.stale");
        stale.receipt.receipt_id = id("configuration.receipt.stale");
        stale.receipt.idempotency_key = id("configuration.idempotency.stale");
        stale.receipt.result_revision_id = stale.next_revision.revision_id.clone();
        stale.audit_event.event_id = id("configuration.audit.stale");
        stale.audit_event.idempotency_key = Some(stale.receipt.idempotency_key.clone());
        stale.audit_event.result_revision_id = Some(stale.next_revision.revision_id.clone());
        stale.audit_event.receipt_id = Some(stale.receipt.receipt_id.clone());
        assert_eq!(
            store.commit(stale).await,
            Err(ConfigurationStoreError::RevisionConflict)
        );

        assert_eq!(
            store.audit(None, 1).await.unwrap(),
            vec![commit.audit_event.clone()]
        );
        assert!(
            store
                .audit(Some(&commit.audit_event.event_id), 1)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(count(&connection, "configuration_revisions").await, 2);
        assert_eq!(
            count(&connection, "configuration_mutation_receipts").await,
            1
        );
        assert_eq!(
            count(&connection, "configuration_change_plan_events").await,
            2
        );
        assert_eq!(count(&connection, "configuration_audit_events").await, 1);
    }

    #[tokio::test]
    async fn rollback_terminal_event_is_persisted_and_visible_in_audit() {
        let (_directory, connection) = setup().await;
        let root = root_revision();
        seed_revision(&connection, &root).await;
        let store = ConfigurationSqlStore::new(&connection);
        let (plan, mut commit) = protected_commit(&root);
        commit.next_revision.operation_kind = "rollback_apply".to_owned();
        commit.audit_event.event_kind = ConfigurationAuditEventKindV1::RollbackApplied;

        store.save_change_plan(&plan).await.unwrap();
        store.commit(commit.clone()).await.unwrap();

        let mut rows = connection
            .query(
                "SELECT event_kind
                 FROM configuration_change_plan_events
                 WHERE plan_id = ?1 AND sequence = 1",
                params![plan.plan_id.as_str()],
            )
            .await
            .unwrap();
        assert_eq!(
            rows.next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap(),
            "rollback_applied"
        );
        assert_eq!(
            store.audit(None, 1).await.unwrap(),
            vec![commit.audit_event]
        );
    }

    #[tokio::test]
    async fn failed_configuration_commit_leaves_no_partial_revision_receipt_or_audit() {
        let (directory, connection) = setup().await;
        let root = root_revision();
        seed_revision(&connection, &root).await;
        let store = ConfigurationSqlStore::new(&connection);
        let (plan, commit) = protected_commit(&root);
        store.save_change_plan(&plan).await.unwrap();
        drop(store);

        validate_commit_bindings(&commit).unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        assert!(
            commit_configuration_transaction(&transaction, &commit, true)
                .await
                .is_err()
        );
        drop(transaction);
        drop(connection);

        let database = libsql::Builder::new_local(directory.path().join("configuration.db"))
            .build()
            .await
            .unwrap();
        let connection = database.connect().unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .await
            .unwrap();
        assert_eq!(count(&connection, "configuration_revisions").await, 1);
        assert_eq!(
            count(&connection, "configuration_mutation_receipts").await,
            0
        );
        assert_eq!(count(&connection, "configuration_audit_events").await, 0);
        assert_eq!(
            count(&connection, "configuration_change_plan_events").await,
            1
        );
    }
}
