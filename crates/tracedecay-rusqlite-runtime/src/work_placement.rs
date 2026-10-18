//! Durable Work placement rows: compare-and-swap publication and the
//! database-enforced exclusivity of a managed target root.
//!
//! Linked and isolated placements are exclusive and fenced. That rule is
//! enforced by the partial unique index in `work/schema.rs`, not only by the
//! service that reads
//! [`target_held_by_other`](WorkPlacementStoragePort::target_held_by_other). The read is what
//! produces a *typed* refusal; the index is what makes the rule survive a crash
//! between the read and the write.

use std::time::Duration;

use tracedecay_contracts::{
    WorkPlacementPublicationPort, WorkPlacementStorageError, WorkPlacementStoragePort,
};
use tracedecay_domain::{
    ProjectId, RepositoryId, WorkAuthority, WorkPlacementIdentityV1, WorkPlacementKindV1,
    WorkPlacementStateV1, WorkPlacementV1,
};

use crate::exact_sql::{ExactSqlRows, ExactSqlStatement, ExactSqlValue};
use crate::work::{
    RegisteredWorkQuery, RegisteredWorkWrite, WorkSqliteStorage, authority_params_owned,
    exact_sql_statement, exact_sql_text, registered_work_query,
};
use crate::work_run_control::WorkSqliteJobStorage;

impl WorkSqliteStorage {
    /// Reads one run's placement on a reader; the awaiting task parks no
    /// thread.
    pub async fn load_placement_async(
        &self,
        authority: &WorkAuthority,
        identity: &WorkPlacementIdentityV1,
    ) -> Result<Option<WorkPlacementV1>, WorkPlacementStorageError> {
        let rows = self
            .handle()
            .query_async(
                load_placement_statement(authority, identity)?,
                Duration::from_secs(5),
            )
            .await
            .map_err(|_| WorkPlacementStorageError::Unavailable)?;
        placement_from_rows(&rows)
    }
}

impl WorkPlacementStoragePort for WorkSqliteJobStorage<'_> {
    fn load_placement(
        &self,
        authority: &WorkAuthority,
        identity: &WorkPlacementIdentityV1,
    ) -> Result<Option<WorkPlacementV1>, WorkPlacementStorageError> {
        load_placement_from(self.transaction, authority, identity)
    }

    fn target_held_by_other(
        &self,
        authority: &WorkAuthority,
        identity: &WorkPlacementIdentityV1,
        root: &str,
    ) -> Result<bool, WorkPlacementStorageError> {
        target_held_by_other_from(self.transaction, authority, identity, root)
    }
}

/// The job's transaction commits the publication, and rolls back a refused
/// compare-and-swap.
impl WorkPlacementPublicationPort for WorkSqliteJobStorage<'_> {
    fn publish_placement(
        &self,
        authority: &WorkAuthority,
        expected: Option<u64>,
        next: &WorkPlacementV1,
    ) -> Result<(), WorkPlacementStorageError> {
        publish_placement_tx(self.transaction, authority, expected, next)
    }
}

impl WorkPlacementStoragePort for WorkSqliteStorage {
    fn load_placement(
        &self,
        authority: &WorkAuthority,
        identity: &WorkPlacementIdentityV1,
    ) -> Result<Option<WorkPlacementV1>, WorkPlacementStorageError> {
        load_placement_from(self.handle(), authority, identity)
    }

    fn target_held_by_other(
        &self,
        authority: &WorkAuthority,
        identity: &WorkPlacementIdentityV1,
        root: &str,
    ) -> Result<bool, WorkPlacementStorageError> {
        target_held_by_other_from(self.handle(), authority, identity, root)
    }

    fn has_target_holder_in_exact_repository_root(
        &self,
        project_id: &ProjectId,
        repository_id: &RepositoryId,
        root: &str,
    ) -> Result<bool, WorkPlacementStorageError> {
        let rows = registered_work_query(
            self.handle(),
            "SELECT task_id FROM work_placements_v1
             WHERE project_id = ?1 AND repository_id = ?2
               AND target_root = ?3 AND state IN ('admitted', 'quarantined')
             LIMIT 1",
            vec![
                ExactSqlValue::Text(project_id.as_str().to_owned()),
                ExactSqlValue::Text(repository_id.as_str().to_owned()),
                ExactSqlValue::Text(root.to_owned()),
            ],
        )
        .map_err(|_| WorkPlacementStorageError::Unavailable)?;
        Ok(!rows.rows.is_empty())
    }
}

fn load_placement_statement(
    authority: &WorkAuthority,
    identity: &WorkPlacementIdentityV1,
) -> Result<ExactSqlStatement, WorkPlacementStorageError> {
    exact_sql_statement(
        "SELECT placement_payload FROM work_placements_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5
           AND task_id = ?6 AND run_id = ?7",
        authority_params_owned(authority)
            .into_iter()
            .chain(identity_params(identity))
            .collect(),
    )
    .map_err(|_| WorkPlacementStorageError::Unavailable)
}

fn placement_from_rows(
    rows: &ExactSqlRows,
) -> Result<Option<WorkPlacementV1>, WorkPlacementStorageError> {
    let Some(payload) = rows
        .rows
        .first()
        .and_then(|row| exact_sql_text(&row.values, 0))
    else {
        return Ok(None);
    };
    serde_json::from_str(payload)
        .map(Some)
        .map_err(|_| WorkPlacementStorageError::Unavailable)
}

