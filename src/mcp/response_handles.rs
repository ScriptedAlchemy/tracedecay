//! Local response-handle cache for reversible MCP truncation.
//!
//! Handles are stored in the resolved project store's `response-handles` root.
//! They are only references to local files, never external URLs or remote
//! identifiers.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::errors::{Result, TraceDecayError};
use crate::storage::resolve_response_handle_root;

pub const RESPONSE_HANDLE_TTL_SECS: i64 = 86_400;
pub const RESPONSE_RETRIEVE_TOOL: &str = "tracedecay_retrieve";

const HANDLE_HEX_CHARS: usize = 24;
const HANDLE_PREFIX: &str = "rh_";

struct ResponseHandleTelemetry {
    truncation_total: AtomicU64,
    reversible_truncation_total: AtomicU64,
    irreversible_truncation_total: AtomicU64,
    bytes_before_truncation_total: AtomicU64,
    bytes_after_truncation_total: AtomicU64,
    truncation_time_us_total: AtomicU64,
    store_attempts: AtomicU64,
    store_success: AtomicU64,
    store_failures: AtomicU64,
    store_skipped_no_project_root: AtomicU64,
    store_time_us_total: AtomicU64,
    retrieve_hits: AtomicU64,
    retrieve_misses: AtomicU64,
    retrieve_expired: AtomicU64,
    retrieve_failures: AtomicU64,
    retrieve_time_us_total: AtomicU64,
    cleanup_runs: AtomicU64,
    cleanup_removed_expired_total: AtomicU64,
    cleanup_failures: AtomicU64,
    cleanup_time_us_total: AtomicU64,
    last_truncation_at: AtomicI64,
    last_store_failure_at: AtomicI64,
    last_retrieve_failure_at: AtomicI64,
    last_expired_at: AtomicI64,
    last_cleanup_at: AtomicI64,
}

