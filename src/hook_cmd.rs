use std::{future::Future, time::Duration};

use crate::cli::Commands;

const USER_PROMPT_HOOK_BUDGET: Duration = Duration::from_millis(1_500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HookInput {
    NoInput,
    Stdin,
}

#[derive(Debug)]
pub(crate) enum HookAdmission {
    NotHook,
    Acquired(tracedecay::lifecycle_lease::LifecycleLease),
    Busy,
}

pub(crate) fn admit_hook_command(command: &Commands) -> HookAdmission {
    if hook_input(command).is_none() {
        return HookAdmission::NotHook;
    }
    admission_from_attempt(tracedecay::lifecycle_lease::try_acquire_shared(
        "agent hook",
    ))
}

fn admission_from_attempt(
    attempt: tracedecay::errors::Result<tracedecay::lifecycle_lease::SharedLeaseAttempt>,
) -> HookAdmission {
    match attempt {
        Ok(tracedecay::lifecycle_lease::SharedLeaseAttempt::Acquired(lease)) => {
            HookAdmission::Acquired(lease)
        }
        Ok(tracedecay::lifecycle_lease::SharedLeaseAttempt::Busy) | Err(_) => HookAdmission::Busy,
    }
}

pub(crate) fn drain_busy_hook_stdin(command: &Commands) {
    use std::io::IsTerminal;

    let Some(input_mode) = hook_input(command) else {
        return;
    };
    if input_mode == HookInput::NoInput || std::io::stdin().is_terminal() {
        return;
    }
    let _ = drain_hook_input(input_mode, &mut std::io::stdin().lock());
}

fn drain_hook_input(mode: HookInput, input: &mut impl std::io::Read) -> std::io::Result<u64> {
    match mode {
        HookInput::NoInput => Ok(0),
        HookInput::Stdin => std::io::copy(input, &mut std::io::sink()),
    }
}

pub(crate) fn hook_input(command: &Commands) -> Option<HookInput> {
    match command {
        Commands::HookPreToolUse => Some(HookInput::NoInput),
        Commands::HookClaudeSessionStart
        | Commands::HookClaudePostToolUse
        | Commands::HookClaudeSubagentStart
        | Commands::HookPromptSubmit
        | Commands::HookStop
        | Commands::HookKiroPreToolUse
        | Commands::HookKiroPromptSubmit
        | Commands::HookKiroPostToolUse
        | Commands::HookCursorSubagentStart
        | Commands::HookCursorPostToolUse
        | Commands::HookCursorBeforeSubmitPrompt
        | Commands::HookCursorPreCompact
        | Commands::HookCursorAfterFileEdit
        | Commands::HookCursorSessionStart
        | Commands::HookCursorSessionEnd
        | Commands::HookCursorAfterShell
        | Commands::HookCursorWorkspaceOpen
        | Commands::HookCursorStop
        | Commands::HookCodexSessionStart
        | Commands::HookCodexUserPromptSubmit
        | Commands::HookCodexSubagentStart
        | Commands::HookCodexPostToolUse
        | Commands::HookCodexPostCompact
        | Commands::HookCodexStop
        | Commands::HookHermesTerminalReceipt
        | Commands::HookKimiEvent
        | Commands::HookOpenCodeEvent
        | Commands::HookOpenCodeToolAfter
        | Commands::HookUserSessionReview => Some(HookInput::Stdin),
        _ => None,
    }
}

pub(crate) async fn handle_hook_command(command: Commands) -> tracedecay::errors::Result<()> {
    // Claude command hooks own a single JSON document on stdin. Early lease
    // admission guarantees this dispatcher runs only while a shared lease is
    // held; the busy path drains piped input without producing hook output.
    match command {
        Commands::HookPreToolUse => {
            tracedecay::hooks::hook_pre_tool_use();
        }
        Commands::HookPromptSubmit => {
            run_bounded_claude_prompt_hook(tracedecay::hooks::hook_prompt_submit()).await;
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
        Commands::HookClaudeSubagentStart => {
            exit_if_nonzero(tracedecay::hooks::hook_claude_subagent_start().await);
        }
        Commands::HookKiroPreToolUse => {
            exit_if_nonzero(tracedecay::hooks::hook_kiro_pre_tool_use());
        }
        Commands::HookKiroPromptSubmit => {
            run_bounded_prompt_hook(
                "Kiro UserPromptSubmit",
                tracedecay::hooks::hook_kiro_prompt_submit(),
            )
            .await;
        }
        Commands::HookKiroPostToolUse => {
            exit_if_nonzero(tracedecay::hooks::hook_kiro_post_tool_use().await);
        }
        Commands::HookCursorSubagentStart => {
            exit_if_nonzero(tracedecay::hooks::hook_cursor_subagent_start().await);
        }
        Commands::HookCursorPostToolUse => {
            exit_if_nonzero(tracedecay::hooks::hook_cursor_post_tool_use().await);
        }
        Commands::HookCursorBeforeSubmitPrompt => {
            run_bounded_prompt_hook(
                "Cursor beforeSubmitPrompt",
                tracedecay::hooks::hook_cursor_before_submit_prompt(),
            )
            .await;
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
            run_bounded_prompt_hook(
                "Codex UserPromptSubmit",
                tracedecay::hooks::hook_codex_user_prompt_submit(),
            )
            .await;
        }
        Commands::HookCodexSubagentStart => {
            exit_if_nonzero(tracedecay::hooks::hook_codex_subagent_start().await);
        }
        Commands::HookCodexPostToolUse => {
            exit_if_nonzero(tracedecay::hooks::hook_codex_post_tool_use().await);
        }
        Commands::HookCodexPostCompact => {
            exit_if_nonzero(tracedecay::hooks::hook_codex_post_compact().await);
        }
        Commands::HookCodexStop => {
            exit_if_nonzero(tracedecay::hooks::hook_codex_stop().await);
        }
        Commands::HookHermesTerminalReceipt => {
            exit_if_nonzero(tracedecay::hooks::hook_hermes_terminal_receipt().await);
        }
        Commands::HookKimiEvent => {
            exit_if_nonzero(tracedecay::hooks::hook_kimi_event().await);
        }
        Commands::HookOpenCodeEvent => {
            exit_if_nonzero(tracedecay::hooks::hook_opencode_event().await);
        }
        Commands::HookOpenCodeToolAfter => {
            exit_if_nonzero(tracedecay::hooks::hook_opencode_tool_after().await);
        }
        Commands::HookUserSessionReview => {
            exit_if_nonzero(tracedecay::hooks::hook_user_session_review().await);
        }
        _ => unreachable!("non-hook command passed to hook dispatcher"),
    }
    Ok(())
}

async fn run_bounded_claude_prompt_hook(future: impl Future<Output = ()>) {
    if tokio::time::timeout(USER_PROMPT_HOOK_BUDGET, future)
        .await
        .is_err()
    {
        eprintln!(
            "[tracedecay] Claude UserPromptSubmit exceeded the {}ms hot-hook budget; continuing without injected context",
            USER_PROMPT_HOOK_BUDGET.as_millis()
        );
    }
}

async fn run_bounded_prompt_hook(name: &str, future: impl Future<Output = i32>) {
    match tokio::time::timeout(USER_PROMPT_HOOK_BUDGET, future).await {
        Ok(code) => exit_if_nonzero(code),
        Err(_) => eprintln!(
            "[tracedecay] {name} exceeded the {}ms hot-hook budget; continuing without injected context",
            USER_PROMPT_HOOK_BUDGET.as_millis()
        ),
    }
}

fn exit_if_nonzero(code: i32) {
    if code != 0 {
        std::process::exit(code);
    }
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::Write;

    use super::{
        Commands, HookAdmission, HookInput, admission_from_attempt, drain_hook_input, hook_input,
    };
    use tracedecay::lifecycle_lease::{
        acquire_exclusive_for_profile, try_acquire_shared_for_profile,
    };

    fn hook_commands() -> Vec<(Commands, HookInput)> {
        vec![
            (Commands::HookPreToolUse, HookInput::NoInput),
            (Commands::HookPromptSubmit, HookInput::Stdin),
            (Commands::HookStop, HookInput::Stdin),
            (Commands::HookClaudeSessionStart, HookInput::Stdin),
            (Commands::HookClaudePostToolUse, HookInput::Stdin),
            (Commands::HookClaudeSubagentStart, HookInput::Stdin),
            (Commands::HookKiroPreToolUse, HookInput::Stdin),
            (Commands::HookKiroPromptSubmit, HookInput::Stdin),
            (Commands::HookKiroPostToolUse, HookInput::Stdin),
            (Commands::HookCursorSubagentStart, HookInput::Stdin),
            (Commands::HookCursorPostToolUse, HookInput::Stdin),
            (Commands::HookCursorBeforeSubmitPrompt, HookInput::Stdin),
            (Commands::HookCursorPreCompact, HookInput::Stdin),
            (Commands::HookCursorAfterFileEdit, HookInput::Stdin),
            (Commands::HookCursorSessionStart, HookInput::Stdin),
            (Commands::HookCursorSessionEnd, HookInput::Stdin),
            (Commands::HookCursorAfterShell, HookInput::Stdin),
            (Commands::HookCursorWorkspaceOpen, HookInput::Stdin),
            (Commands::HookCursorStop, HookInput::Stdin),
            (Commands::HookCodexSessionStart, HookInput::Stdin),
            (Commands::HookCodexUserPromptSubmit, HookInput::Stdin),
            (Commands::HookCodexSubagentStart, HookInput::Stdin),
            (Commands::HookCodexPostToolUse, HookInput::Stdin),
            (Commands::HookCodexPostCompact, HookInput::Stdin),
            (Commands::HookCodexStop, HookInput::Stdin),
            (Commands::HookHermesTerminalReceipt, HookInput::Stdin),
            (Commands::HookKimiEvent, HookInput::Stdin),
            (Commands::HookOpenCodeEvent, HookInput::Stdin),
            (Commands::HookOpenCodeToolAfter, HookInput::Stdin),
            (Commands::HookUserSessionReview, HookInput::Stdin),
        ]
    }

    #[test]
    fn all_hook_commands_have_explicit_input_semantics() {
        let hooks = hook_commands();
        assert_eq!(hooks.len(), 30);
        assert_eq!(
            hooks
                .iter()
                .filter(|(_, input)| *input == HookInput::NoInput)
                .count(),
            1
        );
        assert_eq!(
            hooks
                .iter()
                .filter(|(_, input)| *input == HookInput::Stdin)
                .count(),
            29
        );
        for (command, expected) in hooks {
            assert_eq!(hook_input(&command), Some(expected));
            assert!(crate::should_skip_agent_install_maintenance(&command));
        }
    }

    #[test]
    fn user_prompt_hook_budget_stays_well_below_host_deadlines() {
        assert_eq!(
            super::USER_PROMPT_HOOK_BUDGET,
            std::time::Duration::from_millis(1_500)
        );
        assert!(super::USER_PROMPT_HOOK_BUDGET < std::time::Duration::from_secs(5));
    }

    #[test]
    fn unrelated_exclusive_owner_produces_busy_admission() {
        let tmp = tempfile::tempdir().unwrap();
        let lock_path = tmp.path().join("lifecycle.lock");
        let mut external = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        fs2::FileExt::try_lock_exclusive(&external).unwrap();
        writeln!(external, "external-token\tmigration\t999").unwrap();
        external.flush().unwrap();
        let attempt = try_acquire_shared_for_profile(tmp.path(), "agent hook");

        assert!(matches!(
            admission_from_attempt(attempt),
            HookAdmission::Busy
        ));
    }

    #[test]
    fn process_owned_exclusive_lease_is_not_inherited_by_hooks() {
        let tmp = tempfile::tempdir().unwrap();
        let _exclusive = acquire_exclusive_for_profile(tmp.path(), "post-update").unwrap();
        let attempt = try_acquire_shared_for_profile(tmp.path(), "agent hook");

        assert!(matches!(
            admission_from_attempt(attempt),
            HookAdmission::Busy
        ));
    }

    #[test]
    fn normal_shared_lease_admits_hook_dispatch() {
        let tmp = tempfile::tempdir().unwrap();
        let attempt = try_acquire_shared_for_profile(tmp.path(), "agent hook");

        assert!(matches!(
            admission_from_attempt(attempt),
            HookAdmission::Acquired(_)
        ));
    }

    #[test]
    fn lifecycle_profile_errors_quiesce_hooks_like_a_busy_lease() {
        let tmp = tempfile::tempdir().unwrap();
        let profile_file = tmp.path().join("not-a-profile");
        std::fs::write(&profile_file, "file").unwrap();
        let attempt = try_acquire_shared_for_profile(&profile_file, "agent hook");

        assert!(matches!(
            admission_from_attempt(attempt),
            HookAdmission::Busy
        ));
    }

    #[test]
    fn busy_stdin_hooks_drain_but_legacy_no_input_hooks_do_not() {
        let mut stdin_payload = b"{\"hook_event_name\":\"SessionStart\"}".as_slice();
        let stdin_len = stdin_payload.len() as u64;
        let drained = drain_hook_input(HookInput::Stdin, &mut stdin_payload).unwrap();
        assert_eq!(drained, stdin_len);
        assert!(stdin_payload.is_empty());

        let mut legacy_payload = b"terminal input must remain unread".as_slice();
        let legacy_len = legacy_payload.len();
        let drained = drain_hook_input(HookInput::NoInput, &mut legacy_payload).unwrap();
        assert_eq!(drained, 0);
        assert_eq!(legacy_payload.len(), legacy_len);
    }
}
