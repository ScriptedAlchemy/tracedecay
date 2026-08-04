//! Durable workflow authority over the canonical Work registered SQL channel.
//!
//! Definitions, handoffs, and execution fencing share the exact
//! `ExactSqlHandle` owned by `WorkSqliteStorage`. This module never opens a
//! private connection or creates a second Work authority.

use std::time::Duration;

use tracedecay_application::{
    TaskHandoffAuthorityError, TaskHandoffAuthorityPort, TaskHandoffConsumeOutcome,
    TaskHandoffGrantV1, TaskHandoffScopeV1, WorkflowDefinitionAuthorityError,
    WorkflowDefinitionAuthorityPort, WorkflowExecutionAdmissionV1, WorkflowExecutionAuthorityError,
    WorkflowExecutionAuthorityPort, WorkflowExecutionFenceV1, WorkflowExecutionIdentityV1,
    WorkflowExecutionTruthV1, WorkflowFanOutCheckpointV1,
};
use tracedecay_domain::{
    AttemptId, ManifestDigest, UtcMicros, WorkFenceEpochV1, WorkLeaseFenceV1, WorkLeaseId,
    WorkflowDefinitionId, WorkflowDefinitionV1, canonical_sha256,
};

use crate::exact_sql::{
    ExactSqlError, ExactSqlHandle, ExactSqlRows, ExactSqlStatement, ExactSqlTransaction,
    ExactSqlValue,
};
use crate::work::WorkSqliteStorage;

const WORKFLOW_SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS workflow_definitions_v1 (
    definition_id TEXT NOT NULL,
    definition_version INTEGER NOT NULL CHECK (definition_version > 0),
    payload TEXT NOT NULL,
    payload_digest TEXT NOT NULL,
    PRIMARY KEY (definition_id, definition_version)
) STRICT;

CREATE TABLE IF NOT EXISTS workflow_activations_v1 (
    definition_id TEXT NOT NULL PRIMARY KEY,
    active_version INTEGER NOT NULL CHECK (active_version > 0)
) STRICT;

CREATE TABLE IF NOT EXISTS workflow_handoffs_v1 (
    token_digest TEXT NOT NULL PRIMARY KEY,
    scope_payload TEXT NOT NULL,
    issued_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL CHECK (expires_at > issued_at),
    consumed INTEGER NOT NULL CHECK (consumed IN (0, 1))
) STRICT;

CREATE TABLE IF NOT EXISTS workflow_executions_v1 (
    definition_id TEXT NOT NULL,
    definition_version INTEGER NOT NULL CHECK (definition_version > 0),
    run_id TEXT NOT NULL,
    step_id TEXT NOT NULL,
    plan_digest TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    lease_id TEXT NOT NULL,
    fence_epoch INTEGER NOT NULL CHECK (fence_epoch > 0),
    checkpoint_payload TEXT,
    terminal_payload TEXT,
    PRIMARY KEY (definition_id, definition_version, run_id, step_id)
) STRICT;
";

/// Workflow persistence on the registered Work exact-SQL handle.
#[derive(Clone)]
pub struct WorkflowSqliteAuthority {
    handle: ExactSqlHandle,
}

impl WorkflowSqliteAuthority {
    /// Clone the crate-visible Work handle and install workflow tables through it.
    pub fn from_work_storage(
        storage: &WorkSqliteStorage,
    ) -> Result<Self, WorkflowSqliteAuthorityBuildError> {
        let authority = Self {
            handle: storage.handle.clone(),
        };
        authority.install_schema()?;
        Ok(authority)
    }

    fn install_schema(&self) -> Result<(), WorkflowSqliteAuthorityBuildError> {
        self.handle
            .execute_batch(WORKFLOW_SCHEMA_V1.to_owned())
            .map(|_| ())
            .map_err(|_| WorkflowSqliteAuthorityBuildError::Unavailable)
    }
}

/// Construction failure for the durable workflow authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowSqliteAuthorityBuildError {
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

fn execution_unavailable(_: ExactSqlError) -> WorkflowExecutionAuthorityError {
    WorkflowExecutionAuthorityError::Unavailable(
        "workflow execution authority unavailable".to_owned(),
    )
}

