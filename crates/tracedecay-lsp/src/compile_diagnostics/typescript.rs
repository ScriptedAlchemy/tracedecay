//! `tsc -p <tsconfig> --pretty false --noEmit` driver.
//!
//! tsc emits diagnostics as one-per-line text:
//!
//! ```text
//! src/lib.ts(4,15): error TS2322: Type 'string' is not assignable to type 'number'.
//! ```
//!
//! The parser extracts file, line, column, level, code, and message from
//! that shape. Multi-line continuations are concatenated into the prior
//! diagnostic; malformed diagnostic headers are failures, not continuations.
//! Each project [`super::tsconfig`] discovers is checked with its own `-p`
//! run rather than `tsc --build`, so a reference is checked as the project it
//! is and nothing is emitted.
//!
//! The compiler is the project's own `node_modules/.bin/tsc`, so the check
//! runs the TypeScript version the project pins rather than whatever happens
//! to be on the daemon's `PATH`. A tsconfig without one is a typed
//! missing-compiler state, not an empty success, because a caller that asks
//! for diagnostics must learn that nothing checked the project.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Semaphore;

use super::{Diagnostic, is_diagnostic_level};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1;
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::git::{GitCommandBounds, GitCommandError, bounded_command_output};
use tracedecay_runtime_core::native_runtime;

/// Limit simultaneous compiler children as well as their background CPU demand.
const MAX_CONCURRENT_COMPILERS: usize = 4;

/// Shared by project producers in one daemon generation.
#[derive(Clone)]
pub struct TypeScriptCompilerAdmission {
    permits: Arc<Semaphore>,
    background_cpu: Arc<ProcessBackgroundCpuV1>,
    cancellation: CancellationToken,
}

impl TypeScriptCompilerAdmission {
    #[must_use]
    pub fn new(background_cpu: Arc<ProcessBackgroundCpuV1>) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(MAX_CONCURRENT_COMPILERS)),
            background_cpu,
            cancellation: CancellationToken::new(),
        }
    }

    #[must_use]
    pub fn concurrency(&self) -> usize {
        MAX_CONCURRENT_COMPILERS
    }

    pub fn cancel(&self) {
        self.permits.close();
        self.cancellation.cancel();
    }
}

/// What one compiler child may consume once it holds a permit. A check that
/// exceeds any bound fails; it never reports a clean project.
#[derive(Clone, Copy)]
struct CheckLimits {
    deadline: Duration,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
}

const CHECK_LIMITS: CheckLimits = CheckLimits {
    deadline: Duration::from_mins(20),
    max_stdout_bytes: 64 * 1024 * 1024,
    max_stderr_bytes: 1024 * 1024,
};

/// Installs the project's own compiler where the producer looks for it; the
/// command for a package with no workspace lockfile to install from.
pub const TYPESCRIPT_INSTALL_COMMAND: &str = "npm install --save-dev typescript";

/// Runs one resolved compiler over one tsconfig and parses its report.
///
/// The compiler runs from `project_root`, so tsc reports every path relative
/// to the root the code index addresses files by, whichever package it checks.
///
/// tsc exits 0 for a clean project and 1 or 2 (`DiagnosticsPresent_*`) when it
/// reported diagnostics. A nonzero exit without a readable diagnostic, any
/// other exit, or a diagnostic without a file location means the project was
/// not checked, and that is a typed failure rather than a clean page.
///
/// Producers sharing `admission` share its compiler capacity. A running check is
/// bounded by a deadline and output limits, and exceeding one is a failure.
///
/// With `build_info`, the check is incremental against that
/// `--tsBuildInfoFile`: tsc rechecks only what changed since the check that
/// wrote it and records the program files it read there.
#[tracing::instrument(
    name = "compile_diagnostics.typescript.run_compiler",
    level = "trace",
    skip_all
)]
pub async fn run_compiler(
    admission: &TypeScriptCompilerAdmission,
    compiler: &Path,
    project_root: &Path,
    tsconfig: &Path,
    build_info: Option<&Path>,
) -> Result<Vec<Diagnostic>> {
    run_compiler_within(
        admission,
        compiler,
        project_root,
        tsconfig,
        build_info,
        CHECK_LIMITS,
    )
    .await
}

