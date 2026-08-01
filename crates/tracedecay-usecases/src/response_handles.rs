//! Transport-neutral local response-handle storage.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_runtime_core::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::storage::resolve_response_handle_root;

pub const RESPONSE_HANDLE_TTL_SECS: i64 = 86_400;

const HANDLE_HEX_CHARS: usize = 24;
const HANDLE_PREFIX: &str = "rh_";

#[derive(Debug, Clone)]
pub struct ResponseHandleRecord {
    pub handle: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub content: String,
    pub response_handle_root: PathBuf,
}

impl ResponseHandleRecord {
    pub fn original_chars(&self) -> usize {
        self.content.len()
    }
}

#[derive(Debug, Clone)]
pub enum ResponseHandleLookup {
    Found(ResponseHandleRecord),
    Missing,
    Expired { created_at: i64, expires_at: i64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredResponseHandleRecord {
    created_at: i64,
    expires_at: i64,
    content: String,
}

pub fn is_valid_response_handle(handle: &str) -> bool {
    let Some(hex) = handle.strip_prefix(HANDLE_PREFIX) else {
        return false;
    };
    hex.len() == HANDLE_HEX_CHARS && hex.chars().all(|ch| ch.is_ascii_hexdigit())
}

pub fn store_response_handle(
    project_root: &Path,
    content: &str,
    now: i64,
) -> Result<ResponseHandleRecord> {
    let response_handle_root = resolve_response_handle_root(project_root)?;
    fs::create_dir_all(&response_handle_root)?;
    let handle = response_handle_for(content);
    let record = ResponseHandleRecord {
        handle: handle.clone(),
        created_at: now,
        expires_at: now.saturating_add(RESPONSE_HANDLE_TTL_SECS),
        content: content.to_owned(),
        response_handle_root: response_handle_root.clone(),
    };
    let stored = StoredResponseHandleRecord {
        created_at: record.created_at,
        expires_at: record.expires_at,
        content: record.content.clone(),
    };
    let path = response_handle_path(&response_handle_root, &handle)?;
    let temporary = path.with_extension(format!("json.tmp.{}", std::process::id()));
    fs::write(&temporary, serde_json::to_vec_pretty(&stored)?)?;
    fs::rename(temporary, path)?;
    Ok(record)
}

pub fn retrieve_response_handle(
    project_root: &Path,
    handle: &str,
    now: i64,
) -> Result<ResponseHandleLookup> {
    let response_handle_root = resolve_response_handle_root(project_root)?;
    let path = response_handle_path(&response_handle_root, handle)?;
    if !path.exists() {
        return Ok(ResponseHandleLookup::Missing);
    }
    let stored: StoredResponseHandleRecord = serde_json::from_slice(&fs::read(&path)?)?;
    if stored.expires_at <= now {
        let _ = fs::remove_file(path);
        return Ok(ResponseHandleLookup::Expired {
            created_at: stored.created_at,
            expires_at: stored.expires_at,
        });
    }
    Ok(ResponseHandleLookup::Found(ResponseHandleRecord {
        handle: handle.to_owned(),
        created_at: stored.created_at,
        expires_at: stored.expires_at,
        content: stored.content,
        response_handle_root,
    }))
}

fn response_handle_for(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    format!(
        "{HANDLE_PREFIX}{}",
        hex::encode(&digest[..HANDLE_HEX_CHARS / 2])
    )
}

fn response_handle_path(root: &Path, handle: &str) -> Result<PathBuf> {
    if !is_valid_response_handle(handle) {
        return Err(TraceDecayError::Config {
            message: format!(
                "invalid response handle: expected `{HANDLE_PREFIX}` followed by {HANDLE_HEX_CHARS} hex characters"
            ),
        });
    }
    Ok(root.join(format!("{handle}.json")))
}
