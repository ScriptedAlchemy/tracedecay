#![allow(clippy::too_many_arguments, clippy::collapsible_if)]
// binary crate: match lib allow policy for CLI dispatch
use clap::{ArgMatches, CommandFactory, FromArgMatches};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

// Opt-in allocator features (see Cargo.toml). Exactly one global allocator
// may exist per binary, so overlapping selections resolve by fixed precedence
// rather than a compile error: jemalloc wins, then mimalloc. `production` selects
// mimalloc (see Cargo.toml for the measurements); only a build that opts out
// of it keeps the system allocator, and the installed service unit must not
// cap glibc's arenas either way: `MALLOC_ARENA_MAX=2` once did, to bound
// retained memory, and put 60% of a 20-worker daemon's CPU into two arena
// locks while RSS still reached 15 GB.
#[cfg(feature = "alloc-jemalloc")]
#[global_allocator]
static JEMALLOC_ALLOCATOR: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[cfg(all(feature = "alloc-mimalloc", not(feature = "alloc-jemalloc")))]
#[global_allocator]
static MIMALLOC_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod agent_cmd;
mod analytics_cmd;
mod application_cli;
mod automation_cli;
mod cli;
mod cloud;
mod commands;
mod cost_cmd;
mod cost_summary;
mod display;
mod git_cmd;
mod global;
mod hook_capture_cmd;
mod hook_cmd;
mod lsp_cmd;
mod macos_codesign;
mod monitor_cmd;
mod process_allocator;
mod product_runtime;
mod project_cmd;
mod remote_command;
mod serve_cmd;
mod sessions_cmd;
mod status_cmd;
mod tool_command;
mod update_cmd;
mod upgrade;
mod work_cli;
mod work_command;
mod workflow_cli;
mod workflow_command;

use cli::*;
use tracedecay_contracts::retrieval::{
    AdminCliRegistryContextV1, AdminCliResultV1, AdminCliSurfaceRequestV1,
};
use tracedecay_contracts::retrieval::{AdminProjectResultV1, AdminProjectSurfaceRequestV1};
use tracedecay_daemon_service::logging::StderrTracingDefault;
use tracedecay_domain::process_heap::collect_idle_thread_heap_v1;
use tracedecay_runtime_core::config::{ProfileRoot, admit_process_host_program_search_path};

pub(crate) fn current_unix_timestamp() -> i64 {
    tracedecay_runtime_core::tracedecay::current_timestamp()
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
                    let display = spinner_tail(&text, SPINNER_MESSAGE_MAX_CHARS);
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
            let _ = write!(stderr, "\x1b[?25h");
            let _ = writeln!(stderr, "\r\x1b[2K\x1b[32m✔\x1b[0m {}", message);
        } else {
            let _ = writeln!(stderr, "{message}");
        }
        let _ = stderr.flush();
    }

    fn stop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.take()
            && h.join().is_err()
        {
            let mut stderr = std::io::stderr();
            let _ = writeln!(stderr, "\r\x1b[2Kprogress renderer thread panicked");
            let _ = stderr.flush();
        }
    }
}

/// Character bound for one rendered spinner message, so a long path or
/// progress line does not wrap on a typical terminal. Counted in Unicode
/// scalar values, not display columns: wide CJK glyphs still take two
/// columns each.
const SPINNER_MESSAGE_MAX_CHARS: usize = 50;

/// The last `max_chars` characters of `text`, with a leading `…` standing in
/// for the dropped prefix when the message is longer than that. Always slices
/// on a character boundary, so multibyte messages cannot panic the renderer.
fn spinner_tail(text: &str, max_chars: usize) -> std::borrow::Cow<'_, str> {
    let excess = text.chars().count().saturating_sub(max_chars);
    if excess == 0 {
        return text.into();
    }
    // Drop one extra character so the ellipsis fits inside the bound.
    let start = text
        .char_indices()
        .nth(excess + 1)
        .map_or(text.len(), |(index, _)| index);
    format!("…{}", &text[start..]).into()
}

#[cfg(test)]
mod spinner_tail_tests {
    use super::{SPINNER_MESSAGE_MAX_CHARS, spinner_tail};

    #[test]
    fn ascii_within_the_bound_is_unchanged() {
        let text = "a".repeat(SPINNER_MESSAGE_MAX_CHARS);
        assert_eq!(spinner_tail(&text, SPINNER_MESSAGE_MAX_CHARS), text);
    }

    /// Matches the previous byte-based output for ASCII: an ellipsis plus the
    /// last 49 characters.
    #[test]
    fn ascii_over_the_bound_keeps_the_tail_behind_an_ellipsis() {
        let text = format!("{}{}", "x".repeat(20), "y".repeat(40));
        let tail = spinner_tail(&text, SPINNER_MESSAGE_MAX_CHARS);
        assert_eq!(tail, format!("…{}{}", "x".repeat(9), "y".repeat(40)));
        assert_eq!(tail.chars().count(), SPINNER_MESSAGE_MAX_CHARS);
    }

