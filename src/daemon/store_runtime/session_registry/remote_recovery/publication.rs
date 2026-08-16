use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU8;

use tracedecay_domain::ProjectId;
use tracedecay_global_db::session_temporal::relations::SessionRelationScope;
use tracedecay_runtime_core::storage::PrivateStoreIo;
use tracedecay_store::{StoreRuntimeBindingV1, StoreShardIdV1};

use super::super::open_runtime_during_remote_restore;
use super::artifacts::{replay_current_authority_state, validate_isolated_restore};
use super::{
    DatabaseAuthority, DestructiveMaintenanceTarget, RegisteredGlobalDb, RegisteredGlobalDbLeaseV1,
    RemoteRecoveryPublicationContextV1, Result, interruption_value, registry_open_error,
    session_registry_error,
};

mod mounted_identity;
mod quarantine;
mod sqlite_family;
mod unpublished_cleanup;

use mounted_identity::validate_existing_mounted_identity;
use quarantine::RemoteRestoreQuarantineV1;
pub(super) use quarantine::mark_remote_restore_rollback_required;
pub(in crate::daemon::store_runtime::session_registry) use quarantine::remote_restore_activated_open_identity;
use quarantine::{
    PrepublicationRestoreV1, RetiredMountedRestoreTargetV1, activated_remote_restore,
    completed_remote_restore, read_remote_restore_quarantine, rollback_required,
    validate_completed_remote_restore,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RestorePublicationV1 {
    Published,
    RolledBack,
}

fn sqlite_sidecar(path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}-{suffix}", path.display()))
}

pub(super) fn quarantine_sqlite_sidecars(destination: &Path, quarantine: &Path) -> Result<()> {
    for suffix in ["wal", "shm"] {
        let source = sqlite_sidecar(destination, suffix);
        if !source.try_exists().map_err(|error| {
            session_registry_error("inspect remote restore SQLite sidecar", error.to_string())
        })? {
            continue;
        }
        let retained = sqlite_sidecar(quarantine, suffix);
        if retained.try_exists().map_err(|error| {
            session_registry_error("inspect quarantined SQLite sidecar", error.to_string())
        })? {
            return Err(session_registry_error(
                "quarantine remote restore SQLite sidecar",
                format!("quarantine '{}' already exists", retained.display()),
            ));
        }
        DatabaseAuthority::replace_file_atomically(
            &source,
            &retained,
            "unverified remote restore SQLite sidecar",
        )
        .map_err(|error| {
            session_registry_error(
                "quarantine remote restore SQLite sidecar",
                format!("{error:?}"),
            )
        })?;
    }
    PrivateStoreIo::sync_sqlite_family(quarantine).map_err(|error| {
        session_registry_error(
            "sync quarantined remote restore SQLite family",
            error.to_string(),
        )
    })
}

