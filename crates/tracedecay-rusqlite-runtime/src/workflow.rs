//! Durable workflow authority over the canonical Work registered SQL channel.
//!
//! Definitions, handoffs, and run events share the exact registered writer
//! owned by `WorkSqliteStorage`. Fresh stores install the final schema as one
//! Work schema; attaching this authority never creates or migrates tables.

use std::time::Duration;

use tracedecay_application::{
    TaskHandoffAuthorityError, TaskHandoffAuthorityPort, TaskHandoffConsumeOutcome,
    TaskHandoffGrantV1, TaskHandoffScopeV1, WorkflowDefinitionAuthorityError,
    WorkflowDefinitionAuthorityPort,
};
use tracedecay_domain::{
    ManifestDigest, UtcMicros, WorkflowDefinitionId, WorkflowDefinitionV1, canonical_sha256,
};

use crate::exact_sql::{
    ExactSqlError, ExactSqlHandle, ExactSqlRows, ExactSqlStatement, ExactSqlTransaction,
    ExactSqlValue,
};
use crate::work::WorkSqliteStorage;

mod run_journal;

/// Workflow persistence on the registered Work exact-SQL handle.
#[derive(Clone)]
pub struct WorkflowSqliteAuthority {
    handle: ExactSqlHandle,
}

impl WorkflowSqliteAuthority {
    /// Clone the crate-visible Work handle after the registered store proves
    /// the exact final schema.
    pub fn from_work_storage(
        storage: &WorkSqliteStorage,
    ) -> Result<Self, WorkflowSqliteAuthorityBuildError> {
        storage
            .require_exact_schema()
            .map_err(|_| WorkflowSqliteAuthorityBuildError::ResetRequired)?;
        Ok(Self {
            handle: storage.handle.clone(),
        })
    }
}

/// Construction failure for the durable workflow authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowSqliteAuthorityBuildError {
    ResetRequired,
    Unavailable,
}

fn definition_unavailable(_: ExactSqlError) -> WorkflowDefinitionAuthorityError {
    WorkflowDefinitionAuthorityError::Unavailable(
        "workflow definition authority unavailable".to_owned(),
    )
}

fn definition_codec_unavailable() -> WorkflowDefinitionAuthorityError {
    WorkflowDefinitionAuthorityError::Unavailable(
        "workflow definition authority unavailable".to_owned(),
    )
}

fn handoff_unavailable(_: ExactSqlError) -> TaskHandoffAuthorityError {
    TaskHandoffAuthorityError::Unavailable("workflow handoff authority unavailable".to_owned())
}

fn handoff_codec_unavailable() -> TaskHandoffAuthorityError {
    TaskHandoffAuthorityError::Unavailable("workflow handoff authority unavailable".to_owned())
}

fn statement(
    sql: &str,
    params: Vec<MigrationSqlValue>,
) -> Result<MigrationSqlStatement, MigrationSqlError> {
    MigrationSqlStatement::new(sql.to_owned(), params)
}

fn exact_sql_text(values: &[ExactSqlValue], index: usize) -> Option<&str> {
    match values.get(index)? {
        ExactSqlValue::Text(value) => Some(value),
        _ => None,
    }
}

fn exact_sql_integer(values: &[ExactSqlValue], index: usize) -> Option<i64> {
    match values.get(index)? {
        ExactSqlValue::Integer(value) => Some(*value),
        _ => None,
    }
}

fn version_i64(version: u64) -> Result<i64, ()> {
    i64::try_from(version).map_err(|_| ())
}

fn version_u64(value: i64) -> Result<u64, ()> {
    u64::try_from(value).map_err(|_| ())
}

fn definition_digest(
    definition: &WorkflowDefinitionV1,
) -> Result<ManifestDigest, WorkflowDefinitionAuthorityError> {
    canonical_sha256(definition).map_err(|_| definition_codec_unavailable())
}

fn encode_definition(
    definition: &WorkflowDefinitionV1,
) -> Result<String, WorkflowDefinitionAuthorityError> {
    serde_json::to_string(definition).map_err(|_| definition_codec_unavailable())
}

fn decode_definition(
    payload: &str,
) -> Result<WorkflowDefinitionV1, WorkflowDefinitionAuthorityError> {
    serde_json::from_str(payload).map_err(|_| definition_codec_unavailable())
}

fn encode_json<T: serde::Serialize>(value: &T) -> Result<String, ()> {
    serde_json::to_string(value).map_err(|_| ())
}

fn decode_json<T: serde::de::DeserializeOwned>(payload: &str) -> Result<T, ()> {
    serde_json::from_str(payload).map_err(|_| ())
}

fn query_handle(
    handle: &ExactSqlHandle,
    sql: &str,
    params: Vec<ExactSqlValue>,
) -> Result<ExactSqlRows, ExactSqlError> {
    handle.query(statement(sql, params)?, Duration::from_secs(5))
}

