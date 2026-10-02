use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use tracedecay_runtime_core::config::ProfileRoot;

use tracedecay_domain::NativeHostIdentityV1;
use tracedecay_domain::UtcMicros;
use tracedecay_hooks::delivery_spool::{
    HookDeliveryReceiptOutcomeV1, HookDeliveryReceiptRefusalV1,
};
use tracedecay_hooks::{NativeHookCaptureOutcomeV1, NativeHookCaptureSourceV1};

use crate::cli::Commands;

/// Every native hook subcommand, the source it captures from, and the host
/// event it answers when that subcommand is specific to one event.
const NATIVE_CAPTURE_COMMANDS: &[(&str, NativeHookCaptureSourceV1, Option<&str>)] = &[
    (
        "hook-prompt-submit",
        NativeHookCaptureSourceV1::Host(NativeHostIdentityV1::ClaudeCode),
        Some("UserPromptSubmit"),
    ),
    (
        "hook-stop",
        NativeHookCaptureSourceV1::Host(NativeHostIdentityV1::ClaudeCode),
        Some("Stop"),
    ),
    (
        "hook-claude-session-start",
        NativeHookCaptureSourceV1::Host(NativeHostIdentityV1::ClaudeCode),
        Some("SessionStart"),
    ),
    (
        "hook-claude-post-tool-use",
        NativeHookCaptureSourceV1::Host(NativeHostIdentityV1::ClaudeCode),
        Some("PostToolUse"),
    ),
    (
        "hook-claude-subagent-start",
        NativeHookCaptureSourceV1::Host(NativeHostIdentityV1::ClaudeCode),
        Some("SubagentStart"),
    ),
    (
        "hook-kiro-pre-tool-use",
        NativeHookCaptureSourceV1::Host(NativeHostIdentityV1::Kiro),
        Some("preToolUse"),
    ),
    (
        "hook-kiro-prompt-submit",
        NativeHookCaptureSourceV1::Host(NativeHostIdentityV1::Kiro),
        Some("userPromptSubmit"),
    ),
    (
        "hook-kiro-post-tool-use",
        NativeHookCaptureSourceV1::Host(NativeHostIdentityV1::Kiro),
        Some("postToolUse"),
    ),
    (
        "hook-cursor-subagent-start",
        NativeHookCaptureSourceV1::Host(NativeHostIdentityV1::CursorDesktop),
        Some("subagentStart"),
    ),
    (
        "hook-cursor-post-tool-use",
        NativeHookCaptureSourceV1::Host(NativeHostIdentityV1::CursorDesktop),
        Some("postToolUse"),
    ),
    (
        "hook-cursor-before-submit-prompt",
        NativeHookCaptureSourceV1::Host(NativeHostIdentityV1::CursorDesktop),
        Some("beforeSubmitPrompt"),
    ),
    (
        "hook-cursor-pre-compact",
        NativeHookCaptureSourceV1::Host(NativeHostIdentityV1::CursorDesktop),
        Some("preCompact"),
    ),
    (
        "hook-cursor-after-file-edit",
        NativeHookCaptureSourceV1::Host(NativeHostIdentityV1::CursorDesktop),
        Some("afterFileEdit"),
    ),
    (
        "hook-cursor-session-start",
        NativeHookCaptureSourceV1::Host(NativeHostIdentityV1::CursorDesktop),
        Some("sessionStart"),
    ),
    (
        "hook-cursor-session-end",
        NativeHookCaptureSourceV1::Host(NativeHostIdentityV1::CursorDesktop),
        Some("sessionEnd"),
    ),
    (
        "hook-cursor-after-shell",
        NativeHookCaptureSourceV1::Host(NativeHostIdentityV1::CursorDesktop),
        Some("afterShellExecution"),
    ),
    (
        "hook-cursor-workspace-open",
        NativeHookCaptureSourceV1::Host(NativeHostIdentityV1::CursorDesktop),
        Some("workspaceOpen"),
    ),
    (
        "hook-cursor-stop",
        NativeHookCaptureSourceV1::Host(NativeHostIdentityV1::CursorDesktop),
        Some("stop"),
    ),
    (
        "hook-codex-session-start",
        NativeHookCaptureSourceV1::Host(NativeHostIdentityV1::Codex),
        Some("SessionStart"),
    ),
    (
        "hook-codex-user-prompt-submit",
        NativeHookCaptureSourceV1::Host(NativeHostIdentityV1::Codex),
        Some("UserPromptSubmit"),
    ),
    (
        "hook-codex-subagent-start",
        NativeHookCaptureSourceV1::Host(NativeHostIdentityV1::Codex),
        Some("SubagentStart"),
    ),
    (
        "hook-codex-post-tool-use",
        NativeHookCaptureSourceV1::Host(NativeHostIdentityV1::Codex),
        Some("PostToolUse"),
    ),
    (
        "hook-codex-stop",
        NativeHookCaptureSourceV1::Host(NativeHostIdentityV1::Codex),
        Some("Stop"),
    ),
    (
        "hook-hermes-terminal-receipt",
        NativeHookCaptureSourceV1::Host(NativeHostIdentityV1::Hermes),
        None,
    ),
    (
        "hook-kimi-event",
        NativeHookCaptureSourceV1::Host(NativeHostIdentityV1::KimiCode),
        None,
    ),
    (
        "hook-opencode-event",
        NativeHookCaptureSourceV1::Host(NativeHostIdentityV1::OpenCode),
        None,
    ),
    (
        "hook-opencode-tool-after",
        NativeHookCaptureSourceV1::OpenCodeToolExecuteAfter,
        Some(tracedecay_agent_hosts::hooks::OPENCODE_TOOL_EXECUTE_AFTER_HOOK_NAME),
    ),
    (
        "hook-pi-event",
        NativeHookCaptureSourceV1::Host(NativeHostIdentityV1::Pi),
        None,
    ),
    (
        "hook-droid-event",
        NativeHookCaptureSourceV1::Host(NativeHostIdentityV1::FactoryDroid),
        None,
    ),
];

