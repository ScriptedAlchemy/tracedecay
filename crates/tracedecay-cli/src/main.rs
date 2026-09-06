#![allow(clippy::too_many_arguments, clippy::collapsible_if)]
// binary crate: match lib allow policy for CLI dispatch
// Required for the hotpath feature: layout computation for the boxed
// `_inner` async body chain reachable from `run()` overflows the default
// query depth ("query depth increased by 130").
#![recursion_limit = "256"]
#[cfg(any(feature = "hotpath", test))]
use clap::ArgMatches;
use clap::{CommandFactory, FromArgMatches};
#[cfg(any(feature = "hotpath", test))]
use std::ffi::OsStr;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
#[cfg(feature = "hotpath")]
use std::sync::{Arc, Mutex};

#[cfg(feature = "hotpath-alloc")]
#[global_allocator]
static HOTPATH_ALLOCATOR: hotpath::CountingAllocator = hotpath::CountingAllocator::new();

// Opt-in allocator features (see Cargo.toml). Exactly one global allocator
// may exist per binary, so overlapping selections resolve by fixed precedence
// rather than a compile error: hotpath-alloc's counting allocator wins in
// measurement builds, then jemalloc, then mimalloc. The default build keeps
// the system allocator (glibc malloc on Linux), whose retained-arena behavior
// the daemon compensates for with `malloc_trim` at maintenance boundaries and
// `MALLOC_ARENA_MAX=2` in the installed service unit; neither compensation is
// load-bearing under jemalloc or mimalloc.
#[cfg(all(feature = "alloc-jemalloc", not(feature = "hotpath-alloc")))]
#[global_allocator]
static JEMALLOC_ALLOCATOR: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[cfg(all(
    feature = "alloc-mimalloc",
    not(feature = "alloc-jemalloc"),
    not(feature = "hotpath-alloc")
))]
#[global_allocator]
static MIMALLOC_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod agent_cmd;
mod analytics_cmd;
mod automation_cli;
mod cli;
mod commands;
mod cost_cmd;
mod display;
mod git_cmd;
mod global;
mod hook_capture_cmd;
mod hook_cmd;
mod lsp_cmd;
mod monitor_cmd;
mod product_runtime;
mod project_cmd;
mod remote_command;
mod semantic_cmd;
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
use tracedecay::daemon::StderrTracingDefault;

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
const MAX_ASYNC_WORKER_THREADS: usize = 16;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AsyncRuntimeFlavor {
    CurrentThread,
    MultiThread,
}
#[cfg(feature = "hotpath")]
const HOTPATH_OUTPUT_FORMAT_ENV: &str = "HOTPATH_OUTPUT_FORMAT";
#[cfg(feature = "hotpath")]
const HOTPATH_OUTPUT_PATH_ENV: &str = "HOTPATH_OUTPUT_PATH";
#[cfg(feature = "hotpath")]
const HOTPATH_FOCUS_ENV: &str = "HOTPATH_FOCUS";
#[cfg(feature = "hotpath")]
const HOTPATH_METRICS_SERVER_OFF_ENV: &str = "HOTPATH_METRICS_SERVER_OFF";
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
        // `tool` is a one-shot daemon client. A multi-thread runtime eagerly
        // starts up to 16 workers even though the command drives one socket
        // request and exits; the current thread already has a fixed 16 MiB
        // stack and Tokio's blocking pool remains available when needed.
        Some(Commands::Tool { .. }) => AsyncRuntimeFlavor::CurrentThread,
        _ => AsyncRuntimeFlavor::MultiThread,
    }
}

/// Keep enough bounded blocking workers to run every admitted background CPU
/// unit plus serving work that does not consume that CPU budget. Before the
/// profile-scoped worker plan is installed, using the host width is the safe
/// upper bound for any later exact plan. The result is host-bounded: it is at
/// most `available + MIN_SERVING_BLOCKING_RESERVE`.
fn tokio_blocking_thread_limit() -> usize {
    let available = std::thread::available_parallelism().map_or(1, usize::from);
    let effective = tracedecay::code_index::parallelism::installed_worker_status()
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

fn process_exit_code(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

#[cfg(any(feature = "hotpath", test))]
fn hotpath_output_format_is_valid(output_format: Option<&OsStr>) -> bool {
    output_format.is_none_or(|value| {
        value.to_str().is_some_and(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "table" | "json" | "json-pretty" | "jsonpretty" | "none"
            )
        })
    })
}

#[cfg(any(feature = "hotpath", test))]
fn hotpath_output_format_is_none(output_format: Option<&OsStr>) -> bool {
    output_format
        .and_then(OsStr::to_str)
        .is_some_and(|value| value.eq_ignore_ascii_case("none"))
}

#[cfg(any(feature = "hotpath", test))]
fn hotpath_output_path_is_valid(output_path: Option<&OsStr>) -> bool {
    output_path.is_none_or(|value| value.to_str().is_some_and(|value| !value.is_empty()))
}

#[cfg(any(feature = "hotpath", test))]
fn hotpath_focus_is_valid(focus: Option<&OsStr>) -> bool {
    let Some(focus) = focus.and_then(OsStr::to_str) else {
        // Hotpath also uses `std::env::var`, so non-Unicode focus is treated
        // as absent rather than parsed.
        return true;
    };
    focus
        .strip_prefix('/')
        .and_then(|pattern| pattern.strip_suffix('/'))
        .is_none_or(|pattern| regex::Regex::new(pattern).is_ok())
}

