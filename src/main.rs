// Rust guideline compliant 2025-10-17
// Updated 2026-03-23: compact bordered table for status output
use clap::Parser;
use std::io::{self, BufRead, IsTerminal, Write};
use std::process;

use tracedecay::tracedecay::TraceDecay;

mod cli;
mod commands;
mod global;
mod serve;
mod tool_command;

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
}

impl Spinner {
    pub(crate) fn new() -> Self {
        let message = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let msg = message.clone();
        let stp = stop.clone();
        // Hide cursor while spinner is active.
        let _ = write!(std::io::stderr(), "\x1b[?25l");
        let _ = std::io::stderr().flush();
        let handle = std::thread::spawn(move || {
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
        });
        Self {
            message,
            stop,
            handle: Some(handle),
        }
    }

    pub(crate) fn set_message(&self, msg: &str) {
        if let Ok(mut locked) = self.message.lock() {
            *locked = msg.to_string();
        }
    }

    pub(crate) fn done(mut self, message: &str) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let mut stderr = std::io::stderr();
        // Show cursor again, then print the done line.
        let _ = write!(stderr, "\x1b[?25h");
        let _ = writeln!(stderr, "\r\x1b[2K\x1b[32m✔\x1b[0m {}", message);
        let _ = stderr.flush();
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        // If the spinner wasn't explicitly finished (e.g. `?` propagated an
        // error), still stop the thread, clear the line, and restore the
        // cursor so the terminal is left in a sane state.
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let mut stderr = std::io::stderr();
        let _ = write!(stderr, "\r\x1b[2K\x1b[?25h");
        let _ = stderr.flush();
    }
}

fn hermes_profile_targets(
    home: &std::path::Path,
) -> tracedecay::errors::Result<Vec<Option<String>>> {
    let mut targets = vec![None];
    let profiles_dir = home.join(".hermes/profiles");
    let entries = match std::fs::read_dir(&profiles_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(targets),
        Err(e) => {
            return Err(tracedecay::errors::TraceDecayError::Config {
                message: format!(
                    "failed to read Hermes profiles directory {}: {e}",
                    profiles_dir.display()
                ),
            });
        }
    };

    let mut profile_names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "failed to read Hermes profiles directory {}: {e}",
                profiles_dir.display()
            ),
        })?;
        let file_type =
            entry
                .file_type()
                .map_err(|e| tracedecay::errors::TraceDecayError::Config {
                    message: format!(
                        "failed to inspect Hermes profile {}: {e}",
                        entry.path().display()
                    ),
                })?;
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name().into_string().map_err(|_| {
            tracedecay::errors::TraceDecayError::Config {
                message: format!(
                    "Hermes profile path is not valid UTF-8: {}",
                    entry.path().display()
                ),
            }
        })?;
        profile_names.push(name);
    }
    profile_names.sort();
    targets.extend(profile_names.into_iter().map(Some));
    Ok(targets)
}

fn validate_hermes_profile_flags(
    agent: Option<&str>,
    profile: &Option<String>,
    all_profiles: bool,
) -> tracedecay::errors::Result<()> {
    if profile.is_some() && agent != Some("hermes") {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "`--profile` is only supported with `--agent hermes`".to_string(),
        });
    }
    if all_profiles && agent != Some("hermes") {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "`--all-profiles` is only supported with `--agent hermes`".to_string(),
        });
    }
    Ok(())
}

/// Validates `--project-root` (Hermes plugin project pin): hermes-only and
/// absolute, so the generated plugin never depends on the install cwd.
fn validate_hermes_project_root_flag(
    agent: Option<&str>,
    project_root: &Option<String>,
) -> tracedecay::errors::Result<Option<std::path::PathBuf>> {
    let Some(project_root) = project_root else {
        return Ok(None);
    };
    if agent != Some("hermes") {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "`--project-root` is only supported with `--agent hermes`".to_string(),
        });
    }
    let path = std::path::PathBuf::from(project_root);
    if !path.is_absolute() {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: format!("`--project-root` must be an absolute path, got '{project_root}'"),
        });
    }
    Ok(Some(path))
}

