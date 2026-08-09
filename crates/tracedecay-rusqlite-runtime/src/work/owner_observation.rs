//! Pending scan and exact CAS for Work-owned observability source markers.

use std::num::NonZeroU16;

use tracedecay_application::{
    PendingWorkOwnerObservationV1, WorkOwnerObservationKindV1, WorkOwnerObservationMarkOutcomeV1,
    WorkOwnerObservationMarkerV1, WorkOwnerObservationReceiptV1, WorkOwnerObservationScanCursorV1,
    WorkOwnerObservationStorageErrorV1, WorkOwnerObservationStoragePortV1,
};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, RepositoryId, WorkAuthority, WorkCommandId, WorktreeId,
};

use crate::exact_sql::ExactSqlValue;

use super::{
    WorkSqliteStorage, authority_params_owned, exact_sql_integer, exact_sql_statement,
    exact_sql_text, registered_work_query,
};

type StorageError = WorkOwnerObservationStorageErrorV1;

impl WorkOwnerObservationStoragePortV1 for WorkSqliteStorage {
    fn pending_owner_observations(
        &self,
        after: Option<&WorkOwnerObservationScanCursorV1>,
        limit: NonZeroU16,
    ) -> Result<Vec<PendingWorkOwnerObservationV1>, StorageError> {
        let rows = registered_work_query(
            &self.handle,
            "SELECT kind, project_id, repository_id, worktree_id, actor_id, policy_digest,
                    command_id, receipt_revision, receipt_digest, receipt_payload, ordered_at
             FROM (
                 SELECT 'retry' AS kind, project_id, repository_id, worktree_id, actor_id,
                        policy_digest, command_id, 1 AS receipt_revision, receipt_digest,
                        receipt_payload, restarted_at AS ordered_at
                 FROM work_retry_receipts_v1 WHERE observation_state = 'pending'
                 UNION ALL
                 SELECT 'leak' AS kind, project_id, repository_id, worktree_id, actor_id,
                        policy_digest, command_id, revision AS receipt_revision, receipt_digest,
                        receipt_payload, observed_at AS ordered_at
                 FROM work_leak_adjudications_v1 WHERE observation_state = 'pending'
                 UNION ALL
                 SELECT 'duplicate' AS kind, project_id, repository_id, worktree_id, actor_id,
                        policy_digest, command_id, revision AS receipt_revision, receipt_digest,
                        receipt_payload, occurred_at AS ordered_at
                 FROM work_duplicate_adjudications_v1 WHERE observation_state = 'pending'
             )
             WHERE ?1 IS NULL OR (
                 ordered_at, kind, command_id, project_id, repository_id,
                 worktree_id, actor_id, policy_digest
             ) > (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ORDER BY ordered_at, kind, command_id, project_id, repository_id,
                      worktree_id, actor_id, policy_digest
             LIMIT ?9",
            vec![
                after
                    .map(|cursor| ExactSqlValue::Integer(cursor.ordered_at_micros))
                    .unwrap_or(ExactSqlValue::Null),
                after
                    .map(|cursor| ExactSqlValue::Text(kind_text(cursor.kind).to_owned()))
                    .unwrap_or(ExactSqlValue::Null),
                after
                    .map(|cursor| ExactSqlValue::Text(cursor.command_id.as_str().to_owned()))
                    .unwrap_or(ExactSqlValue::Null),
                after
                    .map(|cursor| {
                        ExactSqlValue::Text(cursor.authority.project_id().as_str().to_owned())
                    })
                    .unwrap_or(ExactSqlValue::Null),
                after
                    .map(|cursor| {
                        ExactSqlValue::Text(cursor.authority.repository_id().as_str().to_owned())
                    })
                    .unwrap_or(ExactSqlValue::Null),
                after
                    .map(|cursor| {
                        ExactSqlValue::Text(cursor.authority.worktree_id().as_str().to_owned())
                    })
                    .unwrap_or(ExactSqlValue::Null),
                after
                    .map(|cursor| {
                        ExactSqlValue::Text(cursor.authority.actor_id().as_str().to_owned())
                    })
                    .unwrap_or(ExactSqlValue::Null),
                after
                    .map(|cursor| {
                        ExactSqlValue::Text(cursor.authority.policy_digest().as_str().to_owned())
                    })
                    .unwrap_or(ExactSqlValue::Null),
                ExactSqlValue::Integer(i64::from(limit.get())),
            ],
        )
        .map_err(|_| StorageError::Unavailable)?;
        rows.rows
            .iter()
            .map(|row| decode_pending(&row.values))
            .collect()
    }

    fn mark_owner_observation_durable(
        &self,
        marker: &WorkOwnerObservationMarkerV1,
    ) -> Result<WorkOwnerObservationMarkOutcomeV1, StorageError> {
        if marker.receipt_revision == 0 || marker.receipt_digest.validate().is_err() {
            return Err(StorageError::Conflict);
        }
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(|_| StorageError::Unavailable)?;
        let (update, query) = marker_statements(marker)?;
        let changed = transaction
            .execute(update)
            .map_err(|_| StorageError::Unavailable)?;
        if changed.changed_rows == 1 {
            transaction
                .commit()
                .map_err(|_| StorageError::Unavailable)?;
            return Ok(WorkOwnerObservationMarkOutcomeV1::Marked);
        }
        let rows = registered_work_query(&transaction, &query.0, query.1)
            .map_err(|_| StorageError::Unavailable)?;
        let replayed = rows.rows.first().is_some_and(|row| {
            exact_sql_text(&row.values, 0) == Some("durable")
                && exact_sql_text(&row.values, 1) == Some(marker.receipt_digest.as_str())
                && exact_sql_integer(&row.values, 2).and_then(|value| u64::try_from(value).ok())
                    == Some(marker.receipt_revision)
        });
        let _ = transaction.rollback();
        if replayed {
            Ok(WorkOwnerObservationMarkOutcomeV1::Replayed)
        } else {
            Err(StorageError::Conflict)
        }
    }
}

fn decode_pending(values: &[ExactSqlValue]) -> Result<PendingWorkOwnerObservationV1, StorageError> {
    let text = |index| {
        exact_sql_text(values, index)
            .map(str::to_owned)
            .ok_or(StorageError::Unavailable)
    };
    let kind = match text(0)?.as_str() {
        "retry" => WorkOwnerObservationKindV1::Retry,
        "leak" => WorkOwnerObservationKindV1::Leak,
        "duplicate" => WorkOwnerObservationKindV1::Duplicate,
        _ => return Err(StorageError::Unavailable),
    };
    let authority = WorkAuthority::new(
        ProjectId::new(text(1)?).map_err(|_| StorageError::Unavailable)?,
        RepositoryId::new(text(2)?).map_err(|_| StorageError::Unavailable)?,
        WorktreeId::new(text(3)?).map_err(|_| StorageError::Unavailable)?,
        ActorId::new(text(4)?).map_err(|_| StorageError::Unavailable)?,
        ManifestDigest::new(text(5)?).map_err(|_| StorageError::Unavailable)?,
    )
    .map_err(|_| StorageError::Unavailable)?;
    let command_id = WorkCommandId::new(text(6)?).map_err(|_| StorageError::Unavailable)?;
    let receipt_revision = exact_sql_integer(values, 7)
        .and_then(|value| u64::try_from(value).ok())
        .filter(|revision| *revision > 0)
        .ok_or(StorageError::Unavailable)?;
    let receipt_digest = ManifestDigest::new(text(8)?).map_err(|_| StorageError::Unavailable)?;
    let payload = text(9)?;
    let receipt = match kind {
        WorkOwnerObservationKindV1::Retry => WorkOwnerObservationReceiptV1::Retry(
            serde_json::from_str(&payload).map_err(|_| StorageError::Unavailable)?,
        ),
        WorkOwnerObservationKindV1::Leak => WorkOwnerObservationReceiptV1::Leak(
            serde_json::from_str(&payload).map_err(|_| StorageError::Unavailable)?,
        ),
        WorkOwnerObservationKindV1::Duplicate => WorkOwnerObservationReceiptV1::Duplicate(
            serde_json::from_str(&payload).map_err(|_| StorageError::Unavailable)?,
        ),
    };
    let ordered_at_micros = exact_sql_integer(values, 10).ok_or(StorageError::Unavailable)?;
    let pending = PendingWorkOwnerObservationV1 {
        scan_cursor: WorkOwnerObservationScanCursorV1 {
            ordered_at_micros,
            kind,
            command_id: command_id.clone(),
            authority: authority.clone(),
        },
        marker: WorkOwnerObservationMarkerV1 {
            kind,
            authority,
            command_id,
            receipt_revision,
            receipt_digest,
        },
        receipt,
    };
    if pending.validate() {
        Ok(pending)
    } else {
        Err(StorageError::Unavailable)
    }
}

const fn kind_text(kind: WorkOwnerObservationKindV1) -> &'static str {
    match kind {
        WorkOwnerObservationKindV1::Retry => "retry",
        WorkOwnerObservationKindV1::Leak => "leak",
        WorkOwnerObservationKindV1::Duplicate => "duplicate",
    }
}

