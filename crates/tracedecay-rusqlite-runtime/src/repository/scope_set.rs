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
