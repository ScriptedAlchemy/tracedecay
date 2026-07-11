use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::daemon::{HookRouteMetadata, HookTerminalReceipt};
use crate::errors::{Result, TraceDecayError};
use crate::storage::PrivateStoreIo;
use crate::tracedecay::current_timestamp;

const STATE_FILE: &str = "host_receipts.json";
const LOCK_FILE: &str = "host_receipts.lock";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingHostReceipt {
    pub generation: u64,
    pub session_key: String,
    pub dedupe_key: String,
    pub received_at: i64,
    pub route: Option<HookRouteMetadata>,
    pub receipt: HookTerminalReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyHostReceipt {
    pub pending: PendingHostReceipt,
    pub transcript_watermark: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct HostReceiptReadiness {
    generation: u64,
    transcript_watermark: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct HostReceiptState {
    #[serde(default)]
    generation: u64,
    #[serde(default)]
    sessions: BTreeMap<String, PendingHostReceipt>,
    #[serde(default)]
    readiness: BTreeMap<String, HostReceiptReadiness>,
    #[serde(default)]
    recent_dedupe_keys: Vec<String>,
}

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

fn session_key(route: Option<&HookRouteMetadata>) -> String {
    route
        .and_then(|route| route.session_id.as_deref().or(route.thread_id.as_deref()))
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown-session")
        .to_string()
}

fn dedupe_key(session: &str, receipt: &HookTerminalReceipt) -> String {
    format!(
        "{session}\u{1f}{}\u{1f}{}\u{1f}{}",
        receipt.turn_id.as_deref().unwrap_or_default(),
        receipt.tool_call_id.as_deref().unwrap_or_default(),
        receipt.transcript_watermark.as_deref().unwrap_or_default(),
    )
}

fn paths(dashboard_root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    (
        dashboard_root.join(STATE_FILE),
        dashboard_root.join(format!(".{STATE_FILE}.tmp")),
        dashboard_root.join(LOCK_FILE),
    )
}

fn with_locked_state<T>(
    dashboard_root: &Path,
    mutate: impl FnOnce(&mut HostReceiptState) -> Result<T>,
) -> Result<T> {
    std::fs::create_dir_all(dashboard_root).map_err(|error| {
        config_error(format!("failed to create host receipt directory: {error}"))
    })?;
    let (state_path, temp_path, lock_path) = paths(dashboard_root);
    let mut lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| config_error(format!("failed to open host receipt lock: {error}")))?;
    lock.lock_exclusive()
        .map_err(|error| config_error(format!("failed to lock host receipts: {error}")))?;
    let mut state = std::fs::read(&state_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let output = mutate(&mut state)?;
    let bytes = serde_json::to_vec_pretty(&state)?;
    PrivateStoreIo::write_file_atomically(&state_path, &temp_path, &bytes)
        .map_err(|error| config_error(format!("failed to persist host receipts: {error}")))?;
    let _ = lock.flush();
    let _ = lock.unlock();
    Ok(output)
}

pub async fn record(
    dashboard_root: &Path,
    route: Option<HookRouteMetadata>,
    receipt: HookTerminalReceipt,
) -> Result<bool> {
    let root = dashboard_root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        with_locked_state(&root, |state| {
            let session = session_key(route.as_ref());
            let key = dedupe_key(&session, &receipt);
            if state.recent_dedupe_keys.contains(&key) {
                return Ok(false);
            }
            state.recent_dedupe_keys.push(key.clone());
            if state.recent_dedupe_keys.len() > 256 {
                state.recent_dedupe_keys.remove(0);
            }
            state.generation = state.generation.saturating_add(1);
            state.sessions.insert(
                session.clone(),
                PendingHostReceipt {
                    generation: state.generation,
                    session_key: session,
                    dedupe_key: key,
                    received_at: current_timestamp(),
                    route,
                    receipt,
                },
            );
            Ok(true)
        })
    })
    .await
    .map_err(|error| config_error(format!("host receipt task failed: {error}")))?
}

pub async fn mark_turn_ingested(
    dashboard_root: &Path,
    route: Option<HookRouteMetadata>,
    transcript_watermark: &str,
) -> Result<()> {
    let root = dashboard_root.to_path_buf();
    let watermark = transcript_watermark.to_string();
    tokio::task::spawn_blocking(move || {
        with_locked_state(&root, |state| {
            let session = session_key(route.as_ref());
            state.readiness.insert(
                session,
                HostReceiptReadiness {
                    generation: state.generation,
                    transcript_watermark: watermark,
                },
            );
            Ok(())
        })
    })
    .await
    .map_err(|error| config_error(format!("host receipt task failed: {error}")))?
}

pub async fn oldest_ready(dashboard_root: &Path) -> Result<Option<ReadyHostReceipt>> {
    let root = dashboard_root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        with_locked_state(&root, |state| {
            let pending = state
                .sessions
                .values()
                .filter(|receipt| {
                    state
                        .readiness
                        .get(&receipt.session_key)
                        .is_some_and(|ready| ready.generation >= receipt.generation)
                })
                .min_by_key(|receipt| receipt.generation)
                .cloned();
            Ok(pending.and_then(|pending| {
                state
                    .readiness
                    .get(&pending.session_key)
                    .map(|ready| ReadyHostReceipt {
                        pending,
                        transcript_watermark: ready.transcript_watermark.clone(),
                    })
            }))
        })
    })
    .await
    .map_err(|error| config_error(format!("host receipt task failed: {error}")))?
}

