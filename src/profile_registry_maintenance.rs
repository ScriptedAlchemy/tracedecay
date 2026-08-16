//! Exact-final profile registry maintenance.
//!
//! This composition boundary opens the daemon-owned final registry for
//! explicit offline maintenance. Orphan inspection, relinking, and retirement
//! semantics live with the registry store in `tracedecay-global-db`; this
//! wrapper owns only profile/runtime composition.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::global_db::{
    RegisteredGlobalDb, RegisteredGlobalDbLeaseV1, registry_maintenance::RegistryGcReport,
    registry_maintenance::RegistryOrphanRelinkApplyReport,
    registry_maintenance::RegistryOrphanRelinkReport,
};

pub struct ProfileRegistryMaintenanceRuntime {
    profile_database: RegisteredGlobalDbLeaseV1,
}

impl ProfileRegistryMaintenanceRuntime {
    /// Opens an existing exact-final profile registry without creating one.
    pub async fn try_open_existing(profile_root: &Path) -> crate::errors::Result<Option<Self>> {
        if !profile_root
            .try_exists()
            .map_err(|error| crate::errors::TraceDecayError::Database {
                operation: "inspect existing profile root".to_string(),
                message: error.to_string(),
            })?
        {
            return Ok(None);
        }
        let profile_root = profile_root.canonicalize().map_err(|error| {
            crate::errors::TraceDecayError::Database {
                operation: "resolve existing profile registry".to_string(),
                message: error.to_string(),
            }
        })?;
        if !profile_root
            .join("global.db")
            .try_exists()
            .map_err(|error| crate::errors::TraceDecayError::Database {
                operation: "inspect existing profile registry".to_string(),
                message: error.to_string(),
            })?
        {
            return Ok(None);
        }
        Self::open(&profile_root).await.map(Some)
    }

    pub async fn open(profile_root: &Path) -> crate::errors::Result<Self> {
        let identity = crate::daemon::profile_identity::load_existing(profile_root)?;
        let registry =
            crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
                identity,
            )
            .await?;
        let profile_database = registry.profile_database().await?;
        Ok(Self { profile_database })
    }

    pub async fn registered_project_paths(&self) -> crate::errors::Result<Vec<PathBuf>> {
        self.profile_database
            .try_list_code_project_paths(usize::MAX)
            .await
    }

    pub async fn classify_project_storage(
        &self,
        project_root: &Path,
        profile_root: &Path,
    ) -> crate::errors::Result<crate::storage::ProjectStorageLocation> {
        let location = crate::storage::classify_project_storage(project_root);
        if location.status != crate::storage::ProjectStorageStatus::Stale {
            return Ok(location);
        }
        let Some(store) = self
            .profile_database
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

    pub async fn delete_project_paths(
        &self,
        project_paths: &[PathBuf],
    ) -> crate::errors::Result<usize> {
        crate::global_db::registry_maintenance::retire_registry_project_paths(
            self.profile_database.as_ref(),
            project_paths,
        )
        .await
    }

    pub async fn apply_orphan_relink(
        &self,
        report: &RegistryOrphanRelinkReport,
    ) -> std::result::Result<RegistryOrphanRelinkApplyReport, Vec<String>> {
        crate::global_db::registry_maintenance::apply_registry_orphan_relink_report(
            self.profile_database.as_ref(),
            report,
        )
        .await
    }

    pub async fn registry_gc(
        &self,
        profile_root: &Path,
        prefix: Option<String>,
        apply: bool,
    ) -> crate::errors::Result<RegistryGcReport> {
        if apply {
            crate::global_db::registry_maintenance::apply_registry_gc(
                self.profile_database.as_ref(),
                profile_root,
                prefix,
            )
            .await
        } else {
            crate::global_db::registry_maintenance::registry_gc_report(
                self.profile_database.as_ref(),
                profile_root,
                prefix,
            )
            .await
        }
    }

    pub fn database(&self) -> &RegisteredGlobalDb {
        self.profile_database.as_ref()
    }
}
