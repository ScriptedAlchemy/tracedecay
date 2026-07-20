//! SQLite primitives for configuration migration receipts and quarantine.
//!
//! The shared global-db lifecycle wires this adapter into its transaction
//! boundary. This module intentionally does not register itself or mutate the
//! legacy configuration file.

use libsql::{Connection, params};
use thiserror::Error;
use tracedecay_domain::ManifestDigest;
use tracedecay_domain::configuration::ConfigurationSnapshotId;

use super::migration::{
    ConfigurationMigrationQuarantineEntryV1, ConfigurationMigrationReceiptV1,
    LegacyConfigurationSourceKindV1,
};
use super::schema::{ConfigurationSchemaError, ensure_configuration_schema};

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
                "SELECT initial_snapshot_id, created_at
                 FROM configuration_migration_receipts
                 WHERE receipt_name = ?1 AND source_snapshot_digest = ?2",
                params![receipt_name, source_snapshot_digest.as_str()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let initial_snapshot_id = ConfigurationSnapshotId::new(row.get::<String>(0)?)
            .map_err(|error| ConfigurationStorageError::Encoding(error.to_string()))?;
        let created_at = tracedecay_domain::UtcMicros(row.get::<i64>(1)?);
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
            initial_snapshot_id,
            created_at,
        }))
    }

    pub async fn record_migration_receipt(
        &self,
        receipt: &ConfigurationMigrationReceiptV1,
    ) -> Result<(), ConfigurationStorageError> {
        self.connection
            .execute(
                "INSERT OR IGNORE INTO configuration_migration_receipts (
                    receipt_name, source_snapshot_digest, initial_snapshot_id, created_at
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![
                    receipt.receipt_name,
                    receipt.source_snapshot_digest.as_str(),
                    receipt.initial_snapshot_id.as_str(),
                    receipt.created_at.0,
                ],
            )
            .await?;
        Ok(())
    }

    pub async fn quarantine(
        &self,
        entry: &ConfigurationMigrationQuarantineEntryV1,
    ) -> Result<(), ConfigurationStorageError> {
        self.connection
            .execute(
                "INSERT OR IGNORE INTO configuration_migration_quarantine (
                    source_kind, source_key_digest, reason_code, redacted_value_digest, quarantined_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    source_kind_name(entry.source_kind),
                    entry.source_key_digest.as_str(),
                    entry.reason.as_str(),
                    entry.redacted_value_digest.as_str(),
                    entry.quarantined_at.0,
                ],
            )
            .await?;
        Ok(())
    }
}

fn source_kind_name(source_kind: LegacyConfigurationSourceKindV1) -> &'static str {
    source_kind.as_str()
}
