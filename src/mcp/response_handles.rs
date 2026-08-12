//! Local response-handle cache for reversible MCP truncation.
//!
//! Handles are stored in the resolved project store's `response-handles` root.
//! They are only references to local files, never external URLs or remote
//! identifiers.

use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::errors::{Result, TraceDecayError};

// The transport-neutral handle authority lives in `tracedecay-usecases`. This
// module keeps the MCP telemetry and adapters.
pub use tracedecay_usecases::response_handles::{
    RESPONSE_HANDLE_TTL_SECS, ResponseHandleLookup, ResponseHandleRecord,
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

fn public_inventory_problem(error: &TraceDecayError) -> (&'static str, &'static str) {
    match error {
        TraceDecayError::File { message, .. }
            if message.starts_with("corrupt response-handle record:") =>
        {
            (
                "corrupt_handle_record",
                "The local response-handle inventory contains a corrupt record.",
            )
        }
        _ => (
            "handle_inventory_unavailable",
            "The local response-handle inventory is unavailable.",
        ),
    }
}

pub(crate) fn public_retrieve_error(error: TraceDecayError) -> TraceDecayError {
    match error {
        TraceDecayError::Config { message } if message.starts_with("invalid response handle:") => {
            TraceDecayError::Config { message }
        }
        TraceDecayError::File { message, .. }
            if message.starts_with("corrupt response-handle record:") =>
        {
            TraceDecayError::File {
                message:
                    "corrupt response-handle record: cached payload failed integrity validation"
                        .to_string(),
                path: "response-handles".to_string(),
            }
        }
        _ => TraceDecayError::File {
            message: "response-handle cache is unavailable".to_string(),
            path: "response-handles".to_string(),
        },
    }
}

pub fn response_handle_stats_json(project_root: Option<&Path>) -> Value {
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
    if let (Some(project_root), Some(object)) = (project_root, stats.as_object_mut()) {
        let on_disk =
            match tracedecay_usecases::response_handles::inventory_response_handles(project_root) {
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
pub fn store_response_handle(
    project_root: &Path,
    content: &str,
    now: i64,
) -> Result<ResponseHandleRecord> {
    let started = Instant::now();
    let caller = std::panic::Location::caller();
    let telemetry = telemetry();
    telemetry.store_attempts.fetch_add(1, Ordering::Relaxed);
    let result =
        tracedecay_usecases::response_handles::store_response_handle(project_root, content, now);
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
    let result =
        tracedecay_usecases::response_handles::retrieve_response_handle(project_root, handle, now);
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
pub fn cleanup_expired_response_handles(project_root: &Path, now: i64) -> Result<usize> {
    let started = Instant::now();
    let caller = std::panic::Location::caller();
    let telemetry = telemetry();
    telemetry.cleanup_runs.fetch_add(1, Ordering::Relaxed);
    let result =
        tracedecay_usecases::response_handles::cleanup_expired_response_handles(project_root, now);
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
    result.map(|cleanup| cleanup.removed_expired)
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
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

fn error_class(error: &TraceDecayError) -> &'static str {
    match error {
        TraceDecayError::ResetRequired { .. } => "reset_required",
        TraceDecayError::File { .. } => "file",
        TraceDecayError::Parse { .. } => "parse",
        TraceDecayError::Database { .. } => "database",
        TraceDecayError::Search { .. } => "search",
        TraceDecayError::Config { .. } => "config",
        TraceDecayError::ProfileResetRequired { .. } => "profile_reset_required",
        TraceDecayError::ProjectRoute { .. } => "project_route",
        TraceDecayError::SyncLock { .. } => "sync_lock",
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

/// Serializes lib unit tests that store response handles under the
/// process-global profile root (`TRACEDECAY_DATA_DIR`). Uses the shared
/// user-data-dir test lock so env mutation cannot race profile resolution.
#[cfg(test)]
pub(crate) fn lock_response_handle_store() -> std::sync::MutexGuard<'static, ()> {
    crate::config::lock_user_data_dir_test_env()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_route_error_class_is_distinct() {
        let error = TraceDecayError::project_route(
            "project_route_unavailable",
            true,
            "project registry is warming",
        );

        assert_eq!(error_class(&error), "project_route");
    }

    #[test]
    fn reset_required_error_class_is_distinct() {
        let error =
            TraceDecayError::reset_required("session store", "session store reset required");

        assert_eq!(error_class(&error), "reset_required");
    }

    #[test]
    fn public_inventory_problem_never_exposes_the_local_path() {
        let error = TraceDecayError::File {
            message: "corrupt response-handle record: invalid JSON".to_string(),
            path: "/private/operator/cache/secret.json".to_string(),
        };

        let (reason_code, detail) = public_inventory_problem(&error);

        assert_eq!(reason_code, "corrupt_handle_record");
        assert!(!detail.contains("/private/operator"));

        let sanitized = public_retrieve_error(error);
        assert!(matches!(
            sanitized,
            TraceDecayError::File { message, path }
                if message.starts_with("corrupt response-handle record:")
                    && !message.contains("invalid JSON")
                    && path == "response-handles"
        ));
    }
}