fn query_tx(
    transaction: &ExactSqlTransaction,
    sql: &str,
    params: Vec<ExactSqlValue>,
) -> Result<ExactSqlRows, ExactSqlError> {
    transaction.query(statement(sql, params)?)
}

fn execute_tx(
    transaction: &ExactSqlTransaction,
    sql: &str,
    params: Vec<ExactSqlValue>,
) -> Result<(), ExactSqlError> {
    transaction.execute(statement(sql, params)?).map(|_| ())
}

impl WorkflowDefinitionAuthorityPort for WorkflowSqliteAuthority {
    fn insert(
        &self,
        definition: &WorkflowDefinitionV1,
    ) -> Result<(), WorkflowDefinitionAuthorityError> {
        let version = version_i64(definition.definition_version())
            .map_err(|_| definition_codec_unavailable())?;
        let payload = encode_definition(definition)?;
        let digest = definition_digest(definition)?;
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(definition_unavailable)?;
        let existing = query_tx(
            &transaction,
            "SELECT payload, payload_digest FROM workflow_definitions_v1
             WHERE definition_id = ?1 AND definition_version = ?2",
            vec![
                ExactSqlValue::Text(definition.definition_id().as_str().to_owned()),
                ExactSqlValue::Integer(version),
            ],
        )
        .map_err(definition_unavailable)?;
        if let Some(row) = existing.rows.first() {
            let existing_digest =
                exact_sql_text(&row.values, 1).ok_or_else(definition_codec_unavailable)?;
            let outcome = if existing_digest == digest.as_str() {
                Err(WorkflowDefinitionAuthorityError::AlreadyExists)
            } else {
                let existing_payload =
                    exact_sql_text(&row.values, 0).ok_or_else(definition_codec_unavailable)?;
                let existing_definition = decode_definition(existing_payload)?;
                if &existing_definition == definition {
                    Err(WorkflowDefinitionAuthorityError::AlreadyExists)
                } else {
                    Err(WorkflowDefinitionAuthorityError::Conflict)
                }
            };
            let _ = transaction.rollback();
            return outcome;
        }
        execute_tx(
            &transaction,
            "INSERT INTO workflow_definitions_v1 (
                 definition_id, definition_version, payload, payload_digest
             ) VALUES (?1, ?2, ?3, ?4)",
            vec![
                ExactSqlValue::Text(definition.definition_id().as_str().to_owned()),
                ExactSqlValue::Integer(version),
                ExactSqlValue::Text(payload),
                ExactSqlValue::Text(digest.as_str().to_owned()),
            ],
        )
        .map_err(definition_unavailable)?;
        transaction
            .commit()
            .map(|_| ())
            .map_err(definition_unavailable)
    }

    fn load(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
    ) -> Result<Option<WorkflowDefinitionV1>, WorkflowDefinitionAuthorityError> {
        let version =
            version_i64(definition_version).map_err(|_| definition_codec_unavailable())?;
        let rows = query_handle(
            &self.handle,
            "SELECT payload FROM workflow_definitions_v1
             WHERE definition_id = ?1 AND definition_version = ?2",
            vec![
                ExactSqlValue::Text(definition_id.as_str().to_owned()),
                ExactSqlValue::Integer(version),
            ],
        )
        .map_err(definition_unavailable)?;
        rows.rows
            .first()
            .map(|row| {
                let payload =
                    exact_sql_text(&row.values, 0).ok_or_else(definition_codec_unavailable)?;
                decode_definition(payload)
            })
            .transpose()
    }

    fn active_version(
        &self,
        definition_id: &WorkflowDefinitionId,
    ) -> Result<Option<u64>, WorkflowDefinitionAuthorityError> {
        let rows = query_handle(
            &self.handle,
            "SELECT active_version FROM workflow_activations_v1 WHERE definition_id = ?1",
            vec![ExactSqlValue::Text(definition_id.as_str().to_owned())],
        )
        .map_err(definition_unavailable)?;
        rows.rows
            .first()
            .map(|row| {
                let version =
                    exact_sql_integer(&row.values, 0).ok_or_else(definition_codec_unavailable)?;
                version_u64(version).map_err(|_| definition_codec_unavailable())
            })
            .transpose()
    }

    fn compare_and_swap_activation(
        &self,
        definition_id: &WorkflowDefinitionId,
        expected_version: Option<u64>,
        replacement_version: u64,
    ) -> Result<(), WorkflowDefinitionAuthorityError> {
        let replacement =
            version_i64(replacement_version).map_err(|_| definition_codec_unavailable())?;
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(definition_unavailable)?;
        let rows = query_tx(
            &transaction,
            "SELECT active_version FROM workflow_activations_v1 WHERE definition_id = ?1",
            vec![ExactSqlValue::Text(definition_id.as_str().to_owned())],
        )
        .map_err(definition_unavailable)?;
        let current = rows
            .rows
            .first()
            .map(|row| {
                let version =
                    exact_sql_integer(&row.values, 0).ok_or_else(definition_codec_unavailable)?;
                version_u64(version).map_err(|_| definition_codec_unavailable())
            })
            .transpose()?;
        if current != expected_version {
            let _ = transaction.rollback();
            return Err(WorkflowDefinitionAuthorityError::Conflict);
        }
        execute_tx(
            &transaction,
            "INSERT INTO workflow_activations_v1 (definition_id, active_version)
             VALUES (?1, ?2)
             ON CONFLICT(definition_id) DO UPDATE SET
                 active_version = excluded.active_version",
            vec![
                ExactSqlValue::Text(definition_id.as_str().to_owned()),
                ExactSqlValue::Integer(replacement),
            ],
        )
        .map_err(definition_unavailable)?;
        transaction
            .commit()
            .map(|_| ())
            .map_err(definition_unavailable)
    }
}

