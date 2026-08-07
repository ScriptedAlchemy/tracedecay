//! Durable Work placement rows: compare-and-swap publication and the
//! database-enforced exclusivity of a managed target root.
//!
//! The exclusivity rule ("linked and isolated placements are canonical,
//! exclusive, fenced" — Plan 32) is enforced by the partial unique index in
//! `work/schema.rs`, not only by the service that reads
//! [`target_holder`](WorkPlacementStoragePort::target_holder). The read is what
//! produces a *typed* refusal; the index is what makes the rule survive a crash
//! between the read and the write.

use tracedecay_application::{WorkPlacementStorageError, WorkPlacementStoragePort};
use tracedecay_domain::{
    RunId, TaskId, WorkAuthority, WorkPlacementIdentityV1, WorkPlacementKindV1,
    WorkPlacementStateV1, WorkPlacementV1,
};

use crate::exact_sql::ExactSqlValue;
use crate::work::{
    WorkSqliteStorage, authority_params_owned, exact_sql_statement, exact_sql_text,
    registered_work_query,
};

impl WorkPlacementStoragePort for WorkSqliteStorage {
    fn load_placement(
        &self,
        authority: &WorkAuthority,
        identity: &WorkPlacementIdentityV1,
    ) -> Result<Option<WorkPlacementV1>, WorkPlacementStorageError> {
        let rows = registered_work_query(
            &self.handle,
            "SELECT placement_payload FROM work_placements_v1
             WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
               AND actor_id = ?4 AND policy_digest = ?5
               AND task_id = ?6 AND run_id = ?7",
            authority_params_owned(authority)
                .into_iter()
                .chain(identity_params(identity))
                .collect(),
        )
        .map_err(|_| WorkPlacementStorageError::Unavailable)?;
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

    fn target_holder(
        &self,
        authority: &WorkAuthority,
        root: &str,
    ) -> Result<Option<WorkPlacementIdentityV1>, WorkPlacementStorageError> {
        let rows = registered_work_query(
            &self.handle,
            "SELECT task_id, run_id FROM work_placements_v1
             WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
               AND actor_id = ?4 AND policy_digest = ?5
               AND target_root = ?6 AND state IN ('admitted', 'quarantined')",
            authority_params_owned(authority)
                .into_iter()
                .chain([ExactSqlValue::Text(root.to_owned())])
                .collect(),
        )
        .map_err(|_| WorkPlacementStorageError::Unavailable)?;
        let Some(row) = rows.rows.first() else {
            return Ok(None);
        };
        let task_id = exact_sql_text(&row.values, 0).ok_or(WorkPlacementStorageError::Unavailable)?;
        let run_id = exact_sql_text(&row.values, 1).ok_or(WorkPlacementStorageError::Unavailable)?;
        let task_id = TaskId::new(task_id.to_owned())
            .map_err(|_| WorkPlacementStorageError::Unavailable)?;
        let run_id =
            RunId::new(run_id.to_owned()).map_err(|_| WorkPlacementStorageError::Unavailable)?;
        Ok(Some(WorkPlacementIdentityV1::new(task_id, run_id)))
    }

    fn publish_placement(
        &self,
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
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(|_| WorkPlacementStorageError::Unavailable)?;

        let changed = match expected {
            None => transaction
                .execute(
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
                    .execute(
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
            let _ = transaction.rollback();
            return Err(WorkPlacementStorageError::AuthorityConflict);
        }
        transaction
            .commit()
            .map_err(|_| WorkPlacementStorageError::Unavailable)?;
        Ok(())
    }
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
