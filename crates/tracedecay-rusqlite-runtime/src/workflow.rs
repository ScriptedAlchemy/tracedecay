//! Durable workflow authority over the canonical registered SQL channel.

use std::time::Duration;

use tracedecay_application::{
    TaskHandoffAuthorityError, TaskHandoffAuthorityPort, TaskHandoffConsumeOutcome,
    TaskHandoffGrant, TaskHandoffScope, WorkflowDefinitionAuthorityError,
    WorkflowDefinitionAuthorityPort, WorkflowDefinitionDisposition,
    WorkflowDefinitionLifecycleCommand, WorkflowDefinitionTransitionEntry,
    WorkflowDefinitionTransitionOutcome, WorkflowEffectAuthorityErrorV1,
    WorkflowEffectAuthorityPortV1, WorkflowEffectIdentityV1, WorkflowEffectJournalRecordV1,
    WorkflowEffectJournalStateV1, WorkflowEffectOutcomeV1, WorkflowEffectPreparedV1,
    WorkflowEffectProblemV1, WorkflowEffectTerminalV1,
};
use tracedecay_domain::{
    ManifestDigest, UtcMicros, WorkflowDefinition, WorkflowDefinitionId, canonical_sha256,
};

use crate::exact_sql::{
    ExactSqlError, ExactSqlError as MigrationSqlError, ExactSqlHandle, ExactSqlRows,
    ExactSqlStatement, ExactSqlStatement as MigrationSqlStatement, ExactSqlTransaction,
    ExactSqlValue, ExactSqlValue as MigrationSqlValue,
};
mod disposition;
mod effect_mutation;
mod schema;

pub use schema::{
    WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1, WORKFLOW_SCHEMA_IDENTITY_V1, WORKFLOW_SCHEMA_VERSION_V1,
    WORKFLOW_TABLE_CONTRACTS_V1, WorkflowColumnContractV1, WorkflowTableContractV1,
    install_workflow_schema,
};

const WORKFLOW_EFFECT_SELECT: &str = "SELECT identity_digest, state, terminal_payload,
        identity_payload, identity_payload_digest,
        terminal_payload_digest, operation,
        prepared_payload, prepared_payload_digest
 FROM workflow_effect_journal
 WHERE idempotency_key = ?1";

/// Workflow effect/source journals and handoffs on the registered writer.
///
/// Workflow definition topology is owned by the registered graph adapter. The
/// SQL authority retains only immutable source payloads needed to make effect
/// retries deterministic.
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

    pub fn load_definition_source(
        &self,
        definition_id: &tracedecay_domain::WorkflowDefinitionId,
        definition_version: u64,
    ) -> Result<Option<WorkflowDefinition>, WorkflowSqliteAuthorityBuildError> {
        let version = i64::try_from(definition_version)
            .map_err(|_| WorkflowSqliteAuthorityBuildError::Unavailable)?;
        let rows = self
            .storage
            .query(
                ExactSqlStatement::new(
                    "SELECT payload, payload_digest
                     FROM workflow_definition_source_journal
                     WHERE definition_id = ?1 AND definition_version = ?2"
                        .to_owned(),
                    vec![
                        ExactSqlValue::Text(definition_id.as_str().to_owned()),
                        ExactSqlValue::Integer(version),
                    ],
                )
                .map_err(|_| WorkflowSqliteAuthorityBuildError::Unavailable)?,
                Duration::from_secs(5),
            )
            .map_err(|_| WorkflowSqliteAuthorityBuildError::Unavailable)?;
        let Some(row) = rows.rows.first() else {
            return Ok(None);
        };
        let Some(ExactSqlValue::Text(payload)) = row.values.first() else {
            return Err(WorkflowSqliteAuthorityBuildError::ResetRequired);
        };
        let Some(ExactSqlValue::Text(stored_digest)) = row.values.get(1) else {
            return Err(WorkflowSqliteAuthorityBuildError::ResetRequired);
        };
        let definition: WorkflowDefinition = serde_json::from_str(payload)
            .map_err(|_| WorkflowSqliteAuthorityBuildError::ResetRequired)?;
        let digest = canonical_sha256(&definition)
            .map_err(|_| WorkflowSqliteAuthorityBuildError::ResetRequired)?;
        if digest.as_str() != stored_digest
            || definition.definition_id() != definition_id
            || definition.definition_version() != definition_version
        {
            return Err(WorkflowSqliteAuthorityBuildError::ResetRequired);
        }
        Ok(Some(definition))
    }
}

