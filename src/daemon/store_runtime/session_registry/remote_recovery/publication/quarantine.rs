use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracedecay_runtime_core::storage::PrivateStoreIo;

use super::{RestorePublicationV1, Result, session_registry_error};
use crate::daemon::store_runtime::session_registry::ProjectSessionTerminalVacancyAuthorityV1;
use crate::db::DatabaseAuthority;

const REMOTE_RESTORE_QUARANTINE_VERSION: &str = "tracedecay.remote-restore.v3";

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
    version: String,
    staging: PathBuf,
    rollback: PathBuf,
    expected_rollback_identity: u64,
    expected_published_identity: u64,
    terminal_vacancy: ProjectSessionTerminalVacancyAuthorityV1,
    phase: RemoteRestoreQuarantinePhaseV1,
}

impl RemoteRestoreQuarantineV1 {
    pub(super) fn terminal_outcome(&self) -> Option<RestorePublicationV1> {
        match self.phase {
            RemoteRestoreQuarantinePhaseV1::Published
            | RemoteRestoreQuarantinePhaseV1::ActivatedPublished => {
                Some(RestorePublicationV1::Published)
            }
            RemoteRestoreQuarantinePhaseV1::RolledBack
            | RemoteRestoreQuarantinePhaseV1::ActivatedRolledBack => {
                Some(RestorePublicationV1::RolledBack)
            }
            RemoteRestoreQuarantinePhaseV1::Publishing
            | RemoteRestoreQuarantinePhaseV1::RollbackRequired => None,
        }
    }

    pub(super) fn expected_identity(&self, outcome: RestorePublicationV1) -> u64 {
        match outcome {
            RestorePublicationV1::Published => self.expected_published_identity,
            RestorePublicationV1::RolledBack => self.expected_rollback_identity,
        }
    }

    pub(super) fn terminal_vacancy(&self) -> &ProjectSessionTerminalVacancyAuthorityV1 {
        &self.terminal_vacancy
    }

    pub(super) fn is_activated(&self) -> bool {
        matches!(
            self.phase,
            RemoteRestoreQuarantinePhaseV1::ActivatedPublished
                | RemoteRestoreQuarantinePhaseV1::ActivatedRolledBack
        )
    }

    fn restart_outcome_for_observed_identity(
        &self,
        observed_identity: u64,
    ) -> Result<RestorePublicationV1> {
        match self.phase {
            RemoteRestoreQuarantinePhaseV1::Publishing => {
                if observed_identity == self.expected_published_identity {
                    Ok(RestorePublicationV1::Published)
                } else if observed_identity == self.expected_rollback_identity {
                    Ok(RestorePublicationV1::RolledBack)
                } else {
                    Err(session_registry_error(
                        "recover remote restore quarantine",
                        format!(
                            "publishing destination identity {observed_identity} matches neither durable rollback nor published identity"
                        ),
                    ))
                }
            }
            RemoteRestoreQuarantinePhaseV1::RollbackRequired => Err(session_registry_error(
                "recover remote restore quarantine",
                "remote restore requires an explicit rollback before admission can resume"
                    .to_owned(),
            )),
            RemoteRestoreQuarantinePhaseV1::Published
            | RemoteRestoreQuarantinePhaseV1::ActivatedPublished => {
                if observed_identity == self.expected_published_identity {
                    Ok(RestorePublicationV1::Published)
                } else {
                    Err(session_registry_error(
                        "recover remote restore quarantine",
                        format!(
                            "published destination identity {observed_identity} does not match durable identity {}",
                            self.expected_published_identity
                        ),
                    ))
                }
            }
            RemoteRestoreQuarantinePhaseV1::RolledBack
            | RemoteRestoreQuarantinePhaseV1::ActivatedRolledBack => {
                if observed_identity == self.expected_rollback_identity {
                    Ok(RestorePublicationV1::RolledBack)
                } else {
                    Err(session_registry_error(
                        "recover remote restore quarantine",
                        format!(
                            "rolled-back destination identity {observed_identity} does not match durable identity {}",
                            self.expected_rollback_identity
                        ),
                    ))
                }
            }
        }
    }
}

