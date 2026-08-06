use std::path::{Path, PathBuf};
use std::sync::Arc;

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
        let mut mounted = self.project_memory.lock().await;
        if let Some(database) = mounted.get(&project_id) {
            if database.canonical_database_path() != database_path {
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
                    shard_id,
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
                        shard_id,
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
        if runtime.canonical_path() != database_path {
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
            mounted.insert(project_id, Arc::new(database.clone()));
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

    pub(crate) async fn close_code_graph_paths(
        &self,
        database_paths: impl IntoIterator<Item = PathBuf>,
    ) -> Result<()> {
        for database_path in database_paths {
            let closed = self
                .registry
                .close_path(&database_path)
                .await
                .map_err(|error| {
                    session_registry_error(
                        "close registered code-shard runtime",
                        format!("{error:?}"),
                    )
                })?;
            if let Some(closed) = closed {
                self.resolver
                    .retire_code_authority(&closed.binding().shard_id, closed.path())
                    .map_err(|error| {
                        session_registry_error(
                            "retire registered code-shard authority",
                            format!("{error:?}"),
                        )
                    })?;
            }
        }
        Ok(())
    }

    /// Mounts the mutable graph for this exact project/repository/worktree
    /// identity. The checkout path is used only by the Git identity authority;
    /// it is never itself the shard identity.
    pub(crate) async fn code_graph_worktree(
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

    /// Mounts the mutable graph for an exact named Git ref in this worktree.
    /// The ref is normalized to its full `refs/heads/*` identity before it
    /// enters the shard key.
    pub(crate) async fn code_graph_branch(
        &self,
        project_root: &Path,
        project_id: ProjectId,
        branch_name: &str,
        database_path: PathBuf,
        database_authority: DatabaseAuthority,
        access: DatabaseAccessMode,
    ) -> Result<Database> {
        self.code_graph_branch_with_authority(
            project_root,
            project_id,
            branch_name,
            database_path,
            Some(database_authority),
            access,
        )
        .await
    }

    pub(crate) async fn code_graph_branch_registered(
        &self,
        project_root: &Path,
        project_id: ProjectId,
        branch_name: &str,
        database_path: PathBuf,
        access: DatabaseAccessMode,
    ) -> Result<Database> {
        self.code_graph_branch_with_authority(
            project_root,
            project_id,
            branch_name,
            database_path,
            None,
            access,
        )
        .await
    }

    async fn code_graph_branch_with_authority(
        &self,
        _project_root: &Path,
        project_id: ProjectId,
        _branch_name: &str,
        database_path: PathBuf,
        database_authority: Option<DatabaseAuthority>,
        access: DatabaseAccessMode,
    ) -> Result<Database> {
        self.project_graph_database(project_id, database_path, database_authority, access)
            .await
    }
}