/// Native hook callbacks are their own process boundary: each resolves its
/// profile from the environment here, before any hook authority runs.
pub(crate) fn try_run(args: &[OsString]) -> Option<i32> {
    let command = args.get(1)?.to_str()?;
    // Native callbacks must never enter normal CLI startup: that path owns
    // lifecycle maintenance and may open product state before the daemon has
    // admitted the observation.
    if command == "hook-pre-tool-use" {
        if args.len() != 2 {
            return Some(refused("hook callbacks take no arguments"));
        }
        // Claude's pre-tool callback has no replay-safe native observation.
        // An empty successful response preserves the host's normal allow path
        // without reviving the removed hook-local policy authority. The
        // invocation itself is still adoption telemetry, and `TOOL_INPUT`
        // carries no event name, so the hook name is supplied here. The
        // allow response never depends on it: a process without a profile
        // records nothing and still allows the tool.
        match ProfileRoot::from_env() {
            Ok(profile) => drop(
                tracedecay_agent_hosts::hooks::record_native_capture_invoked(
                    &tracedecay::hook_runtime(profile),
                    std::env::current_dir().ok().as_deref(),
                    NativeHostIdentityV1::ClaudeCode,
                    Some("preToolUse"),
                    &std::env::var("TOOL_INPUT").unwrap_or_default(),
                ),
            ),
            Err(error) => tracing::debug!(%error, "preToolUse invocation not recorded"),
        }
        return Some(0);
    }
    // Hooks with a provider-supported synchronous response must enter the
    // async composition root: their existing handlers perform the canonical
    // V2 admission/replay journey and render only daemon-approved guidance.
    // The remaining native callbacks stay on the capture-only fast path.
    if native_response_command_from_name(command) {
        return None;
    }
    let (source, hook_name) = capture_command_from_name(command)?;
    Some(if args.len() == 2 {
        match ProfileRoot::from_env() {
            Ok(profile) => run_native_capture(&profile, source, hook_name),
            Err(error) => refused(error),
        }
    } else {
        refused("hook callbacks take no arguments")
    })
}

#[cfg(any(feature = "hotpath", test))]
pub(crate) fn is_hook_protocol_invocation(args: &[OsString]) -> bool {
    args.get(1)
        .and_then(|value| value.to_str())
        .is_some_and(|command| command.starts_with("hook-"))
}

pub(crate) fn is_native_hook_command(command: &Commands) -> bool {
    matches!(command, Commands::HookPreToolUse) || capture_command_for(command).is_some()
}

/// The capture source of a native hook subcommand and the host event it
/// names, when it names one.
pub(crate) fn capture_command_for(
    command: &Commands,
) -> Option<(NativeHookCaptureSourceV1, Option<&'static str>)> {
    capture_command_name(command).and_then(capture_command_from_name)
}

