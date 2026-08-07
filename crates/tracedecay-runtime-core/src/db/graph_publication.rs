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
            &self.retained_runtime().binding().shard_id.scope,
            tracedecay_store::StoreShardScopeV1::Project { .. }
        ) {
            return Err(TraceDecayError::Database {
                message: "graph publication storage is only available for project shards"
                    .to_owned(),
                operation: "attach project graph publication storage".to_owned(),
            });
        }
        let authority = self.write_authority()?;
        let handle = self
            .retained_runtime()
            .authorized_exact_sql_handle(authority)
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to attach project graph publication storage: {error:?}"),
                operation: "attach project graph publication storage".to_owned(),
            })?;
        if handle.binding() != self.retained_runtime().binding()
            || handle.verified_locator() != self.retained_runtime().locator().verified()
        {
            return Err(TraceDecayError::Database {
                message:
                    "authorized graph publication handle does not match retained project runtime"
                        .to_owned(),
                operation: "attach project graph publication storage".to_owned(),
            });
        }
        tracedecay_rusqlite_runtime::repository::GraphPublicationExactSqlStorage::from_authorized_handle(handle)
            .map_err(|error| TraceDecayError::Database {
                message: format!(
                    "failed to bind project graph publication storage: {error}"
                ),
                operation: "attach project graph publication storage".to_owned(),
            })
    }
}
