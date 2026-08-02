//! Durable workflow aggregate events and published heads on the registered Work channel.

use tracedecay_application::{
    WorkflowAggregateAppendRequest, WorkflowAggregateSnapshot, WorkflowAppendOutcome,
    WorkflowDefinitionEventStorePort, WorkflowRunEventStorePort, WorkflowStateStoreError,
};
use tracedecay_domain::{
    RunId, WorkflowDefinitionEventV1, WorkflowDefinitionId, WorkflowDefinitionProjectionV1,
    WorkflowRunEventV1, WorkflowRunProjectionV1,
};

use crate::migration_sql::{
    MigrationSqlHandle, MigrationSqlRows, MigrationSqlTransaction, MigrationSqlValue,
};
use crate::workflow::{
    WorkflowSqliteAuthority, execute_tx, migration_text, query_handle, query_tx,
};

pub(crate) const WORKFLOW_STATE_SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS workflow_definition_events_v1 (
    definition_id TEXT NOT NULL,
    definition_version INTEGER NOT NULL CHECK (definition_version > 0),
    aggregate_version INTEGER NOT NULL CHECK (aggregate_version > 0),
    command_id TEXT NOT NULL,
    input_digest TEXT NOT NULL,
    occurred_at INTEGER NOT NULL,
    event_payload TEXT NOT NULL,
    PRIMARY KEY (definition_id, definition_version, aggregate_version),
    UNIQUE (definition_id, definition_version, command_id)
) STRICT;

CREATE TABLE IF NOT EXISTS workflow_definition_heads_v1 (
    definition_id TEXT NOT NULL,
    definition_version INTEGER NOT NULL CHECK (definition_version > 0),
    aggregate_version INTEGER NOT NULL CHECK (aggregate_version > 0),
    projection_payload TEXT NOT NULL,
    PRIMARY KEY (definition_id, definition_version)
) STRICT;

CREATE TABLE IF NOT EXISTS workflow_run_events_v1 (
    run_id TEXT NOT NULL,
    aggregate_version INTEGER NOT NULL CHECK (aggregate_version > 0),
    command_id TEXT NOT NULL,
    input_digest TEXT NOT NULL,
    occurred_at INTEGER NOT NULL,
    event_payload TEXT NOT NULL,
    PRIMARY KEY (run_id, aggregate_version),
    UNIQUE (run_id, command_id)
) STRICT;

CREATE TABLE IF NOT EXISTS workflow_run_heads_v1 (
    run_id TEXT NOT NULL PRIMARY KEY,
    aggregate_version INTEGER NOT NULL CHECK (aggregate_version > 0),
    projection_payload TEXT NOT NULL
) STRICT;
";

impl WorkflowDefinitionEventStorePort for WorkflowSqliteAuthority {
    fn load(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
    ) -> Result<
        WorkflowAggregateSnapshot<WorkflowDefinitionProjectionV1, WorkflowDefinitionEventV1>,
        WorkflowStateStoreError,
    > {
        let projection = definition_head(self.state_handle(), definition_id, definition_version)?
            .ok_or(WorkflowStateStoreError::NotFoundOrNotAuthorized)?;
        let events = definition_history(self.state_handle(), definition_id, definition_version)?;
        Ok(WorkflowAggregateSnapshot { projection, events })
    }

    fn history(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
    ) -> Result<Vec<WorkflowDefinitionEventV1>, WorkflowStateStoreError> {
        definition_history(self.state_handle(), definition_id, definition_version)
    }

    fn append(
        &self,
        request: &WorkflowAggregateAppendRequest<WorkflowDefinitionEventV1>,
    ) -> Result<WorkflowAppendOutcome<WorkflowDefinitionProjectionV1>, WorkflowStateStoreError>
    {
        let transaction = self.state_handle().begin_immediate().map_err(unavailable)?;
        let event = &request.event;
        let id = event.definition_id();
        let version = event.definition_version();
        if let Some(prior_digest) = definition_command_digest(&transaction, event)? {
            let current = definition_head_tx(&transaction, id, version)?
                .ok_or(WorkflowStateStoreError::Unavailable)?;
            let result = if prior_digest == event.input_digest().as_str() {
                Ok(WorkflowAppendOutcome::Replayed(current))
            } else {
                Err(WorkflowStateStoreError::IdempotencyConflict)
            };
            let _ = transaction.rollback();
            return result;
        }
        let current = definition_head_tx(&transaction, id, version)?;
        if current.as_ref().map(|head| head.aggregate_version()) != request.expected_version {
            let _ = transaction.rollback();
            return Err(WorkflowStateStoreError::VersionConflict);
        }
        let next = match current {
            Some(head) => head.apply(event),
            None => WorkflowDefinitionProjectionV1::rebuild(std::slice::from_ref(event)),
        }
        .map_err(WorkflowStateStoreError::InvalidHistory)?;
        insert_definition_event(&transaction, event)?;
        publish_definition_head(&transaction, &next)?;
        transaction.commit().map_err(unavailable)?;
        Ok(WorkflowAppendOutcome::Appended(next))
    }
}

