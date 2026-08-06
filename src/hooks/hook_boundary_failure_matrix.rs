//! hook-boundary failure matrix for host-hook telemetry dispositions.
//!
//! Covers sticky failure aggregation, timeout stickiness, daemon-unavailable
//! transport mapping, cancel/backpressure stickiness, Unknown↔typed order
//! permutations (later typed replaces Unknown; typed/timeout/cancel stick),
//! and rejection of default-success when no typed admission status is observed.
//! These rows do not open a project observation writer or advance a durable
//! frontier.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::analytics::{HOOK_ANALYTICS_FILENAME, record_hook_invoked};
use super::tool_hints::HintAgent;
use super::{TestDaemonHookActionGuard, daemon_hook_action, lock_test_env};
use crate::config::USER_DATA_DIR_ENV;

struct EnvGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set_path(key: &'static str, value: &Path) -> Self {
        let previous = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

fn enroll_project(project_root: &Path, project_id: &str) -> PathBuf {
    crate::storage::write_enrollment_marker(
        project_root,
        &crate::storage::EnrollmentMarker {
            project_id: project_id.to_string(),
            storage_mode: crate::storage::StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let layout = crate::storage::resolve_layout_for_current_profile(project_root).unwrap();
    std::fs::create_dir_all(&layout.data_root).unwrap();
    // Hook telemetry is fail-closed: the timing span only records a
    // `hook_completed` row once a runtime configuration snapshot is published.
    // Bootstrap the default snapshot so these disposition rows are observable.
    crate::config::bootstrap_runtime_configuration(project_root, &layout)
        .expect("publish hook test runtime configuration");
    layout.data_root
}

fn read_analytics_rows(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn completed_row<'a>(rows: &'a [Value], hook_name: &str) -> &'a Value {
    rows.iter()
        .find(|row| row["event"] == "hook_completed" && row["hook_name"] == hook_name)
        .unwrap_or_else(|| panic!("missing hook_completed row for {hook_name}"))
}

#[test]
fn matrix_rejects_default_success_when_disposition_absent() {
    let _lock = lock_test_env();
    let project = tempfile::tempdir().unwrap();
    let profile = tempfile::tempdir().unwrap();
    let project_root = project.path().canonicalize().unwrap();
    let profile_root = profile.path().canonicalize().unwrap();
    let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
    let data_root = enroll_project(&project_root, "proj_hook_matrix_default");

    {
        let _span = record_hook_invoked(
            Some(&project_root),
            HintAgent::Claude,
            "noDisposition",
            "{}",
        );
    }

    let rows = read_analytics_rows(&data_root.join(HOOK_ANALYTICS_FILENAME));
    let row = completed_row(&rows, "noDisposition");
    assert_ne!(row["disposition"]["status"], "supported");
    assert_eq!(row["disposition"]["status"], "unknown");
    assert_eq!(row["disposition"]["reason_code"], "disposition_absent");
    assert_eq!(row["disposition"]["class"], "unknown");
}

#[test]
fn matrix_rejects_default_success_for_untyped_ok_then_keeps_later_typed_failure() {
    let _lock = lock_test_env();
    let project = tempfile::tempdir().unwrap();
    let profile = tempfile::tempdir().unwrap();
    let project_root = project.path().canonicalize().unwrap();
    let profile_root = profile.path().canonicalize().unwrap();
    let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
    let data_root = enroll_project(&project_root, "proj_hook_matrix_untyped");

    let untyped_ok = Ok(serde_json::json!({ "reset": true }));
    let failure = Ok(serde_json::json!({
        "admission": {
            "status": "unavailable",
            "retryable": true,
            "reason_code": "daemon_unavailable"
        }
    }));
    {
        let span = record_hook_invoked(
            Some(&project_root),
            HintAgent::Codex,
            "untypedThenFailure",
            "{}",
        );
        span.note_completed_daemon_call(Some(10), 1, &untyped_ok);
        span.note_completed_daemon_call(Some(10), 1, &failure);
    }

    let rows = read_analytics_rows(&data_root.join(HOOK_ANALYTICS_FILENAME));
    let row = completed_row(&rows, "untypedThenFailure");
    assert_eq!(row["disposition"]["status"], "unavailable");
    assert_eq!(row["disposition"]["reason_code"], "daemon_unavailable");
    assert_ne!(row["disposition"]["status"], "supported");
}

#[test]
fn matrix_sticky_failure_survives_later_success_for_unavailable_cancel_backpressure_timeout() {
    let _lock = lock_test_env();
    let project = tempfile::tempdir().unwrap();
    let profile = tempfile::tempdir().unwrap();
    let project_root = project.path().canonicalize().unwrap();
    let profile_root = profile.path().canonicalize().unwrap();
    let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
    let data_root = enroll_project(&project_root, "proj_hook_matrix_sticky");

    let success = Ok(serde_json::json!({
        "admission": { "status": "supported", "retryable": false }
    }));
    let unavailable = Ok(serde_json::json!({
        "admission": {
            "status": "unavailable",
            "retryable": true,
            "reason_code": "daemon_unavailable"
        }
    }));
    let cancelled = Ok(serde_json::json!({
        "admission": {
            "status": "backpressured",
            "retryable": true,
            "reason_code": "hook_cancelled"
        }
    }));
    let backpressure = Ok(serde_json::json!({
        "admission": {
            "status": "backpressured",
            "retryable": true,
            "reason_code": "spool_overflow"
        }
    }));

    {
        let span = record_hook_invoked(
            Some(&project_root),
            HintAgent::Claude,
            "unavailableThenSuccess",
            "{}",
        );
        span.note_completed_daemon_call(Some(1), 1, &unavailable);
        span.note_completed_daemon_call(Some(1), 1, &success);
    }
    {
        let span = record_hook_invoked(
            Some(&project_root),
            HintAgent::Kiro,
            "cancelThenSuccess",
            "{}",
        );
        span.note_completed_daemon_call(Some(1), 1, &cancelled);
        span.note_completed_daemon_call(Some(1), 1, &success);
    }
    {
        let span = record_hook_invoked(
            Some(&project_root),
            HintAgent::Codex,
            "backpressureThenSuccess",
            "{}",
        );
        span.note_completed_daemon_call(Some(1), 1, &backpressure);
        span.note_completed_daemon_call(Some(1), 1, &success);
    }
    {
        let span = record_hook_invoked(
            Some(&project_root),
            HintAgent::Cursor,
            "timeoutThenSuccess",
            "{}",
        );
        span.note_timed_out(true);
        span.note_timed_out(false);
        span.note_daemon_result(&success);
    }

    let rows = read_analytics_rows(&data_root.join(HOOK_ANALYTICS_FILENAME));
    let unavailable_row = completed_row(&rows, "unavailableThenSuccess");
    assert_eq!(unavailable_row["disposition"]["status"], "unavailable");
    assert_eq!(unavailable_row["disposition"]["class"], "transport");

    let cancel_row = completed_row(&rows, "cancelThenSuccess");
    assert_eq!(cancel_row["disposition"]["status"], "backpressured");
    assert_eq!(cancel_row["disposition"]["class"], "cancellation");
    assert_eq!(cancel_row["disposition"]["reason_code"], "hook_cancelled");

    let backpressure_row = completed_row(&rows, "backpressureThenSuccess");
    assert_eq!(backpressure_row["disposition"]["status"], "backpressured");
    assert_eq!(
        backpressure_row["disposition"]["reason_code"],
        "spool_overflow"
    );

    let timeout_row = completed_row(&rows, "timeoutThenSuccess");
    assert_eq!(timeout_row["timeout"]["timed_out"], true);
    assert_eq!(timeout_row["disposition"]["class"], "timeout");
    assert_ne!(timeout_row["disposition"]["status"], "supported");
}

#[test]
fn matrix_unknown_order_permutations_later_typed_replaces_unknown() {
    let _lock = lock_test_env();
    let project = tempfile::tempdir().unwrap();
    let profile = tempfile::tempdir().unwrap();
    let project_root = project.path().canonicalize().unwrap();
    let profile_root = profile.path().canonicalize().unwrap();
    let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
    let data_root = enroll_project(&project_root, "proj_hook_matrix_unknown_order");

    let success = Ok(serde_json::json!({
        "admission": { "status": "supported", "retryable": false }
    }));
    let unavailable = Ok(serde_json::json!({
        "admission": {
            "status": "unavailable",
            "retryable": true,
            "reason_code": "daemon_unavailable"
        }
    }));
    let cancelled = Ok(serde_json::json!({
        "admission": {
            "status": "backpressured",
            "retryable": true,
            "reason_code": "hook_cancelled"
        }
    }));

    {
        let span = record_hook_invoked(
            Some(&project_root),
            HintAgent::Claude,
            "unknownThenSuccess",
            "{}",
        );
        span.note_completed_daemon_notification(Some(1));
        span.note_completed_daemon_call(Some(1), 1, &success);
    }
    {
        let span = record_hook_invoked(
            Some(&project_root),
            HintAgent::Codex,
            "unknownThenFailure",
            "{}",
        );
        span.note_completed_daemon_notification(Some(1));
        span.note_completed_daemon_call(Some(1), 1, &unavailable);
    }
    {
        let span = record_hook_invoked(
            Some(&project_root),
            HintAgent::Kiro,
            "successThenUnknown",
            "{}",
        );
        span.note_completed_daemon_call(Some(1), 1, &success);
        span.note_completed_daemon_notification(Some(1));
    }
    {
        let span = record_hook_invoked(
            Some(&project_root),
            HintAgent::Cursor,
            "timeoutThenUnknown",
            "{}",
        );
        span.note_timed_out(true);
        span.note_completed_daemon_notification(Some(1));
    }
    {
        let span = record_hook_invoked(
            Some(&project_root),
            HintAgent::Claude,
            "cancelThenUnknown",
            "{}",
        );
        span.note_completed_daemon_call(Some(1), 1, &cancelled);
        span.note_completed_daemon_notification(Some(1));
    }

    let rows = read_analytics_rows(&data_root.join(HOOK_ANALYTICS_FILENAME));
    let unknown_then_success = completed_row(&rows, "unknownThenSuccess");
    assert_eq!(unknown_then_success["disposition"]["status"], "supported");
    assert_eq!(unknown_then_success["disposition"]["class"], "application");

    let unknown_then_failure = completed_row(&rows, "unknownThenFailure");
    assert_eq!(unknown_then_failure["disposition"]["status"], "unavailable");
    assert_eq!(unknown_then_failure["disposition"]["class"], "transport");

    let success_then_unknown = completed_row(&rows, "successThenUnknown");
    assert_eq!(success_then_unknown["disposition"]["status"], "supported");
    assert_eq!(success_then_unknown["disposition"]["class"], "application");

    let timeout_then_unknown = completed_row(&rows, "timeoutThenUnknown");
    assert_eq!(timeout_then_unknown["disposition"]["class"], "timeout");

    let cancel_then_unknown = completed_row(&rows, "cancelThenUnknown");
    assert_eq!(cancel_then_unknown["disposition"]["class"], "cancellation");
    assert_eq!(
        cancel_then_unknown["disposition"]["reason_code"],
        "hook_cancelled"
    );
}

#[test]
fn matrix_daemon_unavailable_transport_does_not_invent_success() {
    super::run_with_test_env_lock(async {
        let project = tempfile::tempdir().unwrap();
        let profile = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        let profile_root = profile.path().canonicalize().unwrap();
        let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
        let data_root = enroll_project(&project_root, "proj_hook_matrix_transport");

        // An installed empty responder deterministically returns Config/error from
        // daemon_hook_action. It cannot fall through to a live daemon socket.
        {
            let guard = TestDaemonHookActionGuard::install(std::iter::empty());
            let span =
                record_hook_invoked(Some(&project_root), HintAgent::Cursor, "daemonDown", "{}");
            let result = daemon_hook_action(
                Some(&project_root),
                serde_json::json!({ "action": "reset_counter" }),
                Some(&span),
            )
            .await;
            assert!(
                result.is_err(),
                "expected daemon-unavailable transport error"
            );
            assert_eq!(guard.calls().len(), 1);
        }

        let rows = read_analytics_rows(&data_root.join(HOOK_ANALYTICS_FILENAME));
        let row = completed_row(&rows, "daemonDown");
        assert_eq!(row["disposition"]["status"], "unavailable");
        assert_eq!(row["disposition"]["class"], "transport");
        assert_ne!(row["disposition"]["status"], "supported");
    });
}