fn load_placement_from(
    source: &impl RegisteredWorkQuery,
    authority: &WorkAuthority,
    identity: &WorkPlacementIdentityV1,
) -> Result<Option<WorkPlacementV1>, WorkPlacementStorageError> {
    let rows = source
        .work_query(load_placement_statement(authority, identity)?)
        .map_err(|_| WorkPlacementStorageError::Unavailable)?;
    placement_from_rows(&rows)
}

/// The holder test is scoped by project and repository only. The exclusivity
/// index has the same key, so this read and the write it guards agree on what
/// "the same root" means across actors, policy digests, and worktree routes.
fn target_held_by_other_from(
    source: &impl RegisteredWorkQuery,
    authority: &WorkAuthority,
    identity: &WorkPlacementIdentityV1,
    root: &str,
) -> Result<bool, WorkPlacementStorageError> {
    let rows = registered_work_query(
        source,
        "SELECT 1 FROM work_placements_v1
         WHERE project_id = ?1 AND repository_id = ?2
           AND target_root = ?8 AND state IN ('admitted', 'quarantined')
           AND NOT (worktree_id = ?3 AND actor_id = ?4 AND policy_digest = ?5
                    AND task_id = ?6 AND run_id = ?7)
         LIMIT 1",
        authority_params_owned(authority)
            .into_iter()
            .chain(identity_params(identity))
            .chain([ExactSqlValue::Text(root.to_owned())])
            .collect(),
    )
    .map_err(|_| WorkPlacementStorageError::Unavailable)?;
    Ok(!rows.rows.is_empty())
}

fn publish_placement_tx(
    transaction: &impl RegisteredWorkWrite,
    authority: &WorkAuthority,
    expected: Option<u64>,
    next: &WorkPlacementV1,
) -> Result<(), WorkPlacementStorageError> {
    let payload =
        serde_json::to_string(next).map_err(|_| WorkPlacementStorageError::Unavailable)?;
    let authority_version = i64::try_from(next.authority_version())
        .map_err(|_| WorkPlacementStorageError::Unavailable)?;
    let target_root = next.target().root().map_or(ExactSqlValue::Null, |root| {
        ExactSqlValue::Text(root.to_owned())
    });

    let changed = match expected {
        None => transaction
            .work_execute(
                exact_sql_statement(
                    "INSERT OR IGNORE INTO work_placements_v1 (
                            project_id, repository_id, worktree_id, actor_id, policy_digest,
                            task_id, run_id, kind, target_root, state, authority_version,
                            placement_payload
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    authority_params_owned(authority)
                        .into_iter()
                        .chain(identity_params(next.identity()))
                        .chain([
                            ExactSqlValue::Text(kind_text(next.target().kind())),
                            target_root,
                            ExactSqlValue::Text(state_text(next.state())),
                            ExactSqlValue::Integer(authority_version),
                            ExactSqlValue::Text(payload),
                        ])
                        .collect(),
                )
                .map_err(|_| WorkPlacementStorageError::Unavailable)?,
            )
            // `INSERT OR IGNORE` also absorbs the exclusivity index
            // violation, so a second holder of the same root lands in the
            // same typed conflict as a racing first admission.
            .map_err(|_| WorkPlacementStorageError::Unavailable)?,
        Some(expected) => {
            let expected_version =
                i64::try_from(expected).map_err(|_| WorkPlacementStorageError::Unavailable)?;
            transaction
                .work_execute(
                    exact_sql_statement(
                        "UPDATE work_placements_v1 SET
                                kind = ?8, target_root = ?9, state = ?10,
                                authority_version = ?11, placement_payload = ?12
                             WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
                               AND actor_id = ?4 AND policy_digest = ?5
                               AND task_id = ?6 AND run_id = ?7
                               AND authority_version = ?13",
                        authority_params_owned(authority)
                            .into_iter()
                            .chain(identity_params(next.identity()))
                            .chain([
                                ExactSqlValue::Text(kind_text(next.target().kind())),
                                target_root,
                                ExactSqlValue::Text(state_text(next.state())),
                                ExactSqlValue::Integer(authority_version),
                                ExactSqlValue::Text(payload),
                                ExactSqlValue::Integer(expected_version),
                            ])
                            .collect(),
                    )
                    .map_err(|_| WorkPlacementStorageError::Unavailable)?,
                )
                .map_err(|_| WorkPlacementStorageError::Unavailable)?
        }
    };
    if changed.changed_rows != 1 {
        return Err(WorkPlacementStorageError::AuthorityConflict);
    }
    Ok(())
}

fn identity_params(identity: &WorkPlacementIdentityV1) -> [ExactSqlValue; 2] {
    [
        ExactSqlValue::Text(identity.task_id().as_str().to_owned()),
        ExactSqlValue::Text(identity.run_id().as_str().to_owned()),
    ]
}

fn kind_text(kind: WorkPlacementKindV1) -> String {
    match kind {
        WorkPlacementKindV1::NoManagedPlacement => "no_managed_placement",
        WorkPlacementKindV1::CleanInPlace => "clean_in_place",
        WorkPlacementKindV1::LinkedWorktree => "linked_worktree",
        WorkPlacementKindV1::IsolatedClone => "isolated_clone",
    }
    .to_owned()
}

fn state_text(state: WorkPlacementStateV1) -> String {
    match state {
        WorkPlacementStateV1::Admitted => "admitted",
        WorkPlacementStateV1::Released => "released",
        WorkPlacementStateV1::Quarantined => "quarantined",
    }
    .to_owned()
}