    #[test]
    fn multibyte_latin_cjk_and_boundary_spanning_text_never_split_a_character() {
        for text in [
            "é".repeat(60),
            "字".repeat(60),
            format!("{}{}", "a".repeat(49), "日本語"),
            format!("{}🦀{}", "p".repeat(48), "q".repeat(10)),
            "/tmp/répertoire/très/long/chemin/vers/le/projet/源/lib.rs".repeat(2),
        ] {
            let tail = spinner_tail(&text, SPINNER_MESSAGE_MAX_CHARS);
            assert_eq!(tail.chars().count(), SPINNER_MESSAGE_MAX_CHARS, "{text}");
            let kept = tail.strip_prefix('…').unwrap_or_else(|| panic!("{text}"));
            assert!(text.ends_with(kept), "{text}");
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
/// builds, `tracedecay serve` and `tracedecay tool` died with
/// STATUS_STACK_OVERFLOW on Windows CI. Running the runtime on a thread with
/// an explicit stack size gives every platform the same headroom.
const ASYNC_STACK_BYTES: usize = 16 * 1024 * 1024;
const MAX_ASYNC_WORKER_THREADS: usize = 16;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AsyncRuntimeFlavor {
    CurrentThread,
    MultiThread,
}

const MIN_SERVING_BLOCKING_RESERVE: usize = 4;
const DEFAULT_MAX_DAEMON_CPU_THREADS: usize = 16;
const DAEMON_CPU_THREADS_ENV: &str = "TRACEDECAY_DAEMON_CPU_THREADS";
const RAYON_NUM_THREADS_ENV: &str = "RAYON_NUM_THREADS";

fn async_worker_threads() -> usize {
    std::thread::available_parallelism()
        .map_or(1, usize::from)
        .clamp(1, MAX_ASYNC_WORKER_THREADS)
}

fn async_runtime_flavor(command: Option<&Commands>) -> AsyncRuntimeFlavor {
    match command {
        // One-shot daemon clients drive a single socket request and exit. A
        // multi-thread runtime eagerly starts up to 16 workers they never
        // use; the current thread already has a fixed 16 MiB stack and
        // Tokio's blocking pool remains available when needed.
        Some(Commands::Tool { .. } | Commands::Status { .. }) => AsyncRuntimeFlavor::CurrentThread,
        _ => AsyncRuntimeFlavor::MultiThread,
    }
}

/// `tool` and `status` talk to an already-running daemon and then exit. They
/// do not ingest transcripts, open a local graph, or serve the dashboard, so
/// the inverted runtime ports, cloud probes, and C-library allocator route
/// are work they never read.
fn is_one_shot_daemon_client(command: Option<&Commands>) -> bool {
    matches!(
        command,
        Some(Commands::Tool { .. } | Commands::Status { .. })
    )
}

/// Print the clap version line without constructing the command tree or
/// installing process-global runtime slots. Only the lone `--version` / `-V`
/// form is handled here; mixed argv still goes through clap.
fn try_print_cli_version(args: &[std::ffi::OsString]) -> Option<ExitCode> {
    let mut saw_version = false;
    for arg in args.iter().skip(1) {
        let arg = arg.to_str()?;
        if arg == "--version" || arg == "-V" {
            saw_version = true;
            continue;
        }
        return None;
    }
    if !saw_version {
        return None;
    }
    println!(
        "tracedecay {}",
        crate::product_runtime::PRODUCT_BUILD_VERSION
    );
    Some(ExitCode::SUCCESS)
}

/// Keep enough bounded blocking workers to run every admitted background CPU
/// unit plus serving work that does not consume that CPU budget. Before the
/// profile-scoped worker plan is installed, using the host width is the safe
/// upper bound for any later exact plan. The result is host-bounded: it is at
/// most `available + MIN_SERVING_BLOCKING_RESERVE`.
fn tokio_blocking_thread_limit() -> usize {
    let available = std::thread::available_parallelism().map_or(1, usize::from);
    let effective = tracedecay_code_index::parallelism::installed_worker_status()
        .map(|status| usize::from(status.effective_workers))
        .unwrap_or(available);
    tokio_blocking_thread_limit_from(available, effective)
}

fn tokio_blocking_thread_limit_from(available: usize, effective_workers: usize) -> usize {
    let available = available.max(1);
    let effective_workers = effective_workers.clamp(1, available);
    let serving_reserve = available
        .saturating_sub(effective_workers)
        .max(MIN_SERVING_BLOCKING_RESERVE);
    effective_workers.saturating_add(serving_reserve)
}

#[cfg(test)]
mod blocking_thread_limit_tests {
    use super::*;

    #[test]
    fn blocking_limit_covers_effective_width_and_serving_reserve() {
        assert_eq!(tokio_blocking_thread_limit_from(96, 48), 96);
        assert_eq!(tokio_blocking_thread_limit_from(96, 96), 100);
        assert_eq!(tokio_blocking_thread_limit_from(8, 8), 12);
    }

    #[test]
    fn blocking_limit_is_bounded_by_host_width_plus_reserve() {
        for available in 1..=256 {
            for effective in 1..=available {
                let limit = tokio_blocking_thread_limit_from(available, effective);
                assert!(limit >= effective + MIN_SERVING_BLOCKING_RESERVE);
                assert!(limit <= available + MIN_SERVING_BLOCKING_RESERVE);
            }
        }
    }
}

fn daemon_cpu_threads_from(
    available: usize,
    configured: Option<(&str, &str)>,
) -> Result<usize, String> {
    match configured {
        Some((source, raw)) => match raw.parse::<usize>().ok().filter(|threads| *threads > 0) {
            Some(threads) => Ok(threads),
            None if source == RAYON_NUM_THREADS_ENV => {
                Ok(available.clamp(1, DEFAULT_MAX_DAEMON_CPU_THREADS))
            }
            None => Err(format!("{source} must be a positive integer, got {raw:?}")),
        },
        None => Ok(available.clamp(1, DEFAULT_MAX_DAEMON_CPU_THREADS)),
    }
}

fn is_daemon_run(command: Option<&Commands>) -> bool {
    matches!(
        command,
        Some(Commands::Daemon {
            action: DaemonAction::Run { .. }
        })
    )
}

fn install_daemon_cpu_pool(command: Option<&Commands>) -> tracedecay_domain::errors::Result<()> {
    if !is_daemon_run(command) {
        return Ok(());
    }
    let available = std::thread::available_parallelism().map_or(1, usize::from);
    let configured = std::env::var(DAEMON_CPU_THREADS_ENV)
        .ok()
        .map(|value| (DAEMON_CPU_THREADS_ENV, value))
        .or_else(|| {
            std::env::var(RAYON_NUM_THREADS_ENV)
                .ok()
                .map(|value| (RAYON_NUM_THREADS_ENV, value))
        });
    let threads = daemon_cpu_threads_from(
        available,
        configured
            .as_ref()
            .map(|(source, value)| (*source, value.as_str())),
    )
    .map_err(|message| tracedecay_domain::errors::TraceDecayError::Config { message })?;
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|index| format!("tracedecay-cpu-{index}"))
        .build_global()
        .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("failed to start daemon CPU pool: {error}"),
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommandOutcome {
    Success,
    Exit(i32),
}

/// `EX_TEMPFAIL`: a `wait_for` status read ended before the index reached
/// the requested state; rerunning the wait may reach it.
const READINESS_WAIT_TIMED_OUT_EXIT_CODE: u8 = 75;

fn process_exit_code(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

#[cfg(unix)]
fn restore_sigpipe_default() -> std::io::Result<()> {
    // SAFETY: `async_main` calls this only after selecting the one-shot tool
    // client mode, before that mode starts worker threads or writes output.
    let previous = unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    if previous == libc::SIG_ERR {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn main() -> ExitCode {
    let args = std::env::args_os().collect::<Vec<_>>();
    if let Some(code) = try_print_cli_version(&args) {
        return code;
    }
    admit_process_host_program_search_path();

    if let Some(command) = args.get(1).and_then(|value| value.to_str()) {
        tracing::trace!(name: "cli.command.name", value = ?command);
    }
    if let Some(code) = {
        let _span = tracing::trace_span!("cli.hook.native_capture").entered();
        hook_capture_cmd::try_run(&args)
    } {
        return process_exit_code(code);
    }
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
            return ExitCode::FAILURE;
        }
    };
    match result {
        Ok(CommandOutcome::Success) => ExitCode::SUCCESS,
        Ok(CommandOutcome::Exit(code)) => process_exit_code(code),
        Err(e) => {
            let code = if tracedecay_daemon_identity::daemon_unreachable(&e) {
                ExitCode::from(tracedecay_daemon_identity::DAEMON_UNREACHABLE_EXIT_CODE)
            } else if matches!(
                &e,
                tracedecay_domain::errors::TraceDecayError::ToolRefused(refusal)
                    if refusal.code.as_deref() == Some(tracedecay_contracts::code_index_freshness::CODE_INDEX_READINESS_WAIT_TIMED_OUT)
            ) {
                ExitCode::from(READINESS_WAIT_TIMED_OUT_EXIT_CODE)
            } else if matches!(
                &e,
                tracedecay_domain::errors::TraceDecayError::ToolRefused(refusal)
                    if refusal.code.as_deref() == Some(tool_command::TEST_GATE_REFUSAL)
            ) {
                ExitCode::from(tool_command::TEST_GATE_EXIT_CODE)
            } else {
                ExitCode::FAILURE
            };
            // A typed reset refusal from any command ends with the refused
            // authority and the exact command that resets it; a typed route
            // detail prints as fields.
            eprintln!("Error: {}", commands::process_error_text(e));
            code
        }
    }
}

fn async_main() -> tracedecay_domain::errors::Result<CommandOutcome> {
    let args: Vec<String> = std::env::args().collect();

    if let Some(command) = args.get(1) {
        // Fallback identity for Clap help/version/parse failures. A successful
        // parse replaces it below with the exact canonical nested command path.
        tracing::trace!(name: "cli.command.name", value = ?command.as_str());
    }
    if render_dynamic_command_help(&args) {
        return Ok(CommandOutcome::Success);
    }
    let matches = match Cli::command().try_get_matches_from(args) {
        Ok(matches) => matches,
        Err(error) => {
            let code = error.exit_code();
            error.print()?;
            return Ok(CommandOutcome::Exit(code));
        }
    };

    let command_name = command_profile_label(&matches);
    let json_requested = json_flag_set(&matches);
    let mut cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(error) => {
            let code = error.exit_code();
            error.print()?;
            return Ok(CommandOutcome::Exit(code));
        }
    };
    normalize_tool_reserved_global_flags(&mut cli);
    #[cfg(unix)]
    if matches!(cli.command.as_ref(), Some(Commands::Tool { .. })) {
        restore_sigpipe_default().map_err(|error| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("failed to configure tool pipeline output: {error}"),
            }
        })?;
    }
    // Route tracing events (degradation causes, ingest warnings) to stderr.
    // without a subscriber every `tracing::warn!` in the runtime is silently
    // dropped, which hid the causes behind typed catch-up reason codes. The
    // daemon runs through this same entrypoint, so this is also the daemon's
    // subscriber; RUST_LOG raises verbosity (default `warn`).
    //
    // Installed after parsing rather than first thing: hook stderr belongs to
    // the host, so which command is running has to be known before anything
    // is allowed to write there.
    tracedecay_daemon_service::logging::install_stderr_tracing(stderr_tracing_default(
        cli.command.as_ref(),
    ));
    // Handshake advertises this binary's registered build version. Help and
    // clap version already returned above, so only a command that may talk
    // to the daemon pays the registration.
    tracedecay_project::product_runtime::register_product_runtime(
        crate::product_runtime::provider(),
    )?;
    if !is_one_shot_daemon_client(cli.command.as_ref()) {
        crate::cloud::admit_sync_probes();
        // Inverted runtime ports are read by ingest, installers, hooks, and
        // project open. One-shot daemon clients call the daemon directly and
        // never read those slots.
        tracedecay::register_runtime_ports()?;
        process_allocator::configure_process_allocator();
    }
    // Bound only Rayon's global pool for daemon workloads that actually use
    // it. Code indexing owns a separately planned pool shared by semantic
    // projection, so changing this ceiling cannot silently narrow that budget.
    {
        let _span = tracing::trace_span!("daemon_cpu_pool_install").entered();
        install_daemon_cpu_pool(cli.command.as_ref())
    }?;
    let runtime_flavor = async_runtime_flavor(cli.command.as_ref());
    let worker_threads = match runtime_flavor {
        AsyncRuntimeFlavor::CurrentThread => 1,
        AsyncRuntimeFlavor::MultiThread => async_worker_threads(),
    };
    let blocking_threads = tokio_blocking_thread_limit();
    let runtime = {
        let _span = tracing::trace_span!("tokio_runtime_build").entered();
        {
            let build = match runtime_flavor {
                AsyncRuntimeFlavor::CurrentThread => tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .max_blocking_threads(blocking_threads)
                    .thread_stack_size(ASYNC_STACK_BYTES)
                    .build(),
                AsyncRuntimeFlavor::MultiThread => tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .worker_threads(worker_threads)
                    .max_blocking_threads(blocking_threads)
                    .thread_stack_size(ASYNC_STACK_BYTES)
                    .on_thread_park(collect_idle_thread_heap_v1)
                    .build(),
            };
            build.map_err(|e| tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("failed to start async runtime: {e}"),
            })
        }
    }?;