fn fence_path(destination: &Path) -> PathBuf {
    super::super::super::remote_restore_quarantine_fence_path(destination)
}

fn write(destination: &Path, quarantine: &RemoteRestoreQuarantineV1) -> Result<()> {
    let payload = serde_json::to_vec(quarantine).map_err(|error| {
        session_registry_error("encode remote restore quarantine", error.to_string())
    })?;
    let fence = fence_path(destination);
    let staging = fence.with_extension("staging");
    PrivateStoreIo::write_file_atomically_durable(&fence, &staging, &payload).map_err(|error| {
        session_registry_error("write remote restore quarantine", error.to_string())
    })
}

pub(super) fn read_remote_restore_quarantine(
    destination: &Path,
) -> Result<Option<RemoteRestoreQuarantineV1>> {
    let fence = fence_path(destination);
    let Some(payload) =
        DatabaseAuthority::read_record_strict(&fence, "remote restore quarantine fence").map_err(
            |error| session_registry_error("read remote restore quarantine", format!("{error:?}")),
        )?
    else {
        return Ok(None);
    };
    let quarantine: RemoteRestoreQuarantineV1 =
        serde_json::from_str(&payload).map_err(|error| {
            session_registry_error("decode remote restore quarantine", error.to_string())
        })?;
    if quarantine.version != REMOTE_RESTORE_QUARANTINE_VERSION {
        return Err(session_registry_error(
            "decode remote restore quarantine",
            format!("unsupported version '{}'", quarantine.version),
        ));
    }
    Ok(Some(quarantine))
}

pub(super) fn install_remote_restore_quarantine(
    destination: &Path,
    staging: &Path,
    rollback: &Path,
    expected_rollback_identity: u64,
    expected_published_identity: u64,
    terminal_vacancy: ProjectSessionTerminalVacancyAuthorityV1,
) -> Result<()> {
    if read_remote_restore_quarantine(destination)?.is_some() {
        return Err(session_registry_error(
            "install remote restore quarantine",
            "another remote restore fence is already present".to_owned(),
        ));
    }
    write(
        destination,
        &RemoteRestoreQuarantineV1 {
            version: REMOTE_RESTORE_QUARANTINE_VERSION.to_owned(),
            staging: staging.to_path_buf(),
            rollback: rollback.to_path_buf(),
            expected_rollback_identity,
            expected_published_identity,
            terminal_vacancy,
            phase: RemoteRestoreQuarantinePhaseV1::Publishing,
        },
    )
}

pub(super) fn validate_completed_remote_restore(
    destination: &Path,
    quarantine: &RemoteRestoreQuarantineV1,
    outcome: RestorePublicationV1,
) -> Result<()> {
    let observed =
        tracedecay_runtime_core::db::sqlite_generation_identity(destination).map_err(|error| {
            session_registry_error(
                "validate completed remote restore identity",
                format!("{error:?}"),
            )
        })?;
    let expected = quarantine.expected_identity(outcome);
    if observed != expected {
        return Err(session_registry_error(
            "validate completed remote restore identity",
            format!("destination identity {observed} does not match terminal identity {expected}"),
        ));
    }
    Ok(())
}

