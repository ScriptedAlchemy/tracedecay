#![allow(clippy::collapsible_if)]
// Rust guideline compliant 2025-10-17
// Updated 2026-03-23: compact bordered table for status output
use clap::{CommandFactory, Parser};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process;

mod agent_cmd;
mod automation_cli;
mod cli;
mod commands;
mod cost_cmd;
mod global;
mod hook_cmd;
mod lsp_cmd;
mod project_cmd;
mod sessions_cmd;
mod status_cmd;
mod tool_command;
mod update_cmd;

pub use tracedecay::serve;

use cli::*;

/// Alias for the shared timestamp utility.
pub(crate) fn current_unix_timestamp() -> i64 {
    tracedecay::tracedecay::current_timestamp()
}

/// A self-animating spinner that ticks on a background thread.
/// Call `set_message` to update what is displayed; the background thread
/// redraws at ~80 ms intervals. Call `done` to stop and print a final line.
pub(crate) struct Spinner {
    message: std::sync::Arc<std::sync::Mutex<String>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    interactive: bool,
}

impl Spinner {
    pub(crate) fn new() -> Self {
        let message = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let interactive = std::io::stderr().is_terminal();
        let handle = if interactive {
            Some(Self::spawn_interactive_spinner(
                message.clone(),
                stop.clone(),
            ))
        } else {
            None
        };

        Self {
            message,
            stop,
            handle,
            interactive,
        }
    }

    fn spawn_interactive_spinner(
        message: std::sync::Arc<std::sync::Mutex<String>>,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> std::thread::JoinHandle<()> {
        let msg = message.clone();
        let stp = stop.clone();
        // Hide cursor while spinner is active.
        let _ = write!(std::io::stderr(), "\x1b[?25l");
        let _ = std::io::stderr().flush();
        std::thread::spawn(move || {
            let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let mut idx = 0usize;
            while !stp.load(std::sync::atomic::Ordering::Relaxed) {
                let text = msg
                    .lock()
                    .map_or_else(|_| String::new(), |locked| locked.clone());
                if !text.is_empty() {
                    let frame = frames[idx % frames.len()];
                    idx += 1;
                    // Truncate to avoid line wrapping on typical terminals.
                    let display: std::borrow::Cow<str> = if text.len() > 50 {
                        format!("…{}", &text[text.len() - 49..]).into()
                    } else {
                        text.as_str().into()
                    };
                    let mut stderr = std::io::stderr();
                    let _ = write!(stderr, "\r\x1b[2K{} {}", frame, display);
                    let _ = stderr.flush();
                }
                std::thread::sleep(std::time::Duration::from_millis(80));
            }
        })
    }

    pub(crate) fn set_message(&self, msg: &str) {
        if let Ok(mut locked) = self.message.lock() {
            *locked = msg.to_string();
        }
    }

    pub(crate) fn done(mut self, message: &str) {
        self.stop();
        let mut stderr = std::io::stderr();
        if self.interactive {
            // Show cursor again, then print the done line.
            let _ = write!(stderr, "\x1b[?25h");
            let _ = writeln!(stderr, "\r\x1b[2K\x1b[32m✔\x1b[0m {}", message);
        } else {
            let _ = writeln!(stderr, "{message}");
        }
        let _ = stderr.flush();
    }

    fn stop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        // If the spinner wasn't explicitly finished (e.g. `?` propagated an
        // error), still stop the thread, clear the line, and restore the
        // cursor so the terminal is left in a sane state.
        self.stop();
        if self.interactive {
            let mut stderr = std::io::stderr();
            let _ = write!(stderr, "\r\x1b[2K\x1b[?25h");
            let _ = stderr.flush();
        }
    }
}

/// Stack size for the thread driving the async entrypoint. Windows gives the
/// process main thread only 1 MiB of stack (Linux and macOS give 8 MiB), and
/// the combined CLI + MCP tool-dispatch futures exceed that in unoptimized
/// builds — `tracedecay serve` and `tracedecay tool` died with
/// STATUS_STACK_OVERFLOW on Windows CI. Running the runtime on a thread with
/// an explicit stack size gives every platform the same headroom.
const ASYNC_STACK_BYTES: usize = 16 * 1024 * 1024;

fn main() {
    let spawned = std::thread::Builder::new()
        .name("tracedecay-main".to_string())
        .stack_size(ASYNC_STACK_BYTES)
        .spawn(async_main);
    let result = match spawned {
        Ok(handle) => match handle.join() {
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        },
        Err(e) => {
            eprintln!("Error: failed to spawn main thread: {e}");
            process::exit(1);
        }
    };
    if let Err(e) = result {
        eprintln!("Error: {}", e);
        process::exit(1);
    }
}

