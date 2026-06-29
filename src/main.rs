// Rust guideline compliant 2025-10-17
// Updated 2026-03-23: compact bordered table for status output
use clap::{CommandFactory, Parser};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
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

pub use tracedecay::serve;

use cli::*;

struct ReplayStdioTransport {
    replay_line: Option<String>,
    inner: tracedecay::mcp::StdioTransport,
}

impl ReplayStdioTransport {
    fn new(replay_line: Option<String>) -> Self {
        Self {
            replay_line,
            inner: tracedecay::mcp::StdioTransport::new(),
        }
    }
}

impl tracedecay::mcp::McpTransport for ReplayStdioTransport {
    async fn read_line(&mut self) -> std::io::Result<Option<String>> {
        if self.replay_line.is_some() {
            return Ok(self.replay_line.take());
        }
        self.inner.read_line().await
    }

    async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        self.inner.write_line(line).await
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush().await
    }
}

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
    runtime.block_on(run(cli))
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
    run_startup_preamble(&command);
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

fn run_startup_preamble(command: &Commands) {
    let skip_startup_maintenance = should_skip_startup_maintenance(command);
    let skip_agent_install_maintenance = should_skip_agent_install_maintenance(command);

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
    if !skip_startup_maintenance {
        global::try_flush(&mut user_config, is_force_flush);
    }
    if !is_local_install_command(command) {
        user_config.save_if_exists();
    }

    if is_first_run && !skip_startup_maintenance {
        eprintln!(
            "note: tracedecay uploads anonymous token savings counts to a worldwide counter.\n\
             \x20     Run `tracedecay disable-upload-counter` to opt out."
        );
    }

    // The "beta merged into stable" nudge that lived here through 4.3.x was
    // retired in 4.3.12. The beta channel is open again as of v5.0.0-beta.1
    // and beta users now stay on beta until they explicitly switch off.

    // Best-effort check: warn if install needs re-running.
    if !skip_agent_install_maintenance {
        tracedecay::agents::claude::check_install_stale();
        maybe_run_silent_reinstall(&mut user_config);
    }
}

fn maybe_run_silent_reinstall(user_config: &mut tracedecay::user_config::UserConfig) {
    // Silent reinstall: re-run install for every tracked agent so permissions,
    // hooks, and MCP config stay in sync with the new binary.
    //
    // Two signals can trigger this:
    //   (a) `previous_version` (set by `tracedecay upgrade` / `channel switch`
    //       just before replacing the binary) differs from the running version
    //       AND the transition is a minor/major bump. Patch bumps are no-ops:
    //       we just advance `previous_version` and skip reinstall.
    //   (b) Fallback for external upgrades (`brew upgrade`, `cargo install`):
    //       the running version is newer than `last_installed_version`.
    let running = env!("CARGO_PKG_VERSION");
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
        if let (Some(home), Some(bin)) = (
            tracedecay::agents::home_dir(),
            tracedecay::agents::which_tracedecay(),
        ) {
            let mut all_ok = true;
            for id in &user_config.installed_agents {
                if let Ok(ag) = tracedecay::agents::get_integration(id) {
                    let ctx = tracedecay::agents::InstallContext {
                        home: home.clone(),
                        tracedecay_bin: bin.clone(),
                        tool_permissions: tracedecay::agents::expected_tool_perms(),
                        profile: None,
                        project_root: None,
                        dashboard: true,
                    };
                    if ag.install(&ctx).is_err() {
                        all_ok = false;
                    }
                }
            }
            if all_ok {
                user_config.last_installed_version = running.to_string();
                user_config.previous_version = running.to_string();
                user_config.save();
            }
        }
    } else if upgrade_detected {
        // Patch-only bump (or nothing to reinstall) — advance the marker so we
        // don't keep re-checking on every subsequent startup.
        user_config.previous_version = running.to_string();
        user_config.save();
    }
}