fn capture_command_from_name(
    command: &str,
) -> Option<(NativeHookCaptureSourceV1, Option<&'static str>)> {
    NATIVE_CAPTURE_COMMANDS
        .iter()
        .find_map(|(name, source, event)| (*name == command).then_some((*source, *event)))
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
            | "hook-codex-user-prompt-submit"
            | "hook-codex-post-tool-use"
            | "hook-hermes-terminal-receipt"
            | "hook-kiro-prompt-submit"
            | "hook-kimi-event"
            | "hook-opencode-event"
            | "hook-opencode-tool-after"
            | "hook-pi-event"
            | "hook-droid-event"
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
        Commands::HookPiEvent => Some("hook-pi-event"),
        Commands::HookDroidEvent => Some("hook-droid-event"),
        _ => None,
    }
}

/// Every bounded lock wait on the capture path gets one synchronous budget
/// measured from its own lock attempt, not from hook start: the analytics
/// row, enrolled-layout lookup, decode, and spool-root creation that precede
/// admission must not spend the budget an uncontended spool lock would then
/// be refused for. The response hooks' output write waits the same way.
struct PreparedNativeCapture {
    outcome: NativeHookCaptureOutcomeV1,
    /// The spooled event's data root and material, retained for its delivery
    /// receipt.
    delivery: Option<(
        std::path::PathBuf,
        tracedecay_hooks::NativeEnvelopeMaterialV1,
    )>,
    /// Why a refused capture did not land, beyond what its outcome names.
    cause: Option<String>,
}

impl PreparedNativeCapture {
    fn plain(outcome: NativeHookCaptureOutcomeV1) -> Self {
        Self {
            outcome,
            delivery: None,
            cause: None,
        }
    }

    fn scope_unavailable(cause: String) -> Self {
        Self {
            cause: Some(cause),
            ..Self::plain(NativeHookCaptureOutcomeV1::ScopeUnavailable)
        }
    }
}

fn prepare_native_capture(
    profile: &ProfileRoot,
    source: NativeHookCaptureSourceV1,
    payload: &[u8],
    working_directory: &std::io::Result<std::path::PathBuf>,
) -> PreparedNativeCapture {
    let project_root = match working_directory {
        Ok(project_root) => project_root,
        Err(error) => {
            return PreparedNativeCapture::scope_unavailable(format!(
                "working directory is unavailable: {error}"
            ));
        }
    };
    let layout = match tracedecay_runtime_core::storage::resolve_persisted_layout(
        project_root,
        profile.data_dir(),
    ) {
        Ok(Some(layout)) => layout,
        Ok(None) => return PreparedNativeCapture::plain(NativeHookCaptureOutcomeV1::Unbound),
        Err(error) => {
            return PreparedNativeCapture::scope_unavailable(format!(
                "project layout could not be resolved: {error}"
            ));
        }
    };
    let worktree_id = match tracedecay_agent_hosts::hooks::hook_worktree_id_for_layout(
        &tracedecay::hook_runtime(profile.clone()),
        &layout,
    ) {
        Ok(worktree_id) => worktree_id,
        Err(error) => {
            return PreparedNativeCapture::scope_unavailable(format!(
                "worktree identity is unavailable: {error}"
            ));
        }
    };
    let Some(now) = current_time() else {
        return PreparedNativeCapture::scope_unavailable(
            "the system clock is before the Unix epoch".to_owned(),
        );
    };
    match tracedecay_agent_hosts::hooks::native_capture_material(source, payload, now) {
        Ok(material) => {
            let outcome = tracedecay_hooks::capture_native_event_for_replay(
                &layout.data_root,
                worktree_id,
                source,
                payload,
                material,
                now,
                tracedecay_hooks::HOOK_SYNCHRONOUS_BUDGET,
            );
            let delivery = (outcome == NativeHookCaptureOutcomeV1::Captured)
                .then_some((layout.data_root, material));
            PreparedNativeCapture {
                outcome,
                delivery,
                cause: None,
            }
        }
        Err(
            tracedecay_hooks::NativeHookDecodeError::UnsupportedNativeEvent
            | tracedecay_hooks::NativeHookDecodeError::UnsupportedNativeFamily,
        ) => PreparedNativeCapture::plain(NativeHookCaptureOutcomeV1::Unsupported),
        Err(error) => PreparedNativeCapture {
            cause: Some(error.to_string()),
            ..PreparedNativeCapture::plain(NativeHookCaptureOutcomeV1::Rejected)
        },
    }
}