/// Classifies the physical database after a process restart. `Publishing` is
/// resolved only when the durable identities prove that the native swap either
/// reached the new file or remained/returned at the rollback file. An
/// explicit rollback-required phase stays fenced rather than guessing a
/// terminal outcome.
pub(super) fn recover_remote_restore_quarantine_outcome(
    destination: &Path,
    quarantine: &RemoteRestoreQuarantineV1,
) -> Result<RestorePublicationV1> {
    let observed =
        tracedecay_runtime_core::db::sqlite_generation_identity(destination).map_err(|error| {
            session_registry_error("recover remote restore quarantine", format!("{error:?}"))
        })?;
    let outcome = quarantine.restart_outcome_for_observed_identity(observed)?;
    if quarantine.phase == RemoteRestoreQuarantinePhaseV1::Publishing {
        complete_remote_restore_quarantine(destination, outcome)?;
    }
    Ok(outcome)
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
    if quarantine.terminal_outcome() == Some(outcome) {
        return Ok(());
    }
    if quarantine.terminal_outcome().is_some() {
        return Err(session_registry_error(
            "complete remote restore quarantine",
            "remote restore already has a different terminal outcome".to_owned(),
        ));
    }
    if quarantine.phase == RemoteRestoreQuarantinePhaseV1::RollbackRequired
        && outcome == RestorePublicationV1::Published
    {
        return Err(session_registry_error(
            "complete remote restore quarantine",
            "rollback-required recovery cannot publish the rejected candidate".to_owned(),
        ));
    }
    quarantine.phase = match outcome {
        RestorePublicationV1::Published => RemoteRestoreQuarantinePhaseV1::Published,
        RestorePublicationV1::RolledBack => RemoteRestoreQuarantinePhaseV1::RolledBack,
    };
    write(destination, &quarantine)
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
    if quarantine.terminal_outcome() != Some(outcome) {
        return Err(session_registry_error(
            "activate remote restore quarantine",
            "remote restore terminal outcome is unavailable or different".to_owned(),
        ));
    }
    quarantine.phase = match outcome {
        RestorePublicationV1::Published => RemoteRestoreQuarantinePhaseV1::ActivatedPublished,
        RestorePublicationV1::RolledBack => RemoteRestoreQuarantinePhaseV1::ActivatedRolledBack,
    };
    write(destination, &quarantine)
}

