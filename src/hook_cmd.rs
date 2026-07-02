use crate::cli::Commands;

pub(crate) async fn handle_hook_command(command: Commands) -> tracedecay::errors::Result<()> {
    match command {
        Commands::HookPreToolUse => {
            tracedecay::hooks::hook_pre_tool_use();
        }
        Commands::HookPromptSubmit => {
            tracedecay::hooks::hook_prompt_submit().await;
        }
        Commands::HookStop => {
            tracedecay::hooks::hook_stop().await;
        }
        Commands::HookClaudeSessionStart => {
            exit_if_nonzero(tracedecay::hooks::hook_claude_session_start().await);
        }
        Commands::HookClaudePostToolUse => {
            exit_if_nonzero(tracedecay::hooks::hook_claude_post_tool_use().await);
        }
        Commands::HookKiroPreToolUse => {
            exit_if_nonzero(tracedecay::hooks::hook_kiro_pre_tool_use());
        }
        Commands::HookKiroPromptSubmit => {
            exit_if_nonzero(tracedecay::hooks::hook_kiro_prompt_submit().await);
        }
        Commands::HookKiroPostToolUse => {
            exit_if_nonzero(tracedecay::hooks::hook_kiro_post_tool_use().await);
        }
        Commands::HookCursorSubagentStart => {
            exit_if_nonzero(tracedecay::hooks::hook_cursor_subagent_start());
        }
        Commands::HookCursorPostToolUse => {
            exit_if_nonzero(tracedecay::hooks::hook_cursor_post_tool_use());
        }
        Commands::HookCursorBeforeSubmitPrompt => {
            exit_if_nonzero(tracedecay::hooks::hook_cursor_before_submit_prompt().await);
        }
        Commands::HookCursorPreCompact => {
            exit_if_nonzero(tracedecay::hooks::hook_cursor_pre_compact().await);
        }
        Commands::HookCursorAfterFileEdit => {
            exit_if_nonzero(tracedecay::hooks::hook_cursor_after_file_edit().await);
        }
        Commands::HookCursorSessionStart => {
            exit_if_nonzero(tracedecay::hooks::hook_cursor_session_start().await);
        }
        Commands::HookCursorSessionEnd => {
            exit_if_nonzero(tracedecay::hooks::hook_cursor_session_end().await);
        }
        Commands::HookCursorAfterShell => {
            exit_if_nonzero(tracedecay::hooks::hook_cursor_after_shell().await);
        }
        Commands::HookCursorWorkspaceOpen => {
            exit_if_nonzero(tracedecay::hooks::hook_cursor_workspace_open().await);
        }
        Commands::HookCursorStop => {
            exit_if_nonzero(tracedecay::hooks::hook_cursor_stop().await);
        }
        Commands::HookCodexSessionStart => {
            exit_if_nonzero(tracedecay::hooks::hook_codex_session_start().await);
        }
        Commands::HookCodexUserPromptSubmit => {
            exit_if_nonzero(tracedecay::hooks::hook_codex_user_prompt_submit().await);
        }
        Commands::HookCodexSubagentStart => {
            exit_if_nonzero(tracedecay::hooks::hook_codex_subagent_start());
        }
        Commands::HookCodexPostToolUse => {
            exit_if_nonzero(tracedecay::hooks::hook_codex_post_tool_use().await);
        }
        Commands::HookCodexPostCompact => {
            exit_if_nonzero(tracedecay::hooks::hook_codex_post_compact().await);
        }
        _ => unreachable!("non-hook command passed to hook dispatcher"),
    }
    Ok(())
}

fn exit_if_nonzero(code: i32) {
    if code != 0 {
        std::process::exit(code);
    }
}