impl WorkflowRunEventStorePort for WorkflowSqliteAuthority {
    fn load(
        &self,
        run_id: &RunId,
    ) -> Result<
        WorkflowAggregateSnapshot<WorkflowRunProjectionV1, WorkflowRunEventV1>,
        WorkflowStateStoreError,
    > {
        let projection = run_head(self.state_handle(), run_id)?
            .ok_or(WorkflowStateStoreError::NotFoundOrNotAuthorized)?;
        let events = run_history(self.state_handle(), run_id)?;
        Ok(WorkflowAggregateSnapshot { projection, events })
    }

    fn history(&self, run_id: &RunId) -> Result<Vec<WorkflowRunEventV1>, WorkflowStateStoreError> {
        run_history(self.state_handle(), run_id)
    }

    fn append(
        &self,
        request: &WorkflowAggregateAppendRequest<WorkflowRunEventV1>,
    ) -> Result<WorkflowAppendOutcome<WorkflowRunProjectionV1>, WorkflowStateStoreError> {
        let transaction = self.state_handle().begin_immediate().map_err(unavailable)?;
        let event = &request.event;
        if let Some(prior_digest) = run_command_digest(&transaction, event)? {
            let current = run_head_tx(&transaction, event.run_id())?
                .ok_or(WorkflowStateStoreError::Unavailable)?;
            let result = if prior_digest == event.input_digest().as_str() {
                Ok(WorkflowAppendOutcome::Replayed(current))
            } else {
                Err(WorkflowStateStoreError::IdempotencyConflict)
            };
            let _ = transaction.rollback();
            return result;
        }
        let current = run_head_tx(&transaction, event.run_id())?;
        if current.as_ref().map(|head| head.aggregate_version()) != request.expected_version {
            let _ = transaction.rollback();
            return Err(WorkflowStateStoreError::VersionConflict);
        }
        let next = match current {
            Some(head) => head.apply(event),
            None => WorkflowRunProjectionV1::rebuild(std::slice::from_ref(event)),
        }
        .map_err(WorkflowStateStoreError::InvalidHistory)?;
        insert_run_event(&transaction, event)?;
        publish_run_head(&transaction, &next)?;
        transaction.commit().map_err(unavailable)?;
        Ok(WorkflowAppendOutcome::Appended(next))
    }
}

fn definition_history(
    handle: &MigrationSqlHandle,
    definition_id: &WorkflowDefinitionId,
    definition_version: u64,
) -> Result<Vec<WorkflowDefinitionEventV1>, WorkflowStateStoreError> {
    let rows = query_handle(
        handle,
        "SELECT event_payload FROM workflow_definition_events_v1
         WHERE definition_id = ?1 AND definition_version = ?2 ORDER BY aggregate_version",
        vec![
            MigrationSqlValue::Text(definition_id.as_str().to_owned()),
            MigrationSqlValue::Integer(version_i64(definition_version)?),
        ],
    )
    .map_err(unavailable)?;
    decode_events(rows)
}

fn run_history(
    handle: &MigrationSqlHandle,
    run_id: &RunId,
) -> Result<Vec<WorkflowRunEventV1>, WorkflowStateStoreError> {
    let rows = query_handle(
        handle,
        "SELECT event_payload FROM workflow_run_events_v1
         WHERE run_id = ?1 ORDER BY aggregate_version",
        vec![MigrationSqlValue::Text(run_id.as_str().to_owned())],
    )
    .map_err(unavailable)?;
    decode_events(rows)
}