#[cfg(any(feature = "hotpath", test))]
fn hotpath_requires_protocol_safe_output(
    hook_protocol: bool,
    usable_output_path: bool,
    output_format: Option<&OsStr>,
) -> bool {
    !usable_output_path && (hook_protocol || output_format.is_none())
}

#[cfg(feature = "hotpath")]
fn configure_hotpath_output(args: &[std::ffi::OsString]) -> Result<(), String> {
    let hook_protocol = hook_capture_cmd::is_hook_protocol_invocation(args);
    if hook_protocol {
        // Hook stderr belongs to the host and the process serves exactly one
        // request, so the live metrics endpoint has no consumer here. Losing
        // the fixed-port race would print a hotpath error onto the host's
        // stderr stream, which hosts read as a hook failure.
        unsafe {
            std::env::set_var(HOTPATH_METRICS_SERVER_OFF_ENV, "1");
        }
    }
    let output_path = std::env::var_os(HOTPATH_OUTPUT_PATH_ENV);
    let output_format = std::env::var_os(HOTPATH_OUTPUT_FORMAT_ENV);
    let focus = std::env::var_os(HOTPATH_FOCUS_ENV);
    let valid_path = hotpath_output_path_is_valid(output_path.as_deref());
    let valid_format = hotpath_output_format_is_valid(output_format.as_deref());
    let valid_focus = hotpath_focus_is_valid(focus.as_deref());
    if !hook_protocol && !valid_path {
        return Err(format!(
            "{HOTPATH_OUTPUT_PATH_ENV} must be a non-empty Unicode path"
        ));
    }
    if !hook_protocol && !valid_focus {
        return Err(format!(
            "{HOTPATH_FOCUS_ENV} contains an invalid /regular expression/"
        ));
    }
    if hook_protocol && !valid_focus {
        // Hotpath compiles /regex/ focus lazily from the first measurement
        // guard and panics on an invalid pattern. An empty text focus matches
        // every label and preserves the hook's status without host output.
        unsafe {
            std::env::set_var(HOTPATH_FOCUS_ENV, "");
        }
    }
    let force_report_off = hook_protocol && (!valid_path || !valid_format);
    if !hook_protocol && !valid_format {
        return Err(format!(
            "{HOTPATH_OUTPUT_FORMAT_ENV} must be one of table, json, json-pretty, or none"
        ));
    }
    let report_off = hotpath_output_format_is_none(output_format.as_deref())
        || force_report_off
        || hotpath_requires_protocol_safe_output(
            hook_protocol,
            output_path.is_some() && valid_path,
            output_format.as_deref(),
        );
    if report_off {
        // No feature-enabled process may append a default table to an ordinary
        // CLI protocol stream. Hooks are stricter: malformed output variables
        // and an explicit stdout format are ignored unless Hotpath can read a
        // non-empty report path through its Unicode environment API.
        unsafe {
            std::env::set_var(HOTPATH_OUTPUT_FORMAT_ENV, "none");
        }
        // Hotpath resolves and opens its writer before it handles the `none`
        // format. Remove even a valid path whenever reports are off so guard
        // drop cannot create, truncate, or diagnose an output destination.
        unsafe {
            std::env::remove_var(HOTPATH_OUTPUT_PATH_ENV);
        }
    }
    Ok(())
}

#[cfg(feature = "hotpath")]
fn hotpath_guard() -> hotpath::HotpathGuard {
    // The CPU report section autospawns an external `hotpath-samply`/`samply`
    // profiler that SIGSTOPs this process while it attaches perf sampling and
    // SIGCONTs it only once the attach succeeds. A profiler failure inside
    // that window leaves the process stopped forever, so headless invocations
    // (hooks, `--yes` flows, protocol streams) must never enter it implicitly.
    // CPU sampling remains available only by explicit operator request:
    // `HOTPATH_REPORT` (e.g. `functions-cpu`) takes precedence over this
    // default exclusion.
    hotpath::HotpathGuardBuilder::new("tracedecay")
        .sections_exclude(vec![hotpath::Section::FunctionsCpu])
        .build()
}

#[cfg(feature = "hotpath")]
struct ProcessHotpathGuard {
    guard: Arc<Mutex<Option<hotpath::HotpathGuard>>>,
}

#[cfg(feature = "hotpath")]
impl ProcessHotpathGuard {
    #[hotpath::measure(label = "cli.hotpath.install_shutdown_finalizer")]
    fn install(guard: hotpath::HotpathGuard) -> Result<Self, String> {
        let guard = Arc::new(Mutex::new(Some(guard)));
        let watchdog_guard = Arc::clone(&guard);
        if !tracedecay::daemon::install_hotpath_shutdown_finalizer(move || {
            let guard = watchdog_guard
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            drop(guard);
        }) {
            return Err("Hotpath shutdown finalizer is already installed".to_owned());
        }
        Ok(Self { guard })
    }
}

#[cfg(feature = "hotpath")]
impl Drop for ProcessHotpathGuard {
    fn drop(&mut self) {
        let guard = self
            .guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        drop(guard);
    }
}

