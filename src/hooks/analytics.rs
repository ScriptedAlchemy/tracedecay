use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;

use crate::application::host_admission::{
    HostAdmissionDispositionClass as HookDispositionClass, HostAdmissionStatus,
    HostAdmissionTelemetryDisposition as HookDispositionTelemetry,
};
use crate::errors::TraceDecayError;
use tracedecay_hooks::HookTransportDispositionV1;

use super::tool_hints::{HintAgent, ToolHint};
use super::{HookWorkspaceStatus, claude, prompt_like_text};

pub(crate) const HOOK_ANALYTICS_FILENAME: &str = "hook_analytics.jsonl";

const HOST_HOOK_TELEMETRY_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum HostHookTelemetryCoverage {
    HostMeasured,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct HookTimeoutTelemetry {
    budget_ms: Option<u64>,
    timed_out: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct HookCompletedTelemetry {
    schema_version: u32,
    coverage: HostHookTelemetryCoverage,
    agent: String,
    hook_name: String,
    prompt_category: Option<String>,
    duration_us: Option<u64>,
    duration_ms: Option<u64>,
    hook_wall_time_us: Option<u64>,
    hook_wall_time_ms: Option<u64>,
    daemon_rtt_us: Option<u64>,
    daemon_call_count: Option<u64>,
    payload_bytes: Option<u64>,
    daemon_ipc_payload_bytes: Option<u64>,
    timeout: HookTimeoutTelemetry,
    disposition: HookDispositionTelemetry,
}

impl HookCompletedTelemetry {
    fn from_row(row: &Value) -> Option<(Self, bool)> {
        if row.get("event").and_then(Value::as_str) != Some("hook_completed") {
            return None;
        }

        let (disposition, disposition_folded_to_unknown) =
            HookDispositionTelemetry::from_telemetry_wire(row.get("disposition"));

        let timeout = row.get("timeout").and_then(Value::as_object);
        let telemetry = Self {
            schema_version: row
                .get("schema_version")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or_default(),
            coverage: HostHookTelemetryCoverage::HostMeasured,
            agent: row
                .get("agent")
                .and_then(Value::as_str)
                .map_or_else(|| "unknown".to_owned(), bounded_identifier),
            hook_name: row
                .get("hook_name")
                .and_then(Value::as_str)
                .map_or_else(|| "unknown".to_owned(), bounded_identifier),
            prompt_category: row
                .get("prompt_category")
                .and_then(Value::as_str)
                .map(bounded_identifier),
            duration_us: optional_u64_field(row, "duration_us"),
            duration_ms: optional_u64_field(row, "duration_ms"),
            hook_wall_time_us: optional_u64_field(row, "hook_wall_time_us"),
            hook_wall_time_ms: optional_u64_field(row, "hook_wall_time_ms"),
            daemon_rtt_us: optional_u64_field(row, "daemon_rtt_us"),
            daemon_call_count: optional_u64_field(row, "daemon_call_count"),
            payload_bytes: optional_u64_field(row, "payload_bytes"),
            daemon_ipc_payload_bytes: optional_u64_field(row, "daemon_ipc_payload_bytes"),
            timeout: HookTimeoutTelemetry {
                budget_ms: timeout
                    .and_then(|value| value.get("budget_ms"))
                    .and_then(|value| {
                        value
                            .is_number()
                            .then(|| value.as_u64().unwrap_or_default())
                    }),
                timed_out: timeout
                    .and_then(|value| value.get("timed_out"))
                    .and_then(Value::as_bool),
            },
            disposition,
        };
        Some((telemetry, disposition_folded_to_unknown))
    }
}

fn optional_u64_field(value: &Value, field: &str) -> Option<u64> {
    value.get(field).and_then(Value::as_u64)
}

#[derive(Clone, Default)]
struct HookTimingState {
    daemon_rtt_us: Option<u64>,
    daemon_ipc_payload_bytes: Option<u64>,
    daemon_call_count: u64,
    timeout_budget_ms: Option<u64>,
    timed_out: Option<bool>,
    disposition: Option<HookDispositionTelemetry>,
}

pub(crate) struct HookTimingSpan {
    root: Option<PathBuf>,
    agent: &'static str,
    hook_name: String,
    prompt_category: Option<&'static str>,
    started: Instant,
    enabled: bool,
    payload_bytes: Option<u64>,
    state: Mutex<HookTimingState>,
}

impl HookTimingSpan {
    fn new(
        root: Option<&Path>,
        agent: HintAgent,
        hook_name: &str,
        prompt_category: Option<&'static str>,
        payload_bytes: Option<u64>,
    ) -> Self {
        Self::new_named(
            root,
            agent.as_key(),
            hook_name,
            prompt_category,
            payload_bytes,
        )
    }

    fn new_named(
        root: Option<&Path>,
        agent: &'static str,
        hook_name: &str,
        prompt_category: Option<&'static str>,
        payload_bytes: Option<u64>,
    ) -> Self {
        // Hooks must not synchronously open a store, contact the daemon, or
        // parse legacy configuration, so a daemon-published snapshot is the
        // only authority consulted here. A hook subprocess starts with an
        // empty snapshot cache, so treating "no authority" as "off" silenced
        // every `hook_completed` row in production while `hook_invoked` — the
        // other half of the same span, written by the same unconditional
        // recorder — kept flowing. That renders every real hook as invoked but
        // never finished. Only an authority that explicitly says timings are
        // off suppresses the completion row.
        let enabled = root
            .and_then(|root| crate::config::cached_telemetry_config(root).ok())
            .is_none_or(|telemetry| telemetry.timings);
        Self {
            root: root.map(Path::to_path_buf),
            agent,
            hook_name: bounded_identifier(hook_name),
            prompt_category,
            started: Instant::now(),
            enabled,
            payload_bytes,
            state: Mutex::new(HookTimingState::default()),
        }
    }

    pub(super) fn note_timeout_budget(&self, budget: Duration) {
        self.state().timeout_budget_ms = Some(duration_as_millis_u64(budget));
    }

    pub(crate) fn note_timed_out(&self, timed_out: bool) {
        let mut state = self.state();
        if timed_out {
            state.timed_out = Some(true);
            merge_disposition(
                &mut state.disposition,
                HookDispositionTelemetry::timeout("hook_timeout"),
            );
        } else if state.timed_out.is_none() {
            state.timed_out = Some(false);
        }
    }

    pub(crate) fn note_daemon_result(&self, result: &Result<Value, TraceDecayError>) {
        note_result(&mut self.state(), result);
    }

    pub(crate) fn note_completed_daemon_call(
        &self,
        payload_bytes: Option<u64>,
        rtt_us: u64,
        result: &Result<Value, TraceDecayError>,
    ) {
        let mut state = self.state();
        note_daemon_sample(&mut state, payload_bytes, rtt_us);
        note_result(&mut state, result);
    }

    pub(crate) fn note_completed_daemon_notification(&self, payload_bytes: Option<u64>) {
        let mut state = self.state();
        note_daemon_payload(&mut state, payload_bytes);
        merge_disposition(
            &mut state.disposition,
            HookDispositionTelemetry::unknown("notify_outcome_unavailable"),
        );
    }

    pub(crate) fn note_hook_v2_disposition(&self, disposition: HookTransportDispositionV1) {
        merge_disposition(
            &mut self.state().disposition,
            disposition_from_hook_v2(disposition),
        );
    }

    fn state(&self) -> std::sync::MutexGuard<'_, HookTimingState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn note_daemon_sample(state: &mut HookTimingState, payload_bytes: Option<u64>, rtt_us: u64) {
    state.daemon_rtt_us = Some(
        state
            .daemon_rtt_us
            .unwrap_or_default()
            .saturating_add(rtt_us),
    );
    state.daemon_call_count = state.daemon_call_count.saturating_add(1);
    note_daemon_payload(state, payload_bytes);
}

fn note_daemon_payload(state: &mut HookTimingState, payload_bytes: Option<u64>) {
    if let Some(bytes) = payload_bytes {
        state.daemon_ipc_payload_bytes = Some(
            state
                .daemon_ipc_payload_bytes
                .unwrap_or_default()
                .saturating_add(bytes),
        );
    }
}

impl Drop for HookTimingSpan {
    fn drop(&mut self) {
        if !self.enabled {
            return;
        }
        let elapsed_us = elapsed_us(self.started);
        let state = self.state().clone();
        let timeout = HookTimeoutTelemetry {
            budget_ms: state.timeout_budget_ms,
            timed_out: state.timed_out,
        };
        // Reject default-success: an unobserved disposition is typed unknown,
        // never invented Supported.
        let disposition = state
            .disposition
            .unwrap_or_else(|| HookDispositionTelemetry::unknown("disposition_absent"));
        let telemetry = HookCompletedTelemetry {
            schema_version: HOST_HOOK_TELEMETRY_SCHEMA_VERSION,
            coverage: HostHookTelemetryCoverage::HostMeasured,
            agent: self.agent.to_owned(),
            hook_name: self.hook_name.clone(),
            prompt_category: self.prompt_category.map(str::to_owned),
            duration_us: Some(elapsed_us),
            duration_ms: Some(elapsed_us / 1000),
            hook_wall_time_us: Some(elapsed_us),
            hook_wall_time_ms: Some(elapsed_us / 1000),
            daemon_rtt_us: state.daemon_rtt_us,
            daemon_call_count: Some(state.daemon_call_count),
            payload_bytes: self.payload_bytes,
            daemon_ipc_payload_bytes: state.daemon_ipc_payload_bytes,
            timeout,
            disposition,
        };
        if let Ok(fields) = serde_json::to_value(telemetry) {
            record_hook_analytics(self.root.as_deref(), "hook_completed", fields);
        }
    }
}

fn note_result(state: &mut HookTimingState, result: &Result<Value, TraceDecayError>) {
    match result {
        Ok(value) => {
            if let Some(candidate) = disposition_from_daemon_output(value) {
                merge_disposition(&mut state.disposition, candidate);
            }
            // Reject default-success: Ok without a typed admission status must
            // not invent Supported. Leave disposition unset so Drop emits
            // disposition_absent, and so a later typed failure still sticks.
        }
        Err(error) => {
            merge_disposition(&mut state.disposition, disposition_from_daemon_error(error));
        }
    }
}

fn disposition_from_hook_v2(disposition: HookTransportDispositionV1) -> HookDispositionTelemetry {
    match disposition {
        HookTransportDispositionV1::Accepted => HookDispositionTelemetry::from_parts(
            HostAdmissionStatus::Supported,
            Some(false),
            Some("hook_v2_accepted".to_owned()),
        ),
        HookTransportDispositionV1::AcceptedForReplay => HookDispositionTelemetry::from_parts(
            HostAdmissionStatus::AcceptedForReplay,
            Some(true),
            Some("hook_v2_spooled".to_owned()),
        ),
        HookTransportDispositionV1::CatchupRequired => HookDispositionTelemetry::from_parts(
            HostAdmissionStatus::Degraded,
            Some(true),
            Some("hook_v2_catchup_required".to_owned()),
        ),
    }
}

// Precedence: later typed replaces Unknown; sticky among typed failures and
// timeout/cancellation (higher severity wins). Unknown never overwrites typed.
fn merge_disposition(
    current: &mut Option<HookDispositionTelemetry>,
    candidate: HookDispositionTelemetry,
) {
    if current
        .as_ref()
        .is_none_or(|existing| disposition_severity(&candidate) > disposition_severity(existing))
    {
        *current = Some(candidate);
    }
}

fn disposition_severity(disposition: &HookDispositionTelemetry) -> u8 {
    match disposition.class {
        HookDispositionClass::Timeout => 5,
        HookDispositionClass::Cancellation => 4,
        HookDispositionClass::Transport => 3,
        HookDispositionClass::Application
            if disposition.reason_code.is_some()
                || matches!(
                    disposition.status,
                    HostAdmissionStatus::Degraded
                        | HostAdmissionStatus::Unavailable
                        | HostAdmissionStatus::Backpressured
                ) =>
        {
            2
        }
        // Typed success outranks provisional Unknown so later Supported wins.
        HookDispositionClass::Application => 1,
        HookDispositionClass::Unknown => 0,
    }
}

#[cfg(test)]
pub(crate) fn host_hook_telemetry_contract() -> Value {
    let mut hosts = tracedecay_domain::HostIntegrationIdV1::ALL
        .into_iter()
        .map(tracedecay_domain::HostIntegrationIdV1::as_str)
        .collect::<Vec<_>>();
    hosts.push("other");
    let provider_coverage = tracedecay_domain::HostIntegrationIdV1::ALL.map(|host| {
        serde_json::json!({
            "host": host.as_str(),
            "status": if host == tracedecay_domain::HostIntegrationIdV1::Hermes {
                "partial"
            } else {
                "instrumented"
            },
        })
    });
    serde_json::json!({
        "schema_version": HOST_HOOK_TELEMETRY_SCHEMA_VERSION,
        "metrics": {
            "hook_wall_time": ["hook_wall_time_us", "hook_wall_time_ms"],
            "daemon_rtt": ["daemon_rtt_us", "daemon_call_count"],
            "payload_bytes": ["payload_bytes", "daemon_ipc_payload_bytes"],
            "timeout": ["timeout.budget_ms", "timeout.timed_out"],
            "disposition": [
                "disposition.status",
                "disposition.retryable",
                "disposition.reason_code",
                "disposition.class",
            ],
        },
        // daemon_rtt_us is host-measured end-to-end IPC RTT, not daemon-internal
        // processing. Daemon processing duration is not emitted on hook_completed.
        "latency_semantics": {
            "hook_wall_time": {
                "role": "host_measured_hook_span",
                "fields": ["hook_wall_time_us", "hook_wall_time_ms"],
            },
            "host_ipc_rtt": {
                "role": "true_host_ipc_rtt",
                "event_field": "daemon_rtt_us",
                "aliases": ["daemon_rtt"],
            },
            "daemon_processing_duration": {
                "status": "unavailable",
                "blocker": "hook_completed_does_not_emit_daemon_processing_duration",
            },
        },
        "aggregation_dimensions": {
            "host": hosts,
            "disposition_class": [
                "application", "transport", "timeout", "cancellation", "unknown"
            ],
            "disposition_status": [
                "supported", "degraded", "unavailable", "unknown", "backpressured",
                "accepted_for_replay", "committed", "exact_duplicate"
            ],
            "retryable": [true, false, null],
            "excluded_untrusted_dimensions": ["hook_name", "disposition.reason_code"],
        },
        "provider_coverage": provider_coverage,
    })
}

/// Length-only host-event size. The bytes themselves are never retained.
pub(crate) fn measure_host_event_payload_bytes(event_json: &str) -> Option<u64> {
    u64::try_from(event_json.len()).ok()
}

/// Length-only JSON wire size. Serialized bytes are dropped immediately.
pub(crate) fn measure_json_payload_bytes<T: Serialize + ?Sized>(value: &T) -> Option<u64> {
    serde_json::to_vec(value)
        .ok()
        .and_then(|bytes| u64::try_from(bytes.len()).ok())
}

pub(crate) fn elapsed_us(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}

fn duration_as_millis_u64(budget: Duration) -> u64 {
    u64::try_from(budget.as_millis()).unwrap_or(u64::MAX)
}

fn bounded_identifier(value: &str) -> String {
    const MAX_IDENTIFIER_BYTES: usize = 64;
    if value.is_empty()
        || value.len() > MAX_IDENTIFIER_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return "unknown".to_string();
    }
    value.to_string()
}

fn disposition_from_daemon_output(output: &Value) -> Option<HookDispositionTelemetry> {
    if let Some(admission) = output.get("admission") {
        return HookDispositionTelemetry::from_daemon_wire(admission);
    }
    if output.get("status").and_then(Value::as_str).is_some() {
        return HookDispositionTelemetry::from_daemon_wire(output);
    }
    None
}

fn disposition_from_daemon_error(error: &TraceDecayError) -> HookDispositionTelemetry {
    if let Some((reason_code, retryable, _)) = error.hook_runtime_context() {
        return HookDispositionTelemetry::from_hook_runtime_error(reason_code, retryable);
    }
    match error {
        TraceDecayError::Config { .. } | TraceDecayError::Io(_) => {
            HookDispositionTelemetry::daemon_unavailable()
        }
        _ => HookDispositionTelemetry::unknown("daemon_error"),
    }
}

pub(crate) fn record_hook_invoked(
    root: Option<&Path>,
    agent: HintAgent,
    hook_name: &str,
    event_json: &str,
) -> HookTimingSpan {
    let parsed: Value = serde_json::from_str(event_json).unwrap_or(Value::Null);
    // Length only — never persist event content, prompts, tools, credentials, or paths here.
    let payload_bytes = measure_host_event_payload_bytes(event_json);
    let prompt_category = inferred_prompt_category(&parsed);
    record_hook_analytics(
        root,
        "hook_invoked",
        serde_json::json!({
            "schema_version": HOST_HOOK_TELEMETRY_SCHEMA_VERSION,
            "coverage": HostHookTelemetryCoverage::HostMeasured,
            "agent": agent.as_key(),
            "hook_name": bounded_identifier(hook_name),
            "prompt_category": prompt_category,
            "payload_bytes": payload_bytes,
        }),
    );
    HookTimingSpan::new(root, agent, hook_name, prompt_category, payload_bytes)
}

pub(crate) fn record_other_hook_invoked(
    root: Option<&Path>,
    hook_name: &str,
    event_json: &str,
) -> HookTimingSpan {
    let parsed: Value = serde_json::from_str(event_json).unwrap_or(Value::Null);
    let payload_bytes = measure_host_event_payload_bytes(event_json);
    let prompt_category = inferred_prompt_category(&parsed);
    record_hook_analytics(
        root,
        "hook_invoked",
        serde_json::json!({
            "schema_version": HOST_HOOK_TELEMETRY_SCHEMA_VERSION,
            "coverage": HostHookTelemetryCoverage::HostMeasured,
            "agent": "other",
            "hook_name": bounded_identifier(hook_name),
            "prompt_category": prompt_category,
            "payload_bytes": payload_bytes,
        }),
    );
    HookTimingSpan::new_named(root, "other", hook_name, prompt_category, payload_bytes)
}

pub(super) fn mint_hint_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        "h-{:x}-{:x}-{:x}",
        now_unix_millis(),
        std::process::id(),
        seq
    )
}