/// `hook_name` is the host event the subcommand names, when it names one.
pub(crate) fn run_native_capture(
    profile: &ProfileRoot,
    source: NativeHookCaptureSourceV1,
    hook_name: Option<&str>,
) -> i32 {
    let runtime = tracedecay::hook_runtime(profile.clone());
    let working_directory = std::env::current_dir();
    let payload = match read_bounded_stdin() {
        Ok(payload) => payload,
        Err(refusal) => {
            tracedecay_agent_hosts::hooks::record_native_capture_stdin_refused(
                &runtime,
                working_directory.as_deref().ok(),
                source.host(),
                hook_name,
                matches!(refusal, StdinRefusal::Oversized),
            );
            return refused(refusal);
        }
    };
    // The invocation is analytics-visible whatever the capture outcome: an
    // unbound, unsupported, or rejected callback still proves the host fired
    // the hook, which is the one thing adoption telemetry must not lose.
    let telemetry = tracedecay_agent_hosts::hooks::record_native_capture_invoked(
        &runtime,
        working_directory.as_deref().ok(),
        source.host(),
        hook_name,
        &String::from_utf8_lossy(&payload),
    );
    let prepared = prepare_native_capture(profile, source, &payload, &working_directory);

    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    if stdout
        .write_all(b"{}\n")
        .and_then(|()| stdout.flush())
        .is_err()
    {
        return refused("hook response could not be written to stdout");
    }
    drop(stdout);
    let receipt = prepared
        .delivery
        .as_ref()
        .map(|(data_root, material)| retain_delivery_receipt(data_root, source, *material));
    telemetry.note_capture_outcome(&prepared.outcome, receipt.as_ref());
    match prepared.outcome {
        NativeHookCaptureOutcomeV1::Captured
        | NativeHookCaptureOutcomeV1::Unsupported
        | NativeHookCaptureOutcomeV1::Unbound => 0,
        NativeHookCaptureOutcomeV1::Rejected
        | NativeHookCaptureOutcomeV1::Full
        | NativeHookCaptureOutcomeV1::ResetRequired(_)
        | NativeHookCaptureOutcomeV1::Unavailable(_)
        | NativeHookCaptureOutcomeV1::ScopeUnavailable
        | NativeHookCaptureOutcomeV1::AdmissionTimedOut => refused(match prepared.cause {
            Some(cause) => format!(
                "native capture did not land: {} ({cause})",
                prepared.outcome
            ),
            None => format!("native capture did not land: {}", prepared.outcome),
        }),
    }
}

/// Retains the delivery receipt of an event that already spooled. The writer
/// is opened only here, after the host response, so it is held for exactly
/// one append. A refusal loses the receipt, not the event, and is recorded on
/// the invocation's `hook_completed` row by the caller.
fn retain_delivery_receipt(
    data_root: &Path,
    source: NativeHookCaptureSourceV1,
    material: tracedecay_hooks::NativeEnvelopeMaterialV1,
) -> HookDeliveryReceiptOutcomeV1 {
    let Some(settlement) = current_time()
        .and_then(|delivered_at| native_hook_delivery_settlement(source, material, delivered_at))
    else {
        return HookDeliveryReceiptOutcomeV1::Refused {
            reason: HookDeliveryReceiptRefusalV1::IdentityUnavailable,
        };
    };
    match tracedecay_hooks::HookDeliverySourceReceiptV1::new(settlement) {
        Ok(receipt) => HookDeliveryReceiptOutcomeV1::retain(
            tracedecay_hooks::hook_delivery_receipt_spool_root(data_root, source.host()),
            tracedecay_hooks::HOOK_SYNCHRONOUS_BUDGET,
            &receipt,
        ),
        Err(error) => Err(error).into(),
    }
}

/// The one exit-1 site of the capture fast path. A successful hook is silent
/// on stderr (the host shows every byte to the user), but a refused one must
/// name its reason there: this path runs before the tracing subscriber is
/// installed, so a `tracing::warn!` here was dropped and every refusal
/// surfaced as a bare exit 1 with `{}` and empty stderr.
fn refused(reason: impl std::fmt::Display) -> i32 {
    eprintln!("tracedecay hook: {reason}");
    1
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

#[derive(Clone, Copy, Debug)]
enum StdinRefusal {
    Unreadable,
    Oversized,
}

impl std::fmt::Display for StdinRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Unreadable => "stdin was unreadable",
            Self::Oversized => "stdin exceeded the payload bound",
        })
    }
}

fn read_bounded_stdin() -> Result<Vec<u8>, StdinRefusal> {
    let bound = tracedecay_framing::MAX_WIRE_MESSAGE_BYTES;
    let mut payload = Vec::new();
    std::io::stdin()
        .lock()
        .take((bound + 1) as u64)
        .read_to_end(&mut payload)
        .map_err(|_| StdinRefusal::Unreadable)?;
    (payload.len() <= bound)
        .then_some(payload)
        .ok_or(StdinRefusal::Oversized)
}

fn current_time() -> Option<UtcMicros> {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    let micros = i64::try_from(elapsed.as_micros()).ok()?;
    Some(UtcMicros(micros))
}
