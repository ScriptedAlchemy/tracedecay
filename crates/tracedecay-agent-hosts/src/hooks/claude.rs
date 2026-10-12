//! Claude Code hook handlers.
//!
//! Claude and Codex share the common hook JSON shape.

use std::path::PathBuf;
use std::time::Instant;

use serde_json::Value;
use tracedecay_contracts::retrieval::HookRuntimeSurfaceRequestV1;

use crate::ports::hook_runtime::HookRuntimeV1;

use super::post_tool_use::is_post_tool_use_failure_event;
use super::{
    additional_context_json, event_project_root_with_identity, read_hook_event,
    record_hook_invoked_parsed,
};
use tracedecay_domain::HostIntegrationIdV1;

pub(super) fn is_code_research_prompt(prompt: &str) -> bool {
    let lower = prompt.to_ascii_lowercase();
    let exploration_patterns = [
        "explore",
        "codebase structure",
        "codebase architecture",
        "codebase overview",
        "source files contents",
        "read every",
        "full contents",
        "entire codebase",
        "architecture and structure",
        "call graph",
        "call path",
        "call chain",
        "symbol relat",
        "symbol lookup",
        "who calls",
        "callers of",
        "callees of",
    ];
    exploration_patterns.iter().any(|pat| lower.contains(pat))
}

/// Claude Code `SessionStart` hook handler.
#[tracing::instrument(name = "hosts.hooks.claude.session_start", level = "trace", skip_all)]
pub async fn hook_claude_session_start(runtime: &HookRuntimeV1) -> i32 {
    claude_guidance_hook(runtime, "SessionStart").await
}

/// Claude Code `PostCompact` hook handler.
///
/// Claude does not currently expose machine-verifiable provenance for the
/// compacted source frontier. The daemon therefore treats this event as a
/// read-only capability probe and returns typed unavailable without publishing
/// transcript or summary state.
#[tracing::instrument(name = "hosts.hooks.claude.post_compact", level = "trace", skip_all)]
pub async fn hook_claude_post_compact(runtime: &HookRuntimeV1) -> i32 {
    let event = read_hook_event!(
        runtime,
        tracedecay_domain::NativeHostIdentityV1::ClaudeCode,
        "PostCompact"
    );
    let parsed = serde_json::from_str::<Value>(&event).unwrap_or(Value::Null);
    let root = event_project_root_with_identity(runtime, &parsed).await;
    let hook_telemetry = record_hook_invoked_parsed(
        runtime,
        root.as_deref(),
        HostIntegrationIdV1::Claude,
        "PostCompact",
        &event,
        &parsed,
    );
    let args = HookRuntimeSurfaceRequestV1::ClaudeCompact {
        event_json: event.clone(),
        user_scope: root.is_none(),
    };
    if let Err(error) =
        super::daemon_hook_action(runtime, root.as_deref(), args, Some(&hook_telemetry)).await
    {
        tracing::warn!(%error, "Claude PostCompact daemon call failed");
    }
    if !super::write_hook_output(
        &runtime.profile,
        root.as_deref(),
        tracedecay_domain::NativeHostIdentityV1::ClaudeCode,
        &event,
        &serde_json::json!({}).to_string(),
    )
    .await
    {
        return 1;
    }
    0
}

/// Claude Code `PostToolUse` / `PostToolUseFailure` hook handler.
#[tracing::instrument(name = "hosts.hooks.claude.post_tool_use", level = "trace", skip_all)]
pub async fn hook_claude_post_tool_use(runtime: &HookRuntimeV1) -> i32 {
    let started = Instant::now();
    let event = read_hook_event!(
        runtime,
        tracedecay_domain::NativeHostIdentityV1::ClaudeCode,
        "PostToolUse"
    );
    let (root, response) = claude_post_tool_use_response(runtime, &event, started).await;
    if let Some(response) = response
        && !super::write_hook_output(
            &runtime.profile,
            root.as_deref(),
            tracedecay_domain::NativeHostIdentityV1::ClaudeCode,
            &event,
            &response,
        )
        .await
    {
        return 1;
    }
    0
}

/// Returns the identity-resolved root alongside the response so the handler
/// does not repeat the registry-probing resolution for output delivery.
async fn claude_post_tool_use_response(
    runtime: &HookRuntimeV1,
    event: &str,
    started: Instant,
) -> (Option<PathBuf>, Option<String>) {
    let parsed = serde_json::from_str::<Value>(event).unwrap_or(Value::Null);
    let hook_event_name = if is_post_tool_use_failure_event(&parsed) {
        "PostToolUseFailure"
    } else {
        "PostToolUse"
    };
    let root = event_project_root_with_identity(runtime, &parsed).await;
    let hook_telemetry = record_hook_invoked_parsed(
        runtime,
        root.as_deref(),
        HostIntegrationIdV1::Claude,
        hook_event_name,
        event,
        &parsed,
    );
    let response = super::dispatch::dispatch_for_scope(
        runtime,
        tracedecay_domain::NativeHostIdentityV1::ClaudeCode,
        event,
        root.as_deref(),
        Some(&hook_telemetry),
        started,
    )
    .await
    .into_recorded_guidance(&hook_telemetry)
    .flatten()
    .map(|guidance| additional_context_json(hook_event_name, &guidance));
    (root, response)
}

/// `Stop` hook handler: submits the native turn boundary to the daemon.
#[tracing::instrument(name = "hosts.hooks.claude.stop", level = "trace", skip_all)]
pub async fn hook_stop(runtime: &HookRuntimeV1) -> i32 {
    claude_guidance_hook(runtime, "Stop").await
}

/// SessionStart and Stop share one guidance envelope. The project root is
/// resolved once and reused for both dispatch and stdout delivery.
async fn claude_guidance_hook(runtime: &HookRuntimeV1, hook_name: &'static str) -> i32 {
    let started = Instant::now();
    let event = read_hook_event!(
        runtime,
        tracedecay_domain::NativeHostIdentityV1::ClaudeCode,
        hook_name
    );
    let parsed = serde_json::from_str::<Value>(&event).unwrap_or(Value::Null);
    let root = event_project_root_with_identity(runtime, &parsed).await;
    let hook_telemetry = record_hook_invoked_parsed(
        runtime,
        root.as_deref(),
        HostIntegrationIdV1::Claude,
        hook_name,
        &event,
        &parsed,
    );
    let output = super::dispatch::dispatch_for_scope(
        runtime,
        tracedecay_domain::NativeHostIdentityV1::ClaudeCode,
        &event,
        root.as_deref(),
        Some(&hook_telemetry),
        started,
    )
    .await
    .into_recorded_guidance(&hook_telemetry)
    .flatten()
    .map_or_else(
        || serde_json::json!({}).to_string(),
        |guidance| additional_context_json(hook_name, &guidance),
    );
    if !super::write_hook_output(
        &runtime.profile,
        root.as_deref(),
        tracedecay_domain::NativeHostIdentityV1::ClaudeCode,
        &event,
        &output,
    )
    .await
    {
        return 1;
    }
    0
}
