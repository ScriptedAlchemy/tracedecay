//! Hook handlers for Claude Code, Kiro, Cursor, and Codex integrations.
//!
//! Each agent sends its own event schema and expects its own output shape, so
//! handlers stay agent-specific while shared plumbing lives here.

use std::future::Future;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use tracedecay_hooks::{DaemonHookEvent, HookRouteMetadata};

mod analytics;
mod claude;
mod codex;
mod cursor;
mod cursor_compact;
mod daemon_ports;
mod dispatch;
pub mod hint_outcomes;
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod hook_boundary_failure_matrix;
mod kiro;
pub mod memory_inject;
mod post_tool_use;
mod steering;
mod store_layout;
pub mod tool_hints;
pub use dispatch::NATIVE_HOOK_HOSTS;
pub use dispatch::NativeContextScoutLifecycleV1;
pub use dispatch::native_capture_material;
pub use dispatch::project_and_worktree_locators_for_scope as hook_scope_locators;
pub use dispatch::project_id_for_layout as hook_project_id_for_layout;
pub use dispatch::protected_session_id_for_native as protected_native_session_id;
pub use dispatch::publish_daemon_bindings as publish_hook_bindings;

pub use claude::{
    evaluate_hook_decision, hook_claude_post_compact, hook_claude_post_tool_use,
    hook_claude_session_start, hook_claude_subagent_start, hook_pre_tool_use, hook_prompt_submit,
    hook_stop,
};
pub use codex::{
    codex_additional_context_json, codex_apply_patch_rel_paths, codex_project_root_from_event,
    codex_subagent_start_log_line, codex_user_prompt_submit_context_for_event,
    codex_workspace_status_from_event, evaluate_codex_subagent_start, hook_codex_post_compact,
    hook_codex_post_tool_use, hook_codex_session_start, hook_codex_stop, hook_codex_subagent_start,
    hook_codex_user_prompt_submit, record_codex_subagent_start,
};
pub use cursor::{
    CURSOR_CATCH_UP_INGEST_MAX_BYTES, cursor_project_root_from_event, cursor_session_start_json,
    evaluate_cursor_subagent_start, hook_cursor_after_file_edit, hook_cursor_after_shell,
    hook_cursor_post_tool_use, hook_cursor_pre_compact, hook_cursor_session_end,
    hook_cursor_session_start, hook_cursor_stop, hook_cursor_subagent_start,
    hook_cursor_workspace_open,
};
pub use cursor_compact::{CursorPreCompactOutcome, cursor_pre_compact_via_daemon};
pub use kiro::{
    evaluate_kiro_pre_tool_use, hook_kiro_post_tool_use, hook_kiro_pre_tool_use,
    hook_kiro_prompt_submit, kiro_post_tool_use_rel_paths,
};
pub use steering::{
    CODEX_SESSION_CONTEXT_BUDGET, CURSOR_PLUGIN_SKILLS, CURSOR_SESSION_CONTEXT_BUDGET,
    HookWorkspaceStatus, build_codex_session_context, build_codex_session_context_for_workspace,
    build_cursor_session_context, cursor_staleness_hint,
};

#[cfg(test)]
use analytics::HOOK_ANALYTICS_FILENAME;
use analytics::mint_hint_id;
pub use analytics::{
    HookCompletedReadinessDistributions, host_hook_telemetry_contract,
    measure_host_event_payload_bytes,
};
use analytics::{
    record_hint_analytics, record_hint_emitted, record_hook_analytics, record_hook_invoked,
    record_hook_invoked_parsed, record_other_hook_invoked, record_workspace_status_analytics,
};

pub fn aggregate_hook_completed_readiness(rows: &[Value]) -> HookCompletedReadinessDistributions {
    analytics::aggregate_hook_completed_readiness(rows)
}

use tool_hints::{HintAgent, ToolHint};
use tracedecay_policy::hint_delivery::HintDeliveryDecisionV1;

#[hotpath::measure(future = true, label = "agent_hosts.hooks.dispatch_kimi_event")]
pub async fn dispatch_kimi_event(event_json: &str, project_root: &Path) -> Option<String> {
    let telemetry = record_other_hook_invoked(Some(project_root), "kimi_event", event_json);
    dispatch::dispatch(
        tracedecay_hooks::HookHostV1::KimiCode,
        event_json,
        project_root,
        Some(&telemetry),
    )
    .await
    .into_recorded_guidance(&telemetry)
    .flatten()
}

