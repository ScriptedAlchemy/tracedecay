use std::path::Path;

use sha2::{Digest, Sha256};
use tracedecay_domain::canonical_text::encode_lowercase_hex;
use tracedecay_domain::{BrainId, BrainNodeId, UserProfileId};
use tracedecay_store::{StoreRuntimeBindingV1, StoreShardScopeV1};

use crate::exact_sql::{ExactSqlHandle, ExactSqlStatement};

use super::{READ_WAIT, RemoteSqliteStorageErrorV1, RemoteSqliteStorageV1, one_row, row_text, text};

/// Loads one text column from an identity row; any shape mismatch means the
/// singleton table does not have the exact final persisted layout.
fn identity_text(
    row: &crate::exact_sql::ExactSqlRow,
    index: usize,
) -> Result<&str, RemoteSqliteStorageErrorV1> {
    row_text(row, index).map_err(|_| RemoteSqliteStorageErrorV1::Corruption)
}

pub(super) fn bind_node_identity(
    handle: &ExactSqlHandle,
    binding: &StoreRuntimeBindingV1,
) -> Result<(), RemoteSqliteStorageErrorV1> {
    let StoreShardScopeV1::RemoteNode { node_id } = &binding.shard_id.scope else {
        return Err(RemoteSqliteStorageErrorV1::BindingMismatch);
    };
    let rows = handle.query(
        ExactSqlStatement::new(
            "SELECT brain_id, profile_id, node_id
             FROM remote_node_identity WHERE singleton = 1"
                .to_owned(),
            Vec::new(),
        )?,
        READ_WAIT,
    )?;
    let row = rows
        .rows
        .first()
        .ok_or(RemoteSqliteStorageErrorV1::ResetRequired)?;
    if rows.rows.len() != 1
        || identity_text(row, 0)? != binding.shard_id.brain_id.as_str()
        || identity_text(row, 1)? != binding.shard_id.profile_id.as_str()
        || identity_text(row, 2)? != node_id.as_str()
    {
        return Err(RemoteSqliteStorageErrorV1::BindingMismatch);
    }
    Ok(())
}

pub(super) fn provision_node_identity(
    handle: &ExactSqlHandle,
    binding: &StoreRuntimeBindingV1,
) -> Result<(), RemoteSqliteStorageErrorV1> {
    let StoreShardScopeV1::RemoteNode { node_id } = &binding.shard_id.scope else {
        return Err(RemoteSqliteStorageErrorV1::BindingMismatch);
    };
    let transaction = handle.begin_immediate()?;
    let existing = transaction.query(ExactSqlStatement::new(
        "SELECT EXISTS(SELECT 1 FROM remote_node_identity)".to_owned(),
        Vec::new(),
    )?)?;
    let row = one_row(existing)?;
    if !matches!(
        row.values.first(),
        Some(crate::exact_sql::ExactSqlValue::Integer(0))
    ) {
        return Err(RemoteSqliteStorageErrorV1::ResetRequired);
    }
    transaction.execute(ExactSqlStatement::new(
        "INSERT INTO remote_node_identity (
            singleton, brain_id, profile_id, node_id
         ) VALUES (1, ?1, ?2, ?3)"
            .to_owned(),
        vec![
            text(binding.shard_id.brain_id.as_str()),
            text(binding.shard_id.profile_id.as_str()),
            text(node_id.as_str()),
        ],
    )?)?;
    transaction.commit()?;
    bind_node_identity(handle, binding)
}

impl RemoteSqliteStorageV1 {
    /// Reads the typed singleton used by daemon startup to remount only stores
    /// owned by the active profile. Exact final schema admission still occurs
    /// through [`Self::from_registered`] before the store is published.
    pub fn discover_registered_node(
        path: &Path,
        expected_brain: &BrainId,
        expected_profile: &UserProfileId,
    ) -> Result<BrainNodeId, RemoteSqliteStorageErrorV1> {
        let connection = rusqlite::Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|_| RemoteSqliteStorageErrorV1::Unavailable)?;
        let (brain_id, profile_id, node_id): (String, String, String) = connection
            .query_row(
                "SELECT brain_id, profile_id, node_id
                 FROM remote_node_identity WHERE singleton = 1",
                (),
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|_| RemoteSqliteStorageErrorV1::ResetRequired)?;
        if brain_id != expected_brain.as_str() || profile_id != expected_profile.as_str() {
            return Err(RemoteSqliteStorageErrorV1::BindingMismatch);
        }
        let node_id =
            BrainNodeId::new(node_id).map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
        let expected_directory = encode_lowercase_hex(&Sha256::digest(node_id.as_str().as_bytes()));
        if path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            != Some(expected_directory.as_str())
        {
            return Err(RemoteSqliteStorageErrorV1::BindingMismatch);
        }
        Ok(node_id)
    }
}
