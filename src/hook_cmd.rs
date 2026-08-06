use crate::cli::Commands;

pub(crate) async fn handle_hook_command(command: Commands) -> tracedecay::errors::Result<()> {
    // Claude PostCompact is a daemon-owned pressure probe, not a native
    // capture source: Claude exposes no machine-verifiable compacted payload,
    // so the daemon records the boundary and reports typed unavailable.
    if matches!(command, Commands::HookClaudePostCompact) {
        exit_if_nonzero(tracedecay::hooks::hook_claude_post_compact().await);
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