pub(super) fn record_hint_analytics(
    root: Option<&Path>,
    event: &str,
    agent: HintAgent,
    session_id: Option<&str>,
    hint_id: &str,
    hint: &ToolHint,
) {
    record_hook_analytics(
        root,
        event,
        serde_json::json!({
            "agent": agent.as_key(),
            "session_id": session_id,
            "category": hint.category.as_key(),
            "hint_id": hint_id,
        }),
    );
}

pub(super) fn record_workspace_status_analytics(
    root: Option<&Path>,
    status: HookWorkspaceStatus,
    session_id: Option<&str>,
) {
    record_hook_analytics(
        root,
        "workspace_status",
        serde_json::json!({
            "agent": HintAgent::Codex.as_key(),
            "session_id": session_id,
            "workspace_status": status.as_key(),
        }),
    );
}

pub(super) fn record_hint_emitted(
    root: Option<&Path>,
    agent: HintAgent,
    session_id: Option<&str>,
    hint_id: &str,
    hint: &ToolHint,
) {
    let event = if session_id.is_none() {
        "missing_session"
    } else {
        "hint_emitted"
    };
    record_hint_analytics(root, event, agent, session_id, hint_id, hint);
}

fn inferred_prompt_category(parsed: &Value) -> Option<&'static str> {
    let text = prompt_like_text(parsed)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if text.is_empty() {
        return None;
    }
    if claude::is_code_research_prompt(&text) {
        Some("code_research")
    } else if text.contains("test") || text.contains("failing") || text.contains("ci") {
        Some("test_or_ci")
    } else if text.contains("dashboard") || text.contains("ui") || text.contains("frontend") {
        Some("dashboard_or_ui")
    } else if text.contains("bug") || text.contains("fix") || text.contains("error") {
        Some("debug_or_fix")
    } else {
        Some("general")
    }
}

