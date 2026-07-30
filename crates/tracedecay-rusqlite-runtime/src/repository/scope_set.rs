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