fn refresh_generated_plugins() -> tracedecay::errors::Result<()> {
    let home = tracedecay_home_dir()?;
    let tracedecay_bin = tracedecay_bin_on_path()?;
    eprintln!("Refreshing tracedecay-generated plugin artifacts (agent configs are not touched)");

    // Detection-driven, not `installed_agents`-driven: each integration
    // decides whether generated artifacts exist on this machine, so stale
    // tracking state can neither skip a real install nor install anywhere new.
    let mut refreshed_any = false;
    let mut config_only_installed: Vec<&'static str> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    for ag in tracedecay::agents::all_integrations() {
        let ctx = tracedecay::agents::InstallContext {
            home: home.clone(),
            tracedecay_bin: tracedecay_bin.clone(),
            tool_permissions: tracedecay::agents::expected_tool_perms(),
            profile: None,
            project_root: None,
            dashboard: true,
        };
        match ag.update_plugin(&ctx) {
            Ok(tracedecay::agents::UpdatePluginOutcome::Refreshed(paths)) => {
                refreshed_any = true;
                for path in paths {
                    eprintln!(
                        "  \x1b[32m✔\x1b[0m {}: refreshed {}",
                        ag.id(),
                        path.display()
                    );
                }
            }
            Ok(tracedecay::agents::UpdatePluginOutcome::NotInstalled) => {}
            Ok(tracedecay::agents::UpdatePluginOutcome::ConfigOnly) => {
                if ag.has_tracedecay(&home) {
                    config_only_installed.push(ag.id());
                }
            }
            Err(e) => failures.push(format!("{}: {e}", ag.id())),
        }
    }
    if !config_only_installed.is_empty() {
        eprintln!(
            "  Config-managed integrations left untouched: {} (run `tracedecay reinstall` to refresh their config entries)",
            config_only_installed.join(", ")
        );
    }
    if !refreshed_any {
        eprintln!("No generated plugin installs detected — nothing to update.");
    }
    if !failures.is_empty() {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: format!("update-plugin failed for {}", failures.join("; ")),
        });
    }

    Ok(())
}

fn refresh_daemon_service() -> tracedecay::errors::Result<()> {
    let tracedecay_bin = tracedecay_bin_on_path()?;
    let spec = tracedecay::daemon::service_spec(tracedecay_bin, None)?;
    let socket_path = tracedecay::daemon::installed_service_socket_path()?
        .unwrap_or_else(|| spec.socket_path.clone());
    match tracedecay::daemon::refresh_installed_service(&spec)? {
        Some(service_path) => {
            eprintln!(
                "\x1b[32m✔\x1b[0m Daemon service refreshed at {}",
                service_path.display()
            );
            eprintln!("Daemon socket: {}", socket_path.display());
        }
        None => {
            eprintln!("TraceDecay daemon service is not installed; skipping daemon restart.");
        }
    }
    Ok(())
}

fn tracedecay_home_dir() -> tracedecay::errors::Result<PathBuf> {
    tracedecay::agents::home_dir().ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
        message: "could not determine home directory".to_string(),
    })
}

pub(crate) fn tracedecay_bin_on_path() -> tracedecay::errors::Result<String> {
    tracedecay::agents::which_tracedecay().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "tracedecay not found on PATH".to_string(),
        }
    })
}

fn run_update_steps<U, P>(mut upgrade: U, mut post_update: P) -> tracedecay::errors::Result<()>
where
    U: FnMut() -> tracedecay::errors::Result<()>,
    P: FnMut() -> tracedecay::errors::Result<()>,
{
    upgrade()?;
    post_update()?;
    Ok(())
}

fn run_update_command() -> tracedecay::errors::Result<()> {
    run_update_steps(
        || tracedecay::upgrade::run_upgrade().map(|_| ()),
        run_post_update_subcommand,
    )
}

fn run_post_update_subcommand() -> tracedecay::errors::Result<()> {
    let tracedecay_bin = tracedecay_bin_on_path()?;
    let status = std::process::Command::new(&tracedecay_bin)
        .arg("post-update")
        .status()
        .map_err(|e| tracedecay::errors::TraceDecayError::Config {
            message: format!("failed to run post-update with '{tracedecay_bin}': {e}"),
        })?;
    if status.success() {
        return Ok(());
    }
    Err(tracedecay::errors::TraceDecayError::Config {
        message: format!("post-update failed with status: {status}"),
    })
}

fn run_post_update_tasks() -> tracedecay::errors::Result<()> {
    refresh_generated_plugins()?;
    if let Err(error) = refresh_daemon_service() {
        eprintln!("  \x1b[33mwarning:\x1b[0m daemon service refresh failed: {error}");
    }
    Ok(())
}

