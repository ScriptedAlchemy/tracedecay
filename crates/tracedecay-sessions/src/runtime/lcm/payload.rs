use std::path::{Path, PathBuf};

pub use crate::application::session::lcm::contracts::validate_payload_ref;
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};
use tracedecay_runtime_core::tracedecay::current_timestamp;

use super::{LcmError, LcmPayloadExpansion, LcmPayloadRef, gc, util};

mod delete_recovery;
mod filesystem_authority;
mod rollback;

#[cfg(test)]
pub use delete_recovery::delete_external_payload;
pub use delete_recovery::{
    CommittedPayloadRemoval, PreparedPayloadDelete, payload_file_fingerprint,
    remove_committed_payload_file,
};
pub use delete_recovery::{DeleteOpts, DeleteOutcome};
#[cfg(test)]
pub use delete_recovery::{
    reconcile_committed_payload_drain, remove_committed_payload_file_with,
};
pub use filesystem_authority::VerifiedPayloadAuthority;
use filesystem_authority::{
    PayloadFileWrite, prepare_payload_dir, read_verified_payload_file, write_private_file,
};
pub use filesystem_authority::{
    ensure_contained, existing_payload_dir, existing_payload_dir_opt,
};
pub use rollback::PayloadFileRollback;

pub async fn delete_external_payload_in_transaction(
    conn: &(impl Executor + ?Sized),
    storage_root: &Path,
    payload_ref: &str,
    opts: &DeleteOpts,
) -> Result<PreparedPayloadDelete, LcmError> {
    delete_recovery::delete_external_payload_in_transaction(conn, storage_root, payload_ref, opts)
        .await
}

pub fn canonical_storage_root(storage_root: &Path) -> Result<PathBuf, LcmError> {
    filesystem_authority::canonical_storage_root(storage_root)
}

pub fn payload_dir(storage_root: &Path) -> PathBuf {
    storage_root.join("lcm-payloads")
}

pub fn extract_payload_refs_from_text(text: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let mut offset = 0usize;
    while let Some(relative) = text[offset..].find('[') {
        let start = offset + relative;
        let tail = &text[start..];
        let Some(end_relative) = tail.find(']') else {
            break;
        };
        let placeholder = &tail[..=end_relative];
        if !is_external_payload_placeholder(placeholder) {
            offset = start + '['.len_utf8();
            continue;
        }
        offset = start + end_relative + 1;
        let Some(ref_relative) = placeholder.find("ref=") else {
            continue;
        };
        let ref_start = ref_relative + "ref=".len();
        let ref_tail = &placeholder[ref_start..placeholder.len().saturating_sub(1)];
        let end = ref_tail
            .find(|ch: char| ch == ';' || ch == ',' || ch.is_whitespace())
            .unwrap_or(ref_tail.len());
        let candidate = ref_tail[..end].trim();
        if validate_payload_ref(candidate).is_ok() && !refs.iter().any(|value| value == candidate) {
            refs.push(candidate.to_string());
        }
    }
    refs
}

fn is_external_payload_placeholder(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "[externalized payload:",
        "[gc'd externalized payload:",
        "[externalized lcm ingest payload:",
        "[externalized tool output:",
        "[gc'd externalized tool output:",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
}

pub struct ExternalPayloadWrite<'a> {
    pub provider: &'a str,
    pub session_id: &'a str,
    pub message_id: &'a str,
    pub kind: &'a str,
    pub content: &'a str,
    pub metadata_json: Option<String>,
}

pub fn write_external_payload_tracked(
    storage_root: &Path,
    write: ExternalPayloadWrite<'_>,
    rollback: &mut PayloadFileRollback,
) -> Result<LcmPayloadRef, LcmError> {
    let (payload, file_write) = write_external_payload_inner(storage_root, write)?;
    if file_write.created {
        rollback.record_created(file_write.authority);
    }
    Ok(payload)
}

#[cfg(test)]
pub fn write_external_payload(
    storage_root: &Path,
    provider: &str,
    session_id: &str,
    message_id: &str,
    kind: &str,
    content: &str,
    metadata_json: Option<String>,
) -> Result<LcmPayloadRef, LcmError> {
    write_external_payload_inner(
        storage_root,
        ExternalPayloadWrite {
            provider,
            session_id,
            message_id,
            kind,
            content,
            metadata_json,
        },
    )
    .map(|(payload, _file_write)| payload)
}