#[hotpath::measure(future = true, label = "agent_hosts.hooks.dispatch_opencode_event")]
pub async fn dispatch_opencode_event(event_json: &str, project_root: &Path) -> Option<String> {
    let telemetry = record_other_hook_invoked(Some(project_root), "opencode_event", event_json);
    let dispatch = if tracedecay_hooks::decode_opencode_lsp_event(event_json.as_bytes()).is_ok() {
        dispatch::dispatch_opencode_lsp_updated(event_json, project_root, Some(&telemetry)).await
    } else {
        dispatch::dispatch(
            tracedecay_hooks::HookHostV1::OpenCode,
            event_json,
            project_root,
            Some(&telemetry),
        )
        .await
    };
    dispatch.into_recorded_guidance(&telemetry).flatten()
}

#[hotpath::measure(
    future = true,
    label = "agent_hosts.hooks.dispatch_opencode_tool_after"
)]
pub async fn dispatch_opencode_tool_after(event_json: &str, project_root: &Path) -> Option<String> {
    let telemetry =
        record_other_hook_invoked(Some(project_root), "opencode_tool_after", event_json);
    dispatch::dispatch_opencode_tool_after(event_json, project_root, Some(&telemetry))
        .await
        .into_recorded_guidance(&telemetry)
        .flatten()
}

#[hotpath::measure(future = true, label = "hosts.hooks.write_output")]
pub(crate) async fn write_hook_output(
    project_root: Option<&Path>,
    host: tracedecay_hooks::HookHostV1,
    event_json: &str,
    output: &str,
    telemetry: Option<&analytics::HookTimingSpan>,
) -> bool {
    let delivery_writer = match project_root {
        None => None,
        Some(project_root) => {
            let Some(layout) = store_layout::enrolled_layout(project_root) else {
                tracing::warn!(
                    host = host.hook_key(),
                    "Hook output delivery has no enrolled project layout"
                );
                return false;
            };
            match tracedecay_hooks::HookDeliveryReceiptSpoolV1::open(
                tracedecay_hooks::hook_delivery_receipt_spool_root(&layout.data_root, host),
            ) {
                Ok(writer) => Some(writer),
                Err(error) => {
                    tracing::warn!(host = host.hook_key(), %error, "Hook output delivery receipt spool could not be opened");
                    return false;
                }
            }
        }
    };
    // Scoped so both the stdout handle and its lock guard (neither of which is
    // `Send`) are fully dropped before the delivery-settlement await below;
    // otherwise the compiler must treat this future as holding a `!Send`
    // guard across that suspend point.
    let written = {
        let stdout = std::io::stdout();
        let mut stdout = stdout.lock();
        stdout
            .write_all(output.as_bytes())
            .and_then(|()| stdout.write_all(b"\n"))
            .and_then(|()| stdout.flush())
    };
    if let Err(error) = written {
        eprintln!("tracedecay hook: failed to flush host output: {error}");
        return false;
    }
    let Some(project_root) = project_root else {
        return true;
    };
    let Some(delivery_writer) = delivery_writer else {
        tracing::error!(
            host = host.hook_key(),
            "Hook output delivery writer disappeared"
        );
        return false;
    };
    let parsed = serde_json::from_str::<Value>(event_json).unwrap_or(Value::Null);
    let session = event_session_id(&parsed).unwrap_or_else(|| "session-unavailable".to_owned());
    let settled_at = daemon_ports::now_utc();
    let Some(owner) = hook_output_owner_event_id(host, event_json, output) else {
        tracing::error!(
            host = host.hook_key(),
            "Hook output delivery identity could not be derived"
        );
        return false;
    };
    let Ok(channel) = tracedecay_domain::canonical_sha256(&(
        "tracedecay.hook-output-channel.v1",
        host.hook_key(),
        session,
    )) else {
        tracing::error!(
            host = host.hook_key(),
            "Hook output delivery channel identity could not be derived"
        );
        return false;
    };
    let settlement = tracedecay_domain::DeliverySettlementV1 {
        attempt: tracedecay_domain::DeliverySettlementAttemptV1 {
            owner_event_id: owner,
            event_class: tracedecay_domain::DeliveryEventClassV1::OperationTerminal,
            channel: tracedecay_domain::DeliveryChannelIdentityV1 {
                surface: tracedecay_domain::DeliverySurfaceFamilyV1::Hook,
                channel_ref: format!(
                    "hook:{}:{}",
                    host.hook_key(),
                    channel.as_str().trim_start_matches("sha256:")
                ),
            },
            work_attempt: None,
            eligible: 1,
            valid_at: settled_at,
            attempted_at: settled_at,
        },
        outcome: tracedecay_domain::DeliverySettlementOutcomeV1::Delivered,
        settled_at,
        drop_reason: None,
    };
    let receipt = match tracedecay_hooks::HookDeliverySourceReceiptV1::new(settlement) {
        Ok(receipt) => receipt,
        Err(error) => {
            tracing::warn!(%error, "Hook output delivery receipt is invalid");
            return false;
        }
    };
    let receipt = match delivery_writer.append_or_replay(&receipt) {
        Ok(receipt) => receipt,
        Err(error) => {
            tracing::warn!(%error, "Hook output delivery receipt could not be persisted");
            return false;
        }
    };
    let settlement = receipt.settlement;
    drop(delivery_writer);
    if let Err(error) = daemon_hook_action(
        Some(project_root),
        serde_json::json!({
            "action": "delivery_settlement",
            "settlement": settlement,
        }),
        telemetry,
    )
    .await
    {
        // The source receipt is durable and the daemon replay lane will retry
        // the exact retained settlement after a transport or daemon failure.
        tracing::warn!(%error, "Hook output delivery receipt could not be reported; retained for replay");
    }
    true
}

