use std::fs;
use std::path::Path;

use super::filesystem_authority::{
    ensure_contained, existing_payload_dir_opt, inspect_payload_file_for_delete,
    read_payload_file_for_verify, remove_verified_payload_file, verify_payload_file_authority,
};
use super::{LcmError, gc, load_payload_metadata, util, validate_payload_ref};
#[cfg(test)]
use crate::db::engine::{Connection, TransactionBehavior};
use crate::db::engine::{Executor, params};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeleteOpts {
    pub rewrite_placeholders: bool,
    pub remove_file: bool,
    pub verify_hash: bool,
}

impl Default for DeleteOpts {
    fn default() -> Self {
        Self {
            rewrite_placeholders: true,
            remove_file: true,
            verify_hash: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeleteOutcome {
    pub metadata_row_existed: bool,
    pub file_existed: bool,
    pub file_removed: bool,
    pub placeholders_rewritten: usize,
    pub bytes_freed: u64,
}

pub(crate) struct PreparedPayloadDelete {
    pub outcome: DeleteOutcome,
    pub pending_removal_bytes: Option<u64>,
}

pub(crate) enum CommittedPayloadRemoval {
    Missing,
    Removed(u64),
    ReplacementPreserved,
}

#[cfg(test)]
pub(crate) async fn delete_external_payload(
    conn: &Connection,
    storage_root: &Path,
    payload_ref: &str,
    opts: &DeleteOpts,
) -> Result<DeleteOutcome, LcmError> {
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let prepared =
        delete_external_payload_in_transaction(&transaction, storage_root, payload_ref, opts)
            .await?;
    transaction.commit().await?;
    let mut outcome = prepared.outcome;
    if prepared.pending_removal_bytes.is_some() {
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let drained = gc::drain_pending_payload_delete_in_transaction(
            &transaction,
            storage_root,
            payload_ref,
        )
        .await;
        let drained = match drained {
            Ok(removed) => transaction
                .commit()
                .await
                .map(|()| removed)
                .map_err(Into::into),
            Err(error) => {
                let _ = transaction.rollback().await;
                Err(error)
            }
        };
        reconcile_committed_payload_drain(&mut outcome, payload_ref, drained);
    }
    Ok(outcome)
}

#[cfg(test)]
pub(crate) fn reconcile_committed_payload_drain(
    outcome: &mut DeleteOutcome,
    payload_ref: &str,
    drained: Result<Option<u64>, LcmError>,
) {
    match drained {
        Ok(removed) => {
            outcome.file_removed = removed.is_some();
            outcome.bytes_freed = removed.unwrap_or_default();
        }
        Err(error) => {
            outcome.file_removed = false;
            outcome.bytes_freed = 0;
            tracing::warn!(
                payload_ref,
                %error,
                "payload metadata deletion committed; deferred payload file removal remains pending"
            );
        }
    }
}

pub(crate) async fn delete_external_payload_in_transaction(
    conn: &(impl Executor + ?Sized),
    storage_root: &Path,
    payload_ref: &str,
    opts: &DeleteOpts,
) -> Result<PreparedPayloadDelete, LcmError> {
    validate_payload_ref(payload_ref)?;
    // The DB-side cleanup below must still run for a store whose payload
    // directory is gone — the file simply counts as already removed.
    let dir = existing_payload_dir_opt(storage_root)?;
    let path = match dir.as_deref() {
        Some(dir) => {
            let path = dir.join(payload_ref);
            ensure_contained(dir, &path)?;
            Some(path)
        }
        None => None,
    };

    let metadata = match load_payload_metadata(conn, payload_ref).await {
        Ok(payload) => Some(payload),
        Err(LcmError::PayloadNotFound) => None,
        Err(err) => return Err(err),
    };
    let (file_existed, file_bytes) = match path.as_deref() {
        Some(path) => inspect_payload_file_for_delete(path)?,
        None => (false, 0),
    };

    if opts.verify_hash
        && file_existed
        && let (Some(metadata), Some(path)) = (metadata.as_ref(), path.as_deref())
    {
        verify_payload_file_authority(
            path,
            &metadata.content_hash,
            metadata.byte_count,
            metadata.char_count,
        )?
        .ok_or(LcmError::PayloadMissing)?;
    }

    let file_fingerprint = if opts.remove_file && file_existed && metadata.is_none() {
        dir.as_deref()
            .map(|dir| payload_file_fingerprint(dir, payload_ref))
            .transpose()?
    } else {
        None
    };

    let metadata_row_existed = metadata.is_some();
    let expected_bytes = metadata.as_ref().map_or_else(
        || {
            file_fingerprint
                .as_ref()
                .map_or(file_bytes, |(_, bytes, _)| *bytes)
        },
        |payload| payload.byte_count,
    );
    let expected_chars = metadata.as_ref().map_or_else(
        || file_fingerprint.as_ref().map_or(0, |(_, _, chars)| *chars),
        |payload| payload.char_count,
    );
    let mut placeholders_rewritten = 0usize;

    let tombstone_missing_payload =
        opts.rewrite_placeholders && !opts.remove_file && !opts.verify_hash;
    if let Some(metadata) = metadata.as_ref()
        && gc::referenced_payload_refs(conn, &metadata.provider, None)
            .await?
            .contains(payload_ref)
        && !tombstone_missing_payload
    {
        return Err(LcmError::StillReferenced);
    }
    conn.execute(
        "DELETE FROM lcm_external_payloads WHERE payload_ref = ?1",
        params![payload_ref],
    )
    .await?;
    conn.execute(
        "DELETE FROM lcm_gc_marks WHERE payload_ref = ?1",
        params![payload_ref],
    )
    .await?;
    if opts.rewrite_placeholders {
        placeholders_rewritten = tombstone_residual_placeholders(conn, payload_ref).await?;
    }

    let file_removed = opts.remove_file && file_existed;
    if file_removed {
        gc::stage_payload_delete(
            conn,
            payload_ref,
            metadata
                .as_ref()
                .map(|payload| payload.content_hash.as_str())
                .or_else(|| file_fingerprint.as_ref().map(|(hash, _, _)| hash.as_str())),
            expected_bytes,
            expected_chars,
        )
        .await?;
    }

    Ok(PreparedPayloadDelete {
        outcome: DeleteOutcome {
            metadata_row_existed,
            file_existed,
            file_removed: false,
            placeholders_rewritten,
            bytes_freed: 0,
        },
        pending_removal_bytes: file_removed.then_some(expected_bytes),
    })
}

/// Removes a payload only after its database deletion tombstone committed.
/// A replacement file with different content is retained for recovery rather
/// than unlinked under a stale tombstone.
pub(crate) fn remove_committed_payload_file(
    storage_root: &Path,
    payload_ref: &str,
    expected_hash: Option<&str>,
    expected_bytes: u64,
    expected_chars: Option<u64>,
) -> Result<CommittedPayloadRemoval, LcmError> {
    remove_committed_payload_file_with(
        storage_root,
        payload_ref,
        expected_hash,
        expected_bytes,
        expected_chars,
        |_, _| Ok(()),
    )
}

pub(crate) fn remove_committed_payload_file_with<F>(
    storage_root: &Path,
    payload_ref: &str,
    expected_hash: Option<&str>,
    expected_bytes: u64,
    expected_chars: Option<u64>,
    after_quarantine: F,
) -> Result<CommittedPayloadRemoval, LcmError>
where
    F: FnOnce(&Path, &Path) -> Result<(), LcmError>,
{
    validate_payload_ref(payload_ref)?;
    let Some(dir) = existing_payload_dir_opt(storage_root)? else {
        return Ok(CommittedPayloadRemoval::Missing);
    };
    let path = dir.join(payload_ref);
    ensure_contained(&dir, &path)?;
    let (Some(expected_hash), Some(expected_chars)) = (expected_hash, expected_chars) else {
        return match fs::symlink_metadata(&path) {
            Ok(_) => Ok(CommittedPayloadRemoval::ReplacementPreserved),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(CommittedPayloadRemoval::Missing)
            }
            Err(error) => Err(LcmError::Io(error.to_string())),
        };
    };
    let quarantine_name = format!(
        ".tracedecay-pending-delete-{}",
        util::sha256_hex(payload_ref.as_bytes())
    );
    let quarantine = dir.join(&quarantine_name);
    ensure_contained(&dir, &quarantine)?;

    match fs::symlink_metadata(&quarantine) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err(LcmError::InvalidPayloadRef),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            match fs::rename(&path, &quarantine) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(CommittedPayloadRemoval::Missing);
                }
                Err(err) => return Err(LcmError::Io(err.to_string())),
            }
        }
        Err(err) => return Err(LcmError::Io(err.to_string())),
    }

    let authority = match verify_payload_file_authority(
        &quarantine,
        expected_hash,
        expected_bytes,
        expected_chars,
    ) {
        Ok(Some(authority)) => authority,
        Ok(None) => {
            return Err(LcmError::Io(format!(
                "payload deletion quarantine disappeared for {payload_ref}"
            )));
        }
        Err(LcmError::PayloadIntegrityMismatch | LcmError::InvalidPayloadRef) => {
            restore_quarantined_payload(&path, &quarantine, payload_ref)?;
            return Ok(CommittedPayloadRemoval::ReplacementPreserved);
        }
        Err(error) => return Err(error),
    };
    after_quarantine(&path, &quarantine)?;

    match remove_verified_payload_file(&authority) {
        Ok(true) => Ok(CommittedPayloadRemoval::Removed(expected_bytes)),
        Ok(false) => Err(LcmError::Io(format!(
            "payload deletion quarantine disappeared for {payload_ref}"
        ))),
        Err(LcmError::PayloadIntegrityMismatch | LcmError::InvalidPayloadRef) => {
            restore_quarantined_payload(&path, &quarantine, payload_ref)?;
            Ok(CommittedPayloadRemoval::ReplacementPreserved)
        }
        Err(error) => Err(error),
    }
}

