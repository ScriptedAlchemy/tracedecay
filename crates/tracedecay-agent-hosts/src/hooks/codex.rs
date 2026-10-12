//! Codex CLI hook handlers.
//!
//! Codex emits its own hook output shape.

use std::path::{Path, PathBuf};
use std::time::Instant;
use tracedecay_contracts::retrieval::HookRuntimeSurfaceRequestV1;

use serde_json::Value;

use crate::ports::hook_runtime::HookRuntimeV1;
use tracedecay_runtime_core::config::ProfileRoot;

use super::steering::{HookWorkspaceStatus, UNINDEXED_STATUS_LINE};
use super::tool_hints::{ToolHint, ToolHintInput, decide_hint};
use super::{
    additional_context_json, append_tool_hint, deduped_project_hint_with_id, event_cwd_from_parsed,
    event_project_root, event_project_root_from_json, event_project_root_with_identity,
    event_session_id, is_project_like_workspace, mint_hint_id, prompt_like_text, read_hook_event,
    record_hint_analytics, record_hook_invoked_parsed, record_workspace_status_analytics,
};
use tracedecay_domain::HostIntegrationIdV1;

/// Codex `SessionStart` hook handler.
#[tracing::instrument(name = "hosts.hooks.codex.session_start", level = "trace", skip_all)]
pub async fn hook_codex_session_start(runtime: &HookRuntimeV1) -> i32 {
    let started = Instant::now();
    let event = read_hook_event!(
        runtime,
        tracedecay_domain::NativeHostIdentityV1::Codex,
        "SessionStart"
    );
    let parsed = serde_json::from_str::<Value>(&event).unwrap_or(Value::Null);
    let root = event_project_root_with_identity(runtime, &parsed).await;
    let hook_telemetry = record_hook_invoked_parsed(
        runtime,
        root.as_deref(),
        HostIntegrationIdV1::Codex,
        "SessionStart",
        &event,
        &parsed,
    );
    let guidance = super::dispatch::dispatch_for_scope(
        runtime,
        tracedecay_domain::NativeHostIdentityV1::Codex,
        &event,
        root.as_deref(),
        Some(&hook_telemetry),
        started,
    )
    .await
    .into_recorded_guidance(&hook_telemetry)
    .flatten();
    let output = guidance.map_or_else(
        || serde_json::json!({}).to_string(),
        |guidance| additional_context_json("SessionStart", &guidance),
    );
    if !super::write_hook_output(
        &runtime.profile,
        root.as_deref(),
        tracedecay_domain::NativeHostIdentityV1::Codex,
        &event,
        &output,
    )
    .await
    {
        return 1;
    }
    0
}

/// Codex `UserPromptSubmit` hook handler.
///
/// Resets the local counter and injects steering context for the new turn.
#[tracing::instrument(
    name = "hosts.hooks.codex.user_prompt_submit",
    level = "trace",
    skip_all
)]
pub async fn hook_codex_user_prompt_submit(runtime: &HookRuntimeV1) -> i32 {
    let event = read_hook_event!(
        runtime,
        tracedecay_domain::NativeHostIdentityV1::Codex,
        "UserPromptSubmit"
    );
    let parsed = serde_json::from_str::<Value>(&event).unwrap_or(Value::Null);
    let root = event_project_root_with_identity(runtime, &parsed).await;
    // A compatibility prompt callback can run before TraceDecay is installed
    // for this profile. Only an existing profile can own projectless ingest.
    let profile = tracedecay_runtime_core::storage::read_existing_profile_identity_record(
        &runtime
            .profile
            .data_dir()
            .join(tracedecay_runtime_core::storage::PROFILE_IDENTITY_FILENAME),
    );
    match profile {
        Ok(None) => {
            return i32::from(
                !super::write_hook_output(
                    &runtime.profile,
                    None,
                    tracedecay_domain::NativeHostIdentityV1::Codex,
                    &event,
                    "{}",
                )
                .await,
            );
        }
        Err(error) => {
            tracing::warn!(%error, "Codex prompt profile identity is unavailable");
            return 1;
        }
        Ok(Some(_)) => {}
    }
    let hook_telemetry = record_hook_invoked_parsed(
        runtime,
        root.as_deref(),
        HostIntegrationIdV1::Codex,
        "UserPromptSubmit",
        &event,
        &parsed,
    );
    if let Some(root) = root.as_deref() {
        super::reset_counter_for_project(runtime, root, Some(&hook_telemetry)).await;
    }
    let session_id = event_session_id(&parsed);
    if root.is_none() {
        // Keep recall current, but wait for the native Stop receipt before
        // reflection so one completed turn schedules one review rather than a
        // prompt-only review followed immediately by a final-turn review.
        let _ =
            super::ingest_user_session(runtime, "Codex", session_id, Some(&hook_telemetry)).await;
    }
    let context = Box::pin(codex_user_prompt_submit_context_with_root(
        &runtime.profile,
        &parsed,
        root.as_deref(),
    ))
    .await;
    // A turn-local prompt with nothing to say emits no `hookSpecificOutput` at
    // all, exactly as `hook_codex_session_start` does for guidance-free
    // admission. An `additionalContext: ""` envelope is not "no context": Codex
    // treats it as an injected empty context block.
    let output = if context.is_empty() {
        serde_json::json!({}).to_string()
    } else {
        additional_context_json("UserPromptSubmit", &context)
    };
    if !super::write_hook_output(
        &runtime.profile,
        root.as_deref(),
        tracedecay_domain::NativeHostIdentityV1::Codex,
        &event,
        &output,
    )
    .await
    {
        return 1;
    }
    0
}

