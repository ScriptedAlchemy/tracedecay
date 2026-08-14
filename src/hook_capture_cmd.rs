use std::ffi::OsString;
use std::io::{Read, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use tracedecay_domain::UtcMicros;
use tracedecay_hooks::{HookHostV1, NativeHookCaptureOutcomeV1, NativeHookCaptureSourceV1};

use crate::cli::Commands;

const NATIVE_CAPTURE_COMMANDS: &[(&str, NativeHookCaptureSourceV1)] = &[
    (
        "hook-prompt-submit",
        NativeHookCaptureSourceV1::Host(HookHostV1::ClaudeCode),
    ),
    (
        "hook-stop",
        NativeHookCaptureSourceV1::Host(HookHostV1::ClaudeCode),
    ),
    (
        "hook-claude-session-start",
        NativeHookCaptureSourceV1::Host(HookHostV1::ClaudeCode),
    ),
    (
        "hook-claude-post-tool-use",
        NativeHookCaptureSourceV1::Host(HookHostV1::ClaudeCode),
    ),
    (
        "hook-claude-subagent-start",
        NativeHookCaptureSourceV1::Host(HookHostV1::ClaudeCode),
    ),
    (
        "hook-kiro-pre-tool-use",
        NativeHookCaptureSourceV1::Host(HookHostV1::Kiro),
    ),
    (
        "hook-kiro-prompt-submit",
        NativeHookCaptureSourceV1::Host(HookHostV1::Kiro),
    ),
    (
        "hook-kiro-post-tool-use",
        NativeHookCaptureSourceV1::Host(HookHostV1::Kiro),
    ),
    (
        "hook-cursor-subagent-start",
        NativeHookCaptureSourceV1::Host(HookHostV1::CursorDesktop),
    ),
    (
        "hook-cursor-post-tool-use",
        NativeHookCaptureSourceV1::Host(HookHostV1::CursorDesktop),
    ),
    (
        "hook-cursor-before-submit-prompt",
        NativeHookCaptureSourceV1::Host(HookHostV1::CursorDesktop),
    ),
    (
        "hook-cursor-pre-compact",
        NativeHookCaptureSourceV1::Host(HookHostV1::CursorDesktop),
    ),
    (
        "hook-cursor-after-file-edit",
        NativeHookCaptureSourceV1::Host(HookHostV1::CursorDesktop),
    ),
    (
        "hook-cursor-session-start",
        NativeHookCaptureSourceV1::Host(HookHostV1::CursorDesktop),
    ),
    (
        "hook-cursor-session-end",
        NativeHookCaptureSourceV1::Host(HookHostV1::CursorDesktop),
    ),
    (
        "hook-cursor-after-shell",
        NativeHookCaptureSourceV1::Host(HookHostV1::CursorDesktop),
    ),
    (
        "hook-cursor-workspace-open",
        NativeHookCaptureSourceV1::Host(HookHostV1::CursorDesktop),
    ),
    (
        "hook-cursor-stop",
        NativeHookCaptureSourceV1::Host(HookHostV1::CursorDesktop),
    ),
    (
        "hook-codex-session-start",
        NativeHookCaptureSourceV1::Host(HookHostV1::Codex),
    ),
    (
        "hook-codex-user-prompt-submit",
        NativeHookCaptureSourceV1::Host(HookHostV1::Codex),
    ),
    (
        "hook-codex-subagent-start",
        NativeHookCaptureSourceV1::Host(HookHostV1::Codex),
    ),
    (
        "hook-codex-post-tool-use",
        NativeHookCaptureSourceV1::Host(HookHostV1::Codex),
    ),
    (
        "hook-codex-stop",
        NativeHookCaptureSourceV1::Host(HookHostV1::Codex),
    ),
    (
        "hook-hermes-terminal-receipt",
        NativeHookCaptureSourceV1::Host(HookHostV1::Hermes),
    ),
    (
        "hook-kimi-event",
        NativeHookCaptureSourceV1::Host(HookHostV1::KimiCode),
    ),
    (
        "hook-opencode-event",
        NativeHookCaptureSourceV1::Host(HookHostV1::OpenCode),
    ),
    (
        "hook-opencode-tool-after",
        NativeHookCaptureSourceV1::OpenCodeToolExecuteAfter,
    ),
];

pub(crate) fn try_run(args: &[OsString]) -> Option<i32> {
    let command = args.get(1)?.to_str()?;
    // Native callbacks must never enter normal CLI startup: that path owns
    // lifecycle maintenance and may open product state before the daemon has
    // admitted the observation.
    if command == "hook-pre-tool-use" {
        // Claude's pre-tool callback has no replay-safe native observation.
        // An empty successful response preserves the host's normal allow path
        // without reviving the removed hook-local policy authority.
        return (args.len() == 2).then_some(0).or(Some(1));
    }
    // Hooks with a provider-supported synchronous response must enter the
    // async composition root: their existing handlers perform the canonical
    // V2 admission/replay journey and render only daemon-approved guidance.
    // The remaining native callbacks stay on the capture-only fast path.
    if native_response_command_from_name(command) {
        return None;
    }
    let source = capture_source_from_name(command)?;
    (args.len() == 2)
        .then(|| run_native_capture(source))
        .or(Some(1))
}

pub(crate) fn is_native_hook_command(command: &Commands) -> bool {
    matches!(command, Commands::HookPreToolUse) || capture_source_for_command(command).is_some()
}

pub(crate) fn capture_source_for_command(command: &Commands) -> Option<NativeHookCaptureSourceV1> {
    capture_command_name(command).and_then(capture_source_from_name)
}

fn capture_source_from_name(command: &str) -> Option<NativeHookCaptureSourceV1> {
    NATIVE_CAPTURE_COMMANDS
        .iter()
        .find_map(|(name, source)| (*name == command).then_some(*source))
}

fn native_response_command_from_name(command: &str) -> bool {
    matches!(
        command,
        "hook-stop"
            | "hook-claude-session-start"
            | "hook-claude-post-tool-use"
            | "hook-cursor-session-start"
            | "hook-cursor-post-tool-use"
            | "hook-codex-session-start"
            | "hook-codex-post-tool-use"
            | "hook-hermes-terminal-receipt"
            | "hook-kiro-prompt-submit"
            | "hook-kimi-event"
            | "hook-opencode-event"
            | "hook-opencode-tool-after"
    )
}

fn capture_command_name(command: &Commands) -> Option<&'static str> {
    match command {
        Commands::HookPromptSubmit => Some("hook-prompt-submit"),
        Commands::HookStop => Some("hook-stop"),
        Commands::HookClaudeSessionStart => Some("hook-claude-session-start"),
        Commands::HookClaudePostToolUse => Some("hook-claude-post-tool-use"),
        Commands::HookClaudeSubagentStart => Some("hook-claude-subagent-start"),
        Commands::HookKiroPreToolUse => Some("hook-kiro-pre-tool-use"),
        Commands::HookKiroPromptSubmit => Some("hook-kiro-prompt-submit"),
        Commands::HookKiroPostToolUse => Some("hook-kiro-post-tool-use"),
        Commands::HookCursorSubagentStart => Some("hook-cursor-subagent-start"),
        Commands::HookCursorPostToolUse => Some("hook-cursor-post-tool-use"),
        Commands::HookCursorBeforeSubmitPrompt => Some("hook-cursor-before-submit-prompt"),
        Commands::HookCursorPreCompact => Some("hook-cursor-pre-compact"),
        Commands::HookCursorAfterFileEdit => Some("hook-cursor-after-file-edit"),
        Commands::HookCursorSessionStart => Some("hook-cursor-session-start"),
        Commands::HookCursorSessionEnd => Some("hook-cursor-session-end"),
        Commands::HookCursorAfterShell => Some("hook-cursor-after-shell"),
        Commands::HookCursorWorkspaceOpen => Some("hook-cursor-workspace-open"),
        Commands::HookCursorStop => Some("hook-cursor-stop"),
        Commands::HookCodexSessionStart => Some("hook-codex-session-start"),
        Commands::HookCodexUserPromptSubmit => Some("hook-codex-user-prompt-submit"),
        Commands::HookCodexSubagentStart => Some("hook-codex-subagent-start"),
        Commands::HookCodexPostToolUse => Some("hook-codex-post-tool-use"),
        Commands::HookCodexStop => Some("hook-codex-stop"),
        Commands::HookHermesTerminalReceipt => Some("hook-hermes-terminal-receipt"),
        Commands::HookKimiEvent => Some("hook-kimi-event"),
        Commands::HookOpenCodeEvent => Some("hook-opencode-event"),
        Commands::HookOpenCodeToolAfter => Some("hook-opencode-tool-after"),
        _ => None,
    }
}

