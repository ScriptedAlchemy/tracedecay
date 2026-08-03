//! Graph metadata operations over a registry-owned physical runtime.

use tracedecay_store::StoreShardScopeV1;

use super::registry::{StoreRuntimeHandle, StoreRuntimeRegistryFailure};
use crate::db::engine::{Connection, TransactionBehavior};

pub(crate) struct GraphRuntimeMetadata<'runtime> {
    _runtime: &'runtime StoreRuntimeHandle,
    connection: Connection,
}

impl StoreRuntimeHandle {
    pub(crate) fn graph_metadata(
        &self,
    ) -> Result<GraphRuntimeMetadata<'_>, StoreRuntimeRegistryFailure> {
        if !matches!(
            &self.binding().shard_id.scope,
            StoreShardScopeV1::Code { .. }
        ) {
            return Err(StoreRuntimeRegistryFailure::UnsupportedShardScope);
        }

        let authority = self.database_authority("authorize graph metadata")?;
        let handle = self.authorized_exact_sql_handle(authority)?;

        Ok(GraphRuntimeMetadata {
            _runtime: self,
            connection: Connection::attach(handle),
        })
    }
}

impl GraphRuntimeMetadata<'_> {
    pub async fn get(&self, key: &str) -> Result<Option<String>, StoreRuntimeRegistryFailure> {
        self._runtime
            .validate_registered_read("read registered graph metadata")?;
        let mut rows = self
            .connection
            .query("SELECT value FROM metadata WHERE key = ?1", [key])
            .await
            .map_err(graph_metadata_failure)?;
        let Some(row) = rows.next().await.map_err(graph_metadata_failure)? else {
            return Ok(None);
        };
        row.get(0).map(Some).map_err(graph_metadata_failure)
    }

    pub async fn set(&self, key: &str, value: &str) -> Result<(), StoreRuntimeRegistryFailure> {
        self._runtime
            .validate_registered_read("write registered graph metadata")?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(graph_metadata_failure)?;
        transaction
            .execute(
                "INSERT INTO metadata (key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [key, value],
            )
            .await
            .map_err(graph_metadata_failure)?;
        transaction.commit().await.map_err(graph_metadata_failure)
    }
}

fn graph_metadata_failure(error: crate::db::engine::Error) -> StoreRuntimeRegistryFailure {
    StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
        operation: "access graph metadata",
        message: error.to_string(),
    }
}
