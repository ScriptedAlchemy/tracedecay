use super::*;
use crate::config::USER_DATA_DIR_ENV;
use std::ffi::OsString;
use std::time::Duration;

struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
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

#[test]
fn unbound_hook_analytics_do_not_create_a_missing_profile() {
    let _lock = crate::hooks::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let profile_root = home.path().join(".tracedecay");
    let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
    assert!(!profile_root.exists());

    let event = r#"{"hook_event_name":"Stop","session_id":"s1"}"#;
    let parsed = serde_json::from_str(event).unwrap();
    drop(record_hook_invoked_parsed(
        None,
        HintAgent::Claude,
        "Stop",
        event,
        &parsed,
    ));

    assert!(
        !profile_root.exists(),
        "unbound hook analytics created a missing profile"
    );
}

#[test]
fn telemetry_contract_is_canonical_and_bounded() {
    let contract = host_hook_telemetry_contract();
    assert_eq!(
        contract["schema_version"],
        HOST_HOOK_TELEMETRY_SCHEMA_VERSION
    );
    assert_eq!(contract["provider_coverage"].as_array().unwrap().len(), 5);
    assert_eq!(
        contract["metrics"]["timeout"],
        serde_json::json!(["timeout.budget_ms", "timeout.timed_out"])
    );
}

#[test]
fn disposition_classifier_distinguishes_outcomes() {
    assert_eq!(
        HookDispositionTelemetry::timeout("hook_timeout").class,
        HookDispositionClass::Timeout
    );
    assert_eq!(
        HookDispositionTelemetry::daemon_unavailable().class,
        HookDispositionClass::Transport
    );
    assert_eq!(
        HookDispositionTelemetry::from_parts(
            HostAdmissionStatus::Backpressured,
            Some(false),
            Some("hook_cancelled".to_owned()),
        )
        .class,
        HookDispositionClass::Cancellation
    );
    assert_eq!(
        HookDispositionTelemetry::unknown("unknown_provider").class,
        HookDispositionClass::Unknown
    );
    let typed_success = HookDispositionTelemetry::from_daemon_wire(&serde_json::json!({
        "status": "supported",
        "retryable": false
    }))
    .expect("typed success");
    assert_eq!(typed_success.status, HostAdmissionStatus::Supported);
    let untrusted = HookDispositionTelemetry::from_daemon_wire(&serde_json::json!({
        "status": "degraded",
        "retryable": false,
        "reason_code": "private reasoning content"
    }))
    .unwrap();
    assert_eq!(untrusted.reason_code.as_deref(), Some("unclassified"));
}

#[test]
fn native_dispatch_dispositions_remain_distinct_in_telemetry() {
    let accepted = disposition_from_native_dispatch(HookTransportDispositionV1::Accepted);
    let spooled = disposition_from_native_dispatch(HookTransportDispositionV1::AcceptedForReplay);
    let catchup = disposition_from_native_dispatch(HookTransportDispositionV1::CatchupRequired);

    assert_eq!(accepted.status, HostAdmissionStatus::Supported);
    assert_eq!(accepted.reason_code.as_deref(), Some("hook_v2_accepted"));
    assert_eq!(spooled.status, HostAdmissionStatus::AcceptedForReplay);
    assert_eq!(spooled.reason_code.as_deref(), Some("hook_v2_spooled"));
    assert_eq!(catchup.status, HostAdmissionStatus::Degraded);
    assert_eq!(
        catchup.reason_code.as_deref(),
        Some("hook_v2_catchup_required")
    );
}

/// A hook subprocess never has a published snapshot — nothing in the hook
/// path opens a store or contacts the daemon before the span is built — so
/// treating absence as "timings off" suppressed `hook_completed` for every
/// real hook while `hook_invoked` kept being written. Absence must behave
/// like the invocation half; only an authority that says off turns it off.
#[test]
fn timing_span_follows_the_published_snapshot_and_defaults_to_recording() {
    let project = tempfile::tempdir().unwrap();
    let project_root = project.path().canonicalize().unwrap();
    let span = HookTimingSpan::new(
        Some(&project_root),
        HintAgent::Claude,
        "missingConfiguration",
        None,
        None,
    );
    assert!(
        span.enabled,
        "no published snapshot must not silence the completion half of the span"
    );

    publish_telemetry_timings(&project_root, "project.hook-timings-disabled", false);
    let disabled = HookTimingSpan::new(
        Some(&project_root),
        HintAgent::Claude,
        "disabledConfiguration",
        None,
        None,
    );
    assert!(
        !disabled.enabled,
        "an authority that disables timings must disable the span"
    );

    publish_telemetry_timings(&project_root, "project.hook-timings-enabled", true);
    let enabled = HookTimingSpan::new(
        Some(&project_root),
        HintAgent::Claude,
        "enabledConfiguration",
        None,
        None,
    );
    assert!(
        enabled.enabled,
        "an authority that enables timings must enable the span"
    );
}