fn hook_output_owner_event_id(
    host: tracedecay_hooks::HookHostV1,
    event_json: &str,
    output: &str,
) -> Option<String> {
    let owner = tracedecay_domain::canonical_sha256(&(
        "tracedecay.hook-output-delivery.v1",
        host.hook_key(),
        event_json,
        output,
    ))
    .ok()?;
    Some(format!(
        "hook:output:{}",
        owner.as_str().trim_start_matches("sha256:")
    ))
}

macro_rules! read_hook_event {
    () => {{
        match $crate::hooks::read_stdin_bounded() {
            Ok($crate::hooks::HookStdinRead::Event(event)) => event,
            Ok($crate::hooks::HookStdinRead::Oversized) => {
                eprintln!(
                    "tracedecay hook: stdin exceeds wire message bound ({})",
                    ::tracedecay_framing::WIRE_RECORD_TOO_LARGE
                );
                return 0;
            }
            Err(e) => {
                eprintln!("tracedecay hook: failed to read stdin: {e}");
                return 1;
            }
        }
    }};
}
pub(crate) use read_hook_event;

/// Shared native-event handler body: read the bounded event, resolve the
/// project root, dispatch, and deliver any guidance for `host`.
async fn hook_native_event(
    host: tracedecay_hooks::HookHostV1,
    dispatch: impl AsyncFnOnce(&str, &Path) -> Option<String>,
) -> i32 {
    let event = read_hook_event!();
    let Some(root) = native_event_project_root(&event).await else {
        return 0;
    };
    if let Some(guidance) = dispatch(&event, &root).await
        && !write_hook_output(Some(&root), host, &event, &guidance, None).await
    {
        return 1;
    }
    0
}

#[hotpath::measure(future = true, label = "hosts.hooks.kimi_event")]
pub async fn hook_kimi_event() -> i32 {
    hook_native_event(tracedecay_hooks::HookHostV1::KimiCode, dispatch_kimi_event).await
}

#[hotpath::measure(future = true, label = "hosts.hooks.opencode_event")]
pub async fn hook_opencode_event() -> i32 {
    hook_native_event(
        tracedecay_hooks::HookHostV1::OpenCode,
        dispatch_opencode_event,
    )
    .await
}

#[hotpath::measure(future = true, label = "hosts.hooks.opencode_tool_after")]
pub async fn hook_opencode_tool_after() -> i32 {
    hook_native_event(
        tracedecay_hooks::HookHostV1::OpenCode,
        dispatch_opencode_tool_after,
    )
    .await
}

#[hotpath::measure(future = true, label = "hosts.hooks.native_resolve_root")]
async fn native_event_project_root(event: &str) -> Option<PathBuf> {
    let parsed = serde_json::from_str::<Value>(event).ok();
    let start = parsed
        .as_ref()
        .and_then(|value| value.get("cwd"))
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())?;
    crate::ports::hook_runtime::resolve_project_root_with_identity(&start).await
}

pub(crate) async fn daemon_tool_json(
    project_root: Option<&Path>,
    tool_name: &str,
    arguments: Value,
) -> crate::errors::Result<Value> {
    crate::ports::hook_runtime::daemon_tool_json(project_root, tool_name, arguments, false).await
}

