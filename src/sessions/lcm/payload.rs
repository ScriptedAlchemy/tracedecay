use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use libsql::{params, Connection};

use crate::sessions::SessionMessageRecord;
use crate::tracedecay::current_timestamp;

use super::{gc, raw, util, LcmError, LcmPayloadExpansion, LcmPayloadRef};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PayloadFileIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

#[cfg(target_os = "linux")]
const O_NOFOLLOW: i32 = 0o40_0000;

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

pub(crate) fn write_external_payload(
    storage_root: &Path,
    provider: &str,
    session_id: &str,
    message_id: &str,
    kind: &str,
    content: &str,
    metadata_json: Option<String>,
) -> Result<LcmPayloadRef, LcmError> {
    let content_hash = util::sha256_hex(content.as_bytes());
    let owner_hash = util::sha256_hex(
        format!("{provider}\0{session_id}\0{message_id}\0{content_hash}").as_bytes(),
    );
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
        created_at: current_timestamp(),
        metadata_json,
    })
}

/// Moves externalized payload ownership from one session id to another inside
/// the caller's transaction. Mirrors hermes-lcm `reassign_externalized_payloads`
/// (payload files are keyed by ref, so only the DB ownership row moves).
pub(crate) async fn reassign_session_payloads(
    conn: &Connection,
    provider: &str,
    old_session_id: &str,
    new_session_id: &str,
) -> Result<u64, LcmError> {
    if old_session_id.is_empty() || new_session_id.is_empty() || old_session_id == new_session_id {
        return Ok(0);
    }
    conn.execute(
        "UPDATE lcm_external_payloads
         SET session_id = ?3
         WHERE provider = ?1 AND session_id = ?2",
        params![provider, old_session_id, new_session_id],
    )
    .await
    .map_err(|err| LcmError::Db(err.to_string()))
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
            util::opt_text(payload.metadata_json.as_deref()),
        ],
    )
    .await?;
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
    let payload = match load_payload_metadata(conn, payload_ref).await {
        Ok(payload) => payload,
        Err(LcmError::PayloadNotFound) if tombstoned_raw_ref_exists(conn, payload_ref).await? => {
            return Err(LcmError::PayloadGcd);
        }
        Err(err) => return Err(err),
    };
    if payload.provider != provider || payload.session_id != session_id {
        return Err(LcmError::PayloadNotOwnedBySession);
    }
    ensure_current_raw_payload_ref(conn, &payload).await?;

    let dir = existing_payload_dir(storage_root)?;
    let path = dir.join(payload_ref);
    ensure_contained(&dir, &path)?;
    let content = read_payload_file(&path)?;
    if util::sha256_hex(content.as_bytes()) != payload.content_hash {
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

async fn tombstoned_raw_ref_exists(conn: &Connection, payload_ref: &str) -> Result<bool, LcmError> {
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

pub async fn delete_external_payload(
    conn: &Connection,
    storage_root: &Path,
    payload_ref: &str,
    opts: &DeleteOpts,
) -> Result<DeleteOutcome, LcmError> {
    validate_payload_ref(payload_ref)?;
    let dir = existing_payload_dir(storage_root)?;
    let path = dir.join(payload_ref);
    ensure_contained(&dir, &path)?;

    let metadata = match load_payload_metadata(conn, payload_ref).await {
        Ok(payload) => Some(payload),
        Err(LcmError::PayloadNotFound) => None,
        Err(err) => return Err(err),
    };
    let (file_existed, file_identity) = inspect_payload_file_for_delete(&path)?;

    if opts.verify_hash && file_existed {
        if let Some(metadata) = metadata.as_ref() {
            let (content, identity) =
                read_payload_file_for_verify(&path)?.ok_or(LcmError::PayloadMissing)?;
            if Some(identity) != file_identity
                || util::sha256_hex(&content) != metadata.content_hash
            {
                return Err(LcmError::PayloadIntegrityMismatch);
            }
        }
    }

    let metadata_row_existed = metadata.is_some();
    let expected_bytes = metadata.as_ref().map_or(0, |payload| payload.byte_count);
    let mut placeholders_rewritten = 0usize;

    conn.execute("BEGIN IMMEDIATE", ()).await?;
    let tx_result: Result<(), LcmError> = async {
        if opts.verify_hash {
            if let Some(metadata) = metadata.as_ref() {
                if gc::referenced_payload_refs(conn, &metadata.provider, None)
                    .await?
                    .contains(payload_ref)
                {
                    return Err(LcmError::StillReferenced);
                }
            }
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
        Ok(())
    }
    .await;
    match tx_result {
        Ok(()) => conn.execute("COMMIT", ()).await.map(|_| ())?,
        Err(err) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            return Err(err);
        }
    }

    let file_removed = if opts.remove_file && file_existed {
        safe_remove_payload_file_checked(&dir, payload_ref, file_identity.as_ref())?
    } else {
        false
    };

    Ok(DeleteOutcome {
        metadata_row_existed,
        file_existed,
        file_removed,
        placeholders_rewritten,
        bytes_freed: if file_removed { expected_bytes } else { 0 },
    })
}

async fn tombstone_residual_placeholders(
    conn: &Connection,
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
                params![store_id, util::opt_text(content.as_deref()), snippet_text, index_text, util::opt_text(metadata_json.as_deref())],
            )
            .await?;
        } else {
            conn.execute(
                "UPDATE lcm_raw_messages
                 SET content = ?2, snippet_text = ?3, index_text = ?4, metadata_json = ?5
                 WHERE store_id = ?1",
                params![
                    store_id,
                    util::opt_text(content.as_deref()),
                    snippet_text,
                    index_text,
                    util::opt_text(metadata_json.as_deref())
                ],
            )
            .await?;
        }
        changed_total += changed;
    }
    Ok(changed_total)
}