    {
        let command_family = cli.command.as_ref().map_or("none", |command| {
            CommandFamily::for_command(command).as_profile_label()
        });
        tracing::trace!(name: "process_command_family", value = ?command_family);
        tracing::trace!(name: "cli.command.name", value = ?command_name.as_str());
    }
    let foreground_daemon = matches!(
        cli.command.as_ref(),
        Some(Commands::Daemon {
            action: DaemonAction::Run { .. }
        })
    );

    let result = {
        let _span = tracing::trace_span!("process_command").entered();
        runtime.block_on(tracing::Instrument::instrument(
            run(cli),
            tracing::trace_span!("process_command_future"),
        ))
    };

    // Runtime drop waits indefinitely for blocking tasks. Daemon integrations
    // can leave OS-backed watcher work behind after their async handles abort,
    // so bound teardown after the command's own graceful shutdown completes.
    //
    // The foreground daemon already coordinated every owner with typed
    // receipts; a blocking task still running here is one its shutdown owner
    // reported as pending and abandoned at the task-abort deadline. Waiting
    // for it a second time only spends the supervisor's TERM grace.
    if foreground_daemon {
        runtime.shutdown_background();
    } else {
        runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    }
    if json_requested
        && let Err(error) = &result
        && !matches!(
            error,
            tracedecay_domain::errors::TraceDecayError::ToolRefused(_)
        )
    {
        println!(
            "{}",
            tracedecay::mcp::tools::command_refusal_document(error)?
        );
    }
    result
}

/// Whether the parsed command asked for `--json` output. A `ToolRefused`
/// error is an owner refusal its command already rendered, so only the
/// remaining refusals print their problem document at the process boundary.
fn json_flag_set(matches: &ArgMatches) -> bool {
    matches
        .try_get_one::<bool>("json")
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false)
        || matches
            .subcommand()
            .is_some_and(|(_, matches)| json_flag_set(matches))
}

