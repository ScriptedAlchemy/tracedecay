// Rust guideline compliant 2025-10-17
use crate::db::engine::params;

use super::connection::{Database, DatabaseWriteTransaction};
use crate::errors::{Result, TraceDecayError};

/// Result of one metadata point read whose payload is projected by SQLite only
/// when its encoded byte length is within the caller's bound.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BoundedMetadataValue {
    Missing,
    Value { value: String, encoded_bytes: usize },
    Oversized { encoded_bytes: usize },
}

impl Database {
    /// Reads a metadata value by key, returning `None` if not set.
    pub async fn get_metadata(&self, key: &str) -> Result<Option<String>> {
        let mut rows = self
            .read_connection()
            .query("SELECT value FROM metadata WHERE key = ?1", params![key])
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query metadata: {e}"),
                operation: "get_metadata".to_string(),
            })?;

        match rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to read metadata row: {e}"),
            operation: "get_metadata".to_string(),
        })? {
            Some(row) => {
                let value: String = row.get(0).map_err(|e| TraceDecayError::Database {
                    message: format!("failed to read metadata value: {e}"),
                    operation: "get_metadata".to_string(),
                })?;
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }

    /// Reads a metadata value without materializing an over-limit payload.
    ///
    /// SQLite measures the stored value as bytes and conditionally projects
    /// the value in the same query. The runtime therefore receives only the
    /// measured length and `NULL` when the value exceeds the caller's limit.
    pub async fn get_metadata_bounded(
        &self,
        key: &str,
        max_encoded_bytes: usize,
    ) -> Result<BoundedMetadataValue> {
        // SQLite lengths are signed 64-bit integers. A larger Rust bound is
        // equivalent to SQLite's maximum representable value, not a smaller
        // policy limit.
        let sql_limit = match i64::try_from(max_encoded_bytes) {
            Ok(limit) => limit,
            Err(_) => i64::MAX,
        };
        let mut rows = self
            .read_connection()
            .query(
                "SELECT length(CAST(value AS BLOB)), \
                 CASE WHEN length(CAST(value AS BLOB)) <= ?2 THEN value ELSE NULL END \
                 FROM metadata WHERE key = ?1",
                params![key, sql_limit],
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to query bounded metadata: {error}"),
                operation: "get_metadata_bounded".to_owned(),
            })?;

        let Some(row) = rows
            .next()
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to read bounded metadata row: {error}"),
                operation: "get_metadata_bounded".to_owned(),
            })?
        else {
            return Ok(BoundedMetadataValue::Missing);
        };
        let encoded_bytes = row
            .get::<i64>(0)
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to read bounded metadata length: {error}"),
                operation: "get_metadata_bounded".to_owned(),
            })?;
        let encoded_bytes =
            usize::try_from(encoded_bytes).map_err(|_| TraceDecayError::Database {
                message: "bounded metadata length was outside the platform range".to_owned(),
                operation: "get_metadata_bounded".to_owned(),
            })?;
        let value = row
            .get::<Option<String>>(1)
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to read bounded metadata value: {error}"),
                operation: "get_metadata_bounded".to_owned(),
            })?;

        match value {
            Some(value) if encoded_bytes <= max_encoded_bytes => Ok(BoundedMetadataValue::Value {
                value,
                encoded_bytes,
            }),
            None if encoded_bytes > max_encoded_bytes => {
                Ok(BoundedMetadataValue::Oversized { encoded_bytes })
            }
            Some(_) | None => Err(TraceDecayError::Database {
                message: "bounded metadata projection disagreed with its measured length"
                    .to_owned(),
                operation: "get_metadata_bounded".to_owned(),
            }),
        }
    }

    /// Reads a metadata value through an already-open canonical write
    /// transaction. Compound durable operations use this to keep their
    /// compare-and-set and metadata update on one writer lane.
    pub async fn get_metadata_unguarded(
        &self,
        transaction: &DatabaseWriteTransaction<'_>,
        key: &str,
    ) -> Result<Option<String>> {
        let mut rows = transaction
            .query_engine("SELECT value FROM metadata WHERE key = ?1", params![key])
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query transactional metadata: {e}"),
                operation: "get_metadata_unguarded".to_string(),
            })?;

        match rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to read transactional metadata row: {e}"),
            operation: "get_metadata_unguarded".to_string(),
        })? {
            Some(row) => {
                let value: String = row.get(0).map_err(|e| TraceDecayError::Database {
                    message: format!("failed to read transactional metadata value: {e}"),
                    operation: "get_metadata_unguarded".to_string(),
                })?;
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }

    /// Sets a metadata value, creating or replacing the entry.
    pub async fn set_metadata(&self, key: &str, value: &str) -> Result<()> {
        let transaction = self.begin_write_transaction("set_metadata").await?;
        self.set_metadata_unguarded(&transaction, key, value)
            .await?;
        transaction.commit().await
    }

    pub async fn set_metadata_unguarded(
        &self,
        transaction: &DatabaseWriteTransaction<'_>,
        key: &str,
        value: &str,
    ) -> Result<()> {
        transaction
            .execute_engine(
                "INSERT OR REPLACE INTO metadata (key, value) VALUES (?1, ?2)",
                params![key, value],
            )
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to set metadata: {e}"),
                operation: "set_metadata".to_string(),
            })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{BoundedMetadataValue, Database};
    use crate::db::{DatabaseAuthority, TestDatabaseRuntimeMode};

    async fn database(label: &str) -> (tempfile::TempDir, Database) {
        let directory = tempfile::tempdir().expect("create bounded metadata fixture directory");
        let path = directory.path().join(format!("{label}.db"));
        let authority = DatabaseAuthority::acquire_test(&path, label)
            .expect("acquire bounded metadata fixture authority");
        let (database, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .expect("publish bounded metadata fixture runtime");
        (directory, database)
    }

    #[tokio::test]
    async fn bounded_metadata_distinguishes_missing_exact_and_oversized_values() {
        let (_directory, database) = database("bounded-metadata-exact").await;
        database
            .set_metadata("bounded", "three")
            .await
            .expect("seed bounded metadata text");

        assert_eq!(
            database
                .get_metadata_bounded("missing", 5)
                .await
                .expect("read missing bounded metadata"),
            BoundedMetadataValue::Missing
        );
        assert_eq!(
            database
                .get_metadata_bounded("bounded", 5)
                .await
                .expect("read exact-limit metadata"),
            BoundedMetadataValue::Value {
                value: "three".to_owned(),
                encoded_bytes: 5,
            }
        );
        assert_eq!(
            database
                .get_metadata_bounded("bounded", 4)
                .await
                .expect("measure oversized metadata"),
            BoundedMetadataValue::Oversized { encoded_bytes: 5 }
        );
    }

    #[tokio::test]
    async fn bounded_metadata_measures_utf8_bytes_and_never_projects_large_values() {
        let (_directory, database) = database("bounded-metadata-bytes").await;
        database
            .set_metadata("utf8", "é")
            .await
            .expect("seed multibyte metadata text");
        let large = "x".repeat(8 * 1024 * 1024);
        database
            .set_metadata("large", &large)
            .await
            .expect("seed large metadata text");

        assert_eq!(
            database
                .get_metadata_bounded("utf8", 1)
                .await
                .expect("measure UTF-8 metadata bytes"),
            BoundedMetadataValue::Oversized { encoded_bytes: 2 }
        );
        assert_eq!(
            database
                .get_metadata_bounded("large", 1)
                .await
                .expect("measure large metadata without projecting it"),
            BoundedMetadataValue::Oversized {
                encoded_bytes: large.len(),
            }
        );
    }

    #[tokio::test]
    async fn bounded_metadata_rejects_in_limit_non_text_but_skips_oversized_non_text() {
        let (_directory, database) = database("bounded-metadata-non-text").await;
        database
            .execute_write(
                "seed invalid metadata bytes",
                "INSERT OR REPLACE INTO metadata (key, value) VALUES (?1, ?2)",
                crate::db::engine::params!["invalid", vec![0xff_u8, 0xfe_u8]],
            )
            .await
            .expect("seed non-text metadata bytes");

        assert!(database.get_metadata_bounded("invalid", 2).await.is_err());
        assert_eq!(
            database
                .get_metadata_bounded("invalid", 1)
                .await
                .expect("skip oversized non-text metadata"),
            BoundedMetadataValue::Oversized { encoded_bytes: 2 }
        );
    }
}