impl WorkflowDefinitionAuthorityPort for WorkflowSqliteAuthority {
    fn insert(
        &self,
        definition: &WorkflowDefinition,
    ) -> Result<(), WorkflowDefinitionAuthorityError> {
        let version = version_i64(definition.definition_version())
            .map_err(|_| definition_authority_unavailable())?;
        let payload =
            serde_json::to_string(definition).map_err(|_| definition_authority_unavailable())?;
        let digest =
            canonical_sha256(definition).map_err(|_| definition_authority_unavailable())?;
        let transaction = self
            .storage
            .begin_immediate()
            .map_err(|_| definition_authority_unavailable())?;
        let existing = query_tx(
            &transaction,
            "SELECT payload_digest FROM workflow_definition_source_journal
             WHERE definition_id = ?1 AND definition_version = ?2",
            vec![
                ExactSqlValue::Text(definition.definition_id().as_str().to_owned()),
                ExactSqlValue::Integer(version),
            ],
        )
        .map_err(|_| definition_authority_unavailable())?;
        if let Some(row) = existing.rows.first() {
            let outcome = if sql_text(&row.values, 0) == Some(digest.as_str()) {
                WorkflowDefinitionAuthorityError::AlreadyExists
            } else {
                WorkflowDefinitionAuthorityError::Conflict
            };
            let _ = transaction.rollback();
            return Err(outcome);
        }
        execute_tx(
            &transaction,
            "INSERT INTO workflow_definition_source_journal (
                 definition_id, definition_version, payload, payload_digest
             ) VALUES (?1, ?2, ?3, ?4)",
            vec![
                ExactSqlValue::Text(definition.definition_id().as_str().to_owned()),
                ExactSqlValue::Integer(version),
                ExactSqlValue::Text(payload),
                ExactSqlValue::Text(digest.as_str().to_owned()),
            ],
        )
        .map_err(|_| definition_authority_unavailable())?;
        disposition::seed_candidate_disposition(
            &transaction,
            definition.definition_id(),
            definition.definition_version(),
            UtcMicros(0),
        )
        .map_err(|_| definition_authority_unavailable())?;
        transaction
            .commit()
            .map(|_| ())
            .map_err(|_| definition_authority_unavailable())
    }

    fn load(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
    ) -> Result<Option<WorkflowDefinition>, WorkflowDefinitionAuthorityError> {
        self.load_definition_source(definition_id, definition_version)
            .map_err(|_| definition_authority_unavailable())
    }

    fn list(
        &self,
        definition_id: Option<&WorkflowDefinitionId>,
    ) -> Result<Vec<WorkflowDefinition>, WorkflowDefinitionAuthorityError> {
        let (sql, values) = match definition_id {
            Some(definition_id) => (
                "SELECT payload, payload_digest
                 FROM workflow_definition_source_journal
                 WHERE definition_id = ?1
                 ORDER BY definition_id, definition_version",
                vec![ExactSqlValue::Text(definition_id.as_str().to_owned())],
            ),
            None => (
                "SELECT payload, payload_digest
                 FROM workflow_definition_source_journal
                 ORDER BY definition_id, definition_version",
                Vec::new(),
            ),
        };
        let rows = self
            .storage
            .query(
                ExactSqlStatement::new(sql.to_owned(), values)
                    .map_err(|_| definition_authority_unavailable())?,
                Duration::from_secs(5),
            )
            .map_err(|_| definition_authority_unavailable())?;
        rows.rows.iter().map(decode_definition_source_row).collect()
    }

    fn load_disposition(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
    ) -> Result<Option<WorkflowDefinitionDisposition>, WorkflowDefinitionAuthorityError> {
        let transaction = self
            .storage
            .begin_immediate()
            .map_err(|_| definition_authority_unavailable())?;
        let disposition =
            disposition::load_disposition_tx(&transaction, definition_id, definition_version)
                .map_err(|_| definition_authority_unavailable())?;
        transaction
            .commit()
            .map_err(|_| definition_authority_unavailable())?;
        Ok(disposition)
    }

    fn transition(
        &self,
        command: &WorkflowDefinitionLifecycleCommand,
    ) -> Result<WorkflowDefinitionTransitionOutcome, WorkflowDefinitionAuthorityError> {
        let transaction = self
            .storage
            .begin_immediate()
            .map_err(|_| definition_authority_unavailable())?;
        let outcome = match disposition::apply_lifecycle_transition(&transaction, command) {
            Ok(outcome) => outcome,
            Err(_) => {
                let _ = transaction.rollback();
                return Err(definition_authority_unavailable());
            }
        };
        transaction
            .commit()
            .map_err(|_| definition_authority_unavailable())?;
        Ok(outcome)
    }

    fn transition_history(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
    ) -> Result<Vec<WorkflowDefinitionTransitionEntry>, WorkflowDefinitionAuthorityError> {
        let transaction = self
            .storage
            .begin_immediate()
            .map_err(|_| definition_authority_unavailable())?;
        let entries =
            disposition::transition_history_tx(&transaction, definition_id, definition_version)
                .map_err(|_| definition_authority_unavailable())?;
        transaction
            .commit()
            .map_err(|_| definition_authority_unavailable())?;
        Ok(entries)
    }
}

