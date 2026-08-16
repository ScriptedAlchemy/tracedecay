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
        let writable = matches!(&access, DatabaseAccessMode::ReadWrite);
        {
            let mounted = self.project_memory.lock().await;
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
        }
        if writable {
            self.ensure_project_memory_runtime_capacity(&project_id)
                .await?;
        }
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

    pub(crate) async fn retire_project_session_relation_graph(
        &self,
        project_id: &ProjectId,
    ) -> Result<()> {
        let mut mounted = self.project_sessions.lock().await;
        let Some(database) = mounted.get(project_id) else {
            return Ok(());
        };
        if Arc::strong_count(database) != 1 {
            self.retirement_refusals
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            return Err(session_registry_error(
                "retire project session runtime",
                "project session runtime is still leased".to_owned(),
            ));
        }
        let (graph_binding, graph_verified_locator) = database
            .session_relation_graph_identity()
            .map(|(binding, locator)| (binding.clone(), locator.clone()))?;
        let runtime_binding = database.binding().clone();
        let runtime_authority = database.authority().clone();
        self.remote_replay_transaction
            .unregister_target(project_id, database.binding())
            .map_err(|error| {
                session_registry_error("retire project session replay target", error)
            })?;
        let database = mounted.remove(project_id).ok_or_else(|| {
            session_registry_error(
                "retire project session relation graph",
                "mounted ProjectSessions authority disappeared during retirement".to_owned(),
            )
        })?;
        drop(database);
        drop(mounted);
        if let Err(close_error) = super::code_graph::graph_attachment::close_retained(
            &self.graph_registry,
            graph_binding,
            graph_verified_locator,
        )
        .await
        {
            let restored = self
                .mount_registered_project_sessions(project_id.clone())
                .await
                .map_err(|restore_error| {
                    session_registry_error(
                        "restore project session authority after relation graph close refusal",
                        format!("{close_error}; remount failed: {restore_error}"),
                    )
                })?;
            if let Some(session_sync) = self
                .session_sync_service
                .get()
                .and_then(std::sync::Weak::upgrade)
            {
                session_sync
                    .rebind_project(self.identity.profile_id(), project_id, &restored)
                    .await
                    .map_err(|rebind_error| {
                        session_registry_error(
                            "restore project session sync after relation graph close refusal",
                            format!("{close_error}; rebind failed: {rebind_error}"),
                        )
                    })?;
            }
            self.retirement_refusals
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            return Err(close_error);
        }
        if let Err(close_error) = self
            .registry
            .close_exact(&runtime_binding, &runtime_authority)
            .await
        {
            let restored = self
                .mount_registered_project_sessions(project_id.clone())
                .await
                .map_err(|restore_error| {
                    session_registry_error(
                        "restore project session authority after runtime close refusal",
                        format!("{close_error:?}; remount failed: {restore_error}"),
                    )
                })?;
            if let Some(session_sync) = self
                .session_sync_service
                .get()
                .and_then(std::sync::Weak::upgrade)
            {
                session_sync
                    .rebind_project(self.identity.profile_id(), project_id, &restored)
                    .await
                    .map_err(|rebind_error| {
                        session_registry_error(
                            "restore project session sync after runtime close refusal",
                            format!("{close_error:?}; rebind failed: {rebind_error}"),
                        )
                    })?;
            }
            self.retirement_refusals
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            return Err(session_registry_error(
                "close project session runtime",
                format!("{close_error:?}"),
            ));
        }
        self.retired_project_session_runtimes
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Ok(())
    }

    pub(crate) async fn retire_project_memory_graph(&self, project_id: &ProjectId) -> Result<()> {
        let (runtime_binding, runtime_authority, shard_id, database_path) = {
            let mounted = self.project_memory.lock().await;
            let Some(database) = mounted.get(project_id) else {
                return Ok(());
            };
            if Arc::strong_count(database) != 1 {
                self.retirement_refusals
                    .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                return Err(session_registry_error(
                    "retire project memory runtime",
                    "project memory runtime is still leased".to_owned(),
                ));
            }
            (
                database.retained_runtime().binding().clone(),
                database.write_authority()?,
                database.retained_runtime().binding().shard_id.clone(),
                database.canonical_database_path().to_path_buf(),
            )
        };
        self.retire_memory_graph_reconciliation_task(&shard_id)
            .await?;
        let database = self.project_memory.lock().await.remove(project_id);
        drop(database);
        if let Err(close_error) = self
            .registry
            .close_exact(&runtime_binding, &runtime_authority)
            .await
        {
            self.project_graph_database(
                project_id.clone(),
                database_path,
                Some(runtime_authority),
                DatabaseAccessMode::ReadWrite,
            )
            .await
            .map_err(|restore_error| {
                session_registry_error(
                    "restore project memory authority after runtime close refusal",
                    format!("{close_error:?}; remount failed: {restore_error}"),
                )
            })?;
            self.retirement_refusals
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            return Err(session_registry_error(
                "close project memory runtime",
                format!("{close_error:?}"),
            ));
        }
        self.retired_project_memory_runtimes
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Ok(())
    }

    async fn retire_profile_session_relation_graph(&self) -> Result<()> {
        let mut mounted = self.profile_sessions.lock().await;
        let Some(database) = mounted.as_ref() else {
            return Ok(());
        };
        if Arc::strong_count(database) != 1 {
            self.retirement_refusals
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            return Err(session_registry_error(
                "retire profile session runtime",
                "profile session runtime is still leased".to_owned(),
            ));
        }
        let (graph_binding, graph_verified_locator) = database
            .session_relation_graph_identity()
            .map(|(binding, locator)| (binding.clone(), locator.clone()))?;
        let runtime_binding = database.binding().clone();
        let runtime_authority = database.authority().clone();
        let database = mounted.take().ok_or_else(|| {
            session_registry_error(
                "retire profile session runtime",
                "mounted ProfileSessions authority disappeared during retirement".to_owned(),
            )
        })?;
        drop(database);
        drop(mounted);
        if let Err(close_error) = super::code_graph::graph_attachment::close_retained(
            &self.graph_registry,
            graph_binding,
            graph_verified_locator,
        )
        .await
        {
            self.profile_sessions().await.map_err(|restore_error| {
                session_registry_error(
                    "restore profile session authority after relation graph close refusal",
                    format!("{close_error}; remount failed: {restore_error}"),
                )
            })?;
            self.retirement_refusals
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            return Err(close_error);
        }
        if let Err(close_error) = self
            .registry
            .close_exact(&runtime_binding, &runtime_authority)
            .await
        {
            self.profile_sessions().await.map_err(|restore_error| {
                session_registry_error(
                    "restore profile session authority after runtime close refusal",
                    format!("{close_error:?}; remount failed: {restore_error}"),
                )
            })?;
            self.retirement_refusals
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            return Err(session_registry_error(
                "close profile session runtime",
                format!("{close_error:?}"),
            ));
        }
        Ok(())
    }

    async fn retire_profile_memory_graph(&self) -> Result<()> {
        let (runtime_binding, runtime_authority, shard_id) = {
            let mounted = self.profile_memory.lock().await;
            let Some(database) = mounted.as_ref() else {
                return Ok(());
            };
            if Arc::strong_count(database) != 1 {
                self.retirement_refusals
                    .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                return Err(session_registry_error(
                    "retire profile memory runtime",
                    "profile memory runtime is still leased".to_owned(),
                ));
            }
            (
                database.retained_runtime().binding().clone(),
                database.write_authority()?,
                database.retained_runtime().binding().shard_id.clone(),
            )
        };
        self.retire_memory_graph_reconciliation_task(&shard_id)
            .await?;
        let database = self.profile_memory.lock().await.take();
        drop(database);
        if let Err(close_error) = self
            .registry
            .close_exact(&runtime_binding, &runtime_authority)
            .await
        {
            self.profile_memory().await.map_err(|restore_error| {
                session_registry_error(
                    "restore profile memory authority after runtime close refusal",
                    format!("{close_error:?}; remount failed: {restore_error}"),
                )
            })?;
            self.retirement_refusals
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            return Err(session_registry_error(
                "close profile memory runtime",
                format!("{close_error:?}"),
            ));
        }
        Ok(())
    }

    pub(crate) async fn shutdown_retained_runtimes(&self) -> Result<()> {
        self.cancel_memory_graph_reconciliation_tasks();
        let mut failures = Vec::new();
        let project_ids = self
            .project_sessions
            .lock()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for project_id in project_ids {
            if let Err(error) = self
                .retire_project_session_relation_graph(&project_id)
                .await
            {
                failures.push(error.to_string());
            }
        }
        let project_ids = self
            .project_memory
            .lock()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for project_id in project_ids {
            if let Err(error) = self.retire_project_memory_graph(&project_id).await {
                failures.push(error.to_string());
            }
        }
        if let Err(error) = self.retire_profile_session_relation_graph().await {
            failures.push(error.to_string());
        }
        if let Err(error) = self.retire_profile_memory_graph().await {
            failures.push(error.to_string());
        }
        if let Err(error) = self.shutdown_memory_graph_reconciliation_tasks().await {
            failures.push(error);
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(session_registry_error(
                "shutdown retained session runtimes",
                failures.join("; "),
            ))
        }
    }

    pub(crate) async fn retire_retained_runtimes_for_capacity(&self) -> Result<()> {
        let project_ids = self
            .project_sessions
            .lock()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for project_id in project_ids {
            self.retire_project_session_relation_graph(&project_id)
                .await?;
        }
        let project_ids = self
            .project_memory
            .lock()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for project_id in project_ids {
            self.retire_project_memory_graph(&project_id).await?;
        }
        self.retire_profile_session_relation_graph().await?;
        self.retire_profile_memory_graph().await?;
        if self.memory_graph_reconciliation_tasks.retained_count()? != 0 {
            self.retirement_refusals
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            return Err(session_registry_error(
                "retire profile session runtime registry",
                "profile has pending memory graph reconciliation tasks".to_owned(),
            ));
        }
        Ok(())
    }

    pub(super) async fn ensure_project_session_runtime_capacity(
        &self,
        requested: &ProjectId,
    ) -> Result<()> {
        loop {
            let candidate = {
                let mounted = self.project_sessions.lock().await;
                if mounted.contains_key(requested)
                    || mounted.len() < self.project_runtime_capacity.get()
                {
                    return Ok(());
                }
                mounted.iter().find_map(|(project_id, database)| {
                    (Arc::strong_count(database) == 1).then(|| project_id.clone())
                })
            };
            let Some(candidate) = candidate else {
                self.retirement_refusals
                    .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                return Err(session_registry_error(
                    "admit project session runtime",
                    "project runtime retention capacity is exhausted by active session runtimes"
                        .to_owned(),
                ));
            };
            if let Err(error) = self.retire_project_session_relation_graph(&candidate).await {
                return Err(session_registry_error(
                    "retire idle project session runtime",
                    error.to_string(),
                ));
            }
        }
    }

    pub(super) async fn ensure_project_memory_runtime_capacity(
        &self,
        requested: &ProjectId,
    ) -> Result<()> {
        loop {
            let candidate = {
                let mounted = self.project_memory.lock().await;
                if mounted.contains_key(requested)
                    || mounted.len() < self.project_runtime_capacity.get()
                {
                    return Ok(());
                }
                mounted.iter().find_map(|(project_id, database)| {
                    (Arc::strong_count(database) == 1).then(|| project_id.clone())
                })
            };
            let Some(candidate) = candidate else {
                self.retirement_refusals
                    .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                return Err(session_registry_error(
                    "admit project memory runtime",
                    "project runtime retention capacity is exhausted by active memory runtimes"
                        .to_owned(),
                ));
            };
            if let Err(error) = self.retire_project_memory_graph(&candidate).await {
                return Err(session_registry_error(
                    "retire idle project memory runtime",
                    error.to_string(),
                ));
            }
        }
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