async fn run_compiler_within(
    admission: &TypeScriptCompilerAdmission,
    compiler: &Path,
    project_root: &Path,
    tsconfig: &Path,
    build_info: Option<&Path>,
    limits: CheckLimits,
) -> Result<Vec<Diagnostic>> {
    let mut cmd = Command::new(compiler);
    cmd.arg("-p")
        .arg(tsconfig)
        .arg("--noEmit")
        .arg("--pretty")
        .arg("false");
    if let Some(build_info) = build_info {
        cmd.arg("--incremental")
            .arg("--tsBuildInfoFile")
            .arg(build_info);
    }
    cmd.current_dir(project_root);
    // Dropping this future cancels the token, and the runner then kills and
    // reaps tsc.
    let cancel = admission.cancellation.child_token();
    let _cancel_on_drop = CancelOnDrop(cancel.clone());
    let permit = Arc::clone(&admission.permits)
        .acquire_owned()
        .await
        .map_err(|_| TraceDecayError::Config {
            message: "the TypeScript compiler admission closed".to_owned(),
        })?;
    let bounds = GitCommandBounds {
        deadline: Instant::now() + limits.deadline,
        cancel: Some(cancel),
        max_stdout_bytes: limits.max_stdout_bytes,
        max_stderr_bytes: limits.max_stderr_bytes,
    };
    let background_cpu = Arc::clone(&admission.background_cpu);
    // The permit travels with the blocking runner, which kills and reaps the
    // child before returning, so it is held for the child's whole lifetime
    // even when this future is dropped first.
    let output = native_runtime::spawn_blocking(move || {
        let _permit = permit;
        let _cpu = background_cpu
            .acquire_cancellable(|| {
                bounds
                    .cancel
                    .as_ref()
                    .is_some_and(CancellationToken::is_cancelled)
                    || Instant::now() >= bounds.deadline
            })
            .ok_or_else(|| {
                if bounds
                    .cancel
                    .as_ref()
                    .is_some_and(CancellationToken::is_cancelled)
                {
                    GitCommandError::Cancelled
                } else {
                    GitCommandError::DeadlineExceeded
                }
            })?;
        bounded_command_output(cmd, None, &bounds)
    })
    .await
    .map_err(|error| TraceDecayError::Config {
        message: format!("failed to run `{}`: {error}", compiler.display()),
    })?
    .map_err(|error| TraceDecayError::Config {
        message: match error {
            GitCommandError::Unavailable(error) => {
                format!("failed to spawn `{}`: {error}", compiler.display())
            }
            GitCommandError::ReadOutput { stream, source } => {
                format!("failed to read `{}` {stream}: {source}", compiler.display())
            }
            other => format!("`{}` did not finish: {other}", compiler.display()),
        },
    })?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if let Some(global) = stdout
        .lines()
        .map(str::trim)
        .find(|line| is_global_error(line))
    {
        return Err(TraceDecayError::Config {
            message: format!(
                "`{}` could not check `{}`: {global}",
                compiler.display(),
                tsconfig.display()
            ),
        });
    }
    if !matches!(output.status.code(), Some(0..=2)) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(TraceDecayError::Config {
            message: format!(
                "`{}` exited with {} checking `{}`: {}",
                compiler.display(),
                output.status,
                tsconfig.display(),
                stderr.trim().lines().next().unwrap_or("no output")
            ),
        });
    }
    let diagnostics = parse_tsc_output(&stdout)?;
    if !output.status.success() && diagnostics.is_empty() {
        return Err(TraceDecayError::Config {
            message: format!(
                "`{}` exited with {} checking `{}` without readable diagnostics",
                compiler.display(),
                output.status,
                tsconfig.display()
            ),
        });
    }
    Ok(diagnostics)
}

struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// A tsc report line that carries no file location (`error TS18003: …`);
/// such lines describe the check itself failing, not a source finding.
fn is_global_error(line: &str) -> bool {
    line.starts_with("error TS")
}

// Select the header before validating its fields: a malformed location or code
// must not let a diagnostic-shaped fragment in the message become a new header.
fn diagnostic_header_end(line: &str) -> Option<usize> {
    line.match_indices("):").find_map(|(close, _)| {
        let (level, _) = line[close + 2..].trim_start().split_once(' ')?;
        is_diagnostic_level(level).then_some(close)
    })
}

