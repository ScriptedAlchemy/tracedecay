use crate::cli::Commands;

pub(crate) async fn handle_hook_command(command: Commands) -> tracedecay::errors::Result<()> {
    // Claude PostCompact is a daemon-owned pressure probe, not a native
    // capture source: Claude exposes no machine-verifiable compacted payload,
    // so the daemon records the boundary and reports typed unavailable.
    if matches!(command, Commands::HookClaudePostCompact) {
        exit_if_nonzero(tracedecay::hooks::hook_claude_post_compact().await);
        return Ok(());
    }
    // Codex PostCompact is likewise a daemon-owned pressure probe rather than
    // a native capture source: the daemon lands the session's rollout through
    // the canonical transcript ingest route and runs the daemon-owned
    // compression journey at the pressure boundary, which a deferred spool
    // replay cannot honor.
    if matches!(command, Commands::HookCodexPostCompact) {
        exit_if_nonzero(tracedecay::hooks::hook_codex_post_compact().await);
        return Ok(());
    }
    let native_response_code = match &command {
        Commands::HookStop => Some(tracedecay::hooks::hook_stop().await),
        Commands::HookClaudeSessionStart => {
            Some(tracedecay::hooks::hook_claude_session_start().await)
        }
        Commands::HookClaudePostToolUse => {
            Some(tracedecay::hooks::hook_claude_post_tool_use().await)
        }
        Commands::HookCursorSessionStart => {
            Some(tracedecay::hooks::hook_cursor_session_start().await)
        }
        Commands::HookCodexSessionStart => {
            Some(tracedecay::hooks::hook_codex_session_start().await)
        }
        Commands::HookCodexPostToolUse => Some(tracedecay::hooks::hook_codex_post_tool_use().await),
        Commands::HookHermesTerminalReceipt => {
            Some(tracedecay::hooks::hook_hermes_terminal_receipt().await)
        }
        Commands::HookKiroPromptSubmit => Some(tracedecay::hooks::hook_kiro_prompt_submit().await),
        _ => None,
    };
    if let Some(code) = native_response_code {
        exit_if_nonzero(code);
        return Ok(());
    }
    if let Some(source) = crate::hook_capture_cmd::capture_source_for_command(&command) {
        exit_if_nonzero(crate::hook_capture_cmd::run_native_capture(source));
        return Ok(());
    }
    if crate::hook_capture_cmd::is_native_hook_command(&command) {
        return Ok(());
    }
    unreachable!("non-hook command passed to hook dispatcher")
}

fn exit_if_nonzero(code: i32) {
    if code != 0 {
        std::process::exit(code);
    }
}