pub(super) fn record_hook_analytics(
    root: Option<&Path>,
    event: &str,
    mut fields: serde_json::Value,
) {
    let Some(path) = hook_analytics_path(root) else {
        return;
    };
    let Some(fields) = fields.as_object_mut() else {
        return;
    };
    if !matches!(event, "hook_invoked" | "hook_completed")
        && let Some(root) = root
    {
        fields.insert(
            "project_root".to_string(),
            serde_json::Value::String(root.display().to_string()),
        );
    }
    fields.insert(
        "event".to_string(),
        serde_json::Value::String(event.to_string()),
    );
    fields.insert(
        "ts_unix_ms".to_string(),
        serde_json::Value::Number(serde_json::Number::from(now_unix_millis())),
    );
    let Ok(line) = serde_json::to_string(&fields) else {
        return;
    };
    append_private_jsonl(&path, &line);
}

/// Chooses the analytics file for a hook invocation.
///
/// A hook fires in whatever directory the agent happens to be in, so it must
/// not be the thing that decides a directory is a project. When no authority
/// already names this checkout, analytics go to the profile-wide file rather
/// than to a store shard minted from the path — writing here used to create
/// `projects/proj_<path hash>/` for directories that never became projects, and
/// those shards then outnumbered the real stores.
fn hook_analytics_path(root: Option<&Path>) -> Option<PathBuf> {
    let enrolled_data_root = root.and_then(|root| {
        crate::storage::resolve_enrolled_layout_for_current_profile(root)
            .ok()
            .flatten()
            .map(|layout| layout.data_root)
    });
    match enrolled_data_root {
        Some(data_root) => Some(data_root.join(HOOK_ANALYTICS_FILENAME)),
        None => crate::storage::default_profile_root()
            .ok()
            .map(|profile_root| profile_root.join(HOOK_ANALYTICS_FILENAME)),
    }
}