/// Parse tsc stdout without silently losing a diagnostic-looking header.
#[tracing::instrument(
    name = "compile_diagnostics.typescript.parse",
    level = "trace",
    skip_all
)]
pub fn parse_tsc_output(stdout: &str) -> Result<Vec<Diagnostic>> {
    let mut out: Vec<Diagnostic> = Vec::new();
    for line in stdout.lines() {
        if let Some(diag) = parse_tsc_line(line) {
            out.push(diag);
            continue;
        }
        if is_global_error(line.trim_start()) || diagnostic_header_end(line).is_some() {
            return Err(TraceDecayError::Config {
                message: format!("unrecognized TypeScript diagnostic: {line}"),
            });
        }
        if let Some(last) = out.last_mut() {
            let trimmed = line.trim_end();
            if !trimmed.is_empty() {
                last.message.push(' ');
                last.message.push_str(trimmed);
            }
        }
    }
    Ok(out)
}

/// Parse a single tsc output line into a `Diagnostic`. Returns `None` for
/// non-diagnostic lines or malformed locations/codes.
pub fn parse_tsc_line(line: &str) -> Option<Diagnostic> {
    let close = diagnostic_header_end(line)?;
    let before = &line[..close];
    let open = before.rfind('(')?;
    let file = &before[..open];
    if file.is_empty() {
        return None;
    }
    let (line_no, column) = before[open + 1..].split_once(',')?;
    let line_no = line_no.trim().parse::<u32>().ok()?;
    let column = column.trim().parse::<u32>().ok()?;
    if line_no == 0 || column == 0 {
        return None;
    }

    let after = line[close + 2..].trim_start();
    let (level, details) = after.split_once(' ')?;
    if !is_diagnostic_level(level) {
        return None;
    }
    let (code, message) = details.split_once(':')?;
    let digits = code.strip_prefix("TS")?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }

    Some(Diagnostic {
        file: file.to_owned(),
        line_start: line_no,
        line_end: line_no,
        column,
        level: level.to_owned(),
        code: code.to_owned(),
        message: message.trim().to_owned(),
        driver: "typescript",
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use tracedecay_runtime_core::test_executable::write_executable_script;

    #[test]
    fn parse_basic_error_line() {
        let line =
            "src/lib.ts(4,15): error TS2322: Type 'string' is not assignable to type 'number'.";
        let d = parse_tsc_line(line).expect("should parse");
        assert_eq!(d.file, "src/lib.ts");
        assert_eq!(d.line_start, 4);
        assert_eq!(d.column, 15);
        assert_eq!(d.level, "error");
        assert_eq!(d.code, "TS2322");
        assert!(d.message.contains("not assignable"));
        assert_eq!(d.driver, "typescript");
    }

    #[test]
    fn parse_parenthesized_paths_without_consuming_message_locations() {
        for file in [
            "app/(auth)/page.tsx",
            "/Users/Alice/Work (2026)/(group)/page.tsx",
            r"C:\Work (2026)\app\(auth)\page.tsx",
            "app/(1,2)/(nested)/page.tsx",
        ] {
            let message = "Related text (9,9): error TS9999: is part of this message.";
            let line = format!("{file}(4,15): error TS2322: {message}");
            let diagnostic = parse_tsc_line(&line).expect("parenthesized source path");
            assert_eq!(diagnostic.file, file);
            assert_eq!((diagnostic.line_start, diagnostic.column), (4, 15));
            assert_eq!(diagnostic.code, "TS2322");
            assert_eq!(diagnostic.message, message);
        }
    }

    #[test]
    fn malformed_locations_and_codes_are_not_fabricated() {
        for line in [
            "a.ts(1,nope): error TS2322: Bad column.",
            "a.ts(0,1): error TS2322: Bad line.",
            "a.ts(1,0): error TS2322: Bad column.",
            "a.ts(1,1): error TSnope: Bad code.",
        ] {
            assert!(parse_tsc_line(line).is_none(), "{line}");
            assert!(parse_tsc_output(line).is_err(), "{line}");
        }
    }

    #[test]
    fn parse_returns_none_for_blank_lines() {
        assert!(parse_tsc_line("").is_none());
        assert!(parse_tsc_line("   ").is_none());
        let d = parse_tsc_line("a.ts(1,2): warning TS6133: unused.").expect("diagnostic line");
        assert_eq!((d.file.as_str(), d.line_start, d.column), ("a.ts", 1, 2));
    }

    #[test]
    fn parse_returns_none_for_summary_line() {
        assert!(parse_tsc_line("Found 3 errors.").is_none());
        let d = parse_tsc_line("a.ts(3,1): error TS1005: ';' expected.").expect("diagnostic line");
        assert_eq!(
            (d.code.as_str(), d.message.as_str()),
            ("TS1005", "';' expected.")
        );
    }

    #[test]
    fn parse_continuation_appends_to_prior() {
        let stdout = "src/a.ts(1,1): error TS2322: Outer message.\n  Inner detail line.\n";
        let diags = parse_tsc_output(stdout).expect("valid continuation");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("Outer message"));
        assert!(diags[0].message.contains("Inner detail"));
    }

    #[test]
    fn parse_multiple_diagnostics() {
        let stdout = "\
src/a.ts(1,1): error TS2322: First.
src/b.ts(2,2): warning TS6133: Second.
";
        let diags = parse_tsc_output(stdout).expect("valid diagnostics");
        assert_eq!(diags.len(), 2);
        assert_eq!(diags[0].file, "src/a.ts");
        assert_eq!(diags[1].level, "warning");
    }

    #[test]
    fn mixed_paths_keep_each_diagnostic_and_its_continuation() {
        let stdout = "src/a.ts(1,1): error TS2322: First.\n  First detail.\napp/(auth)/page.tsx(4,15): error TS1005: Second.\n  Second detail.\n";
        let diags = parse_tsc_output(stdout).expect("mixed paths");
        assert_eq!(diags.len(), 2);
        assert_eq!(diags[0].message, "First.   First detail.");
        assert_eq!(diags[1].file, "app/(auth)/page.tsx");
        assert_eq!(diags[1].message, "Second.   Second detail.");
    }

    #[test]
    fn an_unparsed_second_diagnostic_is_not_a_continuation() {
        let stdout =
            "src/a.ts(1,1): error TS2322: First.\nsrc/b.ts(2,nope): error TS1005: Second.\n";
        let error = parse_tsc_output(stdout).expect_err("incomplete report");
        assert!(error.to_string().contains("src/b.ts"));
    }

    #[test]
    fn diagnostic_looking_message_cannot_replace_a_malformed_header() {
        for line in [
            "a.ts(1,nope): error TS2322: Related text (9,9): error TS9999: detail",
            "a.ts(1,1): error TSnope: Related text (9,9): error TS9999: detail",
        ] {
            assert!(parse_tsc_line(line).is_none(), "{line}");
            assert!(parse_tsc_output(line).is_err(), "{line}");
        }
    }

    #[test]
    fn diagnostic_looking_quoted_types_remain_continuation_text() {
        let stdout = "a.ts(1,1): error TS2322: Type mismatch.\n  Type '\"label: error TS9999\"' is not assignable to type '\"other: warning TS9998\"'.";
        let diagnostics = parse_tsc_output(stdout).expect("quoted literal type");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].file, "a.ts");
        assert_eq!(diagnostics[0].code, "TS2322");
        assert_eq!(
            diagnostics[0].message,
            "Type mismatch.   Type '\"label: error TS9999\"' is not assignable to type '\"other: warning TS9998\"'."
        );
    }

    #[cfg(unix)]
    fn block<T>(future: impl Future<Output = T>) -> T {
        native_runtime::block_on(future).expect("native runtime")
    }

    #[cfg(unix)]
    fn admission() -> TypeScriptCompilerAdmission {
        TypeScriptCompilerAdmission::new(Arc::new(ProcessBackgroundCpuV1::new(
            std::num::NonZeroUsize::new(MAX_CONCURRENT_COMPILERS).unwrap(),
        )))
    }

    #[cfg(unix)]
    #[test]
    fn run_compiler_accepts_every_diagnostics_present_exit_code() {
        for exit in [0, 1, 2] {
            let project = tempfile::tempdir().unwrap();
            let tsconfig = project.path().join("packages/app/tsconfig.json");
            let compiler = project.path().join("tsc");
            write_executable_script(
                &compiler,
                format!(
                    "#!/bin/sh\n[ \"$1 $2 $3\" = \"-p {} --noEmit\" ] || exit 9\necho \"packages/app/src/(auth)/index.ts(3,14): error TS4023: Exported variable 'value' cannot be named.\"\nexit {exit}\n",
                    tsconfig.display()
                ),
            ).unwrap();

            let diagnostics = block(run_compiler(
                &admission(),
                &compiler,
                project.path(),
                &tsconfig,
                None,
            ))
            .unwrap_or_else(|error| panic!("exit {exit} is a checked project: {error}"));
            assert_eq!(diagnostics.len(), 1, "exit {exit}");
            assert_eq!(diagnostics[0].file, "packages/app/src/(auth)/index.ts");
            assert_eq!(diagnostics[0].code, "TS4023");
            assert_eq!((diagnostics[0].line_start, diagnostics[0].column), (3, 14));
        }
    }

    #[cfg(unix)]
    #[test]
    fn run_compiler_refuses_diagnostic_exits_without_readable_findings() {
        for (exit, report) in [
            (1, ""),
            (2, "compiler output was not understood"),
            (1, "a.ts(1,nope): error TS2322: Malformed location."),
            (2, "a.ts(1,nope): error TS2322: Malformed location."),
        ] {
            let project = tempfile::tempdir().unwrap();
            let compiler = project.path().join("tsc");
            write_executable_script(
                &compiler,
                format!("#!/bin/sh\nprintf '%s\\n' '{report}'\nexit {exit}\n"),
            )
            .unwrap();
            let result = block(run_compiler(
                &admission(),
                &compiler,
                project.path(),
                &project.path().join("tsconfig.json"),
                None,
            ));
            assert!(result.is_err(), "exit {exit} and report {report:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn run_compiler_accepts_a_clean_empty_report() {
        let project = tempfile::tempdir().unwrap();
        let compiler = project.path().join("tsc");
        write_executable_script(&compiler, "#!/bin/sh\nexit 0\n").unwrap();
        let diagnostics = block(run_compiler(
            &admission(),
            &compiler,
            project.path(),
            &project.path().join("tsconfig.json"),
            None,
        ))
        .expect("clean compiler exit");
        assert!(diagnostics.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn run_compiler_refuses_a_check_that_named_no_inputs() {
        let project = tempfile::tempdir().unwrap();
        let compiler = project.path().join("tsc");
        write_executable_script(
            &compiler,
            "#!/bin/sh\necho \"error TS18003: No inputs were found in config file.\"\nexit 3\n",
        )
        .unwrap();

        let error = block(run_compiler(
            &admission(),
            &compiler,
            project.path(),
            &project.path().join("tsconfig.json"),
            None,
        ))
        .expect_err("a file-less tsc error is not a clean project");
        assert!(
            error.to_string().contains("TS18003"),
            "the refusal names the compiler's own error: {error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn dropping_a_check_kills_the_compiler() {
        assert_running_check_settles(false);
    }

    #[cfg(unix)]
    #[test]
    fn closing_admission_kills_the_running_compiler() {
        assert_running_check_settles(true);
    }

    #[cfg(unix)]
    fn assert_running_check_settles(close_admission: bool) {
        let project = tempfile::tempdir().unwrap();
        let compiler = project.path().join("tsc");
        let pid_file = project.path().join("tsc.pid");
        write_executable_script(
            &compiler,
            "#!/bin/sh\necho $$ > tsc.pid.tmp && mv tsc.pid.tmp tsc.pid\nexec sleep 60\n",
        )
        .unwrap();
        let tsconfig = project.path().join("tsconfig.json");

        let admission = admission();
        let pid = block(async {
            let check = std::pin::pin!(run_compiler(
                &admission,
                &compiler,
                project.path(),
                &tsconfig,
                None
            ));
            let started = std::pin::pin!(async {
                loop {
                    if let Ok(pid) = std::fs::read_to_string(&pid_file) {
                        return pid.trim().to_owned();
                    }
                    native_runtime::sleep_until(Instant::now() + Duration::from_millis(10))
                        .await
                        .unwrap();
                }
            });
            match futures_util::future::select(check, started).await {
                futures_util::future::Either::Left((result, _)) => {
                    panic!("the check finished before it was dropped: {result:?}")
                }
                futures_util::future::Either::Right((pid, check)) => {
                    if close_admission {
                        admission.cancel();
                        assert!(check.await.is_err());
                    }
                    pid
                }
            }
        });

        let deadline = Instant::now() + Duration::from_secs(10);
        while Command::new("kill")
            .args(["-0", &pid])
            .output()
            .unwrap()
            .status
            .success()
        {
            assert!(
                Instant::now() < deadline,
                "tsc {pid} outlived its dropped check"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while admission.permits.available_permits() != MAX_CONCURRENT_COMPILERS {
            assert!(
                Instant::now() < deadline,
                "child settlement retained a permit"
            );
            std::thread::yield_now();
        }
        assert_eq!(admission.background_cpu.active_units(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn dropping_a_waiting_check_never_spawns_and_releases_its_demand() {
        let project = tempfile::tempdir().unwrap();
        let compiler = project.path().join("tsc");
        write_executable_script(&compiler, "#!/bin/sh\necho ran >> ran\nexit 0\n").unwrap();
        let tsconfig = project.path().join("tsconfig.json");
        let admission = admission();
        block(async {
            let held = admission
                .permits
                .acquire_many(MAX_CONCURRENT_COMPILERS as u32)
                .await
                .unwrap();
            let mut waiting = Box::pin(run_compiler(
                &admission,
                &compiler,
                project.path(),
                &tsconfig,
                None,
            ));
            assert!(futures_util::poll!(waiting.as_mut()).is_pending());
            assert!(!project.path().join("ran").exists());
            drop(waiting);
            drop(held);
            assert!(
                run_compiler(&admission, &compiler, project.path(), &tsconfig, None)
                    .await
                    .unwrap()
                    .is_empty()
            );
        });
        assert_eq!(
            std::fs::read_to_string(project.path().join("ran")).unwrap(),
            "ran\n"
        );
        assert_eq!(
            admission.permits.available_permits(),
            MAX_CONCURRENT_COMPILERS
        );
    }

    #[cfg(unix)]
    #[test]
    fn closed_admission_refuses_waiters_and_successor_checks() {
        let project = tempfile::tempdir().unwrap();
        let compiler = project.path().join("tsc");
        write_executable_script(&compiler, "#!/bin/sh\necho ran > ran\nexit 0\n").unwrap();
        let tsconfig = project.path().join("tsconfig.json");
        let admission = admission();
        block(async {
            let held = admission
                .permits
                .acquire_many(MAX_CONCURRENT_COMPILERS as u32)
                .await
                .unwrap();
            let mut waiting = Box::pin(run_compiler(
                &admission,
                &compiler,
                project.path(),
                &tsconfig,
                None,
            ));
            assert!(futures_util::poll!(waiting.as_mut()).is_pending());
            admission.cancel();
            assert!(
                waiting
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("admission closed")
            );
            drop(held);
            assert!(
                run_compiler(&admission, &compiler, project.path(), &tsconfig, None)
                    .await
                    .is_err()
            );
        });
        assert!(!project.path().join("ran").exists());
    }

    #[cfg(unix)]
    #[test]
    fn compiler_waits_for_the_existing_background_cpu_authority() {
        let project = tempfile::tempdir().unwrap();
        let compiler = project.path().join("tsc");
        write_executable_script(&compiler, "#!/bin/sh\necho ran > ran\nexit 0\n").unwrap();
        let tsconfig = project.path().join("tsconfig.json");
        let cpu = Arc::new(ProcessBackgroundCpuV1::new(
            std::num::NonZeroUsize::new(1).unwrap(),
        ));
        let admission = TypeScriptCompilerAdmission::new(Arc::clone(&cpu));
        let held = cpu.acquire();
        let release = std::thread::spawn({
            let cpu = Arc::clone(&cpu);
            let root = project.path().to_owned();
            move || {
                let deadline = Instant::now() + Duration::from_secs(10);
                while cpu.waiting_work_units() == 0 {
                    assert!(Instant::now() < deadline);
                    std::thread::yield_now();
                }
                assert!(!root.join("ran").exists());
                drop(held);
            }
        });
        assert!(
            block(run_compiler(
                &admission,
                &compiler,
                project.path(),
                &tsconfig,
                None
            ))
            .unwrap()
            .is_empty()
        );
        release.join().unwrap();
        assert!(project.path().join("ran").exists());
        assert_eq!(cpu.active_units(), 0);
        assert_eq!(cpu.waiting_work_units(), 0);
    }

    #[cfg(unix)]
    fn tiny_limits() -> CheckLimits {
        CheckLimits {
            deadline: Duration::from_millis(300),
            max_stdout_bytes: 4096,
            max_stderr_bytes: 4096,
        }
    }

    #[cfg(unix)]
    #[test]
    fn endless_compiler_output_fails_the_check() {
        let project = tempfile::tempdir().unwrap();
        let compiler = project.path().join("tsc");
        write_executable_script(
            &compiler,
            "#!/bin/sh\nexec yes 'a.ts(1,1): error TS1: x.'\n",
        )
        .unwrap();
        let tsconfig = project.path().join("tsconfig.json");
        let error = block(run_compiler_within(
            &admission(),
            &compiler,
            project.path(),
            &tsconfig,
            None,
            CheckLimits {
                deadline: Duration::from_secs(30),
                ..tiny_limits()
            },
        ))
        .expect_err("output beyond the bound is a failed check, not a report");
        assert!(error.to_string().contains("exceeded"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn endless_compiler_stderr_fails_the_check() {
        let project = tempfile::tempdir().unwrap();
        let compiler = project.path().join("tsc");
        write_executable_script(&compiler, "#!/bin/sh\nexec yes 'failure' >&2\n").unwrap();
        let error = block(run_compiler_within(
            &admission(),
            &compiler,
            project.path(),
            &project.path().join("tsconfig.json"),
            None,
            CheckLimits {
                deadline: Duration::from_secs(30),
                ..tiny_limits()
            },
        ))
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("stderr output exceeded 4096 bytes"),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_stalled_compiler_fails_at_its_deadline() {
        let project = tempfile::tempdir().unwrap();
        let compiler = project.path().join("tsc");
        write_executable_script(&compiler, "#!/bin/sh\nexec sleep 60\n").unwrap();
        let tsconfig = project.path().join("tsconfig.json");
        let started = Instant::now();
        let error = block(run_compiler_within(
            &admission(),
            &compiler,
            project.path(),
            &tsconfig,
            None,
            tiny_limits(),
        ))
        .expect_err("a stalled compiler never reports clean");
        assert!(error.to_string().contains("deadline"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(20));
    }

    #[cfg(unix)]
    #[test]
    fn a_large_within_policy_report_retains_every_finding() {
        let project = tempfile::tempdir().unwrap();
        let compiler = project.path().join("tsc");
        write_executable_script(
            &compiler,
            "#!/bin/sh\ni=0; while [ \"$i\" -lt 1000 ]; do echo 'a.ts(1,1): error TS1: x.'; i=$((i+1)); done\nexit 2\n",
        ).unwrap();
        let diagnostics = block(run_compiler_within(
            &admission(),
            &compiler,
            project.path(),
            &project.path().join("tsconfig.json"),
            None,
            CheckLimits {
                deadline: Duration::from_secs(30),
                max_stdout_bytes: 64 * 1024,
                max_stderr_bytes: 4096,
            },
        ))
        .unwrap();
        assert_eq!(diagnostics.len(), 1000);
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_checks_never_exceed_the_shared_compiler_capacity() {
        let project = tempfile::tempdir().unwrap();
        let compiler = project.path().join("tsc");
        write_executable_script(
            &compiler,
            "#!/bin/sh\n\
             d=\"$(dirname \"$0\")\"\n\
             mkdir \"$d/active.$$\"\n\
             ls -d \"$d\"/active.* | wc -l >> \"$d/seen\"\n\
             sleep 0.3\n\
             rmdir \"$d/active.$$\"\n\
             exit 0\n",
        )
        .unwrap();
        let tsconfig = project.path().join("tsconfig.json");
        let admission = admission();
        let results = block(futures_util::future::join_all(
            (0..MAX_CONCURRENT_COMPILERS * 3)
                .map(|_| run_compiler(&admission, &compiler, project.path(), &tsconfig, None)),
        ));
        assert!(results.iter().all(Result::is_ok), "{results:?}");
        let seen = std::fs::read_to_string(project.path().join("seen")).unwrap();
        let peak = seen
            .lines()
            .map(|line| line.trim().parse::<usize>().unwrap())
            .max()
            .unwrap();
        assert!(
            (2..=MAX_CONCURRENT_COMPILERS).contains(&peak),
            "peak concurrent compilers {peak}"
        );
    }
}
