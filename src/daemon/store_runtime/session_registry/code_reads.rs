use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracedecay_runtime_core::path_safety::same_canonical_path;
use tracedecay_store::{ProjectId, StoreShardIdV1};

use super::{
    DaemonSessionRuntimeRegistryV1, Database, DatabaseAccessMode, DatabaseAuthority, Result,
    open_runtime, session_registry_error,
};
use crate::errors::TraceDecayError;

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
        let has_entry = {
            let mounted = self
                .project_owners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match mounted.get(&project_id) {
                Some(super::ProjectRuntimeOwnerStateV1::Ready(owners)) => {
                    if let Some(owner) = owners.memory.as_ref() {
                        let database = match &access {
                            DatabaseAccessMode::ReadWrite => {
                                owner.graph_runtime.issue_database_lease()
                            }
                            DatabaseAccessMode::ReadOnly => {
                                owner.graph_runtime.issue_database_read_only_lease()
                            }
                        }
                        .map_err(|error| {
                            session_registry_error(
                                "issue retained project graph database client",
                                error.to_string(),
                            )
                        })?;
                        if !same_canonical_path(database.canonical_database_path(), &database_path)
                        {
                            return Err(session_registry_error(
                                "reuse project graph runtime",
                                format!(
                                    "project graph locator {} differs from retained canonical locator {}",
                                    database_path.display(),
                                    database.canonical_database_path().display()
                                ),
                            ));
                        }
                        return Ok(database);
                    }
                    true
                }
                Some(super::ProjectRuntimeOwnerStateV1::Opening) => {
                    return Err(TraceDecayError::project_route(
                        "project_runtime_opening",
                        true,
                        "Project runtime is already opening",
                    ));
                }
                Some(super::ProjectRuntimeOwnerStateV1::Retiring)
                | Some(super::ProjectRuntimeOwnerStateV1::ReplacingSessions)
                | Some(super::ProjectRuntimeOwnerStateV1::Recovering)
                | Some(super::ProjectRuntimeOwnerStateV1::RecoveryRequired(_))
                | Some(super::ProjectRuntimeOwnerStateV1::Faulted(_)) => {
                    return Err(TraceDecayError::project_route(
                        "project_runtime_retiring",
                        true,
                        "Project runtime is unavailable while retirement is terminal or in progress",
                    ));
                }
                None => false,
            }
        };
        if !writable {
            let shard_id = StoreShardIdV1::project(
                self.identity.brain_id().clone(),
                self.identity.profile_id().clone(),
                project_id,
            );
            let runtime = match self
                .registry
                .open(super::StoreRuntimeOpenRequest::new_read_only(
                    shard_id,
                    self.incarnation,
                    Some(
                        self.profile_authority_pin("mount project graph store read-only")
                            .await?,
                    ),
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
            return Database::publish_runtime(runtime, DatabaseAccessMode::ReadOnly)
                .await?
                .issue_read_only_lease()
                .map_err(|error| {
                    session_registry_error(
                        "issue project graph read-only database client",
                        format!("{error:?}"),
                    )
                });
        }
        let mut admission = match if has_entry {
            self.extend_project_runtime_owner(&project_id)
        } else {
            self.admit_project_runtime_owner(&project_id)
        }? {
            super::ProjectRuntimeOwnerAdmissionV1::Opening(admission) => admission,
            super::ProjectRuntimeOwnerAdmissionV1::Existing => {
                return Err(TraceDecayError::project_route(
                    "project_runtime_opening",
                    true,
                    "Project runtime changed while opening its graph store",
                ));
            }
        };

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
                    Some(
                        self.profile_authority_pin("mount project graph store")
                            .await?,
                    ),
                    Some(authority),
                    matches!(&access, DatabaseAccessMode::ReadWrite),
                    "mount project graph store",
                )
                .await?
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
        let (owner, database) = self.publish_memory_owner(shard_id, runtime).await?;
        admission.publish_memory(owner)?;
        Ok(database.as_ref().clone())
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
        let mut owners = self
            .project_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(
            owners.get(project_id),
            Some(super::ProjectRuntimeOwnerStateV1::Ready(owners))
                if owners.sessions.is_none() && owners.memory.is_none()
        ) {
            owners.remove(project_id);
        }
    }

    async fn restore_replaced_project_session_ready(
        &self,
        project_id: &ProjectId,
        replacement: &mut super::ProjectSessionReplacementReservationV1,
    ) -> Result<()> {
        let lease = replacement.issue_old_lease()?;
        let path = lease.db_path().to_path_buf();
        let (issuer, binding, locator, path) = replacement.replay_descriptor(path)?;
        self.rebind_project_session_sync(project_id, &lease).await?;
        drop(lease);
        self.remote_replay_transaction
            .register_target(project_id.clone(), issuer, binding, locator, path)
            .map_err(|error| session_registry_error("restore project replay target", error))?;
        replacement.restore_old_ready()
    }

    pub(crate) async fn retire_project_session_relation_graph(
        &self,
        project_id: &ProjectId,
    ) -> Result<()> {
        let Some(mut replacement) = self.reserve_project_session_replacement(project_id)? else {
            return Ok(());
        };

        // Replay and sync may issue counted database clients. Fence both before
        // the database reservation so their in-flight work appears as a real
        // Store blocker instead of racing a native close.
        let old_lease = replacement.issue_old_lease()?;
        let path = old_lease.db_path().to_path_buf();
        let (_, binding, _, _) = replacement.replay_descriptor(path)?;
        drop(old_lease);
        self.remote_replay_transaction
            .unregister_target(project_id, &binding)
            .map_err(|error| session_registry_error("quiesce project replay target", error))?;
        if let Err(error) = self.retire_project_session_sync(project_id).await {
            let restore = self
                .restore_replaced_project_session_ready(project_id, &mut replacement)
                .await;
            if let Err(restore_error) = restore {
                replacement.commit_recovery_required(
                    super::ProjectSessionRecoveryPhaseV1::ReservationAbandoned,
                )?;
                return Err(session_registry_error(
                    "quiesce project session sync",
                    format!("{error}; restore={restore_error}"),
                ));
            }
            return Err(error);
        }

        let graph_target = replacement.graph_retirement_target()?;
        let graph_reservation = match self
            .graph_registry
            .reserve_retirement_batch(vec![graph_target])
        {
            Ok(reservation) => reservation,
            Err(refusal) => {
                let restore = self
                    .restore_replaced_project_session_ready(project_id, &mut replacement)
                    .await;
                if let Err(restore_error) = restore {
                    replacement.commit_recovery_required(
                        super::ProjectSessionRecoveryPhaseV1::ReservationAbandoned,
                    )?;
                    return Err(session_registry_error(
                        "reserve project session graph retirement",
                        format!("{}; restore={restore_error}", refusal.error()),
                    ));
                }
                return Err(session_registry_error(
                    "reserve project session graph retirement",
                    refusal.error().to_string(),
                ));
            }
        };
        let store_target = match replacement.reserve_store_target() {
            Ok(target) => target,
            Err(error) => {
                drop(graph_reservation);
                let restore = self
                    .restore_replaced_project_session_ready(project_id, &mut replacement)
                    .await;
                if let Err(restore_error) = restore {
                    replacement.commit_recovery_required(
                        super::ProjectSessionRecoveryPhaseV1::ReservationAbandoned,
                    )?;
                    return Err(session_registry_error(
                        "reserve project session Store retirement",
                        format!("{error}; restore={restore_error}"),
                    ));
                }
                return Err(error);
            }
        };
        let store_reservation = match self.registry.reserve_retirement_batch(vec![store_target]) {
            tracedecay_runtime_core::store_runtime::registry::StoreRuntimeRetirementResult::Reserved(
                reservation,
            ) => reservation,
            tracedecay_runtime_core::store_runtime::registry::StoreRuntimeRetirementResult::Blocked(
                refusal,
            ) => {
                let (blockers, mut targets) = refusal.into_parts();
                let target = targets.pop().ok_or_else(|| {
                    session_registry_error(
                        "recover blocked project session Store retirement",
                        "Store refusal omitted the exact retirement target".to_owned(),
                    )
                })?;
                if !targets.is_empty() {
                    return Err(session_registry_error(
                        "recover blocked project session Store retirement",
                        "Store refusal returned an unexpected target count".to_owned(),
                    ));
                }
                let handoff = target.into_database_graph_owner_handoff().map_err(|_| {
                    session_registry_error(
                        "recover blocked project session Store retirement",
                        "Store refusal lost the paired database/graph owner handoff".to_owned(),
                    )
                })?;
                replacement.restore_store_target(handoff.cancel_to_ready_graph_target())?;
                drop(graph_reservation);
                let restore = self
                    .restore_replaced_project_session_ready(project_id, &mut replacement)
                    .await;
                if let Err(restore_error) = restore {
                    replacement.commit_recovery_required(
                        super::ProjectSessionRecoveryPhaseV1::ReservationAbandoned,
                    )?;
                    return Err(session_registry_error(
                        "reserve project session Store retirement",
                        format!("{blockers:?}; restore={restore_error}"),
                    ));
                }
                return Err(session_registry_error(
                    "reserve project session Store retirement",
                    format!("{blockers:?}"),
                ));
            }
        };
        let mut native = super::ProjectSessionNativeRetirementV1::new(
            replacement,
            graph_reservation,
            store_reservation,
        );
        let graph = match native.graph_mut()?.commit(
            Arc::new(tracedecay_graph_db::NeverCancelled),
            std::time::Instant::now() + std::time::Duration::from_secs(30),
        ) {
            Ok(commit) => {
                native.mark_graph_native_boundary();
                commit
            }
            Err(refusal) => {
                let mut replacement = native.cancel_before_native()?;
                let restore = self
                    .restore_replaced_project_session_ready(project_id, &mut replacement)
                    .await;
                if let Err(restore_error) = restore {
                    replacement.commit_recovery_required(
                        super::ProjectSessionRecoveryPhaseV1::ReservationAbandoned,
                    )?;
                    return Err(session_registry_error(
                        "commit project session graph retirement",
                        format!("{}; restore={restore_error}", refusal.error()),
                    ));
                }
                return Err(session_registry_error(
                    "commit project session graph retirement",
                    refusal.error().to_string(),
                ));
            }
        };
        let store = match native.store_mut()?.commit() {
            Ok(commit) => commit,
            Err(error) => {
                native.recover_after_graph_native_boundary(graph)?;
                return Err(session_registry_error(
                    "commit project session Store retirement",
                    format!("{error:?}"),
                ));
            }
        };
        let vacancy = native.into_vacancy(graph, store)?;
        vacancy.commit_without_sessions()
    }

    pub(crate) async fn retire_project_memory_graph(&self, project_id: &ProjectId) -> Result<()> {
        let Some(mut retirement) = self.reserve_project_runtime_retirement(project_id)? else {
            return Ok(());
        };
        if retirement
            .retained
            .as_ref()
            .and_then(|owners| owners.memory.as_ref())
            .is_none()
        {
            return retirement.commit_ready_or_remove();
        }
        let operation_admission = retirement
            .memory()?
            .graph_runtime
            .reserve_operation_retirement()
            .map_err(|error| {
                session_registry_error(
                    "reserve project memory graph operation admission",
                    error.to_string(),
                )
            })?;
        let reconciliation = retirement
            .memory()?
            .reconciliation
            .reserve_retirement()
            .map_err(|blocker| {
                session_registry_error(
                    "reserve project memory reconciliation retirement",
                    format!("{blocker:?}"),
                )
            })?;
        let graph_target = retirement.memory()?.graph_runtime.graph_retirement_target();
        let mut graph_reservation = self
            .graph_registry
            .reserve_retirement_batch(vec![graph_target])
            .map_err(|refusal| {
                session_registry_error(
                    "reserve project memory graph retirement",
                    refusal.error().to_string(),
                )
            })?;
        let store_target = {
            let owner = retirement.memory()?;
            let database = owner
                .graph_runtime
                .reserve_database_retirement()
                .map_err(|error| {
                    session_registry_error(
                        "reserve project memory database retirement",
                        error.to_string(),
                    )
                })?;
            let graph = owner
                .graph_runtime
                .take_store_graph_retirement_target()
                .map_err(|error| {
                    session_registry_error(
                        "reserve project memory graph Store target",
                        error.to_string(),
                    )
                })?;
            match database.into_store_retirement_target_with_graph(graph) {
                Ok(target) => target,
                Err(refusal) => {
                    let (error, database, graph) = refusal.into_parts();
                    drop(database);
                    owner
                        .graph_runtime
                        .restore_store_graph_retirement_target(graph)
                        .map_err(|restore_error| {
                            session_registry_error(
                                "restore project memory graph Store target",
                                restore_error.to_string(),
                            )
                        })?;
                    return Err(session_registry_error(
                        "compose project memory Store retirement target",
                        format!("{error:?}"),
                    ));
                }
            }
        };
        let mut store_reservation = match self.registry.reserve_retirement_batch(vec![store_target]) {
            tracedecay_runtime_core::store_runtime::registry::StoreRuntimeRetirementResult::Reserved(
                reservation,
            ) => reservation,
            tracedecay_runtime_core::store_runtime::registry::StoreRuntimeRetirementResult::Blocked(
                refusal,
            ) => {
                let (blockers, mut targets) = refusal.into_parts();
                let target = targets.pop().ok_or_else(|| {
                    session_registry_error(
                        "recover blocked project memory Store retirement",
                        "Store refusal omitted the exact retirement target".to_owned(),
                    )
                })?;
                if !targets.is_empty() {
                    return Err(session_registry_error(
                        "recover blocked project memory Store retirement",
                        "Store refusal returned an unexpected target count".to_owned(),
                    ));
                }
                let target = target.into_database_graph_owner_handoff().map_err(|_| {
                    session_registry_error(
                        "recover blocked project memory Store retirement",
                        "Store refusal lost the paired database/graph owner handoff".to_owned(),
                    )
                })?;
                retirement
                    .memory()?
                    .graph_runtime
                    .restore_store_graph_retirement_target(target.cancel_to_ready_graph_target())
                    .map_err(|error| {
                        session_registry_error(
                            "restore project memory graph Store target",
                            error.to_string(),
                        )
                    })?;
                return Err(session_registry_error(
                    "reserve project memory Store retirement",
                    format!("{blockers:?}"),
                ));
            }
        };
        let reconciliation = match reconciliation.commit_and_wait().await {
            Ok(terminal) => terminal,
            Err(error) => {
                let mut targets = store_reservation.cancel().map_err(|cancel_error| {
                    session_registry_error(
                        "cancel project memory Store retirement after reconciliation start refusal",
                        format!("{cancel_error:?}"),
                    )
                })?;
                let target = targets.pop().ok_or_else(|| {
                    session_registry_error(
                        "recover project memory Store retirement after reconciliation start refusal",
                        "Store cancellation omitted the exact retirement target".to_owned(),
                    )
                })?;
                if !targets.is_empty() {
                    return Err(session_registry_error(
                        "recover project memory Store retirement after reconciliation start refusal",
                        "Store cancellation returned an unexpected target count".to_owned(),
                    ));
                }
                let target = target.into_database_graph_owner_handoff().map_err(|_| {
                    session_registry_error(
                        "recover project memory Store retirement after reconciliation start refusal",
                        "Store cancellation lost the paired database/graph owner handoff".to_owned(),
                    )
                })?;
                retirement
                    .memory()?
                    .graph_runtime
                    .restore_store_graph_retirement_target(target.cancel_to_ready_graph_target())
                    .map_err(|restore_error| {
                        session_registry_error(
                            "restore project memory graph Store target",
                            restore_error.to_string(),
                        )
                    })?;
                return Err(session_registry_error(
                    "start project memory reconciliation retirement",
                    format!("{error:?}"),
                ));
            }
        };
        if !matches!(
            reconciliation,
            tracedecay_runtime_core::db::MemoryGraphReconciliationRetirementTerminalV1::CancelledAndJoined
        ) {
            operation_admission.commit();
            retirement.commit_fault(super::ProjectRuntimeRetirementFaultV1::Reconciliation(
                reconciliation,
            ))?;
            return Err(session_registry_error(
                "retire project memory reconciliation",
                "memory reconciliation reached a terminal failure after admission closed".to_owned(),
            ));
        }
        let graph = match graph_reservation.commit(
            Arc::new(tracedecay_graph_db::NeverCancelled),
            std::time::Instant::now() + std::time::Duration::from_secs(30),
        ) {
            Ok(commit) => commit,
            Err(refusal) => {
                operation_admission.commit();
                retirement.commit_fault(super::ProjectRuntimeRetirementFaultV1::GraphRefusal(
                    refusal.error().clone(),
                ))?;
                return Err(session_registry_error(
                    "commit project memory graph retirement",
                    refusal.error().to_string(),
                ));
            }
        };
        let store = match store_reservation.commit() {
            Ok(commit) => commit,
            Err(error) => {
                let mut targets = store_reservation.cancel().map_err(|cancel_error| {
                    session_registry_error(
                        "cancel project memory Store retirement after graph terminal",
                        format!("{cancel_error:?}"),
                    )
                })?;
                let target = targets.pop().ok_or_else(|| {
                    session_registry_error(
                        "recover project memory Store retirement after graph terminal",
                        "Store cancellation omitted the exact retirement target".to_owned(),
                    )
                })?;
                if !targets.is_empty() {
                    return Err(session_registry_error(
                        "recover project memory Store retirement after graph terminal",
                        "Store cancellation returned an unexpected target count".to_owned(),
                    ));
                }
                let handoff = target.into_database_graph_owner_handoff().map_err(|_| {
                    session_registry_error(
                        "recover project memory Store retirement after graph terminal",
                        "Store cancellation lost the paired database/graph owner handoff"
                            .to_owned(),
                    )
                })?;
                retirement
                    .memory()?
                    .graph_runtime
                    .restore_store_graph_retirement_target(handoff.cancel_to_ready_graph_target())
                    .map_err(|restore_error| {
                        session_registry_error(
                            "restore project memory graph Store target after graph terminal",
                            restore_error.to_string(),
                        )
                    })?;
                operation_admission.commit();
                retirement.commit_fault(super::ProjectRuntimeRetirementFaultV1::StoreStart(
                    error.clone(),
                ))?;
                return Err(session_registry_error(
                    "commit project memory Store retirement",
                    format!("{error:?}"),
                ));
            }
        };
        let graph_closed = graph.outcomes().iter().all(|outcome| {
            matches!(
                outcome,
                tracedecay_graph_db::GraphDbRetirementOutcome::Closed(_)
            )
        });
        let store_closed = store.outcomes().iter().all(|outcome| {
            matches!(
                outcome,
                tracedecay_runtime_core::store_runtime::registry::StoreRuntimeRetirementOutcome::Closed { .. }
            )
        });
        if graph_closed && store_closed {
            operation_admission.commit();
            return retirement.commit_without_memory();
        }
        operation_admission.commit();
        retirement
            .commit_fault(super::ProjectRuntimeRetirementFaultV1::Terminal { graph, store })?;
        Err(session_registry_error(
            "retire project memory runtime",
            "project memory graph or Store retirement reached a terminal failure".to_owned(),
        ))
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