pub(crate) fn run_native_capture(source: NativeHookCaptureSourceV1) -> i32 {
    let payload = match read_bounded_stdin() {
        Ok(payload) => payload,
        Err(()) => return 1,
    };
    let mut delivery_writer = None;
    let mut delivery_open_error = None;
    let mut delivery_material = None;
    let outcome = match std::env::current_dir() {
        Ok(project_root) => {
            match tracedecay_runtime_core::storage::resolve_enrolled_layout_for_current_profile(
                &project_root,
            ) {
                Ok(Some(layout)) => match current_time() {
                    Some(now) => {
                        match tracedecay::hooks::native_capture_material(source, &payload, now) {
                            Ok(material) => {
                                let outcome = tracedecay_hooks::capture_native_event_for_replay(
                                    &layout.data_root,
                                    source,
                                    &payload,
                                    material,
                                    now,
                                );
                                if outcome == NativeHookCaptureOutcomeV1::Captured {
                                    match tracedecay_hooks::HookDeliveryReceiptSpoolV1::open(
                                        tracedecay_hooks::hook_delivery_receipt_spool_root(
                                            &layout.data_root,
                                            source.host(),
                                        ),
                                    ) {
                                        Ok(writer) => delivery_writer = Some(writer),
                                        Err(error) => delivery_open_error = Some(error),
                                    }
                                    delivery_material = Some(material);
                                }
                                outcome
                            }
                            Err(
                                tracedecay_hooks::NativeHookDecodeError::UnsupportedNativeEvent
                                | tracedecay_hooks::NativeHookDecodeError::UnsupportedNativeFamily,
                            ) => NativeHookCaptureOutcomeV1::Unsupported,
                            Err(_) => NativeHookCaptureOutcomeV1::Rejected,
                        }
                    }
                    None => NativeHookCaptureOutcomeV1::Unavailable,
                },
                Ok(None) => NativeHookCaptureOutcomeV1::Unbound,
                Err(_) => NativeHookCaptureOutcomeV1::Unavailable,
            }
        }
        Err(_) => NativeHookCaptureOutcomeV1::Unavailable,
    };

    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    if stdout
        .write_all(b"{}\n")
        .and_then(|()| stdout.flush())
        .is_err()
    {
        return 1;
    }
    drop(stdout);
    if outcome == NativeHookCaptureOutcomeV1::Captured {
        let Some(writer) = delivery_writer else {
            if let Some(error) = delivery_open_error {
                eprintln!("tracedecay hook: delivery receipt spool unavailable: {error}");
            } else {
                eprintln!("tracedecay hook: delivery receipt writer unavailable");
            }
            return 1;
        };
        let (Some(material), Some(delivered_at)) = (delivery_material, current_time()) else {
            return 1;
        };
        let Some(settlement) = native_hook_delivery_settlement(source, material, delivered_at)
        else {
            return 1;
        };
        let Ok(receipt) = tracedecay_hooks::HookDeliverySourceReceiptV1::new(settlement) else {
            return 1;
        };
        if writer.append(&receipt).is_err() {
            return 1;
        }
    }
    match outcome {
        NativeHookCaptureOutcomeV1::Captured
        | NativeHookCaptureOutcomeV1::Unsupported
        | NativeHookCaptureOutcomeV1::Unbound => 0,
        NativeHookCaptureOutcomeV1::Rejected
        | NativeHookCaptureOutcomeV1::Full
        | NativeHookCaptureOutcomeV1::ResetRequired
        | NativeHookCaptureOutcomeV1::Unavailable => 1,
    }
}

