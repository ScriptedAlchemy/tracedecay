//! First-writer publication for a Remote Brain node.
//!
//! Publication only seeds a Brain's writer on this node: once a writer is
//! published, it changes through the fenced promotion journal, never by
//! republishing. A node accepts a writer only for a repository scope it was
//! provisioned for, and only together with the replay policy that admits that
//! writer's frames, so a later promotion never journals against a missing
//! policy.

use thiserror::Error;
use tracedecay_contracts::remote::replay::RemoteReplayPolicyEvidenceV1;

use crate::exact_sql::{ExactSqlRow, ExactSqlTransaction, text_at};

use super::policy::replay_scope_digest;
use super::*;

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteWriterPublicationErrorV1 {
    #[error("remote writer authority or replay policy is invalid for this node")]
    InvalidWriter,
    #[error("remote Brain node is not registered with this daemon")]
    NodeNotRegistered,
    #[error("remote writer scope was never provisioned on this node")]
    ScopeNotProvisioned,
    #[error("remote writer project has no mounted ProjectSessions authority")]
    ProjectUnavailable,
    #[error("a different remote writer authority or replay policy is already published")]
    Conflict,
    #[error("remote writer authority storage is corrupt")]
    Corruption,
    #[error("remote writer authority storage is unavailable")]
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteWriterPublicationStateV1 {
    Unpublished,
    Published,
}

impl RemoteSqliteStorageV1 {
    /// Admits a publication without writing, so callers can refuse before
    /// seeding any other durable sink.
    pub fn writer_publication_state(
        &self,
        writer: &RemoteWriterAuthorityV1,
        policy: &RemoteReplayPolicyEvidenceV1,
        published_at: UtcMicros,
    ) -> Result<RemoteWriterPublicationStateV1, RemoteWriterPublicationErrorV1> {
        let transaction = self.handle().begin_immediate().map_err(unavailable)?;
        let state = admit_publication(&transaction, &self.binding, writer, policy, published_at)?;
        transaction.rollback().map_err(unavailable)?;
        Ok(state)
    }

    pub fn publish_authority(
        &self,
        writer: &RemoteWriterAuthorityV1,
        policy: &RemoteReplayPolicyEvidenceV1,
        published_at: UtcMicros,
    ) -> Result<(), RemoteWriterPublicationErrorV1> {
        let transaction = self.handle().begin_immediate().map_err(unavailable)?;
        if admit_publication(&transaction, &self.binding, writer, policy, published_at)?
            == RemoteWriterPublicationStateV1::Published
        {
            transaction.rollback().map_err(unavailable)?;
            return Ok(());
        }
        let state = CurrentRemoteAuthorityStateV1::Available(writer.authority.clone());
        hotpath::measure_block!("rusqlite.remote.persist_authority", {
            execute(
                &transaction,
                "INSERT INTO remote_replay_policies (
                    scope_digest, policy_revision, evidence_json
                 ) VALUES (?1, ?2, ?3)
                 ON CONFLICT(scope_digest) DO NOTHING",
                vec![
                    text(policy_scope_digest(policy)?.as_str()),
                    integer(policy.policy_revision)?,
                    text(&encode(policy)?),
                ],
            )?;
            execute(
                &transaction,
                "INSERT INTO remote_authorities (
                    brain_id, runtime_binding_json, authority_state_json, writer_json, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                vec![
                    text(writer.authority.fence.brain_id.as_str()),
                    text(&encode(&self.binding)?),
                    text(&encode(&state)?),
                    text(&encode(writer)?),
                    ExactSqlValue::Integer(published_at.0),
                ],
            )
        })?;
        transaction.commit().map_err(unavailable)?;
        Ok(())
    }
}