fn async_main() -> tracedecay::errors::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if render_dynamic_command_help(&args) {
        return Ok(());
    }
    let cli = Cli::parse_from(args);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(ASYNC_STACK_BYTES)
        .build()
        .map_err(|e| tracedecay::errors::TraceDecayError::Config {
            message: format!("failed to start async runtime: {e}"),
        })?;
    let result = runtime.block_on(run(cli));
    // Runtime drop waits indefinitely for blocking tasks. Daemon integrations
    // can leave OS-backed watcher work behind after their async handles abort,
    // so bound teardown after the command's own graceful shutdown completes.
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    result
}

fn render_dynamic_command_help(args: &[String]) -> bool {
    let command_args = args.get(1..).unwrap_or_default();
    let is_tool_command_help = matches!(
        command_args,
        [command, help] if command == "tool" && matches!(help.as_str(), "-h" | "--help")
    );
    if !is_tool_command_help {
        return false;
    }

    let mut command = Cli::command();
    if let Some(tool) = command.find_subcommand_mut("tool") {
        let _ = tool.print_long_help();
        println!();
    }
    true
}

async fn run(cli: Cli) -> tracedecay::errors::Result<()> {
    let command = match cli.command {
        Some(cmd) => cmd,
        None => return commands::handle_no_command().await,
    };

    maybe_run_extract_worker(&command);
    let _hook_lease = match hook_cmd::admit_hook_command(&command)? {
        hook_cmd::HookAdmission::NotHook => None,
        hook_cmd::HookAdmission::Acquired(lease) => Some(lease),
        hook_cmd::HookAdmission::Busy => {
            hook_cmd::drain_busy_hook_stdin(&command);
            return Ok(());
        }
    };
    run_startup_preamble(&command).await;
    dispatch_command(command).await
}

fn maybe_run_extract_worker(command: &Commands) {
    // Worker mode bypasses every normal startup path (no config load, no
    // worldwide-counter ping, no agent checks). The token handshake inside
    // run_worker is the only authentication; this dispatch must happen
    // before anything else can side-effect on disk or network.
    if matches!(command, Commands::ExtractWorker) {
        tracedecay::extraction_worker::run_worker();
    }
}

async fn run_startup_preamble(command: &Commands) {
    let startup_policy = CommandStartupPolicy::for_command(command);

    // First-run notice (check BEFORE any config save creates the file)
    let is_first_run = tracedecay::user_config::UserConfig::is_fresh();

    // Best-effort flush of pending worldwide counter tokens.
    let is_force_flush = matches!(
        command,
        Commands::Init { .. } | Commands::Sync { .. } | Commands::Status { .. }
    );
    let mut user_config = tracedecay::user_config::UserConfig::load();
    // Skip the worldwide-counter flush on hot startup paths. `try_flush`
    // makes a synchronous HTTP call (#84) which can add seconds to
    // `tracedecay serve` startup on slow networks — long enough to blow the
    // MCP client's 30 s `initialize` timeout.
    if startup_policy.runs_startup_maintenance() {
        global::try_flush(&mut user_config, is_force_flush);
    }
    if !is_local_install_command(command) {
        if let Err(err) = user_config.save_if_exists() {
            eprintln!("warning: could not save tracedecay config: {err}");
        }
    }

    if is_first_run && startup_policy.runs_startup_maintenance() {
        eprintln!(
            "note: tracedecay can optionally upload anonymous token savings counts to a worldwide counter.\n\
             \x20     Run `tracedecay enable-upload-counter` to opt in."
        );
    }

    // The "beta merged into stable" nudge that lived here through 4.3.x was
    // retired in 4.3.12. The beta channel is open again as of v5.0.0-beta.1
    // and beta users now stay on beta until they explicitly switch off.

    // Best-effort check: warn if install needs re-running.
    if startup_policy.runs_agent_install_maintenance() {
        tracedecay::agents::claude::check_install_stale();
        maybe_run_silent_reinstall(&mut user_config).await;
    }
}

/// What startup maintenance should do about the version markers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SilentReinstallAction {
    /// Re-run install for every tracked agent.
    Reinstall,
    /// Patch-only bump (or nothing to reinstall): just advance the marker.
    AdvanceMarker,
    /// Markers already match the running version — nothing to do.
    Nothing,
}

