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
    agent: String,
    hook_wall_time_us: Option<u64>,
    daemon_rtt_us: Option<u64>,
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
            agent: row
                .get("agent")
                .and_then(Value::as_str)
                .map_or_else(|| "unknown".to_owned(), bounded_identifier),
            hook_wall_time_us: optional_u64_field(row, "hook_wall_time_us"),
            daemon_rtt_us: optional_u64_field(row, "daemon_rtt_us"),
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
    #[cfg(test)]
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
        record_hook_analytics(
            self.root.as_deref(),
            "hook_completed",
            serde_json::json!({
                "schema_version": HOST_HOOK_TELEMETRY_SCHEMA_VERSION,
                "coverage": HostHookTelemetryCoverage::HostMeasured,
                "agent": self.agent,
                "hook_name": self.hook_name.as_str(),
                "prompt_category": self.prompt_category,
                "duration_us": elapsed_us,
                "duration_ms": elapsed_us / 1000,
                "hook_wall_time_us": elapsed_us,
                "hook_wall_time_ms": elapsed_us / 1000,
                "daemon_rtt_us": state.daemon_rtt_us,
                "daemon_call_count": state.daemon_call_count,
                "payload_bytes": self.payload_bytes,
                "daemon_ipc_payload_bytes": state.daemon_ipc_payload_bytes,
                "timeout": timeout,
                "disposition": disposition,
            }),
        );
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

/// Shared implementation for [`record_hook_invoked`] and
/// [`record_other_hook_invoked`], which differ only in how the analytics
/// `agent` key is derived (a typed [`HintAgent`] vs. the literal `"other"`).
fn record_hook_invoked_named(
    root: Option<&Path>,
    agent_key: &'static str,
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
            "agent": agent_key,
            "hook_name": bounded_identifier(hook_name),
            "prompt_category": prompt_category,
            "payload_bytes": payload_bytes,
        }),
    );
    HookTimingSpan::new_named(root, agent_key, hook_name, prompt_category, payload_bytes)
}

pub(crate) fn record_hook_invoked(
    root: Option<&Path>,
    agent: HintAgent,
    hook_name: &str,
    event_json: &str,
) -> HookTimingSpan {
    record_hook_invoked_named(root, agent.as_key(), hook_name, event_json)
}

pub(crate) fn record_other_hook_invoked(
    root: Option<&Path>,
    hook_name: &str,
    event_json: &str,
) -> HookTimingSpan {
    record_hook_invoked_named(root, "other", hook_name, event_json)
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
mod tests;
