//! SQLite persistence for canonical authorized scope sets.
//!
//! The executor operates only on an already-open connection. Locator,
//! attachment, migration scheduling, and daemon authority remain with their
//! existing owners.

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use thiserror::Error;
use tracedecay_application::{AuthorizedScopeSet, AuthorizedScopeSetError};
use tracedecay_domain::{ManifestDigest, ScopeSetId, ScopeSetRevision};
use tracedecay_store::runtime::{
    AuthorizedScopeSetRecordV1, ScopeSetCasOutcomeV1, ScopeSetCompareAndSwapV1,
    ScopeSetStoreContractError,
};

use crate::exact_sql::{
    ExactSqlError, ExactSqlHandle, ExactSqlRow, ExactSqlStatement, ExactSqlValue,
};

pub const AUTHORIZED_SCOPE_SET_SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS authorized_scope_sets_v1 (
    scope_set_id TEXT PRIMARY KEY NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    digest TEXT NOT NULL,
    canonical_payload BLOB NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS authorized_scope_set_transactions_v1 (
    idempotency_key TEXT PRIMARY KEY NOT NULL,
    command_digest TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'prepared', 'applied', 'conflict')),
    next_payload BLOB NOT NULL,
    result_payload BLOB
) STRICT;
CREATE TABLE IF NOT EXISTS authorized_scope_set_replica_receipts_v1 (
    idempotency_key TEXT NOT NULL,
    replica_key TEXT NOT NULL,
    authorization_digest TEXT NOT NULL,
    PRIMARY KEY (idempotency_key, replica_key),
    FOREIGN KEY (idempotency_key)
        REFERENCES authorized_scope_set_transactions_v1(idempotency_key)
) STRICT;
";