/// Hooks are silent on stderr unless `RUST_LOG` says otherwise: their host
/// owns that stream and reads unexpected output as a hook failure. Every other
/// command keeps the crate default of `warn`.
fn stderr_tracing_default(command: Option<&Commands>) -> StderrTracingDefault {
    match command {
        Some(command) if CommandFamily::for_command(command) == CommandFamily::Hook => {
            StderrTracingDefault::Silent
        }
        _ => StderrTracingDefault::Warn,
    }
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

/// Derive the exact static Clap command path from Clap's own parsed authority.
/// Dynamic `tool`, `work`, and `workflow` operation identities are recorded by
/// their dispatch adapters because they are arguments rather than subcommands.
fn command_profile_label(matches: &ArgMatches) -> String {
    let mut path = String::new();
    let mut cursor = matches;
    while let Some((name, nested)) = cursor.subcommand() {
        if !path.is_empty() {
            path.push('.');
        }
        path.push_str(name);
        cursor = nested;
    }
    if path.is_empty() {
        "none".to_owned()
    } else {
        path
    }
}

async fn run(cli: Cli) -> tracedecay_domain::errors::Result<CommandOutcome> {
    let host_bundle = HostBundleCliOptions {
        component: cli.component,
        dry_run: cli.dry_run,
        yes: cli.yes,
        adopt: cli.adopt,
    };
    let profile = command_profile(cli.command.as_ref())?;
    let command = match cli.command {
        Some(cmd) => cmd,
        None => {
            commands::handle_no_command(&profile).await?;
            return Ok(CommandOutcome::Success);
        }
    };

    run_startup_preamble(&profile, &command).await;
    dispatch_command(&profile, command, host_bundle).await
}

/// The one profile this process serves, resolved from its environment. The
/// foreground daemon launched with `--profile-root` (the Windows task shape)
/// serves that data directory instead of the environment's.
fn command_profile(command: Option<&Commands>) -> tracedecay_domain::errors::Result<ProfileRoot> {
    match command {
        Some(Commands::Daemon {
            action:
                DaemonAction::Run {
                    profile_root: Some(profile_root),
                    ..
                },
        }) => Ok(ProfileRoot::from_env_with_data_dir(profile_root)),
        _ => ProfileRoot::from_env(),
    }
}

#[tracing::instrument(name = "cli.startup.preamble", level = "trace", skip_all)]
async fn run_startup_preamble(profile: &ProfileRoot, command: &Commands) {
    let startup_policy = CommandStartupPolicy::for_command(command);

    // Check first-run before any config save creates the file.
    let profile_root = profile.data_dir();
    let is_first_run = !tracedecay_session_memory::user_config::UserConfig::exists(profile_root);

    let is_force_flush = matches!(command, Commands::Sync { .. } | Commands::Status { .. });
    match tracedecay_session_memory::user_config::UserConfig::load(profile_root) {
        Ok(user_config) => {
            flush_worldwide_counter(profile, command, is_force_flush, user_config).await;
        }
        Err(err) => eprintln!("warning: {err}"),
    }

    if is_first_run && startup_policy.runs_startup_maintenance() {
        eprintln!(
            "note: tracedecay can optionally upload anonymous token savings counts to a worldwide counter.\n\
             \x20     Run `tracedecay enable-upload-counter` to opt in."
        );
    }

    if startup_policy.runs_agent_install_check()
        && let Some(home) = profile.home()
    {
        tracedecay_agent_hosts::agents::claude::check_install_stale(home);
    }
}

async fn flush_worldwide_counter(
    profile: &ProfileRoot,
    command: &Commands,
    is_force_flush: bool,
    mut user_config: tracedecay_session_memory::user_config::UserConfig,
) {
    let profile_root = profile.data_dir();
    // Skip the worldwide-counter flush on hot startup paths. `try_flush`
    // makes a synchronous HTTP call which can add seconds to
    // `tracedecay serve` startup on slow networks, long enough to blow the
    // MCP client's 30 s `initialize` timeout. The canonical setting lookup is
    // only consulted when there are pending tokens to flush: with nothing
    // pending the setting cannot change behavior, and probing it on every
    // command turned the daemon's transient "runtime still mounting" state
    // into per-command stderr noise. A failed lookup on an ordinary command
    // is deferred (the next command retries); the flush-bearing commands
    // (`sync`, `status`) still surface it, so a persistent failure
    // stays visible exactly where the flush is expected to happen.
    // The setting belongs to the profile, so the flush does not depend on
    // the current directory.
    if runs_worldwide_counter_flush(command) && user_config.pending_upload > 0 {
        match commands::canonical_upload_enabled(profile).await {
            Ok(upload_enabled) => {
                global::try_flush(&mut user_config, is_force_flush, upload_enabled);
            }
            Err(error) if is_force_flush => {
                eprintln!(
                    "warning: canonical worldwide-counter upload setting is unavailable: {}",
                    commands::annotate_reset_required(error, None)
                );
            }
            Err(error) => {
                tracing::debug!(
                    "worldwide-counter flush deferred: canonical upload setting unavailable: \
                     {error}"
                );
            }
        }
    }
    if !is_local_install_command(command)
        && let Err(err) = user_config.save_if_exists(profile_root)
    {
        eprintln!("warning: could not save tracedecay config: {err}");
    }
}

async fn resolve_registered_project_root(
    profile: &ProfileRoot,
    project_id: Option<String>,
    project_path: Option<String>,
) -> tracedecay_domain::errors::Result<Option<PathBuf>> {
    let selector = match (project_id, project_path) {
        (Some(project_id), _) => project_id,
        (None, Some(project_path)) => registered_project_path_selector(&project_path)?,
        (None, None) => return Ok(None),
    };
    let request = AdminCliSurfaceRequestV1::RegistryContext {
        project_arg: Some(PathBuf::from(&selector)),
    };
    match commands::admin_cli_result(profile, None, request).await? {
        AdminCliResultV1::RegistryContext(AdminCliRegistryContextV1::Ok { project, .. }) => {
            Ok(Some(PathBuf::from(project.display_root)))
        }
        AdminCliResultV1::RegistryContext(_) => {
            Err(tracedecay_domain::errors::TraceDecayError::project_route(
                "project_route_not_found",
                false,
                format!("registered project not found for '{selector}'"),
            ))
        }
        _ => Err(commands::admin_cli_result_mismatch("registry_context")),
    }
}

/// A `--project-path` selector as the registry must see it. The daemon
/// resolves paths from its own directory, so a path-shaped selector is
/// canonicalized against the CLI's working directory here; any other selector
/// is a registered alias and passes through unchanged.
pub(crate) fn registered_project_path_selector(
    selector: &str,
) -> tracedecay_domain::errors::Result<String> {
    if !tracedecay_global_db::RegisteredGlobalDb::is_explicit_project_path_selector(selector) {
        return Ok(selector.to_owned());
    }
    let config_error = |message| tracedecay_domain::errors::TraceDecayError::Config { message };
    tracedecay_runtime_core::path_safety::canonical_existing_identity(Path::new(selector.trim()))
        .map_err(|error| config_error(format!("--project-path '{selector}': {error}")))?
        .into_os_string()
        .into_string()
        .map_err(|path| {
            config_error(format!(
                "--project-path '{selector}' resolves to a non-UTF-8 path '{}'",
                path.to_string_lossy()
            ))
        })
}

pub(crate) async fn resolve_cli_project_root(
    profile: &ProfileRoot,
    path: Option<String>,
    project_id: Option<String>,
    project_path: Option<String>,
) -> tracedecay_domain::errors::Result<PathBuf> {
    if let Some(root) = resolve_registered_project_root(profile, project_id, project_path).await? {
        return Ok(root);
    }
    Ok(tracedecay_configuration::resolve_path_with_discovery(
        profile, path,
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandFamily {
    Project,
    Runtime,
    Agent,
    Hook,
    Update,
    Configuration,
    Diagnostics,
    Knowledge,
}

impl CommandFamily {
    fn as_profile_label(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Runtime => "runtime",
            Self::Agent => "agent",
            Self::Hook => "hook",
            Self::Update => "update",
            Self::Configuration => "configuration",
            Self::Diagnostics => "diagnostics",
            Self::Knowledge => "knowledge",
        }
    }

    fn for_command(command: &Commands) -> Self {
        match command {
            Commands::Init { .. }
            | Commands::Sync { .. }
            | Commands::Status { .. }
            | Commands::Projects { .. }
            | Commands::Branch { .. }
            | Commands::Memory { .. }
            | Commands::Storage { .. }
            | Commands::Wipe { .. }
            | Commands::List { .. } => Self::Project,
            Commands::Tool { .. }
            | Commands::Work { .. }
            | Commands::Workflow { .. }
            | Commands::Lsp { .. }
            | Commands::Remote { .. }
            | Commands::Dashboard { .. }
            | Commands::Serve { .. }
            | Commands::Daemon { .. } => Self::Runtime,
            Commands::Install { .. }
            | Commands::Reinstall { .. }
            | Commands::UpdatePlugin { .. }
            | Commands::Uninstall { .. }
            | Commands::FeedbackRollback { .. } => Self::Agent,
            Commands::HookPreToolUse
            | Commands::HookPromptSubmit
            | Commands::HookStop
            | Commands::HookClaudeSessionStart
            | Commands::HookClaudePostToolUse
            | Commands::HookClaudePostCompact
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
            | Commands::HookKimiEvent
            | Commands::HookOpenCodeEvent
            | Commands::HookOpenCodeToolAfter
            | Commands::HookPiEvent
            | Commands::HookDroidEvent => Self::Hook,
            Commands::Upgrade { .. }
            | Commands::Update { .. }
            | Commands::PostUpdate { .. }
            | Commands::PackageHook { .. }
            | Commands::Channel { .. } => Self::Update,
            Commands::CurrentCounter { .. }
            | Commands::ResetCounter { .. }
            | Commands::DisableUploadCounter
            | Commands::EnableUploadCounter => Self::Configuration,
            Commands::Doctor { .. }
            | Commands::Cost { .. }
            | Commands::Gain { .. }
            | Commands::Monitor => Self::Diagnostics,
            Commands::Git { .. }
            | Commands::Sessions { .. }
            | Commands::Analytics { .. }
            | Commands::Automation { .. } => Self::Knowledge,
        }
    }
}

fn validate_host_bundle_options(
    command: &Commands,
    family: CommandFamily,
    host_bundle: &HostBundleCliOptions,
) -> tracedecay_domain::errors::Result<()> {
    // `wipe` is the one non-lifecycle command that destroys deployed state, so
    // it takes the same `--yes` confirmation as the lifecycle mutations instead
    // of an interactive-only `go!` prompt. It owns no host component and has no
    // preview, so `--component` and `--dry-run` stay rejected.
    if matches!(command, Commands::Wipe { .. }) {
        if host_bundle.component.is_some() || host_bundle.dry_run || host_bundle.adopt {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "wipe accepts --yes to confirm; --component, --dry-run, and --adopt are only valid \
                          with install, update-plugin, reinstall, or uninstall"
                    .to_string(),
            });
        }
        return Ok(());
    }
    // `projects forget` destroys one registered project's rows and stores, so
    // it REQUIRES `--yes` (its handler refuses to run without it) and takes
    // the global `--dry-run` as its preview. It owns no host component.
    if matches!(
        command,
        Commands::Projects {
            action: ProjectsAction::Forget { .. },
        }
    ) {
        if host_bundle.component.is_some() || host_bundle.adopt {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "projects forget accepts --yes to confirm and --dry-run to preview; \
                          --component and --adopt are only valid with install, update-plugin, \
                          reinstall, or uninstall"
                    .to_string(),
            });
        }
        return Ok(());
    }
    // `sessions git-sync` takes the global `--dry-run` as its preview; it
    // needs no confirmation and owns no host component.
    if matches!(
        command,
        Commands::Sessions {
            action: SessionsAction::GitSync { .. },
        }
    ) {
        if host_bundle.component.is_some() || host_bundle.yes || host_bundle.adopt {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "sessions git-sync accepts --dry-run to preview; --component, --yes, and \
                          --adopt are only valid with install, update-plugin, reinstall, or uninstall"
                    .to_string(),
            });
        }
        return Ok(());
    }
    // The scoped storage reset destroys refused store state, so it REQUIRES
    // the same `--yes` confirmation (its handler refuses to run without it).
    // Like `wipe`, it owns no host component and has no preview.
    if matches!(
        command,
        Commands::Storage {
            action: ProfileStorageAction::ResetProjectStore { .. },
        }
    ) {
        if host_bundle.component.is_some() || host_bundle.dry_run || host_bundle.adopt {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "storage resets accept --yes to confirm; --component, --dry-run, and --adopt are \
                          only valid with install, update-plugin, reinstall, or uninstall"
                    .to_string(),
            });
        }
        return Ok(());
    }
    // `--component`, `--dry-run`, and `--yes` are declared as global flags so
    // clap accepts them before the subcommand is known, but they are only
    // meaningful for the agent-lifecycle commands. Enforcing that scope here
    // (rather than via a global clap `requires = "component"`) keeps the flags
    // from leaking a spurious `--component` requirement onto unrelated verbs
    // such as `branch gc` and `storage report`.
    if !matches!(family, CommandFamily::Agent)
        && (host_bundle.component.is_some()
            || host_bundle.dry_run
            || host_bundle.yes
            || host_bundle.adopt)
    {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message:
                "--component, --dry-run, --yes, and --adopt are only valid with install, update-plugin, reinstall, or uninstall"
                    .to_string(),
        });
    }
    if host_bundle.adopt && !host_bundle.yes {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message:
                "--adopt requires --yes because it authorizes taking ownership of existing bytes"
                    .to_string(),
        });
    }
    if host_bundle.adopt
        && !matches!(
            command,
            Commands::Install { .. } | Commands::UpdatePlugin { .. } | Commands::Reinstall { .. }
        )
    {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: "--adopt is valid only with install, update-plugin, or reinstall".to_string(),
        });
    }
    Ok(())
}

