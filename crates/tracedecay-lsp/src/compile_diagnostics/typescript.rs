//! `tsc -p <tsconfig> --pretty false --noEmit` driver.
//!
//! tsc emits diagnostics as one-per-line text:
//!
//! ```text
//! src/lib.ts(4,15): error TS2322: Type 'string' is not assignable to type 'number'.
//! ```
//!
//! The parser extracts file, line, column, level, code, and message from
//! that shape. Multi-line `error: …` continuations are concatenated into
//! the prior diagnostic. Each project [`super::tsconfig`] discovers is checked
//! with its own `-p` run rather than `tsc --build`, so a reference is checked
//! as the project it is and nothing is emitted.
//!
//! The compiler is the project's own `node_modules/.bin/tsc`, so the check
//! runs the TypeScript version the project pins rather than whatever happens
//! to be on the daemon's `PATH`. A tsconfig without one is a typed
//! missing-compiler state, not an empty success, because a caller that asks
//! for diagnostics must learn that nothing checked the project.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::process::Stdio;

use super::tsconfig::{typescript_install_command, typescript_projects};
use super::{Diagnostic, Driver, Scope, is_diagnostic_level};
use tracedecay_domain::errors::{Result, TraceDecayError};

/// Installs the project's own compiler where the producer looks for it; the
/// command for a package with no workspace lockfile to install from.
pub const TYPESCRIPT_INSTALL_COMMAND: &str = "npm install --save-dev typescript";

pub struct TscDriver;

impl Driver for TscDriver {
    fn name(&self) -> &'static str {
        "typescript"
    }

    fn detect(&self, project_root: &Path) -> bool {
        project_root.join("tsconfig.json").exists()
    }

    fn run<'a>(
        &'a self,
        project_root: &'a Path,
        _scope: &'a Scope,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Diagnostic>>> + Send + 'a>> {
        Box::pin(hotpath::future!(
            async move {
                let mut diagnostics = Vec::new();
                for project in typescript_projects(project_root) {
                    let Some(compiler) = project.compiler else {
                        let package = project.tsconfig.parent().unwrap_or(project_root);
                        return Err(TraceDecayError::Config {
                            message: format!(
                                "no TypeScript compiler for '{}': run `{}`",
                                project.tsconfig.display(),
                                typescript_install_command(project_root, package)
                            ),
                        });
                    };
                    diagnostics
                        .extend(run_compiler(&compiler, project_root, &project.tsconfig).await?);
                }
                Ok(diagnostics)
            },
            label = "compile_diagnostics.typescript.tsc"
        ))
    }
}

