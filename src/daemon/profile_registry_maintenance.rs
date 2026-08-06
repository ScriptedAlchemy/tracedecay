use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::storage::ProjectStorageLocation;

pub struct ProfileRegistryMaintenance {
    database: Arc<RegisteredGlobalDb>,
}

impl ProfileRegistryMaintenance {
    pub async fn try_open_existing(profile_root: &Path) -> Result<Option<Self>> {
        if !profile_root
            .try_exists()
            .map_err(|error| TraceDecayError::Database {
                operation: "inspect existing profile root".to_owned(),
                message: error.to_string(),
            })?
        {
            return Ok(None);
        }
        let profile_root =
            profile_root
                .canonicalize()
                .map_err(|error| TraceDecayError::Database {
                    operation: "resolve existing profile registry".to_owned(),
                    message: error.to_string(),
                })?;
        if !profile_root
            .join("global.db")
            .try_exists()
            .map_err(|error| TraceDecayError::Database {
                operation: "inspect existing profile registry".to_owned(),
                message: error.to_string(),
            })?
        {
            return Ok(None);
        }
        let identity = super::profile_identity::load_or_create(&profile_root)?;
        let registry =
            super::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(identity)
                .await?;
        Ok(Some(Self {
            database: registry.profile_database().await?,
        }))
    }

    pub async fn registered_project_paths(&self) -> Result<Vec<PathBuf>> {
        self.database.try_list_code_project_paths(usize::MAX).await
    }

    pub async fn classify_project_storage(
        &self,
        project_root: &Path,
        profile_root: &Path,
    ) -> Result<ProjectStorageLocation> {
        let location = crate::storage::classify_project_storage(project_root);
        if location.status != crate::storage::ProjectStorageStatus::Stale {
            return Ok(location);
        }
        let Some(store) = self
            .database
            .try_resolve_project_store_record_by_alias(project_root)
            .await?
        else {
            return Ok(location);
        };
        Ok(
            crate::storage::classify_registry_storage(project_root, profile_root, &store)
                .unwrap_or(location),
        )
    }

    pub fn canonical_project_key(project_root: &Path) -> String {
        RegisteredGlobalDb::canonical_project_key(project_root)
    }

    pub async fn delete_project_paths(&self, project_paths: &[PathBuf]) -> Result<usize> {
        let transaction = self.database.begin_write_transaction().await?;
        const CHUNK: usize = 256;
        let mut deleted = 0_usize;
        for chunk in project_paths.chunks(CHUNK) {
            let sql = format!(
                "DELETE FROM projects WHERE path IN ({})",
                vec!["?"; chunk.len()].join(",")
            );
            let values = chunk
                .iter()
                .map(|path| {
                    crate::db::engine::Value::Text(RegisteredGlobalDb::project_path_alias_key(path))
                })
                .collect::<Vec<_>>();
            deleted = deleted.saturating_add(
                crate::db::engine::Executor::execute(&transaction, &sql, values).await? as usize,
            );
        }
        transaction.commit().await?;
        Ok(deleted)
    }

    pub async fn registry_gc(
        &self,
        prefix: Option<String>,
        apply: bool,
    ) -> Result<crate::global_db::RegistryGcReport> {
        if apply {
            crate::global_db::apply_registry_gc(self.database.as_ref(), prefix).await
        } else {
            crate::global_db::registry_gc_report(self.database.as_ref(), prefix).await
        }
    }
}
