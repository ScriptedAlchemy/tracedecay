use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracedecay_runtime_core::path_safety::same_canonical_path;
use tracedecay_store::{ProjectId, StoreShardIdV1};

use super::{
    DaemonSessionRuntimeRegistryV1, Database, DatabaseAccessMode, DatabaseAuthority, Result,
    open_runtime, session_registry_error,
};

impl DaemonSessionRuntimeRegistryV1 {
    async fn project_graph_database(
        &self,
        project_id: ProjectId,
        database_path: PathBuf,
        database_authority: Option<DatabaseAuthority>,
        access: DatabaseAccessMode,
    ) -> Result<Database> {
        // The graph database is project-wide. Worktree/ref/snapshot identity is
        // retained in graph generations and query scope, never in the mutable
        // SQLite owner. Holding this map lock through first publication makes
        // linked-worktree opens one singleflight even though their route-local
        // servers and code-index schedulers remain distinct.
        //
        // Both sides of every locator check here are resolved before they are
        // compared. The retained and registered sides have been through
        // `fs::canonicalize` while the requested side is the name
        // `StoreLayout` built, and those two spellings differ on hosts that
        // offer more than one name for a file: Windows canonicalizes to the
        // `\\?\` verbatim form, macOS resolves `/var` to `/private/var`.
        // Comparing spellings refused a mount whose two locators name one
        // file.
        let mut mounted = self.project_memory.lock().await;
        if let Some(database) = mounted.get(&project_id) {
            if !same_canonical_path(database.canonical_database_path(), &database_path) {
                return Err(session_registry_error(
                    "reuse project graph runtime",
                    format!(
                        "project graph locator {} differs from retained canonical locator {}",
                        database_path.display(),
                        database.canonical_database_path().display()
                    ),
                ));
            }
            return match access {
                DatabaseAccessMode::ReadWrite => Ok(database.as_ref().clone()),
                DatabaseAccessMode::ReadOnly => {
                    Database::publish_runtime(
                        database.retained_runtime().clone(),
                        DatabaseAccessMode::ReadOnly,
                    )
                    .await
                }
            };
        }

        let shard_id = StoreShardIdV1::project(
            self.identity.brain_id().clone(),
            self.identity.profile_id().clone(),
            project_id.clone(),
        );
        let runtime = match database_authority {
            Some(authority) => {
                open_runtime(
                    &self.registry,
                    self.resolver.as_ref(),
                    shard_id.clone(),
                    self.incarnation,
                    Some(self.profile_pin.clone()),
                    Some(authority),
                    matches!(&access, DatabaseAccessMode::ReadWrite),
                    "mount project graph store",
                )
                .await?
            }
            None if matches!(&access, DatabaseAccessMode::ReadOnly) => {
                match self
                    .registry
                    .open(super::StoreRuntimeOpenRequest::new_read_only(
                        shard_id.clone(),
                        self.incarnation,
                        Some(self.profile_pin.clone()),
                    ))
                    .await
                {
                    super::StoreRuntimeOpenResult::Published(runtime) => runtime,
                    super::StoreRuntimeOpenResult::Failed(failure) => {
                        return Err(session_registry_error(
                            "mount project graph store read-only",
                            format!("{failure:?}"),
                        ));
                    }
                }
            }
            None => {
                return Err(session_registry_error(
                    "mount project graph store",
                    "writable graph routing requires daemon write authority".to_owned(),
                ));
            }
        };
        if !same_canonical_path(runtime.canonical_path(), &database_path) {
            return Err(session_registry_error(
                "mount project graph runtime",
                format!(
                    "project graph locator {} differs from registered locator {}",
                    database_path.display(),
                    runtime.canonical_path().display()
                ),
            ));
        }
        let writable = matches!(&access, DatabaseAccessMode::ReadWrite);
        let database = Database::publish_runtime(runtime, access).await?;
        if writable {
            let database = Arc::new(database);
            let graph_runtime = self
                .retain_memory_graph_runtime(shard_id.clone(), Arc::clone(&database))
                .await?;
            database.bind_memory_graph_runtime(Arc::new(graph_runtime))?;
            self.retain_memory_graph_reconciliation_task(&shard_id, database.as_ref())?;
            super::code_graph::schedule_bound_memory_graph_reconciliation(database.as_ref())?;
            mounted.insert(project_id, Arc::clone(&database));
            return Ok(database.as_ref().clone());
        }
        Ok(database)
    }

    pub(crate) async fn begin_destructive_code_maintenance(
        &self,
        root: &Path,
        database_paths: impl IntoIterator<Item = PathBuf>,
    ) -> Result<super::DestructiveMaintenanceReservation> {
        let target =
            super::DestructiveMaintenanceTarget::new(root, database_paths).map_err(|error| {
                session_registry_error(
                    "construct destructive code-store reservation",
                    format!("{error:?}"),
                )
            })?;
        let reservation = self
            .registry
            .begin_destructive_maintenance(target)
            .await
            .map_err(|error| {
                session_registry_error(
                    "reserve destructive code-store maintenance",
                    format!("{error:?}"),
                )
            })?;
        for closed in reservation.closed() {
            self.resolver
                .retire_code_authority(&closed.binding().shard_id, closed.path())
                .map_err(|error| {
                    session_registry_error(
                        "retire destructively closed code-shard authority",
                        format!("{error:?}"),
                    )
                })?;
        }
        Ok(reservation)
    }

    /// Drops the daemon's retained project facades before a destructive store
    /// reservation closes the underlying physical runtimes. The reservation
    /// then proves that no stale handle can recreate the deleted shard.
    pub(crate) async fn drop_project_runtime_caches(&self, project_id: &ProjectId) {
        self.project_memory.lock().await.remove(project_id);
        self.project_sessions.lock().await.remove(project_id);
    }

    /// Mounts the project-wide mutable graph. The checkout path is exact route
    /// provenance; the canonical database locator is supplied by `StoreLayout`.
    pub(crate) async fn project_graph(
        &self,
        _project_root: &Path,
        project_id: ProjectId,
        database_path: PathBuf,
        database_authority: DatabaseAuthority,
        access: DatabaseAccessMode,
    ) -> Result<Database> {
        self.project_graph_database(project_id, database_path, Some(database_authority), access)
            .await
    }

    pub(crate) async fn project_graph_registered(
        &self,
        project_id: ProjectId,
        database_path: PathBuf,
        access: DatabaseAccessMode,
    ) -> Result<Database> {
        self.project_graph_database(project_id, database_path, None, access)
            .await
    }
}
