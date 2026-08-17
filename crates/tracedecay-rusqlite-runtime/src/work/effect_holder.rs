//! Exact Work-attempt effect-holder persistence.

use serde::Deserialize;
use tracedecay_application::{
    WorkAttemptEffectDispatchOutcomeV1, WorkAttemptEffectHolderV1, WorkAttemptEffectResolutionV1,
    WorkAttemptEffectStorageErrorV1, WorkAttemptEffectStoragePortV1,
};
use tracedecay_domain::{UtcMicros, WorkAttemptIdentityV1, WorkAttemptV1, WorkAuthority};

use crate::exact_sql::{ExactSqlTransaction, ExactSqlValue};
use crate::work::{
    RegisteredWorkQuery, WorkSqliteStorage, authority_params_owned, exact_sql_integer,
    exact_sql_statement, exact_sql_text, registered_work_query,
};

type StorageError = WorkAttemptEffectStorageErrorV1;

impl WorkAttemptEffectStoragePortV1 for WorkSqliteStorage {
    fn begin_effect_dispatch(
        &self,
        authority: &WorkAuthority,
        holder: &WorkAttemptEffectHolderV1,
    ) -> Result<WorkAttemptEffectDispatchOutcomeV1, StorageError> {
        holder.validate().map_err(|_| StorageError::Conflict)?;
        let transaction = self
            .handle()
            .begin_immediate()
            .map_err(|_| StorageError::Unavailable)?;
        require_open_attempt(&transaction, authority, holder)?;
        if let Some(existing) = load_holder(&transaction, authority, holder.attempt())? {
            let _ = transaction.rollback();
            return if same_dispatch(&existing, holder) {
                Ok(WorkAttemptEffectDispatchOutcomeV1::Replayed(existing))
            } else {
                Err(StorageError::Conflict)
            };
        }
        insert_holder(&transaction, authority, holder)?;
        transaction
            .commit()
            .map_err(|_| StorageError::Unavailable)?;
        Ok(WorkAttemptEffectDispatchOutcomeV1::Recorded(holder.clone()))
    }

    fn settle_effect_dispatch(
        &self,
        authority: &WorkAuthority,
        attempt: &WorkAttemptIdentityV1,
        resolution: WorkAttemptEffectResolutionV1,
        resolved_at: UtcMicros,
    ) -> Result<WorkAttemptEffectHolderV1, StorageError> {
        let transaction = self
            .handle()
            .begin_immediate()
            .map_err(|_| StorageError::Unavailable)?;
        let existing = load_holder(&transaction, authority, attempt)?
            .ok_or(StorageError::NotFoundOrNotAuthorized)?;
        let next = match existing.resolution() {
            None => existing
                .with_resolution(resolution, resolved_at)
                .map_err(|_| StorageError::Conflict)?,
            Some(current) if current == resolution => {
                transaction
                    .commit()
                    .map_err(|_| StorageError::Unavailable)?;
                return Ok(existing);
            }
            Some(WorkAttemptEffectResolutionV1::Unknown)
                if resolution == WorkAttemptEffectResolutionV1::NoEffect =>
            {
                existing
                    .with_resolution(resolution, resolved_at)
                    .map_err(|_| StorageError::Conflict)?
            }
            Some(_) => {
                let _ = transaction.rollback();
                return Err(StorageError::Conflict);
            }
        };
        update_holder(&transaction, authority, &next)?;
        transaction
            .commit()
            .map_err(|_| StorageError::Unavailable)?;
        Ok(next)
    }

    fn load_effect_dispatch(
        &self,
        authority: &WorkAuthority,
        attempt: &WorkAttemptIdentityV1,
    ) -> Result<Option<WorkAttemptEffectHolderV1>, StorageError> {
        let transaction = self
            .handle()
            .begin_deferred()
            .map_err(|_| StorageError::Unavailable)?;
        let holder = load_holder(&transaction, authority, attempt)?;
        transaction
            .commit()
            .map_err(|_| StorageError::Unavailable)?;
        Ok(holder)
    }
}

