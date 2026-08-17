use super::connection::Database;
use crate::errors::{Result, TraceDecayError};

impl Database {
    /// Retains the issuing database client with one exact-SQL purpose adapter.
    ///
    /// This is the only database-core path from write authority to retained
    /// repository SQL. The exact handle cannot escape the resulting adapter.
    fn retained_exact_sql_for_purpose(
        &self,
        operation: &str,
    ) -> Result<tracedecay_rusqlite_runtime::repository::RetainedExactSqlCapability> {
        let authority = self.write_authority()?;
        let handle = self
            .authorized_exact_sql_handle(authority)
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to authorize retained exact SQL: {error:?}"),
                operation: operation.to_owned(),
            })?;
        if handle.binding() != self.registered_binding()
            || handle.verified_locator() != self.registered_verified_locator()
        {
            return Err(TraceDecayError::Database {
                message: "authorized exact-SQL handle does not match the retained database runtime"
                    .to_owned(),
                operation: operation.to_owned(),
            });
        }
        Ok(
            tracedecay_rusqlite_runtime::repository::RetainedExactSqlCapability::from_authorized_handle_with_guard(
                handle,
                self.client_guard(),
            ),
        )
    }

    /// Owner-bound Work persistence over this database's canonical runtime.
    pub fn work_storage(&self) -> Result<tracedecay_rusqlite_runtime::work::WorkSqliteStorage> {
        Ok(
            tracedecay_rusqlite_runtime::work::WorkSqliteStorage::from_retained_exact_sql(
                self.retained_exact_sql_for_purpose("attach Work storage")?,
            ),
        )
    }

    /// Owner-bound workflow source and effect journal authority.
    pub fn workflow_storage(
        &self,
    ) -> Result<tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority> {
        tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority::from_retained_exact_sql(
            self.retained_exact_sql_for_purpose("attach workflow storage")?,
        )
        .map_err(|error| TraceDecayError::Database {
            message: format!("failed to validate workflow storage schema: {error:?}"),
            operation: "attach workflow storage".to_owned(),
        })
    }

    /// Owner-bound authorized scope-set persistence.
    pub fn authorized_scope_set_storage(
        &self,
    ) -> Result<tracedecay_rusqlite_runtime::repository::AuthorizedScopeSetSqliteStorage> {
        Ok(
            tracedecay_rusqlite_runtime::repository::AuthorizedScopeSetSqliteStorage::from_retained_exact_sql(
                self.retained_exact_sql_for_purpose("attach authorized scope-set storage")?,
            ),
        )
    }

    /// Owner-bound single-use handoff-open authority.
    pub fn handoff_open_storage(
        &self,
    ) -> Result<tracedecay_rusqlite_runtime::handoff::HandoffOpenSqliteAuthority> {
        tracedecay_rusqlite_runtime::handoff::HandoffOpenSqliteAuthority::from_retained_exact_sql(
            self.retained_exact_sql_for_purpose("attach handoff-open storage")?,
        )
        .map_err(|error| TraceDecayError::Database {
            message: format!("failed to validate handoff-open storage schema: {error:?}"),
            operation: "attach handoff-open storage".to_owned(),
        })
    }

    /// Owner-bound Remote Brain storage over this database's canonical runtime.
    pub fn remote_storage(
        &self,
        keyring: std::sync::Arc<dyn tracedecay_rusqlite_runtime::remote::RemoteSpoolKeyringV1>,
    ) -> Result<tracedecay_rusqlite_runtime::remote::RemoteSqliteStorageV1> {
        tracedecay_rusqlite_runtime::remote::RemoteSqliteStorageV1::from_retained_exact_sql(
            self.retained_exact_sql_for_purpose("attach Remote Brain storage")?,
            keyring,
        )
        .map_err(|error| TraceDecayError::Database {
            message: format!("failed to validate Remote Brain storage schema: {error:?}"),
            operation: "attach Remote Brain storage".to_owned(),
        })
    }

    /// Owner-bound initial Remote Brain storage that seeds its node identity.
    pub fn provision_remote_storage(
        &self,
        keyring: std::sync::Arc<dyn tracedecay_rusqlite_runtime::remote::RemoteSpoolKeyringV1>,
    ) -> Result<tracedecay_rusqlite_runtime::remote::RemoteSqliteStorageV1> {
        tracedecay_rusqlite_runtime::remote::RemoteSqliteStorageV1::provision_retained_exact_sql(
            self.retained_exact_sql_for_purpose("provision Remote Brain storage")?,
            keyring,
        )
        .map_err(|error| TraceDecayError::Database {
            message: format!("failed to provision Remote Brain storage: {error:?}"),
            operation: "provision Remote Brain storage".to_owned(),
        })
    }

    /// Owner-bound Remote Brain recovery operation authority.
    pub fn remote_recovery_authority(
        &self,
        effects: std::sync::Arc<
            dyn tracedecay_rusqlite_runtime::remote::RemoteRecoveryPhysicalEffectsV1,
        >,
    ) -> Result<tracedecay_rusqlite_runtime::remote::RemoteRecoverySqliteAuthorityV1> {
        tracedecay_rusqlite_runtime::remote::RemoteRecoverySqliteAuthorityV1::from_retained_exact_sql(
            self.retained_exact_sql_for_purpose("attach Remote Brain recovery authority")?,
            effects,
        )
        .map_err(|error| TraceDecayError::Database {
            message: format!("failed to validate Remote Brain recovery schema: {error:?}"),
            operation: "attach Remote Brain recovery authority".to_owned(),
        })
    }
}