async fn resolve_registered_project_root(
    project_id: Option<String>,
    project_path: Option<String>,
) -> tracedecay::errors::Result<Option<PathBuf>> {
    let db = tracedecay::global_db::GlobalDb::open()
        .await
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: "could not open tracedecay project registry; run tracedecay init first"
                .to_string(),
        })?;
    let context = if let Some(project_id) = project_id.as_deref() {
        db.project_registry_context_by_id(project_id).await
    } else if let Some(project_path) = project_path.as_deref() {
        db.project_registry_context_by_alias(Path::new(project_path))
            .await
    } else {
        return Ok(None);
    };

    context
        .map(|context| PathBuf::from(context.project.display_root))
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: "registered project not found for selector".to_string(),
        })
        .map(Some)
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
            profile,
            all_profiles,
            project_root,
            no_dashboard,
            automation,
        } => {
            agent_cmd::handle_install_command(
                agent,
                local,
                profile,
                all_profiles,
                project_root,
                no_dashboard,
                automation,
            )
            .await?;
        }
        Commands::Reinstall => {
            agent_cmd::handle_reinstall_command().await?;
        }
        Commands::UpdatePlugin => {
            refresh_generated_plugins()?;
        }
        Commands::Uninstall {
            agent,
            profile,
            all_profiles,
        } => {
            agent_cmd::handle_uninstall_command(agent, profile, all_profiles).await?;
        }
        Commands::ExtractWorker => {
            // Handled by the early dispatch at the top of run(); this arm
            // exists only for clap match exhaustiveness.
            unreachable!("extract-worker handled by early dispatch")
        }
        hook_command @ (Commands::HookPreToolUse
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
        | Commands::HookCodexPostCompact) => {
            hook_cmd::handle_hook_command(hook_command).await?;
        }
        Commands::Dashboard {
            path,
            host,
            port,
            open,
        } => {
            let project_path = tracedecay::config::resolve_path_with_discovery(path);
            let cg = serve::ensure_initialized(&project_path).await?;
            tracedecay::dashboard::run(&cg, &host, port, open).await?;
        }
        Commands::Serve { path, timings } => {
            if matches!(std::env::var("DISABLE_TRACEDECAY").as_deref(), Ok("true")) {
                // Allow users to opt out per-project by setting
                // DISABLE_TRACEDECAY=true. The process exits cleanly so the
                // host does not retry.
                return Ok(());
            }
            let original_cwd = std::env::current_dir().ok();
            // Track the first stdin line if we need to peek at `initialize` roots.
            let mut peeked_line: Option<String> = None;
            let explicit_path = path.is_some();
            let project_path = tracedecay::config::resolve_path_with_discovery(path);
            let cg = match serve::ensure_initialized(&project_path).await {
                Ok(cg) => cg,
                Err(e) => {
                    if explicit_path {
                        return Err(e);
                    }
                    // CWD-based discovery failed (e.g. VS Code launched us from ~).
                    // Next try MCP initialize roots from editor workspace context.
                    if let Some(p) = serve::resolve_serve_from_mcp_roots(&mut peeked_line).await {
                        serve::ensure_initialized(&p).await?
                    } else {
                        // Last resort: fall back to the global DB's registered projects.
                        match serve::resolve_serve_from_global_db().await {
                            serve::ServeGlobalDbResolution::Found(p) => {
                                serve::ensure_initialized(&p).await?
                            }
                            serve::ServeGlobalDbResolution::Ambiguous(paths) => {
                                return Err(tracedecay::errors::TraceDecayError::Config {
                                    message: serve::global_db_ambiguity_message(&paths),
                                });
                            }
                            serve::ServeGlobalDbResolution::None => {
                                return Err(tracedecay::errors::TraceDecayError::Config {
                                    message: format!(
                                        "no TraceDecay index found at '{}' and no projects registered in the global database — run 'tracedecay init' in your project first",
                                        project_path.display()
                                    ),
                                });
                            }
                        }
                    }
                }
            };

            // Compute scope prefix: relative path from project root to original cwd
            let scope_prefix = original_cwd.and_then(|cwd| {
                cwd.strip_prefix(cg.project_root())
                    .ok()
                    .filter(|rel| !rel.as_os_str().is_empty())
                    .map(|rel| rel.to_string_lossy().into_owned())
            });

            let handshake = tracedecay::daemon::DaemonHandshake::for_current_client(
                Some(cg.project_root().to_path_buf()),
                scope_prefix,
                timings,
                false,
            )?;
            let socket_path = tracedecay::daemon::default_socket_path()?;
            if socket_path.exists() {
                tracedecay::daemon::proxy_stdio_to_daemon(&socket_path, &handshake, peeked_line)
                    .await?;
            } else {
                let server =
                    tracedecay::mcp::McpServer::new(cg, handshake.scope_prefix.clone()).await;
                let mut transport = ReplayStdioTransport::new(peeked_line);
                server.run(&mut transport).await?;
            }
        }
        Commands::Daemon { action } => match action {
            DaemonAction::Run { socket } => {
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
            DaemonAction::Status => {
                let socket_path = tracedecay::daemon::socket_path_or_default(None)?;
                print!("{}", tracedecay::daemon::service_status(&socket_path));
            }
        },
        Commands::Upgrade => {
            tracedecay::upgrade::run_upgrade()?;
        }
        Commands::Update => {
            run_update_command()?;
        }
        Commands::PostUpdate => {
            run_post_update_tasks()?;
        }
        Commands::Channel { channel } => match channel {
            Some(target) => {
                tracedecay::upgrade::switch_channel(&target)?;
            }
            None => tracedecay::upgrade::show_channel(),
        },
        Commands::CurrentCounter { path } => {
            let project_path = tracedecay::config::resolve_path(path);
            let cg = serve::ensure_initialized(&project_path).await?;
            let value = cg.get_local_counter().await?;
            println!("{value}");
        }
        Commands::ResetCounter { path } => {
            let project_path = tracedecay::config::resolve_path(path);
            let cg = serve::ensure_initialized(&project_path).await?;
            let prev = cg.get_local_counter().await?;
            cg.reset_local_counter().await?;
            eprintln!("Local counter reset (was {prev})");
        }
        Commands::DisableUploadCounter => {
            commands::handle_upload_counter(false);
        }
        Commands::EnableUploadCounter => {
            commands::handle_upload_counter(true);
        }
        Commands::Gitignore { path, action } => {
            commands::handle_gitignore(path, action)?;
        }
        Commands::Doctor { agent } => {
            tracedecay::doctor::run_doctor(agent.as_deref()).await;
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
                let cg = crate::serve::ensure_initialized(&project_path).await?;
                let status = cg.project_memory_status().await?;
                let largest_bank_fact_count =
                    status_cmd::largest_memory_bank_fact_count_at(&cg.store_layout().graph_db_path)
                        .await?;
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

fn should_skip_startup_maintenance(command: &Commands) -> bool {
    matches!(
        command,
        Commands::Install { .. }
            | Commands::Reinstall
            | Commands::UpdatePlugin
            | Commands::Update
            | Commands::PostUpdate
            | Commands::Uninstall { .. }
            | Commands::Lsp { .. }
            | Commands::Doctor { .. }
            | Commands::Migrate { .. }
            | Commands::Projects { .. }
            | Commands::HookPreToolUse
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
            | Commands::Daemon { .. }
            // `Serve` is the hot path used by MCP clients (Claude Code,
            // Codex, etc.). Clients impose a 30 s `initialize` timeout, so
            // every pre-serve startup task — `try_flush` network round-trip,
            // `check_install_stale`, the silent-reinstall loop over every
            // tracked agent — risks pushing us past it on slow networks or
            // big home-dir trees (#84). Skip them; the same maintenance
            // runs on the user's next interactive `tracedecay …` invocation.
            | Commands::Serve { .. }
    )
}

fn should_skip_agent_install_maintenance(command: &Commands) -> bool {
    // Selectively gate the implicit `check_install_stale` + silent-reinstall
    // path so agent permissions/hooks/MCP config stay in sync after a binary
    // upgrade, without firing on paths where it would be wrong or wasteful:
    //   - `Serve`: the MCP hot path with a 30 s client `initialize` timeout
    //     (#84). Reinstalling every tracked agent before the stdio loop starts
    //     can blow that budget, so it must stay off `serve`.
    //   - `Install` / `Reinstall`: already perform installation — don't
    //     double-install as an implicit prelude to the explicit command.
    //   - `UpdatePlugin` / `Update`: explicit maintenance paths that manage
    //     plugin refresh themselves; an implicit silent reinstall beforehand
    //     would rewrite configs and break the update-plugin contract.
    //   - `Uninstall`: about to remove agent configs — don't reinstall them
    //     first (per the original #84 intent).
    //   - `Doctor` / `Migrate`: read-only diagnostics — must not mutate agent
    //     configs as a side effect.
    //   - `Tool`: per-invocation tool calls are a hot-ish path; skip the
    //     reinstall scan there too.
    // Every other command (the normal everyday invocations) runs maintenance.
    matches!(
        command,
        Commands::Serve { .. }
            | Commands::Install { .. }
            | Commands::Reinstall
            | Commands::UpdatePlugin
            | Commands::Update
            | Commands::PostUpdate
            | Commands::Uninstall { .. }
            | Commands::Lsp { .. }
            | Commands::Doctor { .. }
            | Commands::Migrate { .. }
            | Commands::Projects { .. }
            | Commands::Tool { .. }
            | Commands::Daemon { .. }
    )
}

fn is_local_install_command(command: &Commands) -> bool {
    matches!(command, Commands::Install { local: true, .. })
}

#[cfg(test)]
mod startup_tests;

// handle_branch_action, handle_wipe, handle_list, handle_no_command,
// init_and_index, and print_sync_doctor have been moved to src/commands.rs.
//
// update_global_db, try_flush, check_for_update, gather_target_projects,
// gather_local_projects, gather_local_projects_from, find_descendant_tracedecay,
// print_flash_warning, and tracedecay_dir_size have been moved to src/global.rs.
