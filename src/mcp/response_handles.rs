//! Local response-handle cache for reversible MCP truncation.
//!
//! Handles are project-local and stored under `.tracedecay/response-handles`.
//! They are only references to local files, never external URLs or remote
//! identifiers.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::get_tracedecay_dir;
use crate::errors::{Result, TraceDecayError};

pub const RESPONSE_HANDLE_TTL_SECS: i64 = 86_400;
pub const RESPONSE_RETRIEVE_TOOL: &str = "tracedecay_retrieve";

const CACHE_DIR_NAME: &str = "response-handles";
const HANDLE_HEX_CHARS: usize = 24;
const HANDLE_PREFIX: &str = "rh_";

#[derive(Debug, Clone)]
pub struct ResponseHandleRecord {
    pub handle: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub content: String,
}

impl ResponseHandleRecord {
    pub fn original_chars(&self) -> usize {
        self.content.len()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredResponseHandleRecord {
    created_at: i64,
    expires_at: i64,
    content: String,
}

pub fn store_response_handle(
    project_root: &Path,
    content: &str,
    now: i64,
) -> Result<ResponseHandleRecord> {
    let handle = response_handle_for(content);
    let record = ResponseHandleRecord {
        handle: handle.clone(),
        created_at: now,
        expires_at: now.saturating_add(RESPONSE_HANDLE_TTL_SECS),
        content: content.to_string(),
    };
    let dir = response_handle_dir(project_root);
    fs::create_dir_all(&dir)?;
    let path = response_handle_path(project_root, &handle)?;
    let tmp_path = path.with_extension(format!("json.tmp.{}", std::process::id()));
    let stored = StoredResponseHandleRecord {
        created_at: record.created_at,
        expires_at: record.expires_at,
        content: record.content.clone(),
    };
    let payload = serde_json::to_string_pretty(&stored)?;
    fs::write(&tmp_path, payload)?;
    fs::rename(&tmp_path, &path)?;
    Ok(record)
}

pub fn retrieve_response_handle(
    project_root: &Path,
    handle: &str,
    now: i64,
) -> Result<Option<ResponseHandleRecord>> {
    let path = response_handle_path(project_root, handle)?;
    if !path.exists() {
        return Ok(None);
    }
    let payload = fs::read_to_string(&path)?;
    let record: StoredResponseHandleRecord = serde_json::from_str(&payload)?;
    if record.expires_at <= now {
        let _ = fs::remove_file(&path);
        return Ok(None);
    }
    Ok(Some(ResponseHandleRecord {
        handle: handle.to_string(),
        created_at: record.created_at,
        expires_at: record.expires_at,
        content: record.content,
    }))
}

pub fn cleanup_expired_response_handles(project_root: &Path, now: i64) -> Result<usize> {
    let dir = response_handle_dir(project_root);
    if !dir.exists() {
        return Ok(0);
    }
    let mut removed = 0;
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(payload) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(record) = serde_json::from_str::<StoredResponseHandleRecord>(&payload) else {
            continue;
        };
        if record.expires_at <= now && fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

fn response_handle_for(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    let hex = hex::encode(&digest[..(HANDLE_HEX_CHARS / 2)]);
    format!("{HANDLE_PREFIX}{hex}")
}

fn response_handle_dir(project_root: &Path) -> PathBuf {
    get_tracedecay_dir(project_root).join(CACHE_DIR_NAME)
}

fn response_handle_path(project_root: &Path, handle: &str) -> Result<PathBuf> {
    validate_handle(handle)?;
    Ok(response_handle_dir(project_root).join(format!("{handle}.json")))
}

fn validate_handle(handle: &str) -> Result<()> {
    let Some(hex) = handle.strip_prefix(HANDLE_PREFIX) else {
        return Err(TraceDecayError::Config {
            message: "invalid response handle".to_string(),
        });
    };
    if hex.len() != HANDLE_HEX_CHARS || !hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(TraceDecayError::Config {
            message: "invalid response handle".to_string(),
        });
    }
    Ok(())
}