pub fn safe_remove_payload_file(dir: &Path, payload_ref: &str) -> Result<bool, LcmError> {
    safe_remove_payload_file_checked(dir, payload_ref, None)
}

fn safe_remove_payload_file_checked(
    dir: &Path,
    payload_ref: &str,
    expected_identity: Option<&PayloadFileIdentity>,
) -> Result<bool, LcmError> {
    validate_payload_ref(payload_ref)?;
    let path = dir.join(payload_ref);
    ensure_contained(dir, &path)?;
    let Some((file, _opened, _lstat, identity)) = open_verified_payload_file(&path)? else {
        return Ok(false);
    };
    if let Some(expected_identity) = expected_identity {
        same_payload_file_identity(&identity, expected_identity)?;
    }
    drop(file);
    ensure_contained(dir, &path)?;
    fs::remove_file(&path).map_err(|err| LcmError::Io(err.to_string()))?;
    Ok(true)
}

fn inspect_payload_file_for_delete(
    path: &Path,
) -> Result<(bool, Option<PayloadFileIdentity>), LcmError> {
    Ok(match open_verified_payload_file(path)? {
        Some((_file, _opened, _lstat, identity)) => (true, Some(identity)),
        None => (false, None),
    })
}

fn read_payload_file_for_verify(
    path: &Path,
) -> Result<Option<(Vec<u8>, PayloadFileIdentity)>, LcmError> {
    let Some((mut file, _opened, _lstat, identity)) = open_verified_payload_file(path)? else {
        return Ok(None);
    };
    let mut content = Vec::new();
    file.read_to_end(&mut content)
        .map_err(|err| LcmError::Io(err.to_string()))?;
    Ok(Some((content, identity)))
}