#[hotpath::measure(future = true, label = "hosts.hooks.daemon_action")]
pub(crate) async fn daemon_hook_action(
    project_root: Option<&Path>,
    mut arguments: Value,
    telemetry: Option<&analytics::HookTimingSpan>,
) -> crate::errors::Result<Value> {
    arguments["format"] = serde_json::json!("json");
    let payload_bytes = analytics::measure_json_payload_bytes(&arguments);
    #[cfg(test)]
    if let Some(result) = take_test_daemon_hook_action(project_root, &arguments) {
        if let Some(telemetry) = telemetry {
            telemetry.note_completed_daemon_call(payload_bytes, 0, &result);
        }
        return result;
    }
    let started = std::time::Instant::now();
    let result = crate::ports::hook_runtime::daemon_tool_json(
        project_root,
        "tracedecay_hook_runtime",
        arguments,
        project_root.is_some(),
    )
    .await;
    if let Some(telemetry) = telemetry {
        telemetry.note_completed_daemon_call(
            payload_bytes,
            analytics::elapsed_us(started),
            &result,
        );
    }
    result
}

#[hotpath::measure(future = true, label = "hosts.hooks.ingest_user_session")]
pub(crate) async fn ingest_user_session(
    provider: &str,
    session_id: Option<String>,
    telemetry: Option<&analytics::HookTimingSpan>,
) -> bool {
    if session_id.is_none() {
        return false;
    }
    match daemon_hook_action(
        None,
        serde_json::json!({
            "action": "ingest_transcript",
            "provider": provider.to_lowercase(),
            "user_scope": true,
            "session_id": session_id,
        }),
        telemetry,
    )
    .await
    {
        Ok(result) => result
            .get("messages_upserted")
            .and_then(Value::as_u64)
            .is_some_and(|count| count > 0),
        Err(error) => {
            tracing::warn!(provider, %error, "user transcript ingest daemon call failed");
            false
        }
    }
}

/// Fail-open transcript ingest shared by Cursor and Kiro catch-up hooks.
///
/// Daemon errors and budget timeouts are typed here. They must not collapse
/// into a zero-counter success that hosts would treat as "nothing to review".
#[derive(Default)]
pub(crate) struct TranscriptIngestOutcome {
    user_scope: bool,
    messages_upserted: u64,
    failed: bool,
    timed_out: bool,
}

impl TranscriptIngestOutcome {
    fn from_daemon_result(result: &Value) -> Self {
        Self {
            user_scope: result
                .get("user_scope")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            messages_upserted: result
                .get("messages_upserted")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            failed: false,
            timed_out: false,
        }
    }

    fn failed() -> Self {
        Self {
            failed: true,
            ..Self::default()
        }
    }

    fn timed_out() -> Self {
        Self {
            timed_out: true,
            ..Self::default()
        }
    }

    pub(crate) fn should_schedule_user_review(&self) -> bool {
        !self.failed && !self.timed_out && self.user_scope && self.messages_upserted > 0
    }
}

enum IngestAttempt {
    Succeeded(Value),
    Failed,
    TimedOut,
}

pub(crate) async fn await_within_stop_budget<T>(
    work: impl Future<Output = T>,
    budget: Duration,
    telemetry: Option<&analytics::HookTimingSpan>,
    on_timeout: impl FnOnce() -> T,
) -> T {
    if let Some(telemetry) = telemetry {
        telemetry.note_timeout_budget(budget);
    }
    match tokio::time::timeout(budget, work).await {
        Ok(value) => {
            if let Some(telemetry) = telemetry {
                telemetry.note_timed_out(false);
            }
            value
        }
        Err(_) => {
            if let Some(telemetry) = telemetry {
                telemetry.note_timed_out(true);
            }
            on_timeout()
        }
    }
}

#[hotpath::measure(future = true, label = "hosts.hooks.ingest_transcript")]
pub(crate) async fn ingest_transcript_for_event(
    provider: &str,
    event_json: &str,
    project_root: Option<&Path>,
    max_new_bytes: Option<u64>,
    budget: Duration,
    telemetry: Option<&analytics::HookTimingSpan>,
) -> TranscriptIngestOutcome {
    let mut args = serde_json::json!({
        "action": "ingest_transcript",
        "provider": provider,
        "user_scope": project_root.is_none(),
        "event_json": event_json,
    });
    if let Some(max_new_bytes) = max_new_bytes {
        args["max_new_bytes"] = serde_json::json!(max_new_bytes);
    }
    args["timeout_budget_ms"] = serde_json::json!(budget.as_millis() as u64);
    match await_within_stop_budget(
        async {
            match daemon_hook_action(project_root, args, telemetry).await {
                Ok(result) => IngestAttempt::Succeeded(result),
                Err(error) => {
                    tracing::warn!(provider, %error, "transcript ingest daemon call failed");
                    IngestAttempt::Failed
                }
            }
        },
        budget,
        telemetry,
        || {
            tracing::warn!(provider, "transcript ingest daemon call timed out");
            IngestAttempt::TimedOut
        },
    )
    .await
    {
        IngestAttempt::Succeeded(result) => TranscriptIngestOutcome::from_daemon_result(&result),
        IngestAttempt::Failed => TranscriptIngestOutcome::failed(),
        IngestAttempt::TimedOut => TranscriptIngestOutcome::timed_out(),
    }
}