pub async fn codex_user_prompt_submit_context_for_event(
    runtime: &HookRuntimeV1,
    event: &str,
) -> String {
    let parsed = serde_json::from_str::<Value>(event).unwrap_or(Value::Null);
    let root = event_project_root_with_identity(runtime, &parsed).await;
    codex_user_prompt_submit_context_with_root(&runtime.profile, &parsed, root.as_deref()).await
}

/// [`codex_user_prompt_submit_context_for_event`] for handlers that already
/// resolved the identity-aware project root, so the registry probe runs once
/// per event.
async fn codex_user_prompt_submit_context_with_root(
    profile: &ProfileRoot,
    parsed: &Value,
    root: Option<&Path>,
) -> String {
    let cwd = event_cwd_from_parsed(parsed);
    let session_id = event_session_id(parsed);
    let status = codex_workspace_status(root, cwd.as_deref());
    record_workspace_status_analytics(profile.data_dir(), root, status, session_id.as_deref());
    let mut context = if matches!(status, HookWorkspaceStatus::UnindexedProject) {
        UNINDEXED_STATUS_LINE.to_string()
    } else {
        String::new()
    };
    if !matches!(status, HookWorkspaceStatus::Generic)
        && let Some(hint) = codex_prompt_hint(profile, parsed)
    {
        append_tool_hint(&mut context, &hint);
    }
    context
}

/// Codex `PostToolUse` hook handler.
///
/// The native event enters the canonical V2 admission/replay journey. Only
/// daemon-approved ready guidance is rendered in Codex's documented
/// `additionalContext` shape; unavailable or guidance-free admission is silent.
#[tracing::instrument(name = "hosts.hooks.codex.post_tool_use", level = "trace", skip_all)]
pub async fn hook_codex_post_tool_use(runtime: &HookRuntimeV1) -> i32 {
    let started = Instant::now();
    let event = read_hook_event!(
        runtime,
        tracedecay_domain::NativeHostIdentityV1::Codex,
        "PostToolUse"
    );
    // One parse supplies exact scope and analytics attribution.
    let parsed = serde_json::from_str::<Value>(&event).unwrap_or(Value::Null);
    let root = event_project_root_with_identity(runtime, &parsed).await;
    let hook_telemetry = record_hook_invoked_parsed(
        runtime,
        root.as_deref(),
        HostIntegrationIdV1::Codex,
        "PostToolUse",
        &event,
        &parsed,
    );
    let guidance = super::dispatch::dispatch_for_scope(
        runtime,
        tracedecay_domain::NativeHostIdentityV1::Codex,
        &event,
        root.as_deref(),
        Some(&hook_telemetry),
        started,
    )
    .await
    .into_recorded_guidance(&hook_telemetry)
    .flatten();
    if let Some(guidance) = guidance
        && !super::write_hook_output(
            &runtime.profile,
            root.as_deref(),
            tracedecay_domain::NativeHostIdentityV1::Codex,
            &event,
            &additional_context_json("PostToolUse", &guidance),
        )
        .await
    {
        return 1;
    }
    0
}

/// Codex `PostCompact` hook handler.
///
/// Codex exposes a pressure boundary but no authenticated compacted payload.
/// The daemon lands the session's rollout through the canonical transcript
/// ingest route and then runs the daemon-owned compression journey; the hook
/// itself only forwards the boundary and fails open.
#[tracing::instrument(name = "hosts.hooks.codex.post_compact", level = "trace", skip_all)]
pub async fn hook_codex_post_compact(runtime: &HookRuntimeV1) -> i32 {
    let event = read_hook_event!(
        runtime,
        tracedecay_domain::NativeHostIdentityV1::Codex,
        "PostCompact"
    );
    let parsed = serde_json::from_str::<Value>(&event).unwrap_or(Value::Null);
    let root = event_project_root_with_identity(runtime, &parsed).await;
    let hook_telemetry = record_hook_invoked_parsed(
        runtime,
        root.as_deref(),
        HostIntegrationIdV1::Codex,
        "PostCompact",
        &event,
        &parsed,
    );
    if std::env::var_os(
        tracedecay_sessions::runtime::hosts::codex_app_server::CODEX_SUMMARY_CHILD_ENV,
    )
    .is_none()
    {
        codex_post_compact(runtime, &event, Some(&hook_telemetry)).await;
    }
    if !super::write_hook_output(
        &runtime.profile,
        root.as_deref(),
        tracedecay_domain::NativeHostIdentityV1::Codex,
        &event,
        &serde_json::json!({}).to_string(),
    )
    .await
    {
        return 1;
    }
    0
}