pub(in crate::daemon::store_runtime::session_registry) fn remote_restore_activated_open_identity(
    destination: &Path,
) -> Result<Option<u64>> {
    let Some(quarantine) = read_remote_restore_quarantine(destination)? else {
        return Ok(None);
    };
    let outcome = quarantine.terminal_outcome().ok_or_else(|| {
        session_registry_error(
            "authorize remote restore open",
            "project sessions are fenced by an incomplete remote restore".to_owned(),
        )
    })?;
    if !quarantine.is_activated() {
        return Err(session_registry_error(
            "authorize remote restore open",
            "remote restore is terminal but has not activated its owner".to_owned(),
        ));
    }
    validate_completed_remote_restore(destination, &quarantine, outcome)?;
    Ok(Some(quarantine.expected_identity(outcome)))
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::{BrainId, LocatorDigest, ProjectId, UserProfileId};
    use tracedecay_store::{
        StoreAuthorityEpochV1, StoreIncarnationV1, StoreRuntimeBindingV1, StoreShardIdV1,
        VerifiedStoreLocatorV1,
    };

    use super::*;

    fn terminal_vacancy() -> ProjectSessionTerminalVacancyAuthorityV1 {
        let project_id =
            ProjectId::new("project.remote-restore-restart").expect("project identity");
        let shard_id = StoreShardIdV1::project_sessions(
            BrainId::new("brain.remote-restore-restart").expect("brain identity"),
            UserProfileId::new("profile.remote-restore-restart").expect("profile identity"),
            project_id,
        );
        let incarnation = StoreIncarnationV1::new(7).expect("store incarnation");
        ProjectSessionTerminalVacancyAuthorityV1 {
            binding: StoreRuntimeBindingV1::new(
                shard_id.clone(),
                incarnation,
                StoreAuthorityEpochV1::new(11).expect("authority epoch"),
            ),
            locator: VerifiedStoreLocatorV1::new(
                shard_id,
                incarnation,
                LocatorDigest::new(format!("sha256:{}", "a".repeat(64))).expect("locator digest"),
            ),
        }
    }

    fn journal(phase: RemoteRestoreQuarantinePhaseV1) -> RemoteRestoreQuarantineV1 {
        RemoteRestoreQuarantineV1 {
            version: REMOTE_RESTORE_QUARANTINE_VERSION.to_owned(),
            staging: PathBuf::from("/tmp/restart-staging.sqlite"),
            rollback: PathBuf::from("/tmp/restart-rollback.sqlite"),
            expected_rollback_identity: 41,
            expected_published_identity: 73,
            terminal_vacancy: terminal_vacancy(),
            phase,
        }
    }

    #[test]
    fn durable_journal_restart_classifies_every_quarantine_phase() {
        let cases = [
            (
                RemoteRestoreQuarantinePhaseV1::Publishing,
                73,
                Some(RestorePublicationV1::Published),
            ),
            (
                RemoteRestoreQuarantinePhaseV1::Publishing,
                41,
                Some(RestorePublicationV1::RolledBack),
            ),
            (RemoteRestoreQuarantinePhaseV1::RollbackRequired, 41, None),
            (
                RemoteRestoreQuarantinePhaseV1::Published,
                73,
                Some(RestorePublicationV1::Published),
            ),
            (
                RemoteRestoreQuarantinePhaseV1::RolledBack,
                41,
                Some(RestorePublicationV1::RolledBack),
            ),
            (
                RemoteRestoreQuarantinePhaseV1::ActivatedPublished,
                73,
                Some(RestorePublicationV1::Published),
            ),
            (
                RemoteRestoreQuarantinePhaseV1::ActivatedRolledBack,
                41,
                Some(RestorePublicationV1::RolledBack),
            ),
        ];

        for (phase, observed, expected) in cases {
            let encoded = serde_json::to_vec(&journal(phase)).expect("durable journal encoding");
            let reopened: RemoteRestoreQuarantineV1 =
                serde_json::from_slice(&encoded).expect("durable journal decoding");
            match expected {
                Some(expected) => assert_eq!(
                    reopened
                        .restart_outcome_for_observed_identity(observed)
                        .expect("restart outcome"),
                    expected,
                    "phase {phase:?}"
                ),
                None => assert!(
                    reopened
                        .restart_outcome_for_observed_identity(observed)
                        .is_err(),
                    "phase {phase:?} must remain fail-closed"
                ),
            }
        }
    }

    #[test]
    fn restart_rejects_mismatched_terminal_identity() {
        for phase in [
            RemoteRestoreQuarantinePhaseV1::Publishing,
            RemoteRestoreQuarantinePhaseV1::Published,
            RemoteRestoreQuarantinePhaseV1::RolledBack,
            RemoteRestoreQuarantinePhaseV1::ActivatedPublished,
            RemoteRestoreQuarantinePhaseV1::ActivatedRolledBack,
        ] {
            assert!(
                journal(phase)
                    .restart_outcome_for_observed_identity(99)
                    .is_err(),
                "phase {phase:?} must not infer a third physical outcome"
            );
        }
    }

    #[test]
    fn restart_after_terminal_fence_before_maintenance_retains_exact_vacancy() {
        let temporary = tempfile::tempdir().expect("temporary restore directory");
        let destination = temporary.path().join("sessions.sqlite");
        rusqlite::Connection::open(&destination)
            .expect("create rollback destination")
            .execute_batch("CREATE TABLE retained_before_maintenance (id INTEGER PRIMARY KEY);")
            .expect("seed rollback destination");
        let rollback_identity =
            tracedecay_runtime_core::db::sqlite_generation_identity(&destination)
                .expect("rollback identity");
        let published_identity = rollback_identity
            .checked_add(1)
            .expect("distinct published identity");
        let vacancy = terminal_vacancy();

        // This is the crash window after exact Graph/Store closure and before
        // destructive-maintenance admission. The durable fence must already
        // preserve the only restart authority for the terminal vacancy.
        install_remote_restore_quarantine(
            &destination,
            &temporary.path().join("staging.sqlite"),
            &temporary.path().join("rollback.sqlite"),
            rollback_identity,
            published_identity,
            vacancy.clone(),
        )
        .expect("persist terminal fence before maintenance");

        let reopened = read_remote_restore_quarantine(&destination)
            .expect("read terminal fence")
            .expect("terminal fence remains durable");
        assert_eq!(reopened.terminal_vacancy(), &vacancy);
        assert_eq!(
            reopened.phase,
            RemoteRestoreQuarantinePhaseV1::Publishing,
            "maintenance has not yet selected a physical outcome"
        );
        assert_eq!(
            recover_remote_restore_quarantine_outcome(&destination, &reopened)
                .expect("restart resolves unchanged destination"),
            RestorePublicationV1::RolledBack
        );
    }

    #[test]
    fn durable_activation_before_ready_survives_restart_without_remounting_terminal_owner() {
        let temporary = tempfile::tempdir().expect("temporary restore directory");
        let destination = temporary.path().join("sessions.sqlite");
        rusqlite::Connection::open(&destination)
            .expect("create restored destination")
            .execute_batch("CREATE TABLE activated_before_ready (id INTEGER PRIMARY KEY);")
            .expect("seed restored destination");
        let published_identity =
            tracedecay_runtime_core::db::sqlite_generation_identity(&destination)
                .expect("published identity");
        let rollback_identity = published_identity
            .checked_add(1)
            .expect("distinct rollback identity");

        install_remote_restore_quarantine(
            &destination,
            &temporary.path().join("staging.sqlite"),
            &temporary.path().join("rollback.sqlite"),
            rollback_identity,
            published_identity,
            terminal_vacancy(),
        )
        .expect("persist terminal fence");
        complete_remote_restore_quarantine(&destination, RestorePublicationV1::Published)
            .expect("persist physical terminal outcome");
        activate_remote_restore_quarantine(&destination, RestorePublicationV1::Published)
            .expect("persist activation before map Ready");

        // No in-memory owner has been published. A restarted daemon may now
        // rebuild only a fresh candidate because the durable record proves
        // the terminal owner cannot be reopened.
        assert_eq!(
            remote_restore_activated_open_identity(&destination)
                .expect("read activated durable fence"),
            Some(published_identity)
        );
    }

    #[test]
    fn restart_after_ready_before_durable_activation_finishes_only_the_journal() {
        let temporary = tempfile::tempdir().expect("temporary restore directory");
        let destination = temporary.path().join("sessions.sqlite");
        rusqlite::Connection::open(&destination)
            .expect("create restored destination")
            .execute_batch("CREATE TABLE ready_before_activation (id INTEGER PRIMARY KEY);")
            .expect("seed restored destination");
        let published_identity =
            tracedecay_runtime_core::db::sqlite_generation_identity(&destination)
                .expect("published identity");
        let rollback_identity = published_identity
            .checked_add(1)
            .expect("distinct rollback identity");

        install_remote_restore_quarantine(
            &destination,
            &temporary.path().join("staging.sqlite"),
            &temporary.path().join("rollback.sqlite"),
            rollback_identity,
            published_identity,
            terminal_vacancy(),
        )
        .expect("persist terminal fence");
        complete_remote_restore_quarantine(&destination, RestorePublicationV1::Published)
            .expect("persist physical terminal outcome");

        // This records the historical crash window where the exact candidate
        // had reached Ready but its activation journal write had not. Resume
        // must make only the missing durable transition; it must not touch the
        // already-selected physical database or reopen the terminal owner.
        let before = tracedecay_runtime_core::db::sqlite_generation_identity(&destination)
            .expect("identity before journal completion");
        assert_eq!(before, published_identity);
        activate_remote_restore_quarantine(&destination, RestorePublicationV1::Published)
            .expect("finish only the missing durable activation");
        assert_eq!(
            tracedecay_runtime_core::db::sqlite_generation_identity(&destination)
                .expect("identity after journal completion"),
            published_identity
        );
        assert_eq!(
            remote_restore_activated_open_identity(&destination)
                .expect("read completed durable activation"),
            Some(published_identity)
        );
    }
}