pub(crate) async fn reset_counter_for_project(
    project_root: &Path,
    telemetry: Option<&analytics::HookTimingSpan>,
) {
    if let Err(error) = daemon_hook_action(
        Some(project_root),
        serde_json::json!({ "action": "reset_counter" }),
        telemetry,
    )
    .await
    {
        tracing::warn!(%error, "local counter reset daemon call failed");
    }
}

/// Claude and Codex share this `hookSpecificOutput.additionalContext` stdout
/// shape. Cursor uses a flatter `{ additional_context }` object.
pub fn additional_context_json(event_name: &str, additional_context: &str) -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": event_name,
            "additionalContext": additional_context,
        }
    })
    .to_string()
}

pub(crate) fn compact_daemon_args(
    action: &str,
    provider: &str,
    user_scope: bool,
    event_json: &str,
    session_id: Option<&str>,
) -> Value {
    let mut args = serde_json::json!({
        "action": action,
        "provider": provider,
        "user_scope": user_scope,
        "event_json": event_json,
    });
    if let Some(session_id) = session_id {
        args["session_id"] = serde_json::json!(session_id);
    }
    args
}

#[hotpath::measure(future = true, label = "hosts.hooks.notify_event")]
pub(crate) async fn notify_hook_event_with_telemetry(
    project_root: &Path,
    event: DaemonHookEvent,
    telemetry: &analytics::HookTimingSpan,
) {
    let payload_bytes = analytics::measure_json_payload_bytes(&event);
    crate::ports::hook_runtime::notify_hook_event(project_root, event).await;
    telemetry.note_completed_daemon_notification(payload_bytes);
}

pub(crate) async fn notify_hook_event_with_optional_telemetry(
    project_root: &Path,
    event: DaemonHookEvent,
    telemetry: Option<&analytics::HookTimingSpan>,
) {
    match telemetry {
        Some(telemetry) => {
            notify_hook_event_with_telemetry(project_root, event, telemetry).await;
        }
        None => {
            crate::ports::hook_runtime::notify_hook_event(project_root, event).await;
        }
    }
}