pub async fn mark_consumed(
    dashboard_root: &Path,
    session_key: &str,
    generation: u64,
) -> Result<()> {
    let root = dashboard_root.to_path_buf();
    let session_key = session_key.to_string();
    tokio::task::spawn_blocking(move || {
        with_locked_state(&root, |state| {
            if state
                .sessions
                .get(&session_key)
                .is_some_and(|receipt| receipt.generation <= generation)
            {
                state.sessions.remove(&session_key);
            }
            Ok(())
        })
    })
    .await
    .map_err(|error| config_error(format!("host receipt task failed: {error}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(id: &str) -> HookTerminalReceipt {
        HookTerminalReceipt {
            tool_call_id: Some(id.to_string()),
            turn_id: Some("turn-1".to_string()),
            status: Some("success".to_string()),
            duration_ms: Some(12),
            transcript_watermark: Some("watermark-1".to_string()),
        }
    }

    #[tokio::test]
    async fn deduplicates_and_consumes_receipts() {
        let tmp = tempfile::tempdir().unwrap();
        let route = Some(HookRouteMetadata {
            session_id: Some("session-1".to_string()),
            thread_id: None,
            cwd: None,
            worktree: None,
            branch: None,
        });
        assert!(
            record(tmp.path(), route.clone(), receipt("call-1"))
                .await
                .unwrap()
        );
        assert!(!record(tmp.path(), route, receipt("call-1")).await.unwrap());
        mark_turn_ingested(tmp.path(), pending_route("session-1"), "message-1")
            .await
            .unwrap();
        let pending = oldest_ready(tmp.path()).await.unwrap().unwrap().pending;
        mark_consumed(tmp.path(), &pending.session_key, pending.generation)
            .await
            .unwrap();
        assert!(oldest_ready(tmp.path()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn serves_parallel_sessions_oldest_first() {
        let tmp = tempfile::tempdir().unwrap();
        for session_id in ["session-1", "session-2"] {
            let route = pending_route(session_id);
            assert!(
                record(tmp.path(), route, receipt(session_id))
                    .await
                    .unwrap()
            );
            mark_turn_ingested(tmp.path(), pending_route(session_id), session_id)
                .await
                .unwrap();
        }
        let pending = oldest_ready(tmp.path()).await.unwrap().unwrap();
        assert_eq!(pending.pending.session_key, "session-1");
    }

    fn pending_route(session_id: &str) -> Option<HookRouteMetadata> {
        Some(HookRouteMetadata {
            session_id: Some(session_id.to_string()),
            thread_id: None,
            cwd: None,
            worktree: None,
            branch: None,
        })
    }
}
