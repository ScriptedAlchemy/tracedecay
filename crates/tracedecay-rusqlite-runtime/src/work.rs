//! Concrete SQLite persistence for the application-owned Work authority.

use std::time::Duration;

use rusqlite::{Connection, OptionalExtension};
use tracedecay_application::{
    WorkAppendOutcome, WorkAppendRequest, WorkStorageError, WorkStoragePort,
};
use tracedecay_domain::{
    TaskId, WorkAuthority, WorkEvent, WorkProjection, WorkProjectionResumeCursorV1,
    WorkProjectionSnapshotV1, WorkVersion,
};

use crate::exact_sql::{
    ExactSqlHandle, ExactSqlRows, ExactSqlStatement, ExactSqlTransaction, ExactSqlValue,
};

mod events;
mod projection;
mod schema;
mod sql;

pub use schema::{WORK_PRODUCT_SCHEMA_V1, WORK_SCHEMA_V1, install_work_schema};

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

    /// Loads every canonical event for one authority in topology-fold order.
    pub fn load_authority_events(
        &self,
        authority: &WorkAuthority,
    ) -> Result<Vec<WorkEvent>, WorkStorageError> {
        events::load_registered_authority_events(&self.handle, authority)
    }

    pub fn resume_cursor(
        snapshot: &WorkProjectionSnapshotV1,
    ) -> Result<WorkProjectionResumeCursorV1, tracedecay_application::WorkProjectionPortError> {
        projection::projection_cursor(snapshot.generation_id().clone(), snapshot.sequence())
    }
}