fn admit_publication(
    transaction: &ExactSqlTransaction,
    binding: &StoreRuntimeBindingV1,
    writer: &RemoteWriterAuthorityV1,
    policy: &RemoteReplayPolicyEvidenceV1,
    published_at: UtcMicros,
) -> Result<RemoteWriterPublicationStateV1, RemoteWriterPublicationErrorV1> {
    if writer.validate().is_err()
        || writer.authority.fence.brain_id != binding.shard_id.brain_id
        || writer.authority.observed_at > published_at
        || policy.validate().is_err()
        || policy.repository_scope != writer.scope
        || policy.revalidated_at > published_at
    {
        return Err(RemoteWriterPublicationErrorV1::InvalidWriter);
    }
    if !scope_provisioned(transaction, writer)? {
        return Err(RemoteWriterPublicationErrorV1::ScopeNotProvisioned);
    }
    let stored_policy = select(
        transaction,
        "SELECT evidence_json FROM remote_replay_policies WHERE scope_digest = ?1",
        vec![text(policy_scope_digest(policy)?.as_str())],
    )?;
    if let Some(row) = optional_row(stored_policy)?
        && decode::<RemoteReplayPolicyEvidenceV1>(&row, 0)? != *policy
    {
        return Err(RemoteWriterPublicationErrorV1::Conflict);
    }
    let stored_writer = select(
        transaction,
        "SELECT runtime_binding_json, authority_state_json, writer_json
         FROM remote_authorities WHERE brain_id = ?1",
        vec![text(writer.authority.fence.brain_id.as_str())],
    )?;
    let Some(row) = optional_row(stored_writer)? else {
        return Ok(RemoteWriterPublicationStateV1::Unpublished);
    };
    if decode::<StoreRuntimeBindingV1>(&row, 0)? != *binding {
        return Err(RemoteWriterPublicationErrorV1::Corruption);
    }
    let published = decode::<CurrentRemoteAuthorityStateV1>(&row, 1)?
        == CurrentRemoteAuthorityStateV1::Available(writer.authority.clone())
        && decode::<RemoteWriterAuthorityV1>(&row, 2)? == *writer;
    if published {
        Ok(RemoteWriterPublicationStateV1::Published)
    } else {
        Err(RemoteWriterPublicationErrorV1::Conflict)
    }
}

fn scope_provisioned(
    transaction: &ExactSqlTransaction,
    writer: &RemoteWriterAuthorityV1,
) -> Result<bool, RemoteWriterPublicationErrorV1> {
    let grants = select(
        transaction,
        "SELECT grant_json FROM remote_enrollment_grants",
        Vec::new(),
    )?;
    for row in &grants.rows {
        let grant = decode::<EnrollmentGrantV1>(row, 0)?;
        if grant.brain_id == writer.authority.fence.brain_id && grant.scope == writer.scope {
            return Ok(true);
        }
    }
    Ok(false)
}

fn select(
    transaction: &ExactSqlTransaction,
    sql: &str,
    params: Vec<ExactSqlValue>,
) -> Result<ExactSqlRows, RemoteWriterPublicationErrorV1> {
    let statement = ExactSqlStatement::new(sql.to_owned(), params)
        .map_err(|_| RemoteWriterPublicationErrorV1::Corruption)?;
    transaction.query(statement).map_err(unavailable)
}

fn execute(
    transaction: &ExactSqlTransaction,
    sql: &str,
    params: Vec<ExactSqlValue>,
) -> Result<(), RemoteWriterPublicationErrorV1> {
    let statement = ExactSqlStatement::new(sql.to_owned(), params)
        .map_err(|_| RemoteWriterPublicationErrorV1::Corruption)?;
    transaction.execute(statement).map_err(unavailable)?;
    Ok(())
}

fn optional_row(rows: ExactSqlRows) -> Result<Option<ExactSqlRow>, RemoteWriterPublicationErrorV1> {
    let mut rows = rows.rows.into_iter();
    match (rows.next(), rows.next()) {
        (row, None) => Ok(row),
        (_, Some(_)) => Err(RemoteWriterPublicationErrorV1::Corruption),
    }
}

fn decode<T: serde::de::DeserializeOwned>(
    row: &ExactSqlRow,
    index: usize,
) -> Result<T, RemoteWriterPublicationErrorV1> {
    let encoded =
        text_at(&row.values, index).map_err(|_| RemoteWriterPublicationErrorV1::Corruption)?;
    serde_json::from_str(encoded).map_err(|_| RemoteWriterPublicationErrorV1::Corruption)
}

fn encode<T: serde::Serialize>(value: &T) -> Result<String, RemoteWriterPublicationErrorV1> {
    serde_json::to_string(value).map_err(|_| RemoteWriterPublicationErrorV1::Corruption)
}

fn integer(value: u64) -> Result<ExactSqlValue, RemoteWriterPublicationErrorV1> {
    i64::try_from(value)
        .map(ExactSqlValue::Integer)
        .map_err(|_| RemoteWriterPublicationErrorV1::InvalidWriter)
}

fn policy_scope_digest(
    policy: &RemoteReplayPolicyEvidenceV1,
) -> Result<ManifestDigest, RemoteWriterPublicationErrorV1> {
    replay_scope_digest(&policy.repository_scope)
        .map_err(|_| RemoteWriterPublicationErrorV1::InvalidWriter)
}

fn unavailable(_: ExactSqlError) -> RemoteWriterPublicationErrorV1 {
    RemoteWriterPublicationErrorV1::Unavailable
}