fn execution_codec_unavailable() -> WorkflowExecutionAuthorityError {
    WorkflowExecutionAuthorityError::Unavailable(
        "workflow execution authority unavailable".to_owned(),
    )
}

fn statement(sql: &str, params: Vec<ExactSqlValue>) -> Result<ExactSqlStatement, ExactSqlError> {
    ExactSqlStatement::new(sql.to_owned(), params)
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

#[derive(Clone)]
struct StoredExecution {
    plan_digest: ManifestDigest,
    fence: WorkflowExecutionFenceV1,
    checkpoint: Option<WorkflowFanOutCheckpointV1>,
    terminal: Option<WorkflowExecutionTruthV1>,
}

fn identity_params(identity: &WorkflowExecutionIdentityV1) -> Result<Vec<ExactSqlValue>, ()> {
    Ok(vec![
        ExactSqlValue::Text(identity.definition_id.as_str().to_owned()),
        ExactSqlValue::Integer(version_i64(identity.definition_version)?),
        ExactSqlValue::Text(identity.run_id.as_str().to_owned()),
        ExactSqlValue::Text(identity.step_id.as_str().to_owned()),
    ])
}

fn load_execution(
    transaction: &ExactSqlTransaction,
    identity: &WorkflowExecutionIdentityV1,
) -> Result<Option<StoredExecution>, WorkflowExecutionAuthorityError> {
    let rows = query_tx(
        transaction,
        "SELECT plan_digest, attempt_id, lease_id, fence_epoch, checkpoint_payload, terminal_payload
         FROM workflow_executions_v1
         WHERE definition_id = ?1
           AND definition_version = ?2
           AND run_id = ?3
           AND step_id = ?4",
        identity_params(identity).map_err(|_| execution_codec_unavailable())?,
    )
    .map_err(execution_unavailable)?;
    let Some(row) = rows.rows.first() else {
        return Ok(None);
    };
    let plan_digest = ManifestDigest::new(
        exact_sql_text(&row.values, 0)
            .ok_or_else(execution_codec_unavailable)?
            .to_owned(),
    )
    .map_err(|_| execution_codec_unavailable())?;
    let attempt_id = AttemptId::new(
        exact_sql_text(&row.values, 1)
            .ok_or_else(execution_codec_unavailable)?
            .to_owned(),
    )
    .map_err(|_| execution_codec_unavailable())?;
    let lease_id = WorkLeaseId::new(
        exact_sql_text(&row.values, 2)
            .ok_or_else(execution_codec_unavailable)?
            .to_owned(),
    )
    .map_err(|_| execution_codec_unavailable())?;
    let fence_epoch = exact_sql_integer(&row.values, 3).ok_or_else(execution_codec_unavailable)?;
    let fence = WorkflowExecutionFenceV1 {
        attempt_id,
        lease: WorkLeaseFenceV1::new(
            lease_id,
            WorkFenceEpochV1::new(
                version_u64(fence_epoch).map_err(|_| execution_codec_unavailable())?,
            )
            .map_err(|_| execution_codec_unavailable())?,
        )
        .map_err(|_| execution_codec_unavailable())?,
    };
    let checkpoint = match row.values.get(4) {
        Some(ExactSqlValue::Null) | None => None,
        Some(ExactSqlValue::Text(payload)) => {
            Some(decode_json(payload).map_err(|_| execution_codec_unavailable())?)
        }
        _ => return Err(execution_codec_unavailable()),
    };
    let terminal = match row.values.get(5) {
        Some(ExactSqlValue::Null) | None => None,
        Some(ExactSqlValue::Text(payload)) => {
            Some(decode_json(payload).map_err(|_| execution_codec_unavailable())?)
        }
        _ => return Err(execution_codec_unavailable()),
    };
    Ok(Some(StoredExecution {
        plan_digest,
        fence,
        checkpoint,
        terminal,
    }))
}

fn insert_execution(
    transaction: &ExactSqlTransaction,
    identity: &WorkflowExecutionIdentityV1,
    fence: &WorkflowExecutionFenceV1,
    plan_digest: &ManifestDigest,
) -> Result<(), WorkflowExecutionAuthorityError> {
    let mut params = identity_params(identity).map_err(|_| execution_codec_unavailable())?;
    params.extend([
        ExactSqlValue::Text(plan_digest.as_str().to_owned()),
        ExactSqlValue::Text(fence.attempt_id.as_str().to_owned()),
        ExactSqlValue::Text(fence.lease.lease_id().as_str().to_owned()),
        ExactSqlValue::Integer(
            version_i64(fence.lease.epoch().get()).map_err(|_| execution_codec_unavailable())?,
        ),
    ]);
    execute_tx(
        transaction,
        "INSERT INTO workflow_executions_v1 (
             definition_id, definition_version, run_id, step_id,
             plan_digest, attempt_id, lease_id, fence_epoch,
             checkpoint_payload, terminal_payload
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, NULL)",
        params,
    )
    .map_err(execution_unavailable)
}