fn append_private_jsonl(path: &Path, line: &str) {
    let _ = crate::storage::PrivateStoreIo::append_line(path, line);
}

fn now_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}

mod readiness;

#[cfg(test)]
pub(crate) use readiness::empty_hook_completed_readiness_distributions;
pub(crate) use readiness::{
    HookCompletedReadinessDistributions, aggregate_hook_completed_readiness,
};
#[cfg(test)]
use readiness::{
    LATENCY_BUCKET_UPPER_US, MAX_DISPOSITION_SERIES, MAX_READINESS_INPUT_ROWS, MetricAvailability,
    READINESS_HOST_BUCKETS,
};

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
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
    fn hook_v2_transport_dispositions_remain_distinct_in_telemetry() {
        let accepted = disposition_from_hook_v2(HookTransportDispositionV1::Accepted);
        let spooled = disposition_from_hook_v2(HookTransportDispositionV1::AcceptedForReplay);
        let catchup = disposition_from_hook_v2(HookTransportDispositionV1::CatchupRequired);

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
        let analytics_jsonl =
            std::fs::read_to_string(data_root.join(HOOK_ANALYTICS_FILENAME)).unwrap();
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
                let _guard =
                    crate::hooks::TestDaemonHookActionGuard::install([serde_json::json!({
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
                .find(|row| {
                    row["event"] == "hook_completed" && row["hook_name"] == "daemonBoundary"
                })
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
            .find(|row| {
                row["event"] == "hook_completed" && row["hook_name"] == "notificationBoundary"
            })
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
            let span =
                record_hook_invoked(Some(&project_root), HintAgent::Claude, "untypedOk", "{}");
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
}
