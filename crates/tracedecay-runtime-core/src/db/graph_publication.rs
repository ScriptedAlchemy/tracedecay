use super::connection::Database;
use crate::errors::{Result, TraceDecayError};

impl Database {
    /// Owner-bound relational graph publication authority for this project
    /// shard. The retained runtime supplies its authorized exact-SQL handle;
    /// no database path is exposed or reopened.
    pub fn graph_publication_storage(
        &self,
    ) -> Result<tracedecay_rusqlite_runtime::repository::GraphPublicationExactSqlStorage> {
        if !matches!(
            &self.registered_binding().shard_id.scope,
            tracedecay_store::StoreShardScopeV1::Project { .. }
                | tracedecay_store::StoreShardScopeV1::ProfileMemory
        ) {
            return Err(TraceDecayError::Database {
                message: "graph publication storage requires a project or profile-memory shard"
                    .to_owned(),
                operation: "attach graph publication storage".to_owned(),
            });
        }
        let authority = self.write_authority()?;
        let handle = self
            .authorized_exact_sql_handle(authority)
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to attach graph publication storage: {error:?}"),
                operation: "attach graph publication storage".to_owned(),
            })?;
        if handle.binding() != self.registered_binding()
            || handle.verified_locator() != self.registered_verified_locator()
        {
            return Err(TraceDecayError::Database {
                message:
                    "authorized graph publication handle does not match retained shard runtime"
                        .to_owned(),
                operation: "attach graph publication storage".to_owned(),
            });
        }
        tracedecay_rusqlite_runtime::repository::GraphPublicationExactSqlStorage::from_authorized_handle_with_guard(
            handle,
            self.client_guard(),
        )
        .map_err(|error| TraceDecayError::Database {
                message: format!(
                    "failed to bind project graph publication storage: {error}"
                ),
                operation: "attach graph publication storage".to_owned(),
        })
    }
}