async fn dispatch_command(
    profile: &ProfileRoot,
    command: Commands,
    host_bundle: HostBundleCliOptions,
) -> tracedecay_domain::errors::Result<CommandOutcome> {
    let family = CommandFamily::for_command(&command);
    validate_host_bundle_options(&command, family, &host_bundle)?;
    match family {
        CommandFamily::Project => {
            dispatch_project_command(profile, command, host_bundle.yes, host_bundle.dry_run)
                .await?;
            Ok(CommandOutcome::Success)
        }
        CommandFamily::Runtime => dispatch_runtime_command(profile, command).await,
        CommandFamily::Agent => dispatch_agent_command(profile, command, host_bundle)
            .await
            .map(lifecycle_command_outcome),
        CommandFamily::Hook => dispatch_hook_command(profile, command).await,
        CommandFamily::Update => dispatch_update_command(profile, command)
            .await
            .map(lifecycle_command_outcome),
        CommandFamily::Configuration => {
            dispatch_configuration_command(profile, command).await?;
            Ok(CommandOutcome::Success)
        }
        CommandFamily::Diagnostics => dispatch_diagnostics_command(profile, command).await,
        CommandFamily::Knowledge => {
            dispatch_knowledge_command(profile, command, host_bundle.dry_run).await?;
            Ok(CommandOutcome::Success)
        }
    }
}

async fn dispatch_project_command(
    profile: &ProfileRoot,
    command: Commands,
    assume_yes: bool,
    dry_run: bool,
) -> tracedecay_domain::errors::Result<()> {
    match command {
        Commands::Init {
            path,
            path_flag,
            adopt_project,
            fresh,
            wait,
        } => {
            // clap enforces that at most one of these is present.
            commands::handle_init(
                profile,
                path.or(path_flag),
                adopt_project,
                fresh,
                assume_yes,
                wait,
            )
            .await?;
        }
        Commands::Sync { path, verbose } => {
            commands::handle_sync(profile, path, verbose).await?;
        }
        Commands::Status {
            path,
            project_id,
            project_path,
            json,
            short,
            runtime,
        } => {
            status_cmd::handle_status_command(
                profile,
                path,
                project_id,
                project_path,
                json,
                short,
                runtime,
            )
            .await?;
        }
        Commands::Projects { action } => {
            project_cmd::handle_projects_action(profile, action, assume_yes, dry_run).await?;
        }
        Commands::Branch { action } => {
            commands::handle_branch_action(profile, action).await?;
        }
        Commands::Memory { action } => {
            dispatch_memory_command(profile, action).await?;
        }
        Commands::Storage { action } => {
            commands::handle_profile_storage_action(profile, action, assume_yes).await?;
        }
        Commands::Wipe { stale: true, .. } => {
            commands::handle_wipe_stale(profile, assume_yes).await?;
        }
        Commands::Wipe { all, stale: false } => {
            commands::handle_wipe(profile, all, assume_yes).await?;
        }
        Commands::List { all } => {
            commands::handle_list(profile, all).await?;
        }
        _ => unreachable!("non-project command passed to project dispatcher"),
    }
    Ok(())
}

#[tracing::instrument(name = "cli.memory.status", level = "trace", skip_all)]
async fn dispatch_memory_command(
    profile: &ProfileRoot,
    action: MemoryAction,
) -> tracedecay_domain::errors::Result<()> {
    match action {
        MemoryAction::Status {
            json,
            path,
            project_id,
            project_path,
        } => {
            let project_path =
                resolve_cli_project_root(profile, path, project_id, project_path).await?;
            let result = commands::daemon_tool_json(
                profile,
                Some(&project_path),
                "tracedecay_memory_status",
                serde_json::json!({ "format": "json" }),
            )
            .await?;
            let status: tracedecay_contracts::retained_surfaces::MemoryStatusResultV1 =
                commands::retained_tool_payload("tracedecay_memory_status", result)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                print!(
                    "{}",
                    status_cmd::format_memory_status_report(&status.memory)
                );
            }
        }
    }
    Ok(())
}

fn open_dashboard_url(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let status = Command::new("open").arg(url).status()?;
    #[cfg(target_os = "windows")]
    let status = Command::new("cmd")
        .args(["/C", "start", "", url])
        .status()?;
    #[cfg(all(unix, not(target_os = "macos")))]
    let status = Command::new("xdg-open").arg(url).status()?;
    #[cfg(not(any(unix, target_os = "windows")))]
    return Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "no platform opener",
    ));
    #[cfg(any(unix, target_os = "windows"))]
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("opener exited {status}")))
    }
}

