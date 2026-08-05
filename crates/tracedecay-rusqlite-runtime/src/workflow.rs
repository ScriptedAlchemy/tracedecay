//! Durable workflow authority over the canonical registered SQL channel.

use std::time::Duration;

use tracedecay_application::{
    TaskHandoffAuthorityError, TaskHandoffAuthorityPort, TaskHandoffConsumeOutcome,
    TaskHandoffGrant, TaskHandoffScope, WorkflowDefinitionAuthorityError,
    WorkflowDefinitionAuthorityPort,
};
use tracedecay_domain::{
    ManifestDigest, UtcMicros, WorkflowDefinition, WorkflowDefinitionId, canonical_sha256,
};

use crate::exact_sql::{
    ExactSqlError, ExactSqlError as MigrationSqlError, ExactSqlHandle, ExactSqlRows,
    ExactSqlRows as MigrationSqlRows, ExactSqlStatement,
    ExactSqlStatement as MigrationSqlStatement, ExactSqlTransaction, ExactSqlValue,
    ExactSqlValue as MigrationSqlValue,
};
mod schema;

pub use schema::{WORKFLOW_SCHEMA_V1, install_workflow_schema};

/// Workflow persistence on the registered Work writer.
#[derive(Clone)]
pub struct WorkflowSqliteAuthority {
    storage: ExactSqlHandle,
}

impl WorkflowSqliteAuthority {
    pub fn from_registered(
        storage: ExactSqlHandle,
    ) -> Result<Self, WorkflowSqliteAuthorityBuildError> {
        require_workflow_schema(&storage)?;
        Ok(Self { storage })
    }
}

/// Construction failure for the durable workflow authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowSqliteAuthorityBuildError {
    ResetRequired,
    Unavailable,
}