fn write_external_payload_inner(
    storage_root: &Path,
    write: ExternalPayloadWrite<'_>,
) -> Result<(LcmPayloadRef, PayloadFileWrite), LcmError> {
    let ExternalPayloadWrite {
        provider,
        session_id,
        message_id,
        kind,
        content,
        metadata_json,
    } = write;
    let content_hash = util::sha256_hex(content.as_bytes());
    let owner_hash = util::sha256_hex(
        format!("{provider}\0{session_id}\0{message_id}\0{content_hash}").as_bytes(),
    );
    let payload_ref = format!("payload_{owner_hash}.payload");
    validate_payload_ref(&payload_ref)?;

    let dir = prepare_payload_dir(storage_root)?;
    let path = dir.join(&payload_ref);
    ensure_contained(&dir, &path)?;
    let file_write = write_private_file(&path, content.as_bytes())?;

    Ok((
        LcmPayloadRef {
            payload_ref,
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            message_id: message_id.to_string(),
            kind: kind.to_string(),
            content_hash,
            byte_count: content.len() as u64,
            char_count: content.chars().count() as u64,
            created_at: current_timestamp(),
            metadata_json,
        },
        file_write,
    ))
}

pub async fn upsert_payload_metadata(
    conn: &(impl Executor + ?Sized),
    payload: &LcmPayloadRef,
) -> Result<(), LcmError> {
    conn.execute(
        "INSERT INTO lcm_external_payloads (
            payload_ref, provider, session_id, message_id, kind, content_hash,
            byte_count, char_count, created_at, metadata_json
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(payload_ref) DO NOTHING",
        params![
            payload.payload_ref.as_str(),
            payload.provider.as_str(),
            payload.session_id.as_str(),
            payload.message_id.as_str(),
            payload.kind.as_str(),
            payload.content_hash.as_str(),
            payload.byte_count as i64,
            payload.char_count as i64,
            payload.created_at,
            payload.metadata_json.as_deref(),
        ],
    )
    .await?;
    let mut rows = conn
        .query(
            "SELECT provider, session_id, message_id, kind, content_hash,
                    byte_count, char_count, created_at, metadata_json
             FROM lcm_external_payloads WHERE payload_ref = ?1",
            params![payload.payload_ref.as_str()],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| LcmError::Db("payload manifest replay row disappeared".to_string()))?;
    let matches = row.get::<String>(0)? == payload.provider
        && row.get::<String>(1)? == payload.session_id
        && row.get::<String>(2)? == payload.message_id
        && row.get::<String>(3)? == payload.kind
        && row.get::<String>(4)? == payload.content_hash
        && row.get::<i64>(5)? == payload.byte_count as i64
        && row.get::<i64>(6)? == payload.char_count as i64
        && row.get::<Option<String>>(8)? == payload.metadata_json;
    if !matches {
        return Err(LcmError::ImmutablePayloadConflict {
            payload_ref: payload.payload_ref.clone(),
        });
    }
    Ok(())
}

pub async fn expand_payload(
    conn: &(impl QueryExecutor + ?Sized),
    storage_root: &Path,
    provider: &str,
    session_id: &str,
    payload_ref: &str,
    offset: usize,
    limit: usize,
) -> Result<LcmPayloadExpansion, LcmError> {
    validate_payload_ref(payload_ref)?;
    let payload = match load_payload_metadata(conn, payload_ref).await {
        Ok(payload) => payload,
        Err(LcmError::PayloadNotFound) if tombstoned_raw_ref_exists(conn, payload_ref).await? => {
            return Err(LcmError::PayloadGcd);
        }
        Err(err) => return Err(err),
    };
    let payload = validate_expand_payload_owner(conn, provider, session_id, payload).await?;

    let dir = existing_payload_dir(storage_root)?;
    let path = dir.join(payload_ref);
    ensure_contained(&dir, &path)?;
    let (content, _authority) = read_verified_payload_file(
        &path,
        &payload.content_hash,
        payload.byte_count,
        payload.char_count,
    )?
    .ok_or(LcmError::PayloadMissing)?;
    let content = String::from_utf8(content).map_err(|_| LcmError::PayloadIntegrityMismatch)?;

    let total_char_count = content.chars().count();
    let start = offset.min(total_char_count);
    let slice = content.chars().skip(start).take(limit).collect::<String>();
    let char_count = slice.chars().count();
    Ok(LcmPayloadExpansion {
        payload_ref: payload.payload_ref,
        provider: payload.provider,
        session_id: payload.session_id,
        message_id: payload.message_id,
        content: slice,
        offset: start as u64,
        char_count: char_count as u64,
        total_char_count: total_char_count as u64,
        byte_count: payload.byte_count,
        content_hash: payload.content_hash,
    })
}

