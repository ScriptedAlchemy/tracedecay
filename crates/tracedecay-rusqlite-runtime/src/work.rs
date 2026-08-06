//! Concrete SQLite persistence for the application-owned Work authority.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension};
use tracedecay_application::{
    WorkAppendOutcome, WorkAppendRequest, WorkAttemptPersistencePort,
    WorkExecutionPersistenceError, WorkProjectionPortError, WorkProjectionReadPort,
    WorkStorageError, WorkStoragePort,
};
use tracedecay_domain::{
    GitGraphEvidenceIntent, GitGraphEvidencePublicationReceipt, GitGraphEvidenceTarget, GitOidV1,
    ManifestDigest, ProjectionGenerationId, TaskId, WorkAttemptIdentityV1, WorkAttemptStateV1,
    WorkAttemptV1, WorkAuthority, WorkCommandId, WorkEvent, WorkProjection,
    WorkProjectionCoverageV1, WorkProjectionDeltaV1, WorkProjectionResumeCursorV1,
    WorkProjectionSequenceRangeV1, WorkProjectionSequenceV1, WorkProjectionSnapshotV1,
    WorkProjectionStateV1, WorkVersion, canonical_sha256,
};
use tracedecay_graph_db::GraphDb;

use crate::exact_sql::{
    ExactSqlHandle, ExactSqlRows, ExactSqlStatement, ExactSqlTransaction, ExactSqlValue,
};

mod attempts;
mod events;
mod git_evidence;
mod projection;
mod schema;
mod sql;
pub mod topology;

pub use git_evidence::WorkGitGraphEvidenceJournalEntry;
pub use schema::{WORK_SCHEMA_V1, install_work_schema};

pub(crate) use projection::*;
pub(crate) use sql::*;

pub trait WorkGitGraphEvidenceNotifier: Send + Sync {
    fn notify(&self);
}

/// Work persistence over the registered exact-SQL channel.
///
/// This is the only transaction implementation Work has: every append,
/// attempt write, and projection read goes through the same registered
/// handle the daemon binds, so no caller can reach a private connection with
/// different transaction or authority behaviour.
#[derive(Clone)]
pub struct WorkSqliteStorage {
    pub(crate) handle: ExactSqlHandle,
    pub(crate) topology: topology::WorkGraphTopologyStore,
    git_graph_evidence_notifier: Option<Arc<dyn WorkGitGraphEvidenceNotifier>>,
}

impl WorkSqliteStorage {
    pub fn from_registered(handle: ExactSqlHandle, graph: GraphDb) -> Self {
        Self {
            handle,
            topology: topology::WorkGraphTopologyStore::new(graph),
            git_graph_evidence_notifier: None,
        }
    }

    #[must_use]
    pub fn with_git_graph_evidence_notifier(
        mut self,
        notifier: Arc<dyn WorkGitGraphEvidenceNotifier>,
    ) -> Self {
        self.git_graph_evidence_notifier = Some(notifier);
        self
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

    pub fn reconcile_graph_publications(
        &self,
        authority: &WorkAuthority,
    ) -> Result<(), WorkStorageError> {
        events::reconcile_graph_publications(&self.handle, &self.topology, authority)
    }

    pub fn pending_git_graph_evidence(
        &self,
        authority: &WorkAuthority,
        limit: u32,
    ) -> Result<Vec<WorkGitGraphEvidenceJournalEntry>, WorkExecutionPersistenceError> {
        git_evidence::pending(&self.handle, authority, limit)
            .map_err(attempts::map_execution_persistence)
    }

    pub fn acknowledge_git_graph_evidence(
        &self,
        authority: &WorkAuthority,
        journal_sequence: u64,
        receipt: &GitGraphEvidencePublicationReceipt,
    ) -> Result<(), WorkExecutionPersistenceError> {
        git_evidence::acknowledge(&self.handle, authority, journal_sequence, receipt)
            .map_err(attempts::map_execution_persistence)
    }
}