fn retain_interrupted_rollback(
    quarantine: &RemoteRestoreQuarantineV1,
    rollback: &Path,
) -> Result<()> {
    if rollback.exists() {
        return Ok(());
    }
    let staging_identity = tracedecay_runtime_core::db::sqlite_generation_identity(
        &quarantine.staging,
    )
    .map_err(|error| {
        session_registry_error(
            "verify interrupted remote restore rollback",
            format!("{error:?}"),
        )
    })?;
    if staging_identity != quarantine.expected_rollback_identity {
        return Err(session_registry_error(
            "verify interrupted remote restore rollback",
            "staging does not retain the exact prior authority".to_owned(),
        ));
    }
    DatabaseAuthority::replace_file_atomically(
        &quarantine.staging,
        rollback,
        "interrupted remote restore rollback",
    )
    .map_err(|error| {
        session_registry_error(
            "retain interrupted remote restore rollback",
            format!("{error:?}"),
        )
    })?;
    PrivateStoreIo::sync_sqlite_family(rollback).map_err(|error| {
        session_registry_error(
            "sync interrupted remote restore rollback",
            error.to_string(),
        )
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FailedPublicationDispositionV1 {
    RemountRolledBack,
    FinishPublished,
    RestoreRetainedRollback(Option<u64>),
}

fn failed_publication_disposition(
    observed_identity: Option<u64>,
    expected_rollback_identity: u64,
    expected_published_identity: u64,
) -> FailedPublicationDispositionV1 {
    match observed_identity {
        Some(identity) if identity == expected_rollback_identity => {
            FailedPublicationDispositionV1::RemountRolledBack
        }
        Some(identity) if identity == expected_published_identity => {
            FailedPublicationDispositionV1::FinishPublished
        }
        identity => FailedPublicationDispositionV1::RestoreRetainedRollback(identity),
    }
}

pub(super) fn restore_retained_rollback_over_unverified_destination(
    destination: &Path,
    rollback: &Path,
    observed_destination_identity: Option<u64>,
    expected_rollback_identity: Option<u64>,
) -> Result<u64> {
    let quarantine = rollback.with_extension("unverified.sqlite3");
    if let Some(observed_identity) = observed_destination_identity {
        if quarantine.try_exists().map_err(|error| {
            session_registry_error(
                "inspect unverified remote restore quarantine",
                error.to_string(),
            )
        })? {
            return Err(session_registry_error(
                "quarantine unverified remote restore",
                format!("quarantine '{}' already exists", quarantine.display()),
            ));
        }
        let current_identity = tracedecay_runtime_core::db::sqlite_generation_identity(destination)
            .map_err(|error| {
                session_registry_error(
                    "verify unverified remote restore destination",
                    format!("{error:?}"),
                )
            })?;
        if current_identity != observed_identity {
            return Err(session_registry_error(
                "verify unverified remote restore destination",
                format!(
                    "destination identity changed from {observed_identity} to {current_identity}"
                ),
            ));
        }
        DatabaseAuthority::replace_file_atomically(
            destination,
            &quarantine,
            "unverified remote restore quarantine",
        )
        .map_err(|error| {
            session_registry_error("quarantine unverified remote restore", format!("{error:?}"))
        })?;
        PrivateStoreIo::sync_sqlite_family(&quarantine).map_err(|error| {
            session_registry_error(
                "sync unverified remote restore quarantine",
                error.to_string(),
            )
        })?;
    } else if destination.try_exists().map_err(|error| {
        session_registry_error(
            "inspect missing remote restore destination",
            error.to_string(),
        )
    })? {
        return Err(session_registry_error(
            "restore retained remote restore rollback",
            "destination appeared after its missing identity was observed".to_owned(),
        ));
    }
    quarantine_sqlite_sidecars(destination, &quarantine)?;

    let rollback_identity = tracedecay_runtime_core::db::sqlite_generation_identity(rollback)
        .map_err(|error| {
            session_registry_error(
                "verify retained remote restore rollback",
                format!("{error:?}"),
            )
        })?;
    if let Some(expected_rollback_identity) = expected_rollback_identity {
        if expected_rollback_identity != rollback_identity {
            return Err(session_registry_error(
                "verify retained remote restore rollback",
                format!(
                    "rollback identity {rollback_identity} does not match retained identity {expected_rollback_identity}"
                ),
            ));
        }
    }

    DatabaseAuthority::replace_file_atomically(
        rollback,
        destination,
        "retained remote restore rollback",
    )
    .map_err(|error| {
        session_registry_error(
            "restore retained remote restore rollback",
            format!("{error:?}"),
        )
    })?;
    PrivateStoreIo::sync_sqlite_family(destination).map_err(|error| {
        session_registry_error("sync retained remote restore rollback", error.to_string())
    })?;
    let restored_identity = tracedecay_runtime_core::db::sqlite_generation_identity(destination)
        .map_err(|error| {
            session_registry_error(
                "verify restored remote restore rollback",
                format!("{error:?}"),
            )
        })?;
    if restored_identity != rollback_identity {
        return Err(session_registry_error(
            "verify restored remote restore rollback",
            format!(
                "restored identity {restored_identity} does not match retained rollback {rollback_identity}"
            ),
        ));
    }
    Ok(restored_identity)
}

impl RemoteRecoveryPublicationContextV1 {
    // The caller holds project recovery admission across this whole crash
    // convergence path, including any physical replacement and publication.
    pub(super) async fn resume_quarantined_restore_while_admitted(
        &self,
        project_id: ProjectId,
        destination: &Path,
        rollback: &Path,
    ) -> Result<Option<RestorePublicationV1>> {
        let Some(quarantine) = read_remote_restore_quarantine(destination)? else {
            return Ok(None);
        };
        let completed = completed_remote_restore(&quarantine);
        if quarantine.rollback != rollback && completed.is_some() {
            return Ok(None);
        }
        if quarantine.rollback != rollback {
            return Err(session_registry_error(
                "resume remote restore quarantine",
                "quarantine rollback path does not match this restore".to_owned(),
            ));
        }
        let activated_outcome = activated_remote_restore(&quarantine);
        let initial_mounted = Arc::clone(&self.project_sessions).lock_owned().await;
        if let (Some(database), Some(outcome)) =
            (initial_mounted.get(&project_id), activated_outcome)
        {
            let expected_identity = match outcome {
                RestorePublicationV1::Published => quarantine.expected_published_identity,
                RestorePublicationV1::RolledBack => quarantine.expected_rollback_identity,
            };
            let expected_shard = StoreShardIdV1::project_sessions(
                self.identity.brain_id().clone(),
                self.identity.profile_id().clone(),
                project_id.clone(),
            );
            if validate_existing_mounted_identity(
                database,
                &expected_shard,
                self.incarnation,
                expected_identity,
                destination,
            )
            .is_ok()
            {
                return Ok(Some(outcome));
            }
        }
        drop(initial_mounted);
        let RetiredMountedRestoreTargetV1 {
            mut mounted,
            reservation,
            preserved_identity: _,
            _quiescence,
        } = self
            .retire_mounted_target_before_replacement(&project_id, destination)
            .await?;
        let mut reservation = reservation;
        let already_activated = activated_outcome.is_some();
        let outcome = if let Some(outcome) = completed {
            validate_completed_remote_restore(destination, &quarantine, outcome)?;
            outcome
        } else {
            let destination_identity =
                tracedecay_runtime_core::db::sqlite_generation_identity(destination).ok();
            if rollback_required(&quarantine)
                && destination_identity == Some(quarantine.expected_published_identity)
            {
                retain_interrupted_rollback(&quarantine, rollback)?;
                if let Some(reservation) = reservation.take() {
                    reservation.finish_deleted().map_err(|error| {
                        session_registry_error(
                            "release quarantined rollback convergence",
                            format!("{error:?}"),
                        )
                    })?;
                }
                drop(mounted);
                self.rollback_published_restore(
                    &project_id,
                    destination,
                    rollback,
                    quarantine.expected_published_identity,
                    quarantine.expected_rollback_identity,
                )
                .await?;
                mounted = Arc::clone(&self.project_sessions).lock_owned().await;
                validate_isolated_restore(destination).map_err(|error| {
                    session_registry_error(
                        "validate resumed rejected remote restore rollback",
                        format!("{error:?}"),
                    )
                })?;
                RestorePublicationV1::RolledBack
            } else {
                match destination_identity {
                    Some(identity) if identity == quarantine.expected_rollback_identity => {
                        if rollback.exists() {
                            let interrupted_identity =
                                tracedecay_runtime_core::db::sqlite_generation_identity(rollback)
                                    .map_err(|error| {
                                    session_registry_error(
                                        "verify interrupted published restore",
                                        format!("{error:?}"),
                                    )
                                })?;
                            if interrupted_identity != quarantine.expected_published_identity {
                                return Err(session_registry_error(
                                    "verify interrupted published restore",
                                    "rollback path does not retain the rejected publication"
                                        .to_owned(),
                                ));
                            }
                            let retained_new = destination.with_extension(format!(
                                "remote-restore-rejected-{:016x}.sqlite3",
                                quarantine.expected_published_identity
                            ));
                            if retained_new.try_exists().map_err(|error| {
                                session_registry_error(
                                    "inspect interrupted rejected remote restore",
                                    error.to_string(),
                                )
                            })? {
                                return Err(session_registry_error(
                                    "retain interrupted rejected remote restore",
                                    "retained rejected destination already exists".to_owned(),
                                ));
                            }
                            DatabaseAuthority::replace_file_atomically(
                                rollback,
                                &retained_new,
                                "interrupted rejected remote restore",
                            )
                            .map_err(|error| {
                                session_registry_error(
                                    "retain interrupted rejected remote restore",
                                    format!("{error:?}"),
                                )
                            })?;
                        }
                        quarantine_sqlite_sidecars(
                            destination,
                            &rollback.with_extension("unverified.sqlite3"),
                        )?;
                        validate_isolated_restore(destination).map_err(|error| {
                            session_registry_error(
                                "validate quarantined remote restore rollback",
                                format!("{error:?}"),
                            )
                        })?;
                        RestorePublicationV1::RolledBack
                    }
                    Some(identity) if identity == quarantine.expected_published_identity => {
                        retain_interrupted_rollback(&quarantine, rollback)?;
                        let rollback_identity =
                            tracedecay_runtime_core::db::sqlite_generation_identity(rollback)
                                .map_err(|error| {
                                    session_registry_error(
                                        "verify quarantined restore rollback",
                                        format!("{error:?}"),
                                    )
                                })?;
                        if rollback_identity != quarantine.expected_rollback_identity {
                            return Err(session_registry_error(
                                "verify quarantined restore rollback",
                                format!(
                                    "rollback identity {rollback_identity} does not match retained identity {}",
                                    quarantine.expected_rollback_identity
                                ),
                            ));
                        }
                        quarantine_sqlite_sidecars(
                            destination,
                            &rollback.with_extension("unverified.sqlite3"),
                        )?;
                        validate_isolated_restore(destination).map_err(|error| {
                            session_registry_error(
                                "validate quarantined published restore",
                                format!("{error:?}"),
                            )
                        })?;
                        validate_isolated_restore(rollback).map_err(|error| {
                            session_registry_error(
                                "validate quarantined restore rollback",
                                format!("{error:?}"),
                            )
                        })?;
                        PrivateStoreIo::sync_sqlite_family(destination).map_err(|error| {
                            session_registry_error(
                                "sync quarantined published restore",
                                error.to_string(),
                            )
                        })?;
                        PrivateStoreIo::sync_sqlite_family(rollback).map_err(|error| {
                            session_registry_error(
                                "sync quarantined restore rollback",
                                error.to_string(),
                            )
                        })?;
                        RestorePublicationV1::Published
                    }
                    observed_identity => {
                        restore_retained_rollback_over_unverified_destination(
                            destination,
                            rollback,
                            observed_identity,
                            Some(quarantine.expected_rollback_identity),
                        )?;
                        validate_isolated_restore(destination).map_err(|error| {
                            session_registry_error(
                                "validate restored quarantined rollback",
                                format!("{error:?}"),
                            )
                        })?;
                        RestorePublicationV1::RolledBack
                    }
                }
            }
        };
        if let Some(reservation) = reservation.take() {
            reservation.finish_deleted().map_err(|error| {
                session_registry_error(
                    "release quarantined remote restore convergence",
                    format!("{error:?}"),
                )
            })?;
        }
        let expected_outcome_identity = match outcome {
            RestorePublicationV1::Published => quarantine.expected_published_identity,
            RestorePublicationV1::RolledBack => quarantine.expected_rollback_identity,
        };
        let (database, outcome) = match self
            .mount_project_sessions(project_id.clone(), expected_outcome_identity)
            .await
        {
            Ok(database) => (database, outcome),
            Err(publication_error)
                if outcome == RestorePublicationV1::Published && !already_activated =>
            {
                mark_remote_restore_rollback_required(
                    destination,
                    rollback,
                    quarantine.expected_rollback_identity,
                    quarantine.expected_published_identity,
                )?;
                if let Some(reservation) = reservation.take() {
                    reservation.finish_deleted().map_err(|error| {
                        session_registry_error(
                            "release rejected restore convergence",
                            format!("{error:?}"),
                        )
                    })?;
                }
                drop(mounted);
                self.rollback_published_restore(
                    &project_id,
                    destination,
                    rollback,
                    quarantine.expected_published_identity,
                    quarantine.expected_rollback_identity,
                )
                .await
                .map_err(|rollback_error| {
                    session_registry_error(
                        "resume rejected remote restore rollback",
                        format!(
                            "validation={publication_error}; rollback failed: {rollback_error}"
                        ),
                    )
                })?;
                mounted = Arc::clone(&self.project_sessions).lock_owned().await;
                let database = self
                    .mount_project_sessions(
                        project_id.clone(),
                        quarantine.expected_rollback_identity,
                    )
                    .await
                    .map_err(|remount| {
                        session_registry_error(
                            "resume rejected remote restore rollback",
                            format!("validation={publication_error}; remount failed: {remount}"),
                        )
                    })?;
                (database, RestorePublicationV1::RolledBack)
            }
            Err(error) => return Err(error),
        };
        if already_activated {
            self.publish_mounted(&mut mounted, project_id, database)
                .await?;
        } else {
            self.publish_quarantined_mounted(
                &mut mounted,
                project_id,
                database,
                destination,
                outcome,
            )
            .await?;
        }
        Ok(Some(outcome))
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
        let database = {
            let mounted = Arc::clone(&self.project_sessions).lock_owned().await;
            mounted.get(&project_id).cloned()
        };
        let Some(database) = database else {
            return Ok(RestorePublicationV1::RolledBack);
        };
        if database.binding() != &expected_binding {
            return Ok(RestorePublicationV1::RolledBack);
        }
        let expected_database_identity =
            database.runtime().opened_file_identity().ok_or_else(|| {
                session_registry_error(
                    "publish remote restore",
                    "opened ProjectSessions identity is unavailable".to_owned(),
                )
            })?;
        let (graph_binding, graph_verified_locator) = database
            .session_relation_graph_identity()
            .map(|(binding, locator)| (binding.clone(), locator.clone()))?;
        let destination = database.db_path().to_path_buf();
        let Some(root) = destination.parent() else {
            return Ok(RestorePublicationV1::RolledBack);
        };
        let target = match DestructiveMaintenanceTarget::new(root, [destination.clone()]) {
            Ok(target) => target,
            Err(_) => return Ok(RestorePublicationV1::RolledBack),
        };
        let lifecycle = self
            .project_lifecycle()
            .map_err(|error| session_registry_error("quiesce remote restore", error.to_string()))?;
        let _quiescence = match lifecycle.quiesce(&project_id, &database).await {
            Ok(quiescence) => quiescence,
            Err(error) => {
                self.rebind_session_sync(&project_id, &database).await?;
                return Err(error);
            }
        };
        let mut mounted = Arc::clone(&self.project_sessions).lock_owned().await;
        if !mounted
            .get(&project_id)
            .is_some_and(|mounted| mounted.shares_client_with(&database))
        {
            if let Some(current) = mounted.get(&project_id).cloned() {
                self.rebind_session_sync(&project_id, &current).await?;
            }
            return Ok(RestorePublicationV1::RolledBack);
        }
        if self
            .replay
            .unregister_target(&project_id, &expected_binding)
            .is_err()
        {
            self.rebind_session_sync(&project_id, &database).await?;
            return Ok(RestorePublicationV1::RolledBack);
        }
        let mounted_database = mounted.remove(&project_id).ok_or_else(|| {
            session_registry_error(
                "publish remote restore",
                "ProjectSessions target disappeared during recovery".to_owned(),
            )
        })?;
        drop(database);
        drop(mounted_database);
        if let Err(error) = super::super::code_graph::graph_attachment::close_retained(
            &self.graph_registry,
            graph_binding,
            graph_verified_locator,
        )
        .await
        {
            let restored = self
                .mount_project_sessions(project_id.clone(), expected_database_identity)
                .await
                .map_err(|remount| {
                    session_registry_error(
                        "restore project sessions after relation graph retirement refusal",
                        format!("{error}; remount failed: {remount}"),
                    )
                })?;
            self.publish_mounted(&mut mounted, project_id, restored)
                .await?;
            return Err(error);
        }

        let reservation = match self.registry.begin_destructive_maintenance(target).await {
            Ok(reservation) => reservation,
            Err(error) => {
                let restored = self
                    .mount_project_sessions(project_id.clone(), expected_database_identity)
                    .await
                    .map_err(|remount| {
                        session_registry_error(
                            "recover failed remote restore quiesce",
                            format!("{error:?}; remount failed: {remount}"),
                        )
                    })?;
                self.publish_mounted(&mut mounted, project_id, restored)
                    .await?;
                return Ok(RestorePublicationV1::RolledBack);
            }
        };
        let Some(closed) = reservation
            .closed()
            .iter()
            .find(|closed| closed.binding() == &expected_binding)
            .cloned()
        else {
            return self
                .abort_and_remount_restore(
                    &mut mounted,
                    project_id,
                    expected_database_identity,
                    reservation,
                    "release incomplete remote restore quiesce",
                    "destructive reservation omitted the exact ProjectSessions binding".to_owned(),
                )
                .await;
        };
        if let Err(error) = replay_current_authority_state(&destination, &staging) {
            return self
                .abort_and_remount_restore(
                    &mut mounted,
                    project_id,
                    expected_database_identity,
                    reservation,
                    "release rejected remote restore",
                    format!("restaged authority is invalid: {error:?}"),
                )
                .await;
        }
        if interruption_value(&interruption).is_some() {
            return self
                .abort_and_remount_restore(
                    &mut mounted,
                    project_id,
                    expected_database_identity,
                    reservation,
                    "cancel remote restore publication",
                    "remote restore was interrupted before publication".to_owned(),
                )
                .await;
        }

        let reservation = match self
            .prepare_remote_restore_swap(
                &mut mounted,
                project_id.clone(),
                &destination,
                &staging,
                &rollback,
                expected_database_identity,
                expected_staging_identity,
                reservation,
            )
            .await?
        {
            PrepublicationRestoreV1::Ready(reservation) => reservation,
            PrepublicationRestoreV1::RolledBack => {
                return Ok(RestorePublicationV1::RolledBack);
            }
        };

        let publication = DatabaseAuthority::replace_sqlite_with_rollback_atomically(
            &staging,
            &destination,
            &rollback,
            closed.opened_file_identity(),
            expected_staging_identity,
        );
        match publication {
            Ok(()) => {
                self.finish_published_restore(
                    mounted,
                    project_id,
                    &destination,
                    &rollback,
                    reservation,
                    expected_staging_identity,
                    closed.opened_file_identity(),
                )
                .await
            }
            Err(error) => {
                let destination_identity =
                    tracedecay_runtime_core::db::sqlite_generation_identity(&destination).ok();
                match failed_publication_disposition(
                    destination_identity,
                    closed.opened_file_identity(),
                    expected_staging_identity,
                ) {
                    FailedPublicationDispositionV1::RemountRolledBack => {
                        quarantine_sqlite_sidecars(
                            &destination,
                            &rollback.with_extension("unverified.sqlite3"),
                        )?;
                        validate_isolated_restore(&destination).map_err(|validation| {
                            session_registry_error(
                                "validate rolled-back remote restore",
                                format!("publication={error}; validation={validation:?}"),
                            )
                        })?;
                        self.abort_and_remount_quarantined_restore(
                            &mut mounted,
                            project_id,
                            &destination,
                            reservation,
                            "release rolled-back remote restore",
                            error.to_string(),
                        )
                        .await
                    }
                    FailedPublicationDispositionV1::FinishPublished => {
                        tracing::warn!(
                            error = %error,
                            "remote restore reported an atomic publication error after the exact restored file became authoritative"
                        );
                        self.finish_published_restore(
                            mounted,
                            project_id,
                            &destination,
                            &rollback,
                            reservation,
                            expected_staging_identity,
                            closed.opened_file_identity(),
                        )
                        .await
                    }
                    FailedPublicationDispositionV1::RestoreRetainedRollback(observed_identity) => {
                        if let Err(rollback_error) =
                            restore_retained_rollback_over_unverified_destination(
                                &destination,
                                &rollback,
                                observed_identity,
                                Some(closed.opened_file_identity()),
                            )
                        {
                            return Err(session_registry_error(
                                "restore retained rollback after unverified publication",
                                format!(
                                    "publication={error}; destination identity={destination_identity:?}; rollback={rollback_error}"
                                ),
                            ));
                        }
                        validate_isolated_restore(&destination).map_err(|validation| {
                            session_registry_error(
                                "validate recovered remote restore rollback",
                                format!("publication={error}; validation={validation:?}"),
                            )
                        })?;
                        tracing::warn!(
                            error = %error,
                            "quarantined an unverified remote restore destination and restored the retained rollback"
                        );
                        self.abort_and_remount_quarantined_restore(
                            &mut mounted,
                            project_id,
                            &destination,
                            reservation,
                            "release recovered remote restore rollback",
                            error.to_string(),
                        )
                        .await
                    }
                }
            }
        }
    }

    pub(super) async fn rollback_published_restore(
        &self,
        project_id: &ProjectId,
        destination: &Path,
        rollback: &Path,
        expected_published_identity: u64,
        expected_rollback_identity: u64,
    ) -> Result<()> {
        let retained_new = destination.with_extension(format!(
            "remote-restore-rejected-{expected_published_identity:016x}.sqlite3"
        ));
        let RetiredMountedRestoreTargetV1 {
            mounted: _mounted,
            reservation,
            preserved_identity,
            _quiescence,
        } = self
            .retire_mounted_target_before_replacement(project_id, destination)
            .await?;
        if preserved_identity != Some(expected_published_identity) {
            return Err(session_registry_error(
                "quiesce remote restore rollback",
                format!(
                    "published identity {preserved_identity:?} does not match expected {expected_published_identity}"
                ),
            ));
        }
        quarantine_sqlite_sidecars(destination, &retained_new)?;
        sqlite_family::checkpoint_for_publication(rollback)?;
        DatabaseAuthority::replace_sqlite_with_rollback_atomically(
            rollback,
            destination,
            &retained_new,
            expected_published_identity,
            expected_rollback_identity,
        )
        .map_err(|error| session_registry_error("rollback remote restore", error.to_string()))?;
        PrivateStoreIo::sync_sqlite_family(destination).map_err(|error| {
            session_registry_error("sync rolled-back remote restore", error.to_string())
        })?;
        PrivateStoreIo::sync_sqlite_family(&retained_new).map_err(|error| {
            session_registry_error("sync rejected remote restore", error.to_string())
        })?;
        if let Some(reservation) = reservation {
            reservation.finish_deleted().map_err(|error| {
                session_registry_error("release remote restore rollback", format!("{error:?}"))
            })?;
        }
        tracing::info!(
            project_id = %project_id,
            retained_rejected_store = %retained_new.display(),
            "remote restore rolled back after registered validation failed"
        );
        Ok(())
    }

    pub(super) async fn mount_project_sessions(
        &self,
        project_id: ProjectId,
        expected_opened_file_identity: u64,
    ) -> Result<RegisteredGlobalDbLeaseV1> {
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
            "reattach restored project session store",
        )
        .await?;
        let expected_binding = runtime.binding().clone();
        let expected_locator = runtime.locator().verified().clone();
        let authority = runtime
            .database_authority("reattach restored project session store")
            .map_err(|error| {
                registry_open_error("reattach restored project session store", error)
            })?;
        let database = RegisteredGlobalDb::migrate_and_attach(
            runtime,
            expected_binding,
            expected_locator,
            authority,
        )
        .await?;
        let relation_graph = super::super::code_graph::graph_attachment::open_session_relation(
            &self.registry,
            &self.graph_registry,
            &self.graph_lifecycle_cancelled,
            self.incarnation,
            shard_id,
        )
        .await?;
        let (relation_graph, graph_binding, graph_verified_locator) = relation_graph.into_parts();
        database.bind_session_relation_graph(
            SessionRelationScope::project_sessions(project_id),
            relation_graph,
            graph_binding,
            graph_verified_locator,
        )?;
        Ok(database)
    }

    pub(super) async fn publish_mounted(
        &self,
        mounted: &mut BTreeMap<ProjectId, RegisteredGlobalDbLeaseV1>,
        project_id: ProjectId,
        database: RegisteredGlobalDbLeaseV1,
    ) -> Result<()> {
        self.prepare_mounted(&project_id, &database).await?;
        mounted.insert(project_id, database);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