type MarkerQuery = (String, Vec<ExactSqlValue>);

fn marker_statements(
    marker: &WorkOwnerObservationMarkerV1,
) -> Result<(crate::exact_sql::ExactSqlStatement, MarkerQuery), StorageError> {
    let mut params = authority_params_owned(&marker.authority);
    params.extend([
        ExactSqlValue::Text(marker.command_id.as_str().to_owned()),
        ExactSqlValue::Text(marker.receipt_digest.as_str().to_owned()),
    ]);
    Ok(match marker.kind {
        WorkOwnerObservationKindV1::Retry => (
            exact_sql_statement(
                "UPDATE work_retry_receipts_v1 SET observation_state = 'durable'
                 WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
                   AND actor_id = ?4 AND policy_digest = ?5 AND command_id = ?6
                   AND receipt_digest = ?7 AND observation_state = 'pending'",
                params.clone(),
            )
            .map_err(|_| StorageError::Unavailable)?,
            (
                "SELECT observation_state, receipt_digest, 1
                 FROM work_retry_receipts_v1
                 WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
                   AND actor_id = ?4 AND policy_digest = ?5 AND command_id = ?6"
                    .to_owned(),
                params[..6].to_vec(),
            ),
        ),
        WorkOwnerObservationKindV1::Leak => {
            params.push(ExactSqlValue::Integer(
                i64::try_from(marker.receipt_revision).map_err(|_| StorageError::Conflict)?,
            ));
            (
                exact_sql_statement(
                    "UPDATE work_leak_adjudications_v1 SET observation_state = 'durable'
                     WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
                       AND actor_id = ?4 AND policy_digest = ?5 AND command_id = ?6
                       AND receipt_digest = ?7 AND revision = ?8
                       AND observation_state = 'pending'",
                    params.clone(),
                )
                .map_err(|_| StorageError::Unavailable)?,
                (
                    "SELECT observation_state, receipt_digest, revision
                     FROM work_leak_adjudications_v1
                     WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
                       AND actor_id = ?4 AND policy_digest = ?5 AND command_id = ?6"
                        .to_owned(),
                    params[..6].to_vec(),
                ),
            )
        }
        WorkOwnerObservationKindV1::Duplicate => {
            params.push(ExactSqlValue::Integer(
                i64::try_from(marker.receipt_revision).map_err(|_| StorageError::Conflict)?,
            ));
            (
                exact_sql_statement(
                    "UPDATE work_duplicate_adjudications_v1 SET observation_state = 'durable'
                     WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
                       AND actor_id = ?4 AND policy_digest = ?5 AND command_id = ?6
                       AND receipt_digest = ?7 AND revision = ?8
                       AND observation_state = 'pending'",
                    params.clone(),
                )
                .map_err(|_| StorageError::Unavailable)?,
                (
                    "SELECT observation_state, receipt_digest, revision
                     FROM work_duplicate_adjudications_v1
                     WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
                       AND actor_id = ?4 AND policy_digest = ?5 AND command_id = ?6"
                        .to_owned(),
                    params[..6].to_vec(),
                ),
            )
        }
    })
}