fn require_open_attempt(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    holder: &WorkAttemptEffectHolderV1,
) -> Result<(), StorageError> {
    let rows = registered_work_query(
        transaction,
        "SELECT terminal, attempt_payload FROM work_attempts_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5
           AND task_id = ?6 AND run_id = ?7 AND attempt_id = ?8",
        authority_params_owned(authority)
            .into_iter()
            .chain(attempt_params(holder.attempt()))
            .collect(),
    )
    .map_err(|_| StorageError::Unavailable)?;
    let Some(row) = rows.rows.first() else {
        return Err(StorageError::NotFoundOrNotAuthorized);
    };
    match exact_sql_integer(&row.values, 0) {
        Some(0) => {
            let payload = exact_sql_text(&row.values, 1).ok_or(StorageError::Unavailable)?;
            let stored: StoredWorkAttemptV1 =
                serde_json::from_str(payload).map_err(|_| StorageError::Unavailable)?;
            if stored.attempt.identity() == holder.attempt()
                && stored.attempt.execution().effect_state() == holder.effect_state()
            {
                Ok(())
            } else {
                Err(StorageError::Conflict)
            }
        }
        Some(_) => Err(StorageError::Conflict),
        None => Err(StorageError::Unavailable),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredWorkAttemptV1 {
    attempt: WorkAttemptV1,
    #[serde(rename = "synthesis")]
    _synthesis: Option<serde_json::Value>,
}

fn load_holder<T>(
    query: &T,
    authority: &WorkAuthority,
    attempt: &WorkAttemptIdentityV1,
) -> Result<Option<WorkAttemptEffectHolderV1>, StorageError>
where
    T: RegisteredWorkQuery,
{
    let rows = registered_work_query(
        query,
        "SELECT effect_state, dispatched_at, deadline, resolution, resolved_at, holder_payload
         FROM work_attempt_effect_holders_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5
           AND task_id = ?6 AND run_id = ?7 AND attempt_id = ?8",
        authority_params_owned(authority)
            .into_iter()
            .chain(attempt_params(attempt))
            .collect(),
    )
    .map_err(|_| StorageError::Unavailable)?;
    rows.rows
        .first()
        .map(|row| {
            let holder: WorkAttemptEffectHolderV1 = serde_json::from_str(
                exact_sql_text(&row.values, 5).ok_or(StorageError::Unavailable)?,
            )
            .map_err(|_| StorageError::Unavailable)?;
            holder.validate().map_err(|_| StorageError::Unavailable)?;
            let resolved_at_matches = match (holder.resolved_at(), row.values.get(4)) {
                (None, Some(ExactSqlValue::Null)) => true,
                (Some(expected), Some(ExactSqlValue::Integer(actual))) => expected.0 == *actual,
                _ => false,
            };
            if holder.attempt() != attempt
                || exact_sql_text(&row.values, 0) != Some(effect_state(&holder))
                || exact_sql_integer(&row.values, 1) != Some(holder.dispatched_at().0)
                || exact_sql_integer(&row.values, 2) != Some(holder.deadline().0)
                || exact_sql_text(&row.values, 3) != Some(resolution(&holder))
                || !resolved_at_matches
            {
                return Err(StorageError::Unavailable);
            }
            Ok(holder)
        })
        .transpose()
}

fn same_dispatch(
    existing: &WorkAttemptEffectHolderV1,
    proposed: &WorkAttemptEffectHolderV1,
) -> bool {
    existing.attempt() == proposed.attempt()
        && existing.effect_state() == proposed.effect_state()
        && existing.dispatched_at() == proposed.dispatched_at()
        && existing.deadline() == proposed.deadline()
}

fn insert_holder(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    holder: &WorkAttemptEffectHolderV1,
) -> Result<(), StorageError> {
    holder.validate().map_err(|_| StorageError::Conflict)?;
    let payload = serde_json::to_string(holder).map_err(|_| StorageError::Unavailable)?;
    transaction
        .execute(
            exact_sql_statement(
                "INSERT INTO work_attempt_effect_holders_v1 (
                    project_id, repository_id, worktree_id, actor_id, policy_digest,
                    task_id, run_id, attempt_id, effect_state, dispatched_at, deadline,
                    resolution, resolved_at, holder_payload
                 ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14
                 )",
                authority_params_owned(authority)
                    .into_iter()
                    .chain(attempt_params(holder.attempt()))
                    .chain([
                        ExactSqlValue::Text(effect_state(holder).to_owned()),
                        ExactSqlValue::Integer(holder.dispatched_at().0),
                        ExactSqlValue::Integer(holder.deadline().0),
                        ExactSqlValue::Text(resolution(holder).to_owned()),
                        optional_micros(holder.resolved_at()),
                        ExactSqlValue::Text(payload),
                    ])
                    .collect(),
            )
            .map_err(|_| StorageError::Unavailable)?,
        )
        .map_err(|_| StorageError::Unavailable)?;
    Ok(())
}

fn update_holder(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    holder: &WorkAttemptEffectHolderV1,
) -> Result<(), StorageError> {
    holder.validate().map_err(|_| StorageError::Conflict)?;
    let payload = serde_json::to_string(holder).map_err(|_| StorageError::Unavailable)?;
    let changed = transaction
        .execute(
            exact_sql_statement(
                "UPDATE work_attempt_effect_holders_v1
                 SET resolution = ?9, resolved_at = ?10, holder_payload = ?11
                 WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
                   AND actor_id = ?4 AND policy_digest = ?5
                   AND task_id = ?6 AND run_id = ?7 AND attempt_id = ?8",
                authority_params_owned(authority)
                    .into_iter()
                    .chain(attempt_params(holder.attempt()))
                    .chain([
                        ExactSqlValue::Text(resolution(holder).to_owned()),
                        optional_micros(holder.resolved_at()),
                        ExactSqlValue::Text(payload),
                    ])
                    .collect(),
            )
            .map_err(|_| StorageError::Unavailable)?,
        )
        .map_err(|_| StorageError::Unavailable)?;
    if changed.changed_rows == 1 {
        Ok(())
    } else {
        Err(StorageError::NotFoundOrNotAuthorized)
    }
}

fn attempt_params(attempt: &WorkAttemptIdentityV1) -> [ExactSqlValue; 3] {
    [
        ExactSqlValue::Text(attempt.task_id().as_str().to_owned()),
        ExactSqlValue::Text(attempt.run_id().as_str().to_owned()),
        ExactSqlValue::Text(attempt.attempt_id().as_str().to_owned()),
    ]
}

fn effect_state(holder: &WorkAttemptEffectHolderV1) -> &'static str {
    match holder.effect_state() {
        tracedecay_domain::WorkEffectStateV1::Observational => "observational",
        tracedecay_domain::WorkEffectStateV1::Intercepted => "intercepted",
        tracedecay_domain::WorkEffectStateV1::CompoundNonRepeatable => "compound_non_repeatable",
    }
}

fn resolution(holder: &WorkAttemptEffectHolderV1) -> &'static str {
    match holder.resolution() {
        None => "pending",
        Some(WorkAttemptEffectResolutionV1::NoEffect) => "no_effect",
        Some(WorkAttemptEffectResolutionV1::Unknown) => "unknown",
    }
}

fn optional_micros(value: Option<UtcMicros>) -> ExactSqlValue {
    match value {
        Some(value) => ExactSqlValue::Integer(value.0),
        None => ExactSqlValue::Null,
    }
}