fn main() -> ExitCode {
    let args = std::env::args_os().collect::<Vec<_>>();
    #[cfg(feature = "hotpath")]
    if let Err(message) = configure_hotpath_output(&args) {
        eprintln!("Error: {message}");
        return ExitCode::FAILURE;
    }
    #[cfg(feature = "hotpath")]
    let _hotpath = match ProcessHotpathGuard::install(hotpath_guard()) {
        Ok(guard) => guard,
        Err(message) => {
            eprintln!("Error: {message}");
            return ExitCode::FAILURE;
        }
    };
    // The guard belongs to the real process boundary rather than the async
    // command body. Native capture hooks intentionally bypass the ordinary
    // composition root, and Clap can terminate before a Tokio runtime exists;
    // both still need one complete Hotpath lifetime and a flushed report in a
    // feature-enabled profiling build.
    #[cfg(feature = "hotpath")]
    if let Some(command) = args.get(1).and_then(|value| value.to_str()) {
        hotpath::val!("cli.command.name").set(&command);
    }
    if let Some(code) =
        hotpath::measure_block!("cli.hook.native_capture", hook_capture_cmd::try_run(&args))
    {
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
            eprintln!("Error: {}", e);
            ExitCode::FAILURE
        }
    }
}

fn async_main() -> tracedecay_domain::errors::Result<CommandOutcome> {
    // This binary is the sole generator of source provenance and the embedded
    // dashboard bundle; the composition library reads both through this
    // set-once registration.
    tracedecay::register_product_runtime(crate::product_runtime::provider())?;
    // Every process-global runtime port the extracted crates invert back into
    // the composition root. Must precede argument parsing: hook, install, and
    // ingest paths all read these slots, and an unregistered slot fails quietly
    // (no LCM redaction, no memory injection, zero turn costs) rather than
    // loudly. The agent-host MCP catalog is no longer among them — host
    // installers read it from `tracedecay-mcp` on demand, so `tool` still
    // pays nothing for the ~160 schemas it never looks at.
    tracedecay::register_runtime_ports()?;
    let args: Vec<String> = std::env::args().collect();
    #[cfg(feature = "hotpath")]
    if let Some(command) = args.get(1) {
        // Fallback identity for Clap help/version/parse failures. A successful
        // parse replaces it below with the exact canonical nested command path.
        hotpath::val!("cli.command.name").set(&command.as_str());
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
    #[cfg(feature = "hotpath")]
    let command_name = command_profile_label(&matches);
    let mut cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(error) => {
            let code = error.exit_code();
            error.print()?;
            return Ok(CommandOutcome::Exit(code));
        }
    };
    normalize_tool_reserved_global_flags(&mut cli);
    if let Some(Commands::Daemon {
        action:
            DaemonAction::Run {
                profile_root: Some(profile_root),
                ..
            },
    }) = cli.command.as_ref()
    {
        // The foreground daemon is the only long-lived owner in this process.
        // Pin its profile before Tokio starts worker threads so every canonical
        // configuration authority observes the Task Scheduler argument.
        unsafe {
            std::env::set_var(tracedecay::config::USER_DATA_DIR_ENV, profile_root);
        }
    }
    // Route tracing events (degradation causes, ingest warnings) to stderr —
    // without a subscriber every `tracing::warn!` in the runtime is silently
    // dropped, which hid the causes behind typed catch-up reason codes. The
    // daemon runs through this same entrypoint, so this is also the daemon's
    // subscriber; RUST_LOG raises verbosity (default `warn`).
    //
    // Installed after parsing rather than first thing: hook stderr belongs to
    // the host, so which command is running has to be known before anything
    // is allowed to write there.
    tracedecay::daemon::install_stderr_tracing(stderr_tracing_default(cli.command.as_ref()));
    // Bound only Rayon's global pool for daemon workloads that actually use
    // it. Code indexing owns a separately planned pool shared by semantic
    // projection, so changing this ceiling cannot silently narrow that budget.
    hotpath::measure_block!(
        "daemon_cpu_pool_install",
        install_daemon_cpu_pool(cli.command.as_ref())
    )?;
    let runtime_flavor = async_runtime_flavor(cli.command.as_ref());
    let worker_threads = match runtime_flavor {
        AsyncRuntimeFlavor::CurrentThread => 1,
        AsyncRuntimeFlavor::MultiThread => async_worker_threads(),
    };
    let blocking_threads = tokio_blocking_thread_limit();
    let runtime = hotpath::measure_block!("tokio_runtime_build", {
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
                .build(),
        };
        build.map_err(|e| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("failed to start async runtime: {e}"),
        })
    })?;
    #[cfg(feature = "hotpath")]
    {
        hotpath::tokio_runtime!(runtime.handle());
        // Process-level runtime shape only. Request, project-server, history,
        // and projection gauges belong on those authorities — not bootstrap.
        hotpath::gauge!("tokio_worker_threads").set(worker_threads);
        hotpath::gauge!("tokio_blocking_threads").set(blocking_threads);
        let command_family = cli.command.as_ref().map_or("none", |command| {
            CommandFamily::for_command(command).as_profile_label()
        });
        hotpath::val!("process_command_family").set(&command_family);
        hotpath::val!("cli.command.name").set(&command_name.as_str());
        hotpath::gauge!("process_in_command").set(1);
    }
    #[cfg(feature = "hotpath")]
    let result = hotpath::measure_block!(
        "process_command",
        runtime.block_on(hotpath::future!(run(cli), label = "process_command_future"))
    );
    #[cfg(not(feature = "hotpath"))]
    let result = runtime.block_on(run(cli));
    #[cfg(feature = "hotpath")]
    hotpath::gauge!("process_in_command").set(0);
    // Runtime drop waits indefinitely for blocking tasks. Daemon integrations
    // can leave OS-backed watcher work behind after their async handles abort,
    // so bound teardown after the command's own graceful shutdown completes.
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    result
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
#[cfg(any(feature = "hotpath", test))]
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
    let command = match cli.command {
        Some(cmd) => cmd,
        None => {
            commands::handle_no_command().await?;
            return Ok(CommandOutcome::Success);
        }
    };

    run_startup_preamble(&command).await;
    dispatch_command(command, host_bundle).await
}

