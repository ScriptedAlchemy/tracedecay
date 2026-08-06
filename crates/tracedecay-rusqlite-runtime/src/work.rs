//! Concrete SQLite persistence for the application-owned Work authority.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension};
use tracedecay_application::{
    WorkAppendOutcome, WorkAppendRequest, WorkAttemptPersistencePort,
    WorkExecutionPersistenceError, WorkProjectionPortError, WorkProjectionReadPort,
    WorkStorageError, WorkStoragePort,
};
use tracedecay_domain::{
    ManifestDigest, ProjectionGenerationId, TaskId, WORK_PROJECTION_STATE_VERSION_V1,
    WorkArtifactRefV1, WorkAttemptIdentityV1, WorkAttemptStateV1, WorkAttemptV1, WorkAuthority,
    WorkCommandId, WorkEvent, WorkProjection, WorkProjectionCoverageV1, WorkProjectionDeltaV1,
    WorkProjectionResumeCursorV1, WorkProjectionSequenceRangeV1, WorkProjectionSequenceV1,
    WorkProjectionSnapshotV1, WorkProjectionStateV1, WorkVersion, canonical_sha256,
    work_artifact_payload_digest,
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
    exact_schema: ExactWorkSchemaV2,
}

impl WorkSqliteStorage {
    pub fn from_registered(handle: ExactSqlHandle, exact_schema: ExactWorkSchemaV2) -> Self {
        Self {
            handle,
            exact_schema,
        }
    }

    pub fn require_exact_schema(&self) -> Result<ExactWorkSchemaV2, WorkSchemaCapabilityErrorV2> {
        Ok(self.exact_schema.clone())
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExactWorkSchemaV2 {
    catalog_fingerprint: Arc<str>,
}

impl ExactWorkSchemaV2 {
    pub fn from_validated_registered_store(
        catalog_fingerprint: impl Into<String>,
    ) -> Result<Self, WorkSchemaCapabilityErrorV2> {
        let catalog_fingerprint = catalog_fingerprint.into();
        if catalog_fingerprint.len() != 64
            || !catalog_fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(WorkSchemaCapabilityErrorV2::InvalidCatalogFingerprint);
        }
        Ok(Self {
            catalog_fingerprint: Arc::from(catalog_fingerprint),
        })
    }

    pub fn catalog_fingerprint(&self) -> &str {
        &self.catalog_fingerprint
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkSchemaCapabilityErrorV2 {
    InvalidCatalogFingerprint,
}