fn silent_reinstall_action(
    user_config: &tracedecay::user_config::UserConfig,
    running: &str,
) -> SilentReinstallAction {
    // Two signals can trigger a reinstall:
    //   (a) `previous_version` (set by `tracedecay upgrade` / `channel switch`
    //       just before replacing the binary) differs from the running version
    //       AND the transition is a minor/major bump. Patch bumps are no-ops:
    //       we just advance `previous_version` and skip reinstall.
    //   (b) Fallback for external upgrades (`brew upgrade`, `cargo install`):
    //       the running version is newer than `last_installed_version`.
    //
    // A successful `post-update` performs the same full tracked-agent
    // install pass and then advances both markers (see
    // `UserConfig::mark_version_installed`), so the next ordinary command
    // does not repeat the reinstall it just performed.
    let previous_version = if user_config.previous_version.is_empty() {
        "6.0.0".to_string()
    } else {
        user_config.previous_version.clone()
    };
    let upgrade_detected = previous_version != running;
    let transition_needs_reinstall = upgrade_detected
        && (tracedecay::cloud::is_newer_minor_version(&previous_version, running)
            || tracedecay::cloud::is_newer_minor_version(running, &previous_version));
    let external_upgrade_needs_reinstall = !upgrade_detected
        && (user_config.last_installed_version.is_empty()
            || tracedecay::cloud::is_newer_version(&user_config.last_installed_version, running));
    let needs_reinstall = transition_needs_reinstall || external_upgrade_needs_reinstall;

    if !user_config.installed_agents.is_empty() && !running.is_empty() && needs_reinstall {
        SilentReinstallAction::Reinstall
    } else if upgrade_detected {
        SilentReinstallAction::AdvanceMarker
    } else {
        SilentReinstallAction::Nothing
    }
}

async fn maybe_run_silent_reinstall(user_config: &mut tracedecay::user_config::UserConfig) {
    // Silent reinstall: re-run install for every tracked agent so permissions,
    // hooks, and MCP config stay in sync with the new binary.
    let running = env!("CARGO_PKG_VERSION");
    match silent_reinstall_action(user_config, running) {
        SilentReinstallAction::Reinstall => run_silent_reinstall(user_config, running).await,
        SilentReinstallAction::AdvanceMarker => {
            user_config.previous_version = running.to_string();
            if let Err(err) = user_config.save() {
                eprintln!("warning: could not save tracedecay config: {err}");
            }
        }
        SilentReinstallAction::Nothing => {}
    }
}

async fn run_silent_reinstall(
    user_config: &mut tracedecay::user_config::UserConfig,
    running: &str,
) {
    if let update_cmd::ReinstallOutcome::AllOk =
        update_cmd::reinstall_tracked_agents(user_config).await
    {
        user_config.mark_version_installed(running);
        if let Err(err) = user_config.save() {
            eprintln!("warning: could not save tracedecay config: {err}");
        }
    }
}

async fn resolve_registered_project_root(
    project_id: Option<String>,
    project_path: Option<String>,
) -> tracedecay::errors::Result<Option<PathBuf>> {
    let Some(selector) = project_id.or(project_path) else {
        return Ok(None);
    };
    let context = commands::daemon_tool_json(
        None,
        "tracedecay_admin_cli",
        serde_json::json!({
            "action": "registry_context",
            "project_arg": selector,
        }),
    )
    .await?;
    let display_root = context
        .get("project")
        .and_then(|project| project.get("display_root"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: "registered project not found for selector".to_string(),
        })?;
    Ok(Some(PathBuf::from(display_root)))
}

pub(crate) async fn resolve_cli_project_root(
    path: Option<String>,
    project_id: Option<String>,
    project_path: Option<String>,
) -> tracedecay::errors::Result<PathBuf> {
    if let Some(root) = resolve_registered_project_root(project_id, project_path).await? {
        return Ok(root);
    }
    Ok(tracedecay::config::resolve_path_with_discovery(path))
}

pub(crate) fn parse_lcm_scope_arg(
    value: &str,
) -> tracedecay::errors::Result<tracedecay::sessions::lcm::LcmScope> {
    use tracedecay::sessions::lcm::LcmScope;
    match value.trim().replace('-', "_").as_str() {
        "all" => Ok(LcmScope::All),
        "session" => Ok(LcmScope::Session),
        "current" => Ok(LcmScope::Current),
        other => Err(tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "invalid session-reflection --scope '{other}'; expected all, session, or current"
            ),
        }),
    }
}