fn restore_quarantined_payload(
    path: &Path,
    quarantine: &Path,
    payload_ref: &str,
) -> Result<(), LcmError> {
    match fs::symlink_metadata(quarantine) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(LcmError::Io(format!(
                "payload deletion quarantine disappeared for {payload_ref}"
            )));
        }
        Err(error) => return Err(LcmError::Io(error.to_string())),
    }
    match fs::symlink_metadata(path) {
        Ok(_) => {
            return Err(LcmError::Io(format!(
                "payload replacement preserved at {} while mismatched quarantine remains at {}",
                path.display(),
                quarantine.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(LcmError::Io(error.to_string())),
    }
    fs::rename(quarantine, path).map_err(|error| LcmError::Io(error.to_string()))
}

pub(crate) fn payload_file_fingerprint(
    dir: &Path,
    payload_ref: &str,
) -> Result<(String, u64, u64), LcmError> {
    validate_payload_ref(payload_ref)?;
    let path = dir.join(payload_ref);
    ensure_contained(dir, &path)?;
    let Some((content, _authority)) = read_payload_file_for_verify(&path)? else {
        return Err(LcmError::PayloadMissing);
    };
    let text = std::str::from_utf8(&content).map_err(|_| LcmError::PayloadIntegrityMismatch)?;
    Ok((
        util::sha256_hex(&content),
        content.len() as u64,
        text.chars().count() as u64,
    ))
}

async fn tombstone_residual_placeholders(
    conn: &(impl Executor + ?Sized),
    payload_ref: &str,
) -> Result<usize, LcmError> {
    let mut rows = conn
        .query(
            "SELECT store_id, storage_kind, payload_ref, content, snippet_text, index_text, metadata_json
             FROM lcm_raw_messages
             WHERE payload_ref = ?1 OR content LIKE ?2 OR snippet_text LIKE ?2 OR index_text LIKE ?2 OR metadata_json LIKE ?2",
            params![payload_ref, format!("%{payload_ref}%")],
        )
        .await?;
    let mut updates = Vec::new();
    while let Some(row) = rows.next().await? {
        let store_id: i64 = row.get(0)?;
        let storage_kind: String = row.get(1)?;
        let raw_payload_ref: Option<String> = row.get(2).unwrap_or(None);
        let mut changed = 0usize;
        let content: Option<String> = row.get(3).unwrap_or(None);
        let snippet_text: String = row.get(4)?;
        let index_text: String = row.get(5)?;
        let metadata_json: Option<String> = row.get(6).unwrap_or(None);
        let new_content = content.map(|text| {
            let tombstoned = gc::tombstone_placeholder_in_text(&text, payload_ref);
            if tombstoned != text {
                changed += 1;
            }
            tombstoned
        });
        let new_snippet = gc::tombstone_placeholder_in_text(&snippet_text, payload_ref);
        if new_snippet != snippet_text {
            changed += 1;
        }
        let new_index = gc::tombstone_placeholder_in_text(&index_text, payload_ref);
        if new_index != index_text {
            changed += 1;
        }
        let new_metadata = metadata_json.map(|text| {
            let tombstoned = gc::tombstone_placeholder_in_text(&text, payload_ref);
            if tombstoned != text {
                changed += 1;
            }
            tombstoned
        });
        let clear_raw_ref = storage_kind == "external"
            && raw_payload_ref
                .as_deref()
                .is_some_and(|value| value == payload_ref);
        if clear_raw_ref {
            changed += 1;
        }
        if changed > 0 {
            updates.push((
                store_id,
                clear_raw_ref,
                new_content,
                new_snippet,
                new_index,
                new_metadata,
                changed,
            ));
        }
    }

    let mut changed_total = 0usize;
    for (store_id, clear_raw_ref, content, snippet_text, index_text, metadata_json, changed) in
        updates
    {
        if clear_raw_ref {
            conn.execute(
                "UPDATE lcm_raw_messages
                 SET storage_kind = 'inline', payload_ref = NULL, content = ?2, snippet_text = ?3, index_text = ?4, metadata_json = ?5
                 WHERE store_id = ?1",
                params![
                    store_id,
                    content.as_deref(),
                    snippet_text,
                    index_text,
                    metadata_json.as_deref()
                ],
            )
            .await?;
        } else {
            conn.execute(
                "UPDATE lcm_raw_messages
                 SET content = ?2, snippet_text = ?3, index_text = ?4, metadata_json = ?5
                 WHERE store_id = ?1",
                params![
                    store_id,
                    content.as_deref(),
                    snippet_text,
                    index_text,
                    metadata_json.as_deref()
                ],
            )
            .await?;
        }
        changed_total += changed;
    }
    Ok(changed_total)
}
