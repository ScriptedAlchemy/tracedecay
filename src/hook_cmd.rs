use crate::cli::Commands;

pub(crate) async fn handle_hook_command(command: Commands) -> tracedecay::errors::Result<()> {
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