fn publish_telemetry_timings(project_root: &Path, project_id: &str, timings: bool) {
    use std::collections::BTreeMap;
    use tracedecay_domain::configuration::{
        ConfigurationLayerIdV1, ConfigurationRevisionId, ConfigurationValueV1, SettingKey,
        TELEMETRY_TIMINGS_SETTING_KEY,
    };

    let project_id = tracedecay_domain::ProjectId::new(project_id.to_owned()).unwrap();
    let revision_id =
        ConfigurationRevisionId::new(format!("revision.{project_id}.timings")).unwrap();
    let snapshot = crate::config::resolver::resolve_configuration(
        &crate::config::registry::ConfigurationRegistry::core().unwrap(),
        &[crate::config::resolver::ConfigurationLayerV1 {
            layer: ConfigurationLayerIdV1::Project {
                project_id: project_id.clone(),
            },
            revision_id: revision_id.clone(),
            entries: BTreeMap::from([(
                SettingKey::new(TELEMETRY_TIMINGS_SETTING_KEY).unwrap(),
                ConfigurationValueV1::Boolean(timings),
            )]),
        }],
    )
    .unwrap()
    .snapshot;
    crate::config::install_pinned_runtime_configuration(
        crate::config::PinnedRuntimeConfiguration::new(
            crate::config::RuntimeConfigurationTarget {
                project_id,
                project_root: project_root.to_path_buf(),
            },
            revision_id,
            snapshot,
        )
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn payload_bytes_are_length_only_and_omit_forbidden_content() {
    let secret_prompt = "benchmark-secret-prompt-text";
    let secret_tool = "benchmark-secret-tool-payload";
    let secret_cred = "benchmark-secret-credential";
    let secret_path = "/home/private/secret-project";
    let secret_reason = "benchmark-secret-reasoning";
    let secret_command = "benchmark-secret-command";
    let event = format!(
        r#"{{"hook_event_name":"Stop","session_id":"s1","cwd":"{secret_path}","command":"{secret_command}","prompt_text":"{secret_prompt}","tool_payload":"{secret_tool}","credentials":"{secret_cred}","private_path":"{secret_path}","reasoning_text":"{secret_reason}"}}"#
    );
    let measured = measure_host_event_payload_bytes(&event).unwrap();
    assert_eq!(measured, event.len() as u64);

    let _lock = crate::hooks::lock_test_env();
    let project = tempfile::Builder::new()
        .prefix("benchmark-secret-project-path-")
        .tempdir()
        .unwrap();
    let profile = tempfile::tempdir().unwrap();
    let project_root = project.path().canonicalize().unwrap();
    let profile_root = profile.path().canonicalize().unwrap();
    let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
    let data_root = enroll_project(&project_root, "proj_hook_privacy");

    {
        let span = record_hook_invoked(Some(&project_root), HintAgent::Claude, "Stop", &event);
        span.note_timeout_budget(Duration::from_millis(750));
        span.note_timed_out(false);
        span.note_completed_daemon_call(
            Some(34),
            12,
            &Ok(serde_json::json!({
                "admission": { "status": "supported", "retryable": false }
            })),
        );
        std::thread::sleep(Duration::from_millis(2));
    }

    let rows = read_analytics_rows(&data_root.join(HOOK_ANALYTICS_FILENAME));
    let completed = rows
        .iter()
        .find(|row| row["event"] == "hook_completed")
        .expect("hook_completed");
    let analytics_jsonl = std::fs::read_to_string(data_root.join(HOOK_ANALYTICS_FILENAME)).unwrap();
    assert_eq!(completed["payload_bytes"], measured);
    assert_eq!(completed["daemon_rtt_us"], 12);
    assert_eq!(completed["daemon_ipc_payload_bytes"], 34);
    assert_eq!(completed["timeout"]["budget_ms"], 750);
    assert_eq!(completed["timeout"]["timed_out"], false);
    assert_eq!(completed["disposition"]["class"], "application");
    assert!(completed["hook_wall_time_us"].as_u64().unwrap() >= 1_000);
    assert_eq!(completed["coverage"], "host_measured");
    assert_eq!(
        completed["schema_version"],
        HOST_HOOK_TELEMETRY_SCHEMA_VERSION
    );
    for forbidden in [
        secret_prompt,
        secret_tool,
        secret_cred,
        secret_path,
        secret_reason,
        secret_command,
        "prompt_text",
        "tool_payload",
        "credentials",
        "private_path",
        "reasoning_text",
        "benchmark-secret-",
    ] {
        assert!(
            !analytics_jsonl.contains(forbidden),
            "analytics leaked forbidden content `{forbidden}`: {analytics_jsonl}"
        );
    }
    for forbidden_field in [
        "project_root",
        "event_cwd",
        "command",
        "session_id",
        "tool_name",
    ] {
        assert!(
            !analytics_jsonl.contains(&format!("\"{forbidden_field}\"")),
            "telemetry persisted forbidden field `{forbidden_field}`: {analytics_jsonl}"
        );
    }
}

#[test]
fn daemon_hook_action_records_completed_rtt_and_wire_length() {
    crate::hooks::run_with_test_env_lock(async {
        let project = tempfile::tempdir().unwrap();
        let profile = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        let profile_root = profile.path().canonicalize().unwrap();
        let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
        let data_root = enroll_project(&project_root, "proj_hook_daemon_boundary");

        {
            let _guard = crate::hooks::TestDaemonHookActionGuard::install([serde_json::json!({
                "admission": { "status": "committed", "retryable": false },
                "reset": true,
            })]);
            let span = record_hook_invoked(
                Some(&project_root),
                HintAgent::Cursor,
                "daemonBoundary",
                r#"{"hook_event_name":"daemonBoundary"}"#,
            );
            let result = crate::hooks::daemon_hook_action(
                Some(&project_root),
                serde_json::json!({ "action": "reset_counter" }),
                Some(&span),
            )
            .await;
            assert!(result.is_ok());
        }

        let rows = read_analytics_rows(&data_root.join(HOOK_ANALYTICS_FILENAME));
        let completed = rows
            .iter()
            .find(|row| row["event"] == "hook_completed" && row["hook_name"] == "daemonBoundary")
            .expect("daemon boundary completed row");
        assert!(completed["daemon_rtt_us"].as_u64().is_some());
        assert_eq!(completed["daemon_call_count"], 1);
        assert!(completed["daemon_ipc_payload_bytes"].as_u64().unwrap() > 0);
        assert_eq!(completed["disposition"]["status"], "committed");
        assert_eq!(completed["disposition"]["class"], "application");
    });
}

#[test]
fn one_way_notification_does_not_claim_round_trip_time() {
    let _lock = crate::hooks::lock_test_env();
    let project = tempfile::tempdir().unwrap();
    let profile = tempfile::tempdir().unwrap();
    let project_root = project.path().canonicalize().unwrap();
    let profile_root = profile.path().canonicalize().unwrap();
    let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
    let data_root = enroll_project(&project_root, "proj_hook_notification_boundary");

    {
        let span = record_hook_invoked(
            Some(&project_root),
            HintAgent::Cursor,
            "notificationBoundary",
            r#"{"hook_event_name":"notificationBoundary"}"#,
        );
        span.note_completed_daemon_notification(Some(37));
    }

    let rows = read_analytics_rows(&data_root.join(HOOK_ANALYTICS_FILENAME));
    let completed = rows
        .iter()
        .find(|row| row["event"] == "hook_completed" && row["hook_name"] == "notificationBoundary")
        .expect("notification boundary completed row");
    assert!(completed["daemon_rtt_us"].is_null());
    assert_eq!(completed["daemon_call_count"], 0);
    assert_eq!(completed["daemon_ipc_payload_bytes"], 37);
    assert_eq!(completed["disposition"]["status"], "unknown");
    assert_eq!(
        completed["disposition"]["reason_code"],
        "notify_outcome_unavailable"
    );
}

#[test]
fn hook_disposition_aggregation_preserves_failures_and_sticky_timeout() {
    let _lock = crate::hooks::lock_test_env();
    let project = tempfile::tempdir().unwrap();
    let profile = tempfile::tempdir().unwrap();
    let project_root = project.path().canonicalize().unwrap();
    let profile_root = profile.path().canonicalize().unwrap();
    let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
    let data_root = enroll_project(&project_root, "proj_hook_aggregation");
    let success = Ok(serde_json::json!({
        "admission": { "status": "supported", "retryable": false }
    }));
    let failure = Ok(serde_json::json!({
        "admission": {
            "status": "unavailable",
            "retryable": true,
            "reason_code": "daemon_unavailable"
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
            "failureThenSuccess",
            "{}",
        );
        span.note_completed_daemon_call(Some(100), 10, &failure);
        span.note_completed_daemon_call(Some(200), 20, &success);
    }
    {
        let span = record_hook_invoked(
            Some(&project_root),
            HintAgent::Codex,
            "successThenFailure",
            "{}",
        );
        span.note_completed_daemon_call(Some(200), 20, &success);
        span.note_completed_daemon_call(Some(100), 10, &failure);
    }
    {
        let span = record_hook_invoked(
            Some(&project_root),
            HintAgent::Kiro,
            "backpressureThenSuccess",
            "{}",
        );
        span.note_completed_daemon_call(None, 1, &backpressure);
        span.note_completed_daemon_call(None, 1, &success);
    }
    {
        let span = record_hook_invoked(
            Some(&project_root),
            HintAgent::Cursor,
            "stickyTimeout",
            "{}",
        );
        span.note_timed_out(true);
        span.note_timed_out(false);
        span.note_daemon_result(&success);
    }

    let rows = read_analytics_rows(&data_root.join(HOOK_ANALYTICS_FILENAME));
    for hook_name in ["failureThenSuccess", "successThenFailure"] {
        let row = rows
            .iter()
            .find(|row| row["event"] == "hook_completed" && row["hook_name"] == hook_name)
            .unwrap_or_else(|| panic!("missing completed row for {hook_name}"));
        assert_eq!(row["disposition"]["status"], "unavailable");
        assert_eq!(row["disposition"]["class"], "transport");
        assert_eq!(row["daemon_call_count"], 2);
        assert_eq!(row["daemon_rtt_us"], 30);
        assert_eq!(row["daemon_ipc_payload_bytes"], 300);
    }
    let backpressure_row = rows
        .iter()
        .find(|row| {
            row["event"] == "hook_completed" && row["hook_name"] == "backpressureThenSuccess"
        })
        .expect("backpressure completed row");
    assert_eq!(backpressure_row["disposition"]["status"], "backpressured");
    assert_eq!(backpressure_row["daemon_call_count"], 2);

    let timeout_row = rows
        .iter()
        .find(|row| row["event"] == "hook_completed" && row["hook_name"] == "stickyTimeout")
        .expect("sticky timeout completed row");
    assert_eq!(timeout_row["timeout"]["timed_out"], true);
    assert_eq!(timeout_row["disposition"]["class"], "timeout");
}

#[test]
fn hook_disposition_order_permutations_unknown_typed_timeout_cancel() {
    let _lock = crate::hooks::lock_test_env();
    let project = tempfile::tempdir().unwrap();
    let profile = tempfile::tempdir().unwrap();
    let project_root = project.path().canonicalize().unwrap();
    let profile_root = profile.path().canonicalize().unwrap();
    let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
    let data_root = enroll_project(&project_root, "proj_hook_unknown_order");
    let success = Ok(serde_json::json!({
        "admission": { "status": "supported", "retryable": false }
    }));
    let failure = Ok(serde_json::json!({
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
        span.note_completed_daemon_call(Some(10), 1, &success);
    }
    {
        let span = record_hook_invoked(
            Some(&project_root),
            HintAgent::Codex,
            "unknownThenFailure",
            "{}",
        );
        span.note_completed_daemon_notification(Some(1));
        span.note_completed_daemon_call(Some(10), 1, &failure);
    }
    {
        let span = record_hook_invoked(
            Some(&project_root),
            HintAgent::Kiro,
            "successThenUnknown",
            "{}",
        );
        span.note_completed_daemon_call(Some(10), 1, &success);
        span.note_completed_daemon_notification(Some(1));
    }
    {
        let span = record_hook_invoked(
            Some(&project_root),
            HintAgent::Cursor,
            "failureThenUnknown",
            "{}",
        );
        span.note_completed_daemon_call(Some(10), 1, &failure);
        span.note_completed_daemon_notification(Some(1));
    }
    {
        let span = record_hook_invoked(
            Some(&project_root),
            HintAgent::Claude,
            "unknownThenTimeout",
            "{}",
        );
        span.note_completed_daemon_notification(Some(1));
        span.note_timed_out(true);
    }
    {
        let span = record_hook_invoked(
            Some(&project_root),
            HintAgent::Codex,
            "timeoutThenUnknown",
            "{}",
        );
        span.note_timed_out(true);
        span.note_completed_daemon_notification(Some(1));
    }
    {
        let span = record_hook_invoked(
            Some(&project_root),
            HintAgent::Kiro,
            "unknownThenCancel",
            "{}",
        );
        span.note_completed_daemon_notification(Some(1));
        span.note_completed_daemon_call(Some(10), 1, &cancelled);
    }
    {
        let span = record_hook_invoked(
            Some(&project_root),
            HintAgent::Cursor,
            "cancelThenUnknown",
            "{}",
        );
        span.note_completed_daemon_call(Some(10), 1, &cancelled);
        span.note_completed_daemon_notification(Some(1));
    }

    let rows = read_analytics_rows(&data_root.join(HOOK_ANALYTICS_FILENAME));
    let row = |name: &str| {
        rows.iter()
            .find(|row| row["event"] == "hook_completed" && row["hook_name"] == name)
            .unwrap_or_else(|| panic!("missing completed row for {name}"))
    };

    assert_eq!(
        row("unknownThenSuccess")["disposition"]["status"],
        "supported"
    );
    assert_eq!(
        row("unknownThenSuccess")["disposition"]["class"],
        "application"
    );
    assert_eq!(
        row("unknownThenFailure")["disposition"]["status"],
        "unavailable"
    );
    assert_eq!(
        row("unknownThenFailure")["disposition"]["class"],
        "transport"
    );
    assert_eq!(
        row("successThenUnknown")["disposition"]["status"],
        "supported"
    );
    assert_eq!(
        row("successThenUnknown")["disposition"]["class"],
        "application"
    );
    assert_eq!(
        row("failureThenUnknown")["disposition"]["status"],
        "unavailable"
    );
    assert_eq!(
        row("failureThenUnknown")["disposition"]["class"],
        "transport"
    );
    assert_eq!(row("unknownThenTimeout")["disposition"]["class"], "timeout");
    assert_eq!(row("timeoutThenUnknown")["disposition"]["class"], "timeout");
    assert_eq!(
        row("unknownThenCancel")["disposition"]["class"],
        "cancellation"
    );
    assert_eq!(
        row("unknownThenCancel")["disposition"]["reason_code"],
        "hook_cancelled"
    );
    assert_eq!(
        row("cancelThenUnknown")["disposition"]["class"],
        "cancellation"
    );
    assert_eq!(
        row("cancelThenUnknown")["disposition"]["reason_code"],
        "hook_cancelled"
    );
}

#[test]
fn concurrent_spans_keep_rtt_payload_and_disposition_isolated() {
    crate::hooks::run_with_test_env_lock(async {
        let project = tempfile::tempdir().unwrap();
        let profile = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        let profile_root = profile.path().canonicalize().unwrap();
        let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
        let data_root = enroll_project(&project_root, "proj_hook_concurrent");
        let first = record_hook_invoked(
            Some(&project_root),
            HintAgent::Cursor,
            "firstHook",
            r#"{"hook_event_name":"firstHook"}"#,
        );
        let second = record_hook_invoked(
            Some(&project_root),
            HintAgent::Kiro,
            "secondHook",
            r#"{"hook_event_name":"secondHook"}"#,
        );
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let first_barrier = std::sync::Arc::clone(&barrier);
        let first_task = tokio::spawn(async move {
            first_barrier.wait().await;
            tokio::task::yield_now().await;
            first.note_completed_daemon_call(
                Some(101),
                11,
                &Ok(serde_json::json!({
                    "admission": { "status": "supported", "retryable": false }
                })),
            );
        });
        let second_task = tokio::spawn(async move {
            barrier.wait().await;
            second.note_completed_daemon_call(
                Some(202),
                22,
                &Ok(serde_json::json!({
                    "admission": {
                        "status": "unavailable",
                        "retryable": true,
                        "reason_code": "daemon_unavailable"
                    }
                })),
            );
        });
        first_task.await.unwrap();
        second_task.await.unwrap();

        let rows = read_analytics_rows(&data_root.join(HOOK_ANALYTICS_FILENAME));
        let first_row = rows
            .iter()
            .find(|row| row["event"] == "hook_completed" && row["hook_name"] == "firstHook")
            .expect("first completed row");
        let second_row = rows
            .iter()
            .find(|row| row["event"] == "hook_completed" && row["hook_name"] == "secondHook")
            .expect("second completed row");
        assert_eq!(first_row["daemon_rtt_us"], 11);
        assert_eq!(first_row["daemon_ipc_payload_bytes"], 101);
        assert_eq!(first_row["disposition"]["class"], "application");
        assert_eq!(second_row["daemon_rtt_us"], 22);
        assert_eq!(second_row["daemon_ipc_payload_bytes"], 202);
        assert_eq!(second_row["disposition"]["class"], "transport");
    });
}

#[test]
fn untyped_ok_daemon_output_emits_unknown_not_default_success() {
    let _lock = crate::hooks::lock_test_env();
    let project = tempfile::tempdir().unwrap();
    let profile = tempfile::tempdir().unwrap();
    let project_root = project.path().canonicalize().unwrap();
    let profile_root = profile.path().canonicalize().unwrap();
    let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
    let data_root = enroll_project(&project_root, "proj_untyped_ok_disposition");

    {
        let span = record_hook_invoked(Some(&project_root), HintAgent::Claude, "untypedOk", "{}");
        span.note_daemon_result(&Ok(serde_json::json!({"result": {}})));
    }

    let rows = read_analytics_rows(&data_root.join(HOOK_ANALYTICS_FILENAME));
    let completed = rows
        .iter()
        .find(|row| row["event"] == "hook_completed")
        .expect("hook_completed row");
    assert_eq!(completed["disposition"]["status"], "unknown");
    assert_eq!(completed["disposition"]["class"], "unknown");
    assert_eq!(
        completed["disposition"]["reason_code"],
        "disposition_absent"
    );
    assert_ne!(completed["disposition"]["status"], "supported");
}

struct CompletedSample<'a> {
    agent: &'a str,
    hook_name: &'a str,
    wall_us: Option<u64>,
    rtt_us: Option<u64>,
    payload_bytes: Option<u64>,
    daemon_ipc_bytes: Option<u64>,
    timed_out: Option<bool>,
    budget_ms: Option<u64>,
    disposition: Option<Value>,
}

fn sample_completed(sample: CompletedSample<'_>) -> Value {
    let CompletedSample {
        agent,
        hook_name,
        wall_us,
        rtt_us,
        payload_bytes,
        daemon_ipc_bytes,
        timed_out,
        budget_ms,
        disposition,
    } = sample;
    let mut row = serde_json::json!({
        "event": "hook_completed",
        "agent": agent,
        "hook_name": hook_name,
        "schema_version": 1,
    });
    if let Some(wall) = wall_us {
        row["hook_wall_time_us"] = Value::from(wall);
    }
    match rtt_us {
        Some(rtt) => row["daemon_rtt_us"] = Value::from(rtt),
        None => row["daemon_rtt_us"] = Value::Null,
    }
    match payload_bytes {
        Some(bytes) => row["payload_bytes"] = Value::from(bytes),
        None => row["payload_bytes"] = Value::Null,
    }
    match daemon_ipc_bytes {
        Some(bytes) => row["daemon_ipc_payload_bytes"] = Value::from(bytes),
        None => row["daemon_ipc_payload_bytes"] = Value::Null,
    }
    row["timeout"] = serde_json::json!({
        "budget_ms": budget_ms,
        "timed_out": timed_out,
    });
    if let Some(disposition) = disposition {
        row["disposition"] = disposition;
    }
    row
}

#[test]
fn readiness_aggregation_distinguishes_null_from_zero_and_rejects_default_success() {
    let rows = vec![
        sample_completed(CompletedSample {
            agent: "claude",
            hook_name: "PostToolUse",
            wall_us: Some(0),
            rtt_us: Some(0),
            payload_bytes: Some(0),
            daemon_ipc_bytes: Some(0),
            timed_out: Some(false),
            budget_ms: Some(50),
            disposition: Some(serde_json::json!({
                "status": "supported",
                "retryable": false,
                "class": "application"
            })),
        }),
        sample_completed(CompletedSample {
            agent: "claude",
            hook_name: "PostToolUse",
            wall_us: Some(12_000),
            rtt_us: None,
            payload_bytes: None,
            daemon_ipc_bytes: None,
            timed_out: None,
            budget_ms: None,
            disposition: None,
        }),
        serde_json::json!({"event": "hook_invoked", "agent": "claude"}),
    ];
    let aggregate = aggregate_hook_completed_readiness(&rows);
    assert_eq!(aggregate.collection_status, MetricAvailability::Measured);
    assert_eq!(aggregate.input_rows_received, 3);
    assert_eq!(aggregate.input_rows_processed, 3);
    assert_eq!(aggregate.input_rows_dropped_at_cap, 0);
    assert_eq!(aggregate.events_considered, 2);
    assert_eq!(aggregate.events_skipped_non_completed, 1);

    let wall = &aggregate.hook_wall_time_distribution[0].summary;
    assert_eq!(wall.availability, MetricAvailability::Measured);
    assert_eq!(wall.present_count, 2);
    assert_eq!(wall.absent_count, 0);
    assert_eq!(wall.min, Some(0));
    assert_eq!(wall.max, Some(12_000));

    let rtt = &aggregate.host_ipc_rtt_distribution[0].summary;
    assert_eq!(rtt.present_count, 1);
    assert_eq!(rtt.absent_count, 1);
    assert_eq!(rtt.min, Some(0));
    assert_ne!(rtt.availability, MetricAvailability::Unavailable);

    let payload = &aggregate.payload_bytes_distribution[0];
    assert_eq!(payload.host_event_payload_bytes.present_count, 1);
    assert_eq!(payload.host_event_payload_bytes.absent_count, 1);
    assert_eq!(payload.host_event_payload_bytes.min, Some(0));
    assert_eq!(payload.daemon_ipc_payload_bytes.present_count, 1);
    assert_eq!(payload.daemon_ipc_payload_bytes.absent_count, 1);

    let timeout = &aggregate.timeout_outcomes_by_host[0];
    assert_eq!(timeout.timed_out_true, 0);
    assert_eq!(timeout.timed_out_false, 1);
    assert_eq!(timeout.timed_out_unavailable, 1);
    assert_eq!(timeout.budget_ms_present, 1);
    assert_eq!(timeout.budget_ms_absent, 1);

    assert!(
        aggregate
            .disposition_counts_by_host
            .iter()
            .any(|row| row.class == HookDispositionClass::Unknown
                && row.status == HostAdmissionStatus::Unknown
                && row.retryable.is_none()
                && row.count == 1)
    );
    assert!(
        aggregate
            .disposition_counts_by_host
            .iter()
            .any(|row| row.class == HookDispositionClass::Application
                && row.status == HostAdmissionStatus::Supported
                && row.retryable == Some(false)
                && row.count == 1)
    );
    assert!(
        !aggregate
            .disposition_counts_by_host
            .iter()
            .any(|row| row.class == HookDispositionClass::Unknown
                && row.status == HostAdmissionStatus::Supported)
    );
}

#[test]
fn readiness_aggregation_preserves_sticky_failure_dispositions_and_timeouts() {
    let rows = vec![
        sample_completed(CompletedSample {
            agent: "codex",
            hook_name: "sessionStart",
            wall_us: Some(5_000),
            rtt_us: Some(900),
            payload_bytes: Some(128),
            daemon_ipc_bytes: Some(64),
            timed_out: Some(true),
            budget_ms: Some(10),
            disposition: Some(serde_json::json!({
                "status": "degraded",
                "retryable": true,
                "reason_code": "hook_timeout",
                "class": "timeout"
            })),
        }),
        sample_completed(CompletedSample {
            agent: "codex",
            hook_name: "sessionStart",
            wall_us: Some(4_000),
            rtt_us: Some(800),
            payload_bytes: Some(100),
            daemon_ipc_bytes: Some(50),
            timed_out: Some(false),
            budget_ms: Some(10),
            disposition: Some(serde_json::json!({
                "status": "unavailable",
                "retryable": true,
                "reason_code": "daemon_unavailable",
                "class": "transport"
            })),
        }),
    ];
    let aggregate = aggregate_hook_completed_readiness(&rows);
    let timeout = &aggregate.timeout_outcomes_by_host[0];
    assert_eq!(timeout.timed_out_true, 1);
    assert_eq!(timeout.timed_out_false, 1);
    assert_eq!(timeout.timed_out_unavailable, 0);
    assert!(
        aggregate
            .disposition_counts_by_host
            .iter()
            .any(|row| row.class == HookDispositionClass::Timeout
                && row.status == HostAdmissionStatus::Degraded
                && row.retryable == Some(true))
    );
    assert!(aggregate.disposition_counts_by_host.iter().any(|row| {
        row.class == HookDispositionClass::Transport
            && row.status == HostAdmissionStatus::Unavailable
            && row.retryable == Some(true)
    }));
    assert!(
        !aggregate
            .disposition_counts_by_host
            .iter()
            .any(|row| row.class == HookDispositionClass::Application
                && row.status == HostAdmissionStatus::Supported)
    );
}

#[test]
fn readiness_aggregation_is_bounded_and_privacy_safe() {
    const EXCESS_ROWS: usize = 123;
    let mut rows = Vec::with_capacity(MAX_READINESS_INPUT_ROWS + EXCESS_ROWS);
    for index in 0..(MAX_READINESS_INPUT_ROWS + EXCESS_ROWS) {
        let agent = match index % 7 {
            0 => "claude".to_string(),
            1 => "codex".to_string(),
            2 => "cursor".to_string(),
            3 => "hermes".to_string(),
            4 => "kiro".to_string(),
            _ => format!("untrusted_host_{index}"),
        };
        let (class, status) = if index % 2 == 0 {
            ("application".to_string(), "supported".to_string())
        } else {
            (
                format!("untrusted_class_{index}"),
                format!("untrusted_status_{index}"),
            )
        };
        rows.push(sample_completed(CompletedSample {
            agent: &agent,
            hook_name: &format!("hook{index}"),
            wall_us: Some(1_000),
            rtt_us: Some(100),
            payload_bytes: Some(32),
            daemon_ipc_bytes: Some(16),
            timed_out: Some(false),
            budget_ms: Some(20),
            disposition: Some(serde_json::json!({
                "status": status,
                "retryable": index % 3 == 0,
                "class": class,
                "reason_code": format!("reason_{index}")
            })),
        }));
    }
    // Put sensitive values inside the retained newest suffix so this remains
    // a real privacy assertion after the oldest prefix is dropped.
    rows[EXCESS_ROWS]["session_id"] = Value::from("sess-leak");
    rows[EXCESS_ROWS]["event_cwd"] = Value::from("/private/path/secret");
    rows[EXCESS_ROWS]["command"] = Value::from("cat /etc/passwd");
    rows[EXCESS_ROWS]["prompt"] = Value::from("user secret prompt text");
    rows[EXCESS_ROWS]["hook_name"] = Value::from("privateHookName");
    rows[EXCESS_ROWS]["disposition"]["reason_code"] = Value::from("private-reason");

    let aggregate = aggregate_hook_completed_readiness(&rows);
    assert_eq!(
        aggregate.input_rows_received,
        (MAX_READINESS_INPUT_ROWS + EXCESS_ROWS) as u64
    );
    assert_eq!(
        aggregate.input_rows_processed,
        MAX_READINESS_INPUT_ROWS as u64
    );
    assert_eq!(aggregate.input_rows_dropped_at_cap, EXCESS_ROWS as u64);
    assert_eq!(aggregate.events_considered, MAX_READINESS_INPUT_ROWS as u64);
    assert!(aggregate.rows_folded_to_other_host > 0);
    assert!(aggregate.disposition_values_folded_to_unknown > 0);
    assert!(aggregate.hook_wall_time_distribution.len() <= READINESS_HOST_BUCKETS);
    assert!(aggregate.host_ipc_rtt_distribution.len() <= READINESS_HOST_BUCKETS);
    assert!(aggregate.payload_bytes_distribution.len() <= READINESS_HOST_BUCKETS);
    assert!(aggregate.timeout_outcomes_by_host.len() <= READINESS_HOST_BUCKETS);
    assert!(aggregate.disposition_counts_by_host.len() <= MAX_DISPOSITION_SERIES);
    assert_eq!(
        aggregate
            .disposition_counts_by_host
            .iter()
            .map(|row| row.count)
            .sum::<u64>(),
        MAX_READINESS_INPUT_ROWS as u64
    );
    assert_eq!(
        aggregate.hook_wall_time_distribution[0]
            .summary
            .buckets
            .len(),
        LATENCY_BUCKET_UPPER_US.len()
    );
    assert!(
        aggregate
            .unavailable_metrics
            .iter()
            .any(
                |metric| metric.metric == "daemon_processing_duration_distribution"
                    && metric.status == MetricAvailability::Unavailable
            )
    );

    let encoded = serde_json::to_string(&aggregate).unwrap();
    for forbidden in [
        "sess-leak",
        "/private/path",
        "cat /etc/passwd",
        "user secret prompt",
        "reasoning_text",
        "privateHookName",
        "private-reason",
        "untrusted_host_",
        "untrusted_class_",
        "untrusted_status_",
        "hook_name",
        "reason_code",
    ] {
        assert!(
            !encoded.contains(forbidden),
            "aggregate leaked forbidden content: {forbidden}"
        );
    }
}

#[test]
fn readiness_aggregation_consumes_newest_bounded_suffix() {
    const EXCESS_ROWS: usize = 250;
    let total = MAX_READINESS_INPUT_ROWS + EXCESS_ROWS;
    let mut rows = Vec::with_capacity(total);
    for index in 0..total {
        // Ascending chronological order: oldest prefix carries wall=1, newest
        // suffix carries wall=50_000. Cap must keep the newest window.
        let wall = if index < EXCESS_ROWS { 1 } else { 50_000 };
        let mut row = sample_completed(CompletedSample {
            agent: "claude",
            hook_name: &format!("hook{index}"),
            wall_us: Some(wall),
            rtt_us: Some(100),
            payload_bytes: Some(32),
            daemon_ipc_bytes: Some(16),
            timed_out: Some(false),
            budget_ms: Some(20),
            disposition: Some(serde_json::json!({
                "status": "supported",
                "retryable": false,
                "class": "application",
                "reason_code": "ok"
            })),
        });
        row["ts_unix_ms"] = Value::from(index as i64);
        row["session_id"] = Value::from(format!("sess-{index:05}"));
        rows.push(row);
    }

    let first = aggregate_hook_completed_readiness(&rows);
    assert_eq!(first.input_rows_received, total as u64);
    assert_eq!(first.input_rows_processed, MAX_READINESS_INPUT_ROWS as u64);
    assert_eq!(first.input_rows_dropped_at_cap, EXCESS_ROWS as u64);
    assert_eq!(
        first.hook_wall_time_distribution[0].summary.min,
        Some(50_000)
    );
    assert_eq!(
        first.hook_wall_time_distribution[0].summary.max,
        Some(50_000)
    );

    // Append another newest completed event; metrics must advance with the
    // sliding newest window (oldest of the prior window drops out).
    let mut advanced = rows;
    let mut newer = sample_completed(CompletedSample {
        agent: "claude",
        hook_name: "hook-newest",
        wall_us: Some(75_000),
        rtt_us: Some(100),
        payload_bytes: Some(32),
        daemon_ipc_bytes: Some(16),
        timed_out: Some(false),
        budget_ms: Some(20),
        disposition: Some(serde_json::json!({
            "status": "supported",
            "retryable": false,
            "class": "application",
            "reason_code": "ok"
        })),
    });
    newer["ts_unix_ms"] = Value::from(total as i64);
    newer["session_id"] = Value::from("sess-newest");
    advanced.push(newer);

    let second = aggregate_hook_completed_readiness(&advanced);
    assert_eq!(second.input_rows_dropped_at_cap, (EXCESS_ROWS + 1) as u64);
    assert_eq!(
        second.hook_wall_time_distribution[0].summary.max,
        Some(75_000)
    );
    assert_eq!(
        second.hook_wall_time_distribution[0].summary.min,
        Some(50_000)
    );
}

#[test]
fn readiness_aggregation_tie_order_is_stable_under_cap() {
    const EXCESS_ROWS: usize = 17;
    let total = MAX_READINESS_INPUT_ROWS + EXCESS_ROWS;
    let mut rows = Vec::with_capacity(total);
    for index in 0..total {
        // Identical timestamps: secondary keys (session_id) decide which
        // rows fall outside the newest bounded suffix.
        let mut row = sample_completed(CompletedSample {
            agent: "claude",
            hook_name: "postToolUse",
            wall_us: Some(1_000 + index as u64),
            rtt_us: Some(100),
            payload_bytes: Some(32),
            daemon_ipc_bytes: Some(16),
            timed_out: Some(false),
            budget_ms: Some(20),
            disposition: Some(serde_json::json!({
                "status": "supported",
                "retryable": false,
                "class": "application",
                "reason_code": "ok"
            })),
        });
        row["ts_unix_ms"] = Value::from(1_700_000_000_000_i64);
        row["session_id"] = Value::from(format!("sess-{index:05}"));
        rows.push(row);
    }
    // Shuffle then restore deterministic ascending order via the production
    // comparator keys (ts, session_id, hook_name, agent).
    rows.reverse();
    rows.sort_by(|left, right| {
        let key = |row: &Value| {
            (
                row.get("ts_unix_ms")
                    .and_then(Value::as_i64)
                    .unwrap_or_default(),
                row.get("session_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                row.get("hook_name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                row.get("agent")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            )
        };
        key(left).cmp(&key(right))
    });

    let first = aggregate_hook_completed_readiness(&rows);
    let second = aggregate_hook_completed_readiness(&rows);
    assert_eq!(first, second);
    // Newest suffix starts at session sess-00017 (drops sess-00000..00016).
    assert_eq!(
        first.hook_wall_time_distribution[0].summary.min,
        Some(1_000 + EXCESS_ROWS as u64)
    );
    assert_eq!(
        first.hook_wall_time_distribution[0].summary.max,
        Some(1_000 + (total as u64 - 1))
    );
}

#[test]
fn empty_readiness_distributions_are_honest_no_samples_not_zero_fill() {
    let empty = empty_hook_completed_readiness_distributions();
    assert_eq!(empty.collection_status, MetricAvailability::NoSamples);
    assert_eq!(empty.input_rows_received, 0);
    assert_eq!(empty.input_rows_processed, 0);
    assert_eq!(empty.input_rows_dropped_at_cap, 0);
    assert_eq!(empty.events_considered, 0);
    assert!(empty.hook_wall_time_distribution.is_empty());
    assert!(empty.host_ipc_rtt_distribution.is_empty());
    assert!(empty.payload_bytes_distribution.is_empty());
    assert!(empty.timeout_outcomes_by_host.is_empty());
    assert!(empty.disposition_counts_by_host.is_empty());
    assert_eq!(empty.unavailable_metrics.len(), 1);
    assert_eq!(
        empty.unavailable_metrics[0].blocker,
        "hook_completed_does_not_emit_daemon_processing_duration"
    );
    assert_eq!(empty.bounds.max_input_rows, MAX_READINESS_INPUT_ROWS as u64);
    assert_eq!(empty.bounds.host_buckets, READINESS_HOST_BUCKETS as u64);
}

#[test]
fn telemetry_contract_separates_host_ipc_rtt_from_daemon_processing() {
    let contract = host_hook_telemetry_contract();
    assert_eq!(
        contract["latency_semantics"]["host_ipc_rtt"]["event_field"],
        "daemon_rtt_us"
    );
    assert_eq!(
        contract["latency_semantics"]["daemon_processing_duration"]["status"],
        "unavailable"
    );
}

/// The dashboard's diagnostics summary must carry a *measured* readiness
/// aggregate, and must never echo untrusted row values back out.
///
/// `tracedecay-dashboard-api` owns the summary but not the aggregation: it
/// calls the readiness projection through a port this crate installs
/// (`install_dashboard_hook_readiness_projection`). Only that composition can
/// answer `measured`, so the safety contract is proven here, over the real
/// projection, rather than in the dashboard crate's own lib tests where the
/// port is never mounted.
#[test]
fn dashboard_diagnostics_summary_aggregates_hook_completed_rows_safely() {
    crate::hooks::install_dashboard_hook_readiness_projection()
        .expect("dashboard hook readiness projection installs");

    let hook_analytics = tracedecay_dashboard_api::analytics_api::HookAnalyticsRows {
        rows: vec![serde_json::json!({
            "event": "hook_completed",
            "agent": "untrusted-host",
            "hook_name": "privateHookName",
            "hook_wall_time_us": 0,
            "daemon_rtt_us": null,
            "payload_bytes": 0,
            "daemon_ipc_payload_bytes": null,
            "timeout": {"budget_ms": null, "timed_out": null},
            "disposition": {
                "class": "untrusted-class",
                "status": "untrusted-status",
                "retryable": null,
                "reason_code": "private-reason"
            }
        })],
        sources: Vec::new(),
        window: tracedecay_dashboard_api::analytics_api::HookAnalyticsWindow::default(),
    };

    let summary = crate::dashboard::analytics_api::diagnostics_summary_from_parts(
        0,
        &hook_analytics,
        None,
    );
    let readiness = &summary["hook_readiness"];

    assert_eq!(readiness["collection_status"], "measured");
    assert_eq!(readiness["events_considered"], 1);
    // An unrecognised host folds into the bounded `other` bucket rather than
    // opening an unbounded series keyed by attacker-chosen text.
    assert_eq!(readiness["hook_wall_time_distribution"][0]["host"], "other");
    assert_eq!(
        readiness["hook_wall_time_distribution"][0]["summary"]["min"],
        0
    );
    // A present-but-zero sample is measured; an absent one stays honest.
    assert_eq!(
        readiness["host_ipc_rtt_distribution"][0]["summary"]["availability"],
        "no_samples"
    );
    // An unrecognised disposition class is `unknown`, never echoed verbatim.
    assert_eq!(readiness["disposition_counts_by_host"][0]["class"], "unknown");

    let encoded = serde_json::to_string(readiness).expect("readiness encodes");
    for forbidden in [
        "untrusted-host",
        "privateHookName",
        "private-reason",
        "hook_name",
        "reason_code",
    ] {
        assert!(
            !encoded.contains(forbidden),
            "readiness must not leak {forbidden}: {encoded}"
        );
    }
}