fn hermes_selected_profile_targets(
    home: &std::path::Path,
    profile: &Option<String>,
    all_profiles: bool,
) -> tracedecay::errors::Result<Vec<Option<String>>> {
    if all_profiles {
        hermes_profile_targets(home)
    } else {
        Ok(vec![profile.clone()])
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
    let cli = Cli::parse();
    let spawned = std::thread::Builder::new()
        .name("tracedecay-main".to_string())
        .stack_size(ASYNC_STACK_BYTES)
        .spawn(move || async_main(cli));
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

fn async_main(cli: Cli) -> tracedecay::errors::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(ASYNC_STACK_BYTES)
        .build()
        .map_err(|e| tracedecay::errors::TraceDecayError::Config {
            message: format!("failed to start async runtime: {e}"),
        })?;
    runtime.block_on(run(cli))
}

async fn run(cli: Cli) -> tracedecay::errors::Result<()> {
    let command = match cli.command {
        Some(cmd) => cmd,
        None => return commands::handle_no_command().await,
    };

    // Worker mode bypasses every normal startup path (no config load, no
    // worldwide-counter ping, no agent checks). The token handshake inside
    // run_worker is the only authentication; this dispatch must happen
    // before anything else can side-effect on disk or network.
    if matches!(command, Commands::ExtractWorker) {
        tracedecay::extraction_worker::run_worker();
    }

    let skip_startup_maintenance = should_skip_startup_maintenance(&command);
    let skip_agent_install_maintenance = should_skip_agent_install_maintenance(&command);

    // First-run notice (check BEFORE any config save creates the file)
    let is_first_run = tracedecay::user_config::UserConfig::is_fresh();

    // Best-effort flush of pending worldwide counter tokens.
    // `matches!` borrows `command` temporarily; the borrow is dropped
    // before the `match command` move below, so this compiles.
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
    if !is_local_install_command(&command) {
        user_config.save_if_exists();
    }

    if is_first_run && !skip_startup_maintenance {
        eprintln!(
            "note: tracedecay uploads anonymous token-saved counts to a worldwide counter.\n\
             \x20     Run `tracedecay disable-upload-counter` to opt out."
        );
    }

    // The "beta merged into stable" nudge that lived here through 4.3.x was
    // retired in 4.3.12. The beta channel is open again as of v5.0.0-beta.1
    // and beta users now stay on beta until they explicitly switch off.

    // Best-effort check: warn if install needs re-running.
    if !skip_agent_install_maintenance {
        tracedecay::agents::claude::check_install_stale();
    }

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
    if !skip_agent_install_maintenance {
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
                || tracedecay::cloud::is_newer_version(
                    &user_config.last_installed_version,
                    running,
                ));
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
            // Patch-only bump (or nothing to reinstall) — advance the marker
            // so we don't keep re-checking on every subsequent startup.
            user_config.previous_version = running.to_string();
            user_config.save();
        }
    }

    match command {
        Commands::Init { path, skip_folders } => {
            let project_path = tracedecay::config::resolve_path(path);
            if TraceDecay::is_initialized(&project_path) {
                eprintln!(
                    "\x1b[31merror:\x1b[0m TraceDecay is already initialized at '{}'.\n\
                     Use \x1b[1mtracedecay sync\x1b[0m to update the index, or \
                     \x1b[1mtracedecay sync --force\x1b[0m to rebuild it.",
                    project_path.display()
                );
                std::process::exit(1);
            }
            // Check for updates in parallel with indexing
            let version_handle = std::thread::spawn(tracedecay::cloud::fetch_latest_version);
            commands::init_and_index(&project_path, &skip_folders, false).await?;

            // Print update notice from parallel check (suppressed for 15 min)
            if let Ok(Some(latest)) = version_handle.join() {
                let current_version = env!("CARGO_PKG_VERSION");
                let now = current_unix_timestamp();
                let mut config = tracedecay::user_config::UserConfig::load();
                config.cached_latest_version = latest.clone();
                config.last_version_check_at = now;
                config.save_if_exists();
                if tracedecay::cloud::is_newer_version(current_version, &latest)
                    && now - config.last_version_warning_at >= 900
                {
                    eprintln!(
                        "\n\x1b[33mUpdate available: v{} → v{}\x1b[0m\n  Run: \x1b[1mtracedecay upgrade\x1b[0m",
                        current_version, latest
                    );
                    config.last_version_warning_at = now;
                    config.save_if_exists();
                }
            }
        }
        Commands::Sync {
            path,
            force,
            skip_folders,
            doctor,
            verbose,
        } => {
            let project_path = tracedecay::config::resolve_path_with_discovery(path);
            if !TraceDecay::is_initialized(&project_path) {
                eprintln!(
                    "\x1b[31merror:\x1b[0m no TraceDecay index found at '{}'.\n\
                     Run \x1b[1mtracedecay init\x1b[0m to create one first.",
                    project_path.display()
                );
                std::process::exit(1);
            }
            // Warn if legacy .codegraph directory exists
            if project_path.join(".codegraph").is_dir() {
                eprintln!(
                    "warning: found legacy .codegraph/ directory at '{}'. \
                     tracedecay now uses .tracedecay/ — the old directory can be safely deleted.",
                    project_path.display()
                );
            }
            // Check for updates in parallel with indexing
            let version_handle = std::thread::spawn(tracedecay::cloud::fetch_latest_version);

            if force {
                commands::init_and_index(&project_path, &skip_folders, verbose).await?;
            } else {
                let mut cg = TraceDecay::open(&project_path).await?;
                cg.add_skip_folders(&skip_folders);
                let spinner = Spinner::new();
                let sync_start = std::time::Instant::now();
                let result = cg
                    .sync_with_progress_verbose(
                        |current, total, detail| {
                            if current == 0 {
                                // Phase message (scanning, hashing, detecting, resolving)
                                spinner.set_message(detail);
                            } else {
                                // Per-file progress with ETA
                                let elapsed = sync_start.elapsed().as_secs_f64();
                                let eta = if current > 1 {
                                    let per_file = elapsed / (current - 1) as f64;
                                    let remaining = per_file * (total - current) as f64;
                                    if remaining >= 1.0 {
                                        format!(" (ETA: {remaining:.0}s)")
                                    } else {
                                        String::new()
                                    }
                                } else {
                                    String::new()
                                };
                                spinner.set_message(&format!(
                                    "[{current}/{total}] syncing {detail}{eta}"
                                ));
                            }
                        },
                        |msg| {
                            if verbose {
                                eprintln!("  \x1b[2m[verbose]\x1b[0m {msg}");
                            }
                        },
                    )
                    .await?;
                let skipped_msg = if result.skipped_paths.is_empty() {
                    String::new()
                } else {
                    format!(", {} skipped", result.skipped_paths.len())
                };
                spinner.done(&format!(
                    "sync done — {} added, {} modified, {} removed{skipped_msg} in {}ms",
                    result.files_added,
                    result.files_modified,
                    result.files_removed,
                    result.duration_ms
                ));
                if !result.skipped_paths.is_empty() {
                    eprintln!();
                    eprintln!(
                        "\x1b[33mSkipped ({}) — files found but not readable:\x1b[0m",
                        result.skipped_paths.len()
                    );
                    for (path, reason) in &result.skipped_paths {
                        eprintln!("  ! {path}: {reason}");
                    }
                }
                if doctor {
                    commands::print_sync_doctor(&result);
                }
                global::update_global_db(&cg).await;
            }

            // Print update notice from parallel check (suppressed for 15 min)
            if let Ok(Some(latest)) = version_handle.join() {
                let current_version = env!("CARGO_PKG_VERSION");
                let now = current_unix_timestamp();
                let mut config = tracedecay::user_config::UserConfig::load();
                config.cached_latest_version = latest.clone();
                config.last_version_check_at = now;
                config.save_if_exists();
                if tracedecay::cloud::is_newer_version(current_version, &latest)
                    && now - config.last_version_warning_at >= 900
                {
                    eprintln!(
                        "\n\x1b[33mUpdate available: v{} → v{}\x1b[0m\n  Run: \x1b[1mtracedecay upgrade\x1b[0m",
                        current_version, latest
                    );
                    config.last_version_warning_at = now;
                    config.save_if_exists();
                }
            }
        }
        Commands::Status {
            path,
            json,
            short,
            details,
            runtime,
        } => {
            let project_path = tracedecay::config::resolve_path_with_discovery(path);
            let cg = if TraceDecay::is_initialized(&project_path) {
                TraceDecay::open(&project_path).await?
            } else if !io::stdin().is_terminal() {
                eprintln!(
                    "No TraceDecay index found at '{}'. Non-interactive: skipping index creation (run `tracedecay init`).",
                    project_path.display()
                );
                return Ok(());
            } else {
                eprint!(
                    "No TraceDecay index found at '{}'. Create one now? [Y/n] ",
                    project_path.display()
                );
                io::stderr().flush().ok();
                let mut answer = String::new();
                io::stdin().lock().read_line(&mut answer).map_err(|e| {
                    tracedecay::errors::TraceDecayError::Config {
                        message: format!("failed to read stdin: {e}"),
                    }
                })?;
                let answer = answer.trim();
                if answer.is_empty() || answer.eq_ignore_ascii_case("y") {
                    commands::init_and_index(&project_path, &[], false).await?
                } else {
                    return Ok(());
                }
            };
            if runtime {
                let snap = tracedecay::runtime_telemetry::collect(&cg).await?;
                if json {
                    println!("{}", tracedecay::runtime_telemetry::to_pretty_json(&snap));
                } else {
                    print!("{}", tracedecay::runtime_telemetry::to_text_report(&snap));
                }
                return Ok(());
            }
            let stats = cg.get_stats().await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&stats).unwrap_or_default()
                );
            } else {
                let tokens_saved = cg.get_tokens_saved().await.unwrap_or(0);
                // Register project and read global total in one open.
                // Subtract this project's count so "Global" means "all other projects".
                let gdb = tracedecay::global_db::GlobalDb::open().await;
                let global_tokens_saved = match &gdb {
                    Some(db) => {
                        db.upsert(&project_path, tokens_saved).await;
                        db.global_tokens_saved()
                            .await
                            .map(|total| total.saturating_sub(tokens_saved))
                            .filter(|&other| other > 0)
                    }
                    None => None,
                };
                // Fetch worldwide total (1s timeout, 60s client cache TTL)
                let mut config = tracedecay::user_config::UserConfig::load();
                let now = current_unix_timestamp();
                let worldwide = if now - config.last_worldwide_fetch_at < 60 {
                    // Use cached value
                    if config.last_worldwide_total > 0 {
                        Some(config.last_worldwide_total)
                    } else {
                        None
                    }
                } else if let Some(total) = tracedecay::cloud::fetch_worldwide_total() {
                    config.last_worldwide_total = total;
                    config.last_worldwide_fetch_at = now;
                    config.save_if_exists();
                    Some(total)
                } else if config.last_worldwide_total > 0 {
                    Some(config.last_worldwide_total) // fallback to cache
                } else {
                    None
                };
                // Fetch country flags (30 min cache)
                let country_flags = if now - config.last_flags_fetch_at < 1800 {
                    config.cached_country_flags.clone()
                } else {
                    let fresh = tracedecay::cloud::fetch_country_flags();
                    if !fresh.is_empty() {
                        config.cached_country_flags = fresh.clone();
                        config.last_flags_fetch_at = now;
                        config.save_if_exists();
                    }
                    if fresh.is_empty() && !config.cached_country_flags.is_empty() {
                        config.cached_country_flags.clone()
                    } else {
                        fresh
                    }
                };
                if !short {
                    print!("{}", include_str!("resources/logo.ansi"));
                }
                let branch_info = cg.active_branch().map(|_| {
                    let ts_dir = tracedecay::config::get_tracedecay_dir(&project_path);
                    let meta = tracedecay::branch_meta::load_branch_meta(&ts_dir);
                    let has_tracking = meta.as_ref().is_some_and(|m| !m.branches.is_empty());
                    let display_branch = if has_tracking {
                        cg.serving_branch().unwrap_or("[single-db]").to_string()
                    } else {
                        "[single-db]".to_string()
                    };
                    let parent =
                        meta.and_then(|m| m.branches.get(cg.serving_branch()?)?.parent.clone());
                    tracedecay::display::BranchInfo {
                        branch: display_branch,
                        parent,
                        is_fallback: cg.is_fallback(),
                    }
                });
                // Ingest new session data so cost info is up-to-date.
                if let Some(ref db) = gdb {
                    tracedecay::accounting::parser::ingest(db).await;
                }
                // Best-effort cost summary for the status header.
                let cost_info = match &gdb {
                    Some(db) => {
                        tracedecay::accounting::quick_cost_summary(
                            db,
                            tokens_saved,
                            global_tokens_saved.unwrap_or(0),
                        )
                        .await
                    }
                    None => None,
                };
                if short {
                    tracedecay::display::print_status_header(
                        &stats,
                        tokens_saved,
                        global_tokens_saved,
                        worldwide,
                        &country_flags,
                        branch_info.as_ref(),
                        cost_info.as_ref(),
                    );
                } else {
                    tracedecay::display::print_status_table(
                        &stats,
                        tokens_saved,
                        global_tokens_saved,
                        worldwide,
                        &country_flags,
                        branch_info.as_ref(),
                        cost_info.as_ref(),
                        details,
                    );
                }

                // Warn if the data dir is not in .gitignore
                if !tracedecay::config::is_in_gitignore(&project_path) {
                    let dir_name = tracedecay::config::active_data_dir_name(&project_path);
                    eprintln!(
                        "\n\x1b[33mWarning: {dir_name} is not in .gitignore — \
                         run `echo {dir_name} >> .gitignore` to exclude it from git.\x1b[0m"
                    );
                }

                // Version check (5 min cache, always show for status)
                global::check_for_update(&mut config, false, true);
            }
        }
        Commands::Tool {
            project,
            name,
            args,
        } => {
            tool_command::run(project, name, args).await?;
        }
        Commands::Install {
            agent,
            local,
            profile,
            all_profiles,
            project_root,
            no_dashboard,
        } => {
            validate_hermes_profile_flags(agent.as_deref(), &profile, all_profiles)?;
            let pinned_project_root =
                validate_hermes_project_root_flag(agent.as_deref(), &project_root)?;
            let home = tracedecay::agents::home_dir().ok_or_else(|| {
                tracedecay::errors::TraceDecayError::Config {
                    message: "could not determine home directory".to_string(),
                }
            })?;
            let tracedecay_bin = tracedecay::agents::which_tracedecay().ok_or_else(|| {
                tracedecay::errors::TraceDecayError::Config {
                    message: "tracedecay not found on PATH. Install it from this repo first:\n  \
                          cargo binstall --git https://github.com/ScriptedAlchemy/tokensave tracedecay\n  \
                          cargo install --git https://github.com/ScriptedAlchemy/tokensave tracedecay"
                        .to_string(),
                }
            })?;
            if local {
                let project_path = std::env::current_dir().map_err(|e| {
                    tracedecay::errors::TraceDecayError::Config {
                        message: format!("could not determine current project directory: {e}"),
                    }
                })?;
                let ctx = tracedecay::agents::InstallContext {
                    home: home.clone(),
                    tracedecay_bin: tracedecay_bin.clone(),
                    tool_permissions: tracedecay::agents::expected_tool_perms(),
                    profile: profile.clone(),
                    project_root: pinned_project_root.clone(),
                    dashboard: !no_dashboard,
                };
                let mut installed_names: Vec<String> = Vec::new();

                if let Some(id) = agent {
                    let ag = tracedecay::agents::get_integration(&id)?;
                    for target_profile in
                        hermes_selected_profile_targets(&home, &profile, all_profiles)?
                    {
                        let ctx = tracedecay::agents::InstallContext {
                            home: home.clone(),
                            tracedecay_bin: tracedecay_bin.clone(),
                            tool_permissions: tracedecay::agents::expected_tool_perms(),
                            profile: target_profile,
                            project_root: pinned_project_root.clone(),
                            dashboard: !no_dashboard,
                        };
                        ag.install_local(&ctx, &project_path)?;
                        ag.post_install(Some(&project_path)).await;
                    }
                    installed_names.push(ag.name().to_string());
                } else {
                    let (to_install, _) =
                        tracedecay::agents::pick_integrations_interactive(&home, &[])?;
                    for id in &to_install {
                        let ag = tracedecay::agents::get_integration(id)?;
                        if ag.supports_local_install() {
                            ag.install_local(&ctx, &project_path)?;
                            ag.post_install(Some(&project_path)).await;
                            installed_names.push(ag.name().to_string());
                        } else {
                            eprintln!(
                                "Skipping {}: project-local install is not supported",
                                ag.name()
                            );
                        }
                    }
                }

                eprintln!();
                if installed_names.is_empty() {
                    eprintln!("No local changes.");
                } else {
                    for name in &installed_names {
                        eprintln!("\x1b[32m+\x1b[0m {name} (local)");
                    }
                }
                return Ok(());
            }

            let mut user_cfg = tracedecay::user_config::UserConfig::load();
            tracedecay::agents::migrate_installed_agents(&home, &mut user_cfg);

            let mut installed_names: Vec<String> = Vec::new();
            let mut removed_names: Vec<String> = Vec::new();
            let project_path = std::env::current_dir().ok();

            if let Some(id) = agent {
                let ag = tracedecay::agents::get_integration(&id)?;
                let name = ag.name().to_string();
                for target_profile in
                    hermes_selected_profile_targets(&home, &profile, all_profiles)?
                {
                    let ctx = tracedecay::agents::InstallContext {
                        home: home.clone(),
                        tracedecay_bin: tracedecay_bin.clone(),
                        tool_permissions: tracedecay::agents::expected_tool_perms(),
                        profile: target_profile,
                        project_root: pinned_project_root.clone(),
                        dashboard: !no_dashboard,
                    };
                    ag.install(&ctx)?;
                    ag.post_install(project_path.as_deref()).await;
                }
                if !user_cfg.installed_agents.contains(&id) {
                    user_cfg.installed_agents.push(id);
                    installed_names.push(name);
                }
                user_cfg.save();
            } else {
                let (to_install, to_uninstall) = tracedecay::agents::pick_integrations_interactive(
                    &home,
                    &user_cfg.installed_agents,
                )?;

                for id in &to_uninstall {
                    let ag = tracedecay::agents::get_integration(id)?;
                    let ctx = tracedecay::agents::InstallContext {
                        home: home.clone(),
                        tracedecay_bin: tracedecay_bin.clone(),
                        tool_permissions: tracedecay::agents::expected_tool_perms(),
                        profile: profile.clone(),
                        project_root: pinned_project_root.clone(),
                        dashboard: !no_dashboard,
                    };
                    ag.uninstall(&ctx)?;
                    removed_names.push(ag.name().to_string());
                    user_cfg.installed_agents.retain(|a| a != id);
                }
                for id in &to_install {
                    let ag = tracedecay::agents::get_integration(id)?;
                    let ctx = tracedecay::agents::InstallContext {
                        home: home.clone(),
                        tracedecay_bin: tracedecay_bin.clone(),
                        tool_permissions: tracedecay::agents::expected_tool_perms(),
                        profile: profile.clone(),
                        project_root: pinned_project_root.clone(),
                        dashboard: !no_dashboard,
                    };
                    ag.install(&ctx)?;
                    ag.post_install(project_path.as_deref()).await;
                    installed_names.push(ag.name().to_string());
                    if !user_cfg.installed_agents.contains(id) {
                        user_cfg.installed_agents.push(id.clone());
                    }
                }
                user_cfg.save();
            }

            eprintln!();
            if installed_names.is_empty() && removed_names.is_empty() {
                eprintln!("No changes.");
            } else {
                for name in &installed_names {
                    eprintln!("\x1b[32m+\x1b[0m {name}");
                }
                for name in &removed_names {
                    eprintln!("\x1b[31m-\x1b[0m {name}");
                }
            }

            user_cfg.last_installed_version = env!("CARGO_PKG_VERSION").to_string();
            user_cfg.save();

            tracedecay::agents::offer_git_post_commit_hook(&tracedecay_bin);
        }
        Commands::Reinstall => {
            let home = tracedecay::agents::home_dir().ok_or_else(|| {
                tracedecay::errors::TraceDecayError::Config {
                    message: "could not determine home directory".to_string(),
                }
            })?;
            let tracedecay_bin = tracedecay::agents::which_tracedecay().ok_or_else(|| {
                tracedecay::errors::TraceDecayError::Config {
                    message: "tracedecay not found on PATH".to_string(),
                }
            })?;
            let mut user_cfg = tracedecay::user_config::UserConfig::load();
            tracedecay::agents::migrate_installed_agents(&home, &mut user_cfg);

            if user_cfg.installed_agents.is_empty() {
                eprintln!("No installed agents found. Run `tracedecay install` first.");
            } else {
                let agents = user_cfg.installed_agents.clone();
                let project_path = std::env::current_dir().ok();
                eprintln!(
                    "Reinstalling {} agent(s): {}",
                    agents.len(),
                    agents.join(", ")
                );
                for id in &agents {
                    let ag = tracedecay::agents::get_integration(id)?;
                    let ctx = tracedecay::agents::InstallContext {
                        home: home.clone(),
                        tracedecay_bin: tracedecay_bin.clone(),
                        tool_permissions: tracedecay::agents::expected_tool_perms(),
                        profile: None,
                        project_root: None,
                        dashboard: true,
                    };
                    ag.install(&ctx)?;
                    ag.post_install(project_path.as_deref()).await;
                }
                eprintln!("\x1b[32m✔\x1b[0m All agents reinstalled");
                user_cfg.last_installed_version = env!("CARGO_PKG_VERSION").to_string();
                user_cfg.save();
            }
        }
        Commands::UpdatePlugin => {
            let home = tracedecay::agents::home_dir().ok_or_else(|| {
                tracedecay::errors::TraceDecayError::Config {
                    message: "could not determine home directory".to_string(),
                }
            })?;
            let tracedecay_bin = tracedecay::agents::which_tracedecay().ok_or_else(|| {
                tracedecay::errors::TraceDecayError::Config {
                    message: "tracedecay not found on PATH".to_string(),
                }
            })?;
            eprintln!(
                "Refreshing tracedecay-generated plugin artifacts (agent configs are not touched)"
            );

            // Detection-driven, not `installed_agents`-driven: each
            // integration decides whether generated artifacts exist on this
            // machine, so stale tracking state can neither skip a real
            // install nor install anywhere new.
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
        }
        Commands::Uninstall {
            agent,
            profile,
            all_profiles,
        } => {
            validate_hermes_profile_flags(agent.as_deref(), &profile, all_profiles)?;
            let home = tracedecay::agents::home_dir().ok_or_else(|| {
                tracedecay::errors::TraceDecayError::Config {
                    message: "could not determine home directory".to_string(),
                }
            })?;
            let mut user_cfg = tracedecay::user_config::UserConfig::load();
            tracedecay::agents::migrate_installed_agents(&home, &mut user_cfg);

            if let Some(id) = agent {
                let ag = tracedecay::agents::get_integration(&id)?;
                for target_profile in
                    hermes_selected_profile_targets(&home, &profile, all_profiles)?
                {
                    let ctx = tracedecay::agents::InstallContext {
                        home: home.clone(),
                        tracedecay_bin: String::new(),
                        tool_permissions: tracedecay::agents::expected_tool_perms(),
                        profile: target_profile,
                        project_root: None,
                        dashboard: true,
                    };
                    ag.uninstall(&ctx)?;
                }
                user_cfg.installed_agents.retain(|a| a != &id);
                user_cfg.save();
            } else {
                for id in user_cfg.installed_agents.clone() {
                    if let Ok(ag) = tracedecay::agents::get_integration(&id) {
                        let ctx = tracedecay::agents::InstallContext {
                            home: home.clone(),
                            tracedecay_bin: String::new(),
                            tool_permissions: tracedecay::agents::expected_tool_perms(),
                            profile: None,
                            project_root: None,
                            dashboard: true,
                        };
                        ag.uninstall(&ctx).ok();
                    }
                }
                user_cfg.installed_agents.clear();
                user_cfg.save();
                eprintln!("All agent integrations removed.");
            }
        }
        Commands::ExtractWorker => {
            // Handled by the early dispatch at the top of run(); this arm
            // exists only for clap match exhaustiveness.
            unreachable!("extract-worker handled by early dispatch")
        }
        Commands::HookPreToolUse => {
            tracedecay::hooks::hook_pre_tool_use();
        }
        Commands::HookPromptSubmit => {
            tracedecay::hooks::hook_prompt_submit().await;
        }
        Commands::HookStop => {
            tracedecay::hooks::hook_stop().await;
        }
        Commands::HookKiroPreToolUse => {
            let code = tracedecay::hooks::hook_kiro_pre_tool_use();
            if code != 0 {
                process::exit(code);
            }
        }
        Commands::HookKiroPromptSubmit => {
            let code = tracedecay::hooks::hook_kiro_prompt_submit().await;
            if code != 0 {
                process::exit(code);
            }
        }
        Commands::HookKiroPostToolUse => {
            let code = tracedecay::hooks::hook_kiro_post_tool_use().await;
            if code != 0 {
                process::exit(code);
            }
        }
        Commands::HookCursorSubagentStart => {
            let code = tracedecay::hooks::hook_cursor_subagent_start();
            if code != 0 {
                process::exit(code);
            }
        }
        Commands::HookCursorPostToolUse => {
            let code = tracedecay::hooks::hook_cursor_post_tool_use();
            if code != 0 {
                process::exit(code);
            }
        }
        Commands::HookCursorBeforeSubmitPrompt => {
            let code = tracedecay::hooks::hook_cursor_before_submit_prompt().await;
            if code != 0 {
                process::exit(code);
            }
        }
        Commands::HookCursorAfterFileEdit => {
            let code = tracedecay::hooks::hook_cursor_after_file_edit().await;
            if code != 0 {
                process::exit(code);
            }
        }
        Commands::HookCursorSessionStart => {
            let code = tracedecay::hooks::hook_cursor_session_start().await;
            if code != 0 {
                process::exit(code);
            }
        }
        Commands::HookCursorSessionEnd => {
            let code = tracedecay::hooks::hook_cursor_session_end().await;
            if code != 0 {
                process::exit(code);
            }
        }
        Commands::HookCursorAfterShell => {
            let code = tracedecay::hooks::hook_cursor_after_shell().await;
            if code != 0 {
                process::exit(code);
            }
        }
        Commands::HookCursorWorkspaceOpen => {
            let code = tracedecay::hooks::hook_cursor_workspace_open().await;
            if code != 0 {
                process::exit(code);
            }
        }
        Commands::HookCursorStop => {
            let code = tracedecay::hooks::hook_cursor_stop().await;
            if code != 0 {
                process::exit(code);
            }
        }
        Commands::HookCodexSessionStart => {
            let code = tracedecay::hooks::hook_codex_session_start().await;
            if code != 0 {
                process::exit(code);
            }
        }
        Commands::HookCodexUserPromptSubmit => {
            let code = tracedecay::hooks::hook_codex_user_prompt_submit().await;
            if code != 0 {
                process::exit(code);
            }
        }
        Commands::HookCodexSubagentStart => {
            let code = tracedecay::hooks::hook_codex_subagent_start();
            if code != 0 {
                process::exit(code);
            }
        }
        Commands::HookCodexPostToolUse => {
            let code = tracedecay::hooks::hook_codex_post_tool_use().await;
            if code != 0 {
                process::exit(code);
            }
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
            if matches!(std::env::var("DISABLE_TRACEDECAY").as_deref(), Ok("true"))
                || matches!(std::env::var("DISABLE_TOKENSAVE").as_deref(), Ok("true"))
            {
                // Allow users to opt out per-project by setting
                // DISABLE_TRACEDECAY=true (legacy DISABLE_TOKENSAVE still supported).
                // The process exits cleanly so the host does not retry.
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

            let server = tracedecay::mcp::McpServer::new(cg, scope_prefix).await;
            server.set_timings_enabled(timings);
            let mut transport = tracedecay::mcp::StdioTransport::new();
            // If we peeked at stdin to read `initialize` roots, replay that line.
            if let Some(line) = peeked_line {
                server.handle_and_write(&line, &mut transport).await?;
            }
            server.run(&mut transport).await?;
            server.shutdown().await;
        }
        Commands::Upgrade => {
            tracedecay::upgrade::run_upgrade()?;
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
            let mut config = tracedecay::user_config::UserConfig::load();
            config.upload_enabled = false;
            config.save();
            eprintln!("Worldwide counter upload disabled. You can re-enable with `tracedecay enable-upload-counter`.");
        }
        Commands::EnableUploadCounter => {
            let mut config = tracedecay::user_config::UserConfig::load();
            config.upload_enabled = true;
            config.save();
            eprintln!("Worldwide counter upload enabled.");
        }
        Commands::Gitignore { path, action } => {
            let project_path = tracedecay::config::resolve_path(path);
            let mut config = tracedecay::config::load_config(&project_path)?;
            match action.as_deref() {
                Some("on") => {
                    config.git_ignore = true;
                    tracedecay::config::save_config(&project_path, &config)?;
                    eprintln!(
                        "gitignore enabled — .gitignore rules will be respected during indexing."
                    );
                    eprintln!("Run `tracedecay sync` to re-index with the new setting.");
                }
                Some("off") => {
                    config.git_ignore = false;
                    tracedecay::config::save_config(&project_path, &config)?;
                    eprintln!(
                        "gitignore disabled — .gitignore rules will be ignored during indexing."
                    );
                    eprintln!("Run `tracedecay sync` to re-index with the new setting.");
                }
                Some(other) => {
                    return Err(tracedecay::errors::TraceDecayError::Config {
                        message: format!("unknown action '{other}': expected 'on' or 'off'"),
                    });
                }
                None => {
                    let status = if config.git_ignore { "on" } else { "off" };
                    eprintln!("gitignore: {status}");
                }
            }
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
            // Refresh LiteLLM pricing if cache is older than 24h
            tracedecay::accounting::pricing::refresh_if_stale();

            let gdb = match tracedecay::global_db::GlobalDb::open().await {
                Some(db) => db,
                None => {
                    eprintln!("Could not open global database.");
                    process::exit(1);
                }
            };

            // Ingest new session data before querying
            let ingest_stats = tracedecay::accounting::parser::ingest(&gdb).await;
            if ingest_stats.turns_inserted > 0 {
                eprintln!(
                    "Ingested {} new turns from Claude Code sessions.",
                    ingest_stats.turns_inserted
                );
            }

            let since = tracedecay::accounting::metrics::parse_range(&range);
            let tokens_saved = gdb.global_tokens_saved().await.unwrap_or(0);
            let summary =
                tracedecay::accounting::metrics::cost_summary(&gdb, since, tokens_saved).await;

            let Some(s) = summary else {
                println!("No session data found. Use Claude Code and then run `tracedecay cost` to see spending.");
                return Ok(());
            };

            if let Some(ref fmt) = export {
                match fmt.as_str() {
                    "json" => {
                        let obj = serde_json::json!({
                            "range": range,
                            "total_cost_usd": s.total_cost,
                            "total_input_tokens": s.total_input_tokens,
                            "total_output_tokens": s.total_output_tokens,
                            "tokens_saved": s.tokens_saved,
                            "efficiency_ratio": s.efficiency_ratio,
                            "by_model": s.by_model.iter().map(|(m, c, t)| serde_json::json!({"model": m, "cost": c, "tokens": t})).collect::<Vec<_>>(),
                            "by_category": s.by_category.iter().map(|(cat, c, n)| serde_json::json!({"category": cat, "cost": c, "turns": n})).collect::<Vec<_>>(),
                        });
                        println!("{}", serde_json::to_string_pretty(&obj).unwrap_or_default());
                    }
                    "csv" => {
                        if by_model {
                            println!("model,cost_usd,tokens");
                            for (model, cost, tokens) in &s.by_model {
                                println!("{model},{cost:.4},{tokens}");
                            }
                        } else if by_task {
                            println!("category,cost_usd,turns");
                            for (cat, cost, turns) in &s.by_category {
                                println!("{cat},{cost:.4},{turns}");
                            }
                        } else {
                            println!(
                                "total_cost_usd,input_tokens,output_tokens,tokens_saved,efficiency"
                            );
                            println!(
                                "{:.4},{},{},{},{:.4}",
                                s.total_cost,
                                s.total_input_tokens,
                                s.total_output_tokens,
                                s.tokens_saved,
                                s.efficiency_ratio
                            );
                        }
                    }
                    _ => eprintln!("Unknown export format '{fmt}'. Use 'json' or 'csv'."),
                }
            } else if by_model {
                let total = s.total_cost.max(0.001);
                println!(
                    "  {:<24} {:>10} {:>10} {:>6}",
                    "Model", "Cost", "Tokens", "Share"
                );
                for (model, cost, tokens) in &s.by_model {
                    let share = cost / total * 100.0;
                    let tok_str = tracedecay::display::format_token_count(*tokens);
                    println!(
                        "  {:<24} {:>9} {:>10} {:>5.0}%",
                        model,
                        format!("${cost:.2}"),
                        tok_str,
                        share
                    );
                }
            } else if by_task {
                println!("  {:<16} {:>10} {:>6}", "Category", "Cost", "Turns");
                for (cat, cost, turns) in &s.by_category {
                    println!("  {:<16} {:>9} {:>6}", cat, format!("${cost:.2}"), turns);
                }
            } else {
                // Default summary
                let today_since = tracedecay::accounting::metrics::parse_range("today");
                let today_cost = gdb.total_cost_since(today_since).await.unwrap_or(0.0);
                let today_breakdown = gdb
                    .token_breakdown_since(today_since)
                    .await
                    .unwrap_or((0, 0, 0));

                let fmt_row = |label: &str, cost: f64, input: u64, output: u64, cache_read: u64| {
                    let input_s = tracedecay::display::format_token_count(input);
                    let output_s = tracedecay::display::format_token_count(output);
                    let cache_pct = if input + cache_read > 0 {
                        (cache_read as f64 / (input + cache_read) as f64) * 100.0
                    } else {
                        0.0
                    };
                    println!(
                        "  {:<10} {:>9} {:>10} {:>10} {:>9.0}%",
                        label,
                        format!("${cost:.2}"),
                        input_s,
                        output_s,
                        cache_pct
                    );
                };

                println!(
                    "  {:<10} {:>10} {:>10} {:>10} {:>10}",
                    "Period", "Cost", "Input", "Output", "Cache-hit"
                );
                fmt_row(
                    "Today",
                    today_cost,
                    today_breakdown.0,
                    today_breakdown.1,
                    today_breakdown.2,
                );
                fmt_row(
                    &range,
                    s.total_cost,
                    s.total_input_tokens,
                    s.total_output_tokens,
                    s.total_cache_read_tokens,
                );

                if s.tokens_saved > 0 {
                    let saved_str = tracedecay::display::format_token_count(s.tokens_saved);
                    println!();
                    println!(
                        "  Savings  {} tokens ({:.0}% efficiency)",
                        saved_str,
                        s.efficiency_ratio * 100.0
                    );
                }
            }
        }
        Commands::Bench {
            queries,
            json,
            path,
            max_nodes,
        } => {
            let project_path = tracedecay::config::resolve_path(path);
            let cg = serve::ensure_initialized(&project_path).await?;

            let opts = tracedecay::bench::BenchOptions {
                format: if json {
                    tracedecay::bench::OutputFormat::Json
                } else {
                    tracedecay::bench::OutputFormat::Markdown
                },
                max_nodes,
            };

            let report = match queries {
                Some(p) => {
                    tracedecay::bench::run_bench(&cg, std::path::Path::new(&p), opts).await?
                }
                None => {
                    tracedecay::bench::run_bench_with_toml(
                        &cg,
                        tracedecay::bench::DEFAULT_QUERIES_TOML,
                        opts,
                    )
                    .await?
                }
            };

            if json {
                println!("{}", tracedecay::bench::format_report_json(&report));
            } else {
                print!("{}", tracedecay::bench::format_report_console(&report));
            }
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
        Commands::Branch { action } => {
            commands::handle_branch_action(action).await?;
        }
        Commands::Memory { action } => {
            commands::handle_memory_action(action).await?;
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
            | Commands::Uninstall { .. }
            | Commands::Doctor { .. }
            | Commands::HookPreToolUse
            | Commands::HookPromptSubmit
            | Commands::HookStop
            | Commands::HookKiroPreToolUse
            | Commands::HookKiroPromptSubmit
            | Commands::HookKiroPostToolUse
            | Commands::HookCursorSubagentStart
            | Commands::HookCursorPostToolUse
            | Commands::HookCursorBeforeSubmitPrompt
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
    //   - `UpdatePlugin`: guarantees that agent config files are not written;
    //     an implicit silent reinstall beforehand would rewrite configs and
    //     break that contract.
    //   - `Uninstall`: about to remove agent configs — don't reinstall them
    //     first (per the original #84 intent).
    //   - `Doctor`: a read-only diagnostic — must not mutate agent configs as
    //     a side effect (per the original #84 intent).
    //   - `Tool`: per-invocation tool calls are a hot-ish path; skip the
    //     reinstall scan there too.
    // Every other command (the normal everyday invocations) runs maintenance.
    matches!(
        command,
        Commands::Serve { .. }
            | Commands::Install { .. }
            | Commands::Reinstall
            | Commands::UpdatePlugin
            | Commands::Uninstall { .. }
            | Commands::Doctor { .. }
            | Commands::Tool { .. }
    )
}

fn is_local_install_command(command: &Commands) -> bool {
    matches!(command, Commands::Install { local: true, .. })
}

#[cfg(test)]
mod startup_tests {
    use super::{should_skip_agent_install_maintenance, should_skip_startup_maintenance, Commands};

    #[test]
    fn doctor_skips_startup_maintenance() {
        let command = Commands::Doctor {
            agent: Some("kiro".to_string()),
        };
        assert!(should_skip_startup_maintenance(&command));
    }

    #[test]
    fn explicit_agent_config_commands_skip_startup_maintenance() {
        assert!(should_skip_startup_maintenance(&Commands::Install {
            agent: Some("kiro".to_string()),
            local: false,
            profile: None,
            all_profiles: false,
            project_root: None,
            no_dashboard: false,
        }));
        assert!(should_skip_startup_maintenance(&Commands::Reinstall));
        assert!(should_skip_startup_maintenance(&Commands::UpdatePlugin));
        assert!(should_skip_startup_maintenance(&Commands::Uninstall {
            agent: Some("kiro".to_string()),
            profile: None,
            all_profiles: false,
        }));
    }

    #[test]
    fn normal_commands_keep_startup_maintenance() {
        assert!(!should_skip_startup_maintenance(&Commands::Status {
            path: None,
            json: false,
            short: false,
            details: false,
            runtime: false,
        }));
    }

    #[test]
    fn agent_install_maintenance_is_selective() {
        // Skip the implicit reinstall scan on the hot path (`serve`), on the
        // explicit install commands (they already install), and on per-call
        // tool invocations.
        assert!(should_skip_agent_install_maintenance(&Commands::Serve {
            path: None,
            timings: false,
        }));
        assert!(should_skip_agent_install_maintenance(&Commands::Install {
            agent: Some("cursor".to_string()),
            local: false,
            profile: None,
            all_profiles: false,
            project_root: None,
            no_dashboard: false,
        }));
        assert!(should_skip_agent_install_maintenance(&Commands::Reinstall));
        // `update-plugin` promises byte-identical configs; the implicit
        // silent-reinstall prelude would rewrite them.
        assert!(should_skip_agent_install_maintenance(
            &Commands::UpdatePlugin
        ));
        assert!(should_skip_agent_install_maintenance(&Commands::Tool {
            project: None,
            name: Some("message_search".to_string()),
            args: Vec::new(),
        }));

        // Also skip for uninstall (about to remove configs) and doctor (a
        // read-only diagnostic) — restoring the original #84 intent.
        assert!(should_skip_agent_install_maintenance(
            &Commands::Uninstall {
                agent: Some("cursor".to_string()),
                profile: None,
                all_profiles: false,
            }
        ));
        assert!(should_skip_agent_install_maintenance(&Commands::Doctor {
            agent: Some("cursor".to_string()),
        }));

        // Run maintenance for normal everyday command invocations so a binary
        // upgrade re-syncs agent config.
        assert!(!should_skip_agent_install_maintenance(&Commands::Init {
            path: None,
            skip_folders: Vec::new(),
        }));
        assert!(!should_skip_agent_install_maintenance(&Commands::Status {
            path: None,
            json: false,
            short: false,
            details: false,
            runtime: false,
        }));
    }

    #[test]
    fn serve_skips_startup_maintenance() {
        // `tracedecay serve` is the MCP hot path with a 30 s client-side
        // `initialize` timeout (#84). Pre-serve maintenance work
        // (worldwide-counter flush, install-stale check, silent reinstall)
        // must NOT run on this path.
        assert!(should_skip_startup_maintenance(&Commands::Serve {
            path: None,
            timings: false,
        }));
    }

    #[test]
    fn claude_and_kiro_hooks_skip_startup_maintenance() {
        // Claude and Kiro lifecycle hooks are agent-invoked hot-path
        // commands, exactly like the Cursor/Codex hooks already in the
        // skip-list. They must skip the synchronous `try_flush` network
        // round-trip (and the rest of the pre-command startup maintenance)
        // so they stay fast on every tool-use/prompt/stop event (#84).
        assert!(should_skip_startup_maintenance(&Commands::HookPreToolUse));
        assert!(should_skip_startup_maintenance(&Commands::HookPromptSubmit));
        assert!(should_skip_startup_maintenance(&Commands::HookStop));
        assert!(should_skip_startup_maintenance(
            &Commands::HookKiroPreToolUse
        ));
        assert!(should_skip_startup_maintenance(
            &Commands::HookKiroPromptSubmit
        ));
        assert!(should_skip_startup_maintenance(
            &Commands::HookKiroPostToolUse
        ));
    }
}

// handle_branch_action, handle_wipe, handle_list, handle_no_command,
// init_and_index, and print_sync_doctor have been moved to src/commands.rs.
//
// update_global_db, try_flush, check_for_update, gather_target_projects,
// gather_local_projects, gather_local_projects_from, find_descendant_tracedecay,
// print_flash_warning, and tracedecay_dir_size have been moved to src/global.rs.
// direct test 1774739850
