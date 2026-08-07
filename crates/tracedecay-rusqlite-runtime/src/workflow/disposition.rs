//! Transactional workflow-definition lifecycle dispositions.
//!
//! Plan 32 ("Typed workflow definitions") keeps `candidate`, `validate`,
//! `activate`, `retire`, and `reject` as retained lifecycle operations over
//! immutable definition versions. The definition payload never changes —
//! "Editing creates a new version; admitted runs remain pinned" — so the
//! disposition is a separate compare-and-swap aggregate, and every state a
//! transition passes through is appended to an immutable journal.

use tracedecay_application::{
    WorkflowDefinitionDisposition, WorkflowDefinitionLifecycleCommand,
    WorkflowDefinitionLifecycleState, WorkflowDefinitionTransitionEntry,
    WorkflowDefinitionTransitionOutcome,
};
use tracedecay_domain::{UtcMicros, WorkflowDefinitionId};

use crate::exact_sql::{ExactSqlError, ExactSqlRow, ExactSqlTransaction, ExactSqlValue};

use super::{execute_tx, execute_tx_changed, query_tx, sql_integer, sql_text, version_i64};

/// Failure decoding or applying a stored lifecycle disposition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DispositionError {
    Corrupt,
    Sql,
}

impl From<ExactSqlError> for DispositionError {
    fn from(_: ExactSqlError) -> Self {
        Self::Sql
    }
}

const DISPOSITION_SELECT: &str = "SELECT state, revision, transitioned_at
 FROM workflow_definition_disposition
 WHERE definition_id = ?1 AND definition_version = ?2";

const TRANSITION_SELECT: &str = "SELECT to_revision, from_revision, operation,
        from_state, to_state, transitioned_at
 FROM workflow_definition_transition_journal
 WHERE definition_id = ?1 AND definition_version = ?2
 ORDER BY to_revision";

/// Seeds the `candidate` disposition a freshly registered version starts in.
///
/// Registration is the only writer of revision 1, and replayed registration of
/// a byte-identical definition must not disturb an already advanced
/// disposition, so the insert is unconditionally ignored when a row exists.
pub(super) fn seed_candidate_disposition(
    transaction: &ExactSqlTransaction,
    definition_id: &WorkflowDefinitionId,
    definition_version: u64,
    registered_at: UtcMicros,
) -> Result<(), DispositionError> {
    let version = version_i64(definition_version).map_err(|()| DispositionError::Corrupt)?;
    execute_tx(
        transaction,
        "INSERT OR IGNORE INTO workflow_definition_disposition (
             definition_id, definition_version, state, revision, transitioned_at
         ) VALUES (?1, ?2, 'candidate', 1, ?3)",
        vec![
            ExactSqlValue::Text(definition_id.as_str().to_owned()),
            ExactSqlValue::Integer(version),
            ExactSqlValue::Integer(registered_at.0),
        ],
    )?;
    Ok(())
}

pub(super) fn load_disposition_tx(
    transaction: &ExactSqlTransaction,
    definition_id: &WorkflowDefinitionId,
    definition_version: u64,
) -> Result<Option<WorkflowDefinitionDisposition>, DispositionError> {
    let version = version_i64(definition_version).map_err(|()| DispositionError::Corrupt)?;
    let rows = query_tx(
        transaction,
        DISPOSITION_SELECT,
        vec![
            ExactSqlValue::Text(definition_id.as_str().to_owned()),
            ExactSqlValue::Integer(version),
        ],
    )?;
    let Some(row) = rows.rows.first() else {
        return Ok(None);
    };
    decode_disposition(definition_id, definition_version, row).map(Some)
}

pub(super) fn transition_history_tx(
    transaction: &ExactSqlTransaction,
    definition_id: &WorkflowDefinitionId,
    definition_version: u64,
) -> Result<Vec<WorkflowDefinitionTransitionEntry>, DispositionError> {
    let version = version_i64(definition_version).map_err(|()| DispositionError::Corrupt)?;
    let rows = query_tx(
        transaction,
        TRANSITION_SELECT,
        vec![
            ExactSqlValue::Text(definition_id.as_str().to_owned()),
            ExactSqlValue::Integer(version),
        ],
    )?;
    rows.rows
        .iter()
        .map(|row| decode_transition(definition_id, definition_version, row))
        .collect()
}

