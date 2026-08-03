use tracedecay_runtime_core::db::engine::Connection;
use tracedecay_runtime_core::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::store_runtime::schema::{StoreSchemaContractV2, StoreSchemaKindV2};

const REGISTERED_CATALOG_FINGERPRINT_V2: &str =
    "63875ae92b818b73c7599f0663cbdf7fdca1cb029a6687ad26b2a1fcc25b94de";

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
    let final_fragments = format!(
        "{}
         {}
         PRAGMA application_id = 1413763634;
         PRAGMA user_version = 1;",
        crate::session_content::SESSION_CONTENT_SCHEMA_DDL,
        tracedecay_rusqlite_runtime::remote::REMOTE_OBSERVATION_EVENTS_SCHEMA,
    );
    transaction
        .execute_schema_batch_step(&final_fragments)
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