/// Resolves the tracedecay project root for a Codex event from its `cwd`.
///
/// Kept as the published Codex-named entry point; the resolution itself is the
/// host-neutral `super::event_project_root` every `cwd`-carrying host shares.
pub fn codex_project_root_from_event(profile: &ProfileRoot, event_json: &str) -> Option<PathBuf> {
    event_project_root_from_json(profile, event_json)
}

fn codex_workspace_status(root: Option<&Path>, cwd: Option<&Path>) -> HookWorkspaceStatus {
    if root.is_some() {
        return HookWorkspaceStatus::Initialized;
    }
    if cwd.is_some_and(is_project_like_workspace) {
        HookWorkspaceStatus::UnindexedProject
    } else {
        HookWorkspaceStatus::Generic
    }
}

pub fn codex_workspace_status_from_event(
    profile: &ProfileRoot,
    event_json: &str,
) -> HookWorkspaceStatus {
    let parsed = serde_json::from_str::<Value>(event_json).unwrap_or(Value::Null);
    let root = event_project_root(profile, &parsed);
    let cwd = event_cwd_from_parsed(&parsed);
    codex_workspace_status(root.as_deref(), cwd.as_deref())
}

#[tracing::instrument(name = "hosts.hooks.codex.compact_daemon", level = "trace", skip_all)]
async fn codex_post_compact(
    runtime: &HookRuntimeV1,
    event_json: &str,
    telemetry: Option<&super::analytics::HookTimingSpan>,
) {
    let parsed = serde_json::from_str::<Value>(event_json).unwrap_or(Value::Null);
    let Some(root) = event_project_root_with_identity(runtime, &parsed).await else {
        return;
    };
    let args = HookRuntimeSurfaceRequestV1::CodexCompact {
        event_json: event_json.to_owned(),
    };
    if let Err(error) = super::daemon_hook_action(runtime, Some(&root), args, telemetry).await {
        tracing::warn!(%error, "Codex PostCompact daemon call failed");
    }
}

fn deduped_codex_hint(
    profile: &ProfileRoot,
    parsed: &Value,
    hint_id: &str,
    hint: ToolHint,
) -> Option<ToolHint> {
    deduped_project_hint_with_id(
        profile,
        event_project_root(profile, parsed).as_deref(),
        HostIntegrationIdV1::Codex,
        event_session_id(parsed),
        hint_id,
        hint,
    )
}

fn codex_prompt_hint(profile: &ProfileRoot, parsed: &Value) -> Option<ToolHint> {
    let hint = decide_hint(&ToolHintInput {
        agent: HostIntegrationIdV1::Codex,
        session_id: event_session_id(parsed),
        tool_name: None,
        command: None,
        prompt: prompt_like_text(parsed),
        subagent_type: None,
        file_path: None,
        captured_output: None,
        trusted_failure: false,
        edit_text: None,
        hints_enabled: true,
    })?;
    let root = event_project_root(profile, parsed);
    let hint_id = mint_hint_id();
    record_hint_analytics(
        profile.data_dir(),
        root.as_deref(),
        "hint_candidate",
        HostIntegrationIdV1::Codex,
        event_session_id(parsed).as_deref(),
        &hint_id,
        &hint,
    );
    deduped_codex_hint(profile, parsed, &hint_id, hint)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::super::tool_hints::HintCategory;
    use super::*;

    #[test]
    fn codex_prompt_hints_dedupe_by_session_and_category() {
        let project = tempfile::tempdir().unwrap();
        let profile_dir = tempfile::tempdir().unwrap();
        let project_root = project.path().canonicalize().unwrap();
        let profile_root = profile_dir.path().canonicalize().unwrap();
        let profile = ProfileRoot::new(&profile_root);
        tracedecay_runtime_core::storage::pin_fixture_repository_identity(
            &project_root,
            "proj_hook_codex_prompt",
        )
        .unwrap();
        let layout =
            tracedecay_runtime_core::storage::resolve_layout(&project_root, &profile_root).unwrap();
        std::fs::create_dir_all(&layout.data_root).unwrap();
        let event = serde_json::json!({
            "session_id": "codex-session-1",
            "cwd": project_root,
            "prompt": "Please explain the impact of changing parse_user"
        });

        let first = codex_prompt_hint(&profile, &event).unwrap();
        assert_eq!(first.category, HintCategory::Impact);

        assert!(
            codex_prompt_hint(&profile, &event).is_none(),
            "Codex should use shared per-session hint dedupe for prompt hints"
        );
    }
}
