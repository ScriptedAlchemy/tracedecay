//! Concrete SQLite persistence for the application-owned Work authority.

use std::collections::BTreeSet;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension};
use tracedecay_application::{
    WorkAppendOutcome, WorkAppendRequest, WorkAttemptPersistencePort,
    WorkExecutionPersistenceError, WorkProjectionPortError, WorkProjectionReadPort,
    WorkStorageError, WorkStoragePort,
};
use tracedecay_domain::{
    ManifestDigest, ProjectionGenerationId, TaskId, WORK_PROJECTION_STATE_VERSION_V1,
    WorkAttemptIdentityV1, WorkAttemptStateV1, WorkAttemptV1, WorkAuthority, WorkCommandId,
    WorkEvent, WorkProjection, WorkProjectionCoverageV1, WorkProjectionDeltaV1,
    WorkProjectionResumeCursorV1, WorkProjectionSequenceRangeV1, WorkProjectionSequenceV1,
    WorkProjectionSnapshotV1, WorkProjectionStateV1, WorkVersion, canonical_sha256,
};

use crate::exact_sql::{
    ExactSqlHandle, ExactSqlRows, ExactSqlStatement, ExactSqlTransaction, ExactSqlValue,
};

mod attempts;
mod events;
mod projection;
mod schema;
mod sql;

pub use schema::{WORK_SCHEMA_V1, install_work_schema};

pub(crate) use projection::*;
pub(crate) use sql::*;

/// Work persistence over the registered exact-SQL channel.
///
/// This is the only transaction implementation Work has: every append,
/// attempt write, and projection read goes through the same registered
/// handle the daemon binds, so no caller can reach a private connection with
/// different transaction or authority behaviour.
#[derive(Clone)]
pub struct WorkSqliteStorage {
    pub(crate) handle: ExactSqlHandle,
}

impl WorkSqliteStorage {
    pub fn from_registered(handle: ExactSqlHandle) -> Self {
        Self { handle }
    }

    pub fn owner_cursor(
        connection: &Connection,
        authority: &WorkAuthority,
    ) -> rusqlite::Result<u64> {
        let sequence = connection
            .query_row(
                "SELECT sequence
                 FROM work_owner_cursors_v1
                 WHERE project_id = ?1
                   AND repository_id = ?2
                   AND worktree_id = ?3
                   AND actor_id = ?4
                   AND policy_digest = ?5",
                authority_params(authority),
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        u64::try_from(sequence).map_err(|_| invalid_storage("negative Work owner cursor"))
    }

    pub fn resume_cursor(
        snapshot: &WorkProjectionSnapshotV1,
    ) -> Result<WorkProjectionResumeCursorV1, WorkProjectionPortError> {
        projection_cursor(snapshot.generation_id().clone(), snapshot.sequence())
    }
}
