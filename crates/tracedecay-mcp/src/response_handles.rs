//! Local response-handle cache for reversible MCP truncation.
//!
//! Handles are stored in the owning store's `response-handles` root.
//! They are only references to local files, never external URLs or remote
//! identifiers.

use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use tracedecay_domain::errors::{Result, TraceDecayError};

// The transport-neutral handle authority lives in
// `tracedecay_session_memory::response_handles`. This module wraps it with the
// MCP telemetry counters and re-exports the record types so MCP callers import
// the authority and its telemetry-wrapped operations from one path.
pub use tracedecay_session_memory::response_handles::{
    RESPONSE_HANDLE_TTL_SECS, ResponseHandleError, ResponseHandleLookup, ResponseHandleRecord,
};
pub const RESPONSE_RETRIEVE_TOOL: &str = "tracedecay_retrieve";

#[derive(Default)]
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
    cleanup_removed_staging_total: AtomicU64,
    cleanup_removed_tombstones_total: AtomicU64,
    cleanup_failures: AtomicU64,
    cleanup_time_us_total: AtomicU64,
    last_truncation_at: AtomicI64,
    last_store_failure_at: AtomicI64,
    last_retrieve_failure_at: AtomicI64,
    last_expired_at: AtomicI64,
    last_cleanup_at: AtomicI64,
}

fn telemetry() -> &'static ResponseHandleTelemetry {
    static TELEMETRY: OnceLock<ResponseHandleTelemetry> = OnceLock::new();
    TELEMETRY.get_or_init(ResponseHandleTelemetry::default)
}

fn public_inventory_problem(error: &ResponseHandleError) -> (&'static str, &'static str) {
    match error {
        ResponseHandleError::CorruptRecord { .. } => (
            "corrupt_handle_record",
            "The local response-handle inventory contains a corrupt record.",
        ),
        ResponseHandleError::InvalidHandle | ResponseHandleError::Store(_) => (
            "handle_inventory_unavailable",
            "The local response-handle inventory is unavailable.",
        ),
    }
}

/// Why a stored record could not be read back: the record exists but failed
/// integrity validation.
pub const RETRIEVE_CORRUPT_RECORD_REASON: &str = "corrupt_handle_record";
/// Why a stored record could not be read back: the cache itself is unreadable.
pub const RETRIEVE_READ_FAILED_REASON: &str = "handle_read_failed";

/// The caller-facing form of a handle-read failure. An invalid handle stays a
/// request error; a store failure becomes a typed, retryable route problem
/// that names no local path, so it keeps its reason across the owner boundary.
pub fn public_retrieve_error(error: ResponseHandleError) -> TraceDecayError {
    match error {
        ResponseHandleError::InvalidHandle => error.into(),
        ResponseHandleError::CorruptRecord { .. } => TraceDecayError::project_route(
            RETRIEVE_CORRUPT_RECORD_REASON,
            true,
            "corrupt response-handle record: cached payload failed integrity validation",
        ),
        ResponseHandleError::Store(_) => TraceDecayError::project_route(
            RETRIEVE_READ_FAILED_REASON,
            true,
            "response-handle cache is unavailable",
        ),
    }
}

pub fn response_handle_stats_json(response_handle_root: Option<&Path>) -> Value {
    let telemetry = telemetry();
    let counter = |value: &AtomicU64| value.load(Ordering::Relaxed);
    let timestamp = |value: &AtomicI64| value.load(Ordering::Relaxed);
    let mut stats = json!({
        "truncation_total": counter(&telemetry.truncation_total),
        "reversible_truncation_total": counter(&telemetry.reversible_truncation_total),
        "irreversible_truncation_total": counter(&telemetry.irreversible_truncation_total),
        "bytes_before_truncation_total": counter(&telemetry.bytes_before_truncation_total),
        "bytes_after_truncation_total": counter(&telemetry.bytes_after_truncation_total),
        "truncation_time_us_total": counter(&telemetry.truncation_time_us_total),
        "store_attempts": counter(&telemetry.store_attempts),
        "store_success": counter(&telemetry.store_success),
        "store_failures": counter(&telemetry.store_failures),
        "store_skipped_no_project_root": counter(&telemetry.store_skipped_no_project_root),
        "store_time_us_total": counter(&telemetry.store_time_us_total),
        "retrieve_hits": counter(&telemetry.retrieve_hits),
        "retrieve_misses": counter(&telemetry.retrieve_misses),
        "retrieve_expired": counter(&telemetry.retrieve_expired),
        "retrieve_failures": counter(&telemetry.retrieve_failures),
        "retrieve_time_us_total": counter(&telemetry.retrieve_time_us_total),
        "cleanup_runs": counter(&telemetry.cleanup_runs),
        "cleanup_removed_expired_total": counter(&telemetry.cleanup_removed_expired_total),
        "cleanup_removed_staging_total": counter(&telemetry.cleanup_removed_staging_total),
        "cleanup_removed_tombstones_total": counter(&telemetry.cleanup_removed_tombstones_total),
        "cleanup_failures": counter(&telemetry.cleanup_failures),
        "cleanup_time_us_total": counter(&telemetry.cleanup_time_us_total),
        "last_truncation_at": timestamp_json(timestamp(&telemetry.last_truncation_at)),
        "last_store_failure_at": timestamp_json(timestamp(&telemetry.last_store_failure_at)),
        "last_retrieve_failure_at": timestamp_json(timestamp(&telemetry.last_retrieve_failure_at)),
        "last_expired_at": timestamp_json(timestamp(&telemetry.last_expired_at)),
        "last_cleanup_at": timestamp_json(timestamp(&telemetry.last_cleanup_at)),
    });
    if let (Some(root), Some(object)) = (response_handle_root, stats.as_object_mut()) {
        let on_disk =
            match tracedecay_session_memory::response_handles::inventory_response_handles(root) {
                Ok(inventory) => json!({
                    "available": true,
                    "file_count": inventory.file_count,
                    "total_bytes": inventory.total_bytes,
                    "oldest_expires_at": inventory.oldest_expires_at,
                    "newest_expires_at": inventory.newest_expires_at,
                }),
                Err(error) => {
                    let (reason_code, detail) = public_inventory_problem(&error);
                    json!({
                        "available": false,
                        "reason_code": reason_code,
                        "detail": detail,
                    })
                }
            };
        object.insert("on_disk".to_string(), on_disk);
    }
    stats
}

