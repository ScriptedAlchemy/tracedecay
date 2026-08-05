use crate::cli::Commands;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HookInput {
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
    if std::io::stdin().is_terminal() {
        return;
    }
    let _ = drain_hook_input(input_mode, &mut std::io::stdin().lock());
}

fn drain_hook_input(_mode: HookInput, input: &mut impl std::io::Read) -> std::io::Result<u64> {
    std::io::copy(input, &mut std::io::sink())
}

pub(crate) fn hook_input(command: &Commands) -> Option<HookInput> {
    if crate::hook_capture_cmd::is_native_hook_command(command) {
        return None;
    }
    match command {
        Commands::HookUserSessionReview => Some(HookInput::Stdin),
        _ => None,
    }
}

pub(crate) async fn handle_hook_command(command: Commands) -> tracedecay::errors::Result<()> {
    if let Some(source) = crate::hook_capture_cmd::capture_source_for_command(&command) {
        exit_if_nonzero(crate::hook_capture_cmd::run_native_capture(source));
        return Ok(());
    }
    if crate::hook_capture_cmd::is_native_hook_command(&command) {
        return Ok(());
    }
    match command {
        Commands::HookUserSessionReview => {
            exit_if_nonzero(tracedecay::hooks::hook_user_session_review().await);
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
        vec![(Commands::HookUserSessionReview, HookInput::Stdin)]
    }

    #[test]
    fn lifecycle_guarded_hooks_have_explicit_input_semantics() {
        let hooks = hook_commands();
        assert_eq!(
            hooks
                .iter()
                .filter(|(_, input)| *input == HookInput::Stdin)
                .count(),
            hooks.len()
        );
        for (command, expected) in hooks {
            assert_eq!(hook_input(&command), Some(expected));
            assert!(crate::should_skip_agent_install_maintenance(&command));
        }
    }

    #[test]
    fn native_capture_commands_bypass_the_profile_lifecycle_lease() {
        for command in [
            Commands::HookPreToolUse,
            Commands::HookPromptSubmit,
            Commands::HookStop,
            Commands::HookClaudeSessionStart,
            Commands::HookClaudePostToolUse,
            Commands::HookClaudeSubagentStart,
            Commands::HookKiroPreToolUse,
            Commands::HookKiroPromptSubmit,
            Commands::HookKiroPostToolUse,
            Commands::HookCursorSubagentStart,
            Commands::HookCursorPostToolUse,
            Commands::HookCursorBeforeSubmitPrompt,
            Commands::HookCursorPreCompact,
            Commands::HookCursorAfterFileEdit,
            Commands::HookCursorSessionStart,
            Commands::HookCursorSessionEnd,
            Commands::HookCursorAfterShell,
            Commands::HookCursorWorkspaceOpen,
            Commands::HookCursorStop,
            Commands::HookCodexSessionStart,
            Commands::HookCodexUserPromptSubmit,
            Commands::HookCodexSubagentStart,
            Commands::HookCodexPostToolUse,
            Commands::HookCodexPostCompact,
            Commands::HookCodexStop,
            Commands::HookHermesTerminalReceipt,
            Commands::HookKimiEvent,
            Commands::HookOpenCodeEvent,
            Commands::HookOpenCodeToolAfter,
        ] {
            assert_eq!(hook_input(&command), None);
        }
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
    fn busy_stdin_hooks_drain_input() {
        let mut stdin_payload = b"{\"hook_event_name\":\"SessionStart\"}".as_slice();
        let stdin_len = stdin_payload.len() as u64;
        let drained = drain_hook_input(HookInput::Stdin, &mut stdin_payload).unwrap();
        assert_eq!(drained, stdin_len);
        assert!(stdin_payload.is_empty());
    }
}