#[hotpath::measure(future = true, label = "hosts.hooks.hermes_terminal_receipt")]
pub async fn hook_hermes_terminal_receipt() -> i32 {
    let event_json = read_hook_event!();
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&event_json) else {
        return 0;
    };
    let cwd = parsed
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            parsed.get("route").and_then(|route| {
                route
                    .get("cwd")
                    .or_else(|| route.get("worktree"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
        });
    let project_root = match cwd {
        Some(cwd) => {
            crate::ports::hook_runtime::resolve_project_root_with_identity(std::path::Path::new(
                &cwd,
            ))
            .await
        }
        None => None,
    };
    let hook_name = parsed
        .get("hook_event_name")
        .or_else(|| parsed.get("event"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("nativeCallback");
    let hook_telemetry = record_hook_invoked(
        project_root.as_deref(),
        HintAgent::Hermes,
        hook_name,
        &event_json,
    );
    let guidance = dispatch::dispatch_for_scope(
        tracedecay_hooks::HookHostV1::Hermes,
        &event_json,
        project_root.as_deref(),
        Some(&hook_telemetry),
    )
    .await
    .into_recorded_guidance(&hook_telemetry)
    .flatten();
    if guidance.is_none()
        && let Ok(event) = serde_json::from_value::<DaemonHookEvent>(parsed)
        && event.agent == "hermes"
        && matches!(
            event.event.as_str(),
            "terminalReceipt" | "turnCompleted" | "turnIngested"
        )
        && event.receipt.is_some()
    {
        if let Some(project_root) = project_root.as_ref() {
            notify_hook_event_with_telemetry(project_root, event, &hook_telemetry).await;
        } else if let Err(error) = daemon_hook_action(
            None,
            serde_json::json!({ "action": "hermes_receipt", "event": event }),
            Some(&hook_telemetry),
        )
        .await
        {
            tracing::warn!(%error, "user Hermes receipt daemon call failed");
        }
    }
    let output = guidance.map_or_else(
        || serde_json::json!({}).to_string(),
        |guidance| serde_json::json!({ "additional_context": guidance }).to_string(),
    );
    if !write_hook_output(
        project_root.as_deref(),
        tracedecay_hooks::HookHostV1::Hermes,
        &event_json,
        &output,
        Some(&hook_telemetry),
    )
    .await
    {
        return 1;
    }
    0
}

pub async fn schedule_user_session_review(provider: &str, session_id: Option<&str>) {
    let hint = daemon_hook_action(
        None,
        serde_json::json!({
            "action": "user_review",
            "provider": provider,
            "session_id": session_id,
        }),
        None,
    );
    let _ = tokio::time::timeout(std::time::Duration::from_millis(25), hint).await;
}

const TRACEDECAY_RESEARCH_BLOCK_REASON: &str = "STOP: Use tracedecay MCP tools \
(tracedecay_context, tracedecay_grep, tracedecay_search, tracedecay_callees, \
tracedecay_callers, tracedecay_impact, tracedecay_files, tracedecay_affected) \
instead of agents for code research. Route literal/regex text to tracedecay_grep, \
symbol names to tracedecay_search, and concepts to tracedecay_context. TraceDecay \
is faster and more precise for symbol relationships, call paths, and code structure. \
Only use agents for code exploration if you have already tried tracedecay and it \
cannot answer the question.";

fn research_block_reason(hint: Option<ToolHint>) -> String {
    let base = std::env::var("TRACEDECAY_RESEARCH_BLOCK_REASON")
        .ok()
        .unwrap_or_else(|| TRACEDECAY_RESEARCH_BLOCK_REASON.to_string());
    hint.map_or_else(
        || base.clone(),
        |hint| format!("{}\n\n{}", base, format_tool_hint(&hint)),
    )
}

#[cfg(test)]
pub(crate) struct EnvGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

#[cfg(test)]
impl EnvGuard {
    pub(crate) fn set_path(key: &'static str, value: &Path) -> Self {
        let previous = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

#[cfg(test)]
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

#[cfg(test)]
pub(crate) fn lock_test_env() -> std::sync::MutexGuard<'static, ()> {
    crate::config::lock_user_data_dir_test_env()
}

#[cfg(test)]
pub(crate) fn run_with_test_env_lock<T>(future: impl std::future::Future<Output = T>) -> T {
    let _lock = lock_test_env();
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build hook test runtime")
        .block_on(future)
}

#[cfg(test)]
#[derive(Default)]
struct TestDaemonHookActionState {
    owner: Option<std::thread::ThreadId>,
    responses: std::collections::VecDeque<Value>,
    calls: Vec<(Option<PathBuf>, Value)>,
}

#[cfg(test)]
struct TestDaemonHookActionSlot {
    state: std::sync::Mutex<TestDaemonHookActionState>,
    available: std::sync::Condvar,
}

#[cfg(test)]
static TEST_DAEMON_HOOK_ACTION: std::sync::LazyLock<TestDaemonHookActionSlot> =
    std::sync::LazyLock::new(|| TestDaemonHookActionSlot {
        state: std::sync::Mutex::new(TestDaemonHookActionState::default()),
        available: std::sync::Condvar::new(),
    });

#[cfg(test)]
pub(crate) struct TestDaemonHookActionGuard {
    owner: std::thread::ThreadId,
}

#[cfg(test)]
impl TestDaemonHookActionGuard {
    pub(crate) fn install(responses: impl IntoIterator<Item = Value>) -> Self {
        let owner = std::thread::current().id();
        let mut state = TEST_DAEMON_HOOK_ACTION
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state = TEST_DAEMON_HOOK_ACTION
            .available
            .wait_while(state, |state| state.owner.is_some())
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.owner = Some(owner);
        state.responses = responses.into_iter().collect();
        state.calls.clear();
        Self { owner }
    }

    pub(crate) fn calls(&self) -> Vec<(Option<PathBuf>, Value)> {
        let state = TEST_DAEMON_HOOK_ACTION
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(state.owner, Some(self.owner));
        state.calls.clone()
    }
}

#[cfg(test)]
impl Drop for TestDaemonHookActionGuard {
    fn drop(&mut self) {
        let mut state = TEST_DAEMON_HOOK_ACTION
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.owner == Some(self.owner) {
            *state = TestDaemonHookActionState::default();
            TEST_DAEMON_HOOK_ACTION.available.notify_one();
        }
    }
}

#[cfg(test)]
fn take_test_daemon_hook_action(
    project_root: Option<&Path>,
    arguments: &Value,
) -> Option<crate::errors::Result<Value>> {
    let mut state = TEST_DAEMON_HOOK_ACTION
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if state.owner != Some(std::thread::current().id()) {
        return None;
    }
    state
        .calls
        .push((project_root.map(Path::to_path_buf), arguments.clone()));
    Some(
        state
            .responses
            .pop_front()
            .ok_or_else(|| crate::errors::TraceDecayError::Config {
                message: "daemon hook test responder has no response".to_string(),
            }),
    )
}

pub(crate) fn hook_route_metadata_from_parsed(
    parsed: &Value,
    project_root: &Path,
) -> HookRouteMetadata {
    let cwd = event_cwd_from_parsed(parsed);
    let route_root = cwd.as_deref().unwrap_or(project_root);
    let worktree = crate::worktree::git_worktree_root(route_root)
        .unwrap_or_else(|| project_root.to_path_buf());
    let branch = crate::branch::current_branch(&worktree);
    HookRouteMetadata {
        session_id: hook_route_session_id(parsed),
        thread_id: text_field(
            parsed,
            &[
                "thread_id",
                "threadId",
                "conversation_thread_id",
                "conversationThreadId",
            ],
        ),
        cwd,
        worktree: Some(worktree),
        branch,
    }
}

const HOOK_SESSION_ID_KEYS: &[&str] = &[
    "session_id",
    "sessionId",
    "conversation_id",
    "conversationId",
    "chat_id",
    "chatId",
];

fn hook_route_session_id(parsed: &Value) -> Option<String> {
    text_field(parsed, HOOK_SESSION_ID_KEYS)
}

fn deduped_project_hint_with_id(
    root: Option<&Path>,
    agent: HintAgent,
    session_id: Option<String>,
    hint_id: &str,
    hint: ToolHint,
) -> Option<ToolHint> {
    let Some(session_id) = session_id else {
        record_hint_emitted(root, agent, None, hint_id, &hint);
        return Some(hint);
    };

    // Hooks may carry a stable session id without a project root. Persist
    // those decisions in the user profile so one missing cwd does not turn
    // every prompt/tool event into the same repeated hint.
    let project_path = root
        .and_then(store_layout::layout)
        .filter(|layout| layout.data_root.is_dir())
        .map(|layout| layout.data_root.join("tool_hints_seen.json"));
    let path = project_path.or_else(|| {
        crate::storage::default_profile_root()
            .ok()
            .map(|profile| profile.join("tool_hints_seen.json"))
    });
    let Some(path) = path else {
        record_hint_emitted(root, agent, Some(&session_id), hint_id, &hint);
        return Some(hint);
    };
    let mut dedupe = tool_hints::ToolHintDedupe::load_or_default(&path);
    let decision = dedupe.decide(&session_id, hint.category);
    // Every decision — including the suppressed ones — advances the persisted
    // budget, so the save is unconditional.
    let _ = dedupe.save(&path);

    // A suppressed decision still records the hint as it stood; only the
    // escalation path reports the escalated wording.
    let (event, reported) = match decision {
        HintDeliveryDecisionV1::Deliver => ("hint_emitted", hint),
        HintDeliveryDecisionV1::DeliverEscalation => ("hint_escalated", hint.escalated()),
        HintDeliveryDecisionV1::SuppressBudget => ("suppressed_budget", hint),
        HintDeliveryDecisionV1::SuppressDuplicate => ("suppressed_duplicate", hint),
    };
    record_hint_analytics(root, event, agent, Some(&session_id), hint_id, &reported);

    matches!(
        decision,
        HintDeliveryDecisionV1::Deliver | HintDeliveryDecisionV1::DeliverEscalation
    )
    .then_some(reported)
}

fn nearest_project_like_root(start: &Path) -> Option<PathBuf> {
    if let Some(root) = crate::worktree::git_worktree_root(start) {
        return Some(root);
    }
    let mut dir = start.to_path_buf();
    loop {
        if project_marker_exists(&dir) {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn is_project_like_workspace(cwd: &Path) -> bool {
    nearest_project_like_root(cwd).is_some()
}

fn project_marker_exists(dir: &Path) -> bool {
    const MARKERS: &[&str] = &[
        ".git",
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "go.mod",
        "pom.xml",
        "build.gradle",
        "build.gradle.kts",
        "deno.json",
        "tsconfig.json",
    ];
    MARKERS.iter().any(|marker| dir.join(marker).exists())
}

fn rel_under_root(root: &Path, abs: &Path) -> Option<String> {
    let stripped = abs.strip_prefix(root).ok()?;
    if stripped.as_os_str().is_empty() {
        return None;
    }
    if stripped.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return None;
    }
    Some(stripped.to_string_lossy().replace('\\', "/"))
}

fn text_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn prompt_like_text(parsed: &Value) -> Option<String> {
    [
        "prompt",
        "user_prompt",
        "message",
        "input",
        "task",
        "description",
    ]
    .iter()
    .find_map(|key| parsed.get(*key).and_then(Value::as_str))
    .filter(|text| !text.is_empty())
    .map(str::to_string)
}

fn event_session_id(parsed: &Value) -> Option<String> {
    hook_route_session_id(parsed)
}

fn event_cwd_from_parsed(parsed: &Value) -> Option<PathBuf> {
    let cwd = parsed.get("cwd").and_then(Value::as_str)?;
    let path = Path::new(cwd);
    if path.as_os_str().is_empty() {
        None
    } else {
        Some(path.to_path_buf())
    }
}

/// Resolves the tracedecay project root named by a hook event's `cwd`.
///
/// Claude, Codex, and Kiro all send the session working directory under the
/// same key, so this resolver is host-neutral and lives beside the other
/// event-field readers rather than in any one host's module.
fn event_project_root(parsed: &Value) -> Option<PathBuf> {
    let cwd = event_cwd_from_parsed(parsed)?;
    crate::config::discover_project_root(&cwd)
}

/// [`event_project_root`] for callers that hold only the raw event JSON.
fn event_project_root_from_json(event_json: &str) -> Option<PathBuf> {
    let parsed: Value = serde_json::from_str(event_json).ok()?;
    event_project_root(&parsed)
}

/// The project root of the hook process's own working directory. Used by the
/// surfaces whose payload carries no `cwd` at all.
fn process_cwd_project_root() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    crate::config::discover_project_root(&cwd)
}

/// Resolves the project root from the event `cwd`, falling back to the hook
/// process's working directory when the event omits one entirely. A present but
/// non-project `cwd` still resolves to nothing: the event named a directory, and
/// silently re-attributing it to wherever the hook happens to run would route the
/// event into an unrelated project.
fn event_project_root_or_process_cwd(parsed: &Value) -> Option<PathBuf> {
    match event_cwd_from_parsed(parsed) {
        Some(cwd) => crate::config::discover_project_root(&cwd),
        None => process_cwd_project_root(),
    }
}

/// Identity-aware [`event_project_root`]: consults the registry so a
/// global-store-only checkout still resolves. Shared by every host whose session
/// events carry `cwd`.
#[hotpath::measure(future = true, label = "hosts.hooks.resolve_root")]
async fn event_project_root_with_identity(parsed: &Value) -> Option<PathBuf> {
    let cwd = event_cwd_from_parsed(parsed)?;
    crate::ports::hook_runtime::resolve_project_root_with_identity(&cwd).await
}

fn format_tool_hint(hint: &ToolHint) -> String {
    format!("tracedecay hint: {}\n{}", hint.message, hint.context)
}

fn append_tool_hint(context: &mut String, hint: &ToolHint) {
    if !context.ends_with('\n') {
        context.push('\n');
    }
    context.push_str(&format_tool_hint(hint));
    context.push('\n');
}

pub(crate) enum HookStdinRead {
    Event(String),
    /// Stdin exceeded [`tracedecay_framing::MAX_WIRE_MESSAGE_BYTES`].
    /// No payload bytes are retained.
    Oversized,
}

/// Read host-hook stdin with the host wire message byte cap enforced before
/// whole-body materialization.
pub(crate) fn read_stdin_bounded() -> std::io::Result<HookStdinRead> {
    read_stdin_bounded_from(&mut std::io::stdin().lock())
}

/// Testable stdin reader: streams until EOF while retaining at most
/// [`tracedecay_framing::MAX_WIRE_MESSAGE_BYTES`].
pub(crate) fn read_stdin_bounded_from(
    reader: &mut impl std::io::Read,
) -> std::io::Result<HookStdinRead> {
    use tracedecay_framing::{MAX_WIRE_MESSAGE_BYTES, WireReadOutcome, read_bounded_to_string};
    match read_bounded_to_string(reader, MAX_WIRE_MESSAGE_BYTES)? {
        WireReadOutcome::Ready(event) => Ok(HookStdinRead::Event(event)),
        WireReadOutcome::Oversized => Ok(HookStdinRead::Oversized),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod wire_stdin_bound_tests;

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod hint_analytics_tests;

#[cfg(test)]
mod tests;