async fn dispatch_command(command: Commands) -> tracedecay::errors::Result<()> {
    match command {
        Commands::Init {
            path,
            skip_folders,
            include_folders,
        } => {
            commands::handle_init(path, skip_folders, include_folders).await?;
        }
        Commands::Sync {
            path,
            force,
            skip_folders,
            include_folders,
            doctor,
            verbose,
        } => {
            commands::handle_sync(path, force, skip_folders, include_folders, doctor, verbose)
                .await?;
        }
        Commands::Status {
            path,
            project_id,
            project_path,
            json,
            short,
            details,
            runtime,
        } => {
            status_cmd::handle_status_command(
                path,
                project_id,
                project_path,
                json,
                short,
                details,
                runtime,
            )
            .await?;
        }
        Commands::Tool {
            project,
            name,
            args,
        } => {
            tool_command::run(project, name, args).await?;
        }
        Commands::Lsp { action } => {
            lsp_cmd::handle_lsp_action(action)?;
        }
        Commands::Install {
            agent,
            local,
            no_dashboard,
            automation,
            auto_apply,
        } => {
            agent_cmd::handle_install_command(
                agent,
                local,
                no_dashboard,
                automation.then_some(agent_cmd::CodexAutomationInstall { auto_apply }),
            )
            .await?;
        }
        Commands::Reinstall => {
            agent_cmd::handle_reinstall_command().await?;
        }
        Commands::UpdatePlugin => {
            update_cmd::refresh_generated_plugins().await?;
        }
        Commands::Uninstall { agent } => {
            agent_cmd::handle_uninstall_command(agent).await?;
        }
        Commands::ExtractWorker => {
            // Handled by the early dispatch at the top of run(); this arm
            // exists only for clap match exhaustiveness.
            unreachable!("extract-worker handled by early dispatch")
        }
        hook_command @ (Commands::HookPreToolUse
        | Commands::HookPromptSubmit
        | Commands::HookStop
        | Commands::HookClaudeSessionStart
        | Commands::HookClaudePostToolUse
        | Commands::HookClaudeSubagentStart
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
        | Commands::HookUserSessionReview) => {
            hook_cmd::handle_hook_command(hook_command).await?;
        }
        Commands::Dashboard {
            path,
            host,
            port,
            open,
        } => {
            let project_path = tracedecay::config::resolve_path_with_discovery(path);
            let result = commands::daemon_tool_json(
                Some(&project_path),
                "tracedecay_dashboard",
                serde_json::json!({
                    "action": "start",
                    "host": host,
                    "port": port,
                    "format": "json",
                }),
            )
            .await?;
            let url = result
                .get("url")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                    message: "daemon dashboard response omitted URL".to_string(),
                })?;
            println!("tracedecay dashboard listening on {url}");
            eprintln!("Serving project {}", project_path.display());
            if open {
                match open::that(url) {
                    Ok(()) => eprintln!("Opened dashboard in default browser: {url}"),
                    Err(error) => {
                        eprintln!("Warning: could not open browser for {url}: {error}")
                    }
                }
            }
        }
        Commands::Serve { path, timings } => {
            if matches!(std::env::var("DISABLE_TRACEDECAY").as_deref(), Ok("true")) {
                // Allow users to opt out per-project by setting
                // DISABLE_TRACEDECAY=true. The process exits cleanly so the
                // host does not retry.
                return Ok(());
            }
            // The MCP server is long-lived, so it may run the detached
            // structured-row backfill sweep; one-shot CLI/hook processes never
            // do (they would drop the sweep mid-parse on exit).
            tracedecay::global_db::mark_process_long_lived_for_structured_backfill();
            serve::run_serve(path, timings).await?;
        }
        Commands::Daemon { action } => match action {
            DaemonAction::Run { socket } => {
                // Long-lived host: allowed to run the structured-row sweep.
                tracedecay::global_db::mark_process_long_lived_for_structured_backfill();
                let socket_path = tracedecay::daemon::socket_path_or_default(socket)?;
                tracedecay::daemon::run_foreground(socket_path).await?;
            }
            DaemonAction::InstallService { socket, no_start } => {
                let tracedecay_bin = tracedecay::agents::which_tracedecay().ok_or_else(|| {
                    tracedecay::errors::TraceDecayError::Config {
                        message: "tracedecay not found on PATH".to_string(),
                    }
                })?;
                let spec = tracedecay::daemon::service_spec(tracedecay_bin, socket)?;
                let service_path = tracedecay::daemon::install_service(&spec, !no_start)?;
                eprintln!(
                    "Installed TraceDecay daemon service at {}",
                    service_path.display()
                );
                eprintln!("Daemon socket: {}", spec.socket_path.display());
            }
            DaemonAction::UninstallService { no_stop } => {
                let service_path = tracedecay::daemon::uninstall_service(!no_stop)?;
                eprintln!(
                    "Removed TraceDecay daemon service at {}",
                    service_path.display()
                );
            }
            DaemonAction::Restart => {
                update_cmd::restart_daemon_service()?;
            }
            DaemonAction::Status => {
                let socket_path = tracedecay::daemon::socket_path_or_default(None)?;
                print!("{}", tracedecay::daemon::service_status(&socket_path));
            }
        },
        Commands::Upgrade {
            no_heal,
            no_reinstall,
        } => {
            update_cmd::run_upgrade_command(no_heal, no_reinstall)?;
        }
        Commands::Update {
            no_heal,
            no_reinstall,
        } => {
            update_cmd::run_update_command(no_heal, no_reinstall)?;
        }
        Commands::Dogfood => {
            update_cmd::run_dogfood_command()?;
        }
        Commands::PostUpdate {
            no_heal,
            no_reinstall,
            lifecycle_lease_token,
            previous_daemon_state,
        } => {
            let lifecycle_lease = tracedecay::lifecycle_lease::acquire_exclusive_or_inherited(
                "post-update",
                lifecycle_lease_token.as_deref(),
            )?;
            update_cmd::run_post_update_tasks(
                no_heal,
                no_reinstall,
                &lifecycle_lease,
                previous_daemon_state,
            )
            .await?;
        }
        Commands::Channel { channel } => match channel {
            Some(target) => {
                tracedecay::upgrade::switch_channel(&target)?;
            }
            None => tracedecay::upgrade::show_channel(),
        },
        Commands::CurrentCounter { path } => {
            let project_path = tracedecay::config::resolve_path(path);
            let result = commands::daemon_tool_json(
                Some(&project_path),
                "tracedecay_admin_project",
                serde_json::json!({ "action": "counter_get" }),
            )
            .await?;
            let value = result
                .get("counter")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                    message: "daemon counter response omitted counter".to_string(),
                })?;
            println!("{value}");
        }
        Commands::ResetCounter { path } => {
            let project_path = tracedecay::config::resolve_path(path);
            let result = commands::daemon_tool_json(
                Some(&project_path),
                "tracedecay_admin_project",
                serde_json::json!({ "action": "counter_get" }),
            )
            .await?;
            let prev = result
                .get("counter")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                    message: "daemon counter response omitted counter".to_string(),
                })?;
            commands::daemon_tool_json(
                Some(&project_path),
                "tracedecay_admin_project",
                serde_json::json!({ "action": "counter_reset" }),
            )
            .await?;
            eprintln!("Local counter reset (was {prev})");
        }
        Commands::DisableUploadCounter => {
            commands::handle_upload_counter(false);
        }
        Commands::EnableUploadCounter => {
            commands::handle_upload_counter(true);
        }
        Commands::Gitignore { path, action } => {
            commands::handle_gitignore(path, action).await?;
        }
        Commands::Doctor { agent } => {
            tracedecay::doctor::run_doctor(agent.as_deref()).await?;
        }
        Commands::Cost {
            range,
            by_model,
            by_task,
            export,
        } => {
            cost_cmd::handle_cost(range, by_model, by_task, export).await?;
        }
        Commands::Bench {
            queries,
            json,
            path,
            max_nodes,
        } => {
            commands::handle_bench(queries, json, path, max_nodes).await?;
        }
        Commands::Gain {
            all,
            history,
            range,
            json,
        } => {
            commands::handle_gain(all, history, &range, json).await?;
        }
        Commands::Monitor => {
            if let Err(e) = tracedecay::monitor::run() {
                eprintln!("Monitor error: {e}");
                process::exit(1);
            }
        }
        Commands::Sessions { action } => {
            sessions_cmd::handle_sessions_action(action).await?;
        }
        Commands::Analytics { action } => match action {
            AnalyticsAction::Diagnostics { all, no_sync, .. } => {
                tracedecay::analytics_bridge::run_analytics_diagnostics(all, no_sync).await?;
            }
            AnalyticsAction::Sync => {
                tracedecay::analytics_bridge::run_analytics_sync().await?;
            }
        },
        Commands::Projects { action } => {
            project_cmd::handle_projects_action(action).await?;
        }
        Commands::Branch { action } => {
            commands::handle_branch_action(action).await?;
        }
        Commands::Memory { action } => match action {
            MemoryAction::Status {
                json,
                path,
                project_id,
                project_path,
            } => {
                let project_path = resolve_cli_project_root(path, project_id, project_path).await?;
                let result = commands::daemon_tool_json(
                    Some(&project_path),
                    "tracedecay_admin_project",
                    serde_json::json!({ "action": "memory_status" }),
                )
                .await?;
                let status: tracedecay::memory::types::MemoryStatus =
                    serde_json::from_value(result.get("status").cloned().ok_or_else(|| {
                        tracedecay::errors::TraceDecayError::Config {
                            message: "daemon memory response omitted status".to_string(),
                        }
                    })?)?;
                let largest_bank_fact_count = result
                    .get("largest_bank_fact_count")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                    .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                        message: "daemon memory response omitted largest bank count".to_string(),
                    })?;
                let largest_bank_utilization_pct = if status.estimated_capacity > 0 {
                    largest_bank_fact_count as f64 / status.estimated_capacity as f64 * 100.0
                } else {
                    0.0
                };
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "memory": status,
                            "largest_bank_fact_count": largest_bank_fact_count,
                            "largest_bank_utilization_pct": largest_bank_utilization_pct,
                        }))
                        .unwrap_or_default()
                    );
                } else {
                    print!(
                        "{}",
                        status_cmd::format_memory_status_report(&status, largest_bank_fact_count)
                    );
                }
            }
            other => {
                commands::handle_memory_action(other).await?;
            }
        },
        Commands::Automation { action } => {
            automation_cli::handle_automation_command(action).await?;
        }
        Commands::Migrate { action } => {
            commands::handle_migrate_action(action).await?;
        }
        Commands::Wipe { all } => {
            commands::handle_wipe(all).await?;
        }
        Commands::List { all } => {
            commands::handle_list(all).await?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandStartupPolicy {
    Full,
    SkipAll,
}

impl CommandStartupPolicy {
    fn for_command(command: &Commands) -> Self {
        if hook_cmd::hook_input(command).is_some() {
            return Self::SkipAll;
        }

        match command {
            // Tool calls are the documented MCP fallback and must remain a local,
            // latency-bounded protocol path. Unrelated counter uploads or agent
            // maintenance belong on interactive commands and daemon background work.
            Commands::Tool { .. } => Self::SkipAll,
            // Explicit lifecycle/maintenance commands manage their own work.
            // Serve is also latency-sensitive: clients impose a 30 s MCP
            // initialize timeout, so no implicit startup work belongs there.
            Commands::Install { .. }
            | Commands::Reinstall
            | Commands::UpdatePlugin
            | Commands::Upgrade { .. }
            | Commands::Update { .. }
            | Commands::Dogfood
            | Commands::PostUpdate { .. }
            | Commands::Uninstall { .. }
            | Commands::Lsp { .. }
            | Commands::Doctor { .. }
            | Commands::Analytics { .. }
            | Commands::Sessions {
                action: SessionsAction::Unfinished { .. },
            }
            | Commands::Migrate { .. }
            | Commands::Projects { .. }
            | Commands::Daemon { .. }
            | Commands::Serve { .. } => Self::SkipAll,
            _ => Self::Full,
        }
    }

    fn runs_startup_maintenance(self) -> bool {
        !matches!(self, Self::SkipAll)
    }

    fn runs_agent_install_maintenance(self) -> bool {
        matches!(self, Self::Full)
    }
}

#[cfg(test)]
fn should_skip_startup_maintenance(command: &Commands) -> bool {
    !CommandStartupPolicy::for_command(command).runs_startup_maintenance()
}

#[cfg(test)]
fn should_skip_agent_install_maintenance(command: &Commands) -> bool {
    !CommandStartupPolicy::for_command(command).runs_agent_install_maintenance()
}

fn is_local_install_command(command: &Commands) -> bool {
    matches!(command, Commands::Install { local: true, .. })
}

#[cfg(test)]
mod startup_tests;

// handle_branch_action, handle_wipe, handle_list, handle_no_command,
// init_and_index has been moved to src/commands.rs.
//
// update_global_db, try_flush, check_for_update, gather_target_projects,
// gather_local_projects, gather_local_projects_from, find_descendant_tracedecay,
// print_flash_warning, and tracedecay_dir_size have been moved to src/global.rs.
