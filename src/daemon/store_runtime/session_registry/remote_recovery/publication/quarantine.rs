use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::OwnedMutexGuard;
use tracedecay_domain::{ProjectId, canonical_sha256};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_runtime_core::storage::PrivateStoreIo;

use super::super::artifacts::validate_isolated_restore;
use super::super::{
    DatabaseAuthority, DestructiveMaintenanceReservation, DestructiveMaintenanceTarget,
    RemoteRecoveryPublicationContextV1, Result, registry_open_error, session_registry_error,
};
use super::RestorePublicationV1;
use super::mounted_identity::validate_existing_mounted_identity;

const REMOTE_RESTORE_QUARANTINE_VERSION: &str = "tracedecay.remote-restore-quarantine.v1";

pub(super) struct RetiredMountedRestoreTargetV1 {
    pub(super) mounted: OwnedMutexGuard<BTreeMap<ProjectId, RegisteredGlobalDbLeaseV1>>,
    pub(super) reservation: Option<DestructiveMaintenanceReservation>,
    pub(super) preserved_identity: Option<u64>,
    pub(super) _quiescence: Option<
        crate::daemon::branch_admin::remote_recovery_lifecycle::RemoteRecoveryProjectQuiescenceV1,
    >,
}

pub(super) enum PrepublicationRestoreV1 {
    Ready(DestructiveMaintenanceReservation),
    RolledBack,
}

pub(super) fn replacement_target(destination: &Path) -> Result<DestructiveMaintenanceTarget> {
    DestructiveMaintenanceTarget::new(
        destination.parent().ok_or_else(|| {
            session_registry_error(
                "retire remote restore target",
                "restore destination has no parent directory".to_owned(),
            )
        })?,
        [destination.to_path_buf()],
    )
    .map_err(|error| session_registry_error("retire remote restore target", format!("{error:?}")))
}

pub(super) async fn lock_project_sessions_for_replacement(
    project_sessions: &Arc<tokio::sync::Mutex<BTreeMap<ProjectId, RegisteredGlobalDbLeaseV1>>>,
) -> OwnedMutexGuard<BTreeMap<ProjectId, RegisteredGlobalDbLeaseV1>> {
    Arc::clone(project_sessions).lock_owned().await
}