impl TaskHandoffAuthorityPort for WorkflowSqliteAuthority {
    fn issue(&self, grant: &TaskHandoffGrantV1) -> Result<(), TaskHandoffAuthorityError> {
        let scope_payload = encode_json(grant.scope()).map_err(|_| handoff_codec_unavailable())?;
        let transaction = self.handle.begin_immediate().map_err(handoff_unavailable)?;
        let existing = query_tx(
            &transaction,
            "SELECT 1 FROM workflow_handoffs_v1 WHERE token_digest = ?1",
            vec![ExactSqlValue::Text(
                grant.token_digest().as_str().to_owned(),
            )],
        )
        .map_err(handoff_unavailable)?;
        if !existing.rows.is_empty() {
            let _ = transaction.rollback();
            return Err(TaskHandoffAuthorityError::Conflict);
        }
        execute_tx(
            &transaction,
            "INSERT INTO workflow_handoffs_v1 (
                 token_digest, scope_payload, issued_at, expires_at, consumed
             ) VALUES (?1, ?2, ?3, ?4, 0)",
            vec![
                ExactSqlValue::Text(grant.token_digest().as_str().to_owned()),
                ExactSqlValue::Text(scope_payload),
                ExactSqlValue::Integer(grant.issued_at().0),
                ExactSqlValue::Integer(grant.expires_at().0),
            ],
        )
        .map_err(handoff_unavailable)?;
        transaction
            .commit()
            .map(|_| ())
            .map_err(handoff_unavailable)
    }

    fn consume(
        &self,
        token_digest: &ManifestDigest,
        expected_scope: &TaskHandoffScopeV1,
        consumed_at: UtcMicros,
    ) -> Result<TaskHandoffConsumeOutcome, TaskHandoffAuthorityError> {
        let transaction = self.handle.begin_immediate().map_err(handoff_unavailable)?;
        let rows = query_tx(
            &transaction,
            "SELECT scope_payload, expires_at, consumed FROM workflow_handoffs_v1
             WHERE token_digest = ?1",
            vec![ExactSqlValue::Text(token_digest.as_str().to_owned())],
        )
        .map_err(handoff_unavailable)?;
        let Some(row) = rows.rows.first() else {
            let _ = transaction.rollback();
            return Ok(TaskHandoffConsumeOutcome::Missing);
        };
        let scope_payload = exact_sql_text(&row.values, 0).ok_or_else(handoff_codec_unavailable)?;
        let scope: TaskHandoffScopeV1 =
            decode_json(scope_payload).map_err(|_| handoff_codec_unavailable())?;
        if &scope != expected_scope {
            let _ = transaction.rollback();
            return Ok(TaskHandoffConsumeOutcome::ScopeMismatch);
        }
        let expires_at = exact_sql_integer(&row.values, 1).ok_or_else(handoff_codec_unavailable)?;
        if consumed_at.0 >= expires_at {
            let _ = transaction.rollback();
            return Ok(TaskHandoffConsumeOutcome::Expired);
        }
        let consumed = exact_sql_integer(&row.values, 2).ok_or_else(handoff_codec_unavailable)?;
        if consumed != 0 {
            let _ = transaction.rollback();
            return Ok(TaskHandoffConsumeOutcome::Replay);
        }
        execute_tx(
            &transaction,
            "UPDATE workflow_handoffs_v1 SET consumed = 1 WHERE token_digest = ?1 AND consumed = 0",
            vec![ExactSqlValue::Text(token_digest.as_str().to_owned())],
        )
        .map_err(handoff_unavailable)?;
        transaction
            .commit()
            .map(|_| TaskHandoffConsumeOutcome::Consumed)
            .map_err(handoff_unavailable)
    }
}
