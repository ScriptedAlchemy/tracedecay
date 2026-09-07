use crate::cli::Commands;

#[hotpath::measure(label = "cli.hook.dispatch", future = true)]
pub(crate) async fn handle_hook_command(
    command: Commands,
) -> tracedecay_domain::errors::Result<i32> {
    handle_hook_command_inner(command).await
}

fn handle_hook_command_inner(
    command: Commands,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = tracedecay_domain::errors::Result<i32>> + Send + 'static>,
> {
    // Erase the deeply nested hook-dispatch future before it reaches the
    // measured wrapper so every profiling feature can compute its layout.
    Box::pin(async move {
        let runtime = tracedecay::hook_runtime();
        // Claude PostCompact is a daemon-owned pressure probe, not a native
        // capture source: Claude exposes no machine-verifiable compacted payload,
        // so the daemon records the boundary and reports typed unavailable.
        if matches!(command, Commands::HookClaudePostCompact) {
            return Ok(tracedecay_agent_hosts::hooks::hook_claude_post_compact(&runtime).await);
        }
        // Codex PostCompact is likewise a daemon-owned pressure probe rather than
        // a native capture source: the daemon lands the session's rollout through
        // the canonical transcript ingest route and runs the daemon-owned
        // compression journey at the pressure boundary, which a deferred spool
        // replay cannot honor.
        if matches!(command, Commands::HookCodexPostCompact) {
            return Ok(tracedecay_agent_hosts::hooks::hook_codex_post_compact(&runtime).await);
        }
        let native_response_code = match &command {
            Commands::HookStop => Some(tracedecay_agent_hosts::hooks::hook_stop(&runtime).await),
            Commands::HookClaudeSessionStart => {
                Some(tracedecay_agent_hosts::hooks::hook_claude_session_start(&runtime).await)
            }
            Commands::HookClaudePostToolUse => {
                Some(tracedecay_agent_hosts::hooks::hook_claude_post_tool_use(&runtime).await)
            }
            Commands::HookCursorSessionStart => {
                Some(tracedecay_agent_hosts::hooks::hook_cursor_session_start(&runtime).await)
            }
            Commands::HookCursorPostToolUse => {
                Some(tracedecay_agent_hosts::hooks::hook_cursor_post_tool_use(&runtime).await)
            }
            Commands::HookCodexSessionStart => {
                Some(tracedecay_agent_hosts::hooks::hook_codex_session_start(&runtime).await)
            }
            Commands::HookCodexUserPromptSubmit => {
                Some(tracedecay_agent_hosts::hooks::hook_codex_user_prompt_submit(&runtime).await)
            }
            Commands::HookCodexPostToolUse => {
                Some(tracedecay_agent_hosts::hooks::hook_codex_post_tool_use(&runtime).await)
            }
            Commands::HookHermesTerminalReceipt => {
                Some(tracedecay_agent_hosts::hooks::hook_hermes_terminal_receipt(&runtime).await)
            }
            Commands::HookKiroPromptSubmit => {
                Some(tracedecay_agent_hosts::hooks::hook_kiro_prompt_submit(&runtime).await)
            }
            Commands::HookKimiEvent => {
                Some(tracedecay_agent_hosts::hooks::hook_kimi_event(&runtime).await)
            }
            Commands::HookOpenCodeEvent => {
                Some(tracedecay_agent_hosts::hooks::hook_opencode_event(&runtime).await)
            }
            Commands::HookOpenCodeToolAfter => {
                Some(tracedecay_agent_hosts::hooks::hook_opencode_tool_after(&runtime).await)
            }
            _ => None,
        };
        if let Some(code) = native_response_code {
            return Ok(code);
        }
        if let Some(source) = crate::hook_capture_cmd::capture_source_for_command(&command) {
            return Ok(crate::hook_capture_cmd::run_native_capture(source));
        }
        if crate::hook_capture_cmd::is_native_hook_command(&command) {
            return Ok(0);
        }
        unreachable!("non-hook command passed to hook dispatcher")
    })
}