async fn dispatch_runtime_command(
    profile: &ProfileRoot,
    command: Commands,
) -> tracedecay_domain::errors::Result<CommandOutcome> {
    match command {
        Commands::Tool {
            project,
            name,
            args,
        } => {
            tool_command::run(profile, project, name, args).await?;
        }
        Commands::Work { invocation } => work_command::run(profile, invocation).await?,
        Commands::Workflow { invocation } => workflow_command::run(profile, invocation).await?,
        Commands::Remote { action } => {
            let profile = profile.clone();
            let command = action.into();
            tracing::Instrument::instrument(
                async {
                    tokio::task::spawn_blocking(move || {
                        crate::remote_command::run(&profile, command)
                    })
                    .await
                    .map_err(|error| {
                        tracedecay_domain::errors::TraceDecayError::Config {
                            message: format!("remote command task failed to join: {error}"),
                        }
                    })?
                },
                tracing::trace_span!("cli.remote.run"),
            )
            .await?;
        }
        Commands::Lsp { action } => {
            lsp_cmd::handle_lsp_action(profile, action).await?;
        }
        Commands::Dashboard {
            path,
            host,
            port,
            open,
        } => {
            let project_path = tracedecay_configuration::resolve_path_with_discovery(profile, path);
            if profile.is_ambient_project_root(&project_path) {
                return Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: tracedecay_runtime_core::config::ambient_project_root_guidance(
                        &project_path,
                    ),
                });
            }
            let result = tracing::Instrument::instrument(
                commands::daemon_tool_json(
                    profile,
                    Some(&project_path),
                    "tracedecay_dashboard",
                    serde_json::json!({
                        "action": "start",
                        "host": host,
                        "port": port,
                        "format": "json",
                    }),
                ),
                tracing::trace_span!("cli.dashboard.start"),
            )
            .await?;
            let url = result
                .get("url")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
                    message: "daemon dashboard response omitted URL".to_string(),
                })?;
            // The daemon keys hosted dashboards by canonicalized project
            // root, so any response reached here always serves this
            // project; only the requested host/port may differ from what is
            // actually bound (an idle dashboard for this same project was
            // already listening before this request was sent).
            let status = result
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("started");
            match status {
                "already_running" | "stopping" => {
                    println!("tracedecay dashboard already listening on {url}");
                    eprintln!(
                        "Dashboard bound to launch project {}",
                        project_path.display()
                    );
                    eprintln!(
                        "Code search covers this project only; rebound with --path to serve another."
                    );
                    let port_honored = result
                        .get("requested_port_honored")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(true);
                    if !port_honored {
                        let requested_port = result
                            .get("requested_port")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(u64::from(port));
                        let bound_port = result
                            .get("port")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or_default();
                        eprintln!(
                            "Note: --port {requested_port} was not honored; a dashboard for this project was already running on port {bound_port}."
                        );
                    }
                    if status == "stopping" {
                        eprintln!(
                            "Note: the existing dashboard is shutting down; this URL may stop responding shortly."
                        );
                    }
                }
                _ => {
                    println!("tracedecay dashboard listening on {url}");
                    eprintln!(
                        "Dashboard bound to launch project {}",
                        project_path.display()
                    );
                    eprintln!(
                        "Code search covers this project only; rebound with --path to serve another."
                    );
                }
            }
            if open {
                match open_dashboard_url(url) {
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
                return Ok(CommandOutcome::Success);
            }
            // The MCP server is long-lived, so it may run the detached
            // structured-row backfill sweep; one-shot CLI/hook processes never
            // do (they would drop the sweep mid-parse on exit).
            tracedecay_store_runtime::mark_process_long_lived_for_session_maintenance();
            tracing::Instrument::instrument(
                serve_cmd::run_serve(profile, path, timings),
                tracing::trace_span!("cli.serve.run"),
            )
            .await?;
        }
        Commands::Daemon { action } => return dispatch_daemon_command(profile, action).await,
        _ => unreachable!("non-runtime command passed to runtime dispatcher"),
    }
    Ok(CommandOutcome::Success)
}

async fn dispatch_daemon_command(
    profile: &ProfileRoot,
    action: DaemonAction,
) -> tracedecay_domain::errors::Result<CommandOutcome> {
    match action {
        DaemonAction::Run {
            socket,
            profile_root: _,
            remote_listen,
            remote_tls_cert,
            remote_tls_key,
        } => {
            // Long-lived host: allowed to run the structured-row sweep.
            tracedecay_store_runtime::mark_process_long_lived_for_session_maintenance();
            let socket_path =
                tracedecay_daemon_control::socket_path_or_default(profile.data_dir(), socket)?;
            let remote_tls = tracedecay_daemon_control::RemoteBrainTlsConfig::from_optional_parts(
                remote_listen,
                remote_tls_cert.map(PathBuf::from),
                remote_tls_key.map(PathBuf::from),
            )?;
            // Boxed on purpose: `run_foreground` is the daemon's entire
            // bootstrap state machine, and `Instrument::instrument` wraps by
            // value — unboxed, the whole machine inlines into this dispatch future and
            // overflows the main thread's stack at startup (measured tonight;
            // same class as the 37MB serve_broker_socket_client machine).
            Box::pin(tracing::Instrument::instrument(
                tracedecay::daemon::run_foreground(profile.clone(), socket_path, remote_tls),
                tracing::trace_span!("cli.daemon.run"),
            ))
            .await?;
        }
        DaemonAction::InstallService {
            socket,
            no_start,
            remote_listen,
            remote_tls_cert,
            remote_tls_key,
        } => {
            let tracedecay_bin = tracedecay_agent_hosts::agents::which_tracedecay_path()
                .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
                    message: "tracedecay not found on PATH".to_string(),
                })?;
            let remote_tls = tracedecay_daemon_control::RemoteBrainTlsConfig::from_optional_parts(
                remote_listen,
                remote_tls_cert.map(PathBuf::from),
                remote_tls_key.map(PathBuf::from),
            )?;
            let spec = tracedecay_daemon_control::service_spec_with_remote_tls(
                profile,
                tracedecay_bin,
                socket,
                remote_tls,
            )?;
            let service_path = {
                let _span = tracing::trace_span!("cli.daemon.install_service").entered();
                tracedecay_daemon_control::install_service(
                    &spec,
                    !no_start,
                    crate::product_runtime::PRODUCT_BUILD_VERSION,
                )
            }?;
            eprintln!(
                "Installed TraceDecay daemon service at {}",
                service_path.display()
            );
            if cfg!(windows) {
                let profile_root =
                    tracedecay_daemon_control::installed_service_socket_path(profile)?
                        .and_then(|path| path.parent().map(|parent| parent.to_path_buf()))
                        .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
                            message: "installed Windows daemon task has no absolute profile root"
                                .to_string(),
                        })?;
                eprintln!("Daemon profile root: {}", profile_root.display());
                eprintln!("Daemon endpoint: authenticated loopback (authority-discovered)");
            } else {
                eprintln!("Daemon socket: {}", spec.socket_path.display());
            }
        }
        DaemonAction::UninstallService { no_stop } => {
            let service_path = {
                let _span = tracing::trace_span!("cli.daemon.uninstall_service").entered();
                tracedecay_daemon_control::uninstall_service(
                    profile,
                    !no_stop,
                    crate::product_runtime::PRODUCT_BUILD_VERSION,
                )
            }?;
            eprintln!(
                "Removed TraceDecay daemon service at {}",
                service_path.display()
            );
        }
        DaemonAction::Start => {
            {
                let _span = tracing::trace_span!("cli.daemon.start").entered();
                tracedecay_daemon_control::start_service(
                    profile,
                    crate::product_runtime::PRODUCT_BUILD_VERSION,
                )
            }?;
            eprintln!("Started TraceDecay daemon service");
        }
        DaemonAction::Stop => {
            {
                let _span = tracing::trace_span!("cli.daemon.stop").entered();
                tracedecay_daemon_control::stop_service(
                    profile,
                    crate::product_runtime::PRODUCT_BUILD_VERSION,
                )
            }?;
            eprintln!("Stopped TraceDecay daemon service");
        }
        DaemonAction::Restart => {
            {
                let _span = tracing::trace_span!("cli.daemon.restart").entered();
                update_cmd::restart_daemon_service(profile)
            }?;
        }
        DaemonAction::Status => {
            let socket_path =
                tracedecay_daemon_control::socket_path_or_default(profile.data_dir(), None)?;
            let status = {
                let _span = tracing::trace_span!("cli.daemon.status").entered();
                tracedecay_daemon_control::service_status(
                    profile,
                    &socket_path,
                    crate::product_runtime::PRODUCT_BUILD_VERSION,
                )
            };
            print!("{status}");
            return Ok(if status.is_ready() {
                CommandOutcome::Success
            } else {
                CommandOutcome::Exit(1)
            });
        }
    }
    Ok(CommandOutcome::Success)
}

