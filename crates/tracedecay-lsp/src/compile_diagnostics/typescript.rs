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
use std::time::{Duration, Instant};

use super::{Diagnostic, is_diagnostic_level};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::git::{GitCommandBounds, GitCommandError, bounded_command_output};
use tracedecay_runtime_core::native_runtime;

/// tsc has no deadline of its own: a check runs until it finishes or its
/// caller drops it. This only satisfies the bounded runner's required field.
const UNBOUNDED_CHECK: Duration = Duration::from_hours(365 * 24);

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
/// With `build_info`, the check is incremental against that
/// `--tsBuildInfoFile`: tsc rechecks only what changed since the check that
/// wrote it and records the program files it read there.
#[tracing::instrument(
    name = "compile_diagnostics.typescript.run_compiler",
    level = "trace",
    skip_all
)]
pub async fn run_compiler(
    compiler: &Path,
    project_root: &Path,
    tsconfig: &Path,
    build_info: Option<&Path>,
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
    let cancel = CancellationToken::new();
    let _cancel_on_drop = CancelOnDrop(cancel.clone());
    let bounds = GitCommandBounds {
        deadline: Instant::now() + UNBOUNDED_CHECK,
        cancel: Some(cancel),
        max_stdout_bytes: usize::MAX,
        max_stderr_bytes: usize::MAX,
    };
    let output = native_runtime::spawn_blocking(move || bounded_command_output(cmd, None, &bounds))
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

    fn block<T>(future: impl Future<Output = T>) -> T {
        native_runtime::block_on(future).expect("native runtime")
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

            let diagnostics = block(run_compiler(&compiler, project.path(), &tsconfig, None))
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
        let project = tempfile::tempdir().unwrap();
        let compiler = project.path().join("tsc");
        let pid_file = project.path().join("tsc.pid");
        write_executable_script(
            &compiler,
            "#!/bin/sh\necho $$ > tsc.pid.tmp && mv tsc.pid.tmp tsc.pid\nexec sleep 60\n",
        )
        .unwrap();
        let tsconfig = project.path().join("tsconfig.json");

        let pid = block(async {
            let check = std::pin::pin!(run_compiler(&compiler, project.path(), &tsconfig, None));
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
                futures_util::future::Either::Right((pid, _check)) => pid,
            }
        });

        let deadline = Instant::now() + Duration::from_secs(10);
        while Command::new("kill")
            .args(["-0", &pid])
            .status()
            .unwrap()
            .success()
        {
            assert!(
                Instant::now() < deadline,
                "tsc {pid} outlived its dropped check"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