fn decode_definition_source_row(
    row: &crate::exact_sql::ExactSqlRow,
) -> Result<WorkflowDefinition, WorkflowDefinitionAuthorityError> {
    let Some(ExactSqlValue::Text(payload)) = row.values.first() else {
        return Err(definition_authority_unavailable());
    };
    let Some(ExactSqlValue::Text(stored_digest)) = row.values.get(1) else {
        return Err(definition_authority_unavailable());
    };
    let definition: WorkflowDefinition =
        serde_json::from_str(payload).map_err(|_| definition_authority_unavailable())?;
    let digest = canonical_sha256(&definition).map_err(|_| definition_authority_unavailable())?;
    if digest.as_str() != stored_digest {
        return Err(definition_authority_unavailable());
    }
    Ok(definition)
}

fn definition_authority_unavailable() -> WorkflowDefinitionAuthorityError {
    WorkflowDefinitionAuthorityError::Unavailable(
        "workflow definition source journal is unavailable".to_owned(),
    )
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
                "SELECT name, sql FROM sqlite_master
                 WHERE type = 'table'
                   AND name IN (
                       'workflow_definition_disposition',
                       'workflow_definition_source_journal',
                       'workflow_definition_transition_journal',
                       'workflow_effect_journal',
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
            Some(ExactSqlValue::Text(name)) => match row.values.get(1) {
                Some(ExactSqlValue::Text(sql)) => Some((name.as_str(), sql.as_str())),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    if actual
        != WORKFLOW_TABLE_CONTRACTS_V1
            .iter()
            .map(|table| (table.name, table.sql))
            .collect::<Vec<_>>()
    {
        return Err(WorkflowSqliteAuthorityBuildError::ResetRequired);
    }
    let schema = handle
        .query(
            ExactSqlStatement::new(
                "SELECT singleton, schema_version, definition_digest FROM workflow_schema
                 ORDER BY singleton"
                    .to_owned(),
                Vec::new(),
            )
            .map_err(|_| WorkflowSqliteAuthorityBuildError::Unavailable)?,
            Duration::from_secs(5),
        )
        .map_err(|_| WorkflowSqliteAuthorityBuildError::Unavailable)?;
    let valid_schema = schema.rows.len() == 1
        && schema.rows.first().is_some_and(|row| {
            matches!(row.values.first(), Some(ExactSqlValue::Integer(1)))
                && matches!(
                    row.values.get(1),
                    Some(ExactSqlValue::Integer(WORKFLOW_SCHEMA_VERSION_V1))
                )
                && matches!(
                    row.values.get(2),
                    Some(ExactSqlValue::Text(digest))
                        if digest == WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1
                )
        });
    if !valid_schema {
        return Err(WorkflowSqliteAuthorityBuildError::ResetRequired);
    }
    for table in WORKFLOW_TABLE_CONTRACTS_V1 {
        require_columns(handle, table)?;
    }
    Ok(())
}

fn require_columns(
    handle: &ExactSqlHandle,
    table: &WorkflowTableContractV1,
) -> Result<(), WorkflowSqliteAuthorityBuildError> {
    let columns = handle
        .query(
            ExactSqlStatement::new(format!("PRAGMA table_info({})", table.name), Vec::new())
                .map_err(|_| WorkflowSqliteAuthorityBuildError::Unavailable)?,
            Duration::from_secs(5),
        )
        .map_err(|_| WorkflowSqliteAuthorityBuildError::Unavailable)?;
    let exact = columns.rows.len() == table.columns.len()
        && columns
            .rows
            .iter()
            .zip(table.columns)
            .all(|(row, column)| {
                matches!(row.values.get(1), Some(ExactSqlValue::Text(actual)) if actual == column.name)
                    && matches!(row.values.get(2), Some(ExactSqlValue::Text(actual)) if actual == column.sql_type)
                    && matches!(row.values.get(3), Some(ExactSqlValue::Integer(actual)) if *actual == column.not_null)
                    && matches!(row.values.get(5), Some(ExactSqlValue::Integer(actual)) if *actual == column.primary_key)
            });
    if exact {
        Ok(())
    } else {
        Err(WorkflowSqliteAuthorityBuildError::ResetRequired)
    }
}

fn handoff_unavailable(_: ExactSqlError) -> TaskHandoffAuthorityError {
    TaskHandoffAuthorityError::Unavailable("workflow handoff authority unavailable".to_owned())
}

fn handoff_codec_unavailable() -> TaskHandoffAuthorityError {
    TaskHandoffAuthorityError::Unavailable("workflow handoff authority unavailable".to_owned())
}

fn workflow_effect_unavailable(_: ExactSqlError) -> WorkflowEffectAuthorityErrorV1 {
    WorkflowEffectAuthorityErrorV1::Unavailable(
        "registered workflow effect storage unavailable".to_owned(),
    )
}

fn workflow_effect_codec_unavailable() -> WorkflowEffectAuthorityErrorV1 {
    WorkflowEffectAuthorityErrorV1::Unavailable(
        "registered workflow effect receipt unavailable".to_owned(),
    )
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

fn definition_digest(definition: &WorkflowDefinition) -> Result<ManifestDigest, ()> {
    canonical_sha256(definition).map_err(|_| ())
}

fn encode_definition(definition: &WorkflowDefinition) -> Result<String, ()> {
    serde_json::to_string(definition).map_err(|_| ())
}

fn encode_json<T: serde::Serialize>(value: &T) -> Result<String, ()> {
    serde_json::to_string(value).map_err(|_| ())
}

fn decode_json<T: serde::de::DeserializeOwned>(payload: &str) -> Result<T, ()> {
    serde_json::from_str(payload).map_err(|_| ())
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

fn execute_tx_changed(
    transaction: &ExactSqlTransaction,
    sql: &str,
    params: Vec<ExactSqlValue>,
) -> Result<usize, ExactSqlError> {
    transaction
        .execute(statement(sql, params)?)
        .map(|result| result.changed_rows)
}

impl TaskHandoffAuthorityPort for WorkflowSqliteAuthority {
    fn issue(&self, grant: &TaskHandoffGrant) -> Result<(), TaskHandoffAuthorityError> {
        let scope_payload = encode_json(grant.scope()).map_err(|_| handoff_codec_unavailable())?;
        let transaction = self
            .storage
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

impl WorkflowEffectAuthorityPortV1 for WorkflowSqliteAuthority {
    fn reserve_effect(
        &self,
        identity: &WorkflowEffectIdentityV1,
        prepared: &WorkflowEffectPreparedV1,
    ) -> Result<WorkflowEffectJournalRecordV1, WorkflowEffectAuthorityErrorV1> {
        if prepared.input_digest() != identity.input_digest()
            || prepared
                .operation()
                .is_some_and(|operation| operation != identity.operation())
        {
            return Err(WorkflowEffectAuthorityErrorV1::IdentityConflict);
        }
        let identity_digest = identity
            .identity_digest()
            .map_err(|_| workflow_effect_codec_unavailable())?;
        let identity_payload =
            encode_json(identity).map_err(|_| workflow_effect_codec_unavailable())?;
        let identity_payload_digest = identity
            .payload_digest()
            .map_err(|_| workflow_effect_codec_unavailable())?;
        let prepared_payload =
            encode_json(prepared).map_err(|_| workflow_effect_codec_unavailable())?;
        let prepared_payload_digest = prepared
            .payload_digest()
            .map_err(|_| workflow_effect_codec_unavailable())?;
        let transaction = self
            .storage
            .begin_immediate()
            .map_err(workflow_effect_unavailable)?;
        let existing = query_tx(
            &transaction,
            WORKFLOW_EFFECT_SELECT,
            vec![ExactSqlValue::Text(
                identity.idempotency_key().as_str().to_owned(),
            )],
        )
        .map_err(workflow_effect_unavailable)?;
        if let Some(row) = existing.rows.first() {
            let persisted_digest =
                sql_text(&row.values, 0).ok_or_else(workflow_effect_codec_unavailable)?;
            if persisted_digest != identity_digest.as_str() {
                let _ = transaction.rollback();
                return Err(WorkflowEffectAuthorityErrorV1::IdentityConflict);
            }
            let persisted_identity = decode_workflow_effect_identity(&row.values)?;
            if persisted_identity
                .identity_digest()
                .map_err(|_| workflow_effect_codec_unavailable())?
                != identity_digest
            {
                let _ = transaction.rollback();
                return Err(WorkflowEffectAuthorityErrorV1::IdentityConflict);
            }
            let persisted_prepared = decode_workflow_effect_preparation(&row.values)?;
            if persisted_prepared.input_digest() != persisted_identity.input_digest()
                || persisted_prepared
                    .operation()
                    .is_some_and(|operation| operation != persisted_identity.operation())
            {
                let _ = transaction.rollback();
                return Err(WorkflowEffectAuthorityErrorV1::IdentityConflict);
            }
            let record = decode_workflow_effect_record(&row.values)?;
            if let Some(terminal) = record.terminal() {
                terminal
                    .identity()
                    .validate()
                    .map_err(|_| workflow_effect_codec_unavailable())?;
                if terminal
                    .identity()
                    .identity_digest()
                    .map_err(|_| workflow_effect_codec_unavailable())?
                    != identity_digest
                {
                    let _ = transaction.rollback();
                    return Err(WorkflowEffectAuthorityErrorV1::IdentityConflict);
                }
            }
            transaction.commit().map_err(workflow_effect_unavailable)?;
            return Ok(record);
        }
        execute_tx(
            &transaction,
            "INSERT INTO workflow_effect_journal (
                 idempotency_key, identity_digest, identity_payload,
                 identity_payload_digest, prepared_payload,
                 prepared_payload_digest, operation, state, terminal_payload,
                 terminal_payload_digest, created_at, updated_at
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7,
                 'before_effect', NULL, NULL, ?8, ?8
             )",
            vec![
                ExactSqlValue::Text(identity.idempotency_key().as_str().to_owned()),
                ExactSqlValue::Text(identity_digest.as_str().to_owned()),
                ExactSqlValue::Text(identity_payload),
                ExactSqlValue::Text(identity_payload_digest.as_str().to_owned()),
                ExactSqlValue::Text(prepared_payload),
                ExactSqlValue::Text(prepared_payload_digest.as_str().to_owned()),
                ExactSqlValue::Text(identity.operation().as_str().to_owned()),
                ExactSqlValue::Integer(identity.started_at().0),
            ],
        )
        .map_err(workflow_effect_unavailable)?;
        transaction.commit().map_err(workflow_effect_unavailable)?;
        Ok(WorkflowEffectJournalRecordV1::before_effect())
    }

    fn execute_effect(
        &self,
        identity: &WorkflowEffectIdentityV1,
        prepared: &WorkflowEffectPreparedV1,
        ended_at: UtcMicros,
    ) -> Result<WorkflowEffectJournalRecordV1, WorkflowEffectAuthorityErrorV1> {
        let reserved = self.reserve_effect(identity, prepared)?;
        if reserved.terminal().is_some() {
            return reconcile_workflow_effect(&self.storage, identity, reserved);
        }
        let identity_digest = identity
            .identity_digest()
            .map_err(|_| workflow_effect_codec_unavailable())?;
        let transaction = self
            .storage
            .begin_immediate()
            .map_err(workflow_effect_unavailable)?;
        let current = query_tx(
            &transaction,
            WORKFLOW_EFFECT_SELECT,
            vec![ExactSqlValue::Text(
                identity.idempotency_key().as_str().to_owned(),
            )],
        )
        .map_err(workflow_effect_unavailable)?;
        let row = current
            .rows
            .first()
            .ok_or_else(workflow_effect_codec_unavailable)?;
        if sql_text(&row.values, 0) != Some(identity_digest.as_str()) {
            let _ = transaction.rollback();
            return Err(WorkflowEffectAuthorityErrorV1::IdentityConflict);
        }
        let persisted_identity = decode_workflow_effect_identity(&row.values)?;
        if persisted_identity
            .identity_digest()
            .map_err(|_| workflow_effect_codec_unavailable())?
            != identity_digest
        {
            let _ = transaction.rollback();
            return Err(WorkflowEffectAuthorityErrorV1::IdentityConflict);
        }
        let persisted_prepared = decode_workflow_effect_preparation(&row.values)?;
        if persisted_prepared.input_digest() != persisted_identity.input_digest()
            || persisted_prepared
                .operation()
                .is_some_and(|operation| operation != persisted_identity.operation())
        {
            let _ = transaction.rollback();
            return Err(WorkflowEffectAuthorityErrorV1::IdentityConflict);
        }
        let current_record = decode_workflow_effect_record(&row.values)?;
        if current_record.terminal().is_some() {
            transaction.commit().map_err(workflow_effect_unavailable)?;
            return reconcile_workflow_effect(&self.storage, identity, current_record);
        }
        let claimed = execute_tx_changed(
            &transaction,
            "UPDATE workflow_effect_journal
             SET state = 'in_flight', updated_at = ?2
             WHERE idempotency_key = ?1
               AND state IN ('before_effect', 'in_flight')
               AND terminal_payload IS NULL",
            vec![
                ExactSqlValue::Text(identity.idempotency_key().as_str().to_owned()),
                ExactSqlValue::Integer(ended_at.0),
            ],
        )
        .map_err(workflow_effect_unavailable)?;
        if claimed != 1 {
            let _ = transaction.rollback();
            return Err(WorkflowEffectAuthorityErrorV1::InvalidTransition);
        }
        let outcome = if persisted_identity.deadline().is_elapsed_at(ended_at) {
            WorkflowEffectOutcomeV1::Problem(WorkflowEffectProblemV1::TimedOut)
        } else {
            effect_mutation::apply_workflow_effect(&transaction, &persisted_prepared, ended_at)?
        };
        let terminal = WorkflowEffectTerminalV1::new(persisted_identity, ended_at, outcome)?;
        let terminal_payload =
            encode_json(&terminal).map_err(|_| workflow_effect_codec_unavailable())?;
        let terminal_payload_digest =
            canonical_sha256(&("tracedecay.runtime.workflow-effect-terminal.v1", &terminal))
                .map_err(|_| workflow_effect_codec_unavailable())?;
        let committed = execute_tx_changed(
            &transaction,
            "UPDATE workflow_effect_journal
             SET state = 'committed', terminal_payload = ?2,
                 terminal_payload_digest = ?3, updated_at = ?4
             WHERE idempotency_key = ?1
               AND state = 'in_flight'
               AND terminal_payload IS NULL",
            vec![
                ExactSqlValue::Text(identity.idempotency_key().as_str().to_owned()),
                ExactSqlValue::Text(terminal_payload),
                ExactSqlValue::Text(terminal_payload_digest.as_str().to_owned()),
                ExactSqlValue::Integer(ended_at.0),
            ],
        )
        .map_err(workflow_effect_unavailable)?;
        if committed != 1 {
            let _ = transaction.rollback();
            return Err(WorkflowEffectAuthorityErrorV1::InvalidTransition);
        }
        transaction.commit().map_err(workflow_effect_unavailable)?;
        reconcile_workflow_effect(
            &self.storage,
            identity,
            WorkflowEffectJournalRecordV1::with_terminal(
                WorkflowEffectJournalStateV1::Committed,
                terminal,
            )?,
        )
    }
}

fn decode_workflow_effect_state(
    value: &str,
) -> Result<WorkflowEffectJournalStateV1, WorkflowEffectAuthorityErrorV1> {
    match value {
        "before_effect" => Ok(WorkflowEffectJournalStateV1::BeforeEffect),
        "in_flight" => Ok(WorkflowEffectJournalStateV1::InFlight),
        "committed" => Ok(WorkflowEffectJournalStateV1::Committed),
        "reconciled" => Ok(WorkflowEffectJournalStateV1::Reconciled),
        _ => Err(workflow_effect_codec_unavailable()),
    }
}

fn decode_workflow_effect_record(
    values: &[ExactSqlValue],
) -> Result<WorkflowEffectJournalRecordV1, WorkflowEffectAuthorityErrorV1> {
    let state = decode_workflow_effect_state(
        sql_text(values, 1).ok_or_else(workflow_effect_codec_unavailable)?,
    )?;
    match values.get(2) {
        Some(ExactSqlValue::Text(payload)) => {
            let expected_digest =
                sql_text(values, 5).ok_or_else(workflow_effect_codec_unavailable)?;
            let terminal: WorkflowEffectTerminalV1 =
                decode_json(payload).map_err(|_| workflow_effect_codec_unavailable())?;
            terminal
                .validate()
                .map_err(|_| workflow_effect_codec_unavailable())?;
            if canonical_sha256(&("tracedecay.runtime.workflow-effect-terminal.v1", &terminal))
                .map_err(|_| workflow_effect_codec_unavailable())?
                .as_str()
                != expected_digest
            {
                return Err(workflow_effect_codec_unavailable());
            }
            WorkflowEffectJournalRecordV1::with_terminal(state, terminal)
        }
        Some(ExactSqlValue::Null)
            if matches!(
                state,
                WorkflowEffectJournalStateV1::BeforeEffect | WorkflowEffectJournalStateV1::InFlight
            ) && matches!(values.get(5), Some(ExactSqlValue::Null)) =>
        {
            WorkflowEffectJournalRecordV1::pending(state)
        }
        _ => Err(workflow_effect_codec_unavailable()),
    }
}

fn decode_workflow_effect_identity(
    values: &[ExactSqlValue],
) -> Result<WorkflowEffectIdentityV1, WorkflowEffectAuthorityErrorV1> {
    let payload = sql_text(values, 3).ok_or_else(workflow_effect_codec_unavailable)?;
    let expected_digest = sql_text(values, 4).ok_or_else(workflow_effect_codec_unavailable)?;
    let identity: WorkflowEffectIdentityV1 =
        decode_json(payload).map_err(|_| workflow_effect_codec_unavailable())?;
    identity
        .validate()
        .map_err(|_| workflow_effect_codec_unavailable())?;
    if identity
        .payload_digest()
        .map_err(|_| workflow_effect_codec_unavailable())?
        .as_str()
        != expected_digest
    {
        return Err(workflow_effect_codec_unavailable());
    }
    if sql_text(values, 6) != Some(identity.operation().as_str()) {
        return Err(workflow_effect_codec_unavailable());
    }
    Ok(identity)
}

fn decode_workflow_effect_preparation(
    values: &[ExactSqlValue],
) -> Result<WorkflowEffectPreparedV1, WorkflowEffectAuthorityErrorV1> {
    let payload = sql_text(values, 7).ok_or_else(workflow_effect_codec_unavailable)?;
    let expected_digest = sql_text(values, 8).ok_or_else(workflow_effect_codec_unavailable)?;
    let prepared: WorkflowEffectPreparedV1 =
        decode_json(payload).map_err(|_| workflow_effect_codec_unavailable())?;
    if prepared
        .payload_digest()
        .map_err(|_| workflow_effect_codec_unavailable())?
        .as_str()
        != expected_digest
    {
        return Err(workflow_effect_codec_unavailable());
    }
    Ok(prepared)
}

fn reconcile_workflow_effect(
    storage: &ExactSqlHandle,
    identity: &WorkflowEffectIdentityV1,
    record: WorkflowEffectJournalRecordV1,
) -> Result<WorkflowEffectJournalRecordV1, WorkflowEffectAuthorityErrorV1> {
    if record.state() == WorkflowEffectJournalStateV1::Reconciled {
        return Ok(record);
    }
    if record.state() != WorkflowEffectJournalStateV1::Committed {
        return Err(WorkflowEffectAuthorityErrorV1::InvalidTransition);
    }
    let terminal = record
        .terminal()
        .cloned()
        .ok_or(WorkflowEffectAuthorityErrorV1::InvalidTransition)?;
    let transaction = storage
        .begin_immediate()
        .map_err(workflow_effect_unavailable)?;
    let changed = execute_tx_changed(
        &transaction,
        "UPDATE workflow_effect_journal
         SET state = 'reconciled', updated_at = ?2
         WHERE idempotency_key = ?1 AND state = 'committed'",
        vec![
            ExactSqlValue::Text(identity.idempotency_key().as_str().to_owned()),
            ExactSqlValue::Integer(terminal.ended_at().0),
        ],
    )
    .map_err(workflow_effect_unavailable)?;
    if changed == 0 {
        let current = query_tx(
            &transaction,
            WORKFLOW_EFFECT_SELECT,
            vec![ExactSqlValue::Text(
                identity.idempotency_key().as_str().to_owned(),
            )],
        )
        .map_err(workflow_effect_unavailable)?;
        let row = current
            .rows
            .first()
            .ok_or_else(workflow_effect_codec_unavailable)?;
        let identity_digest = identity
            .identity_digest()
            .map_err(|_| workflow_effect_codec_unavailable())?;
        let persisted_identity = decode_workflow_effect_identity(&row.values)?;
        if sql_text(&row.values, 0) != Some(identity_digest.as_str())
            || persisted_identity
                .identity_digest()
                .map_err(|_| workflow_effect_codec_unavailable())?
                != identity_digest
        {
            let _ = transaction.rollback();
            return Err(WorkflowEffectAuthorityErrorV1::IdentityConflict);
        }
        let persisted_prepared = decode_workflow_effect_preparation(&row.values)?;
        if persisted_prepared.input_digest() != persisted_identity.input_digest()
            || persisted_prepared
                .operation()
                .is_some_and(|operation| operation != persisted_identity.operation())
        {
            let _ = transaction.rollback();
            return Err(WorkflowEffectAuthorityErrorV1::IdentityConflict);
        }
        let current_record = decode_workflow_effect_record(&row.values)?;
        if current_record.state() != WorkflowEffectJournalStateV1::Reconciled
            || current_record.terminal() != Some(&terminal)
        {
            let _ = transaction.rollback();
            return Err(WorkflowEffectAuthorityErrorV1::InvalidTransition);
        }
        transaction.commit().map_err(workflow_effect_unavailable)?;
        return Ok(current_record);
    }
    if changed != 1 {
        let _ = transaction.rollback();
        return Err(WorkflowEffectAuthorityErrorV1::InvalidTransition);
    }
    transaction.commit().map_err(workflow_effect_unavailable)?;
    WorkflowEffectJournalRecordV1::with_terminal(WorkflowEffectJournalStateV1::Reconciled, terminal)
}
