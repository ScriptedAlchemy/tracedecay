use std::num::NonZeroU64;
use std::sync::Arc;

use super::connection::Database;
use crate::resident_memory::{
    ProcessResidentMemoryV1, ResidentMemoryComponentIdV1, ResidentMemoryKeyV1,
};
pub use tracedecay_store::runtime::CodeIndexStoredBytes;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_store::runtime::{
    CodeIndexAllocationAdmission, CodeIndexStorageError, CodeIndexStorageResult,
    StoreShardScopeV1,
};

#[cfg(test)]
mod tests;

struct CodeIndexResidentAdmission {
    resident_memory: Arc<ProcessResidentMemoryV1>,
    key: Option<ResidentMemoryKeyV1>,
}

/// Materialize encoded bytes only after their capacity is charged. Non-SQL
/// readers and in-memory benchmark fixtures use the same resident authority.
pub fn reserve_encoded_bytes(
    memory: &Arc<ProcessResidentMemoryV1>,
    size: usize,
    fill: impl FnOnce(&mut [u8]) -> Result<()>,
) -> Result<CodeIndexStoredBytes> {
    let requested = NonZeroU64::new(u64::try_from(size).map_err(|error| {
        TraceDecayError::Database {
            operation: "admit encoded code-index bytes".to_owned(),
            message: error.to_string(),
        }
    })?.max(1)).ok_or_else(|| TraceDecayError::Database {
        operation: "admit encoded code-index bytes".to_owned(),
        message: "encoded allocation has no positive size".to_owned(),
    })?;
    let component = ResidentMemoryComponentIdV1::new("code-index-encoded-read")
        .map_err(|error| TraceDecayError::Database {
            operation: "admit encoded code-index bytes".to_owned(),
            message: error.to_string(),
        })?;
    let reservation = memory.reserve_process_shared(component, requested)
        .map_err(|error| TraceDecayError::Database {
            operation: "admit encoded code-index bytes".to_owned(),
            message: error.to_string(),
        })?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(size).map_err(|error| TraceDecayError::Database {
        operation: "allocate encoded code-index bytes".to_owned(),
        message: error.to_string(),
    })?;
    bytes.resize(size, 0);
    fill(&mut bytes)?;
    Ok(CodeIndexStoredBytes::from_reserved(bytes, Box::new(reservation)))
}

impl CodeIndexAllocationAdmission for CodeIndexResidentAdmission {
    fn reserve(&self, bytes: u64) -> CodeIndexStorageResult<Box<dyn Send + Sync>> {
        let requested = NonZeroU64::new(bytes).ok_or_else(|| {
            CodeIndexStorageError::InvalidRequest("empty allocation reservation".to_owned())
        })?;
        match &self.key {
            Some(key) => self.resident_memory.reserve(key.clone(), requested)
                .map(|reservation| Box::new(reservation) as Box<dyn Send + Sync>),
            None => {
                let component = ResidentMemoryComponentIdV1::new("code-index-storage")
                    .map_err(|error| CodeIndexStorageError::InvalidRequest(error.to_string()))?;
                self.resident_memory.reserve_process_shared(component, requested)
                    .map(|reservation| Box::new(reservation) as Box<dyn Send + Sync>)
            }
        }.map_err(|error| CodeIndexStorageError::AllocationRefused(error.to_string()))
    }
}

impl Database {
    /// Bind encoded index storage to this registered project's physical SQL
    /// authority and the caller's existing exact-generation resident ledger.
    pub fn code_index_storage(
        &self,
        resident_memory: Arc<ProcessResidentMemoryV1>,
        key: ResidentMemoryKeyV1,
    ) -> Result<tracedecay_rusqlite_runtime::repository::CodeIndexSqliteStorage> {
        if !matches!(
            &self.registered_binding().shard_id.scope,
            StoreShardScopeV1::Project { project_id } if project_id == &key.project_id
        ) {
            return Err(TraceDecayError::Database {
                message: "code-index storage requires the exact registered project database".to_owned(),
                operation: "attach code-index storage".to_owned(),
            });
        }
        Ok(tracedecay_rusqlite_runtime::repository::CodeIndexSqliteStorage::from_retained_exact_sql(
            self.retained_exact_sql_for_purpose("attach code-index storage")?,
            Arc::new(CodeIndexResidentAdmission { resident_memory, key: Some(key) }),
            self.write_authority()?.token().to_owned(),
        ))
    }

    /// Project-owned reads whose head must be decoded before a generation is
    /// known charge the same process ledger, without fabricating a generation.
    pub fn project_code_index_storage(
        &self,
        resident_memory: Arc<ProcessResidentMemoryV1>,
        project_id: &tracedecay_domain::ProjectId,
    ) -> Result<tracedecay_rusqlite_runtime::repository::CodeIndexSqliteStorage> {
        if !matches!(
            &self.registered_binding().shard_id.scope,
            StoreShardScopeV1::Project { project_id: owner } if owner == project_id
        ) {
            return Err(TraceDecayError::Database {
                message: "code-index storage requires the exact registered project database".to_owned(),
                operation: "attach project code-index storage".to_owned(),
            });
        }
        Ok(tracedecay_rusqlite_runtime::repository::CodeIndexSqliteStorage::from_retained_exact_sql(
            self.retained_exact_sql_for_purpose("attach project code-index storage")?,
            Arc::new(CodeIndexResidentAdmission { resident_memory, key: None }),
            self.write_authority()?.token().to_owned(),
        ))
    }

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
    pub async fn workflow_storage(
        &self,
    ) -> Result<tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority> {
        tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority::from_retained_exact_sql(
            self.retained_exact_sql_for_purpose("attach workflow storage")?,
        )
        .await
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
    pub async fn handoff_open_storage(
        &self,
    ) -> Result<tracedecay_rusqlite_runtime::handoff::HandoffOpenSqliteAuthority> {
        tracedecay_rusqlite_runtime::handoff::HandoffOpenSqliteAuthority::from_retained_exact_sql(
            self.retained_exact_sql_for_purpose("attach handoff-open storage")?,
        )
        .await
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

    /// Owner-bound initial Remote Brain storage that seeds its node identity
    /// serving `member_brain`.
    pub fn provision_remote_storage(
        &self,
        keyring: std::sync::Arc<dyn tracedecay_rusqlite_runtime::remote::RemoteSpoolKeyringV1>,
        member_brain: &tracedecay_domain::BrainId,
    ) -> Result<tracedecay_rusqlite_runtime::remote::RemoteSqliteStorageV1> {
        tracedecay_rusqlite_runtime::remote::RemoteSqliteStorageV1::provision_retained_exact_sql(
            self.retained_exact_sql_for_purpose("provision Remote Brain storage")?,
            keyring,
            member_brain,
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