impl ResponseHandleTelemetry {
    const fn new() -> Self {
        Self {
            truncation_total: AtomicU64::new(0),
            reversible_truncation_total: AtomicU64::new(0),
            irreversible_truncation_total: AtomicU64::new(0),
            bytes_before_truncation_total: AtomicU64::new(0),
            bytes_after_truncation_total: AtomicU64::new(0),
            truncation_time_us_total: AtomicU64::new(0),
            store_attempts: AtomicU64::new(0),
            store_success: AtomicU64::new(0),
            store_failures: AtomicU64::new(0),
            store_skipped_no_project_root: AtomicU64::new(0),
            store_time_us_total: AtomicU64::new(0),
            retrieve_hits: AtomicU64::new(0),
            retrieve_misses: AtomicU64::new(0),
            retrieve_expired: AtomicU64::new(0),
            retrieve_failures: AtomicU64::new(0),
            retrieve_time_us_total: AtomicU64::new(0),
            cleanup_runs: AtomicU64::new(0),
            cleanup_removed_expired_total: AtomicU64::new(0),
            cleanup_failures: AtomicU64::new(0),
            cleanup_time_us_total: AtomicU64::new(0),
            last_truncation_at: AtomicI64::new(0),
            last_store_failure_at: AtomicI64::new(0),
            last_retrieve_failure_at: AtomicI64::new(0),
            last_expired_at: AtomicI64::new(0),
            last_cleanup_at: AtomicI64::new(0),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ResponseHandleTelemetrySnapshot {
    truncation_total: u64,
    reversible_truncation_total: u64,
    irreversible_truncation_total: u64,
    bytes_before_truncation_total: u64,
    bytes_after_truncation_total: u64,
    truncation_time_us_total: u64,
    store_attempts: u64,
    store_success: u64,
    store_failures: u64,
    store_skipped_no_project_root: u64,
    store_time_us_total: u64,
    retrieve_hits: u64,
    retrieve_misses: u64,
    retrieve_expired: u64,
    retrieve_failures: u64,
    retrieve_time_us_total: u64,
    cleanup_runs: u64,
    cleanup_removed_expired_total: u64,
    cleanup_failures: u64,
    cleanup_time_us_total: u64,
    last_truncation_at: i64,
    last_store_failure_at: i64,
    last_retrieve_failure_at: i64,
    last_expired_at: i64,
    last_cleanup_at: i64,
}

#[derive(Debug, Clone, Copy, Default)]
struct ResponseHandleCacheSnapshot {
    file_count: u64,
    total_bytes: u64,
    corrupt_files: u64,
    scan_failures: u64,
    oldest_expires_at: Option<i64>,
    newest_expires_at: Option<i64>,
}

fn telemetry() -> &'static ResponseHandleTelemetry {
    static TELEMETRY: OnceLock<ResponseHandleTelemetry> = OnceLock::new();
    TELEMETRY.get_or_init(ResponseHandleTelemetry::new)
}

fn telemetry_snapshot() -> ResponseHandleTelemetrySnapshot {
    let telemetry = telemetry();
    ResponseHandleTelemetrySnapshot {
        truncation_total: telemetry.truncation_total.load(Ordering::Relaxed),
        reversible_truncation_total: telemetry
            .reversible_truncation_total
            .load(Ordering::Relaxed),
        irreversible_truncation_total: telemetry
            .irreversible_truncation_total
            .load(Ordering::Relaxed),
        bytes_before_truncation_total: telemetry
            .bytes_before_truncation_total
            .load(Ordering::Relaxed),
        bytes_after_truncation_total: telemetry
            .bytes_after_truncation_total
            .load(Ordering::Relaxed),
        truncation_time_us_total: telemetry.truncation_time_us_total.load(Ordering::Relaxed),
        store_attempts: telemetry.store_attempts.load(Ordering::Relaxed),
        store_success: telemetry.store_success.load(Ordering::Relaxed),
        store_failures: telemetry.store_failures.load(Ordering::Relaxed),
        store_skipped_no_project_root: telemetry
            .store_skipped_no_project_root
            .load(Ordering::Relaxed),
        store_time_us_total: telemetry.store_time_us_total.load(Ordering::Relaxed),
        retrieve_hits: telemetry.retrieve_hits.load(Ordering::Relaxed),
        retrieve_misses: telemetry.retrieve_misses.load(Ordering::Relaxed),
        retrieve_expired: telemetry.retrieve_expired.load(Ordering::Relaxed),
        retrieve_failures: telemetry.retrieve_failures.load(Ordering::Relaxed),
        retrieve_time_us_total: telemetry.retrieve_time_us_total.load(Ordering::Relaxed),
        cleanup_runs: telemetry.cleanup_runs.load(Ordering::Relaxed),
        cleanup_removed_expired_total: telemetry
            .cleanup_removed_expired_total
            .load(Ordering::Relaxed),
        cleanup_failures: telemetry.cleanup_failures.load(Ordering::Relaxed),
        cleanup_time_us_total: telemetry.cleanup_time_us_total.load(Ordering::Relaxed),
        last_truncation_at: telemetry.last_truncation_at.load(Ordering::Relaxed),
        last_store_failure_at: telemetry.last_store_failure_at.load(Ordering::Relaxed),
        last_retrieve_failure_at: telemetry.last_retrieve_failure_at.load(Ordering::Relaxed),
        last_expired_at: telemetry.last_expired_at.load(Ordering::Relaxed),
        last_cleanup_at: telemetry.last_cleanup_at.load(Ordering::Relaxed),
    }
}

pub fn response_handle_stats_json(project_root: Option<&Path>) -> Value {
    let snapshot = telemetry_snapshot();
    let mut stats = json!({
        "truncation_total": snapshot.truncation_total,
        "reversible_truncation_total": snapshot.reversible_truncation_total,
        "irreversible_truncation_total": snapshot.irreversible_truncation_total,
        "bytes_before_truncation_total": snapshot.bytes_before_truncation_total,
        "bytes_after_truncation_total": snapshot.bytes_after_truncation_total,
        "truncation_time_us_total": snapshot.truncation_time_us_total,
        "store_attempts": snapshot.store_attempts,
        "store_success": snapshot.store_success,
        "store_failures": snapshot.store_failures,
        "store_skipped_no_project_root": snapshot.store_skipped_no_project_root,
        "store_time_us_total": snapshot.store_time_us_total,
        "retrieve_hits": snapshot.retrieve_hits,
        "retrieve_misses": snapshot.retrieve_misses,
        "retrieve_expired": snapshot.retrieve_expired,
        "retrieve_failures": snapshot.retrieve_failures,
        "retrieve_time_us_total": snapshot.retrieve_time_us_total,
        "cleanup_runs": snapshot.cleanup_runs,
        "cleanup_removed_expired_total": snapshot.cleanup_removed_expired_total,
        "cleanup_failures": snapshot.cleanup_failures,
        "cleanup_time_us_total": snapshot.cleanup_time_us_total,
        "last_truncation_at": timestamp_json(snapshot.last_truncation_at),
        "last_store_failure_at": timestamp_json(snapshot.last_store_failure_at),
        "last_retrieve_failure_at": timestamp_json(snapshot.last_retrieve_failure_at),
        "last_expired_at": timestamp_json(snapshot.last_expired_at),
        "last_cleanup_at": timestamp_json(snapshot.last_cleanup_at),
    });
    if let (Some(project_root), Some(object)) = (project_root, stats.as_object_mut()) {
        let cache = response_handle_cache_snapshot(project_root);
        object.insert(
            "on_disk".to_string(),
            json!({
                "file_count": cache.file_count,
                "total_bytes": cache.total_bytes,
                "corrupt_files": cache.corrupt_files,
                "scan_failures": cache.scan_failures,
                "oldest_expires_at": cache.oldest_expires_at,
                "newest_expires_at": cache.newest_expires_at,
            }),
        );
    }
    stats
}

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

#[track_caller]
pub fn store_response_handle(
    project_root: &Path,
    content: &str,
    now: i64,
) -> Result<ResponseHandleRecord> {
    let started = Instant::now();
    let caller = std::panic::Location::caller();
    let telemetry = telemetry();
    telemetry.store_attempts.fetch_add(1, Ordering::Relaxed);
    let handle = response_handle_for(content);
    let result = (|| {
        let dir = response_handle_dir(project_root)?;
        let record = ResponseHandleRecord {
            handle: handle.clone(),
            created_at: now,
            expires_at: now.saturating_add(RESPONSE_HANDLE_TTL_SECS),
            content: content.to_string(),
            response_handle_root: dir.clone(),
        };
        fs::create_dir_all(&dir)?;
        let path = response_handle_path_in_dir(&dir, &handle)?;
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
    })();
    telemetry
        .store_time_us_total
        .fetch_add(duration_micros_u64(started.elapsed()), Ordering::Relaxed);
    match &result {
        Ok(_) => {
            telemetry.store_success.fetch_add(1, Ordering::Relaxed);
        }
        Err(error) => {
            telemetry.store_failures.fetch_add(1, Ordering::Relaxed);
            telemetry
                .last_store_failure_at
                .store(now, Ordering::Relaxed);
            eprintln!(
                "[tracedecay] response-handle event=store_failed handle={} payload_bytes={} error_class={} caller={}#{} error={}",
                clipped_handle_for_log(&handle),
                content.len(),
                error_class(error),
                caller.file(),
                caller.line(),
                error
            );
        }
    }
    result
}

#[track_caller]
pub fn retrieve_response_handle(
    project_root: &Path,
    handle: &str,
    now: i64,
) -> Result<ResponseHandleLookup> {
    let started = Instant::now();
    let caller = std::panic::Location::caller();
    let telemetry = telemetry();
    let result = (|| -> Result<RetrieveOutcome> {
        let dir = response_handle_dir(project_root)?;
        read_response_handle_from_root(&dir, handle, now)
    })();
    telemetry
        .retrieve_time_us_total
        .fetch_add(duration_micros_u64(started.elapsed()), Ordering::Relaxed);
    match result {
        Ok(RetrieveOutcome::Hit(record)) => {
            telemetry.retrieve_hits.fetch_add(1, Ordering::Relaxed);
            Ok(ResponseHandleLookup::Found(record))
        }
        Ok(RetrieveOutcome::Miss) => {
            telemetry.retrieve_misses.fetch_add(1, Ordering::Relaxed);
            Ok(ResponseHandleLookup::Missing)
        }
        Ok(RetrieveOutcome::Expired {
            created_at,
            expires_at,
            removed,
        }) => {
            telemetry.retrieve_expired.fetch_add(1, Ordering::Relaxed);
            telemetry.last_expired_at.store(now, Ordering::Relaxed);
            eprintln!(
                "[tracedecay] response-handle event=retrieve_expired handle={} expires_at={} removed={} caller={}#{}",
                clipped_handle_for_log(handle),
                expires_at,
                removed,
                caller.file(),
                caller.line()
            );
            Ok(ResponseHandleLookup::Expired {
                created_at,
                expires_at,
            })
        }
        Err(error) => {
            telemetry.retrieve_failures.fetch_add(1, Ordering::Relaxed);
            telemetry
                .last_retrieve_failure_at
                .store(now, Ordering::Relaxed);
            eprintln!(
                "[tracedecay] response-handle event=retrieve_failed handle={} error_class={} caller={}#{} error={}",
                clipped_handle_for_log(handle),
                error_class(&error),
                caller.file(),
                caller.line(),
                error
            );
            Err(error)
        }
    }
}

#[track_caller]
pub fn cleanup_expired_response_handles(project_root: &Path, now: i64) -> Result<usize> {
    let started = Instant::now();
    let caller = std::panic::Location::caller();
    let telemetry = telemetry();
    telemetry.cleanup_runs.fetch_add(1, Ordering::Relaxed);
    let result = (|| {
        let dir = response_handle_dir(project_root)?;
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
    })();
    telemetry
        .cleanup_time_us_total
        .fetch_add(duration_micros_u64(started.elapsed()), Ordering::Relaxed);
    match &result {
        Ok(removed) => {
            telemetry
                .cleanup_removed_expired_total
                .fetch_add(*removed as u64, Ordering::Relaxed);
            telemetry.last_cleanup_at.store(now, Ordering::Relaxed);
            if *removed > 0 {
                eprintln!(
                    "[tracedecay] response-handle event=cleanup_expired removed={} caller={}#{}",
                    removed,
                    caller.file(),
                    caller.line()
                );
            }
        }
        Err(error) => {
            telemetry.cleanup_failures.fetch_add(1, Ordering::Relaxed);
            telemetry.last_cleanup_at.store(now, Ordering::Relaxed);
            eprintln!(
                "[tracedecay] response-handle event=cleanup_failed error_class={} caller={}#{} error={}",
                error_class(error),
                caller.file(),
                caller.line(),
                error
            );
        }
    }
    result
}

#[cfg(test)]
pub(crate) fn retrieve_response_handle_from_root(
    response_handle_root: &Path,
    handle: &str,
    now: i64,
) -> Result<ResponseHandleLookup> {
    let outcome = read_response_handle_from_root(response_handle_root, handle, now)?;
    Ok(match outcome {
        RetrieveOutcome::Hit(record) => ResponseHandleLookup::Found(record),
        RetrieveOutcome::Miss => ResponseHandleLookup::Missing,
        RetrieveOutcome::Expired {
            created_at,
            expires_at,
            ..
        } => ResponseHandleLookup::Expired {
            created_at,
            expires_at,
        },
    })
}

enum RetrieveOutcome {
    Hit(ResponseHandleRecord),
    Miss,
    Expired {
        created_at: i64,
        expires_at: i64,
        removed: bool,
    },
}

fn read_response_handle_from_root(
    response_handle_root: &Path,
    handle: &str,
    now: i64,
) -> Result<RetrieveOutcome> {
    let path = response_handle_path_in_dir(response_handle_root, handle)?;
    if !path.exists() {
        return Ok(RetrieveOutcome::Miss);
    }
    let payload = fs::read_to_string(&path)?;
    let record: StoredResponseHandleRecord = serde_json::from_str(&payload)?;
    if record.expires_at <= now {
        let removed = fs::remove_file(&path).is_ok();
        return Ok(RetrieveOutcome::Expired {
            created_at: record.created_at,
            expires_at: record.expires_at,
            removed,
        });
    }
    Ok(RetrieveOutcome::Hit(ResponseHandleRecord {
        handle: handle.to_string(),
        created_at: record.created_at,
        expires_at: record.expires_at,
        content: record.content,
        response_handle_root: response_handle_root.to_path_buf(),
    }))
}

fn response_handle_for(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    let hex = hex::encode(&digest[..(HANDLE_HEX_CHARS / 2)]);
    format!("{HANDLE_PREFIX}{hex}")
}

fn response_handle_dir(project_root: &Path) -> Result<PathBuf> {
    resolve_response_handle_root(project_root)
}

fn response_handle_path_in_dir(response_handle_root: &Path, handle: &str) -> Result<PathBuf> {
    validate_handle(handle)?;
    Ok(response_handle_root.join(format!("{handle}.json")))
}

fn validate_handle(handle: &str) -> Result<()> {
    let Some(hex) = handle.strip_prefix(HANDLE_PREFIX) else {
        return Err(TraceDecayError::Config {
            message: format!(
                "invalid response handle: expected `{HANDLE_PREFIX}` followed by {HANDLE_HEX_CHARS} hex characters copied from a truncated MCP response envelope"
            ),
        });
    };
    if hex.len() != HANDLE_HEX_CHARS || !hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(TraceDecayError::Config {
            message: format!(
                "invalid response handle: expected `{HANDLE_PREFIX}` followed by {HANDLE_HEX_CHARS} hex characters copied from a truncated MCP response envelope"
            ),
        });
    }
    Ok(())
}

#[track_caller]
pub fn observe_response_truncation(
    original_bytes: usize,
    emitted_bytes: usize,
    reversible: bool,
    now: i64,
    handle_status: &'static str,
    duration: Duration,
) {
    let caller = std::panic::Location::caller();
    let telemetry = telemetry();
    telemetry.truncation_total.fetch_add(1, Ordering::Relaxed);
    telemetry.bytes_before_truncation_total.fetch_add(
        original_bytes.min(u64::MAX as usize) as u64,
        Ordering::Relaxed,
    );
    telemetry.bytes_after_truncation_total.fetch_add(
        emitted_bytes.min(u64::MAX as usize) as u64,
        Ordering::Relaxed,
    );
    telemetry
        .truncation_time_us_total
        .fetch_add(duration_micros_u64(duration), Ordering::Relaxed);
    telemetry.last_truncation_at.store(now, Ordering::Relaxed);
    if reversible {
        telemetry
            .reversible_truncation_total
            .fetch_add(1, Ordering::Relaxed);
    } else {
        telemetry
            .irreversible_truncation_total
            .fetch_add(1, Ordering::Relaxed);
    }
    eprintln!(
        "[tracedecay] response-handle event=truncated reversible={} handle_status={} original_bytes={} emitted_bytes={} caller={}#{}",
        reversible,
        handle_status,
        original_bytes,
        emitted_bytes,
        caller.file(),
        caller.line()
    );
}

pub fn note_response_handle_store_skipped_no_project_root() {
    telemetry()
        .store_skipped_no_project_root
        .fetch_add(1, Ordering::Relaxed);
}

fn response_handle_cache_snapshot(project_root: &Path) -> ResponseHandleCacheSnapshot {
    let mut snapshot = ResponseHandleCacheSnapshot::default();
    let Ok(dir) = response_handle_dir(project_root) else {
        snapshot.scan_failures = 1;
        return snapshot;
    };
    if !dir.exists() {
        return snapshot;
    }
    let Ok(entries) = fs::read_dir(&dir) else {
        snapshot.scan_failures = 1;
        return snapshot;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            snapshot.scan_failures += 1;
            continue;
        };
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        if let Ok(meta) = fs::metadata(&path) {
            snapshot.file_count += 1;
            snapshot.total_bytes = snapshot.total_bytes.saturating_add(meta.len());
        } else {
            snapshot.scan_failures += 1;
            continue;
        }
        let Ok(payload) = fs::read_to_string(&path) else {
            snapshot.corrupt_files += 1;
            continue;
        };
        let Ok(record) = serde_json::from_str::<StoredResponseHandleRecord>(&payload) else {
            snapshot.corrupt_files += 1;
            continue;
        };
        snapshot.oldest_expires_at = Some(
            snapshot
                .oldest_expires_at
                .map_or(record.expires_at, |oldest| oldest.min(record.expires_at)),
        );
        snapshot.newest_expires_at = Some(
            snapshot
                .newest_expires_at
                .map_or(record.expires_at, |newest| newest.max(record.expires_at)),
        );
    }
    snapshot
}

fn timestamp_json(value: i64) -> Value {
    if value > 0 {
        json!(value)
    } else {
        Value::Null
    }
}

fn duration_micros_u64(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

fn error_class(error: &TraceDecayError) -> &'static str {
    match error {
        TraceDecayError::File { .. } => "file",
        TraceDecayError::Parse { .. } => "parse",
        TraceDecayError::Database { .. } => "database",
        TraceDecayError::Search { .. } => "search",
        TraceDecayError::Config { .. } => "config",
        TraceDecayError::SyncLock { .. } => "sync_lock",
        TraceDecayError::Io(_) => "io",
        TraceDecayError::Libsql(_) => "libsql",
        TraceDecayError::Json(_) => "json",
    }
}

fn clipped_handle_for_log(handle: &str) -> String {
    const MAX_LOG_HANDLE_CHARS: usize = 64;
    let mut chars = handle.chars();
    let clipped: String = chars.by_ref().take(MAX_LOG_HANDLE_CHARS).collect();
    if chars.next().is_some() {
        format!("{clipped}…")
    } else {
        clipped
    }
}