fn update_execution_fence(
    transaction: &ExactSqlTransaction,
    identity: &WorkflowExecutionIdentityV1,
    fence: &WorkflowExecutionFenceV1,
) -> Result<(), WorkflowExecutionAuthorityError> {
    let mut params = vec![
        ExactSqlValue::Text(fence.attempt_id.as_str().to_owned()),
        ExactSqlValue::Text(fence.lease.lease_id().as_str().to_owned()),
        ExactSqlValue::Integer(
            version_i64(fence.lease.epoch().get()).map_err(|_| execution_codec_unavailable())?,
        ),
    ];
    params.extend(identity_params(identity).map_err(|_| execution_codec_unavailable())?);
    execute_tx(
        transaction,
        "UPDATE workflow_executions_v1
         SET attempt_id = ?1, lease_id = ?2, fence_epoch = ?3
         WHERE definition_id = ?4
           AND definition_version = ?5
           AND run_id = ?6
           AND step_id = ?7",
        params,
    )
    .map_err(execution_unavailable)
}

impl WorkflowExecutionAuthorityPort for WorkflowSqliteAuthority {
    fn begin(
        &self,
        identity: &WorkflowExecutionIdentityV1,
        fence: &WorkflowExecutionFenceV1,
        plan_digest: &ManifestDigest,
    ) -> Result<WorkflowExecutionAdmissionV1, WorkflowExecutionAuthorityError> {
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(execution_unavailable)?;
        let stored = load_execution(&transaction, identity)?;
        let Some(stored) = stored else {
            insert_execution(&transaction, identity, fence, plan_digest)?;
            transaction.commit().map_err(execution_unavailable)?;
            return Ok(WorkflowExecutionAdmissionV1::Execute);
        };

        if stored.fence.attempt_id != fence.attempt_id
            || stored.fence.lease.lease_id() != fence.lease.lease_id()
            || stored.fence.lease.epoch().get() > fence.lease.epoch().get()
        {
            let _ = transaction.rollback();
            return Ok(WorkflowExecutionAdmissionV1::StaleLease);
        }

        if let Some(terminal) = stored.terminal {
            if &stored.plan_digest != plan_digest {
                let _ = transaction.rollback();
                return Ok(WorkflowExecutionAdmissionV1::PlanConflict);
            }
            if stored.fence != *fence {
                update_execution_fence(&transaction, identity, fence)?;
                transaction.commit().map_err(execution_unavailable)?;
            } else {
                let _ = transaction.rollback();
            }
            return Ok(WorkflowExecutionAdmissionV1::Replay(terminal));
        }

        if &stored.plan_digest != plan_digest {
            let _ = transaction.rollback();
            return Ok(WorkflowExecutionAdmissionV1::PlanConflict);
        }

        if stored.fence != *fence {
            update_execution_fence(&transaction, identity, fence)?;
        }
        let admission = match stored.checkpoint {
            Some(checkpoint) => WorkflowExecutionAdmissionV1::Recover { checkpoint },
            None => WorkflowExecutionAdmissionV1::Execute,
        };
        transaction.commit().map_err(execution_unavailable)?;
        Ok(admission)
    }