fn native_hook_delivery_settlement(
    source: NativeHookCaptureSourceV1,
    material: tracedecay_hooks::NativeEnvelopeMaterialV1,
    delivered_at: UtcMicros,
) -> Option<tracedecay_domain::DeliverySettlementV1> {
    let host = source.host();
    let owner = tracedecay_domain::canonical_sha256(&(
        "tracedecay.native-hook-output-delivery.v1",
        host.hook_key(),
        material.event_id,
    ))
    .ok()?;
    let channel = tracedecay_domain::canonical_sha256(&(
        "tracedecay.native-hook-output-channel.v1",
        host.hook_key(),
        material.protected_session_id,
    ))
    .ok()?;
    let attempted_at = std::cmp::max(material.observed_at, delivered_at);
    Some(tracedecay_domain::DeliverySettlementV1 {
        attempt: tracedecay_domain::DeliverySettlementAttemptV1 {
            owner_event_id: format!(
                "hook:native:{}",
                owner.as_str().trim_start_matches("sha256:")
            ),
            event_class: tracedecay_domain::DeliveryEventClassV1::Activity,
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
            valid_at: material.observed_at,
            attempted_at,
        },
        outcome: tracedecay_domain::DeliverySettlementOutcomeV1::Delivered,
        settled_at: attempted_at,
        drop_reason: None,
    })
}

fn read_bounded_stdin() -> Result<Vec<u8>, ()> {
    let bound = tracedecay_hooks::MAX_HOOK_PAYLOAD_BYTES;
    let mut payload = Vec::with_capacity(bound);
    std::io::stdin()
        .lock()
        .take((bound + 1) as u64)
        .read_to_end(&mut payload)
        .map_err(|_| ())?;
    (payload.len() <= bound).then_some(payload).ok_or(())
}

fn current_time() -> Option<UtcMicros> {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    let micros = i64::try_from(elapsed.as_micros()).ok()?;
    Some(UtcMicros(micros))
}