#[hotpath::measure(label = "cli.startup.preamble", future = true)]
async fn run_startup_preamble(command: &Commands) {
    let startup_policy = CommandStartupPolicy::for_command(command);

    // Check first-run before any config save creates the file.
    let is_first_run = tracedecay_session_memory::user_config::UserConfig::is_fresh();

    let is_force_flush = matches!(
        command,
        Commands::Init { .. } | Commands::Sync { .. } | Commands::Status { .. }
    );
    let mut user_config = tracedecay_session_memory::user_config::UserConfig::load();
    // Skip the worldwide-counter flush on hot startup paths. `try_flush`
    // makes a synchronous HTTP call which can add seconds to
    // `tracedecay serve` startup on slow networks — long enough to blow the
    // MCP client's 30 s `initialize` timeout. The canonical setting lookup is
    // only consulted when there are pending tokens to flush: with nothing
    // pending the setting cannot change behavior, and probing it on every
    // command turned the daemon's transient "runtime still mounting" state
    // into per-command stderr noise. A failed lookup on an ordinary command
    // is deferred (the next command retries); the flush-bearing commands
    // (`init`, `sync`, `status`) still surface it, so a persistent failure
    // stays visible exactly where the flush is expected to happen.
    if startup_policy.runs_startup_maintenance()
        && user_config.pending_upload > 0
        && let Ok(cwd) = std::env::current_dir()
        && let Some(project_root) =
            tracedecay::config::discover_project_root_with_identity(&cwd).await
    {
        match commands::canonical_upload_enabled(&project_root).await {
            Ok(upload_enabled) => {
                global::try_flush(&mut user_config, is_force_flush, upload_enabled);
            }
            Err(error) if is_force_flush => {
                eprintln!(
                    "warning: canonical worldwide-counter upload setting is unavailable: {error}"
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
        && let Err(err) = user_config.save_if_exists()
    {
        eprintln!("warning: could not save tracedecay config: {err}");
    }

    if is_first_run && startup_policy.runs_startup_maintenance() {
        eprintln!(
            "note: tracedecay can optionally upload anonymous token savings counts to a worldwide counter.\n\
             \x20     Run `tracedecay enable-upload-counter` to opt in."
        );
    }

    if startup_policy.runs_agent_install_check() {
        tracedecay::agents::claude::check_install_stale();
    }
}

async fn resolve_registered_project_root(
    project_id: Option<String>,
    project_path: Option<String>,
) -> tracedecay_domain::errors::Result<Option<PathBuf>> {
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
        .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
            message: "registered project not found for selector".to_string(),
        })?;
    Ok(Some(PathBuf::from(display_root)))
}

pub(crate) async fn resolve_cli_project_root(
    path: Option<String>,
    project_id: Option<String>,
    project_path: Option<String>,
) -> tracedecay_domain::errors::Result<PathBuf> {
    if let Some(root) = resolve_registered_project_root(project_id, project_path).await? {
        return Ok(root);
    }
    Ok(tracedecay::config::resolve_path_with_discovery(path))
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
    #[cfg(feature = "hotpath")]
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
            | Commands::Semantic { .. }
            | Commands::Lsp { .. }
            | Commands::Remote { .. }
            | Commands::Dashboard { .. }
            | Commands::Serve { .. }
            | Commands::Daemon { .. } => Self::Runtime,
            Commands::Install { .. }
            | Commands::Reinstall { .. }
            | Commands::UpdatePlugin { .. }
            | Commands::Uninstall { .. }
            | Commands::FeedbackRollback { .. }
            | Commands::HostBundle { .. } => Self::Agent,
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
            | Commands::HookOpenCodeToolAfter => Self::Hook,
            Commands::Upgrade { .. }
            | Commands::Update { .. }
            | Commands::PostUpdate { .. }
            | Commands::PackageHook { .. }
            | Commands::Channel { .. } => Self::Update,
            Commands::CurrentCounter { .. }
            | Commands::ResetCounter { .. }
            | Commands::DisableUploadCounter
            | Commands::EnableUploadCounter
            | Commands::Gitignore { .. } => Self::Configuration,
            Commands::Doctor
            | Commands::Cost { .. }
            | Commands::Bench { .. }
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
    // The scoped storage resets destroy refused store state, so they REQUIRE
    // the same `--yes` confirmation (their handlers refuse to run without it).
    // Like `wipe`, they own no host component and have no preview.
    if matches!(
        command,
        Commands::Storage {
            action: ProfileStorageAction::ResetAuthority { .. }
                | ProfileStorageAction::ResetProjectStore { .. },
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

fn is_full_component_set_adoption(command: &Commands, host_bundle: &HostBundleCliOptions) -> bool {
    host_bundle.component.is_none()
        && host_bundle.yes
        && host_bundle.adopt
        && matches!(
            command,
            Commands::Install { local: false, .. }
                | Commands::Reinstall { local: false, .. }
                | Commands::UpdatePlugin { local: false, .. }
        )
}

async fn dispatch_command(
    command: Commands,
    host_bundle: HostBundleCliOptions,
) -> tracedecay_domain::errors::Result<CommandOutcome> {
    let family = CommandFamily::for_command(&command);
    validate_host_bundle_options(&command, family, &host_bundle)?;
    match family {
        CommandFamily::Project => {
            dispatch_project_command(command, host_bundle.yes, host_bundle.dry_run).await?;
            Ok(CommandOutcome::Success)
        }
        CommandFamily::Runtime => {
            dispatch_runtime_command(command).await?;
            Ok(CommandOutcome::Success)
        }
        CommandFamily::Agent => {
            dispatch_agent_command(command, host_bundle).await?;
            Ok(CommandOutcome::Success)
        }
        CommandFamily::Hook => dispatch_hook_command(command).await,
        CommandFamily::Update => {
            dispatch_update_command(command).await?;
            Ok(CommandOutcome::Success)
        }
        CommandFamily::Configuration => {
            dispatch_configuration_command(command).await?;
            Ok(CommandOutcome::Success)
        }
        CommandFamily::Diagnostics => {
            dispatch_diagnostics_command(command).await?;
            Ok(CommandOutcome::Success)
        }
        CommandFamily::Knowledge => {
            dispatch_knowledge_command(command).await?;
            Ok(CommandOutcome::Success)
        }
    }
}

async fn dispatch_project_command(
    command: Commands,
    assume_yes: bool,
    dry_run: bool,
) -> tracedecay_domain::errors::Result<()> {
    match command {
        Commands::Init {
            path,
            path_flag,
            skip_folders,
            include_folders,
            adopt_project,
            fresh,
        } => {
            // clap enforces that at most one of these is present.
            commands::handle_init(
                path.or(path_flag),
                skip_folders,
                include_folders,
                adopt_project,
                fresh,
                assume_yes,
            )
            .await?;
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
            runtime,
        } => {
            status_cmd::handle_status_command(path, project_id, project_path, json, short, runtime)
                .await?;
        }
        Commands::Projects { action } => {
            project_cmd::handle_projects_action(action, assume_yes, dry_run).await?;
        }
        Commands::Branch { action } => {
            commands::handle_branch_action(action).await?;
        }
        Commands::Memory { action } => {
            dispatch_memory_command(action).await?;
        }
        Commands::Storage { action } => {
            commands::handle_profile_storage_action(action, assume_yes).await?;
        }
        Commands::Wipe { all } => {
            commands::handle_wipe(all, assume_yes).await?;
        }
        Commands::List { all } => {
            commands::handle_list(all).await?;
        }
        _ => unreachable!("non-project command passed to project dispatcher"),
    }
    Ok(())
}

#[hotpath::measure(label = "cli.memory.status", future = true)]
async fn dispatch_memory_command(action: MemoryAction) -> tracedecay_domain::errors::Result<()> {
    match action {
        MemoryAction::Status {
            json,
            path,
            project_id,
            project_path,
        } => {
            let project_path = resolve_cli_project_root(path, project_id, project_path).await?;
            let result = commands::daemon_tool_json(
                Some(&project_path),
                "tracedecay_memory_status",
                serde_json::json!({ "format": "json" }),
            )
            .await?;
            let status: tracedecay_application::retained_surfaces::MemoryStatusResultV1 =
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

async fn dispatch_runtime_command(command: Commands) -> tracedecay_domain::errors::Result<()> {
    match command {
        Commands::Tool {
            project,
            name,
            args,
        } => {
            tool_command::run(project, name, args).await?;
        }
        Commands::Work { invocation } => work_command::run(invocation).await?,
        Commands::Workflow { invocation } => workflow_command::run(invocation).await?,
        Commands::Semantic { action } => semantic_cmd::run(action).await?,
        Commands::Remote { action } => {
            hotpath::measure_block!("cli.remote.run", crate::remote_command::run(action.into()))?;
        }
        Commands::Lsp { action } => {
            lsp_cmd::handle_lsp_action(action).await?;
        }
        Commands::Dashboard {
            path,
            host,
            port,
            open,
        } => {
            let project_path = tracedecay::config::resolve_path_with_discovery(path);
            let result = hotpath::future!(
                commands::daemon_tool_json(
                    Some(&project_path),
                    "tracedecay_dashboard",
                    serde_json::json!({
                        "action": "start",
                        "host": host,
                        "port": port,
                        "format": "json",
                    }),
                ),
                label = "cli.dashboard.start"
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
                    eprintln!("Serving project {}", project_path.display());
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
                    eprintln!("Serving project {}", project_path.display());
                }
            }
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
            tracedecay::daemon::mark_process_long_lived_for_session_maintenance();
            hotpath::future!(serve_cmd::run_serve(path, timings), label = "cli.serve.run").await?;
        }
        Commands::Daemon { action } => {
            dispatch_daemon_command(action).await?;
        }
        _ => unreachable!("non-runtime command passed to runtime dispatcher"),
    }
    Ok(())
}

async fn dispatch_daemon_command(action: DaemonAction) -> tracedecay_domain::errors::Result<()> {
    match action {
        DaemonAction::Run {
            socket,
            profile_root: _,
            remote_listen,
            remote_tls_cert,
            remote_tls_key,
        } => {
            // Long-lived host: allowed to run the structured-row sweep.
            tracedecay::daemon::mark_process_long_lived_for_session_maintenance();
            let socket_path = tracedecay_daemon_control::socket_path_or_default(socket)?;
            let remote_tls = tracedecay_daemon_control::RemoteBrainTlsConfig::from_optional_parts(
                remote_listen,
                remote_tls_cert.map(PathBuf::from),
                remote_tls_key.map(PathBuf::from),
            )?;
            // Boxed on purpose: `run_foreground` is the daemon's entire
            // bootstrap state machine, and `hotpath::future!` wraps by value -
            // unboxed, the whole machine inlines into this dispatch future and
            // overflows the main thread's stack at startup (measured tonight;
            // same class as the 37MB serve_broker_socket_client machine).
            Box::pin(hotpath::future!(
                tracedecay::daemon::run_foreground(socket_path, remote_tls),
                label = "cli.daemon.run"
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
            let tracedecay_bin = tracedecay::agents::which_tracedecay_path().ok_or_else(|| {
                tracedecay_domain::errors::TraceDecayError::Config {
                    message: "tracedecay not found on PATH".to_string(),
                }
            })?;
            let remote_tls = tracedecay_daemon_control::RemoteBrainTlsConfig::from_optional_parts(
                remote_listen,
                remote_tls_cert.map(PathBuf::from),
                remote_tls_key.map(PathBuf::from),
            )?;
            let spec = tracedecay_daemon_control::service_spec_with_remote_tls(
                tracedecay_bin,
                socket,
                remote_tls,
            )?;
            let service_path = hotpath::measure_block!(
                "cli.daemon.install_service",
                tracedecay_daemon_control::install_service(
                    &spec,
                    !no_start,
                    crate::product_runtime::PRODUCT_BUILD_VERSION,
                )
            )?;
            eprintln!(
                "Installed TraceDecay daemon service at {}",
                service_path.display()
            );
            if cfg!(windows) {
                let profile_root = tracedecay_daemon_control::installed_service_socket_path()?
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
            let service_path = hotpath::measure_block!(
                "cli.daemon.uninstall_service",
                tracedecay_daemon_control::uninstall_service(
                    !no_stop,
                    crate::product_runtime::PRODUCT_BUILD_VERSION,
                )
            )?;
            eprintln!(
                "Removed TraceDecay daemon service at {}",
                service_path.display()
            );
        }
        DaemonAction::Start => {
            hotpath::measure_block!(
                "cli.daemon.start",
                tracedecay_daemon_control::start_service(
                    crate::product_runtime::PRODUCT_BUILD_VERSION
                )
            )?;
            eprintln!("Started TraceDecay daemon service");
        }
        DaemonAction::Stop => {
            hotpath::measure_block!(
                "cli.daemon.stop",
                tracedecay_daemon_control::stop_service(
                    crate::product_runtime::PRODUCT_BUILD_VERSION
                )
            )?;
            eprintln!("Stopped TraceDecay daemon service");
        }
        DaemonAction::Restart => {
            hotpath::measure_block!("cli.daemon.restart", update_cmd::restart_daemon_service())?;
        }
        DaemonAction::Status => {
            let socket_path = tracedecay_daemon_control::socket_path_or_default(None)?;
            hotpath::measure_block!(
                "cli.daemon.status",
                print!(
                    "{}",
                    tracedecay_daemon_control::service_status(&socket_path)
                )
            );
        }
    }
    Ok(())
}

async fn dispatch_agent_command(
    command: Commands,
    host_bundle: HostBundleCliOptions,
) -> tracedecay_domain::errors::Result<()> {
    let full_reinstall_preflight = matches!(
        &command,
        Commands::Reinstall {
            local: false,
            agent: None,
        }
    ) && host_bundle.component.is_none()
        && host_bundle.dry_run
        && !host_bundle.yes;
    let full_component_set_adoption = is_full_component_set_adoption(&command, &host_bundle);
    // `--dry-run` / `--yes` preview or confirm a first-party component
    // mutation, so they normally require `--component` to name the target.
    // Full `reinstall --dry-run` is the read-only exception: it validates the
    // same tracked integration set that post-update will refresh.
    if !matches!(
        command,
        Commands::FeedbackRollback { .. } | Commands::HostBundle { .. }
    ) && host_bundle.component.is_none()
        && (host_bundle.dry_run || host_bundle.yes)
        && !full_reinstall_preflight
        && !full_component_set_adoption
    {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: "--dry-run and --yes require --component to select the target host component"
                .to_string(),
        });
    }
    match command {
        Commands::Install {
            agent,
            local,
            no_dashboard,
            automation,
        } => {
            if host_bundle.component.is_some() {
                if local || automation || no_dashboard {
                    return Err(tracedecay_domain::errors::TraceDecayError::Config {
                        message: "--component cannot be combined with --local, --automation, or --no-dashboard"
                            .to_string(),
                    });
                }
                agent_cmd::handle_host_bundle_component_command(
                    agent,
                    agent_cmd::HostBundleCliOperation::Install,
                    host_bundle,
                )
                .await?;
            } else {
                agent_cmd::handle_install_command(
                    agent,
                    local,
                    no_dashboard,
                    automation.then_some(agent_cmd::CodexAutomationInstall),
                    host_bundle.adopt,
                )
                .await?;
            }
        }
        Commands::Reinstall { local, agent } => {
            if host_bundle.component.is_some() {
                if local {
                    return Err(tracedecay_domain::errors::TraceDecayError::Config {
                        message: "--component cannot be combined with --local".to_string(),
                    });
                }
                agent_cmd::handle_host_bundle_component_command(
                    None,
                    agent_cmd::HostBundleCliOperation::Repair,
                    host_bundle,
                )
                .await?;
            } else if host_bundle.dry_run {
                agent_cmd::handle_reinstall_preflight_command()?;
            } else if local {
                agent_cmd::handle_project_local_lifecycle_command(
                    agent.expect("--local requires --agent"),
                    agent_cmd::HostBundleCliOperation::Repair,
                )
                .await?;
            } else {
                agent_cmd::handle_reinstall_command(host_bundle.adopt).await?;
            }
        }
        Commands::UpdatePlugin { local, agent } => {
            if host_bundle.component.is_some() {
                if local {
                    return Err(tracedecay_domain::errors::TraceDecayError::Config {
                        message: "--component cannot be combined with --local".to_string(),
                    });
                }
                agent_cmd::handle_host_bundle_component_command(
                    None,
                    agent_cmd::HostBundleCliOperation::Update,
                    host_bundle,
                )
                .await?;
            } else if local {
                agent_cmd::handle_project_local_lifecycle_command(
                    agent.expect("--local requires --agent"),
                    agent_cmd::HostBundleCliOperation::Update,
                )
                .await?;
            } else {
                agent_cmd::handle_update_plugin_command(host_bundle.adopt).await?;
            }
        }
        Commands::Uninstall { agent, local } => {
            if host_bundle.component.is_some() {
                if local {
                    return Err(tracedecay_domain::errors::TraceDecayError::Config {
                        message: "--component cannot be combined with --local".to_string(),
                    });
                }
                agent_cmd::handle_host_bundle_component_command(
                    agent,
                    agent_cmd::HostBundleCliOperation::Uninstall,
                    host_bundle,
                )
                .await?;
            } else if local {
                agent_cmd::handle_project_local_lifecycle_command(
                    agent.expect("--local requires --agent"),
                    agent_cmd::HostBundleCliOperation::Uninstall,
                )
                .await?;
            } else {
                agent_cmd::handle_uninstall_command(agent).await?;
            }
        }
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
            agent_cmd::handle_feedback_rollback_command(action).await?;
        }
        Commands::HostBundle { action } => {
            if matches!(
                &action,
                crate::cli::HostBundleAction::ArtifactBackup { .. }
                    | crate::cli::HostBundleAction::ArtifactRestore { .. }
            ) {
                agent_cmd::handle_host_bundle_artifact_command(action, host_bundle).await?;
            } else {
                if host_bundle.component.is_some() {
                    return Err(tracedecay_domain::errors::TraceDecayError::Config {
                        message: "host-bundle recovery operates on the whole component set"
                            .to_string(),
                    });
                }
                agent_cmd::handle_host_bundle_recovery_command(
                    action,
                    host_bundle.dry_run,
                    host_bundle.yes,
                )
                .await?;
            }
        }
        _ => unreachable!("non-agent command passed to agent dispatcher"),
    }
    Ok(())
}

async fn dispatch_hook_command(
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
        | Commands::HookOpenCodeToolAfter) => hook_cmd::handle_hook_command(hook_command).await?,
        _ => unreachable!("non-hook command passed to hook dispatcher"),
    };
    Ok(CommandOutcome::Exit(code))
}

async fn dispatch_update_command(command: Commands) -> tracedecay_domain::errors::Result<()> {
    match command {
        Commands::Upgrade { no_reinstall } => {
            update_cmd::run_upgrade_command(no_reinstall)?;
        }
        Commands::Update { no_reinstall } => {
            update_cmd::run_update_command(no_reinstall)?;
        }
        Commands::PostUpdate {
            no_reinstall,
            lifecycle_lease_token,
        } => {
            update_cmd::run_post_update_command(no_reinstall, lifecycle_lease_token.as_deref())
                .await?;
        }
        Commands::PackageHook {
            action: PackageHookAction::Scoop { action },
        } => match action {
            ScoopPackageHookAction::Prepare {
                package_id,
                state_file,
            } => {
                hotpath::measure_block!(
                    "cli.package_hook.prepare",
                    tracedecay_daemon_control::prepare_scoop_package_service(
                        &package_id,
                        &state_file,
                        crate::product_runtime::PRODUCT_BUILD_VERSION,
                    )
                )?;
            }
            ScoopPackageHookAction::Restore {
                package_id,
                state_file,
            } => {
                hotpath::measure_block!(
                    "cli.package_hook.restore",
                    tracedecay_daemon_control::restore_scoop_package_service(
                        &package_id,
                        &state_file,
                        crate::product_runtime::PRODUCT_BUILD_VERSION,
                    )
                )?;
            }
        },
        Commands::Channel { channel } => match channel {
            Some(target) => {
                hotpath::measure_block!(
                    "cli.channel.switch",
                    crate::upgrade::switch_channel(&target)
                )?;
            }
            None => {
                hotpath::measure_block!("cli.channel.show", crate::upgrade::show_channel())
            }
        },
        _ => unreachable!("non-update command passed to update dispatcher"),
    }
    Ok(())
}

async fn dispatch_configuration_command(
    command: Commands,
) -> tracedecay_domain::errors::Result<()> {
    match command {
        Commands::CurrentCounter { path } => {
            let project_path = tracedecay::config::resolve_path(path);
            let result = hotpath::future!(
                commands::daemon_tool_json(
                    Some(&project_path),
                    "tracedecay_admin_project",
                    serde_json::json!({ "action": "counter_get" }),
                ),
                label = "cli.counter.current"
            )
            .await?;
            let value = result
                .get("counter")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
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
                .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
                    message: "daemon counter response omitted counter".to_string(),
                })?;
            hotpath::future!(
                commands::daemon_tool_json(
                    Some(&project_path),
                    "tracedecay_admin_project",
                    serde_json::json!({ "action": "counter_reset" }),
                ),
                label = "cli.counter.reset"
            )
            .await?;
            eprintln!("Local counter reset (was {prev})");
        }
        Commands::DisableUploadCounter => {
            commands::handle_upload_counter(false).await?;
        }
        Commands::EnableUploadCounter => {
            commands::handle_upload_counter(true).await?;
        }
        Commands::Gitignore { path, action } => {
            commands::handle_gitignore(path, action).await?;
        }
        _ => unreachable!("non-configuration command passed to configuration dispatcher"),
    }
    Ok(())
}

async fn dispatch_diagnostics_command(command: Commands) -> tracedecay_domain::errors::Result<()> {
    match command {
        Commands::Doctor => {
            hotpath::future!(tracedecay::doctor::run_doctor(), label = "cli.doctor.run").await?;
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
            hotpath::measure_block!("cli.monitor.run", monitor_cmd::run())?;
        }
        _ => unreachable!("non-diagnostics command passed to diagnostics dispatcher"),
    }
    Ok(())
}

async fn dispatch_knowledge_command(command: Commands) -> tracedecay_domain::errors::Result<()> {
    match command {
        Commands::Git { action } => {
            git_cmd::handle_git_action(action).await?;
        }
        Commands::Sessions { action } => {
            sessions_cmd::handle_sessions_action(action).await?;
        }
        Commands::Analytics { action } => match action {
            AnalyticsAction::Diagnostics { all, no_sync, .. } => {
                hotpath::future!(
                    analytics_cmd::run_analytics_diagnostics(all, no_sync),
                    label = "cli.analytics.diagnostics"
                )
                .await?;
            }
            AnalyticsAction::Sync => {
                hotpath::future!(
                    analytics_cmd::run_analytics_sync(),
                    label = "cli.analytics.sync"
                )
                .await?;
            }
        },
        Commands::Automation { action } => {
            automation_cli::handle_automation_command(action).await?;
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
            | Commands::Work { .. }
            | Commands::Workflow { .. }
            | Commands::Semantic { .. }
            | Commands::Remote { .. }
            | Commands::Git { .. } => Self::SkipAll,
            // Explicit lifecycle/maintenance commands manage their own work.
            // Serve is also latency-sensitive: clients impose a 30 s MCP
            // initialize timeout, so no implicit startup work belongs there.
            Commands::Install { .. }
            | Commands::Reinstall { .. }
            | Commands::UpdatePlugin { .. }
            | Commands::FeedbackRollback { .. }
            | Commands::HostBundle { .. }
            | Commands::Upgrade { .. }
            | Commands::Update { .. }
            | Commands::PostUpdate { .. }
            | Commands::PackageHook { .. }
            | Commands::Uninstall { .. }
            | Commands::Lsp { .. }
            | Commands::Doctor
            | Commands::Analytics { .. }
            | Commands::Sessions {
                action:
                    SessionsAction::Import { .. }
                    | SessionsAction::GitSync { .. }
                    | SessionsAction::Unfinished { .. },
            }
            | Commands::Storage { .. }
            | Commands::Wipe { .. }
            | Commands::Projects { .. }
            | Commands::Daemon { .. }
            | Commands::Serve { .. } => Self::SkipAll,
            // Inspection-only commands retain ordinary startup maintenance but
            // do not need the unrelated agent-install health check.
            Commands::Status { .. }
            | Commands::CurrentCounter { .. }
            | Commands::Cost { .. }
            | Commands::Bench { .. }
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
            | Commands::Gitignore { action: None, .. }
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