pub fn read_verified_payload_content(
    storage_root: &Path,
    payload_ref: &str,
    content_hash: &str,
    byte_count: usize,
    char_count: usize,
) -> Result<String, LcmError> {
    validate_payload_ref(payload_ref)?;
    let dir = existing_payload_dir(storage_root)?;
    let path = dir.join(payload_ref);
    ensure_contained(&dir, &path)?;
    let byte_count = u64::try_from(byte_count).map_err(|_| LcmError::PayloadIntegrityMismatch)?;
    let char_count = u64::try_from(char_count).map_err(|_| LcmError::PayloadIntegrityMismatch)?;
    let (content, _authority) =
        read_verified_payload_file(&path, content_hash, byte_count, char_count)?
            .ok_or(LcmError::PayloadMissing)?;
    String::from_utf8(content).map_err(|_| LcmError::PayloadIntegrityMismatch)
}

async fn validate_expand_payload_owner(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: &str,
    payload: LcmPayloadRef,
) -> Result<LcmPayloadRef, LcmError> {
    if payload.provider != provider || payload.session_id != session_id {
        return Err(LcmError::PayloadNotOwnedBySession);
    }
    ensure_current_raw_payload_ref(conn, &payload).await?;
    Ok(payload)
}

async fn tombstoned_raw_ref_exists(
    conn: &(impl QueryExecutor + ?Sized),
    payload_ref: &str,
) -> Result<bool, LcmError> {
    let mut rows = conn
        .query(
            "SELECT content, snippet_text, index_text, metadata_json
             FROM lcm_raw_messages
             WHERE content LIKE ?1 OR snippet_text LIKE ?1 OR index_text LIKE ?1 OR metadata_json LIKE ?1",
            params![format!("%{payload_ref}%")],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        for index in 0..4 {
            let value: Option<String> = row.get(index).unwrap_or(None);
            if value
                .as_deref()
                .is_some_and(|text| gc::text_has_tombstoned_payload_ref(text, payload_ref))
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

async fn ensure_current_raw_payload_ref(
    conn: &(impl QueryExecutor + ?Sized),
    payload: &LcmPayloadRef,
) -> Result<(), LcmError> {
    let mut rows = conn
        .query(
            "SELECT 1
             FROM lcm_raw_messages
             WHERE provider = ?1
               AND session_id = ?2
               AND message_id = ?3
               AND storage_kind = 'external'
               AND payload_ref = ?4
             LIMIT 1",
            params![
                payload.provider.as_str(),
                payload.session_id.as_str(),
                payload.message_id.as_str(),
                payload.payload_ref.as_str(),
            ],
        )
        .await?;
    if rows.next().await?.is_some() {
        return Ok(());
    }

    let mut rows = conn
        .query(
            "SELECT content, snippet_text, index_text, metadata_json
             FROM lcm_raw_messages
             WHERE provider = ?1
               AND session_id = ?2
               AND message_id = ?3
             LIMIT 1",
            params![
                payload.provider.as_str(),
                payload.session_id.as_str(),
                payload.message_id.as_str(),
            ],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(LcmError::PayloadNotFound);
    };
    for index in 0..4 {
        let value: Option<String> = row.get(index).unwrap_or(None);
        if value
            .as_deref()
            .map(extract_payload_refs_from_text)
            .unwrap_or_default()
            .iter()
            .any(|reference| reference == &payload.payload_ref)
        {
            return Ok(());
        }
    }
    Err(LcmError::PayloadNotFound)
}

pub async fn load_payload_metadata(
    conn: &(impl QueryExecutor + ?Sized),
    payload_ref: &str,
) -> Result<LcmPayloadRef, LcmError> {
    let mut rows = conn
        .query(
            "SELECT payload_ref, provider, session_id, message_id, kind, content_hash,
                    byte_count, char_count, created_at, metadata_json
             FROM lcm_external_payloads
             WHERE payload_ref = ?1",
            params![payload_ref],
        )
        .await?;
    let row = rows.next().await?.ok_or(LcmError::PayloadNotFound)?;
    let byte_count: i64 = row.get(6)?;
    let char_count: i64 = row.get(7)?;
    Ok(LcmPayloadRef {
        payload_ref: row.get(0)?,
        provider: row.get(1)?,
        session_id: row.get(2)?,
        message_id: row.get(3)?,
        kind: row.get(4)?,
        content_hash: row.get(5)?,
        byte_count: byte_count.max(0) as u64,
        char_count: char_count.max(0) as u64,
        created_at: row.get(8)?,
        metadata_json: row.get(9)?,
    })
}

#[cfg(test)]
#[path = "payload/rollback_tests.rs"]
mod rollback_tests;