/// Runs one resolved compiler over one tsconfig and parses its report.
///
/// The compiler runs from `project_root`, so tsc reports every path relative
/// to the root the code index addresses files by, whichever package it checks.
///
/// tsc exits 0 for a clean project and 1 or 2 (`DiagnosticsPresent_*`) when it
/// reported diagnostics; all three are answers. Any other exit (an invalid
/// project, a reference cycle), or a diagnostic without a file location (a
/// `tsconfig.json` that names no inputs, an unreadable option), means the
/// project was not checked, and that is a typed failure rather than a clean
/// page.
pub async fn run_compiler(
    compiler: &Path,
    project_root: &Path,
    tsconfig: &Path,
) -> Result<Vec<Diagnostic>> {
    let mut cmd = tokio::process::Command::new(compiler);
    cmd.arg("-p")
        .arg(tsconfig)
        .arg("--noEmit")
        .arg("--pretty")
        .arg("false")
        .current_dir(project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = cmd
        .output()
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("failed to spawn `{}`: {error}", compiler.display()),
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
    Ok(parse_tsc_output(&stdout))
}

/// A tsc report line that carries no file location (`error TS18003: …`);
/// such lines describe the check itself failing, not a source finding.
fn is_global_error(line: &str) -> bool {
    line.starts_with("error TS")
}

/// Parse the full tsc stdout into a flat diagnostic list. Top-level so it
/// can be unit-tested without spawning tsc.
#[hotpath::measure(label = "compile_diagnostics.typescript.parse")]
pub fn parse_tsc_output(stdout: &str) -> Vec<Diagnostic> {
    let mut out: Vec<Diagnostic> = Vec::new();
    for line in stdout.lines() {
        if let Some(diag) = parse_tsc_line(line) {
            out.push(diag);
            continue;
        }
        // Continuation line: append to the prior diagnostic if its text
        // doesn't start a new file(line,col): error pattern.
        if let Some(last) = out.last_mut() {
            let trimmed = line.trim_end();
            if !trimmed.is_empty() {
                last.message.push(' ');
                last.message.push_str(trimmed);
            }
        }
    }
    out
}

/// Parse a single tsc output line into a `Diagnostic`. Returns `None` for
/// non-diagnostic lines (banners, summary lines, blanks).
pub fn parse_tsc_line(line: &str) -> Option<Diagnostic> {
    // file(line,col): level TSnnnn: message
    let open = line.find('(')?;
    let close = line[open..].find(')')? + open;
    let after = &line[close + 1..];
    let after = after.strip_prefix(':')?.trim_start();

    let file = line[..open].to_string();
    let location = &line[open + 1..close];
    let mut parts = location.splitn(2, ',');
    let line_no: u32 = parts.next()?.trim().parse().ok()?;
    let column: u32 = parts.next()?.trim().parse().unwrap_or(1);

    // after = "error TS2322: Type ..." or "warning TS####: ..."
    let mut tokens = after.splitn(3, ' ');
    let level = tokens.next()?.to_string();
    if !is_diagnostic_level(&level) {
        return None;
    }
    let code_token = tokens.next()?;
    let message = tokens.next()?.trim().to_string();
    let code = code_token.trim_end_matches(':').to_string();

    Some(Diagnostic {
        file,
        line_start: line_no,
        line_end: line_no,
        column: column.max(1),
        level,
        code,
        message,
        driver: "typescript",
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

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
    fn parse_returns_none_for_blank_lines() {
        assert!(parse_tsc_line("").is_none());
        assert!(parse_tsc_line("   ").is_none());
    }

    #[test]
    fn parse_returns_none_for_summary_line() {
        // tsc summary lines like "Found 3 errors."
        assert!(parse_tsc_line("Found 3 errors.").is_none());
    }

    #[test]
    fn parse_continuation_appends_to_prior() {
        let stdout = "src/a.ts(1,1): error TS2322: Outer message.\n  Inner detail line.\n";
        let diags = parse_tsc_output(stdout);
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
        let diags = parse_tsc_output(stdout);
        assert_eq!(diags.len(), 2);
        assert_eq!(diags[0].file, "src/a.ts");
        assert_eq!(diags[1].level, "warning");
    }

    /// `tsc --noEmit` exits 2 (`DiagnosticsPresent_OutputsGenerated`) for a
    /// project with errors; treating only 0 and 1 as answers turned every
    /// real finding into a producer failure.
    #[cfg(unix)]
    #[tokio::test]
    async fn run_compiler_accepts_every_diagnostics_present_exit_code() {
        for exit in [0, 1, 2] {
            let project = tempfile::tempdir().unwrap();
            let tsconfig = project.path().join("packages/app/tsconfig.json");
            let compiler = project.path().join("tsc");
            // Reports only when pointed at exactly the requested tsconfig.
            write_script(
                &compiler,
                &format!(
                    "#!/bin/sh\n[ \"$1 $2 $3\" = \"-p {} --noEmit\" ] || exit 9\necho \"packages/app/src/index.ts(3,14): error TS4023: Exported variable 'value' cannot be named.\"\nexit {exit}\n",
                    tsconfig.display()
                ),
            );

            let diagnostics = run_compiler(&compiler, project.path(), &tsconfig)
                .await
                .unwrap_or_else(|error| panic!("exit {exit} is a checked project: {error}"));
            assert_eq!(diagnostics.len(), 1, "exit {exit}");
            assert_eq!(diagnostics[0].code, "TS4023");
            assert_eq!((diagnostics[0].line_start, diagnostics[0].column), (3, 14));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_compiler_refuses_a_check_that_named_no_inputs() {
        let project = tempfile::tempdir().unwrap();
        let compiler = project.path().join("tsc");
        write_script(
            &compiler,
            "#!/bin/sh\necho \"error TS18003: No inputs were found in config file.\"\nexit 3\n",
        );

        let error = run_compiler(
            &compiler,
            project.path(),
            &project.path().join("tsconfig.json"),
        )
        .await
        .expect_err("a file-less tsc error is not a clean project");
        assert!(
            error.to_string().contains("TS18003"),
            "the refusal names the compiler's own error: {error}"
        );
    }

    /// Writes an executable script from a child shell so this process never
    /// holds a writable descriptor to it: a sibling test forking while one is
    /// open makes executing the script fail with `ETXTBSY`.
    #[cfg(unix)]
    fn write_script(path: &Path, body: &str) {
        use std::io::Write;

        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("cat > \"$0\" && chmod 755 \"$0\"")
            .arg(path)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(body.as_bytes())
            .unwrap();
        assert!(child.wait().unwrap().success());
    }
}