#[track_caller]
pub fn store_response_handle(root: &Path, content: &str, now: i64) -> Result<ResponseHandleRecord> {
    let started = Instant::now();
    let caller = std::panic::Location::caller();
    let telemetry = telemetry();
    telemetry.store_attempts.fetch_add(1, Ordering::Relaxed);
    let result =
        tracedecay_session_memory::response_handles::store_response_handle(root, content, now);
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
            tracing::warn!(
                payload_bytes = content.len(),
                error_class = error_class(error),
                caller_file = caller.file(),
                caller_line = caller.line(),
                %error,
                "response handle store failed"
            );
        }
    }
    result.map_err(TraceDecayError::from)
}

#[track_caller]
#[hotpath::measure(label = "mcp.server.response.handle_retrieve")]
pub fn retrieve_response_handle(
    root: &Path,
    handle: &str,
    now: i64,
) -> std::result::Result<ResponseHandleLookup, ResponseHandleError> {
    let started = Instant::now();
    let caller = std::panic::Location::caller();
    let telemetry = telemetry();
    let result =
        tracedecay_session_memory::response_handles::retrieve_response_handle(root, handle, now);
    telemetry
        .retrieve_time_us_total
        .fetch_add(duration_micros_u64(started.elapsed()), Ordering::Relaxed);
    match result {
        Ok(ResponseHandleLookup::Found(record)) => {
            telemetry.retrieve_hits.fetch_add(1, Ordering::Relaxed);
            Ok(ResponseHandleLookup::Found(record))
        }
        Ok(ResponseHandleLookup::Missing) => {
            telemetry.retrieve_misses.fetch_add(1, Ordering::Relaxed);
            Ok(ResponseHandleLookup::Missing)
        }
        Ok(ResponseHandleLookup::Expired {
            created_at,
            expires_at,
        }) => {
            telemetry.retrieve_expired.fetch_add(1, Ordering::Relaxed);
            telemetry.last_expired_at.store(now, Ordering::Relaxed);
            tracing::debug!(
                handle = %clipped_handle_for_log(handle),
                expires_at,
                caller_file = caller.file(),
                caller_line = caller.line(),
                "response handle expired"
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
            tracing::warn!(
                handle = %clipped_handle_for_log(handle),
                error_class = error_class(&error),
                caller_file = caller.file(),
                caller_line = caller.line(),
                %error,
                "response handle retrieval failed"
            );
            Err(error)
        }
    }
}

#[track_caller]
#[hotpath::measure(label = "mcp.server.response.handle_cleanup")]
pub fn cleanup_expired_response_handles(root: &Path, now: i64) -> Result<usize> {
    let started = Instant::now();
    let caller = std::panic::Location::caller();
    let telemetry = telemetry();
    telemetry.cleanup_runs.fetch_add(1, Ordering::Relaxed);
    let result =
        tracedecay_session_memory::response_handles::cleanup_expired_response_handles(root, now);
    telemetry
        .cleanup_time_us_total
        .fetch_add(duration_micros_u64(started.elapsed()), Ordering::Relaxed);
    match &result {
        Ok(cleanup) => {
            telemetry
                .cleanup_removed_expired_total
                .fetch_add(cleanup.removed_expired as u64, Ordering::Relaxed);
            telemetry
                .cleanup_removed_staging_total
                .fetch_add(cleanup.removed_staging as u64, Ordering::Relaxed);
            telemetry
                .cleanup_removed_tombstones_total
                .fetch_add(cleanup.removed_tombstones as u64, Ordering::Relaxed);
            telemetry.last_cleanup_at.store(now, Ordering::Relaxed);
            if cleanup.removed_expired > 0
                || cleanup.removed_staging > 0
                || cleanup.removed_tombstones > 0
            {
                tracing::debug!(
                    removed = cleanup.removed_expired,
                    removed_staging = cleanup.removed_staging,
                    removed_tombstones = cleanup.removed_tombstones,
                    caller_file = caller.file(),
                    caller_line = caller.line(),
                    "expired response handles removed"
                );
            }
        }
        Err(error) => {
            telemetry.cleanup_failures.fetch_add(1, Ordering::Relaxed);
            telemetry.last_cleanup_at.store(now, Ordering::Relaxed);
            tracing::warn!(
                error_class = error_class(error),
                caller_file = caller.file(),
                caller_line = caller.line(),
                %error,
                "response handle cleanup failed"
            );
        }
    }
    result
        .map(|cleanup| cleanup.removed_expired)
        .map_err(TraceDecayError::from)
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
    tracing::trace!(
        reversible,
        handle_status,
        original_bytes,
        emitted_bytes,
        caller_file = caller.file(),
        caller_line = caller.line(),
        "response truncated"
    );
}

pub fn note_response_handle_store_skipped_no_project_root() {
    telemetry()
        .store_skipped_no_project_root
        .fetch_add(1, Ordering::Relaxed);
}

fn timestamp_json(value: i64) -> Value {
    if value > 0 { json!(value) } else { Value::Null }
}

fn duration_micros_u64(duration: Duration) -> u64 {
    tracedecay_runtime_core::tracedecay::saturating_duration_micros(duration)
}

fn error_class(error: &ResponseHandleError) -> &'static str {
    let error = match error {
        ResponseHandleError::InvalidHandle => return "invalid_handle",
        ResponseHandleError::CorruptRecord { .. } => return "corrupt_record",
        ResponseHandleError::Store(error) => error,
    };
    match error {
        TraceDecayError::ResetRequired { .. } => "reset_required",
        TraceDecayError::File { .. } => "file",
        TraceDecayError::Database { .. } => "database",
        TraceDecayError::Search { .. } => "search",
        TraceDecayError::Config { .. } => "config",
        TraceDecayError::HostCliUnavailable { .. } => "host_cli_unavailable",
        TraceDecayError::ProfileResetRequired { .. } => "profile_reset_required",
        TraceDecayError::ProjectRoute { .. } => "project_route",
        TraceDecayError::ProjectOpen { .. } => "project_open",
        TraceDecayError::InvalidRequest { .. } => "invalid_request",
        TraceDecayError::ToolRefused { .. } => "tool_refused",
        TraceDecayError::SyncLock { .. } => "sync_lock",
        TraceDecayError::LockDeadline { .. } => "lock_deadline",
        TraceDecayError::Io(_) => "io",
        TraceDecayError::Sqlite(_) => "sqlite",
        TraceDecayError::Json(_) => "json",
        TraceDecayError::Automation(_) => "automation",
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

#[cfg(test)]
mod tests {
    use tracedecay_domain::errors::InvalidRequestReason;

    use super::*;

    #[test]
    fn public_inventory_problem_never_exposes_the_local_path() {
        let error = ResponseHandleError::CorruptRecord {
            path: "/private/operator/cache/secret.json".into(),
            reason: "invalid JSON".to_string(),
        };

        let (reason_code, detail) = public_inventory_problem(&error);

        assert_eq!(reason_code, "corrupt_handle_record");
        assert!(!detail.contains("/private/operator"));

        let sanitized = public_retrieve_error(error);
        assert_eq!(
            sanitized.project_route_context(),
            Some((
                "corrupt_handle_record",
                true,
                "corrupt response-handle record: cached payload failed integrity validation"
            ))
        );
    }

    /// A store failure whose text reads like a corrupt record or an invalid
    /// handle is still the cache being unavailable.
    #[test]
    fn store_failures_are_classified_by_type_not_text() {
        let lookalike = || {
            ResponseHandleError::Store(TraceDecayError::File {
                message: "corrupt response-handle record: invalid response handle:".to_string(),
                path: "/cache/rh_000000000000000000000000.json".to_string(),
            })
        };

        assert_eq!(
            public_inventory_problem(&lookalike()).0,
            "handle_inventory_unavailable"
        );
        assert_eq!(
            public_retrieve_error(lookalike()).project_route_context(),
            Some((
                "handle_read_failed",
                true,
                "response-handle cache is unavailable"
            ))
        );
        assert!(matches!(
            public_retrieve_error(ResponseHandleError::InvalidHandle),
            TraceDecayError::InvalidRequest {
                reason: InvalidRequestReason::InvalidResponseHandle,
                ..
            }
        ));
    }
}