fn decode_events<T: serde::de::DeserializeOwned>(
    rows: MigrationSqlRows,
) -> Result<Vec<T>, WorkflowStateStoreError> {
    if rows.rows.is_empty() {
        return Err(WorkflowStateStoreError::NotFoundOrNotAuthorized);
    }
    rows.rows
        .into_iter()
        .map(|row| {
            migration_text(&row.values, 0)
                .ok_or(WorkflowStateStoreError::Unavailable)
                .and_then(|payload| {
                    serde_json::from_str(payload).map_err(|_| WorkflowStateStoreError::Unavailable)
                })
        })
        .collect()
}

fn definition_head(
    handle: &MigrationSqlHandle,
    definition_id: &WorkflowDefinitionId,
    definition_version: u64,
) -> Result<Option<WorkflowDefinitionProjectionV1>, WorkflowStateStoreError> {
    decode_optional_head(
        query_handle(
            handle,
            "SELECT projection_payload FROM workflow_definition_heads_v1
             WHERE definition_id = ?1 AND definition_version = ?2",
            vec![
                MigrationSqlValue::Text(definition_id.as_str().to_owned()),
                MigrationSqlValue::Integer(version_i64(definition_version)?),
            ],
        )
        .map_err(unavailable)?,
    )
}

fn definition_head_tx(
    transaction: &MigrationSqlTransaction,
    definition_id: &WorkflowDefinitionId,
    definition_version: u64,
) -> Result<Option<WorkflowDefinitionProjectionV1>, WorkflowStateStoreError> {
    decode_optional_head(
        query_tx(
            transaction,
            "SELECT projection_payload FROM workflow_definition_heads_v1
             WHERE definition_id = ?1 AND definition_version = ?2",
            vec![
                MigrationSqlValue::Text(definition_id.as_str().to_owned()),
                MigrationSqlValue::Integer(version_i64(definition_version)?),
            ],
        )
        .map_err(unavailable)?,
    )
}

fn run_head(
    handle: &MigrationSqlHandle,
    run_id: &RunId,
) -> Result<Option<WorkflowRunProjectionV1>, WorkflowStateStoreError> {
    decode_optional_head(
        query_handle(
            handle,
            "SELECT projection_payload FROM workflow_run_heads_v1 WHERE run_id = ?1",
            vec![MigrationSqlValue::Text(run_id.as_str().to_owned())],
        )
        .map_err(unavailable)?,
    )
}

fn run_head_tx(
    transaction: &MigrationSqlTransaction,
    run_id: &RunId,
) -> Result<Option<WorkflowRunProjectionV1>, WorkflowStateStoreError> {
    decode_optional_head(
        query_tx(
            transaction,
            "SELECT projection_payload FROM workflow_run_heads_v1 WHERE run_id = ?1",
            vec![MigrationSqlValue::Text(run_id.as_str().to_owned())],
        )
        .map_err(unavailable)?,
    )
}

fn decode_optional_head<T: serde::de::DeserializeOwned>(
    rows: MigrationSqlRows,
) -> Result<Option<T>, WorkflowStateStoreError> {
    rows.rows
        .first()
        .map(|row| {
            let payload =
                migration_text(&row.values, 0).ok_or(WorkflowStateStoreError::Unavailable)?;
            serde_json::from_str(payload).map_err(|_| WorkflowStateStoreError::Unavailable)
        })
        .transpose()
}

fn definition_command_digest(
    transaction: &MigrationSqlTransaction,
    event: &WorkflowDefinitionEventV1,
) -> Result<Option<String>, WorkflowStateStoreError> {
    command_digest(
        query_tx(
            transaction,
            "SELECT input_digest FROM workflow_definition_events_v1
             WHERE definition_id = ?1 AND definition_version = ?2 AND command_id = ?3",
            vec![
                MigrationSqlValue::Text(event.definition_id().as_str().to_owned()),
                MigrationSqlValue::Integer(version_i64(event.definition_version())?),
                MigrationSqlValue::Text(event.command_id().as_str().to_owned()),
            ],
        )
        .map_err(unavailable)?,
    )
}

fn run_command_digest(
    transaction: &MigrationSqlTransaction,
    event: &WorkflowRunEventV1,
) -> Result<Option<String>, WorkflowStateStoreError> {
    command_digest(
        query_tx(
            transaction,
            "SELECT input_digest FROM workflow_run_events_v1
             WHERE run_id = ?1 AND command_id = ?2",
            vec![
                MigrationSqlValue::Text(event.run_id().as_str().to_owned()),
                MigrationSqlValue::Text(event.command_id().as_str().to_owned()),
            ],
        )
        .map_err(unavailable)?,
    )
}

