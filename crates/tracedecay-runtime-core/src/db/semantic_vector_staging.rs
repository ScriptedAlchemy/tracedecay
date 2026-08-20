use super::connection::Database;
use crate::errors::{Result, TraceDecayError};

impl Database {
    /// Owner-bound metadata-only semantic-vector staging authority.
    pub fn semantic_vector_publication_authority(
        &self,
    ) -> Result<tracedecay_rusqlite_runtime::repository::SemanticVectorStagingExactSqlStorage> {
        if !matches!(
            &self.registered_binding().shard_id.scope,
            tracedecay_store::StoreShardScopeV1::Project { .. }
        ) {
            return Err(TraceDecayError::Database {
                message: "semantic vector staging is only available for project shards".to_owned(),
                operation: "attach semantic vector staging storage".to_owned(),
            });
        }
        let authority = self.write_authority()?;
        let handle = self
            .authorized_exact_sql_handle(authority)
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to attach semantic vector staging storage: {error:?}"),
                operation: "attach semantic vector staging storage".to_owned(),
            })?;
        if handle.binding() != self.registered_binding()
            || handle.verified_locator() != self.registered_verified_locator()
        {
            return Err(TraceDecayError::Database {
                message: "semantic vector staging handle does not match retained project runtime"
                    .to_owned(),
                operation: "attach semantic vector staging storage".to_owned(),
            });
        }
        tracedecay_rusqlite_runtime::repository::SemanticVectorStagingExactSqlStorage::from_authorized_handle_with_guard(
            handle,
            self.client_guard(),
        )
        .map_err(|error| TraceDecayError::Database {
                message: format!("failed to attach semantic vector staging storage: {error}"),
                operation: "attach semantic vector staging storage".to_owned(),
        })
    }
}