/// A lifecycle that committed everything it could but left a host waiting on
/// an operator step exits with its own status instead of plain success.
fn lifecycle_command_outcome(completion: agent_cmd::HostLifecycleCompletion) -> CommandOutcome {
    match completion {
        agent_cmd::HostLifecycleCompletion::Complete => CommandOutcome::Success,
        agent_cmd::HostLifecycleCompletion::PendingOperatorAction => {
            CommandOutcome::Exit(completion.exit_code())
        }
    }
}

async fn dispatch_agent_command(
    profile: &ProfileRoot,
    command: Commands,
    host_bundle: HostBundleCliOptions,
) -> tracedecay_domain::errors::Result<agent_cmd::HostLifecycleCompletion> {
    use agent_cmd::HostBundleCliOperation as Operation;

    let (operation, agent, local, no_dashboard, automation, git_hook) = match command {
        Commands::FeedbackRollback { mut action } => {
            if host_bundle.component.is_some() || host_bundle.dry_run {
                return Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: "feedback-rollback does not accept host-component selectors"
                        .to_string(),
                });
            }
            match &mut action {
                crate::cli::FeedbackRollbackAction::Apply { yes, .. }
                | crate::cli::FeedbackRollbackAction::Restore { yes, .. } => {
                    *yes |= host_bundle.yes;
                }
                crate::cli::FeedbackRollbackAction::DryRun { .. } => {}
            }
            agent_cmd::handle_feedback_rollback_command(profile, action).await?;
            return Ok(agent_cmd::HostLifecycleCompletion::Complete);
        }
        Commands::Install {
            agent,
            local,
            no_dashboard,
            automation,
            git_hook,
        } => (
            Operation::Install,
            agent,
            local,
            no_dashboard,
            automation,
            git_hook,
        ),
        Commands::Reinstall { local, agent } => {
            (Operation::Repair, agent, local, false, false, false)
        }
        Commands::UpdatePlugin { local, agent } => {
            (Operation::Update, agent, local, false, false, false)
        }
        Commands::Uninstall { agent, local } => {
            (Operation::Uninstall, agent, local, false, false, false)
        }
        _ => unreachable!("non-agent command passed to agent dispatcher"),
    };
    let completion = if local {
        if host_bundle.component.is_some() || host_bundle.dry_run {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "--component and --dry-run cannot be combined with --local".to_string(),
            });
        }
        let agent_id = agent.ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
            message: "--local requires a project-capable --agent".to_string(),
        })?;
        agent_cmd::handle_project_local_lifecycle_command(profile, agent_id, operation).await?;
        agent_cmd::HostLifecycleCompletion::Complete
    } else {
        agent_cmd::handle_host_lifecycle_command(
            profile,
            agent,
            operation,
            host_bundle,
            no_dashboard,
            automation.then_some(agent_cmd::CodexAutomationInstall),
        )
        .await?
    };
    if git_hook {
        agent_cmd::install_requested_git_hook(profile)?;
    } else if operation == Operation::Install
        && let Some(home) = profile.home()
    {
        tracedecay_agent_hosts::agents::report_git_post_commit_hook_status(home);
    }
    Ok(completion)
}

async fn dispatch_hook_command(
    profile: &ProfileRoot,
    command: Commands,
) -> tracedecay_domain::errors::Result<CommandOutcome> {
    let code = match command {
        hook_command @ (Commands::HookPreToolUse
        | Commands::HookPromptSubmit
        | Commands::HookStop
        | Commands::HookClaudeSessionStart
        | Commands::HookClaudePostToolUse
        | Commands::HookClaudePostCompact
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
        | Commands::HookKimiEvent
        | Commands::HookOpenCodeEvent
        | Commands::HookOpenCodeToolAfter
        | Commands::HookPiEvent
        | Commands::HookDroidEvent) => {
            hook_cmd::handle_hook_command(profile.clone(), hook_command).await?
        }
        _ => unreachable!("non-hook command passed to hook dispatcher"),
    };
    Ok(CommandOutcome::Exit(code))
}

async fn dispatch_update_command(
    profile: &ProfileRoot,
    command: Commands,
) -> tracedecay_domain::errors::Result<agent_cmd::HostLifecycleCompletion> {
    match command {
        Commands::Upgrade { no_reinstall } => {
            update_cmd::run_upgrade_command(profile, no_reinstall).await?;
        }
        Commands::Update { no_reinstall } => {
            return update_cmd::run_update_command(profile, no_reinstall).await;
        }
        Commands::PostUpdate {
            no_reinstall,
            lifecycle_lease_token,
        } => {
            return update_cmd::run_post_update_command(
                profile,
                no_reinstall,
                lifecycle_lease_token.as_deref(),
            )
            .await;
        }
        Commands::PackageHook {
            action: PackageHookAction::Scoop { action },
        } => match action {
            ScoopPackageHookAction::Prepare {
                package_id,
                state_file,
            } => {
                {
                    let _span = tracing::trace_span!("cli.package_hook.prepare").entered();
                    tracedecay_daemon_control::prepare_scoop_package_service(
                        &package_id,
                        &state_file,
                        crate::product_runtime::PRODUCT_BUILD_VERSION,
                    )
                }?;
            }
            ScoopPackageHookAction::Restore {
                package_id,
                state_file,
            } => {
                {
                    let _span = tracing::trace_span!("cli.package_hook.restore").entered();
                    tracedecay_daemon_control::restore_scoop_package_service(
                        &package_id,
                        &state_file,
                        crate::product_runtime::PRODUCT_BUILD_VERSION,
                    )
                }?;
            }
        },
        Commands::Channel { channel } => match channel {
            Some(target) => {
                {
                    let _span = tracing::trace_span!("cli.channel.switch").entered();
                    crate::upgrade::switch_channel(profile, &target)
                }?;
            }
            None => {
                let _span = tracing::trace_span!("cli.channel.show").entered();
                crate::upgrade::show_channel()
            }
        },
        _ => unreachable!("non-update command passed to update dispatcher"),
    }
    Ok(agent_cmd::HostLifecycleCompletion::Complete)
}

async fn dispatch_configuration_command(
    profile: &ProfileRoot,
    command: Commands,
) -> tracedecay_domain::errors::Result<()> {
    match command {
        Commands::CurrentCounter { path } => {
            let project_path = tracedecay_configuration::resolve_path(path);
            let value = tracing::Instrument::instrument(
                commands::local_counter(profile, &project_path),
                tracing::trace_span!("cli.counter.current"),
            )
            .await?;
            println!("{value}");
        }
        Commands::ResetCounter { path } => {
            let project_path = tracedecay_configuration::resolve_path(path);
            let prev = commands::local_counter(profile, &project_path).await?;
            let reset = tracing::Instrument::instrument(
                commands::admin_project(
                    profile,
                    &project_path,
                    AdminProjectSurfaceRequestV1::CounterReset {},
                ),
                tracing::trace_span!("cli.counter.reset"),
            )
            .await?;
            if !matches!(reset, AdminProjectResultV1::CounterReset(_)) {
                return Err(commands::unexpected_admin_project_result());
            }
            eprintln!("Local counter reset (was {prev})");
        }
        Commands::DisableUploadCounter => {
            commands::handle_upload_counter(profile, false).await?;
        }
        Commands::EnableUploadCounter => {
            commands::handle_upload_counter(profile, true).await?;
        }
        _ => unreachable!("non-configuration command passed to configuration dispatcher"),
    }
    Ok(())
}

