//! Kiro hook handlers.
//!
//! Kiro sends hook event JSON on stdin; successful hook stdout is added to
//! context.

use std::time::Instant;

use serde_json::Value;

use crate::ports::hook_runtime::HookRuntimeV1;

use super::{event_project_root_or_process_cwd, read_hook_event, record_hook_invoked_parsed};
use tracedecay_domain::HostIntegrationIdV1;

/// Largest transcript tail the Kiro `userPromptSubmit` hook will read per call.
const KIRO_HOT_INGEST_MAX_BYTES: u64 = 256 * 1024;
/// Wall-clock budget for the Kiro prompt-submit catch-up ingest.
const KIRO_HOT_INGEST_BUDGET: std::time::Duration = std::time::Duration::from_millis(1_500);

/// Kiro `userPromptSubmit` hook handler.
///
/// Resets the per-turn counter, catches up transcripts, and injects bounded
/// user/project memory relevant to the submitted prompt.
#[tracing::instrument(
    name = "agent_hosts.hooks.kiro.prompt_submit",
    level = "trace",
    skip_all
)]
pub async fn hook_kiro_prompt_submit(runtime: &HookRuntimeV1) -> i32 {
    let started = Instant::now();
    let event = read_hook_event!(
        runtime,
        tracedecay_domain::NativeHostIdentityV1::Kiro,
        "userPromptSubmit"
    );
    let parsed = serde_json::from_str::<Value>(&event).unwrap_or(Value::Null);
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
                    tracedecay_domain::NativeHostIdentityV1::Kiro,
                    &event,
                    "{}",
                )
                .await,
            );
        }
        Err(error) => {
            tracing::warn!(%error, "Kiro prompt profile identity is unavailable");
            return 1;
        }
        Ok(Some(_)) => {}
    }
    let root = event_project_root_or_process_cwd(&runtime.profile, &parsed);
    let hook_telemetry = record_hook_invoked_parsed(
        runtime,
        root.as_deref(),
        HostIntegrationIdV1::Kiro,
        "userPromptSubmit",
        &event,
        &parsed,
    );
    let dispatch_guidance = if let Some(root) = root.as_deref() {
        super::dispatch::dispatch(
            runtime,
            tracedecay_domain::NativeHostIdentityV1::Kiro,
            &event,
            root,
            Some(&hook_telemetry),
            started,
        )
        .await
        .into_recorded_guidance(&hook_telemetry)
    } else {
        None
    };
    if let Some(root) = root.as_deref() {
        super::reset_counter_for_project(runtime, root, Some(&hook_telemetry)).await;
    }
    super::ingest_transcript_for_event(
        runtime,
        "kiro",
        &event,
        root.as_deref(),
        Some(KIRO_HOT_INGEST_MAX_BYTES),
        KIRO_HOT_INGEST_BUDGET,
        Some(&hook_telemetry),
    )
    .await;
    let output = dispatch_guidance
        .flatten()
        .unwrap_or_else(|| serde_json::json!({}).to_string());
    if !super::write_hook_output(
        &runtime.profile,
        root.as_deref(),
        tracedecay_domain::NativeHostIdentityV1::Kiro,
        &event,
        &output,
    )
    .await
    {
        return 1;
    }
    0
}
