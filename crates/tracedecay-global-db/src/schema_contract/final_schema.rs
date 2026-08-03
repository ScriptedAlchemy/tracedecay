use tracedecay_runtime_core::db::engine::Connection;
use tracedecay_runtime_core::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::store_runtime::schema::{StoreSchemaContractV2, StoreSchemaKindV2};

const REGISTERED_CATALOG_FINGERPRINT_V2: &str =
    "e70b2b5c62401ca1b6e6962fd4c91bd5e20025c55a7b8ecf8b1b0f725f4a4a40";

pub fn final_registered_schema_contract() -> Result<StoreSchemaContractV2> {
    StoreSchemaContractV2::new(
        StoreSchemaKindV2::Registered,
        REGISTERED_CATALOG_FINGERPRINT_V2,
    )
    .map_err(|_| TraceDecayError::Database {
        operation: "load final registered schema contract".to_owned(),
        message: "compiled registered schema fingerprint is invalid".to_owned(),
    })
}

pub async fn install_final_registered_schema(connection: &Connection) -> Result<()> {
    super::super::ensure_registered_schema(connection).await?;
    let transaction = connection.authorized_long_lease_transaction().await?;
    transaction
        .execute_schema_batch_step(
            "PRAGMA application_id = 1413763634;
             PRAGMA user_version = 1;",
        )
        .await?;
    transaction.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;
    use tracedecay_runtime_core::db::engine::TestConnection;
    use tracedecay_runtime_core::store_runtime::schema::{
        observe_store_schema, validate_store_schema,
    };

    use super::*;

    #[tokio::test]
    async fn final_registered_catalog_fingerprint_is_pinned() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("global.db");
        let connection = TestConnection::open(&path);
        install_final_registered_schema(&connection).await.unwrap();
        let observed = observe_store_schema(&connection).await.unwrap();
        assert_eq!(
            observed.catalog_fingerprint.as_deref(),
            Some(REGISTERED_CATALOG_FINGERPRINT_V2)
        );
        validate_store_schema(
            &connection,
            &path,
            &final_registered_schema_contract().unwrap(),
        )
        .await
        .unwrap();
    }
}