async fn dispatch_diagnostics_command(
    profile: &ProfileRoot,
    command: Commands,
) -> tracedecay_domain::errors::Result<CommandOutcome> {
    match command {
        Commands::Doctor { json } => {
            let completion = tracing::Instrument::instrument(
                tracedecay::doctor::run_doctor(
                    profile,
                    crate::cloud::doctor_network_probes(),
                    json,
                ),
                tracing::trace_span!("cli.doctor.run"),
            )
            .await?;
            match completion {
                tracedecay::doctor::DoctorCompletion::Healthy => {}
                tracedecay::doctor::DoctorCompletion::PendingOperatorAction => {
                    return Ok(CommandOutcome::Exit(
                        agent_cmd::PENDING_OPERATOR_ACTION_EXIT_CODE,
                    ));
                }
                tracedecay::doctor::DoctorCompletion::Issues(_) => {
                    return Ok(CommandOutcome::Exit(1));
                }
            }
        }
        Commands::Cost {
            range,
            by_model,
            export,
        } => {
            cost_cmd::handle_cost(profile, range, by_model, export).await?;
        }
        Commands::Gain {
            all,
            history,
            range,
            json,
        } => {
            commands::handle_gain(profile, all, history, &range, json).await?;
        }
        Commands::Monitor => {
            {
                let _span = tracing::trace_span!("cli.monitor.run").entered();
                monitor_cmd::run(profile)
            }?;
        }
        _ => unreachable!("non-diagnostics command passed to diagnostics dispatcher"),
    }
    Ok(CommandOutcome::Success)
}

async fn dispatch_knowledge_command(
    profile: &ProfileRoot,
    command: Commands,
    dry_run: bool,
) -> tracedecay_domain::errors::Result<()> {
    match command {
        Commands::Git { action } => {
            git_cmd::handle_git_action(profile, action).await?;
        }
        Commands::Sessions { action } => {
            sessions_cmd::handle_sessions_action(profile, action, dry_run).await?;
        }
        Commands::Analytics { action } => match action {
            AnalyticsAction::Diagnostics { all, no_sync } => {
                tracing::Instrument::instrument(
                    analytics_cmd::run_analytics_diagnostics(profile, all, no_sync),
                    tracing::trace_span!("cli.analytics.diagnostics"),
                )
                .await?;
            }
            AnalyticsAction::Sync => {
                tracing::Instrument::instrument(
                    analytics_cmd::run_analytics_sync(profile),
                    tracing::trace_span!("cli.analytics.sync"),
                )
                .await?;
            }
        },
        Commands::Automation { action } => {
            automation_cli::handle_automation_command(profile, action).await?;
        }
        _ => unreachable!("non-knowledge command passed to knowledge dispatcher"),
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandStartupPolicy {
    Full,
    SkipAgentInstallCheck,
    SkipAll,
}

impl CommandStartupPolicy {
    fn for_command(command: &Commands) -> Self {
        if hook_capture_cmd::is_native_hook_command(command) {
            return Self::SkipAll;
        }

        match command {
            // Tool calls are the documented MCP fallback and must remain a local,
            // latency-bounded protocol path. Unrelated counter uploads or agent
            // maintenance belong on interactive commands and daemon background work.
            Commands::Tool { .. }
            | Commands::Status { .. }
            | Commands::Work { .. }
            | Commands::Workflow { .. }
            | Commands::Remote { .. }
            | Commands::Git { .. } => Self::SkipAll,
            // Explicit lifecycle/maintenance commands manage their own work.
            // Serve is also latency-sensitive: clients impose a 30 s MCP
            // initialize timeout, so no implicit startup work belongs there.
            Commands::Install { .. }
            | Commands::Reinstall { .. }
            | Commands::UpdatePlugin { .. }
            | Commands::FeedbackRollback { .. }
            | Commands::Upgrade { .. }
            | Commands::Update { .. }
            | Commands::PostUpdate { .. }
            | Commands::PackageHook { .. }
            | Commands::Uninstall { .. }
            | Commands::Lsp { .. }
            | Commands::Doctor { .. }
            | Commands::Analytics { .. }
            | Commands::Sessions {
                action:
                    SessionsAction::Import { .. }
                    | SessionsAction::GitSync { .. }
                    | SessionsAction::Unfinished { .. }
                    | SessionsAction::UnusedContext { .. },
            }
            | Commands::Storage { .. }
            | Commands::Wipe { .. }
            | Commands::Projects { .. }
            | Commands::Daemon { .. }
            | Commands::Serve { .. } => Self::SkipAll,
            // Inspection-only commands retain ordinary startup maintenance but
            // do not need the unrelated agent-install health check.
            Commands::CurrentCounter { .. }
            | Commands::Cost { .. }
            | Commands::Gain { .. }
            | Commands::Monitor
            | Commands::List { .. }
            | Commands::Memory {
                action: MemoryAction::Status { .. },
            }
            | Commands::Sessions {
                action:
                    SessionsAction::Search(_)
                    | SessionsAction::Refresh {
                        action: SessionsRefreshAction::Status(_),
                    },
            }
            | Commands::Branch {
                action:
                    BranchAction::List { .. }
                    | BranchAction::Autotrack {
                        action: BranchAutotrackAction::Status { .. },
                    },
            }
            | Commands::Channel { channel: None }
            | Commands::Automation {
                action:
                    AutomationAction::Config {
                        action:
                            AutomationConfigAction::Get { .. } | AutomationConfigAction::Explain { .. },
                    }
                    | AutomationAction::Runs {
                        action:
                            AutomationRunsAction::List { .. }
                            | AutomationRunsAction::View { .. }
                            | AutomationRunsAction::Artifact { .. },
                    }
                    | AutomationAction::Skills {
                        action:
                            AutomationSkillsAction::List { .. } | AutomationSkillsAction::View { .. },
                    }
                    | AutomationAction::Facts {
                        action:
                            AutomationFactsAction::List { .. } | AutomationFactsAction::View { .. },
                    },
            } => Self::SkipAgentInstallCheck,
            // Unknown and mutating actions conservatively retain the full
            // preamble. Read-only actions must opt in above by exact variant.
            _ => Self::Full,
        }
    }

    fn runs_startup_maintenance(self) -> bool {
        !matches!(self, Self::SkipAll)
    }

    fn runs_agent_install_check(self) -> bool {
        matches!(self, Self::Full)
    }
}

fn runs_worldwide_counter_flush(command: &Commands) -> bool {
    !matches!(command, Commands::Init { .. })
        && CommandStartupPolicy::for_command(command).runs_startup_maintenance()
}

#[cfg(test)]
fn should_skip_startup_maintenance(command: &Commands) -> bool {
    !CommandStartupPolicy::for_command(command).runs_startup_maintenance()
}

#[cfg(test)]
fn should_skip_agent_install_check(command: &Commands) -> bool {
    !CommandStartupPolicy::for_command(command).runs_agent_install_check()
}

fn is_local_install_command(command: &Commands) -> bool {
    matches!(command, Commands::Install { local: true, .. })
}

fn normalize_tool_reserved_global_flags(cli: &mut Cli) {
    if !cli.dry_run {
        return;
    }
    let Some(Commands::Tool { args, .. }) = cli.command.as_mut() else {
        return;
    };
    // Clap recognizes the lifecycle-global `--dry-run` before a tool's first
    // trailing argument. Return it to the tool parser so the documented
    // reserved flag has the same meaning on either side of `--args`.
    args.push("--dry-run".to_owned());
    cli.dry_run = false;
}

#[cfg(test)]
mod startup_tests;