    fn checkpoint(
        &self,
        identity: &WorkflowExecutionIdentityV1,
        fence: &WorkflowExecutionFenceV1,
        checkpoint: &WorkflowFanOutCheckpointV1,
    ) -> Result<(), WorkflowExecutionAuthorityError> {
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(execution_unavailable)?;
        let stored = load_execution(&transaction, identity)?
            .ok_or(WorkflowExecutionAuthorityError::Conflict)?;
        if stored.terminal.is_some()
            || &stored.fence != fence
            || stored.plan_digest != checkpoint.plan_digest
            || !checkpoint_is_well_formed(checkpoint)
            || stored
                .checkpoint
                .as_ref()
                .is_some_and(|current| !checkpoint_advances(current, checkpoint))
        {
            let _ = transaction.rollback();
            return Err(WorkflowExecutionAuthorityError::Conflict);
        }
        let payload = encode_json(checkpoint).map_err(|_| execution_codec_unavailable())?;
        let mut params = vec![ExactSqlValue::Text(payload)];
        params.extend(identity_params(identity).map_err(|_| execution_codec_unavailable())?);
        execute_tx(
            &transaction,
            "UPDATE workflow_executions_v1
             SET checkpoint_payload = ?1
             WHERE definition_id = ?2
               AND definition_version = ?3
               AND run_id = ?4
               AND step_id = ?5",
            params,
        )
        .map_err(execution_unavailable)?;
        transaction
            .commit()
            .map(|_| ())
            .map_err(execution_unavailable)
    }

    fn complete(
        &self,
        identity: &WorkflowExecutionIdentityV1,
        fence: &WorkflowExecutionFenceV1,
        truth: &WorkflowExecutionTruthV1,
    ) -> Result<(), WorkflowExecutionAuthorityError> {
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(execution_unavailable)?;
        let stored = load_execution(&transaction, identity)?
            .ok_or(WorkflowExecutionAuthorityError::Conflict)?;
        let checkpoint = truth.checkpoint();
        if stored.terminal.is_some()
            || &stored.fence != fence
            || stored.plan_digest != checkpoint.plan_digest
            || !checkpoint_is_well_formed(checkpoint)
            || checkpoint
                .children
                .iter()
                .any(|child| child.receipt.is_none())
            || match stored.checkpoint.as_ref() {
                Some(stored) => stored != checkpoint,
                None => !checkpoint.children.is_empty(),
            }
        {
            let _ = transaction.rollback();
            return Err(WorkflowExecutionAuthorityError::Conflict);
        }
        let payload = encode_json(truth).map_err(|_| execution_codec_unavailable())?;
        let mut params = vec![ExactSqlValue::Text(payload)];
        params.extend(identity_params(identity).map_err(|_| execution_codec_unavailable())?);
        execute_tx(
            &transaction,
            "UPDATE workflow_executions_v1
             SET terminal_payload = ?1
             WHERE definition_id = ?2
               AND definition_version = ?3
               AND run_id = ?4
               AND step_id = ?5",
            params,
        )
        .map_err(execution_unavailable)?;
        transaction
            .commit()
            .map(|_| ())
            .map_err(execution_unavailable)
    }
}

fn checkpoint_advances(
    current: &WorkflowFanOutCheckpointV1,
    replacement: &WorkflowFanOutCheckpointV1,
) -> bool {
    checkpoint_is_well_formed(replacement)
        && current.plan_digest == replacement.plan_digest
        && current.children.iter().all(|stored| {
            replacement.children.iter().any(|candidate| {
                stored.task_id == candidate.task_id
                    && stored.attempt_identity == candidate.attempt_identity
                    && stored.lease.lease_id() == candidate.lease.lease_id()
                    && stored.lease.epoch().get() <= candidate.lease.epoch().get()
                    && match (&stored.receipt, &candidate.receipt) {
                        (None, _) => true,
                        (Some(stored_receipt), Some(candidate_receipt)) => {
                            stored.lease == candidate.lease && stored_receipt == candidate_receipt
                        }
                        (Some(_), None) => false,
                    }
            })
        })
}

fn checkpoint_is_well_formed(checkpoint: &WorkflowFanOutCheckpointV1) -> bool {
    checkpoint
        .children
        .iter()
        .enumerate()
        .all(|(index, child)| {
            if child.task_id != *child.attempt_identity.task_id() {
                return false;
            }
            checkpoint.children.iter().skip(index + 1).all(|other| {
                child.task_id != other.task_id
                    && child.attempt_identity != other.attempt_identity
                    && child.lease.lease_id() != other.lease.lease_id()
            })
        })
}