/// Applies one compare-and-swap lifecycle transition.
///
/// The stored revision must equal `command.expected_revision`; a mismatch that
/// the immutable journal already attributes to this exact command is a replay
/// and returns the stored disposition unchanged, and any other mismatch is a
/// typed revision conflict. Every state on the operation's path gets its own
/// journal entry before the disposition is swapped.
pub(super) fn apply_lifecycle_transition(
    transaction: &ExactSqlTransaction,
    command: &WorkflowDefinitionLifecycleCommand,
) -> Result<WorkflowDefinitionTransitionOutcome, DispositionError> {
    let version =
        version_i64(command.definition_version).map_err(|()| DispositionError::Corrupt)?;
    let Some(current) = load_disposition_tx(
        transaction,
        &command.definition_id,
        command.definition_version,
    )?
    else {
        return Ok(WorkflowDefinitionTransitionOutcome::Missing);
    };
    if current.revision != command.expected_revision {
        let replayed = query_tx(
            transaction,
            "SELECT 1 FROM workflow_definition_transition_journal
             WHERE definition_id = ?1 AND definition_version = ?2
               AND from_revision = ?3 AND operation = ?4",
            vec![
                ExactSqlValue::Text(command.definition_id.as_str().to_owned()),
                ExactSqlValue::Integer(version),
                ExactSqlValue::Integer(
                    i64::try_from(command.expected_revision)
                        .map_err(|_| DispositionError::Corrupt)?,
                ),
                ExactSqlValue::Text(command.operation.as_str().to_owned()),
            ],
        )?;
        return Ok(if replayed.rows.is_empty() {
            WorkflowDefinitionTransitionOutcome::RevisionConflict(current)
        } else {
            WorkflowDefinitionTransitionOutcome::Replayed(current)
        });
    }
    let Some(path) = command.operation.path_from(current.state) else {
        return Ok(WorkflowDefinitionTransitionOutcome::IllegalTransition(
            current,
        ));
    };

    let mut state = current.state;
    let mut revision = current.revision;
    for next in path {
        let from_revision = i64::try_from(revision).map_err(|_| DispositionError::Corrupt)?;
        revision = revision.checked_add(1).ok_or(DispositionError::Corrupt)?;
        let to_revision = i64::try_from(revision).map_err(|_| DispositionError::Corrupt)?;
        execute_tx(
            transaction,
            "INSERT INTO workflow_definition_transition_journal (
                 definition_id, definition_version, to_revision, from_revision,
                 operation, from_state, to_state, transitioned_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            vec![
                ExactSqlValue::Text(command.definition_id.as_str().to_owned()),
                ExactSqlValue::Integer(version),
                ExactSqlValue::Integer(to_revision),
                ExactSqlValue::Integer(from_revision),
                ExactSqlValue::Text(command.operation.as_str().to_owned()),
                ExactSqlValue::Text(state.as_str().to_owned()),
                ExactSqlValue::Text(next.as_str().to_owned()),
                ExactSqlValue::Integer(command.transitioned_at.0),
            ],
        )?;
        state = *next;
    }

    let swapped = execute_tx_changed(
        transaction,
        "UPDATE workflow_definition_disposition
         SET state = ?3, revision = ?4, transitioned_at = ?5
         WHERE definition_id = ?1 AND definition_version = ?2 AND revision = ?6",
        vec![
            ExactSqlValue::Text(command.definition_id.as_str().to_owned()),
            ExactSqlValue::Integer(version),
            ExactSqlValue::Text(state.as_str().to_owned()),
            ExactSqlValue::Integer(i64::try_from(revision).map_err(|_| DispositionError::Corrupt)?),
            ExactSqlValue::Integer(command.transitioned_at.0),
            ExactSqlValue::Integer(
                i64::try_from(current.revision).map_err(|_| DispositionError::Corrupt)?,
            ),
        ],
    )?;
    if swapped != 1 {
        return Err(DispositionError::Corrupt);
    }
    Ok(WorkflowDefinitionTransitionOutcome::Applied(
        WorkflowDefinitionDisposition {
            definition_id: command.definition_id.clone(),
            definition_version: command.definition_version,
            state,
            revision,
            transitioned_at: command.transitioned_at,
        },
    ))
}

fn decode_disposition(
    definition_id: &WorkflowDefinitionId,
    definition_version: u64,
    row: &ExactSqlRow,
) -> Result<WorkflowDefinitionDisposition, DispositionError> {
    let state = decode_state(sql_text(&row.values, 0))?;
    let revision = decode_revision(sql_integer(&row.values, 1))?;
    let transitioned_at = sql_integer(&row.values, 2).ok_or(DispositionError::Corrupt)?;
    Ok(WorkflowDefinitionDisposition {
        definition_id: definition_id.clone(),
        definition_version,
        state,
        revision,
        transitioned_at: UtcMicros(transitioned_at),
    })
}

fn decode_transition(
    definition_id: &WorkflowDefinitionId,
    definition_version: u64,
    row: &ExactSqlRow,
) -> Result<WorkflowDefinitionTransitionEntry, DispositionError> {
    let to_revision = decode_revision(sql_integer(&row.values, 0))?;
    let from_revision = decode_revision(sql_integer(&row.values, 1))?;
    let operation = sql_text(&row.values, 2)
        .and_then(tracedecay_application::WorkflowLifecycleOperation::from_operation_key)
        .ok_or(DispositionError::Corrupt)?;
    let from_state = decode_state(sql_text(&row.values, 3))?;
    let to_state = decode_state(sql_text(&row.values, 4))?;
    let transitioned_at = sql_integer(&row.values, 5).ok_or(DispositionError::Corrupt)?;
    Ok(WorkflowDefinitionTransitionEntry {
        definition_id: definition_id.clone(),
        definition_version,
        operation,
        from_state,
        to_state,
        from_revision,
        to_revision,
        transitioned_at: UtcMicros(transitioned_at),
    })
}

fn decode_state(value: Option<&str>) -> Result<WorkflowDefinitionLifecycleState, DispositionError> {
    value
        .and_then(WorkflowDefinitionLifecycleState::from_state_key)
        .ok_or(DispositionError::Corrupt)
}

fn decode_revision(value: Option<i64>) -> Result<u64, DispositionError> {
    value
        .filter(|revision| *revision > 0)
        .map(|revision| revision as u64)
        .ok_or(DispositionError::Corrupt)
}
