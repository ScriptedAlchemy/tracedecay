//! Durable remote-deletion tombstones owned by the registered profile store.
//!
//! A tombstone is deliberately retained after its project registry row and
//! profile-sharded data directory are removed. Reopening a stale enrollment
//! marker must therefore fail closed instead of recreating remote data.

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

/// One retained deletion fact for an authenticated profile.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct RemoteDeletionTombstone {
    pub target: RemoteDeletionTarget,
    pub profile_id: String,
    /// Present only for a project deletion. Account tombstones use the empty
    /// database key so SQLite's composite primary key remains non-null.
    pub project_id: Option<String>,
    pub tombstone_id: String,
    pub recorded_at_micros: i64,
}

impl RemoteDeletionTombstone {
    pub fn validate(&self) -> Result<()> {
        validate_identifier("remote deletion profile id", &self.profile_id)?;
        validate_identifier("remote deletion tombstone id", &self.tombstone_id)?;
        if self.recorded_at_micros <= 0 {
            return Err(remote_deletion_error(
                "validate remote deletion tombstone",
                "remote deletion timestamp must be positive",
            ));
        }
        match (&self.target, &self.project_id) {
            (RemoteDeletionTarget::Account, None) => Ok(()),
            (RemoteDeletionTarget::Project, Some(project_id)) => {
                validate_identifier("remote deletion project id", project_id)
            }
            (RemoteDeletionTarget::Account, Some(_)) => Err(remote_deletion_error(
                "validate remote deletion tombstone",
                "account tombstones must not name a project",
            )),
            (RemoteDeletionTarget::Project, None) => Err(remote_deletion_error(
                "validate remote deletion tombstone",
                "project tombstones require a project id",
            )),
        }
    }

    fn project_key(&self) -> &str {
        match self.project_id.as_deref() {
            Some(project_id) => project_id,
            None => "",
        }
    }
}

impl RegisteredGlobalDb {
    /// Retain a canonical tombstone. Repeated requests for the same target are
    /// idempotent and return the first persisted receipt exactly.
    pub async fn record_remote_deletion_tombstone(
        &self,
        tombstone: RemoteDeletionTombstone,
    ) -> Result<RemoteDeletionTombstone> {
        tombstone.validate()?;
        let transaction = self.begin_write_transaction().await?;
        let existing = read_tombstone(
            &transaction,
            &tombstone.profile_id,
            tombstone.target,
            tombstone.project_key(),
        )
        .await?;
        if let Some(existing) = existing {
            transaction.commit().await.map_err(|error| {
                remote_deletion_error("commit remote deletion tombstone replay", error)
            })?;
            return Ok(existing);
        }
        transaction
            .execute(
                "INSERT INTO remote_deletion_tombstones
                    (profile_id, target_kind, project_id, tombstone_id, recorded_at_micros)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
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
        Ok(tombstone)
    }

    /// Returns the account tombstone first, then the exact project tombstone.
    /// This is the replay fence used before any persisted enrollment is opened.
    pub async fn remote_deletion_tombstone_for_project(
        &self,
        profile_id: &str,
        project_id: &str,
    ) -> Result<Option<RemoteDeletionTombstone>> {
        validate_identifier("remote deletion profile id", profile_id)?;
        validate_identifier("remote deletion project id", project_id)?;
        let snapshot = self.read_snapshot().await?;
        if let Some(tombstone) =
            read_tombstone(&snapshot, profile_id, RemoteDeletionTarget::Account, "").await?
        {
            return Ok(Some(tombstone));
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
        validate_identifier("remote deletion profile id", profile_id)?;
        let snapshot = self.read_snapshot().await?;
        read_tombstone(&snapshot, profile_id, RemoteDeletionTarget::Account, "").await
    }

    /// Delete one derived project-registry row after its tombstone is durable
    /// and the exact profile shard has been removed.
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

async fn read_tombstone(
    executor: &impl QueryExecutor,
    profile_id: &str,
    target: RemoteDeletionTarget,
    project_id: &str,
) -> Result<Option<RemoteDeletionTombstone>> {
    let mut rows = executor
        .query(
            "SELECT target_kind, profile_id, project_id, tombstone_id, recorded_at_micros
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
    let target = RemoteDeletionTarget::from_str(&target_kind).ok_or_else(|| {
        remote_deletion_error("decode remote deletion target", "unknown target kind")
    })?;
    let project_id: String = row
        .get(2)
        .map_err(|error| remote_deletion_error("decode remote deletion project id", error))?;
    let tombstone = RemoteDeletionTombstone {
        target,
        profile_id: row
            .get(1)
            .map_err(|error| remote_deletion_error("decode remote deletion profile id", error))?,
        project_id: (!project_id.is_empty()).then_some(project_id),
        tombstone_id: row
            .get(3)
            .map_err(|error| remote_deletion_error("decode remote deletion id", error))?,
        recorded_at_micros: row
            .get(4)
            .map_err(|error| remote_deletion_error("decode remote deletion timestamp", error))?,
    };
    tombstone.validate()?;
    Ok(Some(tombstone))
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
