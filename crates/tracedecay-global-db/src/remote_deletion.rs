//! Durable remote-deletion tombstones and cleanup state.

use serde::{Deserialize, Serialize};
use tracedecay_runtime_core::db::engine::{QueryExecutor, params};
use tracedecay_runtime_core::errors::TraceDecayError;

use crate::RegisteredGlobalDb;

type Result<T> = std::result::Result<T, TraceDecayError>;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteDeletionTarget {
    Account,
    Project,
}

impl RemoteDeletionTarget {
    fn as_str(self) -> &'static str {
        match self {
            Self::Account => "account",
            Self::Project => "project",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "account" => Some(Self::Account),
            "project" => Some(Self::Project),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteDeletionFailureCode {
    InvalidRequest,
    AuthorityUnavailable,
    TargetNotFound,
    TombstoneConflict,
    TombstoneUnavailable,
    ProjectEnumerationUnavailable,
    RuntimeOwnersSettling,
    RuntimeRetirementIncomplete,
    ShardCleanupFailed,
    RegistryCleanupFailed,
}

impl RemoteDeletionFailureCode {
    fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::AuthorityUnavailable => "authority_unavailable",
            Self::TargetNotFound => "target_not_found",
            Self::TombstoneConflict => "tombstone_conflict",
            Self::TombstoneUnavailable => "tombstone_unavailable",
            Self::ProjectEnumerationUnavailable => "project_enumeration_unavailable",
            Self::RuntimeOwnersSettling => "runtime_owners_settling",
            Self::RuntimeRetirementIncomplete => "runtime_retirement_incomplete",
            Self::ShardCleanupFailed => "shard_cleanup_failed",
            Self::RegistryCleanupFailed => "registry_cleanup_failed",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "invalid_request" => Some(Self::InvalidRequest),
            "authority_unavailable" => Some(Self::AuthorityUnavailable),
            "target_not_found" => Some(Self::TargetNotFound),
            "tombstone_conflict" => Some(Self::TombstoneConflict),
            "tombstone_unavailable" => Some(Self::TombstoneUnavailable),
            "project_enumeration_unavailable" => Some(Self::ProjectEnumerationUnavailable),
            "runtime_owners_settling" => Some(Self::RuntimeOwnersSettling),
            "runtime_retirement_incomplete" => Some(Self::RuntimeRetirementIncomplete),
            "shard_cleanup_failed" => Some(Self::ShardCleanupFailed),
            "registry_cleanup_failed" => Some(Self::RegistryCleanupFailed),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteDeletionPhase {
    ValidateRequest,
    ResolveAuthority,
    ResolveTarget,
    PersistTombstone,
    EnumerateProjects,
    CancelRuntimeOwners,
    RemoveShard,
    RemoveRegistryEntry,
}

impl RemoteDeletionPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::ValidateRequest => "validate_request",
            Self::ResolveAuthority => "resolve_authority",
            Self::ResolveTarget => "resolve_target",
            Self::PersistTombstone => "persist_tombstone",
            Self::EnumerateProjects => "enumerate_projects",
            Self::CancelRuntimeOwners => "cancel_runtime_owners",
            Self::RemoveShard => "remove_shard",
            Self::RemoveRegistryEntry => "remove_registry_entry",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "validate_request" => Some(Self::ValidateRequest),
            "resolve_authority" => Some(Self::ResolveAuthority),
            "resolve_target" => Some(Self::ResolveTarget),
            "persist_tombstone" => Some(Self::PersistTombstone),
            "enumerate_projects" => Some(Self::EnumerateProjects),
            "cancel_runtime_owners" => Some(Self::CancelRuntimeOwners),
            "remove_shard" => Some(Self::RemoveShard),
            "remove_registry_entry" => Some(Self::RemoveRegistryEntry),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RemoteDeletionCleanupState {
    Pending,
    Settling {
        failure_code: RemoteDeletionFailureCode,
        phase: RemoteDeletionPhase,
        retryable: bool,
    },
    Partial {
        failure_code: RemoteDeletionFailureCode,
        phase: RemoteDeletionPhase,
        retryable: bool,
    },
    Deleted,
}

impl RemoteDeletionCleanupState {
    fn status(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Settling { .. } => "settling",
            Self::Partial { .. } => "partial",
            Self::Deleted => "deleted",
        }
    }

    fn failure(&self) -> Option<(RemoteDeletionFailureCode, RemoteDeletionPhase, bool)> {
        match self {
            Self::Settling {
                failure_code,
                phase,
                retryable,
            }
            | Self::Partial {
                failure_code,
                phase,
                retryable,
            } => Some((*failure_code, *phase, *retryable)),
            Self::Pending | Self::Deleted => None,
        }
    }

    fn decode(
        status: &str,
        failure_code: Option<&str>,
        phase: Option<&str>,
        retryable: Option<i64>,
    ) -> Result<Self> {
        let failure = || {
            let failure_code = failure_code
                .and_then(RemoteDeletionFailureCode::from_str)
                .ok_or_else(|| {
                    remote_deletion_error("decode remote deletion state", "unknown failure code")
                })?;
            let phase = phase
                .and_then(RemoteDeletionPhase::from_str)
                .ok_or_else(|| {
                    remote_deletion_error("decode remote deletion state", "unknown failure phase")
                })?;
            let retryable = match retryable {
                Some(0) => false,
                Some(1) => true,
                _ => {
                    return Err(remote_deletion_error(
                        "decode remote deletion state",
                        "invalid retryable flag",
                    ));
                }
            };
            Ok((failure_code, phase, retryable))
        };
        match status {
            "pending" if failure_code.is_none() && phase.is_none() && retryable.is_none() => {
                Ok(Self::Pending)
            }
            "settling" => {
                let (failure_code, phase, retryable) = failure()?;
                Ok(Self::Settling {
                    failure_code,
                    phase,
                    retryable,
                })
            }
            "partial" => {
                let (failure_code, phase, retryable) = failure()?;
                Ok(Self::Partial {
                    failure_code,
                    phase,
                    retryable,
                })
            }
            "deleted" if failure_code.is_none() && phase.is_none() && retryable.is_none() => {
                Ok(Self::Deleted)
            }
            _ => Err(remote_deletion_error(
                "decode remote deletion state",
                "invalid cleanup state",
            )),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct RemoteDeletionTombstone {
    pub target: RemoteDeletionTarget,
    pub profile_id: String,
    pub project_id: Option<String>,
    pub tombstone_id: String,
    pub recorded_at_micros: i64,
    pub cleanup: RemoteDeletionCleanupState,
}

impl RemoteDeletionTombstone {
    fn project_key(&self) -> &str {
        self.project_id.as_deref().unwrap_or("")
    }

    fn validate_new(&self) -> Result<()> {
        validate_identifier("profile id", &self.profile_id)?;
        validate_identifier("tombstone id", &self.tombstone_id)?;
        if self.recorded_at_micros <= 0 {
            return Err(remote_deletion_error(
                "validate remote deletion tombstone",
                "recorded timestamp must be positive",
            ));
        }
        match (self.target, self.project_id.as_deref()) {
            (RemoteDeletionTarget::Account, None) => {}
            (RemoteDeletionTarget::Project, Some(project_id)) => {
                validate_identifier("project id", project_id)?;
            }
            (RemoteDeletionTarget::Account, Some(_)) => {
                return Err(remote_deletion_error(
                    "validate remote deletion tombstone",
                    "account tombstones cannot name a project",
                ));
            }
            (RemoteDeletionTarget::Project, None) => {
                return Err(remote_deletion_error(
                    "validate remote deletion tombstone",
                    "project tombstones require a project",
                ));
            }
        }
        if self.cleanup != RemoteDeletionCleanupState::Pending {
            return Err(remote_deletion_error(
                "validate remote deletion tombstone",
                "new tombstones must start pending",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteDeletionTombstoneRecordOutcome {
    Recorded(RemoteDeletionTombstone),
    Replayed(RemoteDeletionTombstone),
    Conflict { existing: RemoteDeletionTombstone },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteDeletionTombstoneTransitionOutcome {
    Updated(RemoteDeletionTombstone),
    StateChanged { existing: RemoteDeletionTombstone },
    Conflict { existing: RemoteDeletionTombstone },
}

impl RegisteredGlobalDb {
    pub async fn remote_deletion_tombstone(
        &self,
        profile_id: &str,
        target: RemoteDeletionTarget,
        project_id: Option<&str>,
    ) -> Result<Option<RemoteDeletionTombstone>> {
        validate_identifier("profile id", profile_id)?;
        let project_key = match target {
            RemoteDeletionTarget::Account => "",
            RemoteDeletionTarget::Project => {
                let project_id = project_id.ok_or_else(|| {
                    remote_deletion_error("read remote deletion tombstone", "missing project id")
                })?;
                validate_identifier("project id", project_id)?;
                project_id
            }
        };
        let snapshot = self.read_snapshot().await?;
        read_tombstone(&snapshot, profile_id, target, project_key).await
    }

    pub async fn record_remote_deletion_tombstone(
        &self,
        tombstone: RemoteDeletionTombstone,
    ) -> Result<RemoteDeletionTombstoneRecordOutcome> {
        tombstone.validate_new()?;
        let transaction = self.begin_write_transaction().await?;
        if let Some(existing) = read_tombstone(
            &transaction,
            &tombstone.profile_id,
            tombstone.target,
            tombstone.project_key(),
        )
        .await?
        {
            transaction.commit().await.map_err(|error| {
                remote_deletion_error("commit remote deletion tombstone replay", error)
            })?;
            return if existing.tombstone_id == tombstone.tombstone_id {
                Ok(RemoteDeletionTombstoneRecordOutcome::Replayed(existing))
            } else {
                Ok(RemoteDeletionTombstoneRecordOutcome::Conflict { existing })
            };
        }
        transaction
            .execute(
                "INSERT INTO remote_deletion_tombstones
                    (profile_id, target_kind, project_id, tombstone_id, recorded_at_micros,
                     cleanup_status, failure_code, failure_phase, retryable)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'pending', NULL, NULL, NULL)",
                params![
                    tombstone.profile_id.as_str(),
                    tombstone.target.as_str(),
                    tombstone.project_key(),
                    tombstone.tombstone_id.as_str(),
                    tombstone.recorded_at_micros,
                ],
            )
            .await
            .map_err(|error| remote_deletion_error("record remote deletion tombstone", error))?;
        transaction
            .commit()
            .await
            .map_err(|error| remote_deletion_error("commit remote deletion tombstone", error))?;
        Ok(RemoteDeletionTombstoneRecordOutcome::Recorded(tombstone))
    }

    pub async fn transition_remote_deletion_tombstone(
        &self,
        tombstone: &RemoteDeletionTombstone,
        expected: RemoteDeletionCleanupState,
        next: RemoteDeletionCleanupState,
    ) -> Result<RemoteDeletionTombstoneTransitionOutcome> {
        validate_transition(&expected, &next)?;
        let transaction = self.begin_write_transaction().await?;
        let existing = read_tombstone(
            &transaction,
            &tombstone.profile_id,
            tombstone.target,
            tombstone.project_key(),
        )
        .await?
        .ok_or_else(|| {
            remote_deletion_error(
                "transition remote deletion tombstone",
                "tombstone is unavailable",
            )
        })?;
        if existing.tombstone_id != tombstone.tombstone_id {
            transaction.commit().await.map_err(|error| {
                remote_deletion_error("commit remote deletion conflict read", error)
            })?;
            return Ok(RemoteDeletionTombstoneTransitionOutcome::Conflict { existing });
        }
        if existing.cleanup != expected {
            transaction.commit().await.map_err(|error| {
                remote_deletion_error("commit remote deletion state read", error)
            })?;
            return Ok(RemoteDeletionTombstoneTransitionOutcome::StateChanged { existing });
        }
        let (failure_code, failure_phase, retryable) =
            next.failure()
                .map_or((None, None, None), |(code, phase, retryable)| {
                    (
                        Some(code.as_str()),
                        Some(phase.as_str()),
                        Some(i64::from(retryable)),
                    )
                });
        transaction
            .execute(
                "UPDATE remote_deletion_tombstones
                 SET cleanup_status = ?1, failure_code = ?2, failure_phase = ?3, retryable = ?4
                 WHERE profile_id = ?5 AND target_kind = ?6 AND project_id = ?7
                   AND tombstone_id = ?8",
                params![
                    next.status(),
                    failure_code,
                    failure_phase,
                    retryable,
                    tombstone.profile_id.as_str(),
                    tombstone.target.as_str(),
                    tombstone.project_key(),
                    tombstone.tombstone_id.as_str(),
                ],
            )
            .await
            .map_err(|error| {
                remote_deletion_error("transition remote deletion tombstone", error)
            })?;
        transaction.commit().await.map_err(|error| {
            remote_deletion_error("commit remote deletion tombstone transition", error)
        })?;
        let updated = RemoteDeletionTombstone {
            cleanup: next,
            ..existing
        };
        Ok(RemoteDeletionTombstoneTransitionOutcome::Updated(updated))
    }

    pub async fn remote_deletion_tombstone_for_project(
        &self,
        profile_id: &str,
        project_id: &str,
    ) -> Result<Option<RemoteDeletionTombstone>> {
        validate_identifier("profile id", profile_id)?;
        validate_identifier("project id", project_id)?;
        let snapshot = self.read_snapshot().await?;
        if let Some(account) =
            read_tombstone(&snapshot, profile_id, RemoteDeletionTarget::Account, "").await?
        {
            return Ok(Some(account));
        }
        read_tombstone(
            &snapshot,
            profile_id,
            RemoteDeletionTarget::Project,
            project_id,
        )
        .await
    }

    pub async fn remote_account_deletion_tombstone(
        &self,
        profile_id: &str,
    ) -> Result<Option<RemoteDeletionTombstone>> {
        validate_identifier("profile id", profile_id)?;
        let snapshot = self.read_snapshot().await?;
        read_tombstone(&snapshot, profile_id, RemoteDeletionTarget::Account, "").await
    }

    pub async fn delete_remote_deleted_project_registry_row(&self, project_id: &str) -> Result<()> {
        validate_identifier("remote deletion project id", project_id)?;
        let transaction = self.begin_write_transaction().await?;
        transaction
            .execute(
                "DELETE FROM code_projects WHERE project_id = ?1",
                params![project_id],
            )
            .await
            .map_err(|error| {
                remote_deletion_error("remove remote-deleted project registry row", error)
            })?;
        transaction.commit().await.map_err(|error| {
            remote_deletion_error("commit remote-deleted project registry row", error)
        })
    }
}

fn validate_transition(
    expected: &RemoteDeletionCleanupState,
    next: &RemoteDeletionCleanupState,
) -> Result<()> {
    let allowed = match expected {
        RemoteDeletionCleanupState::Deleted => next == &RemoteDeletionCleanupState::Deleted,
        RemoteDeletionCleanupState::Pending
        | RemoteDeletionCleanupState::Settling { .. }
        | RemoteDeletionCleanupState::Partial { .. } => {
            !matches!(next, RemoteDeletionCleanupState::Pending)
        }
    };
    if !allowed {
        return Err(remote_deletion_error(
            "validate remote deletion transition",
            "cleanup state transition is not allowed",
        ));
    }
    Ok(())
}

async fn read_tombstone(
    executor: &impl QueryExecutor,
    profile_id: &str,
    target: RemoteDeletionTarget,
    project_id: &str,
) -> Result<Option<RemoteDeletionTombstone>> {
    let mut rows = executor
        .query(
            "SELECT target_kind, profile_id, project_id, tombstone_id, recorded_at_micros,
                    cleanup_status, failure_code, failure_phase, retryable
             FROM remote_deletion_tombstones
             WHERE profile_id = ?1 AND target_kind = ?2 AND project_id = ?3",
            params![profile_id, target.as_str(), project_id],
        )
        .await
        .map_err(|error| remote_deletion_error("read remote deletion tombstone", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| remote_deletion_error("read remote deletion tombstone row", error))?
    else {
        return Ok(None);
    };
    let target_kind: String = row
        .get(0)
        .map_err(|error| remote_deletion_error("decode remote deletion target", error))?;
    let target = RemoteDeletionTarget::from_str(&target_kind)
        .ok_or_else(|| remote_deletion_error("decode remote deletion target", "unknown target"))?;
    let project_id: String = row
        .get(2)
        .map_err(|error| remote_deletion_error("decode remote deletion project", error))?;
    let cleanup_status: String = row
        .get(5)
        .map_err(|error| remote_deletion_error("decode remote deletion cleanup status", error))?;
    let failure_code: Option<String> = row
        .get(6)
        .map_err(|error| remote_deletion_error("decode remote deletion failure code", error))?;
    let failure_phase: Option<String> = row
        .get(7)
        .map_err(|error| remote_deletion_error("decode remote deletion failure phase", error))?;
    let retryable: Option<i64> = row
        .get(8)
        .map_err(|error| remote_deletion_error("decode remote deletion retryability", error))?;
    Ok(Some(RemoteDeletionTombstone {
        target,
        profile_id: row
            .get(1)
            .map_err(|error| remote_deletion_error("decode remote deletion profile", error))?,
        project_id: (!project_id.is_empty()).then_some(project_id),
        tombstone_id: row
            .get(3)
            .map_err(|error| remote_deletion_error("decode remote deletion id", error))?,
        recorded_at_micros: row
            .get(4)
            .map_err(|error| remote_deletion_error("decode remote deletion timestamp", error))?,
        cleanup: RemoteDeletionCleanupState::decode(
            &cleanup_status,
            failure_code.as_deref(),
            failure_phase.as_deref(),
            retryable,
        )?,
    }))
}

fn validate_identifier(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 256 {
        return Err(remote_deletion_error(
            "validate remote deletion tombstone",
            format!("{field} must be non-empty and at most 256 bytes"),
        ));
    }
    Ok(())
}

fn remote_deletion_error(operation: &str, error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Database {
        operation: operation.to_owned(),
        message: error.to_string(),
    }
}