fn require_workflow_schema(
    handle: &ExactSqlHandle,
) -> Result<(), WorkflowSqliteAuthorityBuildError> {
    let rows = handle
        .query(
            ExactSqlStatement::new(
                "SELECT name FROM sqlite_master
                 WHERE type = 'table'
                   AND name IN (
                       'workflow_definitions',
                       'workflow_activations',
                       'workflow_handoffs',
                       'workflow_schema'
                   )
                 ORDER BY name"
                    .to_owned(),
                Vec::new(),
            )
            .map_err(|_| WorkflowSqliteAuthorityBuildError::Unavailable)?,
            Duration::from_secs(5),
        )
        .map_err(|_| WorkflowSqliteAuthorityBuildError::Unavailable)?;
    let actual = rows
        .rows
        .iter()
        .filter_map(|row| match row.values.first() {
            Some(ExactSqlValue::Text(value)) => Some(value.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if actual
        != [
            "workflow_activations",
            "workflow_definitions",
            "workflow_handoffs",
            "workflow_schema",
        ]
    {
        return Err(WorkflowSqliteAuthorityBuildError::ResetRequired);
    }
    let schema = handle
        .query(
            ExactSqlStatement::new(
                "SELECT schema_version, definition_digest FROM workflow_schema
                 WHERE singleton = 1"
                    .to_owned(),
                Vec::new(),
            )
            .map_err(|_| WorkflowSqliteAuthorityBuildError::Unavailable)?,
            Duration::from_secs(5),
        )
        .map_err(|_| WorkflowSqliteAuthorityBuildError::Unavailable)?;
    let valid_schema = schema.rows.first().is_some_and(|row| {
        matches!(row.values.first(), Some(ExactSqlValue::Integer(1)))
            && matches!(
                row.values.get(1),
                Some(ExactSqlValue::Text(digest))
                    if digest
                        == schema::WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1
            )
    });
    if !valid_schema {
        return Err(WorkflowSqliteAuthorityBuildError::ResetRequired);
    }
    require_columns(
        handle,
        "workflow_definitions",
        &[
            ("definition_id", "TEXT", 1, 1),
            ("definition_version", "INTEGER", 1, 2),
            ("payload", "TEXT", 1, 0),
            ("payload_digest", "TEXT", 1, 0),
        ],
    )?;
    require_columns(
        handle,
        "workflow_activations",
        &[
            ("definition_id", "TEXT", 1, 1),
            ("active_version", "INTEGER", 1, 0),
        ],
    )?;
    require_columns(
        handle,
        "workflow_handoffs",
        &[
            ("token_digest", "TEXT", 1, 1),
            ("scope_payload", "TEXT", 1, 0),
            ("issued_at", "INTEGER", 1, 0),
            ("expires_at", "INTEGER", 1, 0),
            ("consumed", "INTEGER", 1, 0),
        ],
    )?;
    Ok(())
}

fn require_columns(
    handle: &ExactSqlHandle,
    table: &str,
    expected: &[(&str, &str, i64, i64)],
) -> Result<(), WorkflowSqliteAuthorityBuildError> {
    let columns = handle
        .query(
            ExactSqlStatement::new(format!("PRAGMA table_info({table})"), Vec::new())
                .map_err(|_| WorkflowSqliteAuthorityBuildError::Unavailable)?,
            Duration::from_secs(5),
        )
        .map_err(|_| WorkflowSqliteAuthorityBuildError::Unavailable)?;
    let exact = columns.rows.len() == expected.len()
        && columns
            .rows
            .iter()
            .zip(expected)
            .all(|(row, (name, sql_type, not_null, primary_key))| {
                matches!(row.values.get(1), Some(ExactSqlValue::Text(actual)) if actual == name)
                    && matches!(row.values.get(2), Some(ExactSqlValue::Text(actual)) if actual == sql_type)
                    && matches!(row.values.get(3), Some(ExactSqlValue::Integer(actual)) if actual == not_null)
                    && matches!(row.values.get(5), Some(ExactSqlValue::Integer(actual)) if actual == primary_key)
            });
    if exact {
        Ok(())
    } else {
        Err(WorkflowSqliteAuthorityBuildError::ResetRequired)
    }
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

fn sql_text(values: &[MigrationSqlValue], index: usize) -> Option<&str> {
    match values.get(index)? {
        ExactSqlValue::Text(value) => Some(value),
        _ => None,
    }
}

fn sql_integer(values: &[MigrationSqlValue], index: usize) -> Option<i64> {
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
    definition: &WorkflowDefinition,
) -> Result<ManifestDigest, WorkflowDefinitionAuthorityError> {
    canonical_sha256(definition).map_err(|_| definition_codec_unavailable())
}

fn encode_definition(
    definition: &WorkflowDefinition,
) -> Result<String, WorkflowDefinitionAuthorityError> {
    serde_json::to_string(definition).map_err(|_| definition_codec_unavailable())
}

fn decode_definition(
    payload: &str,
) -> Result<WorkflowDefinition, WorkflowDefinitionAuthorityError> {
    serde_json::from_str(payload).map_err(|_| definition_codec_unavailable())
}

fn encode_json<T: serde::Serialize>(value: &T) -> Result<String, ()> {
    serde_json::to_string(value).map_err(|_| ())
}

fn decode_json<T: serde::de::DeserializeOwned>(payload: &str) -> Result<T, ()> {
    serde_json::from_str(payload).map_err(|_| ())
}

fn query_handle(
    storage: &ExactSqlHandle,
    sql: &str,
    params: Vec<MigrationSqlValue>,
) -> Result<MigrationSqlRows, MigrationSqlError> {
    storage.query(statement(sql, params)?, Duration::from_secs(5))
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
        definition: &WorkflowDefinition,
    ) -> Result<(), WorkflowDefinitionAuthorityError> {
        let version = version_i64(definition.definition_version())
            .map_err(|_| definition_codec_unavailable())?;
        let payload = encode_definition(definition)?;
        let digest = definition_digest(definition)?;
        let transaction = self
            .storage
            .handle
            .begin_immediate()
            .map_err(definition_unavailable)?;
        let existing = query_tx(
            &transaction,
            "SELECT payload, payload_digest FROM workflow_definitions
             WHERE definition_id = ?1 AND definition_version = ?2",
            vec![
                ExactSqlValue::Text(definition.definition_id().as_str().to_owned()),
                ExactSqlValue::Integer(version),
            ],
        )
        .map_err(definition_unavailable)?;
        if let Some(row) = existing.rows.first() {
            let existing_digest =
                sql_text(&row.values, 1).ok_or_else(definition_codec_unavailable)?;
            let outcome = if existing_digest == digest.as_str() {
                Err(WorkflowDefinitionAuthorityError::AlreadyExists)
            } else {
                let existing_payload =
                    sql_text(&row.values, 0).ok_or_else(definition_codec_unavailable)?;
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
            "INSERT INTO workflow_definitions (
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
    ) -> Result<Option<WorkflowDefinition>, WorkflowDefinitionAuthorityError> {
        let version =
            version_i64(definition_version).map_err(|_| definition_codec_unavailable())?;
        let rows = query_handle(
            &self.storage,
            "SELECT payload FROM workflow_definitions
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
                let payload = sql_text(&row.values, 0).ok_or_else(definition_codec_unavailable)?;
                decode_definition(payload)
            })
            .transpose()
    }

    fn active_version(
        &self,
        definition_id: &WorkflowDefinitionId,
    ) -> Result<Option<u64>, WorkflowDefinitionAuthorityError> {
        let rows = query_handle(
            &self.storage,
            "SELECT active_version FROM workflow_activations WHERE definition_id = ?1",
            vec![MigrationSqlValue::Text(definition_id.as_str().to_owned())],
        )
        .map_err(definition_unavailable)?;
        rows.rows
            .first()
            .map(|row| {
                let version =
                    sql_integer(&row.values, 0).ok_or_else(definition_codec_unavailable)?;
                version_u64(version).map_err(|_| definition_codec_unavailable())
            })
            .transpose()
    }

    fn list(
        &self,
        definition_id: Option<&WorkflowDefinitionId>,
    ) -> Result<Vec<WorkflowDefinition>, WorkflowDefinitionAuthorityError> {
        let (sql, params) = match definition_id {
            Some(definition_id) => (
                "SELECT payload FROM workflow_definitions
                 WHERE definition_id = ?1
                 ORDER BY definition_id, definition_version",
                vec![ExactSqlValue::Text(definition_id.as_str().to_owned())],
            ),
            None => (
                "SELECT payload FROM workflow_definitions
                 ORDER BY definition_id, definition_version",
                Vec::new(),
            ),
        };
        let rows = query_handle(&self.storage, sql, params).map_err(definition_unavailable)?;
        rows.rows
            .iter()
            .map(|row| {
                let payload = sql_text(&row.values, 0).ok_or_else(definition_codec_unavailable)?;
                decode_definition(payload)
            })
            .collect()
    }

    fn compare_and_swap_activation(
        &self,
        definition_id: &WorkflowDefinitionId,
        expected_version: Option<u64>,
        replacement_version: Option<u64>,
    ) -> Result<(), WorkflowDefinitionAuthorityError> {
        let transaction = self
            .storage
            .handle
            .begin_immediate()
            .map_err(definition_unavailable)?;
        let rows = query_tx(
            &transaction,
            "SELECT active_version FROM workflow_activations WHERE definition_id = ?1",
            vec![MigrationSqlValue::Text(definition_id.as_str().to_owned())],
        )
        .map_err(definition_unavailable)?;
        let current = rows
            .rows
            .first()
            .map(|row| {
                let version =
                    sql_integer(&row.values, 0).ok_or_else(definition_codec_unavailable)?;
                version_u64(version).map_err(|_| definition_codec_unavailable())
            })
            .transpose()?;
        if current != expected_version {
            let _ = transaction.rollback();
            return Err(WorkflowDefinitionAuthorityError::Conflict);
        }
        match replacement_version {
            Some(replacement_version) => {
                let replacement =
                    version_i64(replacement_version).map_err(|_| definition_codec_unavailable())?;
                execute_tx(
                    &transaction,
                    "INSERT INTO workflow_activations (definition_id, active_version)
                     VALUES (?1, ?2)
                     ON CONFLICT(definition_id) DO UPDATE SET
                         active_version = excluded.active_version",
                    vec![
                        ExactSqlValue::Text(definition_id.as_str().to_owned()),
                        ExactSqlValue::Integer(replacement),
                    ],
                )
                .map_err(definition_unavailable)?;
            }
            None => {
                execute_tx(
                    &transaction,
                    "DELETE FROM workflow_activations WHERE definition_id = ?1",
                    vec![ExactSqlValue::Text(definition_id.as_str().to_owned())],
                )
                .map_err(definition_unavailable)?;
            }
        }
        transaction
            .commit()
            .map(|_| ())
            .map_err(definition_unavailable)
    }
}

impl TaskHandoffAuthorityPort for WorkflowSqliteAuthority {
    fn issue(&self, grant: &TaskHandoffGrant) -> Result<(), TaskHandoffAuthorityError> {
        let scope_payload = encode_json(grant.scope()).map_err(|_| handoff_codec_unavailable())?;
        let transaction = self
            .storage
            .handle
            .begin_immediate()
            .map_err(handoff_unavailable)?;
        let existing = query_tx(
            &transaction,
            "SELECT 1 FROM workflow_handoffs WHERE token_digest = ?1",
            vec![MigrationSqlValue::Text(
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
            "INSERT INTO workflow_handoffs (
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
        expected_scope: &TaskHandoffScope,
        consumed_at: UtcMicros,
    ) -> Result<TaskHandoffConsumeOutcome, TaskHandoffAuthorityError> {
        let transaction = self
            .storage
            .handle
            .begin_immediate()
            .map_err(handoff_unavailable)?;
        let rows = query_tx(
            &transaction,
            "SELECT scope_payload, expires_at, consumed FROM workflow_handoffs
             WHERE token_digest = ?1",
            vec![ExactSqlValue::Text(token_digest.as_str().to_owned())],
        )
        .map_err(handoff_unavailable)?;
        let Some(row) = rows.rows.first() else {
            let _ = transaction.rollback();
            return Ok(TaskHandoffConsumeOutcome::Missing);
        };
        let scope_payload = sql_text(&row.values, 0).ok_or_else(handoff_codec_unavailable)?;
        let scope: TaskHandoffScope =
            decode_json(scope_payload).map_err(|_| handoff_codec_unavailable())?;
        if &scope != expected_scope {
            let _ = transaction.rollback();
            return Ok(TaskHandoffConsumeOutcome::ScopeMismatch);
        }
        let expires_at = sql_integer(&row.values, 1).ok_or_else(handoff_codec_unavailable)?;
        if consumed_at.0 >= expires_at {
            let _ = transaction.rollback();
            return Ok(TaskHandoffConsumeOutcome::Expired);
        }
        let consumed = sql_integer(&row.values, 2).ok_or_else(handoff_codec_unavailable)?;
        if consumed != 0 {
            let _ = transaction.rollback();
            return Ok(TaskHandoffConsumeOutcome::Replay);
        }
        execute_tx(
            &transaction,
            "UPDATE workflow_handoffs SET consumed = 1 WHERE token_digest = ?1 AND consumed = 0",
            vec![MigrationSqlValue::Text(token_digest.as_str().to_owned())],
        )
        .map_err(handoff_unavailable)?;
        transaction
            .commit()
            .map(|_| TaskHandoffConsumeOutcome::Consumed)
            .map_err(handoff_unavailable)
    }
}