#[derive(Debug, Error)]
pub enum AuthorizedScopeSetStoreError {
    #[error("authorized scope-set SQLite operation failed")]
    Sqlite(#[from] rusqlite::Error),
    #[error("authorized scope-set serialization failed")]
    Serialization(#[from] serde_json::Error),
    #[error("authorized scope-set application contract failed: {0}")]
    Application(#[from] AuthorizedScopeSetError),
    #[error("authorized scope-set persistence contract failed: {0}")]
    StoreContract(#[from] ScopeSetStoreContractError),
    #[error("authorized scope-set persisted data is invalid: {0}")]
    InvalidData(String),
    #[error("authorized scope-set actor does not match the stored owner")]
    OwnershipMismatch,
    #[error("authorized scope-set idempotency key conflicts with a prior command")]
    IdempotencyConflict,
    #[error(transparent)]
    RegisteredStore(#[from] ExactSqlError),
}

/// Persistence executor for one exact scope-set record.
#[derive(Clone, Copy, Debug, Default)]
pub struct AuthorizedScopeSetExecutor;

impl AuthorizedScopeSetExecutor {
    /// Install the isolated schema into a test or migration-owned connection.
    pub fn install_schema(connection: &Connection) -> Result<(), AuthorizedScopeSetStoreError> {
        connection.execute_batch(AUTHORIZED_SCOPE_SET_SCHEMA_V1)?;
        Ok(())
    }

    pub fn read(
        connection: &Connection,
        scope_set_id: &ScopeSetId,
    ) -> Result<Option<AuthorizedScopeSet>, AuthorizedScopeSetStoreError> {
        let record = read_record(connection, scope_set_id)?;
        record.map(decode_record).transpose()
    }

    pub fn compare_and_swap(
        connection: &mut Connection,
        expected_revision: Option<ScopeSetRevision>,
        next: &AuthorizedScopeSet,
    ) -> Result<ScopeSetCasOutcomeV1, AuthorizedScopeSetStoreError> {
        next.validate()?;
        let payload = serde_json::to_vec(next)?;
        let record = AuthorizedScopeSetRecordV1::new(
            next.scope_set_id().clone(),
            next.revision(),
            next.digest().clone(),
            payload,
        )?;
        let command = ScopeSetCompareAndSwapV1::new(expected_revision, record.clone())?;

        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let actual_revision = read_revision(&transaction, next.scope_set_id())?;
        if actual_revision != command.expected_revision {
            transaction.commit()?;
            return Ok(ScopeSetCasOutcomeV1::Conflict {
                expected_revision: command.expected_revision,
                actual_revision,
            });
        }
        if actual_revision.is_some() {
            let current = read_record(&transaction, next.scope_set_id())?
                .map(decode_record)
                .transpose()?
                .ok_or_else(|| {
                    AuthorizedScopeSetStoreError::InvalidData(
                        "scope-set revision exists without a canonical payload".to_owned(),
                    )
                })?;
            if current.actor_id() != next.actor_id() {
                return Err(AuthorizedScopeSetStoreError::OwnershipMismatch);
            }
        }

        match command.expected_revision {
            None => {
                transaction.execute(
                    "INSERT INTO authorized_scope_sets_v1
                         (scope_set_id, revision, digest, canonical_payload)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        command.next.scope_set_id.as_str(),
                        revision_to_i64(command.next.revision)?,
                        command.next.digest.as_str(),
                        command.next.canonical_payload,
                    ],
                )?;
            }
            Some(expected) => {
                let changed = transaction.execute(
                    "UPDATE authorized_scope_sets_v1
                     SET revision = ?2, digest = ?3, canonical_payload = ?4
                     WHERE scope_set_id = ?1 AND revision = ?5",
                    params![
                        command.next.scope_set_id.as_str(),
                        revision_to_i64(command.next.revision)?,
                        command.next.digest.as_str(),
                        command.next.canonical_payload,
                        revision_to_i64(expected)?,
                    ],
                )?;
                if changed != 1 {
                    return Err(AuthorizedScopeSetStoreError::InvalidData(
                        "scope-set CAS lost its immediate transaction authority".to_owned(),
                    ));
                }
            }
        }
        transaction.commit()?;
        Ok(ScopeSetCasOutcomeV1::Applied(record))
    }
}

/// Scope-set persistence over the exact registered and fenced project store.
#[derive(Clone)]
pub struct AuthorizedScopeSetSqliteStorage {
    handle: ExactSqlHandle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthorizedScopeSetDurableCasV1 {
    Pending(AuthorizedScopeSet),
    Prepared(AuthorizedScopeSet),
    Applied(AuthorizedScopeSet),
    Conflict(Option<AuthorizedScopeSet>),
}

impl AuthorizedScopeSetSqliteStorage {
    pub fn from_registered(handle: ExactSqlHandle) -> Self {
        Self { handle }
    }

    pub fn read(
        &self,
        scope_set_id: &ScopeSetId,
    ) -> Result<Option<AuthorizedScopeSet>, AuthorizedScopeSetStoreError> {
        let rows = self.handle.query(
            registered_read_statement(scope_set_id)?,
            std::time::Duration::from_secs(5),
        )?;
        decode_registered_rows(rows.rows)
    }

    pub fn compare_and_swap(
        &self,
        expected_revision: Option<ScopeSetRevision>,
        next: &AuthorizedScopeSet,
    ) -> Result<ScopeSetCasOutcomeV1, AuthorizedScopeSetStoreError> {
        let transaction = self.handle.begin_immediate()?;
        let current = decode_registered_rows(
            transaction
                .query(registered_read_statement(next.scope_set_id())?)?
                .rows,
        )?;
        let actual_revision = current.as_ref().map(AuthorizedScopeSet::revision);
        if actual_revision != expected_revision {
            transaction.rollback()?;
            return Ok(ScopeSetCasOutcomeV1::Conflict {
                expected_revision,
                actual_revision,
            });
        }
        if current
            .as_ref()
            .is_some_and(|current| current.actor_id() != next.actor_id())
        {
            transaction.rollback()?;
            return Err(AuthorizedScopeSetStoreError::OwnershipMismatch);
        }
        let payload = serde_json::to_vec(next)?;
        transaction.execute(ExactSqlStatement::new(
            "INSERT INTO authorized_scope_sets_v1 (
                 scope_set_id, revision, digest, canonical_payload
             ) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(scope_set_id) DO UPDATE SET
                 revision = excluded.revision,
                 digest = excluded.digest,
                 canonical_payload = excluded.canonical_payload"
                .to_owned(),
            vec![
                ExactSqlValue::Text(next.scope_set_id().as_str().to_owned()),
                ExactSqlValue::Integer(revision_to_i64(next.revision())?),
                ExactSqlValue::Text(next.digest().as_str().to_owned()),
                ExactSqlValue::Blob(payload.clone()),
            ],
        )?)?;
        transaction.commit()?;
        Ok(ScopeSetCasOutcomeV1::Applied(
            AuthorizedScopeSetRecordV1::new(
                next.scope_set_id().clone(),
                next.revision(),
                next.digest().clone(),
                payload,
            )?,
        ))
    }

    pub fn begin_durable_compare_and_swap(
        &self,
        idempotency_key: &str,
        command_digest: &ManifestDigest,
        expected_revision: Option<ScopeSetRevision>,
        next: &AuthorizedScopeSet,
    ) -> Result<AuthorizedScopeSetDurableCasV1, AuthorizedScopeSetStoreError> {
        if idempotency_key.is_empty() || idempotency_key.trim() != idempotency_key {
            return Err(AuthorizedScopeSetStoreError::InvalidData(
                "scope-set idempotency key is not canonical".to_owned(),
            ));
        }
        command_digest
            .validate()
            .map_err(|error| AuthorizedScopeSetStoreError::InvalidData(error.to_string()))?;
        next.validate()?;
        let transaction = self.handle.begin_immediate()?;
        if let Some(replay) = read_durable_cas(&transaction, idempotency_key, command_digest)? {
            transaction.rollback()?;
            return Ok(replay);
        }
        let current = decode_registered_rows(
            transaction
                .query(registered_read_statement(next.scope_set_id())?)?
                .rows,
        )?;
        if current.as_ref().map(AuthorizedScopeSet::revision) != expected_revision {
            insert_durable_cas(
                &transaction,
                idempotency_key,
                command_digest,
                "conflict",
                next,
                current.as_ref(),
            )?;
            transaction.commit()?;
            return Ok(AuthorizedScopeSetDurableCasV1::Conflict(current));
        }
        if current
            .as_ref()
            .is_some_and(|current| current.actor_id() != next.actor_id())
        {
            transaction.rollback()?;
            return Err(AuthorizedScopeSetStoreError::OwnershipMismatch);
        }
        insert_durable_cas(
            &transaction,
            idempotency_key,
            command_digest,
            "pending",
            next,
            None,
        )?;
        transaction.commit()?;
        Ok(AuthorizedScopeSetDurableCasV1::Pending(next.clone()))
    }

    pub fn prepare_durable_replica(
        &self,
        idempotency_key: &str,
        command_digest: &ManifestDigest,
        next: &AuthorizedScopeSet,
    ) -> Result<AuthorizedScopeSetDurableCasV1, AuthorizedScopeSetStoreError> {
        next.validate()?;
        let transaction = self.handle.begin_immediate()?;
        if let Some(replay) = read_durable_cas(&transaction, idempotency_key, command_digest)? {
            transaction.rollback()?;
            return Ok(replay);
        }
        insert_durable_cas(
            &transaction,
            idempotency_key,
            command_digest,
            "pending",
            next,
            None,
        )?;
        transaction.commit()?;
        Ok(AuthorizedScopeSetDurableCasV1::Pending(next.clone()))
    }

    pub fn record_durable_replica(
        &self,
        idempotency_key: &str,
        command_digest: &ManifestDigest,
        replica_key: &ManifestDigest,
        authorization_digest: &ManifestDigest,
    ) -> Result<(), AuthorizedScopeSetStoreError> {
        replica_key
            .validate()
            .map_err(|error| AuthorizedScopeSetStoreError::InvalidData(error.to_string()))?;
        authorization_digest
            .validate()
            .map_err(|error| AuthorizedScopeSetStoreError::InvalidData(error.to_string()))?;
        let transaction = self.handle.begin_immediate()?;
        let Some(state) = read_durable_cas(&transaction, idempotency_key, command_digest)? else {
            transaction.rollback()?;
            return Err(AuthorizedScopeSetStoreError::InvalidData(
                "scope-set durable CAS journal is missing".to_owned(),
            ));
        };
        if !matches!(state, AuthorizedScopeSetDurableCasV1::Pending(_)) {
            transaction.rollback()?;
            return Err(AuthorizedScopeSetStoreError::InvalidData(
                "scope-set terminal journal cannot accept replica receipts".to_owned(),
            ));
        }
        transaction.execute(ExactSqlStatement::new(
            "INSERT INTO authorized_scope_set_replica_receipts_v1
                 (idempotency_key, replica_key, authorization_digest)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(idempotency_key, replica_key) DO UPDATE SET
                 authorization_digest = excluded.authorization_digest"
                .to_owned(),
            vec![
                ExactSqlValue::Text(idempotency_key.to_owned()),
                ExactSqlValue::Text(replica_key.as_str().to_owned()),
                ExactSqlValue::Text(authorization_digest.as_str().to_owned()),
            ],
        )?)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn complete_durable_compare_and_swap(
        &self,
        idempotency_key: &str,
        command_digest: &ManifestDigest,
        expected_replicas: &[(ManifestDigest, ManifestDigest)],
    ) -> Result<AuthorizedScopeSetDurableCasV1, AuthorizedScopeSetStoreError> {
        if expected_replicas.is_empty() {
            return Err(AuthorizedScopeSetStoreError::InvalidData(
                "scope-set durable CAS requires at least one replica".to_owned(),
            ));
        }
        for (replica_key, authorization_digest) in expected_replicas {
            replica_key
                .validate()
                .map_err(|error| AuthorizedScopeSetStoreError::InvalidData(error.to_string()))?;
            authorization_digest
                .validate()
                .map_err(|error| AuthorizedScopeSetStoreError::InvalidData(error.to_string()))?;
        }
        let transaction = self.handle.begin_immediate()?;
        let Some(state) = read_durable_cas(&transaction, idempotency_key, command_digest)? else {
            transaction.rollback()?;
            return Err(AuthorizedScopeSetStoreError::InvalidData(
                "scope-set durable CAS journal is missing".to_owned(),
            ));
        };
        match state {
            AuthorizedScopeSetDurableCasV1::Prepared(_) => {
                transaction.rollback()?;
                return Err(AuthorizedScopeSetStoreError::InvalidData(
                    "scope-set coordinator journal contains a participant prepare".to_owned(),
                ));
            }
            AuthorizedScopeSetDurableCasV1::Applied(_)
            | AuthorizedScopeSetDurableCasV1::Conflict(_) => {
                transaction.rollback()?;
                return Ok(state);
            }
            AuthorizedScopeSetDurableCasV1::Pending(next) => {
                let rows = transaction
                    .query(ExactSqlStatement::new(
                        "SELECT replica_key, authorization_digest
                         FROM authorized_scope_set_replica_receipts_v1
                         WHERE idempotency_key = ?1
                         ORDER BY replica_key"
                            .to_owned(),
                        vec![ExactSqlValue::Text(idempotency_key.to_owned())],
                    )?)?
                    .rows;
                let mut actual = rows
                    .into_iter()
                    .map(|row| match row.values.as_slice() {
                        [
                            ExactSqlValue::Text(replica_key),
                            ExactSqlValue::Text(authorization_digest),
                        ] => Ok((
                            ManifestDigest::new(replica_key.clone()).map_err(|error| {
                                AuthorizedScopeSetStoreError::InvalidData(error.to_string())
                            })?,
                            ManifestDigest::new(authorization_digest.clone()).map_err(|error| {
                                AuthorizedScopeSetStoreError::InvalidData(error.to_string())
                            })?,
                        )),
                        _ => Err(AuthorizedScopeSetStoreError::InvalidData(
                            "scope-set replica receipt has an invalid shape".to_owned(),
                        )),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                actual.sort();
                let mut expected = expected_replicas.to_vec();
                expected.sort();
                if expected
                    .windows(2)
                    .any(|pair| pair[0].0.as_str() == pair[1].0.as_str())
                {
                    transaction.rollback()?;
                    return Err(AuthorizedScopeSetStoreError::InvalidData(
                        "scope-set durable CAS has duplicate replica keys".to_owned(),
                    ));
                }
                if actual != expected {
                    transaction.rollback()?;
                    return Ok(AuthorizedScopeSetDurableCasV1::Pending(next));
                }
                let current = decode_registered_rows(
                    transaction
                        .query(registered_read_statement(next.scope_set_id())?)?
                        .rows,
                )?;
                let expected_revision = match next.revision().get() {
                    1 => None,
                    revision => Some(ScopeSetRevision::new(revision - 1).map_err(|error| {
                        AuthorizedScopeSetStoreError::InvalidData(error.to_string())
                    })?),
                };
                if current.as_ref().map(AuthorizedScopeSet::revision) != expected_revision {
                    let result_payload = current
                        .as_ref()
                        .map(serde_json::to_vec)
                        .transpose()?
                        .map_or(ExactSqlValue::Null, ExactSqlValue::Blob);
                    transaction.execute(ExactSqlStatement::new(
                        "UPDATE authorized_scope_set_transactions_v1
                         SET status = 'conflict', result_payload = ?2
                         WHERE idempotency_key = ?1 AND status = 'pending'"
                            .to_owned(),
                        vec![
                            ExactSqlValue::Text(idempotency_key.to_owned()),
                            result_payload,
                        ],
                    )?)?;
                    transaction.commit()?;
                    return Ok(AuthorizedScopeSetDurableCasV1::Conflict(current));
                }
                if current
                    .as_ref()
                    .is_some_and(|current| current.actor_id() != next.actor_id())
                {
                    transaction.rollback()?;
                    return Err(AuthorizedScopeSetStoreError::OwnershipMismatch);
                }
                write_registered_scope_set(&transaction, &next)?;
                transaction.execute(ExactSqlStatement::new(
                    "UPDATE authorized_scope_set_transactions_v1
                     SET status = 'applied', result_payload = next_payload
                     WHERE idempotency_key = ?1 AND status = 'pending'"
                        .to_owned(),
                    vec![ExactSqlValue::Text(idempotency_key.to_owned())],
                )?)?;
                transaction.commit()?;
                Ok(AuthorizedScopeSetDurableCasV1::Applied(next))
            }
        }
    }

    /// Mark a participant prepare terminal without making it readable.
    ///
    /// Canonical visibility belongs exclusively to the coordinator store.
    pub fn complete_durable_replica(
        &self,
        idempotency_key: &str,
        command_digest: &ManifestDigest,
    ) -> Result<AuthorizedScopeSetDurableCasV1, AuthorizedScopeSetStoreError> {
        let transaction = self.handle.begin_immediate()?;
        let Some(state) = read_durable_cas(&transaction, idempotency_key, command_digest)? else {
            transaction.rollback()?;
            return Err(AuthorizedScopeSetStoreError::InvalidData(
                "scope-set durable replica journal is missing".to_owned(),
            ));
        };
        let AuthorizedScopeSetDurableCasV1::Pending(next) = state else {
            transaction.rollback()?;
            return Ok(state);
        };
        transaction.execute(ExactSqlStatement::new(
            "UPDATE authorized_scope_set_transactions_v1
             SET status = 'prepared', result_payload = next_payload
             WHERE idempotency_key = ?1 AND status = 'pending'"
                .to_owned(),
            vec![ExactSqlValue::Text(idempotency_key.to_owned())],
        )?)?;
        transaction.commit()?;
        Ok(AuthorizedScopeSetDurableCasV1::Prepared(next))
    }

    /// Remove a non-terminal replica prepare after the canonical coordinator
    /// has either published or rejected the command.
    pub fn discard_durable_prepare(
        &self,
        idempotency_key: &str,
        command_digest: &ManifestDigest,
    ) -> Result<(), AuthorizedScopeSetStoreError> {
        let transaction = self.handle.begin_immediate()?;
        let Some(state) = read_durable_cas(&transaction, idempotency_key, command_digest)? else {
            transaction.rollback()?;
            return Ok(());
        };
        if !matches!(state, AuthorizedScopeSetDurableCasV1::Pending(_)) {
            transaction.rollback()?;
            return Ok(());
        }
        transaction.execute(ExactSqlStatement::new(
            "DELETE FROM authorized_scope_set_replica_receipts_v1
             WHERE idempotency_key = ?1"
                .to_owned(),
            vec![ExactSqlValue::Text(idempotency_key.to_owned())],
        )?)?;
        transaction.execute(ExactSqlStatement::new(
            "DELETE FROM authorized_scope_set_transactions_v1
             WHERE idempotency_key = ?1 AND status = 'pending'"
                .to_owned(),
            vec![ExactSqlValue::Text(idempotency_key.to_owned())],
        )?)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn conflict_durable_compare_and_swap(
        &self,
        idempotency_key: &str,
        command_digest: &ManifestDigest,
        current: Option<&AuthorizedScopeSet>,
    ) -> Result<AuthorizedScopeSetDurableCasV1, AuthorizedScopeSetStoreError> {
        let transaction = self.handle.begin_immediate()?;
        let Some(state) = read_durable_cas(&transaction, idempotency_key, command_digest)? else {
            transaction.rollback()?;
            return Err(AuthorizedScopeSetStoreError::InvalidData(
                "scope-set durable CAS journal is missing".to_owned(),
            ));
        };
        match state {
            AuthorizedScopeSetDurableCasV1::Prepared(_) => {
                transaction.rollback()?;
                Err(AuthorizedScopeSetStoreError::InvalidData(
                    "scope-set coordinator journal contains a participant prepare".to_owned(),
                ))
            }
            AuthorizedScopeSetDurableCasV1::Applied(_)
            | AuthorizedScopeSetDurableCasV1::Conflict(_) => {
                transaction.rollback()?;
                Ok(state)
            }
            AuthorizedScopeSetDurableCasV1::Pending(_) => {
                let result_payload = current
                    .map(serde_json::to_vec)
                    .transpose()?
                    .map_or(ExactSqlValue::Null, ExactSqlValue::Blob);
                transaction.execute(ExactSqlStatement::new(
                    "UPDATE authorized_scope_set_transactions_v1
                     SET status = 'conflict', result_payload = ?2
                     WHERE idempotency_key = ?1 AND status = 'pending'"
                        .to_owned(),
                    vec![
                        ExactSqlValue::Text(idempotency_key.to_owned()),
                        result_payload,
                    ],
                )?)?;
                transaction.commit()?;
                Ok(AuthorizedScopeSetDurableCasV1::Conflict(current.cloned()))
            }
        }
    }
}

fn read_durable_cas(
    transaction: &crate::exact_sql::ExactSqlTransaction,
    idempotency_key: &str,
    command_digest: &ManifestDigest,
) -> Result<Option<AuthorizedScopeSetDurableCasV1>, AuthorizedScopeSetStoreError> {
    let rows = transaction
        .query(ExactSqlStatement::new(
            "SELECT command_digest, status, next_payload, result_payload
             FROM authorized_scope_set_transactions_v1
             WHERE idempotency_key = ?1"
                .to_owned(),
            vec![ExactSqlValue::Text(idempotency_key.to_owned())],
        )?)?
        .rows;
    let Some(row) = rows.into_iter().next() else {
        return Ok(None);
    };
    let [
        ExactSqlValue::Text(stored_digest),
        ExactSqlValue::Text(status),
        ExactSqlValue::Blob(next_payload),
        result_payload,
    ] = row.values.as_slice()
    else {
        return Err(AuthorizedScopeSetStoreError::InvalidData(
            "scope-set durable CAS journal has an invalid shape".to_owned(),
        ));
    };
    if stored_digest != command_digest.as_str() {
        return Err(AuthorizedScopeSetStoreError::IdempotencyConflict);
    }
    let next = decode_scope_set_payload(next_payload)?;
    match status.as_str() {
        "pending" => Ok(Some(AuthorizedScopeSetDurableCasV1::Pending(next))),
        "prepared" => Ok(Some(AuthorizedScopeSetDurableCasV1::Prepared(next))),
        "applied" => Ok(Some(AuthorizedScopeSetDurableCasV1::Applied(next))),
        "conflict" => {
            let current = match result_payload {
                ExactSqlValue::Null => None,
                ExactSqlValue::Blob(payload) => Some(decode_scope_set_payload(payload)?),
                _ => {
                    return Err(AuthorizedScopeSetStoreError::InvalidData(
                        "scope-set conflict replay has an invalid result".to_owned(),
                    ));
                }
            };
            Ok(Some(AuthorizedScopeSetDurableCasV1::Conflict(current)))
        }
        _ => Err(AuthorizedScopeSetStoreError::InvalidData(
            "scope-set durable CAS journal has an invalid status".to_owned(),
        )),
    }
}

fn insert_durable_cas(
    transaction: &crate::exact_sql::ExactSqlTransaction,
    idempotency_key: &str,
    command_digest: &ManifestDigest,
    status: &str,
    next: &AuthorizedScopeSet,
    result: Option<&AuthorizedScopeSet>,
) -> Result<(), AuthorizedScopeSetStoreError> {
    let result_payload = result
        .map(serde_json::to_vec)
        .transpose()?
        .map_or(ExactSqlValue::Null, ExactSqlValue::Blob);
    transaction.execute(ExactSqlStatement::new(
        "INSERT INTO authorized_scope_set_transactions_v1 (
             idempotency_key, command_digest, status, next_payload, result_payload
         ) VALUES (?1, ?2, ?3, ?4, ?5)"
            .to_owned(),
        vec![
            ExactSqlValue::Text(idempotency_key.to_owned()),
            ExactSqlValue::Text(command_digest.as_str().to_owned()),
            ExactSqlValue::Text(status.to_owned()),
            ExactSqlValue::Blob(serde_json::to_vec(next)?),
            result_payload,
        ],
    )?)?;
    Ok(())
}

fn write_registered_scope_set(
    transaction: &crate::exact_sql::ExactSqlTransaction,
    next: &AuthorizedScopeSet,
) -> Result<(), AuthorizedScopeSetStoreError> {
    transaction.execute(ExactSqlStatement::new(
        "INSERT INTO authorized_scope_sets_v1 (
             scope_set_id, revision, digest, canonical_payload
         ) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(scope_set_id) DO UPDATE SET
             revision = excluded.revision,
             digest = excluded.digest,
             canonical_payload = excluded.canonical_payload"
            .to_owned(),
        vec![
            ExactSqlValue::Text(next.scope_set_id().as_str().to_owned()),
            ExactSqlValue::Integer(revision_to_i64(next.revision())?),
            ExactSqlValue::Text(next.digest().as_str().to_owned()),
            ExactSqlValue::Blob(serde_json::to_vec(next)?),
        ],
    )?)?;
    Ok(())
}

fn decode_scope_set_payload(
    payload: &[u8],
) -> Result<AuthorizedScopeSet, AuthorizedScopeSetStoreError> {
    let scope_set: AuthorizedScopeSet = serde_json::from_slice(payload)?;
    scope_set
        .validate()
        .map_err(|error| AuthorizedScopeSetStoreError::InvalidData(error.to_string()))?;
    Ok(scope_set)
}

fn registered_read_statement(
    scope_set_id: &ScopeSetId,
) -> Result<ExactSqlStatement, AuthorizedScopeSetStoreError> {
    Ok(ExactSqlStatement::new(
        "SELECT revision, digest, canonical_payload
         FROM authorized_scope_sets_v1
         WHERE scope_set_id = ?1"
            .to_owned(),
        vec![ExactSqlValue::Text(scope_set_id.as_str().to_owned())],
    )?)
}

fn decode_registered_rows(
    rows: Vec<ExactSqlRow>,
) -> Result<Option<AuthorizedScopeSet>, AuthorizedScopeSetStoreError> {
    let Some(row) = rows.into_iter().next() else {
        return Ok(None);
    };
    let [
        ExactSqlValue::Integer(revision),
        ExactSqlValue::Text(digest),
        ExactSqlValue::Blob(payload),
    ] = row.values.as_slice()
    else {
        return Err(AuthorizedScopeSetStoreError::InvalidData(
            "registered scope-set row has an invalid shape".to_owned(),
        ));
    };
    let scope_set: AuthorizedScopeSet = serde_json::from_slice(payload)?;
    if scope_set.revision() != revision_from_i64(*revision)?
        || scope_set.digest().as_str() != digest
    {
        return Err(AuthorizedScopeSetStoreError::InvalidData(
            "registered scope-set metadata does not match its canonical payload".to_owned(),
        ));
    }
    scope_set
        .validate()
        .map_err(|error| AuthorizedScopeSetStoreError::InvalidData(error.to_string()))?;
    Ok(Some(scope_set))
}

fn read_record(
    connection: &Connection,
    scope_set_id: &ScopeSetId,
) -> Result<Option<AuthorizedScopeSetRecordV1>, AuthorizedScopeSetStoreError> {
    let row = connection
        .query_row(
            "SELECT revision, digest, canonical_payload
             FROM authorized_scope_sets_v1
             WHERE scope_set_id = ?1",
            [scope_set_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            },
        )
        .optional()?;
    row.map(|(revision, digest, payload)| {
        AuthorizedScopeSetRecordV1::new(
            scope_set_id.clone(),
            revision_from_i64(revision)?,
            ManifestDigest::new(digest)
                .map_err(|error| AuthorizedScopeSetStoreError::InvalidData(error.to_string()))?,
            payload,
        )
        .map_err(AuthorizedScopeSetStoreError::from)
    })
    .transpose()
}

fn read_revision(
    connection: &Connection,
    scope_set_id: &ScopeSetId,
) -> Result<Option<ScopeSetRevision>, AuthorizedScopeSetStoreError> {
    connection
        .query_row(
            "SELECT revision FROM authorized_scope_sets_v1 WHERE scope_set_id = ?1",
            [scope_set_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .map(revision_from_i64)
        .transpose()
}

fn decode_record(
    record: AuthorizedScopeSetRecordV1,
) -> Result<AuthorizedScopeSet, AuthorizedScopeSetStoreError> {
    record.validate()?;
    let set: AuthorizedScopeSet = serde_json::from_slice(&record.canonical_payload)?;
    set.validate()?;
    if set.scope_set_id() != &record.scope_set_id
        || set.revision() != record.revision
        || set.digest() != &record.digest
    {
        return Err(AuthorizedScopeSetStoreError::InvalidData(
            "scope-set row identity does not match canonical payload".to_owned(),
        ));
    }
    Ok(set)
}

fn revision_to_i64(revision: ScopeSetRevision) -> Result<i64, AuthorizedScopeSetStoreError> {
    i64::try_from(revision.get()).map_err(|_| {
        AuthorizedScopeSetStoreError::InvalidData(
            "scope-set revision exceeds SQLite integer range".to_owned(),
        )
    })
}

fn revision_from_i64(revision: i64) -> Result<ScopeSetRevision, AuthorizedScopeSetStoreError> {
    u64::try_from(revision)
        .map_err(|_| {
            AuthorizedScopeSetStoreError::InvalidData("scope-set revision is negative".to_owned())
        })
        .and_then(|value| {
            ScopeSetRevision::new(value)
                .map_err(|error| AuthorizedScopeSetStoreError::InvalidData(error.to_string()))
        })
}