fn command_digest(rows: MigrationSqlRows) -> Result<Option<String>, WorkflowStateStoreError> {
    rows.rows
        .first()
        .map(|row| {
            migration_text(&row.values, 0)
                .map(str::to_owned)
                .ok_or(WorkflowStateStoreError::Unavailable)
        })
        .transpose()
}

fn insert_definition_event(
    transaction: &MigrationSqlTransaction,
    event: &WorkflowDefinitionEventV1,
) -> Result<(), WorkflowStateStoreError> {
    execute_tx(
        transaction,
        "INSERT INTO workflow_definition_events_v1 (
             definition_id, definition_version, aggregate_version, command_id,
             input_digest, occurred_at, event_payload
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        vec![
            MigrationSqlValue::Text(event.definition_id().as_str().to_owned()),
            MigrationSqlValue::Integer(version_i64(event.definition_version())?),
            MigrationSqlValue::Integer(version_i64(event.aggregate_version().get())?),
            MigrationSqlValue::Text(event.command_id().as_str().to_owned()),
            MigrationSqlValue::Text(event.input_digest().as_str().to_owned()),
            MigrationSqlValue::Integer(event.occurred_at().0),
            MigrationSqlValue::Text(encode(event)?),
        ],
    )
    .map_err(unavailable)
}

fn publish_definition_head(
    transaction: &MigrationSqlTransaction,
    projection: &WorkflowDefinitionProjectionV1,
) -> Result<(), WorkflowStateStoreError> {
    execute_tx(
        transaction,
        "INSERT INTO workflow_definition_heads_v1 (
             definition_id, definition_version, aggregate_version, projection_payload
         ) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(definition_id, definition_version) DO UPDATE SET
             aggregate_version = excluded.aggregate_version,
             projection_payload = excluded.projection_payload",
        vec![
            MigrationSqlValue::Text(projection.definition().definition_id().as_str().to_owned()),
            MigrationSqlValue::Integer(version_i64(projection.definition().definition_version())?),
            MigrationSqlValue::Integer(version_i64(projection.aggregate_version().get())?),
            MigrationSqlValue::Text(encode(projection)?),
        ],
    )
    .map_err(unavailable)
}

fn insert_run_event(
    transaction: &MigrationSqlTransaction,
    event: &WorkflowRunEventV1,
) -> Result<(), WorkflowStateStoreError> {
    execute_tx(
        transaction,
        "INSERT INTO workflow_run_events_v1 (
             run_id, aggregate_version, command_id, input_digest, occurred_at, event_payload
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        vec![
            MigrationSqlValue::Text(event.run_id().as_str().to_owned()),
            MigrationSqlValue::Integer(version_i64(event.aggregate_version().get())?),
            MigrationSqlValue::Text(event.command_id().as_str().to_owned()),
            MigrationSqlValue::Text(event.input_digest().as_str().to_owned()),
            MigrationSqlValue::Integer(event.occurred_at().0),
            MigrationSqlValue::Text(encode(event)?),
        ],
    )
    .map_err(unavailable)
}

fn publish_run_head(
    transaction: &MigrationSqlTransaction,
    projection: &WorkflowRunProjectionV1,
) -> Result<(), WorkflowStateStoreError> {
    execute_tx(
        transaction,
        "INSERT INTO workflow_run_heads_v1 (run_id, aggregate_version, projection_payload)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(run_id) DO UPDATE SET
             aggregate_version = excluded.aggregate_version,
             projection_payload = excluded.projection_payload",
        vec![
            MigrationSqlValue::Text(projection.run_id().as_str().to_owned()),
            MigrationSqlValue::Integer(version_i64(projection.aggregate_version().get())?),
            MigrationSqlValue::Text(encode(projection)?),
        ],
    )
    .map_err(unavailable)
}

fn encode<T: serde::Serialize>(value: &T) -> Result<String, WorkflowStateStoreError> {
    serde_json::to_string(value).map_err(|_| WorkflowStateStoreError::Unavailable)
}

fn version_i64(value: u64) -> Result<i64, WorkflowStateStoreError> {
    i64::try_from(value).map_err(|_| WorkflowStateStoreError::Unavailable)
}

fn unavailable(_: impl std::fmt::Debug) -> WorkflowStateStoreError {
    WorkflowStateStoreError::Unavailable
}
