use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use libsql::{params, Connection, Value};
use sha2::{Digest, Sha256};

use crate::sessions::SessionMessageRecord;

use super::{raw, LcmError, LcmPayloadExpansion, LcmPayloadRef};

#[cfg(target_os = "linux")]
const O_NOFOLLOW: i32 = 0o400000;

pub struct LcmStore<'db> {
    conn: &'db Connection,
    storage_root: PathBuf,
}

impl<'db> LcmStore<'db> {
    pub(crate) fn new(conn: &'db Connection, storage_root: PathBuf) -> Self {
        Self { conn, storage_root }
    }

    pub async fn ingest_raw_message(&self, message: &SessionMessageRecord) -> Result<(), LcmError> {
        raw::upsert_raw_message_with_payload(self.conn, &self.storage_root, message)
            .await
            .map(|_| ())
    }

    pub async fn lcm_expand_payload(
        &self,
        provider: &str,
        session_id: &str,
        payload_ref: &str,
        offset: usize,
        limit: usize,
    ) -> Result<LcmPayloadExpansion, LcmError> {
        expand_payload(
            self.conn,
            &self.storage_root,
            provider,
            session_id,
            payload_ref,
            offset,
            limit,
        )
        .await
    }
}

pub fn payload_dir(storage_root: &Path) -> PathBuf {
    storage_root.join("lcm-payloads")
}

pub fn validate_payload_ref(payload_ref: &str) -> Result<&str, LcmError> {
    if payload_ref.is_empty()
        || payload_ref == "."
        || payload_ref == ".."
        || payload_ref.contains('/')
        || payload_ref.contains('\\')
    {
        return Err(LcmError::InvalidPayloadRef);
    }

    let mut components = Path::new(payload_ref).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(payload_ref),
        _ => Err(LcmError::InvalidPayloadRef),
    }
}

pub(crate) fn extract_payload_refs_from_text(text: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let mut offset = 0usize;
    while let Some(relative) = text[offset..].find("ref=") {
        let start = offset + relative + "ref=".len();
        let tail = &text[start..];
        let end = tail
            .find(|ch: char| ch == ']' || ch == ';' || ch == ',' || ch.is_whitespace())
            .unwrap_or(tail.len());
        let candidate = tail[..end].trim();
        if validate_payload_ref(candidate).is_ok() && !refs.iter().any(|value| value == candidate) {
            refs.push(candidate.to_string());
        }
        offset = start.saturating_add(end).min(text.len());
    }
    refs
}

pub(crate) fn write_external_payload(
    storage_root: &Path,
    provider: &str,
    session_id: &str,
    message_id: &str,
    kind: &str,
    content: &str,
    _metadata_json: Option<String>,
) -> Result<LcmPayloadRef, LcmError> {
    let content_hash = sha256_hex(content.as_bytes());
    let owner_hash =
        sha256_hex(format!("{provider}\0{session_id}\0{message_id}\0{content_hash}").as_bytes());
    let payload_ref = format!("payload_{owner_hash}.payload");
    validate_payload_ref(&payload_ref)?;

    let dir = prepare_payload_dir(storage_root)?;
    let path = dir.join(&payload_ref);
    ensure_contained(&dir, &path)?;
    write_private_file(&path, content.as_bytes())?;

    Ok(LcmPayloadRef {
        payload_ref,
        provider: provider.to_string(),
        session_id: session_id.to_string(),
        message_id: message_id.to_string(),
        kind: kind.to_string(),
        content_hash,
        byte_count: content.len() as u64,
        char_count: content.chars().count() as u64,
        created_at: unixepoch(),
        metadata_json: _metadata_json,
    })
}

pub(crate) async fn upsert_payload_metadata(
    conn: &Connection,
    payload: &LcmPayloadRef,
) -> Result<(), LcmError> {
    conn.execute(
        "INSERT INTO lcm_external_payloads (
            payload_ref, provider, session_id, message_id, kind, content_hash,
            byte_count, char_count, created_at, metadata_json
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(payload_ref) DO UPDATE SET
            provider = excluded.provider,
            session_id = excluded.session_id,
            message_id = excluded.message_id,
            kind = excluded.kind,
            content_hash = excluded.content_hash,
            byte_count = excluded.byte_count,
            char_count = excluded.char_count,
            created_at = excluded.created_at,
            metadata_json = excluded.metadata_json",
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
            opt_text(payload.metadata_json.as_deref()),
        ],
    )
    .await
    .map_err(|err| LcmError::Db(err.to_string()))?;
    Ok(())
}