pub(super) fn reject_unbound_retained_rollback(rollback: &Path) -> Result<()> {
    Err(session_registry_error(
        "resume retained remote restore rollback",
        format!(
            "rollback '{}' has no matching durable pre-publication fence",
            rollback.display()
        ),
    ))
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RemoteRestoreQuarantinePhaseV1 {
    Publishing,
    RollbackRequired,
    Published,
    RolledBack,
    ActivatedPublished,
    ActivatedRolledBack,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RemoteRestoreQuarantineV1 {
    pub(super) version: String,
    pub(super) staging: PathBuf,
    pub(super) rollback: PathBuf,
    pub(super) expected_rollback_identity: u64,
    pub(super) expected_published_identity: u64,
    phase: RemoteRestoreQuarantinePhaseV1,
}

fn write_remote_restore_quarantine(
    destination: &Path,
    quarantine: &RemoteRestoreQuarantineV1,
) -> Result<()> {
    let fence = super::super::super::remote_restore_quarantine_fence_path(destination);
    let payload = serde_json::to_vec(quarantine).map_err(|error| {
        session_registry_error("encode remote restore quarantine fence", error.to_string())
    })?;
    let staging = fence.with_extension("quarantine.staging");
    PrivateStoreIo::write_file_atomically_durable(&fence, &staging, &payload).map_err(|error| {
        session_registry_error("write remote restore quarantine fence", error.to_string())
    })
}

fn transition_path(
    destination: &Path,
    quarantine: &RemoteRestoreQuarantineV1,
    transition: &str,
) -> Result<PathBuf> {
    let digest = canonical_sha256(&(
        "tracedecay.remote-restore-quarantine-transition.v1",
        &quarantine.staging,
        &quarantine.rollback,
        quarantine.expected_rollback_identity,
        quarantine.expected_published_identity,
    ))
    .map_err(|error| {
        session_registry_error(
            "derive remote restore quarantine transition",
            error.to_string(),
        )
    })?;
    let suffix = digest.as_str().strip_prefix("sha256:").ok_or_else(|| {
        session_registry_error(
            "derive remote restore quarantine transition",
            "canonical transition digest is not SHA-256".to_owned(),
        )
    })?;
    Ok(
        super::super::super::remote_restore_quarantine_fence_path(destination)
            .with_extension(format!("{suffix}.{transition}.json")),
    )
}

fn same_operation(left: &RemoteRestoreQuarantineV1, right: &RemoteRestoreQuarantineV1) -> bool {
    left.version == right.version
        && left.staging == right.staging
        && left.rollback == right.rollback
        && left.expected_rollback_identity == right.expected_rollback_identity
        && left.expected_published_identity == right.expected_published_identity
}

fn read_transition(
    destination: &Path,
    quarantine: &RemoteRestoreQuarantineV1,
    transition_kind: &str,
) -> Result<Option<RemoteRestoreQuarantineV1>> {
    let path = transition_path(destination, quarantine, transition_kind)?;
    let Some(payload) =
        DatabaseAuthority::read_record_strict(&path, "remote restore quarantine transition")
            .map_err(|error| {
                session_registry_error(
                    "read remote restore quarantine transition",
                    format!("{error:?}"),
                )
            })?
    else {
        return Ok(None);
    };
    let transition: RemoteRestoreQuarantineV1 =
        serde_json::from_str(&payload).map_err(|error| {
            session_registry_error(
                "decode remote restore quarantine transition",
                error.to_string(),
            )
        })?;
    if !same_operation(quarantine, &transition) {
        return Err(session_registry_error(
            "validate remote restore quarantine transition",
            "transition does not match its immutable restore fence".to_owned(),
        ));
    }
    let valid_phase = match transition_kind {
        "activated" => matches!(
            transition.phase,
            RemoteRestoreQuarantinePhaseV1::ActivatedPublished
                | RemoteRestoreQuarantinePhaseV1::ActivatedRolledBack
        ),
        "terminal" => matches!(
            transition.phase,
            RemoteRestoreQuarantinePhaseV1::Published | RemoteRestoreQuarantinePhaseV1::RolledBack
        ),
        "rollback-required" => transition.phase == RemoteRestoreQuarantinePhaseV1::RollbackRequired,
        _ => false,
    };
    if !valid_phase {
        return Err(session_registry_error(
            "validate remote restore quarantine transition",
            "transition records an invalid phase".to_owned(),
        ));
    }
    Ok(Some(transition))
}

fn write_transition(
    destination: &Path,
    quarantine: &RemoteRestoreQuarantineV1,
    transition: &str,
) -> Result<()> {
    if let Some(existing) = read_transition(destination, quarantine, transition)? {
        return if existing == *quarantine {
            Ok(())
        } else {
            Err(session_registry_error(
                "write remote restore quarantine transition",
                "transition already records a different terminal phase".to_owned(),
            ))
        };
    }
    let path = transition_path(destination, quarantine, transition)?;
    let payload = serde_json::to_vec(quarantine).map_err(|error| {
        session_registry_error(
            "encode remote restore quarantine transition",
            error.to_string(),
        )
    })?;
    let staging = path.with_extension("transition.staging");
    PrivateStoreIo::write_file_atomically_durable(&path, &staging, &payload).map_err(|error| {
        session_registry_error(
            "write remote restore quarantine transition",
            error.to_string(),
        )
    })
}

pub(super) fn read_remote_restore_quarantine(
    destination: &Path,
) -> Result<Option<RemoteRestoreQuarantineV1>> {
    let fence = super::super::super::remote_restore_quarantine_fence_path(destination);
    let Some(payload) =
        DatabaseAuthority::read_record_strict(&fence, "remote restore quarantine fence").map_err(
            |error| {
                session_registry_error("read remote restore quarantine fence", format!("{error:?}"))
            },
        )?
    else {
        return Ok(None);
    };
    let quarantine: RemoteRestoreQuarantineV1 =
        serde_json::from_str(&payload).map_err(|error| {
            session_registry_error("decode remote restore quarantine fence", error.to_string())
        })?;
    if quarantine.version != REMOTE_RESTORE_QUARANTINE_VERSION {
        return Err(session_registry_error(
            "decode remote restore quarantine fence",
            format!("unsupported version '{}'", quarantine.version),
        ));
    }
    if quarantine.phase != RemoteRestoreQuarantinePhaseV1::Publishing {
        return Err(session_registry_error(
            "decode remote restore quarantine fence",
            "immutable restore fence records a non-publishing phase".to_owned(),
        ));
    }
    if let Some(activated) = read_transition(destination, &quarantine, "activated")? {
        return Ok(Some(activated));
    }
    if let Some(terminal) = read_transition(destination, &quarantine, "terminal")? {
        return Ok(Some(terminal));
    }
    if let Some(rollback) = read_transition(destination, &quarantine, "rollback-required")? {
        return Ok(Some(rollback));
    }
    Ok(Some(quarantine))
}

pub(super) fn remote_restore_quarantine_active(destination: &Path) -> Result<bool> {
    Ok(
        read_remote_restore_quarantine(destination)?.is_some_and(|quarantine| {
            matches!(
                quarantine.phase,
                RemoteRestoreQuarantinePhaseV1::Publishing
                    | RemoteRestoreQuarantinePhaseV1::RollbackRequired
                    | RemoteRestoreQuarantinePhaseV1::Published
                    | RemoteRestoreQuarantinePhaseV1::RolledBack
            )
        }),
    )
}

#[cfg(test)]
pub(super) fn remote_restore_quarantine_blocks_open(destination: &Path) -> Result<bool> {
    let Some(quarantine) = read_remote_restore_quarantine(destination)? else {
        return Ok(false);
    };
    let Some(outcome) = activated_remote_restore(&quarantine) else {
        return Ok(true);
    };
    Ok(validate_completed_remote_restore(destination, &quarantine, outcome).is_err())
}

pub(in crate::daemon::store_runtime::session_registry) fn remote_restore_activated_open_identity(
    destination: &Path,
) -> Result<Option<u64>> {
    let Some(quarantine) = read_remote_restore_quarantine(destination)? else {
        return Ok(None);
    };
    let Some(outcome) = activated_remote_restore(&quarantine) else {
        return Err(session_registry_error(
            "authorize activated remote restore open",
            "project sessions are fenced by an incomplete remote restore".to_owned(),
        ));
    };
    validate_completed_remote_restore(destination, &quarantine, outcome)?;
    Ok(Some(expected_terminal_identity(&quarantine, outcome)))
}

fn validate_opened_runtime_identity(
    quarantine: &RemoteRestoreQuarantineV1,
    outcome: RestorePublicationV1,
    opened_file_identity: u64,
) -> Result<()> {
    let expected_identity = expected_terminal_identity(quarantine, outcome);
    if opened_file_identity != expected_identity {
        return Err(session_registry_error(
            "validate opened remote restore runtime",
            format!(
                "opened identity {opened_file_identity} does not match activated identity {expected_identity}"
            ),
        ));
    }
    Ok(())
}

fn expected_terminal_identity(
    quarantine: &RemoteRestoreQuarantineV1,
    outcome: RestorePublicationV1,
) -> u64 {
    match outcome {
        RestorePublicationV1::Published => quarantine.expected_published_identity,
        RestorePublicationV1::RolledBack => quarantine.expected_rollback_identity,
    }
}

pub(super) fn validate_completed_remote_restore(
    destination: &Path,
    quarantine: &RemoteRestoreQuarantineV1,
    outcome: RestorePublicationV1,
) -> Result<()> {
    let expected_identity = expected_terminal_identity(quarantine, outcome);
    let observed_identity = tracedecay_runtime_core::db::sqlite_generation_identity(destination)
        .map_err(|error| {
            session_registry_error(
                "validate completed remote restore identity",
                format!("{error:?}"),
            )
        })?;
    if observed_identity != expected_identity {
        return Err(session_registry_error(
            "validate completed remote restore identity",
            format!(
                "destination identity {observed_identity} does not match terminal identity {expected_identity}"
            ),
        ));
    }
    validate_isolated_restore(destination).map_err(|error| {
        session_registry_error(
            "validate completed remote restore SQLite family",
            format!("{error:?}"),
        )
    })
}

pub(super) fn install_remote_restore_quarantine(
    destination: &Path,
    staging: &Path,
    rollback: &Path,
    expected_rollback_identity: u64,
    expected_published_identity: u64,
) -> Result<()> {
    if remote_restore_quarantine_active(destination)? {
        return Err(session_registry_error(
            "install remote restore quarantine fence",
            "another incomplete remote restore is already fenced".to_owned(),
        ));
    }
    write_remote_restore_quarantine(
        destination,
        &RemoteRestoreQuarantineV1 {
            version: REMOTE_RESTORE_QUARANTINE_VERSION.to_owned(),
            staging: staging.to_path_buf(),
            rollback: rollback.to_path_buf(),
            expected_rollback_identity,
            expected_published_identity,
            phase: RemoteRestoreQuarantinePhaseV1::Publishing,
        },
    )
}

pub(in crate::daemon::store_runtime::session_registry::remote_recovery) fn mark_remote_restore_rollback_required(
    destination: &Path,
    rollback: &Path,
    expected_rollback_identity: u64,
    expected_published_identity: u64,
) -> Result<()> {
    let mut quarantine = read_remote_restore_quarantine(destination)?.ok_or_else(|| {
        session_registry_error(
            "mark remote restore rollback required",
            "remote restore quarantine fence is unavailable".to_owned(),
        )
    })?;
    if quarantine.rollback != rollback
        || quarantine.expected_rollback_identity != expected_rollback_identity
        || quarantine.expected_published_identity != expected_published_identity
    {
        return Err(session_registry_error(
            "mark remote restore rollback required",
            "remote restore quarantine identity changed".to_owned(),
        ));
    }
    match quarantine.phase {
        RemoteRestoreQuarantinePhaseV1::Publishing => {
            quarantine.phase = RemoteRestoreQuarantinePhaseV1::RollbackRequired;
            write_transition(destination, &quarantine, "rollback-required")
        }
        RemoteRestoreQuarantinePhaseV1::RollbackRequired => Ok(()),
        RemoteRestoreQuarantinePhaseV1::Published
        | RemoteRestoreQuarantinePhaseV1::RolledBack
        | RemoteRestoreQuarantinePhaseV1::ActivatedPublished
        | RemoteRestoreQuarantinePhaseV1::ActivatedRolledBack => Err(session_registry_error(
            "mark remote restore rollback required",
            "completed remote restore cannot re-enter rollback".to_owned(),
        )),
    }
}

pub(super) fn completed_remote_restore(
    quarantine: &RemoteRestoreQuarantineV1,
) -> Option<RestorePublicationV1> {
    match quarantine.phase {
        RemoteRestoreQuarantinePhaseV1::Published => Some(RestorePublicationV1::Published),
        RemoteRestoreQuarantinePhaseV1::RolledBack => Some(RestorePublicationV1::RolledBack),
        RemoteRestoreQuarantinePhaseV1::ActivatedPublished => Some(RestorePublicationV1::Published),
        RemoteRestoreQuarantinePhaseV1::ActivatedRolledBack => {
            Some(RestorePublicationV1::RolledBack)
        }
        RemoteRestoreQuarantinePhaseV1::Publishing
        | RemoteRestoreQuarantinePhaseV1::RollbackRequired => None,
    }
}

pub(super) fn activated_remote_restore(
    quarantine: &RemoteRestoreQuarantineV1,
) -> Option<RestorePublicationV1> {
    match quarantine.phase {
        RemoteRestoreQuarantinePhaseV1::ActivatedPublished => Some(RestorePublicationV1::Published),
        RemoteRestoreQuarantinePhaseV1::ActivatedRolledBack => {
            Some(RestorePublicationV1::RolledBack)
        }
        RemoteRestoreQuarantinePhaseV1::Publishing
        | RemoteRestoreQuarantinePhaseV1::RollbackRequired
        | RemoteRestoreQuarantinePhaseV1::Published
        | RemoteRestoreQuarantinePhaseV1::RolledBack => None,
    }
}

pub(super) fn rollback_required(quarantine: &RemoteRestoreQuarantineV1) -> bool {
    quarantine.phase == RemoteRestoreQuarantinePhaseV1::RollbackRequired
}

pub(super) fn activated_fence_matches_preserved_authority(
    destination: &Path,
    quarantine: &RemoteRestoreQuarantineV1,
    expected_preserved_identity: u64,
) -> bool {
    let Some(outcome) = activated_remote_restore(quarantine) else {
        return false;
    };
    expected_terminal_identity(quarantine, outcome) == expected_preserved_identity
        && validate_completed_remote_restore(destination, quarantine, outcome).is_ok()
}

pub(super) fn complete_remote_restore_quarantine(
    destination: &Path,
    outcome: RestorePublicationV1,
) -> Result<()> {
    let mut quarantine = read_remote_restore_quarantine(destination)?.ok_or_else(|| {
        session_registry_error(
            "complete remote restore quarantine",
            "remote restore quarantine fence is unavailable".to_owned(),
        )
    })?;
    if completed_remote_restore(&quarantine) == Some(outcome) {
        return Ok(());
    }
    if completed_remote_restore(&quarantine).is_some() {
        return Err(session_registry_error(
            "complete remote restore quarantine",
            "remote restore already records a different terminal outcome".to_owned(),
        ));
    }
    if rollback_required(&quarantine) && outcome == RestorePublicationV1::Published {
        return Err(session_registry_error(
            "complete remote restore quarantine",
            "rejected remote restore cannot be completed as published".to_owned(),
        ));
    }
    quarantine.phase = match outcome {
        RestorePublicationV1::Published => RemoteRestoreQuarantinePhaseV1::Published,
        RestorePublicationV1::RolledBack => RemoteRestoreQuarantinePhaseV1::RolledBack,
    };
    write_transition(destination, &quarantine, "terminal")
}

pub(super) fn activate_remote_restore_quarantine(
    destination: &Path,
    outcome: RestorePublicationV1,
) -> Result<()> {
    let mut quarantine = read_remote_restore_quarantine(destination)?.ok_or_else(|| {
        session_registry_error(
            "activate remote restore quarantine",
            "remote restore quarantine fence is unavailable".to_owned(),
        )
    })?;
    if activated_remote_restore(&quarantine) == Some(outcome) {
        return Ok(());
    }
    if completed_remote_restore(&quarantine) != Some(outcome) {
        return Err(session_registry_error(
            "activate remote restore quarantine",
            "remote restore terminal outcome is unavailable or different".to_owned(),
        ));
    }
    quarantine.phase = match outcome {
        RestorePublicationV1::Published => RemoteRestoreQuarantinePhaseV1::ActivatedPublished,
        RestorePublicationV1::RolledBack => RemoteRestoreQuarantinePhaseV1::ActivatedRolledBack,
    };
    write_transition(destination, &quarantine, "activated")
}

impl RemoteRecoveryPublicationContextV1 {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn prepare_remote_restore_swap(
        &self,
        mounted: &mut BTreeMap<ProjectId, RegisteredGlobalDbLeaseV1>,
        project_id: ProjectId,
        destination: &Path,
        staging: &Path,
        rollback: &Path,
        expected_database_identity: u64,
        expected_published_identity: u64,
        reservation: DestructiveMaintenanceReservation,
    ) -> Result<PrepublicationRestoreV1> {
        if let Err(error) = super::sqlite_family::checkpoint_for_publication(destination) {
            self.abort_and_remount_restore(
                mounted,
                project_id,
                expected_database_identity,
                reservation,
                "release failed remote restore destination checkpoint",
                error.to_string(),
            )
            .await?;
            return Ok(PrepublicationRestoreV1::RolledBack);
        }
        if let Err(error) = super::sqlite_family::checkpoint_for_publication(staging) {
            self.abort_and_remount_restore(
                mounted,
                project_id,
                expected_database_identity,
                reservation,
                "release failed remote restore staging checkpoint",
                error.to_string(),
            )
            .await?;
            return Ok(PrepublicationRestoreV1::RolledBack);
        }
        if let Err(error) = install_remote_restore_quarantine(
            destination,
            staging,
            rollback,
            expected_database_identity,
            expected_published_identity,
        ) {
            let installed = read_remote_restore_quarantine(destination)?;
            if installed.as_ref().is_some_and(|fence| {
                fence.staging == staging
                    && fence.rollback == rollback
                    && fence.expected_rollback_identity == expected_database_identity
                    && fence.expected_published_identity == expected_published_identity
                    && completed_remote_restore(fence).is_none()
                    && !rollback_required(fence)
            }) {
                self.abort_and_remount_quarantined_restore(
                    mounted,
                    project_id,
                    destination,
                    reservation,
                    "release failed remote restore quarantine installation",
                    error.to_string(),
                )
                .await?;
                return Ok(PrepublicationRestoreV1::RolledBack);
            }
            if installed.is_none() {
                self.abort_and_remount_restore(
                    mounted,
                    project_id,
                    expected_database_identity,
                    reservation,
                    "release failed remote restore quarantine installation",
                    error.to_string(),
                )
                .await?;
                return Ok(PrepublicationRestoreV1::RolledBack);
            }
            if installed.as_ref().is_some_and(|fence| {
                activated_fence_matches_preserved_authority(
                    destination,
                    fence,
                    expected_database_identity,
                )
            }) {
                self.abort_and_remount_restore(
                    mounted,
                    project_id,
                    expected_database_identity,
                    reservation,
                    "release failed remote restore quarantine installation",
                    error.to_string(),
                )
                .await?;
                return Ok(PrepublicationRestoreV1::RolledBack);
            }
            return Err(session_registry_error(
                "release failed remote restore quarantine installation",
                format!("installation={error}; a foreign quarantine fence is active"),
            ));
        }
        Ok(PrepublicationRestoreV1::Ready(reservation))
    }

    pub(super) async fn retire_mounted_target_before_replacement(
        &self,
        project_id: &ProjectId,
        destination: &Path,
    ) -> Result<RetiredMountedRestoreTargetV1> {
        let initial_mounted = lock_project_sessions_for_replacement(&self.project_sessions).await;
        let database = initial_mounted.get(project_id).cloned();
        let Some(database) = database else {
            let preserved_identity =
                tracedecay_runtime_core::db::sqlite_generation_identity(destination).ok();
            let reservation = self
                .registry
                .begin_destructive_maintenance(replacement_target(destination)?)
                .await
                .map_err(|error| {
                    session_registry_error(
                        "retire unmounted remote restore target",
                        format!("{error:?}"),
                    )
                })?;
            return Ok(RetiredMountedRestoreTargetV1 {
                mounted: initial_mounted,
                reservation: Some(reservation),
                preserved_identity,
                _quiescence: None,
            });
        };
        drop(initial_mounted);
        if database.db_path() != destination {
            return Err(session_registry_error(
                "retire mounted remote restore target",
                format!(
                    "mounted path '{}' does not match restore destination '{}'",
                    database.db_path().display(),
                    destination.display()
                ),
            ));
        }
        let expected_binding = database.binding().clone();
        let expected_identity = database.runtime().opened_file_identity().ok_or_else(|| {
            session_registry_error(
                "retire mounted remote restore target",
                "opened ProjectSessions identity is unavailable".to_owned(),
            )
        })?;
        let close_authority = database.authority().clone();
        let (graph_binding, graph_verified_locator) = database
            .session_relation_graph_identity()
            .map(|(binding, locator)| (binding.clone(), locator.clone()))?;
        let target = replacement_target(destination)?;
        let lifecycle = self.project_lifecycle().map_err(|error| {
            session_registry_error("retire mounted remote restore target", error.to_string())
        })?;
        let quiescence = lifecycle.quiesce(project_id, &database).await?;
        let mut mounted = Arc::clone(&self.project_sessions).lock_owned().await;
        if !mounted
            .get(project_id)
            .is_some_and(|mounted| mounted.shares_client_with(&database))
        {
            if let Some(current) = mounted.get(project_id).cloned() {
                self.rebind_session_sync(project_id, &current).await?;
            }
            return Err(session_registry_error(
                "retire mounted remote restore target",
                "mounted ProjectSessions authority changed during quiescence".to_owned(),
            ));
        }
        if let Err(error) = self.replay.unregister_target(project_id, &expected_binding) {
            self.rebind_session_sync(project_id, &database).await?;
            return Err(session_registry_error(
                "retire mounted remote restore target",
                error,
            ));
        }
        let mounted_database = mounted.remove(project_id).ok_or_else(|| {
            session_registry_error(
                "retire mounted remote restore target",
                "mounted ProjectSessions authority disappeared during retirement".to_owned(),
            )
        })?;
        drop(database);
        drop(mounted_database);
        if let Err(error) = super::super::super::code_graph::graph_attachment::close_retained(
            &self.graph_registry,
            graph_binding,
            graph_verified_locator,
        )
        .await
        {
            let restored = self
                .mount_project_sessions(project_id.clone(), expected_identity)
                .await
                .map_err(|remount| {
                    session_registry_error(
                        "restore mounted target after graph close refusal",
                        format!("{error}; remount failed: {remount}"),
                    )
                })?;
            self.publish_mounted(&mut mounted, project_id.clone(), restored)
                .await?;
            return Err(error);
        }
        let preserved_identity =
            tracedecay_runtime_core::db::sqlite_generation_identity(destination).ok();
        if preserved_identity != Some(expected_identity) {
            self.registry
                .close_exact_stale_attachment(
                    &expected_binding,
                    &close_authority,
                    expected_identity,
                )
                .await
                .map_err(|error| {
                    session_registry_error(
                        "close stale mounted remote restore target",
                        format!("{error:?}"),
                    )
                })?;
            return Ok(RetiredMountedRestoreTargetV1 {
                mounted,
                reservation: None,
                preserved_identity,
                _quiescence: Some(quiescence),
            });
        }
        let reservation = match self.registry.begin_destructive_maintenance(target).await {
            Ok(reservation) => reservation,
            Err(error) => {
                let restored = self
                    .mount_project_sessions(project_id.clone(), expected_identity)
                    .await
                    .map_err(|remount| {
                        session_registry_error(
                            "restore mounted target after retirement refusal",
                            format!("{error:?}; remount failed: {remount}"),
                        )
                    })?;
                self.publish_mounted(&mut mounted, project_id.clone(), restored)
                    .await?;
                return Err(session_registry_error(
                    "retire mounted remote restore target",
                    format!("{error:?}"),
                ));
            }
        };
        if !reservation
            .closed()
            .iter()
            .any(|closed| closed.binding() == &expected_binding)
        {
            reservation.abort_preserved().map_err(|error| {
                session_registry_error(
                    "release incomplete mounted restore retirement",
                    format!("{error:?}"),
                )
            })?;
            let restored = self
                .mount_project_sessions(project_id.clone(), expected_identity)
                .await?;
            self.publish_mounted(&mut mounted, project_id.clone(), restored)
                .await?;
            return Err(session_registry_error(
                "retire mounted remote restore target",
                "destructive reservation omitted the exact ProjectSessions binding".to_owned(),
            ));
        }
        Ok(RetiredMountedRestoreTargetV1 {
            mounted,
            reservation: Some(reservation),
            preserved_identity,
            _quiescence: Some(quiescence),
        })
    }

    pub(in crate::daemon::store_runtime::session_registry::remote_recovery) async fn resume_retained_rollback(
        &self,
        _project_id: ProjectId,
        destination: &Path,
        rollback: &Path,
        destination_matches_restore: bool,
    ) -> Result<bool> {
        if destination_matches_restore {
            return Ok(false);
        }
        if rollback.exists() {
            reject_unbound_retained_rollback(rollback)?;
        }
        if !destination.try_exists().map_err(|error| {
            session_registry_error("inspect remote restore destination", error.to_string())
        })? {
            return Err(session_registry_error(
                "resume remote restore",
                "destination and retained rollback are both unavailable".to_owned(),
            ));
        }
        Ok(false)
    }

    // Only called from the staged-restore transaction while its recovery
    // admission guard excludes daemon-wide deletion.
    pub(in crate::daemon::store_runtime::session_registry::remote_recovery) async fn ensure_project_sessions_target_while_admitted(
        &self,
        project_id: ProjectId,
        expected_opened_file_identity: u64,
        expected_destination: &Path,
    ) -> Result<()> {
        let mut mounted = Arc::clone(&self.project_sessions).lock_owned().await;
        if let Some(database) = mounted.get(&project_id) {
            let expected_shard = tracedecay_store::StoreShardIdV1::project_sessions(
                self.identity.brain_id().clone(),
                self.identity.profile_id().clone(),
                project_id.clone(),
            );
            validate_existing_mounted_identity(
                database,
                &expected_shard,
                self.incarnation,
                expected_opened_file_identity,
                expected_destination,
            )?;
            return Ok(());
        }
        let database = self
            .mount_project_sessions(project_id.clone(), expected_opened_file_identity)
            .await?;
        self.publish_mounted(&mut mounted, project_id, database)
            .await
    }

    pub(super) async fn prepare_mounted(
        &self,
        project_id: &ProjectId,
        database: &RegisteredGlobalDbLeaseV1,
    ) -> Result<()> {
        let runtime = database.runtime().clone();
        let authority = runtime
            .database_authority("register restored remote replay target")
            .map_err(|error| {
                registry_open_error("register restored remote replay target", error)
            })?;
        self.replay
            .register_target(project_id.clone(), runtime, authority)
            .map_err(|error| session_registry_error("register restored replay target", error))?;
        if let Err(rebind_error) = self.rebind_session_sync(project_id, database).await {
            let cleanup = self
                .retire_unpublished_mounted(project_id, database)
                .await
                .err();
            return Err(session_registry_error(
                "prepare restored project sessions publication",
                format!("session_sync={rebind_error}; cleanup={cleanup:?}"),
            ));
        }
        Ok(())
    }

    pub(in crate::daemon::store_runtime::session_registry::remote_recovery) async fn publish_quarantined_mounted(
        &self,
        mounted: &mut BTreeMap<ProjectId, RegisteredGlobalDbLeaseV1>,
        project_id: ProjectId,
        database: RegisteredGlobalDbLeaseV1,
        destination: &Path,
        outcome: RestorePublicationV1,
    ) -> Result<()> {
        let quarantine = read_remote_restore_quarantine(destination)?.ok_or_else(|| {
            session_registry_error(
                "publish quarantined remote restore",
                "remote restore quarantine fence is unavailable".to_owned(),
            )
        })?;
        let opened_file_identity = database.runtime().opened_file_identity().ok_or_else(|| {
            session_registry_error(
                "publish quarantined remote restore",
                "opened runtime identity is unavailable".to_owned(),
            )
        })?;
        validate_opened_runtime_identity(&quarantine, outcome, opened_file_identity)?;
        PrivateStoreIo::sync_sqlite_family(destination).map_err(|error| {
            session_registry_error(
                "sync attached remote restore SQLite family",
                error.to_string(),
            )
        })?;
        complete_remote_restore_quarantine(destination, outcome)?;
        self.prepare_mounted(&project_id, &database).await?;
        mounted.insert(project_id.clone(), database);
        if let Err(error) = activate_remote_restore_quarantine(destination, outcome) {
            let database = mounted.remove(&project_id).ok_or_else(|| {
                session_registry_error(
                    "activate remote restore publication",
                    "mounted restore disappeared before cleanup".to_owned(),
                )
            })?;
            let cleanup = self
                .retire_unpublished_mounted(&project_id, &database)
                .await
                .err();
            return Err(session_registry_error(
                "activate remote restore publication",
                format!("activation={error}; cleanup={cleanup:?}"),
            ));
        }
        Ok(())
    }

    pub(super) async fn abort_and_remount_quarantined_restore(
        &self,
        mounted: &mut BTreeMap<ProjectId, RegisteredGlobalDbLeaseV1>,
        project_id: ProjectId,
        destination: &Path,
        reservation: DestructiveMaintenanceReservation,
        operation: &'static str,
        failure: String,
    ) -> Result<RestorePublicationV1> {
        reservation.abort_preserved().map_err(|release_failure| {
            session_registry_error(
                operation,
                format!("{failure}; release failed: {release_failure:?}"),
            )
        })?;
        let expected_rollback_identity = read_remote_restore_quarantine(destination)?
            .ok_or_else(|| {
                session_registry_error(
                    operation,
                    "remote restore quarantine fence is unavailable".to_owned(),
                )
            })?
            .expected_rollback_identity;
        let restored = self
            .mount_project_sessions(project_id.clone(), expected_rollback_identity)
            .await
            .map_err(|remount| {
                session_registry_error(operation, format!("{failure}; remount failed: {remount}"))
            })?;
        self.publish_quarantined_mounted(
            mounted,
            project_id,
            restored,
            destination,
            RestorePublicationV1::RolledBack,
        )
        .await?;
        Ok(RestorePublicationV1::RolledBack)
    }
}
