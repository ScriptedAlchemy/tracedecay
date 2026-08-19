use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU8;

use tracedecay_domain::ProjectId;
use tracedecay_global_db::session_temporal::relations::SessionRelationScope;
use tracedecay_runtime_core::storage::PrivateStoreIo;
use tracedecay_store::{StoreRuntimeBindingV1, StoreShardIdV1};

use super::super::open_runtime_during_remote_restore;
use super::{
    RemoteRecoveryPublicationContextV1, Result, interruption_value, session_registry_error,
};
use crate::daemon::store_runtime::session_registry::{
    ProjectRuntimeOwnerStateV1, ProjectSessionNativeRetirementV1,
    ProjectSessionReplacementReservationV1, ProjectSessionReplacementVacancyV1,
    RegisteredSessionOwnerV1, SessionGraphOwnerV1,
};
use crate::db::{Database, DatabaseAccessMode};
use crate::global_db::{RegisteredGlobalDbLeaseV1, RegisteredGlobalDbOwnerV1};

mod quarantine;

pub(in crate::daemon::store_runtime::session_registry) use quarantine::remote_restore_activated_open_identity;
use quarantine::{
    activate_remote_restore_quarantine, complete_remote_restore_quarantine,
    install_remote_restore_quarantine, read_remote_restore_quarantine,
    recover_remote_restore_quarantine_outcome,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RestorePublicationV1 {
    Published,
    RolledBack,
}

impl RemoteRecoveryPublicationContextV1 {
    fn issue_session_lease(
        &self,
        owner: &RegisteredSessionOwnerV1,
        project_id: &ProjectId,
    ) -> Result<RegisteredGlobalDbLeaseV1> {
        let database = owner.database.issue_lease().map_err(|error| {
            session_registry_error(
                "issue remote recovery project session client",
                format!("{error:?}"),
            )
        })?;
        let graph = owner.relation_graph.graph.issue_lease().map_err(|error| {
            session_registry_error(
                "issue remote recovery session relation graph client",
                error.to_string(),
            )
        })?;
        database
            .bind_session_relation_graph(
                SessionRelationScope::project_sessions(project_id.clone()),
                graph,
                owner.relation_graph.graph.binding().clone(),
                owner.relation_graph.graph.verified_locator().clone(),
            )
            .map_err(|_| {
                session_registry_error(
                    "bind remote recovery session relation graph client",
                    "issued graph client did not match the exact project owner".to_owned(),
                )
            })?;
        Ok(database)
    }

    async fn restore_replacement_ready(
        &self,
        project_id: &ProjectId,
        replacement: &mut ProjectSessionReplacementReservationV1,
    ) -> Result<()> {
        let lease = replacement.issue_old_lease()?;
        let path = lease.db_path().to_path_buf();
        let (issuer, binding, locator, path) = replacement.replay_descriptor(path)?;
        self.rebind_project_session_sync(project_id, &lease).await?;
        drop(lease);
        self.replay
            .register_target(project_id.clone(), issuer, binding, locator, path)
            .map_err(|error| {
                session_registry_error("restore remote recovery replay target", error)
            })?;
        replacement.restore_old_ready()
    }

    async fn recover_pre_native<T>(
        &self,
        project_id: &ProjectId,
        mut replacement: ProjectSessionReplacementReservationV1,
        operation: &'static str,
        error: crate::errors::TraceDecayError,
    ) -> Result<T> {
        // A recovered candidate has never been published to Ready. Its prior
        // exact close proof remains the only authority for another physical
        // swap, so a reversible refusal must retain that same candidate in
        // RecoveryRequired rather than rebind it as a serving owner.
        if replacement.is_recovered_candidate() {
            replacement.commit_recovered_candidate_required()?;
            return Err(error);
        }
        match self
            .restore_replacement_ready(project_id, &mut replacement)
            .await
        {
            Ok(()) => Err(error),
            Err(restore_error) => {
                replacement.commit_recovery_required(
                    super::super::ProjectSessionRecoveryPhaseV1::ReservationAbandoned,
                )?;
                Err(session_registry_error(
                    operation,
                    format!("{error}; restore={restore_error}"),
                ))
            }
        }
    }

    async fn reserve_project_session_vacancy(
        &self,
        project_id: &ProjectId,
        destination: &Path,
    ) -> Result<ProjectSessionReplacementVacancyV1> {
        let Some(replacement) = self
            .project_owners
            .reserve_session_replacement(project_id)?
        else {
            return Err(session_registry_error(
                "reserve remote recovery project sessions",
                "project session owner is not mounted".to_owned(),
            ));
        };
        self.retire_replacement_to_vacancy(project_id, destination, replacement)
            .await
    }

    async fn retire_replacement_to_vacancy(
        &self,
        project_id: &ProjectId,
        destination: &Path,
        mut replacement: ProjectSessionReplacementReservationV1,
    ) -> Result<ProjectSessionReplacementVacancyV1> {
        let lease = replacement.issue_old_lease()?;
        if lease.db_path() != destination {
            drop(lease);
            return self
                .recover_pre_native(
                    project_id,
                    replacement,
                    "reserve remote recovery project sessions",
                    session_registry_error(
                        "reserve remote recovery project sessions",
                        "mounted project session locator differs from restore destination"
                            .to_owned(),
                    ),
                )
                .await;
        }
        let (_, binding, _, _) = replacement.replay_descriptor(lease.db_path().to_path_buf())?;
        let quiescence = self.project_lifecycle()?.quiesce(project_id, &lease).await;
        drop(lease);
        if let Err(error) = quiescence {
            return self
                .recover_pre_native(
                    project_id,
                    replacement,
                    "quiesce remote recovery project",
                    error,
                )
                .await;
        }
        if let Err(error) = self.replay.unregister_target(project_id, &binding) {
            return self
                .recover_pre_native(
                    project_id,
                    replacement,
                    "quiesce remote recovery replay",
                    session_registry_error("quiesce remote recovery replay", error),
                )
                .await;
        }
        let graph_target = replacement.graph_retirement_target()?;
        let graph = match self
            .graph_registry
            .reserve_retirement_batch(vec![graph_target])
        {
            Ok(graph) => graph,
            Err(refusal) => {
                return self
                    .recover_pre_native(
                        project_id,
                        replacement,
                        "reserve remote recovery relation graph",
                        session_registry_error(
                            "reserve remote recovery relation graph",
                            refusal.error().to_string(),
                        ),
                    )
                    .await;
            }
        };
        let store_target = match replacement.reserve_store_target() {
            Ok(target) => target,
            Err(error) => {
                drop(graph);
                return self
                    .recover_pre_native(
                        project_id,
                        replacement,
                        "reserve remote recovery Store target",
                        error,
                    )
                    .await;
            }
        };
        let store = match self.registry.reserve_retirement_batch(vec![store_target]) {
            tracedecay_runtime_core::store_runtime::registry::StoreRuntimeRetirementResult::Reserved(
                reservation,
            ) => reservation,
            tracedecay_runtime_core::store_runtime::registry::StoreRuntimeRetirementResult::Blocked(
                refusal,
            ) => {
                let (blockers, mut targets) = refusal.into_parts();
                let target = targets.pop().ok_or_else(|| {
                    session_registry_error(
                        "recover blocked remote recovery Store retirement",
                        "Store refusal omitted the exact paired target".to_owned(),
                    )
                })?;
                if !targets.is_empty() {
                    return Err(session_registry_error(
                        "recover blocked remote recovery Store retirement",
                        "Store refusal returned an unexpected target count".to_owned(),
                    ));
                }
                let handoff = target.into_database_graph_owner_handoff().map_err(|_| {
                    session_registry_error(
                        "recover blocked remote recovery Store retirement",
                        "Store refusal lost the exact database/graph handoff".to_owned(),
                    )
                })?;
                replacement.restore_store_target(handoff.cancel_to_ready_graph_target())?;
                drop(graph);
                return self
                    .recover_pre_native(
                        project_id,
                        replacement,
                        "reserve remote recovery Store retirement",
                        session_registry_error(
                            "reserve remote recovery Store retirement",
                            format!("{blockers:?}"),
                        ),
                    )
                    .await;
            }
        };
        let mut native = ProjectSessionNativeRetirementV1::new(replacement, graph, store);
        let graph = match native.graph_mut()?.commit(
            Arc::new(tracedecay_graph_db::NeverCancelled),
            std::time::Instant::now() + std::time::Duration::from_secs(30),
        ) {
            Ok(commit) => {
                native.mark_graph_native_boundary();
                commit
            }
            Err(refusal) => {
                let replacement = native.cancel_before_native()?;
                return self
                    .recover_pre_native(
                        project_id,
                        replacement,
                        "commit remote recovery relation graph",
                        session_registry_error(
                            "commit remote recovery relation graph",
                            refusal.error().to_string(),
                        ),
                    )
                    .await;
            }
        };
        let store = match native.store_mut()?.commit() {
            Ok(commit) => commit,
            Err(error) => {
                native.recover_after_graph_native_boundary(graph)?;
                return Err(session_registry_error(
                    "commit remote recovery Store retirement",
                    format!("{error:?}"),
                ));
            }
        };
        let vacancy = native.into_vacancy(graph, store)?;
        Ok(vacancy)
    }

    async fn open_candidate_owner(
        &self,
        project_id: ProjectId,
        expected_opened_file_identity: u64,
    ) -> Result<RegisteredSessionOwnerV1> {
        let shard_id = StoreShardIdV1::project_sessions(
            self.identity.brain_id().clone(),
            self.identity.profile_id().clone(),
            project_id.clone(),
        );
        let runtime = open_runtime_during_remote_restore(
            &self.registry,
            self.resolver.as_ref(),
            shard_id.clone(),
            self.incarnation,
            Some(self.profile_pin.clone()),
            expected_opened_file_identity,
            "open remote recovery replacement project session store",
        )
        .await?;
        let database = Database::publish_runtime(runtime, DatabaseAccessMode::ReadWrite).await?;
        let database = RegisteredGlobalDbOwnerV1::admit_and_attach(database).await?;
        let (graph, store_target) =
            super::super::code_graph::graph_attachment::open_session_relation_owner(
                &self.registry,
                &self.graph_registry,
                &self.graph_lifecycle_cancelled,
                self.incarnation,
                shard_id,
            )
            .await?;
        Ok(RegisteredSessionOwnerV1 {
            database,
            relation_graph: SessionGraphOwnerV1 {
                graph,
                store_target,
            },
        })
    }

    async fn activate_candidate(
        &self,
        project_id: &ProjectId,
        activation: super::super::ProjectSessionCandidateActivationV1,
        destination: &Path,
        outcome: RestorePublicationV1,
    ) -> Result<()> {
        let (lease, issuer, binding, locator) = activation.issue_lease_with_replay_descriptor()?;
        let path = lease.db_path().to_path_buf();
        if let Err(error) =
            self.replay
                .register_target(project_id.clone(), issuer, binding.clone(), locator, path)
        {
            return Err(session_registry_error(
                "register remote recovery replacement replay target",
                error,
            ));
        }
        if let Err(error) = self.rebind_project_session_sync(project_id, &lease).await {
            let unregister = self.replay.unregister_target(project_id, &binding);
            return Err(session_registry_error(
                "rebind remote recovery replacement session sync",
                format!("{error}; replay cleanup={unregister:?}"),
            ));
        }
        drop(lease);
        if let Err(error) = activate_remote_restore_quarantine(destination, outcome) {
            let unregister = self.replay.unregister_target(project_id, &binding);
            let retire = self.retire_project_session_sync(project_id).await;
            return Err(session_registry_error(
                "durably activate remote recovery replacement owner",
                format!("{error}; replay cleanup={unregister:?}; sync cleanup={retire:?}"),
            ));
        }
        if let Err(error) = activation.publish() {
            let unregister = self.replay.unregister_target(project_id, &binding);
            let retire = self.retire_project_session_sync(project_id).await;
            return Err(session_registry_error(
                "publish remote recovery replacement owner",
                format!("{error}; replay cleanup={unregister:?}; sync cleanup={retire:?}"),
            ));
        }
        Ok(())
    }

    fn resume_terminal_vacancy(
        &self,
        project_id: &ProjectId,
        terminal_vacancy: crate::daemon::store_runtime::session_registry::ProjectSessionTerminalVacancyAuthorityV1,
    ) -> Result<(
        ProjectSessionReplacementVacancyV1,
        Option<RegisteredSessionOwnerV1>,
    )> {
        match self.project_owners.reserve_session_recovery(project_id)? {
            Some(recovery) => recovery.into_terminal_vacancy(),
            None => {
                self.project_owners
                    .reconstruct_durable_terminal_recovery(project_id, terminal_vacancy)?;
                let recovery = self
                    .project_owners
                    .reserve_session_recovery(project_id)?
                    .ok_or_else(|| {
                        session_registry_error(
                            "rebuild terminal remote recovery vacancy",
                            "durable recovery record disappeared before reservation".to_owned(),
                        )
                    })?;
                recovery.into_terminal_vacancy()
            }
        }
    }

    pub(super) async fn ensure_project_sessions_target_while_admitted(
        &self,
        project_id: ProjectId,
        expected_opened_file_identity: u64,
        expected_destination: &Path,
    ) -> Result<()> {
        let entries = self.project_owners.lock().map_err(|_| {
            session_registry_error(
                "inspect remote recovery project session owner",
                "project runtime owner map lock is poisoned".to_owned(),
            )
        })?;
        let Some(ProjectRuntimeOwnerStateV1::Ready(owners)) = entries.get(&project_id) else {
            return Err(session_registry_error(
                "ensure remote recovery project session target",
                "project session owner is unavailable outside a terminal replacement".to_owned(),
            ));
        };
        let owner = owners.sessions.as_ref().ok_or_else(|| {
            session_registry_error(
                "ensure remote recovery project session target",
                "project runtime has no session owner".to_owned(),
            )
        })?;
        let lease = self.issue_session_lease(owner, &project_id)?;
        drop(entries);
        if lease.db_path() != expected_destination {
            return Err(session_registry_error(
                "ensure remote recovery project session target",
                "project session owner path differs from recovery destination".to_owned(),
            ));
        }
        let observed = tracedecay_runtime_core::db::sqlite_generation_identity(
            expected_destination,
        )
        .map_err(|error| {
            session_registry_error(
                "ensure remote recovery project session target",
                format!("{error:?}"),
            )
        })?;
        if observed != expected_opened_file_identity {
            return Err(session_registry_error(
                "ensure remote recovery project session target",
                "project session identity differs from recovery destination".to_owned(),
            ));
        }
        Ok(())
    }

    pub(super) async fn resume_quarantined_restore_while_admitted(
        &self,
        project_id: ProjectId,
        destination: &Path,
        _rollback: &Path,
    ) -> Result<Option<RestorePublicationV1>> {
        let Some(quarantine) = read_remote_restore_quarantine(destination)? else {
            return Ok(None);
        };
        let outcome = recover_remote_restore_quarantine_outcome(destination, &quarantine)?;
        if self
            .ensure_project_sessions_target_while_admitted(
                project_id.clone(),
                quarantine.expected_identity(outcome),
                destination,
            )
            .await
            .is_err()
        {
            let (vacancy, candidate) =
                self.resume_terminal_vacancy(&project_id, quarantine.terminal_vacancy().clone())?;
            let candidate = match candidate {
                Some(candidate) => candidate,
                None => {
                    self.open_candidate_owner(
                        project_id.clone(),
                        quarantine.expected_identity(outcome),
                    )
                    .await?
                }
            };
            let activation = vacancy.begin_candidate_activation(candidate)?;
            self.activate_candidate(&project_id, activation, destination, outcome)
                .await?;
        } else if !quarantine.is_activated() {
            // A pre-cutover crash could publish the map owner before this
            // journal transition. The exact Ready owner proves activation
            // already completed, so durably finish that half without opening
            // a second candidate.
            activate_remote_restore_quarantine(destination, outcome)?;
        }
        Ok(Some(outcome))
    }

    pub(super) async fn resume_retained_rollback(
        &self,
        _project_id: ProjectId,
        _destination: &Path,
        rollback: &Path,
        destination_matches_restore: bool,
    ) -> Result<bool> {
        if destination_matches_restore || !rollback.exists() {
            return Ok(false);
        }
        Err(session_registry_error(
            "resume retained remote restore rollback",
            "retained rollback has no terminal quarantine authority".to_owned(),
        ))
    }

    pub(super) async fn rollback_published_restore(
        &self,
        project_id: &ProjectId,
        destination: &Path,
        rollback: &Path,
        expected_published_identity: u64,
        expected_rollback_identity: u64,
    ) -> Result<()> {
        let recovery = self
            .project_owners
            .reserve_session_recovery(project_id)?
            .ok_or_else(|| {
                session_registry_error(
                    "rollback published remote restore",
                    "project session recovery state is unavailable".to_owned(),
                )
            })?;
        let vacancy = if recovery.has_candidate() {
            let replacement = recovery.into_candidate_replacement()?;
            self.retire_replacement_to_vacancy(project_id, destination, replacement)
                .await?
        } else {
            let (vacancy, candidate) = recovery.into_terminal_vacancy()?;
            if let Some(candidate) = candidate {
                vacancy.retain_candidate_for_recovery(candidate)?;
                return Err(session_registry_error(
                    "rollback published remote restore",
                    "recovery candidate changed while selecting rollback ownership".to_owned(),
                ));
            }
            vacancy
        };
        let target = super::super::super::registry::DestructiveMaintenanceTarget::new(
            destination.parent().ok_or_else(|| {
                session_registry_error(
                    "rollback published remote restore",
                    "restore destination has no parent directory".to_owned(),
                )
            })?,
            [destination.to_path_buf()],
        )
        .map_err(|error| {
            session_registry_error("rollback published remote restore", format!("{error:?}"))
        })?;
        let reservation = self
            .registry
            .begin_destructive_maintenance(target)
            .await
            .map_err(|error| {
                session_registry_error(
                    "reserve published remote restore rollback",
                    format!("{error:?}"),
                )
            })?;
        let rejected = destination.with_extension(format!(
            "remote-restore-rejected-{expected_published_identity:016x}.sqlite3"
        ));
        crate::db::DatabaseAuthority::replace_sqlite_with_rollback_atomically(
            rollback,
            destination,
            &rejected,
            expected_published_identity,
            expected_rollback_identity,
        )
        .map_err(|error| {
            session_registry_error("rollback published remote restore", error.to_string())
        })?;
        PrivateStoreIo::sync_sqlite_family(destination).map_err(|error| {
            session_registry_error("sync rolled back remote restore", error.to_string())
        })?;
        reservation.finish_deleted().map_err(|error| {
            session_registry_error(
                "release published remote restore rollback",
                format!("{error:?}"),
            )
        })?;
        complete_remote_restore_quarantine(destination, RestorePublicationV1::RolledBack)?;
        let candidate = self
            .open_candidate_owner(project_id.clone(), expected_rollback_identity)
            .await?;
        let activation = vacancy.begin_candidate_activation(candidate)?;
        self.activate_candidate(
            project_id,
            activation,
            destination,
            RestorePublicationV1::RolledBack,
        )
        .await
    }

    pub(super) async fn publish_restore(
        &self,
        project_id: ProjectId,
        staging: PathBuf,
        rollback: PathBuf,
        expected_binding: StoreRuntimeBindingV1,
        expected_staging_identity: u64,
        interruption: Arc<AtomicU8>,
    ) -> Result<RestorePublicationV1> {
        if interruption_value(&interruption).is_some() {
            return Ok(RestorePublicationV1::RolledBack);
        }
        let destination = {
            let entries = self.project_owners.lock().map_err(|_| {
                session_registry_error(
                    "inspect remote recovery publication owner",
                    "project runtime owner map lock is poisoned".to_owned(),
                )
            })?;
            let Some(ProjectRuntimeOwnerStateV1::Ready(owners)) = entries.get(&project_id) else {
                return Ok(RestorePublicationV1::RolledBack);
            };
            let Some(owner) = owners.sessions.as_ref() else {
                return Ok(RestorePublicationV1::RolledBack);
            };
            if owner.database.registered_binding() != &expected_binding {
                return Ok(RestorePublicationV1::RolledBack);
            }
            let lease = self.issue_session_lease(owner, &project_id)?;
            lease.db_path().to_path_buf()
        };
        let old_identity = tracedecay_runtime_core::db::sqlite_generation_identity(&destination)
            .map_err(|error| {
                session_registry_error("inspect remote restore source", format!("{error:?}"))
            })?;
        let vacancy = self
            .reserve_project_session_vacancy(&project_id, &destination)
            .await?;
        let terminal_vacancy = vacancy.durable_terminal_authority()?;
        // The exact paired Graph/Store close receipts have crossed their
        // irreversible boundary. Persist the terminal vacancy before the
        // maintenance reservation can await so restart recovery never has to
        // infer or remount that closed owner.
        install_remote_restore_quarantine(
            &destination,
            &staging,
            &rollback,
            old_identity,
            expected_staging_identity,
            terminal_vacancy,
        )?;
        let destructive = super::super::super::registry::DestructiveMaintenanceTarget::new(
            destination.parent().ok_or_else(|| {
                session_registry_error(
                    "publish remote restore",
                    "restore destination has no parent directory".to_owned(),
                )
            })?,
            [destination.clone()],
        )
        .map_err(|error| session_registry_error("publish remote restore", format!("{error:?}")))?;
        let reservation = self
            .registry
            .begin_destructive_maintenance(destructive)
            .await
            .map_err(|error| {
                session_registry_error("reserve remote restore file swap", format!("{error:?}"))
            })?;
        if interruption_value(&interruption).is_some() {
            return Err(session_registry_error(
                "publish remote restore",
                "remote restore was interrupted after native retirement".to_owned(),
            ));
        }
        crate::db::DatabaseAuthority::replace_sqlite_with_rollback_atomically(
            &staging,
            &destination,
            &rollback,
            old_identity,
            expected_staging_identity,
        )
        .map_err(|error| session_registry_error("publish remote restore", error.to_string()))?;
        PrivateStoreIo::sync_sqlite_family(&destination).map_err(|error| {
            session_registry_error("sync published remote restore", error.to_string())
        })?;
        reservation.finish_deleted().map_err(|error| {
            session_registry_error("release published remote restore", format!("{error:?}"))
        })?;
        complete_remote_restore_quarantine(&destination, RestorePublicationV1::Published)?;
        let candidate = self
            .open_candidate_owner(project_id.clone(), expected_staging_identity)
            .await?;
        let activation = vacancy.begin_candidate_activation(candidate)?;
        self.activate_candidate(
            &project_id,
            activation,
            &destination,
            RestorePublicationV1::Published,
        )
        .await?;
        Ok(RestorePublicationV1::Published)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tracedecay_domain::ProjectId;
    use tracedecay_graph_db::NeverCancelled;
    use tracedecay_runtime_core::store_runtime::registry::StoreRuntimeRetirementResult;

    use super::*;
    use crate::daemon::store_runtime::session_registry::{
        DaemonSessionRuntimeRegistryV1, ProjectRuntimeOwnerStateV1,
        ProjectSessionNativeRetirementV1, ProjectSessionRecoveryPhaseV1,
    };

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn recovered_candidate_pre_native_retirement_retry_stays_recovery_required() {
        let temporary = tempfile::tempdir().expect("temporary project parent");
        let root = temporary
            .path()
            .canonicalize()
            .expect("canonical fixture root");
        let profile_root = root.join("profile");
        let project_root = root.join("project");
        std::fs::create_dir_all(&project_root).expect("project root");
        let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
            .expect("durable profile identity");
        let _database_scope = crate::db::enter_daemon_database_scope(
            &profile_root,
            1,
            "recovered candidate retirement retry",
        )
        .expect("daemon database scope");
        let project_id =
            ProjectId::new("project.recovered-candidate-retry").expect("typed project identity");
        crate::storage::pin_fixture_repository_identity(&project_root, project_id.as_str())
            .expect("project enrollment");

        let registry = DaemonSessionRuntimeRegistryV1::open(identity.clone())
            .await
            .expect("session runtime registry");
        let initial = registry
            .project_sessions(project_id.clone(), [project_root])
            .await
            .expect("registered project sessions");
        let destination = initial.db_path().to_path_buf();
        drop(initial);

        let mut replacement = registry
            .reserve_project_session_replacement(&project_id)
            .expect("reserve prior session owner")
            .expect("mounted session owner");
        let graph_target = replacement
            .graph_retirement_target()
            .expect("prior graph target");
        let mut graph = registry
            .graph_registry
            .reserve_retirement_batch(vec![graph_target])
            .expect("reserve prior graph");
        let store_target = replacement
            .reserve_store_target()
            .expect("reserve prior Store target");
        let mut store = match registry
            .registry
            .reserve_retirement_batch(vec![store_target])
        {
            StoreRuntimeRetirementResult::Reserved(reservation) => reservation,
            StoreRuntimeRetirementResult::Blocked(refusal) => {
                panic!("prior Store target unexpectedly blocked: {refusal:?}")
            }
        };
        let graph_commit = graph
            .commit(
                Arc::new(NeverCancelled),
                std::time::Instant::now() + std::time::Duration::from_secs(30),
            )
            .expect("close prior graph");
        let store_commit = store.commit().expect("close prior Store runtime");
        let vacancy = replacement
            .into_vacancy(graph_commit, store_commit)
            .expect("prior paired close proof");

        let expected_identity =
            tracedecay_runtime_core::db::sqlite_generation_identity(&destination)
                .expect("prior database identity");
        let profile_pin = registry
            .profile_pin
            .lock()
            .await
            .as_ref()
            .cloned()
            .expect("profile pin");
        let publication = RemoteRecoveryPublicationContextV1::new(
            registry.identity.clone(),
            registry.incarnation,
            registry.resolver.clone(),
            registry.registry.clone(),
            registry.graph_registry.clone(),
            Arc::clone(&registry.graph_lifecycle_cancelled),
            profile_pin,
            registry.project_owners.clone(),
            Arc::clone(&registry.remote_replay_transaction),
            Arc::clone(&registry.session_sync_service),
            Arc::clone(&registry.remote_recovery_project_lifecycle),
        );
        let candidate = publication
            .open_candidate_owner(project_id.clone(), expected_identity)
            .await
            .expect("open recovered candidate");
        let expected_binding = candidate.database.registered_binding().clone();
        // Dropping this guard models cancellation while activation is waiting
        // on replay/session-sync. The candidate is already map-owned, so its
        // exact graph/Store owner remains retryable instead of being dropped
        // after a downstream service obtains a counted lease.
        let activation = vacancy
            .begin_candidate_activation(candidate)
            .expect("install candidate activation guard");
        let (activation_lease, _, _, _) = activation
            .issue_lease_with_replay_descriptor()
            .expect("issue candidate activation lease");
        drop(activation_lease);
        drop(activation);

        for retry in 0..2 {
            let recovery = registry
                .project_owners
                .reserve_session_recovery(&project_id)
                .expect("reserve terminal recovery")
                .expect("terminal recovery state");
            let replacement = recovery
                .into_candidate_replacement()
                .expect("candidate remains retryable before native retirement");
            let graph_target = replacement
                .graph_retirement_target()
                .expect("candidate graph target");
            let graph = registry
                .graph_registry
                .reserve_retirement_batch(vec![graph_target])
                .expect("reserve candidate graph retirement");
            let mut replacement = replacement;
            let store_target = replacement
                .reserve_store_target()
                .expect("reserve candidate Store target");
            let store = match registry
                .registry
                .reserve_retirement_batch(vec![store_target])
            {
                StoreRuntimeRetirementResult::Reserved(reservation) => reservation,
                StoreRuntimeRetirementResult::Blocked(refusal) => {
                    panic!("candidate Store target unexpectedly blocked: {refusal:?}")
                }
            };
            // Dropping the paired guard is the cancellation path before the
            // graph native boundary. It must restore this same candidate to
            // RecoveryRequired so the next iteration can reserve it again.
            drop(ProjectSessionNativeRetirementV1::new(
                replacement,
                graph,
                store,
            ));

            let entries = registry.project_owners.lock().expect("project owner map");
            let Some(ProjectRuntimeOwnerStateV1::RecoveryRequired(recovery)) =
                entries.get(&project_id)
            else {
                panic!("retry {retry} reopened or lost the recovered candidate");
            };
            assert!(
                recovery.sessions.is_none(),
                "retry {retry} retained an old owner"
            );
            let candidate = recovery
                .candidate_sessions
                .as_ref()
                .expect("retry must retain the same unpublished candidate");
            assert_eq!(candidate.database.registered_binding(), &expected_binding);
            assert!(matches!(
                &recovery.phase,
                ProjectSessionRecoveryPhaseV1::Terminal(proof) if proof.verify()
            ));
        }
    }
}