async fn expand_payload(
    conn: &Connection,
    storage_root: &Path,
    provider: &str,
    session_id: &str,
    payload_ref: &str,
    offset: usize,
    limit: usize,
) -> Result<LcmPayloadExpansion, LcmError> {
    validate_payload_ref(payload_ref)?;
    let payload = load_payload_metadata(conn, payload_ref).await?;
    if payload.provider != provider || payload.session_id != session_id {
        return Err(LcmError::PayloadNotOwnedBySession);
    }
    ensure_current_raw_payload_ref(conn, &payload).await?;

    let dir = existing_payload_dir(storage_root)?;
    let path = dir.join(payload_ref);
    ensure_contained(&dir, &path)?;
    let content = read_payload_file(&path)?;
    if sha256_hex(content.as_bytes()) != payload.content_hash {
        return Err(LcmError::PayloadIntegrityMismatch);
    }

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

async fn ensure_current_raw_payload_ref(
    conn: &Connection,
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
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    if rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .is_some()
    {
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
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
    else {
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

async fn load_payload_metadata(
    conn: &Connection,
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
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|err| LcmError::Db(err.to_string()))?
        .ok_or(LcmError::PayloadNotFound)?;
    let byte_count: i64 = row.get(6).map_err(|err| LcmError::Db(err.to_string()))?;
    let char_count: i64 = row.get(7).map_err(|err| LcmError::Db(err.to_string()))?;
    Ok(LcmPayloadRef {
        payload_ref: row.get(0).map_err(|err| LcmError::Db(err.to_string()))?,
        provider: row.get(1).map_err(|err| LcmError::Db(err.to_string()))?,
        session_id: row.get(2).map_err(|err| LcmError::Db(err.to_string()))?,
        message_id: row.get(3).map_err(|err| LcmError::Db(err.to_string()))?,
        kind: row.get(4).map_err(|err| LcmError::Db(err.to_string()))?,
        content_hash: row.get(5).map_err(|err| LcmError::Db(err.to_string()))?,
        byte_count: byte_count.max(0) as u64,
        char_count: char_count.max(0) as u64,
        created_at: row.get(8).map_err(|err| LcmError::Db(err.to_string()))?,
        metadata_json: row.get(9).map_err(|err| LcmError::Db(err.to_string()))?,
    })
}

fn prepare_payload_dir(storage_root: &Path) -> Result<PathBuf, LcmError> {
    let root = canonical_storage_root(storage_root)?;
    let dir = root.join("lcm-payloads");
    match fs::symlink_metadata(&dir) {
        Ok(metadata) => ensure_actual_private_dir(&dir, metadata)?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(&dir).map_err(|err| LcmError::Io(err.to_string()))?;
            set_private_dir_permissions(&dir)?;
        }
        Err(err) => return Err(LcmError::Io(err.to_string())),
    }
    ensure_payload_dir_under_root(&root, &dir)?;
    Ok(dir)
}

fn existing_payload_dir(storage_root: &Path) -> Result<PathBuf, LcmError> {
    let root = canonical_storage_root(storage_root)?;
    let dir = root.join("lcm-payloads");
    let metadata = fs::symlink_metadata(&dir).map_err(|err| LcmError::Io(err.to_string()))?;
    ensure_actual_private_dir(&dir, metadata)?;
    ensure_payload_dir_under_root(&root, &dir)?;
    Ok(dir)
}

fn canonical_storage_root(storage_root: &Path) -> Result<PathBuf, LcmError> {
    let metadata =
        fs::symlink_metadata(storage_root).map_err(|err| LcmError::Io(err.to_string()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(LcmError::InvalidPayloadRef);
    }
    storage_root
        .canonicalize()
        .map_err(|err| LcmError::Io(err.to_string()))
}

fn ensure_actual_private_dir(dir: &Path, metadata: fs::Metadata) -> Result<(), LcmError> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(LcmError::InvalidPayloadRef);
    }
    set_private_dir_permissions(dir)?;
    Ok(())
}

fn ensure_payload_dir_under_root(root: &Path, dir: &Path) -> Result<(), LcmError> {
    let canonical_dir = dir
        .canonicalize()
        .map_err(|err| LcmError::Io(err.to_string()))?;
    if canonical_dir.parent() == Some(root) {
        Ok(())
    } else {
        Err(LcmError::InvalidPayloadRef)
    }
}

fn ensure_contained(root: &Path, path: &Path) -> Result<(), LcmError> {
    let parent = path.parent().ok_or(LcmError::InvalidPayloadRef)?;
    if parent == root {
        Ok(())
    } else {
        Err(LcmError::InvalidPayloadRef)
    }
}

fn write_private_file(path: &Path, content: &[u8]) -> Result<(), LcmError> {
    let mut file = match private_file_options()
        .create_new(true)
        .write(true)
        .open(path)
    {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            return ensure_existing_payload_matches(path, content);
        }
        Err(err) => return Err(LcmError::Io(err.to_string())),
    };
    file.write_all(content)
        .map_err(|err| LcmError::Io(err.to_string()))?;
    file.sync_all()
        .map_err(|err| LcmError::Io(err.to_string()))?;
    Ok(())
}

fn ensure_existing_payload_matches(path: &Path, content: &[u8]) -> Result<(), LcmError> {
    let mut file = private_file_options()
        .read(true)
        .open(path)
        .map_err(|err| LcmError::Io(err.to_string()))?;
    let mut existing = Vec::new();
    file.read_to_end(&mut existing)
        .map_err(|err| LcmError::Io(err.to_string()))?;
    if existing == content {
        Ok(())
    } else {
        Err(LcmError::PayloadIntegrityMismatch)
    }
}

fn read_payload_file(path: &Path) -> Result<String, LcmError> {
    let mut file = private_file_options()
        .read(true)
        .open(path)
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                LcmError::PayloadMissing
            } else {
                LcmError::Io(err.to_string())
            }
        })?;
    let mut content = String::new();
    file.read_to_string(&mut content)
        .map_err(|err| LcmError::Io(err.to_string()))?;
    Ok(content)
}

#[cfg(unix)]
fn private_file_options() -> fs::OpenOptions {
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = fs::OpenOptions::new();
    options.mode(0o600);
    #[cfg(target_os = "linux")]
    options.custom_flags(O_NOFOLLOW);
    options
}

#[cfg(not(unix))]
fn private_file_options() -> fs::OpenOptions {
    fs::OpenOptions::new()
}

#[cfg(unix)]
fn set_private_dir_permissions(path: &Path) -> Result<(), LcmError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|err| LcmError::Io(err.to_string()))
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &Path) -> Result<(), LcmError> {
    Ok(())
}

fn sha256_hex(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    hex::encode(hasher.finalize())
}

fn unixepoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn opt_text(value: Option<&str>) -> Value {
    value.map_or(Value::Null, |s| Value::Text(s.to_string()))
}