fn open_verified_payload_file(
    path: &Path,
) -> Result<Option<(fs::File, fs::Metadata, fs::Metadata, PayloadFileIdentity)>, LcmError> {
    let file = match private_file_options().read(true).open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            if fs::symlink_metadata(path)
                .is_ok_and(|metadata| metadata.file_type().is_symlink() || !metadata.is_file())
            {
                return Err(LcmError::InvalidPayloadRef);
            }
            return Err(LcmError::Io(err.to_string()));
        }
    };
    let opened = file
        .metadata()
        .map_err(|err| LcmError::Io(err.to_string()))?;
    if !opened.is_file() {
        return Err(LcmError::InvalidPayloadRef);
    }
    let lstat = fs::symlink_metadata(path).map_err(|err| LcmError::Io(err.to_string()))?;
    if lstat.file_type().is_symlink() || !lstat.is_file() {
        return Err(LcmError::InvalidPayloadRef);
    }
    same_file_identity(&opened, &lstat)?;
    let identity = payload_file_identity(&opened);
    Ok(Some((file, opened, lstat, identity)))
}

#[cfg(unix)]
fn same_file_identity(opened: &fs::Metadata, lstat: &fs::Metadata) -> Result<(), LcmError> {
    use std::os::unix::fs::MetadataExt;

    if opened.dev() == lstat.dev() && opened.ino() == lstat.ino() {
        Ok(())
    } else {
        Err(LcmError::InvalidPayloadRef)
    }
}

#[cfg(unix)]
fn payload_file_identity(metadata: &fs::Metadata) -> PayloadFileIdentity {
    use std::os::unix::fs::MetadataExt;

    PayloadFileIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
    }
}

#[cfg(unix)]
fn same_payload_file_identity(
    actual: &PayloadFileIdentity,
    expected: &PayloadFileIdentity,
) -> Result<(), LcmError> {
    if actual == expected {
        Ok(())
    } else {
        Err(LcmError::InvalidPayloadRef)
    }
}

#[cfg(not(unix))]
fn same_file_identity(_opened: &fs::Metadata, _lstat: &fs::Metadata) -> Result<(), LcmError> {
    Ok(())
}

#[cfg(not(unix))]
fn payload_file_identity(_metadata: &fs::Metadata) -> PayloadFileIdentity {
    PayloadFileIdentity {}
}

#[cfg(not(unix))]
fn same_payload_file_identity(
    _actual: &PayloadFileIdentity,
    _expected: &PayloadFileIdentity,
) -> Result<(), LcmError> {
    Ok(())
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

pub(crate) async fn load_payload_metadata(
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

fn prepare_payload_dir(storage_root: &Path) -> Result<PathBuf, LcmError> {
    let root = canonical_storage_root(storage_root)?;
    let dir = root.join("lcm-payloads");
    match fs::symlink_metadata(&dir) {
        Ok(metadata) => ensure_actual_private_dir(&dir, &metadata)?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(&dir).map_err(|err| LcmError::Io(err.to_string()))?;
            set_private_dir_permissions(&dir)?;
        }
        Err(err) => return Err(LcmError::Io(err.to_string())),
    }
    ensure_payload_dir_under_root(&root, &dir)?;
    Ok(dir)
}

pub(crate) fn existing_payload_dir(storage_root: &Path) -> Result<PathBuf, LcmError> {
    let root = canonical_storage_root(storage_root)?;
    let dir = root.join("lcm-payloads");
    let metadata = fs::symlink_metadata(&dir).map_err(|err| LcmError::Io(err.to_string()))?;
    ensure_actual_private_dir(&dir, &metadata)?;
    ensure_payload_dir_under_root(&root, &dir)?;
    Ok(dir)
}

pub(crate) fn canonical_storage_root(storage_root: &Path) -> Result<PathBuf, LcmError> {
    let metadata =
        fs::symlink_metadata(storage_root).map_err(|err| LcmError::Io(err.to_string()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(LcmError::InvalidPayloadRef);
    }
    storage_root
        .canonicalize()
        .map_err(|err| LcmError::Io(err.to_string()))
}

fn ensure_actual_private_dir(dir: &Path, metadata: &fs::Metadata) -> Result<(), LcmError> {
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

pub(crate) fn ensure_contained(root: &Path, path: &Path) -> Result<(), LcmError> {
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
